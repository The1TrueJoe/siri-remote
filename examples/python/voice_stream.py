#!/usr/bin/env python3
"""Consume the live microphone stream while the Siri button is held.

The remote emits 16 kHz mono Opus, which siri-events decodes to raw PCM and
writes to a FIFO as it arrives — so you can transcribe during the press rather
than after it. What you do with the audio is your business; this just proves
it flows.

    mkfifo /tmp/siri.pcm
    siri-events listen DC:2B:2A:E0:18:71 --pcm-fifo /tmp/siri.pcm &
    ./voice_stream.py /tmp/siri.pcm

To feed a recogniser instead, replace the body of `on_chunk`. The format is
exactly what Vosk, whisper.cpp and friends expect, so no resampling is needed.
"""
import struct
import sys

SAMPLE_RATE = 16000
CHUNK_SAMPLES = 320  # 20 ms, one Opus frame from the remote


def on_chunk(pcm):
    """Called with a list of int16 samples, 20 ms at a time."""
    peak = max(abs(s) for s in pcm) if pcm else 0
    bar = "#" * min(40, peak // 400)
    print(f"\r{peak:6d} {bar:<40}", end="", flush=True)


def main():
    path = sys.argv[1] if len(sys.argv) > 1 else "/tmp/siri.pcm"
    print(f"reading PCM from {path} — hold Siri and speak")
    with open(path, "rb") as f:
        while True:
            raw = f.read(CHUNK_SAMPLES * 2)
            if not raw:
                continue  # writer between utterances; keep the FIFO open
            samples = struct.unpack(f"<{len(raw) // 2}h", raw)
            on_chunk(list(samples))


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        print()
