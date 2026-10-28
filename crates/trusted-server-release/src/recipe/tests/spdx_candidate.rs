//! Checks of the unqualified candidate whole-document SPDX: the assertions the
//! real CI builds run on every completed artifact, the measurement they print,
//! and the wiring rejections a pure completion fixture can reach.
//!
//! Byte identity of the canonical and reproduction candidates is recorded as a
//! measurement only; it is not a gate.

use serde_json::{Value, json};
use sha2::Sha256;

use super::complete::{Built, built, wasm_with};
use super::fixture::OTHER_COMMIT;
use super::*;
use crate::policy::ReleaseTarget;
use crate::recipe::AppleToolEvidence;
use crate::spdx::candidate::{CandidateSpdx, candidate_from_completed};

/// SPDX identifier of the executable root package.
const ROOT_SPDX_ID: &str = "SPDXRef-Artifact";
/// Gap stated when GNU privileged helper contents stay unmeasured.
const GNU_PRIVILEGED_HELPER_GAP: &str = "gnu-privileged-helper-content-unmeasured";
/// Gap stated when the Apple tool closure is not fully measured.
const APPLE_TOOL_CLOSURE_GAP: &str = "apple-tool-closure-unmeasured";

/// Renders and checks the candidate SPDX of one real completed build.
///
/// # Panics
///
/// Panics when the candidate cannot be rendered, does not validate, binds
/// another root digest, omits or invents a projected package, or misses a
/// coverage gap its target family must state.
pub(super) fn check_real_candidate(
    completed: &RecipeCompleted,
    source: &ApprovedSource,
) -> CandidateSpdx {
    let candidate = candidate_from_completed(completed, source).unwrap_or_else(|error| {
        panic!("should render a candidate SPDX for the real completed build: {error:?}")
    });
    assert!(
        !candidate.gaps().is_empty(),
        "should state at least one coverage gap"
    );
    assert_eq!(
        candidate.sha256(),
        hex::encode(Sha256::digest(candidate.bytes())),
        "should hash the exact candidate document bytes"
    );
    let document: Value =
        serde_json::from_slice(candidate.bytes()).expect("should parse the candidate document");
    crate::documents::validate_spdx(&document).expect("should satisfy the pinned SPDX schema");
    let packages = document["packages"]
        .as_array()
        .expect("should list candidate packages");
    let root = packages
        .iter()
        .find(|package| package["SPDXID"] == ROOT_SPDX_ID)
        .expect("should contain the executable root package");
    assert_eq!(
        root["checksums"],
        json!([{"algorithm": "SHA256", "checksumValue": completed.artifact().sha256()}]),
        "should bind the root package to the completed artifact SHA-256"
    );
    assert_eq!(
        packages.len(),
        1 + projected_component_count(completed),
        "should contain the root plus exactly the four projections' components"
    );
    let comment = document["creationInfo"]["comment"]
        .as_str()
        .expect("should carry a creation comment");
    assert!(
        comment.contains("candidate"),
        "should name the document a candidate"
    );
    for gap in candidate.gaps() {
        assert!(
            comment.contains(gap.as_str()),
            "should state gap {gap} in the creation comment"
        );
    }
    check_platform_gaps(completed, candidate.gaps());
    candidate
}

/// Sums the component counts of the four projections of one completed build.
fn projected_component_count(completed: &RecipeCompleted) -> usize {
    let rust = crate::spdx::cargo::CargoProjection::from_completed(completed)
        .expect("should project the real Rust packages");
    let native = crate::spdx::native::NativeProjection::from_completed(completed)
        .expect("should project the real native contributors");
    let javascript = crate::spdx::javascript::JavascriptProjection::from_completed(completed)
        .expect("should project the real JavaScript modules");
    let platform = crate::spdx::platform::PlatformProjection::from_completed(completed)
        .expect("should project the real platform facts");
    rust.component_count()
        + native.component_count()
        + javascript.component_count()
        + platform.component_count()
}

/// Requires the target family's platform gaps and no Apple gap elsewhere.
pub(super) fn check_platform_gaps(completed: &RecipeCompleted, gaps: &[String]) {
    let view = completed
        .composition_view()
        .expect("should expose the sealed composition view");
    let platform = view.platform();
    let target = completed.asset().artifact().target;
    let facts = PlatformGapFacts {
        unresolved_tool_roles: platform.unresolved_native_tool_roles(),
        apple_exclusions: platform.apple().map(AppleToolEvidence::coverage_exclusions),
        apple_closure_unmeasured: platform.apple_tool_closure_unmeasured(),
    };
    let required = required_platform_gaps(target, &facts).unwrap_or_else(|reason| {
        panic!("should fit {} platform evidence: {reason}", target.triple())
    });
    for gap in &required {
        assert!(
            gaps.contains(gap),
            "should state the {} platform gap {gap}",
            target.triple()
        );
    }
    if !target.is_darwin() {
        assert!(
            !gaps.iter().any(|gap| gap.starts_with("apple-")),
            "should state no Apple gap for {}",
            target.triple()
        );
    }
}

