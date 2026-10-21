/* See proto.h. Pure logic, mirrors PROTOCOL.md §5-6. */
#include "proto.h"

#include <string.h>

#define SPEEX_TRAILER 10 /* [len u16][4 bytes ?][stream-time f32] */

struct siri_finger siri_proto_finger(const uint8_t *f)
{
	struct siri_finger out;
	out.x = (int16_t)(((int)f[0] + 255 * (f[1] & 7) - 230) / 15);
	out.y = (int16_t)(((f[2] & 0x80) ? (int)f[2] : (int)f[2] + 255) - 188);
	out.pressure = f[5];
	return out;
}

static float read_f32_le(const uint8_t *p)
{
	uint32_t u = (uint32_t)p[0] | ((uint32_t)p[1] << 8) | ((uint32_t)p[2] << 16) |
		     ((uint32_t)p[3] << 24);
	float f;
	memcpy(&f, &u, sizeof(f));
	return f;
}

/* Speex frame extractor: layout [0x10][buttons][frame][len u16 LE][4][f32]. */
static bool extract_speex(const uint8_t *d, size_t n, const uint8_t **frame,
			  size_t *flen, float *t)
{
	if (n < 2 + 1 + SPEEX_TRAILER || d[0] != SIRI_AUDIO_MARKER) {
		return false;
	}
	size_t tpos = n - SPEEX_TRAILER;
	uint16_t len = (uint16_t)d[tpos] | ((uint16_t)d[tpos + 1] << 8);
	if (len == 0 || (size_t)(2 + len) != tpos) {
		return false;
	}
	float st = read_f32_le(&d[n - 4]);
	if (!(st >= 0.0f)) { /* also rejects NaN */
		return false;
	}
	*frame = &d[2];
	*flen = len;
	*t = st;
	return true;
}

/* Opus (A1962): TOC 0xB8 somewhere after the 2-byte header; length-prefixed. */
static bool extract_opus(const uint8_t *d, size_t n, const uint8_t **frame, size_t *flen)
{
	for (size_t i = 1; i < n; i++) {
		if ((d[i] >> 3) != 23) { /* config 23 = 0xB8 family */
			continue;
		}
		size_t len = d[i - 1];
		if (len != 0 && i + len <= n) {
			*frame = &d[i];
			*flen = len;
			return true;
		}
	}
	return false;
}

void siri_proto_parse(const uint8_t *d, size_t n, struct siri_report *o)
{
	memset(o, 0, sizeof(*o));
	o->stream_time = -1.0f;

	if (n >= 40) {
		o->kind = SIRI_REPORT_AUDIO;
		o->buttons = (d[0] == SIRI_AUDIO_MARKER) ? d[1] : 0;
		if (extract_speex(d, n, &o->frame, &o->frame_len, &o->stream_time)) {
			o->codec = SIRI_CODEC_SPEEX;
		} else if (extract_opus(d, n, &o->frame, &o->frame_len)) {
			o->codec = SIRI_CODEC_OPUS;
		} else {
			o->kind = SIRI_REPORT_UNKNOWN;
		}
		return;
	}
	if (n < 2) {
		o->kind = SIRI_REPORT_UNKNOWN;
		return;
	}
	if (n == 2 && d[0] == SIRI_AUDIO_MARKER) {
		o->kind = SIRI_REPORT_AUDIO_END;
		o->buttons = d[1];
		return;
	}

	uint8_t mask = d[1];
	/* Two-finger click -> synthetic TOUCHPAD_2 (PROTOCOL.md §5). */
	if (n >= 3 && d[0] == 2 && (mask & SIRI_BTN_TOUCHPAD)) {
		mask = (mask & ~SIRI_BTN_TOUCHPAD) | SIRI_BTN_TOUCHPAD_2;
	}
	o->buttons = mask;

	if (n >= 3 && d[2] == SIRI_TOUCH_MARKER) {
		if (n >= 13) {
			o->fingers[o->n_fingers++] = siri_proto_finger(&d[6]);
		}
		if (n >= 20) {
			o->fingers[o->n_fingers++] = siri_proto_finger(&d[13]);
		}
		if (o->n_fingers > 0) {
			o->kind = SIRI_REPORT_TOUCH;
			o->pressed = (d[1] & SIRI_BTN_TOUCHPAD) != 0;
			return;
		}
	}
	o->kind = SIRI_REPORT_BUTTONS;
}
