# OpenPal: maintaining a Full and a Portable build

This document is the plan for shipping **two products from one codebase**:

1. **Full** — the current installed, signed desktop app with every feature
   (catalog, custom models, multimodal, plugins, voice, diagnostics).
2. **Portable** — a *no-install* build for **Windows, Linux, and macOS** that runs
   from a folder/zip (or an AppImage), with an **optional model** acquired through
   the in-app/website download flow. It maximizes features but deliberately drops
   anything that can't be made truly portable and lightweight.

It complements [`ARCHITECTURE.md`](../../ARCHITECTURE.md); read that first for how the
app is structured (Tauri shell → `openpal-core` → `llama-server` subprocess + Python
plugin sidecars).

---

## 1. The core decision

**One source tree, one git history, one binary. Differentiate at *runtime* (a
portable-mode marker) and at *packaging* (a separate bundler target) — not at
compile time.**

A four-way design review (compile-time Cargo features **A**; runtime marker-driven
mode **B**; packaging-only split **C**; hybrid **D**) landed on:

> **Adopt B (behavior) + C (packaging). Defer A.**

Why: the expensive part — cross-OS packaging (WebView2 runtime, AppImage/glibc,
signing, per-OS CI) — is *identical for all four approaches*, so the only real
differentiator is the toggle mechanism, and the runtime marker is the lowest-drift
one. The signed Full mac product stays **byte-identical** because the base
`tauri.conf.json` is never touched; portable is purely additive.

Pull **A** (compiling out the plugin/audio surface) in *later*, only if a binary-size
budget or a Chromium-CVE security audit makes it worth the doubled build matrix.

### Locked decisions

| Decision | Choice |
|---|---|
| **Model** | No hardcoded embed. Base = **Portable Lite** (0 weights); the "with model" experience is an **assisted, hardware-aware download** (reuses `selection.rs::recommend`). An optional pre-staged model (`bundler portable --model <id>`) exists for airgapped distribution only. |
| **GPU** | **Vulkan superset** build on Win/Linux (GPU when present, CPU fallback baked in) + a **force-CPU toggle** for buggy drivers. Metal on macOS. |
| **Arch** | **Full coverage**: macOS arm64 (M1–M5) + Intel x64; Windows x64 + ARM64; Linux x64 + arm64. |
| **macOS signing** | **Ad-hoc only** (`signingIdentity = "-"`), inside-out over the engine dylibs; **skip notarization** for portable. The Full product keeps real Developer ID + notarization. |

---

## 2. What "portable" costs per OS

The **webview runtime is the dominant cost**, not our code.

### Windows — a ZIP, not an installer
- `tauri build --no-bundle` → bare `.exe`; hand-assemble a flat folder + zip. (NSIS
  `currentUser` is **not** portable — it still writes `%LOCALAPPDATA%`, registry,
  shortcuts.)
- Ship the **fixed-version WebView2 runtime** (~250 MB extracted / ~150–180 MB
  zipped — the single biggest line item) next to the exe; point at it via
  `WEBVIEW2_BROWSER_EXECUTABLE_FOLDER`. Requires **Tauri ≥ 2.8.3** (we are on 2.11.2 ✓).
- **Windows 10 + WebView2 ≥ 120**: the renderer runs in an AppContainer, so an
  unpackaged app must run two `icacls` ACL grants (`ALL APPLICATION PACKAGES` /
  `ALL RESTRICTED APPLICATION PACKAGES`) on the runtime folder **on first launch** or
  the webview silently fails. No admin needed (the user owns the extracted folder).
  Win11 is exempt.
- The fixed runtime **cannot run from a UNC/network path** → run from local disk/USB.
- `crt-static` the exe and drop `vcruntime140.dll`, `vcruntime140_1.dll`,
  `msvcp140.dll` next to `llama-server.exe` (the VC++ redist is not guaranteed on a
  clean Windows; the engine links it).

