//! Conservative native projection from sealed composition and verified originals.

use std::collections::BTreeSet;

use error_stack::Report;
use serde::Serialize;
use serde_json::json;

use crate::composition::native::{NativeContributor, NativeInventory, reviewed_wrapper_version};
use crate::composition::origin::{Original, VerifiedPackageOrigins};
use crate::composition::{CargoObservations, CompositionView, UnitContext};
use crate::error::ReleaseError;
use crate::materials::CargoOrigin;
use crate::recipe::RecipeCompleted;

use super::{
    Component, ComponentRole, Dependency, Origin, OriginFields, cargo, lower_hex, plain,
    relative_source, spdx_error,
};

/// Renderer inputs borrowed from sealed native receipts, without complete coverage.
pub(crate) struct NativeProjection<'a> {
    view: CompositionView<'a>,
    components: Vec<Component>,
}

impl<'a> NativeProjection<'a> {
    /// Joins measured native inputs to the completed build's verified originals.
    ///
    /// # Errors
    ///
    /// Rejects changed receipts, wrong source/material identities and conflicting
    /// package, compiler-context, feature or original-source joins.
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
                "native originals belong to another source or material tree",
            ));
        }
        let components = project(view.native(), view.observations(), origins)?;
        Ok(Self { view, components })
    }

    /// Returns the number of checked native wrapper-context components.
    pub(crate) fn component_count(&self) -> usize {
        self.components.len()
    }

    /// Returns the borrowed measured receipts with their existing coverage gaps.
    pub(crate) fn inventory(&self) -> &NativeInventory {
        self.view.native()
    }

    /// Releases the renderer components; native contexts project no package edges.
    pub(super) fn into_parts(self) -> (Vec<Component>, Vec<Dependency>) {
        (self.components, Vec::new())
    }
}

/// Sanitized native facts; no opaque Cargo IDs or private working paths are kept.
#[derive(Serialize)]
pub(super) struct NativeOriginal {
    locked_source: String,
    cargo_wrapper_name: String,
    cargo_wrapper_version: String,
    native_implementation_version: Option<String>,
    original_registry_archive_sha256: String,
    context: UnitContext,
    features: Vec<String>,
    prepared_source_tree_sha256: String,
    source_subtrees: Vec<Subtree>,
    static_libraries: Vec<String>,
    static_output_archives: Vec<Archive>,
}

#[derive(Serialize)]
struct Subtree {
    relative_path: String,
    tree_sha256: String,
}

#[derive(Serialize)]
struct Archive {
    filename: String,
    size: u64,
    sha256: String,
}

fn project(
    inventory: &NativeInventory,
    observed: &CargoObservations,
    origins: &VerifiedPackageOrigins,
) -> Result<Vec<Component>, Report<ReleaseError>> {
    let mut seen = BTreeSet::new();
    let mut stable = BTreeSet::new();
    let mut components = Vec::new();
    for contributor in inventory.contributors() {
        if !seen.insert((
            contributor.package_id(),
            contributor.context() == UnitContext::Target,
        )) {
            return Err(spdx_error("duplicate native package/context receipt"));
        }
        let mut matching = observed
            .packages()
            .iter()
            .filter(|package| package.id() == contributor.package_id());
        let package = matching
            .next()
            .ok_or_else(|| spdx_error("native receipt has no observed package"))?;
        if matching.next().is_some()
            || package.name() != contributor.name()
            || package.version() != contributor.version()
            || !contributor.reviewed_wrapper()
        {
            return Err(spdx_error(
                "native receipt has an unknown or conflicting Cargo wrapper",
            ));
        }
        let verified = origins
            .package(contributor.package_id())
            .ok_or_else(|| spdx_error("native receipt has no verified exact-ID original"))?;
        if verified.name() != contributor.name() || verified.version() != contributor.version() {
            return Err(spdx_error(
                "native receipt conflicts with verified wrapper identity",
            ));
        }
        let Original::Material {
            source,
            original: CargoOrigin::Registry { archive_sha256 },
        } = verified.origin()
        else {
            return Err(spdx_error(
                "native input lacks a verified reviewed registry original",
            ));
        };
        if archive_sha256 != contributor.original_sha256() {
            return Err(spdx_error(
                "native original registry archive digest differs",
            ));
        }
        cargo::validate_material(
            source,
            &CargoOrigin::Registry {
                archive_sha256: archive_sha256.clone(),
            },
        )?;
        let expected_classification = match contributor.context() {
            UnitContext::Target => "conservative_embedded_contributor",
            UnitContext::Host => "build_tool_native_contributor",
        };
        if contributor.classification() != expected_classification {
            return Err(spdx_error(
                "native classification conflicts with observed context",
            ));
        }
        let consumers = observed
            .units()
            .iter()
            .filter(|unit| {
                unit.package_id() == contributor.package_id()
                    && unit.context() == contributor.context()
                    && !unit.target_kind().iter().any(|kind| kind == "custom-build")
            })
            .collect::<Vec<_>>();
        if consumers.is_empty()
            || consumers
                .iter()
                .any(|unit| unit.features() != contributor.features())
            || contributor.features().iter().collect::<BTreeSet<_>>().len()
                != contributor.features().len()
        {
            return Err(spdx_error(
                "native compiler context or effective unit features differ",
            ));
        }
        let original = original(contributor, source)?;
        let identity = serde_json::to_string(&json!([
            "native-registry-v1",
            source,
            archive_sha256,
            contributor.name(),
            contributor.version(),
            contributor.context()
        ]))
        .map_err(spdx_error)?;
        plain(&identity)?;
        if !stable.insert(identity.clone()) {
            return Err(spdx_error(
                "distinct native Cargo aliases have the same stable original/context identity",
            ));
        }
        components.push(Component {
            identity,
            name: format!(
                "Native inputs from Cargo wrapper {}@{}",
                contributor.name(),
                contributor.version()
            ),
            version: None,
            roles: vec![if contributor.context() == UnitContext::Target {
                ComponentRole::ObservedNative
            } else {
                ComponentRole::BuildMaterial
            }],
            origin: Some(Origin::NativeVerified { original }),
            declared_license: None,
        });
    }
    let mut expected = BTreeSet::new();
    for unit in observed.units() {
        if unit.target_kind().iter().any(|kind| kind == "custom-build") {
            continue;
        }
        let package = observed
            .packages()
            .iter()
            .find(|package| package.id() == unit.package_id())
            .ok_or_else(|| spdx_error("native compiler context has no observed package"))?;
        if reviewed_wrapper_version(package.name()).is_some() {
            expected.insert((package.id(), unit.context() == UnitContext::Target));
        }
    }
    if seen != expected {
        return Err(spdx_error(
            "observed native consuming contexts lack exact measured receipts",
        ));
    }
    Ok(components)
}

