//! Explicit real Linux CLI candidate fixture; never selected by a normal
//! focused unit run.
//!
//! The run is ordinary CI candidate evidence (`release_qualification:
//! false`): the toolchain, builder and startup images and fixture
//! acquisition are unqualified and unauthenticated, so nothing here
//! qualifies a release. The canonical build is gated (ELF policy, debug
//! info, remap scan, planted operator values, clean startup); the
//! reproduction build is a recorded measurement only, and runs only when
//! `TS_RELEASE_NATIVE_REPRODUCTION=measure` (otherwise the measurement
//! records it as deferred to the scheduled run).

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::super::compare::read_artifact;
use super::super::native::{check_operator_values_absent, measure_native_repeatability};
use super::super::remap::scan_remapped_bytes;
use super::super::startup::{LINUX_CLI_ASSET, NativeStartupAccepted, StartupCopy};
use super::candidate::js_inventory_sha256;
use super::candidate::{CargoFixture, cargo_docker_settings, ci_build_provenance};
use super::cli_candidate::{
    CliCandidateFacts, CliFixture, NativeReproduction, OPERATOR_SENTINELS, ReproductionOutcome,
    check_build_budget, cli_candidate_measurement, finish_cleanup, native_reproduction_from_env,
    new_startup_client, reproduction_result,
};
use super::full::try_proven_boundary;
use super::real_fixture::{
    cargo_fixture, environment_with_operator_values, path_text, try_ordinary_ci_ambient,
    try_prepare_vendor,
};
use super::*;
use crate::boundary::{Boundary, BoundaryRoots, ConfinedCommand, ImageSelection, ProbeProgram};
use crate::documents::{parse_json, read_bounded};
use crate::inspect::{ElfPolicy, inspect_elf};
use crate::preflight::AuditedEnvironment;
use crate::preparation::tests::full::{JsFixtureRun, run_two_fresh_js_builds};
use crate::process::{Cancellation, ProcessLimits};

/// Reads the ordinary CI CLI fixture named by `TS_RELEASE_CLI_FIXTURE_ROOT`.
fn cli_fixture() -> (PathBuf, CliFixture) {
    let root = std::env::var_os("TS_RELEASE_CLI_FIXTURE_ROOT")
        .map(PathBuf::from)
        .expect("should explicitly supply the ordinary CI CLI fixture root");
    let root = fs::canonicalize(root).expect("should resolve the CLI fixture root");
    let raw = read_bounded(&root.join("cli-fixture.json"), DocumentRole::Metadata)
        .expect("should read the CLI fixture description");
    let document =
        parse_json(&raw.bytes, DocumentRole::Metadata).expect("should parse the description");
    let fixture = CliFixture::parse(&document).expect("should accept the CLI fixture description");
    (root, fixture)
}

/// Everything shared by both CLI builds.
struct CliInputs<'a> {
    js: &'a JsFixtureRun,
    fixture: &'a CargoFixture,
    cli: &'a CliFixture,
    audited: &'a AuditedEnvironment,
    deadline: Instant,
    cancellation: &'a Cancellation,
}

/// One confined CLI build with the private roots its output lives in.
struct CliBuild {
    _materials: tempfile::TempDir,
    _work: tempfile::TempDir,
    completed: RecipeCompleted,
    /// Host roots of this build that must not appear in the final bytes.
    host_roots: Vec<String>,
}

