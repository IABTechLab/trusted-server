//! Checked informational exports; rendered membership is conservative, not final-function attribution.

use std::collections::{BTreeMap, BTreeSet};

use error_stack::Report;
use serde_json::Value;
#[cfg(test)]
use sha2::Digest as _;

use super::{JsBuildCompleted, inventory_modules};
use crate::documents::{Role, parse_json, read_bounded_with_check};
use crate::error::ReleaseError;
use crate::preparation::preparation_error;

type Result<T> = core::result::Result<T, Report<ReleaseError>>;

/// Original collector facts exported only after the owning handoff is rehashed.
///
/// These observations grant no execution, qualification or complete-coverage authority.
#[derive(Debug, serde::Serialize)]
pub(crate) struct RetainedContributions {
    #[serde(skip)]
    raw_inventory: Vec<u8>,
    inventory_sha256: String,
    inventory: Inventory,
}

#[derive(Debug, serde::Serialize)]
struct Inventory {
    schema_version: u64,
    source_commit: String,
    modules: Vec<ModuleContributions>,
    materials: Vec<Material>,
    tools: Vec<Tool>,
}

/// Conservative post-Rollup origins for a complete final bundle.
#[derive(Debug, serde::Serialize)]
pub(crate) struct ModuleContributions {
    id: String,
    filename: String,
    size: u64,
    sha256: String,
    contributions: Vec<Contribution>,
}

/// A positively rendered origin; later minification can remove its code.
#[derive(Debug, serde::Serialize)]
pub(crate) struct Contribution {
    #[serde(flatten)]
    origin: Origin,
    stage: String,
    rendered_length: u64,
}

/// Checked original-source identity or a reviewed generated producer observation.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub(crate) enum Origin {
    Source {
        path: String,
        transform: String,
        sha256: String,
    },
    Npm {
        lock_path: String,
        package_name: String,
        package_version: String,
        integrity: String,
        path: String,
        transform: String,
        sha256: String,
    },
    Generated {
        id: String,
        plugin: String,
        owner_lock_path: String,
        generated_sha256: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        input: Option<Box<Origin>>,
    },
}

/// Retained material observations, with structured trees distinct from raw files.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub(crate) enum Material {
    Source {
        id: String,
        path: String,
        sha256: String,
    },
    NpmPackage {
        id: String,
        lock_path: String,
        name: String,
        version: String,
        integrity: String,
        installed_tree_sha256: String,
    },
    ToolDistribution {
        id: String,
        name: String,
        version: String,
        sha256: String,
    },
    LifecycleEvent {
        id: String,
        package_material_id: String,
        event: String,
        script_sha256: String,
    },
    LifecycleOutput {
        id: String,
        package_material_id: String,
        event: String,
        script_sha256: String,
        path: String,
        size: u64,
        sha256: String,
    },
}

/// A collector tool role joined to its measured distribution material.
#[derive(Debug, serde::Serialize)]
pub(crate) struct Tool {
    name: String,
    version: String,
    role: String,
    material_id: String,
}

impl RetainedContributions {
    /// Returns the exact original collector inventory bytes.
    pub(crate) fn raw_inventory(&self) -> &[u8] {
        &self.raw_inventory
    }
    /// Returns the approved collector source identity.
    pub(crate) fn source_commit(&self) -> &str {
        &self.inventory.source_commit
    }
    /// Returns the raw original inventory digest, not a reserialization digest.
    pub(crate) fn inventory_sha256(&self) -> &str {
        &self.inventory_sha256
    }
    /// Returns the checked conservative bundle observations.
    pub(crate) fn modules(&self) -> &[ModuleContributions] {
        &self.inventory.modules
    }
    /// Returns observed materials without implying they contribute runtime bytes.
    pub(crate) fn materials(&self) -> &[Material] {
        &self.inventory.materials
    }
    /// Returns checked tool-to-distribution joins from the original collector.
    pub(crate) fn tools(&self) -> &[Tool] {
        &self.inventory.tools
    }
}

impl ModuleContributions {
    /// Returns the checked complete bundle filename.
    pub(crate) fn filename(&self) -> &str {
        &self.filename
    }
    /// Returns complete final bundle length.
    pub(crate) fn size(&self) -> u64 {
        self.size
    }
    /// Returns the discovered module identity.
    pub(crate) fn id(&self) -> &str {
        &self.id
    }
    /// Returns the complete final bundle digest.
    pub(crate) fn sha256(&self) -> &str {
        &self.sha256
    }
    /// Returns conservative rendered origins, before final minification.
    pub(crate) fn contributions(&self) -> &[Contribution] {
        &self.contributions
    }
}

