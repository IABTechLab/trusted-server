//! Flat diagnostic exports from owning build results, without release authority.
//!
//! This candidate format deliberately differs from the strict collector's build
//! record. Runtime acceptance is retained only from its actual owning run;
//! pair claims and qualified workflow identity remain unavailable.
//! [`strict_record`] separately renders the collector's record shape from the
//! same owners; it is not yet an export member.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};

use error_stack::Report;
use serde_json::json;

use super::{collection_error, digest};
use crate::documents::hash_bounded_with_check;
use crate::error::ReleaseError;
use crate::recipe::{FastlyRuntimeAccepted, NativeStartupAccepted, RecipeCompleted, copy_artifact};

const RECORD: &str = "candidate-build-record.json";
const COMPOSITION: &str = "composition.json";
const ARTIFACT: &str = "artifact";
const INVENTORY: &str = "tsjs-inventory.json";
const DOCUMENT_LIMIT: u64 = 16 * 1024 * 1024;

pub(crate) mod strict_record;

/// Owned flat candidate files, with no signing or trusted dependency authority.
#[derive(Debug)]
pub(crate) struct CandidateBuildExport {
    directory: tempfile::TempDir,
    root: PathBuf,
    members: BTreeMap<String, Member>,
}

#[derive(Debug)]
struct Member {
    size: u64,
    sha256: String,
}

enum RuntimeAcceptance<'a> {
    Fastly(&'a FastlyRuntimeAccepted),
    Native(&'a NativeStartupAccepted),
}

impl RuntimeAcceptance<'_> {
    fn diagnostic_for(
        &self,
        completed: &RecipeCompleted,
    ) -> Result<serde_json::Value, Report<ReleaseError>> {
        match self {
            Self::Fastly(accepted) => accepted.diagnostic_for(completed),
            Self::Native(accepted) => accepted.diagnostic_for(completed),
        }
    }
}

impl CandidateBuildExport {
    /// Copies measured owning results into a new flat diagnostic handoff.
    ///
    /// Actual socket observations and an approved-source recipe definition are
    /// mandatory. Exact invocation identity remains separate from that shared
    /// definition. Single-build ambient observations make no A/B variation claim.
    /// The original owner is checked again after the copy.
    ///
    /// # Errors
    ///
    /// Rejects missing measured receipts, missing composition, changed owned
    /// outputs, unsafe paths, oversized documents or changed copied bytes.
    pub(crate) fn from_completed(
        completed: &RecipeCompleted,
    ) -> Result<Self, Report<ReleaseError>> {
        Self::from_owned(completed, None)
    }

    /// Copies a candidate with its actual role-bound final-byte Fastly acceptance.
    ///
    /// # Errors
    ///
    /// Rejects another source, role or artifact, missing required build evidence,
    /// changed owned files or an oversized diagnostic record.
    pub(crate) fn with_fastly_runtime(
        completed: &RecipeCompleted,
        accepted: &FastlyRuntimeAccepted,
    ) -> Result<Self, Report<ReleaseError>> {
        Self::from_owned(completed, Some(RuntimeAcceptance::Fastly(accepted)))
    }

    /// Copies a candidate with its actual role-bound native startup acceptance.
    ///
    /// # Errors
    ///
    /// Rejects another source, role, selected CLI package or final artifact,
    /// missing build evidence, changed owned files or oversized diagnostics.
    pub(crate) fn with_native_startup(
        completed: &RecipeCompleted,
        accepted: &NativeStartupAccepted,
    ) -> Result<Self, Report<ReleaseError>> {
        Self::from_owned(completed, Some(RuntimeAcceptance::Native(accepted)))
    }

