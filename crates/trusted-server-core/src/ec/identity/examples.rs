//! Fictional providers from spec 4.6, for tests of registration, coordination
//! and capture.

use std::time::Duration;

use error_stack::Report;
use http::Method;
use serde::{Deserialize, Serialize};
use serde_json::json;
use url::Url;
use validator::Validate;

use super::descriptor::{Captures, ConsentPolicy, Input, ProviderDescriptor, Resolves};
use super::exchange::{ProviderRequest, ProviderResponse};
use super::ids::UnixTime;
use super::input::{CaptureInput, ResolveInput, StoredId};
use super::provider::{IdentityProvider, IdentityProviderConfig};
use super::resolution::{IdentityProviderError, Resolution, ResolvedEid, ResolvedUid};
use crate::settings::IntegrationConfig;

/// Integration ID of [`ExampleIdProvider`].
pub(crate) const EXAMPLEID_INTEGRATION_ID: &str = "exampleid";

/// Integration ID of [`ExampleCaptureProvider`].
pub(crate) const EXAMPLECAPTURE_INTEGRATION_ID: &str = "examplecapture";

/// Config of the fictional `exampleid` integration.
#[derive(Debug, Clone, Deserialize, Validate)]
pub(crate) struct ExampleIdConfig {
    pub enabled: bool,
    #[validate(url)]
    pub endpoint: String,
    pub partner_id: u32,
    pub gvl_id: u16,
}

impl IntegrationConfig for ExampleIdConfig {
    fn is_enabled(&self) -> bool {
        self.enabled
    }
}

impl IdentityProviderConfig for ExampleIdConfig {
    fn build_descriptor(&self) -> Result<ProviderDescriptor, Report<IdentityProviderError>> {
        ProviderDescriptor::builder(EXAMPLEID_INTEGRATION_ID.parse()?)
            .with_source("exampleid.example.com".parse()?)
            .with_resolves(
                Resolves::new()
                    .with_endpoint(&self.endpoint)
                    .with_needs([Input::ClientIp, Input::UserAgent])
                    .with_wants([Input::PageUrl])
                    .with_private_state(),
            )
            .with_consent_policy(
                ConsentPolicy::strict()
                    .with_tcf_purposes([4])
                    .with_tcf_vendor(self.gvl_id),
            )
            .build()
    }
}

/// A valid [`ExampleIdConfig`].
pub(crate) fn example_config() -> ExampleIdConfig {
    ExampleIdConfig {
        enabled: true,
        endpoint: "https://api.example.com/fetch".to_owned(),
        partner_id: 7,
        gvl_id: 1,
    }
}

/// A resolving provider that round-trips private state.
pub(crate) struct ExampleIdProvider {
    descriptor: ProviderDescriptor,
    partner_id: u32,
}

/// The private state [`ExampleIdProvider`] keeps between calls.
#[derive(Serialize, Deserialize)]
pub(crate) struct ExampleIdState {
    signature: String,
}

#[derive(Serialize)]
struct FetchBody<'a> {
    partner: u32,
    ip: String,
    ua: &'a str,
    page: Option<&'a str>,
    gdpr: bool,
    consent: Option<&'a str>,
    signature: Option<String>,
}

#[derive(Deserialize)]
struct FetchReply {
    uid: Option<String>,
    signature: Option<String>,
    link_type: Option<u8>,
    max_age_sec: Option<u64>,
}

impl ExampleIdProvider {
    /// Builds the provider from its validated config.
    ///
    /// # Errors
    ///
    /// Returns [`IdentityProviderError::Config`] when the config cannot
    /// describe a valid provider.
    pub(crate) fn new(config: &ExampleIdConfig) -> Result<Self, Report<IdentityProviderError>> {
        Ok(Self {
            descriptor: config.build_descriptor()?,
            partner_id: config.partner_id,
        })
    }
}

