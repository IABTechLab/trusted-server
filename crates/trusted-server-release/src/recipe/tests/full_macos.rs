//! Explicit real macOS Cargo fixtures; never selected by a normal focused
//! unit run.
//!
//! Both runs are ordinary CI candidate evidence (`release_qualification:
//! false`): the Xcode, toolchain and fixture acquisition are unqualified and
//! unauthenticated, and the unqualified `sandbox-exec` boundary only reaps
//! descendants after each run (`post-run-reaped`), which only the test-only
//! ordinary CI tool selection admits. Nothing here qualifies a release.
//!
//! A Darwin build's Rust tool root and target directory must be direct
//! children of one parent, which the linker strips from every debug-map
//! object path. These fixtures use the Cargo fixture root as that parent:
//! the target directory is `<fixture>/target`, beside `<fixture>/tools`, so
//! builds run one at a time and remove it afterwards.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::json;

use super::candidate::{
    CargoFixture, FixtureHost, ci_build_provenance, denial_measurement, reported_errno,
};
use super::macos_candidate::darwin_cli_asset;
use super::real_fixture::{ATTEMPTS, DenialCase, DenialSelection, bind_denial_case, cargo_fixture};
use super::*;
use crate::boundary::{Boundary, DenialProof, DescendantCleanup, ProbeProgram};
use crate::process::{Cancellation, ProcessLimits};

/// Reads the ordinary CI Cargo fixture and requires its macOS kind.
pub(super) fn macos_cargo_fixture() -> CargoFixture {
    let fixture = cargo_fixture();
    assert_eq!(
        fixture.host,
        FixtureHost::MacosSandbox,
        "should run on a macOS Cargo fixture that names no image"
    );
    fixture
}

/// The Darwin `ts` asset of the running producer's architecture, which must
/// build for the `HOST_TARGET` the CI job asserted.
pub(super) fn job_host_asset() -> AssetId {
    let host_target =
        std::env::var("HOST_TARGET").expect("should name the HOST_TARGET the job asserted");
    darwin_cli_asset(std::env::consts::ARCH, &host_target)
        .expect("should select the Darwin ts asset of the host architecture")
}

/// The target directory beside the fixture's tool root.
pub(super) fn darwin_target_dir(fixture: &CargoFixture) -> PathBuf {
    fixture.root.join("target")
}

/// Removes a spent Darwin target directory; an absent one is already gone.
///
/// # Errors
///
/// Returns [`ReleaseError::Recipe`] when the directory cannot be removed.
pub(super) fn remove_target_dir(target: &Path) -> Result<(), Report<ReleaseError>> {
    match fs::remove_dir_all(target) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(recipe_error(format!(
            "cannot remove the Darwin target directory: {error}"
        ))),
    }
}

/// A configured macOS sandbox boundary with its proof for this instance.
pub(super) struct ProvenSandbox {
    pub(super) boundary: Boundary,
    pub(super) proof: DenialProof,
}

/// Configures the exact recipe boundary as an unqualified macOS sandbox and
/// proves its denial with the staged producer probe.
///
/// # Errors
///
/// Returns an error when the boundary cannot be configured or proven, the
/// proof does not report the sandbox `EPERM`, or the boundary does not
/// report post-run-reaped descendant cleanup.
pub(super) fn try_proven_sandbox(
    prepared: &PreparedRecipe,
    fixture: &CargoFixture,
) -> Result<ProvenSandbox, Report<ReleaseError>> {
    let boundary = Boundary::mac_sandbox_for_tests(prepared.boundary_roots()?)?;
    if boundary.descendant_cleanup() != DescendantCleanup::ReapedAfterRun {
        return Err(recipe_error(
            "the macOS sandbox does not report post-run-reaped cleanup",
        ));
    }
    let probe = ProbeProgram::from_staged_release_binary(&fixture.probe())?;
    let limits = ProcessLimits {
        wall: Duration::from_secs(60),
        stdout_max: 4096,
        stderr_max: 4096,
    };
    let proof = boundary.establish_denial(&probe, limits)?;
    if proof.denied_errno() != libc::EPERM {
        return Err(recipe_error(
            "denial proof does not establish the sandbox EPERM denial",
        ));
    }
    Ok(ProvenSandbox { boundary, proof })
}

