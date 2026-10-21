//! Background loop that pushes locally-queued sync changes to the relay and
//! pulls/applies incoming ones — the write side of Phase 3 multi-device
//! conversation sync (`docs/design/MULTI_DEVICE_SYNC.md`,
//! `docs/SYNC_PAIRING_PROTOCOL.md`). Spawned once for the app's lifetime
//! from `state.rs::finish_unlock`, alongside `idle::run`/`parental::run`.
//!
//! Phase 0-2 built manual import/export, the relay transport, and device
//! pairing (`openpal_core::sync::pairing`). This loop is the piece that
//! actually moves conversation/project/artifact/settings/custom-model
//! changes over that pairing infrastructure: drain `sync_outbox` and push
//! each item, then pull whatever's waiting in this device's data mailbox and
//! route it into the right apply function — `openpal_core::sync::
//! conversations::apply_remote_*` for the four conversation-shaped envelope
//! variants, `openpal_core::sync::capture::apply_remote_*` (which wraps
//! `field_versions`'s win/lose compare + conflict-file write) for the other
//! five. The write-path side (enqueuing into `sync_outbox` alongside each
//! local write) lives in `commands.rs`, not here.
//!
//! Zero-cost when sync is off (`Settings::sync_enabled == false`, the
//! default): each tick reads that one setting under a short-lived lock and
//! returns immediately — the same "zero cost when off" rule
//! `AppState::pairing`'s own doc comment establishes for Phase 2's pairing
//! UI. Likewise, sync being enabled but not yet paired on this device (no
//! local identity, or no group secret sealed yet) is a normal, expected
//! state — e.g. before the user has ever created or joined a sync group —
//! and returns quietly rather than logging a warning every tick.
//!
//! `Settings::sync_relay_url` (Settings → Devices → Sync) can repoint
//! where this loop reaches the relay: `commands::update_settings` validates
//! a new value and calls `MailboxClient::set_base_url` on the SAME client
//! `state.pairing.transport()` returns here — the exact one the pairing
//! flows already use — rather than this loop building a second client from
//! the setting itself, which would risk push/pull talking to a different
//! relay than pairing negotiated the mailbox addresses against.

use crate::state::AppState;
use openpal_core::sync::envelope::{self, ChangeEnvelope};
use openpal_core::sync::hlc::Hlc;
use openpal_core::sync::{
    artifact_sweep, capture, conversations, field_versions, key_state, outbox, pairing, roster,
};
use openpal_core::types::{ChatMessage, Conversation, SamplingParams, TaskType};
use std::path::Path;
use std::time::Duration;
use tauri::{AppHandle, Manager};

/// How often to check for outbound/inbound sync work. Coarser cadence than
/// `idle.rs`'s own 5s isn't needed for a first cut — sync doesn't need to be
/// faster than that either, so this reuses the same interval rather than
/// inventing a different one for no reason.
const TICK_INTERVAL: Duration = Duration::from_secs(5);

/// How long a pushed envelope survives on the relay before expiring
/// unclaimed. Long enough that a currently-offline device still receives it
/// whenever it next comes online — same rationale as `pairing.rs`'s own
/// `ROTATION_ENVELOPE_TTL_SECONDS`.
const PUSH_TTL_SECONDS: u64 = 30 * 24 * 3600;

/// Cap on how many outbox items one tick pushes, so a long backlog built up
/// while offline doesn't try to push everything in a single tick.
const PUSH_BATCH_LIMIT: usize = 50;

/// Phase 4 artifact sync's trigger cadence — every 12th tick, i.e. ~60s at
/// `TICK_INTERVAL`. Artifacts are created rarely (an explicit agent-mode
/// tool call, not a per-message event) and effectively never mutated in
/// place once created, so hashing the whole `artifacts` table on every 5s
/// tick like push/pull would be near-total waste almost every pass. See
/// `openpal_core::sync::artifact_sweep`'s own doc comment for why this is a
/// periodic sweep rather than a write-time capture like every other synced
/// table.
const ARTIFACT_SWEEP_EVERY_N_TICKS: u64 = 12;

