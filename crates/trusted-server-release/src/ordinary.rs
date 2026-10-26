//! Informational ordinary-release observations; these confer no qualification.
//!
//! Parsing and pairing establish neither source eligibility nor tool provenance.
//! Before comparison is enabled, the runtime recorder and transfer join must
//! enforce source-owned tool pins and actual filesystem identities; these
//! observation roots are only checked lexically here.

use error_stack::Report;
use serde::Deserialize;
use serde_json::Value;
use std::path::{Component, Path};

use crate::documents::{self, Role};
use crate::error::ReleaseError;

const PROFILE: &str = "ordinary-release-fastly-byte-equality-v1";
const ASSET: &str = "trusted-server-fastly.wasm";
const GAPS: [&str; 4] = [
    "alternate-user-unqualified",
    "boundary-unqualified",
    "materials-unqualified",
    "tools-unqualified",
];

/// Validated ordinary observations, without controlled-recipe authority.
pub(crate) struct Observation {
    value: Value,
    facts: Facts,
}

impl Observation {
    /// The original strict JSON facts retained for informational serialization.
    pub(crate) fn value(&self) -> &Value {
        &self.value
    }
}

/// Checks the narrower ordinary input/environment pairing, never qualification.
pub(crate) fn pair(
    canonical: &Observation,
    reproduction: &Observation,
) -> Result<(), Report<ReleaseError>> {
    let canonical = &canonical.facts;
    let reproduction = &reproduction.facts;
    require(
        canonical.role == "canonical" && reproduction.role == "reproduction",
        "ordinary comparison requires canonical then reproduction",
    )?;
    require(
        canonical.source_commit == reproduction.source_commit
            && canonical.recipe_sha256 == reproduction.recipe_sha256
            && canonical.cargo_lock_sha256 == reproduction.cargo_lock_sha256
            && canonical.npm_lock_sha256 == reproduction.npm_lock_sha256
            && canonical.js_inventory_sha256 == reproduction.js_inventory_sha256
            && canonical.tools == reproduction.tools,
        "ordinary build inputs differ",
    )?;
    let canonical = &canonical.environment;
    let reproduction = &reproduction.environment;
    require(
        canonical.started_at_seconds != reproduction.started_at_seconds
            && canonical.umask == 0o022
            && reproduction.umask == 0o027
            && canonical.jobs == 4
            && reproduction.jobs == 2
            && canonical.uid == reproduction.uid,
        "ordinary environment pairing rejected",
    )?;
    for left in [&canonical.checkout_root, &canonical.target_root] {
        for right in [&reproduction.checkout_root, &reproduction.target_root] {
            require(
                !Path::new(left).starts_with(right) && !Path::new(right).starts_with(left),
                "ordinary build roots overlap",
            )?;
        }
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Facts {
    version: u8,
    profile: String,
    release_qualification: bool,
    source_commit: String,
    asset: String,
    target: String,
    variant: String,
    role: String,
    recipe_sha256: String,
    cargo_lock_sha256: String,
    npm_lock_sha256: String,
    js_inventory_sha256: String,
    tools: Tools,
    environment: Environment,
    artifact: Artifact,
    gaps: Vec<String>,
}

#[derive(Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Tool {
    version: String,
    sha256: String,
}

#[derive(Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Tools {
    rustc: Tool,
    node: Tool,
    acquisition: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Environment {
    checkout_root: String,
    target_root: String,
    started_at_seconds: u64,
    umask: u32,
    jobs: u32,
    locale: String,
    timezone: String,
    uid: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Artifact {
    size: u64,
    sha256: String,
}

fn require(condition: bool, message: &'static str) -> Result<(), Report<ReleaseError>> {
    if !condition {
        return Err(Report::new(ReleaseError::Document).attach(message));
    }
    Ok(())
}

fn digest(text: &str, length: usize) -> bool {
    text.len() == length
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn root(text: &str) -> bool {
    let path = Path::new(text);
    text.len() <= 4096
        && path.is_absolute()
        && path.components().count() > 1
        && path
            .components()
            .all(|part| matches!(part, Component::RootDir | Component::Normal(_)))
        && !text.chars().any(char::is_control)
        && !text.contains("//")
        && !text.ends_with('/')
        && !text.split('/').any(|part| part == "." || part == "..")
}

/// Parses one ordinary Fastly observation without granting execution authority.
pub(crate) fn observation(bytes: &[u8]) -> Result<Observation, Report<ReleaseError>> {
    let value = documents::parse_json(bytes, Role::Metadata).map_err(|_| {
        Report::new(ReleaseError::Document).attach("ordinary observation JSON rejected")
    })?;
    let facts: Facts = serde_json::from_value(value.clone()).map_err(|_| {
        Report::new(ReleaseError::Document).attach("ordinary observation fields or types rejected")
    })?;
    require(
        facts.version == 1
            && facts.profile == PROFILE
            && !facts.release_qualification
            && facts.asset == ASSET
            && facts.target == "wasm32-wasip1"
            && facts.variant == "default"
            && matches!(facts.role.as_str(), "canonical" | "reproduction"),
        "ordinary identity or qualification claim rejected",
    )?;
    require(
        digest(&facts.source_commit, 40)
            && [
                &facts.recipe_sha256,
                &facts.cargo_lock_sha256,
                &facts.npm_lock_sha256,
                &facts.js_inventory_sha256,
                &facts.artifact.sha256,
                &facts.tools.rustc.sha256,
                &facts.tools.node.sha256,
            ]
            .into_iter()
            .all(|text| digest(text, 64)),
        "ordinary digest rejected",
    )?;
    require(
        facts.tools.acquisition == "ordinary-hosted-toolchain"
            && [&facts.tools.rustc, &facts.tools.node]
                .into_iter()
                .all(|tool| {
                    !tool.version.is_empty()
                        && tool.version.len() <= 512
                        && !tool.version.chars().any(char::is_control)
                }),
        "ordinary tool observations rejected",
    )?;
    let environment = &facts.environment;
    require(
        root(&environment.checkout_root)
            && root(&environment.target_root)
            && !Path::new(&environment.checkout_root).starts_with(&environment.target_root)
            && !Path::new(&environment.target_root).starts_with(&environment.checkout_root)
            && environment.started_at_seconds > 0
            && environment.umask <= 0o777
            && (1..=256).contains(&environment.jobs)
            && environment.locale == "C"
            && environment.timezone == "UTC"
            && environment.uid > 0,
        "ordinary environment observations rejected",
    )?;
    require(
        facts.artifact.size > 0 && facts.artifact.size <= Role::Executable.limit(),
        "ordinary artifact size rejected",
    )?;
    require(facts.gaps == GAPS, "ordinary assurance gaps rejected")?;
    Ok(Observation { value, facts })
}

#[cfg(test)]
mod tests;
