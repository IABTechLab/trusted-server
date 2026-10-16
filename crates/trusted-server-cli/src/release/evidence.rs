//! R-EVIDENCE: the complete verified evidence graph of an official release.
//!
//! Every proof is the single result object of one successful pinned
//! `gh attestation verify --bundle ... --format json` run (see
//! [`super::gh::GhRunner::verify_attestation`]). Primitive gh success is
//! never sufficient: every certificate field, statement subject and predicate
//! edge below is checked again here, and any missing, extra or mismatched
//! edge fails closed.
//!
//! JSON shape of the pinned gh 2.97.0 output, confirmed from primary sources:
//! - Each array element is `AttestationProcessingResult` with `attestation`
//!   and `verificationResult`:
//!   <https://github.com/cli/cli/blob/v2.97.0/pkg/cmd/attestation/verification/sigstore.go>
//!   and the `--format json` help text in
//!   <https://github.com/cli/cli/blob/v2.97.0/pkg/cmd/attestation/verify/verify.go>.
//! - `verificationResult` is sigstore-go v1.2.2 (pinned in gh's `go.mod`)
//!   `VerificationResult`: `mediaType`, `statement` (protojson in-toto v1
//!   Statement: `_type`, `subject[].name`, `subject[].digest`,
//!   `predicateType`, `predicate`) and `signature.certificate`, a
//!   `certificate.Summary` with Fulcio `Extensions` flattened into it:
//!   <https://github.com/sigstore/sigstore-go/blob/v1.2.2/pkg/verify/signed_entity.go>,
//!   <https://github.com/sigstore/sigstore-go/blob/v1.2.2/pkg/fulcio/certificate/summarize.go>,
//!   <https://github.com/sigstore/sigstore-go/blob/v1.2.2/pkg/fulcio/certificate/extensions.go>,
//!   <https://github.com/in-toto/attestation/blob/v1.2.0/go/v1/statement.pb.go>.
//! - The GitHub workflow provenance predicate layout follows the
//!   `@actions/attest` builder:
//!   <https://github.com/actions/toolkit/blob/main/packages/attest/src/provenance.ts>.
//!   Real attest 4.2.2 provenance/SPDX fixtures were verified with gh 2.97.0;
//!   the measured consumer qualification report records their identities,
//!   digests and replay commands. Official project first-use remains separate.

mod joins;

#[cfg(test)]
mod upstream_format_tests;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use error_stack::Report;
use regex::Regex;
use serde_json::{Map, Value, json};

use super::artifact::AssetMeasurement;
use super::documents::{
    ApprovedRelease, FileRecord, IndexArtifact, ReleaseIndex, lower_hex, parse_release_index,
};
use super::error::{ReleaseError, report};
use super::gh::{PredicateKind, VerificationMode, VerifiedAttestation};
use super::policy::{
    AcceptanceRules, AcceptedProducerPolicy, BUILD_TYPE, ConsumerPolicy, GITHUB_HOST, ISSUER,
    PROVENANCE_PREDICATE, REPOSITORY, REPOSITORY_ID, REPOSITORY_OWNER_ID, SBOM_PREDICATE,
    accept_producer_policy, certificate_identity,
};
use super::source_policy::ApprovedSourcePolicy;

pub(crate) use joins::ComparisonOutcome;

/// sigstore-go `VerificationResultMediaType01`.
const VERIFICATION_MEDIA_TYPE: &str =
    "application/vnd.dev.sigstore.verificationresult+json;version=0.1";
/// in-toto v1 statement type.
const STATEMENT_TYPE: &str = "https://in-toto.io/Statement/v1";
/// Fixed name of the signed release index.
pub(crate) const INDEX_NAME: &str = "release-index.json";
/// Caller workflow path inside the official repository.
const CALLER_PATH: &str = ".github/workflows/release.yml";
const CERTIFICATE: [&str; 3] = ["verificationResult", "signature", "certificate"];
const STATEMENT: [&str; 2] = ["verificationResult", "statement"];

static RUN_URI: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^https://github\.com/IABTechLab/trusted-server/actions/runs/[1-9][0-9]{0,19}/attempts/[1-9][0-9]{0,4}$", // allow-domain: github
    )
    .expect("should compile the reviewed run invocation pattern")
});

