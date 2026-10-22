//! Explicit selection of the pinned Viceroy executable.
//!
//! The gate never searches `PATH`: the caller names one absolute regular
//! file. Selection hashes it, requires `--version` to report exactly
//! [`PINNED_VICEROY_VERSION`], and rehashes it so the reported version and
//! the recorded digest describe the same bytes.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use error_stack::Report;

use super::{RuntimeRejection, runtime_error};
use crate::documents::hash_bounded_with_check;
use crate::error::ReleaseError;
use crate::process::{BoundedCommand, ProcessLimits};

/// The Viceroy version pinned by `.tool-versions`.
pub(crate) const PINNED_VICEROY_VERSION: &str = "0.21.0";
/// Upper bound on the Viceroy executable size.
const VICEROY_LIMIT: u64 = 512 * 1024 * 1024;

/// A selected, version-checked and hashed Viceroy executable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SelectedViceroy {
    path: PathBuf,
    version: String,
    sha256: String,
}

fn selection_error(message: impl std::fmt::Display) -> Report<ReleaseError> {
    runtime_error(RuntimeRejection::Viceroy, message)
}

impl SelectedViceroy {
    /// Selects the Viceroy executable at the absolute `path`.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeRejection::Viceroy`] when `path` is not absolute, is
    /// not a regular file, cannot be hashed, does not report exactly the
    /// pinned version, or changes while it is checked.
    pub(crate) fn select(path: &Path) -> Result<Self, Report<ReleaseError>> {
        if !path.is_absolute() {
            return Err(selection_error("Viceroy path is not absolute"));
        }
        let metadata = std::fs::symlink_metadata(path)
            .map_err(|error| selection_error(format!("cannot inspect Viceroy: {error}")))?;
        if !metadata.is_file() {
            return Err(selection_error("Viceroy is not a regular file"));
        }
        let sha256 = digest(path)?;
        let working_directory = path
            .parent()
            .ok_or_else(|| selection_error("Viceroy path has no parent directory"))?;
        let stdout = BoundedCommand::new(
            path,
            working_directory,
            ProcessLimits {
                wall: Duration::from_secs(60),
                stdout_max: 4096,
                stderr_max: 4096,
            },
        )
        .map_err(|error| error.change_context(ReleaseError::Runtime))?
        .arguments([OsString::from("--version")])
        .run()
        .and_then(crate::process::ProcessResult::require_success)
        .map_err(|error| {
            error
                .change_context(ReleaseError::Runtime)
                .attach(RuntimeRejection::Viceroy)
        })?;
        let version = parse_version_output(&stdout)?;
        let selected = Self {
            path: path.to_owned(),
            version,
            sha256,
        };
        selected.reverify()?;
        Ok(selected)
    }

    /// The selected absolute path.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// The reported version, equal to [`PINNED_VICEROY_VERSION`].
    pub(crate) fn version(&self) -> &str {
        &self.version
    }

    /// SHA-256 of the selected executable.
    pub(crate) fn sha256(&self) -> &str {
        &self.sha256
    }

    /// Requires the executable to still hash to the recorded digest.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeRejection::Viceroy`] when it cannot be hashed or its
    /// bytes changed.
    pub(crate) fn reverify(&self) -> Result<(), Report<ReleaseError>> {
        if digest(&self.path)? != self.sha256 {
            return Err(selection_error(
                "Viceroy executable changed after selection",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
impl SelectedViceroy {
    /// A selection that was never version-checked, for tests that must be
    /// rejected before Viceroy would run.
    pub(crate) fn unverified_for_tests(path: &Path, sha256: &str) -> Self {
        Self {
            path: path.to_owned(),
            version: PINNED_VICEROY_VERSION.to_owned(),
            sha256: sha256.to_owned(),
        }
    }
}

fn digest(path: &Path) -> Result<String, Report<ReleaseError>> {
    hash_bounded_with_check(path, VICEROY_LIMIT, &|| Ok(()))
        .map(|measured| measured.sha256)
        .map_err(|error| {
            error
                .change_context(ReleaseError::Runtime)
                .attach(RuntimeRejection::Viceroy)
        })
}

/// Parses `viceroy <version>\n` and requires the pinned version.
///
/// # Errors
///
/// Returns [`RuntimeRejection::Viceroy`] for any other output, including a
/// different, prefixed or suffixed version.
pub(in crate::recipe) fn parse_version_output(
    stdout: &[u8],
) -> Result<String, Report<ReleaseError>> {
    let version = std::str::from_utf8(stdout)
        .ok()
        .and_then(|text| text.strip_suffix('\n'))
        .and_then(|line| line.strip_prefix("viceroy "))
        .filter(|version| !version.contains(char::is_whitespace))
        .ok_or_else(|| selection_error("unrecognized Viceroy version output"))?;
    if version != PINNED_VICEROY_VERSION {
        return Err(selection_error(format!(
            "Viceroy reports {version:?}, not the pinned {PINNED_VICEROY_VERSION}"
        )));
    }
    Ok(version.to_owned())
}
