//! Recovery code generation and normalization — the second wrap-file slot's
//! "passphrase" (`docs/SPACES_AUTHORING.md` §9's recovery slot). This is
//! **not** typed by the user: the app generates it, shows it once, and asks
//! the user to save it somewhere safe (a password manager, printed out,
//! etc.). It exists purely so a lost OS keychain entry — an OS reinstall, a
//! "Reset Keychain", a corrupted keychain database — doesn't mean
//! permanently unrecoverable data: `crates/app/src/commands.rs`'s
//! `confirm_recovery_code` seals a second copy of the data-encryption key
//! under a key derived from this code (see `super::kdf`), written alongside
//! the keychain-sealed slot as `<data>/keys/recovery.wrap`.
//!
//! Encoded as Crockford Base32 (excludes `I`/`L`/`O`/`U`, which are easy to
//! misread as `1`/`1`/`0`/`V` when handwritten or read aloud), hand-rolled
//! here rather than pulling in a crate for it — this repo's convention for a
//! small, self-contained utility like this one (see `docs/SPACES_AUTHORING.md`'s
//! own dependency table, which makes the same call for comparable cases).

use rand::rngs::OsRng;
use rand::RngCore;

const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Raw entropy behind a generated code, in bytes.
const CODE_BYTES: usize = 16;

/// Characters per hyphen-separated group in the formatted code.
const GROUP_LEN: usize = 5;

/// 16 random bytes (`rand::rngs::OsRng` — this crate's established "all
/// randomness via OsRng, never `thread_rng`" rule, see
/// `super::key::DataKey::random` for the precedent) encoded as 26 Crockford
/// Base32 characters, formatted in hyphen-separated groups of 5 for
/// readability (e.g. `"XXXXX-XXXXX-XXXXX-XXXXX-XXXXX-X"` — 5 full groups
/// plus a 1-character remainder for 26 characters total).
pub fn generate() -> String {
    let mut bytes = [0u8; CODE_BYTES];
    OsRng.fill_bytes(&mut bytes);
    group(&encode(&bytes))
}

/// Uppercases and strips everything that isn't in the Crockford alphabet
/// (hyphens, whitespace, anything else) — so display formatting never
/// affects the key derived from a code, and this same function can later
/// normalize a recovery code the user re-types during actual recovery (a
/// future phase).
pub fn normalize(input: &str) -> String {
    input
        .chars()
        .filter_map(|c| {
            let upper = c.to_ascii_uppercase();
            (upper.is_ascii() && ALPHABET.contains(&(upper as u8))).then_some(upper)
        })
        .collect()
}

/// 5-bits-at-a-time Crockford Base32, no padding character — the last group
/// is simply short instead. `CODE_BYTES` bytes is `CODE_BYTES * 8` bits,
/// which for 16 bytes is 128 bits: 25 full 5-bit groups plus a 3-bit
/// remainder, so this always emits exactly 26 characters for `generate`'s
/// input length.
/// `pub(crate)`, not `pub`: reused as-is by `crate::sync::pairing` to format
/// the pairing fingerprint (a truncated hash, not a recovery code) with the
/// same Crockford Base32 alphabet — see that module for why reusing this
/// exact encoder rather than a second implementation matters.
pub(crate) fn encode(bytes: &[u8]) -> String {
    let mut buffer: u32 = 0;
    let mut bits_in_buffer: u32 = 0;
    let mut out = String::with_capacity((bytes.len() * 8).div_ceil(5));
    for &byte in bytes {
        buffer = (buffer << 8) | u32::from(byte);
        bits_in_buffer += 8;
        while bits_in_buffer >= 5 {
            bits_in_buffer -= 5;
            let index = (buffer >> bits_in_buffer) & 0x1F;
            out.push(ALPHABET[index as usize] as char);
        }
    }
    if bits_in_buffer > 0 {
        let index = (buffer << (5 - bits_in_buffer)) & 0x1F;
        out.push(ALPHABET[index as usize] as char);
    }
    out
}

/// Inserts a `-` every `GROUP_LEN` characters — purely cosmetic; see
/// `normalize` for undoing it.
pub(crate) fn group(code: &str) -> String {
    code.as_bytes()
        .chunks(GROUP_LEN)
        .map(|chunk| std::str::from_utf8(chunk).expect("Crockford alphabet is ASCII"))
        .collect::<Vec<_>>()
        .join("-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_uses_only_the_crockford_alphabet_and_hyphens() {
        let code = generate();
        assert!(code
            .chars()
            .all(|c| c == '-' || ALPHABET.contains(&(c as u8))));
    }

    #[test]
    fn two_calls_produce_different_codes() {
        assert_ne!(generate(), generate());
    }

    #[test]
    fn normalize_strips_hyphens_and_whitespace_and_uppercases() {
        assert_eq!(normalize("ab12-cd34 ef56"), "AB12CD34EF56");
    }

    #[test]
    fn normalize_is_idempotent() {
        let once = normalize("ab12-cd34 ef56");
        assert_eq!(normalize(&once), once);
    }

    #[test]
    fn normalize_agrees_regardless_of_hyphen_placement_and_extra_whitespace() {
        let generated = generate();
        let raw: String = generated.chars().filter(|c| *c != '-').collect();
        // Re-hyphenate/space the same raw characters completely differently
        // from `group`'s own 5-char grouping.
        let reformatted = format!("  {}-{}\n {}", &raw[..3], &raw[3..11], &raw[11..]);
        assert_eq!(normalize(&generated), normalize(&reformatted));
    }
}
