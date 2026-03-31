use error_stack::Report;
use fastly::http::Method;
use fastly::{Error, Request, Response};
use log_fastly::Logger;

use trusted_server_core::auction::endpoints::handle_auction;
use trusted_server_core::auction::{build_orchestrator, AuctionOrchestrator};
use trusted_server_core::auth::enforce_basic_auth;
use trusted_server_core::constants::{
    ENV_FASTLY_IS_STAGING, ENV_FASTLY_SERVICE_VERSION, HEADER_X_GEO_INFO_AVAILABLE,
    HEADER_X_TS_ENV, HEADER_X_TS_VERSION,
};
use trusted_server_core::ec::admin::handle_register_partner;
use trusted_server_core::ec::batch_sync::handle_batch_sync;
use trusted_server_core::ec::finalize::ec_finalize_response;
use trusted_server_core::ec::identify::{cors_preflight_identify, handle_identify};
use trusted_server_core::ec::kv::KvIdentityGraph;
use trusted_server_core::ec::partner::PartnerStore;
use trusted_server_core::ec::pull_sync::{
    build_pull_sync_context, dispatch_pull_sync, PullSyncContext,
};
use trusted_server_core::ec::sync_pixel::{handle_sync, FastlyRateLimiter, RATE_COUNTER_NAME};
use trusted_server_core::ec::EcContext;
use trusted_server_core::error::TrustedServerError;
use trusted_server_core::geo::GeoInfo;
use trusted_server_core::http_util::sanitize_forwarded_headers;
use trusted_server_core::integrations::IntegrationRegistry;
use trusted_server_core::proxy::{
    handle_first_party_click, handle_first_party_proxy, handle_first_party_proxy_rebuild,
    handle_first_party_proxy_sign,
};
use trusted_server_core::publisher::{handle_publisher_request, handle_tsjs_dynamic};
use trusted_server_core::request_signing::{
    handle_deactivate_key, handle_rotate_key, handle_trusted_server_discovery,
    handle_verify_signature,
};
use trusted_server_core::settings::Settings;
use trusted_server_core::settings_data::get_settings;

mod error;
use crate::error::to_error_response;

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

    let outcome = futures::executor::block_on(route_request(
        &settings,
        &orchestrator,
        &integration_registry,
        req,
    ))?;

    let RouteOutcome {
        response,
        pull_sync_context,
    } = outcome;

    response.send_to_client();

    if let Some(context) = pull_sync_context {
        run_pull_sync_after_send(&settings, &context);
    }

    Ok(())
}

#[must_use]
struct RouteOutcome {
    response: Response,
    pull_sync_context: Option<PullSyncContext>,
}

