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
- Streaming replies no longer freeze after the first words and dump the rest at once — per-token updates re-render only the live message's markdown, so reply growth stays smooth even in long conversations.
- In-app update install on the Full (installed-app) SKU no longer fails with a 404: the bundler's manifest --full-dir now archives/signs/uploads the Full SKU's updater artifacts and writes their real download URLs into latest.json, replacing the hand-edited platforms block that used to drift from the release's assets.
- Surface the failing feed endpoint in update logs and document the engine-update feed bootstrap
- Automate the engine-update feed: a nightly `engine-update.yml` publishes the rolling `engine-latest` channel (one sequential fetch-and-merge loop, feed created on first run), and a `feed-health.yml` alarm fails loudly if either configured update feed stops resolving — the update gate fails open, so a vanished feed is otherwise invisible to every user and dev.
- Script the two remaining hand-steps of the update feeds as a dispatch-only `publish-update-feeds` workflow: promote a chosen stable `v*` draft into the live app feed (with feed verification and pre-release/draft guards), and/or refresh the `engine-latest` engine feed on demand (with optional target/tag inputs) — still only ever triggered by a human.
- Writing Tools' editing surface is reworked: Correct marks each error in place on one editable surface and writes a suggestion in when it is activated (no tracked-changes pane, no second original box), corrections separated only by whitespace are one suggestion, and the result actions — Keep all, Discard all, Copy, Download, Clear — are named icons. The standalone Improve screen now streams its rewrite and offers Natural, Professional, Direct or a Custom tone whose instructions are remembered across restarts; the composer's inline Improve keeps its one-shot, single-tone behaviour.

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

