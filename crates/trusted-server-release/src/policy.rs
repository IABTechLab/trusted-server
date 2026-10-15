//! Compiled-in release cohort contract and strict parsing of tracked release inputs.
//!
//! The JSON inputs are compared against this source-owned contract; they never
//! supply commands, packages, targets or authority. Pending or JSON-qualified
//! pins remain diagnostics until authenticated acquisition receipts exist.

use std::collections::BTreeSet;
use std::path::Path;

use error_stack::Report;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use crate::documents::{Role, parse_json, read_bounded};
use crate::error::ReleaseError;
use tools::{BUILDER_ENVIRONMENT, ToolsDocument, check_tools};

mod tools;

/// Source-relative path of the tracked cohort selection.
pub(crate) const COHORT_PATH: &str = "scripts/release/release-cohort.json";
/// Source-relative path of the tracked tool, environment and recipe pins.
pub(crate) const TOOLS_PATH: &str = "scripts/release/release-tools.json";
/// Source-relative path of the tracked consumer release policy.
pub(crate) const CLI_POLICY_PATH: &str = "crates/trusted-server-cli/release-policy.json";
/// Branches whose tips may supply release sources; candidates cannot extend it.
pub(crate) const ELIGIBLE_BRANCHES: [&str; 1] = ["main"];

const COHORT_ID: &str = "phase1-fastly-cli";
const SCHEMA_VERSION: u64 = 1;
const GH_VERSION: &str = "2.97.0";
const ATTEST_VERSION: &str = "4.2.2";
const ATTEST_REVISION: &str = "1e69f48acb82d1966a394da916b4c1698aa569d6";
const READONLY_ACTIONS: [&str; 1] = ["readonly-verify"];
const CLI_POLICY_FIELDS: [&str; 13] = [
    "artifacts",
    "bounds",
    "cohort",
    "github_host",
    "historical_producer_policies",
    "identity",
    "policy_version",
    "predicates",
    "public_actions",
    "repository",
    "repository_id",
    "repository_owner_id",
    "tool_versions",
];
const NOT_ESTABLISHED: [&str; 2] = [
    "authenticated acquisition receipts (Task 8)",
    "remote branch-tip ancestry and ruleset validation (Task 14)",
];

/// Target triples the compiled cohort contract can build.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReleaseTarget {
    Wasm32Wasip1,
    Aarch64AppleDarwin,
    X86_64AppleDarwin,
    X86_64UnknownLinuxGnu,
}

impl ReleaseTarget {
    const ALL: [Self; 4] = [
        Self::Wasm32Wasip1,
        Self::Aarch64AppleDarwin,
        Self::X86_64AppleDarwin,
        Self::X86_64UnknownLinuxGnu,
    ];

    /// Returns the Rust target triple.
    pub(crate) fn triple(self) -> &'static str {
        match self {
            Self::Wasm32Wasip1 => "wasm32-wasip1",
            Self::Aarch64AppleDarwin => "aarch64-apple-darwin",
            Self::X86_64AppleDarwin => "x86_64-apple-darwin",
            Self::X86_64UnknownLinuxGnu => "x86_64-unknown-linux-gnu",
        }
    }

    /// Returns whether the target needs the explicit macOS loader floor.
    pub(crate) fn is_darwin(self) -> bool {
        matches!(self, Self::Aarch64AppleDarwin | Self::X86_64AppleDarwin)
    }

    fn from_triple(triple: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|target| target.triple() == triple)
    }
}

/// One source-owned asset identity; JSON can only restate it.
pub(crate) struct FixedArtifact {
    pub(crate) name: &'static str,
    pub(crate) package: &'static str,
    pub(crate) bin: &'static str,
    pub(crate) target: ReleaseTarget,
    kind: &'static str,
    role: &'static str,
    pub(crate) assurance_profile: &'static str,
    capability: &'static str,
    architecture: &'static str,
    minimum_version: &'static str,
    runtime: &'static str,
    recipe_environment: &'static str,
}

