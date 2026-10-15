//! Bounded private snapshots supplied to the verifier child.

use std::fs::{File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::time::Instant;

use error_stack::Report;
use sha2::{Digest as _, Sha256};

use crate::release::artifact::AssetMeasurement;
use crate::release::bounds::{AttemptDeadline, Cancellation};
use crate::release::error::{ReleaseError, report};

fn checked_transfer(
    path: &Path,
    mut destination: Option<&mut File>,
    limit: u64,
    deadline: AttemptDeadline,
    cancellation: &Cancellation,
) -> Result<AssetMeasurement, Report<ReleaseError>> {
    let mut input = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|error| report(ReleaseError::Asset, error))?;
    let metadata = input
        .metadata()
        .map_err(|error| report(ReleaseError::Asset, error))?;
    if !metadata.is_file() || metadata.nlink() != 1 || metadata.len() > limit {
        return Err(report(
            ReleaseError::Asset,
            "gh input is not a bounded regular file",
        ));
    }
    let mut hasher = Sha256::new();
    let mut length = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        if cancellation.is_cancelled() {
            return Err(report(
                ReleaseError::Cancelled,
                "gh input snapshot cancelled",
            ));
        }
        if Instant::now() >= deadline.instant() {
            return Err(report(
                ReleaseError::Timeout,
                "gh input snapshot exceeded deadline",
            ));
        }
        let read = input
            .read(&mut buffer)
            .map_err(|error| report(ReleaseError::Asset, error))?;
        if read == 0 {
            break;
        }
        length += read as u64;
        if length > limit {
            return Err(report(
                ReleaseError::Asset,
                "gh input exceeds its role limit",
            ));
        }
        let chunk = &buffer[..read];
        hasher.update(chunk);
        if let Some(output) = destination.as_deref_mut() {
            output
                .write_all(chunk)
                .map_err(|error| report(ReleaseError::Asset, error))?;
        }
    }
    Ok(AssetMeasurement::declared(
        &hex::encode(hasher.finalize()),
        length,
    ))
}

/// Copies a bounded file into an attempt-private directory and measures it.
///
/// # Errors
///
/// Returns an asset, cancellation or timeout error for failed snapshots.
pub(super) fn snapshot(
    path: &Path,
    directory: &Path,
    limit: u64,
    deadline: AttemptDeadline,
    cancellation: &Cancellation,
) -> Result<(PathBuf, AssetMeasurement), Report<ReleaseError>> {
    let name = path
        .file_name()
        .ok_or_else(|| report(ReleaseError::Asset, "gh input has no file name"))?;
    let private = directory.join(name);
    let mut output =
        File::create_new(&private).map_err(|error| report(ReleaseError::Asset, error))?;
    let measurement = checked_transfer(path, Some(&mut output), limit, deadline, cancellation)?;
    Ok((private, measurement))
}

/// Requires the current file to match its pre-invocation snapshot.
///
/// # Errors
///
/// Returns an asset error if a substitution or read failure occurs.
pub(super) fn unchanged(
    path: &Path,
    expected: &AssetMeasurement,
    limit: u64,
    deadline: AttemptDeadline,
    cancellation: &Cancellation,
) -> Result<(), Report<ReleaseError>> {
    if &checked_transfer(path, None, limit, deadline, cancellation)? != expected {
        return Err(report(
            ReleaseError::Asset,
            "gh input changed during verification",
        ));
    }
    Ok(())
}
