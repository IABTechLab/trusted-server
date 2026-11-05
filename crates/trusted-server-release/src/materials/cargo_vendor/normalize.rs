//! Producer-owned normalization of Git package manifests.
//!
//! A Git package whose `Cargo.toml` inherits from its workspace
//! (`version.workspace = true`, `serde = { workspace = true }`, `[lints]
//! workspace = true`) cannot be read from a directory source as committed.
//! This module resolves that inheritance itself, from the package manifest
//! and the workspace root manifest of the same verified Git tree, so no
//! caller-supplied manifest is ever trusted. It is pure: it reads only the
//! bytes it is handed.
//!
//! The rules follow the Cargo reference for workspace inheritance:
//!
//! - The workspace root is the package manifest itself when it has a
//!   `[workspace]` table, else the nearest enclosing directory whose
//!   `Cargo.toml` has one. A root whose `exclude` covers the package is
//!   refused rather than skipped.
//! - `[package]` fields listed in [`INHERITED_PACKAGE_FIELDS`] are copied from
//!   `[workspace.package]`.
//! - A dependency with `workspace = true` starts from the
//!   `[workspace.dependencies]` entry of the same key. Its `features` are
//!   added to the workspace features, its `optional` is taken as written, and
//!   `default-features = true` re-enables defaults the workspace turned off.
//!   A workspace `path` is rewritten relative to the package directory, as
//!   Cargo does; in the vendor layout every Git package is `vendor/<name>/`,
//!   so sibling paths keep resolving.
//! - `[lints] workspace = true` is replaced by `[workspace.lints]`.
//!
//! Anything else that could change resolution is refused: unknown or
//! path-relocating inherited fields (`readme`, `license-file`), `[patch]` and
//! `[replace]`, a `package.workspace` pointer, unsupported keys on either side
//! of an inherited dependency, and a member `default-features = false` that
//! Cargo would ignore or reject. The output is serialized with every table's
//! keys sorted, so equal inputs always yield equal bytes.

use error_stack::Report;
use toml::{Table, Value};

use super::{inherits_workspace, parse_manifest, require_identity};
use crate::error::ReleaseError;
use crate::materials::material_error;

/// `[package]` fields resolved from `[workspace.package]`.
pub(super) const INHERITED_PACKAGE_FIELDS: [&str; 14] = [
    "authors",
    "categories",
    "description",
    "documentation",
    "edition",
    "exclude",
    "homepage",
    "include",
    "keywords",
    "license",
    "publish",
    "repository",
    "rust-version",
    "version",
];
/// Inheritable fields that hold a workspace-relative path; never relocated.
const RELOCATING_PACKAGE_FIELDS: [&str; 2] = ["license-file", "readme"];
/// Dependency tables, at the top level and under `[target.<cfg>]`.
const DEPENDENCY_TABLES: [&str; 3] = ["build-dependencies", "dependencies", "dev-dependencies"];
/// Legacy spellings that are refused instead of merged.
const LEGACY_DEPENDENCY_TABLES: [&str; 2] = ["build_dependencies", "dev_dependencies"];
/// Keys a member may write beside `workspace = true` on a dependency.
const MEMBER_DEPENDENCY_KEYS: [&str; 4] = ["default-features", "features", "optional", "workspace"];
/// Keys a `[workspace.dependencies]` entry may hold.
const WORKSPACE_DEPENDENCY_KEYS: [&str; 5] =
    ["default-features", "features", "package", "path", "version"];
/// First lines of every producer-normalized manifest.
const HEADER: &str = "# Normalized by trusted-server-release from the verified Git tree:\n\
# workspace inheritance is resolved against the enclosing workspace root.\n\
# The committed manifest is kept beside this file as Cargo.toml.orig.\n\n";

/// The committed manifest of one locked Git package.
pub(super) struct PackageManifest<'a> {
    /// Locked package name.
    pub(super) name: &'a str,
    /// Locked package version.
    pub(super) version: &'a str,
    /// Repository-relative `/`-separated directory; empty at the root.
    pub(super) directory: &'a str,
    /// Committed manifest bytes.
    pub(super) bytes: &'a [u8],
}

/// A `Cargo.toml` found in a directory above the package, in the same tree.
#[derive(Debug)]
pub(super) struct AncestorManifest {
    /// Repository-relative `/`-separated directory; empty at the root.
    pub(super) directory: String,
    /// Committed manifest bytes.
    pub(super) bytes: Vec<u8>,
}

/// The workspace a package inherits from.
struct WorkspaceRoot {
    directory: String,
    table: Table,
}

