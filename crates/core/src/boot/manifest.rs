//! `<data>/boot.json` — unencrypted, non-secret, meant to be read before
//! any unlock attempt (`docs/SPACES_AUTHORING.md` §9). It exists because
//! `ui/src/i18n/index.ts` boots at a hardcoded `lng: "en"`, and
//! `Settings.language` (read today via a normal, `AppState`-gated command)
//! will move behind unlock once the bootstrap split lands — without this
//! file, the least recoverable screen in the product (the unlock screen)
//! would render in English for every French user, at every launch,
//! forever. Same load/save conventions as `crate::settings`: corrupt or
//! missing is never a reason to fail to start.

use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct BootManifest {
    pub schema_version: u32,
    pub language: Option<String>,
    pub log_level: Option<String>,
    pub store_id: Option<String>,
    pub last_write_counter: u64,
    /// The encryption scheme in use, e.g. `"sqlcipher-aes256gcm-argon2id"`
    /// once a later phase makes it real. `None` today.
    pub scheme: Option<String>,
    /// Best-effort, informational only (e.g. `"apfs"`, `"ntfs"`, `"exfat"`)
    /// — never used to gate correctness.
    pub filesystem: Option<String>,
}

impl Default for BootManifest {
    fn default() -> Self {
        Self {
            schema_version: 1,
            language: None,
            log_level: None,
            store_id: None,
            last_write_counter: 0,
            scheme: None,
            filesystem: None,
        }
    }
}

pub fn load(path: &Path) -> BootManifest {
    match std::fs::read_to_string(path) {
        Ok(raw) => serde_json::from_str(&raw).unwrap_or_else(|e| {
            tracing::warn!("boot.json is corrupt ({e}); using defaults");
            BootManifest::default()
        }),
        Err(_) => BootManifest::default(),
    }
}

pub fn save(path: &Path, manifest: &BootManifest) -> crate::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(manifest)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_falls_back_to_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = load(&dir.path().join("boot.json"));
        assert_eq!(manifest, BootManifest::default());
    }

    #[test]
    fn corrupt_file_falls_back_to_defaults_without_panicking() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("boot.json");
        std::fs::write(&path, "{ not json").unwrap();
        assert_eq!(load(&path), BootManifest::default());
    }

    #[test]
    fn roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("boot.json");
        let manifest = BootManifest {
            schema_version: 1,
            language: Some("fr".to_string()),
            log_level: Some("debug".to_string()),
            store_id: Some("abc123".to_string()),
            last_write_counter: 42,
            scheme: Some("sqlcipher-aes256gcm-argon2id".to_string()),
            filesystem: Some("apfs".to_string()),
        };
        save(&path, &manifest).unwrap();
        assert_eq!(load(&path), manifest);
    }

    #[test]
    fn a_payload_missing_newer_fields_still_loads() {
        // Forward-compat: a boot.json written by an older binary, before a
        // field existed, must not become unreadable — same property
        // `settings.rs`'s own tests pin for `Settings`.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("boot.json");
        std::fs::write(&path, r#"{"schemaVersion":1,"language":"en"}"#).unwrap();
        let manifest = load(&path);
        assert_eq!(manifest.language, Some("en".to_string()));
        assert_eq!(manifest.store_id, None);
        assert_eq!(manifest.last_write_counter, 0);
    }

    #[test]
    fn save_creates_missing_parent_directories() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("boot.json");
        save(&path, &BootManifest::default()).unwrap();
        assert!(path.exists());
    }
}
