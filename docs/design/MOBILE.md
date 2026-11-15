# OpenPal: extending to Android and iOS

This document is the plan for taking OpenPal from a Windows/Linux/macOS desktop
app to a five-platform app (+ Android, iOS) **from the same codebase**. It
complements [`ARCHITECTURE.md`](../../ARCHITECTURE.md) (read that first for how the
app is structured today: Tauri shell → `openpal-core` → `llama-server` subprocess +
Python plugin sidecars), [`PORTABLE.md`](PORTABLE.md) (the existing
full/portable desktop split — mobile is a third product line, not a replacement
for either), and [`REMOTE_INFERENCE.md`](../REMOTE_INFERENCE.md) (desktop-as-
inference-host, merged 2026-08-28 — a genuinely different, complementary path to
a mobile experience; see the Phase 1 status note below for how the two relate).

Produced 2026-08-28 via a research + independent-proposal + two-judge review
workflow (5 research agents, 3 competing strategy proposals, 2 adversarial judges
— both judges independently converged on the same winner). Full source material
kept in the session; this doc is the synthesis.

---

## 1. The constraint that decides everything

Before "how many repos," there's a harder question: **does the current engine
architecture even run on mobile?** No — and this isn't a maturity gap that a
newer Tauri release fixes, it's a kernel/App-Store-policy wall, verified directly
against source and policy docs rather than assumed:

- **iOS**: third-party apps cannot call `fork()`/`exec()`/`posix_spawn()` **at
  all** — a kernel sandbox denial (TrustedBSD MAC framework), not a review
  guideline you might talk your way past. Verified against Tauri's own iOS shell
  plugin source (`plugins/shell/ios/Sources/ShellPlugin.swift`): the entire class
  has exactly one command, `open(url)`. There is no spawn/execute/kill handler to
  even attempt.
- **Android (10+ / API 29+)**: W^X enforcement means a binary can only be
  `exec()`'d if it was baked into the read-only APK at build time — never one
  downloaded or written into the app's writable data dir at runtime. That's
  exactly OpenPal's pattern today: `llama-server` is staged into a resource dir
  and spawned, and each plugin's Python venv/interpreter is created and spawned
  the same way. Google Play's Device and Network Abuse policy independently
  forbids downloading executable code (including compiled `.so` extensions
  inside `pip`-installed wheels) from anywhere but Play at runtime.