/// Resolves the package's workspace inheritance and returns the new manifest.
///
/// `ancestors` must be ordered nearest first and come from the same verified
/// tree as the package.
///
/// # Errors
///
/// Returns [`ReleaseError::Material`] for a manifest that does not parse, no
/// enclosing workspace root, an inherited value the root does not define, an
/// unsupported or path-relocating inherited key, `[patch]`, `[replace]` or a
/// `package.workspace` pointer, a result that still inherits, or a result
/// whose name or version differs from the locked identity.
pub(super) fn normalize_manifest(
    package: &PackageManifest<'_>,
    ancestors: &[AncestorManifest],
) -> Result<Vec<u8>, Report<ReleaseError>> {
    let mut manifest = parse_manifest(package.bytes)?;
    for refused in ["patch", "replace"] {
        if manifest.contains_key(refused) {
            return Err(material_error(format!(
                "Git package {} declares [{refused}], which normalization does not support",
                package.name
            )));
        }
    }
    let root = match manifest.remove("workspace") {
        Some(Value::Table(table)) => WorkspaceRoot {
            directory: package.directory.to_owned(),
            table,
        },
        Some(_) => return Err(material_error("[workspace] is not a table")),
        None => enclosing_root(package, ancestors)?,
    };
    resolve_package_fields(&mut manifest, &root, package.name)?;
    resolve_dependency_tables(&mut manifest, &root, package.directory)?;
    resolve_lints(&mut manifest, &root)?;
    if inherits_workspace(&Value::Table(manifest.clone())) {
        return Err(material_error(format!(
            "Git package {} still inherits from its workspace after normalization",
            package.name
        )));
    }
    let manifest = sorted_table(manifest);
    let body = toml::to_string(&manifest).map_err(material_error)?;
    let bytes = format!("{HEADER}{body}").into_bytes();
    if parse_manifest(&bytes)? != manifest {
        return Err(material_error(
            "normalized manifest does not round-trip through TOML",
        ));
    }
    require_identity(&bytes, package.name, package.version)?;
    Ok(bytes)
}

/// Finds the nearest enclosing `[workspace]` root that does not exclude the package.
fn enclosing_root(
    package: &PackageManifest<'_>,
    ancestors: &[AncestorManifest],
) -> Result<WorkspaceRoot, Report<ReleaseError>> {
    for ancestor in ancestors {
        let mut table = parse_manifest(&ancestor.bytes)?;
        let package_table = table.get("package").and_then(Value::as_table);
        if package_table.is_some_and(|package| package.contains_key("workspace")) {
            return Err(material_error(format!(
                "ancestor manifest in {:?} points to another workspace root; unsupported",
                ancestor.directory
            )));
        }
        let has_package = package_table.is_some();
        let workspace = match table.remove("workspace") {
            Some(Value::Table(workspace)) => workspace,
            Some(_) => return Err(material_error("[workspace] is not a table")),
            None if has_package => continue,
            None => {
                return Err(material_error(format!(
                    "ancestor manifest in {:?} has neither [package] nor [workspace]",
                    ancestor.directory
                )));
            }
        };
        let relative = within(&ancestor.directory, package.directory).ok_or_else(|| {
            material_error("workspace candidate does not enclose the Git package")
        })?;
        if excluded(&workspace, &relative)? {
            return Err(material_error(format!(
                "Git package {} is excluded from its nearest workspace root; unsupported",
                package.name
            )));
        }
        return Ok(WorkspaceRoot {
            directory: ancestor.directory.clone(),
            table: workspace,
        });
    }
    Err(material_error(format!(
        "Git package {} inherits from a workspace, but no workspace root encloses it in the verified tree",
        package.name
    )))
}

/// Splits a repository-relative directory into its components.
fn components(directory: &str) -> Vec<&str> {
    directory
        .split('/')
        .filter(|part| !part.is_empty())
        .collect()
}

/// Returns `directory` relative to `ancestor`, if `ancestor` encloses it.
fn within(ancestor: &str, directory: &str) -> Option<Vec<String>> {
    let ancestor = components(ancestor);
    let directory = components(directory);
    directory
        .strip_prefix(ancestor.as_slice())
        .map(|rest| rest.iter().map(|part| (*part).to_owned()).collect())
}

