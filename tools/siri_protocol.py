#!/usr/bin/env python3
"""Siri Remote (1st gen, A1513) input-report protocol.

Pure parsing, no I/O, so it can be tested without hardware.

Everything arrives as notifications on ONE characteristic (value handle
0x0023) and is distinguished by payload length:

    2 bytes    button bitmask only
    13 bytes   one finger on the touchpad
    20 bytes   two fingers
    ~101 bytes Opus-encoded microphone audio (while Siri is held)

Layout and constants verified against:
  https://github.com/Yanndroid/SiriRemote-Linux
  https://github.com/wiz78/SiriRemote-HomeAssistant
"""

# --- GATT handles (identical on our unit; confirmed by our own GATT dump) ---
H_ENABLE = 0x001D          # write 0xAF here to unlock the input stream
H_INPUT = 0x0023           # notifications: buttons / touch / audio
H_INPUT_CCCD = 0x0024
H_BATTERY = 0x0028
H_BATTERY_CCCD = 0x0029
H_POWER = 0x002B
H_POWER_CCCD = 0x002C
# Apple vendor service 8341f2b4-...; characteristic 30e69638-... is r/w/notify.
# Its purpose is undocumented; subscribed so we can see whether anything (e.g.
# microphone audio) is routed here rather than through the HID report.
H_VENDOR = 0x003A
H_VENDOR_CCCD = 0x003B
ENABLE_BYTE = 0xAF

# The remote needs an ATT MTU large enough for a 101-byte notification
# (101 payload + 3 bytes of ATT header). The reference implementation uses
# exactly 104; anything >= that works.
MIN_MTU = 104

BUTTONS = [
    (0x01, "AIRPLAY"),
    (0x02, "VOLUME_UP"),
    (0x04, "VOLUME_DOWN"),
    (0x08, "PLAY_PAUSE"),
    (0x10, "SIRI"),
    (0x20, "MENU"),
    (0x40, "TOUCHPAD_2"),   # synthesised: two-finger click
    (0x80, "TOUCHPAD"),
]

SIRI_BIT = 0x10
TOUCHPAD_BIT = 0x80
TOUCHPAD_2_BIT = 0x40
TOUCH_EVENT_MARKER = 50     # data[2] == 50 marks a touchpad report

POWER_CHARGING = 0xAB
POWER_DISCHARGING = 0xAF
POWER_PLUGGED_IN = 0xBB

# Opus TOC for this remote: 0xB8 = config 23, mono, 1 frame
# config 23 -> CELT-only, wideband (16 kHz), 20 ms frames (320 samples)
OPUS_TOC_CONFIG = 23
AUDIO_SAMPLE_RATE = 16000
AUDIO_CHANNELS = 1
AUDIO_FRAME_SAMPLES = 320


def decode_buttons(mask):
    return {name for bit, name in BUTTONS if mask & bit}


def decode_finger(f):
    """Decode one 7-byte finger record into (x, y, pressure)."""
    x = int((f[0] + 255 * (f[1] & 7) - 230) / 15)
    y = (f[2] if f[2] & 128 else f[2] + 255) - 188
    p = f[5]
    return x, y, p


def is_opus_toc(byte):
    return (byte >> 3) == OPUS_TOC_CONFIG


def extract_opus(data):
    """Pull the length-prefixed Opus packet out of an audio notification.

    The frame is laid out as [len][TOC ...opus...]. Rather than hard-coding an
    offset -- which the public captures describe only loosely -- we locate the
    Opus TOC byte and validate it against the length prefix that precedes it.
    That is self-checking: a false positive would have to be a byte that both
    looks like a TOC and is preceded by a byte matching the remaining length.
    """
    for i in range(1, len(data)):
        if not is_opus_toc(data[i]):
            continue
        length = data[i - 1]
        if length == 0:
            continue
        if i + length <= len(data):
            return bytes(data[i:i + length]), i
    return None, None


def parse(data):
    """Turn one notification payload into a structured event dict."""
    n = len(data)

    if n >= 40:
        opus, offset = extract_opus(data)
        return {
            "kind": "audio",
            "length": n,
            "opus": opus,
            "opus_offset": offset,
            "raw": bytes(data),
        }

    if n < 2:
        return {"kind": "unknown", "length": n, "raw": bytes(data)}

    mask = data[1]
    # a two-finger click reports as touchpad with finger count 2
    if n >= 3 and data[0] == 2 and mask & TOUCHPAD_BIT:
        mask = (mask & ~TOUCHPAD_BIT) | TOUCHPAD_2_BIT

    event = {
        "kind": "button",
        "length": n,
        "mask": mask,
        "buttons": decode_buttons(mask),
        "raw": bytes(data),
    }

    if n >= 3 and data[2] == TOUCH_EVENT_MARKER:
        fingers = []
        if n >= 13:
            fingers.append(decode_finger(data[6:13]))
        if n >= 20:
            fingers.append(decode_finger(data[13:20]))
        if fingers:
            event["kind"] = "touch"
            event["fingers"] = fingers
            event["pressed"] = bool(data[1] & TOUCHPAD_BIT)

    return event
