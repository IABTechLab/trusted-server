//! Prebid.js automatic configuration injection for proxied pages.
//!
//! This module automatically detects Prebid.js in HTML content and injects
//! configuration to ensure all requests go through the first-party domain.
//! Uses lol_html for efficient streaming HTML processing.

use error_stack::Report;
use lol_html::{element, html_content, HtmlRewriter, Settings as RewriterSettings};
use std::cell::RefCell;
use std::rc::Rc;

use crate::error::TrustedServerError;
use crate::settings::Settings;

/// Creates a streaming HTML rewriter that injects Prebid.js configuration.
///
/// This rewriter:
/// 1. Detects Prebid.js indicators in the HTML stream
/// 2. Injects configuration script at the optimal location
/// 3. Ensures the configuration overrides any existing settings
///
/// # Errors
///
/// Returns a [`TrustedServerError`] if rewriter creation fails
pub fn create_prebid_rewriter<'h, O>(
    output: O,
    request_host: &str,
    request_scheme: &str,
    settings: &Settings,
) -> Result<HtmlRewriter<'h, O>, Report<TrustedServerError>>
where
    O: lol_html::OutputSink + 'h,
{
    // Skip if auto-configuration is disabled
    if !settings.prebid.auto_configure {
        let rewriter_settings = RewriterSettings::default();
        return Ok(HtmlRewriter::new(rewriter_settings, output));
    }

    // Generate the configuration script
    let config_script = generate_prebid_config_script(request_host, request_scheme, settings);

    // Track whether we've found Prebid.js and injected config
    let prebid_detected = Rc::new(RefCell::new(false));
    let config_injected = Rc::new(RefCell::new(false));

    // Clone for use in closures
    let prebid_detected_clone1 = prebid_detected.clone();
    let prebid_detected_clone2 = prebid_detected.clone();
    let _prebid_detected_clone3 = prebid_detected.clone();
    let config_injected_clone1 = config_injected.clone();
    let config_injected_clone2 = config_injected.clone();
    let _config_injected_clone3 = config_injected.clone();
    let config_script_clone1 = config_script.clone();
    let config_script_clone2 = config_script.clone();

    let rewriter_settings = RewriterSettings {
        element_content_handlers: vec![
            // Detect Prebid.js in script tags and inject after
            element!("script", move |el| {
                // Check script src for prebid
                if let Some(src) = el.get_attribute("src") {
                    if src.contains("prebid") || src.contains("pbjs") {
                        *prebid_detected_clone1.borrow_mut() = true;
                        log::info!("Detected Prebid.js script tag: {}", src);

                        // Inject config after this script if not already done
                        if !*config_injected_clone1.borrow() {
                            el.after(&config_script_clone1, html_content::ContentType::Html);
                            *config_injected_clone1.borrow_mut() = true;
                            log::info!("Injected Prebid config after script tag");
                        }
                    }
                }

                Ok(())
            }),
            // Check for inline scripts that use pbjs
            element!("script:not([src])", move |el| {
                // If we haven't injected yet but detected prebid
                if *prebid_detected_clone2.borrow() && !*config_injected_clone2.borrow() {
                    el.after(&config_script_clone2, html_content::ContentType::Html);
                    *config_injected_clone2.borrow_mut() = true;
                    log::info!("Injected Prebid config after inline script");
                }

                Ok(())
            }),
            // Fallback injection point at end of head
            element!("head", move |el| {
                // When processing head tag, check if we should inject
                // Use before_close to inject at end of head
                el.prepend(&config_script, html_content::ContentType::Html);
                log::info!("Injected Prebid config in head tag");
                Ok(())
            }),
        ],

        // Text handler to detect pbjs in inline scripts
        document_content_handlers: vec![lol_html::doc_text!(move |text| {
            let content = text.as_str();
            if content.contains("pbjs")
                || content.contains("prebid")
                || content.contains("pbjs.que")
                || content.contains("pbjs.addAdUnits")
                || content.contains("pbjs.setConfig")
            {
                *prebid_detected.borrow_mut() = true;
                log::debug!("Detected Prebid.js in text content");
            }
            Ok(())
        })],

        ..RewriterSettings::default()
    };

    Ok(HtmlRewriter::new(rewriter_settings, output))
}

