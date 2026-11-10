//! Pick the best model for this machine — the heart of "it just works".
//!
//! ## Memory model (Apple Silicon unified memory)
//!
//! Estimated working set for a model at our default 8k context:
//!
//! ```text
//! working_set = size_bytes * 1.10          (weights + runtime overhead)
//!             + kv_cache(params, 8192)     (~0.4–1.5 GB depending on size; a
//!                                           simple params-based step function
//!                                           is fine: <5B: 400MB, <15B: 800MB,
//!                                           <40B: 1.2GB, else 1.5GB)
//!             + 700 MB                     (llama-server + Metal buffers)
//! ```
//!
//! The OS and the user's other apps need room too, so we only let models use
//! a budget of `total_ram * 0.70`:
//!
//! - `working_set <= budget * 0.60` → [`FitLevel::Perfect`]
//! - `working_set <= budget * 0.85` → [`FitLevel::Good`]
//! - `working_set <= budget`        → [`FitLevel::Tight`]
//! - otherwise, when the "run huge models from disk" Settings toggle is on
//!   → [`FitLevel::DiskSlow`] (dense or MoE — the toggle alone decides
//!   eligibility; the user is the judge of whether to try it, not a
//!   resident-set fit check). The `why` copy still distinguishes two cases
//!   for honesty: a MoE model whose active experts alone fit `budget`
//!   (`resident_set = size_bytes * (active_params_b / params_b) + kv_cache +
//!   700 MB`) gets the gentler "runs slowly from disk" line; everything
//!   else (dense, or MoE whose active set also doesn't fit) gets a starker
//!   one — llama.cpp's mmap loader touches every weight of a dense model
//!   every token, so streaming one bigger than RAM means reading nearly the
//!   whole file per word.
//! - otherwise                      → [`FitLevel::TooBig`]
//!
//! ## Recommendation
//!
//! Among visible models with fit >= Good whose download also fits on disk
//! (`size_bytes * 1.1 + 1 GB <= free_disk_bytes`), pick the highest
//! `quality_score`, giving Perfect fits a +8 bonus — at similar quality the
//! model that leaves more headroom is the kinder default for a beginner's
//! Mac. Ties go to the smaller (faster) model. If nothing reaches Good, fall
//! back to the best Tight fit; the smallest model (excluding `DiskSlow`
//! entries) is the last resort so `primary_model_id` is effectively always
//! Some. `DiskSlow` is opt-in and intentionally slow, so it is never picked
//! as the primary recommendation — only surfaced as a selectable option.
//!
//! ## Copy guidelines (IMPORTANT — read by total beginners)
//!
//! `why`, `headline` and `reasons` are shown verbatim in the UI:
//! - Talk about "memory", never "RAM/VRAM/unified memory".
//! - Round sizes to friendly numbers ("about 2.5 GB").
//! - Reasons explain the *benefit*, not the spec: "Runs fast on your Apple M4
//!   chip", "Leaves plenty of room for your other apps", "Downloads in a few
//!   minutes on a typical connection".
//! - Tight fit: gently suggest closing other apps, never warn scarily.
//! - MoE models (`spec.moe`): mention they're "surprisingly fast for their
//!   size".

use crate::catalog::Catalog;
use crate::types::{FitLevel, HardwareInfo, ModelSpec, RankedModel, Recommendation};

const MIB: u64 = 1024 * 1024;
const GIB: f64 = (1024u64 * 1024 * 1024) as f64;
/// Added to `quality_score` for Perfect fits when picking the primary
/// recommendation (see module docs).
const PERFECT_FIT_BONUS: i32 = 8;
/// Share of total RAM a model's working set may use — the rest is left for
/// the OS and the user's other apps. Mirrored verbatim (same literal value)
/// by `ui/src/lib/speedEstimate.ts::RAM_CACHEABLE_FRACTION`, which uses it
/// to derate the Tight-fit tok/s estimate — if this is retuned, reconsider
/// that constant too (see docs/design/SPEED_ESTIMATE_ACCURACY.md).
const RAM_BUDGET_FRACTION: f64 = 0.70;

