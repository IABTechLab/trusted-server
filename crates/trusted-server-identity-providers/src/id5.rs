//! ID5 identity provider: resolves the ID5 ID server side.
//!
//! Posts the request's consent and network signals to ID5's fetch endpoint.
//! It needs no login and sends no hashed email. ID5 returns complete EIDs
//! under `ids`, or the legacy `universal_uid`, which is given to the first
//! configured source.
//!
//! The request field names for the client IP and user agent follow ID5's
//! server-to-server integration and should be confirmed with ID5 for each
//! partner account.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::num::NonZeroUsize;
use std::time::Duration;

use bytes::Bytes;
use error_stack::{Report, ResultExt as _};
use http::header::{CONTENT_TYPE, ORIGIN};
use http::{HeaderMap, HeaderValue, Method};
use serde::{Deserialize, Serialize};
use trusted_server_core::ec::eid_types::SourceDomain;
use trusted_server_core::ec::provider::{
    EidLevelFields, EidShape, IdentityProvider, LookupInput, LookupOutcome, ProviderCapabilities,
    ProviderCommon, ProviderConfigError, ProviderEid, ProviderError, ProviderExpiry, ProviderKind,
    ProviderRequest, ProviderResponse, ProviderUid, SourceCapability, SourceInput,
};
use url::Url;

use crate::common::{VendorEid, is_routable_ip, join_section_ids};

/// The `universal_uid` ID5 returns for a visitor who opted out.
const OPTED_OUT_UID: &str = "0";

/// Most EIDs accepted from one ID5 response.
const MAX_EIDS: NonZeroUsize = NonZeroUsize::new(4).expect("should be non-zero");

/// Most UIDs accepted per ID5 EID.
const MAX_UIDS_PER_EID: NonZeroUsize = NonZeroUsize::new(2).expect("should be non-zero");

/// The ID5 provider kind.
pub struct Id5;

/// Settings for an `id5` provider.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Id5Settings {
    /// The ID5 partner number assigned to the publisher.
    pub partner_id: u32,
}

/// A built ID5 provider.
#[derive(Debug)]
pub struct Id5Provider {
    capabilities: ProviderCapabilities,
    url: Url,
    partner_id: u32,
}

impl ProviderKind for Id5 {
    const KIND: &'static str = "id5";
    type Settings = Id5Settings;
    type Provider = Id5Provider;

    fn build(
        common: &ProviderCommon<'_>,
        settings: Id5Settings,
    ) -> Result<Id5Provider, Report<ProviderConfigError>> {
        if common.sources.is_empty() {
            return Err(Report::new(ProviderConfigError::Invalid)
                .attach("an id5 provider needs at least one source"));
        }
        let mut url = common.endpoint.clone();
        url.path_segments_mut()
            .map_err(|()| {
                Report::new(ProviderConfigError::Invalid).attach("endpoint cannot hold a path")
            })?
            .pop_if_empty()
            .push(&format!("{}.json", settings.partner_id));
        Ok(Id5Provider {
            capabilities: ProviderCapabilities {
                sources: common
                    .sources
                    .iter()
                    .map(|source| SourceCapability {
                        source: source.clone(),
                        input: SourceInput::Anonymous,
                    })
                    .collect(),
                shape: EidShape {
                    max_eids: MAX_EIDS,
                    max_uids_per_eid: MAX_UIDS_PER_EID,
                    eid_level_fields: EidLevelFields::Preserved,
                },
            },
            url,
            partner_id: settings.partner_id,
        })
    }
}

#[derive(Serialize)]
struct FetchBody<'a> {
    partner: u32,
    v: &'static str,
    o: &'static str,
    #[serde(rename = "nbPage")]
    nb_page: u32,
    gdpr: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    gdpr_consent: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    us_privacy: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    gpp_string: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    gpp_sid: Option<String>,
    tml: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    ua: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ip: Option<String>,
}

#[derive(Deserialize)]
struct FetchReply<'body> {
    #[serde(default, borrow)]
    universal_uid: Option<Cow<'body, str>>,
    #[serde(default, borrow)]
    ids: BTreeMap<Cow<'body, str>, Id5Id<'body>>,
    #[serde(default)]
    cache_control: Option<CacheControl>,
}

#[derive(Deserialize)]
struct Id5Id<'body> {
    #[serde(default, borrow)]
    eid: Option<VendorEid<'body>>,
}

#[derive(Deserialize)]
struct CacheControl {
    max_age_sec: Option<u64>,
}

impl Id5Provider {
    fn primary_source(&self) -> &SourceDomain {
        &self.capabilities.sources[0].source
    }

    fn is_known(&self, source: &str) -> bool {
        self.capabilities
            .sources
            .iter()
            .any(|known| known.source.as_str() == source)
    }
}

