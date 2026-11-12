//! The HTTP client shared by every command that calls a deployed service's `^/_ts/admin`
//! routes.
//!
//! One implementation rather than one per command: each command sends the shared admin
//! password, so the service URL checks, the source of the password, the refusal to follow
//! redirects and the `401` hint must not drift apart between them.

use std::time::Duration;

use reqwest::StatusCode;

use crate::error::{CliResult, cli_error};

/// Environment variable carrying the admin password.
///
/// Read from the environment and never accepted as a flag: an argument is visible to every
/// other process on the host through `ps`, and lands in shell history.
pub(crate) const ADMIN_PASSWORD_ENVIRONMENT_VARIABLE: &str = "TRUSTED_SERVER_ADMIN_PASSWORD";

/// Hint appended to a `401` answer by every admin command.
pub(crate) const UNAUTHORIZED_HINT: &str =
    " — check the admin username and $TRUSTED_SERVER_ADMIN_PASSWORD";

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// A validated admin service base URL and the credentials to send to it.
pub(crate) struct AdminClient {
    base: reqwest::Url,
    username: String,
    password: String,
}

/// A request to one admin route.
pub(crate) enum AdminRequest<'a> {
    /// `GET` with an optional `Cookie` header.
    Get {
        path: &'a str,
        cookie: Option<&'a str>,
    },
    /// `POST` with a JSON body.
    PostJson { path: &'a str, body: String },
}

/// The service's answer: the status and the body as text.
pub(crate) struct AdminResponse {
    pub(crate) status: StatusCode,
    pub(crate) body: String,
}

impl AdminClient {
    /// Validate `service` and read the admin password from the environment.
    ///
    /// The URL is checked first, so an unsafe `--service` is reported even when the
    /// password is also missing.
    ///
    /// # Errors
    ///
    /// Returns an error when `service` is not a base HTTPS URL (HTTP is allowed only for
    /// loopback hosts), embeds credentials, a path, query or fragment, or when
    /// [`ADMIN_PASSWORD_ENVIRONMENT_VARIABLE`] is unset.
    pub(crate) fn new(service: &str, username: &str) -> CliResult<Self> {
        let base = service_base_url(service)?;
        let password = std::env::var(ADMIN_PASSWORD_ENVIRONMENT_VARIABLE).map_err(|_| {
            format!(
                "set {ADMIN_PASSWORD_ENVIRONMENT_VARIABLE} to the service's admin password \
                 (it is read from the environment, never a flag, so it does not reach `ps` \
                 output or shell history)"
            )
        })?;
        Ok(Self {
            base,
            username: username.to_owned(),
            password,
        })
    }

    /// The absolute URL of an admin route on this service.
    pub(crate) fn endpoint(&self, path: &str) -> String {
        let mut url = self.base.clone();
        url.set_path(path);
        url.to_string()
    }

    /// Send one request with basic auth and return the status and body.
    ///
    /// Redirects are never followed: a redirect to a login page would otherwise arrive as
    /// a `200`, and following one cross-origin would resend the credentials.
    ///
    /// # Errors
    ///
    /// Returns an error when the HTTP runtime or client cannot be built, the service
    /// cannot be reached, or the response body cannot be read.
    pub(crate) fn send(&self, request: AdminRequest<'_>) -> CliResult<AdminResponse> {
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
            let (endpoint, builder) = match request {
                AdminRequest::Get { path, cookie } => {
                    let endpoint = self.endpoint(path);
                    let mut builder = client.get(&endpoint);
                    if let Some(cookie) = cookie {
                        builder = builder.header(reqwest::header::COOKIE, cookie);
                    }
                    (endpoint, builder)
                }
                AdminRequest::PostJson { path, body } => {
                    let endpoint = self.endpoint(path);
                    let builder = client
                        .post(&endpoint)
                        .header(reqwest::header::CONTENT_TYPE, "application/json")
                        .body(body);
                    (endpoint, builder)
                }
            };
            let response = builder
                .basic_auth(&self.username, Some(&self.password))
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
}

impl AdminResponse {
    /// The service's explanation of a refusal.
    ///
    /// The admin handlers answer `{"error": "..."}`; anything else (the plaintext `401`
    /// challenge, an adapter's plaintext `5xx`) is returned trimmed.
    pub(crate) fn error_text(&self) -> String {
        serde_json::from_str::<serde_json::Value>(&self.body)
            .ok()
            .and_then(|value| value.get("error")?.as_str().map(str::to_owned))
            .unwrap_or_else(|| self.body.trim().to_owned())
    }
}

/// Parse and check an admin service base URL.
fn service_base_url(service: &str) -> CliResult<reqwest::Url> {
    let url =
        reqwest::Url::parse(service).map_err(|_| "--service must be an absolute HTTPS URL")?;
    crate::url_guard::require_credential_safe_transport(&url, "--service")?;
    if url.host().is_none()
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.path().trim_matches('/').is_empty()
    {
        return cli_error(
            "--service must be a base HTTPS URL without credentials, a path, query, or fragment",
        );
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoint(service: &str, path: &str) -> CliResult<String> {
        let base = service_base_url(service)?;
        let client = AdminClient {
            base,
            username: "admin".to_owned(),
            password: "example-password".to_owned(),
        };
        Ok(client.endpoint(path))
    }

    #[test]
    fn the_endpoint_url_survives_a_trailing_slash() {
        for service in [
            "https://edge.example.com",
            "https://edge.example.com/",
            "https://edge.example.com///",
        ] {
            assert_eq!(
                endpoint(service, "/_ts/admin/cache/purge").expect("should accept a base URL"),
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
                service_base_url(service).is_ok(),
                "should permit loopback development: {service}"
            );
        }
        // RFC 5737 and RFC 3849 documentation addresses: non-loopback hosts that are
        // never routable.
        for service in [
            "http://192.0.2.1",     // allow-domain: 192.0.2.1
            "http://[2001:db8::1]", // allow-domain: 2001:db8::1
            "http://localhost.example.com",
        ] {
            assert!(
                service_base_url(service).is_err(),
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
                service_base_url(service).is_err(),
                "should require an unambiguous service base URL: {service}"
            );
        }
    }

    #[test]
    fn error_text_prefers_the_json_error_field() {
        let json = AdminResponse {
            status: StatusCode::NOT_FOUND,
            body: r#"{"error":"EC entry not found"}"#.to_owned(),
        };
        assert_eq!(json.error_text(), "EC entry not found");

        let plain = AdminResponse {
            status: StatusCode::UNAUTHORIZED,
            body: " Unauthorized\n".to_owned(),
        };
        assert_eq!(
            plain.error_text(),
            "Unauthorized",
            "should fall back to the trimmed body"
        );
    }
}
