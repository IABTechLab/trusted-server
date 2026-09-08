//! Checked documentation records and repository-safe generation.

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
compile_error!("docs-parity supports only Linux and macOS hosts");

use std::env;
use std::fs;
use std::io::Write as _;
use std::path::Path;
use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use error_stack::{Report, ResultExt as _};

use crate::repository::{NormalizedRelativePath, Repository, read_bounded_regular_file};

pub mod classification;
pub mod cli_help;
pub mod dependency_snapshot;
pub mod gates;
pub mod integrations;
pub mod markdown;
pub mod model;
mod repository;
pub mod routes;
pub mod scanner;
pub mod settings;
pub mod snippets;
pub mod workflow;

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
    #[display("documentation generation or link validation failed")]
    Markdown,
    #[display("configuration documentation semantics failed")]
    Settings,
    #[display("integration documentation semantics failed")]
    Integrations,
    #[display("adapter route documentation semantics failed")]
    Routes,
    #[display("CLI help documentation semantics failed")]
    CliHelp,
    #[display("documentation snippet semantics failed")]
    Snippets,
    #[display("documentation workflow policy failed")]
    Workflow,
    #[display("dependency snapshot semantics failed")]
    DependencySnapshot,
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
    /// Check or atomically update named generated Markdown regions.
    Generate(GenerateArguments),
    /// Check local or explicitly requested external Markdown links.
    Links(LinksArguments),
    /// Check the public page, navigation, orphan, and diagram inventories.
    Pages(PagesArguments),
    /// Check extracted settings semantics and the source-template contract.
    Settings(SettingsArguments),
    /// Check integration inventories and behavioral receipt domains.
    Integrations(IntegrationsArguments),
    /// Check adapter route and support inventories.
    Routes(RoutesArguments),
    /// Capture, import, or check recursive native CLI help.
    CliHelp(CliHelpArguments),
    /// Check classified Markdown snippets in isolated modes.
    Snippets(SnippetsArguments),
    /// Check the currently activated repository workflow policy.
    Workflow(WorkflowArguments),
    /// Generate or validate a bounded dependency snapshot artifact.
    DependencySnapshot(DependencySnapshotArguments),
}

#[derive(Args, Debug)]
#[command(args_conflicts_with_subcommands = true, subcommand_negates_reqs = true)]
struct CliHelpArguments {
    /// Validate imported native goldens without network access.
    #[arg(long, required = true)]
    check: bool,
    #[command(subcommand)]
    action: Option<CliHelpAction>,
}

#[derive(Debug, Subcommand)]
enum CliHelpAction {
    /// Build the native `ts` binary and capture its recursive help tree.
    Capture {
        /// Destination for the deterministic inner ZIP.
        #[arg(long)]
        output: PathBuf,
    },
    /// Authenticate and import a successful PR #1049 hosted capture run.
    ImportHosted {
        /// GitHub Actions run ID containing both platform artifacts.
        #[arg(long)]
        run_id: u64,
    },
}

#[derive(Args, Debug)]
struct SnippetsArguments {
    /// Validate all classified fences without changing repository bytes.
    #[arg(long, required = true)]
    check: bool,
}

#[derive(Args, Debug)]
struct PagesArguments {
    /// Validate publication inventories without changing repository bytes.
    #[arg(long, required = true)]
    check: bool,
}

#[derive(Args, Debug)]
struct WorkflowArguments {
    /// Validate the currently activated capture workflow policy.
    #[arg(long, required = true)]
    check: bool,
}

#[derive(Args, Debug)]
struct DependencySnapshotArguments {
    #[command(subcommand)]
    action: DependencySnapshotAction,
}

#[derive(Debug, Subcommand)]
enum DependencySnapshotAction {
    /// Generate a bounded dependency-submission inner ZIP.
    Generate {
        /// Destination archive path.
        #[arg(long)]
        output: PathBuf,
    },
    /// Validate an existing dependency-snapshot inner ZIP.
    Validate {
        /// Archive path to validate.
        #[arg(long)]
        archive: PathBuf,
    },
}

#[derive(Args, Debug)]
struct SettingsArguments {
    /// Validate settings records without changing repository bytes.
    #[arg(long, required = true)]
    check: bool,
}

#[derive(Args, Debug)]
struct IntegrationsArguments {
    /// Validate integration records without changing repository bytes.
    #[arg(long, required = true)]
    check: bool,
}

#[derive(Args, Debug)]
struct RoutesArguments {
    /// Validate route records without changing repository bytes.
    #[arg(long, required = true)]
    check: bool,
}

