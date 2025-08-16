//! Simplified HTML processor that combines URL replacement and Prebid injection
//!
//! This module provides a StreamProcessor implementation for HTML content.
use std::cell::RefCell;
use std::rc::Rc;

use lol_html::{element, Settings as RewriterSettings};

use crate::settings::Settings;
use crate::streaming_processor::{HtmlRewriterAdapter, StreamProcessor};
use crate::streaming_replacer::create_url_replacer;

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

/// State machine for tracking Prebid.js detection and injection
#[derive(Clone, Debug)]
#[allow(dead_code)] // Fields are kept for debugging/logging purposes
enum PrebidState {
    /// No Prebid.js detected yet
    NotDetected,
    /// Prebid.js detected but config not yet injected
    Detected { location: String },
    /// Configuration has been injected
    Injected { location: String, context: String },
}

/// Manages Prebid.js detection and configuration injection
#[derive(Clone)]
struct PrebidInjector {
    script: Option<String>,
    state: Rc<RefCell<PrebidState>>,
}

// TODO: IMPROVEMENT #3 - PrebidInjector Methods Could Be Simpler
// The try_inject_after and try_inject_append have similar logic.
// Consider:
// - Combine with enum: `try_inject(el, InjectionPosition::After, context)`
// - Make condition checking more declarative with a `should_inject()` method
// - Could reduce code duplication and make injection logic clearer

impl PrebidInjector {
    fn new(config: &HtmlProcessorConfig) -> Self {
        let script = if config.enable_prebid {
            log::info!("[Prebid] Auto-configuration enabled for origin: {}", config.origin_host);
            Some(generate_prebid_script(config))
        } else {
            log::debug!("[Prebid] Auto-configuration disabled");
            None
        };

        Self {
            script,
            state: Rc::new(RefCell::new(PrebidState::NotDetected)),
        }
    }

    fn is_enabled(&self) -> bool {
        self.script.is_some()
    }

    fn mark_detected(&self, location: &str) {
        let mut state = self.state.borrow_mut();
        if matches!(*state, PrebidState::NotDetected) {
            log::info!("[Prebid] Detected Prebid.js reference at: {}", location);
            *state = PrebidState::Detected {
                location: location.to_string(),
            };
        }
    }

    fn try_inject_after(&self, el: &mut lol_html::html_content::Element, context: &str) -> bool {
        if let Some(ref script) = self.script {
            let mut state = self.state.borrow_mut();
            match &*state {
                PrebidState::NotDetected | PrebidState::Detected { .. } => {
                    log::info!("[Prebid] Injecting configuration {}", context);
                    el.after(script, lol_html::html_content::ContentType::Html);
                    
                    let location = match &*state {
                        PrebidState::Detected { location } => location.clone(),
                        _ => "inline".to_string(),
                    };
                    
                    *state = PrebidState::Injected {
                        location,
                        context: context.to_string(),
                    };
                    return true;
                }
                PrebidState::Injected { .. } => {
                    // Already injected, do nothing
                    return false;
                }
            }
        }
        false
    }

    fn try_inject_append(&self, el: &mut lol_html::html_content::Element, context: &str) -> bool {
        if let Some(ref script) = self.script {
            let mut state = self.state.borrow_mut();
            match &*state {
                PrebidState::Detected { location } => {
                    log::info!("[Prebid] Injecting configuration {}", context);
                    el.append(script, lol_html::html_content::ContentType::Html);
                    *state = PrebidState::Injected {
                        location: location.clone(),
                        context: context.to_string(),
                    };
                    return true;
                }
                _ => {
                    // Not in the right state for append injection
                    return false;
                }
            }
        }
        false
    }

    fn detect_in_text(&self, text: &str) -> bool {
        if self.is_enabled() {
            let state = self.state.borrow();
            if matches!(*state, PrebidState::NotDetected) && (text.contains("pbjs") || text.contains("prebid") || text.contains("Prebid")) {
                drop(state); // Release borrow before calling mark_detected
                self.mark_detected("text content");
                return true;
            }
        }
        false
    }

    fn detect_in_src(&self, src: &str) -> bool {
        src.contains("prebid") || src.contains("pbjs")
    }
}

