//! Shared utilities for Next.js integration modules.

use std::borrow::Cow;

use std::sync::LazyLock;

use regex::Regex;

use crate::host_rewrite::rewrite_bare_host_at_boundaries;

// These are static code-defined literals, not config-derived patterns, so they
// intentionally remain lazy statics instead of participating in
// `Settings::prepare_runtime`.
/// RSC push script call pattern for extracting payload string boundaries.
pub(crate) static RSC_PUSH_CALL_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?s)(?:(?:self|window)\.__next_f\.push|\(\s*(?:self|window)\.__next_f\s*=\s*(?:self|window)\.__next_f\s*\|\|\s*\[\]\s*\)\s*\.push)\(\[\s*1\s*,\s*(['"])"#,
    )
    .expect("valid RSC push call regex")
});

/// Generic URL pattern for RSC payload rewriting.
pub(crate) static RSC_URL_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(https?)?(:)?(\\\\\\\\\\\\\\\\//|\\\\\\\\//|\\/\\/|//)(?P<host>\[[^\]]+\](?::\d+)?|[^/"'\\\s?#<>,)]+)"#,
    )
    .expect("valid RSC URL rewrite regex")
});

/// Find the payload string boundaries within an RSC push script.
///
/// Returns `Some((start, end))` where `start` is the position after the opening quote
/// and `end` is the position of the closing quote.
pub(crate) fn find_rsc_push_payload_range(script: &str) -> Option<(usize, usize)> {
    let cap = RSC_PUSH_CALL_PATTERN.captures(script)?;
    let quote_match = cap.get(1)?;
    let quote = quote_match
        .as_str()
        .chars()
        .next()
        .expect("push call regex should capture a quote character");
    let payload_start = quote_match.end();

    let bytes = script.as_bytes();
    let mut i = payload_start;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            i += 2;
        } else if bytes[i] == b'\\' {
            return None;
        } else if bytes[i] == quote as u8 {
            return Some((payload_start, i));
        } else {
            i += 1;
        }
    }

    None
}

/// Strip an origin host from the start of an authority/value while preserving an
/// optional explicit port and requiring a safe hostname boundary.
///
/// This helper currently matches hostname-style origins only. Bracketed IPv6
/// authorities are not normalized here, so IPv6 origin rewriting remains
/// unsupported until the caller can provide a bracketed authority consistently.
///
/// Examples:
/// - `origin.example.com/path` -> `Some("/path")`
/// - `origin.example.com:8443/path` -> `Some(":8443/path")`
/// - `origin.example.com.evil/path` -> `None`
pub(crate) fn strip_origin_host_with_optional_port<'a>(
    value: &'a str,
    origin_host: &str,
) -> Option<&'a str> {
    let suffix = value.strip_prefix(origin_host)?;
    if suffix.is_empty() {
        return Some(suffix);
    }

    if matches!(
        suffix.as_bytes().first(),
        Some(b'/') | Some(b'?') | Some(b'#')
    ) {
        return Some(suffix);
    }

    let port_and_rest = suffix.strip_prefix(':')?;
    let port_len = port_and_rest.bytes().take_while(u8::is_ascii_digit).count();
    if port_len == 0 {
        return None;
    }

    let rest = &port_and_rest[port_len..];
    if rest.is_empty()
        || matches!(
            rest.as_bytes().first(),
            Some(b'/') | Some(b'?') | Some(b'#')
        )
    {
        Some(suffix)
    } else {
        None
    }
}

// =============================================================================
// URL Rewriting
// =============================================================================

/// Rewriter for URL patterns in RSC payloads.
///
/// This rewrites all occurrences of origin URLs in content, including:
/// - Full URLs: `https://origin.example.com/path` or `http://origin.example.com/path`
/// - Protocol-relative: `//origin.example.com/path`
/// - Escaped variants: `\/\/origin.example.com` (JSON-escaped)
/// - Bare hostnames: `origin.example.com` (as JSON values)
///
/// Use this for RSC T-chunk content where any origin URL should be rewritten.
/// For attribute-specific rewriting (e.g., only rewrite `"href"` values), use
/// the `UrlRewriter` in `script_rewriter.rs` instead.
#[derive(Clone, Default)]
pub(crate) struct RscUrlRewriter;

