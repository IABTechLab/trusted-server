//! Record revisions and keyed hashes of identifiers.
//!
//! Every hash here is an HMAC with a key derived from `ec.passphrase` under
//! its own label, so a hash cannot be linked to an EC ID or UID without the
//! key, and hashes from different uses never collide.

use core::fmt;

use hmac::{Hmac, Mac as _};
use sha2::Sha256;

use super::ids::IdentityProviderId;
use super::input::RandomSource;
use crate::ec::kv_types::KvPartnerId;
use crate::settings::Settings;

type HmacSha256 = Hmac<Sha256>;

/// Keys derived from `ec.passphrase`, once per settings load.
#[derive(Clone)]
pub(crate) struct IdentityKeys {
    revision: [u8; 32],
    invalidated: [u8; 32],
    visitor: [u8; 32],
    fingerprint: [u8; 32],
    /// AES-256 key for `__Host-ts-eids-s`.
    pub(crate) cookie_s: [u8; 32],
    /// AES-256 key for `__Host-ts-eids-r-*`.
    pub(crate) cookie_r: [u8; 32],
}

impl fmt::Debug for IdentityKeys {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("IdentityKeys([redacted])")
    }
}

fn derive(passphrase: &[u8], label: &[u8]) -> [u8; 32] {
    let mut mac =
        HmacSha256::new_from_slice(passphrase).expect("should accept any HMAC key length");
    mac.update(label);
    mac.finalize().into_bytes().into()
}

/// The first 16 hex digits of an HMAC over `parts`, NUL-separated.
fn keyed_hex16(key: &[u8; 32], parts: &[&[u8]]) -> String {
    let mut mac = HmacSha256::new_from_slice(key).expect("should accept a 32-byte key");
    for (index, part) in parts.iter().enumerate() {
        if index > 0 {
            mac.update(b"\0");
        }
        mac.update(part);
    }
    let digest = mac.finalize().into_bytes();
    hex::encode(&digest[..8])
}

impl IdentityKeys {
    /// Derives every identity key from `ec.passphrase`.
    #[must_use]
    pub(crate) fn from_settings(settings: &Settings) -> Self {
        let passphrase = settings.ec.passphrase.expose().as_bytes();
        Self {
            revision: derive(passphrase, b"trusted-server/identity/revision/v1"),
            invalidated: derive(passphrase, b"trusted-server/identity/iv/v1"),
            visitor: derive(passphrase, b"trusted-server/identity/visitor/v1"),
            fingerprint: derive(passphrase, b"trusted-server/identity/fingerprint/v1"),
            cookie_s: derive(passphrase, b"trusted-server/ts-eids-s/key/v1"),
            cookie_r: derive(passphrase, b"trusted-server/ts-eids-r/key/v1"),
        }
    }
}

/// A record's opaque revision: 16 lowercase hex digits.
///
/// Drawn at random on every write of a record, including a refresh that
/// keeps the UID, because neither the UID nor `at` can tell two refreshes in
/// one second apart.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct Revision(String);

impl Revision {
    /// Wraps a stored `rv` or `pv`. Callers have validated the format.
    #[must_use]
    pub(crate) fn from_stored(value: &str) -> Self {
        Self(value.to_owned())
    }

