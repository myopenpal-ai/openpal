//! The pinned classifier and how it gets onto disk.
//!
//! [Horizon-Labs/prompt-injection-guard-small] (Apache-2.0, ungated): an
//! mmBERT-small (141M parameters, multilingual — French included) sequence
//! classifier, `0: SAFE / 1: INJECTION`. Only two of its files are needed,
//! both pinned to one revision and checked by size and sha256 — the
//! publisher's own int8-embedding ONNX export (scores within 0.015 of the
//! fp32 weights at half the download) and its `tokenizer.json`, which also
//! carries the model's obfuscation normalizer (NFKC, invisible and
//! tag-character stripping), so no preprocessing is reimplemented here.
//!
//! [Horizon-Labs/prompt-injection-guard-small]: https://huggingface.co/Horizon-Labs/prompt-injection-guard-small

use crate::download;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

pub(crate) const MODEL_FILE: &str = "model_quantized.onnx";
pub(crate) const TOKENIZER_FILE: &str = "tokenizer.json";

const REPO: &str = "https://huggingface.co/Horizon-Labs/prompt-injection-guard-small/resolve/0e3774536debbcd71dbb8534ef71451b583853b8";

struct PinnedFile {
    name: &'static str,
    path_in_repo: &'static str,
    bytes: u64,
    sha256: &'static str,
}

const FILES: [PinnedFile; 2] = [
    PinnedFile {
        name: MODEL_FILE,
        path_in_repo: "onnx/model_quantized.onnx",
        bytes: 268_409_234,
        sha256: "61cd0741cdbfcc229e32e1de1392f352736d87d1e8b3c2b67ad9836bb7910d5d",
    },
    PinnedFile {
        name: TOKENIZER_FILE,
        path_in_repo: "tokenizer.json",
        bytes: 34_387_555,
        sha256: "47834a7dbbb0c4324fe9569fe47955241594a9ee539c9e478755d6fbc51e5f46",
    },
];

/// Room left free on the disk after the download, same margin
/// `commands::download_model` keeps for a chat model.
const DISK_HEADROOM: u64 = 1024 * 1024 * 1024;

/// Whether both pinned files are in `dir` at their full size. A file only
/// ever reaches its final name after `download::download_file` checked its
/// sha256, so size is enough here — no re-hashing 300 MB on every launch.
pub fn files_present(dir: &Path) -> bool {
    FILES.iter().all(|file| {
        std::fs::metadata(dir.join(file.name)).is_ok_and(|meta| meta.len() == file.bytes)
    })
}

/// Stand-ins for both pinned files at their full size — sparse, so nothing
/// near 300 MB is written — making [`files_present`] true without a download.
#[cfg(test)]
pub(crate) fn fake_download(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    for file in &FILES {
        std::fs::File::create(dir.join(file.name))
            .and_then(|f| f.set_len(file.bytes))
            .unwrap();
    }
}

/// Download whichever pinned file is missing from `dir` (a no-op when both
/// are there). Resumes an interrupted download — Hugging Face's CDN does
/// drop long transfers — and refuses to start when the disk is too full.
pub async fn provision(http: &reqwest::Client, dir: &Path) -> Result<(), String> {
    if files_present(dir) {
        return Ok(());
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("couldn't create {dir:?}: {e}"))?;

    let still_needed: u64 = FILES
        .iter()
        .filter(|file| {
            !std::fs::metadata(dir.join(file.name)).is_ok_and(|meta| meta.len() == file.bytes)
        })
        .map(|file| {
            file.bytes
                .saturating_sub(download::partial_bytes(&dir.join(file.name)))
        })
        .sum();
    let probe_dir = dir.to_path_buf();
    let free = tokio::task::spawn_blocking(move || crate::hardware::free_disk_bytes(&probe_dir))
        .await
        .map_err(|e| e.to_string())?;
    if free < still_needed + DISK_HEADROOM {
        return Err(format!(
            "not enough free disk space ({} MB needed, {} MB free)",
            (still_needed + DISK_HEADROOM) / 1_000_000,
            free / 1_000_000
        ));
    }

    for file in &FILES {
        download::download_file(
            http,
            &format!("{REPO}/{}", file.path_in_repo),
            &dir.join(file.name),
            Some(file.bytes),
            Some(file.sha256),
            Arc::new(AtomicBool::new(false)),
            |_| {},
        )
        .await
        .map_err(|e| format!("{}: {e}", file.name))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_are_present_only_at_their_full_size() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!files_present(dir.path()));
        for file in &FILES {
            std::fs::write(dir.path().join(file.name), b"short").unwrap();
        }
        assert!(!files_present(dir.path()), "truncated files don't count");
    }

    #[test]
    fn every_pinned_file_comes_from_the_pinned_revision_over_https() {
        assert!(REPO.starts_with("https://huggingface.co/"));
        assert!(REPO.ends_with("/resolve/0e3774536debbcd71dbb8534ef71451b583853b8"));
        for file in &FILES {
            assert_eq!(file.sha256.len(), 64, "{}", file.name);
            assert!(file.bytes > 0, "{}", file.name);
        }
    }
}
