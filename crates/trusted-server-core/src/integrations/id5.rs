//! ID5 identity provider: resolves the ID5 ID server side.
//!
//! Calls ID5's fetch endpoint with the request's consent, page and network
//! signals, and the hashed email when the JS SDK set one. ID5 returns either
//! complete EIDs under `ids` or the legacy `universal_uid`. The signature ID5
//! returns is kept as private state and sent on the next call.
//!
//! The request field names for the client IP and user agent follow ID5's
//! server-to-server integration and should be confirmed with ID5 for each
//! partner account.

use std::collections::BTreeMap;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use error_stack::Report;
use serde::{Deserialize, Serialize};
use serde_json::json;
use url::Url;
use validator::Validate;

use crate::ec::identity::{
    ConsentPolicy, IdentityProvider, IdentityProviderConfig, IdentityProviderError, Input,
    ProviderDescriptor, ProviderRequest, ProviderResponse, Resolution, ResolveInput, ResolvedEid,
    ResolvedUid, Resolves,
};
use crate::error::TrustedServerError;
use crate::integrations::IntegrationRegistration;
use crate::integrations::vendor_eid::VendorEid;
use crate::integrations::vendor_request::is_routable_ip;
use crate::settings::{IntegrationConfig, Settings};

/// Integration ID of the ID5 provider.
pub(crate) const ID5_INTEGRATION_ID: &str = "id5";

/// The consent purposes ID5's GVL entry lists (GVL version 182).
const ID5_TCF_PURPOSES: [u8; 4] = [1, 3, 5, 10];

/// ID5's IAB Global Vendor List ID.
const ID5_GVL_ID: u16 = 131;

/// The sources ID5 can return. Operators narrow them with
/// `[ec.identity.providers.id5].sources`.
const ID5_SOURCES: &[&str] = &["id5-sync.com", "true-link-id5-sync.com", "euid.eu"];

/// The universal UID ID5 returns for a visitor who opted out at ID5.
const ID5_OPTED_OUT_UID: &str = "0";

/// Config of the `id5` integration.
#[derive(Debug, Clone, Deserialize, Validate)]
pub struct Id5Config {
    /// Enables the provider.
    #[serde(default)]
    pub enabled: bool,
    /// The partner number ID5 assigned to the publisher.
    pub partner_id: u32,
    /// The fetch endpoint; `<partner_id>.json` is appended to its path.
    #[serde(default = "default_endpoint")]
    #[validate(url)]
    pub endpoint: String,
}

fn default_endpoint() -> String {
    "https://id5-sync.com/g/v2".to_owned()
}

impl IntegrationConfig for Id5Config {
    fn is_enabled(&self) -> bool {
        self.enabled
    }
}

impl IdentityProviderConfig for Id5Config {
    fn build_descriptor(&self) -> Result<ProviderDescriptor, Report<IdentityProviderError>> {
        let mut builder = ProviderDescriptor::builder(ID5_INTEGRATION_ID.parse()?);
        for source in ID5_SOURCES {
            builder = builder.with_source(source.parse()?);
        }
        builder
            .with_resolves(
                Resolves::new()
                    .with_endpoint(&self.endpoint)
                    .with_wants([
                        Input::ClientIp,
                        Input::UserAgent,
                        Input::PageUrl,
                        Input::HashedEmail,
                    ])
                    .with_private_state(),
            )
            .with_consent_policy(
                ConsentPolicy::strict()
                    .with_tcf_purposes(ID5_TCF_PURPOSES)
                    .with_tcf_vendor(ID5_GVL_ID),
            )
            .build()
    }
}

/// The ID5 provider.
pub struct Id5Provider {
    descriptor: ProviderDescriptor,
    partner_id: u32,
}

impl Id5Provider {
    /// Builds the provider from its validated config.
    ///
    /// # Errors
    ///
    /// Returns [`IdentityProviderError::Config`] when the config cannot
    /// describe a valid provider.
    pub fn new(config: &Id5Config) -> Result<Self, Report<IdentityProviderError>> {
        Ok(Self {
            descriptor: config.build_descriptor()?,
            partner_id: config.partner_id,
        })
    }
}

/// The signature ID5 returns, sent back on the next call.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Id5State {
    signature: String,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    s: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    u: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tml: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ua: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ip: Option<String>,
}