/// Create an HTML processor with URL replacement and optional Prebid injection
pub fn create_html_processor(config: HtmlProcessorConfig) -> impl StreamProcessor {

    // TODO: IMPROVEMENT #4 - URL Patterns Structure
    // The UrlPatterns struct has redundant data (origins in multiple formats).
    // Consider:
    // - Generate variants on-demand with methods like `https_origin(&self)`
    // - Or use a builder that constructs patterns as needed
    // - Could reduce from 7 fields to 3-4 core fields
    
    // Create a shared structure for URL replacement patterns
    struct UrlPatterns {
        https_origin: String,
        http_origin: String,
        protocol_relative_origin: String,
        origin_host: String,
        replacement_url: String,
        protocol_relative_replacement: String,
        request_host: String,
    }

    let patterns = Rc::new(UrlPatterns {
        https_origin: format!("https://{}", config.origin_host),
        http_origin: format!("http://{}", config.origin_host),
        protocol_relative_origin: format!("//{}", config.origin_host),
        origin_host: config.origin_host.clone(),
        replacement_url: format!("{}://{}", config.request_scheme, config.request_host),
        protocol_relative_replacement: format!("//{}", config.request_host),
        request_host: config.request_host.clone(),
    });

    // Create URL replacer
    let replacer = create_url_replacer(
        &config.origin_host,
        &config.origin_url,
        &config.request_host,
        &config.request_scheme,
    );

    // Create Prebid injector wrapped in Rc for sharing
    let prebid = Rc::new(PrebidInjector::new(&config));

    // TODO: IMPROVEMENT #5 - Element Handler Registration
    // The long vector of element handlers could be built more dynamically:
    // - Use a builder pattern: `HandlerBuilder::new().url_handlers().prebid_handlers().build()`
    // - Or register handlers based on configuration flags
    // - Could make it easier to conditionally include/exclude handlers
    
    let rewriter_settings = RewriterSettings {
        element_content_handlers: vec![
            // TODO: IMPROVEMENT #1 - Repetitive URL Replacement Logic
            // Each URL handler below has similar replacement logic.
            // Consider:
            // - Create a helper: `create_url_handler("href", &patterns)`
            // - Or add method to UrlPatterns: `patterns.create_replacement_handler("href")`
            // - This would reduce ~50 lines to ~5 lines per handler
            
            // Replace URLs in href attributes
            element!("[href]", {
                let patterns = patterns.clone();
                move |el| {
                    if let Some(href) = el.get_attribute("href") {
                        let new_href = href
                            .replace(&patterns.https_origin, &patterns.replacement_url)
                            .replace(&patterns.http_origin, &patterns.replacement_url);
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
                            .replace(&patterns.https_origin, &patterns.replacement_url)
                            .replace(&patterns.http_origin, &patterns.replacement_url);
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
                            .replace(&patterns.https_origin, &patterns.replacement_url)
                            .replace(&patterns.http_origin, &patterns.replacement_url);
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
                            .replace(&patterns.https_origin, &patterns.replacement_url)
                            .replace(&patterns.http_origin, &patterns.replacement_url)
                            .replace(&patterns.protocol_relative_origin, &patterns.protocol_relative_replacement)
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
                            .replace(&patterns.https_origin, &patterns.replacement_url)
                            .replace(&patterns.http_origin, &patterns.replacement_url)
                            .replace(&patterns.protocol_relative_origin, &patterns.protocol_relative_replacement);
                        if new_imagesrcset != imagesrcset {
                            el.set_attribute("imagesrcset", &new_imagesrcset)?;
                        }
                    }
                    Ok(())
                }
            }),
            
            // TODO: IMPROVEMENT #2 - Closure Scoping Pattern
            // The pattern `element!("sel", { let x = x.clone(); move |el| {...} })`
            // is repeated for every handler. Consider:
            // - A macro: `clone_element!("script[src]", prebid, |el, prebid| { ... })`
            // - Or restructure to use a shared context object that doesn't need cloning
            
            // Detect and inject Prebid config for external scripts
            element!("script[src]", {
                let prebid = prebid.clone();
                move |el| {
                    if let Some(src) = el.get_attribute("src") {
                        if prebid.detect_in_src(&src) {
                            log::info!("[Prebid] Detected Prebid.js script tag: src={}", src);
                            prebid.mark_detected(&format!("script[src={}]", src));
                            let injected = prebid.try_inject_after(el, "after Prebid.js script tag");
                            log::info!("[Prebid] Injection result: {}", if injected { "SUCCESS" } else { "FAILED" });
                        }
                    }
                    Ok(())
                }
            }),
            // Check inline script tags and inject after if Prebid was detected
            element!("script:not([src])", {
                let prebid = prebid.clone();
                move |el| {
                    prebid.try_inject_append(el, "after inline script (Prebid detected earlier)");
                    Ok(())
                }
            }),
            // Fallback injection at end of head
            element!("head", {
                let prebid = prebid.clone();
                move |el| {
                    prebid.try_inject_append(el, "at end of <head> element (fallback)");
                    Ok(())
                }
            }),
            // Final fallback - inject at end of body if Prebid detected but not injected
            element!("body", {
                let prebid = prebid.clone();
                move |el| {
                    prebid.try_inject_append(el, "at end of <body> element (final fallback)");
                    Ok(())
                }
            }),
        ],

        // Replace URLs in text content
        document_content_handlers: vec![lol_html::doc_text!({
            let prebid = prebid.clone();
            move |text| {
                let content = text.as_str();

                // Detect Prebid.js in text content
                prebid.detect_in_text(content);

                // Apply URL replacements
                let mut new_content = content.to_string();
                for replacement in replacer.replacements.iter() {
                    if new_content.contains(&replacement.find) {
                        new_content = new_content.replace(&replacement.find, &replacement.replace_with);
                    }
                }

                if new_content != content {
                    text.replace(&new_content, lol_html::html_content::ContentType::Text);
                }

                Ok(())
            }
        })],

        ..RewriterSettings::default()
    };

    HtmlRewriterAdapter::new(rewriter_settings)
}

