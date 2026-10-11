//! `ConsentBroker`: pending per-call approval plus the persisted "Always
//! allow" grant store. One mutex guards both so a Settings-page revoke can
//! never race an in-flight orchestrator's save and silently lose either
//! side.
//!
//! Also brokers a second, unrelated kind of pending round-trip: `ask_user`'s
//! clarifying-question answers (see `register_clarification`/
//! `resolve_clarification` below). It lives here rather than in its own
//! struct because it's the same shape (a oneshot per in-flight `call_id`,
//! drained by `cancel_all` when a turn stops) and every place an `AgentDeps`
//! is built already carries a `ConsentBroker` — a second broker type would
//! mean threading a second `Arc` field through every one of those call sites
//! for no real benefit.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tokio::sync::{oneshot, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ConsentDecision {
    AllowOnce,
    AlwaysAllow,
    Deny,
}

/// A persisted "Always allow" grant.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolGrant {
    pub grant_key: String,
    pub tool_id: String,
    /// Human-readable summary shown in the Settings "standing permissions"
    /// list — built from the resolved target, not the model's raw argument.
    pub summary: String,
    pub created_at_ms: u64,
    /// Absolute-path fs grants outside the workspace default to a 30-day
    /// expiry; `run_shell`/`run_code`/workspace-relative fs grants are
    /// already narrow (exact command/code text, or a workspace-relative
    /// path) and stay non-expiring.
    #[serde(default)]
    pub expires_at_ms: Option<u64>,
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

struct Pending {
    sender: oneshot::Sender<(ConsentDecision, Option<String>)>,
}

struct State {
    pending: HashMap<String, Pending>,
    grants: HashMap<String, ToolGrant>,
    /// Pending `ask_user` answers, keyed by the tool call's own `call_id`
    /// (mirrors `pending`'s keying by `request_id` — a clarifying question
    /// has no separate request/decision split, so one id does both jobs).
    clarifications: HashMap<String, oneshot::Sender<String>>,
}

pub struct ConsentBroker {
    state: Mutex<State>,
    grants_path: PathBuf,
}

impl ConsentBroker {
    pub fn new(grants_path: PathBuf) -> Self {
        let grants = load_grants(&grants_path);
        Self {
            state: Mutex::new(State {
                pending: HashMap::new(),
                grants,
                clarifications: HashMap::new(),
            }),
            grants_path,
        }
    }

    /// Register a pending request *before* the corresponding
    /// `ToolConsentRequested` event is emitted, so a response can never
    /// arrive before there's something to resolve it against.
    pub async fn register(
        &self,
        request_id: &str,
    ) -> oneshot::Receiver<(ConsentDecision, Option<String>)> {
        let (tx, rx) = oneshot::channel();
        let mut state = self.state.lock().await;
        state
            .pending
            .insert(request_id.to_string(), Pending { sender: tx });
        rx
    }

    /// Resolve a pending request (the `respond_tool_consent` command).
    /// `false` if nothing was pending with this id (already resolved, or
    /// stale — e.g. the turn ended first).
    pub async fn resolve(
        &self,
        request_id: &str,
        decision: ConsentDecision,
        note: Option<String>,
    ) -> bool {
        let mut state = self.state.lock().await;
        match state.pending.remove(request_id) {
            Some(pending) => {
                let _ = pending.sender.send((decision, note));
                true
            }
            None => false,
        }
    }

    /// Resolve every still-pending request as a denial (the turn was
    /// stopped/ended) so no orchestrator await hangs forever. Also resolves
    /// every pending clarifying question with `note` as its answer, for the
    /// same reason.
    pub async fn cancel_all(&self, note: &str) {
        let mut state = self.state.lock().await;
        for (_, pending) in state.pending.drain() {
            let _ = pending
                .sender
                .send((ConsentDecision::Deny, Some(note.to_string())));
        }
        for (_, sender) in state.clarifications.drain() {
            let _ = sender.send(note.to_string());
        }
    }

    /// Register a pending `ask_user` answer *before* the corresponding
    /// `ChatEvent::ClarifyingQuestionAsked` is emitted — same ordering
    /// requirement as [`Self::register`], for the same reason.
    pub async fn register_clarification(&self, call_id: &str) -> oneshot::Receiver<String> {
        let (tx, rx) = oneshot::channel();
        let mut state = self.state.lock().await;
        state.clarifications.insert(call_id.to_string(), tx);
        rx
    }

    /// Resolve a pending clarifying question (the `respond_clarifying_question`
    /// command). `false` if nothing was pending with this id.
    pub async fn resolve_clarification(&self, call_id: &str, answer: String) -> bool {
        let mut state = self.state.lock().await;
        match state.clarifications.remove(call_id) {
            Some(sender) => {
                let _ = sender.send(answer);
                true
            }
            None => false,
        }
    }

    pub async fn find_grant(&self, grant_key: &str) -> Option<ToolGrant> {
        let mut state = self.state.lock().await;
        let expired = matches!(
            state.grants.get(grant_key),
            Some(g) if g.expires_at_ms.is_some_and(|exp| now_ms() >= exp)
        );
        if expired {
            state.grants.remove(grant_key);
            save_grants(&self.grants_path, &state.grants);
            return None;
        }
        state.grants.get(grant_key).cloned()
    }

    pub async fn save_grant(&self, grant: ToolGrant) {
        let mut state = self.state.lock().await;
        state.grants.insert(grant.grant_key.clone(), grant);
        save_grants(&self.grants_path, &state.grants);
    }

    pub async fn revoke_grant(&self, grant_key: &str) {
        let mut state = self.state.lock().await;
        state.grants.remove(grant_key);
        save_grants(&self.grants_path, &state.grants);
    }

