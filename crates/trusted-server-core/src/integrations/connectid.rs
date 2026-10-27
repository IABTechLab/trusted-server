//! Yahoo `ConnectID` identity provider: resolves `ConnectID` server side.
//!
//! Calls Yahoo's user-profile sync endpoint with the hashed email from the
//! `ts-hem` cookie, the page and the consent strings. Yahoo returns the
//! `ConnectID` and a lifetime in hours.

use std::time::Duration;

use error_stack::Report;
use http::StatusCode;
use serde::Deserialize;
use validator::Validate;

use crate::ec::identity::{
    ConsentPolicy, IdentityProvider, IdentityProviderConfig, IdentityProviderError, Input,
    ProviderDescriptor, ProviderRequest, ProviderResponse, Resolution, ResolveInput, ResolvedEid,
    ResolvedUid, Resolves,
};
use crate::error::TrustedServerError;
use crate::integrations::IntegrationRegistration;
use crate::settings::{IntegrationConfig, Settings};

/// Integration ID of the `ConnectID` provider.
pub(crate) const CONNECTID_INTEGRATION_ID: &str = "connectid";

/// The consent purposes Yahoo's GVL entry lists (GVL version 182).
const YAHOO_TCF_PURPOSES: [u8; 11] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];

/// Yahoo's IAB Global Vendor List ID.
const YAHOO_GVL_ID: u16 = 25;

const SECONDS_PER_HOUR: u64 = 3_600;

/// Config of the `connectid` integration.
#[derive(Debug, Clone, Deserialize, Validate)]
pub struct ConnectIdConfig {
    /// Enables the provider.
    #[serde(default)]
    pub enabled: bool,
    /// The pixel ID Yahoo assigned to the publisher.
    pub pixel_id: u64,
    /// The sync endpoint; the pixel ID and `fed` are appended to its path.
    #[serde(default = "default_endpoint")]
    #[validate(url)]
    pub endpoint: String,
}

fn default_endpoint() -> String {
    "https://ups.analytics.yahoo.com/ups".to_owned()
}

impl IntegrationConfig for ConnectIdConfig {
    fn is_enabled(&self) -> bool {
        self.enabled
    }
}

impl IdentityProviderConfig for ConnectIdConfig {
    fn build_descriptor(&self) -> Result<ProviderDescriptor, Report<IdentityProviderError>> {
        ProviderDescriptor::builder(CONNECTID_INTEGRATION_ID.parse()?)
            .with_source("yahoo.com".parse()?)
            .with_resolves(
                Resolves::new()
                    .with_endpoint(&self.endpoint)
                    .with_needs([Input::HashedEmail])
                    .with_wants([Input::PageUrl]),
            )
            .with_consent_policy(
                ConsentPolicy::strict()
                    .with_tcf_purposes(YAHOO_TCF_PURPOSES)
                    .with_tcf_vendor(YAHOO_GVL_ID),
            )
            .build()
    }
}

/// The Yahoo `ConnectID` provider.
pub struct ConnectIdProvider {
    descriptor: ProviderDescriptor,
    pixel_id: String,
}

impl ConnectIdProvider {
    /// Builds the provider from its validated config.
    ///
    /// # Errors
    ///
    /// Returns [`IdentityProviderError::Config`] when the config cannot
    /// describe a valid provider.
    pub fn new(config: &ConnectIdConfig) -> Result<Self, Report<IdentityProviderError>> {
        Ok(Self {
            descriptor: config.build_descriptor()?,
            pixel_id: config.pixel_id.to_string(),
        })
    }
}

#[derive(Deserialize)]
struct SyncReply {
    #[serde(rename = "connectId")]
    connect_id: Option<String>,
    /// The lowercase spelling Yahoo also sends, often next to `connectId`.
    connectid: Option<String>,
    /// Hours.
    ttl: Option<u64>,
}

