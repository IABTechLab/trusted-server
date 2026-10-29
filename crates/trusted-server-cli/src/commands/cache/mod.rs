//! `ts cache` — operator control over shared template and origin caches.

pub mod purge;

use clap::Subcommand;

use crate::error::CliResult;

/// Subcommands under `ts cache`.
#[derive(Debug, Subcommand)]
pub enum CacheCommand {
    /// Purge cached templates and tagged origin responses through a deployed service's admin endpoint.
    Purge(PurgeArgs),
}

/// Arguments for `ts cache purge`.
///
/// # Why this calls the service rather than the Fastly purge API
///
/// The recorded Fastly API token is config- and secret-store write only, with no purge
/// permission (`adapter-fastly/src/management_api.rs`). The service's own admin endpoint
/// purges from inside the running service, where the platform SDK needs no API token at
/// all, so this path works with the credentials an operator already has.
///
/// It also leaves one implementation of the purge. The surrogate key is derived
/// server-side from the URL, so the CLI cannot drift out of agreement with the cache it is
/// purging — a class of bug that a second client-side derivation would reintroduce.
#[derive(Debug, clap::Args)]
pub struct PurgeArgs {
    /// HTTPS base URL of the service; HTTP is permitted only for loopback development.
    #[arg(long)]
    pub service: String,

    /// Purge every cached template and tagged origin response.
    #[arg(long, conflicts_with = "page")]
    pub all: bool,

    /// Purge one reader-facing page URL, including its exact scheme, host, and port.
    #[arg(long, conflicts_with = "all")]
    pub page: Option<String>,

    /// Admin username for the service's `^/_ts/admin` basic auth.
    #[arg(long, default_value = "admin")]
    pub username: String,
}

/// Environment variable carrying the admin password.
///
/// Read from the environment and never accepted as a flag: an argument is visible to every
/// other process on the host through `ps`, and lands in shell history.
pub const ADMIN_PASSWORD_ENVIRONMENT_VARIABLE: &str = "TRUSTED_SERVER_ADMIN_PASSWORD";

/// Run a `ts cache` subcommand.
///
/// # Errors
///
/// Returns an error when neither scope is given, when the admin password is absent from
/// the environment, when the service cannot be reached, or when it refuses the purge.
pub fn run(command: CacheCommand, out: &mut impl std::io::Write) -> CliResult<()> {
    match command {
        CacheCommand::Purge(args) => purge::run_purge(&args, out),
    }
}
