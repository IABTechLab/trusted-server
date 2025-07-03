use fastly::http::header;
use fastly::Request;
use handlebars::Handlebars;
use hmac::{Hmac, Mac};
use serde_json::json;
use sha2::Sha256;

use crate::constants::{HEADER_SYNTHETIC_PUB_USER_ID, HEADER_SYNTHETIC_TRUSTED_SERVER};
use crate::cookies::handle_request_cookies;
use crate::settings::Settings;

type HmacSha256 = Hmac<Sha256>;

/// Generates a fresh synthetic_id based on request parameters
pub fn generate_synthetic_id(settings: &Settings, req: &Request) -> String {
    let user_agent = req
        .get_header(header::USER_AGENT)
        .map(|h| h.to_str().unwrap_or("unknown"));
    let first_party_id = handle_request_cookies(req).and_then(|jar| {
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
        .unwrap();
    log::info!("Input string for fresh ID: {} {}", input_string, data);

    let mut mac = HmacSha256::new_from_slice(settings.synthetic.secret_key.as_bytes())
        .expect("HMAC can take key of any size");
    mac.update(input_string.as_bytes());
    let fresh_id = hex::encode(mac.finalize().into_bytes());

    log::info!("Generated fresh ID: {}", fresh_id);

    fresh_id
}

/// Gets or creates a synthetic_id from the request
pub fn get_or_generate_synthetic_id(settings: &Settings, req: &Request) -> String {
    // First try to get existing Trusted Server ID from header
    if let Some(synthetic_id) = req
        .get_header(HEADER_SYNTHETIC_TRUSTED_SERVER)
        .and_then(|h| h.to_str().ok())
        .map(|s| s.to_string())
    {
        log::info!("Using existing Synthetic ID from header: {}", synthetic_id);
        return synthetic_id;
    }

    let req_cookie_jar: Option<cookie::CookieJar> = handle_request_cookies(req);
    match req_cookie_jar {
        Some(jar) => {
            let ts_cookie = jar.get("synthetic_id");
            if let Some(cookie) = ts_cookie {
                let id = cookie.value().to_string();
                log::info!("Using existing Trusted Server ID from cookie: {}", id);
                return id;
            }
        }
        None => {
            log::warn!("No cookie header found in request");
        }
    }

    // If no existing Synthetic ID found, generate a fresh one
    let fresh_id = generate_synthetic_id(settings, req);
    log::info!(
        "No existing Synthetic ID found, using fresh ID: {}",
        fresh_id
    );
    fresh_id
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::HEADER_X_PUB_USER_ID;
    use fastly::http::{HeaderName, HeaderValue};

    fn create_test_request(headers: Vec<(HeaderName, &str)>) -> Request {
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
                template: "{{ client_ip }}:{{ user_agent }}:{{ first_party_id }}:{{ auth_user_id }}:{{ publisher_domain }}:{{ accept_language }}".to_string(),
            },
        }
    }

    #[test]
    fn test_generate_synthetic_id() {
        let settings: Settings = create_settings();
        let req = create_test_request(vec![
            (header::USER_AGENT, "Mozilla/5.0"),
            (header::COOKIE, "pub_userid=12345"),
            (HEADER_X_PUB_USER_ID, "67890"),
            (header::HOST, "example.com"),
            (header::ACCEPT_LANGUAGE, "en-US,en;q=0.9"),
        ]);

        let synthetic_id = generate_synthetic_id(&settings, &req);
        log::info!("Generated synthetic ID: {}", synthetic_id);
        assert_eq!(
            synthetic_id,
            "07cd73bb8c7db39753ab6b10198b10c3237a3f5a6d2232c6ce578f2c2a623e56"
        )
    }

    #[test]
    fn test_get_or_generate_synthetic_id_with_header() {
        let settings = create_settings();
        let req = create_test_request(vec![(
            HEADER_SYNTHETIC_TRUSTED_SERVER,
            "existing_synthetic_id",
        )]);

        let synthetic_id = get_or_generate_synthetic_id(&settings, &req);
        assert_eq!(synthetic_id, "existing_synthetic_id");
    }

    #[test]
    fn test_get_or_generate_synthetic_id_with_cookie() {
        let settings = create_settings();
        let req = create_test_request(vec![(header::COOKIE, "synthetic_id=existing_cookie_id")]);

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
