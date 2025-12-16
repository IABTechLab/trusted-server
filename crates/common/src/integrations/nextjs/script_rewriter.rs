use std::sync::Arc;

use once_cell::sync::Lazy;
use regex::{escape, Regex};

use crate::integrations::{
    IntegrationScriptContext, IntegrationScriptRewriter, ScriptRewriteAction,
};

use super::rsc::{rewrite_rsc_tchunks_with_rewriter, RscUrlRewriter};
use super::{NextJsIntegrationConfig, NEXTJS_INTEGRATION_ID};

/// RSC push payload pattern for extraction.
static RSC_PUSH_PATTERN: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"self\.__next_f\.push\(\[\s*1\s*,\s*(['"])"#).expect("valid RSC push regex")
});

#[derive(Clone, Copy)]
pub(super) enum NextJsRewriteMode {
    Structured,
    Streamed,
}

pub(super) struct NextJsScriptRewriter {
    config: Arc<NextJsIntegrationConfig>,
    mode: NextJsRewriteMode,
}

impl NextJsScriptRewriter {
    pub(super) fn new(config: Arc<NextJsIntegrationConfig>, mode: NextJsRewriteMode) -> Self {
        Self { config, mode }
    }

    fn rewrite_structured(
        &self,
        content: &str,
        ctx: &IntegrationScriptContext<'_>,
    ) -> ScriptRewriteAction {
        if ctx.origin_host.is_empty()
            || ctx.request_host.is_empty()
            || self.config.rewrite_attributes.is_empty()
        {
            return ScriptRewriteAction::keep();
        }

        let rewriter = UrlRewriter::new(
            ctx.origin_host,
            ctx.request_host,
            ctx.request_scheme,
            &self.config.rewrite_attributes,
            false, // preserve_length not used for structured payloads
        );

        if let Some(rewritten) = rewrite_nextjs_values_with_rewriter(content, &rewriter) {
            ScriptRewriteAction::replace(rewritten)
        } else {
            ScriptRewriteAction::keep()
        }
    }

    fn rewrite_streamed(
        &self,
        content: &str,
        ctx: &IntegrationScriptContext<'_>,
    ) -> ScriptRewriteAction {
        let rsc_rewriter =
            RscUrlRewriter::new(ctx.origin_host, ctx.request_host, ctx.request_scheme);

        if let Some((payload, quote, start, end)) = extract_rsc_push_payload(content) {
            let rewritten_payload = rewrite_rsc_tchunks_with_rewriter(payload, &rsc_rewriter);

            if rewritten_payload != payload {
                let mut result = String::with_capacity(content.len());
                result.push_str(&content[..start]);
                result.push(quote);
                result.push_str(&rewritten_payload);
                result.push(quote);
                result.push_str(&content[end + 1..]);
                return ScriptRewriteAction::replace(result);
            }
        }

        let rewritten = rsc_rewriter.rewrite_to_string(content);
        if rewritten != content {
            return ScriptRewriteAction::replace(rewritten);
        }

        ScriptRewriteAction::keep()
    }
}

/// Extract RSC payload from a self.__next_f.push([1, '...']) call.
/// Returns (payload_content, quote_char, start_pos, end_pos).
fn extract_rsc_push_payload(content: &str) -> Option<(&str, char, usize, usize)> {
    let cap = RSC_PUSH_PATTERN.captures(content)?;
    let quote_match = cap.get(1)?;
    let quote = quote_match.as_str().chars().next()?;
    let content_start = quote_match.end();

    let search_from = &content[content_start..];
    let mut pos = 0;
    let mut escape = false;

    for c in search_from.chars() {
        if escape {
            escape = false;
            pos += c.len_utf8();
            continue;
        }
        if c == '\\' {
            escape = true;
            pos += 1;
            continue;
        }
        if c == quote {
            let content_end = content_start + pos;
            return Some((
                &content[content_start..content_end],
                quote,
                content_start - 1,
                content_end,
            ));
        }
        pos += c.len_utf8();
    }

    None
}