fn ram_budget_bytes(hw: &HardwareInfo) -> f64 {
    hw.total_ram_bytes as f64 * RAM_BUDGET_FRACTION
}

/// Whether `spec`'s full weights don't fit in `hw`'s memory budget —
/// deliberately independent of the `disk_offload` Settings toggle, unlike
/// [`fit_for`]. This is the question that matters for engine-startup safety:
/// `crates/app/src/commands.rs::start_engine` uses it to decide whether to
/// force `-ngl 0`, and a model doesn't stop being dangerous to partially
/// GPU-offload just because the toggle happens to be off right now, or
/// because it's a dense/custom model that can never reach `FitLevel::DiskSlow`
/// in the first place. Metal wires the *whole* mmapped file into a residency
/// set as soon as any GPU layers are requested for a file that doesn't fit
/// (llama.cpp issue #24510) — that's a property of the file and the machine,
/// not of this app's Settings state.
pub fn exceeds_ram(spec: &ModelSpec, hw: &HardwareInfo) -> bool {
    working_set_bytes(spec) as f64 > ram_budget_bytes(hw)
}

/// How well `spec` fits `hw`, plus one plain-English sentence explaining it.
/// `disk_offload` is the user's "run huge models from disk" Settings toggle
/// (see module docs on [`FitLevel::DiskSlow`]) — `false` reproduces the
/// pre-disk-offload behavior exactly (a model that doesn't fit in memory is
/// always `TooBig`).
pub fn fit_for(
    spec: &crate::types::ModelSpec,
    hw: &HardwareInfo,
    disk_offload: bool,
) -> (FitLevel, String) {
    let working_set = working_set_bytes(spec) as f64;
    let budget = ram_budget_bytes(hw);

    let fit = if working_set <= budget * 0.60 {
        FitLevel::Perfect
    } else if working_set <= budget * 0.85 {
        FitLevel::Good
    } else if working_set <= budget {
        FitLevel::Tight
    } else if disk_offload {
        FitLevel::DiskSlow
    } else {
        FitLevel::TooBig
    };

    let ram_gb = friendly_ram_gb(hw.total_ram_bytes);
    let mut why = match fit {
        FitLevel::Perfect => format!(
            "Fits easily in your {ram_gb} GB of memory, with plenty of room left for everything else."
        ),
        FitLevel::Good => {
            format!("Fits comfortably in your {ram_gb} GB of memory with room to spare.")
        }
        FitLevel::Tight => format!(
            "Fits in your {ram_gb} GB of memory. Closing other apps while you chat will keep it feeling quick."
        ),
        // Two honesty tiers: a MoE model whose active experts alone fit
        // `budget` gets the gentler line (llama.cpp keeps only those
        // resident); everything else — dense, or MoE whose active set also
        // doesn't fit — has no such shortcut, so it's told plainly.
        FitLevel::DiskSlow if resident_set_fits(spec, budget) => {
            "Too big to fit in your memory, but it can run slowly straight from \
             disk, so answers may take several minutes."
                .to_string()
        }
        FitLevel::DiskSlow => "This doesn't fit your memory at all, so running it means reading \
                               almost the whole file from disk for every word. Expect it to be \
                               dramatically slower, often many minutes per reply."
            .to_string(),
        FitLevel::TooBig => format!(
            "Needs more memory than your computer's {ram_gb} GB, so it wouldn't run well here."
        ),
    };
    // Only for models that already fit comfortably — DiskSlow is the opposite
    // of fast, so it must never get the "surprisingly fast" bonus line.
    if spec.moe && fit > FitLevel::DiskSlow {
        why.push_str(" Its efficient design also makes it surprisingly fast for its size.");
    }

    (fit, why)
}

