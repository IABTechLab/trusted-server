//! `LiveRamp` ATS identity provider: resolves and refreshes ATS envelopes
//! server side.
//!
//! An initial lookup sends the hashed email from the `ts-hem` cookie. A
//! refresh sends the stored envelope. Envelopes expire after 15 days in
//! California and 30 days elsewhere. The response shape and the consent
//! parameter names are assumed (spec 12.1) and should be confirmed with
//! `LiveRamp`.

use std::time::Duration;

use error_stack::Report;
use http::StatusCode;
use serde::{Deserialize, Serialize};
use validator::Validate;

use crate::ec::identity::{
    ConsentPolicy, IdentityProvider, IdentityProviderConfig, IdentityProviderError, Input,
    ProviderDescriptor, ProviderRequest, ProviderResponse, Resolution, ResolveInput, ResolveKind,
    ResolvedEid, ResolvedUid, Resolves,
};
use crate::error::TrustedServerError;
use crate::integrations::IntegrationRegistration;
use crate::integrations::vendor_request::is_routable_ip;
use crate::settings::{IntegrationConfig, Settings};

/// Integration ID of the `LiveRamp` provider.
pub(crate) const LIVERAMP_INTEGRATION_ID: &str = "liveramp";

/// The consent purposes `LiveRamp`'s GVL entry lists (GVL version 182).
const LIVERAMP_TCF_PURPOSES: [u8; 10] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10];

/// `LiveRamp`'s IAB Global Vendor List ID.
const LIVERAMP_GVL_ID: u16 = 97;

/// The regular ATS envelope product.
const ATS_ENVELOPE_TYPE: u32 = 19;
const ATS_ENVELOPE_SOURCE: &str = "envelopeLiveramp";

/// Identifier types: 4 is a hashed email, 19 an envelope refresh.
const IT_HASHED_EMAIL: &str = "4";
const IT_ENVELOPE: &str = "19";

/// Consent type 4: an IAB TCF v2 string.
const CT_TCF: &str = "4";

const SECONDS_PER_DAY: u64 = 86_400;

/// Config of the `liveramp` integration.
#[derive(Debug, Clone, Deserialize, Validate)]
pub struct LiveRampConfig {
    /// Enables the provider.
    #[serde(default)]
    pub enabled: bool,
    /// The ATS envelope endpoint.
    #[serde(default = "default_endpoint")]
    #[validate(url)]
    pub endpoint: String,
    /// `LiveRamp` placement ID. An operator identifier, not a secret.
    #[validate(length(min = 1))]
    pub placement_id: String,
    /// Countries the placement is approved for, as ISO 3166-1 alpha-2.
    #[validate(length(min = 1))]
    pub approved_countries: Vec<String>,
}

fn default_endpoint() -> String {
    "https://api.rlcdn.com/api/identity/v2/envelope".to_owned()
}

impl IntegrationConfig for LiveRampConfig {
    fn is_enabled(&self) -> bool {
        self.enabled
    }
}

impl IdentityProviderConfig for LiveRampConfig {
    fn build_descriptor(&self) -> Result<ProviderDescriptor, Report<IdentityProviderError>> {
        ProviderDescriptor::builder(LIVERAMP_INTEGRATION_ID.parse()?)
            .with_source("liveramp.com".parse()?)
            .with_expiring_ids()
            .with_resolves(
                Resolves::new()
                    .with_endpoint(&self.endpoint)
                    .with_needs([Input::ClientIp, Input::Region])
                    .with_needs_for(ResolveKind::Initial, [Input::HashedEmail]),
            )
            .with_consent_policy(
                ConsentPolicy::strict()
                    .with_tcf_purposes(LIVERAMP_TCF_PURPOSES)
                    .with_tcf_vendor(LIVERAMP_GVL_ID),
            )
            .build()
    }
}

/// The `LiveRamp` ATS provider.
pub struct LiveRampProvider {
    descriptor: ProviderDescriptor,
    placement_id: String,
    approved_countries: Vec<String>,
}

impl LiveRampProvider {
    /// Builds the provider from its validated config.
    ///
    /// # Errors
    ///
    /// Returns [`IdentityProviderError::Config`] when the config cannot
    /// describe a valid provider.
    pub fn new(config: &LiveRampConfig) -> Result<Self, Report<IdentityProviderError>> {
        Ok(Self {
            descriptor: config.build_descriptor()?,
            placement_id: config.placement_id.clone(),
            approved_countries: config
                .approved_countries
                .iter()
                .map(|country| country.to_ascii_uppercase())
                .collect(),
        })
    }
}

