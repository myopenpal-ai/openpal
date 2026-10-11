/** Live "how fast would this feel" estimate for a model that's resident in
 * memory — the simple Models-tab slider's speed preview. Distinct from
 * `disk_speed.rs`/`probeDiskSpeed`'s estimate, which is for the opt-in
 * "diskSlow" tier that streams weights from storage; this is for the normal
 * case (perfect/good/tight fit) where the weights already live in RAM/VRAM.
 *
 * Token generation at batch size 1 is memory-bandwidth-bound: producing one
 * token reads every active weight once, so tok/s ≈ bandwidth / active bytes.
 * At Perfect/Good fit that's the whole formula — no evidence it needs
 * correcting there (see docs/design/SPEED_ESTIMATE_ACCURACY.md). But this used to
 * apply that same naive math regardless of whether the model actually fit
 * the machine at all, which made it wildly optimistic for MoE models whose
 * working set is Tight, DiskSlow, or TooBig — those are exactly the cases
 * `disk_speed.rs` already has a calibrated, derated formula for, just not
 * wired into this quick per-card estimate. `fit` (the same `FitLevel`
 * already computed by `selection.rs`/`get_hf_quant_picker` and already in
 * scope at every call site) now branches by regime: Perfect/Good (or no fit
 * info, for back-compat) get the unqualified number; Tight gets a
 * continuous derating as the model's footprint approaches the RAM budget,
 * reusing `disk_speed.rs`'s own calibrated constants rather than inventing
 * new ones; DiskSlow/TooBig get `0` — not a fabricated bandwidth-based
 * guess, since a real per-machine estimate for that regime already exists
 * via `probeDiskSpeed`, and every caller already hides its speed line when
 * this returns `0`. Deliberately coarse even so — real speed also depends
 * on quantization kernel, context length, and other factors this doesn't
 * model. This exists so a user can feel the *relative* trade-off while
 * picking a model, not to promise an exact number. Full diagnosis and
 * calibration rationale: docs/design/SPEED_ESTIMATE_ACCURACY.md. */

import type { FitLevel, HardwareInfo } from "./types";
import { totalModelSizeBytes } from "./format";

/** The handful of fields this estimate actually needs — structurally typed
 * (like `totalModelSizeBytes`) so it works for both a full `ModelSpec` and
 * the Hugging Face browser's `HfQuantOption` (real params/moe/activeParams
 * once `get_hf_quant_picker` has detected them, see crate::gguf_meta). */
export interface SpeedEstimateSpec {
  sizeBytes: number;
  extraShards?: { sizeBytes: number }[];
  moe: boolean;
  activeParamsB?: number;
  paramsB: number;
}

/** Apple's published bandwidth figures are decimal (SI) GB/s — e.g. M1's
 * well-known 68.25 GB/s is LPDDR4X-4266 MT/s (decimal mega) × a 128-bit bus —
 * not binary GiB/s. Converting with 1024**3 instead would inflate every
 * estimate by ~7.4%. */
const GB = 1_000_000_000;

/** Coarse unified-memory bandwidth per Apple Silicon chip, in GB/s (Apple's
 * published specs). Matched by substring, most-specific first, so "M4 Max"
 * is checked before the bare "M4" fallback. */
const APPLE_BANDWIDTH_GBPS: [string, number][] = [
  ["M1 Ultra", 800],
  ["M2 Ultra", 800],
  ["M3 Ultra", 819],
  ["M1 Max", 400],
  ["M2 Max", 400],
  ["M3 Max", 400],
  ["M4 Max", 546],
  ["M1 Pro", 200],
  ["M2 Pro", 200],
  ["M3 Pro", 150],
  ["M4 Pro", 273],
  ["M1", 68.25],
  ["M2", 100],
  ["M3", 100],
  ["M4", 120],
];

/** Unknown Apple Silicon (a future chip this table hasn't been updated for
 * yet): a mid-range guess rather than refusing to estimate at all. */
const UNKNOWN_APPLE_BANDWIDTH_GBPS = 100;

/** Conservative discrete/integrated GPU VRAM bandwidth guess when we don't
 * know the exact card (Windows/Linux with Metal-equivalent GPU support) —
 * deliberately modest so the estimate doesn't overpromise. */
const GENERIC_GPU_BANDWIDTH_GBPS = 300;

/** Conservative dual/quad-channel system-RAM bandwidth guess for CPU-only
 * generation — real desktop DDR4/DDR5 typically lands well under its rated
 * peak once overhead and non-sequential access are accounted for. */
const CPU_BANDWIDTH_GBPS = 40;

/** Share of total RAM assumed available to cache a resident model's weight
 * pages once the OS/app/engine's own overhead is accounted for — reused
 * verbatim from `selection.rs::RAM_BUDGET_FRACTION`, the same number that
 * already decided the Tight tier `fit` was computed from, so the miss rate
 * below is guaranteed to land in `[0, 1)` for exactly the regime it's used
 * in rather than drifting out of step with the tier the card is already
 * showing. Keep in sync with crates/core/src/selection.rs::RAM_BUDGET_FRACTION. */
