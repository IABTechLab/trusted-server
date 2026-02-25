use serde::Serialize;
use serde_json::Value;

use crate::auction::types::OrchestratorExt;

/// Minimal subset of `OpenRTB` 2.x bid request used by Trusted Server.
#[derive(Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub struct OpenRtbRequest {
    /// Unique ID of the bid request, provided by the exchange.
    pub id: String,
    pub imp: Vec<Imp>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub site: Option<Site>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<User>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device: Option<Device>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub regs: Option<Regs>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ext: Option<RequestExt>,
}

#[derive(Debug, Serialize)]
pub struct Imp {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub banner: Option<Banner>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ext: Option<ImpExt>,
}

#[derive(Debug, Serialize)]
pub struct Banner {
    pub format: Vec<Format>,
}

#[derive(Debug, Serialize)]
pub struct Format {
    pub w: u32,
    pub h: u32,
}

#[derive(Debug, Serialize)]
pub struct Site {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<String>,
}

#[derive(Debug, Serialize, Default)]
pub struct User {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// TCF v2 consent string (raw TC String from `euconsent-v2` cookie).
    ///
    /// `OpenRTB` 2.6 canonical field for GDPR consent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub consent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ext: Option<UserExt>,
}

#[derive(Debug, Serialize, Default)]
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub synthetic_fresh: Option<String>,
}

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

#[derive(Debug, Serialize, Default)]
pub struct Device {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ua: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub geo: Option<Geo>,
}

#[derive(Debug, Serialize)]
pub struct Geo {
    /// Location type per `OpenRTB` spec (1=GPS, 2=IP address, 3=user provided)
    #[serde(rename = "type")]
    pub geo_type: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub city: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
}

#[derive(Debug, Serialize, Default)]
pub struct Regs {
    /// GDPR applicability flag (1 = GDPR applies, 0 = does not apply).
    ///
    /// `OpenRTB` 2.6 canonical field. Set based on TCF consent presence.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gdpr: Option<u8>,
    /// US Privacy string (4-character IAB CCPA format).
    ///
    /// `OpenRTB` 2.6 top-level field (migrated from `regs.ext.us_privacy`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub us_privacy: Option<String>,
    /// GPP consent string (raw `__gpp` cookie value).
    ///
    /// `OpenRTB` 2.6 canonical field for IAB Global Privacy Platform.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gpp: Option<String>,
    /// GPP section ID list (active sections in the GPP string).
    ///
    /// `OpenRTB` 2.6 canonical field, derived from decoded GPP data.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gpp_sid: Option<Vec<u16>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ext: Option<RegsExt>,
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

#[derive(Debug, Serialize, Default)]
pub struct RequestExt {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prebid: Option<PrebidExt>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trusted_server: Option<TrustedServerExt>,
}

#[derive(Debug, Serialize, Default)]
pub struct PrebidExt {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub debug: Option<bool>,
}

#[derive(Debug, Serialize, Default)]
pub struct TrustedServerExt {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_host: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_scheme: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ImpExt {
    pub prebid: PrebidImpExt,
}

#[derive(Debug, Serialize)]
pub struct PrebidImpExt {
    pub bidder: std::collections::HashMap<String, Value>,
}

/// Minimal subset of `OpenRTB` 2.x bid response used by Trusted Server.
#[derive(Debug, Serialize)]
pub struct OpenRtbResponse {
    pub id: String,
    pub seatbid: Vec<SeatBid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ext: Option<ResponseExt>,
}

#[derive(Debug, Serialize)]
pub struct SeatBid {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seat: Option<String>,
    pub bid: Vec<OpenRtbBid>,
}

#[derive(Debug, Serialize)]
pub struct OpenRtbBid {
    pub id: String,
    pub impid: String,
    pub price: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub adm: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub crid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub w: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub h: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub adomain: Option<Vec<String>>,
}

#[derive(Debug, Serialize)]
pub struct ResponseExt {
    pub orchestrator: OrchestratorExt,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auction::types::OrchestratorExt;

    #[test]
    fn openrtb_response_serializes_expected_fields() {
        let response = OpenRtbResponse {
            id: "auction-1".to_string(),
            seatbid: vec![SeatBid {
                seat: Some("bidder-a".to_string()),
                bid: vec![OpenRtbBid {
                    id: "bidder-a-slot-1".to_string(),
                    impid: "slot-1".to_string(),
                    price: 1.25,
                    adm: Some("<div>Test Creative HTML</div>".to_string()),
                    crid: Some("bidder-a-creative".to_string()),
                    w: Some(300),
                    h: Some(250),
                    adomain: Some(vec!["example.com".to_string()]),
                }],
            }],
            ext: Some(ResponseExt {
                orchestrator: OrchestratorExt {
                    strategy: "parallel_only".to_string(),
                    providers: 2,
                    total_bids: 3,
                    time_ms: 12,
                },
            }),
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
                    "time_ms": 12
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
            gdpr: ext.gdpr,
            us_privacy: ext.us_privacy.clone(),
            gpp: ext.gpp.clone(),
            gpp_sid: ext.gpp_sid.clone(),
            ext: Some(ext),
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
            ext: Some(UserExt {
                consent: Some("CPXxGfAPXxGfA".to_string()),
                consented_providers_settings: Some(ConsentedProvidersSettings {
                    consented_providers: Some("2~2628.2316~dv.".to_string()),
                }),
                eids: None,
                synthetic_fresh: None,
            }),
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
}
