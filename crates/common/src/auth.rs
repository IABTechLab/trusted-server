use base64::{engine::general_purpose::STANDARD, Engine as _};
use fastly::http::{header, StatusCode};
use fastly::{Request, Response};

use crate::settings::Settings;

const BASIC_AUTH_REALM: &str = r#"Basic realm="Trusted Server""#;

/// Admin path prefix that is always protected regardless of handler configuration.
///
/// Requests to paths starting with this prefix are denied with `401 Unauthorized`
/// unless a configured handler explicitly covers them with valid credentials.
const ADMIN_PATH_PREFIX: &str = "/admin/";

/// Enforces Basic-auth for incoming requests.
///
/// For most paths, authentication is only required when a configured handler's
/// `path` regex matches the request path. **Admin paths** (`/admin/…`) are an
/// exception: they are *always* gated behind authentication. If no handler
/// covers an admin path the request is rejected outright.
///
/// # Returns
///
/// * `Some(Response)` — a `401 Unauthorized` response that should be sent back
///   to the client (auth failed or no handler covers an admin path).
/// * `None` — the request is allowed to proceed.
pub fn enforce_basic_auth(settings: &Settings, req: &Request) -> Option<Response> {
    let path = req.get_path();
    let is_admin = path.starts_with(ADMIN_PATH_PREFIX);

    let handler = match settings.handler_for_path(path) {
        Some(h) => h,
        // No handler covers this path. Admin paths are always denied;
        // all other paths pass through unauthenticated.
        None if is_admin => {
            log::warn!("Admin path {path} requested but no handler covers it — denying access");
            return Some(unauthorized_response());
        }
        None => return None,
    };

    let (username, password) = match extract_credentials(req) {
        Some(credentials) => credentials,
        None => return Some(unauthorized_response()),
    };

    if username == handler.username && password == handler.password {
        None
    } else {
        Some(unauthorized_response())
    }
}

fn extract_credentials(req: &Request) -> Option<(String, String)> {
    let header_value = req
        .get_header(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())?;

    let mut parts = header_value.splitn(2, ' ');
    let scheme = parts.next()?.trim();
    if !scheme.eq_ignore_ascii_case("basic") {
        return None;
    }

    let token = parts.next()?.trim();
    if token.is_empty() {
        return None;
    }

    let decoded = STANDARD.decode(token).ok()?;
    let credentials = String::from_utf8(decoded).ok()?;

    let mut credentials_parts = credentials.splitn(2, ':');
    let username = credentials_parts.next()?.to_string();
    let password = credentials_parts.next()?.to_string();

    Some((username, password))
}

fn unauthorized_response() -> Response {
    Response::from_status(StatusCode::UNAUTHORIZED)
        .with_header(header::WWW_AUTHENTICATE, BASIC_AUTH_REALM)
        .with_header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .with_body_text_plain("Unauthorized")
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::STANDARD;
    use fastly::http::{header, Method};

    use crate::test_support::tests::crate_test_settings_str;

    fn settings_with_handlers() -> Settings {
        let config = crate_test_settings_str();
        Settings::from_toml(&config).expect("should parse settings with handlers")
    }

    #[test]
    fn no_challenge_for_non_protected_path() {
        let settings = settings_with_handlers();
        let req = Request::new(Method::GET, "https://example.com/open");

        assert!(enforce_basic_auth(&settings, &req).is_none());
    }

    #[test]
    fn challenge_when_missing_credentials() {
        let settings = settings_with_handlers();
        let req = Request::new(Method::GET, "https://example.com/secure");

        let response = enforce_basic_auth(&settings, &req).expect("should challenge");
        assert_eq!(response.get_status(), StatusCode::UNAUTHORIZED);
        let realm = response
            .get_header(header::WWW_AUTHENTICATE)
            .expect("should have WWW-Authenticate header");
        assert_eq!(realm, BASIC_AUTH_REALM);
    }

    #[test]
    fn allow_when_credentials_match() {
        let settings = settings_with_handlers();
        let mut req = Request::new(Method::GET, "https://example.com/secure/data");
        let token = STANDARD.encode("user:pass");
        req.set_header(header::AUTHORIZATION, format!("Basic {token}"));

        assert!(enforce_basic_auth(&settings, &req).is_none());
    }

    #[test]
    fn challenge_when_credentials_mismatch() {
        let settings = settings_with_handlers();
        let mut req = Request::new(Method::GET, "https://example.com/secure/data");
        let token = STANDARD.encode("user:wrong");
        req.set_header(header::AUTHORIZATION, format!("Basic {token}"));

        let response = enforce_basic_auth(&settings, &req).expect("should challenge");
        assert_eq!(response.get_status(), StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn challenge_when_scheme_is_not_basic() {
        let settings = settings_with_handlers();
        let mut req = Request::new(Method::GET, "https://example.com/secure");
        req.set_header(header::AUTHORIZATION, "Bearer token");

        let response = enforce_basic_auth(&settings, &req).expect("should challenge");
        assert_eq!(response.get_status(), StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn deny_admin_path_when_no_handler_covers_it() {
        let settings = settings_with_handlers();
        let req = Request::new(Method::POST, "https://example.com/admin/keys/rotate");

        let response = enforce_basic_auth(&settings, &req)
            .expect("should deny admin path without matching handler");
        assert_eq!(
            response.get_status(),
            StatusCode::UNAUTHORIZED,
            "should return 401 for uncovered admin path"
        );
    }

    #[test]
    fn deny_admin_deactivate_when_no_handler_covers_it() {
        let settings = settings_with_handlers();
        let req = Request::new(Method::POST, "https://example.com/admin/keys/deactivate");

        let response = enforce_basic_auth(&settings, &req)
            .expect("should deny admin deactivate without matching handler");
        assert_eq!(response.get_status(), StatusCode::UNAUTHORIZED);
    }

    fn settings_with_admin_handler() -> Settings {
        let config = crate_test_settings_str()
            + r#"
            [[handlers]]
            path = "^/admin"
            username = "admin"
            password = "secret"
            "#;
        Settings::from_toml(&config).expect("should parse settings with admin handler")
    }

    #[test]
    fn allow_admin_path_with_valid_credentials() {
        let settings = settings_with_admin_handler();
        let mut req = Request::new(Method::POST, "https://example.com/admin/keys/rotate");
        let token = STANDARD.encode("admin:secret");
        req.set_header(header::AUTHORIZATION, format!("Basic {token}"));

        assert!(
            enforce_basic_auth(&settings, &req).is_none(),
            "should allow admin path with correct credentials"
        );
    }

    #[test]
    fn challenge_admin_path_with_wrong_credentials() {
        let settings = settings_with_admin_handler();
        let mut req = Request::new(Method::POST, "https://example.com/admin/keys/rotate");
        let token = STANDARD.encode("admin:wrong");
        req.set_header(header::AUTHORIZATION, format!("Basic {token}"));

        let response = enforce_basic_auth(&settings, &req)
            .expect("should challenge admin path with wrong credentials");
        assert_eq!(response.get_status(), StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn challenge_admin_path_with_missing_credentials() {
        let settings = settings_with_admin_handler();
        let req = Request::new(Method::POST, "https://example.com/admin/keys/rotate");

        let response = enforce_basic_auth(&settings, &req)
            .expect("should challenge admin path with missing credentials");
        assert_eq!(response.get_status(), StatusCode::UNAUTHORIZED);
    }
}
