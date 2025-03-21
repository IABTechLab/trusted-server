use fastly::http::header;
use fastly::Request;
use hmac::{Hmac, Mac};
use log;
use sha2::Sha256;

use crate::constants::SYNTH_HEADER_POTSI;
use crate::cookies::handle_request_cookies;
use crate::settings::Settings;

type HmacSha256 = Hmac<Sha256>;

/// Generates a fresh synthetic_id based on request parameters
pub fn generate_synthetic_id(settings: &Settings, req: &Request) -> String {
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

    let input_string = format!(
        "{}:{}:{}",
        first_party_id.unwrap_or("anonymous".to_string()),
        auth_user_id.unwrap_or("anonymous"),
        publisher_domain.unwrap_or("unknown.com"),
    );

    log::info!("Input string for fresh ID: {}", input_string);

    let mut mac = HmacSha256::new_from_slice(settings.synthetic.secret_key.as_bytes())
        .expect("HMAC can take key of any size");
    mac.update(input_string.as_bytes());
    let fresh_id = hex::encode(mac.finalize().into_bytes());

    log::info!("Generated fresh ID: {}", fresh_id);

    fresh_id
}

/// Gets or creates a synthetic_id from the request
pub fn get_or_generate_synthetic_id(settings: &Settings, req: &Request) -> String {
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
    let fresh_id = generate_synthetic_id(settings, req);
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

    fn create_settings() -> Settings {
        Settings {
            ad_server: crate::settings::AdServer {
                ad_partner_url: "https://example.com".to_string(),
                sync_url: "https://example.com/synthetic_id={{synthetic_id}}".to_string(),
            },
            prebid: crate::settings::Prebid {
                server_url: "https://example.com".to_string(),
            },
            synthetic: crate::settings::Synthetic {
                counter_store: "https://example.com".to_string(),
                opid_store: "https://example.com".to_string(),
                secret_key: "secret_key".to_string(),
            },
        }
    }

    #[test]
    fn test_generate_synthetic_id() {
        log::info!("Hello!");
        let settings: Settings = create_settings();
        let req = create_test_request(vec![
            (&header::COOKIE.to_string(), "pub_userid=12345"),
            ("X-Pub-User-ID", "67890"),
            (&header::HOST.to_string(), "example.com"),
        ]);

        let synthetic_id = generate_synthetic_id(&settings, &req);

        log::info!("Generated Synthetic ID: {}", synthetic_id);

        assert_eq!(
            synthetic_id,
            "f109d9239172c7bd5af42fbb25b6018e95ee1e44660ec3b4ea7bd006e232b13e"
        )
    }

    #[test]
    fn test_get_or_generate_synthetic_id_with_header() {
        let settings = create_settings();
        let req = create_test_request(vec![(SYNTH_HEADER_POTSI, "existing_potsi_id")]);

        let synthetic_id = get_or_generate_synthetic_id(&settings, &req);
        assert_eq!(synthetic_id, "existing_potsi_id");
    }

    #[test]
    fn test_get_or_generate_synthetic_id_with_cookie() {
        let settings = create_settings();
        let req = create_test_request(vec![(
            &header::COOKIE.to_string(),
            "synthetic_id=existing_cookie_id",
        )]);

        let synthetic_id = get_or_generate_synthetic_id(&settings, &req);
        assert_eq!(synthetic_id, "existing_cookie_id");
    }

    #[test]
    fn test_get_or_generate_synthetic_id_generate_new() {
        let settings = create_settings();
        let req = create_test_request(vec![]);

        let synthetic_id = get_or_generate_synthetic_id(&settings, &req);
        assert!(!synthetic_id.is_empty());
    }
}
