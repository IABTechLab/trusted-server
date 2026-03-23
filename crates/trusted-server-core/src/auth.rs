use base64::{engine::general_purpose::STANDARD, Engine as _};
use error_stack::Report;
use fastly::http::{header, StatusCode};
use fastly::{Request, Response};
use sha2::{Digest as _, Sha256};
use subtle::ConstantTimeEq as _;

use crate::error::TrustedServerError;
use crate::settings::Settings;

const BASIC_AUTH_REALM: &str = r#"Basic realm="Trusted Server""#;

/// Enforce HTTP basic auth for the matched handler, if any.
///
/// Returns `Ok(None)` when the request does not target a protected handler or
/// when the supplied credentials are valid. Returns `Ok(Some(Response))` with
/// the auth challenge when credentials are missing or invalid.
///
/// # Errors
///
/// Returns an error when handler configuration is invalid, such as an
/// un-compilable path regex.
pub fn enforce_basic_auth(
    settings: &Settings,
    req: &Request,
) -> Result<Option<Response>, Report<TrustedServerError>> {
    let Some(handler) = settings.handler_for_path(req.get_path())? else {
        return Ok(None);
    };

    let (username, password) = match extract_credentials(req) {
        Some(credentials) => credentials,
        None => return Ok(Some(unauthorized_response())),
    };

    // Hash before comparing to normalise lengths — `ct_eq` on raw byte slices
    // short-circuits when lengths differ, which would leak credential length.
    // SHA-256 produces fixed-size digests so the comparison is truly constant-time.
    //
    // Note: constant-time guarantees are best-effort on WASM targets because the
    // runtime optimiser/JIT may re-introduce variable-time paths. This is an
    // inherent limitation of all constant-time code in managed runtimes.
    let username_match = Sha256::digest(handler.username.expose().as_bytes())
        .ct_eq(&Sha256::digest(username.as_bytes()));
    let password_match = Sha256::digest(handler.password.expose().as_bytes())
        .ct_eq(&Sha256::digest(password.as_bytes()));

    if bool::from(username_match & password_match) {
        Ok(None)
    } else {
        Ok(Some(unauthorized_response()))
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

    use crate::test_support::tests::{crate_test_settings_str, create_test_settings};

    #[test]
    fn no_challenge_for_non_protected_path() {
        let settings = create_test_settings();
        let req = Request::new(Method::GET, "https://example.com/open");

        assert!(enforce_basic_auth(&settings, &req)
            .expect("should evaluate auth")
            .is_none());
    }

    #[test]
    fn challenge_when_missing_credentials() {
        let settings = create_test_settings();
        let req = Request::new(Method::GET, "https://example.com/secure");

        let response = enforce_basic_auth(&settings, &req)
            .expect("should evaluate auth")
            .expect("should challenge");
        assert_eq!(response.get_status(), StatusCode::UNAUTHORIZED);
        let realm = response
            .get_header(header::WWW_AUTHENTICATE)
            .expect("should have WWW-Authenticate header");
        assert_eq!(realm, BASIC_AUTH_REALM);
    }

    #[test]
    fn allow_when_credentials_match() {
        let settings = create_test_settings();
        let mut req = Request::new(Method::GET, "https://example.com/secure/data");
        let token = STANDARD.encode("user:pass");
        req.set_header(header::AUTHORIZATION, format!("Basic {token}"));

        assert!(enforce_basic_auth(&settings, &req)
            .expect("should evaluate auth")
            .is_none());
    }

    #[test]
    fn challenge_when_both_credentials_wrong() {
        let settings = create_test_settings();
        let mut req = Request::new(Method::GET, "https://example.com/secure/data");
        let token = STANDARD.encode("wrong:wrong");
        req.set_header(header::AUTHORIZATION, format!("Basic {token}"));

        let response = enforce_basic_auth(&settings, &req)
            .expect("should evaluate auth")
            .expect("should challenge");
        assert_eq!(response.get_status(), StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn challenge_when_scheme_is_not_basic() {
        let settings = create_test_settings();
        let mut req = Request::new(Method::GET, "https://example.com/secure");
        req.set_header(header::AUTHORIZATION, "Bearer token");

        let response = enforce_basic_auth(&settings, &req)
            .expect("should evaluate auth")
            .expect("should challenge");
        assert_eq!(response.get_status(), StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn returns_error_for_invalid_handler_regex_without_panicking() {
        let config = crate_test_settings_str().replace(r#"path = "^/secure""#, r#"path = "(""#);
        let err = Settings::from_toml(&config).expect_err("should reject invalid handler regex");
        assert!(
            err.to_string()
                .contains("Handler path regex `(` failed to compile"),
            "should describe the invalid handler regex"
        );
    }

    #[test]
    fn allow_admin_path_with_valid_credentials() {
        let settings = create_test_settings();
        let mut req = Request::new(Method::POST, "https://example.com/admin/keys/rotate");
        let token = STANDARD.encode("admin:admin-pass");
        req.set_header(header::AUTHORIZATION, format!("Basic {token}"));

        assert!(
            enforce_basic_auth(&settings, &req)
                .expect("should evaluate auth")
                .is_none(),
            "should allow admin path with correct credentials"
        );
    }

    #[test]
    fn challenge_admin_path_with_wrong_credentials() {
        let settings = create_test_settings();
        let mut req = Request::new(Method::POST, "https://example.com/admin/keys/rotate");
        let token = STANDARD.encode("admin:wrong");
        req.set_header(header::AUTHORIZATION, format!("Basic {token}"));

        let response = enforce_basic_auth(&settings, &req)
            .expect("should evaluate auth")
            .expect("should challenge admin path with wrong credentials");
        assert_eq!(response.get_status(), StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn challenge_admin_path_with_missing_credentials() {
        let settings = create_test_settings();
        let req = Request::new(Method::POST, "https://example.com/admin/keys/rotate");

        let response = enforce_basic_auth(&settings, &req)
            .expect("should evaluate auth")
            .expect("should challenge admin path with missing credentials");
        assert_eq!(response.get_status(), StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn challenge_when_username_wrong_password_correct() {
        let settings = create_test_settings();
        let mut req = Request::new(Method::GET, "https://example.com/secure/data");
        let token = STANDARD.encode("wrong:pass");
        req.set_header(header::AUTHORIZATION, format!("Basic {token}"));

        let response = enforce_basic_auth(&settings, &req)
            .expect("should evaluate auth")
            .expect("should challenge when only username is wrong");
        assert_eq!(response.get_status(), StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn challenge_when_username_correct_password_wrong() {
        let settings = create_test_settings();
        let mut req = Request::new(Method::GET, "https://example.com/secure/data");
        let token = STANDARD.encode("user:wrong");
        req.set_header(header::AUTHORIZATION, format!("Basic {token}"));

        let response = enforce_basic_auth(&settings, &req)
            .expect("should evaluate auth")
            .expect("should challenge when only password is wrong");
        assert_eq!(response.get_status(), StatusCode::UNAUTHORIZED);
    }
}
