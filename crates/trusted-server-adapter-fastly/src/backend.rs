use std::time::Duration;

use error_stack::{Report, ResultExt as _};
use fastly::backend::Backend;
use url::Url;

use trusted_server_core::error::TrustedServerError;
use trusted_server_core::platform::{BackendNamingPolicy, PlatformBackendSpec, PredictedBackend};

#[cfg(test)]
const MAX_BACKEND_NAME_LEN: usize = 255;
#[cfg(test)]
const SPEC_DIGEST_HEX_LEN: usize = 32;

/// Returns the default port for the given scheme (443 for HTTPS, 80 for HTTP).
#[inline]
fn default_port_for_scheme(scheme: &str) -> u16 {
    if scheme.eq_ignore_ascii_case("https") {
        443
    } else {
        80
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct NormalizedBackendHost {
    identity: String,
    authority: String,
    is_ip_literal: bool,
}

/// Normalize URL-derived and direct hosts for transport and TLS use.
///
/// `url::Url::host_str()` preserves brackets around IPv6 literals. Fastly's
/// backend target and HTTP authority require those brackets, while certificate
/// identity matching requires the bare address and SNI must not be sent for IP
/// literals.
#[inline]
fn normalize_backend_host(host: &str) -> NormalizedBackendHost {
    let unbracketed = host
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or(host);
    match unbracketed.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V6(_)) => NormalizedBackendHost {
            identity: unbracketed.to_owned(),
            authority: format!("[{unbracketed}]"),
            is_ip_literal: true,
        },
        Ok(std::net::IpAddr::V4(_)) => NormalizedBackendHost {
            identity: unbracketed.to_owned(),
            authority: unbracketed.to_owned(),
            is_ip_literal: true,
        },
        Err(_) => NormalizedBackendHost {
            identity: host.to_owned(),
            authority: host.to_owned(),
            is_ip_literal: false,
        },
    }
}

/// Compute the Host header value for a backend request.
///
/// For standard ports (443 for HTTPS, 80 for HTTP), returns just the hostname.
/// For non-standard ports, returns "hostname:port" to ensure backends that
/// generate URLs based on the Host header include the port.
///
/// This fixes the issue where backends behind reverse proxies (like Caddy)
/// would generate URLs without the port when the Host header didn't include it.
#[inline]
fn compute_host_header(scheme: &str, host: &str, port: u16) -> String {
    let host = normalize_backend_host(host).authority;
    if port == default_port_for_scheme(scheme) {
        host
    } else {
        format!("{host}:{port}")
    }
}

/// Default first-byte timeout for backends (15 seconds).
pub(crate) const DEFAULT_FIRST_BYTE_TIMEOUT: Duration = Duration::from_secs(15);
/// Default timeout between response body bytes for backends (10 seconds).
pub(crate) const DEFAULT_BETWEEN_BYTES_TIMEOUT: Duration = Duration::from_secs(10);

/// Configuration for creating a dynamic Fastly backend.
///
/// Uses the builder pattern so that new options can be added without changing
/// existing call sites — fields carry sensible defaults.
pub struct BackendConfig<'a> {
    scheme: &'a str,
    host: &'a str,
    port: Option<u16>,
    certificate_check: bool,
    first_byte_timeout: Duration,
    between_bytes_timeout: Duration,
    host_header_override: Option<&'a str>,
    discriminator: Option<&'a str>,
}

impl<'a> BackendConfig<'a> {
    /// Create a new configuration with required fields and safe defaults.
    ///
    /// `certificate_check` defaults to `true`.
    /// `first_byte_timeout` defaults to 15 seconds.
    #[must_use]
    pub fn new(scheme: &'a str, host: &'a str) -> Self {
        Self {
            scheme,
            host,
            port: None,
            certificate_check: true,
            first_byte_timeout: DEFAULT_FIRST_BYTE_TIMEOUT,
            between_bytes_timeout: DEFAULT_BETWEEN_BYTES_TIMEOUT,
            host_header_override: None,
            discriminator: None,
        }
    }

    /// Set the port for the backend. When `None`, the default port for the
    /// scheme is used (443 for HTTPS, 80 for HTTP).
    #[must_use]
    pub fn port(mut self, port: Option<u16>) -> Self {
        self.port = port;
        self
    }

    /// Control TLS certificate verification. Defaults to `true`.
    #[must_use]
    pub fn certificate_check(mut self, check: bool) -> Self {
        self.certificate_check = check;
        self
    }

