//! Completed-build wiring rejections of a candidate index entry.
//!
//! A pure completion fixture has no composition receipts and no actual
//! runtime or startup acceptance, so these tests reach only the owner checks
//! that run before acceptance binding. Successful entries require the real
//! completed candidate builds.

use super::complete::{built, wasm_with};
use super::fixture::OTHER_COMMIT;
use super::*;
use crate::distribution::{AcceptanceKind, check_candidate_owner};
use crate::inspect::tests::builders::{ElfSpec, elf};

const FASTLY: &str = "trusted-server-fastly.wasm";
const LINUX: &str = "ts-x86_64-unknown-linux-gnu";

fn completed(asset: &'static str, role: Role) -> RecipeCompleted {
    let bytes = if asset == FASTLY {
        wasm_with(&[(".debug_info", b"i"), (".debug_line", b"l")])
    } else {
        elf(&ElfSpec {
            payload: b"/trusted-server/src/main.rs",
            ..ElfSpec::conforming()
        })
    };
    let mut build = built(asset, role, &bytes);
    let stdout = build.stdout(|_| {});
    build
        .finish(stdout)
        .expect("should complete the accepted fixture artifact")
}

fn source(commit: &str) -> ApprovedSource {
    let commit = CommitId::parse(commit).expect("should parse the fixture commit");
    ApprovedSource::in_memory_for_tests(&commit, &[])
}

fn rejection(result: Result<(), Report<ReleaseError>>) -> String {
    format!(
        "{:?}",
        result.expect_err("should reject the candidate owner")
    )
}

#[test]
fn candidate_owner_accepts_the_canonical_build_with_its_acceptance_family() {
    check_candidate_owner(
        &completed(FASTLY, Role::Canonical),
        &source(COMMIT),
        AcceptanceKind::FastlyRuntime,
    )
    .expect("should accept the canonical Fastly owner");
    check_candidate_owner(
        &completed(LINUX, Role::Canonical),
        &source(COMMIT),
        AcceptanceKind::NativeStartup,
    )
    .expect("should accept the canonical native owner");
}

#[test]
fn candidate_owner_rejects_a_reproduction_build() {
    let error = rejection(check_candidate_owner(
        &completed(FASTLY, Role::Reproduction),
        &source(COMMIT),
        AcceptanceKind::FastlyRuntime,
    ));
    assert!(
        error.contains("only from the canonical build"),
        "should reject the reproduction role: {error}"
    );
}

#[test]
fn candidate_owner_rejects_the_wrong_acceptance_family() {
    let fastly = rejection(check_candidate_owner(
        &completed(FASTLY, Role::Canonical),
        &source(COMMIT),
        AcceptanceKind::NativeStartup,
    ));
    assert!(
        fastly.contains("requires FastlyRuntime acceptance"),
        "should reject native acceptance for Fastly: {fastly}"
    );
    let native = rejection(check_candidate_owner(
        &completed(LINUX, Role::Canonical),
        &source(COMMIT),
        AcceptanceKind::FastlyRuntime,
    ));
    assert!(
        native.contains("requires NativeStartup acceptance"),
        "should reject Fastly acceptance for a native CLI: {native}"
    );
}

#[test]
fn candidate_owner_rejects_a_source_at_another_commit() {
    let error = rejection(check_candidate_owner(
        &completed(FASTLY, Role::Canonical),
        &source(OTHER_COMMIT),
        AcceptanceKind::FastlyRuntime,
    ));
    assert!(
        error.contains("approved source differs from the completed build commit"),
        "should reject the mismatched source: {error}"
    );
}
