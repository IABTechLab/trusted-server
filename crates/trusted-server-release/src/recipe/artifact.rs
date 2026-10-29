//! Host-side copy of the built executable into the fresh output directory.
//!
//! The executable stays inside the writable build root, which a confined
//! child could still hold open or relink, so it is never published in place.
//! It is read once through a held descriptor and written into a new file
//! created exclusively and without following links, hashing exactly the bytes
//! written.

use std::fs::{self, File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Component, Path, PathBuf};

use error_stack::Report;
use sha2::{Digest as _, Sha256};

use super::recipe_error;
use crate::error::ReleaseError;

/// Largest executable the recipe copies.
pub(crate) const EXECUTABLE_LIMIT: u64 = 512 * 1024 * 1024;
/// Mode of the published asset, independent of the process umask.
const ASSET_MODE: u32 = 0o755;
/// Longest accepted asset file name.
const NAME_LIMIT: usize = 160;

/// The copied asset and the digest of exactly the bytes written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CopiedArtifact {
    name: String,
    size: u64,
    sha256: String,
    path: PathBuf,
}

impl CopiedArtifact {
    /// Asset file name inside the output directory.
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// Written size in bytes.
    pub(crate) fn size(&self) -> u64 {
        self.size
    }

    /// Lowercase hex SHA-256 of the written bytes.
    pub(crate) fn sha256(&self) -> &str {
        &self.sha256
    }

    /// Host path of the written file.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

fn artifact_error(detail: impl std::fmt::Display) -> Report<ReleaseError> {
    recipe_error(format!("artifact copy: {detail}"))
}

/// Requires a flat file name of letters, digits, `.`, `-` and `_` that does
/// not start with a dot.
fn check_name(name: &str) -> Result<(), Report<ReleaseError>> {
    let first_ok = name
        .bytes()
        .next()
        .is_some_and(|byte| byte.is_ascii_alphanumeric());
    let body_ok = name
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'));
    if name.len() > NAME_LIMIT || !first_ok || !body_ok || name.contains("..") {
        return Err(artifact_error(format!(
            "output name {name:?} is not a safe flat name"
        )));
    }
    Ok(())
}

/// Requires an existing real directory that no symlink aliases.
fn check_output_directory(output_dir: &Path) -> Result<(), Report<ReleaseError>> {
    let metadata = fs::symlink_metadata(output_dir).map_err(|error| {
        artifact_error(format!("output directory cannot be inspected: {error}"))
    })?;
    let resolved = fs::canonicalize(output_dir)
        .map_err(|error| artifact_error(format!("output directory cannot be resolved: {error}")))?;
    if !metadata.is_dir() || resolved.as_os_str() != output_dir.as_os_str() {
        return Err(artifact_error(
            "output directory is not a real directory reached without symlinks",
        ));
    }
    Ok(())
}

/// Requires every directory from the writable `root` down to the source's
/// parent to be a real directory, so no parent symlink can redirect the read.
///
/// Dot segments and sources outside `root` are rejected. The path is
/// re-resolved at the end and must equal itself. The leaf is opened with
/// `O_NOFOLLOW` separately. A confined child no longer runs when this is
/// called, so only a concurrent host process could race the walk.
fn check_source_path(root: &Path, source: &Path) -> Result<(), Report<ReleaseError>> {
    let relative = source
        .strip_prefix(root)
        .map_err(|_| artifact_error("source is outside the writable root"))?;
    let normal = source.components().collect::<PathBuf>().as_os_str() == source.as_os_str()
        && relative
            .components()
            .all(|component| matches!(component, Component::Normal(_)));
    let parent = source
        .parent()
        .filter(|_| normal && source.is_absolute() && !relative.as_os_str().is_empty())
        .ok_or_else(|| artifact_error("source is not an absolute normal path below the root"))?;
    let mut current = root.to_path_buf();
    let mut directories = vec![root.to_path_buf()];
    directories.extend(
        parent
            .strip_prefix(root)
            .map_err(|_| artifact_error("source parent is outside the writable root"))?
            .components()
            .map(|component| {
                current.push(component);
                current.clone()
            }),
    );
    for directory in &directories {
        let metadata = fs::symlink_metadata(directory).map_err(|error| {
            artifact_error(format!("source directory cannot be inspected: {error}"))
        })?;
        if !metadata.is_dir() {
            return Err(artifact_error(format!(
                "source directory {} is a symlink or not a directory",
                directory.display()
            )));
        }
    }
    let resolved = fs::canonicalize(parent)
        .map_err(|error| artifact_error(format!("source directory cannot be resolved: {error}")))?;
    if resolved != parent || !resolved.starts_with(root) {
        return Err(artifact_error(
            "source directory resolves outside the writable root",
        ));
    }
    Ok(())
}

/// Opens the source without following a leaf symlink and checks its facts.
fn open_source(source: &Path, limit: u64) -> Result<(File, fs::Metadata), Report<ReleaseError>> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(source)
        .map_err(|error| artifact_error(format!("source cannot be opened: {error}")))?;
    let metadata = file
        .metadata()
        .map_err(|error| artifact_error(format!("source cannot be inspected: {error}")))?;
    if !metadata.is_file() {
        return Err(artifact_error("source is not a regular file"));
    }
    // Cargo hard-links the executable into `deps/`; a third link is foreign.
    if !(1..=2).contains(&metadata.nlink()) {
        return Err(artifact_error(format!(
            "source has {} hard links; at most the Cargo deps link is expected",
            metadata.nlink()
        )));
    }
    if metadata.len() > limit {
        return Err(artifact_error(format!(
            "source exceeds the {limit} byte limit"
        )));
    }
    Ok((file, metadata))
}

