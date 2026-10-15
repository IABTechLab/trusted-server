//! Original dependency identities derived from the independently approved source.
//!
//! Lockfile checksums authenticate original archive bytes. They do not prove
//! extraction, installation, lifecycle execution, tool qualification or JS
//! build completion.

mod archive;
mod extract;
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
pub(crate) use extract::ExtractedOriginal;
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
    /// Extracts a verified npm original into new private state.
    ///
    /// # Errors
    ///
    /// Returns a material error if the original is not bound to this source
    /// and locked location, or its archive violates the extraction policy.
    pub(crate) fn extract_npm(
        &self,
        location: &str,
        original: VerifiedOriginal,
    ) -> Result<ExtractedOriginal, Report<ReleaseError>> {
        let locked = self
            .npm
            .get(location)
            .ok_or_else(|| material_error("unknown npm location"))?;
        self.join_original(&original, &format!("npm:{location}"))?;
        let index_alias = matches!(
            (locked.name.as_str(), locked.version.as_str()),
            ("agent-base", "7.1.4")
                | ("http-proxy-agent", "7.0.2")
                | ("https-proxy-agent", "7.0.6")
        );
        extract::extract(
            original,
            extract::Selection {
                root: "package",
                index_alias,
            },
        )
    }

    /// Extracts a verified registry crate under its exact locked root.
    ///
    /// # Errors
    ///
    /// Returns a material error for a mismatched identity or unsafe archive.
    pub(crate) fn extract_registry(
        &self,
        name: &str,
        version: &str,
        original: VerifiedOriginal,
    ) -> Result<ExtractedOriginal, Report<ReleaseError>> {
        if !self
            .cargo
            .registry
            .contains_key(&(name.to_owned(), version.to_owned()))
        {
            return Err(material_error("unknown registry identity"));
        }
        self.join_original(&original, &format!("crate:{name}@{version}"))?;
        extract::extract(
            original,
            extract::Selection {
                root: &format!("{name}-{version}"),
                index_alias: false,
            },
        )
    }

    fn join_original(
        &self,
        original: &VerifiedOriginal,
        identity: &str,
    ) -> Result<(), Report<ReleaseError>> {
        if original.source_commit() != self.source_commit || original.identity() != identity {
            return Err(material_error(
                "original is not bound to this approved locked identity",
            ));
        }
        Ok(())
    }

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
