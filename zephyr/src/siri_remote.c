/*
 * siri_remote - BLE central for the Apple TV Siri Remote, on Zephyr.
 * See siri_remote.h (API) and PROTOCOL.md (wire protocol).
 *
 * UNVERIFIED (no SDK/board here - review before trusting on hardware):
 *  - Exact security level the remote requires (we use L2 / encryption).
 *  - Whether the 0xAF / 0x98 writes to 0x001D must be write-WITH-response
 *    (we use write-without-response first; switch to bt_gatt_write if the
 *    remote NAKs).
 *  - Subscribing by hard-coded handles (no discovery) - valid per PROTOCOL.md
 *    but assumes the handle map is stable across units (it has been).
 *  - RX buffers must be sized for 101-byte notifications; see Kconfig.
 */
#include "siri_remote.h"
#include "proto.h"

#include <zephyr/bluetooth/bluetooth.h>
#include <zephyr/bluetooth/conn.h>
#include <zephyr/bluetooth/gatt.h>
#include <zephyr/kernel.h>
#include <zephyr/logging/log.h>
#include <zephyr/settings/settings.h>
#include <string.h>

LOG_MODULE_REGISTER(siri_remote, CONFIG_SIRI_REMOTE_LOG_LEVEL);

/* Handle map, PROTOCOL.md §4. */
#define H_ENABLE       0x001Du
#define H_INPUT        0x0023u
#define H_INPUT_CCC    0x0024u
#define H_BATTERY      0x0028u
#define H_BATTERY_CCC  0x0029u
#define H_POWER        0x002Bu
#define H_POWER_CCC    0x002Cu
#define H_VENDOR_CCC   0x003Bu

static const uint8_t UNLOCK[] = {0xAF};
static const uint8_t STREAM_START[] = {0x98, 0x01};

#define TAIL_TIMEOUT K_MSEC(600)
#define AUDIO_FRAME_SEC 0.02f
#define MAX_CONCEAL 25

static struct {
	struct siri_remote_config cfg;
	bt_addr_le_t addr;
	struct bt_conn *conn;
	bool connected;
	uint8_t held;          /* last button bitmask */
	/* voice session */
	bool voice_active;
	bool voice_released;
	uint32_t voice_frames;
	int64_t voice_started;
	float last_time;
	/* swipe tracking */
	bool touching;
	int16_t tx0, ty0, tx1, ty1;
	struct k_work_delayable tail_work;
	struct k_work_delayable connect_work;
} S;

static struct bt_gatt_subscribe_params sub_input;
static struct bt_gatt_subscribe_params sub_batt;
static struct bt_gatt_subscribe_params sub_power;
static struct bt_gatt_subscribe_params sub_vendor;

/* ---- helpers ---- */

static void emit_button_edges(uint8_t now)
{
	uint8_t changed = now ^ S.held;
	const struct siri_remote_cb *cb = S.cfg.cb;
	for (int bit = 0; bit < 8; bit++) {
		uint8_t m = 1u << bit;
		if (!(changed & m)) {
			continue;
		}
		bool down = (now & m) != 0;
		if (m == SIRI_BTN_SIRI && down) {
			/* Siri-down: start the mic stream (PROTOCOL.md §3). */
			if (S.conn) {
				int err = bt_gatt_write_without_response(
					S.conn, H_ENABLE, STREAM_START,
					sizeof(STREAM_START), false);
				if (err) {
					LOG_WRN("stream-start write: %d", err);
				}
			}
			S.voice_active = true;
			S.voice_released = false;
			S.voice_frames = 0;
			S.last_time = -1.0f;
			S.voice_started = k_uptime_get();
			if (cb && cb->on_voice_start) {
				cb->on_voice_start();
			}
		}
		if (m == SIRI_BTN_SIRI && !down) {
			S.voice_released = true;
			k_work_reschedule(&S.tail_work, TAIL_TIMEOUT);
		}
		if (cb && cb->on_button) {
			cb->on_button(m, down);
		}
	}
	S.held = now;
}

static void voice_finish(void)
{
	if (!S.voice_active) {
		return;
	}
	struct siri_voice_stats st = {
		.frames = S.voice_frames,
		.concealed = 0,
		.held_ms = (uint32_t)(k_uptime_get() - S.voice_started),
	};
	S.voice_active = false;
	S.voice_released = false;
	if (S.cfg.cb && S.cfg.cb->on_voice_end) {
		S.cfg.cb->on_voice_end(&st);
	}
}

static void tail_timeout(struct k_work *w)
{
	ARG_UNUSED(w);
	voice_finish();
}

