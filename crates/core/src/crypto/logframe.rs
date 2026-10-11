//! Self-describing AEAD frame format for encrypted log files
//! (`docs/SPACES_AUTHORING.md` §9 "Decided: logs are encrypted"). One
//! AES-256-GCM-sealed frame per log line, each carrying its own length
//! prefix and an authenticated `marker` (caller-defined: the app log uses
//! a YYYYMMDD day stamp so a per-day key can be re-derived; the engine log
//! ignores it) — same "everything an attacker could edit belongs in the
//! AAD" rule `super::wrap`'s header already follows: a rewritten marker
//! fails to open rather than silently decrypting under the wrong context.
//!
//! Lives in `openpal_core`, not `crates/app`, purely because
//! `crate::engine` (core) needs it as much as `crates/app::logging` does,
//! and core cannot depend on the app crate — one shared primitive, two
//! call sites, rather than a parallel reimplementation in each.
//!
//! Frame layout, all integers big-endian:
//! ```text
//! [4 bytes]  frame body length (marker + nonce + ciphertext + tag)
//! [8 bytes]  marker (u64) — authenticated, not encrypted
//! [12 bytes] random nonce
//! [N bytes]  ciphertext || 16-byte GCM tag
//! ```

use super::key::DataKey;
use chrono::Datelike;
use rand::rngs::OsRng;
use rand::RngCore;
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM, NONCE_LEN};
use zeroize::Zeroize;

/// Today's UTC calendar day as `YYYYMMDD` — the marker the app log uses to
/// re-derive a fresh per-day key (capping how many frames ever share one
/// key, well short of GCM's nonce-reuse caution threshold). Lives here
/// rather than in `crates/app` so that crate doesn't need `chrono` as a
/// direct dependency just to compute one integer — same reasoning as
/// `crate::parental::now_epoch_seconds`'s own doc comment.
pub fn today_marker() -> u64 {
    let now = chrono::Utc::now();
    now.year() as u64 * 10_000 + now.month() as u64 * 100 + now.day() as u64
}

const LEN_PREFIX: usize = 4;
const MARKER_LEN: usize = 8;
const TAG_LEN: usize = 16;
/// The smallest a frame body can legitimately be: marker + nonce + an
/// empty ciphertext's tag. Anything shorter is corrupt, not just empty.
const MIN_BODY_LEN: usize = MARKER_LEN + NONCE_LEN + TAG_LEN;

/// Seal `plaintext` as one frame under `key`, with `marker` as the AEAD's
/// authenticated associated data. Returns the complete frame (length
/// prefix included), ready to `write_all` onto any sink.
pub fn seal_frame(key: &DataKey, marker: u64, plaintext: &[u8]) -> Vec<u8> {
    let unbound = UnboundKey::new(&AES_256_GCM, key.as_bytes())
        .expect("DataKey is always exactly AES-256-GCM's 32-byte key length");
    let sealing = LessSafeKey::new(unbound);
    let mut nonce_bytes = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::assume_unique_for_key(nonce_bytes);
    let marker_bytes = marker.to_be_bytes();

    // `in_out` briefly holds the plaintext line before
    // `seal_in_place_append_tag` overwrites it with ciphertext — matches
    // `super::wrap::seal`'s own convention, though a log line (unlike a
    // raw DEK) isn't itself key material, so no zeroize-on-drop here.
    let mut in_out = plaintext.to_vec();
    sealing
        .seal_in_place_append_tag(nonce, Aad::from(marker_bytes), &mut in_out)
        .expect("sealing an in-memory buffer under a valid key cannot fail");

    let body_len = MARKER_LEN + NONCE_LEN + in_out.len();
    let mut frame = Vec::with_capacity(LEN_PREFIX + body_len);
    frame.extend_from_slice(&(body_len as u32).to_be_bytes());
    frame.extend_from_slice(&marker_bytes);
    frame.extend_from_slice(&nonce_bytes);
    frame.extend_from_slice(&in_out);
    frame
}

/// One successfully authenticated frame.
pub struct OpenedFrame {
    pub marker: u64,
    pub plaintext: Vec<u8>,
}

