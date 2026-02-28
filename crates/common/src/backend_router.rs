use error_stack::Report;
use regex::Regex;
use std::collections::HashMap;
use std::sync::OnceLock;

use crate::error::TrustedServerError;
use crate::settings::{BackendRoutingConfig, PathPattern};

/// Backend routing system that selects the appropriate origin URL based on request host and path.
///
/// Leverages Trusted Server's dynamic backend creation - we just need to select the right
/// origin URL, and the backend will be created automatically via [`crate::backend::BackendConfig::from_url()`].
///
/// Supports:
/// - Domain-based routing (exact match + www normalization)
/// - Path-based routing (optional prefix/regex patterns)
/// - Fallback to default origin
#[derive(Debug, Clone)]
pub struct BackendRouter {
    routes: Vec<BackendRoute>,
    domain_index: HashMap<String, usize>,
    default_origin: String,
    default_certificate_check: bool,
}

#[derive(Debug, Clone)]
pub struct BackendRoute {
    pub origin_url: String,
    pub certificate_check: bool,
    #[allow(dead_code)]
    domains: Vec<String>,
    path_patterns: Vec<CompiledPathPattern>,
}

#[derive(Clone)]
struct CompiledPathPattern {
    host: Option<String>,
    path_prefix: Option<String>,
    path_regex: Option<OnceLock<Regex>>,
    path_regex_pattern: Option<String>,
}

impl core::fmt::Debug for CompiledPathPattern {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("CompiledPathPattern")
            .field("host", &self.host)
            .field("path_prefix", &self.path_prefix)
            .field("path_regex_pattern", &self.path_regex_pattern)
            .finish()
    }
}

impl CompiledPathPattern {
    fn new(pattern: &PathPattern) -> Self {
        Self {
            host: pattern.host.clone(),
            path_prefix: pattern.path_prefix.clone(),
            path_regex: pattern.path_regex.as_ref().map(|_| OnceLock::new()),
            path_regex_pattern: pattern.path_regex.clone(),
        }
    }

    fn compiled_regex(&self) -> Option<&Regex> {
        if let (Some(regex_cell), Some(pattern)) = (&self.path_regex, &self.path_regex_pattern) {
            Some(regex_cell.get_or_init(|| {
                Regex::new(pattern).expect("path_regex pattern should be valid")
            }))
        } else {
            None
        }
    }

    fn matches(&self, host: &str, path: &str) -> bool {
        let host_matches = match &self.host {
            None => true,
            Some(pattern) if pattern == "*" => true,
            Some(pattern) => {
                let normalized_host = normalize_domain(host);
                let normalized_pattern = normalize_domain(pattern);
                normalized_host == normalized_pattern
            }
        };

        if !host_matches {
            return false;
        }

        if let Some(prefix) = &self.path_prefix {
            return path.starts_with(prefix);
        }

        if let Some(regex) = self.compiled_regex() {
            return regex.is_match(path);
        }

        true
    }
}

impl BackendRouter {
    /// Creates a new [`BackendRouter`] from backend configurations.
    ///
    /// Backends are stored as origin URL + `certificate_check` pairs.
    /// The actual Fastly backend will be created dynamically at request time.
    ///
    /// # Errors
    ///
    /// Returns an error if a path regex pattern fails to compile.
    pub fn new(
        backends: &[BackendRoutingConfig],
        default_origin: String,
        default_certificate_check: bool,
    ) -> Result<Self, Report<TrustedServerError>> {
        let mut domain_index = HashMap::new();
        let mut routes = Vec::with_capacity(backends.len());

        for (idx, backend) in backends.iter().enumerate() {
            for domain in &backend.domains {
                let normalized = normalize_domain(domain);
                domain_index.insert(normalized, idx);
            }

            let path_patterns = backend
                .path_patterns
                .iter()
                .map(CompiledPathPattern::new)
                .collect();

            routes.push(BackendRoute {
                origin_url: backend.origin_url.clone(),
                certificate_check: backend.certificate_check,
                domains: backend.domains.clone(),
                path_patterns,
            });
        }

        Ok(Self {
            routes,
            domain_index,
            default_origin,
            default_certificate_check,
        })
    }

    /// Selects the appropriate origin URL and TLS settings based on request host and path.
    ///
    /// Selection priority:
    /// 1. Exact domain match
    /// 2. www. prefix normalization (www.example.com → example.com)
    /// 3. Path pattern matching (prefix or regex)
    /// 4. Fallback to default origin
    ///
    /// Returns `(origin_url, certificate_check)` tuple.
    #[must_use]
    pub fn select_origin(&self, host: &str, path: &str) -> (&str, bool) {
        let normalized_host = normalize_domain(host);

        // Try domain index first (fastest lookup)
        if let Some(&idx) = self.domain_index.get(&normalized_host) {
            let route = &self.routes[idx];
            return (&route.origin_url, route.certificate_check);
        }

        // Try path patterns
        for route in &self.routes {
            for pattern in &route.path_patterns {
                if pattern.matches(host, path) {
                    return (&route.origin_url, route.certificate_check);
                }
            }
        }

        // Fallback to default
        (&self.default_origin, self.default_certificate_check)
    }
}

