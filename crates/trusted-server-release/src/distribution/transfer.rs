//! Test-only ordinary-CI transfer of candidate entries between CI jobs.
//!
//! Each real candidate job exports its rechecked [`CandidateArtifactEntry`]
//! as six flat files (the final executable, the shared JS inventory and the
//! four unsigned companions) plus a separate CI-internal descriptor. A
//! collector job downloads the four exact uploads, rebuilds each entry with
//! [`CandidateArtifactEntry::from_ordinary_ci_transfer`], which reruns every
//! producer join over the transferred bytes, and assembles the unqualified
//! candidate index with the unchanged [`assemble_candidate_index`].
//!
//! The descriptor is ordinary-CI plumbing, not a release schema. Nothing here
//! binds a trusted workflow transfer: companions keep `null` workflow and
//! transfer identities and the `trusted-workflow-transfer-not-bound` gap, and
//! nothing grants signing, publication or qualification authority.

use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::fs::DirBuilderExt as _;
use std::path::Path;

use error_stack::Report;
use serde_json::{Value, json};

use super::entry::{ENTRY_GAPS, MAX_GAP_LENGTH, NATIVE_REPRODUCTION_GAP, policy_identity};
use super::joins::{
    EvidenceJoin, INVENTORY_ROLE, check_build_evidence, check_comparison, check_spdx_root,
    inventory_modules, parse_informational,
};
use super::{
    CandidateArtifactEntry, CandidateIndex, CompanionFile, CompanionRole, INVENTORY_NAME,
    assemble_candidate_index, distribution_error, sha256_hex,
};
use crate::collection::producer::write_new;
use crate::documents::{Role as DocumentRole, canonical_json, lower_hex, parse_json};
use crate::error::ReleaseError;
use crate::policy::ReleaseTarget;
use crate::recipe::AssetId;

/// Flat member name of the transferred final executable.
pub(crate) const ARTIFACT_MEMBER: &str = "artifact";
/// Prefix of every candidate-entry upload name.
const UPLOAD_PREFIX: &str = "candidate-entry-";
/// Keys the producing job's transfer file carries.
const TRANSFER_KEYS: [&str; 3] = ["asset", "entry_sha256", "members"];
/// Keys the CI describe step adds from the exact upload-step outputs.
const UPLOAD_KEYS: [&str; 4] = [
    "archive_sha256",
    "artifact_id",
    "artifact_name",
    "run_attempt",
];
/// Domain of the rendered build-evidence companion.
const EVIDENCE_DOMAIN: &str = "trusted-server.release.candidate-build-evidence";
/// Domain of the rendered build-recipe companion.
const RECIPE_DOMAIN: &str = "trusted-server.release.candidate-build-recipe";
/// Domain of the rendered rebuild-check companion.
const REBUILD_DOMAIN: &str = "trusted-server.release.candidate-rebuild-check";

type Result<T> = core::result::Result<T, Report<ReleaseError>>;

fn parse(bytes: &[u8], role: DocumentRole) -> Result<Value> {
    parse_json(bytes, role).map_err(|error| error.change_context(ReleaseError::Distribution))
}

fn write_member(path: &Path, bytes: &[u8]) -> Result<()> {
    write_new(path, bytes).map_err(|error| error.change_context(ReleaseError::Distribution))
}

/// Creates a new private directory, refusing an existing path.
fn create_private_directory(directory: &Path) -> Result<()> {
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(directory)
        .map_err(|error| {
            distribution_error(format!(
                "cannot create new transfer directory {}: {error}",
                directory.display()
            ))
        })
}

/// The byte bound of one transferred member of `asset`.
fn member_limit(asset: AssetId, name: &str) -> Option<u64> {
    if name == ARTIFACT_MEMBER {
        return Some(DocumentRole::Executable.limit());
    }
    if name == INVENTORY_NAME {
        return Some(INVENTORY_ROLE.limit());
    }
    CompanionRole::ALL
        .into_iter()
        .find(|role| name == format!("{}{}", asset.artifact().name, role.suffix()))
        .map(CompanionRole::limit)
}