#[derive(Deserialize)]
struct FetchReply {
    universal_uid: Option<String>,
    signature: Option<String>,
    #[serde(default)]
    ext: Option<serde_json::Value>,
    #[serde(default)]
    ids: BTreeMap<String, Id5Id>,
    cache_control: Option<CacheControl>,
}

#[derive(Deserialize)]
struct Id5Id {
    eid: Option<VendorEid>,
}

#[derive(Deserialize)]
struct CacheControl {
    max_age_sec: Option<u64>,
}

impl IdentityProvider for Id5Provider {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    fn request(
        &self,
        input: &ResolveInput<'_>,
    ) -> Result<Option<ProviderRequest>, Report<IdentityProviderError>> {
        let consent = input.consent();
        let page = input.page_url().ok().map(Url::as_str);
        let body = FetchBody {
            partner: self.partner_id,
            v: env!("CARGO_PKG_VERSION"),
            o: "trusted-server",
            nb_page: 1,
            gdpr: u8::from(consent.gdpr_applies()),
            gdpr_consent: consent.raw_tc_string(),
            us_privacy: consent.raw_us_privacy(),
            gpp_string: consent.raw_gpp_string(),
            gpp_sid: consent.gpp_section_ids().map(join_ids),
            s: input.state::<Id5State>()?.map(|state| state.signature),
            u: page,
            tml: page,
            pd: input
                .hashed_email()
                .ok()
                .map(|email| STANDARD.encode(format!("1={}", email.to_hex()))),
            ua: input.user_agent().ok(),
            ip: input
                .client_ip()
                .ok()
                .filter(|ip| is_routable_ip(*ip))
                .map(|ip| ip.to_string()),
        };
        let mut url = input.endpoint()?.clone();
        url.path_segments_mut()
            .map_err(|_| IdentityProviderError::config("endpoint cannot hold a path"))?
            .pop_if_empty()
            .push(&format!("{}.json", self.partner_id));
        ProviderRequest::post_json(&url, &body)?
            .try_with_header("origin", &format!("https://{}", input.publisher_domain()))
            .map(Some)
    }

    fn response(
        &self,
        response: &ProviderResponse,
        input: &ResolveInput<'_>,
    ) -> Result<Resolution, Report<IdentityProviderError>> {
        let reply: FetchReply = response.success_json()?;
        let primary = input.primary_source();

        let mut eids: Vec<ResolvedEid> = reply
            .ids
            .values()
            .filter_map(|id| {
                let eid = id.eid.as_ref()?;
                let source = self.descriptor.source_named(&eid.source)?;
                eid.to_resolved(source)
            })
            .collect();
        if !eids.iter().any(|eid| eid.source() == primary) {
            match reply.universal_uid.as_deref() {
                Some(ID5_OPTED_OUT_UID) => return Ok(Resolution::invalidate(primary)),
                Some(uid) if !uid.is_empty() => {
                    let mut resolved = ResolvedUid::new(uid).with_atype(1);
                    if let Some(link_type) = reply.ext.as_ref().and_then(|ext| ext.get("linkType"))
                    {
                        resolved = resolved.with_ext(json!({ "linkType": link_type }));
                    }
                    eids.push(ResolvedEid::new(primary, resolved));
                }
                _ => {}
            }
        }
        if let Some(max_age) = reply.cache_control.and_then(|cache| cache.max_age_sec) {
            eids = eids
                .into_iter()
                .map(|eid| eid.with_refresh_in(Duration::from_secs(max_age)))
                .collect();
        }

        let mut resolution = if eids.is_empty() {
            Resolution::no_match()
        } else {
            let returned: Vec<_> = eids.iter().map(|eid| eid.source().clone()).collect();
            let mut found = Resolution::found(eids);
            for (source, _) in input.sources() {
                if !returned.contains(source) && input.stored(source).is_some() {
                    found = found.also_invalidate(source);
                }
            }
            found
        };
        if let Some(signature) = reply.signature {
            resolution = resolution.replacing_state(&Id5State { signature });
        }
        Ok(resolution)
    }
}

fn join_ids(ids: &[u16]) -> String {
    ids.iter().map(u16::to_string).collect::<Vec<_>>().join(",")
}

