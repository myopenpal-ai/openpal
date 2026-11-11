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
| Rust real-model prompt-injection detector | `crates/core/tests/prompt_injection_corpus.rs` (`#[ignore]`) | `cargo test -p openpal-core --test prompt_injection_corpus -- --ignored --nocapture` |
| Frontend unit/component tests | `ui/src/**/*.test.ts(x)` (Vitest) | `npm test` (in `ui/`) |
| Frontend mocked-IPC e2e | `ui/tests/e2e/**` (Playwright) | `npm run test:e2e` (in `ui/`) |
| Python plugin tests | `plugins/web-search/tests/`, `plugins/speech/tests/` (pytest) | `python -m pytest` (in each plugin dir) |
| Native e2e (real built app) | not in this repo yet — see [Native e2e](#native-e2e-real-built-app) | — |

## CI workflows

| Workflow | Triggers | Blocks merge? | What it runs |
|---|---|---|---|
| `.github/workflows/ci.yml` | every PR, push to `main` | **yes** (required) | four parallel Rust jobs — `rust-clippy` (`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`) and a `rust-test` matrix with one job per crate (`rust-test (openpal-core)`, `rust-test (openpal-app)`, `rust-test (openpal-bundler)`, each running `cargo test -p <crate>`) — plus the `frontend` job (`ui` typecheck + `vite build`, ESLint, Vitest, Playwright mocked-IPC e2e) |
| `.github/workflows/plugin-tests.yml` | PR/push touching `plugins/web-search/**` or `plugins/speech/**` | yes, scoped | pytest for whichever plugin(s) changed |
| `.github/workflows/engine-e2e.yml` | PR/push touching `crates/core/src/engine/**`, `download.rs`, `sse.rs`, `writing_tools.rs`, `prompt_enhancer.rs`, or any file under `crates/core/tests/` | yes, scoped | the real llama-server engine tests (stages the engine via the bundler, downloads the ~140MB SmolLM2 test model, runs a real chat round-trip for plain chat, Writing Tools Translate/Correct, and the Message improver) |
| `.github/workflows/injection-e2e.yml` | PR/push touching `crates/core/src/injection/**`, `crates/core/tests/prompt_injection_corpus.rs`, `crates/core/tests/fixtures/prompt_injection/**` or `Cargo.lock` | scoped | the real prompt-injection classifier (downloads the pinned ~300MB model, cached by its sha256) over the documented benign/injection sample sets, one known payload per route, a buried payload's most suspicious window, and a review card's excerpt holding a payload at the end of a window longer than the card shows |
| `.github/workflows/notify-platforms.yml` | PR/push touching `crates/app/src/notify/**`, `crates/app/src/lib.rs`, `crates/app/Cargo.toml` or `Cargo.lock` | yes, scoped | `cargo clippy -p openpal-app --all-targets -- -D warnings` and `cargo test -p openpal-app notify` on macOS and Windows runners, and `cargo clippy -p openpal-app` for `aarch64-linux-android` with the runner's NDK — the platform-gated notification backends `ci.yml`'s Linux job never compiles. Not the Kotlin half (no Gradle build), and nothing that shows a notification |
| `.github/workflows/portable.yml` (pre-existing, unchanged) | manual dispatch, `v*` tags | no (release pipeline, not a test gate) | cross-platform Portable builds + signed draft release |

Path-filtered workflows (`plugin-tests.yml`, `engine-e2e.yml`,
`injection-e2e.yml`, `notify-platforms.yml`) simply don't run at all on a
PR that doesn't touch their paths — GitHub treats a required check from a
workflow that wasn't triggered as satisfied, so they're safe to mark as
required status checks without blocking unrelated PRs.

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

**The prompt-injection review is covered by unit tests, with no model.**
They run a fake classifier (`Detector::with_classifier`), so the bare
`cargo test` covers them:
- `injection::review`'s tests cover the pause, Continue reaching the model
  byte-identical, Stop, the Stop button while checking or while the card is
  up, "unchecked" on a classifier error or timeout, and per-conversation
  memory.
- `agent::orchestrator`'s tests record every request the fake model server
  gets (`fake_server_capturing`). That proves the model saw nothing of a
  flagged result before Continue, and nothing at all after Stop.
- The glue every checked turn goes through, `injection::run_guarded`, lives
  in core so it can be tested there; the app crate has no Tauri test harness.
- Which turns are checked at all is one pure function,
  `TurnRoute::decide` / `TurnRoute::scans` in `commands.rs`, pinned by
  `turn_route_follows_route_turns_order`: only agent and web-search turns.
  That table is the only guard on the mapping. `injection-review.spec.ts`
  plays the backend's part, so it can't show that plain chat doesn't pause.
- `injection`'s own tests load a stand-in classifier over sparse
  placeholder files (`model::fake_download`): loaded by the first scan,
  never ahead of it; kept while a tool turn holds it (`ClassifierHold`), with
  the idle clock restarting when the last hold drops; unloaded with the
  engine; and never dropped mid-scan by the idle timer. The app-side calls
  (`commands::unload_classifier`, `idle.rs`) have no test, for the reason
  above.

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
The `#[ignore]`d tests — `engine_e2e.rs`/`writing_tools_e2e.rs`/`prompt_enhancer_e2e.rs`'s real llama-server runs,
`prompt_injection_corpus.rs`'s real-classifier runs (sample sets and their
README in `crates/core/tests/fixtures/prompt_injection/`), and
`huggingface.rs`'s 3 live-network tests — download real resources and hit
live services, so they're deliberately excluded from the default run and only
reachable via `engine-e2e.yml`'s or `injection-e2e.yml`'s path-filtered jobs
or a manual `-- --ignored` invocation. A failure there means "investigate,"
not "the PR that triggered it broke something."

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
- `state/store.ts`'s prompt-injection review handling (the
  `"prompt-injection review"` block in `store.test.ts`): a review held until
  answered, the notices and `withheldFromModel` saved at settle, the
  "Checking…" status, and Retry. It mocks `../lib/api`, and its `chatStream`
  mock plays the turn by calling `onEvent` with `ChatEvent`s.
