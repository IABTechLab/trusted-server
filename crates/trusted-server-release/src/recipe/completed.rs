//! The proof a recipe build ran to completion, with its copied artifact.
//!
//! [`RecipeCompleted`] has private fields and no public constructor:
//! [`RecipeCompleted::record_success`] is called only from the recipe's
//! successful-run path, after Cargo's output was parsed and the artifact was
//! copied and inspected. It binds everything an attestation needs and can
//! re-measure the retained output before handoff.

use std::fs;
use std::io::Write as _;
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use error_stack::Report;
use sha2::{Digest as _, Sha256};

use super::artifact::{CopiedArtifact, EXECUTABLE_LIMIT};
use super::{
    AssetId, CheckedAmbient, ExactInvocationDigest, RecipeDefinitionIdentity, Role, recipe_error,
};
use crate::boundary::{DenialReceipt, DescendantCleanup};
use crate::composition::{CargoCapture, CompositionView, RetainedCompositionExport};
use crate::documents::hash_bounded_with_check;
use crate::error::ReleaseError;
use crate::javascript::contributions::RetainedContributions;
use crate::source::CommitId;

/// File name of the retained Cargo message stream.
const STDOUT_FILE: &str = "cargo-stdout.jsonl";
/// Original JS observer bytes, retained independently of the preparation lease.
const JS_INVENTORY_FILE: &str = "tsjs-inventory.json";

/// Facts about one successful run, supplied by the recipe's run path.
pub(super) struct SuccessFacts {
    pub(super) asset: AssetId,
    pub(super) role: Role,
    pub(super) commit: CommitId,
    pub(super) cargo_lock_sha256: String,
    pub(super) materials_digest: String,
    pub(super) js_inventory_sha256: String,
    pub(super) js_settings_sha256: String,
    pub(super) js_tools_sha256: String,
    pub(super) js_contributions: Option<RetainedContributions>,
    pub(super) rust_tools_digest: String,
    pub(super) boundary_fingerprint: [u8; 32],
    pub(super) denial_receipt: Option<DenialReceipt>,
    pub(super) recipe_definition: Option<RecipeDefinitionIdentity>,
    pub(super) plan_digest: ExactInvocationDigest,
    pub(super) ambient: CheckedAmbient,
    pub(super) started_at_seconds: u64,
    pub(super) elapsed: Duration,
    pub(super) descendant_cleanup: DescendantCleanup,
    pub(super) stdout: Vec<u8>,
    pub(super) stderr_sha256: String,
    pub(super) artifact: CopiedArtifact,
    pub(super) output_dir: PathBuf,
    pub(super) normalized_manifest_digests: std::collections::BTreeMap<String, String>,
    pub(super) derived_manifest_digests: std::collections::BTreeMap<String, String>,
    pub(super) cargo_observations: Option<CargoCapture>,
}

/// A built, copied and bound artifact.
pub(crate) struct RecipeCompleted {
    facts: SuccessFacts,
    stdout_sha256: String,
    retained: tempfile::TempDir,
}

impl std::fmt::Debug for RecipeCompleted {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RecipeCompleted")
            .field("asset", &self.facts.asset)
            .field("role", &self.facts.role)
            .field("artifact", &self.facts.artifact)
            .field("plan_digest", &self.facts.plan_digest)
            .finish_non_exhaustive()
    }
}

impl RecipeCompleted {
    /// Returns the approved source-owned definition retained by this build.
    ///
    /// This selects recipe policy without proving tool qualification or which
    /// bootstrap built the producer that executed it.
    ///
    /// # Errors
    ///
    /// Rejects changed outputs, an unbound fixture or another source/asset selection.
    pub(crate) fn recipe_definition(
        &self,
    ) -> Result<&RecipeDefinitionIdentity, Report<ReleaseError>> {
        self.rehash_for_handoff()?;
        self.facts
            .recipe_definition
            .as_ref()
            .filter(|definition| definition.matches_selection(&self.facts.commit, self.facts.asset))
            .ok_or_else(|| {
                recipe_error("completed build has no matching approved recipe definition")
            })
    }