/// Outcome of parsing the frame at the front of a byte slice.
pub enum FrameParse<'a> {
    /// Authenticated successfully; `rest` is whatever follows.
    Ok(OpenedFrame, &'a [u8]),
    /// The length prefix claims more bytes than remain — a truncated
    /// trailing frame (e.g. a crash mid-write). Not corruption: the
    /// caller should stop here, the same tolerance the old plaintext tail
    /// had for a half-written last line.
    Truncated,
    /// The frame parsed structurally but failed to authenticate (bit
    /// rot, a wrong key, or a genuinely tampered marker) — `rest` is what
    /// follows, so a caller can skip just this one frame and keep going,
    /// strictly better than plaintext's total inability to detect this at
    /// all.
    Corrupt(&'a [u8]),
}

/// `key_for_marker` derives (or selects) the key that should have sealed
/// a frame carrying the given (still-unauthenticated) marker — e.g. the
/// app log re-derives a per-day key from the marker; the engine log
/// ignores the marker and always returns its one fixed session key. The
/// derived key is only trusted once the AEAD open using it and the
/// marker as AAD actually succeeds.
pub fn parse_one<'a>(
    bytes: &'a [u8],
    key_for_marker: &dyn Fn(u64) -> DataKey,
) -> Option<FrameParse<'a>> {
    if bytes.len() < LEN_PREFIX {
        return None; // clean EOF — not even a length prefix left
    }
    let body_len = u32::from_be_bytes(bytes[..LEN_PREFIX].try_into().unwrap()) as usize;
    let total = LEN_PREFIX + body_len;
    if bytes.len() < total {
        return Some(FrameParse::Truncated);
    }
    let body = &bytes[LEN_PREFIX..total];
    let rest = &bytes[total..];
    if body_len < MIN_BODY_LEN {
        return Some(FrameParse::Corrupt(rest));
    }

    let marker_bytes: [u8; MARKER_LEN] = body[..MARKER_LEN].try_into().unwrap();
    let marker = u64::from_be_bytes(marker_bytes);
    let (nonce_bytes, ct_and_tag) = body[MARKER_LEN..].split_at(NONCE_LEN);
    let Ok(nonce) = Nonce::try_assume_unique_for_key(nonce_bytes) else {
        return Some(FrameParse::Corrupt(rest));
    };

    let key = key_for_marker(marker);
    let Ok(unbound) = UnboundKey::new(&AES_256_GCM, key.as_bytes()) else {
        return Some(FrameParse::Corrupt(rest));
    };
    let opening = LessSafeKey::new(unbound);
    let mut buf = ct_and_tag.to_vec();
    match opening.open_in_place(nonce, Aad::from(marker_bytes), &mut buf) {
        Ok(plaintext) => {
            let plaintext = plaintext.to_vec();
            buf.zeroize();
            Some(FrameParse::Ok(OpenedFrame { marker, plaintext }, rest))
        }
        Err(_) => {
            buf.zeroize();
            Some(FrameParse::Corrupt(rest))
        }
    }
}

/// Walk every frame in `bytes` front to back: authenticated frames are
/// collected in order, a single corrupt frame is skipped (not fatal to
/// the rest), and a truncated trailing frame stops the walk cleanly.
pub fn open_all(bytes: &[u8], key_for_marker: &dyn Fn(u64) -> DataKey) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut rest = bytes;
    loop {
        match parse_one(rest, key_for_marker) {
            None | Some(FrameParse::Truncated) => break,
            Some(FrameParse::Ok(frame, next)) => {
                out.push(frame.plaintext);
                rest = next;
            }
            Some(FrameParse::Corrupt(next)) => rest = next,
        }
    }
    out
}

