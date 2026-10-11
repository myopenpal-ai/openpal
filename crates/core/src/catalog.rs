//! The curated model catalog, embedded at compile time.
//!
//! Every entry in `data/catalog.json` has been verified: the URL is a public,
//! single-file GGUF download and `sizeBytes` matches the host's Content-Length.
//! `moe`/`paramsB`/`activeParamsB` have also been checked against each
//! model's official source — see `docs/CATALOG_MODEL_SPECS.md` for the
//! per-entry citations; re-verify there before hand-editing those fields.
//! Quality scores are hand-curated relative rankings used by [`crate::selection`].

use crate::types::{general_sampling_defaults, ModelSpec, SamplingParams, TaskType};

const CATALOG_JSON: &str = include_str!("../data/catalog.json");

#[derive(Debug, Clone)]
pub struct Catalog {
    models: Vec<ModelSpec>,
}

impl Catalog {
    /// Parse the embedded catalog. Panics only if the embedded JSON is invalid,
    /// which is covered by a unit test and therefore cannot happen at runtime.
    pub fn load() -> Self {
        let models: Vec<ModelSpec> =
            serde_json::from_str(CATALOG_JSON).expect("embedded catalog.json is valid");
        Self { models }
    }

    /// All models, including hidden test models.
    pub fn all(&self) -> &[ModelSpec] {
        &self.models
    }

    /// Models that should appear in the UI.
    pub fn visible(&self) -> impl Iterator<Item = &ModelSpec> {
        self.models.iter().filter(|m| !m.hidden)
    }

    pub fn get(&self, id: &str) -> Option<&ModelSpec> {
        self.models.iter().find(|m| m.id == id)
    }

    /// Recommended sampling defaults for `model_id` at `task_type`: when
    /// `show_thinking` is true and the model has a curated thinking-mode
    /// entry for this task ([`ModelSpec::task_defaults_thinking`]), that
    /// wins; otherwise its regular (instruct/non-thinking) override
    /// ([`ModelSpec::task_defaults`]) when the catalog has one for this
    /// task type; otherwise the model-agnostic general default
    /// ([`general_sampling_defaults`]). A `model_id` outside the catalog
    /// (Hugging Face / local-file models, which carry no `ModelSpec` here)
    /// gets the general default outright, same as a catalog entry with no
    /// override of its own — mirrored in the frontend's
    /// `recommendedSampling` (`ui/src/lib/types.ts`).
    pub fn recommended_sampling(
        &self,
        model_id: &str,
        task_type: TaskType,
        show_thinking: bool,
    ) -> SamplingParams {
        let Some(model) = self.get(model_id) else {
            return general_sampling_defaults(task_type);
        };
        if show_thinking {
            if let Some(sampling) = model.task_defaults_thinking.get(&task_type) {
                return *sampling;
            }
        }
        model
            .task_defaults
            .get(&task_type)
            .copied()
            .unwrap_or_else(|| general_sampling_defaults(task_type))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_catalog_parses_and_is_sane() {
        let catalog = Catalog::load();
        assert!(
            catalog.visible().count() >= 5,
            "catalog should cover all RAM tiers"
        );
        for m in catalog.all() {
            assert!(m.url.starts_with("https://"), "{}: url must be https", m.id);
            assert!(m.size_bytes > 0, "{}: sizeBytes must be set", m.id);
            assert!(m.file_name.ends_with(".gguf"), "{}: must be a gguf", m.id);
            assert!(!m.description.is_empty(), "{}: needs a description", m.id);
        }
        // ids are unique
        let mut ids: Vec<_> = catalog.all().iter().map(|m| &m.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), catalog.all().len(), "duplicate model ids");
    }

    /// A catalog entry's `extraArgs` must never set GPU layers itself:
    /// `start_engine` (crates/app/src/commands.rs) appends `extraArgs` after
    /// its own `-ngl <cfg.gpu_layers>`, so a per-model `-ngl` would silently
    /// win — and for a MoE model running in "disk slow" mode, that safety
    /// override (`-ngl 0`, avoiding a Metal OOM per llama.cpp issue #24510)
    /// existing to keep it correct at all depends on this never happening.
    #[test]
    fn no_catalog_entry_sets_its_own_gpu_layers() {
        let catalog = Catalog::load();
        for m in catalog.all() {
            assert!(
                !m.extra_args
                    .iter()
                    .any(|a| a == "-ngl" || a == "--n-gpu-layers"),
                "{}: extraArgs must not set GPU layers — that's EngineConfig.gpu_layers' job",
                m.id
            );
        }
    }

