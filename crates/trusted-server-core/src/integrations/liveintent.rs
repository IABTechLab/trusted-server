//! `LiveIntent` identity provider: resolves `LiveIntent`'s IDs server side.
//!
//! Calls `LiveIntent`'s identity exchange with the visitor's first-party
//! `LiveIntent` cookie, consent and network signals, and asks for the
//! attributes the operator configured. Each attribute `LiveIntent` returns
//! maps to one `OpenRTB` source.

use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use error_stack::Report;
use http::StatusCode;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use validator::Validate;

use crate::ec::identity::{
    ConsentPolicy, IdentityProvider, IdentityProviderConfig, IdentityProviderError, Input,
    ProviderDescriptor, ProviderRequest, ProviderResponse, Resolution, ResolveInput, ResolvedEid,
    ResolvedUid, Resolves, SourceDomain,
};
use crate::error::TrustedServerError;
use crate::integrations::IntegrationRegistration;
use crate::integrations::vendor_request::is_routable_ip;
use crate::settings::{IntegrationConfig, Settings};

/// Integration ID of the `LiveIntent` provider.
pub(crate) const LIVEINTENT_INTEGRATION_ID: &str = "liveintent";

/// The consent purposes `LiveIntent`'s GVL entry lists (GVL version 182).
const LIVEINTENT_TCF_PURPOSES: [u8; 3] = [1, 2, 7];

/// `LiveIntent`'s IAB Global Vendor List ID.
const LIVEINTENT_GVL_ID: u16 = 148;

/// `LiveIntent`'s first-party ID cookie on the publisher's domain.
const FIRST_PARTY_COOKIE: &str = "_lc2_fpi";

/// The attribute that carries `LiveIntent`'s own ID.
const NON_ID_ATTRIBUTE: &str = "nonId";

/// Each attribute `LiveIntent` resolves and the source it carries.
const ATTRIBUTE_SOURCES: &[(&str, &str)] = &[
    (NON_ID_ATTRIBUTE, "liveintent.com"),
    ("bidswitch", "bidswitch.net"),
    ("medianet", "media.net"),
    ("magnite", "rubiconproject.com"),
    ("index", "liveintent.indexexchange.com"),
    ("openx", "openx.net"),
    ("pubmatic", "pubmatic.com"),
    ("sovrn", "liveintent.sovrn.com"),
    ("sharethrough", "sharethrough.com"),
    ("sonobi", "liveintent.sonobi.com"),
    ("triplelift", "liveintent.triplelift.com"),
    ("zetassp", "zeta-ssp.liveintent.com"),
    ("vidazoo", "liveintent.vidazoo.com"),
    ("nexxen", "liveintent.unrulymedia.com"),
    ("uid2", "uidapi.com"),
    ("thetradedesk", "adserver.org"),
];

/// Config of the `liveintent` integration.
#[derive(Debug, Clone, Deserialize, Validate)]
pub struct LiveIntentConfig {
    /// Enables the provider.
    #[serde(default)]
    pub enabled: bool,
    /// The publisher ID `LiveIntent` assigned, or `any`.
    #[serde(default = "default_publisher_id")]
    #[validate(length(min = 1))]
    pub publisher_id: String,
    /// The distributor ID `LiveIntent` assigned to a distributor of the
    /// integration, if any. It takes the place of `prebid` in the path.
    pub distributor_id: Option<String>,
    /// The identity exchange endpoint.
    #[serde(default = "default_endpoint")]
    #[validate(url)]
    pub endpoint: String,
    /// The attributes to resolve, from the table in this module. Each one's
    /// source needs an `[[ec.partners]]` entry. Default: `nonId`.
    #[serde(default = "default_resolve")]
    #[validate(length(min = 1))]
    pub resolve: Vec<String>,
}

fn default_publisher_id() -> String {
    "any".to_owned()
}

fn default_endpoint() -> String {
    "https://idx.liadm.com/idex".to_owned()
}

