use std::fs;

use super::fixture::*;
use super::*;
use crate::boundary::BoundaryRoots;
#[cfg(target_os = "macos")]
use crate::boundary::{Boundary, DenialProof};

#[test]
fn js_handoff_for_other_source_commit_is_rejected() {
    Fixture::new(COMMIT)
        .bind("ts-aarch64-apple-darwin", Role::Canonical)
        .expect("should bind the matching handoff");
    let other = Fixture::new(OTHER_COMMIT);
    assert!(
        bind_error(other.bind("ts-aarch64-apple-darwin", Role::Canonical))
            .contains("another source commit"),
        "should reject a JS handoff built from another commit"
    );
    assert!(
        !other.base.join("target").exists(),
        "should not create the target after a rejection"
    );
    let fixture = Fixture::new(COMMIT);
    let request = fixture.request(
        "ts-aarch64-apple-darwin",
        Role::Canonical.spec_label(),
        OTHER_COMMIT,
    );
    assert!(
        bind_error(fixture.bind_with(request, Role::Canonical, COMMIT))
            .contains("another source commit"),
        "should reject a request naming another commit"
    );
}

#[test]
fn js_handoff_mutated_after_completion_is_rejected() {
    let changed = Fixture::new(COMMIT);
    let request = changed.request(
        "ts-aarch64-apple-darwin",
        Role::Canonical.spec_label(),
        COMMIT,
    );
    fs::write(changed.retained().join("bundles/tsjs-core.js"), b"tampered")
        .expect("should mutate a retained module");
    assert!(
        bind_error(changed.bind_with(request, Role::Canonical, COMMIT))
            .contains("no longer rehashes"),
        "should reject a mutated retained module"
    );
    let extra = Fixture::new(COMMIT);
    let request = extra.request(
        "ts-aarch64-apple-darwin",
        Role::Canonical.spec_label(),
        COMMIT,
    );
    fs::write(extra.retained().join("unrecorded"), b"extra").expect("should add an entry");
    assert!(
        bind_error(extra.bind_with(request, Role::Canonical, COMMIT))
            .contains("no longer rehashes"),
        "should reject an unrecorded retained entry"
    );
    assert!(
        !changed.base.join("target").exists(),
        "should not create the target after a rejection"
    );
}

#[test]
fn request_for_other_js_directory_or_role_is_rejected() {
    let fixture = Fixture::new(COMMIT);
    let foreign = Fixture::new(COMMIT);
    let request = foreign.request(
        "ts-aarch64-apple-darwin",
        Role::Canonical.spec_label(),
        COMMIT,
    );
    assert!(
        bind_error(fixture.bind_with(request, Role::Canonical, COMMIT)).contains("does not name"),
        "should reject a request naming another JS handoff directory"
    );
    let request = fixture.request(
        "ts-aarch64-apple-darwin",
        Role::Reproduction.spec_label(),
        COMMIT,
    );
    assert!(
        bind_error(fixture.bind_with(request, Role::Canonical, COMMIT)).contains("ambient role"),
        "should reject an ambient of another role"
    );
}