/// Registers the ID5 provider when `[integrations.id5]` is enabled.
///
/// # Errors
///
/// Returns an error when the config does not parse or describes an invalid
/// provider.
pub fn register(
    settings: &Settings,
) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
    let Some(config) = settings.integration_config::<Id5Config>(ID5_INTEGRATION_ID)? else {
        return Ok(None);
    };
    log::info!(
        "Registering ID5 identity provider (partner={})",
        config.partner_id
    );
    Ok(Some(
        IntegrationRegistration::builder(ID5_INTEGRATION_ID)
            .try_with_identity_provider(Id5Provider::new(&config))?
            .without_js()
            .build(),
    ))
}

#[cfg(test)]
mod tests {
    use http::StatusCode;

    use super::*;
    use crate::ec::identity::test_support::{
        ConformanceCases, HarnessSetup, ProviderHarness, TestConsent,
        check_descriptor_ignores_secrets,
    };
    use crate::ec::identity::{HashedEmail, SourceDomain, StateChange, StoredId, When};

    fn config() -> Id5Config {
        Id5Config {
            enabled: true,
            partner_id: 1234,
            endpoint: default_endpoint(),
        }
    }

    fn provider() -> Id5Provider {
        Id5Provider::new(&config()).expect("should build the provider")
    }

    #[test]
    fn sends_consent_page_and_hashed_email() {
        let provider = provider();
        let mut harness = ProviderHarness::new(&provider)
            .with_client_ip("192.0.2.10")
            .with_user_agent("ExampleBrowser/1.0")
            .with_page_url("https://www.example.com/article")
            .with_hashed_email(HashedEmail::for_test([0xab; 32]))
            .with_consent(TestConsent::default().with_us_privacy("1YNN"))
            .with_state(&Id5State {
                signature: "prior-signature".to_owned(),
            });

        let request = harness
            .request()
            .expect("should build a request")
            .expect("should not skip");
        assert_eq!(
            request.url().as_str(),
            "https://id5-sync.com/g/v2/1234.json",
            "should call the partner's fetch URL"
        );
        let body: serde_json::Value = harness.request_json();

        assert_eq!(body["partner"], 1234, "should send the partner");
        assert_eq!(body["gdpr"], 0, "should send that GDPR does not apply");
        assert_eq!(body["us_privacy"], "1YNN", "should send US privacy");
        assert_eq!(
            body["s"], "prior-signature",
            "should send the stored signature"
        );
        assert_eq!(
            body["u"], "https://www.example.com/article",
            "should send the page"
        );
        assert_eq!(body["ip"], "192.0.2.10", "should send the client IP");
        assert_eq!(
            body["pd"],
            STANDARD.encode(format!("1={}", "ab".repeat(32))),
            "should send the hashed email as partner data"
        );
    }

    #[test]
    fn resolves_complete_eids_and_keeps_the_signature() {
        let provider = provider();
        let mut harness = ProviderHarness::new(&provider);
        harness.request().expect("should build a request");

        let resolution = harness
            .respond(
                StatusCode::OK,
                &json!({
                    "signature": "next-signature",
                    "ids": {
                        "id5": { "eid": {
                            "source": "id5-sync.com",
                            "uids": [{ "id": "ID5*example", "atype": 1, "ext": { "linkType": 2 } }]
                        }},
                        "trueLinkId": { "eid": {
                            "source": "true-link-id5-sync.com",
                            "uids": [{ "id": "true-link-example", "atype": 1 }]
                        }},
                        "other": { "eid": {
                            "source": "other.example.com",
                            "uids": [{ "id": "ignored" }]
                        }}
                    },
                    "cache_control": { "max_age_sec": 7200 }
                }),
            )
            .expect("should parse");

        let sources: Vec<&str> = resolution
            .eids()
            .iter()
            .map(|eid| eid.source().as_str())
            .collect();
        assert_eq!(
            sources,
            ["id5-sync.com", "true-link-id5-sync.com"],
            "should keep only declared sources"
        );
        let state: Id5State = harness.state_of(&resolution).expect("should replace state");
        assert_eq!(
            state.signature, "next-signature",
            "should keep the signature"
        );
    }

