use std::sync::Arc;

use regex::{escape, Regex};
use serde::{Deserialize, Serialize};
use validator::Validate;

use crate::integrations::{
    IntegrationRegistration, IntegrationScriptContext, IntegrationScriptRewriter,
    ScriptRewriteAction,
};
use crate::settings::{IntegrationConfig, Settings};

const NEXTJS_INTEGRATION_ID: &str = "nextjs";

#[derive(Debug, Clone, Deserialize, Serialize, Validate)]
pub struct NextJsIntegrationConfig {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(
        default = "default_rewrite_attributes",
        deserialize_with = "crate::settings::vec_from_seq_or_map"
    )]
    #[validate(length(min = 1))]
    pub rewrite_attributes: Vec<String>,
    #[serde(default = "default_rewrite_prebid")]
    pub rewrite_prebid: bool,
}

impl IntegrationConfig for NextJsIntegrationConfig {
    fn is_enabled(&self) -> bool {
        self.enabled
    }
}

fn default_enabled() -> bool {
    false
}

fn default_rewrite_attributes() -> Vec<String> {
    vec!["href".to_string(), "link".to_string(), "url".to_string()]
}

fn default_rewrite_prebid() -> bool {
    true
}

pub fn register(settings: &Settings) -> Option<IntegrationRegistration> {
    let config = build(settings)?;
    let structured = Arc::new(NextJsScriptRewriter::new(
        Arc::clone(&config),
        NextJsRewriteMode::Structured,
    ));
    let streamed = Arc::new(NextJsScriptRewriter::new(
        config,
        NextJsRewriteMode::Streamed,
    ));

    Some(
        IntegrationRegistration::builder(NEXTJS_INTEGRATION_ID)
            .with_script_rewriter(structured)
            .with_script_rewriter(streamed)
            .build(),
    )
}

fn build(settings: &Settings) -> Option<Arc<NextJsIntegrationConfig>> {
    let config = settings
        .integration_config::<NextJsIntegrationConfig>(NEXTJS_INTEGRATION_ID)
        .ok()
        .flatten()?;
    Some(Arc::new(config))
}

#[derive(Clone, Copy)]
enum NextJsRewriteMode {
    Structured,
    Streamed,
}

struct NextJsScriptRewriter {
    config: Arc<NextJsIntegrationConfig>,
    mode: NextJsRewriteMode,
}

impl NextJsScriptRewriter {
    fn new(config: Arc<NextJsIntegrationConfig>, mode: NextJsRewriteMode) -> Self {
        Self { config, mode }
    }

    fn rewrite_values(
        &self,
        content: &str,
        ctx: &IntegrationScriptContext<'_>,
    ) -> ScriptRewriteAction {
        if let Some(rewritten) = rewrite_nextjs_values(
            content,
            ctx.origin_host,
            ctx.request_host,
            ctx.request_scheme,
            &self.config.rewrite_attributes,
            self.config.rewrite_prebid,
        ) {
            ScriptRewriteAction::replace(rewritten)
        } else {
            ScriptRewriteAction::keep()
        }
    }
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
            NextJsRewriteMode::Structured => self.rewrite_values(content, ctx),
            NextJsRewriteMode::Streamed => {
                // Check for Next.js streaming patterns:
                // - self.__next_f = Flight data (component streaming)
                // - self.__next_s = Script streaming (for <Script> components)
                if !content.contains("self.__next_f") && !content.contains("self.__next_s") {
                    return ScriptRewriteAction::keep();
                }
                self.rewrite_values(content, ctx)
            }
        }
    }
}

