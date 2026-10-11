#!/usr/bin/env python3
"""First-run setup for the OpenPal Voice plugin (PLUGINS.md §3).

The framework runs this INSIDE the plugin venv (after pip-installing
requirements.txt):

    <venv>/bin/python setup_plugin.py --data-dir <data>

(cwd = plugin dir, env OPENPAL_PLUGIN_DATA=<data>, PYTHONUNBUFFERED=1).

It downloads everything the service needs, into the plugin's *data dir* (the
plugin dir itself is read-only):

  1. The prebuilt Piper TTS binary for this platform -> <data>/piper/
  2. One Piper voice model (.onnx + .onnx.json)       -> <data>/voices/
  3. The faster-whisper STT model                     -> <data>/whisper/

Progress is newline-delimited JSON on **stdout** (flushed):

    {"type":"progress","message":"…","fraction":0.4}
    {"type":"log","message":"…"}
    {"type":"done"}                       # terminal, success
    {"type":"error","message":"…"}        # terminal, user-safe failure

**stderr** is a free-form log (teed to setup.log). Idempotent: re-running with
everything already present is fast. Catches all exceptions, emits exactly one
terminal event, and exits 0.
"""

from __future__ import annotations

import argparse
import json
import os
import platform
import shutil
import sys
import tarfile
import tempfile
import traceback
import urllib.request
import zipfile

from openpal_speech import config as cfgmod


# --------------------------------------------------------------------------
# Progress plumbing
# --------------------------------------------------------------------------

def _emit(obj: dict) -> None:
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()


def progress(message: str, fraction: float | None = None) -> None:
    obj: dict = {"type": "progress", "message": message}
    if fraction is not None:
        obj["fraction"] = round(float(fraction), 3)
    _emit(obj)


def log(message: str) -> None:
    _emit({"type": "log", "message": message})


def stderr(message: str) -> None:
    print(message, file=sys.stderr, flush=True)


# --------------------------------------------------------------------------
# Download helper
# --------------------------------------------------------------------------

def _download(url: str, dest_path: str, message: str, lo: float, hi: float) -> None:
    """Stream `url` to `dest_path`, mapping download progress into [lo, hi]."""
    stderr(f"[setup] downloading {url}")
    req = urllib.request.Request(
        url, headers={"User-Agent": "OpenPal-speech-plugin/0.1.0"}
    )
    with urllib.request.urlopen(req, timeout=120) as resp:  # nosec - pinned hosts
        try:
            total = int(resp.headers.get("Content-Length") or "0")
        except (TypeError, ValueError):
            total = 0
        read = 0
        chunk = 1024 * 256
        tmp = dest_path + ".part"
        try:
            with open(tmp, "wb") as out:
                while True:
                    buf = resp.read(chunk)
                    if not buf:
                        break
                    out.write(buf)
                    read += len(buf)
                    if total > 0:
                        frac = lo + (hi - lo) * (read / total)
                        progress(f"{message} {read // (1024 * 1024)} MB", frac)
            os.replace(tmp, dest_path)
        except BaseException:
            # Don't leave a half-written .part stub behind on error/interrupt.
            try:
                os.unlink(tmp)
            except OSError:
                pass
            raise
    stderr(f"[setup] downloaded {read} bytes to {dest_path}")


# --------------------------------------------------------------------------
# Steps
# --------------------------------------------------------------------------

def _install_piper(cfg: cfgmod.Config) -> None:
    """Download + extract the prebuilt Piper binary (skip if already present),
    then make sure its shared libraries are in place (macOS — see below)."""
    binary = cfg.piper_binary()
    # Re-extract unless the binary AND its espeak-ng phoneme data are both there,
    # so a partially-populated piper dir is repaired rather than trusted.
    if os.path.isfile(binary) and os.path.isdir(cfg.espeak_data_dir()):
        log("Piper voice engine already present; skipping download.")
    else:
        url = cfgmod.piper_url()
        is_zip = url.endswith(".zip")
        with tempfile.TemporaryDirectory() as td:
            archive = os.path.join(td, "piper-archive")
            progress("Downloading the voice engine…", 0.05)
            _download(url, archive, "Downloading the voice engine…", 0.05, 0.28)

            progress("Unpacking the voice engine…", 0.30)
            # Extract into the data dir; the archive's single top-level "piper/"
            # directory becomes <data>/piper/.
            if is_zip:
                with zipfile.ZipFile(archive) as zf:
                    _safe_extract_zip(zf, cfg.data_dir)
            else:
                with tarfile.open(archive, "r:gz") as tf:
                    _safe_extract_tar(tf, cfg.data_dir)

        if not os.path.isfile(binary):
            raise RuntimeError(
                f"Piper binary missing after extract (expected {binary})"
            )
        try:
            os.chmod(binary, 0o755)
        except OSError:
            pass

    # The macOS Piper asset omits the binary's shared libraries; supply them.
    # Runs even when the binary was already present, so existing (broken) installs
    # self-heal. No-op on other platforms and when the libs are already there.
    _install_piper_libs(cfg)
    progress("Voice engine ready.", 0.35)


