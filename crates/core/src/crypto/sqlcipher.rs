//! `PRAGMA key`-application shared by every SQLCipher connection in this
//! crate — the per-space store (`crate::spaces::db`) and the app-wide store
//! (`crate::app_db`) both key their connections through [`apply_key`], so
//! the security-critical hex-literal construction and eager-validation
//! logic lives in exactly one place rather than two independently
//! maintained copies.

use super::key::DataKey;
use rusqlite::Connection;
use zeroize::Zeroizing;

/// `PRAGMA key = "x'<64 hex>'"` is SQLCipher's raw-key-as-hex-blob-literal
/// syntax — not a string, so this must be built as an exact literal and run
/// via `execute_batch`, never through `pragma_update`'s typed helper, which
/// would auto-quote the value as a SQL string and corrupt the syntax.
/// Applied immediately after opening and before any other statement
/// (including a migration), on both read-only and writable connections —
/// SQLCipher decrypts on read too.
///
/// `PRAGMA key` itself never fails, even for a wrong key: SQLCipher only
/// validates lazily, on first page read. Without a probe here, a wrong-key
/// connection would sail through and only fail confusingly later, inside a
/// migration's own `PRAGMA user_version` read or some later command. The
/// cheap `sqlite_master` read below forces that validation now, so a wrong
/// key fails right here with a clear, catchable error. On a brand-new empty
/// file this is a no-op cost — there is nothing to validate yet, and the
/// first write establishes the header under whatever key was set.
pub(crate) fn apply_key(conn: &Connection, dek: &DataKey) -> rusqlite::Result<()> {
    let hex = to_hex(dek.as_bytes());
    // Wrapped in `Zeroizing` immediately at construction — `format!` itself
    // has no zeroizing variant, so the raw hex DEK would otherwise sit in an
    // ordinary, unscrubbed `String` allocation for the length of this call.
    let pragma: Zeroizing<String> = Zeroizing::new(format!("PRAGMA key = \"x'{}'\";", *hex));
    conn.execute_batch(&pragma)?;
    conn.query_row("SELECT count(*) FROM sqlite_master", [], |_| Ok(()))
        .map_err(|_| {
            rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_NOTADB),
                Some("wrong key, or the database file is damaged".to_string()),
            )
        })?;
    Ok(())
}

/// Hand-rolled rather than a new direct dependency on the `hex` crate (which
/// is presently only transitive) — matches this crate's existing convention
/// of hand-rolling small, self-contained encoders (see `recovery.rs`'s own
/// Crockford Base32).
fn to_hex(bytes: &[u8; 32]) -> Zeroizing<String> {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(64);
    for byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    Zeroizing::new(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_hex_encodes_all_256_byte_values() {
        let bytes: [u8; 32] = std::array::from_fn(|i| i as u8);
        let hex = to_hex(&bytes);
        assert_eq!(
            hex.as_str(),
            "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f"
        );
    }

    #[test]
    fn correct_key_reopens_but_a_wrong_key_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.sqlite");
        let dek_a = DataKey::random();
        let dek_b = DataKey::random();

        {
            let conn = Connection::open(&path).unwrap();
            apply_key(&conn, &dek_a).unwrap();
            conn.execute_batch("CREATE TABLE t (x INTEGER); INSERT INTO t VALUES (1);")
                .unwrap();
        }

        let conn = Connection::open(&path).unwrap();
        apply_key(&conn, &dek_a).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM t", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);

        let conn = Connection::open(&path).unwrap();
        assert!(apply_key(&conn, &dek_b).is_err());
    }
}
