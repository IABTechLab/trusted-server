use cookie::{Cookie, CookieJar};
use fastly::http::header;
use fastly::Request;

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

pub fn handle_request_cookies(req: &Request) -> Option<CookieJar> {
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

pub fn create_synthetic_cookie(settings: &Settings, synthetic_id: &str) -> String {
    format!(
        "synthetic_id={}; Domain={}; Path=/; Secure; SameSite=Lax; Max-Age={}",
        synthetic_id, settings.publisher.cookie_domain, COOKIE_MAX_AGE,
    )
}

#[cfg(test)]
mod tests {
    use crate::test_support::tests::create_test_settings;

    use super::*;

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
        let req = Request::get("http://example.com").with_header(header::COOKIE, "c1=v1;c2=v2");
        let jar = handle_request_cookies(&req).unwrap();

        assert!(jar.iter().count() == 2);
        assert_eq!(jar.get("c1").unwrap().value(), "v1");
        assert_eq!(jar.get("c2").unwrap().value(), "v2");
    }

    #[test]
    fn test_handle_request_cookies_with_empty_cookie() {
        let req = Request::get("http://example.com").with_header(header::COOKIE, "");
        let jar = handle_request_cookies(&req).unwrap();

        assert!(jar.iter().count() == 0);
    }

    #[test]
    fn test_handle_request_cookies_no_cookie_header() {
        let req: Request = Request::get("https://example.com");
        let jar = handle_request_cookies(&req);

        assert!(jar.is_none());
    }

    #[test]
    fn test_handle_request_cookies_invalid_cookie_header() {
        let req = Request::get("http://example.com").with_header(header::COOKIE, "invalid");
        let jar = handle_request_cookies(&req).unwrap();

        assert!(jar.iter().count() == 0);
    }

    #[test]
    fn test_create_synthetic_cookie() {
        let settings = create_test_settings();
        let result = create_synthetic_cookie(&settings, "12345");
        assert_eq!(
            result,
            format!(
                "synthetic_id=12345; Domain={}; Path=/; Secure; SameSite=Lax; Max-Age={}",
                settings.publisher.cookie_domain, COOKIE_MAX_AGE,
            )
        );
    }
}
