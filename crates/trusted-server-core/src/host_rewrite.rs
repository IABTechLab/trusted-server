/// Rewrite bare host occurrences (e.g. `origin.example.com/news`) only when the match is a full
/// hostname token, not part of a larger hostname like `cdn.origin.example.com`.
///
/// This is used by both HTML (`__next_f` payloads) and Flight (`text/x-component`) rewriting to
/// avoid corrupting unrelated hostnames.
pub(crate) fn rewrite_bare_host_at_boundaries(
    text: &str,
    origin_host: &str,
    request_host: &str,
) -> Option<String> {
    if origin_host.is_empty() || request_host.is_empty() || !text.contains(origin_host) {
        return None;
    }

    fn is_host_char(byte: u8) -> bool {
        byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b':')
    }

    let origin_len = origin_host.len();
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut search = 0;
    let mut replaced_any = false;

    while let Some(rel) = text[search..].find(origin_host) {
        let pos = search + rel;
        let end = pos + origin_len;

        let before_ok = pos == 0 || !is_host_char(bytes[pos - 1]);
        let after_ok = end == bytes.len() || !is_host_char(bytes[end]);

        if before_ok && after_ok {
            out.push_str(&text[search..pos]);
            out.push_str(request_host);
            replaced_any = true;
            search = end;
        } else {
            out.push_str(&text[search..pos + 1]);
            search = pos + 1;
        }
    }

    if !replaced_any {
        return None;
    }

    out.push_str(&text[search..]);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORIGIN_HOST: &str = "origin.example.com";
    const REQUEST_HOST: &str = "proxy.example.com";

    fn assert_rewrite(input: &str, expected: &str) {
        assert_eq!(
            rewrite_bare_host_at_boundaries(input, ORIGIN_HOST, REQUEST_HOST),
            Some(expected.to_string()),
            "should rewrite bare host at valid boundaries"
        );
    }

    fn assert_no_rewrite(input: &str, message: &str) {
        assert_eq!(
            rewrite_bare_host_at_boundaries(input, ORIGIN_HOST, REQUEST_HOST),
            None,
            "{message}"
        );
    }

    #[test]
    fn returns_none_when_origin_or_request_host_is_empty() {
        assert_eq!(
            rewrite_bare_host_at_boundaries("origin.example.com", "", REQUEST_HOST),
            None,
            "should ignore empty origin host"
        );
        assert_eq!(
            rewrite_bare_host_at_boundaries("origin.example.com", ORIGIN_HOST, ""),
            None,
            "should ignore empty request host"
        );
    }

    #[test]
    fn returns_none_when_origin_host_is_absent() {
        assert_no_rewrite(
            "https://other.example.com/news",
            "should return none when origin host is absent",
        );
    }

    #[test]
    fn rewrites_exact_bare_host() {
        assert_rewrite("origin.example.com", "proxy.example.com");
    }

    #[test]
    fn rewrites_bare_host_with_path_query_and_fragment() {
        assert_rewrite(
            "origin.example.com/news?x=1#top",
            "proxy.example.com/news?x=1#top",
        );
    }

    #[test]
    fn rewrites_bare_host_as_path_segment() {
        assert_rewrite(
            "https://cdn.example.com/assets/origin.example.com/image.png",
            "https://cdn.example.com/assets/proxy.example.com/image.png",
        );
    }

    #[test]
    fn rewrites_multiple_valid_occurrences() {
        assert_rewrite(
            "origin.example.com/a and origin.example.com/b",
            "proxy.example.com/a and proxy.example.com/b",
        );
    }

    #[test]
    fn rewrites_hosts_surrounded_by_punctuation_and_whitespace() {
        assert_rewrite(
            r#"{"host":"origin.example.com", "next": (origin.example.com) }"#,
            r#"{"host":"proxy.example.com", "next": (proxy.example.com) }"#,
        );
    }

    #[test]
    fn does_not_rewrite_subdomains_or_embedded_prefixes() {
        assert_no_rewrite(
            "cdn.origin.example.com",
            "should not rewrite host embedded in a subdomain",
        );
        assert_no_rewrite(
            "notorigin.example.com",
            "should not rewrite host embedded in a larger host token",
        );
        assert_no_rewrite(
            "foo-origin.example.com",
            "should not rewrite host preceded by host-character punctuation",
        );
    }

    #[test]
    fn does_not_rewrite_suffix_domains_or_host_char_continuations() {
        assert_no_rewrite(
            "origin.example.com.uk",
            "should not rewrite host followed by a domain suffix",
        );
        assert_no_rewrite(
            "origin.example.com-prod",
            "should not rewrite host followed by host-character punctuation",
        );
    }

    #[test]
    fn rewrites_origin_host_with_port_when_origin_includes_port() {
        assert_eq!(
            rewrite_bare_host_at_boundaries(
                "origin.example.com:8443/path",
                "origin.example.com:8443",
                REQUEST_HOST,
            ),
            Some("proxy.example.com/path".to_string()),
            "should rewrite host and port when origin host includes the port"
        );
    }

    #[test]
    fn does_not_rewrite_host_with_port_when_origin_omits_port() {
        assert_no_rewrite(
            "origin.example.com:8443/path",
            "should not rewrite host with port when origin omits port",
        );
    }
}