/// Identity values derived only from the independently approved release.
struct Expected {
    san: String,
    caller: String,
    source_ref: String,
    commit: String,
    repository_uri: String,
}

impl Expected {
    fn new(release: &ApprovedRelease) -> Self {
        let repository_uri = format!("https://{GITHUB_HOST}/{REPOSITORY}");
        let source_ref = format!("refs/tags/{}", release.tag().as_str());
        Self {
            san: certificate_identity(release.tag()),
            caller: format!("{repository_uri}/{CALLER_PATH}@{source_ref}"),
            source_ref,
            commit: release.commit().as_str().to_owned(),
            repository_uri,
        }
    }
}

pub(super) fn at<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    path.iter().try_fold(value, |current, key| current.get(key))
}

pub(super) fn evidence_error(message: impl std::fmt::Display) -> Report<ReleaseError> {
    report(ReleaseError::Evidence, message)
}

fn require_text<'a>(value: &'a Value, path: &[&str]) -> Result<&'a str, Report<ReleaseError>> {
    at(value, path)
        .and_then(Value::as_str)
        .ok_or_else(|| evidence_error(format!("proof field `{}` is missing", path.join("."))))
}

/// Checks the verified certificate of one proof and returns its run URI.
fn check_certificate(proof: &Value, expected: &Expected) -> Result<String, Report<ReleaseError>> {
    let certificate = at(proof, &CERTIFICATE)
        .ok_or_else(|| evidence_error("proof has no verified certificate"))?;
    let fields = [
        ("subjectAlternativeName", expected.san.as_str()),
        ("issuer", ISSUER),
        ("buildSignerURI", expected.san.as_str()),
        ("buildSignerDigest", expected.commit.as_str()),
        ("sourceRepositoryURI", expected.repository_uri.as_str()),
        ("sourceRepositoryDigest", expected.commit.as_str()),
        ("sourceRepositoryRef", expected.source_ref.as_str()),
        ("sourceRepositoryIdentifier", REPOSITORY_ID),
        ("sourceRepositoryOwnerIdentifier", REPOSITORY_OWNER_ID),
        ("buildConfigURI", expected.caller.as_str()),
        ("buildConfigDigest", expected.commit.as_str()),
        ("buildTrigger", "push"),
        ("runnerEnvironment", "github-hosted"),
    ];
    for (field, value) in fields {
        if require_text(certificate, &[field])? != value {
            return Err(evidence_error(format!(
                "certificate `{field}` differs from the approved release identity"
            )));
        }
    }
    let run = require_text(certificate, &["runInvocationURI"])?;
    if !RUN_URI.is_match(run) {
        return Err(evidence_error(
            "certificate `runInvocationURI` is not an official repository run attempt",
        ));
    }
    Ok(run.to_owned())
}

/// Returns the verified statement after checking its envelope types.
fn statement<'a>(
    proof: &'a Value,
    predicate_type: &str,
) -> Result<&'a Value, Report<ReleaseError>> {
    if require_text(proof, &["verificationResult", "mediaType"])? != VERIFICATION_MEDIA_TYPE {
        return Err(evidence_error("unsupported verification result media type"));
    }
    let statement =
        at(proof, &STATEMENT).ok_or_else(|| evidence_error("proof has no verified statement"))?;
    if require_text(statement, &["_type"])? != STATEMENT_TYPE {
        return Err(evidence_error("statement is not an in-toto v1 statement"));
    }
    if require_text(statement, &["predicateType"])? != predicate_type {
        return Err(evidence_error(format!(
            "statement predicate type is not `{predicate_type}`"
        )));
    }
    Ok(statement)
}

fn subject_entry(subject: &Value) -> Option<(&str, &str)> {
    let object = subject.as_object()?;
    let digest = object.get("digest")?.as_object()?;
    let name = object.get("name")?.as_str()?;
    let sha256 = digest.get("sha256")?.as_str()?;
    (object.len() == 2 && digest.len() == 1 && lower_hex(sha256, 64)).then_some((name, sha256))
}

