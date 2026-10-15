//! Checks original bytes and retains a private copy, without extracting them.

use std::fs::{File, Metadata, OpenOptions};
use std::io::{Read as _, Seek as _, Write};
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::path::Path;
use std::time::Instant;

use error_stack::Report;
use sha2::{Digest as _, Sha256, Sha512};

use super::material_error;
use crate::error::ReleaseError;
use crate::process::Cancellation;

/// Original dependency archives are distinct from toolchain distributions.
/// The current npm/crate material stage accepts at most 64 MiB per original.
const ARCHIVE_LIMIT: u64 = 64 * 1024 * 1024;

#[derive(Clone, Copy)]
pub(super) enum Expected<'a> {
    Sha256(&'a str),
    Sha512(&'a [u8; 64]),
}

fn stamp(metadata: &Metadata) -> (u64, u64, u64, u64, i64, i64, i64, i64) {
    (
        metadata.dev(),
        metadata.ino(),
        metadata.nlink(),
        metadata.len(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec(),
    )
}

/// Exact verified original bytes, retained independently of their input path.
///
/// No deserialization or caller-supplied digest constructor is available.
pub(crate) struct VerifiedOriginal {
    file: File,
    source_commit: String,
    identity: String,
    sha256: String,
    length: u64,
    deadline: Instant,
    cancellation: Cancellation,
}

fn check_limits(
    deadline: Instant,
    cancellation: &Cancellation,
) -> Result<(), Report<ReleaseError>> {
    if cancellation.is_cancelled() || Instant::now() >= deadline {
        return Err(material_error(
            "original material verification cancelled or timed out",
        ));
    }
    Ok(())
}

impl VerifiedOriginal {
    pub(super) fn check(&self) -> Result<(), Report<ReleaseError>> {
        check_limits(self.deadline, &self.cancellation)
    }

    /// Copies the retained original after checking its exact bytes again.
    ///
    /// # Errors
    ///
    /// Returns a material error if retained bytes changed or copying fails.
    pub(crate) fn copy_verified(
        &mut self,
        destination: &mut impl Write,
    ) -> Result<(), Report<ReleaseError>> {
        check_limits(self.deadline, &self.cancellation)?;
        self.file.rewind().map_err(material_error)?;
        let mut hasher = Sha256::new();
        let mut length = 0_u64;
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            check_limits(self.deadline, &self.cancellation)?;
            let count = self.file.read(&mut buffer).map_err(material_error)?;
            if count == 0 {
                break;
            }
            length += count as u64;
            if length > self.length {
                return Err(material_error("retained original grew"));
            }
            hasher.update(&buffer[..count]);
        }
        if length != self.length || hex::encode(hasher.finalize()) != self.sha256 {
            return Err(material_error("retained original changed"));
        }
        self.file.rewind().map_err(material_error)?;
        let mut copied = 0_u64;
        loop {
            check_limits(self.deadline, &self.cancellation)?;
            let count = self.file.read(&mut buffer).map_err(material_error)?;
            if count == 0 {
                break;
            }
            copied += count as u64;
            if copied > self.length {
                return Err(material_error("retained original grew while copying"));
            }
            destination
                .write_all(&buffer[..count])
                .map_err(material_error)?;
        }
        if copied != self.length {
            return Err(material_error("retained original changed while copying"));
        }
        check_limits(self.deadline, &self.cancellation)?;
        Ok(())
    }

    /// Returns the approved source that supplies this archive's expected identity.
    pub(crate) fn source_commit(&self) -> &str {
        &self.source_commit
    }

    /// Returns the original's source-bound package or installation identity.
    pub(crate) fn identity(&self) -> &str {
        &self.identity
    }

    /// Returns the SHA-256 of the exact retained distribution bytes.
    pub(crate) fn sha256(&self) -> &str {
        &self.sha256
    }
}

pub(super) fn verify(
    path: &Path,
    expected: Expected<'_>,
    source_commit: &str,
    identity: &str,
    deadline: Instant,
    cancellation: &Cancellation,
) -> Result<VerifiedOriginal, Report<ReleaseError>> {
    check_limits(deadline, cancellation)?;
    let mut input = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .map_err(material_error)?;
    let before = input.metadata().map_err(material_error)?;
    if !before.is_file() || before.nlink() != 1 || before.len() > ARCHIVE_LIMIT {
        return Err(material_error(
            "original must be a bounded regular single-link file",
        ));
    }
    let mut file = tempfile::tempfile().map_err(material_error)?;
    let mut sha256 = Sha256::new();
    let mut sha512 = Sha512::new();
    let mut length = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        check_limits(deadline, cancellation)?;
        let count = input.read(&mut buffer).map_err(material_error)?;
        if count == 0 {
            break;
        }
        length += count as u64;
        if length > ARCHIVE_LIMIT {
            return Err(material_error("original exceeds archive limit"));
        }
        sha256.update(&buffer[..count]);
        sha512.update(&buffer[..count]);
        file.write_all(&buffer[..count]).map_err(material_error)?;
    }
    if length != before.len() || stamp(&before) != stamp(&input.metadata().map_err(material_error)?)
    {
        return Err(material_error(
            "original changed while reading its held descriptor",
        ));
    }
    let digest = hex::encode(sha256.finalize());
    let matches = match expected {
        Expected::Sha256(wanted) => digest == wanted,
        Expected::Sha512(wanted) => sha512.finalize()[..] == wanted[..],
    };
    if !matches {
        return Err(material_error(
            "original archive differs from its approved lockfile identity",
        ));
    }
    check_limits(deadline, cancellation)?;
    Ok(VerifiedOriginal {
        file,
        source_commit: source_commit.to_owned(),
        identity: identity.to_owned(),
        sha256: digest,
        length,
        deadline,
        cancellation: cancellation.clone(),
    })
}
