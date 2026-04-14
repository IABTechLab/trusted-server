use edgezero_core::body::Body as EdgeBody;
use edgezero_core::http::{
    header, HeaderName, HeaderValue, Method, Request as HttpRequest, Response as HttpResponse,
};
use error_stack::Report;
use fastly::http::Method as FastlyMethod;
use fastly::{Error, Request as FastlyRequest, Response as FastlyResponse};

use trusted_server_core::auction::endpoints::handle_auction;
use trusted_server_core::auction::{build_orchestrator, AuctionOrchestrator};
use trusted_server_core::auth::enforce_basic_auth;
use trusted_server_core::constants::{
    ENV_FASTLY_IS_STAGING, ENV_FASTLY_SERVICE_VERSION, HEADER_X_GEO_INFO_AVAILABLE,
    HEADER_X_TS_ENV, HEADER_X_TS_VERSION,
};
use trusted_server_core::error::{IntoHttpResponse, TrustedServerError};
use trusted_server_core::geo::GeoInfo;
use trusted_server_core::integrations::IntegrationRegistry;
use trusted_server_core::platform::RuntimeServices;
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

mod app;
mod compat;
mod error;
mod logging;
mod management_api;
mod middleware;
mod platform;

use crate::app::TrustedServerApp;
use crate::error::to_error_response;
use crate::platform::{build_runtime_services, open_kv_store, UnavailableKvStore};
use edgezero_core::app::Hooks as _;

/// Returns `true` if the raw config-store value represents an enabled flag.
///
/// Accepted values (after whitespace trimming): `"true"` and `"1"`.
/// All other values, including the empty string, are treated as disabled.
fn parse_edgezero_flag(value: &str) -> bool {
    let v = value.trim();
    v == "true" || v == "1"
}

/// Reads the `edgezero_enabled` key from the `"trusted_server_config"` Fastly
/// [`ConfigStore`].
///
/// Returns `Err` on any store open or key-read failure, so callers should use
/// `.unwrap_or(false)` to ensure the legacy path is the safe default.
///
/// # Errors
///
/// - [`fastly::Error`] if the config store cannot be opened or the key cannot be read.
fn is_edgezero_enabled() -> Result<bool, fastly::Error> {
    let store = fastly::ConfigStore::try_open("trusted_server_config")
        .map_err(|e| fastly::Error::msg(format!("failed to open config store: {e}")))?;
    let value = store
        .try_get("edgezero_enabled")
        .map_err(|e| fastly::Error::msg(format!("failed to read edgezero_enabled: {e}")))?
        .unwrap_or_default();
    Ok(parse_edgezero_flag(&value))
}

#[fastly::main]
fn main(req: FastlyRequest) -> Result<FastlyResponse, Error> {
    // Health probe bypasses routing, settings, and app construction — cheap liveness signal.
    if req.get_method() == FastlyMethod::GET && req.get_path() == "/health" {
        return Ok(FastlyResponse::from_status(200).with_body_text_plain("ok"));
    }

    logging::init_logger();

    // Safe default: if the flag cannot be read (store unavailable, key missing),
    // fall back to the legacy path to avoid accidentally routing through an
    // untested EdgeZero path.
    if is_edgezero_enabled().unwrap_or_else(|e| {
        log::warn!("failed to read edgezero_enabled flag, falling back to legacy path: {e}");
        false
    }) {
        log::info!("routing request through EdgeZero path");
        let app = TrustedServerApp::build_app();
        // `run_app_with_config` and `run_app_with_logging` call `init_logger`
        // internally — a second `set_logger` call panics because our custom
        // fern logger is already initialised above.  `dispatch_with_config`
        // skips logger initialisation and injects the config store directly.
        edgezero_adapter_fastly::dispatch_with_config(&app, req, "trusted_server_config")
    } else {
        log::info!("routing request through legacy path");
        legacy_main(req)
    }
}

/// Handles a request using the original Fastly-native entry point.
///
/// Preserves identical semantics to the pre-PR14 `main()`. Called when
/// the `edgezero_enabled` config flag is absent or `false`.
///
/// The thin fastly↔http conversion layer (via `compat::from_fastly_request` /
/// `compat::to_fastly_response`) lives here in the adapter crate. `compat.rs`
/// will be deleted in PR 15 once this legacy path is retired.
///
/// # Errors
///
/// Propagates [`fastly::Error`] from the Fastly SDK.
// TODO: delete after Phase 5 EdgeZero cutover — see issue #495
fn legacy_main(mut req: FastlyRequest) -> Result<FastlyResponse, Error> {
    let settings = match get_settings() {
        Ok(s) => s,
        Err(e) => {
            log::error!("Failed to load settings: {:?}", e);
            return Ok(to_error_response(&e));
        }
    };
    log::debug!("Settings {settings:?}");

    // Build the auction orchestrator once at startup
    let orchestrator = match build_orchestrator(&settings) {
        Ok(orchestrator) => orchestrator,
        Err(e) => {
            log::error!("Failed to build auction orchestrator: {:?}", e);
            return Ok(to_error_response(&e));
        }
    };

    let integration_registry = match IntegrationRegistry::new(&settings) {
        Ok(r) => r,
        Err(e) => {
            log::error!("Failed to create integration registry: {:?}", e);
            return Ok(to_error_response(&e));
        }
    };

    let kv_store = match open_kv_store(&settings.synthetic.opid_store) {
        Ok(s) => s,
        Err(e) => {
            // Degrade gracefully: routes that do not touch synthetic IDs
            // (e.g. /.well-known/, /verify-signature, /admin/keys/*) must
            // still succeed even when the KV store is unavailable.
            // Handlers that call kv_handle() will receive KvError::Unavailable.
            log::warn!(
                "KV store '{}' unavailable, synthetic ID routes will return errors: {e}",
                settings.synthetic.opid_store
            );
            std::sync::Arc::new(UnavailableKvStore)
                as std::sync::Arc<dyn trusted_server_core::platform::PlatformKvStore>
        }
    };
    // Strip client-spoofable forwarded headers at the edge before building
    // any request-derived context or converting to the core HTTP types.
    compat::sanitize_fastly_forwarded_headers(&mut req);

    let runtime_services = build_runtime_services(&req, kv_store);
    let geo_info = runtime_services
        .geo()
        .lookup(runtime_services.client_info().client_ip)
        .unwrap_or_else(|e| {
            log::warn!("geo lookup failed: {e}");
            None
        });
    let http_req = compat::from_fastly_request(req);

    let mut response = futures::executor::block_on(route_request(
        &settings,
        &orchestrator,
        &integration_registry,
        &runtime_services,
        http_req,
    ))
    .unwrap_or_else(|e| http_error_response(&e));

    finalize_response(&settings, geo_info.as_ref(), &mut response);

    Ok(compat::to_fastly_response(response))
}

