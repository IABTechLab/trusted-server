//! Sends provider requests: request checks, transport limits, decoding.
//!
//! Providers never do I/O. For each call this module builds the provider's
//! request, checks it (HTTPS, a declared host, no `Cookie`), applies the
//! per-visitor burst cap, sends it with the provider's timeout, buffers and
//! decodes the response under `max_response_bytes`, and hands the bytes back
//! to the provider. Provider errors are logged with their context only.

use std::collections::BTreeMap;
use std::io::Read;
use std::time::Duration;

use error_stack::Report;
use http::{StatusCode, header};
use web_time::Instant;

use super::catalog::ProviderCatalog;
use super::config::ProviderSettings;
use super::coordinator::CallOutcome;
use super::descriptor::{ProviderDescriptor, ResolveKind};
use super::exchange::{ProviderRequest, ProviderResponse};
use super::ids::{IdentityProviderId, SourceDomain};
use super::input::{ResolveInput, ResolveSignals, StoredId};
use super::mutation::ExpectedState;
use super::provider::IdentityProvider;
use super::resolution::IdentityProviderError;
use super::revision::{IdentityKeys, visitor_key};
use crate::ec::rate_limiter::RateLimiter;
use crate::platform::{
    PlatformBackendSpec, PlatformHttpRequest, PlatformPendingRequest, PlatformResponse,
    RuntimeServices,
};
use crate::settings::Settings;

/// Shared dependencies of identity work, in one struct so request handlers
/// stay within their argument limit.
pub(crate) struct IdentityDeps<'a> {
    pub(crate) settings: &'a Settings,
    pub(crate) catalog: &'a ProviderCatalog,
    pub(crate) keys: &'a IdentityKeys,
    pub(crate) services: &'a RuntimeServices,
    pub(crate) rate_limiter: Option<&'a dyn RateLimiter>,
}

/// One provider call to make.
#[derive(Debug, Clone)]
pub(crate) struct Call {
    pub(crate) settings: ProviderSettings,
    pub(crate) provider: IdentityProviderId,
    /// The sources to resolve, each with its kind.
    pub(crate) kinds: BTreeMap<SourceDomain, ResolveKind>,
    /// The provider's stored records.
    pub(crate) stored: BTreeMap<SourceDomain, StoredId>,
    /// The provider's private state JSON.
    pub(crate) state_json: Option<String>,
}

/// How one call ended.
#[derive(Debug, Clone)]
pub(crate) struct CallReport {
    pub(crate) provider: IdentityProviderId,
    pub(crate) outcome: CallOutcome,
    /// The state the call read, for the atomic check of its result.
    pub(crate) expected_state: ExpectedState,
}

/// One dispatch round.
pub(crate) struct DispatchRequest<'a> {
    pub(crate) deps: &'a IdentityDeps<'a>,
    /// The request's signals; each call gets its own stored records.
    pub(crate) signals: &'a ResolveSignals<'a>,
    /// Hold mode: the shared wall-clock budget from the first dispatch.
    pub(crate) hold_budget: Option<Duration>,
    /// The full EC ID, for the per-visitor burst cap key.
    pub(crate) visitor: Option<&'a str>,
}

/// Checks a provider's request before it is sent: `https`, a host the
/// descriptor declares, and no `Cookie` header.
///
/// # Errors
///
/// `Config` naming the failed check.
pub(crate) fn check_request(
    request: &ProviderRequest,
    descriptor: &ProviderDescriptor,
) -> Result<(), Report<IdentityProviderError>> {
    if request.url().scheme() != "https" {
        return Err(IdentityProviderError::config("request is not https"));
    }
    let host = request.url().host_str().unwrap_or_default();
    let allowed = descriptor
        .resolves()
        .is_some_and(|resolves| resolves.hosts().iter().any(|known| known == host));
    if !allowed {
        return Err(IdentityProviderError::config(
            "request host is not declared",
        ));
    }
    if request.has_cookie_header() {
        return Err(IdentityProviderError::config(
            "request carries a Cookie header",
        ));
    }
    Ok(())
}

