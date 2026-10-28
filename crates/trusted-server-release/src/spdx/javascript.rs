//! Artifact-specific conservative projection of checked original JavaScript facts.

use std::collections::{BTreeMap, BTreeSet};

use error_stack::Report;
use serde_json::json;

use crate::composition::CompositionView;
use crate::composition::javascript::JsMembership;
use crate::error::ReleaseError;
use crate::javascript::contributions::{
    Material, ModuleContributions, Origin as JsOrigin, RetainedContributions,
};
use crate::recipe::RecipeCompleted;

use super::{Component, ComponentRole, Dependency, Origin, lower_hex, plain, spdx_error};

/// Checked renderer inputs without complete native/tool coverage authority.
pub(crate) struct JavascriptProjection<'a> {
    view: CompositionView<'a>,
    originals: &'a RetainedContributions,
    components: Vec<Component>,
}

impl<'a> JavascriptProjection<'a> {
    /// Joins the sealed artifact observations to its frozen original inventory.
    ///
    /// # Errors
    ///
    /// Rejects changed receipts and conflicting source, inventory or module joins.
    pub(crate) fn from_completed(
        completed: &'a RecipeCompleted,
    ) -> Result<Self, Report<ReleaseError>> {
        let view = completed.composition_view()?;
        let originals = completed.javascript_contributions()?;
        let components = project(
            view.javascript(),
            originals,
            completed.commit().as_str(),
            completed.artifact().sha256(),
        )?;
        Ok(Self {
            view,
            originals,
            components,
        })
    }
    /// Returns the exact checked collector facts, retaining observation lengths and stages.
    pub(crate) fn originals(&self) -> &RetainedContributions {
        self.originals
    }
    /// Returns the checked renderer component count.
    pub(crate) fn component_count(&self) -> usize {
        self.components.len()
    }
    /// Returns final-byte observations borrowed from the sealed owner.
    pub(crate) fn membership(&self) -> &JsMembership {
        self.view.javascript()
    }
    /// Releases the renderer components; JavaScript projects no package edges.
    pub(super) fn into_parts(self) -> (Vec<Component>, Vec<Dependency>) {
        (self.components, Vec::new())
    }
}

/// Typed originals copied only from the checked collector export.
#[derive(Clone, serde::Serialize)]
pub(super) enum JavascriptOriginal {
    Bundle {
        id: String,
        filename: String,
        size: u64,
        sha256: String,
    },
    Contribution(JsOrigin),
    Material(Material),
}

fn key(
    commit: &str,
    kind: &str,
    facts: &impl serde::Serialize,
) -> Result<String, Report<ReleaseError>> {
    let value = serde_json::to_string(&json!(["javascript-v1", commit, kind, facts]))
        .map_err(spdx_error)?;
    plain(&value)?;
    Ok(value)
}

fn material_id(material: &Material) -> &str {
    match material {
        Material::Source { id, .. }
        | Material::NpmPackage { id, .. }
        | Material::ToolDistribution { id, .. }
        | Material::LifecycleEvent { id, .. }
        | Material::LifecycleOutput { id, .. } => id,
    }
}

