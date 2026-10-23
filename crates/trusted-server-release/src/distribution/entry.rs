//! One canonical candidate entry and its rendered unsigned companions.
//!
//! [`candidate_entry_from_completed`] is the only production constructor: it
//! binds a canonical [`RecipeCompleted`] to its approved source and its actual
//! runtime or startup acceptance, renders the candidate SPDX itself, and
//! renders build evidence, recipe and rebuild-check companions whose joins
//! are rechecked by the producer copy of the consumer rules. [`EntryParts`]
//! is a private plain struct so the pure renderer can be tested directly;
//! production has no byte or JSON constructor. The one exception is
//! test-only: `distribution::transfer` rebuilds an entry from the flat files
//! an ordinary-CI candidate job uploaded, reruns every join above over those
//! bytes and derives the policy identity from collector-read policy bytes.
//! It exists only under `cfg(test)` and grants no release authority.

use std::collections::{BTreeMap, BTreeSet};

use error_stack::Report;
use serde_json::{Value, json};

use super::joins::{
    EvidenceJoin, ModuleIdentity, check_build_evidence, check_comparison, check_spdx_root,
    inventory_modules, parse_informational,
};
use super::{CandidateArtifactEntry, CompanionFile, CompanionRole, distribution_error, sha256_hex};
use crate::documents::{Role as DocumentRole, canonical_json, lower_hex, parse_json};
use crate::error::ReleaseError;
use crate::policy::{CLI_POLICY_PATH, CLI_POLICY_VERSION, ReleaseTarget};
use crate::recipe::{
    AssetId, FastlyEqualityChecked, FastlyRuntimeAccepted, NativeStartupAccepted, RecipeCompleted,
    Role,
};
use crate::source::ApprovedSource;
use crate::spdx::candidate::candidate_from_completed;

/// Gaps every candidate entry states until signing and workflow binding exist.
pub(super) const ENTRY_GAPS: [&str; 4] = [
    "authenticated-bootstrap-not-established",
    "signature-bundles-awaiting-signature",
    "trusted-workflow-transfer-not-bound",
    "workflow-run-identity-unavailable",
];
/// Gap stated when no reproduction comparison exists for a native asset.
pub(super) const NATIVE_REPRODUCTION_GAP: &str = "native-reproduction-unavailable";
/// Longest accepted gap text.
pub(super) const MAX_GAP_LENGTH: usize = 256;
/// Final-byte classification of a JS module retained in the executable.
const EMBEDDED_CLASSIFICATION: &str = "conservative_embedded_js";
/// Final-byte classification of a JS module absent from the executable.
const MATERIAL_CLASSIFICATION: &str = "js_build_material";

/// The actual owning acceptance required for one canonical asset.
pub(crate) enum CandidateAcceptance<'a> {
    /// Final-byte Viceroy acceptance plus canonical/reproduction equality.
    Fastly {
        runtime: &'a FastlyRuntimeAccepted,
        equality: &'a FastlyEqualityChecked,
    },
    /// Actual startup of the exact copied native CLI.
    Native(&'a NativeStartupAccepted),
}

impl CandidateAcceptance<'_> {
    fn kind(&self) -> AcceptanceKind {
        match self {
            Self::Fastly { .. } => AcceptanceKind::FastlyRuntime,
            Self::Native(_) => AcceptanceKind::NativeStartup,
        }
    }
}

/// Which acceptance family an asset requires.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AcceptanceKind {
    /// The Fastly Wasm asset.
    FastlyRuntime,
    /// A native CLI asset.
    NativeStartup,
}

/// Checks that `completed` may supply a candidate entry under `kind`.
///
/// # Errors
///
/// Rejects a reproduction build, an approved source at another commit, or an
/// acceptance family that does not match the completed asset.
pub(crate) fn check_candidate_owner(
    completed: &RecipeCompleted,
    source: &ApprovedSource,
    kind: AcceptanceKind,
) -> Result<(), Report<ReleaseError>> {
    if completed.role() != Role::Canonical {
        return Err(distribution_error(
            "candidate index entries come only from the canonical build",
        ));
    }
    if source.commit() != completed.commit() {
        return Err(distribution_error(
            "approved source differs from the completed build commit",
        ));
    }
    let expected = if completed.asset().artifact().target == ReleaseTarget::Wasm32Wasip1 {
        AcceptanceKind::FastlyRuntime
    } else {
        AcceptanceKind::NativeStartup
    };
    if kind != expected {
        return Err(distribution_error(format!(
            "asset `{}` requires {expected:?} acceptance, not {kind:?}",
            completed.asset().artifact().name
        )));
    }
    Ok(())
}

