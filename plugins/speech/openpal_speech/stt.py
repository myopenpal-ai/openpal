"""Speech-to-text with faster-whisper.

The model is loaded lazily on the first transcription (so the service can report
healthy immediately on start) and cached for the process lifetime.
"""

from __future__ import annotations

import os
import sys
import tempfile
import threading

from .config import Config

_model = None
_lock = threading.Lock()


def _log(msg: str) -> None:
    print(f"[stt] {msg}", file=sys.stderr, flush=True)


def get_model(cfg: Config):
    """Load (once) and return the WhisperModel."""
    global _model
    if _model is None:
        with _lock:
            if _model is None:
                from faster_whisper import WhisperModel  # heavy import, defer it

                _log(f"loading whisper '{cfg.whisper_model}' (int8, cpu)…")
                _model = WhisperModel(
                    cfg.whisper_model,
                    device="cpu",
                    compute_type="int8",
                    download_root=cfg.whisper_dir,
                )
                _log("whisper ready")
    return _model


def transcribe(cfg: Config, wav_bytes: bytes, language: str | None = None) -> str:
    """Transcribe WAV-encoded audio (any sample rate; mono or stereo — Whisper
    resamples internally) to text."""
    if not wav_bytes:
        return ""
    model = get_model(cfg)
    # faster-whisper reads a file path most reliably (via PyAV/ffmpeg), so spill
    # the request body to a temp file and clean it up after.
    fd, path = tempfile.mkstemp(suffix=".wav")
    try:
        with os.fdopen(fd, "wb") as fh:
            fh.write(wav_bytes)
        segments, _info = model.transcribe(
            path,
            language=language,
            beam_size=1,
            vad_filter=False,
        )
        return "".join(seg.text for seg in segments).strip()
    finally:
        try:
            os.unlink(path)
        except OSError:
            pass


def _reset_for_tests() -> None:
    global _model
    _model = None
