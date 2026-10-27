//! Ordinary Fastly path remapping, without controlled recipe qualification.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::MetadataExt as _;
use std::path::Path;

use error_stack::Report;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use wasmparser::{Parser, Payload};

use super::{require, root};
use crate::documents::{self, Role};
use crate::error::ReleaseError;
use crate::recipe;

/// Closed four-map argument plan; it constructs no qualified recipe.
pub(super) struct Flags {
    /// The exact pair passed to Cargo.
    pub(super) arguments: Vec<String>,
    /// Canonical UTF-8 argument JSON including its trailing LF.
    pub(super) bytes: Vec<u8>,
    /// Digest of those retained bytes.
    pub(super) sha256: String,
    roots: [String; 4],
}

fn rejected() -> Report<ReleaseError> {
    Report::new(ReleaseError::Document).attach("ordinary path normalization inputs rejected")
}

fn text(path: &Path) -> Result<String, Report<ReleaseError>> {
    path.to_str()
        .filter(|value| {
            root(value)
                && !value.contains(['=', ':', '"', '\\'])
                && !value.chars().any(char::is_control)
        })
        .map(str::to_owned)
        .ok_or_else(rejected)
}

/// Derives identical source-owned maps from either role's physical roots.
///
/// # Errors
///
/// Rejects unapproved roles, layout names, aliases in text, overlaps and unsafe encoding.
pub(super) fn plan(
    checkout: &Path,
    target: &Path,
    role: &str,
) -> Result<Flags, Report<ReleaseError>> {
    require(
        matches!(role, "canonical" | "reproduction"),
        "ordinary normalization role is invalid",
    )?;
    text(checkout)?;
    text(target)?;
    require(
        checkout
            .file_name()
            .is_some_and(|name| name == format!("ordinary-{role}-source").as_str())
            && target
                .file_name()
                .is_some_and(|name| name == format!("ordinary-fastly-{role}-target").as_str()),
        "ordinary normalization roots do not match their source-owned role layout",
    )?;
    let workspace = checkout.parent().ok_or_else(rejected)?;
    let temporary = target.parent().ok_or_else(rejected)?;
    let roots = [
        text(&workspace.join("ordinary-canonical-source"))?,
        text(&workspace.join("ordinary-reproduction-source"))?,
        text(&temporary.join("ordinary-fastly-canonical-target"))?,
        text(&temporary.join("ordinary-fastly-reproduction-target"))?,
    ];
    for (index, left) in roots.iter().enumerate() {
        for right in roots.iter().skip(index + 1) {
            require(
                !Path::new(left).starts_with(right) && !Path::new(right).starts_with(left),
                "ordinary normalization roots overlap",
            )?;
        }
    }
    let destinations = [
        recipe::REMAPPED_CHECKOUT,
        recipe::REMAPPED_CHECKOUT,
        recipe::REMAPPED_TARGET,
        recipe::REMAPPED_TARGET,
    ];
    let flags: Vec<_> = roots
        .iter()
        .zip(destinations)
        .map(|(from, to)| format!("--remap-path-prefix={from}={to}"))
        .collect();
    let arguments = recipe::rustflags_arguments("wasm32-wasip1", &flags)?;
    let bytes = documents::canonical_json(&json!(arguments), Role::Metadata)?;
    let sha256 = hex::encode(Sha256::digest(&bytes));
    Ok(Flags {
        arguments,
        bytes,
        sha256,
        roots,
    })
}

/// Checks real runner layout and configuration before any target build starts.
///
/// # Errors
///
/// Rejects aliases, different owners, nonempty targets and competing Cargo flags.
pub(super) fn prepare(
    checkout: &Path,
    target: &Path,
    role: &str,
    inherited: &[(OsString, OsString)],
) -> Result<Flags, Report<ReleaseError>> {
    let flags = plan(checkout, target, role)?;
    // SAFETY: geteuid only reads the process identity and has no memory preconditions.
    let owner = unsafe { libc::geteuid() };
    for path in &flags.roots {
        match fs::symlink_metadata(path) {
            Ok(metadata) => require(
                metadata.is_dir()
                    && metadata.uid() == owner
                    && fs::canonicalize(path).is_ok_and(|resolved| resolved == Path::new(path)),
                "ordinary normalization root is aliased or not owned by this runner",
            )?,
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound
                    && Path::new(path) != checkout
                    && Path::new(path) != target => {}
            _ => return Err(rejected()),
        }
    }
    require(
        fs::read_dir(target)
            .map_err(|_| rejected())?
            .next()
            .is_none(),
        "ordinary normalization target must be fresh and empty",
    )?;
    validate_configuration(checkout, inherited)?;
    Ok(flags)
}

fn flag_override(name: &str) -> bool {
    matches!(
        name,
        "RUSTFLAGS" | "CARGO_ENCODED_RUSTFLAGS" | "CARGO_BUILD_RUSTFLAGS"
    ) || (name.starts_with("CARGO_TARGET_") && name.ends_with("_RUSTFLAGS"))
}

fn absent_layer(path: &Path) -> Result<(), Report<ReleaseError>> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        _ => Err(rejected()
            .attach("ordinary normalization refuses an unhandled Cargo configuration layer")),
    }
}

