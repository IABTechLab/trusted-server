//! Embedded consumer release policy and producer-policy acceptance.
//!
//! The reviewed `release-policy.json` bytes are compiled in; their SHA-256 is
//! computed at runtime and never stored inside the policy, avoiding a
//! self-hash cycle. Parsing is closed: every field must equal the compiled
//! constants below, so neither a downloaded index nor operator input can
//! redefine repository identity, signer identity, supported combinations,
//! tool pins or resource bounds.

use std::collections::BTreeSet;

use error_stack::Report;
use serde::Deserialize;
use serde_json::{Value, json};

use super::bounds;
use super::documents::{
    Compatibility, ReleaseIndex, ReleaseTag, Role, lower_hex, parse_json, sha256_hex,
};
use super::error::{ReleaseError, report};
use super::source_policy::ApprovedSourcePolicy;

/// Exact reviewed consumer policy bytes.
pub(crate) const EMBEDDED_POLICY: &[u8] = include_bytes!("../../release-policy.json");
/// Policy version this consumer was compiled with.
pub(crate) const CONSUMER_POLICY_VERSION: u64 = 1;
/// Official repository slug.
pub(crate) const REPOSITORY: &str = "IABTechLab/trusted-server";
/// GitHub host serving the official repository.
pub(crate) const GITHUB_HOST: &str = "github.com";
/// Numeric repository identifier.
pub(crate) const REPOSITORY_ID: &str = "950860906";
/// Numeric repository owner identifier.
pub(crate) const REPOSITORY_OWNER_ID: &str = "82903538";
/// Fixed phase-one cohort.
pub(crate) const COHORT: &str = "phase1-fastly-cli";
/// Sigstore certificate OIDC issuer.
pub(crate) const ISSUER: &str = "https://token.actions.githubusercontent.com";
/// Certificate SAN and builder ID before the release tag.
pub(crate) const SIGNER_WORKFLOW_PREFIX: &str =
    "https://github.com/IABTechLab/trusted-server/.github/workflows/release-build.yml@refs/tags/";
/// Exactly qualified GitHub CLI version.
pub(crate) const GH_VERSION: &str = "2.97.0";
/// Pinned `actions/attest` version.
pub(crate) const ATTEST_VERSION: &str = "4.2.2";
/// Pinned `actions/attest` commit.
pub(crate) const ATTEST_REVISION: &str = "1e69f48acb82d1966a394da916b4c1698aa569d6";
/// SLSA provenance predicate type.
pub(crate) const PROVENANCE_PREDICATE: &str = "https://slsa.dev/provenance/v1";
/// SPDX SBOM predicate type.
pub(crate) const SBOM_PREDICATE: &str = "https://spdx.dev/Document/v2.3";
/// GitHub Actions workflow build type.
pub(crate) const BUILD_TYPE: &str = "https://actions.github.io/buildtypes/workflow/v1";
/// The only action any artifact currently authorizes.
pub(crate) const READONLY_VERIFY: &str = "readonly-verify";
/// Capability that the v1 schema allows but current policy forbids.
pub(crate) const STAGING_CAPABILITY: &str = "fastly-staging";
/// Every capability the v1 schema can express.
pub(crate) const KNOWN_CAPABILITIES: [&str; 3] =
    ["fastly-production", STAGING_CAPABILITY, "cli-execute"];
const KNOWN_KINDS: [&str; 2] = ["wasm-core", "native-executable"];
const KNOWN_PROFILES: [&str; 2] = ["provenance-sbom-repeatable", "provenance-sbom"];
const KNOWN_ACTIONS: [&str; 1] = [READONLY_VERIFY];

/// One source-owned cohort identity; policy JSON can only restate it.
#[derive(Debug)]
pub(crate) struct FixedArtifact {
    pub(crate) name: &'static str,
    pub(crate) package: &'static str,
    pub(crate) bin: &'static str,
    pub(crate) kind: &'static str,
    pub(crate) role: &'static str,
    pub(crate) assurance_profile: &'static str,
    pub(crate) capabilities: &'static [&'static str],
    pub(crate) target: &'static str,
    pub(crate) runtime: &'static str,
    pub(crate) minimum_version: &'static str,
    pub(crate) architecture: &'static str,
}

