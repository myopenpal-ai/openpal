#!/usr/bin/env python3
"""First-run setup for the OpenPal web-search plugin (PLUGINS.md §3).

The framework runs this INSIDE the plugin venv:

    <venv>/bin/python setup_plugin.py --data-dir <data>

(cwd = plugin dir, env OPENPAL_PLUGIN_DATA=<data>, PYTHONUNBUFFERED=1).

What it does, all into the plugin's *data dir* (the plugin dir itself is
read-only):

  1. Download a PINNED SearXNG source tarball from GitHub.
  2. Extract it to <data>/searxng-src/ (top-level dir stripped).
  3. pip install SearXNG's own requirements.txt into the current venv.
  4. Generate <data>/settings.yml (json format enabled, loopback-friendly).

Progress is reported as newline-delimited JSON on **stdout** (one object per
line, flushed):

    {"type":"progress","message":"…","fraction":0.4}
    {"type":"log","message":"…"}
    {"type":"done"}                       # terminal, success
    {"type":"error","message":"…"}        # terminal, user-safe failure

**stderr** is a free-form log (the framework tees it to setup.log).

The script is idempotent / resumable: re-running with an already-extracted,
already-installed, already-configured data dir is fast and safe. It catches all
exceptions, emits exactly one terminal event, and exits 0 so the framework reads
the event instead of a broken pipe.
"""

from __future__ import annotations

import argparse
import json
import os
import secrets
import shutil
import subprocess
import sys
import tarfile
import tempfile
import traceback
import urllib.request

# --------------------------------------------------------------------------
# Pinned SearXNG source.
#
# SearXNG no longer publishes git tags or GitHub Releases — it ships a rolling
# release off `master`. So we pin an immutable COMMIT SHA (reproducible and
# tamper-evident) rather than a moving tag. To bump: pick a newer commit from
# https://github.com/searxng/searxng/commits/master , update both constants,
# and re-verify the tarball downloads + `searx.webapp` imports.
#
# Pinned: searxng/searxng @ cf1410af8d794793c8af02b97d3e1cf3bc7a7dda
#         (master HEAD on 2026-06-15; verified the codeload tarball downloads
#          and contains searx/webapp.py, requirements.txt, searx/settings.yml).
# --------------------------------------------------------------------------
SEARXNG_REF = "cf1410af8d794793c8af02b97d3e1cf3bc7a7dda"
SEARXNG_TARBALL_URL = (
    f"https://github.com/searxng/searxng/archive/{SEARXNG_REF}.tar.gz"
)

# Marker file that records the ref we successfully extracted, so re-runs that
# already have the right source can skip the (slow) download + extract.
_EXTRACT_MARKER = ".openpal_searxng_ref"
# Marker that records we already pip-installed SearXNG's deps for this ref.
_DEPS_MARKER = ".openpal_searxng_deps_ok"


def _emit(obj: dict) -> None:
    """Write one NDJSON event to stdout and flush."""
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
# Steps
# --------------------------------------------------------------------------

def _download_tarball(dest_path: str) -> None:
    """Stream the pinned tarball to dest_path, emitting download progress."""
    stderr(f"[setup] downloading {SEARXNG_TARBALL_URL}")
    req = urllib.request.Request(
        SEARXNG_TARBALL_URL,
        headers={"User-Agent": "OpenPal-web-search-plugin/0.1.0"},
    )
    # 0.05..0.45 of the overall bar is "download".
    with urllib.request.urlopen(req, timeout=120) as resp:  # nosec - pinned host
        total = 0
        try:
            total = int(resp.headers.get("Content-Length") or "0")
        except (TypeError, ValueError):
            total = 0
        read = 0
        chunk = 1024 * 256
        with open(dest_path, "wb") as out:
            while True:
                buf = resp.read(chunk)
                if not buf:
                    break
                out.write(buf)
                read += len(buf)
                if total > 0:
                    frac = 0.05 + 0.40 * (read / total)
                    progress(
                        f"Downloading SearXNG… {read // (1024*1024)} MB",
                        frac,
                    )
    stderr(f"[setup] downloaded {read} bytes to {dest_path}")