impl IdentityProvider for Id5Provider {
    fn capabilities(&self) -> &ProviderCapabilities {
        &self.capabilities
    }

    fn build_request(
        &self,
        lookup: &LookupInput<'_>,
    ) -> Result<ProviderRequest, Report<ProviderError>> {
        let consent = lookup.consent;
        let origin = format!("https://{}", lookup.publisher_domain);
        let body = FetchBody {
            partner: self.partner_id,
            v: env!("CARGO_PKG_VERSION"),
            o: "trusted-server",
            nb_page: 1,
            gdpr: u8::from(consent.gdpr_applies),
            gdpr_consent: consent.raw_tc_string.as_deref(),
            us_privacy: consent.raw_us_privacy.as_deref(),
            gpp_string: consent.raw_gpp_string.as_deref(),
            gpp_sid: consent.gpp_section_ids.as_deref().map(join_section_ids),
            tml: &origin,
            ua: lookup.client.user_agent,
            ip: lookup
                .client
                .ip
                .filter(|ip| is_routable_ip(*ip))
                .map(|ip| ip.to_string()),
        };
        let body = serde_json::to_vec(&body).change_context(ProviderError::Request)?;
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            ORIGIN,
            HeaderValue::from_str(&origin).change_context(ProviderError::Request)?,
        );
        Ok(ProviderRequest {
            method: Method::POST,
            url: self.url.clone(),
            headers,
            body: Bytes::from(body),
        })
    }

    fn parse_response<'body>(
        &self,
        response: ProviderResponse<'body>,
    ) -> Result<LookupOutcome<'body>, Report<ProviderError>> {
        if !response.status.is_success() {
            return Err(Report::new(ProviderError::InvalidResponse)
                .attach(format!("ID5 returned status {}", response.status)));
        }
        let reply: FetchReply<'body> =
            serde_json::from_slice(response.body).change_context(ProviderError::InvalidResponse)?;
        let expiry = reply
            .cache_control
            .and_then(|cache| cache.max_age_sec)
            .map_or(ProviderExpiry::Unspecified, |seconds| {
                ProviderExpiry::After(Duration::from_secs(seconds))
            });

        let mut eids: Vec<ProviderEid<'body>> = reply
            .ids
            .into_values()
            .filter_map(|id| id.eid)
            .filter(|eid| self.is_known(&eid.source))
            .map(|eid| eid.into_provider_eid(expiry))
            .collect();
        let primary = self.primary_source().as_str();
        if !eids.iter().any(|eid| eid.source == primary)
            && let Some(uid) = reply
                .universal_uid
                .filter(|uid| !uid.is_empty() && uid != OPTED_OUT_UID)
        {
            eids.push(ProviderEid {
                source: Cow::Owned(primary.to_owned()),
                inserter: None,
                matcher: None,
                mm: None,
                ext: None,
                uids: vec![ProviderUid {
                    id: uid,
                    atype: Some(1),
                    ext: None,
                    expiry,
                }],
            });
        }
        Ok(if eids.is_empty() {
            LookupOutcome::NoMatch
        } else {
            LookupOutcome::Found(eids)
        })
    }
}

#[cfg(test)]
mod tests {
    use http::StatusCode;
    use serde_json::json;
    use trusted_server_core::consent::types::ConsentContext;
    use trusted_server_core::ec::provider::{ClientSignals, OwnedEntries, build_kind};
    use trusted_server_core::settings::{IdentityProviderConfig, ProviderSecrets};

    use super::*;

    fn provider() -> Box<dyn IdentityProvider> {
        let config: IdentityProviderConfig = serde_json::from_value(json!({
            "id": "id5",
            "kind": "id5",
            "endpoint": "https://id5.example.com/g/v2",
            "allowed_domains": ["id5.example.com"],
            "sources": ["id5.example.com", "id5-link.example.com"],
            "settings": { "partner_id": 1234 },
        }))
        .expect("should parse provider config");
        build_kind::<Id5>(&config, &ProviderSecrets::default()).expect("should build provider")
    }