    fn from_owned(
        completed: &RecipeCompleted,
        accepted: Option<RuntimeAcceptance<'_>>,
    ) -> Result<Self, Report<ReleaseError>> {
        let definition = completed.recipe_definition()?;
        let denial = completed.denial_receipt()?;
        let composition = completed.composition_export()?;
        let javascript = completed.javascript_contributions()?;
        let artifact = completed.artifact();
        let observed = completed.ambient().observed();
        let runtime = accepted
            .map(|accepted| accepted.diagnostic_for(completed))
            .transpose()?;
        let gaps = [
            "runtime-acceptance-not-exported",
            "ambient-pair-claims-not-exported",
            "composition-coverage-incomplete",
            "bootstrap-not-authenticated",
            "workflow-producer-authority-not-established",
        ]
        .into_iter()
        .filter(|gap| *gap != "runtime-acceptance-not-exported" || runtime.is_none())
        .collect::<Vec<_>>();
        let [inventory, settings, js_tools] = completed.js_digests();
        let record = serde_json::to_vec(&json!({
            "domain": "trusted-server.release.candidate-build-diagnostic",
            "schema_version": 1,
            "release_qualification": false,
            "source_commit": completed.commit().as_str(),
            "asset": artifact.name(), "role": completed.role().spec_label(),
            "recipe_sha256": definition.sha256(),
            "recipe_definition": definition.descriptor(),
            "invocation_sha256": completed.plan_digest(),
            "artifact": {"member": ARTIFACT, "sha256": artifact.sha256(), "size": artifact.size()},
            "composition": {"member": COMPOSITION, "sha256": composition.sha256(), "summary": composition.summary()},
            "javascript_inventory": {"member": INVENTORY, "sha256": javascript.inventory_sha256(),
                "size": javascript.raw_inventory().len()},
            "inputs": {"cargo_lock": completed.cargo_lock_sha256(),
                "materials": completed.materials_digest(), "rust_tools": completed.rust_tools_digest(),
                "js_inventory": inventory, "js_settings": settings, "js_tools": js_tools},
            "socket_observations": denial,
            "runtime_acceptance": runtime,
            "ambient_observation": {
                "umask": observed.umask, "jobs": observed.jobs,
                "host_locale": observed.host_locale, "host_timezone": observed.host_timezone,
                "child_locale": observed.child_locale, "child_timezone": observed.child_timezone,
                "execution_uid": observed.execution_uid, "checkout_root": observed.checkout_root,
                "started_at_seconds": observed.started_at_seconds,
            },
            "elapsed_seconds": completed.elapsed().as_secs(),
            "gaps": gaps,
        }))
        .map_err(collection_error)?;
        if record.len() as u64 > DOCUMENT_LIMIT {
            return Err(collection_error("candidate build record exceeds its bound"));
        }
        let directory = tempfile::tempdir().map_err(collection_error)?;
        let root = fs::canonicalize(directory.path()).map_err(collection_error)?;
        let copied = copy_artifact(
            artifact.path(),
            completed.rehash_for_handoff()?,
            &root,
            ARTIFACT,
            artifact.size(),
        )?;
        if copied.size() != artifact.size() || copied.sha256() != artifact.sha256() {
            return Err(collection_error("candidate artifact copy changed"));
        }
        let mut members = BTreeMap::from([(
            ARTIFACT.to_owned(),
            Member {
                size: copied.size(),
                sha256: copied.sha256().to_owned(),
            },
        )]);
        for (name, bytes) in [
            (COMPOSITION, composition.bytes()),
            (INVENTORY, javascript.raw_inventory()),
            (RECORD, record.as_slice()),
        ] {
            write_new(&root.join(name), bytes)?;
            members.insert(
                name.to_owned(),
                Member {
                    size: bytes.len() as u64,
                    sha256: digest(bytes),
                },
            );
        }
        completed.rehash_for_handoff()?;
        let export = Self {
            directory,
            root,
            members,
        };
        export.rehash_for_handoff()?;
        Ok(export)
    }

    /// Returns the flat directory after checking every exact retained member.
    ///
    /// The owned files survive recipe/source/target cleanup. This verifies their
    /// integrity only and cannot authenticate a workflow upload or release.
    ///
    /// # Errors
    ///
    /// Rejects missing, extra, changed, aliased, linked or oversized members.
    pub(crate) fn rehash_for_handoff(&self) -> Result<&Path, Report<ReleaseError>> {
        let root = fs::canonicalize(self.directory.path()).map_err(collection_error)?;
        if root != self.root {
            return Err(collection_error("candidate export directory changed"));
        }
        let names = fs::read_dir(&self.root)
            .map_err(collection_error)?
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(collection_error)?;
        if names.len() != self.members.len()
            || names.iter().any(|name| {
                !name
                    .to_str()
                    .is_some_and(|name| self.members.contains_key(name))
            })
        {
            return Err(collection_error("candidate export members changed"));
        }
        for (name, expected) in &self.members {
            let measured =
                hash_bounded_with_check(&self.root.join(name), expected.size, &|| Ok(()))
                    .map_err(|error| error.change_context(ReleaseError::Collection))?;
            if measured.size != expected.size || measured.sha256 != expected.sha256 {
                return Err(collection_error("candidate export bytes changed"));
            }
        }
        Ok(&self.root)
    }
}