    /// Returns measured socket observations bound to this completed execution.
    ///
    /// This diagnostic receipt does not reconstruct a network-denial capability.
    ///
    /// # Errors
    ///
    /// Rejects changed retained outputs, an unmeasured fixture or another boundary.
    pub(crate) fn denial_receipt(&self) -> Result<&DenialReceipt, Report<ReleaseError>> {
        self.rehash_for_handoff()?;
        self.facts
            .denial_receipt
            .as_ref()
            .filter(|receipt| receipt.matches_fingerprint(&self.facts.boundary_fingerprint))
            .ok_or_else(|| recipe_error("completed build has no matching measured socket receipt"))
    }

    /// Records one successful run and retains its Cargo message stream.
    ///
    /// Only the recipe's successful-run path calls this.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Recipe`] when the stream cannot be retained.
    pub(super) fn record_success(facts: SuccessFacts) -> Result<Self, Report<ReleaseError>> {
        let retained = tempfile::tempdir()
            .map_err(|error| recipe_error(format!("cannot retain Cargo output: {error}")))?;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(retained.path().join(STDOUT_FILE))
            .map_err(|error| recipe_error(format!("cannot retain Cargo output: {error}")))?;
        file.write_all(&facts.stdout)
            .and_then(|()| file.sync_all())
            .map_err(|error| recipe_error(format!("cannot retain Cargo output: {error}")))?;
        let stdout_sha256 = hex::encode(Sha256::digest(&facts.stdout));
        if let Some(captured) = &facts.cargo_observations {
            captured.retain(retained.path())?;
            captured.rehash(retained.path())?;
        }
        if let Some(javascript) = &facts.js_contributions {
            check_javascript_binding(&facts, javascript)?;
            let mut inventory = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(retained.path().join(JS_INVENTORY_FILE))
                .map_err(|error| recipe_error(format!("cannot retain JS inventory: {error}")))?;
            inventory
                .write_all(javascript.raw_inventory())
                .and_then(|()| inventory.sync_all())
                .map_err(|error| recipe_error(format!("cannot retain JS inventory: {error}")))?;
        }
        Ok(Self {
            facts,
            stdout_sha256,
            retained,
        })
    }

    /// The asset that was built.
    pub(crate) fn asset(&self) -> AssetId {
        self.facts.asset
    }

    /// The role of this build.
    pub(crate) fn role(&self) -> Role {
        self.facts.role
    }

    /// The approved source commit that was built.
    pub(crate) fn commit(&self) -> &CommitId {
        &self.facts.commit
    }

    /// SHA-256 of the approved raw `Cargo.lock`.
    pub(crate) fn cargo_lock_sha256(&self) -> &str {
        &self.facts.cargo_lock_sha256
    }

    /// Digest of the vendored dependency tree.
    pub(crate) fn materials_digest(&self) -> &str {
        &self.facts.materials_digest
    }

    /// Digests of the Git manifests the producer normalized, by vendor directory.
    ///
    /// Each was derived from the verified Git tree alone by the producer's
    /// reviewed normalization; on their own they do not make the build
    /// unqualified. They are also covered by the materials digest.
    pub(crate) fn normalized_manifest_digests(
        &self,
    ) -> &std::collections::BTreeMap<String, String> {
        &self.facts.normalized_manifest_digests
    }

    /// Digests of caller-derived Git manifests the build consumed, by vendor directory.
    ///
    /// Production vendoring accepts no caller-supplied manifest, so this is
    /// empty there. A non-empty set marks the build as built from derived
    /// inputs, so it is unqualified, and a qualified authority must refuse
    /// to attest it.
    pub(crate) fn derived_manifest_digests(&self) -> &std::collections::BTreeMap<String, String> {
        &self.facts.derived_manifest_digests
    }