impl RscUrlRewriter {
    pub(crate) fn new() -> Self {
        Self
    }

    pub(crate) fn rewrite<'a>(
        &self,
        input: &'a str,
        origin_host: &str,
        request_host: &str,
        request_scheme: &str,
    ) -> Cow<'a, str> {
        if origin_host.is_empty() || !input.contains(origin_host) {
            return Cow::Borrowed(input);
        }

        // Phase 1: Regex-based URL pattern rewriting (handles escaped slashes, schemes, etc.)
        let replaced = RSC_URL_PATTERN.replace_all(input, |caps: &regex::Captures<'_>| {
            let host = caps.name("host").map_or("", |m| m.as_str());
            let Some(host_suffix) = strip_origin_host_with_optional_port(host, origin_host) else {
                return caps
                    .get(0)
                    .expect("should capture the matched RSC URL")
                    .as_str()
                    .to_string();
            };

            let slashes = caps.get(3).map_or("//", |m| m.as_str());
            if caps.get(1).is_some() {
                format!("{request_scheme}:{slashes}{request_host}{host_suffix}")
            } else {
                format!("{slashes}{request_host}{host_suffix}")
            }
        });

        // Phase 2: Handle bare host occurrences not matched by the URL regex
        // (e.g., `siteProductionDomain`). Only check if regex made no changes,
        // because if it did, we already know origin_host was present.
        let text = match &replaced {
            Cow::Borrowed(s) => *s,
            Cow::Owned(s) => s.as_str(),
        };

        if !text.contains(origin_host) {
            return replaced;
        }

        rewrite_bare_host_at_boundaries(text, origin_host, request_host)
            .map(Cow::Owned)
            .unwrap_or(replaced)
    }

    pub(crate) fn rewrite_to_string(
        &self,
        input: &str,
        origin_host: &str,
        request_host: &str,
        request_scheme: &str,
    ) -> String {
        self.rewrite(input, origin_host, request_host, request_scheme)
            .into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_double_quoted_payload() {
        let script = r#"self.__next_f.push([1,"hello world"])"#;
        let (start, end) = find_rsc_push_payload_range(script).expect("should find payload");
        assert_eq!(&script[start..end], "hello world");
    }

    #[test]
    fn finds_single_quoted_payload() {
        let script = r#"self.__next_f.push([1,'hello world'])"#;
        let (start, end) = find_rsc_push_payload_range(script).expect("should find payload");
        assert_eq!(&script[start..end], "hello world");
    }

    #[test]
    fn finds_assignment_form() {
        let script = r#"(self.__next_f=self.__next_f||[]).push([1,"payload"])"#;
        let (start, end) = find_rsc_push_payload_range(script).expect("should find payload");
        assert_eq!(&script[start..end], "payload");
    }

    #[test]
    fn returns_none_for_trailing_backslash() {
        let script = r#"self.__next_f.push([1,"incomplete\"])"#;
        assert!(find_rsc_push_payload_range(script).is_none());
    }

    #[test]
    fn returns_none_for_unterminated_string() {
        let script = r#"self.__next_f.push([1,"no closing quote"#;
        assert!(find_rsc_push_payload_range(script).is_none());
    }

    // RscUrlRewriter tests

    #[test]
    fn rsc_url_rewriter_rewrites_https_url() {
        let rewriter = RscUrlRewriter::new();
        let input = r#"{"url":"https://origin.example.com/path"}"#;
        let result = rewriter.rewrite(input, "origin.example.com", "proxy.example.com", "https");
        assert_eq!(result, r#"{"url":"https://proxy.example.com/path"}"#);
    }

    #[test]
    fn rsc_url_rewriter_rewrites_http_url() {
        let rewriter = RscUrlRewriter::new();
        let input = r#"{"url":"http://origin.example.com/path"}"#;
        let result = rewriter.rewrite(input, "origin.example.com", "proxy.example.com", "http");
        assert_eq!(result, r#"{"url":"http://proxy.example.com/path"}"#);
    }

    #[test]
    fn rsc_url_rewriter_rewrites_protocol_relative_url() {
        let rewriter = RscUrlRewriter::new();
        let input = r#"{"url":"//origin.example.com/path"}"#;
        let result = rewriter.rewrite(input, "origin.example.com", "proxy.example.com", "https");
        assert_eq!(result, r#"{"url":"//proxy.example.com/path"}"#);
    }

    #[test]
    fn rsc_url_rewriter_rewrites_escaped_slashes() {
        let rewriter = RscUrlRewriter::new();
        let input = r#"{"url":"\/\/origin.example.com/path"}"#;
        let result = rewriter.rewrite(input, "origin.example.com", "proxy.example.com", "https");
        assert_eq!(result, r#"{"url":"\/\/proxy.example.com/path"}"#);
    }

    #[test]
    fn rsc_url_rewriter_rewrites_bare_host() {
        let rewriter = RscUrlRewriter::new();
        let input = r#"{"siteProductionDomain":"origin.example.com"}"#;
        let result = rewriter.rewrite(input, "origin.example.com", "proxy.example.com", "https");
        assert_eq!(result, r#"{"siteProductionDomain":"proxy.example.com"}"#);
    }

    #[test]
    fn rsc_url_rewriter_rewrites_explicit_port_urls() {
        let rewriter = RscUrlRewriter::new();
        let input = r#"{"url":"https://origin.example.com:8443/path","asset":"//origin.example.com:9443/file.js"}"#;
        let result = rewriter.rewrite(input, "origin.example.com", "proxy.example.com", "https");
        assert_eq!(
            result,
            r#"{"url":"https://proxy.example.com:8443/path","asset":"//proxy.example.com:9443/file.js"}"#
        );
    }

    #[test]
    fn rsc_url_rewriter_does_not_rewrite_partial_hostname() {
        let rewriter = RscUrlRewriter::new();
        let input = r#"{"domain":"subexample.com"}"#;
        let result = rewriter.rewrite(input, "example.com", "proxy.example.com", "https");
        // Should not rewrite because "example.com" is not a standalone host here
        assert_eq!(result, r#"{"domain":"subexample.com"}"#);
    }

    #[test]
    fn rsc_url_rewriter_no_change_when_origin_not_present() {
        let rewriter = RscUrlRewriter::new();
        let input = r#"{"url":"https://other.example.com/path"}"#;
        let result = rewriter.rewrite(input, "origin.example.com", "proxy.example.com", "https");
        // Should return borrowed reference (no allocation)
        assert!(matches!(result, Cow::Borrowed(_)));
        assert_eq!(result, input);
    }

    #[test]
    fn rsc_url_rewriter_supports_regex_metacharacters_in_origin_literal() {
        let rewriter = RscUrlRewriter::new();
        let input = r#"{"url":"https://origin.(example).com/path"}"#;
        let result = rewriter.rewrite(input, "origin.(example).com", "proxy.example.com", "https");
        assert_eq!(result, r#"{"url":"https://proxy.example.com/path"}"#);
    }

    #[test]
    fn strip_origin_host_with_optional_port_enforces_boundaries() {
        assert_eq!(
            strip_origin_host_with_optional_port(
                "origin.example.com:8443/path",
                "origin.example.com"
            ),
            Some(":8443/path")
        );
        assert_eq!(
            strip_origin_host_with_optional_port("origin.example.com/path", "origin.example.com"),
            Some("/path")
        );
        assert_eq!(
            strip_origin_host_with_optional_port(
                "origin.example.com.evil/path",
                "origin.example.com"
            ),
            None
        );
        assert_eq!(
            strip_origin_host_with_optional_port(
                "origin.example.com:not-a-port/path",
                "origin.example.com"
            ),
            None
        );
    }

    #[test]
    fn strip_origin_host_with_optional_port_does_not_match_bracketed_ipv6_authority() {
        assert_eq!(
            strip_origin_host_with_optional_port("[2001:db8::1]/path", "2001:db8::1"),
            None
        );
    }
}
