//! Reject fetched sources while admitting exact Cargo registry bookkeeping.

use std::fs::{self, Metadata, OpenOptions};
use std::io;
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::path::Path;

use error_stack::{Report, ResultExt as _};
use sha2::{Digest as _, Sha256};

use super::recipe_error;
use crate::documents::hash_bounded_with_check;
use crate::error::ReleaseError;

// Cargo 1.98's cargo-util creates exactly this tag before source replacement.
// The upstream URL is a required wire-format byte sequence, never a fetch target.
const CACHE_TAG: &[u8] = concat!(
    "Signature: 8a477f597d28d172789f06886806bc55\n",
    "# This file is a cache directory tag created by cargo.\n",
    "# For information about cache directory tags see https://bford.info/cachedir/\n" // allow-domain: bford.info
)
.as_bytes();

fn stamp(metadata: &Metadata) -> (u64, u64, u64, u64, u32, i64, i64, i64, i64) {
    (
        metadata.dev(),
        metadata.ino(),
        metadata.nlink(),
        metadata.len(),
        metadata.mode(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec(),
    )
}

/// Checks a private Cargo home without creating, deleting or repairing entries.
///
/// # Errors
///
/// Rejects fetched sources, unexpected bookkeeping, aliases, changed inputs,
/// unreadable paths and cancellation or an expired owning attempt.
pub(super) fn check(
    cargo_home: &Path,
    check_attempt: &dyn Fn() -> Result<(), Report<ReleaseError>>,
) -> Result<(), Report<ReleaseError>> {
    check_attempt()?;
    match fs::symlink_metadata(cargo_home.join("git")) {
        Ok(_) => return Err(recipe_error("Cargo home holds a git cache")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(recipe_error(format!(
                "Cargo home git cache cannot be inspected: {error}"
            )));
        }
    }
    let registry = cargo_home.join("registry");
    let before = match fs::symlink_metadata(&registry) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return check_attempt(),
        Err(error) => {
            return Err(recipe_error(format!(
                "Cargo registry cannot be inspected: {error}"
            )));
        }
    };
    if !before.is_dir() {
        return Err(recipe_error(
            "Cargo registry bookkeeping must be a direct directory",
        ));
    }
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(&registry)
        .map_err(recipe_error)?;
    if stamp(&before) != stamp(&directory.metadata().map_err(recipe_error)?) {
        return Err(recipe_error(
            "Cargo registry directory changed before inspection",
        ));
    }
    for entry in fs::read_dir(&registry).map_err(recipe_error)? {
        check_attempt()?;
        let entry = entry.map_err(recipe_error)?;
        if entry.file_name() != "CACHEDIR.TAG" {
            return Err(recipe_error(
                "Cargo home holds a registry cache or unexpected registry entry",
            ));
        }
        let tag = hash_bounded_with_check(&entry.path(), CACHE_TAG.len() as u64, check_attempt)
            .change_context(ReleaseError::Recipe)?;
        if tag.size != CACHE_TAG.len() as u64
            || tag.sha256 != hex::encode(Sha256::digest(CACHE_TAG))
        {
            return Err(recipe_error(
                "Cargo registry cache tag differs from exact Cargo bookkeeping",
            ));
        }
    }
    let after = fs::symlink_metadata(&registry).map_err(recipe_error)?;
    if !after.is_dir()
        || stamp(&before) != stamp(&after)
        || stamp(&before) != stamp(&directory.metadata().map_err(recipe_error)?)
    {
        return Err(recipe_error(
            "Cargo registry directory changed during inspection",
        ));
    }
    check_attempt()
}

#[cfg(test)]
mod tests;
