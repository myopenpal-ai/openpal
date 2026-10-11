//! Thin glue between "a table's own save/delete function" and "queue an
//! encrypted change envelope" — so a call site in `crates/app/src/
//! commands.rs` (or an agent-mode tool) is a single function call instead of
//! hand-rolling `field_versions`/`envelope`/`outbox` plumbing at every write
//! site. One `*_synced` function per synced table, plus (for the remote/
//! incoming side `crates/app/src/sync_loop.rs` needs) one `apply_remote_*`
//! function per table.
//!
//! **Deliberately does NOT take `group_secret`.** The suggested shape this
//! module was scaffolded from included it alongside `mailbox_id`, but
//! nothing here actually needs it: `mailbox_id` is already the fully-resolved
//! target address (the caller derives it once via
//! `pairing::data_mailbox_id(&group_secret, pairing::next_gen_n(conn))`), and
//! encryption only needs `sync_key`. Carrying `group_secret` through anyway
//! would be a dead parameter on every function here — and `cargo clippy -D
//! warnings` (this workspace's CI gate) would flag it as unused on every one
//! of them, since none of these tables' envelopes are re-derived from it.
//!
//! Every function here is a plain, synchronous, non-`async` operation over a
//! `&Connection`/`&mut Connection` — callers (Tauri commands) do the async
//! `state.settings.lock().await` check for `sync_enabled` *before* calling
//! into this module, then take the (sync, `std::sync::Mutex`-guarded)
//! `app_db` lock and call in, exactly like every existing plain
//! `store::save`/`projects::save`/etc. call site already does.

use crate::crypto::sync_key::SyncKey;
use crate::custom::CustomModels;
use crate::sync::envelope::{self, ChangeEnvelope};
use crate::sync::hlc::{Hlc, HlcClock};
use crate::sync::{artifact_push_state, conversations, field_versions, outbox};
use crate::types::{Artifact, ModelSpec, Project, Settings};
use crate::Result;
use rusqlite::Connection;
use std::path::Path;

// ---------------------------------------------------------------------------
// Local writes -> plain save + capture + enqueue
// ---------------------------------------------------------------------------

/// `store::save` (via `sync::conversations::apply_local`, which calls it
/// internally) plus turning every populated field of the resulting
/// `AppliedLocal` into its own encrypted envelope, enqueued to `mailbox_id`.
/// See `conversations::apply_local`'s own doc comment for the merge
/// semantics — this function only adds the encrypt+enqueue step on top.
pub fn save_conversation_synced(
    conn: &mut Connection,
    conversation: &crate::types::Conversation,
    clock: &HlcClock,
    sync_key: &SyncKey,
    mailbox_id: &str,
) -> Result<()> {
    let applied = conversations::apply_local(conn, conversation, clock)?;

    for op in applied.message_ops {
        let envelope = ChangeEnvelope::AppendMessage {
            conversation_id: conversation.id.clone(),
            hlc: op.hlc.clone(),
            message: op.message,
        };
        enqueue_envelope(conn, sync_key, mailbox_id, &op.hlc, &envelope)?;
    }
    if let Some((hlc, snapshot)) = applied.snapshot {
        let envelope = ChangeEnvelope::ConversationSnapshot {
            hlc: hlc.clone(),
            conversation: snapshot,
        };
        enqueue_envelope(conn, sync_key, mailbox_id, &hlc, &envelope)?;
    }
    if let Some((hlc, title)) = applied.title_update {
        let envelope = ChangeEnvelope::ConversationTitle {
            conversation_id: conversation.id.clone(),
            hlc: hlc.clone(),
            title,
        };
        enqueue_envelope(conn, sync_key, mailbox_id, &hlc, &envelope)?;
    }
    if let Some((hlc, project_id)) = applied.project_update {
        let envelope = ChangeEnvelope::ConversationProject {
            conversation_id: conversation.id.clone(),
            hlc: hlc.clone(),
            project_id,
        };
        enqueue_envelope(conn, sync_key, mailbox_id, &hlc, &envelope)?;
    }
    Ok(())
}

