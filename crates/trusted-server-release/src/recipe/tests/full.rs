//! Explicit real Linux Cargo fixtures; never selected by a normal focused unit run.
//!
//! Both runs are ordinary CI candidate evidence (`release_qualification:
//! false`): the toolchain, container image and fixture acquisition are
//! unqualified and unauthenticated, so nothing here qualifies a release.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::super::ambient::ObservedAmbient;
use super::super::compare::{FASTLY_ASSET, check_fastly_equality};
use super::super::remap::{RemapScan, scan_remapped_bytes};
use super::candidate::{
    CargoFixture, cargo_docker_settings, cargo_measurement, denial_measurement,
    equality_measurement, reported_errno,
};
use super::*;
use crate::boundary::{Boundary, DenialProof, ImageSelection, ProbeProgram};
use crate::documents::{parse_json, read_bounded};
use crate::materials::{GitVendorInput, LockedMaterials, NormalizedManifest, VendorInputs};
use crate::preflight::child::build_child_for_tests;
use crate::preflight::{AuditedEnvironment, audit_environment};
use crate::preparation::tests::full::{JsFixtureRun, run_two_fresh_js_builds};
use crate::process::{Cancellation, ProcessLimits};

/// The committed denial fixture workspace.
const DENIAL_FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/cargo-denial");
/// Fictional committer epoch of the denial fixture child environment.
const DENIAL_EPOCH: u64 = 1_700_000_000;
/// Members that attempt a connection while Cargo builds them.
const ATTEMPTS: [&str; 3] = ["build-direct", "build-grandchild", "macro-attempt"];

/// Reads the ordinary CI Cargo fixture named by `TS_RELEASE_CARGO_FIXTURE_ROOT`.
fn cargo_fixture() -> CargoFixture {
    let root = std::env::var_os("TS_RELEASE_CARGO_FIXTURE_ROOT")
        .map(PathBuf::from)
        .expect("should explicitly supply the ordinary CI Cargo fixture root");
    let root = fs::canonicalize(root).expect("should resolve the Cargo fixture root");
    let raw = read_bounded(&root.join("fixture.json"), DocumentRole::Metadata)
        .expect("should read the Cargo fixture description");
    let document =
        parse_json(&raw.bytes, DocumentRole::Metadata).expect("should parse the description");
    CargoFixture::parse(&root, &document).expect("should accept the Cargo fixture description")
}

fn minimal_environment() -> AuditedEnvironment {
    audit_environment(&[(OsString::from("PATH"), OsString::from("/usr/bin:/bin"))])
        .expect("should audit the minimal environment")
}

/// Conditions of an ordinary CI build of `role`.
///
/// The umask and jobs are the profile values the plan enforces on the child.
/// The reproduction build uses the test-only alternate profile: its host locale and timezone
/// are profile labels, while the child runs with the normalized `C` locale
/// and `UTC` timezone either way. This is not an independent qualified
/// observation, and the production reproduction build stays unqualified.
fn ordinary_ci_ambient(role: Role, checkout: &Path) -> CheckedAmbient {
    let (profile, base) = match role {
        Role::Canonical => (AmbientProfile::CANONICAL, observed_canonical()),
        Role::Reproduction => (
            AmbientProfile::alternate_for_tests(),
            observed_reproduction(),
        ),
    };
    // Reading the real user ID has no preconditions.
    let execution_uid = unsafe { libc::getuid() };
    let started_at_seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("should read a clock after the Unix epoch")
        .as_secs();
    CheckedAmbient::check(
        role,
        &profile,
        ObservedAmbient {
            execution_uid,
            checkout_root: checkout.to_owned(),
            started_at_seconds,
            ..base
        },
    )
    .expect("should match the role profile")
}

/// A configured container boundary with its proof for this instance.
struct Proven {
    _state: tempfile::TempDir,
    boundary: Boundary,
    proof: DenialProof,
}