/// Normalizes a domain by removing "www." prefix and converting to lowercase.
///
/// # Examples
///
/// ```
/// use trusted_server_common::backend_router::normalize_domain;
///
/// assert_eq!(normalize_domain("WWW.EXAMPLE.COM"), "example.com");
/// assert_eq!(normalize_domain("www.example.com"), "example.com");
/// assert_eq!(normalize_domain("example.com"), "example.com");
/// assert_eq!(normalize_domain("sub.example.com"), "sub.example.com");
/// ```
#[must_use]
pub fn normalize_domain(domain: &str) -> String {
    domain
        .to_lowercase()
        .trim_start_matches("www.")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::BackendTimeouts as SettingsBackendTimeouts;

    fn create_test_backend(id: &str, domains: Vec<&str>, origin_url: &str) -> BackendRoutingConfig {
        BackendRoutingConfig {
            id: Some(id.to_string()),
            origin_url: origin_url.to_string(),
            domains: domains.into_iter().map(String::from).collect(),
            path_patterns: vec![],
            certificate_check: true,
            timeouts: SettingsBackendTimeouts::default(),
        }
    }

    fn create_test_backend_with_patterns(
        id: &str,
        origin_url: &str,
        patterns: Vec<PathPattern>,
    ) -> BackendRoutingConfig {
        BackendRoutingConfig {
            id: Some(id.to_string()),
            origin_url: origin_url.to_string(),
            domains: vec![],
            path_patterns: patterns,
            certificate_check: true,
            timeouts: SettingsBackendTimeouts::default(),
        }
    }

    #[test]
    fn test_exact_domain_match() {
        let backends = vec![create_test_backend(
            "backend-a",
            vec!["site-a.example.com", "site-b.example.com"],
            "https://backend-a.example.com",
        )];

        let router = BackendRouter::new(
            &backends,
            "https://default-origin.com".to_string(),
            true,
        )
        .unwrap();

        let (origin, cert_check) = router.select_origin("site-a.example.com", "/");
        assert_eq!(origin, "https://backend-a.example.com");
        assert!(cert_check);

        let (origin, _) = router.select_origin("site-b.example.com", "/article");
        assert_eq!(origin, "https://backend-a.example.com");
    }

    #[test]
    fn test_www_prefix_normalization() {
        let backends = vec![create_test_backend(
            "backend-a",
            vec!["site-a.example.com"],
            "https://backend-a.example.com",
        )];

        let router = BackendRouter::new(
            &backends,
            "https://default-origin.com".to_string(),
            true,
        )
        .unwrap();

        let (origin, _) = router.select_origin("www.site-a.example.com", "/");
        assert_eq!(origin, "https://backend-a.example.com");

        let (origin, _) = router.select_origin("WWW.SITE-A.EXAMPLE.COM", "/");
        assert_eq!(origin, "https://backend-a.example.com");
    }

    #[test]
    fn test_subdomain_no_match() {
        let backends = vec![create_test_backend(
            "backend-a",
            vec!["site-a.example.com"],
            "https://backend-a.example.com",
        )];

        let router = BackendRouter::new(
            &backends,
            "https://default-origin.com".to_string(),
            true,
        )
        .unwrap();

        let (origin, _) = router.select_origin("trending.site-a.example.com", "/");
        assert_eq!(
            origin, "https://default-origin.com",
            "trending.site-a.example.com should fall back to default"
        );
    }

    #[test]
    fn test_path_prefix_matching() {
        let backends = vec![create_test_backend_with_patterns(
            "backend-b",
            "https://backend-b.example.com",
            vec![
                PathPattern {
                    host: Some("site-c.example.com".to_string()),
                    path_prefix: Some("/.api/".to_string()),
                    path_regex: None,
                },
                PathPattern {
                    host: Some("site-c.example.com".to_string()),
                    path_prefix: Some("/my-account".to_string()),
                    path_regex: None,
                },
            ],
        )];

        let router = BackendRouter::new(
            &backends,
            "https://default-origin.com".to_string(),
            true,
        )
        .unwrap();

        let (origin, _) = router.select_origin("site-c.example.com", "/.api/users");
        assert_eq!(origin, "https://backend-b.example.com");

        let (origin, _) = router.select_origin("site-c.example.com", "/my-account/settings");
        assert_eq!(origin, "https://backend-b.example.com");

        let (origin, _) = router.select_origin("site-c.example.com", "/articles");
        assert_eq!(origin, "https://default-origin.com");
    }

    #[test]
    fn test_path_regex_matching() {
        let backends = vec![create_test_backend_with_patterns(
            "backend-c",
            "https://backend-c.example.com",
            vec![PathPattern {
                host: Some("*".to_string()),
                path_prefix: None,
                path_regex: Some("^/image/upload/".to_string()),
            }],
        )];

        let router = BackendRouter::new(
            &backends,
            "https://default-origin.com".to_string(),
            true,
        )
        .unwrap();

        let (origin, _) = router.select_origin("site-a.example.com", "/image/upload/v1234/photo.jpg");
        assert_eq!(origin, "https://backend-c.example.com");

        let (origin, _) = router.select_origin("site-a.example.com", "/images/photo.jpg");
        assert_eq!(origin, "https://default-origin.com");
    }

    #[test]
    fn test_wildcard_host_pattern() {
        let backends = vec![create_test_backend_with_patterns(
            "s3",
            "http://s3.amazonaws.com",
            vec![PathPattern {
                host: None,
                path_prefix: Some("/bucket/".to_string()),
                path_regex: None,
            }],
        )];

        let router = BackendRouter::new(
            &backends,
            "https://default-origin.com".to_string(),
            true,
        )
        .unwrap();

        let (origin, _) = router.select_origin("anydomain.com", "/bucket/file.txt");
        assert_eq!(origin, "http://s3.amazonaws.com");

        let (origin, _) = router.select_origin("another.com", "/bucket/");
        assert_eq!(origin, "http://s3.amazonaws.com");
    }

    #[test]
    fn test_fallback_to_default() {
        let backends = vec![create_test_backend(
            "backend-a",
            vec!["site-a.example.com"],
            "https://backend-a.example.com",
        )];

        let router = BackendRouter::new(
            &backends,
            "https://default-origin.com".to_string(),
            true,
        )
        .unwrap();

        let (origin, _) = router.select_origin("unknown.com", "/");
        assert_eq!(origin, "https://default-origin.com");
    }

    #[test]
    fn test_multiple_backends_priority() {
        let backends = vec![
            create_test_backend(
                "backend-a",
                vec!["site-a.example.com"],
                "https://backend-a.example.com",
            ),
            create_test_backend(
                "backend-b",
                vec!["site-c.example.com"],
                "https://backend-b.example.com",
            ),
        ];

        let router = BackendRouter::new(
            &backends,
            "https://default-origin.com".to_string(),
            true,
        )
        .unwrap();

        let (origin, _) = router.select_origin("site-a.example.com", "/");
        assert_eq!(origin, "https://backend-a.example.com");

        let (origin, _) = router.select_origin("site-c.example.com", "/");
        assert_eq!(origin, "https://backend-b.example.com");
    }

    #[test]
    fn test_normalize_domain() {
        assert_eq!(normalize_domain("example.com"), "example.com");
        assert_eq!(normalize_domain("www.example.com"), "example.com");
        assert_eq!(normalize_domain("WWW.EXAMPLE.COM"), "example.com");
        assert_eq!(normalize_domain("Www.Example.Com"), "example.com");
        assert_eq!(normalize_domain("sub.example.com"), "sub.example.com");
        assert_eq!(
            normalize_domain("www.sub.example.com"),
            "sub.example.com",
            "should only strip leading www"
        );
    }

    #[test]
    fn test_certificate_check_setting() {
        let backends = vec![BackendRoutingConfig {
            id: Some("custom".to_string()),
            origin_url: "https://custom-origin.com".to_string(),
            domains: vec!["custom.com".to_string()],
            path_patterns: vec![],
            certificate_check: false,
            timeouts: SettingsBackendTimeouts::default(),
        }];

        let router = BackendRouter::new(
            &backends,
            "https://default-origin.com".to_string(),
            true,
        )
        .unwrap();

        let (origin, cert_check) = router.select_origin("custom.com", "/");
        assert_eq!(origin, "https://custom-origin.com");
        assert!(!cert_check, "should respect backend-specific certificate_check");
    }

    #[test]
    fn test_domain_and_path_pattern_precedence() {
        let backends = vec![
            create_test_backend(
                "backend-a",
                vec!["site-c.example.com"],
                "https://backend-a.example.com",
            ),
            create_test_backend_with_patterns(
                "backend-b",
                "https://backend-b.example.com",
                vec![PathPattern {
                    host: Some("site-c.example.com".to_string()),
                    path_prefix: Some("/.api/".to_string()),
                    path_regex: None,
                }],
            ),
        ];

        let router = BackendRouter::new(
            &backends,
            "https://default-origin.com".to_string(),
            true,
        )
        .unwrap();

        let (origin, _) = router.select_origin("site-c.example.com", "/");
        assert_eq!(
            origin, "https://backend-a.example.com",
            "domain match should take precedence over path pattern"
        );

        let (origin, _) = router.select_origin("site-c.example.com", "/.api/users");
        assert_eq!(
            origin, "https://backend-a.example.com",
            "domain match should still take precedence for API paths"
        );
    }
}
