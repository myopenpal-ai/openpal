# OpenPal

**Your own AI. Private, free, and right here on your Mac.**

OpenPal is a macOS desktop app for total beginners: it looks at your Mac,
picks the AI model that fits it best, downloads it with one click, and gives
you a warm, simple chat. No account needed, no jargon — nothing leaves your
computer unless you turn on an optional cloud model.

Built with [Tauri 2](https://v2.tauri.app), Rust, React, and
[llama.cpp](https://github.com/ggml-org/llama.cpp).

## Philosophy

OpenPal is opinionated on purpose: the goal is getting non-technical people
to actually adopt local, private AI — not maximizing configurability. It
picks one model and uses it for everything, chat, the agent, web search,
even when a second, specialized model would be more efficient, because
juggling models is exactly the kind of decision a beginner shouldn't have to
make. Want more control? Settings → Advanced and the plugin system let you
go further without changing what everyone else sees by default. See
[ARCHITECTURE.md](ARCHITECTURE.md#design-philosophy) for the full rationale.

## How it works

1. **Scan** — on first launch OpenPal detects your chip, memory, and free disk
   space.
2. **Match** — a curated catalog of verified, open models (Llama, Qwen, Gemma,
   GPT-OSS) is ranked against your machine; the best fit is recommended in
   plain English.
3. **Download** — one click, resumable, with friendly progress.
4. **Chat** — a bundled `llama-server` (llama.cpp) runs the model locally with
   Metal acceleration on Apple Silicon; replies stream into a clean chat UI.

Want something beyond the curated picks? Settings → AI model lets you add any
GGUF from Hugging Face (paste a link or "owner/repo") or use a `.gguf` file
already on your Mac, in place.

## Plugins

OpenPal has a small plugin system (Plugins in the sidebar): enable an add-on and
it sets itself up; disable it and it stops. Plugins are folders with a
`plugin.json` manifest — built-in ones ship in [`plugins/`](plugins/), and you
can drop your own into the app's data folder. See
[`plugins/PLUGINS.md`](plugins/PLUGINS.md) for the contract.

**Web search** (the first plugin) lets your assistant search the web, read
pages, and cite its sources — the model decides when to look something up, just
like Claude or ChatGPT. It stays private: it runs its **own local
[SearXNG](https://docs.searxng.org/)** search engine, which it downloads and
starts for you automatically — **no Docker, no commands**. Enable it in Plugins
(or tap the globe in the chat box) and watch the one-time setup; after that the
agent loop runs locally against your own model. Details:
[`plugins/web-search/`](plugins/web-search/README.md).

## Project layout

The three concerns are strictly separated:

| Path          | Crate / package    | Role |
| ------------- | ------------------ | ---- |
| `crates/core` | `openpal-core`     | All logic, zero GUI: hardware detection, model catalog & selection, resumable downloads, llama-server lifecycle + streaming chat. |
| `crates/app`  | `openpal-app`      | The Tauri GUI shell: thin commands over core, window config, capabilities. |
| `ui/`         | `openpal-ui`       | React + TypeScript frontend (Vite). Talks to Rust only through `ui/src/lib/api.ts`. |
| `bundler/`    | `openpal-bundler`  | Bundling & deployment module: fetches/stages the llama.cpp engine, generates icons, builds, signs, and notarizes. Never imported by the app. |

See [ARCHITECTURE.md](ARCHITECTURE.md) for the full picture.

## Development

Prerequisites: Rust (rustup), Node 18+, Xcode command line tools.

```bash
git clone https://github.com/myopenpal-ai/openpal.git && cd openpal
npm run setup                                  # root + ui npm install
cargo run -p openpal-bundler -- fetch-engine   # stage llama-server (one time)
npm run dev                                    # tauri dev
npm run dev:onboarding                         # same, on a throwaway first-run profile
```

`dev:onboarding` shows the first-run onboarding on every launch without
touching your real data: it runs `npm run dev` in portable mode against
`target/debug/data`, emptied each time (downloaded models are kept; pass
`-- --wipe-models` for a true first run). Portable mode has no recovery-code
screen and no Plugins tab, and Settings → Factory reset must not be used in
that session: it also deletes the OS-keychain key an installed OpenPal
shares. It refuses to start while another dev server holds :5173.

### Trying a different llama.cpp build

OpenPal ships with a pinned llama.cpp engine (`crates/app/resources/llama/`).
To test a newer build — e.g. one that adds a model architecture the current
release doesn't know yet, like Kimi K2 GGUF support — stage it somewhere else
and point the dev app at it, without touching the shipped default:

```bash
# Stage build b11454 into a git-ignored throwaway dir (default stays untouched)
cargo run -p openpal-bundler -- fetch-engine --tag b11454 --force --dest "$PWD/target/k2-engine"

# Run the app against it (use the absolute path: tauri dev runs from crates/app/)
OPENPAL_ENGINE_DIR="$PWD/target/k2-engine" npm run dev
```

`OPENPAL_ENGINE_DIR` is a dev/test override: when set, that engine wins over
both the bundled build and any in-app downloaded update, and the app logs
which binary it picked. Unset it (or run plain `npm run dev`) to go back to
the normal engine.

Check your environment anytime:

```bash
cargo run -p openpal-bundler -- doctor
```

### Tests

```bash
cargo test --workspace                  # Rust unit tests
cargo test -p openpal-core -- --ignored # real-engine e2e (downloads a ~140 MB
                                        # test model, runs real inference)
cd ui && npm test                       # frontend unit/component tests (Vitest)
cd ui && npm run test:e2e               # frontend mocked-IPC e2e (Playwright)
cd plugins/web-search && python -m pytest
cd plugins/speech && python -m pytest
```

CI runs the Rust/frontend suites (lint, unit, and the Playwright e2e) on
every pull request; the real-engine e2e and the Python plugin suites are
path-filtered to the code they actually exercise. See
[docs/TESTING.md](docs/TESTING.md) for the full strategy, CI wiring, and the
prioritized backlog of remaining test gaps.

## Building & shipping

Everything ships through the bundler module:

```bash
cargo run -p openpal-bundler -- icon       # regenerate the icon set
cargo run -p openpal-bundler -- build      # .app + .dmg via tauri build
cargo run -p openpal-bundler -- release    # fetch + icon + build + sign/notarize
```

Signing and notarization activate when credentials are present:

```bash
export SIGNING_IDENTITY="Developer ID Application: You (TEAMID)"
export APPLE_ID="you@example.com" APPLE_PASSWORD="app-specific" APPLE_TEAM_ID="TEAMID"
cargo run -p openpal-bundler -- release
```

Unsigned builds work fine locally (Gatekeeper will warn on other Macs).

## Platform support

| Platform | Status |
| --- | --- |
| macOS, Apple Silicon | Tested — the supported platform today. |
| macOS, Intel | Builds in CI (portable build); untested. |
| Windows, Linux (x64, arm64) | Portable builds are produced by CI ([`docs/design/PORTABLE.md`](docs/design/PORTABLE.md)) but haven't been tested on real machines yet — expect rough edges. |
| Android | Early scaffolding only, not usable yet ([`docs/design/MOBILE.md`](docs/design/MOBILE.md)). |

The core/GUI/bundler separation exists so other platforms land by adding
bundler targets and engine assets — the app logic is already portable. If
you run OpenPal on Windows or Linux, bug reports are very welcome.

Unfinished plugins (web search, voice, writing tools, …) only appear once
**Settings → Advanced → Experimental features** is on. Agent mode,
multi-device sync and remote access are separate opt-in switches in Settings,
all off by default.

## Contributing

Contributions are welcome — start with [CONTRIBUTING.md](CONTRIBUTING.md),
and read the [design philosophy](ARCHITECTURE.md#design-philosophy) before
proposing a feature. Please report security issues privately, as described in
[SECURITY.md](SECURITY.md).

## License

MIT. Models keep their own licenses (shown in the app's model library).
