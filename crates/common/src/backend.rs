use std::time::Duration;

use error_stack::{Report, ResultExt};
use fastly::backend::Backend;
use url::Url;

use crate::error::TrustedServerError;

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
    if port != default_port_for_scheme(scheme) {
        format!("{}:{}", host, port)
    } else {
        host.to_string()
    }
}

/// Ensure a dynamic backend exists for the given origin and return its name.
///
/// The backend name is derived from the scheme and `host[:port]` to avoid collisions across
/// http/https or different ports. If a backend with the derived name already exists,
/// this function logs and reuses it.
///
/// # Arguments
///
/// * `scheme` - The URL scheme ("http" or "https")
/// * `host` - The hostname
/// * `port` - Optional port number
/// * `certificate_check` - If true, enables TLS certificate verification (default for production)
///
/// # Errors
///
/// Returns an error if the host is empty or if backend creation fails
/// (except for `NameInUse` which reuses the existing backend).
pub fn ensure_origin_backend(
    scheme: &str,
    host: &str,
    port: Option<u16>,
    certificate_check: bool,
) -> Result<String, Report<TrustedServerError>> {
    if host.is_empty() {
        return Err(Report::new(TrustedServerError::Proxy {
            message: "missing host".to_string(),
        }));
    }

    let target_port = port.unwrap_or_else(|| default_port_for_scheme(scheme));

    let host_with_port = format!("{}:{}", host, target_port);

    // Name: iframe_<scheme>_<host>_<port> (sanitize '.' and ':')
    // Include cert setting in name to avoid reusing a backend with different cert settings
    let name_base = format!("{}_{}_{}", scheme, host, target_port);
    let cert_suffix = if certificate_check { "" } else { "_nocert" };
    let backend_name = format!(
        "backend_{}{}",
        name_base.replace(['.', ':'], "_"),
        cert_suffix
    );

    let host_header = compute_host_header(scheme, host, target_port);

    // Target base is host[:port]; SSL is enabled only for https scheme
    let mut builder = Backend::builder(&backend_name, &host_with_port)
        .override_host(&host_header)
        .connect_timeout(Duration::from_secs(1))
        .first_byte_timeout(Duration::from_secs(15))
        .between_bytes_timeout(Duration::from_secs(10));
    if scheme.eq_ignore_ascii_case("https") {
        builder = builder.enable_ssl().sni_hostname(host);
        if certificate_check {
            builder = builder
                .enable_ssl()
                .sni_hostname(host)
                .check_certificate(host);
        } else {
            log::warn!(
                "INSECURE: certificate check disabled for backend: {}",
                backend_name
            );
        }
        log::info!("enable ssl for backend: {}", backend_name);
    }

    match builder.finish() {
        Ok(_) => {
            log::info!(
                "created dynamic backend: {} -> {}",
                backend_name,
                host_with_port
            );
            Ok(backend_name)
        }
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("NameInUse") || msg.contains("already in use") {
                log::info!("reusing existing dynamic backend: {}", backend_name);
                Ok(backend_name)
            } else {
                Err(Report::new(TrustedServerError::Proxy {
                    message: format!(
                        "dynamic backend creation failed ({} -> {}): {}",
                        backend_name, host_with_port, msg
                    ),
                }))
            }
        }
    }
}

/// Ensures a dynamic backend exists for the given origin URL.
///
/// Parses the URL and delegates to [`ensure_origin_backend`] to create or reuse a backend.
///
/// # Errors
///
/// Returns an error if the URL cannot be parsed or lacks a host, or if backend creation fails.
pub fn ensure_backend_from_url(
    origin_url: &str,
    certificate_check: bool,
) -> Result<String, Report<TrustedServerError>> {
    let parsed_url = Url::parse(origin_url).change_context(TrustedServerError::Proxy {
        message: format!("Invalid origin_url: {}", origin_url),
    })?;

    let scheme = parsed_url.scheme();
    let host = parsed_url.host_str().ok_or_else(|| {
        Report::new(TrustedServerError::Proxy {
            message: "Missing host in origin_url".to_string(),
        })
    })?;
    let port = parsed_url.port();

    ensure_origin_backend(scheme, host, port, certificate_check)
}

#[cfg(test)]
mod tests {
    use super::{compute_host_header, ensure_origin_backend};

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
        let name = ensure_origin_backend("https", "origin.example.com", None, true)
            .expect("should create backend for valid HTTPS origin");
        assert_eq!(name, "backend_https_origin_example_com_443");
    }

    #[test]
    fn returns_name_for_https_without_cert_check() {
        let name = ensure_origin_backend("https", "origin.example.com", None, false)
            .expect("should create backend with cert check disabled");
        assert_eq!(name, "backend_https_origin_example_com_443_nocert");
    }

    #[test]
    fn returns_name_for_http_with_port_and_sanitizes() {
        let name = ensure_origin_backend("http", "api.test-site.org", Some(8080), true)
            .expect("should create backend for HTTP origin with explicit port");
        assert_eq!(name, "backend_http_api_test-site_org_8080");
        assert!(
            name.ends_with("_8080"),
            "should sanitize ':' to '_' in backend name"
        );
    }

    #[test]
    fn returns_name_for_http_without_port_defaults_to_80() {
        let name = ensure_origin_backend("http", "example.org", None, true)
            .expect("should create backend defaulting to port 80 for HTTP");
        assert_eq!(name, "backend_http_example_org_80");
    }

    #[test]
    fn error_on_missing_host() {
        let err =
            ensure_origin_backend("https", "", None, true).expect_err("should reject empty host");
        let msg = err.to_string();
        assert!(
            msg.contains("missing host"),
            "should report missing host in error message"
        );
    }

    #[test]
    fn second_call_reuses_existing_backend() {
        let first = ensure_origin_backend("https", "reuse.example.com", None, true)
            .expect("should create backend on first call");
        let second = ensure_origin_backend("https", "reuse.example.com", None, true)
            .expect("should reuse backend on second call");
        assert_eq!(
            first, second,
            "should return same backend name on repeat call"
        );
    }
}
