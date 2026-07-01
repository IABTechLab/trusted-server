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
// On non-macOS targets `DevCommand` is an empty enum: the by-value parameter is
// consumed by an empty `match`, which clippy reads as a needless by-value pass.
// Taking `&DevCommand` is not an option — a zero-arm `match` is not exhaustive
// over a reference type — so the owned parameter is required.
#[cfg_attr(
    not(target_os = "macos"),
    allow(
        clippy::needless_pass_by_value,
        reason = "empty enum requires owned value for exhaustive match"
    )
)]
pub fn run(command: DevCommand) -> Result<(), String> {
    match command {
        #[cfg(target_os = "macos")]
        DevCommand::Proxy(args) => proxy::run(&args).map_err(|report| format!("{report:?}")),
    }
}
