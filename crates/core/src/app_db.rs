//! The app-wide SQLCipher database — `<data_dir>/app.sqlite`. Holds every
//! store that used to be a plaintext JSON/raw file: conversations
//! (`crate::store`), projects (`crate::projects`), artifact metadata +
//! payloads (`crate::artifacts`), and the three single-document stores —
//! settings, custom models, plugin records (`crate::settings`/
//! `crate::custom`/`crate::plugin`).
//!
//! One connection, opened once at boot (`crates/app/src/state.rs`'s
//! `finish_unlock`, before the DEK is moved into `AppState`) and held for
//! the process's lifetime — there is exactly one of these, so it lives as a
//! plain `Arc<Mutex<Connection>>` field on `AppState`, not a cache keyed by
//! anything. Nothing here is model-callable, so there's no read-only/
//! writable split to worry about either — every access goes through narrow,
//! fixed-shape functions in the modules named above.
//!
//! Settings/custom-models/plugin-records each get their own singleton-row
//! table (`key TEXT PRIMARY KEY`, one literal `'current'` row,
//! `ON CONFLICT DO UPDATE`) rather than a generic shared key-value table —
//! there are only ever three possible keys, and this crate avoids generic
//! abstraction it doesn't need.
//!
//! Also holds `agent_audit` (`crate::agent::audit`) — the hash-chained
//! tool-call lifecycle log needs no key of its own once it's a table here,
//! since every row rides this connection's already-applied `PRAGMA key`.
//!
//! **Known gap, tracked deliberately rather than silently:** unlike skills
//! (`crate::skills::migrate_legacy_skills`, run from `finish_unlock` when
//! `app_db` is freshly created and a legacy `<data_dir>/skills/` directory
//! is found), **no equivalent migration exists yet for conversations,
//! projects, artifacts, settings, custom models, or plugin records.** An
//! install that already has plaintext `conversations/*.json`,
//! `projects/*.json`, `artifacts/*/*`, `settings.json`,
//! `custom-models.json`, or `plugins.json` on disk from before this store
//! existed will boot into a fresh, empty `app.sqlite` — those files are
//! never read, never migrated, and never deleted. Acceptable pre-launch
//! (no real user data exists to strand yet), but this must be built,
//! mirroring `migrate_legacy_skills`'s pattern for each store, before this
//! ships to anyone with an existing install — otherwise "your data is
//! encrypted" is false for every upgrading user, silently.

use crate::crypto::key::DataKey;
use rusqlite::Connection;
use std::path::Path;

/// Bumped whenever a new step is appended to [`migrate`]. There is
/// deliberately no generic migration framework — see this module's header.
pub const SCHEMA_VERSION: i64 = 11;

/// Failure while opening or migrating `app_db`.
#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    #[error("{0}")]
    Sqlite(#[from] rusqlite::Error),
    /// `PRAGMA user_version` was left at something other than
    /// [`SCHEMA_VERSION`] after running every step in [`migrate`]'s ladder —
    /// in practice this means the database was already migrated by a newer
    /// build of the app (or a dev database left over from one), and this
    /// build has no ladder step to reconcile the gap. This used to be a
    /// `debug_assert_eq!`, which panicked instead of erroring — harmless in
    /// most contexts, but `app_db::open` runs inside a Tauri `setup()`
    /// callback that cannot unwind (see `crates/app/src/lib.rs`'s crash
    /// handler), so that panic used to abort the whole process with no
    /// message at all.
    #[error(
        "the database's schema version ({found}) doesn't match what this build of OpenPal \
         supports ({supported}). It was likely created by a different version of the app"
    )]
    SchemaVersionMismatch { found: i64, supported: i64 },
}

