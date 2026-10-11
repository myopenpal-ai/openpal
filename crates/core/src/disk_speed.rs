//! Measure the model-install volume's write throughput as a stand-in for its
//! read speed, to gate and set expectations for `FitLevel::DiskSlow` —
//! llama.cpp streaming a bigger-than-RAM MoE model's inactive experts from
//! disk. See `docs/design/DISK_OFFLOAD.md` Phase 3.
//!
//! ## Why a write probe, not a read probe
//!
//! The plan calls for probing "at download-confirm time" — before the model
//! file exists on the target volume, so there's nothing of the model's own to
//! read yet. And a read probe of *any* freshly-written file would mostly
//! measure the OS page cache, not the disk: right after writing, those pages
//! are almost certainly still hot in RAM. Writing a throwaway probe file and
//! `fsync`-ing it forces the data to physical storage before the clock stops,
//! sidestepping the cache entirely. Write and read bandwidth aren't identical
//! (e.g. QLC SSDs slow down once an SLC write cache is exhausted, and some
//! USB bridges are asymmetric), but it's an honest, cache-proof measurement
//! of "how fast is this volume". Callers should cache the result for the
//! life of the process rather than re-probe on every use — the model install
//! directory doesn't move without an app relaunch (see
//! `crates/app/src/commands.rs`'s `cached_volume_speed`).

use crate::types::{DiskSpeedClass, DiskSpeedInfo, ModelSpec};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// How much to write for the probe — big enough to smooth out a drive's
/// burst-write cache (many SSDs/USB sticks report misleadingly high speeds
/// for the first tens of MB), small enough to add well under a few seconds on
/// any drive worth offering disk mode on in the first place.
pub const PROBE_BYTES: u64 = 256 * 1024 * 1024;

const PROBE_CHUNK_BYTES: usize = 1024 * 1024;

/// At or above this: internal NVMe / USB4 / Thunderbolt territory (see
/// `docs/design/DISK_OFFLOAD.md`'s verified bandwidth notes).
const FAST_FLOOR_BYTES_PER_SEC: f64 = 2.0 * 1024.0 * 1024.0 * 1024.0;
/// Below this: not worth offering disk mode at all — `docs/design/DISK_OFFLOAD.md`'s
/// "Refuse (keep TooBig) below a floor" design decision.
const REFUSE_FLOOR_BYTES_PER_SEC: f64 = 0.5 * 1024.0 * 1024.0 * 1024.0;

fn classify(bytes_per_sec: f64) -> DiskSpeedClass {
    if bytes_per_sec >= FAST_FLOOR_BYTES_PER_SEC {
        DiskSpeedClass::Fast
    } else if bytes_per_sec >= REFUSE_FLOOR_BYTES_PER_SEC {
        DiskSpeedClass::Slow
    } else {
        DiskSpeedClass::TooSlow
    }
}

/// Process-unique probe file names — several probes can legitimately race
/// (a download-confirm probe from the UI, an engine-start probe for a
/// different model), and each must clean up only its own file.
static PROBE_COUNTER: AtomicU64 = AtomicU64::new(0);

fn probe_file_path(dir: &Path) -> PathBuf {
    let n = PROBE_COUNTER.fetch_add(1, Ordering::Relaxed);
    dir.join(format!(
        ".openpal-diskspeed-probe-{}-{n}.tmp",
        std::process::id()
    ))
}

/// Write `total_bytes` to `path` in chunks and `fsync` before returning, so
/// the caller's elapsed time reflects real storage I/O, not the page cache.
/// The fill byte is non-zero: some filesystems special-case all-zero writes
/// as sparse holes and never touch the disk for them, which would make a
/// write probe measure nothing.
fn write_and_sync(path: &Path, total_bytes: u64) -> crate::Result<()> {
    let mut file = std::fs::File::create(path)?;
    let chunk = vec![0xA5u8; PROBE_CHUNK_BYTES];
    let mut written = 0u64;
    while written < total_bytes {
        let n = ((total_bytes - written) as usize).min(PROBE_CHUNK_BYTES);
        file.write_all(&chunk[..n])?;
        written += n as u64;
    }
    file.sync_all()?;
    Ok(())
}

