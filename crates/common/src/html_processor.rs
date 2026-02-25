//! Simplified HTML processor that combines URL replacement and integration injection
//!
//! This module provides a [`StreamProcessor`] implementation for HTML content.
//!
//! ## Streaming Behavior with Post-Processing
//!
//! When post-processors are registered (e.g., Next.js RSC URL rewriting), the processor
//! uses **lazy accumulation** to optimize streaming:
//!
//! 1. **Initial streaming**: Chunks are streamed immediately until RSC content is detected
//! 2. **Accumulation trigger**: When RSC scripts or placeholders are found, buffering begins
//! 3. **Post-processing**: At document end, accumulated HTML is processed to rewrite RSC payloads
//!
//! ### Streaming Ratios
//!
//! Observed streaming performance:
//! - **Non-RSC pages**: 96%+ streaming (minimal buffering)
//! - **RSC pages**: 28-37% streaming (depends on where RSC scripts appear in HTML)
//! - **Before optimization**: 0% streaming (everything buffered)
//!
//! The streaming ratio for RSC pages is limited by Next.js's architecture: RSC scripts
//! appear at the end of the HTML and make up 60-72% of the document. Bytes already
//! streamed before RSC detection cannot be recovered, so the post-processor's fallback
//! re-parse path handles RSC scripts in the already-streamed prefix.
//!
//! ## Memory Safety
//!
//! Accumulated output is limited to [`MAX_ACCUMULATED_HTML_BYTES`] (10MB) to prevent
//! unbounded memory growth from malicious or extremely large documents.
use std::cell::Cell;
use std::io;
use std::rc::Rc;
use std::sync::Arc;

use lol_html::{element, html_content::ContentType, text, Settings as RewriterSettings};

/// Maximum size for accumulated HTML output when post-processing is required.
/// This prevents unbounded memory growth from malicious or extremely large documents.
const MAX_ACCUMULATED_HTML_BYTES: usize = 10 * 1024 * 1024; // 10 MB

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
    /// Accumulated output from intermediate chunks. Only populated once we
    /// detect that post-processing will be needed (e.g. an RSC placeholder was
    /// inserted or a fragmented RSC script was observed). Before that trigger,
    /// chunks stream through immediately.
    accumulated_output: Vec<u8>,
    /// Number of bytes already streamed to the caller before accumulation began.
    /// When accumulation triggers, we cannot recover those bytes, so we must
    /// fall back to the post-processor's re-parse path for any RSC scripts in
    /// the already-streamed prefix.
    streamed_bytes: usize,
    /// Whether we are accumulating output for post-processing.
    accumulating: bool,
    origin_host: String,
    request_host: String,
    request_scheme: String,
    document_state: IntegrationDocumentState,
}

impl HtmlWithPostProcessing {
    /// Check whether we need to start accumulating output for post-processing.
    ///
    /// Processors may inspect [`IntegrationDocumentState`] to lazily trigger
    /// accumulation once they detect content that requires whole-document
    /// post-processing.
    fn needs_accumulation(&self) -> bool {
        self.post_processors
            .iter()
            .any(|processor| processor.needs_accumulation(&self.document_state))
    }
}

