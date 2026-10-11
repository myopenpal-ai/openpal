# Speed-estimate accuracy — diagnosing and fixing the resident-memory tok/s estimate

Diagnosis produced 2026-07-19 (multi-agent research workflow: doc-digest agent,
web-research agent, adversarial numeric-audit agent, two independent
implementation proposals) in response to a user report of large estimate/reality
gaps on an Apple M4 Mac. Fix implemented the same day.

## The report

| Model | quant | estimate | reality |
|---|---|---|---|
| gemma 4 26B A4B it (MoE) | UD Q4_K_XL | 9.8 | 1.5 |
| gemma 4 12B (dense) | (default) | 16 | 8.9 |
| gemma 4 E4B it (dense-equivalent) | UD Q6_K_XL | 16 | 16 |
| Qwen3.6 35B A3B (MoE) | UD Q4_K_XL | 53 | 1.3 |

## Verdict

**Confirmed root cause: `estimateTokensPerSec` (`ui/src/lib/speedEstimate.ts`)
computed `bandwidth / active_bytes` with zero awareness of whether the model
actually fits the machine.** It was called unconditionally at all three UI
sites that show a speed number (`Library.tsx`'s `ModelCard`, `ChatView.tsx`'s
`ModelQuickPicker`, `Discover.tsx`'s HF quant picker) regardless of `FitLevel`.
A separate module, `crates/core/src/disk_speed.rs`, already has a calibrated,
derated formula for exactly this situation (see `docs/design/DISK_OFFLOAD.md`), but
it was wired only into the opt-in disk-offload confirm dialog
(`probeDiskSpeed`), never into the always-on quick per-card estimate.

The two catastrophically wrong rows above (6.5x and ~40x over) are both true
sparse-MoE models (`moe: true`, small active-param fraction); the two
accurate/near-accurate rows are both dense/dense-equivalent. That pattern,
plus the numeric audit below, is consistent with these two MoE downloads not
actually fitting the machine's RAM budget — i.e. reality was dominated by
disk streaming or OS memory pressure that the estimate never modeled, not by
some exotic MoE-specific effect. See "Two hypotheses" below for the full
reasoning, including what the evidence does *not* settle.

## What's actually new here vs. `docs/design/DISK_OFFLOAD.md`

`DISK_OFFLOAD.md` and `disk_speed.rs` are entirely scoped to the opt-in
`FitLevel::DiskSlow` streaming path. Neither one mentions
`speedEstimate.ts`/`estimateTokensPerSec` at all — the always-on per-card
quick estimate this doc is about was outside that workstream's awareness
entirely. This doc complements `DISK_OFFLOAD.md` rather than duplicating it;
for the disk-streaming bench matrix and calibration history, see that file.

## Two hypotheses, adversarially investigated

**H1 — RAM-fit blindness.** The estimate never checks `FitLevel` or whether
the disk-offload toggle is even on. If a model's working set exceeds the RAM
budget, real throughput is dominated by disk streaming / OS paging while the
estimate still assumes full in-RAM bandwidth. Alone, this predicts arbitrarily
large errors for any model that doesn't fit, regardless of MoE-ness.

**H2 — Intrinsic MoE routing/dispatch overhead, present even when fully
RAM-resident.** llama.cpp's MoE forward pass does data-dependent expert
routing and gathers scattered, non-contiguous expert weight blocks — the same
phenomenon `disk_speed.rs`'s own `MEASURED_SHORTFALL_FACTOR` comment
attributes to disk I/O ("generation pages in scattered active-expert blocks
with many small random reads"), but potentially also present, in some smaller
form, purely in compute/memory-gather terms once every byte is already RAM
resident.

### Evidence gathered

- **`docs/design/DISK_OFFLOAD.md` + `disk_speed.rs` full read**: every bench point
  and every mention of "routing locality" in that doc is causally tied to
  disk streaming / page-cache-miss behavior specifically. No benchmark
  anywhere in the doc isolates a fully RAM-resident sparse-MoE model against
  a naive bandwidth/active-bytes prediction. Real evidentiary gap, not a
  refutation of H2 — that experiment was simply out of that workstream's
  scope.
- **Web research**: one real academic study (arXiv:2606.21428, OLMoE-1B-7B on
  M2 Pro/Metal, comfortably RAM-resident) measured a ~10% shortfall vs. a
  dense model of the same active-param count — real, Apple-Silicon-specific
  evidence for H2, but far too small on its own to explain 6.5x/40x gaps.
  Router/dispatch overhead was 33.5% of MoE-block time on Metal vs. 8.9% on
  CUDA in that study — Metal dispatch overhead is real and proportionally
  larger, but the net effect at OLMoE's scale was still only ~10%. No public
  benchmark was found of a large fine-grained MoE (Qwen3-30B-A3B-class or
  bigger) confirmed fully RAM-resident on a plain M4 — the single most
  relevant missing data point. Separately, a llama.cpp CPU-backend issue
  (#19480) found a genuine ~3-4x shortfall from a confirmed implementation
  gap (the CPU path not fully exploiting sparse activation) — suggestive but
  not confirmed for the Metal GPU path most Apple Silicon users actually run.
- **Independent numeric audit**: recomputing the naive formula against the
  catalog's `gemma-4-12b` entry (7.38 GB, 120 GB/s M4 bandwidth) gives 16.26
  tok/s — matches the user's reported "16" to within 1.6%, confirming the
  formula/units/hardware bucket used throughout this diagnosis. Sparsity
  (active/total fraction) correlates with error magnitude in *direction* but
  not cleanly in *magnitude*: the 26B-A4B error (6.53x) matches "MoE buys
  zero speedup" (1/sparsity ≈ 6.5x) almost exactly, but the 35B-A3B error
  (40.8x) is 3-4x larger than that same simple model predicts (≈11.7x) —
  something beyond sparsity alone is needed for the more extreme case. Across
  the only physically plausible base-M4 RAM sizes (16/24/32 GB), both MoE
  models' estimated working sets exceed the RAM budget (DiskSlow/TooBig
  territory) at 16 GB and 24 GB; only at 32 GB does the smaller MoE model
  become RAM-resident (Tight fit), at which point H1 alone could no longer
  explain its error and H2 would be required. **The user's exact RAM size was
  never stated and was the single largest swing factor in this analysis** —
  the fix below sidesteps that gap by reading real `hw.totalRamBytes` at
  runtime instead of assuming a RAM size.

### Verdict on H1 vs. H2

**Non-exclusive; H1 is the more probable dominant driver of the largest gaps,
with H2 a real but secondary contributor.** Order-of-magnitude collapses
(10-100x) are the documented signature of RAM-fit problems in
`disk_speed.rs`'s own calibration data (e.g. GLM-4.5-Air ~0.124 tok/s
measured, DeepSeek-R1 ~1.3 tok/s measured); the one clean fully-resident
Apple-Silicon MoE measurement found (OLMoE) puts H2's magnitude around 10%,
nowhere close to sufficient alone. The unresolved residual on the sparser
35B-A3B example (unexplained even by simple 1/sparsity, and even after
applying the dense-derating implied by the 12B row) is the strongest single
piece of evidence that H2-type effects are real and not fully subsumed by H1
— but with only 2 MoE data points, both size-estimated rather than measured,
this remains suggestive, not conclusive.

## The fix

`estimateTokensPerSec` now takes an optional `fit: FitLevel` — the same
`FitLevel` already computed by `selection.rs`/`get_hf_quant_picker` and
already in scope, unused, at all three call sites. No new IPC, no new
`ModelSpec`/`HardwareInfo` fields.

```
estimateTokensPerSec(spec, hw, fit?):
  bytes = activeWeightBytes(spec)                          // unchanged
  if bytes <= 0: return 0
  naive = bandwidthBytesPerSec(hw) / bytes                  // unchanged formula

  if fit is null/undefined, "perfect", or "good": return naive
  if fit is "diskSlow" or "tooBig": return 0

  // fit == "tight": resident, but crowded against the RAM budget
  cacheable = hw.totalRamBytes * 0.70          // RAM_CACHEABLE_FRACTION, = selection.rs::RAM_BUDGET_FRACTION
  missRate  = clamp(1 - cacheable / totalModelSizeBytes(spec), 0, 1)
  derate    = 1 + missRate * (6.0 - 1)         // RESIDENCY_SHORTFALL_FACTOR, = disk_speed.rs::MEASURED_SHORTFALL_FACTOR
  return naive / derate
```

Both new constants are **reused verbatim** from already-calibrated Rust
constants rather than fit fresh from the 2 available MoE data points — see
`RAM_CACHEABLE_FRACTION`/`RESIDENCY_SHORTFALL_FACTOR` in `speedEstimate.ts`
for the full rationale. `selection.rs::RAM_BUDGET_FRACTION` and
`disk_speed.rs::MEASURED_SHORTFALL_FACTOR` now cross-reference their TS
mirrors so a future retune doesn't silently drift the two apart.

Note this applies uniformly regardless of `spec.moe` — a dense model whose
working set is Tight/DiskSlow gets the same treatment, matching
`disk_speed.rs`'s own precedent (its formula uses `fraction = 1.0` for dense
models rather than special-casing them out).