/// Periodic (not just device-removal-triggered) SyncKey rotation — silent,
/// default-path key hygiene with no setting a beginner has to configure.
/// Whichever comes first, matching `docs/SYNC_PAIRING_PROTOCOL.md`'s
/// "Periodic rotation": 90 days since this device's current key was
/// installed, or 500 envelopes seen (pushed or pulled) under it.
const ROTATION_MAX_AGE_MS: i64 = 90 * 24 * 3600 * 1000;
const ROTATION_MAX_ENVELOPES: u32 = 500;

pub async fn run(app: AppHandle) {
    let mut ticker = tokio::time::interval(TICK_INTERVAL);
    let mut tick_count: u64 = 0;
    loop {
        ticker.tick().await;
        let Some(state) = app.try_state::<AppState>() else {
            continue;
        };
        // Runs on tick 0 too (the very first tick after boot), so artifacts
        // created before sync was enabled — or while offline — get picked
        // up immediately rather than waiting a full sweep interval.
        let run_artifact_sweep = tick_count.is_multiple_of(ARTIFACT_SWEEP_EVERY_N_TICKS);
        tick_count += 1;
        if let Err(e) = tick(&state, run_artifact_sweep).await {
            tracing::warn!("sync tick failed: {e}");
        }
    }
}

/// One push+pull cycle. Only a genuinely tick-fatal error (can't read the
/// outbox, a pull request itself failing) propagates out of here — a single
/// failed push or a single pulled envelope that fails to apply is logged and
/// skipped inline so it doesn't abort the rest of the tick.
async fn tick(state: &AppState, run_artifact_sweep: bool) -> openpal_core::Result<()> {
    // Zero cost when off.
    let sync_enabled = state.settings.lock().await.sync_enabled;
    if !sync_enabled {
        return Ok(());
    }

    // Not paired yet — normal/expected, not an error.
    if !state.pairing.has_local_identity() {
        return Ok(());
    }
    let group_secret = match state.pairing.load_group_secret() {
        Ok(secret) => secret,
        Err(_) => return Ok(()),
    };
    let sync_key = match state.pairing.load_sync_key() {
        Ok(key) => key,
        Err(_) => return Ok(()),
    };

    // Always the same transport the pairing flows already use.
    let transport = state.pairing.transport();

    // `gen_n` and this device's own `device_id`: synchronous DB read, lock
    // dropped before any `.await` below — `rusqlite::Connection` isn't
    // `Sync`, so a `std::sync::MutexGuard` around it must never cross an
    // await point (same discipline `pairing.rs`'s doc comments establish at
    // length, and `commands::sync_device_remove` already follows).
    let (gen_n, own_device_id) = {
        let conn = state.app_db.lock().unwrap();
        let gen_n = pairing::next_gen_n(&conn);
        let own_device_id = roster::list(&conn)
            .into_iter()
            .find(|entry| entry.is_self)
            .map(|entry| entry.device_id);
        (gen_n, own_device_id)
    };
    let Some(own_device_id) = own_device_id else {
        // Paired (group secret + sync key both loaded above) but no
        // `is_self` roster row yet — shouldn't normally happen; nothing safe
        // to do this tick.
        tracing::warn!("sync: paired but no self roster row found; skipping this tick");
        return Ok(());
    };

    let mailbox = pairing::data_mailbox_id(&group_secret, gen_n);

    // Artifact sync's trigger — see `ARTIFACT_SWEEP_EVERY_N_TICKS`'s doc
    // comment and `openpal_core::sync::artifact_sweep`'s own doc comment for
    // why this is a periodic sweep rather than a write-time capture. Run
    // before the push phase below so a freshly-detected change is pushed in
    // this same tick, not one cycle later.
    if run_artifact_sweep {
        let mut conn = state.app_db.lock().unwrap();
        match artifact_sweep::sweep_once(&mut conn, &state.sync_clock, &sync_key, &mailbox) {
            Ok(stats) if stats.pushed > 0 || stats.failed > 0 => {
                tracing::debug!(?stats, "sync: artifact sweep")
            }
            Ok(_) => {}
            Err(e) => tracing::warn!("sync: artifact sweep failed: {e}"),
        }
    }

    // Push whatever's queued.
    let outbox_items = {
        let conn = state.app_db.lock().unwrap();
        outbox::drain(&conn, PUSH_BATCH_LIMIT)?
    };
    let mut pushed_count: u32 = 0;
    for item in outbox_items {
        match transport
            .push(&mailbox, &item.ciphertext, 1, Some(PUSH_TTL_SECONDS))
            .await
        {
            Ok(_) => {
                pushed_count += 1;
                let conn = state.app_db.lock().unwrap();
                if let Err(e) = outbox::ack(&conn, &item.envelope_id) {
                    tracing::warn!(
                        "sync: pushed but couldn't ack outbox item {}: {e}",
                        item.envelope_id
                    );
                }
            }
            Err(e) => {
                // A briefly-unreachable relay isn't exceptional — leave it
                // queued, the next tick retries.
                tracing::debug!(
                    "sync: push failed for outbox item {}, will retry: {e}",
                    item.envelope_id
                );
            }
        }
    }

    // Pull and apply.
    let pulled = transport
        .pull(&mailbox)
        .await
        .map_err(|e| openpal_core::Error::Pairing(format!("sync pull failed: {e}")))?;

    let conflicts_dir = state.paths.data_dir.join("sync-conflicts");
    let mut batched_messages: Vec<(String, Hlc, ChatMessage)> = Vec::new();
    let mut batched_envelope_ids: Vec<i64> = Vec::new();
    let mut to_ack: Vec<i64> = Vec::new();

    for pulled_env in &pulled {
        let Some(ciphertext) = pulled_env.ciphertext() else {
            // Malformed base64 — never fatal, same posture
            // `mailbox_client.rs` documents for `ciphertext()` itself.
            to_ack.push(pulled_env.envelope_id);
            continue;
        };
        let change = match envelope::decrypt(&sync_key, &ciphertext) {
            Ok(change) => change,
            Err(_) => {
                // Garbled or foreign envelope — discard, never partially
                // trust (same posture `envelope::decrypt`'s doc comment
                // establishes).
                to_ack.push(pulled_env.envelope_id);
                continue;
            }
        };
        if envelope_device_id(&change) == own_device_id.as_str() {
            // Our own push, echoed back on the shared mailbox — applying it
            // through the remote path would be wasted work at best (an
            // equal HLC never wins an LWW comparison) and risks treating our
            // own change as a conflict. Skip applying, still ack it so it
            // doesn't linger.
            to_ack.push(pulled_env.envelope_id);
            continue;
        }

        match route_pulled_envelope(state, &conflicts_dir, change) {
            Ok(Some(message_op)) => {
                batched_messages.push(message_op);
                batched_envelope_ids.push(pulled_env.envelope_id);
            }
            Ok(None) => to_ack.push(pulled_env.envelope_id),
            Err(e) => {
                // A hard error (e.g. a DB error) — leave unacked, the next
                // tick retries this envelope. Keep going with the rest of
                // the batch rather than aborting it.
                tracing::warn!(
                    "sync: failed to apply pulled envelope {}: {e}",
                    pulled_env.envelope_id
                );
            }
        }
    }

    // Batch every AppendMessage op collected above into one
    // recompute-and-save pass per conversation, rather than one per op —
    // see `apply_remote_messages_batch`'s own doc comment. Every envelope
    // that fed into the batch is acked together, only if the whole batch
    // succeeded; on failure none of them are acked, so the next tick
    // re-pulls and retries them (each op insert is itself idempotent, so a
    // retried batch is safe).
    if !batched_messages.is_empty() {
        let batch_len = batched_messages.len();
        let batch_result = {
            let mut conn = state.app_db.lock().unwrap();
            conversations::apply_remote_messages_batch(&mut conn, batched_messages)
        };
        match batch_result {
            Ok(()) => to_ack.extend(batched_envelope_ids),
            Err(e) => {
                tracing::warn!("sync: failed to apply a batch of {batch_len} pulled messages: {e}")
            }
        }
    }

    for envelope_id in to_ack {
        if let Err(e) = transport.ack(&mailbox, envelope_id).await {
            tracing::debug!("sync: couldn't ack pulled envelope {envelope_id}: {e}");
        }
    }

    let keys_dir = state.paths.data_dir.join("keys");

    // Control-mailbox poll: apply any incoming device-removal or periodic
    // key rotation addressed to this device. `apply_incoming_rotate`
    // previously had no caller anywhere in the shipped app — the "Add a
    // device" screen's own poll loop only ever looked at `JoinRequest`s,
    // never `Rotate`s, and nothing else polled `control_mailbox_id` in the
    // background — so a survivor had no code path that would ever pick up
    // a rotation. This tick is that path now.
    let control_mailbox = pairing::control_mailbox_id(&group_secret);
    let control_pulled = transport.pull(&control_mailbox).await.map_err(|e| {
        openpal_core::Error::Pairing(format!("sync control-mailbox pull failed: {e}"))
    })?;
    for env in &control_pulled {
        let Some(raw) = env.ciphertext() else {
            continue;
        };
        let applied = {
            let conn = state.app_db.lock().unwrap();
            pairing::apply_incoming_rotate(
                &conn,
                &keys_dir,
                crate::secret_store::store().as_ref(),
                &group_secret,
                &own_device_id,
                &raw,
            )
        };
        match applied {
            Ok(true) => {
                if let Err(e) = transport.ack(&control_mailbox, env.envelope_id).await {
                    tracing::debug!(
                        "sync: couldn't ack applied rotation envelope {}: {e}",
                        env.envelope_id
                    );
                }
            }
            Ok(false) => {
                // Not addressed to us, stale/already-applied, or failed
                // verification — leave unacked. `deliver_rotations` pushes
                // one copy per survivor to this SAME mailbox; acking a
                // copy that wasn't ours would delete another survivor's
                // still-needed envelope before they poll it.
            }
            Err(e) => tracing::warn!(
                "sync: failed applying control-mailbox envelope {}: {e}",
                env.envelope_id
            ),
        }
    }

    // Periodic rotation trigger — see `ROTATION_MAX_AGE_MS`/
    // `ROTATION_MAX_ENVELOPES`'s doc comment and
    // `docs/SYNC_PAIRING_PROTOCOL.md`'s "Periodic rotation" for the policy.
    // Evaluated after this tick's own control-mailbox processing above, so
    // a rotation this device just received (which resets its own
    // `envelopes_since_rotation`/`key_installed_at_ms`) is reflected before
    // deciding whether ANOTHER rotation is also due — avoids a device
    // compounding two rotations in the same tick.
    let envelope_activity = pushed_count + pulled.len() as u32;
    let periodic_rotation = {
        let conn = state.app_db.lock().unwrap();
        let envelopes_since_rotation = key_state::record_envelopes(&conn, envelope_activity)?;
        let key_installed_at_ms = key_state::get(&conn).key_installed_at_ms;
        let age_ms = now_ms() as i64 - key_installed_at_ms;
        let due =
            age_ms >= ROTATION_MAX_AGE_MS || envelopes_since_rotation >= ROTATION_MAX_ENVELOPES;
        if due && pairing::is_designated_initiator(&roster::list(&conn), &own_device_id) {
            Some(pairing::rotate_key_periodically(
                &conn,
                &keys_dir,
                crate::secret_store::store().as_ref(),
                &group_secret,
            ))
        } else {
            None
        }
    };
    if let Some(result) = periodic_rotation {
        match result {
            Ok(pending) => pairing::deliver_rotations(transport, pending, &group_secret).await,
            Err(e) => tracing::warn!(
                "sync: periodic rotation trigger fired but couldn't build the rotation: {e}"
            ),
        }
    }

    Ok(())
}

