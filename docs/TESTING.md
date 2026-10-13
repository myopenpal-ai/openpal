# Testing strategy

How OpenPal is tested, top to bottom, and how CI enforces it. The goal is the
standard SaaS-industry shape — a test pyramid (lots of cheap unit tests, fewer
integration tests, a thin e2e layer) enforced by CI gates on every pull
request — adapted to what this repo actually is: a Tauri desktop app with a
Rust core, a Python-sidecar plugin system, and no server to deploy.

## Layout at a glance

| Layer | Where | Run with |
|---|---|---|
| Rust unit tests | `crates/core/src/*.rs`, `crates/app/src/*.rs`, `bundler/src/*.rs` (inline `#[cfg(test)]`) | `cargo test -p <crate>` |
| Rust real-engine e2e | `crates/core/tests/{engine,writing_tools,prompt_enhancer}_e2e.rs` (`#[ignore]`) | `cargo test -p openpal-core -- --ignored` |
| Frontend unit/component tests | `ui/src/**/*.test.ts(x)` (Vitest) | `npm test` (in `ui/`) |
| Frontend mocked-IPC e2e | `ui/tests/e2e/**` (Playwright) | `npm run test:e2e` (in `ui/`) |
| Python plugin tests | `plugins/web-search/tests/`, `plugins/speech/tests/` (pytest) | `python -m pytest` (in each plugin dir) |
| Native e2e (real built app) | not in this repo yet — see [Native e2e](#native-e2e-real-built-app) | — |

## CI workflows

| Workflow | Triggers | Blocks merge? | What it runs |
|---|---|---|---|
| `.github/workflows/ci.yml` | every PR, push to `main` | **yes** (required) | `cargo fmt --check`, `cargo clippy --workspace -- -D warnings`, `cargo test` per crate, `ui` typecheck + `vite build`, Vitest, Playwright mocked-IPC e2e |
| `.github/workflows/plugin-tests.yml` | PR/push touching `plugins/web-search/**` or `plugins/speech/**` | yes, scoped | pytest for whichever plugin(s) changed |
| `.github/workflows/engine-e2e.yml` | PR/push touching `crates/core/src/engine/**`, `download.rs`, `sse.rs`, `writing_tools.rs`, `prompt_enhancer.rs`, or any file under `crates/core/tests/` | yes, scoped | the real llama-server engine tests (stages the engine via the bundler, downloads the ~140MB SmolLM2 test model, runs a real chat round-trip for plain chat, Writing Tools Translate/Correct, and the Message improver) |
| `.github/workflows/portable.yml` (pre-existing, unchanged) | manual dispatch, `v*` tags | no (release pipeline, not a test gate) | cross-platform Portable builds + signed draft release |

Path-filtered workflows (`plugin-tests.yml`, `engine-e2e.yml`) simply don't
run at all on a PR that doesn't touch their paths — GitHub treats a required
check from a workflow that wasn't triggered as satisfied, so they're safe to
mark as required status checks without blocking unrelated PRs.

### Why these boundaries

- **One `ci.yml`, not one-workflow-per-language**: Rust and frontend checks
  are the ones every contributor's PR should pay for, every time — they're
  fast (a few minutes total) and catch the most common regressions
  (compile errors, lint drift, broken unit tests, broken IPC contracts).
- **Plugin tests are path-filtered**: the Python plugins are a separate
  concern from the Rust/TS app (different language, different install step,
  different people likely touching them) and their own suites don't exercise
  anything Rust/TS-side — no reason to pay their `pip install` cost on every
  unrelated PR.
- **The real engine e2e is path-filtered *and* heavier**: it needs network
  (fetches a real llama.cpp binary + a real ~140MB model from Hugging Face)
  and runs real inference, so it's the slowest, least hermetic check in the
  repo. Scoping it to the two files whose behavior it actually exercises
  (`engine/**`, `download.rs`, `sse.rs`, `writing_tools.rs`, `prompt_enhancer.rs`) keeps it from taxing every PR while still
  giving fast, targeted signal on the highest-risk Rust module.

## Rust

### Unit tests

`crates/core` already had strong inline coverage (182 tests across 22
modules) before this pass. `crates/app` had thin coverage (pure functions in
`commands.rs`/`logging.rs` only). `bundler/` had effectively none (3 tests in
one function) despite being release-critical — it's now at 27, covering the
pure/deterministic logic in every file (path joiners, `sha256_hex`,
`artifact_names`, `check_license`, `copy_plugin_dir`, `first_line`,
`resolve_identity`, the icon SDF math).

**Workspace-wide prerequisite fixed in this pass**: `cargo fmt --check` and
`cargo clippy --all-targets -- -D warnings` had never been run against this
codebase — the former failed on every source file (rustfmt defaults were
never applied), the latter had ~11 real violations across `crates/core`,
`crates/app`, and `bundler`. Both are now clean workspace-wide; this had to
happen before either could be a meaningful CI gate rather than one that's red
from the moment it's turned on.

### Known gaps (prioritized backlog, not yet implemented)

The boundaries below are the actual current risk surface — Rust code that's
covered as pure logic but not at the process/network boundary it wraps.
Highest-value first:

1. **`huggingface.rs::list_gguf_files`** — status-code branching (401/403 →
   gated, 404, 429 retry-after, 5xx, non-JSON body) is only reachable today
   via the 3 `#[ignore]`d live-network tests. A `wiremock` server (new dev-dep;
   `reqwest`/`tokio` are already workspace deps, so this fits the existing
   `#[tokio::test]` style) would cover all five branches hermetically.
2. **`download.rs`** — resume (`Range`/206), a server that ignores `Range`
   and replies 200 (truncate-and-restart), and cancel-mid-stream are all
   untested, even though the file already has the exact scaffolding needed
   (`serve_once`, a hand-rolled one-shot loopback HTTP server used for the
   existing checksum tests) — extending that pattern to multi-request
   scenarios is straightforward.
3. **`engine/mod.rs`'s `EngineManager::start()` health-poll loop** — the three real branches
   (connection-refused vs. 503-loading vs. 200-ready) and the
   child-exited-during-startup / startup-timeout paths are only exercised by
   the real-binary `#[ignore]`d test. A fake `/health` HTTP server standing in
   for llama-server (the actual spawned "child" can be a no-op placeholder
   process) would cover this without needing the real binary.
4. **`plugin.rs::run_setup`/`start_service`** — the venv-creation → pip
   install → setup-script → service-health-poll pipeline, and the
   `SetupState`/`ServiceState` transitions it drives, have zero coverage from
   any test that spawns even a fake process. A tiny fixture script standing
   in for `python3` (or `OPENPAL_PYTHON` pointed at a shell script) would
   close this.
5. Smaller, cheap wins: `crates/app/src/audio.rs::encode_wav` (pure,
   hardware-free, currently untested), `commands.rs::model_status`/
   `friendly`, `state.rs::resolve_data_dir`'s portable-mode branches.

None of these block the CI gate going in — they're gaps in an
already-passing suite, not failures. Pick them up incrementally.

### What CI runs vs. what stays manual

`cargo test -p <crate>` (bare, `--ignored` excluded) is what `ci.yml` runs.
The `#[ignore]`d tests — `engine_e2e.rs`/`writing_tools_e2e.rs`/`prompt_enhancer_e2e.rs`'s real llama-server runs and
`huggingface.rs`'s 3 live-network tests — download real resources and hit
live services, so they're deliberately excluded from the default run and only
reachable via `engine-e2e.yml`'s path-filtered job or a manual
`-- --ignored` invocation. A failure there means "investigate," not "the PR
that triggered it broke something."

## Frontend (`ui/`)

There was no test infrastructure at all before this pass — no framework, no
test files, no lint config. `npm run build` (`tsc --noEmit && vite build`)
was the only safety net.

### Unit/component tests (Vitest + Testing Library)

Added `vitest` + `@testing-library/react`/`dom`/`jest-dom`/`user-event` +
`jsdom`, wired into the existing `vite.config.ts` (Vitest's `defineConfig`
from `vitest/config` is a drop-in superset of Vite's own, so `plugins`/
`server`/`build` are untouched). Run with `npm test` (`npm run test:watch`
for the interactive runner). Currently covers:

- `lib/format.ts` — every formatting tier boundary (`formatTokensPerSec`,
  `formatDuration`, `timeAgo`, `formatChatStats`), `totalModelSizeBytes`
  (split-GGUF shard summing), `isAutoRecommendable`/`roughFitLabel`.
- `lib/speedEstimate.ts` — the fit-level branching (`perfect`/`good` →
  unqualified estimate, `diskSlow`/`tooBig` → exactly 0, `tight` →
  continuous derate), the Apple-chip bandwidth table's most-specific-match
  ordering (`"M4 Max"` must not fall through to bare `"M4"`), MoE vs. dense.
- `state/store.ts` — the exported pure predicates `modelBrowserAvailable`
  and `cloudModelsAvailable`.

**Backlog** (from the research pass, not yet implemented — concrete enough to
pick up directly): `lib/api.ts`'s Channel-based methods need a hand-rolled
`vi.mock("@tauri-apps/api/core")` (the official `@tauri-apps/api/mocks`
helper covers plain invoke/response methods fine via `mockIPC`, but a real
Tauri runtime replaces a `Channel` argument with a numeric callback id before
a mock handler would ever see it — faking the transport for streaming needs
its own fake `Channel` class); `store.ts`'s `send()` action (title truncation,
the "drop the empty placeholder reply" rule, citation de-dup) and
`updateGeneration()`'s save-debounce, both via `vi.mock("../lib/api")` +
`vi.useFakeTimers()`; `components/Dropdown.tsx`'s `useCloseOnOutside` hook;
`ChatView.tsx`'s `AttachmentList` (needs a one-line `export` added — it's
currently module-private) and the lightbox/doc-panel Escape-stacking order.

