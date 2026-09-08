//! Workspace package-to-README equality checks.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use error_stack::{Report, ResultExt as _};
use toml::Value;

use crate::repository::{NormalizedRelativePath, Repository};

const MAXIMUM_MANIFEST_BYTES: usize = 256 * 1024;
const MAXIMUM_README_BYTES: usize = 4 * 1024 * 1024;
const MAXIMUM_PACKAGES: usize = 256;

/// Failure while reconciling workspace packages and crate READMEs.
#[derive(Debug, derive_more::Display)]
#[display("invalid workspace README inventory: {detail}")]
pub struct ReadmeError {
    detail: String,
}

impl core::error::Error for ReadmeError {}

/// Validate an in-memory workspace package and README inventory.
///
/// `files` must contain every tracked top-level `crates/*/Cargo.toml` and
/// `crates/*/README.md` record. Each workspace member must declare the exact
/// package-relative `README.md`, and no unlisted crate record is accepted.
///
/// # Errors
///
/// Returns an error for malformed manifests, unsafe or duplicate members,
/// missing README metadata or files, and extra unlisted crate records.
pub fn validate_inventory(
    root_manifest: &str,
    files: &BTreeMap<String, Vec<u8>>,
) -> Result<(), Report<ReadmeError>> {
    if root_manifest.len() > MAXIMUM_MANIFEST_BYTES {
        return Err(readme_error("root Cargo.toml exceeds the byte limit"));
    }
    let root = toml::from_str::<Value>(root_manifest)
        .map_err(|error| readme_error(format!("root Cargo.toml is malformed: {error}")))?;
    let members = root
        .get("workspace")
        .and_then(|workspace| workspace.get("members"))
        .and_then(Value::as_array)
        .ok_or_else(|| readme_error("root Cargo.toml has no workspace.members array"))?;
    if members.is_empty() || members.len() > MAXIMUM_PACKAGES {
        return Err(readme_error("workspace member count is outside bounds"));
    }

    let mut member_paths = BTreeSet::new();
    for member in members {
        let member = member
            .as_str()
            .ok_or_else(|| readme_error("workspace member is not a string"))?;
        if !valid_member_path(member) || !member_paths.insert(member.to_owned()) {
            return Err(readme_error(format!(
                "workspace member is unsafe or duplicated: {member}"
            )));
        }
    }

    let mut expected_files = BTreeSet::new();
    for member in &member_paths {
        let manifest_path = format!("{member}/Cargo.toml");
        let readme_path = format!("{member}/README.md");
        expected_files.insert(manifest_path.clone());
        expected_files.insert(readme_path.clone());

        let manifest_bytes = files
            .get(&manifest_path)
            .ok_or_else(|| readme_error(format!("missing package manifest: {manifest_path}")))?;
        if manifest_bytes.len() > MAXIMUM_MANIFEST_BYTES {
            return Err(readme_error(format!(
                "package manifest exceeds the byte limit: {manifest_path}"
            )));
        }
        let manifest_text = core::str::from_utf8(manifest_bytes).map_err(|_error| {
            readme_error(format!("package manifest is not UTF-8: {manifest_path}"))
        })?;
        let manifest = toml::from_str::<Value>(manifest_text).map_err(|error| {
            readme_error(format!(
                "package manifest is malformed: {manifest_path}: {error}"
            ))
        })?;
        let package = manifest
            .get("package")
            .and_then(Value::as_table)
            .ok_or_else(|| {
                readme_error(format!("manifest has no package table: {manifest_path}"))
            })?;
        let package_name = package
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.trim().is_empty())
            .ok_or_else(|| readme_error(format!("package has no name: {manifest_path}")))?;
        if package.get("readme").and_then(Value::as_str) != Some("README.md") {
            return Err(readme_error(format!(
                "package {package_name} must declare readme = \"README.md\""
            )));
        }

        let readme = files
            .get(&readme_path)
            .ok_or_else(|| readme_error(format!("missing package README: {readme_path}")))?;
        if readme.is_empty() || readme.len() > MAXIMUM_README_BYTES {
            return Err(readme_error(format!(
                "package README is empty or exceeds the byte limit: {readme_path}"
            )));
        }
        core::str::from_utf8(readme).map_err(|_error| {
            readme_error(format!("package README is not UTF-8: {readme_path}"))
        })?;
    }

    let actual_files = files
        .keys()
        .filter(|path| is_top_level_crate_record(path))
        .cloned()
        .collect::<BTreeSet<_>>();
    if actual_files != expected_files {
        let missing = expected_files.difference(&actual_files).collect::<Vec<_>>();
        let extra = actual_files.difference(&expected_files).collect::<Vec<_>>();
        return Err(readme_error(format!(
            "crate README/manifest set differs: missing={missing:?}, extra={extra:?}"
        )));
    }
    Ok(())
}

fn valid_member_path(member: &str) -> bool {
    let mut components = member.split('/');
    matches!(components.next(), Some("crates"))
        && components.next().is_some_and(|name| {
            !name.is_empty()
                && name != "."
                && name != ".."
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        })
        && components.next().is_none()
}

fn is_top_level_crate_record(path: &str) -> bool {
    let mut components = path.split('/');
    matches!(components.next(), Some("crates"))
        && components.next().is_some_and(|name| !name.is_empty())
        && components
            .next()
            .is_some_and(|name| matches!(name, "Cargo.toml" | "README.md"))
        && components.next().is_none()
}

pub(crate) fn check_repository(repository: &Repository) -> Result<(), Report<ReadmeError>> {
    let root_path =
        NormalizedRelativePath::new(Path::new("Cargo.toml")).change_context_lazy(|| {
            ReadmeError {
                detail: "root manifest path is invalid".to_owned(),
            }
        })?;
    let root_bytes = repository
        .read_tracked_bounded(&root_path, MAXIMUM_MANIFEST_BYTES)
        .change_context_lazy(|| ReadmeError {
            detail: "cannot read root Cargo.toml".to_owned(),
        })?;
    let root = core::str::from_utf8(&root_bytes)
        .map_err(|_error| readme_error("root Cargo.toml is not UTF-8"))?;

    let mut files = BTreeMap::new();
    for path in repository
        .tracked_paths()
        .change_context_lazy(|| ReadmeError {
            detail: "cannot enumerate tracked crate records".to_owned(),
        })?
    {
        let path_text = path.as_utf8().change_context_lazy(|| ReadmeError {
            detail: "tracked crate path is not UTF-8".to_owned(),
        })?;
        if !is_top_level_crate_record(path_text) {
            continue;
        }
        let maximum_bytes = if path_text.ends_with("Cargo.toml") {
            MAXIMUM_MANIFEST_BYTES
        } else {
            MAXIMUM_README_BYTES
        };
        let bytes = repository
            .read_tracked_bounded(&path, maximum_bytes)
            .change_context_lazy(|| ReadmeError {
                detail: format!("cannot read tracked crate record: {path_text}"),
            })?;
        files.insert(path_text.to_owned(), bytes);
    }
    validate_inventory(root, &files)
}

fn readme_error(detail: impl Into<String>) -> Report<ReadmeError> {
    Report::new(ReadmeError {
        detail: detail.into(),
    })
}
