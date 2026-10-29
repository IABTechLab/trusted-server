//! Compiled-in release cohort contract and strict parsing of tracked release inputs.
//!
//! The JSON inputs are compared against this source-owned contract; they never
//! supply commands, packages, targets or authority. Pending or JSON-qualified
//! pins remain diagnostics until authenticated acquisition receipts exist.

use std::collections::BTreeSet;
use std::path::Path;

use error_stack::Report;
use serde::Deserialize;
use serde::de::DeserializeOwned;
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
/// Version of the tracked consumer release policy the compiled contract accepts.
pub(crate) const CLI_POLICY_VERSION: u64 = 1;
/// Retained cohort label; controlled index meaning remains destination-only.
pub(crate) const COHORT_ID: &str = "phase1-fastly-cli";
/// Official repository named by every release index.
pub(crate) const REPOSITORY: &str = "IABTechLab/trusted-server";
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
const NOT_ESTABLISHED: [&str; 4] = [
    "authenticated acquisition receipts (Task 8)",
    "git executable identity (Task 8)",
    "rustup override/toolchain selection (bootstrap, Task 8)",
    "remote branch-tip ancestry and ruleset validation (Task 14)",
];

/// Targets of the four closed controlled-recipe destination rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReleaseTarget {
    Wasm32Wasip1,
    Aarch64AppleDarwin,
    X86_64AppleDarwin,
    X86_64UnknownLinuxGnu,
}

