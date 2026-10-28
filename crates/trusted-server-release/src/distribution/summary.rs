//! Test-only re-check and measurement summary of one candidate entry.
//!
//! The real CI builds use it to show that an entry built from an actual
//! completed build re-passes the producer joins and the release-index
//! `#/$defs/artifact` sub-schema from its retained bytes alone, and to print
//! what the entry carries. It grants no signing, publication or
//! qualification authority.

use std::collections::BTreeSet;

use error_stack::Report;
use serde_json::{Value, json};

use super::joins::{
    EvidenceJoin, check_build_evidence, check_comparison, check_spdx_root, inventory_modules,
    parse_informational,
};
use super::{CandidateArtifactEntry, CompanionRole, distribution_error, sha256_hex};
use crate::documents::{
    Role as DocumentRole, canonical_json, parse_json, validate_release_index_artifact,
};
use crate::error::ReleaseError;

/// One companion's identity recomputed from its exact bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
struct CompanionSummary {
    key: &'static str,
    name: String,
    size: u64,
    sha256: String,
}

/// What one rechecked candidate entry carries, for the printed measurement.
#[derive(Clone, Debug)]
pub(crate) struct EntrySummary {
    name: &'static str,
    artifact_sha256: String,
    artifact_size: u64,
    inventory_sha256: String,
    entry_sha256: String,
    entry_size: u64,
    rebuild_outcome: String,
    companions: Vec<CompanionSummary>,
    gaps: Vec<String>,
}

impl EntrySummary {
    /// The rechecked rebuild-check outcome.
    pub(crate) fn rebuild_outcome(&self) -> &str {
        &self.rebuild_outcome
    }

    /// SHA-256 of the exact JS inventory bytes the entry names.
    pub(crate) fn inventory_sha256(&self) -> &str {
        &self.inventory_sha256
    }

    /// SHA-256 of the canonical rendered index artifact entry.
    pub(crate) fn entry_sha256(&self) -> &str {
        &self.entry_sha256
    }

    /// The ordinary CI measurement of this entry.
    pub(crate) fn measurement(&self) -> Value {
        let companions: serde_json::Map<String, Value> = self
            .companions
            .iter()
            .map(|companion| {
                (
                    companion.key.to_owned(),
                    json!({
                        "name": companion.name,
                        "size": companion.size,
                        "sha256": companion.sha256,
                    }),
                )
            })
            .collect();
        json!({
            "kind": "ordinary-ci-candidate-entry",
            "asset": self.name,
            "artifact": {"sha256": self.artifact_sha256, "size": self.artifact_size},
            "entry": {"sha256": self.entry_sha256, "size": self.entry_size},
            "js_inventory_sha256": self.inventory_sha256,
            "rebuild_check_outcome": self.rebuild_outcome,
            "companions": companions,
            "gaps": self.gaps,
            "artifact_schema": "#/$defs/artifact",
            "release_qualification": false,
        })
    }
}

impl CandidateArtifactEntry {
    /// Rechecks this entry from its retained bytes and summarizes it.
    ///
    /// Rehashes every companion, validates the rendered index artifact
    /// against the retained `#/$defs/artifact` sub-schema, reruns every
    /// producer join, and requires each informational companion to state
    /// exactly the entry's non-empty gaps.
    ///
    /// # Errors
    ///
    /// Rejects companion bytes that differ from their records, a rendered
    /// artifact the sub-schema rejects, any failed join, a missing rebuild
    /// outcome, and empty or inconsistent gaps.
    pub(crate) fn recheck_for_measurement(&self) -> Result<EntrySummary, Report<ReleaseError>> {
        self.check_companions(&mut BTreeSet::new())?;
        if self.gaps.is_empty() {
            return Err(distribution_error("candidate entry states no gaps"));
        }
        let inventory_sha256 = sha256_hex(&self.inventory);
        let artifact_entry = self.index_entry(&inventory_sha256);
        validate_release_index_artifact(&artifact_entry)
            .map_err(|error| error.change_context(ReleaseError::Distribution))?;
        let companions = self.recomputed_companions(&artifact_entry)?;
        let modules = inventory_modules(&self.inventory, &self.source_commit)?;
        check_build_evidence(
            self.companion_bytes(CompanionRole::BuildEvidence)?,
            &EvidenceJoin {
                source_commit: &self.source_commit,
                target: self.asset.artifact().target.triple(),
                artifact_sha256: &self.artifact_sha256,
                inventory_sha256: &inventory_sha256,
                modules: &modules,
            },
        )?;
        check_spdx_root(
            self.companion_bytes(CompanionRole::Sbom)?,
            &self.artifact_sha256,
        )?;
        parse_informational(
            self.companion_bytes(CompanionRole::BuildRecipe)?,
            &self.source_commit,
        )?;
        let rebuild_check = self.companion_bytes(CompanionRole::RebuildCheck)?;
        check_comparison(rebuild_check, &self.source_commit)?;
        for role in [
            CompanionRole::BuildEvidence,
            CompanionRole::BuildRecipe,
            CompanionRole::RebuildCheck,
        ] {
            let document = parse_json(self.companion_bytes(role)?, DocumentRole::Companion)
                .map_err(|error| error.change_context(ReleaseError::Distribution))?;
            if document["release_qualification"] != false || document["gaps"] != json!(self.gaps) {
                return Err(distribution_error(format!(
                    "{} companion must be unqualified and state exactly the entry gaps",
                    role.key()
                )));
            }
        }
        let rebuild_outcome = parse_informational(rebuild_check, &self.source_commit)?["outcome"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| distribution_error("rebuild check has no outcome"))?;
        let entry_bytes = canonical_json(&artifact_entry, DocumentRole::Metadata)
            .map_err(|error| error.change_context(ReleaseError::Distribution))?;
        Ok(EntrySummary {
            name: self.name(),
            artifact_sha256: self.artifact_sha256.clone(),
            artifact_size: self.artifact_size,
            inventory_sha256,
            entry_sha256: sha256_hex(&entry_bytes),
            entry_size: entry_bytes.len() as u64,
            rebuild_outcome,
            companions,
            gaps: self.gaps.clone(),
        })
    }

    /// The exact bytes of the companion with `role`.
    fn companion_bytes(&self, role: CompanionRole) -> Result<&[u8], Report<ReleaseError>> {
        self.companions
            .iter()
            .find(|file| file.role == role)
            .map(|file| file.bytes.as_slice())
            .ok_or_else(|| distribution_error(format!("entry has no {} companion", role.key())))
    }

    /// Recomputes each companion identity from bytes and requires the
    /// rendered artifact to record exactly it.
    fn recomputed_companions(
        &self,
        artifact_entry: &Value,
    ) -> Result<Vec<CompanionSummary>, Report<ReleaseError>> {
        self.companions
            .iter()
            .map(|file| {
                let summary = CompanionSummary {
                    key: file.role.key(),
                    name: file.name.clone(),
                    size: file.bytes.len() as u64,
                    sha256: sha256_hex(&file.bytes),
                };
                let recorded = &artifact_entry["companions"][summary.key];
                if recorded
                    != &json!({"name": summary.name, "size": summary.size, "sha256": summary.sha256})
                {
                    return Err(distribution_error(format!(
                        "rendered {} record differs from its recomputed bytes",
                        summary.key
                    )));
                }
                Ok(summary)
            })
            .collect()
    }
}
