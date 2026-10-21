//! Renders the strict collector's `build-record.json` from owning build results.
//!
//! [`super::CandidateBuildExport`] keeps its own diagnostic record. This
//! renderer states the same owned observations in the collector's schema, so
//! the collector's unchanged checks can run on producer output. It grants no
//! authority: the collector still binds identity fields to trusted upload
//! outputs, and the record cannot reconstruct any local proof type.
//! `composition_sha256` names the producer's own sealed composition receipt,
//! which the collector parses strictly and binds to this record.
//!
//! Remaining gaps before a real producer transfer can pass the collector:
//!
//! - Job identity: the producing job's workflow job ID and attempt are not
//!   owned by the completed build, so the caller must supply [`ProducerJob`].
//! - Native startup: the collector schema allows no runtime block for native
//!   builds, so native startup acceptance cannot be represented.

use std::collections::BTreeSet;

use error_stack::Report;
use serde_json::{Value, json};

use super::{DOCUMENT_LIMIT, collection_error};
use crate::boundary::DenialReceipt;
use crate::error::ReleaseError;
use crate::policy::FIXED_ARTIFACTS;
use crate::recipe::{
    AmbientClaim, Dimension, FastlyRuntimeAccepted, ObservedAmbient, RecipeCompleted, Role,
};

/// Member name the strict collector reads from one build transfer.
pub(crate) const STRICT_RECORD: &str = "build-record.json";
/// Longest workflow job ID in bytes, as the collector's upload identity check bounds it.
const JOB_LIMIT: usize = 128;

type Result<T> = std::result::Result<T, Report<ReleaseError>>;

/// The producing job's workflow job ID and run attempt, as its runner reports them.
///
/// This is self-reported context, not authority: the collector requires it to
/// equal the trusted dependency upload identity of the same transfer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProducerJob {
    job: String,
    attempt: u64,
}

impl ProducerJob {
    /// Accepts exactly the job IDs and attempts the collector's upload check accepts.
    ///
    /// # Errors
    ///
    /// Rejects an empty job ID, one over [`JOB_LIMIT`] bytes or holding a
    /// control character, and attempt `0`.
    pub(crate) fn new(job: &str, attempt: u64) -> Result<Self> {
        if job.is_empty()
            || job.len() > JOB_LIMIT
            || job.chars().any(char::is_control)
            || attempt == 0
        {
            return Err(collection_error(
                "producer job identity is empty, unbounded or has no attempt",
            ));
        }
        Ok(Self {
            job: job.to_owned(),
            attempt,
        })
    }
}

/// Approved input digests the collector compares across a pair.
#[derive(Clone, Debug)]
struct InputDigests {
    cargo_lock: String,
    materials: String,
    rust_tools: String,
    js_settings: String,
    js_tools: String,
}

/// Measured socket controls, boundary identity and exact invocation of one build.
#[derive(Clone, Debug)]
struct SocketControls {
    /// Boundary instance the measured denial receipt names.
    fingerprint: String,
    /// Boundary instance that executed the build. Instance identity only: it
    /// includes a fresh per-instance nonce and says nothing about the invocation.
    execution_fingerprint: String,
    /// Exact invocation digest: program, arguments including jobs,
    /// environment, checkout-root working directory and umask.
    invocation_digest: String,
    control_exit: i32,
    control_accepts: u32,
    probe_exit: i32,
    probe_errno: i32,
    probe_accepts: u32,
}

impl SocketControls {
    fn from_owned(completed: &RecipeCompleted, receipt: &DenialReceipt) -> Result<Self> {
        let count = |accepts: usize| u32::try_from(accepts).map_err(collection_error);
        let (control_exit, control_accepts) = receipt.control();
        let (probe_exit, probe_accepts) = receipt.probe();
        Ok(Self {
            fingerprint: receipt.fingerprint().to_owned(),
            execution_fingerprint: hex::encode(completed.boundary_fingerprint()),
            invocation_digest: completed.plan_digest().to_owned(),
            control_exit,
            control_accepts: count(control_accepts)?,
            probe_exit,
            probe_errno: receipt.denied_errno(),
            probe_accepts: count(probe_accepts)?,
        })
    }
}

