//! Simplified HTML processor that combines URL replacement and Prebid injection
//!
//! This module provides a StreamProcessor implementation for HTML content.
use std::cell::Cell;
use std::collections::BTreeSet;
use std::rc::Rc;

use lol_html::{element, html_content::ContentType, text, Settings as RewriterSettings};
use regex::Regex;

use crate::integrations::{
    IntegrationAttributeContext, IntegrationRegistry, IntegrationScriptContext,
};
use crate::settings::Settings;
use crate::streaming_processor::{HtmlRewriterAdapter, StreamProcessor};
use crate::tsjs;

/// Configuration for HTML processing
#[derive(Clone)]
pub struct HtmlProcessorConfig {
    pub origin_host: String,
    pub request_host: String,
    pub request_scheme: String,
    pub integrations: IntegrationRegistry,
    pub nextjs_enabled: bool,
    pub nextjs_attributes: Vec<String>,
    pub integration_assets: Vec<String>,
}

impl HtmlProcessorConfig {
    /// Create from settings and request parameters
    pub fn from_settings(
        settings: &Settings,
        integrations: &IntegrationRegistry,
        origin_host: &str,
        request_host: &str,
        request_scheme: &str,
    ) -> Self {
        let asset_set: BTreeSet<String> = integrations
            .registered_integrations()
            .into_iter()
            .flat_map(|meta| meta.assets)
            .collect();

        Self {
            origin_host: origin_host.to_string(),
            request_host: request_host.to_string(),
            request_scheme: request_scheme.to_string(),
            integrations: integrations.clone(),
            nextjs_enabled: settings.publisher.nextjs.enabled,
            nextjs_attributes: settings.publisher.nextjs.rewrite_attributes.clone(),
            integration_assets: asset_set.into_iter().collect(),
        }
    }
}

