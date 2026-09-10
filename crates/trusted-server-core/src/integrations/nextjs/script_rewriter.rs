use std::sync::{Arc, Mutex};

use error_stack::Report;
use regex::{Regex, escape};

use crate::error::TrustedServerError;
use crate::integrations::{
    IntegrationScriptContext, IntegrationScriptRewriter, ScriptRewriteAction,
};

use super::shared::strip_origin_host_with_optional_port;
use super::{NEXTJS_INTEGRATION_ID, NextJsIntegrationConfig};

pub(super) struct NextJsNextDataRewriter {
    config: Arc<NextJsIntegrationConfig>,
    rewriter: UrlRewriter,
    /// Accumulates text fragments when `lol_html` splits a text node across
    /// chunk boundaries. Drained on `is_last_in_text_node`.
    ///
    /// Uses `Mutex` to satisfy the `Sync` bound on `IntegrationScriptRewriter`.
    /// The pipeline is single-threaded (`lol_html::HtmlRewriter` is `!Send`),
    /// so the lock is uncontended. `lol_html` delivers text chunks sequentially
    /// per element — the buffer is always empty when a new element's text begins.
    accumulated_text: Mutex<String>,
}

impl NextJsNextDataRewriter {
    pub(super) fn new(
        config: Arc<NextJsIntegrationConfig>,
    ) -> Result<Self, Report<TrustedServerError>> {
        Ok(Self {
            rewriter: UrlRewriter::new(&config.rewrite_attributes)?,
            config,
            accumulated_text: Mutex::new(String::new()),
        })
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

        if let Some(rewritten) = self.rewriter.rewrite_embedded(
            content,
            ctx.origin_host,
            ctx.request_host,
            ctx.request_scheme,
        ) {
            ScriptRewriteAction::replace(rewritten)
        } else {
            ScriptRewriteAction::keep()
        }
    }
}

impl IntegrationScriptRewriter for NextJsNextDataRewriter {
    fn integration_id(&self) -> &'static str {
        NEXTJS_INTEGRATION_ID
    }

    fn selector(&self) -> &'static str {
        "script#__NEXT_DATA__"
    }

    fn rewrite(&self, content: &str, ctx: &IntegrationScriptContext<'_>) -> ScriptRewriteAction {
        if self.config.rewrite_attributes.is_empty() {
            return ScriptRewriteAction::keep();
        }

        let mut buf = self
            .accumulated_text
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        if !ctx.is_last_in_text_node {
            // Intermediate fragment — accumulate and suppress output.
            buf.push_str(content);
            return ScriptRewriteAction::RemoveNode;
        }

        // Last fragment. If nothing was accumulated, process directly.
        if buf.is_empty() {
            return self.rewrite_structured(content, ctx);
        }

        // Complete the accumulated text and process the full content.
        // If rewrite_structured returns Keep, we must still emit the full
        // accumulated text via Replace — intermediate fragments were already
        // removed from lol_html's output via RemoveNode.
        buf.push_str(content);
        let full_content = std::mem::take(&mut *buf);
        let action = self.rewrite_structured(&full_content, ctx);
        if matches!(action, ScriptRewriteAction::Keep) {
            return ScriptRewriteAction::replace(full_content);
        }
        action
    }
}

#[cfg(test)]
fn rewrite_nextjs_values(
    content: &str,
    origin_host: &str,
    request_host: &str,
    request_scheme: &str,
    attributes: &[String],
) -> Option<String> {
    if origin_host.is_empty() || request_host.is_empty() || attributes.is_empty() {
        return None;
    }

    UrlRewriter::new(attributes)
        .expect("should build Next.js URL rewriter")
        .rewrite_embedded(content, origin_host, request_host, request_scheme)
}

/// Rewrites URLs in structured Next.js JSON payloads (e.g., `__NEXT_DATA__`).
///
/// This rewriter uses combined regex patterns to find and replace URLs
/// in JSON content. It handles full URLs, protocol-relative URLs, and bare hostnames.
/// Patterns for all attributes are combined with alternation for efficiency.
#[derive(Clone)]
struct UrlRewriter {
    /// Single regex matching value patterns for all configured attributes.
    value_pattern: Option<Regex>,
}

