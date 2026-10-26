//! Exact compiled Cargo identities joined to approved source and verified originals.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use error_stack::Report;
use serde::Serialize;

use super::{CargoObservations, composition_error, native::NativeSource};
use crate::documents::{Role, read_bounded};
use crate::error::ReleaseError;
use crate::materials::{CargoOrigin, PreparedCargoMaterials};
use crate::recipe::ExecutionPaths;
use crate::source::{ApprovedSource, FreshSource, FreshSourceVerification};

/// Approved tracked manifests bound to one independent source checkout.
pub(crate) struct WorkspaceOrigins {
    root: PathBuf,
    source_commit: String,
    cargo_lock_sha256: String,
    manifests: BTreeMap<PathBuf, String>,
    verification: WorkspaceVerification,
}

enum WorkspaceVerification {
    Approved(FreshSourceVerification),
    #[cfg(test)]
    Fixture,
}

impl WorkspaceOrigins {
    /// Rechecks all approved tracked bytes before and after recipe execution.
    ///
    /// # Errors
    /// Rejects changed source, unexpected files and expired preparation attempts.
    pub(crate) fn verify_unchanged(&self) -> Result<(), Report<ReleaseError>> {
        match &self.verification {
            WorkspaceVerification::Approved(verification) => verification.verify_unchanged(&[]),
            #[cfg(test)]
            WorkspaceVerification::Fixture => Ok(()),
        }
    }
    /// Binds approved tracked identities to an unchanged independent checkout.
    ///
    /// # Errors
    /// Rejects a changed checkout, mismatched commit or missing approved lock.
    pub(crate) fn bind(
        approved: &ApprovedSource,
        fresh: &FreshSource,
    ) -> Result<Self, Report<ReleaseError>> {
        fresh.verify_unchanged(&[])?;
        if approved.commit().as_str() != fresh.commit() {
            return Err(composition_error(
                "fresh checkout has a different approved commit",
            ));
        }
        real_path(fresh.root())?;
        Ok(Self {
            root: fresh.root().to_owned(),
            source_commit: fresh.commit().to_owned(),
            cargo_lock_sha256: approved
                .tracked_sha256("Cargo.lock")
                .ok_or_else(|| composition_error("approved source has no Cargo.lock"))?
                .to_owned(),
            manifests: approved.cargo_manifest_digests(),
            verification: WorkspaceVerification::Approved(fresh.verification()),
        })
    }
}

/// Origins for compiled packages only; host paths never enter serialized evidence.
#[derive(Serialize)]
pub(crate) struct VerifiedPackageOrigins {
    source_commit: String,
    materials_sha256: String,
    packages: BTreeMap<String, PackageOrigin>,
    #[serde(skip)]
    native_sources: BTreeMap<String, NativeSource>,
}

/// One exact observed package joined to an approved original.
#[derive(Serialize)]
pub(crate) struct PackageOrigin {
    name: String,
    version: String,
    manifest_path: PathBuf,
    prepared_manifest_sha256: String,
    origin: Original,
}

/// The approved workspace or verified dependency original behind a package.
#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum Original {
    /// A tracked package within the approved source commit.
    Workspace {
        source_commit: String,
        path: PathBuf,
    },
    /// A registry archive or Git object closure verified by material preparation.
    Material {
        source: String,
        original: CargoOrigin,
    },
}

impl VerifiedPackageOrigins {
    /// Returns the approved source commit bound to these package origins.
    pub(crate) fn source_commit(&self) -> &str {
        &self.source_commit
    }

    /// Returns the measured prepared Cargo materials tree identity.
    pub(crate) fn materials_sha256(&self) -> &str {
        &self.materials_sha256
    }

    /// Looks up a verified origin by its exact observed Cargo package ID.
    pub(crate) fn package(&self, package_id: &str) -> Option<&PackageOrigin> {
        self.packages.get(package_id)
    }

    /// Registry archives and prepared roots authorized by the exact-ID join.
    pub(crate) fn native_sources(&self) -> &BTreeMap<String, NativeSource> {
        &self.native_sources
    }
}

impl PackageOrigin {
    /// Returns the package name checked against its verified original.
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// Returns the package version checked against its verified original.
    pub(crate) fn version(&self) -> &str {
        &self.version
    }