/// `store::delete` plus capture — a tombstone, not a bare physical delete
/// signal (`docs/design/MULTI_DEVICE_SYNC.md`'s "Deletes are tombstones, never
/// physical row removal"). Records the deletion's `Hlc` under
/// `field_versions` scope `"conversation_deleted"` (so a later-arriving but
/// causally-earlier append/title/project envelope for this conversation is
/// recognized as stale by `crates/app/src/sync_loop.rs`'s resurrection
/// guard, rather than resurrecting it via that file's placeholder-creation
/// step), tombstones every `sync_message_ops` row for this conversation
/// (mirrors [`conversations::tombstone_all`] — nothing left behind for a
/// future rewrite to accidentally revive), and only then physically deletes
/// the local row (`store::delete` is already idempotent on a missing row,
/// so this is safe even if this device never had the conversation at all —
/// it only needs to happen for the local UI to reflect the deletion).
pub fn delete_conversation_synced(
    conn: &mut Connection,
    conversation_id: &str,
    clock: &HlcClock,
    sync_key: &SyncKey,
    mailbox_id: &str,
) -> Result<()> {
    let tx = conn.transaction()?;
    let hlc = field_versions::record_local(&tx, "conversation_deleted", conversation_id, clock)?;
    conversations::tombstone_all(&tx, conversation_id)?;
    crate::store::delete(&tx, conversation_id)?;
    let envelope = ChangeEnvelope::ConversationDeleted {
        conversation_id: conversation_id.to_string(),
        hlc: hlc.clone(),
    };
    enqueue_envelope(&tx, sync_key, mailbox_id, &hlc, &envelope)?;
    tx.commit()?;
    Ok(())
}

/// Applies (or rejects) a received conversation deletion. Returns `true` if
/// applied (tombstoned + deleted), `false` if it lost against a newer local
/// state (e.g. a local edit made after the remote side sent this deletion) —
/// discarded, matching [`apply_remote_project_deleted`]'s own reasoning: a
/// delete's only content is the fact it happened, already fully captured by
/// a small marker rather than needing the conversation's full content
/// preserved in a conflict file.
pub fn apply_remote_conversation_deleted(
    conn: &mut Connection,
    hlc: Hlc,
    conversation_id: &str,
    conflicts_dir: &Path,
) -> Result<bool> {
    let tx = conn.transaction()?;
    let wins =
        field_versions::try_apply_remote(&tx, "conversation_deleted", conversation_id, &hlc)?;
    if wins {
        conversations::tombstone_all(&tx, conversation_id)?;
        crate::store::delete(&tx, conversation_id)?;
    } else {
        #[derive(serde::Serialize)]
        struct AttemptedDelete<'a> {
            attempted_delete: bool,
            conversation_id: &'a str,
        }
        field_versions::write_conflict_file(
            conflicts_dir,
            "conversation_deleted",
            conversation_id,
            &hlc,
            &AttemptedDelete {
                attempted_delete: true,
                conversation_id,
            },
        )?;
    }
    tx.commit()?;
    Ok(wins)
}

pub fn save_project_synced(
    conn: &mut Connection,
    project: &Project,
    clock: &HlcClock,
    sync_key: &SyncKey,
    mailbox_id: &str,
) -> Result<()> {
    let tx = conn.transaction()?;
    crate::projects::save(&tx, project)?;
    let hlc = field_versions::record_local(&tx, "projects", &project.id, clock)?;
    let envelope = ChangeEnvelope::ProjectRow {
        hlc: hlc.clone(),
        project: project.clone(),
    };
    enqueue_envelope(&tx, sync_key, mailbox_id, &hlc, &envelope)?;
    tx.commit()?;
    Ok(())
}

pub fn delete_project_synced(
    conn: &mut Connection,
    project_id: &str,
    clock: &HlcClock,
    sync_key: &SyncKey,
    mailbox_id: &str,
) -> Result<()> {
    let tx = conn.transaction()?;
    crate::projects::delete(&tx, project_id)?;
    let hlc = field_versions::record_local(&tx, "projects", project_id, clock)?;
    let envelope = ChangeEnvelope::ProjectDeleted {
        row_key: project_id.to_string(),
        hlc: hlc.clone(),
    };
    enqueue_envelope(&tx, sync_key, mailbox_id, &hlc, &envelope)?;
    tx.commit()?;
    Ok(())
}