/// Configures the exact recipe boundary and proves its denial.
fn proven_boundary(prepared: &PreparedRecipe, fixture: &CargoFixture) -> Proven {
    let state = tempfile::tempdir().expect("should create disjoint boundary host state");
    // These IDs only select the existing unprivileged CI worker account.
    let user = unsafe { (libc::getuid(), libc::getgid()) };
    let parallelism = std::thread::available_parallelism()
        .map_or(1, |count| u32::try_from(count.get()).unwrap_or(u32::MAX));
    let boundary = Boundary::linux_container(
        prepared
            .boundary_roots()
            .expect("should derive the exact recipe roots"),
        cargo_docker_settings(user, parallelism),
        ImageSelection::Test {
            id: fixture.image_id.clone(),
        },
        state.path(),
    )
    .expect("should configure the owned PID/network container boundary");
    let probe = ProbeProgram::from_staged_release_binary(&fixture.probe())
        .expect("should select the staged producer probe");
    let limits = ProcessLimits {
        wall: Duration::from_secs(60),
        stdout_max: 4096,
        stderr_max: 4096,
    };
    let proof = boundary
        .establish_denial(&probe, limits)
        .expect("should establish real positive and denied socket controls");
    assert!(
        matches!(proof.denied_errno(), libc::EPERM | libc::EACCES),
        "should prove denial by the seccomp profile, not by a missing route"
    );
    Proven {
        _state: state,
        boundary,
        proof,
    }
}

/// Copies a tree of directories and regular files into a new directory.
fn copy_tree(source: &Path, destination: &Path) {
    fs::create_dir(destination).expect("should create the copied directory");
    for entry in fs::read_dir(source).expect("should list the fixture directory") {
        let entry = entry.expect("should read the fixture entry");
        let kind = entry.file_type().expect("should inspect the fixture entry");
        let target = destination.join(entry.file_name());
        if kind.is_dir() {
            copy_tree(&entry.path(), &target);
        } else if kind.is_file() {
            fs::copy(entry.path(), &target).expect("should copy the fixture file");
        } else {
            panic!("should hold only directories and regular files");
        }
    }
}

/// One bound denial fixture member with the roots it owns.
struct DenialCase {
    _work: tempfile::TempDir,
    _completed: JsBuildCompleted,
    prepared: PreparedRecipe,
}

/// Binds one denial fixture member over fresh private roots.
fn bind_denial_case(fixture: &CargoFixture, member: &str) -> DenialCase {
    let work = tempfile::tempdir().expect("should create the case roots");
    let base = fs::canonicalize(work.path()).expect("should resolve the case roots");
    let checkout = base.join("checkout");
    copy_tree(Path::new(DENIAL_FIXTURE), &checkout);
    for directory in ["materials/vendor", "cargo-state", "output"] {
        fs::create_dir_all(base.join(directory)).expect("should create a case root");
    }
    let completed = JsBuildCompleted::fixture_for_tests(&fixture.source_commit);
    let retained = fs::canonicalize(
        completed
            .rehash_for_handoff()
            .expect("should rehash the fixture handoff"),
    )
    .expect("should resolve the retained root");
    let request = RecipeRequest::parse_for_asset(
        AssetId::denial_fixture(member).expect("should resolve the fixture member"),
        &RecipeInput {
            asset: member,
            variant: "default",
            role: Role::Canonical.spec_label(),
            commit: &fixture.source_commit,
            js_dir: &retained.join("bundles"),
            js_inventory: &retained.join("tsjs-inventory.json"),
            materials: &base.join("materials"),
            cargo_state: &base.join("cargo-state"),
            output: &base.join("output"),
            target_dir: &base.join("target"),
        },
    )
    .expect("should accept the case request");
    let commit = CommitId::parse(&fixture.source_commit).expect("should parse the commit");
    let child = build_child_for_tests(
        &minimal_environment(),
        &commit,
        DENIAL_EPOCH,
        &child_request(&request),
    )
    .expect("should build the child environment");
    let prepared = PreparedRecipe::bind(BindInputs {
        commit: &commit,
        child,
        completed: &completed,
        tools: SelectedRustTools::for_tests(&fixture.tools(), ReleaseTarget::Wasm32Wasip1)
            .expect("should select the ordinary CI Rust tools"),
        materials: PreparedCargoMaterials::for_tests(&base.join("materials"), commit.as_str(), &[])
            .expect("should describe the empty vendor directory"),
        ambient: ordinary_ci_ambient(Role::Canonical, &checkout),
        request,
    })
    .expect("should bind the denial case");
    DenialCase {
        _work: work,
        _completed: completed,
        prepared,
    }
}

