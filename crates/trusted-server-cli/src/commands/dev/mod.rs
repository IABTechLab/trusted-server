#[cfg(any(target_os = "macos", target_os = "linux"))]
pub mod proxy;

/// The `ts dev …` command group.
#[derive(Debug, clap::Subcommand)]
pub enum DevCommand {
    /// Run the local production-hostname dev proxy (macOS and Linux).
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    Proxy(proxy::ProxyArgs),
}

/// Dispatches a `dev` subcommand.
///
/// # Errors
/// Returns the subcommand's failure rendered as a message. On unsupported targets
/// `DevCommand` has no variants, so this never returns an error there.
// On unsupported targets `DevCommand` is an empty enum: the by-value parameter is
// consumed by an empty `match`, which clippy reads as a needless by-value pass.
// Taking `&DevCommand` is not an option — a zero-arm `match` is not exhaustive
// over a reference type — so the owned parameter is required.
#[cfg_attr(
    not(any(target_os = "macos", target_os = "linux")),
    allow(
        clippy::needless_pass_by_value,
        reason = "empty enum requires owned value for exhaustive match"
    )
)]
pub fn run(command: DevCommand) -> Result<(), String> {
    match command {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        DevCommand::Proxy(args) => proxy::run(&args).map_err(|report| format!("{report:?}")),
    }
}
