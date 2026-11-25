//! Shared client for a deployed service's `^/_ts/admin` routes.
//!
//! `ts cache purge` and `ts keys` all call basic-auth admin routes on a running service.
//! They share one implementation of the transport rules here so that the service URL
//! checks ([`crate::url_guard::service_endpoint`]), the password source, redirect refusal
//! and 401 guidance cannot drift apart between commands.

use core::error::Error;
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

/// Shorter than [`REQUEST_TIMEOUT`] so a stalled TCP or TLS handshake fails as a connect
/// error rather than as an overall timeout that reqwest does not mark as one.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

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
            // Without it the overall timeout fires first on a stalled handshake, and the
            // message below would claim the request was sent when nothing left this machine.
            .connect_timeout(CONNECT_TIMEOUT)
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
            .map_err(|error| {
                let is_connect = error.is_connect();
                let error = describe_cause(&error.without_url());
                if is_connect {
                    format!("could not reach {endpoint}: {error}")
                } else {
                    // The request may have been delivered: a key rotation is not idempotent, so
                    // the operator must check before retrying rather than assume nothing ran.
                    format!(
                        "sent the request to {endpoint} but got no response ({error}); the \
                         service may have applied it, so check its state before retrying"
                    )
                }
            })?;
        let status = response.status();
        let body = response.text().await.map_err(|error| {
            let error = describe_cause(&error.without_url());
            // A status line means the service handled the request, so a retry could apply a
            // rotation twice.
            format!(
                "{endpoint} answered {status} but its body could not be read ({error}); the \
                 service may have applied the request, so check its state before retrying"
            )
        })?;
        Ok(AdminResponse { status, body })
    })
}

/// Describe an error by its causes rather than its generic top-level text.
///
/// reqwest reports every transport failure as `error sending request`; the useful detail
/// (refused, timed out, reset) lives in the `source()` chain. The outermost message is kept
/// only when there is no cause, and repeated messages in the chain are dropped.
fn describe_cause(error: &(dyn Error + 'static)) -> String {
    let mut messages: Vec<String> = Vec::new();
    let mut cause = error.source();
    while let Some(current) = cause {
        let message = current.to_string();
        if messages.last() != Some(&message) {
            messages.push(message);
        }
        cause = current.source();
    }
    if messages.is_empty() {
        error.to_string()
    } else {
        messages.join(": ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, derive_more::Display)]
    #[display("{message}")]
    struct ChainError {
        message: &'static str,
        source: Option<Box<ChainError>>,
    }

    impl Error for ChainError {
        fn source(&self) -> Option<&(dyn Error + 'static)> {
            self.source
                .as_deref()
                .map(|source| source as &(dyn Error + 'static))
        }
    }

    fn chain(messages: &[&'static str]) -> ChainError {
        let mut error: Option<ChainError> = None;
        for message in messages.iter().rev() {
            error = Some(ChainError {
                message,
                source: error.map(Box::new),
            });
        }
        error.expect("should build a non-empty chain")
    }

    #[test]
    fn describe_cause_drops_the_generic_outer_message_and_repeats() {
        let error = chain(&[
            "error sending request",
            "client error (Connect)",
            "connection refused",
            "connection refused",
        ]);

        assert_eq!(
            describe_cause(&error),
            "client error (Connect): connection refused",
            "should list the causes once each without the outer text"
        );
    }

    #[test]
    fn describe_cause_keeps_the_message_when_there_is_no_cause() {
        let error = chain(&["error decoding response body"]);

        assert_eq!(
            describe_cause(&error),
            "error decoding response body",
            "should fall back to the error's own text"
        );
    }
}