/// Create an HTML processor with URL replacement and optional Prebid injection
pub fn create_html_processor(config: HtmlProcessorConfig) -> impl StreamProcessor {
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

        fn rewrite_nextjs_values(&self, content: &str, attributes: &[String]) -> Option<String> {
            let mut rewritten = content.to_string();
            let mut changed = false;
            let escaped_origin = regex::escape(&self.origin_host);
            for attribute in attributes {
                let escaped_attr = regex::escape(attribute);
                let pattern = format!(
                    r#"(?P<prefix>(?:\\*")?{attr}(?:\\*")?:\\*")(?P<scheme>https?://|//){origin}"#,
                    attr = escaped_attr,
                    origin = escaped_origin
                );
                let regex = Regex::new(&pattern).expect("valid Next.js rewrite regex");
                let new_value = regex.replace_all(&rewritten, |caps: &regex::Captures| {
                    let scheme = &caps["scheme"];
                    let replacement = if scheme == "//" {
                        format!("//{}", self.request_host)
                    } else {
                        self.replacement_url()
                    };
                    format!("{}{}", &caps["prefix"], replacement)
                });
                if new_value != rewritten {
                    changed = true;
                    rewritten = new_value.into_owned();
                }
            }
            if changed {
                Some(rewritten)
            } else {
                None
            }
        }
    }

    let patterns = Rc::new(UrlPatterns {
        origin_host: config.origin_host.clone(),
        request_host: config.request_host.clone(),
        request_scheme: config.request_scheme.clone(),
    });

    let nextjs_attributes = Rc::new(config.nextjs_attributes.clone());

    let injected_tsjs = Rc::new(Cell::new(false));
    let integration_assets = Rc::new(config.integration_assets.clone());
    let injected_assets = Rc::new(Cell::new(false));
    let integration_registry = config.integrations.clone();
    let script_rewriters = integration_registry.script_rewriters();

    let mut element_content_handlers = vec![
        element!("head", {
            let injected_tsjs = injected_tsjs.clone();
            let integration_assets = integration_assets.clone();
            let injected_assets = injected_assets.clone();
            move |el| {
                if !injected_tsjs.get() {
                    let loader = tsjs::core_script_tag();
                    el.prepend(&loader, ContentType::Html);
                    injected_tsjs.set(true);
                }
                if !integration_assets.is_empty() && !injected_assets.get() {
                    for asset in integration_assets.iter() {
                        let attrs = format!("async data-tsjs-integration=\"{}\"", asset);
                        let tag = tsjs::integration_script_tag(asset, &attrs);
                        el.append(&tag, ContentType::Html);
                    }
                    injected_assets.set(true);
                }
                Ok(())
            }
        }),
        element!("[href]", {
            let patterns = patterns.clone();
            let integrations = integration_registry.clone();
            move |el| {
                if let Some(mut href) = el.get_attribute("href") {
                    let original_href = href.clone();
                    let new_href = href
                        .replace(&patterns.https_origin(), &patterns.replacement_url())
                        .replace(&patterns.http_origin(), &patterns.replacement_url());
                    if new_href != href {
                        href = new_href;
                    }

                    if let Some(integration_href) = integrations.rewrite_attribute(
                        "href",
                        &href,
                        &IntegrationAttributeContext {
                            attribute_name: "href",
                            request_host: &patterns.request_host,
                            request_scheme: &patterns.request_scheme,
                            origin_host: &patterns.origin_host,
                        },
                    ) {
                        href = integration_href;
                    }

                    if href != original_href {
                        el.set_attribute("href", &href)?;
                    }
                }
                Ok(())
            }
        }),
        element!("[src]", {
            let patterns = patterns.clone();
            let integrations = integration_registry.clone();
            move |el| {
                if let Some(mut src) = el.get_attribute("src") {
                    let original_src = src.clone();
                    let new_src = src
                        .replace(&patterns.https_origin(), &patterns.replacement_url())
                        .replace(&patterns.http_origin(), &patterns.replacement_url());
                    if new_src != src {
                        src = new_src;
                    }

                    if let Some(integration_src) = integrations.rewrite_attribute(
                        "src",
                        &src,
                        &IntegrationAttributeContext {
                            attribute_name: "src",
                            request_host: &patterns.request_host,
                            request_scheme: &patterns.request_scheme,
                            origin_host: &patterns.origin_host,
                        },
                    ) {
                        src = integration_src;
                    }

                    if src != original_src {
                        el.set_attribute("src", &src)?;
                    }
                }
                Ok(())
            }
        }),
        element!("[action]", {
            let patterns = patterns.clone();
            let integrations = integration_registry.clone();
            move |el| {
                if let Some(mut action) = el.get_attribute("action") {
                    let original_action = action.clone();
                    let new_action = action
                        .replace(&patterns.https_origin(), &patterns.replacement_url())
                        .replace(&patterns.http_origin(), &patterns.replacement_url());
                    if new_action != action {
                        action = new_action;
                    }

                    if let Some(integration_action) = integrations.rewrite_attribute(
                        "action",
                        &action,
                        &IntegrationAttributeContext {
                            attribute_name: "action",
                            request_host: &patterns.request_host,
                            request_scheme: &patterns.request_scheme,
                            origin_host: &patterns.origin_host,
                        },
                    ) {
                        action = integration_action;
                    }

                    if action != original_action {
                        el.set_attribute("action", &action)?;
                    }
                }
                Ok(())
            }
        }),
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

                    if let Some(integration_srcset) = integrations.rewrite_attribute(
                        "srcset",
                        &srcset,
                        &IntegrationAttributeContext {
                            attribute_name: "srcset",
                            request_host: &patterns.request_host,
                            request_scheme: &patterns.request_scheme,
                            origin_host: &patterns.origin_host,
                        },
                    ) {
                        srcset = integration_srcset;
                    }

                    if srcset != original_srcset {
                        el.set_attribute("srcset", &srcset)?;
                    }
                }
                Ok(())
            }
        }),
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

                    if let Some(integration_imagesrcset) = integrations.rewrite_attribute(
                        "imagesrcset",
                        &imagesrcset,
                        &IntegrationAttributeContext {
                            attribute_name: "imagesrcset",
                            request_host: &patterns.request_host,
                            request_scheme: &patterns.request_scheme,
                            origin_host: &patterns.origin_host,
                        },
                    ) {
                        imagesrcset = integration_imagesrcset;
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
        element_content_handlers.push(text!(selector, {
            let rewriter = rewriter.clone();
            let patterns = patterns.clone();
            move |text| {
                let ctx = IntegrationScriptContext {
                    selector,
                    request_host: &patterns.request_host,
                    request_scheme: &patterns.request_scheme,
                    origin_host: &patterns.origin_host,
                };
                if let Some(rewritten) = rewriter.rewrite(text.as_str(), &ctx) {
                    text.replace(&rewritten, ContentType::Text);
                }
                Ok(())
            }
        }));
    }

    if config.nextjs_enabled && !nextjs_attributes.is_empty() {
        element_content_handlers.push(text!("script#__NEXT_DATA__", {
            let patterns = patterns.clone();
            let attributes = nextjs_attributes.clone();
            move |text| {
                let content = text.as_str();
                if let Some(rewritten) = patterns.rewrite_nextjs_values(content, &attributes) {
                    text.replace(&rewritten, ContentType::Text);
                }
                Ok(())
            }
        }));

        element_content_handlers.push(text!("script", {
            let patterns = patterns.clone();
            let attributes = nextjs_attributes.clone();
            move |text| {
                let content = text.as_str();
                if !content.contains("self.__next_f") {
                    return Ok(());
                }
                if let Some(rewritten) = patterns.rewrite_nextjs_values(content, &attributes) {
                    text.replace(&rewritten, ContentType::Text);
                }
                Ok(())
            }
        }));
    }

    let rewriter_settings = RewriterSettings {
        element_content_handlers,

        // TODO: Consider adding text content replacement if needed with settings
        // // Replace URLs in text content
        // document_content_handlers: vec![lol_html::doc_text!({
        //     move |text| {
        //         let content = text.as_str();

        //         // Apply URL replacements
        //         let mut new_content = content.to_string();
        //         for replacement in replacer.replacements.iter() {
        //             if new_content.contains(&replacement.find) {
        //                 new_content = new_content.replace(&replacement.find, &replacement.replace_with);
        //             }
        //         }

        //         if new_content != content {
        //             text.replace(&new_content, lol_html::html_content::ContentType::Text);
        //         }

        //         Ok(())
        //     }
        // })],
        ..RewriterSettings::default()
    };

    HtmlRewriterAdapter::new(rewriter_settings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::streaming_processor::{Compression, PipelineConfig, StreamingPipeline};
    use crate::test_support::tests::create_test_settings;
    use crate::tsjs;
    use serde_json::json;
    use std::io::Cursor;

    const MOCK_TESTLIGHT_SRC: &str = "https://mock.testassets/testlight.js";

    struct MockBundleGuard;

    fn mock_testlight_bundle() -> MockBundleGuard {
        tsjs::mock_integration_bundle("testlight", MOCK_TESTLIGHT_SRC);
        MockBundleGuard
    }

    impl Drop for MockBundleGuard {
        fn drop(&mut self) {
            tsjs::clear_mock_integration_bundles();
        }
    }

    fn create_test_config() -> HtmlProcessorConfig {
        HtmlProcessorConfig {
            origin_host: "origin.example.com".to_string(),
            request_host: "test.example.com".to_string(),
            request_scheme: "https".to_string(),
            integrations: IntegrationRegistry::default(),
            nextjs_enabled: false,
            nextjs_attributes: vec!["href".to_string(), "link".to_string(), "url".to_string()],
            integration_assets: Vec::new(),
        }
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
    fn test_always_injects_tsjs_script() {
        let html = r#"<html><head>
            <script src="/js/prebid.min.js"></script>
            <link rel="preload" as="script" href="https://cdn.prebid.org/prebid.js" />
        </head><body></body></html>"#;

        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config(
                "prebid",
                &json!({
                    "enabled": true,
                    "server_url": "https://test-prebid.com/openrtb2/auction",
                    "timeout_ms": 1000,
                    "bidders": ["mocktioneer"],
                    "auto_configure": false,
                    "debug": false
                }),
            )
            .expect("should update prebid config");
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
        let result = pipeline.process(Cursor::new(html.as_bytes()), &mut output);
        assert!(result.is_ok());
        let processed = String::from_utf8_lossy(&output);
        // When auto-configure is disabled, do not rewrite Prebid references
        assert!(processed.contains("/js/prebid.min.js"));
        assert!(processed.contains("cdn.prebid.org/prebid.js"));
        assert!(processed.contains("/static/tsjs=tsjs-core.min.js"));
    }

    #[test]
    fn test_rewrites_nextjs_script_when_enabled() {
        let html = r#"<html><body>
            <script id="__NEXT_DATA__" type="application/json">
                {"props":{"pageProps":{"primary":{"href":"https://origin.example.com/reviews"},"secondary":{"href":"http://origin.example.com/sign-in"},"fallbackHref":"http://origin.example.com/legacy","protoRelative":"//origin.example.com/assets/logo.png"}}}
            </script>
        </body></html>"#;

        let mut config = create_test_config();
        config.nextjs_enabled = true;
        config.nextjs_attributes = vec!["href".to_string(), "link".to_string(), "url".to_string()];
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
        println!("processed={processed}");
        println!("processed stream payload: {}", processed);
        println!("processed stream payload: {}", processed);

        assert!(
            processed.contains(r#""href":"https://test.example.com/reviews""#),
            "Should rewrite https Next.js href values"
        );
        assert!(
            processed.contains(r#""href":"https://test.example.com/sign-in""#),
            "Should rewrite http Next.js href values"
        );
        assert!(
            processed.contains(r#""fallbackHref":"http://origin.example.com/legacy""#),
            "Should leave other fields untouched"
        );
        assert!(
            processed.contains(r#""protoRelative":"//origin.example.com/assets/logo.png""#),
            "Should not rewrite non-href keys"
        );
        assert!(
            !processed.contains("\"href\":\"https://origin.example.com/reviews\""),
            "Should remove origin https href"
        );
        assert!(
            !processed.contains("\"href\":\"http://origin.example.com/sign-in\""),
            "Should remove origin http href"
        );
    }

    #[test]
    fn test_rewrites_nextjs_stream_payload() {
        let html = r#"<html><body>
            <script>
                self.__next_f.push([1,"chunk", "prefix {\"inner\":\"value\"} \\\"href\\\":\\\"http://origin.example.com/dashboard\\\", \\\"link\\\":\\\"https://origin.example.com/api-test\\\" suffix", {"href":"http://origin.example.com/secondary","dataHost":"https://origin.example.com/api"}]);
            </script>
        </body></html>"#;

        let mut config = create_test_config();
        config.nextjs_enabled = true;
        config.nextjs_attributes = vec!["href".to_string(), "link".to_string(), "url".to_string()];
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
            "Should rewrite escaped href sequences inside streamed payloads. Content: {}",
            normalized
        );
        assert!(
            normalized.contains("\"href\":\"https://test.example.com/secondary\""),
            "Should rewrite plain href attributes inside streamed payloads"
        );
        assert!(
            normalized.contains("\"link\":\"https://test.example.com/api-test\""),
            "Should rewrite additional configured attributes like link"
        );
        assert!(
            processed.contains("\"dataHost\":\"https://origin.example.com/api\""),
            "Should leave non-href properties untouched"
        );
    }

    #[test]
    fn test_nextjs_rewrite_respects_flag() {
        let html = r#"<html><body>
            <script id="__NEXT_DATA__" type="application/json">
                {"props":{"pageProps":{"href":"https://origin.example.com/reviews"}}}
            </script>
        </body></html>"#;

        let config = create_test_config();
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
            processed.contains("origin.example.com"),
            "Should leave Next.js data untouched when disabled"
        );
        assert!(
            !processed.contains("test.example.com/reviews"),
            "Should not rewrite Next.js data when flag is off"
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
            <img src="http://origin.example.com/image.jpg">
            <form action="https://origin.example.com/submit">
        </html>"#;

        let mut output = Vec::new();
        pipeline
            .process(Cursor::new(html.as_bytes()), &mut output)
            .unwrap();

        let result = String::from_utf8(output).unwrap();
        assert!(result.contains(r#"href="https://test.example.com/page""#));
        assert!(result.contains(r#"src="https://test.example.com/image.jpg""#));
        assert!(result.contains(r#"action="https://test.example.com/submit""#));
        assert!(!result.contains("origin.example.com"));
    }

    #[test]
    fn test_html_processor_config_from_settings() {
        let settings = create_test_settings();
        let registry = IntegrationRegistry::new(&settings);
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
        assert!(
            !config.nextjs_enabled,
            "Next.js rewrites should default to disabled"
        );
        assert_eq!(
            config.nextjs_attributes,
            vec!["href".to_string(), "link".to_string(), "url".to_string()],
            "Should default to rewriting href/link/url attributes"
        );
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
            .unwrap();
        let result = String::from_utf8(output).unwrap();

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
        use serde_json::json;

        let html = r#"<html><head>
            <script src="https://cdn.testlight.com/v1/testlight.js"></script>
        </head><body></body></html>"#;

        let _bundle_guard = mock_testlight_bundle();
        let mut settings = Settings::default();
        let shim_src = tsjs::integration_script_src("testlight");
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

        let registry = IntegrationRegistry::new(&settings);
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
        let expected_src = tsjs::integration_script_src("testlight");
        assert!(
            processed.contains(&expected_src),
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
        encoder.write_all(html.as_bytes()).unwrap();
        let compressed_input = encoder.finish().unwrap();

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
            .unwrap();

        // Ensure we produced output
        assert!(
            !compressed_output.is_empty(),
            "Should produce compressed output"
        );

        // Decompress and verify
        let mut decoder = GzDecoder::new(&compressed_output[..]);
        let mut decompressed = String::new();
        decoder.read_to_string(&mut decompressed).unwrap();

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