const SCHEMA_V1: &str = "
    CREATE TABLE conversations (
        id             TEXT PRIMARY KEY,
        title          TEXT NOT NULL,
        created_at_ms  INTEGER NOT NULL,
        updated_at_ms  INTEGER NOT NULL,
        message_count  INTEGER NOT NULL,
        task_type      TEXT NOT NULL,
        project_id     TEXT,
        -- Full serde_json::to_string_pretty(&Conversation) — see crate::store's
        -- own doc comment for why this stays a blob rather than being
        -- normalized: Conversation/ChatMessage are large, deeply nested, and
        -- have accumulated #[serde(default)] fields over years. The other
        -- columns are a read-optimization cache re-derived on every save();
        -- there is exactly one write path (crate::store::save) that is
        -- allowed to touch either the blob or the cache columns.
        data           TEXT NOT NULL
    );
    CREATE INDEX conversations_by_updated ON conversations (updated_at_ms DESC);
    CREATE INDEX conversations_by_project ON conversations (project_id);

    CREATE TABLE projects (
        id             TEXT PRIMARY KEY,
        name           TEXT NOT NULL,
        created_at_ms  INTEGER NOT NULL,
        updated_at_ms  INTEGER NOT NULL
    );

    -- artifact_id is only unique within one conversation, not globally
    -- (crate::artifacts's own doc comment) — composite key matches that.
    CREATE TABLE artifacts (
        conversation_id TEXT NOT NULL,
        id              TEXT NOT NULL,
        filename        TEXT NOT NULL,
        -- Always UTF-8 text, capped at crate::artifacts::MAX_ARTIFACT_BYTES,
        -- never streamed — TEXT, not BLOB, matches the actual data shape.
        content         TEXT NOT NULL,
        size_bytes      INTEGER NOT NULL,
        PRIMARY KEY (conversation_id, id)
    );

    CREATE TABLE settings       (key TEXT PRIMARY KEY, json TEXT NOT NULL);
    CREATE TABLE custom_models  (key TEXT PRIMARY KEY, json TEXT NOT NULL);
    CREATE TABLE plugin_records (key TEXT PRIMARY KEY, json TEXT NOT NULL);
    PRAGMA user_version = 1;";

/// Added by `crate::skills`'s move onto this database — see that module's
/// own header for why this is two tables rather than a `data` blob column.
const SCHEMA_V2: &str = "
    CREATE TABLE skills (
        slug        TEXT PRIMARY KEY,
        name        TEXT NOT NULL,
        description TEXT NOT NULL,
        body        TEXT NOT NULL
    );

    -- skill_slug is a plain TEXT column, not a SQL FOREIGN KEY — this
    -- codebase's established convention (see artifacts.conversation_id for
    -- the same choice): cascades are done explicitly in application code,
    -- not via ON DELETE CASCADE / PRAGMA foreign_keys, which nothing else
    -- in app_db turns on.
    CREATE TABLE skill_files (
        skill_slug  TEXT NOT NULL,
        path        TEXT NOT NULL,
        content     TEXT NOT NULL,
        PRIMARY KEY (skill_slug, path)
    );
    CREATE INDEX skill_files_by_skill ON skill_files (skill_slug);
    PRAGMA user_version = 2;";

/// Added by `crate::agent::audit`'s move onto this database — see that
/// module's own header for why the hash-chained log needs no key of its
/// own once it's a table here. `line` carries the same JSON-with-`prevHash`
/// shape `AuditLog::append` built for the old standalone file; `id`
/// (not `ts_ms`) is the chain's ordering column, since two events in the
/// same millisecond must still have an unambiguous order.
const SCHEMA_V3: &str = "
    CREATE TABLE agent_audit (
        id     INTEGER PRIMARY KEY AUTOINCREMENT,
        ts_ms  INTEGER NOT NULL,
        line   TEXT NOT NULL
    );
    CREATE INDEX agent_audit_by_ts ON agent_audit (ts_ms);
    PRAGMA user_version = 3;";

