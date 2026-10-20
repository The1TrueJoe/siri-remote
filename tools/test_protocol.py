#!/usr/bin/env python3
"""Unit tests for the Siri Remote payload parser.

No hardware required. These exist so that the first real button press does not
crash the daemon at 3am.
"""
import sys

import siri_protocol as proto

failures = []


def check(name, got, want):
    ok = got == want
    print(f"  {'PASS' if ok else 'FAIL'}  {name}")
    if not ok:
        print(f"        got  {got!r}\n        want {want!r}")
        failures.append(name)


print("buttons")
check("menu", proto.parse(bytes([0x00, 0x20]))["buttons"], {"MENU"})
check("siri", proto.parse(bytes([0x00, 0x10]))["buttons"], {"SIRI"})
check("release", proto.parse(bytes([0x00, 0x00]))["buttons"], set())
check("chord vol_up+airplay",
      proto.parse(bytes([0x00, 0x03]))["buttons"], {"AIRPLAY", "VOLUME_UP"})
check("all released kind", proto.parse(bytes([0x00, 0x00]))["kind"], "button")

print("\ntwo-finger click promotion")
# finger count 2 + touchpad bit -> TOUCHPAD_2 instead of TOUCHPAD
ev = proto.parse(bytes([0x02, 0x80, 0x00]))
check("two-finger click", ev["buttons"], {"TOUCHPAD_2"})
ev = proto.parse(bytes([0x01, 0x80, 0x00]))
check("one-finger click", ev["buttons"], {"TOUCHPAD"})

print("\ntouch reports")
# 13 bytes, marker 50 at index 2, finger record at 6:13
touch = bytearray(13)
touch[0], touch[1], touch[2] = 0x01, 0x80, 50
touch[6], touch[7], touch[8], touch[11] = 230, 0, 188, 42   # x=0, y=0, p=42
ev = proto.parse(bytes(touch))
check("13-byte kind", ev["kind"], "touch")
check("one finger", len(ev["fingers"]), 1)
check("origin decode", ev["fingers"][0], (0, 0, 42))
check("pressed flag", ev["pressed"], True)

touch2 = bytearray(20)
touch2[0], touch2[1], touch2[2] = 0x02, 0x00, 50
touch2[6], touch2[7], touch2[8], touch2[11] = 230, 0, 188, 10
# x=10 needs f[0] + 255*(f[1]&7) == 380, i.e. 125 + 255*1
touch2[13], touch2[14], touch2[15], touch2[18] = 125, 1, 200, 20
ev = proto.parse(bytes(touch2))
check("20-byte kind", ev["kind"], "touch")
check("two fingers", len(ev["fingers"]), 2)
check("second finger x", ev["fingers"][1][0], 10)

print("\nsigned-wrap Y decoding")
# y bytes >=188 map to negative-ish, <128 wrap through +255
check("y at 188", proto.decode_finger(bytes([230, 0, 188, 0, 0, 0, 0]))[1], 0)
check("y at 255", proto.decode_finger(bytes([230, 0, 255, 0, 0, 0, 0]))[1], 67)
check("y at 38", proto.decode_finger(bytes([230, 0, 38, 0, 0, 0, 0]))[1], 105)

print("\naudio framing")
opus = bytes([0xB8]) + bytes(range(85))          # TOC + 85 bytes = 86 total
packet = bytes([0x00, 0x00, 0x00]) + bytes([86]) + opus   # header + len + opus
ev = proto.parse(packet)
check("audio kind", ev["kind"], "audio")
check("opus extracted", ev["opus"], opus)
check("opus length", len(ev["opus"]), 86)
check("toc recognised", proto.is_opus_toc(0xB8), True)
check("toc config", 0xB8 >> 3, 23)
check("non-toc rejected", proto.is_opus_toc(0x00), False)

print("\nrobustness (must not raise)")
for name, payload in [
    ("empty", b""),
    ("one byte", b"\x00"),
    ("short touch marker", bytes([0x01, 0x80, 50])),
    ("audio with no toc", bytes(101)),
    ("truncated audio", bytes([0, 0, 0, 200, 0xB8]) + bytes(40)),
]:
    try:
        ev = proto.parse(payload)
        print(f"  PASS  {name} -> {ev['kind']}")
    except Exception as e:
        print(f"  FAIL  {name} raised {e!r}")
        failures.append(name)

print(f"\n{'ALL PASS' if not failures else str(len(failures)) + ' FAILURE(S): ' + ', '.join(failures)}")
sys.exit(1 if failures else 0)
