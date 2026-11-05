//! Records ordinary build facts without controlled-recipe authority.

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use error_stack::Report;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{SourceIdentity, javascript_identity, real_root, source_file, source_identity};
use crate::documents::{self, Role};
use crate::error::ReleaseError;
use crate::ordinary::{
    ASSET, Environment, GAPS, Observation, PROFILE, Tool, Tools, digest, normalization,
    observation, require, root,
};
use crate::process::{BoundedCommand, ProcessLimits};

#[derive(Debug, PartialEq, Eq)]
struct Pins {
    rust: String,
    node: String,
}

fn rejected() -> Report<ReleaseError> {
    Report::new(ReleaseError::Document)
        .attach("ordinary measurement inputs or tool pins are invalid")
}

fn version_pin(value: &str) -> bool {
    value.len() <= 32
        && value.split('.').count() == 3
        && value.split('.').all(|part| {
            !part.is_empty()
                && part.bytes().all(|byte| byte.is_ascii_digit())
                && (part == "0" || !part.starts_with('0'))
        })
}

fn pins(checkout: &Path) -> Result<Pins, Report<ReleaseError>> {
    let rust = source_file(checkout, "rust-toolchain.toml")?;
    let node = source_file(checkout, ".tool-versions")?;
    require(
        rust.bytes.len() <= 64 * 1024 && node.bytes.len() <= 64 * 1024,
        "ordinary source tool pin files exceed their bound",
    )?;
    let rust: toml::Value =
        toml::from_str(std::str::from_utf8(&rust.bytes).map_err(|_| rejected())?)
            .map_err(|_| rejected())?;
    let rust = rust
        .get("toolchain")
        .and_then(|value| value.get("channel"))
        .and_then(toml::Value::as_str)
        .filter(|value| version_pin(value))
        .ok_or_else(rejected)?
        .to_owned();
    let mut selected = Vec::new();
    for line in std::str::from_utf8(&node.bytes)
        .map_err(|_| rejected())?
        .lines()
    {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.first() == Some(&"nodejs") {
            require(
                fields.len() == 2 && version_pin(fields[1]),
                "ordinary Node selection must be one exact source pin",
            )?;
            selected.push(fields[1]);
        }
    }
    require(
        selected.len() == 1,
        "ordinary source must contain exactly one Node pin",
    )?;
    Ok(Pins {
        rust,
        node: selected[0].to_owned(),
    })
}

fn version_matches(kind: &str, pin: &str, version: &str) -> bool {
    if !matches!(kind, "rustc" | "node")
        || !version_pin(pin)
        || version.len() > 512
        || version.chars().any(char::is_control)
    {
        return false;
    }
    if kind == "node" {
        return version == format!("v{pin}");
    }
    let prefix = format!("rustc {pin} (");
    let Some(body) = version
        .strip_prefix(&prefix)
        .and_then(|value| value.strip_suffix(')'))
    else {
        return false;
    };
    let Some((hash, date)) = body.split_once(' ') else {
        return false;
    };
    (digest(hash, 9) || digest(hash, 40))
        && date.len() == 10
        && date.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 4 | 7) {
                byte == b'-'
            } else {
                byte.is_ascii_digit()
            }
        })
}