    #[test]
    fn falls_back_to_the_universal_uid() {
        let provider = provider();
        let mut harness = ProviderHarness::new(&provider);
        harness.request().expect("should build a request");

        let resolution = harness
            .respond(
                StatusCode::OK,
                &json!({ "universal_uid": "ID5*legacy", "ext": { "linkType": 1 } }),
            )
            .expect("should parse");

        let eid = &resolution.eids()[0];
        assert_eq!(
            eid.source().as_str(),
            "id5-sync.com",
            "should use the primary source"
        );
        assert_eq!(eid.uids()[0].id(), "ID5*legacy", "should keep the UID");
        assert_eq!(
            eid.uids()[0].ext(),
            Some(&json!({ "linkType": 1 })),
            "should keep the link type"
        );
    }

    #[test]
    fn an_opted_out_visitor_invalidates_the_id() {
        let provider = provider();
        let mut harness = ProviderHarness::new(&provider);
        harness.request().expect("should build a request");

        let resolution = harness
            .respond(StatusCode::OK, &json!({ "universal_uid": "0" }))
            .expect("should parse");

        assert_eq!(
            resolution.invalidated()[0].as_str(),
            "id5-sync.com",
            "should invalidate the ID5 ID"
        );
    }

    fn source(domain: &str) -> SourceDomain {
        domain.parse().expect("should parse the source")
    }

    fn requested(harness: &mut ProviderHarness<'_>) -> ProviderRequest {
        harness
            .request()
            .expect("should build a request")
            .expect("should not skip")
    }

    #[test]
    fn leaves_out_every_signal_the_request_lacks() {
        let provider = provider();
        let mut harness = ProviderHarness::new(&provider);
        requested(&mut harness);

        let body: serde_json::Value = harness.request_json();

        for key in [
            "s",
            "pd",
            "ua",
            "ip",
            "u",
            "tml",
            "gdpr_consent",
            "gpp_string",
        ] {
            assert!(body.get(key).is_none(), "should not send `{key}`: {body}");
        }
        assert_eq!(body["o"], "trusted-server", "should name the origin");
        assert_eq!(body["nbPage"], 1, "should send a page count");
    }

    #[test]
    fn leaves_out_a_loopback_or_private_client_ip() {
        for ip in ["127.0.0.1", "10.0.0.8", "::1"] {
            let provider = provider();
            let mut harness = ProviderHarness::new(&provider).with_client_ip(ip);
            requested(&mut harness);

            let body: serde_json::Value = harness.request_json();

            assert!(body.get("ip").is_none(), "should not send {ip}");
        }
    }

    #[test]
    fn sends_an_ipv6_client_ip() {
        let provider = provider();
        let mut harness = ProviderHarness::new(&provider).with_client_ip("2001:db8::7");
        requested(&mut harness);

        let body: serde_json::Value = harness.request_json();

        assert_eq!(
            body["ip"], "2001:db8::7",
            "should send a public IPv6 address"
        );
    }

    #[test]
    fn skips_without_vendor_consent_where_gdpr_applies() {
        let provider = provider();
        let mut harness = ProviderHarness::new(&provider)
            .with_consent(TestConsent::default().with_gdpr_applies(true));

        let request = harness.request().expect("should not fail");

        assert!(request.is_none(), "should not call ID5 without vendor 131");
    }

    #[test]
    fn sends_the_origin_of_the_publisher() {
        let provider = provider();
        let mut harness = ProviderHarness::new(&provider);

        let request = requested(&mut harness);

        let origin = request
            .headers()
            .get("origin")
            .and_then(|value| value.to_str().ok())
            .expect("should send an origin");
        assert!(
            origin.starts_with("https://") && !origin.ends_with('/'),
            "should send an https origin without a path: {origin}"
        );
    }

    #[test]
    fn appends_the_partner_file_to_an_endpoint_with_a_trailing_slash() {
        let provider = Id5Provider::new(&Id5Config {
            endpoint: "https://id5.example.com/custom/".to_owned(),
            ..config()
        })
        .expect("should build the provider");
        let mut harness = ProviderHarness::new(&provider);

        let request = requested(&mut harness);

        assert_eq!(
            request.url().as_str(),
            "https://id5.example.com/custom/1234.json",
            "should not double the slash"
        );
    }