    /// The JS handoff digests: inventory, settings and tools, in that order.
    pub(crate) fn js_digests(&self) -> [&str; 3] {
        [
            &self.facts.js_inventory_sha256,
            &self.facts.js_settings_sha256,
            &self.facts.js_tools_sha256,
        ]
    }

    /// Digest of the Rust tool tree measured at binding.
    pub(crate) fn rust_tools_digest(&self) -> &str {
        &self.facts.rust_tools_digest
    }

    /// Execution fingerprint of the boundary instance that ran the build.
    pub(crate) fn boundary_fingerprint(&self) -> [u8; 32] {
        self.facts.boundary_fingerprint
    }

    /// Digest of the exact Cargo plan that ran.
    pub(crate) fn plan_digest(&self) -> &str {
        self.facts.plan_digest.sha256()
    }

    /// The ambient conditions the build ran under.
    pub(crate) fn ambient(&self) -> &CheckedAmbient {
        &self.facts.ambient
    }

    /// Unix start time of the build in seconds.
    pub(crate) fn started_at_seconds(&self) -> u64 {
        self.facts.started_at_seconds
    }

    /// How the boundary that ran the build ended its descendants.
    pub(crate) fn descendant_cleanup(&self) -> DescendantCleanup {
        self.facts.descendant_cleanup
    }

    /// Wall time the build took.
    pub(crate) fn elapsed(&self) -> Duration {
        self.facts.elapsed
    }

    /// SHA-256 of Cargo's message stream.
    pub(crate) fn stdout_sha256(&self) -> &str {
        &self.stdout_sha256
    }

    /// SHA-256 of the captured diagnostics stream.
    pub(crate) fn stderr_sha256(&self) -> &str {
        &self.facts.stderr_sha256
    }

    /// Supporting Cargo observations; absent only in pure completion fixtures.
    pub(crate) fn cargo_observations_summary(&self) -> Option<serde_json::Value> {
        self.facts
            .cargo_observations
            .as_ref()
            .map(CargoCapture::summary)
    }

    /// Exports exact composition receipts bound to this completed build.
    ///
    /// # Errors
    ///
    /// Rejects changed owned outputs, missing composition, or summary-only
    /// observations. The export preserves existing incomplete coverage.
    pub(crate) fn composition_export(
        &self,
    ) -> Result<RetainedCompositionExport, Report<ReleaseError>> {
        self.rehash_for_handoff()?;
        self.facts
            .cargo_observations
            .as_ref()
            .ok_or_else(|| recipe_error("completed build has no retained composition"))?
            .export(self.retained.path())
    }

