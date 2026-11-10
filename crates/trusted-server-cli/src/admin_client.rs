//! Shared client for a deployed service's `^/_ts/admin` routes.
//!
//! `ts cache purge` and `ts keys` all call basic-auth admin routes on a running service.
//! They share one implementation of the transport rules here so that the service URL
//! checks ([`crate::url_guard::service_endpoint`]), the password source, redirect refusal
//! and 401 guidance cannot drift apart between commands.

use std::time::Duration;

use reqwest::StatusCode;

use crate::error::CliResult;

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
    let endpoint = crate::url_guard::service_endpoint(service, path)?;
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
