use std::sync::Arc;

use once_cell::sync::Lazy;
use regex::Regex;

use crate::integrations::{IntegrationHtmlContext, IntegrationHtmlPostProcessor};

use super::rsc::rewrite_rsc_scripts_combined;
use super::{NextJsIntegrationConfig, NEXTJS_INTEGRATION_ID};

/// RSC push script pattern for HTML post-processing.
static RSC_SCRIPT_PATTERN: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"<script\b[^>]*>\s*self\.__next_f\.push\(\[\s*1\s*,\s*(['"])"#)
        .expect("valid RSC script regex")
});

/// RSC script ending pattern.
static RSC_SCRIPT_ENDING: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"^\s*\]\s*\)\s*;?\s*</script>"#).expect("valid RSC ending regex"));

pub(crate) struct NextJsHtmlPostProcessor {
    config: Arc<NextJsIntegrationConfig>,
}

impl NextJsHtmlPostProcessor {
    pub(crate) fn new(config: Arc<NextJsIntegrationConfig>) -> Self {
        Self { config }
    }
}

impl IntegrationHtmlPostProcessor for NextJsHtmlPostProcessor {
    fn integration_id(&self) -> &'static str {
        NEXTJS_INTEGRATION_ID
    }

    fn should_process(&self, html: &str, ctx: &IntegrationHtmlContext<'_>) -> bool {
        if !self.config.enabled || self.config.rewrite_attributes.is_empty() {
            return false;
        }

        html.contains("__next_f.push") && html.contains(ctx.origin_host)
    }

    fn post_process(&self, html: &mut String, ctx: &IntegrationHtmlContext<'_>) -> bool {
        if log::log_enabled!(log::Level::Debug) {
            let origin_before = html.matches(ctx.origin_host).count();
            log::debug!(
                "NextJs post-processor running: html_len={}, origin_matches={}, origin={}, proxy={}://{}",
                html.len(),
                origin_before,
                ctx.origin_host,
                ctx.request_scheme,
                ctx.request_host
            );
        }

        post_process_rsc_html_in_place(html, ctx.origin_host, ctx.request_host, ctx.request_scheme)
    }
}

#[derive(Debug, Clone, Copy)]
struct RscPushScriptRange {
    payload_start: usize,
    payload_end: usize,
}

fn find_rsc_push_scripts(html: &str) -> Vec<RscPushScriptRange> {
    let mut scripts = Vec::new();
    let mut search_pos = 0;

    while search_pos < html.len() {
        let Some(cap) = RSC_SCRIPT_PATTERN.captures(&html[search_pos..]) else {
            break;
        };

        let quote_match = cap.get(1).expect("script regex should capture quote");
        let quote = quote_match
            .as_str()
            .chars()
            .next()
            .expect("quote should exist");
        let payload_start = search_pos + quote_match.end();

        let mut i = payload_start;
        let bytes = html.as_bytes();
        while i < bytes.len() {
            if bytes[i] == b'\\' && i + 1 < bytes.len() {
                i += 2; // Skip escape sequence (safe: we checked i+1 exists)
            } else if bytes[i] == b'\\' {
                // Trailing backslash at end of content - malformed
                break;
            } else if bytes[i] == quote as u8 {
                break;
            } else {
                i += 1;
            }
        }

        if i >= bytes.len() || bytes[i] != quote as u8 {
            search_pos = payload_start;
            continue;
        }

        let after_quote = &html[i + 1..];
        let Some(ending_match) = RSC_SCRIPT_ENDING.find(after_quote) else {
            search_pos = payload_start;
            continue;
        };

        let payload_end = i;
        let script_end = i + 1 + ending_match.end();

        scripts.push(RscPushScriptRange {
            payload_start,
            payload_end,
        });

        search_pos = script_end;
    }

    scripts
}