/// `true` if `bytes` starts with something that looks like a well-formed
/// frame's length prefix (the implied frame fits within `bytes`) — used
/// by callers to decide "is this file the encrypted frame format, or a
/// legacy/portable plaintext file?" without attempting a decrypt. Not a
/// security check (an attacker-controlled plaintext file could coincide
/// with this shape); purely a format sniff.
pub fn looks_like_frames(bytes: &[u8]) -> bool {
    if bytes.len() < LEN_PREFIX {
        return false;
    }
    let body_len = u32::from_be_bytes(bytes[..LEN_PREFIX].try_into().unwrap()) as usize;
    body_len >= MIN_BODY_LEN && LEN_PREFIX + body_len <= bytes.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed_key(key: DataKey) -> impl Fn(u64) -> DataKey {
        move |_marker| DataKey::from_bytes(*key.as_bytes())
    }

    #[test]
    fn roundtrips_one_frame() {
        let key = DataKey::random();
        let frame = seal_frame(&key, 20260816, b"hello world");
        let opened = open_all(&frame, &fixed_key(key));
        assert_eq!(opened, vec![b"hello world".to_vec()]);
    }

    #[test]
    fn roundtrips_many_frames_concatenated() {
        let key = DataKey::random();
        let mut bytes = Vec::new();
        for line in ["one", "two", "three"] {
            bytes.extend(seal_frame(&key, 1, line.as_bytes()));
        }
        let opened = open_all(&bytes, &fixed_key(key));
        assert_eq!(
            opened,
            vec![b"one".to_vec(), b"two".to_vec(), b"three".to_vec()]
        );
    }

    #[test]
    fn a_truncated_trailing_frame_stops_cleanly_without_losing_prior_frames() {
        let key = DataKey::random();
        let mut bytes = seal_frame(&key, 1, b"complete");
        bytes.extend(seal_frame(&key, 1, b"this one gets cut off"));
        bytes.truncate(bytes.len() - 5); // chop the last frame mid-ciphertext

        let opened = open_all(&bytes, &fixed_key(key));
        assert_eq!(opened, vec![b"complete".to_vec()]);
    }

    #[test]
    fn a_corrupt_frame_is_skipped_not_fatal_to_the_rest() {
        let key = DataKey::random();
        let mut bytes = seal_frame(&key, 1, b"before");
        let corrupt_start = bytes.len();
        bytes.extend(seal_frame(&key, 1, b"corrupted"));
        // Flip a byte inside the second frame's ciphertext.
        let flip_at = corrupt_start + LEN_PREFIX + MARKER_LEN + NONCE_LEN;
        bytes[flip_at] ^= 0xFF;
        bytes.extend(seal_frame(&key, 1, b"after"));

        let opened = open_all(&bytes, &fixed_key(key));
        assert_eq!(opened, vec![b"before".to_vec(), b"after".to_vec()]);
    }

    #[test]
    fn a_rewritten_marker_fails_to_authenticate_rather_than_opening_under_the_wrong_key() {
        let key = DataKey::random();
        let mut bytes = seal_frame(&key, 20260816, b"secret");
        // The marker occupies bytes[LEN_PREFIX..LEN_PREFIX+MARKER_LEN].
        bytes[LEN_PREFIX] ^= 0xFF;

        // Even a `key_for_marker` that (implausibly) still returns the
        // right key must fail, since the AAD used at open time no longer
        // matches what was sealed.
        let opened = open_all(&bytes, &fixed_key(key));
        assert!(opened.is_empty(), "a tampered marker must not open");
    }

    #[test]
    fn wrong_key_fails_to_open() {
        let key = DataKey::random();
        let frame = seal_frame(&key, 1, b"hello");
        let opened = open_all(&frame, &fixed_key(DataKey::random()));
        assert!(opened.is_empty());
    }

    #[test]
    fn per_marker_key_selection_lets_a_file_mix_two_days_keys() {
        // Exercises the actual app-log use case: the closure derives a
        // different key depending on the (still-unauthenticated) marker
        // it's handed, and only the correct derivation opens each frame.
        let key_a = DataKey::random();
        let key_b = DataKey::random();
        let mut bytes = seal_frame(&key_a, 20260815, b"yesterday");
        bytes.extend(seal_frame(&key_b, 20260816, b"today"));

        let key_a_bytes = *key_a.as_bytes();
        let key_b_bytes = *key_b.as_bytes();
        let opened = open_all(&bytes, &move |marker| {
            if marker == 20260815 {
                DataKey::from_bytes(key_a_bytes)
            } else {
                DataKey::from_bytes(key_b_bytes)
            }
        });
        assert_eq!(opened, vec![b"yesterday".to_vec(), b"today".to_vec()]);
    }

    #[test]
    fn empty_input_opens_to_nothing() {
        let key = DataKey::random();
        assert!(open_all(&[], &fixed_key(key)).is_empty());
    }

    #[test]
    fn today_marker_looks_like_an_8_digit_yyyymmdd() {
        let marker = today_marker();
        assert!((19_000_101..=99_991_231).contains(&marker), "{marker}");
    }

    #[test]
    fn looks_like_frames_rejects_plaintext_and_accepts_a_real_frame() {
        assert!(!looks_like_frames(
            b"2026-08-16T00:00:00Z INFO starting up\n"
        ));
        assert!(!looks_like_frames(b""));
        let key = DataKey::random();
        assert!(looks_like_frames(&seal_frame(&key, 1, b"line")));
    }
}