/// The exact six flat member names transferred for `asset`.
pub(crate) fn member_names(asset: AssetId) -> BTreeSet<String> {
    let mut names: BTreeSet<String> = CompanionRole::ALL
        .into_iter()
        .map(|role| format!("{}{}", asset.artifact().name, role.suffix()))
        .collect();
    names.insert(ARTIFACT_MEMBER.to_owned());
    names.insert(INVENTORY_NAME.to_owned());
    names
}

/// The exact upload name of `asset` from producing `run_id` and `run_attempt`.
pub(crate) fn upload_name(asset: AssetId, run_id: u64, run_attempt: u64) -> String {
    format!(
        "{UPLOAD_PREFIX}{}-{run_id}-{run_attempt}",
        asset.artifact().name
    )
}

impl CandidateArtifactEntry {
    /// The exact flat member bytes of this entry plus its final executable.
    pub(super) fn transfer_members<'a>(&'a self, artifact: &'a [u8]) -> BTreeMap<String, &'a [u8]> {
        let mut members: BTreeMap<String, &[u8]> = self
            .companions
            .iter()
            .map(|file| (file.name.clone(), file.bytes.as_slice()))
            .collect();
        members.insert(ARTIFACT_MEMBER.to_owned(), artifact);
        members.insert(INVENTORY_NAME.to_owned(), &self.inventory);
        members
    }

    /// Exports this rechecked entry for the ordinary-CI index collector.
    ///
    /// Writes exactly the six flat members into the new directory
    /// `upload_directory` and the transfer file (asset, member sizes and
    /// the rechecked entry digest) to the new file `transfer_file`, which
    /// must lie outside the upload directory.
    ///
    /// # Errors
    ///
    /// Rejects executable bytes other than the entry's recorded size and
    /// digest, a failed recheck, an existing directory or file, a transfer
    /// file inside the upload directory and any write failure.
    pub(crate) fn export_flat_for_ordinary_ci(
        &self,
        artifact: &[u8],
        upload_directory: &Path,
        transfer_file: &Path,
    ) -> Result<()> {
        if artifact.len() as u64 != self.artifact_size
            || sha256_hex(artifact) != self.artifact_sha256
        {
            return Err(distribution_error(
                "exported executable bytes differ from the entry's recorded artifact",
            ));
        }
        if transfer_file.starts_with(upload_directory) {
            return Err(distribution_error(
                "transfer file must lie outside the uploaded directory",
            ));
        }
        let summary = self.recheck_for_measurement()?;
        let members = self.transfer_members(artifact);
        let sizes: BTreeMap<&str, u64> = members
            .iter()
            .map(|(name, bytes)| (name.as_str(), bytes.len() as u64))
            .collect();
        let transfer = canonical_json(
            &json!({
                "asset": self.name(),
                "entry_sha256": summary.entry_sha256(),
                "members": sizes,
            }),
            DocumentRole::Metadata,
        )
        .map_err(|error| error.change_context(ReleaseError::Distribution))?;
        create_private_directory(upload_directory)?;
        for (name, bytes) in &members {
            write_member(&upload_directory.join(name), bytes)?;
        }
        write_member(transfer_file, &transfer)
    }