/// Added by `crate::remote`'s Remote Access feature (Settings → Devices):
/// `remote_access_config` is a fourth singleton-row store (see this module's
/// own header on why settings/custom-models/plugin-records each get their
/// own row rather than a generic key-value table) holding the gateway's
/// enabled flag, persisted port, and self-signed TLS cert/key — persisted
/// (unlike the engine's per-start random port) because a remote client
/// needs a *stable* address to reconnect to across desktop restarts/wakes,
/// especially over a port-forward or overlay-network path where re-running
/// LAN discovery isn't an option. `paired_devices` is a real multi-row
/// table, one row per device a pairing session has completed for; the
/// token itself is never stored, only a salted hash (`crate::remote::store`
/// mirrors `applock.rs`'s `hash_pin`/`new_salt` shape) so a stolen database
/// backup can't be replayed as a bearer token.
const SCHEMA_V4: &str = "
    CREATE TABLE remote_access_config (key TEXT PRIMARY KEY, json TEXT NOT NULL);

    CREATE TABLE paired_devices (
        id             TEXT PRIMARY KEY,
        name           TEXT NOT NULL,
        token_hash     TEXT NOT NULL,
        token_salt     TEXT NOT NULL,
        created_at_ms  INTEGER NOT NULL,
        last_seen_ms   INTEGER
    );
    PRAGMA user_version = 4;";

/// Added by `crate::sync::roster`'s move onto this database —
/// `docs/SYNC_PAIRING_PROTOCOL.md`'s "Group creation" step 5: the local
/// record of every device in this device's sync group, including itself
/// (`is_self`). Covered by this database's existing SQLCipher/DEK
/// encryption — no new wrap file needed, this is local metadata, not shared
/// secret material. `is_self` follows this crate's existing convention for
/// a SQL boolean column (`INTEGER`, `0`/`1` — same as every other integer
/// column here; nothing in `app_db` uses SQLite's nonstandard `BOOLEAN`
/// affinity name). Numbered V5 (not V4) because `crate::remote`'s Remote
/// Access feature independently claimed V4 for `remote_access_config`/
/// `paired_devices` on `main` first — same underlying pattern (a new
/// device/pairing-adjacent table), different feature, reconciled here by
/// simply ordering sync's migrations after Remote Access's rather than
/// picking a colliding number.
const SCHEMA_V5: &str = "
    CREATE TABLE sync_devices (
        device_id          TEXT PRIMARY KEY,
        identity_sign_pub  BLOB NOT NULL,
        box_pub            BLOB NOT NULL,
        label              TEXT NOT NULL,
        gen_added          INTEGER NOT NULL,
        is_self            INTEGER NOT NULL DEFAULT 0
    );
    PRAGMA user_version = 5;";

/// Phase 3 conversation-data sync (`docs/design/MULTI_DEVICE_SYNC.md`'s Phase 3,
/// the plan behind `crate::sync::conversations`/`crate::sync::field_versions`).
///
/// `sync_field_versions`: generic last-writer-wins version tracking, one
/// row per `(scope, row_key)` — `scope` names which table/field is being
/// versioned (`"projects"`/`"artifacts"`/`"settings"`/`"custom_models"` for
/// whole-row LWW on those existing tables; `"conversation_title"`/
/// `"conversation_project"` as two INDEPENDENT registers so editing one
/// never clobbers a concurrent edit to the other; `"conversation_snapshot"`
/// as the whole-conversation LWW fallback used when a rewrite/truncation is
/// detected). `row_key` is the row's own primary key (`"current"` for the
/// singleton settings/custom_models tables).
///
/// `sync_message_ops`: the per-message append-log conversations' messages
/// merge through instead of LWW-replacing — `op_id` is
/// `"{hlc_ms}-{hlc_counter}-{hlc_device_id}"`, `tombstoned` excludes an op
/// from the canonical-order recompute (set when a rewrite supersedes it).
///
/// `sync_outbox`: locally-queued encrypted envelopes waiting to be pushed
/// to the relay, written in the SAME transaction as the local write that
/// produced them (see `crate::sync::conversations::apply`'s doc comment)
/// so a crash between \"saved locally\" and \"queued to send\" can't happen.
const SCHEMA_V6: &str = "
    CREATE TABLE sync_field_versions (
        scope         TEXT NOT NULL,
        row_key       TEXT NOT NULL,
        hlc_ms        INTEGER NOT NULL,
        hlc_counter   INTEGER NOT NULL,
        hlc_device_id TEXT NOT NULL,
        PRIMARY KEY (scope, row_key)
    );
    CREATE TABLE sync_message_ops (
        op_id           TEXT PRIMARY KEY,
        conversation_id TEXT NOT NULL,
        hlc_ms          INTEGER NOT NULL,
        hlc_counter     INTEGER NOT NULL,
        hlc_device_id   TEXT NOT NULL,
        message_json    TEXT NOT NULL,
        tombstoned      INTEGER NOT NULL DEFAULT 0
    );
    CREATE INDEX sync_message_ops_by_conv
        ON sync_message_ops (conversation_id, tombstoned, hlc_ms, hlc_counter, hlc_device_id);
    CREATE TABLE sync_outbox (
        envelope_id   TEXT PRIMARY KEY,
        mailbox_id    TEXT NOT NULL,
        ciphertext    BLOB NOT NULL,
        created_at_ms INTEGER NOT NULL
    );
    PRAGMA user_version = 6;";