/// This device's own `device_id`, as carried by every `ChangeEnvelope`
/// variant's embedded `Hlc`.
fn envelope_device_id(change: &ChangeEnvelope) -> &str {
    match change {
        ChangeEnvelope::AppendMessage { hlc, .. }
        | ChangeEnvelope::ConversationSnapshot { hlc, .. }
        | ChangeEnvelope::ConversationTitle { hlc, .. }
        | ChangeEnvelope::ConversationProject { hlc, .. }
        | ChangeEnvelope::ConversationUnread { hlc, .. }
        | ChangeEnvelope::ConversationPinned { hlc, .. }
        | ChangeEnvelope::ConversationDeleted { hlc, .. }
        | ChangeEnvelope::ProjectRow { hlc, .. }
        | ChangeEnvelope::ProjectDeleted { hlc, .. }
        | ChangeEnvelope::ArtifactRow { hlc, .. }
        | ChangeEnvelope::SettingsRow { hlc, .. }
        | ChangeEnvelope::CustomModelsRow { hlc, .. } => &hlc.device_id,
    }
}

/// Applies one already-decrypted, already-confirmed-not-self-echoed pulled
/// envelope. `AppendMessage` is the one exception: rather than applying it
/// immediately, this only ensures the target conversation exists (see
/// [`ensure_conversation_placeholder`]) and returns the `(conversation_id,
/// hlc, message)` triple for the caller to batch across the whole pull cycle
/// via `conversations::apply_remote_messages_batch`. Every other variant is
/// fully applied (or, on an LWW loss, written to a conflict file) before
/// returning `Ok(None)`.
fn route_pulled_envelope(
    state: &AppState,
    conflicts_dir: &Path,
    change: ChangeEnvelope,
) -> openpal_core::Result<Option<(String, Hlc, ChatMessage)>> {
    match change {
        ChangeEnvelope::AppendMessage {
            conversation_id,
            hlc,
            message,
        } => {
            // Resurrection guard: a deletion tombstoned this conversation
            // at-or-after this op's HLC — don't recreate it via the
            // placeholder step below. See `is_stale_against_deletion`'s
            // doc comment.
            if is_stale_against_deletion(state, &conversation_id, &hlc) {
                return Ok(None);
            }
            ensure_conversation_placeholder(state, &conversation_id)?;
            Ok(Some((conversation_id, hlc, message)))
        }
        ChangeEnvelope::ConversationSnapshot { hlc, conversation } => {
            // No placeholder needed — this applies the full conversation
            // itself via `crate::store::save`, even for a conversation this
            // device has never seen. A snapshot rewrite is itself a
            // stronger "does this conversation exist" signal than a plain
            // append/title/project edit, so it isn't guarded by
            // `is_stale_against_deletion` — `field_versions` scope
            // `"conversation_snapshot"` already gives it its own
            // independent LWW comparison via `apply_remote_snapshot`.
            let mut conn = state.app_db.lock().unwrap();
            conversations::apply_remote_snapshot(&mut conn, conflicts_dir, hlc, conversation)?;
            Ok(None)
        }
        ChangeEnvelope::ConversationTitle {
            conversation_id,
            hlc,
            title,
        } => {
            if is_stale_against_deletion(state, &conversation_id, &hlc) {
                return Ok(None);
            }
            ensure_conversation_placeholder(state, &conversation_id)?;
            let conn = state.app_db.lock().unwrap();
            conversations::apply_remote_title(&conn, &conversation_id, hlc, title)?;
            Ok(None)
        }
        ChangeEnvelope::ConversationProject {
            conversation_id,
            hlc,
            project_id,
        } => {
            if is_stale_against_deletion(state, &conversation_id, &hlc) {
                return Ok(None);
            }
            ensure_conversation_placeholder(state, &conversation_id)?;
            let conn = state.app_db.lock().unwrap();
            conversations::apply_remote_project(&conn, &conversation_id, hlc, project_id)?;
            Ok(None)
        }
        ChangeEnvelope::ConversationUnread {
            conversation_id,
            hlc,
            unread,
        } => {
            if is_stale_against_deletion(state, &conversation_id, &hlc) {
                return Ok(None);
            }
            ensure_conversation_placeholder(state, &conversation_id)?;
            let conn = state.app_db.lock().unwrap();
            conversations::apply_remote_unread(&conn, &conversation_id, hlc, unread)?;
            Ok(None)
        }
        ChangeEnvelope::ConversationPinned {
            conversation_id,
            hlc,
            pinned,
        } => {
            if is_stale_against_deletion(state, &conversation_id, &hlc) {
                return Ok(None);
            }
            ensure_conversation_placeholder(state, &conversation_id)?;
            let conn = state.app_db.lock().unwrap();
            conversations::apply_remote_pinned(&conn, &conversation_id, hlc, pinned)?;
            Ok(None)
        }
        ChangeEnvelope::ConversationDeleted {
            conversation_id,
            hlc,
        } => {
            let mut conn = state.app_db.lock().unwrap();
            capture::apply_remote_conversation_deleted(
                &mut conn,
                hlc,
                &conversation_id,
                conflicts_dir,
            )?;
            Ok(None)
        }
        // The remaining five variants each have a ready-made
        // `capture::apply_remote_*` wrapper that already does the
        // `field_versions::try_apply_remote` win/lose compare, the winning
        // apply via the target table's own `save`/`delete`, and the losing
        // conflict-file write — see `capture.rs`'s own doc comment ("Mirrors
        // `sync::conversations::apply_remote_*`'s shape for the other four
        // synced tables... Called by `crates/app/src/sync_loop.rs`"). Delegate
        // rather than re-deriving the same LWW routing here.
        ChangeEnvelope::ProjectRow { hlc, project } => {
            let conn = state.app_db.lock().unwrap();
            capture::apply_remote_project_row(&conn, hlc, project, conflicts_dir)?;
            Ok(None)
        }
        ChangeEnvelope::ProjectDeleted { row_key, hlc } => {
            let conn = state.app_db.lock().unwrap();
            capture::apply_remote_project_deleted(&conn, hlc, &row_key, conflicts_dir)?;
            Ok(None)
        }
        ChangeEnvelope::ArtifactRow {
            hlc,
            conversation_id,
            artifact_id,
            filename,
            content,
        } => {
            let conn = state.app_db.lock().unwrap();
            capture::apply_remote_artifact_row(
                &conn,
                hlc,
                &conversation_id,
                &artifact_id,
                &filename,
                &content,
                conflicts_dir,
            )?;
            Ok(None)
        }
        ChangeEnvelope::SettingsRow { hlc, settings_json } => {
            let conn = state.app_db.lock().unwrap();
            capture::apply_remote_settings_row(&conn, hlc, &settings_json, conflicts_dir)?;
            Ok(None)
        }
        ChangeEnvelope::CustomModelsRow { hlc, models } => {
            let conn = state.app_db.lock().unwrap();
            capture::apply_remote_custom_models_row(&conn, hlc, models, conflicts_dir)?;
            Ok(None)
        }
    }
}