/// Whether the call was an initial lookup or a refresh, carried from
/// `request` to `response`: a 204 means different things for each.
#[derive(Debug, Serialize, Deserialize)]
enum LiveRampCall {
    Initial,
    Refresh,
}

#[derive(Deserialize)]
struct EnvelopeReply {
    #[serde(default)]
    envelopes: Vec<EnvelopeProduct>,
}

#[derive(Deserialize)]
struct EnvelopeProduct {
    #[serde(rename = "type")]
    product_type: u32,
    source: String,
    value: String,
}

impl IdentityProvider for LiveRampProvider {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    fn request(
        &self,
        input: &ResolveInput<'_>,
    ) -> Result<Option<ProviderRequest>, Report<IdentityProviderError>> {
        let region = input.region()?;
        if !self
            .approved_countries
            .iter()
            .any(|country| country == region.country())
        {
            return Ok(None);
        }
        let source = input.primary_source();
        let (call, identifier_type, identifier_value) = match input.stored(source) {
            Some(stored) if input.is_refresh(source) => {
                (LiveRampCall::Refresh, IT_ENVELOPE, stored.uid().to_owned())
            }
            _ => (
                LiveRampCall::Initial,
                IT_HASHED_EMAIL,
                input.hashed_email()?.to_hex(),
            ),
        };
        let consent = input.consent();
        let mut request = ProviderRequest::get(input.endpoint()?)
            .with_query("pid", &self.placement_id)
            .with_query("it", identifier_type)
            .with_query("iv", &identifier_value)
            .with_query("atype", "3")
            .try_with_call_context(&call)?
            .try_with_header("origin", &format!("https://{}", input.publisher_domain()))?;
        let client_ip = input.client_ip()?;
        if is_routable_ip(client_ip) {
            request = request.try_with_header("x-forwarded-for", &client_ip.to_string())?;
        }
        if let Some(tc_string) = consent.raw_tc_string() {
            request = request.with_query("ct", CT_TCF).with_query("cv", tc_string);
        }
        if let Some(gpp) = consent.raw_gpp_string() {
            request = request.with_query("gpp", gpp);
        }
        if let Some(ids) = consent.gpp_section_ids() {
            let ids: Vec<String> = ids.iter().map(u16::to_string).collect();
            request = request.with_query("gpp_sid", &ids.join(","));
        }
        Ok(Some(request))
    }

    fn response(
        &self,
        response: &ProviderResponse,
        input: &ResolveInput<'_>,
    ) -> Result<Resolution, Report<IdentityProviderError>> {
        let source = input.primary_source();
        let call: LiveRampCall = response
            .call_context()?
            .ok_or_else(|| IdentityProviderError::config("missing call context"))?;
        if response.status() == StatusCode::NO_CONTENT {
            return Ok(match call {
                LiveRampCall::Refresh => Resolution::invalidate(source),
                LiveRampCall::Initial => Resolution::no_match(),
            });
        }
        let reply: EnvelopeReply = response.success_json()?;
        let Some(envelope) = reply.envelopes.into_iter().find(|product| {
            product.product_type == ATS_ENVELOPE_TYPE
                && product.source == ATS_ENVELOPE_SOURCE
                && !product.value.is_empty()
        }) else {
            return Ok(Resolution::no_match());
        };
        let lifetime_days = if input.region()?.is("US", "CA") {
            15
        } else {
            30
        };
        let eid = ResolvedEid::new(source, ResolvedUid::new(envelope.value).with_atype(3))
            .with_expires_in(Duration::from_secs(lifetime_days * SECONDS_PER_DAY));
        Ok(Resolution::found([eid]))
    }
}