/// Renders the candidate entry of one canonical completed build.
///
/// The candidate SPDX is rendered here from the same completed owner, and the
/// owner is rehashed again after rendering.
///
/// # Errors
///
/// Rejects any [`check_candidate_owner`] failure, an acceptance bound to
/// another source, role, asset or final bytes, missing composition or JS
/// evidence, a Fastly executable omitting a prepared module, an untracked
/// consumer policy, and every rendering or join failure.
pub(crate) fn candidate_entry_from_completed(
    completed: &RecipeCompleted,
    source: &ApprovedSource,
    acceptance: &CandidateAcceptance<'_>,
) -> Result<CandidateArtifactEntry, Report<ReleaseError>> {
    check_candidate_owner(completed, source, acceptance.kind())?;
    let (comparison, compatibility_tests) = bind_acceptance(completed, acceptance)?;
    let spdx = candidate_from_completed(completed, source)?;
    let view = completed.composition_view()?;
    let javascript = completed.javascript_contributions()?;
    let artifact = completed.artifact();
    let membership = view.javascript();
    if membership.artifact_sha256() != artifact.sha256()
        || membership.inventory_sha256() != javascript.inventory_sha256()
    {
        return Err(distribution_error(
            "JS membership belongs to another artifact or inventory",
        ));
    }
    let modules = membership
        .modules()
        .map(|(module, classification)| {
            let embedded = match classification {
                EMBEDDED_CLASSIFICATION => true,
                MATERIAL_CLASSIFICATION => false,
                other => {
                    return Err(distribution_error(format!(
                        "unknown JS module classification `{other}`"
                    )));
                }
            };
            Ok(ModuleObservation {
                id: module.id().to_owned(),
                filename: module.filename().to_owned(),
                size: module.size(),
                sha256: module.sha256().to_owned(),
                embedded,
            })
        })
        .collect::<Result<Vec<_>, Report<ReleaseError>>>()?;
    let policy_bytes = source
        .read_tracked(CLI_POLICY_PATH, DocumentRole::Metadata)
        .map_err(|error| error.change_context(ReleaseError::Distribution))?;
    let (policy_version, policy_sha256) = policy_identity(&policy_bytes)?;
    let definition = completed.recipe_definition()?;
    let parts = EntryParts {
        asset: completed.asset(),
        source_commit: completed.commit().as_str().to_owned(),
        policy_version,
        policy_sha256,
        artifact_sha256: artifact.sha256().to_owned(),
        artifact_size: artifact.size(),
        inventory: javascript.raw_inventory().to_vec(),
        modules,
        spdx: spdx.bytes().to_vec(),
        spdx_gaps: spdx.gaps().to_vec(),
        recipe: RecipeFacts {
            definition_sha256: definition.sha256().to_owned(),
            invocation_sha256: completed.plan_digest().to_owned(),
            cargo_lock_sha256: completed.cargo_lock_sha256().to_owned(),
            materials_digest: completed.materials_digest().to_owned(),
            rust_tools_digest: completed.rust_tools_digest().to_owned(),
        },
        comparison,
        compatibility_tests,
    };
    let entry = render_entry(parts)?;
    completed.rehash_for_handoff()?;
    Ok(entry)
}

