//! Shared client for a deployed service's `^/_ts/admin` routes.
//!
//! `ts cache purge` and `ts keys` all call basic-auth admin routes on a running service.
//! They share one implementation of the transport rules here so that the service URL
//! checks, the password source, redirect refusal and 401 guidance cannot drift apart
//! between commands.

use std::time::Duration;

use reqwest::StatusCode;

use crate::error::{CliResult, cli_error};

/// Environment variable carrying the admin password.
///
/// Read from the environment and never accepted as a flag: an argument is visible to every
/// other process on the host through `ps`, and lands in shell history.
pub const ADMIN_PASSWORD_ENVIRONMENT_VARIABLE: &str = "TRUSTED_SERVER_ADMIN_PASSWORD";

/// Hint appended to a 401 failure, shared by every admin command.
pub(crate) const UNAUTHORIZED_HINT: &str =
    " — check the admin username and $TRUSTED_SERVER_ADMIN_PASSWORD";

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// The raw answer from an admin route.
pub(crate) struct AdminResponse {
    /// HTTP status the service answered with.
    pub(crate) status: StatusCode,
    /// Response body as text.
    pub(crate) body: String,
}

/// Join the service base URL and an admin path without doubling or dropping a slash.
///
/// # Errors
///
/// Returns an error when `service` is not an absolute base URL, carries credentials, a
/// path, query or fragment, or uses plaintext HTTP to a non-loopback host.
pub(crate) fn admin_endpoint(service: &str, path: &str) -> CliResult<String> {
    let mut url =
        reqwest::Url::parse(service).map_err(|_| "--service must be an absolute HTTPS URL")?;
    crate::url_guard::require_credential_safe_transport(&url, "--service")?;
    if url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
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

/// Read the admin password from [`ADMIN_PASSWORD_ENVIRONMENT_VARIABLE`].
///
/// # Errors
///
/// Returns an error explaining where the password must come from when it is unset.
pub(crate) fn admin_password() -> CliResult<String> {
    std::env::var(ADMIN_PASSWORD_ENVIRONMENT_VARIABLE).map_err(|_| {
        format!(
            "set {ADMIN_PASSWORD_ENVIRONMENT_VARIABLE} to the service's admin password \
             (it is read from the environment, never a flag, so it does not reach `ps` \
             output or shell history)"
        )
    })
}

/// POST a JSON body to an admin route and return the status and body.
///
/// The endpoint is validated and the password read before any connection is made, so an
/// unsafe URL or a missing credential never reaches the network. Redirects are not
/// followed, so a login page cannot masquerade as a success.
///
/// # Errors
///
/// Returns an error when the endpoint is invalid, the password is missing, or the
/// service cannot be reached or its body read. Non-success statuses are returned as an
/// [`AdminResponse`] for the caller to interpret.
pub(crate) fn post_admin_json(
    service: &str,
    path: &str,
    username: &str,
    body: String,
) -> CliResult<AdminResponse> {
    let endpoint = admin_endpoint(service, path)?;
    let password = admin_password()?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("failed to start the HTTP runtime: {error}"))?;

    // Without this the first HTTPS request panics with "No provider set".
    crate::tls::install_crypto_provider();

    runtime.block_on(async {
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| format!("failed to build the HTTP client: {error}"))?;
        let response = client
            .post(&endpoint)
            .basic_auth(username, Some(&password))
            .header("content-type", "application/json")
            .body(body)
            .send()
            .await
            .map_err(|error| format!("could not reach {endpoint}: {error}"))?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|error| format!("could not read the response from {endpoint}: {error}"))?;
        Ok(AdminResponse { status, body })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_endpoint_url_survives_a_trailing_slash() {
        for service in [
            "https://edge.example.com",
            "https://edge.example.com/",
            "https://edge.example.com///",
        ] {
            assert_eq!(
                admin_endpoint(service, "/_ts/admin/cache/purge")
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
                admin_endpoint(service, "/_ts/admin/keys/rotate").is_ok(),
                "should permit loopback development: {service}"
            );
        }
        for service in [
            "http://192.0.2.1",     // allow-domain: 192.0.2.1
            "http://[2001:db8::1]", // allow-domain: 2001:db8::1
            "http://localhost.example.com",
        ] {
            assert!(
                admin_endpoint(service, "/_ts/admin/keys/rotate").is_err(),
                "should refuse remote plaintext transport: {service}"
            );
        }
    }

    #[test]
    fn service_urls_cannot_redirect_the_admin_path_or_embed_credentials() {
        for service in [
            "not a URL",
            "https://example.com/path",
            "https://example.com?query=1",
            "https://example.com#fragment",
            "https://user:example-password@example.com",
        ] {
            assert!(
                admin_endpoint(service, "/_ts/admin/keys/rotate").is_err(),
                "should require an unambiguous service base URL: {service}"
            );
        }
    }
}
