# Disk offload — running models larger than RAM

Plan produced 2026-07-07 (multi-agent research workflow: 3 web-research agents +
1 codebase-seam agent, each adversarially verified; key llama.cpp claims checked
against master `c198af4` and against the **bundled b9833 binary's `--help`**).

## Verdict

**Feasible with stock llama.cpp, but only for MoE models, and on macOS only via
the CPU backend.** The mechanism is llama.cpp's default `mmap` load path: weights
are read-only file-backed pages, so under memory pressure the OS *discards and
re-reads* them — no swap writes, no SSD wear. Dense models beyond RAM are
physically hopeless (~0.07–0.7 tok/s: every token touches every weight, so
tok/s ≈ SSD bandwidth ÷ model size). MoE models touch only active experts per
token, which lands in "patient assistant" territory (~1–15 tok/s depending on
how far the model exceeds RAM, disk speed, and expert-routing locality).

This is a power-user feature in a beginner product: opt-in toggle, honest copy,
conservative guardrails. LM Studio warns + "Load anyway"; Ollama hard-refuses;
GPT4All silently swaps. OpenPal today is Ollama-style (TooBig disables
Download). The plan moves us to LM-Studio-style informed consent, but scoped to
the only case that actually works (MoE).

## Verified technical foundation (July 2026)

Sources: llama.cpp master c198af4, PRs #11397/#15077/#18166/#24009/#11427/#6187,
issues #24510/#19825/#18949/#20757, HF model cards, ubergarm's DeepSeek-R1-from-
NVMe report, FlexInfer (arXiv 2503.03777). ✅ = adversarially confirmed.

1. ✅ **mmap (default-on) is the only workable >RAM path.** `--no-mmap` mallocs
   anonymous memory → swap thrash on Linux, commit-limit hard-fail on Windows.
   `--direct-io` (Jan 2026, PR #18166) disables mmap where it applies — real
   O_DIRECT is Linux-only, silent no-op on macOS — and reads everything into
   anonymous RAM, so it is *not* a >RAM alternative. Evicted mmap pages are
   clean/read-only: re-read from the GGUF, never written → **no SSD wear**.
   Only anonymous memory (KV cache, activations) can swap.
2. ✅ **macOS Metal is the blocker.** With any `-ngl > 0`, weights are wrapped
   via `newBufferWithBytesNoCopy` and **wired** through an `MTLResidencySet`
   (macOS ≥ 15, PR #11427). Issue #24510 (open): partial offload wires the whole
   [min,max] file span — a 947 MiB offload wired a 30 GB file. Metal's ceiling is
   `recommendedMaxWorkingSetSize` ≈ 75% of RAM. **>RAM on Metal fails.**
   Candidate escapes, both needing hands-on testing: `-ngl 0` (CPU backend —
   pages stay file-backed/evictable) and `GGML_METAL_NO_RESIDENCY=1` (leaves
   no-copy buffers unwired; upstream warns it hurts perf).
3. ✅ **macOS does not degrade gracefully.** Issue #19825 (M4 Pro 24 GB, MoE >
   RAM via mmap): page-fault storms, system-wide sluggishness, kernel panics
   (wired GPU memory can't be reclaimed → panic instead of jetsam-killing the
   process). ollama#4151: whole-screen freezes even with models that technically
   fit. Guardrails must keep a safety margin, not just "allow it".
4. ✅ **Vulkan (Win/Linux) partial offload is safe for paging.** The Vulkan
   device reports `buffer_from_host_ptr=false`, so GPU layers are *copied* to
   VRAM while CPU-side layers stay in the mmap, file-backed and evictable
   (loader explicitly avoids pinned host buffers when mmap is on,
   llama-model-loader.cpp:1196-1204). Windows cosmetic quirk: mmap inflates
   apparent RAM usage but consumes no commit charge (#18949).
5. ✅ **Linux startup reads the whole file.** Prefetch is hardcoded
   (`MAP_POPULATE` + `madvise(WILLNEED)`, no CLI flag; only `--numa` disables
   it). macOS/Windows loads are lazy/advisory. Directly affects time-to-health.
6. ✅ **`/health` returns 200 only after load + warmup**; the HTTP listener
   starts before load and serves 503 meanwhile — our 300 ms poll loop is fine,
   only the 180 s cap (`engine.rs:77`) is at risk. `--no-warmup` (exact flag)
   makes readiness near-instant on macOS/Windows. Since PR #24009 (2026-06-02)
   warmup no longer touches all MoE experts — our b9833 is post-#24009. ⚠️ The
   bundler tracks the *latest* llama.cpp release (`bundler/src/main.rs:47`),
   not a hard pin — behavior notes here are per-build.
7. ✅ **MoE offload flags exist in our bundled b9833** (verified via `--help` on
   `crates/app/resources/llama/llama-server`): `-ot/--override-tensor
   "<regex>=<device>"`, `-cmoe/--cpu-moe`, `-ncmoe/--n-cpu-moe N` (first N
   layers' experts to CPU), plus `-ctk/-ctv` KV quant (q8_0 etc.; quantized V
   needs flash-attn, default auto) and `-nkvo/--no-kv-offload`.
8. **Performance envelope (measured, not marketing):**
   - Dense > RAM: 0.07–0.7 tok/s (FlexInfer; llama2-70B mmap 0.46–0.51 tok/s in
     the true streaming regime). Never interactive. **Excluded from scope.**
   - MoE > RAM, SSD streaming: often **0.5–2 tok/s** (ubergarm: DeepSeek-R1
     212 GB on 96 GB RAM + Gen5 NVMe ≈ 1.3 tok/s; GLM-4.5-Air Q4 on 32 GB ≈
     0.66 tok/s). Effective NVMe bandwidth during inference is 2–5 GB/s even on
     12 GB/s-rated drives (random access) — plan with *measured* bandwidth.
   - MoE slightly over RAM / hot set fits: dramatically better — Qwen3.5-35B-A3B
     IQ3 (13 GB) on a 16 GB M4 mini: **17 tok/s** with mmap, zero swap.
   - MoE fitting in RAM with experts on CPU: gpt-oss-120b ≈ 80 tok/s (M2 Ultra),
     25+ tok/s (64 GB DDR5 + 12 GB GPU, `--n-cpu-moe`).
   - ⚠️ Routing locality is model-dependent: Qwen3-30B-A3B caches at 97–99% hit
     rates; **gpt-oss-120b routing is flat** (hot-list covers only 28–45% of
     decode traffic) — page cache saves Qwen-style models, not gpt-oss, once
     you're far over RAM.
   - Upstream has **no expert-cache tier**: #20757 closed 2026-04-08
     unimplemented; forks (ik_llama.cpp, llama-cpp-moe-flash) claim 7–10 tok/s
     >RAM but are single-source/unreplicated — we stay on stock llama.cpp.
9. **External drives (portable build!):** USB4 (~3–4 GB/s) ≈ base-Mac internal;
   common 10 Gbps drives (~1 GB/s) are ~3× worse than base internal — MoE-only,
   and only after a disk-speed probe.

## What OpenPal's rules mean today (verified seams)

- Fit math: `working_set = size×1.10 + kv_step(params) + 700 MiB` vs
  `0.70 × total_ram` (`selection.rs:60-71,217-232`); over budget ⇒ `TooBig` ⇒
  Download hard-disabled (`Library.tsx:212`) and excluded from `recommend`.
  Nuance: an *installed* TooBig model (user-added local file) can still be run
  today — the gate is download-only.
- `fit_for(spec, hw)` has **no Settings access** (`selection.rs:56-59`) — a
  toggle-aware fit needs the signature threaded through `get_models` /
  `get_system_report` (both already hold `State`).
- `FitLevel` derives `Ord` from variant order (`types.rs:137-148`); a new
  variant touches: the enum position, `selection.rs:74-87` (why-copy),
  `:147-160` (headline), `:112-145` (sort/gates), TS union `types.ts:18`, both
  `Record`s in `Library.tsx:7-19`, string comparisons `Library.tsx:34,112,212`,
  `Onboarding.tsx:159`, and a CSS badge class.
- Engine flags: `EngineConfig` is built in exactly one place
  (`commands.rs:765-772`); `spawn_server` appends `cfg.extra_args` **last**
  (`engine.rs:464`), and the `show_thinking` append (`commands.rs:761-764`) is
  the per-settings override pattern. `gpu_layers` is binary 99/0
  (`commands.rs:767`).
- `STARTUP_TIMEOUT = 180 s` (`engine.rs:77`) is the only load-time ceiling;
  `chat_stream` has no timeout (slow generation is safe).
- **Everything is single-file GGUF**: one url/size/sha256 per spec
  (`types.rs:71-82`), HF browser rejects split GGUFs
  (`huggingface.rs:211-215`), download verifies one exact size. HF caps files at
  ~50 GB ⇒ models above ~50 GB *require* sharded-download work (gpt-oss-120b is
  3 shards; llama-server auto-opens shards 2..n given shard 1, PR #6187).
- Settings-toggle template: `force_cpu` end-to-end (`types.rs:467,500` →
  `commands.rs:1326-1333` → `store.ts:635-644` → `SettingsSheet.tsx:476-492`,
  consumed at next `start_engine` only).
- Catalog gaps: `moe` is a bare bool — no active-params field, no offload hints,
  no shard list. `extra_args` is the existing escape hatch.

## Design decisions (recommended)

- ~~**MoE-only.** Dense >RAM stays `TooBig` forever; the physics doesn't move.~~
  **Superseded (2026-07-18):** the toggle alone now decides `DiskSlow`
  eligibility for dense models too — "let the user be the judge" — since the
  physics were always "possible but often 0.07-0.7 tok/s," not "impossible."
  `selection::fit_for` gives dense (or MoE-without-a-known-active-size)
  models a starker "almost the whole file is read from disk every word"
  warning instead of the gentler MoE copy, but no longer refuses the option
  outright. See `crates/core/src/selection.rs`'s module docs.
- **Stock llama.cpp only** (user constraint). No ik_llama.cpp / fork adoption;
  revisit if upstream ever ships an expert cache.
- **Per-OS engine recipe** for disk-offload starts:
  - macOS: `-ngl 0` (avoid Metal wiring entirely) + `--no-warmup`. Keep mmap
    (default). Experiment later with `GGML_METAL_NO_RESIDENCY=1` + `-ngl N`.
  - Windows: `-ngl 99 --cpu-moe --no-warmup` (Vulkan-safe per #4 above;
    force_cpu setting still wins).
  - Linux: same as Windows, but expect a full-file read at mmap time
    (MAP_POPULATE) → needs the longer startup timeout below.
  - All: cap context at 8k in this mode (KV is anonymous memory — the part that
    actually swaps); optionally `-ctk q8_0 -ctv q8_0` later.
- **Resident-fit rule, not "anything goes":** allow disk mode only when the
  *resident* estimate fits the existing budget:
  `resident = active_fraction × size_bytes + kv_step + 700 MiB ≤ 0.70 × RAM`,
  where `active_fraction = active_params_b / params_b` (new catalog field),
  **and** `size_bytes × 1.1 + 1 GiB` fits free disk (existing rule). This keeps
  us out of the #19825 kernel-panic regime (which was "resident set itself
  doesn't fit").
- **New FitLevel variant `DiskSlow`** slotted between `TooBig` and `Tight`
  (never auto-recommended; visible + downloadable only when the toggle is on).
- **Informed consent at download AND at first run**, LM-Studio-style but
  honest about speed: show an estimated tok/s range from a disk-speed probe,
  in beginner language ("answers may take several minutes").

## Phases

### Phase 1 — Toggle + fit plumbing (pure app work, no engine changes) ✅ DONE
- `Settings.disk_offload: bool` (default false) cloned from the `force_cpu`
  template end-to-end; Advanced section copy: "Let huge AI models run from
  disk. They can be much smarter, but answers get slow."
- `ModelSpec.active_params_b: Option<f32>` (+ `activeParamsB` in catalog.json)
  for MoE entries; `kv_cache_bytes` keeps using total params.
- New `FitLevel::DiskSlow`: emitted by `fit_for` only when
  `spec.moe && resident_fits && !ram_fits` — thread `disk_offload` (or a
  `FitOptions`) into `fit_for`/`recommend`; when the toggle is off, DiskSlow
  collapses to TooBig so nothing changes for default users.
- UI: badge "Slow — runs from disk", Download enabled with a confirmation
  dialog, collapsed-section and Onboarding filters updated, CSS badge class.
- Tests: fit matrix (toggle on/off × moe × sizes), serde back-compat for old
  settings.json, ranked ordering with the new variant.
- Shipped as designed. `FitLevel::is_auto_recommendable()` (Rust) /
  `isAutoRecommendable()` (`ui/src/lib/format.ts`) centralize the "never
  auto-recommend DiskSlow" policy so it isn't hand-duplicated per call site.
  Installed models are now exempt from the collapsed "too big" bucket in
  Library.tsx (closes a gap that also pre-existed for oversized custom/local
  models). `saveSettings` refreshes the models list when `diskOffload` changes.

### Phase 2 — Engine recipe for DiskSlow starts ✅ DONE
- `start_engine`: when the selected model's fit is DiskSlow, force context to
  ≤ 8192 and append `--no-warmup` (append-last pattern, same as `show_thinking`).
- **Per-OS flag branching turned out to be unnecessary.** `HardwareInfo.has_metal`
  is `is_apple_silicon` only (`crates/core/src/hardware.rs`) — Windows/Linux
  already always compute `gpu_layers: 0` today (no Vulkan/GPU wiring exists yet
  in `start_engine`'s engine-start path, separate from the Vulkan-superset
  *engine binary* the portable build ships). So the one flag that actually
  matters — forcing `-ngl 0` — is a single unconditional `if disk_slow { 0 }`
  branch, safe and correct on every OS without `cfg!()` branching. The
  `--cpu-moe` flag from the original plan is moot while gpu_layers is already 0
  everywhere for DiskSlow; revisit if/when Windows/Linux GPU offload gets wired
  into `start_engine` for the *non*-disk-slow case.
- **Safety hardening**: `extra_args` is appended *after* `-ngl <gpu_layers>` on
  the command line (`engine::spawn_server`), so a catalog entry's own `-ngl`
  would silently override the DiskSlow safety guarantee. Added
  `strip_gpu_layers_flag()` (defense in depth — unreachable with today's
  catalog data) plus a catalog test (`no_catalog_entry_sets_its_own_gpu_layers`)
  asserting no entry's `extraArgs` ever sets it.
- Startup timeout: mode-aware via a new `EngineManager::start()` parameter
  (`STARTUP_TIMEOUT` stays 180s; `disk_slow_startup_timeout(size_bytes)` = 
  `max(300s, 2 × size / 100 MiB/s)` — a conservative, unmeasured placeholder
  documented for replacement by Phase 3's probe; erring toward "waits too long"
  rather than false-failing a genuinely loading model).
- Loading + first-chat UX: extended "this can take a few minutes" copy in
  Library.tsx (waking card), Sidebar.tsx (EnginePill), and ChatView.tsx
  (composer placeholder), via a shared `isStartingModelDiskSlow()` helper.
  `boot()` now waits for the initial `refreshModels()` before auto-resuming a
  selected model, so this copy is correct from the very first "starting" frame
  after app launch (closes a race the review caught).
- `EngineManager::spawn_server` needed no change (flags ride `extra_args`, as
  planned). The Metal-residency experiment (`GGML_METAL_NO_RESIDENCY=1`) is
  still open for later — DiskSlow stays `-ngl 0` (CPU backend) for now, which
  the research phase already confirmed is the only verified-safe option.

### Phase 3 — Disk-speed probe + guardrail calibration (empirical gate)
**Split into two halves. Only the first is done.**

- ✅ **Software probe — DONE.** `crates/core/src/disk_speed.rs` writes a
  throwaway 256 MiB file to the model volume in 1 MiB chunks (non-zero fill
  byte — some filesystems no-op an all-zero write as a sparse hole), `fsync`s
  it, times the write, deletes the file, and classifies `bytes_per_sec` into
  Fast (≥2 GB/s) / Slow (≥0.5 GB/s) / TooSlow (<0.5 GB/s, hard refuse — no
  "Download anyway"). **Deliberately measures write, not read**, and not at
  random offsets: the plan's original "read at random offsets" design doesn't
  work before the model file exists (nothing of its own to read yet), and
  reading any freshly-written file mostly measures the OS page cache, not the
  disk. A write+`fsync` sidesteps the cache entirely at the cost of measuring
  a related-but-not-identical quantity (see the module's doc comment for the
  full reasoning, including the known QLC/USB-bridge asymmetric-speed
  caveat). The plan's tok/s formula had a bug (`bw × active_fraction / size`
  is off by roughly `1 / active_fraction²`); the shipped formula is
  `bw / (active_fraction × size)` — bandwidth over active-weight bytes —
  sanity-checked against the DeepSeek-R1 data point above (see
  `disk_speed::estimated_tokens_per_sec`'s doc comment for the numbers).
  Probed once per app run and cached (`AppState::disk_speed_cache`,
  `commands::cached_volume_speed`) — the model volume can't move without an
  app relaunch, so re-probing on every download-confirm-dialog open and every
  engine start was pure waste, and worse, let two probes race on the same
  disk I/O and corrupt each other's reading. `probe_disk_speed` (new Tauri
  command) surfaces bytes/sec + class + a per-model tok/s estimate to the
  Library "run from disk" confirm dialog; `start_engine` feeds the same
  measurement into `disk_slow_startup_timeout` (now clamped to a 1-hour
  ceiling, not just floored at 300s — an unbounded pathologically-low probe
  reading must not turn into a multi-day silent hang).
- ⚠️ **Real-hardware bench matrix — PARTIALLY DONE (2026-07-08).** One real
  run completed: 16 GB Apple Silicon (M4, macOS 26.5), `glm-4.5-air`
  (UD-IQ2_XXS, 42.68 GB) downloaded for real into the live app's own models
  dir (sha256-verified byte-for-byte against the catalog entry) and run with
  the *exact* `start_engine` DiskSlow recipe (`-ngl 0 -c 8192 --no-warmup`,
  via the bundled b9833 `llama-server` binary). No GUI-automation tool was
  available in this environment, so this drove the engine subprocess and its
  HTTP API directly rather than through the Tauri app — same binary, same
  args, same model file, just not through the frontend.
  - **Safety verdict: PASS.** Write-probe measured 2.38 GB/s (Fast).
    `/health` came up in 98s. RSS held at 1.7–2.7 GB across ~35 minutes of
    continuous load + generation — nowhere near the 16 GB budget. Zero
    crashes, zero kernel panics, zero hangs; system uptime was unaffected
    and the process exited cleanly on SIGTERM with memory fully released.
    This is a direct real-hardware refutation of #19825/#24510 for the
    **`-ngl 0` CPU-only path specifically** — consistent with that bug being
    about *partial* Metal offload wiring the whole mmapped span; `-ngl 0`
    never touches Metal at all. Partial-offload behavior itself is still
    untested (nothing in this feature does that on macOS, by design), and
    32 GB Mac / Windows+Vulkan / Linux remain unverified.
  - **Performance verdict: worse than planned — a real measurement, not an
    estimate.** Read directly from llama-server's own timing output across
    two requests (one ran ~32 min / 204 tokens before its client connection
    was cut, one ran to completion): **0.124 tok/s generation, 0.288 tok/s
    prompt processing.** ~4× slower than this doc's prior "expect 1–6 tok/s"
    guess, and ~4× slower than `disk_speed::estimated_tokens_per_sec`'s
    write-probe-based formula predicted for this exact model (~0.49 tok/s
    from the measured 2.38 GB/s write speed). At this rate a 200-token reply
    takes ~27 minutes — unusable, not just slow. Likely explanation
    (unconfirmed): sequential write bandwidth is a poor proxy for the real
    bottleneck (many small random reads as the OS pages in scattered
    active-expert blocks per token), and/or GLM-4.5-Air's expert-routing
    locality is poor the way the plan already flags gpt-oss-120b's flat
    routing as a worst case. **One data point — not enough to recalibrate
    the formula or thresholds on its own, but a strong signal the
    write-probe estimate is optimistic and consent-dialog copy shouldn't
    oversell tok/s.**
  - Still not run: 32 GB M-series (does more RAM/page-cache headroom help
    the tok/s number at all?), Windows/Linux + GPU (`-ngl 99 --cpu-moe`),
    and a cheaper comparison model (`gpt-oss-20b`, 12.11 GB, also exceeds
    this machine's RAM budget — a much faster follow-up run than the 42 GB
    flagship). `disk_offload` stays off-by-default / power-user-framed —
    one confirmed-safe-but-confirmed-slow data point moves the go/no-go
    gate forward, it doesn't clear it.
  - 16 GB M-series: Qwen3-30B-A3B Q4 (18.6 GB, CPU-only) — expect 5–15 tok/s
    (still unverified; a different model from the one actually run).
  - 32 GB M-series: `glm-4.5-air` — **verified on 16 GB instead, see above**;
    the 32 GB run is still needed.
  - Win/Linux 32–64 GB + GPU: same models with `-ngl 99 --cpu-moe`. This is
    now what `start_engine` actually produces on such machines (see
    `gpu_plan`, shipped 2026-07-08) — still untested on real hardware.
  - Decide from data: minimum RAM per catalog entry, whether
    `GGML_METAL_NO_RESIDENCY=1` + partial `-ngl` beats `-ngl 0` on Macs,
    whether KV quant is worth it, and whether the write-probe's floor/ceiling
    thresholds — and its tok/s formula — need recalibrating against what
    real hardware shows (this run's ~4× gap is a first data point).
  - **Speed-lever bench, round 1 (2026-07-08 evening, same 16 GB M4,
    glm-4.5-air, exact DiskSlow recipe):**
    - Baseline reproduced: **0.127 tok/s** generation (64 tokens), PP 0.565
      tok/s on a 30-token prompt — consistent with the morning's 0.124, so
      run-to-run noise is small.
    - **`--spec-type draft-mtp` CRASHES b9833 with the Unsloth UD-IQ2_XXS
      GGUF.** The file *does* contain the `blk.46.nextn.*` MTP tensors
      (verified via GGUF header parse), but the loader logs
      `unused tensor blk.46.nextn.shared_head_norm.weight -- ignoring` — the
      conversion predates b9833's MTP tensor mapping — and the first
      generation then aborts inside the glm4-moe graph builder
      (`glm4-moe.cpp:149: "This GGUF does not support multimodal. Please
      reconvert it."`). Hard abort, reproducible. **Do NOT ship draft-mtp in
      this entry's `extraArgs`**; revisit after the next engine bump and/or
      an Unsloth reconversion (ubergarm's GLM-4.5-Air GGUF reportedly keeps
      MTP tensors in a supported layout).
    - **`--spec-type ngram-mod`: no win** — 0.121 tok/s vs the 0.124–0.127
      baseline band on the same free-form prompt (64 tokens). Matches the
      research prediction (~1.0× on non-repetitive output). Not shipped;
      might still help code/JSON-heavy tasks, but doesn't earn a default.
  - **Speed-lever bench, round 2 (2026-07-11, same 16 GB M4, glm-4.5-air,
    exact DiskSlow recipe — `scripts/bench/bench_round2.sh`):**
    - **`-b 2048 -ub 2048` doubles prompt processing: 10.65 tok/s vs 5.04
      tok/s** at the default `-ub 512` on the same 2354-token prompt
      (221 s vs 467 s to first token), generation unchanged (0.123 vs
      0.116 tok/s — within noise of the 0.121–0.127 baseline band). Makes
      mechanical sense: every expert paged in from disk serves 4× the
      tokens per forward pass. Note PP on a *long* prompt is ~5–10 tok/s,
      not the 0.3–0.6 tok/s seen on 30-token prompts — short-prompt PP is
      dominated by first-touch page-in, so it amortizes with length.
    - **`-fa on -ctk q8_0 -ctv q8_0`: no regression** — 0.153 tok/s
      generation (32 tokens), at or above the baseline band. (It ran last
      of four configs, so residual page-cache warmth may flatter it —
      recorded as "no regression", not as a win.) KV quant halves the KV
      cache's claim on the RAM headroom the streamed weights page through.
    - `--spec-type ngram-mod` re-run: 0.122 tok/s — confirms round 1's
      no-win verdict.
    - **Both measured winners are now wired into the DiskSlow recipe**
      (`start_engine` appends `-b 2048 -ub 2048 -fa on -ctk q8_0 -ctv
      q8_0` alongside `--no-warmup` for every disk-slow start), satisfying
      the plan's "ship only with same-machine measurements" rule.
  - **Speed-lever bench, round 3 safe half (2026-07-11, same 16 GB M4):**
    gpt-oss-20b mxfp4 (`ggml-org/gpt-oss-20b-GGUF`, 12,109,566,560 bytes,
    sha256-verified) under the exact DiskSlow recipe (`-ngl 0 -c 8192
    --no-warmup`) — the first *measured* near-fit point, and the flat-
    routing worst case at that:
    - **3.68 tok/s generation** (64 tokens), 4.29 tok/s PP (28-token
      prompt), healthy in ~45 s, RSS ~4.8 GB, clean run. Near-fit is
      interactive-usable, exactly the regime the feature should steer
      users toward.
    - **Estimator validated**: `estimated_tokens_per_sec` predicts ~4.1
      for this spec (12.11 GB, ~20.9B/3.6B active, 2.38 GB/s probe) —
      within ~11% of measured, *without* refitting the shortfall factor,
      despite gpt-oss's flat expert routing. Both same-machine calibration
      points (0.124 measured vs ~0.13 est far-over-RAM; 3.68 measured vs
      ~4.1 est near-fit) now bracket the regime well; locked in as a
      regression test (`estimate_is_calibrated_to_the_measured_gpt_oss_20b_bench`).
  - **Still unmeasured:** round 3's guarded `GGML_METAL_NO_RESIDENCY=1
    -ngl 99 --cpu-moe` Metal experiment (`scripts/bench/bench_round3.sh`,
    wired-memory watchdog with a 13.5 GB abort threshold — deliberately
    deferred until the machine can be sacrificed; user decision
    2026-07-11).
  - **Speed-lever bench, round 4 — MTP self-drafting on a *dense* model
    (2026-11-08, same 16 GB M4, engine **b11960** = `0.8.0-dev (build
    11960, commit 8642541b2)`, `scripts/bench/bench_qwen_mtp.sh`):**
    - Model: **qwen3.6-27b** Q4_K_M (17,106,773,120 bytes, sha256-verified)
      + its bf16 mmproj (931,146,304 bytes, sha256-verified), exact
      DiskSlow recipe (`-ngl 0 -b 2048 -ub 2048 -fa on -ctk q8_0 -ctv
      q8_0 --no-warmup`) + `--mmproj`, 30-token prompt, `n_predict 128`.
      The *dense* case the MoE expert-union defeat (next section) doesn't
      cover: MTP self-drafting batches one forward pass per draft window,
      so the streamed weights are read once per window instead of once per
      token.
    - **Baseline (no spec): did NOT finish** — both reps were still
      generating at the 60-min per-request cap, so it's recorded as an
      upper bound of **<0.036 tok/s**.
    - **`--spec-type draft-mtp` wins at every depth, monotonically with
      depth**: depth 2 → **0.050 tok/s** (`draft_n` 90, 81 accepted, mean
      draft len 2.80) · depth 3 → **0.062** (110/89, 3.41) · depth 4 →
      **0.074** (128/95, 3.97) — all ≥ ~2× the baseline bound, clean runs
      (RSS ~2.1–2.6 GB at health, mmproj loaded, 0 errors, clean exit).
      Single-rep measurement (round-2 reps stopped mid-run by user
      decision); the monotone depth ladder matches the mechanism, but
      re-bench after any engine bump.
    - **SHIPPED 2026-11-08**: `start_engine` enables self-drafting for the
      `qwen3.6-27b` catalog entry only (`--spec-type draft-mtp
      --spec-draft-n-max 4`, from the bench-verified `mtp` field — see
      `ModelSpec::mtp`), gated on the *current* engine binary advertising
      `draft-mtp` in `--help` (probed fresh per start, since an in-app
      engine update can swap the binary mid-session). Depth 4 measured
      best, so the shipped depth is 4 (llama-server's own default is 3).
    - **GLM-4.5-Air re-probed on b11960** (same UD-IQ2_XXS GGUF, same
      recipe): round 1's b9833 hard abort is **gone** — `draft-mtp` runs,
      but shows **no speed win** (depth 2: 0.178, depth 3: 0.162 vs 0.181
      baseline tok/s; `draft_n` 52–74) — the MoE expert-union effect wins
      as predicted. The entry stays un-armed; no real-GGUF fallback repro
      exists, so the crash fallback ships as defensive engineering verified
      by unit tests.
  - **Production follow-up — issue #6 (2026-11-08, the day after ship):
    the live qwen3.6-27b MTP engine broke, so recovery is now a first-class
    part of the feature rather than a defensive afterthought.** The user's
    app reported `stream read failed: error decoding response body` on an
    agent turn, then two `classify request failed` warnings 20 s apart, then
    `request failed: error sending request`. Diagnosis against the live
    engine + a controlled repro of the exact production recipe found:
    - **Warm-cache generation is glacial on this machine (~0.05–0.08
      tok/s), so a 64-token turn is 15–20 min — and that's the MTP *win*;
      the shipped depth-4 number (0.074) is real.** The plain-chat
      first-message fallback never fires on a mid-stream break, so the
      original recovery didn't cover any of what actually broke.
    - **The MTP server aborted a mid-answer stream** (partial body decode —
      the user's first error), then **stalled a concurrent request for the
      full 20 s** the app's classify path waits, then **hard-aborted
      (SIGABRT)** inside Metal teardown (`ggml_metal_rsets_free` →
      `ggml_abort`) at exit after its parent app closed (orphaned to
      launchd) — `~/Library/Logs/DiagnosticReports/llama-server-*.ips` is
      the crash report. A non-MTP control with the same model stalled too
      (accepted a request, then 0.1% CPU, no generation): the engine/speed
      story is the deep problem; MTP's marginal failures are the part the
      app can recover from.
    - **Built (user decision: keep MTP, recover):** `EngineManager` gained
      three primitives — `is_healthy()` (subprocess-exit check + one
      loopback `/health` GET, 3 s cap), `mtp_fallback_armed()` (an
      `MtpRestart` snapshot exists), and `restart_without_mtp(generation)`
      — the *one* restart that strips the `--spec-*` args and refuses a
      superseded engine. Three recovery paths reuse it:
      1. **Plain-chat first message** (already shipped): `chat_stream`
         retries once after a pre-answer MTP crash — never resends a
         mid-answer break.
      2. **Agent/web-search turns** (`run_agent_turn_with_recovery`): a
         turn error with zero tokens emitted on an armed-but-unhealthy
         engine restarts it without MTP. The turn is **never replayed** —
         agent turns may run tools with real side effects.
      3. **Idle-loop watchdog** (`maybe_recover_engine`, the same 5 s tick
         as auto-unload): when idle (never while `generating`), a dead or
         wedged engine is restarted without MTP, or stopped if it was
         already MTP-less.
    - No agent turn is ever auto-resent (tool side effects); recovery
      re-arms the engine for the user's *next* request and surfaces the
      original error.
    - Re-bench on the next engine bump stands — b11960's MTP on this
      hardware is functional but not useful-speed at any depth, which the
      round-4 single-rep green-light (with caveats) undersold; the recovery
      layer is what makes shipping it defensible.
  - **Second production repro — 2026-11-10 (engine b12110, same M4, GPU
    layers active):** the identical first symptom recurred
    (`agent turn: stream read failed: error decoding response body`), and
    the engine log proved the recovery shipped for #6 would *not* have
    fired: the severance happened while the process stayed alive and kept
    serving tasks (slot 0 released 66 tokens, slot 1 later ran 1734 to
    completion), so `is_healthy()` answered true and `child_died()` stayed
    false — every 2026-11-08 recovery predicate was false. The recovery's
    "dead or wedged" framing missed the actual recurring mode: **a live
    engine severing an in-flight stream**.
    - **Widened (keep MTP, recover — same user decision):** the MTP
      snapshot itself is now the recovery trigger, not liveness. Any
      request-level failure on an MTP-armed engine retires MTP for the
      next turn: `chat_stream` restarts stripped and resends, but only
      when zero tokens were emitted (a mid-answer break surfaces its
      original error against the now-stripped engine — restarting is not
      replaying); agent/web-search turns restart stripped on any turn
      error and — because restarting is not replaying — replay the turn
      exactly once when the failed attempt provably emitted *nothing*
      (no token, no reasoning, no consent/ask prompt, no started tool
      call, no search), against a freshly fetched endpoint (the restart
      moves the port). A turn that showed anything or ran any tool is
      never replayed. `restart_without_mtp` consumes the snapshot on the
      stripped start, so retirement is one-shot per MTP-armed session
      and recovery cannot loop.
    - Re-bench on the next engine bump still stands; b12110's MTP remains
      functional but flaky on this hardware — recovery is the product.
  - **Post-mortem follow-up — 2026-11-10 evening: the recurring
    `stream read failed: error decoding response body` was NOT an MTP
    failure at all. It is a client-side 30-second read timeout.**
    The symptom kept recurring through two MTP-recovery fixes (the
    die/wedge restart and the widened any-failure retirement above), so the
    root cause was hunted to ground:
    - **The error string is reqwest's `Kind::Decode` display.** Every
      body-read error on `Response::bytes_stream()` is mapped through
      `reqwest::error::decode` (verified in the vendored 0.12.28 source),
      which displays as exactly `error decoding response body`.
    - **The trigger is a 30 s `read_timeout` on the app's shared HTTP
      client** (`state.http` in `crates/app/src/state.rs`: `connect_timeout
      20 s`, `read_timeout 30 s`). reqwest applies it per body read and
      cannot override it per-request. The agent/tool loop used that exact
      client (`AgentDeps.http` ← `state.http.clone()`), so any streaming
      turn that produces no body bytes for 30 consecutive seconds is cut
      off mid-body and the connection dropped.
    - **llama-server is silent for far longer than 30 s before the first
      body byte.** An agent turn carries OpenAI `tools` (~350-token prompt);
      on the disk-streamed qwen3.6-27b the measured prompt processing is
      60–100+ s of zero output (reproduced 2026-11-10: 346 tokens at 3.42
      tok/s = 101 s before the first byte). Generation then streams at
      ~0.05–0.08 tok/s — inter-token gaps of 12–20 s, spiking past 30 s
      under memory pressure, which is the mid-answer variant of the same
      cut-off (issue #6's "partial body decode" and "stalled a concurrent
      request" observations are this too, and the 20 s classify timeout
      that fired 20 s apart in #6 is the same class of problem one rung
      lower).
    - **Live reproduction (2026-11-10,** exact DiskSlow recipe + b12110,
      MTP armed): the agent request got `200 OK` + SSE headers at +2.3 s,
      then **zero bytes**, then a 30 s read-timeout kill at +32.4 s; the
      server logged `stop: cancel task, id_task = 9` — the same line and
      the same ~30 s offset as the failing production session's
      `engine.log` (`stop: cancel task, id_task = 0` at +30.1 s). The
      server itself never severed anything with MTP armed — it survived,
      released the slot and kept serving, exactly as production's slot 1
      kept running 1734 tokens. This is a client-side timeout, not a
      server abort.
    - **Why the failing session's engine had no MTP at all:** its
      `engine.log` lacks the `creating MTP draft context` line and reports
      `blk.64.nextn.*` (the MTP-head tensors) as `unused — ignoring` — the
      fingerprints of an MTP-disabled load (the MTP-enabled repro shows the
      draft-context line and no unused-tensor warnings). So `mtp_fallback_
      armed()` was false and both recovery fixes correctly declined to
      act — and if it had fired, retiring MTP couldn't help anyway: the
      cut-off is client-side and independent of the server's `--spec-*`
      flags. That is why two fixes "didn't work" — they were aimed at the
      wrong layer.
    - **Fixed at the root (shipped 2026-11-10, engine b12110):** the
      agent/tool-loop, subagent, web-search and cloud-chat streaming paths
      now use a dedicated client built by `agent_bridge::turn_http_client`
      — `connect_timeout 20 s`, **no `read_timeout` and no total timeout**,
      mirroring the engine's own `chat_stream` client. REST calls (Hugging
      Face browser, cloud API surface) keep the 30 s read timeout, which is
      right for them; the web-search/fetch tools keep their explicit 20 s
      per-request timeouts on top of it. Long-hang backstops are unchanged:
      the user cancel flag and the wedged-engine idle watchdog. `state.http`
      now carries a comment forbidding streaming use, and
      `build_deps`/`build_web_search_deps` no longer accept a client
      parameter so the hazard can't be reintroduced from a call site. The
      two MTP-recovery changes stay as defensive engineering (a real MTP
      head abort is still handled), but they are no longer the story for
      this error.

### Phase 4 — Catalog entries for big MoE models (≤ 50 GB single-file first) ✅ DONE
- **Shipped visible** (user decision, accepting the gap until Phase 3's
  real-hardware bench runs — the existing gates: opt-in toggle, resident-fit
  math, forced `-ngl 0`, speed-floor refusal, explicit confirm dialog, all
  still apply).
- `glm-4.5-air`: the disk-mode flagship. `unsloth/GLM-4.5-Air-GGUF`,
  `GLM-4.5-Air-UD-IQ2_XXS.gguf`, verified 42,682,240,064 bytes (sha256
  confirmed via the HF API, independently cross-checked with a live HEAD
  request), 106B total / 12B active MoE, MIT, 131072 context (from the base
  model's `config.json`). Resident-set math confirmed by hand and locked in
  as a test using the real catalog entry
  (`glm_4_5_air_reaches_disk_slow_on_a_32gb_machine_but_not_without_the_toggle`):
  DiskSlow on a 32 GB machine with the toggle on, TooBig with it off.
- `glm-4.7-flash`: research (per this phase's own "re-check the 2026 field"
  bullet) confirmed this is real, MIT, 31.2B total / ~3.6B active MoE, and
  its Q4_K_M quant (18,312,339,808 bytes, verified) fits 24–32 GB machines
  **without** disk mode — exactly as anticipated. Added as a normal catalog
  entry (no disk-mode involvement at all), filling a real gap in the regular
  24–32 GB tier at zero extra engineering cost.
- Both verified against the live HF API (exact byte size + sha256 + license),
  not estimated — matching `catalog.rs`'s existing verification bar.
- **Found and fixed a real safety gap while wiring this in**: `start_engine`
  computed its "should I force `-ngl 0`" decision from
  `fit_for(spec, hw, settings.disk_offload) == FitLevel::DiskSlow` — i.e. from
  the *current* toggle state. Once a catalog entry existed that's actually
  bigger than RAM, this became reachable: download a huge model with the
  toggle on, turn the toggle off, hit "Use this model" (installed models
  aren't gated on fit) → `fit` becomes `TooBig`, not `DiskSlow` →
  `gpu_layers` falls through to `99` on Apple Silicon → the exact
  partial-Metal-offload-of-a-bigger-than-RAM-file scenario issue #24510
  documents as an OOM/panic risk. (Same gap for a dense/custom oversized
  model, which can never reach `DiskSlow` at all.) Fixed with a new
  toggle-independent `selection::exceeds_ram(spec, hw)` — the download-time
  gate stays toggle-dependent (correct: that's a consent question), but the
  engine-start safety recipe now checks "does the full model fit in RAM",
  full stop. `start_engine` uses this instead of the toggle-dependent fit.
- License gate: both entries are MIT, holding the Apache/MIT-only rule for
  default-visible catalog entries.

### Phase 5 — Sharded GGUF support ✅ DONE (plumbing only, 2026-07-08)
- `ModelSpec` grows `extra_shards: Vec<ModelShard>` (`#[serde(default)]` —
  every pre-existing catalog.json/custom-models.json entry parses unchanged,
  defaulting to a single shard). `url`/`file_name`/`size_bytes`/`sha256` keep
  meaning "shard 1", which is exactly why `model_file_path` needed **no code
  change** — it already returns shard 1's path, the correct `-m` argument for
  llama.cpp's auto-shard-discovery (given every shard lands in the same
  directory with matching `-NNNNN-of-MMMMM` naming).
- **Safety-critical thread**: `spec.size_bytes` alone drove RAM-fit math
  (`selection::exceeds_ram`/`fit_for`) and disk-speed tok/s estimation — both
  gate whether `start_engine` forces `-ngl 0`. Undercounting a sharded
  model's size to shard 1 alone could have misjudged it as fitting RAM and
  let it reach `-ngl 99` on Metal, exactly the bigger-than-RAM-file OOM/panic
  scenario (#24510) Phase 4 already had to fix once. Added
  `ModelSpec::total_size_bytes()` (sum across shards) and threaded it through
  every fit/gating call site (`selection.rs`, `disk_speed.rs`,
  `start_engine`'s startup-timeout calc) — a regression test
  (`fit_math_counts_every_shard_not_just_shard_one`) pins this: a 10 GB
  shard 1 that would rate `Perfect` alone correctly reaches `DiskSlow` once
  its 70 GB of extra shards are counted.
- `download_model` generalized from its old two-phase special case (weights,
  then optional mmproj) into a sequential loop over "parts": every shard is
  **required** (a missing shard hard-fails the whole download, unlike
  mmproj's existing soft-degrade-to-text-only tolerance); progress is one
  combined bar across every part. `delete_model` and the disk-space precheck
  loop the same way. `model_status` now gates "Installed" on **every**
  required file existing (a model missing shard 2 can't actually run) while
  still treating mmproj as non-gating, unchanged from before.
- Hugging Face browser (`huggingface.rs`) no longer rejects split GGUFs: it
  groups tree entries by shard stem/total, keeps only a complete contiguous
  `1..=total` set (an incomplete/gated listing is dropped, not guessed at),
  and lists each group as one pickable entry (`HfFile.extra_shards`).
  `custom::hf_spec` renames every shard of a user-added split model to share
  one id-derived stem (`{id}-00001-of-00003.gguf`, …) — required because
  llama.cpp's shard auto-discovery only substitutes the index, so every
  shard's renamed file name must be identical apart from that.
- Found and fixed the same latent gap in the **Portable-build staging tool**
  (`bundler/src/model.rs`, a separate download/verify implementation from
  `crates/core`): it would have silently staged only shard 1 of a sharded
  catalog entry into a portable bundle. Mirrored the same `extra_shards`
  shape there too.
- **Deliberately not shipped in this phase**: an actual sharded catalog entry
  (`gpt-oss-120b`, `Qwen3-235B`). This phase's own prerequisite — "only worth
  it if Phase 3 benchmarks show >50 GB models are actually usable" — points
  the other way: Phase 3's one real data point (glm-4.5-air, 42.68 GB,
  **0.124 tok/s** on a 16 GB Mac) suggests an even larger sharded model would
  be worse, not better. Adding a real sharded flagship is a separate,
  not-yet-made product decision now that the plumbing exists.

## Speed-up levers (researched + adversarially verified 2026-07-08)

A dedicated research pass (4 web-research agents, findings verified against
llama.cpp primary sources — master source, PRs, issues) into "can DiskSlow be
made faster with stock llama.cpp". ✅ = claim verified against a primary
source by a second agent.

- ✅ **There is no "hot experts in RAM" control in stock llama.cpp at
  `-ngl 0`.** `-ot`/`--cpu-moe`/`--n-cpu-moe` are backend *placement* flags;
  with only the CPU backend they resolve to the buffer type already in use —
  no-ops. mmap is one flat mapping (whole-file `POSIX_MADV_WILLNEED`, no
  per-tensor mlock/madvise, no readahead knob); `--mlock` on a >RAM model
  locks a file-order prefix then warns and gives up. The OS page cache is the
  only expert-cache tier, so expert-routing locality is what decides tok/s.
- ✅ **Upstream watch — PR #25294** ("llama: stream MoE routed experts from
  disk", opened 2026-07-04): per-layer expert-slab RAM cache
  (`--moe-stream-cache`) + async O_DIRECT streaming workers, explicitly
  targeted at unified-memory machines (Mac, DGX Spark). Author-measured
  **2.4× decode / 5.3× prefill** over exactly our recipe (mmap +
  CPU-side experts) on a 254 GB model. Unmerged as of 2026-07-08; noted
  limitation: single llama_context only. **When this merges into a release
  the bundler picks up, revisit the whole DiskSlow recipe** — it replaces
  page-cache luck with a real expert cache. (Expert-cache attempts before it
  — issue #20757, PR #24524 — all died; this one has real benchmarks.)
- ✅ **Windows/Linux + GPU is the real mixed-RAM/disk path** (`-ngl 99
  --n-cpu-moe`): CUDA/Vulkan report `buffer_from_host_ptr=false`, so hot
  tensors are *copied* into VRAM while experts stay file-backed. Tens of
  tok/s for RAM+VRAM-fitting MoE (gpt-oss-120b ~53-67 tok/s on an RTX 3060
  12 GB); for truly >RAM models decode stays SSD-bound but PP multiplies.
  (Caveat from verification: the token-embedding table always stays in host
  memory; that's a tiny get_rows, irrelevant.) This is open question #4's
  feature, now grounded.
  **SHIPPED 2026-07-08 (code-complete, real-hardware-unverified):**
  `gpu::detect_vulkan_gpu` asks the bundled binary via
  `llama-server --list-devices` once at startup
  (`HardwareInfo.gpu_name`/`gpu_vram_bytes`), and `commands::gpu_plan`
  gives MoE models `-ngl 99 --cpu-moe` when a ≥ 8 GiB Vulkan GPU is
  present (`force_cpu` still wins; dense models stay CPU-only — offloading
  them safely needs VRAM-fit math, and a failed GPU allocation is a failed
  engine start). Metal/DiskSlow behavior is unchanged and pinned by
  `gpu_plan_never_offloads_a_disk_slow_model_on_metal`. Needs a real
  Windows/Linux + GPU machine to verify end-to-end — no such hardware was
  available to this session.
- ✅ **macOS Metal stays blocked** (#24510 open, no fix in flight; zero
  maintainer engagement). `GGML_METAL_NO_RESIDENCY` (any non-empty value)
  verifiably still disables residency-set wiring on master — the only
  stock-binary escape hatch for `-ngl > 0` on >RAM models, with **zero
  published success reports**; bench-only until proven (Track C). The
  GGUF-tensor-reordering workaround demonstrated in #24510 (wired span
  30,973 → 947 MiB) works but needs file preprocessing we're not doing.
- **Speculative decoding is mostly defeated by the expert-union effect** on
  MoE (verifying K draft tokens activates the union of each token's experts:
  measured 2-3× data movement, arXiv 2506.20675; published MoE benchmarks
  show draft-model speculation is a net LOSS). The exception worth testing:
  **GLM-4.5-Air's native MTP head** — `--spec-type draft-mtp` (merged
  upstream 2026-05-16, present in our b9833), and our UD-IQ2_XXS GGUF
  **verifiably retains the `blk.46.nextn.*` tensors** (checked 2026-07-08,
  local GGUF header parse). Self-drafting costs no extra assets/RAM;
  disk-bound expectation is a modest ~1.2-1.5×. n-gram modes (`ngram-mod`)
  are free but ~1.0× on free-form chat. Nobody has published spec-decode
  numbers in the disk-streaming regime — our A/B is a first.
- ✅ **MTP self-drafting works on *dense* models — MEASURED + SHIPPED
  2026-11-08** (bench round 4, qwen3.6-27b, exact DiskSlow recipe): the
  expert-union defeat doesn't apply — one batched forward pass reads the
  streamed weights per draft window instead of per token. Depth-4
  `draft-mtp` measured **0.074 tok/s vs a <0.036 baseline bound** (~2×,
  monotone in depth: 0.050 → 0.062 → 0.074). **Shipped for the
  `qwen3.6-27b` catalog entry only**, behind a per-start `--help` probe of
  the current engine binary (see `ModelSpec::mtp`). GLM-4.5-Air re-probed
  on b11960 while doing this: its MTP head no longer crashes (b9833's hard
  abort is fixed upstream) but shows **no win** (0.162–0.178 vs 0.181
  baseline) — the MoE prediction holds, so it stays un-armed.
- ✅ **Prompt processing scales with `-ub`** (each physical batch is one
  streaming pass over the weights, so fewer, fuller passes ≈ linear PP gain
  on long prompts), at the cost of compute-buffer RAM that competes with the
  page cache on a 16 GB machine. **MEASURED + SHIPPED 2026-07-11** (bench
  round 2, glm-4.5-air, 2354-token prompt): `-b 2048 -ub 2048` gives
  **2.1× PP** (10.65 vs 5.04 tok/s, 221 s vs 467 s to first token),
  generation unchanged, RSS unchanged (~4.6 GB either way — the bigger
  compute buffer didn't measurably squeeze the page cache).
  KV-quant (`-ctk/-ctv q8_0`, needs flash-attn) + `-fa on` frees ~1-2 GB —
  a rounding error at 2.5× RAM, potentially a step change for models that
  *nearly* fit. **MEASURED + SHIPPED 2026-07-11**: no generation
  regression (0.153 tok/s, at/above the 0.121–0.127 band, warm-cache
  caveat). Both now appended to every DiskSlow start by `start_engine`.
- **The dominant lever is model choice, not flags**: on the same 16 GB
  M4-class hardware and same recipe, a 13 GB Qwen3.5-35B-A3B quant reports
  17.3 tok/s and an ~18 GB GLM-4.7-Flash ~4 tok/s vs our measured 0.124
  tok/s at 42.7 GB — a 30-140× spread. Hence the consent dialog now steers
  toward the best-fitting alternative model.
- **Rejected**: forks (ik_llama.cpp is healthy but verifiably has nothing
  for SSD-paging workloads), `--direct-io`/`--no-mmap`/`--mlock` (all
  counterproductive for >RAM), external draft models (net-negative on MoE),
  GGUF reordering (engineering cost + untested panic-risk path).

## Risk register

| Risk | Severity | Mitigation |
|---|---|---|
| macOS kernel panic / system freeze under pressure (#19825) | High | MoE-only + resident-fit rule + `-ngl 0` (nothing wired). **16 GB Mac go/no-go bench PASSED (2026-07-08)**: glm-4.5-air, ~35 min continuous run, zero crashes — 32 GB Mac / Win / Linux still open |
| Metal wires whole file with partial `-ngl` (#24510, open) | High | Never use `-ngl > 0` on macOS in disk mode until upstream fixes or NO_RESIDENCY validated |
| Sub-1 tok/s disappointment (flat-routing models, slow disks) | High — **confirmed, not hypothetical**: glm-4.5-air measured 0.124 tok/s on a 16 GB Mac (2026-07-08), ~4× worse than the probe's own estimate | Disk-speed probe + honest estimate in the consent dialog (the estimate itself undershot the real slowdown by ~4× in this run); refuse below floor — consent-dialog copy may need to set expectations lower still |
| Startup timeout kills legit slow loads (Linux MAP_POPULATE) | Medium | Mode-aware timeout in Phase 2 |
| Engine not hard-pinned (bundler tracks latest release) | Medium | Pin or record the b-tag; warmup/flag semantics verified per-build (post-#24009 today) |
| User-added models: `params_b` estimated, `moe=false`, no active-params | Low | Disk mode is catalog-only at first; custom models keep current behavior |
| KV/context blowup (fit math assumes 8k, engine may start higher) | Low | Hard-cap context in disk mode (Phase 2) |
| ~~`-ngl 0` safety only applied when the *current* toggle state classified a model DiskSlow — toggle-off-after-download or a dense/custom oversized model got `gpu_layers: 99` on Metal instead~~ | ~~High~~ | **Fixed in Phase 4**: `selection::exceeds_ram` decouples the engine-start safety recipe from the toggle entirely |

## Open questions (need user decision or Phase 3 data)

1. ~~Ship order: is Phase 5 (sharding) in scope at all, or stop at ≤50 GB
   single-file models?~~ **Resolved 2026-07-08**: in scope, shipped as
   plumbing-only (see Phase 5 above). Still open: whether to actually add a
   sharded catalog entry (gpt-oss-120b, Qwen3-235B), given Phase 3's
   real-hardware data argues against it for now.
2. Should the disk-speed probe run at download time only, or re-run at engine
   start (portable build may move between drives)?
3. Copy/tone for the consent dialog ("minutes per answer" honesty vs
   discouraging adoption).
4. Whether to expose `--n-cpu-moe` tuning for 64 GB+ users whose MoE models
   *fit* — same plumbing, big speed win, separate feature ("expert offload for
   speed" vs "disk offload for size") — recommended as a fast follow.
