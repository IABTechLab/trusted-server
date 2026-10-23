use std::fs;
use std::os::unix::fs::{PermissionsExt as _, symlink};

use super::fixture::*;
use super::*;
use crate::boundary::DescendantCleanup;
use crate::policy::FIXED_ARTIFACTS;

#[test]
fn candidate_tool_evidence_records_measured_tree_and_selected_relative_roles() {
    let fixture = Fixture::new(COMMIT);
    let tools = fixture.tools_for(ReleaseTarget::Wasm32Wasip1);
    let evidence = tools
        .candidate_measurement()
        .expect("should measure selected tools without granting release authority");
    let value = serde_json::to_value(evidence).expect("should serialize candidate tool evidence");
    assert_eq!(
        value["tree_sha256"],
        tools.digest().expect("should hash tools"),
        "should bind the whole selected Rust tree"
    );
    assert_eq!(
        value["cargo"], "bin/cargo",
        "should identify the selected Cargo role"
    );
    assert_eq!(
        value["rustc"], "bin/rustc",
        "should identify the selected compiler role"
    );
    assert_eq!(
        value["target_standard_library"], "lib/rustlib/wasm32-wasip1/lib",
        "should identify the selected target sysroot libraries"
    );
    assert_eq!(
        value["identity_scope"], "selected_tool_tree",
        "should state measured identity scope"
    );
    assert_eq!(
        value["origin"], "candidate_measurement",
        "should not claim original acquisition provenance"
    );
    assert_eq!(
        value["qualification"], "unqualified",
        "should preserve candidate authority"
    );
}

#[test]
fn candidate_tool_measurement_rechecks_selected_layout() {
    let fixture = Fixture::new(COMMIT);
    let tools = fixture.tools_for(ReleaseTarget::Wasm32Wasip1);
    fs::remove_file(fixture.base.join("tools/bin/rustc")).expect("should remove selected compiler");
    assert!(
        tools.candidate_measurement().is_err(),
        "should reject a missing selected compiler"
    );
}

fn rejection(fixture: &Fixture, target: ReleaseTarget) -> String {
    match SelectedRustTools::for_tests(&fixture.base.join("tools"), target) {
        Ok(_) => panic!("should reject the tools"),
        Err(error) => format!("{error:?}"),
    }
}

#[test]
fn every_asset_target_needs_its_own_standard_library_directory() {
    assert!(
        !FIXED_ARTIFACTS.is_empty(),
        "should select a nonzero number of targets"
    );
    for artifact in &FIXED_ARTIFACTS {
        let fixture = Fixture::new(COMMIT);
        let tools = fixture.tools_for(artifact.target);
        assert_eq!(
            tools.target(),
            artifact.target,
            "should record the checked target"
        );
        let standard = fixture
            .base
            .join("tools/lib/rustlib")
            .join(artifact.target.triple())
            .join("lib");
        fs::remove_dir_all(&standard).expect("should remove the target standard library");
        assert!(
            rejection(&fixture, artifact.target).contains("standard library"),
            "should reject {} without its standard library directory",
            artifact.target.triple()
        );
    }
}

#[test]
fn other_targets_standard_library_does_not_satisfy_the_asset_target() {
    let fixture = Fixture::new(COMMIT);
    fs::remove_dir_all(
        fixture
            .base
            .join("tools/lib/rustlib/aarch64-apple-darwin/lib"),
    )
    .expect("should remove one target");
    SelectedRustTools::for_tests(
        &fixture.base.join("tools"),
        ReleaseTarget::X86_64UnknownLinuxGnu,
    )
    .expect("should accept a target whose library is present");
    assert!(
        rejection(&fixture, ReleaseTarget::Aarch64AppleDarwin).contains("standard library"),
        "should reject the target whose library is absent although others are present"
    );
}

#[test]
fn standard_library_directory_must_hold_a_libstd_file() {
    let fixture = Fixture::new(COMMIT);
    let standard = fixture.base.join("tools/lib/rustlib/wasm32-wasip1/lib");
    fs::remove_file(standard.join("libstd-0123456789abcdef.rlib")).expect("should remove libstd");
    assert!(
        rejection(&fixture, ReleaseTarget::Wasm32Wasip1).contains("libstd"),
        "should reject an empty standard library directory"
    );
    fs::create_dir(standard.join("libstd-subdirectory")).expect("should add a directory");
    assert!(
        rejection(&fixture, ReleaseTarget::Wasm32Wasip1).contains("libstd"),
        "should reject a libstd-named directory"
    );
    fs::write(standard.join("libcore-0123.rlib"), b"fictional").expect("should add libcore");
    assert!(
        rejection(&fixture, ReleaseTarget::Wasm32Wasip1).contains("libstd"),
        "should reject a directory with only other libraries"
    );
}

