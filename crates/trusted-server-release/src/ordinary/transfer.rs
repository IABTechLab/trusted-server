//! Ordinary archive-byte joins, without authenticated transport authority.

use error_stack::Report;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use std::collections::BTreeMap;
use std::path::Path;

use super::archive::{self, Profile};
use super::run::{Archive, Bindings, Build};
use super::{ASSET, Equality, Observation, compare, diagnostic, observation, require};
use crate::error::ReleaseError;

/// Four exact-ID downloads in canonical build/observation, reproduction order.
pub(super) struct Archives<'a>(pub(super) [&'a [u8]; 4]);

/// A complete ordinary equality receipt and retained canonical release inputs.
pub(super) struct Checked {
    bindings: Bindings,
    equality: Equality,
    binary: Vec<u8>,
    spdx: Vec<u8>,
    reproduction_spdx_sha256: String,
}

impl Checked {
    /// The informational ordinary receipt; it grants no qualification.
    pub(super) fn receipt(&self) -> Value {
        let context = self.bindings.context();
        let [canonical, reproduction] = self.bindings.builds();
        let observed = self.equality.canonical.value();
        json!({
            "version": 1,
            "profile": observed["profile"],
            "release_qualification": false,
            "source_commit": context.source_commit,
            "asset": ASSET,
            "target": "wasm32-wasip1",
            "variant": "default",
            "recipe_sha256": observed["recipe_sha256"],
            "cargo_lock_sha256": observed["cargo_lock_sha256"],
            "npm_lock_sha256": observed["npm_lock_sha256"],
            "js_inventory_sha256": observed["js_inventory_sha256"],
            "tools": observed["tools"],
            "outcome": "matched",
            "canonical_sha256": self.equality.sha256(),
            "reproduction_sha256": self.equality.sha256(),
            "canonical": self.build_record(canonical, &self.equality.canonical, &hex::encode(Sha256::digest(&self.spdx))),
            "reproduction": self.build_record(reproduction, &self.equality.reproduction, &self.reproduction_spdx_sha256),
            "gaps": observed["gaps"],
        })
    }

    fn build_record(&self, build: &Build, observation: &Observation, spdx: &str) -> Value {
        let context = self.bindings.context();
        let upload = |archive: &Archive| {
            json!({
                "id": archive.id, "name": archive.name,
                "sha256": archive.sha256, "size": archive.size,
            })
        };
        json!({
            "run_id": context.run_id,
            "run_attempt": context.run_attempt,
            "job_id": build.job_id,
            "artifact": upload(&build.artifact),
            "observation_artifact": upload(&build.observation),
            "environment": observation.value()["environment"],
            "size": self.equality.size(),
            "sha256": observation.facts.artifact.sha256,
            "spdx_sha256": spdx,
        })
    }

    /// Exact canonical inputs retained from the checked build archive.
    pub(super) fn inputs(&self) -> (&[u8], &[u8]) {
        (&self.binary, &self.spdx)
    }
}

fn unpack(
    bytes: &[u8],
    expected: &Archive,
    profile: Profile,
) -> Result<BTreeMap<String, Vec<u8>>, Report<ReleaseError>> {
    let limit = match profile {
        Profile::Build => 768 * 1024 * 1024,
        Profile::Observation => 2 * 1024 * 1024,
    };
    require(
        bytes.len() <= limit
            && bytes.len() as u64 == expected.size
            && hex::encode(Sha256::digest(bytes)) == expected.sha256,
        "ordinary archive digest or size disagrees with its upload/service identity",
    )
    .map_err(|report| diagnostic::tag(report, diagnostic::Code::TransferRejected))?;
    archive::contents(bytes, profile)
        .map_err(|report| diagnostic::tag(report, diagnostic::Code::TransferRejected))
}

fn observed(
    bytes: &[u8],
    bindings: &Bindings,
    build: &Build,
) -> Result<Observation, Report<ReleaseError>> {
    let observed = observation(bytes)
        .map_err(|report| diagnostic::tag(report, diagnostic::Code::InputsRejected))?;
    let started = observed.facts.environment.started_at_seconds;
    // Observations record whole seconds; the server upload interval above keeps
    // its full precision. Compare the observation's stated resolution here.
    require(
        observed.facts.source_commit == bindings.context().source_commit
            && started >= build.started.unix_timestamp() as u64
            && started <= build.completed.unix_timestamp() as u64,
        "ordinary observation source or start disagrees with its producing job",
    )
    .map_err(|report| diagnostic::tag(report, diagnostic::Code::IdentityMismatch))?;
    Ok(observed)
}

/// Checks whole archives before parsing and joins retained bytes to observations.
pub(super) fn check(
    bindings: Bindings,
    archives: &Archives<'_>,
) -> Result<Checked, Report<ReleaseError>> {
    let [canonical, reproduction] = bindings.builds();
    let mut left = unpack(archives.0[0], &canonical.artifact, Profile::Build)?;
    let left_observation = unpack(archives.0[1], &canonical.observation, Profile::Observation)?;
    let right = unpack(archives.0[2], &reproduction.artifact, Profile::Build)?;
    let right_observation = unpack(
        archives.0[3],
        &reproduction.observation,
        Profile::Observation,
    )?;
    let canonical = observed(
        &left_observation["fastly-observation.json"],
        &bindings,
        canonical,
    )?;
    let reproduction = observed(
        &right_observation["fastly-observation.json"],
        &bindings,
        reproduction,
    )?;
    let equality = compare(canonical, reproduction, &left[ASSET], &right[ASSET])?;
    for (observed, bytes) in [
        (&equality.canonical, &left[ASSET]),
        (&equality.reproduction, &right[ASSET]),
    ] {
        let facts = &observed.facts;
        let flags = super::normalization::plan(
            Path::new(&facts.environment.checkout_root),
            Path::new(&facts.environment.target_root),
            &facts.role,
        )?;
        super::normalization::scan(bytes, &flags).map_err(|report| {
            report.attach(
                diagnostic::Failure::new(diagnostic::Code::InputsRejected)
                    .binaries(&left[ASSET], &right[ASSET]),
            )
        })?;
    }
    let spdx_name = format!("{ASSET}.spdx.json");
    let reproduction_spdx_sha256 = hex::encode(Sha256::digest(&right[&spdx_name]));
    // Exact ZIP membership is already checked. SPDX semantics remain enforced
    // by the existing producer/signing/publication path, not by this receipt.
    let binary = left
        .remove(ASSET)
        .ok_or_else(|| Report::new(ReleaseError::Document))?;
    let spdx = left
        .remove(&spdx_name)
        .ok_or_else(|| Report::new(ReleaseError::Document))?;
    Ok(Checked {
        bindings,
        equality,
        binary,
        spdx,
        reproduction_spdx_sha256,
    })
}

#[cfg(test)]
pub(super) mod tests;