    /// Rebuilds one entry from an ordinary-CI job's transferred bytes.
    ///
    /// Test-only. `policy` must be the consumer policy bytes the collector
    /// read from its own authenticated checkout; the policy identity is never
    /// taken from the producing job. Every producer join reruns over the
    /// exact bytes: the shared inventory, build evidence (including the
    /// executable's size and digest, which the transferred executable must
    /// match), the SPDX root, the informational recipe and the rebuild check.
    /// Fastly must record the checked `matched` equality and native assets
    /// `unavailable`; every companion must be unqualified, carry `null`
    /// workflow and transfer identities and state the same sorted gaps.
    ///
    /// # Errors
    ///
    /// Rejects a missing, extra or misnamed member, any companion naming
    /// another asset, target, source or executable, a qualified companion,
    /// differing or incomplete gaps, a comparison outcome other than the
    /// asset's rule, a policy version other than the compiled one and any
    /// failed join or oversized companion.
    pub(crate) fn from_ordinary_ci_transfer(
        asset: AssetId,
        source_commit: &str,
        policy: &[u8],
        members: &BTreeMap<String, Vec<u8>>,
    ) -> Result<Self> {
        let expected = member_names(asset);
        if !members.keys().eq(expected.iter()) {
            return Err(distribution_error(format!(
                "transfer for `{}` must carry exactly its six flat members",
                asset.artifact().name
            )));
        }
        if !lower_hex(source_commit, 40) {
            return Err(distribution_error("expected source commit is malformed"));
        }
        let (policy_version, policy_sha256) = policy_identity(policy)?;
        let member = |name: &str| -> &[u8] { members.get(name).map_or(&[], Vec::as_slice) };
        let companion = |role: CompanionRole| {
            member(&format!("{}{}", asset.artifact().name, role.suffix())).to_vec()
        };
        let evidence_bytes = companion(CompanionRole::BuildEvidence);
        let evidence = parse(&evidence_bytes, DocumentRole::Companion)?;
        let identity = check_evidence_identity(asset, source_commit, &evidence)?;
        let artifact = member(ARTIFACT_MEMBER);
        if artifact.len() as u64 != identity.artifact_size
            || sha256_hex(artifact) != identity.artifact_sha256
        {
            return Err(distribution_error(
                "transferred executable differs from its build evidence",
            ));
        }
        let inventory = member(INVENTORY_NAME).to_vec();
        let inventory_sha256 = sha256_hex(&inventory);
        let modules = inventory_modules(&inventory, source_commit)?;
        check_build_evidence(
            &evidence_bytes,
            &EvidenceJoin {
                source_commit,
                target: asset.artifact().target.triple(),
                artifact_sha256: &identity.artifact_sha256,
                inventory_sha256: &inventory_sha256,
                modules: &modules,
            },
        )?;
        if is_fastly(asset) && evidence["embedded_modules"] != evidence["prepared_modules"] {
            return Err(distribution_error(
                "Fastly executable must embed every prepared JS module",
            ));
        }
        let spdx = companion(CompanionRole::Sbom);
        check_spdx_root(&spdx, &identity.artifact_sha256)?;
        let recipe_bytes = companion(CompanionRole::BuildRecipe);
        let recipe = parse_informational(&recipe_bytes, source_commit)?;
        check_recipe(asset, &recipe)?;
        let rebuild_bytes = companion(CompanionRole::RebuildCheck);
        check_comparison(&rebuild_bytes, source_commit)?;
        let rebuild = parse_informational(&rebuild_bytes, source_commit)?;
        check_rebuild(asset, &identity.artifact_sha256, &rebuild)?;
        let gaps = check_gaps(asset, &evidence)?;
        for (role, document) in [
            (CompanionRole::BuildRecipe, &recipe),
            (CompanionRole::RebuildCheck, &rebuild),
        ] {
            if document["release_qualification"] != false || document["gaps"] != json!(gaps) {
                return Err(distribution_error(format!(
                    "{} companion must be unqualified and state exactly the entry gaps",
                    role.key()
                )));
            }
        }
        let name = asset.artifact().name;
        let entry = Self {
            asset,
            source_commit: source_commit.to_owned(),
            policy_version,
            policy_sha256,
            inventory,
            artifact_size: identity.artifact_size,
            artifact_sha256: identity.artifact_sha256,
            companions: vec![
                CompanionFile::new(CompanionRole::Sbom, name, spdx),
                CompanionFile::new(CompanionRole::BuildEvidence, name, evidence_bytes),
                CompanionFile::new(CompanionRole::BuildRecipe, name, recipe_bytes),
                CompanionFile::new(CompanionRole::RebuildCheck, name, rebuild_bytes),
            ],
            gaps,
        };
        entry.check_companions(&mut BTreeSet::new())?;
        Ok(entry)
    }
}

fn is_fastly(asset: AssetId) -> bool {
    asset.artifact().target == ReleaseTarget::Wasm32Wasip1
}

/// The executable identity build evidence records.
struct EvidenceIdentity {
    artifact_sha256: String,
    artifact_size: u64,
}