/// Detects if the HTML chunk contains Prebid.js indicators.
///
/// This is used for quick detection without full parsing.
pub fn detect_prebid_in_chunk(chunk: &[u8]) -> bool {
    // Convert to string for searching (lossy is OK for detection)
    let content = String::from_utf8_lossy(chunk);

    content.contains("pbjs")
        || content.contains("prebid.js")
        || content.contains("/prebid")
        || content.contains("pbjs.que")
        || content.contains("pbjs.")
        || content.contains("window.pbjs")
}

/// Generates the Prebid configuration script to inject.
pub fn generate_prebid_config_script(
    request_host: &str,
    request_scheme: &str,
    settings: &Settings,
) -> String {
    let bidders_json =
        serde_json::to_string(&settings.prebid.bidders).unwrap_or_else(|_| "[]".to_string());

    format!(
        r#"
<script data-trusted-server="prebid-config">
(function() {{
    'use strict';
    
    // Mark that we've injected configuration
    window.__trustedServerPrebid = true;
    
    // Ensure pbjs exists
    window.pbjs = window.pbjs || {{}};
    window.pbjs.que = window.pbjs.que || [];
    
    // Configuration function
    function configurePrebid() {{
        if (typeof pbjs !== 'undefined' && typeof pbjs.setConfig === 'function') {{
            console.log('[Trusted Server] Configuring Prebid.js for first-party serving');
            
            // Force s2sConfig to use our first-party endpoints
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
                    allowUnknownBidderCodes: true,
                    coopSync: true,
                    userSyncLimit: 10
                }},
                userSync: {{
                    syncEnabled: true,
                    pixelEnabled: true,
                    iframeEnabled: true,
                    syncsPerBidder: 5,
                    syncDelay: 3000,
                    auctionDelay: 0,
                    filterSettings: {{
                        all: {{
                            bidders: '*',
                            filter: 'include'
                        }}
                    }}
                }},
                debug: {}
            }});
            
            // Override any subsequent setConfig calls to preserve our settings
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
            // Retry if Prebid.js not loaded yet
            setTimeout(configurePrebid, 100);
        }}
    }}
    
    // Start configuration
    if (document.readyState === 'loading') {{
        document.addEventListener('DOMContentLoaded', configurePrebid);
    }} else {{
        configurePrebid();
    }}
    
    // Also push to queue for safety
    window.pbjs.que.push(configurePrebid);
}})();
</script>
"#,
        settings.prebid.account_id,
        bidders_json,
        request_scheme,
        request_host,
        request_scheme,
        request_host,
        settings.prebid.timeout_ms,
        settings.prebid.debug,
        request_scheme,
        request_host,
        request_scheme,
        request_host
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::tests::create_test_settings;

    #[test]
    fn test_detect_prebid_in_chunk() {
        assert!(detect_prebid_in_chunk(b"var pbjs = pbjs || {};"));
        assert!(detect_prebid_in_chunk(
            b"<script src='/prebid.js'></script>"
        ));
        assert!(detect_prebid_in_chunk(b"pbjs.que.push(function() {"));
        assert!(detect_prebid_in_chunk(b"pbjs.addAdUnits(adUnits);"));
        assert!(!detect_prebid_in_chunk(b"<html><body>Hello</body></html>"));
    }

    #[test]
    fn test_generate_config_script() {
        let settings = create_test_settings();
        let script = generate_prebid_config_script("example.com", "https", &settings);

        assert!(script.contains("https://example.com/openrtb2/auction"));
        assert!(script.contains("https://example.com/cookie_sync"));
        assert!(script.contains("window.__trustedServerPrebid = true"));
        assert!(script.contains("[\"kargo\",\"rubicon\",\"appnexus\",\"openx\"]"));
    }

    #[test]
    fn test_rewriter_creation() {
        let settings = create_test_settings();
        let mut output = Vec::new();

        let result = create_prebid_rewriter(
            |chunk: &[u8]| {
                output.extend_from_slice(chunk);
            },
            "example.com",
            "https",
            &settings,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_rewriter_with_disabled_config() {
        let mut settings = create_test_settings();
        settings.prebid.auto_configure = false;
        let mut output = Vec::new();

        let result = create_prebid_rewriter(
            |chunk: &[u8]| {
                output.extend_from_slice(chunk);
            },
            "example.com",
            "https",
            &settings,
        );
        assert!(result.is_ok());
    }
}
