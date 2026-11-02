//! `LiveIntent` identity provider: resolves `LiveIntent`'s IDs server side.
//!
//! Calls `LiveIntent`'s identity exchange with consent and network signals
//! and asks for the attributes the operator configured. Each attribute maps
//! to one configured `OpenRTB` source. The lookup is anonymous: no hashed
//! email and no first-party `LiveIntent` cookie are sent.
//!
//! As in Prebid's `LiveIntent` module, every attribute other than `nonId` is
//! a partner ID and its UID carries `ext.provider` naming `LiveIntent`.
//!
//! This kind has no Lockr capability: Lockr AIM is not documented to
//! deliver `LiveIntent` IDs, so a `lockr` settings table is rejected.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::num::NonZeroUsize;
use std::time::{Duration, SystemTime};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use bytes::Bytes;
use error_stack::{Report, ResultExt as _};
use http::header::{DATE, EXPIRES, ORIGIN};
use http::{HeaderMap, HeaderValue, Method, StatusCode};
use serde::Deserialize;
use serde_json::value::RawValue;
use trusted_server_core::ec::eid_types::SourceDomain;
use trusted_server_core::ec::provider::{
    EidLevelFields, EidShape, IdentityProvider, LookupInput, LookupOutcome, ProviderCapabilities,
    ProviderCommon, ProviderConfigError, ProviderEid, ProviderError, ProviderExpiry, ProviderKind,
    ProviderRequest, ProviderResponse, ProviderUid, SourceCapability, SourceInput,
};
use url::Url;

use crate::common::{bounded_expiry, is_routable_ip, join_section_ids};

/// `atype` `LiveIntent` IDs carry: a person-based ID.
const LIVEINTENT_ATYPE: i32 = 3;

/// The attribute holding `LiveIntent`'s own ID; the others are partner IDs.
const NON_ID_ATTRIBUTE: &str = "nonId";

/// UID `ext` Prebid puts on partner IDs that `LiveIntent` resolved.
const PARTNER_UID_EXT: &str = r#"{"provider":"liveintent.com"}"#;

/// Prefix of a `LiveIntent`-issued distributor ID, such as `did-0123`.
const DISTRIBUTOR_ID_PREFIX: &str = "did-";

/// Most UIDs accepted per `LiveIntent` EID.
const MAX_UIDS_PER_EID: NonZeroUsize = NonZeroUsize::new(1).expect("should be non-zero");

/// The `LiveIntent` provider kind.
pub struct LiveIntent;

/// Settings for a `liveintent` provider.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LiveIntentSettings {
    /// The `LiveIntent` publisher ID; `LiveIntent` accepts `any`.
    #[serde(default = "default_publisher_id")]
    pub publisher_id: String,
    /// The `LiveIntent`-issued distributor ID, such as `did-0123`, when the
    /// publisher has one.
    #[serde(default)]
    pub distributor_id: Option<String>,
    /// Map of `LiveIntent` attribute name to the source it is stored under.
    ///
    /// Every source must be one of the provider's configured `sources`.
    pub attributes: BTreeMap<String, String>,
}

fn default_publisher_id() -> String {
    "any".to_owned()
}

/// A built `LiveIntent` provider.
#[derive(Debug)]
pub struct LiveIntentProvider {
    capabilities: ProviderCapabilities,
    url: Url,
    distributor_id: Option<String>,
    attributes: Box<[(String, SourceDomain)]>,
    partner_uid_ext: &'static RawValue,
}

/// Whether `id` has the `did-` form `LiveIntent` accepts; it answers any
/// other distributor ID with `400 Malformed did`.
fn is_distributor_id(id: &str) -> bool {
    id.strip_prefix(DISTRIBUTOR_ID_PREFIX).is_some_and(|rest| {
        !rest.is_empty() && rest.bytes().all(|byte| byte.is_ascii_alphanumeric())
    })
}