#[derive(Args, Debug)]
struct GenerateArguments {
    /// Validate generated bytes without changing repository files.
    #[arg(long, conflicts_with = "update", required_unless_present = "update")]
    check: bool,
    /// Atomically replace drifted generated region bodies.
    #[arg(long, conflicts_with = "check", required_unless_present = "check")]
    update: bool,
}

#[derive(Args, Debug)]
struct LinksArguments {
    /// Validate relative files, routes, anchors, and publication inventories.
    #[arg(
        long,
        conflicts_with = "external",
        required_unless_present = "external"
    )]
    local: bool,
    /// Perform the scheduled/manual bounded external network check.
    #[arg(long, conflicts_with = "local", required_unless_present = "local")]
    external: bool,
    /// Validate without changing repository bytes.
    #[arg(
        long,
        conflicts_with = "artifact",
        required_unless_present = "artifact"
    )]
    check: bool,
    /// Write a complete deterministic result artifact and exit cleanly on findings.
    #[arg(long, conflicts_with_all = ["check", "local"], requires = "external")]
    artifact: Option<PathBuf>,
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
    /// Run the explicit deterministic offline check registry.
    #[arg(long, conflicts_with = "tracked_paths_record")]
    all: bool,
    /// Repository-relative generated record to compare.
    #[arg(long, conflicts_with = "all")]
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
        Command::Generate(arguments) => generate(&repository, &arguments),
        Command::Links(arguments) => links(&repository, &arguments),
        Command::Pages(arguments) => pages(&repository, &arguments),
        Command::Settings(arguments) => settings(&repository, &arguments),
        Command::Integrations(arguments) => integrations(&repository, &arguments),
        Command::Routes(arguments) => routes(&repository, &arguments),
        Command::CliHelp(arguments) => cli_help(&repository, &arguments),
        Command::Snippets(arguments) => snippets(&repository, &arguments),
        Command::Workflow(arguments) => workflow(&repository, &arguments),
        Command::DependencySnapshot(arguments) => dependency_snapshot(&repository, &arguments),
    }
}

fn pages(
    repository: &Repository,
    arguments: &PagesArguments,
) -> Result<Outcome, Report<DocsParityError>> {
    debug_assert!(arguments.check, "clap should require page check mode");
    markdown::check_local_repository(repository).change_context(DocsParityError::Markdown)?;
    Ok(Outcome::Clean)
}

fn cli_help(
    repository: &Repository,
    arguments: &CliHelpArguments,
) -> Result<Outcome, Report<DocsParityError>> {
    if arguments.check {
        cli_help::check_repository(repository).change_context(DocsParityError::CliHelp)?;
    } else {
        match arguments
            .action
            .as_ref()
            .expect("clap should require a CLI-help mode")
        {
            CliHelpAction::Capture { output } => {
                cli_help::capture_repository(repository, output)
                    .change_context(DocsParityError::CliHelp)?;
            }
            CliHelpAction::ImportHosted { run_id } => {
                cli_help::import_hosted_repository(repository, *run_id)
                    .change_context(DocsParityError::CliHelp)?;
            }
        }
    }
    Ok(Outcome::Clean)
}

fn snippets(
    repository: &Repository,
    arguments: &SnippetsArguments,
) -> Result<Outcome, Report<DocsParityError>> {
    debug_assert!(arguments.check, "clap should require snippet check mode");
    snippets::check_repository(repository).change_context(DocsParityError::Snippets)?;
    Ok(Outcome::Clean)
}

fn workflow(
    repository: &Repository,
    arguments: &WorkflowArguments,
) -> Result<Outcome, Report<DocsParityError>> {
    debug_assert!(arguments.check, "clap should require workflow check mode");
    workflow::check_capture_repository(repository).change_context(DocsParityError::Workflow)?;
    Ok(Outcome::Clean)
}

fn dependency_snapshot(
    repository: &Repository,
    arguments: &DependencySnapshotArguments,
) -> Result<Outcome, Report<DocsParityError>> {
    let context = dependency_context().change_context(DocsParityError::DependencySnapshot)?;
    match &arguments.action {
        DependencySnapshotAction::Generate { output } => {
            let bytes = dependency_snapshot::generate_repository_archive(repository, &context)
                .change_context(DocsParityError::DependencySnapshot)?;
            write_output_atomically(output, &bytes)
                .change_context(DocsParityError::DependencySnapshot)?;
        }
        DependencySnapshotAction::Validate { archive } => {
            let bytes = read_bounded_regular_file(archive, 4 * 1024 * 1024)
                .change_context(DocsParityError::DependencySnapshot)
                .attach("dependency snapshot path is not a stable bounded regular file")?;
            dependency_snapshot::validate_archive(&bytes, &context)
                .change_context(DocsParityError::DependencySnapshot)?;
        }
    }
    Ok(Outcome::Clean)
}

