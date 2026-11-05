//! Parses identities without granting acquisition or execution authority.

use std::collections::{BTreeMap, BTreeSet};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use error_stack::Report;
use serde_json::Value;

use super::material_error;
use crate::documents::{Role, lower_hex, parse_json};
use crate::error::ReleaseError;

// This runtime acquisition identity must match the approved lockfile registry.
pub(super) const NPM_REGISTRY: &str = "https://registry.npmjs.org/"; // allow-domain: registry.npmjs.org
pub(super) const CARGO_REGISTRY: &str = "registry+https://github.com/rust-lang/crates.io-index";

/// One original npm distribution, retaining its installation-path join.
pub(super) struct NpmMaterial {
    pub(super) name: String,
    pub(super) version: String,
    pub(super) resolved: String,
    pub(super) sha512: [u8; 64],
}

/// One registry crate's exact original distribution identity.
pub(super) struct RegistryMaterial {
    pub(super) name: String,
    pub(super) version: String,
    pub(super) sha256: String,
}

/// One Git package's exact locked identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct GitPackage {
    pub(super) name: String,
    pub(super) version: String,
    /// Lock source without its `#<commit>` suffix, for example
    /// `git+https://example.com/org/repo?tag=v1`.
    pub(super) selector: String,
    pub(super) revision: String,
}

/// Cargo acquisition expectations, not verified dependency trees.
pub(super) struct CargoMaterials {
    pub(super) registry: BTreeMap<(String, String), RegistryMaterial>,
    pub(super) git: BTreeSet<(String, String)>,
    pub(super) git_packages: Vec<GitPackage>,
}

fn text<'a>(value: &'a Value, field: &str) -> Result<&'a str, Report<ReleaseError>> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| material_error(format!("locked npm package has no {field}")))
}

fn integrity(value: &str) -> Result<[u8; 64], Report<ReleaseError>> {
    let encoded = value
        .strip_prefix("sha512-")
        .ok_or_else(|| material_error("npm archive requires one SHA-512 integrity"))?;
    let bytes = STANDARD.decode(encoded).map_err(material_error)?;
    if STANDARD.encode(&bytes) != encoded {
        return Err(material_error("npm integrity is not canonical base64"));
    }
    bytes
        .try_into()
        .map_err(|_| material_error("npm SHA-512 identity must have 64 bytes"))
}