/// Whether `spec`'s active parameters (the part of a MoE model llama.cpp must
/// actually keep resident while the rest streams from disk) fit `budget` —
/// used only to pick the gentler of `fit_for`'s two `DiskSlow` explanations,
/// not to gate `DiskSlow` itself (the toggle alone decides eligibility now;
/// see the module docs). Always `false` for dense models and for MoE models
/// without a known active size — there's nothing to base the gentler
/// promise on, so they get the starker one instead. `ModelSpec::active_fraction`
/// guards the params_b division: every catalog entry has a real parameter
/// count today, but nothing stops a future data error, so this must never
/// silently misclassify via a `0.0 / 0.0` or divide-by-zero rather than
/// plainly falling back to the starker copy.
fn resident_set_fits(spec: &crate::types::ModelSpec, budget: f64) -> bool {
    spec.active_fraction()
        .is_some_and(|fraction| memory_estimate(spec, fraction as f64) as f64 <= budget)
}

/// Estimated memory the model needs at `resident_fraction` of its full size —
/// the shared shape behind both [`working_set_bytes`] (fraction `1.10`, all
/// weights resident) and [`resident_set_fits`] (fraction = active-parameter
/// share, MoE disk streaming): weights at that fraction, plus KV cache, plus
/// runtime overhead.
fn memory_estimate(spec: &ModelSpec, resident_fraction: f64) -> u64 {
    (spec.total_size_bytes() as f64 * resident_fraction) as u64
        + kv_cache_bytes(spec.params_b)
        + 700 * MIB
}

/// Rank every visible catalog model for this machine and pick the winner.
/// `ranked` is sorted best-first: fit level desc, then quality desc, then
/// smaller size first. `disk_offload` is the user's "run huge models from
/// disk" Settings toggle — see [`fit_for`] and [`FitLevel::DiskSlow`]; a
/// `DiskSlow` entry is never chosen as `primary_model_id`.
pub fn recommend(catalog: &Catalog, hw: &HardwareInfo, disk_offload: bool) -> Recommendation {
    struct Entry<'a> {
        spec: &'a ModelSpec,
        fit: FitLevel,
        why: String,
    }

    let mut entries: Vec<Entry> = catalog
        .visible()
        .map(|spec| {
            let (fit, why) = fit_for(spec, hw, disk_offload);
            Entry { spec, fit, why }
        })
        .collect();
    entries.sort_by(|a, b| {
        b.fit
            .cmp(&a.fit)
            .then(b.spec.quality_score.cmp(&a.spec.quality_score))
            .then(a.spec.total_size_bytes().cmp(&b.spec.total_size_bytes()))
    });

    let fits_on_disk =
        |spec: &ModelSpec| spec.total_size_bytes() as f64 * 1.1 + GIB <= hw.free_disk_bytes as f64;
    // Quality decides, but a Perfect fit gets a head start: at similar quality
    // the model that leaves more headroom is the kinder default for a
    // beginner's Mac. Ties go to the smaller (faster) model.
    let score = |e: &Entry| {
        i32::from(e.spec.quality_score)
            + if e.fit == FitLevel::Perfect {
                PERFECT_FIT_BONUS
            } else {
                0
            }
    };
    let best_at_least = |level: FitLevel| {
        entries
            .iter()
            .filter(|e| e.fit >= level && fits_on_disk(e.spec))
            .max_by(|a, b| {
                score(a)
                    .cmp(&score(b))
                    .then(b.spec.total_size_bytes().cmp(&a.spec.total_size_bytes()))
            })
    };

    let primary = best_at_least(FitLevel::Good)
        .or_else(|| best_at_least(FitLevel::Tight))
        .or_else(|| {
            // Last resort so `primary_model_id` is effectively always Some —
            // but never an opt-in tier like DiskSlow (see `is_auto_recommendable`).
            entries
                .iter()
                .filter(|e| e.fit.is_auto_recommendable())
                .min_by_key(|e| e.spec.total_size_bytes())
        });

    let headline = match primary {
        Some(e) if e.fit >= FitLevel::Good => {
            "Great news! Your computer can run a really capable AI model.".to_string()
        }
        Some(e) if e.fit == FitLevel::Tight => {
            "Your computer can run a capable AI model, but it needs most of your memory while it works."
                .to_string()
        }
        Some(_) => {
            "Your computer can run a small AI model, great for quick questions and writing help."
                .to_string()
        }
        None => "We couldn't find a model for this computer.".to_string(),
    };

    let reasons = primary
        .map(|e| reasons_for(e.spec, e.fit, hw))
        .unwrap_or_default();

    Recommendation {
        primary_model_id: primary.map(|e| e.spec.id.clone()),
        headline,
        reasons,
        ranked: entries
            .iter()
            .map(|e| RankedModel {
                model_id: e.spec.id.clone(),
                fit: e.fit,
                why: e.why.clone(),
            })
            .collect(),
    }
}

