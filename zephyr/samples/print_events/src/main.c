/*
 * siri_remote sample: connect to a remote and log every event.
 * Set the remote address with CONFIG_... or edit REMOTE_ADDR below.
 */
#include <zephyr/kernel.h>
#include <zephyr/logging/log.h>

#include <siri_remote.h>

LOG_MODULE_REGISTER(sample, LOG_LEVEL_INF);

/* TODO: make this a Kconfig / settings value; hard-coded for the sample. */
#define REMOTE_ADDR "DC:2B:2A:E0:18:71"

static void on_connected(uint16_t mtu, enum siri_model model)
{
	LOG_INF("connected: mtu=%u model=%d", mtu, model);
}

static void on_button(uint8_t mask, bool down)
{
	LOG_INF("button 0x%02x %s", mask, down ? "down" : "up");
}

static void on_swipe(enum siri_swipe dir)
{
	static const char *n[] = {"UP", "DOWN", "LEFT", "RIGHT"};
	LOG_INF("swipe %s", n[dir]);
}

static void on_battery(uint8_t pct)
{
	LOG_INF("battery %u%%", pct);
}

static void on_voice_start(void)
{
	LOG_INF("voice start");
}

static void on_voice_frame(enum siri_codec codec, const uint8_t *data, size_t len)
{
	ARG_UNUSED(data);
	LOG_INF("voice frame: %s %u bytes",
		codec == SIRI_CODEC_SPEEX ? "speex" : "opus", (unsigned)len);
}

static void on_voice_end(const struct siri_voice_stats *st)
{
	LOG_INF("voice end: %u frames, held %u ms", st->frames, st->held_ms);
}

static const struct siri_remote_cb cb = {
	.on_connected = on_connected,
	.on_button = on_button,
	.on_swipe = on_swipe,
	.on_battery = on_battery,
	.on_voice_start = on_voice_start,
	.on_voice_frame = on_voice_frame,
	.on_voice_end = on_voice_end,
};

int main(void)
{
	const struct siri_remote_config cfg = {
		.address = REMOTE_ADDR,
		.addr_public = true,
		.cb = &cb,
	};
	int err = siri_remote_start(&cfg);
	if (err) {
		LOG_ERR("siri_remote_start: %d", err);
	}
	return 0;
}
