//! The evidence a cloud call sends for a request, being the `User-Agent` and
//! the `Sec-CH-UA` client hints the browser sent with it.
//!
//! Which hints the service asks for depends on the browser alone, so nothing
//! else is sent, neither the address nor a cookie. An answer is keyed by the
//! evidence, so where a loading cache module holds it, every reader whose
//! browser sends the same values shares one answer and one call.

use trusted_server_core::evidence::RequestInfo;

use crate::client::Evidence;

/// Builds the evidence for a request, being its `User-Agent` and each of its
/// `Sec-CH-UA` headers, by lower case name and in name order, so the same
/// headers always give the same cache key.
#[must_use]
pub fn evidence_for(request_info: &dyn RequestInfo) -> Evidence {
    let mut client_hints: Vec<(String, String)> = request_info
        .header_names()
        .into_iter()
        .filter(|name| is_user_agent_hint(name))
        .filter_map(|name| {
            request_info
                .header(name)
                .map(|value| (name.to_ascii_lowercase(), value.to_owned()))
        })
        .collect();
    client_hints.sort();
    Evidence {
        user_agent: request_info.user_agent().to_owned(),
        client_hints,
    }
}

/// Whether `name` is `Sec-CH-UA` or one of the `Sec-CH-UA-` hints, which
/// describe the browser and the device it runs on.
fn is_user_agent_hint(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name == "sec-ch-ua" || name.starts_with("sec-ch-ua-")
}

#[cfg(test)]
mod tests {
    use trusted_server_core::evidence::OwnedRequestInfo;

    use super::*;

    const CHROME: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                          (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36";

    /// A request from an address, carrying `headers`.
    fn request_with(headers: &[(&'static str, &'static str)]) -> OwnedRequestInfo {
        let mut map = http::HeaderMap::new();
        for (name, value) in headers {
            map.append(
                http::HeaderName::from_static(name),
                http::HeaderValue::from_static(value),
            );
        }
        OwnedRequestInfo::new("2.125.160.216".to_owned(), map)
    }

    /// The `User-Agent` and the `Sec-CH-UA` hints are the whole of the
    /// evidence. The address, the cookies, a hint outside that family and
    /// every other header are left out.
    #[test]
    fn the_evidence_is_the_user_agent_and_the_sec_ch_ua_hints() {
        let request = request_with(&[
            ("user-agent", CHROME),
            ("sec-ch-ua-platform", "\"Windows\""),
            ("sec-ch-ua", "\"Chromium\";v=\"140\""),
            ("sec-ch-ua-mobile", "?0"),
            ("sec-ch-prefers-color-scheme", "dark"),
            ("cookie", "51D_ScreenPixelsWidth=1080; ts-ec=something"),
            ("accept-language", "en-GB"),
        ]);

        assert_eq!(
            evidence_for(&request),
            Evidence {
                user_agent: CHROME.to_owned(),
                client_hints: vec![
                    ("sec-ch-ua".to_owned(), "\"Chromium\";v=\"140\"".to_owned()),
                    ("sec-ch-ua-mobile".to_owned(), "?0".to_owned()),
                    ("sec-ch-ua-platform".to_owned(), "\"Windows\"".to_owned()),
                ],
            }
        );
    }

    /// The hints are in name order whatever order the browser sent them in,
    /// because the evidence is part of the cache key.
    #[test]
    fn the_hints_are_in_name_order_whatever_order_they_came_in() {
        let one = request_with(&[("sec-ch-ua-mobile", "?0"), ("sec-ch-ua", "\"A\"")]);
        let other = request_with(&[("sec-ch-ua", "\"A\""), ("sec-ch-ua-mobile", "?0")]);

        assert_eq!(evidence_for(&one), evidence_for(&other));
        assert_eq!(evidence_for(&one).client_hints[0].0, "sec-ch-ua");
    }
}