/// Binds one Darwin denial fixture member for `target` with the Xcode tools.
fn bind_darwin_denial_case(
    fixture: &CargoFixture,
    member: &str,
    target: ReleaseTarget,
    apple: &AppleDeveloperTools,
) -> DenialCase {
    bind_denial_case(
        fixture,
        member,
        DenialSelection {
            asset: AssetId::darwin_denial_fixture(member, target)
                .expect("should resolve the Darwin fixture member"),
            tools: SelectedRustTools::for_tests(&fixture.tools(), target)
                .expect("should select the ordinary CI Rust tools")
                .with_apple_developer_tools(apple.clone()),
            target_dir: Some(darwin_target_dir(fixture)),
        },
    )
}

#[test]
#[ignore = "requires the explicit ordinary CI macOS Cargo fixture, Xcode and the macOS sandbox; run separately in producer CI"]
fn cargo_children_inherit_denial_macos() {
    let fixture = macos_cargo_fixture();
    let target = job_host_asset().artifact().target;
    let cancellation = Cancellation::default();
    let apple = AppleDeveloperTools::for_ordinary_ci(
        Instant::now() + Duration::from_secs(60 * 60),
        &cancellation,
    )
    .expect("should measure the selected Xcode");
    let target_dir = darwin_target_dir(&fixture);
    let mut observed = BTreeMap::new();
    for case in ATTEMPTS {
        let bound = bind_darwin_denial_case(&fixture, case, target, &apple);
        let proven = try_proven_sandbox(&bound.prepared, &fixture)
            .unwrap_or_else(|error| panic!("should prove the {case} sandbox: {error:?}"));
        let error = bound
            .prepared
            .execute(&proven.boundary, &proven.proof)
            .expect_err("should fail a build that attempts a connection");
        remove_target_dir(&target_dir).expect("should remove the failed case's target");
        let text = format!("{error:?}");
        let reported = reported_errno(case, &text)
            .unwrap_or_else(|| panic!("should report the {case} attempt errno: {text}"));
        assert_eq!(
            reported,
            proven.proof.denied_errno(),
            "should deny the {case} attempt with the proven sandbox errno: {text}"
        );
        assert_eq!(
            reported,
            libc::EPERM,
            "should deny {case} with the sandbox EPERM: {text}"
        );
        assert_ne!(
            reported,
            libc::ECONNREFUSED,
            "should deny {case} before any listener refuses it"
        );
        observed.insert(case.to_owned(), reported);
    }
    let bound = bind_darwin_denial_case(&fixture, "control", target, &apple);
    let proven = try_proven_sandbox(&bound.prepared, &fixture)
        .unwrap_or_else(|error| panic!("should prove the control sandbox: {error:?}"));
    let control = bound.prepared.execute(&proven.boundary, &proven.proof);
    remove_target_dir(&target_dir).expect("should remove the control target");
    let control = control.unwrap_or_else(|error| {
        panic!("should build and accept the Darwin control through the confined recipe: {error:?}")
    });
    control
        .rehash_for_handoff()
        .expect("should rehash the control artifact before reporting it");
    assert_eq!(
        control.asset().artifact().name,
        format!("denial-control-{}", target.triple()),
        "should copy the control under its Darwin row name"
    );
    assert_eq!(
        control.descendant_cleanup(),
        DescendantCleanup::ReapedAfterRun,
        "should record the post-run-reaped cleanup"
    );
    let mut measurement = denial_measurement(libc::EPERM, &observed, &control);
    measurement["kind"] = json!("ordinary-ci-macos-cargo-denial");
    measurement["target"] = json!(target.triple());
    measurement["boundary"] = json!("macos-sandbox-exec");
    measurement["descendant_cleanup"] = json!(control.descendant_cleanup().to_string());
    measurement["apple_developer_tools"] = apple.measurement();
    measurement["ci_build_provenance"] = ci_build_provenance(&fixture);
    let text = serde_json::to_string_pretty(&measurement).expect("should encode the measurement");
    println!("{text}");
    fs::write(fixture.root.join("cargo-denial-measurement.json"), text)
        .expect("should retain the macOS denial measurement");
}
