//! `ts ec` — operator diagnostics for Edge Cookie identity through a deployed service's
//! admin routes.
//!
//! The responses carry raw consent strings and partner UIDs. A successful response is
//! written only to the command's output: never to disk, and never through the logger. A
//! refusal is reported like any other CLI error, with the service's `error` text.

pub mod eids;
pub mod show;

use clap::Subcommand;

use crate::error::CliResult;

/// Subcommands under `ts ec`.
#[derive(Debug, Subcommand)]
pub enum EcCommand {
    /// Show the identity-graph record stored for an EC ID and its auction EID preview.
    Show(ShowArgs),
    /// Preview which configured partners would match `ts-eids` and `sharedId` cookie values.
    Eids(EidsArgs),
}

/// Arguments for `ts ec show`.
#[derive(Debug, clap::Args)]
pub struct ShowArgs {
    /// HTTPS base URL of the service; HTTP is permitted only for loopback development.
    #[arg(long)]
    pub service: String,

    /// EC ID to look up: 64 lowercase hex characters, a dot, then 6 alphanumerics.
    pub ec_id: String,

    /// Admin username for the service's `^/_ts/admin` basic auth.
    #[arg(long, default_value = "admin")]
    pub username: String,

    /// Print the service's full JSON response unchanged.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `ts ec eids`.
///
/// The values are what a browser sends: copy them from the cookie table in the browser's
/// developer tools. At least one is required.
#[derive(Debug, clap::Args)]
#[command(group(
    clap::ArgGroup::new("cookies")
        .required(true)
        .multiple(true)
        .args(["ts_eids", "shared_id"])
))]
pub struct EidsArgs {
    /// HTTPS base URL of the service; HTTP is permitted only for loopback development.
    #[arg(long)]
    pub service: String,

    /// Value of the `ts-eids` cookie.
    #[arg(long)]
    pub ts_eids: Option<String>,

    /// Value of the `sharedId` cookie.
    #[arg(long)]
    pub shared_id: Option<String>,

    /// Admin username for the service's `^/_ts/admin` basic auth.
    #[arg(long, default_value = "admin")]
    pub username: String,

    /// Print the service's full JSON response unchanged.
    #[arg(long)]
    pub json: bool,
}

/// Run a `ts ec` subcommand.
///
/// # Errors
///
/// Returns an error when an argument is invalid, the admin password is absent from the
/// environment, the service cannot be reached, or it refuses the request.
pub fn run(command: EcCommand, out: &mut impl std::io::Write) -> CliResult<()> {
    match command {
        EcCommand::Show(args) => show::run_show(&args, out),
        EcCommand::Eids(args) => eids::run_eids(&args, out),
    }
}

/// Describe a success body that does not match the expected response.
///
/// Only the position is reported. `serde_json`'s message can quote the offending value,
/// which here could be a consent string or partner UID, and CLI errors are logged.
fn unexpected_answer(expected: &str, error: &serde_json::Error) -> String {
    format!(
        "the service's answer is not {expected} (JSON line {}, column {})",
        error.line(),
        error.column()
    )
}

/// The service's body exactly as received, ending in a newline so the shell prompt does
/// not run into it. A newline is added only when the body lacks one.
fn json_passthrough(body: &str) -> String {
    if body.ends_with('\n') {
        body.to_owned()
    } else {
        format!("{body}\n")
    }
}

/// Write the rendered report, mapping an I/O failure to a CLI error.
fn write_report(out: &mut impl std::io::Write, report: &str) -> CliResult<()> {
    out.write_all(report.as_bytes())
        .and_then(|()| out.flush())
        .map_err(|error| format!("failed to write the result: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unexpected_answer_does_not_quote_response_values() {
        let error = serde_json::from_str::<u64>(r#""uid-fictional-secret""#)
            .expect_err("should fail to parse a string as a number");
        assert!(
            error.to_string().contains("uid-fictional-secret"),
            "precondition: serde_json quotes the value"
        );

        let message = unexpected_answer("an EC lookup response", &error);

        assert!(
            !message.contains("uid-fictional-secret"),
            "should not echo response data, got: {message}"
        );
        assert!(message.contains("line 1"), "should give the position");
    }

    #[test]
    fn json_passthrough_keeps_the_body_and_adds_only_a_missing_newline() {
        assert_eq!(
            json_passthrough("{} "),
            "{} \n",
            "should keep trailing spaces"
        );
        assert_eq!(
            json_passthrough("{}\n"),
            "{}\n",
            "should not double the newline"
        );
    }
}
