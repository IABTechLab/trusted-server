use edgezero_core::body::Body as EdgeBody;
use edgezero_core::http::{
    header, HeaderValue, Method, Request as HttpRequest, Response as HttpResponse,
};
use error_stack::Report;
use fastly::http::Method as FastlyMethod;
use fastly::{Error, Request as FastlyRequest, Response as FastlyResponse};

use trusted_server_core::auction::endpoints::handle_auction;
use trusted_server_core::auction::AuctionOrchestrator;
use trusted_server_core::auth::enforce_basic_auth;
use trusted_server_core::compat;
use trusted_server_core::error::{IntoHttpResponse, TrustedServerError};
use trusted_server_core::geo::GeoInfo;
use trusted_server_core::integrations::IntegrationRegistry;
use trusted_server_core::platform::PlatformGeo as _;
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

mod app;
mod error;
mod logging;
mod management_api;
mod middleware;
mod platform;
#[cfg(test)]
mod route_tests;

use crate::app::{build_state, TrustedServerApp};
use crate::error::to_error_response;
use crate::middleware::apply_finalize_headers;
use crate::platform::{build_runtime_services, open_kv_store, FastlyPlatformGeo};
use edgezero_core::app::Hooks as _;

/// Returns `true` if the raw config-store value represents an enabled flag.
///
/// Accepted values (after whitespace trimming): `"true"` and `"1"`.
/// All other values, including the empty string, are treated as disabled.
fn parse_edgezero_flag(value: &str) -> bool {
    let v = value.trim();
    v.eq_ignore_ascii_case("true") || v == "1"
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
fn main(mut req: FastlyRequest) -> Result<FastlyResponse, Error> {
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
        log::debug!("routing request through EdgeZero path");
        let app = TrustedServerApp::build_app();
        // Strip client-spoofable forwarded headers before handing off to the
        // EdgeZero dispatcher, mirroring the sanitization done in legacy_main.
        compat::sanitize_fastly_forwarded_headers(&mut req);
        // Capture client IP before the request is consumed by dispatch.
        let client_ip = req.get_client_ip_addr();
        // `run_app_with_config` and `run_app_with_logging` call `init_logger`
        // internally — a second `set_logger` call panics because our custom
        // fern logger is already initialised above.  `dispatch_with_config`
        // skips logger initialisation and injects the config store directly.
        let mut response = compat::from_fastly_response(
            edgezero_adapter_fastly::dispatch_with_config(&app, req, "trusted_server_config")?,
        );
        // Apply finalize headers at the entry point so that router-level 405/404
        // responses for unregistered HTTP methods (e.g. TRACE, WebDAV verbs)
        // carry TS/geo headers, matching the legacy path's all-methods guarantee.
        // For requests that ran through FinalizeResponseMiddleware, this is
        // idempotent — header::insert overwrites with the same values.
        if let Ok(settings) = get_settings() {
            let geo_info = FastlyPlatformGeo.lookup(client_ip).unwrap_or_else(|e| {
                log::warn!("entry-point geo lookup failed: {e}");
                None
            });
            apply_finalize_headers(&settings, geo_info.as_ref(), &mut response);
        }
        Ok(compat::to_fastly_response(response))
    } else {
        log::debug!("routing request through legacy path");
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
    let state = match build_state() {
        Ok(state) => state,
        Err(e) => {
            log::error!("Failed to build application state: {:?}", e);
            return Ok(to_error_response(&e));
        }
    };
    log::debug!("Settings {:?}", state.settings);
    // Strip client-spoofable forwarded headers at the edge before building
    // any request-derived context or converting to the core HTTP types.
    compat::sanitize_fastly_forwarded_headers(&mut req);

    let runtime_services = build_runtime_services(&req, std::sync::Arc::clone(&state.kv_store));
    let http_req = compat::from_fastly_request(req);

    let mut response = futures::executor::block_on(route_request(
        &state.settings,
        &state.orchestrator,
        &state.registry,
        &runtime_services,
        http_req,
    ))
    .unwrap_or_else(|e| http_error_response(&e));

    let geo_info = if response.status() == edgezero_core::http::StatusCode::UNAUTHORIZED {
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

    finalize_response(&state.settings, geo_info.as_ref(), &mut response);

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
            match runtime_services_for_consent_route(settings, runtime_services) {
                Ok(auction_services) => {
                    handle_auction(settings, orchestrator, &auction_services, req).await
                }
                Err(e) => Err(e),
            }
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

            match runtime_services_for_consent_route(settings, runtime_services) {
                Ok(publisher_services) => handle_publisher_request(
                    settings,
                    integration_registry,
                    &publisher_services,
                    req,
                )
                .await
                .and_then(|pub_response| {
                    resolve_publisher_response(pub_response, settings, integration_registry)
                }),
                Err(e) => Err(e),
            }
        }
    }
}

pub(crate) fn resolve_publisher_response(
    publisher_response: PublisherResponse,
    settings: &Settings,
    integration_registry: &IntegrationRegistry,
) -> Result<HttpResponse, Report<TrustedServerError>> {
    match publisher_response {
        PublisherResponse::Buffered(response) => Ok(response),
        PublisherResponse::Stream {
            mut response,
            body,
            params,
        } => {
            let mut output = Vec::new();
            stream_publisher_body(body, &mut output, &params, settings, integration_registry)?;
            response.headers_mut().insert(
                header::CONTENT_LENGTH,
                HeaderValue::from(output.len() as u64),
            );
            *response.body_mut() = EdgeBody::from(output);
            Ok(response)
        }
        PublisherResponse::PassThrough { mut response, body } => {
            *response.body_mut() = body;
            Ok(response)
        }
    }
}

fn runtime_services_for_consent_route(
    settings: &Settings,
    runtime_services: &RuntimeServices,
) -> Result<RuntimeServices, Report<TrustedServerError>> {
    let Some(store_name) = settings.consent.consent_store.as_deref() else {
        return Ok(runtime_services.clone());
    };

    open_kv_store(store_name)
        .map(|store| runtime_services.clone().with_kv_store(store))
        .map_err(|e| {
            Report::new(TrustedServerError::KvStore {
                store_name: store_name.to_string(),
                message: e.to_string(),
            })
        })
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
    apply_finalize_headers(settings, geo_info, response);
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
        assert!(parse_edgezero_flag("TRUE"), "should parse uppercase 'TRUE'");
        assert!(
            parse_edgezero_flag("True"),
            "should parse mixed-case 'True'"
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
    }
}