    /// Returns the source-relative or prepared vendor-relative manifest path.
    pub(crate) fn relative_manifest_path(&self) -> &Path {
        &self.manifest_path
    }

    /// Returns the measured prepared manifest digest.
    pub(crate) fn prepared_manifest_sha256(&self) -> &str {
        &self.prepared_manifest_sha256
    }

    /// Returns the verified original without reconstructing it from a package name.
    pub(crate) fn origin(&self) -> &Original {
        &self.origin
    }
}

/// Joins observed package IDs to exact measured source identities.
///
/// # Errors
/// Rejects changed materials, unapproved manifests, aliases and mismatched identities.
pub(crate) fn join_origins(
    observed: &CargoObservations,
    workspace: &WorkspaceOrigins,
    materials: &PreparedCargoMaterials,
    paths: &dyn ExecutionPaths,
) -> Result<VerifiedPackageOrigins, Report<ReleaseError>> {
    workspace.verify_unchanged()?;
    if materials.source_commit() != workspace.source_commit
        || materials.cargo_lock_sha256() != workspace.cargo_lock_sha256
    {
        return Err(composition_error(
            "Cargo materials do not belong to the approved source and lock",
        ));
    }
    materials.rehash()?;
    real_path(&workspace.root)?;
    let mut packages = BTreeMap::new();
    let mut native_sources = BTreeMap::new();
    let mut used_manifests = BTreeSet::new();
    for package in &observed.packages {
        super::normal_path(&package.manifest_path)?;
        if !used_manifests.insert(package.manifest_path.clone()) {
            return Err(composition_error(
                "compiled package manifest has ambiguous Cargo identities",
            ));
        }
        let (relative, hash, origin) = if let Some(source) = &package.source {
            let mut candidates = Vec::new();
            for (directory, candidate) in materials.packages() {
                let host = materials.vendor_dir().join(directory).join("Cargo.toml");
                if paths.execution_path(&host)? == package.manifest_path {
                    candidates.push((directory, candidate, host));
                }
            }
            let [(directory, candidate, host)] = candidates.as_slice() else {
                return Err(composition_error(
                    "compiled dependency has no unique prepared manifest",
                ));
            };
            if candidate.name() != package.name
                || candidate.version() != package.version
                || candidate.source() != source
            {
                return Err(composition_error(
                    "compiled dependency differs from its verified original identity",
                ));
            }
            let manifest = measured_manifest(host, candidate.prepared_manifest_sha256())?;
            require_identity(&manifest, &package.name, &package.version, None)?;
            if let CargoOrigin::Registry { archive_sha256 } = candidate.origin() {
                native_sources.insert(
                    package.id.clone(),
                    NativeSource {
                        original_sha256: archive_sha256.clone(),
                        source_root: materials.vendor_dir().join(directory),
                    },
                );
            }
            (
                PathBuf::from("vendor").join(directory).join("Cargo.toml"),
                candidate.prepared_manifest_sha256().to_owned(),
                Original::Material {
                    source: source.clone(),
                    original: candidate.origin().clone(),
                },
            )
        } else {
            let mut candidates = Vec::new();
            for (relative, hash) in &workspace.manifests {
                if paths.execution_path(&workspace.root.join(relative))? == package.manifest_path {
                    candidates.push((relative, hash));
                }
            }
            let [(relative, hash)] = candidates.as_slice() else {
                return Err(composition_error(
                    "compiled workspace package has no unique approved tracked manifest",
                ));
            };
            let manifest = measured_manifest(&workspace.root.join(relative), hash)?;
            require_identity(
                &manifest,
                &package.name,
                &package.version,
                Some((workspace, relative)),
            )?;
            (
                (*relative).clone(),
                (*hash).clone(),
                Original::Workspace {
                    source_commit: workspace.source_commit.clone(),
                    path: (*relative).clone(),
                },
            )
        };
        if packages
            .insert(
                package.id.clone(),
                PackageOrigin {
                    name: package.name.clone(),
                    version: package.version.clone(),
                    manifest_path: relative,
                    prepared_manifest_sha256: hash,
                    origin,
                },
            )
            .is_some()
        {
            return Err(composition_error("compiled Cargo ID is duplicated"));
        }
    }
    materials.rehash()?;
    workspace.verify_unchanged()?;
    Ok(VerifiedPackageOrigins {
        source_commit: workspace.source_commit.clone(),
        materials_sha256: materials.digest().to_owned(),
        packages,
        native_sources,
    })
}