/// The complete phase-one cohort, in policy order.
pub(crate) const FIXED_ARTIFACTS: [FixedArtifact; bounds::COHORT_EXECUTABLES] = [
    FixedArtifact {
        name: "trusted-server-fastly.wasm",
        package: "trusted-server-adapter-fastly",
        bin: "trusted-server-adapter-fastly",
        kind: "wasm-core",
        role: "server",
        assurance_profile: "provenance-sbom-repeatable",
        capabilities: &["fastly-production"],
        target: "wasm32-wasip1",
        runtime: "fastly-compute",
        minimum_version: "wasip1",
        architecture: "wasm32",
    },
    FixedArtifact {
        name: "ts-aarch64-apple-darwin",
        package: "trusted-server-cli",
        bin: "ts",
        kind: "native-executable",
        role: "operator-cli",
        assurance_profile: "provenance-sbom",
        capabilities: &["cli-execute"],
        target: "aarch64-apple-darwin",
        runtime: "macos",
        minimum_version: "13.0",
        architecture: "aarch64",
    },
    FixedArtifact {
        name: "ts-x86_64-apple-darwin",
        package: "trusted-server-cli",
        bin: "ts",
        kind: "native-executable",
        role: "operator-cli",
        assurance_profile: "provenance-sbom",
        capabilities: &["cli-execute"],
        target: "x86_64-apple-darwin",
        runtime: "macos",
        minimum_version: "13.0",
        architecture: "x86_64",
    },
    FixedArtifact {
        name: "ts-x86_64-unknown-linux-gnu",
        package: "trusted-server-cli",
        bin: "ts",
        kind: "native-executable",
        role: "operator-cli",
        assurance_profile: "provenance-sbom",
        capabilities: &["cli-execute"],
        target: "x86_64-unknown-linux-gnu",
        runtime: "linux-gnu",
        minimum_version: "glibc-2.34",
        architecture: "x86_64",
    },
];

/// The only artifact variant in the phase-one cohort.
pub(crate) const FIXED_VARIANT: &str = "default";

/// Declared identity fields shared by policy and index artifact entries.
#[derive(Clone, Copy, Debug)]
pub(crate) struct DeclaredIdentity<'a> {
    pub(crate) role: &'a str,
    pub(crate) kind: &'a str,
    pub(crate) package: &'a str,
    pub(crate) target: &'a str,
    pub(crate) variant: &'a str,
    pub(crate) assurance_profile: &'a str,
    pub(crate) capabilities: &'a [String],
    pub(crate) compatibility: &'a Compatibility,
}

impl FixedArtifact {
    /// Returns the declared runtime compatibility of this artifact.
    pub(crate) fn compatibility(&self) -> Compatibility {
        Compatibility {
            runtime: self.runtime.to_owned(),
            minimum_version: self.minimum_version.to_owned(),
            architecture: self.architecture.to_owned(),
        }
    }

    /// Returns whether a declared identity is exactly this supported
    /// role/kind/package/target/variant/profile/capability/compatibility
    /// combination. Capabilities compare as a set.
    pub(crate) fn matches(&self, declared: &DeclaredIdentity<'_>) -> bool {
        let declared_capabilities: BTreeSet<&str> =
            declared.capabilities.iter().map(String::as_str).collect();
        let fixed_capabilities: BTreeSet<&str> = self.capabilities.iter().copied().collect();
        declared.role == self.role
            && declared.kind == self.kind
            && declared.package == self.package
            && declared.target == self.target
            && declared.variant == FIXED_VARIANT
            && declared.assurance_profile == self.assurance_profile
            && declared.capabilities.len() == fixed_capabilities.len()
            && declared_capabilities == fixed_capabilities
            && *declared.compatibility == self.compatibility()
    }
}