/// `artifacts::save` plus capture. Returns the saved `Artifact` (sanitized
/// filename included), same as the plain function. Called exclusively by
/// `crate::sync::artifact_sweep::sweep_once` (Phase 4) — the only artifact
/// WRITE call site (the agent-mode `create_artifact` tool) has no `AppState`
/// access to reach a `SyncKey`/mailbox id, so sync is triggered by a
/// periodic reconciliation sweep rather than at write time; see
/// `artifact_sweep`'s own doc comment for why. Also records the pushed
/// content's hash via `artifact_push_state::set`, in the same transaction,
/// so the sweep's next pass can tell this content apart from a real edit.
#[allow(clippy::too_many_arguments)] // each param is independently necessary; grouping them would be an abstraction for its own sake, same call as agent_bridge.rs::build_deps's existing allow
pub fn save_artifact_synced(
    conn: &mut Connection,
    conversation_id: &str,
    artifact_id: &str,
    filename: &str,
    content: &str,
    clock: &HlcClock,
    sync_key: &SyncKey,
    mailbox_id: &str,
) -> Result<Artifact> {
    let tx = conn.transaction()?;
    let artifact = crate::artifacts::save(&tx, conversation_id, artifact_id, filename, content)?;
    let row_key = artifact_row_key(conversation_id, artifact_id);
    let hlc = field_versions::record_local(&tx, "artifacts", &row_key, clock)?;
    let envelope = ChangeEnvelope::ArtifactRow {
        hlc: hlc.clone(),
        conversation_id: conversation_id.to_string(),
        artifact_id: artifact_id.to_string(),
        filename: artifact.filename.clone(),
        content: artifact.content.clone(),
    };
    enqueue_envelope(&tx, sync_key, mailbox_id, &hlc, &envelope)?;
    artifact_push_state::set(
        &tx,
        conversation_id,
        artifact_id,
        &artifact_push_state::hash_content(content),
    )?;
    tx.commit()?;
    Ok(artifact)
}

pub fn save_settings_synced(
    conn: &mut Connection,
    settings: &Settings,
    clock: &HlcClock,
    sync_key: &SyncKey,
    mailbox_id: &str,
) -> Result<()> {
    let tx = conn.transaction()?;
    crate::settings::save(&tx, settings)?;
    let hlc = field_versions::record_local(&tx, "settings", "current", clock)?;
    let settings_json = serde_json::to_string(settings)?;
    let envelope = ChangeEnvelope::SettingsRow {
        hlc: hlc.clone(),
        settings_json,
    };
    enqueue_envelope(&tx, sync_key, mailbox_id, &hlc, &envelope)?;
    tx.commit()?;
    Ok(())
}

pub fn save_custom_models_synced(
    conn: &mut Connection,
    custom: &CustomModels,
    clock: &HlcClock,
    sync_key: &SyncKey,
    mailbox_id: &str,
) -> Result<()> {
    let tx = conn.transaction()?;
    crate::custom::save(&tx, custom)?;
    let hlc = field_versions::record_local(&tx, "custom_models", "current", clock)?;
    let envelope = ChangeEnvelope::CustomModelsRow {
        hlc: hlc.clone(),
        models: custom.all().to_vec(),
    };
    enqueue_envelope(&tx, sync_key, mailbox_id, &hlc, &envelope)?;
    tx.commit()?;
    Ok(())
}

fn enqueue_envelope(
    conn: &Connection,
    sync_key: &SyncKey,
    mailbox_id: &str,
    hlc: &Hlc,
    envelope: &ChangeEnvelope,
) -> Result<()> {
    let ciphertext = envelope::encrypt(sync_key, envelope)?;
    outbox::enqueue(conn, &hlc.op_id(), mailbox_id, &ciphertext)
}

fn artifact_row_key(conversation_id: &str, artifact_id: &str) -> String {
    format!("{conversation_id}:{artifact_id}")
}

// ---------------------------------------------------------------------------
// Remote/incoming writes -> LWW compare, apply-or-conflict-file
// ---------------------------------------------------------------------------
//
// Mirrors `sync::conversations::apply_remote_*`'s shape for the other four
// synced tables: `field_versions::try_apply_remote` decides win/lose; a win
// applies via the table's own existing `save`/`delete` (unchanged); a loss
// writes the losing content to a conflict file instead of applying it,
// never silently dropping it. Called by `crates/app/src/sync_loop.rs` once
// it decrypts an envelope and dispatches on its variant.