fn check_evidence_identity(
    asset: AssetId,
    source_commit: &str,
    evidence: &Value,
) -> Result<EvidenceIdentity> {
    let artifact = asset.artifact();
    if evidence["domain"] != EVIDENCE_DOMAIN
        || evidence["schema_version"] != 1
        || evidence["release_qualification"] != false
        || evidence["asset"] != artifact.name
        || evidence["target"] != artifact.target.triple()
        || evidence["source_commit"] != source_commit
        || !evidence["workflow_run_id"].is_null()
        || !evidence["transfer_artifact_id"].is_null()
    {
        return Err(distribution_error(format!(
            "build evidence is not the unqualified candidate evidence of `{}` at the expected source",
            artifact.name
        )));
    }
    let artifact_sha256 = evidence["artifact_sha256"]
        .as_str()
        .filter(|digest| lower_hex(digest, 64))
        .map(str::to_owned);
    let artifact_size = evidence["artifact_size"]
        .as_u64()
        .filter(|size| (1..=DocumentRole::Executable.limit()).contains(size));
    let (Some(artifact_sha256), Some(artifact_size)) = (artifact_sha256, artifact_size) else {
        return Err(distribution_error(
            "build evidence executable identity is malformed",
        ));
    };
    Ok(EvidenceIdentity {
        artifact_sha256,
        artifact_size,
    })
}

fn check_recipe(asset: AssetId, recipe: &Value) -> Result<()> {
    let artifact = asset.artifact();
    if recipe["domain"] != RECIPE_DOMAIN
        || recipe["asset"] != artifact.name
        || recipe["package"] != artifact.package
        || recipe["target"] != artifact.target.triple()
        || !recipe["workflow_run_id"].is_null()
    {
        return Err(distribution_error(format!(
            "build recipe is not the candidate recipe of `{}`",
            artifact.name
        )));
    }
    Ok(())
}

fn check_rebuild(asset: AssetId, artifact_sha256: &str, rebuild: &Value) -> Result<()> {
    let name = asset.artifact().name;
    if rebuild["domain"] != REBUILD_DOMAIN
        || rebuild["asset"] != name
        || rebuild["canonical_sha256"] != artifact_sha256
        || !rebuild["workflow_run_id"].is_null()
        || !rebuild["reproduction_transfer_artifact_id"].is_null()
    {
        return Err(distribution_error(format!(
            "rebuild check is not the candidate check of `{name}`"
        )));
    }
    let rule_holds = if is_fastly(asset) {
        rebuild["outcome"] == "matched" && rebuild["reproduction_sha256"] == artifact_sha256
    } else {
        rebuild["outcome"] == "unavailable" && rebuild["reproduction_sha256"].is_null()
    };
    if !rule_holds {
        return Err(distribution_error(format!(
            "rebuild check of `{name}` must be the checked Fastly equality or unavailable for a native asset"
        )));
    }
    Ok(())
}

/// Requires sorted, unique, well-formed gaps covering the fixed entry gaps,
/// the native reproduction gap exactly for native assets and at least one
/// SPDX gap.
fn check_gaps(asset: AssetId, evidence: &Value) -> Result<Vec<String>> {
    let gaps = evidence["gaps"]
        .as_array()
        .ok_or_else(|| distribution_error("build evidence states no gap list"))?
        .iter()
        .map(|gap| {
            gap.as_str()
                .filter(|text| {
                    !text.is_empty()
                        && text.len() <= MAX_GAP_LENGTH
                        && !text
                            .chars()
                            .any(|character| character.is_control() || character.is_whitespace())
                })
                .map(str::to_owned)
                .ok_or_else(|| distribution_error("candidate gap text is malformed"))
        })
        .collect::<Result<Vec<String>>>()?;
    if gaps.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(distribution_error(
            "candidate gaps must be sorted and unique",
        ));
    }
    let has = |wanted: &str| gaps.iter().any(|gap| gap == wanted);
    if let Some(missing) = ENTRY_GAPS.iter().find(|gap| !has(gap)) {
        return Err(distribution_error(format!(
            "candidate gaps omit the fixed gap `{missing}`"
        )));
    }
    if has(NATIVE_REPRODUCTION_GAP) == is_fastly(asset) {
        return Err(distribution_error(
            "native reproduction gap must be stated exactly for native assets",
        ));
    }
    let fixed = ENTRY_GAPS.len() + usize::from(!is_fastly(asset));
    if gaps.len() <= fixed {
        return Err(distribution_error("candidate SPDX must state its gaps"));
    }
    Ok(gaps)
}

