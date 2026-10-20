#!/usr/bin/env python3
"""Minimal libopus decoder via ctypes, plus WAV output.

ctypes rather than a Python opus binding so there is nothing to install
beyond the system libopus, and nothing to rot.
"""
import ctypes
import ctypes.util
import wave

OPUS_OK = 0
MAX_FRAME_SAMPLES = 5760      # 120 ms at 48 kHz, the safe maximum per the API


def _load():
    for name in ("libopus.so.0", "libopus.so", ctypes.util.find_library("opus")):
        if not name:
            continue
        try:
            return ctypes.CDLL(name)
        except OSError:
            continue
    raise OSError("libopus not found (apt install libopus0)")


class OpusDecoder:
    def __init__(self, sample_rate=16000, channels=1):
        self.lib = _load()
        self.sample_rate = sample_rate
        self.channels = channels

        self.lib.opus_decoder_create.argtypes = [
            ctypes.c_int32, ctypes.c_int, ctypes.POINTER(ctypes.c_int)]
        self.lib.opus_decoder_create.restype = ctypes.c_void_p
        self.lib.opus_decode.argtypes = [
            ctypes.c_void_p, ctypes.c_char_p, ctypes.c_int32,
            ctypes.POINTER(ctypes.c_int16), ctypes.c_int, ctypes.c_int]
        self.lib.opus_decode.restype = ctypes.c_int
        self.lib.opus_decoder_destroy.argtypes = [ctypes.c_void_p]
        self.lib.opus_strerror.argtypes = [ctypes.c_int]
        self.lib.opus_strerror.restype = ctypes.c_char_p

        err = ctypes.c_int()
        self.dec = self.lib.opus_decoder_create(
            sample_rate, channels, ctypes.byref(err))
        if err.value != OPUS_OK or not self.dec:
            raise RuntimeError(f"opus_decoder_create: {self.strerror(err.value)}")

        self._buf = (ctypes.c_int16 * (MAX_FRAME_SAMPLES * channels))()

    def strerror(self, code):
        return self.lib.opus_strerror(code).decode()

    def decode(self, packet):
        """Decode one Opus packet -> PCM bytes (16-bit little-endian)."""
        n = self.lib.opus_decode(
            self.dec, packet, len(packet), self._buf, MAX_FRAME_SAMPLES, 0)
        if n < 0:
            raise RuntimeError(f"opus_decode: {self.strerror(n)}")
        return bytes(memoryview(self._buf)[: n * self.channels].cast("B")), n

    def close(self):
        if getattr(self, "dec", None):
            self.lib.opus_decoder_destroy(self.dec)
            self.dec = None

    def __enter__(self):
        return self

    def __exit__(self, *a):
        self.close()


def write_wav(path, pcm, sample_rate=16000, channels=1):
    with wave.open(path, "wb") as w:
        w.setnchannels(channels)
        w.setsampwidth(2)
        w.setframerate(sample_rate)
        w.writeframes(pcm)
    return path


def decode_packets(packets, sample_rate=16000, channels=1):
    """Decode a list of Opus packets into one PCM buffer."""
    out = bytearray()
    frames = 0
    with OpusDecoder(sample_rate, channels) as dec:
        for p in packets:
            pcm, n = dec.decode(p)
            out += pcm
            frames += n
    return bytes(out), frames
