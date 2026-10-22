//! Conservative projection of sealed installed-tool and logical runtime facts.

use std::collections::BTreeSet;

use error_stack::Report;
use serde_json::json;

use super::{
    Component, ComponentRole, Dependency, Origin, OriginFields, lower_hex, plain, spdx_error,
};
use crate::composition::CompositionView;
use crate::composition::external::Runtime;
use crate::composition::platform::PlatformEvidence;
use crate::error::ReleaseError;
use crate::policy::ReleaseTarget;
use crate::recipe::RecipeCompleted;

#[cfg(test)]
mod tests;

/// Borrowed sealed platform evidence with conservative renderer components.
///
/// Coverage exclusions and incompleteness remain in the exact borrowed receipt;
/// this adapter grants no complete-document or acquisition authority.
pub(crate) struct PlatformProjection<'a> {
    view: CompositionView<'a>,
    components: Vec<Component>,
}

impl<'a> PlatformProjection<'a> {
    /// Projects typed platform measurements after validating their completed owner.
    ///
    /// # Errors
    ///
    /// Rejects changed sealed receipts, another artifact, target, source, Rust
    /// tree, GNU boundary/plan or an inconsistent Apple SDK observation.
    pub(crate) fn from_completed(
        completed: &'a RecipeCompleted,
    ) -> Result<Self, Report<ReleaseError>> {
        let view = completed.composition_view()?;
        let owner = Owner {
            target: completed.asset().artifact().target,
            artifact: completed.artifact().sha256().to_owned(),
            rust_tree: completed.rust_tools_digest().to_owned(),
            source: completed.commit().as_str().to_owned(),
            receipt_source: view.package_origins().source_commit().to_owned(),
            boundary: completed.boundary_fingerprint(),
            plan: completed.plan_digest().to_owned(),
        };
        let components = project(view.platform(), &owner)?;
        Ok(Self { view, components })
    }
    /// Returns the number of renderer components without completeness authority.
    pub(crate) fn component_count(&self) -> usize {
        self.components.len()
    }
    /// Borrows exact sealed measurements, retaining exclusions and incompleteness.
    pub(crate) fn evidence(&self) -> &PlatformEvidence {
        self.view.platform()
    }
    /// Returns the digest of the exact sealed owning receipt.
    pub(crate) fn receipt_sha256(&self) -> &str {
        self.view.receipt_sha256()
    }
    /// Returns sorted, unique coverage gaps recorded in the sealed measurements.
    pub(super) fn coverage_gaps(&self) -> Vec<String> {
        coverage_gaps(self.view.platform())
    }
    /// Releases the renderer components; platform facts project no package edges.
    pub(super) fn into_parts(self) -> (Vec<Component>, Vec<Dependency>) {
        (self.components, Vec::new())
    }
}

/// Names each recorded incompleteness without upgrading any measurement.
fn coverage_gaps(platform: &PlatformEvidence) -> Vec<String> {
    let mut gaps = BTreeSet::new();
    if !platform.material_native_coverage_complete() {
        gaps.insert("native-material-coverage-incomplete".to_owned());
    }
    for role in platform.unresolved_native_tool_roles() {
        gaps.insert(format!("native-tool-host-path-unresolved:{role}"));
    }
    if platform.apple_tool_closure_unmeasured() {
        gaps.insert("apple-tool-closure-unmeasured".to_owned());
    }
    if let Some(apple) = platform.apple() {
        for exclusion in apple.coverage_exclusions() {
            gaps.insert(format!("apple-coverage-excluded:{exclusion}"));
        }
    }
    if let Some(gnu) = platform.gnu() {
        if gnu.tool_closure_unmeasured() {
            gaps.insert("gnu-tool-closure-unmeasured".to_owned());
        }
        if gnu.original_acquisition_unverified() {
            gaps.insert("gnu-original-acquisition-unverified".to_owned());
        }
        if !gnu.content_unmeasured_files().is_empty() {
            gaps.insert("gnu-privileged-helper-content-unmeasured".to_owned());
        }
    }
    if platform.fastly_tools().is_some() {
        gaps.insert("fastly-tool-closure-unmeasured".to_owned());
        gaps.insert("fastly-original-acquisition-unverified".to_owned());
    }
    gaps.into_iter().collect()
}

struct Owner {
    target: ReleaseTarget,
    artifact: String,
    rust_tree: String,
    source: String,
    receipt_source: String,
    boundary: [u8; 32],
    plan: String,
}

fn identity(
    target: ReleaseTarget,
    role: &str,
    value: &str,
) -> Result<String, Report<ReleaseError>> {
    plain(role)?;
    plain(value)?;
    let key = serde_json::to_string(&json!(["platform-v1", target.triple(), role, value]))
        .map_err(spdx_error)?;
    plain(&key)?;
    Ok(key)
}

