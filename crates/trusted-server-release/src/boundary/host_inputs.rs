//! Host-owned Docker client inputs pinned for the life of one boundary.
//!
//! The reviewed seccomp baseline, the derived effective profile and an empty
//! Docker client configuration directory live in a fresh private directory
//! that is disjoint from every mounted root (checked before anything is
//! created). The two profiles, the fixed client executable and the empty
//! configuration are rehashed before every Docker host command, so a change
//! between commands rejects the run instead of silently applying.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{Read as _, Write as _};
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

use error_stack::Report;
use sha2::{Digest as _, Sha256};
use tempfile::TempDir;

use super::roots::{overlaps, validate_path_text};
use super::seccomp;
use crate::error::ReleaseError;

const BASELINE_NAME: &str = "seccomp-baseline.json";
const EFFECTIVE_NAME: &str = "seccomp-effective.json";
const CONFIG_NAME: &str = "docker-config";
const READ_ONLY_FILE: u32 = 0o444;
const READ_ONLY_DIRECTORY: u32 = 0o500;
const HASH_CHUNK: usize = 64 * 1024;

fn input_error(message: impl std::fmt::Display) -> Report<ReleaseError> {
    Report::new(ReleaseError::Boundary).attach(message.to_string())
}

/// Hashes a regular file without following a final symlink.
fn hash_regular(path: &Path) -> Result<String, Report<ReleaseError>> {
    let mut file = File::options()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|error| input_error(format!("cannot open {}: {error}", path.display())))?;
    let metadata = file
        .metadata()
        .map_err(|error| input_error(format!("cannot stat {}: {error}", path.display())))?;
    if !metadata.is_file() {
        return Err(input_error(format!(
            "{} is not a regular file",
            path.display()
        )));
    }
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; HASH_CHUNK];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| input_error(format!("cannot read {}: {error}", path.display())))?;
        let Some(chunk) = buffer.get(..read).filter(|chunk| !chunk.is_empty()) else {
            return Ok(hex::encode(hasher.finalize()));
        };
        hasher.update(chunk);
    }
}

/// A file whose digest must not change.
#[derive(Debug)]
struct PinnedFile {
    path: PathBuf,
    sha256: String,
}

impl PinnedFile {
    fn verify(&self) -> Result<(), Report<ReleaseError>> {
        if hash_regular(&self.path)? != self.sha256 {
            return Err(input_error(format!(
                "pinned Docker input {} changed",
                self.path.display()
            )));
        }
        Ok(())
    }
}

/// Identity of the empty configuration directory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DirectoryIdentity {
    device: u64,
    inode: u64,
    mode: u32,
}

fn directory_identity(path: &Path) -> Result<DirectoryIdentity, Report<ReleaseError>> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| input_error(format!("cannot inspect {}: {error}", path.display())))?;
    if !metadata.is_dir() {
        return Err(input_error(format!(
            "{} is not a directory",
            path.display()
        )));
    }
    Ok(DirectoryIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
        mode: metadata.mode(),
    })
}

fn entry_names(path: &Path) -> Result<BTreeSet<OsString>, Report<ReleaseError>> {
    let list_error =
        |error: std::io::Error| input_error(format!("cannot list {}: {error}", path.display()));
    let mut names = BTreeSet::new();
    for entry in fs::read_dir(path).map_err(list_error)? {
        names.insert(entry.map_err(list_error)?.file_name());
    }
    Ok(names)
}

fn write_read_only(path: &Path, bytes: &[u8]) -> Result<PinnedFile, Report<ReleaseError>> {
    let write_error =
        |error: std::io::Error| input_error(format!("cannot write {}: {error}", path.display()));
    let mut file = File::options()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(write_error)?;
    file.write_all(bytes).map_err(write_error)?;
    file.sync_all().map_err(write_error)?;
    fs::set_permissions(path, fs::Permissions::from_mode(READ_ONLY_FILE)).map_err(write_error)?;
    let pinned = PinnedFile {
        path: path.to_path_buf(),
        sha256: seccomp::sha256_hex(bytes),
    };
    pinned.verify()?;
    Ok(pinned)
}

/// Rejects a state parent that overlaps any mounted root.
///
/// # Errors
///
/// Returns [`ReleaseError::Boundary`] when `parent` is invalid, missing, or
/// equal to, inside or above any of `mounted`.
pub(crate) fn check_disjoint(
    parent: &Path,
    mounted: &[&Path],
) -> Result<PathBuf, Report<ReleaseError>> {
    validate_path_text(parent)?;
    let canonical = fs::canonicalize(parent)
        .map_err(|error| input_error(format!("cannot resolve Docker state parent: {error}")))?;
    validate_path_text(&canonical)?;
    if let Some(root) = mounted.iter().find(|root| overlaps(&canonical, root)) {
        return Err(input_error(format!(
            "Docker host inputs at {} overlap mounted root {}",
            canonical.display(),
            root.display()
        )));
    }
    Ok(canonical)
}