/// Derives the producer policy identity from the exact approved policy bytes.
///
/// Mirrors the CLI's `producer_identity`: the version is the integer
/// `policy_version` (at least 1) of the strictly parsed bytes and the digest is
/// the SHA-256 of those same bytes. The version must also equal the compiled
/// [`CLI_POLICY_VERSION`].
///
/// # Errors
///
/// Rejects malformed JSON, a missing, zero or non-integer version, or a
/// version other than the compiled one.
pub(super) fn policy_identity(bytes: &[u8]) -> Result<(u64, String), Report<ReleaseError>> {
    let policy = parse_json(bytes, DocumentRole::Metadata)
        .map_err(|error| error.change_context(ReleaseError::Distribution))?;
    let version = policy["policy_version"]
        .as_u64()
        .filter(|version| *version >= 1)
        .ok_or_else(|| distribution_error("approved consumer policy has no integer version"))?;
    if version != CLI_POLICY_VERSION {
        return Err(distribution_error(format!(
            "approved consumer policy version {version} differs from compiled version {CLI_POLICY_VERSION}"
        )));
    }
    Ok((version, sha256_hex(bytes)))
}

/// Binds the acceptance to this exact completed build and derives its facts.
fn bind_acceptance(
    completed: &RecipeCompleted,
    acceptance: &CandidateAcceptance<'_>,
) -> Result<(RebuildComparison, Vec<CompatibilityTest>), Report<ReleaseError>> {
    let artifact = completed.artifact();
    match acceptance {
        CandidateAcceptance::Fastly { runtime, equality } => {
            let observed = runtime.observed();
            if equality.asset() != completed.asset()
                || equality.sha256() != artifact.sha256()
                || equality.size() != artifact.size()
                || runtime.source_commit() != completed.commit()
                || runtime.role() != completed.role()
                || observed.artifact_sha256() != artifact.sha256()
                || observed.artifact_size() != artifact.size()
            {
                return Err(distribution_error(
                    "Fastly acceptance belongs to another source, role or final output",
                ));
            }
            let facts = BTreeMap::from([
                ("viceroy_version", observed.viceroy_version().to_owned()),
                ("viceroy_sha256", observed.viceroy_sha256().to_owned()),
                ("fixture_sha256", observed.fixture_sha256().to_owned()),
                ("health_status", observed.health_status().to_string()),
            ]);
            Ok((
                RebuildComparison::Matched {
                    reproduction_sha256: equality.sha256().to_owned(),
                    reproduction_size: equality.size(),
                },
                vec![CompatibilityTest {
                    name: "fastly-final-byte-publisher-request",
                    facts,
                }],
            ))
        }
        CandidateAcceptance::Native(accepted) => {
            let diagnostic = accepted.diagnostic_for(completed)?;
            let kind = diagnostic["startup"]["kind"]
                .as_str()
                .filter(|_| diagnostic["release_qualification"] == false)
                .ok_or_else(|| distribution_error("native startup diagnostic is incomplete"))?;
            Ok((
                RebuildComparison::Unavailable,
                vec![CompatibilityTest {
                    name: "native-startup",
                    facts: BTreeMap::from([("startup_kind", kind.to_owned())]),
                }],
            ))
        }
    }
}

/// One prepared JS module and its final-byte membership.
#[derive(Clone, Debug)]
pub(super) struct ModuleObservation {
    pub(super) id: String,
    pub(super) filename: String,
    pub(super) size: u64,
    pub(super) sha256: String,
    pub(super) embedded: bool,
}

impl ModuleObservation {
    fn record(&self) -> Value {
        json!({"id": self.id, "filename": self.filename, "size": self.size, "sha256": self.sha256})
    }
}

/// Recipe identities recorded as authenticated informational data.
#[derive(Clone, Debug)]
pub(super) struct RecipeFacts {
    pub(super) definition_sha256: String,
    pub(super) invocation_sha256: String,
    pub(super) cargo_lock_sha256: String,
    pub(super) materials_digest: String,
    pub(super) rust_tools_digest: String,
}

/// The informational rebuild comparison of one entry.
#[derive(Clone, Debug)]
pub(super) enum RebuildComparison {
    /// Canonical and reproduction final bytes were checked identical.
    Matched {
        reproduction_sha256: String,
        reproduction_size: u64,
    },
    /// No reproduction comparison exists for this asset.
    Unavailable,
}

/// One passed compatibility test and its stable facts (no timings).
#[derive(Clone, Debug)]
pub(super) struct CompatibilityTest {
    pub(super) name: &'static str,
    pub(super) facts: BTreeMap<&'static str, String>,
}

