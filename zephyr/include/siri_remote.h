/*
 * siri_remote - Apple TV Siri Remote (A1513/A1962) BLE central for Zephyr.
 *
 * A board-agnostic reusable module: it connects to the remote, unlocks it,
 * starts the microphone stream on Siri-down, asks for a fast connection
 * interval, and parses every notification into callbacks. It decides nothing
 * about what the events mean - that is the application's job (see the ESP
 * firmware in control4-appletv-remote/firmware).
 *
 * The wire protocol is documented in PROTOCOL.md at the repo root; this header
 * is the C mirror of §9 "event model".
 */
#ifndef SIRI_REMOTE_H_
#define SIRI_REMOTE_H_

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Button bitmask (byte 1 of every input report), PROTOCOL.md §5. */
#define SIRI_BTN_AIRPLAY   0x01
#define SIRI_BTN_VOL_UP    0x02
#define SIRI_BTN_VOL_DOWN  0x04
#define SIRI_BTN_PLAY_PAUSE 0x08
#define SIRI_BTN_SIRI      0x10
#define SIRI_BTN_MENU      0x20
#define SIRI_BTN_TOUCHPAD  0x80
/* Synthetic: two-finger click (finger-count 2 + touchpad bit). */
#define SIRI_BTN_TOUCHPAD_2 0x40

enum siri_swipe {
	SIRI_SWIPE_UP,
	SIRI_SWIPE_DOWN,
	SIRI_SWIPE_LEFT,
	SIRI_SWIPE_RIGHT,
};

enum siri_codec {
	SIRI_CODEC_SPEEX, /* A1513: 16 kHz wideband Speex, 70-byte frames */
	SIRI_CODEC_OPUS,  /* A1962: 16 kHz Opus, TOC 0xB8                 */
};

enum siri_power {
	SIRI_POWER_CHARGING,
	SIRI_POWER_DISCHARGING,
	SIRI_POWER_PLUGGED_IN,
	SIRI_POWER_UNKNOWN,
};

enum siri_model {
	SIRI_MODEL_UNKNOWN,
	SIRI_MODEL_A1513, /* product 0x0266, Speex */
	SIRI_MODEL_A1962, /* product 0x026D, Opus  */
};

struct siri_finger {
	int16_t x;
	int16_t y;
	uint8_t pressure;
};

struct siri_voice_stats {
	uint32_t frames;    /* frames received */
	uint32_t concealed; /* frames synthesised for loss (if the app conceals) */
	uint32_t held_ms;   /* how long Siri was held */
};

/*
 * Callbacks. All are optional (leave NULL). They run in the Bluetooth RX
 * thread context - keep them short; hand audio frames off to a queue rather
 * than processing inline.
 */
struct siri_remote_cb {
	void (*on_connected)(uint16_t mtu, enum siri_model model);
	void (*on_button)(uint8_t mask, bool down); /* edge: one per changed bit */
	void (*on_touch)(const struct siri_finger *fingers, size_t n, bool pressed);
	void (*on_swipe)(enum siri_swipe dir);
	void (*on_battery)(uint8_t percent);
	void (*on_power)(enum siri_power state);
	void (*on_voice_start)(void);
	/* One 20 ms frame, codec-tagged, still encoded (Speex or Opus). */
	void (*on_voice_frame)(enum siri_codec codec, const uint8_t *data, size_t len);
	void (*on_voice_end)(const struct siri_voice_stats *stats);
	void (*on_disconnected)(uint8_t reason);
};

struct siri_remote_config {
	/* Remote BD address as a string, e.g. "DC:2B:2A:E0:18:71". */
	const char *address;
	/* Address type: true = public (the Siri Remote is public). */
	bool addr_public;
	const struct siri_remote_cb *cb;
};

/*
 * Start the central. Enables Bluetooth (if not already), loads bonds from
 * settings, and begins connecting to the configured remote - retrying while it
 * waits for a button press to wake it. Returns 0 or a negative errno.
 */
int siri_remote_start(const struct siri_remote_config *cfg);

/* Stop and disconnect. */
int siri_remote_stop(void);

/* True once connected and unlocked. */
bool siri_remote_connected(void);

#ifdef __cplusplus
}
#endif

#endif /* SIRI_REMOTE_H_ */
