//! Audit fingerprint of the helper-source closure at the approved commit.

use std::collections::BTreeSet;

use error_stack::Report;

use super::tree::check_tracked_path;
use super::{Records, digest_records, source_error};
use crate::error::ReleaseError;

const RELEASE_CRATE_PREFIX: &str = "crates/trusted-server-release/";

/// Root files always included in the helper-source closure.
const CLOSURE_FILES: [&str; 10] = [
    ".cargo/config.toml",
    ".tool-versions",
    "Cargo.lock",
    "Cargo.toml",
    "rust-toolchain.toml",
    "scripts/release/bootstrap.sh",
    "scripts/release/build-js.sh",
    "scripts/release/build.sh",
    "scripts/release/collect-js.mjs",
    "scripts/release/observe-npm.mjs",
];

/// Path-dependency manifests that must be listed as workspace members.
const REQUIRED_MEMBER_MANIFESTS: [&str; 1] = ["crates/trusted-server-openrtb/Cargo.toml"];

/// Digests the release crate, root build inputs and every member manifest.
///
/// Returns the digest and the sorted covered paths. This is an audit
/// fingerprint, not authority.
pub(super) fn helper_closure(
    records: &Records,
    manifest: &[u8],
) -> Result<(String, Vec<String>), Report<ReleaseError>> {
    let mut paths = member_manifests(records, &workspace_members(manifest)?)?;
    for path in CLOSURE_FILES {
        if !records.contains_key(path) {
            return Err(source_error(format!(
                "helper closure file {path} is not tracked"
            )));
        }
        paths.insert(path.to_owned());
    }
    paths.extend(
        records
            .keys()
            .filter(|path| {
                path.starts_with(RELEASE_CRATE_PREFIX)
                    || path.starts_with("crates/trusted-server-js/lib/")
            })
            .cloned(),
    );
    let digest = digest_records(paths.iter().filter_map(|path| records.get_key_value(path)));
    Ok((digest, paths.into_iter().collect()))
}

/// Explicit `[workspace] members` from the tracked root manifest.
fn workspace_members(manifest: &[u8]) -> Result<Vec<String>, Report<ReleaseError>> {
    let text =
        std::str::from_utf8(manifest).map_err(|_| source_error("root Cargo.toml must be UTF-8"))?;
    let table: toml::Table = toml::from_str(text)
        .map_err(|error| source_error(format!("root Cargo.toml is invalid: {error}")))?;
    let members = table
        .get("workspace")
        .and_then(|workspace| workspace.get("members"))
        .and_then(toml::Value::as_array)
        .ok_or_else(|| source_error("root Cargo.toml must list [workspace] members"))?;
    members
        .iter()
        .map(|member| {
            let member = member
                .as_str()
                .ok_or_else(|| source_error("workspace members must be strings"))?;
            if member.contains(['*', '?', '[', ']']) {
                return Err(source_error(format!(
                    "workspace member {member} uses a glob; the closure requires explicit members"
                )));
            }
            Ok(member.to_owned())
        })
        .collect()
}

/// Tracked manifests for every member, including the required path dependencies.
fn member_manifests(
    records: &Records,
    members: &[String],
) -> Result<BTreeSet<String>, Report<ReleaseError>> {
    let mut manifests = BTreeSet::new();
    for member in members {
        let manifest = format!("{member}/Cargo.toml");
        check_tracked_path(&manifest)?;
        if !records.contains_key(&manifest) {
            return Err(source_error(format!(
                "workspace member manifest {manifest} is not tracked"
            )));
        }
        manifests.insert(manifest);
    }
    if let Some(required) = REQUIRED_MEMBER_MANIFESTS
        .iter()
        .find(|required| !manifests.contains(**required))
    {
        return Err(source_error(format!(
            "{required} must be a listed workspace member manifest"
        )));
    }
    Ok(manifests)
}
