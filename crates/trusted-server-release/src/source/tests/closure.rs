//! Helper closure, inventory join and child-environment checks.

use super::*;

#[test]
fn closure_requires_every_listed_member_manifest() {
    let mut fixture = Fixture::new();
    write(
        &fixture.root,
        "Cargo.toml",
        b"[workspace]\nmembers = [\"crates/trusted-server-openrtb\", \"crates/missing\"]\n",
    );
    fixture.commit_all();
    assert_rejects(&fixture, "crates/missing/Cargo.toml");
    write(
        &fixture.root,
        "Cargo.toml",
        b"[workspace]\nmembers = [\"crates/trusted-server-release\"]\n",
    );
    fixture.commit_all();
    assert_rejects(&fixture, "crates/trusted-server-openrtb/Cargo.toml");
    write(
        &fixture.root,
        "Cargo.toml",
        b"[workspace]\nmembers = [\"crates/*\"]\n",
    );
    fixture.commit_all();
    assert_rejects(&fixture, "glob");
}

#[test]
fn closure_digest_changes_with_helper_sources() {
    let mut fixture = Fixture::new();
    let before = fixture.authenticate().expect("should authenticate");
    write(
        &fixture.root,
        "crates/trusted-server-release/src/main.rs",
        b"fn main() { }\n",
    );
    fixture.commit_all();
    let after = fixture
        .authenticate()
        .expect("should authenticate changed helper");
    assert_ne!(
        before.closure_digest(),
        after.closure_digest(),
        "should invalidate the closure when helper source changes"
    );
    write(
        &fixture.root,
        "crates/other/src/lib.rs",
        b"// changed outside closure\n",
    );
    fixture.commit_all();
    let outside = fixture
        .authenticate()
        .expect("should authenticate outside change");
    assert_eq!(
        after.closure_digest(),
        outside.closure_digest(),
        "should ignore sources outside the helper closure"
    );
}

#[test]
fn joins_inventory_only_to_the_approved_commit() {
    let fixture = Fixture::new();
    let source = fixture.authenticate().expect("should authenticate");
    let inventory = |commit: &str| {
        serde_json::to_vec(&json!({
            "materials": [],
            "modules": [],
            "schema_version": 1,
            "source_commit": commit,
            "tools": [],
        }))
        .expect("should encode inventory")
    };
    join_inventory(&source, &inventory(&fixture.commit)).expect("should join matching inventory");
    assert!(
        join_inventory(&source, &inventory(&"a".repeat(40))).is_err(),
        "should reject a different inventory commit"
    );
    let mut extra: Value =
        serde_json::from_slice(&inventory(&fixture.commit)).expect("should decode inventory");
    extra["approved"] = json!(true);
    assert!(
        join_inventory(&source, &serde_json::to_vec(&extra).expect("should encode")).is_err(),
        "should reject unknown inventory fields"
    );
}

#[test]
fn child_environment_takes_commit_and_epoch_from_approved_source() {
    let fixture = Fixture::new();
    let audited = fixture.audited();
    let source = fixture.authenticate().expect("should authenticate");
    let child = child_environment(
        &audited,
        &source,
        &ChildRequest {
            target: ReleaseTarget::Wasm32Wasip1,
            home: PathBuf::from("/private/release/home"),
            cargo_home: PathBuf::from("/private/release/cargo-home"),
            tool_directories: Vec::new(),
        },
    )
    .expect("should build child environment");
    assert_eq!(
        child.variables().get("TRUSTED_SERVER__GIT_VERSION"),
        Some(&fixture.commit),
        "should inject the approved commit"
    );
    assert_eq!(
        child
            .variables()
            .get("SOURCE_DATE_EPOCH")
            .map(String::as_str),
        Some("1700000000"),
        "should inject the approved committer epoch"
    );
}
