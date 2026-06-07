use edgezero_core::body::Body as EdgeBody;
use edgezero_core::http::{
    header, HeaderName, HeaderValue, Method, Request as HttpRequest, Response as HttpResponse,
};
use error_stack::Report;
use fastly::http::Method as FastlyMethod;
use fastly::{Request as FastlyRequest, Response as FastlyResponse};

use trusted_server_core::auction::endpoints::handle_auction;
use trusted_server_core::auction::{build_orchestrator, AuctionOrchestrator};
use trusted_server_core::auth::enforce_basic_auth;
use trusted_server_core::compat;
use trusted_server_core::constants::{
    COOKIE_SHAREDID, COOKIE_TS_EIDS, ENV_FASTLY_IS_STAGING, ENV_FASTLY_SERVICE_VERSION,
    HEADER_X_GEO_INFO_AVAILABLE, HEADER_X_TS_ENV, HEADER_X_TS_VERSION,
};
use trusted_server_core::ec::batch_sync::handle_batch_sync;
use trusted_server_core::ec::consent::ec_consent_withdrawn;
use trusted_server_core::ec::device::DeviceSignals;
use trusted_server_core::ec::finalize::ec_finalize_response;
use trusted_server_core::ec::identify::{cors_preflight_identify, handle_identify};
use trusted_server_core::ec::kv::KvIdentityGraph;
use trusted_server_core::ec::pull_sync::{
    build_pull_sync_context, dispatch_pull_sync, PullSyncContext,
};
use trusted_server_core::ec::rate_limiter::{FastlyRateLimiter, RATE_COUNTER_NAME};
use trusted_server_core::ec::registry::PartnerRegistry;
use trusted_server_core::ec::EcContext;
use trusted_server_core::error::{IntoHttpResponse, TrustedServerError};
use trusted_server_core::geo::GeoInfo;
use trusted_server_core::integrations::{IntegrationRegistry, ProxyDispatchInput};
use trusted_server_core::http_util::is_navigation_request;
use trusted_server_core::platform::RuntimeServices;
use trusted_server_core::proxy::{
    handle_first_party_click, handle_first_party_proxy, handle_first_party_proxy_rebuild,
    handle_first_party_proxy_sign,
};
use trusted_server_core::publisher::{
    handle_publisher_request, handle_tsjs_dynamic, stream_publisher_body,
    OwnedProcessResponseParams, PublisherResponse,
};
use trusted_server_core::request_signing::{
    handle_deactivate_key, handle_rotate_key, handle_trusted_server_discovery,
    handle_verify_signature,
};
use trusted_server_core::settings::Settings;
use trusted_server_core::settings_data::get_settings;

mod error;
mod logging;
mod management_api;
mod platform;
#[cfg(test)]
mod route_tests;

use crate::error::to_error_response;
use crate::logging::init_logger;
use crate::platform::{build_runtime_services, UnavailableKvStore};

/// Result of routing a request, distinguishing buffered from streaming publisher responses.
///
/// The streaming arm keeps the publisher body out of WASM heap until it is written directly
/// to the client via [`fastly::Response::stream_to_client`].  All other routes are buffered.
///
/// [`AuthChallenge`](HandlerOutcome::AuthChallenge) marks responses produced by this server's
/// own `enforce_basic_auth` so the geo-lookup gate can distinguish them from origin-forwarded
/// 401s, which should still carry geo headers.
enum HandlerOutcome {
    Buffered(HttpResponse),
    AuthChallenge(HttpResponse),
    Streaming {
        response: HttpResponse,
        body: EdgeBody,
        params: OwnedProcessResponseParams,
    },
}

impl HandlerOutcome {
    #[cfg(test)]
    fn status(&self) -> edgezero_core::http::StatusCode {
        match self {
            HandlerOutcome::Buffered(resp) | HandlerOutcome::AuthChallenge(resp) => resp.status(),
            HandlerOutcome::Streaming { response, .. } => response.status(),
        }
    }
}