#[test]
fn symlinked_or_missing_tool_layout_is_rejected() {
    let fixture = Fixture::new(COMMIT);
    let standard = fixture.base.join("tools/lib/rustlib/wasm32-wasip1/lib");
    fs::rename(&standard, fixture.base.join("real-std")).expect("should move the directory");
    symlink(fixture.base.join("real-std"), &standard).expect("should alias the directory");
    assert!(
        rejection(&fixture, ReleaseTarget::Wasm32Wasip1).contains("symlink"),
        "should reject a symlinked standard library directory"
    );
    let fixture = Fixture::new(COMMIT);
    fs::remove_file(fixture.base.join("tools/bin/rustc")).expect("should remove rustc");
    assert!(
        rejection(&fixture, ReleaseTarget::Wasm32Wasip1).contains("Rust tool"),
        "should reject missing rustc"
    );
}

#[test]
fn tool_digest_detects_edits_hard_links_and_escaping_aliases() {
    let fixture = Fixture::new(COMMIT);
    let tools = fixture.tools_for(ReleaseTarget::Aarch64AppleDarwin);
    let baseline = tools.digest().expect("should digest the tools");
    assert_eq!(
        baseline,
        tools.digest().expect("should digest again"),
        "should be stable"
    );

    fs::write(
        fixture.base.join("tools/bin/cargo"),
        b"fictional tool, edited",
    )
    .expect("should edit cargo");
    let edited = tools.digest().expect("should digest the edited tools");
    assert_ne!(baseline, edited, "should detect an edited tool");
    fs::write(fixture.base.join("tools/bin/cargo"), b"fictional tool")
        .expect("should restore cargo");
    assert_eq!(
        baseline,
        tools.digest().expect("should digest restored tools"),
        "should return to the baseline"
    );

    fs::set_permissions(
        fixture.base.join("tools/bin/rustc"),
        fs::Permissions::from_mode(0o700),
    )
    .expect("should chmod rustc");
    assert_ne!(
        baseline,
        tools.digest().expect("should digest chmod tools"),
        "should detect a mode change"
    );
    fs::set_permissions(
        fixture.base.join("tools/bin/rustc"),
        fs::Permissions::from_mode(0o644),
    )
    .expect("should restore mode");

    fs::hard_link(
        fixture.base.join("tools/bin/cargo"),
        fixture.base.join("tools/bin/cargo-link"),
    )
    .expect("should hard-link cargo");
    assert!(tools.digest().is_err(), "should reject a hard-linked tool");
    fs::remove_file(fixture.base.join("tools/bin/cargo-link")).expect("should remove the link");

    symlink("../../other-work", fixture.base.join("tools/escape"))
        .expect("should add escaping alias");
    assert!(
        tools.digest().is_err(),
        "should reject an alias escaping the tool root"
    );
}

#[test]
fn tools_edited_after_binding_are_rejected_at_the_execute_gate() {
    let fixture = Fixture::new(COMMIT);
    let prepared = fixture
        .bind("ts-aarch64-apple-darwin", Role::Canonical)
        .expect("should bind the recipe");
    prepared
        .check_tools()
        .expect("should accept the untouched tools");
    prepared
        .check_materials()
        .expect("should accept the untouched materials");
    fs::write(fixture.base.join("tools/bin/rustc"), b"swapped compiler")
        .expect("should swap rustc");
    let error = prepared
        .check_tools()
        .expect_err("should reject the swapped tools");
    assert!(
        format!("{error:?}").contains("Rust tools changed"),
        "should name the changed tools"
    );
    fs::write(fixture.base.join("tools/bin/rustc"), b"fictional tool")
        .expect("should restore rustc");
    prepared
        .check_tools()
        .expect("should accept the restored tools");
    fs::write(
        fixture
            .base
            .join("materials/vendor/example-1.0.0/Cargo.toml"),
        b"swapped dependency",
    )
    .expect("should swap a vendored file");
    assert!(
        format!(
            "{:?}",
            prepared
                .check_materials()
                .expect_err("should reject swapped materials")
        )
        .contains("changed after binding"),
        "should name the changed materials"
    );
}

