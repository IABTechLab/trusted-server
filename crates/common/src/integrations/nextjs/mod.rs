use std::sync::Arc;

use serde::{Deserialize, Serialize};
use validator::Validate;

use crate::integrations::IntegrationRegistration;
use crate::settings::{IntegrationConfig, Settings};

const NEXTJS_INTEGRATION_ID: &str = "nextjs";

mod html_post_process;
mod rsc;
mod rsc_placeholders;
mod script_rewriter;
mod shared;

// Re-export deprecated legacy functions for backward compatibility.
// Production code should use the placeholder-based approach via NextJsHtmlPostProcessor.
#[allow(deprecated)]
pub use html_post_process::{post_process_rsc_html, post_process_rsc_html_in_place};
pub use rsc::rewrite_rsc_scripts_combined;

use html_post_process::NextJsHtmlPostProcessor;
use rsc_placeholders::NextJsRscPlaceholderRewriter;
use script_rewriter::NextJsNextDataRewriter;

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
    #[serde(default = "default_max_combined_payload_bytes")]
    pub max_combined_payload_bytes: usize,
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

fn default_max_combined_payload_bytes() -> usize {
    10 * 1024 * 1024
}

#[must_use]
pub fn register(settings: &Settings) -> Option<IntegrationRegistration> {
    let config = match build(settings) {
        Some(config) => {
            log::info!(
                "NextJS integration registered: enabled={}, rewrite_attributes={:?}, max_combined_payload_bytes={}",
                config.enabled,
                config.rewrite_attributes,
                config.max_combined_payload_bytes
            );
            config
        }
        None => {
            log::info!("NextJS integration not registered (disabled or missing config)");
            return None;
        }
    };

    // Register a structured (Pages Router __NEXT_DATA__) rewriter.
    let structured = Arc::new(NextJsNextDataRewriter::new(config.clone()));

    // Insert placeholders for App Router RSC payload scripts during the initial HTML rewrite pass,
    // then substitute them during post-processing without re-parsing HTML.
    let placeholders = Arc::new(NextJsRscPlaceholderRewriter::new(config.clone()));

    // Register post-processor for cross-script RSC T-chunks
    let post_processor = Arc::new(NextJsHtmlPostProcessor::new(config.clone()));

    let builder = IntegrationRegistration::builder(NEXTJS_INTEGRATION_ID)
        .with_script_rewriter(structured)
        .with_script_rewriter(placeholders)
        .with_html_post_processor(post_processor);

    Some(builder.build())
}

fn build(settings: &Settings) -> Option<Arc<NextJsIntegrationConfig>> {
    let config = settings
        .integration_config::<NextJsIntegrationConfig>(NEXTJS_INTEGRATION_ID)
        .ok()
        .flatten()?;
    Some(Arc::new(config))
}

#[cfg(test)]
mod tests {
    use super::rsc_placeholders::RSC_PAYLOAD_PLACEHOLDER_PREFIX;
    use super::*;
    use crate::html_processor::{create_html_processor, HtmlProcessorConfig};
    use crate::integrations::IntegrationRegistry;
    use crate::streaming_processor::{Compression, PipelineConfig, StreamingPipeline};
    use crate::test_support::tests::create_test_settings;
    use serde_json::json;
    use std::io::Cursor;

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
        let registry = IntegrationRegistry::new(&settings).expect("should create registry");
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