/// Combined result from `route_request`, bundling the handler outcome with the
/// EC context and cookies needed for post-send finalization and pull sync.
struct RouteResult {
    outcome: HandlerOutcome,
    ec_context: EcContext,
    finalize_kv_graph: Option<KvIdentityGraph>,
    eids_cookie: Option<String>,
    sharedid_cookie: Option<String>,
    is_real_browser: bool,
}

/// Entry point for the Fastly Compute program.
///
/// Uses an undecorated `main()` with `FastlyRequest::from_client()` instead of
/// `#[fastly::main]` so we can call `send_to_client()` explicitly when needed.
fn main() {
    init_logger();

    let mut req = FastlyRequest::from_client();

    // Keep the health probe independent from settings loading and routing so
    // readiness checks still get a cheap liveness response during startup.
    if req.get_method() == FastlyMethod::GET && req.get_path() == "/health" {
        FastlyResponse::from_status(200)
            .with_body_text_plain("ok")
            .send_to_client();
        return;
    }

    let settings = match get_settings() {
        Ok(s) => s,
        Err(e) => {
            log::error!("Failed to load settings: {:?}", e);
            to_error_response(&e).send_to_client();
            return;
        }
    };
    // lgtm[rust/cleartext-logging]
    // `Settings` uses `Redacted<T>` for secrets, so this debug dump is redacted.
    log::debug!("Settings {settings:?}");

    // Short-circuit the ja4 debug probe before finalize_response so that
    // Cache-Control: no-store, private cannot be replaced by operator [response_headers].
    if req.get_method() == FastlyMethod::GET && req.get_path() == "/_ts/debug/ja4" {
        if settings.debug.ja4_endpoint_enabled {
            build_ja4_debug_response(&req).send_to_client();
        } else {
            FastlyResponse::from_status(fastly::http::StatusCode::NOT_FOUND).send_to_client();
        }
        return;
    }

    // Build the auction orchestrator once at startup
    let orchestrator = match build_orchestrator(&settings) {
        Ok(orchestrator) => orchestrator,
        Err(e) => {
            log::error!("Failed to build auction orchestrator: {:?}", e);
            to_error_response(&e).send_to_client();
            return;
        }
    };

    let integration_registry = match IntegrationRegistry::new(&settings) {
        Ok(r) => r,
        Err(e) => {
            log::error!("Failed to create integration registry: {:?}", e);
            to_error_response(&e).send_to_client();
            return;
        }
    };

    let partner_registry = match PartnerRegistry::from_config(&settings.ec.partners) {
        Ok(registry) => registry,
        Err(e) => {
            log::error!("Failed to build partner registry: {:?}", e);
            to_error_response(&e).send_to_client();
            return;
        }
    };

    // Start with an unavailable primary KV slot. EC-backed routes lazily
    // replace it with the configured EC identity store at dispatch time so
    // unrelated routes stay available when EC KV is unavailable.
    let kv_store = std::sync::Arc::new(UnavailableKvStore)
        as std::sync::Arc<dyn trusted_server_core::platform::PlatformKvStore>;
    // Strip client-spoofable forwarded headers at the edge before building
    // any request-derived context or converting to the core HTTP types.
    compat::sanitize_fastly_forwarded_headers(&mut req);

    let runtime_services = build_runtime_services(&req, kv_store);
    let http_req = compat::from_fastly_request(req);

    let route_result = futures::executor::block_on(route_request(
        &settings,
        &orchestrator,
        &integration_registry,
        &partner_registry,
        &runtime_services,
        http_req,
    ))
    .unwrap_or_else(|e| RouteResult {
        outcome: HandlerOutcome::Buffered(http_error_response(&e)),
        ec_context: EcContext::default(),
        finalize_kv_graph: None,
        eids_cookie: None,
        sharedid_cookie: None,
        is_real_browser: false,
    });

    let RouteResult {
        outcome,
        ec_context,
        finalize_kv_graph,
        eids_cookie,
        sharedid_cookie,
        is_real_browser,
    } = route_result;

    // Skip geo lookup for our own auth challenges: avoids exposing geo headers to
    // unauthenticated callers.  Origin-forwarded 401s are not AuthChallenge and
    // do receive geo headers — the client already reached the origin anyway.
    let geo_info = if matches!(outcome, HandlerOutcome::AuthChallenge(_)) {
        None
    } else {
        runtime_services
            .geo()
            .lookup(runtime_services.client_info().client_ip)
            .unwrap_or_else(|e| {
                log::warn!("geo lookup failed: {e}");
                None
            })
    };

    match outcome {
        HandlerOutcome::Buffered(mut response) | HandlerOutcome::AuthChallenge(mut response) => {
            finalize_response(&settings, geo_info.as_ref(), &mut response);
            let mut fastly_resp = compat::to_fastly_response(response);
            ec_finalize_response(
                &settings,
                &ec_context,
                finalize_kv_graph.as_ref(),
                &partner_registry,
                eids_cookie.as_deref(),
                sharedid_cookie.as_deref(),
                &mut fastly_resp,
            );
            fastly_resp.send_to_client();

            if is_real_browser {
                if let Some(context) = build_pull_sync_context(&ec_context) {
                    run_pull_sync_after_send(&settings, &partner_registry, &context);
                }
            }
        }
        HandlerOutcome::Streaming {
            mut response,
            body,
            params,
        } => {
            finalize_response(&settings, geo_info.as_ref(), &mut response);
            let mut fastly_resp = compat::to_fastly_response_skeleton(response);
            ec_finalize_response(
                &settings,
                &ec_context,
                finalize_kv_graph.as_ref(),
                &partner_registry,
                eids_cookie.as_deref(),
                sharedid_cookie.as_deref(),
                &mut fastly_resp,
            );
            let mut streaming_body = fastly_resp.stream_to_client();
            let mut stream_succeeded = false;
            match stream_publisher_body(
                body,
                &mut streaming_body,
                &params,
                &settings,
                &integration_registry,
            ) {
                Ok(()) => {
                    if let Err(e) = streaming_body.finish() {
                        log::error!("failed to finish streaming body: {e}");
                    } else {
                        stream_succeeded = true;
                    }
                }
                Err(e) => {
                    log::error!("streaming processing failed: {e:?}");
                    // Headers already committed. Drop the body so the client sees a
                    // truncated response (EOF mid-stream) — standard proxy behavior.
                    drop(streaming_body);
                }
            }

            if is_real_browser && stream_succeeded {
                if let Some(context) = build_pull_sync_context(&ec_context) {
                    run_pull_sync_after_send(&settings, &partner_registry, &context);
                }
            }
        }
    }
}