def _extract_strip_top(tar_path: str, dest_dir: str) -> None:
    """Extract the tarball into dest_dir, stripping the single top-level dir."""
    stderr(f"[setup] extracting {tar_path} -> {dest_dir}")
    # Fresh extraction: remove any partial previous attempt.
    if os.path.isdir(dest_dir):
        shutil.rmtree(dest_dir, ignore_errors=True)
    os.makedirs(dest_dir, exist_ok=True)

    with tarfile.open(tar_path, "r:gz") as tf:
        members = tf.getmembers()
        # Determine the common top-level prefix (e.g. "searxng-<sha>/").
        top = None
        for m in members:
            parts = m.name.split("/", 1)
            if parts[0]:
                top = parts[0]
                break
        if not top:
            raise RuntimeError("tarball had no top-level directory")
        prefix = top + "/"

        safe_members = []
        for m in members:
            if not m.name.startswith(prefix):
                continue
            rel = m.name[len(prefix):]
            if not rel:
                continue
            # Defend against path traversal in the archive.
            norm = os.path.normpath(rel)
            if norm.startswith("..") or os.path.isabs(norm):
                stderr(f"[setup] skipping suspicious member: {m.name}")
                continue
            m.name = rel
            safe_members.append(m)

        # Python 3.12+ supports the 'data' extraction filter; fall back if not.
        try:
            tf.extractall(dest_dir, members=safe_members, filter="data")  # type: ignore[arg-type]
        except TypeError:
            tf.extractall(dest_dir, members=safe_members)  # nosec - sanitized above
    stderr(f"[setup] extracted {len(safe_members)} members")


def _pip_install_searxng_reqs(searxng_src: str) -> None:
    """Install SearXNG's own Python deps into the current venv."""
    reqs = os.path.join(searxng_src, "requirements.txt")
    if not os.path.isfile(reqs):
        raise RuntimeError("SearXNG requirements.txt missing after extract")
    cmd = [sys.executable, "-m", "pip", "install", "-r", reqs]
    stderr(f"[setup] {' '.join(cmd)}")
    # Stream pip output to our stderr (-> setup.log) so failures are diagnosable.
    proc = subprocess.run(
        cmd,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )
    if proc.stdout:
        stderr(proc.stdout)
    if proc.returncode != 0:
        raise RuntimeError(
            f"pip install of SearXNG dependencies failed (exit {proc.returncode})"
        )


_SETTINGS_TEMPLATE = """\
# Auto-generated by the OpenPal web-search plugin setup. Safe to delete; it will
# be regenerated on the next setup run. Bind address / port are passed at runtime
# by service.py directly to the server, so they are intentionally omitted here.
#
# use_default_settings: true => start from SearXNG's built-in defaults and only
# override the handful of keys OpenPal needs. Resilient across SearXNG versions.

use_default_settings: true

general:
  instance_name: "OpenPal Search"
  enable_metrics: false

server:
  # Loopback-only single-user instance.
  secret_key: "{secret_key}"
  # The bot/rate limiter needs valkey/redis; we omit it. Safe because the
  # instance is bound to 127.0.0.1 and used only by the local OpenPal app.
  limiter: false
  # Not a public instance: disables hardening that would reject the programmatic
  # JSON requests the agent makes.
  public_instance: false
  # Don't proxy result images (saves CPU/bandwidth; agent doesn't need it).
  image_proxy: false

search:
  # CRITICAL: the agent calls /search?...&format=json. "json" MUST be present
  # here or those requests return HTTP 403.
  formats:
    - html
    - json
  safe_search: 0
  language: "en"
  # No autocomplete backend (avoids extra outbound calls; agent doesn't use it).
  autocomplete: ""

ui:
  default_locale: "en"

# A sane general-purpose engine set. Everything not listed keeps its built-in
# default (use_default_settings: true), so we only flip the few that matter.
engines:
  - name: duckduckgo
    disabled: false
  - name: bing
    disabled: false
  - name: brave
    disabled: false
  - name: wikipedia
    disabled: false
  - name: startpage
    disabled: false
"""


