use serde::Serialize;
use serde_json::Value;

use crate::auction::types::OrchestratorExt;

pub type OpenRtbRequest = trusted_server_openrtb::BidRequest;
pub type OpenRtbResponse = trusted_server_openrtb::BidResponse;
pub type OpenRtbBid = trusted_server_openrtb::Bid;

pub use trusted_server_openrtb::{
    Banner, Bid, BidResponse, Device, Format, Geo, Imp, Publisher, Regs, SeatBid, Site, ToExt, User,
};

/// Convert a `u32` value to `i32` for `OpenRTB` fields, logging a warning and
/// returning `None` if the value exceeds `i32::MAX`.
#[must_use]
pub fn to_openrtb_i32(value: u32, field_name: &str, context: &str) -> Option<i32> {
    match i32::try_from(value) {
        Ok(converted) => Some(converted),
        Err(_) => {
            log::warn!(
                "openrtb: omitting {}={} for {} because value exceeds i32::MAX",
                field_name,
                value,
                context
            );
            None
        }
    }
}

// ============================================================================
// Extension types (project-specific, not part of the OpenRTB spec)
// ============================================================================

#[derive(Debug, Serialize)]
pub struct UserExt {
    /// TCF v2 consent string (Prebid reads `user.ext.consent`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub consent: Option<String>,
    /// Google Additional Consent settings for Ad Manager / `AdX` demand.
    #[serde(
        rename = "ConsentedProvidersSettings",
        skip_serializing_if = "Option::is_none"
    )]
    pub consented_providers_settings: Option<ConsentedProvidersSettings>,
    /// Extended User IDs from identity providers.
    ///
    /// Gated by TCF Purpose 1 (storage) and Purpose 4 (personalized ads).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub eids: Option<Vec<Eid>>,
    /// Whether this EC ID was freshly generated for this request.
    ///
    /// **Breaking change:** this wire field was previously named `synthetic_fresh`.
    /// Downstream PBS modules or analytics reading the old name must be updated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ec_fresh: Option<String>,
}

impl ToExt for UserExt {}

/// Google Additional Consent (AC) string container.
///
/// Covers ad tech providers not in the IAB Global Vendor List but
/// participating in the Google ecosystem. Required for Google Ad Manager
/// and `AdX` demand.
///
/// Format: `{version}~{provider_ids}~dv.` where provider IDs are
/// dot-separated Google ATP IDs.
#[derive(Debug, Serialize, Default)]
pub struct ConsentedProvidersSettings {
    /// The AC string value (e.g. `"2~2628.2316.3119~dv."`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub consented_providers: Option<String>,
}

/// An Extended User ID entry from an identity provider.
#[derive(Debug, Serialize)]
pub struct Eid {
    /// Identity provider domain (e.g. `"id5-sync.com"`).
    pub source: String,
    /// One or more user IDs from this provider.
    pub uids: Vec<Uid>,
}

/// A single user identifier within an [`Eid`] entry.
#[derive(Debug, Serialize)]
pub struct Uid {
    /// The identifier value.
    pub id: String,
    /// Agent type: 1 = cookie/device, 2 = person, 3 = user-provided.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub atype: Option<u8>,
    /// Provider-specific extension data.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ext: Option<Value>,
}

/// Prebid-compatible `regs.ext` consent fields.
///
/// Prebid Server reads consent signals from `regs.ext.*` rather than the
/// `OpenRTB` 2.6 top-level locations. We populate both to maximise
/// compatibility (see proposal Key Decision #2 — Dual-Placement).
#[derive(Debug, Clone, Serialize, Default)]
pub struct RegsExt {
    /// GDPR applicability flag (mirrors `regs.gdpr`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gdpr: Option<u8>,
    /// US Privacy string (mirrors `regs.us_privacy`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub us_privacy: Option<String>,
    /// GPP consent string (mirrors `regs.gpp`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gpp: Option<String>,
    /// GPP section ID list (mirrors `regs.gpp_sid`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gpp_sid: Option<Vec<u16>>,
}

impl ToExt for RegsExt {}

#[derive(Debug, Serialize)]
pub struct RequestExt {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prebid: Option<PrebidExt>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trusted_server: Option<TrustedServerExt>,
}