const FALLBACK_UNAVAILABLE: &str = "unavailable";
const FALLBACK_NOT_SENT: &str = "not sent";
const FALLBACK_NONE: &str = "none";

// TODO: remove after JA4 evaluation completes — see #645
fn build_ja4_debug_response(req: &FastlyRequest) -> FastlyResponse {
    let ja4 = req.get_tls_ja4().unwrap_or(FALLBACK_UNAVAILABLE);
    let h2 = req
        .get_client_h2_fingerprint()
        .unwrap_or(FALLBACK_UNAVAILABLE);
    let cipher = req
        .get_tls_cipher_openssl_name()
        .unwrap_or(FALLBACK_UNAVAILABLE);
    let tls_version = req.get_tls_protocol().unwrap_or(FALLBACK_UNAVAILABLE);
    let ua = req.get_header_str("user-agent").unwrap_or(FALLBACK_NONE);
    let ch_mobile = req
        .get_header_str("sec-ch-ua-mobile")
        .unwrap_or(FALLBACK_NOT_SENT);
    let ch_platform = req
        .get_header_str("sec-ch-ua-platform")
        .unwrap_or(FALLBACK_NOT_SENT);

    let body = format!(
        "ja4:         {ja4}\n\
         h2_fp:       {h2}\n\
         cipher:      {cipher}\n\
         tls_version: {tls_version}\n\
         user-agent:  {ua}\n\
         ch-mobile:   {ch_mobile}\n\
         ch-platform: {ch_platform}\n"
    );

    FastlyResponse::from_status(fastly::http::StatusCode::OK)
        .with_header(fastly::http::header::CACHE_CONTROL, "no-store, private")
        .with_header(
            fastly::http::header::VARY,
            "User-Agent, Sec-CH-UA-Mobile, Sec-CH-UA-Platform",
        )
        .with_content_type(fastly::mime::TEXT_PLAIN_UTF_8)
        .with_body(body)
}

