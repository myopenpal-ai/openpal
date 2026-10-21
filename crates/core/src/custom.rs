//! User-added models (Advanced settings): any Hugging Face GGUF or a GGUF
//! file already on disk.
//!
//! They are stored as a plain JSON array of [`ModelSpec`]s in the app-wide
//! database's `custom_models` table (`crate::app_db`, single `'current'`
//! row — same singleton-row idiom as `crate::settings`), so everything
//! downstream (status, fit, download, engine) treats them exactly like
//! catalog entries. Differences from the catalog:
//!
//! - ids are generated (`custom-…`) and never collide with each other or the
//!   catalog (the caller passes the taken ids).
//! - Hugging Face models download into the models directory under a file name
//!   derived from their id, so two repos offering the same file name can't
//!   overwrite each other.
//! - Local-file models run from where they are (`spec.local_path`); OpenPal
//!   never copies, moves, or deletes the user's own file.
//! - `params_b`/`moe`/`active_params_b` come from parsing the GGUF file's own
//!   header (see [`crate::gguf_meta`]) when that succeeds — exact, not a
//!   guess. Falls back to a file-size/quantization heuristic (`moe: false`)
//!   only when the header can't be parsed, so adding a model never fails on
//!   this best-effort detection.
//!
//! Like settings, a corrupt registry falls back to empty with a warning —
//! custom models must never prevent the app from starting.