#[test]
#[ignore = "requires the explicit ordinary CI Cargo fixture and Linux Docker; run separately in producer CI"]
fn cargo_children_inherit_denial() {
    let fixture = cargo_fixture();
    let mut observed = BTreeMap::new();
    let mut proof_errno = None;
    for case in ATTEMPTS {
        let bound = bind_denial_case(&fixture, case);
        let proven = proven_boundary(&bound.prepared, &fixture);
        let errno = proven.proof.denied_errno();
        let error = bound
            .prepared
            .execute(&proven.boundary, &proven.proof)
            .expect_err("should fail a build that attempts a connection");
        let text = format!("{error:?}");
        let reported = reported_errno(case, &text)
            .unwrap_or_else(|| panic!("should report the {case} attempt errno: {text}"));
        assert_eq!(
            reported, errno,
            "should deny the {case} attempt with the proven seccomp errno: {text}"
        );
        assert_ne!(
            reported,
            libc::ECONNREFUSED,
            "should deny {case} before the empty network namespace refuses it"
        );
        observed.insert(case.to_owned(), reported);
        proof_errno = Some(errno);
    }
    let bound = bind_denial_case(&fixture, "control");
    let proven = proven_boundary(&bound.prepared, &fixture);
    let control = bound
        .prepared
        .execute(&proven.boundary, &proven.proof)
        .expect("should build the control through the confined recipe");
    control
        .rehash_for_handoff()
        .expect("should rehash the control artifact before reporting it");
    assert_eq!(
        control.asset().artifact().name,
        "denial-control.wasm",
        "should copy the control under its asset name"
    );
    let measurement = denial_measurement(
        proof_errno.expect("should observe every attempt"),
        &observed,
        &control,
    );
    let text = serde_json::to_string_pretty(&measurement).expect("should encode the measurement");
    println!("{text}");
    fs::write(fixture.root.join("cargo-denial-measurement.json"), text)
        .expect("should retain the denial measurement");
}

/// Prepares the vendor tree from the fixture's retained originals.
fn prepare_vendor(
    fixture: &CargoFixture,
    locked: &LockedMaterials,
    root: &Path,
    deadline: Instant,
    cancellation: &Cancellation,
) -> PreparedCargoMaterials {
    let collection = locked
        .git_collection(Path::new("/usr/bin/git"), deadline, cancellation)
        .expect("should select the runner Git for object verification only");
    let verified: Vec<_> = fixture
        .git
        .iter()
        .map(|source| {
            collection
                .verify(&source.selector, &source.revision, &source.objects)
                .expect("should verify the retained locked Git closure")
        })
        .collect();
    let manifests: Vec<_> = fixture
        .normalized
        .iter()
        .map(|pair| NormalizedManifest {
            cargo_toml: fs::read(&pair.cargo_toml).expect("should read the normalized manifest"),
            cargo_toml_orig: fs::read(&pair.cargo_toml_orig)
                .expect("should read the committed manifest"),
        })
        .collect();
    let inputs: Vec<GitVendorInput<'_>> = fixture
        .normalized
        .iter()
        .zip(&manifests)
        .map(|(pair, manifest)| {
            let index = fixture
                .git
                .iter()
                .position(|source| source.selector == pair.selector)
                .expect("should name a retained Git source");
            GitVendorInput {
                material: &verified[index],
                name: &pair.name,
                version: &pair.version,
                normalized: Some(manifest.clone()),
            }
        })
        .collect();
    locked
        .prepare_cargo_vendor(
            &VendorInputs {
                registry_archives: &fixture.registry,
                git: &inputs,
            },
            root,
            deadline,
            cancellation,
        )
        .expect("should vendor every locked original")
}