def _write_settings(settings_path: str) -> None:
    """Generate settings.yml with a fresh secret key (idempotent)."""
    if os.path.isfile(settings_path):
        # Keep the existing key (and any user tweaks) on re-run.
        stderr(f"[setup] settings.yml already present at {settings_path}; keeping it")
        return
    secret = secrets.token_hex(32)
    tmp = settings_path + ".tmp"
    with open(tmp, "w", encoding="utf-8") as fh:
        fh.write(_SETTINGS_TEMPLATE.format(secret_key=secret))
    os.replace(tmp, settings_path)
    stderr(f"[setup] wrote {settings_path}")


# --------------------------------------------------------------------------
# Orchestration
# --------------------------------------------------------------------------

def run_setup(data_dir: str) -> None:
    os.makedirs(data_dir, exist_ok=True)
    searxng_src = os.path.join(data_dir, "searxng-src")
    settings_path = os.path.join(data_dir, "settings.yml")
    extract_marker = os.path.join(searxng_src, _EXTRACT_MARKER)
    deps_marker = os.path.join(searxng_src, _DEPS_MARKER)

    progress("Preparing web search…", 0.02)

    # --- 1 & 2: download + extract (skip if already at the pinned ref) -----
    already_extracted = (
        os.path.isfile(extract_marker)
        and _read_text(extract_marker).strip() == SEARXNG_REF
        and os.path.isfile(os.path.join(searxng_src, "searx", "webapp.py"))
    )
    if already_extracted:
        log("SearXNG source already present; skipping download.")
        progress("SearXNG source ready.", 0.45)
    else:
        progress("Downloading SearXNG…", 0.05)
        with tempfile.TemporaryDirectory() as td:
            tar_path = os.path.join(td, "searxng.tar.gz")
            _download_tarball(tar_path)
            progress("Extracting SearXNG…", 0.46)
            _extract_strip_top(tar_path, searxng_src)
        _write_text(extract_marker, SEARXNG_REF + "\n")
        progress("SearXNG source ready.", 0.50)

    # --- 3: install SearXNG's own deps (skip if already done for this ref) -
    if (
        os.path.isfile(deps_marker)
        and _read_text(deps_marker).strip() == SEARXNG_REF
    ):
        log("SearXNG dependencies already installed; skipping pip.")
        progress("Dependencies ready.", 0.9)
    else:
        progress("Installing SearXNG dependencies… (this can take a minute)", 0.55)
        _pip_install_searxng_reqs(searxng_src)
        _write_text(deps_marker, SEARXNG_REF + "\n")
        progress("Dependencies installed.", 0.9)

    # --- 4: settings.yml --------------------------------------------------
    progress("Writing configuration…", 0.95)
    _write_settings(settings_path)

    progress("Web search ready.", 1.0)
    _emit({"type": "done"})


def _read_text(path: str) -> str:
    try:
        with open(path, "r", encoding="utf-8") as fh:
            return fh.read()
    except OSError:
        return ""


def _write_text(path: str, text: str) -> None:
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf-8") as fh:
        fh.write(text)


def main(argv=None) -> int:
    if argv is None:
        argv = sys.argv[1:]
    parser = argparse.ArgumentParser(add_help=False)
    parser.add_argument("--data-dir", dest="data_dir", default="")
    args, _unknown = parser.parse_known_args(argv)

    data_dir = args.data_dir or os.environ.get("OPENPAL_PLUGIN_DATA", "")
    if not data_dir:
        _emit({
            "type": "error",
            "message": "Web search setup could not find its data directory.",
        })
        return 0

    try:
        run_setup(data_dir)
    except Exception as exc:  # absolute catch-all -> one user-safe error event.
        traceback.print_exc()
        stderr(f"[setup] FAILED: {exc}")
        _emit({
            "type": "error",
            "message": (
                "Couldn't set up web search. Check your internet connection and "
                "try turning web search on again."
            ),
        })
    return 0


if __name__ == "__main__":
    sys.exit(main())