#[derive(Debug, Serialize, serde::Deserialize)]
pub struct PrebidExt {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub debug: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub returnallbidstatus: Option<bool>,
}

impl ToExt for RequestExt {}

#[derive(Debug, Serialize)]
pub struct TrustedServerExt {
    /// Version of the signing protocol (e.g., "1.1")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_host: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_scheme: Option<String>,
    /// Unix timestamp in milliseconds for replay protection
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ts: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct ImpExt {
    pub prebid: PrebidImpExt,
}

impl ToExt for ImpExt {}

#[derive(Debug, Default, Serialize)]
pub struct PrebidImpExt {
    #[serde(skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub bidder: std::collections::HashMap<String, Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub storedrequest: Option<ImpStoredRequest>,
}

/// PBS imp-level stored request reference.
///
/// PBS merges the stored imp JSON (keyed by `id`) into the outgoing request,
/// populating bidder params that are not sent inline.
#[derive(Debug, Serialize)]
pub struct ImpStoredRequest {
    pub id: String,
}

#[derive(Debug, Serialize)]
pub struct ResponseExt {
    pub orchestrator: OrchestratorExt,
}

impl ToExt for ResponseExt {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auction::types::OrchestratorExt;

    #[test]
    fn openrtb_response_round_trips_with_struct_literals() {
        let bid = OpenRtbBid {
            id: Some("bidder-a-slot-1".to_string()),
            impid: Some("slot-1".to_string()),
            price: Some(1.25),
            adm: Some("<div>Test Creative HTML</div>".to_string()),
            crid: Some("bidder-a-creative".to_string()),
            w: Some(300),
            h: Some(250),
            adomain: vec!["example.com".to_string()],
            ..Default::default()
        };

        let seatbid = SeatBid {
            seat: Some("bidder-a".to_string()),
            bid: vec![bid],
            ..Default::default()
        };

        let ext = ResponseExt {
            orchestrator: OrchestratorExt {
                strategy: "parallel_only".to_string(),
                providers: 2,
                total_bids: 3,
                time_ms: 12,
                provider_details: vec![],
            },
        }
        .to_ext();

        let response = OpenRtbResponse {
            id: Some("auction-1".to_string()),
            seatbid: vec![seatbid],
            ext,
            ..Default::default()
        };

        let serialized = serde_json::to_value(&response).expect("should serialize");
        let expected = serde_json::json!({
            "id": "auction-1",
            "seatbid": [{
                "seat": "bidder-a",
                "bid": [{
                    "id": "bidder-a-slot-1",
                    "impid": "slot-1",
                    "price": 1.25,
                    "adm": "<div>Test Creative HTML</div>",
                    "crid": "bidder-a-creative",
                    "w": 300,
                    "h": 250,
                    "adomain": ["example.com"]
                }]
            }],
            "ext": {
                "orchestrator": {
                    "strategy": "parallel_only",
                    "providers": 2,
                    "total_bids": 3,
                    "time_ms": 12,
                    "provider_details": []
                }
            }
        });

        assert_eq!(serialized, expected);
    }

    #[test]
    fn regs_serializes_dual_placement_consent_fields() {
        // Mirror the production pattern: build ext, then duplicate into top-level.
        let ext = RegsExt {
            gdpr: Some(1),
            us_privacy: Some("1YNN".to_string()),
            gpp: Some("DBACNY~CPXxRfA".to_string()),
            gpp_sid: Some(vec![2, 6]),
        };
        let regs = Regs {
            coppa: None,
            gdpr: Some(true),
            us_privacy: ext.us_privacy.clone(),
            gpp: ext.gpp.clone(),
            gpp_sid: ext
                .gpp_sid
                .as_ref()
                .map(|ids| ids.iter().map(|&id| i32::from(id)).collect())
                .unwrap_or_default(),
            ext: ext.to_ext(),
        };

        let serialized = serde_json::to_value(&regs).expect("should serialize");
        // Top-level fields
        assert_eq!(serialized["gdpr"], 1, "top-level gdpr should be 1");
        assert_eq!(
            serialized["us_privacy"], "1YNN",
            "top-level us_privacy should match"
        );
        assert_eq!(
            serialized["gpp"], "DBACNY~CPXxRfA",
            "top-level gpp should match"
        );
        assert_eq!(
            serialized["gpp_sid"],
            serde_json::json!([2, 6]),
            "top-level gpp_sid should match"
        );
        // ext-based fields (Prebid reads these)
        let ext = &serialized["ext"];
        assert_eq!(ext["gdpr"], 1, "ext gdpr should mirror top-level");
        assert_eq!(
            ext["us_privacy"], "1YNN",
            "ext us_privacy should mirror top-level"
        );
        assert_eq!(
            ext["gpp"], "DBACNY~CPXxRfA",
            "ext gpp should mirror top-level"
        );
        assert_eq!(
            ext["gpp_sid"],
            serde_json::json!([2, 6]),
            "ext gpp_sid should mirror top-level"
        );
    }

