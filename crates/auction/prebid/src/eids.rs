//! The cookie the Prebid page script persists the browser's identifiers in,
//! and the identifier module that reads it.
//!
//! The script writes `<public prefix>-eids`, such as `ts-eids`, holding a
//! base64 JSON array of `OpenRTB`-style entries (`{source, uids: [...]}`) from
//! Prebid's user ID modules, trimmed by the script to fit its own size cap.
//! The earlier flattened shape (`{source, id, atype}` per entry) is still
//! read, for a cookie written before the script changed. Core is handed what
//! the cookie holds through [`IdentifierModule`] and names the cookie
//! nowhere.

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use serde::Deserialize;
use serde_json::Value as JsonValue;

use trusted_server_core::ec::eids::{Eid, Uid};
use trusted_server_core::ec::identifiers::IdentifierModule;
use trusted_server_core::evidence::RequestInfo;
use trusted_server_core::module_context::{ModuleCall, Withheld};
use trusted_server_core::platform::RuntimeServices;

/// The cookie the script writes, named after the public prefix with
/// `PublicPrefix::cookie_name`, so `ts-eids` under core's own prefix.
pub const EID_COOKIE: &str = "eids";

/// Maximum raw cookie size accepted before base64 decode.
const MAX_EIDS_COOKIE_BYTES: usize = 8 * 1024;

/// Reads the identifiers the script persisted for the request.
#[derive(Debug, Default, Clone, Copy)]
pub struct PrebidEids;

impl PrebidEids {
    /// What the cookie holds, or nothing when the request carries none or
    /// the value cannot be read.
    fn read(&self, evidence: &dyn RequestInfo, services: &RuntimeServices) -> Vec<Eid> {
        let name = services.public_prefix().cookie_name(EID_COOKIE);
        let Some(value) = evidence.cookie(&name) else {
            return Vec::new();
        };
        match parse_eids_cookie(value) {
            Ok(eids) => eids,
            Err(error) => {
                log::trace!("Prebid EIDs: dropping the `{name}` cookie: {error}");
                Vec::new()
            }
        }
    }
}

impl IdentifierModule for PrebidEids {
    fn id(&self) -> &'static str {
        super::MODULE
    }

    fn identifiers(&self, call: ModuleCall<'_>) -> Result<Vec<Eid>, Withheld> {
        call.inject(self, Self::read)
    }
}

/// Legacy flattened cookie entry.
#[derive(Debug, Deserialize)]
struct LegacyCookieEid {
    source: String,
    id: String,
    atype: i32,
}

/// `OpenRTB`-style cookie entry.
#[derive(Debug, Deserialize)]
struct StructuredCookieEid {
    source: String,
    #[serde(default)]
    uids: Vec<StructuredCookieUid>,
}

#[derive(Debug, Deserialize)]
struct StructuredCookieUid {
    id: String,
    #[serde(default)]
    atype: Option<i32>,
    #[serde(default)]
    ext: Option<JsonValue>,
}

enum DecodedCookieEids {
    Legacy(Vec<LegacyCookieEid>),
    Structured(Vec<StructuredCookieEid>),
}

/// Parses the cookie's value into `OpenRTB`-style `Eid` entries, in either of
/// the shapes the script has written.
///
/// # Errors
///
/// Returns an error when the cookie exceeds the raw size limit, is not valid
/// base64, or does not contain either supported JSON payload shape.
pub fn parse_eids_cookie(cookie_value: &str) -> Result<Vec<Eid>, String> {
    decode_eids_cookie(cookie_value).map(DecodedCookieEids::into_openrtb)
}

fn decode_eids_cookie(cookie_value: &str) -> Result<DecodedCookieEids, String> {
    if eids_cookie_exceeds_size_limit(cookie_value) {
        return Err(format!("cookie too large ({} bytes)", cookie_value.len()));
    }

    let bytes = BASE64
        .decode(cookie_value)
        .map_err(|e| format!("base64 decode failed: {e}"))?;

    if let Ok(eids) = serde_json::from_slice::<Vec<LegacyCookieEid>>(&bytes) {
        return Ok(DecodedCookieEids::Legacy(eids));
    }

    let structured = serde_json::from_slice::<Vec<StructuredCookieEid>>(&bytes)
        .map_err(|e| format!("JSON parse failed: {e}"))?;
    Ok(DecodedCookieEids::Structured(structured))
}

