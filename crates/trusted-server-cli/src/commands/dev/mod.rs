pub mod proxy;

use proxy::{ProxyArgs, ProxyError};

/// The `ts dev …` command group.
#[derive(Debug, clap::Subcommand)]
pub enum DevCommand {
    /// Run the local production-hostname dev proxy.
    Proxy(ProxyArgs),
}

/// Dispatches a `dev` subcommand.
///
/// # Errors
/// Propagates failures from the chosen subcommand.
pub fn run(command: DevCommand) -> Result<(), error_stack::Report<ProxyError>> {
    match command {
        DevCommand::Proxy(args) => proxy::run(args),
    }
}