    /// The hex digits.
    #[must_use]
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

/// A fresh random revision.
#[must_use]
pub(crate) fn new_revision(random: &dyn RandomSource) -> Revision {
    let mut bytes = [0u8; 8];
    random.fill(&mut bytes);
    Revision(hex::encode(bytes))
}

/// The revision of a stored record: its `rv`, or for a legacy record without
/// one, the keyed hash of its `(uid, at)` until its next write.
#[must_use]
pub(crate) fn record_revision(keys: &IdentityKeys, record: &KvPartnerId) -> Revision {
    if let Some(rv) = &record.rv {
        return Revision(rv.clone());
    }
    let at = record.at.unwrap_or_default().to_string();
    Revision(keyed_hex16(
        &keys.revision,
        &[record.uid.as_bytes(), at.as_bytes()],
    ))
}

/// The keyed hash stored in `ps.iv` for a UID the owner invalidated.
#[must_use]
pub(crate) fn invalidated_uid_hash(
    keys: &IdentityKeys,
    provider: &IdentityProviderId,
    uid: &str,
) -> String {
    keyed_hex16(
        &keys.invalidated,
        &[provider.as_str().as_bytes(), uid.as_bytes()],
    )
}

/// The per-visitor rate-limit key component, from the full EC ID.
#[must_use]
pub(crate) fn visitor_key(keys: &IdentityKeys, ec_id: &str) -> String {
    keyed_hex16(&keys.visitor, &[ec_id.as_bytes()])
}

/// The consent fingerprint over the signals a provider's policy reads.
#[must_use]
pub(crate) fn consent_fingerprint(keys: &IdentityKeys, bits: &str) -> String {
    keyed_hex16(&keys.fingerprint, &[bits.as_bytes()])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ec::identity::input::SeededRandom;
    use crate::ec::kv_types::KvPartnerId;
    use crate::redacted::Redacted;
    use crate::test_support::tests::create_test_settings;

    fn keys() -> IdentityKeys {
        IdentityKeys::from_settings(&create_test_settings())
    }

    #[test]
    fn new_revisions_are_lowercase_hex_and_differ() {
        let random = SeededRandom::new(7);
        let first = new_revision(&random);
        let second = new_revision(&random);
        assert_eq!(first.as_str().len(), 16, "should be 16 hex digits");
        assert!(
            first
                .as_str()
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
            "should be lowercase hex"
        );
        assert_ne!(
            first, second,
            "two writes of the same UID get different revisions"
        );
    }

    #[test]
    fn record_revision_prefers_rv_and_hashes_legacy_records() {
        let keys = keys();
        let with_rv = KvPartnerId {
            rv: Some("0123456789abcdef".to_owned()),
            ..KvPartnerId::new("u")
        };
        assert_eq!(
            record_revision(&keys, &with_rv).as_str(),
            "0123456789abcdef",
            "should use rv"
        );

        let legacy_a = KvPartnerId::browser("u", Some(10));
        let legacy_b = KvPartnerId::browser("u", Some(11));
        assert_eq!(
            record_revision(&keys, &legacy_a),
            record_revision(&keys, &legacy_a.clone()),
            "should be stable"
        );
        assert_ne!(
            record_revision(&keys, &legacy_a),
            record_revision(&keys, &legacy_b),
            "at changes it"
        );
        assert_eq!(
            record_revision(&keys, &legacy_a).as_str().len(),
            16,
            "legacy revisions are 16 hex digits too"
        );
    }

    #[test]
    fn hashes_are_keyed_and_namespaced() {
        let keys = keys();
        let provider: IdentityProviderId = "p".parse().expect("should parse id");
        let iv = invalidated_uid_hash(&keys, &provider, "u");
        let visitor = visitor_key(&keys, "u");
        assert_eq!(iv.len(), 16, "iv length");
        assert_ne!(
            iv, visitor,
            "different labels give different hashes for the same input"
        );
        assert_ne!(
            invalidated_uid_hash(&keys, &"q".parse().expect("should parse id"), "u"),
            iv,
            "the provider is part of the iv hash"
        );

        let mut other_settings = create_test_settings();
        other_settings.ec.passphrase = Redacted::new("another-example-passphrase-value".to_owned());
        let other = IdentityKeys::from_settings(&other_settings);
        assert_ne!(
            invalidated_uid_hash(&other, &provider, "u"),
            iv,
            "keyed by the passphrase"
        );
        assert_ne!(
            other.cookie_s, keys.cookie_s,
            "cookie keys follow the passphrase"
        );
        assert_ne!(keys.cookie_s, keys.cookie_r, "each cookie has its own key");
        assert_eq!(
            consent_fingerprint(&keys, "bits").len(),
            16,
            "fingerprint length"
        );
        assert_eq!(
            format!("{keys:?}"),
            "IdentityKeys([redacted])",
            "Debug never prints keys"
        );
    }
}