fn default_resolve() -> Vec<String> {
    vec![NON_ID_ATTRIBUTE.to_owned()]
}

impl IntegrationConfig for LiveIntentConfig {
    fn is_enabled(&self) -> bool {
        self.enabled
    }
}

impl LiveIntentConfig {
    /// The configured attributes and their sources, in config order.
    fn attributes(&self) -> Result<Vec<(String, SourceDomain)>, Report<IdentityProviderError>> {
        self.resolve
            .iter()
            .map(|attribute| {
                let (name, source) = ATTRIBUTE_SOURCES
                    .iter()
                    .find(|(name, _)| name == attribute)
                    .ok_or_else(|| {
                        IdentityProviderError::config(format!(
                            "unknown LiveIntent attribute `{attribute}`"
                        ))
                    })?;
                Ok(((*name).to_owned(), source.parse()?))
            })
            .collect()
    }
}

impl IdentityProviderConfig for LiveIntentConfig {
    fn build_descriptor(&self) -> Result<ProviderDescriptor, Report<IdentityProviderError>> {
        let mut builder = ProviderDescriptor::builder(LIVEINTENT_INTEGRATION_ID.parse()?);
        for (_, source) in self.attributes()? {
            builder = builder.with_source(source);
        }
        builder
            .with_resolves(
                Resolves::new()
                    .with_endpoint(&self.endpoint)
                    .with_wants([Input::ClientIp, Input::PageUrl])
                    .with_cookie(FIRST_PARTY_COOKIE),
            )
            .with_consent_policy(
                ConsentPolicy::strict()
                    .with_tcf_purposes(LIVEINTENT_TCF_PURPOSES)
                    .with_tcf_vendor(LIVEINTENT_GVL_ID),
            )
            .build()
    }
}

/// The `LiveIntent` provider.
pub struct LiveIntentProvider {
    descriptor: ProviderDescriptor,
    path: String,
    distributor_id: Option<String>,
    attributes: Vec<(String, SourceDomain)>,
}

impl LiveIntentProvider {
    /// Builds the provider from its validated config.
    ///
    /// # Errors
    ///
    /// Returns [`IdentityProviderError::Config`] when the config names an
    /// unknown attribute or cannot describe a valid provider.
    pub fn new(config: &LiveIntentConfig) -> Result<Self, Report<IdentityProviderError>> {
        let distributor = config.distributor_id.as_deref().unwrap_or("prebid");
        Ok(Self {
            descriptor: config.build_descriptor()?,
            path: format!("{distributor}/{}", config.publisher_id),
            distributor_id: config.distributor_id.clone(),
            attributes: config.attributes()?,
        })
    }
}