    #[test]
    fn rejects_a_non_success_status() {
        for status in [StatusCode::BAD_REQUEST, StatusCode::INTERNAL_SERVER_ERROR] {
            let provider = provider();
            let mut harness = ProviderHarness::new(&provider);
            requested(&mut harness);

            let error = harness
                .respond(status, &json!({ "universal_uid": "ID5*x" }))
                .expect_err("should reject the status");

            assert!(
                matches!(error.current_context(), IdentityProviderError::Rejected { status: got } if *got == status),
                "should report {status} as rejected: {error:?}"
            );
        }
    }

    #[test]
    fn a_body_that_is_not_json_is_malformed() {
        let provider = provider();
        let mut harness = ProviderHarness::new(&provider);
        requested(&mut harness);

        let error = harness
            .respond_raw(StatusCode::OK, &[], b"<html>")
            .expect_err("should fail");

        assert!(
            matches!(error.current_context(), IdentityProviderError::Malformed),
            "should be malformed: {error:?}"
        );
    }

    #[test]
    fn an_empty_reply_is_no_match() {
        for reply in [
            json!({}),
            json!({ "universal_uid": "" }),
            json!({ "ids": {} }),
        ] {
            let provider = provider();
            let mut harness = ProviderHarness::new(&provider);
            requested(&mut harness);

            let resolution = harness
                .respond(StatusCode::OK, &reply)
                .expect("should parse");

            assert!(resolution.is_no_match(), "should be no match for {reply}");
        }
    }

    #[test]
    fn an_ids_entry_without_uids_falls_back_to_the_universal_uid() {
        let provider = provider();
        let mut harness = ProviderHarness::new(&provider);
        requested(&mut harness);

        let resolution = harness
            .respond(
                StatusCode::OK,
                &json!({
                    "universal_uid": "ID5*legacy",
                    "ids": { "id5": { "eid": { "source": "id5-sync.com", "uids": [{ "id": "" }] } } }
                }),
            )
            .expect("should parse");

        assert_eq!(resolution.eids().len(), 1, "should resolve one entry");
        assert_eq!(
            resolution.eids()[0].uids()[0].id(),
            "ID5*legacy",
            "should use the universal UID"
        );
    }

    #[test]
    fn a_complete_primary_eid_wins_over_an_opted_out_universal_uid() {
        let provider = provider();
        let mut harness = ProviderHarness::new(&provider);
        requested(&mut harness);

        let resolution = harness
            .respond(
                StatusCode::OK,
                &json!({
                    "universal_uid": "0",
                    "ids": { "id5": { "eid": { "source": "id5-sync.com", "uids": [{ "id": "ID5*new" }] } } }
                }),
            )
            .expect("should parse");

        assert!(resolution.invalidated().is_empty(), "should not invalidate");
        assert_eq!(
            resolution.eids()[0].uids()[0].id(),
            "ID5*new",
            "should keep the EID"
        );
    }

    #[test]
    fn cache_control_sets_the_refresh_of_every_entry() {
        let provider = provider();
        let mut harness = ProviderHarness::new(&provider);
        requested(&mut harness);

        let resolution = harness
            .respond(
                StatusCode::OK,
                &json!({ "universal_uid": "ID5*x", "cache_control": { "max_age_sec": 7200 } }),
            )
            .expect("should parse");

        assert_eq!(
            resolution.eids()[0].refresh(),
            Some(When::In(Duration::from_secs(7200))),
            "should refresh after max_age_sec"
        );
    }

    #[test]
    fn without_cache_control_sets_no_refresh() {
        let provider = provider();
        let mut harness = ProviderHarness::new(&provider);
        requested(&mut harness);

        let resolution = harness
            .respond(StatusCode::OK, &json!({ "universal_uid": "ID5*x" }))
            .expect("should parse");

        assert_eq!(
            resolution.eids()[0].refresh(),
            None,
            "should leave refresh to defaults"
        );
    }