/// One confined Fastly build with the private roots its output lives in.
struct FastlyBuild {
    _materials: tempfile::TempDir,
    _work: tempfile::TempDir,
    completed: RecipeCompleted,
    /// Host roots of this build that must not appear in the final bytes.
    host_roots: Vec<String>,
    /// The checkout root as the confined build saw it.
    execution_checkout: String,
}

fn path_text(path: &Path) -> String {
    path.to_str().expect("should be a UTF-8 path").to_owned()
}

/// Builds the Fastly Wasm for `role` from the shared JS handoff.
///
/// Each role gets its own fresh source copy (a distinct checkout root), its
/// own freshly prepared vendor tree, fresh state, output and target roots,
/// and its own boundary with its own denial proof. The target directory and
/// source copy are removed after the build; only the copied output remains.
fn build_fastly(
    js: &JsFixtureRun,
    fixture: &CargoFixture,
    role: Role,
    deadline: Instant,
    cancellation: &Cancellation,
) -> FastlyBuild {
    let fresh = js
        .source
        .materialize(deadline, cancellation)
        .expect("should materialize a fresh tracked source for this role");
    let materials_directory = tempfile::tempdir().expect("should create the materials root");
    let materials_root =
        fs::canonicalize(materials_directory.path()).expect("should resolve the materials root");
    let materials = prepare_vendor(fixture, &js.locked, &materials_root, deadline, cancellation);
    assert_eq!(
        materials.cargo_lock_sha256(),
        fixture.cargo_lock_sha256,
        "should vendor from the fixture's Cargo.lock"
    );
    let work = tempfile::tempdir().expect("should create the build roots");
    let base = fs::canonicalize(work.path()).expect("should resolve the build roots");
    for directory in ["cargo-state", "output"] {
        fs::create_dir(base.join(directory)).expect("should create a build root");
    }
    let retained = fs::canonicalize(
        js.completed
            .rehash_for_handoff()
            .expect("should rehash the JS handoff"),
    )
    .expect("should resolve the retained root");
    let request = RecipeRequest::parse(&RecipeInput {
        asset: FASTLY_ASSET,
        variant: "default",
        role: role.spec_label(),
        commit: &fixture.source_commit,
        js_dir: &retained.join("bundles"),
        js_inventory: &retained.join("tsjs-inventory.json"),
        materials: &materials_root,
        cargo_state: &base.join("cargo-state"),
        output: &base.join("output"),
        target_dir: &base.join("target"),
    })
    .expect("should accept the Fastly request");
    let prepared = PreparedRecipe::new(
        &js.source,
        &fresh,
        &js.audited,
        &js.completed,
        SelectedRustTools::for_tests(&fixture.tools(), ReleaseTarget::Wasm32Wasip1)
            .expect("should select the ordinary CI Rust tools"),
        materials,
        ordinary_ci_ambient(role, fresh.root()),
        request,
    )
    .expect("should bind the real source, handoff, tools and materials");
    let proven = proven_boundary(&prepared, fixture);
    let execution_checkout = path_text(
        &proven
            .boundary
            .execution_path(fresh.root())
            .expect("should translate the checkout root"),
    );
    let host_roots = [
        fresh.root(),
        materials_root.as_path(),
        base.as_path(),
        retained.as_path(),
    ]
    .map(path_text)
    .to_vec();
    let completed = prepared
        .execute(&proven.boundary, &proven.proof)
        .unwrap_or_else(|error| {
            panic!(
                "should build the role {role:?} Fastly Wasm through the confined recipe: {error:?}"
            )
        });
    completed
        .rehash_for_handoff()
        .expect("should rehash the artifact before reporting it");
    assert!(
        !completed.derived_manifest_digests().is_empty(),
        "should mark the build as consuming derived Git manifests"
    );
    println!(
        "{}",
        serde_json::to_string_pretty(&cargo_measurement(&completed))
            .expect("should encode the build measurement")
    );
    fs::remove_dir_all(base.join("target")).expect("should remove the spent target directory");
    fresh.close().expect("should remove the fresh source copy");
    FastlyBuild {
        _materials: materials_directory,
        _work: work,
        completed,
        host_roots,
        execution_checkout,
    }
}