/// Checks recorded versions against the independent comparison checkout's pins.
///
/// # Errors
///
/// Rejects ambiguous source pins or any recorded compiler/Node version mismatch.
pub(in crate::ordinary) fn verify_versions(
    checkout: &Path,
    value: &serde_json::Value,
) -> Result<(), Report<ReleaseError>> {
    let pins = pins(checkout)?;
    let tools: Tools = serde_json::from_value(value.clone()).map_err(|_| rejected())?;
    require(
        version_matches("rustc", &pins.rust, &tools.rustc.version)
            && version_matches("node", &pins.node, &tools.node.version),
        "ordinary recorded tool versions differ from comparison source pins",
    )
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Request {
    role: String,
    source_commit: String,
    environment: Environment,
    rustc: PathBuf,
    node: PathBuf,
    cargo_messages: PathBuf,
    cargo_metadata: PathBuf,
    rustflags: PathBuf,
}

#[derive(PartialEq, Eq)]
struct RootIdentity {
    device: u64,
    inode: u64,
    owner: u32,
}

struct Captured {
    request: Request,
    roots: [RootIdentity; 2],
}

fn execution_uid() -> u32 {
    // SAFETY: geteuid has no memory preconditions and only reads process identity.
    unsafe { libc::geteuid() }
}

fn root_identities(request: &Request) -> Result<[RootIdentity; 2], Report<ReleaseError>> {
    let identity = |path: &str| -> Result<RootIdentity, Report<ReleaseError>> {
        let metadata = fs::symlink_metadata(path).map_err(|_| rejected())?;
        require(
            metadata.is_dir(),
            "ordinary captured root is no longer a directory",
        )?;
        Ok(RootIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
            owner: metadata.uid(),
        })
    };
    Ok([
        identity(&request.environment.checkout_root)?,
        identity(&request.environment.target_root)?,
    ])
}

fn validate(request: &Request) -> Result<(), Report<ReleaseError>> {
    let env = &request.environment;
    let checkout = Path::new(&env.checkout_root);
    let target = Path::new(&env.target_root);
    real_root(checkout)?;
    real_root(target)?;
    require(
        digest(&request.source_commit, 40)
            && env.started_at_seconds > 0
            && env.locale == "C"
            && env.timezone == "UTC"
            && env.uid > 0
            && env.uid == execution_uid()
            && matches!(
                (request.role.as_str(), env.jobs, env.umask),
                ("canonical", 4, 0o022) | ("reproduction", 2, 0o027)
            )
            && !checkout.starts_with(target)
            && !target.starts_with(checkout),
        "ordinary capture source, role or environment is invalid",
    )?;
    let checkout_metadata = fs::metadata(checkout).map_err(|_| rejected())?;
    let target_metadata = fs::metadata(target).map_err(|_| rejected())?;
    require(
        checkout_metadata.uid() == env.uid
            && target_metadata.uid() == env.uid
            && (checkout_metadata.dev(), checkout_metadata.ino())
                != (target_metadata.dev(), target_metadata.ino()),
        "ordinary source/target ownership or filesystem identity is invalid",
    )?;
    for (path, kind) in [(&request.rustc, "rustc"), (&request.node, "node")] {
        require(
            path.to_str().is_some_and(root)
                && path.file_name().is_some_and(|name| name == kind)
                && fs::canonicalize(path).is_ok_and(|resolved| resolved == *path),
            "ordinary tool path must name the actual canonical executable, not a shim",
        )?;
    }
    for path in [
        &request.cargo_messages,
        &request.cargo_metadata,
        &request.rustflags,
    ] {
        require(
            path.to_str().is_some_and(root)
                && fs::canonicalize(path).is_ok_and(|resolved| resolved == *path),
            "ordinary Cargo capture path is missing or aliased",
        )?;
    }
    Ok(())
}

fn capture(bytes: &[u8]) -> Result<Captured, Report<ReleaseError>> {
    let request = serde_json::from_value(
        documents::parse_json(bytes, Role::Metadata).map_err(|_| rejected())?,
    )
    .map_err(|_| rejected())?;
    validate(&request)?;
    let roots = root_identities(&request)?;
    require(
        roots
            .iter()
            .all(|root| root.owner == request.environment.uid),
        "ordinary captured root owner changed",
    )?;
    Ok(Captured { request, roots })
}

fn tool(program: &Path, working: &Path) -> Result<Tool, Report<ReleaseError>> {
    let before = documents::hash_bounded_with_check(program, Role::Executable.limit(), &|| Ok(()))?;
    require(
        before.size > 0 && before.mode & 0o111 != 0 && before.mode & 0o6000 == 0,
        "ordinary selected tool is empty, privileged or not executable",
    )?;
    let raw = (|| {
        BoundedCommand::new(
            program,
            working,
            ProcessLimits {
                wall: Duration::from_secs(10),
                stdout_max: 1024,
                stderr_max: 1024,
            },
        )?
        .arguments([OsString::from("--version")])
        .environment("LC_ALL", "C")?
        .environment("TZ", "UTC")?
        .run()?
        .require_success()
    })()
    .map_err(|_| rejected())?;
    let raw = raw.strip_suffix(b"\n").ok_or_else(rejected)?;
    let version = std::str::from_utf8(raw).map_err(|_| rejected())?.to_owned();
    let after = documents::hash_bounded_with_check(program, Role::Executable.limit(), &|| Ok(()))?;
    require(
        before.sha256 == after.sha256 && before.size == after.size && before.mode == after.mode,
        "ordinary selected tool changed while querying its version",
    )?;
    Ok(Tool {
        version,
        sha256: before.sha256,
    })
}

