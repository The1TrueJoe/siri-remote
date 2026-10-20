#!/usr/bin/env python3
"""Streaming speech-to-text for Siri Remote audio, via Vosk.

The remote emits Opus at 16 kHz mono, which is exactly Vosk's required input
format -- no resampling, no format conversion. Frames are fed in as they
arrive (one per 20 ms), so a transcript is ready essentially the moment the
Siri button is released.

Vosk is optional: if it or the model is missing, `available()` is False and
callers should fall back to just writing a WAV.
"""
import json
import os

DEFAULT_MODEL = os.environ.get(
    "VOSK_MODEL", "/home/pi/models/vosk-model-small-en-us-0.15")

try:
    from vosk import Model, KaldiRecognizer, SetLogLevel
    SetLogLevel(-1)                      # keep kaldi's chatter out of the journal
    _VOSK = True
except Exception:                        # pragma: no cover - depends on host
    _VOSK = False

_model = None


def available(model_path=DEFAULT_MODEL):
    return _VOSK and os.path.isdir(model_path)


def _load(model_path):
    global _model
    if _model is None:
        _model = Model(model_path)
    return _model


class StreamingTranscriber:
    """Feed PCM as it arrives; read partials live and a transcript at the end."""

    def __init__(self, sample_rate=16000, model_path=DEFAULT_MODEL):
        if not available(model_path):
            raise RuntimeError(f"vosk unavailable or model missing: {model_path}")
        self.rec = KaldiRecognizer(_load(model_path), sample_rate)
        self.rec.SetWords(True)
        self._finals = []

    def feed(self, pcm):
        """Feed PCM bytes. Returns a completed utterance string, or None."""
        if self.rec.AcceptWaveform(pcm):
            text = json.loads(self.rec.Result()).get("text", "").strip()
            if text:
                self._finals.append(text)
                return text
        return None

    def partial(self):
        return json.loads(self.rec.PartialResult()).get("partial", "").strip()

    def final(self):
        """Flush and return the full transcript."""
        text = json.loads(self.rec.FinalResult()).get("text", "").strip()
        if text:
            self._finals.append(text)
        return " ".join(self._finals).strip()


def transcribe_pcm(pcm, sample_rate=16000, model_path=DEFAULT_MODEL):
    """One-shot convenience wrapper."""
    t = StreamingTranscriber(sample_rate, model_path)
    chunk = 640                            # 20 ms at 16 kHz, 16-bit mono
    for i in range(0, len(pcm), chunk):
        t.feed(pcm[i:i + chunk])
    return t.final()