use crate::huggingface;
use crate::types::{HfFile, HfModelDetails, ModelShard, ModelSource, ModelSpec};
use crate::{Error, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

/// The user's custom models. Plain data; persistence is explicit via
/// [`load`]/[`save`].
#[derive(Debug, Clone, Default)]
pub struct CustomModels {
    models: Vec<ModelSpec>,
}

impl CustomModels {
    pub fn all(&self) -> &[ModelSpec] {
        &self.models
    }

    /// Mutable access to every entry — for in-place metadata backfills (see
    /// `crate::app::commands::backfill_custom_model_metadata`) that update a
    /// handful of fields on existing specs rather than replacing them.
    pub fn all_mut(&mut self) -> impl Iterator<Item = &mut ModelSpec> {
        self.models.iter_mut()
    }

    pub fn get(&self, id: &str) -> Option<&ModelSpec> {
        self.models.iter().find(|m| m.id == id)
    }

    /// Add `spec`, returning its id. The id must already be unique — build
    /// specs with [`hf_spec`]/[`local_spec`], which take the ids in use.
    pub fn add(&mut self, spec: ModelSpec) -> String {
        let id = spec.id.clone();
        debug_assert!(self.get(&id).is_none(), "duplicate custom model id {id}");
        self.models.push(spec);
        id
    }

    /// Add `spec`, or replace the entry already holding its id in place —
    /// how a data import (`crate::import`) brings an entry back from another
    /// device without duplicating one this device already has.
    pub fn upsert(&mut self, spec: ModelSpec) {
        match self.models.iter_mut().find(|m| m.id == spec.id) {
            Some(existing) => *existing = spec,
            None => self.models.push(spec),
        }
    }

    /// Remove the model with `id`. Idempotent.
    pub fn remove(&mut self, id: &str) {
        self.models.retain(|m| m.id != id);
    }
}

pub fn load(conn: &Connection) -> CustomModels {
    let row: Option<String> = conn
        .query_row(
            "SELECT json FROM custom_models WHERE key = 'current'",
            [],
            |row| row.get(0),
        )
        .optional()
        .unwrap_or(None);
    match row {
        Some(raw) => match serde_json::from_str(&raw) {
            Ok(models) => CustomModels { models },
            Err(e) => {
                tracing::warn!("custom models row is corrupt ({e}); starting empty");
                CustomModels::default()
            }
        },
        None => CustomModels::default(),
    }
}

pub fn save(conn: &Connection, custom: &CustomModels) -> Result<()> {
    let json = serde_json::to_string_pretty(&custom.models)?;
    conn.execute(
        "INSERT INTO custom_models (key, json) VALUES ('current', ?1)
         ON CONFLICT(key) DO UPDATE SET json = excluded.json",
        params![json],
    )?;
    Ok(())
}

/// Build the spec for one Hugging Face GGUF the user picked (a single file,
/// or shard 1 of a split model — see `file.extra_shards`).
/// `taken_ids` is every model id already in use (catalog + custom).
/// `shape`, when given, is `(params_b, moe, active_params_b)` from actually
/// parsing this repo's own GGUF header (`crate::gguf_meta`, fetched by
/// `huggingface::fetch_gguf_header_meta`) — real, not a guess. `None` falls
/// back to today's file-size/quant heuristic with `moe: false`, same as
/// before this detection existed (e.g. when the fetch failed or hasn't run).
/// `details`, when given, is the repo's fetched model-card metadata
/// (`huggingface::model_details`) — its real license and creation date beat
/// the placeholder/unknown fallbacks below. `None` (a lookup that failed, or
/// a caller like `decorate_hf_file` that only needs this spec transiently
/// for a fit estimate) falls back exactly as before this existed.
pub fn hf_spec(
    repo_id: &str,
    revision: &str,
    file: &HfFile,
    shape: Option<(f32, bool, Option<f32>)>,
    taken_ids: &[&str],
    details: Option<&HfModelDetails>,
) -> ModelSpec {
    let shard1_name = file.path.rsplit('/').next().unwrap_or(&file.path);
    let lower = shard1_name.to_ascii_lowercase();
    let shard_info = huggingface::split_shard_info(&lower);
    // For a split model, name/id from the shared stem ("gpt-oss-120b-mxfp4"),
    // not shard 1's literal file name ("…-00001-of-00003") — the byte-length
    // slice is safe because GGUF file names are ASCII, so lowercasing never
    // changes length.
    let name_source = match shard_info {
        Some((stem, ..)) => &shard1_name[..stem.len()],
        None => shard1_name,
    };
    let id = unique_id(&slug(name_source.trim_end_matches(".gguf")), taken_ids);
    let publisher = repo_id.split('/').next().unwrap_or(repo_id).to_string();

    // Every shard is renamed to share the id-derived stem so same-named
    // files from different repos never collide in the models directory —
    // llama.cpp's auto-shard-discovery requires exactly this: shard 2..N's
    // file names must differ from shard 1's only in the "-NNNNN-of-MMMMM"
    // index, so the renamed stem has to be identical across every shard.
    let file_name = match shard_info {
        Some((_, index, total)) => format!("{id}-{index:05}-of-{total:05}.gguf"),
        None => format!("{id}.gguf"),
    };
    let extra_shards: Vec<ModelShard> = file
        .extra_shards
        .iter()
        .map(|shard| {
            let shard_name = shard
                .path
                .rsplit('/')
                .next()
                .unwrap_or(&shard.path)
                .to_ascii_lowercase();
            let (_, index, total) = huggingface::split_shard_info(&shard_name)
                .expect("HfFile.extra_shards entries are always split-GGUF shard file names");
            ModelShard {
                url: huggingface::resolve_url(repo_id, revision, &shard.path),
                file_name: format!("{id}-{index:05}-of-{total:05}.gguf"),
                size_bytes: shard.size_bytes,
                sha256: shard.sha256.clone(),
            }
        })
        .collect();
    // Parameter-count estimate needs the model's real total size, not shard
    // 1 alone, or a split model's params_b (and everything fit math derives
    // from it) would be badly undercounted.
    let total_size_bytes = file.size_bytes + extra_shards.iter().map(|s| s.size_bytes).sum::<u64>();
    let (params_b, moe, active_params_b) = shape.unwrap_or((
        huggingface::estimate_params_b(total_size_bytes, &file.quant),
        false,
        None,
    ));
    let license = details
        .and_then(|d| d.license.clone())
        .unwrap_or_else(|| "See the model's Hugging Face page".to_string());
    let release_date = details.and_then(|d| d.created_at.clone());

    ModelSpec {
        name: display_name(name_source),
        family: "Added by you".to_string(),
        publisher,
        description: format!("Your own pick from Hugging Face ({repo_id})."),
        strengths: vec!["Your pick".to_string()],
        url: huggingface::resolve_url(repo_id, revision, &file.path),
        file_name,
        size_bytes: file.size_bytes,
        extra_shards,
        quant: file.quant.clone(),
        sha256: file.sha256.clone(),
        params_b,
        moe,
        active_params_b,
        context_max: 8192,
        // Not in the curated ranking; recommendation only looks at the
        // catalog, so this is display-neutral.
        quality_score: 0,
        license,
        extra_args: Vec::new(),
        hidden: false,
        curated: false,
        // Custom (Hugging Face / local-file) models never carry a curated
        // sampling recommendation — always fall back to the general default.
        task_defaults: HashMap::new(),
        task_defaults_thinking: HashMap::new(),
        source: ModelSource::HuggingFace,
        local_path: None,
        // We can't tell whether an arbitrary user-added GGUF is a vision model
        // (and the picker filters out mmproj adapters), so treat it as text-only.
        vision: false,
        mmproj_url: None,
        mmproj_file_name: None,
        mmproj_size_bytes: None,
        mmproj_sha256: None,
        // No curation pipeline for custom models — same treatment as quality_score: 0.
        task_scores: HashMap::new(),
        // Same repo -> same group: adding a second quant from this repo
        // later automatically ties it to this one for the "All models"
        // table's sub-rows (`ModelSpec::group_key`).
        group_id: Some(repo_id.to_string()),
        release_date,
        id,
    }
}

/// Build the spec for a GGUF file already on the user's disk. The file is
/// used in place. Call [`validate_gguf`] first.
pub fn local_spec(path: &Path, size_bytes: u64, taken_ids: &[&str]) -> ModelSpec {
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "model.gguf".to_string());
    let quant = huggingface::quant_from_file_name(&file_name);
    let id = unique_id(&slug(file_name.trim_end_matches(".gguf")), taken_ids);
    // The file's own GGUF header gives an exact parameter count and real
    // MoE-ness — fall back to the file-size/quant heuristic only when it
    // can't be parsed (an unusual file, or a shape this parser doesn't
    // recognize yet). Adding a local file must never fail just because this
    // best-effort detection didn't work.
    let (params_b, moe, active_params_b) = match crate::gguf_meta::parse_local(path) {
        Ok(meta) => meta.model_shape(),
        Err(_) => (
            huggingface::estimate_params_b(size_bytes, &quant),
            false,
            None,
        ),
    };
    ModelSpec {
        name: display_name(&file_name),
        family: "Added by you".to_string(),
        publisher: "From your Mac".to_string(),
        description: "A model file you added from your own computer.".to_string(),
        strengths: vec!["Your file".to_string()],
        url: String::new(), // nothing to download — it's already here
        file_name,
        size_bytes,
        extra_shards: vec![], // local files are used in place, never split
        sha256: None,
        params_b,
        quant,
        moe,
        active_params_b,
        context_max: 8192,
        quality_score: 0,
        license: "See where you got the file".to_string(),
        extra_args: Vec::new(),
        hidden: false,
        curated: false,
        // Custom (Hugging Face / local-file) models never carry a curated
        // sampling recommendation — always fall back to the general default.
        task_defaults: HashMap::new(),
        task_defaults_thinking: HashMap::new(),
        source: ModelSource::LocalFile,
        local_path: Some(path.to_path_buf()),
        vision: false,
        mmproj_url: None,
        mmproj_file_name: None,
        mmproj_size_bytes: None,
        mmproj_sha256: None,
        // No curation pipeline for custom models — same treatment as quality_score: 0.
        task_scores: HashMap::new(),
        // A local file has no repo to group by, and no metadata source for
        // when it was published.
        group_id: None,
        release_date: None,
        id,
    }
}