/// Requires the statement subjects to be exactly `expected` (name → SHA-256).
fn check_subjects(
    statement: &Value,
    expected: &BTreeMap<&str, &str>,
) -> Result<(), Report<ReleaseError>> {
    let subjects = at(statement, &["subject"])
        .and_then(Value::as_array)
        .ok_or_else(|| evidence_error("statement has no subject array"))?;
    let mut signed = BTreeMap::new();
    for subject in subjects {
        let (name, sha256) = subject_entry(subject)
            .ok_or_else(|| evidence_error("statement subject is not exactly {name, sha256}"))?;
        if signed.insert(name, sha256).is_some() {
            return Err(evidence_error(format!(
                "statement names subject `{name}` twice"
            )));
        }
    }
    if &signed != expected {
        return Err(evidence_error(
            "signed subjects differ from the exact expected names and digests",
        ));
    }
    Ok(())
}

fn expected_provenance(expected: &Expected, run: &str) -> Value {
    json!({
        "buildDefinition": {
            "buildType": BUILD_TYPE,
            "externalParameters": {
                "workflow": {
                    "ref": expected.source_ref,
                    "repository": expected.repository_uri,
                    "path": CALLER_PATH,
                }
            },
            "internalParameters": {
                "github": {
                    "event_name": "push",
                    "repository_id": REPOSITORY_ID,
                    "repository_owner_id": REPOSITORY_OWNER_ID,
                    "runner_environment": "github-hosted",
                }
            },
            "resolvedDependencies": [{
                "uri": format!("git+{}@{}", expected.repository_uri, expected.source_ref),
                "digest": {"gitCommit": expected.commit},
            }],
        },
        "runDetails": {
            "builder": {"id": expected.san},
            "metadata": {"invocationId": run},
        },
    })
}

/// Checks the SLSA predicate section by section, then exactly.
fn check_provenance_predicate(
    statement: &Value,
    expected: &Expected,
    run: &str,
) -> Result<(), Report<ReleaseError>> {
    let wanted = expected_provenance(expected, run);
    let actual = at(statement, &["predicate"])
        .ok_or_else(|| evidence_error("provenance statement has no predicate"))?;
    let sections: [&[&str]; 6] = [
        &["buildDefinition", "buildType"],
        &["buildDefinition", "externalParameters"],
        &["buildDefinition", "internalParameters"],
        &["buildDefinition", "resolvedDependencies"],
        &["runDetails", "builder"],
        &["runDetails", "metadata"],
    ];
    for section in sections {
        if at(actual, section) != at(&wanted, section) {
            return Err(evidence_error(format!(
                "provenance `{}` differs from the approved caller, source or run",
                section.join(".")
            )));
        }
    }
    if actual != &wanted {
        return Err(evidence_error(
            "provenance predicate contains unexpected structures",
        ));
    }
    Ok(())
}

/// Verifies one provenance proof and returns its certificate run URI.
fn check_provenance_proof(
    proof: &Value,
    expected: &Expected,
    subjects: &BTreeMap<&str, &str>,
) -> Result<String, Report<ReleaseError>> {
    let run = check_certificate(proof, expected)?;
    let statement = statement(proof, PROVENANCE_PREDICATE)?;
    check_provenance_predicate(statement, expected, &run)?;
    check_subjects(statement, subjects)?;
    Ok(run)
}

fn require_same_run(run: &str, other: &str) -> Result<(), Report<ReleaseError>> {
    if run != other {
        return Err(evidence_error(
            "collected proofs come from different workflow runs or attempts",
        ));
    }
    Ok(())
}

fn check_measurement(
    record: &FileRecord,
    actual: &AssetMeasurement,
) -> Result<(), Report<ReleaseError>> {
    if record.size != actual.length() || record.sha256 != actual.sha256() {
        return Err(evidence_error(format!(
            "`{}` bytes differ from the signed index length or digest",
            record.name
        )));
    }
    Ok(())
}

/// Retained bytes and verified proofs for one consumed cohort artifact.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ConsumedArtifact<'a> {
    pub(crate) name: &'a str,
    pub(crate) executable: &'a AssetMeasurement,
    pub(crate) spdx: &'a [u8],
    pub(crate) build_evidence: &'a [u8],
    pub(crate) build_recipe: &'a [u8],
    pub(crate) rebuild_check: &'a [u8],
    pub(crate) provenance_bundle: &'a AssetMeasurement,
    pub(crate) sbom_bundle: &'a AssetMeasurement,
    pub(crate) provenance_proof: &'a VerifiedAttestation,
    pub(crate) spdx_proof: &'a VerifiedAttestation,
}

