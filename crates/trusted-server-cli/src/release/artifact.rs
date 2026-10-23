//! Retained release asset bytes and the read-only verification report.
//!
//! Downloads stream into a private, unlinked temporary file while the role
//! byte limit is enforced chunk by chunk, so an oversize executable is
//! rejected mid-transfer instead of after it was fully written. A
//! [`RetainedAsset`] is only a measured download; a [`VerifiedReleaseAsset`]
//! is constructed exclusively from a [`PublicationVerified`] state that names
//! the same asset, digest and length, and is re-hashed before every later use.
//!
//! Nothing here authorizes deployment: [`VerificationReport`] is a read-only
//! summary, and action-scoped authorization belongs to a later integration.

use std::fs::{File, OpenOptions};
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

use error_stack::Report;
use sha2::{Digest as _, Sha256};

use super::bounds::{self, AttemptDeadline, Cancellation};
use super::error::{ReleaseError, report};
use super::evidence::ComparisonOutcome;
use super::github::PublicationVerified;

/// Read buffer used when re-hashing a retained file.
const REHASH_CHUNK: usize = 64 * 1024;

/// The byte-limit class of a release asset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AssetRole {
    /// A cohort executable (512 MiB).
    Executable,
    /// A Sigstore bundle or build companion (32 MiB).
    Companion,
    /// An SPDX SBOM document (16 MiB).
    Spdx,
    /// The index, JS inventory or `SHA256SUMS` (1 MiB).
    Metadata,
}

impl AssetRole {
    /// Returns the R-BOUNDS byte limit for this role.
    pub(crate) const fn limit(self) -> u64 {
        match self {
            Self::Executable => bounds::EXECUTABLE_BYTES,
            Self::Companion => bounds::COMPANION_BYTES,
            Self::Spdx => bounds::SPDX_BYTES,
            Self::Metadata => bounds::METADATA_BYTES,
        }
    }
}

/// The exact SHA-256 and length of distributed bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AssetMeasurement {
    sha256: String,
    length: u64,
}

impl AssetMeasurement {
    /// Measures exact in-memory bytes without reserializing them.
    pub(crate) fn of_bytes(bytes: &[u8]) -> Self {
        Self {
            sha256: hex::encode(Sha256::digest(bytes)),
            length: bytes.len() as u64,
        }
    }

    /// Records a length and digest declared by already verified evidence.
    ///
    /// Used only for expectations; retained bytes are always re-hashed
    /// before a [`VerifiedReleaseAsset`] is constructed.
    pub(crate) fn declared(sha256: &str, length: u64) -> Self {
        Self {
            sha256: sha256.to_owned(),
            length,
        }
    }

    /// Returns the lowercase hexadecimal SHA-256.
    pub(crate) fn sha256(&self) -> &str {
        &self.sha256
    }

    /// Returns the byte length.
    pub(crate) fn length(&self) -> u64 {
        self.length
    }
}

/// A download in progress into a private retained file.
#[derive(Debug)]
pub(crate) struct RetainedDownload {
    name: String,
    role: AssetRole,
    file: File,
    hasher: Sha256,
    length: u64,
}

impl RetainedDownload {
    /// Starts a download into a new anonymous, owner-only temporary file.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Asset`] if the private file cannot be created.
    pub(crate) fn start(name: &str, role: AssetRole) -> Result<Self, Report<ReleaseError>> {
        let file = tempfile::tempfile().map_err(|source| report(ReleaseError::Asset, source))?;
        Ok(Self {
            name: name.to_owned(),
            role,
            file,
            hasher: Sha256::new(),
            length: 0,
        })
    }

    /// Appends one received chunk, enforcing the role limit before writing.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Asset`] once the total would exceed the role
    /// limit (the transfer must then be aborted) or the write fails.
    pub(crate) fn push(&mut self, chunk: &[u8]) -> Result<(), Report<ReleaseError>> {
        let remaining = self.role.limit().saturating_sub(self.length);
        if chunk.len() as u64 > remaining {
            return Err(report(
                ReleaseError::Asset,
                format!(
                    "asset `{}` exceeds its {} byte role limit; download aborted",
                    self.name,
                    self.role.limit()
                ),
            ));
        }
        self.file
            .write_all(chunk)
            .map_err(|source| report(ReleaseError::Asset, source))?;
        self.hasher.update(chunk);
        self.length += chunk.len() as u64;
        Ok(())
    }