#[test]
fn cargo_registry_bookkeeping_directory_is_not_a_fetched_source_cache() {
    let fixture = Fixture::new(COMMIT);
    let prepared = fixture
        .bind("ts-aarch64-apple-darwin", Role::Canonical)
        .expect("should bind the recipe");
    fs::create_dir(fixture.base.join("cargo-state/cargo-home/registry"))
        .expect("should create the registry bookkeeping directory");
    prepared
        .check_no_fetched_sources()
        .expect("should accept the empty registry directory created before source replacement");
}

#[test]
fn handoff_edits_and_cargo_home_caches_are_rejected_around_the_run() {
    let fixture = Fixture::new(COMMIT);
    let retained = fixture.retained();
    let prepared = fixture
        .bind("ts-aarch64-apple-darwin", Role::Canonical)
        .expect("should bind the recipe");
    prepared
        .check_inputs()
        .expect("should accept the untouched handoff, tools and materials");
    fs::write(retained.join("bundles/tsjs-core.js"), b"tampered")
        .expect("should edit a retained module");
    assert!(
        format!(
            "{:?}",
            prepared
                .check_inputs()
                .expect_err("should reject the edited handoff")
        )
        .contains("retained JS handoff changed"),
        "should name the changed handoff"
    );
    fs::write(retained.join("bundles/tsjs-core.js"), b"fictional module")
        .expect("should restore the retained module");
    prepared
        .check_inputs()
        .expect("should accept the restored handoff");

    prepared
        .check_no_fetched_sources()
        .expect("should accept a Cargo home without caches");
    for cache in ["registry", "git"] {
        let path = fixture.base.join("cargo-state/cargo-home").join(cache);
        fs::create_dir(&path).expect("should create a Cargo cache");
        if cache == "registry" {
            fs::create_dir(path.join("index")).expect("should create fetched registry state");
        }
        assert!(
            format!(
                "{:?}",
                prepared
                    .check_no_fetched_sources()
                    .expect_err("should reject a Cargo cache")
            )
            .contains(&format!("holds a {cache} cache")),
            "should name the {cache} cache"
        );
        fs::remove_dir_all(&path).expect("should remove the Cargo cache");
    }
}

#[test]
fn only_the_ordinary_ci_selection_permits_a_candidate_run() {
    let fixture = Fixture::new(COMMIT);
    let tools = fixture.tools_for(ReleaseTarget::Wasm32Wasip1);
    assert!(
        tools.permits_candidate_run(),
        "should let the ordinary CI selection start a candidate run"
    );
    assert_eq!(
        tools.authority(),
        Authority::Unqualified,
        "should still carry no release authority"
    );
}

#[test]
fn cleanup_gate_admits_reaped_cleanup_only_with_the_allowance() {
    let cases = [
        (DescendantCleanup::OwnedByContainer, false, true),
        (DescendantCleanup::OwnedByContainer, true, true),
        (DescendantCleanup::ReapedAfterRun, true, true),
        (DescendantCleanup::ReapedAfterRun, false, false),
    ];
    for (cleanup, allowance, admitted) in cases {
        let result = super::super::execute::cleanup_gate(cleanup, allowance);
        assert_eq!(
            result.is_ok(),
            admitted,
            "should decide {cleanup} with allowance {allowance}"
        );
        if let Err(error) = result {
            assert!(
                format!("{error:?}")
                    .contains("post-run-reaped cleanup is not permitted for this selection"),
                "should name the refused cleanup: {error:?}"
            );
        }
    }
}

#[test]
fn only_the_ordinary_ci_selection_permits_post_run_reaped_cleanup() {
    let fixture = Fixture::new(COMMIT);
    for artifact in &FIXED_ARTIFACTS {
        assert!(
            fixture
                .tools_for(artifact.target)
                .permits_post_run_reaped_cleanup(),
            "should let the ordinary CI selection accept reaped cleanup for {}",
            artifact.name
        );
    }
    assert!(
        ToolSelection::OrdinaryCi.permits_post_run_reaped_cleanup(),
        "should allow the cfg(test) ordinary CI selection"
    );
    // The qualified arm cannot be reached: no qualified selection exists, so
    // Task 8 must add an explicit decision before production can pass.
    let never: fn(QualifiedRustTools) -> ! = |qualification| match qualification {};
    let qualified: fn(QualifiedRustTools) -> ToolSelection = ToolSelection::Qualified;
    let _ = (never, qualified);
}
