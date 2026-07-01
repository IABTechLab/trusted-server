// `ts dev proxy` is macOS-only; its dependencies are scoped to macOS in
// `Cargo.toml`, so the module and the `Proxy` subcommand only exist there. On
// other host targets `ts dev` parses but exposes no subcommands.
#[cfg(target_os = "macos")]
pub mod proxy;

/// The `ts dev …` command group.
#[derive(Debug, clap::Subcommand)]
pub enum DevCommand {
    /// Run the local production-hostname dev proxy (macOS only).
    #[cfg(target_os = "macos")]
    Proxy(proxy::ProxyArgs),
}

/// Dispatches a `dev` subcommand.
///
/// # Errors
/// Returns the subcommand's failure rendered as a message. On non-macOS targets
/// `DevCommand` has no variants, so this never returns an error there.
pub fn run(command: DevCommand) -> Result<(), String> {
    match command {
        #[cfg(target_os = "macos")]
        DevCommand::Proxy(args) => proxy::run(&args).map_err(|report| format!("{report:?}")),
    }
}