/// One producing job's checked upload descriptor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EntryDescriptor {
    asset: AssetId,
    entry_sha256: String,
    artifact_id: u64,
    artifact_name: String,
    archive_sha256: String,
    members: BTreeMap<String, u64>,
}

impl EntryDescriptor {
    /// Strictly parses the descriptor the producing job emitted for `asset`.
    ///
    /// The descriptor's own `asset` must equal the slot's asset, its upload
    /// name must be exactly that asset's name for `run_id` and the producing
    /// job's own `run_attempt`, and its members must be exactly the six flat
    /// names, each non-empty and within its role bound.
    ///
    /// Attempts may differ between descriptors: re-running only a failed job
    /// gives it a new attempt while the others keep their earlier uploads.
    /// REST metadata has no attempt field, so the exact recomputed name, with
    /// the corroborated run, ID, digest, head and repository, carries the
    /// binding.
    ///
    /// # Errors
    ///
    /// Rejects malformed JSON, missing or unknown keys, a zero ID or attempt,
    /// a malformed digest, another asset or upload name, and wrong member
    /// names or sizes.
    pub(crate) fn parse(bytes: &[u8], asset: AssetId, run_id: u64) -> Result<Self> {
        let value = parse(bytes, DocumentRole::Metadata)?;
        let keys: BTreeSet<&str> = value
            .as_object()
            .ok_or_else(|| distribution_error("candidate entry descriptor must be an object"))?
            .keys()
            .map(String::as_str)
            .collect();
        let wanted: BTreeSet<&str> = TRANSFER_KEYS.into_iter().chain(UPLOAD_KEYS).collect();
        if keys != wanted {
            return Err(distribution_error(
                "candidate entry descriptor must carry exactly its transfer and upload keys",
            ));
        }
        let name = asset.artifact().name;
        if value["asset"] != name {
            return Err(distribution_error(format!(
                "descriptor names another asset than `{name}`"
            )));
        }
        let entry_sha256 = value["entry_sha256"]
            .as_str()
            .filter(|digest| lower_hex(digest, 64))
            .ok_or_else(|| distribution_error("descriptor entry digest is malformed"))?;
        let archive = value["archive_sha256"].as_str().unwrap_or_default();
        let archive_sha256 = archive.strip_prefix("sha256:").unwrap_or(archive);
        if !lower_hex(archive_sha256, 64) {
            return Err(distribution_error("descriptor archive digest is malformed"));
        }
        let artifact_id = value["artifact_id"]
            .as_u64()
            .filter(|id| *id > 0)
            .ok_or_else(|| distribution_error("descriptor upload ID must be positive"))?;
        let run_attempt = value["run_attempt"]
            .as_u64()
            .filter(|attempt| *attempt > 0)
            .ok_or_else(|| distribution_error("descriptor run attempt must be positive"))?;
        let artifact_name = upload_name(asset, run_id, run_attempt);
        if value["artifact_name"] != artifact_name.as_str() {
            return Err(distribution_error(format!(
                "descriptor upload name must be `{artifact_name}`"
            )));
        }
        let members = parse_members(asset, &value["members"])?;
        Ok(Self {
            asset,
            entry_sha256: entry_sha256.to_owned(),
            artifact_id,
            artifact_name,
            archive_sha256: archive_sha256.to_owned(),
            members,
        })
    }

    /// The asset this descriptor's slot names.
    pub(crate) fn asset(&self) -> AssetId {
        self.asset
    }

    /// The exact upload ID returned by the producing upload step.
    pub(crate) fn artifact_id(&self) -> u64 {
        self.artifact_id
    }

    /// The exact upload name the producing job used.
    pub(crate) fn artifact_name(&self) -> &str {
        &self.artifact_name
    }

    /// The original upload ZIP digest, without a `sha256:` prefix.
    pub(crate) fn archive_sha256(&self) -> &str {
        &self.archive_sha256
    }

    /// The exact flat member names and sizes declared by the producing job.
    pub(crate) fn members(&self) -> &BTreeMap<String, u64> {
        &self.members
    }
}

