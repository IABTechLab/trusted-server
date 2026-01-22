use std::sync::Arc;

use regex::{escape, Regex};

use crate::integrations::{
    IntegrationScriptContext, IntegrationScriptRewriter, ScriptRewriteAction,
};

use super::{NextJsIntegrationConfig, NEXTJS_INTEGRATION_ID};

pub(super) struct NextJsNextDataRewriter {
    config: Arc<NextJsIntegrationConfig>,
}

impl NextJsNextDataRewriter {
    pub(super) fn new(config: Arc<NextJsIntegrationConfig>) -> Self {
        Self { config }
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
        );

        if let Some(rewritten) = rewrite_nextjs_values_with_rewriter(content, &rewriter) {
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

        self.rewrite_structured(content, ctx)
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
) -> Option<String> {
    if origin_host.is_empty() || request_host.is_empty() || attributes.is_empty() {
        return None;
    }

    let rewriter = UrlRewriter::new(origin_host, request_host, request_scheme, attributes);

    rewrite_nextjs_values_with_rewriter(content, &rewriter)
}

/// Rewrites URLs in structured Next.js JSON payloads (e.g., `__NEXT_DATA__`).
///
/// This rewriter uses combined regex patterns to find and replace URLs
/// in JSON content. It handles full URLs, protocol-relative URLs, and bare hostnames.
/// Patterns for all attributes are combined with alternation for efficiency.
struct UrlRewriter {
    #[cfg_attr(not(test), allow(dead_code))]
    origin_host: String,
    request_host: String,
    request_scheme: String,
    /// Single regex matching URL patterns for all attributes
    embedded_pattern: Option<Regex>,
    /// Single regex matching bare hostname patterns for all attributes
    bare_host_pattern: Option<Regex>,
}

impl UrlRewriter {
    fn new(
        origin_host: &str,
        request_host: &str,
        request_scheme: &str,
        attributes: &[String],
    ) -> Self {
        let escaped_origin = escape(origin_host);

        // Build a single regex with alternation for all attributes
        let embedded_pattern = if attributes.is_empty() {
            None
        } else {
            let attr_alternation = attributes
                .iter()
                .map(|attr| escape(attr))
                .collect::<Vec<_>>()
                .join("|");
            let pattern = format!(
                r#"(?P<prefix>(?:\\*")?(?:{attrs})(?:\\*")?:\\*")(?P<scheme>https?://|//){origin}(?P<path>[^"\\]*)(?P<quote>\\*")"#,
                attrs = attr_alternation,
                origin = escaped_origin,
            );
            Some(Regex::new(&pattern).expect("valid Next.js rewrite regex"))
        };

        let bare_host_pattern = if attributes.is_empty() {
            None
        } else {
            let attr_alternation = attributes
                .iter()
                .map(|attr| escape(attr))
                .collect::<Vec<_>>()
                .join("|");
            let pattern = format!(
                r#"(?P<prefix>(?:\\*")?(?:{attrs})(?:\\*")?:\\*"){origin}(?P<suffix>\\*")"#,
                attrs = attr_alternation,
                origin = escaped_origin,
            );
            Some(Regex::new(&pattern).expect("valid Next.js bare host rewrite regex"))
        };

        Self {
            origin_host: origin_host.to_string(),
            request_host: request_host.to_string(),
            request_scheme: request_scheme.to_string(),
            embedded_pattern,
            bare_host_pattern,
        }
    }

    #[cfg(test)]
    fn rewrite_url_value(&self, url: &str) -> Option<String> {
        if let Some(rest) = url.strip_prefix("https://") {
            if rest.starts_with(&self.origin_host) {
                let path = &rest[self.origin_host.len()..];
                return Some(format!(
                    "{}://{}{}",
                    self.request_scheme, self.request_host, path
                ));
            }
        } else if let Some(rest) = url.strip_prefix("http://") {
            if rest.starts_with(&self.origin_host) {
                let path = &rest[self.origin_host.len()..];
                return Some(format!(
                    "{}://{}{}",
                    self.request_scheme, self.request_host, path
                ));
            }
        } else if let Some(rest) = url.strip_prefix("//") {
            if rest.starts_with(&self.origin_host) {
                let path = &rest[self.origin_host.len()..];
                return Some(format!("//{}{}", self.request_host, path));
            }
        } else if url == self.origin_host {
            return Some(self.request_host.clone());
        } else if url.starts_with(&self.origin_host) {
            let path = &url[self.origin_host.len()..];
            return Some(format!("{}{}", self.request_host, path));
        }
        None
    }

    fn rewrite_embedded(&self, input: &str) -> Option<String> {
        let mut result = input.to_string();
        let mut changed = false;

        if let Some(regex) = &self.embedded_pattern {
            let request_host = &self.request_host;
            let request_scheme = &self.request_scheme;

            let next_value = regex.replace_all(&result, |caps: &regex::Captures<'_>| {
                let prefix = &caps["prefix"];
                let scheme = &caps["scheme"];
                let path = &caps["path"];
                let quote = &caps["quote"];

                let new_url = if scheme == "//" {
                    format!("//{}{}", request_host, path)
                } else {
                    format!("{}://{}{}", request_scheme, request_host, path)
                };

                format!("{prefix}{new_url}{quote}")
            });

            if next_value != result {
                changed = true;
                result = next_value.into_owned();
            }
        }

        if let Some(regex) = &self.bare_host_pattern {
            let request_host = &self.request_host;

            let next_value = regex.replace_all(&result, |caps: &regex::Captures<'_>| {
                let prefix = &caps["prefix"];
                let suffix = &caps["suffix"];

                format!("{prefix}{request_host}{suffix}")
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
#[allow(clippy::panic, clippy::unwrap_used)]
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
        let rewriter = NextJsNextDataRewriter::new(test_config());
        let document_state = IntegrationDocumentState::default();
        let result = rewriter.rewrite(payload, &ctx("script#__NEXT_DATA__", &document_state));

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
        let rewriter = UrlRewriter::new(
            "origin.example.com",
            "proxy.example.com",
            "http",
            &["url".into()],
        );

        let new_url = rewriter
            .rewrite_url_value("https://origin.example.com/news")
            .expect("URL should be rewritten");
        assert_eq!(new_url, "http://proxy.example.com/news");
    }
}