pub fn apply_remote_project_row(
    conn: &Connection,
    hlc: Hlc,
    project: Project,
    conflicts_dir: &Path,
) -> Result<bool> {
    let wins = field_versions::try_apply_remote(conn, "projects", &project.id, &hlc)?;
    if wins {
        crate::projects::save(conn, &project)?;
    } else {
        field_versions::write_conflict_file(
            conflicts_dir,
            "projects",
            &project.id,
            &hlc,
            &project,
        )?;
    }
    Ok(wins)
}

/// A losing `ProjectDeleted` has no richer content than the fact that a
/// delete was attempted — the conflict file records that, not a `Project`
/// value that doesn't exist in this envelope.
pub fn apply_remote_project_deleted(
    conn: &Connection,
    hlc: Hlc,
    row_key: &str,
    conflicts_dir: &Path,
) -> Result<bool> {
    let wins = field_versions::try_apply_remote(conn, "projects", row_key, &hlc)?;
    if wins {
        crate::projects::delete(conn, row_key)?;
    } else {
        #[derive(serde::Serialize)]
        struct AttemptedDelete<'a> {
            attempted_delete: bool,
            row_key: &'a str,
        }
        field_versions::write_conflict_file(
            conflicts_dir,
            "projects",
            row_key,
            &hlc,
            &AttemptedDelete {
                attempted_delete: true,
                row_key,
            },
        )?;
    }
    Ok(wins)
}

#[derive(serde::Serialize)]
struct ConflictArtifact<'a> {
    conversation_id: &'a str,
    artifact_id: &'a str,
    filename: &'a str,
    content: &'a str,
}

pub fn apply_remote_artifact_row(
    conn: &Connection,
    hlc: Hlc,
    conversation_id: &str,
    artifact_id: &str,
    filename: &str,
    content: &str,
    conflicts_dir: &Path,
) -> Result<bool> {
    let row_key = artifact_row_key(conversation_id, artifact_id);
    let wins = field_versions::try_apply_remote(conn, "artifacts", &row_key, &hlc)?;
    if wins {
        crate::artifacts::save(conn, conversation_id, artifact_id, filename, content)?;
    } else {
        field_versions::write_conflict_file(
            conflicts_dir,
            "artifacts",
            &row_key,
            &hlc,
            &ConflictArtifact {
                conversation_id,
                artifact_id,
                filename,
                content,
            },
        )?;
    }
    Ok(wins)
}

/// `settings_json` is the whole `Settings` blob, already serialized (see
/// `ChangeEnvelope::SettingsRow`'s own doc comment for why the wire schema
/// carries it opaquely). A malformed incoming blob falls back to recording it
/// as a plain string in the conflict file rather than panicking or silently
/// discarding it.
pub fn apply_remote_settings_row(
    conn: &Connection,
    hlc: Hlc,
    settings_json: &str,
    conflicts_dir: &Path,
) -> Result<bool> {
    let wins = field_versions::try_apply_remote(conn, "settings", "current", &hlc)?;
    if wins {
        let settings: Settings = serde_json::from_str(settings_json)?;
        crate::settings::save(conn, &settings)?;
    } else {
        let content: serde_json::Value = serde_json::from_str(settings_json)
            .unwrap_or_else(|_| serde_json::Value::String(settings_json.to_string()));
        field_versions::write_conflict_file(conflicts_dir, "settings", "current", &hlc, &content)?;
    }
    Ok(wins)
}