    /// Set the maximum time to wait for the first byte of the response.
    ///
    /// Defaults to 15 seconds. For latency-sensitive paths like auction
    /// requests, callers should set a tighter timeout derived from the
    /// auction deadline.
    #[must_use]
    pub fn first_byte_timeout(mut self, timeout: Duration) -> Self {
        self.first_byte_timeout = timeout;
        self
    }

    /// Set the maximum time to wait between response body bytes.
    ///
    /// Defaults to 10 seconds. Auction backends should set this to the same
    /// remaining budget as the first-byte timeout so slow-drip bodies cannot
    /// hold the auction past its deadline.
    #[must_use]
    pub fn between_bytes_timeout(mut self, timeout: Duration) -> Self {
        self.between_bytes_timeout = timeout;
        self
    }

    /// Set the outbound Host header sent to the backend origin.
    #[must_use]
    pub fn host_header_override(mut self, host: Option<&'a str>) -> Self {
        self.host_header_override = host;
        self
    }

    /// Set an optional stable discriminator folded into the backend name.
    ///
    /// Two callers targeting the same origin with the same transport timeout
    /// otherwise share a backend name. Auction response correlation keys on the
    /// backend name, so a shared name would let one provider's response be
    /// parsed as another's. A per-provider discriminator keeps the names
    /// distinct while staying stable across requests.
    #[must_use]
    pub fn discriminator(mut self, discriminator: Option<&'a str>) -> Self {
        self.discriminator = discriminator;
        self
    }

    fn platform_spec(&self) -> PlatformBackendSpec {
        PlatformBackendSpec {
            scheme: self.scheme.to_owned(),
            host: normalize_backend_host(self.host).identity,
            port: self.port,
            host_header_override: self.host_header_override.map(str::to_owned),
            certificate_check: self.certificate_check,
            first_byte_timeout: self.first_byte_timeout,
            between_bytes_timeout: self.between_bytes_timeout,
            discriminator: self.discriminator.map(str::to_owned),
        }
    }

    /// Compute the deterministic backend name and resolved port without
    /// registering anything.
    fn predict_backend(&self) -> Result<PredictedBackend, Report<TrustedServerError>> {
        BackendNamingPolicy::Fastly
            .predict(&self.platform_spec())
            .change_context(TrustedServerError::Proxy {
                message: "backend name prediction failed".to_owned(),
            })
    }

    /// Return the deterministic backend name without registering anything.
    ///
    /// Convenience wrapper over `Self::predict_backend` that discards the
    /// resolved port, used by [`crate::platform::PlatformBackend`]
    /// implementations that only need the name for correlation.
    ///
    /// # Errors
    ///
    /// Returns an error if the host is empty.
    #[allow(dead_code, reason = "retained for backend-name parity tests")]
    pub fn predict_name(self) -> Result<String, Report<TrustedServerError>> {
        self.predict_backend().map(|prediction| prediction.name)
    }

