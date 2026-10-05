// `ts dev proxy` is macOS-only; its dependencies are scoped to macOS in
// `Cargo.toml`, so the module and the `Proxy` subcommand only exist there. The
// `ts dev audit` group has no OS-specific dependencies and is available on
// every host target.
pub mod audit;
#[cfg(target_os = "macos")]
pub mod proxy;

/// The `ts dev …` command group.
#[derive(Debug, clap::Subcommand)]
pub enum DevCommand {
    /// Audit origin responses (e.g. cache headers per content type).
    #[command(subcommand)]
    Audit(audit::DevAuditCommand),
    /// Run the local production-hostname dev proxy (macOS only).
    #[cfg(target_os = "macos")]
    Proxy(proxy::ProxyArgs),
}

/// Dispatches a `dev` subcommand.
///
/// # Errors
/// Returns the subcommand's failure rendered as a message.
pub fn run(command: DevCommand) -> Result<(), String> {
    match command {
        DevCommand::Audit(command) => audit::run(&command),
        #[cfg(target_os = "macos")]
        DevCommand::Proxy(args) => proxy::run(&args).map_err(|report| format!("{report:?}")),
    }
}
