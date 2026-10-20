#!/usr/bin/env python3
"""Validate the Opus pipeline against real captured Siri Remote frames.

Runs with no hardware: the frames come from the reference project's capture,
so a clean decode here proves our decoder settings and framing are right
before the remote is even available.

Usage:  python3 test_audio_decode.py frames.txt [out.wav]
"""
import struct
import sys

from opus_wav import OpusDecoder, write_wav
from siri_protocol import extract_opus, is_opus_toc, AUDIO_SAMPLE_RATE


def main():
    path = sys.argv[1] if len(sys.argv) > 1 else "frames.txt"
    out = sys.argv[2] if len(sys.argv) > 2 else "decoded.wav"

    lines = [l.strip() for l in open(path) if l.strip()]
    print(f"{len(lines)} captured frame(s)\n")

    pcm = bytearray()
    ok = failed = 0
    total_samples = 0

    with OpusDecoder(AUDIO_SAMPLE_RATE, 1) as dec:
        for i, line in enumerate(lines, 1):
            raw = bytes(int(b, 16) for b in line.split())
            declared = raw[0]
            toc = raw[1]

            # cross-check the self-locating extractor against the naive layout
            found, offset = extract_opus(raw)

            packet = raw[1:1 + declared]
            note = ""
            if len(packet) < declared:
                note = f"  (truncated: have {len(packet)} of {declared})"

            try:
                audio, nsamples = dec.decode(packet)
                pcm += audio
                total_samples += nsamples
                ok += 1
                if i <= 3:
                    ms = nsamples * 1000 / AUDIO_SAMPLE_RATE
                    print(f"  frame {i}: len={declared} toc=0x{toc:02X} "
                          f"config={toc >> 3} -> {nsamples} samples "
                          f"({ms:.0f} ms){note}")
                    print(f"           extractor found offset={offset}, "
                          f"{len(found) if found else 0} bytes -- "
                          f"{'AGREES' if found == packet else 'DIFFERS'}")
            except Exception as e:
                failed += 1
                if failed <= 3:
                    print(f"  frame {i}: FAILED {e}{note}")

    print(f"\ndecoded {ok}/{len(lines)} frames, {failed} failed")
    if not ok:
        print("no audio decoded")
        return 1

    secs = total_samples / AUDIO_SAMPLE_RATE
    print(f"total {total_samples} samples = {secs:.2f}s at {AUDIO_SAMPLE_RATE} Hz")

    # crude signal check: silence would be all zeros
    samples = struct.unpack(f"<{len(pcm) // 2}h", bytes(pcm))
    peak = max(abs(s) for s in samples)
    rms = (sum(s * s for s in samples) / len(samples)) ** 0.5
    print(f"peak amplitude {peak}, RMS {rms:.1f} "
          f"-- {'REAL AUDIO' if rms > 50 else 'suspiciously quiet'}")

    write_wav(out, bytes(pcm), AUDIO_SAMPLE_RATE, 1)
    print(f"wrote {out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