/// A reviewed historical producer-policy identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HistoricalPolicy<'a> {
    pub(crate) version: u64,
    pub(crate) sha256: &'a str,
}

/// Reviewed historical producer policies this consumer still accepts.
///
/// Empty for consumer v1. A new CLI policy must explicitly retain or retire
/// each identity here; an index can never add one.
pub(crate) const HISTORICAL_PRODUCER_POLICIES: &[HistoricalPolicy<'static>] = &[];

/// Returns the exact certificate identity for a release tag.
pub(crate) fn certificate_identity(tag: &ReleaseTag) -> String {
    format!("{SIGNER_WORKFLOW_PREFIX}{}", tag.as_str())
}

/// The validated consumer policy and its externally computed identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConsumerPolicy {
    version: u64,
    sha256: String,
}

impl ConsumerPolicy {
    /// Returns the consumer policy version.
    pub(crate) fn version(&self) -> u64 {
        self.version
    }

    /// Returns the SHA-256 of the exact policy bytes.
    pub(crate) fn sha256(&self) -> &str {
        &self.sha256
    }

    /// Returns the official repository slug.
    pub(crate) fn repository(&self) -> &'static str {
        REPOSITORY
    }

    /// Returns the fixed cohort name.
    pub(crate) fn cohort(&self) -> &'static str {
        COHORT
    }

    /// Returns the supported cohort artifacts.
    pub(crate) fn artifacts(&self) -> &'static [FixedArtifact] {
        &FIXED_ARTIFACTS
    }

    /// Returns the supported artifact with this exact name.
    pub(crate) fn artifact(&self, name: &str) -> Option<&'static FixedArtifact> {
        FIXED_ARTIFACTS
            .iter()
            .find(|artifact| artifact.name == name)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyDocument {
    artifacts: Vec<PolicyArtifact>,
    bounds: Value,
    cohort: String,
    github_host: String,
    historical_producer_policies: Vec<HistoricalDocument>,
    identity: Value,
    policy_version: u64,
    predicates: Value,
    public_actions: Vec<String>,
    repository: String,
    repository_id: String,
    repository_owner_id: String,
    tool_versions: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyArtifact {
    actions: Vec<String>,
    assurance_profile: String,
    bin: String,
    capabilities: Vec<String>,
    compatibility: Compatibility,
    default_features: bool,
    features: Vec<String>,
    kind: String,
    name: String,
    package: String,
    role: String,
    target: String,
    variant: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoricalDocument {
    policy_version: u64,
    policy_sha256: String,
}

/// Parses and validates the compiled-in consumer policy.
///
/// # Errors
///
/// Returns an error if the embedded bytes violate the compiled contract,
/// which indicates an unreviewed policy edit.
pub(crate) fn embedded_policy() -> Result<ConsumerPolicy, Report<ReleaseError>> {
    parse_consumer_policy(EMBEDDED_POLICY)
}

/// Parses consumer policy bytes and requires them to equal the compiled contract.
///
/// # Errors
///
/// Returns [`ReleaseError::Document`] for malformed JSON and
/// [`ReleaseError::Policy`] for unknown fields, kinds, profiles, actions or
/// capabilities, forbidden `fastly-staging`, or any changed identity, tool pin
/// or bound.
pub(crate) fn parse_consumer_policy(bytes: &[u8]) -> Result<ConsumerPolicy, Report<ReleaseError>> {
    let value = parse_json(bytes, Role::Policy)?;
    let document = PolicyDocument::deserialize(&value)
        .map_err(|source| report(ReleaseError::Policy, source))?;
    check_scalars(&document)?;
    check_structures(&document)?;
    check_artifacts(&document.artifacts)?;
    Ok(ConsumerPolicy {
        version: document.policy_version,
        sha256: sha256_hex(bytes),
    })
}

fn check_scalars(document: &PolicyDocument) -> Result<(), Report<ReleaseError>> {
    if document.policy_version != CONSUMER_POLICY_VERSION {
        return Err(report(
            ReleaseError::Policy,
            "consumer policy version is not the compiled version",
        ));
    }
    if document.repository != REPOSITORY
        || document.github_host != GITHUB_HOST
        || document.repository_id != REPOSITORY_ID
        || document.repository_owner_id != REPOSITORY_OWNER_ID
        || document.cohort != COHORT
    {
        return Err(report(
            ReleaseError::Policy,
            "policy repository, host, numeric IDs or cohort differ from compiled values",
        ));
    }
    check_actions(&document.public_actions)?;
    let historical: Vec<(u64, &str)> = document
        .historical_producer_policies
        .iter()
        .map(|entry| (entry.policy_version, entry.policy_sha256.as_str()))
        .collect();
    let compiled: Vec<(u64, &str)> = HISTORICAL_PRODUCER_POLICIES
        .iter()
        .map(|entry| (entry.version, entry.sha256))
        .collect();
    if historical != compiled {
        return Err(report(
            ReleaseError::Policy,
            "policy historical producer list differs from the compiled allowlist",
        ));
    }
    Ok(())
}

fn check_structures(document: &PolicyDocument) -> Result<(), Report<ReleaseError>> {
    if document.tool_versions
        != json!({
            "actions_attest": ATTEST_VERSION,
            "actions_attest_revision": ATTEST_REVISION,
            "gh": GH_VERSION,
        })
    {
        return Err(report(
            ReleaseError::Policy,
            "policy tool versions differ from the qualified gh and attest pins",
        ));
    }
    if document.predicates
        != json!({
            "build_type": BUILD_TYPE,
            "provenance": PROVENANCE_PREDICATE,
            "sbom": SBOM_PREDICATE,
        })
    {
        return Err(report(
            ReleaseError::Policy,
            "policy predicates differ from compiled values",
        ));
    }
    if document.identity != expected_identity() {
        return Err(report(
            ReleaseError::Policy,
            "policy signer identity differs from compiled values",
        ));
    }
    if document.bounds != expected_bounds() {
        return Err(report(
            ReleaseError::Policy,
            "policy bounds differ from compiled R-BOUNDS",
        ));
    }
    Ok(())
}

fn expected_identity() -> Value {
    let template = format!("{SIGNER_WORKFLOW_PREFIX}<tag>");
    json!({
        "builder_id": template,
        "caller_commit": "same-approved-source-commit",
        "caller_path": ".github/workflows/release.yml",
        "caller_repository": format!("https://{GITHUB_HOST}/{REPOSITORY}"),
        "certificate_fields": [
            "buildConfigDigest",
            "buildConfigURI",
            "buildTrigger",
            "runInvocationURI",
            "sourceRepositoryIdentifier",
            "sourceRepositoryOwnerIdentifier",
        ],
        "certificate_san": template,
        "event": "push",
        "external_parameters": "exact-caller-workflow-only-no-inputs",
        "invocation": "same-repository-run-and-attempt-across-certificate-and-provenance",
        "issuer": ISSUER,
        "runner": "github-hosted",
        "signer_commit": "same-approved-source-commit",
        "source_commit": "independently-approved-40-lowercase-hex",
        "source_dependency": format!("git+https://{GITHUB_HOST}/{REPOSITORY}@refs/tags/<tag>"),
        "source_ref": "refs/tags/<tag>",
    })
}

fn expected_bounds() -> Value {
    json!({
        "attempt_seconds": bounds::ATTEMPT_SECONDS,
        "bundle_bytes": bounds::BUNDLE_BYTES,
        "child_seconds": bounds::CHILD_SECONDS,
        "cohort_executables": bounds::COHORT_EXECUTABLES,
        "companion_bytes": bounds::COMPANION_BYTES,
        "executable_bytes": bounds::EXECUTABLE_BYTES,
        "index_bytes": bounds::INDEX_BYTES,
        "inventory_bytes": bounds::INVENTORY_BYTES,
        "json_depth": bounds::JSON_DEPTH,
        "matching_attestations_per_bundle": bounds::MATCHING_ATTESTATIONS_PER_BUNDLE,
        "metadata_bytes": bounds::METADATA_BYTES,
        "policy_bytes": bounds::POLICY_BYTES,
        "spdx_bytes": bounds::SPDX_BYTES,
        "stderr_bytes": bounds::STDERR_BYTES,
        "stdout_bytes": bounds::STDOUT_BYTES,
        "trusted_roots_bytes": bounds::TRUSTED_ROOTS_BYTES,
    })
}

fn check_actions(actions: &[String]) -> Result<(), Report<ReleaseError>> {
    if let Some(unknown) = actions
        .iter()
        .find(|action| !KNOWN_ACTIONS.contains(&action.as_str()))
    {
        return Err(report(
            ReleaseError::Policy,
            format!("unknown policy action `{unknown}`"),
        ));
    }
    if actions != [READONLY_VERIFY] {
        return Err(report(
            ReleaseError::Policy,
            "policy actions must be exactly [readonly-verify]",
        ));
    }
    Ok(())
}

fn check_artifacts(artifacts: &[PolicyArtifact]) -> Result<(), Report<ReleaseError>> {
    if artifacts.len() != FIXED_ARTIFACTS.len() {
        return Err(report(
            ReleaseError::Policy,
            "policy must list exactly the cohort artifacts",
        ));
    }
    let mut names = BTreeSet::new();
    for artifact in artifacts {
        if !names.insert(artifact.name.as_str()) {
            return Err(report(
                ReleaseError::Policy,
                format!("policy lists `{}` more than once", artifact.name),
            ));
        }
        check_policy_artifact(artifact)?;
    }
    Ok(())
}

fn check_policy_artifact(artifact: &PolicyArtifact) -> Result<(), Report<ReleaseError>> {
    if !KNOWN_KINDS.contains(&artifact.kind.as_str()) {
        return Err(report(
            ReleaseError::Policy,
            format!("unknown artifact kind `{}`", artifact.kind),
        ));
    }
    if !KNOWN_PROFILES.contains(&artifact.assurance_profile.as_str()) {
        return Err(report(
            ReleaseError::Policy,
            format!("unknown assurance profile `{}`", artifact.assurance_profile),
        ));
    }
    check_actions(&artifact.actions)?;
    check_capability_vocabulary(&artifact.capabilities)?;
    let fixed = FIXED_ARTIFACTS
        .iter()
        .find(|fixed| fixed.name == artifact.name)
        .ok_or_else(|| {
            report(
                ReleaseError::Policy,
                format!("unknown policy artifact `{}`", artifact.name),
            )
        })?;
    let declared = DeclaredIdentity {
        role: &artifact.role,
        kind: &artifact.kind,
        package: &artifact.package,
        target: &artifact.target,
        variant: &artifact.variant,
        assurance_profile: &artifact.assurance_profile,
        capabilities: &artifact.capabilities,
        compatibility: &artifact.compatibility,
    };
    if !fixed.matches(&declared)
        || artifact.bin != fixed.bin
        || !artifact.default_features
        || !artifact.features.is_empty()
    {
        return Err(report(
            ReleaseError::Policy,
            format!(
                "policy artifact `{}` differs from the supported combination",
                artifact.name
            ),
        ));
    }
    Ok(())
}

/// Rejects capabilities the v1 schema cannot express and the
/// structurally valid but policy-forbidden `fastly-staging`.
///
/// # Errors
///
/// Returns [`ReleaseError::Policy`] for an unknown or forbidden capability.
pub(crate) fn check_capability_vocabulary(
    capabilities: &[String],
) -> Result<(), Report<ReleaseError>> {
    if let Some(unknown) = capabilities
        .iter()
        .find(|capability| !KNOWN_CAPABILITIES.contains(&capability.as_str()))
    {
        return Err(report(
            ReleaseError::Policy,
            format!("unknown capability `{unknown}`"),
        ));
    }
    if capabilities
        .iter()
        .any(|capability| capability == STAGING_CAPABILITY)
    {
        return Err(report(
            ReleaseError::Policy,
            "fastly-staging is structurally valid but forbidden by current policy",
        ));
    }
    Ok(())
}

/// Which accepted producer policy an index was built under.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AcceptedProducerPolicy {
    /// The producer policy is byte-identical to this consumer's policy.
    Current,
    /// The producer policy is a reviewed historical identity.
    Historical { version: u64, sha256: String },
}

/// The consumer identity and allowlist that producer policies are judged by.
#[derive(Clone, Copy, Debug)]
pub(crate) struct AcceptanceRules<'a> {
    pub(crate) current_version: u64,
    pub(crate) current_sha256: &'a str,
    pub(crate) historical: &'a [HistoricalPolicy<'a>],
}