impl IntegrationScriptRewriter for NextJsScriptRewriter {
    fn integration_id(&self) -> &'static str {
        NEXTJS_INTEGRATION_ID
    }

    fn selector(&self) -> &'static str {
        match self.mode {
            NextJsRewriteMode::Structured => "script#__NEXT_DATA__",
            NextJsRewriteMode::Streamed => "script",
        }
    }

    fn rewrite(&self, content: &str, ctx: &IntegrationScriptContext<'_>) -> ScriptRewriteAction {
        if self.config.rewrite_attributes.is_empty() {
            return ScriptRewriteAction::keep();
        }

        match self.mode {
            NextJsRewriteMode::Structured => self.rewrite_structured(content, ctx),
            NextJsRewriteMode::Streamed => {
                if content.contains("__next_f.push") {
                    return ScriptRewriteAction::keep();
                }
                if content.contains("__next_f") {
                    return self.rewrite_streamed(content, ctx);
                }
                ScriptRewriteAction::keep()
            }
        }
    }
}

fn rewrite_nextjs_values_with_rewriter(content: &str, rewriter: &UrlRewriter) -> Option<String> {
    rewriter.rewrite_embedded(content)
}

#[cfg(test)]
fn rewrite_nextjs_values(
    content: &str,
    origin_host: &str,
    request_host: &str,
    request_scheme: &str,
    attributes: &[String],
    preserve_length: bool,
) -> Option<String> {
    if origin_host.is_empty() || request_host.is_empty() || attributes.is_empty() {
        return None;
    }

    let rewriter = UrlRewriter::new(
        origin_host,
        request_host,
        request_scheme,
        attributes,
        preserve_length,
    );

    rewrite_nextjs_values_with_rewriter(content, &rewriter)
}

/// Rewrites URLs in structured Next.js JSON payloads (e.g., `__NEXT_DATA__`).
///
/// This rewriter uses attribute-specific regex patterns to find and replace URLs
/// in JSON content. It handles full URLs, protocol-relative URLs, and bare hostnames.
///
/// The `preserve_length` option adds whitespace padding to maintain byte length,
/// which was an early attempt at RSC compatibility. This is no longer needed for
/// RSC payloads (T-chunk lengths are recalculated instead), but is kept for
/// potential future use cases where length preservation is required.
struct UrlRewriter {
    origin_host: String,
    request_host: String,
    request_scheme: String,
    embedded_patterns: Vec<Regex>,
    bare_host_patterns: Vec<Regex>,
    /// When true, adds whitespace padding to maintain original byte length.
    /// Currently unused in production (always false).
    preserve_length: bool,
}

impl UrlRewriter {
    fn new(
        origin_host: &str,
        request_host: &str,
        request_scheme: &str,
        attributes: &[String],
        preserve_length: bool,
    ) -> Self {
        let escaped_origin = escape(origin_host);

        let embedded_patterns = attributes
            .iter()
            .map(|attr| {
                let escaped_attr = escape(attr);
                let pattern = format!(
                    r#"(?P<prefix>(?:\\*")?{attr}(?:\\*")?:\\*")(?P<scheme>https?://|//){origin}(?P<path>[^"\\]*)(?P<quote>\\*")"#,
                    attr = escaped_attr,
                    origin = escaped_origin,
                );
                Regex::new(&pattern).expect("valid Next.js rewrite regex")
            })
            .collect();

        let bare_host_patterns = attributes
            .iter()
            .map(|attr| {
                let escaped_attr = escape(attr);
                let pattern = format!(
                    r#"(?P<prefix>(?:\\*")?{attr}(?:\\*")?:\\*"){origin}(?P<suffix>\\*")"#,
                    attr = escaped_attr,
                    origin = escaped_origin,
                );
                Regex::new(&pattern).expect("valid Next.js bare host rewrite regex")
            })
            .collect();

        Self {
            origin_host: origin_host.to_string(),
            request_host: request_host.to_string(),
            request_scheme: request_scheme.to_string(),
            embedded_patterns,
            bare_host_patterns,
            preserve_length,
        }
    }

