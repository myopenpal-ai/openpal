//! Parental-controls enforcement (`enforce_send`/`check_send`, called from
//! `commands::chat_stream` and `commands::run_one_shot_completion`) and the
//! background usage-time accrual loop (`run`, spawned once for the app's
//! lifetime from `lib.rs::run()`).
//!
//! Mirrors `idle.rs`'s shape (a coarse ticker reading `AppState` each tick)
//! but for the opposite purpose: instead of unloading after inactivity,
//! this accrues "the app is open" time toward the daily usage cap. That's
//! deliberately not idle-adjusted — "usage" here means screen time, the
//! same predictable definition a beginner (or a kid) would expect, not
//! "actively typing".

use crate::state::AppState;
use openpal_core::types::{LockReason, ParentalConfig};
use std::time::Duration;
use tauri::{AppHandle, Manager};

/// How often the ticker adds to today's usage-seconds counter while the
/// usage cap is on. Coarser than idle.rs's 5s check interval — usage-cap
/// enforcement only needs to notice within roughly this margin, not to the
/// second.
const TICK: Duration = Duration::from_secs(30);

pub async fn run(app: AppHandle) {
    let mut ticker = tokio::time::interval(TICK);
    loop {
        ticker.tick().await;
        let Some(state) = app.try_state::<AppState>() else {
            continue;
        };
        accrue_usage(&state).await;
    }
}

async fn accrue_usage(state: &AppState) {
    let config = state.parental_config.lock().await.clone();
    if !config.enabled || !config.usage_cap_enabled {
        return; // nothing to accrue while the feature (or just the usage cap) is off
    }
    let (today, _) = openpal_core::parental::now_local_parts();
    let mut ps = state.parental_state.lock().await;
    openpal_core::parental::roll_over(&mut ps, &today);
    ps.usage_seconds += TICK.as_secs();
    if let Err(e) = openpal_core::parental::save_state(&state.parental_state_path, &ps) {
        tracing::warn!("failed to save parental usage state: {e}");
    }
}

/// Called at the top of `commands::chat_stream` and by every Writing Tools
/// one-shot completion (`translate_stream`/`correct_stream`, via
/// `run_one_shot_completion`'s `ParentalGate::CountMessage`): refuse (with a
/// plain-language explanation) if parental controls currently block
/// sending, else count this message toward today's cap and let the turn
/// proceed. Backend-authoritative on purpose — the composer's own lock
/// banner is the primary UX (see `ui/src/views/ChatView.tsx`), this is the
/// backstop that makes it non-optional. Behaviour is unchanged from before
/// `gate`/`check_send` existed — see `gate`'s doc for why.
pub async fn enforce_send(state: &AppState) -> Result<(), String> {
    gate(state, true).await
}

/// Like [`enforce_send`], but never increments the daily message count —
/// only the lock check applies. Used by the Message improver
/// (`commands::enhance_prompt`, via `run_one_shot_completion`'s
/// `ParentalGate::CheckOnly`) when parental controls are off, so an
/// improvement — which is always followed by a real, counted send — never
/// gets double-charged against a cap nobody set. When parental controls
/// *are* on, the improver instead passes `ParentalGate::CountMessage` (i.e.
/// calls [`enforce_send`]) so a capped child can't use it as an unlimited,
/// friction-free channel to the model once the visible chat cap is reached
/// — see `commands::enhance_prompt`.
pub async fn check_send(state: &AppState) -> Result<(), String> {
    gate(state, false).await
}

/// Shared body of [`enforce_send`]/[`check_send`]: loads config/state,
/// defers the actual lock-check-and-maybe-count decision to the pure core
/// helper `openpal_core::parental::gate_send` (so that logic is unit-tested
/// without needing an `AppState`), then persists the result. Mirrors the
/// original `enforce_send`'s short-circuit exactly when parental controls
/// are off: `gate_send` leaves `state` untouched in that case, so this
/// skips the disk write entirely, just as the pre-refactor code did by
/// returning before ever locking `parental_state`.
async fn gate(state: &AppState, count: bool) -> Result<(), String> {
    let config = state.parental_config.lock().await.clone();
    let (today, minute) = openpal_core::parental::now_local_parts();
    let mut ps = state.parental_state.lock().await;
    openpal_core::parental::gate_send(&config, &mut ps, &today, minute, count)
        .map_err(|reason| lock_message(reason, &config))?;
    if config.enabled {
        openpal_core::parental::save_state(&state.parental_state_path, &ps).map_err(|e| {
            tracing::warn!("failed to save parental message count: {e}");
            "Something went wrong checking parental controls. Try again.".to_string()
        })?;
    }
    Ok(())
}

fn lock_message(reason: LockReason, config: &ParentalConfig) -> String {
    match reason {
        LockReason::Schedule => format!(
            "Parental controls: the AI is only available between {} and {}.",
            config.allowed_start, config.allowed_end
        ),
        LockReason::UsageCap => {
            "Parental controls: today's usage time is up. Try again tomorrow.".to_string()
        }
        LockReason::MessageCap => {
            "Parental controls: today's message limit is reached. Try again tomorrow.".to_string()
        }
    }
}
