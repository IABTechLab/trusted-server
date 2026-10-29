//! Ordinary source and consumed-private-JS identities, without qualification.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path};

use error_stack::Report;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use super::{digest, require, root};
use crate::documents::{self, RawBytes, Role};
use crate::error::ReleaseError;

mod record;

pub(super) use record::{measure, verify_versions};

/// Source-owned ordinary build input identities, independent of runner paths.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct SourceIdentity {
    /// Canonical fixed recipe and source-file digest.
    pub(super) recipe_sha256: String,
    /// Raw Cargo lockfile digest, separate from the recipe identity.
    pub(super) cargo_lock_sha256: String,
    /// Raw npm lockfile digest, separate from the recipe identity.
    pub(super) npm_lock_sha256: String,
}

/// Hashes the fixed source-owned ordinary recipe and separate dependency locks.
///
/// # Errors
///
/// Rejects malformed source identity, aliased roots, or missing, empty, unsafe
/// or oversized fixed input files. This establishes no source approval.
pub(super) fn source_identity(
    checkout: &Path,
    source_commit: &str,
) -> Result<SourceIdentity, Report<ReleaseError>> {
    real_root(checkout)?;
    require(
        digest(source_commit, 40),
        "ordinary source commit is invalid",
    )?;
    let mut files = BTreeMap::new();
    for file in [
        "scripts/release/phase1-build.sh",
        ".cargo/config.toml",
        "Cargo.toml",
        "rust-toolchain.toml",
        ".tool-versions",
    ] {
        files.insert(file, source_file(checkout, file)?.sha256);
    }
    let recipe = json!({
        "version": 1, "source_commit": source_commit,
        "package": "trusted-server-adapter-fastly",
        "bin": "trusted-server-adapter-fastly", "target": "wasm32-wasip1",
        "profile": "release", "default_features": true, "features": [],
        "locked": true, "files": files,
        "normalization": super::normalization::policy(),
    });
    Ok(SourceIdentity {
        recipe_sha256: hex::encode(Sha256::digest(documents::canonical_json(
            &recipe,
            Role::Metadata,
        )?)),
        cargo_lock_sha256: source_file(checkout, "Cargo.lock")?.sha256,
        npm_lock_sha256: source_file(checkout, "crates/trusted-server-js/lib/package-lock.json")?
            .sha256,
    })
}

fn real_root(path: &Path) -> Result<(), Report<ReleaseError>> {
    require(
        path.to_str().is_some_and(root)
            && fs::canonicalize(path).is_ok_and(|resolved| resolved == path)
            && fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_dir()),
        "ordinary input root is missing or aliased",
    )
}

fn source_file(root: &Path, relative: &str) -> Result<RawBytes, Report<ReleaseError>> {
    let path = root.join(relative);
    require(
        fs::canonicalize(&path).is_ok_and(|resolved| resolved == path),
        "ordinary source input is missing or aliased",
    )?;
    let bytes = documents::read_bounded(&path, Role::Source)?;
    require(!bytes.bytes.is_empty(), "ordinary source input is empty")?;
    Ok(bytes)
}

/// Hashes the exact private JS set named by the successful Cargo build stream.
///
/// # Errors
///
/// Rejects malformed or unsuccessful Cargo inputs, ambiguous package/results,
/// aliased paths, or a missing, extra, empty, unsafe or oversized bundle set.
/// The digest does not prove complete composition or embedded membership.
pub(super) fn javascript_identity(
    checkout: &Path,
    target: &Path,
    metadata: &[u8],
    messages: &[u8],
) -> Result<String, Report<ReleaseError>> {
    real_root(checkout)?;
    real_root(target)?;
    require(
        !checkout.starts_with(target) && !target.starts_with(checkout),
        "ordinary source and target roots overlap",
    )?;
    let metadata =
        documents::parse_json(metadata, Role::CargoObservation).map_err(|_| js_error())?;
    let package = js_package(checkout, &metadata)?;
    let messages = crate::recipe::observed_cargo_messages(messages).map_err(|_| js_error())?;
    let matching: Vec<_> = messages
        .iter()
        .filter(|message| {
            message["reason"] == "build-script-executed" && message["package_id"] == package
        })
        .collect();
    require(
        matching.len() == 1,
        "ordinary JS build-script result is absent or ambiguous",
    )?;
    require(
        matching[0]["env"].as_array().is_some_and(Vec::is_empty),
        "ordinary JS result selected prebuilt or unexpected generated environment",
    )?;
    let text = matching[0]["out_dir"].as_str().ok_or_else(js_error)?;
    require(
        root(text),
        "ordinary JS output path is not absolute and normal",
    )?;
    let output = Path::new(text);
    let prefix = target.join("wasm32-wasip1/release/build");
    let relative = output.strip_prefix(&prefix).map_err(|_| js_error())?;
    let parts: Vec<_> = relative.components().collect();
    require(
        matches!(parts.as_slice(), [Component::Normal(package), Component::Normal(out)] if package.to_str().is_some_and(|name| name.starts_with("trusted-server-js-") && name.len() <= 128) && *out == "out"),
        "ordinary JS output is outside the exact target-side build-script layout",
    )?;
    real_root(output)?;
    let bundles = output.join("tsjs-dist");
    real_root(&bundles)?;
    let expected = expected_bundles(checkout)?;
    let mut found = BTreeSet::new();
    let mut records = BTreeMap::new();
    let mut total = 0_u64;
    for entry in fs::read_dir(&bundles).map_err(|_| js_error())? {
        let entry = entry.map_err(|_| js_error())?;
        let name = entry.file_name().into_string().map_err(|_| js_error())?;
        require(
            expected.contains(&name) && found.insert(name.clone()),
            "ordinary private JS has an unexpected or duplicate entry",
        )?;
        let raw = source_file(&bundles, &name)?;
        total = total
            .checked_add(raw.bytes.len() as u64)
            .ok_or_else(js_error)?;
        require(
            total <= 512 * 1024 * 1024,
            "ordinary private JS exceeds its total byte limit",
        )?;
        records.insert(
            name.clone(),
            json!({"name":name,"size":raw.bytes.len(),"sha256":raw.sha256}),
        );
    }
    require(
        found == expected,
        "ordinary private JS is missing a source-selected bundle",
    )?;
    let values: Vec<_> = records.into_values().collect();
    Ok(hex::encode(Sha256::digest(documents::canonical_json(
        &json!(values),
        Role::Metadata,
    )?)))
}