impl UrlRewriter {
    fn new(attributes: &[String]) -> Result<Self, Report<TrustedServerError>> {
        let value_pattern = if attributes.is_empty() {
            None
        } else {
            let attr_alternation = attributes
                .iter()
                .map(|attr| escape(attr))
                .collect::<Vec<_>>()
                .join("|");
            let pattern = format!(
                r#"(?P<prefix>(?:\\*")?(?:{attr_alternation})(?:\\*")?\s*:\s*\\*")(?P<value>[^"\\]*)(?P<quote>\\*")"#,
            );
            Some(Regex::new(&pattern).map_err(|err| {
                super::configuration_error(format!(
                    "failed to compile __NEXT_DATA__ URL rewrite regex for attributes {attributes:?}: {err}"
                ))
            })?)
        };

        Ok(Self { value_pattern })
    }

    fn rewrite_url_value(
        &self,
        origin_host: &str,
        url: &str,
        request_host: &str,
        request_scheme: &str,
    ) -> Option<String> {
        if let Some(rest) = url.strip_prefix("https://") {
            if let Some(path) = strip_origin_host_with_optional_port(rest, origin_host) {
                return Some(format!("{request_scheme}://{request_host}{path}"));
            }
        } else if let Some(rest) = url.strip_prefix("http://") {
            if let Some(path) = strip_origin_host_with_optional_port(rest, origin_host) {
                return Some(format!("{request_scheme}://{request_host}{path}"));
            }
        } else if let Some(rest) = url.strip_prefix("//") {
            if let Some(path) = strip_origin_host_with_optional_port(rest, origin_host) {
                return Some(format!("//{request_host}{path}"));
            }
        } else if url == origin_host {
            return Some(request_host.to_owned());
        } else if let Some(path) = strip_origin_host_with_optional_port(url, origin_host) {
            return Some(format!("{request_host}{path}"));
        }
        None
    }

    fn rewrite_embedded(
        &self,
        input: &str,
        origin_host: &str,
        request_host: &str,
        request_scheme: &str,
    ) -> Option<String> {
        let Some(regex) = &self.value_pattern else {
            return None;
        };
        if origin_host.is_empty() || !input.contains(origin_host) {
            return None;
        }
        let next_value = regex.replace_all(input, |caps: &regex::Captures<'_>| {
            let prefix = &caps["prefix"];
            let value = &caps["value"];
            let quote = &caps["quote"];

            if let Some(new_url) =
                self.rewrite_url_value(origin_host, value, request_host, request_scheme)
            {
                format!("{prefix}{new_url}{quote}")
            } else {
                caps.get(0)
                    .expect("should capture matched attribute value")
                    .as_str()
                    .to_owned()
            }
        });

