use base64::{engine::general_purpose, Engine};
use ed25519_dalek::{Signer as Ed25519Signer, SigningKey, VerifyingKey};
use std::sync::{LazyLock, OnceLock};

use crate::error::TrustedServerError;

// Hard coding for now use Fastly KV later
static SIGNING_KEY: OnceLock<SigningKey> = OnceLock::new();

pub fn set_signing_key(bytes: &[u8; 32]) -> Result<(), SigningKey> {
    SIGNING_KEY.set(SigningKey::from_bytes(bytes))
}

// I'm assuming that our algo will be Signing::EdDsa so we wont need to specify in the header
// do we need the `kid` in the header?

pub fn sign(payload: &[u8]) -> Result<String, TrustedServerError> {
    let signing_key = match SIGNING_KEY.get() {
        Some(key) => key,
        None => {
            return Err(TrustedServerError::Configuration { message: "Signing key not set".into() });
        }
    };

    let signature_bytes = signing_key.sign(payload).to_bytes();

    Ok(general_purpose::URL_SAFE_NO_PAD.encode(signature_bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set_test_key() {
        set_signing_key(&[
            1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24,
            25, 26, 27, 28, 29, 30, 31, 32,
        ])
        .expect("signing key should not be initialized");
    }

    #[test]
    fn test_sign() {
        set_test_key();

        let signature = sign(b"these pretzles are making me thirsty").unwrap();

        assert_eq!(signature, "hj_8vNGv_luCWSEvtfeKjGEwZOupaV8gcyREGpEc1u7uPzvnraB49iTr5UnOLKdQGTA5BpjQxJAdKXxx_JIMBA");
    }
}
