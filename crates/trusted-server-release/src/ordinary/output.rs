//! Fresh atomic ordinary outputs; no index or qualification evidence writer.

use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::{
    DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _,
};
use std::path::{Path, PathBuf};

use error_stack::Report;
use serde_json::Value;
use sha2::{Digest as _, Sha256};

use super::transfer::Checked;
use super::{Observation, require, root};
use crate::documents::{self, Role};
use crate::error::ReleaseError;

const COMPANION: &str = "trusted-server-fastly.wasm.rebuild-check.json";

/// Seals one matched ordinary comparison under the permanent companion name.
///
/// # Errors
///
/// Rejects inconsistent retained bytes, noncanonical parents, reused roots,
/// unsafe paths or any failure to persist the exact private result atomically.
pub(super) fn companion(
    checked: &Checked,
    directory: &Path,
) -> Result<PathBuf, Report<ReleaseError>> {
    let value = checked.receipt();
    let (binary, spdx) = checked.inputs();
    require(
        value["outcome"] == "matched"
            && value["release_qualification"] == false
            && value["canonical_sha256"] == hex::encode(Sha256::digest(binary))
            && value["canonical"]["size"].as_u64() == Some(binary.len() as u64)
            && value["canonical"]["spdx_sha256"] == hex::encode(Sha256::digest(spdx)),
        "ordinary retained canonical inputs changed before companion handoff",
    )?;
    persist(directory, COMPANION, &value, Role::Companion)
}

/// Retains the unpublished ordinary observation under its fixed transfer name.
///
/// # Errors
///
/// Rejects a reused/aliased output root or a failed atomic private write.
pub(super) fn observation(
    observed: &Observation,
    directory: &Path,
) -> Result<PathBuf, Report<ReleaseError>> {
    persist(
        directory,
        "fastly-observation.json",
        observed.value(),
        Role::Metadata,
    )
}

fn invalid() -> Report<ReleaseError> {
    Report::new(ReleaseError::Document).attach("ordinary fresh output cannot be sealed")
}

fn same_directory(held: &fs::File, path: &Path) -> Result<(), Report<ReleaseError>> {
    let before = held.metadata().map_err(|_| invalid())?;
    let now = fs::symlink_metadata(path).map_err(|_| invalid())?;
    require(
        now.is_dir()
            && (before.dev(), before.ino(), before.uid()) == (now.dev(), now.ino(), now.uid()),
        "ordinary output directory identity changed",
    )
}

fn directory(path: &Path) -> Result<fs::File, Report<ReleaseError>> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|_| invalid())
}

fn persist(
    output: &Path,
    name: &str,
    value: &Value,
    role: Role,
) -> Result<PathBuf, Report<ReleaseError>> {
    let bytes = documents::canonical_json(value, role)?;
    let parent = output.parent().ok_or_else(invalid)?;
    require(
        output.to_str().is_some_and(root)
            && fs::canonicalize(parent).is_ok_and(|resolved| resolved == parent)
            && fs::symlink_metadata(output)
                .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound),
        "ordinary output must use an absent root below its canonical parent",
    )?;
    let held_parent = directory(parent)?;
    fs::DirBuilder::new()
        .mode(0o700)
        .create(output)
        .map_err(|_| invalid())?;
    let held_output = directory(output)?;
    same_directory(&held_parent, parent)?;
    let destination = output.join(name);
    let mut temporary = tempfile::Builder::new()
        .prefix(".ordinary-")
        .tempfile_in(output)
        .map_err(|_| invalid())?;
    temporary
        .as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|_| invalid())?;
    temporary.write_all(&bytes).map_err(|_| invalid())?;
    temporary.as_file().sync_all().map_err(|_| invalid())?;
    same_directory(&held_parent, parent)?;
    same_directory(&held_output, output)?;
    temporary
        .persist_noclobber(&destination)
        .map_err(|_| invalid())?;
    held_output.sync_all().map_err(|_| invalid())?;
    held_parent.sync_all().map_err(|_| invalid())?;
    same_directory(&held_parent, parent)?;
    same_directory(&held_output, output)?;
    let retained = documents::read_bounded(&destination, role)?;
    let entries = fs::read_dir(output)
        .map_err(|_| invalid())?
        .take(2)
        .map(|entry| entry.map(|entry| entry.file_name()));
    require(
        retained.bytes == bytes && exact_entry(entries, name)?,
        "ordinary output has changed or contains unexpected entries",
    )?;
    Ok(destination)
}

fn exact_entry(
    entries: impl Iterator<Item = std::io::Result<std::ffi::OsString>>,
    expected: &str,
) -> Result<bool, Report<ReleaseError>> {
    let entries = entries
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| invalid())?;
    Ok(entries.len() == 1 && entries[0] == expected)
}

#[cfg(test)]
mod tests;