impl IdentityProvider for ConnectIdProvider {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    fn request(
        &self,
        input: &ResolveInput<'_>,
    ) -> Result<Option<ProviderRequest>, Report<IdentityProviderError>> {
        let mut url = input.endpoint()?.clone();
        url.path_segments_mut()
            .map_err(|_| IdentityProviderError::config("endpoint cannot hold a path"))?
            .pop_if_empty()
            .push(&self.pixel_id)
            .push("fed");
        let consent = input.consent();
        let mut request = ProviderRequest::get(&url)
            .with_query("v", "1")
            .with_query("1p", "0")
            .with_query("pixelId", &self.pixel_id)
            .with_query("he", &input.hashed_email()?.to_hex())
            .with_query("gdpr", if consent.gdpr_applies() { "1" } else { "0" });
        if let Some(tc_string) = consent.raw_tc_string() {
            request = request.with_query("gdpr_consent", tc_string);
        }
        if let Some(us_privacy) = consent.raw_us_privacy() {
            request = request.with_query("us_privacy", us_privacy);
        }
        if let Some(gpp) = consent.raw_gpp_string() {
            request = request.with_query("gpp", gpp);
        }
        if let Some(ids) = consent.gpp_section_ids() {
            let ids: Vec<String> = ids.iter().map(u16::to_string).collect();
            request = request.with_query("gpp_sid", &ids.join(","));
        }
        if let Ok(page) = input.page_url() {
            request = request.with_query("url", page.as_str());
        }
        Ok(Some(request))
    }

    fn response(
        &self,
        response: &ProviderResponse,
        input: &ResolveInput<'_>,
    ) -> Result<Resolution, Report<IdentityProviderError>> {
        if response.status() == StatusCode::NO_CONTENT {
            return Ok(Resolution::no_match());
        }
        let reply: SyncReply = response.success_json()?;
        let Some(connect_id) = [reply.connect_id, reply.connectid]
            .into_iter()
            .flatten()
            .find(|id| !id.is_empty())
        else {
            return Ok(Resolution::no_match());
        };
        let mut eid = ResolvedEid::new(
            input.primary_source(),
            ResolvedUid::new(connect_id).with_atype(3),
        );
        if let Some(hours) = reply.ttl.filter(|hours| *hours > 0) {
            eid = eid.with_refresh_in(Duration::from_secs(hours.saturating_mul(SECONDS_PER_HOUR)));
        }
        Ok(Resolution::found([eid]))
    }
}

