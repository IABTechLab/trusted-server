// consumed by Task 7 preparation and Task 9 recipes
#[allow(dead_code)]
mod boundary;
// Atomic output and schema helpers are consumed by later producer stages.
#[allow(dead_code)]
mod documents;
mod error;
mod git_objects;
#[allow(dead_code, reason = "consumed by Task 9 recipes")]
mod inspect;
#[allow(dead_code, reason = "production JS tool qualification remains closed")]
mod javascript;
#[allow(dead_code, reason = "consumed by Task 7 material preparation")]
mod materials;
mod policy;
mod preflight;
#[allow(dead_code, reason = "production JS tool qualification remains closed")]
mod preparation;
// `expect` would be unfulfilled in the test build, where the tests use it.
#[allow(dead_code, reason = "consumed by Task 7 boundary and preparation")]
mod process;
#[allow(dead_code)]
mod source;

use std::ffi::OsString;
use std::io::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use error::ReleaseError;
use error_stack::Report;
use error_stack::fmt::ColorMode;
use source::CommitId;

/// Exit status of `audit-inputs` when any input is incomplete.
const INCOMPLETE_STATUS: u8 = 3;

#[derive(Parser)]
#[command(
    name = "trusted-server-release",
    disable_help_flag = true,
    disable_help_subcommand = true,
    disable_version_flag = true
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Fail-closed release preflight over the current-directory source root.
    Preflight {
        #[arg(long)]
        cohort: PathBuf,
        #[arg(long = "source-commit")]
        source_commit: String,
        #[arg(long = "require-qualified")]
        require_qualified: bool,
    },
    /// Diagnostic report of pending inputs; constructs no authorization.
    AuditInputs,
    /// Network-denial socket probe used by the boundary controls.
    BoundaryProbe {
        #[command(subcommand)]
        probe: ProbeCommand,
    },
}

#[derive(Subcommand)]
enum ProbeCommand {
    /// Attempts one TCP connect to a literal loopback `IP:PORT`, prints
    /// `connected` (exit 0) or `denied errno=<n>` (exit 3), and does nothing
    /// else.
    Connect { address: String },
}

/// Writes one human-readable diagnostic line to stderr.
///
/// The `log` facade is deliberately not used: no logger is installed before
/// the environment audit runs, and configuring one from inherited variables
/// such as `RUST_LOG` would itself be an unaudited input channel. Write
/// failures are ignored because diagnostics never change the outcome.
pub(crate) fn notice(message: std::fmt::Arguments<'_>) {
    let _ = writeln!(std::io::stderr(), "{message}");
}

fn main() -> ExitCode {
    let inherited: Vec<(OsString, OsString)> = std::env::vars_os().collect();
    Report::set_color_mode(ColorMode::None);
    let arguments: Vec<OsString> = std::env::args_os().collect();
    match run(&inherited, &arguments) {
        Ok(status) => status,
        Err(report) => {
            notice(format_args!("{report:?}"));
            ExitCode::FAILURE
        }
    }
}

fn run(
    inherited: &[(OsString, OsString)],
    arguments: &[OsString],
) -> Result<ExitCode, Report<ReleaseError>> {
    let cli = Cli::try_parse_from(arguments).map_err(|error| {
        Report::new(ReleaseError::Usage).attach(format!("invalid invocation: {}", error.kind()))
    })?;
    match cli.command {
        Command::Preflight {
            cohort,
            source_commit,
            require_qualified,
        } => {
            let commit = CommitId::parse(&source_commit)?;
            preflight::run_preflight(inherited, &cohort, &commit, require_qualified)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::AuditInputs => audit_inputs(),
        Command::BoundaryProbe {
            probe: ProbeCommand::Connect { address },
        } => boundary_probe(&address),
    }
}

/// Runs the single-connect probe and reports its real outcome.
fn boundary_probe(address: &str) -> Result<ExitCode, Report<ReleaseError>> {
    let address = boundary::parse_loopback(address)?;
    let report = boundary::connect_probe(address)?;
    std::io::stdout()
        .write_all(format!("{report}\n").as_bytes())
        .map_err(|error| {
            Report::new(ReleaseError::Boundary)
                .attach(format!("cannot write probe report: {error}"))
        })?;
    Ok(match report {
        boundary::ProbeReport::Connected => ExitCode::SUCCESS,
        boundary::ProbeReport::Denied { .. } => ExitCode::from(boundary::DENIED_STATUS),
    })
}

fn audit_inputs() -> Result<ExitCode, Report<ReleaseError>> {
    let root = preflight::source_root()?;
    let inputs = policy::load_inputs(&root)?;
    let audit = policy::audit_inputs(&inputs);
    let bytes = documents::canonical_json(&audit.to_json(), documents::Role::Metadata)?;
    std::io::stdout().write_all(&bytes).map_err(|error| {
        Report::new(ReleaseError::Incomplete).attach(format!("cannot write audit report: {error}"))
    })?;
    if audit.is_complete() {
        return Ok(ExitCode::SUCCESS);
    }
    notice(format_args!(
        "{}: release inputs are incomplete; this diagnostic authorizes nothing",
        ReleaseError::Incomplete
    ));
    Ok(ExitCode::from(INCOMPLETE_STATUS))
}