fn probe_volume_speed_sized(dir: &Path, total_bytes: u64) -> crate::Result<DiskSpeedInfo> {
    let path = probe_file_path(dir);
    let start = std::time::Instant::now();
    let result = write_and_sync(&path, total_bytes);
    let elapsed = start.elapsed();
    // Best-effort cleanup either way — a probe must never leave litter
    // behind, not even when the write itself failed partway through. Not
    // fatal on its own (e.g. a transient AV/indexer file lock on Windows)
    // but worth a log line so an accumulation of stray probe files has a
    // findable cause.
    if let Err(e) = std::fs::remove_file(&path) {
        tracing::warn!(path = %path.display(), "failed to clean up disk-speed probe file: {e}");
    }
    result?;
    // Floor the elapsed time: an implausibly fast write (a tiny probe size in
    // tests, or a tmpfs-backed dir) would otherwise divide by (near) zero.
    let seconds = elapsed.as_secs_f64().max(0.001);
    let bytes_per_sec = total_bytes as f64 / seconds;
    Ok(DiskSpeedInfo {
        bytes_per_sec,
        class: classify(bytes_per_sec),
        estimated_tokens_per_sec: None,
        calibrated: false,
    })
}

/// Probe `dir`'s volume (must already exist — the app's models directory is
/// created at startup) by writing and `fsync`-ing [`PROBE_BYTES`] of data.
/// Blocking I/O — call from `spawn_blocking`.
pub fn probe_volume_speed(dir: &Path) -> crate::Result<DiskSpeedInfo> {
    probe_volume_speed_sized(dir, PROBE_BYTES)
}

/// The write probe measures sequential write bandwidth, but generation pages
/// in scattered active-expert blocks with many small random reads — the
/// 2026-07-08 real-hardware bench (16 GB M4, GLM-4.5-Air UD-IQ2_XXS 42.7 GB,
/// probe 2.38 GB/s, measured **0.124 tok/s**; see `docs/design/DISK_OFFLOAD.md`'s
/// bench matrix) showed the raw bandwidth-over-active-bytes formula is far
/// too optimistic. This divisor was fit to that measurement (with the
/// page-cache miss rate below already modeled). One data point, so it stays
/// a coarse, deliberately conservative constant; revisit as the bench matrix
/// grows. Mirrored verbatim (same literal value) by
/// `ui/src/lib/speedEstimate.ts::RESIDENCY_SHORTFALL_FACTOR`, which
/// extrapolates it into the resident-but-Tight-fit regime for the quick
/// per-card estimate — if this is retuned from new bench data, reconsider
/// that constant too (see docs/design/SPEED_ESTIMATE_ACCURACY.md).
const MEASURED_SHORTFALL_FACTOR: f64 = 6.0;

/// RAM assumed unavailable for page-caching model weights: OS, the app, the
/// engine's own anonymous memory (KV cache, activations). Deliberately
/// coarse — same spirit as `selection.rs`'s fixed overhead constant.
const RESERVED_NON_CACHE_BYTES: u64 = 4 * 1024 * 1024 * 1024;

/// A model never streams *every* active byte from disk — some fraction of
/// the file always stays page-cached. Floor the modeled miss rate so a
/// nearly-fully-cacheable model doesn't produce an absurdly high estimate.
const MIN_PAGE_MISS_RATE: f64 = 0.05;

