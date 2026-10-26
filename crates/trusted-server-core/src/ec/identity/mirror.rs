//! The `ts-eids-s` mirror payload: the servable server set for page scripts,
//! capped to one cookie (spec 6.2). The server never reads the cookie back.

// Temporary: the coordinator, the first non-test caller, arrives in a later
// task. Remove this when it lands.
#![allow(dead_code)]

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::json;

use super::ids::IdentityProviderId;
use crate::openrtb::Eid;

/// Maximum bytes of the encoded cookie value.
pub(crate) const MIRROR_MAX_VALUE_BYTES: usize = 2048;
/// Maximum cookie lifetime: that of `ts-eids`.
pub(crate) const MIRROR_MAX_AGE_SEC: u64 = 86_400;

/// One source's servable server entries, as bidders receive them.
#[derive(Debug, Clone)]
pub(crate) struct MirrorSource {
    pub(crate) source: String,
    /// The provider whose `cookie_priority` ranks the source, for the log.
    pub(crate) provider: Option<IdentityProviderId>,
    pub(crate) priority: i32,
    /// The source's earliest `exp`, in Unix seconds.
    pub(crate) expires_at: Option<u64>,
    pub(crate) eids: Vec<Eid>,
}

/// What the request's KV read gave the mirror.
#[derive(Debug, Clone, Copy)]
pub(crate) enum MirrorRead<'a> {
    /// No KV read: the response carries no mirror member.
    NotRead,
    /// Consent denies EID persistence, or the row is a tombstone, withdrawn
    /// or missing.
    Cleared,
    /// The servable server set, in output order.
    Servable(&'a [MirrorSource]),
}

/// The mirror member of an `/auction` or `/_ts/page-bids` response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MirrorPayload {
    Value { value: String, max_age: u64 },
    Clear,
}

impl MirrorPayload {
    /// The JSON the shim reads.
    #[must_use]
    pub(crate) fn to_json(&self) -> serde_json::Value {
        match self {
            Self::Value { value, max_age } => json!({ "value": value, "max_age": max_age }),
            Self::Clear => json!({ "clear": true }),
        }
    }
}

fn encode(sources: &[&MirrorSource]) -> String {
    let eids: Vec<&Eid> = sources.iter().flat_map(|source| &source.eids).collect();
    let bytes =
        serde_json::to_vec(&json!({ "v": 1, "eids": eids })).expect("should serialize EIDs");
    URL_SAFE_NO_PAD.encode(bytes)
}

fn source_bytes(source: &MirrorSource) -> usize {
    serde_json::to_vec(&source.eids).map_or(usize::MAX, |bytes| bytes.len())
}

/// The mirror payload for a request (spec 6.2).
///
/// # Performance
///
/// Encodes the set once per omission. A set holds at most one entry set per
/// configured source, so this stays small.
#[must_use]
pub(crate) fn mirror_payload(read: MirrorRead<'_>, now: u64) -> Option<MirrorPayload> {
    let sources = match read {
        MirrorRead::NotRead => return None,
        MirrorRead::Cleared => return Some(MirrorPayload::Clear),
        MirrorRead::Servable(sources) => sources,
    };
    let mut kept: Vec<&MirrorSource> = sources.iter().collect();
    let mut value = encode(&kept);
    while value.len() > MIRROR_MAX_VALUE_BYTES && !kept.is_empty() {
        let index = (0..kept.len())
            .min_by_key(|&index| {
                (
                    kept[index].priority,
                    core::cmp::Reverse(source_bytes(kept[index])),
                    core::cmp::Reverse(index),
                )
            })
            .expect("should hold a source");
        let left_out = kept.remove(index);
        log::info!(
            "Leaving source '{}' of provider '{}' out of the ts-eids-s mirror, which exceeds {MIRROR_MAX_VALUE_BYTES} bytes",
            left_out.source,
            left_out
                .provider
                .as_ref()
                .map_or("none", IdentityProviderId::as_str),
        );
        value = encode(&kept);
    }
    if kept.is_empty() {
        return Some(MirrorPayload::Clear);
    }
    let max_age = kept
        .iter()
        .filter_map(|source| source.expires_at)
        .map(|exp| exp.saturating_sub(now).max(1))
        .fold(MIRROR_MAX_AGE_SEC, u64::min);
    Some(MirrorPayload::Value { value, max_age })
}

#[cfg(test)]
mod tests {
    use base64::Engine as _;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use serde_json::json;

    use super::*;
    use crate::openrtb::{Eid, Uid};

    const NOW: u64 = 1_760_000_000;