/// Final-byte Fastly runtime facts taken from this build's own acceptance.
#[derive(Clone, Debug)]
struct RuntimeFacts {
    role: Role,
    source_commit: String,
    final_wasm_sha256: String,
    final_wasm_size: u64,
    publisher_status: u16,
    viceroy_version: String,
}

/// One build's owned observations, in the strict collector's record shape.
#[derive(Clone, Debug)]
pub(crate) struct StrictBuildRecord {
    source_commit: String,
    recipe_sha256: String,
    asset: String,
    role: Role,
    job: ProducerJob,
    inputs: InputDigests,
    inventory_sha256: String,
    artifact_sha256: String,
    artifact_size: u64,
    composition_sha256: String,
    ambient: ObservedAmbient,
    boundary: SocketControls,
    runtime: Option<RuntimeFacts>,
}

impl StrictBuildRecord {
    /// Collects the strict record facts from one completed build.
    ///
    /// A Fastly build requires its own role-bound final-byte acceptance, which
    /// is checked against this build before use. A native build carries none:
    /// the collector schema has no native startup block, so native startup
    /// acceptance is not represented here.
    ///
    /// # Errors
    ///
    /// Rejects missing measured socket receipts, recipe definition, composition
    /// or JS evidence, changed owned outputs, a missing Fastly acceptance, an
    /// acceptance for another build, and any acceptance on a native build.
    pub(crate) fn from_owned(
        completed: &RecipeCompleted,
        accepted: Option<&FastlyRuntimeAccepted>,
        job: ProducerJob,
    ) -> Result<Self> {
        let definition = completed.recipe_definition()?;
        let denial = completed.denial_receipt()?;
        let composition = completed.composition_export()?;
        let javascript = completed.javascript_contributions()?;
        let artifact = completed.artifact();
        let fastly = completed.asset().artifact().name == FIXED_ARTIFACTS[0].name;
        let runtime = match (fastly, accepted) {
            (true, Some(accepted)) => {
                accepted.diagnostic_for(completed)?;
                let observed = accepted.observed();
                Some(RuntimeFacts {
                    role: accepted.role(),
                    source_commit: accepted.source_commit().as_str().to_owned(),
                    final_wasm_sha256: observed.artifact_sha256().to_owned(),
                    final_wasm_size: observed.artifact_size(),
                    publisher_status: observed.publisher().status,
                    viceroy_version: observed.viceroy_version().to_owned(),
                })
            }
            (true, None) => {
                return Err(collection_error(
                    "a Fastly strict record requires this build's own runtime acceptance",
                ));
            }
            (false, Some(_)) => {
                return Err(collection_error(
                    "a native strict record cannot carry a Fastly runtime acceptance",
                ));
            }
            (false, None) => None,
        };
        let [_, js_settings, js_tools] = completed.js_digests();
        let record = Self {
            source_commit: completed.commit().as_str().to_owned(),
            recipe_sha256: definition.sha256().to_owned(),
            asset: artifact.name().to_owned(),
            role: completed.role(),
            job,
            inputs: InputDigests {
                cargo_lock: completed.cargo_lock_sha256().to_owned(),
                materials: completed.materials_digest().to_owned(),
                rust_tools: completed.rust_tools_digest().to_owned(),
                js_settings: js_settings.to_owned(),
                js_tools: js_tools.to_owned(),
            },
            inventory_sha256: javascript.inventory_sha256().to_owned(),
            artifact_sha256: artifact.sha256().to_owned(),
            artifact_size: artifact.size(),
            composition_sha256: composition.sha256().to_owned(),
            ambient: completed.ambient().observed().clone(),
            boundary: SocketControls::from_owned(completed, denial)?,
            runtime,
        };
        completed.rehash_for_handoff()?;
        Ok(record)
    }

