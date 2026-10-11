"""Unit tests for the small building blocks (no models, no network)."""

from __future__ import annotations

import os
import tarfile
import wave
import io

import pytest

import setup_plugin
from openpal_speech import config as cfgmod
from openpal_speech import tts
from openpal_speech import voices
from openpal_speech.tts import silence_wav


# --- platform asset selection ---------------------------------------------

@pytest.mark.parametrize(
    "system,machine,expected",
    [
        ("Darwin", "arm64", "piper_macos_aarch64.tar.gz"),
        ("Darwin", "x86_64", "piper_macos_x64.tar.gz"),
        ("Linux", "x86_64", "piper_linux_x86_64.tar.gz"),
        ("Linux", "aarch64", "piper_linux_aarch64.tar.gz"),
        ("Windows", "AMD64", "piper_windows_amd64.zip"),
    ],
)
def test_piper_asset(system, machine, expected):
    assert cfgmod.piper_asset(system, machine) == expected


def test_piper_asset_unsupported():
    with pytest.raises(RuntimeError):
        cfgmod.piper_asset("Plan9", "pdp11")


def test_piper_url_uses_pinned_version():
    url = cfgmod.piper_url("Darwin", "arm64")
    assert cfgmod.PIPER_VERSION in url
    assert url.endswith("piper_macos_aarch64.tar.gz")


# --- piper-phonemize (macOS dylib supplement) ------------------------------

@pytest.mark.parametrize(
    "machine,expected",
    [
        ("arm64", "piper-phonemize_macos_aarch64.tar.gz"),
        ("aarch64", "piper-phonemize_macos_aarch64.tar.gz"),
        ("x86_64", "piper-phonemize_macos_x64.tar.gz"),
    ],
)
def test_piper_phonemize_asset(machine, expected):
    assert cfgmod.piper_phonemize_asset("Darwin", machine) == expected


def test_piper_phonemize_asset_non_macos():
    # The supplement only exists for macOS; other platforms bundle their libs.
    for system in ("Linux", "Windows"):
        with pytest.raises(RuntimeError):
            cfgmod.piper_phonemize_asset(system, "x86_64")


def test_piper_phonemize_url_uses_pinned_version():
    url = cfgmod.piper_phonemize_url("Darwin", "arm64")
    assert cfgmod.PIPER_PHONEMIZE_VERSION in url
    assert "piper-phonemize" in url
    assert url.endswith("piper-phonemize_macos_aarch64.tar.gz")


# --- voice url construction -----------------------------------------------

def test_voice_urls():
    onnx, meta = cfgmod.voice_urls("en_US-amy-medium")
    assert onnx.endswith("/en/en_US/amy/medium/en_US-amy-medium.onnx")
    assert meta == onnx + ".json"


def test_voice_urls_other_locale():
    onnx, _ = cfgmod.voice_urls("de_DE-thorsten-low")
    assert onnx.endswith("/de/de_DE/thorsten/low/de_DE-thorsten-low.onnx")


def test_voice_urls_bad_id():
    with pytest.raises(ValueError):
        cfgmod.voice_urls("only-two")  # needs <locale>-<name>-<quality> (3 parts)


# --- config paths ----------------------------------------------------------

def test_config_paths():
    cfg = cfgmod.Config(data_dir="/var/data/speech")
    assert cfg.whisper_dir == "/var/data/speech/whisper"
    assert cfg.voices_dir == "/var/data/speech/voices"
    assert cfg.piper_dir == "/var/data/speech/piper"
    assert cfg.piper_lib_dir == "/var/data/speech/piper/lib"
    assert cfg.voice_path() == "/var/data/speech/voices/en_US-amy-medium.onnx"
    assert cfg.voice_path("x_Y-z-low") == "/var/data/speech/voices/x_Y-z-low.onnx"


def test_config_from_env(monkeypatch):
    monkeypatch.setenv("OPENPAL_PLUGIN_DATA", "/tmp/x")
    assert cfgmod.config_from_env().data_dir == "/tmp/x"


def test_config_from_env_missing(monkeypatch):
    monkeypatch.delenv("OPENPAL_PLUGIN_DATA", raising=False)
    with pytest.raises(RuntimeError):
        cfgmod.config_from_env()


# --- piper subprocess environment (macOS dylib resolution) -----------------

def test_piper_env_prepends_dyld_on_macos(tmp_path, monkeypatch):
    monkeypatch.setattr(tts.sys, "platform", "darwin")
    cfg = cfgmod.Config(data_dir=str(tmp_path))
    os.makedirs(cfg.piper_lib_dir)
    monkeypatch.setenv("DYLD_LIBRARY_PATH", "/pre/existing")
    env = tts._piper_env(cfg)
    assert env is not None
    parts = env["DYLD_LIBRARY_PATH"].split(os.pathsep)
    assert parts[0] == cfg.piper_lib_dir       # our lib dir comes first
    assert "/pre/existing" in parts            # inherited value preserved


