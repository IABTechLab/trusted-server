//! Checked diagnostic runtime export; it cannot reconstruct acceptance from JSON.

use error_stack::Report;
use serde_json::{Value, json};

use super::{FastlyRuntimeAccepted, RuntimeRejection, runtime_error};
use crate::documents::{Role as DocumentRole, canonical_json};
use crate::error::ReleaseError;
use crate::recipe::compare::FASTLY_ASSET;
use crate::recipe::{AssetId, RecipeCompleted, Role};
use crate::source::CommitId;

impl FastlyRuntimeAccepted {
    /// Exports accepted runtime facts bound to this exact source, role and artifact.
    ///
    /// The immutable owning result survives build/runtime working-root cleanup.
    /// Its diagnostic representation grants no qualification or signing authority.
    ///
    /// # Errors
    ///
    /// Rejects changed completed outputs, another source/role/artifact or a record
    /// exceeding the metadata bound. JSON cannot construct runtime acceptance.
    pub(crate) fn diagnostic_for(
        &self,
        completed: &RecipeCompleted,
    ) -> Result<Value, Report<ReleaseError>> {
        completed.rehash_for_handoff()?;
        self.check_export_binding(
            completed.commit(),
            completed.role(),
            completed.asset(),
            completed.artifact().sha256(),
            completed.artifact().size(),
        )?;
        let record = self.diagnostic_record()?;
        completed.rehash_for_handoff()?;
        Ok(record)
    }

    fn check_export_binding(
        &self,
        commit: &CommitId,
        role: Role,
        asset: AssetId,
        digest: &str,
        size: u64,
    ) -> Result<(), Report<ReleaseError>> {
        if self.source_commit() != commit
            || self.role != role
            || asset.artifact().name != FASTLY_ASSET
            || self.observed().artifact_sha256() != digest
            || self.observed().artifact_size() != size
        {
            return Err(runtime_error(
                RuntimeRejection::Artifact,
                "runtime acceptance belongs to another source, role or final artifact",
            ));
        }
        Ok(())
    }

    fn diagnostic_record(&self) -> Result<Value, Report<ReleaseError>> {
        let observed = self.observed();
        let publisher = observed.publisher();
        let output = observed.viceroy_output();
        let startup = u64::try_from(observed.startup().as_millis())
            .map_err(|error| runtime_error(RuntimeRejection::Startup, error))?;
        let record = json!({
            "domain": "trusted-server.release.fastly-runtime-diagnostic",
            "schema_version": 1, "release_qualification": false,
            "source_commit": self.source_commit().as_str(), "role": self.role.spec_label(),
            "asset": FASTLY_ASSET,
            "artifact": {"sha256": observed.artifact_sha256(), "size": observed.artifact_size()},
            "core_format": "core-wasm-fastly-command",
            "viceroy": {"version": observed.viceroy_version(), "sha256": observed.viceroy_sha256()},
            "fixture_sha256": observed.fixture_sha256(), "config_sha256": observed.config_sha256(),
            "health_status": observed.health_status(),
            "publisher": {"status": publisher.status, "body_size": publisher.body_size,
                "body_sha256": publisher.body_sha256, "script_src": publisher.script_src},
            "origin_requests": observed.origin_requests(), "startup_milliseconds": startup,
            "viceroy_outcome": observed.viceroy_outcome().to_string(),
            "viceroy_output": {"stdout_sha256": output.stdout_sha256,
                "stderr_sha256": output.stderr_sha256, "stderr_tail": output.stderr_tail},
        });
        canonical_json(&record, DocumentRole::Metadata)
            .map_err(|error| error.change_context(ReleaseError::Runtime))?;
        Ok(record)
    }
}

#[cfg(test)]
mod tests;