pub fn post_process_rsc_html(
    html: &str,
    origin_host: &str,
    request_host: &str,
    request_scheme: &str,
) -> String {
    let mut result = html.to_string();
    post_process_rsc_html_in_place(&mut result, origin_host, request_host, request_scheme);
    result
}

pub fn post_process_rsc_html_in_place(
    html: &mut String,
    origin_host: &str,
    request_host: &str,
    request_scheme: &str,
) -> bool {
    let scripts = find_rsc_push_scripts(html.as_str());
    if scripts.is_empty() {
        return false;
    }

    let payloads: Vec<&str> = scripts
        .iter()
        .map(|s| &html[s.payload_start..s.payload_end])
        .collect();

    if !payloads.iter().any(|p| p.contains(origin_host)) {
        return false;
    }

    if log::log_enabled!(log::Level::Debug) {
        let origin_count_before: usize = payloads
            .iter()
            .map(|p| p.matches(origin_host).count())
            .sum();
        log::debug!(
            "post_process_rsc_html: {} scripts, {} origin URLs, origin={}, proxy={}://{}",
            payloads.len(),
            origin_count_before,
            origin_host,
            request_scheme,
            request_host
        );
    }

    let rewritten_payloads = rewrite_rsc_scripts_combined(
        payloads.as_slice(),
        origin_host,
        request_host,
        request_scheme,
    );

    let mut changed = false;
    for (i, original) in payloads.iter().enumerate() {
        if rewritten_payloads[i] != *original {
            changed = true;
            break;
        }
    }

    if !changed {
        return false;
    }

    for (i, script) in scripts.iter().enumerate().rev() {
        html.replace_range(
            script.payload_start..script.payload_end,
            &rewritten_payloads[i],
        );
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn post_process_rsc_html_rewrites_cross_script_tchunks() {
        let html = r#"<html><body>
<script>self.__next_f.push([1,"other:data\n1a:T40,partial content"])</script>
<script>self.__next_f.push([1," with https://origin.example.com/page goes here"])</script>
</body></html>"#;

        let result = post_process_rsc_html(html, "origin.example.com", "test.example.com", "https");

        assert!(
            result.contains("test.example.com/page"),
            "URL should be rewritten. Got: {}",
            result
        );
        assert!(
            result.contains(":T3c,"),
            "T-chunk length should be updated. Got: {}",
            result
        );
        assert!(result.contains("<html>") && result.contains("</html>"));
        assert!(result.contains("self.__next_f.push"));
    }

    #[test]
    fn post_process_rsc_html_handles_prettified_format() {
        let html = r#"<html><body>
    <script>
      self.__next_f.push([
        1,
        '445:{"ID":878799,"title":"News","url":"http://origin.example.com/news","target":""}'
      ]);
    </script>
    <script>
      self.__next_f.push([
        1,
        '446:{"url":"https://origin.example.com/reviews"}'
      ]);
    </script>
</body></html>"#;

        let result = post_process_rsc_html(html, "origin.example.com", "test.example.com", "https");

        assert!(
            result.contains("test.example.com/news"),
            "First URL should be rewritten. Got: {}",
            result
        );
        assert!(
            result.contains("test.example.com/reviews"),
            "Second URL should be rewritten. Got: {}",
            result
        );
        assert!(
            !result.contains("origin.example.com"),
            "No origin URLs should remain. Got: {}",
            result
        );
        assert!(result.contains("<html>") && result.contains("</html>"));
        assert!(result.contains("self.__next_f.push"));
    }

    #[test]
    fn post_process_rewrites_html_href_inside_tchunk() {
        let html = r#"<html><body>
    <script>
      self.__next_f.push([
        1,
        '53d:T4d9,\u003cdiv\u003e\u003ca href="https://origin.example.com/about-us"\u003eAbout\u003c/a\u003e\u003c/div\u003e'
      ]);
    </script>
</body></html>"#;

        let result = post_process_rsc_html(html, "origin.example.com", "test.example.com", "https");

        assert!(
            result.contains("test.example.com/about-us"),
            "HTML href URL in T-chunk should be rewritten. Got: {}",
            result
        );
        assert!(
            !result.contains("origin.example.com"),
            "No origin URLs should remain. Got: {}",
            result
        );
        assert!(
            !result.contains(":T4d9,"),
            "T-chunk length should have been recalculated (original was 4d9). Got: {}",
            result
        );
    }

    #[test]
    fn handles_nextjs_inlined_data_nonce_fixture() {
        // Fixture mirrors Next.js `createInlinedDataReadableStream` output:
        // `<script nonce="...">self.__next_f.push([1,"..."]) </script>`
        let html = include_str!("fixtures/inlined-data-nonce.html");
        let scripts = find_rsc_push_scripts(html);
        assert_eq!(scripts.len(), 1, "Should find exactly one RSC data script");

        let rewritten =
            post_process_rsc_html(html, "origin.example.com", "proxy.example.com", "https");
        assert!(
            rewritten.contains("https://proxy.example.com/news"),
            "Fixture URL should be rewritten. Got: {rewritten}"
        );
        assert!(
            !rewritten.contains("https://origin.example.com/news"),
            "Origin URL should be removed. Got: {rewritten}"
        );
    }

    #[test]
    fn handles_nextjs_inlined_data_html_escaping_fixture() {
        // Fixture includes `\\u003c` escapes, matching Next.js `htmlEscapeJsonString` behavior.
        let html = include_str!("fixtures/inlined-data-escaped.html");
        let scripts = find_rsc_push_scripts(html);
        assert_eq!(scripts.len(), 1, "Should find exactly one RSC data script");

        let rewritten =
            post_process_rsc_html(html, "origin.example.com", "proxy.example.com", "https");
        assert!(
            rewritten.contains("https://proxy.example.com/about"),
            "Escaped fixture URL should be rewritten. Got: {rewritten}"
        );
        assert!(
            rewritten.contains(r#"\\u003ca href=\\\"https://proxy.example.com/about\\\""#),
            "Escaped HTML should remain escaped and rewritten. Got: {rewritten}"
        );
        assert!(
            !rewritten.contains("https://origin.example.com/about"),
            "Origin URL should be removed. Got: {rewritten}"
        );
    }

    #[test]
    fn handles_trailing_backslash_gracefully() {
        // Malformed content with trailing backslash should not panic
        let html = r#"<html><body>
<script>self.__next_f.push([1,"content with trailing backslash\"])</script>
<script>self.__next_f.push([1,"valid https://origin.example.com/page"])</script>
</body></html>"#;

        let scripts = find_rsc_push_scripts(html);
        // The first script is malformed (trailing backslash escapes the quote),
        // so it won't be detected as valid. The second one should be found.
        assert!(
            !scripts.is_empty(),
            "Should find at least the valid script. Found: {}",
            scripts.len()
        );

        // Should not panic during processing
        let result = post_process_rsc_html(html, "origin.example.com", "test.example.com", "https");
        assert!(
            result.contains("test.example.com") || result.contains("origin.example.com"),
            "Processing should complete without panic"
        );
    }

    #[test]
    fn handles_unterminated_string_gracefully() {
        // Content where string never closes - should not hang or panic
        let html = r#"<html><body>
<script>self.__next_f.push([1,"content without closing quote
</body></html>"#;

        let scripts = find_rsc_push_scripts(html);
        assert_eq!(
            scripts.len(),
            0,
            "Should not find scripts with unterminated strings"
        );
    }

    #[test]
    fn no_origin_returns_unchanged() {
        let html = r#"<html><body>
<script>self.__next_f.push([1,"content without origin URLs"])</script>
</body></html>"#;

        let result = post_process_rsc_html(html, "origin.example.com", "test.example.com", "https");
        assert_eq!(result, html, "HTML without origin should be unchanged");
    }
}