/// Registers the `ConnectID` provider when `[integrations.connectid]` is
/// enabled.
///
/// # Errors
///
/// Returns an error when the config does not parse or describes an invalid
/// provider.
pub fn register(
    settings: &Settings,
) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
    let Some(config) = settings.integration_config::<ConnectIdConfig>(CONNECTID_INTEGRATION_ID)?
    else {
        return Ok(None);
    };
    log::info!(
        "Registering Yahoo ConnectID identity provider (pixel={})",
        config.pixel_id
    );
    Ok(Some(
        IntegrationRegistration::builder(CONNECTID_INTEGRATION_ID)
            .try_with_identity_provider(ConnectIdProvider::new(&config))?
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
    use crate::ec::identity::{HashedEmail, ResolveKind, When};

    fn config() -> ConnectIdConfig {
        ConnectIdConfig {
            enabled: true,
            pixel_id: 58_776,
            endpoint: "https://ups.example.com/ups".to_owned(),
        }
    }

    fn provider() -> ConnectIdProvider {
        ConnectIdProvider::new(&config()).expect("should build the provider")
    }

    #[test]
    fn sends_the_hashed_email_to_the_pixel() {
        let provider = provider();
        let mut harness = ProviderHarness::new(&provider)
            .with_hashed_email(HashedEmail::for_test([0xab; 32]))
            .with_page_url("https://www.example.com/article");

        let request = harness
            .request()
            .expect("should build a request")
            .expect("should not skip");

        assert_eq!(
            request.url().path(),
            "/ups/58776/fed",
            "should address the pixel"
        );
        let he = request
            .url()
            .query_pairs()
            .find(|(key, _)| key == "he")
            .map(|(_, value)| value.into_owned());
        assert_eq!(he, Some("ab".repeat(32)), "should send the HEM hex");
    }

    #[test]
    fn resolves_the_connect_id_with_its_ttl() {
        let provider = provider();
        let mut harness =
            ProviderHarness::new(&provider).with_hashed_email(HashedEmail::for_test([0xab; 32]));
        harness.request().expect("should build a request");

        let resolution = harness
            .respond(
                StatusCode::OK,
                &json!({ "connectId": "cid-example", "connectid": "cid-example", "ttl": 24 }),
            )
            .expect("should parse");

        let eid = &resolution.eids()[0];
        assert_eq!(eid.source().as_str(), "yahoo.com", "should use yahoo.com");
        assert_eq!(eid.uids()[0].id(), "cid-example", "should keep the ID");
        assert!(eid.refresh().is_some(), "should set a refresh from the ttl");
    }

    #[test]
    fn an_empty_reply_is_no_match() {
        let provider = provider();
        let mut harness =
            ProviderHarness::new(&provider).with_hashed_email(HashedEmail::for_test([0xab; 32]));
        harness.request().expect("should build a request");

        let resolution = harness
            .respond(StatusCode::OK, &json!({}))
            .expect("should parse");

        assert!(resolution.is_no_match(), "should be no match");
    }

    fn with_email(provider: &ConnectIdProvider) -> ProviderHarness<'_> {
        ProviderHarness::new(provider).with_hashed_email(HashedEmail::for_test([0x0f; 32]))
    }

    fn requested(harness: &mut ProviderHarness<'_>) -> ProviderRequest {
        harness
            .request()
            .expect("should build a request")
            .expect("should not skip")
    }

    fn query_values(request: &ProviderRequest, name: &str) -> Vec<String> {
        request
            .url()
            .query_pairs()
            .filter(|(key, _)| key == name)
            .map(|(_, value)| value.into_owned())
            .collect()
    }

    #[test]
    fn skips_without_a_hashed_email() {
        let provider = provider();
        let mut harness = ProviderHarness::new(&provider);

        assert!(
            harness.request().expect("should not fail").is_none(),
            "should not call Yahoo without the HEM it needs"
        );
    }

    #[test]
    fn skips_without_vendor_consent_where_gdpr_applies() {
        let provider = provider();
        let mut harness =
            with_email(&provider).with_consent(TestConsent::default().with_gdpr_applies(true));

        assert!(
            harness.request().expect("should not fail").is_none(),
            "should not call Yahoo without vendor 25"
        );
    }

    #[test]
    fn addresses_the_pixel_and_sends_lowercase_hex() {
        let provider = provider();
        let mut harness = with_email(&provider);

        let request = requested(&mut harness);

        assert_eq!(
            request.url().path(),
            "/ups/58776/fed",
            "should address the pixel"
        );
        assert_eq!(
            query_values(&request, "pixelId"),
            ["58776"],
            "should repeat the pixel"
        );
        assert_eq!(
            query_values(&request, "he"),
            ["0f".repeat(32)],
            "should send lowercase hex"
        );
        assert_eq!(
            query_values(&request, "1p"),
            ["0"],
            "should mark a server call"
        );
        assert_eq!(query_values(&request, "gdpr"), ["0"], "should send gdpr");
    }

    #[test]
    fn sends_consent_strings_it_has_and_nothing_else() {
        let provider = provider();
        let mut harness =
            with_email(&provider).with_consent(TestConsent::default().with_us_privacy("1YNN"));

        let request = requested(&mut harness);

        assert_eq!(
            query_values(&request, "us_privacy"),
            ["1YNN"],
            "should send US privacy"
        );
        for name in ["gdpr_consent", "gpp", "gpp_sid", "url"] {
            assert!(
                query_values(&request, name).is_empty(),
                "should not send `{name}`"
            );
        }
    }

    #[test]
    fn sends_the_page_without_its_query() {
        let provider = provider();
        let mut harness = with_email(&provider)
            .with_page_url("https://www.example.com/a/b?email=x%40example.com");

        let request = requested(&mut harness);

        assert_eq!(
            query_values(&request, "url"),
            ["https://www.example.com/a/b"],
            "should never forward the page query"
        );
    }

    #[test]
    fn accepts_either_key_and_prefers_connect_id() {
        for (reply, expected) in [
            (json!({ "connectId": "camel" }), "camel"),
            (json!({ "connectid": "lower" }), "lower"),
            (
                json!({ "connectId": "camel", "connectid": "lower" }),
                "camel",
            ),
            (json!({ "connectId": "", "connectid": "lower" }), "lower"),
        ] {
            let provider = provider();
            let mut harness = with_email(&provider);
            requested(&mut harness);

            let resolution = harness
                .respond(StatusCode::OK, &reply)
                .expect("should parse");

            assert_eq!(
                resolution.eids()[0].uids()[0].id(),
                expected,
                "should pick `{expected}` from {reply}"
            );
        }
    }

    #[test]
    fn the_id_is_a_person_based_atype_3_on_yahoo_com() {
        let provider = provider();
        let mut harness = with_email(&provider);
        requested(&mut harness);

        let resolution = harness
            .respond(StatusCode::OK, &json!({ "connectId": "c1" }))
            .expect("should parse");

        let eid = &resolution.eids()[0];
        assert_eq!(eid.source().as_str(), "yahoo.com", "should write yahoo.com");
        assert_eq!(eid.uids()[0].atype(), Some(3), "should be atype 3");
    }

    #[test]
    fn a_zero_or_missing_ttl_sets_no_refresh() {
        for reply in [
            json!({ "connectId": "c1", "ttl": 0 }),
            json!({ "connectId": "c1" }),
        ] {
            let provider = provider();
            let mut harness = with_email(&provider);
            requested(&mut harness);

            let resolution = harness
                .respond(StatusCode::OK, &reply)
                .expect("should parse");

            assert_eq!(
                resolution.eids()[0].refresh(),
                None,
                "should not refresh for {reply}"
            );
        }
    }

    #[test]
    fn a_huge_ttl_saturates_instead_of_overflowing() {
        let provider = provider();
        let mut harness = with_email(&provider);
        requested(&mut harness);

        let resolution = harness
            .respond(
                StatusCode::OK,
                &json!({ "connectId": "c1", "ttl": u64::MAX }),
            )
            .expect("should parse");

        assert_eq!(
            resolution.eids()[0].refresh(),
            Some(When::In(Duration::from_secs(u64::MAX))),
            "should saturate"
        );
    }

    #[test]
    fn no_content_and_null_ids_are_no_match() {
        let provider = provider();
        let mut harness = with_email(&provider);
        requested(&mut harness);
        let resolution = harness
            .respond_raw(StatusCode::NO_CONTENT, &[], b"")
            .expect("should accept 204");
        assert!(resolution.is_no_match(), "204 should be no match");

        let mut harness = with_email(&provider);
        requested(&mut harness);
        let resolution = harness
            .respond(StatusCode::OK, &json!({ "connectId": null, "ttl": 24 }))
            .expect("should parse");
        assert!(resolution.is_no_match(), "a null ID should be no match");
    }

    #[test]
    fn rejects_errors_and_malformed_bodies() {
        let provider = provider();
        let mut harness = with_email(&provider);
        requested(&mut harness);
        let error = harness
            .respond(StatusCode::FORBIDDEN, &json!({}))
            .expect_err("should reject 403");
        assert!(
            matches!(
                error.current_context(),
                IdentityProviderError::Rejected { .. }
            ),
            "should be rejected: {error:?}"
        );

        let mut harness = with_email(&provider);
        requested(&mut harness);
        let error = harness
            .respond_raw(StatusCode::OK, &[], b"{\"connectId\": 5}")
            .expect_err("should fail on a numeric ID");
        assert!(
            matches!(error.current_context(), IdentityProviderError::Malformed),
            "should be malformed: {error:?}"
        );
    }

    #[test]
    fn declares_the_hem_as_needed() {
        let descriptor = config().build_descriptor().expect("should build");

        let resolves = descriptor.resolves().expect("should resolve");
        assert!(
            resolves
                .needs(ResolveKind::Initial)
                .contains(&Input::HashedEmail),
            "should need the HEM"
        );
        assert_eq!(
            descriptor.consent_policy().tcf_vendor(),
            Some(YAHOO_GVL_ID),
            "should name GVL 25"
        );
    }

    #[test]
    fn conforms() {
        ProviderHarness::new(&provider()).check_conformance(
            &ConformanceCases::default().with_resolve_case(
                HarnessSetup::default().with_hashed_email(HashedEmail::for_test([0xab; 32])),
                StatusCode::OK,
                &json!({ "connectId": "cid-example" }),
            ),
        );
    }

    #[test]
    fn descriptor_ignores_secrets() {
        check_descriptor_ignores_secrets(&config(), &config());
    }
}