/// Check that `path` is a readable GGUF (magic bytes "GGUF") and return its
/// size. The error message is safe to show.
pub fn validate_gguf(path: &Path) -> Result<u64> {
    let mut file = std::fs::File::open(path).map_err(|e| {
        tracing::warn!("could not open {path:?}: {e}");
        Error::Download("That file couldn't be opened. Check that it still exists.".to_string())
    })?;
    let mut magic = [0u8; 4];
    let readable = file.read_exact(&mut magic).is_ok() && &magic == b"GGUF";
    if !readable {
        return Err(Error::Download(
            "That file isn't a GGUF model. OpenPal runs models in the GGUF format, so \
             look for files ending in .gguf."
                .to_string(),
        ));
    }
    Ok(file.metadata()?.len())
}

/// "Qwen3-4B-Q4_K_M.gguf" -> "Qwen3 4B Q4_K_M" — keep it recognizable, just
/// friendlier than a raw file name. Underscores stay so quant labels like
/// "Q4_K_M" survive intact.
fn display_name(file_name: &str) -> String {
    file_name
        .trim_end_matches(".gguf")
        .replace('-', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Lowercase ascii letters/digits/dashes, e.g. "Qwen3 4B (v2)" -> "qwen3-4b-v2".
fn slug(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let out = out.trim_matches('-').to_string();
    if out.is_empty() {
        "model".to_string()
    } else {
        out
    }
}

/// "custom-<slug>", appending "-2", "-3", … until free.
fn unique_id(base_slug: &str, taken_ids: &[&str]) -> String {
    let base = format!("custom-{base_slug}");
    if !taken_ids.contains(&base.as_str()) {
        return base;
    }
    (2..)
        .map(|n| format!("{base}-{n}"))
        .find(|id| !taken_ids.contains(&id.as_str()))
        .expect("some suffix is always free")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn push_str(buf: &mut Vec<u8>, s: &str) {
        buf.extend_from_slice(&(s.len() as u64).to_le_bytes());
        buf.extend_from_slice(s.as_bytes());
    }

    /// Minimal valid GGUF bytes: one dense 100x100 tensor, `general.architecture` set.
    fn dense_gguf_bytes() -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"GGUF");
        buf.extend_from_slice(&3u32.to_le_bytes());
        buf.extend_from_slice(&1u64.to_le_bytes()); // tensor_count
        buf.extend_from_slice(&1u64.to_le_bytes()); // kv_count
        push_str(&mut buf, "general.architecture");
        buf.extend_from_slice(&8u32.to_le_bytes()); // STRING type tag
        push_str(&mut buf, "llama");
        push_str(&mut buf, "token_embd.weight");
        buf.extend_from_slice(&2u32.to_le_bytes()); // n_dims
        buf.extend_from_slice(&100u64.to_le_bytes());
        buf.extend_from_slice(&100u64.to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes()); // ggml_type, unused
        buf.extend_from_slice(&0u64.to_le_bytes()); // data offset, unused
        buf
    }

    /// Minimal valid MoE GGUF bytes: 8 experts, 2 used, one expert-stack tensor.
    fn moe_gguf_bytes() -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"GGUF");
        buf.extend_from_slice(&3u32.to_le_bytes());
        buf.extend_from_slice(&1u64.to_le_bytes()); // tensor_count
        buf.extend_from_slice(&2u64.to_le_bytes()); // kv_count
        push_str(&mut buf, "qwen3moe.expert_count");
        buf.extend_from_slice(&4u32.to_le_bytes()); // UINT32 type tag
        buf.extend_from_slice(&8u32.to_le_bytes());
        push_str(&mut buf, "qwen3moe.expert_used_count");
        buf.extend_from_slice(&4u32.to_le_bytes());
        buf.extend_from_slice(&2u32.to_le_bytes());
        push_str(&mut buf, "blk.0.ffn_gate_exps.weight");
        buf.extend_from_slice(&3u32.to_le_bytes()); // n_dims
        buf.extend_from_slice(&8u64.to_le_bytes());
        buf.extend_from_slice(&50u64.to_le_bytes());
        buf.extend_from_slice(&50u64.to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes());
        buf.extend_from_slice(&0u64.to_le_bytes());
        buf
    }

    fn hf_file(path: &str, size: u64) -> HfFile {
        HfFile {
            path: path.to_string(),
            size_bytes: size,
            quant: huggingface::quant_from_file_name(path),
            sha256: Some("ab".repeat(32)),
            extra_shards: vec![],
        }
    }

    #[test]
    fn hf_spec_renames_every_shard_to_share_the_id_stem() {
        let mut file = hf_file("gpt-oss-120b-mxfp4-00001-of-00003.gguf", 20_000_000_000);
        file.extra_shards = vec![
            crate::types::HfShardFile {
                path: "gpt-oss-120b-mxfp4-00002-of-00003.gguf".to_string(),
                size_bytes: 25_000_000_000,
                sha256: Some("bb".repeat(32)),
            },
            crate::types::HfShardFile {
                path: "gpt-oss-120b-mxfp4-00003-of-00003.gguf".to_string(),
                size_bytes: 18_000_000_000,
                sha256: Some("cc".repeat(32)),
            },
        ];
        let spec = hf_spec("openai/gpt-oss-120b-GGUF", "main", &file, None, &[], None);

        // Name/id come from the shared stem, not shard 1's literal file name.
        assert_eq!(spec.id, "custom-gpt-oss-120b-mxfp4");
        assert!(!spec.id.contains("00001"));

        assert_eq!(
            spec.file_name,
            "custom-gpt-oss-120b-mxfp4-00001-of-00003.gguf"
        );
        assert_eq!(spec.extra_shards.len(), 2);
        assert_eq!(
            spec.extra_shards[0].file_name,
            "custom-gpt-oss-120b-mxfp4-00002-of-00003.gguf"
        );
        assert_eq!(
            spec.extra_shards[1].file_name,
            "custom-gpt-oss-120b-mxfp4-00003-of-00003.gguf"
        );
        // Every shard shares the exact same stem — only the index differs.
        let stem = |name: &str| {
            name.rsplit_once("-of-")
                .unwrap()
                .0
                .rsplit_once('-')
                .unwrap()
                .0
                .to_string()
        };
        assert_eq!(stem(&spec.file_name), stem(&spec.extra_shards[0].file_name));
        assert_eq!(stem(&spec.file_name), stem(&spec.extra_shards[1].file_name));

        assert_eq!(
            spec.extra_shards[0].url,
            "https://huggingface.co/openai/gpt-oss-120b-GGUF/resolve/main/gpt-oss-120b-mxfp4-00002-of-00003.gguf"
        );
        assert_eq!(spec.extra_shards[1].size_bytes, 18_000_000_000);
        assert_eq!(
            spec.total_size_bytes(),
            20_000_000_000 + 25_000_000_000 + 18_000_000_000
        );
        // params_b estimated from the true total, not shard 1's 20 GB alone.
        assert!(spec.params_b > 60.0, "got {}", spec.params_b);
    }

    #[test]
    fn hf_specs_are_complete_and_collision_free() {
        let file = hf_file("Qwen3-4B-Q4_K_M.gguf", 2_500_000_000);
        let spec = hf_spec("unsloth/Qwen3-4B-GGUF", "main", &file, None, &[], None);
        assert_eq!(spec.id, "custom-qwen3-4b-q4-k-m");
        assert_eq!(spec.name, "Qwen3 4B Q4_K_M");
        assert_eq!(spec.publisher, "unsloth");
        assert_eq!(spec.file_name, format!("{}.gguf", spec.id));
        assert_eq!(
            spec.url,
            "https://huggingface.co/unsloth/Qwen3-4B-GGUF/resolve/main/Qwen3-4B-Q4_K_M.gguf"
        );
        assert_eq!(spec.source, ModelSource::HuggingFace);
        assert!(spec.params_b > 2.0 && spec.params_b < 8.0);
        // Groups by repo id, and no metadata lookup means the placeholder
        // license and an unknown release date, same as before `details` existed.
        assert_eq!(spec.group_id.as_deref(), Some("unsloth/Qwen3-4B-GGUF"));
        assert_eq!(spec.license, "See the model's Hugging Face page");
        assert_eq!(spec.release_date, None);

        // Same file from another repo gets a fresh id and file name, and its
        // own group (a different repo is a different model, even if the
        // file happens to be identical).
        let spec2 = hf_spec("other/Repo-GGUF", "main", &file, None, &[&spec.id], None);
        assert_eq!(spec2.id, "custom-qwen3-4b-q4-k-m-2");
        assert_ne!(spec2.file_name, spec.file_name);
        assert_eq!(spec2.group_id.as_deref(), Some("other/Repo-GGUF"));
    }

    #[test]
    fn hf_spec_uses_real_license_and_release_date_when_details_are_known() {
        let file = hf_file("Llama-3.2-1B-Instruct-Q4_K_M.gguf", 900_000_000);
        let details = HfModelDetails {
            repo_id: "bartowski/Llama-3.2-1B-Instruct-GGUF".to_string(),
            author: Some("bartowski".to_string()),
            pipeline_tag: None,
            downloads: None,
            likes: None,
            license: Some("llama3.2".to_string()),
            gated: false,
            last_modified: Some("2026-05-01T00:00:00.000Z".to_string()),
            created_at: Some("2024-09-25T00:00:00.000Z".to_string()),
            tags: vec![],
            gguf: None,
        };
        let spec = hf_spec(
            "bartowski/Llama-3.2-1B-Instruct-GGUF",
            "main",
            &file,
            None,
            &[],
            Some(&details),
        );
        assert_eq!(spec.license, "llama3.2");
        assert_eq!(
            spec.release_date.as_deref(),
            Some("2024-09-25T00:00:00.000Z")
        );
    }

    #[test]
    fn hf_spec_uses_the_real_shape_when_given() {
        let file = hf_file("Qwen3-30B-A3B-Q4_K_M.gguf", 20_000_000_000);
        let spec = hf_spec(
            "Qwen/Qwen3-30B-A3B-GGUF",
            "main",
            &file,
            Some((30.0, true, Some(3.0))),
            &[],
            None,
        );
        assert_eq!(spec.params_b, 30.0);
        assert!(spec.moe);
        assert_eq!(spec.active_params_b, Some(3.0));
    }

    #[test]
    fn local_specs_use_the_file_in_place() {
        let path = Path::new("/Users/me/Models/My Model_Q8_0.gguf");
        let spec = local_spec(path, 1_000_000, &[]);
        assert_eq!(spec.source, ModelSource::LocalFile);
        assert_eq!(spec.local_path.as_deref(), Some(path));
        assert_eq!(spec.quant, "Q8_0");
        assert!(spec.url.is_empty());
        // No such file on disk, so the header can't be read — falls back to
        // the size-based estimate rather than failing.
        assert!(!spec.moe);
        assert_eq!(spec.active_params_b, None);
    }

    #[test]
    fn local_spec_reads_real_shape_from_a_dense_gguf_header() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("model.gguf");
        let bytes = dense_gguf_bytes();
        std::fs::write(&path, &bytes).unwrap();

        let spec = local_spec(&path, bytes.len() as u64, &[]);
        assert!(!spec.moe);
        assert_eq!(spec.active_params_b, None);
        assert!(
            (spec.params_b - 10_000f32 / 1e9).abs() < 1e-12,
            "got {}",
            spec.params_b
        );
    }

    #[test]
    fn local_spec_reads_real_shape_from_an_moe_gguf_header() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("model.gguf");
        let bytes = moe_gguf_bytes();
        std::fs::write(&path, &bytes).unwrap();

        let spec = local_spec(&path, bytes.len() as u64, &[]);
        assert!(spec.moe);
        // 8 experts * 50*50 = 20_000 expert params; used 2/8 -> active 5_000.
        let expected_active_b = 5_000f32 / 1e9;
        assert!((spec.active_params_b.unwrap() - expected_active_b).abs() < 1e-12);
    }

    #[test]
    fn local_spec_falls_back_to_the_estimate_when_the_header_cant_be_parsed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("not-really-gguf.gguf");
        std::fs::write(&path, b"GGUF not a real header").unwrap();

        let spec = local_spec(&path, 1_000_000, &[]);
        assert!(!spec.moe);
        assert_eq!(spec.active_params_b, None);
        assert!(
            spec.params_b > 0.0,
            "still falls back to the size-based estimate"
        );
    }

    /// See `crate::store::tests::test_conn`'s own doc comment for why
    /// `.keep()` is used here.
    fn test_conn() -> Connection {
        let dir = tempfile::tempdir().unwrap().keep();
        crate::app_db::open(&dir.join("app.sqlite"), None)
            .unwrap()
            .0
    }

    #[test]
    fn registry_roundtrip_and_corrupt_fallback() {
        let conn = test_conn();

        assert!(load(&conn).all().is_empty());

        let mut custom = CustomModels::default();
        let id = custom.add(hf_spec(
            "o/r",
            "main",
            &hf_file("m-Q4_K_M.gguf", 10),
            None,
            &[],
            None,
        ));
        save(&conn, &custom).unwrap();

        let loaded = load(&conn);
        assert_eq!(loaded.all().len(), 1);
        assert!(loaded.get(&id).is_some());

        let mut loaded = loaded;
        loaded.remove(&id);
        loaded.remove(&id); // idempotent
        assert!(loaded.all().is_empty());

        conn.execute(
            "INSERT INTO custom_models (key, json) VALUES ('current', '{not json')
             ON CONFLICT(key) DO UPDATE SET json = excluded.json",
            [],
        )
        .unwrap();
        assert!(
            load(&conn).all().is_empty(),
            "corrupt row falls back to empty"
        );
    }

    #[test]
    fn gguf_validation_checks_the_magic() {
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("good.gguf");
        std::fs::write(&good, b"GGUF\x03\x00\x00\x00rest").unwrap();
        assert_eq!(validate_gguf(&good).unwrap(), 12);

        let bad = dir.path().join("bad.gguf");
        std::fs::write(&bad, b"MZ\x90\x00").unwrap();
        assert!(validate_gguf(&bad).is_err());
        assert!(validate_gguf(&dir.path().join("missing.gguf")).is_err());
    }

    #[test]
    fn slugs_are_clean() {
        assert_eq!(slug("Qwen3 4B (v2)"), "qwen3-4b-v2");
        assert_eq!(slug("___"), "model");
        assert_eq!(unique_id("m", &["custom-m", "custom-m-2"]), "custom-m-3");
    }
}