        // Note: URLs may have padding characters for length preservation
        assert!(
            processed.contains("test.example.com") && processed.contains("/reviews"),
            "should rewrite https Next.js href values to test.example.com"
        );
        assert!(
            processed.contains("test.example.com") && processed.contains("/sign-in"),
            "should rewrite http Next.js href values to test.example.com"
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
    fn html_processor_rewrites_rsc_stream_payload_with_length_preservation() {
        // RSC payloads (self.__next_f.push) are rewritten via post-processing.
        // The streaming phase skips RSC push scripts, and the HTML post-processor handles them
        // at end-of-document to correctly handle cross-script T-chunks.
        let html = r#"<html><body>
            <script>self.__next_f.push([1,"prefix {\"inner\":\"value\"} \\\"href\\\":\\\"http://origin.example.com/dashboard\\\", \\\"link\\\":\\\"https://origin.example.com/api-test\\\" suffix"])</script>
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
        let registry = IntegrationRegistry::new(&settings).expect("should create registry");
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

        let final_html = String::from_utf8_lossy(&output);

        // RSC payloads should be rewritten via end-of-document post-processing
        assert!(
            final_html.contains("test.example.com"),
            "RSC stream payloads should be rewritten to proxy host via post-processing. Output: {}",
            final_html
        );
        assert!(
            !final_html.contains(RSC_PAYLOAD_PLACEHOLDER_PREFIX),
            "RSC placeholder markers should not appear in final HTML. Output: {}",
            final_html
        );
    }

    #[test]
    fn html_processor_rewrites_rsc_stream_payload_with_chunked_input() {
        // RSC payloads are rewritten via post-processing, even with chunked streaming input
        let html = r#"<html><body>
<script>self.__next_f.push([1,'{"href":"https://origin.example.com/app","url":"http://origin.example.com/api"}'])</script>
        </body></html>"#;

        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config(
                "nextjs",
                &json!({
                    "enabled": true,
                    "rewrite_attributes": ["href", "url"],
                }),
            )
            .expect("should update nextjs config");
        let registry = IntegrationRegistry::new(&settings).expect("should create registry");
        let config = config_from_settings(&settings, &registry);
        let processor = create_html_processor(config);
        let pipeline_config = PipelineConfig {
            input_compression: Compression::None,
            output_compression: Compression::None,
            chunk_size: 32,
        };
        let mut pipeline = StreamingPipeline::new(pipeline_config, processor);

        let mut output = Vec::new();
        pipeline
            .process(Cursor::new(html.as_bytes()), &mut output)
            .unwrap();

        let final_html = String::from_utf8_lossy(&output);

        // RSC payloads should be rewritten via end-of-document post-processing
        assert!(
            final_html.contains("test.example.com"),
            "RSC stream payloads should be rewritten to proxy host with chunked input. Output: {}",
            final_html
        );
        assert!(
            !final_html.contains(RSC_PAYLOAD_PLACEHOLDER_PREFIX),
            "RSC placeholder markers should not appear in final HTML. Output: {}",
            final_html
        );
    }

    #[test]
    fn html_processor_respects_max_combined_payload_bytes() {
        // When the combined payload size exceeds `max_combined_payload_bytes` and the document
        // contains cross-script T-chunks, we skip post-processing to avoid breaking hydration.
        let html = r#"<html><body>
<script>self.__next_f.push([1,"other:data\n1a:T40,partial content"])</script>
<script>self.__next_f.push([1," with https://origin.example.com/page goes here"])</script>
</body></html>"#;

        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config(
                "nextjs",
                &json!({
                    "enabled": true,
                    "rewrite_attributes": ["href", "link", "url"],
                    "max_combined_payload_bytes": 1,
                }),
            )
            .expect("should update nextjs config");
        let registry = IntegrationRegistry::new(&settings).expect("should create registry");
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

        let final_html = String::from_utf8_lossy(&output);

        assert!(
            final_html.contains("https://origin.example.com/page"),
            "Origin URL should remain when rewrite is skipped due to size limit. Output: {}",
            final_html
        );
        assert!(
            !final_html.contains("test.example.com"),
            "Proxy host should not be introduced when rewrite is skipped. Output: {}",
            final_html
        );
        assert!(
            !final_html.contains(RSC_PAYLOAD_PLACEHOLDER_PREFIX),
            "RSC placeholder markers should not appear in final HTML. Output: {}",
            final_html
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

    #[test]
    fn html_processor_rewrites_rsc_payloads_with_length_preservation() {
        // RSC payloads (self.__next_f.push) are rewritten via post-processing.
        // This allows navigation to stay on proxy while correctly handling cross-script T-chunks.

        let html = r#"<html><body>
<script>self.__next_f.push([1,'458:{"ID":879000,"title":"Makes","url":"https://origin.example.com/makes","children":"$45a"}\n442:["$443"]'])</script>
</body></html>"#;

        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config(
                "nextjs",
                &json!({
                    "enabled": true,
                    "rewrite_attributes": ["url"],
                }),
            )
            .expect("should update nextjs config");

        let registry = IntegrationRegistry::new(&settings).expect("should create registry");
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
        let final_html = String::from_utf8_lossy(&output);

        // RSC payloads should be rewritten via post-processing
        assert!(
            final_html.contains("test.example.com"),
            "RSC payload URLs should be rewritten to proxy host. Output: {}",
            final_html
        );

        // Verify the RSC payload structure is preserved
        assert!(
            final_html.contains(r#""ID":879000"#),
            "RSC payload ID should be preserved"
        );
        assert!(
            final_html.contains(r#""title":"Makes""#),
            "RSC payload title should be preserved"
        );
        assert!(
            final_html.contains(r#""children":"$45a""#),
            "RSC payload children reference should be preserved"
        );

        // Verify \n separators are preserved (crucial for RSC parsing)
        assert!(
            final_html.contains(r#"\n442:"#),
            "RSC record separator \\n should be preserved. Output: {}",
            final_html
        );
        assert!(
            !final_html.contains(RSC_PAYLOAD_PLACEHOLDER_PREFIX),
            "RSC placeholder markers should not appear in final HTML. Output: {}",
            final_html
        );
    }

    #[test]
    fn html_processor_preserves_non_rsc_scripts_with_chunked_streaming() {
        // Regression test: ensure non-RSC scripts are preserved when streamed alongside RSC scripts.
        // With small chunk sizes, scripts get fragmented and the buffering logic must correctly
        // handle non-RSC scripts without corrupting them.
        let html = r#"<html><body>
<script>console.log("hello world");</script>
<script>self.__next_f.push([1,'{"url":"https://origin.example.com/page"}'])</script>
<script>window.analytics = { track: function(e) { console.log(e); } };</script>
</body></html>"#;

        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config(
                "nextjs",
                &json!({
                    "enabled": true,
                    "rewrite_attributes": ["url"],
                }),
            )
            .expect("should update nextjs config");
        let registry = IntegrationRegistry::new(&settings).expect("should create registry");
        let config = config_from_settings(&settings, &registry);
        let processor = create_html_processor(config);
        // Use small chunk size to force fragmentation
        let pipeline_config = PipelineConfig {
            input_compression: Compression::None,
            output_compression: Compression::None,
            chunk_size: 16,
        };
        let mut pipeline = StreamingPipeline::new(pipeline_config, processor);

        let mut output = Vec::new();
        pipeline
            .process(Cursor::new(html.as_bytes()), &mut output)
            .unwrap();
        let final_html = String::from_utf8_lossy(&output);

        // Non-RSC scripts should be preserved
        assert!(
            final_html.contains(r#"console.log("hello world");"#),
            "First non-RSC script should be preserved intact. Output: {}",
            final_html
        );
        assert!(
            final_html.contains("window.analytics"),
            "Third non-RSC script should be preserved. Output: {}",
            final_html
        );
        assert!(
            final_html.contains("track: function(e)"),
            "Third non-RSC script content should be intact. Output: {}",
            final_html
        );

        // RSC scripts should be rewritten
        assert!(
            final_html.contains("test.example.com"),
            "RSC URL should be rewritten. Output: {}",
            final_html
        );
        assert!(
            !final_html.contains(RSC_PAYLOAD_PLACEHOLDER_PREFIX),
            "No placeholders should remain. Output: {}",
            final_html
        );
    }
}