    /// Ensure a dynamic backend exists for this configuration and return its name.
    ///
    /// The name is a collision-resistant function of the complete backend spec
    /// (see `Self::predict_backend`), so different specs — for example, different
    /// timeout values — always produce different backend registrations and a
    /// tight deadline cannot be silently widened by an earlier registration.
    ///
    /// # Errors
    ///
    /// Returns an error if the host is empty or if backend creation fails
    /// (except for `NameInUse` which reuses the existing backend).
    pub fn ensure(self) -> Result<String, Report<TrustedServerError>> {
        let prediction = self.predict_backend()?;
        let backend_name = prediction.name;
        let target_port = prediction.port;
        let host = normalize_backend_host(self.host);

        let host_with_port = format!("{}:{target_port}", host.authority);

        let host_header = self.host_header_override.map_or_else(
            || compute_host_header(self.scheme, &host.identity, target_port),
            str::to_owned,
        );

        // Target base is host[:port]; SSL is enabled only for https scheme
        let mut builder = Backend::builder(&backend_name, &host_with_port)
            .override_host(&host_header)
            .connect_timeout(Duration::from_secs(1))
            .first_byte_timeout(self.first_byte_timeout)
            .between_bytes_timeout(self.between_bytes_timeout);
        if self.scheme.eq_ignore_ascii_case("https") {
            builder = builder.enable_ssl();
            if !host.is_ip_literal {
                builder = builder.sni_hostname(&host.identity);
            }
            if self.certificate_check {
                builder = builder.check_certificate(&host.identity);
            } else {
                log::warn!("INSECURE: certificate check disabled for backend: {backend_name}");
            }
            log::info!("enable ssl for backend: {backend_name}");
        }

        match builder.finish() {
            Ok(_) => {
                log::info!("created dynamic backend: {backend_name} -> {host_with_port}");
                Ok(backend_name)
            }
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("NameInUse") || msg.contains("already in use") {
                    log::info!("reusing existing dynamic backend: {backend_name}");
                    Ok(backend_name)
                } else {
                    Err(Report::new(TrustedServerError::Proxy {
                        message: format!(
                            "dynamic backend creation failed ({backend_name} -> {host_with_port}): {msg}"
                        ),
                    }))
                }
            }
        }
    }

    /// Parse an origin URL into its (scheme, host, port) components.
    ///
    /// Centralises URL parsing so that [`from_url`](Self::from_url) and
    /// [`from_url_with_first_byte_timeout`](Self::from_url_with_first_byte_timeout)
    /// share one code-path.
    fn parse_origin(
        origin_url: &str,
    ) -> Result<(String, String, Option<u16>), Report<TrustedServerError>> {
        let parsed_url = Url::parse(origin_url).change_context(TrustedServerError::Proxy {
            message: format!("Invalid origin_url: {origin_url}"),
        })?;

        let scheme = parsed_url.scheme().to_owned();
        let host = parsed_url
            .host_str()
            .ok_or_else(|| {
                Report::new(TrustedServerError::Proxy {
                    message: "Missing host in origin_url".to_owned(),
                })
            })?
            .to_owned();
        let port = parsed_url.port();

        Ok((scheme, host, port))
    }

    /// Parse an origin URL and ensure a dynamic backend exists for it.
    ///
    /// This is a convenience constructor that parses the URL, extracts scheme,
    /// host, and port, then calls [`ensure`](Self::ensure) with the default
    /// 15 s first-byte timeout.
    ///
    /// # Errors
    ///
    /// Returns an error if the URL cannot be parsed or lacks a host, or if
    /// backend creation fails.
    pub fn from_url(
        origin_url: &str,
        certificate_check: bool,
    ) -> Result<String, Report<TrustedServerError>> {
        Self::from_url_with_first_byte_timeout(
            origin_url,
            certificate_check,
            DEFAULT_FIRST_BYTE_TIMEOUT,
        )
    }

    /// Parse an origin URL and ensure a dynamic backend with a custom
    /// first-byte timeout.
    ///
    /// For latency-sensitive paths (e.g. auction bid requests) callers should
    /// pass the remaining auction budget so that individual requests don't hang
    /// longer than the overall deadline allows.
    ///
    /// # Errors
    ///
    /// Returns an error if the URL cannot be parsed or lacks a host, or if
    /// backend creation fails.
    pub fn from_url_with_first_byte_timeout(
        origin_url: &str,
        certificate_check: bool,
        first_byte_timeout: Duration,
    ) -> Result<String, Report<TrustedServerError>> {
        Self::from_url_with_first_byte_timeout_and_host_header_override(
            origin_url,
            certificate_check,
            first_byte_timeout,
            None,
        )
    }

    fn from_url_with_first_byte_timeout_and_host_header_override(
        origin_url: &str,
        certificate_check: bool,
        first_byte_timeout: Duration,
        host_header_override: Option<&str>,
    ) -> Result<String, Report<TrustedServerError>> {
        let (scheme, host, port) = Self::parse_origin(origin_url)?;

        BackendConfig::new(&scheme, &host)
            .port(port)
            .certificate_check(certificate_check)
            .first_byte_timeout(first_byte_timeout)
            .host_header_override(host_header_override)
            .ensure()
    }
}

#[cfg(test)]
mod tests {
    use trusted_server_core::platform::BackendNamingError;

    use super::{
        BackendConfig, MAX_BACKEND_NAME_LEN, SPEC_DIGEST_HEX_LEN, compute_host_header,
        normalize_backend_host,
    };

