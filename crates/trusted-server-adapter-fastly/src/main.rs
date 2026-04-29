use error_stack::Report;
use fastly::http::Method;
use fastly::{Error, Request, Response};

use trusted_server_core::auction::endpoints::handle_auction;
use trusted_server_core::auction::{build_orchestrator, AuctionOrchestrator};
use trusted_server_core::auth::enforce_basic_auth;
use trusted_server_core::constants::{
    COOKIE_SHAREDID, COOKIE_TS_EIDS, ENV_FASTLY_IS_STAGING, ENV_FASTLY_SERVICE_VERSION,
    HEADER_X_GEO_INFO_AVAILABLE, HEADER_X_TS_ENV, HEADER_X_TS_VERSION,
};
use trusted_server_core::ec::batch_sync::handle_batch_sync;
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
use trusted_server_core::http_util::sanitize_forwarded_headers;
use trusted_server_core::integrations::IntegrationRegistry;
use trusted_server_core::platform::RuntimeServices;
use trusted_server_core::proxy::{
    handle_first_party_click, handle_first_party_proxy, handle_first_party_proxy_rebuild,
    handle_first_party_proxy_sign,
};
use trusted_server_core::publisher::{
    handle_publisher_request, handle_tsjs_dynamic, stream_publisher_body, PublisherResponse,
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
        Ok(r) => r,
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

async fn route_request(
    settings: &Settings,
    orchestrator: &AuctionOrchestrator,
    integration_registry: &IntegrationRegistry,
    partner_registry: &PartnerRegistry,
    runtime_services: &RuntimeServices,
    mut req: Request,
) -> Result<RouteOutcome, Error> {
    // Strip client-spoofable forwarded headers at the edge.
    // On Fastly this service IS the first proxy — these headers from
    // clients are untrusted and can hijack URL rewriting (see #409).
    sanitize_forwarded_headers(&mut req);

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
        log::debug!(
            "Bot gate: blocking EC operations (ja4={:?}, platform={:?}, is_mobile={})",
            device_signals.ja4_class,
            device_signals.platform_class,
            device_signals.is_mobile,
        );
    }

    // S2S batch sync — uses Bearer auth (not EC cookies), so skip EC
    // context creation and the EC finalize middleware entirely.
    if req.get_method() == Method::POST && req.get_path() == "/_ts/api/v1/batch-sync" {
        let mut response = match enforce_basic_auth(settings, &req) {
            Ok(Some(response)) => response,
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

    // Bot gate: suppress KV-backed EC side effects for unrecognized clients.
    // Response finalization still runs for revocations/returning-user cookie
    // reconciliation, but generated ECs are not written without a KV graph.
    let kv_graph = if is_real_browser {
        maybe_identity_graph(settings)
    } else {
        None
    };

    // `get_settings()` should already have rejected invalid handler regexes.
    // Keep this fallback so manually-constructed or otherwise unprepared
    // settings still become an error response instead of panicking.
    match enforce_basic_auth(settings, &req) {
        Ok(Some(mut response)) => {
            ec_finalize_response(
                settings,
                &ec_context,
                kv_graph.as_ref(),
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
        (Method::POST, "/_ts/admin/keys/rotate") => {
            (handle_rotate_key(settings, runtime_services, req), false)
        }
        (Method::POST, "/_ts/admin/keys/deactivate") => (
            handle_deactivate_key(settings, runtime_services, req),
            false,
        ),
        (Method::GET, "/_ts/api/v1/identify") => (
            // Bot gate is intentionally write-only in this PR. `/identify` reads
            // remain gated by bearer auth + consent, even when request-classification
            // signals are missing, so previously written EC entries stay queryable.
            require_identity_graph(settings)
                .and_then(|kv| handle_identify(settings, &kv, partner_registry, &req, &ec_context)),
            false,
        ),
        (Method::OPTIONS, "/_ts/api/v1/identify") => {
            (cors_preflight_identify(settings, &req), false)
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
                .handle_proxy(&m, path, settings, kv_graph.as_ref(), &mut ec_context, req)
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
                kv_graph.as_ref(),
                &mut ec_context,
                req,
            ) {
                Ok(PublisherResponse::Stream {
                    mut response,
                    body,
                    params,
                }) => {
                    // Publisher fallback has multiple delivery modes.
                    // EC finalization is header-only, so it must happen before
                    // headers are committed on the streaming path.
                    ec_finalize_response(
                        settings,
                        &ec_context,
                        kv_graph.as_ref(),
                        partner_registry,
                        eids_cookie.as_deref(),
                        sharedid_cookie.as_deref(),
                        &mut response,
                    );
                    finalize_response(settings, geo_info.as_ref(), &mut response);

                    let mut streaming_body = response.stream_to_client();
                    let mut stream_succeeded = false;
                    if let Err(err) = stream_publisher_body(
                        body,
                        &mut streaming_body,
                        &params,
                        settings,
                        integration_registry,
                    ) {
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

    // Bot gate still suppresses KV-backed side effects and pull sync via
    // `kv_graph = None`, but response finalization always runs so cookie
    // writes and revocations behave consistently for browser traffic.
    ec_finalize_response(
        settings,
        &ec_context,
        kv_graph.as_ref(),
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
