//! The wrap file (`docs/SPACES_AUTHORING.md` §9 "Key provisioning" and "The
//! wrap file also solves a problem SQLite cannot"): one `DataKey` (a DEK)
//! sealed under another `DataKey` used as a KEK, via AES-256-GCM. Every
//! weakenable KDF parameter (`m_cost`, `t_cost`, and the not-yet-used-until
//! Phase 6.8 `norm_id`/`wordlist_id`) lives in the AEAD's associated data —
//! authenticated, so an edit to any of them (a downgrade attempt) fails
//! decryption rather than silently weakening a future rewrap. A 4-byte
//! SHA-256-prefix checksum sits *outside* the AEAD so a damaged file can be
//! told apart from a wrong passphrase without ever attempting an AEAD open
//! (the doc's own measured 72 ms `WrongPassphrase` path assumes exactly
//! this ordering).
//!
//! This module only knows how to write and read one wrap file correctly —
//! it keeps a single `.bak` (the prior generation, written as a real copy;
//! the doc found hardlinks fail `EOPNOTSUPP` on FAT32) but has no opinion
//! on *when* to fall back to it. That policy is a later phase's job.

use super::key::DataKey;
use rand::rngs::OsRng;
use rand::RngCore;
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM, NONCE_LEN};
use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use zeroize::Zeroize;

const CURRENT_VERSION: u8 = 1;
const HEADER_LEN: usize = 1 + 4 + 4 + 1 + 1 + 16; // version, m_cost, t_cost, norm_id, wordlist_id, salt
const CHECKSUM_LEN: usize = 4;
const TAG_LEN: usize = 16;

pub struct WrapHeader {
    pub version: u8,
    pub m_cost_kib: u32,
    pub t_cost: u32,
    pub norm_id: u8,
    pub wordlist_id: u8,
    pub salt: [u8; 16],
}

impl WrapHeader {
    /// A fresh header for the current version, ready for `write_wrap`.
    /// `norm_id`/`wordlist_id` are unused until Phase 6.8 (portable) — `0`
    /// until then.
    pub fn new(salt: [u8; 16]) -> Self {
        Self {
            version: CURRENT_VERSION,
            m_cost_kib: super::kdf::M_COST_KIB,
            t_cost: super::kdf::T_COST,
            norm_id: 0,
            wordlist_id: 0,
            salt,
        }
    }

    /// A header for a keychain-backed slot: no KDF runs (the keychain
    /// access itself is the authentication), so `m_cost_kib`/`t_cost` are
    /// `0` — an honest signal of that, not a weakened real KDF. Salt is
    /// zeroed since it's unused when no KDF runs. Mirrors
    /// `crates/app/src/bootstrap.rs::mint_or_unlock`'s `keychain.wrap`
    /// construction exactly — factored out here so `crate::sync::pairing`'s
    /// three new keychain-backed wrap files (`sync_group.wrap`/`sync.wrap`/
    /// `sync_identity.wrap`) don't each re-derive this by hand.
    pub fn keychain_backed() -> Self {
        Self {
            m_cost_kib: 0,
            t_cost: 0,
            ..Self::new([0u8; 16])
        }
    }

    fn to_bytes(&self) -> [u8; HEADER_LEN] {
        let mut out = [0u8; HEADER_LEN];
        out[0] = self.version;
        out[1..5].copy_from_slice(&self.m_cost_kib.to_be_bytes());
        out[5..9].copy_from_slice(&self.t_cost.to_be_bytes());
        out[9] = self.norm_id;
        out[10] = self.wordlist_id;
        out[11..27].copy_from_slice(&self.salt);
        out
    }