    /// Assert a computed name is `backend_<body>_<hex digest>` and stays within
    /// Fastly's length limit. The digest is what makes the name injective, so
    /// checking its presence and width guards the collision-safety property.
    fn assert_backend_name_shape(name: &str, expected_body: &str) {
        let prefix = format!("backend_{expected_body}_");
        assert!(
            name.starts_with(&prefix),
            "name should start with the readable body `{prefix}`, got {name}"
        );
        let digest = &name[prefix.len()..];
        assert_eq!(
            digest.len(),
            SPEC_DIGEST_HEX_LEN,
            "digest suffix should be {SPEC_DIGEST_HEX_LEN} hex chars, got {digest}"
        );
        assert!(
            digest.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "digest suffix should be hex, got {digest}"
        );
        assert!(
            name.len() <= MAX_BACKEND_NAME_LEN,
            "name should stay within the {MAX_BACKEND_NAME_LEN}-char limit, got {}",
            name.len()
        );
    }

    // Tests for compute_host_header - the fix for port preservation in Host header
    #[test]
    fn ipv6_hosts_are_bracketed_only_for_authority_values() {
        let bare = normalize_backend_host("2001:db8::1");
        let bracketed = normalize_backend_host("[2001:db8::1]");
        assert_eq!(bare, bracketed);
        assert_eq!(bare.identity, "2001:db8::1");
        assert_eq!(bare.authority, "[2001:db8::1]");
        assert!(bare.is_ip_literal, "IPv6 must not be sent as TLS SNI");
        assert_eq!(
            normalize_backend_host("cdn.example.com"),
            super::NormalizedBackendHost {
                identity: "cdn.example.com".to_string(),
                authority: "cdn.example.com".to_string(),
                is_ip_literal: false,
            }
        );
        assert_eq!(
            compute_host_header("https", "[2001:db8::1]", 443),
            "[2001:db8::1]"
        );
        assert_eq!(
            compute_host_header("https", "[2001:db8::1]", 8443),
            "[2001:db8::1]:8443"
        );
    }

    #[test]
    fn url_derived_ipv6_host_uses_bare_tls_identity_without_sni() {
        let (scheme, url_host, port) =
            BackendConfig::parse_origin("https://[2001:db8::7]:8443/openrtb")
                .expect("should parse IPv6 provider URL");
        assert_eq!(scheme, "https");
        assert_eq!(url_host, "[2001:db8::7]");
        assert_eq!(port, Some(8443));

        let normalized = normalize_backend_host(&url_host);
        assert_eq!(normalized.identity, "2001:db8::7");
        assert_eq!(normalized.authority, "[2001:db8::7]");
        assert!(
            normalized.is_ip_literal,
            "IP literals must omit TLS SNI while retaining a bare certificate identity"
        );

        let from_url_name = BackendConfig::new(&scheme, &url_host)
            .port(port)
            .predict_name()
            .expect("should predict URL-derived IPv6 backend name");
        let from_bare_name = BackendConfig::new(&scheme, "2001:db8::7")
            .port(port)
            .predict_name()
            .expect("should predict bare IPv6 backend name");
        assert_eq!(
            from_url_name, from_bare_name,
            "URL and direct IPv6 paths must preserve backend naming parity"
        );
    }

    #[test]
    fn host_header_includes_port_for_non_standard_https() {
        assert_eq!(
            compute_host_header("https", "cdn.example.com", 9443),
            "cdn.example.com:9443",
            "should include non-standard HTTPS port 9443 in Host header"
        );
        assert_eq!(
            compute_host_header("https", "cdn.example.com", 8443),
            "cdn.example.com:8443",
            "should include non-standard HTTPS port 8443 in Host header"
        );
    }

    #[test]
    fn host_header_excludes_port_for_standard_https() {
        assert_eq!(
            compute_host_header("https", "cdn.example.com", 443),
            "cdn.example.com",
            "should omit standard HTTPS port 443 from Host header"
        );
    }

    #[test]
    fn host_header_includes_port_for_non_standard_http() {
        assert_eq!(
            compute_host_header("http", "cdn.example.com", 8080),
            "cdn.example.com:8080",
            "should include non-standard HTTP port 8080 in Host header"
        );
    }

    #[test]
    fn host_header_excludes_port_for_standard_http() {
        assert_eq!(
            compute_host_header("http", "cdn.example.com", 80),
            "cdn.example.com",
            "should omit standard HTTP port 80 from Host header"
        );
    }