    #[cfg(test)]
    fn rewrite_url_value(&self, url: &str) -> Option<(String, String)> {
        let original_len = url.len();

        let new_url = if let Some(rest) = url.strip_prefix("https://") {
            if rest.starts_with(&self.origin_host) {
                let path = &rest[self.origin_host.len()..];
                Some(format!(
                    "{}://{}{}",
                    self.request_scheme, self.request_host, path
                ))
            } else {
                None
            }
        } else if let Some(rest) = url.strip_prefix("http://") {
            if rest.starts_with(&self.origin_host) {
                let path = &rest[self.origin_host.len()..];
                Some(format!(
                    "{}://{}{}",
                    self.request_scheme, self.request_host, path
                ))
            } else {
                None
            }
        } else if let Some(rest) = url.strip_prefix("//") {
            if rest.starts_with(&self.origin_host) {
                let path = &rest[self.origin_host.len()..];
                Some(format!("//{}{}", self.request_host, path))
            } else {
                None
            }
        } else if url == self.origin_host {
            Some(self.request_host.clone())
        } else if url.starts_with(&self.origin_host) {
            let path = &url[self.origin_host.len()..];
            Some(format!("{}{}", self.request_host, path))
        } else {
            None
        };

        new_url.map(|url| {
            let padding = if self.preserve_length {
                Self::calculate_padding(url.len(), original_len)
            } else {
                String::new()
            };
            (url, padding)
        })
    }

    #[cfg(test)]
    fn calculate_padding(new_url_len: usize, original_len: usize) -> String {
        if new_url_len >= original_len {
            String::new()
        } else {
            " ".repeat(original_len - new_url_len)
        }
    }

    fn rewrite_embedded(&self, input: &str) -> Option<String> {
        let mut result = input.to_string();
        let mut changed = false;

        for regex in &self.embedded_patterns {
            let origin_host = &self.origin_host;
            let request_host = &self.request_host;
            let request_scheme = &self.request_scheme;
            let preserve_length = self.preserve_length;

            let next_value = regex.replace_all(&result, |caps: &regex::Captures<'_>| {
                let prefix = &caps["prefix"];
                let scheme = &caps["scheme"];
                let path = &caps["path"];
                let quote = &caps["quote"];

                let original_url_len = scheme.len() + origin_host.len() + path.len();

                let new_url = if scheme == "//" {
                    format!("//{}{}", request_host, path)
                } else {
                    format!("{}://{}{}", request_scheme, request_host, path)
                };

                let padding = if preserve_length && new_url.len() < original_url_len {
                    " ".repeat(original_url_len - new_url.len())
                } else {
                    String::new()
                };

                format!("{prefix}{new_url}{quote}{padding}")
            });

            if next_value != result {
                changed = true;
                result = next_value.into_owned();
            }
        }

        for regex in &self.bare_host_patterns {
            let origin_host = &self.origin_host;
            let request_host = &self.request_host;
            let preserve_length = self.preserve_length;

            let next_value = regex.replace_all(&result, |caps: &regex::Captures<'_>| {
                let prefix = &caps["prefix"];
                let suffix = &caps["suffix"];

                let padding = if preserve_length && request_host.len() < origin_host.len() {
                    " ".repeat(origin_host.len() - request_host.len())
                } else {
                    String::new()
                };

                format!("{prefix}{request_host}{suffix}{padding}")
            });