impl ProviderKind for LiveIntent {
    const KIND: &'static str = "liveintent";
    type Settings = LiveIntentSettings;
    type Provider = LiveIntentProvider;

    fn build(
        common: &ProviderCommon<'_>,
        settings: LiveIntentSettings,
    ) -> Result<LiveIntentProvider, Report<ProviderConfigError>> {
        let attributes = settings
            .attributes
            .into_iter()
            .map(|(attribute, source)| {
                let source = common
                    .sources
                    .iter()
                    .find(|known| known.as_str() == source)
                    .ok_or_else(|| {
                        Report::new(ProviderConfigError::Invalid).attach(format!(
                            "attribute `{attribute}` maps to `{source}`, which is not a configured source"
                        ))
                    })?;
                Ok((attribute, source.clone()))
            })
            .collect::<Result<Box<[_]>, Report<ProviderConfigError>>>()?;
        let max_eids = NonZeroUsize::new(attributes.len()).ok_or_else(|| {
            Report::new(ProviderConfigError::Invalid)
                .attach("a liveintent provider needs at least one attribute")
        })?;
        if let Some(id) = settings
            .distributor_id
            .as_deref()
            .filter(|id| !is_distributor_id(id))
        {
            return Err(Report::new(ProviderConfigError::Invalid).attach(format!(
                "distributor_id `{id}` is not a LiveIntent `did-` distributor ID"
            )));
        }
        let partner_uid_ext = serde_json::from_str::<&'static RawValue>(PARTNER_UID_EXT)
            .change_context(ProviderConfigError::Invalid)?;
        let distributor = settings.distributor_id.as_deref().unwrap_or("prebid");
        let mut url = common
            .endpoint
            .ok_or_else(|| {
                Report::new(ProviderConfigError::Invalid)
                    .attach("a liveintent provider needs an endpoint")
            })?
            .clone();
        url.path_segments_mut()
            .map_err(|()| {
                Report::new(ProviderConfigError::Invalid).attach("endpoint cannot hold a path")
            })?
            .pop_if_empty()
            .push(distributor)
            .push(&settings.publisher_id);
        Ok(LiveIntentProvider {
            capabilities: ProviderCapabilities {
                sources: attributes
                    .iter()
                    .map(|(_, source)| SourceCapability {
                        source: source.clone(),
                        input: SourceInput::Anonymous,
                    })
                    .collect(),
                shape: EidShape {
                    max_eids,
                    max_uids_per_eid: MAX_UIDS_PER_EID,
                    eid_level_fields: EidLevelFields::Ignored,
                },
            },
            url,
            distributor_id: settings.distributor_id,
            attributes,
            partner_uid_ext,
        })
    }
}

/// Reads the `Expires` header as a bounded TTL.
///
/// The TTL is `Expires` minus the response's `Date`, so it needs no local
/// clock. It is bounded by [`bounded_expiry`]: a missing or unparseable
/// `Expires` or `Date` gets the default TTL, so a UID never lives forever,
/// and an `Expires` at or before `Date` keeps the UID for the minimum TTL
/// instead of dropping a valid ID.
fn expiry(headers: &HeaderMap) -> ProviderExpiry {
    let date = |name| -> Option<SystemTime> {
        let value = headers.get(name)?.to_str().ok()?;
        httpdate::parse_http_date(value).ok()
    };
    let ttl = date(EXPIRES)
        .zip(date(DATE))
        .map(|(expires, date)| expires.duration_since(date).unwrap_or(Duration::ZERO));
    bounded_expiry(ttl)
}

impl IdentityProvider for LiveIntentProvider {
    fn capabilities(&self) -> &ProviderCapabilities {
        &self.capabilities
    }