async fn route_request(
    settings: &Settings,
    orchestrator: &AuctionOrchestrator,
    integration_registry: &IntegrationRegistry,
    runtime_services: &RuntimeServices,
    req: HttpRequest,
) -> Result<HttpResponse, Report<TrustedServerError>> {
    // `get_settings()` should already have rejected invalid handler regexes.
    // Keep this fallback so manually-constructed or otherwise unprepared
    // settings still become an error response instead of panicking.
    match enforce_basic_auth(settings, &req) {
        Ok(Some(response)) => return Ok(response),
        Ok(None) => {}
        Err(e) => return Err(e),
    }

    // Get path and method for routing
    let path = req.uri().path().to_string();
    let method = req.method().clone();

    // Match known routes and handle them
    match (method, path.as_str()) {
        // Serve the tsjs library
        (Method::GET, path) if path.starts_with("/static/tsjs=") => {
            handle_tsjs_dynamic(&req, integration_registry)
        }

        // Discovery endpoint for trusted-server capabilities and JWKS
        (Method::GET, "/.well-known/trusted-server.json") => {
            handle_trusted_server_discovery(settings, runtime_services, req)
        }

        // Signature verification endpoint
        (Method::POST, "/verify-signature") => {
            handle_verify_signature(settings, runtime_services, req)
        }

        // Key rotation admin endpoints
        // Keep in sync with Settings::ADMIN_ENDPOINTS in crates/trusted-server-core/src/settings.rs
        (Method::POST, "/admin/keys/rotate") => handle_rotate_key(settings, runtime_services, req),
        (Method::POST, "/admin/keys/deactivate") => {
            handle_deactivate_key(settings, runtime_services, req)
        }

        // Unified auction endpoint (returns creative HTML inline)
        (Method::POST, "/auction") => {
            handle_auction(settings, orchestrator, runtime_services, req).await
        }

        // tsjs endpoints
        (Method::GET, "/first-party/proxy") => {
            handle_first_party_proxy(settings, runtime_services, req).await
        }
        (Method::GET, "/first-party/click") => {
            handle_first_party_click(settings, runtime_services, req).await
        }
        (Method::GET, "/first-party/sign") | (Method::POST, "/first-party/sign") => {
            handle_first_party_proxy_sign(settings, runtime_services, req).await
        }
        (Method::POST, "/first-party/proxy-rebuild") => {
            handle_first_party_proxy_rebuild(settings, runtime_services, req).await
        }
        (m, path) if integration_registry.has_route(&m, path) => integration_registry
            .handle_proxy(&m, path, settings, runtime_services, req)
            .await
            .unwrap_or_else(|| {
                Err(Report::new(TrustedServerError::BadRequest {
                    message: format!("Unknown integration route: {path}"),
                }))
            }),

        // No known route matched, proxy to publisher origin as fallback
        _ => {
            log::info!(
                "No known route matched for path: {}, proxying to publisher origin",
                path
            );

            handle_publisher_request(settings, integration_registry, runtime_services, req).await
        }
    }
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
        let header_name = HeaderName::from_bytes(key.as_bytes());
        let header_value = HeaderValue::from_str(value);
        if let (Ok(header_name), Ok(header_value)) = (header_name, header_value) {
            response.headers_mut().insert(header_name, header_value);
        } else {
            log::warn!(
                "Skipping invalid configured response header value for {}",
                key
            );
        }
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

#[cfg(test)]
mod tests {
    use super::parse_edgezero_flag;

    #[test]
    fn parses_true_flag_values() {
        assert!(parse_edgezero_flag("true"), "should parse 'true'");
        assert!(parse_edgezero_flag("1"), "should parse '1'");
        assert!(parse_edgezero_flag("  true  "), "should trim whitespace");
        assert!(
            parse_edgezero_flag("  1  "),
            "should trim whitespace around '1'"
        );
    }

    #[test]
    fn rejects_non_true_flag_values() {
        assert!(!parse_edgezero_flag("false"), "should not parse 'false'");
        assert!(!parse_edgezero_flag(""), "should not parse empty string");
        assert!(
            !parse_edgezero_flag("  "),
            "should not parse whitespace-only"
        );
        assert!(!parse_edgezero_flag("yes"), "should not parse 'yes'");
        assert!(
            !parse_edgezero_flag("TRUE"),
            "should not parse uppercase 'TRUE'"
        );
    }
}