fn read_capped(reader: impl Read, cap: usize) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let limit = u64::try_from(cap).unwrap_or(u64::MAX).saturating_add(1);
    reader.take(limit).read_to_end(&mut out).ok()?;
    (out.len() <= cap).then_some(out)
}

/// Undoes `Content-Encoding`, capping the decoded size. `None` when over the
/// cap, corrupt, or an encoding TS does not decode.
#[must_use]
pub(crate) fn decode_body(encoding: Option<&str>, body: Vec<u8>, cap: usize) -> Option<Vec<u8>> {
    let encoding = encoding.map(|value| value.trim().to_ascii_lowercase());
    match encoding.as_deref() {
        None | Some("" | "identity") => (body.len() <= cap).then_some(body),
        Some("gzip" | "x-gzip") => read_capped(flate2::read::MultiGzDecoder::new(&body[..]), cap),
        Some("br") => read_capped(brotli::Decompressor::new(&body[..], 4096), cap),
        Some(_) => None,
    }
}

/// A sent call: what finishing it needs, beside its pending request.
struct InFlight<'a> {
    provider: std::sync::Arc<dyn IdentityProvider>,
    call: Call,
    signals: ResolveSignals<'a>,
    call_context: Option<String>,
    state_failed: bool,
    expected_state: ExpectedState,
}

fn expected_state_for(provider: &dyn IdentityProvider, call: &Call) -> ExpectedState {
    let stateful = provider
        .descriptor()
        .resolves()
        .is_some_and(super::descriptor::Resolves::has_private_state);
    match (stateful, &call.state_json) {
        (false, _) => ExpectedState::NotRead,
        (true, Some(json)) => ExpectedState::Json(json.clone()),
        (true, None) => ExpectedState::Absent,
    }
}

fn log_provider_error(id: &IdentityProviderId, stage: &str, error: &Report<IdentityProviderError>) {
    log::warn!(
        "Identity provider {id} {stage} failed: {}",
        error.current_context()
    );
}

/// Calls `request` and maps its result. `Ok` holds the request and whether
/// the state failed to decode.
fn build_request(
    provider: &dyn IdentityProvider,
    call: &Call,
    signals: &ResolveSignals<'_>,
) -> Result<(ProviderRequest, bool), CallOutcome> {
    let id = provider.descriptor().id();
    let input = ResolveInput::new(provider.descriptor(), call.kinds.clone(), signals);
    let result = provider.request(&input);
    let undeclared = input.recorded_errors();
    if !undeclared.is_empty() {
        log::warn!("Identity provider {id} read undeclared inputs: {undeclared:?}");
        return Err(CallOutcome::InputFailure);
    }
    match result {
        Ok(Some(request)) => Ok((request, input.state_decode_failed())),
        Ok(None) => Err(CallOutcome::Skipped),
        Err(error) => match error.current_context() {
            IdentityProviderError::InputUnavailable { .. } => Err(CallOutcome::InputUnavailable),
            IdentityProviderError::InvalidState => Err(CallOutcome::InvalidState(None)),
            _ => {
                log_provider_error(id, "request", &error);
                Err(CallOutcome::InputFailure)
            }
        },
    }
}

fn burst_capped(request: &DispatchRequest<'_>, call: &Call) -> bool {
    let (Some(limiter), Some(visitor)) = (request.deps.rate_limiter, request.visitor) else {
        return false;
    };
    let key = format!(
        "identity:{}:{}",
        call.provider,
        visitor_key(request.deps.keys, visitor)
    );
    match limiter.exceeded_per_minute(&key, call.settings.max_calls_per_minute) {
        Ok(exceeded) => exceeded,
        Err(error) => {
            log::warn!(
                "Identity provider {} burst cap check failed: {}",
                call.provider,
                error.current_context()
            );
            true
        }
    }
}

