//! Explicit real macOS CLI candidate fixture; never selected by a normal
//! focused unit run.
//!
//! The run is ordinary CI candidate evidence (`release_qualification:
//! false`): the Xcode, toolchain, fixture acquisition and the JS handoff
//! adopted from the Linux job are unqualified and unauthenticated, and the
//! macOS sandbox only reaps descendants after each run. The canonical build
//! is gated (Mach-O acceptance in completion, the debug map and measured
//! SDK, the remap and final-path scans, planted operator values, native
//! startup); the reproduction build is a recorded measurement only, and
//! runs only when `TS_RELEASE_NATIVE_REPRODUCTION=measure` (otherwise the
//! measurement records it as deferred to the scheduled run).

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::super::compare::read_artifact;
use super::super::native::{check_operator_values_absent, measure_native_repeatability};
use super::super::remap::scan_remapped_bytes;
use super::super::startup::native::{NativeStartupLayout, measure_host_identity};
use super::super::startup::{NativeStartupAccepted, StartupCopy};
use super::candidate::js_inventory_sha256;
use super::candidate::{CargoFixture, ci_build_provenance};
use super::cli_candidate::{
    NativeReproduction, OPERATOR_SENTINELS, ReproductionOutcome, check_build_budget,
    finish_cleanup, native_reproduction_from_env, reproduction_result,
};
use super::full_macos::{
    darwin_target_dir, job_host_asset, macos_cargo_fixture, remove_target_dir, try_proven_sandbox,
};
use super::macos_candidate::{
    MEASUREMENT_FILE, MacosCliCandidateFacts, macos_cli_candidate_measurement,
};
use super::real_fixture::{
    environment_with_operator_values, minimal_environment, path_text, try_ordinary_ci_ambient,
    try_prepare_vendor,
};
use super::*;
use crate::boundary::DescendantCleanup;
use crate::inspect::{MachoCpu, MachoPolicy, inspect_macho};
use crate::materials::LockedMaterials;
use crate::preflight::AuditedEnvironment;
use crate::process::Cancellation;
use crate::source::authenticate;

/// Everything shared by both macOS CLI builds.
struct MacosInputs<'a> {
    fixture: &'a CargoFixture,
    source: &'a ApprovedSource,
    locked: &'a LockedMaterials,
    js: &'a JsBuildCompleted,
    apple: &'a AppleDeveloperTools,
    asset: AssetId,
    audited: &'a AuditedEnvironment,
    deadline: Instant,
    cancellation: &'a Cancellation,
}

/// One confined macOS CLI build with the private roots its output lives in.
struct MacosBuild {
    _materials: tempfile::TempDir,
    _work: tempfile::TempDir,
    completed: RecipeCompleted,
    /// Host roots of this build that must not appear in the final bytes.
    host_roots: Vec<String>,
}

fn fixture_io(error: std::io::Error) -> Report<ReleaseError> {
    Report::new(ReleaseError::Recipe)
        .attach("macOS CLI fixture filesystem operation failed")
        .attach(error)
}

fn close_directory(directory: Option<tempfile::TempDir>) -> Result<(), Report<ReleaseError>> {
    directory.map_or(Ok(()), |directory| directory.close().map_err(fixture_io))
}

