//! Bounded authenticated codec for the short-lived server identity handoff.

use std::collections::{BTreeMap, BTreeSet};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac as _};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use crate::settings::{IdentityHandoffConfig, IdentityHandoffKey};

use super::is_valid_ec_id;
use super::kv_types::MAX_UID_LENGTH;
use super::partner::normalize_partner_source_domain;

/// Maximum size of the complete encoded cookie value.
pub(crate) const MAX_HANDOFF_BYTES: usize = 2048;
/// Maximum remaining lifetime of an entry accepted by the codec.
pub(crate) const HANDOFF_TTL_SECS: u64 = 300;

const TOKEN_VERSION: &str = "v1";
const SIGNING_DOMAIN: &[u8] = b"trusted-server/ts-eids-s/hmac-sha256\0";
const MAX_HANDOFF_SOURCES: usize = 64;
const MAX_KEY_ID_BYTES: usize = 32;
const MAX_MODULE_ID_BYTES: usize = 64;

type HmacSha256 = Hmac<Sha256>;

/// One authenticated server-capture observation.
///
/// This type deliberately omits `Debug`: UIDs must not leak through routine
/// formatting of handoff state.
#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct HandoffEntry {
    #[serde(rename = "m")]
    pub(crate) module: String,
    #[serde(rename = "s")]
    pub(crate) source: String,
    #[serde(rename = "u")]
    pub(crate) uid: String,
    #[serde(rename = "x", skip_serializing_if = "Option::is_none")]
    pub(crate) expires_at: Option<u64>,
    #[serde(rename = "d")]
    pub(crate) deadline: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HandoffEnvelope {
    #[serde(rename = "p")]
    publisher: String,
    #[serde(rename = "e")]
    ec_id: String,
    #[serde(rename = "d")]
    deadline: u64,
    #[serde(rename = "i")]
    entries: Vec<HandoffEntry>,
}

/// Encodes entries with the configured active key.
///
/// Invalid entries fail closed. When multiple valid entries exceed either the
/// source-count or encoded-size bound, complete entries are removed by
/// `(deadline, source)` order, earliest first.
pub(crate) fn encode(
    config: &IdentityHandoffConfig,
    publisher: &str,
    ec_id: &str,
    entries: Vec<HandoffEntry>,
    now: u64,
) -> Option<String> {
    if !config.enabled || publisher.is_empty() || !is_valid_ec_id(ec_id) {
        return None;
    }

    let key_id = config.active_key_id.as_str();
    let key = configured_key(config, key_id)?;
    let secret = decode_secret(key)?;

    let max_deadline = now.saturating_add(HANDOFF_TTL_SECS);
    let mut by_source = BTreeMap::new();
    for mut entry in entries {
        if !valid_module_id(&entry.module)
            || entry.uid.trim().is_empty()
            || entry.uid.len() > MAX_UID_LENGTH
            || entry.deadline > max_deadline
            || entry
                .expires_at
                .is_some_and(|expires_at| entry.deadline > expires_at)
        {
            return None;
        }

        entry.source = normalize_partner_source_domain(&entry.source).ok()?;
        if by_source.insert(entry.source.clone(), entry).is_some() {
            return None;
        }
    }

    // Never issue already-expired observations. This also lets callers rewrite
    // a merged handoff without extending or reissuing stale entries.
    by_source.retain(|_, entry| entry_is_live(entry, now));
    if by_source.is_empty() {
        return None;
    }

    // Reject an individually unencodable entry without letting it suppress
    // smaller entries in the same handoff. The exact signed value is measured,
    // so JSON escaping, publisher binding, key ID, and signature all count.
    let mut individually_oversized = Vec::new();
    for (source, entry) in &by_source {
        let value = encode_envelope_value(publisher, ec_id, key_id, &secret, vec![entry.clone()])?;
        if value.len() > MAX_HANDOFF_BYTES {
            individually_oversized.push(source.clone());
        }
    }
    for source in individually_oversized {
        by_source.remove(&source);
    }
    if by_source.is_empty() {
        return None;
    }

    while by_source.len() > MAX_HANDOFF_SOURCES {
        remove_earliest(&mut by_source)?;
    }

    loop {
        let entries = by_source.values().cloned().collect();
        let value = encode_envelope_value(publisher, ec_id, key_id, &secret, entries)?;
        if value.len() <= MAX_HANDOFF_BYTES {
            return Some(value);
        }
        remove_earliest(&mut by_source)?;
    }
}