async fn route_request(
    settings: &Settings,
    orchestrator: &AuctionOrchestrator,
    integration_registry: &IntegrationRegistry,
    partner_registry: &PartnerRegistry,
    runtime_services: &RuntimeServices,
    req: HttpRequest,
) -> Result<RouteResult, Report<TrustedServerError>> {
    // Build a Fastly request reference for APIs that require fastly types
    // (EcContext, device signals, cookie extraction).  This is headers/method/URI
    // only — body has already been moved into `req`.
    let fastly_req_ref = compat::to_fastly_request_ref(&req);

    // Extract device signals from TLS/H2/UA.  TLS fingerprints are available
    // on the fastly request reference even without the body.
    let device_signals = derive_device_signals(&fastly_req_ref);
    let is_real_browser = device_signals.looks_like_browser();

    if !is_real_browser {
        log::info!(
            "Bot gate: blocking EC operations (ja4={:?}, platform={:?}, is_mobile={})",
            device_signals.ja4_class,
            device_signals.platform_class,
            device_signals.is_mobile,
        );
    }

    // Extract the Prebid EIDs and SharedID cookies before routing.
    let eids_cookie = extract_cookie_value(&fastly_req_ref, COOKIE_TS_EIDS);
    let sharedid_cookie = extract_cookie_value(&fastly_req_ref, COOKIE_SHAREDID);

    // Extract geo info.
    let geo_info = runtime_services
        .geo()
        .lookup(runtime_services.client_info().client_ip)
        .unwrap_or_else(|e| {
            log::warn!("geo lookup failed during routing: {e}");
            None
        });

    // S2S batch sync — uses Bearer auth (not EC cookies), so skip EC
    // context creation and the EC finalize middleware entirely.
    if req.method() == Method::POST && req.uri().path() == "/_ts/api/v1/batch-sync" {
        match enforce_basic_auth(settings, &req) {
            Ok(Some(response)) => {
                return Ok(RouteResult {
                    outcome: HandlerOutcome::AuthChallenge(response),
                    ec_context: EcContext::default(),
                    finalize_kv_graph: None,
                    eids_cookie,
                    sharedid_cookie,
                    is_real_browser,
                });
            }
            Ok(None) => {}
            Err(e) => return Err(e),
        }
        let fastly_req = compat::to_fastly_request(req);
        let result = require_identity_graph(settings).and_then(|kv| {
            let limiter = FastlyRateLimiter::new(RATE_COUNTER_NAME);
            handle_batch_sync(&kv, partner_registry, &limiter, fastly_req)
        });
        let outcome = match result {
            Ok(fastly_resp) => HandlerOutcome::Buffered(compat::from_fastly_response(fastly_resp)),
            Err(e) => HandlerOutcome::Buffered(http_error_response(&e)),
        };
        return Ok(RouteResult {
            outcome,
            ec_context: EcContext::default(),
            finalize_kv_graph: None,
            eids_cookie,
            sharedid_cookie,
            is_real_browser,
        });
    }

    // Build EC context using the fastly request reference (headers/method/URI).
    let mut ec_context =
        match EcContext::read_from_request_with_geo(settings, &fastly_req_ref, geo_info.as_ref()) {
            Ok(context) => context,
            Err(err) => {
                return Ok(RouteResult {
                    outcome: HandlerOutcome::Buffered(http_error_response(&err)),
                    ec_context: EcContext::default(),
                    finalize_kv_graph: None,
                    eids_cookie,
                    sharedid_cookie,
                    is_real_browser,
                });
            }
        };

    // Pass device signals to EcContext so they are stored on new entries.
    ec_context.set_device_signals(device_signals);

    // Bot gate: suppress KV-backed EC writes for unrecognized clients, except
    // consent withdrawals. Revocations need the KV graph so tombstones remain
    // authoritative even for privacy-extension-heavy clients that do not look
    // like known browsers.
    let kv_graph = if is_real_browser {
        maybe_identity_graph(settings)
    } else {
        None
    };
    let finalize_kv_graph = if is_real_browser || ec_consent_withdrawn(ec_context.consent()) {
        maybe_identity_graph(settings)
    } else {
        None
    };

    // `get_settings()` should already have rejected invalid handler regexes.
    // Keep this fallback so manually-constructed or otherwise unprepared
    // settings still become an error response instead of panicking.
    match enforce_basic_auth(settings, &req) {
        Ok(Some(response)) => {
            return Ok(RouteResult {
                outcome: HandlerOutcome::AuthChallenge(response),
                ec_context,
                finalize_kv_graph,
                eids_cookie,
                sharedid_cookie,
                is_real_browser,
            });
        }
        Ok(None) => {}
        Err(e) => return Err(e),
    }

    // Get path and method for routing
    let path = req.uri().path().to_string();
    let method = req.method().clone();

    // Match known routes and handle them
    let (result, organic_route) = match (method, path.as_str()) {
        // Serve the tsjs library
        (Method::GET, path) if path.starts_with("/static/tsjs=") => {
            (handle_tsjs_dynamic(&req, integration_registry), false)
        }

        // Discovery endpoint for trusted-server capabilities and JWKS
        (Method::GET, "/.well-known/trusted-server.json") => (
            handle_trusted_server_discovery(settings, runtime_services, req),
            false,
        ),

        // Signature verification endpoint
        (Method::POST, "/verify-signature") => (
            handle_verify_signature(settings, runtime_services, req),
            false,
        ),

        // Admin endpoints
        // Keep in sync with Settings::ADMIN_ENDPOINTS in crates/trusted-server-core/src/settings.rs
        (Method::POST, "/admin/keys/rotate") | (Method::POST, "/_ts/admin/keys/rotate") => {
            (handle_rotate_key(settings, runtime_services, req), false)
        }
        (Method::POST, "/admin/keys/deactivate") | (Method::POST, "/_ts/admin/keys/deactivate") => {
            (
                handle_deactivate_key(settings, runtime_services, req),
                false,
            )
        }
        (Method::GET, "/_ts/api/v1/identify") => {
            let fastly_ref = compat::to_fastly_request_ref(&req);
            let outcome = require_identity_graph(settings).and_then(|kv| {
                handle_identify(settings, &kv, partner_registry, &fastly_ref, &ec_context)
                    .map(compat::from_fastly_response)
            });
            (outcome, false)
        }
        (Method::OPTIONS, "/_ts/api/v1/identify") => {
            let fastly_ref = compat::to_fastly_request_ref(&req);
            let outcome =
                cors_preflight_identify(settings, &fastly_ref).map(compat::from_fastly_response);
            (outcome, false)
        }

        // Unified auction endpoint (returns creative HTML inline)
        (Method::POST, "/auction") => {
            let registry_ref = if partner_registry.is_empty() {
                None
            } else {
                Some(partner_registry)
            };
            (
                handle_auction(
                    settings,
                    orchestrator,
                    kv_graph.as_ref(),
                    registry_ref,
                    &ec_context,
                    runtime_services,
                    req,
                )
                .await,
                false,
            )
        }

        // First-party proxy/click/sign/rebuild endpoints
        (Method::GET, "/first-party/proxy") => (
            handle_first_party_proxy(settings, runtime_services, req).await,
            false,
        ),
        (Method::GET, "/first-party/click") => (
            handle_first_party_click(settings, runtime_services, req).await,
            false,
        ),
        (Method::GET, "/first-party/sign") | (Method::POST, "/first-party/sign") => (
            handle_first_party_proxy_sign(settings, runtime_services, req).await,
            false,
        ),
        (Method::POST, "/first-party/proxy-rebuild") => (
            handle_first_party_proxy_rebuild(settings, runtime_services, req).await,
            false,
        ),
        (m, path) if integration_registry.has_route(&m, path) => {
            let fastly_req = compat::to_fastly_request(req);
            let result = integration_registry
                .handle_proxy(ProxyDispatchInput {
                    method: &m,
                    path,
                    settings,
                    kv: kv_graph.as_ref(),
                    ec_context: &mut ec_context,
                    services: runtime_services,
                    req: fastly_req,
                })
                .await
                .unwrap_or_else(|| {
                    Err(Report::new(TrustedServerError::BadRequest {
                        message: format!("Unknown integration route: {path}"),
                    }))
                })
                .map(compat::from_fastly_response);
            (result, true)
        }

        // No known route matched, proxy to publisher origin as fallback
        _ => {
            log::info!(
                "No known route matched for path: {}, proxying to publisher origin",
                path
            );

            // Generate EC ID if needed — mirrors the integration proxy path in registry.rs.
            // Only for document navigations by recognised browsers; subresource requests
            // may lack consent signals such as Sec-GPC.
            if is_real_browser && is_navigation_request(&req) {
                if let Err(err) = ec_context.generate_if_needed(settings, kv_graph.as_ref()) {
                    log::warn!("EC generation failed for publisher proxy: {err:?}");
                }
            }

            match handle_publisher_request(settings, integration_registry, runtime_services, req)
                .await
            {
                Ok(PublisherResponse::Stream {
                    response,
                    body,
                    params,
                }) => {
                    return Ok(RouteResult {
                        outcome: HandlerOutcome::Streaming {
                            response,
                            body,
                            params,
                        },
                        ec_context,
                        finalize_kv_graph,
                        eids_cookie,
                        sharedid_cookie,
                        is_real_browser,
                    });
                }
                Ok(PublisherResponse::PassThrough { mut response, body }) => {
                    *response.body_mut() = body;
                    (Ok(response), true)
                }
                Ok(PublisherResponse::Buffered(response)) => (Ok(response), true),
                Err(e) => {
                    log::error!("Failed to proxy to publisher origin: {:?}", e);
                    (Err(e), true)
                }
            }
        }
    };

    let _ = organic_route;

    let outcome = result
        .map(HandlerOutcome::Buffered)
        .unwrap_or_else(|e| HandlerOutcome::Buffered(http_error_response(&e)));

    Ok(RouteResult {
        outcome,
        ec_context,
        finalize_kv_graph,
        eids_cookie,
        sharedid_cookie,
        is_real_browser,
    })
}