/// Every independently obtained trust input and downloaded evidence item.
#[derive(Clone, Copy, Debug)]
pub(crate) struct EvidenceInputs<'a> {
    pub(crate) release: &'a ApprovedRelease,
    pub(crate) consumer_policy: &'a ConsumerPolicy,
    pub(crate) source_policy: &'a ApprovedSourcePolicy,
    pub(crate) index_bytes: &'a [u8],
    pub(crate) index_proof: &'a VerifiedAttestation,
    pub(crate) index_bundle: &'a AssetMeasurement,
    pub(crate) inventory_bytes: &'a [u8],
    pub(crate) artifacts: &'a [ConsumedArtifact<'a>],
}

/// The verified evidence of one consumed artifact.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct VerifiedArtifactEvidence {
    name: String,
    measurement: AssetMeasurement,
    assurance_profile: String,
    comparison: ComparisonOutcome,
}

impl VerifiedArtifactEvidence {
    /// Returns the asset name.
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// Returns the verified executable digest and length.
    pub(crate) fn measurement(&self) -> &AssetMeasurement {
        &self.measurement
    }

    /// Returns the required assurance profile of this artifact.
    pub(crate) fn assurance_profile(&self) -> &str {
        &self.assurance_profile
    }

    /// Returns the informational rebuild comparison outcome.
    pub(crate) fn comparison(&self) -> ComparisonOutcome {
        self.comparison
    }
}

/// Every R-EVIDENCE edge held for the consumed artifacts.
///
/// Only [`verify_build_evidence`] constructs it. It proves build evidence,
/// not official publication; see [`super::github::verify_publication`].
#[derive(Debug)]
pub(crate) struct BuildEvidenceVerified {
    mode: VerificationMode,
    release: ApprovedRelease,
    index: ReleaseIndex,
    index_measurement: AssetMeasurement,
    index_bundle: AssetMeasurement,
    consumer_policy: ConsumerPolicy,
    producer_policy: AcceptedProducerPolicy,
    run_invocation: String,
    consumed: Vec<VerifiedArtifactEvidence>,
}

impl BuildEvidenceVerified {
    /// Returns the trust context shared by every build proof.
    pub(crate) fn mode(&self) -> VerificationMode {
        self.mode
    }
    /// Returns the approved release.
    pub(crate) fn release(&self) -> &ApprovedRelease {
        &self.release
    }

    /// Returns the provenance-verified index.
    pub(crate) fn index(&self) -> &ReleaseIndex {
        &self.index
    }

    /// Returns the exact index bytes' digest and length.
    pub(crate) fn index_measurement(&self) -> &AssetMeasurement {
        &self.index_measurement
    }

    /// Returns the exact provenance bundle verified for this index.
    pub(crate) fn index_bundle(&self) -> &AssetMeasurement {
        &self.index_bundle
    }

    /// Returns the consumer policy version.
    pub(crate) fn consumer_policy_version(&self) -> u64 {
        self.consumer_policy.version()
    }

    /// Returns the consumer policy exact-byte digest.
    pub(crate) fn consumer_policy_sha256(&self) -> &str {
        self.consumer_policy.sha256()
    }

    /// Returns how the producer policy was accepted.
    pub(crate) fn producer_policy(&self) -> &AcceptedProducerPolicy {
        &self.producer_policy
    }

    /// Returns the single workflow run attempt shared by every proof.
    pub(crate) fn run_invocation(&self) -> &str {
        &self.run_invocation
    }

    /// Returns the verified consumed artifacts.
    pub(crate) fn consumed(&self) -> &[VerifiedArtifactEvidence] {
        &self.consumed
    }
}