    fn build_request(
        &self,
        lookup: &LookupInput<'_>,
    ) -> Result<ProviderRequest, Report<ProviderError>> {
        let consent = lookup.consent;
        let origin = format!("https://{}", lookup.publisher_domain);
        let mut url = self.url.clone();
        {
            let mut query = url.query_pairs_mut();
            if let Some(distributor) = &self.distributor_id {
                query.append_pair("did", distributor);
            }
            query.append_pair("gdpr", if consent.gdpr_applies { "1" } else { "0" });
            if let Some(tc_string) = &consent.raw_tc_string {
                query.append_pair("gdpr_consent", tc_string);
            }
            if let Some(us_privacy) = &consent.raw_us_privacy {
                query.append_pair("us_privacy", us_privacy);
            }
            if let Some(gpp) = &consent.raw_gpp_string {
                query.append_pair("gpp_s", gpp);
            }
            if let Some(ids) = &consent.gpp_section_ids {
                query.append_pair("gpp_as", &join_section_ids(ids));
            }
            query.append_pair("pu", &origin);
            if let Some(ip) = lookup.client.ip.filter(|ip| is_routable_ip(*ip)) {
                let name = if ip.is_ipv4() { "pip" } else { "pip6" };
                query.append_pair(name, &URL_SAFE_NO_PAD.encode(ip.to_string()));
            }
            for (attribute, source) in &self.attributes {
                if lookup.due_sources.contains(source) {
                    query.append_pair("resolve", attribute);
                }
            }
        }
        let mut headers = HeaderMap::new();
        headers.insert(
            ORIGIN,
            HeaderValue::from_str(&origin).change_context(ProviderError::Request)?,
        );
        Ok(ProviderRequest {
            method: Method::GET,
            url,
            headers,
            body: Bytes::new(),
        })
    }

