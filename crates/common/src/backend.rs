use std::time::Duration;

use error_stack::{Report, ResultExt};
use fastly::backend::Backend;
use url::Url;

use crate::error::TrustedServerError;

/// Ensure a dynamic backend exists for the given origin and return its name.
///
/// The backend name is derived from the scheme and `host[:port]` to avoid collisions across
/// http/https or different ports. If a backend with the derived name already exists,
/// this function logs and reuses it.
pub fn ensure_origin_backend(
    scheme: &str,
    host: &str,
    port: Option<u16>,
) -> Result<String, Report<TrustedServerError>> {
    if host.is_empty() {
        return Err(Report::new(TrustedServerError::Proxy {
            message: "missing host".to_string(),
        }));
    }

    let is_https = scheme.eq_ignore_ascii_case("https");
    let target_port = match (port, is_https) {
        (Some(p), _) => p,
        (None, true) => 443,
        (None, false) => 80,
    };

    let host_with_port = format!("{}:{}", host, target_port);

    // Name: iframe_<scheme>_<host>_<port> (sanitize '.' and ':')
    let name_base = format!("{}_{}_{}", scheme, host, target_port);
    let backend_name = format!("backend_{}", name_base.replace(['.', ':'], "_"));

    // Target base is host[:port]; SSL is enabled only for https scheme
    let mut builder = Backend::builder(&backend_name, &host_with_port)
        .override_host(host)
        .connect_timeout(Duration::from_secs(1))
        .first_byte_timeout(Duration::from_secs(15))
        .between_bytes_timeout(Duration::from_secs(10));
    if scheme.eq_ignore_ascii_case("https") {
        builder = builder.enable_ssl();
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
pub fn ensure_backend_from_url(origin_url: &str) -> Result<String, Report<TrustedServerError>> {
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

    ensure_origin_backend(scheme, host, port)
}

#[cfg(test)]
mod tests {
    use super::ensure_origin_backend;

    #[test]
    fn returns_name_for_https_no_port() {
        let name = ensure_origin_backend("https", "origin.example.com", None).unwrap();
        assert_eq!(name, "backend_https_origin_example_com_443");
    }

    #[test]
    fn returns_name_for_http_with_port_and_sanitizes() {
        let name = ensure_origin_backend("http", "api.test-site.org", Some(8080)).unwrap();
        assert_eq!(name, "backend_http_api_test-site_org_8080");
        // Explicitly check that ':' was replaced with '_'
        assert!(name.ends_with("_8080"));
    }

    #[test]
    fn returns_name_for_http_without_port_defaults_to_80() {
        let name = ensure_origin_backend("http", "example.org", None).unwrap();
        assert_eq!(name, "backend_http_example_org_80");
    }

    #[test]
    fn error_on_missing_host() {
        let err = ensure_origin_backend("https", "", None).err().unwrap();
        let msg = err.to_string();
        assert!(msg.contains("missing host"));
    }

    #[test]
    fn second_call_reuses_existing_backend() {
        let first = ensure_origin_backend("https", "reuse.example.com", None).unwrap();
        let second = ensure_origin_backend("https", "reuse.example.com", None).unwrap();
        assert_eq!(first, second);
    }
}