    /// Borrows sealed typed composition after rechecking this completed build.
    ///
    /// # Errors
    ///
    /// Rejects changed owned outputs or missing actual composition receipts.
    pub(crate) fn composition_view(&self) -> Result<CompositionView<'_>, Report<ReleaseError>> {
        self.rehash_for_handoff()?;
        self.facts
            .cargo_observations
            .as_ref()
            .ok_or_else(|| recipe_error("completed build has no retained composition"))?
            .view(self.retained.path())
    }

    /// Borrows checked original JS evidence retained when the recipe was bound.
    ///
    /// This snapshot survives cleanup and expiry of its original preparation
    /// attempt. It grants no new execution or composition-coverage authority.
    ///
    /// # Errors
    ///
    /// Rejects missing evidence, changed retained bytes or another source/digest.
    pub(crate) fn javascript_contributions(
        &self,
    ) -> Result<&RetainedContributions, Report<ReleaseError>> {
        self.rehash_for_handoff()?;
        self.facts
            .js_contributions
            .as_ref()
            .ok_or_else(|| recipe_error("completed build has no retained original JS inventory"))
    }

    /// The copied artifact.
    pub(crate) fn artifact(&self) -> &CopiedArtifact {
        &self.facts.artifact
    }

    /// Reads the retained final bytes after rechecking their size and digest.
    ///
    /// # Errors
    ///
    /// Rejects changed owned outputs, an oversized artifact or bytes whose size
    /// or SHA-256 differ from the recorded artifact.
    pub(crate) fn read_final_bytes(&self) -> Result<Vec<u8>, Report<ReleaseError>> {
        super::compare::read_artifact(self)
    }

    /// Re-measures the output directory and retained stream before handoff.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Recipe`] when the output directory holds
    /// anything but the artifact, the artifact is not one regular file with
    /// one link and the recorded size and SHA-256, or the retained Cargo
    /// stream no longer matches its digest.
    pub(crate) fn rehash_for_handoff(&self) -> Result<&Path, Report<ReleaseError>> {
        let artifact = &self.facts.artifact;
        let names = fs::read_dir(&self.facts.output_dir)
            .map_err(|error| recipe_error(format!("output cannot be listed: {error}")))?
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| recipe_error(format!("output cannot be listed: {error}")))?;
        if names != [std::ffi::OsString::from(artifact.name())] {
            return Err(recipe_error(
                "output directory holds more than the artifact",
            ));
        }
        let metadata = fs::symlink_metadata(artifact.path())
            .map_err(|error| recipe_error(format!("artifact cannot be inspected: {error}")))?;
        if !metadata.is_file() || metadata.nlink() != 1 {
            return Err(recipe_error("artifact is not one regular unlinked file"));
        }
        let measured = hash_bounded_with_check(artifact.path(), EXECUTABLE_LIMIT, &|| Ok(()))
            .map_err(|error| error.change_context(ReleaseError::Recipe))?;
        if measured.size != artifact.size() || measured.sha256 != artifact.sha256() {
            return Err(recipe_error("artifact bytes changed after the copy"));
        }
        let stream = hash_bounded_with_check(
            &self.retained.path().join(STDOUT_FILE),
            64 * 1024 * 1024,
            &|| Ok(()),
        )
        .map_err(|error| error.change_context(ReleaseError::Recipe))?;
        if stream.sha256 != self.stdout_sha256 {
            return Err(recipe_error("retained Cargo output changed"));
        }
        if let Some(captured) = &self.facts.cargo_observations {
            captured.rehash(self.retained.path())?;
        }
        if let Some(javascript) = &self.facts.js_contributions {
            check_javascript_binding(&self.facts, javascript)?;
            let measured = hash_bounded_with_check(
                &self.retained.path().join(JS_INVENTORY_FILE),
                javascript.raw_inventory().len() as u64,
                &|| Ok(()),
            )
            .map_err(|error| error.change_context(ReleaseError::Recipe))?;
            if measured.size != javascript.raw_inventory().len() as u64
                || measured.sha256 != javascript.inventory_sha256()
            {
                return Err(recipe_error("retained original JS inventory changed"));
            }
        }
        Ok(&self.facts.output_dir)
    }
}

fn check_javascript_binding(
    facts: &SuccessFacts,
    javascript: &RetainedContributions,
) -> Result<(), Report<ReleaseError>> {
    if javascript.source_commit() != facts.commit.as_str()
        || javascript.inventory_sha256() != facts.js_inventory_sha256
        || hex::encode(Sha256::digest(javascript.raw_inventory())) != facts.js_inventory_sha256
    {
        return Err(recipe_error(
            "retained JS inventory source or digest differs",
        ));
    }
    Ok(())
}

#[cfg(test)]
impl RecipeCompleted {
    /// Path of original retained JS observer bytes, for tamper tests.
    pub(crate) fn retained_inventory_path(&self) -> PathBuf {
        self.retained.path().join(JS_INVENTORY_FILE)
    }

    /// Path of the retained Cargo message stream, for tamper tests.
    pub(crate) fn retained_stdout_path(&self) -> PathBuf {
        self.retained.path().join(STDOUT_FILE)
    }

    /// Bound facts, for tests that forge one mismatched input.
    pub(super) fn facts_mut_for_tests(&mut self) -> &mut SuccessFacts {
        &mut self.facts
    }
}
