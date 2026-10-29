//! Helper closure, inventory join and child-environment checks.

use super::*;

#[test]
fn root_lifecycle_review_reads_only_the_approved_manifest() {
    let mut fixture = Fixture::new();
    let manifest = serde_json::to_vec(
        &json!({"name":"fictional-project", "scripts":{"build":"node build.mjs"}}),
    )
    .expect("should encode root manifest");
    write(&fixture.root, "Cargo.lock", b"version = 4\npackage = []\n");
    write(
        &fixture.root,
        "crates/trusted-server-js/lib/package-lock.json",
        &serde_json::to_vec(&json!({"lockfileVersion":3,"packages":{}}))
            .expect("should encode empty lock"),
    );
    write(
        &fixture.root,
        "crates/trusted-server-js/lib/package.json",
        &manifest,
    );
    fixture.commit_all();
    let source = fixture
        .authenticate()
        .expect("should authenticate root source");
    let materials = crate::materials::LockedMaterials::from_source(&source)
        .expect("should read approved locks");
    let review = materials
        .review_root(&source)
        .expect("should review hook-free approved root");
    assert_eq!(
        review.source_commit(),
        fixture.commit,
        "should bind declarations to approved source"
    );
    write(
        &fixture.root,
        "crates/trusted-server-js/lib/package.json",
        b"{}",
    );
    assert!(
        materials.review_root(&source).is_err(),
        "should reject root manifest changed after approval"
    );
    write(
        &fixture.root,
        "crates/trusted-server-js/lib/package.json",
        &manifest,
    );
    write(
        &fixture.root,
        "crates/trusted-server-js/lib/binding.gyp",
        b"fictional implicit binding",
    );
    fixture.commit_all();
    let source = fixture
        .authenticate()
        .expect("should authenticate changed source");
    let current = crate::materials::LockedMaterials::from_source(&source)
        .expect("should read current approved locks");
    assert!(
        current.review_root(&source).is_err(),
        "should reject approved root implicit install"
    );
    assert!(
        materials.review_root(&source).is_err(),
        "should reject another approved source identity"
    );
}

#[test]
fn material_lockfiles_are_read_only_as_approved_tracked_bytes() {
    let mut fixture = Fixture::new();
    let original_bytes = b"fictional original registry distribution";
    let checksum = hex::encode(Sha256::digest(original_bytes));
    let registry_line = include_str!("../../../../../Cargo.lock")
        .lines()
        .find(|line| line.starts_with("source = \"registry+"))
        .expect("should derive the reviewed registry identity from the source lock");
    let lock = format!(
        "version = 4\n[[package]]\nname = \"fictional\"\nversion = \"1.0.0\"\n{registry_line}\nchecksum = \"{checksum}\"\n"
    );
    write(&fixture.root, "Cargo.lock", lock.as_bytes());
    write(
        &fixture.root,
        "crates/trusted-server-js/lib/package-lock.json",
        br#"{"lockfileVersion":3,"packages":{}}"#,
    );
    fixture.commit_all();
    let source = fixture
        .authenticate()
        .expect("should authenticate fixture source");
    let materials = crate::materials::LockedMaterials::from_source(&source)
        .expect("should read approved lockfiles");
    let original = fixture.parent.path().join("fictional.crate");
    fs::write(&original, original_bytes).expect("should create original outside source roots");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let cancellation = crate::process::Cancellation::default();
    let retained = materials
        .verify_registry("fictional", "1.0.0", &original, deadline, &cancellation)
        .expect("should authenticate original from the approved lockfile");
    assert_eq!(
        retained.source_commit(),
        fixture.commit,
        "should bind the original to approved source"
    );
    assert_eq!(
        retained.identity(),
        "crate:fictional@1.0.0",
        "should retain locked package identity"
    );
    assert_eq!(
        retained.sha256(),
        checksum,
        "should retain exact original archive bytes"
    );
    assert!(
        materials
            .verify_registry("unknown", "1.0.0", &original, deadline, &cancellation)
            .is_err(),
        "should reject an archive absent from approved registry materials"
    );
    assert!(
        materials
            .verify_npm("node_modules/unknown", &original, deadline, &cancellation)
            .is_err(),
        "should reject an archive absent from approved npm locations"
    );
    assert!(
        source
            .read_tracked("../Cargo.lock", Role::Metadata)
            .is_err(),
        "should reject paths outside the approved tracked names"
    );
    write(&fixture.root, "Cargo.lock", b"version = 4\n");
    assert!(
        crate::materials::LockedMaterials::from_source(&source).is_err(),
        "should reject lockfile changes after source authentication"
    );
}

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

#[test]
fn helper_closure_includes_the_actual_npm_observer() {
    let mut fixture = Fixture::new();
    write(
        &fixture.root,
        "scripts/release/observe-npm.mjs",
        b"// first observer\n",
    );
    fixture.commit_all();
    let first = fixture
        .authenticate()
        .expect("should authenticate observer source")
        .closure_digest()
        .to_owned();
    write(
        &fixture.root,
        "scripts/release/observe-npm.mjs",
        b"// changed observer\n",
    );
    fixture.commit_all();
    let second = fixture
        .authenticate()
        .expect("should authenticate changed observer source")
        .closure_digest()
        .to_owned();
    assert_ne!(
        first, second,
        "should invalidate helper selection when observer code changes"
    );
}
