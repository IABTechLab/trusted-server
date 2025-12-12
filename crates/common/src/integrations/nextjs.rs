use std::sync::Arc;

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
    _attributes: &[String], // Unused in blanket rewrite mode, kept for API compatibility
) -> Option<String> {
    if origin_host.is_empty() || request_host.is_empty() {
        return None;
    }

    let mut rewritten = content.to_string();
    let mut changed = false;

    // Blanket rewrite: Replace ALL occurrences of origin URLs with proxy URLs
    // This ensures consistency and prevents React hydration mismatches

    // Pattern 1: https://origin.example.com -> https://proxy.example.com
    let https_origin = format!("https://{}", origin_host);
    let https_replacement = format!("{}://{}", request_scheme, request_host);
    if rewritten.contains(&https_origin) {
        rewritten = rewritten.replace(&https_origin, &https_replacement);
        changed = true;
    }

    // Pattern 2: http://origin.example.com -> https://proxy.example.com (upgrade to https)
    let http_origin = format!("http://{}", origin_host);
    if rewritten.contains(&http_origin) {
        rewritten = rewritten.replace(&http_origin, &https_replacement);
        changed = true;
    }

    // Pattern 3: Escaped slashes - https:\/\/origin.example.com -> https:\/\/proxy.example.com
    if rewritten.contains(&format!("https:\\/\\/{}", origin_host)) {
        rewritten = rewritten.replace(
            &format!("https:\\/\\/{}", origin_host),
            &format!("https:\\/\\/{}", request_host),
        );
        changed = true;
    }

    // Pattern 4: Escaped slashes - http:\/\/origin.example.com -> https:\/\/proxy.example.com
    if rewritten.contains(&format!("http:\\/\\/{}", origin_host)) {
        rewritten = rewritten.replace(
            &format!("http:\\/\\/{}", origin_host),
            &format!("https:\\/\\/{}", request_host),
        );
        changed = true;
    }

    // Pattern 5: Protocol-relative - //origin.example.com -> //proxy.example.com
    let protocol_relative_origin = format!("//{}", origin_host);
    let protocol_relative_replacement = format!("//{}", request_host);
    if rewritten.contains(&protocol_relative_origin) {
        rewritten = rewritten.replace(&protocol_relative_origin, &protocol_relative_replacement);
        changed = true;
    }

    // Pattern 6: Protocol-relative with escaped slashes - \/\/origin.example.com -> \/\/proxy.example.com
    if rewritten.contains(&format!("\\/\\/{}", origin_host)) {
        rewritten = rewritten.replace(
            &format!("\\/\\/{}", origin_host),
            &format!("\\/\\/{}", request_host),
        );
        changed = true;
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
                // Blanket rewrite: ALL URLs are rewritten
                assert!(
                    value.contains(r#""fallbackHref":"https://ts.example.com/legacy""#),
                    "blanket rewrite should rewrite fallbackHref: {}",
                    value
                );
                assert!(
                    value.contains(r#""protoRelative":"//ts.example.com/assets/logo.png""#),
                    "blanket rewrite should rewrite protocol-relative URLs: {}",
                    value
                );
                // Origin should not appear anywhere
                assert!(
                    !value.contains("origin.example.com"),
                    "blanket rewrite should remove all origin URLs: {}",
                    value
                );
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

        // The key assertion: escaping must be preserved!
        assert!(
            rewritten.contains(r#""href":"https:\/\/ts.example.com\/page""#),
            "should preserve escaped slashes in href: {}",
            rewritten
        );
        assert!(
            rewritten.contains(r#""src":"https:\/\/ts.example.com\/script.js""#),
            "should preserve escaped slashes in src and upgrade http to https: {}",
            rewritten
        );
        assert!(
            !rewritten.contains("origin.example.com"),
            "should not contain origin domain: {}",
            rewritten
        );

        // Verify escape count is preserved
        let original_backslashes = content.matches('\\').count();
        let rewritten_backslashes = rewritten.matches('\\').count();
        assert_eq!(
            original_backslashes, rewritten_backslashes,
            "backslash count must be preserved (was {}, now {}): {}",
            original_backslashes, rewritten_backslashes, rewritten
        );
    }

    #[test]
    fn rewrites_urls_in_rsc_streaming_payload() {
        // Test realistic RSC streaming payload with escaped JSON inside JavaScript string
        // This simulates: self.__next_f.push([1,"{\"url\":\"https:\/\/origin.example.com\/api\"}"])
        let content =
            r#"self.__next_f.push([1,"{\"url\":\"https:\/\/origin.example.com\/api\"}"])"#;
        let rewritten = rewrite_nextjs_values(
            content,
            "origin.example.com",
            "ts.example.com",
            "https",
            &["url".into()],
        )
        .expect("should rewrite URLs in RSC streaming payload");

        // Must preserve the escaped slashes in the JSON
        assert!(
            rewritten.contains(r#"\"url\":\"https:\/\/ts.example.com\/api\""#),
            "should preserve escaped slashes in RSC payload: {}",
            rewritten
        );

        // Verify escape count is preserved (critical for JSON parsing)
        let original_backslashes = content.matches('\\').count();
        let rewritten_backslashes = rewritten.matches('\\').count();
        assert_eq!(
            original_backslashes, rewritten_backslashes,
            "backslash count must be preserved in RSC payload (was {}, now {}): {}",
            original_backslashes, rewritten_backslashes, rewritten
        );
    }

    #[test]
    fn blanket_rewrite_catches_all_url_fields() {
        // Test that blanket rewrite catches fields not in the rewrite_attributes list
        // This prevents React hydration mismatches from inconsistent URL rewriting
        let content = r#"{
            "url":"https://origin.example.com/news",
            "featured_image":"https://origin.example.com/.image/img.jpg",
            "favicon":"https://origin.example.com/favicon.ico",
            "siteBaseUrl":"https://origin.example.com",
            "source_url":"https://origin.example.com/source",
            "thumbnail":"http://origin.example.com/thumb.jpg",
            "logo":"//origin.example.com/logo.png"
        }"#;

        let rewritten = rewrite_nextjs_values(
            content,
            "origin.example.com",
            "ts.example.com",
            "https",
            &["url".into()], // Only "url" in attributes, but should catch ALL fields
        )
        .expect("should rewrite all URLs");

        // All URLs should be rewritten, regardless of field name
        assert!(
            rewritten.contains(r#""url":"https://ts.example.com/news""#),
            "should rewrite url field"
        );
        assert!(
            rewritten.contains(r#""featured_image":"https://ts.example.com/.image/img.jpg""#),
            "should rewrite featured_image field: {}",
            rewritten
        );
        assert!(
            rewritten.contains(r#""favicon":"https://ts.example.com/favicon.ico""#),
            "should rewrite favicon field: {}",
            rewritten
        );
        assert!(
            rewritten.contains(r#""siteBaseUrl":"https://ts.example.com""#),
            "should rewrite siteBaseUrl field: {}",
            rewritten
        );
        assert!(
            rewritten.contains(r#""source_url":"https://ts.example.com/source""#),
            "should rewrite source_url field: {}",
            rewritten
        );
        assert!(
            rewritten.contains(r#""thumbnail":"https://ts.example.com/thumb.jpg""#),
            "should rewrite thumbnail and upgrade http to https: {}",
            rewritten
        );
        assert!(
            rewritten.contains(r#""logo":"//ts.example.com/logo.png""#),
            "should rewrite protocol-relative logo: {}",
            rewritten
        );

        // Origin domain should not appear anywhere
        assert!(
            !rewritten.contains("origin.example.com"),
            "should not contain origin domain anywhere: {}",
            rewritten
        );
    }

    #[test]
    fn blanket_rewrite_handles_escaped_slashes_in_all_fields() {
        // Test that escaped slashes are preserved for any field, not just listed attributes
        let content = r#"{"featured_image":"https:\/\/origin.example.com\/img.jpg","siteBaseUrl":"https:\/\/origin.example.com"}"#;

        let rewritten = rewrite_nextjs_values(
            content,
            "origin.example.com",
            "ts.example.com",
            "https",
            &[], // Empty attribute list - blanket rewrite should still work
        )
        .expect("should rewrite with escaped slashes");

        assert!(
            rewritten.contains(r#""featured_image":"https:\/\/ts.example.com\/img.jpg""#),
            "should preserve escaped slashes in featured_image: {}",
            rewritten
        );
        assert!(
            rewritten.contains(r#""siteBaseUrl":"https:\/\/ts.example.com""#),
            "should preserve escaped slashes in siteBaseUrl: {}",
            rewritten
        );

        // Verify escape count preserved
        let original_backslashes = content.matches('\\').count();
        let rewritten_backslashes = rewritten.matches('\\').count();
        assert_eq!(
            original_backslashes, rewritten_backslashes,
            "backslash count must be preserved"
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
        // Blanket rewrite: ALL fields with URLs are rewritten, not just "href"
        assert!(
            processed.contains(r#""fallbackHref":"https://test.example.com/legacy""#),
            "should rewrite fallbackHref field with blanket rewrite: {}",
            processed
        );
        assert!(
            processed.contains(r#""protoRelative":"//test.example.com/assets/logo.png""#),
            "should rewrite protocol-relative URLs in all fields: {}",
            processed
        );
        // Origin domain should not appear anywhere due to blanket rewrite
        assert!(
            !processed.contains("origin.example.com"),
            "should remove ALL origin URLs with blanket rewrite: {}",
            processed
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
        // Blanket rewrite: ALL URLs are rewritten
        assert!(
            processed.contains("\"dataHost\":\"https://test.example.com/api\""),
            "should rewrite ALL fields with blanket rewrite: {}",
            processed
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
