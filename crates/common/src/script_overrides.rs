use fastly::http::StatusCode;
use fastly::Response;

use crate::settings::Settings;

/// Handles requests for overridden scripts by returning an empty JavaScript response.
///
/// This is useful for blocking or stubbing specific script files without breaking
/// the page. The response includes appropriate headers for caching and content type.
///
/// # Returns
///
/// Returns an HTTP 200 response with:
/// - Empty body with a comment explaining the override
/// - `Content-Type: application/javascript; charset=utf-8`
/// - Long cache headers for optimal CDN performance
#[allow(unused)]
pub fn handle_script_override(_settings: &Settings) -> Response {
    let body = "// Script overridden by Trusted Server\n";

    Response::from_status(StatusCode::OK)
        .with_header("Content-Type", "application/javascript; charset=utf-8")
        .with_header("Cache-Control", "public, max-age=31536000, immutable")
        .with_body(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::tests::create_test_settings;

    #[test]
    fn test_handle_script_override_returns_empty_js() {
        let settings = create_test_settings();
        let response = handle_script_override(&settings);

        assert_eq!(response.get_status(), StatusCode::OK);

        let content_type = response
            .get_header_str("Content-Type")
            .expect("Content-Type header should be present");
        assert_eq!(content_type, "application/javascript; charset=utf-8");

        let cache_control = response
            .get_header_str("Cache-Control")
            .expect("Cache-Control header should be present");
        assert!(cache_control.contains("max-age=31536000"));
        assert!(cache_control.contains("immutable"));

        let body = response.into_body_str();
        assert!(body.contains("// Script overridden by Trusted Server"));
    }
}
