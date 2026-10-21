//! Per-message merge for `conversations.messages` — the one synced table
//! that merges rather than last-writer-wins-replaces (see
//! `crate::sync::field_versions` for the other four tables and for
//! `conversations.title`/`project_id`, which use LWW independently of this
//! module).
//!
//! **Why this exists instead of simpler whole-row LWW**: two devices both
//! being used between sync sessions (chat on a phone during a commute,
//! then on a desktop that evening) is this feature's single most common
//! real scenario, and whole-row LWW would make that the case where an
//! entire session's messages get shelved into a conflict file instead of
//! merged. A 3-angle design review of an earlier draft found and fixed
//! real bugs before this was written — see this module's function-level
//! doc comments for what each specific mechanism defends against.
//!
//! **Atomicity**: [`apply_local`]/[`apply_remote_message`]/
//! [`apply_remote_snapshot`] all take `&mut Connection` and run their
//! entire read-diff-write sequence inside one `rusqlite::Transaction`
//! (`BEGIN IMMEDIATE`). A design review found that a naive "poll and
//! notice the local array grew" capture design lets a remote merge and an
//! untracked local append race and silently drop the local message — the
//! fix isn't a *new* lock: `crates/app`'s `AppState.app_db` is already a
//! single `std::sync::Mutex<Connection>` that every command (sync or not)
//! already serializes through. As long as **both** the local write path
//! and the remote-apply path go through *this module's* functions — which
//! callers must invoke while already holding that one lock, exactly like
//! every other command touching `app_db` — two operations for the same (or
//! different) conversations can never interleave their read-modify-write
//! sequences. The transaction adds crash-atomicity (all-or-nothing on
//! power loss); the pre-existing global lock adds the concurrency
//! guarantee. Two separate properties, two separate (both already
//! available) mechanisms — no new mutex needed.
//!
//! **Scope note**: this module never touches encryption or the relay. It
//! returns plain data (newly-minted ops/snapshots) for the caller (the
//! sync engine, `crates/app/src/sync_loop.rs`) to encrypt and enqueue into
//! `sync_outbox` separately. A crash between "this module's transaction
//! committed" and "the caller enqueued the outbox row" is a recoverable
//! gap (the local data is already correct and saved; at worst that one
//! change doesn't get pushed until the next reconciliation pass notices an
//! op with no matching outbox row) — a much narrower risk than the
//! silent-permanent-loss bug this design already closed by construction.

use crate::sync::field_versions;
use crate::sync::hlc::{Hlc, HlcClock};
use crate::types::{ChatMessage, Conversation};
use crate::{Error, Result};
use rusqlite::{params, Connection};
use std::path::Path;

/// A newly-minted per-message op this device just produced (a local pure
/// append, or one of the reseed ops after a rewrite/first-seed) — the
/// caller encrypts and pushes each of these as its own envelope.
#[derive(Debug, Clone)]
pub struct MintedMessageOp {
    pub hlc: Hlc,
    pub message: ChatMessage,
}

/// Result of [`apply_local`] — what (if anything) needs to be broadcast to
/// other devices as a result of this save. Multiple fields can be set at
/// once (e.g. a save that changes both the title and appends a message).
#[derive(Debug, Default)]
pub struct AppliedLocal {
    /// New per-message ops to broadcast individually — set for a pure
    /// append, or for the reseed ops that follow a rewrite (see
    /// [`apply_local`]'s rewrite branch). Empty for a save that only
    /// touched title/project_id.
    pub message_ops: Vec<MintedMessageOp>,
    /// Set instead of (never alongside a non-empty) `message_ops` when this
    /// save was a rewrite/truncation — broadcast the whole conversation as
    /// one snapshot envelope rather than per-message ops, so other devices
    /// can detect the history rewrite via [`apply_remote_snapshot`] rather
    /// than trying to interpret it as ordinary appends.
    pub snapshot: Option<(Hlc, Conversation)>,
    pub title_update: Option<(Hlc, String)>,
    pub project_update: Option<(Hlc, Option<String>)>,
}