fn component(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn npm_name(name: &str) -> bool {
    let parts: Vec<&str> = name.split('/').collect();
    match parts.as_slice() {
        [name] => component(name),
        [scope, name] => scope.strip_prefix('@').is_some_and(component) && component(name),
        _ => false,
    }
}

pub(super) fn parse_npm(
    bytes: &[u8],
) -> Result<BTreeMap<String, NpmMaterial>, Report<ReleaseError>> {
    let document = parse_json(bytes, Role::Metadata)?;
    if document.get("lockfileVersion").and_then(Value::as_u64) != Some(3) {
        return Err(material_error("npm lockfile must use version 3"));
    }
    let packages = document
        .get("packages")
        .and_then(Value::as_object)
        .ok_or_else(|| material_error("npm lockfile has no package table"))?;
    let mut result = BTreeMap::new();
    for (path, package) in packages {
        if path.is_empty() {
            continue;
        }
        if !path.starts_with("node_modules/")
            || path.contains('\\')
            || path
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..")
            || path.chars().any(char::is_control)
        {
            return Err(material_error(
                "npm installation path is not a safe locked location",
            ));
        }
        let locations = path
            .strip_prefix("node_modules/")
            .ok_or_else(|| material_error("missing locked installation prefix"))?;
        if locations
            .split("/node_modules/")
            .any(|name| !npm_name(name))
        {
            return Err(material_error("invalid nested npm installation location"));
        }
        let inferred = path
            .rsplit("node_modules/")
            .next()
            .ok_or_else(|| material_error("missing npm package name"))?;
        let name = package
            .get("name")
            .map(|value| {
                value
                    .as_str()
                    .ok_or_else(|| material_error("locked npm name must be a string"))
            })
            .transpose()?
            .unwrap_or(inferred);
        if name != inferred {
            return Err(material_error(
                "npm aliases are outside the supported material contract",
            ));
        }
        let linked = package
            .get("link")
            .map(|value| {
                value
                    .as_bool()
                    .ok_or_else(|| material_error("locked npm link selector must be a boolean"))
            })
            .transpose()?
            .unwrap_or(false);
        let version = text(package, "version")?;
        if !npm_name(name)
            || !version
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+'))
        {
            return Err(material_error(
                "npm identity contains unsupported path or version characters",
            ));
        }
        let resolved = text(package, "resolved")?;
        let basename = name
            .rsplit('/')
            .next()
            .ok_or_else(|| material_error("invalid npm package name"))?;
        let expected = format!("{NPM_REGISTRY}{name}/-/{basename}-{version}.tgz");
        if resolved != expected || linked {
            return Err(material_error(
                "npm distribution is not its canonical registry archive",
            ));
        }
        result.insert(
            path.clone(),
            NpmMaterial {
                name: name.to_owned(),
                version: version.to_owned(),
                resolved: resolved.to_owned(),
                sha512: integrity(text(package, "integrity")?)?,
            },
        );
    }
    Ok(result)
}

pub(super) fn parse_cargo(bytes: &[u8]) -> Result<CargoMaterials, Report<ReleaseError>> {
    let text = std::str::from_utf8(bytes).map_err(material_error)?;
    let lock: toml::Value = toml::from_str(text).map_err(material_error)?;
    if lock.get("version").and_then(toml::Value::as_integer) != Some(4) {
        return Err(material_error("Cargo lockfile must use version 4"));
    }
    let packages = lock
        .get("package")
        .and_then(toml::Value::as_array)
        .ok_or_else(|| material_error("Cargo lockfile has no package array"))?;
    let mut result = CargoMaterials {
        registry: BTreeMap::new(),
        git: BTreeSet::new(),
        git_packages: Vec::new(),
    };
    let mut identities = BTreeSet::new();
    for package in packages {
        let field = |key| {
            package
                .get(key)
                .and_then(toml::Value::as_str)
                .ok_or_else(|| material_error(format!("Cargo package has no {key}")))
        };
        let name = field("name")?;
        let version = field("version")?;
        let source = package
            .get("source")
            .map(|value| {
                value
                    .as_str()
                    .ok_or_else(|| material_error("Cargo source must be a string"))
            })
            .transpose()?;
        if !identities.insert((name, version, source)) {
            return Err(material_error("duplicate Cargo package identity"));
        }
        match source {
            None => {}
            Some(CARGO_REGISTRY) => {
                let checksum = field("checksum")?;
                if !lower_hex(checksum, 64) {
                    return Err(material_error("registry archive has no exact SHA-256"));
                }
                result.registry.insert(
                    (name.to_owned(), version.to_owned()),
                    RegistryMaterial {
                        name: name.to_owned(),
                        version: version.to_owned(),
                        sha256: checksum.to_owned(),
                    },
                );
            }
            Some(source) if source.starts_with("git+https://") => {
                let (repository, commit) = source
                    .rsplit_once('#')
                    .ok_or_else(|| material_error("Git material lacks a locked revision"))?;
                if !lower_hex(commit, 40) || repository.chars().any(char::is_control) {
                    return Err(material_error(
                        "Git material requires a full lowercase revision",
                    ));
                }
                result
                    .git
                    .insert((repository.to_owned(), commit.to_owned()));
                result.git_packages.push(GitPackage {
                    name: name.to_owned(),
                    version: version.to_owned(),
                    selector: repository.to_owned(),
                    revision: commit.to_owned(),
                });
            }
            Some(_) => return Err(material_error("unsupported Cargo material source")),
        }
    }
    Ok(result)
}