/// Verifies index provenance and approved-source policy before using its fields.
///
/// # Errors
///
/// Returns evidence, document or policy errors for any invalid proof or binding.
pub(crate) fn verify_index(
    release: &ApprovedRelease,
    policy: &ConsumerPolicy,
    source: &ApprovedSourcePolicy,
    bytes: &[u8],
    proof: &VerifiedAttestation,
    bundle: &AssetMeasurement,
) -> Result<ReleaseIndex, Report<ReleaseError>> {
    let measurement = AssetMeasurement::of_bytes(bytes);
    let subjects = BTreeMap::from([(INDEX_NAME, measurement.sha256())]);
    let claims = proof.for_inputs(release, PredicateKind::Provenance, &measurement, bundle)?;
    check_provenance_proof(claims, &Expected::new(release), &subjects)?;
    let index = parse_release_index(bytes, policy, release)?;
    accept_producer_policy(&index, source, &AcceptanceRules::compiled(policy))?;
    Ok(index)
}

/// Verifies the complete evidence graph for the consumed artifacts.
///
/// Index provenance is verified against the approved release before any
/// index field is trusted; the index is then parsed under the embedded
/// policy and its producer-policy identity bound to approved-source bytes.
///
/// # Errors
///
/// Returns [`ReleaseError::Evidence`] for any missing, extra or mismatched
/// edge, and document or policy errors from index parsing and producer-policy
/// acceptance.
pub(crate) fn verify_build_evidence(
    inputs: &EvidenceInputs<'_>,
) -> Result<BuildEvidenceVerified, Report<ReleaseError>> {
    check_consumed_names(inputs.artifacts)?;
    let mode = inputs.index_proof.mode();
    if inputs.artifacts.iter().any(|artifact| {
        artifact.provenance_proof.mode() != mode || artifact.spdx_proof.mode() != mode
    }) {
        return Err(evidence_error(
            "build proofs have different online/offline trust contexts",
        ));
    }

    let expected = Expected::new(inputs.release);
    let index_measurement = AssetMeasurement::of_bytes(inputs.index_bytes);
    let index_subject = BTreeMap::from([(INDEX_NAME, index_measurement.sha256())]);
    let index_proof = inputs.index_proof.for_inputs(
        inputs.release,
        PredicateKind::Provenance,
        &index_measurement,
        inputs.index_bundle,
    )?;
    let run = check_provenance_proof(index_proof, &expected, &index_subject)?;
    let index = parse_release_index(inputs.index_bytes, inputs.consumer_policy, inputs.release)?;
    let producer_policy = accept_producer_policy(
        &index,
        inputs.source_policy,
        &AcceptanceRules::compiled(inputs.consumer_policy),
    )?;
    check_measurement(
        &index.tsjs_inventory,
        &AssetMeasurement::of_bytes(inputs.inventory_bytes),
    )?;
    let modules = joins::parse_inventory(inputs.inventory_bytes, &expected.commit)?;
    let context = ArtifactContext {
        index: &index,
        expected: &expected,
        run: &run,
        modules: &modules,
        release: inputs.release,
    };
    let consumed = inputs
        .artifacts
        .iter()
        .map(|artifact| context.verify(artifact))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(BuildEvidenceVerified {
        mode,
        release: inputs.release.clone(),
        index,
        index_measurement,
        index_bundle: inputs.index_bundle.clone(),
        consumer_policy: inputs.consumer_policy.clone(),
        producer_policy,
        run_invocation: run,
        consumed,
    })
}

fn check_consumed_names(artifacts: &[ConsumedArtifact<'_>]) -> Result<(), Report<ReleaseError>> {
    let names: BTreeSet<&str> = artifacts.iter().map(|artifact| artifact.name).collect();
    if artifacts.is_empty() || names.len() != artifacts.len() {
        return Err(evidence_error(
            "verification needs one or more distinct consumed artifacts",
        ));
    }
    Ok(())
}

struct ArtifactContext<'a> {
    index: &'a ReleaseIndex,
    expected: &'a Expected,
    run: &'a str,
    modules: &'a [joins::ModuleRecord],
    release: &'a ApprovedRelease,
}

