//! Observed Cargo compilation units, without granting material or release authority.
//!
//! Metadata describes package identities and potential graph membership. Only
//! compiler messages describe compiled units and their effective features.
//! Native output validation and final executable membership are separate gates.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use error_stack::Report;
use serde::Serialize;
use serde_json::{Value, json};

use crate::documents::{Role, parse_json};
use crate::error::ReleaseError;
use crate::policy::ReleaseTarget;
use crate::recipe::observed_cargo_messages;

mod capture;
pub(crate) mod external;
pub(crate) mod graph;
pub(crate) mod javascript;
pub(crate) mod native;
pub(crate) mod origin;
pub(crate) mod platform;
pub(crate) use capture::{CargoCapture, CompositionView, RetainedCompositionExport};

const MAX_PACKAGES: usize = 16_384;
const MAX_MESSAGES: usize = 65_536;

fn composition_error(detail: impl std::fmt::Display) -> Report<ReleaseError> {
    Report::new(ReleaseError::Composition).attach(detail.to_string())
}

fn text<'a>(value: &'a Value, label: &str) -> Result<&'a str, Report<ReleaseError>> {
    value
        .as_str()
        .filter(|text| {
            !text.is_empty() && text.len() <= 8192 && !text.chars().any(char::is_control)
        })
        .ok_or_else(|| composition_error(format!("{label} is not bounded plain text")))
}

fn array<'a>(value: &'a Value, label: &str) -> Result<&'a [Value], Report<ReleaseError>> {
    value
        .as_array()
        .map(Vec::as_slice)
        .ok_or_else(|| composition_error(format!("{label} is not an array")))
}

fn strings(value: &Value, label: &str, unique: bool) -> Result<Vec<String>, Report<ReleaseError>> {
    let values = array(value, label)?
        .iter()
        .map(|value| text(value, label).map(str::to_owned))
        .collect::<Result<Vec<_>, _>>()?;
    if unique && values.iter().collect::<BTreeSet<_>>().len() != values.len() {
        return Err(composition_error(format!("{label} contains duplicates")));
    }
    Ok(values)
}

fn normal_path(path: &Path) -> Result<(), Report<ReleaseError>> {
    if !path.is_absolute()
        || path == Path::new("/")
        || path.components().collect::<PathBuf>().as_os_str() != path.as_os_str()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir))
    {
        return Err(composition_error(
            "observation path is not absolute and normal",
        ));
    }
    Ok(())
}

fn path(value: &Value, label: &str) -> Result<PathBuf, Report<ReleaseError>> {
    let path = PathBuf::from(text(value, label)?);
    normal_path(&path)?;
    Ok(path)
}

/// Explicit-target output layout of one already selected build.
pub(crate) struct CargoLayout {
    target: ReleaseTarget,
    target_dir: PathBuf,
    checkout: PathBuf,
}

impl CargoLayout {
    /// Checks the namespace paths used to classify compiler outputs.
    ///
    /// # Errors
    ///
    /// Rejects relative, root-only, overlapping or non-normal directories.
    pub(crate) fn new(
        target: ReleaseTarget,
        target_dir: PathBuf,
        checkout: PathBuf,
    ) -> Result<Self, Report<ReleaseError>> {
        normal_path(&target_dir)?;
        normal_path(&checkout)?;
        if target_dir.starts_with(&checkout) || checkout.starts_with(&target_dir) {
            return Err(composition_error("composition roots overlap"));
        }
        Ok(Self {
            target,
            target_dir,
            checkout,
        })
    }

    fn context(&self, path: &Path) -> Result<UnitContext, Report<ReleaseError>> {
        normal_path(path)?;
        for (context, root) in [
            (
                UnitContext::Target,
                self.target_dir.join(self.target.triple()).join("release"),
            ),
            (UnitContext::Host, self.target_dir.join("release")),
        ] {
            if path
                .strip_prefix(&root)
                .is_ok_and(|relative| !relative.as_os_str().is_empty())
            {
                return Ok(context);
            }
        }
        Err(composition_error(
            "compiler output is outside both selected release layouts",
        ))
    }
}

