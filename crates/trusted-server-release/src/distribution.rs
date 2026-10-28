//! Unqualified candidate release index assembled from completed builds.
//!
//! A [`CandidateIndex`] joins exactly one canonical entry per fixed cohort
//! asset into release-index v1 bytes that pass the retained governing schema
//! and mirrored consumer semantic checks. It is a candidate only:
//!
//! - its tag is the fixed sentinel [`CANDIDATE_TAG`], never a release tag;
//! - its bytes are only ever named [`CANDIDATE_INDEX_NAME`], never
//!   `release-index.json`;
//! - both signature bundle slots are [`BundleSlot::AwaitingSignature`] and
//!   render the true empty-file record, because [`SignedCompanions`] is
//!   uninhabited and no file is written for them;
//! - every unsigned companion carries `"release_qualification": false` and
//!   its explicit gaps.
//!
//! No command, writer, signer or publisher consumes this module. Only the
//! test-only `transfer` and `ordinary` modules write candidate files, for the
//! ordinary-CI cross-job join. Nothing here grants signing, publication or
//! qualification authority.

use std::collections::{BTreeMap, BTreeSet};

use error_stack::Report;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use crate::documents::{
    Role as DocumentRole, canonical_json, is_safe_flat_name, lower_hex, parse_json,
    validate_release_index, validate_release_index_artifact,
};
use crate::error::ReleaseError;
use crate::policy::{COHORT_ID, FIXED_ARTIFACTS, REPOSITORY};
use crate::recipe::AssetId;

mod entry;
mod joins;
#[cfg(test)]
mod ordinary;
#[cfg(test)]
mod summary;
#[cfg(test)]
pub(crate) mod transfer;

#[cfg(test)]
mod tests;

#[allow(
    unused_imports,
    reason = "the completed-build entry path has no production caller yet"
)]
pub(crate) use entry::{
    AcceptanceKind, CandidateAcceptance, candidate_entry_from_completed, check_candidate_owner,
};

/// Fixed tag of every candidate index; never an official release tag.
pub(crate) const CANDIDATE_TAG: &str = "v0.0.0-unqualified-candidate";
/// The only name candidate index bytes are ever given.
pub(crate) const CANDIDATE_INDEX_NAME: &str = "candidate-release-index.json";
/// Fixed name of the shared JS inventory asset.
const INVENTORY_NAME: &str = "tsjs-inventory.json";
/// SHA-256 of zero bytes: the true digest of an empty bundle file record.
const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
/// Gaps every candidate index states in addition to its entries' gaps.
const INDEX_GAPS: [&str; 2] = [
    "candidate-tag-sentinel",
    "signature-bundles-awaiting-signature",
];
/// Signature bundle companion keys and their asset-name suffixes.
const BUNDLE_SUFFIXES: [(&str, &str); 2] = [
    ("provenance", ".provenance.sigstore.json"),
    ("sbom_attestation", ".sbom.sigstore.json"),
];

/// Builds a distribution error carrying `message`.
fn distribution_error(message: impl std::fmt::Display) -> Report<ReleaseError> {
    Report::new(ReleaseError::Distribution).attach(message.to_string())
}

/// Lowercase hexadecimal SHA-256 of the exact bytes.
fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Signed companion bundles; uninhabited until isolated signing exists.
pub(crate) enum SignedCompanions {}

/// One signature bundle slot of an index artifact entry.
pub(crate) enum BundleSlot {
    /// No bundle exists yet; the slot renders the true empty-file record.
    AwaitingSignature,
    /// A signed bundle; unreachable because [`SignedCompanions`] is uninhabited.
    Signed(SignedCompanions),
}

impl BundleSlot {
    /// Renders this slot's index file record under `name`.
    fn record(&self, name: &str) -> Value {
        match self {
            Self::AwaitingSignature => json!({"name": name, "size": 0, "sha256": EMPTY_SHA256}),
            Self::Signed(signed) => match *signed {},
        }
    }
}

/// The four present unsigned companion roles of one artifact entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CompanionRole {
    Sbom,
    BuildEvidence,
    BuildRecipe,
    RebuildCheck,
}

impl CompanionRole {
    const ALL: [Self; 4] = [
        Self::Sbom,
        Self::BuildEvidence,
        Self::BuildRecipe,
        Self::RebuildCheck,
    ];