/// Builds the Linux `ts` for `role` from the shared JS handoff in the
/// glibc 2.34 builder image.
///
/// Each role gets its own fresh source copy, vendor tree, state, output and
/// target roots and its own proven boundary. Setup, execution and explicit
/// source/target cleanup failures are returned so reproduction is optional.
fn build_cli(inputs: &CliInputs<'_>, role: Role) -> Result<CliBuild, Report<ReleaseError>> {
    check_build_budget(inputs.deadline, inputs.cancellation)?;
    let fresh = inputs
        .js
        .source
        .materialize(inputs.deadline, inputs.cancellation)?;
    let mut materials_directory = None;
    let mut work = None;
    let mut target = None;
    let built = (|| {
        materials_directory = Some(tempfile::tempdir().map_err(fixture_io)?);
        let materials_root = fs::canonicalize(
            materials_directory
                .as_ref()
                .ok_or_else(|| recipe_error("missing materials root"))?
                .path(),
        )
        .map_err(fixture_io)?;
        let materials = try_prepare_vendor(
            inputs.fixture,
            &inputs.js.locked,
            &materials_root,
            inputs.deadline,
            inputs.cancellation,
        )?;
        work = Some(tempfile::tempdir().map_err(fixture_io)?);
        let base = fs::canonicalize(
            work.as_ref()
                .ok_or_else(|| recipe_error("missing work root"))?
                .path(),
        )
        .map_err(fixture_io)?;
        target = Some(base.join("target"));
        for directory in ["cargo-state", "output"] {
            fs::create_dir(base.join(directory)).map_err(fixture_io)?;
        }
        let retained =
            fs::canonicalize(inputs.js.completed.rehash_for_handoff()?).map_err(fixture_io)?;
        let request = RecipeRequest::parse(&RecipeInput {
            asset: LINUX_CLI_ASSET,
            variant: "default",
            role: role.spec_label(),
            commit: &inputs.fixture.source_commit,
            js_dir: &retained.join("bundles"),
            js_inventory: &retained.join("tsjs-inventory.json"),
            materials: &materials_root,
            cargo_state: &base.join("cargo-state"),
            output: &base.join("output"),
            target_dir: &base.join("target"),
        })?;
        let prepared = PreparedRecipe::new(
            &inputs.js.source,
            &fresh,
            inputs.audited,
            &inputs.js.completed,
            SelectedRustTools::for_tests(
                &inputs.fixture.tools(),
                ReleaseTarget::X86_64UnknownLinuxGnu,
            )?,
            materials,
            try_ordinary_ci_ambient(role, fresh.root())?,
            request,
        )?;
        let proven = try_proven_boundary(&prepared, inputs.fixture, &inputs.cli.builder_image_id)?;
        let host_roots = [
            fresh.root(),
            materials_root.as_path(),
            base.as_path(),
            retained.as_path(),
        ]
        .iter()
        .map(|root| {
            root.to_str()
                .map(str::to_owned)
                .ok_or_else(|| recipe_error("CLI root is not UTF-8"))
        })
        .collect::<Result<Vec<_>, _>>()?;
        let completed = prepared.execute(&proven.boundary, &proven.proof)?;
        completed.rehash_for_handoff()?;
        Ok((completed, host_roots))
    })();
    // Attempt every cleanup even when setup, execution, or another cleanup fails.
    let built = finish_cleanup(built, fresh.close());
    let built = finish_cleanup(
        built,
        target
            .as_ref()
            .map_or(Ok(()), |target| match fs::remove_dir_all(target) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(fixture_io(error)),
            }),
    );
    let built = built.and_then(|(completed, host_roots)| {
        completed.denial_receipt()?;
        completed.recipe_definition()?;
        Ok((completed, host_roots))
    });
    match built {
        Ok((completed, host_roots)) => Ok(CliBuild {
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

fn fixture_io(error: std::io::Error) -> Report<ReleaseError> {
    Report::new(ReleaseError::Recipe)
        .attach("CLI fixture filesystem operation failed")
        .attach(error)
}

fn close_directory(directory: Option<tempfile::TempDir>) -> Result<(), Report<ReleaseError>> {
    directory.map_or(Ok(()), |directory| directory.close().map_err(fixture_io))
}

/// Scans the final bytes for every host and fixture root and the container
/// prefix, and returns the fixed remapped prefixes found.
fn scan_final_bytes(bytes: &[u8], build: &CliBuild, fixture_roots: &[&Path]) -> Vec<&'static str> {
    let mut forbidden = build.host_roots.clone();
    forbidden.extend(fixture_roots.iter().map(|root| path_text(root)));
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

/// Starts the exact final bytes as `ts --version` in every clean image.
fn start_everywhere(completed: &RecipeCompleted, cli: &CliFixture) -> NativeStartupAccepted {
    let scratch = tempfile::tempdir().expect("should create startup scratch space");
    let base = fs::canonicalize(scratch.path()).expect("should resolve startup scratch space");
    let copy_directory = base.join("startup-copy");
    let copy = StartupCopy::create(completed, &copy_directory)
        .expect("should copy the final bytes read-only");
    let observed = (|| {
        let (_state, client) = new_startup_client(Path::new("/usr/bin/docker"), &copy_directory)?;
        NativeStartupAccepted::accept_linux(completed, copy, &client, &cli.startup_images)
    })();
    let observed = observed.expect("should start every clean image and remove the startup copy");
    completed
        .rehash_for_handoff()
        .expect("should leave the accepted artifact unchanged");
    observed
}

#[test]
#[ignore = "requires explicit JS, Cargo and CLI ordinary CI fixtures and Linux Docker; run separately in producer CI"]
fn linux_cli_candidate_builds_inspects_and_starts_cleanly() {
    // Read before any build so a missing selection fails in seconds.
    let native_reproduction = native_reproduction_from_env();
    let fixture = cargo_fixture();
    let provenance = ci_build_provenance(&fixture);
    let (cli_root, cli) = cli_fixture();
    preflight_cli_builder(&fixture, &cli, &cli_root)
        .expect("should load the staged probe and Rust tools on glibc 2.34 before JS builds");
    let js = run_two_fresh_js_builds();
    assert_eq!(
        js.source.commit().as_str(),
        fixture.source_commit,
        "should build the Cargo fixture's source commit"
    );
    let (audited, removed) = environment_with_operator_values();
    let cancellation = Cancellation::default();
    let inputs = CliInputs {
        js: &js,
        fixture: &fixture,
        cli: &cli,
        audited: &audited,
        deadline: Instant::now() + Duration::from_secs(50 * 60),
        cancellation: &cancellation,
    };
    let canonical = build_cli(&inputs, Role::Canonical).unwrap_or_else(|error| {
        panic!("should build the canonical Linux ts through the confined recipe: {error:?}")
    });
    let bytes = read_artifact(&canonical.completed).expect("should reread the final bytes");
    let elf = inspect_elf(&bytes, &ElfPolicy::linux_x86_64_gnu())
        .expect("should accept the final bytes under the glibc 2.34 ELF policy");
    elf.require_debug_sections()
        .expect("should keep the profile's debug info");
    let remap_found = scan_final_bytes(&bytes, &canonical, &[&fixture.root, &cli_root]);
    let planted: Vec<&str> = OPERATOR_SENTINELS.iter().map(|(_, value)| *value).collect();
    check_operator_values_absent(&bytes, &planted)
        .expect("should embed none of the planted operator values");
    let startup = start_everywhere(&canonical.completed, &cli);
    let mut canonical_measurement = cli_candidate_measurement(&CliCandidateFacts {
        canonical: &canonical.completed,
        builder: &cli,
        elf: &elf,
        remap_found: &remap_found,
        operator_values_removed: &removed,
        startup: startup
            .linux_runs()
            .expect("should retain actual Linux startup runs"),
        reproduction: ReproductionOutcome::NotAttempted,
    });
    let inventory_sha256 = js_inventory_sha256(&canonical.completed);
    canonical_measurement["js_inventory_sha256"] = serde_json::json!(inventory_sha256);
    canonical_measurement["ci_build_provenance"] = provenance.clone();
    fs::write(
        cli_root.join("linux-cli-candidate-measurement.json"),
        serde_json::to_vec_pretty(&canonical_measurement)
            .expect("should encode the canonical measurement"),
    )
    .expect("should retain accepted canonical evidence before optional reproduction");
    let reproduction_build = match native_reproduction {
        NativeReproduction::Measure => Some(build_cli(&inputs, Role::Reproduction)),
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
    let mut measurement = cli_candidate_measurement(&CliCandidateFacts {
        canonical: &canonical.completed,
        builder: &cli,
        elf: &elf,
        remap_found: &remap_found,
        operator_values_removed: &removed,
        startup: startup
            .linux_runs()
            .expect("should retain actual Linux startup runs"),
        reproduction,
    });
    measurement["js_inventory_sha256"] = serde_json::json!(inventory_sha256);
    measurement["ci_build_provenance"] = provenance;
    retain_cli_measurement(&cli_root, &measurement);
    if let Some((_, Some(error))) = &paired {
        panic!("should pair builds over identical inputs: {error:?}");
    }
}

/// Retains `measurement` as the Linux CLI candidate measurement.
fn retain_cli_measurement(cli_root: &Path, measurement: &serde_json::Value) {
    let text = serde_json::to_string_pretty(measurement).expect("should encode the measurement");
    fs::write(cli_root.join("linux-cli-candidate-measurement.json"), text)
        .expect("should retain the Linux CLI candidate measurement");
}

/// Loads the staged probe and copied Rust tools on the builder's ABI before JS work.
fn preflight_cli_builder(
    fixture: &CargoFixture,
    cli: &CliFixture,
    evidence_root: &Path,
) -> Result<(), Report<ReleaseError>> {
    let work = tempfile::tempdir().map_err(fixture_io)?;
    let state = tempfile::tempdir().map_err(fixture_io)?;
    let scripts = tempfile::tempdir().map_err(fixture_io)?;
    let root = fs::canonicalize(work.path()).map_err(fixture_io)?;
    let result = (|| {
        let script_root = fs::canonicalize(scripts.path()).map_err(fixture_io)?;
        let script = script_root.join("filesystem-controls.sh");
        fs::write(&script, FILESYSTEM_CONTROLS).map_err(fixture_io)?;
        fs::set_permissions(&script, fs::Permissions::from_mode(0o555)).map_err(fixture_io)?;
        for directory in ["home", "tmp", "cargo-home"] {
            fs::create_dir(root.join(directory)).map_err(fixture_io)?;
        }
        let roots = BoundaryRoots::new(
            &[script_root],
            &[fixture.tools()],
            std::slice::from_ref(&root),
        )?;

        // These IDs select the existing unprivileged ordinary CI worker.
        let user = unsafe { (libc::getuid(), libc::getgid()) };
        let boundary = Boundary::linux_container(
            roots,
            cargo_docker_settings(user, 1),
            ImageSelection::Test {
                id: cli.builder_image_id.clone(),
            },
            state.path(),
        )?;
        let limits = ProcessLimits {
            wall: Duration::from_secs(60),
            stdout_max: 4096,
            stderr_max: 4096,
        };
        let probe = ProbeProgram::from_staged_release_binary(&fixture.probe())?;
        let proof = boundary.establish_denial(&probe, limits)?;
        let controls = ConfinedCommand::new(&script, &root, limits)?
            .environment("HOME", boundary.execution_path(&root.join("home"))?)?
            .environment("TMPDIR", boundary.execution_path(&root.join("tmp"))?)?
            .environment(
                "CARGO_HOME",
                boundary.execution_path(&root.join("cargo-home"))?,
            )?
            .environment("LC_ALL", "C")?;
        let observed = boundary.run_confined(&controls, &proof)?;
        let evidence = serde_json::json!({
            "kind": "ordinary-ci-builder-filesystem-controls",
            "builder_image_id": cli.builder_image_id,
            "stdout": String::from_utf8_lossy(observed.stdout()),
            "stderr": String::from_utf8_lossy(observed.stderr()),
            "outcome": format!("{:?}", observed.outcome()),
            "release_qualification": false,
        });
        let evidence = serde_json::to_vec_pretty(&evidence)
            .map_err(|error| recipe_error(format!("cannot encode filesystem controls: {error}")))?;
        fs::write(
            evidence_root.join("builder-filesystem-controls.json"),
            evidence,
        )
        .map_err(fixture_io)?;

        let stdout = observed.require_success()?;
        if stdout != FILESYSTEM_CONTROL_OUTPUT.as_bytes() {
            return Err(recipe_error(
                "filesystem controls did not report every denied root and bound TMPDIR success",
            ));
        }
        if root
            .join("tmp/ts-release-filesystem-control")
            .try_exists()
            .map_err(fixture_io)?
        {
            return Err(recipe_error(
                "filesystem controls left the bound temporary file behind",
            ));
        }
        for tool in ["rustc", "cargo"] {
            let command =
                ConfinedCommand::new(&fixture.tools().join("bin").join(tool), &root, limits)?
                    .arguments([OsString::from("--version")])
                    .environment("LC_ALL", "C")?;
            boundary.run_confined(&command, &proof)?.require_success()?;
        }
        Ok(())
    })();
    let result = finish_cleanup(result, scripts.close().map_err(fixture_io));
    let result = finish_cleanup(result, state.close().map_err(fixture_io));
    finish_cleanup(result, work.close().map_err(fixture_io))
}

/// Fixed ordinary CI controls: redirections test permissions without external helpers.
const FILESYSTEM_CONTROLS: &str = r#"#!/bin/sh
set -eu
for directory in /tmp /dev/shm /dev; do
    if ( : > "$directory/ts-release-filesystem-control" ); then
        printf 'unexpected writable root %s\n' "$directory" >&2
        exit 1
    fi
    printf 'denied %s\n' "$directory"
done
for file in /etc/hosts /etc/hostname /etc/resolv.conf; do
    [ -f "$file" ]
    if ( : >> "$file" ); then
        printf 'unexpected writable daemon file %s\n' "$file" >&2
        exit 1
    fi
    printf 'denied %s\n' "$file"
done
path=$TMPDIR/ts-release-filesystem-control

printf 'bound temporary root\n' > "$path"
IFS= read -r observed < "$path"
[ "$observed" = 'bound temporary root' ]
/usr/bin/rm -- "$path"
[ ! -e "$path" ]
printf 'bound TMPDIR write read cleanup accepted\n'
"#;

/// Every expected control must report success, in the fixed script's order.
const FILESYSTEM_CONTROL_OUTPUT: &str = "denied /tmp\ndenied /dev/shm\ndenied /dev\ndenied /etc/hosts\ndenied /etc/hostname\ndenied /etc/resolv.conf\nbound TMPDIR write read cleanup accepted\n";
