use std::collections::HashMap;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{PartnerConfig, Partners, Settings};

    fn create_test_settings() -> Settings {
        let mut settings = Settings::default();

        let gam_config = PartnerConfig {
            enabled: true,
            name: "Google Ad Manager".to_string(),
            domains_to_proxy: vec![
                "securepubads.g.doubleclick.net".to_string(),
                "tpc.googlesyndication.com".to_string(),
            ],
            proxy_domain: "creatives.auburndao.com".to_string(),
            backend_name: "gam_proxy_backend".to_string(),
        };

        let equativ_config = PartnerConfig {
            enabled: true,
            name: "Equativ".to_string(),
            domains_to_proxy: vec!["creatives.sascdn.com".to_string()],
            proxy_domain: "creatives.auburndao.com".to_string(),
            backend_name: "equativ_proxy_backend".to_string(),
        };

        settings.partners = Some(Partners {
            gam: Some(gam_config),
            equativ: Some(equativ_config),
            prebid: None,
        });

        settings
    }

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
}
