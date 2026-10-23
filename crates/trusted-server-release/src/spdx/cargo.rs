//! Conservative projection of verified observed Cargo packages.
//!
//! This adapter preserves borrowed sealed unit observations separately from
//! package roles and potential package relationships. It grants no complete
//! composition, qualification, signing or publication authority.

use std::collections::{BTreeMap, BTreeSet};

use error_stack::Report;
use serde_json::json;

use crate::composition::graph::GraphEvidence;
use crate::composition::origin::{Original, VerifiedPackageOrigins};
use crate::composition::{CargoObservations, CompositionView, RustPackage, UnitContext};
use crate::error::ReleaseError;
use crate::materials::{CargoOrigin, GitSourceConfig};
use crate::recipe::RecipeCompleted;

use super::{
    Component, ComponentRole, Dependency, Origin, lower_hex, plain, relative_source, spdx_error,
};

/// Renderer-ready observed Rust packages, retaining their sealed unit evidence.
///
/// Native/tool completeness and complete SPDX generation remain unavailable.
pub(crate) struct CargoProjection<'a> {
    view: CompositionView<'a>,
    components: Vec<Component>,
    dependencies: Vec<Dependency>,
}

impl<'a> CargoProjection<'a> {
    /// Projects exact observed package IDs after checking the completed owner.
    ///
    /// # Errors
    ///
    /// Rejects changed receipts, mismatched source/material identities, missing
    /// origins, ambiguous package identities and unsupported compiler contexts.
    pub(crate) fn from_completed(
        completed: &'a RecipeCompleted,
    ) -> Result<Self, Report<ReleaseError>> {
        let view = completed.composition_view()?;
        let origins = view.package_origins();
        if origins.source_commit() != completed.commit().as_str()
            || !lower_hex(origins.source_commit(), 40)
            || origins.materials_sha256() != completed.materials_digest()
            || !lower_hex(origins.materials_sha256(), 64)
        {
            return Err(spdx_error(
                "Cargo originals belong to another source or material tree",
            ));
        }
        let (components, dependencies) = project(view.observations(), view.graph(), origins)?;
        Ok(Self {
            view,
            components,
            dependencies,
        })
    }

    /// Returns the number of verified observed package components.
    pub(crate) fn component_count(&self) -> usize {
        self.components.len()
    }

    /// Returns borrowed sealed units with their original contexts and features.
    pub(crate) fn observations(&self) -> &CargoObservations {
        self.view.observations()
    }

    /// Releases the renderer components and potential package relations.
    pub(super) fn into_parts(self) -> (Vec<Component>, Vec<Dependency>) {
        (self.components, self.dependencies)
    }
}