    fn from_bytes(bytes: &[u8]) -> Option<(Self, &[u8])> {
        if bytes.len() < HEADER_LEN {
            return None;
        }
        let (head, rest) = bytes.split_at(HEADER_LEN);
        Some((
            Self {
                version: head[0],
                m_cost_kib: u32::from_be_bytes(head[1..5].try_into().ok()?),
                t_cost: u32::from_be_bytes(head[5..9].try_into().ok()?),
                norm_id: head[9],
                wordlist_id: head[10],
                salt: head[11..27].try_into().ok()?,
            },
            rest,
        ))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum WrapError {
    #[error("{0}")]
    Io(#[from] io::Error),
    #[error("the wrap file is too short or malformed")]
    Malformed,
    #[error("the wrap file is damaged")]
    ChecksumMismatch,
    #[error("wrong passphrase, or the wrap file's header was tampered with")]
    WrongPassphrase,
}

fn checksum(blob: &[u8]) -> [u8; CHECKSUM_LEN] {
    let digest = Sha256::digest(blob);
    let mut out = [0u8; CHECKSUM_LEN];
    out.copy_from_slice(&digest[..CHECKSUM_LEN]);
    out
}

fn seal(dek: &DataKey, kek: &DataKey, header: &WrapHeader) -> Result<Vec<u8>, WrapError> {
    let unbound =
        UnboundKey::new(&AES_256_GCM, kek.as_bytes()).map_err(|_| WrapError::Malformed)?;
    let key = LessSafeKey::new(unbound);
    let mut nonce_bytes = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::assume_unique_for_key(nonce_bytes);

    // `in_out` briefly holds the raw plaintext DEK (from `to_vec()`) before
    // `seal_in_place_append_tag` overwrites it with ciphertext — zeroized
    // unconditionally below, on every exit path, since a plain `Vec<u8>`
    // drop only deallocates and never scrubs its contents.
    let mut in_out = dek.as_bytes().to_vec();
    let result = key
        .seal_in_place_append_tag(nonce, Aad::from(header.to_bytes()), &mut in_out)
        .map_err(|_| WrapError::Malformed)
        .map(|()| {
            let mut blob = header.to_bytes().to_vec();
            blob.extend_from_slice(&nonce_bytes);
            blob.extend_from_slice(&in_out); // ciphertext || tag
            blob
        });
    in_out.zeroize();
    result
}

fn open(blob: &[u8], kek: &DataKey) -> Result<DataKey, WrapError> {
    let (header, rest) = WrapHeader::from_bytes(blob).ok_or(WrapError::Malformed)?;
    if rest.len() < NONCE_LEN + TAG_LEN {
        return Err(WrapError::Malformed);
    }
    let (nonce_bytes, ciphertext_and_tag) = rest.split_at(NONCE_LEN);
    let nonce = Nonce::try_assume_unique_for_key(nonce_bytes).map_err(|_| WrapError::Malformed)?;
    let unbound =
        UnboundKey::new(&AES_256_GCM, kek.as_bytes()).map_err(|_| WrapError::Malformed)?;
    let key = LessSafeKey::new(unbound);

    // `buf` holds the raw plaintext DEK/KEK once `open_in_place` succeeds —
    // zeroized unconditionally below, on every exit path (success or
    // failure), rather than left for a plain `Vec<u8>`'s drop to just
    // deallocate without scrubbing it.
    let mut buf = ciphertext_and_tag.to_vec();
    let result = key
        .open_in_place(nonce, Aad::from(header.to_bytes()), &mut buf)
        .map_err(|_| WrapError::WrongPassphrase)
        .and_then(|plaintext| {
            let bytes: [u8; 32] = plaintext.try_into().map_err(|_| WrapError::Malformed)?;
            Ok(DataKey::from_bytes(bytes))
        });
    buf.zeroize();
    result
}

fn bak_path_for(path: &Path) -> PathBuf {
    let mut s: OsString = path.as_os_str().to_os_string();
    s.push(".bak");
    PathBuf::from(s)
}

fn tmp_path_for(path: &Path) -> PathBuf {
    let mut s: OsString = path.as_os_str().to_os_string();
    s.push(".tmp");
    PathBuf::from(s)
}

#[cfg(unix)]
fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

#[cfg(not(unix))]
fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = fs::File::create(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// Best-effort: durability of the *rename itself*, not just the file's
/// content. Unix-only for now — there's no existing precedent in this
/// crate for the Windows equivalent (`FlushFileBuffers` on a directory
/// handle), and it's not attempted here.
#[cfg(unix)]
fn fsync_dir(file_path: &Path) {
    if let Some(dir) = file_path.parent().filter(|d| !d.as_os_str().is_empty()) {
        if let Ok(d) = fs::File::open(dir) {
            let _ = d.sync_all();
        }
    }
}

#[cfg(not(unix))]
fn fsync_dir(_file_path: &Path) {}

/// Writes `dek`, sealed under `kek`, to `path` — write, verify, rename,
/// fsync, in that order (the doc's own phrase): write to a temp file and
/// fsync its contents, read it back and confirm it decrypts to exactly
/// `dek` *before* committing, only then atomically rename over `path`, then
/// fsync the containing directory. If `path` already holds a wrap file, its
/// current bytes are preserved as `path.bak` first (a real copy, not a
/// hardlink) — this function has no opinion on when a caller should later
/// fall back to that `.bak`.
pub fn write_wrap(
    path: &Path,
    dek: &DataKey,
    kek: &DataKey,
    header: &WrapHeader,
) -> Result<(), WrapError> {
    let blob = seal(dek, kek, header)?;
    let mut file_bytes = blob;
    let sum = checksum(&file_bytes);
    file_bytes.extend_from_slice(&sum);

    if path.exists() {
        fs::copy(path, bak_path_for(path))?;
    }

    let tmp_path = tmp_path_for(path);
    write_private(&tmp_path, &file_bytes)?;

    // Verify before committing: read the temp file back and confirm it
    // round-trips to exactly the key we just sealed.
    let recovered = read_wrap(&tmp_path, kek)?;
    if recovered.as_bytes() != dek.as_bytes() {
        return Err(WrapError::Malformed);
    }

    fs::rename(&tmp_path, path)?;
    fsync_dir(path);
    Ok(())
}

/// Reads the file and checksum-verifies it, returning the blob (header ||
/// nonce || ciphertext || tag, checksum already stripped and confirmed)
/// shared by `read_wrap` and `read_header` below.
fn read_checked_blob(path: &Path) -> Result<Vec<u8>, WrapError> {
    checked_blob(&fs::read(path)?).map(<[u8]>::to_vec)
}

fn checked_blob(bytes: &[u8]) -> Result<&[u8], WrapError> {
    if bytes.len() <= CHECKSUM_LEN {
        return Err(WrapError::Malformed);
    }
    let (blob, sum) = bytes.split_at(bytes.len() - CHECKSUM_LEN);
    if checksum(blob) != sum {
        return Err(WrapError::ChecksumMismatch);
    }
    Ok(blob)
}

/// Reads and unseals the `DataKey` at `path`. Checksum first — cheap, and
/// disambiguates a damaged file (`ChecksumMismatch`) from a wrong
/// passphrase (`WrongPassphrase`) without ever attempting an AEAD open.
pub fn read_wrap(path: &Path, kek: &DataKey) -> Result<DataKey, WrapError> {
    let blob = read_checked_blob(path)?;
    open(&blob, kek)
}

/// Reads and checksum-verifies the file and parses its header — the AEAD's
/// authenticated associated data — WITHOUT attempting decryption. No KEK
/// needed. For a caller that needs the header's own fields (the KDF salt/
/// parameters this file was actually sealed with) before it has a KEK to
/// derive, e.g. to attempt an unlock with a user-provided secret.
pub fn read_header(path: &Path) -> Result<WrapHeader, WrapError> {
    let blob = read_checked_blob(path)?;
    let (header, _rest) = WrapHeader::from_bytes(&blob).ok_or(WrapError::Malformed)?;
    Ok(header)
}

/// [`read_wrap`] for a wrap file's bytes already in memory — a copy of
/// `recovery.wrap` carried inside a backup (`crate::backup`), say.
pub fn open_wrap_bytes(bytes: &[u8], kek: &DataKey) -> Result<DataKey, WrapError> {
    open(checked_blob(bytes)?, kek)
}

/// [`read_header`] for a wrap file's bytes already in memory.
pub fn header_from_bytes(bytes: &[u8]) -> Result<WrapHeader, WrapError> {
    let (header, _rest) =
        WrapHeader::from_bytes(checked_blob(bytes)?).ok_or(WrapError::Malformed)?;
    Ok(header)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header() -> WrapHeader {
        WrapHeader::new([9u8; 16])
    }

    #[test]
    fn roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keychain.wrap");
        let dek = DataKey::random();
        let kek = DataKey::random();
        write_wrap(&path, &dek, &kek, &header()).unwrap();
        let recovered = read_wrap(&path, &kek).unwrap();
        assert_eq!(recovered, dek);
    }

    #[test]
    fn wrong_kek_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keychain.wrap");
        let dek = DataKey::random();
        write_wrap(&path, &dek, &DataKey::random(), &header()).unwrap();
        let err = read_wrap(&path, &DataKey::random()).unwrap_err();
        assert!(matches!(err, WrapError::WrongPassphrase));
    }

    #[test]
    fn a_damaged_checksum_region_is_reported_before_any_aead_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keychain.wrap");
        let kek = DataKey::random();
        write_wrap(&path, &DataKey::random(), &kek, &header()).unwrap();

        let mut bytes = fs::read(&path).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xFF; // flip a bit in the checksum itself
        fs::write(&path, &bytes).unwrap();

        let err = read_wrap(&path, &kek).unwrap_err();
        assert!(matches!(err, WrapError::ChecksumMismatch));
    }

    #[test]
    fn tampering_with_an_authenticated_header_field_is_rejected_not_silently_weakened() {
        // Reproduces the doc's own m_cost-downgrade finding: editing a
        // weakenable header field (even leaving the checksum consistent
        // with the new bytes) must fail decryption, not succeed with
        // weaker parameters.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keychain.wrap");
        let kek = DataKey::random();
        write_wrap(&path, &DataKey::random(), &kek, &header()).unwrap();

        let mut bytes = fs::read(&path).unwrap();
        // m_cost_kib occupies bytes[1..5] of the header.
        bytes[1] ^= 0xFF;
        let blob_len = bytes.len() - CHECKSUM_LEN;
        let new_sum = checksum(&bytes[..blob_len]);
        bytes[blob_len..].copy_from_slice(&new_sum); // keep the checksum self-consistent

        fs::write(&path, &bytes).unwrap();
        let err = read_wrap(&path, &kek).unwrap_err();
        assert!(matches!(err, WrapError::WrongPassphrase));
    }

    #[test]
    fn a_prior_generation_is_preserved_as_bak_on_rewrap() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keychain.wrap");
        let kek = DataKey::random();
        let first = DataKey::random();
        write_wrap(&path, &first, &kek, &header()).unwrap();

        let second = DataKey::random();
        write_wrap(&path, &second, &kek, &header()).unwrap();

        let bak = bak_path_for(&path);
        assert!(bak.exists());
        let from_bak = read_wrap(&bak, &kek).unwrap();
        assert_eq!(from_bak, first);
        let from_current = read_wrap(&path, &kek).unwrap();
        assert_eq!(from_current, second);
    }