### Why `diskSlow`/`tooBig` return `0`, not a derated guess

Every call site already has the idiom `speed > 0 && <show the speed line>`
(`Library.tsx`, `ChatView.tsx`, `Discover.tsx` all do this independently). So
`0` cleanly means "don't show a number" with zero new null-handling anywhere,
and it avoids showing a *second*, disagreeing numeric guess next to the real
per-machine estimate `probeDiskSpeed` already produces in the disk-offload
confirm dialog.

### Walkthrough (16 GB RAM scenario — the most common base-M4 config, and the
one the numeric audit found most consistent with the reported numbers)

| Model | fit | old estimate | new estimate | reality | old error | new error |
|---|---|---|---|---|---|---|
| 26B-A4B | tight (borderline; diskSlow at this size is also plausible) | 9.8 | ~4.3 | 1.5 | 6.5x | ~2.9x |
| 12B (dense) | good/perfect | 16 | 16 (unchanged) | 8.9 | 1.8x | 1.8x |
| E4B (dense-equiv) | good/perfect | 16 | 16 (unchanged) | 16 | 1.0x | 1.0x |
| 35B-A3B | tight (borderline; diskSlow at this size is also plausible) | 53 | ~16.6 | 1.3 | 40.8x | ~12.7x |

Honest read: this **meaningfully shrinks both MoE errors** without touching
either dense row (no evidence either dense row needs correcting — see
"Rejected alternatives"). It does **not** close either gap completely; a real
residual remains, larger for the sparser 35B-A3B case, consistent with H2
being real but secondary. If the model is actually classified `diskSlow`
rather than `tight` at the user's real RAM size, the new estimate is `0` (no
number shown at all) rather than an under/overshoot — arguably the more
honest outcome given `disk_speed.rs`'s own DeepSeek-R1 calibration point
shows this class of formula can still be off by an order of magnitude in
either direction that far into the streaming regime.