/// Streams `source` into `destination`, returning the byte count and digest.
fn stream(
    source: &File,
    before: &fs::Metadata,
    destination: &mut File,
) -> Result<(u64, String), Report<ReleaseError>> {
    let mut input = source;
    let mut digest = Sha256::new();
    let mut size = 0_u64;
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let count = input
            .read(&mut buffer)
            .map_err(|error| artifact_error(format!("source read failed: {error}")))?;
        if count == 0 {
            break;
        }
        size += count as u64;
        if size > before.len() {
            return Err(artifact_error("source grows while it is copied"));
        }
        digest.update(&buffer[..count]);
        destination
            .write_all(&buffer[..count])
            .map_err(|error| artifact_error(format!("output write failed: {error}")))?;
    }
    let after = source
        .metadata()
        .map_err(|error| artifact_error(format!("source cannot be re-inspected: {error}")))?;
    let stable = (
        before.dev(),
        before.ino(),
        before.len(),
        before.mtime(),
        before.mtime_nsec(),
    ) == (
        after.dev(),
        after.ino(),
        after.len(),
        after.mtime(),
        after.mtime_nsec(),
    );
    if size != before.len() || !stable {
        return Err(artifact_error("source changes while it is copied"));
    }
    Ok((size, hex::encode(digest.finalize())))
}

/// Copies the executable at `source`, below the writable `source_root`, to
/// `output_dir/name`.
///
/// # Errors
///
/// Returns [`ReleaseError::Recipe`] for an unsafe name, a source path outside
/// `source_root`, with dot segments or below a symlinked directory, an aliased output
/// directory, a source that is a symlink, not a regular file, has more than
/// two hard links, exceeds `limit` or changes while copied, or an output that
/// already exists (including as a symlink). A failed copy removes the file it
/// created.
pub(crate) fn copy_artifact(
    source: &Path,
    source_root: &Path,
    output_dir: &Path,
    name: &str,
    limit: u64,
) -> Result<CopiedArtifact, Report<ReleaseError>> {
    check_name(name)?;
    check_output_directory(output_dir)?;
    check_source_path(source_root, source)?;
    let (input, before) = open_source(source, limit)?;
    let path = output_dir.join(name);
    let mut destination = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&path)
        .map_err(|error| {
            artifact_error(format!(
                "output {name} cannot be created exclusively: {error}"
            ))
        })?;
    let written = stream(&input, &before, &mut destination)
        .and_then(|(size, sha256)| finish(&destination, &path, size).map(|()| (size, sha256)));
    match written {
        Ok((size, sha256)) => Ok(CopiedArtifact {
            name: name.to_owned(),
            size,
            sha256,
            path,
        }),
        Err(error) => {
            // The file was created exclusively above, so it is ours to remove.
            drop(destination);
            let _ = fs::remove_file(&path);
            Err(error)
        }
    }
}

/// Sets the final mode, syncs and verifies the written file.
fn finish(destination: &File, path: &Path, size: u64) -> Result<(), Report<ReleaseError>> {
    destination
        .set_permissions(fs::Permissions::from_mode(ASSET_MODE))
        .and_then(|()| destination.sync_all())
        .map_err(|error| artifact_error(format!("output cannot be finalized: {error}")))?;
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| artifact_error(format!("output cannot be re-inspected: {error}")))?;
    if !metadata.is_file() || metadata.nlink() != 1 || metadata.len() != size {
        return Err(artifact_error(
            "output is not one regular file of the written size",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
