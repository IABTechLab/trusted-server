//! Explicit real Linux fixture; never selected by a normal focused unit run.

use super::*;
use crate::boundary::{DockerSettings, ImageSelection, ProbeProgram};
use crate::documents::read_bounded;
use crate::preflight::audit_environment;
use crate::source::{CommitId, authenticate};

fn host_limits() -> ProcessLimits {
    ProcessLimits {
        wall: Duration::from_secs(300),
        stdout_max: 1024 * 1024,
        stderr_max: 1024 * 1024,
    }
}

#[test]
#[ignore = "requires explicit original-only fixture and Linux Docker; run separately in producer CI"]
fn real_two_fresh_js_builds() {
    let fixture = std::env::var_os("TS_RELEASE_JS_FIXTURE_ROOT")
        .map(PathBuf::from)
        .expect("should explicitly supply original-only fixture root");
    let fixture = fs::canonicalize(fixture).expect("should resolve fixture root");
    let context = read_bounded(&fixture.join("fixture.json"), Role::Metadata)
        .expect("should read fixture context");
    let context = parse_json(&context.bytes, Role::Metadata).expect("should parse fixture context");
    let commit = CommitId::parse(
        context["source_commit"]
            .as_str()
            .expect("should retain source identity"),
    )
    .expect("should parse independently selected source identity");
    let environment =
        audit_environment(&[(OsString::from("PATH"), OsString::from("/usr/bin:/bin"))])
            .expect("should audit minimal source environment");
    let source = authenticate(&fixture.join("source"), &commit, &environment)
        .expect("should authenticate fixture tracked source");
    let locked =
        LockedMaterials::from_source(&source).expect("should derive approved locked npm materials");
    let cancellation = Cancellation::default();
    let deadline = Instant::now() + Duration::from_secs(3600);
    let originals = locked
        .prepare_npm_originals(&fixture.join("originals"), deadline, &cancellation)
        .expect("should authenticate every original archive and review declarations");
    let tools = SelectedJsTools::for_tests(&fixture.join("tools"))
        .expect("should measure ordinary CI candidate tools");
    let prepared = PreparedJsBuild::new(
        &source,
        &locked,
        &originals,
        tools,
        JsAttempt::new(deadline, cancellation),
    )
    .expect("should own two independent fresh installations");
    let host = tempfile::tempdir().expect("should create disjoint boundary host state");
    let image = ImageSelection::Test {
        id: context["image_id"]
            .as_str()
            .expect("should select the already-built ordinary CI fixture image")
            .to_owned(),
    };
    // These IDs only select the existing unprivileged CI worker account.
    let user = unsafe { (libc::getuid(), libc::getgid()) };
    let settings = DockerSettings {
        client: PathBuf::from("/usr/bin/docker"),
        cpus: 1,
        memory_bytes: 4 * 1024 * 1024 * 1024,
        pids_limit: 512,
        tmpfs_bytes: 256 * 1024 * 1024,
        user,
        stop_timeout_seconds: 5,
        host_limits: host_limits(),
    };
    let first = Boundary::linux_container(
        prepared
            .boundary_roots(0)
            .expect("should select only A roots"),
        settings.clone(),
        image.clone(),
        host.path(),
    )
    .expect("should configure A owned PID/network boundary");
    let second = Boundary::linux_container(
        prepared
            .boundary_roots(1)
            .expect("should select only B roots"),
        settings,
        image,
        host.path(),
    )
    .expect("should configure B disjoint boundary");
    let probe = ProbeProgram::from_staged_release_binary(&fixture.join("tools/probe"))
        .expect("should select staged native producer probe");
    let limits = ProcessLimits {
        wall: Duration::from_secs(60),
        stdout_max: 4096,
        stderr_max: 4096,
    };
    let first_proof = first
        .establish_denial(&probe, limits)
        .expect("should establish real A positive and denied socket controls");
    let second_proof = second
        .establish_denial(&probe, limits)
        .expect("should establish independent real B controls");
    let owned_roots: Vec<_> = prepared
        .sides
        .iter()
        .flat_map(|side| {
            [
                side.source.root().to_owned(),
                side.state.path().to_owned(),
                side.metadata.path().to_owned(),
            ]
        })
        .collect();
    let module_count = prepared.module_ids.len();
    let completed=prepared.run([&first,&second],[&first_proof,&second_proof]).expect("should complete both actual fresh offline installations, lifecycle observations, negative tool controls, full suites and final-byte equality");
    assert!(
        owned_roots.iter().all(|root| !root.exists()),
        "should close all six private A/B source, state and metadata roots before completion"
    );
    assert_eq!(
        completed.source_commit(),
        commit.as_str(),
        "should bind actual approved source"
    );
    assert!(
        completed.test_counts().iter().all(|count| *count > 0),
        "should execute nonzero full registered suites on both sides"
    );
    assert_eq!(
        completed.tools_sha256().len(),
        64,
        "should bind measured complete tool tree"
    );
    assert_eq!(
        completed.settings_sha256().len(),
        64,
        "should bind fixed execution settings"
    );
    assert_eq!(
        completed.inventory_sha256().len(),
        64,
        "should retain original A inventory digest"
    );
    completed
        .rehash_for_handoff()
        .expect("should rejoin retained A bytes before downstream handoff");
    let measurement = json!({
        "schema_version": 1,
        "kind": "ordinary-ci-two-fresh-js-builds",
        "source_commit": completed.source_commit(),
        "test_counts": completed.test_counts(),
        "module_count": module_count,
        "tools_sha256": completed.tools_sha256(),
        "settings_sha256": completed.settings_sha256(),
        "inventory_sha256": completed.inventory_sha256(),
        "private_roots_removed": owned_roots.len(),
        "release_qualification": false,
    });
    fs::write(
        fixture.join("js-execution-measurement.json"),
        serde_json::to_vec_pretty(&measurement).expect("should encode actual CI measurement"),
    )
    .expect("should retain actual CI measurement after successful handoff");
}
