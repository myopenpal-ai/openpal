//! Reference implementation of the TOFU (trust-on-first-use) certificate
//! pin check `docs/REMOTE_INFERENCE.md`'s "Pairing" section describes
//! (pin `fingerprint` from the pairing payload against the gateway's actual
//! presented TLS certificate) but never previously defined a concrete
//! mismatch behavior for. No client exists anywhere in this repo yet
//! (mobile or otherwise) to implement it, so this module — and
//! `crates/core/tests/remote_tofu_integration.rs`'s exercise of it — ARE
//! the de-facto spec: hard reject on mismatch, no partial trust, no
//! re-prompt-and-continue. A future real client should import/port
//! [`verify_pin`] directly rather than re-deriving this logic.

use super::fingerprint_hex;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PinError {
    #[error(
        "This device's certificate doesn't match the one you paired with. It may have \
         changed, or something may be intercepting the connection. Pair again to continue."
    )]
    Mismatch,
}

/// Compares `presented_cert_der` (the certificate a TLS connection actually
/// presented) against `pinned_fingerprint_hex` (the fingerprint captured at
/// pairing time, from `PairingPayload.fingerprint`) using the exact same
/// SHA-256-of-DER construction the gateway itself uses to compute that
/// fingerprint ([`fingerprint_hex`]). Not constant-time: a certificate
/// fingerprint is public by design (it's printed directly in the pairing
/// QR code/manual code), so there's no secret here for a timing side
/// channel to leak.
///
/// A legitimate certificate change (e.g. the user reinstalled OpenPal, or
/// a future "regenerate certificate" admin action) is indistinguishable
/// from a MITM at this layer — by design, matching TOFU's usual trade-off.
/// The only recovery path is re-pairing, exactly like first-time pairing;
/// there is no "trust the new one" prompt.
pub fn verify_pin(presented_cert_der: &[u8], pinned_fingerprint_hex: &str) -> Result<(), PinError> {
    let actual = fingerprint_hex(presented_cert_der);
    if actual.eq_ignore_ascii_case(pinned_fingerprint_hex) {
        Ok(())
    } else {
        Err(PinError::Mismatch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matching_fingerprint_is_accepted() {
        let cert_der = b"pretend this is a DER-encoded certificate";
        let fingerprint = fingerprint_hex(cert_der);
        assert!(verify_pin(cert_der, &fingerprint).is_ok());
    }

    #[test]
    fn matching_fingerprint_is_case_insensitive() {
        let cert_der = b"pretend this is a DER-encoded certificate";
        let fingerprint = fingerprint_hex(cert_der).to_uppercase();
        assert!(verify_pin(cert_der, &fingerprint).is_ok());
    }

    #[test]
    fn a_different_certificate_is_rejected() {
        let real_cert = b"the real gateway's certificate";
        let attacker_cert = b"a MITM's own, different certificate";
        let pinned = fingerprint_hex(real_cert);
        assert_eq!(verify_pin(attacker_cert, &pinned), Err(PinError::Mismatch));
    }
}