impl Contribution {
    /// Returns the observed contribution stage before later minification.
    pub(crate) fn stage(&self) -> &str {
        &self.stage
    }
    /// Returns checked ownership without implying final-function reachability.
    pub(crate) fn origin(&self) -> &Origin {
        &self.origin
    }
    /// Returns positive length at the recorded Rollup observation stage.
    pub(crate) fn rendered_length(&self) -> u64 {
        self.rendered_length
    }
}

impl Tool {
    /// Returns the observed tool identity.
    pub(crate) fn name(&self) -> &str {
        &self.name
    }
    /// Returns the observed tool version.
    pub(crate) fn version(&self) -> &str {
        &self.version
    }
    /// Returns its recorded build role.
    pub(crate) fn role(&self) -> &str {
        &self.role
    }
    /// Returns its checked distribution material identity.
    pub(crate) fn material_id(&self) -> &str {
        &self.material_id
    }
}

pub(super) fn export(completed: &JsBuildCompleted) -> Result<RetainedContributions> {
    let root = completed.rehash_for_handoff()?;
    let raw = read_bounded_with_check(&root.join("tsjs-inventory.json"), Role::Metadata, &|| {
        completed.attempt.check()
    })?;
    if raw.sha256 != completed.inventory_sha256 {
        return Err(preparation_error(
            "original JS inventory changed during export",
        ));
    }
    let value = parse_json(&raw.bytes, Role::Metadata)?;
    fields(
        &value,
        &[
            "schema_version",
            "source_commit",
            "modules",
            "materials",
            "tools",
        ],
    )?;
    if value["schema_version"] != 1 || value["source_commit"] != completed.source_commit {
        return Err(preparation_error(
            "JS contribution source or schema differs",
        ));
    }
    let expected: Vec<_> = completed.modules.keys().cloned().collect();
    let modules = inventory_modules(&raw.bytes, &expected)?;
    for (id, parsed) in &modules {
        let owned = &completed.modules[id];
        if parsed.filename != owned.filename
            || parsed.size != owned.size
            || parsed.sha256 != owned.sha256
        {
            return Err(preparation_error(
                "contribution module differs from owned module",
            ));
        }
    }
    let material_values = array(&value["materials"])?;
    let mut material_ids = BTreeMap::new();
    let mut materials = Vec::new();
    for entry in material_values {
        let id = text(entry, "id")?;
        if material_ids.insert(id.clone(), entry).is_some() {
            return Err(preparation_error("duplicate JS material identity"));
        }
        materials.push(material(entry)?);
    }
    for material in &materials {
        validate_material_join(material, &material_ids)?;
    }
    let mut exported_modules = Vec::new();
    let mut original_files = BTreeMap::new();
    for entry in array(&value["modules"])? {
        fields(
            entry,
            &["id", "filename", "size", "sha256", "contributions"],
        )?;
        let mut contributions = Vec::new();
        let mut origins = BTreeSet::new();
        for observed in array(&entry["contributions"])? {
            if observed["stage"] != "rollup-rendered" {
                return Err(preparation_error("unknown JS contribution stage"));
            }
            let rendered_length = observed["rendered_length"]
                .as_u64()
                .filter(|n| *n > 0 && *n <= 32 * 1024 * 1024)
                .ok_or_else(|| preparation_error("invalid rendered contribution length"))?;
            let origin = origin(observed, &material_ids, true)?;
            consistent_originals(&origin, &mut original_files)?;
            let identity = origin_key(&origin);
            if !origins.insert(identity) {
                return Err(preparation_error("duplicate JS rendered origin"));
            }
            contributions.push(Contribution {
                origin,
                stage: "rollup-rendered".to_owned(),
                rendered_length,
            });
        }
        let module = &modules[&text(entry, "id")?];
        exported_modules.push(ModuleContributions {
            id: text(entry, "id")?,
            filename: module.filename.clone(),
            size: module.size,
            sha256: module.sha256.clone(),
            contributions,
        });
    }
    let mut names = BTreeSet::new();
    let mut tools = Vec::new();
    for entry in array(&value["tools"])? {
        fields(entry, &["name", "version", "role", "material_id"])?;
        let tool = Tool {
            name: text(entry, "name")?,
            version: text(entry, "version")?,
            role: text(entry, "role")?,
            material_id: text(entry, "material_id")?,
        };
        let material = material_ids
            .get(&tool.material_id)
            .ok_or_else(|| preparation_error("missing JS tool material"))?;
        if material["kind"] != "tool-distribution"
            || material["name"] != tool.name
            || material["version"] != tool.version
            || ![
                "runtime",
                "package-manager",
                "bundler",
                "minifier",
                "test-runner",
            ]
            .contains(&tool.role.as_str())
            || !names.insert(tool.name.clone())
        {
            return Err(preparation_error("invalid JS tool ownership"));
        }
        tools.push(tool);
    }
    completed.rehash_for_handoff()?;
    Ok(RetainedContributions {
        raw_inventory: raw.bytes,
        inventory_sha256: raw.sha256,
        inventory: Inventory {
            schema_version: 1,
            source_commit: completed.source_commit.clone(),
            modules: exported_modules,
            materials,
            tools,
        },
    })
}