impl DecodedCookieEids {
    fn into_openrtb(self) -> Vec<Eid> {
        match self {
            Self::Legacy(entries) => legacy_cookie_eids_to_openrtb(entries),
            Self::Structured(entries) => structured_cookie_eids_to_openrtb(entries),
        }
    }
}

fn eids_cookie_exceeds_size_limit(cookie_value: &str) -> bool {
    cookie_value.len() > MAX_EIDS_COOKIE_BYTES
}

fn structured_cookie_eids_to_openrtb(entries: Vec<StructuredCookieEid>) -> Vec<Eid> {
    let mut eids = Vec::new();

    for entry in entries {
        if entry.source.is_empty() {
            continue;
        }

        let uids: Vec<_> = entry
            .uids
            .into_iter()
            .filter_map(structured_cookie_uid_to_openrtb)
            .collect();
        if uids.is_empty() {
            continue;
        }

        eids.push(Eid {
            source: entry.source,
            uids,
        });
    }

    eids
}

fn structured_cookie_uid_to_openrtb(uid: StructuredCookieUid) -> Option<Uid> {
    if uid.id.is_empty() {
        return None;
    }

    let atype = uid.atype.filter(|atype| *atype >= 0);
    let ext = match uid.ext {
        Some(JsonValue::Object(_)) => uid.ext,
        _ => None,
    };

    Some(Uid {
        id: uid.id,
        atype,
        ext,
    })
}