fn rewrite_nextjs_values(
    content: &str,
    origin_host: &str,
    request_host: &str,
    request_scheme: &str,
    attributes: &[String],
    rewrite_prebid: bool,
) -> Option<String> {
    if origin_host.is_empty() || request_host.is_empty() {
        return None;
    }

    let mut rewritten = content.to_string();
    let mut changed = false;

    // First, rewrite attribute-based URLs (href, link, url, etc.)
    if !attributes.is_empty() {
        let escaped_origin = escape(origin_host);
        let replacement_scheme = format!("{}://{}", request_scheme, request_host);

        for attribute in attributes {
            let escaped_attr = escape(attribute);
            let pattern = format!(
                r#"(?P<prefix>(?:\\*")?{attr}(?:\\*")?:\\*")(?P<scheme>https?://|//){origin}"#,
                attr = escaped_attr,
                origin = escaped_origin,
            );
            let regex = Regex::new(&pattern).expect("valid Next.js rewrite regex");
            let next_value = regex.replace_all(&rewritten, |caps: &regex::Captures<'_>| {
                let scheme = &caps["scheme"];
                let replacement = if scheme == "//" {
                    format!("//{}", request_host)
                } else {
                    replacement_scheme.clone()
                };
                format!("{}{}", &caps["prefix"], replacement)
            });
            if next_value != rewritten {
                changed = true;
                rewritten = next_value.into_owned();
            }
        }
    }

    // Second, rewrite prebid script URLs to our static shim endpoint
    if rewrite_prebid {
        // Match prebid URLs in two contexts:
        // 1. URLs in JSON attribute values (with double quotes)
        // 2. URLs inside JavaScript code within JSON strings

        // We use a broader pattern that matches URL-like paths containing prebid
        // The pattern looks for: (/ or http) followed by path containing "prebid" and ending in ".js"
        // This works even when the URL is embedded in JavaScript inside a JSON string
        let prebid_pattern = r#"(?P<url>(?:https?:)?//[^\s"'\\]+/[^\s"'\\]*prebid[^\s"'\\]*\.js(?:\?[^\s"'\\]*)?|/[^\s"'\\]*/prebid[^\s"'\\]*\.js(?:\?[^\s"'\\]*)?|/\.static/prebid[^\s"'\\]*\.js)"#;
        let prebid_regex = Regex::new(prebid_pattern).expect("valid prebid URL regex");

        let next_value = prebid_regex.replace_all(&rewritten, |caps: &regex::Captures<'_>| {
            let url = &caps["url"];
            let lower = url.to_lowercase();
            if lower.contains("prebid") && lower.contains(".js") {
                changed = true;
                "/static/scripts/prebid.min.js".to_string()
            } else {
                caps[0].to_string()
            }
        });

        if next_value != rewritten {
            changed = true;
            rewritten = next_value.into_owned();
        }
    }

    changed.then_some(rewritten)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integrations::{IntegrationScriptContext, ScriptRewriteAction};

    fn test_config() -> Arc<NextJsIntegrationConfig> {
        Arc::new(NextJsIntegrationConfig {
            enabled: true,
            rewrite_attributes: vec!["href".into(), "link".into(), "url".into()],
            rewrite_prebid: true,
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
                assert!(value.contains(r#""href":"https://ts.example.com/reviews""#));
                assert!(value.contains(r#""href":"https://ts.example.com/sign-in""#));
                assert!(value.contains(r#""fallbackHref":"http://origin.example.com/legacy""#));
                assert!(value.contains(r#""protoRelative":"//origin.example.com/assets/logo.png""#));
            }
            _ => panic!("Expected rewrite to update payload"),
        }
    }

    #[test]
    fn streamed_rewriter_only_runs_for_next_payloads() {
        let rewriter = NextJsScriptRewriter::new(test_config(), NextJsRewriteMode::Streamed);

        let noop = rewriter.rewrite("console.log('hello');", &ctx("script"));
        assert!(matches!(noop, ScriptRewriteAction::Keep));

        let payload = r#"self.__next_f.push(["chunk", "{\"href\":\"https://origin.example.com/app\"}"]);
        "#;
        let rewritten = rewriter.rewrite(payload, &ctx("script"));
        match rewritten {
            ScriptRewriteAction::Replace(value) => {
                assert!(value.contains(r#"https://ts.example.com/app"#));
            }
            _ => panic!("Expected streamed payload rewrite"),
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
            false,
        )
        .expect("should rewrite protocol relative link");

        assert!(rewritten.contains(r#""link":"//ts.example.com/image.png""#));
    }

    #[test]
    fn rewrite_prebid_urls_in_structured_payload() {
        let payload = r#"{"props":{"pageProps":{"scripts":[{"src":"https://cdn.example.com/prebid.js"},{"src":"https://cdn.example.com/app.js"}]}}}"#;
        let rewritten = rewrite_nextjs_values(
            payload,
            "origin.example.com",
            "ts.example.com",
            "https",
            &[],
            true,
        )
        .expect("should rewrite prebid URL");

        assert!(
            rewritten.contains(r#""src":"/static/scripts/prebid.min.js""#),
            "prebid script should be rewritten to static endpoint"
        );
        assert!(
            rewritten.contains(r#""src":"https://cdn.example.com/app.js""#),
            "non-prebid script should remain unchanged"
        );
    }

    #[test]
    fn rewrite_prebid_urls_with_min_and_query_params() {
        let payload = r#"{"script":"https://cdn.prebid.org/prebid.min.js?v=1.2.3"}"#;
        let rewritten = rewrite_nextjs_values(
            payload,
            "origin.example.com",
            "ts.example.com",
            "https",
            &[],
            true,
        )
        .expect("should rewrite prebid URL with query params");

        assert!(
            rewritten.contains(r#""/static/scripts/prebid.min.js""#),
            "prebid.min.js with query params should be rewritten"
        );
    }

    #[test]
    fn rewrite_prebid_protocol_relative_urls() {
        let payload = r#"{"src":"//cdn.example.com/prebidjs.min.js"}"#;
        let rewritten = rewrite_nextjs_values(
            payload,
            "origin.example.com",
            "ts.example.com",
            "https",
            &[],
            true,
        )
        .expect("should rewrite protocol-relative prebid URL");

        assert!(
            rewritten.contains(r#""/static/scripts/prebid.min.js""#),
            "protocol-relative prebid URL should be rewritten"
        );
    }

    #[test]
    fn rewrite_prebid_urls_with_escaped_quotes() {
        // Simulate escaped JSON strings as they appear in Next.js streaming payloads
        let payload =
            r#"self.__next_f.push([1,"{\"src\":\"https://cdn.example.com/prebid.js\"}"]);"#;
        let rewritten = rewrite_nextjs_values(
            payload,
            "origin.example.com",
            "ts.example.com",
            "https",
            &[],
            true,
        )
        .expect("should rewrite prebid URL with escaped quotes");

        assert!(
            rewritten.contains(r#"\"/static/scripts/prebid.min.js\""#),
            "escaped prebid URL should be rewritten and remain escaped"
        );
    }

    #[test]
    fn rewrite_prebid_respects_config_flag() {
        let payload = r#"{"src":"https://cdn.example.com/prebid.js"}"#;

        // With rewrite_prebid = true
        let rewritten = rewrite_nextjs_values(
            payload,
            "origin.example.com",
            "ts.example.com",
            "https",
            &[],
            true,
        );
        assert!(rewritten.is_some(), "should rewrite when flag is true");

        // With rewrite_prebid = false
        let not_rewritten = rewrite_nextjs_values(
            payload,
            "origin.example.com",
            "ts.example.com",
            "https",
            &[],
            false,
        );
        assert!(
            not_rewritten.is_none(),
            "should not rewrite when flag is false"
        );
    }

    #[test]
    fn rewrite_prebid_in_javascript_inside_json() {
        // Test JavaScript code with single quotes inside a JSON string (like dangerouslySetInnerHTML)
        // The outer JSON uses double quotes, inner JavaScript uses single quotes
        let payload = r#"{"__html":"var s=document.createElement('script');s.src='/.static/prebid/1.0.8/prebid.min.js';document.head.appendChild(s)"}"#;

        // Should rewrite the prebid URL even though it's inside JavaScript code in a JSON string
        let rewritten = rewrite_nextjs_values(
            payload,
            "origin.example.com",
            "ts.example.com",
            "https",
            &[],
            true,
        )
        .expect("should rewrite prebid URL inside JavaScript");

        assert!(
            rewritten.contains("/static/scripts/prebid.min.js"),
            "should rewrite prebid URL to blank endpoint"
        );
        assert!(
            !rewritten.contains("/.static/prebid/"),
            "should not contain original prebid path"
        );
        assert!(
            rewritten.contains("s.src="),
            "should preserve JavaScript structure"
        );
    }

    #[test]
    fn rewrite_prebid_in_next_s_streaming() {
        // Test self.__next_s pattern (script streaming)
        let payload = r#"(self.__next_s=self.__next_s||[]).push(["https://cdn.example.com/prebid.js",{"id":"prebid-script"}])"#;
        let rewritten = rewrite_nextjs_values(
            payload,
            "origin.example.com",
            "ts.example.com",
            "https",
            &[],
            true,
        )
        .expect("should rewrite prebid URL in __next_s");

        assert!(
            rewritten.contains("\"/static/scripts/prebid.min.js\""),
            "prebid URL in __next_s should be rewritten"
        );
    }

    #[test]
    fn integration_test_rewriter_with_prebid() {
        let payload = r#"self.__next_f.push([1, "{\"props\":{\"scripts\":[{\"src\":\"https://cdn.example.com/prebid.min.js\"},{\"src\":\"/app.js\"}]}}"]);"#;
        let rewriter = NextJsScriptRewriter::new(test_config(), NextJsRewriteMode::Streamed);
        let result = rewriter.rewrite(payload, &ctx("script"));

        match result {
            ScriptRewriteAction::Replace(value) => {
                assert!(
                    value.contains(r#"\"/static/scripts/prebid.min.js\""#),
                    "prebid URL should be rewritten in streaming payload"
                );
                assert!(
                    value.contains(r#"\"/app.js\""#),
                    "non-prebid script should remain unchanged"
                );
            }
            _ => panic!("Expected rewrite action"),
        }
    }
}