/// Called by the chat-send/rename/move-to-project flow **instead of**
/// `crate::store::save` directly, whenever sync is enabled — this is the
/// integration point that closes the local/remote race described in this
/// module's top doc comment. Diffs `conversation` against what's currently
/// recorded for its id, classifies the write, applies it, and calls
/// `crate::store::save` itself (unchanged function, called from here
/// instead of by the original caller).
pub fn apply_local(
    conn: &mut Connection,
    conversation: &Conversation,
    clock: &HlcClock,
) -> Result<AppliedLocal> {
    let tx = conn.transaction()?;
    let mut result = AppliedLocal::default();

    // --- title / project_id: independent LWW registers, unrelated to the
    // message-merge logic below. A local write always wins trivially. ---
    if let Ok(previous) = crate::store::get(&tx, &conversation.id) {
        if previous.title != conversation.title {
            let hlc =
                field_versions::record_local(&tx, "conversation_title", &conversation.id, clock)?;
            result.title_update = Some((hlc, conversation.title.clone()));
        }
        if previous.project_id != conversation.project_id {
            let hlc =
                field_versions::record_local(&tx, "conversation_project", &conversation.id, clock)?;
            result.project_update = Some((hlc, conversation.project_id.clone()));
        }
    }
    // A brand-new conversation (no previous row) needs no title/project
    // broadcast of its own — it's covered by the message seed/append path
    // below carrying the conversation's initial state through `store::save`.

    // --- messages: seed / pure-append / rewrite ---
    if !is_seeded(&tx, &conversation.id)? {
        seed(
            &tx,
            &conversation.id,
            &conversation.messages,
            clock.device_id(),
        )?;
        result.message_ops = conversation
            .messages
            .iter()
            .enumerate()
            .map(|(i, message)| MintedMessageOp {
                hlc: Hlc::synthetic(i as u32, clock.device_id()),
                message: message.clone(),
            })
            .collect();
    } else {
        let current = load_ordered_messages(&tx, &conversation.id)?;
        if is_pure_append(&current, &conversation.messages) {
            let new_tail = &conversation.messages[current.len()..];
            for message in new_tail {
                let hlc = clock.next();
                insert_op(&tx, &conversation.id, &hlc, message)?;
                result.message_ops.push(MintedMessageOp {
                    hlc,
                    message: message.clone(),
                });
            }
        } else if !messages_equal(&current, &conversation.messages) {
            // Rewrite/truncation (e.g. edit-and-regenerate, which drops a
            // suffix and appends a new one — see this module's top doc
            // comment on why messages aren't actually append-only).
            let hlc = clock.next();
            field_versions::record_local(&tx, "conversation_snapshot", &conversation.id, clock)?;
            reseed_after_rewrite(&tx, &conversation.id, &conversation.messages, &hlc)?;
            result.snapshot = Some((hlc, conversation.clone()));
        }
        // else: current == conversation.messages, nothing changed about the
        // message list on this save (e.g. a title-only edit) — no-op here.
    }

    crate::store::save(&tx, conversation)?;
    tx.commit()?;
    Ok(result)
}

/// Applies one received `AppendMessage` op. Idempotent — applying the same
/// `(hlc, message)` twice is a no-op the second time (checked via
/// `op_id` as the primary key, so a relay redelivery is harmless). Returns
/// `true` if this was new and the local `messages` array was
/// recomputed+saved, `false` if it was already applied.
pub fn apply_remote_message(
    conn: &mut Connection,
    conversation_id: &str,
    hlc: Hlc,
    message: ChatMessage,
) -> Result<bool> {
    let tx = conn.transaction()?;
    let inserted = insert_op(&tx, conversation_id, &hlc, &message)?;
    if inserted {
        recompute_and_save(&tx, conversation_id)?;
    }
    tx.commit()?;
    Ok(inserted)
}

/// Batched version of [`apply_remote_message`] for catch-up after being
/// offline — inserts every op (each dedup-checked) but recomputes+saves
/// `store::save` at most once per distinct conversation touched in this
/// batch, not once per op. A large catch-up (hundreds of messages after
/// days offline) would otherwise do a full array-rebuild-and-rewrite per
/// message for what the user experiences as one sync.
pub fn apply_remote_messages_batch(
    conn: &mut Connection,
    ops: Vec<(String, Hlc, ChatMessage)>,
) -> Result<()> {
    let tx = conn.transaction()?;
    let mut dirty = std::collections::HashSet::new();
    for (conversation_id, hlc, message) in ops {
        if insert_op(&tx, &conversation_id, &hlc, &message)? {
            dirty.insert(conversation_id);
        }
    }
    for conversation_id in dirty {
        recompute_and_save(&tx, &conversation_id)?;
    }
    tx.commit()?;
    Ok(())
}

/// Applies (or rejects) one received `ConversationSnapshot` envelope — the
/// rewrite/truncation fallback (see [`apply_local`]'s rewrite branch and
/// this module's top doc comment). Compares `hlc` against
/// `field_versions` scope `"conversation_snapshot"`: if it wins, replaces
/// the local conversation wholesale (title/project_id/messages, via
/// `crate::store::save`) and reseeds the op log from the new array so
/// future pure appends from any device merge normally again; if it loses,
/// writes `conversation` to a conflict file under `conflicts_dir` instead
/// of applying it — never silently dropped. Returns `true` if applied,
/// `false` if it lost and was written to a conflict file.
pub fn apply_remote_snapshot(
    conn: &mut Connection,
    conflicts_dir: &Path,
    hlc: Hlc,
    conversation: Conversation,
) -> Result<bool> {
    let tx = conn.transaction()?;
    let wins =
        field_versions::try_apply_remote(&tx, "conversation_snapshot", &conversation.id, &hlc)?;
    if !wins {
        tx.commit()?; // still commit: try_apply_remote's "loses" path made no writes, but nothing else pending either
        write_conflict_file(conflicts_dir, &conversation.id, &hlc, &conversation)?;
        return Ok(false);
    }
    reseed_after_rewrite(&tx, &conversation.id, &conversation.messages, &hlc)?;
    crate::store::save(&tx, &conversation)?;
    tx.commit()?;
    Ok(true)
}

/// Applies (or rejects) a received title update, independent of any
/// project_id update happening concurrently (see this module's top doc
/// comment on why title/project_id are separate LWW registers). Returns
/// `true` if applied (and `crate::store::save` called with the new title),
/// `false` if it lost against a newer local edit — discarded; for a single
/// scalar field there's no meaningful conflict-file content beyond the one
/// value already visible in the losing envelope's own delivery history,
/// unlike a snapshot conflict, which is full conversation content that
/// could otherwise be lost.
pub fn apply_remote_title(
    conn: &Connection,
    conversation_id: &str,
    hlc: Hlc,
    title: String,
) -> Result<bool> {
    let wins = field_versions::try_apply_remote(conn, "conversation_title", conversation_id, &hlc)?;
    if wins {
        let mut conversation = crate::store::get(conn, conversation_id)?;
        conversation.title = title;
        crate::store::save(conn, &conversation)?;
    }
    Ok(wins)
}