/// Honest tok/s estimate for `spec` streaming from a volume measured at
/// `bytes_per_sec` on a machine with `total_ram_bytes` of RAM: every token
/// needs roughly its active experts' worth of weight bytes read, but only
/// the fraction that *isn't* page-cached actually hits the disk — so
/// throughput ≈ `bw / (active_bytes × miss_rate)`, derated by
/// [`MEASURED_SHORTFALL_FACTOR`] because the sequential write probe
/// systematically overstates the random-read bandwidth generation actually
/// gets. Modeling the miss rate matters in both directions: without it, the
/// far-over-RAM case reads ~4× too fast and the *nearly-fits* case — the one
/// disk offload is actually good at — reads 10-30× too slow, scaring users
/// away from the models that work best. `None` only when `spec` carries no
/// size at all. A dense model (or an MoE model without a known active size,
/// see [`ModelSpec::active_fraction`]) has no active-expert shortcut — every
/// weight is touched every token — so the whole file stands in for "active
/// bytes" instead; the returned `bool` is `true` only when the real
/// active-expert share was used, i.e. only that case is actually calibrated
/// against the benchmarks below. The dense/unknown-active fallback is an
/// unvalidated extrapolation of the same formula — treat it as rougher still.
///
/// Calibration points (all in `docs/design/DISK_OFFLOAD.md`):
/// - GLM-4.5-Air, 16 GB M4, 2.38 GB/s probe: estimate ~0.13 vs 0.124 tok/s
///   measured (the shortfall factor was fit to this point).
/// - gpt-oss-20b near-fit (12.11 GB, same 16 GB M4): estimate ~4.1 vs
///   **3.68 tok/s measured** (2026-07-11) — the first same-machine near-fit
///   measurement, and it landed within ~11% *without* refitting, despite
///   gpt-oss's flat expert routing being the modeled worst case.
/// - Qwen3.5-35B-A3B-class near-fit (13 GB file, 16 GB M4-class): estimate
///   ~7.6 vs 17.3 tok/s reported — conservative, right side of the tale.
/// - ubergarm's DeepSeek-R1 (212 GB, ~5.5% active, 96 GB RAM, ~4 GB/s):
///   estimate ~0.12 vs ~1.3 tok/s measured — an order of magnitude
///   pessimistic (expert-routing locality isn't modeled). Treat the whole
///   thing as a set-expectations-low floor, not a promise.
///
/// (The plan doc's own draft formula, `bw × active_fraction / size`, was off
/// by roughly `1 / active_fraction²` — corrected here to
/// `bw / (active_fraction × size)`, i.e. bandwidth over active-weight bytes.)
pub fn estimated_tokens_per_sec(
    bytes_per_sec: f64,
    spec: &ModelSpec,
    total_ram_bytes: u64,
) -> Option<(f64, bool)> {
    let calibrated = spec.active_fraction().is_some();
    let fraction = spec.active_fraction().unwrap_or(1.0);
    let total_bytes = spec.total_size_bytes() as f64;
    let active_bytes = total_bytes * fraction as f64;
    if active_bytes <= 0.0 {
        return None;
    }
    let cacheable = total_ram_bytes.saturating_sub(RESERVED_NON_CACHE_BYTES) as f64;
    let miss_rate = (1.0 - cacheable / total_bytes).clamp(MIN_PAGE_MISS_RATE, 1.0);
    let estimate = bytes_per_sec / (active_bytes * miss_rate) / MEASURED_SHORTFALL_FACTOR;
    Some((estimate, calibrated))
}

fn probe_for_model_sized(
    dir: &Path,
    spec: &ModelSpec,
    total_ram_bytes: u64,
    total_bytes: u64,
) -> crate::Result<DiskSpeedInfo> {
    let mut info = probe_volume_speed_sized(dir, total_bytes)?;
    if let Some((estimate, calibrated)) =
        estimated_tokens_per_sec(info.bytes_per_sec, spec, total_ram_bytes)
    {
        info.estimated_tokens_per_sec = Some(estimate);
        info.calibrated = calibrated;
    }
    Ok(info)
}

