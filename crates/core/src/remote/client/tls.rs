//! TLS trust model for [`super::RemoteBackend`]'s connection to a paired
//! desktop's gateway (`crate::remote::gateway`) — mirrors that gateway's
//! own posture: its self-signed certificate's Subject Alternative Name is
//! hardcoded to `"localhost"` (`crate::remote::generate_self_signed_cert`),
//! so no real LAN-IP connection can ever pass standard hostname
//! verification against it, no matter how the trust anchor is configured.
//! Fingerprint pinning — skip hostname/chain checks, verify the presented
//! leaf certificate's SHA-256 DER hash against a known-good value — isn't
//! just the simpler option here, it's the only one that works; see
//! `crate::remote::PairingPayload::fingerprint`'s own doc comment.
//!
//! Two verifiers for two different moments:
//! - [`CapturingVerifier`] accepts *any* certificate, once, during the
//!   single `/remote/v1/pair` request, and records its fingerprint. This is
//!   the actual trust-on-first-use step for this (QR-less, v1) pairing
//!   flow — a real, disclosed weakening relative to the desktop-to-desktop
//!   QR flow, whose fingerprint travels inside the scanned payload itself,
//!   verified before the first byte is ever sent over the wire. Here, a
//!   LAN/overlay-network attacker active during that one pairing request
//!   could intercept the code. Acceptable for this feature's threat model
//!   (see `crate::remote` module docs and `docs/REMOTE_INFERENCE.md`), but
//!   real — not silently equivalent to QR-strength pinning.
//! - [`PinnedVerifier`] accepts only a certificate matching a known-good
//!   fingerprint — used for every request after pairing succeeds, once the
//!   fingerprint captured above is persisted and trusted.
//!
//! Both verifiers still cryptographically verify the handshake signature
//! (`rustls::crypto::verify_tls12_signature`/`verify_tls13_signature`,
//! against the same `ring` algorithms `gateway.rs` already installs
//! process-wide) — only the "is this cert trusted for this hostname" check
//! is replaced. Skipping signature verification too would let anyone
//! answer the handshake with an arbitrary certificate and no proof they
//! hold its private key; matching a fingerprint alone doesn't protect
//! against that if the signature itself is never checked.

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{verify_tls12_signature, verify_tls13_signature, CryptoProvider};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, Error as TlsError, SignatureScheme};
use std::sync::{Arc, Mutex};

fn ring_provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// Trust-on-first-use verifier: accepts any certificate and records its
/// fingerprint for the caller to read back afterward. Only ever used for
/// the single, short-lived pairing request — never reused for an
/// already-paired connection.
#[derive(Debug)]
pub struct CapturingVerifier {
    captured: Mutex<Option<String>>,
    provider: Arc<CryptoProvider>,
}

impl CapturingVerifier {
    fn new(provider: Arc<CryptoProvider>) -> Self {
        Self {
            captured: Mutex::new(None),
            provider,
        }
    }

    /// The fingerprint of the certificate presented during the handshake,
    /// once one has completed. `None` before that, or if the connection
    /// never got that far.
    pub fn captured_fingerprint(&self) -> Option<String> {
        self.captured.lock().unwrap().clone()
    }
}

