"""Paths, defaults, and download URLs for the Voice plugin.

Everything mutable lives under the plugin DATA dir (the plugin directory itself
is read-only — PLUGINS.md §1):

    <data>/piper/            extracted Piper binary + espeak-ng-data
    <data>/voices/           <voice>.onnx (+ .onnx.json) TTS voice models
    <data>/whisper/          faster-whisper (CTranslate2) STT model cache
"""

from __future__ import annotations

import os
import platform
from dataclasses import dataclass

# --- defaults (overridable via env, mostly for development) ----------------

# faster-whisper model size. "base" (~145 MB) is a good speed/quality balance on
# CPU; "small" is more accurate but ~3x slower. Multilingual by default.
DEFAULT_WHISPER_MODEL = os.environ.get("OPENPAL_WHISPER_MODEL", "base")

# A Piper voice id in the canonical "<locale>-<name>-<quality>" form. amy-medium
# is a natural US-English voice at ~63 MB.
DEFAULT_VOICE = os.environ.get("OPENPAL_SPEECH_VOICE", "en_US-amy-medium")

# --- pinned download sources -----------------------------------------------

# Official prebuilt Piper binaries (rhasspy/piper releases). Pinned to an
# immutable tag for reproducibility; bump deliberately and re-verify.
PIPER_VERSION = "2023.11.14-2"
PIPER_RELEASE_BASE = (
    f"https://github.com/rhasspy/piper/releases/download/{PIPER_VERSION}"
)

# The macOS Piper asset above ships the `piper` binary but — uniquely among the
# platforms — OMITS its bundled shared libraries (only the onnxruntime .dSYM is
# present, not the .dylib). The binary links them via @rpath and has no LC_RPATH,
# so it aborts at launch ("dyld: Library not loaded: @rpath/libespeak-ng.1.dylib").
# We supply the missing libs (libespeak-ng, libonnxruntime, libpiper_phonemize)
# from the matching piper-phonemize release — built from the same toolchain and
# versions — and point Piper at them via DYLD_LIBRARY_PATH (see tts.synthesize).
# Linux/Windows Piper archives bundle their libraries, so this is macOS-only.
PIPER_PHONEMIZE_VERSION = "2023.11.14-4"
PIPER_PHONEMIZE_RELEASE_BASE = (
    f"https://github.com/rhasspy/piper-phonemize/releases/download/{PIPER_PHONEMIZE_VERSION}"
)

# The leaf names the macOS `piper` binary loads via @rpath. All three must be
# present in piper_lib_dir for Piper to launch; setup keys its idempotency /
# self-heal off them (a single one is not enough — a partial extract is broken).
REQUIRED_PIPER_LIBS = (
    "libespeak-ng.1.dylib",
    "libonnxruntime.1.14.1.dylib",
    "libpiper_phonemize.1.dylib",
)

# Piper voices live in the rhasspy/piper-voices HF repo, pinned to a tag.
VOICE_REPO_BASE = "https://huggingface.co/rhasspy/piper-voices/resolve/v1.0.0"


def piper_asset(system: str | None = None, machine: str | None = None) -> str:
    """The Piper release asset filename for this platform."""
    system = system or platform.system()
    machine = (machine or platform.machine()).lower()
    if system == "Darwin":
        if machine in ("arm64", "aarch64"):
            return "piper_macos_aarch64.tar.gz"
        return "piper_macos_x64.tar.gz"
    if system == "Linux":
        if machine in ("arm64", "aarch64"):
            return "piper_linux_aarch64.tar.gz"
        return "piper_linux_x86_64.tar.gz"
    if system == "Windows":
        return "piper_windows_amd64.zip"
    raise RuntimeError(f"Unsupported platform for Piper: {system}/{machine}")


def piper_url(system: str | None = None, machine: str | None = None) -> str:
    return f"{PIPER_RELEASE_BASE}/{piper_asset(system, machine)}"


def piper_phonemize_asset(
    system: str | None = None, machine: str | None = None
) -> str:
    """The piper-phonemize release asset that carries the macOS dylibs Piper
    needs. Only macOS is supported (other platforms bundle their own libs)."""
    system = system or platform.system()
    machine = (machine or platform.machine()).lower()
    if system != "Darwin":
        raise RuntimeError(
            f"piper-phonemize libs are only needed on macOS, not {system}"
        )
    if machine in ("arm64", "aarch64"):
        return "piper-phonemize_macos_aarch64.tar.gz"
    return "piper-phonemize_macos_x64.tar.gz"


def piper_phonemize_url(
    system: str | None = None, machine: str | None = None
) -> str:
    return f"{PIPER_PHONEMIZE_RELEASE_BASE}/{piper_phonemize_asset(system, machine)}"


def _voice_dir_url(voice: str) -> str:
    """The repo directory URL for a "<locale>-<name>-<quality>" voice id, e.g.
    .../en/en_US/amy/medium. Raises ValueError on a malformed id."""
    parts = voice.split("-")
    if len(parts) != 3:
        raise ValueError(f"unrecognized voice id: {voice!r}")
    locale, name, quality = parts  # e.g. en_US, amy, medium
    lang = locale.split("_")[0]  # e.g. en
    return f"{VOICE_REPO_BASE}/{lang}/{locale}/{name}/{quality}"


def voice_urls(voice: str = DEFAULT_VOICE) -> tuple[str, str]:
    """(onnx_url, onnx_json_url) for a "<locale>-<name>-<quality>" voice id."""
    base = f"{_voice_dir_url(voice)}/{voice}"
    return base + ".onnx", base + ".onnx.json"


def voice_sample_url(voice: str = DEFAULT_VOICE) -> str:
    """URL of a short MP3 sample for a voice id. rhasspy publishes one per voice
    in the same pinned repo `voice_urls` resolves against; the app fetches it so
    the user can preview a voice before downloading the (much larger) model."""
    return f"{_voice_dir_url(voice)}/samples/speaker_0.mp3"


@dataclass
class Config:
    data_dir: str
    whisper_model: str = DEFAULT_WHISPER_MODEL
    voice: str = DEFAULT_VOICE

    @property
    def whisper_dir(self) -> str:
        return os.path.join(self.data_dir, "whisper")

    @property
    def voices_dir(self) -> str:
        return os.path.join(self.data_dir, "voices")

    @property
    def piper_dir(self) -> str:
        # The Piper tarball's single top-level "piper/" directory extracts here.
        return os.path.join(self.data_dir, "piper")

    @property
    def piper_lib_dir(self) -> str:
        # macOS-only: the supplementary Piper shared libraries (see the
        # PIPER_PHONEMIZE_* notes above). Piper is launched with this on
        # DYLD_LIBRARY_PATH so dyld can resolve its @rpath/*.dylib dependencies.
        return os.path.join(self.piper_dir, "lib")

    def voice_path(self, voice: str | None = None) -> str:
        return os.path.join(self.voices_dir, (voice or self.voice) + ".onnx")

    def piper_binary(self) -> str:
        exe = "piper.exe" if platform.system() == "Windows" else "piper"
        return os.path.join(self.piper_dir, exe)

    def espeak_data_dir(self) -> str:
        return os.path.join(self.piper_dir, "espeak-ng-data")


def config_from_env(data_dir: str | None = None) -> Config:
    data_dir = data_dir or os.environ.get("OPENPAL_PLUGIN_DATA", "")
    if not data_dir:
        raise RuntimeError("no --data-dir / OPENPAL_PLUGIN_DATA")
    return Config(data_dir=data_dir)