/// True if `conversation_id` has a recorded deletion tombstone
/// (`field_versions` scope `"conversation_deleted"`, see
/// `capture::delete_conversation_synced`/`apply_remote_conversation_deleted`)
/// whose `Hlc` is not causally before `hlc` — i.e. the pulled op is stale
/// relative to a deletion this device already knows about and must not
/// resurrect the conversation via [`ensure_conversation_placeholder`].
/// Mirrors this project's established "compare HLCs, newest state wins"
/// rule (same one `field_versions::try_apply_remote` already applies to
/// scalar fields), applied here to "does this conversation exist at all."
/// A genuinely later op (HLC after the deletion) is NOT considered stale —
/// same LWW answer this codebase already gives for a rewrite superseding
/// an earlier snapshot, applied consistently to deletion too.
fn is_stale_against_deletion(state: &AppState, conversation_id: &str, hlc: &Hlc) -> bool {
    let conn = state.app_db.lock().unwrap();
    match field_versions::get(&conn, "conversation_deleted", conversation_id) {
        Some(deleted_at) => *hlc <= deleted_at,
        None => false,
    }
}

/// `apply_remote_message`/`apply_remote_title`/`apply_remote_project` all
/// call `crate::store::get` internally and error if the conversation
/// doesn't exist yet — true the first time this device ever sees an
/// envelope for a conversation another device created. Creates a minimal
/// placeholder row (empty title, no messages, default sampling/context
/// length/task type) first, so the real apply function has something to
/// update. A no-op if the conversation already exists.
///
/// `apply_remote_snapshot` doesn't need this — it saves the full
/// conversation from the envelope directly.
fn ensure_conversation_placeholder(
    state: &AppState,
    conversation_id: &str,
) -> openpal_core::Result<()> {
    let conn = state.app_db.lock().unwrap();
    if openpal_core::store::get(&conn, conversation_id).is_ok() {
        return Ok(());
    }
    let now = now_ms();
    let placeholder = Conversation {
        id: conversation_id.to_string(),
        title: String::new(),
        created_at_ms: now,
        updated_at_ms: now,
        messages: Vec::new(),
        sampling: SamplingParams::default(),
        context_length: 8192,
        task_type: TaskType::default(),
        project_id: None,
        pinned: false,
        branches: std::collections::HashMap::new(),
        forked_from: None,
    };
    openpal_core::store::save(&conn, &placeholder)
}

fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
