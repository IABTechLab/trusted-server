//! The `OpenRTB` 2.6 wire types the driver writes and reads, and the
//! extension objects this project adds to them.

use serde::Serialize;

pub use trusted_server_openrtb::{
    Banner, Bid, BidRequest, BidResponse, Device, Eid, Format, Geo, Imp, Object, Publisher, Regs,
    SeatBid, Site, ToExt, Uid, User,
};

/// The request the driver sends.
pub type OpenRtbRequest = BidRequest;
/// The response an exchange answers with.
pub type OpenRtbResponse = BidResponse;
/// One bid in a response.
pub type OpenRtbBid = Bid;

/// Converts a `u32` to the `i32` an `OpenRTB` field holds, logging and
/// returning `None` when the value does not fit.
#[must_use]
pub fn to_openrtb_i32(value: u32, field_name: &str, context: &str) -> Option<i32> {
    if let Ok(converted) = i32::try_from(value) {
        Some(converted)
    } else {
        log::warn!(
            "openrtb: omitting {field_name}={value} for {context} because value exceeds i32::MAX"
        );
        None
    }
}

/// Formats the identifiers an auction request carries as the `eids` array
/// of the request's user object.
#[must_use]
pub fn format_eids(eids: &[trusted_server_core::ec::eids::Eid]) -> Vec<Eid> {
    eids.iter()
        .map(|eid| Eid {
            source: Some(eid.source.clone()),
            uids: eid
                .uids
                .iter()
                .map(|uid| Uid {
                    id: Some(uid.id.clone()),
                    atype: uid.atype,
                    ext: uid.ext.as_ref().and_then(|ext| match ext {
                        serde_json::Value::Object(object) => Some(object.clone()),
                        _ => None,
                    }),
                })
                .collect(),
            ..Default::default()
        })
        .collect()
}

/// The `user.ext` object.
#[derive(Debug, Serialize)]
pub struct UserExt {
    /// TCF v2 consent string, which Prebid reads from `user.ext.consent`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub consent: Option<String>,
    /// Google Additional Consent settings for Ad Manager and `AdX` demand.
    #[serde(
        rename = "ConsentedProvidersSettings",
        skip_serializing_if = "Option::is_none"
    )]
    pub consented_providers_settings: Option<ConsentedProvidersSettings>,
    /// The identifiers the request carries, already gated by the permissions
    /// the auction resolved.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub eids: Option<Vec<Eid>>,
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

/// Prebid-compatible `regs.ext` consent fields.
///
/// Prebid Server reads consent signals from `regs.ext.*` rather than the
/// `OpenRTB` 2.6 top-level locations, so the driver writes both.
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

/// The `ext.trusted_server` object the driver writes on every request it
/// signs, and on an unsigned one whose source keeps the request identity.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regs_serializes_dual_placement_consent_fields() {
        // Mirror the production pattern: build ext, then duplicate into top-level.
        let ext = RegsExt {
            gdpr: Some(1),
            us_privacy: Some("1YNN".to_owned()),
            gpp: Some("DBACNY~CPXxRfA".to_owned()),
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
            id: Some("user-1".to_owned()),
            consent: Some("CPXxGfAPXxGfA".to_owned()),
            ext: UserExt {
                consent: Some("CPXxGfAPXxGfA".to_owned()),
                consented_providers_settings: Some(ConsentedProvidersSettings {
                    consented_providers: Some("2~2628.2316~dv.".to_owned()),
                }),
                eids: None,
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
            id: Some("user-1".to_owned()),
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
    fn identifiers_are_formatted_as_eids_with_their_agent_type() {
        let eids = format_eids(&[trusted_server_core::ec::eids::Eid {
            source: "google.com".to_owned(),
            uids: vec![trusted_server_core::ec::eids::Uid {
                id: "pair-id".to_owned(),
                atype: Some(571187),
                ext: Some(serde_json::json!({"linkType": 1})),
            }],
        }]);

        let serialized = serde_json::to_value(&eids).expect("should serialize");
        assert_eq!(
            serialized,
            serde_json::json!([{
                "source": "google.com",
                "uids": [{"id": "pair-id", "atype": 571187, "ext": {"linkType": 1}}]
            }]),
            "should carry the source, the identifier, the vendor's agent type and the extension"
        );
    }
}