fn real_path(path: &Path) -> Result<(), Report<ReleaseError>> {
    super::normal_path(path)?;
    if fs::canonicalize(path).map_err(composition_error)? != path {
        return Err(composition_error("origin path contains an alias"));
    }
    Ok(())
}

fn measured_manifest(path: &Path, expected: &str) -> Result<toml::Table, Report<ReleaseError>> {
    real_path(path)?;
    let raw = read_bounded(path, Role::Source)?;
    real_path(path)?;
    if raw.sha256 != expected || raw.bytes.len() > 1024 * 1024 {
        return Err(composition_error(
            "origin manifest differs from its approved digest",
        ));
    }
    toml::from_str(std::str::from_utf8(&raw.bytes).map_err(composition_error)?)
        .map_err(composition_error)
}

fn require_identity(
    manifest: &toml::Table,
    name: &str,
    version: &str,
    workspace: Option<(&WorkspaceOrigins, &Path)>,
) -> Result<(), Report<ReleaseError>> {
    let package = manifest
        .get("package")
        .and_then(toml::Value::as_table)
        .ok_or_else(|| composition_error("origin manifest has no package"))?;
    if package.get("name").and_then(toml::Value::as_str) != Some(name)
        || package.contains_key("workspace")
    {
        return Err(composition_error(
            "origin package name or workspace selection differs",
        ));
    }
    if package.get("version").and_then(toml::Value::as_str) == Some(version) {
        return Ok(());
    }
    if package
        .get("version")
        .and_then(toml::Value::as_table)
        .is_none_or(|value| {
            value.len() != 1 || value.get("workspace").and_then(toml::Value::as_bool) != Some(true)
        })
    {
        return Err(composition_error("origin package version differs"));
    }
    let (workspace, relative) = workspace
        .ok_or_else(|| composition_error("prepared manifest has unresolved version inheritance"))?;
    let root_relative = Path::new("Cargo.toml");
    let mut parent = relative.parent();
    while let Some(directory) = parent {
        let ancestor = directory.join("Cargo.toml");
        if ancestor != relative
            && ancestor != root_relative
            && let Some(hash) = workspace.manifests.get(&ancestor)
        {
            let ancestor = measured_manifest(&workspace.root.join(&ancestor), hash)?;
            if ancestor.contains_key("workspace") {
                return Err(composition_error(
                    "nested workspace version inheritance is unreviewed",
                ));
            }
        }
        parent = directory.parent();
    }
    let hash = workspace
        .manifests
        .get(root_relative)
        .ok_or_else(|| composition_error("approved root workspace manifest is missing"))?;
    let root = measured_manifest(&workspace.root.join(root_relative), hash)?;
    if root
        .get("workspace")
        .and_then(|value| value.get("package"))
        .and_then(|value| value.get("version"))
        .and_then(toml::Value::as_str)
        != Some(version)
    {
        return Err(composition_error("approved workspace version differs"));
    }
    Ok(())
}

#[cfg(test)]
mod tests;

#[cfg(test)]
impl VerifiedPackageOrigins {
    /// Provides fictional registry joins for owner-checked native test fixtures.
    pub(crate) fn fixture_native_registry(observed: &CargoObservations) -> Self {
        Self {
            source_commit: "a".repeat(40),
            materials_sha256: "b".repeat(64),
            packages: observed
                .packages()
                .iter()
                .map(|package| {
                    (
                        package.id().to_owned(),
                        PackageOrigin {
                            name: package.name().to_owned(),
                            version: package.version().to_owned(),
                            manifest_path: Path::new("native")
                                .join(package.name())
                                .join("Cargo.toml"),
                            prepared_manifest_sha256: "b".repeat(64),
                            origin: Original::Material {
                                source: "registry+https://example.com/index".to_owned(),
                                original: crate::materials::CargoOrigin::Registry {
                                    archive_sha256: "a".repeat(64),
                                },
                            },
                        },
                    )
                })
                .collect(),
            native_sources: BTreeMap::new(),
        }
    }
}
