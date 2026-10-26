//! Record revisions, state revisions, UID hashes and the visitor key.

use hmac::{Hmac, Mac as _};
use sha2::{Digest as _, Sha256};

use super::input::RandomSource;
use crate::ec::kv_types::{KvEid, KvPartnerId, RECORD_REVISION_HEX_LEN};
use crate::settings::Settings;

// Temporary: first used by a later identity task.
#[allow(dead_code)]
type HmacSha256 = Hmac<Sha256>;

/// The visitor key, derived from `ec.passphrase` once per settings load.
///
/// It is the only keyed value: nothing stored in the row is keyed
/// (spec 9.3).
#[derive(Clone)]
pub(crate) struct IdentityKeys {
    visitor: [u8; 32],
}

impl core::fmt::Debug for IdentityKeys {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("IdentityKeys([redacted])")
    }
}

impl IdentityKeys {
    /// Derives the visitor key from `ec.passphrase`.
    #[must_use]
    // Temporary: first used by a later identity task.
    pub(crate) fn from_settings(settings: &Settings) -> Self {
        let passphrase = settings.ec.passphrase.expose().as_bytes();
        let mut mac =
            HmacSha256::new_from_slice(passphrase).expect("should accept any HMAC key length");
        mac.update(b"trusted-server/identity/visitor/v1");
        Self {
            visitor: mac.finalize().into_bytes().into(),
        }
    }
}

fn hex16(digest: &[u8]) -> String {
    hex::encode(&digest[..RECORD_REVISION_HEX_LEN / 2])
}

/// A record's stored revision: 16 lowercase hex digits.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct RecordRevision(String);

impl RecordRevision {
    /// Wraps a stored `rv`. `KvEntry::validate` has checked the format.
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

/// A record's revision (spec 5.4). `Unset` for a record without `rv`, which
/// only a schema version 1 row holds, until its next write gives it one.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum Revision {
    /// The record has no `rv`.
    Unset,
    /// The record's `rv`.
    Set(RecordRevision),
}

/// A fresh random revision, drawn on every write of a record.
#[must_use]
pub(crate) fn new_revision(random: &dyn RandomSource) -> RecordRevision {
    let mut bytes = [0u8; RECORD_REVISION_HEX_LEN / 2];
    random.fill(&mut bytes);
    RecordRevision(hex::encode(bytes))
}

/// The revision of a stored record. One revision covers the source's whole
/// entry set.
#[must_use]
pub(crate) fn record_revision(record: &KvPartnerId) -> Revision {
    record.rv.as_deref().map_or(Revision::Unset, |rv| {
        Revision::Set(RecordRevision::from_stored(rv))
    })
}

/// The hash of a provider's `ps.st` a call read, so a later apply can tell
/// whether another writer changed it since.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StateRevision(String);

impl StateRevision {
    /// The first 16 hex digits of the SHA-256 of `st` as compact JSON, or of
    /// the empty string when there is none. Compact JSON is never empty, so
    /// absent and present state never share a revision.
    #[must_use]
    // Temporary: first used by a later identity task.
    #[allow(dead_code)]
    pub(crate) fn of(st: Option<&serde_json::Value>) -> Self {
        let bytes = st
            .and_then(|value| serde_json::to_vec(value).ok())
            .unwrap_or_default();
        Self(hex16(&Sha256::digest(bytes)))
    }
}

/// The hash stored in `ps.iv` or `ps.ov` for a removed UID: the first 16 hex
/// digits of its unkeyed SHA-256 (spec 6.1).
#[must_use]
pub(crate) fn uid_hash(uid: &str) -> String {
    hex16(&Sha256::digest(uid.as_bytes()))
}

/// One hash per UID of a removed set, in set order, without duplicates.
#[must_use]
// Temporary: first used by a later identity task.
#[allow(dead_code)]
pub(crate) fn set_hashes(set: &[KvEid]) -> Vec<String> {
    let mut hashes = Vec::new();
    for uid in set.iter().flat_map(|entry| &entry.uids) {
        let hash = uid_hash(&uid.id);
        if !hashes.contains(&hash) {
            hashes.push(hash);
        }
    }
    hashes
}