fn origin(value: &Value, materials: &BTreeMap<String, &Value>, rendered: bool) -> Result<Origin> {
    let mut keys = match value["kind"].as_str() {
        Some("source") => vec!["kind", "path", "transform", "sha256"],
        Some("npm") => vec![
            "kind",
            "lock_path",
            "package_name",
            "package_version",
            "integrity",
            "path",
            "transform",
            "sha256",
        ],
        Some("generated") if rendered => {
            let mut keys = vec![
                "kind",
                "id",
                "plugin",
                "owner_lock_path",
                "generated_sha256",
            ];
            if value.get("input").is_some() {
                keys.push("input");
            }
            keys
        }
        _ => return Err(preparation_error("unknown JS origin kind")),
    };
    if rendered {
        keys.extend(["stage", "rendered_length"]);
    }
    fields(value, &keys)?;
    if value["kind"] == "generated" {
        let id = value["id"]
            .as_str()
            .filter(|v| v.len() <= 4096)
            .ok_or_else(|| preparation_error("invalid virtual origin identity"))?
            .to_owned();
        let plugin = text(value, "plugin")?;
        let owner_lock_path = text(value, "owner_lock_path")?;
        let owner = materials
            .get(&format!("npm:{owner_lock_path}"))
            .ok_or_else(|| preparation_error("missing generated producer material"))?;
        if owner_lock_path != "node_modules/vite"
            || owner["kind"] != "npm-package"
            || owner["name"] != "vite"
        {
            return Err(preparation_error("unreviewed generated owner"));
        }
        let input = value
            .get("input")
            .map(|input| origin(input, materials, false).map(Box::new))
            .transpose()?;
        let valid = match (&*plugin, input.as_deref()) {
            ("commonjs", None) => id == "\0commonjsHelpers.js",
            ("vite:build-import-analysis", None) => id == "\0vite/preload-helper.js",
            ("commonjs", Some(Origin::Source { path, .. })) => {
                id == format!("\0{path}?commonjs-es-import")
            }
            (
                "commonjs",
                Some(Origin::Npm {
                    lock_path, path, ..
                }),
            ) => id == format!("\0{lock_path}/{path}?commonjs-es-import"),
            _ => false,
        };
        if !valid {
            return Err(preparation_error(
                "unknown or inconsistent generated producer",
            ));
        }
        return Ok(Origin::Generated {
            id,
            plugin,
            owner_lock_path,
            generated_sha256: digest(value, "generated_sha256")?,
            input,
        });
    }
    let path = checked_path(value, "path")?;
    let transform = text(value, "transform")?;
    if !["identity", "raw", "inline"].contains(&transform.as_str()) {
        return Err(preparation_error("unknown JS source transformation"));
    }
    let sha256 = digest(value, "sha256")?;
    if value["kind"] == "source" {
        if path.split('/').any(|part| part == "node_modules") {
            return Err(preparation_error("unmapped npm source ownership"));
        }
        return Ok(Origin::Source {
            path,
            transform,
            sha256,
        });
    }
    let lock_path = checked_path(value, "lock_path")?;
    let package_name = text(value, "package_name")?;
    let package_version = text(value, "package_version")?;
    let integrity = integrity(value)?;
    let owner = materials
        .get(&format!("npm:{lock_path}"))
        .ok_or_else(|| preparation_error("missing npm contribution material"))?;
    if owner["kind"] != "npm-package"
        || owner["lock_path"] != lock_path
        || owner["name"] != package_name
        || owner["version"] != package_version
        || owner["integrity"] != integrity
    {
        return Err(preparation_error(
            "npm contribution differs from installed ownership",
        ));
    }
    if path.split('/').any(|part| part == "node_modules") {
        return Err(preparation_error("nested npm input needs its own owner"));
    }
    Ok(Origin::Npm {
        lock_path,
        package_name,
        package_version,
        integrity,
        path,
        transform,
        sha256,
    })
}