/// Builds the Darwin `ts` for `role` from the adopted JS handoff.
///
/// Each role gets its own fresh source copy, vendor tree, state and output
/// roots and its own proven sandbox; both share the target directory beside
/// the tool root, which is removed after each build. Setup, execution and
/// cleanup failures are returned so reproduction stays optional.
fn build_macos_cli(
    inputs: &MacosInputs<'_>,
    role: Role,
) -> Result<MacosBuild, Report<ReleaseError>> {
    check_build_budget(inputs.deadline, inputs.cancellation)?;
    let target_dir = darwin_target_dir(inputs.fixture);
    let fresh = inputs
        .source
        .materialize(inputs.deadline, inputs.cancellation)?;
    let mut materials_directory = None;
    let mut work = None;
    let built = (|| {
        let materials_owner = tempfile::tempdir().map_err(fixture_io)?;
        let materials_root = fs::canonicalize(materials_owner.path()).map_err(fixture_io)?;
        materials_directory = Some(materials_owner);
        let materials = try_prepare_vendor(
            inputs.fixture,
            inputs.locked,
            &materials_root,
            inputs.deadline,
            inputs.cancellation,
        )?;
        let work_owner = tempfile::tempdir().map_err(fixture_io)?;
        let base = fs::canonicalize(work_owner.path()).map_err(fixture_io)?;
        work = Some(work_owner);
        for directory in ["cargo-state", "output"] {
            fs::create_dir(base.join(directory)).map_err(fixture_io)?;
        }
        let retained = fs::canonicalize(inputs.js.rehash_for_handoff()?).map_err(fixture_io)?;
        let artifact = inputs.asset.artifact();
        let request = RecipeRequest::parse(&RecipeInput {
            asset: artifact.name,
            variant: "default",
            role: role.spec_label(),
            commit: inputs.source.commit().as_str(),
            js_dir: &retained.join("bundles"),
            js_inventory: &retained.join("tsjs-inventory.json"),
            materials: &materials_root,
            cargo_state: &base.join("cargo-state"),
            output: &base.join("output"),
            target_dir: &target_dir,
        })?;
        let tools = SelectedRustTools::for_tests(&inputs.fixture.tools(), artifact.target)?
            .with_apple_developer_tools(inputs.apple.clone());
        let prepared = PreparedRecipe::new(
            inputs.source,
            &fresh,
            inputs.audited,
            inputs.js,
            tools,
            materials,
            try_ordinary_ci_ambient(role, fresh.root())?,
            request,
        )?;
        let proven = try_proven_sandbox(&prepared, inputs.fixture)?;
        let host_roots = [
            fresh.root(),
            materials_root.as_path(),
            base.as_path(),
            retained.as_path(),
            target_dir.as_path(),
        ]
        .map(path_text)
        .to_vec();
        let completed = prepared.execute(&proven.boundary, &proven.proof)?;
        completed.rehash_for_handoff()?;
        Ok((completed, host_roots))
    })();
    // Attempt every cleanup even when setup, execution, or another cleanup fails.
    let built = finish_cleanup(built, fresh.close());
    let built = finish_cleanup(built, remove_target_dir(&target_dir));
    let built = built.and_then(|(completed, host_roots)| {
        completed.denial_receipt()?;
        completed.recipe_definition()?;
        Ok((completed, host_roots))
    });
    match built {
        Ok((completed, host_roots)) => Ok(MacosBuild {
            _materials: materials_directory
                .ok_or_else(|| recipe_error("missing materials root"))?,
            _work: work.ok_or_else(|| recipe_error("missing work root"))?,
            completed,
            host_roots,
        }),
        Err(error) => {
            let result = finish_cleanup::<()>(Err(error), close_directory(materials_directory));
            Err(finish_cleanup(result, close_directory(work))
                .expect_err("should retain the failed build and cleanup errors"))
        }
    }
}

/// Scans the final bytes for every host and fixture root and `RUNNER_TEMP`,
/// and returns the fixed remapped prefixes found.
fn scan_final_bytes(bytes: &[u8], build: &MacosBuild, fixture_root: &Path) -> Vec<&'static str> {
    let mut forbidden = build.host_roots.clone();
    forbidden.push(path_text(fixture_root));
    if let Some(runner_temp) = std::env::var_os("RUNNER_TEMP") {
        forbidden.push(
            runner_temp
                .into_string()
                .expect("should name RUNNER_TEMP in UTF-8"),
        );
    }
    let forbidden: Vec<&str> = forbidden.iter().map(String::as_str).collect();
    scan_remapped_bytes(bytes, &forbidden)
        .unwrap_or_else(|error| panic!("should prove remapping in the final bytes: {error:?}"))
        .found()
        .to_vec()
}

