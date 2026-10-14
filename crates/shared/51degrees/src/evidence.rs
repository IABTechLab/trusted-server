//! The evidence a cloud call sends for a request, being the address, the
//! `User-Agent` and what the browser gathered.
//!
//! The answer is held by the evidence, so the same visitor asking again
//! costs no call.

use std::net::IpAddr;

use trusted_server_core::evidence::RequestInfo;

use crate::client::{BROWSER_EVIDENCE, Evidence};

/// Builds the evidence for a request.
///
/// The address is parsed and written back out, so the same address written two
/// ways, such as `2001:db8::1` and `2001:0db8:0000::0001`, gives one key and
/// one call.
#[must_use]
pub fn evidence_for(request_info: &dyn RequestInfo) -> Evidence {
    let raw = request_info.client_ip();
    let client_ip = raw
        .parse::<IpAddr>()
        .map_or_else(|_| raw.to_owned(), |address| address.to_string());
    Evidence {
        client_ip,
        user_agent: request_info.user_agent().to_owned(),
        browser: browser_evidence(request_info),
    }
}

/// Reads the evidence the browser gathered from the request's cookies.
///
/// The screen and the high entropy client hints exist only in the browser, so
/// a page writes them to `51D_` cookies. A first page view has none, and the
/// service answers from the `User-Agent`.
fn browser_evidence(request_info: &dyn RequestInfo) -> Vec<(&'static str, String)> {
    let Some(header) = request_info.header("cookie") else {
        return Vec::new();
    };
    let mut found = Vec::new();
    // In BROWSER_EVIDENCE order rather than header order, so the same cookies
    // always give the same cache key.
    for name in BROWSER_EVIDENCE {
        if let Some(value) = cookie_value(header, name) {
            found.push((*name, value));
        }
    }
    found
}

/// The value of one cookie in a `Cookie` header, or `None`.
///
/// Split on the first `=` only, because a base64 value carries `=` padding.
fn cookie_value(header: &str, name: &str) -> Option<String> {
    header.split(';').find_map(|part| {
        let part = part.trim();
        let (key, value) = part.split_once('=')?;
        (key == name && !value.is_empty()).then(|| value.to_owned())
    })
}

#[cfg(test)]
mod tests {
    use trusted_server_core::evidence::OwnedRequestInfo;

    use super::*;

    const CHROME: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                          (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36";

    fn request_with_cookies(cookies: &str) -> OwnedRequestInfo {
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::USER_AGENT,
            http::HeaderValue::from_static(CHROME),
        );
        headers.insert(
            http::header::COOKIE,
            http::HeaderValue::from_str(cookies).expect("should build the test header"),
        );
        OwnedRequestInfo::new("2.125.160.216".to_owned(), headers)
    }

    /// The browser's evidence is read off its `51D_` cookies, in a fixed order
    /// whatever order the browser sent them in, because the evidence is part
    /// of the cache key.
    #[test]
    fn the_browsers_evidence_is_read_off_its_cookies() {
        /// A case's label, the `Cookie` header, and the evidence expected.
        type Case = (
            &'static str,
            &'static str,
            &'static [(&'static str, &'static str)],
        );

        const SCREEN: &[(&str, &str)] = &[
            ("51D_ScreenPixelsWidth", "1080"),
            ("51D_ScreenPixelsHeight", "2424"),
        ];
        let cases: &[Case] = &[
            (
                // The screen exists only in the browser, so this is the only way
                // the server ever learns it.
                "the screen, beside another cookie",
                "ts-ec=something; 51D_ScreenPixelsWidth=1080; 51D_ScreenPixelsHeight=2424",
                SCREEN,
            ),
            (
                "the screen, sent the other way round",
                "51D_ScreenPixelsHeight=2424; 51D_ScreenPixelsWidth=1080",
                SCREEN,
            ),
            (
                // Nothing has run in the browser yet on a first page view.
                "a first page view",
                "ts-ec=something",
                &[],
            ),
            (
                // The high entropy hints are base64 with `=` padding, which a split
                // on every `=` would cut off.
                "a base64 value and its padding",
                "51D_GetHighEntropyValues=eyJtb2RlbCI6IlBpeGVsIn0=",
                &[("51D_GetHighEntropyValues", "eyJtb2RlbCI6IlBpeGVsIn0=")],
            ),
            (
                // An empty value tells the service nothing and would only split
                // the cache.
                "an empty value",
                "51D_ScreenPixelsWidth=; 51D_ScreenPixelsHeight=2424",
                &[("51D_ScreenPixelsHeight", "2424")],
            ),
        ];
        for &(case, cookies, expected) in cases {
            let expected: Vec<(&str, String)> = expected
                .iter()
                .map(|(name, value)| (*name, (*value).to_owned()))
                .collect();

            assert_eq!(
                evidence_for(&request_with_cookies(cookies)).browser,
                expected,
                "{case}"
            );
        }
    }

    /// The address is written back out in its one canonical form, so the same
    /// address written two ways is one cache key and one call. An address that
    /// does not parse is passed on as it came.
    #[test]
    fn the_address_is_sent_in_one_form() {
        for (case, address, expected) in [
            (
                "an IPv6 address written out in full",
                "2001:0db8:0000::0001",
                "2001:db8::1",
            ),
            (
                "the same address written short",
                "2001:db8::1",
                "2001:db8::1",
            ),
            ("an IPv4 address", "192.0.2.1", "192.0.2.1"),
            (
                "an address that does not parse",
                "not-an-address",
                "not-an-address",
            ),
        ] {
            let request = OwnedRequestInfo::new(address.to_owned(), http::HeaderMap::new());

            assert_eq!(evidence_for(&request).client_ip, expected, "{case}");
        }
    }
}