            if next_value != result {
                changed = true;
                result = next_value.into_owned();
            }
        }

        changed.then_some(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integrations::ScriptRewriteAction;

    fn test_config() -> Arc<NextJsIntegrationConfig> {
        Arc::new(NextJsIntegrationConfig {
            enabled: true,
            rewrite_attributes: vec!["href".into(), "link".into(), "url".into()],
        })
    }

    fn ctx(selector: &'static str) -> IntegrationScriptContext<'static> {
        IntegrationScriptContext {
            selector,
            request_host: "ts.example.com",
            request_scheme: "https",
            origin_host: "origin.example.com",
        }
    }

    #[test]
    fn structured_rewriter_updates_next_data_payload() {
        let payload = r#"{"props":{"pageProps":{"primary":{"href":"https://origin.example.com/reviews"},"secondary":{"href":"http://origin.example.com/sign-in"},"fallbackHref":"http://origin.example.com/legacy","protoRelative":"//origin.example.com/assets/logo.png"}}}"#;
        let rewriter = NextJsScriptRewriter::new(test_config(), NextJsRewriteMode::Structured);
        let result = rewriter.rewrite(payload, &ctx("script#__NEXT_DATA__"));

        match result {
            ScriptRewriteAction::Replace(value) => {
                assert!(value.contains("ts.example.com") && value.contains("/reviews"));
                assert!(value.contains("ts.example.com") && value.contains("/sign-in"));
                assert!(value.contains(r#""fallbackHref":"http://origin.example.com/legacy""#));
                assert!(value.contains(r#""protoRelative":"//origin.example.com/assets/logo.png""#));
            }
            _ => panic!("Expected rewrite to update payload"),
        }
    }

    #[test]
    fn streamed_rewriter_skips_non_next_payloads() {
        let rewriter = NextJsScriptRewriter::new(test_config(), NextJsRewriteMode::Streamed);

        let noop = rewriter.rewrite("console.log('hello');", &ctx("script"));
        assert!(matches!(noop, ScriptRewriteAction::Keep));

        let payload =
            r#"self.__next_f.push([1, "{\"href\":\"https://origin.example.com/app\"}"]);"#;
        let result = rewriter.rewrite(payload, &ctx("script"));
        assert!(
            matches!(result, ScriptRewriteAction::Keep),
            "Streamed rewriter should skip __next_f.push payloads (handled by post-processor)"
        );

        let init_script = r#"(self.__next_f = self.__next_f || []).push([0]); var url = "https://origin.example.com/api";"#;
        let init_result = rewriter.rewrite(init_script, &ctx("script"));
        assert!(
            matches!(
                init_result,
                ScriptRewriteAction::Keep | ScriptRewriteAction::Replace(_)
            ),
            "Streamed rewriter should handle non-push __next_f scripts"
        );
    }

    #[test]
    fn rewrite_helper_handles_protocol_relative_urls() {
        let content = r#"{"props":{"pageProps":{"link":"//origin.example.com/image.png"}}}"#;
        let rewritten = rewrite_nextjs_values(
            content,
            "origin.example.com",
            "ts.example.com",
            "https",
            &["link".into()],
            false,
        )
        .expect("should rewrite protocol relative link");

        assert!(rewritten.contains("ts.example.com") && rewritten.contains("/image.png"));
    }

    #[test]
    fn truncated_string_without_urls_is_not_modified() {
        let truncated = r#"self.__next_f.push([
  1,
  '430:I[6061,["749","static/chunks/16bf9003-553c36acd7d8a04b.js","4669","static/chun'
]);"#;

        let result = rewrite_nextjs_values(
            truncated,
            "origin.example.com",
            "proxy.example.com",
            "http",
            &["url".into()],
            true,
        );

        assert!(
            result.is_none(),
            "Truncated content without URLs should not be modified"
        );
    }

    #[test]
    fn complete_string_with_url_is_rewritten() {
        let complete = r#"self.__next_f.push([
  1,
  '{"url":"https://origin.example.com/path/to/resource"}'
]);"#;

        let result = rewrite_nextjs_values(
            complete,
            "origin.example.com",
            "proxy.example.com",
            "http",
            &["url".into()],
            true,
        )
        .expect("should rewrite URL");

        assert!(
            result.contains("proxy.example.com") && result.contains("/path/to/resource"),
            "Complete URL should be rewritten. Got: {result}"
        );
    }

    #[test]
    fn truncated_url_without_closing_quote_is_not_modified() {
        let truncated_url = r#"self.__next_f.push([
  1,
  '\"url\":\"https://origin.example.com/rss?title=%20'
]);"#;

        let result = rewrite_nextjs_values(
            truncated_url,
            "origin.example.com",
            "proxy.example.com",
            "http",
            &["url".into()],
            true,
        );

        assert!(
            result.is_none(),
            "Truncated URL without closing quote should not be modified"
        );
    }

    #[test]
    fn backslash_n_is_preserved() {
        let input =
            r#"self.__next_f.push([1, 'foo\n{"url":"https://origin.example.com/test"}\nbar']);"#;

        let backslash_n_pos = input.find(r"\n").expect("should contain \\n");
        assert_eq!(
            &input.as_bytes()[backslash_n_pos..backslash_n_pos + 2],
            [0x5C, 0x6E],
            "Input should have literal backslash-n"
        );

        let rewritten = rewrite_nextjs_values(
            input,
            "origin.example.com",
            "proxy.example.com",
            "http",
            &["url".into()],
            true,
        )
        .expect("should rewrite URL");

        let new_pos = rewritten.find(r"\n").expect("should contain \\n");
        assert_eq!(
            &rewritten.as_bytes()[new_pos..new_pos + 2],
            [0x5C, 0x6E],
            "Rewritten should preserve literal backslash-n"
        );
    }

    #[test]
    fn site_production_domain_is_rewritten() {
        let input = r#"self.__next_f.push([1, '{"siteProductionDomain":"origin.example.com","url":"https://origin.example.com/news"}']);"#;

        let rewritten = rewrite_nextjs_values(
            input,
            "origin.example.com",
            "proxy.example.com",
            "http",
            &["url".into(), "siteProductionDomain".into()],
            true,
        )
        .expect("should rewrite URLs");

        assert!(
            rewritten.contains("proxy.example.com") && rewritten.contains("/news"),
            "Expected host to be rewritten. Got: {rewritten}"
        );
        assert!(
            !rewritten.contains("origin.example.com"),
            "Original host should not remain"
        );
    }

    #[test]
    fn whitespace_padding_calculation() {
        let padding = UrlRewriter::calculate_padding(21, 24);
        assert_eq!(padding.len(), 3, "Should need 3 spaces");
        assert_eq!(padding, "   ", "Should be 3 spaces");

        let padding = UrlRewriter::calculate_padding(24, 24);
        assert_eq!(padding.len(), 0);

        let padding = UrlRewriter::calculate_padding(30, 24);
        assert_eq!(padding.len(), 0);
    }

    #[test]
    fn whitespace_padding_rewrite() {
        let rewriter = UrlRewriter::new(
            "origin.example.com",
            "proxy.example.com",
            "http",
            &["url".into()],
            true,
        );

        let original_url = "https://origin.example.com/news";
        let result = rewriter
            .rewrite_url_value(original_url)
            .expect("URL should be rewritten");
        let (new_url, padding) = result;

        assert_eq!(new_url, "http://proxy.example.com/news");
        assert_eq!(
            new_url.len() + padding.len(),
            original_url.len(),
            "URL + padding should equal original length"
        );
        assert_eq!(padding, "  ", "Should be 2 spaces");
    }

    #[test]
    fn no_padding_when_disabled() {
        let rewriter = UrlRewriter::new(
            "origin.example.com",
            "proxy.example.com",
            "http",
            &["url".into()],
            false,
        );

        let (new_url, padding) = rewriter
            .rewrite_url_value("https://origin.example.com/news")
            .expect("URL should be rewritten");
        assert_eq!(new_url, "http://proxy.example.com/news");
        assert_eq!(padding, "", "No padding when preserve_length is false");
    }
}