fn finish(
    captured: &Captured,
    initial: &SourceIdentity,
    pins: &Pins,
    tools: &Tools,
) -> Result<Observation, Report<ReleaseError>> {
    let request = &captured.request;
    validate(request)?;
    require(
        root_identities(request)? == captured.roots,
        "ordinary captured root identity changed",
    )?;
    require(
        version_matches("rustc", &pins.rust, &tools.rustc.version)
            && version_matches("node", &pins.node, &tools.node.version),
        "ordinary measured tool versions do not match the source pins",
    )?;
    let checkout = Path::new(&request.environment.checkout_root);
    let target = Path::new(&request.environment.target_root);
    let metadata = documents::read_bounded(&request.cargo_metadata, Role::CargoObservation)?;
    let messages = documents::read_bounded(&request.cargo_messages, Role::CargoObservation)?;
    let js = javascript_identity(checkout, target, &metadata.bytes, &messages.bytes)?;
    let binary = checkout.join("release-assets").join(ASSET);
    require(
        fs::canonicalize(&binary).is_ok_and(|resolved| resolved == binary),
        "ordinary final binary path is missing or aliased",
    )?;
    let binary = documents::read_bounded(&binary, Role::Executable)?;
    let flags = normalization::plan(checkout, target, &request.role)?;
    let retained = documents::read_bounded(&request.rustflags, Role::Metadata)?;
    require(
        retained.bytes == flags.bytes && retained.sha256 == flags.sha256,
        "ordinary captured Cargo arguments differ from the source-owned four-map plan",
    )?;
    normalization::scan(&binary.bytes, &flags)?;
    let source = source_identity(checkout, &request.source_commit)?;
    require(
        source == *initial,
        "ordinary source inputs changed during measurement",
    )?;
    require(
        root_identities(request)? == captured.roots,
        "ordinary captured root identity changed before sealing",
    )?;
    let value = json!({
        "version":1,"profile":PROFILE,"release_qualification":false,
        "source_commit":request.source_commit,"asset":ASSET,"target":"wasm32-wasip1","variant":"default","role":request.role,
        "recipe_sha256":source.recipe_sha256,"cargo_lock_sha256":source.cargo_lock_sha256,"npm_lock_sha256":source.npm_lock_sha256,
        "js_inventory_sha256":js,"rustflags_sha256":retained.sha256,"tools":tools,"environment":request.environment,
        "artifact":{"size":binary.bytes.len(),"sha256":binary.sha256},"gaps":GAPS,
    });
    observation(&documents::canonical_json(&value, Role::Metadata)?)
}

/// Records one ordinary observation from strict, source-owned invocation data.
///
/// # Errors
///
/// Rejects invalid captures, source/ownership changes, unsafe retained inputs,
/// failed bounded version queries, tool-pin mismatches or changed executables.
/// Authentication and controlled-recipe qualification remain unestablished.
pub(in crate::ordinary) fn measure(bytes: &[u8]) -> Result<Observation, Report<ReleaseError>> {
    let captured = capture(bytes)?;
    let request = &captured.request;
    let checkout = Path::new(&request.environment.checkout_root);
    let source = source_identity(checkout, &request.source_commit)?;
    let pins = pins(checkout)?;
    let tools = Tools {
        rustc: tool(&request.rustc, checkout)?,
        node: tool(&request.node, checkout)?,
        acquisition: "ordinary-hosted-toolchain".to_owned(),
    };
    finish(&captured, &source, &pins, &tools)
}

#[cfg(test)]
mod tests;
