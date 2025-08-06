use std::collections::HashMap;

use error_stack::Report;
use fastly::http::header;
use fastly::{Request, Response};

use crate::error::TrustedServerError;
use crate::settings::Settings;

/// Manages partner-specific URL rewriting and proxy configurations
pub struct PartnerManager {
    /// Map of original domain -> proxy domain for rewriting URLs
    domain_mappings: HashMap<String, String>,
    /// Map of original domain -> backend name for proxying requests
    backend_mappings: HashMap<String, String>,
}

impl PartnerManager {
    /// Create a new PartnerManager from settings
    pub fn from_settings(settings: &Settings) -> Self {
        let mut domain_mappings = HashMap::new();
        let mut backend_mappings = HashMap::new();

        if let Some(partners) = &settings.partners {
            // Process GAM partner config
            if let Some(gam) = &partners.gam {
                if gam.enabled {
                    for domain in &gam.domains_to_proxy {
                        domain_mappings.insert(domain.clone(), gam.proxy_domain.clone());
                        backend_mappings.insert(domain.clone(), gam.backend_name.clone());
                    }
                }
            }

            // Process Equativ partner config
            if let Some(equativ) = &partners.equativ {
                if equativ.enabled {
                    for domain in &equativ.domains_to_proxy {
                        domain_mappings.insert(domain.clone(), equativ.proxy_domain.clone());
                        backend_mappings.insert(domain.clone(), equativ.backend_name.clone());
                    }
                }
            }

            // Process Prebid partner config
            if let Some(prebid) = &partners.prebid {
                if prebid.enabled {
                    for domain in &prebid.domains_to_proxy {
                        domain_mappings.insert(domain.clone(), prebid.proxy_domain.clone());
                        backend_mappings.insert(domain.clone(), prebid.backend_name.clone());
                    }
                }
            }
        }

        Self {
            domain_mappings,
            backend_mappings,
        }
    }

    /// Rewrite a URL to use the configured proxy domain
    pub fn rewrite_url(&self, original_url: &str) -> String {
        let mut rewritten_url = original_url.to_string();

        for (original_domain, proxy_domain) in &self.domain_mappings {
            if rewritten_url.contains(original_domain) {
                rewritten_url = rewritten_url.replace(original_domain, proxy_domain);
                // Only replace the first match to avoid multiple replacements
                break;
            }
        }

        rewritten_url
    }

    /// Get the backend name for a given domain (for proxying)
    pub fn get_backend_for_domain(&self, domain: &str) -> Option<&str> {
        self.backend_mappings.get(domain).map(|s| s.as_str())
    }

    /// Check if a domain should be proxied
    pub fn should_proxy_domain(&self, domain: &str) -> bool {
        self.domain_mappings.contains_key(domain)
    }

    /// Get all domains that should be proxied
    pub fn get_proxied_domains(&self) -> Vec<&String> {
        self.domain_mappings.keys().collect()
    }

    /// Rewrite multiple URLs in a text content (for HTML/JS content)
    pub fn rewrite_content(&self, content: &str) -> String {
        let mut rewritten_content = content.to_string();

        for (original_domain, proxy_domain) in &self.domain_mappings {
            // Use regex-like replacement for all occurrences
            rewritten_content = rewritten_content.replace(original_domain, proxy_domain);
        }

        rewritten_content
    }
}