impl IdentityProvider for LiveIntentProvider {
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
            .extend(self.path.split('/'));
        let mut request = ProviderRequest::get(&url)
            .try_with_header("origin", &format!("https://{}", input.publisher_domain()))?;
        if let Some(distributor) = &self.distributor_id {
            request = request.with_query("did", distributor);
        }
        if let Some(fpid) = input.cookie(FIRST_PARTY_COOKIE)? {
            request = request.with_query("duid", fpid);
        }
        let consent = input.consent();
        request = request.with_query("gdpr", if consent.gdpr_applies() { "1" } else { "0" });
        if let Some(tc_string) = consent.raw_tc_string() {
            request = request.with_query("gdpr_consent", tc_string);
        }
        if let Some(us_privacy) = consent.raw_us_privacy() {
            request = request.with_query("us_privacy", us_privacy);
        }
        if let Some(gpp) = consent.raw_gpp_string() {
            request = request.with_query("gpp_s", gpp);
        }
        if let Some(ids) = consent.gpp_section_ids() {
            let ids: Vec<String> = ids.iter().map(u16::to_string).collect();
            request = request.with_query("gpp_as", &ids.join(","));
        }
        if let Ok(page) = input.page_url() {
            request = request.with_query("pu", page.origin().ascii_serialization().as_str());
        }
        if let Ok(ip) = input.client_ip()
            && is_routable_ip(ip)
        {
            let name = if ip.is_ipv4() { "pip" } else { "pip6" };
            request = request.with_query(name, &URL_SAFE_NO_PAD.encode(ip.to_string()));
        }
        for (attribute, _) in &self.attributes {
            request = request.with_query("resolve", attribute);
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
        let reply: Map<String, Value> = response.success_json()?;
        let segments = reply.get("segments").cloned();
        let lifetime = expires_in(response, input);

        let eids: Vec<ResolvedEid> = self
            .attributes
            .iter()
            .filter(|(_, source)| input.sources().any(|(wanted, _)| wanted == source))
            .filter_map(|(attribute, source)| {
                let id = reply.get(attribute)?.as_str().filter(|id| !id.is_empty())?;
                let mut eid = ResolvedEid::new(source, resolved_uid(attribute, id));
                if attribute == NON_ID_ATTRIBUTE
                    && let Some(segments) = &segments
                {
                    eid = eid.with_ext(json!({ "segments": segments }));
                }
                if attribute == "thetradedesk" {
                    eid = eid.with_matcher("liveintent.com");
                }
                if let Some(lifetime) = lifetime {
                    eid = eid.with_expires_in(lifetime);
                }
                Some(eid)
            })
            .collect();
        Ok(if eids.is_empty() {
            Resolution::no_match()
        } else {
            Resolution::found(eids)
        })
    }
}

fn resolved_uid(attribute: &str, id: &str) -> ResolvedUid {
    let uid = ResolvedUid::new(id).with_atype(3);
    match attribute {
        NON_ID_ATTRIBUTE => uid,
        "thetradedesk" => {
            uid.with_ext(json!({ "rtiPartner": "TDID", "provider": "liveintent.com" }))
        }
        _ => uid.with_ext(json!({ "provider": "liveintent.com" })),
    }
}

/// The lifetime `LiveIntent` gives in its `Expires` header, when it is a
/// valid HTTP date in the future.
fn expires_in(response: &ProviderResponse, input: &ResolveInput<'_>) -> Option<Duration> {
    let expires = response.header("expires")?;
    let at = chrono::DateTime::parse_from_rfc2822(expires)
        .ok()?
        .timestamp();
    let seconds = u64::try_from(at).ok()?.checked_sub(input.now().as_secs())?;
    (seconds > 0).then(|| Duration::from_secs(seconds))
}

