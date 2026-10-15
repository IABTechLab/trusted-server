//! Original dependency identities derived from the independently approved source.
//!
//! Lockfile checksums authenticate original archive bytes. They do not prove
//! extraction, installation, lifecycle execution, tool qualification or JS
//! build completion.

mod archive;
mod locked;

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Instant;

use error_stack::Report;
use sha2::{Digest as _, Sha256};

use crate::documents::Role;
use crate::error::ReleaseError;
use crate::process::Cancellation;
use crate::source::ApprovedSource;
pub(crate) use archive::VerifiedOriginal;
use locked::{CargoMaterials, NpmMaterial, parse_cargo, parse_npm};

const NPM_LOCK: &str = "crates/trusted-server-js/lib/package-lock.json";

fn material_error(detail: impl std::fmt::Display) -> Report<ReleaseError> {
    Report::new(ReleaseError::Material).attach(detail.to_string())
}

/// Expected original dependency identities, bound to approved lockfile bytes.
///
/// This is not a completed installation or a qualified tool selection.
pub(crate) struct LockedMaterials {
    source_commit: String,
    cargo_lock_sha256: String,
    npm_lock_sha256: String,
    cargo: CargoMaterials,
    npm: BTreeMap<String, NpmMaterial>,
}

impl LockedMaterials {
    /// Loads only the approved tracked lockfiles and derives their identities.
    ///
    /// # Errors
    ///
    /// Returns a material/source/document error if either lockfile changed,
    /// has an unsupported format or cannot identify an original acquisition.
    pub(crate) fn from_source(source: &ApprovedSource) -> Result<Self, Report<ReleaseError>> {
        let cargo = source.read_tracked("Cargo.lock", Role::Metadata)?;
        let npm = source.read_tracked(NPM_LOCK, Role::Metadata)?;
        Ok(Self {
            source_commit: source.commit().as_str().to_owned(),
            cargo_lock_sha256: hex::encode(Sha256::digest(&cargo)),
            npm_lock_sha256: hex::encode(Sha256::digest(&npm)),
            cargo: parse_cargo(&cargo)?,
            npm: parse_npm(&npm)?,
        })
    }

    /// Verifies an original npm archive selected by its locked installation path.
    ///
    /// Optional packages receive the same checksum checks as every other package.
    ///
    /// # Errors
    ///
    /// Returns a material error for an unknown location or changed archive.
    pub(crate) fn verify_npm(
        &self,
        location: &str,
        path: &Path,
        deadline: Instant,
        cancellation: &Cancellation,
    ) -> Result<VerifiedOriginal, Report<ReleaseError>> {
        let locked = self
            .npm
            .get(location)
            .ok_or_else(|| material_error("npm archive is not in the approved lockfile"))?;
        archive::verify(
            path,
            archive::Expected::Sha512(&locked.sha512),
            &self.source_commit,
            &format!("npm:{location}"),
            deadline,
            cancellation,
        )
    }

    /// Verifies an original registry crate selected by its exact locked identity.
    ///
    /// # Errors
    ///
    /// Returns a material error for an unknown identity or changed archive.
    pub(crate) fn verify_registry(
        &self,
        name: &str,
        version: &str,
        path: &Path,
        deadline: Instant,
        cancellation: &Cancellation,
    ) -> Result<VerifiedOriginal, Report<ReleaseError>> {
        let locked = self
            .cargo
            .registry
            .get(&(name.to_owned(), version.to_owned()))
            .ok_or_else(|| material_error("crate archive is not in the approved lockfile"))?;
        archive::verify(
            path,
            archive::Expected::Sha256(&locked.sha256),
            &self.source_commit,
            &format!("crate:{name}@{version}"),
            deadline,
            cancellation,
        )
    }
}

#[cfg(test)]
mod tests;