## Rejected alternatives (and why)

**A flat "real bandwidth = X% of Apple's published peak" derating constant**
was considered (it would explain the 12B row: 8.9/16.3 ≈ 55%). Rejected
because it's **directly contradicted within the same 4-point sample**: the
E4B row matched the naive, undated estimate exactly (16 vs 16, i.e. implies
100% efficiency). Shipping either number fixes one dense row and breaks the
other. Not enough evidence to pick a winner — leaving Perfect/Good/dense
estimates untouched is the more defensible default until a larger, less
self-contradictory calibration set exists.

**A sparsity-credit exponent** (`effectiveFraction = fraction ** k` for some
`k` between 0 and 1, letting sparser MoE models get progressively less credit
for their active-param advantage) was independently proposed and evaluated.
It measurably shrinks both MoE errors (a fit exponent around 0.5 gets 26B-A4B
to ~1.4x and 35B-A3B to ~6.5-7x), but was rejected for this pass because: (a)
it's explicitly a curve-fit to 2 noisy, size-estimated data points, not a
physically derived constant; (b) the real mechanism it's meant to proxy
(expert-count/routing-locality-dependent dispatch overhead) is architecture-
dependent in a way a single scalar can't capture — `gguf_meta.rs` already
parses `expert_count`/`expert_used_count` but never threads them into
`ModelSpec`/`HfQuantOption`, so a real expert-count-aware term would need new
fields mirrored Rust↔TS, which isn't justified by 2 data points that
simultaneously differ in both fraction and expert count; (c) it targets H2,
which the evidence suggests is real but secondary — RAM-fit awareness (H1) is
the higher-value, better-evidenced fix to ship first. Worth revisiting once
more real (not name-inferred) MoE bench points exist, ideally including a
confirmed fully-RAM-resident measurement.

## Residual limitations (honest, not hidden)

- The Tight-tier derating reuses a constant (`RESIDENCY_SHORTFALL_FACTOR`)
  that was calibrated for disk streaming, extrapolated into the resident
  regime on the argument that they're one continuum — not independently
  validated for the resident case. It closes roughly half the observed gap
  on the two available MoE data points, not all of it.
  Do not oversell this as "fixed" — see the walkthrough above.
- `residencyMissRate` checks raw `totalModelSizeBytes`, not `selection.rs`'s
  fuller `working_set_bytes` (+1.10 margin, + KV cache, + 700 MiB) — the
  *tier gate* (`fit === "tight"`) is exact (comes straight from Rust); only
  the *magnitude* within that tier uses this cheaper approximation, so it can
  slightly under-derate right at the tight/diskSlow boundary.
- Still doesn't model context-length-dependent attention cost, quantization
  kernel efficiency differences, or expert-count/routing-locality — all
  flagged as real but out of scope for this pass.
- Dense models below Tight fit are still shown with **zero** derating even
  though the 12B/E4B split above shows real-world dense accuracy is
  genuinely inconsistent in this app's own data — deliberately left alone
  rather than shipping a constant contradicted by its own calibration point.

## Files touched

- `ui/src/lib/speedEstimate.ts` — the formula, new constants, new helper,
  updated header doc.
- `ui/src/views/Library.tsx`, `ui/src/views/ChatView.tsx`,
  `ui/src/views/Discover.tsx` — one-line `fit` passthrough each.
- `crates/core/src/selection.rs`, `crates/core/src/disk_speed.rs` —
  doc-comment cross-references only, no behavior change.
