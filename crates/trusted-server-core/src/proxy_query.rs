//! Raw query and redirect-boundary handling for creative proxy disclosure.

use url::Url;

pub(crate) fn rewrite_ec_query(target: &mut Url, ec: Option<&str>) {
    let reserved = |pair: &str| {
        let key = pair.split_once('=').map_or(pair, |(key, _)| key);
        urlencoding::decode(key).is_ok_and(|key| key == "ts-ec")
    };
    let source = target.query();
    if ec.is_none() && !source.is_some_and(|query| query.split('&').any(reserved)) {
        return;
    }
    let encoded = ec.map(|ec| {
        url::form_urlencoded::Serializer::new(String::new())
            .append_pair("ts-ec", ec)
            .finish()
    });
    let mut pairs: Vec<_> = source
        .into_iter()
        .flat_map(|query| query.split('&'))
        .filter(|pair| !reserved(pair))
        .collect();
    if let Some(encoded) = encoded.as_deref() {
        pairs.push(encoded);
    }
    let query = (!pairs.is_empty()).then(|| pairs.join("&"));
    target.set_query(query.as_deref());
}

pub(crate) fn location_path_allows_identity(location: &str) -> bool {
    let before_query = location.split(['?', '#']).next().unwrap_or_default();
    let absolute = before_query.split_once("://").filter(|(scheme, _)| {
        scheme
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphabetic)
            && scheme
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.'))
    });
    let path = if let Some((_, remainder)) = absolute {
        remainder.find('/').map_or("", |index| &remainder[index..])
    } else if let Some(remainder) = before_query.strip_prefix("//") {
        remainder.find('/').map_or("", |index| &remainder[index..])
    } else {
        before_query
    };
    !before_query.contains('\\') && !path.contains('%')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_query_removes_all_once_decoded_reserved_keys_without_reencoding() {
        let mut target = Url::parse("https://example.com/event?a=%2f&ts-ec=old&key&ts%2Dec=echo&empty=&TS-EC=keep&ts%252Dec=nested&bad%=keep&&z=hello+there").expect("should parse target");
        rewrite_ec_query(&mut target, None);
        assert_eq!(
            target.query(),
            Some("a=%2f&key&empty=&TS-EC=keep&ts%252Dec=nested&bad%=keep&&z=hello+there"),
            "should preserve untouched raw query pairs"
        );
        rewrite_ec_query(&mut target, Some("new +&"));
        assert_eq!(
            target.query(),
            Some(
                "a=%2f&key&empty=&TS-EC=keep&ts%252Dec=nested&bad%=keep&&z=hello+there&ts-ec=new+%2B%26"
            ),
            "should append exactly one safely encoded EC"
        );
    }

    #[test]
    fn proxy_query_preserves_unchanged_query_and_removes_empty_reserved_query() {
        for source in [
            "https://example.com/event",
            "https://example.com/event?",
            "https://example.com/event?a=%2f&&key",
        ] {
            let mut target = Url::parse(source).expect("should parse target");
            rewrite_ec_query(&mut target, None);
            assert_eq!(
                target.as_str(),
                source,
                "should leave query bytes unchanged without reserved keys"
            );
        }
        let mut target =
            Url::parse("https://example.com/event?ts-ec&ts%2dec=x").expect("should parse target");
        rewrite_ec_query(&mut target, None);
        assert_eq!(
            target.query(),
            None,
            "should remove query when every pair was reserved"
        );
    }

    #[test]
    fn proxy_redirect_path_checks_before_url_normalization() {
        for location in [
            "/events/%2e%2e/event",
            "/foo://%2e%2e/event",
            "/events\\event",
            "https://example.com/%65vent?q=1",
            "//example.com/%65vent",
        ] {
            assert!(
                !location_path_allows_identity(location),
                "should deny raw ambiguous path {location}"
            );
        }
        for location in [
            "?next=%2f",
            "#fragment",
            "/events/event?x=%2f",
            "https://example.com/event?x=%2f",
            "//example.com/event?x=%2f",
            "../event",
            "event",
        ] {
            assert!(
                location_path_allows_identity(location),
                "should permit unescaped raw path {location}"
            );
        }
    }
}