fn origin_key(origin: &Origin) -> String {
    match origin {
        Origin::Source {
            path, transform, ..
        } => format!("source:{path}:{transform}"),
        Origin::Npm {
            lock_path,
            path,
            transform,
            ..
        } => format!("npm:{lock_path}:{path}:{transform}"),
        Origin::Generated { id, .. } => format!("generated:{id}"),
    }
}

fn consistent_originals(origin: &Origin, originals: &mut BTreeMap<String, String>) -> Result<()> {
    let (key, sha256) = match origin {
        Origin::Source { path, sha256, .. } => (format!("source:{path}"), sha256),
        Origin::Npm {
            lock_path,
            path,
            sha256,
            ..
        } => (format!("npm:{lock_path}:{path}"), sha256),
        Origin::Generated {
            id,
            generated_sha256,
            input,
            ..
        } => {
            if let Some(input) = input {
                consistent_originals(input, originals)?;
            }
            (format!("generated:{id}"), generated_sha256)
        }
    };
    if originals
        .insert(key, sha256.clone())
        .is_some_and(|previous| previous != *sha256)
    {
        return Err(preparation_error(
            "conflicting original JS contribution bytes",
        ));
    }
    Ok(())
}

fn material(value: &Value) -> Result<Material> {
    let id = text(value, "id")?;
    Ok(match value["kind"].as_str() {
        Some("source") => {
            fields(value, &["kind", "id", "path", "sha256"])?;
            let path = checked_path(value, "path")?;
            if id != format!("source:{path}") {
                return Err(preparation_error("invalid source material identity"));
            }
            Material::Source {
                id,
                path,
                sha256: digest(value, "sha256")?,
            }
        }
        Some("npm-package") => {
            fields(
                value,
                &[
                    "kind",
                    "id",
                    "lock_path",
                    "name",
                    "version",
                    "integrity",
                    "installed_tree_sha256",
                ],
            )?;
            let lock_path = checked_path(value, "lock_path")?;
            let name = text(value, "name")?;
            if id != format!("npm:{lock_path}")
                || lock_path.rsplit("node_modules/").next() != Some(name.as_str())
                || !lock_path.starts_with("node_modules/")
            {
                return Err(preparation_error("invalid npm material identity"));
            }
            Material::NpmPackage {
                id,
                lock_path,
                name,
                version: text(value, "version")?,
                integrity: integrity(value)?,
                installed_tree_sha256: digest(value, "installed_tree_sha256")?,
            }
        }
        Some("tool-distribution") => {
            fields(value, &["kind", "id", "name", "version", "sha256"])?;
            Material::ToolDistribution {
                id,
                name: text(value, "name")?,
                version: text(value, "version")?,
                sha256: digest(value, "sha256")?,
            }
        }
        Some("lifecycle-event") => {
            fields(
                value,
                &[
                    "kind",
                    "id",
                    "package_material_id",
                    "event",
                    "script_sha256",
                ],
            )?;
            Material::LifecycleEvent {
                id,
                package_material_id: text(value, "package_material_id")?,
                event: event(value)?,
                script_sha256: digest(value, "script_sha256")?,
            }
        }
        Some("lifecycle-output") => {
            fields(
                value,
                &[
                    "kind",
                    "id",
                    "package_material_id",
                    "event",
                    "script_sha256",
                    "path",
                    "size",
                    "sha256",
                ],
            )?;
            Material::LifecycleOutput {
                id,
                package_material_id: text(value, "package_material_id")?,
                event: event(value)?,
                script_sha256: digest(value, "script_sha256")?,
                path: checked_path(value, "path")?,
                size: value["size"]
                    .as_u64()
                    .filter(|n| *n <= 32 * 1024 * 1024)
                    .ok_or_else(|| preparation_error("invalid lifecycle output size"))?,
                sha256: digest(value, "sha256")?,
            }
        }
        _ => return Err(preparation_error("unknown JS material kind")),
    })
}