/// Authenticates and decodes a handoff for the exact publisher and full EC ID.
///
/// An authenticated envelope whose entries have all expired returns
/// `Some(Vec::new())`, allowing the caller to clear it. Structural, signature,
/// key, and binding failures return `None`.
pub(crate) fn decode(
    config: &IdentityHandoffConfig,
    publisher: &str,
    ec_id: &str,
    value: &str,
    now: u64,
) -> Option<Vec<HandoffEntry>> {
    if !config.enabled || publisher.is_empty() || value.len() > MAX_HANDOFF_BYTES {
        return None;
    }

    let mut segments = value.split('.');
    let version = segments.next()?;
    let key_id = segments.next()?;
    let payload_b64 = segments.next()?;
    let signature_b64 = segments.next()?;
    if segments.next().is_some() || version != TOKEN_VERSION || !valid_key_id(key_id) {
        return None;
    }

    let key = configured_key(config, key_id)?;
    let secret = decode_secret(key)?;
    let signature = URL_SAFE_NO_PAD.decode(signature_b64.as_bytes()).ok()?;
    let mut mac = new_mac(&secret)?;
    update_mac(&mut mac, version, key_id, payload_b64);
    mac.verify_slice(&signature).ok()?;

    let payload = URL_SAFE_NO_PAD.decode(payload_b64.as_bytes()).ok()?;
    let envelope: HandoffEnvelope = serde_json::from_slice(&payload).ok()?;
    if envelope.publisher != publisher
        || envelope.ec_id != ec_id
        || !is_valid_ec_id(&envelope.ec_id)
        || envelope.entries.is_empty()
        || envelope.entries.len() > MAX_HANDOFF_SOURCES
    {
        return None;
    }

    let max_future_deadline = now.saturating_add(HANDOFF_TTL_SECS);
    let mut seen_sources = BTreeSet::new();
    for entry in &envelope.entries {
        let normalized_source = normalize_partner_source_domain(&entry.source).ok()?;
        if normalized_source != entry.source
            || !valid_module_id(&entry.module)
            || entry.uid.trim().is_empty()
            || entry.uid.len() > MAX_UID_LENGTH
            || entry.deadline > max_future_deadline
            || entry
                .expires_at
                .is_some_and(|expires_at| entry.deadline > expires_at)
            || !seen_sources.insert(entry.source.as_str())
        {
            return None;
        }
    }

    let latest_deadline = envelope.entries.iter().map(|entry| entry.deadline).max()?;
    if envelope.deadline != latest_deadline || envelope.deadline > max_future_deadline {
        return None;
    }

    Some(
        envelope
            .entries
            .into_iter()
            .filter(|entry| entry_is_live(entry, now))
            .collect(),
    )
}

fn configured_key<'a>(
    config: &'a IdentityHandoffConfig,
    key_id: &str,
) -> Option<&'a IdentityHandoffKey> {
    if !valid_key_id(key_id) {
        return None;
    }

    let mut matches = config.keys.iter().filter(|key| key.id == key_id);
    let key = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    Some(key)
}

fn decode_secret(key: &IdentityHandoffKey) -> Option<[u8; 32]> {
    if !valid_key_id(&key.id) {
        return None;
    }
    URL_SAFE_NO_PAD
        .decode(key.secret.expose().as_bytes())
        .ok()?
        .try_into()
        .ok()
}

fn valid_key_id(value: &str) -> bool {
    valid_safe_id(value, MAX_KEY_ID_BYTES)
}

fn valid_module_id(value: &str) -> bool {
    valid_safe_id(value, MAX_MODULE_ID_BYTES)
}