fn component(
    target: ReleaseTarget,
    role: &str,
    value: &str,
    name: String,
    roles: Vec<ComponentRole>,
    origin: Origin,
) -> Result<Component, Report<ReleaseError>> {
    plain(&name)?;
    Ok(Component {
        identity: identity(target, role, value)?,
        name,
        version: None,
        roles,
        origin: Some(origin),
        declared_license: None,
    })
}

fn project(
    platform: &PlatformEvidence,
    owner: &Owner,
) -> Result<Vec<Component>, Report<ReleaseError>> {
    let external = platform.external_requirements();
    if !lower_hex(&owner.source, 40)
        || owner.source != owner.receipt_source
        || !lower_hex(&owner.artifact, 64)
        || !lower_hex(&owner.rust_tree, 64)
        || platform.target() != owner.target.triple()
        || platform.artifact_sha256() != owner.artifact
        || platform.rust().target() != owner.target.triple()
        || platform.rust().tree_sha256() != owner.rust_tree
        || external.target() != owner.target
        || external.artifact_sha256() != owner.artifact
        || platform.apple().is_some() != owner.target.is_darwin()
    {
        return Err(spdx_error(
            "platform measurements differ from the sealed source/artifact/target/tool owner",
        ));
    }
    let mut components = vec![component(
        owner.target,
        "rust-installed-tree",
        &owner.rust_tree,
        "Measured Rust tool tree".to_owned(),
        vec![ComponentRole::BuildMaterial, ComponentRole::BuildTool],
        Origin::StructuredTree {
            sha256: owner.rust_tree.clone(),
        },
    )?];
    if let Some(apple) = platform.apple() {
        if external.macho_sdk() != Some(apple.sdk_version()) {
            return Err(spdx_error(
                "Apple SDK differs from accepted executable requirements",
            ));
        }
        for (role, sha256) in apple.tool_files() {
            components.push(measured_file(
                owner.target,
                &format!("apple-{role}"),
                sha256,
                ComponentRole::BuildTool,
            )?);
        }
        components.push(measured_file(
            owner.target,
            "apple-sdk-settings",
            apple.sdk_settings_sha256(),
            ComponentRole::BuildMaterial,
        )?);
        if let Some(sha256) = apple.sdk_tree_sha256() {
            if !lower_hex(sha256, 64) {
                return Err(spdx_error("invalid detached SDK tree identity"));
            }
            components.push(component(
                owner.target,
                "apple-sdk-installed-tree",
                sha256,
                "Measured detached Apple SDK tree".to_owned(),
                vec![ComponentRole::BuildMaterial],
                Origin::StructuredTree {
                    sha256: sha256.to_owned(),
                },
            )?);
        }
        if let Some(sha256) = apple.liblto_sha256() {
            components.push(measured_file(
                owner.target,
                "apple-liblto",
                sha256,
                ComponentRole::BuildTool,
            )?);
        }
        if let Some(sha256) = apple.libtapi_sha256() {
            components.push(measured_file(
                owner.target,
                "apple-libtapi",
                sha256,
                ComponentRole::BuildTool,
            )?);
        }
        let toolchain_trees = [
            (
                "apple-toolchain-library-tree",
                apple.library_tree_sha256(),
                "Measured Apple toolchain library directory tree",
                ComponentRole::BuildMaterial,
            ),
            (
                "apple-clang-resource-tree",
                apple.clang_resource_tree_sha256(),
                "Measured Apple clang resource directory tree",
                ComponentRole::BuildMaterial,
            ),
            (
                "apple-toolchain-helper-tree",
                apple.helper_tree_sha256(),
                "Measured Apple toolchain helper directory tree",
                ComponentRole::BuildTool,
            ),
        ];
        for (role, digest, name, component_role) in toolchain_trees {
            let Some(sha256) = digest else {
                continue;
            };
            if !lower_hex(sha256, 64) {
                return Err(spdx_error("invalid Apple toolchain tree identity"));
            }
            components.push(component(
                owner.target,
                role,
                sha256,
                name.to_owned(),
                vec![component_role],
                Origin::StructuredTree {
                    sha256: sha256.to_owned(),
                },
            )?);
        }
    }
    if let Some(gnu) = platform.gnu() {
        if owner.target != ReleaseTarget::X86_64UnknownLinuxGnu {
            return Err(spdx_error("GNU measurements belong to another platform"));
        }
        gnu.verify_binding(&owner.boundary, &owner.plan)
            .map_err(|error| error.change_context(ReleaseError::Spdx))?;
        let sha256 = gnu.installed_files_sha256();
        if !lower_hex(sha256, 64) {
            return Err(spdx_error("invalid GNU installed-image identity"));
        }
        components.push(component(
            owner.target,
            "gnu-installed-image-tree",
            sha256,
            format!(
                "Measured GNU installed image files; {} reviewed privileged helpers bound by metadata only",
                gnu.content_unmeasured_files().len()
            ),
            vec![ComponentRole::BuildMaterial],
            Origin::StructuredTree {
                sha256: sha256.to_owned(),
            },
        )?);
        for (role, sha256) in gnu.measured_files() {
            let component_role = match role {
                "compiler_runtime" => ComponentRole::BuildMaterial,
                "compiler"
                | "archiver"
                | "gcc_linker_candidate"
                | "gcc_assembler_candidate"
                | "configured_cc1"
                | "configured_collect2"
                | "configured_as"
                | "configured_ld" => ComponentRole::BuildTool,
                _ => return Err(spdx_error("GNU measured file role has not been reviewed")),
            };
            components.push(measured_file(
                owner.target,
                &format!("gnu-selected-or-configured-{role}"),
                sha256,
                component_role,
            )?);
        }
    }
    if let Some(fastly) = platform.fastly_tools() {
        if owner.target != ReleaseTarget::Wasm32Wasip1 {
            return Err(spdx_error("Fastly measurements belong to another platform"));
        }
        fastly
            .verify_binding(&owner.boundary, &owner.plan)
            .map_err(|error| error.change_context(ReleaseError::Spdx))?;
        for (role, sha256) in fastly.measured_files() {
            if ![
                "host_compiler",
                "host_archiver",
                "configured_host_assembler",
                "wasm_rust_linker",
            ]
            .contains(&role)
            {
                return Err(spdx_error("Fastly file role has not been reviewed"));
            }
            components.push(measured_file(
                owner.target,
                &format!("fastly-selected-or-configured-{role}"),
                sha256,
                ComponentRole::BuildTool,
            )?);
        }
    }
    let mut runtime = |kind: &str, value: &str| -> Result<(), Report<ReleaseError>> {
        components.push(component(
            owner.target,
            kind,
            value,
            format!("Logical {kind}: {value}"),
            vec![ComponentRole::ExternalRuntime],
            Origin::RuntimeObservation {
                artifact_sha256: owner.artifact.clone(),
            },
        )?);
        Ok(())
    };
    match external.runtime() {
        Runtime::Elf {
            interpreter,
            needed,
            version_requirements,
            max_glibc,
        } => {
            runtime("ELF loader", interpreter)?;
            for library in needed {
                runtime("ELF needed library", library)?;
            }
            for requirement in version_requirements {
                runtime(
                    "ELF symbol version",
                    &serde_json::to_string(requirement).map_err(spdx_error)?,
                )?;
            }
            runtime("ELF maximum required GLIBC", max_glibc)?;
        }
        Runtime::Macho {
            loader,
            cpu,
            dylibs,
            minimum_os,
            sdk,
        } => {
            runtime("Mach-O loader", loader)?;
            runtime("Mach-O CPU", cpu)?;
            for library in dylibs {
                runtime("Mach-O dylib", library)?;
            }
            runtime("Mach-O minimum OS", minimum_os)?;
            runtime("Mach-O SDK", sdk)?;
        }
        Runtime::Wasm { import_modules } => {
            for module in import_modules {
                runtime("Wasm host import", module)?;
            }
        }
    }
    Ok(components)
}

