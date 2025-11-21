use std::sync::Arc;

use regex::{escape, Regex};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
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

/// Extract Next.js streaming payload components
/// Returns: (prefix, chunk_id, json_str_escaped)
fn extract_nextjs_streaming_payload(content: &str) -> Option<(String, String, String)> {
    let pattern = Regex::new(r#"(self\.__next_[fs]\.push\(\[)(\d+),"(.+)"\]\)"#).ok()?;
    let caps = pattern.captures(content)?;

    Some((
        caps.get(1)?.as_str().to_string(),
        caps.get(2)?.as_str().to_string(),
        caps.get(3)?.as_str().to_string(),
    ))
}

fn has_prebid_reference(value: &JsonValue) -> bool {
    match value {
        JsonValue::String(s) => s.contains("prebid") && s.contains(".js"),
        JsonValue::Object(obj) => obj.values().any(|v| has_prebid_reference(v)),
        JsonValue::Array(arr) => arr.iter().any(|v| has_prebid_reference(v)),
        _ => false,
    }
}

fn has_init_code(props: &JsonValue) -> bool {
    if let JsonValue::Object(obj) = props {
        if let Some(JsonValue::String(children)) = obj.get("children") {
            return children.contains("pbjs=pbjs||{}") || children.contains("pbjs.que");
        }
    }
    false
}

fn process_react_element(elem: &mut JsonValue, remove_prebid: bool) -> bool {
    if !remove_prebid {
        return true;
    }

    // Check if this is a React element: ["$", "element_type", {props}]
    if let JsonValue::Array(arr) = elem {
        if arr.len() >= 3 {
            // Check for ["$", ...] pattern
            if let Some(JsonValue::String(dollar)) = arr.get(0) {
                if dollar != "$" {
                    return true;
                }
            }

            let elem_type = arr.get(1).and_then(|v| v.as_str()).map(|s| s.to_string());

            if let Some(elem_type) = elem_type {
                if let Some(props) = arr.get_mut(2) {
                    if !has_prebid_reference(props) {
                        return true;
                    }

                    if elem_type == "link" {
                        return false; // Remove link elements entirely
                    }

                    if elem_type == "script" {
                        if has_init_code(props) {
                            // Keep script but remove src/href
                            if let JsonValue::Object(obj) = props {
                                obj.remove("src");
                                obj.remove("href");
                            }
                            return true;
                        } else {
                            return false; // Remove script without init
                        }
                    }
                }
            }
        }
    }

    true // Keep by default
}

/// Clean Next.js streaming payloads using proper JSON parsing
/// Handles: self.__next_f.push([id, "JSON_STRING"])
fn clean_nextjs_streaming_payload(content: &str, remove_prebid: bool) -> Option<String> {
    let (prefix, chunk_id, json_escaped) = extract_nextjs_streaming_payload(content)?;

    // Unescape the JSON string
    let json_str = json_escaped.replace(r#"\""#, "\"");

    // The JSON string contains comma-separated React elements, not a single valid JSON
    // Wrap it in array brackets to parse
    let json_with_brackets = format!("[{}]", json_str);

    let mut elements: Vec<JsonValue> = serde_json::from_str(&json_with_brackets).ok()?;

    elements.retain_mut(|elem| process_react_element(elem, remove_prebid));

    if elements.is_empty() {
        return Some(format!(r#"{}{},"[]"]);"#, prefix, chunk_id));
    }

    let json_array = serde_json::to_string(&elements).ok()?;

    let json_str_rebuilt = &json_array[1..json_array.len() - 1];

    let json_escaped_rebuilt = json_str_rebuilt.replace('"', r#"\""#);

    // Reconstruct the full JavaScript
    Some(format!(
        r#"{}{},"{}"]);"#,
        prefix, chunk_id, json_escaped_rebuilt
    ))
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

    // Remove prebid-related elements from Next.js payloads
    if remove_prebid && rewritten.contains("prebid") && rewritten.contains(".js") {
        let mut handled = false;

        // Try serde-based approach for streaming payloads: self.__next_f.push([id, "JSON_STRING"])
        if rewritten.contains("__next_f") || rewritten.contains("__next_s") {
            if let Some(cleaned) = clean_nextjs_streaming_payload(&rewritten, true) {
                changed = true;
                rewritten = cleaned;
                handled = true;
            }
        }

        // Fallback: Handle [0, {...}] style payloads (autoblog.com pattern)
        // Also runs if serde approach failed
        if !handled {
            if let Ok(next_s_pattern) =
                Regex::new(r#"\[\d+\s*,\s*\{[^\]]*?prebid[^\]]*?\.js[^\]]*?\}\],?"#)
            {
                let new_value =
                    next_s_pattern.replace_all(&rewritten, |caps: &regex::Captures<'_>| {
                        let matched = &caps[0];
                        // Only remove if it doesn't contain init code
                        if matched.contains("pbjs=pbjs||{}") || matched.contains("pbjs.que") {
                            matched.to_string()
                        } else {
                            String::new()
                        }
                    });
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

    #[test]
    fn streamed_rewriter_handles_next_s_script_streaming() {
        // Real-world example from autoblog.com using self.__next_s for script streaming
        let payload = r#"(self.__next_s=self.__next_s||[]).push([0,{"children":"if(window.innerWidth>=768){var s=document.createElement('script');s.src='/js/prebid.min.js?v=2025-11-20-233540-a956a5e-008356';document.head.appendChild(s)}","id":"pbjs-bundle"}])"#;
        let rewriter = NextJsScriptRewriter::new(
            test_config_with_prebid_removal(),
            NextJsRewriteMode::Streamed,
        );
        let result = rewriter.rewrite(payload, &ctx("script"));

        match result {
            ScriptRewriteAction::Replace(value) => {
                assert!(
                    !value.contains("prebid.min.js"),
                    "Should remove prebid file references from __next_s payloads"
                );
                assert!(
                    !value.contains("/js/prebid.min.js"),
                    "Should remove full prebid path"
                );
                assert!(
                    value.contains("self.__next_s"),
                    "Should preserve __next_s structure"
                );
            }
            _ => panic!("Expected prebid file removal from __next_s payload"),
        }
    }

    #[test]
    fn streamed_rewriter_only_processes_next_payloads() {
        let rewriter = NextJsScriptRewriter::new(
            test_config_with_prebid_removal(),
            NextJsRewriteMode::Streamed,
        );

        // Non-Next.js script should be kept
        let regular_script = r#"console.log('hello'); var x = 123;"#;
        let result = rewriter.rewrite(regular_script, &ctx("script"));
        assert!(
            matches!(result, ScriptRewriteAction::Keep),
            "Should skip non-Next.js scripts"
        );

        // __next_f with content to rewrite should be processed
        let next_f =
            r#"self.__next_f.push([1, "{\"href\":\"https://origin.example.com/page\"}"]);"#;
        let result_f = rewriter.rewrite(next_f, &ctx("script"));
        assert!(
            matches!(result_f, ScriptRewriteAction::Replace(_)),
            "Should process __next_f payloads"
        );

        // __next_s with content to rewrite should be processed
        let next_s = r#"self.__next_s.push([0, {"children":"code","src":"/js/prebid.min.js"}]);"#;
        let result_s = rewriter.rewrite(next_s, &ctx("script"));
        assert!(
            matches!(result_s, ScriptRewriteAction::Replace(_)),
            "Should process __next_s payloads"
        );
    }
}