fn parse_members(asset: AssetId, value: &Value) -> Result<BTreeMap<String, u64>> {
    let object = value
        .as_object()
        .ok_or_else(|| distribution_error("descriptor members must be an object"))?;
    let names: BTreeSet<String> = object.keys().cloned().collect();
    if names != member_names(asset) {
        return Err(distribution_error(format!(
            "descriptor must declare exactly the six flat members of `{}`",
            asset.artifact().name
        )));
    }
    object
        .iter()
        .map(|(name, size)| {
            let limit = member_limit(asset, name).unwrap_or_default();
            size.as_u64()
                .filter(|size| (1..=limit).contains(size))
                .map(|size| (name.clone(), size))
                .ok_or_else(|| {
                    distribution_error(format!(
                        "descriptor member `{name}` size is empty or exceeds its bound"
                    ))
                })
        })
        .collect()
}

/// Requires four descriptors with distinct upload IDs, one per cohort asset.
///
/// # Errors
///
/// Rejects a repeated upload ID or asset.
pub(crate) fn check_distinct_uploads(descriptors: &[&EntryDescriptor]) -> Result<()> {
    let ids: BTreeSet<u64> = descriptors.iter().map(|item| item.artifact_id).collect();
    let assets: BTreeSet<&str> = descriptors
        .iter()
        .map(|item| item.asset.artifact().name)
        .collect();
    if ids.len() != descriptors.len() || assets.len() != descriptors.len() {
        return Err(distribution_error(
            "candidate entry uploads must have distinct IDs and assets",
        ));
    }
    Ok(())
}

/// One downloaded upload: its descriptor and its exact extracted members.
pub(crate) struct ReceivedEntry {
    /// The checked descriptor the producing job emitted.
    pub(crate) descriptor: EntryDescriptor,
    /// The flat members read through the unchanged ZIP reader.
    pub(crate) members: BTreeMap<String, Vec<u8>>,
}

/// Rebuilds each received entry and assembles the unqualified candidate index.
///
/// Each rebuilt entry must recheck to exactly the entry digest its producing
/// job reported, and its members must be exactly the declared names and sizes.
///
/// # Errors
///
/// Rejects repeated uploads, members differing from their descriptor, any
/// rebuild failure, an entry digest other than the producing job's, and every
/// [`assemble_candidate_index`] rejection.
pub(crate) fn assemble_received(
    received: &[ReceivedEntry],
    source_commit: &str,
    policy: &[u8],
) -> Result<CandidateIndex> {
    let descriptors: Vec<&EntryDescriptor> = received.iter().map(|item| &item.descriptor).collect();
    check_distinct_uploads(&descriptors)?;
    let entries = received
        .iter()
        .map(|item| {
            let declared = &item.descriptor.members;
            let matches = item.members.len() == declared.len()
                && item
                    .members
                    .iter()
                    .all(|(name, bytes)| declared.get(name) == Some(&(bytes.len() as u64)));
            if !matches {
                return Err(distribution_error(format!(
                    "received members of `{}` differ from their descriptor",
                    item.descriptor.asset.artifact().name
                )));
            }
            let entry = CandidateArtifactEntry::from_ordinary_ci_transfer(
                item.descriptor.asset,
                source_commit,
                policy,
                &item.members,
            )?;
            if entry.recheck_for_measurement()?.entry_sha256() != item.descriptor.entry_sha256 {
                return Err(distribution_error(format!(
                    "rebuilt entry of `{}` differs from the producing job's entry",
                    entry.name()
                )));
            }
            Ok(entry)
        })
        .collect::<Result<Vec<_>>>()?;
    assemble_candidate_index(&entries)
}

/// Writes the index, shared inventory and sixteen companions into the new
/// directory `root`.
///
/// # Errors
///
/// Rejects an existing directory and any write failure.
pub(crate) fn write_candidate_index(index: &CandidateIndex, root: &Path) -> Result<()> {
    create_private_directory(root)?;
    write_member(&root.join(index.name()), index.bytes())?;
    write_member(&root.join(INVENTORY_NAME), index.inventory())?;
    for (name, bytes) in index.companions() {
        write_member(&root.join(name), bytes)?;
    }
    Ok(())
}
