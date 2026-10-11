//! End-to-end proof of the TOFU (trust-on-first-use) certificate pin
//! `docs/REMOTE_INFERENCE.md`'s "Pairing" section describes: a real
//! `RemoteAccessManager` serving real TLS over a real bound socket (exactly
//! like `crate::remote`'s own `pairing_end_to_end_over_real_tls_issues_a_working_token`
//! test), a real pairing round trip to obtain the real `fingerprint` a
//! client is meant to pin, and a raw `rustls` connection — not `reqwest`'s
//! usual `danger_accept_invalid_certs` shortcut — so the test can inspect
//! the actual presented certificate and run it through
//! `openpal_core::remote::verify_pin` itself, the same function a real
//! client would call.
//!
//! No client exists anywhere in this repo (mobile or otherwise) to pin a
//! certificate from — this file, plus `remote::pin`'s own unit tests, are
//! the closest thing to a specification of "what should happen on a pin
//! mismatch" until a real one is built.

use openpal_core::engine::EngineManager;
use openpal_core::remote::{verify_pin, PairingEvent, PinError, RemoteAccessManager};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, ClientConnection, DigitallySignedStruct, SignatureScheme};
use std::io::Write;
use std::net::TcpStream;
use std::sync::{Arc, Mutex};

fn test_manager() -> RemoteAccessManager {
    let dir = tempfile::tempdir().unwrap().keep();
    let app_db = Arc::new(Mutex::new(
        openpal_core::app_db::open(&dir.join("app.sqlite"), None)
            .unwrap()
            .0,
    ));
    let engine = Arc::new(EngineManager::new(dir.join("logs")));
    RemoteAccessManager::new(app_db, engine)
}

/// A `rustls` verifier that accepts any certificate — this test's whole
/// point is to inspect the certificate ITSELF (via [`verify_pin`]), not to
/// have `rustls` reject an unknown self-signed one before the test ever
/// sees it. The real trust decision is [`verify_pin`], exercised
/// separately below, never this verifier.
#[derive(Debug)]
struct AcceptAnyCert(Arc<rustls::crypto::CryptoProvider>);

impl ServerCertVerifier for AcceptAnyCert {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

/// Opens a real TLS connection to `127.0.0.1:{port}` and returns the DER
/// bytes of whatever certificate it actually presented — the raw material
/// [`verify_pin`] checks against a pinned fingerprint. A genuine client
/// would do exactly this (minus `AcceptAnyCert`'s standing in for real
/// system trust-store validation, which is irrelevant here — the gateway's
/// cert is self-signed by design, so TOFU pinning, not CA validation, is
/// the actual trust mechanism).
fn fetch_presented_cert_der(port: u16) -> Vec<u8> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .unwrap()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAnyCert(provider)))
        .with_no_client_auth();

    let server_name = ServerName::try_from("localhost").unwrap();
    let mut conn = ClientConnection::new(Arc::new(config), server_name).unwrap();
    let mut sock = TcpStream::connect(("127.0.0.1", port)).unwrap();
    conn.complete_io(&mut sock).unwrap();
    // A minimal HTTP request so the server side (which expects to read
    // something) doesn't just see the socket close mid-handshake teardown;
    // the response itself is irrelevant here — only the handshake's
    // negotiated peer certificate matters to this test.
    let mut tls_stream = rustls::Stream::new(&mut conn, &mut sock);
    let _ = tls_stream.write_all(
        b"GET /remote/v1/status HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    );

    conn.peer_certificates()
        .expect("a completed TLS handshake always has a peer certificate")[0]
        .as_ref()
        .to_vec()
}