    /// The index companion field name.
    fn key(self) -> &'static str {
        match self {
            Self::Sbom => "sbom",
            Self::BuildEvidence => "build_evidence",
            Self::BuildRecipe => "build_recipe",
            Self::RebuildCheck => "rebuild_check",
        }
    }

    /// The required suffix after the asset name.
    fn suffix(self) -> &'static str {
        match self {
            Self::Sbom => ".spdx.json",
            Self::BuildEvidence => ".build-evidence.json",
            Self::BuildRecipe => ".build-recipe.json",
            Self::RebuildCheck => ".rebuild-check.json",
        }
    }

    /// The byte bound of this companion role.
    fn limit(self) -> u64 {
        match self {
            Self::Sbom => DocumentRole::Spdx.limit(),
            Self::BuildEvidence | Self::BuildRecipe | Self::RebuildCheck => {
                DocumentRole::Companion.limit()
            }
        }
    }
}

/// One rendered unsigned companion and the identity recorded for it.
#[derive(Clone, Debug)]
struct CompanionFile {
    role: CompanionRole,
    name: String,
    size: u64,
    sha256: String,
    bytes: Vec<u8>,
}

impl CompanionFile {
    /// Records `bytes` under the role-derived name for `asset`.
    fn new(role: CompanionRole, asset: &str, bytes: Vec<u8>) -> Self {
        Self {
            role,
            name: format!("{asset}{}", role.suffix()),
            size: bytes.len() as u64,
            sha256: sha256_hex(&bytes),
            bytes,
        }
    }

    fn record(&self) -> Value {
        json!({"name": self.name, "size": self.size, "sha256": self.sha256})
    }
}

/// One canonical cohort asset's checked index entry and companion bytes.
///
/// Only [`candidate_entry_from_completed`] constructs it in production; its
/// fields are private and production has no byte or JSON constructor. The
/// single exception is test-only: `distribution::transfer` rebuilds an entry
/// from an ordinary-CI job's transferred files and reruns every producer join
/// over those bytes, so the four real entries can be joined across CI jobs.
#[derive(Clone, Debug)]
pub(crate) struct CandidateArtifactEntry {
    asset: AssetId,
    source_commit: String,
    policy_version: u64,
    policy_sha256: String,
    inventory: Vec<u8>,
    artifact_size: u64,
    artifact_sha256: String,
    companions: Vec<CompanionFile>,
    gaps: Vec<String>,
}

impl CandidateArtifactEntry {
    /// The cohort asset name of this entry.
    pub(crate) fn name(&self) -> &'static str {
        self.asset.artifact().name
    }

    /// Sorted, unique, non-empty gaps stated by this entry's companions.
    pub(crate) fn gaps(&self) -> &[String] {
        &self.gaps
    }

    /// Rehashes every companion and checks its role-derived safe name.
    fn check_companions(&self, names: &mut BTreeSet<String>) -> Result<(), Report<ReleaseError>> {
        let roles: Vec<CompanionRole> = self.companions.iter().map(|file| file.role).collect();
        if roles != CompanionRole::ALL {
            return Err(distribution_error(format!(
                "entry `{}` must carry exactly its four unsigned companions",
                self.name()
            )));
        }
        for file in &self.companions {
            if file.name != format!("{}{}", self.name(), file.role.suffix()) {
                return Err(distribution_error(format!(
                    "companion `{}` is not named for its asset and role",
                    file.name.escape_debug()
                )));
            }
            insert_name(names, &file.name)?;
            if file.size == 0
                || file.size > file.role.limit()
                || file.size != file.bytes.len() as u64
                || file.sha256 != sha256_hex(&file.bytes)
            {
                return Err(distribution_error(format!(
                    "companion `{}` bytes differ from its recorded size or digest",
                    file.name
                )));
            }
        }
        Ok(())
    }

    /// Renders this entry's index artifact object.
    fn index_entry(&self, inventory_sha256: &str) -> Value {
        let mut value = self.asset.artifact().index_identity();
        value["size"] = json!(self.artifact_size);
        value["sha256"] = json!(self.artifact_sha256);
        value["js_inventory_sha256"] = json!(inventory_sha256);
        let mut companions = serde_json::Map::new();
        for file in &self.companions {
            companions.insert(file.role.key().to_owned(), file.record());
        }
        for (key, suffix) in BUNDLE_SUFFIXES {
            let name = format!("{}{suffix}", self.name());
            companions.insert(key.to_owned(), BundleSlot::AwaitingSignature.record(&name));
        }
        value["companions"] = Value::Object(companions);
        value
    }
}

/// Inserts a safe flat name, rejecting case-insensitive duplicates.
fn insert_name(names: &mut BTreeSet<String>, name: &str) -> Result<(), Report<ReleaseError>> {
    if !is_safe_flat_name(name) {
        return Err(distribution_error(format!(
            "`{}` is not a safe flat release name",
            name.escape_debug()
        )));
    }
    if !names.insert(name.to_ascii_lowercase()) {
        return Err(distribution_error(format!(
            "`{name}` collides with another release name"
        )));
    }
    Ok(())
}

