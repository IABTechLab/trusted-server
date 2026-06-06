/// Rewrite bare host occurrences (e.g. `origin.example.com/news`) only when the match is a full
/// hostname token, not part of a larger hostname like `cdn.origin.example.com`.
///
/// A numeric `:port` immediately after the host is treated as part of a standalone authority and
/// is preserved when rewriting the host.
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

    fn has_valid_port_boundary(bytes: &[u8], port_start: usize) -> bool {
        let mut index = port_start;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
        }

        index > port_start && (index == bytes.len() || !is_host_char(bytes[index]))
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
        let after_ok = end == bytes.len()
            || if bytes[end] == b':' {
                has_valid_port_boundary(bytes, end + 1)
            } else {
                !is_host_char(bytes[end])
            };

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

    fn rewrite(text: &str) -> Option<String> {
        rewrite_bare_host_at_boundaries(text, ORIGIN_HOST, REQUEST_HOST)
    }

    #[test]
    fn rewrites_exact_host() {
        assert_eq!(
            rewrite(ORIGIN_HOST),
            Some(REQUEST_HOST.to_string()),
            "should rewrite a host that spans the whole input"
        );
    }

    #[test]
    fn rewrites_valid_boundary_separators() {
        let input = r#"("origin.example.com"), origin.example.com/path origin.example.com?x=1 origin.example.com#frag"#;
        let expected = r#"("proxy.example.com"), proxy.example.com/path proxy.example.com?x=1 proxy.example.com#frag"#;

        assert_eq!(
            rewrite(input),
            Some(expected.to_string()),
            "should rewrite hosts followed by punctuation and URL separators"
        );
    }

    #[test]
    fn rewrites_multiple_occurrences() {
        assert_eq!(
            rewrite("origin.example.com origin.example.com/news origin.example.com?x=1"),
            Some("proxy.example.com proxy.example.com/news proxy.example.com?x=1".to_string()),
            "should rewrite every standalone occurrence"
        );
    }

    #[test]
    fn returns_none_for_empty_input_or_missing_origin() {
        assert_eq!(rewrite(""), None, "should ignore empty input");
        assert_eq!(
            rewrite("other.example.com/news"),
            None,
            "should ignore input without the origin host"
        );
        assert_eq!(
            rewrite_bare_host_at_boundaries("origin.example.com", "", REQUEST_HOST),
            None,
            "should ignore an empty origin host"
        );
        assert_eq!(
            rewrite_bare_host_at_boundaries("origin.example.com", ORIGIN_HOST, ""),
            None,
            "should ignore an empty request host"
        );
    }

    #[test]
    fn does_not_rewrite_larger_hostname_tokens() {
        let input = "cdn.origin.example.com notorigin.example.com origin.example.com.evil origin.example.com-extra origin.example.comextra";

        assert_eq!(
            rewrite(input),
            None,
            "should not rewrite subdomains, embedded words, or larger hostname tokens"
        );
    }

    #[test]
    fn rewrites_host_with_valid_numeric_port() {
        let input = "origin.example.com:8443/path origin.example.com:9443?x=1 origin.example.com:443#frag origin.example.com:8080";
        let expected = "proxy.example.com:8443/path proxy.example.com:9443?x=1 proxy.example.com:443#frag proxy.example.com:8080";

        assert_eq!(
            rewrite(input),
            Some(expected.to_string()),
            "should rewrite a standalone host while preserving a numeric port"
        );
    }

    #[test]
    fn does_not_rewrite_invalid_port_like_suffixes() {
        let input = "origin.example.com:not-a-port origin.example.com:8443evil origin.example.com:8443.evil origin.example.com:";

        assert_eq!(
            rewrite(input),
            None,
            "should not treat arbitrary colon suffixes as host boundaries"
        );
    }
}
