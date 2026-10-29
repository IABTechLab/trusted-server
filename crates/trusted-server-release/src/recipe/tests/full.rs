//! Explicit real Linux Cargo fixtures; never selected by a normal focused unit run.
//!
//! Both runs are ordinary CI candidate evidence (`release_qualification:
//! false`): the toolchain, container image and fixture acquisition are
//! unqualified and unauthenticated, so nothing here qualifies a release.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::super::compare::{FASTLY_ASSET, check_fastly_equality};
use super::super::compare::{FastlyEqualityChecked, read_artifact};
use super::super::remap::{RemapScan, scan_remapped_bytes};
use super::super::runtime::{
    FastlyRuntimeAccepted, HealthOnlySuccess, OriginPresence, RuntimeRejection, SelectedViceroy,
    accept_fastly_runtime, run_gate,
};
use super::candidate::js_inventory_sha256;
use super::candidate::{
    CargoFixture, SCHEMA_VERSION, cargo_docker_settings, cargo_measurement, ci_build_provenance,
    denial_measurement, equality_measurement, reported_errno,
};
use super::real_fixture::{
    ATTEMPTS, DenialCase, DenialSelection, bind_denial_case, cargo_fixture, ordinary_ci_ambient,
    path_text, try_prepare_vendor,
};
use super::*;
use crate::boundary::{Boundary, DenialProof, ImageSelection, ProbeProgram};
use crate::materials::{LockedMaterials, resolution_differences};
use crate::preparation::tests::full::{JsFixtureRun, run_two_fresh_js_builds};
use crate::process::{Cancellation, ProcessLimits};

/// A configured container boundary with its proof for this instance.
pub(super) struct Proven {
    _state: tempfile::TempDir,
    pub(super) boundary: Boundary,
    pub(super) proof: DenialProof,
}

/// The builder image ID a Linux container fixture names.
fn linux_image(fixture: &CargoFixture) -> &str {
    fixture
        .container_image_id()
        .expect("should name the Linux builder image")
}

/// Configures the exact recipe boundary in the image `image_id` and proves
/// its denial.
pub(super) fn proven_boundary(
    prepared: &PreparedRecipe,
    fixture: &CargoFixture,
    image_id: &str,
) -> Proven {
    try_proven_boundary(prepared, fixture, image_id)
        .expect("should prove the exact recipe boundary")
}

pub(super) fn try_proven_boundary(
    prepared: &PreparedRecipe,
    fixture: &CargoFixture,
    image_id: &str,
) -> Result<Proven, Report<ReleaseError>> {
    let state = tempfile::tempdir()
        .map_err(|error| recipe_error(format!("cannot create boundary state: {error}")))?;

    // These IDs only select the existing unprivileged CI worker account.
    let user = unsafe { (libc::getuid(), libc::getgid()) };
    let parallelism = std::thread::available_parallelism()
        .map_or(1, |count| u32::try_from(count.get()).unwrap_or(u32::MAX));
    let boundary = Boundary::linux_container(
        prepared.boundary_roots()?,
        cargo_docker_settings(user, parallelism),
        ImageSelection::Test {
            id: image_id.to_owned(),
        },
        state.path(),
    )?;
    let probe = ProbeProgram::from_staged_release_binary(&fixture.probe())?;
    let limits = ProcessLimits {
        wall: Duration::from_secs(60),
        stdout_max: 4096,
        stderr_max: 4096,
    };
    let proof = boundary.establish_denial(&probe, limits)?;
    if !matches!(proof.denied_errno(), libc::EPERM | libc::EACCES) {
        return Err(recipe_error(
            "denial proof does not establish seccomp denial",
        ));
    }
    Ok(Proven {
        _state: state,
        boundary,
        proof,
    })
}

/// Binds one Wasm denial fixture member over fresh private roots.
fn bind_wasm_denial_case(fixture: &CargoFixture, member: &str) -> DenialCase {
    bind_denial_case(
        fixture,
        member,
        DenialSelection {
            asset: AssetId::denial_fixture(member).expect("should resolve the fixture member"),
            tools: SelectedRustTools::for_tests(&fixture.tools(), ReleaseTarget::Wasm32Wasip1)
                .expect("should select the ordinary CI Rust tools"),
            target_dir: None,
        },
    )
}

