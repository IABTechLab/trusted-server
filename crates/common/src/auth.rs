use base64::{engine::general_purpose::STANDARD, Engine as _};
use fastly::http::{header, StatusCode};
use fastly::{Request, Response};

use crate::settings::Settings;

const BASIC_AUTH_REALM: &str = r#"Basic realm="Trusted Server""#;

pub fn enforce_basic_auth(settings: &Settings, req: &Request) -> Option<Response> {
    let handler = settings.handler_for_path(req.get_path())?;

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
}
