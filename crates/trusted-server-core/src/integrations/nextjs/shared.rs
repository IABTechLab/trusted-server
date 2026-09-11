//! Shared utilities for Next.js integration modules.

use std::borrow::Cow;
use std::cell::RefCell;

use std::sync::LazyLock;

use regex::Regex;

use crate::host_rewrite::rewrite_bare_host_at_boundaries;

// These are static code-defined literals, not config-derived patterns, so they
// intentionally remain lazy statics instead of participating in
// `Settings::prepare_runtime`.
/// RSC push script call pattern for extracting payload string boundaries.
///
/// The `self.`/`window.` receiver is required. A fragmented script keeps its
/// receiver out of the output stream via [`RSC_RECEIVER_CANDIDATES`] instead of
/// relaxing this pattern, because an unqualified `__next_f.push([1,"…"])`
/// cannot be distinguished from an unrelated publisher script that happens to
/// own a property of the same name.
pub(crate) static RSC_PUSH_CALL_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?s)(?:(?:self|window)\.__next_f\.push|(?:\(\s*)?(?:self|window)\.__next_f\s*=\s*(?:self|window)\.__next_f\s*\|\|\s*\[\]\s*\)\s*\.push)\(\[\s*1\s*,\s*(['"])"#,
    )
    .expect("valid RSC push call regex")
});

/// RSC push call pattern for a claim whose receiver already streamed.
///
/// Anchored to the start of the claimed fragment and only usable once the
/// receiver has been verified out of band by
/// [`receiver_context_is_flight_push`], so it cannot widen what an
/// unfragmented script is allowed to match.
pub(crate) static RSC_PUSH_CALL_PATTERN_TRIMMED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?s)^__next_f(?:\.push|\s*=\s*(?:self|window)\.__next_f\s*\|\|\s*\[\]\s*\)\s*\.push)\(\[\s*1\s*,\s*(['"])"#,
    )
    .expect("valid trimmed RSC push call regex")
});

/// Longest receiver context worth retaining: `(window.` plus one boundary byte.
pub(crate) const RSC_RECEIVER_CONTEXT_BYTES: usize = 9;

/// Whether text preceding a bare `__next_f` proves a Next.js Flight receiver.
///
/// `context` is the tail of the script text already released for the current
/// text node. An empty context is *not* accepted: a script that opens with an
/// unqualified `__next_f.push` is not something Next.js emits, and accepting it
/// would let any global of that name be rewritten.
pub(crate) fn receiver_context_is_flight_push(context: &str) -> bool {
    ["self.", "window."].iter().any(|receiver| {
        context.strip_suffix(receiver).is_some_and(|leading| {
            leading
                .as_bytes()
                .last()
                .is_none_or(|byte| !is_receiver_continuation(*byte))
        })
    })
}

/// Characters that would make a matched receiver the tail of a longer member
/// expression or identifier, as in `myself.__next_f` or `foo.window.__next_f`.
fn is_receiver_continuation(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$' | b'.')
}

/// Find the payload string boundaries within an RSC push script.
///
/// Returns `Some((start, end))` where `start` is the position after the opening quote
/// and `end` is the position of the closing quote.
pub(crate) fn find_rsc_push_payload_range(script: &str) -> Option<(usize, usize)> {
    let cap = RSC_PUSH_CALL_PATTERN.captures(script)?;
    let call = cap.get(0)?;
    // The receiver must stand alone: `myAnalytics.__next_f` and `foo.window.__next_f`
    // are unrelated member expressions, not Next.js Flight receivers.
    if call.start() > 0 && is_receiver_continuation(script.as_bytes()[call.start() - 1]) {
        return None;
    }
    payload_range_after_call(script, &cap)
}

/// Find the payload string boundaries of a claim whose receiver already streamed.
///
/// Callers must first prove the receiver with [`receiver_context_is_flight_push`];
/// `script` has to begin at the `__next_f` identifier.
pub(crate) fn find_trimmed_rsc_push_payload_range(script: &str) -> Option<(usize, usize)> {
    let cap = RSC_PUSH_CALL_PATTERN_TRIMMED.captures(script)?;
    payload_range_after_call(script, &cap)
}

fn payload_range_after_call(script: &str, cap: &regex::Captures<'_>) -> Option<(usize, usize)> {
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

    if matches!(suffix.as_bytes().first(), Some(b'/' | b'?' | b'#')) {
        return Some(suffix);
    }

    let port_and_rest = suffix.strip_prefix(':')?;
    let port_len = port_and_rest.bytes().take_while(u8::is_ascii_digit).count();
    if port_len == 0 {
        return None;
    }

    let rest = &port_and_rest[port_len..];
    (rest.is_empty() || matches!(rest.as_bytes().first(), Some(b'/' | b'?' | b'#')))
        .then_some(suffix)
}

// =============================================================================
// URL Rewriting
// =============================================================================

/// Compile a URL-matching regex anchored to a specific `origin_host`.
///
/// The slash alternatives cover all escape variants that appear in RSC payloads:
/// `\\\\//` (JSON double-encoded), `\\//` (JSON encoded), `\/\/` (escaped), `//` (plain).
///
/// Compiling per-request ensures only origin URLs are matched, avoiding the closure overhead
/// that a broad static pattern would incur for every URL-like token in the payload.
fn build_origin_url_pattern(origin_host: &str) -> Result<Regex, regex::Error> {
    let escaped = regex::escape(origin_host);
    Regex::new(&format!(
        r"(https?)?(:)?(\\\\\\\\\\\\\\\\//|\\\\\\\\//|\\/\\/|//)(?P<host>{escaped}(?::\d+)?)"
    ))
}

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
///
/// The compiled regex for the last-seen `origin_host` is cached so that
/// repeated calls within a single request (e.g. across multiple RSC payloads)
/// avoid recompiling the same pattern each time.
#[derive(Clone, Default)]
pub(crate) struct RscUrlRewriter {
    cached_pattern: RefCell<Option<(String, Regex)>>,
}

impl RscUrlRewriter {
    pub(crate) fn new() -> Self {
        Self::default()
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

        // Check cache first. Use the regex inside the borrow scope to avoid cloning it.
        // The same origin_host is used for every payload in a request, so this avoids
        // recompiling the pattern on each call. regex::escape() guarantees validity, but
        // treat compilation failure as non-fatal rather than panic.
        let cached = self.cached_pattern.borrow();
        if let Some((ref host, ref regex)) = *cached
            && host == origin_host
        {
            return Self::apply_rewrite(input, regex, origin_host, request_host, request_scheme);
        }
        drop(cached);

        // Cache miss: compile and store the pattern, then apply.
        match build_origin_url_pattern(origin_host) {
            Ok(pattern) => {
                let result =
                    Self::apply_rewrite(input, &pattern, origin_host, request_host, request_scheme);
                *self.cached_pattern.borrow_mut() = Some((origin_host.to_owned(), pattern));
                result
            }
            Err(e) => {
                log::error!("Failed to compile origin URL pattern: {e}");
                Cow::Borrowed(input)
            }
        }
    }

    fn apply_rewrite<'a>(
        input: &'a str,
        origin_pattern: &Regex,
        origin_host: &str,
        request_host: &str,
        request_scheme: &str,
    ) -> Cow<'a, str> {
        // Phase 1: Regex-based URL pattern rewriting (handles escaped slashes, schemes, etc.)
        let replaced = origin_pattern.replace_all(input, |caps: &regex::Captures<'_>| {
            let host = caps.name("host").map_or("", |m| m.as_str());
            let Some(host_suffix) = strip_origin_host_with_optional_port(host, origin_host) else {
                return caps
                    .get(0)
                    .expect("should capture the matched RSC URL")
                    .as_str()
                    .to_owned();
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
        let script = "self.__next_f.push([1,'hello world'])";
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

    #[test]
    fn find_rsc_push_payload_range_accepts_qualified_receivers() {
        for script in [
            r#"self.__next_f.push([1,"payload"])"#,
            r#"window.__next_f.push([1,"payload"])"#,
            r#";(self.__next_f=self.__next_f||[]).push([1,"payload"])"#,
        ] {
            let (start, end) = find_rsc_push_payload_range(script)
                .unwrap_or_else(|| panic!("should match qualified receiver in `{script}`"));
            assert_eq!(
                &script[start..end],
                "payload",
                "should capture the Flight payload of `{script}`"
            );
        }
    }

    #[test]
    fn find_rsc_push_payload_range_requires_a_qualified_receiver() {
        assert_eq!(
            find_rsc_push_payload_range(r#"__next_f.push([1,"payload"])"#),
            None,
            "should not claim an unqualified push whose receiver cannot be verified"
        );
    }

    #[test]
    fn find_rsc_push_payload_range_rejects_foreign_receivers() {
        for script in [
            r#"myAnalytics.__next_f.push([1,"https://origin.example.com/track"])"#,
            r#"foo.bar.__next_f.push([1,"payload"])"#,
            r#"window.myapp.__next_f.push([1,"payload"])"#,
            r#"a__next_f.push([1,"payload"])"#,
            r#"var x=1; other.__next_f.push([1,"payload"])"#,
            r#"foo.window.__next_f.push([1,"payload"])"#,
            r#"myself.__next_f.push([1,"payload"])"#,
            r#"(myself.__next_f=self.__next_f||[]).push([1,"payload"])"#,
        ] {
            assert_eq!(
                find_rsc_push_payload_range(script),
                None,
                "should not treat `{script}` as a Next.js Flight push"
            );
        }
    }
}