/// Plain checked facts of one entry, before rendering.
#[derive(Clone, Debug)]
pub(super) struct EntryParts {
    pub(super) asset: AssetId,
    pub(super) source_commit: String,
    pub(super) policy_version: u64,
    pub(super) policy_sha256: String,
    pub(super) artifact_sha256: String,
    pub(super) artifact_size: u64,
    pub(super) inventory: Vec<u8>,
    pub(super) modules: Vec<ModuleObservation>,
    pub(super) spdx: Vec<u8>,
    pub(super) spdx_gaps: Vec<String>,
    pub(super) recipe: RecipeFacts,
    pub(super) comparison: RebuildComparison,
    pub(super) compatibility_tests: Vec<CompatibilityTest>,
}

impl EntryParts {
    fn is_fastly(&self) -> bool {
        self.asset.artifact().target == ReleaseTarget::Wasm32Wasip1
    }

    fn check_identity(&self) -> Result<(), Report<ReleaseError>> {
        if !lower_hex(&self.source_commit, 40)
            || !lower_hex(&self.policy_sha256, 64)
            || self.policy_version < 1
            || !lower_hex(&self.artifact_sha256, 64)
            || self.artifact_size == 0
            || self.artifact_size > DocumentRole::Executable.limit()
        {
            return Err(distribution_error(
                "candidate entry source, policy or artifact identity is malformed",
            ));
        }
        if self.compatibility_tests.is_empty() {
            return Err(distribution_error(
                "candidate entry needs its passed compatibility test",
            ));
        }
        Ok(())
    }

    /// Returns the outcome fields after checking the asset's comparison rule.
    fn comparison_fields(&self) -> Result<(&'static str, Value), Report<ReleaseError>> {
        match (&self.comparison, self.is_fastly()) {
            (
                RebuildComparison::Matched {
                    reproduction_sha256,
                    reproduction_size,
                },
                true,
            ) if *reproduction_sha256 == self.artifact_sha256
                && *reproduction_size == self.artifact_size =>
            {
                Ok(("matched", json!(reproduction_sha256)))
            }
            (RebuildComparison::Unavailable, false) => Ok(("unavailable", Value::Null)),
            _ => Err(distribution_error(
                "rebuild comparison must be the checked Fastly equality or unavailable for a native asset",
            )),
        }
    }

    /// Sorted unique gaps: fixed entry gaps, comparison gaps and SPDX gaps.
    fn gaps(&self) -> Result<Vec<String>, Report<ReleaseError>> {
        if self.spdx_gaps.is_empty() {
            return Err(distribution_error("candidate SPDX must state its gaps"));
        }
        let mut gaps: BTreeSet<String> = ENTRY_GAPS.iter().map(|gap| (*gap).to_owned()).collect();
        if matches!(self.comparison, RebuildComparison::Unavailable) {
            gaps.insert(NATIVE_REPRODUCTION_GAP.to_owned());
        }
        for gap in &self.spdx_gaps {
            if gap.is_empty()
                || gap.len() > MAX_GAP_LENGTH
                || gap
                    .chars()
                    .any(|character| character.is_control() || character.is_whitespace())
            {
                return Err(distribution_error("candidate gap text is malformed"));
            }
            gaps.insert(gap.clone());
        }
        Ok(gaps.into_iter().collect())
    }

    /// Sorted prepared modules and the embedded subset.
    fn module_records(&self) -> Result<(Vec<Value>, Vec<Value>), Report<ReleaseError>> {
        let mut modules: Vec<&ModuleObservation> = self.modules.iter().collect();
        modules.sort_by(|left, right| left.id.cmp(&right.id));
        if modules.windows(2).any(|pair| pair[0].id == pair[1].id) {
            return Err(distribution_error("prepared JS module ids must be unique"));
        }
        if self.is_fastly() && modules.iter().any(|module| !module.embedded) {
            return Err(distribution_error(
                "Fastly executable must embed every prepared JS module",
            ));
        }
        let prepared = modules.iter().map(|module| module.record()).collect();
        let embedded = modules
            .iter()
            .filter(|module| module.embedded)
            .map(|module| module.record())
            .collect();
        Ok((prepared, embedded))
    }
}

