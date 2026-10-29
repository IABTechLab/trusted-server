//! Informational ordinary-release observations; these confer no qualification.
//!
//! Parsing and pairing establish neither source eligibility nor tool provenance.
//! Before comparison is enabled, the runtime recorder and transfer join must
//! enforce source-owned tool pins and actual filesystem identities; these
//! observation roots are only checked lexically here.

use error_stack::Report;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use std::path::{Component, Path};

use crate::documents::{self, Role};
use crate::error::ReleaseError;

mod archive;
mod commands;
mod diagnostic;
mod inputs;
mod normalization;
mod output;
mod run;
mod transfer;

pub(crate) use commands::{compare_transfers, measure_build, prepare_rustflags, transfer_plan};

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

/// Final-byte equality of an ordinary pair, without controlled-recipe authority.
pub(crate) struct Equality {
    canonical: Observation,
    reproduction: Observation,
    sha256: String,
    size: u64,
}

impl Equality {
    /// SHA-256 of the complete equal final bytes.
    pub(crate) fn sha256(&self) -> &str {
        &self.sha256
    }

    /// Size of the complete equal final bytes.
    pub(crate) fn size(&self) -> u64 {
        self.size
    }
}

/// Compares complete ordinary final bytes after checking their input pairing.
pub(crate) fn compare(
    canonical: Observation,
    reproduction: Observation,
    left: &[u8],
    right: &[u8],
) -> Result<Equality, Report<ReleaseError>> {
    pair(&canonical, &reproduction)
        .map_err(|report| diagnostic::tag(report, diagnostic::Code::IdentityMismatch))?;
    let mut digests = Vec::new();
    for (observed, bytes) in [(&canonical, left), (&reproduction, right)] {
        let size = u64::try_from(bytes.len())
            .map_err(|_| Report::new(ReleaseError::Document).attach("ordinary size overflow"))?;
        require(
            size > 0 && size <= Role::Executable.limit() && size == observed.facts.artifact.size,
            "ordinary executable changed after observation: size or byte bound rejected",
        )
        .map_err(|report| {
            report.attach(
                diagnostic::Failure::new(diagnostic::Code::SizeMismatch).binaries(left, right),
            )
        })?;
        let digest = hex::encode(Sha256::digest(bytes));
        require(
            digest == observed.facts.artifact.sha256,
            "ordinary executable changed after observation",
        )
        .map_err(|report| {
            report.attach(
                diagnostic::Failure::new(diagnostic::Code::DigestMismatch).binaries(left, right),
            )
        })?;
        digests.push(digest);
    }
    if left != right {
        let offset = left
            .iter()
            .zip(right)
            .position(|(left, right)| left != right)
            .unwrap_or(left.len().min(right.len()));
        let window = |bytes: &[u8]| {
            let start = offset.saturating_sub(8).min(bytes.len());
            let end = start.saturating_add(16).min(bytes.len());
            hex::encode(&bytes[start..end])
        };
        return Err(Report::new(ReleaseError::Document)
            .attach(format!(
                "ordinary final-byte mismatch: canonical_size={}, reproduction_size={}, \
             canonical_sha256={}, reproduction_sha256={}, first_offset={}, \
             canonical_window={}, reproduction_window={}",
                left.len(),
                right.len(),
                digests[0],
                digests[1],
                offset,
                window(left),
                window(right),
            ))
            .attach(diagnostic::Failure::mismatch(left, right, offset)));
    }
    Ok(Equality {
        sha256: canonical.facts.artifact.sha256.clone(),
        size: canonical.facts.artifact.size,
        canonical,
        reproduction,
    })
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
    let canonical_environment = &canonical.environment;
    let reproduction_environment = &reproduction.environment;
    require(
        canonical_environment.started_at_seconds != reproduction_environment.started_at_seconds
            && canonical_environment.umask == 0o022
            && reproduction_environment.umask == 0o027
            && canonical_environment.jobs == 4
            && reproduction_environment.jobs == 2
            && canonical_environment.uid == reproduction_environment.uid,
        "ordinary environment pairing rejected",
    )?;
    for left in [
        &canonical_environment.checkout_root,
        &canonical_environment.target_root,
    ] {
        for right in [
            &reproduction_environment.checkout_root,
            &reproduction_environment.target_root,
        ] {
            require(
                !Path::new(left).starts_with(right) && !Path::new(right).starts_with(left),
                "ordinary build roots overlap",
            )?;
        }
    }
    let mut arguments = Vec::new();
    for facts in [canonical, reproduction] {
        let flags = normalization::plan(
            Path::new(&facts.environment.checkout_root),
            Path::new(&facts.environment.target_root),
            &facts.role,
        )?;
        require(
            flags.sha256 == facts.rustflags_sha256,
            "ordinary retained rustflags disagree with derived physical roots",
        )?;
        arguments.push(flags.arguments);
    }
    require(
        arguments[0] == arguments[1],
        "ordinary builds used different raw remap arguments",
    )?;

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
    rustflags_sha256: String,
    tools: Tools,
    environment: Environment,
    artifact: Artifact,
    gaps: Vec<String>,
}

#[derive(Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Tool {
    version: String,
    sha256: String,
}

#[derive(Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Tools {
    rustc: Tool,
    node: Tool,
    acquisition: String,
}

#[derive(Deserialize, Serialize)]
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
                &facts.rustflags_sha256,
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
pub(super) mod tests;
