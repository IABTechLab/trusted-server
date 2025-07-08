//! Synthetic ID generation using HMAC.
//!
//! This module provides functionality for generating privacy-preserving synthetic IDs
//! based on various request parameters and a secret key.

use error_stack::{Report, ResultExt};
use fastly::http::header;
use fastly::Request;
use handlebars::Handlebars;
use hmac::{Hmac, Mac};
use serde_json::json;
use sha2::Sha256;

use crate::constants::{HEADER_SYNTHETIC_PUB_USER_ID, HEADER_SYNTHETIC_TRUSTED_SERVER};
use crate::cookies::handle_request_cookies;
use crate::error::TrustedServerError;
use crate::settings::Settings;

type HmacSha256 = Hmac<Sha256>;

/// Generates a fresh synthetic ID based on request parameters.
///
/// Creates a deterministic ID using HMAC-SHA256 with the configured secret key
/// and various request attributes including IP, user agent, cookies, and headers.
///
/// # Errors
///
/// - [`TrustedServerError::Template`] if the template rendering fails
/// - [`TrustedServerError::SyntheticId`] if HMAC generation fails
pub fn generate_synthetic_id(
    settings: &Settings,
    req: &Request,
) -> Result<String, Report<TrustedServerError>> {
    let user_agent = req
        .get_header(header::USER_AGENT)
        .map(|h| h.to_str().unwrap_or("unknown"));
    let first_party_id = handle_request_cookies(req).ok().flatten().and_then(|jar| {
        jar.get("pub_userid")
            .map(|cookie| cookie.value().to_string())
    });
    let auth_user_id = req
        .get_header(HEADER_SYNTHETIC_PUB_USER_ID)
        .map(|h| h.to_str().unwrap_or("anonymous"));
    let publisher_domain = req
        .get_header(header::HOST)
        .map(|h| h.to_str().unwrap_or("unknown"));
    let client_ip = req.get_client_ip_addr().map(|ip| ip.to_string());
    let accept_language = req
        .get_header(header::ACCEPT_LANGUAGE)
        .and_then(|h| h.to_str().ok())
        .map(|lang| lang.split(',').next().unwrap_or("unknown"));

    let handlebars = Handlebars::new();
    let data = &json!({
        "client_ip": client_ip.unwrap_or("unknown".to_string()),
        "user_agent": user_agent.unwrap_or("unknown"),
        "first_party_id": first_party_id.unwrap_or("anonymous".to_string()),
        "auth_user_id": auth_user_id.unwrap_or("anonymous"),
        "publisher_domain": publisher_domain.unwrap_or("unknown.com"),
        "accept_language": accept_language.unwrap_or("unknown")
    });

    let input_string = handlebars
        .render_template(&settings.synthetic.template, data)
        .change_context(TrustedServerError::Template {
            message: "Failed to render synthetic ID template".to_string(),
        })?;

    log::info!("Input string for fresh ID: {} {}", input_string, data);

    let mut mac = HmacSha256::new_from_slice(settings.synthetic.secret_key.as_bytes())
        .change_context(TrustedServerError::SyntheticId {
            message: "Failed to create HMAC instance".to_string(),
        })?;
    mac.update(input_string.as_bytes());
    let fresh_id = hex::encode(mac.finalize().into_bytes());

    log::info!("Generated fresh ID: {}", fresh_id);

    Ok(fresh_id)
}

/// Gets or creates a synthetic ID from the request.
///
/// Attempts to retrieve an existing synthetic ID from:
/// 1. The `X-Synthetic-Trusted-Server` header
/// 2. The `synthetic_id` cookie
///
/// If neither exists, generates a new synthetic ID.
///
/// # Errors
///
/// - [`TrustedServerError::Template`] if template rendering fails during generation
/// - [`TrustedServerError::SyntheticId`] if ID generation fails
pub fn get_or_generate_synthetic_id(
    settings: &Settings,
    req: &Request,
) -> Result<String, Report<TrustedServerError>> {
    // First try to get existing Trusted Server ID from header
    if let Some(synthetic_id) = req
        .get_header(HEADER_SYNTHETIC_TRUSTED_SERVER)
        .and_then(|h| h.to_str().ok())
        .map(|s| s.to_string())
    {
        log::info!("Using existing Synthetic ID from header: {}", synthetic_id);
        return Ok(synthetic_id);
    }

    // Try to get synthetic ID from cookies
    match handle_request_cookies(req)? {
        Some(jar) => {
            if let Some(cookie) = jar.get("synthetic_id") {
                let id = cookie.value().to_string();
                log::info!("Using existing Trusted Server ID from cookie: {}", id);
                return Ok(id);
            }
        }
        None => {
            log::debug!("No cookie header found in request");
        }
    }

    // If no existing Synthetic ID found, generate a fresh one
    let fresh_id = generate_synthetic_id(settings, req)?;
    log::info!(
        "No existing Synthetic ID found, using fresh ID: {}",
        fresh_id
    );
    Ok(fresh_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fastly::http::{HeaderName, HeaderValue};

    use crate::constants::HEADER_X_PUB_USER_ID;
    use crate::test_support::tests::create_test_settings;

    fn create_test_request(headers: Vec<(HeaderName, &str)>) -> Request {
        let mut req = Request::new("GET", "http://example.com");
        for (key, value) in headers {
            req.set_header(
                key,
                HeaderValue::from_str(value).expect("should create valid header value"),
            );
        }

        req
    }

    #[test]
    fn test_generate_synthetic_id() {
        let settings: Settings = create_test_settings();
        let req = create_test_request(vec![
            (header::USER_AGENT, "Mozilla/5.0"),
            (header::COOKIE, "pub_userid=12345"),
            (HEADER_X_PUB_USER_ID, "67890"),
            (header::HOST, settings.publisher.domain.as_str()),
            (header::ACCEPT_LANGUAGE, "en-US,en;q=0.9"),
        ]);

        let synthetic_id =
            generate_synthetic_id(&settings, &req).expect("should generate synthetic ID");
        log::info!("Generated synthetic ID: {}", synthetic_id);
        assert_eq!(
            synthetic_id,
            "a1748067b3908f2c9e0f6ea30a341328ba4b84de45448b13d1007030df14a98e"
        )
    }

    #[test]
    fn test_get_or_generate_synthetic_id_with_header() {
        let settings = create_test_settings();
        let req = create_test_request(vec![(
            HEADER_SYNTHETIC_TRUSTED_SERVER,
            "existing_synthetic_id",
        )]);

        let synthetic_id = get_or_generate_synthetic_id(&settings, &req)
            .expect("should get or generate synthetic ID");
        assert_eq!(synthetic_id, "existing_synthetic_id");
    }

    #[test]
    fn test_get_or_generate_synthetic_id_with_cookie() {
        let settings = create_test_settings();
        let req = create_test_request(vec![(header::COOKIE, "synthetic_id=existing_cookie_id")]);

        let synthetic_id = get_or_generate_synthetic_id(&settings, &req)
            .expect("should get or generate synthetic ID");
        assert_eq!(synthetic_id, "existing_cookie_id");
    }

    #[test]
    fn test_get_or_generate_synthetic_id_generate_new() {
        let settings = create_test_settings();
        let req = create_test_request(vec![]);

        let synthetic_id = get_or_generate_synthetic_id(&settings, &req)
            .expect("should get or generate synthetic ID");
        assert!(!synthetic_id.is_empty());
    }
}