### Mocked-IPC e2e (Playwright)

The biggest gap wasn't just "no unit tests" — there was also no way to
exercise a real user flow through the actual rendered app. Tauri's native
WebDriver e2e (`tauri-driver`) has no macOS support as of this writing (only
Linux/WebKitGTK and Windows/WebView2 are "true" OS-level WebDriver — macOS
support only exists via a pre-1.0 embedded-driver plugin, not worth adopting
yet), and even where it works it needs a full native build. The pragmatic
middle tier — and the one every PR now runs — is Playwright against the
plain Vite dev server with the Tauri IPC layer mocked:

- `ui/playwright.config.ts` boots `npm run dev` and points Chromium at it.
- `ui/tests/e2e/support/tauriMock.ts` injects a minimal
  `window.__TAURI_INTERNALS__.invoke` shim via `page.addInitScript` — every
  registered command resolves to a canned fixture. Deliberately doesn't
  implement the real `Channel` wire protocol (see the note in that file);
  flows that stream (chat, downloads, plugin setup) need their own
  purpose-built mock, same caveat as the Vitest side.
- `ui/tests/e2e/onboarding.spec.ts` is the first (smoke) spec: a fresh
  install's `boot()` sequence resolves against fixtures and the app renders
  the onboarding welcome screen, with zero browser console errors.