fn project(
    observed: &CargoObservations,
    graph: &GraphEvidence,
    origins: &VerifiedPackageOrigins,
) -> Result<(Vec<Component>, Vec<Dependency>), Report<ReleaseError>> {
    let mut identities = BTreeMap::new();
    let mut stable = BTreeSet::new();
    let mut components = Vec::new();
    let observed_ids = observed
        .packages()
        .iter()
        .map(RustPackage::id)
        .collect::<BTreeSet<_>>();
    if observed_ids.len() != observed.packages().len()
        || observed_ids
            != graph
                .observed_package_ids()
                .iter()
                .map(String::as_str)
                .collect()
        || !observed_ids.contains(graph.selected_package_id())
        || observed
            .units()
            .iter()
            .any(|unit| !observed_ids.contains(unit.package_id()))
    {
        return Err(spdx_error(
            "Cargo package, unit and graph observations disagree",
        ));
    }
    for package in observed.packages() {
        let verified = origins
            .package(package.id())
            .ok_or_else(|| spdx_error("observed Cargo ID has no verified original"))?;
        if package.name() != verified.name()
            || package.version() != verified.version()
            || !lower_hex(verified.prepared_manifest_sha256(), 64)
        {
            return Err(spdx_error(
                "observed package facts conflict with verified original",
            ));
        }
        let (identity, origin) = match verified.origin() {
            Original::Workspace {
                source_commit,
                path,
            } => {
                let path = path
                    .to_str()
                    .ok_or_else(|| spdx_error("tracked manifest is not UTF-8"))?;
                if source_commit != origins.source_commit()
                    || verified.relative_manifest_path() != std::path::Path::new(path)
                {
                    return Err(spdx_error(
                        "workspace original conflicts with selected source",
                    ));
                }
                (
                    workspace_identity(source_commit, path, package.name(), package.version())?,
                    Origin::Source {
                        commit: source_commit.clone(),
                        path: path.to_owned(),
                    },
                )
            }
            Original::Material { source, original } => (
                material_identity(source, original, package.name(), package.version())?,
                Origin::CargoMaterial {
                    source: source.clone(),
                    original: original.clone(),
                    prepared_manifest_sha256: verified.prepared_manifest_sha256().to_owned(),
                },
            ),
        };
        if !stable.insert(identity.clone())
            || identities.insert(package.id(), identity.clone()).is_some()
        {
            return Err(spdx_error(
                "distinct observed Cargo aliases have the same stable original identity",
            ));
        }
        let roles = package_roles(observed, package.id())?;
        components.push(Component {
            identity,
            name: package.name().to_owned(),
            version: Some(package.version().to_owned()),
            roles,
            origin: Some(origin),
            declared_license: package.license().map(str::to_owned),
        });
    }
    let mut dependencies = Vec::new();
    let mut edges = BTreeSet::new();
    for edge in graph.potential_edges() {
        for dependency in project_edge(edge.parent(), edge.dependency(), edge.kinds(), &identities)?
        {
            if !edges.insert((
                dependency.from.clone(),
                dependency.to.clone(),
                dependency.build,
            )) {
                return Err(spdx_error(
                    "duplicate or self-referential projected dependency",
                ));
            }
            dependencies.push(dependency);
        }
    }
    Ok((components, dependencies))
}

fn workspace_identity(
    commit: &str,
    path: &str,
    name: &str,
    version: &str,
) -> Result<String, Report<ReleaseError>> {
    if !lower_hex(commit, 40) {
        return Err(spdx_error("workspace identity has no selected commit"));
    }
    relative_source(path)?;
    identity(
        &json!(["cargo-workspace-v1", commit, path, name, version]),
        name,
        version,
    )
}

fn identity(
    value: &serde_json::Value,
    name: &str,
    version: &str,
) -> Result<String, Report<ReleaseError>> {
    plain(name)?;
    plain(version)?;
    let key = serde_json::to_string(value).map_err(spdx_error)?;
    plain(&key)?;
    Ok(key)
}

fn material_identity(
    source: &str,
    original: &CargoOrigin,
    name: &str,
    version: &str,
) -> Result<String, Report<ReleaseError>> {
    validate_material(source, original)?;
    let facts = match original {
        CargoOrigin::Registry { archive_sha256 } => {
            json!(["cargo-registry-v1", source, archive_sha256, name, version])
        }
        CargoOrigin::Git {
            revision,
            repository_tree,
            package_tree,
            package_path,
            ..
        } => json!([
            "cargo-git-v1",
            source,
            revision,
            repository_tree,
            package_tree,
            package_path,
            name,
            version
        ]),
    };
    identity(&facts, name, version)
}