    /// Renders the exact strict collector record bytes.
    ///
    /// # Errors
    ///
    /// Rejects a non-UTF-8 checkout root or a record exceeding the metadata bound.
    pub(crate) fn to_bytes(&self) -> Result<Vec<u8>> {
        let bytes = serde_json::to_vec(&self.to_value()?).map_err(collection_error)?;
        if bytes.len() as u64 > DOCUMENT_LIMIT {
            return Err(collection_error("strict build record exceeds its bound"));
        }
        Ok(bytes)
    }

    fn to_value(&self) -> Result<Value> {
        let ambient = &self.ambient;
        let checkout_root = ambient
            .checkout_root
            .to_str()
            .ok_or_else(|| collection_error("observed checkout root is not UTF-8"))?;
        // The compiled placement policy, not a pair observation: a single build
        // cannot see its counterpart, and the collector's pair comparison
        // verifies every varied dimension from both observed records.
        let claim = AmbientClaim::standard();
        let boundary = &self.boundary;
        // A denial receipt exists only after the confined probe was denied, so
        // its probe run is the confined one.
        let runtime = self.runtime.as_ref().map(|runtime| {
            // Acceptance exists only after the publisher page held this run's
            // origin marker exactly once and exactly one injected script.
            json!({
                "role": runtime.role.spec_label(),
                "source_commit": runtime.source_commit,
                "final_wasm_sha256": runtime.final_wasm_sha256,
                "final_wasm_size": runtime.final_wasm_size,
                "publisher_status": runtime.publisher_status,
                "injected_scripts": 1,
                "origin_marker_matched": true,
                "viceroy_version": runtime.viceroy_version,
            })
        });
        Ok(json!({
            "schema_version": 1,
            "source_commit": self.source_commit,
            "recipe_sha256": self.recipe_sha256,
            "asset": self.asset,
            "role": self.role.spec_label(),
            "job": self.job.job,
            "attempt": self.job.attempt,
            "inputs": {
                "cargo_lock": self.inputs.cargo_lock,
                "materials": self.inputs.materials,
                "rust_tools": self.inputs.rust_tools,
                "js_settings": self.inputs.js_settings,
                "js_tools": self.inputs.js_tools,
            },
            "inventory_sha256": self.inventory_sha256,
            "artifact_sha256": self.artifact_sha256,
            "artifact_size": self.artifact_size,
            "composition_sha256": self.composition_sha256,
            "ambient": {
                "umask": ambient.umask,
                "jobs": ambient.jobs,
                "checkout_root": checkout_root,
                "started_at_seconds": ambient.started_at_seconds,
                "execution_uid": ambient.execution_uid,
                "host_locale": ambient.host_locale,
                "host_timezone": ambient.host_timezone,
                "child_locale": ambient.child_locale,
                "child_timezone": ambient.child_timezone,
            },
            "claim": {
                "varied": dimension_names(&claim.varied),
                "normalized": dimension_names(&claim.normalized),
                "unsupported": dimension_names(&claim.unsupported),
            },
            "boundary": {
                "fingerprint": boundary.fingerprint,
                "execution_fingerprint": boundary.execution_fingerprint,
                "invocation_digest": boundary.invocation_digest,
                "confined": true,
                "control_exit": boundary.control_exit,
                "control_accepts": boundary.control_accepts,
                "probe_exit": boundary.probe_exit,
                "probe_errno": boundary.probe_errno,
                "probe_accepts": boundary.probe_accepts,
            },
            "runtime": runtime,
        }))
    }
}

fn dimension_names(dimensions: &BTreeSet<Dimension>) -> Vec<&'static str> {
    dimensions
        .iter()
        .map(|dimension| match dimension {
            Dimension::Umask => "umask",
            Dimension::Jobs => "jobs",
            Dimension::CheckoutRoot => "checkout_root",
            Dimension::StartTime => "start_time",
            Dimension::Locale => "locale",
            Dimension::Timezone => "timezone",
            Dimension::ExecutionUser => "execution_user",
        })
        .collect()
}

#[cfg(test)]
mod tests;
