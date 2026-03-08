use std::cell::Cell;
use std::sync::Arc;

use error_stack::Report;
use regex::{escape, Regex};

use crate::error::TrustedServerError;
use crate::integrations::{
    IntegrationScriptContext, IntegrationScriptRewriter, ScriptRewriteAction,
};

use super::{NextJsIntegrationConfig, NEXTJS_INTEGRATION_ID};

pub(super) struct NextJsNextDataRewriter {
    config: Arc<NextJsIntegrationConfig>,
    rewriter: UrlRewriter,
}

impl NextJsNextDataRewriter {
    pub(super) fn new(
        config: Arc<NextJsIntegrationConfig>,
    ) -> Result<Self, Report<TrustedServerError>> {
        Ok(Self {
            rewriter: UrlRewriter::new(&config.rewrite_attributes)?,
            config,
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

        if let Some(rewritten) = rewrite_nextjs_values_with_rewriter(
            content,
            &self.rewriter,
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

        self.rewrite_structured(content, ctx)
    }
}

fn rewrite_nextjs_values_with_rewriter(
    content: &str,
    rewriter: &UrlRewriter,
    origin_host: &str,
    request_host: &str,
    request_scheme: &str,
) -> Option<String> {
    rewriter.rewrite_embedded(content, origin_host, request_host, request_scheme)
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

    let rewriter = UrlRewriter::new(attributes).expect("should build Next.js URL rewriter");

    rewrite_nextjs_values_with_rewriter(
        content,
        &rewriter,
        origin_host,
        request_host,
        request_scheme,
    )
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
                r#"(?P<prefix>(?:\\*")?(?:{attrs})(?:\\*")?:\\*")(?P<value>[^"\\]*)(?P<quote>\\*")"#,
                attrs = attr_alternation,
            );
            Some(Regex::new(&pattern).map_err(|err| {
                super::configuration_error(format!(
                    "failed to compile __NEXT_DATA__ URL rewrite regex for attributes {:?}: {err}",
                    attributes
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
        fn strip_origin_host_with_boundary<'a>(
            value: &'a str,
            origin_host: &str,
        ) -> Option<&'a str> {
            let suffix = value.strip_prefix(origin_host)?;
            let boundary_ok = suffix.is_empty()
                || matches!(
                    suffix.as_bytes().first(),
                    Some(b'/') | Some(b'?') | Some(b'#')
                );
            boundary_ok.then_some(suffix)
        }

        if let Some(rest) = url.strip_prefix("https://") {
            if let Some(path) = strip_origin_host_with_boundary(rest, origin_host) {
                return Some(format!("{request_scheme}://{request_host}{path}"));
            }
        } else if let Some(rest) = url.strip_prefix("http://") {
            if let Some(path) = strip_origin_host_with_boundary(rest, origin_host) {
                return Some(format!("{request_scheme}://{request_host}{path}"));
            }
        } else if let Some(rest) = url.strip_prefix("//") {
            if let Some(path) = strip_origin_host_with_boundary(rest, origin_host) {
                return Some(format!("//{request_host}{path}"));
            }
        } else if url == origin_host {
            return Some(request_host.to_string());
        } else if let Some(path) = strip_origin_host_with_boundary(url, origin_host) {
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
        let changed = Cell::new(false);
        let next_value = regex.replace_all(input, |caps: &regex::Captures<'_>| {
            let prefix = &caps["prefix"];
            let value = &caps["value"];
            let quote = &caps["quote"];

            if let Some(new_url) =
                self.rewrite_url_value(origin_host, value, request_host, request_scheme)
            {
                changed.set(true);
                format!("{prefix}{new_url}{quote}")
            } else {
                caps.get(0)
                    .expect("should capture matched attribute value")
                    .as_str()
                    .to_string()
            }
        });

        changed.get().then(|| next_value.into_owned())
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
}