impl ArtifactContext<'_> {
    fn verify(
        &self,
        consumed: &ConsumedArtifact<'_>,
    ) -> Result<VerifiedArtifactEvidence, Report<ReleaseError>> {
        let entry = self
            .index
            .artifacts
            .iter()
            .find(|entry| entry.name == consumed.name)
            .ok_or_else(|| {
                evidence_error(format!(
                    "`{}` is not a signed index artifact",
                    consumed.name
                ))
            })?;
        check_companion_bytes(entry, consumed)?;
        joins::parse_informational(consumed.build_recipe)?;
        let provenance_subjects = provenance_subjects(entry);
        let proof = consumed.provenance_proof.for_inputs(
            self.release,
            PredicateKind::Provenance,
            consumed.executable,
            consumed.provenance_bundle,
        )?;
        let run = check_provenance_proof(proof, self.expected, &provenance_subjects)?;
        require_same_run(self.run, &run)?;
        self.check_sbom_proof(entry, consumed)?;
        joins::check_build_evidence(consumed.build_evidence, entry, self.index, self.modules)?;
        Ok(VerifiedArtifactEvidence {
            name: entry.name.clone(),
            measurement: consumed.executable.clone(),
            assurance_profile: entry.assurance_profile.clone(),
            comparison: joins::parse_comparison(consumed.rebuild_check)?,
        })
    }

    fn check_sbom_proof(
        &self,
        entry: &IndexArtifact,
        consumed: &ConsumedArtifact<'_>,
    ) -> Result<(), Report<ReleaseError>> {
        let proof = consumed.spdx_proof.for_inputs(
            self.release,
            PredicateKind::Spdx,
            consumed.executable,
            consumed.sbom_bundle,
        )?;
        let run = check_certificate(proof, self.expected)?;
        require_same_run(self.run, &run)?;
        let statement = statement(proof, SBOM_PREDICATE)?;
        check_subjects(
            statement,
            &BTreeMap::from([(entry.name.as_str(), entry.sha256.as_str())]),
        )?;
        let predicate = at(statement, &["predicate"])
            .ok_or_else(|| evidence_error("SPDX statement has no predicate"))?;
        joins::check_spdx(predicate, consumed.spdx, &entry.sha256)
    }
}

/// Index → executable and companions: exact actual lengths and digests.
fn check_companion_bytes(
    entry: &IndexArtifact,
    consumed: &ConsumedArtifact<'_>,
) -> Result<(), Report<ReleaseError>> {
    let executable = FileRecord {
        name: entry.name.clone(),
        size: entry.size,
        sha256: entry.sha256.clone(),
    };
    let companions = &entry.companions;
    let pairs = [
        (&executable, consumed.executable.clone()),
        (&companions.sbom, AssetMeasurement::of_bytes(consumed.spdx)),
        (
            &companions.build_evidence,
            AssetMeasurement::of_bytes(consumed.build_evidence),
        ),
        (
            &companions.build_recipe,
            AssetMeasurement::of_bytes(consumed.build_recipe),
        ),
        (
            &companions.rebuild_check,
            AssetMeasurement::of_bytes(consumed.rebuild_check),
        ),
        (&companions.provenance, consumed.provenance_bundle.clone()),
        (&companions.sbom_attestation, consumed.sbom_bundle.clone()),
    ];
    pairs
        .iter()
        .try_for_each(|(record, actual)| check_measurement(record, actual))
}

/// Artifact provenance subjects: executable, SBOM, build evidence, recipe and
/// rebuild check, exactly.
fn provenance_subjects(entry: &IndexArtifact) -> BTreeMap<&str, &str> {
    let companions = &entry.companions;
    [
        (entry.name.as_str(), entry.sha256.as_str()),
        (
            companions.sbom.name.as_str(),
            companions.sbom.sha256.as_str(),
        ),
        (
            companions.build_evidence.name.as_str(),
            companions.build_evidence.sha256.as_str(),
        ),
        (
            companions.build_recipe.name.as_str(),
            companions.build_recipe.sha256.as_str(),
        ),
        (
            companions.rebuild_check.name.as_str(),
            companions.rebuild_check.sha256.as_str(),
        ),
    ]
    .into_iter()
    .collect()
}

/// Returns the object at `value`, or an evidence error naming `what`.
pub(super) fn object<'a>(
    value: &'a Value,
    what: &str,
) -> Result<&'a Map<String, Value>, Report<ReleaseError>> {
    value
        .as_object()
        .ok_or_else(|| evidence_error(format!("{what} must be a JSON object")))
}