    fn source(name: &str, priority: i32, expires_at: Option<u64>, ids: &[&str]) -> MirrorSource {
        MirrorSource {
            source: name.to_owned(),
            provider: Some("p".parse().expect("should parse the provider ID")),
            priority,
            expires_at,
            eids: vec![Eid::new(
                name.to_owned(),
                ids.iter()
                    .map(|id| Uid {
                        id: (*id).to_owned(),
                        atype: Some(1),
                        ext: None,
                    })
                    .collect(),
            )],
        }
    }

    fn decode(payload: &MirrorPayload) -> (serde_json::Value, u64) {
        let MirrorPayload::Value { value, max_age } = payload else {
            panic!("should hold a value");
        };
        let bytes = URL_SAFE_NO_PAD
            .decode(value)
            .expect("should be unpadded base64url");
        (
            serde_json::from_slice(&bytes).expect("should be JSON"),
            *max_age,
        )
    }

    #[test]
    fn no_read_is_absent_and_a_denial_or_empty_set_is_clear() {
        assert_eq!(
            mirror_payload(MirrorRead::NotRead, NOW),
            None,
            "should leave the cookie as it is"
        );
        assert_eq!(
            mirror_payload(MirrorRead::Cleared, NOW),
            Some(MirrorPayload::Clear),
            "should clear on a denial"
        );
        assert_eq!(
            mirror_payload(MirrorRead::Servable(&[]), NOW),
            Some(MirrorPayload::Clear),
            "should clear an empty set"
        );
        assert_eq!(
            MirrorPayload::Clear.to_json(),
            json!({ "clear": true }),
            "should serialize the clear shape"
        );
    }

    #[test]
    fn value_is_the_v1_entries_in_unpadded_base64url() {
        let sources = [source("example.com", 0, None, &["example-a"])];

        let payload =
            mirror_payload(MirrorRead::Servable(&sources), NOW).expect("should return a payload");

        let (decoded, max_age) = decode(&payload);
        assert_eq!(
            decoded,
            json!({ "v": 1, "eids": [{ "source": "example.com", "uids": [{ "id": "example-a", "atype": 1 }] }] }),
            "should hold the entries exactly as bidders receive them"
        );
        assert_eq!(
            max_age, MIRROR_MAX_AGE_SEC,
            "should live a day without an exp"
        );
        let MirrorPayload::Value { value, .. } = &payload else {
            panic!("should hold a value")
        };
        assert_eq!(
            payload.to_json(),
            json!({ "value": value, "max_age": MIRROR_MAX_AGE_SEC }),
            "should serialize"
        );
    }

    #[test]
    fn max_age_never_outlives_the_earliest_exp() {
        let sources = [
            source("a.example.com", 0, Some(NOW + 3_600), &["a"]),
            source("b.example.com", 0, Some(NOW + 600), &["b"]),
            source("c.example.com", 0, Some(NOW + 999_999), &["c"]),
        ];

        let (_, max_age) = decode(
            &mirror_payload(MirrorRead::Servable(&sources), NOW).expect("should return a payload"),
        );

        assert_eq!(max_age, 600, "should take the earliest exp");
    }

    #[test]
    fn an_oversized_set_drops_whole_sources_lowest_priority_then_largest() {
        let big = |name: &str, priority, count| {
            let ids: Vec<String> = (0..count)
                .map(|index| format!("{name}-{index:040}"))
                .collect();
            source(
                name,
                priority,
                None,
                &ids.iter().map(String::as_str).collect::<Vec<_>>(),
            )
        };
        let sources = [
            big("keep-high", 10, 6),
            big("drop-low-large", 0, 12),
            big("keep-low-small", 0, 6),
            big("keep-mid", 5, 6),
        ];

        let payload =
            mirror_payload(MirrorRead::Servable(&sources), NOW).expect("should return a payload");

        let MirrorPayload::Value { value, .. } = &payload else {
            panic!("should hold a value")
        };
        assert!(value.len() <= MIRROR_MAX_VALUE_BYTES, "should fit the cap");
        let (decoded, _) = decode(&payload);
        let kept: Vec<&str> = decoded["eids"]
            .as_array()
            .expect("should be an array")
            .iter()
            .map(|eid| eid["source"].as_str().expect("should name the source"))
            .collect();
        assert_eq!(
            kept,
            vec!["keep-high", "keep-low-small", "keep-mid"],
            "should drop the largest lowest-priority source whole"
        );
    }

    #[test]
    fn a_set_where_nothing_fits_is_clear() {
        let ids: Vec<String> = (0..80).map(|index| format!("only-{index:040}")).collect();
        let sources = [source(
            "only.example.com",
            0,
            None,
            &ids.iter().map(String::as_str).collect::<Vec<_>>(),
        )];

        assert_eq!(
            mirror_payload(MirrorRead::Servable(&sources), NOW),
            Some(MirrorPayload::Clear),
            "should clear rather than keep an empty or partial set"
        );
    }
}