    #[test]
    fn a_stored_source_missing_from_the_reply_is_invalidated() {
        let provider = provider();
        let mut harness = ProviderHarness::new(&provider).with_stored(StoredId::for_test(
            &source("true-link-id5-sync.com"),
            "old-true-link",
        ));
        requested(&mut harness);

        let resolution = harness
            .respond(StatusCode::OK, &json!({ "universal_uid": "ID5*x" }))
            .expect("should parse");

        assert_eq!(
            resolution.invalidated(),
            [source("true-link-id5-sync.com")],
            "should remove the stale TrueLink ID"
        );
    }

    #[test]
    fn a_source_neither_stored_nor_returned_is_not_invalidated() {
        let provider = provider();
        let mut harness = ProviderHarness::new(&provider);
        requested(&mut harness);

        let resolution = harness
            .respond(StatusCode::OK, &json!({ "universal_uid": "ID5*x" }))
            .expect("should parse");

        assert!(
            resolution.invalidated().is_empty(),
            "should invalidate nothing"
        );
    }

    #[test]
    fn keeps_a_new_signature_even_without_an_id() {
        let provider = provider();
        let mut harness = ProviderHarness::new(&provider);
        requested(&mut harness);

        let resolution = harness
            .respond(StatusCode::OK, &json!({ "signature": "fresh" }))
            .expect("should parse");

        assert!(resolution.is_no_match(), "should find nothing");
        let state: Id5State = harness.state_of(&resolution).expect("should replace state");
        assert_eq!(state.signature, "fresh", "should keep the signature");
    }

    #[test]
    fn keeps_the_stored_signature_when_the_reply_has_none() {
        let provider = provider();
        let mut harness = ProviderHarness::new(&provider).with_state(&Id5State {
            signature: "prior".to_owned(),
        });
        requested(&mut harness);

        let resolution = harness
            .respond(StatusCode::OK, &json!({ "universal_uid": "ID5*x" }))
            .expect("should parse");

        assert!(
            matches!(resolution.state_change(), StateChange::Keep),
            "should leave the state alone"
        );
    }

    #[test]
    fn unknown_reply_fields_are_ignored() {
        let provider = provider();
        let mut harness = ProviderHarness::new(&provider);
        requested(&mut harness);

        let resolution = harness
            .respond(
                StatusCode::OK,
                &json!({ "universal_uid": "ID5*x", "privacy": { "jurisdiction": "other" }, "future": [1] }),
            )
            .expect("should parse a reply with new fields");

        assert_eq!(resolution.eids().len(), 1, "should still resolve");
    }

    #[test]
    fn descriptor_declares_the_vendor_sources_and_inputs() {
        let descriptor = config().build_descriptor().expect("should build");

        let sources: Vec<&str> = descriptor
            .sources()
            .iter()
            .map(SourceDomain::as_str)
            .collect();
        assert_eq!(sources, ID5_SOURCES, "should declare every ID5 source");
        let resolves = descriptor.resolves().expect("should resolve");
        assert!(
            resolves.wants().contains(&Input::HashedEmail),
            "should want the hashed email"
        );
        assert!(
            resolves.has_private_state(),
            "should keep the signature as state"
        );
    }

    #[test]
    fn config_requires_a_partner_id_and_a_url_endpoint() {
        let missing: Result<Id5Config, _> = serde_json::from_value(json!({ "enabled": true }));
        assert!(missing.is_err(), "should require partner_id");

        let config: Id5Config = serde_json::from_value(
            json!({ "enabled": true, "partner_id": 1, "endpoint": "not a url" }),
        )
        .expect("should parse");
        assert!(
            config.validate().is_err(),
            "should reject a non-URL endpoint"
        );

        let defaulted: Id5Config =
            serde_json::from_value(json!({ "partner_id": 1 })).expect("should parse");
        assert!(!defaulted.is_enabled(), "should default to disabled");
        assert_eq!(
            defaulted.endpoint,
            default_endpoint(),
            "should default the endpoint"
        );
    }

    #[test]
    fn conforms() {
        ProviderHarness::new(&provider()).check_conformance_with_state::<Id5State>(
            &ConformanceCases::default().with_resolve_case(
                HarnessSetup::default().with_client_ip("192.0.2.10"),
                StatusCode::OK,
                &json!({ "universal_uid": "ID5*example", "signature": "s" }),
            ),
        );
    }

    #[test]
    fn descriptor_ignores_secrets() {
        check_descriptor_ignores_secrets(&config(), &config());
    }
}