/// Reports whether the root's `exclude` list covers `relative`.
fn excluded(workspace: &Table, relative: &[String]) -> Result<bool, Report<ReleaseError>> {
    let Some(exclude) = workspace.get("exclude") else {
        return Ok(false);
    };
    for entry in string_array(exclude, "workspace.exclude")? {
        let parts: Vec<&str> = components(&entry)
            .into_iter()
            .filter(|part| *part != ".")
            .collect();
        if parts.contains(&"..") {
            return Err(material_error(
                "workspace.exclude leaves the workspace root; unsupported",
            ));
        }
        if relative.len() >= parts.len()
            && parts
                .iter()
                .zip(relative)
                .all(|(left, right)| left == right)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Returns `value` as an array of strings.
fn string_array(value: &Value, label: &str) -> Result<Vec<String>, Report<ReleaseError>> {
    value
        .as_array()
        .and_then(|values| {
            values
                .iter()
                .map(|value| value.as_str().map(str::to_owned))
                .collect::<Option<Vec<_>>>()
        })
        .ok_or_else(|| material_error(format!("{label} is not an array of strings")))
}

/// Reports whether `value` is a `{ workspace = ... }` inheritance marker,
/// requiring exactly `{ workspace = true }` when it is.
fn is_marker(value: &Value, label: &str) -> Result<bool, Report<ReleaseError>> {
    let Some(table) = value.as_table() else {
        return Ok(false);
    };
    match table.get("workspace") {
        None => Ok(false),
        Some(Value::Boolean(true)) if table.len() == 1 => Ok(true),
        Some(_) => Err(material_error(format!(
            "{label} must be exactly {{ workspace = true }} to inherit"
        ))),
    }
}

/// Replaces inherited `[package]` fields with `[workspace.package]` values.
fn resolve_package_fields(
    manifest: &mut Table,
    root: &WorkspaceRoot,
    name: &str,
) -> Result<(), Report<ReleaseError>> {
    let package = manifest
        .get_mut("package")
        .and_then(Value::as_table_mut)
        .ok_or_else(|| material_error("Git manifest has no [package] table"))?;
    if package.get("name").and_then(Value::as_str) != Some(name) {
        return Err(material_error(format!(
            "Git manifest does not name package {name} as a plain string"
        )));
    }
    if package.contains_key("workspace") {
        return Err(material_error(
            "package.workspace pointers are not supported by normalization",
        ));
    }
    let defaults = root.table.get("package").and_then(Value::as_table);
    for (key, value) in package.iter_mut() {
        if !is_marker(value, &format!("package.{key}"))? {
            continue;
        }
        if RELOCATING_PACKAGE_FIELDS.contains(&key.as_str()) {
            return Err(material_error(format!(
                "inherited package.{key} names a workspace-relative path; unsupported"
            )));
        }
        if !INHERITED_PACKAGE_FIELDS.contains(&key.as_str()) {
            return Err(material_error(format!(
                "package.{key} is not an inheritable field"
            )));
        }
        *value = defaults
            .and_then(|defaults| defaults.get(key))
            .cloned()
            .ok_or_else(|| {
                material_error(format!(
                    "package.{key} inherits a value [workspace.package] does not define"
                ))
            })?;
    }
    Ok(())
}

/// Resolves inherited dependencies in every top-level and target table.
fn resolve_dependency_tables(
    manifest: &mut Table,
    root: &WorkspaceRoot,
    package_directory: &str,
) -> Result<(), Report<ReleaseError>> {
    let workspace_dependencies = root.table.get("dependencies").and_then(Value::as_table);
    let resolver = DependencyResolver {
        workspace_dependencies,
        workspace_directory: &root.directory,
        package_directory,
    };
    resolver.resolve_tables(manifest, "")?;
    if let Some(targets) = manifest.get_mut("target") {
        let targets = targets
            .as_table_mut()
            .ok_or_else(|| material_error("[target] is not a table"))?;
        for (cfg, target) in targets.iter_mut() {
            let target = target
                .as_table_mut()
                .ok_or_else(|| material_error(format!("target.{cfg} is not a table")))?;
            resolver.resolve_tables(target, &format!("target.{cfg}."))?;
        }
    }
    Ok(())
}

/// The workspace context one package's inherited dependencies resolve in.
struct DependencyResolver<'a> {
    workspace_dependencies: Option<&'a Table>,
    workspace_directory: &'a str,
    package_directory: &'a str,
}

impl DependencyResolver<'_> {
    /// Resolves the dependency tables directly inside `scope`.
    fn resolve_tables(&self, scope: &mut Table, prefix: &str) -> Result<(), Report<ReleaseError>> {
        for legacy in LEGACY_DEPENDENCY_TABLES {
            if scope.contains_key(legacy) {
                return Err(material_error(format!(
                    "{prefix}{legacy} is a legacy spelling normalization does not support"
                )));
            }
        }
        for section in DEPENDENCY_TABLES {
            let Some(dependencies) = scope.get_mut(section) else {
                continue;
            };
            let dependencies = dependencies
                .as_table_mut()
                .ok_or_else(|| material_error(format!("{prefix}{section} is not a table")))?;
            for (key, dependency) in dependencies.iter_mut() {
                let label = format!("{prefix}{section}.{key}");
                if dependency
                    .as_table()
                    .is_some_and(|table| table.contains_key("workspace"))
                {
                    *dependency = Value::Table(self.inherit(key, dependency, &label)?);
                }
            }
        }
        Ok(())
    }

    /// Builds one inherited dependency from its workspace entry.
    fn inherit(
        &self,
        key: &str,
        member: &Value,
        label: &str,
    ) -> Result<Table, Report<ReleaseError>> {
        let member = member
            .as_table()
            .ok_or_else(|| material_error(format!("{label} is not a table")))?;
        if member.get("workspace") != Some(&Value::Boolean(true)) {
            return Err(material_error(format!("{label} must set workspace = true")));
        }
        if let Some(unsupported) = member
            .keys()
            .find(|name| !MEMBER_DEPENDENCY_KEYS.contains(&name.as_str()))
        {
            return Err(material_error(format!(
                "{label} sets {unsupported} beside workspace = true; unsupported"
            )));
        }
        let mut resolved = match self.workspace_dependencies.and_then(|table| table.get(key)) {
            Some(Value::String(version)) => {
                Table::from_iter([("version".to_owned(), Value::String(version.clone()))])
            }
            Some(Value::Table(table)) => table.clone(),
            Some(_) => {
                return Err(material_error(format!(
                    "workspace.dependencies.{key} is neither a version nor a table"
                )));
            }
            None => {
                return Err(material_error(format!(
                    "{label} inherits workspace.dependencies.{key}, which is not defined"
                )));
            }
        };
        if let Some(unsupported) = resolved
            .keys()
            .find(|name| !WORKSPACE_DEPENDENCY_KEYS.contains(&name.as_str()))
        {
            return Err(material_error(format!(
                "workspace.dependencies.{key} sets {unsupported}; unsupported"
            )));
        }
        if let Some(path) = resolved.get("path") {
            let path = path.as_str().ok_or_else(|| {
                material_error(format!("workspace.dependencies.{key}.path is not a string"))
            })?;
            let relocated = relocate(self.workspace_directory, path, self.package_directory)?;
            resolved.insert("path".to_owned(), Value::String(relocated));
        }
        merge_default_features(&mut resolved, member, label)?;
        merge_features(&mut resolved, member, label)?;
        if let Some(optional) = member.get("optional") {
            if !optional.is_bool() {
                return Err(material_error(format!("{label}.optional is not a boolean")));
            }
            resolved.insert("optional".to_owned(), optional.clone());
        }
        Ok(resolved)
    }
}