fn project(
    membership: &JsMembership,
    originals: &RetainedContributions,
    commit: &str,
    artifact: &str,
) -> Result<Vec<Component>, Report<ReleaseError>> {
    if !lower_hex(commit, 40)
        || originals.source_commit() != commit
        || membership.artifact_sha256() != artifact
        || !lower_hex(artifact, 64)
        || membership.inventory_sha256() != originals.inventory_sha256()
    {
        return Err(spdx_error(
            "JavaScript original source, inventory or artifact identity differs",
        ));
    }
    let mut components = Vec::new();
    let mut material_keys = BTreeMap::new();
    let generated_owners = originals
        .modules()
        .iter()
        .flat_map(ModuleContributions::contributions)
        .filter_map(|entry| match entry.origin() {
            JsOrigin::Generated {
                owner_lock_path, ..
            } => Some(format!("npm:{owner_lock_path}")),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    for material in originals.materials() {
        let identity = key(commit, "material", material)?;
        if material_keys
            .insert(material_id(material), identity.clone())
            .is_some()
        {
            return Err(spdx_error("duplicate JavaScript material"));
        }
        let (name, version) = match material {
            Material::NpmPackage { name, version, .. }
            | Material::ToolDistribution { name, version, .. } => {
                (name.clone(), Some(version.clone()))
            }
            _ => (material_id(material).to_owned(), None),
        };
        let tool = generated_owners.contains(material_id(material))
            || originals
                .tools()
                .iter()
                .any(|tool| tool.material_id() == material_id(material));
        components.push(component(
            identity,
            name,
            version,
            if tool {
                ComponentRole::BuildTool
            } else {
                ComponentRole::BuildMaterial
            },
            commit,
            JavascriptOriginal::Material(material.clone()),
            None,
        ));
    }
    let modules = membership.modules().collect::<Vec<_>>();
    if modules.len() != originals.modules().len() {
        return Err(spdx_error("JavaScript module sets differ"));
    }
    let mut observed = BTreeMap::new();
    for (module, classification) in modules {
        if observed
            .insert(module.id(), (module, classification))
            .is_some()
        {
            return Err(spdx_error("duplicate JavaScript module"));
        }
    }
    for module in originals.modules() {
        let (found, classification) = observed
            .remove(module.id())
            .ok_or_else(|| spdx_error("missing JavaScript module"))?;
        if found.filename() != module.filename()
            || found.size() != module.size()
            || found.sha256() != module.sha256()
        {
            return Err(spdx_error("JavaScript complete module identities differ"));
        }
        let retained = match classification {
            "conservative_embedded_js" => true,
            "js_build_material" => false,
            _ => return Err(spdx_error("unknown final JavaScript classification")),
        };
        let bundle = JavascriptOriginal::Bundle {
            id: module.id().to_owned(),
            filename: module.filename().to_owned(),
            size: module.size(),
            sha256: module.sha256().to_owned(),
        };
        components.push(component(
            key(commit, "bundle", &bundle)?,
            module.id().to_owned(),
            None,
            if retained {
                ComponentRole::RetainedJavascript
            } else {
                ComponentRole::BuildMaterial
            },
            commit,
            bundle,
            None,
        ));
        for contribution in module.contributions() {
            if contribution.stage() != "rollup-rendered" || contribution.rendered_length() == 0 {
                return Err(spdx_error("unknown JavaScript contribution observation"));
            }
            let origin = contribution.origin();
            let producer = match origin {
                JsOrigin::Generated {
                    owner_lock_path, ..
                } => Some(
                    material_keys
                        .get(format!("npm:{owner_lock_path}").as_str())
                        .ok_or_else(|| spdx_error("missing generated JavaScript owner"))?
                        .clone(),
                ),
                _ => None,
            };
            let original = JavascriptOriginal::Contribution(origin.clone());
            let identity = key(commit, "rendered", &json!([module.id(), original]))?;
            components.push(component(
                identity,
                format!("{} rendered input", module.id()),
                None,
                if retained {
                    ComponentRole::ConservativeJavascript
                } else {
                    ComponentRole::BuildMaterial
                },
                commit,
                original,
                producer,
            ));
        }
    }
    Ok(components)
}

fn component(
    identity: String,
    name: String,
    version: Option<String>,
    role: ComponentRole,
    commit: &str,
    original: JavascriptOriginal,
    producer: Option<String>,
) -> Component {
    Component {
        identity,
        name,
        version,
        roles: vec![role],
        origin: Some(Origin::JavascriptVerified {
            source_commit: commit.to_owned(),
            original,
            producer,
        }),
        declared_license: None,
    }
}

#[cfg(test)]
mod tests;

pub(super) fn origin_fields(
    component: &Component,
    inputs: &super::SpdxInputs<'_>,
    commit: &str,
    original: &JavascriptOriginal,
    producer: Option<&str>,
) -> Result<super::OriginFields, Report<ReleaseError>> {
    if commit != inputs.source_commit || !lower_hex(commit, 40) {
        return Err(spdx_error("JavaScript source differs from selected source"));
    }
    if component.declared_license.is_some() {
        return Err(spdx_error(
            "JavaScript declared license exceeds measured facts",
        ));
    }
    if !matches!(
        (original, component.roles.as_slice()),
        (
            JavascriptOriginal::Bundle { .. },
            [ComponentRole::RetainedJavascript | ComponentRole::BuildMaterial]
        ) | (
            JavascriptOriginal::Contribution(_),
            [ComponentRole::ConservativeJavascript | ComponentRole::BuildMaterial]
        ) | (
            JavascriptOriginal::Material(_),
            [ComponentRole::BuildTool | ComponentRole::BuildMaterial]
        )
    ) {
        return Err(spdx_error(
            "JavaScript origin and component roles are incompatible",
        ));
    }
    let generated = matches!(
        original,
        JavascriptOriginal::Contribution(JsOrigin::Generated { .. })
    );
    if generated != producer.is_some()
        || producer.is_some_and(|id| {
            !inputs.components.iter().any(|candidate| {
                candidate.identity == id
                    && candidate.identity != component.identity
                    && candidate.roles.contains(&ComponentRole::BuildTool)
            })
        })
    {
        return Err(spdx_error(
            "generated JavaScript has no checked build-tool owner",
        ));
    }
    let checksum = match original {
        JavascriptOriginal::Bundle { sha256, .. } => Some(("SHA256", sha256.clone())),
        JavascriptOriginal::Contribution(
            JsOrigin::Source { sha256, .. } | JsOrigin::Npm { sha256, .. },
        ) => Some(("SHA256", sha256.clone())),
        JavascriptOriginal::Contribution(JsOrigin::Generated {
            generated_sha256, ..
        }) => Some(("SHA256", generated_sha256.clone())),
        JavascriptOriginal::Material(Material::NpmPackage { integrity, .. }) => {
            Some(("SHA512", archive_sha512(integrity)?))
        }
        JavascriptOriginal::Material(
            Material::Source { sha256, .. }
            | Material::ToolDistribution { sha256, .. }
            | Material::LifecycleOutput { sha256, .. },
        ) => Some(("SHA256", sha256.clone())),
        JavascriptOriginal::Material(Material::LifecycleEvent { .. }) => None,
    };
    if checksum.as_ref().is_some_and(|(algorithm, digest)| {
        !lower_hex(digest, if *algorithm == "SHA512" { 128 } else { 64 })
    }) {
        return Err(spdx_error("invalid JavaScript original digest"));
    }
    let facts = serde_json::to_string(
        &json!({"source_commit":commit,"original":original,"producer":producer}),
    )
    .map_err(spdx_error)?;
    let info = format!(
        "Verified original JavaScript facts {facts}; npm archive SHA512 integrity and structured installed-tree SHA256 are distinct. Rendered inputs are conservative post-Rollup, before esbuild minification; final function reachability is not asserted."
    );
    plain(&info)?;
    Ok(("NOASSERTION".to_owned(), info, checksum))
}

fn archive_sha512(integrity: &str) -> Result<String, Report<ReleaseError>> {
    let encoded = integrity
        .strip_prefix("sha512-")
        .ok_or_else(|| spdx_error("npm original integrity is not SHA512"))?;
    let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, encoded)
        .map_err(spdx_error)?;
    if bytes.len() != 64 {
        return Err(spdx_error("npm original SHA512 length differs"));
    }
    Ok(hex::encode(bytes))
}
