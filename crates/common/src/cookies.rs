use cookie::{Cookie, CookieJar};
use http::header;

use crate::http_wrapper::RequestWrapper;
use crate::settings::Settings;

const COOKIE_MAX_AGE: i32 = 365 * 24 * 60 * 60; // 1 year

// return empty cookie jar for unparsable cookies
pub fn parse_cookies_to_jar(s: &str) -> CookieJar {
    let cookie_str = s.trim().to_owned();
    let mut jar = CookieJar::new();
    let cookies = Cookie::split_parse(cookie_str).filter_map(Result::ok);

    for cookie in cookies {
        jar.add_original(cookie);
    }

    jar
}

pub fn handle_request_cookies<T: RequestWrapper>(req: &T) -> Option<CookieJar> {
    match req.get_header(header::COOKIE) {
        Some(header_value) => {
            let header_value_str: &str = header_value.to_str().unwrap_or("");
            let jar: CookieJar = parse_cookies_to_jar(header_value_str);
            Some(jar)
        }
        None => {
            log::warn!("No cookie header found in request");
            None
        }
    }
}

pub fn create_synthetic_cookie(synthetic_id: &str, settings: &Settings) -> String {
    format!(
        "synthetic_id={}; Domain={}; Path=/; Secure; SameSite=Lax; Max-Age={}",
        synthetic_id, settings.server.cookie_domain, COOKIE_MAX_AGE,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::http_wrapper::tests::HttpRequestWrapper;
    use http::request;

    #[test]
    fn test_parse_cookies_to_jar() {
        let header_value = "c1=v1; c2=v2";
        let jar = parse_cookies_to_jar(header_value);

        assert!(jar.iter().count() == 2);
        assert_eq!(jar.get("c1").unwrap().value(), "v1");
        assert_eq!(jar.get("c2").unwrap().value(), "v2");
    }

    #[test]
    fn test_parse_cookies_to_jar_not_unique() {
        let cookie_str = "c1=v1;c1=v2";
        let jar = parse_cookies_to_jar(cookie_str);

        assert!(jar.iter().count() == 1);
        assert_eq!(jar.get("c1").unwrap().value(), "v2");
    }

    #[test]
    fn test_parse_cookies_to_jar_emtpy() {
        let cookie_str = "";
        let jar = parse_cookies_to_jar(cookie_str);

        assert!(jar.iter().count() == 0);
    }

    #[test]
    fn test_parse_cookies_to_jar_invalid() {
        let cookie_str = "invalid";
        let jar = parse_cookies_to_jar(cookie_str);

        assert!(jar.iter().count() == 0);
    }

    #[test]
    fn test_handle_request_cookies() {
        let builder = request::Builder::new()
            .method("GET")
            .uri("http://example.com")
            .header(header::COOKIE, "c1=v1; c2=v2");
        let req = HttpRequestWrapper::new(builder);
        let jar = handle_request_cookies(&req).unwrap();

        assert!(jar.iter().count() == 2);
        assert_eq!(jar.get("c1").unwrap().value(), "v1");
        assert_eq!(jar.get("c2").unwrap().value(), "v2");
    }

    #[test]
    fn test_handle_request_cookies_with_empty_cookie() {
        let builder = request::Builder::new()
            .method("GET")
            .uri("http://example.com")
            .header(header::COOKIE, "");
        let req = HttpRequestWrapper::new(builder);
        let jar = handle_request_cookies(&req).unwrap();

        assert!(jar.iter().count() == 0);
    }

    #[test]
    fn test_handle_request_cookies_no_cookie_header() {
        let builder = request::Builder::new()
            .method("GET")
            .uri("http://example.com");
        let req = HttpRequestWrapper::new(builder);
        let jar = handle_request_cookies(&req);

        assert!(jar.is_none());
    }

    #[test]
    fn test_handle_request_cookies_invalid_cookie_header() {
        let builder = request::Builder::new()
            .method("GET")
            .uri("http://example.com")
            .header(header::COOKIE, "invalid");
        let req = HttpRequestWrapper::new(builder);
        let jar = handle_request_cookies(&req).unwrap();

        assert!(jar.iter().count() == 0);
    }

    #[test]
    fn test_create_synthetic_cookie() {
        // Create a test settings
        let settings_toml = r#"
[server]
domain = "example.com"
cookie_domain = ".example.com"

[ad_server]
ad_partner_url = "test"
sync_url = "test"

[prebid]
server_url = "test"

[synthetic]
counter_store = "test"
opid_store = "test"
secret_key = "test-key"
template = "test"
        "#;

        let settings: Settings = config::Config::builder()
            .add_source(config::File::from_str(
                settings_toml,
                config::FileFormat::Toml,
            ))
            .build()
            .unwrap()
            .try_deserialize()
            .unwrap();

        let result = create_synthetic_cookie("12345", &settings);
        assert_eq!(
            result,
            "synthetic_id=12345; Domain=.example.com; Path=/; Secure; SameSite=Lax; Max-Age=31536000"
        );
    }
}
