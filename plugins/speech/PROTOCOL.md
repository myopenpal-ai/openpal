# OpenPal Voice service — wire protocol

The Voice plugin advertises two **service capabilities** — `tts` (text-to-voice)
and `stt` (voice-to-text). Unlike a `chat` plugin (which the framework drives as
a per-turn subprocess), Voice is consumed by the app *directly over its managed
loopback HTTP service*. See `plugins/PLUGINS.md` §7.

The framework starts the service as:

    <venv>/bin/python service.py --port <port> --data-dir <data>

and polls `GET /healthz` until it answers (PLUGINS.md §4). The Rust app then
finds the enabled, set-up plugin advertising `tts` / `stt`
(`PluginManager::active_service_plugin`), gets the running port
(`ensure_service`), and calls the endpoints below. The webview never talks to
this service — audio capture/playback happen in Rust (the CSP forbids the
webview reaching loopback HTTP), so audio crosses the IPC boundary as bytes.

## Endpoints

### `GET /healthz`
→ `200 {"status":"ok"}`. Answers as soon as the socket is bound; the STT/TTS
models load lazily on first use, so health does not wait on them.

### `POST /transcribe` — speech-to-text (VTT)
- **Request body:** raw **WAV** bytes (`Content-Type: audio/wav`). Any sample
  rate, mono or stereo — faster-whisper resamples to 16 kHz internally, so the
  app sends the microphone's native capture format unchanged.
- **Response:** `200 {"text": "transcribed words"}` (may be `""` for silence).

### `POST /speak` — text-to-speech (TTV)
- **Request body:** JSON `{"text": "...", "voice": "<optional voice id>"}`.
  Optional Piper synthesis controls may also be sent — any combination of
  `length_scale` (phoneme length; >1 slower, <1 faster), `noise_scale` (audio
  variability), `noise_w` (cadence variability), and `sentence_silence`
  (seconds of pause between sentences). Each is clamped to a safe range; an
  omitted control keeps Piper's built-in default. The Rust app derives these
  from the user's read-aloud Settings (speed → `length_scale`, expressiveness →
  the two noise controls, pause → `sentence_silence`).
- **Response:** `200` with `Content-Type: audio/wav` — a complete WAV the app
  plays via rodio. Empty text yields a short silent WAV.

### `GET /voices` — list selectable voices
- **Response:** `200 {"voices": [ {"id","name","language","gender","quality","sizeMb","previewUrl","installed"}, … ]}`.
  The list is a curated catalog (`openpal_speech.voices.CATALOG`) plus any voice
  already on disk that isn't in it. `installed` is true when both the `.onnx`
  model and its `.onnx.json` are present. `previewUrl` is a short MP3 sample the
  app plays to preview the voice (empty when the id doesn't parse). Keys are
  camelCase because the payload is proxied verbatim through Rust to the webview.

### `POST /voices/download` — install a voice
- **Request body:** JSON `{"voice": "<catalog voice id>"}`. Downloads the voice
  model into `<data>/voices/` (a no-op if already installed). Only catalog ids
  are accepted — the id is interpolated into a download URL and a file path.
- **Response:** `200` with the refreshed `{"voices": […]}` list (same shape as
  `GET /voices`). A bad/unknown id or a failed download returns an error (below).

Voice selection itself is the app's concern: the user's chosen voice id lives in
Settings and is sent to `POST /speak` as `voice`; this plugin only lists and
installs the models.

## Errors
Any failure returns a non-2xx with `{"error": "...message..."}`. The Rust app
logs the detail and shows the user a safe sentence (e.g. *"Couldn't turn your
speech into text. Please try again."*). The service never crashes on a bad
request — it returns the error and keeps serving.

## What runs underneath
- **STT:** `faster-whisper` (CTranslate2), model `base` by default, int8 on CPU.
  Model cached in `<data>/whisper/`.
- **TTS:** the prebuilt **Piper** binary (`<data>/piper/piper`), voice
  `en_US-amy-medium` (`<data>/voices/`). Piper reads the utterance on stdin and
  writes a WAV; we return its bytes.

Both the model size (`OPENPAL_WHISPER_MODEL`) and voice (`OPENPAL_SPEECH_VOICE`)
can be overridden by environment variable for development.