#[test]
#[ignore = "requires the explicit ordinary CI Cargo fixture and Linux Docker; run separately in producer CI"]
fn cargo_children_inherit_denial() {
    let fixture = cargo_fixture();
    let mut observed = BTreeMap::new();
    let mut proof_errno = None;
    for case in ATTEMPTS {
        let bound = bind_wasm_denial_case(&fixture, case);
        let proven = proven_boundary(&bound.prepared, &fixture, linux_image(&fixture));
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
    let bound = bind_wasm_denial_case(&fixture, "control");
    let proven = proven_boundary(&bound.prepared, &fixture, linux_image(&fixture));
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
    let mut measurement = denial_measurement(
        proof_errno.expect("should observe every attempt"),
        &observed,
        &control,
    );
    measurement["ci_build_provenance"] = ci_build_provenance(&fixture);
    let text = serde_json::to_string_pretty(&measurement).expect("should encode the measurement");
    println!("{text}");
    fs::write(fixture.root.join("cargo-denial-measurement.json"), text)
        .expect("should retain the denial measurement");
}

/// Prepares the vendor tree from the fixture's retained originals.
///
/// The fixture's `cargo vendor` manifests only name which locked Git packages
/// to supply closures for; the producer normalizes every inheriting manifest
/// itself from the verified tree.
fn prepare_vendor(
    fixture: &CargoFixture,
    locked: &LockedMaterials,
    root: &Path,
    deadline: Instant,
    cancellation: &Cancellation,
) -> PreparedCargoMaterials {
    try_prepare_vendor(fixture, locked, root, deadline, cancellation)
        .expect("should vendor every locked original")
}

/// Compares every producer-normalized manifest with the one `cargo vendor`
/// wrote, before anything is built, and requires equal resolution facts.
///
/// This is ordinary CI differential evidence, not authority: the comparison
/// manifests are unauthenticated and never reach the vendor tree.
fn normalized_manifest_differential(
    fixture: &CargoFixture,
    materials: &PreparedCargoMaterials,
) -> serde_json::Value {
    let mut packages = serde_json::Map::new();
    let mut differing = 0_usize;
    for package in &fixture.comparison_manifests {
        let directory = materials.vendor_dir().join(&package.name);
        let producer =
            fs::read(directory.join("Cargo.toml")).expect("should read the producer manifest");
        let cargo_vendor = fs::read(&package.cargo_vendor_manifest)
            .expect("should read the cargo vendor comparison manifest");
        let build_rs_present = directory.join("build.rs").is_file();
        let differences = resolution_differences(&producer, &cargo_vendor, build_rs_present)
            .expect("should compare the producer and cargo vendor manifests");
        differing += usize::from(!differences.is_empty());
        packages.insert(
            package.name.clone(),
            serde_json::json!({
                "producer_normalized": materials
                    .normalized_manifest_digests()
                    .contains_key(&package.name),
                "differences": differences,
            }),
        );
    }
    let measurement = serde_json::json!({
        "schema_version": SCHEMA_VERSION,
        "kind": "ordinary-ci-normalized-manifest-differential",
        "compared": packages.len(),
        "normalized_manifest_count": materials.normalized_manifest_digests().len(),
        "derived_manifest_digests": materials.derived_manifest_digests().len(),
        "differing": differing,
        "packages": packages,
        "authority": "evidence-only",
        "release_qualification": false,
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&measurement).expect("should encode the differential")
    );
    assert_eq!(
        differing, 0,
        "should resolve every Git manifest as cargo vendor does: {measurement}"
    );
    measurement
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
    /// The producer-normalized versus `cargo vendor` manifest comparison.
    differential: serde_json::Value,
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
    let differential = normalized_manifest_differential(fixture, &materials);
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
    let proven = proven_boundary(&prepared, fixture, linux_image(fixture));
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
        completed.derived_manifest_digests().is_empty()
            && !completed.normalized_manifest_digests().is_empty(),
        "should build from producer-normalized Git manifests and no caller-derived one"
    );
    println!(
        "{}",
        serde_json::to_string_pretty(&cargo_measurement(&completed))
            .expect("should encode the build measurement")
    );
    fs::remove_dir_all(base.join("target")).expect("should remove the spent target directory");
    fresh.close().expect("should remove the fresh source copy");
    completed
        .denial_receipt()
        .expect("should retain measured socket observations after working-root cleanup");
    completed
        .recipe_definition()
        .expect("should retain the approved recipe definition after working-root cleanup");
    FastlyBuild {
        _materials: materials_directory,
        _work: work,
        completed,
        host_roots,
        execution_checkout,
        differential,
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
#[ignore = "requires explicit JS and Cargo ordinary CI fixtures, Linux Docker and the pinned Viceroy; run separately in producer CI"]
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
    let differential = serde_json::json!({
        "canonical": canonical.differential,
        "reproduction": reproduction.differential,
    });
    fs::write(
        fixture
            .root
            .join("normalized-manifest-differential-measurement.json"),
        serde_json::to_string_pretty(&differential).expect("should encode the differential"),
    )
    .expect("should retain the normalized manifest differential");
    measurement["remap_scan"] = serde_json::json!({
        "forbidden": ["/boundary/", "host build roots", "fixture root", "RUNNER_TEMP"],
        "found": scan.found(),
    });
    measurement["js_inventory_sha256"] =
        serde_json::json!(js_inventory_sha256(&canonical.completed));
    measurement["ci_build_provenance"] = ci_build_provenance(&fixture);
    retain_equality_measurement(&fixture, &measurement);
    let [canonical_runtime, reproduction_runtime] = run_runtime_gate(&RuntimeGateInputs {
        fixture: &fixture,
        source: &js.source,
        canonical: &canonical,
        reproduction: &reproduction,
        checked: &checked,
    });
    assert!(
        canonical_runtime
            .diagnostic_for(&reproduction.completed)
            .is_err(),
        "should reject attaching canonical acceptance to the equal-byte reproduction role"
    );
    assert!(
        reproduction_runtime
            .diagnostic_for(&canonical.completed)
            .is_err(),
        "should reject attaching reproduction acceptance to the equal-byte canonical role"
    );
}

/// Prints and retains `measurement` as the canonical/reproduction equality
/// measurement.
fn retain_equality_measurement(fixture: &CargoFixture, measurement: &serde_json::Value) {
    let text = serde_json::to_string_pretty(measurement).expect("should encode the measurement");
    println!("{text}");
    fs::write(
        fixture
            .root
            .join("cargo-canonical-reproduction-equality-measurement.json"),
        text,
    )
    .expect("should retain the canonical/reproduction equality measurement");
}

/// Everything the chained runtime gate needs from the equality run.
struct RuntimeGateInputs<'a> {
    fixture: &'a CargoFixture,
    source: &'a ApprovedSource,
    canonical: &'a FastlyBuild,
    reproduction: &'a FastlyBuild,
    checked: &'a FastlyEqualityChecked,
}