/// Starts a read-only copy of the exact final bytes natively as
/// `ts --version`, outside every build root, and removes the copy.
fn start_natively(build: &MacosBuild) -> NativeStartupAccepted {
    let scratch = tempfile::tempdir().expect("should create startup scratch space");
    let base = fs::canonicalize(scratch.path()).expect("should resolve startup scratch space");
    let copy_directory = base.join("startup-copy");
    let home = base.join("home");
    let tmpdir = base.join("tmp");
    for directory in [&home, &tmpdir] {
        fs::create_dir(directory).expect("should create private startup state");
    }
    let copy = StartupCopy::create_native(&build.completed, &copy_directory)
        .expect("should copy the final bytes read-only");
    let build_roots: Vec<&Path> = build.host_roots.iter().map(Path::new).collect();
    let started = NativeStartupAccepted::accept_macos(
        &build.completed,
        copy,
        &NativeStartupLayout {
            copy_directory: &copy_directory,
            home: &home,
            tmpdir: &tmpdir,
            build_roots: &build_roots,
        },
    );
    let started =
        started.expect("should start the copy natively on the reviewed host and remove it");
    build
        .completed
        .rehash_for_handoff()
        .expect("should leave the accepted artifact unchanged");
    started
}

/// Encodes, prints and retains the measurement beside the Cargo fixture.
/// Returns the retained measurement so a later check can extend it.
fn retain_measurement(
    fixture: &CargoFixture,
    facts: &MacosCliCandidateFacts<'_>,
    provenance: &serde_json::Value,
) -> serde_json::Value {
    let mut measurement = macos_cli_candidate_measurement(facts);
    measurement["js_inventory_sha256"] = serde_json::json!(js_inventory_sha256(facts.canonical));
    measurement["ci_build_provenance"] = provenance.clone();
    write_measurement(fixture, &measurement);
    measurement
}

/// Prints and retains `measurement` as the macOS CLI candidate measurement.
fn write_measurement(fixture: &CargoFixture, measurement: &serde_json::Value) {
    let text = serde_json::to_string_pretty(measurement).expect("should encode the measurement");
    println!("{text}");
    fs::write(fixture.root.join(MEASUREMENT_FILE), text)
        .expect("should retain the macOS CLI candidate measurement");
}