/// The platform facts a real candidate's gaps must restate.
struct PlatformGapFacts<'a> {
    unresolved_tool_roles: &'a [&'static str],
    apple_exclusions: Option<&'a [&'static str]>,
    apple_closure_unmeasured: bool,
}

/// Names the gaps one target family's candidate must state.
///
/// Fastly Wasm must state every unresolved native tool host path, the Linux
/// GNU CLI its unmeasured privileged helper contents, and Apple targets every
/// reviewed exclusion plus an unmeasured tool closure.
///
/// # Errors
///
/// Returns the reason when the facts lack what the target family records:
/// Fastly with no unresolved tool role, or Apple with no Apple evidence or no
/// Apple gap at all.
fn required_platform_gaps(
    target: ReleaseTarget,
    facts: &PlatformGapFacts<'_>,
) -> Result<Vec<String>, String> {
    match target {
        ReleaseTarget::Wasm32Wasip1 => {
            if facts.unresolved_tool_roles.is_empty() {
                return Err("Fastly evidence records no unresolved native tool role".to_owned());
            }
            Ok(facts
                .unresolved_tool_roles
                .iter()
                .map(|role| format!("native-tool-host-path-unresolved:{role}"))
                .collect())
        }
        ReleaseTarget::X86_64UnknownLinuxGnu => Ok(vec![GNU_PRIVILEGED_HELPER_GAP.to_owned()]),
        ReleaseTarget::Aarch64AppleDarwin | ReleaseTarget::X86_64AppleDarwin => {
            let exclusions = facts
                .apple_exclusions
                .ok_or_else(|| "Apple target records no Apple tool evidence".to_owned())?;
            let mut required = exclusions
                .iter()
                .map(|exclusion| format!("apple-coverage-excluded:{exclusion}"))
                .collect::<Vec<_>>();
            if facts.apple_closure_unmeasured {
                required.push(APPLE_TOOL_CLOSURE_GAP.to_owned());
            }
            if required.is_empty() {
                return Err(
                    "Apple evidence records neither exclusions nor an unmeasured closure"
                        .to_owned(),
                );
            }
            Ok(required)
        }
    }
}

/// The measured identity of one candidate document.
struct CandidateDocument<'a> {
    sha256: &'a str,
    bytes: &'a [u8],
    gaps: &'a [String],
}

impl<'a> From<&'a CandidateSpdx> for CandidateDocument<'a> {
    fn from(candidate: &'a CandidateSpdx) -> Self {
        Self {
            sha256: candidate.sha256(),
            bytes: candidate.bytes(),
            gaps: candidate.gaps(),
        }
    }
}

/// The printed measurement of a build's candidate SPDX documents.
pub(super) fn candidate_spdx_measurement(
    canonical: &CandidateSpdx,
    reproduction: Option<&CandidateSpdx>,
) -> Value {
    candidate_pair_measurement(
        &CandidateDocument::from(canonical),
        reproduction.map(CandidateDocument::from).as_ref(),
    )
}

/// Records both documents and whether their bytes match, without gating on it.
fn candidate_pair_measurement(
    canonical: &CandidateDocument<'_>,
    reproduction: Option<&CandidateDocument<'_>>,
) -> Value {
    let document = |candidate: &CandidateDocument<'_>| {
        json!({
            "sha256": candidate.sha256,
            "size": candidate.bytes.len(),
            "gaps": candidate.gaps,
        })
    };
    json!({
        "kind": "ordinary-ci-candidate-spdx",
        "canonical": document(canonical),
        "reproduction": reproduction.map(document),
        "byte_identical": reproduction.map(|reproduction| reproduction.bytes == canonical.bytes),
        "byte_identity_gate": false,
        "release_qualification": false,
    })
}

/// Completes the pure Fastly fixture build at the fixture commit.
fn completed_fixture() -> (Built, RecipeCompleted) {
    let bytes = wasm_with(&[(".debug_info", b"i"), (".debug_line", b"l")]);
    let mut build = built("trusted-server-fastly.wasm", Role::Canonical, &bytes);
    let stdout = build.stdout(|_| {});
    let completed = build
        .finish(stdout)
        .expect("should complete the accepted fixture artifact");
    (build, completed)
}

fn rejection(result: Result<CandidateSpdx, Report<ReleaseError>>) -> String {
    match result {
        Ok(_) => panic!("should reject the candidate"),
        Err(error) => format!("{error:?}"),
    }
}