fn maybe_identity_graph(settings: &Settings) -> Option<KvIdentityGraph> {
    settings.ec.ec_store.as_ref().map(KvIdentityGraph::new)
}

fn run_pull_sync_after_send(
    settings: &Settings,
    partner_registry: &PartnerRegistry,
    context: &PullSyncContext,
) {
    let kv = match require_identity_graph(settings) {
        Ok(kv) => kv,
        Err(err) => {
            log::debug!("Pull sync: identity graph unavailable, skipping: {err:?}");
            return;
        }
    };

    let limiter = FastlyRateLimiter::new(RATE_COUNTER_NAME);
    dispatch_pull_sync(settings, &kv, partner_registry, &limiter, context);
}

/// Applies all standard response headers: geo, version, staging, and configured headers.
///
/// Called from every response path (including auth early-returns) so that all
/// outgoing responses carry a consistent set of Trusted Server headers.
///
/// Header precedence (last write wins): geo headers are set first, then
/// version/staging, then operator-configured `settings.response_headers`.
/// This means operators can intentionally override any managed header.
fn finalize_response(settings: &Settings, geo_info: Option<&GeoInfo>, response: &mut HttpResponse) {
    if let Some(geo) = geo_info {
        geo.set_response_headers(response);
    } else {
        response.headers_mut().insert(
            HEADER_X_GEO_INFO_AVAILABLE,
            HeaderValue::from_static("false"),
        );
    }

    if let Ok(v) = ::std::env::var(ENV_FASTLY_SERVICE_VERSION) {
        if let Ok(value) = HeaderValue::from_str(&v) {
            response.headers_mut().insert(HEADER_X_TS_VERSION, value);
        } else {
            log::warn!("Skipping invalid FASTLY_SERVICE_VERSION response header value");
        }
    }
    if ::std::env::var(ENV_FASTLY_IS_STAGING).as_deref() == Ok("1") {
        response
            .headers_mut()
            .insert(HEADER_X_TS_ENV, HeaderValue::from_static("staging"));
    }

    for (key, value) in &settings.response_headers {
        let header_name = HeaderName::from_bytes(key.as_bytes())
            .expect("settings.response_headers validated at load time");
        let header_value =
            HeaderValue::from_str(value).expect("settings.response_headers validated at load time");
        response.headers_mut().insert(header_name, header_value);
    }
}