fn valid_safe_id(value: &str, max_bytes: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_bytes
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn entry_is_live(entry: &HandoffEntry, now: u64) -> bool {
    entry.deadline > now && entry.expires_at.is_none_or(|expires_at| expires_at > now)
}

fn remove_earliest(entries: &mut BTreeMap<String, HandoffEntry>) -> Option<()> {
    let source = entries
        .values()
        .min_by(|left, right| {
            left.deadline
                .cmp(&right.deadline)
                .then_with(|| left.source.cmp(&right.source))
        })?
        .source
        .clone();
    entries.remove(&source).map(|_| ())
}

fn encode_envelope_value(
    publisher: &str,
    ec_id: &str,
    key_id: &str,
    secret: &[u8; 32],
    entries: Vec<HandoffEntry>,
) -> Option<String> {
    let deadline = entries.iter().map(|entry| entry.deadline).max()?;
    let envelope = HandoffEnvelope {
        publisher: publisher.to_owned(),
        ec_id: ec_id.to_owned(),
        deadline,
        entries,
    };
    let payload = serde_json::to_vec(&envelope).ok()?;
    sign_value(key_id, secret, &payload)
}

fn sign_value(key_id: &str, secret: &[u8; 32], payload: &[u8]) -> Option<String> {
    let payload_b64 = URL_SAFE_NO_PAD.encode(payload);
    let mut mac = new_mac(secret)?;
    update_mac(&mut mac, TOKEN_VERSION, key_id, &payload_b64);
    let signature_b64 = URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
    Some(format!(
        "{TOKEN_VERSION}.{key_id}.{payload_b64}.{signature_b64}"
    ))
}

fn new_mac(secret: &[u8; 32]) -> Option<HmacSha256> {
    <HmacSha256 as hmac::KeyInit>::new_from_slice(secret).ok()
}

fn update_mac(mac: &mut HmacSha256, version: &str, key_id: &str, payload_b64: &str) {
    mac.update(SIGNING_DOMAIN);
    mac.update(version.as_bytes());
    mac.update(b"\0");
    mac.update(key_id.as_bytes());
    mac.update(b"\0");
    mac.update(payload_b64.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::redacted::Redacted;

    const NOW: u64 = 1_800_000_000;
    const PUBLISHER: &str = "publisher.example";
    const SOURCE: &str = "id.example";

    fn ec_id(seed: char) -> String {
        format!("{}.Ab123Z", seed.to_string().repeat(64))
    }

    fn key(id: &str, byte: u8) -> IdentityHandoffKey {
        IdentityHandoffKey {
            id: id.to_owned(),
            secret: Redacted::new(URL_SAFE_NO_PAD.encode([byte; 32])),
        }
    }

    fn config() -> IdentityHandoffConfig {
        IdentityHandoffConfig {
            enabled: true,
            active_key_id: "current".to_owned(),
            keys: vec![key("current", 7), key("previous", 9)],
        }
    }

    fn entry(source: &str, uid: &str, deadline: u64) -> HandoffEntry {
        HandoffEntry {
            module: "lockr".to_owned(),
            source: source.to_owned(),
            uid: uid.to_owned(),
            expires_at: None,
            deadline,
        }
    }

    fn signed_envelope(
        config: &IdentityHandoffConfig,
        envelope: &HandoffEnvelope,
        key_id: &str,
    ) -> String {
        let key = configured_key(config, key_id).expect("test key should be configured");
        let secret = decode_secret(key).expect("test key should decode");
        let payload = serde_json::to_vec(envelope).expect("test envelope should serialize");
        sign_value(key_id, &secret, &payload).expect("test envelope should sign")
    }

    fn resign_payload(config: &IdentityHandoffConfig, payload: &serde_json::Value) -> String {
        let key = configured_key(config, "current").expect("should configure test key");
        let secret = decode_secret(key).expect("should decode test key");
        let payload = serde_json::to_vec(payload).expect("should serialize test payload");
        sign_value("current", &secret, &payload).expect("should sign test payload")
    }

    fn token_payload(value: &str) -> serde_json::Value {
        let payload = value.split('.').nth(2).expect("token should have payload");
        let decoded = URL_SAFE_NO_PAD
            .decode(payload.as_bytes())
            .expect("payload should decode");
        serde_json::from_slice(&decoded).expect("payload should be JSON")
    }

    #[test]
    fn round_trip_normalizes_sources_and_preserves_entry_deadlines() {
        let config = config();
        let ec_id = ec_id('a');
        let entries = vec![HandoffEntry {
            module: "lockr_capture".to_owned(),
            source: "ID.Example.".to_owned(),
            uid: "opaque-user-id".to_owned(),
            expires_at: Some(NOW + 240),
            deadline: NOW + 180,
        }];

        let value =
            encode(&config, PUBLISHER, &ec_id, entries, NOW).expect("valid handoff should encode");
        let decoded =
            decode(&config, PUBLISHER, &ec_id, &value, NOW).expect("valid handoff should decode");

        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].source, SOURCE);
        assert_eq!(decoded[0].module, "lockr_capture");
        assert_eq!(decoded[0].uid, "opaque-user-id");
        assert_eq!(decoded[0].expires_at, Some(NOW + 240));
        assert_eq!(decoded[0].deadline, NOW + 180);
        assert!(value.len() <= MAX_HANDOFF_BYTES);
    }

    #[test]
    fn disabled_codec_neither_encodes_nor_decodes() {
        let mut config = config();
        let ec_id = ec_id('a');
        let value = encode(
            &config,
            PUBLISHER,
            &ec_id,
            vec![entry(SOURCE, "uid", NOW + 100)],
            NOW,
        )
        .expect("enabled codec should encode");

        config.enabled = false;
        assert!(
            encode(
                &config,
                PUBLISHER,
                &ec_id,
                vec![entry(SOURCE, "uid", NOW + 100)],
                NOW,
            )
            .is_none()
        );
        assert!(decode(&config, PUBLISHER, &ec_id, &value, NOW).is_none());
    }

    #[test]
    fn decode_accepts_rotated_non_active_key() {
        let mut old_config = config();
        old_config.active_key_id = "previous".to_owned();
        let ec_id = ec_id('a');
        let value = encode(
            &old_config,
            PUBLISHER,
            &ec_id,
            vec![entry(SOURCE, "uid", NOW + 100)],
            NOW,
        )
        .expect("old active key should encode");

        let new_config = config();
        assert!(decode(&new_config, PUBLISHER, &ec_id, &value, NOW).is_some());
    }

    #[test]
    fn invalid_or_ambiguous_key_material_fails_closed() {
        let ec_id = ec_id('a');
        for secret in [
            URL_SAFE_NO_PAD.encode([1_u8; 31]),
            URL_SAFE_NO_PAD.encode([1_u8; 33]),
            format!("{}=", URL_SAFE_NO_PAD.encode([1_u8; 32])),
            "not base64!".to_owned(),
        ] {
            let config = IdentityHandoffConfig {
                enabled: true,
                active_key_id: "current".to_owned(),
                keys: vec![IdentityHandoffKey {
                    id: "current".to_owned(),
                    secret: Redacted::new(secret),
                }],
            };
            assert!(
                encode(
                    &config,
                    PUBLISHER,
                    &ec_id,
                    vec![entry(SOURCE, "uid", NOW + 100)],
                    NOW,
                )
                .is_none()
            );
        }

        let mut duplicate = config();
        duplicate.keys.push(key("current", 11));
        assert!(
            encode(
                &duplicate,
                PUBLISHER,
                &ec_id,
                vec![entry(SOURCE, "uid", NOW + 100)],
                NOW,
            )
            .is_none()
        );

        let mut unsafe_id = config();
        unsafe_id.active_key_id = "bad.key".to_owned();
        unsafe_id.keys.push(key("bad.key", 4));
        assert!(
            encode(
                &unsafe_id,
                PUBLISHER,
                &ec_id,
                vec![entry(SOURCE, "uid", NOW + 100)],
                NOW,
            )
            .is_none()
        );
    }

    #[test]
    fn signature_and_exact_bindings_are_required() {
        let config = config();
        let bound_ec_id = ec_id('a');
        let value = encode(
            &config,
            PUBLISHER,
            &bound_ec_id,
            vec![entry(SOURCE, "uid", NOW + 100)],
            NOW,
        )
        .expect("valid handoff should encode");

        assert!(decode(&config, "other.example", &bound_ec_id, &value, NOW).is_none());
        assert!(
            decode(&config, PUBLISHER, &ec_id('b'), &value, NOW).is_none(),
            "the full EC ID must match"
        );

        let mut tampered = value.into_bytes();
        let index = tampered
            .iter()
            .rposition(|byte| *byte == b'.')
            .expect("token should have a signature separator")
            + 1;
        tampered[index] = if tampered[index] == b'A' { b'B' } else { b'A' };
        let tampered = String::from_utf8(tampered).expect("test token remains ASCII");
        assert!(decode(&config, PUBLISHER, &bound_ec_id, &tampered, NOW).is_none());
    }

    #[test]
    fn rejects_oversized_value_before_decoding() {
        let config = config();
        assert!(
            decode(
                &config,
                PUBLISHER,
                &ec_id('a'),
                &"A".repeat(MAX_HANDOFF_BYTES + 1),
                NOW,
            )
            .is_none()
        );
    }

    #[test]
    fn rejects_unknown_envelope_and_entry_fields() {
        let config = config();
        let ec_id = ec_id('a');
        let value = encode(
            &config,
            PUBLISHER,
            &ec_id,
            vec![entry(SOURCE, "uid", NOW + 100)],
            NOW,
        )
        .expect("valid handoff should encode");

        let mut envelope_unknown = token_payload(&value);
        envelope_unknown
            .as_object_mut()
            .expect("envelope should be an object")
            .insert("unknown".to_owned(), serde_json::json!(true));
        let value = resign_payload(&config, &envelope_unknown);
        assert!(decode(&config, PUBLISHER, &ec_id, &value, NOW).is_none());

        let valid = encode(
            &config,
            PUBLISHER,
            &ec_id,
            vec![entry(SOURCE, "uid", NOW + 100)],
            NOW,
        )
        .expect("valid handoff should encode");
        let mut entry_unknown = token_payload(&valid);
        entry_unknown["i"][0]["unknown"] = serde_json::json!(true);
        let value = resign_payload(&config, &entry_unknown);
        assert!(decode(&config, PUBLISHER, &ec_id, &value, NOW).is_none());
    }

    #[test]
    fn rejects_invalid_entries_and_duplicate_normalized_sources() {
        let config = config();
        let ec_id = ec_id('a');
        let cases = vec![
            entry(SOURCE, " ", NOW + 100),
            entry(SOURCE, &"u".repeat(MAX_UID_LENGTH + 1), NOW + 100),
            HandoffEntry {
                module: "bad.module".to_owned(),
                ..entry(SOURCE, "uid", NOW + 100)
            },
            entry("not_a_domain", "uid", NOW + 100),
            entry(SOURCE, "uid", NOW + HANDOFF_TTL_SECS + 1),
            HandoffEntry {
                expires_at: Some(NOW + 50),
                ..entry(SOURCE, "uid", NOW + 100)
            },
        ];
        for invalid in cases {
            assert!(
                encode(&config, PUBLISHER, &ec_id, vec![invalid], NOW).is_none(),
                "invalid entry should fail closed"
            );
        }

        assert!(
            encode(
                &config,
                PUBLISHER,
                &ec_id,
                vec![
                    entry("ID.Example", "one", NOW + 100),
                    entry("id.example.", "two", NOW + 200),
                ],
                NOW,
            )
            .is_none(),
            "duplicates are checked after normalization"
        );
    }

    #[test]
    fn decode_filters_each_expiry_and_returns_some_when_all_expired() {
        let config = config();
        let ec_id = ec_id('a');
        let value = encode(
            &config,
            PUBLISHER,
            &ec_id,
            vec![
                entry("early.example", "early", NOW + 10),
                HandoffEntry {
                    expires_at: Some(NOW + 20),
                    ..entry("provider.example", "provider", NOW + 20)
                },
                entry("live.example", "live", NOW + 100),
            ],
            NOW,
        )
        .expect("valid handoff should encode");

        let decoded = decode(&config, PUBLISHER, &ec_id, &value, NOW + 20)
            .expect("authenticated mixed-expiry handoff should decode");
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].source, "live.example");

        let decoded = decode(&config, PUBLISHER, &ec_id, &value, NOW + 100)
            .expect("authenticated expired handoff should support cleanup");
        assert!(decoded.is_empty());
    }

    #[test]
    fn decode_rejects_wrong_envelope_deadline_and_excess_future_deadline() {
        let config = config();
        let ec_id = ec_id('a');
        let envelope = HandoffEnvelope {
            publisher: PUBLISHER.to_owned(),
            ec_id: ec_id.clone(),
            deadline: NOW + 99,
            entries: vec![entry(SOURCE, "uid", NOW + 100)],
        };
        let value = signed_envelope(&config, &envelope, "current");
        assert!(decode(&config, PUBLISHER, &ec_id, &value, NOW).is_none());

        let envelope = HandoffEnvelope {
            deadline: NOW + HANDOFF_TTL_SECS + 1,
            entries: vec![entry(SOURCE, "uid", NOW + HANDOFF_TTL_SECS + 1)],
            ..envelope
        };
        let value = signed_envelope(&config, &envelope, "current");
        assert!(decode(&config, PUBLISHER, &ec_id, &value, NOW).is_none());
    }

    #[test]
    fn decode_rejects_noncanonical_or_duplicate_sources_even_when_signed() {
        let config = config();
        let ec_id = ec_id('a');
        let envelope = HandoffEnvelope {
            publisher: PUBLISHER.to_owned(),
            ec_id: ec_id.clone(),
            deadline: NOW + 100,
            entries: vec![entry("ID.Example", "uid", NOW + 100)],
        };
        let value = signed_envelope(&config, &envelope, "current");
        assert!(decode(&config, PUBLISHER, &ec_id, &value, NOW).is_none());

        let envelope = HandoffEnvelope {
            entries: vec![
                entry(SOURCE, "one", NOW + 100),
                entry(SOURCE, "two", NOW + 100),
            ],
            ..envelope
        };
        let value = signed_envelope(&config, &envelope, "current");
        assert!(decode(&config, PUBLISHER, &ec_id, &value, NOW).is_none());
    }

    #[test]
    fn individually_oversized_entry_does_not_suppress_smaller_entry() {
        let config = config();
        let ec_id = ec_id('a');
        let value = encode(
            &config,
            PUBLISHER,
            &ec_id,
            vec![
                entry("small.example", "small", NOW + 100),
                entry("oversized.example", &"\0".repeat(MAX_UID_LENGTH), NOW + 200),
            ],
            NOW,
        )
        .expect("smaller entry should still encode");

        let decoded =
            decode(&config, PUBLISHER, &ec_id, &value, NOW).expect("bounded handoff should decode");
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].source, "small.example");
    }

    #[test]
    fn deterministic_truncation_removes_earliest_deadlines_first() {
        let config = config();
        let ec_id = ec_id('a');
        let entries: Vec<_> = (0..10)
            .map(|index| {
                entry(
                    &format!("id-{index}.example"),
                    &format!("{index}-{}", "u".repeat(500)),
                    NOW + 100 + index,
                )
            })
            .collect();
        let mut reversed = entries.clone();
        reversed.reverse();

        let value =
            encode(&config, PUBLISHER, &ec_id, entries, NOW).expect("bounded subset should encode");
        let reversed_value = encode(&config, PUBLISHER, &ec_id, reversed, NOW)
            .expect("reordered bounded subset should encode");
        assert_eq!(value, reversed_value, "encoding should be deterministic");
        assert!(value.len() <= MAX_HANDOFF_BYTES);

        let decoded = decode(&config, PUBLISHER, &ec_id, &value, NOW)
            .expect("truncated handoff should decode");
        assert!(decoded.len() < 10);
        assert!(decoded.iter().all(|entry| entry.deadline > NOW + 100));
        assert!(
            decoded.iter().any(|entry| entry.deadline == NOW + 109),
            "latest entry should survive"
        );
    }

    #[test]
    fn source_count_truncation_keeps_the_latest_entries() {
        let config = config();
        let ec_id = ec_id('a');
        let entries: Vec<_> = (0..=MAX_HANDOFF_SOURCES)
            .map(|index| {
                entry(
                    &format!("id-{index}.example"),
                    "u",
                    NOW + 100 + index as u64,
                )
            })
            .collect();

        let value = encode(&config, PUBLISHER, &ec_id, entries, NOW)
            .expect("bounded source subset should encode");
        let decoded = decode(&config, PUBLISHER, &ec_id, &value, NOW)
            .expect("bounded source subset should decode");

        assert!(decoded.len() <= MAX_HANDOFF_SOURCES);
        assert!(decoded.iter().all(|entry| entry.deadline > NOW + 100));
        assert!(
            decoded
                .iter()
                .any(|entry| entry.deadline == NOW + 100 + MAX_HANDOFF_SOURCES as u64),
            "latest entry should survive"
        );
    }

    #[test]
    fn decode_rejects_more_than_sixty_four_sources_even_when_signed() {
        let config = config();
        let ec_id = ec_id('a');
        let entries: Vec<_> = (0..=MAX_HANDOFF_SOURCES)
            .map(|index| entry(&format!("id-{index}.example"), "u", NOW + 100))
            .collect();
        let envelope = HandoffEnvelope {
            publisher: PUBLISHER.to_owned(),
            ec_id: ec_id.clone(),
            deadline: NOW + 100,
            entries,
        };
        let value = signed_envelope(&config, &envelope, "current");

        // The normal encoded-size cap usually rejects this fixture first. Its
        // source-count check is still exercised if compact schema changes ever
        // make the same signed fixture fit.
        assert!(decode(&config, PUBLISHER, &ec_id, &value, NOW).is_none());
    }
}
