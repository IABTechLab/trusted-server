//! Simplified HTML processor that combines URL replacement and Prebid injection
//!
//! This module provides a StreamProcessor implementation for HTML content.
use std::cell::Cell;
use std::rc::Rc;

use lol_html::{element, html_content::ContentType, Settings as RewriterSettings};

use crate::settings::Settings;
use crate::streaming_processor::{HtmlRewriterAdapter, StreamProcessor};

/// Configuration for HTML processing
#[derive(Clone)]
pub struct HtmlProcessorConfig {
    pub origin_host: String,
    pub origin_url: String,
    pub request_host: String,
    pub request_scheme: String,
    pub enable_prebid: bool,
    pub prebid_account_id: String,
    pub prebid_bidders: Vec<String>,
    pub prebid_timeout_ms: u32,
    pub prebid_debug: bool,
}

impl HtmlProcessorConfig {
    /// Create from settings and request parameters
    pub fn from_settings(
        settings: &Settings,
        origin_host: &str,
        origin_url: &str,
        request_host: &str,
        request_scheme: &str,
    ) -> Self {
        Self {
            origin_host: origin_host.to_string(),
            origin_url: origin_url.to_string(),
            request_host: request_host.to_string(),
            request_scheme: request_scheme.to_string(),
            enable_prebid: settings.prebid.auto_configure,
            prebid_account_id: settings.prebid.account_id.clone(),
            prebid_bidders: settings.prebid.bidders.clone(),
            prebid_timeout_ms: settings.prebid.timeout_ms,
            prebid_debug: settings.prebid.debug,
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
    }

    let patterns = Rc::new(UrlPatterns {
        origin_host: config.origin_host.clone(),
        request_host: config.request_host.clone(),
        request_scheme: config.request_scheme.clone(),
    });

    let injected_tsjs = Rc::new(Cell::new(false));
    let tsjs_loader: &'static str = include_str!("../../../static/tsjs/tsjs.html");

    let rewriter_settings = RewriterSettings {
        element_content_handlers: vec![
            // Inject tsjs once at the start of <head>
            element!("head", {
                let injected_tsjs = injected_tsjs.clone();
                move |el| {
                    if !injected_tsjs.get() {
                        el.prepend(tsjs_loader, ContentType::Html);
                        injected_tsjs.set(true);
                    }
                    Ok(())
                }
            }),
            // Replace URLs in href attributes
            element!("[href]", {
                let patterns = patterns.clone();
                move |el| {
                    if let Some(href) = el.get_attribute("href") {
                        let new_href = href
                            .replace(&patterns.https_origin(), &patterns.replacement_url())
                            .replace(&patterns.http_origin(), &patterns.replacement_url());
                        if new_href != href {
                            el.set_attribute("href", &new_href)?;
                        }
                    }
                    Ok(())
                }
            }),
            // Replace URLs in src attributes
            element!("[src]", {
                let patterns = patterns.clone();
                move |el| {
                    if let Some(src) = el.get_attribute("src") {
                        let new_src = src
                            .replace(&patterns.https_origin(), &patterns.replacement_url())
                            .replace(&patterns.http_origin(), &patterns.replacement_url());
                        if new_src != src {
                            el.set_attribute("src", &new_src)?;
                        }
                    }
                    Ok(())
                }
            }),
            // Replace URLs in action attributes
            element!("[action]", {
                let patterns = patterns.clone();
                move |el| {
                    if let Some(action) = el.get_attribute("action") {
                        let new_action = action
                            .replace(&patterns.https_origin(), &patterns.replacement_url())
                            .replace(&patterns.http_origin(), &patterns.replacement_url());
                        if new_action != action {
                            el.set_attribute("action", &new_action)?;
                        }
                    }
                    Ok(())
                }
            }),
            // Replace URLs in srcset attributes (for responsive images)
            element!("[srcset]", {
                let patterns = patterns.clone();
                move |el| {
                    if let Some(srcset) = el.get_attribute("srcset") {
                        let new_srcset = srcset
                            .replace(&patterns.https_origin(), &patterns.replacement_url())
                            .replace(&patterns.http_origin(), &patterns.replacement_url())
                            .replace(
                                &patterns.protocol_relative_origin(),
                                &patterns.protocol_relative_replacement(),
                            )
                            .replace(&patterns.origin_host, &patterns.request_host);

                        if new_srcset != srcset {
                            el.set_attribute("srcset", &new_srcset)?;
                        }
                    }
                    Ok(())
                }
            }),
            // Replace URLs in imagesrcset attributes (for link preload)
            element!("[imagesrcset]", {
                let patterns = patterns.clone();
                move |el| {
                    if let Some(imagesrcset) = el.get_attribute("imagesrcset") {
                        let new_imagesrcset = imagesrcset
                            .replace(&patterns.https_origin(), &patterns.replacement_url())
                            .replace(&patterns.http_origin(), &patterns.replacement_url())
                            .replace(
                                &patterns.protocol_relative_origin(),
                                &patterns.protocol_relative_replacement(),
                            );
                        if new_imagesrcset != imagesrcset {
                            el.set_attribute("imagesrcset", &new_imagesrcset)?;
                        }
                    }
                    Ok(())
                }
            }),
        ],

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
    use std::io::Cursor;

    fn create_test_config() -> HtmlProcessorConfig {
        HtmlProcessorConfig {
            origin_host: "origin.example.com".to_string(),
            origin_url: "https://origin.example.com".to_string(),
            request_host: "test.example.com".to_string(),
            request_scheme: "https".to_string(),
            enable_prebid: false,
            prebid_account_id: "test-account".to_string(),
            prebid_bidders: vec!["kargo".to_string(), "rubicon".to_string()],
            prebid_timeout_ms: 1000,
            prebid_debug: false,
        }
    }

    #[test]
    fn test_injects_tsjs_script_and_keeps_prebid_refs() {
        let html = r#"<html><head>
            <script src="/js/prebid.min.js"></script>
            <link rel="preload" as="script" href="https://cdn.prebid.org/prebid.js" />
        </head><body></body></html>"#;

        let config = create_test_config();
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
        assert!(processed.contains("/static/tsjs.min.js"));
        // We no longer rewrite Prebid references; they will be shimmed server-side
        assert!(processed.contains("prebid.min.js"));
        assert!(processed.contains("cdn.prebid.org/prebid.js"));
    }

    #[test]
    fn test_injects_tsjs_script_when_prebid_has_query_string() {
        let html = r#"<html><head>
            <script src="/wp-content/plugins/prebidjs/js/prebidjs.min.js?v=1.2.3"></script>
        </head><body></body></html>"#;

        let config = create_test_config();
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
        assert!(processed.contains("/static/tsjs.min.js"));
        assert!(processed.contains("prebidjs.min.js"));
    }

    #[test]
    fn test_always_injects_tsjs_script() {
        let html = r#"<html><head>
            <script src="/js/prebid.min.js"></script>
            <link rel="preload" as="script" href="https://cdn.prebid.org/prebid.js" />
        </head><body></body></html>"#;

        let mut config = create_test_config();
        config.enable_prebid = false; // No longer affects tsjs injection
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
        assert!(processed.contains("/js/prebid.min.js"));
        assert!(processed.contains("cdn.prebid.org/prebid.js"));
        assert!(processed.contains("/static/tsjs.min.js"));
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
        use crate::test_support::tests::create_test_settings;

        let settings = create_test_settings();
        let config = HtmlProcessorConfig::from_settings(
            &settings,
            "origin.test-publisher.com",
            "https://origin.test-publisher.com",
            "proxy.example.com",
            "https",
        );

        assert_eq!(config.origin_host, "origin.test-publisher.com");
        assert_eq!(config.origin_url, "https://origin.test-publisher.com");
        assert_eq!(config.request_host, "proxy.example.com");
        assert_eq!(config.request_scheme, "https");
        assert!(config.enable_prebid); // Uses default true
        assert_eq!(config.prebid_account_id, "1001"); // Uses default
        assert_eq!(config.prebid_bidders.len(), 4); // Uses default bidders
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
        config.origin_url = "https://www.test-publisher.com".to_string();
        config.request_host = "test-publisher-ts.edgecompute.app".to_string();
        config.enable_prebid = true; // Enable Prebid auto-configuration

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
        config.origin_url = "https://www.test-publisher.com".to_string();
        config.request_host = "test-publisher-ts.edgecompute.app".to_string();
        config.enable_prebid = true;

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
        config.origin_url = "https://www.test-publisher.com".to_string();
        config.request_host = "test-publisher-ts.edgecompute.app".to_string();
        config.enable_prebid = true;

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