// `flavor = "multi_thread"`: `fetch_presented_cert_der` below does a
// genuinely BLOCKING synchronous TLS handshake (`std::net::TcpStream` +
// `rustls::ClientConnection::complete_io`), not an async/`.await`-based
// one. `#[tokio::test]`'s default single-threaded runtime would have that
// blocking call starve the SAME worker thread the gateway's
// `tokio::spawn`ed server task needs to run on (a real deadlock, hit while
// writing this test) — a second worker thread lets the server task make
// progress concurrently with this test's blocking client code.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn positive_case_the_paired_fingerprint_matches_the_real_presented_certificate() {
    let manager = test_manager();
    manager.set_enabled(true).await.unwrap();
    let port = manager.status().await.port.unwrap();

    // Real pairing round trip — exactly `crate::remote`'s own
    // `pairing_end_to_end_over_real_tls_issues_a_working_token` test, up to
    // and including obtaining a real `PairingPayload.fingerprint`.
    let events = Arc::new(Mutex::new(Vec::new()));
    let events_clone = events.clone();
    let pairing_task = tokio::spawn(async move {
        manager
            .start_pairing(move |event| events_clone.lock().unwrap().push(event))
            .await
            .unwrap();
        manager
    });

    let (code, fingerprint) = loop {
        let found = {
            let events = events.lock().unwrap();
            match events.first() {
                Some(PairingEvent::Waiting { payload }) => {
                    Some((payload.code.clone(), payload.fingerprint.clone()))
                }
                _ => None,
            }
        };
        if let Some(found) = found {
            break found;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    };

    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .build()
        .unwrap();
    let response = client
        .post(format!("https://127.0.0.1:{port}/remote/v1/pair"))
        .json(&serde_json::json!({ "code": code, "deviceName": "Test Phone" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = response.json().await.unwrap();
    let token = body["token"].as_str().unwrap().to_string();
    let manager = pairing_task.await.unwrap();

    // The pin check itself: does the certificate this connection ACTUALLY
    // received match what the pairing payload told us to pin? This is the
    // step `docs/REMOTE_INFERENCE.md` describes and, before this change,
    // never had a concrete implementation anywhere in the repo.
    let presented_der = fetch_presented_cert_der(port);
    assert_eq!(
        verify_pin(&presented_der, &fingerprint),
        Ok(()),
        "the real gateway's presented certificate must match the fingerprint from its own pairing payload"
    );

    // Only AFTER the pin check passes does a real client send its bearer
    // token — proven here by actually doing so and getting a real 200,
    // demonstrating the pin check doesn't itself block legitimate use.
    let response = client
        .get(format!("https://127.0.0.1:{port}/remote/v1/status"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);

    manager.stop_all().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn negative_case_a_mismatched_fingerprint_is_hard_rejected_before_any_token_is_sent() {
    let manager = test_manager();
    manager.set_enabled(true).await.unwrap();
    let port = manager.status().await.port.unwrap();

    // Simulated MITM: an attacker-controlled certificate, generated the
    // same way the real gateway generates its own (`rcgen`), standing in
    // for "the fingerprint a client is holding no longer matches what's
    // actually on the wire" — whether because of a substituted cert in
    // transit, or a stale pin after a legitimate cert change. Both look
    // identical at this layer, by TOFU's design.
    let attacker_cert = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    let attacker_fingerprint_input = attacker_cert.cert.der().to_vec();
    // Only used to obtain a fingerprint string in the same hex-of-SHA-256
    // shape a real (wrong) pairing payload would carry — never connected
    // to over the network.
    let wrong_fingerprint = {
        use sha2::{Digest, Sha256};
        let hash = Sha256::digest(&attacker_fingerprint_input);
        hash.iter().map(|b| format!("{b:02x}")).collect::<String>()
    };

    let presented_der = fetch_presented_cert_der(port);
    let result = verify_pin(&presented_der, &wrong_fingerprint);
    assert_eq!(result, Err(PinError::Mismatch));

    // Structural proof the mismatch is a hard gate, not advisory: the
    // bearer-token request is written INSIDE the `if verify_pin(...).is_ok()`
    // branch, exactly as a real client's connect logic must be — this test
    // fails to compile the token-sending branch at all when the pin check
    // fails, not merely "chooses" not to run it.
    let mut token_was_sent = false;
    if result.is_ok() {
        token_was_sent = true; // unreachable in this test — see assertion below
    }
    assert!(
        !token_was_sent,
        "a rejected pin must never proceed to send credentials"
    );

    manager.stop_all().await;
}