    fn parse_response<'body>(
        &self,
        response: ProviderResponse<'body>,
    ) -> Result<LookupOutcome<'body>, Report<ProviderError>> {
        if response.status == StatusCode::NO_CONTENT {
            return Ok(LookupOutcome::NoMatch);
        }
        if !response.status.is_success() {
            return Err(Report::new(ProviderError::InvalidResponse)
                .attach(format!("LiveIntent returned status {}", response.status)));
        }
        let reply: HashMap<String, &'body RawValue> =
            serde_json::from_slice(response.body).change_context(ProviderError::InvalidResponse)?;
        let expiry = expiry(response.headers);
        let eids: Vec<ProviderEid<'body>> = self
            .attributes
            .iter()
            .filter_map(|(attribute, source)| {
                let raw = reply.get(attribute.as_str())?;
                let id = Cow::<'body, str>::deserialize(*raw)
                    .ok()
                    .filter(|id| !id.is_empty())?;
                Some(ProviderEid {
                    source: Cow::Owned(source.as_str().to_owned()),
                    inserter: None,
                    matcher: None,
                    mm: None,
                    ext: None,
                    uids: vec![ProviderUid {
                        id,
                        atype: Some(LIVEINTENT_ATYPE),
                        ext: (attribute != NON_ID_ATTRIBUTE).then_some(self.partner_uid_ext),
                        expiry,
                    }],
                })
            })
            .collect();
        Ok(if eids.is_empty() {
            LookupOutcome::NoMatch
        } else {
            LookupOutcome::Found(eids)
        })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use trusted_server_core::consent::types::ConsentContext;
    use trusted_server_core::ec::provider::{
        ClientSignals, OwnedEntries, RequestCookies, build_kind,
    };
    use trusted_server_core::settings::{IdentityProviderConfig, ProviderSecrets};

    use super::*;

    fn config(attributes: &serde_json::Value) -> IdentityProviderConfig {
        serde_json::from_value(json!({
            "id": "liveintent",
            "kind": "liveintent",
            "endpoint": "https://li.example.com/idex",
            "allowed_domains": ["li.example.com"],
            "sources": ["li.example.com", "li-partner.example.com"],
            "settings": { "publisher_id": "12345", "attributes": attributes },
        }))
        .expect("should parse provider config")
    }

    fn provider() -> Box<dyn IdentityProvider> {
        build_kind::<LiveIntent>(
            &config(&json!({ "nonId": "li.example.com", "partner": "li-partner.example.com" })),
            &ProviderSecrets::default(),
        )
        .expect("should build provider")
    }

    fn due() -> Vec<SourceDomain> {
        ["li.example.com", "li-partner.example.com"]
            .into_iter()
            .map(|source| SourceDomain::try_from(source).expect("should parse source"))
            .collect()
    }

    #[test]
    fn rejects_an_attribute_mapped_to_an_unconfigured_source() {
        let error = build_kind::<LiveIntent>(
            &config(&json!({ "nonId": "other.example.com" })),
            &ProviderSecrets::default(),
        )
        .err()
        .expect("should reject the config");

        assert!(
            matches!(error.current_context(), ProviderConfigError::Invalid),
            "should report invalid config"
        );
    }

    #[test]
    fn calls_the_exchange_with_consent_ip_and_due_attributes() {
        let provider = provider();
        let consent = ConsentContext {
            raw_us_privacy: Some("1YNN".to_owned()),
            ..ConsentContext::default()
        };
        let due = due();
        let lookup = LookupInput {
            ec_id: "ec-example",
            consent: &consent,
            client: ClientSignals {
                ip: Some("11.22.33.44".parse().expect("should parse ip")),
                user_agent: None,
            },
            publisher_domain: "www.example.com",
            due_sources: &due[..1],
            current: OwnedEntries::new(None, &[]),
            cookies: RequestCookies::default(),
            hashed_email: None,
        };

        let request = provider
            .build_request(&lookup)
            .expect("should build request");
        let query: Vec<(String, String)> = request
            .url
            .query_pairs()
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
        let value = |name: &str| {
            query
                .iter()
                .filter(|(key, _)| key == name)
                .map(|(_, value)| value.as_str())
                .collect::<Vec<_>>()
        };

        assert_eq!(request.method, Method::GET, "should use GET");
        assert_eq!(
            request.url.path(),
            "/idex/prebid/12345",
            "should call the distributor and publisher path"
        );
        assert_eq!(value("gdpr"), ["0"], "should send that GDPR does not apply");
        assert_eq!(value("us_privacy"), ["1YNN"], "should send US privacy");
        assert_eq!(
            value("pu"),
            ["https://www.example.com"],
            "should send the origin"
        );
        assert_eq!(
            value("pip"),
            [URL_SAFE_NO_PAD.encode("11.22.33.44").as_str()],
            "should send the encoded IP"
        );
        assert_eq!(
            value("resolve"),
            ["nonId"],
            "should ask only for due attributes"
        );
    }

    #[test]
    fn maps_returned_attributes_to_sources_with_the_expires_header() {
        let provider = provider();
        let mut headers = HeaderMap::new();
        headers.insert(
            EXPIRES,
            HeaderValue::from_static("Sun, 06 Nov 1994 10:49:37 GMT"),
        );
        headers.insert(
            DATE,
            HeaderValue::from_static("Sun, 06 Nov 1994 08:49:37 GMT"),
        );
        // Escape the `nonId` key as `LiveIntent` may, which a borrowed key
        // cannot hold.
        let body = json!({
            "nonId": "li-example",
            "partner": "partner-example",
            "unknown": "x",
        })
        .to_string()
        .replace("\"nonId\"", "\"non\\u0049d\"")
        .into_bytes();

        let outcome = provider
            .parse_response(ProviderResponse {
                status: StatusCode::OK,
                headers: &headers,
                body: &body,
            })
            .expect("should parse response");
        let LookupOutcome::Found(eids) = outcome else {
            panic!("should find EIDs");
        };

        assert_eq!(eids.len(), 2, "should return each configured attribute");
        assert_eq!(eids[0].source, "li.example.com", "should map nonId");
        assert_eq!(eids[0].uids[0].id, "li-example", "should keep the ID");
        assert_eq!(eids[0].uids[0].atype, Some(3), "should set atype 3");
        assert!(
            matches!(eids[0].uids[0].expiry, ProviderExpiry::After(d) if d == Duration::from_secs(7200)),
            "should use Expires relative to Date"
        );
        assert!(
            eids[0].uids[0].ext.is_none(),
            "should keep nonId without ext"
        );
        assert_eq!(
            eids[1].source, "li-partner.example.com",
            "should map partner"
        );
        assert_eq!(
            eids[1].uids[0].ext.map(RawValue::get),
            Some(r#"{"provider":"liveintent.com"}"#),
            "should name LiveIntent as the provider of a partner ID"
        );
    }

    #[test]
    fn rejects_a_distributor_id_liveintent_did_not_issue() {
        for distributor_id in ["prebid", "did-", "did-01/2"] {
            let mut config = config(&json!({ "nonId": "li.example.com" }));
            config
                .settings
                .insert("distributor_id".to_owned(), json!(distributor_id));

            let error = build_kind::<LiveIntent>(&config, &ProviderSecrets::default())
                .err()
                .expect("should reject the distributor ID");

            assert!(
                matches!(error.current_context(), ProviderConfigError::Invalid),
                "should report invalid config for `{distributor_id}`"
            );
        }
    }

    #[test]
    fn calls_the_distributor_path_with_a_did_distributor_id() {
        let mut config = config(&json!({ "nonId": "li.example.com" }));
        config
            .settings
            .insert("distributor_id".to_owned(), json!("did-0123"));
        let provider = build_kind::<LiveIntent>(&config, &ProviderSecrets::default())
            .expect("should build provider");
        let consent = ConsentContext::default();
        let due = due();
        let lookup = LookupInput {
            ec_id: "ec-example",
            consent: &consent,
            client: ClientSignals {
                ip: None,
                user_agent: None,
            },
            publisher_domain: "www.example.com",
            due_sources: &due,
            current: OwnedEntries::new(None, &[]),
            cookies: RequestCookies::default(),
            hashed_email: None,
        };

        let request = provider
            .build_request(&lookup)
            .expect("should build request");

        assert_eq!(
            request.url.path(),
            "/idex/did-0123/12345",
            "should call the distributor path"
        );
        assert!(
            request
                .url
                .query_pairs()
                .any(|(key, value)| key == "did" && value == "did-0123"),
            "should send the distributor ID"
        );
    }

    #[test]
    fn bounds_a_missing_or_past_expires() {
        let mut past = HeaderMap::new();
        past.insert(
            EXPIRES,
            HeaderValue::from_static("Sun, 06 Nov 1994 08:49:37 GMT"),
        );
        past.insert(
            DATE,
            HeaderValue::from_static("Mon, 07 Nov 1994 08:49:37 GMT"),
        );

        for (headers, expected) in [
            (HeaderMap::new(), crate::common::DEFAULT_TTL),
            (past, crate::common::MIN_TTL),
        ] {
            assert!(
                matches!(expiry(&headers), ProviderExpiry::After(d) if d == expected),
                "should bound the expiry to {expected:?}"
            );
        }
    }

    #[test]
    fn reports_no_match_for_no_content_and_empty_ids() {
        let provider = provider();
        let headers = HeaderMap::new();

        let empty_id = serde_json::to_vec(&json!({ "nonId": "" })).expect("should encode body");
        for (status, body) in [
            (StatusCode::NO_CONTENT, &b""[..]),
            (StatusCode::OK, &empty_id[..]),
        ] {
            let outcome = provider
                .parse_response(ProviderResponse {
                    status,
                    headers: &headers,
                    body,
                })
                .expect("should parse response");
            assert!(
                matches!(outcome, LookupOutcome::NoMatch),
                "should report no match"
            );
        }
    }

    #[test]
    fn rejects_an_error_status_and_a_malformed_body() {
        let provider = provider();
        let headers = HeaderMap::new();

        for (status, body) in [
            (StatusCode::BAD_GATEWAY, &b"{}"[..]),
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