/// Adds a bearer-token expiry to `paired_devices` (`crate::remote::store`).
/// Previously a paired device's token never expired — a captured/logged
/// token granted indefinite access. Backfills existing rows to
/// `created_at_ms + 90d` (`crate::remote::store::TOKEN_LIFETIME_MS`) so an
/// upgrading install doesn't retroactively lock out devices paired before
/// this column existed.
const SCHEMA_V7: &str = "
    ALTER TABLE paired_devices ADD COLUMN expires_at_ms INTEGER NOT NULL DEFAULT 0;
    UPDATE paired_devices SET expires_at_ms = created_at_ms + 7776000000 WHERE expires_at_ms = 0;
    PRAGMA user_version = 7;";

/// The authoritative, persisted "current sync-key generation" register
/// (`crate::sync::key_state`) — previously nothing played this role.
/// `crate::sync::pairing::next_gen_n` recomputed `max(sync_devices.gen_added)`,
/// which is write-once pairing metadata, not a live counter, so it never
/// actually advanced after a device-removal key rotation. Singleton row
/// (`CHECK (id = 1)`, same idiom as `remote_access_config`'s
/// `key = 'current'`, but typed integer columns since this needs arithmetic
/// comparison, not just blob storage). `envelopes_since_rotation` backs the
/// count-based half of the periodic-rotation trigger (see
/// `docs/SYNC_PAIRING_PROTOCOL.md`'s \"Revocation and rotation\").
const SCHEMA_V8: &str = "
    CREATE TABLE sync_key_state (
        id                       INTEGER PRIMARY KEY CHECK (id = 1),
        current_gen_n            INTEGER NOT NULL DEFAULT 0,
        key_installed_at_ms      INTEGER NOT NULL DEFAULT 0,
        envelopes_since_rotation INTEGER NOT NULL DEFAULT 0
    );
    PRAGMA user_version = 8;";

/// Phase 4 artifact sync's local dedup cache (`crate::sync::
/// artifact_push_state`) — one row per `(conversation_id, artifact_id)`
/// recording the SHA-256 hex digest of the content that was last *enqueued*
/// for sync, so the periodic reconciliation sweep
/// (`crate::sync::artifact_sweep`) can tell \"already pushed, nothing
/// changed\" from \"new or edited since last sweep\" without re-sending
/// unchanged content every pass. Deliberately a separate table from
/// `sync_field_versions`: that table answers \"who wins a cross-device
/// conflict\" (an `Hlc`), this one answers \"is the current content the
/// same thing I already queued\" (a content hash) — a local-only dedup
/// cache never itself synced or compared against a remote value, not a
/// version register.
const SCHEMA_V9: &str = "
    CREATE TABLE sync_artifact_push_state (
        conversation_id TEXT NOT NULL,
        artifact_id     TEXT NOT NULL,
        content_hash    TEXT NOT NULL,
        PRIMARY KEY (conversation_id, artifact_id)
    );
    PRAGMA user_version = 9;";

