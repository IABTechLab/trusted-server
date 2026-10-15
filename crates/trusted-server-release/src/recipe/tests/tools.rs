use std::fs;
use std::os::unix::fs::{PermissionsExt as _, symlink};

use super::fixture::*;
use super::*;
use crate::policy::FIXED_ARTIFACTS;

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
        .bind("ts-aarch64-apple-darwin", Role::A)
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