/// Registers the `LiveRamp` provider when `[integrations.liveramp]` is
/// enabled.
///
/// # Errors
///
/// Returns an error when the config does not parse or describes an invalid
/// provider.
pub fn register(
    settings: &Settings,
) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
    let Some(config) = settings.integration_config::<LiveRampConfig>(LIVERAMP_INTEGRATION_ID)?
    else {
        return Ok(None);
    };
    log::info!(
        "Registering LiveRamp ATS identity provider (countries={})",
        config.approved_countries.join(",")
    );
    Ok(Some(
        IntegrationRegistration::builder(LIVERAMP_INTEGRATION_ID)
            .try_with_identity_provider(LiveRampProvider::new(&config))?
            .without_js()
            .build(),
    ))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::ec::identity::test_support::{
        ConformanceCases, HarnessSetup, ProviderHarness, TestConsent,
        check_descriptor_ignores_secrets,
    };
    use crate::ec::identity::{HashedEmail, ResolveKind, SourceDomain, StoredId};

    fn config() -> LiveRampConfig {
        LiveRampConfig {
            enabled: true,
            endpoint: "https://ats.example.com/api/identity/v2/envelope".to_owned(),
            placement_id: "placement-1".to_owned(),
            approved_countries: vec!["us".to_owned()],
        }
    }

    fn provider() -> LiveRampProvider {
        LiveRampProvider::new(&config()).expect("should build the provider")
    }

    fn source() -> SourceDomain {
        "liveramp.com".parse().expect("should parse the source")
    }

    fn query(request: &ProviderRequest, name: &str) -> Option<String> {
        request
            .url()
            .query_pairs()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.into_owned())
    }

    #[test]
    fn initial_lookup_sends_the_hashed_email() {
        let provider = provider();
        let mut harness = ProviderHarness::new(&provider)
            .with_client_ip("192.0.2.10")
            .with_region("US", Some("NY"))
            .with_hashed_email(HashedEmail::for_test([0xab; 32]));

        let request = harness
            .request()
            .expect("should build a request")
            .expect("should not skip");

        assert_eq!(
            query(&request, "it").as_deref(),
            Some("4"),
            "should be a HEM lookup"
        );
        assert_eq!(
            query(&request, "iv"),
            Some("ab".repeat(32)),
            "should send the HEM hex"
        );
        assert_eq!(
            request
                .headers()
                .get("x-forwarded-for")
                .map(http::HeaderValue::as_bytes),
            Some(b"192.0.2.10".as_slice()),
            "should forward the client IP"
        );
    }

    #[test]
    fn skips_unapproved_countries() {
        let provider = provider();
        let mut harness = ProviderHarness::new(&provider)
            .with_client_ip("192.0.2.10")
            .with_region("FR", None)
            .with_hashed_email(HashedEmail::for_test([0xab; 32]));

        assert!(
            harness.request().expect("should not fail").is_none(),
            "should not call LiveRamp outside approved countries"
        );
    }

    #[test]
    fn california_envelopes_expire_after_15_days() {
        let provider = provider();
        let mut harness = ProviderHarness::new(&provider)
            .with_client_ip("192.0.2.10")
            .with_region("US", Some("CA"))
            .with_hashed_email(HashedEmail::for_test([0xab; 32]));
        harness.request().expect("should build a request");

        let resolution = harness
            .respond(
                StatusCode::OK,
                &json!({ "envelopes": [
                    { "type": 19, "source": "envelopeLiveramp", "value": "envelope-example" }
                ]}),
            )
            .expect("should parse");

        let eid = &resolution.eids()[0];
        assert_eq!(
            eid.uids()[0].id(),
            "envelope-example",
            "should keep the envelope"
        );
        assert_eq!(
            eid.expiry().map(|when| harness.resolve(when)),
            Some(crate::ec::identity::UnixTime::from_secs(
                1_760_000_000 + 15 * SECONDS_PER_DAY
            )),
            "should expire after 15 days in California"
        );
    }

    #[test]
    fn a_refused_refresh_invalidates_the_envelope() {
        let provider = provider();
        let mut harness = ProviderHarness::new(&provider)
            .with_client_ip("192.0.2.10")
            .with_region("US", Some("NY"))
            .with_stored(
                StoredId::for_test(&source(), "old-envelope")
                    .expiring_at(crate::ec::identity::UnixTime::from_secs(1)),
            );

        let request = harness
            .request()
            .expect("should build a request")
            .expect("should not skip");
        assert_eq!(
            query(&request, "it").as_deref(),
            Some("19"),
            "should be a refresh"
        );

        let resolution = harness
            .respond_raw(StatusCode::NO_CONTENT, &[], b"")
            .expect("should accept 204");
        assert_eq!(
            resolution.invalidated(),
            [source()],
            "should invalidate the envelope"
        );
    }

    fn us_visitor(provider: &LiveRampProvider) -> ProviderHarness<'_> {
        ProviderHarness::new(provider)
            .with_client_ip("192.0.2.10")
            .with_region("US", Some("NY"))
    }

    fn envelope_reply(value: &str) -> serde_json::Value {
        json!({ "envelopes": [{ "type": 19, "source": "envelopeLiveramp", "value": value }] })
    }

    #[test]
    fn forwards_only_a_routable_client_ip() {
        for (ip, forwarded) in [
            ("192.0.2.10", true),
            ("127.0.0.1", false),
            ("10.0.0.3", false),
        ] {
            let provider = provider();
            let mut harness = ProviderHarness::new(&provider)
                .with_client_ip(ip)
                .with_region("US", Some("NY"))
                .with_hashed_email(HashedEmail::for_test([0x01; 32]));

            let request = harness
                .request()
                .expect("should build a request")
                .expect("should not skip");

            assert_eq!(
                request.headers().get("x-forwarded-for").is_some(),
                forwarded,
                "should forward {ip} only when it is routable"
            );
        }
    }

    #[test]
    fn an_initial_lookup_without_a_hashed_email_is_skipped() {
        let provider = provider();
        let mut harness = us_visitor(&provider);

        assert!(
            harness.request().expect("should not fail").is_none(),
            "should not call LiveRamp without a HEM or an envelope"
        );
    }

    #[test]
    fn a_refresh_needs_no_hashed_email_and_sends_the_stored_envelope() {
        let provider = provider();
        let mut harness = us_visitor(&provider).with_stored(
            StoredId::for_test(&source(), "stored-envelope")
                .expiring_at(crate::ec::identity::UnixTime::from_secs(1)),
        );

        let request = harness
            .request()
            .expect("should build a request")
            .expect("should not skip");

        assert_eq!(
            query(&request, "it").as_deref(),
            Some("19"),
            "should refresh"
        );
        assert_eq!(
            query(&request, "iv").as_deref(),
            Some("stored-envelope"),
            "should send the envelope"
        );
    }

    #[test]
    fn sends_the_placement_atype_origin_and_no_cookie() {
        let provider = provider();
        let mut harness =
            us_visitor(&provider).with_hashed_email(HashedEmail::for_test([0x01; 32]));

        let request = harness
            .request()
            .expect("should build a request")
            .expect("should not skip");

        assert_eq!(
            query(&request, "pid").as_deref(),
            Some("placement-1"),
            "should send the placement"
        );
        assert_eq!(
            query(&request, "atype").as_deref(),
            Some("3"),
            "should ask for atype 3"
        );
        assert!(
            request.headers().get("origin").is_some(),
            "should send an origin"
        );
        assert!(
            request.headers().get(http::header::COOKIE).is_none(),
            "should send no cookie"
        );
        for name in ["ct", "cv", "gpp", "gpp_sid"] {
            assert!(
                query(&request, name).is_none(),
                "should not send `{name}` without consent strings"
            );
        }
    }

    #[test]
    fn an_undecodable_gpp_string_fails_closed() {
        let provider = provider();
        let mut harness = us_visitor(&provider)
            .with_hashed_email(HashedEmail::for_test([0x01; 32]))
            .with_consent(TestConsent::default().with_gpp_string("not-gpp", &[7]));

        assert!(
            harness.request().expect("should not fail").is_none(),
            "should not send a HEM under consent it cannot read"
        );
    }

    #[test]
    fn envelopes_outside_california_expire_after_30_days() {
        let provider = provider();
        let mut harness =
            us_visitor(&provider).with_hashed_email(HashedEmail::for_test([0xab; 32]));
        harness.request().expect("should build a request");

        let resolution = harness
            .respond(StatusCode::OK, &envelope_reply("envelope-ny"))
            .expect("should parse");

        let eid = &resolution.eids()[0];
        assert_eq!(
            eid.expiry().map(|when| harness.resolve(when)),
            Some(crate::ec::identity::UnixTime::from_secs(
                1_760_000_000 + 30 * SECONDS_PER_DAY
            )),
            "should expire after 30 days"
        );
        assert_eq!(eid.uids()[0].atype(), Some(3), "should be atype 3");
    }

    #[test]
    fn picks_the_ats_envelope_among_other_products() {
        let provider = provider();
        let mut harness =
            us_visitor(&provider).with_hashed_email(HashedEmail::for_test([0xab; 32]));
        harness.request().expect("should build a request");

        let resolution = harness
            .respond(
                StatusCode::OK,
                &json!({ "envelopes": [
                    { "type": 20, "source": "envelopeLiveramp", "value": "wrong-type" },
                    { "type": 19, "source": "other", "value": "wrong-source" },
                    { "type": 19, "source": "envelopeLiveramp", "value": "" },
                    { "type": 19, "source": "envelopeLiveramp", "value": "the-envelope" }
                ]}),
            )
            .expect("should parse");

        assert_eq!(resolution.eids().len(), 1, "should keep one envelope");
        assert_eq!(
            resolution.eids()[0].uids()[0].id(),
            "the-envelope",
            "should pick type 19"
        );
    }

    #[test]
    fn no_usable_envelope_is_no_match() {
        for reply in [json!({}), json!({ "envelopes": [] }), envelope_reply("")] {
            let provider = provider();
            let mut harness =
                us_visitor(&provider).with_hashed_email(HashedEmail::for_test([0xab; 32]));
            harness.request().expect("should build a request");

            let resolution = harness
                .respond(StatusCode::OK, &reply)
                .expect("should parse");

            assert!(resolution.is_no_match(), "should be no match for {reply}");
        }
    }

    #[test]
    fn a_204_on_an_initial_lookup_is_no_match_not_an_invalidation() {
        let provider = provider();
        let mut harness =
            us_visitor(&provider).with_hashed_email(HashedEmail::for_test([0xab; 32]));
        harness.request().expect("should build a request");

        let resolution = harness
            .respond_raw(StatusCode::NO_CONTENT, &[], b"")
            .expect("should accept 204");

        assert!(resolution.is_no_match(), "should be no match");
        assert!(resolution.invalidated().is_empty(), "should not invalidate");
    }

    #[test]
    fn rejects_errors_and_malformed_envelopes() {
        let provider = provider();
        let mut harness =
            us_visitor(&provider).with_hashed_email(HashedEmail::for_test([0xab; 32]));
        harness.request().expect("should build a request");
        let error = harness
            .respond(StatusCode::INTERNAL_SERVER_ERROR, &json!({}))
            .expect_err("should reject 500");
        assert!(
            matches!(
                error.current_context(),
                IdentityProviderError::Rejected { .. }
            ),
            "should be rejected: {error:?}"
        );

        let mut harness =
            us_visitor(&provider).with_hashed_email(HashedEmail::for_test([0xab; 32]));
        harness.request().expect("should build a request");
        let error = harness
            .respond(StatusCode::OK, &json!({ "envelopes": [{ "type": "19" }] }))
            .expect_err("should fail on a product without fields");
        assert!(
            matches!(error.current_context(), IdentityProviderError::Malformed),
            "should be malformed: {error:?}"
        );
    }

    #[test]
    fn declares_an_expiring_source_and_needs_the_hem_only_initially() {
        let descriptor = config().build_descriptor().expect("should build");

        assert!(descriptor.is_expiring(&source()), "envelopes should expire");
        let resolves = descriptor.resolves().expect("should resolve");
        assert!(
            resolves
                .needs(ResolveKind::Initial)
                .contains(&Input::HashedEmail),
            "an initial lookup should need the HEM"
        );
        assert!(
            !resolves
                .needs(ResolveKind::Refresh)
                .contains(&Input::HashedEmail),
            "a refresh should not need the HEM"
        );
    }

    #[test]
    fn conforms() {
        ProviderHarness::new(&provider()).check_conformance(
            &ConformanceCases::default().with_resolve_case(
                HarnessSetup::default()
                    .with_client_ip("192.0.2.10")
                    .with_region("US", Some("NY"))
                    .with_hashed_email(HashedEmail::for_test([0xab; 32])),
                StatusCode::OK,
                &json!({ "envelopes": [
                    { "type": 19, "source": "envelopeLiveramp", "value": "envelope-example" }
                ]}),
            ),
        );
    }

    #[test]
    fn descriptor_ignores_secrets() {
        check_descriptor_ignores_secrets(&config(), &config());
    }
}
