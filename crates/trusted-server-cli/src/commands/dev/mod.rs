#[cfg(any(target_os = "macos", target_os = "linux"))]
pub mod proxy;
// Needs a real HTTP client; its dependencies are scoped to non-wasm hosts.
#[cfg(not(target_family = "wasm"))]
pub mod sandbox_probe;

/// The `ts dev …` command group.
#[derive(Debug, clap::Subcommand)]
pub enum DevCommand {
    /// Run the local production-hostname dev proxy (macOS and Linux).
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    Proxy(proxy::ProxyArgs),
    /// Measure sandbox reuse from the counters on workload responses.
    #[cfg(not(target_family = "wasm"))]
    SandboxProbe(sandbox_probe::SandboxProbeArgs),
}

/// Dispatches a `dev` subcommand.
///
/// # Errors
/// Returns the subcommand's failure rendered as a message.
pub fn run(command: DevCommand) -> Result<(), String> {
    match command {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        DevCommand::Proxy(args) => proxy::run(&args).map_err(|report| format!("{report:?}")),
        #[cfg(not(target_family = "wasm"))]
        DevCommand::SandboxProbe(args) => sandbox_probe::run(&args),
    }
}