impl ReleaseTarget {
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
    // Only the Task 5/7 child-environment builder (`preflight::child`) calls this.
    #[allow(dead_code)]
    pub(crate) fn is_darwin(self) -> bool {
        matches!(self, Self::Aarch64AppleDarwin | Self::X86_64AppleDarwin)
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

impl FixedArtifact {
    /// Renders this asset's fixed release-index identity fields.
    ///
    /// The result is exactly the governing schema's per-asset `oneOf` row: the
    /// name, role, kind, package, target, default variant, assurance profile,
    /// the single compiled capability and the compatibility triple. Nothing in
    /// it comes from JSON input, so it can never add `fastly-staging`.
    #[allow(dead_code, reason = "consumed by Phase 3 index construction")]
    pub(crate) fn index_identity(&self) -> Value {
        json!({
            "name": self.name,
            "role": self.role,
            "kind": self.kind,
            "package": self.package,
            "target": self.target.triple(),
            "variant": "default",
            "assurance_profile": self.assurance_profile,
            "capabilities": [self.capability],
            "compatibility": {
                "runtime": self.runtime,
                "minimum_version": self.minimum_version,
                "architecture": self.architecture,
            },
        })
    }
}

/// The four controlled destination rows, not the seven ordinary release assets.
///
/// These recipes and index identities remain closed. Ordinary Axum metadata
/// below grants neither a controlled recipe nor seven-asset index authority.
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

/// Source-owned ordinary metadata, without a controlled target or recipe.
struct OrdinaryArtifact {
    name: &'static str,
    package: &'static str,
    bin: &'static str,
    target: &'static str,
    kind: &'static str,
    role: &'static str,
    assurance_profile: &'static str,
    capability: &'static str,
    architecture: &'static str,
    minimum_version: &'static str,
    runtime: &'static str,
}

fn ordinary_artifacts() -> Vec<OrdinaryArtifact> {
    let mut artifacts: Vec<_> = FIXED_ARTIFACTS
        .iter()
        .map(|fixed| OrdinaryArtifact {
            name: fixed.name,
            package: fixed.package,
            bin: fixed.bin,
            target: fixed.target.triple(),
            kind: fixed.kind,
            role: fixed.role,
            assurance_profile: fixed.assurance_profile,
            capability: fixed.capability,
            architecture: fixed.architecture,
            minimum_version: fixed.minimum_version,
            runtime: fixed.runtime,
        })
        .collect();
    for (name, target, architecture, minimum_version, runtime) in [
        (
            "trusted-server-axum-x86_64-unknown-linux-gnu",
            "x86_64-unknown-linux-gnu",
            "x86_64",
            "glibc-2.34",
            "linux-gnu",
        ),
        (
            "trusted-server-axum-x86_64-unknown-linux-musl",
            "x86_64-unknown-linux-musl",
            "x86_64",
            "static",
            "linux-musl",
        ),
        (
            "trusted-server-axum-aarch64-unknown-linux-gnu",
            "aarch64-unknown-linux-gnu",
            "aarch64",
            "glibc-2.34",
            "linux-gnu",
        ),
    ] {
        artifacts.push(OrdinaryArtifact {
            name,
            target,
            architecture,
            minimum_version,
            runtime,
            package: "trusted-server-adapter-axum",
            bin: "trusted-server-axum",
            kind: "native-executable",
            role: "server",
            assurance_profile: "provenance-sbom",
            capability: "axum-production",
        });
    }
    artifacts
}

/// Test-only rows naming the members of the Cargo denial fixture workspace.
///
/// They are never part of [`FIXED_ARTIFACTS`] or the cohort, so no request
/// text can select them; only test code reaches them by package name.
#[cfg(test)]
pub(crate) const DENIAL_FIXTURE_ARTIFACTS: [FixedArtifact; 4] = [
    denial_fixture_row("denial-build-direct.wasm", "build-direct"),
    denial_fixture_row("denial-build-grandchild.wasm", "build-grandchild"),
    denial_fixture_row("denial-macro-attempt.wasm", "macro-attempt"),
    denial_fixture_row("denial-control.wasm", "control"),
];

/// Test-only rows naming the Cargo denial fixture members built for each
/// macOS CPU, so the macOS candidate jobs can prove build-script, grandchild
/// and procedural-macro denial through the actual Darwin recipe.
///
/// Like [`DENIAL_FIXTURE_ARTIFACTS`] they are outside [`FIXED_ARTIFACTS`]
/// and the cohort; only test code reaches them by package and target.
#[cfg(test)]
pub(crate) const DARWIN_DENIAL_FIXTURE_ARTIFACTS: [FixedArtifact; 8] = [
    darwin_denial_row(
        "denial-build-direct-aarch64-apple-darwin",
        "build-direct",
        ARM64,
    ),
    darwin_denial_row(
        "denial-build-grandchild-aarch64-apple-darwin",
        "build-grandchild",
        ARM64,
    ),
    darwin_denial_row(
        "denial-macro-attempt-aarch64-apple-darwin",
        "macro-attempt",
        ARM64,
    ),
    darwin_denial_row("denial-control-aarch64-apple-darwin", "control", ARM64),
    darwin_denial_row(
        "denial-build-direct-x86_64-apple-darwin",
        "build-direct",
        INTEL,
    ),
    darwin_denial_row(
        "denial-build-grandchild-x86_64-apple-darwin",
        "build-grandchild",
        INTEL,
    ),
    darwin_denial_row(
        "denial-macro-attempt-x86_64-apple-darwin",
        "macro-attempt",
        INTEL,
    ),
    darwin_denial_row("denial-control-x86_64-apple-darwin", "control", INTEL),
];

/// The arm64 macOS target of the Darwin denial rows.
#[cfg(test)]
const ARM64: ReleaseTarget = ReleaseTarget::Aarch64AppleDarwin;
/// The Intel macOS target of the Darwin denial rows.
#[cfg(test)]
const INTEL: ReleaseTarget = ReleaseTarget::X86_64AppleDarwin;

/// One Darwin denial fixture row for `target`.
#[cfg(test)]
const fn darwin_denial_row(
    name: &'static str,
    package: &'static str,
    target: ReleaseTarget,
) -> FixedArtifact {
    // Evaluated at compile time, so a non-Darwin row fails the build.
    let architecture = match target {
        ReleaseTarget::Aarch64AppleDarwin => "aarch64",
        ReleaseTarget::X86_64AppleDarwin => "x86_64",
        ReleaseTarget::Wasm32Wasip1 | ReleaseTarget::X86_64UnknownLinuxGnu => {
            panic!("Darwin denial rows need a Darwin target")
        }
    };
    FixedArtifact {
        target,
        architecture,
        minimum_version: "13.0",
        runtime: "macos",
        ..denial_fixture_row(name, package)
    }
}

/// One Wasm denial fixture row whose binary is named after its package.
#[cfg(test)]
const fn denial_fixture_row(name: &'static str, package: &'static str) -> FixedArtifact {
    FixedArtifact {
        name,
        package,
        bin: package,
        target: ReleaseTarget::Wasm32Wasip1,
        kind: "denial-fixture",
        role: "test-fixture",
        assurance_profile: "none",
        capability: "none",
        architecture: "wasm32",
        minimum_version: "wasip1",
        runtime: "none",
        recipe_environment: "ordinary-ci",
    }
}

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
    pub(crate) fn input_digests(&self) -> [(&'static str, &str); 3] {
        self.digests
            .each_ref()
            .map(|digest| (digest.path, digest.sha256.as_str()))
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
pub(crate) struct QualifiedSelection<'a> {
    receipts: Vec<String>,
    inputs: &'a ReleaseInputs,
}

impl QualifiedSelection<'_> {
    /// Summarizes the selected inputs for the caller's diagnostics.
    pub(crate) fn summary(&self) -> String {
        format!(
            "selected {} authenticated pins from {} inputs",
            self.receipts.len(),
            self.inputs.digests.len()
        )
    }
}

/// Selects production pins only when every pin is qualified and authenticated.
///
/// # Errors
///
/// Fails on any pending qualification or null pin, and otherwise fails because
/// authenticated acquisition receipts cannot be constructed yet.
pub(crate) fn select_qualified<'a>(
    inputs: &'a ReleaseInputs,
    receipts: &[AcquisitionReceipt],
) -> Result<QualifiedSelection<'a>, Report<ReleaseError>> {
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
        inputs,
    })
}

