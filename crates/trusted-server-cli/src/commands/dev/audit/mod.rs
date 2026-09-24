//! The `ts dev audit …` command group.
//!
//! Groups origin-audit subcommands under `ts dev audit`. Today the only member
//! is `headers`, which audits cache-header posture per content type. Unlike
//! `ts dev proxy` (macOS-only), audit subcommands are available on all host
//! platforms — they have no OS-specific dependencies.

mod headers;

use std::process;

pub use headers::AuditHeadersArgs;

/// Subcommands of `ts dev audit`.
#[derive(Debug, clap::Subcommand)]
pub enum DevAuditCommand {
    /// Audit origin cache-related response headers per content type.
    Headers(AuditHeadersArgs),
}

/// Dispatches a `dev audit` subcommand.
///
/// # Errors
///
/// Returns the subcommand's failure message when fetching, config resolution,
/// or output rendering fails.
pub fn run(command: &DevAuditCommand) -> Result<(), String> {
    match command {
        DevAuditCommand::Headers(args) => {
            let exit_code = headers::run(args)?;
            // 0 means every content-type group passed. Non-zero audit outcomes
            // (1 = a group failed, 3 = warnings only) are surfaced as process
            // exit codes for CI gating. 3 is used for warnings rather than 2
            // because `main` already exits 2 for any CLI error.
            if exit_code == 0 {
                Ok(())
            } else {
                process::exit(exit_code)
            }
        }
    }
}