/// The host or explicit target context observed in Cargo's output paths.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum UnitContext {
    Host,
    Target,
}

/// A package observed in actual compilation, identified by its exact Cargo ID.
#[derive(Debug, Serialize)]
pub(crate) struct RustPackage {
    id: String,
    name: String,
    version: String,
    source: Option<String>,
    manifest_path: PathBuf,
    license: Option<String>,
}

/// One compiler observation, including its actual features and output context.
#[derive(Debug, Serialize)]
pub(crate) struct RustUnit {
    package_id: String,
    target_name: String,
    target_kind: Vec<String>,
    crate_types: Vec<String>,
    context: UnitContext,
    features: Vec<String>,
    profile: Value,
    filenames: Vec<PathBuf>,
    executable: Option<PathBuf>,
}

/// A build-script execution observation, without invented per-unit feature joins.
#[derive(Debug, Serialize)]
pub(crate) struct BuildScriptObservation {
    package_id: String,
    context: UnitContext,
    out_dir: PathBuf,
    linked_libs: Vec<String>,
    linked_paths: Vec<String>,
    cfgs: Vec<String>,
    environment: Vec<(String, String)>,
}

/// Conservative observed membership; not a verified material-origin inventory.
#[derive(Debug, Serialize)]
pub(crate) struct CargoObservations {
    packages: Vec<RustPackage>,
    units: Vec<RustUnit>,
    build_scripts: Vec<BuildScriptObservation>,
}

impl CargoObservations {
    /// Returns packages actually observed in compiler messages.
    pub(crate) fn packages(&self) -> &[RustPackage] {
        &self.packages
    }

    /// Returns actual units without promoting metadata feature unions.
    pub(crate) fn units(&self) -> &[RustUnit] {
        &self.units
    }

    /// Returns observed build-script executions.
    pub(crate) fn build_scripts(&self) -> &[BuildScriptObservation] {
        &self.build_scripts
    }
}

impl RustPackage {
    /// Returns the exact opaque Cargo package identity.
    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    /// Returns the checked package name.
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// Returns the checked package version.
    pub(crate) fn version(&self) -> &str {
        &self.version
    }

    /// Returns the metadata license declaration, without a license conclusion.
    pub(crate) fn license(&self) -> Option<&str> {
        self.license.as_deref()
    }
}

impl RustUnit {
    /// Returns the exact observed package identity.
    pub(crate) fn package_id(&self) -> &str {
        &self.package_id
    }

    /// Returns the compiler target name.
    pub(crate) fn target_name(&self) -> &str {
        &self.target_name
    }

    /// Returns compiler target kinds, including host proc macros and build scripts.
    pub(crate) fn target_kind(&self) -> &[String] {
        &self.target_kind
    }

    /// Returns the observed crate types.
    pub(crate) fn crate_types(&self) -> &[String] {
        &self.crate_types
    }

    /// Returns the observed host or selected target output context.
    pub(crate) fn context(&self) -> UnitContext {
        self.context
    }

    /// Returns effective features from this compiler message only.
    pub(crate) fn features(&self) -> &[String] {
        &self.features
    }
}

fn package(value: &Value) -> Result<RustPackage, Report<ReleaseError>> {
    let optional = |key: &str| match value.get(key) {
        Some(Value::Null) => Ok(None),
        Some(value) => text(value, key).map(|text| Some(text.to_owned())),
        None => Err(composition_error(format!("package has no {key}"))),
    };
    Ok(RustPackage {
        id: text(&value["id"], "package ID")?.to_owned(),
        name: text(&value["name"], "package name")?.to_owned(),
        version: text(&value["version"], "package version")?.to_owned(),
        source: optional("source")?,
        manifest_path: path(&value["manifest_path"], "package manifest")?,
        license: optional("license")?,
    })
}