fn integrations(
    repository: &Repository,
    arguments: &IntegrationsArguments,
) -> Result<Outcome, Report<DocsParityError>> {
    debug_assert!(
        arguments.check,
        "clap should require integration check mode"
    );
    integrations::check_repository(repository).change_context(DocsParityError::Integrations)?;
    Ok(Outcome::Clean)
}

fn routes(
    repository: &Repository,
    arguments: &RoutesArguments,
) -> Result<Outcome, Report<DocsParityError>> {
    debug_assert!(arguments.check, "clap should require route check mode");
    routes::check_repository(repository).change_context(DocsParityError::Routes)?;
    Ok(Outcome::Clean)
}

fn settings(
    repository: &Repository,
    arguments: &SettingsArguments,
) -> Result<Outcome, Report<DocsParityError>> {
    debug_assert!(arguments.check, "clap should require settings check mode");
    settings::check_repository(repository).change_context(DocsParityError::Settings)?;
    Ok(Outcome::Clean)
}

fn links(
    repository: &Repository,
    arguments: &LinksArguments,
) -> Result<Outcome, Report<DocsParityError>> {
    if arguments.local {
        debug_assert!(arguments.check, "local links require check mode");
        markdown::check_local_repository(repository).change_context(DocsParityError::Markdown)?;
    } else if let Some(artifact) = &arguments.artifact {
        let context = link_context()?;
        let result = markdown::collect_external_repository(repository, &context)
            .change_context(DocsParityError::Markdown)?;
        let bytes = markdown::encode_link_results_archive(&result)
            .change_context(DocsParityError::Markdown)?;
        write_output_atomically(artifact, &bytes)?;
    } else {
        debug_assert!(
            arguments.check,
            "interactive external links require check mode"
        );
        debug_assert!(
            arguments.external,
            "clap should require one link-check scope"
        );
        markdown::check_external_repository(repository)
            .change_context(DocsParityError::Markdown)?;
    }
    Ok(Outcome::Clean)
}

fn generate(
    repository: &Repository,
    arguments: &GenerateArguments,
) -> Result<Outcome, Report<DocsParityError>> {
    let drift = markdown::generate(repository, arguments.update)
        .change_context(DocsParityError::Markdown)?;
    if arguments.check && drift {
        Ok(Outcome::Drift)
    } else {
        Ok(if arguments.update {
            Outcome::Updated
        } else {
            Outcome::Clean
        })
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
    if arguments.all {
        return check_all(repository);
    }
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

type OfflineCheckFunction = fn(&Repository) -> Result<bool, Report<DocsParityError>>;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum OfflineCheckKind {
    Classification,
    CliHelp,
    Generated,
    Integrations,
    LocalLinks,
    Routes,
    Scanner,
    Settings,
    Snippets,
    WorkflowCapture,
}

impl OfflineCheckKind {
    const fn name(self) -> &'static str {
        match self {
            Self::Classification => "classification",
            Self::CliHelp => "cli-help",
            Self::Generated => "generated",
            Self::Integrations => "integrations",
            Self::LocalLinks => "local-links",
            Self::Routes => "routes",
            Self::Scanner => "scanner",
            Self::Settings => "settings",
            Self::Snippets => "snippets",
            Self::WorkflowCapture => "workflow-capture",
        }
    }
}

#[derive(Clone, Copy)]
struct OfflineCheckEntry {
    kind: OfflineCheckKind,
    run: OfflineCheckFunction,
}

const OFFLINE_CHECKS: [OfflineCheckEntry; 10] = [
    OfflineCheckEntry {
        kind: OfflineCheckKind::Classification,
        run: offline_classification,
    },
    OfflineCheckEntry {
        kind: OfflineCheckKind::CliHelp,
        run: offline_cli_help,
    },
    OfflineCheckEntry {
        kind: OfflineCheckKind::Generated,
        run: offline_generated,
    },
    OfflineCheckEntry {
        kind: OfflineCheckKind::Integrations,
        run: offline_integrations,
    },
    OfflineCheckEntry {
        kind: OfflineCheckKind::LocalLinks,
        run: offline_local_links,
    },
    OfflineCheckEntry {
        kind: OfflineCheckKind::Routes,
        run: offline_routes,
    },
    OfflineCheckEntry {
        kind: OfflineCheckKind::Scanner,
        run: offline_scanner,
    },
    OfflineCheckEntry {
        kind: OfflineCheckKind::Settings,
        run: offline_settings,
    },
    OfflineCheckEntry {
        kind: OfflineCheckKind::Snippets,
        run: offline_snippets,
    },
    OfflineCheckEntry {
        kind: OfflineCheckKind::WorkflowCapture,
        run: offline_workflow_capture,
    },
];

#[cfg(test)]
fn offline_check_names() -> Vec<&'static str> {
    OFFLINE_CHECKS
        .iter()
        .map(|entry| entry.kind.name())
        .collect()
}

