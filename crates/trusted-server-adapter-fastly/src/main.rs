use error_stack::Report;
use fastly::http::{header, Method};
use fastly::{Error, Request, Response};

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
use trusted_server_core::error::TrustedServerError;
use trusted_server_core::geo::GeoInfo;
use trusted_server_core::integrations::{IntegrationRegistry, ProxyDispatchInput};
use trusted_server_core::platform::RuntimeServices;
use trusted_server_core::proxy::{
    handle_first_party_click, handle_first_party_proxy, handle_first_party_proxy_rebuild,
    handle_first_party_proxy_sign,
};
use trusted_server_core::publisher::{
    handle_page_bids, handle_publisher_request, handle_tsjs_dynamic, stream_publisher_body_async,
    PublisherResponse,
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

fn main() -> Result<(), Error> {
    init_logger();

    let req = Request::from_client();

    // Keep the health probe independent from settings loading and routing so
    // readiness checks still get a cheap liveness response during startup.
    if req.get_method() == Method::GET && req.get_path() == "/health" {
        Response::from_status(200)
            .with_body_text_plain("ok")
            .send_to_client();
        return Ok(());
    }

    let settings = match get_settings() {
        Ok(s) => s,
        Err(e) => {
            log::error!("Failed to load settings: {:?}", e);
            to_error_response(&e).send_to_client();
            return Ok(());
        }
    };
    // lgtm[rust/cleartext-logging]
    // `Settings` uses `Redacted<T>` for secrets, so this debug dump is redacted.
    log::debug!("Settings {settings:?}");

    // Short-circuit the ja4 debug probe before finalize_response so that
    // Cache-Control: no-store, private cannot be replaced by operator [response_headers].
    if req.get_method() == Method::GET && req.get_path() == "/_ts/debug/ja4" {
        if settings.debug.ja4_endpoint_enabled {
            build_ja4_debug_response(&req).send_to_client();
        } else {
            Response::from_status(fastly::http::StatusCode::NOT_FOUND).send_to_client();
        }
        return Ok(());
    }

    // Build the auction orchestrator once at startup
    let orchestrator = match build_orchestrator(&settings) {
        Ok(orchestrator) => orchestrator,
        Err(e) => {
            log::error!("Failed to build auction orchestrator: {:?}", e);
            to_error_response(&e).send_to_client();
            return Ok(());
        }
    };

    let integration_registry = match IntegrationRegistry::new(&settings) {
        Ok(r) => r,
        Err(e) => {
            log::error!("Failed to create integration registry: {:?}", e);
            to_error_response(&e).send_to_client();
            return Ok(());
        }
    };

    let partner_registry = match PartnerRegistry::from_config(&settings.ec.partners) {
        Ok(registry) => registry,
        Err(e) => {
            log::error!("Failed to build partner registry: {:?}", e);
            to_error_response(&e).send_to_client();
            return Ok(());
        }
    };

    // Start with an unavailable primary KV slot. EC-backed routes lazily
    // replace it with the configured EC identity store at dispatch time so
    // unrelated routes stay available when EC KV is unavailable.
    let kv_store = std::sync::Arc::new(UnavailableKvStore)
        as std::sync::Arc<dyn trusted_server_core::platform::PlatformKvStore>;
    let runtime_services = build_runtime_services(&req, kv_store);

    let outcome = futures::executor::block_on(route_request(
        &settings,
        &orchestrator,
        &integration_registry,
        &partner_registry,
        &runtime_services,
        settings.creative_opportunity_slots(),
        req,
    ))?;

    let RouteOutcome {
        response,
        pull_sync_context,
    } = outcome;

    if let Some(response) = response {
        response.send_to_client();
    }

    if let Some(context) = pull_sync_context {
        run_pull_sync_after_send(&settings, &partner_registry, &context);
    }

    Ok(())
}

#[must_use]
struct RouteOutcome {
    response: Option<Response>,
    pull_sync_context: Option<PullSyncContext>,
}

const FALLBACK_UNAVAILABLE: &str = "unavailable";
const FALLBACK_NOT_SENT: &str = "not sent";
const FALLBACK_NONE: &str = "none";

// TODO: remove after JA4 evaluation completes — see #645
fn build_ja4_debug_response(req: &Request) -> Response {
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

    Response::from_status(fastly::http::StatusCode::OK)
        .with_header(header::CACHE_CONTROL, "no-store, private")
        .with_header(
            header::VARY,
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
    slots: &[trusted_server_core::creative_opportunities::CreativeOpportunitySlot],
    mut req: Request,
) -> Result<RouteOutcome, Error> {
    // Strip client-spoofable forwarded headers at the edge.
    // On Fastly this service IS the first proxy — these headers from
    // clients are untrusted and can hijack URL rewriting (see #409).
    compat::sanitize_fastly_forwarded_headers(&mut req);

    // Extract geo info before auth check or routing consumes the request.
    #[allow(deprecated)]
    let geo_info = GeoInfo::from_request(&req);

    // Extract the Prebid EIDs cookie before routing consumes the request.
    let eids_cookie = extract_cookie_value(&req, COOKIE_TS_EIDS);
    let sharedid_cookie = extract_cookie_value(&req, COOKIE_SHAREDID);

    // Derive device signals from TLS/H2/UA for bot detection.
    // This is pure in-memory computation — no KV I/O.
    let device_signals = derive_device_signals(&req);
    let is_real_browser = device_signals.looks_like_browser();

    if !is_real_browser {
        log::info!(
            "Bot gate: blocking EC operations (ja4={:?}, platform={:?}, is_mobile={})",
            device_signals.ja4_class,
            device_signals.platform_class,
            device_signals.is_mobile,
        );
    }

    // S2S batch sync — uses Bearer auth (not EC cookies), so skip EC
    // context creation and the EC finalize middleware entirely.
    if req.get_method() == Method::POST && req.get_path() == "/_ts/api/v1/batch-sync" {
        let auth_req = compat::from_fastly_headers_ref(&req);
        let mut response = match enforce_basic_auth(settings, &auth_req) {
            Ok(Some(response)) => compat::to_fastly_response(response),
            Ok(None) => require_identity_graph(settings)
                .map(|kv| {
                    let limiter = FastlyRateLimiter::new(RATE_COUNTER_NAME);
                    handle_batch_sync(&kv, partner_registry, &limiter, req)
                })
                .and_then(|r| r)
                .unwrap_or_else(|e| to_error_response(&e)),
            Err(e) => {
                log::error!("Failed to evaluate basic auth: {:?}", e);
                to_error_response(&e)
            }
        };
        finalize_response(settings, geo_info.as_ref(), &mut response);
        return Ok(RouteOutcome {
            response: Some(response),
            pull_sync_context: None,
        });
    }

    let mut ec_context =
        match EcContext::read_from_request_with_geo(settings, &req, geo_info.as_ref()) {
            Ok(context) => context,
            Err(err) => {
                let mut response = to_error_response(&err);
                finalize_response(settings, geo_info.as_ref(), &mut response);
                return Ok(RouteOutcome {
                    response: Some(response),
                    pull_sync_context: None,
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
    let auth_req = compat::from_fastly_headers_ref(&req);
    match enforce_basic_auth(settings, &auth_req) {
        Ok(Some(response)) => {
            let mut response = compat::to_fastly_response(response);
            ec_finalize_response(
                settings,
                &ec_context,
                finalize_kv_graph.as_ref(),
                partner_registry,
                eids_cookie.as_deref(),
                sharedid_cookie.as_deref(),
                &mut response,
            );
            finalize_response(settings, geo_info.as_ref(), &mut response);
            return Ok(RouteOutcome {
                response: Some(response),
                pull_sync_context: None,
            });
        }
        Ok(None) => {}
        Err(e) => {
            log::error!("Failed to evaluate basic auth: {:?}", e);
            let mut response = to_error_response(&e);
            finalize_response(settings, geo_info.as_ref(), &mut response);
            return Ok(RouteOutcome {
                response: Some(response),
                pull_sync_context: None,
            });
        }
    }

    // Get path and method for routing
    let path = req.get_path().to_string();
    let method = req.get_method().clone();

    let registry_ref = if partner_registry.is_empty() {
        None
    } else {
        Some(partner_registry)
    };

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
        (Method::GET, "/_ts/api/v1/identify") => (
            require_identity_graph(settings)
                .and_then(|kv| handle_identify(settings, &kv, partner_registry, &req, &ec_context)),
            false,
        ),
        (Method::OPTIONS, "/_ts/api/v1/identify") => {
            (cors_preflight_identify(settings, &req), false)
        }

        // Unified auction endpoint (returns creative HTML inline)
        (Method::POST, "/auction") => (
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
        ),

        // SPA/CSR navigation endpoint — returns slots + bids JSON for the given path
        (Method::GET, "/__ts/page-bids") => (
            handle_page_bids(
                settings,
                runtime_services,
                kv_graph.as_ref(),
                trusted_server_core::publisher::AuctionDispatch {
                    orchestrator,
                    slots,
                    registry: registry_ref,
                },
                &ec_context,
                req,
            )
            .await,
            false,
        ),

        // tsjs endpoints
        (Method::GET, "/first-party/proxy") => {
            (handle_first_party_proxy(settings, req).await, false)
        }
        (Method::GET, "/first-party/click") => {
            (handle_first_party_click(settings, req).await, false)
        }
        (Method::GET, "/first-party/sign") | (Method::POST, "/first-party/sign") => {
            (handle_first_party_proxy_sign(settings, req).await, false)
        }
        (Method::POST, "/first-party/proxy-rebuild") => {
            (handle_first_party_proxy_rebuild(settings, req).await, false)
        }
        (m, path) if integration_registry.has_route(&m, path) => {
            let result = integration_registry
                .handle_proxy(ProxyDispatchInput {
                    method: &m,
                    path,
                    settings,
                    kv: kv_graph.as_ref(),
                    ec_context: &mut ec_context,
                    req,
                })
                .await
                .unwrap_or_else(|| {
                    Err(Report::new(TrustedServerError::BadRequest {
                        message: format!("Unknown integration route: {path}"),
                    }))
                });
            (result, true)
        }

        // No known route matched, proxy to publisher origin as fallback
        _ => {
            log::info!(
                "No known route matched for path: {}, proxying to publisher origin",
                path
            );

            match handle_publisher_request(
                settings,
                integration_registry,
                runtime_services,
                kv_graph.as_ref(),
                &mut ec_context,
                trusted_server_core::publisher::AuctionDispatch {
                    orchestrator,
                    slots,
                    registry: registry_ref,
                },
                req,
            )
            .await
            {
                Ok(PublisherResponse::Stream {
                    mut response,
                    body,
                    mut params,
                }) => {
                    // Publisher fallback has multiple delivery modes.
                    // EC finalization is header-only, so it must happen before
                    // headers are committed on the streaming path.
                    ec_finalize_response(
                        settings,
                        &ec_context,
                        finalize_kv_graph.as_ref(),
                        partner_registry,
                        eids_cookie.as_deref(),
                        sharedid_cookie.as_deref(),
                        &mut response,
                    );
                    finalize_response(settings, geo_info.as_ref(), &mut response);

                    let mut streaming_body = response.stream_to_client();
                    let mut stream_succeeded = false;
                    if let Err(err) = stream_publisher_body_async(
                        body,
                        &mut streaming_body,
                        &mut params,
                        settings,
                        integration_registry,
                        orchestrator,
                        runtime_services,
                    )
                    .await
                    {
                        // Headers are already committed. Log and abort rather
                        // than trying to replace the response mid-stream.
                        log::error!("Streaming processing failed: {err:?}");
                        drop(streaming_body);
                    } else if let Err(err) = streaming_body.finish() {
                        log::error!("Failed to finish streaming body: {err}");
                    } else {
                        stream_succeeded = true;
                    }

                    let pull_sync_context = if is_real_browser && stream_succeeded {
                        build_pull_sync_context(&ec_context)
                    } else {
                        None
                    };

                    return Ok(RouteOutcome {
                        response: None,
                        pull_sync_context,
                    });
                }
                Ok(PublisherResponse::PassThrough { mut response, body }) => {
                    response.set_body(body);
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

    let route_succeeded = result.is_ok();

    // Convert any errors to HTTP error responses
    let mut response = result.unwrap_or_else(|e| to_error_response(&e));

    // Bot gate still suppresses generated EC writes and pull sync via
    // `kv_graph = None`, but withdrawal finalization uses `finalize_kv_graph`
    // so revocation tombstones are not lost for bot-classified clients.
    ec_finalize_response(
        settings,
        &ec_context,
        finalize_kv_graph.as_ref(),
        partner_registry,
        eids_cookie.as_deref(),
        sharedid_cookie.as_deref(),
        &mut response,
    );

    finalize_response(settings, geo_info.as_ref(), &mut response);

    let pull_sync_context = if is_real_browser && organic_route && route_succeeded {
        // Pull sync is intentionally refreshed only from successful organic
        // browser traffic. This keeps the trigger narrow in the current PR;
        // broadening it to auction-heavy or SPA-only flows is a follow-up
        // product decision rather than an implicit behavior change here.
        build_pull_sync_context(&ec_context)
    } else {
        None
    };

    Ok(RouteOutcome {
        response: Some(response),
        pull_sync_context,
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
fn finalize_response(settings: &Settings, geo_info: Option<&GeoInfo>, response: &mut Response) {
    if let Some(geo) = geo_info {
        geo.set_response_headers(response);
    } else {
        response.set_header(HEADER_X_GEO_INFO_AVAILABLE, "false");
    }

    if let Ok(v) = ::std::env::var(ENV_FASTLY_SERVICE_VERSION) {
        response.set_header(HEADER_X_TS_VERSION, v);
    }
    if ::std::env::var(ENV_FASTLY_IS_STAGING).as_deref() == Ok("1") {
        response.set_header(HEADER_X_TS_ENV, "staging");
    }

    for (key, value) in &settings.response_headers {
        // Never overwrite a privacy-critical Cache-Control header (private, no-store, etc.)
        // that was set for per-user responses (HTML or page-bids).
        if **key == header::CACHE_CONTROL
            && response
                .get_header(header::CACHE_CONTROL)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.contains("private"))
        {
            continue;
        }
        response.set_header(key, value);
    }
}

/// Constructs a `KvIdentityGraph` from settings, or returns 503 if the
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
fn extract_cookie_value(req: &Request, name: &str) -> Option<String> {
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
fn derive_device_signals(req: &Request) -> DeviceSignals {
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
        let req = Request::get("https://example.com/_ts/debug/ja4");

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
            response.get_header_str(header::CACHE_CONTROL),
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
