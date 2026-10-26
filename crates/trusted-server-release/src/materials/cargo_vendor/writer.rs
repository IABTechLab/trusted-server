//! Writes one Cargo directory-source package and its `.cargo-checksum.json`.
//!
//! The file format is the one `cargo vendor` writes and Cargo's directory
//! source reads: `{"files":{"<path>":"<sha256>"},"package":<sha256|null>}`.
//! Every listed file is rehashed by Cargo when the package is used; `package`
//! must equal the `Cargo.lock` checksum for registry crates and is `null` for
//! Git packages.

use std::collections::BTreeMap;
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

use error_stack::Report;
use serde::Serialize;
use sha2::{Digest as _, Sha256};

use crate::error::ReleaseError;
use crate::materials::material_error;

/// Reserved name Cargo reads the checksums from.
pub(super) const CHECKSUM_FILE: &str = ".cargo-checksum.json";
/// Longest accepted package-relative path.
const PATH_LIMIT: usize = 4096;

#[derive(Serialize)]
struct Checksum<'a> {
    files: &'a BTreeMap<String, String>,
    package: Option<&'a str>,
}

/// One package directory under the vendor root, being written.
pub(super) struct PackageWriter {
    directory: PathBuf,
    files: BTreeMap<String, String>,
}

/// Requires a nonempty `/`-separated path of plain components.
fn check_relative(relative: &str) -> Result<(), Report<ReleaseError>> {
    let plain = !relative.is_empty()
        && relative.len() <= PATH_LIMIT
        && relative.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && !part.contains(['\\', '\0', ':'])
                && !part.chars().any(char::is_control)
        });
    if !plain || relative == CHECKSUM_FILE {
        return Err(material_error(format!(
            "vendored path {relative:?} is unsafe or reserved"
        )));
    }
    Ok(())
}

impl PackageWriter {
    /// Creates `vendor/<directory_name>`, which must not exist yet.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Material`] when the directory exists already
    /// (two locked packages with one name and version) or cannot be created.
    pub(super) fn create(
        vendor: &Path,
        directory_name: &str,
    ) -> Result<Self, Report<ReleaseError>> {
        let directory = vendor.join(directory_name);
        DirBuilder::new()
            .mode(0o755)
            .create(&directory)
            .map_err(|error| {
                material_error(format!(
                    "vendor directory {directory_name} cannot be created exclusively: {error}"
                ))
            })?;
        Ok(Self {
            directory,
            files: BTreeMap::new(),
        })
    }

    /// Writes one file exactly once and records its SHA-256.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Material`] for an unsafe or reserved path, a
    /// repeated path, or a failed write.
    pub(super) fn write(
        &mut self,
        relative: &str,
        bytes: &[u8],
        executable: bool,
    ) -> Result<(), Report<ReleaseError>> {
        check_relative(relative)?;
        if self.files.contains_key(relative) {
            return Err(material_error(format!(
                "vendored path {relative} is repeated"
            )));
        }
        let path = self.directory.join(relative);
        if let Some(parent) = path.parent() {
            DirBuilder::new()
                .recursive(true)
                .mode(0o755)
                .create(parent)
                .map_err(material_error)?;
        }
        let mode = if executable { 0o755 } else { 0o644 };
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&path)
            .map_err(material_error)?;
        file.write_all(bytes).map_err(material_error)?;
        file.set_permissions(fs::Permissions::from_mode(mode))
            .map_err(material_error)?;
        file.sync_all().map_err(material_error)?;
        self.files
            .insert(relative.to_owned(), hex::encode(Sha256::digest(bytes)));
        Ok(())
    }

    /// Returns the recorded SHA-256 of a written file.
    pub(super) fn digest_of(&self, relative: &str) -> Option<&str> {
        self.files.get(relative).map(String::as_str)
    }

    /// Writes `.cargo-checksum.json` and returns the written file digests.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Material`] when the file cannot be written.
    pub(super) fn finish(
        self,
        package_checksum: Option<&str>,
    ) -> Result<BTreeMap<String, String>, Report<ReleaseError>> {
        let body = serde_json::to_vec(&Checksum {
            files: &self.files,
            package: package_checksum,
        })
        .map_err(material_error)?;
        let path = self.directory.join(CHECKSUM_FILE);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&path)
            .map_err(material_error)?;
        file.write_all(&body).map_err(material_error)?;
        file.set_permissions(fs::Permissions::from_mode(0o644))
            .map_err(material_error)?;
        file.sync_all().map_err(material_error)?;
        Ok(self.files)
    }
}
