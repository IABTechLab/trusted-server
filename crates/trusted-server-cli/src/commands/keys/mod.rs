//! `ts keys` — inspect a deployed service's request-signing keys.
//!
//! Both subcommands call routes that need no credentials, so neither asks for nor sends
//! the admin password.

pub mod list;
pub mod verify;

use std::time::Duration;

use clap::Subcommand;

use crate::error::CliResult;
use crate::run::RunOutcome;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Subcommands under `ts keys`.
#[derive(Debug, Subcommand)]
pub enum KeysCommand {
    /// List the active signing keys a service publishes in its discovery document.
    List(ListArgs),
    /// Ask a service whether a signature over a payload verifies under a key.
    Verify(VerifyArgs),
}

/// Arguments for `ts keys list`.
#[derive(Debug, clap::Args)]
pub struct ListArgs {
    /// HTTPS base URL of the service; HTTP is permitted only for loopback development.
    #[arg(long)]
    pub service: String,

    /// Print the discovery document exactly as the service returned it.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `ts keys verify`.
#[derive(Debug, clap::Args)]
pub struct VerifyArgs {
    /// HTTPS base URL of the service; HTTP is permitted only for loopback development.
    #[arg(long)]
    pub service: String,

    /// Key identifier the signature was made with.
    #[arg(long)]
    pub kid: String,

    /// Canonical payload that was signed.
    #[arg(long)]
    pub payload: String,

    /// Base64-encoded signature to check.
    #[arg(long)]
    pub signature: String,
}

/// Run a `ts keys` subcommand.
///
/// `ts keys verify` returns [`RunOutcome::AssertionFailed`] when the signature does not
/// verify, so the process exits non-zero and a script can rely on the exit code.
///
/// # Errors
///
/// Returns an error when the service URL is unsafe or malformed, the service cannot be
/// reached, or it answers with a non-success status or an unexpected body.
pub fn run(command: KeysCommand, out: &mut impl std::io::Write) -> CliResult<RunOutcome> {
    match command {
        KeysCommand::List(args) => list::run_list(&args, out).map(|()| RunOutcome::Success),
        KeysCommand::Verify(args) => verify::run_verify(&args, out),
    }
}

/// An unauthenticated request to send to the service.
enum KeysRequest<'body> {
    Get,
    PostJson(&'body str),
}

/// Send one request without credentials and return the status and body text.
///
/// Redirects are not followed: a service that redirects these routes is misconfigured,
/// and following one would quietly report on a different host than the operator named.
fn send(endpoint: &str, request: &KeysRequest<'_>) -> CliResult<(reqwest::StatusCode, String)> {
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
        let builder = match request {
            KeysRequest::Get => client.get(endpoint),
            KeysRequest::PostJson(body) => client
                .post(endpoint)
                .header("content-type", "application/json")
                .body((*body).to_owned()),
        };
        let response = builder
            .header("accept", "application/json")
            .send()
            .await
            .map_err(|error| format!("could not reach {endpoint}: {error}"))?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|error| format!("could not read the response from {endpoint}: {error}"))?;
        Ok((status, text))
    })
}
