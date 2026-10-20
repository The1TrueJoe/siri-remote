#!/usr/bin/env python3
"""End-to-end check: captured Opus frames -> PCM -> transcript.

Exercises the whole voice path with no hardware attached.
"""
import sys
import wave

import transcribe
from opus_wav import OpusDecoder
from siri_protocol import AUDIO_SAMPLE_RATE


def from_frames(path):
    pcm = bytearray()
    with OpusDecoder(AUDIO_SAMPLE_RATE, 1) as dec:
        for line in open(path):
            line = line.strip()
            if not line:
                continue
            raw = bytes(int(b, 16) for b in line.split())
            audio, _ = dec.decode(raw[1:1 + raw[0]])
            pcm += audio
    return bytes(pcm)


def main():
    src = sys.argv[1] if len(sys.argv) > 1 else "frames.txt"
    print(f"vosk available: {transcribe.available()}")
    if not transcribe.available():
        return 1

    pcm = from_frames(src) if not src.endswith(".wav") else \
        wave.open(src).readframes(10 ** 9)
    secs = len(pcm) / 2 / AUDIO_SAMPLE_RATE
    print(f"{len(pcm)} PCM bytes = {secs:.2f}s\n")

    t = transcribe.StreamingTranscriber(AUDIO_SAMPLE_RATE)
    chunk = 640
    for i in range(0, len(pcm), chunk):
        done = t.feed(pcm[i:i + chunk])
        if done:
            print(f"  utterance: {done!r}")
    print(f"  last partial: {t.partial()!r}")
    print(f"\nFINAL TRANSCRIPT: {t.final()!r}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