/// Validated unsigned candidate index bytes and every file they name.
///
/// It carries no signature, publication or qualification capability.
#[derive(Debug)]
pub(crate) struct CandidateIndex {
    bytes: Vec<u8>,
    sha256: String,
    inventory: Vec<u8>,
    companions: BTreeMap<String, Vec<u8>>,
    gaps: Vec<String>,
}

impl CandidateIndex {
    /// The fixed candidate index name; never `release-index.json`.
    pub(crate) fn name(&self) -> &'static str {
        CANDIDATE_INDEX_NAME
    }

    /// The exact canonical index bytes.
    pub(crate) fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// SHA-256 of the exact index bytes.
    pub(crate) fn sha256(&self) -> &str {
        &self.sha256
    }

    /// The exact shared JS inventory bytes the index names.
    pub(crate) fn inventory(&self) -> &[u8] {
        &self.inventory
    }

    /// The sixteen present unsigned companion files, by exact name.
    pub(crate) fn companions(&self) -> &BTreeMap<String, Vec<u8>> {
        &self.companions
    }

    /// Sorted, unique gaps of the index and all of its entries.
    pub(crate) fn gaps(&self) -> &[String] {
        &self.gaps
    }
}

/// Assembles the candidate index from exactly one entry per cohort asset.
///
/// Entries may arrive in any order; artifacts are emitted sorted by name, so
/// equal entries always render identical bytes.
///
/// # Errors
///
/// Rejects a missing, extra or duplicated asset, entries from different
/// source commits, producer policies or inventories, unsafe or colliding
/// names, companion bytes differing from their records, and any rendered
/// index that fails the retained schema or mirrored consumer checks.
pub(crate) fn assemble_candidate_index(
    entries: &[CandidateArtifactEntry],
) -> Result<CandidateIndex, Report<ReleaseError>> {
    let ordered = cohort_order(entries)?;
    let shared = check_shared_identity(&ordered)?;
    let inventory_sha256 = sha256_hex(shared.inventory);
    let mut names = BTreeSet::new();
    insert_name(&mut names, CANDIDATE_INDEX_NAME)?;
    insert_name(&mut names, INVENTORY_NAME)?;
    let mut artifacts = Vec::with_capacity(ordered.len());
    let mut companions = BTreeMap::new();
    let mut gaps: BTreeSet<String> = INDEX_GAPS.iter().map(|gap| (*gap).to_owned()).collect();
    for entry in &ordered {
        insert_name(&mut names, entry.name())?;
        entry.check_companions(&mut names)?;
        let artifact = entry.index_entry(&inventory_sha256);
        validate_release_index_artifact(&artifact)
            .map_err(|error| error.change_context(ReleaseError::Distribution))?;
        artifacts.push(artifact);
        for file in &entry.companions {
            companions.insert(file.name.clone(), file.bytes.clone());
        }
        gaps.extend(entry.gaps.iter().cloned());
    }
    let index = json!({
        "schema_version": 1,
        "repository": REPOSITORY,
        "tag": CANDIDATE_TAG,
        "source_commit": shared.source_commit,
        "recipe_source_commit": shared.source_commit,
        "cohort_source_commit": shared.source_commit,
        "cohort": COHORT_ID,
        "policy_version": shared.policy_version,
        "policy_sha256": shared.policy_sha256,
        "tsjs_inventory": {
            "name": INVENTORY_NAME,
            "size": shared.inventory.len(),
            "sha256": inventory_sha256,
        },
        "artifacts": artifacts,
    });
    let bytes = canonical_json(&index, DocumentRole::Metadata)
        .map_err(|error| error.change_context(ReleaseError::Distribution))?;
    check_rendered_index(&bytes)?;
    Ok(CandidateIndex {
        sha256: sha256_hex(&bytes),
        bytes,
        inventory: shared.inventory.to_vec(),
        companions,
        gaps: gaps.into_iter().collect(),
    })
}

/// Orders entries by asset name, requiring each fixed asset exactly once.
fn cohort_order(
    entries: &[CandidateArtifactEntry],
) -> Result<Vec<&CandidateArtifactEntry>, Report<ReleaseError>> {
    let mut ordered: BTreeMap<&str, &CandidateArtifactEntry> = BTreeMap::new();
    for entry in entries {
        if ordered.insert(entry.name(), entry).is_some() {
            return Err(distribution_error(format!(
                "asset `{}` appears more than once",
                entry.name()
            )));
        }
    }
    if let Some(missing) = FIXED_ARTIFACTS
        .iter()
        .find(|fixed| !ordered.contains_key(fixed.name))
    {
        return Err(distribution_error(format!(
            "candidate index is missing asset `{}`",
            missing.name
        )));
    }
    if ordered.len() != FIXED_ARTIFACTS.len() {
        return Err(distribution_error(
            "candidate index must list exactly the fixed cohort assets",
        ));
    }
    Ok(ordered.into_values().collect())
}