        (next_value.as_ref() != input).then(|| next_value.into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integrations::IntegrationDocumentState;
    use crate::integrations::ScriptRewriteAction;

    fn test_config() -> Arc<NextJsIntegrationConfig> {
        Arc::new(NextJsIntegrationConfig {
            enabled: true,
            rewrite_attributes: vec!["href".into(), "link".into(), "url".into()],
            max_combined_payload_bytes: 10 * 1024 * 1024,
        })
    }

    fn ctx<'a>(
        selector: &'static str,
        document_state: &'a IntegrationDocumentState,
    ) -> IntegrationScriptContext<'a> {
        IntegrationScriptContext {
            selector,
            request_host: "ts.example.com",
            request_scheme: "https",
            origin_host: "origin.example.com",
            is_last_in_text_node: true,
            document_state,
        }
    }

    #[test]
    fn structured_rewriter_updates_next_data_payload() {
        let payload = r#"{"props":{"pageProps":{"primary":{"href":"https://origin.example.com/reviews"},"secondary":{"href":"http://origin.example.com/sign-in"},"fallbackHref":"http://origin.example.com/legacy","protoRelative":"//origin.example.com/assets/logo.png"}}}"#;
        let rewriter = NextJsNextDataRewriter::new(test_config())
            .expect("should build Next.js structured rewriter");
        let document_state = IntegrationDocumentState::default();
        let result = rewriter.rewrite(payload, &ctx("script#__NEXT_DATA__", &document_state));

        match result {
            ScriptRewriteAction::Replace(value) => {
                assert!(value.contains("ts.example.com") && value.contains("/reviews"));
                assert!(value.contains("ts.example.com") && value.contains("/sign-in"));
                assert!(value.contains(r#""fallbackHref":"http://origin.example.com/legacy""#));
                assert!(
                    value.contains(r#""protoRelative":"//origin.example.com/assets/logo.png""#)
                );
            }
            _ => panic!("Expected rewrite to update payload"),
        }
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
        )
        .expect("should rewrite protocol relative link");

        assert!(rewritten.contains("ts.example.com") && rewritten.contains("/image.png"));
    }

    #[test]
    fn rewrite_helper_handles_whitespace_around_colons() {
        let content = r#"{
            "props": {
                "pageProps": {
                    "siteProductionDomain": "origin.example.com",
                    "siteBaseUrl": "https://origin.example.com",
                    "href": "https://origin.example.com/reviews"
                }
            }
        }"#;
        let rewritten = rewrite_nextjs_values(
            content,
            "origin.example.com",
            "ts.example.com",
            "https",
            &[
                "href".into(),
                "siteBaseUrl".into(),
                "siteProductionDomain".into(),
            ],
        )
        .expect("should rewrite pretty-printed JSON values");

        assert!(rewritten.contains(r#""siteProductionDomain": "ts.example.com""#));
        assert!(rewritten.contains(r#""siteBaseUrl": "https://ts.example.com""#));
        assert!(rewritten.contains(r#""href": "https://ts.example.com/reviews""#));
    }

    #[test]
    fn rewrite_helper_rewrites_explicit_port_urls() {
        let content = r#"{"props":{"pageProps":{"url":"https://origin.example.com:8443/news","link":"//origin.example.com:9443/image.png"}}}"#;
        let rewritten = rewrite_nextjs_values(
            content,
            "origin.example.com",
            "ts.example.com",
            "https",
            &["url".into(), "link".into()],
        )
        .expect("should rewrite explicit port URLs");

        assert!(rewritten.contains("https://ts.example.com:8443/news"));
        assert!(rewritten.contains("//ts.example.com:9443/image.png"));
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
    fn url_rewriter_rewrites_url() {
        let rewriter = UrlRewriter::new(&["url".into()]).expect("should build URL rewriter");

        let new_url = rewriter
            .rewrite_url_value(
                "origin.example.com",
                "https://origin.example.com/news",
                "proxy.example.com",
                "http",
            )
            .expect("URL should be rewritten");
        assert_eq!(new_url, "http://proxy.example.com/news");
    }

    #[test]
    fn url_rewriter_does_not_rewrite_partial_hostname_matches() {
        let rewriter = UrlRewriter::new(&["url".into(), "siteProductionDomain".into()])
            .expect("should build URL rewriter");
        let input = r#"{"url":"https://origin.example.com.evil/news","siteProductionDomain":"origin.example.com.evil"}"#;

        let rewritten =
            rewriter.rewrite_embedded(input, "origin.example.com", "proxy.example.com", "https");

        assert!(
            rewritten.is_none(),
            "should not rewrite partial hostname matches"
        );
    }

    #[test]
    fn url_rewriter_rewrites_explicit_port_url() {
        let rewriter = UrlRewriter::new(&["url".into()]).expect("should build URL rewriter");

        let new_url = rewriter
            .rewrite_url_value(
                "origin.example.com",
                "https://origin.example.com:8443/news",
                "proxy.example.com",
                "http",
            )
            .expect("URL with explicit port should be rewritten");
        assert_eq!(new_url, "http://proxy.example.com:8443/news");
    }

    #[test]
    fn url_rewriter_supports_regex_metacharacters_in_literals() {
        let rewriter = UrlRewriter::new(&["href(".into(), "u|rl".into()])
            .expect("should build URL rewriter with metacharacters");
        let input = r#"{"href(":"https://origin.(example).com/news","u|rl":"//origin.(example).com/assets/logo.png"}"#;

        let rewritten = rewriter
            .rewrite_embedded(input, "origin.(example).com", "proxy.example.com", "https")
            .expect("should rewrite metacharacter-heavy literals");

        assert!(rewritten.contains("https://proxy.example.com/news"));
        assert!(rewritten.contains("//proxy.example.com/assets/logo.png"));
    }

    #[test]
    fn fragmented_next_data_is_accumulated_and_rewritten() {
        let rewriter = NextJsNextDataRewriter::new(test_config()).expect("should build rewriter");
        let document_state = IntegrationDocumentState::default();

        let fragment1 = r#"{"props":{"pageProps":{"href":"https://origin."#;
        let fragment2 = r#"example.com/reviews"}}}"#;

        let ctx_intermediate = IntegrationScriptContext {
            selector: "script#__NEXT_DATA__",
            request_host: "ts.example.com",
            request_scheme: "https",
            origin_host: "origin.example.com",
            is_last_in_text_node: false,
            document_state: &document_state,
        };
        let ctx_last = IntegrationScriptContext {
            is_last_in_text_node: true,
            ..ctx_intermediate
        };

        let action1 = rewriter.rewrite(fragment1, &ctx_intermediate);
        assert_eq!(
            action1,
            ScriptRewriteAction::RemoveNode,
            "should suppress intermediate fragment"
        );

        let action2 = rewriter.rewrite(fragment2, &ctx_last);
        match action2 {
            ScriptRewriteAction::Replace(rewritten) => {
                assert!(
                    rewritten.contains("ts.example.com"),
                    "should rewrite origin to proxy host. Got: {rewritten}"
                );
                assert!(
                    rewritten.contains("/reviews"),
                    "should preserve path. Got: {rewritten}"
                );
                assert!(
                    !rewritten.contains("origin.example.com"),
                    "should not contain original host. Got: {rewritten}"
                );
            }
            other => panic!("expected Replace, got {other:?}"),
        }
    }

    #[test]
    fn unfragmented_next_data_works_without_accumulation() {
        let rewriter = NextJsNextDataRewriter::new(test_config()).expect("should build rewriter");
        let document_state = IntegrationDocumentState::default();
        let payload = r#"{"props":{"pageProps":{"href":"https://origin.example.com/page"}}}"#;

        let ctx_single = IntegrationScriptContext {
            selector: "script#__NEXT_DATA__",
            request_host: "ts.example.com",
            request_scheme: "https",
            origin_host: "origin.example.com",
            is_last_in_text_node: true,
            document_state: &document_state,
        };

        let action = rewriter.rewrite(payload, &ctx_single);
        match action {
            ScriptRewriteAction::Replace(rewritten) => {
                assert!(
                    rewritten.contains("ts.example.com"),
                    "should rewrite. Got: {rewritten}"
                );
            }
            other => panic!("expected Replace, got {other:?}"),
        }
    }

    #[test]
    fn fragmented_next_data_without_rewritable_urls_preserves_content() {
        let rewriter = NextJsNextDataRewriter::new(test_config()).expect("should build rewriter");
        let document_state = IntegrationDocumentState::default();

        // __NEXT_DATA__ JSON with no origin URLs — rewrite_structured returns Keep.
        let fragment1 = r#"{"props":{"pageProps":{"title":"Hello"#;
        let fragment2 = r#" World","count":42}}}"#;

        let ctx_intermediate = IntegrationScriptContext {
            selector: "script#__NEXT_DATA__",
            request_host: "ts.example.com",
            request_scheme: "https",
            origin_host: "origin.example.com",
            is_last_in_text_node: false,
            document_state: &document_state,
        };
        let ctx_last = IntegrationScriptContext {
            is_last_in_text_node: true,
            ..ctx_intermediate
        };

        let action1 = rewriter.rewrite(fragment1, &ctx_intermediate);
        assert_eq!(action1, ScriptRewriteAction::RemoveNode);

        // Last fragment: even though no URLs to rewrite, must emit full content
        // because intermediate fragments were removed.
        let action2 = rewriter.rewrite(fragment2, &ctx_last);
        match action2 {
            ScriptRewriteAction::Replace(content) => {
                let expected = format!("{fragment1}{fragment2}");
                assert_eq!(
                    content, expected,
                    "should emit full accumulated content unchanged"
                );
            }
            other => panic!("expected Replace with passthrough, got {other:?}"),
        }
    }
}