    /// Completes the download and returns the measured retained asset.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Asset`] if the file cannot be flushed.
    pub(crate) fn finish(mut self) -> Result<RetainedAsset, Report<ReleaseError>> {
        self.file
            .flush()
            .map_err(|source| report(ReleaseError::Asset, source))?;
        Ok(RetainedAsset {
            name: self.name,
            role: self.role,
            file: self.file,
            measurement: AssetMeasurement {
                sha256: hex::encode(self.hasher.finalize()),
                length: self.length,
            },
        })
    }
}

/// Streams a response body into a retained file under the role limit.
///
/// A declared `Content-Length` above the limit is rejected before any byte
/// is read; the streamed total is enforced independently while receiving.
///
/// # Errors
///
/// Returns [`ReleaseError::Asset`] for oversize or failed transfers and
/// [`ReleaseError::Timeout`] when the attempt deadline elapses.
pub(crate) async fn stream_into(
    mut response: reqwest::Response,
    mut download: RetainedDownload,
    deadline: AttemptDeadline,
) -> Result<RetainedAsset, Report<ReleaseError>> {
    if response
        .content_length()
        .is_some_and(|length| length > download.role.limit())
    {
        return Err(report(
            ReleaseError::Asset,
            format!(
                "asset `{}` declares more than its role limit",
                download.name
            ),
        ));
    }
    let transfer = async {
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|source| report(ReleaseError::Asset, source.without_url()))?
        {
            download.push(&chunk)?;
        }
        download.finish()
    };
    tokio::time::timeout_at(tokio::time::Instant::from_std(deadline.instant()), transfer)
        .await
        .map_err(|_| {
            report(
                ReleaseError::Timeout,
                "asset download exceeded the deadline",
            )
        })?
}

/// A fully downloaded, measured but not yet verified release asset.
#[derive(Debug)]
pub(crate) struct RetainedAsset {
    name: String,
    role: AssetRole,
    file: File,
    measurement: AssetMeasurement,
}

impl RetainedAsset {
    /// Copies retained bytes to a canonical private filename for gh snapshots.
    ///
    /// # Errors
    ///
    /// Rejects unsafe names, collisions, changed bytes, limits or cancellation.
    pub(crate) fn materialize(
        &mut self,
        directory: &Path,
        deadline: AttemptDeadline,
        cancellation: &Cancellation,
    ) -> Result<PathBuf, Report<ReleaseError>> {
        if !super::documents::safe_name(&self.name) || !directory.is_absolute() {
            return Err(report(
                ReleaseError::Asset,
                "unsafe retained materialization path",
            ));
        }
        let path = directory.join(&self.name);
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .map_err(|error| report(ReleaseError::Asset, error))?;
        self.file
            .seek(SeekFrom::Start(0))
            .map_err(|error| report(ReleaseError::Asset, error))?;
        let mut hasher = Sha256::new();
        let mut length = 0_u64;
        let mut buffer = [0_u8; REHASH_CHUNK];
        loop {
            if cancellation.is_cancelled() {
                return Err(report(ReleaseError::Cancelled, "retained copy cancelled"));
            }
            if Instant::now() >= deadline.instant() {
                return Err(report(
                    ReleaseError::Timeout,
                    "retained copy exceeded deadline",
                ));
            }
            let read = self
                .file
                .read(&mut buffer)
                .map_err(|error| report(ReleaseError::Asset, error))?;
            if read == 0 {
                break;
            }
            length += read as u64;
            if length > self.role.limit() {
                return Err(report(
                    ReleaseError::Asset,
                    "retained copy exceeds role limit",
                ));
            }
            let chunk = &buffer[..read];
            output
                .write_all(chunk)
                .map_err(|error| report(ReleaseError::Asset, error))?;
            hasher.update(chunk);
        }
        let measurement = AssetMeasurement::declared(&hex::encode(hasher.finalize()), length);
        if measurement != self.measurement {
            return Err(report(
                ReleaseError::Asset,
                "retained bytes changed before gh verification",
            ));
        }
        output
            .flush()
            .map_err(|error| report(ReleaseError::Asset, error))?;
        Ok(path)
    }

    /// Returns the release asset name.
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// Returns the measured digest and length.
    pub(crate) fn measurement(&self) -> &AssetMeasurement {
        &self.measurement
    }

