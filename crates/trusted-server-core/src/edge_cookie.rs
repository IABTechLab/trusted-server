//! EC ID extraction from incoming HTTP requests.
//!
//! Reads an existing EC ID from the `x-ts-ec` header or `ts-ec` cookie.
//! Generation is handled by [`crate::ec::generation`].

use edgezero_core::body::Body as EdgeBody;
use error_stack::Report;
use http::Request;

use crate::constants::{COOKIE_TS_EC, HEADER_X_TS_EC};
use crate::cookies::handle_request_cookies;
use crate::ec::cookies::ec_id_has_only_allowed_chars;
use crate::error::TrustedServerError;

/// Gets an existing EC ID from the request.
///
/// Attempts to retrieve an existing EC ID from:
/// 1. The `x-ts-ec` header
/// 2. The `ts-ec` cookie
///
/// Returns `None` if neither source contains an EC ID.
///
/// # Errors
///
/// - [`TrustedServerError::InvalidHeaderValue`] if cookie parsing fails
pub fn get_ec_id(req: &Request<EdgeBody>) -> Result<Option<String>, Report<TrustedServerError>> {
    if let Some(ec_id) = req
        .headers()
        .get(HEADER_X_TS_EC)
        .and_then(|h| h.to_str().ok())
    {
        if ec_id_has_only_allowed_chars(ec_id) {
            log::trace!("Using existing EC ID from header: {}", ec_id);
            return Ok(Some(ec_id.to_string()));
        }
        log::warn!("Rejected EC ID from x-ts-ec header with disallowed characters");
    }

    match handle_request_cookies(req)? {
        Some(jar) => {
            if let Some(cookie) = jar.get(COOKIE_TS_EC) {
                let value = cookie.value();
                if ec_id_has_only_allowed_chars(value) {
                    log::trace!("Using existing EC ID from cookie: {}", value);
                    return Ok(Some(value.to_string()));
                }
                log::warn!("Rejected EC ID from cookie with disallowed characters");
            }
        }
        None => {
            log::debug!("No cookie header found in request");
        }
    }

    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::{header, HeaderName};

    fn create_test_request(headers: &[(HeaderName, &str)]) -> Request<EdgeBody> {
        let mut builder = Request::builder().method("GET").uri("http://example.com");
        for (key, value) in headers {
            builder = builder.header(key, *value);
        }
        builder
            .body(EdgeBody::empty())
            .expect("should build test request")
    }

    #[test]
    fn get_ec_id_returns_header_value_when_present() {
        let req = create_test_request(&[(HEADER_X_TS_EC, "existing_ec_id")]);
        let ec_id = get_ec_id(&req).expect("should get EC ID");
        assert_eq!(ec_id, Some("existing_ec_id".to_string()));
    }

    #[test]
    fn get_ec_id_returns_cookie_value_when_present() {
        let req = create_test_request(&[(
            header::COOKIE,
            &format!("{}=existing_cookie_id", COOKIE_TS_EC),
        )]);
        let ec_id = get_ec_id(&req).expect("should get EC ID");
        assert_eq!(ec_id, Some("existing_cookie_id".to_string()));
    }

    #[test]
    fn get_ec_id_returns_none_when_absent() {
        let req = create_test_request(&[]);
        let ec_id = get_ec_id(&req).expect("should handle missing ID");
        assert!(ec_id.is_none());
    }

    #[test]
    fn get_ec_id_rejects_header_with_disallowed_chars_falls_back_to_cookie() {
        let req = create_test_request(&[
            (HEADER_X_TS_EC, "evil;injected"),
            (header::COOKIE, &format!("{}=valid_cookie_id", COOKIE_TS_EC)),
        ]);
        let ec_id = get_ec_id(&req).expect("should handle invalid header gracefully");
        assert_eq!(
            ec_id,
            Some("valid_cookie_id".to_string()),
            "should reject tampered header and fall back to valid cookie"
        );
    }

    #[test]
    fn get_ec_id_rejects_cookie_with_disallowed_chars() {
        let req = create_test_request(&[(
            header::COOKIE,
            &format!("{}=bad<script>value", COOKIE_TS_EC),
        )]);
        let ec_id = get_ec_id(&req).expect("should handle invalid cookie gracefully");
        assert!(
            ec_id.is_none(),
            "should reject cookie with disallowed characters"
        );
    }
}