/// Applies Cargo's `default-features` rule for an inherited dependency.
fn merge_default_features(
    resolved: &mut Table,
    member: &Table,
    label: &str,
) -> Result<(), Report<ReleaseError>> {
    let boolean = |table: &Table, side: &str| match table.get("default-features") {
        None => Ok(None),
        Some(Value::Boolean(value)) => Ok(Some(*value)),
        Some(_) => Err(material_error(format!(
            "{side} default-features of {label} is not a boolean"
        ))),
    };
    let workspace = boolean(resolved, "workspace")?;
    match (boolean(member, "member")?, workspace) {
        (Some(true), Some(false)) => {
            resolved.insert("default-features".to_owned(), Value::Boolean(true));
        }
        (Some(false), workspace) if workspace != Some(false) => {
            return Err(material_error(format!(
                "{label} sets default-features = false, which Cargo ignores unless the workspace entry does too"
            )));
        }
        _ => {}
    }
    Ok(())
}

/// Appends member `features` to the workspace features, keeping the first
/// occurrence of each, as Cargo's ordered feature set does.
fn merge_features(
    resolved: &mut Table,
    member: &Table,
    label: &str,
) -> Result<(), Report<ReleaseError>> {
    let workspace = match resolved.get("features") {
        Some(value) => string_array(value, &format!("workspace features of {label}"))?,
        None => Vec::new(),
    };
    let added = match member.get("features") {
        Some(value) => string_array(value, &format!("{label}.features"))?,
        None => Vec::new(),
    };
    let mut features: Vec<String> = Vec::new();
    for feature in workspace.into_iter().chain(added) {
        if !features.contains(&feature) {
            features.push(feature);
        }
    }
    if !features.is_empty() {
        resolved.insert(
            "features".to_owned(),
            Value::Array(features.into_iter().map(Value::String).collect()),
        );
    }
    Ok(())
}