fn http_error_response(report: &Report<TrustedServerError>) -> HttpResponse {
    let root_error = report.current_context();
    log::error!("Error occurred: {:?}", report);

    let mut response =
        HttpResponse::new(EdgeBody::from(format!("{}\n", root_error.user_message())));
    *response.status_mut() = root_error.status_code();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    response
}

/// Constructs a `KvIdentityGraph` from settings, or returns an error if the
/// `ec_store` config is not set.
fn require_identity_graph(
    settings: &Settings,
) -> Result<KvIdentityGraph, Report<TrustedServerError>> {
    let store_name = settings.ec.ec_store.as_deref().ok_or_else(|| {
        Report::new(TrustedServerError::KvStore {
            store_name: "ec.ec_store".to_owned(),
            message: "ec.ec_store is not configured".to_owned(),
        })
    })?;
    Ok(KvIdentityGraph::new(store_name))
}

/// Extracts a named cookie value from the request's `Cookie` header.
fn extract_cookie_value(req: &FastlyRequest, name: &str) -> Option<String> {
    let cookie_header = req.get_header_str("cookie")?;
    for pair in cookie_header.split(';') {
        let pair = pair.trim();
        if let Some((key, value)) = pair.split_once('=') {
            if key.trim() == name {
                return Some(value.trim().to_owned());
            }
        }
    }
    None
}