fn measured_file(
    target: ReleaseTarget,
    role: &str,
    sha256: &str,
    component_role: ComponentRole,
) -> Result<Component, Report<ReleaseError>> {
    if !lower_hex(sha256, 64) {
        return Err(spdx_error("invalid measured platform raw-file identity"));
    }
    component(
        target,
        role,
        sha256,
        format!("Measured platform file ({role})"),
        vec![component_role],
        Origin::PlatformMeasuredFile {
            role: role.to_owned(),
            sha256: sha256.to_owned(),
        },
    )
}

pub(super) fn file_origin_fields(
    component: &Component,
    role: &str,
    sha256: &str,
) -> Result<OriginFields, Report<ReleaseError>> {
    plain(role)?;
    if !lower_hex(sha256, 64)
        || component.roles.is_empty()
        || component.roles.iter().any(|role| {
            !matches!(
                role,
                ComponentRole::BuildMaterial | ComponentRole::BuildTool
            )
        })
    {
        return Err(spdx_error(
            "measured platform files cannot assert original acquisition or runtime bytes",
        ));
    }
    Ok((
        "NOASSERTION".to_owned(),
        format!(
            "Measured installed platform raw file for {role}; no original acquisition, download location, license or executed helper claim is asserted."
        ),
        Some(("SHA256", sha256.to_owned())),
    ))
}