impl StreamProcessor for HtmlWithPostProcessing {
    fn process_chunk(&mut self, chunk: &[u8], is_last: bool) -> Result<Vec<u8>, io::Error> {
        let output = self.inner.process_chunk(chunk, is_last)?;

        // No post-processors → stream through immediately (fast path).
        if self.post_processors.is_empty() {
            return Ok(output);
        }

        // If we're not yet accumulating, check if we need to start.
        // This allows non-RSC pages with post-processors registered to stream
        // through without buffering.
        if !self.accumulating && self.needs_accumulation() {
            self.accumulating = true;
            log::debug!(
                "HTML post-processing: switching to accumulation mode, streamed_bytes={}",
                self.streamed_bytes
            );
        }

        if !self.accumulating {
            if !is_last {
                self.streamed_bytes += output.len();
                return Ok(output);
            }

            // Final chunk, never accumulated — check if post-processing is needed.
            // This handles the rare case where RSC scripts appear only in the final
            // chunk, or where fragmented scripts need the fallback re-parse path.
            let ctx = IntegrationHtmlContext {
                request_host: &self.request_host,
                request_scheme: &self.request_scheme,
                origin_host: &self.origin_host,
                document_state: &self.document_state,
            };

            let Ok(output_str) = std::str::from_utf8(&output) else {
                return Ok(output);
            };

            if !self
                .post_processors
                .iter()
                .any(|p| p.should_process(output_str, &ctx))
            {
                return Ok(output);
            }

            // Post-processing needed on just the final chunk.
            // This is only correct if no earlier chunks contained RSC content
            // (which would mean they were already streamed without rewriting).
            // In practice, this handles pages where RSC scripts are small
            // enough to fit in the final chunk.
            let mut html = String::from_utf8(output).map_err(|e| {
                io::Error::other(format!(
                    "HTML post-processing expected valid UTF-8 output: {e}"
                ))
            })?;

            for processor in &self.post_processors {
                if processor.should_process(&html, &ctx) {
                    processor.post_process(&mut html, &ctx);
                }
            }

            return Ok(html.into_bytes());
        }

        // Accumulating mode: buffer output for end-of-document post-processing.
        // Check size limit to prevent unbounded memory growth.
        if self.accumulated_output.len() + output.len() > MAX_ACCUMULATED_HTML_BYTES {
            return Err(io::Error::other(format!(
                "HTML post-processing: accumulated output would exceed {}MB size limit \
                 (current: {} bytes, chunk: {} bytes)",
                MAX_ACCUMULATED_HTML_BYTES / (1024 * 1024),
                self.accumulated_output.len(),
                output.len()
            )));
        }

        self.accumulated_output.extend_from_slice(&output);
        if !is_last {
            return Ok(Vec::new());
        }

        // All chunks received — run post-processing on the accumulated output.
        let full_output = std::mem::take(&mut self.accumulated_output);
        if full_output.is_empty() {
            return Ok(full_output);
        }

        let Ok(output_str) = std::str::from_utf8(&full_output) else {
            return Ok(full_output);
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
            return Ok(full_output);
        }

        let mut html = String::from_utf8(full_output).map_err(|e| {
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
                "HTML post-processing complete: origin_host={}, output_len={}, streamed_prefix_bytes={}",
                self.origin_host,
                html.len(),
                self.streamed_bytes,
            );
        }

        Ok(html.into_bytes())
    }