/// Applies (or rejects) a received project_id update — see
/// [`apply_remote_title`]'s doc comment, same shape, independent register.
pub fn apply_remote_project(
    conn: &Connection,
    conversation_id: &str,
    hlc: Hlc,
    project_id: Option<String>,
) -> Result<bool> {
    let wins =
        field_versions::try_apply_remote(conn, "conversation_project", conversation_id, &hlc)?;
    if wins {
        let mut conversation = crate::store::get(conn, conversation_id)?;
        conversation.project_id = project_id;
        crate::store::save(conn, &conversation)?;
    }
    Ok(wins)
}

/// Applies (or rejects) a received "New reply" hint change — see
/// [`apply_remote_title`]'s doc comment, same shape, independent register
/// (`"conversation_unread"`). Writes through `crate::store::set_unread`, not
/// `crate::store::save`: the flag isn't part of `Conversation`.
pub fn apply_remote_unread(
    conn: &Connection,
    conversation_id: &str,
    hlc: Hlc,
    unread: bool,
) -> Result<bool> {
    let wins =
        field_versions::try_apply_remote(conn, "conversation_unread", conversation_id, &hlc)?;
    if wins {
        crate::store::set_unread(conn, conversation_id, unread)?;
    }
    Ok(wins)
}

/// Applies (or rejects) a received pin/unpin — see [`apply_remote_title`]'s
/// doc comment, same shape, independent register (`"conversation_pinned"`).
pub fn apply_remote_pinned(
    conn: &Connection,
    conversation_id: &str,
    hlc: Hlc,
    pinned: bool,
) -> Result<bool> {
    let wins =
        field_versions::try_apply_remote(conn, "conversation_pinned", conversation_id, &hlc)?;
    if wins {
        let mut conversation = crate::store::get(conn, conversation_id)?;
        conversation.pinned = pinned;
        crate::store::save(conn, &conversation)?;
    }
    Ok(wins)
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Whether this conversation has *any* recorded op history yet — from a
/// local seed, a local rewrite's reseed, or simply having received one or
/// more remote ops (`apply_remote_message`/`_batch` never call `seed`/
/// `reseed_after_rewrite` themselves, so "seeded" has to mean "any op
/// exists," not a separate flag those functions would otherwise have to
/// remember to set — an earlier version of this module used a distinct
/// `sync_conversation_seeded` table for this and a device that only ever
/// *received* ops for a conversation never marked it, so its first later
/// local edit would incorrectly re-seed from scratch instead of treating
/// existing content as history to diff against. Caught by this module's
/// own `offline_concurrent_appends_from_two_devices_converge_identically`
/// test.
fn is_seeded(conn: &Connection, conversation_id: &str) -> Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sync_message_ops WHERE conversation_id = ?1",
        params![conversation_id],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

fn seed(
    conn: &Connection,
    conversation_id: &str,
    messages: &[ChatMessage],
    device_id: &str,
) -> Result<()> {
    for (i, message) in messages.iter().enumerate() {
        let hlc = Hlc::synthetic(i as u32, device_id);
        insert_op(conn, conversation_id, &hlc, message)?;
    }
    Ok(())
}

/// Tombstones every currently-untombstoned op for `conversation_id`, then
/// inserts one fresh op per message in `messages`, each `Hlc` sharing
/// `base.physical_ms`/`base.device_id` with a per-index counter — same
/// intra-batch ordering shape as `Hlc::synthetic`'s index-based counter,
/// just anchored to a real rewrite event's timestamp instead of `0`. This
/// re-establishes a clean history so pure appends after the rewrite (from
/// any device) merge normally again via the ordinary [`is_pure_append`]
/// check — [`is_seeded`] sees the fresh (untombstoned) rows this leaves
/// behind, no separate bookkeeping needed.
/// Tombstones every currently-untombstoned op for `conversation_id`, with no
/// reseed after — the deletion counterpart to [`reseed_after_rewrite`]'s
/// same first step. Used by [`apply_remote_conversation_deleted`] via
/// `crate::sync::capture::apply_remote_conversation_deleted` (this module
/// doesn't itself expose a delete-envelope apply function — see this
/// module's top doc comment on why conversations' scalar-ish "does this
/// conversation still exist at all" question lives in `capture.rs` next to
/// the other tables' delete-envelope handling, not here).
pub fn tombstone_all(conn: &Connection, conversation_id: &str) -> Result<()> {
    conn.execute(
        "UPDATE sync_message_ops SET tombstoned = 1 WHERE conversation_id = ?1 AND tombstoned = 0",
        params![conversation_id],
    )?;
    Ok(())
}

fn reseed_after_rewrite(
    conn: &Connection,
    conversation_id: &str,
    messages: &[ChatMessage],
    base: &Hlc,
) -> Result<()> {
    tombstone_all(conn, conversation_id)?;
    for (i, message) in messages.iter().enumerate() {
        let hlc = Hlc {
            physical_ms: base.physical_ms,
            counter: i as u32,
            device_id: base.device_id.clone(),
        };
        insert_op(conn, conversation_id, &hlc, message)?;
    }
    Ok(())
}

/// Inserts one op. Returns `true` if it was new, `false` if this exact
/// `op_id` (hlc + device_id) was already recorded (idempotent redelivery).
fn insert_op(
    conn: &Connection,
    conversation_id: &str,
    hlc: &Hlc,
    message: &ChatMessage,
) -> Result<bool> {
    let op_id = hlc.op_id();
    let message_json = serde_json::to_string(message)?;
    let changed = conn.execute(
        "INSERT OR IGNORE INTO sync_message_ops \
            (op_id, conversation_id, hlc_ms, hlc_counter, hlc_device_id, message_json, tombstoned) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0)",
        params![
            op_id,
            conversation_id,
            hlc.physical_ms,
            hlc.counter,
            hlc.device_id,
            message_json
        ],
    )?;
    Ok(changed > 0)
}

fn load_ordered_messages(conn: &Connection, conversation_id: &str) -> Result<Vec<ChatMessage>> {
    let mut stmt = conn.prepare(
        "SELECT message_json FROM sync_message_ops \
         WHERE conversation_id = ?1 AND tombstoned = 0 \
         ORDER BY hlc_ms ASC, hlc_counter ASC, hlc_device_id ASC",
    )?;
    let rows = stmt.query_map(params![conversation_id], |row| row.get::<_, String>(0))?;
    let mut out = Vec::new();
    for row in rows {
        let json = row?;
        out.push(serde_json::from_str(&json)?);
    }
    Ok(out)
}

fn recompute_and_save(conn: &Connection, conversation_id: &str) -> Result<()> {
    let mut conversation = crate::store::get(conn, conversation_id)?;
    conversation.messages = load_ordered_messages(conn, conversation_id)?;
    crate::store::save(conn, &conversation)
}

/// A pure append iff `new` starts with exactly `current` (every existing
/// message unchanged, in the same order, with only new messages appended
/// after them). Anything else — a shorter array, a changed message, a
/// reordered one — is a rewrite, handled by the caller's rewrite branch.
fn is_pure_append(current: &[ChatMessage], new: &[ChatMessage]) -> bool {
    new.len() >= current.len() && messages_equal(current, &new[..current.len()])
}

/// `ChatMessage` doesn't implement `PartialEq` (and shouldn't be made to
/// just for this — it would require every field type, transitively, to
/// implement it too, for a struct used throughout the codebase). Comparing
/// via serialized JSON is exactly as precise for this module's purpose
/// (detecting "did the content actually change") and keeps that
/// requirement fully contained here.
fn messages_equal(a: &[ChatMessage], b: &[ChatMessage]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b.iter()).all(|(x, y)| {
        match (serde_json::to_string(x), serde_json::to_string(y)) {
            (Ok(x), Ok(y)) => x == y,
            _ => false,
        }
    })
}

