//! `ts config migrate`, which rewrites a settings document written for an
//! earlier release into the shape this release reads.
//!
//! The rewrite works on the file alone, for the adapter the deployment runs
//! on, keeps the comments and the order the operator wrote, and leaves in
//! place, with directions, every setting that needs a decision or a value
//! the document does not hold. See
//! `docs/superpowers/specs/2026-10-17-config-migrate-design.md`.

mod layout;
mod rules;
#[cfg(test)]
mod tests;

use std::fs;
use std::io::Write;
use std::path::PathBuf;

use crate::RunOutcome;
use crate::commands::audit::generate::validate::validate_source_config;

use rules::{Migration, migrate};

/// Rewrite a settings document written for an earlier release into the shape
/// this release reads.
#[derive(Debug, clap::Args)]
pub struct ConfigMigrateArgs {
    /// The settings document to migrate, such as `trusted-server.toml`.
    pub file: PathBuf,
    /// The adapter the deployment runs on, which decides what main's location
    /// lookup becomes.
    #[arg(long, value_enum)]
    pub adapter: Adapter,
    /// Rewrite the file in place, rather than print the migrated document.
    #[arg(long, conflicts_with = "check")]
    pub write: bool,
    /// Change nothing, and exit 1 when the document needs migrating.
    #[arg(long)]
    pub check: bool,
}

/// The adapter a deployment runs on.
///
/// Main's location lookup and location headers differed by adapter, and
/// the Axum and Spin adapters refuse `[geo] module = "platform"` at startup
/// because neither host has a location lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Adapter {
    /// Fastly Compute.
    Fastly,
    /// Cloudflare Workers.
    Cloudflare,
    /// The Axum dev server.
    Axum,
    /// Spin.
    Spin,
}

impl Adapter {
    /// Whether the host has a location lookup of its own, which `[geo]
    /// module = "platform"` selects.
    pub(crate) const fn has_location_lookup(self) -> bool {
        matches!(self, Self::Fastly | Self::Cloudflare)
    }

    /// The adapter as a line on stderr names it.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Fastly => "Fastly",
            Self::Cloudflare => "Cloudflare",
            Self::Axum => "the Axum dev server",
            Self::Spin => "Spin",
        }
    }
}

/// Migrates the document `args` names, printing the result on stdout and
/// what was done and what is left on stderr.
///
/// # Errors
///
/// When the file cannot be read or written, or is not TOML.
pub fn run_config_migrate(args: &ConfigMigrateArgs) -> Result<RunOutcome, String> {
    let stdout = std::io::stdout();
    let stderr = std::io::stderr();
    run_with_writers(args, &mut stdout.lock(), &mut stderr.lock())
}

/// [`run_config_migrate`], writing to `out` and `report`.
///
/// The outcome is a success when nothing is left to do and the migrated
/// document passes validation, and with `--check` when the document needs
/// no migrating.
///
/// # Errors
///
/// When the file cannot be read or written, or is not TOML.
pub(crate) fn run_with_writers(
    args: &ConfigMigrateArgs,
    out: &mut dyn Write,
    report: &mut dyn Write,
) -> Result<RunOutcome, String> {
    let input = fs::read_to_string(&args.file)
        .map_err(|error| format!("failed to read {}: {error}", args.file.display()))?;
    let migration = migrate(&input, args.adapter)
        .map_err(|error| format!("failed to read {}: {error}", args.file.display()))?;
    write_report(report, &migration)?;

    if args.check {
        let needs = needs_migrating(&migration);
        line(
            report,
            if needs {
                "the document needs migrating"
            } else {
                "the document needs no migrating"
            },
        )?;
        return Ok(outcome(!needs));
    }

    if args.write {
        if migration.text != input {
            fs::write(&args.file, &migration.text)
                .map_err(|error| format!("failed to write {}: {error}", args.file.display()))?;
        }
    } else {
        out.write_all(migration.text.as_bytes())
            .map_err(|error| format!("failed to write the migrated document: {error}"))?;
    }

    let valid = validate_source_config(&migration.text);
    match &valid {
        Ok(()) => line(report, "the migrated document passes validation")?,
        Err(reason) => line(
            report,
            &format!(
                "the migrated document does not pass validation ({reason}). `ts config \
                 validate` prints the whole refusal"
            ),
        )?,
    }
    Ok(outcome(valid.is_ok() && migration.report.to_do.is_empty()))
}

/// Whether the document needs migrating, being a rewrite to make or an item
/// left for the operator.
fn needs_migrating(migration: &Migration) -> bool {
    !migration.report.changed.is_empty() || !migration.report.to_do.is_empty()
}

const fn outcome(success: bool) -> RunOutcome {
    if success {
        RunOutcome::Success
    } else {
        RunOutcome::AssertionFailed
    }
}

/// One line on stderr for each change, each item left to do and each
/// change in behavior to check.
fn write_report(report: &mut dyn Write, migration: &Migration) -> Result<(), String> {
    let lines = &migration.report;
    if lines.changed.is_empty() && lines.to_do.is_empty() {
        line(report, "nothing to migrate")?;
    }
    for (label, items) in [
        ("changed", &lines.changed),
        ("to do", &lines.to_do),
        ("check", &lines.check),
    ] {
        for item in items {
            line(report, &format!("{label}: {item}"))?;
        }
    }
    Ok(())
}

fn line(report: &mut dyn Write, text: &str) -> Result<(), String> {
    writeln!(report, "{text}").map_err(|error| format!("failed to write command output: {error}"))
}
