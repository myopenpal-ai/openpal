# Voice (speech) plugin

Adds two local, private speech features to OpenPal:

- **Read aloud (TTV)** — a 🔊 button on any message reads it out, using a local
  [Piper](https://github.com/rhasspy/piper) voice.
- **Voice typing (VTT)** — a 🎙 button in the composer dictates what you say into
  the message box, using [faster-whisper](https://github.com/SYSTRAN/faster-whisper).

Everything runs on your Mac. Once set up, no internet is needed.

## How it fits together

This is a standard OpenPal plugin (see `../PLUGINS.md`). It advertises the
`tts` and `stt` capabilities and runs one small loopback HTTP **service**
(`service.py`, stdlib `http.server`) that the Rust app calls. The wire contract
is in [`PROTOCOL.md`](./PROTOCOL.md).

Audio I/O lives in the Rust app (`crates/app/src/audio.rs`): the microphone is
captured with `cpal` and playback uses `rodio`, because the webview's locked CSP
can't reach the loopback service or open the mic. Captured audio is sent to
`/transcribe` as WAV; synthesized audio comes back from `/speak` as WAV and is
played natively.

## Setup downloads (first enable)

Into the plugin data dir (`<app-data>/plugin-data/speech/`):

1. The prebuilt **Piper** binary for your platform → `piper/`
2. One Piper **voice** (`en_US-amy-medium`, ~63 MB) → `voices/`
3. The **faster-whisper** model (`base`, ~145 MB) → `whisper/`

On macOS, step 1 also pulls Piper's shared libraries (`libespeak-ng`,
`libonnxruntime`, `libpiper_phonemize`) into `piper/lib/` — see the note below.

> Why a Piper *binary* and not `pip install piper-tts`? The `piper-phonemize`
> wheel has no macOS arm64 build, so the pip install fails on Apple Silicon.
> Shipping the official prebuilt binary (the same approach OpenPal uses for
> `llama-server`) avoids that and works across platforms.
>
> **macOS quirk:** the official macOS Piper asset ships the `piper` binary but
> *omits* its bundled `.dylib`s (only the onnxruntime `.dSYM` is included), and
> the binary loads them via `@rpath` with no `LC_RPATH` — so it aborts at launch
> with `dyld: Library not loaded: @rpath/libespeak-ng.1.dylib`. We supply the
> missing libraries from the matching [`piper-phonemize`](https://github.com/rhasspy/piper-phonemize)
> release into `piper/lib/`, and launch Piper with that directory on
> `DYLD_LIBRARY_PATH` (`openpal_speech/tts.py`). Linux/Windows archives bundle
> their libraries, so this only applies to macOS.
>
> **Upgrading an install set up before this fix:** the version bump re-runs
> setup (idempotent — it fetches just the missing libs), then **restart the app
> or toggle Voice off/on in Add-ons** so the service reloads with the change.

## Development

```bash
# from this directory, with the plugin venv active (or any venv with
# faster-whisper installed), and the data dir populated by a prior setup run:
export OPENPAL_PLUGIN_DATA=/path/to/plugin-data/speech
python -m openpal_speech speak "hello there" /tmp/out.wav
python -m openpal_speech transcribe /tmp/out.wav

# run the service by hand:
python service.py --port 8900 --data-dir "$OPENPAL_PLUGIN_DATA"

# tests (no models/network needed):
python -m pytest
```