fn unit(
    message: &Value,
    package: &Value,
    layout: &CargoLayout,
) -> Result<RustUnit, Report<ReleaseError>> {
    if message["fresh"].as_bool() != Some(false)
        || message["profile"]["test"].as_bool() != Some(false)
    {
        return Err(composition_error(
            "compiler unit is cached or is not a non-test build",
        ));
    }
    if path(&message["manifest_path"], "unit manifest")?
        != path(&package["manifest_path"], "package manifest")?
    {
        return Err(composition_error(
            "compiler unit manifest differs from its package",
        ));
    }
    let target = &message["target"];
    let target_name = text(&target["name"], "target name")?.to_owned();
    let target_kind = strings(&target["kind"], "target kinds", true)?;
    let crate_types = strings(&target["crate_types"], "crate types", true)?;
    if target_kind.is_empty() || crate_types.is_empty() {
        return Err(composition_error(
            "compiler target has no kind or crate type",
        ));
    }
    let matching = array(&package["targets"], "metadata targets")?
        .iter()
        .filter(|candidate| {
            ["name", "kind", "crate_types", "src_path"]
                .into_iter()
                .all(|field| candidate[field] == target[field])
        })
        .count();
    path(&target["src_path"], "target source")?;
    if matching != 1 {
        return Err(composition_error(
            "compiler target has no unique metadata target",
        ));
    }
    let filenames = array(&message["filenames"], "compiler filenames")?
        .iter()
        .map(|value| path(value, "compiler filename"))
        .collect::<Result<Vec<_>, _>>()?;
    let first = filenames
        .first()
        .ok_or_else(|| composition_error("compiler unit has no output"))?;
    let context = layout.context(first)?;
    for filename in &filenames {
        if layout.context(filename)? != context {
            return Err(composition_error(
                "one compiler unit spans host and target layouts",
            ));
        }
    }
    if target_kind
        .iter()
        .any(|kind| matches!(kind.as_str(), "custom-build" | "proc-macro"))
        && context != UnitContext::Host
    {
        return Err(composition_error(
            "build script or proc macro is not a host unit",
        ));
    }
    let executable = match &message["executable"] {
        Value::Null => None,
        value => {
            let executable = path(value, "unit executable")?;
            if layout.context(&executable)? != context || !filenames.contains(&executable) {
                return Err(composition_error("executable is outside its unit outputs"));
            }
            Some(executable)
        }
    };
    let mut features = strings(&message["features"], "effective features", true)?;
    features.sort();
    Ok(RustUnit {
        package_id: text(&message["package_id"], "unit package ID")?.to_owned(),
        target_name,
        target_kind,
        crate_types,
        context,
        features,
        profile: message["profile"].clone(),
        filenames,
        executable,
    })
}

fn build_script(
    message: &Value,
    layout: &CargoLayout,
) -> Result<BuildScriptObservation, Report<ReleaseError>> {
    let out_dir = path(&message["out_dir"], "build script OUT_DIR")?;
    let context = layout.context(&out_dir)?;
    let environment = array(&message["env"], "build script environment")?
        .iter()
        .map(|entry| {
            let fields = array(entry, "build script environment pair")?;
            let [name, value] = fields else {
                return Err(composition_error("environment entry is not one pair"));
            };
            let name = text(name, "build script environment name")?.to_owned();
            let value = value
                .as_str()
                .filter(|value| value.len() <= 65_536 && !value.contains('\0'))
                .ok_or_else(|| composition_error("build script environment value is invalid"))?
                .to_owned();
            Ok((name, value))
        })
        .collect::<Result<Vec<_>, Report<ReleaseError>>>()?;
    Ok(BuildScriptObservation {
        package_id: text(&message["package_id"], "build script package ID")?.to_owned(),
        context,
        out_dir,
        linked_libs: strings(&message["linked_libs"], "linked libraries", false)?,
        linked_paths: strings(&message["linked_paths"], "link search paths", false)?,
        cfgs: strings(&message["cfgs"], "build script cfg", false)?,
        environment,
    })
}

