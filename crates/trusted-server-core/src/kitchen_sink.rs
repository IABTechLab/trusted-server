//! Embedded kitchen-sink fixture serving and HTML processing.

use edgezero_core::body::Body as EdgeBody;
use error_stack::Report;
use http::{header, HeaderValue, Method, Request, Response, StatusCode};
use sha2::{Digest as _, Sha256};

use crate::error::TrustedServerError;
use crate::html_processor::{create_html_processor, HtmlProcessorConfig};
use crate::http_util::RequestInfo;
use crate::integrations::IntegrationRegistry;
use crate::platform::RuntimeServices;
use crate::settings::Settings;
use crate::streaming_processor::StreamProcessor as _;

/// URL path prefix for the embedded kitchen-sink fixture.
pub const KITCHEN_SINK_PREFIX: &str = "/_ts/kitchen-sink";

const KITCHEN_SINK_PREFIX_WITH_SLASH: &str = "/_ts/kitchen-sink/";
const HEADER_X_KITCHEN_SINK: &str = "x-trusted-server-kitchen-sink";
const CACHE_CONTROL_HTML: &str = "no-cache";
const CACHE_CONTROL_ASSET: &str = "public, max-age=300";

/// Returns true when a path belongs to the embedded kitchen-sink route space.
#[must_use]
pub fn is_kitchen_sink_path(path: &str) -> bool {
    path == KITCHEN_SINK_PREFIX || path.starts_with(KITCHEN_SINK_PREFIX_WITH_SLASH)
}

/// Handles an embedded kitchen-sink request.
///
/// HTML assets are processed through the normal Trusted Server HTML processor;
/// all other assets are served directly from the embedded static bundle.
///
/// # Errors
///
/// Returns [`TrustedServerError`] when HTML processing fails or generated header
/// values cannot be represented.
pub fn handle_kitchen_sink_request(
    settings: &Settings,
    integration_registry: &IntegrationRegistry,
    services: &RuntimeServices,
    req: &Request<EdgeBody>,
) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
    let path = req.uri().path();
    if !is_kitchen_sink_path(path) || !settings.debug.kitchen_sink_enabled {
        return Ok(not_found_response(req.method()));
    }

    if !matches!(*req.method(), Method::GET | Method::HEAD) {
        return Ok(method_not_allowed_response());
    }

    if path == KITCHEN_SINK_PREFIX {
        return Ok(redirect_to_slash_response(req.method()));
    }

    let relative_path = &path[KITCHEN_SINK_PREFIX_WITH_SLASH.len()..];
    let asset_path = if relative_path.is_empty() {
        "index.html"
    } else {
        relative_path
    };

    if has_invalid_path_segment(asset_path) {
        return Ok(not_found_response(req.method()));
    }

    let Some(asset) = trusted_server_kitchen_sink::asset_for_path(asset_path) else {
        return Ok(not_found_response(req.method()));
    };

    if is_html_asset(asset.path, asset.content_type) {
        let body = process_html_asset(settings, integration_registry, services, req, asset.body)?;
        let etag = etag_for_bytes(&body);
        return Ok(asset_response(
            StatusCode::OK,
            req.method(),
            asset.content_type,
            CACHE_CONTROL_HTML,
            Some(&etag),
            "processed",
            body,
        ));
    }

    if request_etag_matches(req, asset.etag) {
        return Ok(asset_response(
            StatusCode::NOT_MODIFIED,
            req.method(),
            asset.content_type,
            CACHE_CONTROL_ASSET,
            Some(asset.etag),
            "raw",
            Vec::new(),
        ));
    }

    Ok(asset_response(
        StatusCode::OK,
        req.method(),
        asset.content_type,
        CACHE_CONTROL_ASSET,
        Some(asset.etag),
        "raw",
        asset.body.to_vec(),
    ))
}

fn process_html_asset(
    settings: &Settings,
    integration_registry: &IntegrationRegistry,
    services: &RuntimeServices,
    req: &Request<EdgeBody>,
    body: &[u8],
) -> Result<Vec<u8>, Report<TrustedServerError>> {
    let request_info = RequestInfo::from_request(req, services.client_info());
    let config = HtmlProcessorConfig::from_settings(
        settings,
        integration_registry,
        &settings.publisher.origin_host(),
        &request_info.host,
        &request_info.scheme,
    );
    let mut processor = create_html_processor(config);
    processor.process_chunk(body, true).map_err(|err| {
        Report::new(TrustedServerError::Proxy {
            message: format!("kitchen-sink HTML processing failed: {err}"),
        })
    })
}

