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
    vec![
        "href".to_string(),
        "link".to_string(),
        "url".to_string(),
        "src".to_string(),    // For scripts/images/iframes
        "action".to_string(), // For form actions
        "poster".to_string(), // For video posters
    ]
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
) -> Option<String> {
    if origin_host.is_empty() || request_host.is_empty() || attributes.is_empty() {
        return None;
    }

    let mut rewritten = content.to_string();
    let mut changed = false;
    let escaped_origin = escape(origin_host);
    let replacement_scheme = format!("{}://{}", request_scheme, request_host);

    for attribute in attributes {
        let escaped_attr = escape(attribute);
        let pattern = format!(
            r#"(?P<prefix>(?:\\*")?{attr}(?:\\*")?:\\*")(?P<scheme>https?:\\?/\\?/|\\?/\\?/){origin}"#,
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

    changed.then_some(rewritten)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::html_processor::{create_html_processor, HtmlProcessorConfig};
    use crate::integrations::{IntegrationRegistry, IntegrationScriptContext, ScriptRewriteAction};
    use crate::streaming_processor::{Compression, PipelineConfig, StreamingPipeline};
    use crate::test_support::tests::create_test_settings;
    use serde_json::json;
    use std::io::Cursor;

    fn test_config() -> Arc<NextJsIntegrationConfig> {
        Arc::new(NextJsIntegrationConfig {
            enabled: true,
            rewrite_attributes: vec![
                "href".into(),
                "link".into(),
                "url".into(),
                "src".into(),
                "action".into(),
                "poster".into(),
            ],
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
    fn structured_rewriter_handles_escaped_forward_slashes() {
        let payload = r#"{"props":{"pageProps":{"href":"https:\/\/origin.example.com\/page","src":"https:\/\/origin.example.com\/script.js","link":"http:\/\/origin.example.com\/api"}}}"#;
        let rewriter = NextJsScriptRewriter::new(test_config(), NextJsRewriteMode::Structured);
        let result = rewriter.rewrite(payload, &ctx("script#__NEXT_DATA__"));

        match result {
            ScriptRewriteAction::Replace(value) => {
                // When input has escaped slashes, output preserves them
                assert!(
                    value.contains("ts.example.com") && value.contains("page"),
                    "should rewrite escaped https href to ts.example.com: {}",
                    value
                );
                assert!(
                    value.contains("ts.example.com") && value.contains("script.js"),
                    "should rewrite escaped https src to ts.example.com: {}",
                    value
                );
                assert!(
                    value.contains("ts.example.com") && value.contains("api"),
                    "should rewrite escaped http link to ts.example.com: {}",
                    value
                );
                assert!(
                    !value.contains("origin.example.com"),
                    "should not contain origin domain: {}",
                    value
                );
            }
            _ => panic!("Expected rewrite to update payload with escaped slashes"),
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
    fn streamed_rewriter_handles_escaped_forward_slashes() {
        let rewriter = NextJsScriptRewriter::new(test_config(), NextJsRewriteMode::Streamed);

        // Test with escaped forward slashes in URLs
        // Note: using regular string (not raw) to properly represent the escaping
        // This simulates JSON where forward slashes are escaped: https:\/\/domain.com
        let payload = "self.__next_f.push([1, \"\\\"src\\\":\\\"https:\\/\\/origin.example.com\\/app.js\\\"\"]);";

        let rewritten = rewriter.rewrite(payload, &ctx("script"));
        match rewritten {
            ScriptRewriteAction::Replace(value) => {
                // The rewriter should handle escaped slashes and produce output with them rewritten
                assert!(
                    value.contains("ts.example.com"),
                    "should rewrite to ts.example.com in streamed payload: {}",
                    value
                );
                // Make sure origin is gone
                assert!(
                    !value.contains("origin.example.com"),
                    "should not contain origin domain: {}",
                    value
                );
            }
            ScriptRewriteAction::Keep => {
                // If Keep is returned, the content didn't change. Let's see what's in the payload.
                panic!("Expected rewrite but got Keep. Payload was: {:?}", payload);
            }
            _ => panic!("Expected Replace action for streamed payload with escaped slashes"),
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

        assert!(rewritten.contains(r#""link":"//ts.example.com/image.png""#));
    }

    #[test]
    fn rewrites_src_action_and_poster_attributes() {
        let content = r#"{"props":{"pageProps":{"script":{"src":"https://origin.example.com/app.js"},"form":{"action":"https://origin.example.com/submit"},"video":{"poster":"https://origin.example.com/thumb.jpg"}}}}"#;
        let rewritten = rewrite_nextjs_values(
            content,
            "origin.example.com",
            "ts.example.com",
            "https",
            &["src".into(), "action".into(), "poster".into()],
        )
        .expect("should rewrite src, action, and poster attributes");

        assert!(
            rewritten.contains(r#""src":"https://ts.example.com/app.js""#),
            "should rewrite src attribute"
        );
        assert!(
            rewritten.contains(r#""action":"https://ts.example.com/submit""#),
            "should rewrite action attribute"
        );
        assert!(
            rewritten.contains(r#""poster":"https://ts.example.com/thumb.jpg""#),
            "should rewrite poster attribute"
        );
    }

    #[test]
    fn rewrites_urls_with_escaped_forward_slashes() {
        // Test the core rewrite function with escaped forward slashes
        let content = r#"{"href":"https:\/\/origin.example.com\/page","src":"http:\/\/origin.example.com\/script.js"}"#;
        let rewritten = rewrite_nextjs_values(
            content,
            "origin.example.com",
            "ts.example.com",
            "https",
            &["href".into(), "src".into()],
        )
        .expect("should rewrite URLs with escaped slashes");

        assert!(
            rewritten.contains("ts.example.com"),
            "should contain rewritten domain: {}",
            rewritten
        );
        assert!(
            !rewritten.contains("origin.example.com"),
            "should not contain origin domain: {}",
            rewritten
        );
    }

    fn config_from_settings(
        settings: &Settings,
        registry: &IntegrationRegistry,
    ) -> HtmlProcessorConfig {
        HtmlProcessorConfig::from_settings(
            settings,
            registry,
            "origin.example.com",
            "test.example.com",
            "https",
        )
    }

    #[test]
    fn html_processor_rewrites_nextjs_script_when_enabled() {
        let html = r#"<html><body>
            <script id="__NEXT_DATA__" type="application/json">
                {"props":{"pageProps":{"primary":{"href":"https://origin.example.com/reviews"},"secondary":{"href":"http://origin.example.com/sign-in"},"fallbackHref":"http://origin.example.com/legacy","protoRelative":"//origin.example.com/assets/logo.png"}}}
            </script>
        </body></html>"#;

        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config(
                "nextjs",
                &json!({
                    "enabled": true,
                    "rewrite_attributes": ["href", "link", "url"],
                }),
            )
            .expect("should update nextjs config");
        let registry = IntegrationRegistry::new(&settings);
        let config = config_from_settings(&settings, &registry);
        let processor = create_html_processor(config);
        let pipeline_config = PipelineConfig {
            input_compression: Compression::None,
            output_compression: Compression::None,
            chunk_size: 8192,
        };
        let mut pipeline = StreamingPipeline::new(pipeline_config, processor);

        let mut output = Vec::new();
        pipeline
            .process(Cursor::new(html.as_bytes()), &mut output)
            .unwrap();
        let processed = String::from_utf8_lossy(&output);

        assert!(
            processed.contains(r#""href":"https://test.example.com/reviews""#),
            "should rewrite https Next.js href values"
        );
        assert!(
            processed.contains(r#""href":"https://test.example.com/sign-in""#),
            "should rewrite http Next.js href values"
        );
        assert!(
            processed.contains(r#""fallbackHref":"http://origin.example.com/legacy""#),
            "should leave other fields untouched"
        );
        assert!(
            processed.contains(r#""protoRelative":"//origin.example.com/assets/logo.png""#),
            "should not rewrite non-href keys"
        );
        assert!(
            !processed.contains("\"href\":\"https://origin.example.com/reviews\""),
            "should remove origin https href"
        );
        assert!(
            !processed.contains("\"href\":\"http://origin.example.com/sign-in\""),
            "should remove origin http href"
        );
    }

    #[test]
    fn html_processor_rewrites_nextjs_stream_payload() {
        let html = r#"<html><body>
            <script>
                self.__next_f.push([1,"chunk", "prefix {\"inner\":\"value\"} \\\"href\\\":\\\"http://origin.example.com/dashboard\\\", \\\"link\\\":\\\"https://origin.example.com/api-test\\\" suffix", {"href":"http://origin.example.com/secondary","dataHost":"https://origin.example.com/api"}]);
            </script>
        </body></html>"#;

        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config(
                "nextjs",
                &json!({
                    "enabled": true,
                    "rewrite_attributes": ["href", "link", "url"],
                }),
            )
            .expect("should update nextjs config");
        let registry = IntegrationRegistry::new(&settings);
        let config = config_from_settings(&settings, &registry);
        let processor = create_html_processor(config);
        let pipeline_config = PipelineConfig {
            input_compression: Compression::None,
            output_compression: Compression::None,
            chunk_size: 8192,
        };
        let mut pipeline = StreamingPipeline::new(pipeline_config, processor);

        let mut output = Vec::new();
        pipeline
            .process(Cursor::new(html.as_bytes()), &mut output)
            .unwrap();
        let processed = String::from_utf8_lossy(&output);
        let normalized = processed.replace('\\', "");
        assert!(
            normalized.contains("\"href\":\"https://test.example.com/dashboard\""),
            "should rewrite escaped href sequences inside streamed payloads: {}",
            normalized
        );
        assert!(
            normalized.contains("\"href\":\"https://test.example.com/secondary\""),
            "should rewrite plain href attributes inside streamed payloads"
        );
        assert!(
            normalized.contains("\"link\":\"https://test.example.com/api-test\""),
            "should rewrite additional configured attributes like link"
        );
        assert!(
            processed.contains("\"dataHost\":\"https://origin.example.com/api\""),
            "should leave non-href properties untouched"
        );
    }

    #[test]
    fn register_respects_enabled_flag() {
        let settings = create_test_settings();
        let registration = register(&settings);

        assert!(
            registration.is_none(),
            "should skip registration when integration is disabled"
        );
    }
}