- `components/InjectionReviewCard.tsx` — an alertdialog named by its title,
  focus on the card itself (never a button, and not taken from a draft being
  typed or from an open preview, whose Escape stays its own), Escape means
  Stop, excerpts and names rendered as literal
  text (never Markdown), the "unchecked" and invisible-characters lines, and
  French. `components/InjectionNotices.tsx` — every route × outcome gives a
  real sentence, with a fallback for a value from a newer version.
- The in-app alerts and the native-notification rules (the settings side is
  the next bullet):
  - `lib/engineEvents.test.ts` and `state/storeAlerts.test.ts` — "model loaded" and
    "model unloaded" come from status *transitions*: the same status read again
    and again is one alert, the first read after launch is a silent baseline, a
    swap A → B is one load (stale or transient statuses in between included),
    failures and the remote states announce nothing, and an older status reply
    landing late is dropped rather than re-announcing an unload.
  - `lib/appEvents.test.ts` and `state/storeAlerts.test.ts` — "answer ready" fires
    once for a turn that really ran to its end (not Stop, not an error, not an empty
    reply, not Translate/Correct toggling `streaming`), its in-app alert is skipped
    only when that conversation is open in the chat view with the More dialog and
    conversation search closed, "download finished" fires
    from the `finished` event once per model (not on cancel, failure, or a queued
    re-run), and a native notification carries the alert's own title and text only
    with the switch on and the window not focused — including for the open
    conversation, and never from a build with no OS notifications
    (`nativeNotifications: false`, set without `isAndroid` so the capability is
    what's tested). These need Vitest because the e2e mock
    cannot stream answers or downloads.
  - `components/AlertHost.test.tsx` — the host on fake timers: a live region that
    exists before any alert, no assertive second announcement, self-dismissal after
    6s that pauses on hover, focus and window blur, Dismiss and Escape (consumed so
    it can't also close a dialog), no focus ever taken, newest first and capped.
    `styles/tailwind.test.ts` pins the shadcn colour mapping to existing palette
    tokens, and `styles/contrast.test.ts` holds the alert icon and dismiss colours
    to 3:1 in all three palettes.
- The "Desktop notifications" setting, settings side:
  - `state/storeNotifications.test.ts` — `setDesktopNotifications` is optimistic,
    persists the whole settings object, writes nothing for an unchanged value
    (including a row saved before the field existed), rolls a failed write back
    (only the latest pick's) with a notice, and asks the OS
    (`requestNotificationPermission`, the call that raises macOS's prompt) only
    after a successful turn-on; `refreshNotificationPermission` only reads, and
    both keep the last answer when the call fails. It has an api mock of its own, so a stray call to
    any other command throws.
  - `views/SettingsSheet.test.tsx` — the Notifications group (named switch
    described by its hint, off for a missing field, one click saves, a failed
    write goes back), the "blocked" line (only with the switch on and the OS
    saying no, a plain description rather than an alert, cleared when the window
    regains focus), its "Open notification settings" button (only while the line
    shows, outside the status region, one `openNotificationSettings` call per
    press, and a fallback sentence appended to the line when that call rejects),
    the group's order between Language and Updates, its absence
    from a build with no OS notifications, and French.
  - `views/Onboarding.test.tsx` — the notifications step sits between appearance
    and the hardware scan: Skip this step writes nothing, "Turn on notifications"
    saves the choice and moves on without waiting for the OS (even when it says
    no), "Skip setup" asks first, and a build with no OS notifications skips the
    step.
  - `lib/settingsSearch.test.ts` and `views/MoreDialog.test.tsx` — the group and
    the switch are findable in English and (accent-folded) French, hidden in a
    build with no OS notifications, and picking the switch opens General with
    focus on it.
- The Rust side, `cargo test -p openpal-app notify`: the length caps and
  escaping (`notify/content.rs`), the macOS status mapping, bundle-path guard and
  click payload (`userInfo` round trip), the Android answer mapping, the deep
  link, each backend the host can build against the shared `assert_contract`
  (on Windows that includes "reading leaves no registry trace, asking registers
  the app id", on a throwaway id it removes again), the backend `select` gives a
  test binary (never the macOS center, which would abort it), and what
  `Notifications` does on top of a fake backend (text capped and escaped per
  backend, the click's conversation passed through untouched, errors passed
  through, a click waiting for the UI once with the latest winning). The macOS
  center and the Android plugin only run in a bundled app; `notify-platforms.yml`
  runs this on macOS and Windows runners and type-checks Android.
- The click, UI side: `state/storeNotifications.test.ts` holds
  `followNotificationClick` (nothing before settings load, so the click waits in
  Rust; the conversation opened with the More dialog and search closed; a click
  naming none changes nothing; a failed read never throws);
  `state/storeAlerts.test.ts` that "answer ready" is posted with its
  conversation and a model event with none; `lib/apiEvents.test.ts` the
  `notification-clicked` event name and `take_notification_click`. The phone
  wording of the switch, its hint, its search entry and the onboarding step
  are in `SettingsSheet.test.tsx`, `settingsSearch.test.ts` and
  `Onboarding.test.tsx`.

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
- Playwright reuses a dev server already on :5173 outside CI
  (`reuseExistingServer`). If another checkout's `npm run dev` holds that
  port, the run silently tests that checkout's code: stop it first, or run
  with a config that uses another port.
- `ui/tests/e2e/support/tauriMock.ts` injects a minimal
  `window.__TAURI_INTERNALS__.invoke` shim via `page.addInitScript` — every
  registered command resolves to a canned fixture. Deliberately doesn't
  implement the real `Channel` wire protocol (see the note in that file);
  flows that stream (chat, downloads, plugin setup) need their own
  purpose-built mock, same caveat as the Vitest side
  (`injection-review.spec.ts`, below, has one for `chat_stream`).
- `ui/tests/e2e/onboarding.spec.ts` is the first (smoke) spec: a fresh
  install's `boot()` sequence resolves against fixtures and the app renders
  the onboarding welcome screen, with zero browser console errors.
- `ui/tests/e2e/onboarding-skip.spec.ts` drives the two skip buttons from a
  fresh install: "Skip this step" (moves on, no question asked) and "Skip setup"
  (asks "Skip setup?" first, then enters the app), including a download held in
  flight and the commands sent to the backend, and the notifications step that
  follows appearance: skipping it leaves "Desktop notifications" off all the way
  into Settings, "Turn on notifications" saves it on (and Settings shows it
  checked, also after a relaunch), the OS answering "denied" lets onboarding go
  on and leaves Settings showing the "blocked" line with no alert and no console
  error, and "Skip setup" from that step asks first; `npm run test:e2e:onboarding`
  (in `ui/`) runs just that spec. It is a headless pass/fail check — to use
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
- `ui/tests/e2e/injection-review.spec.ts` drives the prompt-injection review
  card: attach a file, send, the card, Stop / Continue / Escape, the notice
  line, the "Not sent to the AI" tag, and what `save_conversation` gets;
  also that a card showing up under an open preview or a message being
  typed leaves their keys alone, and that a long file name in a notice
  wraps inside the narrowest chat column. Its
  `openStreamingChat` is the streaming mock to copy for `chat_stream`:
  - On top of `mockTauriIpc`, an init script replaces
    `__TAURI_INTERNALS__.transformCallback` so each Channel callback is kept
    by id, and stubs `unregisterCallback`, which a Channel calls when its
    end frame arrives.
  - It wraps `invoke` so `chat_stream` returns a promise that stays pending.
    The test then plays the backend: it delivers `{ index, message }`
    frames, each `message` a `ChatEvent`, to the callback of the `onEvent`
    Channel's id, and finally `{ index, end: true }` before resolving.
  - Every command is recorded with its arguments, so a test can assert what
    the UI sent, e.g. `respond_injection_review { requestId, decision }`.
- `ui/tests/e2e/alerts.spec.ts` drives the alerts by changing what the mocked
  `get_engine_status` answers while the page polls it on a fake clock: one "Model
  ready" per load however many times Running is read, an unload, a swap, and the
  silent first read; the card paints above an open dialog, never takes focus from
  the composer, clears itself after 6s, is reachable with Tab and dismissed with
  Escape, and stacks newest-first up to three; a card dismissed with the mouse
  (Chromium sends no `pointerleave` when it disappears under a resting pointer)
  and a press on a card that must not count as a click outside an open panel are
  covered, and so is an alert raised while onboarding is up. The clock is
  stopped after boot so only the test moves time. Window `blur`/`focus` events show
  the rule for `notify_native` end to end -- called once with the card's own
  title and text only with the switch on and the window blurred, never with it
  off, and a rejecting call surfaces nothing. Answers and downloads arrive over
  Channels, which this tier cannot stream (Vitest above); what stays manual is a
  notification actually appearing, and a screen reader announcing the card.
- `ui/tests/e2e/notifications.spec.ts` is the Settings → General → Notifications
  group in a real browser: an install saved before the setting existed shows
  "Desktop notifications" off, one click saves `desktopNotifications: true` with
  the rest of the settings, the choice survives a relaunch both ways
  (`support/persistedSettings.ts`), and the switch works from the keyboard. The
  OS is the mocked `notification_permission` (a read) and
  `request_notification_permission` (the ask a click makes), which the spec can
  change while the page runs: opening Settings only reads, turning the switch on
  asks once, the "blocked" line shows only with the switch on and the
  OS answering "denied" (a plain description, no `role="alert"`, the switch still
  usable) next to a button that sends `open_notification_settings` (and, when the
  mock rejects it, a sentence saying where to look), clears when the window
  regains focus after the answer changes, and a failing read shows nothing and
  logs no console error. It also covers the group
  being absent where `get_app_info` says there are no OS notifications (and from
  search), the French labels, and a click that launched the app
  (`take_notification_click` answering at boot) opening its conversation, read
  exactly once; the mock has no event bridge, so the `notification-clicked`
  nudge itself is unit-tested only;
  `more-dialog.spec.ts` covers finding the switch through the More dialog's
  search. What stays manual, because only the shipped app has a notification
  service: a notification actually being posted and shown (also with the window
  minimized, where OpenPal is still the active app), the macOS and Android
  permission prompts, the OS really reporting "blocked" (macOS, Windows and
  Android can), the "Open notification settings" button landing on System
  Settings -> Notifications -> OpenPal (the deep link in `notify/mod.rs`'s
  `macos_settings_url`; the unit tests pin the URL, not where macOS takes it),
  on Windows' notifications page and on Android's app notification settings, a
  click (or tap) bringing OpenPal forward and opening the answer's conversation,
  including the tap that cold-starts the Android app, and that none is posted
  while OpenPal is the focused window. On Windows, check a portable build from a
  folder outside `target/`: before this change its toasts were dropped silently
  (no Start-menu AUMID), so a toast appearing at all, named "OpenPal", is the
  test, and `HKCU\Software\Classes\AppUserModelId\com.myopenpal.openpal` should
  exist only once the switch has been turned on. On Linux, check a GNOME
  session (the notification stays in the tray instead of vanishing, and a click
  opens the conversation). After touching `crates/app/src/notify/`
  on macOS, run the bundled app and read `log show --last 5m --predicate
  'process == "usernoted"'` for the app's bundle id: `Delivering <NotificationRecord`
  means the post arrived; `can't mix modern clients with legacy clients` or
  `Denying message` means something in the process used the legacy and the modern
  center together (see `notify/macos.rs`), and nothing will ever be shown.

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

- **`main`** is trunk: always releasable, protected — both now enforced in the
  repository, not just documented. Feature/fix branches branch from it and
  land back via **PR + squash merge**, and the repository settings plus
  `main`'s branch protection make that the only path: squash is the only
  merge method offered (merge commits and rebase merges are disabled), head
  branches are deleted automatically on merge, direct pushes to `main` are
  rejected, and force-pushes and branch deletion are blocked. The required
  checks are recorded on `main`'s branch protection:
  - `ci.yml` (always run): `rust-clippy (fmt, clippy)`,
    `rust-test (openpal-core)`, `rust-test (openpal-app)`,
    `rust-test (openpal-bundler)`,
    `frontend (typecheck, unit, mocked e2e)`.
  - Path-filtered (auto-satisfied when their paths don't change, so safe to
    mark required): `plugin-tests.yml` → `speech`; `engine-e2e.yml` →
    `engine-e2e`; `notify-platforms.yml` → `notify on macos (clippy, tests)`,
    `notify on windows (clippy, tests)`, `notify on android (clippy)`.
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

These rules are live, not a plan: the repository is set to squash-only with
head-branch deletion, and `main`'s branch protection enforces the
required-check set above (also for admins) and blocks direct pushes,
force-pushes and deletions. Keep them in sync: if a workflow is renamed,
moved or dropped, update the required set on `main`'s branch protection in
the same change, so no PR is ever blocked on a check that no longer exists.

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