/// Plain-English bullets explaining the benefit of the chosen model.
fn reasons_for(spec: &ModelSpec, fit: FitLevel, hw: &HardwareInfo) -> Vec<String> {
    let ram_gb = friendly_ram_gb(hw.total_ram_bytes);
    let room = match fit {
        FitLevel::Perfect => "with plenty of room left for your other apps",
        FitLevel::Good => "with room to spare for your other apps",
        _ => "though closing other apps while you chat will help",
    };
    let mut reasons = vec![
        format!(
            "Your computer's {ram_gb} GB of memory is enough to run {} smoothly, {room}.",
            spec.name
        ),
        if hw.has_metal {
            format!(
                "It runs fast on your {} chip, which is built for this kind of AI.",
                hw.chip
            )
        } else {
            "It runs on your processor. Answers may take a little longer, but everything still works."
                .to_string()
        },
        format!(
            "The download is about {}, ready in a few minutes on a typical connection.",
            friendly_size(spec.total_size_bytes())
        ),
    ];
    if spec.moe {
        reasons.push(
            "Its clever, efficient design makes it surprisingly fast for its size.".to_string(),
        );
    }
    reasons
}

/// Estimated memory the model needs while running, per the module docs.
fn working_set_bytes(spec: &ModelSpec) -> u64 {
    memory_estimate(spec, 1.10)
}

/// KV-cache estimate at our default 8k context: a params-based step function.
fn kv_cache_bytes(params_b: f32) -> u64 {
    if params_b < 5.0 {
        400 * MIB
    } else if params_b < 15.0 {
        800 * MIB
    } else if params_b < 40.0 {
        1228 * MIB // ~1.2 GB
    } else {
        1536 * MIB // 1.5 GB
    }
}

/// Memory size for display: whole gigabytes ("16").
fn friendly_ram_gb(bytes: u64) -> u64 {
    (bytes as f64 / GIB).round().max(1.0) as u64
}

