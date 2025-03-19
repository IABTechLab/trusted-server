use fastly::http::header;
use fastly::Request;
use hmac::{Hmac, Mac};
use log;
use sha2::Sha256;

use crate::constants::{SECRET_KEY, SYNTH_HEADER_POTSI};
use crate::cookies::handle_request_cookies;

type HmacSha256 = Hmac<Sha256>;

/// Generates a fresh synthetic_id based on request parameters
pub fn generate_synthetic_id(req: &Request) -> String {
    let user_agent = req
        .get_header(header::USER_AGENT)
        .map(|h| h.to_str().unwrap_or("Unknown"));
    let first_party_id = handle_request_cookies(req).and_then(|jar| {
        jar.get("pub_userid")
            .map(|cookie| cookie.value().to_string())
    });
    let auth_user_id = req
        .get_header("X-Pub-User-ID")
        .map(|h| h.to_str().unwrap_or("anonymous"));
    let publisher_domain = req
        .get_header(header::HOST)
        .map(|h| h.to_str().unwrap_or("unknown.com"));
    let client_ip = req.get_client_ip_addr().map(|ip| ip.to_string());
    let accept_language = req
        .get_header(header::ACCEPT_LANGUAGE)
        .and_then(|h| h.to_str().ok())
        .map(|lang| lang.split(',').next().unwrap_or("unknown"));

    let input_string = format!(
        "{}:{}:{}:{}:{}:{}",
        client_ip.unwrap_or("unknown".to_string()),
        user_agent.unwrap_or("unknown"),
        first_party_id.unwrap_or("anonymous".to_string()),
        auth_user_id.unwrap_or("anonymous"),
        publisher_domain.unwrap_or("unknown.com"),
        accept_language.unwrap_or("unknown")
    );

    log::info!("Input string for fresh ID: {}", input_string);

    let mut mac = HmacSha256::new_from_slice(SECRET_KEY).expect("HMAC can take key of any size");
    mac.update(input_string.as_bytes());
    let fresh_id = hex::encode(mac.finalize().into_bytes());

    log::info!("Generated fresh ID: {}", fresh_id);

    fresh_id
}

/// Gets or creates a synthetic_id from the request
pub fn get_or_generate_synthetic_id(req: &Request) -> String {
    // First try to get existing POTSI ID from header
    if let Some(potsi) = req
        .get_header(SYNTH_HEADER_POTSI)
        .and_then(|h| h.to_str().ok())
        .map(|s| s.to_string())
    {
        log::info!("Using existing POTSI ID from header: {}", potsi);
        return potsi;
    }

    let req_cookie_jar: Option<cookie::CookieJar> = handle_request_cookies(&req);
    match req_cookie_jar {
        Some(jar) => {
            let potsi_cookie = jar.get("synthetic_id");
            if let Some(cookie) = potsi_cookie {
                let potsi = cookie.value().to_string();
                log::info!("Using existing POTSI ID from cookie: {}", potsi);
                return potsi;
            }
        }
        None => {
            log::warn!("No cookie header found in request");
        }
    }

    // If no existing POTSI ID found, generate a fresh one
    let fresh_id = generate_synthetic_id(req);
    log::info!("No existing POTSI ID found, using fresh ID: {}", fresh_id);
    fresh_id
}

#[cfg(test)]
mod tests {
    use super::*;
    use fastly::http::HeaderValue;

    fn create_test_request(headers: Vec<(&str, &str)>) -> Request {
        let mut req = Request::new("GET", "http://example.com");
        for (key, value) in headers {
            req.set_header(key, HeaderValue::from_str(value).unwrap());
        }

        req
    }

    #[test]
    fn test_generate_synthetic_id() {
        let req = create_test_request(vec![
            (&header::USER_AGENT.to_string(), "Mozilla/5.0"),
            (&header::COOKIE.to_string(), "pub_userid=12345"),
            ("X-Pub-User-ID", "67890"),
            (&header::HOST.to_string(), "example.com"),
            (&header::ACCEPT_LANGUAGE.to_string(), "en-US,en;q=0.9"),
        ]);

        let synthetic_id = generate_synthetic_id(&req);
        assert_eq!(
            synthetic_id,
            "5023f58a61668e5405a804d18662fc0b37518875cac551ed86e5e7223b541600"
        )
    }

    #[test]
    fn test_get_or_generate_synthetic_id_with_header() {
        let req = create_test_request(vec![(SYNTH_HEADER_POTSI, "existing_potsi_id")]);

        let synthetic_id = get_or_generate_synthetic_id(&req);
        assert_eq!(synthetic_id, "existing_potsi_id");
    }

    #[test]
    fn test_get_or_generate_synthetic_id_with_cookie() {
        let req = create_test_request(vec![(
            &header::COOKIE.to_string(),
            "synthetic_id=existing_cookie_id",
        )]);

        let synthetic_id = get_or_generate_synthetic_id(&req);
        assert_eq!(synthetic_id, "existing_cookie_id");
    }

    #[test]
    fn test_get_or_generate_synthetic_id_generate_new() {
        let req = create_test_request(vec![]);

        let synthetic_id = get_or_generate_synthetic_id(&req);
        assert!(!synthetic_id.is_empty());
    }
}
