//! Wave C — end-to-end proof that Phase 3's 4 originally-reviewed scenarios
//! (see `docs/design/MULTI_DEVICE_SYNC.md`'s design-review section and the
//! Phase 3 plan) work once the real pieces are wired together: local write
//! -> `sync::capture::*_synced` (mint HLC, apply, encrypt, enqueue) ->
//! `sync::envelope` (the actual AEAD wire format) -> `sync::outbox` (the
//! actual queue) -> decrypt -> `sync::conversations`/`sync::capture`'s
//! `apply_remote_*` (the actual merge/LWW logic).
//!
//! `sync::conversations`' own unit tests already prove these 4 scenarios
//! correct against `apply_local`/`apply_remote_*` called directly with
//! in-memory Rust values — this file proves the SAME scenarios still hold
//! once every local write actually goes through `capture::*_synced` (HLC
//! minting via a real `HlcClock`, not hand-constructed `Hlc` values) and
//! every remote apply goes through a real encrypt -> outbox -> decrypt
//! round trip (JSON wire serialization + AES-256-GCM), not a bypassed
//! in-memory shortcut. That's the actual, literal thing "combining Wave A
//! and Wave B" means — either layer alone already had its own tests; this
//! is what proves they compose correctly.
//!
//! Deliberately does NOT exercise `crates/app`'s `sync_loop.rs`/
//! `commands.rs` (a `crates/core` integration test can't depend on
//! `crates/app` — wrong dependency direction) — `deliver` below is a
//! from-scratch, in-process stand-in for "the relay round trip", built
//! directly against `sync::outbox`/`sync::envelope`, the same two modules
//! `sync_loop.rs` itself calls. The one piece of `sync_loop.rs`-only logic
//! this file has to duplicate locally is `ensure_conversation_placeholder`
//! (`apply_remote_message`/`_title`/`_project` require the conversation row
//! to already exist) — see that function below for why.

use openpal_core::crypto::sync_key::SyncKey;
use openpal_core::sync::hlc::{Hlc, HlcClock};
use openpal_core::sync::{
    artifact_sweep, capture, conversations, envelope, field_versions, outbox,
};
use openpal_core::types::{ChatMessage, Conversation, Project, SamplingParams, TaskType};
use rusqlite::Connection;

// ---------------------------------------------------------------------------
// Test fixtures
// ---------------------------------------------------------------------------

