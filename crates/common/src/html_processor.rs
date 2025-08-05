//! Simplified HTML processor that combines URL replacement and Prebid injection
//!
//! This module provides a StreamProcessor implementation for HTML content.

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

/// Create an HTML processor with URL replacement and optional Prebid injection
pub fn create_html_processor(config: HtmlProcessorConfig) -> impl StreamProcessor {
    use std::cell::RefCell;
    use std::rc::Rc;

    // Build replacement patterns
    let href_find = format!("https://{}", config.origin_host);
    let href_replace = format!("{}://{}", config.request_scheme, config.request_host);
    let href_find2 = format!("http://{}", config.origin_host);

    // Clone for closures
    let href_find_1 = href_find.clone();
    let href_find_2 = href_find.clone();
    let href_find_3 = href_find.clone();
    let href_replace_1 = href_replace.clone();
    let href_replace_2 = href_replace.clone();
    let href_replace_3 = href_replace.clone();
    let href_find2_1 = href_find2.clone();
    let href_find2_2 = href_find2.clone();
    let href_find2_3 = href_find2.clone();

    // Create URL replacer
    let replacer = create_url_replacer(
        &config.origin_host,
        &config.origin_url,
        &config.request_host,
        &config.request_scheme,
    );

    // Generate Prebid config script if enabled
    let prebid_script = if config.enable_prebid {
        Some(generate_prebid_script(&config))
    } else {
        None
    };

    let prebid_script_1 = prebid_script.clone();
    let prebid_script_2 = prebid_script.clone();

    // Tracking state for injection (using RefCell for interior mutability)
    let prebid_detected = Rc::new(RefCell::new(false));
    let config_injected = Rc::new(RefCell::new(false));

    let prebid_detected_1 = prebid_detected.clone();
    let prebid_detected_2 = prebid_detected.clone();
    let prebid_detected_3 = prebid_detected.clone();
    let config_injected_1 = config_injected.clone();
    let config_injected_2 = config_injected.clone();
    let _config_injected_3 = config_injected.clone();

    let enable_prebid = config.enable_prebid;

    let rewriter_settings = RewriterSettings {
        element_content_handlers: vec![
            // Replace URLs in href attributes
            element!("[href]", move |el| {
                if let Some(href) = el.get_attribute("href") {
                    let new_href = href
                        .replace(&href_find_1, &href_replace_1)
                        .replace(&href_find2_1, &href_replace_1);
                    if new_href != href {
                        el.set_attribute("href", &new_href)?;
                    }
                }
                Ok(())
            }),
            // Replace URLs in src attributes
            element!("[src]", move |el| {
                if let Some(src) = el.get_attribute("src") {
                    let new_src = src
                        .replace(&href_find_2, &href_replace_2)
                        .replace(&href_find2_2, &href_replace_2);
                    if new_src != src {
                        el.set_attribute("src", &new_src)?;
                    }
                }
                Ok(())
            }),
            // Replace URLs in action attributes
            element!("[action]", move |el| {
                if let Some(action) = el.get_attribute("action") {
                    let new_action = action
                        .replace(&href_find_3, &href_replace_3)
                        .replace(&href_find2_3, &href_replace_3);
                    if new_action != action {
                        el.set_attribute("action", &new_action)?;
                    }
                }
                Ok(())
            }),
            // Detect and inject Prebid config
            element!("script", move |el| {
                if let Some(ref script) = prebid_script_1 {
                    if let Some(src) = el.get_attribute("src") {
                        if (src.contains("prebid") || src.contains("pbjs"))
                            && !*config_injected_1.borrow()
                        {
                            el.after(script, lol_html::html_content::ContentType::Html);
                            *config_injected_1.borrow_mut() = true;
                            *prebid_detected_1.borrow_mut() = true;
                        }
                    }
                }
                Ok(())
            }),
            // Fallback injection in head
            element!("head", move |el| {
                if let Some(ref script) = prebid_script_2 {
                    if *prebid_detected_2.borrow() && !*config_injected_2.borrow() {
                        el.append(script, lol_html::html_content::ContentType::Html);
                        *config_injected_2.borrow_mut() = true;
                    }
                }
                Ok(())
            }),
        ],

        // Replace URLs in text content
        document_content_handlers: vec![lol_html::doc_text!(move |text| {
            let content = text.as_str();

            // Detect Prebid.js
            if enable_prebid
                && !*prebid_detected_3.borrow()
                && (content.contains("pbjs") || content.contains("prebid"))
            {
                *prebid_detected_3.borrow_mut() = true;
            }

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
        })],

        ..RewriterSettings::default()
    };

    HtmlRewriterAdapter::new(rewriter_settings)
}

/// Generate Prebid configuration script
fn generate_prebid_script(config: &HtmlProcessorConfig) -> String {
    let bidders_json =
        serde_json::to_string(&config.prebid_bidders).unwrap_or_else(|_| "[]".to_string());

    format!(
        r#"
<script data-trusted-server="prebid-config">
(function() {{
    'use strict';
    
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
        // Should inject Prebid configuration
        assert!(result.contains("window.__trustedServerPrebid = true"));
        assert!(result.contains("pbjs.setConfig"));
        assert!(result.contains("https://test.example.com/openrtb2/auction"));
        assert!(result.contains(r#"["kargo","rubicon"]"#));
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
}
