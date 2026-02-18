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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub synthetic_fresh: Option<String>,
}

impl ToExt for UserExt {}

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

#[derive(Debug, Serialize)]
pub struct PrebidImpExt {
    pub bidder: std::collections::HashMap<String, Value>,
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
    fn regs_serializes_consent_fields() {
        let regs = Regs {
            gdpr: Some(1),
            us_privacy: Some("1YNN".to_string()),
            gpp: Some("DBACNY~CPXxRfA".to_string()),
            gpp_sid: Some(vec![2, 6]),
            ext: None,
        };

        let serialized = serde_json::to_value(&regs).expect("should serialize");
        assert_eq!(serialized["gdpr"], 1);
        assert_eq!(serialized["us_privacy"], "1YNN");
        assert_eq!(serialized["gpp"], "DBACNY~CPXxRfA");
        assert_eq!(serialized["gpp_sid"], serde_json::json!([2, 6]));
        assert!(
            serialized.get("ext").is_none(),
            "ext should be omitted when None"
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
    fn user_serializes_consent_field() {
        let user = User {
            id: Some("user-1".to_string()),
            consent: Some("CPXxGfAPXxGfA".to_string()),
            ext: None,
        };

        let serialized = serde_json::to_value(&user).expect("should serialize");
        assert_eq!(serialized["consent"], "CPXxGfAPXxGfA");
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
}