/* Classify a completed stroke into a swipe (coarse; app may refine). */
static void finish_touch_stroke(void)
{
	if (!S.touching) {
		return;
	}
	S.touching = false;
	int dx = S.tx1 - S.tx0;
	int dy = S.ty1 - S.ty0;
	int adx = dx < 0 ? -dx : dx;
	int ady = dy < 0 ? -dy : dy;
	const int THRESH = 200; /* units; tune on hardware */
	if (adx < THRESH && ady < THRESH) {
		return; /* a tap, not a swipe */
	}
	if (!S.cfg.cb || !S.cfg.cb->on_swipe) {
		return;
	}
	if (adx >= ady) {
		S.cfg.cb->on_swipe(dx >= 0 ? SIRI_SWIPE_RIGHT : SIRI_SWIPE_LEFT);
	} else {
		S.cfg.cb->on_swipe(dy >= 0 ? SIRI_SWIPE_UP : SIRI_SWIPE_DOWN);
	}
}

/* ---- notification routing ---- */

static uint8_t on_input(struct bt_conn *conn, struct bt_gatt_subscribe_params *p,
			const void *data, uint16_t length)
{
	ARG_UNUSED(conn);
	ARG_UNUSED(p);
	if (!data) {
		return BT_GATT_ITER_STOP; /* unsubscribed */
	}
	struct siri_report r;
	siri_proto_parse(data, length, &r);
	const struct siri_remote_cb *cb = S.cfg.cb;

	/* Any non-audio report after a release means the tail is done. */
	if (S.voice_active && S.voice_released && r.kind != SIRI_REPORT_AUDIO) {
		voice_finish();
	}

	switch (r.kind) {
	case SIRI_REPORT_AUDIO:
		emit_button_edges(r.buttons);
		if (!S.voice_active) {
			S.voice_active = true;
			S.voice_started = k_uptime_get();
		}
		S.voice_frames++;
		if (cb && cb->on_voice_frame && r.frame) {
			cb->on_voice_frame(r.codec, r.frame, r.frame_len);
		}
		break;
	case SIRI_REPORT_AUDIO_END:
		emit_button_edges(r.buttons);
		voice_finish();
		break;
	case SIRI_REPORT_TOUCH:
		if (cb && cb->on_touch) {
			cb->on_touch(r.fingers, r.n_fingers, r.pressed);
		}
		if (!S.touching) {
			S.touching = true;
			S.tx0 = r.fingers[0].x;
			S.ty0 = r.fingers[0].y;
		}
		S.tx1 = r.fingers[0].x;
		S.ty1 = r.fingers[0].y;
		emit_button_edges(r.buttons);
		break;
	case SIRI_REPORT_BUTTONS:
		if (S.touching) {
			finish_touch_stroke();
		}
		emit_button_edges(r.buttons);
		break;
	default:
		break;
	}
	return BT_GATT_ITER_CONTINUE;
}

static uint8_t on_battery(struct bt_conn *conn, struct bt_gatt_subscribe_params *p,
			  const void *data, uint16_t length)
{
	ARG_UNUSED(conn);
	ARG_UNUSED(p);
	if (data && length >= 1 && S.cfg.cb && S.cfg.cb->on_battery) {
		S.cfg.cb->on_battery(((const uint8_t *)data)[0]);
	}
	return data ? BT_GATT_ITER_CONTINUE : BT_GATT_ITER_STOP;
}

static uint8_t on_power(struct bt_conn *conn, struct bt_gatt_subscribe_params *p,
			const void *data, uint16_t length)
{
	ARG_UNUSED(conn);
	ARG_UNUSED(p);
	if (data && length >= 1 && S.cfg.cb && S.cfg.cb->on_power) {
		enum siri_power st;
		switch (((const uint8_t *)data)[0]) {
		case 0xAB: st = SIRI_POWER_CHARGING; break;
		case 0xAF: st = SIRI_POWER_DISCHARGING; break;
		case 0xBB: st = SIRI_POWER_PLUGGED_IN; break;
		default: st = SIRI_POWER_UNKNOWN; break;
		}
		S.cfg.cb->on_power(st);
	}
	return data ? BT_GATT_ITER_CONTINUE : BT_GATT_ITER_STOP;
}

static uint8_t on_vendor(struct bt_conn *conn, struct bt_gatt_subscribe_params *p,
			 const void *data, uint16_t length)
{
	ARG_UNUSED(conn); ARG_UNUSED(p); ARG_UNUSED(data); ARG_UNUSED(length);
	return data ? BT_GATT_ITER_CONTINUE : BT_GATT_ITER_STOP;
}

static void subscribe_one(struct bt_gatt_subscribe_params *p, uint16_t value_handle,
			  uint16_t ccc_handle, bt_gatt_notify_func_t func)
{
	memset(p, 0, sizeof(*p));
	p->notify = func;
	p->value = BT_GATT_CCC_NOTIFY;
	p->value_handle = value_handle;
	p->ccc_handle = ccc_handle;
	int err = bt_gatt_subscribe(S.conn, p);
	if (err && err != -EALREADY) {
		LOG_WRN("subscribe %04x: %d", value_handle, err);
	}
}

