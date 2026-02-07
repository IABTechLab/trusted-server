//! Simplified HTML processor that combines URL replacement and Prebid injection
//!
//! This module provides a `StreamProcessor` implementation for HTML content.
use std::cell::Cell;
use std::io;
use std::rc::Rc;
use std::sync::Arc;

use lol_html::{element, html_content::ContentType, text, Settings as RewriterSettings};

use crate::integrations::{
    AttributeRewriteOutcome, IntegrationAttributeContext, IntegrationDocumentState,
    IntegrationHtmlContext, IntegrationHtmlPostProcessor, IntegrationRegistry,
    IntegrationScriptContext, ScriptRewriteAction,
};
use crate::settings::Settings;
use crate::streaming_processor::{HtmlRewriterAdapter, StreamProcessor};
use crate::tsjs;

struct HtmlWithPostProcessing {
    inner: HtmlRewriterAdapter,
    post_processors: Vec<Arc<dyn IntegrationHtmlPostProcessor>>,
    origin_host: String,
    request_host: String,
    request_scheme: String,
    document_state: IntegrationDocumentState,
}

impl StreamProcessor for HtmlWithPostProcessing {
    fn process_chunk(&mut self, chunk: &[u8], is_last: bool) -> Result<Vec<u8>, io::Error> {
        let output = self.inner.process_chunk(chunk, is_last)?;
        if !is_last || output.is_empty() || self.post_processors.is_empty() {
            return Ok(output);
        }

        let Ok(output_str) = std::str::from_utf8(&output) else {
            return Ok(output);
        };

        let ctx = IntegrationHtmlContext {
            request_host: &self.request_host,
            request_scheme: &self.request_scheme,
            origin_host: &self.origin_host,
            document_state: &self.document_state,
        };

        // Preflight to avoid allocating a `String` unless at least one post-processor wants to run.
        if !self
            .post_processors
            .iter()
            .any(|p| p.should_process(output_str, &ctx))
        {
            return Ok(output);
        }

        let mut html = String::from_utf8(output).map_err(|e| {
            io::Error::other(format!(
                "HTML post-processing expected valid UTF-8 output: {e}"
            ))
        })?;

        let mut changed = false;
        for processor in &self.post_processors {
            if processor.should_process(&html, &ctx) {
                changed |= processor.post_process(&mut html, &ctx);
            }
        }

        if changed {
            log::debug!(
                "HTML post-processing complete: origin_host={}, output_len={}",
                self.origin_host,
                html.len()
            );
        }

        Ok(html.into_bytes())
    }

    fn reset(&mut self) {
        self.inner.reset();
        self.document_state.clear();
    }
}

/// Configuration for HTML processing
#[derive(Clone)]
pub struct HtmlProcessorConfig {
    pub origin_host: String,
    pub request_host: String,
    pub request_scheme: String,
    pub integrations: IntegrationRegistry,
}

impl HtmlProcessorConfig {
    /// Create from settings and request parameters
    #[must_use]
    pub fn from_settings(
        _settings: &Settings,
        integrations: &IntegrationRegistry,
        origin_host: &str,
        request_host: &str,
        request_scheme: &str,
    ) -> Self {
        Self {
            origin_host: origin_host.to_string(),
            request_host: request_host.to_string(),
            request_scheme: request_scheme.to_string(),
            integrations: integrations.clone(),
        }
    }
}