/// The sidebar's "Pin" conversation-row action. `pinned` deliberately does
/// NOT get a `sync_field_versions` register of its own (unlike
/// `conversation_title`/`conversation_project` — see
/// `crate::sync::conversations`'s top doc comment): it rides along in
/// `crate::store::save`'s row the same way `sampling`/`context_length`/
/// `task_type` already do, saved locally on every write but not itself
/// broadcast to other paired devices. Same `INTEGER 0/1` convention as
/// `sync_devices.is_self` (SCHEMA_V5) — nothing in `app_db` uses SQLite's
/// nonstandard `BOOLEAN` affinity name.
const SCHEMA_V10: &str = "
    ALTER TABLE conversations ADD COLUMN pinned INTEGER NOT NULL DEFAULT 0;
    PRAGMA user_version = 10;";

/// A pure version-ladder marker — `ChatMessage.id`/`Conversation.branches`
/// (edit/retry history) ride inside the existing `conversations.data` JSON
/// blob, so no `ALTER TABLE`/new column is needed here. This step exists
/// solely so [`open`] can tell `crate::state::finish_unlock` "a
/// pre-existing (non-fresh) database just crossed this version," gating a
/// one-time call to `crate::store::backfill_message_ids` the same way
/// `app_db_was_fresh` already gates `crate::skills::migrate_legacy_skills`
/// — see `backfill_message_ids`'s doc comment for why a random
/// `#[serde(default)]` id isn't safe to rely on without this explicit,
/// persisted, run-exactly-once backfill.
const SCHEMA_V11: &str = "PRAGMA user_version = 11;";

/// Bring `conn`'s schema up to [`SCHEMA_VERSION`]. Ladder shape (`if
/// version == N { ...; version = N+1 }`) — a fresh install has no existing
/// data to carry forward (the one-time migration of pre-existing plaintext
/// installs is separate, per-store work — see e.g. `crate::skills`'s own
/// `migrate_legacy_skills`).
pub(crate) fn migrate(conn: &Connection) -> Result<(), OpenError> {
    let mut version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version == 0 {
        conn.execute_batch(SCHEMA_V1)?;
        version = 1;
    }
    if version == 1 {
        conn.execute_batch(SCHEMA_V2)?;
        version = 2;
    }
    if version == 2 {
        conn.execute_batch(SCHEMA_V3)?;
        version = 3;
    }
    if version == 3 {
        conn.execute_batch(SCHEMA_V4)?;
        version = 4;
    }
    if version == 4 {
        conn.execute_batch(SCHEMA_V5)?;
        version = 5;
    }
    if version == 5 {
        conn.execute_batch(SCHEMA_V6)?;
        version = 6;
    }
    if version == 6 {
        conn.execute_batch(SCHEMA_V7)?;
        version = 7;
    }
    if version == 7 {
        conn.execute_batch(SCHEMA_V8)?;
        version = 8;
    }
    if version == 8 {
        conn.execute_batch(SCHEMA_V9)?;
        version = 9;
    }
    if version == 9 {
        conn.execute_batch(SCHEMA_V10)?;
        version = 10;
    }
    if version == 10 {
        conn.execute_batch(SCHEMA_V11)?;
        version = 11;
    }
    if version != SCHEMA_VERSION {
        return Err(OpenError::SchemaVersionMismatch {
            found: version,
            supported: SCHEMA_VERSION,
        });
    }
    Ok(())
}