**Net effect:** `EngineManager`'s `tokio::process::Command::new(server_binary)…
kill_on_drop(true)` pattern (`crates/core/src/engine.rs`) does not port to either
mobile OS, and neither does `PluginManager`'s venv-create + `pip install` +
spawn-`service.py` pattern (`crates/core/src/plugin.rs`, `plugins/PLUGINS.md`).
This is a fixed cost paid under **every** framework option below — it doesn't
favor or penalize any of them, so it isn't treated as a differentiator in §2, but
it does set the real cost floor for all of them.

**The only proven alternative:** link llama.cpp **in-process** via Rust FFI
(`llama-cpp-2`, a near-1:1 safe wrapper whose `build.rs` already treats
`aarch64-linux-android` and `aarch64-apple-ios` as first-class cross-compile
targets) instead of spawning it as a subprocess. This is not speculative — it's
how every shipping mobile llama.cpp app works:

- llama.cpp's own official examples, `examples/llama.android` (JNI) and
  `examples/llama.swiftui` (Swift/Obj-C++ glue over an `.xcframework`), prove the
  pattern end to end.
- `llama.rn` (the React Native binding used by **PocketPal AI**, **ChatterUI**,
  and **Maid**) follows the identical shape: llama.cpp compiled to a prebuilt
  `.xcframework`/`jniLibs`, called in-process through a JSI bridge — no
  subprocess, no HTTP, direct function calls.

Same story for the Python plugin sidecars: **SearXNG** (web search) has no
realistic embeddable mobile form and no iOS story at all — the pragmatic answer
is to call a hosted search API directly over HTTPS instead (the agent's
search/fetch/rerank *tool loop* is already native Rust, so only the search-index
HTTP backend changes). **faster-whisper/Piper** (speech) have real in-process
mobile replacements (`whisper.cpp`, `sherpa-onnx`, both with first-class
Android/iOS bindings) but porting them is its own multi-week project, scoped out
of mobile v1 below.

---

## 2. Framework choice: Tauri 2 mobile

Given §1's cost is paid identically under any option, the comparison turns on how
much of the existing investment — 17.3k lines of GUI-free Rust core
(`crates/core`), 14.8k lines of React UI behind one clean `invoke`/`Channel` seam
(`ui/src/lib/api.ts`) — survives.

| # | Option | `core` reuse | `ui` reuse | Cost | Maintenance shape |
|---|---|---|---|---|---|
| **1** | **Tauri 2 mobile** — reuse React UI + Rust core, add Android/iOS build targets | ~70–80% | ~60–75% | ~3–5 months to a shippable build, *given* the engine/plugin rework in §1 is done once | **One core, one UI, five shells** |
| 2 | Keep Rust core, native UI (SwiftUI + Jetpack Compose) via UniFFI/FFI | ~85–90% (best) | ~0% (full rewrite, twice) | 8–14 months | One core, **three** UIs |
| 3 | Kotlin Multiplatform + Compose Multiplatform | ~0% or ~80–85% depending on sub-path | ~0% | Inherits option 2's FFI cost without option 1's UI-reuse payoff | Adds a third stack for no codebase-specific win |
| 4 | React Native / Flutter, reimplement business logic, direct llama.cpp binding | ~0–10% (worst) | ~0% | 8–14 months, higher *correctness* risk (re-derives tested hardware/selection/download logic from scratch) | **N cores, N UIs** — permanent duplication |

**Ranked: 1 > 2 > 3 ≈ 4.** Tauri 2 mobile is the only option that substantially
reuses *both* the UI and the core rather than forcing a full rewrite of one or
the other, and the only one that keeps mono-model-first enforced by the same
`engine.rs`/`EngineManager` code path already used (and tested) on desktop,
rather than re-deriving that invariant in a second language. Option 2 is a
credible second choice worth revisiting for *one* platform later if real-user
testing shows WebView performance on low-end Android hardware is a genuine
beginner-UX blocker — but two native UIs from day one is too much permanent
surface for a solo/small maintainer as the *opening* move. Options 3 and 4 have
no compelling case for a codebase that already has a working React UI and a
well-tested Rust core to reuse.

*(Tauri mobile maturity caveat: stable since Tauri v2's 2024-10-02 release, and
by mid-2026 actively maintained — but the Tauri team has publicly said mobile DX
isn't yet at desktop parity. This is a bounded, shrinking risk, which is why the
migration path in §5 front-loads a cheap validation step before committing to the
expensive engine port.)*

---

## 3. Repo strategy: one repo, unchanged topology

Directly answering "1 app for mobile / 1 for desktop? 1 repo per app? 1 repo per
architecture?" — **none of those.** Every real-world case study of a small team
splitting repos by platform (Signal's `Signal-Desktop`/`Signal-Android`/
`Signal-iOS`/`libsignal` split, Mozilla's `application-services`) traces back to
an *organizational* split — independent per-platform teams wanting independent
release cadence — and pays for it with documented version-drift and
publish-latency pain (e.g. a downstream consumer pinning a shared-library version
that later disappears upstream). Projects shaped like OpenPal — one team, every
platform — stay in one repo (Ferrostar, Espanso, Zed, Tauri itself). A repo split
buys nothing here and only adds version-pin bookkeeping.

### Concrete layout

```
Cargo.toml               # workspace, unchanged member list + one addition
crates/core/               # unchanged: business logic, GUI-free
crates/app/                 # unchanged Tauri shell — already carries the
                             #   #[cfg_attr(mobile, tauri::mobile_entry_point)]
                             #   stub and staticlib/cdylib/rlib crate-type,
                             #   unused today. This IS the mobile app crate.
crates/app/gen/apple/       # `cargo tauri ios init` output — COMMITTED
crates/app/gen/android/     #   (not gitignored): needs real hand-edits
                             #   (bundle ID, entitlements, signing config)
                             #   the CLI won't reproduce identically on
                             #   regen. Touch only via one sanctioned
                             #   regen script, to contain diff noise.
crates/engine-ffi/           # NEW workspace crate, optional dependency,
                             #   feature-gated: wraps llama-cpp-2 for the
                             #   in-process mobile backend. Isolated in
                             #   its OWN crate (not a file inside
                             #   crates/core) so NDK/Xcode bindgen never
                             #   leaks into a plain desktop
                             #   `cargo build`/rust-analyzer session.
bundler/                     # unchanged desktop scope; gains `ios`/
                             #   `android` subcommands (one release CLI
                             #   across all five platforms, not sibling
                             #   shell scripts)
ui/                          # unchanged single React package, single
                             #   Zustand store, single lib/api.ts seam.
                             #   Mobile layout = CSS/breakpoints inside
                             #   the SAME components. No ui-mobile/ fork.
plugins/                     # unchanged; mobile resolves an empty
                             #   plugin list via the existing `!portable`
                             #   gate (state.rs:602-605), extended to
                             #   also cover mobile.