/// The one source, policy and inventory every entry must share.
struct SharedIdentity<'a> {
    source_commit: &'a str,
    policy_version: u64,
    policy_sha256: &'a str,
    inventory: &'a [u8],
}

fn check_shared_identity<'a>(
    ordered: &[&'a CandidateArtifactEntry],
) -> Result<SharedIdentity<'a>, Report<ReleaseError>> {
    let first = ordered
        .first()
        .ok_or_else(|| distribution_error("candidate index has no entries"))?;
    let shared = SharedIdentity {
        source_commit: &first.source_commit,
        policy_version: first.policy_version,
        policy_sha256: &first.policy_sha256,
        inventory: &first.inventory,
    };
    if !lower_hex(shared.source_commit, 40)
        || !lower_hex(shared.policy_sha256, 64)
        || shared.policy_version < 1
        || shared.inventory.is_empty()
    {
        return Err(distribution_error(
            "candidate source or producer policy identity is malformed",
        ));
    }
    for entry in ordered {
        if entry.source_commit != shared.source_commit {
            return Err(distribution_error(
                "candidate entries come from different source commits",
            ));
        }
        if entry.policy_version != shared.policy_version
            || entry.policy_sha256 != shared.policy_sha256
        {
            return Err(distribution_error(
                "candidate entries name different producer policies",
            ));
        }
        if entry.inventory.as_slice() != shared.inventory {
            return Err(distribution_error(
                "candidate entries name different JS inventories",
            ));
        }
    }
    Ok(shared)
}

/// Re-parses rendered bytes and repeats the consumer's structural and
/// semantic checks, including the candidate-only sentinels.
fn check_rendered_index(bytes: &[u8]) -> Result<(), Report<ReleaseError>> {
    let parsed = parse_json(bytes, DocumentRole::Metadata)
        .map_err(|error| error.change_context(ReleaseError::Distribution))?;
    validate_release_index(&parsed)
        .map_err(|error| error.change_context(ReleaseError::Distribution))?;
    let source = parsed["source_commit"].as_str().unwrap_or_default();
    if parsed["schema_version"] != 1
        || parsed["repository"] != REPOSITORY
        || parsed["cohort"] != COHORT_ID
        || parsed["tag"] != CANDIDATE_TAG
        || parsed["recipe_source_commit"] != source
        || parsed["cohort_source_commit"] != source
        || parsed["tsjs_inventory"]["name"] != INVENTORY_NAME
    {
        return Err(distribution_error(
            "rendered candidate index header differs from the candidate contract",
        ));
    }
    let artifacts = parsed["artifacts"]
        .as_array()
        .ok_or_else(|| distribution_error("rendered candidate index has no artifacts"))?;
    if artifacts.len() != FIXED_ARTIFACTS.len() {
        return Err(distribution_error(
            "rendered candidate index must list exactly the cohort artifacts",
        ));
    }
    artifacts
        .iter()
        .try_for_each(|artifact| check_rendered_artifact(artifact, &parsed["tsjs_inventory"]))
}

fn check_rendered_artifact(
    artifact: &Value,
    inventory: &Value,
) -> Result<(), Report<ReleaseError>> {
    let name = artifact["name"].as_str().unwrap_or_default();
    let fixed = FIXED_ARTIFACTS
        .iter()
        .find(|fixed| fixed.name == name)
        .ok_or_else(|| distribution_error(format!("unknown rendered asset `{name}`")))?;
    let identity = fixed.index_identity();
    let identity_fields = identity
        .as_object()
        .ok_or_else(|| distribution_error("fixed identity must be an object"))?;
    if identity_fields
        .iter()
        .any(|(key, value)| &artifact[key] != value)
    {
        return Err(distribution_error(format!(
            "rendered asset `{name}` differs from its fixed identity"
        )));
    }
    if artifact["js_inventory_sha256"] != inventory["sha256"] {
        return Err(distribution_error(format!(
            "rendered asset `{name}` names a different JS inventory"
        )));
    }
    let companions = &artifact["companions"];
    for role in CompanionRole::ALL {
        let record = &companions[role.key()];
        if record["name"] != format!("{name}{}", role.suffix())
            || record["size"].as_u64().is_none_or(|size| size == 0)
        {
            return Err(distribution_error(format!(
                "rendered asset `{name}` has a missing or empty {} companion",
                role.key()
            )));
        }
    }
    for (key, suffix) in BUNDLE_SUFFIXES {
        let expected = BundleSlot::AwaitingSignature.record(&format!("{name}{suffix}"));
        if companions[key] != expected {
            return Err(distribution_error(format!(
                "rendered asset `{name}` {key} bundle is not the awaiting-signature sentinel"
            )));
        }
    }
    Ok(())
}