/// Diagnostic description of incomplete inputs. It authorizes nothing.
pub(crate) struct InputAudit<'a> {
    inputs: &'a ReleaseInputs,
    pending: Vec<String>,
}

impl InputAudit<'_> {
    /// Returns whether every JSON pin and external authentication is present.
    ///
    /// This is always `false` while [`NOT_ESTABLISHED`] lists facts this tool
    /// cannot yet establish, so the diagnostic can never report completion.
    pub(crate) fn is_complete(&self) -> bool {
        self.pending.is_empty() && NOT_ESTABLISHED.is_empty()
    }

    /// Renders the deterministic diagnostic report.
    pub(crate) fn to_json(&self) -> Value {
        json!({
            "inputs": self
                .inputs
                .input_digests()
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
pub(crate) fn audit_inputs(inputs: &ReleaseInputs) -> InputAudit<'_> {
    InputAudit {
        inputs,
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

fn typed<T: DeserializeOwned>(value: &Value, path: &str) -> Result<T, Report<ReleaseError>> {
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
    let ordinary = ordinary_artifacts();
    for artifact in &cohort.artifacts {
        if !seen.insert(artifact.name.as_str()) {
            return Err(rejected(format!("duplicate asset {}", artifact.name)));
        }
        let fixed = ordinary
            .iter()
            .find(|fixed| fixed.name == artifact.name)
            .ok_or_else(|| rejected(format!("unknown asset {}", artifact.name)))?;
        if !ordinary.iter().any(|fixed| fixed.target == artifact.target) {
            return Err(rejected(format!(
                "asset {} selects unknown target {}",
                artifact.name, artifact.target
            )));
        }
        check_artifact(fixed, artifact)?;
    }
    if let Some(missing) = ordinary.iter().find(|fixed| !seen.contains(fixed.name)) {
        return Err(rejected(format!("missing asset {}", missing.name)));
    }
    Ok(())
}

fn check_artifact(
    fixed: &OrdinaryArtifact,
    artifact: &CohortArtifact,
) -> Result<(), Report<ReleaseError>> {
    let name = fixed.name;
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
    for (field, expected, actual) in identity_fields(fixed, artifact) {
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

/// Field name, compiled value and candidate value of each fixed identity field.
fn identity_fields<'a>(
    fixed: &OrdinaryArtifact,
    artifact: &'a CohortArtifact,
) -> [(&'static str, &'static str, &'a str); 9] {
    let compatibility = &artifact.compatibility;
    [
        ("package", fixed.package, artifact.package.as_str()),
        ("bin", fixed.bin, artifact.bin.as_str()),
        ("target", fixed.target, artifact.target.as_str()),
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
    ]
}

/// Every non-artifact field of the tracked consumer release policy.
fn compiled_consumer_policy() -> Value {
    let workflow = "https://github.com/IABTechLab/trusted-server/.github/workflows/release-build.yml@refs/tags/<tag>";
    json!({
        "bounds": {
            "attempt_seconds": 900,
            "bundle_bytes": 33_554_432,
            "child_seconds": 120,
            "cohort_executables": FIXED_ARTIFACTS.len(),
            "companion_bytes": 33_554_432,
            "executable_bytes": 536_870_912,
            "index_bytes": 1_048_576,
            "inventory_bytes": 1_048_576,
            "json_depth": 64,
            "matching_attestations_per_bundle": 1,
            "metadata_bytes": 1_048_576,
            "policy_bytes": 1_048_576,
            "spdx_bytes": 16_777_216,
            "stderr_bytes": 1_048_576,
            "stdout_bytes": 134_217_728,
            "trusted_roots_bytes": 1_048_576,
        },
        "cohort": COHORT_ID,
        "github_host": "github.com",
        "historical_producer_policies": [],
        "identity": {
            "builder_id": workflow,
            "caller_commit": "same-approved-source-commit",
            "caller_path": ".github/workflows/release.yml",
            "caller_repository": "https://github.com/IABTechLab/trusted-server",
            "certificate_fields": [
                "buildConfigDigest",
                "buildConfigURI",
                "buildTrigger",
                "runInvocationURI",
                "sourceRepositoryIdentifier",
                "sourceRepositoryOwnerIdentifier",
            ],
            "certificate_san": workflow,
            "event": "push",
            "external_parameters": "exact-caller-workflow-only-no-inputs",
            "invocation": "same-repository-run-and-attempt-across-certificate-and-provenance",
            "issuer": "https://token.actions.githubusercontent.com",
            "runner": "github-hosted",
            "signer_commit": "same-approved-source-commit",
            "source_commit": "independently-approved-40-lowercase-hex",
            "source_dependency": "git+https://github.com/IABTechLab/trusted-server@refs/tags/<tag>",
            "source_ref": "refs/tags/<tag>",
        },
        "policy_version": CLI_POLICY_VERSION,
        "predicates": {
            "build_type": "https://actions.github.io/buildtypes/workflow/v1",
            "provenance": "https://slsa.dev/provenance/v1",
            "sbom": "https://spdx.dev/Document/v2.3",
        },
        "public_actions": READONLY_ACTIONS,
        "repository": REPOSITORY,
        "repository_id": "950860906",
        "repository_owner_id": "82903538",
        "tool_versions": {
            "actions_attest": ATTEST_VERSION,
            "actions_attest_revision": ATTEST_REVISION,
            "gh": GH_VERSION,
        },
    })
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
    let compiled = compiled_consumer_policy();
    for key in CLI_POLICY_FIELDS.iter().filter(|key| **key != "artifacts") {
        if object.get(*key) != compiled.get(*key) {
            return Err(rejected(format!(
                "release-policy field {key} differs from the compiled consumer policy"
            )));
        }
    }
    check_policy_artifacts(&object["artifacts"], cohort)
}

/// Joins the exact four controlled destination rows plus read-only actions.
///
/// The ordinary seven-row cohort has already passed strict validation. Its
/// additional Axum metadata cannot expand the carried consumer/index contract.
fn check_policy_artifacts(policy: &Value, cohort: &Value) -> Result<(), Report<ReleaseError>> {
    let policy_artifacts = policy
        .as_array()
        .ok_or_else(|| rejected("release-policy artifacts must be an array"))?;
    let cohort_artifacts = cohort["artifacts"]
        .as_array()
        .ok_or_else(|| rejected("cohort artifacts must be an array"))?;
    if policy_artifacts.len() != FIXED_ARTIFACTS.len() {
        return Err(rejected(
            "release-policy artifacts must list exactly the four controlled destination artifacts",
        ));
    }
    let readonly = json!(READONLY_ACTIONS);
    for (policy_artifact, fixed) in policy_artifacts.iter().zip(&FIXED_ARTIFACTS) {
        let matching: Vec<_> = cohort_artifacts
            .iter()
            .filter(|row| row["name"] == fixed.name)
            .collect();
        if matching.len() != 1 {
            return Err(rejected(
                "controlled destination row is missing or duplicated",
            ));
        }
        let cohort_artifact = matching[0];
        let policy_object = policy_artifact
            .as_object()
            .ok_or_else(|| rejected("release-policy artifact must be an object"))?;
        let cohort_object = cohort_artifact
            .as_object()
            .ok_or_else(|| rejected("cohort artifact must be an object"))?;
        if policy_object.get("actions") != Some(&readonly) {
            return Err(rejected(
                "release-policy artifact actions must be exactly [\"readonly-verify\"]",
            ));
        }
        let joined = policy_object.len() == cohort_object.len() + 1
            && cohort_object
                .iter()
                .all(|(key, value)| key != "actions" && policy_object.get(key) == Some(value));
        if !joined {
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