/// Refuses configuration that could replace or compete with the target remap.
///
/// # Errors
///
/// Rejects ambient overrides, extensionless config, build flags, competing maps,
/// or unsupported target flag representations. Reviewed arrays are never changed.
pub(super) fn validate_configuration(
    checkout: &Path,
    inherited: &[(OsString, OsString)],
) -> Result<(), Report<ReleaseError>> {
    require(
        !inherited
            .iter()
            .any(|(name, _)| name.to_str().is_some_and(flag_override)),
        "ordinary normalization refuses ambient rustflags",
    )?;
    for ancestor in checkout.ancestors().skip(1) {
        for file in ["config", "config.toml"] {
            absent_layer(&ancestor.join(".cargo").join(file))?;
        }
    }
    let cargo_home = inherited
        .iter()
        .find(|(name, _)| name == "CARGO_HOME")
        .map(|(_, value)| std::path::PathBuf::from(value))
        .or_else(|| {
            inherited
                .iter()
                .find(|(name, _)| name == "HOME")
                .map(|(_, value)| std::path::PathBuf::from(value).join(".cargo"))
        });
    if let Some(home) = cargo_home {
        require(home.is_absolute(), "ordinary Cargo home must be absolute")?;
        for file in ["config", "config.toml"] {
            let candidate = home.join(file);
            if candidate != checkout.join(".cargo/config.toml") {
                absent_layer(&candidate)?;
            }
        }
    }
    recipe::check_no_extensionless_config(checkout, false).map_err(|_| rejected())?;
    let bytes = documents::read_bounded(&checkout.join(".cargo/config.toml"), Role::Metadata)?;
    recipe::check_cargo_config(&bytes.bytes).map_err(|_| rejected())?;
    let table: toml::Value =
        toml::from_str(std::str::from_utf8(&bytes.bytes).map_err(|_| rejected())?)
            .map_err(|_| rejected())?;
    require(
        table.get("include").is_none(),
        "ordinary normalization refuses included Cargo configuration layers",
    )?;
    if let Some(environment) = table.get("env") {
        require(
            !environment
                .as_table()
                .ok_or_else(rejected)?
                .keys()
                .any(|key| flag_override(key)),
            "ordinary normalization refuses configured flag overrides",
        )?;
    }
    if let Some(targets) = table.get("target") {
        for target in targets.as_table().ok_or_else(rejected)?.values() {
            let target = target.as_table().ok_or_else(rejected)?;
            if let Some(flags) = target.get("rustflags") {
                for flag in flags.as_array().ok_or_else(rejected)? {
                    let flag = flag.as_str().ok_or_else(rejected)?;
                    require(
                        !flag.starts_with("--remap-path-prefix"),
                        "ordinary normalization refuses preexisting remap options",
                    )?;
                }
            }
        }
    }
    Ok(())
}

/// Checks complete raw bytes and retained debug/name sections after compilation.
///
/// # Errors
///
/// Rejects raw physical roots, malformed Wasm, missing/duplicate debug sections,
/// or absent normalized source identity. No section is changed or excluded.
pub(super) fn scan(bytes: &[u8], flags: &Flags) -> Result<(), Report<ReleaseError>> {
    require(
        bytes.len() as u64 <= Role::Executable.limit() && Parser::is_core_wasm(bytes),
        "ordinary normalized artifact is not bounded core Wasm",
    )?;
    let forbidden: Vec<_> = flags.roots.iter().map(String::as_str).collect();
    recipe::reject_execution_prefixes(bytes, &forbidden)?;
    let mut sections = BTreeSet::new();
    let mut mapped_source = false;
    for payload in Parser::new(0).parse_all(bytes) {
        if let Payload::CustomSection(section) = payload.map_err(|_| rejected())?
            && matches!(
                section.name(),
                ".debug_info" | ".debug_str" | ".debug_line" | "name"
            )
        {
            require(
                !section.data().is_empty() && sections.insert(section.name()),
                "ordinary normalized debug/name sections are empty or duplicated",
            )?;
            if section.name() == ".debug_str" {
                let prefix = format!("{}/", recipe::REMAPPED_CHECKOUT);
                mapped_source = section.data().split(|byte| *byte == 0).any(|text| {
                    text == recipe::REMAPPED_CHECKOUT.as_bytes()
                        || text.starts_with(prefix.as_bytes())
                });
            }
        }
    }
    require(
        sections.len() == 4 && mapped_source,
        "ordinary normalized debug/source identity is unproven",
    )
}

/// Stable semantic policy, deliberately excluding physical runner paths.
pub(super) fn policy() -> Value {
    json!({"kind":"ordinary-rust-path-remap-v1","target":"wasm32-wasip1", "roles":["canonical","reproduction"],"source_prefix":recipe::REMAPPED_CHECKOUT,"target_prefix":recipe::REMAPPED_TARGET})
}

#[cfg(test)]
mod tests;

#[cfg(test)]
pub(super) const TEST_WASM: &[u8] = b"\0asm\x01\0\0\0\0\x0d\x0b.debug_info\0\0\x1b\x0a.debug_str/trusted-server\0\0\x0d\x0b.debug_line\0\0\x06\x04name\0";
