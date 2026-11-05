//! Translation between host paths and the build's execution namespace.
//!
//! The confined command sees the boundary roots under their execution paths
//! (identical on macOS, `/boundary/<role>/<index>` mount points in the Linux container). Every
//! path the recipe hands to Cargo, and every path Cargo reports back, crosses
//! this seam, so the recipe never guesses a mount layout.

use std::path::{Path, PathBuf};

use error_stack::Report;

use super::recipe_error;
use crate::boundary::Boundary;
use crate::error::ReleaseError;

/// Maps host paths into the execution namespace and writable paths back.
pub(crate) trait ExecutionPaths {
    /// Translates a host path inside any boundary root.
    ///
    /// # Errors
    ///
    /// Returns an error when `host` is outside every root.
    fn execution_path(&self, host: &Path) -> Result<PathBuf, Report<ReleaseError>>;

    /// Translates an execution path inside a writable root back to the host.
    ///
    /// # Errors
    ///
    /// Returns an error when `execution` is outside every writable root, so
    /// a build can never name an immutable or tool file as its output.
    fn host_path(&self, execution: &Path) -> Result<PathBuf, Report<ReleaseError>>;
}

impl ExecutionPaths for Boundary {
    fn execution_path(&self, host: &Path) -> Result<PathBuf, Report<ReleaseError>> {
        Boundary::execution_path(self, host)
    }

    fn host_path(&self, execution: &Path) -> Result<PathBuf, Report<ReleaseError>> {
        Boundary::host_path(self, execution)
    }
}

/// Translates `host` and requires UTF-8 text without `:` or NUL, so the
/// result can travel in an environment variable, a `PATH` or a Cargo config.
///
/// # Errors
///
/// Returns [`ReleaseError::Recipe`] when `host` is outside the roots or the
/// translation is not plain text.
pub(super) fn execution_text(
    paths: &dyn ExecutionPaths,
    label: &str,
    host: &Path,
) -> Result<String, Report<ReleaseError>> {
    let translated = paths
        .execution_path(host)
        .map_err(|error| error.change_context(ReleaseError::Recipe))
        .map_err(|error| error.attach(format!("{label} is outside the boundary roots")))?;
    translated
        .to_str()
        .filter(|text| {
            translated.is_absolute()
                && !text.contains([':', '"', '\\'])
                && !text.chars().any(char::is_control)
        })
        .map(str::to_owned)
        .ok_or_else(|| {
            recipe_error(format!(
                "{label} has an execution path that is not plain absolute UTF-8"
            ))
        })
}

#[cfg(test)]
pub(super) use fake::PrefixPaths;

#[cfg(test)]
mod fake;