fn js_error() -> Report<ReleaseError> {
    Report::new(ReleaseError::Document)
        .attach("ordinary Cargo/JS input is missing, malformed or outside its fixed profile")
}

fn js_package<'a>(checkout: &Path, metadata: &'a Value) -> Result<&'a str, Report<ReleaseError>> {
    require(
        metadata["workspace_root"].as_str().map(Path::new) == Some(checkout),
        "ordinary Cargo metadata belongs to another checkout",
    )?;
    let packages = metadata["packages"]
        .as_array()
        .filter(|packages| packages.len() <= 5000)
        .ok_or_else(js_error)?;
    let mut ids = BTreeSet::new();
    for package in packages {
        require(
            package["id"].as_str().is_some_and(|id| {
                !id.is_empty()
                    && id.len() <= 4096
                    && !id.chars().any(char::is_control)
                    && ids.insert(id)
            }),
            "ordinary Cargo package ID is invalid or duplicated",
        )?;
    }
    let manifest = checkout.join("crates/trusted-server-js/Cargo.toml");
    source_file(checkout, "crates/trusted-server-js/Cargo.toml")?;
    let selected: Vec<_> = packages
        .iter()
        .filter(|package| {
            package["name"] == "trusted-server-js"
                && package["manifest_path"].as_str().map(Path::new) == Some(&manifest)
        })
        .collect();
    require(
        selected.len() == 1,
        "ordinary Cargo metadata has no unique source-owned JS package",
    )?;
    let id = selected[0]["id"].as_str().ok_or_else(js_error)?;
    let members = metadata["workspace_members"]
        .as_array()
        .ok_or_else(js_error)?;
    require(
        members
            .iter()
            .filter(|member| member.as_str() == Some(id))
            .count()
            == 1,
        "ordinary JS package is not a unique workspace member",
    )?;
    Ok(id)
}

fn expected_bundles(checkout: &Path) -> Result<BTreeSet<String>, Report<ReleaseError>> {
    let relative = "crates/trusted-server-js/lib/src";
    source_file(checkout, &format!("{relative}/core/index.ts"))?;
    let integrations = checkout.join(relative).join("integrations");
    real_root(&integrations)?;
    let mut expected = BTreeSet::from(["tsjs-core.js".to_owned()]);
    let mut folded = BTreeSet::from(["core".to_owned()]);
    let mut count = 0;
    for entry in fs::read_dir(integrations).map_err(|_| js_error())? {
        count += 1;
        require(
            count <= 256,
            "ordinary JS source integration count exceeds its bound",
        )?;
        let entry = entry.map_err(|_| js_error())?;
        let kind = entry.file_type().map_err(|_| js_error())?;
        require(
            !kind.is_symlink(),
            "ordinary JS source integration is aliased",
        )?;
        if !kind.is_dir()
            || !entry
                .path()
                .join("index.ts")
                .try_exists()
                .map_err(|_| js_error())?
        {
            continue;
        }
        let id = entry.file_name().into_string().map_err(|_| js_error())?;
        require(
            !id.is_empty()
                && id.len() <= 96
                && id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
                && folded.insert(id.to_ascii_lowercase()),
            "ordinary JS source module identity is unsafe or colliding",
        )?;
        source_file(checkout, &format!("{relative}/integrations/{id}/index.ts"))?;
        expected.insert(format!("tsjs-{id}.js"));
        require(
            expected.len() <= 128,
            "ordinary JS bundle count exceeds its bound",
        )?;
    }
    Ok(expected)
}

#[cfg(test)]
pub(super) mod tests;