async fn route_request(
    settings: &Settings,
    orchestrator: &AuctionOrchestrator,
    integration_registry: &IntegrationRegistry,
    mut req: Request,
) -> Result<RouteOutcome, Error> {
    // Strip client-spoofable forwarded headers at the edge.
    // On Fastly this service IS the first proxy — these headers from
    // clients are untrusted and can hijack URL rewriting (see #409).
    sanitize_forwarded_headers(&mut req);

    // Extract geo info before auth check or routing consumes the request.
    let geo_info = GeoInfo::from_request(&req);

    // S2S batch sync — uses Bearer auth (not EC cookies), so skip EC
    // context creation and the EC finalize middleware entirely.
    if req.get_method() == Method::POST && req.get_path() == "/api/v1/sync" {
        let mut response = require_identity_graph(settings)
            .and_then(|kv| {
                require_partner_store(settings).and_then(|partner_store| {
                    let limiter = FastlyRateLimiter::new(RATE_COUNTER_NAME);
                    handle_batch_sync(&kv, &partner_store, &limiter, req)
                })
            })
            .unwrap_or_else(|e| to_error_response(&e));
        finalize_response(settings, geo_info.as_ref(), &mut response);
        return Ok(RouteOutcome {
            response,
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
                    response,
                    pull_sync_context: None,
                });
            }
        };

    let kv_graph = maybe_identity_graph(settings);

    // `get_settings()` should already have rejected invalid handler regexes.
    // Keep this fallback so manually-constructed or otherwise unprepared
    // settings still become an error response instead of panicking.
    match enforce_basic_auth(settings, &req) {
        Ok(Some(mut response)) => {
            ec_finalize_response(
                settings,
                geo_info.as_ref(),
                &ec_context,
                kv_graph.as_ref(),
                &mut response,
            );
            finalize_response(settings, geo_info.as_ref(), &mut response);
            return Ok(RouteOutcome {
                response,
                pull_sync_context: None,
            });
        }
        Ok(None) => {}
        Err(e) => {
            log::error!("Failed to evaluate basic auth: {:?}", e);
            let mut response = to_error_response(&e);
            finalize_response(settings, geo_info.as_ref(), &mut response);
            return Ok(RouteOutcome {
                response,
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
        (Method::GET, "/.well-known/trusted-server.json") => {
            (handle_trusted_server_discovery(settings, req), false)
        }

        // Signature verification endpoint
        (Method::POST, "/verify-signature") => (handle_verify_signature(settings, req), false),

        // Admin endpoints
        // Keep in sync with Settings::ADMIN_ENDPOINTS in crates/trusted-server-core/src/settings.rs
        (Method::POST, "/admin/keys/rotate") => (handle_rotate_key(settings, req), false),
        (Method::POST, "/admin/keys/deactivate") => (handle_deactivate_key(settings, req), false),
        (Method::POST, "/admin/partners/register") => (
            require_partner_store(settings).and_then(|store| handle_register_partner(&store, req)),
            false,
        ),

        (Method::GET, "/sync") => (
            require_identity_graph(settings).and_then(|kv| {
                require_partner_store(settings).and_then(|partner_store| {
                    handle_sync(settings, &kv, &partner_store, &req, &mut ec_context)
                })
            }),
            false,
        ),
        (Method::GET, "/identify") => (
            require_identity_graph(settings).and_then(|kv| {
                require_partner_store(settings).and_then(|partner_store| {
                    handle_identify(settings, &kv, &partner_store, &req, &ec_context)
                })
            }),
            false,
        ),
        (Method::OPTIONS, "/identify") => (cors_preflight_identify(settings, &req), false),

        // Unified auction endpoint (returns creative HTML inline)
        (Method::POST, "/auction") => {
            let partner_store = require_partner_store(settings).ok();
            (
                handle_auction(
                    settings,
                    orchestrator,
                    kv_graph.as_ref(),
                    partner_store.as_ref(),
                    &ec_context,
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

            let result = match handle_publisher_request(
                settings,
                integration_registry,
                kv_graph.as_ref(),
                &mut ec_context,
                req,
            ) {
                Ok(response) => Ok(response),
                Err(e) => {
                    log::error!("Failed to proxy to publisher origin: {:?}", e);
                    Err(e)
                }
            };
            (result, true)
        }
    };

    let route_succeeded = result.is_ok();

    // Convert any errors to HTTP error responses
    let mut response = result.unwrap_or_else(|e| to_error_response(&e));

    ec_finalize_response(
        settings,
        geo_info.as_ref(),
        &ec_context,
        kv_graph.as_ref(),
        &mut response,
    );

    finalize_response(settings, geo_info.as_ref(), &mut response);

    let pull_sync_context = if organic_route && route_succeeded {
        build_pull_sync_context(&ec_context)
    } else {
        None
    };

    Ok(RouteOutcome {
        response,
        pull_sync_context,
    })
}

fn maybe_identity_graph(settings: &Settings) -> Option<KvIdentityGraph> {
    settings.ec.ec_store.as_ref().map(KvIdentityGraph::new)
}

fn run_pull_sync_after_send(settings: &Settings, context: &PullSyncContext) {
    let kv = match require_identity_graph(settings) {
        Ok(kv) => kv,
        Err(err) => {
            log::debug!("Pull sync: identity graph unavailable, skipping: {err:?}");
            return;
        }
    };

    let partner_store = match require_partner_store(settings) {
        Ok(store) => store,
        Err(err) => {
            log::debug!("Pull sync: partner store unavailable, skipping: {err:?}");
            return;
        }
    };

    let limiter = FastlyRateLimiter::new(RATE_COUNTER_NAME);
    dispatch_pull_sync(settings, &kv, &partner_store, &limiter, context);
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

fn init_logger() {
    let logger = Logger::builder()
        .default_endpoint("tslog")
        .echo_stdout(true)
        .max_level(log::LevelFilter::Info)
        .build()
        .expect("should build Logger");

    fern::Dispatch::new()
        .format(|out, message, record| {
            out.finish(format_args!(
                "{} {} [{}] {}",
                chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                record.level(),
                record
                    .target()
                    .split("::")
                    .last()
                    .unwrap_or(record.target()),
                message
            ))
        })
        .chain(Box::new(logger) as Box<dyn log::Log>)
        .apply()
        .expect("should initialize logger");
}

/// Constructs a `PartnerStore` from settings, or returns 503 if the
/// `partner_store` config is not set.
fn require_partner_store(settings: &Settings) -> Result<PartnerStore, Report<TrustedServerError>> {
    let store_name = settings.ec.partner_store.as_deref().ok_or_else(|| {
        Report::new(TrustedServerError::KvStore {
            store_name: "ec.partner_store".to_owned(),
            message: "ec.partner_store is not configured".to_owned(),
        })
    })?;
    Ok(PartnerStore::new(store_name))
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
