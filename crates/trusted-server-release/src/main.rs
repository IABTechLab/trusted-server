#[allow(dead_code, reason = "candidate source port has no production caller")]
mod collection;
#[allow(dead_code, reason = "candidate source port has no production caller")]
mod distribution;
#[allow(dead_code, reason = "candidate source port has no production caller")]
mod spdx;
// consumed by Task 7 preparation and Task 9 recipes
#[allow(dead_code)]
mod boundary;
#[allow(dead_code, reason = "Task 10 composition capture is being integrated")]
mod composition;
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
mod ordinary;
mod package;
mod policy;
mod preflight;
#[allow(dead_code, reason = "production JS tool qualification remains closed")]
mod preparation;
#[allow(
    dead_code,
    reason = "Task 9 slice 1 recipe layer has no production caller yet"
)]
mod recipe;
// `expect` would be unfulfilled in the test build, where the tests use it.
#[allow(dead_code, reason = "consumed by Task 7 boundary and preparation")]
mod process;
#[allow(dead_code)]
mod source;
mod tool_tree;

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
    /// Seal fixed target-scoped remapping arguments before an ordinary build.
    OrdinaryRustflags {
        #[arg(long)]
        checkout: PathBuf,
        #[arg(long)]
        target: PathBuf,
        #[arg(long)]
        role: String,
        #[arg(long)]
        output: PathBuf,
    },
    /// Record one actual ordinary Fastly build; grants no recipe qualification.
    OrdinaryMeasure {
        #[arg(long)]
        request: PathBuf,
        #[arg(long)]
        output: PathBuf,
    },
    /// Validate current-run upload bindings before downloading exact artifact IDs.
    OrdinaryTransferPlan {
        #[arg(long)]
        input: PathBuf,
    },
    /// Compare the four bound archives and seal only actual ordinary equality.
    OrdinaryCompare {
        #[arg(long)]
        input: PathBuf,
        #[arg(long)]
        archives: PathBuf,
        #[arg(long)]
        source: PathBuf,
        #[arg(long)]
        output: PathBuf,
    },
    /// Inspect a retained manual Fastly package against independently approved digests.
    InspectFastlyPackage {
        #[arg(long)]
        package_dir: PathBuf,
        #[arg(long)]
        manifest: PathBuf,
        #[arg(long)]
        expected_manifest_sha256: String,
        #[arg(long)]
        expected_wasm_sha256: String,
    },
    /// Private fixed in-image GNU tool measurement; supplies no authority.
    #[command(hide = true)]
    GnuImageMeasurement,
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
        Command::OrdinaryRustflags {
            checkout,
            target,
            role,
            output,
        } => {
            let argument =
                ordinary::prepare_rustflags(&checkout, &target, &role, &output, inherited)?;
            writeln!(std::io::stdout(), "{argument}").map_err(|_| {
                Report::new(ReleaseError::Document).attach("cannot write ordinary Cargo argument")
            })?;
            Ok(ExitCode::SUCCESS)
        }
        Command::OrdinaryMeasure { request, output } => {
            ordinary::measure_build(&request, &output)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::OrdinaryTransferPlan { input } => {
            let value = ordinary::transfer_plan(&input)?;
            let bytes = documents::canonical_json(&value, documents::Role::Metadata)?;
            std::io::stdout().write_all(&bytes).map_err(|_| {
                Report::new(ReleaseError::Document).attach("cannot write ordinary transfer plan")
            })?;
            Ok(ExitCode::SUCCESS)
        }
        Command::OrdinaryCompare {
            input,
            archives,
            source,
            output,
        } => {
            ordinary::compare_transfers(&input, &archives, &source, &output)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::InspectFastlyPackage {
            package_dir,
            manifest,
            expected_manifest_sha256,
            expected_wasm_sha256,
        } => {
            let report = package::inspect_package(
                &package_dir,
                &manifest,
                &expected_manifest_sha256,
                &expected_wasm_sha256,
            )?;
            let bytes = documents::canonical_json(&report, documents::Role::Metadata)?;
            std::io::stdout().write_all(&bytes).map_err(|error| {
                Report::new(ReleaseError::Inspect)
                    .attach(format!("cannot write package inspection: {error}"))
            })?;
            Ok(ExitCode::SUCCESS)
        }
        Command::GnuImageMeasurement => {
            let bytes = recipe::gnu::helper_measurement()?;
            std::io::stdout().write_all(&bytes).map_err(|error| {
                Report::new(ReleaseError::Recipe)
                    .attach(format!("cannot write GNU measurement: {error}"))
            })?;
            Ok(ExitCode::SUCCESS)
        }
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
