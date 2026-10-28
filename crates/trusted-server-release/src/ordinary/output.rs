//! Fresh atomic ordinary outputs; no index or qualification evidence writer.

use std::ffi::CString;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::os::fd::{AsRawFd as _, FromRawFd as _};
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

/// Retains CI-only diagnostic JSON in a distinct private directory.
///
/// # Errors
///
/// Rejects unsafe or reused paths and any failed atomic write or readback.
pub(super) fn diagnostic(directory: &Path, value: &Value) -> Result<PathBuf, Report<ReleaseError>> {
    persist(
        directory,
        "ordinary-comparison-diagnostic.json",
        value,
        Role::Metadata,
    )
}

/// Retains the exact target-scoped Cargo argument pair before compilation.
///
/// # Errors
///
/// Rejects reused or aliased output paths and any failed atomic write/readback.
pub(super) fn rustflags(
    directory: &Path,
    arguments: &[String],
) -> Result<PathBuf, Report<ReleaseError>> {
    persist(
        directory,
        "rustflags.json",
        &serde_json::json!(arguments),
        Role::Metadata,
    )
}

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
    persist_with_checkpoints(output, name, value, role, &|_, _| Ok(()))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WriteCheckpoint {
    Persisted,
    Verified,
}

type WriteCheck<'a> = dyn Fn(WriteCheckpoint, &Path) -> Result<(), Report<ReleaseError>> + 'a;

fn persist_with_checkpoints(
    output: &Path,
    name: &str,
    value: &Value,
    role: Role,
    checkpoint: &WriteCheck<'_>,
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
    let persisted = temporary
        .persist_noclobber(&destination)
        .map_err(|_| invalid())?;
    let verification = (|| {
        checkpoint(WriteCheckpoint::Persisted, output)?;
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
        checkpoint(WriteCheckpoint::Verified, output)
    })();
    if let Err(mut failure) = verification {
        if let Err(cleanup) = remove_owned_result(&held_output, &persisted, name) {
            failure = failure.attach(format!("ordinary output rollback failed: {cleanup:?}"));
        }
        return Err(failure);
    }
    Ok(destination)
}

/// Attempts to remove a result still matching our persisted inode.
///
/// Refuses observed replacements; the ordinary boundary does not contain a
/// hostile process with the same UID racing the final check and unlink.
fn remove_owned_result(
    directory: &fs::File,
    persisted: &fs::File,
    name: &str,
) -> Result<(), Report<ReleaseError>> {
    let own = persisted.metadata().map_err(|_| invalid())?;
    if own.nlink() == 0 {
        return Ok(());
    }
    let name = CString::new(name).map_err(|_| invalid())?;
    // SAFETY: the directory descriptor is live and the name is NUL-terminated.
    // O_NOFOLLOW prevents opening a replacement symlink's target.
    let descriptor = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    };
    require(
        descriptor >= 0,
        "ordinary rollback cannot locate its owned result",
    )?;
    // SAFETY: openat returned a new owned descriptor, not held by another File.
    let retained = unsafe { fs::File::from_raw_fd(descriptor) };
    let current = retained.metadata().map_err(|_| invalid())?;
    require(
        current.is_file() && (current.dev(), current.ino()) == (own.dev(), own.ino()),
        "ordinary rollback refuses a replacement result",
    )?;
    // SAFETY: the held directory and NUL-terminated leaf remain live; no parent
    // path is traversed, even if the output root was renamed or replaced.
    let removed = unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) };
    require(removed == 0, "ordinary owned result cannot be removed")?;
    directory.sync_all().map_err(|_| invalid())?;
    require(
        persisted.metadata().map_err(|_| invalid())?.nlink() == 0,
        "ordinary result still has an unknown filesystem link",
    )
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