fn send(
    request: &DispatchRequest<'_>,
    provider_request: &ProviderRequest,
    timeout_ms: u64,
) -> Option<PlatformPendingRequest> {
    let services = request.deps.services;
    let url = provider_request.url();
    let configured = u32::try_from(timeout_ms).unwrap_or(u32::MAX);
    let transport_ms = services
        .backend()
        .canonicalize_transport_timeout_ms(configured, configured);
    let timeout = Duration::from_millis(u64::from(transport_ms.max(1)));
    let backend_name = services
        .backend()
        .ensure(&PlatformBackendSpec {
            scheme: url.scheme().to_owned(),
            host: url.host_str().unwrap_or_default().to_owned(),
            port: url.port(),
            host_header_override: None,
            certificate_check: request.deps.settings.proxy.certificate_check,
            first_byte_timeout: timeout,
            between_bytes_timeout: timeout,
            discriminator: None,
        })
        .ok()?;

    let mut builder = http::Request::builder()
        .method(provider_request.method().clone())
        .uri(url.as_str());
    for (name, value) in provider_request.headers() {
        builder = builder.header(name, value);
    }
    let outbound = builder
        .body(edgezero_core::body::Body::from(
            provider_request.body().to_vec(),
        ))
        .ok()?;
    futures::executor::block_on(
        services
            .http_client()
            .send_async(PlatformHttpRequest::new(outbound, backend_name)),
    )
    .ok()
}

fn content_length_exceeds(response: &PlatformResponse, cap: usize) -> bool {
    response
        .response
        .headers()
        .get(header::CONTENT_LENGTH)
        .is_some_and(|value| {
            value
                .to_str()
                .ok()
                .and_then(|value| value.parse::<usize>().ok())
                .is_none_or(|length| length > cap)
        })
}

fn finish(item: &InFlight<'_>, response: PlatformResponse, cap: usize) -> CallOutcome {
    let descriptor = item.provider.descriptor();
    let id = descriptor.id();
    if content_length_exceeds(&response, cap) {
        log::warn!("Identity provider {id} declared an oversized response");
        return CallOutcome::TransportFailure;
    }
    let status = response.response.status();
    let headers = response.response.headers().clone();
    let raw = response
        .response
        .into_body()
        .into_bytes()
        .unwrap_or_default()
        .to_vec();
    if raw.len() > cap {
        log::warn!("Identity provider {id} returned an oversized response");
        return CallOutcome::TransportFailure;
    }
    let encoding = headers
        .get(header::CONTENT_ENCODING)
        .and_then(|value| value.to_str().ok());
    let Some(body) = decode_body(encoding, raw, cap) else {
        log::warn!("Identity provider {id} response could not be decoded under the cap");
        return CallOutcome::TransportFailure;
    };

    let provider_response = ProviderResponse::new(status, headers, body, item.call_context.clone());
    let input = ResolveInput::new(descriptor, item.call.kinds.clone(), &item.signals);
    let result = item.provider.response(&provider_response, &input);
    let undeclared = input.recorded_errors();
    if !undeclared.is_empty() {
        log::warn!("Identity provider {id} read undeclared inputs: {undeclared:?}");
        return CallOutcome::InputFailure;
    }
    match result {
        Ok(resolution) if item.state_failed || input.state_decode_failed() => {
            CallOutcome::InvalidState(Some(resolution))
        }
        Ok(resolution) => CallOutcome::Resolved(resolution),
        Err(error) => {
            log_provider_error(id, "response", &error);
            match error.current_context() {
                IdentityProviderError::Rejected { status }
                    if *status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() =>
                {
                    CallOutcome::TransportFailure
                }
                IdentityProviderError::InvalidState => CallOutcome::InvalidState(None),
                _ => CallOutcome::InputFailure,
            }
        }
    }
}

