//! Complete pairing with a desktop's Remote Access gateway: connect over
//! TLS trusting nothing yet ([`tls::CapturingVerifier`], via
//! [`tls::tofu_client`]), POST the user-typed code to `/remote/v1/pair`,
//! and — on success — return the fingerprint that connection actually
//! presented (now trustworthy, since the code only the desktop's own
//! screen showed just proved this is the right machine) alongside the
//! paired-device token. Mirrors `crate::remote::gateway`'s `pair_handler`
//! response shape and status-code mapping exactly.

use super::tls;
use serde::{Deserialize, Serialize};
use std::time::Duration;

const PAIR_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone)]
pub struct PairOutcome {
    pub device_id: String,
    pub device_name: String,
    pub token: String,
    /// The fingerprint the gateway's certificate actually presented during
    /// this handshake — the caller pins this for every request from now on.
    pub fingerprint_hex: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairError {
    /// No pairing session is open on the desktop right now — the user
    /// needs to press "Add a device" there first.
    NoSessionOpen,
    /// The session existed but has expired.
    Expired,
    /// The code didn't match; the session may still be open for a retry.
    WrongCode,
    /// Too many wrong attempts — the desktop invalidated the session.
    TooManyAttempts,
    /// The session was already used to complete pairing.
    AlreadyUsed,
    /// Couldn't reach the desktop at all (wrong address, desktop asleep or
    /// off, not on this network).
    Unreachable,
    /// Something else. The message is safe to show to a non-technical user.
    Other { message: String },
}

impl PairError {
    /// A message safe to show to a non-technical user.
    pub fn message(&self) -> String {
        match self {
            PairError::NoSessionOpen => {
                "No pairing session is open. On the desktop, press \"Add a device\" first."
                    .to_string()
            }
            PairError::Expired => {
                "That pairing session expired. Ask the desktop to start a new one.".to_string()
            }
            PairError::WrongCode => {
                "That code didn't match. Check the desktop's screen and try again.".to_string()
            }
            PairError::TooManyAttempts => {
                "Too many incorrect attempts. Ask to pair again from the desktop.".to_string()
            }
            PairError::AlreadyUsed => {
                "This pairing session was already used. Ask the desktop to start a new one."
                    .to_string()
            }
            PairError::Unreachable => "Couldn't reach that device. Check the address and that \
                                        both devices are on the same network."
                .to_string(),
            PairError::Other { message } => message.clone(),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PairRequestBody<'a> {
    code: &'a str,
    device_name: &'a str,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PairResponseBody {
    device_id: String,
    device_name: String,
    token: String,
}

/// Complete pairing against `addr:port`'s gateway. `device_name` identifies
/// this phone in the desktop's paired-device list.
pub async fn pair(
    addr: &str,
    port: u16,
    code: &str,
    device_name: &str,
) -> Result<PairOutcome, PairError> {
    let (client, verifier) = tls::tofu_client().map_err(|message| PairError::Other { message })?;
    let url = format!("https://{addr}:{port}/remote/v1/pair");
    let response = client
        .post(&url)
        .timeout(PAIR_REQUEST_TIMEOUT)
        .json(&PairRequestBody { code, device_name })
        .send()
        .await
        .map_err(|e| {
            tracing::warn!("remote pairing: couldn't reach {addr}:{port}: {e}");
            PairError::Unreachable
        })?;

    match response.status() {
        reqwest::StatusCode::OK => {}
        reqwest::StatusCode::NOT_FOUND => return Err(PairError::NoSessionOpen),
        reqwest::StatusCode::GONE => return Err(PairError::Expired),
        reqwest::StatusCode::CONFLICT => return Err(PairError::AlreadyUsed),
        reqwest::StatusCode::UNAUTHORIZED => {
            // The gateway has no distinct status code for "wrong code" vs.
            // "too many wrong attempts" (see `gateway::pair_handler`) — both
            // are 401, distinguished only by message text. Coupled to that
            // handler's exact wording on purpose; `gateway.rs`'s own tests
            // and this module's tests both pin the string.
            let body = response.text().await.unwrap_or_default();
            return Err(if body.contains("Too many") {
                PairError::TooManyAttempts
            } else {
                PairError::WrongCode
            });
        }
        status => {
            return Err(PairError::Other {
                message: format!("The device rejected pairing (status {status})."),
            })
        }
    }

    // The handshake completed and the gateway accepted the code: the
    // fingerprint captured during that one connection is now trustworthy.
    let fingerprint_hex = verifier
        .captured_fingerprint()
        .ok_or_else(|| PairError::Other {
            message: "Couldn't verify that device's security certificate.".to_string(),
        })?;

    let body: PairResponseBody = response.json().await.map_err(|e| {
        tracing::warn!("remote pairing: malformed response from {addr}:{port}: {e}");
        PairError::Other {
            message: "The device sent back something unexpected.".to_string(),
        }
    })?;

    Ok(PairOutcome {
        device_id: body.device_id,
        device_name: body.device_name,
        token: body.token,
        fingerprint_hex,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::EngineManager;
    use crate::remote::{PairingEvent, RemoteAccessManager};
    use std::sync::{Arc, Mutex};

    /// A real gateway (real TLS, real port) so these tests exercise the
    /// same handshake/pinning path production code does — matching
    /// `crate::remote::mod.rs`'s own `enabling_serves_real_tls_...` test
    /// style rather than driving the router in-process.
    fn test_manager() -> RemoteAccessManager {
        let dir = tempfile::tempdir().unwrap().keep();
        let app_db = Arc::new(Mutex::new(
            crate::app_db::open(&dir.join("app.sqlite"), None)
                .unwrap()
                .0,
        ));
        let engine = Arc::new(EngineManager::new(dir.join("logs")));
        RemoteAccessManager::new(app_db, engine)
    }

    async fn wait_for_code(events: &Arc<Mutex<Vec<PairingEvent>>>) -> String {
        loop {
            {
                let events = events.lock().unwrap();
                if let Some(PairingEvent::Waiting { payload }) = events.first() {
                    return payload.code.clone();
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    #[tokio::test]
    async fn no_open_session_is_reported_clearly() {
        let manager = test_manager();
        manager.set_enabled(true).await.unwrap();
        let port = manager.status().await.port.unwrap();

        let err = pair("127.0.0.1", port, "000000", "Test Phone")
            .await
            .unwrap_err();
        assert_eq!(err, PairError::NoSessionOpen);
    }

    #[tokio::test]
    async fn wrong_code_is_reported_clearly() {
        let manager = test_manager();
        manager.set_enabled(true).await.unwrap();
        let port = manager.status().await.port.unwrap();

        let events = Arc::new(Mutex::new(Vec::new()));
        let events_clone = events.clone();
        // Not awaited: the session stays open (bounded by
        // `PAIRING_SESSION_TTL`, 300s) after a wrong-but-under-cap guess,
        // and this test only needs the code, not the session's outcome.
        let _pairing_task = tokio::spawn(async move {
            let _ = manager
                .start_pairing(move |event| events_clone.lock().unwrap().push(event))
                .await;
        });
        wait_for_code(&events).await;

        let err = pair("127.0.0.1", port, "000000", "Test Phone")
            .await
            .unwrap_err();
        assert_eq!(err, PairError::WrongCode);
    }

    #[tokio::test]
    async fn correct_code_succeeds_and_captures_the_real_fingerprint() {
        let manager = test_manager();
        manager.set_enabled(true).await.unwrap();
        let port = manager.status().await.port.unwrap();

        let events = Arc::new(Mutex::new(Vec::new()));
        let events_clone = events.clone();
        let pairing_task = tokio::spawn(async move {
            manager
                .start_pairing(move |event| events_clone.lock().unwrap().push(event))
                .await
                .unwrap();
        });
        let code = wait_for_code(&events).await;

        let outcome = pair("127.0.0.1", port, &code, "Test Phone").await.unwrap();
        assert_eq!(outcome.device_name, "Test Phone");
        assert!(!outcome.token.is_empty());
        assert_eq!(outcome.fingerprint_hex.len(), 64, "a hex sha-256 digest");

        pairing_task.await.unwrap();
        assert!(matches!(
            events.lock().unwrap().get(1),
            Some(PairingEvent::Paired { device_name }) if device_name == "Test Phone"
        ));
    }

    #[tokio::test]
    async fn unreachable_host_is_reported_clearly() {
        // Nothing is listening on this port.
        let free_port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let err = pair("127.0.0.1", free_port, "000000", "Test Phone")
            .await
            .unwrap_err();
        assert_eq!(err, PairError::Unreachable);
    }
}
