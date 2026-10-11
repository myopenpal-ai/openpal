"""Text-to-speech with the Piper binary.

We shell out to the prebuilt `piper` executable (downloaded at setup) rather than
the `piper-tts` pip package, whose `piper-phonemize` dependency has no macOS
arm64 wheel. Piper reads the utterance on stdin and writes a WAV file; we return
its bytes. The model is small and loads fast, so there is no warm-up step.
"""

from __future__ import annotations

import io
import os
import subprocess
import sys
import tempfile
import wave

from .config import Config


def _log(msg: str) -> None:
    print(f"[tts] {msg}", file=sys.stderr, flush=True)


def silence_wav(seconds: float = 0.2, sample_rate: int = 22050) -> bytes:
    """A short silent mono WAV — returned for empty input so the caller always
    gets valid audio."""
    buf = io.BytesIO()
    with wave.open(buf, "wb") as wf:
        wf.setnchannels(1)
        wf.setsampwidth(2)
        wf.setframerate(sample_rate)
        wf.writeframes(b"\x00\x00" * int(sample_rate * seconds))
    return buf.getvalue()


def _piper_env(cfg: Config) -> dict | None:
    """Environment for the Piper subprocess.

    On macOS the prebuilt Piper binary loads its libraries via `@rpath/…` but
    carries no LC_RPATH, and the macOS asset omits the libraries anyway. Setup
    drops them into `cfg.piper_lib_dir`; pointing DYLD_LIBRARY_PATH there lets
    dyld resolve them by leaf name. Returns None (inherit the parent env) off
    macOS or before the lib dir exists.
    """
    if sys.platform != "darwin":
        return None
    libdir = cfg.piper_lib_dir
    if not os.path.isdir(libdir):
        return None
    env = dict(os.environ)
    existing = env.get("DYLD_LIBRARY_PATH", "")
    env["DYLD_LIBRARY_PATH"] = libdir + (os.pathsep + existing if existing else "")
    return env


# Piper's tunable synthesis controls and the safe range we clamp each to. The
# values that reach us originate in the user's Settings sliders, but a
# hand-edited settings file (or a future caller) could send anything, so we
# never trust them unbounded. A control left as None is omitted entirely,
# leaving Piper's own built-in default for it untouched.
_PIPER_BOUNDS = {
    "length_scale": (0.25, 4.0),  # phoneme length; >1 slower, <1 faster
    "noise_scale": (0.0, 2.0),  # audio variability (timbre)
    "noise_w": (0.0, 2.0),  # phoneme-duration variability (cadence)
    "sentence_silence": (0.0, 5.0),  # seconds of pause between sentences
}


def _scale_flags(
    length_scale: float | None,
    noise_scale: float | None,
    noise_w: float | None,
    sentence_silence: float | None,
) -> list[str]:
    """Piper CLI flags for the given synthesis controls, each clamped to its
    safe range. A None (or non-numeric) control is skipped, so Piper falls back
    to its built-in default for it."""
    flags: list[str] = []
    for name, value in (
        ("length_scale", length_scale),
        ("noise_scale", noise_scale),
        ("noise_w", noise_w),
        ("sentence_silence", sentence_silence),
    ):
        if value is None:
            continue
        try:
            num = float(value)
        except (TypeError, ValueError):
            _log(f"ignoring non-numeric {name}={value!r}")
            continue
        lo, hi = _PIPER_BOUNDS[name]
        flags += [f"--{name}", f"{max(lo, min(hi, num)):.4f}"]
    return flags


def synthesize(
    cfg: Config,
    text: str,
    voice: str | None = None,
    *,
    length_scale: float | None = None,
    noise_scale: float | None = None,
    noise_w: float | None = None,
    sentence_silence: float | None = None,
) -> bytes:
    """Render `text` to a WAV byte string using Piper.

    The keyword-only controls map straight to Piper's synthesis flags (see
    `_PIPER_BOUNDS`); each is clamped, and any left None keeps Piper's default.
    """
    text = (text or "").strip()
    if not text:
        return silence_wav()

    model = cfg.voice_path(voice)
    if not os.path.isfile(model):
        raise RuntimeError(f"voice model not found at {model}; run setup")
    piper = cfg.piper_binary()
    if not os.path.isfile(piper):
        raise RuntimeError(f"piper binary not found at {piper}; run setup")

    fd, out_path = tempfile.mkstemp(suffix=".wav")
    os.close(fd)
    try:
        cmd = [piper, "--model", model, "--output_file", out_path]
        # Point Piper at its bundled phoneme data when present (some builds need
        # this when not launched from their own directory).
        espeak = cfg.espeak_data_dir()
        if os.path.isdir(espeak):
            cmd += ["--espeak_data", espeak]
        cmd += _scale_flags(length_scale, noise_scale, noise_w, sentence_silence)
        proc = subprocess.run(
            cmd,
            input=text.encode("utf-8"),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            cwd=cfg.piper_dir,
            env=_piper_env(cfg),
        )
        if proc.returncode != 0:
            detail = proc.stderr.decode("utf-8", "replace").strip()[-500:]
            raise RuntimeError(f"piper exited {proc.returncode}: {detail}")
        with open(out_path, "rb") as fh:
            data = fh.read()
        if not data:
            raise RuntimeError("piper produced no audio")
        return data
    finally:
        try:
            os.unlink(out_path)
        except OSError:
            pass