/// Appends hashes to a `ps.iv` or `ps.ov` list, oldest first. A hash
/// already present moves to the end. The caller truncates.
// Temporary: first used by a later identity task.
#[allow(dead_code)]
pub(crate) fn push_hashes(list: &mut Vec<String>, hashes: Vec<String>) {
    for hash in hashes {
        list.retain(|existing| *existing != hash);
        list.push(hash);
    }
}

/// The per-visitor rate-limit key component, from the full EC ID.
#[must_use]
pub(crate) fn visitor_key(keys: &IdentityKeys, ec_id: &str) -> String {
    let mut mac = HmacSha256::new_from_slice(&keys.visitor).expect("should accept a 32-byte key");
    mac.update(ec_id.as_bytes());
    hex16(&mac.finalize().into_bytes())
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use sha2::{Digest as _, Sha256};

    use super::*;
    use crate::ec::identity::input::SeededRandom;
    use crate::ec::kv_types::{KvEid, KvPartnerId, KvUid};
    use crate::test_support::tests::create_test_settings;

    fn uid(id: &str) -> KvUid {
        KvUid {
            id: id.to_owned(),
            ..KvUid::default()
        }
    }

    #[test]
    fn new_revisions_are_hex_and_differ() {
        let random = SeededRandom::new(7);
        let first = new_revision(&random);
        let second = new_revision(&random);
        assert_eq!(first.as_str().len(), 16, "should be 16 hex digits");
        assert!(
            first
                .as_str()
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
            "should be lowercase hex"
        );
        assert_ne!(
            first, second,
            "should give two writes of the same UID different revisions"
        );
    }

    #[test]
    fn record_revision_is_rv_or_unset() {
        let with_rv = KvPartnerId {
            rv: Some("0123456789abcdef".to_owned()),
            ..KvPartnerId::new("u")
        };
        assert_eq!(
            record_revision(&with_rv),
            Revision::Set(RecordRevision::from_stored("0123456789abcdef")),
            "should use rv"
        );
        assert_eq!(
            record_revision(&KvPartnerId::browser("u", Some(10))),
            Revision::Unset,
            "should be Unset without rv, whatever the UID and at"
        );
    }

    #[test]
    fn uid_hashes_are_unkeyed_sha256_prefixes() {
        let expected = hex::encode(&Sha256::digest(b"example-uid")[..8]);
        assert_eq!(
            uid_hash("example-uid"),
            expected,
            "should be the first 16 hex digits of SHA-256"
        );

        let mut other_settings = create_test_settings();
        other_settings.ec.passphrase =
            crate::redacted::Redacted::new("another-example-passphrase-value".to_owned());
        assert_ne!(
            visitor_key(&IdentityKeys::from_settings(&other_settings), "ec"),
            visitor_key(&IdentityKeys::from_settings(&create_test_settings()), "ec"),
            "should key only the visitor key by the passphrase"
        );
    }

    #[test]
    fn a_removal_hashes_every_uid_of_the_set_and_push_moves_repeats_to_the_end() {
        let set = vec![
            KvEid {
                uids: vec![uid("a"), uid("b")],
                ..KvEid::default()
            },
            KvEid {
                uids: vec![uid("c"), uid("a")],
                ..KvEid::default()
            },
        ];

        let hashes = set_hashes(&set);

        let expected: Vec<String> = ["a", "b", "c"].iter().map(|id| uid_hash(id)).collect();
        assert_eq!(hashes, expected, "should hash each UID once, in set order");
        assert_eq!(
            set_hashes(&KvPartnerId::new("u").eids),
            vec![uid_hash("u")],
            "should cost one hash for a one-UID record"
        );

        let mut list = vec![uid_hash("a"), uid_hash("z")];
        push_hashes(&mut list, vec![uid_hash("a"), uid_hash("y")]);
        assert_eq!(
            list,
            vec![uid_hash("z"), uid_hash("a"), uid_hash("y")],
            "should move a repeat to the end"
        );
    }

    #[test]
    fn state_revisions_tell_absent_and_changed_state_apart() {
        let state = json!({ "token": "A0" });
        assert_eq!(
            StateRevision::of(Some(&state)),
            StateRevision::of(Some(&state.clone())),
            "should be stable"
        );
        assert_ne!(
            StateRevision::of(Some(&state)),
            StateRevision::of(Some(&json!({ "token": "A1" }))),
            "should change with the state"
        );
        assert_ne!(
            StateRevision::of(None),
            StateRevision::of(Some(&json!(""))),
            "should tell absent from empty"
        );
    }
}
