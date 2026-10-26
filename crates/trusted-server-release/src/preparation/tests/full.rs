//! Explicit real Linux fixture; never selected by a normal focused unit run.

use super::*;
use crate::boundary::{DockerSettings, ImageSelection, ProbeProgram};
use crate::documents::read_bounded;
use crate::javascript::JsBuildCompleted;
use crate::preflight::{AuditedEnvironment, audit_environment};
use crate::source::{CommitId, authenticate};

fn host_limits() -> ProcessLimits {
    ProcessLimits {
        wall: Duration::from_secs(300),
        stdout_max: 1024 * 1024,
        stderr_max: 1024 * 1024,
    }
}

/// One real two-build JS fixture run and the authenticated source it built.
///
/// Ordinary CI evidence only: the tools, image and fixture acquisition are
/// unqualified, so nothing here is release qualification.
pub(crate) struct JsFixtureRun {
    /// The explicitly supplied fixture root.
    pub(crate) fixture: PathBuf,
    /// The audited minimal environment the source was authenticated under.
    pub(crate) audited: AuditedEnvironment,
    /// The locally authenticated fixture source.
    pub(crate) source: ApprovedSource,
    /// The locked materials derived from that source.
    pub(crate) locked: LockedMaterials,
    /// The completed JS handoff of build A.
    pub(crate) completed: JsBuildCompleted,
    /// The ordinary CI measurement of both JS builds.
    pub(crate) measurement: Value,
}

/// Runs two fresh offline JS builds from the fixture named by
/// `TS_RELEASE_JS_FIXTURE_ROOT` and returns the retained A handoff.
///
/// # Panics
///
/// Panics when the fixture is missing or any preparation, boundary or build
/// step fails; this is an explicit CI fixture, not a production path.
pub(crate) fn run_two_fresh_js_builds() -> JsFixtureRun {
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
    let audited = audit_environment(&[(OsString::from("PATH"), OsString::from("/usr/bin:/bin"))])
        .expect("should audit minimal source environment");
    let source = authenticate(&fixture.join("source"), &commit, &audited)
        .expect("should authenticate fixture tracked source");
    let locked =
        LockedMaterials::from_source(&source).expect("should derive approved locked npm materials");
    let (completed, measurement) = two_fresh_js_builds(&fixture, &context, &source, &locked);
    JsFixtureRun {
        fixture,
        audited,
        source,
        locked,
        completed,
        measurement,
    }
}

/// Prepares, proves and runs both JS builds, returning the handoff and measurement.
fn two_fresh_js_builds(
    fixture: &Path,
    context: &Value,
    source: &ApprovedSource,
    locked: &LockedMaterials,
) -> (JsBuildCompleted, Value) {
    let cancellation = Cancellation::default();
    let deadline = Instant::now() + Duration::from_secs(3600);
    let originals = locked
        .prepare_npm_originals(&fixture.join("originals"), deadline, &cancellation)
        .expect("should authenticate every original archive and review declarations");
    let tools = SelectedJsTools::for_tests(&fixture.join("tools"))
        .expect("should measure ordinary CI candidate tools");
    let prepared = PreparedJsBuild::new(
        source,
        locked,
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
    let first_roots = prepared
        .boundary_roots(0)
        .expect("should select only A roots");
    let second_roots = prepared
        .boundary_roots(1)
        .expect("should select only B roots");
    let first = Boundary::linux_container(
        first_roots.clone(),
        settings.clone(),
        image.clone(),
        host.path(),
    )
    .expect("should configure A owned PID/network boundary");
    let second = Boundary::linux_container(second_roots.clone(), settings, image, host.path())
        .expect("should configure B disjoint boundary");
    for (own, other) in [(&first_roots, &second_roots), (&second_roots, &first_roots)] {
        let mut shared = 0;
        for (_, _, root) in other.entries() {
            if own.entries().any(|(_, _, own_root)| own_root == root) {
                shared += 1;
                assert!(
                    !own.writable().iter().any(|path| path == root),
                    "should share only read-only roots between A and B"
                );
            } else {
                assert!(
                    own.entries()
                        .all(|(_, _, own_root)| !own_root.starts_with(root)
                            && !root.starts_with(own_root)),
                    "should never expose the other side's private source, installation or metadata root"
                );
            }
        }
        assert_eq!(
            shared, 2,
            "should share exactly the read-only originals and selected tool roots"
        );
    }
    assert!(
        first.matches_roots(&first_roots) && second.matches_roots(&second_roots),
        "should bind each boundary to exactly its own selected roots"
    );
    assert!(
        !first.matches_roots(&second_roots) && !second.matches_roots(&first_roots),
        "should refuse either side's commands inside the other side's boundary"
    );
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
        "should close all six private source, state and metadata roots of both builds before completion"
    );
    assert_eq!(
        completed.source_commit(),
        source.commit().as_str(),
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
    let contributors = completed
        .retained_contributions()
        .expect("should export actual collector origins after private-root cleanup");
    assert_eq!(
        contributors.inventory_sha256(),
        completed.inventory_sha256(),
        "should preserve the original collector inventory identity"
    );
    assert_eq!(
        contributors.modules().len(),
        module_count,
        "should export every actual final module with its conservative origins"
    );
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
    (completed, measurement)
}

/// Runs the two fresh JS builds and exports the retained handoff and its
/// measurement into the fresh directory named by
/// `TS_RELEASE_JS_HANDOFF_EXPORT`, for the macOS candidate jobs to adopt.
///
/// The export is an unauthenticated ordinary CI artifact; adoption labels it
/// so, and nothing here is release qualification.
#[test]
#[ignore = "requires explicit original-only fixture, an export destination and Linux Docker; run separately in producer CI"]
fn export_js_handoff_for_candidates() {
    let destination = std::env::var_os("TS_RELEASE_JS_HANDOFF_EXPORT")
        .map(PathBuf::from)
        .expect("should explicitly supply the JS handoff export destination");
    let run = run_two_fresh_js_builds();
    run.completed
        .export_ordinary_ci_handoff_for_tests(&run.measurement, &destination)
        .expect("should export the rehashed handoff and its measurement");
    let (handoff, measurement) =
        JsBuildCompleted::read_exported_ordinary_ci_handoff_for_tests(&destination)
            .expect("should read back exactly the exported handoff and measurement");
    JsBuildCompleted::adopt_ordinary_ci_handoff_for_tests(&run.source, &handoff, &measurement)
        .expect("should adopt the export as the macOS jobs will");
    println!(
        "{}",
        serde_json::to_string_pretty(&run.measurement).expect("should encode the JS measurement")
    );
}

#[test]
#[ignore = "requires explicit original-only fixture and Linux Docker; run separately in producer CI"]
fn real_two_fresh_js_builds() {
    let run = run_two_fresh_js_builds();
    fs::write(
        run.fixture.join("js-execution-measurement.json"),
        serde_json::to_vec_pretty(&run.measurement).expect("should encode actual CI measurement"),
    )
    .expect("should retain actual CI measurement after successful handoff");
}