/// Opens (creating the file and its parent directory if absent) and brings
/// the schema up to date. `dek: None` opens unkeyed (portable mode, which
/// doesn't mint a DEK — see `AppState.dek`'s doc comment). Returns
/// `(connection, was_fresh, version_before)`: `was_fresh` is whether this was
/// a brand-new database (`user_version` was `0` before migrating) —
/// `crate::state::finish_unlock` uses this to decide whether a legacy
/// on-disk store (e.g. `crate::skills`'s old `<data_dir>/skills/` files)
/// needs a one-time migration into it, versus this being an ordinary
/// steady-state relaunch. `version_before` is the raw `user_version` seen
/// before migrating (not just a freshness bit) — `finish_unlock` also uses
/// it to gate `crate::store::backfill_message_ids`'s one-time run on
/// `version_before < 11`, the same shape as the `was_fresh` gate but for a
/// migration that only pre-existing (not brand-new) databases need.
pub fn open(path: &Path, dek: Option<&DataKey>) -> Result<(Connection, bool, i64), OpenError> {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let conn = Connection::open(path)?;
    if let Some(dek) = dek {
        crate::crypto::sqlcipher::apply_key(&conn, dek)?;
    }
    let version_before: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    let was_fresh = version_before == 0;
    migrate(&conn)?;
    Ok((conn, was_fresh, version_before))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_creates_file_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.sqlite");
        assert!(!path.exists());

        drop(open(&path, None).unwrap());
        assert!(path.exists());

        // Re-opening must not wipe the schema (the CREATE TABLE guard only
        // fires once, on user_version == 0).
        let (conn, _, _) = open(&path, None).unwrap();
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
    }

    #[test]
    fn open_reports_freshness_correctly() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.sqlite");

        let (_conn, was_fresh, _) = open(&path, None).unwrap();
        assert!(was_fresh, "a brand-new file must report fresh");

        let (_conn, was_fresh, _) = open(&path, None).unwrap();
        assert!(
            !was_fresh,
            "re-opening an existing database must not report fresh"
        );
    }

    #[test]
    fn open_creates_every_table() {
        let dir = tempfile::tempdir().unwrap();
        let (conn, _, _) = open(&dir.path().join("app.sqlite"), None).unwrap();
        for table in [
            "conversations",
            "projects",
            "artifacts",
            "settings",
            "custom_models",
            "plugin_records",
            "skills",
            "skill_files",
            "agent_audit",
            "remote_access_config",
            "paired_devices",
            "sync_devices",
            "sync_field_versions",
            "sync_message_ops",
            "sync_outbox",
            "sync_artifact_push_state",
        ] {
            conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap_or_else(|e| panic!("table {table} missing or unqueryable: {e}"));
        }
    }

    #[test]
    fn unkeyed_open_produces_a_plaintext_readable_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.sqlite");
        drop(open(&path, None).unwrap());
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.starts_with(b"SQLite format 3\0"));
    }

    #[test]
    fn keyed_open_does_not_produce_a_plaintext_readable_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.sqlite");
        let dek = DataKey::random();
        drop(open(&path, Some(&dek)).unwrap());
        let bytes = std::fs::read(&path).unwrap();
        assert!(!bytes.starts_with(b"SQLite format 3\0"));
    }

    #[test]
    fn correct_key_reopens() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.sqlite");
        let dek = DataKey::random();
        {
            let (conn, _, _) = open(&path, Some(&dek)).unwrap();
            conn.execute(
                "INSERT INTO settings (key, json) VALUES ('current', '{}')",
                [],
            )
            .unwrap();
        }
        let (conn, _, _) = open(&path, Some(&dek)).unwrap();
        let json: String = conn
            .query_row("SELECT json FROM settings WHERE key = 'current'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(json, "{}");
    }

    #[test]
    fn wrong_key_fails_to_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.sqlite");
        let dek_a = DataKey::random();
        let dek_b = DataKey::random();
        drop(open(&path, Some(&dek_a)).unwrap());
        assert!(open(&path, Some(&dek_b)).is_err());
    }

    #[test]
    fn unkeyed_open_of_a_keyed_database_fails() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.sqlite");
        let dek = DataKey::random();
        drop(open(&path, Some(&dek)).unwrap());
        assert!(open(&path, None).is_err());
    }

    #[test]
    fn a_schema_version_this_build_does_not_recognize_errors_instead_of_panicking() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.sqlite");
        drop(open(&path, None).unwrap());
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(&format!("PRAGMA user_version = {}", SCHEMA_VERSION + 1))
                .unwrap();
        }
        match open(&path, None) {
            Err(OpenError::SchemaVersionMismatch { found, supported }) => {
                assert_eq!(found, SCHEMA_VERSION + 1);
                assert_eq!(supported, SCHEMA_VERSION);
            }
            other => panic!("expected SchemaVersionMismatch, got {other:?}"),
        }
    }
}