    #[test]
    fn returns_name_for_https_with_cert_check() {
        let name = BackendConfig::new("https", "origin.example.com")
            .ensure()
            .expect("should create backend for valid HTTPS origin");
        assert_backend_name_shape(&name, "https_origin_example_com_443_fb15000_bb10000");
    }

    #[test]
    fn returns_name_for_https_without_cert_check() {
        let name = BackendConfig::new("https", "origin.example.com")
            .certificate_check(false)
            .ensure()
            .expect("should create backend with cert check disabled");
        assert_backend_name_shape(&name, "https_origin_example_com_443_nocert_fb15000_bb10000");
    }

    #[test]
    fn returns_name_for_http_with_port_and_sanitizes() {
        let name = BackendConfig::new("http", "api.test-site.org")
            .port(Some(8080))
            .ensure()
            .expect("should create backend for HTTP origin with explicit port");
        assert_backend_name_shape(&name, "http_api_test-site_org_8080_fb15000_bb10000");
    }

    #[test]
    fn returns_name_for_http_without_port_defaults_to_80() {
        let name = BackendConfig::new("http", "example.org")
            .ensure()
            .expect("should create backend defaulting to port 80 for HTTP");
        assert_backend_name_shape(&name, "http_example_org_80_fb15000_bb10000");
    }

    #[test]
    fn error_on_host_with_control_characters() {
        let err = BackendConfig::new("https", "evil.com\nINFO fake log entry")
            .predict_name()
            .expect_err("should reject host containing newline");
        assert!(
            err.contains::<BackendNamingError>(),
            "should preserve the backend naming error report context"
        );
    }

    #[test]
    fn error_on_missing_host() {
        let err = BackendConfig::new("https", "")
            .ensure()
            .expect_err("should reject empty host");
        assert!(
            err.contains::<BackendNamingError>(),
            "should preserve the original backend naming error report context"
        );
    }

    #[test]
    fn second_call_reuses_existing_backend() {
        let first = BackendConfig::new("https", "reuse.example.com")
            .ensure()
            .expect("should create backend on first call");
        let second = BackendConfig::new("https", "reuse.example.com")
            .ensure()
            .expect("should reuse backend on second call");
        assert_eq!(
            first, second,
            "should return same backend name on repeat call"
        );
    }

    #[test]
    fn host_header_overrides_produce_different_names() {
        let name_a = BackendConfig::new("https", "origin.example.com")
            .host_header_override(Some("www.example.com"))
            .predict_name()
            .expect("should compute name with host header override");
        let name_b = BackendConfig::new("https", "origin.example.com")
            .host_header_override(Some("m.example.com"))
            .predict_name()
            .expect("should compute name with different host header override");

        assert_ne!(
            name_a, name_b,
            "backends with different host header overrides should have different names"
        );
        assert_backend_name_shape(
            &name_a,
            "https_origin_example_com_443_oh_www_example_com_fb15000_bb10000",
        );
        assert_backend_name_shape(
            &name_b,
            "https_origin_example_com_443_oh_m_example_com_fb15000_bb10000",
        );
    }

    #[test]
    fn host_header_override_rejects_control_characters() {
        let err = BackendConfig::new("https", "origin.example.com")
            .host_header_override(Some("www\n.example.com"))
            .predict_name()
            .expect_err("should reject host header override containing newline");

        assert!(
            err.contains::<BackendNamingError>(),
            "should preserve the backend naming error report context"
        );
    }

    #[test]
    fn host_header_override_rejects_invalid_values() {
        for host_header_override in [
            "https://www.example.com",
            "www.example.com/path",
            "www.example.com:",
            "example..com",
            "-",
        ] {
            let err = BackendConfig::new("https", "origin.example.com")
                .host_header_override(Some(host_header_override))
                .predict_name()
                .expect_err("should reject invalid host header override");

            assert!(
                err.contains::<BackendNamingError>(),
                "should preserve the backend naming error report context for {host_header_override:?}"
            );
        }
    }

    #[test]
    fn different_timeouts_produce_different_names() {
        use std::time::Duration;

        let name_a = BackendConfig::new("https", "origin.example.com")
            .first_byte_timeout(Duration::from_secs(2))
            .predict_name()
            .expect("should compute name with 2000ms timeout");
        let name_b = BackendConfig::new("https", "origin.example.com")
            .first_byte_timeout(Duration::from_millis(500))
            .predict_name()
            .expect("should compute name with 500ms timeout");
        assert_ne!(
            name_a, name_b,
            "backends with different timeouts should have different names"
        );
        assert!(
            name_a.contains("_fb2000_bb10000_"),
            "name should include first-byte and between-bytes timeout in the readable body"
        );
        assert!(
            name_b.contains("_fb500_bb10000_"),
            "name should include first-byte and between-bytes timeout in the readable body"
        );
    }