/// Derives device signals from TLS, H2, and UA request data.
///
/// All extraction is pure in-memory — no KV I/O. The Fastly SDK provides
/// `get_tls_ja4()` and `get_client_h2_fingerprint()` on client requests.
fn derive_device_signals(req: &FastlyRequest) -> DeviceSignals {
    let ua = req.get_header_str("user-agent").unwrap_or("");
    let ja4 = req.get_tls_ja4();
    let h2_fp = req.get_client_h2_fingerprint();

    DeviceSignals::derive(ua, ja4, h2_fp)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fastly::mime;

    #[test]
    fn ja4_debug_response_uses_plain_text_and_fallback_values() {
        let req = FastlyRequest::get("https://example.com/_ts/debug/ja4");

        let mut response = build_ja4_debug_response(&req);

        assert_eq!(
            response.get_status(),
            fastly::http::StatusCode::OK,
            "should return 200 OK"
        );
        assert_eq!(
            response.get_content_type(),
            Some(mime::TEXT_PLAIN_UTF_8),
            "should return plain text content"
        );
        assert_eq!(
            response.get_header_str(fastly::http::header::CACHE_CONTROL),
            Some("no-store, private"),
            "should disable caching for the debug response"
        );

        let body = response.take_body_str();

        assert!(
            body.contains("ja4:         unavailable"),
            "should include JA4 fallback"
        );
        assert!(
            body.contains("h2_fp:       unavailable"),
            "should include H2 fingerprint fallback"
        );
        assert!(
            body.contains("cipher:      unavailable"),
            "should include cipher fallback"
        );
        assert!(
            body.contains("tls_version: unavailable"),
            "should include TLS version fallback"
        );
        assert!(
            body.contains("user-agent:  none"),
            "should include user-agent fallback"
        );
        assert!(
            body.contains("ch-mobile:   not sent"),
            "should include sec-ch-ua-mobile fallback"
        );
        assert!(
            body.contains("ch-platform: not sent"),
            "should include sec-ch-ua-platform fallback"
        );
    }
}
