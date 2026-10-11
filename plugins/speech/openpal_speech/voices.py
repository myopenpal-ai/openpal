"""The catalog of selectable Piper voices, plus install/download helpers.

First-run setup installs one default voice (`config.DEFAULT_VOICE`). Users can
add more from the curated catalog below; each lands as `<id>.onnx` (+
`.onnx.json`) under `cfg.voices_dir`, exactly where `setup_plugin` puts the
default, so the existing `tts.synthesize` path picks them up unchanged.

Downloads are restricted to catalog ids on purpose: a voice id is interpolated
into a Hugging Face URL (`config.voice_urls`) and a local filesystem path
(`config.voice_path`), so accepting an arbitrary id would be an injection /
path-traversal risk. The ids below are static literals, which makes that set
trustworthy.
"""

from __future__ import annotations

import os
import sys
import urllib.request

from .config import voice_sample_url, voice_urls

# Curated, single-speaker Piper voices from rhasspy/piper-voices (the same repo
# and pinned revision `config.voice_urls` resolves against). Sizes are
# approximate — Piper models run ~25 MB (low), ~63 MB (medium), ~113 MB (high).
CATALOG: list[dict] = [
    # English (US)
    {"id": "en_US-amy-medium", "name": "Amy", "language": "English (US)", "gender": "female", "quality": "medium", "size_mb": 63},
    {"id": "en_US-joe-medium", "name": "Joe", "language": "English (US)", "gender": "male", "quality": "medium", "size_mb": 63},
    {"id": "en_US-kristin-medium", "name": "Kristin", "language": "English (US)", "gender": "female", "quality": "medium", "size_mb": 63},
    {"id": "en_US-lessac-medium", "name": "Lessac", "language": "English (US)", "gender": "female", "quality": "medium", "size_mb": 63},
    {"id": "en_US-hfc_male-medium", "name": "HFC Male", "language": "English (US)", "gender": "male", "quality": "medium", "size_mb": 63},
    {"id": "en_US-ryan-high", "name": "Ryan", "language": "English (US)", "gender": "male", "quality": "high", "size_mb": 113},
    # English (UK)
    {"id": "en_GB-alba-medium", "name": "Alba", "language": "English (UK)", "gender": "female", "quality": "medium", "size_mb": 63},
    {"id": "en_GB-alan-medium", "name": "Alan", "language": "English (UK)", "gender": "male", "quality": "medium", "size_mb": 63},
    {"id": "en_GB-northern_english_male-medium", "name": "Northern English Male", "language": "English (UK)", "gender": "male", "quality": "medium", "size_mb": 63},
    # Spanish
    {"id": "es_ES-davefx-medium", "name": "DaveFX", "language": "Spanish", "gender": "male", "quality": "medium", "size_mb": 63},
    # French
    {"id": "fr_FR-siwis-medium", "name": "Siwis", "language": "French", "gender": "female", "quality": "medium", "size_mb": 63},
    {"id": "fr_FR-tom-medium", "name": "Tom", "language": "French", "gender": "male", "quality": "medium", "size_mb": 63},
    # German
    {"id": "de_DE-thorsten-medium", "name": "Thorsten", "language": "German", "gender": "male", "quality": "medium", "size_mb": 63},
    {"id": "de_DE-kerstin-low", "name": "Kerstin", "language": "German", "gender": "female", "quality": "low", "size_mb": 25},
    # Italian
    {"id": "it_IT-paola-medium", "name": "Paola", "language": "Italian", "gender": "female", "quality": "medium", "size_mb": 63},
]

_CATALOG_IDS = {e["id"] for e in CATALOG}


def _log(msg: str) -> None:
    print(f"[voices] {msg}", file=sys.stderr, flush=True)


def installed_ids(cfg) -> set[str]:
    """Voice ids fully present in the voices dir — both the `.onnx` model and
    its `.onnx.json` metadata (a half-downloaded voice doesn't count)."""
    try:
        names = set(os.listdir(cfg.voices_dir))
    except OSError:
        return set()
    return {
        n[: -len(".onnx")]
        for n in names
        if n.endswith(".onnx") and (n + ".json") in names
    }


def list_voices(cfg) -> list[dict]:
    """The catalog, each entry tagged `installed`, in `sizeMb` (camelCase, since
    the value travels through Rust to the webview). Any installed voice that
    isn't in the catalog (e.g. one dropped in by hand) is appended too, so the
    UI never hides a voice the user actually has."""
    inst = installed_ids(cfg)

    def shape(e: dict, installed: bool) -> dict:
        try:
            preview = voice_sample_url(e["id"])
        except ValueError:
            preview = ""  # a hand-installed id that doesn't parse — no preview
        return {
            "id": e["id"],
            "name": e["name"],
            "language": e["language"],
            "gender": e["gender"],
            "quality": e["quality"],
            "sizeMb": e["size_mb"],
            "previewUrl": preview,
            "installed": installed,
        }

    out = [shape(e, e["id"] in inst) for e in CATALOG]
    for vid in sorted(inst - _CATALOG_IDS):
        out.append(
            shape(
                {
                    "id": vid,
                    "name": vid,
                    "language": "Other",
                    "gender": "",
                    "quality": "",
                    "size_mb": 0,
                },
                True,
            )
        )
    return out


def _download(url: str, dest: str) -> None:
    """Stream `url` to `dest` atomically: write a `.part` then rename, and never
    leave a half-written stub behind on error."""
    req = urllib.request.Request(
        url, headers={"User-Agent": "OpenPal-speech-plugin/0.1.0"}
    )
    tmp = dest + ".part"
    with urllib.request.urlopen(req, timeout=120) as resp:  # nosec - pinned host
        try:
            with open(tmp, "wb") as out:
                while True:
                    buf = resp.read(1024 * 256)
                    if not buf:
                        break
                    out.write(buf)
            os.replace(tmp, dest)
        except BaseException:
            try:
                os.unlink(tmp)
            except OSError:
                pass
            raise


def download_voice(cfg, voice_id: str) -> None:
    """Download a catalog voice (`.onnx` + `.onnx.json`) into the voices dir.
    A no-op if it's already installed. Rejects ids outside the catalog."""
    if voice_id not in _CATALOG_IDS:
        raise ValueError(f"unknown voice {voice_id!r}")
    onnx = cfg.voice_path(voice_id)
    meta = onnx + ".json"
    os.makedirs(cfg.voices_dir, exist_ok=True)
    if os.path.isfile(onnx) and os.path.isfile(meta):
        _log(f"{voice_id} already installed")
        return
    onnx_url, json_url = voice_urls(voice_id)
    _log(f"downloading {voice_id}")
    # Fetch the small metadata first and the model last. tts.synthesize gates
    # only on the `.onnx`, so making it the last file to land means a concurrent
    # /speak (the service is threaded) never sees a model without its metadata.
    _download(json_url, meta)
    _download(onnx_url, onnx)
    _log(f"installed {voice_id}")