pub(super) fn validate_material(
    source: &str,
    original: &CargoOrigin,
) -> Result<(), Report<ReleaseError>> {
    plain(source)?;
    match original {
        CargoOrigin::Registry { archive_sha256 } => {
            let location = source
                .strip_prefix("registry+")
                .or_else(|| source.strip_prefix("sparse+"))
                .ok_or_else(|| {
                    spdx_error("original registry source is not a locked registry identity")
                })?;
            if !location.strip_prefix("https://").is_some_and(|rest| {
                let authority = rest.split('/').next().unwrap_or_default();
                !authority.is_empty()
                    && authority.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b':')
                    })
                    && rest.bytes().all(|byte| {
                        byte.is_ascii_graphic() && !matches!(byte, b'#' | b'?' | b'\\' | b'"')
                    })
            }) || !lower_hex(archive_sha256, 64)
            {
                return Err(spdx_error(
                    "registry original source or archive SHA-256 is invalid",
                ));
            }
        }
        CargoOrigin::Git {
            revision,
            repository_tree,
            package_tree,
            package_path,
            committed_manifest_blob,
            committed_manifest_sha256,
            normalized_manifest_sha256,
        } => {
            let (selector, locked_revision) = source
                .rsplit_once('#')
                .ok_or_else(|| spdx_error("Git original has no locked revision"))?;
            GitSourceConfig::parse(selector)
                .map_err(|error| error.change_context(ReleaseError::Spdx))?;
            if locked_revision != revision
                || [
                    revision,
                    repository_tree,
                    package_tree,
                    committed_manifest_blob,
                ]
                .iter()
                .any(|value| !lower_hex(value, 40))
                || !lower_hex(committed_manifest_sha256, 64)
                || normalized_manifest_sha256
                    .as_ref()
                    .is_some_and(|value| !lower_hex(value, 64))
            {
                return Err(spdx_error(
                    "Git original object or manifest identities are invalid",
                ));
            }
            if !package_path.is_empty() {
                relative_source(package_path)?;
            }
        }
    }
    Ok(())
}

fn classify(
    context: UnitContext,
    kinds: &[String],
    crate_types: &[String],
) -> Result<ComponentRole, Report<ReleaseError>> {
    if crate_types.is_empty()
        || crate_types.iter().collect::<BTreeSet<_>>().len() != crate_types.len()
    {
        return Err(spdx_error("compiler crate types are empty or duplicated"));
    }
    if kinds.is_empty() || kinds.iter().collect::<BTreeSet<_>>().len() != kinds.len() {
        return Err(spdx_error("compiler target kinds are empty or duplicated"));
    }
    let library_kind = |kind: &String| {
        matches!(
            kind.as_str(),
            "lib" | "rlib" | "dylib" | "staticlib" | "cdylib"
        )
    };
    if kinds.iter().all(library_kind) && crate_types.iter().all(library_kind) {
        return Ok(if context == UnitContext::Host {
            ComponentRole::BuildMaterial
        } else {
            ComponentRole::ObservedRust
        });
    }
    let [kind] = kinds else {
        return Err(spdx_error("compiler target kinds are unsupported"));
    };
    match (context, kind.as_str()) {
        (UnitContext::Host, "proc-macro") if crate_types == ["proc-macro"] => {
            Ok(ComponentRole::BuildTool)
        }
        (UnitContext::Host, "custom-build") if crate_types == ["bin"] => {
            Ok(ComponentRole::BuildTool)
        }
        (UnitContext::Target, "bin") if crate_types == ["bin"] => Ok(ComponentRole::ObservedRust),
        _ => Err(spdx_error(
            "compiler unit context or kind is impossible or unsupported",
        )),
    }
}

#[cfg(test)]
mod tests;

fn package_roles(
    observed: &CargoObservations,
    package_id: &str,
) -> Result<Vec<ComponentRole>, Report<ReleaseError>> {
    let mut roles = BTreeSet::new();
    for unit in observed
        .units()
        .iter()
        .filter(|unit| unit.package_id() == package_id)
    {
        roles.insert(classify(
            unit.context(),
            unit.target_kind(),
            unit.crate_types(),
        )?);
    }
    if roles.is_empty() {
        return Err(spdx_error("observed package has no compiler units"));
    }
    Ok(roles.into_iter().collect())
}

fn project_edge(
    parent: &str,
    dependency: &str,
    kinds: &[String],
    identities: &BTreeMap<&str, String>,
) -> Result<Vec<Dependency>, Report<ReleaseError>> {
    let (Some(from), Some(to)) = (identities.get(parent), identities.get(dependency)) else {
        return Ok(Vec::new());
    };
    let mut result = Vec::new();
    for kind in kinds {
        let build = match kind.as_str() {
            "normal" => false,
            "build" => true,
            "dev" => continue,
            _ => return Err(spdx_error("unknown potential package dependency kind")),
        };
        if from == to {
            return Err(spdx_error("self-referential projected dependency"));
        }
        result.push(Dependency {
            from: from.clone(),
            to: to.clone(),
            build,
        });
    }
    Ok(result)
}