def test_piper_env_none_when_libdir_absent(tmp_path, monkeypatch):
    monkeypatch.setattr(tts.sys, "platform", "darwin")
    cfg = cfgmod.Config(data_dir=str(tmp_path))  # piper/lib not created
    assert tts._piper_env(cfg) is None


def test_piper_env_none_off_macos(tmp_path, monkeypatch):
    monkeypatch.setattr(tts.sys, "platform", "linux")
    cfg = cfgmod.Config(data_dir=str(tmp_path))
    os.makedirs(cfg.piper_lib_dir)
    assert tts._piper_env(cfg) is None


# --- piper-phonemize lib extraction ----------------------------------------

_LIB_PREFIX = "piper-phonemize/lib/"


def _make_phonemize_tar(members):
    """Build an in-memory piper-phonemize-shaped .tar.gz.

    `members` is a list of (name, kind, payload):
      ("foo.dylib", "file", b"...")  or  ("foo.1.dylib", "link", "target").
    """
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w:gz") as tf:
        for name, kind, payload in members:
            ti = tarfile.TarInfo(name)
            if kind == "file":
                ti.size = len(payload)
                tf.addfile(ti, io.BytesIO(payload))
            else:
                ti.type = tarfile.SYMTYPE
                ti.linkname = payload
                tf.addfile(ti)
    buf.seek(0)
    return buf


def test_extract_phonemize_libs_flattens_and_keeps_symlink_chain(tmp_path):
    buf = _make_phonemize_tar([
        (_LIB_PREFIX + "libespeak-ng.1.52.0.1.dylib", "file", b"espeak"),
        (_LIB_PREFIX + "libespeak-ng.1.dylib", "link", "libespeak-ng.1.52.0.1.dylib"),
        (_LIB_PREFIX + "libonnxruntime.1.14.1.dylib", "file", b"onnx"),
        (_LIB_PREFIX + "libpiper_phonemize.1.2.0.dylib", "file", b"phon"),
        (_LIB_PREFIX + "libpiper_phonemize.1.dylib", "link", "libpiper_phonemize.1.2.0.dylib"),
        # bulky extras that must be skipped:
        (_LIB_PREFIX + "libonnxruntime.1.14.1.dylib.dSYM/Contents/DWARF/x", "file", b"dwarf"),
        (_LIB_PREFIX + "pkgconfig/espeak-ng.pc", "file", b"pc"),
        ("piper-phonemize/share/espeak-ng-data/x", "file", b"data"),
    ])
    dest = str(tmp_path / "lib")
    os.makedirs(dest)
    with tarfile.open(fileobj=buf, mode="r:gz") as tf:
        setup_plugin._extract_phonemize_libs(tf, dest)

    # All three required libs resolve (the .1 symlinks chase to real files).
    assert setup_plugin._required_libs_present(dest)
    assert os.path.islink(os.path.join(dest, "libespeak-ng.1.dylib"))
    # The multi-MB .dSYM bundle and pkgconfig are NOT unpacked.
    assert not os.path.exists(os.path.join(dest, "libonnxruntime.1.14.1.dylib.dSYM"))
    assert not os.path.exists(os.path.join(dest, "pkgconfig"))
    # Only flattened, top-level .dylib entries landed.
    assert all(n.endswith(".dylib") for n in os.listdir(dest))


def test_extract_phonemize_libs_rejects_escaping_symlink(tmp_path):
    buf = _make_phonemize_tar([
        (_LIB_PREFIX + "evil.dylib", "link", "../../../../etc/passwd"),
    ])
    dest = str(tmp_path / "lib")
    os.makedirs(dest)
    with tarfile.open(fileobj=buf, mode="r:gz") as tf:
        setup_plugin._extract_phonemize_libs(tf, dest)
    assert not os.path.lexists(os.path.join(dest, "evil.dylib"))


def test_required_libs_present_needs_all_three(tmp_path):
    dest = str(tmp_path)
    assert not setup_plugin._required_libs_present(dest)  # none yet
    # A single lib is not enough — a partial extract must not look "complete".
    open(os.path.join(dest, "libespeak-ng.1.dylib"), "w").close()
    assert not setup_plugin._required_libs_present(dest)
    for name in cfgmod.REQUIRED_PIPER_LIBS:
        open(os.path.join(dest, name), "w").close()
    assert setup_plugin._required_libs_present(dest)


def test_required_libs_present_dangling_symlink_is_absent(tmp_path):
    dest = str(tmp_path)
    for name in cfgmod.REQUIRED_PIPER_LIBS:
        open(os.path.join(dest, name), "w").close()
    # Replace one with a dangling symlink: os.path.exists follows it -> absent.
    target = os.path.join(dest, "libonnxruntime.1.14.1.dylib")
    os.unlink(target)
    os.symlink("nonexistent.dylib", target)
    assert not setup_plugin._required_libs_present(dest)