/// Handles direct asset serving for partner domains (like auburndao.com).
///
/// Fetches assets from original partner domains and serves them as first-party content.
/// This bypasses ad blockers and Safari ITP by making all assets appear to come from edgepubs.com.
///
/// # Errors
///
/// Returns a [`TrustedServerError`] if asset fetching fails.
pub async fn handle_partner_asset(
    _settings: &Settings,
    req: Request,
) -> Result<Response, Report<TrustedServerError>> {
    let path = req.get_path();
    println!("=== HANDLING PARTNER ASSET: {} ===", path);
    log::info!("Handling partner asset request: {}", path);

    // Only handle Equativ/Smart AdServer assets (matching auburndao.com approach)
    let (backend_name, original_host) = ("equativ_sascdn_backend", "creatives.sascdn.com");

    log::info!(
        "Serving asset from backend: {} (original host: {})",
        backend_name,
        original_host
    );

    // Construct full URL using the original host and path
    let full_url = format!("https://{}{}", original_host, path);
    log::info!("Fetching asset URL: {}", full_url);

    let mut asset_req = Request::new(req.get_method().clone(), &full_url);

    // Copy all headers from original request
    for (name, value) in req.get_headers() {
        asset_req.set_header(name, value);
    }

    // Set the Host header to the original domain for proper routing
    asset_req.set_header(header::HOST, original_host);

    // Send to appropriate backend
    match asset_req.send(backend_name) {
        Ok(mut response) => {
            // Match auburndao.com cache control exactly
            let cache_control = "max-age=31536000";

            // No content rewriting needed for Equativ assets (they're mostly images)
            // This matches the auburndao.com approach of serving assets directly

            // Match auburndao.com headers exactly - no modifications
            response.set_header(header::CACHE_CONTROL, cache_control);

            // Don't modify any other headers - keep them exactly as auburndao.com gets them

            println!("=== ASSET RESPONSE HEADERS FOR {} ===", path);
            for (name, value) in response.get_headers() {
                println!("  {}: {:?}", name, value);
            }

            // No special CORB handling needed for Equativ image assets

            log::info!(
                "Partner asset served successfully, cache-control: {}",
                cache_control
            );
            Ok(response)
        }
        Err(e) => {
            log::error!(
                "Error fetching partner asset from {} (original host: {}): {:?}",
                backend_name,
                original_host,
                e
            );
            Err(Report::new(TrustedServerError::Gam {
                message: format!(
                    "Failed to fetch partner asset from {} ({}): path={}, error={:?}",
                    backend_name, original_host, path, e
                ),
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{PartnerConfig, Partners, Settings};

    use crate::test_support::tests::create_test_settings;

    #[test]
    fn test_url_rewriting() {
        let settings = create_test_settings();
        let manager = PartnerManager::from_settings(&settings);

        // Test GAM URL rewriting
        let gam_url = "https://tpc.googlesyndication.com/simgad/12184163379128326694";
        let rewritten = manager.rewrite_url(gam_url);
        assert_eq!(
            rewritten,
            "https://creatives.auburndao.com/simgad/12184163379128326694"
        );

        // Test Equativ URL rewriting
        let equativ_url = "https://creatives.sascdn.com/diff/12345/creative.jpg";
        let rewritten = manager.rewrite_url(equativ_url);
        assert_eq!(
            rewritten,
            "https://creatives.auburndao.com/diff/12345/creative.jpg"
        );

        // Test non-matching URL (should remain unchanged)
        let other_url = "https://example.com/image.jpg";
        let rewritten = manager.rewrite_url(other_url);
        assert_eq!(rewritten, "https://example.com/image.jpg");
    }

    #[test]
    fn test_backend_mapping() {
        let settings = create_test_settings();
        let manager = PartnerManager::from_settings(&settings);

        assert_eq!(
            manager.get_backend_for_domain("tpc.googlesyndication.com"),
            Some("gam_proxy_backend")
        );
        assert_eq!(
            manager.get_backend_for_domain("creatives.sascdn.com"),
            Some("equativ_proxy_backend")
        );
        assert_eq!(manager.get_backend_for_domain("unknown.domain.com"), None);
    }

    #[test]
    fn test_content_rewriting() {
        let settings = create_test_settings();
        let manager = PartnerManager::from_settings(&settings);

        let html_content = r#"
            <img src="https://tpc.googlesyndication.com/simgad/123">
            <script src="https://securepubads.g.doubleclick.net/gpt/pubads.js"></script>
            <img src="https://creatives.sascdn.com/creative.jpg">
        "#;

        let rewritten = manager.rewrite_content(html_content);

        assert!(rewritten.contains("https://creatives.auburndao.com/simgad/123"));
        assert!(rewritten.contains("https://creatives.auburndao.com/gpt/pubads.js"));
        assert!(rewritten.contains("https://creatives.auburndao.com/creative.jpg"));
        assert!(!rewritten.contains("tpc.googlesyndication.com"));
        assert!(!rewritten.contains("securepubads.g.doubleclick.net"));
        assert!(!rewritten.contains("creatives.sascdn.com"));
    }

    #[test]
    fn test_should_proxy_domain() {
        let settings = create_test_settings();
        let manager = PartnerManager::from_settings(&settings);

        assert!(manager.should_proxy_domain("tpc.googlesyndication.com"));
        assert!(manager.should_proxy_domain("securepubads.g.doubleclick.net"));
        assert!(manager.should_proxy_domain("creatives.sascdn.com"));
        assert!(!manager.should_proxy_domain("example.com"));
        assert!(!manager.should_proxy_domain("unknown.domain.com"));
    }

    #[test]
    fn test_get_proxied_domains() {
        let settings = create_test_settings();
        let manager = PartnerManager::from_settings(&settings);

        let domains = manager.get_proxied_domains();
        assert_eq!(domains.len(), 3);
        assert!(domains.iter().any(|d| *d == "tpc.googlesyndication.com"));
        assert!(domains
            .iter()
            .any(|d| *d == "securepubads.g.doubleclick.net"));
        assert!(domains.iter().any(|d| *d == "creatives.sascdn.com"));
    }

    #[test]
    fn test_disabled_partner_config() {
        let mut settings = Settings::default();

        // Create disabled GAM config
        let gam_config = PartnerConfig {
            enabled: false,
            name: "Google Ad Manager".to_string(),
            domains_to_proxy: vec!["securepubads.g.doubleclick.net".to_string()],
            proxy_domain: "creatives.auburndao.com".to_string(),
            backend_name: "gam_proxy_backend".to_string(),
        };

        settings.partners = Some(Partners {
            gam: Some(gam_config),
            equativ: None,
            prebid: None,
        });

        let manager = PartnerManager::from_settings(&settings);

        // Disabled partner should not have any domain mappings
        assert!(!manager.should_proxy_domain("securepubads.g.doubleclick.net"));
        assert_eq!(
            manager.get_backend_for_domain("securepubads.g.doubleclick.net"),
            None
        );

        let url = "https://securepubads.g.doubleclick.net/tag/js/gpt.js";
        assert_eq!(manager.rewrite_url(url), url);
    }

    #[test]
    fn test_empty_partner_config() {
        let settings = Settings::default();
        let manager = PartnerManager::from_settings(&settings);

        // No partners configured
        assert_eq!(manager.get_proxied_domains().len(), 0);
        assert!(!manager.should_proxy_domain("any.domain.com"));

        let url = "https://example.com/test";
        assert_eq!(manager.rewrite_url(url), url);
    }

    #[test]
    fn test_multiple_replacements_in_single_url() {
        let settings = create_test_settings();
        let manager = PartnerManager::from_settings(&settings);

        // URL containing multiple domains (edge case - should only replace first match)
        let url = "https://tpc.googlesyndication.com/path?redirect=https://securepubads.g.doubleclick.net/other";
        let rewritten = manager.rewrite_url(url);

        // Only the first domain should be replaced due to the break statement
        assert!(rewritten.contains("creatives.auburndao.com"));
        assert!(rewritten.contains("/path?redirect="));
    }

    #[test]
    fn test_content_rewriting_with_protocol_variations() {
        let settings = create_test_settings();
        let manager = PartnerManager::from_settings(&settings);

        let content = r#"
            http://tpc.googlesyndication.com/image.jpg
            https://tpc.googlesyndication.com/image2.jpg
            //tpc.googlesyndication.com/image3.jpg
            src="tpc.googlesyndication.com/image4.jpg"
        "#;

        let rewritten = manager.rewrite_content(content);

        assert!(rewritten.contains("http://creatives.auburndao.com/image.jpg"));
        assert!(rewritten.contains("https://creatives.auburndao.com/image2.jpg"));
        assert!(rewritten.contains("//creatives.auburndao.com/image3.jpg"));
        assert!(rewritten.contains("src=\"creatives.auburndao.com/image4.jpg\""));
    }

    #[test]
    fn test_case_sensitive_domain_matching() {
        let settings = create_test_settings();
        let manager = PartnerManager::from_settings(&settings);

        // Domain matching should be case-sensitive
        let url = "https://TPC.GOOGLESYNDICATION.COM/test";
        let rewritten = manager.rewrite_url(url);
        assert_eq!(rewritten, url); // Should not be rewritten due to case mismatch

        let url_lower = "https://tpc.googlesyndication.com/test";
        let rewritten_lower = manager.rewrite_url(url_lower);
        assert!(rewritten_lower.contains("creatives.auburndao.com"));
    }

    #[test]
    fn test_partial_domain_matching() {
        let settings = create_test_settings();
        let manager = PartnerManager::from_settings(&settings);

        // The current implementation does substring replacement, which can match partial domains
        let content = r#"
            https://notsecurepubads.g.doubleclick.net/test
            https://securepubads.g.doubleclick.net.evil.com/test
            https://fake-tpc.googlesyndication.com/test
        "#;

        let rewritten = manager.rewrite_content(content);

        // Due to substring replacement, partial matches will occur
        // "securepubads.g.doubleclick.net" within "notsecurepubads.g.doubleclick.net" gets replaced
        assert!(rewritten.contains("notcreatives.auburndao.com/test"));
        // "securepubads.g.doubleclick.net" within the URL gets replaced, leaving ".evil.com"
        assert!(rewritten.contains("creatives.auburndao.com.evil.com/test"));
        // "tpc.googlesyndication.com" within "fake-tpc.googlesyndication.com" gets replaced
        assert!(rewritten.contains("fake-creatives.auburndao.com/test"));
    }

    #[test]
    fn test_overlapping_domain_configurations() {
        let mut settings = Settings::default();

        // Create configs with overlapping proxy domains
        let gam_config = PartnerConfig {
            enabled: true,
            name: "GAM".to_string(),
            domains_to_proxy: vec!["gam.example.com".to_string()],
            proxy_domain: "proxy.domain.com".to_string(),
            backend_name: "gam_backend".to_string(),
        };

        let equativ_config = PartnerConfig {
            enabled: true,
            name: "Equativ".to_string(),
            domains_to_proxy: vec!["equativ.example.com".to_string()],
            proxy_domain: "proxy.domain.com".to_string(), // Same proxy domain
            backend_name: "equativ_backend".to_string(),
        };

        settings.partners = Some(Partners {
            gam: Some(gam_config),
            equativ: Some(equativ_config),
            prebid: None,
        });

        let manager = PartnerManager::from_settings(&settings);

        // Both domains should map to the same proxy domain but different backends
        assert_eq!(
            manager.rewrite_url("https://gam.example.com/path"),
            "https://proxy.domain.com/path"
        );
        assert_eq!(
            manager.rewrite_url("https://equativ.example.com/path"),
            "https://proxy.domain.com/path"
        );
        assert_eq!(
            manager.get_backend_for_domain("gam.example.com"),
            Some("gam_backend")
        );
        assert_eq!(
            manager.get_backend_for_domain("equativ.example.com"),
            Some("equativ_backend")
        );
    }
}