fn drain(
    request: &DispatchRequest<'_>,
    in_flight: &mut Vec<(PlatformPendingRequest, InFlight<'_>)>,
    started: Instant,
    reports: &mut Vec<CallReport>,
) {
    let cap = request.deps.settings.ec.identity.max_response_bytes;
    for (pending, item) in in_flight.drain(..) {
        let result = futures::executor::block_on(request.deps.services.http_client().wait(pending));
        let elapsed = started.elapsed();
        let id = item.call.provider.clone();
        log::debug!(
            "Identity provider {id} answered after {} ms",
            elapsed.as_millis()
        );
        let outcome = match result {
            Ok(response) => finish(&item, response, cap),
            Err(_) if request.hold_budget.is_some_and(|budget| elapsed >= budget) => {
                CallOutcome::HoldCutOff
            }
            Err(error) => {
                log::warn!(
                    "Identity provider {id} transport failed: {}",
                    error.current_context()
                );
                CallOutcome::TransportFailure
            }
        };
        reports.push(CallReport {
            provider: id,
            outcome,
            expected_state: item.expected_state,
        });
    }
}

/// Builds, checks, sends and finishes every call, at most
/// `max_concurrency` at once.
#[must_use]
pub(crate) fn dispatch(request: &DispatchRequest<'_>, calls: Vec<Call>) -> Vec<CallReport> {
    let settings = request.deps.settings;
    let max_concurrency = settings
        .ec
        .identity
        .effective_max_concurrency(settings.ec.pull_sync_concurrency)
        .max(1);
    let started = Instant::now();
    let mut reports = Vec::new();
    let mut in_flight: Vec<(PlatformPendingRequest, InFlight<'_>)> = Vec::new();

    for call in calls {
        let Some(provider) = request.deps.catalog.get(&call.provider).cloned() else {
            continue;
        };
        let expected_state = expected_state_for(provider.as_ref(), &call);
        let report = |outcome| CallReport {
            provider: call.provider.clone(),
            outcome,
            expected_state: expected_state.clone(),
        };
        let signals =
            request
                .signals
                .for_provider(call.stored.clone(), call.state_json.clone(), false);
        let (provider_request, state_failed) =
            match build_request(provider.as_ref(), &call, &signals) {
                Ok(built) => built,
                Err(outcome) => {
                    reports.push(report(outcome));
                    continue;
                }
            };
        if let Err(error) = check_request(&provider_request, provider.descriptor()) {
            log_provider_error(&call.provider, "request check", &error);
            reports.push(report(CallOutcome::InputFailure));
            continue;
        }
        if burst_capped(request, &call) {
            log::debug!("Identity provider {} hit its burst cap", call.provider);
            reports.push(report(CallOutcome::NotDispatched));
            continue;
        }

        let mut timeout_ms = call.settings.effective_timeout_ms(&settings.ec.identity);
        if let Some(budget) = request.hold_budget {
            let remaining = budget.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                reports.push(report(CallOutcome::HoldCutOff));
                continue;
            }
            timeout_ms = timeout_ms.min(u64::try_from(remaining.as_millis()).unwrap_or(u64::MAX));
        }

        let Some(pending) = send(request, &provider_request, timeout_ms) else {
            log::warn!("Identity provider {} could not be sent", call.provider);
            reports.push(report(CallOutcome::TransportFailure));
            continue;
        };
        in_flight.push((
            pending,
            InFlight {
                provider,
                call_context: provider_request.call_context_json().map(str::to_owned),
                state_failed,
                expected_state,
                signals,
                call,
            },
        ));
        if in_flight.len() >= max_concurrency {
            drain(request, &mut in_flight, started, &mut reports);
        }
    }
    drain(request, &mut in_flight, started, &mut reports);
    reports
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::io::Write as _;
    use std::sync::Arc;

    use error_stack::Report;
    use serde::Deserialize;
    use url::Url;

    use super::*;
    use crate::ec::identity::descriptor::{Input, ProviderDescriptor, Resolves};
    use crate::ec::identity::exchange::{ProviderRequest, ProviderResponse};
    use crate::ec::identity::ids::UnixTime;
    use crate::ec::identity::input::{ProviderConsent, ResolveInput, ThreadRandom};
    use crate::ec::identity::resolution::{IdentityProviderError, Resolution, ResolvedId};
    use crate::ec::identity::test_support::{fixed_provider, test_keys};
    use crate::platform::test_support::{StubHttpClient, build_services_with_http_client};
    use crate::test_support::tests::create_test_settings;

    struct TestPull {
        descriptor: ProviderDescriptor,
        read_user_agent: bool,
    }

    impl TestPull {
        fn new(read_user_agent: bool) -> Self {
            Self {
                descriptor: ProviderDescriptor::builder("tp".parse().expect("should parse id"))
                    .with_source("a.example.com".parse().expect("should parse source"))
                    .with_resolves(
                        Resolves::new()
                            .with_endpoint("https://api.example.com/v1/fetch")
                            .with_needs([Input::ClientIp]),
                    )
                    .build()
                    .expect("should build"),
                read_user_agent,
            }
        }
    }

    #[derive(Deserialize)]
    struct Reply {
        uid: String,
    }

    impl IdentityProvider for TestPull {
        fn descriptor(&self) -> &ProviderDescriptor {
            &self.descriptor
        }

        fn request(
            &self,
            input: &ResolveInput<'_>,
        ) -> Result<Option<ProviderRequest>, Report<IdentityProviderError>> {
            if self.read_user_agent {
                let _ignored = input.user_agent().ok();
            }
            let _ip = input.client_ip()?;
            Ok(Some(ProviderRequest::get(input.endpoint()?)))
        }

        fn response(
            &self,
            response: &ProviderResponse,
            input: &ResolveInput<'_>,
        ) -> Result<Resolution, Report<IdentityProviderError>> {
            let reply: Reply = response.success_json()?;
            Ok(Resolution::found([ResolvedId::new(
                input.primary_source(),
                reply.uid,
            )]))
        }
    }

    fn signals() -> ResolveSignals<'static> {
        ResolveSignals {
            client_ip: Some("192.0.2.10".parse().expect("should parse ip")),
            user_agent: Some("ExampleBrowser/1.0"),
            page_url: None,
            region: None,
            hashed_email: None,
            consent: ProviderConsent::default(),
            publisher_domain: "www.example.com",
            now: UnixTime::from_secs(1_000),
            random: &ThreadRandom,
            ec_id: None,
            stored: BTreeMap::new(),
            state_json: None,
            dry_run: false,
        }
    }

    fn call() -> Call {
        Call {
            settings: ProviderSettings::default(),
            provider: "tp".parse().expect("should parse id"),
            kinds: [(
                "a.example.com".parse().expect("should parse source"),
                ResolveKind::Initial,
            )]
            .into_iter()
            .collect(),
            stored: BTreeMap::new(),
            state_json: None,
        }
    }

    fn run(provider: TestPull, stub: &Arc<StubHttpClient>) -> CallOutcome {
        let settings = create_test_settings();
        let catalog = ProviderCatalog::new(vec![Arc::new(provider)], BTreeMap::new());
        let keys = test_keys();
        let services = build_services_with_http_client(
            Arc::clone(stub) as Arc<dyn crate::platform::PlatformHttpClient>
        );
        let deps = IdentityDeps {
            settings: &settings,
            catalog: &catalog,
            keys: &keys,
            services: &services,
            rate_limiter: None,
        };
        let signals = signals();
        let mut reports = dispatch(
            &DispatchRequest {
                deps: &deps,
                signals: &signals,
                hold_budget: None,
                visitor: None,
            },
            vec![call()],
        );
        assert_eq!(reports.len(), 1, "one report per call");
        reports.remove(0).outcome
    }

    #[test]
    fn check_request_enforces_https_host_and_no_cookie() {
        let provider = fixed_provider("p", &["a.example.com"]);
        let descriptor = provider.descriptor();
        let good = ProviderRequest::get(&Url::parse("https://api.example.com/x").expect("url"));
        check_request(&good, descriptor).expect("control: allowed");
        for (name, request) in [
            (
                "http",
                ProviderRequest::get(&Url::parse("http://api.example.com/x").expect("url")),
            ),
            (
                "host",
                ProviderRequest::get(&Url::parse("https://evil.example.net/x").expect("url")),
            ),
            (
                "cookie",
                good.clone()
                    .try_with_header("cookie", "a=1")
                    .expect("should build header"),
            ),
        ] {
            assert!(
                check_request(&request, descriptor).is_err(),
                "should reject: {name}"
            );
        }
    }

    #[test]
    fn decode_body_caps_decoded_size() {
        let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gzip.write_all(&vec![b'a'; 10_000]).expect("should write");
        let gzipped = gzip.finish().expect("should finish");
        assert_eq!(
            decode_body(Some("gzip"), gzipped.clone(), 10_000).map(|body| body.len()),
            Some(10_000),
            "fits"
        );
        assert!(
            decode_body(Some("gzip"), gzipped, 9_999).is_none(),
            "over the cap after decoding"
        );

        let mut brotli = brotli::CompressorWriter::new(Vec::new(), 4096, 5, 22);
        brotli.write_all(b"hello").expect("should write");
        let compressed = brotli.into_inner();
        assert_eq!(
            decode_body(Some("br"), compressed, 10).as_deref(),
            Some(&b"hello"[..]),
            "brotli"
        );
        assert!(
            decode_body(Some("zstd"), vec![1], 10).is_none(),
            "unknown encoding"
        );
        assert_eq!(
            decode_body(None, b"ok".to_vec(), 10).as_deref(),
            Some(&b"ok"[..]),
            "identity"
        );
        assert!(
            decode_body(None, vec![0; 11], 10).is_none(),
            "identity over the cap"
        );
    }

    #[test]
    fn dispatch_maps_status_codes_to_outcomes() {
        let ok = Arc::new(StubHttpClient::new());
        ok.push_response(200, br#"{"uid":"u"}"#.to_vec());
        assert!(
            matches!(run(TestPull::new(false), &ok), CallOutcome::Resolved(resolution) if resolution.ids()[0].uid() == "u"),
            "200 resolves"
        );
        assert_eq!(ok.recorded_request_uris().len(), 1, "one request sent");

        for (status, expected) in [(503, "transport"), (429, "transport"), (400, "input")] {
            let stub = Arc::new(StubHttpClient::new());
            stub.push_response(status, br#"{}"#.to_vec());
            let outcome = run(TestPull::new(false), &stub);
            match expected {
                "transport" => assert_eq!(outcome, CallOutcome::TransportFailure, "{status}"),
                _ => assert_eq!(outcome, CallOutcome::InputFailure, "{status}"),
            }
        }

        let oversized = Arc::new(StubHttpClient::new());
        oversized.push_response(200, vec![b' '; 70_000]);
        assert_eq!(
            run(TestPull::new(false), &oversized),
            CallOutcome::TransportFailure,
            "a body over max_response_bytes is a transport failure"
        );

        let failed = Arc::new(StubHttpClient::new());
        failed.push_response(200, br#"{"uid":"u"}"#.to_vec());
        failed.push_wait_error();
        assert_eq!(
            run(TestPull::new(false), &failed),
            CallOutcome::TransportFailure,
            "a transport error"
        );
    }

    #[test]
    fn undeclared_read_is_an_input_failure_and_nothing_is_sent() {
        let stub = Arc::new(StubHttpClient::new());
        stub.push_response(200, br#"{"uid":"u"}"#.to_vec());
        assert_eq!(
            run(TestPull::new(true), &stub),
            CallOutcome::InputFailure,
            "an undeclared read even through .ok() fails the call"
        );
        assert!(
            stub.recorded_request_uris().is_empty(),
            "no request is sent"
        );
    }
}