impl IdentityProvider for ExampleIdProvider {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    fn request(
        &self,
        input: &ResolveInput<'_>,
    ) -> Result<Option<ProviderRequest>, Report<IdentityProviderError>> {
        let consent = input.consent();
        let body = FetchBody {
            partner: self.partner_id,
            ip: input.client_ip()?.to_string(),
            ua: input.user_agent()?,
            page: input.page_url().ok().map(Url::as_str),
            gdpr: consent.gdpr_applies(),
            consent: consent.raw_tc_string(),
            signature: input
                .state::<ExampleIdState>()?
                .map(|state| state.signature),
        };
        ProviderRequest::post_json(input.endpoint()?, &body).map(Some)
    }

    fn response(
        &self,
        response: &ProviderResponse,
        input: &ResolveInput<'_>,
    ) -> Result<Resolution, Report<IdentityProviderError>> {
        let reply: FetchReply = response.success_json()?;
        let Some(uid) = reply.uid else {
            return Ok(Resolution::no_match());
        };

        let mut uid = ResolvedUid::new(uid).with_atype(1);
        if let Some(link_type) = reply.link_type {
            uid = uid.with_ext(json!({ "linkType": link_type }));
        }
        let mut eid = ResolvedEid::new(input.primary_source(), uid);
        if let Some(max_age) = reply.max_age_sec {
            eid = eid.with_refresh_in(Duration::from_secs(max_age));
        }

        let resolution = Resolution::found([eid]);
        Ok(match reply.signature {
            Some(signature) => resolution.replacing_state(&ExampleIdState { signature }),
            None => resolution,
        })
    }
}

/// A capture-only provider that reads tokens from the API it proxies.
pub(crate) struct ExampleCaptureProvider {
    descriptor: ProviderDescriptor,
}

#[derive(Deserialize)]
struct TokenReply {
    tokens: Vec<Token>,
}

#[derive(Deserialize)]
struct Token {
    source: String,
    value: String,
    expires: u64,
}

impl ExampleCaptureProvider {
    /// Builds the provider with the capture path of spec 4.6.
    ///
    /// # Errors
    ///
    /// Returns [`IdentityProviderError::Config`] when the descriptor is invalid.
    pub(crate) fn new() -> Result<Self, Report<IdentityProviderError>> {
        Self::with_capture_path("/integrations/examplecapture/api/token")
    }

    /// Builds the provider capturing from `path` instead.
    ///
    /// # Errors
    ///
    /// Returns [`IdentityProviderError::Config`] when the descriptor is invalid.
    pub(crate) fn with_capture_path(path: &str) -> Result<Self, Report<IdentityProviderError>> {
        let descriptor = ProviderDescriptor::builder(EXAMPLECAPTURE_INTEGRATION_ID.parse()?)
            .with_source("examplecapture.example.com".parse()?)
            .with_captures(Captures::new().with_path(Method::POST, path))
            .build()?;
        Ok(Self { descriptor })
    }
}

impl IdentityProvider for ExampleCaptureProvider {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    fn capture(
        &self,
        input: &CaptureInput<'_>,
    ) -> Result<Resolution, Report<IdentityProviderError>> {
        let Ok(reply) = input.response().json::<TokenReply>() else {
            return Ok(Resolution::no_match());
        };
        Ok(Resolution::found(reply.tokens.into_iter().filter_map(
            |token| {
                let source = input.source_named(&token.source)?;
                Some(
                    ResolvedEid::new(source, ResolvedUid::new(token.value))
                        .with_expires_at(UnixTime::from_secs(token.expires)),
                )
            },
        )))
    }
}

#[cfg(test)]
mod tests {
    use http::header::COOKIE;
    use http::{HeaderValue, StatusCode};

    use super::*;
    use crate::ec::identity::descriptor::{EidProvenance, ProviderDescriptorBuilder};
    use crate::ec::identity::ids::SourceDomain;
    use crate::ec::identity::test_support::{
        CaptureFixture, ConformanceCases, ConformanceFailure, HarnessSetup, ProviderHarness,
        TestConsent, check_descriptor_ignores_secrets, resolver_policy,
    };
    use crate::openrtb::{Eid, Uid};