- `ui/tests/e2e/onboarding-skip.spec.ts` drives the two skip buttons from a
  fresh install: "Skip this step" (moves on, no question asked) and "Skip setup"
  (asks "Skip setup?" first, then enters the app), including a download held in
  flight and the commands sent to the backend; `npm run test:e2e:onboarding` (in
  `ui/`) runs just that spec. It is a headless pass/fail check — to use
  onboarding by hand in the real app, run `npm run dev:onboarding` from the repo
  root (a `tauri dev` session on a throwaway first-run profile; see the README's
  [Development](../README.md#development) section).
- `ui/tests/e2e/theme.spec.ts` is the Theme setting meeting a real stylesheet,
  which jsdom never computes. For every OS appearance × theme pair it checks
  which palette `global.css` paints and that Light and Dark pin `color-scheme`;
  that System follows an OS flip while the app is open and a pinned theme
  ignores it; the Settings → General control (names, arrow keys, a pick that
  shows before its save settles); that portalled dialogs, a native select and
  the phone-width layout (English and French) follow it; and that after a
  relaunch (`support/persistedSettings.ts` makes the mocked backend remember
  saves) `<html data-theme>` is already set when the shell or onboarding first
  appears, while the lock screen, drawn before settings can be read, follows the
  OS. It never touches a real window: the OS-drawn frame, how a webview paints
  select popups and scrollbars, a real OS appearance change reaching
  WKWebView/WebView2 and the production stylesheet stay manual checks.

**This tier is a fast frontend regression net, not a substitute for real
e2e** — it validates React state machines, routing, and IPC call shape, not
the actual Rust command handlers, real IPC serialization, subprocess
behavior, or native OS integration (file dialogs, the mic, native webview
quirks). Both this tier and a native tier are needed; neither subsumes the
other.

**Backlog**: the onboarding spec is one smoke test proving the harness works
end-to-end — the actual payoff comes from adding the flows this tier is
built for: model download progress → install → select → `start_engine` →
composer enabled; a full chat send → streamed tokens → rendered reply;
attachment pick → included in the next send; a plugin enable → streamed
setup progress → running, including the *failure* branches (checksum
mismatch, engine spawn failure, plugin setup failure, mid-stream chat error)
that are hardest to trigger for real and where mocking earns its keep most.

## Python plugins

Both `plugins/web-search/tests/` (39 tests) and `plugins/speech/tests/` (39
tests) already existed with solid coverage — pure unit tests plus
integration-with-mocks (a stdlib `http.server`-based fake LLM/SearXNG for
web-search; a real `ThreadingHTTPServer` with `stt`/`tts` monkeypatched for
speech). Nothing in either suite touches a real network call, a real
SearXNG instance, or real model weights/binaries.

**A real, previously-undetected bug was found and fixed in this pass**:
`plugins/speech/tests/test_server.py`'s `/speak` test stub predated four
keyword-only parameters (`length_scale`, `noise_scale`, `noise_w`,
`sentence_silence`) added to `tts.synthesize`'s real signature — the test had
never passed since the commit that introduced both files, it just had never
run in CI. Exactly the class of regression a CI gate exists to catch; fixed
by widening the stub to accept `**_kwargs`.

CI (`plugin-tests.yml`) installs each plugin's plain `requirements.txt` (not
`requirements-rerank.txt` — torch/FlagEmbedding, which no test imports; one
test specifically asserts the graceful no-torch fallback) plus `pytest`/
`pytest-cov`, and does **not** run `setup_plugin.py` (no test needs the real
SearXNG/Piper/Whisper download it does). Coverage is reported
(`--cov-report=term-missing`) but not gated on a threshold yet — 78% for
web-search, 48% for speech (dragged down by genuinely untested paths:
`setup_plugin.py`, the standalone `cli.py`, direct STT invocation — not test
bugs). Revisit a real `--cov-fail-under` once those gaps are deliberately
closed.

## Native e2e (real built app)

Not implemented in this repo. The research behind this plan confirmed
Tauri's native WebDriver e2e (`tauri-driver` + WebdriverIO) is mature only on
Linux (WebKitGTK) and Windows (WebView2) — macOS has no official WKWebView
driver, only a pre-1.0 embedded-plugin workaround not worth adopting yet.
Even on Linux, a real run needs a full `tauri build`, the real engine binary,
and (for a meaningful test) a real model — realistically 10-15 minutes, too
slow and too failure-prone to gate every PR.

Two ways to slot this in, deliberately left as an open decision rather than
built out speculatively:

1. **A scheduled/tag-triggered job in this repo**, e.g.
   `e2e-native.yml` on a nightly `schedule` + `workflow_dispatch` + `push:
   tags: v*`, mirroring `portable.yml`'s own tag trigger — never a required
   PR check, but a visible signal, and a hard go/no-go gate in the release
   runbook before promoting a tag's draft release.
2. **A separate repository/workflow** that consumes this repo's release
   artifacts — `portable.yml` already produces a signed **draft** GitHub
   release on every `v*` tag (inert until a human runs
   `gh release edit <tag> --draft=false`), which is exactly the natural hook
   point: an external e2e run downloads the draft's Portable build, drives it
   with `tauri-driver`, and its result is one more input to the human
   decision of whether to promote the draft to public.

Either way, the two concrete specs worth having once this lands: (a) launch
→ download/load the cached SmolLM2 test model → real chat round-trip → clean
shutdown (exercises real subprocess spawn, port pick, health-check, SSE
streaming, kill-on-drop); (b) enable the web-search plugin → real venv
create + pip install → service running → disable → clean teardown
(lifecycle only, not a live search call — SearXNG calling out to public
search engines is a real flakiness source for a CI gate).

## Branch strategy and required checks

OpenPal ships versioned, signed releases (not a continuously-deployed
service), so the branch model is **trunk-based development with short-lived
release branches** — the common SaaS-industry middle ground between plain
GitHub Flow (no release branches at all) and full GitFlow (a permanent
`develop` branch, which adds process weight this project doesn't need):

- **`main`** is trunk: always releasable, protected. Feature/fix branches
  branch from it and land back via PR + squash merge. Required status
  checks: `ci.yml`'s `rust` and `frontend` jobs (always run), plus
  `plugin-tests.yml`/`engine-e2e.yml` (path-filtered — satisfied
  automatically when not triggered, so safe to mark required).
- **`release/vX.Y`** branches are cut from `main` when stabilizing a release
  — only fixes land here after the cut, no new features. Same required
  checks as `main`; also protected.
- **Tags** (`vX.Y.Z`) are pushed from a release branch (or `main`, for a
  patch release with no separate stabilization period) and trigger
  `portable.yml`'s existing build + signed **draft** release. A human
  promotes the draft after checking it (and, once native e2e exists, after a
  green run against it).
- **Hotfixes** branch from the tag/release branch, PR back into that release
  branch, and get cherry-picked forward into `main` (or vice versa) so a
  patch never silently regresses on the next release.

Setting the actual GitHub branch-protection rules (which checks are
"required", merge restrictions) is a repo-admin action outside this plan's
scope — apply the mapping above via **Settings → Branches** once these
workflows have a few green runs to build confidence in.

## Coverage tooling

Not gated on a hard threshold anywhere yet — the priority was going from
"no CI at all" to "CI runs everything that exists and reports numbers",
not picking an arbitrary percentage to enforce on day one:

- **Rust**: no coverage tool wired up yet. `cargo-llvm-cov` is the
  reasonable choice when this is revisited (native, no separate profiling
  toolchain install needed on a modern Rust).
- **Frontend**: `@vitest/coverage-v8` is already installed
  (`npm test -- --coverage`); not yet added as a CI step or gate.
- **Python**: `pytest-cov` runs in CI and reports inline
  (`--cov-report=term-missing`); see the two baseline numbers above.

Revisit real thresholds once the highest-priority backlog items in each
section above are closed — a threshold set now would either be toothless
(low enough to always pass) or immediately red on the gaps already known
about.