fn original(
    contributor: &NativeContributor,
    source: &str,
) -> Result<NativeOriginal, Report<ReleaseError>> {
    if !lower_hex(contributor.source_tree_sha256(), 64) {
        return Err(spdx_error("native prepared tree digest is invalid"));
    }
    let mut paths = BTreeSet::new();
    let mut source_subtrees = Vec::new();
    for subtree in contributor.source_subtrees() {
        relative_source(subtree.path())?;
        if !paths.insert(subtree.path()) || !lower_hex(subtree.sha256(), 64) {
            return Err(spdx_error(
                "native source subtree identity is invalid or duplicated",
            ));
        }
        source_subtrees.push(Subtree {
            relative_path: subtree.path().to_owned(),
            tree_sha256: subtree.sha256().to_owned(),
        });
    }
    let mut archives = BTreeSet::new();
    let mut static_output_archives = Vec::new();
    for archive in contributor.archives() {
        let filename = archive
            .filename()
            .ok_or_else(|| spdx_error("native output has no logical filename"))?;
        relative_source(filename)?;
        if !archives.insert(filename.to_owned())
            || !filename.ends_with(".a")
            || archive.size() < 8
            || !lower_hex(archive.sha256(), 64)
        {
            return Err(spdx_error(
                "native output archive identity is invalid or duplicated",
            ));
        }
        static_output_archives.push(Archive {
            filename: filename.to_owned(),
            size: archive.size(),
            sha256: archive.sha256().to_owned(),
        });
    }
    let mut libraries = BTreeSet::new();
    for directive in contributor.linked_libs() {
        let library = directive
            .strip_prefix("static=")
            .ok_or_else(|| spdx_error("native directive is not a static library"))?;
        relative_source(library)?;
        if library.contains('/') || !libraries.insert(format!("lib{library}.a")) {
            return Err(spdx_error("native static library is invalid or duplicated"));
        }
    }
    if source_subtrees.is_empty() || archives.is_empty() || archives != libraries {
        return Err(spdx_error(
            "native source or static-output coverage differs from link directives",
        ));
    }
    Ok(NativeOriginal {
        locked_source: source.to_owned(),
        cargo_wrapper_name: contributor.name().to_owned(),
        cargo_wrapper_version: contributor.version().to_owned(),
        native_implementation_version: None,
        original_registry_archive_sha256: contributor.original_sha256().to_owned(),
        context: contributor.context(),
        features: contributor.features().to_vec(),
        prepared_source_tree_sha256: contributor.source_tree_sha256().to_owned(),
        source_subtrees,
        static_libraries: contributor.linked_libs().to_vec(),
        static_output_archives,
    })
}

pub(super) fn origin_fields(
    component: &Component,
    original: &NativeOriginal,
) -> Result<OriginFields, Report<ReleaseError>> {
    let expected = if original.context == UnitContext::Target {
        ComponentRole::ObservedNative
    } else {
        ComponentRole::BuildMaterial
    };
    if component.roles != [expected]
        || component.version.is_some()
        || component.declared_license.is_some()
    {
        return Err(spdx_error(
            "native roles, implementation version or declared license exceed measured facts",
        ));
    }
    cargo::validate_material(
        &original.locked_source,
        &CargoOrigin::Registry {
            archive_sha256: original.original_registry_archive_sha256.clone(),
        },
    )?;
    let facts = serde_json::to_string(original).map_err(spdx_error)?;
    let info = format!(
        "Verified native inputs from Cargo wrapper facts {facts}. Cargo wrapper versions do not identify native implementation versions. Checksum identifies the original registry source archive; prepared/subtree digests are structured tree evidence and static output archives are measured build products. Conservative static-link inputs do not establish final function reachability. Complete native/tool coverage and original tool acquisition are not asserted."
    );
    plain(&info)?;
    Ok((
        "NOASSERTION".to_owned(),
        info,
        Some(("SHA256", original.original_registry_archive_sha256.clone())),
    ))
}

#[cfg(test)]
pub(crate) fn validate_for_tests(
    inventory: &NativeInventory,
    observed: &CargoObservations,
    origins: &VerifiedPackageOrigins,
) -> Result<(), Report<ReleaseError>> {
    project(inventory, observed, origins).map(|_| ())
}

#[cfg(test)]
mod tests;