static void setup_after_secure(void)
{
	/* Unlock (PROTOCOL.md §3), then enable notifications, then ask for a
	 * fast interval (§8). */
	int err = bt_gatt_write_without_response(S.conn, H_ENABLE, UNLOCK, sizeof(UNLOCK), false);
	if (err) {
		LOG_WRN("unlock write: %d", err);
	}
	subscribe_one(&sub_input, H_INPUT, H_INPUT_CCC, on_input);
	subscribe_one(&sub_batt, H_BATTERY, H_BATTERY_CCC, on_battery);
	subscribe_one(&sub_power, H_POWER, H_POWER_CCC, on_power);
	subscribe_one(&sub_vendor, 0 /* value handle unknown */, H_VENDOR_CCC, on_vendor);

	struct bt_le_conn_param *fast = BT_LE_CONN_PARAM(6, 12, 0, 500); /* 7.5-15ms, 5s */
	err = bt_conn_le_param_update(S.conn, fast);
	if (err) {
		LOG_WRN("conn param update: %d (audio may lag)", err);
	}

	S.connected = true;
	if (S.cfg.cb && S.cfg.cb->on_connected) {
		/* Model detection would come from the Device Information service;
		 * left UNKNOWN until read. */
		S.cfg.cb->on_connected(bt_gatt_get_mtu(S.conn), SIRI_MODEL_UNKNOWN);
	}
}

/* ---- connection lifecycle ---- */

static void start_connect(struct k_work *w);

static void on_connected(struct bt_conn *conn, uint8_t err)
{
	if (conn != S.conn) {
		return;
	}
	if (err) {
		LOG_WRN("connect failed: 0x%02x; retrying", err);
		bt_conn_unref(S.conn);
		S.conn = NULL;
		k_work_reschedule(&S.connect_work, K_SECONDS(2));
		return;
	}
	/* Request encryption; unlock happens once secured. */
	int serr = bt_conn_set_security(conn, BT_SECURITY_L2);
	if (serr) {
		LOG_WRN("set_security: %d", serr);
	}
}

static void on_disconnected(struct bt_conn *conn, uint8_t reason)
{
	if (conn != S.conn) {
		return;
	}
	LOG_INF("disconnected (0x%02x)", reason);
	voice_finish();
	S.connected = false;
	S.held = 0;
	if (S.cfg.cb && S.cfg.cb->on_disconnected) {
		S.cfg.cb->on_disconnected(reason);
	}
	bt_conn_unref(S.conn);
	S.conn = NULL;
	k_work_reschedule(&S.connect_work, K_SECONDS(1));
}

static void on_security_changed(struct bt_conn *conn, bt_security_t level,
				enum bt_security_err err)
{
	if (conn != S.conn || err != BT_SECURITY_ERR_SUCCESS) {
		return;
	}
	if (level >= BT_SECURITY_L2) {
		setup_after_secure();
	}
}

BT_CONN_CB_DEFINE(conn_cbs) = {
	.connected = on_connected,
	.disconnected = on_disconnected,
	.security_changed = on_security_changed,
};

/* Just-Works pairing as a NoInputNoOutput central. */
static struct bt_conn_auth_cb auth_cb = {
	.cancel = NULL,
};

static void start_connect(struct k_work *w)
{
	ARG_UNUSED(w);
	if (S.conn) {
		return;
	}
	/* Keep a long-pending connect so a button press completes it. */
	struct bt_conn_le_create_param *cp = BT_CONN_LE_CREATE_PARAM(
		BT_CONN_LE_OPT_NONE, BT_GAP_SCAN_FAST_INTERVAL, BT_GAP_SCAN_FAST_WINDOW);
	struct bt_le_conn_param *init = BT_LE_CONN_PARAM_DEFAULT;
	int err = bt_conn_le_create(&S.addr, cp, init, &S.conn);
	if (err) {
		LOG_WRN("conn_le_create: %d; retrying", err);
		S.conn = NULL;
		k_work_reschedule(&S.connect_work, K_SECONDS(3));
	}
}

int siri_remote_start(const struct siri_remote_config *cfg)
{
	if (!cfg || !cfg->address) {
		return -EINVAL;
	}
	S.cfg = *cfg;
	int err = bt_addr_le_from_str(cfg->address,
				      cfg->addr_public ? "public" : "random", &S.addr);
	if (err) {
		LOG_ERR("bad address %s: %d", cfg->address, err);
		return err;
	}

	k_work_init_delayable(&S.tail_work, tail_timeout);
	k_work_init_delayable(&S.connect_work, start_connect);

	err = bt_enable(NULL);
	if (err && err != -EALREADY) {
		LOG_ERR("bt_enable: %d", err);
		return err;
	}
	settings_load(); /* restore bonds */
	bt_conn_auth_cb_register(&auth_cb);

	k_work_reschedule(&S.connect_work, K_NO_WAIT);
	return 0;
}

int siri_remote_stop(void)
{
	k_work_cancel_delayable(&S.connect_work);
	if (S.conn) {
		bt_conn_disconnect(S.conn, BT_HCI_ERR_REMOTE_USER_TERM_CONN);
	}
	return 0;
}

bool siri_remote_connected(void)
{
	return S.connected;
}