### Linux — AppImage is the only realistic double-click format
- Tauri renders through **system WebKitGTK** (a large, `dlopen`-heavy multi-process
  native tree) — there is no bare-folder portability. AppImage (a built-in Tauri
  bundle target) bundles WebKitGTK via `linuxdeploy-plugin-gtk`. (An extracted AppDir
  *is* a portable single folder if you ever need one.)
- **Build on Ubuntu 22.04** (glibc ~2.35). Forward-only: it won't run on Ubuntu 20.04
  / RHEL 8 / Debian 11. The official `llama.cpp` linux-x64 prebuilt is *also* built on
  ubuntu-22.04, so both share the same floor.
- **FUSE caveat**: Ubuntu 24.04 / Debian 13 dropped `libfuse2` → document
  `apt install libfuse2t64` or `--appimage-extract-and-run`.
- Bundle `libasound` (ALSA) if native audio is kept. The linux-**arm64** prebuilt has
  a **higher** floor (built on ubuntu-24.04-arm, glibc ~2.39).

### macOS — already portable
- The `.app` is already drag-to-run; the `.dmg` is cosmetic. **The same `.app` is the
  portable build.**
- **Must at least ad-hoc sign** (`signingIdentity = "-"`) — Apple Silicon SIGKILLs an
  unsigned binary (and all ~35 engine dylibs). Non-notarized downloads hit a Gatekeeper
  "Open Anyway" + admin-password step (Sequoia/Tahoe removed the right-click→Open
  shortcut); ship `xattr -dr com.apple.quarantine OpenPal.app` instructions.
- **Per-arch zips** (arm64, x64). A universal2 shell with an arm64-only engine is
  broken on Intel — match each engine to its arch instead.

---

## 3. Engine & model strategy

**Engine — Vulkan superset on Win/Linux, Metal on mac.** The Vulkan archive *is* the
CPU archive plus one `ggml-vulkan` lib; backends load dynamically → GPU when present,
CPU fallback otherwise, vendor-neutral (NVIDIA/AMD/Intel). CUDA/ROCm/SYCL are
vendor-locked and 115–650 MB — leave them to the Full product. **Ship a force-CPU
toggle**: buggy iGPU/old-GPU Vulkan drivers can *crash* rather than fall back. Pin
**one** llama.cpp `b`-tag across all OSes.

Staged engine sizes (≈b9833): mac arm64 ~26 MB · win Vulkan ~92 MB (CPU-only ~44 MB) ·
linux Vulkan ~88 MB (CPU-only ~38 MB).

**Model — Lite first, assisted download.**
- **Portable Lite** ships zero weights. On first run, onboarding calls
  `get_system_report → recommend()` (already implemented) to highlight the best-fit
  model for the machine and downloads it via `download.rs`.
- The **download website** should detect OS/arch (route to the right artifact) and may
  ask the user's RAM to recommend a model tier. `selection.rs` is Rust, so a static
  site needs either a small recommend endpoint or a JS port of the fit rule
  (`weights × 1.10 + KV-cache + 700 MB` vs 70 % of RAM).