    #[test]
    fn regs_omits_none_fields() {
        let regs = Regs::default();
        let serialized = serde_json::to_value(&regs).expect("should serialize");
        let obj = serialized.as_object().expect("should be object");
        assert!(
            obj.is_empty(),
            "all-None regs should serialize as empty object"
        );
    }

    #[test]
    fn regs_ext_omits_none_fields() {
        let ext = RegsExt::default();
        let serialized = serde_json::to_value(&ext).expect("should serialize");
        let obj = serialized.as_object().expect("should be object");
        assert!(
            obj.is_empty(),
            "all-None RegsExt should serialize as empty object"
        );
    }

    #[test]
    fn user_serializes_dual_placement_consent() {
        let user = User {
            id: Some("user-1".to_string()),
            consent: Some("CPXxGfAPXxGfA".to_string()),
            ext: UserExt {
                consent: Some("CPXxGfAPXxGfA".to_string()),
                consented_providers_settings: Some(ConsentedProvidersSettings {
                    consented_providers: Some("2~2628.2316~dv.".to_string()),
                }),
                eids: None,
                ec_fresh: None,
            }
            .to_ext(),
            ..Default::default()
        };

        let serialized = serde_json::to_value(&user).expect("should serialize");
        assert_eq!(
            serialized["consent"], "CPXxGfAPXxGfA",
            "top-level user.consent should be set"
        );
        assert_eq!(
            serialized["ext"]["consent"], "CPXxGfAPXxGfA",
            "user.ext.consent should mirror top-level"
        );
        assert_eq!(
            serialized["ext"]["ConsentedProvidersSettings"]["consented_providers"],
            "2~2628.2316~dv.",
            "AC string should be present"
        );
    }

    #[test]
    fn user_omits_consent_when_none() {
        let user = User {
            id: Some("user-1".to_string()),
            consent: None,
            ext: None,
            ..Default::default()
        };

        let serialized = serde_json::to_value(&user).expect("should serialize");
        assert!(
            serialized.get("consent").is_none(),
            "consent should be omitted when None"
        );
    }

    #[test]
    fn eid_serializes_correctly() {
        let eid = Eid {
            source: "id5-sync.com".to_string(),
            uids: vec![Uid {
                id: "ID5-abc123".to_string(),
                atype: Some(1),
                ext: None,
            }],
        };

        let serialized = serde_json::to_value(&eid).expect("should serialize");
        assert_eq!(serialized["source"], "id5-sync.com", "source should match");
        assert_eq!(
            serialized["uids"][0]["id"], "ID5-abc123",
            "uid id should match"
        );
        assert_eq!(serialized["uids"][0]["atype"], 1, "atype should be 1");
        assert!(
            serialized["uids"][0].get("ext").is_none(),
            "ext should be omitted when None"
        );
    }

    #[test]
    fn user_ext_serializes_ec_fresh_not_synthetic_fresh() {
        let ext = UserExt {
            consent: None,
            consented_providers_settings: None,
            eids: None,
            ec_fresh: Some("true".to_string()),
        };

        let serialized = serde_json::to_value(&ext).expect("should serialize UserExt");
        assert_eq!(
            serialized["ec_fresh"], "true",
            "ec_fresh should be present in serialized output"
        );
        assert!(
            serialized.get("synthetic_fresh").is_none(),
            "synthetic_fresh should not appear — field was renamed to ec_fresh"
        );
    }
}
