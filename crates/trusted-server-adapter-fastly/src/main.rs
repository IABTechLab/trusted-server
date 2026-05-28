use std::net::IpAddr;
use std::sync::Arc;

use edgezero_adapter_fastly::FastlyConfigStore;
use edgezero_core::app::Hooks as _;
use edgezero_core::body::Body as EdgeBody;
use edgezero_core::config_store::ConfigStoreHandle;
use edgezero_core::http::{header, HeaderValue, Response as HttpResponse};
use error_stack::Report;
use fastly::http::Method as FastlyMethod;
use fastly::{Request as FastlyRequest, Response as FastlyResponse};

use trusted_server_core::error::TrustedServerError;
use trusted_server_core::geo::GeoInfo;
use trusted_server_core::integrations::IntegrationRegistry;
use trusted_server_core::platform::PlatformGeo as _;
use trusted_server_core::publisher::{stream_publisher_body, PublisherResponse};
use trusted_server_core::settings::Settings;
use trusted_server_core::settings_data::get_settings;

mod app;
mod backend;
mod compat;
mod logging;
mod management_api;
mod middleware;
mod platform;

use crate::app::TrustedServerApp;
use crate::middleware::{apply_finalize_headers, HEADER_X_TS_FINALIZED};
use crate::platform::FastlyPlatformGeo;

const TRUSTED_SERVER_CONFIG_STORE: &str = "trusted_server_config";

/// Opens the Fastly Config Store used by the `EdgeZero` dispatcher.
///
/// # Errors
///
/// Returns [`fastly::Error`] if the config store cannot be opened.
fn open_trusted_server_config_store() -> Result<ConfigStoreHandle, fastly::Error> {
    let store = FastlyConfigStore::try_open(TRUSTED_SERVER_CONFIG_STORE)
        .map_err(|e| fastly::Error::msg(format!("failed to open config store: {e}")))?;
    Ok(ConfigStoreHandle::new(Arc::new(store)))
}

fn health_response(req: &FastlyRequest) -> Option<FastlyResponse> {
    if req.get_method() == FastlyMethod::GET && req.get_path() == "/health" {
        return Some(FastlyResponse::from_status(200).with_body_text_plain("ok"));
    }

    None
}

/// Entry point for the Fastly Compute program.
///
/// Uses an undecorated `main()` with `FastlyRequest::from_client()` instead of
/// `#[fastly::main]` so the `EdgeZero` streaming publisher path can call
/// [`fastly::Response::stream_to_client`] explicitly.
fn main() {
    let req = FastlyRequest::from_client();

    // Health probe bypasses logging, settings, and app construction as a cheap liveness signal.
    if let Some(response) = health_response(&req) {
        response.send_to_client();
        return;
    }

    logging::init_logger();
    edgezero_main(req);
}

/// Handles a request through the `EdgeZero` router path.
fn edgezero_main(mut req: FastlyRequest) {
    let config_store = match open_trusted_server_config_store() {
        Ok(cs) => cs,
        Err(e) => {
            log::error!("failed to open config store: {e}");
            FastlyResponse::from_status(fastly::http::StatusCode::INTERNAL_SERVER_ERROR)
                .with_body_text_plain("Internal Server Error")
                .send_to_client();
            return;
        }
    };

    let app = TrustedServerApp::build_app();

    // Strip client-spoofable forwarded headers before dispatch.
    compat::sanitize_fastly_forwarded_headers(&mut req);

    // Capture client IP before the request is consumed by dispatch.
    let client_ip = req.get_client_ip_addr();

    // `dispatch_with_config_handle` skips logger initialisation and injects
    // the config store directly (init_logger already called in main()).
    let mut response =
        match edgezero_adapter_fastly::dispatch_with_config_handle(&app, req, config_store) {
            Ok(response) => compat::from_fastly_response(response),
            Err(e) => {
                log::error!("EdgeZero dispatch failed: {e}");
                FastlyResponse::from_status(fastly::http::StatusCode::INTERNAL_SERVER_ERROR)
                    .with_body_text_plain("Internal Server Error")
                    .send_to_client();
                return;
            }
        };

    if !response_was_finalized_by_middleware(&mut response) {
        match get_settings() {
            Ok(settings) => {
                apply_entry_point_finalize(&settings, client_ip, &mut response, |client_ip| {
                    FastlyPlatformGeo.lookup(client_ip).unwrap_or_else(|e| {
                        log::warn!("entry-point geo lookup failed: {e}");
                        None
                    })
                })
            }
            Err(e) => {
                log::warn!("entry-point finalize skipped: failed to reload settings: {e:?}");
            }
        }
    }

    compat::to_fastly_response(response).send_to_client();
}

fn response_was_finalized_by_middleware(response: &mut HttpResponse) -> bool {
    response
        .headers_mut()
        .remove(HEADER_X_TS_FINALIZED)
        .is_some()
}

fn apply_entry_point_finalize<F>(
    settings: &Settings,
    client_ip: Option<IpAddr>,
    response: &mut HttpResponse,
    lookup_geo: F,
) where
    F: FnOnce(Option<IpAddr>) -> Option<GeoInfo>,
{
    let geo_info = if response.status() == edgezero_core::http::StatusCode::UNAUTHORIZED {
        None
    } else {
        lookup_geo(client_ip)
    };
    apply_finalize_headers(settings, geo_info.as_ref(), response);
}

pub(crate) fn resolve_publisher_response_buffered(
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

#[cfg(test)]
mod tests {
    use super::*;
    use edgezero_core::http::response_builder;

    #[test]
    fn health_response_short_circuits_get_health() {
        let req = FastlyRequest::get("https://example.com/health");

        let mut response = health_response(&req).expect("should build health response");

        assert_eq!(
            response.get_status(),
            fastly::http::StatusCode::OK,
            "should return 200 OK"
        );
        assert_eq!(
            response.take_body_str(),
            "ok",
            "should return the health body"
        );
    }

    #[test]
    fn health_response_ignores_non_health_paths() {
        let req = FastlyRequest::get("https://example.com/auction");

        assert!(
            health_response(&req).is_none(),
            "should only short-circuit /health"
        );
    }

    #[test]
    fn response_was_finalized_by_middleware_strips_sentinel() {
        let mut response = HttpResponse::new(EdgeBody::empty());
        response
            .headers_mut()
            .insert("x-ts-finalized", HeaderValue::from_static("1"));

        assert!(
            response_was_finalized_by_middleware(&mut response),
            "should detect middleware-finalized responses"
        );
        assert!(
            response.headers().get("x-ts-finalized").is_none(),
            "sentinel should not be sent to clients"
        );
    }

    #[test]
    #[allow(clippy::panic)]
    fn entry_point_finalize_skips_geo_lookup_for_401() {
        let settings = get_settings().expect("should load settings");
        let mut response = response_builder()
            .status(edgezero_core::http::StatusCode::UNAUTHORIZED)
            .body(EdgeBody::empty())
            .expect("should build response");

        apply_entry_point_finalize(&settings, None, &mut response, |_| {
            panic!("should skip entry-point geo lookup for 401 responses");
        });

        assert_eq!(
            response
                .headers()
                .get(trusted_server_core::constants::HEADER_X_GEO_INFO_AVAILABLE)
                .and_then(|v| v.to_str().ok()),
            Some("false"),
            "401 responses should still carry geo-unavailable headers"
        );
    }
}