```

### The one deliberate fork: engine transport

Everything else stays single-implementation. The one place that must genuinely
fork is how the engine is hosted:

- `trait EngineBackend` in `crates/core/src/engine/mod.rs`, with the boundary
  drawn at the **`ChatEvent`-stream level** (the same shape that already reaches
  the UI via a Tauri `Channel`) — not at "HTTP response," which is
  `SubprocessBackend`'s implementation detail only.
- `SubprocessBackend` (`crates/core/src/engine/subprocess.rs`) — today's logic
  moved verbatim, desktop-only, zero behavior change.
- `InProcessBackend` (in the new `crates/engine-ffi` crate) — wraps
  `llama-cpp-2`'s `LlamaModel`/`LlamaContext`/sampler directly; a "chat
  completion" becomes a loop pushing sampled tokens into the same event-channel
  type. No port, no HTTP client, no child process; model lifecycle is Rust
  ownership/`Drop`.
- `EngineManager` stays the single owner of one `EngineBackend` trait object per
  app instance, **on every platform** — this is also where mono-model-first
  keeps living, unforked, instead of needing to be re-derived per platform. Add a
  cheap runtime singleton guard here as insurance (an `InProcessBackend` is
  structurally easier to accidentally double-instantiate than a second
  subprocess would be).
- Scope `InProcessBackend` to only the sampler params / grammar-constrained
  decoding / per-model `extraArgs` (e.g. `--reasoning-budget 0`) that the
  **current model catalog actually uses** — don't chase full `llama-server` CLI
  parity speculatively. Treat closing the grammar/JSON-schema-constrained-
  decoding gap as a named, release-blocking item, not a punch-list afterthought:
  shipping without it means agent tool-calling silently degrades on mobile only,
  which is exactly the kind of platform-specific default-path complexity the
  beginner-first/mono-model-first rules in `AGENTS.md` exist to prevent.

This split is a genuine, irreducible "two places to edit" cost — every new
`llama-server` flag the catalog starts relying on needs a matching
`llama-cpp-2` call site, and nothing in the type system enforces that. Mitigate
with process (a checklist item on the "add a catalog `extraArgs`" PR template),
not more abstraction.

### What's cut, not ported, for mobile v1

All of the following are already Advanced/opt-in on desktop, so cutting them on
mobile is a labeled gap, not a default-path regression — but it should be
**visibly labeled** ("desktop only"), not silently absent:

| Feature | Desktop | Mobile v1 |
|---|---|---|
| Web search | Self-hosted SearXNG (no API key, more private) | Hosted search API over HTTPS (same Rust tool loop, different backend) — disclose the differing privacy story in the toggle copy |
| Speech (STT/TTS) | `faster-whisper` + Piper, Python sidecar | Dropped entirely (real in-process replacements — `whisper.cpp`, `sherpa-onnx` — exist but are their own project; revisit later behind Settings → Advanced) |
| Agent shell/code-exec tools | Rust-native, OS-sandboxed | Omitted from the mobile tool registry |
| Engine auto-update | `engine_update.rs`, independent of app releases | None — engine is compiled in; updates ship through normal app-store review |
| Request-triage autopilot, safety-banner risk detection | `EngineManager::classify`/`classify_risk` via the local engine's `raw_classify` | Works through the pairing: the gateway serves `/remote/v1/classify`, and `RemoteBackend::raw_classify` proxies the exact same body to it with the pinned-TLS bearer client, so a paired phone's triage and risk check run on the desktop's engine like its chat does. `Ok(None)` (both features' "nothing to suggest" answer) is still what an unpaired/stopped/errored round trip maps to. |

Keep a small doc/README table like the one above as the single place every
accepted mobile/desktop difference is listed, so it can be audited at a glance
rather than discovered piecemeal.

**Not cut: the Message improver.** Unlike request-triage/risk-detection
above, "Improve my message" (`docs/design/PROMPT_ENHANCER.md`) is built on
`chat_stream`, not `raw_classify` — the same backend-universal primitive
plain chat already uses — so it works unchanged through `RemoteBackend` on
a paired phone. No Android-specific gating exists in the composer for it
(unlike web search/agent tools, whose gaps are platform-inherent); the
button simply follows the composer's own `ready` state, same as Send.

---

## 4. CI / release strategy

No Bazel/Nx/Turborepo/Moon — that tooling is a *scale* tax (dozens of packages,
multi-hour builds); OpenPal has 4 crates and 1 npm package, well under where it
would pay for itself. Plain GitHub Actions matrix, extended:

1. **5 platforms from 3 runner OSes already in use**: `windows-latest` → Windows;
   `macos-latest` → macOS **and** iOS (Xcode needs a Mac runner regardless);
   `ubuntu-latest` → Linux **and** Android (NDK cross-compiles cleanly from
   Linux). No new CI infrastructure category.
2. **Path-filtered triggers** so a `ui/`-only change doesn't retrigger
   `bundler/`, and vice versa.
3. **Two cadences, one repo**: today's fast desktop loop (cargo check/clippy/
   test, ui vitest/playwright) stays on every PR, minutes not hours. Mobile CI
   (NDK/Xcode toolchain setup, `tauri android/ios build`) goes on a deliberately
   coarser trigger — PRs touching `crates/core`/`crates/app`/`ui/`, a nightly
   run, and a manual `workflow_dispatch` for active mobile work. A solo/small
   team cannot afford a 5-platform-green bar on every push, and that's a stated
   ceiling, not a gap to apologize for.
4. **One version, one tag, one release, five artifacts** — a single `vX.Y.Z` tag
   triggers all five build+sign+publish jobs from the same commit, avoiding the
   version-pin drift the polyrepo case studies documented. If a store review
   delays one platform, that's a store-side staged rollout, not a reason to give
   mobile its own version scheme.
5. **Packaging stays thin**: `bundler/`'s existing desktop scope (fetch-engine
   matrix, macOS codesign/notarize) is untouched; mobile packaging is `bundler
   ios`/`bundler android` subcommands wrapping Tauri's mobile CLI plus
   `xcodebuild -exportArchive` / `gradlew bundleRelease` + `apksigner`.
6. **Secrets**: add Apple App Store Connect API key + provisioning profile, and
   Android Play signing keystore + service-account JSON, to the existing secret
   store.
7. **Pre-release gate a solo team can actually run**: every tagged release goes
   to TestFlight and the Play Internal Testing track *before* public rollout —
   catches store-review-only failures (entitlements, provisioning) CI can't.
   Budget real calendar time for first-time and ongoing store review latency as a
   release-critical-path item.

---

## 5. Migration path

Phased so each step is independently reviewable and the expensive, hard-to-
reverse work (the engine port) only happens after cheaper validation clears.

**Phase 0 — foundation refactor** (ships value even if mobile is later
deprioritized): **DONE**, 2026-08-28. `trait EngineBackend` extracted,
`SubprocessBackend` implements it, zero desktop behavior change — verified via
the full test suite plus a real-model run of the ignored E2E test. Landed on
`main` as part of PR #67 (squash-merged alongside the unrelated Remote Access
feature, which was developed concurrently on top of these same commits).
Items 2 (`keyring` Android feature) and 3 (`sysinfo` field audit) were **not**
done as part of this — still open, folded into Phase 1 below since Phase 1's
actual build/run cycle is what will surface whether they're needed yet.
1. ~~Extract `trait EngineBackend`~~ — done, see above.
2. ~~Add an Android feature to the `keyring` dependency~~ — **done, but not as
   originally scoped**: no such feature exists. `keyring 3.6.3` itself falls
   back to an in-memory `mock` backend for Android (confirmed in its own
   source — Android isn't in its supported-platform list), which "succeeds"
   on every call but doesn't survive a process restart. This was a real,
   severe bug, not just a missing feature: it produced a guaranteed
   unrecoverable `SIGABRT` crash-loop on every second app launch (or, if the
   recovery-code ceremony had run, a forced recovery-code re-entry on
   *every* launch, forever). The `android-keyring` crate (the one candidate
   that actually targets `keyring 3.x`) was investigated and rejected — its
   own README says it "should not be deemed mature enough for production
   level or sensitive applications," and its GitHub repo has since been
   overwritten by a different, incompatible project. Real fix: a new
   `SecretStore` trait (`crates/core/src/crypto/secret_store.rs`) sits
   behind both `crypto::keychain` and `account.rs` (a second, previously
   un-abstracted `keyring` consumer, for the account refresh token — fixed
   in the same pass); desktop/iOS resolve to `KeyringSecretStore` (unchanged
   behavior); Android resolves to a Tauri mobile-plugin bridge
   (`crates/app/src/secret_store_android.rs` +
   `gen/android/.../SecretStorePlugin.kt`) over Android Keystore-backed
   `EncryptedSharedPreferences`. Verified live on the emulator: mint a
   master key, force-stop, relaunch — repeatedly — with no crash and no
   recovery prompt, where every relaunch previously crashed.
3. Audit `sysinfo` field usage in `hardware.rs` against Android/iOS coverage —
   still open. `hardware.rs` did successfully report `ram_gb=2` on the emulator
   in Phase 1 testing (real signal, not a crash), but `chip` came back
   `Unknown` — expected (no Apple-Silicon-style chip ID path on Android) and
   not yet audited for whether other fields silently return bogus values
   instead of `Unknown`/`None`.

**Phase 1 — prove the mobile shell boots** (cheap validation before the
expensive rewrite): **IN PROGRESS**, Android track only (iOS blocked on Xcode —
needs the user's Apple ID, can't be installed headlessly). Environment is fully
set up: Android SDK/NDK 27.2.12479018/emulator installed via Homebrew's
`android-commandlinetools` cask (headless, no Android Studio GUI), an AVD
(`openpal_test`, Pixel 6, API 35 arm64) running, all 4 Android Rust targets
added.

4. `cargo tauri android init` — **done**, `crates/app/gen/android/` scaffolded
   (currently uncommitted in the working tree, not yet folded into a commit).
5. Get a debug build of the UI booting, and confirm it actually renders —
   **done**, four real bugs found and fixed along the way:
   - `rfd` (native dialogs) has no Android/iOS backend at all — a hard compile
     error, not a runtime gap. **Fixed**: `rfd` moved to a desktop-only target
     dependency (`crates/app/Cargo.toml`), `fatal_startup_error` (`lib.rs`)
     split into a desktop arm (unchanged) and a mobile arm (logs + exits, no
     dialog).
   - `tauri-plugin-dialog`'s `pick_folder` doesn't exist on mobile (same
     class of gap). **Fixed**: `import_skill_folder` (`commands.rs`) split
     into a desktop arm (unchanged) and a mobile arm returning a friendly
     "not available on mobile yet" error — same signature, so the frontend
     and command-registration list need zero changes either way.
   - The APK built, installed, and launched, then crashed immediately:
     `dlopen failed: cannot locate symbol "__cxa_pure_virtual"`. Root cause
     (confirmed via `llvm-nm -u` across every staged `.a`, not guessed):
     `cpal`'s Android backend links Google's Oboe C++ library (`oboe-sys`),
     which the NDK's linker leaves with an unresolved C++ ABI symbol unless
     the final `.so` explicitly pulls in `libc++`. Rather than fight that
     link (tried `-lc++_static` via `.cargo/config.toml` target `rustflags`
     first — had no effect, apparently silently overridden by a `RUSTFLAGS`
     env var Tauri's own Android build sets), the actual fix was to **not
     link cpal/rodio/hound on mobile at all**: Voice (STT/TTS) is already
     cut from mobile v1 per the table above, so there's no functionality
     lost. `cpal`/`rodio`/`hound` moved to the same desktop-only target
     dependency table as `rfd`; `crates/app/src/audio.rs` (desktop, cpal/
     rodio, unchanged) now sits behind `#[cfg(not(any(target_os =
     "android", target_os = "ios")))]`, with a new `audio_mobile.rs` (same
     `AudioPlayer`/`record_wav` public API, inert bodies) behind the mobile
     `cfg`, selected via `#[path]` in `lib.rs`'s `mod audio;` — `state.rs`
     and `commands.rs` need zero changes.
   - ~~Currently blocking~~ — **fixed**: past the native-linking crash, the
     app got as far as the WebView's first IPC/asset request (`hardware
     detected chip=Unknown ram_gb=2` logged successfully) before a **Rust
     panic**: `No rustls crypto provider is configured` (from
     `reqwest-0.13.4`'s `async_impl/client.rs`). Root cause, confirmed by
     reading `reqwest` 0.13's actual source rather than guessing: it's
     `tauri-plugin-updater`'s `reqwest` (pulled in by the engine
     auto-update feature, not — as first suspected — Remote Access's
     `axum`/`mdns-sd`/`rcgen` stack), which has no per-client fallback
     provider unlike `openpal-core`'s `reqwest 0.12` (which silently falls
     back to `ring`). Triggered by the frontend's unconditional 5-second
     post-boot update check, on **every platform** — desktop likely hits
     the identical panic but survives silently (it unwinds inside a tokio
     task, and the auto-check's own comment says it "never surfaces an
     error of its own"); Android is fatal because the unwind crosses the
     JNI boundary and aborts the process. Fixed: install the `ring`
     provider (the backend already used elsewhere, e.g.
     `crates/core/src/remote/gateway.rs`) once, as early as possible in
     `crates/app/src/lib.rs`'s `run()`, before `tauri::Builder::default()`.
   - The WebView **confirmed actually rendering**, not just alive: walked
     through onboarding live on the emulator (recovery-code screen →
     welcome → hardware scan → model match → download), correct branding
     and styling on every screen, real interactivity confirmed (Copy
     button, a checkbox, model download progressing over real network).
     This is Phase 1 item 5's actual completion bar, now reached.
6. Minimal responsive-layout pass — **done**. Breakpoint: a single, literal
   `@media (max-width: 600px)` repeated per-file (documented in
   `global.css`'s `:root` — no PostCSS/Sass in this build, so a custom
   property can't be the media condition itself). Sidebar → an off-canvas
   drawer (`ui/src/state/store.ts`'s `mobileSidebarOpen`, mirroring the
   existing `settingsOpen` pattern; hamburger button in the titlebar; scrim;
   auto-closes on every navigation action). Hardware-scan stat cards
   collapse from a fixed 4-column grid to 2×2. `SettingsSheet.tsx` (2,200
   lines) turned out not to need its own rework — it's a single flat
   scrolling column, not tabs — so the shared `.modal` width fix (all three
   dialogs use it) was enough; a real constraint shaped that fix: none of
   `SettingsSheet`/`UpdateDialog`/`SignInDialog` has a close/X button, so
   the margin was narrowed rather than going edge-to-edge, which would have
   deleted the only dismiss affordance. Safe-area insets
   (`viewport-fit=cover`, a `--titlebar-safe-h` token) cover the titlebar,
   sidebar, every main-pane view, the modal backdrop, and the PIN pad —
   verified as a no-op on desktop and on the `openpal_test` AVD (no
   physical notch, so `env(safe-area-inset-*)` is 0px there); real
   non-zero insets aren't exercisable on this emulator, so a real notched
   device is a follow-up check, not done here. Also fixed along the way:
   `.engine-banner-global` (`AutoUnloadBanner`) was hardcoded `left: 272px`
   to clear the desktop sidebar's grid column, which would have squeezed it
   into a sliver once the sidebar stopped occupying layout width on mobile.
   (That banner has since been replaced by `AutoUnloadTooltip` /
   `.auto-unload-tooltip`, a fixed-size popover above the sidebar's model
   row; neither the rule nor its mobile override exists any more.)
   **Deliberately not touched, and out of scope for this pass**: the
   "your Mac" hardcoded-copy issue turned out to be much bigger than
   originally scoped here — 16 strings across `en.json`/`fr.json` in
   lockstep (one with a non-breaking-space gotcha a literal find-replace
   would silently miss), plus 2 backend-hardcoded Rust strings
   (`crates/core/src/custom.rs:231`,
   `crates/core/src/remote/wol_status.rs` lines 53/59/73) that aren't even
   i18n-driven — fixing it needs a new OS/platform field threaded from a
   Tauri command through i18next interpolation. Real, but a separate,
   cross-platform (not mobile-specific) follow-up.
7. **Go/no-go checkpoint** — every item above is now done, so the technical
   bar this phase set is cleared: rendering confirmed, the keychain
   crash-loop fixed, the responsive-layout pass done and verified live on
   the emulator. Recommend **go** for Phase 2, with two gaps worth knowing
   about rather than silently carrying forward: (a) the keychain fix's
   `account.rs` half (the refresh-token store, same `SecretStore`
   mechanism as the master key) is unverified against a real sign-in —
   confirmed by direct DOM/store inspection that the *mechanism* works
   generically, but a live sign-in needs real backend credentials this
   session didn't have and shouldn't create against production
   unilaterally; (b) "your Mac" (item 6, above) ships inaccurate on
   Android/iOS until its own follow-up lands. Neither blocks Phase 2
   starting, but both are worth a conscious "ship anyway" call rather than
   discovering them later.

### Current status & immediate next steps (as of 2026-08-29)

1. ~~Fix the rustls crypto-provider panic~~ — done (item 5 above).
2. ~~Confirm the WebView actually renders~~ — done (item 5 above).
3. ~~Fix the `keyring` Android crash-loop~~ — done (item 2 above), found
   during rendering verification: a real, severe, previously-undocumented
   bug distinct from (and unrelated to) the rustls panic.
4. ~~Responsive-layout pass~~ — done (item 6 above).
5. **Go/no-go recommendation: go** (item 7 above) — bring to the user for
   the actual call, alongside the two known gaps it names.
6. ~~Worth an explicit decision, not an assumption~~ — resolved: brought to
   the user (Remote Access changes the priority order, per this item's own
   framing), who chose **RemoteBackend first**, specifically to avoid
   `InProcessBackend`'s unproven Tauri-mobile + `llama-cpp-2` FFI combination
   (item 8's own week-one-spike risk, below) and get a working, useful mobile
   app in front of people sooner. `InProcessBackend` isn't cancelled — it's
   resequenced after RemoteBackend, not before it.
6a. ~~RemoteBackend — Android~~ — **done**. A fourth `EngineBackend` impl
    (`crates/core/src/remote/client/`) makes a paired desktop's engine usable
    from a phone over the already-shipped Remote Access gateway: TLS
    fingerprint pinning (trust-on-first-use at pairing, pinned after — a
    real, disclosed weakening vs. the desktop's QR flow, see
    `docs/REMOTE_INFERENCE.md`'s "mobile client" section), mDNS
    discovery + a manual-address fallback, pairing persistence
    (`SecretStore`-backed bearer token, mirroring `AccountManager`'s own
    precedent), and a Settings → Remote pairing UI. Verified live end-to-end
    on the `openpal_test` AVD against a real gateway bound to `0.0.0.0`,
    reached via the emulator's `10.0.2.2` host alias — pair, disconnect,
    re-pair, and the gateway going away all correctly reflected in the UI;
    found and fixed one real bug this way (a paired-but-idle desktop showed
    desktop-only "No model installed"/Library-navigation copy on Android,
    where neither makes sense). Not yet tested: a real, physical Android
    phone (only the emulator so far — real notch insets and real Wi-Fi
    multicast behavior are still unconfirmed), and pairing against a desktop
    with an actual model loaded (only the gateway's own `503`
    "engine isn't running" fallback path was exercised, since the test
    harness had no model loaded). `raw_classify` is wired but unexercised on a
    real device too; agent-mode/web-search stay unwired on this backend — see
    the cut-features table above and `docs/REMOTE_INFERENCE.md`'s own scope
    notes.

**Phase 2 — the engine port** (highest-risk/highest-value piece, isolated;
now the *second* mobile-engine move, after RemoteBackend above):
8. **Week-one spike, before anything else in this phase**: a bare, non-Tauri
   Rust binary linking `llama-cpp-2`, run on a real Android phone and a real
   iPhone, streaming tokens from one known-good GGUF. No one has shipped *Tauri
   mobile + llama-cpp-2* together yet (unlike `llama.rn`, proven in three
   shipping apps) — this is the single biggest unproven assumption in the whole
   plan, and it's far cheaper to falsify in week one than after the phase's full
   investment.
9. Build out `crates/engine-ffi`'s `InProcessBackend` behind a feature flag;
   Android first (friendlier NDK/Linux-runner toolchain), then iOS (expect the
   documented Metal-disabled-on-simulator caveat — device-only GPU accel).
10. Wire `InProcessBackend` into `EngineManager` for real streaming chat;
    implement only the sampler/grammar/`extraArgs` call sites the catalog
    actually uses.
11. Replace `gpu.rs`/`hardware.rs` subprocess device probes with in-process
    equivalents for mobile targets.

**Phase 3 — plugins and remaining platform-specific call sites — done
(Android; iOS untouched throughout, same scope as every phase so far):**
12. ~~Extend the plugin-dir gate to also return an empty list on mobile~~ —
    done. `state.rs`'s `resolve_plugin_dirs` skips the built-in dirs on
    Android the same way it already skips them in portable mode: none of
    the 4 built-in plugins do anything there (`speech`/`web-search` need a
    Python this platform doesn't have; `hf-browser`/`smart-model-router` are pure
    Rust but both about managing locally installed models, which don't
    exist on a phone chatting through `RemoteBackend`). Sidebar's Plugins
    nav item is fully hidden, matching portable mode's own treatment — not
    "labeled but empty," since there's no way to ever have anything to
    show. The "Browse" nav item (gated on hf-browser being enabled)
    disappears for free as a consequence.
13. ~~Hosted-search-API path for mobile web search~~ — **deferred by
    explicit user decision**, not built. Wiring one up means committing to
    a specific paid third-party vendor and an ongoing cost through
    OpenPal's own backend (`api.openpal.app`) — not a call to make
    unilaterally from this repo. Asked the user; they chose to label the
    gap clearly instead (matching this doc's own cut-features rule:
    "visibly labeled... not silently absent") over preparing an inert seam
    or picking a vendor now. The composer's web-search toggle is disabled
    on Android with its own title explaining why, rather than being
    clickable and silently failing.
14. ~~Omit `ShellExecTool`/`CodeExecTool` from the mobile agent tool
    registry~~ — turned out to be moot once actually checked: agent mode
    was **already unreachable** on Android before this phase.
    `agent_chat_turn` calls `state.engine.endpoint().await` before ever
    building the tool registry those two live in, and
    `RemoteBackend::endpoint()` always returns `None` by design — so the
    registry those tools sit in is never constructed there at all. The
    real, live gap was that Settings' "Agent tools" toggle and the
    composer's tools button were still fully interactive, and using them
    hit a "Your AI isn't ready yet. Pick a model, then try again." error —
    accurate for desktop, wrong on a phone with no model to pick. Fixed
    with the same labeled-disabled treatment as item 13, on both the
    composer button and the Settings toggle.
15. ~~Swap remaining desktop-only call sites~~ — done, and one of them was
    a **real live bug, not just an unlabeled gap**: `rfd`'s crash dialog
    was already correctly `#[cfg]`-split before this phase (nothing to
    do). But `open_link`'s `os_open` helper had three `#[cfg]` arms for
    macOS/Windows/`all(unix, not(macos))` — and Android satisfies
    `cfg(unix)` in Rust's own target definition, so every external link
    (Hugging Face model cards, "Learn more" copy) was silently spawning a
    nonexistent `xdg-open` binary and failing on every tap. Fixed for
    real: a new `OpenerPlugin.kt` (mirrors `SecretStorePlugin.kt`/
    `WifiLockPlugin.kt`'s shape) calls Android's actual mechanism,
    `Intent.ACTION_VIEW`, bridged via a new `crate::opener` module
    (`#[path]`-swapped like `secret_store`/`wifi_lock`). Verified live:
    tapping a link on the emulator now actually opens it. Also folded in:
    `open_models_folder` (no Android equivalent — there's no local
    model-management concept on a `RemoteBackend` phone at all; reworking
    Library's own Android UX, which still points at this dead end, remains
    its own separate, not-yet-scheduled follow-up) now fails with a clear
    message instead of the same silent
    `xdg-open` failure; `check_for_update`/`install_update` return a
    "Updates for this app come through the Play Store" message on Android
    before ever reaching `tauri-plugin-updater` (which compiles and
    registers fine there, but Play Store policy doesn't want a
    self-updating APK) — confirmed safe for the un-prompted 5-second
    post-boot auto-check, which already swallows any error silently.

**Phase 4 — packaging, CI, first store submission:**
16. Add `bundler ios`/`bundler android` subcommands.
17. Extend CI with the macOS/Linux-runner mobile jobs on the coarse trigger from
    §4; get an unsigned debug artifact building in CI first.
18. Wire signing secrets; get TestFlight/Play Internal Testing uploads working
    from a manual CI trigger.
19. Run a real beta period through TestFlight/Internal Testing by hand at least
    once before folding mobile into the automated release-tag workflow.

**Documented fallback, not a vague gesture**: if Phase 1's or Phase 2's
go/no-go fails, the concrete escape hatch is a standalone React Native +
`llama.rn` app — a real, already-proven mechanism (PocketPal AI, ChatterUI,
Maid all ship on it), just one that costs steady-state code duplication across
two codebases forever (0% UI reuse, ~0–10% core reuse — hardware/selection/
download/settings/crypto logic all re-derived in TypeScript). That's why it
lost the strategy review (5.5–6/10 vs. 8.5/10) as a *first* move rather than a
contingency — keep it as the named Plan B, not the default.

---

## 6. Risks to hold onto

1. **Tauri mobile maturity** is a real, not-fully-retired risk as of mid-2026.
   Phase 1 bounds it to weeks. If it turns out to be a genuine blocker, the
   fallback is a *single-platform* native UI later (e.g. SwiftUI for iOS only),
   never a wholesale native-UI pivot — that reintroduces the N-UI burden this
   whole strategy exists to avoid.
2. **Engine backend split is a permanent two-places-to-edit cost.** Mitigate
   with a PR-template checklist item, not more abstraction.
3. **Mono-model-first is slightly easier to violate by accident** under
   `InProcessBackend` — a second in-process `LlamaContext` is just another
   struct, unlike a second subprocess needing a second port. `EngineManager`
   staying the single enforcement point protects this by design; add a runtime
   singleton assertion as insurance.
4. **No engine auto-update on mobile** — inherent to "no runtime-exec," not a
   choosable design point. Name it as a real UX regression vs. desktop.
5. **Hosted search API is the first user-facing default-path platform
   difference** — forced by sandboxing, not preferred. Keep it behind the
   existing Settings → Advanced gate and disclose the differing privacy story
   in-app.
6. **CI/release honesty**: a 1–3 person team cannot manually verify iOS+Android
   the way it verifies desktop today. TestFlight/Play Internal Testing as a
   mandatory gate adds real lead time per release; store review latency is a
   release-critical-path cost, not a rounding error.
7. **Repo topology doesn't reduce platform-specific *knowledge* burden.**
   Xcode signing/provisioning, Play Console policy, and NDK toolchains are all
   genuinely new surface for whoever becomes "the mobile person," independent of
   how clean the code sharing is. Budget real one-time ramp-up time.
8. **Binary size and CI build time both grow for real** once llama.cpp is
   linked in-process on mobile — comparable published cases cite tens of MB for
   the static lib, and NDK+Xcode CI runs are slower than the current
   three-desktop-OS matrix. This is the concrete reason mobile CI is
   deliberately not run on every commit (§4).
9. **Treat every go/no-go checkpoint as real.** If mobile usage ends up small
   relative to desktop, or a checkpoint surfaces an unacceptable experience, the
   honest move for a small team is to stop before the next phase's investment,
   not to sunk-cost through it.
10. **"Web search on mobile just needs its search backend swapped" understates
    the real dependency.** Verified directly in the code during Phase 0
    planning: `EngineManager::endpoint()` hands out raw HTTP connection details
    (`EngineEndpoint { base_url, api_key, .. }`) that
    `crates/core/src/agent/orchestrator.rs`'s tool-calling loop uses to build
    its own OpenAI-format requests and hand-parse SSE for `delta.tool_calls` —
    bypassing `chat_stream`'s `ChatEvent` boundary entirely. **Both agent mode
    and web search turns go through this** (`commands.rs`'s `agent_chat_turn`
    and `web_search_chat_turn`). So it isn't only the SearXNG-vs-hosted-API
    swap in §3 that mobile needs — the orchestrator's LLM-calling mechanism
    itself has no answer yet for a backend with no HTTP surface, and closing
    that gap (an HTTP-shaped facade on `InProcessBackend`, or refactoring the
    orchestrator to a backend-generic tool-calling primitive) is real,
    non-incidental work that belongs in Phase 2, alongside the
    grammar/JSON-schema-constrained-decoding gap it's already named next to.
    `EngineBackend`'s `endpoint()` method defaults to `None` specifically so
    this stays an open, callable-later seam rather than a blocker for Phase 0.
