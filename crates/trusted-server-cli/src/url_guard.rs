//! Transport checks shared by the commands that send an operator's credentials to a URL.
//!
//! One implementation rather than one per command: a second copy is a second place for the
//! loopback exemption to drift, and every command that gets this wrong sends a session
//! cookie or an admin password over the wire in the clear.

use crate::error::{CliResult, cli_error};

/// Whether the URL names a loopback host a developer runs a service on.
fn is_loopback(url: &reqwest::Url) -> bool {
    match url.host() {
        Some(url::Host::Ipv4(address)) => address.is_loopback(),
        Some(url::Host::Ipv6(address)) => address.is_loopback(),
        Some(url::Host::Domain(name)) => name.eq_ignore_ascii_case("localhost"),
        None => false,
    }
}

/// Refuse a URL that would carry a credential in the clear, or inside its own userinfo.
///
/// `flag` names the option being validated, so the message points at what the operator
/// typed rather than at an internal value.
///
/// # Errors
///
/// Returns an error when the scheme is not HTTPS and the host is not loopback, or when the
/// URL embeds a username or password.
pub(crate) fn require_credential_safe_transport(url: &reqwest::Url, flag: &str) -> CliResult<()> {
    if url.scheme() != "https" && !(url.scheme() == "http" && is_loopback(url)) {
        return cli_error(format!(
            "{flag} requires HTTPS to protect the credentials sent with the request; HTTP is \
             allowed only for loopback development services"
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return cli_error(format!(
            "{flag} must not embed credentials in the URL; userinfo is logged by proxies and \
             is sent before any transport check can protect it"
        ));
    }
    Ok(())
}

/// Join a service base URL and an endpoint path without doubling or dropping a slash.
///
/// The service must be a bare base URL: credentials, a path, a query, or a fragment would
/// each let the typed value change which endpoint is reached.
///
/// # Errors
///
/// Returns an error when `service` is not an absolute URL, fails
/// [`require_credential_safe_transport`], or carries more than a scheme, host, and port.
pub(crate) fn service_endpoint(service: &str, path: &str) -> CliResult<String> {
    let mut url =
        reqwest::Url::parse(service).map_err(|_| "--service must be an absolute HTTPS URL")?;
    require_credential_safe_transport(&url, "--service")?;
    if url.host().is_none()
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.path().trim_matches('/').is_empty()
    {
        return cli_error(
            "--service must be a base HTTPS URL without credentials, a path, query, or fragment",
        );
    }
    url.set_path(path);
    Ok(url.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(url: &str) -> CliResult<()> {
        let parsed = reqwest::Url::parse(url).expect("should parse the test URL");
        require_credential_safe_transport(&parsed, "--url")
    }

    #[test]
    fn https_is_accepted() {
        assert!(check("https://example.com/article").is_ok());
    }

    #[test]
    fn plain_http_is_refused_off_loopback() {
        assert!(
            check("http://example.com/article").is_err(),
            "a probe cookie sent over HTTP is readable by every hop in between"
        );
        assert!(
            check("http://192.0.2.10:8080/article").is_err(),
            "a private-range address is still not loopback"
        );
    }

    #[test]
    fn http_is_allowed_only_on_loopback() {
        assert!(check("http://127.0.0.1:8080/article").is_ok());
        assert!(check("http://localhost:8080/article").is_ok());
        assert!(check("http://[::1]:8080/article").is_ok());
    }

    #[test]
    fn userinfo_is_refused() {
        assert!(
            check("https://user:example-password@example.com/").is_err(),
            "credentials in the URL reach proxy logs and shell history"
        );
        assert!(check("https://user@example.com/").is_err());
    }

    #[test]
    fn the_endpoint_url_survives_a_trailing_slash() {
        for service in [
            "https://edge.example.com",
            "https://edge.example.com/",
            "https://edge.example.com///",
        ] {
            assert_eq!(
                service_endpoint(service, "/_ts/admin/cache/purge")
                    .expect("should accept a secure base URL"),
                "https://edge.example.com/_ts/admin/cache/purge",
                "{service} must resolve to one well-formed endpoint"
            );
        }
    }

    #[test]
    fn only_loopback_services_may_use_plaintext_http() {
        for service in [
            "http://127.0.0.1:8080",
            "http://[::1]:8080",
            "http://localhost:8080",
        ] {
            assert!(
                service_endpoint(service, "/_ts/admin/keys/rotate").is_ok(),
                "should permit loopback development: {service}"
            );
        }
        for service in [
            "http://192.0.2.1",     // allow-domain: 192.0.2.1
            "http://[2001:db8::1]", // allow-domain: 2001:db8::1
            "http://localhost.example.com",
        ] {
            assert!(
                service_endpoint(service, "/_ts/admin/keys/rotate").is_err(),
                "should refuse remote plaintext transport: {service}"
            );
        }
    }

    #[test]
    fn service_urls_cannot_redirect_the_endpoint_path_or_embed_credentials() {
        for service in [
            "not a URL",
            "https://example.com/path",
            "https://example.com?query=1",
            "https://example.com#fragment",
            "https://user:example-password@example.com",
        ] {
            assert!(
                service_endpoint(service, "/_ts/admin/keys/rotate").is_err(),
                "should require an unambiguous service base URL: {service}"
            );
        }
    }
}
