# Changelog

All notable changes to OpenPal are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html)
exactly as [docs/RELEASING.md](docs/RELEASING.md) defines it: `fix` → patch,
`feat` → minor, a breaking change → major.

## [Unreleased]



- Mandatory password encryption: Standard Notes-style per-conversation keys sealed under a password-derived root, a mandatory first-run password on every onboarding path (portable included), and an id-preserving, in-place migration for existing installs.
- Open the app at launch from the OS keychain without a password: on installed desktop builds a password-protected install unlocks straight in (the OS keychain stands in for the typed password), portable builds still prompt, and a keychain failure always falls back to the password gate.
- Block the app and engine on unsafe published versions: a manifest-declared minimum supported version and blocked-version list turn the voluntary update dialog into a blocking, non-dismissable gate until a safe version is installed; an unsafe active engine build blocks chat and model usage until the engine is updated.

## [0.2.0] - 2026-11-16

### Added
- First release of OpenPal: scan your Mac, get a model recommendation, download, and chat with a fully local, private LLM in minutes (runs on-device with Metal acceleration).
- 19-model curated catalog with plain-language fit badges, quantization picker, speed vs. intelligence slider, resumable downloads, run-from-disk, and custom models (Hugging Face or a local .gguf).
- Agent mode: sandboxed shell, code execution, and file access with per-call consent and danger flags.
- Web search: per-conversation toggle, cite-on with sources, powered by a local SearXNG (no Docker).
- Writing tools: Translate, Correct, and Improve your message on the already-loaded model.
- Voice: replies read aloud (Piper TTS) and dictation (faster-whisper), all on-device.
- Projects, pinned & favorite conversations, bulk actions, full-text search, and reusable /skills.
- End-to-end-encrypted multi-device sync through a blind relay, plus remote access (QR pairing, TLS, wake-on-LAN) to share your model over LAN/VPN.
- Security: SQLCipher encryption at rest, OS-keychain key, app-lock PIN, parental controls, prompt-injection detection, safe link and image handling.
- In-app updates for OpenPal and the AI engine, encrypted automatic backups, export/import, and an "erase all data" reset.
- English and French with first-run language detection, and WCAG 2.2 AA-audited accessibility.