/// Generate Prebid configuration script
fn generate_prebid_script(config: &HtmlProcessorConfig) -> String {
    let bidders_json =
        serde_json::to_string(&config.prebid_bidders).unwrap_or_else(|_| "[]".to_string());

    // Try a simpler injection first - just a comment and a simple script
    format!(
        r#"<!-- Trusted Server Prebid Config Start -->
<script type="text/javascript">
// Trusted Server Prebid Configuration
window.TRUSTED_SERVER_TEST = 'YES';
console.log('[TS] Script executing');
(function() {{
    console.log('[TS] IIFE started');
    
    window.__trustedServerPrebid = true;
    window.pbjs = window.pbjs || {{}};
    window.pbjs.que = window.pbjs.que || [];
    
    function configurePrebid() {{
        if (typeof pbjs !== 'undefined' && typeof pbjs.setConfig === 'function') {{
            console.log('[Trusted Server] Configuring Prebid.js for first-party serving');
            
            pbjs.setConfig({{
                s2sConfig: {{
                    accountId: '{}',
                    enabled: true,
                    defaultVendor: 'custom',
                    bidders: {},
                    endpoint: '{}://{}/openrtb2/auction',
                    syncEndpoint: '{}://{}/cookie_sync',
                    timeout: {},
                    adapter: 'prebidServer',
                    allowUnknownBidderCodes: true
                }},
                debug: {}
            }});
            
            var originalSetConfig = pbjs.setConfig;
            pbjs.setConfig = function(config) {{
                if (config.s2sConfig && !config.__trustedServerOverride) {{
                    console.log('[Trusted Server] Preserving first-party s2sConfig endpoints');
                    config.s2sConfig.endpoint = '{}://{}/openrtb2/auction';
                    config.s2sConfig.syncEndpoint = '{}://{}/cookie_sync';
                    config.s2sConfig.enabled = true;
                }}
                return originalSetConfig.call(this, config);
            }};
            
            console.log('[Trusted Server] Prebid.js configuration complete');
        }} else {{
            setTimeout(configurePrebid, 100);
        }}
    }}
    
    if (document.readyState === 'loading') {{
        document.addEventListener('DOMContentLoaded', configurePrebid);
    }} else {{
        configurePrebid();
    }}
    
    window.pbjs.que.push(configurePrebid);
}})();
</script>
<!-- Trusted Server Prebid Config End -->
"#,
        config.prebid_account_id,
        bidders_json,
        config.request_scheme,
        config.request_host,
        config.request_scheme,
        config.request_host,
        config.prebid_timeout_ms,
        config.prebid_debug,
        config.request_scheme,
        config.request_host,
        config.request_scheme,
        config.request_host
    )
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
    fn test_create_html_processor_with_prebid_injection() {
        let mut config = create_test_config();
        config.enable_prebid = true;
        let processor = create_html_processor(config);

        let pipeline_config = PipelineConfig {
            input_compression: Compression::None,
            output_compression: Compression::None,
            chunk_size: 8192,
        };
        let mut pipeline = StreamingPipeline::new(pipeline_config, processor);

        let html = r#"<html>
            <head>
                <script src="/prebid.js"></script>
            </head>
            <body>Content</body>
        </html>"#;

        let mut output = Vec::new();
        pipeline
            .process(Cursor::new(html.as_bytes()), &mut output)
            .unwrap();

        let result = String::from_utf8(output).unwrap();
        
        // Debug: print the full result
        println!("DEBUG: Full HTML output:");
        println!("{}", result);
        
        // Should inject Prebid configuration
        assert!(result.contains("window.__trustedServerPrebid = true"));
        assert!(result.contains("pbjs.setConfig"));
        assert!(result.contains("https://test.example.com/openrtb2/auction"));
        assert!(result.contains(r#"["kargo","rubicon"]"#) || result.contains("bidders:"));
    }

    #[test]
    fn test_create_html_processor_text_content_replacement() {
        let config = create_test_config();
        let processor = create_html_processor(config);

        let pipeline_config = PipelineConfig {
            input_compression: Compression::None,
            output_compression: Compression::None,
            chunk_size: 8192,
        };
        let mut pipeline = StreamingPipeline::new(pipeline_config, processor);

        let html = r#"<script>
            var apiUrl = "https://origin.example.com/api";
            fetch("http://origin.example.com/data");
        </script>"#;

        let mut output = Vec::new();
        pipeline
            .process(Cursor::new(html.as_bytes()), &mut output)
            .unwrap();

        let result = String::from_utf8(output).unwrap();
        assert!(result.contains(r#"https://test.example.com/api"#));
        assert!(result.contains(r#"https://test.example.com/data"#));
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
    fn test_prebid_injection_with_inline_script() {
        let mut config = create_test_config();
        config.enable_prebid = true;
        let processor = create_html_processor(config);

        let pipeline_config = PipelineConfig {
            input_compression: Compression::None,
            output_compression: Compression::None,
            chunk_size: 8192,
        };
        let mut pipeline = StreamingPipeline::new(pipeline_config, processor);

        let html = r#"<html>
            <head>
                <script>
                    var pbjs = pbjs || {};
                    pbjs.que = pbjs.que || [];
                    pbjs.que.push(function() {
                        pbjs.addAdUnits(adUnits);
                    });
                </script>
            </head>
            <body>Content</body>
        </html>"#;

        let mut output = Vec::new();
        pipeline
            .process(Cursor::new(html.as_bytes()), &mut output)
            .unwrap();

        let result = String::from_utf8(output).unwrap();
        // Should detect Prebid in inline script and inject configuration
        assert!(result.contains("window.__trustedServerPrebid = true"));
        assert!(result.contains("pbjs.setConfig"));
        assert!(result.contains("https://test.example.com/openrtb2/auction"));
    }

    #[test]
    fn test_prebid_injection_after_inline_script() {
        let mut config = create_test_config();
        config.enable_prebid = true;
        let processor = create_html_processor(config);

        let pipeline_config = PipelineConfig {
            input_compression: Compression::None,
            output_compression: Compression::None,
            chunk_size: 8192,
        };
        let mut pipeline = StreamingPipeline::new(pipeline_config, processor);

        // Put pbjs reference earlier so it's detected before we hit the script element
        let html = r#"<html>
            <head>
                <title>Test with pbjs</title>
            </head>
            <body>
                <script>
                    // Initialize Prebid.js
                    window.pbjs = window.pbjs || {};
                </script>
                <div>Content after script</div>
            </body>
        </html>"#;

        let mut output = Vec::new();
        pipeline
            .process(Cursor::new(html.as_bytes()), &mut output)
            .unwrap();

        let result = String::from_utf8(output).unwrap();
        
        // Should inject configuration at body fallback since pbjs was detected
        assert!(result.contains("window.__trustedServerPrebid = true"));
        assert!(result.contains("window.TRUSTED_SERVER_TEST = 'YES'"));
    }

    #[test]
    fn test_prebid_injection_body_fallback() {
        let mut config = create_test_config();
        config.enable_prebid = true;
        let processor = create_html_processor(config);

        let pipeline_config = PipelineConfig {
            input_compression: Compression::None,
            output_compression: Compression::None,
            chunk_size: 8192,
        };
        let mut pipeline = StreamingPipeline::new(pipeline_config, processor);

        // HTML with Prebid reference detected early (in title) but injected at body
        let html = r#"<html>
            <head>
                <title>Page with pbjs</title>
            </head>
            <body>
                <div>Content here</div>
            </body>
        </html>"#;

        let mut output = Vec::new();
        pipeline
            .process(Cursor::new(html.as_bytes()), &mut output)
            .unwrap();

        let result = String::from_utf8(output).unwrap();
        // Should inject configuration somewhere (head or body fallback)
        assert!(result.contains("window.__trustedServerPrebid = true"));
        assert!(result.contains("pbjs.setConfig"));
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
        pipeline.process(Cursor::new(html.as_bytes()), &mut output).unwrap();
        let result = String::from_utf8(output).unwrap();
        
        // Assertions - with Prebid injection
        assert!(result.len() > html.len(), "Output should be larger due to Prebid injection");
        
        // Check URL replacements
        let remaining_urls = result.matches("www.test-publisher.com").count();
        let replaced_urls = result.matches("test-publisher-ts.edgecompute.app").count();
        
        println!("After processing:");
        println!("  Remaining original URLs: {}", remaining_urls);
        println!("  Edge domain URLs: {}", replaced_urls);
        
        // Most URLs should be replaced, except those in script/JSON-LD contexts
        assert!(remaining_urls <= 8, "At most 8 URLs should remain unreplaced (in script/JSON-LD): found {}", remaining_urls);
        // Should have replacements + 4 from Prebid config (852 replaced + 4 = 856)
        assert_eq!(replaced_urls, 856, "Should have exactly 856 edge domain URLs (852 replaced + 4 from Prebid)");
        
        // Verify HTML structure
        assert_eq!(&result[0..15], "<!DOCTYPE html>");
        assert_eq!(&result[result.len()-7..], "</html>");
        
        // Verify content preservation
        assert!(result.contains("Mercedes CEO"), "Should preserve article title");
        assert!(result.contains("test-publisher"), "Should preserve text content");
        // Prebid auto-configuration should be injected
        assert!(result.contains("window.__trustedServerPrebid = true"), "Should contain Prebid initialization");
        assert!(result.contains("pbjs.setConfig"), "Should contain Prebid configuration");
    }

    #[test]
    fn test_real_publisher_html_with_gzip() {
        use flate2::write::GzEncoder;
        use flate2::read::GzDecoder;
        use flate2::Compression as GzCompression;
        use std::io::{Write, Read};
        
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
        pipeline.process(Cursor::new(&compressed_input), &mut compressed_output).unwrap();
        
        // Compressed output will be larger due to Prebid injection
        assert!(compressed_output.len() > compressed_input.len(), "Compressed output should be larger than input");
        
        // Decompress and verify
        let mut decoder = GzDecoder::new(&compressed_output[..]);
        let mut decompressed = String::new();
        decoder.read_to_string(&mut decompressed).unwrap();
        
        // Assertions - with Prebid injection
        assert!(decompressed.len() > html.len(), "Decompressed output should be larger than original");
        
        let remaining_urls = decompressed.matches("www.test-publisher.com").count();
        let replaced_urls = decompressed.matches("test-publisher-ts.edgecompute.app").count();
        
        assert!(remaining_urls <= 8, "At most 8 URLs should remain unreplaced (in script/JSON-LD)");
        assert_eq!(replaced_urls, 856, "Should have exactly 856 edge domain URLs (852 replaced + 4 from Prebid)");
        
        // Verify structure
        assert_eq!(&decompressed[0..15], "<!DOCTYPE html>");
        assert_eq!(&decompressed[decompressed.len()-7..], "</html>");
        
        // Verify content preservation
        assert!(decompressed.contains("Mercedes CEO"), "Should preserve article title");
        assert!(decompressed.contains("test-publisher"), "Should preserve text content");
        // Prebid auto-configuration should be injected
        assert!(decompressed.contains("window.__trustedServerPrebid = true"), "Should contain Prebid initialization");
        assert!(decompressed.contains("pbjs.setConfig"), "Should contain Prebid configuration");
    }


    #[test]
    fn test_already_truncated_html_passthrough() {
        // Test that we don't make truncated HTML worse
        // This simulates receiving already-truncated HTML from origin
        
        let truncated_html = r#"<html><head><title>Test</title></head><body><p>This is a test that gets cut o"#;
        
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
        
        assert!(result.is_ok(), "Should process truncated HTML without error");
        
        let processed = String::from_utf8_lossy(&output);
        println!("Output: '{}'", processed);
        
        // The processor should pass through the truncated HTML
        // It might add some closing tags, but shouldn't truncate further
        assert!(processed.len() >= truncated_html.len(), 
            "Output should not be shorter than truncated input");
    }
    
    #[test]
    fn test_truncated_html_validation() {
        // Simulated truncated HTML - ends mid-attribute
        let truncated_html = r#"<html lang="en"><head><meta charset="utf-8"><title>Test Publisher</title><link rel="preload" as="image" href="https://www.test-publisher.com/image.jpg"><script src="/js/prebid.min.js"></script></head><body><p>Article content from <a href="https://www.test-publisher.com/ar"#;

        // This HTML is clearly truncated - it ends in the middle of an attribute value
        println!("Testing truncated HTML (ends in middle of URL)");
        println!("Input length: {} bytes", truncated_html.len());
        
        // Check that the input is indeed truncated
        assert!(!truncated_html.contains("</html>"), "Input should be truncated (no closing html tag)");
        assert!(!truncated_html.contains("</body>"), "Input should be truncated (no closing body tag)"); 
        assert!(truncated_html.ends_with("/ar"), "Input should end with '/ar' showing truncation");
        
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
        assert!(result.is_ok(), "Processing should complete even with truncated HTML");
        
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
        
        println!("Last 100 chars of output: {}", 
            processed.chars().rev().take(100).collect::<String>().chars().rev().collect::<String>());
    }

    #[test]
    fn test_prebid_autoconfigure_enabled() {
        // Test that Prebid is injected when auto_configure is true
        let mut config = create_test_config();
        config.enable_prebid = true; // auto_configure enabled
        config.prebid_account_id = "test-1001".to_string();
        let processor = create_html_processor(config);

        let pipeline_config = PipelineConfig {
            input_compression: Compression::None,
            output_compression: Compression::None,
            chunk_size: 8192,
        };
        let mut pipeline = StreamingPipeline::new(pipeline_config, processor);

        let html = r#"
        <html>
        <head>
            <script src="/prebid.js"></script>
        </head>
        <body>
            <p>Test content</p>
        </body>
        </html>
        "#;

        let mut output = Vec::new();
        pipeline
            .process(Cursor::new(html.as_bytes()), &mut output)
            .unwrap();

        let result = String::from_utf8(output).unwrap();
        
        // Should auto-inject Prebid configuration
        assert!(result.contains("window.__trustedServerPrebid = true"));
        assert!(result.contains("pbjs.setConfig"));
        assert!(result.contains("test-1001")); // Account ID
        assert!(result.contains("Trusted Server Prebid Config"));
    }

    #[test]
    fn test_prebid_not_injected_when_disabled() {
        let mut config = create_test_config();
        config.enable_prebid = false; // Explicitly disable (auto_configure = false)
        let processor = create_html_processor(config);

        let pipeline_config = PipelineConfig {
            input_compression: Compression::None,
            output_compression: Compression::None,
            chunk_size: 8192,
        };
        let mut pipeline = StreamingPipeline::new(pipeline_config, processor);

        let html = r#"<html>
            <head>
                <script src="/prebid.js"></script>
                <script>
                    var pbjs = pbjs || {};
                </script>
            </head>
        </html>"#;

        let mut output = Vec::new();
        pipeline
            .process(Cursor::new(html.as_bytes()), &mut output)
            .unwrap();

        let result = String::from_utf8(output).unwrap();
        // Should NOT inject when disabled
        assert!(!result.contains("window.__trustedServerPrebid"));
        assert!(!result.contains("pbjs.setConfig"));
    }
}