/// Create an HTML processor with URL replacement and optional Prebid injection
#[must_use]
pub fn create_html_processor(config: HtmlProcessorConfig) -> impl StreamProcessor {
    let post_processors = config.integrations.html_post_processors();
    let document_state = IntegrationDocumentState::default();

    // Simplified URL patterns structure - stores only core data and generates variants on-demand
    struct UrlPatterns {
        origin_host: String,
        request_host: String,
        request_scheme: String,
    }

    impl UrlPatterns {
        fn https_origin(&self) -> String {
            format!("https://{}", self.origin_host)
        }

        fn http_origin(&self) -> String {
            format!("http://{}", self.origin_host)
        }

        fn protocol_relative_origin(&self) -> String {
            format!("//{}", self.origin_host)
        }

        fn replacement_url(&self) -> String {
            format!("{}://{}", self.request_scheme, self.request_host)
        }

        fn protocol_relative_replacement(&self) -> String {
            format!("//{}", self.request_host)
        }

        fn rewrite_url_value(&self, value: &str) -> Option<String> {
            if !value.contains(&self.origin_host) {
                return None;
            }

            let https_origin = self.https_origin();
            let http_origin = self.http_origin();
            let protocol_relative_origin = self.protocol_relative_origin();
            let replacement_url = self.replacement_url();
            let protocol_relative_replacement = self.protocol_relative_replacement();

            let mut rewritten = value
                .replace(&https_origin, &replacement_url)
                .replace(&http_origin, &replacement_url)
                .replace(&protocol_relative_origin, &protocol_relative_replacement);

            if rewritten.starts_with(&self.origin_host) {
                let suffix = &rewritten[self.origin_host.len()..];
                let boundary_ok = suffix.is_empty()
                    || matches!(
                        suffix.as_bytes().first(),
                        Some(b'/') | Some(b'?') | Some(b'#')
                    );
                if boundary_ok {
                    rewritten = format!("{}{}", self.request_host, suffix);
                }
            }

            (rewritten != value).then_some(rewritten)
        }
    }

    let patterns = Rc::new(UrlPatterns {
        origin_host: config.origin_host.clone(),
        request_host: config.request_host.clone(),
        request_scheme: config.request_scheme.clone(),
    });

    let injected_tsjs = Rc::new(Cell::new(false));
    let integration_registry = config.integrations.clone();
    let script_rewriters = integration_registry.script_rewriters();

    let mut element_content_handlers = vec![
        // Inject unified tsjs bundle once at the start of <head>
        element!("head", {
            let injected_tsjs = injected_tsjs.clone();
            move |el| {
                if !injected_tsjs.get() {
                    let loader = tsjs::unified_script_tag();
                    el.prepend(&loader, ContentType::Html);
                    injected_tsjs.set(true);
                }
                Ok(())
            }
        }),
        // Replace URLs in href attributes
        element!("[href]", {
            let patterns = patterns.clone();
            let integrations = integration_registry.clone();
            move |el| {
                if let Some(mut href) = el.get_attribute("href") {
                    let original_href = href.clone();
                    if let Some(rewritten) = patterns.rewrite_url_value(&href) {
                        href = rewritten;
                    }

                    match integrations.rewrite_attribute(
                        "href",
                        &href,
                        &IntegrationAttributeContext {
                            attribute_name: "href",
                            request_host: &patterns.request_host,
                            request_scheme: &patterns.request_scheme,
                            origin_host: &patterns.origin_host,
                        },
                    ) {
                        AttributeRewriteOutcome::Unchanged => {}
                        AttributeRewriteOutcome::Replaced(integration_href) => {
                            href = integration_href;
                        }
                        AttributeRewriteOutcome::RemoveElement => {
                            el.remove();
                            return Ok(());
                        }
                    }

                    if href != original_href {
                        el.set_attribute("href", &href)?;
                    }
                }
                Ok(())
            }
        }),
        // Replace URLs in src attributes
        element!("[src]", {
            let patterns = patterns.clone();
            let integrations = integration_registry.clone();
            move |el| {
                if let Some(mut src) = el.get_attribute("src") {
                    let original_src = src.clone();
                    if let Some(rewritten) = patterns.rewrite_url_value(&src) {
                        src = rewritten;
                    }
                    match integrations.rewrite_attribute(
                        "src",
                        &src,
                        &IntegrationAttributeContext {
                            attribute_name: "src",
                            request_host: &patterns.request_host,
                            request_scheme: &patterns.request_scheme,
                            origin_host: &patterns.origin_host,
                        },
                    ) {
                        AttributeRewriteOutcome::Unchanged => {}
                        AttributeRewriteOutcome::Replaced(integration_src) => {
                            src = integration_src;
                        }
                        AttributeRewriteOutcome::RemoveElement => {
                            el.remove();
                            return Ok(());
                        }
                    }

                    if src != original_src {
                        el.set_attribute("src", &src)?;
                    }
                }
                Ok(())
            }
        }),
        // Replace URLs in action attributes
        element!("[action]", {
            let patterns = patterns.clone();
            let integrations = integration_registry.clone();
            move |el| {
                if let Some(mut action) = el.get_attribute("action") {
                    let original_action = action.clone();
                    if let Some(rewritten) = patterns.rewrite_url_value(&action) {
                        action = rewritten;
                    }

                    match integrations.rewrite_attribute(
                        "action",
                        &action,
                        &IntegrationAttributeContext {
                            attribute_name: "action",
                            request_host: &patterns.request_host,
                            request_scheme: &patterns.request_scheme,
                            origin_host: &patterns.origin_host,
                        },
                    ) {
                        AttributeRewriteOutcome::Unchanged => {}
                        AttributeRewriteOutcome::Replaced(integration_action) => {
                            action = integration_action;
                        }
                        AttributeRewriteOutcome::RemoveElement => {
                            el.remove();
                            return Ok(());
                        }
                    }

                    if action != original_action {
                        el.set_attribute("action", &action)?;
                    }
                }
                Ok(())
            }
        }),
        // Replace URLs in srcset attributes (for responsive images)
        element!("[srcset]", {
            let patterns = patterns.clone();
            let integrations = integration_registry.clone();
            move |el| {
                if let Some(mut srcset) = el.get_attribute("srcset") {
                    let original_srcset = srcset.clone();
                    let new_srcset = srcset
                        .replace(&patterns.https_origin(), &patterns.replacement_url())
                        .replace(&patterns.http_origin(), &patterns.replacement_url())
                        .replace(
                            &patterns.protocol_relative_origin(),
                            &patterns.protocol_relative_replacement(),
                        )
                        .replace(&patterns.origin_host, &patterns.request_host);
                    if new_srcset != srcset {
                        srcset = new_srcset;
                    }

                    match integrations.rewrite_attribute(
                        "srcset",
                        &srcset,
                        &IntegrationAttributeContext {
                            attribute_name: "srcset",
                            request_host: &patterns.request_host,
                            request_scheme: &patterns.request_scheme,
                            origin_host: &patterns.origin_host,
                        },
                    ) {
                        AttributeRewriteOutcome::Unchanged => {}
                        AttributeRewriteOutcome::Replaced(integration_srcset) => {
                            srcset = integration_srcset;
                        }
                        AttributeRewriteOutcome::RemoveElement => {
                            el.remove();
                            return Ok(());
                        }
                    }

                    if srcset != original_srcset {
                        el.set_attribute("srcset", &srcset)?;
                    }
                }
                Ok(())
            }
        }),
        // Replace URLs in imagesrcset attributes (for link preload)
        element!("[imagesrcset]", {
            let patterns = patterns.clone();
            let integrations = integration_registry.clone();
            move |el| {
                if let Some(mut imagesrcset) = el.get_attribute("imagesrcset") {
                    let original_imagesrcset = imagesrcset.clone();
                    let new_imagesrcset = imagesrcset
                        .replace(&patterns.https_origin(), &patterns.replacement_url())
                        .replace(&patterns.http_origin(), &patterns.replacement_url())
                        .replace(
                            &patterns.protocol_relative_origin(),
                            &patterns.protocol_relative_replacement(),
                        );
                    if new_imagesrcset != imagesrcset {
                        imagesrcset = new_imagesrcset;
                    }

                    match integrations.rewrite_attribute(
                        "imagesrcset",
                        &imagesrcset,
                        &IntegrationAttributeContext {
                            attribute_name: "imagesrcset",
                            request_host: &patterns.request_host,
                            request_scheme: &patterns.request_scheme,
                            origin_host: &patterns.origin_host,
                        },
                    ) {
                        AttributeRewriteOutcome::Unchanged => {}
                        AttributeRewriteOutcome::Replaced(integration_imagesrcset) => {
                            imagesrcset = integration_imagesrcset;
                        }
                        AttributeRewriteOutcome::RemoveElement => {
                            el.remove();
                            return Ok(());
                        }
                    }

                    if imagesrcset != original_imagesrcset {
                        el.set_attribute("imagesrcset", &imagesrcset)?;
                    }
                }
                Ok(())
            }
        }),
    ];

    for script_rewriter in script_rewriters {
        let selector = script_rewriter.selector();
        let rewriter = script_rewriter.clone();
        let patterns = patterns.clone();
        let document_state = document_state.clone();
        element_content_handlers.push(text!(selector, {
            let rewriter = rewriter.clone();
            let patterns = patterns.clone();
            let document_state = document_state.clone();
            move |text| {
                let ctx = IntegrationScriptContext {
                    selector,
                    request_host: &patterns.request_host,
                    request_scheme: &patterns.request_scheme,
                    origin_host: &patterns.origin_host,
                    is_last_in_text_node: text.last_in_text_node(),
                    document_state: &document_state,
                };
                match rewriter.rewrite(text.as_str(), &ctx) {
                    ScriptRewriteAction::Keep => {}
                    ScriptRewriteAction::Replace(rewritten) => {
                        text.replace(&rewritten, ContentType::Text);
                    }
                    ScriptRewriteAction::RemoveNode => {
                        text.remove();
                    }
                }
                Ok(())
            }
        }));
    }

    let rewriter_settings = RewriterSettings {
        element_content_handlers,
        ..RewriterSettings::default()
    };

    HtmlWithPostProcessing {
        inner: HtmlRewriterAdapter::new(rewriter_settings),
        post_processors,
        origin_host: config.origin_host,
        request_host: config.request_host,
        request_scheme: config.request_scheme,
        document_state,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integrations::{
        AttributeRewriteAction, IntegrationAttributeContext, IntegrationAttributeRewriter,
    };
    use crate::streaming_processor::{Compression, PipelineConfig, StreamingPipeline};
    use crate::test_support::tests::create_test_settings;
    use serde_json::json;
    use std::io::Cursor;
    use std::sync::Arc;

    fn create_test_config() -> HtmlProcessorConfig {
        HtmlProcessorConfig {
            origin_host: "origin.example.com".to_string(),
            request_host: "test.example.com".to_string(),
            request_scheme: "https".to_string(),
            integrations: IntegrationRegistry::default(),
        }
    }

    #[test]
    fn integration_attribute_rewriter_can_remove_elements() {
        struct RemovingLinkRewriter;

        impl IntegrationAttributeRewriter for RemovingLinkRewriter {
            fn integration_id(&self) -> &'static str {
                "removing"
            }

            fn handles_attribute(&self, attribute: &str) -> bool {
                attribute == "href"
            }

            fn rewrite(
                &self,
                _attr_name: &str,
                attr_value: &str,
                _ctx: &IntegrationAttributeContext<'_>,
            ) -> AttributeRewriteAction {
                if attr_value.contains("remove-me") {
                    AttributeRewriteAction::remove_element()
                } else {
                    AttributeRewriteAction::keep()
                }
            }
        }

        let html = r#"<html><body>
            <a href="https://origin.example.com/remove-me">remove</a>
            <a href="https://origin.example.com/keep-me">keep</a>
        </body></html>"#;

        let mut config = create_test_config();
        config.integrations =
            IntegrationRegistry::from_rewriters(vec![Arc::new(RemovingLinkRewriter)], Vec::new());

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
            .expect("pipeline should process HTML");
        let processed = String::from_utf8(output).expect("output should be valid UTF-8");

        assert!(processed.contains("keep-me"));
        assert!(!processed.contains("remove-me"));
    }

    #[test]
    fn test_create_html_processor_url_replacement() {
        let config = create_test_config();
        let processor = create_html_processor(config);

        // Create a pipeline to test the processor
        let pipeline_config = PipelineConfig {
            input_compression: Compression::None,
            output_compression: Compression::None,
            chunk_size: 8192,
        };
        let mut pipeline = StreamingPipeline::new(pipeline_config, processor);

        let html = r#"<html>
            <a href="https://origin.example.com/page">Link</a>
            <a href="//origin.example.com/proto">Proto</a>
            <a href="origin.example.com/bare">Bare</a>
            <img src="http://origin.example.com/image.jpg">
            <img src="//origin.example.com/image2.jpg">
            <form action="https://origin.example.com/submit">
            <form action="//origin.example.com/submit2">
        </html>"#;

        let mut output = Vec::new();
        pipeline
            .process(Cursor::new(html.as_bytes()), &mut output)
            .expect("pipeline should process HTML");

        let result = String::from_utf8(output).expect("output should be valid UTF-8");
        assert!(result.contains(r#"href="https://test.example.com/page""#));
        assert!(result.contains(r#"href="//test.example.com/proto""#));
        assert!(result.contains(r#"href="test.example.com/bare""#));
        assert!(result.contains(r#"src="https://test.example.com/image.jpg""#));
        assert!(result.contains(r#"src="//test.example.com/image2.jpg""#));
        assert!(result.contains(r#"action="https://test.example.com/submit""#));
        assert!(result.contains(r#"action="//test.example.com/submit2""#));
        assert!(!result.contains("origin.example.com"));
    }

    #[test]
    fn test_html_processor_config_from_settings() {
        let settings = create_test_settings();
        let registry = IntegrationRegistry::new(&settings).expect("should create registry");
        let config = HtmlProcessorConfig::from_settings(
            &settings,
            &registry,
            "origin.test-publisher.com",
            "proxy.example.com",
            "https",
        );

        assert_eq!(config.origin_host, "origin.test-publisher.com");
        assert_eq!(config.request_host, "proxy.example.com");
        assert_eq!(config.request_scheme, "https");
    }

    #[test]
    fn test_real_publisher_html() {
        // Test with publisher HTML from test_publisher.html
        let html = include_str!("html_processor.test.html");

        // Count URLs in the test HTML
        let original_urls = html.matches("www.test-publisher.com").count();
        let https_urls = html.matches("https://www.test-publisher.com").count();
        let protocol_relative_urls = html.matches("//www.test-publisher.com").count();

        println!("Test HTML stats:");
        println!("  Total URLs: {}", original_urls);
        println!("  HTTPS URLs: {}", https_urls);
        println!("  Protocol-relative URLs: {}", protocol_relative_urls);

        // Process - replace test-publisher.com with our edge domain
        let mut config = create_test_config();
        config.origin_host = "www.test-publisher.com".to_string(); // Match what's in the HTML
        config.request_host = "test-publisher-ts.edgecompute.app".to_string();

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
            .expect("pipeline should process HTML");
        let result = String::from_utf8(output).expect("output should be valid UTF-8");

        // Assertions - only URL attribute replacements are expected
        // Check URL replacements (not all occurrences will be replaced since
        // we only rewrite attributes, not text/JSON/script bodies)
        let remaining_urls = result.matches("www.test-publisher.com").count();
        let replaced_urls = result.matches("test-publisher-ts.edgecompute.app").count();

        println!("After processing:");
        println!("  Remaining original URLs: {}", remaining_urls);
        println!("  Edge domain URLs: {}", replaced_urls);

        // Expect at least some replacements and fewer originals than before
        assert!(replaced_urls > 0, "Should replace some URLs in attributes");
        assert!(
            remaining_urls < original_urls,
            "Should reduce occurrences of original host in attributes"
        );

        // Verify HTML structure
        assert_eq!(&result[0..15], "<!DOCTYPE html>");
        assert_eq!(&result[result.len() - 7..], "</html>");

        // Verify content preservation
        assert!(
            result.contains("Mercedes CEO"),
            "Should preserve article title"
        );
        assert!(
            result.contains("test-publisher"),
            "Should preserve text content"
        );
        // No Prebid auto-configuration injection performed here
        assert!(
            !result.contains("window.__trustedServerPrebid"),
            "HtmlProcessor should not inject Prebid config"
        );
    }

    #[test]
    fn test_integration_registry_rewrites_integration_scripts() {
        let html = r#"<html><head>
            <script src="https://cdn.testlight.com/v1/testlight.js"></script>
        </head><body></body></html>"#;

        let mut settings = Settings::default();
        let shim_src = "https://edge.example.com/static/testlight.js".to_string();
        settings
            .integrations
            .insert_config(
                "testlight",
                &json!({
                    "enabled": true,
                    "endpoint": "https://example.com/openrtb2/auction",
                    "rewrite_scripts": true,
                    "shim_src": shim_src,
                }),
            )
            .expect("should insert testlight config");

        let registry = IntegrationRegistry::new(&settings).expect("should create registry");
        let mut config = create_test_config();
        config.integrations = registry;

        let processor = create_html_processor(config);
        let pipeline_config = PipelineConfig {
            input_compression: Compression::None,
            output_compression: Compression::None,
            chunk_size: 8192,
        };
        let mut pipeline = StreamingPipeline::new(pipeline_config, processor);

        let mut output = Vec::new();
        let result = pipeline.process(Cursor::new(html.as_bytes()), &mut output);
        assert!(result.is_ok());

        let processed = String::from_utf8_lossy(&output);
        assert!(
            processed.contains(&shim_src),
            "Integration shim should replace integration script reference"
        );
        assert!(
            !processed.contains("cdn.testlight.com"),
            "Original integration URL should be removed"
        );
    }

    #[test]
    fn test_real_publisher_html_with_gzip() {
        use flate2::read::GzDecoder;
        use flate2::write::GzEncoder;
        use flate2::Compression as GzCompression;
        use std::io::{Read, Write};

        let html = include_str!("html_processor.test.html");

        // Count URLs in test HTML
        let _original_urls = html.matches("www.test-publisher.com").count();

        // Compress
        let mut encoder = GzEncoder::new(Vec::new(), GzCompression::default());
        encoder
            .write_all(html.as_bytes())
            .expect("should write to gzip encoder");
        let compressed_input = encoder.finish().expect("should finish gzip encoding");

        println!("Compressed input size: {} bytes", compressed_input.len());

        // Process with compression
        let mut config = create_test_config();
        config.origin_host = "www.test-publisher.com".to_string(); // Match what's in the HTML
        config.request_host = "test-publisher-ts.edgecompute.app".to_string();

        let processor = create_html_processor(config);
        let pipeline_config = PipelineConfig {
            input_compression: Compression::Gzip,
            output_compression: Compression::Gzip,
            chunk_size: 8192,
        };
        let mut pipeline = StreamingPipeline::new(pipeline_config, processor);

        let mut compressed_output = Vec::new();
        pipeline
            .process(Cursor::new(&compressed_input), &mut compressed_output)
            .expect("pipeline should process gzipped HTML");

        // Ensure we produced output
        assert!(
            !compressed_output.is_empty(),
            "Should produce compressed output"
        );

        // Decompress and verify
        let mut decoder = GzDecoder::new(&compressed_output[..]);
        let mut decompressed = String::new();
        decoder
            .read_to_string(&mut decompressed)
            .expect("should decompress gzip output");

        let remaining_urls = decompressed.matches("www.test-publisher.com").count();
        let replaced_urls = decompressed
            .matches("test-publisher-ts.edgecompute.app")
            .count();

        assert!(replaced_urls > 0, "Should replace some URLs in attributes");
        assert!(
            remaining_urls < _original_urls,
            "Should reduce occurrences of original host in attributes"
        );

        // Verify structure
        assert_eq!(&decompressed[0..15], "<!DOCTYPE html>");
        assert_eq!(&decompressed[decompressed.len() - 7..], "</html>");

        // Verify content preservation
        assert!(
            decompressed.contains("Mercedes CEO"),
            "Should preserve article title"
        );
        assert!(
            decompressed.contains("test-publisher"),
            "Should preserve text content"
        );
        // No Prebid auto-configuration injection performed here
        assert!(
            !decompressed.contains("window.__trustedServerPrebid"),
            "HtmlProcessor should not inject Prebid config"
        );
    }

    #[test]
    fn test_already_truncated_html_passthrough() {
        // Test that we don't make truncated HTML worse
        // This simulates receiving already-truncated HTML from origin

        let truncated_html =
            r#"<html><head><title>Test</title></head><body><p>This is a test that gets cut o"#;

        println!("Testing already-truncated HTML");
        println!("Input: '{}'", truncated_html);

        let config = create_test_config();
        let processor = create_html_processor(config);
        let pipeline_config = PipelineConfig {
            input_compression: Compression::None,
            output_compression: Compression::None,
            chunk_size: 8192,
        };
        let mut pipeline = StreamingPipeline::new(pipeline_config, processor);

        let mut output = Vec::new();
        let result = pipeline.process(Cursor::new(truncated_html.as_bytes()), &mut output);

        assert!(
            result.is_ok(),
            "Should process truncated HTML without error"
        );

        let processed = String::from_utf8_lossy(&output);
        println!("Output: '{}'", processed);

        // The processor should pass through the truncated HTML
        // It might add some closing tags, but shouldn't truncate further
        assert!(
            processed.len() >= truncated_html.len(),
            "Output should not be shorter than truncated input"
        );
    }

    #[test]
    fn test_truncated_html_validation() {
        // Simulated truncated HTML - ends mid-attribute
        let truncated_html = r#"<html lang="en"><head><meta charset="utf-8"><title>Test Publisher</title><link rel="preload" as="image" href="https://www.test-publisher.com/image.jpg"><script src="/js/prebid.min.js"></script></head><body><p>Article content from <a href="https://www.test-publisher.com/ar"#;

        // This HTML is clearly truncated - it ends in the middle of an attribute value
        println!("Testing truncated HTML (ends in middle of URL)");
        println!("Input length: {} bytes", truncated_html.len());

        // Check that the input is indeed truncated
        assert!(
            !truncated_html.contains("</html>"),
            "Input should be truncated (no closing html tag)"
        );
        assert!(
            !truncated_html.contains("</body>"),
            "Input should be truncated (no closing body tag)"
        );
        assert!(
            truncated_html.ends_with("/ar"),
            "Input should end with '/ar' showing truncation"
        );

        // Process it through our pipeline
        let mut config = create_test_config();
        config.origin_host = "www.test-publisher.com".to_string(); // Match what's in the HTML
        config.request_host = "test-publisher-ts.edgecompute.app".to_string();

        let processor = create_html_processor(config);
        let pipeline_config = PipelineConfig {
            input_compression: Compression::None,
            output_compression: Compression::None,
            chunk_size: 8192,
        };
        let mut pipeline = StreamingPipeline::new(pipeline_config, processor);

        let mut output = Vec::new();

        // The processor should handle truncated HTML gracefully
        let result = pipeline.process(Cursor::new(truncated_html.as_bytes()), &mut output);

        // Even with truncated input, processing should complete
        assert!(
            result.is_ok(),
            "Processing should complete even with truncated HTML"
        );

        let processed = String::from_utf8_lossy(&output);
        println!("Output length: {} bytes", processed.len());

        // The processor will try to fix the HTML structure
        // lol_html should handle the truncated input and still produce output

        // Check what we got back
        if processed.contains("</html>") {
            println!("Note: lol_html added closing tags to fix truncated HTML");
        }

        // The key issue is that truncated HTML should not cause a panic or error
        // The output might still be malformed, but it should process

        println!(
            "Last 100 chars of output: {}",
            processed
                .chars()
                .rev()
                .take(100)
                .collect::<String>()
                .chars()
                .rev()
                .collect::<String>()
        );
    }
}