pub fn apply_remote_custom_models_row(
    conn: &Connection,
    hlc: Hlc,
    models: Vec<ModelSpec>,
    conflicts_dir: &Path,
) -> Result<bool> {
    let wins = field_versions::try_apply_remote(conn, "custom_models", "current", &hlc)?;
    if wins {
        let mut custom = CustomModels::default();
        for model in models {
            custom.add(model);
        }
        crate::custom::save(conn, &custom)?;
    } else {
        field_versions::write_conflict_file(
            conflicts_dir,
            "custom_models",
            "current",
            &hlc,
            &models,
        )?;
    }
    Ok(wins)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::hlc::HlcClock;
    use crate::types::{Conversation, SamplingParams, TaskType};

    fn test_conn() -> Connection {
        let dir = tempfile::tempdir().unwrap().keep();
        crate::app_db::open(&dir.join("app.sqlite"), None)
            .unwrap()
            .0
    }

    fn one_envelope(conn: &Connection, sync_key: &SyncKey) -> ChangeEnvelope {
        let items = outbox::drain(conn, 10).unwrap();
        assert_eq!(items.len(), 1, "expected exactly one queued envelope");
        envelope::decrypt(sync_key, &items[0].ciphertext).unwrap()
    }

    fn project(id: &str, name: &str) -> Project {
        Project {
            id: id.to_string(),
            name: name.to_string(),
            created_at_ms: 1,
            updated_at_ms: 1,
        }
    }

    fn conversation(id: &str, messages: Vec<crate::types::ChatMessage>) -> Conversation {
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

    fn msg(role: &str, content: &str) -> crate::types::ChatMessage {
        crate::types::ChatMessage {
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

    // -----------------------------------------------------------------
    // Local writes: row written + exactly the expected envelope(s) queued.
    // -----------------------------------------------------------------

    #[test]
    fn save_project_synced_writes_row_and_queues_matching_envelope() {
        let mut conn = test_conn();
        let clock = HlcClock::new("device-a".to_string());
        let sync_key = SyncKey::random();
        let p = project("proj-1", "Research");

        save_project_synced(&mut conn, &p, &clock, &sync_key, "mailbox-1").unwrap();

        assert_eq!(
            crate::projects::get(&conn, "proj-1").unwrap().name,
            "Research"
        );
        match one_envelope(&conn, &sync_key) {
            ChangeEnvelope::ProjectRow { project, .. } => assert_eq!(project.name, "Research"),
            other => panic!("wrong envelope variant: {other:?}"),
        }
    }

    #[test]
    fn delete_project_synced_deletes_row_and_queues_matching_envelope() {
        let mut conn = test_conn();
        let clock = HlcClock::new("device-a".to_string());
        let sync_key = SyncKey::random();
        crate::projects::save(&conn, &project("proj-1", "Research")).unwrap();
        outbox::ack(&conn, "ignored").unwrap(); // no-op, keeps parity with other tests' shape

        delete_project_synced(&mut conn, "proj-1", &clock, &sync_key, "mailbox-1").unwrap();

        assert!(crate::projects::get(&conn, "proj-1").is_err());
        match one_envelope(&conn, &sync_key) {
            ChangeEnvelope::ProjectDeleted { row_key, .. } => assert_eq!(row_key, "proj-1"),
            other => panic!("wrong envelope variant: {other:?}"),
        }
    }

    #[test]
    fn save_artifact_synced_writes_row_and_queues_matching_envelope() {
        let mut conn = test_conn();
        let clock = HlcClock::new("device-a".to_string());
        let sync_key = SyncKey::random();
        crate::store::save(&conn, &conversation("conv-1", vec![])).unwrap();

        let saved = save_artifact_synced(
            &mut conn,
            "conv-1",
            "call-1",
            "report.md",
            "# Hello",
            &clock,
            &sync_key,
            "mailbox-1",
        )
        .unwrap();
        assert_eq!(saved.filename, "report.md");

        assert_eq!(
            crate::artifacts::get(&conn, "conv-1", "call-1")
                .unwrap()
                .content,
            "# Hello"
        );
        match one_envelope(&conn, &sync_key) {
            ChangeEnvelope::ArtifactRow {
                conversation_id,
                artifact_id,
                content,
                ..
            } => {
                assert_eq!(conversation_id, "conv-1");
                assert_eq!(artifact_id, "call-1");
                assert_eq!(content, "# Hello");
            }
            other => panic!("wrong envelope variant: {other:?}"),
        }
        assert_eq!(
            artifact_push_state::get(&conn, "conv-1", "call-1"),
            Some(artifact_push_state::hash_content("# Hello")),
            "the pushed content's hash must be recorded atomically alongside the envelope"
        );
    }

    #[test]
    fn save_settings_synced_writes_row_and_queues_matching_envelope() {
        let mut conn = test_conn();
        let clock = HlcClock::new("device-a".to_string());
        let sync_key = SyncKey::random();
        let settings = Settings {
            onboarding_completed: true,
            ..Settings::default()
        };

        save_settings_synced(&mut conn, &settings, &clock, &sync_key, "mailbox-1").unwrap();

        assert!(crate::settings::load(&conn).onboarding_completed);
        match one_envelope(&conn, &sync_key) {
            ChangeEnvelope::SettingsRow { settings_json, .. } => {
                let round_tripped: Settings = serde_json::from_str(&settings_json).unwrap();
                assert!(round_tripped.onboarding_completed);
            }
            other => panic!("wrong envelope variant: {other:?}"),
        }
    }

    #[test]
    fn save_custom_models_synced_writes_row_and_queues_matching_envelope() {
        let mut conn = test_conn();
        let clock = HlcClock::new("device-a".to_string());
        let sync_key = SyncKey::random();
        let mut custom = CustomModels::default();
        let id = custom.add(crate::custom::local_spec(
            Path::new("/tmp/model.gguf"),
            1_000_000,
            &[],
        ));

        save_custom_models_synced(&mut conn, &custom, &clock, &sync_key, "mailbox-1").unwrap();

        assert!(crate::custom::load(&conn).get(&id).is_some());
        match one_envelope(&conn, &sync_key) {
            ChangeEnvelope::CustomModelsRow { models, .. } => {
                assert_eq!(models.len(), 1);
                assert_eq!(models[0].id, id);
            }
            other => panic!("wrong envelope variant: {other:?}"),
        }
    }

    #[test]
    fn save_conversation_synced_pure_append_queues_one_append_message_envelope() {
        let mut conn = test_conn();
        let clock = HlcClock::new("device-a".to_string());
        let sync_key = SyncKey::random();

        let seed = conversation("conv-1", vec![msg("user", "hello")]);
        save_conversation_synced(&mut conn, &seed, &clock, &sync_key, "mailbox-1").unwrap();
        // Seeding queues one envelope per pre-existing message; drain it so
        // the next assertion only sees the pure-append envelope below.
        outbox::drain(&conn, 10)
            .unwrap()
            .into_iter()
            .for_each(|item| {
                outbox::ack(&conn, &item.envelope_id).unwrap();
            });

        let extended = conversation("conv-1", vec![msg("user", "hello"), msg("assistant", "hi")]);
        save_conversation_synced(&mut conn, &extended, &clock, &sync_key, "mailbox-1").unwrap();

        match one_envelope(&conn, &sync_key) {
            ChangeEnvelope::AppendMessage {
                conversation_id,
                message,
                ..
            } => {
                assert_eq!(conversation_id, "conv-1");
                assert_eq!(message.content, "hi");
            }
            other => panic!("wrong envelope variant: {other:?}"),
        }
    }

    #[test]
    fn save_conversation_synced_title_change_queues_a_title_envelope() {
        let mut conn = test_conn();
        let clock = HlcClock::new("device-a".to_string());
        let sync_key = SyncKey::random();

        let seed = conversation("conv-1", vec![msg("user", "hello")]);
        save_conversation_synced(&mut conn, &seed, &clock, &sync_key, "mailbox-1").unwrap();
        outbox::drain(&conn, 10)
            .unwrap()
            .into_iter()
            .for_each(|item| {
                outbox::ack(&conn, &item.envelope_id).unwrap();
            });

        let mut renamed = seed.clone();
        renamed.title = "Trip planning".to_string();
        save_conversation_synced(&mut conn, &renamed, &clock, &sync_key, "mailbox-1").unwrap();

        match one_envelope(&conn, &sync_key) {
            ChangeEnvelope::ConversationTitle {
                conversation_id,
                title,
                ..
            } => {
                assert_eq!(conversation_id, "conv-1");
                assert_eq!(title, "Trip planning");
            }
            other => panic!("wrong envelope variant: {other:?}"),
        }
    }

    // -----------------------------------------------------------------
    // Remote/incoming writes: wins apply, losses write a conflict file.
    // -----------------------------------------------------------------

    #[test]
    fn apply_remote_project_row_wins_when_newer_and_loses_when_older() {
        let conn = test_conn();
        let clock = HlcClock::new("device-a".to_string());
        let conflicts_dir = tempfile::tempdir().unwrap();

        let newer = clock.next();
        let p = project("proj-1", "From remote");
        assert!(apply_remote_project_row(&conn, newer, p.clone(), conflicts_dir.path()).unwrap());
        assert_eq!(
            crate::projects::get(&conn, "proj-1").unwrap().name,
            "From remote"
        );

        let older = Hlc {
            physical_ms: 1,
            counter: 0,
            device_id: "device-b".to_string(),
        };
        let stale = project("proj-1", "Stale rename");
        assert!(
            !apply_remote_project_row(&conn, older.clone(), stale, conflicts_dir.path()).unwrap()
        );
        assert_eq!(
            crate::projects::get(&conn, "proj-1").unwrap().name,
            "From remote",
            "a losing remote write must not overwrite the newer local state"
        );
        let expected = conflicts_dir
            .path()
            .join(format!("projects-proj-1-{}.json", older.op_id()));
        assert!(
            expected.exists(),
            "the losing write must be preserved in a conflict file"
        );
    }

    #[test]
    fn apply_remote_project_deleted_wins_and_deletes() {
        let conn = test_conn();
        let clock = HlcClock::new("device-a".to_string());
        let conflicts_dir = tempfile::tempdir().unwrap();
        crate::projects::save(&conn, &project("proj-1", "Research")).unwrap();

        let hlc = clock.next();
        assert!(apply_remote_project_deleted(&conn, hlc, "proj-1", conflicts_dir.path()).unwrap());
        assert!(crate::projects::get(&conn, "proj-1").is_err());
    }

    #[test]
    fn apply_remote_artifact_row_wins_and_loses_correctly() {
        let conn = test_conn();
        let clock = HlcClock::new("device-a".to_string());
        let conflicts_dir = tempfile::tempdir().unwrap();
        crate::store::save(&conn, &conversation("conv-1", vec![])).unwrap();

        let newer = clock.next();
        assert!(apply_remote_artifact_row(
            &conn,
            newer,
            "conv-1",
            "call-1",
            "f.txt",
            "from remote",
            conflicts_dir.path()
        )
        .unwrap());
        assert_eq!(
            crate::artifacts::get(&conn, "conv-1", "call-1")
                .unwrap()
                .content,
            "from remote"
        );

        let older = Hlc {
            physical_ms: 1,
            counter: 0,
            device_id: "device-b".to_string(),
        };
        assert!(!apply_remote_artifact_row(
            &conn,
            older.clone(),
            "conv-1",
            "call-1",
            "f.txt",
            "stale",
            conflicts_dir.path()
        )
        .unwrap());
        assert_eq!(
            crate::artifacts::get(&conn, "conv-1", "call-1")
                .unwrap()
                .content,
            "from remote"
        );
        let expected = conflicts_dir
            .path()
            .join(format!("artifacts-conv-1:call-1-{}.json", older.op_id()));
        assert!(expected.exists());
    }

    #[test]
    fn apply_remote_settings_row_wins_and_applies() {
        let conn = test_conn();
        let clock = HlcClock::new("device-a".to_string());
        let conflicts_dir = tempfile::tempdir().unwrap();
        let settings = Settings {
            onboarding_completed: true,
            ..Settings::default()
        };
        let settings_json = serde_json::to_string(&settings).unwrap();

        let hlc = clock.next();
        assert!(
            apply_remote_settings_row(&conn, hlc, &settings_json, conflicts_dir.path()).unwrap()
        );
        assert!(crate::settings::load(&conn).onboarding_completed);
    }

    #[test]
    fn apply_remote_custom_models_row_wins_and_applies() {
        let conn = test_conn();
        let clock = HlcClock::new("device-a".to_string());
        let conflicts_dir = tempfile::tempdir().unwrap();
        let spec = crate::custom::local_spec(Path::new("/tmp/model.gguf"), 1_000_000, &[]);
        let id = spec.id.clone();

        let hlc = clock.next();
        assert!(
            apply_remote_custom_models_row(&conn, hlc, vec![spec], conflicts_dir.path()).unwrap()
        );
        assert!(crate::custom::load(&conn).get(&id).is_some());
    }
}