impl ServerCertVerifier for CapturingVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, TlsError> {
        *self.captured.lock().unwrap() = Some(super::super::fingerprint_hex(end_entity));
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Post-pairing verifier: accepts only a certificate whose SHA-256 DER
/// fingerprint (lowercase hex, matching `crate::remote::fingerprint_hex`'s
/// format) equals a known-good value.
#[derive(Debug)]
pub struct PinnedVerifier {
    fingerprint_hex: String,
    provider: Arc<CryptoProvider>,
}

impl PinnedVerifier {
    fn new(fingerprint_hex: String, provider: Arc<CryptoProvider>) -> Self {
        Self {
            fingerprint_hex: fingerprint_hex.to_lowercase(),
            provider,
        }
    }
}

impl ServerCertVerifier for PinnedVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, TlsError> {
        if super::super::fingerprint_hex(end_entity) == self.fingerprint_hex {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(TlsError::General(
                "the presented certificate doesn't match the paired device's known fingerprint"
                    .to_string(),
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

fn build_client_config(
    verifier: Arc<dyn ServerCertVerifier>,
    provider: Arc<CryptoProvider>,
) -> Result<ClientConfig, String> {
    let versions = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| format!("couldn't configure TLS: {e}"))?;
    Ok(versions
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth())
}

/// A client that trusts nothing yet — for the single `/remote/v1/pair`
/// request only. The returned [`CapturingVerifier`] handle lets the caller
/// read back the fingerprint that connection actually presented once the
/// gateway has accepted the pairing code, so it can be pinned from then on.
pub fn tofu_client() -> Result<(reqwest::Client, Arc<CapturingVerifier>), String> {
    let provider = ring_provider();
    let verifier = Arc::new(CapturingVerifier::new(provider.clone()));
    let config = build_client_config(verifier.clone(), provider)?;
    let client = reqwest::Client::builder()
        .use_preconfigured_tls(config)
        .build()
        .map_err(|e| format!("couldn't build the pairing client: {e}"))?;
    Ok((client, verifier))
}

/// A client pinned to `fingerprint_hex` — for every request to an
/// already-paired gateway.
pub fn pinned_client(fingerprint_hex: &str) -> Result<reqwest::Client, String> {
    let provider = ring_provider();
    let verifier: Arc<dyn ServerCertVerifier> = Arc::new(PinnedVerifier::new(
        fingerprint_hex.to_string(),
        provider.clone(),
    ));
    let config = build_client_config(verifier, provider)?;
    reqwest::Client::builder()
        .use_preconfigured_tls(config)
        .build()
        .map_err(|e| format!("couldn't build the client: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::fingerprint_hex as server_fingerprint_hex;

    /// End to end through a real TLS-bound socket, reusing the gateway's
    /// own cert-generation helper via `crate::remote`'s test-only re-export
    /// path — a pinned client must accept the right fingerprint and reject
    /// every wrong one, and a TOFU client must capture exactly what the
    /// server presented.
    async fn bind_test_tls_server(
        cert_der: Vec<u8>,
        key_der: Vec<u8>,
    ) -> (u16, tokio::task::JoinHandle<()>) {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let config = axum_server::tls_rustls::RustlsConfig::from_der(vec![cert_der], key_der)
            .await
            .unwrap();
        let app = axum::Router::new().route("/", axum::routing::get(|| async { "ok" }));
        // Release the probe listener before the real server binds the same
        // port — held alive any longer and it races `bind_rustls` for the
        // socket (matching `gateway::free_port`'s own release-before-use
        // contract).
        let port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap().port()
        };
        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
        let handle = axum_server::Handle::new();
        let serve_handle = handle.clone();
        let join = tokio::spawn(async move {
            let _ = axum_server::bind_rustls(addr, config)
                .handle(serve_handle)
                .serve(app.into_make_service())
                .await;
        });
        assert!(
            handle.listening().await.is_some(),
            "test TLS server failed to bind"
        );
        (port, join)
    }

    fn self_signed() -> (Vec<u8>, Vec<u8>, String) {
        let rcgen::CertifiedKey { cert, signing_key } =
            rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
        let cert_der = cert.der().to_vec();
        let key_der = signing_key.serialize_der();
        let fingerprint = server_fingerprint_hex(&cert_der);
        (cert_der, key_der, fingerprint)
    }

    #[tokio::test]
    async fn pinned_client_accepts_the_matching_fingerprint() {
        let (cert_der, key_der, fingerprint) = self_signed();
        let (port, _server) = bind_test_tls_server(cert_der, key_der).await;

        let client = pinned_client(&fingerprint).unwrap();
        let response = client
            .get(format!("https://127.0.0.1:{port}/"))
            .send()
            .await
            .unwrap();
        assert!(response.status().is_success());
    }

    #[tokio::test]
    async fn pinned_client_rejects_a_wrong_fingerprint() {
        let (cert_der, key_der, _fingerprint) = self_signed();
        let (port, _server) = bind_test_tls_server(cert_der, key_der).await;

        let wrong = "0".repeat(64);
        let client = pinned_client(&wrong).unwrap();
        let result = client
            .get(format!("https://127.0.0.1:{port}/"))
            .send()
            .await;
        assert!(
            result.is_err(),
            "a wrong fingerprint must fail the handshake"
        );
    }

    #[tokio::test]
    async fn tofu_client_captures_the_real_fingerprint() {
        let (cert_der, key_der, fingerprint) = self_signed();
        let (port, _server) = bind_test_tls_server(cert_der, key_der).await;

        let (client, verifier) = tofu_client().unwrap();
        assert!(verifier.captured_fingerprint().is_none());
        let response = client
            .get(format!("https://127.0.0.1:{port}/"))
            .send()
            .await
            .unwrap();
        assert!(response.status().is_success());
        assert_eq!(verifier.captured_fingerprint(), Some(fingerprint));
    }
}