#[test]
fn bind_creates_fresh_target_and_private_cargo_state() {
    let fixture = Fixture::new(COMMIT);
    fixture
        .bind("ts-aarch64-apple-darwin", Role::Canonical)
        .expect("should bind the recipe");
    for relative in [
        "target",
        "cargo-state/home",
        "cargo-state/cargo-home",
        "cargo-state/tmp",
    ] {
        assert!(
            fixture.base.join(relative).is_dir(),
            "should create {relative}"
        );
    }
    assert_eq!(
        fs::read_dir(fixture.base.join("output"))
            .expect("should list output")
            .count(),
        0,
        "should leave the unmounted output root empty"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn execute_rejects_boundary_with_other_roots() {
    let fixture = Fixture::new(COMMIT);
    let prepared = fixture
        .bind("ts-aarch64-apple-darwin", Role::Canonical)
        .expect("should bind the recipe");
    let other = BoundaryRoots::new(
        &[fixture.base.join("other-source")],
        &[fixture.base.join("tools")],
        &[fixture.base.join("other-work")],
    )
    .expect("should build unrelated roots");
    let boundary = Boundary::mac_sandbox_for_tests(other).expect("should configure, not run");
    let error = prepared
        .execute(&boundary, &DenialProof::for_tests())
        .expect_err("should reject a boundary over other roots");
    assert!(
        format!("{error:?}").contains("outside the boundary roots"),
        "should reject plan paths outside the other roots: {error:?}"
    );
}

#[test]
fn qualified_rust_tools_constructor_is_uninhabited() {
    let never: fn(QualifiedRustTools) -> ! = |qualification| match qualification {};
    let _constructor: fn(&Path, QualifiedRustTools) -> SelectedRustTools =
        SelectedRustTools::qualified;
    let _ = never;
    let closed = AmbientProfile::for_role(Role::Reproduction)
        .expect_err("should keep the alternate ambient closed");
    assert!(
        format!("{closed:?}").contains("not qualified"),
        "should name the missing qualification"
    );
}

#[test]
fn ordinary_ci_selection_carries_no_release_authority() {
    let fixture = Fixture::new(COMMIT);
    let tools = fixture.tools_for(ReleaseTarget::Aarch64AppleDarwin);
    assert_eq!(
        tools.authority(),
        Authority::Unqualified,
        "should never report a test selection as qualified"
    );
    assert_eq!(
        ToolSelection::OrdinaryCi.authority(),
        Authority::Unqualified,
        "should report the ordinary CI selection as unqualified"
    );
}

#[test]
fn root_filled_between_parse_and_bind_is_rejected() {
    for (relative, label) in [("cargo-state", "cargo state"), ("output", "output")] {
        let fixture = Fixture::new(COMMIT);
        let request = fixture.request(
            "ts-aarch64-apple-darwin",
            Role::Canonical.spec_label(),
            COMMIT,
        );
        fs::write(
            fixture.base.join(relative).join("late"),
            b"written after parse",
        )
        .expect("should fill the root after parsing");
        let error = bind_error(fixture.bind_with(request, Role::Canonical, COMMIT));
        assert!(
            error.contains(label) && error.contains("cannot be reused"),
            "should reject a {label} root filled after parsing"
        );
        assert!(
            !fixture.base.join("target").exists(),
            "should not create the target after a rejection"
        );
    }
    let fixture = Fixture::new(COMMIT);
    let request = fixture.request(
        "ts-aarch64-apple-darwin",
        Role::Canonical.spec_label(),
        COMMIT,
    );
    fs::create_dir(fixture.base.join("target")).expect("should create the target after parse");
    assert!(
        bind_error(fixture.bind_with(request, Role::Canonical, COMMIT))
            .contains("cannot be reused"),
        "should reject a target created after parsing"
    );
}

#[test]
fn materials_changed_or_foreign_after_preparation_are_rejected() {
    let fixture = Fixture::new(COMMIT);
    let request = fixture.request(
        "ts-aarch64-apple-darwin",
        Role::Canonical.spec_label(),
        COMMIT,
    );
    let materials = fixture.materials(COMMIT);
    fs::write(
        fixture
            .base
            .join("materials/vendor/example-1.0.0/Cargo.toml"),
        b"tampered after preparation",
    )
    .expect("should edit a vendored file");
    let tools = fixture.tools_for(ReleaseTarget::Aarch64AppleDarwin);
    assert!(
        bind_error(fixture.bind_parts(request, Role::Canonical, COMMIT, tools, materials))
            .contains("no longer rehash"),
        "should reject materials edited after preparation"
    );
    assert!(
        !fixture.base.join("target").exists(),
        "should not create the target after a rejection"
    );

    let fixture = Fixture::new(COMMIT);
    let request = fixture.request(
        "ts-aarch64-apple-darwin",
        Role::Canonical.spec_label(),
        COMMIT,
    );
    let foreign = fixture.materials(OTHER_COMMIT);
    let tools = fixture.tools_for(ReleaseTarget::Aarch64AppleDarwin);
    assert!(
        bind_error(fixture.bind_parts(request, Role::Canonical, COMMIT, tools, foreign))
            .contains("another source commit"),
        "should reject materials derived from another commit"
    );
}

#[test]
fn tools_selected_for_another_target_are_rejected() {
    let fixture = Fixture::new(COMMIT);
    let request = fixture.request(
        "ts-aarch64-apple-darwin",
        Role::Canonical.spec_label(),
        COMMIT,
    );
    let tools = fixture.tools_for(ReleaseTarget::X86_64UnknownLinuxGnu);
    let materials = fixture.materials(COMMIT);
    assert!(
        bind_error(fixture.bind_parts(request, Role::Canonical, COMMIT, tools, materials))
            .contains("another target"),
        "should reject tools whose standard library was checked for another target"
    );
}

#[test]
fn target_inside_checkout_is_rejected_before_creation() {
    let fixture = Fixture::new(COMMIT);
    let inside = fixture.base.join("checkout-canonical/target");
    let request = fixture.request_at(
        "ts-aarch64-apple-darwin",
        Role::Canonical.spec_label(),
        COMMIT,
        &inside,
    );
    assert!(
        bind_error(fixture.bind_with(request, Role::Canonical, COMMIT)).contains("overlap"),
        "should reject a target inside the checkout root"
    );
    assert!(!inside.exists(), "should create nothing in the checkout");
    let fixture = Fixture::new(COMMIT);
    let inside_tools = fixture.base.join("tools/target");
    let request = fixture.request_at(
        "ts-aarch64-apple-darwin",
        Role::Canonical.spec_label(),
        COMMIT,
        &inside_tools,
    );
    assert!(
        bind_error(fixture.bind_with(request, Role::Canonical, COMMIT)).contains("overlap"),
        "should reject a target inside the tools root"
    );
    assert!(!inside_tools.exists(), "should create nothing in the tools");
}

#[test]
fn boundary_roots_mount_materials_read_only_state_and_target_writable_output_unmounted() {
    let fixture = Fixture::new(COMMIT);
    let prepared = fixture
        .bind("ts-aarch64-apple-darwin", Role::Canonical)
        .expect("should bind the recipe");
    let expected = BoundaryRoots::new(
        &[
            fixture.base.join("checkout-canonical"),
            fixture.retained(),
            fixture.base.join("materials"),
        ],
        &[fixture.base.join("tools")],
        &[
            fixture.base.join("cargo-state"),
            fixture.base.join("target"),
        ],
    )
    .and_then(|roots| roots.with_system(&[fixture.developer_dir()]))
    .expect("should build the expected roots");
    assert_eq!(
        prepared.boundary_roots().expect("should derive roots"),
        expected,
        "should mount exactly the planned roles"
    );
    assert_eq!(
        expected.system(),
        [fixture.developer_dir()],
        "should add the Apple developer directory as the only system root"
    );
    let all: Vec<_> = expected
        .entries()
        .map(|(_, _, path)| path.to_owned())
        .collect();
    assert!(
        !all.iter()
            .any(|path| path.starts_with(fixture.base.join("output"))),
        "should not mount the output root"
    );
    assert_eq!(
        expected.writable().len(),
        2,
        "should expose exactly two writable roots"
    );
    assert!(
        expected
            .immutable()
            .contains(&fixture.base.join("materials")),
        "should mount the vendor materials read-only"
    );
}