fn check_all(repository: &Repository) -> Result<Outcome, Report<DocsParityError>> {
    run_offline_registry(|entry| (entry.run)(repository))
}

fn run_offline_registry<F>(mut execute: F) -> Result<Outcome, Report<DocsParityError>>
where
    F: FnMut(&OfflineCheckEntry) -> Result<bool, Report<DocsParityError>>,
{
    let mut drift = false;
    for entry in &OFFLINE_CHECKS {
        drift |= execute(entry).attach_with(|| format!("offline check: {}", entry.kind.name()))?;
    }
    Ok(if drift {
        Outcome::Drift
    } else {
        Outcome::Clean
    })
}

fn offline_classification(repository: &Repository) -> Result<bool, Report<DocsParityError>> {
    classification::check(repository).change_context(DocsParityError::Classification)?;
    Ok(false)
}

fn offline_cli_help(repository: &Repository) -> Result<bool, Report<DocsParityError>> {
    cli_help::check_repository_if_materialized(repository)
        .change_context(DocsParityError::CliHelp)?;
    Ok(false)
}

fn offline_generated(repository: &Repository) -> Result<bool, Report<DocsParityError>> {
    markdown::generate(repository, false).change_context(DocsParityError::Markdown)
}

fn offline_integrations(repository: &Repository) -> Result<bool, Report<DocsParityError>> {
    integrations::check_repository(repository).change_context(DocsParityError::Integrations)?;
    Ok(false)
}

fn offline_local_links(repository: &Repository) -> Result<bool, Report<DocsParityError>> {
    markdown::check_local_repository(repository).change_context(DocsParityError::Markdown)?;
    Ok(false)
}

fn offline_routes(repository: &Repository) -> Result<bool, Report<DocsParityError>> {
    routes::check_repository(repository).change_context(DocsParityError::Routes)?;
    Ok(false)
}

fn offline_scanner(repository: &Repository) -> Result<bool, Report<DocsParityError>> {
    scanner::check(repository).change_context(DocsParityError::Scanner)?;
    Ok(false)
}

fn offline_settings(repository: &Repository) -> Result<bool, Report<DocsParityError>> {
    settings::check_repository(repository).change_context(DocsParityError::Settings)?;
    Ok(false)
}

fn offline_snippets(repository: &Repository) -> Result<bool, Report<DocsParityError>> {
    snippets::check_repository(repository).change_context(DocsParityError::Snippets)?;
    Ok(false)
}

fn offline_workflow_capture(repository: &Repository) -> Result<bool, Report<DocsParityError>> {
    workflow::check_capture_repository(repository).change_context(DocsParityError::Workflow)?;
    Ok(false)
}

fn update(
    repository: &Repository,
    arguments: &UpdateArguments,
) -> Result<Outcome, Report<DocsParityError>> {
    let record = NormalizedRelativePath::new(&arguments.tracked_paths_record)
        .change_context(DocsParityError::Repository)?;
    let original = repository
        .read_optional(&record)
        .change_context(DocsParityError::Repository)?;
    let expected = repository
        .tracked_paths_record()
        .change_context(DocsParityError::Repository)?;
    repository
        .replace_atomically_after_precommit_validation(&record, original.as_deref(), &expected)
        .change_context(DocsParityError::Repository)?;
    Ok(Outcome::Updated)
}

fn dependency_context()
-> Result<dependency_snapshot::DependencySnapshotContext, Report<DocsParityError>> {
    Ok(dependency_snapshot::DependencySnapshotContext {
        repository: required_environment("GITHUB_REPOSITORY")?,
        source_sha: required_environment("GITHUB_SHA")?,
        source_ref: required_environment("GITHUB_REF")?,
        run_id: required_u64_environment("GITHUB_RUN_ID")?,
        run_attempt: required_u64_environment("GITHUB_RUN_ATTEMPT")?,
    })
}

