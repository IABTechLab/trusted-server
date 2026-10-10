// `ts dev proxy` supports macOS and Linux; its dependencies are scoped to
// those targets in `Cargo.toml`, so the module and the `Proxy` subcommand only
// exist there. `ts dev lint` and `ts dev install-hooks` are pure-Rust
// (gitoxide) linters / installers and are available on every host target.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub mod proxy;
// Needs a real HTTP client; its dependencies are scoped to non-wasm hosts.
#[cfg(not(target_family = "wasm"))]
pub mod sandbox_probe;

pub mod install_hooks;
pub mod lint;

use clap::Args;
use error_stack::Report;

use crate::error::CliError;

/// The `ts dev …` command group.
#[derive(Debug, clap::Subcommand)]
pub enum DevCommand {
    /// Run the local production-hostname dev proxy (macOS and Linux).
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    Proxy(proxy::ProxyArgs),
    /// Measure sandbox reuse from the counters on workload responses.
    #[cfg(not(target_family = "wasm"))]
    SandboxProbe(sandbox_probe::SandboxProbeArgs),
    /// Linters for source, config, and documentation.
    Lint {
        /// The lint to run.
        #[command(subcommand)]
        command: lint::LintCommand,
    },
    /// Install the pre-commit hook into this repo (one-time setup).
    InstallHooks(InstallHooksArgs),
}

/// Arguments for `ts dev install-hooks`.
#[derive(Debug, Args)]
pub struct InstallHooksArgs {
    /// Replace an existing unmanaged `.git/hooks/pre-commit` (it is
    /// backed up next to itself and the backup path is printed).
    #[arg(long)]
    pub force: bool,
}

/// Dispatches a `dev` subcommand.
///
/// # Errors
/// Returns the subcommand's failure rendered as a message. `lint` and
/// `install-hooks` manage their own process exit codes via [`finish`], so an
/// `Err(String)` returned here comes from `proxy` or `sandbox-probe`.
pub fn run(command: DevCommand) -> Result<(), String> {
    match command {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        DevCommand::Proxy(args) => proxy::run(&args).map_err(|report| format!("{report:?}")),
        #[cfg(not(target_family = "wasm"))]
        DevCommand::SandboxProbe(args) => sandbox_probe::run(&args),
        DevCommand::Lint { command } => finish(lint::run(command)),
        DevCommand::InstallHooks(args) => finish(install_hooks::run(&args)),
    }
}

/// Renders a linter / installer result into the `ts dev lint` exit contract.
///
/// `ViolationsFound` exits 1 without an error-stack dump (the violation report
/// is already on stdout); `EnvironmentError` exits 2; any other failure exits 1
/// with a dump on stderr. Success returns `Ok(())` so the caller exits 0.
fn finish(result: Result<(), Report<CliError>>) -> Result<(), String> {
    match result {
        Ok(()) => Ok(()),
        Err(error) => match error.current_context() {
            CliError::ViolationsFound { .. } => std::process::exit(1),
            CliError::EnvironmentError => {
                let _ = crate::output::write_stderr_line(format!("{error:?}"));
                std::process::exit(2)
            }
            _ => {
                let _ = crate::output::write_stderr_line(format!("{error:?}"));
                std::process::exit(1)
            }
        },
    }
}