/// Refuses a build script or target path that could leave the package directory.
///
/// Applies to every vendored Git manifest, inheriting or not: `package.build`
/// (when a path) and the `path` of `[lib]` and each `[[bin]]`, `[[example]]`,
/// `[[test]]` and `[[bench]]` must be relative and free of `..` components,
/// so a vendored package only builds from files inside its own directory.
///
/// # Errors
///
/// Returns [`ReleaseError::Material`] for a non-string, absolute or
/// parent-relative path, or a malformed target table.
pub(super) fn check_target_paths(manifest: &Table) -> Result<(), Report<ReleaseError>> {
    let mut paths: Vec<(String, &Value)> = Vec::new();
    if let Some(build) = manifest
        .get("package")
        .and_then(|package| package.get("build"))
        .filter(|build| !build.is_bool())
    {
        paths.push(("package.build".to_owned(), build));
    }
    if let Some(path) = manifest.get("lib").and_then(|lib| lib.get("path")) {
        paths.push(("lib.path".to_owned(), path));
    }
    for kind in ["bin", "example", "test", "bench"] {
        let Some(targets) = manifest.get(kind) else {
            continue;
        };
        let targets = targets
            .as_array()
            .ok_or_else(|| material_error(format!("[[{kind}]] is not an array of tables")))?;
        for (index, target) in targets.iter().enumerate() {
            if let Some(path) = target.get("path") {
                paths.push((format!("{kind}[{index}].path"), path));
            }
        }
    }
    for (label, value) in paths {
        let path = value
            .as_str()
            .ok_or_else(|| material_error(format!("{label} is not a string")))?;
        if path.starts_with('/') || path.split(['/', '\\']).any(|part| part == "..") {
            return Err(material_error(format!(
                "{label} {path:?} leaves the vendored package directory; unsupported"
            )));
        }
    }
    Ok(())
}

/// Rewrites a workspace-relative `path` relative to the package directory.
///
/// # Errors
///
/// Returns [`ReleaseError::Material`] for an empty, absolute or
/// backslash path, or one that leaves the repository.
pub(super) fn relocate(
    workspace_directory: &str,
    path: &str,
    package_directory: &str,
) -> Result<String, Report<ReleaseError>> {
    if path.is_empty() || path.starts_with('/') || path.contains('\\') {
        return Err(material_error(format!(
            "workspace dependency path {path:?} is not a relative repository path"
        )));
    }
    let mut target = components(workspace_directory);
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                target.pop().ok_or_else(|| {
                    material_error(format!(
                        "workspace dependency path {path:?} leaves the repository"
                    ))
                })?;
            }
            other => target.push(other),
        }
    }
    let package = components(package_directory);
    let common = target
        .iter()
        .zip(&package)
        .take_while(|(left, right)| left == right)
        .count();
    let mut relative: Vec<&str> = vec![".."; package.len() - common];
    relative.extend_from_slice(&target[common..]);
    Ok(if relative.is_empty() {
        ".".to_owned()
    } else {
        relative.join("/")
    })
}

/// Replaces `[lints] workspace = true` with `[workspace.lints]`.
fn resolve_lints(manifest: &mut Table, root: &WorkspaceRoot) -> Result<(), Report<ReleaseError>> {
    let Some(lints) = manifest.get_mut("lints") else {
        return Ok(());
    };
    if is_marker(lints, "[lints]")? {
        *lints = root.table.get("lints").cloned().ok_or_else(|| {
            material_error("[lints] inherits [workspace.lints], which is not defined")
        })?;
    }
    Ok(())
}

/// Rebuilds every table with its keys in sorted order.
fn sorted_table(table: Table) -> Table {
    let mut entries: Vec<(String, Value)> = table.into_iter().collect();
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    entries
        .into_iter()
        .map(|(key, value)| (key, sorted_value(value)))
        .collect()
}

fn sorted_value(value: Value) -> Value {
    match value {
        Value::Table(table) => Value::Table(sorted_table(table)),
        Value::Array(values) => Value::Array(values.into_iter().map(sorted_value).collect()),
        other => other,
    }
}