/// Selects the pinned Viceroy named by `TS_RELEASE_VICEROY`.
fn pinned_viceroy() -> SelectedViceroy {
    let path = std::env::var_os("TS_RELEASE_VICEROY")
        .map(PathBuf::from)
        .expect("should explicitly supply the pinned Viceroy executable");
    SelectedViceroy::select(&path).expect("should select the pinned Viceroy 0.21.0")
}

/// The ordinary CI measurement of one accepted runtime gate run.
fn accepted_measurement(accepted: &FastlyRuntimeAccepted) -> serde_json::Value {
    let source_commit = accepted.source_commit().as_str();
    let accepted = accepted.observed();
    let publisher = accepted.publisher();
    let output = accepted.viceroy_output();
    serde_json::json!({
        "source_commit": source_commit,
        "viceroy_output": {
            "stdout_sha256": output.stdout_sha256,
            "stderr_sha256": output.stderr_sha256,
            "stderr_tail": output.stderr_tail,
        },
        "artifact": {
            "sha256": accepted.artifact_sha256(),
            "size": accepted.artifact_size(),
        },
        "fixture_sha256": accepted.fixture_sha256(),
        "config_sha256": accepted.config_sha256(),
        "health_status": accepted.health_status(),
        "publisher": {
            "status": publisher.status,
            "body_size": publisher.body_size,
            "body_sha256": publisher.body_sha256,
            "script_src": publisher.script_src,
        },
        "origin_requests": accepted.origin_requests(),
        "startup_seconds": accepted.startup().as_secs_f64(),
        "viceroy_outcome": accepted.viceroy_outcome().to_string(),
    })
}

/// Runs the final-byte runtime gate on both equal outputs, then proves that
/// a run with a closed origin is rejected although its health check passes.
fn run_runtime_gate(inputs: &RuntimeGateInputs<'_>) -> [FastlyRuntimeAccepted; 2] {
    let RuntimeGateInputs {
        fixture,
        source,
        canonical,
        reproduction,
        checked,
    } = *inputs;
    let viceroy = pinned_viceroy();
    let mut builds = serde_json::Map::new();
    let mut accepted_runs = Vec::new();
    for (role, build) in [("canonical", canonical), ("reproduction", reproduction)] {
        let accepted = accept_fastly_runtime(source, &build.completed, checked, &viceroy)
            .unwrap_or_else(|error| {
                panic!("should accept the {role} final bytes through Viceroy: {error:?}")
            });
        assert_eq!(
            accepted.observed().artifact_sha256(),
            checked.sha256(),
            "should execute the exact accepted {role} bytes"
        );
        builds.insert(role.to_owned(), accepted_measurement(&accepted));
        accepted_runs.push(accepted);
    }
    let bytes = read_artifact(&canonical.completed).expect("should reread the canonical bytes");
    let rejected = run_gate(&bytes, &viceroy, OriginPresence::Closed)
        .expect_err("should reject a run whose origin is unreachable");
    assert!(
        rejected.downcast_ref::<HealthOnlySuccess>().is_some(),
        "should reject a health-only success: {rejected:?}"
    );
    canonical
        .completed
        .rehash_for_handoff()
        .expect("should leave the canonical artifact unchanged");
    let measurement = serde_json::json!({
        "schema_version": SCHEMA_VERSION,
        "kind": "ordinary-ci-fastly-runtime-gate",
        "asset": checked.asset().artifact().name,
        "viceroy": {
            "version": viceroy.version(),
            "sha256": viceroy.sha256(),
        },
        "builds": builds,
        "health_only_rejection": rejected
            .downcast_ref::<RuntimeRejection>()
            .map(ToString::to_string),
        "release_qualification": false,
    });
    let text = serde_json::to_string_pretty(&measurement).expect("should encode the measurement");
    println!("{text}");
    fs::write(
        fixture.root.join("fastly-runtime-gate-measurement.json"),
        text,
    )
    .expect("should retain the runtime gate measurement");
    accepted_runs
        .try_into()
        .expect("should retain exactly the canonical and reproduction runtime owners")
}
