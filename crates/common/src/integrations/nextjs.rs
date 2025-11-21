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
    #[serde(default = "default_remove_prebid")]
    pub remove_prebid: bool,
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

fn default_remove_prebid() -> bool {
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
            self.config.remove_prebid,
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
                if !content.contains("self.__next_f") {
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
    remove_prebid: bool,
) -> Option<String> {
    if origin_host.is_empty() || request_host.is_empty() || attributes.is_empty() {
        return None;
    }

    let mut rewritten = content.to_string();
    let mut changed = false;
    let escaped_origin = escape(origin_host);
    let replacement_scheme = format!("{}://{}", request_scheme, request_host);

    // Rewrite URL attributes
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

    if remove_prebid {
        if rewritten.contains("prebid") && rewritten.contains(".js") {
            if let Ok(prebid_file_pattern) =
                Regex::new(r#"[/\\][^"'\s]*?prebid[^"'\s]*?\.js[^"'\s]*?"#)
            {
                let new_value = prebid_file_pattern.replace_all(&rewritten, "");
                if new_value != rewritten {
                    changed = true;
                    rewritten = new_value.into_owned();
                }
            }
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
            remove_prebid: false,
        })
    }

    fn test_config_with_prebid_removal() -> Arc<NextJsIntegrationConfig> {
        Arc::new(NextJsIntegrationConfig {
            enabled: true,
            rewrite_attributes: vec!["href".into(), "link".into(), "url".into()],
            remove_prebid: true,
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
    fn rewrite_helper_removes_prebid_files_from_nextjs_payload() {
        let payload = r#"self.__next_f.push([1,"[\"$\",\"link\",{\"href\":\"/js/prebid.min.js?v=2025-11-20\"}],[\"$\",\"script\",{\"src\":\"/js/prebid.js\",\"children\":\"var pbjs=pbjs||{};pbjs.que=pbjs.que||[];\"}]"]);"#;
        let rewritten = rewrite_nextjs_values(
            payload,
            "origin.example.com",
            "ts.example.com",
            "https",
            &["href".into()],
            true, // remove_prebid enabled
        )
        .expect("should remove prebid file references");

        assert!(
            !rewritten.contains("prebid.min.js"),
            "Should remove prebid.min.js file references"
        );
        assert!(
            !rewritten.contains("/js/prebid.js"),
            "Should remove prebid.js file references"
        );
        assert!(
            rewritten.contains("pbjs=pbjs||{}"),
            "Should KEEP pbjs initialization (needed by shim)"
        );
        assert!(
            rewritten.contains("pbjs.que"),
            "Should KEEP pbjs.que (needed by shim)"
        );
        assert!(
            rewritten.contains("self.__next_f"),
            "Should preserve Next.js payload structure"
        );
    }

    #[test]
    fn rewrite_helper_respects_remove_prebid_flag() {
        let payload = r#"self.__next_f.push([1,"var pbjs=pbjs||{};"]);"#;

        // With remove_prebid = false, should keep prebid code
        let not_removed = rewrite_nextjs_values(
            payload,
            "origin.example.com",
            "ts.example.com",
            "https",
            &["href".into()],
            false, // remove_prebid disabled
        );

        // Should return None since no attributes to rewrite and prebid removal disabled
        assert!(
            not_removed.is_none(),
            "Should not modify when remove_prebid is false"
        );
    }

    #[test]
    fn streamed_rewriter_removes_prebid_files_when_configured() {
        let payload = r#"self.__next_f.push([1,"[\"$\",\"script\",{\"src\":\"/js/prebid.min.js\"}],[\"$\",\"script\",{\"children\":\"var pbjs=pbjs||{};\"}]"]);"#;
        let rewriter = NextJsScriptRewriter::new(
            test_config_with_prebid_removal(),
            NextJsRewriteMode::Streamed,
        );
        let result = rewriter.rewrite(payload, &ctx("script"));

        match result {
            ScriptRewriteAction::Replace(value) => {
                assert!(
                    !value.contains("prebid.min.js"),
                    "Should remove prebid file references from Next.js payloads"
                );
                assert!(
                    value.contains("var pbjs=pbjs||{}"),
                    "Should keep pbjs initialization code (shim needs it)"
                );
            }
            _ => panic!("Expected prebid file removal from Next.js payload"),
        }
    }
}
