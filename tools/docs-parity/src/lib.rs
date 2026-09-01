//! Checked documentation records and repository-safe generation.

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
compile_error!("docs-parity supports only Linux and macOS hosts");

use std::env;
use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use error_stack::{Report, ResultExt as _};

use crate::repository::{NormalizedRelativePath, Repository};

pub mod classification;
pub mod model;
mod repository;
pub mod scanner;

/// Process exit code for a successful check or update.
pub const EXIT_SUCCESS: i32 = 0;

/// Process exit code for generated-record drift.
pub const EXIT_DRIFT: i32 = 1;

/// Process exit code for invalid input or an operational failure.
pub const EXIT_ERROR: i32 = 2;

#[derive(Debug, derive_more::Display)]
pub enum DocsParityError {
    #[display("cannot access the repository")]
    Repository,
    #[display("cannot read the current directory")]
    CurrentDirectory,
    #[display("documentation source classification failed")]
    Classification,
    #[display("documentation sensitive-data scan failed")]
    Scanner,
}

impl core::error::Error for DocsParityError {}

#[derive(Debug, Parser)]
#[command(
    name = "docs-parity",
    version,
    about = "Check and update deterministic documentation records"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Check repository safety and generated tracked-path records without writing.
    Check(CheckArguments),
    /// Atomically update a generated tracked-path record.
    Update(UpdateArguments),
    /// Check or update the closed tracked-file classification universe.
    Classify(ClassifyArguments),
    /// Check sensitive data or bootstrap exact review candidates.
    Scan(ScanArguments),
}

#[derive(Args, Debug)]
struct ClassifyArguments {
    /// Validate manifests without changing repository bytes.
    #[arg(long, conflicts_with = "update", required_unless_present = "update")]
    check: bool,
    /// Bootstrap or refresh deterministic candidate manifests.
    #[arg(long, conflicts_with = "check", required_unless_present = "check")]
    update: bool,
}

#[derive(Args, Debug)]
struct ScanArguments {
    /// Validate all tracked files without changing repository bytes.
    #[arg(
        long,
        conflicts_with = "bootstrap",
        required_unless_present = "bootstrap"
    )]
    check: bool,
    /// Replace the allowlist with deterministic, unreviewed finding candidates.
    #[arg(long, conflicts_with = "check", required_unless_present = "check")]
    bootstrap: bool,
}

#[derive(Args, Debug)]
struct CheckArguments {
    /// Repository-relative generated record to compare.
    #[arg(long)]
    tracked_paths_record: Option<PathBuf>,
}

#[derive(Args, Debug)]
struct UpdateArguments {
    /// Repository-relative generated record to replace atomically.
    #[arg(long)]
    tracked_paths_record: PathBuf,
}

/// Result of a documentation parity invocation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Outcome {
    /// All requested checks passed.
    Clean,
    /// A generated record differs from its deterministic source.
    Drift,
    /// The requested record was updated successfully.
    Updated,
}

impl Outcome {
    /// Return the stable process exit code for this outcome.
    #[must_use]
    pub const fn exit_code(self) -> i32 {
        match self {
            Self::Clean | Self::Updated => EXIT_SUCCESS,
            Self::Drift => EXIT_DRIFT,
        }
    }
}

/// Parse process arguments and run the selected subcommand.
///
/// Clap handles help, version, and invalid command lines before this function
/// returns. Repository failures are returned as [`Report<DocsParityError>`].
///
/// # Errors
///
/// Returns an error when the current directory or repository cannot be read,
/// when a path crosses the repository boundary, or when an update cannot be
/// committed atomically.
pub fn run_from_env() -> Result<Outcome, Report<DocsParityError>> {
    let cli = Cli::parse();
    let current_directory = env::current_dir().change_context(DocsParityError::CurrentDirectory)?;
    let repository =
        Repository::discover(&current_directory).change_context(DocsParityError::Repository)?;

    match cli.command {
        Command::Check(arguments) => check(&repository, arguments),
        Command::Update(arguments) => update(&repository, &arguments),
        Command::Classify(arguments) => classify(&repository, &arguments),
        Command::Scan(arguments) => scan(&repository, &arguments),
    }
}

fn scan(
    repository: &Repository,
    arguments: &ScanArguments,
) -> Result<Outcome, Report<DocsParityError>> {
    if arguments.check {
        scanner::check(repository).change_context(DocsParityError::Scanner)?;
        Ok(Outcome::Clean)
    } else {
        debug_assert!(arguments.bootstrap, "clap should require one scanner mode");
        scanner::bootstrap(repository).change_context(DocsParityError::Scanner)?;
        Ok(Outcome::Updated)
    }
}

fn classify(
    repository: &Repository,
    arguments: &ClassifyArguments,
) -> Result<Outcome, Report<DocsParityError>> {
    if arguments.check {
        classification::check(repository).change_context(DocsParityError::Classification)?;
        Ok(Outcome::Clean)
    } else {
        debug_assert!(
            arguments.update,
            "clap should require one classification mode"
        );
        classification::update(repository).change_context(DocsParityError::Classification)?;
        Ok(Outcome::Updated)
    }
}

fn check(
    repository: &Repository,
    arguments: CheckArguments,
) -> Result<Outcome, Report<DocsParityError>> {
    let expected = repository
        .tracked_paths_record()
        .change_context(DocsParityError::Repository)?;

    let Some(record) = arguments.tracked_paths_record else {
        return Ok(Outcome::Clean);
    };
    let record =
        NormalizedRelativePath::new(&record).change_context(DocsParityError::Repository)?;
    let actual = repository
        .read_optional(&record)
        .change_context(DocsParityError::Repository)?;

    if actual.as_deref() == Some(expected.as_slice()) {
        Ok(Outcome::Clean)
    } else {
        Ok(Outcome::Drift)
    }
}

fn update(
    repository: &Repository,
    arguments: &UpdateArguments,
) -> Result<Outcome, Report<DocsParityError>> {
    let record = NormalizedRelativePath::new(&arguments.tracked_paths_record)
        .change_context(DocsParityError::Repository)?;
    let expected = repository
        .tracked_paths_record()
        .change_context(DocsParityError::Repository)?;
    repository
        .write_atomically(&record, &expected)
        .change_context(DocsParityError::Repository)?;
    Ok(Outcome::Updated)
}