    /// Re-reads the retained bytes of a non-executable document after
    /// re-checking their digest.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Asset`] for executables (never buffered whole)
    /// or if the retained bytes changed.
    pub(crate) fn document_bytes(&mut self) -> Result<Vec<u8>, Report<ReleaseError>> {
        if self.role == AssetRole::Executable {
            return Err(report(
                ReleaseError::Asset,
                "executables are verified by streamed digest, not buffered",
            ));
        }
        let bytes = read_bounded(&mut self.file, self.role.limit())?;
        if AssetMeasurement::of_bytes(&bytes) != self.measurement {
            return Err(report(
                ReleaseError::Asset,
                format!("retained asset `{}` changed after download", self.name),
            ));
        }
        Ok(bytes)
    }
}

fn read_bounded(file: &mut File, limit: u64) -> Result<Vec<u8>, Report<ReleaseError>> {
    file.seek(SeekFrom::Start(0))
        .map_err(|source| report(ReleaseError::Asset, source))?;
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| report(ReleaseError::Asset, source))?;
    if bytes.len() as u64 > limit {
        return Err(report(
            ReleaseError::Asset,
            "retained asset grew past its role limit",
        ));
    }
    Ok(bytes)
}

fn rehash(
    file: &mut File,
    limit: u64,
    deadline: AttemptDeadline,
    cancellation: &Cancellation,
) -> Result<AssetMeasurement, Report<ReleaseError>> {
    file.seek(SeekFrom::Start(0))
        .map_err(|source| report(ReleaseError::Asset, source))?;
    let mut hasher = Sha256::new();
    let mut length = 0_u64;
    let mut buffer = vec![0_u8; REHASH_CHUNK];
    loop {
        check_attempt(deadline, cancellation)?;
        let read = file
            .read(&mut buffer)
            .map_err(|source| report(ReleaseError::Asset, source))?;
        if read == 0 {
            break;
        }
        length += read as u64;
        if length > limit {
            return Err(report(
                ReleaseError::Asset,
                "retained asset grew past its role limit",
            ));
        }
        hasher.update(buffer.get(..read).unwrap_or_default());
    }
    Ok(AssetMeasurement {
        sha256: hex::encode(hasher.finalize()),
        length,
    })
}

/// Checks the shared deadline and cancellation before file work or success.
pub(crate) fn check_attempt(
    deadline: AttemptDeadline,
    cancellation: &Cancellation,
) -> Result<(), Report<ReleaseError>> {
    if cancellation.is_cancelled() {
        return Err(report(
            ReleaseError::Cancelled,
            "release verification cancelled",
        ));
    }
    if Instant::now() >= deadline.instant() {
        return Err(report(
            ReleaseError::Timeout,
            "release verification deadline elapsed",
        ));
    }
    Ok(())
}

/// A retained release executable whose bytes match verified, published
/// evidence.
///
/// Constructed only by [`VerifiedReleaseAsset::from_publication_bounded`]; an
/// asserted path or hash cannot build one.
#[derive(Debug)]
pub(crate) struct VerifiedReleaseAsset {
    name: String,
    file: File,
    measurement: AssetMeasurement,
    assurance_profile: String,
    comparison: ComparisonOutcome,
    publication_index: AssetMeasurement,
}

impl VerifiedReleaseAsset {
    /// Test compatibility wrapper; production must pass its attempt bounds.
    #[cfg(test)]
    pub(crate) fn from_publication(
        publication: &PublicationVerified,
        asset: RetainedAsset,
    ) -> Result<Self, Report<ReleaseError>> {
        Self::from_publication_bounded(
            publication,
            asset,
            AttemptDeadline::start(),
            &Cancellation::default(),
        )
    }