    #[test]
    fn write_wrap_does_not_leave_a_tmp_file_behind() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keychain.wrap");
        write_wrap(&path, &DataKey::random(), &DataKey::random(), &header()).unwrap();
        assert!(!tmp_path_for(&path).exists());
    }

    #[test]
    fn read_header_returns_exactly_what_write_wrap_was_given() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("recovery.wrap");
        let written = header();
        write_wrap(&path, &DataKey::random(), &DataKey::random(), &written).unwrap();

        // No KEK constructed anywhere in this test — that's the point:
        // `read_header` never needs one.
        let read = read_header(&path).unwrap();
        assert_eq!(read.version, written.version);
        assert_eq!(read.m_cost_kib, written.m_cost_kib);
        assert_eq!(read.t_cost, written.t_cost);
        assert_eq!(read.norm_id, written.norm_id);
        assert_eq!(read.wordlist_id, written.wordlist_id);
        assert_eq!(read.salt, written.salt);
    }

    #[test]
    fn read_header_reports_a_damaged_checksum_region_too() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keychain.wrap");
        write_wrap(&path, &DataKey::random(), &DataKey::random(), &header()).unwrap();

        let mut bytes = fs::read(&path).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xFF; // flip a bit in the checksum itself
        fs::write(&path, &bytes).unwrap();

        let result = read_header(&path);
        assert!(matches!(result, Err(WrapError::ChecksumMismatch)));
    }
}
