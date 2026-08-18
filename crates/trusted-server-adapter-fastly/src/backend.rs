use core::fmt::Write as _;
use std::time::Duration;

use error_stack::{Report, ResultExt as _};
use fastly::backend::Backend;
use sha2::{Digest as _, Sha256};
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

/// Fastly's documented maximum length for a dynamic backend name.
const MAX_BACKEND_NAME_LEN: usize = 255;
/// Maximum length of the human-readable prefix folded into a backend name.
///
/// Bounds the name so that `backend_<prefix>_<digest>` can never exceed
/// [`MAX_BACKEND_NAME_LEN`]: 8 (`backend_`) + 200 + 1 (`_`) +
/// [`SPEC_DIGEST_HEX_LEN`] = 241 ≤ 255.
const MAX_READABLE_PREFIX_LEN: usize = 200;
/// Width of the hex digest suffix — the first 128 bits of a SHA-256 over the
/// full backend spec, which is collision-resistant at the handful-of-hundreds
/// scale of a service's dynamic backends.
const SPEC_DIGEST_HEX_LEN: usize = 32;

/// Hex-encode the first 128 bits of a SHA-256 digest of `canonical`.
///
/// Used to make a backend name a collision-resistant function of the complete
/// backend spec (see [`BackendConfig::canonical_spec_string`]).
fn spec_digest_hex(canonical: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(SPEC_DIGEST_HEX_LEN);
    for byte in digest.iter().take(SPEC_DIGEST_HEX_LEN / 2) {
        write!(hex, "{byte:02x}").expect("should write hex digit to string");
    }
    hex
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

    /// Build an unambiguous, length-prefixed encoding of the complete backend
    /// spec for digesting.
    ///
    /// Every field is prefixed with its byte length so that no two distinct
    /// specs can encode to the same string (a lossy substitution like
    /// `sanitize_backend_name_component` cannot guarantee this). `Option` fields
    /// are presence-tagged so a `None` never aliases a `Some("")`. The result is
    /// fed to [`spec_digest_hex`]; it is never parsed, only hashed.
    fn canonical_spec_string(&self, target_port: u16) -> String {
        fn push_field(buf: &mut String, field: &str) {
            buf.push_str(&field.len().to_string());
            buf.push(':');
            buf.push_str(field);
        }

        let mut buf = String::new();
        push_field(&mut buf, self.scheme);
        push_field(&mut buf, self.host);
        push_field(&mut buf, &target_port.to_string());
        push_field(&mut buf, if self.certificate_check { "1" } else { "0" });
        match self.host_header_override {
            Some(value) => {
                buf.push('s');
                push_field(&mut buf, value);
            }
            None => buf.push('n'),
        }
        match self.discriminator {
            Some(value) => {
                buf.push('s');
                push_field(&mut buf, value);
            }
            None => buf.push('n'),
        }
        push_field(&mut buf, &self.first_byte_timeout.as_millis().to_string());
        push_field(
            &mut buf,
            &self.between_bytes_timeout.as_millis().to_string(),
        );
        buf
    }

    /// Compute the deterministic backend name and resolved port without
    /// registering anything.
    ///
    /// The name is `backend_<readable>_<digest>`, where `<digest>` is a
    /// collision-resistant SHA-256 over an unambiguous encoding of the
    /// *complete* backend spec — scheme, host, port, certificate setting, Host
    /// override, provider discriminator, and the first-byte/between-bytes
    /// timeouts (see [`canonical_spec_string`](Self::canonical_spec_string)).
    /// Because distinct specs yield distinct digests, name equality implies spec
    /// equality: that is what makes reusing a `NameInUse` backend provably safe,
    /// and it prevents "first-registration-wins" poisoning where a later request
    /// with a tighter timeout would inherit an earlier registration's value. The
    /// `<readable>` half is a lossy, bounded slug carried only for logs — any
    /// collision there is harmless because uniqueness comes from the digest. The
    /// whole name is bounded to [`MAX_BACKEND_NAME_LEN`] so a long host or
    /// discriminator can never produce a name Fastly rejects at registration.
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
        let discriminator_suffix = self
            .discriminator
            .map(|d| format!("_p_{}", sanitize_backend_name_component(d)))
            .unwrap_or_default();
        let first_byte_timeout_ms = self.first_byte_timeout.as_millis();
        let between_bytes_timeout_ms = self.between_bytes_timeout.as_millis();

        // Lossy, human-readable slug for logs. Correctness does not depend on
        // it — uniqueness comes from the digest below — so it is bounded to a
        // fixed length. Sanitization only emits ASCII, so a char-boundary take
        // is byte-exact.
        let readable_full = format!(
            "{}{}{}{}_fb{}_bb{}",
            sanitize_backend_name_component(&name_base),
            host_override_suffix,
            cert_suffix,
            discriminator_suffix,
            first_byte_timeout_ms,
            between_bytes_timeout_ms
        );
        let readable: String = readable_full
            .chars()
            .take(MAX_READABLE_PREFIX_LEN)
            .collect();

        // Collision-resistant over the *complete* spec, so name equality implies
        // spec equality and `NameInUse` reuse is safe.
        let digest = spec_digest_hex(&self.canonical_spec_string(target_port));
        let backend_name = format!("backend_{readable}_{digest}");

        // Bounded by construction; assert it so any future format change fails
        // attributably during prediction rather than at Fastly registration.
        if backend_name.len() > MAX_BACKEND_NAME_LEN {
            return Err(Report::new(TrustedServerError::Proxy {
                message: format!(
                    "backend name exceeds {MAX_BACKEND_NAME_LEN}-char limit ({} chars)",
                    backend_name.len()
                ),
            }));
        }

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
    /// setting, `first_byte_timeout`, and `between_bytes_timeout` to avoid
    /// collisions. Different timeout values produce different backend
    /// registrations so that a tight deadline cannot be silently widened by an
    /// earlier registration.
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
            .between_bytes_timeout(self.between_bytes_timeout);
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
    use super::{BackendConfig, MAX_BACKEND_NAME_LEN, SPEC_DIGEST_HEX_LEN, compute_host_header};

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

        let (name_a, _) = BackendConfig::new("https", "origin.example.com")
            .between_bytes_timeout(Duration::from_secs(2))
            .compute_name()
            .expect("should compute name with 2000ms between-bytes timeout");
        let (name_b, _) = BackendConfig::new("https", "origin.example.com")
            .between_bytes_timeout(Duration::from_millis(500))
            .compute_name()
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