/// Writes `bytes` to a new private file, refusing an existing path or link.
///
/// # Errors
///
/// Rejects an existing path, a symlink at `path` and any write or sync failure.
pub(crate) fn write_new(path: &Path, bytes: &[u8]) -> Result<(), Report<ReleaseError>> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(collection_error)?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(collection_error)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;

    // These files exercise diagnostic-file integrity only. They contain no
    // measured execution receipts and cannot enter the owning producer path.
    fn diagnostic_files() -> CandidateBuildExport {
        let directory = tempfile::tempdir().expect("should create diagnostic storage");
        let root = fs::canonicalize(directory.path()).expect("should resolve diagnostic storage");
        let mut members = BTreeMap::new();
        for (name, bytes) in [
            (ARTIFACT, b"fictional executable".as_slice()),
            (COMPOSITION, b"fictional composition".as_slice()),
            (INVENTORY, b"fictional original inventory".as_slice()),
            (RECORD, b"fictional record".as_slice()),
        ] {
            write_new(&root.join(name), bytes).expect("should retain diagnostic member");
            members.insert(
                name.to_owned(),
                Member {
                    size: bytes.len() as u64,
                    sha256: digest(bytes),
                },
            );
        }
        CandidateBuildExport {
            directory,
            root,
            members,
        }
    }

    #[test]
    fn flat_diagnostic_members_survive_without_original_build_roots() {
        let export = diagnostic_files();
        let root = export
            .rehash_for_handoff()
            .expect("should verify retained diagnostic files");
        assert_eq!(
            fs::read_dir(root).expect("should list export").count(),
            4,
            "should retain artifact, original inventory, composition and diagnostic record"
        );
        assert!(
            root.join("tsjs-inventory.json").is_file(),
            "should retain original JS inventory"
        );
    }

    #[test]
    fn each_changed_export_member_blocks_handoff() {
        for name in [ARTIFACT, COMPOSITION, INVENTORY, RECORD] {
            let export = diagnostic_files();
            fs::write(export.root.join(name), b"changed").expect("should change one member");
            assert!(
                export.rehash_for_handoff().is_err(),
                "should reject changed {name}"
            );
        }
    }

    #[test]
    fn missing_and_extra_export_members_block_handoff() {
        let missing = diagnostic_files();
        fs::remove_file(missing.root.join(COMPOSITION)).expect("should remove composition");
        assert!(
            missing.rehash_for_handoff().is_err(),
            "should reject missing member"
        );
        let extra = diagnostic_files();
        fs::write(extra.root.join("unexpected"), b"extra").expect("should add extra member");
        assert!(
            extra.rehash_for_handoff().is_err(),
            "should reject undeclared member"
        );
    }

    #[test]
    fn linked_export_members_block_handoff_even_when_bytes_match() {
        for hard_link in [false, true] {
            let export = diagnostic_files();
            let outside = tempfile::tempdir().expect("should create separate file storage");
            let path = outside.path().join("same-bytes");
            fs::copy(export.root.join(ARTIFACT), &path).expect("should copy matching bytes");
            fs::remove_file(export.root.join(ARTIFACT)).expect("should remove original member");
            if hard_link {
                fs::hard_link(&path, export.root.join(ARTIFACT))
                    .expect("should hard link matching bytes");
            } else {
                symlink(&path, export.root.join(ARTIFACT)).expect("should alias matching bytes");
            }
            assert!(
                export.rehash_for_handoff().is_err(),
                "should reject linked artifact with matching bytes"
            );
        }
    }
}