fn asset_response(
    status: StatusCode,
    method: &Method,
    content_type: &str,
    cache_control: &'static str,
    etag: Option<&str>,
    mode: &'static str,
    body: Vec<u8>,
) -> Response<EdgeBody> {
    let content_length = body.len();
    let response_body = if *method == Method::HEAD || status == StatusCode::NOT_MODIFIED {
        EdgeBody::empty()
    } else {
        EdgeBody::from(body)
    };
    let mut response = Response::new(response_body);
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(content_type).expect("should use a valid kitchen-sink content type"),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(cache_control),
    );
    response.headers_mut().insert(
        header::CONTENT_LENGTH,
        HeaderValue::from(content_length as u64),
    );
    response
        .headers_mut()
        .insert(HEADER_X_KITCHEN_SINK, HeaderValue::from_static(mode));
    if let Some(etag) = etag {
        response.headers_mut().insert(
            header::ETAG,
            HeaderValue::from_str(etag).expect("should use a valid kitchen-sink ETag"),
        );
    }
    apply_security_headers(&mut response);
    response
}

fn redirect_to_slash_response(method: &Method) -> Response<EdgeBody> {
    let body = Vec::new();
    let content_length = body.len();
    let response_body = if *method == Method::HEAD {
        EdgeBody::empty()
    } else {
        EdgeBody::from(body)
    };
    let mut response = Response::new(response_body);
    *response.status_mut() = StatusCode::PERMANENT_REDIRECT;
    response.headers_mut().insert(
        header::LOCATION,
        HeaderValue::from_static(KITCHEN_SINK_PREFIX_WITH_SLASH),
    );
    response.headers_mut().insert(
        header::CONTENT_LENGTH,
        HeaderValue::from(content_length as u64),
    );
    response
        .headers_mut()
        .insert(HEADER_X_KITCHEN_SINK, HeaderValue::from_static("redirect"));
    apply_security_headers(&mut response);
    response
}

fn not_found_response(method: &Method) -> Response<EdgeBody> {
    let body = "Not Found";
    let response_body = if *method == Method::HEAD {
        EdgeBody::empty()
    } else {
        EdgeBody::from(body)
    };
    let mut response = Response::new(response_body);
    *response.status_mut() = StatusCode::NOT_FOUND;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    response
        .headers_mut()
        .insert(header::CONTENT_LENGTH, HeaderValue::from(body.len() as u64));
    apply_security_headers(&mut response);
    response
}

fn method_not_allowed_response() -> Response<EdgeBody> {
    let mut response = Response::new(EdgeBody::from("Method Not Allowed"));
    *response.status_mut() = StatusCode::METHOD_NOT_ALLOWED;
    response
        .headers_mut()
        .insert(header::ALLOW, HeaderValue::from_static("GET, HEAD"));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    response.headers_mut().insert(
        header::CONTENT_LENGTH,
        HeaderValue::from("Method Not Allowed".len() as u64),
    );
    apply_security_headers(&mut response);
    response
}

fn apply_security_headers(response: &mut Response<EdgeBody>) {
    response.headers_mut().insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    response.headers_mut().insert(
        "referrer-policy",
        HeaderValue::from_static("strict-origin-when-cross-origin"),
    );
    response.headers_mut().insert(
        "permissions-policy",
        HeaderValue::from_static("camera=(), geolocation=(), microphone=()"),
    );
}

fn request_etag_matches(req: &Request<EdgeBody>, etag: &str) -> bool {
    req.headers()
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.split(',').any(|candidate| candidate.trim() == etag))
}

fn etag_for_bytes(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    format!("\"sha256-{}\"", hex::encode(digest))
}

fn is_html_asset(path: &str, content_type: &str) -> bool {
    path.ends_with(".html") || content_type.starts_with("text/html")
}

fn has_invalid_path_segment(path: &str) -> bool {
    path.split('/').any(|segment| matches!(segment, "." | ".."))
}

#[cfg(test)]
mod tests {
    use super::*;

    use edgezero_core::body::Body;
    use http::request::Builder;

    use crate::integrations::IntegrationRegistry;
    use crate::platform::test_support::noop_services;
    use crate::test_support::tests::create_test_settings;

    fn registry(settings: &Settings) -> IntegrationRegistry {
        IntegrationRegistry::new(settings).expect("should build integration registry")
    }

    fn request(method: Method, path: &str) -> Request<EdgeBody> {
        request_builder(method, path)
            .body(Body::empty())
            .expect("should build request")
    }

    fn request_builder(method: Method, path: &str) -> Builder {
        Request::builder()
            .method(method)
            .uri(format!("https://edge.example.com{path}"))
            .header(header::HOST, "edge.example.com")
            .header("fastly-ssl", "1")
    }

    fn enabled_settings() -> Settings {
        let mut settings = create_test_settings();
        settings.debug.kitchen_sink_enabled = true;
        settings
    }

    fn response_body(response: Response<EdgeBody>) -> Vec<u8> {
        response
            .into_body()
            .into_bytes()
            .unwrap_or_default()
            .to_vec()
    }