    fn respond<'body>(
        provider: &dyn IdentityProvider,
        headers: &'body HeaderMap,
        body: &'body [u8],
    ) -> LookupOutcome<'body> {
        provider
            .parse_response(ProviderResponse {
                status: StatusCode::OK,
                headers,
                body,
            })
            .expect("should parse response")
    }

    #[test]
    fn posts_consent_and_network_signals_to_the_partner_url() {
        let provider = provider();
        let consent = ConsentContext {
            raw_us_privacy: Some("1YNN".to_owned()),
            gpp_section_ids: Some(vec![7, 8]),
            ..ConsentContext::default()
        };
        let lookup = LookupInput {
            ec_id: "ec-example",
            consent: &consent,
            client: ClientSignals {
                ip: Some("192.0.2.10".parse().expect("should parse ip")),
                user_agent: Some("ExampleBrowser/1.0"),
            },
            publisher_domain: "www.example.com",
            due_sources: &[],
            current: OwnedEntries::new(None, &[]),
            hashed_email: None,
        };

        let request = provider
            .build_request(&lookup)
            .expect("should build request");
        let body: serde_json::Value =
            serde_json::from_slice(&request.body).expect("should send JSON");

        assert_eq!(request.method, Method::POST, "should post");
        assert_eq!(
            request.url.as_str(),
            "https://id5.example.com/g/v2/1234.json",
            "should call the partner's fetch URL"
        );
        assert_eq!(
            request.headers.get(ORIGIN).map(HeaderValue::as_bytes),
            Some(&b"https://www.example.com"[..]),
            "should send the publisher origin"
        );
        assert_eq!(body["partner"], 1234, "should send the partner");
        assert_eq!(body["gdpr"], 0, "should send that GDPR does not apply");
        assert_eq!(body["us_privacy"], "1YNN", "should send US privacy");
        assert_eq!(body["gpp_sid"], "7,8", "should join GPP section IDs");
        assert_eq!(body["ip"], "192.0.2.10", "should send the client IP");
        assert_eq!(
            body["ua"], "ExampleBrowser/1.0",
            "should send the user agent"
        );
    }

    #[test]
    fn drops_a_private_client_ip() {
        let provider = provider();
        let consent = ConsentContext::default();
        let lookup = LookupInput {
            ec_id: "ec-example",
            consent: &consent,
            client: ClientSignals {
                ip: Some("10.1.2.3".parse().expect("should parse ip")),
                user_agent: None,
            },
            publisher_domain: "www.example.com",
            due_sources: &[],
            current: OwnedEntries::new(None, &[]),
            hashed_email: None,
        };

        let request = provider
            .build_request(&lookup)
            .expect("should build request");
        let body: serde_json::Value =
            serde_json::from_slice(&request.body).expect("should send JSON");

        assert!(body.get("ip").is_none(), "should not send a private IP");
    }

    #[test]
    fn parses_complete_eids_for_configured_sources() {
        let provider = provider();
        let headers = HeaderMap::new();
        let body = serde_json::to_vec(&json!({
            "ids": {
                "id5": { "eid": { "source": "id5.example.com", "uids": [{ "id": "id5-example", "atype": 1, "ext": { "linkType": 2 } }] } },
                "other": { "eid": { "source": "unknown.example.com", "uids": [{ "id": "x" }] } },
            },
            "cache_control": { "max_age_sec": 3600 },
        }))
        .expect("should encode body");

        let LookupOutcome::Found(eids) = respond(&*provider, &headers, &body) else {
            panic!("should find an EID");
        };

        assert_eq!(eids.len(), 1, "should drop unconfigured sources");
        assert_eq!(eids[0].source, "id5.example.com", "should keep the source");
        assert_eq!(eids[0].uids[0].id, "id5-example", "should keep the UID");
        assert!(eids[0].uids[0].ext.is_some(), "should keep the UID ext");
        assert!(
            matches!(eids[0].uids[0].expiry, ProviderExpiry::After(d) if d == Duration::from_secs(3600)),
            "should use the cache max age"
        );
    }

    #[test]
    fn falls_back_to_the_legacy_universal_uid() {
        let provider = provider();
        let headers = HeaderMap::new();
        let body = br#"{"universal_uid":"legacy-example"}"#;

        let LookupOutcome::Found(eids) = respond(&*provider, &headers, body) else {
            panic!("should find an EID");
        };

        assert_eq!(
            eids[0].source, "id5.example.com",
            "should use the first source"
        );
        assert_eq!(eids[0].uids[0].id, "legacy-example", "should keep the UID");
    }

    #[test]
    fn reports_no_match_for_an_opted_out_or_empty_reply() {
        let provider = provider();
        let headers = HeaderMap::new();

        for body in [&br#"{"universal_uid":"0"}"#[..], b"{}"] {
            assert!(
                matches!(respond(&*provider, &headers, body), LookupOutcome::NoMatch),
                "should report no match"
            );
        }
    }

    #[test]
    fn rejects_an_error_status_and_a_malformed_body() {
        let provider = provider();
        let headers = HeaderMap::new();

        for (status, body) in [
            (StatusCode::INTERNAL_SERVER_ERROR, &b"{}"[..]),
            (StatusCode::OK, b"not json"),
        ] {
            let error = provider
                .parse_response(ProviderResponse {
                    status,
                    headers: &headers,
                    body,
                })
                .expect_err("should reject the response");
            assert!(
                matches!(error.current_context(), ProviderError::InvalidResponse),
                "should report an invalid response"
            );
        }
    }
}
