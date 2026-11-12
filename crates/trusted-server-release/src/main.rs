//! Ordinary release measurement, equality and Fastly package inspection.

mod documents;
mod error;
mod inspect;
mod ordinary;
mod package;
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "bounded process APIs are exercised by safety tests"
    )
)]
mod process;

use std::ffi::OsString;
use std::io::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use error::ReleaseError;
use error_stack::Report;
use error_stack::fmt::ColorMode;

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
    /// Package the exact Wasm with the fixed service-neutral release manifest.
    PackFastlyPackage {
        #[arg(long)]
        wasm: PathBuf,
        #[arg(long)]
        output: PathBuf,
    },
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
    // Restore waitable children before any thread or child exists; an
    // ignoring launcher would otherwise let the kernel reap supervised leaders.
    match process::establish_waitable_children().and_then(|()| run(&inherited, &arguments)) {
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
        Command::PackFastlyPackage { wasm, output } => {
            package::build_package(&wasm, &output)?;
            Ok(ExitCode::SUCCESS)
        }
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
    }
}