/// Pinned host-owned inputs for every Docker client invocation.
#[derive(Debug)]
pub(crate) struct HostInputs {
    directory: TempDir,
    client: PinnedFile,
    baseline: PinnedFile,
    effective: PinnedFile,
    config: PathBuf,
    config_identity: DirectoryIdentity,
}

impl HostInputs {
    /// Scans, then creates the inputs in a fresh directory under `parent`.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Boundary`] when `parent` overlaps a mounted
    /// root, `client` is not a canonical regular file, the seccomp baseline
    /// fails its reviewed digest, or any input cannot be written and pinned.
    pub(crate) fn create(
        parent: &Path,
        client: &Path,
        mounted: &[&Path],
    ) -> Result<Self, Report<ReleaseError>> {
        let parent = check_disjoint(parent, mounted)?;
        validate_path_text(client)?;
        let canonical_client = fs::canonicalize(client)
            .map_err(|error| input_error(format!("cannot resolve Docker client: {error}")))?;
        if canonical_client != client {
            return Err(input_error(
                "Docker client path must be canonical, not an alias",
            ));
        }
        if mounted.iter().any(|root| overlaps(&canonical_client, root)) {
            return Err(input_error(
                "Docker client must be outside every mounted root",
            ));
        }
        let client = PinnedFile {
            sha256: hash_regular(client)?,
            path: canonical_client,
        };
        let directory = tempfile::Builder::new()
            .prefix("ts-boundary-host-")
            .tempdir_in(&parent)
            .map_err(|error| input_error(format!("cannot create Docker host inputs: {error}")))?;
        if !entry_names(directory.path())?.is_empty() {
            return Err(input_error(
                "fresh Docker host input directory is not empty",
            ));
        }
        let baseline_bytes = seccomp::baseline()?;
        let baseline = write_read_only(&directory.path().join(BASELINE_NAME), baseline_bytes)?;
        let effective_bytes = seccomp::effective(baseline_bytes)?;
        let effective = write_read_only(&directory.path().join(EFFECTIVE_NAME), &effective_bytes)?;
        let config = directory.path().join(CONFIG_NAME);
        fs::create_dir(&config)
            .and_then(|()| {
                fs::set_permissions(&config, fs::Permissions::from_mode(READ_ONLY_DIRECTORY))
            })
            .map_err(|error| input_error(format!("cannot create empty Docker config: {error}")))?;
        let config_identity = directory_identity(&config)?;
        let inputs = Self {
            directory,
            client,
            baseline,
            effective,
            config,
            config_identity,
        };
        inputs.verify()?;
        Ok(inputs)
    }

    /// Rehashes every input; called before every Docker host command.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Boundary`] when the client or either profile
    /// changed, the configuration directory was replaced or is not empty, or
    /// the input directory gained or lost an entry.
    pub(crate) fn verify(&self) -> Result<(), Report<ReleaseError>> {
        self.verify_control()?;
        self.baseline.verify()?;
        self.effective.verify()?;
        Ok(())
    }

    /// Verifies the inputs used by every host operation, including cleanup.
    ///
    /// Cleanup does not consume seccomp profiles, so it may still stop a
    /// container after profile tampering. It must never execute a changed
    /// client or consume a changed configuration.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Boundary`] when a control input changed.
    pub(crate) fn verify_control(&self) -> Result<(), Report<ReleaseError>> {
        self.client.verify()?;
        if directory_identity(&self.config)? != self.config_identity {
            return Err(input_error("empty Docker config directory was replaced"));
        }
        if !entry_names(&self.config)?.is_empty() {
            return Err(input_error("Docker config directory is no longer empty"));
        }
        let expected: BTreeSet<OsString> = [BASELINE_NAME, EFFECTIVE_NAME, CONFIG_NAME]
            .into_iter()
            .map(OsString::from)
            .collect();
        if entry_names(self.directory.path())? != expected {
            return Err(input_error(
                "Docker host input directory gained or lost entries",
            ));
        }
        Ok(())
    }

    /// Returns the private host input directory.
    #[must_use]
    pub(crate) fn directory(&self) -> &Path {
        self.directory.path()
    }

    /// Returns the pinned client executable.
    #[must_use]
    pub(crate) fn client(&self) -> &Path {
        &self.client.path
    }

    /// Returns the empty client configuration directory.
    #[must_use]
    pub(crate) fn config(&self) -> &Path {
        &self.config
    }

    /// Returns the effective seccomp profile path.
    #[must_use]
    pub(crate) fn effective_profile(&self) -> &Path {
        &self.effective.path
    }

    /// Returns the client, baseline and effective digests, in that order.
    #[must_use]
    pub(crate) fn digests(&self) -> [&str; 3] {
        [
            &self.client.sha256,
            &self.baseline.sha256,
            &self.effective.sha256,
        ]
    }
}