fn device(device_id: &str) -> (Connection, HlcClock) {
    let dir = tempfile::tempdir().unwrap().keep();
    let (conn, _, _) = openpal_core::app_db::open(&dir.join("app.sqlite"), None).unwrap();
    (conn, HlcClock::new(device_id.to_string()))
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

fn msg(role: &str, content: &str) -> ChatMessage {
    ChatMessage {
        id: format!("id-{content}"),
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
    }
}

/// `apply_remote_message`/`_title`/`_project` all call `store::get`
/// internally and error if the conversation doesn't exist yet — true the
/// first time a device sees an envelope for a conversation it's never had
/// any row for. Mirrors `crates/app/src/sync_loop.rs::ensure_conversation_
/// placeholder` exactly (duplicated here rather than shared, since a
/// `crates/core` integration test can't reach into `crates/app`).
fn ensure_conversation_placeholder(conn: &Connection, conversation_id: &str) {
    if openpal_core::store::get(conn, conversation_id).is_ok() {
        return;
    }
    openpal_core::store::save(
        conn,
        &Conversation {
            id: conversation_id.to_string(),
            title: String::new(),
            created_at_ms: 0,
            updated_at_ms: 0,
            messages: Vec::new(),
            sampling: SamplingParams::default(),
            context_length: 8192,
            task_type: TaskType::default(),
            project_id: None,
            pinned: false,
            branches: std::collections::HashMap::new(),
            forked_from: None,
        },
    )
    .unwrap();
}

/// Mirrors `crates/app/src/sync_loop.rs::is_stale_against_deletion` exactly
/// (duplicated here for the same reason `ensure_conversation_placeholder`
/// is — a `crates/core` integration test can't reach into `crates/app`).
fn is_stale_against_deletion(conn: &Connection, conversation_id: &str, hlc: &Hlc) -> bool {
    match field_versions::get(conn, "conversation_deleted", conversation_id) {
        Some(deleted_at) => *hlc <= deleted_at,
        None => false,
    }
}

/// Drains `from`'s outbox, decrypts every envelope under the shared
/// `sync_key`, and routes each into `to` via the exact same
/// `conversations`/`capture` apply functions `crates/app/src/sync_loop.rs`
/// calls — a from-scratch stand-in for "the relay delivered these", built
/// against the real `outbox`/`envelope` wire format rather than passing
/// Rust values directly between the two devices. Batches `AppendMessage`
/// envelopes through `apply_remote_messages_batch`, same as the real loop.
fn deliver(from: &Connection, to: &mut Connection, sync_key: &SyncKey) {
    let items = outbox::drain(from, 1000).unwrap();
    let mut message_ops = Vec::new();
    for item in &items {
        match envelope::decrypt(sync_key, &item.ciphertext).unwrap() {
            envelope::ChangeEnvelope::AppendMessage {
                conversation_id,
                hlc,
                message,
            } => {
                if is_stale_against_deletion(to, &conversation_id, &hlc) {
                    continue;
                }
                ensure_conversation_placeholder(to, &conversation_id);
                message_ops.push((conversation_id, hlc, message));
            }
            envelope::ChangeEnvelope::ConversationSnapshot { hlc, conversation } => {
                let conflicts = tempfile::tempdir().unwrap();
                conversations::apply_remote_snapshot(to, conflicts.path(), hlc, conversation)
                    .unwrap();
            }
            envelope::ChangeEnvelope::ConversationTitle {
                conversation_id,
                hlc,
                title,
            } => {
                if is_stale_against_deletion(to, &conversation_id, &hlc) {
                    continue;
                }
                ensure_conversation_placeholder(to, &conversation_id);
                conversations::apply_remote_title(to, &conversation_id, hlc, title).unwrap();
            }
            envelope::ChangeEnvelope::ConversationProject {
                conversation_id,
                hlc,
                project_id,
            } => {
                if is_stale_against_deletion(to, &conversation_id, &hlc) {
                    continue;
                }
                ensure_conversation_placeholder(to, &conversation_id);
                conversations::apply_remote_project(to, &conversation_id, hlc, project_id).unwrap();
            }
            envelope::ChangeEnvelope::ConversationDeleted {
                conversation_id,
                hlc,
            } => {
                let conflicts = tempfile::tempdir().unwrap();
                capture::apply_remote_conversation_deleted(
                    to,
                    hlc,
                    &conversation_id,
                    conflicts.path(),
                )
                .unwrap();
            }
            envelope::ChangeEnvelope::ProjectRow { hlc, project } => {
                let conflicts = tempfile::tempdir().unwrap();
                capture::apply_remote_project_row(to, hlc, project, conflicts.path()).unwrap();
            }
            envelope::ChangeEnvelope::ProjectDeleted { row_key, hlc } => {
                let conflicts = tempfile::tempdir().unwrap();
                capture::apply_remote_project_deleted(to, hlc, &row_key, conflicts.path()).unwrap();
            }
            envelope::ChangeEnvelope::ArtifactRow {
                hlc,
                conversation_id,
                artifact_id,
                filename,
                content,
            } => {
                let conflicts = tempfile::tempdir().unwrap();
                capture::apply_remote_artifact_row(
                    to,
                    hlc,
                    &conversation_id,
                    &artifact_id,
                    &filename,
                    &content,
                    conflicts.path(),
                )
                .unwrap();
            }
            envelope::ChangeEnvelope::SettingsRow { .. }
            | envelope::ChangeEnvelope::CustomModelsRow { .. } => {
                // Not exercised by this file's scenarios — the individual
                // `capture::*_synced`/`apply_remote_*` pairs for these are
                // already covered by `capture.rs`'s own unit tests.
            }
        }
    }
    if !message_ops.is_empty() {
        conversations::apply_remote_messages_batch(to, message_ops).unwrap();
    }
    for item in &items {
        outbox::ack(from, &item.envelope_id).unwrap();
    }
}

// ---------------------------------------------------------------------------
// Scenario 1: two devices both used offline between sync sessions must
// converge to the identical merged history, nothing dropped — the
// feature's primary use case per the plan ("chat on phone during a
// commute, then chat on desktop that evening").
// ---------------------------------------------------------------------------

#[test]
fn offline_concurrent_appends_converge_over_the_real_encrypted_pipe() {
    let (mut conn_a, clock_a) = device("device-a");
    let (mut conn_b, clock_b) = device("device-b");
    let sync_key = SyncKey::random();

    // Both devices start from the same synced history.
    let shared = conversation("conv-1", vec![msg("user", "shared-1")]);
    capture::save_conversation_synced(&mut conn_a, &shared, &clock_a, &sync_key, "mailbox")
        .unwrap();
    deliver(&conn_a, &mut conn_b, &sync_key);

    // Offline: A appends 2, B appends 1 — neither has seen the other's yet.
    let a_extended = conversation(
        "conv-1",
        vec![
            msg("user", "shared-1"),
            msg("assistant", "a-msg-1"),
            msg("user", "a-msg-2"),
        ],
    );
    capture::save_conversation_synced(&mut conn_a, &a_extended, &clock_a, &sync_key, "mailbox")
        .unwrap();

    let b_extended = conversation(
        "conv-1",
        vec![msg("user", "shared-1"), msg("assistant", "b-msg-1")],
    );
    capture::save_conversation_synced(&mut conn_b, &b_extended, &clock_b, &sync_key, "mailbox")
        .unwrap();

    // Both come back online — deliver in both directions.
    deliver(&conn_a, &mut conn_b, &sync_key);
    deliver(&conn_b, &mut conn_a, &sync_key);

    let a_final = openpal_core::store::get(&conn_a, "conv-1")
        .unwrap()
        .messages;
    let b_final = openpal_core::store::get(&conn_b, "conv-1")
        .unwrap()
        .messages;
    let a_contents: Vec<&str> = a_final.iter().map(|m| m.content.as_str()).collect();
    let b_contents: Vec<&str> = b_final.iter().map(|m| m.content.as_str()).collect();
    assert_eq!(
        a_contents, b_contents,
        "both devices must converge to an identical merged order"
    );
    assert_eq!(
        a_final.len(),
        4,
        "shared-1 + 2 from A + 1 from B — nothing dropped, nothing duplicated"
    );
}

// ---------------------------------------------------------------------------
// Scenario 2: title and project_id edited concurrently on different
// devices must both survive — not one clobbering the other via a shared
// version register.
// ---------------------------------------------------------------------------

#[test]
fn concurrent_title_and_project_edits_both_survive_over_the_real_encrypted_pipe() {
    let (mut conn_a, clock_a) = device("device-a");
    let (mut conn_b, clock_b) = device("device-b");
    let sync_key = SyncKey::random();

    let shared = conversation("conv-1", vec![msg("user", "hi")]);
    capture::save_conversation_synced(&mut conn_a, &shared, &clock_a, &sync_key, "mailbox")
        .unwrap();
    deliver(&conn_a, &mut conn_b, &sync_key);

    let project = Project {
        id: "proj-42".to_string(),
        name: "Research".to_string(),
        created_at_ms: 1,
        updated_at_ms: 1,
    };
    capture::save_project_synced(&mut conn_a, &project, &clock_a, &sync_key, "mailbox").unwrap();
    deliver(&conn_a, &mut conn_b, &sync_key);
    outbox::drain(&conn_a, 1000).unwrap(); // (nothing pending; drains to empty, keeps scenario tidy)

    // Device A renames (title only); Device B moves to the project (project_id only) —
    // neither has seen the other's edit yet.
    let mut a_renamed = openpal_core::store::get(&conn_a, "conv-1").unwrap();
    a_renamed.title = "Trip planning".to_string();
    capture::save_conversation_synced(&mut conn_a, &a_renamed, &clock_a, &sync_key, "mailbox")
        .unwrap();

    let mut b_moved = openpal_core::store::get(&conn_b, "conv-1").unwrap();
    b_moved.project_id = Some("proj-42".to_string());
    capture::save_conversation_synced(&mut conn_b, &b_moved, &clock_b, &sync_key, "mailbox")
        .unwrap();

    deliver(&conn_a, &mut conn_b, &sync_key);
    deliver(&conn_b, &mut conn_a, &sync_key);

    let a_final = openpal_core::store::get(&conn_a, "conv-1").unwrap();
    let b_final = openpal_core::store::get(&conn_b, "conv-1").unwrap();
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

// ---------------------------------------------------------------------------
// Scenario 3: edit-and-regenerate (a rewrite/truncation, not a pure
// append — messages aren't actually append-only) must propagate as a
// snapshot, and further appends after the rewrite must still merge.
// ---------------------------------------------------------------------------

#[test]
fn edit_and_regenerate_propagates_as_a_snapshot_and_future_appends_still_merge() {
    let (mut conn_a, clock_a) = device("device-a");
    let (mut conn_b, clock_b) = device("device-b");
    let sync_key = SyncKey::random();

    let original = conversation(
        "conv-1",
        vec![
            msg("user", "what is 2+2"),
            msg("assistant", "5"),
            msg("user", "are you sure"),
        ],
    );
    capture::save_conversation_synced(&mut conn_a, &original, &clock_a, &sync_key, "mailbox")
        .unwrap();
    deliver(&conn_a, &mut conn_b, &sync_key);
    assert_eq!(
        openpal_core::store::get(&conn_b, "conv-1")
            .unwrap()
            .messages
            .len(),
        3
    );

    // Edit-and-regenerate: drop the last 2 messages, replace with a
    // corrected tail — same shape as the frontend's `editMessage` truncation.
    let corrected = conversation(
        "conv-1",
        vec![msg("user", "what is 2+2"), msg("assistant", "4")],
    );
    capture::save_conversation_synced(&mut conn_a, &corrected, &clock_a, &sync_key, "mailbox")
        .unwrap();
    deliver(&conn_a, &mut conn_b, &sync_key);

    let b_after_rewrite = openpal_core::store::get(&conn_b, "conv-1").unwrap();
    assert_eq!(
        b_after_rewrite.messages.len(),
        2,
        "B must receive the rewrite, not keep the stale 3-message history"
    );
    assert_eq!(b_after_rewrite.messages[1].content, "4");

    // A further pure append after the rewrite must still merge normally —
    // proves the reseed on the receiving side left B's op log mergeable,
    // not stuck replaying pre-rewrite history.
    let extended = conversation(
        "conv-1",
        vec![
            msg("user", "what is 2+2"),
            msg("assistant", "4"),
            msg("user", "thanks"),
        ],
    );
    capture::save_conversation_synced(&mut conn_a, &extended, &clock_a, &sync_key, "mailbox")
        .unwrap();
    deliver(&conn_a, &mut conn_b, &sync_key);

    let b_final = openpal_core::store::get(&conn_b, "conv-1").unwrap();
    assert_eq!(b_final.messages.len(), 3);
    assert_eq!(b_final.messages[2].content, "thanks");
    let _ = clock_b; // device B never writes locally in this scenario
}

// ---------------------------------------------------------------------------
// Scenario 4: a freshly-paired device (zero local history for this
// conversation) must receive the other device's pre-existing history
// through the same op mechanism as an ordinary append — no separate
// "history transfer" transport.
// ---------------------------------------------------------------------------

#[test]
fn a_fresh_device_receives_preexisting_history_over_the_real_encrypted_pipe() {
    let (mut conn_a, clock_a) = device("device-a");
    let (mut conn_b, _clock_b) = device("device-b");
    let sync_key = SyncKey::random();

    // Device A already has a 3-message conversation before B ever pairs in.
    let existing = conversation(
        "conv-1",
        vec![
            msg("user", "one"),
            msg("assistant", "two"),
            msg("user", "three"),
        ],
    );
    capture::save_conversation_synced(&mut conn_a, &existing, &clock_a, &sync_key, "mailbox")
        .unwrap();

    // Device B has never seen this conversation at all — no placeholder
    // pre-created, unlike the other scenarios above (this IS the case
    // under test: `deliver`'s own `ensure_conversation_placeholder` call
    // must be what creates the row).
    assert!(
        openpal_core::store::get(&conn_b, "conv-1").is_err(),
        "sanity: B starts with no row for this conversation"
    );

    deliver(&conn_a, &mut conn_b, &sync_key);

    let b_view = openpal_core::store::get(&conn_b, "conv-1").unwrap();
    assert_eq!(
        b_view.messages.len(),
        3,
        "B must end up with all 3 of A's pre-existing messages"
    );
    assert_eq!(b_view.messages[0].content, "one");
    assert_eq!(b_view.messages[1].content, "two");
    assert_eq!(b_view.messages[2].content, "three");
}

// ---------------------------------------------------------------------------
// Scenario 5 (found by a post-Phase-3 compatibility review, not in the
// original 4): deleting a conversation must propagate as a tombstone, and a
// stale op that arrives after the deletion must not resurrect it — closing
// the gap `delete_conversation` originally had (an unconditional,
// unsynced `store::delete`, contradicting `docs/design/MULTI_DEVICE_SYNC.md`'s
// "Deletes are tombstones, never physical row removal").
// ---------------------------------------------------------------------------

#[test]
fn deleting_a_conversation_propagates_and_a_stale_append_does_not_resurrect_it() {
    let (mut conn_a, clock_a) = device("device-a");
    let (mut conn_b, clock_b) = device("device-b");
    let sync_key = SyncKey::random();

    let shared = conversation("conv-1", vec![msg("user", "hello")]);
    capture::save_conversation_synced(&mut conn_a, &shared, &clock_a, &sync_key, "mailbox")
        .unwrap();
    deliver(&conn_a, &mut conn_b, &sync_key);
    assert!(
        openpal_core::store::get(&conn_b, "conv-1").is_ok(),
        "sanity: B has the conversation before the delete"
    );

    // Offline: B appends a message (a stale op relative to the deletion
    // A is about to make) before either side has seen the other's change.
    let b_extended = conversation(
        "conv-1",
        vec![msg("user", "hello"), msg("assistant", "one more thing")],
    );
    capture::save_conversation_synced(&mut conn_b, &b_extended, &clock_b, &sync_key, "mailbox")
        .unwrap();

    // A deletes the conversation, causally after B's append never reached
    // it — but HLC-wise, whichever side's clock is ahead determines who
    // "wins"; this test's real point is the mechanism, not the specific
    // ordering, so drive it explicitly: A's delete uses a fresh `clock_a`
    // tick, guaranteed to observe wall-clock time at least as late as B's
    // append (same-process, sequential execution).
    capture::delete_conversation_synced(&mut conn_a, "conv-1", &clock_a, &sync_key, "mailbox")
        .unwrap();

    // Deliver A's deletion to B first, THEN B's stale append back to A —
    // proving delivery order doesn't matter (the guard is HLC-based, not
    // delivery-order-based).
    deliver(&conn_a, &mut conn_b, &sync_key);
    deliver(&conn_b, &mut conn_a, &sync_key);

    assert!(
        openpal_core::store::get(&conn_a, "conv-1").is_err(),
        "A must have the conversation deleted locally"
    );
    assert!(
        openpal_core::store::get(&conn_b, "conv-1").is_err(),
        "B must apply the deletion, not keep (or resurrect) the conversation from its own stale append"
    );
}

#[test]
fn a_later_edit_after_a_stale_delete_still_applies_normally() {
    // The flip side of the guard above: an op that's genuinely CAUSALLY
    // AFTER the deletion (not stale relative to it) must still apply
    // normally — the guard only suppresses stale resurrection, it isn't a
    // permanent lock on the conversation id. This mirrors the same
    // HLC-ordered "newest state wins" rule already proven for
    // conversation_snapshot rewrites.
    let (mut conn_a, clock_a) = device("device-a");
    let (mut conn_b, clock_b) = device("device-b");
    let sync_key = SyncKey::random();

    let shared = conversation("conv-1", vec![msg("user", "hello")]);
    capture::save_conversation_synced(&mut conn_a, &shared, &clock_a, &sync_key, "mailbox")
        .unwrap();
    deliver(&conn_a, &mut conn_b, &sync_key);

    capture::delete_conversation_synced(&mut conn_a, "conv-1", &clock_a, &sync_key, "mailbox")
        .unwrap();
    deliver(&conn_a, &mut conn_b, &sync_key);
    assert!(
        openpal_core::store::get(&conn_b, "conv-1").is_err(),
        "sanity: B applied the deletion"
    );

    // B starts a fresh conversation under the SAME id after the deletion —
    // e.g. the user immediately typed a new message reusing... in practice
    // this would be a new id, but the guard's own correctness only depends
    // on HLC ordering, not on how the new content came to exist, so a
    // direct `apply_local` call (fresh HLC, strictly after the delete) is
    // the precise thing under test.
    let revived = conversation("conv-1", vec![msg("user", "starting fresh")]);
    capture::save_conversation_synced(&mut conn_b, &revived, &clock_b, &sync_key, "mailbox")
        .unwrap();
    deliver(&conn_b, &mut conn_a, &sync_key);

    let a_final = openpal_core::store::get(&conn_a, "conv-1");
    assert!(
        a_final.is_ok(),
        "a genuinely later op must still apply, not be permanently blocked by an older deletion"
    );
    assert_eq!(a_final.unwrap().messages[0].content, "starting fresh");
}

// ---------------------------------------------------------------------------
// Phase 4 — artifact sync. Unlike every other synced table, artifacts are
// triggered by a periodic reconciliation sweep (`sync::artifact_sweep`), not
// a write-time capture — see that module's doc comment for why. These
// scenarios prove the sweep composes correctly with the same real
// encrypt -> outbox -> decrypt -> apply pipeline the other scenarios above
// already exercise.
// ---------------------------------------------------------------------------

#[test]
fn artifact_sync_sweep_pushes_a_new_artifact_over_the_real_encrypted_pipe() {
    let (mut conn_a, clock_a) = device("device-a");
    let (mut conn_b, _clock_b) = device("device-b");
    let sync_key = SyncKey::random();

    // The real write path never calls `capture::save_artifact_synced`
    // directly (see that function's doc comment) — a plain `artifacts::save`
    // mirrors the actual `create_artifact` agent-mode tool's write.
    ensure_conversation_placeholder(&conn_a, "conv-1");
    openpal_core::artifacts::save(&conn_a, "conv-1", "art-1", "report.md", "# Hello").unwrap();
    assert!(
        outbox::drain(&conn_a, 10).unwrap().is_empty(),
        "sanity: a plain save must not itself queue anything"
    );

    let stats = artifact_sweep::sweep_once(&mut conn_a, &clock_a, &sync_key, "mailbox").unwrap();
    assert_eq!(stats.pushed, 1);

    deliver(&conn_a, &mut conn_b, &sync_key);

    let b_artifact = openpal_core::artifacts::get(&conn_b, "conv-1", "art-1").unwrap();
    assert_eq!(b_artifact.content, "# Hello");
}

#[test]
fn artifact_sync_sweep_does_not_requeue_unchanged_content_on_a_later_pass() {
    let (mut conn_a, clock_a) = device("device-a");
    let sync_key = SyncKey::random();

    ensure_conversation_placeholder(&conn_a, "conv-1");
    openpal_core::artifacts::save(&conn_a, "conv-1", "art-1", "report.md", "# Hello").unwrap();
    let first = artifact_sweep::sweep_once(&mut conn_a, &clock_a, &sync_key, "mailbox").unwrap();
    assert_eq!(first.pushed, 1);
    outbox::drain(&conn_a, 10)
        .unwrap()
        .into_iter()
        .for_each(|item| outbox::ack(&conn_a, &item.envelope_id).unwrap());

    let second = artifact_sweep::sweep_once(&mut conn_a, &clock_a, &sync_key, "mailbox").unwrap();
    assert_eq!(second.skipped_unchanged, 1);
    assert_eq!(second.pushed, 0);
    assert!(
        outbox::drain(&conn_a, 10).unwrap().is_empty(),
        "unchanged content must not be re-queued on a later sweep pass"
    );
}

#[test]
fn artifact_sync_sweep_never_syncs_an_oversized_artifact() {
    let (mut conn_a, clock_a) = device("device-a");
    let (mut conn_b, _clock_b) = device("device-b");
    let sync_key = SyncKey::random();

    ensure_conversation_placeholder(&conn_a, "conv-1");
    let oversized = "x".repeat(artifact_sweep::ARTIFACT_SYNC_MAX_BYTES + 1);
    openpal_core::artifacts::save(&conn_a, "conv-1", "art-1", "big.txt", &oversized).unwrap();

    let stats = artifact_sweep::sweep_once(&mut conn_a, &clock_a, &sync_key, "mailbox").unwrap();
    assert_eq!(stats.skipped_oversized, 1);
    assert!(
        outbox::drain(&conn_a, 10).unwrap().is_empty(),
        "an oversized artifact must never leave the device"
    );

    deliver(&conn_a, &mut conn_b, &sync_key);
    assert!(
        openpal_core::artifacts::get(&conn_b, "conv-1", "art-1").is_err(),
        "B must never receive an artifact that was never pushed"
    );
}