fn link_context() -> Result<markdown::LinkRunContext, Report<DocsParityError>> {
    Ok(markdown::LinkRunContext {
        repository: required_environment("GITHUB_REPOSITORY")?,
        source_ref: required_environment("GITHUB_REF")?,
        source_sha: required_environment("GITHUB_SHA")?,
        run_id: required_u64_environment("GITHUB_RUN_ID")?,
        run_attempt: required_u64_environment("GITHUB_RUN_ATTEMPT")?,
        checked_at: required_environment("DOCS_PARITY_CHECKED_AT")?,
    })
}

fn required_environment(name: &str) -> Result<String, Report<DocsParityError>> {
    let value = env::var(name)
        .change_context(DocsParityError::CurrentDirectory)
        .attach_with(|| format!("missing environment variable: {name}"))?;
    if value.trim().is_empty() || value.len() > 8 * 1024 || value.contains('\0') {
        return Err(Report::new(DocsParityError::CurrentDirectory)
            .attach(format!("invalid environment variable: {name}")));
    }
    Ok(value)
}

fn required_u64_environment(name: &str) -> Result<u64, Report<DocsParityError>> {
    required_environment(name)?
        .parse()
        .change_context(DocsParityError::CurrentDirectory)
        .attach_with(|| format!("environment variable is not an unsigned integer: {name}"))
}

fn write_output_atomically(path: &Path, bytes: &[u8]) -> Result<(), Report<DocsParityError>> {
    let parent = path.parent().ok_or_else(|| {
        Report::new(DocsParityError::Repository).attach("output path has no parent")
    })?;
    fs::create_dir_all(parent).change_context(DocsParityError::Repository)?;
    if path
        .symlink_metadata()
        .is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        return Err(
            Report::new(DocsParityError::Repository).attach("output path cannot be a symlink")
        );
    }
    let mut stage = tempfile::Builder::new()
        .prefix(".docs-parity-output-")
        .tempfile_in(parent)
        .change_context(DocsParityError::Repository)?;
    stage
        .write_all(bytes)
        .change_context(DocsParityError::Repository)?;
    stage
        .as_file()
        .sync_all()
        .change_context(DocsParityError::Repository)?;
    stage
        .persist(path)
        .change_context(DocsParityError::Repository)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn offline_registry_is_the_single_exact_execution_inventory() {
        let expected_kinds = [
            OfflineCheckKind::Classification,
            OfflineCheckKind::CliHelp,
            OfflineCheckKind::Generated,
            OfflineCheckKind::Integrations,
            OfflineCheckKind::LocalLinks,
            OfflineCheckKind::Routes,
            OfflineCheckKind::Scanner,
            OfflineCheckKind::Settings,
            OfflineCheckKind::Snippets,
            OfflineCheckKind::WorkflowCapture,
        ];
        let registered_kinds = OFFLINE_CHECKS
            .iter()
            .map(|entry| entry.kind)
            .collect::<BTreeSet<_>>();
        assert_eq!(
            registered_kinds,
            expected_kinds.into_iter().collect(),
            "the compiled registry must equal the materialized repository-check set"
        );
        let names = offline_check_names();
        assert_eq!(
            names,
            [
                "classification",
                "cli-help",
                "generated",
                "integrations",
                "local-links",
                "routes",
                "scanner",
                "settings",
                "snippets",
                "workflow-capture",
            ]
        );
        assert_eq!(
            names.iter().copied().collect::<BTreeSet<_>>().len(),
            OFFLINE_CHECKS.len(),
            "every registered execution must have one unique stable name"
        );
        assert!(
            OFFLINE_CHECKS.iter().all(|entry| entry.run as usize != 0),
            "every registry entry must carry an executable check"
        );
    }

    #[test]
    fn offline_registry_executes_every_check_and_propagates_drift_and_errors() {
        let mut visited = Vec::new();
        let outcome = run_offline_registry(|entry| {
            visited.push(entry.kind);
            Ok(entry.kind == OfflineCheckKind::Generated)
        })
        .expect("synthetic offline registry should complete");
        assert_eq!(outcome, Outcome::Drift, "one drift result must propagate");
        assert_eq!(
            visited,
            OFFLINE_CHECKS
                .iter()
                .map(|entry| entry.kind)
                .collect::<Vec<_>>(),
            "clean and drift results must not skip later checks"
        );

        let mut visited = Vec::new();
        let failed = run_offline_registry(|entry| {
            visited.push(entry.kind);
            if entry.kind == OfflineCheckKind::Scanner {
                Err(Report::new(DocsParityError::Scanner))
            } else {
                Ok(false)
            }
        });
        assert!(failed.is_err(), "an operational check error must propagate");
        assert_eq!(
            visited.last(),
            Some(&OfflineCheckKind::Scanner),
            "the aggregate must stop at the failed check"
        );
    }
}