    fn reset(&mut self) {
        self.inner.reset();
        self.accumulated_output.clear();
        self.streamed_bytes = 0;
        self.accumulating = false;
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

/// Create an HTML processor with URL replacement and integration hooks
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
            let integrations = integration_registry.clone();
            let patterns = patterns.clone();
            let document_state = document_state.clone();
            move |el| {
                if !injected_tsjs.get() {
                    let mut snippet = String::new();
                    let ctx = IntegrationHtmlContext {
                        request_host: &patterns.request_host,
                        request_scheme: &patterns.request_scheme,
                        origin_host: &patterns.origin_host,
                        document_state: &document_state,
                    };
                    // First inject integration-specific config (e.g., window.__tsjs_prebid)
                    // so it's available when the bundle's auto-init code reads it.
                    for insert in integrations.head_inserts(&ctx) {
                        snippet.push_str(&insert);
                    }
                    // Then inject the TSJS bundle — its top-level init code can now
                    // read the config that was set by the inline scripts above.
                    let module_ids = integrations.js_module_ids();
                    snippet.push_str(&tsjs::tsjs_script_tag(&module_ids));
                    el.prepend(&snippet, ContentType::Html);
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
        accumulated_output: Vec::new(),
        streamed_bytes: 0,
        accumulating: false,
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
        IntegrationHeadInjector, IntegrationHtmlContext,
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
    fn integration_head_injector_prepends_after_tsjs_once() {
        struct TestHeadInjector;

        impl IntegrationHeadInjector for TestHeadInjector {
            fn integration_id(&self) -> &'static str {
                "test"
            }

            fn head_inserts(&self, _ctx: &IntegrationHtmlContext<'_>) -> Vec<String> {
                vec![r#"<script>window.__testHeadInjector=true;</script>"#.to_string()]
            }
        }

        let html = r#"<html><head><title>Test</title></head><body></body></html>"#;

        let mut config = create_test_config();
        config.integrations = IntegrationRegistry::from_rewriters_with_head_injectors(
            Vec::new(),
            Vec::new(),
            vec![Arc::new(TestHeadInjector)],
        );

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

        let tsjs_marker = "id=\"trustedserver-js\"";
        let head_marker = "window.__testHeadInjector=true";

        assert_eq!(
            processed.matches(tsjs_marker).count(),
            1,
            "should inject unified tsjs tag once"
        );
        assert_eq!(
            processed.matches(head_marker).count(),
            1,
            "should inject head snippet once"
        );

        let tsjs_index = processed
            .find(tsjs_marker)
            .expect("should include unified tsjs tag");
        let head_index = processed
            .find(head_marker)
            .expect("should include head snippet");
        let title_index = processed
            .find("<title>")
            .expect("should keep existing head content");

        assert!(
            head_index < tsjs_index,
            "should inject config before tsjs bundle so auto-init can read it"
        );
        assert!(
            tsjs_index < title_index,
            "should prepend all injected content before existing head content"
        );
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

    /// E2E test: verifies that RSC pages with Next.js post-processors produce correct output
    /// when processed through the full streaming pipeline, and quantifies the streaming
    /// behavior (how much output is emitted before `is_last`).
    #[test]
    fn rsc_html_streams_correctly_with_post_processors() {
        use crate::streaming_processor::StreamProcessor;

        // Simulate a Next.js App Router page with multiple RSC scripts, including
        // a cross-script T-chunk (header in script 1, content continues in script 2).
        let html = concat!(
            "<html><head><title>Next.js RSC Page</title>",
            "<link rel=\"stylesheet\" href=\"https://origin.example.com/styles.css\">",
            "</head><body>",
            "<div id=\"content\">Hello World</div>",
            // RSC script 1: contains a T-chunk header that spans into script 2
            r#"<script>self.__next_f.push([1,"0:{\"url\":\"https://origin.example.com/page\"}\n1a:T3e,partial content"])</script>"#,
            // RSC script 2: continuation of the T-chunk from script 1
            r#"<script>self.__next_f.push([1," with https://origin.example.com/more goes here"])</script>"#,
            // Non-RSC script that must be preserved
            r#"<script>console.log("analytics ready");</script>"#,
            "<a href=\"https://origin.example.com/about\">About</a>",
            "</body></html>",
        );

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
        let registry =
            IntegrationRegistry::new(&settings).expect("should create integration registry");

        // Verify post-processors ARE registered (this is the key precondition)
        let post_processors = registry.html_post_processors();
        assert!(
            !post_processors.is_empty(),
            "Next.js post-processors should be registered when enabled"
        );

        let config = HtmlProcessorConfig::from_settings(
            &settings,
            &registry,
            "origin.example.com",
            "test.example.com",
            "https",
        );
        let mut processor = create_html_processor(config);

        // Process in chunks to simulate streaming, tracking per-chunk output
        let bytes = html.as_bytes();
        let chunk_size = 64;
        let chunks: Vec<&[u8]> = bytes.chunks(chunk_size).collect();
        let last_idx = chunks.len().saturating_sub(1);

        let mut intermediate_bytes = 0usize;
        let mut final_bytes = 0usize;
        let mut full_output = Vec::new();

        for (i, chunk) in chunks.iter().enumerate() {
            let is_last = i == last_idx;
            let result = processor
                .process_chunk(chunk, is_last)
                .expect("should process chunk");

            if is_last {
                final_bytes = result.len();
            } else {
                intermediate_bytes += result.len();
            }
            full_output.extend_from_slice(&result);
        }

        let output = String::from_utf8(full_output).expect("output should be valid UTF-8");

        // --- Correctness assertions ---

        // 1. URL rewriting in HTML attributes should work
        assert!(
            output.contains("test.example.com/about"),
            "HTML href URLs should be rewritten. Got: {output}"
        );
        assert!(
            output.contains("test.example.com/styles.css"),
            "Link href URLs should be rewritten. Got: {output}"
        );

        // 2. RSC payloads should be rewritten via post-processing
        assert!(
            output.contains("test.example.com/page"),
            "RSC payload URLs should be rewritten. Got: {output}"
        );

        // 3. No placeholder markers should leak into the output
        assert!(
            !output.contains("__ts_rsc_payload_"),
            "RSC placeholder markers should not appear in final output. Got: {output}"
        );

        // 4. Non-RSC scripts should be preserved
        assert!(
            output.contains("analytics ready"),
            "Non-RSC scripts should be preserved. Got: {output}"
        );

        // 5. HTML structure should be intact
        assert!(
            output.contains("<html>") || output.contains("<html "),
            "HTML should be structurally intact. Got: {output}"
        );
        assert!(
            output.contains("Hello World"),
            "Content should be preserved. Got: {output}"
        );

        // --- Streaming behavior observation ---
        // When post-processors are active, intermediate chunks return empty because
        // the output must be accumulated for post-processing (RSC placeholder
        // substitution). This is a known limitation documented here for visibility.
        println!(
            "Streaming behavior with post-processors: intermediate_bytes={}, final_bytes={}, total={}",
            intermediate_bytes,
            final_bytes,
            intermediate_bytes + final_bytes
        );
        println!(
            "  Streaming ratio: {:.1}% of bytes emitted before is_last",
            if intermediate_bytes + final_bytes > 0 {
                intermediate_bytes as f64 / (intermediate_bytes + final_bytes) as f64 * 100.0
            } else {
                0.0
            }
        );
    }

    /// E2E test: verifies that HTML pages WITHOUT RSC (no post-processors active)
    /// stream incrementally — chunks are emitted before `is_last`.
    #[test]
    fn non_rsc_html_streams_incrementally_without_post_processors() {
        use crate::streaming_processor::StreamProcessor;

        let html = concat!(
            "<html><head><title>Regular Page</title>",
            "<link rel=\"stylesheet\" href=\"https://origin.example.com/styles.css\">",
            "</head><body>",
            "<div>",
            "<a href=\"https://origin.example.com/page1\">Page 1</a>",
            "<a href=\"https://origin.example.com/page2\">Page 2</a>",
            "<a href=\"https://origin.example.com/page3\">Page 3</a>",
            "</div>",
            "</body></html>",
        );

        // No Next.js integration — post_processors will be empty
        let config = create_test_config();
        let mut processor = create_html_processor(config);

        let bytes = html.as_bytes();
        let chunk_size = 64;
        let chunks: Vec<&[u8]> = bytes.chunks(chunk_size).collect();
        let last_idx = chunks.len().saturating_sub(1);

        let mut intermediate_bytes = 0usize;
        let mut final_bytes = 0usize;
        let mut full_output = Vec::new();

        for (i, chunk) in chunks.iter().enumerate() {
            let is_last = i == last_idx;
            let result = processor
                .process_chunk(chunk, is_last)
                .expect("should process chunk");

            if is_last {
                final_bytes = result.len();
            } else {
                intermediate_bytes += result.len();
            }
            full_output.extend_from_slice(&result);
        }

        let output = String::from_utf8(full_output).expect("output should be valid UTF-8");

        // Correctness: URLs should be rewritten
        assert!(
            output.contains("test.example.com/page1"),
            "URLs should be rewritten. Got: {output}"
        );
        assert!(
            !output.contains("origin.example.com"),
            "No origin URLs should remain. Got: {output}"
        );

        // Streaming: intermediate chunks SHOULD produce output (no post-processors)
        assert!(
            intermediate_bytes > 0,
            "Without post-processors, intermediate chunks should emit output (got 0 bytes). \
             This confirms true streaming. Final bytes: {final_bytes}"
        );

        println!(
            "Streaming behavior without post-processors: intermediate_bytes={}, final_bytes={}, total={}",
            intermediate_bytes,
            final_bytes,
            intermediate_bytes + final_bytes
        );
        println!(
            "  Streaming ratio: {:.1}% of bytes emitted before is_last",
            intermediate_bytes as f64 / (intermediate_bytes + final_bytes) as f64 * 100.0
        );
    }

    /// E2E test: RSC Flight responses (`text/x-component`) stream correctly
    /// through the pipeline with URL rewriting and T-row length recalculation.
    #[test]
    fn rsc_flight_response_streams_with_url_rewriting() {
        use crate::rsc_flight::RscFlightUrlRewriter;
        use crate::streaming_processor::StreamProcessor;

        // Simulate a Flight response with mixed row types
        let t_content = r#"{"url":"https://origin.example.com/dashboard"}"#;
        let flight_response = format!(
            "0:[\"https://origin.example.com/page\"]\n\
             1:T{:x},{}\
             2:[\"ok\"]\n",
            t_content.len(),
            t_content,
        );

        let mut processor = RscFlightUrlRewriter::new(
            "origin.example.com",
            "https://origin.example.com",
            "test.example.com",
            "https",
        );

        // Process in small chunks to exercise cross-chunk state handling
        let bytes = flight_response.as_bytes();
        let chunk_size = 11; // intentionally misaligned with row boundaries
        let chunks: Vec<&[u8]> = bytes.chunks(chunk_size).collect();
        let last_idx = chunks.len().saturating_sub(1);

        let mut intermediate_bytes = 0usize;
        let mut full_output = Vec::new();

        for (i, chunk) in chunks.iter().enumerate() {
            let is_last = i == last_idx;
            let result = processor
                .process_chunk(chunk, is_last)
                .expect("should process flight chunk");

            if !is_last {
                intermediate_bytes += result.len();
            }
            full_output.extend_from_slice(&result);
        }

        let output = String::from_utf8(full_output).expect("output should be valid UTF-8");

        // URLs should be rewritten
        assert!(
            output.contains("test.example.com/page"),
            "Newline row URLs should be rewritten. Got: {output}"
        );
        assert!(
            output.contains("test.example.com/dashboard"),
            "T-row URLs should be rewritten. Got: {output}"
        );

        // T-row length should be recalculated
        let rewritten_t_content = r#"{"url":"https://test.example.com/dashboard"}"#;
        let expected_len_hex = format!("{:x}", rewritten_t_content.len());
        assert!(
            output.contains(&format!(":T{expected_len_hex},")),
            "T-row length should be recalculated. Got: {output}"
        );

        // No origin URLs should remain
        assert!(
            !output.contains("origin.example.com"),
            "No origin URLs should remain. Got: {output}"
        );

        // Flight rewriter should stream incrementally
        assert!(
            intermediate_bytes > 0,
            "RSC Flight rewriter should emit output for intermediate chunks (got 0 bytes)"
        );

        // Trailing row should be preserved
        assert!(
            output.contains("2:[\"ok\"]\n"),
            "Trailing rows should be preserved. Got: {output}"
        );
    }
}