#[test]
#[ignore = "requires the explicit macOS Cargo fixture, the exported Linux JS handoff, Xcode and a native macOS 15 runner; run separately in producer CI"]
fn macos_cli_candidate_builds_inspects_and_starts_cleanly() {
    // Read before measuring Xcode so a missing selection fails in seconds.
    let native_reproduction = native_reproduction_from_env();
    let asset = job_host_asset();
    let target = asset.artifact().target;
    let fixture = macos_cargo_fixture();
    let provenance = ci_build_provenance(&fixture);
    let commit = CommitId::parse(&fixture.source_commit).expect("should parse the fixture commit");
    let source = authenticate(
        &fixture.root.join("source"),
        &commit,
        &minimal_environment(),
    )
    .expect("should authenticate the fixture's tracked source");
    let locked = LockedMaterials::from_source(&source).expect("should derive the locked materials");
    let exported = std::env::var_os("TS_RELEASE_JS_HANDOFF_ROOT")
        .map(PathBuf::from)
        .expect("should explicitly supply the downloaded JS handoff root");
    let exported = fs::canonicalize(exported).expect("should resolve the JS handoff root");
    let (handoff, js_measurement) =
        JsBuildCompleted::read_exported_ordinary_ci_handoff_for_tests(&exported)
            .expect("should read exactly the exported handoff and measurement");
    let (js, js_handoff) =
        JsBuildCompleted::adopt_ordinary_ci_handoff_for_tests(&source, &handoff, &js_measurement)
            .expect("should adopt the Linux job's handoff for this source");
    // One attempt covers the canonical build, the measured reproduction and
    // the later Apple re-verification, SPDX and index-entry checks. On
    // macos-15-intel the canonical build alone measured 24 minutes and the
    // two-job reproduction is slower, so 60 minutes left no margin.
    let deadline = Instant::now() + Duration::from_secs(120 * 60);
    let cancellation = Cancellation::default();
    let apple = AppleDeveloperTools::for_ordinary_ci(deadline, &cancellation)
        .expect("should measure the selected Xcode");
    let host = measure_host_identity().expect("should measure the macOS host");
    let (audited, removed) = environment_with_operator_values();
    let inputs = MacosInputs {
        fixture: &fixture,
        source: &source,
        locked: &locked,
        js: &js,
        apple: &apple,
        asset,
        audited: &audited,
        deadline,
        cancellation: &cancellation,
    };

    let canonical = build_macos_cli(&inputs, Role::Canonical).unwrap_or_else(|error| {
        panic!("should build the canonical Darwin ts through the confined recipe: {error:?}")
    });
    assert_eq!(
        canonical.completed.descendant_cleanup(),
        DescendantCleanup::ReapedAfterRun,
        "should record the post-run-reaped cleanup"
    );
    let bytes = read_artifact(&canonical.completed).expect("should reread the final bytes");
    let cpu = match target {
        ReleaseTarget::Aarch64AppleDarwin => MachoCpu::Arm64,
        ReleaseTarget::X86_64AppleDarwin => MachoCpu::X86_64,
        other => panic!("should build a Darwin asset, not {}", other.triple()),
    };
    let macho = inspect_macho(&bytes, &MachoPolicy::macos(cpu))
        .expect("should accept the final bytes under the reviewed Mach-O policy");
    macho
        .require_debug_map()
        .expect("should keep the relative debug map");
    assert_eq!(
        macho.sdk(),
        apple.sdk_version(),
        "should record the measured SDK"
    );
    let remap_found = scan_final_bytes(&bytes, &canonical, &fixture.root);
    let planted: Vec<&str> = OPERATOR_SENTINELS.iter().map(|(_, value)| *value).collect();
    check_operator_values_absent(&bytes, &planted)
        .expect("should embed none of the planted operator values");
    let startup = start_natively(&canonical);
    let facts = |reproduction| MacosCliCandidateFacts {
        canonical: &canonical.completed,
        macho: &macho,
        apple: &apple,
        host: &host,
        js_handoff: &js_handoff,
        remap_found: &remap_found,
        operator_values_removed: &removed,
        startup_stdout: startup.macos_stdout(),
        reproduction,
    };
    retain_measurement(
        &fixture,
        &facts(ReproductionOutcome::NotAttempted),
        &provenance,
    );

    let reproduction_build = match native_reproduction {
        NativeReproduction::Measure => Some(build_macos_cli(&inputs, Role::Reproduction)),
        NativeReproduction::Defer => None,
    };
    let paired = reproduction_build.as_ref().map(|build| {
        reproduction_result(
            build
                .as_ref()
                .map_err(|error| recipe_error(format!("{error:?}"))),
            |reproduction| {
                measure_native_repeatability(&canonical.completed, &reproduction.completed)
            },
        )
    });
    let reproduction = match &paired {
        None => ReproductionOutcome::Deferred,
        Some((Ok(measured), _)) => ReproductionOutcome::Measured(measured),
        Some((Err(error), Some(_))) => ReproductionOutcome::InvariantFailed(error.clone()),
        Some((Err(error), None)) => ReproductionOutcome::BuildFailed(error.clone()),
    };
    retain_measurement(&fixture, &facts(reproduction), &provenance);
    if let Some((_, Some(error))) = &paired {
        panic!("should pair builds over identical inputs: {error:?}");
    }
}
