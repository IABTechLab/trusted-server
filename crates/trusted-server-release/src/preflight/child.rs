//! Allowlisted child-environment construction for recipe execution.
//!
//! The child environment is built from scratch: nothing inherited passes
//! through, and only recipe-selected facts and private locations are set.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use error_stack::Report;

use super::{AuditedEnvironment, RELEASE_BUILD};
use crate::error::ReleaseError;
use crate::policy::ReleaseTarget;
use crate::source::{ApprovedSource, CommitId};

const MACOS_DEPLOYMENT_TARGET: &str = "13.0";

/// A tool directory produced by authenticated acquisition (Task 8).
pub(crate) struct ToolDirectory(pub(super) PathBuf);

/// Recipe-selected private locations for one child build.
pub(crate) struct ChildRequest {
    pub(crate) target: ReleaseTarget,
    pub(crate) home: PathBuf,
    pub(crate) cargo_home: PathBuf,
    pub(crate) tool_directories: Vec<ToolDirectory>,
}

/// An allowlisted child environment constructed from scratch.
pub(crate) struct ChildEnvironment {
    variables: BTreeMap<String, String>,
    removed: Vec<String>,
}

impl ChildEnvironment {
    /// Every variable the child receives; nothing is inherited.
    pub(crate) fn variables(&self) -> &BTreeMap<String, String> {
        &self.variables
    }

    /// Sorted names injected by the recipe, recorded as input facts.
    pub(crate) fn injected_names(&self) -> Vec<&str> {
        self.variables.keys().map(String::as_str).collect()
    }

    /// Inherited operator names removed during the audit.
    pub(crate) fn removed_names(&self) -> &[String] {
        &self.removed
    }
}

fn absolute_text<'a>(label: &str, path: &'a Path) -> Result<&'a str, Report<ReleaseError>> {
    path.to_str()
        .filter(|text| path.is_absolute() && !text.contains([':', '\0']))
        .ok_or_else(|| {
            Report::new(ReleaseError::Environment).attach(format!(
                "{label} must be an absolute UTF-8 path without ':' or NUL"
            ))
        })
}

/// Joins authenticated tool directories into a child `PATH`, if any are given.
fn tool_path(directories: &[ToolDirectory]) -> Result<Option<String>, Report<ReleaseError>> {
    if directories.is_empty() {
        return Ok(None);
    }
    let entries = directories
        .iter()
        .map(|directory| absolute_text("tool directory", &directory.0))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Some(entries.join(":")))
}

/// Builds the child environment from explicit approved facts.
pub(super) fn build_child(
    audited: &AuditedEnvironment,
    commit: &CommitId,
    epoch: u64,
    request: &ChildRequest,
) -> Result<ChildEnvironment, Report<ReleaseError>> {
    let mut variables = BTreeMap::from([
        ("AWS_LC_SYS_USE_SYSTEM".to_owned(), "0".to_owned()),
        (
            format!(
                "AWS_LC_SYS_USE_SYSTEM_{}",
                request.target.triple().replace('-', "_")
            ),
            "0".to_owned(),
        ),
        (
            "CARGO_HOME".to_owned(),
            absolute_text("CARGO_HOME", &request.cargo_home)?.to_owned(),
        ),
        (
            "HOME".to_owned(),
            absolute_text("HOME", &request.home)?.to_owned(),
        ),
        ("LC_ALL".to_owned(), "C".to_owned()),
        ("LIBSQLITE3_SYS_USE_PKG_CONFIG".to_owned(), "0".to_owned()),
        ("SOURCE_DATE_EPOCH".to_owned(), epoch.to_string()),
        (
            "TRUSTED_SERVER__GIT_VERSION".to_owned(),
            commit.as_str().to_owned(),
        ),
        (RELEASE_BUILD.to_owned(), "1".to_owned()),
        ("TZ".to_owned(), "UTC".to_owned()),
    ]);
    if let Some(path) = tool_path(&request.tool_directories)? {
        variables.insert("PATH".to_owned(), path);
    }
    if request.target.is_darwin() {
        variables.insert(
            "MACOSX_DEPLOYMENT_TARGET".to_owned(),
            MACOS_DEPLOYMENT_TARGET.to_owned(),
        );
    }
    Ok(ChildEnvironment {
        variables,
        removed: audited.removed.clone(),
    })
}

/// Builds the allowlisted child environment for one recipe execution.
///
/// This is the production entry point: it requires both the audited
/// inherited environment and the locally authenticated source, so the
/// injected commit and committer epoch can only come from authentication.
///
/// # Errors
///
/// Fails when a private location or tool directory is not an absolute UTF-8
/// path without `:` or NUL.
pub(crate) fn child_environment(
    audited: &AuditedEnvironment,
    source: &ApprovedSource,
    request: &ChildRequest,
) -> Result<ChildEnvironment, Report<ReleaseError>> {
    build_child(audited, source.commit(), source.epoch(), request)
}

/// Builds a child environment from explicit facts, for recipe tests only.
///
/// Production code must go through [`child_environment`], which takes the
/// commit and epoch from an [`ApprovedSource`].
///
/// # Errors
///
/// As [`child_environment`].
#[cfg(test)]
pub(crate) fn build_child_for_tests(
    audited: &AuditedEnvironment,
    commit: &CommitId,
    epoch: u64,
    request: &ChildRequest,
) -> Result<ChildEnvironment, Report<ReleaseError>> {
    build_child(audited, commit, epoch, request)
}
