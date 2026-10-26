//! Fresh owned source copies contain approved tracked bytes only.

use std::fs;
use std::io::Write as _;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{PermissionsExt as _, symlink};
use std::path::{Path, PathBuf};
use std::time::Instant;

use error_stack::Report;
use sha2::{Digest as _, Sha256};
use tempfile::TempDir;

use super::{ApprovedSource, Mode, Records, source_error};
use crate::documents::{Role, read_bounded_with_check};
use crate::error::ReleaseError;
use crate::process::Cancellation;

/// Owns one independent working tree, with no inherited Git configuration.
pub(crate) struct FreshSource {
    directory: TempDir,
    records: Records,
    commit: String,
    deadline: Instant,
    cancellation: Cancellation,
}

/// A source integrity check that preserves the owning preparation attempt.
pub(crate) struct FreshSourceVerification {
    root: PathBuf,
    records: Records,
    deadline: Instant,
    cancellation: Cancellation,
}

impl FreshSourceVerification {
    /// Rechecks the complete tracked source and permitted output exclusions.
    ///
    /// # Errors
    /// Rejects changed or unexpected files, aliases and expired attempts.
    pub(crate) fn verify_unchanged(&self, outputs: &[&str]) -> Result<(), Report<ReleaseError>> {
        verify_source(
            &self.root,
            &self.records,
            self.deadline,
            &self.cancellation,
            outputs,
        )
    }

    /// Expires the original attempt without waiting in deadline regression tests.
    #[cfg(test)]
    pub(crate) fn expire_for_tests(&mut self) {
        self.deadline = Instant::now();
    }
}

/// Creates an owned checkout whose path agrees with canonical boundary roots.
///
/// Resolve the parent before allocation so `TempDir` retains cleanup ownership
/// under the same path, including on macOS where `/var` aliases `/private/var`.
///
/// # Errors
///
/// Rejects an inaccessible parent or a failed private directory allocation.
pub(super) fn fresh_directory(parent: &Path) -> Result<TempDir, Report<ReleaseError>> {
    let parent = fs::canonicalize(parent).map_err(|error| source_error(error.to_string()))?;
    tempfile::tempdir_in(parent).map_err(|error| source_error(error.to_string()))
}

impl FreshSource {
    /// Captures the original source records and shared preparation controls.
    pub(crate) fn verification(&self) -> FreshSourceVerification {
        FreshSourceVerification {
            root: self.root().to_owned(),
            records: self.records.clone(),
            deadline: self.deadline,
            cancellation: self.cancellation.clone(),
        }
    }

    /// Removes the owned tree and reports cleanup failure to the recipe.
    ///
    /// # Errors
    ///
    /// Returns an error if the private source copy cannot be removed. Cleanup
    /// remains available after cancellation or expiration.
    pub(crate) fn close(self) -> Result<(), Report<ReleaseError>> {
        self.directory
            .close()
            .map_err(|error| source_error(error.to_string()))
    }
    /// Returns the private tracked-source root.
    pub(crate) fn root(&self) -> &Path {
        self.directory.path()
    }

    /// Returns the source identity whose bytes were copied.
    pub(crate) fn commit(&self) -> &str {
        &self.commit
    }

    /// Returns the complete approved JS entry-point set in sorted order.
    pub(crate) fn module_ids(&self) -> Vec<String> {
        super::js_module_ids(&self.records)
    }

    /// Rechecks every tracked path and rejects unexpected source files.
    ///
    /// # Errors
    ///
    /// Returns an error for changed source, unexpected files or output roots
    /// overlapping approved tracked paths. Output trees are checked separately
    /// by preparation; this method grants no authority to their contents.
    pub(crate) fn verify_unchanged(&self, outputs: &[&str]) -> Result<(), Report<ReleaseError>> {
        verify_source(
            self.root(),
            &self.records,
            self.deadline,
            &self.cancellation,
            outputs,
        )
    }
}

