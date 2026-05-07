use error_stack::Report;
use regex::Regex;
use std::borrow::Cow;
use std::collections::HashMap;

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
    path_patterns: Vec<CompiledPathPattern>,
}

#[derive(Clone)]
struct CompiledPathPattern {
    host: Option<String>,
    path_prefix: Option<String>,
    path_regex: Option<Regex>,
}

impl core::fmt::Debug for CompiledPathPattern {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("CompiledPathPattern")
            .field("host", &self.host)
            .field("path_prefix", &self.path_prefix)
            .field("path_regex", &self.path_regex.as_ref().map(Regex::as_str))
            .finish()
    }
}

impl CompiledPathPattern {
    fn new(pattern: &PathPattern) -> Result<Self, Report<TrustedServerError>> {
        let path_regex = pattern
            .path_regex
            .as_deref()
            .map(|s| {
                Regex::new(s).map_err(|e| {
                    Report::new(TrustedServerError::Configuration {
                        message: format!("Invalid path_regex pattern `{s}`: {e}"),
                    })
                })
            })
            .transpose()?;

        Ok(Self {
            host: pattern.host.clone(),
            path_prefix: pattern.path_prefix.clone(),
            path_regex,
        })
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

        if let Some(ref regex) = self.path_regex {
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
                let normalized = normalize_domain(domain).into_owned();
                if let Some(existing_idx) = domain_index.insert(normalized.clone(), idx) {
                    log::warn!(
                        "Backend domain '{}' appears in multiple backends (index {} and {}); using backend {}",
                        normalized, existing_idx, idx, idx
                    );
                }
            }

            let path_patterns = backend
                .path_patterns
                .iter()
                .map(CompiledPathPattern::new)
                .collect::<Result<Vec<_>, _>>()?;

            routes.push(BackendRoute {
                origin_url: backend.origin_url.clone(),
                certificate_check: backend.certificate_check,
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
        if let Some(&idx) = self.domain_index.get(normalized_host.as_ref()) {
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
/// Returns a [`Cow::Borrowed`] slice when no transformation is needed (already
/// lowercase, no "www." prefix), avoiding any allocation on the hot request path.
///
/// # Examples
///
/// ```
/// use trusted_server_core::backend_router::normalize_domain;
///
/// assert_eq!(normalize_domain("WWW.EXAMPLE.COM"), "example.com");
/// assert_eq!(normalize_domain("www.example.com"), "example.com");
/// assert_eq!(normalize_domain("example.com"), "example.com");
/// assert_eq!(normalize_domain("sub.example.com"), "sub.example.com");
/// ```
#[must_use]
pub fn normalize_domain(domain: &str) -> Cow<'_, str> {
    if domain.bytes().any(|b| b.is_ascii_uppercase()) {
        let mut lower = domain.to_lowercase();
        while lower.starts_with("www.") {
            lower.drain(..4);
        }
        Cow::Owned(lower)
    } else {
        Cow::Borrowed(domain.trim_start_matches("www."))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_backend(id: &str, domains: Vec<&str>, origin_url: &str) -> BackendRoutingConfig {
        BackendRoutingConfig {
            id: Some(id.to_string()),
            origin_url: origin_url.to_string(),
            domains: domains.into_iter().map(String::from).collect(),
            path_patterns: vec![],
            certificate_check: true,
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
        }
    }

    #[test]
    fn test_exact_domain_match() {
        let backends = vec![create_test_backend(
            "backend-a",
            vec!["site-a.example.com", "site-b.example.com"],
            "https://backend-a.example.com",
        )];

        let router = BackendRouter::new(&backends, "https://default-origin.com".to_string(), true)
            .expect("should build router from valid backends config");

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

        let router = BackendRouter::new(&backends, "https://default-origin.com".to_string(), true)
            .expect("should build router from valid backends config");

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

        let router = BackendRouter::new(&backends, "https://default-origin.com".to_string(), true)
            .expect("should build router from valid backends config");

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

        let router = BackendRouter::new(&backends, "https://default-origin.com".to_string(), true)
            .expect("should build router from valid backends config");

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

        let router = BackendRouter::new(&backends, "https://default-origin.com".to_string(), true)
            .expect("should build router from valid backends config");

        let (origin, _) =
            router.select_origin("site-a.example.com", "/image/upload/v1234/photo.jpg");
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

        let router = BackendRouter::new(&backends, "https://default-origin.com".to_string(), true)
            .expect("should build router from valid backends config");

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

        let router = BackendRouter::new(&backends, "https://default-origin.com".to_string(), true)
            .expect("should build router from valid backends config");

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

        let router = BackendRouter::new(&backends, "https://default-origin.com".to_string(), true)
            .expect("should build router from valid backends config");

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
        }];

        let router = BackendRouter::new(&backends, "https://default-origin.com".to_string(), true)
            .expect("should build router from valid backends config");

        let (origin, cert_check) = router.select_origin("custom.com", "/");
        assert_eq!(origin, "https://custom-origin.com");
        assert!(
            !cert_check,
            "should respect backend-specific certificate_check"
        );
    }

    #[test]
    fn rejects_invalid_path_regex() {
        let backends = vec![BackendRoutingConfig {
            id: None,
            origin_url: "https://example.com".to_string(),
            domains: vec![],
            path_patterns: vec![PathPattern {
                host: None,
                path_prefix: None,
                path_regex: Some("[invalid".to_string()),
            }],
            certificate_check: true,
        }];

        let _err = BackendRouter::new(&backends, "https://default.com".to_string(), true)
            .expect_err("should reject invalid path_regex pattern");
    }

    #[test]
    fn duplicate_domain_uses_last_backend() {
        let backends = vec![
            BackendRoutingConfig {
                id: None,
                origin_url: "https://first.com".to_string(),
                domains: vec!["example.com".to_string()],
                path_patterns: vec![],
                certificate_check: true,
            },
            BackendRoutingConfig {
                id: None,
                origin_url: "https://second.com".to_string(),
                domains: vec!["example.com".to_string()],
                path_patterns: vec![],
                certificate_check: true,
            },
        ];

        let router = BackendRouter::new(&backends, "https://default.com".to_string(), true)
            .expect("should succeed even with duplicate domains");

        let (url, _) = router.select_origin("example.com", "/");
        assert_eq!(
            url, "https://second.com",
            "should route to last backend when domain appears twice"
        );
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

        let router = BackendRouter::new(&backends, "https://default-origin.com".to_string(), true)
            .expect("should build router from valid backends config");

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