/// Download size for display: "about 2.5 GB" granularity.
fn friendly_size(bytes: u64) -> String {
    let gb = bytes as f64 / GIB;
    if gb >= 10.0 {
        format!("{} GB", gb.round() as u64)
    } else if gb >= 1.0 {
        let half = (gb * 2.0).round() / 2.0;
        if half.fract() == 0.0 {
            format!("{} GB", half as u64)
        } else {
            format!("{half:.1} GB")
        }
    } else {
        format!("{} MB", (bytes / MIB).max(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hw(total_ram_bytes: u64) -> HardwareInfo {
        HardwareInfo {
            chip: "Apple M4".to_string(),
            is_apple_silicon: true,
            total_ram_bytes,
            available_ram_bytes: total_ram_bytes / 2,
            cpu_cores: 10,
            has_metal: true,
            free_disk_bytes: 500_000_000_000,
            os_version: "macOS 26.5".to_string(),
            gpu_name: None,
            gpu_vram_bytes: None,
        }
    }

    #[test]
    fn sixteen_gb_machine_gets_a_mid_size_model() {
        let catalog = Catalog::load();
        // Real value reported by 16 GB Apple hardware (16 GiB), not 16e9:
        // gemma-4-12b rates "Good" here, but the Perfect-fit bonus keeps the
        // roomier qwen3-8b as the kinder default.
        let rec = recommend(&catalog, &hw(17_179_869_184), false);
        assert_eq!(rec.primary_model_id.as_deref(), Some("qwen3-8b"));

        let primary = rec
            .ranked
            .iter()
            .find(|r| Some(r.model_id.as_str()) == rec.primary_model_id.as_deref())
            .expect("primary appears in ranked");
        assert!(
            primary.fit >= FitLevel::Good,
            "primary must never be TooBig or Tight here"
        );
    }

    #[test]
    fn eight_gb_machine_gets_a_small_model() {
        let catalog = Catalog::load();
        let rec = recommend(&catalog, &hw(8_589_934_592), false); // 8 GiB
        assert_eq!(
            rec.primary_model_id.as_deref(),
            Some("qwen3-4b-instruct-2507")
        );

        let primary = rec
            .ranked
            .iter()
            .find(|r| Some(r.model_id.as_str()) == rec.primary_model_id.as_deref())
            .unwrap();
        assert_ne!(primary.fit, FitLevel::TooBig);
    }

    #[test]
    fn ranked_is_best_first_complete_and_visible_only() {
        let catalog = Catalog::load();
        let rec = recommend(&catalog, &hw(16_000_000_000), false);

        assert_eq!(rec.ranked.len(), catalog.visible().count());
        for ranked in &rec.ranked {
            let spec = catalog.get(&ranked.model_id).expect("ranked ids exist");
            assert!(!spec.hidden, "{} is hidden and must not be ranked", spec.id);
        }
        for pair in rec.ranked.windows(2) {
            assert!(
                pair[0].fit >= pair[1].fit,
                "ranked must be sorted by fit, best first"
            );
        }
    }

    #[test]
    fn all_user_facing_copy_is_present() {
        let catalog = Catalog::load();
        for total in [
            4_000_000_000u64,
            8_000_000_000,
            16_000_000_000,
            64_000_000_000,
        ] {
            let rec = recommend(&catalog, &hw(total), false);
            assert!(rec.primary_model_id.is_some());
            assert!(!rec.headline.is_empty());
            assert!(!rec.reasons.is_empty());
            assert!(rec.reasons.iter().all(|r| !r.is_empty()));
            assert!(rec.ranked.iter().all(|r| !r.why.is_empty()));
        }
    }

    #[test]
    fn tiny_machine_still_gets_a_recommendation() {
        let catalog = Catalog::load();
        let rec = recommend(&catalog, &hw(3_000_000_000), false);
        // Nothing fits, so the smallest visible model is the last resort.
        assert_eq!(rec.primary_model_id.as_deref(), Some("llama-3.2-1b"));
    }

    #[test]
    fn moe_models_mention_their_speed() {
        let catalog = Catalog::load();
        let moe = catalog.get("gpt-oss-20b").unwrap();
        let (fit, why) = fit_for(moe, &hw(64_000_000_000), false);
        assert!(fit > FitLevel::TooBig);
        assert!(why.contains("surprisingly fast for its size"));
    }

    /// A synthetic 200B-param MoE model, 80GB on disk. `active_params_b`
    /// controls how small its resident set is — callers tune it per test.
    fn synthetic_moe(active_params_b: Option<f32>) -> ModelSpec {
        ModelSpec {
            id: "test-moe".to_string(),
            name: "Test MoE".to_string(),
            family: "Test".to_string(),
            publisher: "Test".to_string(),
            description: String::new(),
            strengths: vec![],
            url: String::new(),
            file_name: "test-moe.gguf".to_string(),
            size_bytes: 80_000_000_000,
            extra_shards: vec![],
            quant: "Q4_K_M".to_string(),
            sha256: None,
            params_b: 200.0,
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
            source: crate::types::ModelSource::Catalog,
            local_path: None,
            vision: false,
            mmproj_url: None,
            mmproj_file_name: None,
            mmproj_size_bytes: None,
            mmproj_sha256: None,
            mtp: false,
            group_id: None,
            release_date: None,
        }
    }

    #[test]
    fn disk_offload_off_keeps_oversized_moe_as_toobig() {
        // 8B active out of 200B total comfortably fits the resident-set rule —
        // but the toggle is off, so this must still be TooBig, unchanged from
        // pre-disk-offload behavior.
        let spec = synthetic_moe(Some(8.0));
        let (fit, why) = fit_for(&spec, &hw(16_000_000_000), false);
        assert_eq!(fit, FitLevel::TooBig);
        assert!(!why.contains("from disk"));
    }

    #[test]
    fn disk_offload_on_classifies_fitting_resident_set_as_diskslow() {
        let spec = synthetic_moe(Some(8.0));
        let (fit, why) = fit_for(&spec, &hw(16_000_000_000), true);
        assert_eq!(fit, FitLevel::DiskSlow);
        assert!(why.contains("disk"));
        assert!(
            !why.contains("surprisingly fast"),
            "DiskSlow must never get the fast-MoE bonus line: {why}"
        );
    }

    #[test]
    fn disk_offload_helps_dense_models_too() {
        // The toggle alone decides DiskSlow eligibility now — "let the user
        // be the judge" — so a dense model reaches it just like MoE, but
        // with the starker explanation since it has no active-expert
        // shortcut to fall back on.
        let mut spec = synthetic_moe(Some(8.0));
        spec.moe = false; // otherwise identical, so only `moe` is under test
        let (fit, why) = fit_for(&spec, &hw(16_000_000_000), true);
        assert_eq!(fit, FitLevel::DiskSlow);
        assert!(
            why.contains("almost the whole file"),
            "expected the starker copy: {why}"
        );
    }

    #[test]
    fn disk_offload_still_helps_when_the_resident_set_also_exceeds_budget() {
        // 150B of the 200B total is "active" — nowhere near small enough to
        // keep resident even with disk streaming, but the toggle no longer
        // gates on that: it's still DiskSlow, just with the starker copy.
        let spec = synthetic_moe(Some(150.0));
        let (fit, why) = fit_for(&spec, &hw(16_000_000_000), true);
        assert_eq!(fit, FitLevel::DiskSlow);
        assert!(
            why.contains("almost the whole file"),
            "expected the starker copy: {why}"
        );
    }

    #[test]
    fn disk_offload_still_helps_without_a_known_active_size() {
        // moe:true but no active_params_b (today's user-added models): still
        // reaches DiskSlow once the toggle alone decides eligibility, with
        // the starker copy since there's nothing to base the gentler
        // "active experts fit" promise on.
        let spec = synthetic_moe(None);
        let (fit, why) = fit_for(&spec, &hw(16_000_000_000), true);
        assert_eq!(fit, FitLevel::DiskSlow);
        assert!(
            why.contains("almost the whole file"),
            "expected the starker copy: {why}"
        );
    }

    #[test]
    fn exceeds_ram_ignores_the_disk_offload_toggle() {
        // A model too big for RAM is too big for RAM whether or not the
        // toggle happens to be on — start_engine relies on this to keep
        // forcing -ngl 0 for an already-installed oversized model even after
        // the user flips the toggle off (or for a dense/custom model that
        // could never reach FitLevel::DiskSlow in the first place).
        let spec = synthetic_moe(Some(8.0));
        let machine = hw(16_000_000_000);
        let (fit_with_toggle_on, _) = fit_for(&spec, &machine, true);
        let (fit_with_toggle_off, _) = fit_for(&spec, &machine, false);
        assert_eq!(fit_with_toggle_on, FitLevel::DiskSlow);
        assert_eq!(fit_with_toggle_off, FitLevel::TooBig);
        // Both cases: the full model still doesn't fit in RAM.
        assert!(exceeds_ram(&spec, &machine));
    }

    #[test]
    fn exceeds_ram_is_false_for_a_model_that_fits() {
        let catalog = Catalog::load();
        let small = catalog.get("llama-3.2-1b").expect("test fixture exists");
        assert!(!exceeds_ram(small, &hw(64_000_000_000)));
    }

    #[test]
    fn exceeds_ram_stays_true_even_when_the_toggle_moves_a_dense_model_to_diskslow() {
        // A dense model can now reach FitLevel::DiskSlow with the toggle on
        // (the toggle alone decides eligibility) — but exceeds_ram is
        // deliberately independent of the toggle (see its own doc comment):
        // start_engine relies on it to keep forcing -ngl 0 for this model
        // regardless of Settings, so it must still flag it as unsafe for GPU
        // offload rather than reading "DiskSlow" as "safe".
        let mut spec = synthetic_moe(Some(8.0));
        spec.moe = false;
        let machine = hw(16_000_000_000);
        assert!(exceeds_ram(&spec, &machine));
        let (fit, _) = fit_for(&spec, &machine, true);
        assert_eq!(fit, FitLevel::DiskSlow);
    }

    #[test]
    fn disk_slow_is_never_the_primary_recommendation() {
        let catalog = Catalog::load();
        // Small enough that gpt-oss-20b's full working set doesn't fit, but
        // (once catalog.json carries its activeParamsB) its resident set does.
        let rec = recommend(&catalog, &hw(8_000_000_000), true);
        assert!(
            rec.ranked.iter().any(|r| r.fit == FitLevel::DiskSlow),
            "expected at least one DiskSlow entry at 8GB with disk offload on"
        );
        if let Some(primary_id) = &rec.primary_model_id {
            let primary_fit = rec
                .ranked
                .iter()
                .find(|r| &r.model_id == primary_id)
                .expect("primary appears in ranked")
                .fit;
            assert_ne!(
                primary_fit,
                FitLevel::DiskSlow,
                "DiskSlow must never be the primary pick"
            );
        }
    }

    /// The Phase 4 flagship: a real 42.68 GB single-file MoE catalog entry
    /// large enough to actually exercise DiskSlow end-to-end (unlike the
    /// synthetic fixtures above, or gpt-oss-20b/qwen3-30b-a3b, both of which
    /// fit normally on any machine this feature would plausibly target).
    #[test]
    fn glm_4_5_air_reaches_disk_slow_on_a_32gb_machine_but_not_without_the_toggle() {
        let catalog = Catalog::load();
        let spec = catalog
            .get("glm-4.5-air")
            .expect("catalog carries the disk-mode flagship");
        let machine = hw(32_000_000_000);

        let (fit_with_toggle, _) = fit_for(spec, &machine, true);
        let (fit_without_toggle, _) = fit_for(spec, &machine, false);
        assert_eq!(fit_with_toggle, FitLevel::DiskSlow);
        assert_eq!(fit_without_toggle, FitLevel::TooBig);

        // The safety behavior in start_engine (forcing -ngl 0) must still
        // trigger for this model even when the toggle is off — this is
        // exactly the case exceeds_ram exists to catch.
        assert!(exceeds_ram(spec, &machine));
    }

    /// Fit math must use the combined size across every shard, not shard 1
    /// alone — otherwise a split model could be misjudged as fitting RAM
    /// comfortably from its first (small) shard, and `start_engine` would
    /// then skip the `-ngl 0` safety recipe for a file that's actually far
    /// bigger than RAM once every shard is counted (llama.cpp issue #24510).
    #[test]
    fn fit_math_counts_every_shard_not_just_shard_one() {
        let mut spec = synthetic_moe(Some(8.0));
        spec.size_bytes = 10_000_000_000; // shard 1 alone: a comfortably small 10 GB
        spec.extra_shards = vec![
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
        // 80 GB total (matching the unsharded synthetic_moe fixture above) on
        // a 64 GB machine: shard 1 alone (10 GB) would rate Perfect; the true
        // 80 GB total does not fit in RAM at all.
        let machine = hw(64_000_000_000);
        assert!(
            exceeds_ram(&spec, &machine),
            "80 GB across 3 shards must not fit a 64 GB machine's budget"
        );

        let (fit, why) = fit_for(&spec, &machine, true);
        assert_eq!(
            fit,
            FitLevel::DiskSlow,
            "must reach DiskSlow via the resident-set rule, not Perfect/Good from shard 1 alone"
        );
        assert!(why.contains("disk"));
    }
}