# --- piper synthesis control flags -----------------------------------------

def test_scale_flags_omits_none():
    # No controls -> no flags, so Piper keeps every one of its own defaults.
    assert tts._scale_flags(None, None, None, None) == []


def test_scale_flags_emits_in_order_and_formats():
    flags = tts._scale_flags(1.0, 0.667, 0.8, 0.2)
    assert flags == [
        "--length_scale", "1.0000",
        "--noise_scale", "0.6670",
        "--noise_w", "0.8000",
        "--sentence_silence", "0.2000",
    ]


def test_scale_flags_clamps_to_bounds():
    # Below/above the safe range is pulled back to the nearest bound.
    assert tts._scale_flags(99.0, None, None, None) == ["--length_scale", "4.0000"]
    assert tts._scale_flags(-5.0, None, None, None) == ["--length_scale", "0.2500"]
    assert tts._scale_flags(None, None, None, 1000.0) == [
        "--sentence_silence", "5.0000",
    ]


def test_scale_flags_skips_non_numeric():
    # A bad value is dropped rather than crashing synthesis.
    assert tts._scale_flags("fast", None, None, None) == []


# --- voice catalog ---------------------------------------------------------

def test_catalog_ids_are_wellformed_and_unique():
    # Every catalog id must parse as Piper's "<locale>-<name>-<quality>" (so
    # config.voice_urls / voice_path can resolve it) and ids must be unique.
    ids = [e["id"] for e in voices.CATALOG]
    assert len(ids) == len(set(ids))
    assert "en_US-amy-medium" in ids  # the default must be selectable
    for vid in ids:
        cfgmod.voice_urls(vid)  # raises ValueError on a malformed id
        sample = cfgmod.voice_sample_url(vid)
        assert sample.startswith("https://huggingface.co/") and sample.endswith(
            "/samples/speaker_0.mp3"
        )


def test_installed_and_list_voices_reflect_disk(tmp_path):
    cfg = cfgmod.Config(data_dir=str(tmp_path))
    os.makedirs(cfg.voices_dir, exist_ok=True)
    # Nothing on disk -> nothing installed.
    assert voices.installed_ids(cfg) == set()
    assert all(not v["installed"] for v in voices.list_voices(cfg))

    # A voice needs BOTH files to count as installed.
    open(cfg.voice_path("en_US-amy-medium"), "wb").close()
    assert voices.installed_ids(cfg) == set()  # missing the .onnx.json
    open(cfg.voice_path("en_US-amy-medium") + ".json", "wb").close()
    assert voices.installed_ids(cfg) == {"en_US-amy-medium"}

    listed = {v["id"]: v for v in voices.list_voices(cfg)}
    assert listed["en_US-amy-medium"]["installed"] is True
    assert listed["en_US-joe-medium"]["installed"] is False
    # Wire shape: camelCase sizeMb for the trip through Rust to the webview.
    assert "sizeMb" in listed["en_US-amy-medium"]
    # Every catalog voice carries a preview sample URL (for not-installed too).
    assert listed["en_US-joe-medium"]["previewUrl"].endswith(
        "/en/en_US/joe/medium/samples/speaker_0.mp3"
    )


def test_list_voices_includes_handinstalled_extras(tmp_path):
    cfg = cfgmod.Config(data_dir=str(tmp_path))
    os.makedirs(cfg.voices_dir, exist_ok=True)
    open(cfg.voice_path("zz_XX-custom-medium"), "wb").close()
    open(cfg.voice_path("zz_XX-custom-medium") + ".json", "wb").close()
    extra = [v for v in voices.list_voices(cfg) if v["id"] == "zz_XX-custom-medium"]
    assert extra and extra[0]["installed"] is True


def test_download_voice_rejects_noncatalog_id(tmp_path):
    cfg = cfgmod.Config(data_dir=str(tmp_path))
    # Must refuse before any URL/path is built (injection / traversal guard).
    with pytest.raises(ValueError):
        voices.download_voice(cfg, "../../etc/passwd")
    with pytest.raises(ValueError):
        voices.download_voice(cfg, "en_US-not-real")


def test_download_voice_is_noop_when_installed(tmp_path):
    cfg = cfgmod.Config(data_dir=str(tmp_path))
    os.makedirs(cfg.voices_dir, exist_ok=True)
    open(cfg.voice_path("en_US-amy-medium"), "wb").close()
    open(cfg.voice_path("en_US-amy-medium") + ".json", "wb").close()
    # Already present -> returns without touching the network.
    voices.download_voice(cfg, "en_US-amy-medium")


# --- silence wav -----------------------------------------------------------

def test_silence_wav_is_valid_wav():
    data = silence_wav(0.1, 16000)
    with wave.open(io.BytesIO(data), "rb") as wf:
        assert wf.getnchannels() == 1
        assert wf.getframerate() == 16000
        assert wf.getnframes() == 1600