fn write_conflict_file(
    conflicts_dir: &Path,
    conversation_id: &str,
    hlc: &Hlc,
    conversation: &Conversation,
) -> Result<()> {
    std::fs::create_dir_all(conflicts_dir).map_err(Error::Io)?;
    let path = conflicts_dir.join(format!(
        "conversation_snapshot-{conversation_id}-{}.json",
        hlc.op_id()
    ));
    let json = serde_json::to_string_pretty(conversation)?;
    std::fs::write(path, json).map_err(Error::Io)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{BranchPoint, ForkOrigin, SamplingParams, TaskType};

    fn test_conn() -> Connection {
        let dir = tempfile::tempdir().unwrap().keep();
        crate::app_db::open(&dir.join("app.sqlite"), None)
            .unwrap()
            .0
    }

    fn conversation(id: &str, messages: Vec<ChatMessage>) -> Conversation {
        Conversation {
            id: id.to_string(),
            title: "Untitled".to_string(),
            created_at_ms: 1,
            updated_at_ms: 1,
            messages,
            sampling: SamplingParams::default(),
            context_length: 8192,
            task_type: TaskType::General,
            project_id: None,
            pinned: false,
            branches: std::collections::HashMap::new(),
            forked_from: None,
        }
    }

    /// Deterministic id for a `msg(role, content)` call — same `content` in,
    /// same id out, so two calls that describe "the same message" (e.g. a
    /// baseline turn built once for a setup step and again for an
    /// unchanged-resave assertion) produce byte-identical `ChatMessage`s,
    /// the way a correctly-backfilled real id round-trips. Tests that need
    /// to reference a branch anchor key compute it via this function rather
    /// than a hardcoded string.
    fn msg_id(content: &str) -> String {
        format!("id-{content}")
    }

    fn msg(role: &str, content: &str) -> ChatMessage {
        ChatMessage {
            id: msg_id(content),
            role: role.to_string(),
            content: content.to_string(),
            created_at_ms: None,
            model: None,
            citations: None,
            thinking: None,
            steps: None,
            attachments: None,
            artifacts: None,
            stats: None,
            display_content: None,
            injection_notices: None,
            withheld_from_model: None,
        }
    }

    // -----------------------------------------------------------------
    // Basic seed + pure-append
    // -----------------------------------------------------------------

    #[test]
    fn fresh_conversation_gets_seeded_then_pure_appends_merge() {
        let mut conn = test_conn();
        let clock = HlcClock::new("device-a".to_string());

        let conv1 = conversation("conv-1", vec![msg("user", "hello")]);
        let applied = apply_local(&mut conn, &conv1, &clock).unwrap();
        assert_eq!(
            applied.message_ops.len(),
            1,
            "the single pre-existing message must be seeded as one op"
        );

        let conv2 = conversation(
            "conv-1",
            vec![msg("user", "hello"), msg("assistant", "hi there")],
        );
        let applied = apply_local(&mut conn, &conv2, &clock).unwrap();
        assert_eq!(
            applied.message_ops.len(),
            1,
            "a pure append must mint exactly one new op for the one new message"
        );

        let saved = crate::store::get(&conn, "conv-1").unwrap();
        assert_eq!(saved.messages.len(), 2);
        assert_eq!(saved.messages[0].content, "hello");
        assert_eq!(saved.messages[1].content, "hi there");
    }

    // -----------------------------------------------------------------
    // Fix for review finding: a freshly-paired device must receive
    // pre-existing history through the same op mechanism, not a separate
    // (missing) transport.
    // -----------------------------------------------------------------

    #[test]
    fn a_freshly_paired_device_receives_preexisting_history_via_seed_ops() {
        let mut conn_a = test_conn();
        let clock_a = HlcClock::new("device-a".to_string());

        // Device A already has 3 messages before it ever enables sync.
        let existing = conversation(
            "conv-1",
            vec![
                msg("user", "one"),
                msg("assistant", "two"),
                msg("user", "three"),
            ],
        );
        let applied = apply_local(&mut conn_a, &existing, &clock_a).unwrap();
        assert_eq!(
            applied.message_ops.len(),
            3,
            "all 3 pre-existing messages must be seeded and returned for broadcast"
        );

        // Device B is fresh — it never had this conversation at all — and
        // receives A's seed ops exactly as it would receive ordinary
        // AppendMessage envelopes.
        let mut conn_b = test_conn();
        // B must have a row to attach messages to; simulate that the
        // conversation metadata (title/project) arrived some other way by
        // pre-creating an empty conversation, mirroring how the real sync
        // engine would create the row before applying its first message op.
        crate::store::save(&conn_b, &conversation("conv-1", vec![])).unwrap();

        for op in applied.message_ops {
            apply_remote_message(&mut conn_b, "conv-1", op.hlc, op.message).unwrap();
        }

        let b_view = crate::store::get(&conn_b, "conv-1").unwrap();
        assert_eq!(
            b_view.messages.len(),
            3,
            "device B must end up with all 3 of A's pre-existing messages"
        );
        assert_eq!(b_view.messages[0].content, "one");
        assert_eq!(b_view.messages[1].content, "two");
        assert_eq!(b_view.messages[2].content, "three");
    }

    // -----------------------------------------------------------------
    // Fix for review finding: offline-concurrent appends from multiple
    // devices must converge to the SAME final order on every device,
    // regardless of delivery order, with nothing dropped.
    // -----------------------------------------------------------------

    #[test]
    fn offline_concurrent_appends_from_two_devices_converge_identically() {
        let mut conn_a = test_conn();
        let clock_a = HlcClock::new("device-a".to_string());
        let mut conn_b = test_conn();
        let clock_b = HlcClock::new("device-b".to_string());

        // Both devices start from the same shared history (as if already
        // synced up to this point).
        let shared = conversation("conv-1", vec![msg("user", "shared-1")]);
        let seed_a = apply_local(&mut conn_a, &shared, &clock_a).unwrap();
        crate::store::save(&conn_b, &shared).unwrap();
        for op in &seed_a.message_ops {
            apply_remote_message(&mut conn_b, "conv-1", op.hlc.clone(), op.message.clone())
                .unwrap();
        }

        // Now offline: A appends 2 messages, B appends 1 — neither sees the
        // other's yet.
        let a_extended = conversation(
            "conv-1",
            vec![
                msg("user", "shared-1"),
                msg("assistant", "a-msg-1"),
                msg("user", "a-msg-2"),
            ],
        );
        let a_ops = apply_local(&mut conn_a, &a_extended, &clock_a)
            .unwrap()
            .message_ops;
        assert_eq!(a_ops.len(), 2);

        let b_extended = conversation(
            "conv-1",
            vec![msg("user", "shared-1"), msg("assistant", "b-msg-1")],
        );
        let b_ops = apply_local(&mut conn_b, &b_extended, &clock_b)
            .unwrap()
            .message_ops;
        assert_eq!(b_ops.len(), 1);

        // Both come back online and pull each other's ops, in DIFFERENT
        // orders on each side, to prove delivery order doesn't matter.
        for op in b_ops.iter().rev() {
            apply_remote_message(&mut conn_a, "conv-1", op.hlc.clone(), op.message.clone())
                .unwrap();
        }
        for op in a_ops.iter() {
            apply_remote_message(&mut conn_b, "conv-1", op.hlc.clone(), op.message.clone())
                .unwrap();
        }

        let a_final = crate::store::get(&conn_a, "conv-1").unwrap().messages;
        let b_final = crate::store::get(&conn_b, "conv-1").unwrap().messages;
        assert!(
            messages_equal(&a_final, &b_final),
            "both devices must converge to an identical final order"
        );
        assert_eq!(
            a_final.len(),
            4,
            "shared-1 + 2 from A + 1 from B — nothing dropped, nothing duplicated"
        );
    }

    // -----------------------------------------------------------------
    // Idempotency: a redelivered op (relay at-least-once delivery) must
    // never duplicate a message.
    // -----------------------------------------------------------------

    #[test]
    fn redelivered_op_is_idempotent() {
        let mut conn = test_conn();
        crate::store::save(&conn, &conversation("conv-1", vec![])).unwrap();
        let hlc = Hlc {
            physical_ms: 1000,
            counter: 0,
            device_id: "device-a".to_string(),
        };
        let message = msg("user", "hello");

        assert!(apply_remote_message(&mut conn, "conv-1", hlc.clone(), message.clone()).unwrap());
        assert!(
            !apply_remote_message(&mut conn, "conv-1", hlc, message).unwrap(),
            "redelivering the identical op must be a no-op"
        );

        let saved = crate::store::get(&conn, "conv-1").unwrap();
        assert_eq!(
            saved.messages.len(),
            1,
            "the message must not be duplicated"
        );
    }

    // -----------------------------------------------------------------
    // Fix for review finding: title and project_id are independent LWW
    // registers — editing one on one device and the other on a different
    // device concurrently must preserve BOTH edits, not clobber one.
    // -----------------------------------------------------------------

    #[test]
    fn title_and_project_edits_from_different_devices_dont_clobber_each_other() {
        let mut conn_a = test_conn();
        let clock_a = HlcClock::new("device-a".to_string());
        let mut conn_b = test_conn();
        let clock_b = HlcClock::new("device-b".to_string());

        let shared = conversation("conv-1", vec![msg("user", "hi")]);
        let seed = apply_local(&mut conn_a, &shared, &clock_a).unwrap();
        crate::store::save(&conn_b, &shared).unwrap();
        for op in &seed.message_ops {
            apply_remote_message(&mut conn_b, "conv-1", op.hlc.clone(), op.message.clone())
                .unwrap();
        }

        // Device A renames the conversation (title only).
        let mut a_renamed = crate::store::get(&conn_a, "conv-1").unwrap();
        a_renamed.title = "Trip planning".to_string();
        let a_applied = apply_local(&mut conn_a, &a_renamed, &clock_a).unwrap();
        let (title_hlc, new_title) = a_applied
            .title_update
            .expect("title changed, must be reported");

        // Device B (independently, before seeing A's rename) moves it to a
        // project (project_id only).
        let mut b_moved = crate::store::get(&conn_b, "conv-1").unwrap();
        b_moved.project_id = Some("proj-42".to_string());
        let b_applied = apply_local(&mut conn_b, &b_moved, &clock_b).unwrap();
        let (project_hlc, new_project) = b_applied
            .project_update
            .expect("project_id changed, must be reported");

        // Cross-apply: A learns about B's project move, B learns about A's
        // rename.
        assert!(apply_remote_project(&conn_a, "conv-1", project_hlc, new_project).unwrap());
        assert!(apply_remote_title(&conn_b, "conv-1", title_hlc, new_title).unwrap());

        let a_final = crate::store::get(&conn_a, "conv-1").unwrap();
        let b_final = crate::store::get(&conn_b, "conv-1").unwrap();
        assert_eq!(
            a_final.title, "Trip planning",
            "A's own rename must survive on A"
        );
        assert_eq!(
            a_final.project_id,
            Some("proj-42".to_string()),
            "B's project move must have applied on A"
        );
        assert_eq!(
            b_final.title, "Trip planning",
            "A's rename must have applied on B"
        );
        assert_eq!(
            b_final.project_id,
            Some("proj-42".to_string()),
            "B's own project move must survive on B"
        );
    }

    // -----------------------------------------------------------------
    // Rewrite/truncation (edit-and-regenerate) — messages aren't actually
    // append-only (see this module's top doc comment) — and appends after
    // a rewrite must still merge normally (reseed correctness).
    // -----------------------------------------------------------------

    #[test]
    fn edit_and_regenerate_triggers_a_snapshot_and_future_appends_still_merge() {
        let mut conn = test_conn();
        let clock = HlcClock::new("device-a".to_string());

        let original = conversation(
            "conv-1",
            vec![
                msg("user", "what is 2+2"),
                msg("assistant", "5"),
                msg("user", "are you sure"),
            ],
        );
        apply_local(&mut conn, &original, &clock).unwrap();

        // Edit-and-regenerate: drop the last 2 messages (the wrong answer +
        // follow-up) and replace with a corrected tail — same shape as the
        // frontend's `editMessage` truncation.
        let corrected = conversation(
            "conv-1",
            vec![msg("user", "what is 2+2"), msg("assistant", "4")],
        );
        let applied = apply_local(&mut conn, &corrected, &clock).unwrap();
        assert!(
            applied.snapshot.is_some(),
            "a truncation must be detected as a rewrite, not a pure append"
        );
        assert!(
            applied.message_ops.is_empty(),
            "a rewrite broadcasts a snapshot, not individual message ops"
        );

        let after_rewrite = crate::store::get(&conn, "conv-1").unwrap();
        assert_eq!(after_rewrite.messages.len(), 2);
        assert_eq!(after_rewrite.messages[1].content, "4");

        // A further pure append after the rewrite must still work — proves
        // the reseed left the op log in a mergeable state, not stuck.
        let extended = conversation(
            "conv-1",
            vec![
                msg("user", "what is 2+2"),
                msg("assistant", "4"),
                msg("user", "thanks"),
            ],
        );
        let applied2 = apply_local(&mut conn, &extended, &clock).unwrap();
        assert_eq!(
            applied2.message_ops.len(),
            1,
            "must be detected as a pure append after the rewrite, not another rewrite"
        );

        let final_state = crate::store::get(&conn, "conv-1").unwrap();
        assert_eq!(final_state.messages.len(), 3);
        assert_eq!(final_state.messages[2].content, "thanks");
    }

    #[test]
    fn a_losing_snapshot_is_written_to_a_conflict_file_not_applied() {
        let mut conn = test_conn();
        let clock = HlcClock::new("device-a".to_string());
        let conflicts_dir = tempfile::tempdir().unwrap();

        let original = conversation("conv-1", vec![msg("user", "hi")]);
        apply_local(&mut conn, &original, &clock).unwrap();
        // Record a NEWER local snapshot version directly, simulating this
        // device having already applied a later rewrite than the incoming
        // one below.
        let newer_local = clock.next();
        field_versions::record_local(&conn, "conversation_snapshot", "conv-1", &clock).unwrap();
        let _ = newer_local;

        let stale_incoming = Hlc {
            physical_ms: 1,
            counter: 0,
            device_id: "device-b".to_string(),
        };
        let incoming_conversation =
            conversation("conv-1", vec![msg("user", "a stale rewrite from B")]);
        let applied = apply_remote_snapshot(
            &mut conn,
            conflicts_dir.path(),
            stale_incoming.clone(),
            incoming_conversation,
        )
        .unwrap();
        assert!(
            !applied,
            "an older snapshot must lose against the newer local one"
        );

        let local_state = crate::store::get(&conn, "conv-1").unwrap();
        assert_eq!(
            local_state.messages[0].content, "hi",
            "the local state must be untouched by a losing snapshot"
        );

        let expected_path = conflicts_dir.path().join(format!(
            "conversation_snapshot-conv-1-{}.json",
            stale_incoming.op_id()
        ));
        assert!(expected_path.exists(), "the losing snapshot's content must be preserved in a conflict file, never silently dropped");
    }

    // -----------------------------------------------------------------
    // `Conversation.branches` (edit/retry history) — proves this module
    // needed zero changes to carry branch data through sync: it rides
    // inside the same whole-`Conversation` clone the rewrite path already
    // broadcasts as a `ConversationSnapshot`.
    // -----------------------------------------------------------------

    #[test]
    fn a_branch_entry_survives_a_rewrite_and_a_remote_snapshot_round_trip() {
        let mut conn = test_conn();
        let clock = HlcClock::new("device-a".to_string());

        let original = conversation(
            "conv-1",
            vec![msg("user", "what is 2+2"), msg("assistant", "5")],
        );
        apply_local(&mut conn, &original, &clock).unwrap();

        // Edit-and-regenerate, same shape as
        // `edit_and_regenerate_triggers_a_snapshot_and_future_appends_still_merge`,
        // but this time also recording the discarded reply as a branch
        // variant — exactly what `ui/src/state/store.ts`'s `editMessage`
        // does before truncating.
        let anchor_key = msg_id("what is 2+2");
        let mut corrected = conversation(
            "conv-1",
            vec![msg("user", "what is 2+2"), msg("assistant", "4")],
        );
        corrected.branches.insert(
            anchor_key.clone(),
            BranchPoint {
                variants: vec![vec![msg("assistant", "5")], vec![msg("assistant", "4")]],
                active: 1,
            },
        );

        let applied = apply_local(&mut conn, &corrected, &clock).unwrap();
        let (hlc, snapshot) = applied
            .snapshot
            .expect("a branch-recording edit must still classify as a rewrite");
        let branch = snapshot
            .branches
            .get(&anchor_key)
            .expect("the branch entry must be part of the broadcast snapshot");
        assert_eq!(branch.variants.len(), 2);
        assert_eq!(branch.active, 1);

        // Apply that snapshot on a second device's connection — proves the
        // whole-struct broadcast/apply path needs no branch-specific code.
        let mut conn_b = test_conn();
        let conflicts_dir = tempfile::tempdir().unwrap();
        let applied_remote =
            apply_remote_snapshot(&mut conn_b, conflicts_dir.path(), hlc, snapshot).unwrap();
        assert!(applied_remote, "a first-ever snapshot for this id must win");

        let b_view = crate::store::get(&conn_b, "conv-1").unwrap();
        let b_branch = b_view
            .branches
            .get(&anchor_key)
            .expect("branch data must have traveled with the snapshot to device B");
        assert_eq!(b_branch.variants.len(), 2);
        assert_eq!(b_branch.active, 1);
        assert_eq!(b_branch.variants[0][0].content, "5");
        assert_eq!(b_branch.variants[1][0].content, "4");
    }

    // -----------------------------------------------------------------
    // `Conversation.forked_from` (the "forked from another conversation"
    // hint) — unlike `branches`, this is meaningful from the very first
    // save of a forked conversation, but that first save always classifies
    // as a *seed* (never-before-seen conversation id), which only
    // broadcasts per-message ops, not the whole struct (see this field's
    // doc comment in `types.rs`). This test starts from an already-seeded
    // conversation instead, to prove the part that DOES work today: once
    // seeded, `forked_from` rides along on a rewrite the same way
    // `branches` does, with zero sync-engine changes needed for it either.
    // -----------------------------------------------------------------

    #[test]
    fn forked_from_survives_a_rewrite_and_a_remote_snapshot_round_trip_once_seeded() {
        let mut conn = test_conn();
        let clock = HlcClock::new("device-a".to_string());
        let origin = ForkOrigin {
            conversation_id: "conv-1".to_string(),
            message_id: msg_id("hello"),
        };

        let mut seeded = conversation("conv-2", vec![msg("user", "hi"), msg("assistant", "hello")]);
        seeded.forked_from = Some(origin.clone());
        apply_local(&mut conn, &seeded, &clock).unwrap();

        let mut edited = conversation(
            "conv-2",
            vec![msg("user", "hi"), msg("assistant", "hello there")],
        );
        edited.forked_from = Some(origin.clone());

        let applied = apply_local(&mut conn, &edited, &clock).unwrap();
        let (hlc, snapshot) = applied
            .snapshot
            .expect("an edit must classify as a rewrite");
        let snapshot_origin = snapshot
            .forked_from
            .clone()
            .expect("forked_from must be part of the broadcast snapshot");
        assert_eq!(snapshot_origin.conversation_id, "conv-1");

        let mut conn_b = test_conn();
        let conflicts_dir = tempfile::tempdir().unwrap();
        let applied_remote =
            apply_remote_snapshot(&mut conn_b, conflicts_dir.path(), hlc, snapshot).unwrap();
        assert!(applied_remote, "a first-ever snapshot for this id must win");

        let b_view = crate::store::get(&conn_b, "conv-2").unwrap();
        let b_origin = b_view
            .forked_from
            .expect("forked_from must have traveled with the snapshot to device B");
        assert_eq!(b_origin.conversation_id, "conv-1");
        assert_eq!(b_origin.message_id, msg_id("hello"));
    }

    #[test]
    fn reapplying_an_unchanged_conversation_with_stable_ids_mints_nothing() {
        let mut conn = test_conn();
        let clock = HlcClock::new("device-a".to_string());

        let original = conversation(
            "conv-1",
            vec![msg("user", "hello"), msg("assistant", "hi there")],
        );
        apply_local(&mut conn, &original, &clock).unwrap();

        // A plain reload+resave with the SAME stable ids (what a correctly
        // round-tripped/backfilled conversation looks like) must be a true
        // no-op. If `ChatMessage.id` were re-minted randomly on every read
        // — the exact hazard `crate::store::backfill_message_ids` exists to
        // prevent — this would instead diff as "every message changed" and
        // trigger a spurious rewrite/reseed for a conversation nobody
        // touched.
        let unchanged = conversation(
            "conv-1",
            vec![msg("user", "hello"), msg("assistant", "hi there")],
        );
        let applied = apply_local(&mut conn, &unchanged, &clock).unwrap();
        assert!(applied.message_ops.is_empty(), "no messages were appended");
        assert!(
            applied.snapshot.is_none(),
            "identical content and ids must not be classified as a rewrite"
        );
    }

    #[test]
    fn a_preexisting_synced_conversation_with_id_less_ops_does_not_spuriously_rewrite_after_backfill(
    ) {
        // Reproduces exactly what a device that enabled sync before
        // `ChatMessage.id` existed has on disk: both `conversations.data`
        // AND `sync_message_ops.message_json` (the table this module's own
        // rewrite classifier reads back through `load_ordered_messages`)
        // are id-less. `crate::store::backfill_message_ids` must patch
        // BOTH — see that function's doc comment — or `apply_local`
        // misclassifies every later save of this conversation as a
        // rewrite: a fresh, different random id gets minted on every
        // deserialize of a still-unbackfilled `sync_message_ops` row,
        // which never equals the stable id backfilled into
        // `conversations.data`.
        let mut conn = test_conn();
        let device_id = "device-a";

        let raw_messages = serde_json::json!([
            {"role": "user", "content": "hello"},
            {"role": "assistant", "content": "hi there"},
        ]);
        conn.execute(
            "INSERT INTO conversations \
                (id, title, created_at_ms, updated_at_ms, message_count, task_type, \
                 project_id, pinned, data) \
             VALUES ('conv-1', 'Untitled', 1, 1, 2, '\"general\"', NULL, 0, ?1)",
            params![serde_json::json!({
                "id": "conv-1",
                "title": "Untitled",
                "createdAtMs": 1,
                "updatedAtMs": 1,
                "messages": raw_messages,
                "contextLength": 8192,
                "taskType": "general",
                "pinned": false,
            })
            .to_string()],
        )
        .unwrap();
        for (i, message) in raw_messages.as_array().unwrap().iter().enumerate() {
            let hlc = Hlc::synthetic(i as u32, device_id);
            conn.execute(
                "INSERT INTO sync_message_ops \
                    (op_id, conversation_id, hlc_ms, hlc_counter, hlc_device_id, \
                     message_json, tombstoned) \
                 VALUES (?1, 'conv-1', ?2, ?3, ?4, ?5, 0)",
                params![
                    hlc.op_id(),
                    hlc.physical_ms,
                    hlc.counter,
                    hlc.device_id,
                    message.to_string()
                ],
            )
            .unwrap();
        }

        crate::store::backfill_message_ids(&mut conn).unwrap();

        let backfilled = crate::store::get(&conn, "conv-1").unwrap();
        assert!(backfilled.messages.iter().all(|m| !m.id.is_empty()));

        let clock = HlcClock::new(device_id.to_string());
        let applied = apply_local(&mut conn, &backfilled, &clock).unwrap();
        assert!(
            applied.message_ops.is_empty(),
            "no new messages were appended"
        );
        assert!(
            applied.snapshot.is_none(),
            "a plain resave of a freshly-backfilled conversation must not be \
             misclassified as a rewrite — this is the actual regression the \
             sync_message_ops half of the backfill exists to prevent"
        );

        // Stable on a further read too, not just "happened to match once".
        let reread = crate::store::get(&conn, "conv-1").unwrap();
        let ids_a: Vec<&str> = backfilled.messages.iter().map(|m| m.id.as_str()).collect();
        let ids_b: Vec<&str> = reread.messages.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids_a, ids_b);
    }
}