/// Registers the `LiveIntent` provider when `[integrations.liveintent]` is
/// enabled.
///
/// # Errors
///
/// Returns an error when the config does not parse or describes an invalid
/// provider.
pub fn register(
    settings: &Settings,
) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
    let Some(config) =
        settings.integration_config::<LiveIntentConfig>(LIVEINTENT_INTEGRATION_ID)?
    else {
        return Ok(None);
    };
    log::info!(
        "Registering LiveIntent identity provider (attributes={})",
        config.resolve.join(",")
    );
    Ok(Some(
        IntegrationRegistration::builder(LIVEINTENT_INTEGRATION_ID)
            .try_with_identity_provider(LiveIntentProvider::new(&config))?
            .without_js()
            .build(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ec::identity::test_support::{
        ConformanceCases, HarnessSetup, ProviderHarness, TestConsent,
        check_descriptor_ignores_secrets,
    };
    use crate::ec::identity::{UnixTime, When};

    fn config(resolve: &[&str]) -> LiveIntentConfig {
        LiveIntentConfig {
            enabled: true,
            publisher_id: "12345".to_owned(),
            distributor_id: None,
            endpoint: default_endpoint(),
            resolve: resolve.iter().map(|name| (*name).to_owned()).collect(),
        }
    }

    fn provider(resolve: &[&str]) -> LiveIntentProvider {
        LiveIntentProvider::new(&config(resolve)).expect("should build the provider")
    }

    #[test]
    fn calls_the_exchange_with_cookie_consent_and_attributes() {
        let provider = provider(&["nonId", "thetradedesk"]);
        let mut harness = ProviderHarness::new(&provider)
            .with_client_ip("192.0.2.10")
            .with_page_url("https://www.example.com/article?x=1")
            .with_cookie(FIRST_PARTY_COOKIE, "fpi-example")
            .with_consent(TestConsent::default().with_us_privacy("1YNN"));

        let request = harness
            .request()
            .expect("should build a request")
            .expect("should not skip");

        let url = request.url();
        assert_eq!(
            url.path(),
            "/idex/prebid/12345",
            "should address the publisher"
        );
        assert_eq!(
            request
                .headers()
                .get("origin")
                .map(http::HeaderValue::as_bytes),
            Some(b"https://www.example.com".as_slice()),
            "should send the publisher origin, which LiveIntent requires"
        );
        let query: Vec<(String, String)> = url.query_pairs().into_owned().collect();
        let value = |name: &str| {
            query
                .iter()
                .filter(|(key, _)| key == name)
                .map(|(_, value)| value.as_str())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            value("duid"),
            ["fpi-example"],
            "should send the first-party ID"
        );
        assert_eq!(value("us_privacy"), ["1YNN"], "should send US privacy");
        assert_eq!(
            value("pu"),
            ["https://www.example.com"],
            "should send the page origin"
        );
        assert_eq!(
            value("pip"),
            [URL_SAFE_NO_PAD.encode("192.0.2.10")],
            "should send the encoded IP"
        );
        assert_eq!(
            value("resolve"),
            ["nonId", "thetradedesk"],
            "should ask for each configured attribute"
        );
    }

    #[test]
    fn leaves_out_a_loopback_client_ip() {
        let provider = provider(&["nonId"]);
        let mut harness = ProviderHarness::new(&provider).with_client_ip("127.0.0.1");

        let request = harness
            .request()
            .expect("should build a request")
            .expect("should not skip");

        assert!(
            !request
                .url()
                .query_pairs()
                .any(|(key, _)| key == "pip" || key == "pip6"),
            "should not send an address LiveIntent cannot match"
        );
    }

    #[test]
    fn maps_attributes_to_sources() {
        let provider = provider(&["nonId", "thetradedesk", "pubmatic"]);
        let mut harness = ProviderHarness::new(&provider);
        harness.request().expect("should build a request");

        let resolution = harness
            .respond(
                StatusCode::OK,
                &json!({
                    "nonId": "li-example",
                    "segments": ["s1"],
                    "thetradedesk": "tdid-example",
                    "bidswitch": "not-configured"
                }),
            )
            .expect("should parse");

        let eids = resolution.eids();
        assert_eq!(
            eids.len(),
            2,
            "should map only configured attributes it returned"
        );
        assert_eq!(
            eids[0].source().as_str(),
            "liveintent.com",
            "should map nonId"
        );
        assert_eq!(
            eids[0].ext(),
            Some(&json!({ "segments": ["s1"] })),
            "should keep segments"
        );
        assert_eq!(
            eids[1].source().as_str(),
            "adserver.org",
            "should map the TDID"
        );
        assert_eq!(
            eids[1].matcher(),
            Some("liveintent.com"),
            "should name the matcher"
        );
    }

    #[test]
    fn no_content_is_no_match() {
        let provider = provider(&["nonId"]);
        let mut harness = ProviderHarness::new(&provider);
        harness.request().expect("should build a request");

        let resolution = harness
            .respond_raw(StatusCode::NO_CONTENT, &[], b"")
            .expect("should accept 204");

        assert!(resolution.is_no_match(), "should be no match");
    }

    #[test]
    fn rejects_an_unknown_attribute() {
        assert!(
            LiveIntentProvider::new(&config(&["unknown"])).is_err(),
            "should refuse an attribute outside the table"
        );
    }

    /// 2026-01-01T00:00:00Z.
    const NOW: u64 = 1_767_225_600;

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
    fn sends_the_distributor_in_the_path_and_as_did_only_when_configured() {
        let distributed = LiveIntentProvider::new(&LiveIntentConfig {
            distributor_id: Some("did-0000".to_owned()),
            ..config(&["nonId"])
        })
        .expect("should build the provider");
        let mut harness = ProviderHarness::new(&distributed);

        let request = requested(&mut harness);

        assert_eq!(
            request.url().path(),
            "/idex/did-0000/12345",
            "should address the distributor"
        );
        assert_eq!(
            query_values(&request, "did"),
            ["did-0000"],
            "should send did"
        );

        let plain = provider(&["nonId"]);
        let mut harness = ProviderHarness::new(&plain);
        let request = requested(&mut harness);
        assert!(
            query_values(&request, "did").is_empty(),
            "should not send did without a distributor, which LiveIntent rejects"
        );
    }

    #[test]
    fn defaults_the_publisher_to_any() {
        let config: LiveIntentConfig =
            serde_json::from_value(json!({ "enabled": true })).expect("should parse");
        let provider = LiveIntentProvider::new(&config).expect("should build");
        let mut harness = ProviderHarness::new(&provider);

        let request = requested(&mut harness);

        assert_eq!(
            request.url().path(),
            "/idex/prebid/any",
            "should use the defaults"
        );
        assert_eq!(
            query_values(&request, "resolve"),
            ["nonId"],
            "should ask for nonId only"
        );
    }

    #[test]
    fn leaves_out_signals_the_request_lacks() {
        let provider = provider(&["nonId"]);
        let mut harness = ProviderHarness::new(&provider);

        let request = requested(&mut harness);

        for name in [
            "duid",
            "pu",
            "pip",
            "pip6",
            "gdpr_consent",
            "us_privacy",
            "gpp_s",
            "gpp_as",
        ] {
            assert!(
                query_values(&request, name).is_empty(),
                "should not send `{name}`"
            );
        }
        assert_eq!(
            query_values(&request, "gdpr"),
            ["0"],
            "should always send gdpr"
        );
    }

    #[test]
    fn sends_an_ipv6_client_ip_as_pip6() {
        let provider = provider(&["nonId"]);
        let mut harness = ProviderHarness::new(&provider).with_client_ip("2001:db8::7");

        let request = requested(&mut harness);

        assert_eq!(
            query_values(&request, "pip6"),
            [URL_SAFE_NO_PAD.encode("2001:db8::7")],
            "should send the encoded IPv6 address"
        );
        assert!(
            query_values(&request, "pip").is_empty(),
            "should not send pip"
        );
    }

    #[test]
    fn leaves_out_private_and_mapped_loopback_addresses() {
        for ip in ["10.1.2.3", "192.168.0.4", "fd00::1", "::ffff:127.0.0.1"] {
            let provider = provider(&["nonId"]);
            let mut harness = ProviderHarness::new(&provider).with_client_ip(ip);

            let request = requested(&mut harness);

            assert!(
                query_values(&request, "pip").is_empty()
                    && query_values(&request, "pip6").is_empty(),
                "should not send {ip}"
            );
        }
    }

    #[test]
    fn sends_only_the_page_origin_never_its_path_or_query() {
        let provider = provider(&["nonId"]);
        let mut harness = ProviderHarness::new(&provider)
            .with_page_url("https://www.example.com:8443/private/path?email=a%40example.com#x");

        let request = requested(&mut harness);

        assert_eq!(
            query_values(&request, "pu"),
            ["https://www.example.com:8443"],
            "should send the origin only"
        );
    }

    #[test]
    fn skips_without_vendor_consent_where_gdpr_applies() {
        let provider = provider(&["nonId"]);
        let mut harness = ProviderHarness::new(&provider)
            .with_consent(TestConsent::default().with_gdpr_applies(true));

        assert!(
            harness.request().expect("should not fail").is_none(),
            "should not call LiveIntent without vendor 148"
        );
    }

    #[test]
    fn partner_ids_carry_the_provider_and_atype_3() {
        let provider = provider(&["nonId", "pubmatic", "uid2"]);
        let mut harness = ProviderHarness::new(&provider);
        requested(&mut harness);

        let resolution = harness
            .respond(
                StatusCode::OK,
                &json!({ "nonId": "li-1", "pubmatic": "pm-1", "uid2": "uid2-1" }),
            )
            .expect("should parse");

        let eids = resolution.eids();
        assert_eq!(eids.len(), 3, "should map all three");
        assert_eq!(
            eids[0].uids()[0].ext(),
            None,
            "nonId should carry no provider ext"
        );
        for eid in &eids[1..] {
            let uid = &eid.uids()[0];
            assert_eq!(
                uid.atype(),
                Some(3),
                "should mark {} as atype 3",
                eid.source()
            );
            assert_eq!(
                uid.ext(),
                Some(&json!({ "provider": "liveintent.com" })),
                "should name LiveIntent as provider for {}",
                eid.source()
            );
        }
        assert_eq!(
            eids[2].source().as_str(),
            "uidapi.com",
            "should map uid2 to uidapi.com"
        );
    }

    #[test]
    fn the_tdid_carries_the_rti_partner() {
        let provider = provider(&["thetradedesk"]);
        let mut harness = ProviderHarness::new(&provider);
        requested(&mut harness);

        let resolution = harness
            .respond(StatusCode::OK, &json!({ "thetradedesk": "tdid-1" }))
            .expect("should parse");

        assert_eq!(
            resolution.eids()[0].uids()[0].ext(),
            Some(&json!({ "rtiPartner": "TDID", "provider": "liveintent.com" })),
            "should match Prebid's TDID shape"
        );
    }

    #[test]
    fn ignores_empty_and_non_string_values() {
        let provider = provider(&["nonId", "pubmatic", "openx"]);
        let mut harness = ProviderHarness::new(&provider);
        requested(&mut harness);

        let resolution = harness
            .respond(
                StatusCode::OK,
                &json!({ "nonId": "", "pubmatic": 42, "openx": null }),
            )
            .expect("should parse");

        assert!(resolution.is_no_match(), "should find nothing usable");
    }

    #[test]
    fn segments_attach_only_to_the_non_id() {
        let provider = provider(&["pubmatic"]);
        let mut harness = ProviderHarness::new(&provider);
        requested(&mut harness);

        let resolution = harness
            .respond(
                StatusCode::OK,
                &json!({ "pubmatic": "pm-1", "segments": ["s1"] }),
            )
            .expect("should parse");

        assert_eq!(
            resolution.eids()[0].ext(),
            None,
            "should not put segments on a partner ID"
        );
    }

    #[test]
    fn a_future_expires_header_sets_the_lifetime() {
        let provider = provider(&["nonId"]);
        let mut harness = ProviderHarness::new(&provider).with_now(UnixTime::from_secs(NOW));
        requested(&mut harness);

        let resolution = harness
            .respond_raw(
                StatusCode::OK,
                &[("expires", "Fri, 02 Jan 2026 00:00:00 GMT")],
                br#"{"nonId":"li-1"}"#,
            )
            .expect("should parse");

        assert_eq!(
            resolution.eids()[0].expiry(),
            Some(When::In(Duration::from_secs(86_400))),
            "should expire when LiveIntent says"
        );
    }

    #[test]
    fn a_past_or_invalid_expires_header_sets_no_lifetime() {
        for expires in [
            "Wed, 31 Dec 2025 00:00:00 GMT",
            "Thu, 01 Jan 2026 00:00:00 GMT",
            "soon",
            "0",
        ] {
            let provider = provider(&["nonId"]);
            let mut harness = ProviderHarness::new(&provider).with_now(UnixTime::from_secs(NOW));
            requested(&mut harness);

            let resolution = harness
                .respond_raw(
                    StatusCode::OK,
                    &[("expires", expires)],
                    br#"{"nonId":"li-1"}"#,
                )
                .expect("should parse");

            assert_eq!(
                resolution.eids()[0].expiry(),
                None,
                "should ignore `{expires}`"
            );
        }
    }

    #[test]
    fn rejects_a_non_success_status_other_than_204() {
        for status in [
            StatusCode::BAD_REQUEST,
            StatusCode::TOO_MANY_REQUESTS,
            StatusCode::BAD_GATEWAY,
        ] {
            let provider = provider(&["nonId"]);
            let mut harness = ProviderHarness::new(&provider);
            requested(&mut harness);

            let error = harness
                .respond(status, &json!({ "message": "Malformed did" }))
                .expect_err("should reject");

            assert!(
                matches!(error.current_context(), IdentityProviderError::Rejected { status: got } if *got == status),
                "should report {status}: {error:?}"
            );
        }
    }

    #[test]
    fn a_body_that_is_not_a_json_object_is_malformed() {
        for body in [&b"[]"[..], b"\"x\"", b"not json"] {
            let provider = provider(&["nonId"]);
            let mut harness = ProviderHarness::new(&provider);
            requested(&mut harness);

            let error = harness
                .respond_raw(StatusCode::OK, &[], body)
                .expect_err("should fail");

            assert!(
                matches!(error.current_context(), IdentityProviderError::Malformed),
                "should be malformed for {:?}: {error:?}",
                String::from_utf8_lossy(body)
            );
        }
    }

    #[test]
    fn declares_one_source_per_attribute_and_the_first_party_cookie() {
        let descriptor = config(&["nonId", "medianet", "nexxen"])
            .build_descriptor()
            .expect("should build");

        let sources: Vec<&str> = descriptor
            .sources()
            .iter()
            .map(SourceDomain::as_str)
            .collect();
        assert_eq!(
            sources,
            ["liveintent.com", "media.net", "liveintent.unrulymedia.com"],
            "should declare the configured attributes' sources in order"
        );
        let resolves = descriptor.resolves().expect("should resolve");
        assert!(
            resolves.cookies().contains(FIRST_PARTY_COOKIE),
            "should declare _lc2_fpi"
        );
        assert!(
            !resolves.wants().contains(&Input::HashedEmail),
            "should not want the HEM"
        );
    }

    #[test]
    fn every_attribute_in_the_table_builds() {
        let names: Vec<&str> = ATTRIBUTE_SOURCES.iter().map(|(name, _)| *name).collect();

        let descriptor = config(&names)
            .build_descriptor()
            .expect("should build every attribute");

        assert_eq!(
            descriptor.sources().len(),
            ATTRIBUTE_SOURCES.len(),
            "one source each"
        );
    }

    #[test]
    fn config_rejects_an_empty_resolve_list_and_a_bad_endpoint() {
        let empty = LiveIntentConfig {
            resolve: Vec::new(),
            ..config(&["nonId"])
        };
        assert!(empty.validate().is_err(), "should require an attribute");

        let bad = LiveIntentConfig {
            endpoint: "nope".to_owned(),
            ..config(&["nonId"])
        };
        assert!(bad.validate().is_err(), "should require a URL endpoint");
    }

    #[test]
    fn conforms() {
        ProviderHarness::new(&provider(&["nonId"])).check_conformance(
            &ConformanceCases::default().with_resolve_case(
                HarnessSetup::default().with_cookie(FIRST_PARTY_COOKIE, "fpi-example"),
                StatusCode::OK,
                &json!({ "nonId": "li-example" }),
            ),
        );
    }

    #[test]
    fn descriptor_ignores_secrets() {
        check_descriptor_ignores_secrets(&config(&["nonId"]), &config(&["nonId"]));
    }
}
