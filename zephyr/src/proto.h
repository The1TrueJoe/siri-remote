/*
 * Pure protocol decoding for the Siri Remote input characteristic (0x0023).
 * No Zephyr dependencies, so it can be unit-tested on the host. Mirrors
 * PROTOCOL.md §5-6.
 */
#ifndef SIRI_PROTO_H_
#define SIRI_PROTO_H_

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#include "siri_remote.h"

#define SIRI_AUDIO_MARKER 0x10
#define SIRI_TOUCH_MARKER 0x32 /* data[2] in a touch report */

enum siri_report_kind {
	SIRI_REPORT_BUTTONS,
	SIRI_REPORT_TOUCH,
	SIRI_REPORT_AUDIO,
	SIRI_REPORT_AUDIO_END, /* 2-byte 0x10 .. (A1962 stream end) */
	SIRI_REPORT_UNKNOWN,
};

struct siri_report {
	enum siri_report_kind kind;
	uint8_t buttons;            /* byte 1 bitmask, valid for every kind */
	/* touch */
	struct siri_finger fingers[2];
	size_t n_fingers;
	bool pressed;
	/* audio */
	enum siri_codec codec;
	const uint8_t *frame; /* points into the input buffer */
	size_t frame_len;
	float stream_time;    /* Speex only; <0 if absent */
};

/* Decode one notification payload from handle 0x0023. */
void siri_proto_parse(const uint8_t *data, size_t len, struct siri_report *out);

/* Decode one 7-byte finger record. */
struct siri_finger siri_proto_finger(const uint8_t *f);

#endif /* SIRI_PROTO_H_ */
