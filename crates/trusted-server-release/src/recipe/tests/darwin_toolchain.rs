//! In-place Apple toolchain identity: helpers, `libLTO` and the clang
//! resource directory bound into the Apple record and its exclusions.

use std::fs;
use std::os::unix::fs::symlink;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::fixture::*;
use super::*;
use crate::process::Cancellation;

/// The fictional toolchain `usr` directory, with its resources and `libLTO`.
fn write_fictional_toolchain(fixture: &Fixture) -> PathBuf {
    let usr = fixture
        .developer_dir()
        .join(TOOLCHAIN_BIN)
        .parent()
        .expect("should have a toolchain usr directory")
        .to_owned();
    fs::create_dir_all(usr.join("lib/clang/17/include"))
        .expect("should create fictional resource directory");
    fs::write(
        usr.join("lib/clang/17/include/stddef.h"),
        b"fictional header",
    )
    .expect("should write fictional resource header");
    symlink("17", usr.join("lib/clang/17.0.0")).expect("should alias resource version");
    fs::write(usr.join("lib/libLTO.dylib"), b"fictional libLTO")
        .expect("should write fictional libLTO");
    symlink("clang", usr.join("bin/clang++")).expect("should alias clang++");
    usr
}

fn with_toolchain(apple: AppleDeveloperTools) -> Result<AppleDeveloperTools, Report<ReleaseError>> {
    apple.with_measured_toolchain_for_tests(
        Instant::now() + Duration::from_secs(60),
        &Cancellation::default(),
    )
}

fn exclusions(apple: &AppleDeveloperTools) -> Vec<&'static str> {
    apple
        .candidate_measurement()
        .expect("should export Apple evidence")
        .coverage_exclusions()
        .to_vec()
}

#[test]
fn measured_toolchain_removes_exactly_its_exclusions_and_keeps_honest_gaps() {
    let fixture = Fixture::new(COMMIT);
    write_fictional_toolchain(&fixture);
    let lexical = fixture.apple_tools();
    let measured = with_toolchain(fixture.apple_tools()).expect("should measure toolchain");
    assert_eq!(
        exclusions(&measured),
        [
            "sdk_headers",
            "sdk_stubs",
            "sdk_frameworks",
            "toolchain_libraries"
        ],
        "should drop only helper, libLTO and resource exclusions"
    );
    let detached = AppleDeveloperTools::with_detached_sdk_for_tests(
        fixture.apple_facts(),
        Instant::now() + Duration::from_secs(60),
        &Cancellation::default(),
    )
    .expect("should stage selected SDK");
    let complete = with_toolchain(detached).expect("should measure toolchain with SDK");
    assert_eq!(
        exclusions(&complete),
        ["toolchain_libraries"],
        "should keep unmeasured toolchain libraries excluded"
    );
    let evidence = measured
        .candidate_measurement()
        .expect("should export measured toolchain evidence");
    assert_eq!(
        evidence.liblto_sha256(),
        Some(hex::encode(Sha256::digest(b"fictional libLTO")).as_str()),
        "should export the libLTO digest"
    );
    assert!(
        evidence.clang_resource_tree_sha256().is_some() && evidence.helper_tree_sha256().is_some(),
        "should export both tree digests"
    );
    let lexical_evidence = lexical
        .candidate_measurement()
        .expect("should export lexical evidence");
    assert!(
        lexical_evidence.liblto_sha256().is_none()
            && lexical_evidence.clang_resource_tree_sha256().is_none()
            && lexical_evidence.helper_tree_sha256().is_none(),
        "should never claim toolchain coverage for lexical facts"
    );
    assert_ne!(
        lexical.identity_sha256().expect("should identify lexical"),
        measured
            .identity_sha256()
            .expect("should identify measured"),
        "should bind the toolchain receipt into the identity record"
    );
    assert_eq!(
        measured.measurement()["toolchain"]["clang_resource_version"],
        "17",
        "should record the selected resource version"
    );
}

#[test]
fn identity_changes_and_verify_fails_when_any_toolchain_input_changes() {
    type Mutation = fn(&std::path::Path);
    let mutations: [(&str, Mutation); 4] = [
        ("resource header", |usr| {
            fs::write(usr.join("lib/clang/17/include/stddef.h"), b"edited")
                .expect("should edit resource header");
        }),
        ("libLTO", |usr| {
            fs::write(usr.join("lib/libLTO.dylib"), b"edited").expect("should edit libLTO");
        }),
        ("added helper", |usr| {
            fs::write(usr.join("bin/dsymutil"), b"fictional helper").expect("should add helper");
        }),
        ("retargeted helper alias", |usr| {
            fs::remove_file(usr.join("bin/clang++")).expect("should remove alias");
            symlink("ld", usr.join("bin/clang++")).expect("should retarget alias");
        }),
    ];
    for (label, mutate) in mutations {
        let fixture = Fixture::new(COMMIT);
        let usr = write_fictional_toolchain(&fixture);
        let original = with_toolchain(fixture.apple_tools()).expect("should measure toolchain");
        let identity = original
            .identity_sha256()
            .expect("should identify original");
        mutate(&usr);
        assert!(
            original.verify().is_err(),
            "should reject changed {label} at verification"
        );
        assert!(
            original.candidate_measurement().is_err(),
            "should reject changed {label} at evidence sealing"
        );
        let remeasured =
            with_toolchain(fixture.apple_tools()).expect("should remeasure changed toolchain");
        assert_ne!(
            remeasured
                .identity_sha256()
                .expect("should identify changed"),
            identity,
            "should change Apple identity when {label} changes"
        );
    }
}

#[test]
fn toolchain_selection_fails_closed_without_liblto_or_with_ambiguous_resources() {
    let fixture = Fixture::new(COMMIT);
    let usr = write_fictional_toolchain(&fixture);
    fs::remove_file(usr.join("lib/libLTO.dylib")).expect("should remove libLTO");
    assert!(
        with_toolchain(fixture.apple_tools()).is_err(),
        "should reject a toolchain without libLTO"
    );
    let fixture = Fixture::new(COMMIT);
    let usr = write_fictional_toolchain(&fixture);
    fs::create_dir(usr.join("lib/clang/18")).expect("should add second resource version");
    assert!(
        with_toolchain(fixture.apple_tools()).is_err(),
        "should reject two clang resource versions"
    );
    let fixture = Fixture::new(COMMIT);
    assert!(
        with_toolchain(fixture.apple_tools()).is_err(),
        "should reject a toolchain without measured resources"
    );
}
