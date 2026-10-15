// Atomic output and schema helpers are consumed by later producer stages.
#[allow(dead_code)]
mod documents;
mod error;
mod policy;
mod preflight;
mod source;

use std::ffi::OsString;
use std::io::Write as _;
use std::path::{Component, Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use error::ReleaseError;
use error_stack::Report;
use error_stack::fmt::ColorMode;

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
}

fn main() -> ExitCode {
    let inherited: Vec<(OsString, OsString)> = std::env::vars_os().collect();
    Report::set_color_mode(ColorMode::None);
    let arguments: Vec<OsString> = std::env::args_os().collect();
    match run(&inherited, &arguments) {
        Ok(status) => status,
        Err(report) => {
            let _ = writeln!(std::io::stderr(), "{report:?}");
            ExitCode::FAILURE
        }
    }
}

fn run(
    inherited: &[(OsString, OsString)],
    arguments: &[OsString],
) -> Result<ExitCode, Report<ReleaseError>> {
    let cli = Cli::try_parse_from(arguments).map_err(|_| Report::new(ReleaseError::Pending))?;
    match cli.command {
        Command::Preflight {
            cohort,
            source_commit,
            require_qualified,
        } => preflight(inherited, &cohort, &source_commit, require_qualified),
        Command::AuditInputs => audit_inputs(),
    }
}

fn current_root() -> Result<PathBuf, Report<ReleaseError>> {
    std::env::current_dir().map_err(|error| {
        Report::new(ReleaseError::Source).attach(format!("cannot resolve source root: {error}"))
    })
}

fn names_cohort(root: &Path, cohort: &Path) -> bool {
    !cohort.as_os_str().is_empty()
        && !cohort
            .components()
            .any(|component| matches!(component, Component::ParentDir))
        && root
            .join(cohort)
            .components()
            .eq(root.join(policy::COHORT_PATH).components())
}

fn preflight(
    inherited: &[(OsString, OsString)],
    cohort: &Path,
    commit: &str,
    require_qualified: bool,
) -> Result<ExitCode, Report<ReleaseError>> {
    let audited = preflight::audit_environment(inherited)?;
    if !audited.removed_names().is_empty() {
        let _ = writeln!(
            std::io::stderr(),
            "removed inherited operator variables: {}",
            audited.removed_names().join(", ")
        );
    }
    let root = current_root()?;
    if !names_cohort(&root, cohort) {
        return Err(Report::new(ReleaseError::Policy).attach(format!(
            "--cohort must name {} below the current source root",
            policy::COHORT_PATH
        )));
    }
    let source = source::authenticate(&root, commit, &audited)?;
    let inputs = policy::load_inputs(&root)?;
    for (path, sha256) in inputs.input_digests() {
        if source.tracked_sha256(path) != Some(sha256.as_str()) {
            return Err(Report::new(ReleaseError::Source)
                .attach(format!("{path} does not match the approved commit")));
        }
    }
    let mut stderr = std::io::stderr();
    let _ = writeln!(
        stderr,
        "source {} authenticated locally (epoch {}, snapshot {}, helper closure {} over {} paths); \
         remote branch-tip ancestry and ruleset validation are not established locally (Task 14)",
        source.commit(),
        source.epoch(),
        source.snapshot().fingerprint(),
        source.closure_digest(),
        source.closure_paths().len()
    );
    if !require_qualified {
        let _ = writeln!(
            stderr,
            "qualified selection is required whether or not --require-qualified is passed"
        );
    }
    let selection = policy::select_qualified(&inputs, &[])?;
    source.snapshot().verify_unchanged()?;
    let _ = writeln!(stderr, "{}", selection.summary());
    Ok(ExitCode::SUCCESS)
}

fn audit_inputs() -> Result<ExitCode, Report<ReleaseError>> {
    let root = current_root()?;
    let inputs = policy::load_inputs(&root)?;
    let audit = policy::audit_inputs(&inputs);
    let bytes = documents::canonical_json(&audit.to_json(), documents::Role::Metadata)?;
    std::io::stdout().write_all(&bytes).map_err(|error| {
        Report::new(ReleaseError::Incomplete).attach(format!("cannot write audit report: {error}"))
    })?;
    if audit.is_complete() {
        return Ok(ExitCode::SUCCESS);
    }
    let _ = writeln!(
        std::io::stderr(),
        "{}: release inputs are incomplete; this diagnostic authorizes nothing",
        ReleaseError::Incomplete
    );
    Ok(ExitCode::from(INCOMPLETE_STATUS))
}