- **Optional pre-staged model** (`bundler portable --model <catalog-id>`) is for
  offline/airgapped distribution only. The weights land in a **read-only `models/`
  folder beside the executable** (sibling of the marker); the app's
  `bundled_models_dir` searches it in addition to the writable `data/models`, so the
  model is **Installed** and runnable even when the medium is read-only. The bundler
  **license-gates** the choice: Apache-2.0/MIT embed freely; **Llama 3.2 / Gemma 3
  require `--accept-license`** (their licenses force ship-the-agreement / "Built with
  Llama" pass-through), and the generated `NOTICE` records the obligation. The
  catalog's smallest permissive pick today is Qwen3-4B Q4_K_M (~2.5 GB); add a
  smaller Apache-2.0 entry (e.g. Qwen3-1.7B ~1.19 GB) to the catalog to embed it.

---

## 4. Feature triage for the portable build

| Feature | Portable | Why |
|---|---|---|
| Chat core (engine subprocess + streaming) | **Keep** | The irreducible product. |
| Catalog + selection + resumable downloads | **Keep (degraded)** | Pure Rust over `reqwest`/`rustls`, not the webview → works everywhere. *Selection* mis-ranks off-mac (RAM-only model, ignores discrete VRAM; copy is Mac-flavored). |
| Custom models (HF GGUF + local `.gguf`) | **Keep** | Pure Rust, ~0 MB; *more* valuable in portable. |
| Documents (PDF via `pdf-extract`, text) | **Keep** | Pure Rust, cross-compiles; works with any model. |
| Vision images (mmproj) | **Keep but dormant** | No small vision model exists (only Gemma-3-4B = 2.5 GB + 851 MB mmproj, Gemma-licensed). Data-driven → the picker auto-hides with a text-only model; lights up if the user later downloads a vision model. |
| **Web-search plugin** | **Cut** | Needs host `python3` + first-run `pip` + SearXNG source + optional torch (GBs). Not portable. |
| **Voice plugin** | **Not shipped built-in** | Same Python problem. Native `cpal/rodio/hound` audio stays compiled; a power user with host Python can drop a plugin into `data/plugins/`. |
| Plugin framework (`PluginManager`) | **Keep compiled, no built-ins** | Pure Rust, ~0 MB; ship no plugin folders and hide the Plugins tab. |
| Logging + diagnostics viewer | **Keep** | *Most* valuable here — no installer, no console. |
| Embedded model | **Lite + optional pre-staged** | See §3. |

---

## 5. Code seams

The codebase is well-positioned — the runtime changes are small and centralized.

| # | Seam | Change | Phase |
|---|---|---|---|
| 1 | `crates/app/src/state.rs` `app_data_dir()` (the single data source) | `resolve_data_dir(app)`: if a `openpal-portable` marker / `data/` dir sits next to `std::env::current_exe()` → `<exe>/data`, else `app_data_dir()`. Use `current_exe()` (cwd is `/` on mac double-click); on mac the folder must be a **sibling of the `.app`**, never inside `Contents/` (breaks signing); add a **writability fallback** for read-only locations. | 1 |
| 2 | `state.rs` `resolve_engine_binary` | Resolve `llama-server.exe` on Windows. | **0 ✓** |
| 3 | `state.rs` `resolve_plugin_dirs` | Return user-only (no built-ins) in portable → `PluginManager` finds nothing → web-search globe + voice mic auto-hide. | 1 |
| 4 | new `get_app_info` command | Return `{ portable, data_dir }` so the Sidebar hides the Plugins tab and Settings shows the resolved data path. | 1 |
| 5 | `crates/core/src/hardware.rs` | Already degrades gracefully cross-platform. VRAM/Vulkan-aware ranking + de-Mac-ified copy is a quality follow-up. | later |
| 6 | `crates/app/src/commands.rs` openers | Real `explorer` (Win) / `xdg-open` (Linux) arms for `open_models_folder`/`open_logs_folder`/`open_link`. | **0 ✓** |
| 7 | `bundler/src/{fetch,paths,main}.rs` | Stage `.dll`/`.so` + `llama-server(.exe)`; cfg-gate the Unix-only chmod; extend `host_can_run`; add a `portable --target` subcommand (+ WebView2 staging, icacls, AppImage). | **0 (staging) ✓** / 2 (packaging) |
| 8 | `crates/app/tauri.portable.conf.json` | Tauri config overlay merged via `--config`; base config untouched. | 2 |

---

## 6. SKU, size & build matrix

**SKUs:** Full (installed) · Portable Lite (no model) · Portable + Model (assisted
download, or optional pre-staged).

| SKU | macOS arm64 | Windows x64 | Linux x64 |
|---|---|---|---|
| Portable Lite | ~70–90 MB | ~190–220 MB zip | ~120–200 MB AppImage |
| + Qwen3-1.7B (if pre-staged) | ~1.3 GB | +~1.2 GB | +~1.2 GB |

**Engine target matrix (full arch coverage):**

| Target | llama.cpp asset | Webview | Notes |
|---|---|---|---|
| macOS arm64 (M1–M5) | `macos-arm64` (Metal) | WKWebView | Base case (already staged). |
| macOS x64 (Intel) | `macos-x64` (Metal) | WKWebView | Cross-build `--target x86_64-apple-darwin`. |
| Windows x64 | `win-vulkan-x64` + MSVC redist DLLs | WebView2 fixed (x64) | `crt-static`. |
| Windows ARM64 | `win-cpu-arm64` (**no Vulkan-arm64 build → CPU-only**) | WebView2 fixed (arm64) | Cross-compile `aarch64-pc-windows-msvc`. |
| Linux x64 | `ubuntu-vulkan-x64` | WebKitGTK (AppImage) | Build on Ubuntu 22.04 (glibc ~2.35). |
| Linux arm64 | `ubuntu-vulkan-arm64` | WebKitGTK (AppImage) | **Higher glibc floor ~2.39**. |

Full arch coverage roughly **doubles the CI matrix** — all in the bundler/CI, not the
app code. **Windows-ARM has no GPU**; **Linux-arm64 can't share the x64 glibc floor** —
documented limits, not parity targets.

---

## 7. Phased roadmap

- **Phase 0 — Cross-platform groundwork** ✅ *(benefits Full too)*: engine
  `.exe`/`.so`/`.dll` resolution + bundler staging that compiles & stages on
  Win/Linux; real cross-platform folder/link openers.
- **Phase 1 — Portable runtime mode (B)** ✅: `resolve_data_dir` marker (or the
  `OPENPAL_PORTABLE` env var, for dev testing — `npm run dev:onboarding` runs `tauri dev`
  this way, so its data lands in `target/debug/data`) + writability
  fallback; starve plugin discovery; `get_app_info` + UI gating; force-CPU engine
  toggle.
- **Phase 2 — Packaging (C)** ✅ *(macOS built & verified; Windows/Linux code-complete,
  pending CI runners)*: `bundler portable --target`; WebView2 fixed-runtime
  staging + env var + Win10 icacls; AppImage on ubuntu-22.04; ad-hoc-sign/zip mac
  `.app`; `tauri.portable.conf.json`.
- **Phase 3 — Optional pre-staged model** ✅: the app resolves a read-only
  `models/` folder beside the executable (`bundled_models_dir`) so a pre-staged
  GGUF shows **Installed** and runs even on read-only media; `bundler portable
  --model <catalog-id>` (and a standalone `fetch-model`) download + SHA-256-verify
  the weights into the bundle, behind a license gate (`--accept-license` required
  for Llama/Gemma pass-through licenses); `LICENSE` + a generated `NOTICE`
  (llama.cpp, WebView2, model attribution) ship in every bundle.
- **Phase 4 — CI/release** ✅ *(scaffold; release runs validate the matrix)*:
  `.github/workflows/portable.yml` builds on native runners (macOS arm64
  `macos-14` · macOS x64 `macos-13` · Windows x64 `windows-latest` + `crt-static`
  + optional fixed WebView2 · Ubuntu 22.04 x64 · Ubuntu 24.04 arm64) and uploads
  each artifact; the engine smoke-test runs in-fetch and the AppImage is
  extract-verified. Deeper smoke (24.04 FUSE-removed host, GPU-less Vulkan
  fallback) and Windows-ARM cross-compile remain manual follow-ups.

---

## 8. Maintenance & risks

- **One binary, no fork.** CI builds every SKU per commit so the portable path can't
  rot. Pin one `llama.cpp` `b`-tag across all OSes; re-validate on bump (asset naming
  has changed historically — e.g. avx2/noavx merged into one CPU build).
- **WebView2 security**: the fixed runtime gets no auto-updates — re-ship it to patch
  Chromium CVEs. (Portable renders no web-search content, which eases this.)
- **The Full mac product is unaffected**: its signed/notarized `.dmg` path is the
  untouched base `tauri.conf.json`.