fn render(value: &Value) -> Result<Vec<u8>, Report<ReleaseError>> {
    canonical_json(value, DocumentRole::Companion)
        .map_err(|error| error.change_context(ReleaseError::Distribution))
}

/// Renders and joins one entry's unsigned companions from plain facts.
pub(super) fn render_entry(
    parts: EntryParts,
) -> Result<CandidateArtifactEntry, Report<ReleaseError>> {
    parts.check_identity()?;
    let gaps = parts.gaps()?;
    let (outcome, reproduction_sha256) = parts.comparison_fields()?;
    let (prepared, embedded) = parts.module_records()?;
    let artifact = parts.asset.artifact();
    let inventory_sha256 = sha256_hex(&parts.inventory);
    let compatibility: Vec<Value> = parts
        .compatibility_tests
        .iter()
        .map(|test| json!({"name": test.name, "outcome": "passed", "facts": test.facts}))
        .collect();
    let build_evidence = render(&json!({
        "domain": "trusted-server.release.candidate-build-evidence",
        "schema_version": 1,
        "release_qualification": false,
        "gaps": gaps,
        "source_commit": parts.source_commit,
        "asset": artifact.name,
        "target": artifact.target.triple(),
        "variant": "default",
        "artifact_sha256": parts.artifact_sha256,
        "artifact_size": parts.artifact_size,
        "js_inventory_sha256": inventory_sha256,
        "prepared_modules": prepared,
        "embedded_modules": embedded,
        "compatibility_tests": compatibility,
        "workflow_run_id": null,
        "transfer_artifact_id": null,
    }))?;
    let recipe = &parts.recipe;
    let build_recipe = render(&json!({
        "domain": "trusted-server.release.candidate-build-recipe",
        "version": 1,
        "release_qualification": false,
        "gaps": gaps,
        "source_commit": parts.source_commit,
        "asset": artifact.name,
        "package": artifact.package,
        "target": artifact.target.triple(),
        "variant": "default",
        "recipe_definition_sha256": recipe.definition_sha256,
        "invocation_sha256": recipe.invocation_sha256,
        "inputs": {
            "cargo_lock_sha256": recipe.cargo_lock_sha256,
            "materials_digest": recipe.materials_digest,
            "rust_tools_digest": recipe.rust_tools_digest,
        },
        "workflow_run_id": null,
    }))?;
    let rebuild_check = render(&json!({
        "domain": "trusted-server.release.candidate-rebuild-check",
        "version": 1,
        "release_qualification": false,
        "gaps": gaps,
        "source_commit": parts.source_commit,
        "asset": artifact.name,
        "outcome": outcome,
        "canonical_sha256": parts.artifact_sha256,
        "reproduction_sha256": reproduction_sha256,
        "workflow_run_id": null,
        "reproduction_transfer_artifact_id": null,
    }))?;
    let modules: Vec<ModuleIdentity> = inventory_modules(&parts.inventory, &parts.source_commit)?;
    check_build_evidence(
        &build_evidence,
        &EvidenceJoin {
            source_commit: &parts.source_commit,
            target: artifact.target.triple(),
            artifact_sha256: &parts.artifact_sha256,
            inventory_sha256: &inventory_sha256,
            modules: &modules,
        },
    )?;
    check_spdx_root(&parts.spdx, &parts.artifact_sha256)?;
    parse_informational(&build_recipe, &parts.source_commit)?;
    check_comparison(&rebuild_check, &parts.source_commit)?;
    let companions = vec![
        CompanionFile::new(CompanionRole::Sbom, artifact.name, parts.spdx),
        CompanionFile::new(CompanionRole::BuildEvidence, artifact.name, build_evidence),
        CompanionFile::new(CompanionRole::BuildRecipe, artifact.name, build_recipe),
        CompanionFile::new(CompanionRole::RebuildCheck, artifact.name, rebuild_check),
    ];
    Ok(CandidateArtifactEntry {
        asset: parts.asset,
        source_commit: parts.source_commit,
        policy_version: parts.policy_version,
        policy_sha256: parts.policy_sha256,
        inventory: parts.inventory,
        artifact_size: parts.artifact_size,
        artifact_sha256: parts.artifact_sha256,
        companions,
        gaps,
    })
}
