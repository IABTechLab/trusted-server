//! Shared utilities for Next.js integration modules.

use std::borrow::Cow;

use once_cell::sync::Lazy;
use regex::{escape, Regex};

use crate::host_rewrite::rewrite_bare_host_at_boundaries;

/// RSC push script call pattern for extracting payload string boundaries.
pub(crate) static RSC_PUSH_CALL_PATTERN: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r#"(?s)(?:(?:self|window)\.__next_f\.push|\(\s*(?:self|window)\.__next_f\s*=\s*(?:self|window)\.__next_f\s*\|\|\s*\[\]\s*\)\s*\.push)\(\[\s*1\s*,\s*(['"])"#,
    )
    .expect("valid RSC push call regex")
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
pub(crate) struct RscUrlRewriter {
    origin_host: String,
    request_host: String,
    request_scheme: String,
    pattern: Regex,
}

impl RscUrlRewriter {
    pub(crate) fn new(origin_host: &str, request_host: &str, request_scheme: &str) -> Self {
        let escaped_origin = escape(origin_host);

        // Match:
        // - https://origin_host or http://origin_host
        // - //origin_host (protocol-relative)
        // - escaped variants inside JSON-in-JS strings (e.g., \/\/origin_host)
        let pattern = Regex::new(&format!(
            r#"(https?)?(:)?(\\\\\\\\\\\\\\\\//|\\\\\\\\//|\\/\\/|//){}"#,
            escaped_origin
        ))
        .expect("valid RSC URL rewrite regex");

        Self {
            origin_host: origin_host.to_string(),
            request_host: request_host.to_string(),
            request_scheme: request_scheme.to_string(),
            pattern,
        }
    }

    pub(crate) fn rewrite<'a>(&self, input: &'a str) -> Cow<'a, str> {
        if !input.contains(&self.origin_host) {
            return Cow::Borrowed(input);
        }

        // Phase 1: Regex-based URL pattern rewriting (handles escaped slashes, schemes, etc.)
        let replaced = self
            .pattern
            .replace_all(input, |caps: &regex::Captures<'_>| {
                let slashes = caps.get(3).map_or("//", |m| m.as_str());
                if caps.get(1).is_some() {
                    format!("{}:{}{}", self.request_scheme, slashes, self.request_host)
                } else {
                    format!("{}{}", slashes, self.request_host)
                }
            });

        // Phase 2: Handle bare host occurrences not matched by the URL regex
        // (e.g., `siteProductionDomain`). Only check if regex made no changes,
        // because if it did, we already know origin_host was present.
        let text = match &replaced {
            Cow::Borrowed(s) => *s,
            Cow::Owned(s) => s.as_str(),
        };

        if !text.contains(&self.origin_host) {
            return replaced;
        }

        rewrite_bare_host_at_boundaries(text, &self.origin_host, &self.request_host)
            .map(Cow::Owned)
            .unwrap_or(replaced)
    }

    pub(crate) fn rewrite_to_string(&self, input: &str) -> String {
        self.rewrite(input).into_owned()
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
        let rewriter = RscUrlRewriter::new("origin.example.com", "proxy.example.com", "https");
        let input = r#"{"url":"https://origin.example.com/path"}"#;
        let result = rewriter.rewrite(input);
        assert_eq!(result, r#"{"url":"https://proxy.example.com/path"}"#);
    }

    #[test]
    fn rsc_url_rewriter_rewrites_http_url() {
        let rewriter = RscUrlRewriter::new("origin.example.com", "proxy.example.com", "http");
        let input = r#"{"url":"http://origin.example.com/path"}"#;
        let result = rewriter.rewrite(input);
        assert_eq!(result, r#"{"url":"http://proxy.example.com/path"}"#);
    }

    #[test]
    fn rsc_url_rewriter_rewrites_protocol_relative_url() {
        let rewriter = RscUrlRewriter::new("origin.example.com", "proxy.example.com", "https");
        let input = r#"{"url":"//origin.example.com/path"}"#;
        let result = rewriter.rewrite(input);
        assert_eq!(result, r#"{"url":"//proxy.example.com/path"}"#);
    }

    #[test]
    fn rsc_url_rewriter_rewrites_escaped_slashes() {
        let rewriter = RscUrlRewriter::new("origin.example.com", "proxy.example.com", "https");
        let input = r#"{"url":"\/\/origin.example.com/path"}"#;
        let result = rewriter.rewrite(input);
        assert_eq!(result, r#"{"url":"\/\/proxy.example.com/path"}"#);
    }

    #[test]
    fn rsc_url_rewriter_rewrites_bare_host() {
        let rewriter = RscUrlRewriter::new("origin.example.com", "proxy.example.com", "https");
        let input = r#"{"siteProductionDomain":"origin.example.com"}"#;
        let result = rewriter.rewrite(input);
        assert_eq!(result, r#"{"siteProductionDomain":"proxy.example.com"}"#);
    }

    #[test]
    fn rsc_url_rewriter_does_not_rewrite_partial_hostname() {
        let rewriter = RscUrlRewriter::new("example.com", "proxy.example.com", "https");
        let input = r#"{"domain":"subexample.com"}"#;
        let result = rewriter.rewrite(input);
        // Should not rewrite because "example.com" is not a standalone host here
        assert_eq!(result, r#"{"domain":"subexample.com"}"#);
    }

    #[test]
    fn rsc_url_rewriter_no_change_when_origin_not_present() {
        let rewriter = RscUrlRewriter::new("origin.example.com", "proxy.example.com", "https");
        let input = r#"{"url":"https://other.example.com/path"}"#;
        let result = rewriter.rewrite(input);
        // Should return borrowed reference (no allocation)
        assert!(matches!(result, Cow::Borrowed(_)));
        assert_eq!(result, input);
    }
}