    pub async fn list_grants(&self) -> Vec<ToolGrant> {
        let state = self.state.lock().await;
        let mut list: Vec<ToolGrant> = state.grants.values().cloned().collect();
        list.sort_by_key(|g| g.created_at_ms);
        list
    }
}

fn load_grants(path: &Path) -> HashMap<String, ToolGrant> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_grants(path: &Path, grants: &HashMap<String, ToolGrant>) {
    let Ok(json) = serde_json::to_string_pretty(grants) else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, json).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grant(key: &str) -> ToolGrant {
        ToolGrant {
            grant_key: key.to_string(),
            tool_id: "run_shell".to_string(),
            summary: "test".to_string(),
            created_at_ms: now_ms(),
            expires_at_ms: None,
        }
    }

    #[tokio::test]
    async fn save_then_find_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let broker = ConsentBroker::new(tmp.path().join("grants.json"));
        broker.save_grant(grant("k1")).await;
        assert!(broker.find_grant("k1").await.is_some());
        assert!(broker.find_grant("nope").await.is_none());
    }

    #[tokio::test]
    async fn revoke_removes_a_grant() {
        let tmp = tempfile::tempdir().unwrap();
        let broker = ConsentBroker::new(tmp.path().join("grants.json"));
        broker.save_grant(grant("k1")).await;
        broker.revoke_grant("k1").await;
        assert!(broker.find_grant("k1").await.is_none());
    }

    #[tokio::test]
    async fn grants_persist_across_broker_instances() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("grants.json");
        ConsentBroker::new(path.clone())
            .save_grant(grant("k1"))
            .await;
        let reloaded = ConsentBroker::new(path);
        assert!(reloaded.find_grant("k1").await.is_some());
    }

    #[tokio::test]
    async fn expired_grants_are_treated_as_a_miss_and_removed() {
        let tmp = tempfile::tempdir().unwrap();
        let broker = ConsentBroker::new(tmp.path().join("grants.json"));
        let mut g = grant("k1");
        g.expires_at_ms = Some(now_ms().saturating_sub(1000));
        broker.save_grant(g).await;
        assert!(broker.find_grant("k1").await.is_none());
        assert!(broker.list_grants().await.is_empty());
    }

    #[tokio::test]
    async fn register_then_resolve_delivers_the_decision() {
        let tmp = tempfile::tempdir().unwrap();
        let broker = ConsentBroker::new(tmp.path().join("grants.json"));
        let rx = broker.register("req1").await;
        assert!(
            broker
                .resolve("req1", ConsentDecision::AllowOnce, None)
                .await
        );
        let (decision, note) = rx.await.unwrap();
        assert_eq!(decision, ConsentDecision::AllowOnce);
        assert_eq!(note, None);
    }

    #[tokio::test]
    async fn resolving_an_unknown_request_id_returns_false() {
        let tmp = tempfile::tempdir().unwrap();
        let broker = ConsentBroker::new(tmp.path().join("grants.json"));
        assert!(!broker.resolve("nope", ConsentDecision::Deny, None).await);
    }

    #[tokio::test]
    async fn cancel_all_resolves_every_pending_request_as_denied() {
        let tmp = tempfile::tempdir().unwrap();
        let broker = ConsentBroker::new(tmp.path().join("grants.json"));
        let rx1 = broker.register("req1").await;
        let rx2 = broker.register("req2").await;
        broker.cancel_all("stopped").await;
        assert_eq!(rx1.await.unwrap().0, ConsentDecision::Deny);
        assert_eq!(rx2.await.unwrap().0, ConsentDecision::Deny);
    }

    #[tokio::test]
    async fn register_then_resolve_clarification_delivers_the_answer() {
        let tmp = tempfile::tempdir().unwrap();
        let broker = ConsentBroker::new(tmp.path().join("grants.json"));
        let rx = broker.register_clarification("call1").await;
        assert!(
            broker
                .resolve_clarification("call1", "Paris".to_string())
                .await
        );
        assert_eq!(rx.await.unwrap(), "Paris");
    }

    #[tokio::test]
    async fn resolving_an_unknown_clarification_id_returns_false() {
        let tmp = tempfile::tempdir().unwrap();
        let broker = ConsentBroker::new(tmp.path().join("grants.json"));
        assert!(
            !broker
                .resolve_clarification("nope", "answer".to_string())
                .await
        );
    }

    #[tokio::test]
    async fn cancel_all_also_resolves_every_pending_clarification() {
        let tmp = tempfile::tempdir().unwrap();
        let broker = ConsentBroker::new(tmp.path().join("grants.json"));
        let rx = broker.register_clarification("call1").await;
        broker.cancel_all("The user stopped this turn.").await;
        assert_eq!(rx.await.unwrap(), "The user stopped this turn.");
        // Drained, not just resolved — a later resolve on the same id must
        // report "nothing pending", not silently overwrite.
        assert!(
            !broker
                .resolve_clarification("call1", "too late".to_string())
                .await
        );
    }

    #[tokio::test]
    async fn concurrent_saves_never_lose_a_write() {
        let tmp = tempfile::tempdir().unwrap();
        let broker = std::sync::Arc::new(ConsentBroker::new(tmp.path().join("grants.json")));
        let mut handles = Vec::new();
        for i in 0..20 {
            let broker = broker.clone();
            handles.push(tokio::spawn(async move {
                broker.save_grant(grant(&format!("k{i}"))).await;
            }));
        }
        for h in handles {
            h.await.unwrap();
        }
        assert_eq!(broker.list_grants().await.len(), 20);
    }
}