    #[test]
    fn disabled_path_returns_not_found() {
        let settings = create_test_settings();
        let registry = registry(&settings);
        let services = noop_services();
        let req = request(Method::GET, KITCHEN_SINK_PREFIX_WITH_SLASH);

        let response = handle_kitchen_sink_request(&settings, &registry, &services, &req)
            .expect("should handle disabled kitchen sink");

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn bare_prefix_redirects_to_trailing_slash() {
        let settings = enabled_settings();
        let registry = registry(&settings);
        let services = noop_services();
        let req = request(Method::GET, KITCHEN_SINK_PREFIX);

        let response = handle_kitchen_sink_request(&settings, &registry, &services, &req)
            .expect("should redirect bare kitchen sink path");

        assert_eq!(response.status(), StatusCode::PERMANENT_REDIRECT);
        assert_eq!(
            response.headers().get(header::LOCATION),
            Some(&HeaderValue::from_static(KITCHEN_SINK_PREFIX_WITH_SLASH))
        );
    }

    #[test]
    fn index_html_is_processed() {
        let settings = enabled_settings();
        let registry = registry(&settings);
        let services = noop_services();
        let req = request(Method::GET, KITCHEN_SINK_PREFIX_WITH_SLASH);

        let response = handle_kitchen_sink_request(&settings, &registry, &services, &req)
            .expect("should serve kitchen sink index");
        let status = response.status();
        let headers = response.headers().clone();
        let body = String::from_utf8(response_body(response)).expect("should return UTF-8 HTML");

        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            headers.get(HEADER_X_KITCHEN_SINK),
            Some(&HeaderValue::from_static("processed"))
        );
        assert!(
            body.contains("id=\"trustedserver-js\""),
            "HTML should include Trusted Server script injection"
        );
    }

    #[test]
    fn raw_asset_is_not_html_processed() {
        let settings = enabled_settings();
        let registry = registry(&settings);
        let services = noop_services();
        let req = request(Method::GET, "/_ts/kitchen-sink/assets/app.js");

        let response = handle_kitchen_sink_request(&settings, &registry, &services, &req)
            .expect("should serve kitchen sink JavaScript");
        let headers = response.headers().clone();
        let body = String::from_utf8(response_body(response)).expect("should return JS text");

        assert_eq!(
            headers.get(HEADER_X_KITCHEN_SINK),
            Some(&HeaderValue::from_static("raw"))
        );
        assert!(
            !body.contains("trustedserver-js"),
            "raw JavaScript should not be HTML processed"
        );
    }

    #[test]
    fn missing_asset_returns_not_found() {
        let settings = enabled_settings();
        let registry = registry(&settings);
        let services = noop_services();
        let req = request(Method::GET, "/_ts/kitchen-sink/missing.html");

        let response = handle_kitchen_sink_request(&settings, &registry, &services, &req)
            .expect("should handle missing asset");

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn head_missing_asset_returns_headers_without_body() {
        let settings = enabled_settings();
        let registry = registry(&settings);
        let services = noop_services();
        let req = request(Method::HEAD, "/_ts/kitchen-sink/missing.html");

        let response = handle_kitchen_sink_request(&settings, &registry, &services, &req)
            .expect("should handle missing HEAD asset");
        let status = response.status();
        let body = response_body(response);

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body.is_empty(), "HEAD 404 response should not carry a body");
    }

    #[test]
    fn unsupported_method_returns_method_not_allowed() {
        let settings = enabled_settings();
        let registry = registry(&settings);
        let services = noop_services();
        let req = request(Method::POST, KITCHEN_SINK_PREFIX_WITH_SLASH);

        let response = handle_kitchen_sink_request(&settings, &registry, &services, &req)
            .expect("should reject unsupported method");

        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(
            response.headers().get(header::ALLOW),
            Some(&HeaderValue::from_static("GET, HEAD"))
        );
    }

    #[test]
    fn head_returns_headers_without_body() {
        let settings = enabled_settings();
        let registry = registry(&settings);
        let services = noop_services();
        let req = request(Method::HEAD, KITCHEN_SINK_PREFIX_WITH_SLASH);

        let response = handle_kitchen_sink_request(&settings, &registry, &services, &req)
            .expect("should serve HEAD request");
        let content_length = response
            .headers()
            .get(header::CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<usize>().ok())
            .expect("HEAD response should include representation length");
        let body = response_body(response);

        assert!(
            content_length > 0,
            "HEAD should advertise representation length"
        );
        assert!(body.is_empty(), "HEAD response should not carry a body");
    }

    #[test]
    fn raw_asset_supports_if_none_match() {
        let settings = enabled_settings();
        let registry = registry(&settings);
        let services = noop_services();
        let first_req = request(Method::GET, "/_ts/kitchen-sink/assets/app.js");
        let first = handle_kitchen_sink_request(&settings, &registry, &services, &first_req)
            .expect("should serve raw asset");
        let etag = first
            .headers()
            .get(header::ETAG)
            .cloned()
            .expect("raw asset should include ETag");
        let second_req = request_builder(Method::GET, "/_ts/kitchen-sink/assets/app.js")
            .header(header::IF_NONE_MATCH, etag.clone())
            .body(Body::empty())
            .expect("should build conditional request");

        let response = handle_kitchen_sink_request(&settings, &registry, &services, &second_req)
            .expect("should handle conditional request");

        assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(response.headers().get(header::ETAG), Some(&etag));
        assert!(response_body(response).is_empty());
    }
}