    /// Every shard of a split catalog entry must be a real `.gguf` file and
    /// share the same "-NNNNN-of-MMMMM" stem as shard 1 — llama.cpp's
    /// auto-shard-discovery relies on that when it opens shard 2..N given
    /// only shard 1's path.
    #[test]
    fn catalog_shards_are_well_formed_and_share_shard_ones_stem() {
        let catalog = Catalog::load();
        for m in catalog.all() {
            if m.extra_shards.is_empty() {
                continue;
            }
            assert!(
                m.file_name.ends_with(".gguf"),
                "{}: shard 1 must be a gguf",
                m.id
            );
            let stem1 = m
                .file_name
                .rsplit_once("-of-")
                .map(|(s, _)| s.rsplit_once('-').map(|(s, _)| s));
            for shard in &m.extra_shards {
                assert!(
                    shard.file_name.ends_with(".gguf"),
                    "{}: extraShards must be gguf files",
                    m.id
                );
                let stem_n = shard
                    .file_name
                    .rsplit_once("-of-")
                    .map(|(s, _)| s.rsplit_once('-').map(|(s, _)| s));
                assert_eq!(
                    stem1, stem_n,
                    "{}: shard {} must share shard 1's stem for llama.cpp auto-discovery",
                    m.id, shard.file_name
                );
            }
        }
    }

    /// Every hand-typed `taskDefaults`/`taskDefaultsThinking` override must
    /// stay within the sampling parameters' sane ranges — the same ranges
    /// the frontend's sliders in `ChatToolbar.tsx` clamp to. Catches a
    /// fat-fingered value (e.g. a `topP` > 1) that would otherwise only
    /// surface as a confusing generation bug.
    #[test]
    fn catalog_task_defaults_use_sane_sampling_ranges() {
        let catalog = Catalog::load();
        for m in catalog.all() {
            for (task_type, sampling) in m.task_defaults.iter().chain(&m.task_defaults_thinking) {
                let label = format!("{}/{:?}", m.id, task_type);
                assert!(
                    (0.0..=2.0).contains(&sampling.temperature),
                    "{label}: temperature out of range"
                );
                assert!(
                    (0.0..=1.0).contains(&sampling.top_p),
                    "{label}: topP out of range"
                );
                assert!(sampling.top_k >= 0, "{label}: topK must not be negative");
                assert!(
                    (0.0..=1.0).contains(&sampling.min_p),
                    "{label}: minP out of range"
                );
                assert!(
                    sampling.repeat_penalty >= 1.0,
                    "{label}: repeatPenalty below 1.0 would encourage repetition"
                );
            }
        }
    }

    /// Every visible catalog entry must carry a hand-curated task-suitability
    /// score for every `TaskType` — `ui/src/lib/triage.ts`'s
    /// `bestModelForTag` (consulted only when the opt-in `smart-model-router`
    /// plugin is enabled) otherwise silently treats a missing task type as a
    /// neutral 0, which would quietly favor whichever real entry got curated
    /// and leave a newly-added catalog model never suggested for any task.
    #[test]
    fn every_visible_catalog_entry_has_all_task_scores() {
        let catalog = Catalog::load();
        for m in catalog.visible() {
            for task_type in TaskType::ALL {
                assert!(
                    m.task_scores.contains_key(&task_type),
                    "{}: missing taskScores.{}",
                    m.id,
                    task_type.wire()
                );
            }
        }
    }

    /// A curated model's own override wins over the general default.
    #[test]
    fn recommended_sampling_prefers_the_models_own_curated_override() {
        let catalog = Catalog::load();
        let overridden = catalog
            .all()
            .iter()
            .find(|m| !m.task_defaults.is_empty())
            .expect("at least one curated entry has a taskDefaults override");
        let recommended = catalog.recommended_sampling(&overridden.id, TaskType::General, false);
        assert_eq!(
            recommended,
            *overridden.task_defaults.get(&TaskType::General).unwrap()
        );
    }

    /// A model outside the catalog (Hugging Face / local-file) always falls
    /// back to the general default — there's no `ModelSpec` for it to carry
    /// an override on.
    #[test]
    fn recommended_sampling_falls_back_to_general_for_an_uncatalogued_model() {
        let catalog = Catalog::load();
        assert_eq!(
            catalog.recommended_sampling("not-a-real-model-id", TaskType::General, true),
            general_sampling_defaults(TaskType::General)
        );
    }

    /// When `show_thinking` is on, a model's thinking-mode override for a
    /// task takes priority over its regular (instruct/non-thinking) one —
    /// the two curated Qwen "hybrid" lines (Qwen3.5, Qwen3.6) rely on this
    /// to get different sampling depending on whether reasoning is showing.
    #[test]
    fn recommended_sampling_prefers_the_thinking_table_when_thinking_is_on() {
        let catalog = Catalog::load();
        let hybrid = catalog
            .all()
            .iter()
            .find(|m| !m.task_defaults_thinking.is_empty())
            .expect("at least one curated entry has a taskDefaultsThinking override");
        let task_type = *hybrid
            .task_defaults_thinking
            .keys()
            .next()
            .expect("non-empty map has a key");

        let thinking = catalog.recommended_sampling(&hybrid.id, task_type, true);
        assert_eq!(
            thinking,
            *hybrid.task_defaults_thinking.get(&task_type).unwrap()
        );

        // With thinking off, the same model/task must NOT use the thinking
        // table — it falls to `task_defaults`/the generic default instead.
        let not_thinking = catalog.recommended_sampling(&hybrid.id, task_type, false);
        assert_ne!(thinking, not_thinking);
    }
}