# piper-phonemize stores the dylibs under "piper-phonemize/lib/"; we flatten that
# into <data>/piper/lib/.
_PHON_LIB_PREFIX = "piper-phonemize/lib/"


def _required_libs_present(libdir: str) -> bool:
    """True only when every dylib the binary loads is present (and, for the
    versioned-symlink chain, resolves — os.path.exists follows symlinks, so a
    dangling link counts as absent and triggers a re-extract)."""
    return all(
        os.path.exists(os.path.join(libdir, name))
        for name in cfgmod.REQUIRED_PIPER_LIBS
    )


def _install_piper_libs(cfg: cfgmod.Config) -> None:
    """macOS only: install the Piper shared libraries the official macOS Piper
    asset omits (libespeak-ng, libonnxruntime, libpiper_phonemize), taken from
    the matching piper-phonemize release. Idempotent; no-op off macOS."""
    if platform.system() != "Darwin":
        return
    libdir = cfg.piper_lib_dir
    # Skip only when ALL required libs are there — a single marker would let a
    # partial extract masquerade as complete and leave Piper permanently broken.
    if _required_libs_present(libdir):
        log("Piper libraries already present; skipping.")
        return

    os.makedirs(libdir, exist_ok=True)
    url = cfgmod.piper_phonemize_url()
    with tempfile.TemporaryDirectory() as td:
        archive = os.path.join(td, "phonemize-archive")
        progress("Finishing the voice engine…", 0.31)
        _download(url, archive, "Finishing the voice engine…", 0.31, 0.34)
        with tarfile.open(archive, "r:gz") as tf:
            _extract_phonemize_libs(tf, libdir)

    if not _required_libs_present(libdir):
        raise RuntimeError(
            f"Piper libraries incomplete after extract in {libdir} "
            f"(need {', '.join(cfgmod.REQUIRED_PIPER_LIBS)})"
        )


def _install_voice(cfg: cfgmod.Config) -> None:
    """Download the Piper voice model (.onnx + .onnx.json)."""
    onnx = cfg.voice_path()
    meta = onnx + ".json"
    os.makedirs(cfg.voices_dir, exist_ok=True)
    if os.path.isfile(onnx) and os.path.isfile(meta):
        log("Voice model already present; skipping.")
        progress("Voice ready.", 0.6)
        return
    onnx_url, json_url = cfgmod.voice_urls(cfg.voice)
    progress("Downloading the voice…", 0.36)
    _download(onnx_url, onnx, "Downloading the voice…", 0.36, 0.57)
    _download(json_url, meta, "Downloading the voice…", 0.57, 0.6)
    progress("Voice ready.", 0.6)


def _install_whisper(cfg: cfgmod.Config) -> None:
    """Pre-download the faster-whisper model so transcription works offline."""
    os.makedirs(cfg.whisper_dir, exist_ok=True)
    progress("Downloading speech recognition… (this can take a minute)", 0.62)
    # Instantiating the model downloads it into download_root (and caches it for
    # the service). We discard the instance.
    from faster_whisper import WhisperModel

    WhisperModel(
        cfg.whisper_model,
        device="cpu",
        compute_type="int8",
        download_root=cfg.whisper_dir,
    )
    progress("Speech recognition ready.", 0.95)


# --------------------------------------------------------------------------
# Safe archive extraction (defend against path traversal)
# --------------------------------------------------------------------------