/// Scans the final bytes for every host root and the container prefix, and
/// requires at least one fixed remapped prefix.
fn scan_final_bytes(fixture: &CargoFixture, builds: [&FastlyBuild; 2]) -> RemapScan {
    let mut forbidden: Vec<String> = builds
        .iter()
        .flat_map(|build| build.host_roots.iter().cloned())
        .collect();
    forbidden.push(path_text(&fixture.root));
    if let Some(runner_temp) = std::env::var_os("RUNNER_TEMP") {
        forbidden.push(
            runner_temp
                .into_string()
                .expect("should name RUNNER_TEMP in UTF-8"),
        );
    }
    let forbidden: Vec<&str> = forbidden.iter().map(String::as_str).collect();
    let bytes = fs::read(builds[0].completed.artifact().path())
        .expect("should read the compared final bytes");
    scan_remapped_bytes(&bytes, &forbidden)
        .unwrap_or_else(|error| panic!("should prove remapping in the final bytes: {error:?}"))
}

#[test]
#[ignore = "requires explicit JS and Cargo ordinary CI fixtures and Linux Docker; run separately in producer CI"]
fn fastly_canonical_and_reproduction_builds_are_byte_identical() {
    let fixture = cargo_fixture();
    let js = run_two_fresh_js_builds();
    assert_eq!(
        js.source.commit().as_str(),
        fixture.source_commit,
        "should build the Cargo fixture's source commit"
    );
    println!(
        "{}",
        serde_json::to_string_pretty(&js.measurement).expect("should encode the JS measurement")
    );
    let cancellation = Cancellation::default();
    let deadline = Instant::now() + Duration::from_secs(40 * 60);
    let canonical = build_fastly(&js, &fixture, Role::Canonical, deadline, &cancellation);
    let reproduction = build_fastly(&js, &fixture, Role::Reproduction, deadline, &cancellation);
    let checked = check_fastly_equality(&canonical.completed, &reproduction.completed)
        .unwrap_or_else(|error| {
            panic!(
                "should build byte-identical canonical and reproduction Fastly outputs: {error:?}"
            )
        });
    let scan = scan_final_bytes(&fixture, [&canonical, &reproduction]);
    let mut measurement =
        equality_measurement(&canonical.completed, &reproduction.completed, &checked);
    for (role, build) in [("canonical", &canonical), ("reproduction", &reproduction)] {
        // Container paths are identical, so checkout variation is host-only on Linux.
        measurement["builds"][role]["ambient"]["execution_checkout_root"] =
            serde_json::json!(build.execution_checkout);
    }
    measurement["remap_scan"] = serde_json::json!({
        "forbidden": ["/boundary/", "host build roots", "fixture root", "RUNNER_TEMP"],
        "found": scan.found(),
    });
    let text = serde_json::to_string_pretty(&measurement).expect("should encode the measurement");
    println!("{text}");
    fs::write(
        fixture
            .root
            .join("cargo-canonical-reproduction-equality-measurement.json"),
        text,
    )
    .expect("should retain the canonical/reproduction equality measurement");
}
