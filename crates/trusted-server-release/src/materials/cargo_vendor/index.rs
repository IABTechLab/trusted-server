//! Package identities recorded only after verified originals finish vendoring.

use serde::Serialize;

/// Original material behind one prepared Cargo directory-source package.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum CargoOrigin {
    /// A distribution verified against the approved lock checksum.
    Registry {
        /// SHA-256 of the original crate archive.
        archive_sha256: String,
    },
    /// A package extracted from a verified Git object closure.
    Git {
        /// Exact locked commit object identity.
        revision: String,
        /// Root tree of the verified repository commit.
        repository_tree: String,
        /// Tree containing the selected package.
        package_tree: String,
        /// Repository-relative package directory; empty for a root package.
        package_path: String,
        /// Committed manifest blob object identity.
        committed_manifest_blob: String,
        /// SHA-256 of the committed manifest bytes before normalization.
        committed_manifest_sha256: String,
        /// SHA-256 of a producer-normalized manifest, when normalization occurred.
        normalized_manifest_sha256: Option<String>,
    },
}

/// Identity of a package written by verified Cargo material preparation.
///
/// Membership is authoritative only through its owning
/// [`PreparedCargoMaterials`](super::PreparedCargoMaterials), whose whole tree
/// must be rehashed before any observed compiler identity is joined to it.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct VerifiedCargoPackage {
    /// Name checked against the approved lock identity.
    pub(super) name: String,
    /// Version checked against the approved lock identity.
    pub(super) version: String,
    /// Complete locked registry or Git source, including any revision.
    pub(super) source: String,
    /// SHA-256 of the manifest written into the directory source.
    pub(super) prepared_manifest_sha256: String,
    /// Verified original identity before any manifest normalization.
    pub(super) origin: CargoOrigin,
}

impl VerifiedCargoPackage {
    /// The package name checked against the locked identity.
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// The package version checked against the locked identity.
    pub(crate) fn version(&self) -> &str {
        &self.version
    }

    /// Complete original Cargo source identity, including a Git revision.
    pub(crate) fn source(&self) -> &str {
        &self.source
    }

    /// SHA-256 of the prepared manifest Cargo reads from the vendor tree.
    pub(crate) fn prepared_manifest_sha256(&self) -> &str {
        &self.prepared_manifest_sha256
    }

    /// Original archive or committed Git package material identity.
    pub(crate) fn origin(&self) -> &CargoOrigin {
        &self.origin
    }
}

#[cfg(test)]
impl VerifiedCargoPackage {
    /// Creates an origin fixture whose prepared bytes are measured separately.
    pub(crate) fn registry_for_tests(
        name: &str,
        version: &str,
        source: &str,
        manifest_sha256: &str,
        archive_sha256: &str,
    ) -> Self {
        Self {
            name: name.to_owned(),
            version: version.to_owned(),
            source: source.to_owned(),
            prepared_manifest_sha256: manifest_sha256.to_owned(),
            origin: CargoOrigin::Registry {
                archive_sha256: archive_sha256.to_owned(),
            },
        }
    }
}

#[cfg(test)]
impl VerifiedCargoPackage {
    /// Replaces a fictional original identity for exact-source join tests.
    pub(crate) fn with_origin_for_tests(mut self, origin: CargoOrigin) -> Self {
        self.origin = origin;
        self
    }
}