    type RequestFn =
        fn(&ResolveInput<'_>) -> Result<Option<ProviderRequest>, Report<IdentityProviderError>>;
    type ResponseFn = fn(
        &ProviderResponse,
        &ResolveInput<'_>,
    ) -> Result<Resolution, Report<IdentityProviderError>>;
    type CaptureFn = fn(&CaptureInput<'_>) -> Result<Resolution, Report<IdentityProviderError>>;

    /// A resolver whose behaviour is two plain functions.
    struct TestResolver {
        descriptor: ProviderDescriptor,
        request: RequestFn,
        response: ResponseFn,
    }

    impl IdentityProvider for TestResolver {
        fn descriptor(&self) -> &ProviderDescriptor {
            &self.descriptor
        }

        fn request(
            &self,
            input: &ResolveInput<'_>,
        ) -> Result<Option<ProviderRequest>, Report<IdentityProviderError>> {
            (self.request)(input)
        }

        fn response(
            &self,
            response: &ProviderResponse,
            input: &ResolveInput<'_>,
        ) -> Result<Resolution, Report<IdentityProviderError>> {
            (self.response)(response, input)
        }
    }

    /// A capturer whose behaviour is one plain function.
    struct TestCapturer {
        descriptor: ProviderDescriptor,
        capture: CaptureFn,
    }

    impl IdentityProvider for TestCapturer {
        fn descriptor(&self) -> &ProviderDescriptor {
            &self.descriptor
        }

        fn capture(
            &self,
            input: &CaptureInput<'_>,
        ) -> Result<Resolution, Report<IdentityProviderError>> {
            (self.capture)(input)
        }
    }

    /// A provider that declares a capability and overrides nothing.
    struct DeclaresOnly(ProviderDescriptor);

    impl IdentityProvider for DeclaresOnly {
        fn descriptor(&self) -> &ProviderDescriptor {
            &self.0
        }
    }

    const CAPTURE_PATH: &str = "/integrations/exampleid/api/token";

    fn source() -> SourceDomain {
        "exampleid.example.com"
            .parse()
            .expect("should parse the source")
    }

    fn builder() -> ProviderDescriptorBuilder {
        ProviderDescriptor::builder(
            EXAMPLEID_INTEGRATION_ID
                .parse()
                .expect("should parse the provider id"),
        )
        .with_source(source())
        .with_consent_policy(resolver_policy())
    }

    fn resolves() -> Resolves {
        Resolves::new().with_endpoint("https://api.exampleid.example.com/v1/fetch")
    }

    fn capturer(capture: CaptureFn) -> TestCapturer {
        TestCapturer {
            descriptor: builder()
                .with_captures(Captures::new().with_path(Method::POST, CAPTURE_PATH))
                .build()
                .expect("should build the descriptor"),
            capture,
        }
    }

    fn capture_cases() -> ConformanceCases {
        ConformanceCases::default()
            .with_capture_case(CaptureFixture::new(Method::POST, CAPTURE_PATH))
    }

    fn plain_request(
        input: &ResolveInput<'_>,
    ) -> Result<Option<ProviderRequest>, Report<IdentityProviderError>> {
        ProviderRequest::post_json(input.endpoint()?, &json!({})).map(Some)
    }

    fn one_uid(
        _response: &ProviderResponse,
        _input: &ResolveInput<'_>,
    ) -> Result<Resolution, Report<IdentityProviderError>> {
        Ok(Resolution::found([ResolvedEid::new(
            &source(),
            ResolvedUid::new("uid-1"),
        )]))
    }

    fn resolver(
        builder: ProviderDescriptorBuilder,
        resolves: Resolves,
        request: RequestFn,
        response: ResponseFn,
    ) -> TestResolver {
        TestResolver {
            descriptor: builder
                .with_resolves(resolves)
                .build()
                .expect("should build the descriptor"),
            request,
            response,
        }
    }

    fn resolve_cases() -> ConformanceCases {
        ConformanceCases::default().with_resolve_case(
            HarnessSetup::default(),
            StatusCode::OK,
            &json!({}),
        )
    }

    fn failure(
        provider: &dyn IdentityProvider,
        cases: &ConformanceCases,
    ) -> Report<ConformanceFailure> {
        ProviderHarness::new(provider)
            .try_check_conformance(cases)
            .expect_err("should fail the conformance check")
    }

    #[test]
    fn example_provider_round_trip() {
        let provider =
            ExampleIdProvider::new(&example_config()).expect("should build the provider");
        let mut harness = ProviderHarness::new(&provider)
            .with_client_ip("192.0.2.10")
            .with_user_agent("ExampleBrowser/1.0");

        let request = harness
            .request()
            .expect("should build a request")
            .expect("should not skip the request");
        assert_eq!(
            request.url().host_str(),
            Some("api.example.com"),
            "should call the declared host"
        );

        let resolution = harness
            .respond(
                StatusCode::OK,
                &json!({ "uid": "example-uid", "signature": "example-signature" }),
            )
            .expect("should parse the response");
        assert_eq!(
            resolution.eids()[0].uids()[0].id(),
            "example-uid",
            "should return the uid"
        );
        let state: ExampleIdState = harness.state_of(&resolution).expect("should replace state");
        assert_eq!(
            state.signature, "example-signature",
            "should keep the signature"
        );
    }

    #[test]
    fn example_provider_conforms() {
        let provider =
            ExampleIdProvider::new(&example_config()).expect("should build the provider");
        ProviderHarness::new(&provider).check_conformance_with_state::<ExampleIdState>(
            &ConformanceCases::default().with_resolve_case(
                HarnessSetup::default()
                    .with_client_ip("192.0.2.10")
                    .with_user_agent("ExampleBrowser/1.0"),
                StatusCode::OK,
                &json!({ "uid": "example-uid", "signature": "s" }),
            ),
        );
    }

    #[test]
    fn stateful_provider_is_pointed_at_the_state_check() {
        let provider =
            ExampleIdProvider::new(&example_config()).expect("should build the provider");

        let error = failure(&provider, &resolve_cases());

        assert!(
            matches!(
                error.current_context(),
                ConformanceFailure::StatefulNeedsStateType
            ),
            "should point at check_conformance_with_state"
        );
    }

    #[test]
    fn capture_provider_conforms() {
        let provider = ExampleCaptureProvider::new().expect("should build the provider");

        ProviderHarness::new(&provider).check_conformance(
            &ConformanceCases::default().with_capture_case(
                CaptureFixture::new(Method::POST, "/integrations/examplecapture/api/token")
                    .with_response(
                        StatusCode::OK,
                        &json!({ "tokens": [{
                            "source": "examplecapture.example.com",
                            "value": "token-1",
                            "expires": 1_760_086_400_u64,
                        }] }),
                    ),
            ),
        );
    }

    #[test]
    fn conformance_catches_an_undeclared_read() {
        fn reads_user_agent(
            input: &ResolveInput<'_>,
        ) -> Result<Option<ProviderRequest>, Report<IdentityProviderError>> {
            let _ignored = input.user_agent().ok();
            plain_request(input)
        }
        let provider = resolver(builder(), resolves(), reads_user_agent, one_uid);

        let error = failure(&provider, &resolve_cases());

        assert!(
            matches!(
                error.current_context(),
                ConformanceFailure::UndeclaredInput { .. }
            ),
            "should report the undeclared input"
        );
    }

    #[test]
    fn conformance_catches_an_undeclared_state_read() {
        fn reads_state(
            input: &ResolveInput<'_>,
        ) -> Result<Option<ProviderRequest>, Report<IdentityProviderError>> {
            let _ignored = input.state::<serde_json::Value>().ok();
            plain_request(input)
        }
        let provider = resolver(builder(), resolves(), reads_state, one_uid);

        let error = failure(&provider, &resolve_cases());

        assert!(
            matches!(
                error.current_context(),
                ConformanceFailure::UndeclaredInput { .. }
            ),
            "should report the undeclared state read"
        );
    }

    #[test]
    fn harness_skips_the_request_when_the_consent_policy_refuses() {
        let provider =
            ExampleIdProvider::new(&example_config()).expect("should build the provider");
        let allowed = ProviderHarness::new(&provider)
            .with_client_ip("192.0.2.10")
            .with_user_agent("ExampleBrowser/1.0")
            .request()
            .expect("should build a request");
        let refused = ProviderHarness::new(&provider)
            .with_client_ip("192.0.2.10")
            .with_user_agent("ExampleBrowser/1.0")
            .with_consent(TestConsent::default().with_gdpr_applies(true))
            .request()
            .expect("should not fail the call");

        assert!(allowed.is_some(), "should build a request without a signal");
        assert!(
            refused.is_none(),
            "should skip the request when GDPR applies and no TCF consent is present"
        );
    }

    #[test]
    fn descriptor_ignores_secrets() {
        check_descriptor_ignores_secrets(&example_config(), &example_config());
    }

    #[test]
    fn output_eids_applies_the_provenance_default_only_to_bare_entries() {
        fn two_entries(
            _response: &ProviderResponse,
            _input: &ResolveInput<'_>,
        ) -> Result<Resolution, Report<IdentityProviderError>> {
            Ok(Resolution::found([
                ResolvedEid::new(&source(), ResolvedUid::new("uid-a"))
                    .with_inserter("vendor-inserter"),
                ResolvedEid::new(&source(), ResolvedUid::new("uid-b")),
            ]))
        }
        let provider = resolver(
            builder().with_provenance(
                &source(),
                EidProvenance::new()
                    .with_inserter("default-inserter")
                    .with_matcher("default-matcher"),
            ),
            resolves(),
            plain_request,
            two_entries,
        );
        let mut harness = ProviderHarness::new(&provider);
        harness
            .request()
            .expect("should build a request")
            .expect("should not skip the request");
        let resolution = harness
            .respond(StatusCode::OK, &json!({}))
            .expect("should resolve");

        let eids = harness.output_eids(&resolution);

        assert_eq!(eids.len(), 2, "should output both entries");
        assert_eq!(
            eids[0].inserter.as_deref(),
            Some("vendor-inserter"),
            "should keep the entry's own fields"
        );
        assert_eq!(
            eids[0].matcher, None,
            "should not mix the default into an entry that sets a field"
        );
        assert_eq!(
            eids[1].inserter.as_deref(),
            Some("default-inserter"),
            "should apply the default to a bare entry"
        );
        assert_eq!(
            eids[1].matcher.as_deref(),
            Some("default-matcher"),
            "should apply the whole default to a bare entry"
        );
    }

    #[test]
    fn conformance_catches_a_cookie_header() {
        fn sets_cookie(
            input: &ResolveInput<'_>,
        ) -> Result<Option<ProviderRequest>, Report<IdentityProviderError>> {
            Ok(plain_request(input)?.map(|request| {
                request.with_header(COOKIE, HeaderValue::from_static("session=example"))
            }))
        }
        let provider = resolver(builder(), resolves(), sets_cookie, one_uid);

        let error = failure(&provider, &resolve_cases());

        assert!(
            matches!(error.current_context(), ConformanceFailure::CookieHeader),
            "should report the Cookie header"
        );
    }

    #[test]
    fn conformance_catches_a_request_to_an_undeclared_host() {
        fn wrong_host(
            _input: &ResolveInput<'_>,
        ) -> Result<Option<ProviderRequest>, Report<IdentityProviderError>> {
            let url = Url::parse("https://other.example.org/v1/fetch").expect("should parse");
            Ok(Some(ProviderRequest::get(&url)))
        }
        let provider = resolver(builder(), resolves(), wrong_host, one_uid);

        let error = failure(&provider, &resolve_cases());

        assert!(
            matches!(error.current_context(), ConformanceFailure::UndeclaredHost),
            "should report the undeclared host"
        );
    }

    #[test]
    fn conformance_catches_a_request_over_plain_http() {
        fn plain_http(
            _input: &ResolveInput<'_>,
        ) -> Result<Option<ProviderRequest>, Report<IdentityProviderError>> {
            let url =
                Url::parse("http://api.exampleid.example.com/v1/fetch").expect("should parse");
            Ok(Some(ProviderRequest::get(&url)))
        }
        let provider = resolver(builder(), resolves(), plain_http, one_uid);

        let error = failure(&provider, &resolve_cases());

        assert!(
            matches!(error.current_context(), ConformanceFailure::InsecureScheme),
            "should report the plain http request"
        );
    }

    #[test]
    fn conformance_catches_an_entry_for_an_undeclared_source() {
        fn other_source(
            _response: &ProviderResponse,
            _input: &ResolveInput<'_>,
        ) -> Result<Resolution, Report<IdentityProviderError>> {
            let other: SourceDomain = "other.example.com".parse().expect("should parse");
            Ok(Resolution::found([ResolvedEid::new(
                &other,
                ResolvedUid::new("uid-1"),
            )]))
        }
        let provider = resolver(builder(), resolves(), plain_request, other_source);

        let error = failure(&provider, &resolve_cases());

        assert!(
            matches!(
                error.current_context(),
                ConformanceFailure::UndeclaredSource { .. }
            ),
            "should report the undeclared source"
        );
    }

    #[test]
    fn conformance_catches_a_33rd_uid() {
        fn thirty_three(
            _response: &ProviderResponse,
            _input: &ResolveInput<'_>,
        ) -> Result<Resolution, Report<IdentityProviderError>> {
            let mut eid = ResolvedEid::new(&source(), ResolvedUid::new("uid-0"));
            for index in 1..33 {
                eid = eid.with_uid(ResolvedUid::new(format!("uid-{index}")));
            }
            Ok(Resolution::found([eid]))
        }
        let provider = resolver(builder(), resolves(), plain_request, thirty_three);

        let error = failure(&provider, &resolve_cases());

        assert!(
            matches!(error.current_context(), ConformanceFailure::LimitHit { .. }),
            "should report the dropped UID"
        );
    }

    #[test]
    fn conformance_catches_a_blank_inserter() {
        fn blank_inserter(
            _response: &ProviderResponse,
            _input: &ResolveInput<'_>,
        ) -> Result<Resolution, Report<IdentityProviderError>> {
            Ok(Resolution::found([ResolvedEid::new(
                &source(),
                ResolvedUid::new("uid-1"),
            )
            .with_inserter(" ")]))
        }
        let provider = resolver(builder(), resolves(), plain_request, blank_inserter);

        let error = failure(&provider, &resolve_cases());

        assert!(
            matches!(error.current_context(), ConformanceFailure::LimitHit { .. }),
            "should report the dropped inserter"
        );
    }

    #[test]
    fn conformance_catches_an_oversized_set() {
        fn eight_large_entries(
            _response: &ProviderResponse,
            _input: &ResolveInput<'_>,
        ) -> Result<Resolution, Report<IdentityProviderError>> {
            let entries = (0..8).map(|index| {
                ResolvedEid::new(
                    &source(),
                    ResolvedUid::new(format!("uid-{index}"))
                        .with_ext(json!({ "pad": "x".repeat(1_000) })),
                )
            });
            Ok(Resolution::found(entries))
        }
        let provider = resolver(builder(), resolves(), plain_request, eight_large_entries);

        let error = failure(&provider, &resolve_cases());

        assert!(
            matches!(
                error.current_context(),
                ConformanceFailure::SetTooLarge { .. }
            ),
            "should report the set over MAX_ENTRY_SET_BYTES"
        );
    }

    #[test]
    fn conformance_catches_a_capability_declared_but_not_overridden() {
        let resolving = DeclaresOnly(
            builder()
                .with_resolves(resolves())
                .build()
                .expect("should build the descriptor"),
        );
        let capturing = DeclaresOnly(
            builder()
                .with_captures(Captures::new().with_path(Method::POST, CAPTURE_PATH))
                .build()
                .expect("should build the descriptor"),
        );

        let resolve_error = failure(&resolving, &resolve_cases());
        let capture_error = failure(&capturing, &capture_cases());

        assert!(
            matches!(
                resolve_error.current_context(),
                ConformanceFailure::NotImplemented { method: "request" }
            ),
            "should report the missing request override"
        );
        assert!(
            matches!(
                capture_error.current_context(),
                ConformanceFailure::NotImplemented { method: "capture" }
            ),
            "should report the missing capture override"
        );
    }

    #[test]
    fn conformance_catches_a_resolver_that_never_builds_a_request() {
        fn skips(
            _input: &ResolveInput<'_>,
        ) -> Result<Option<ProviderRequest>, Report<IdentityProviderError>> {
            Ok(None)
        }
        let provider = resolver(builder(), resolves(), skips, one_uid);

        let error = failure(&provider, &resolve_cases());

        assert!(
            matches!(
                error.current_context(),
                ConformanceFailure::ResolverNeverRequests
            ),
            "should report a resolver that never requests"
        );
    }

    #[test]
    fn conformance_catches_a_capturer_that_never_captures() {
        fn nothing(_input: &CaptureInput<'_>) -> Result<Resolution, Report<IdentityProviderError>> {
            Ok(Resolution::no_match())
        }

        let error = failure(&capturer(nothing), &capture_cases());

        assert!(
            matches!(
                error.current_context(),
                ConformanceFailure::CapturerNeverMatches
            ),
            "should report a capturer that captures nothing"
        );
    }

    #[test]
    fn conformance_catches_state_from_a_stateless_provider() {
        fn returns_state(
            _response: &ProviderResponse,
            _input: &ResolveInput<'_>,
        ) -> Result<Resolution, Report<IdentityProviderError>> {
            Ok(
                Resolution::found([ResolvedEid::new(&source(), ResolvedUid::new("uid-1"))])
                    .replacing_state(&json!({ "signature": "s" })),
            )
        }
        let provider = resolver(builder(), resolves(), plain_request, returns_state);

        let error = failure(&provider, &resolve_cases());

        assert!(
            matches!(error.current_context(), ConformanceFailure::StatelessState),
            "should report state from a provider without private state"
        );
    }

    #[test]
    fn conformance_catches_a_capture_that_changes_state() {
        fn clears_state(
            _input: &CaptureInput<'_>,
        ) -> Result<Resolution, Report<IdentityProviderError>> {
            Ok(
                Resolution::found([ResolvedEid::new(&source(), ResolvedUid::new("uid-1"))])
                    .clearing_state(),
            )
        }

        let error = failure(&capturer(clears_state), &capture_cases());

        assert!(
            matches!(
                error.current_context(),
                ConformanceFailure::CaptureStateChange
            ),
            "should report a capture that changes state"
        );
    }

    #[test]
    fn conformance_catches_a_stateful_provider_that_accepts_a_bare_string_state() {
        fn reads_any_state(
            input: &ResolveInput<'_>,
        ) -> Result<Option<ProviderRequest>, Report<IdentityProviderError>> {
            let _state = input.state::<serde_json::Value>()?;
            plain_request(input)
        }
        fn keeps_state(
            _response: &ProviderResponse,
            _input: &ResolveInput<'_>,
        ) -> Result<Resolution, Report<IdentityProviderError>> {
            Ok(
                Resolution::found([ResolvedEid::new(&source(), ResolvedUid::new("uid-1"))])
                    .replacing_state(&json!({ "signature": "s" })),
            )
        }
        let provider = resolver(
            builder(),
            resolves().with_private_state(),
            reads_any_state,
            keeps_state,
        );

        let error = ProviderHarness::new(&provider)
            .try_check_conformance_with_state::<serde_json::Value>(&resolve_cases())
            .expect_err("should fail a state type that decodes a bare string");

        assert!(
            matches!(
                error.current_context(),
                ConformanceFailure::BareStringStateAccepted
            ),
            "should report the accepted bare string"
        );
    }

    #[test]
    fn conformance_catches_an_expiring_source_without_an_expiry() {
        let provider = resolver(
            builder().with_expiring_source(&source()),
            resolves(),
            plain_request,
            one_uid,
        );

        let error = failure(&provider, &resolve_cases());

        assert!(
            matches!(
                error.current_context(),
                ConformanceFailure::MissingExpiry { .. }
            ),
            "should report the missing expiry"
        );
    }

    #[test]
    fn conformance_accepts_a_complete_set_within_the_limits() {
        fn complete_set(
            _response: &ProviderResponse,
            _input: &ResolveInput<'_>,
        ) -> Result<Resolution, Report<IdentityProviderError>> {
            Ok(Resolution::found([
                ResolvedEid::new(
                    &source(),
                    ResolvedUid::new("uid-a")
                        .with_atype(1)
                        .with_ext(json!({ "stype": "ppuid" })),
                )
                .with_uid(ResolvedUid::new("uid-b"))
                .with_inserter("inserter.example.com")
                .with_matcher("matcher.example.com")
                .with_mm(2)
                .with_ext(json!({ "segments": ["s"] })),
                ResolvedEid::new(&source(), ResolvedUid::new("uid-c"))
                    .with_ext(json!({ "segments": ["t"] })),
            ]))
        }
        let provider = resolver(builder(), resolves(), plain_request, complete_set);

        let result = ProviderHarness::new(&provider).try_check_conformance(&resolve_cases());

        assert!(
            result.is_ok(),
            "should accept two entries and three UIDs with every field"
        );
    }

    #[test]
    fn stored_full_set_reaches_a_refreshing_provider() {
        fn echoes_stored_set(
            input: &ResolveInput<'_>,
        ) -> Result<Option<ProviderRequest>, Report<IdentityProviderError>> {
            let stored = input
                .stored(&source())
                .map(|stored| stored.eids().to_vec())
                .unwrap_or_default();
            ProviderRequest::post_json(input.endpoint()?, &stored).map(Some)
        }
        let provider = resolver(builder(), resolves(), echoes_stored_set, one_uid);
        let set = vec![
            Eid {
                source: "exampleid.example.com".to_owned(),
                uids: vec![
                    Uid {
                        id: "uid-a".to_owned(),
                        atype: Some(1),
                        ext: None,
                    },
                    Uid {
                        id: "uid-b".to_owned(),
                        atype: None,
                        ext: Some(json!({ "stype": "ppuid" })),
                    },
                ],
                inserter: Some("inserter.example.com".to_owned()),
                matcher: None,
                mm: Some(2),
                ext: None,
            },
            Eid {
                source: "exampleid.example.com".to_owned(),
                uids: vec![Uid {
                    id: "uid-c".to_owned(),
                    atype: None,
                    ext: None,
                }],
                inserter: None,
                matcher: None,
                mm: None,
                ext: Some(json!({ "segments": ["s"] })),
            },
        ];
        let mut harness = ProviderHarness::new(&provider)
            .with_stored(StoredId::for_test(&source(), "uid-a").with_eids(set.clone()));

        harness
            .request()
            .expect("should build a request")
            .expect("should not skip the refresh");

        assert_eq!(
            harness.request_json::<Vec<Eid>>(),
            set,
            "should give the provider the stored full set"
        );
    }
}
