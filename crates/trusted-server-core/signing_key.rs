//! The reading of an attestation signing key.
//!
//! A module of the build script, which refuses a schedule key that cannot
//! sign, and of `src/attestation.rs`, which signs with it, so a key the build
//! accepts is one the signer can read.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use p256::ecdsa::SigningKey;
use p256::pkcs8::DecodePrivateKey as _;

/// Reads a key held as base64 of its SEC1 or PKCS #8 DER encoding, saying
/// why when it cannot sign and never repeating the key.
pub(crate) fn read(key_text: &str) -> Result<SigningKey, &'static str> {
    let der = STANDARD
        .decode(key_text.trim())
        .map_err(|_| "the key is not base64")?;
    let secret = p256::SecretKey::from_sec1_der(&der)
        .or_else(|_| p256::SecretKey::from_pkcs8_der(&der))
        .map_err(|_| "the key is not a P-256 private key in SEC1 or PKCS #8 DER")?;
    Ok(SigningKey::from(secret))
}