fn verify_source(
    root: &Path,
    records: &Records,
    deadline: Instant,
    cancellation: &Cancellation,
    outputs: &[&str],
) -> Result<(), Report<ReleaseError>> {
    let check = || check_attempt(deadline, cancellation);
    check()?;
    let metadata = fs::symlink_metadata(root).map_err(|error| source_error(error.to_string()))?;
    if !metadata.is_dir() {
        return Err(source_error(
            "fresh source root changed type or became an alias",
        ));
    }
    if fs::symlink_metadata(root.join(".git")).is_ok() {
        return Err(source_error(
            "fresh source must not acquire repository configuration",
        ));
    }
    super::worktree::compare_with_outputs_controlled(root, records, outputs, &check)
}

impl ApprovedSource {
    /// Copies approved tracked bytes into a new independently owned directory.
    ///
    /// # Errors
    ///
    /// Returns an error for changed original source, failed private writes or a
    /// copied path that does not rejoin its approved mode and digest. No cache,
    /// installed dependencies, generated outputs or Git configuration is copied.
    pub(crate) fn materialize(
        &self,
        deadline: Instant,
        cancellation: &Cancellation,
    ) -> Result<FreshSource, Report<ReleaseError>> {
        self.materialize_with_check(deadline, cancellation, &|| {
            check_attempt(deadline, cancellation)
        })
    }

    /// Copies under the same checked attempt; the callback also supports deterministic cancellation fixtures.
    ///
    /// # Errors
    ///
    /// Returns an error for failed attempt checks, changed source or failed private writes.
    pub(super) fn materialize_with_check(
        &self,
        deadline: Instant,
        cancellation: &Cancellation,
        check: &dyn Fn() -> Result<(), Report<ReleaseError>>,
    ) -> Result<FreshSource, Report<ReleaseError>> {
        check()?;
        super::worktree::compare_with_outputs_controlled(
            &self.snapshot.root,
            &self.snapshot.records,
            &[],
            check,
        )?;
        let directory = fresh_directory(&std::env::temp_dir())?;
        for (relative, (mode, expected)) in &self.snapshot.records {
            check()?;
            let destination = directory.path().join(relative);
            fs::create_dir_all(
                destination
                    .parent()
                    .ok_or_else(|| source_error("missing source parent"))?,
            )
            .map_err(|error| source_error(error.to_string()))?;
            match mode {
                Mode::Symlink => {
                    let target = fs::read_link(self.snapshot.root.join(relative))
                        .map_err(|error| source_error(error.to_string()))?;
                    if hex::encode(Sha256::digest(target.as_os_str().as_bytes())) != *expected {
                        return Err(source_error("approved source alias changed during copying"));
                    }
                    symlink(target, destination)
                        .map_err(|error| source_error(error.to_string()))?;
                }
                Mode::Regular | Mode::Executable => {
                    let raw = read_bounded_with_check(
                        &self.snapshot.root.join(relative),
                        Role::Source,
                        check,
                    )?;
                    if raw.sha256 != *expected {
                        return Err(source_error("approved source bytes changed during copying"));
                    }
                    let mut output = fs::File::create_new(&destination)
                        .map_err(|error| source_error(error.to_string()))?;
                    for chunk in raw.bytes.chunks(64 * 1024) {
                        check()?;
                        output
                            .write_all(chunk)
                            .map_err(|error| source_error(error.to_string()))?;
                    }
                    let permissions = if *mode == Mode::Executable {
                        0o700
                    } else {
                        0o600
                    };
                    fs::set_permissions(destination, fs::Permissions::from_mode(permissions))
                        .map_err(|error| source_error(error.to_string()))?;
                }
            }
        }
        super::worktree::compare_with_outputs_controlled(
            &self.snapshot.root,
            &self.snapshot.records,
            &[],
            check,
        )?;
        let fresh = FreshSource {
            directory,
            records: self.snapshot.records.clone(),
            commit: self.commit.as_str().to_owned(),
            deadline,
            cancellation: cancellation.clone(),
        };
        check()?;
        fresh.verify_unchanged(&[])?;
        Ok(fresh)
    }
}

fn check_attempt(
    deadline: Instant,
    cancellation: &Cancellation,
) -> Result<(), Report<ReleaseError>> {
    if cancellation.is_cancelled() || Instant::now() >= deadline {
        return Err(source_error("source preparation cancelled or timed out"));
    }
    Ok(())
}