#[test]
fn candidate_rejects_an_approved_source_at_another_commit() {
    let (_build, completed) = completed_fixture();
    let other = CommitId::parse(OTHER_COMMIT).expect("should parse the other commit");
    let source = ApprovedSource::in_memory_for_tests(&other, &[]);
    let error = rejection(candidate_from_completed(&completed, &source));
    assert!(
        error.contains("approved source differs from the completed build commit"),
        "should reject the source commit mismatch: {error}"
    );
}

#[test]
fn candidate_rejects_a_completion_without_retained_composition() {
    let (_build, completed) = completed_fixture();
    let commit = CommitId::parse(COMMIT).expect("should parse the fixture commit");
    let source = ApprovedSource::in_memory_for_tests(&commit, &[]);
    let error = rejection(candidate_from_completed(&completed, &source));
    assert!(
        !error.contains("approved source differs")
            && error.contains("completed build has no retained composition"),
        "should pass the commit check before rejecting the missing receipts: {error}"
    );
}

fn facts<'a>(
    unresolved_tool_roles: &'a [&'static str],
    apple_exclusions: Option<&'a [&'static str]>,
    apple_closure_unmeasured: bool,
) -> PlatformGapFacts<'a> {
    PlatformGapFacts {
        unresolved_tool_roles,
        apple_exclusions,
        apple_closure_unmeasured,
    }
}

#[test]
fn fastly_requires_every_unresolved_tool_role() {
    let required = required_platform_gaps(
        ReleaseTarget::Wasm32Wasip1,
        &facts(&["compiler", "linker"], None, false),
    )
    .expect("should accept unresolved Fastly tool roles");
    assert_eq!(
        required,
        [
            "native-tool-host-path-unresolved:compiler",
            "native-tool-host-path-unresolved:linker",
        ],
        "should require one gap per unresolved role"
    );
    assert!(
        required_platform_gaps(ReleaseTarget::Wasm32Wasip1, &facts(&[], None, false)).is_err(),
        "should reject Fastly evidence with no unresolved role"
    );
}

#[test]
fn linux_requires_the_gnu_privileged_helper_gap() {
    let required = required_platform_gaps(
        ReleaseTarget::X86_64UnknownLinuxGnu,
        &facts(&[], None, false),
    )
    .expect("should name the GNU gap");
    assert_eq!(
        required,
        [GNU_PRIVILEGED_HELPER_GAP],
        "should require the privileged helper gap"
    );
}

#[test]
fn apple_requires_exclusions_and_the_unmeasured_closure() {
    let required = required_platform_gaps(
        ReleaseTarget::Aarch64AppleDarwin,
        &facts(&[], Some(&["example-exclusion"]), true),
    )
    .expect("should accept Apple evidence");
    assert_eq!(
        required,
        [
            "apple-coverage-excluded:example-exclusion",
            APPLE_TOOL_CLOSURE_GAP
        ],
        "should require every exclusion and the closure gap"
    );
    assert!(
        required_platform_gaps(ReleaseTarget::X86_64AppleDarwin, &facts(&[], None, true)).is_err(),
        "should reject an Apple target without Apple evidence"
    );
    assert!(
        required_platform_gaps(
            ReleaseTarget::X86_64AppleDarwin,
            &facts(&[], Some(&[]), false)
        )
        .is_err(),
        "should reject Apple evidence that states no gap"
    );
}

#[test]
fn candidate_measurement_records_identity_without_gating() {
    let gaps = vec!["example-gap".to_owned()];
    let canonical = CandidateDocument {
        sha256: "a",
        bytes: b"{}",
        gaps: &gaps,
    };
    let differing = CandidateDocument {
        sha256: "b",
        bytes: b"{ }",
        gaps: &gaps,
    };
    let identical = candidate_pair_measurement(&canonical, Some(&canonical));
    assert_eq!(
        identical["byte_identical"],
        json!(true),
        "should record identical bytes"
    );
    assert_eq!(
        identical["canonical"],
        json!({"sha256": "a", "size": 2, "gaps": ["example-gap"]}),
        "should record digest, size and gaps"
    );
    let differs = candidate_pair_measurement(&canonical, Some(&differing));
    assert_eq!(
        differs["byte_identical"],
        json!(false),
        "should record differing bytes"
    );
    assert_eq!(
        differs["byte_identity_gate"],
        json!(false),
        "should not gate on byte identity"
    );
    let alone = candidate_pair_measurement(&canonical, None);
    assert_eq!(
        (
            alone["reproduction"].clone(),
            alone["byte_identical"].clone()
        ),
        (Value::Null, Value::Null),
        "should record no comparison without a reproduction"
    );
    assert_eq!(
        alone["release_qualification"],
        json!(false),
        "should grant no qualification"
    );
}