def _link_target_escapes(member_name: str, linkname: str) -> bool:
    """True if a sym/hardlink's target is absolute or would resolve outside the
    extraction root (defends the pre-3.12 fallback path, which lacks the
    `data` filter's own link checks)."""
    if os.path.isabs(linkname):
        return True
    resolved = os.path.normpath(os.path.join(os.path.dirname(member_name), linkname))
    return resolved.startswith("..")


def _safe_extract_tar(tf: tarfile.TarFile, dest: str) -> None:
    members = []
    for m in tf.getmembers():
        norm = os.path.normpath(m.name)
        if norm.startswith("..") or os.path.isabs(norm):
            stderr(f"[setup] skipping suspicious member: {m.name}")
            continue
        if (m.issym() or m.islnk()) and _link_target_escapes(norm, m.linkname):
            stderr(f"[setup] skipping suspicious link: {m.name} -> {m.linkname}")
            continue
        members.append(m)
    try:
        tf.extractall(dest, members=members, filter="data")  # py3.12+
    except TypeError:
        tf.extractall(dest, members=members)  # nosec - names + links sanitized above


def _safe_extract_zip(zf: zipfile.ZipFile, dest: str) -> None:
    for name in zf.namelist():
        norm = os.path.normpath(name)
        if norm.startswith("..") or os.path.isabs(norm):
            stderr(f"[setup] skipping suspicious member: {name}")
            continue
        zf.extract(name, dest)


def _extract_phonemize_libs(tf: tarfile.TarFile, dest: str) -> None:
    """Extract just the top-level .dylib files (and their versioned-symlink
    chain, e.g. libespeak-ng.1.dylib -> …1.52.0.1.dylib) from a piper-phonemize
    tarball into `dest`, flattening the "piper-phonemize/lib/" prefix. Skips the
    archive's bulky extras we don't need (the onnxruntime .dSYM, pkgconfig)."""
    members = []
    for m in tf.getmembers():
        if not m.name.startswith(_PHON_LIB_PREFIX):
            continue
        rel = m.name[len(_PHON_LIB_PREFIX):]
        # Keep only the lib dir's own .dylib entries; a "/" means a nested dir
        # (the .dSYM bundle, pkgconfig) and anything else isn't a library.
        if not rel or "/" in rel or not rel.endswith(".dylib"):
            continue
        if os.path.isabs(rel) or os.path.normpath(rel).startswith(".."):
            stderr(f"[setup] skipping suspicious member: {m.name}")
            continue
        if (m.issym() or m.islnk()) and _link_target_escapes(rel, m.linkname):
            stderr(f"[setup] skipping suspicious link: {m.name} -> {m.linkname}")
            continue
        m.name = rel  # land directly in <dest>, not <dest>/piper-phonemize/lib
        members.append(m)
    try:
        tf.extractall(dest, members=members, filter="data")  # py3.12+
    except TypeError:
        tf.extractall(dest, members=members)  # nosec - names + links sanitized above


# --------------------------------------------------------------------------
# Orchestration
# --------------------------------------------------------------------------

def run_setup(data_dir: str) -> None:
    os.makedirs(data_dir, exist_ok=True)
    cfg = cfgmod.Config(data_dir=data_dir)
    progress("Preparing voice features…", 0.02)
    _install_piper(cfg)
    _install_voice(cfg)
    _install_whisper(cfg)
    progress("Voice features ready.", 1.0)
    _emit({"type": "done"})


def main(argv=None) -> int:
    if argv is None:
        argv = sys.argv[1:]
    parser = argparse.ArgumentParser(add_help=False)
    parser.add_argument("--data-dir", dest="data_dir", default="")
    args, _unknown = parser.parse_known_args(argv)

    data_dir = args.data_dir or os.environ.get("OPENPAL_PLUGIN_DATA", "")
    if not data_dir:
        _emit({"type": "error", "message": "Voice setup couldn't find its data directory."})
        return 0

    try:
        run_setup(data_dir)
    except Exception as exc:  # absolute catch-all -> one user-safe error event.
        traceback.print_exc()
        stderr(f"[setup] FAILED: {exc}")
        _emit({
            "type": "error",
            "message": (
                "Couldn't set up Voice. Check your internet connection and try "
                "enabling the add-on again."
            ),
        })
    return 0


if __name__ == "__main__":
    sys.exit(main())