/// [`probe_volume_speed`] plus [`estimated_tokens_per_sec`] for `spec` — the
/// shape the `probe_disk_speed` command returns to the UI.
pub fn probe_for_model(
    dir: &Path,
    spec: &ModelSpec,
    total_ram_bytes: u64,
) -> crate::Result<DiskSpeedInfo> {
    probe_for_model_sized(dir, spec, total_ram_bytes, PROBE_BYTES)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ModelSource;

    #[test]
    fn classifies_at_the_documented_thresholds() {
        assert_eq!(
            classify(2.0 * 1024.0 * 1024.0 * 1024.0),
            DiskSpeedClass::Fast
        );
        assert_eq!(
            classify(3.5 * 1024.0 * 1024.0 * 1024.0),
            DiskSpeedClass::Fast
        );
        assert_eq!(
            classify(0.5 * 1024.0 * 1024.0 * 1024.0),
            DiskSpeedClass::Slow
        );
        assert_eq!(
            classify(1.0 * 1024.0 * 1024.0 * 1024.0),
            DiskSpeedClass::Slow
        );
        assert_eq!(
            classify(0.5 * 1024.0 * 1024.0 * 1024.0 - 1.0),
            DiskSpeedClass::TooSlow
        );
        assert_eq!(classify(1_000_000.0), DiskSpeedClass::TooSlow);
    }

    #[test]
    fn probe_measures_a_real_write_and_cleans_up_the_temp_file() {
        let dir = std::env::temp_dir().join(format!(
            "openpal-disk-speed-test-{}-{}",
            std::process::id(),
            PROBE_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();

        // A small size keeps the test fast while still exercising the real
        // write+fsync+measure+cleanup path.
        let info = probe_volume_speed_sized(&dir, 4 * 1024 * 1024).expect("probe should succeed");
        assert!(info.bytes_per_sec > 0.0);
        assert!(info.estimated_tokens_per_sec.is_none());

        let leftover: Vec<_> = std::fs::read_dir(&dir).unwrap().collect();
        assert!(
            leftover.is_empty(),
            "probe must delete its temp file: {leftover:?}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn probe_errors_on_a_missing_directory() {
        let dir = std::env::temp_dir().join("openpal-disk-speed-test-does-not-exist");
        std::fs::remove_dir_all(&dir).ok();
        assert!(probe_volume_speed_sized(&dir, 4096).is_err());
    }

    fn moe_spec(size_bytes: u64, params_b: f32, active_params_b: Option<f32>) -> ModelSpec {
        ModelSpec {
            id: "test-moe".to_string(),
            name: "Test MoE".to_string(),
            family: "Test".to_string(),
            publisher: "Test".to_string(),
            description: String::new(),
            strengths: vec![],
            url: String::new(),
            file_name: "test-moe.gguf".to_string(),
            size_bytes,
            extra_shards: vec![],
            quant: "Q4_K_M".to_string(),
            sha256: None,
            params_b,
            moe: true,
            active_params_b,
            context_max: 8192,
            quality_score: 50,
            license: "Apache 2.0".to_string(),
            extra_args: vec![],
            hidden: false,
            curated: false,
            task_defaults: std::collections::HashMap::new(),
            task_defaults_thinking: std::collections::HashMap::new(),
            task_scores: std::collections::HashMap::new(),
            source: ModelSource::Catalog,
            local_path: None,
            vision: false,
            mmproj_url: None,
            mmproj_file_name: None,
            mmproj_size_bytes: None,
            mmproj_sha256: None,
            group_id: None,
            release_date: None,
        }
    }

    const RAM_16GB: u64 = 16 * 1024 * 1024 * 1024;
    const RAM_96GB: u64 = 96 * 1024 * 1024 * 1024;

    #[test]
    fn estimate_matches_the_order_of_magnitude_of_measured_deepseek_r1_streaming() {
        // docs/design/DISK_OFFLOAD.md: ubergarm measured ~1.3 tok/s for DeepSeek-R1
        // (212 GB, ~5.5% active) on a 96 GB RAM box with a Gen5 NVMe at
        // ~4 GB/s effective bandwidth. The estimate lands ~0.12 —
        // deliberately pessimistic (routing locality isn't modeled), but
        // within the accepted set-expectations-low band.
        let spec = moe_spec(212_000_000_000, 671.0, Some(37.0));
        let bytes_per_sec = 4.0 * 1024.0 * 1024.0 * 1024.0;
        let (estimate, calibrated) =
            estimated_tokens_per_sec(bytes_per_sec, &spec, RAM_96GB).unwrap();
        assert!(
            calibrated,
            "a known active size must use the calibrated path"
        );
        assert!(
            (0.05..2.0).contains(&estimate),
            "expected the same order of magnitude as ~1.3 tok/s measured, got {estimate}"
        );
    }

    #[test]
    fn estimate_is_calibrated_to_the_measured_glm_4_5_air_bench() {
        // The 2026-07-08 real-hardware bench (docs/design/DISK_OFFLOAD.md): 16 GB M4,
        // probe measured 2.38 GB/s, GLM-4.5-Air UD-IQ2_XXS (42.68 GB, 106B
        // total / 12B active) generated at 0.124 tok/s. The estimate must
        // stay in that measured neighborhood, not the raw formula's ~4×
        // optimistic 0.49.
        let spec = moe_spec(42_682_240_064, 106.0, Some(12.0));
        let bytes_per_sec = 2.38 * 1024.0 * 1024.0 * 1024.0;
        let (estimate, _) = estimated_tokens_per_sec(bytes_per_sec, &spec, RAM_16GB).unwrap();
        assert!(
            (0.06..0.25).contains(&estimate),
            "expected the estimate near the measured 0.124 tok/s, got {estimate}"
        );
    }

    #[test]
    fn estimate_is_calibrated_to_the_measured_gpt_oss_20b_bench() {
        // The 2026-07-11 real-hardware bench (docs/design/DISK_OFFLOAD.md): same
        // 16 GB M4 and 2.38 GB/s probe reading, gpt-oss-20b mxfp4 (12.11 GB,
        // ~20.9B total / 3.6B active) generated at 3.68 tok/s under the
        // exact DiskSlow recipe. The first *measured* (not reported)
        // near-fit point — the estimate (~4.1) landed within ~11% without
        // refitting, and must stay in that neighborhood.
        let spec = moe_spec(12_109_566_560, 20.9, Some(3.6));
        let bytes_per_sec = 2.38 * 1024.0 * 1024.0 * 1024.0;
        let (estimate, _) = estimated_tokens_per_sec(bytes_per_sec, &spec, RAM_16GB).unwrap();
        assert!(
            (1.8..8.0).contains(&estimate),
            "expected the estimate near the measured 3.68 tok/s, got {estimate}"
        );
    }

    #[test]
    fn estimate_does_not_scare_users_away_from_nearly_fitting_models() {
        // The sweet spot disk offload is actually good at: a 13 GB MoE on a
        // 16 GB machine (Qwen3.5-35B-A3B-class; 17.3 tok/s reported on an
        // M4 Mini — docs/design/DISK_OFFLOAD.md's speed-up levers section). The
        // page-cache miss-rate term must keep this estimate in interactive
        // territory instead of the pure-streaming formula's ~0.5 tok/s.
        let spec = moe_spec(13_000_000_000, 35.0, Some(3.0));
        let bytes_per_sec = 2.38 * 1024.0 * 1024.0 * 1024.0;
        let (estimate, _) = estimated_tokens_per_sec(bytes_per_sec, &spec, RAM_16GB).unwrap();
        assert!(
            (2.0..20.0).contains(&estimate),
            "expected an interactive-range estimate for a near-fit MoE, got {estimate}"
        );
    }

    #[test]
    fn dense_models_get_an_uncalibrated_fallback_estimate() {
        // No active-expert shortcut, so the whole file stands in for "active
        // bytes" — still produces an estimate (the toggle no longer requires
        // MoE-ness), just flagged as not calibrated against the benchmarks.
        let mut spec = moe_spec(80_000_000_000, 200.0, Some(8.0));
        spec.moe = false;
        let (_, calibrated) = estimated_tokens_per_sec(1e9, &spec, RAM_16GB).unwrap();
        assert!(!calibrated);
    }

    #[test]
    fn moe_without_a_known_active_size_also_gets_an_uncalibrated_fallback_estimate() {
        let spec = moe_spec(80_000_000_000, 200.0, None);
        let (_, calibrated) = estimated_tokens_per_sec(1e9, &spec, RAM_16GB).unwrap();
        assert!(!calibrated);
    }

    #[test]
    fn estimate_counts_every_shard_not_just_shard_one() {
        // Same 80 GB total as the unsharded fixtures above, split across 3
        // shards — the estimate must come out identical either way, proving
        // it's driven by `total_size_bytes()` rather than `size_bytes` alone.
        let whole = moe_spec(80_000_000_000, 200.0, Some(8.0));
        let mut split = moe_spec(10_000_000_000, 200.0, Some(8.0));
        split.extra_shards = vec![
            crate::types::ModelShard {
                url: String::new(),
                file_name: "test-moe-00002-of-00003.gguf".to_string(),
                size_bytes: 35_000_000_000,
                sha256: None,
            },
            crate::types::ModelShard {
                url: String::new(),
                file_name: "test-moe-00003-of-00003.gguf".to_string(),
                size_bytes: 35_000_000_000,
                sha256: None,
            },
        ];
        let bytes_per_sec = 4.0 * 1024.0 * 1024.0 * 1024.0;
        assert_eq!(
            estimated_tokens_per_sec(bytes_per_sec, &whole, RAM_16GB),
            estimated_tokens_per_sec(bytes_per_sec, &split, RAM_16GB)
        );
    }

    #[test]
    fn probe_for_model_fills_in_the_estimate() {
        let dir = std::env::temp_dir().join(format!(
            "openpal-disk-speed-test-for-model-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let spec = moe_spec(80_000_000_000, 200.0, Some(8.0));

        let info = probe_for_model_sized(&dir, &spec, RAM_16GB, 4 * 1024 * 1024)
            .expect("probe should succeed");
        assert!(info.estimated_tokens_per_sec.is_some());

        std::fs::remove_dir_all(&dir).ok();
    }
}