const RAM_CACHEABLE_FRACTION = 0.7;

/** How much slower generation gets as a resident-but-Tight model's total
 * footprint approaches the cacheable RAM budget — reused verbatim from
 * `disk_speed.rs::MEASURED_SHORTFALL_FACTOR` rather than fit independently.
 * That constant was calibrated against real disk-streaming measurements
 * (GLM-4.5-Air: ~0.13 estimated vs 0.124 tok/s measured), and
 * `disk_speed.rs`'s own doc comment already frames "scattered active-expert
 * reads once the working set stops comfortably fitting" as one continuum
 * from mild residency pressure through to full disk streaming rather than
 * two unrelated phenomena — so the same shortfall ceiling is reused at both
 * ends of it, only the miss-rate input differs. This is an extrapolation of
 * a disk-streaming-calibrated constant into the resident-but-crowded
 * regime, not an independently measured number for that regime — it
 * meaningfully shrinks the error on real MoE examples without closing it
 * completely. See docs/design/SPEED_ESTIMATE_ACCURACY.md for the full walkthrough.
 * Keep in sync with crates/core/src/disk_speed.rs::MEASURED_SHORTFALL_FACTOR. */
const RESIDENCY_SHORTFALL_FACTOR = 6.0;

function appleBandwidthBytesPerSec(chip: string): number {
  for (const [name, gbps] of APPLE_BANDWIDTH_GBPS) {
    if (chip.includes(name)) return gbps * GB;
  }
  return UNKNOWN_APPLE_BANDWIDTH_GBPS * GB;
}

function bandwidthBytesPerSec(hw: HardwareInfo): number {
  if (hw.isAppleSilicon && hw.hasMetal) return appleBandwidthBytesPerSec(hw.chip);
  if (hw.hasMetal || hw.gpuVramBytes) return GENERIC_GPU_BANDWIDTH_GBPS * GB;
  return CPU_BANDWIDTH_GBPS * GB;
}

/** Weight bytes actually touched to produce one token: the active-parameter
 * share for MoE models (only the routed experts are read), the full weights
 * for dense ones. Mirrors `ModelSpec::active_fraction`'s intent in
 * crates/core/src/types.rs — computed here from the already-serialized spec
 * rather than round-tripping to Rust for a number this cheap to derive. */
function activeWeightBytes(spec: SpeedEstimateSpec): number {
  const total = totalModelSizeBytes(spec);
  if (spec.moe && spec.activeParamsB && spec.paramsB > 0) {
    return total * (spec.activeParamsB / spec.paramsB);
  }
  return total;
}

/** How much of `totalBytes` is expected to page out of the cacheable RAM
 * budget — `0` well below `RAM_CACHEABLE_FRACTION` of total RAM, rising
 * toward `1` as `totalBytes` approaches or exceeds it. Mirrors
 * `disk_speed.rs::estimated_tokens_per_sec`'s `cacheable`/`miss_rate` shape,
 * but against total RAM rather than a disk-probe scenario, and with a `0`
 * floor (not disk_speed.rs's `0.05`) — there's no "always some penalty" tax
 * to invent for a model nowhere near the budget. */
function residencyMissRate(totalBytes: number, hw: HardwareInfo): number {
  if (totalBytes <= 0) return 0;
  const cacheable = hw.totalRamBytes * RAM_CACHEABLE_FRACTION;
  return Math.min(1, Math.max(0, 1 - cacheable / totalBytes));
}

/** Rough tokens/sec estimate for running `spec` on `hw`. `fit` is the same
 * `FitLevel` already computed by `selection.rs`/`get_hf_quant_picker` and
 * already in scope at every call site — passing it lets this branch by
 * regime without re-deriving working-set/budget math here (see the header
 * comment for the reasoning). Omit it to get the unqualified bandwidth/
 * active-bytes number, same as before this existed. `0` when the spec
 * carries no size, or when `fit` says the model doesn't actually fit
 * (`diskSlow`/`tooBig`) — callers already hide their speed line on `0`. */
export function estimateTokensPerSec(spec: SpeedEstimateSpec, hw: HardwareInfo, fit?: FitLevel): number {
  const bytes = activeWeightBytes(spec);
  if (bytes <= 0) return 0;
  const naive = bandwidthBytesPerSec(hw) / bytes;

  if (fit == null || fit === "perfect" || fit === "good") return naive;
  if (fit === "diskSlow" || fit === "tooBig") return 0;

  // fit === "tight": resident, but crowded against the RAM budget — derate
  // continuously rather than cliff-dropping at the tier boundary.
  const missRate = residencyMissRate(totalModelSizeBytes(spec), hw);
  const derate = 1 + missRate * (RESIDENCY_SHORTFALL_FACTOR - 1);
  return naive / derate;
}