/// Joins strict build observations with exact metadata package/target identities.
///
/// Only compiler messages supply unit features and contributor membership.
/// Metadata graph unions and uncompiled packages never create units. The
/// result describes observations, not verified native/material coverage.
///
/// # Errors
///
/// Rejects malformed/bounded-input violations, ambiguous joins, cached/test
/// units, invalid output contexts, duplicate units or unobserved build scripts.
pub(crate) fn observe_cargo(
    metadata: &[u8],
    stdout: &[u8],
    layout: &CargoLayout,
) -> Result<CargoObservations, Report<ReleaseError>> {
    let meta = parse_json(metadata, Role::CargoObservation)
        .map_err(|error| error.change_context(ReleaseError::Composition))?;
    if meta["version"].as_u64() != Some(1)
        || path(&meta["workspace_root"], "metadata workspace")? != layout.checkout
        || path(&meta["target_directory"], "metadata target directory")? != layout.target_dir
    {
        return Err(composition_error(
            "metadata version or build layout differs from selection",
        ));
    }
    let records = array(&meta["packages"], "metadata packages")?;
    if records.len() > MAX_PACKAGES {
        return Err(composition_error("too many metadata packages"));
    }
    let mut packages = BTreeMap::new();
    for record in records {
        let id = text(&record["id"], "metadata package ID")?;
        if packages.insert(id, record).is_some() {
            return Err(composition_error("duplicate metadata package ID"));
        }
    }
    let messages = observed_cargo_messages(stdout)
        .map_err(|error| error.change_context(ReleaseError::Composition))?;
    if messages.len() > MAX_MESSAGES {
        return Err(composition_error("too many Cargo messages"));
    }
    let mut units = BTreeMap::new();
    let mut scripts = Vec::new();
    let mut observed_packages = BTreeSet::new();
    for message in &messages {
        match message["reason"].as_str() {
            Some("compiler-artifact") => {
                let id = text(&message["package_id"], "unit package ID")?;
                let package = packages.get(id).ok_or_else(|| {
                    composition_error(format!(
                        "compiled package {id:?} is absent from metadata catalog ({} packages)",
                        packages.len()
                    ))
                })?;
                let unit = unit(message, package, layout)?;
                let key = serde_json::to_vec(&json!([
                    unit.package_id,
                    unit.target_name,
                    unit.target_kind,
                    unit.crate_types,
                    unit.context,
                    unit.features,
                    unit.profile
                ]))
                .map_err(composition_error)?;
                if units.insert(key, unit).is_some() {
                    return Err(composition_error("duplicate compiler unit identity"));
                }
                observed_packages.insert(id);
            }
            Some("build-script-executed") => scripts.push(build_script(message, layout)?),
            Some("compiler-message" | "build-finished") => {}
            _ => return Err(composition_error("unknown Cargo observation reason")),
        }
    }
    let mut script_ids = BTreeSet::new();
    for script in &scripts {
        if !units.values().any(|unit| {
            unit.package_id == script.package_id
                && unit.context == UnitContext::Host
                && unit.target_kind.iter().any(|kind| kind == "custom-build")
        }) || !script_ids.insert((&script.package_id, &script.out_dir))
        {
            return Err(composition_error(
                "build script execution has no unique observed compilation",
            ));
        }
    }
    scripts.sort_by(|a, b| (&a.package_id, &a.out_dir).cmp(&(&b.package_id, &b.out_dir)));
    let packages = observed_packages
        .into_iter()
        .map(|id| package(packages[id]))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(CargoObservations {
        packages,
        units: units.into_values().collect(),
        build_scripts: scripts,
    })
}

#[cfg(test)]
mod tests;