    #[test]
    fn different_between_bytes_timeouts_produce_different_names() {
        use std::time::Duration;

        let name_a = BackendConfig::new("https", "origin.example.com")
            .between_bytes_timeout(Duration::from_secs(2))
            .predict_name()
            .expect("should compute name with 2000ms between-bytes timeout");
        let name_b = BackendConfig::new("https", "origin.example.com")
            .between_bytes_timeout(Duration::from_millis(500))
            .predict_name()
            .expect("should compute name with 500ms between-bytes timeout");

        assert_ne!(
            name_a, name_b,
            "backends with different between-bytes timeouts should have different names"
        );
        assert!(
            name_a.contains("_fb15000_bb2000_"),
            "name should include first-byte and between-bytes timeout in the readable body"
        );
        assert!(
            name_b.contains("_fb15000_bb500_"),
            "name should include first-byte and between-bytes timeout in the readable body"
        );
    }

    #[test]
    fn discriminators_that_sanitize_alike_produce_distinct_names() {
        // `provider.a` and `provider_a` both sanitize to the same readable slug
        // (`.` maps to `_`). Before the spec digest they collided to one backend
        // name, so the second registration silently reused the first — routing
        // one provider's auction traffic through another's backend. The digest
        // over the raw spec must keep them distinct.
        let dotted = BackendConfig::new("https", "gateway.example.com")
            .discriminator(Some("provider.a"))
            .predict_name()
            .expect("should predict name for dotted discriminator");
        let underscored = BackendConfig::new("https", "gateway.example.com")
            .discriminator(Some("provider_a"))
            .predict_name()
            .expect("should predict name for underscored discriminator");
        assert_ne!(
            dotted, underscored,
            "discriminators differing only by a sanitized character must not collide"
        );
    }

    #[test]
    fn host_overrides_that_sanitize_alike_produce_distinct_names() {
        // `host.example.com:8443` (host+port) and `host.example.com.8443` (DNS
        // label) are both valid overrides that sanitize to the same readable
        // slug (`:` and `.` both map to `_`). The digest over the raw value must
        // keep the two backends — with different Host routing — distinct.
        let with_port = BackendConfig::new("https", "origin.example.com")
            .host_header_override(Some("host.example.com:8443"))
            .predict_name()
            .expect("should predict name for host:port override");
        let with_label = BackendConfig::new("https", "origin.example.com")
            .host_header_override(Some("host.example.com.8443"))
            .predict_name()
            .expect("should predict name for dotted-label override");
        assert_ne!(
            with_port, with_label,
            "host overrides differing only by a sanitized character must not collide"
        );
    }

    #[test]
    fn long_host_and_discriminator_stay_within_the_length_limit() {
        // A syntactically valid maximum-length DNS host plus a discriminator
        // previously pushed the name past Fastly's 255-char limit, so
        // `predict_name` succeeded while `ensure` failed at registration. The
        // bounded prefix + fixed-width digest must keep prediction, and the name
        // it predicts, within the limit.
        let label = "a".repeat(63);
        let long_host = format!("{label}.{label}.{label}.{label}.example.com");
        assert!(
            long_host.len() > 200,
            "should exercise a host longer than the readable-prefix bound"
        );
        let name = BackendConfig::new("https", &long_host)
            .discriminator(Some("prebid"))
            .predict_name()
            .expect("should predict a bounded name for a long host and discriminator");
        assert!(
            name.len() <= MAX_BACKEND_NAME_LEN,
            "name should stay within the {MAX_BACKEND_NAME_LEN}-char limit, got {}",
            name.len()
        );

        // Two long hosts sharing the truncated prefix must still resolve to
        // different backends via the digest.
        let other_host = format!("{label}.{label}.{label}.{label}.example.net");
        let other = BackendConfig::new("https", &other_host)
            .discriminator(Some("prebid"))
            .predict_name()
            .expect("should predict a bounded name for the sibling host");
        assert_ne!(
            name, other,
            "hosts sharing a truncated prefix must stay distinct via the digest"
        );
    }
}