impl<'a> AcceptanceRules<'a> {
    /// Returns the compiled rules for this consumer.
    pub(crate) fn compiled(policy: &'a ConsumerPolicy) -> Self {
        Self {
            current_version: policy.version(),
            current_sha256: policy.sha256(),
            historical: HISTORICAL_PRODUCER_POLICIES,
        }
    }
}

/// Returns the exact-byte identity (version, SHA-256) of approved-source bytes.
fn producer_identity(
    approved: &ApprovedSourcePolicy,
) -> Result<(u64, String), Report<ReleaseError>> {
    let producer = parse_json(approved.bytes(), Role::Policy)?;
    let version = producer
        .get("policy_version")
        .and_then(Value::as_u64)
        .filter(|version| *version >= 1)
        .ok_or_else(|| {
            report(
                ReleaseError::ProducerPolicy,
                "approved source policy has no integer version",
            )
        })?;
    Ok((version, sha256_hex(approved.bytes())))
}

/// Binds an index's producer-policy identity to approved-source bytes and
/// accepts only the current policy or a reviewed historical identity.
///
/// The index never selects the consumer policy; acceptance only decides
/// whether the producer contract is one this consumer has reviewed. The
/// consumer's own compiled constraints stay in force either way.
///
/// # Errors
///
/// Returns [`ReleaseError::ProducerPolicy`] if the approved source belongs to
/// another commit, the index identity differs from the approved-source bytes,
/// the producer version is newer than the consumer, or the identity is neither
/// current nor allowlisted.
pub(crate) fn accept_producer_policy(
    index: &ReleaseIndex,
    approved: &ApprovedSourcePolicy,
    rules: &AcceptanceRules<'_>,
) -> Result<AcceptedProducerPolicy, Report<ReleaseError>> {
    if approved.commit().as_str() != index.source_commit {
        return Err(report(
            ReleaseError::ProducerPolicy,
            "approved source policy belongs to another commit",
        ));
    }
    let (producer_version, producer_sha256) = producer_identity(approved)?;
    if index.policy_version != producer_version || index.policy_sha256 != producer_sha256 {
        return Err(report(
            ReleaseError::ProducerPolicy,
            "index policy identity does not match the approved source policy bytes",
        ));
    }
    if !lower_hex(rules.current_sha256, 64) {
        return Err(report(
            ReleaseError::ProducerPolicy,
            "consumer policy digest is malformed",
        ));
    }
    if producer_version == rules.current_version && producer_sha256 == rules.current_sha256 {
        return Ok(AcceptedProducerPolicy::Current);
    }
    if producer_version > rules.current_version {
        return Err(report(
            ReleaseError::ProducerPolicy,
            format!(
                "unsupported producer policy version {producer_version} under consumer policy version {}",
                rules.current_version
            ),
        ));
    }
    if rules
        .historical
        .iter()
        .any(|entry| entry.version == producer_version && entry.sha256 == producer_sha256)
    {
        return Ok(AcceptedProducerPolicy::Historical {
            version: producer_version,
            sha256: producer_sha256,
        });
    }
    Err(report(
        ReleaseError::ProducerPolicy,
        "producer policy is neither current nor a reviewed historical identity",
    ))
}