    /// Binds a retained executable to the publication-verified evidence.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Asset`] unless the asset is a retained
    /// executable consumed by `publication` whose re-hashed bytes equal the
    /// verified digest and length.
    pub(crate) fn from_publication_bounded(
        publication: &PublicationVerified,
        mut asset: RetainedAsset,
        deadline: AttemptDeadline,
        cancellation: &Cancellation,
    ) -> Result<Self, Report<ReleaseError>> {
        let consumed = publication
            .build()
            .consumed()
            .iter()
            .find(|consumed| consumed.name() == asset.name)
            .ok_or_else(|| {
                report(
                    ReleaseError::Asset,
                    format!("asset `{}` was not consumed by verification", asset.name),
                )
            })?;
        if asset.role != AssetRole::Executable {
            return Err(report(
                ReleaseError::Asset,
                "only retained executables become verified release assets",
            ));
        }
        check_attempt(deadline, cancellation)?;
        let measured = rehash(
            &mut asset.file,
            AssetRole::Executable.limit(),
            deadline,
            cancellation,
        )?;
        if measured != asset.measurement || &measured != consumed.measurement() {
            return Err(report(
                ReleaseError::Asset,
                format!("asset `{}` does not match verified evidence", asset.name),
            ));
        }
        Ok(Self {
            name: asset.name,
            file: asset.file,
            measurement: measured,
            assurance_profile: consumed.assurance_profile().to_owned(),
            comparison: consumed.comparison(),
            publication_index: publication.build().index_measurement().clone(),
        })
    }

    /// Re-hashes the retained bytes; call before every later use.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Asset`] if the retained bytes changed.
    pub(crate) fn rehash_bounded(
        &mut self,
        deadline: AttemptDeadline,
        cancellation: &Cancellation,
    ) -> Result<&AssetMeasurement, Report<ReleaseError>> {
        let measured = rehash(
            &mut self.file,
            AssetRole::Executable.limit(),
            deadline,
            cancellation,
        )?;
        if measured != self.measurement {
            return Err(report(
                ReleaseError::Asset,
                format!("verified asset `{}` changed after verification", self.name),
            ));
        }
        Ok(&self.measurement)
    }

    /// Returns the asset name.
    pub(crate) fn name(&self) -> &str {
        &self.name
    }
}

/// Read-only summary of one complete online verification.
///
/// It is not a deployment authorization and cannot construct one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct VerificationReport {
    pub(crate) consumer_policy_version: u64,
    pub(crate) consumer_policy_sha256: String,
    pub(crate) producer_policy_version: u64,
    pub(crate) producer_policy_sha256: String,
    pub(crate) repository: &'static str,
    pub(crate) tag: String,
    pub(crate) source_commit: String,
    pub(crate) asset: String,
    pub(crate) asset_sha256: String,
    pub(crate) asset_length: u64,
    pub(crate) assurance_profile: String,
    pub(crate) run_invocation: String,
    /// Informational rebuild comparison; never an authorization input.
    pub(crate) comparison: ComparisonOutcome,
}

impl VerificationReport {
    /// Test compatibility wrapper; production must pass its attempt bounds.
    #[cfg(test)]
    pub(crate) fn new(
        publication: &PublicationVerified,
        asset: &mut VerifiedReleaseAsset,
    ) -> Result<Self, Report<ReleaseError>> {
        Self::new_bounded(
            publication,
            asset,
            AttemptDeadline::start(),
            &Cancellation::default(),
        )
    }
    /// Summarizes a publication-verified asset after re-hashing it.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Asset`] if the retained bytes changed.
    pub(crate) fn new_bounded(
        publication: &PublicationVerified,
        asset: &mut VerifiedReleaseAsset,
        deadline: AttemptDeadline,
        cancellation: &Cancellation,
    ) -> Result<Self, Report<ReleaseError>> {
        let measurement = asset.rehash_bounded(deadline, cancellation)?.clone();
        let build = publication.build();
        if &asset.publication_index != build.index_measurement() {
            return Err(report(
                ReleaseError::Asset,
                "asset belongs to another publication",
            ));
        }
        check_attempt(deadline, cancellation)?;
        Ok(Self {
            consumer_policy_version: build.consumer_policy_version(),
            consumer_policy_sha256: build.consumer_policy_sha256().to_owned(),
            producer_policy_version: build.index().policy_version,
            producer_policy_sha256: build.index().policy_sha256.clone(),
            repository: super::policy::REPOSITORY,
            tag: build.release().tag().as_str().to_owned(),
            source_commit: build.release().commit().as_str().to_owned(),
            asset: asset.name.clone(),
            asset_sha256: measurement.sha256().to_owned(),
            asset_length: measurement.length(),
            assurance_profile: asset.assurance_profile.clone(),
            run_invocation: build.run_invocation().to_owned(),
            comparison: asset.comparison,
        })
    }
}
