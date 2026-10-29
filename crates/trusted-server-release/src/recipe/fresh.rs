//! Lexical and on-disk checks for recipe request roots.

use std::fs;
use std::io;
use std::path::Path;

use error_stack::Report;

use super::recipe_error;
use crate::error::ReleaseError;

/// Requires an absolute UTF-8 path that is already in normal form.
///
/// `Path::components` silently drops `.` and doubled or trailing separators,
/// so the text is compared with its rebuilt components instead.
pub(super) fn normal_absolute(label: &str, path: &Path) -> Result<(), Report<ReleaseError>> {
    let normal = path.is_absolute()
        && path.to_str().is_some()
        && path
            .components()
            .collect::<std::path::PathBuf>()
            .as_os_str()
            == path.as_os_str()
        && !path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir));
    if normal {
        return Ok(());
    }
    Err(recipe_error(format!(
        "{label} must be an absolute normal path without dot segments"
    )))
}

/// Requires `path` to resolve to itself, so no ancestor or leaf is a symlink.
fn unaliased(label: &str, path: &Path) -> Result<(), Report<ReleaseError>> {
    match fs::canonicalize(path) {
        Ok(resolved) if resolved == path => Ok(()),
        Ok(_) => Err(recipe_error(format!(
            "{label} is a symlink or traverses one"
        ))),
        Err(error) => Err(recipe_error(format!("{label} cannot be resolved: {error}"))),
    }
}

/// Requires an existing real directory that is not an alias.
pub(super) fn real_directory(label: &str, path: &Path) -> Result<(), Report<ReleaseError>> {
    unaliased(label, path)?;
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| recipe_error(format!("{label} cannot be inspected: {error}")))?;
    if !metadata.is_dir() {
        return Err(recipe_error(format!("{label} must be a directory")));
    }
    Ok(())
}

/// Requires an existing real regular file that is not an alias.
pub(super) fn real_file(label: &str, path: &Path) -> Result<(), Report<ReleaseError>> {
    unaliased(label, path)?;
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| recipe_error(format!("{label} cannot be inspected: {error}")))?;
    if !metadata.is_file() {
        return Err(recipe_error(format!("{label} must be a regular file")));
    }
    Ok(())
}

/// Requires a fresh root: a real directory with no entries.
pub(super) fn empty_directory(label: &str, path: &Path) -> Result<(), Report<ReleaseError>> {
    real_directory(label, path)?;
    let mut entries = fs::read_dir(path)
        .map_err(|error| recipe_error(format!("{label} cannot be listed: {error}")))?;
    if entries.next().is_some() {
        return Err(recipe_error(format!(
            "{label} already has entries and cannot be reused"
        )));
    }
    Ok(())
}

/// Requires a path that does not exist yet inside a real parent directory.
pub(super) fn absent_in_real_parent(label: &str, path: &Path) -> Result<(), Report<ReleaseError>> {
    match fs::symlink_metadata(path) {
        Ok(_) => {
            return Err(recipe_error(format!(
                "{label} already exists and cannot be reused"
            )));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(recipe_error(format!(
                "{label} cannot be inspected: {error}"
            )));
        }
    }
    let parent = path
        .parent()
        .ok_or_else(|| recipe_error(format!("{label} has no parent directory")))?;
    real_directory(label, parent)
}

/// Rejects any two labelled paths where one contains or equals the other.
pub(super) fn disjoint(paths: &[(&str, &Path)]) -> Result<(), Report<ReleaseError>> {
    for (index, (first_label, first)) in paths.iter().enumerate() {
        for (second_label, second) in paths.iter().skip(index + 1) {
            if first.starts_with(second) || second.starts_with(first) {
                return Err(recipe_error(format!(
                    "{first_label} and {second_label} overlap"
                )));
            }
        }
    }
    Ok(())
}

/// Requires the prepared materials root: a real directory holding exactly the
/// `vendor` directory, which is itself a real directory.
pub(super) fn prepared_materials(label: &str, path: &Path) -> Result<(), Report<ReleaseError>> {
    real_directory(label, path)?;
    let names = fs::read_dir(path)
        .map_err(|error| recipe_error(format!("{label} cannot be listed: {error}")))?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| recipe_error(format!("{label} cannot be listed: {error}")))?;
    if names != [std::ffi::OsString::from("vendor")] {
        return Err(recipe_error(format!(
            "{label} must hold exactly the prepared vendor directory"
        )));
    }
    real_directory("vendor directory", &path.join("vendor"))
}