fn legacy_cookie_eids_to_openrtb(entries: Vec<LegacyCookieEid>) -> Vec<Eid> {
    entries
        .into_iter()
        .filter(|entry| !entry.source.is_empty() && !entry.id.is_empty())
        .map(|entry| Eid {
            source: entry.source,
            uids: vec![Uid {
                id: entry.id,
                atype: (entry.atype >= 0).then_some(entry.atype),
                ext: None,
            }],
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    use http::HeaderMap;
    use serde_json::json;

    use trusted_server_core::evidence::OwnedRequestInfo;
    use trusted_server_core::module_context::ModuleContext;
    use trusted_server_core::module_context::test_support::request;
    use trusted_server_core::permissions::PermissionSet;
    use trusted_server_core::platform::test_support::noop_services;

    fn encode_json(value: &serde_json::Value) -> String {
        BASE64.encode(serde_json::to_vec(value).expect("should serialize EID JSON"))
    }

    fn with_cookie_header(header: &str) -> OwnedRequestInfo {
        let mut headers = HeaderMap::new();
        headers.insert(
            "cookie",
            header.parse().expect("should build a cookie header"),
        );
        OwnedRequestInfo::new(String::new(), headers)
    }

    /// What the module answers for a request carrying `cookie_header`, under
    /// core's own prefix.
    fn learned(cookie_header: &str) -> Vec<Eid> {
        let evidence = with_cookie_header(cookie_header);
        let services = noop_services();
        let context = ModuleContext::new(request("/"))
            .with_evidence(&evidence)
            .with_services(&services);
        PrebidEids
            .identifiers(context.call(PrebidEids.id(), PermissionSet::none()))
            .expect("the call should carry the evidence and services the module names")
    }

    #[test]
    fn the_module_reads_the_cookie_under_the_public_prefix() {
        let encoded = encode_json(&json!([
            {"source": "id5-sync.com", "uids": [{"id": "ID5_abc", "atype": 1}]}
        ]));

        let eids = learned(&format!("other=1; ts-eids={encoded}; another=2"));

        assert_eq!(eids.len(), 1, "should read the cookie named by the prefix");
        assert_eq!(eids[0].source, "id5-sync.com");
        assert_eq!(eids[0].uids[0].id, "ID5_abc");
    }

    #[test]
    fn the_module_learns_nothing_from_a_request_without_the_cookie() {
        assert!(
            learned("other=1").is_empty(),
            "a request without the cookie carries no identifiers"
        );
    }

    #[test]
    fn the_module_learns_nothing_from_a_cookie_it_cannot_read() {
        assert!(
            learned("ts-eids=not-valid-base64!!!").is_empty(),
            "an unreadable cookie should be dropped, not refused"
        );
    }

    #[test]
    fn the_module_is_withheld_without_the_evidence_it_names() {
        let withheld = PrebidEids
            .identifiers(ModuleCall::empty())
            .expect_err("a call carrying no evidence should withhold it");

        assert_eq!(withheld.value(), "request evidence");
    }

    #[test]
    fn parse_eids_cookie_parses_legacy_flat_payload() {
        let eids = vec![
            json!({"source": "id5-sync.com", "id": "ID5_abc", "atype": 1}),
            json!({"source": "liveramp.com", "id": "LR_xyz", "atype": 3}),
            json!({"source": "google.com", "id": "pair-id", "atype": 571187}),
        ];
        let encoded = BASE64.encode(serde_json::to_vec(&eids).expect("should serialize"));

        let decoded = parse_eids_cookie(&encoded).expect("should decode valid payload");
        assert_eq!(decoded.len(), 3, "should parse all EIDs");
        assert_eq!(decoded[0].source, "id5-sync.com");
        assert_eq!(decoded[0].uids[0].id, "ID5_abc");
        assert_eq!(decoded[1].source, "liveramp.com");
        assert_eq!(decoded[1].uids[0].id, "LR_xyz");
        assert_eq!(
            decoded[2].source, "google.com",
            "should preserve PAIR source"
        );
        assert_eq!(
            decoded[2].uids[0].atype,
            Some(571187),
            "should preserve PAIR vendor-specific atype"
        );
    }

    #[test]
    fn parse_eids_cookie_parses_structured_payload() {
        let eids = vec![json!({
            "source": "sharedid.org",
            "uids": [
                {"id": "shared_123", "atype": 3},
                {"id": "shared_456", "ext": {"provider": "example"}},
                {"id": "shared_invalid", "atype": -1}
            ]
        })];
        let encoded = BASE64.encode(serde_json::to_vec(&eids).expect("should serialize"));

        let decoded = parse_eids_cookie(&encoded).expect("should decode valid payload");
        assert_eq!(decoded.len(), 1, "should parse one structured EID entry");
        assert_eq!(decoded[0].source, "sharedid.org");
        assert_eq!(decoded[0].uids.len(), 3, "should preserve multiple UIDs");
        assert_eq!(decoded[0].uids[0].id, "shared_123");
        assert_eq!(decoded[0].uids[0].atype, Some(3));
        assert_eq!(
            decoded[0].uids[1].ext,
            Some(json!({"provider": "example"})),
            "should preserve UID ext objects"
        );
        assert_eq!(
            decoded[0].uids[2].atype, None,
            "should drop negative atype values"
        );
    }

    #[test]
    fn parse_eids_cookie_preserves_pair_atype() {
        let encoded = encode_json(&json!([
            {
                "source": "google.com",
                "uids": [{ "id": "pair-id", "atype": 571187 }]
            }
        ]));

        let decoded = parse_eids_cookie(&encoded).expect("should decode PAIR EID");

        assert_eq!(
            decoded[0].uids[0].atype,
            Some(571187),
            "should preserve PAIR's vendor-specific atype"
        );
    }

    #[test]
    fn parse_eids_cookie_skips_entries_with_nothing_to_offer() {
        let encoded = encode_json(&json!([
            {"source": "", "uids": [{"id": "no-source", "atype": 1}]},
            {"source": "empty.example", "uids": []},
            {"source": "blank.example", "uids": [{"id": "", "atype": 1}]},
            {"source": "kept.example", "uids": [{"id": "kept", "ext": "not-an-object"}]}
        ]));

        let decoded = parse_eids_cookie(&encoded).expect("should decode the payload");

        assert_eq!(
            decoded.len(),
            1,
            "should keep only the entry with a source and a uid"
        );
        assert_eq!(decoded[0].source, "kept.example");
        assert_eq!(
            decoded[0].uids[0].ext, None,
            "an ext that is not an object should be dropped"
        );
    }

    #[test]
    fn parse_eids_cookie_rejects_invalid_base64() {
        let result = parse_eids_cookie("not-valid-base64!!!");
        assert!(result.is_err(), "should reject invalid base64");
    }

    #[test]
    fn parse_eids_cookie_rejects_invalid_json() {
        let encoded = BASE64.encode(b"not json");
        let result = parse_eids_cookie(&encoded);
        assert!(result.is_err(), "should reject invalid JSON");
    }

    #[test]
    fn eids_cookie_rejects_oversized_payloads() {
        let oversized = "x".repeat(MAX_EIDS_COOKIE_BYTES + 1);
        let exact_limit = "x".repeat(MAX_EIDS_COOKIE_BYTES);

        assert!(
            eids_cookie_exceeds_size_limit(&oversized),
            "should reject cookies larger than the raw size cap"
        );
        assert!(
            !eids_cookie_exceeds_size_limit(&exact_limit),
            "should allow cookies exactly at the raw size cap"
        );
        assert!(
            parse_eids_cookie(&oversized).is_err(),
            "an oversized cookie should be refused before decoding"
        );
    }
}
