use std::time::Duration;

use error_stack::{Report, ResultExt as _};
use fastly::backend::Backend;
use url::Url;

use trusted_server_core::error::TrustedServerError;
use trusted_server_core::host_header::validate_host_header_override_value;

/// Returns the default port for the given scheme (443 for HTTPS, 80 for HTTP).
#[inline]
fn default_port_for_scheme(scheme: &str) -> u16 {
    if scheme.eq_ignore_ascii_case("https") {
        443
    } else {
        80
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
    if port == default_port_for_scheme(scheme) {
        host.to_owned()
    } else {
        format!("{host}:{port}")
    }
}

fn sanitize_backend_name_component(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

/// Default first-byte timeout for backends (15 seconds).
pub(crate) const DEFAULT_FIRST_BYTE_TIMEOUT: Duration = Duration::from_secs(15);

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
    host_header_override: Option<&'a str>,
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
            host_header_override: None,
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

    /// Set the outbound Host header sent to the backend origin.
    #[must_use]
    pub fn host_header_override(mut self, host: Option<&'a str>) -> Self {
        self.host_header_override = host;
        self
    }

    /// Compute the deterministic backend name and resolved port without
    /// registering anything.
    ///
    /// The name encodes scheme, host, port, certificate setting, and
    /// first-byte timeout so that backends with different configurations
    /// never collide.  Including the timeout prevents "first-registration-wins"
    /// poisoning where a later request for the same origin with a tighter
    /// timeout would silently inherit the original registration's value.
    fn compute_name(&self) -> Result<(String, u16), Report<TrustedServerError>> {
        if self.host.is_empty() {
            return Err(Report::new(TrustedServerError::Proxy {
                message: "missing host".to_owned(),
            }));
        }
        if self.host.chars().any(char::is_control) {
            return Err(Report::new(TrustedServerError::Proxy {
                message: "host contains control characters".to_owned(),
            }));
        }
        if self.scheme.chars().any(char::is_control) {
            return Err(Report::new(TrustedServerError::Proxy {
                message: "scheme contains control characters".to_owned(),
            }));
        }
        if let Some(host_header_override) = self.host_header_override {
            validate_host_header_override_value(host_header_override).map_err(|reason| {
                Report::new(TrustedServerError::Proxy {
                    message: format!("host header override {reason}"),
                })
            })?;
        }

        let target_port = self
            .port
            .unwrap_or_else(|| default_port_for_scheme(self.scheme));

        let name_base = format!("{}_{}_{}", self.scheme, self.host, target_port);
        let host_override_suffix = self
            .host_header_override
            .map(|host| format!("_oh_{}", sanitize_backend_name_component(host)))
            .unwrap_or_default();
        let cert_suffix = if self.certificate_check {
            ""
        } else {
            "_nocert"
        };
        let timeout_ms = self.first_byte_timeout.as_millis();
        let backend_name = format!(
            "backend_{}{}{}_t{}",
            sanitize_backend_name_component(&name_base),
            host_override_suffix,
            cert_suffix,
            timeout_ms
        );

        Ok((backend_name, target_port))
    }

    /// Return the deterministic backend name without registering anything.
    ///
    /// Convenience wrapper over `Self::compute_name` that discards the
    /// resolved port, used by [`crate::platform::PlatformBackend`]
    /// implementations that only need the name for correlation.
    ///
    /// # Errors
    ///
    /// Returns an error if the host is empty.
    pub fn predict_name(self) -> Result<String, Report<TrustedServerError>> {
        self.compute_name().map(|(name, _)| name)
    }

    /// Ensure a dynamic backend exists for this configuration and return its name.
    ///
    /// The backend name is derived from the scheme, host, port, certificate
    /// setting, and `first_byte_timeout` to avoid collisions.  Different
    /// timeout values produce different backend registrations so that a
    /// tight deadline cannot be silently widened by an earlier registration.
    ///
    /// # Errors
    ///
    /// Returns an error if the host is empty or if backend creation fails
    /// (except for `NameInUse` which reuses the existing backend).
    pub fn ensure(self) -> Result<String, Report<TrustedServerError>> {
        let (backend_name, target_port) = self.compute_name()?;

        let host_with_port = format!("{}:{}", self.host, target_port);

        let host_header = self.host_header_override.map_or_else(
            || compute_host_header(self.scheme, self.host, target_port),
            str::to_owned,
        );

        // Target base is host[:port]; SSL is enabled only for https scheme
        let mut builder = Backend::builder(&backend_name, &host_with_port)
            .override_host(&host_header)
            .connect_timeout(Duration::from_secs(1))
            .first_byte_timeout(self.first_byte_timeout)
            .between_bytes_timeout(Duration::from_secs(10));
        if self.scheme.eq_ignore_ascii_case("https") {
            builder = builder.enable_ssl().sni_hostname(self.host);
            if self.certificate_check {
                builder = builder.check_certificate(self.host);
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
    use super::{compute_host_header, BackendConfig};

    // Tests for compute_host_header - the fix for port preservation in Host header
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
        assert_eq!(name, "backend_https_origin_example_com_443_t15000");
    }

    #[test]
    fn returns_name_for_https_without_cert_check() {
        let name = BackendConfig::new("https", "origin.example.com")
            .certificate_check(false)
            .ensure()
            .expect("should create backend with cert check disabled");
        assert_eq!(name, "backend_https_origin_example_com_443_nocert_t15000");
    }

    #[test]
    fn returns_name_for_http_with_port_and_sanitizes() {
        let name = BackendConfig::new("http", "api.test-site.org")
            .port(Some(8080))
            .ensure()
            .expect("should create backend for HTTP origin with explicit port");
        assert_eq!(name, "backend_http_api_test-site_org_8080_t15000");
    }

    #[test]
    fn returns_name_for_http_without_port_defaults_to_80() {
        let name = BackendConfig::new("http", "example.org")
            .ensure()
            .expect("should create backend defaulting to port 80 for HTTP");
        assert_eq!(name, "backend_http_example_org_80_t15000");
    }

    #[test]
    fn error_on_host_with_control_characters() {
        let err = BackendConfig::new("https", "evil.com\nINFO fake log entry")
            .predict_name()
            .expect_err("should reject host containing newline");
        assert!(
            err.to_string().contains("control characters"),
            "should report control characters in error message"
        );
    }

    #[test]
    fn error_on_missing_host() {
        let err = BackendConfig::new("https", "")
            .ensure()
            .expect_err("should reject empty host");
        let msg = err.to_string();
        assert!(
            msg.contains("missing host"),
            "should report missing host in error message"
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
        let (name_a, _) = BackendConfig::new("https", "origin.example.com")
            .host_header_override(Some("www.example.com"))
            .compute_name()
            .expect("should compute name with host header override");
        let (name_b, _) = BackendConfig::new("https", "origin.example.com")
            .host_header_override(Some("m.example.com"))
            .compute_name()
            .expect("should compute name with different host header override");

        assert_ne!(
            name_a, name_b,
            "backends with different host header overrides should have different names"
        );
        assert_eq!(
            name_a,
            "backend_https_origin_example_com_443_oh_www_example_com_t15000"
        );
        assert_eq!(
            name_b,
            "backend_https_origin_example_com_443_oh_m_example_com_t15000"
        );
    }

    #[test]
    fn host_header_override_rejects_control_characters() {
        let err = BackendConfig::new("https", "origin.example.com")
            .host_header_override(Some("www\n.example.com"))
            .predict_name()
            .expect_err("should reject host header override containing newline");

        assert!(
            err.to_string().contains("control characters"),
            "should report control characters in error message"
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
                err.to_string().contains("host header override"),
                "should report host header override error for {host_header_override:?}"
            );
        }
    }

    #[test]
    fn different_timeouts_produce_different_names() {
        use std::time::Duration;

        let (name_a, _) = BackendConfig::new("https", "origin.example.com")
            .first_byte_timeout(Duration::from_secs(2))
            .compute_name()
            .expect("should compute name with 2000ms timeout");
        let (name_b, _) = BackendConfig::new("https", "origin.example.com")
            .first_byte_timeout(Duration::from_millis(500))
            .compute_name()
            .expect("should compute name with 500ms timeout");
        assert_ne!(
            name_a, name_b,
            "backends with different timeouts should have different names"
        );
        assert!(
            name_a.ends_with("_t2000"),
            "name should include timeout suffix"
        );
        assert!(
            name_b.ends_with("_t500"),
            "name should include timeout suffix"
        );
    }
}