fn validate_material_join(material: &Material, materials: &BTreeMap<String, &Value>) -> Result<()> {
    match material {
        Material::LifecycleEvent {
            package_material_id,
            ..
        }
        | Material::LifecycleOutput {
            package_material_id,
            ..
        } => {
            let owner = materials
                .get(package_material_id)
                .ok_or_else(|| preparation_error("missing lifecycle package owner"))?;
            if owner["kind"] != "npm-package" {
                return Err(preparation_error("invalid lifecycle package owner"));
            }
            if let Material::LifecycleOutput {
                event,
                script_sha256,
                path,
                ..
            } = material
            {
                let lock = owner["lock_path"]
                    .as_str()
                    .ok_or_else(|| preparation_error("missing lifecycle lock path"))?;
                if !path.starts_with(&format!("{lock}/")) {
                    return Err(preparation_error("lifecycle output escapes owner"));
                }
                let joined = materials.values().any(|entry| {
                    entry["kind"] == "lifecycle-event"
                        && entry["package_material_id"] == *package_material_id
                        && entry["event"] == *event
                        && entry["script_sha256"] == *script_sha256
                });
                if !joined {
                    return Err(preparation_error("lifecycle output lacks observed event"));
                }
                if materials.values().any(|entry| {
                    entry["kind"] == "npm-package"
                        && entry["id"] != *package_material_id
                        && entry["lock_path"].as_str().is_some_and(|nested| {
                            nested.starts_with(&format!("{lock}/"))
                                && path.starts_with(&format!("{nested}/"))
                        })
                }) {
                    return Err(preparation_error(
                        "lifecycle output belongs to nested package",
                    ));
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn fields(value: &Value, expected: &[&str]) -> Result<()> {
    let object = value
        .as_object()
        .ok_or_else(|| preparation_error("JS record is not an object"))?;
    if object.len() != expected.len() || expected.iter().any(|key| !object.contains_key(*key)) {
        return Err(preparation_error("unknown or missing JS record fields"));
    }
    Ok(())
}
fn text(value: &Value, key: &str) -> Result<String> {
    value[key]
        .as_str()
        .filter(|v| !v.is_empty() && v.len() <= 4096 && !v.chars().any(char::is_control))
        .map(str::to_owned)
        .ok_or_else(|| preparation_error("invalid JS identity text"))
}
fn checked_path(value: &Value, key: &str) -> Result<String> {
    let path = text(value, key)?;
    if path.starts_with('/')
        || path.contains('\\')
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(preparation_error("unsafe JS origin path"));
    }
    Ok(path)
}
fn digest(value: &Value, key: &str) -> Result<String> {
    let digest = text(value, key)?;
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(preparation_error("invalid JS SHA-256"));
    }
    Ok(digest)
}
fn integrity(value: &Value) -> Result<String> {
    let integrity = text(value, "integrity")?;
    if integrity.len() != 95
        || !integrity.starts_with("sha512-")
        || !integrity.ends_with("==")
        || !integrity.as_bytes()[7..93]
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || *b == b'+' || *b == b'/')
    {
        return Err(preparation_error("invalid npm archive integrity"));
    }
    Ok(integrity)
}
fn event(value: &Value) -> Result<String> {
    let event = text(value, "event")?;
    if !["preinstall", "install", "postinstall", "prepare"].contains(&event.as_str()) {
        return Err(preparation_error("unknown lifecycle event"));
    }
    Ok(event)
}
fn array(value: &Value) -> Result<&Vec<Value>> {
    value
        .as_array()
        .ok_or_else(|| preparation_error("missing JS observation array"))
}

#[cfg(test)]
mod tests;

#[cfg(test)]
impl JsBuildCompleted {
    /// Creates a rehashed fictional owner with checked collector variants.
    pub(crate) fn fixture_for_projection(commit: &str) -> Self {
        let mut completed = tests::fixture();
        completed.source_commit = commit.to_owned();
        let mut inventory = tests::rich_inventory(&completed);
        inventory["source_commit"] = serde_json::json!(commit);
        let bytes = serde_json::to_vec(&inventory).expect("should encode fictional inventory");
        std::fs::write(
            completed.directory.path().join("tsjs-inventory.json"),
            &bytes,
        )
        .expect("should write fictional original inventory");
        completed.inventory_sha256 = hex::encode(sha2::Sha256::digest(&bytes));
        completed
    }
}