/// The complete phase-one cohort. Every asset uses default features only.
pub(crate) const FIXED_ARTIFACTS: [FixedArtifact; 4] = [
    FixedArtifact {
        name: "trusted-server-fastly.wasm",
        package: "trusted-server-adapter-fastly",
        bin: "trusted-server-adapter-fastly",
        target: ReleaseTarget::Wasm32Wasip1,
        kind: "wasm-core",
        role: "server",
        assurance_profile: "provenance-sbom-repeatable",
        capability: "fastly-production",
        architecture: "wasm32",
        minimum_version: "wasip1",
        runtime: "fastly-compute",
        recipe_environment: BUILDER_ENVIRONMENT,
    },
    FixedArtifact {
        name: "ts-aarch64-apple-darwin",
        package: "trusted-server-cli",
        bin: "ts",
        target: ReleaseTarget::Aarch64AppleDarwin,
        kind: "native-executable",
        role: "operator-cli",
        assurance_profile: "provenance-sbom",
        capability: "cli-execute",
        architecture: "aarch64",
        minimum_version: "13.0",
        runtime: "macos",
        recipe_environment: "macos15-aarch64",
    },
    FixedArtifact {
        name: "ts-x86_64-apple-darwin",
        package: "trusted-server-cli",
        bin: "ts",
        target: ReleaseTarget::X86_64AppleDarwin,
        kind: "native-executable",
        role: "operator-cli",
        assurance_profile: "provenance-sbom",
        capability: "cli-execute",
        architecture: "x86_64",
        minimum_version: "13.0",
        runtime: "macos",
        recipe_environment: "macos15-x86_64",
    },
    FixedArtifact {
        name: "ts-x86_64-unknown-linux-gnu",
        package: "trusted-server-cli",
        bin: "ts",
        target: ReleaseTarget::X86_64UnknownLinuxGnu,
        kind: "native-executable",
        role: "operator-cli",
        assurance_profile: "provenance-sbom",
        capability: "cli-execute",
        architecture: "x86_64",
        minimum_version: "glibc-2.34",
        runtime: "linux-gnu",
        recipe_environment: BUILDER_ENVIRONMENT,
    },
];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CohortDocument {
    artifacts: Vec<CohortArtifact>,
    cohort: String,
    eligible_branches: Vec<String>,
    schema_version: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CohortArtifact {
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
struct Compatibility {
    architecture: String,
    minimum_version: String,
    runtime: String,
}

/// Raw-byte identity of one tracked input file.
#[derive(Clone)]
struct InputDigest {
    path: &'static str,
    sha256: String,
}

/// Parsed tracked release inputs that match the compiled contract.
///
/// This is not authorization: pins may still be pending or unauthenticated.
pub(crate) struct ReleaseInputs {
    digests: [InputDigest; 3],
    tools: ToolsDocument,
}

impl ReleaseInputs {
    /// Returns the source-relative path and raw SHA-256 of each input file.
    pub(crate) fn input_digests(&self) -> [(&'static str, String); 3] {
        self.digests
            .clone()
            .map(|digest| (digest.path, digest.sha256))
    }

    fn pending_items(&self) -> Vec<String> {
        self.tools.pending_items()
    }
}

/// An authenticated acquisition and measured-pin receipt.
///
/// No receipt verifier exists yet (Task 8), so this type is uninhabited and
/// production selection cannot succeed.
pub(crate) enum AcquisitionReceipt {}

/// Production tool selection; constructed only by [`select_qualified`].
pub(crate) struct QualifiedSelection {
    receipts: Vec<String>,
    inputs: [(&'static str, String); 3],
}

impl QualifiedSelection {
    /// Summarizes the selected inputs for the caller's diagnostics.
    pub(crate) fn summary(&self) -> String {
        format!(
            "selected {} authenticated pins from {} inputs",
            self.receipts.len(),
            self.inputs.len()
        )
    }
}

/// Selects production pins only when every pin is qualified and authenticated.
///
/// # Errors
///
/// Fails on any pending qualification or null pin, and otherwise fails because
/// authenticated acquisition receipts cannot be constructed yet.
pub(crate) fn select_qualified(
    inputs: &ReleaseInputs,
    receipts: &[AcquisitionReceipt],
) -> Result<QualifiedSelection, Report<ReleaseError>> {
    let pending = inputs.pending_items();
    if !pending.is_empty() {
        return Err(Report::new(ReleaseError::Unqualified).attach(format!(
            "{} release pins are pending or null; production selection requires every pin qualified: {}",
            pending.len(),
            pending.join("; ")
        )));
    }
    let verified: Vec<String> = receipts.iter().map(|receipt| match *receipt {}).collect();
    if verified.is_empty() {
        return Err(Report::new(ReleaseError::Unqualified).attach(
            "qualified pins require authenticated acquisition receipts (Task 8); \
             production selection remains closed",
        ));
    }
    Ok(QualifiedSelection {
        receipts: verified,
        inputs: inputs.input_digests(),
    })
}

/// Diagnostic description of incomplete inputs. It authorizes nothing.
pub(crate) struct InputAudit {
    inputs: [(&'static str, String); 3],
    pending: Vec<String>,
}

impl InputAudit {
    /// Returns whether every JSON pin and external authentication is present.
    pub(crate) fn is_complete(&self) -> bool {
        self.pending.is_empty() && NOT_ESTABLISHED.is_empty()
    }

    /// Renders the deterministic diagnostic report.
    pub(crate) fn to_json(&self) -> Value {
        json!({
            "inputs": self
                .inputs
                .iter()
                .map(|(path, sha256)| json!({"path": path, "sha256": sha256}))
                .collect::<Vec<_>>(),
            "not_established": NOT_ESTABLISHED,
            "pending": self.pending,
            "report": "release-input-audit",
            "status": if self.is_complete() { "complete" } else { "incomplete" },
        })
    }
}

/// Lists pending and null items for diagnostics without constructing authority.
pub(crate) fn audit_inputs(inputs: &ReleaseInputs) -> InputAudit {
    InputAudit {
        inputs: inputs.input_digests(),
        pending: inputs.pending_items(),
    }
}

/// Reads the three tracked inputs below `root` and checks them.
///
/// # Errors
///
/// Fails when any input cannot be read under its byte bound or violates the
/// compiled contract.
pub(crate) fn load_inputs(root: &Path) -> Result<ReleaseInputs, Report<ReleaseError>> {
    let cohort = read_bounded(&root.join(COHORT_PATH), Role::Metadata)?;
    let tools = read_bounded(&root.join(TOOLS_PATH), Role::Metadata)?;
    let cli_policy = read_bounded(&root.join(CLI_POLICY_PATH), Role::Metadata)?;
    check_inputs(&cohort.bytes, &tools.bytes, &cli_policy.bytes)
}

/// Parses and checks exact input bytes against the compiled contract.
///
/// # Errors
///
/// Fails on malformed JSON, unknown or missing fields, or any deviation from
/// the compiled cohort, tool pins, recipes or consumer policy join.
pub(crate) fn check_inputs(
    cohort: &[u8],
    tools: &[u8],
    cli_policy: &[u8],
) -> Result<ReleaseInputs, Report<ReleaseError>> {
    let cohort_value = parse_json(cohort, Role::Metadata)?;
    let cohort_document: CohortDocument = typed(&cohort_value, COHORT_PATH)?;
    check_cohort(&cohort_document)?;
    let tools_document: ToolsDocument = typed(&parse_json(tools, Role::Metadata)?, TOOLS_PATH)?;
    check_tools(&tools_document)?;
    check_cli_policy(&parse_json(cli_policy, Role::Metadata)?, &cohort_value)?;
    let digest = |path, bytes: &[u8]| InputDigest {
        path,
        sha256: hex::encode(Sha256::digest(bytes)),
    };
    Ok(ReleaseInputs {
        digests: [
            digest(COHORT_PATH, cohort),
            digest(TOOLS_PATH, tools),
            digest(CLI_POLICY_PATH, cli_policy),
        ],
        tools: tools_document,
    })
}

fn typed<T: for<'de> Deserialize<'de>>(
    value: &Value,
    path: &str,
) -> Result<T, Report<ReleaseError>> {
    T::deserialize(value).map_err(|error| rejected(format!("{path}: {error}")))
}

fn rejected(message: impl Into<String>) -> Report<ReleaseError> {
    Report::new(ReleaseError::Policy).attach(message.into())
}

fn check_cohort(cohort: &CohortDocument) -> Result<(), Report<ReleaseError>> {
    if cohort.schema_version != SCHEMA_VERSION {
        return Err(rejected("cohort schema_version must be 1"));
    }
    if cohort.cohort != COHORT_ID {
        return Err(rejected(format!("cohort must be {COHORT_ID}")));
    }
    if cohort.eligible_branches != ELIGIBLE_BRANCHES {
        return Err(rejected(
            "eligible_branches must be exactly [\"main\"]; a candidate cohort cannot \
             approve its own branch without independent review",
        ));
    }
    let mut seen = BTreeSet::new();
    for artifact in &cohort.artifacts {
        if !seen.insert(artifact.name.as_str()) {
            return Err(rejected(format!("duplicate asset {}", artifact.name)));
        }
        let fixed = FIXED_ARTIFACTS
            .iter()
            .find(|fixed| fixed.name == artifact.name)
            .ok_or_else(|| rejected(format!("unknown asset {}", artifact.name)))?;
        check_artifact(fixed, artifact)?;
    }
    if let Some(missing) = FIXED_ARTIFACTS
        .iter()
        .find(|fixed| !seen.contains(fixed.name))
    {
        return Err(rejected(format!("missing asset {}", missing.name)));
    }
    Ok(())
}

fn check_artifact(
    fixed: &FixedArtifact,
    artifact: &CohortArtifact,
) -> Result<(), Report<ReleaseError>> {
    let name = fixed.name;
    if ReleaseTarget::from_triple(&artifact.target).is_none() {
        return Err(rejected(format!(
            "asset {name} selects unknown target {}",
            artifact.target
        )));
    }
    if !artifact.features.is_empty() {
        return Err(rejected(format!(
            "asset {name} requests unsupported features {:?}; only the default variant \
             with reusable-sandbox disabled is attested",
            artifact.features
        )));
    }
    if !artifact.default_features || artifact.variant != "default" {
        return Err(rejected(format!(
            "asset {name} must keep default_features true and variant default"
        )));
    }
    let compatibility = &artifact.compatibility;
    let comparisons = [
        ("package", fixed.package, artifact.package.as_str()),
        ("bin", fixed.bin, artifact.bin.as_str()),
        ("target", fixed.target.triple(), artifact.target.as_str()),
        ("kind", fixed.kind, artifact.kind.as_str()),
        ("role", fixed.role, artifact.role.as_str()),
        (
            "assurance_profile",
            fixed.assurance_profile,
            artifact.assurance_profile.as_str(),
        ),
        (
            "compatibility.architecture",
            fixed.architecture,
            compatibility.architecture.as_str(),
        ),
        (
            "compatibility.minimum_version",
            fixed.minimum_version,
            compatibility.minimum_version.as_str(),
        ),
        (
            "compatibility.runtime",
            fixed.runtime,
            compatibility.runtime.as_str(),
        ),
    ];
    for (field, expected, actual) in comparisons {
        if expected != actual {
            return Err(rejected(format!(
                "asset {name} changes {field} from {expected} to {actual}"
            )));
        }
    }
    if artifact.capabilities != [fixed.capability] {
        return Err(rejected(format!(
            "asset {name} changes capabilities from [{}]",
            fixed.capability
        )));
    }
    Ok(())
}

fn check_cli_policy(policy: &Value, cohort: &Value) -> Result<(), Report<ReleaseError>> {
    let object = policy
        .as_object()
        .ok_or_else(|| rejected("release-policy must be an object"))?;
    let keys: BTreeSet<&str> = object.keys().map(String::as_str).collect();
    if keys != BTreeSet::from(CLI_POLICY_FIELDS) {
        return Err(rejected(
            "release-policy must contain exactly the reviewed top-level fields",
        ));
    }
    if object["cohort"] != COHORT_ID || object["policy_version"] != 1 {
        return Err(rejected(format!(
            "release-policy cohort must be {COHORT_ID} with policy_version 1"
        )));
    }
    if object["public_actions"] != json!(READONLY_ACTIONS) {
        return Err(rejected(
            "release-policy public_actions must be readonly-verify",
        ));
    }
    if object["tool_versions"]
        != json!({
            "actions_attest": ATTEST_VERSION,
            "actions_attest_revision": ATTEST_REVISION,
            "gh": GH_VERSION,
        })
    {
        return Err(rejected(
            "release-policy tool_versions must match the compiled gh and attest pins",
        ));
    }
    if object["bounds"]["cohort_executables"] != FIXED_ARTIFACTS.len() {
        return Err(rejected(
            "release-policy bounds.cohort_executables must equal the cohort size",
        ));
    }
    let policy_artifacts = object["artifacts"]
        .as_array()
        .ok_or_else(|| rejected("release-policy artifacts must be an array"))?;
    let cohort_artifacts = cohort["artifacts"]
        .as_array()
        .ok_or_else(|| rejected("cohort artifacts must be an array"))?;
    if policy_artifacts.len() != cohort_artifacts.len() {
        return Err(rejected(
            "release-policy artifacts must list exactly the cohort artifacts",
        ));
    }
    for (policy_artifact, cohort_artifact) in policy_artifacts.iter().zip(cohort_artifacts) {
        let mut remainder = policy_artifact
            .as_object()
            .cloned()
            .ok_or_else(|| rejected("release-policy artifact must be an object"))?;
        let actions = remainder.remove("actions");
        if actions != Some(json!(READONLY_ACTIONS)) {
            return Err(rejected(
                "release-policy artifact actions must be exactly [\"readonly-verify\"]",
            ));
        }
        if Value::Object(remainder) != *cohort_artifact {
            return Err(rejected(format!(
                "release-policy artifact {} differs from the cohort artifact",
                cohort_artifact["name"]
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
