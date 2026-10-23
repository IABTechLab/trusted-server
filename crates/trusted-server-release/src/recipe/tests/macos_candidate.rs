//! Ordinary CI macOS CLI candidate support: the host asset selection and the
//! measurement the real macOS fixture prints.
//!
//! Pure and compiled everywhere. Nothing here is release qualification: the
//! Xcode, toolchain and adopted JS handoff are unqualified and
//! unauthenticated, descendants are only reaped after each run, and the
//! measurement says so.

use serde_json::{Value, json};

use super::super::startup::native::HostIdentity;
use super::candidate::{SCHEMA_VERSION, cargo_measurement};
use super::cli_candidate::{ReproductionOutcome, reproduction_measurement};
use super::complete::built;
use super::complete_darwin::darwin_executable;
use super::fixture::Fixture;
use super::*;
use crate::inspect::{MachoCpu, MachoFacts, MachoPolicy, inspect_macho};

/// File the macOS CLI candidate measurement is written to.
pub(super) const MEASUREMENT_FILE: &str = "macos-cli-candidate-measurement.json";
/// Kind of the macOS CLI candidate measurement.
const MEASUREMENT_KIND: &str = "ordinary-ci-macos-cli-candidate";
/// The fixed environment names of the native startup child.
const STARTUP_ENVIRONMENT: [&str; 4] = ["HOME", "LC_ALL", "PATH", "TMPDIR"];

/// Selects the Darwin `ts` asset of the running `architecture` and requires
/// it to build for `host_target`, the triple the CI job asserted.
///
/// # Errors
///
/// Returns [`ReleaseError::Recipe`] for an architecture with no Darwin `ts`
/// asset, or an asset whose target is not `host_target` (such as an arm64
/// producer on a job that asserted an Intel host).
pub(super) fn darwin_cli_asset(
    architecture: &str,
    host_target: &str,
) -> Result<AssetId, Report<ReleaseError>> {
    let name = match architecture {
        "aarch64" => "ts-aarch64-apple-darwin",
        "x86_64" => "ts-x86_64-apple-darwin",
        other => {
            return Err(recipe_error(format!(
                "no Darwin ts asset for architecture {other:?}"
            )));
        }
    };
    let asset = AssetId::parse(name)?;
    if asset.artifact().target.triple() != host_target {
        return Err(recipe_error(format!(
            "Darwin ts asset {name} does not build for the host target {host_target:?}"
        )));
    }
    Ok(asset)
}

/// The Mach-O facts the measurement records.
fn macho_measurement(facts: &MachoFacts) -> Value {
    json!({
        "sha256": facts.sha256(),
        "cpu": facts.cpu().to_string(),
        "minos": facts.minos().to_string(),
        "sdk": facts.sdk().to_string(),
        "dylibs": facts.dylibs(),
        "uuid": hex::encode(facts.uuid()),
        "symbol_count": facts.symbol_count(),
        "debug_map_objects": facts.debug_map_objects(),
        "signature": facts.signature().to_string(),
    })
}

/// The host identity the measurement records.
fn host_measurement(host: &HostIdentity) -> Value {
    json!({
        "product_version": host.product_version,
        "architecture": host.architecture,
        "proc_translated": host.proc_translated,
        "hw_optional_arm64": host.hw_optional_arm64,
    })
}

/// Everything the macOS CLI candidate measurement states.
pub(super) struct MacosCliCandidateFacts<'a> {
    pub(super) canonical: &'a RecipeCompleted,
    pub(super) macho: &'a MachoFacts,
    pub(super) apple: &'a AppleDeveloperTools,
    pub(super) host: &'a HostIdentity,
    /// The adoption label of the Linux job's JS handoff.
    pub(super) js_handoff: &'a Value,
    pub(super) remap_found: &'a [&'static str],
    pub(super) operator_values_removed: &'a [String],
    /// The accepted `ts --version` output, once the native startup ran.
    pub(super) startup_stdout: Option<&'a str>,
    pub(super) reproduction: ReproductionOutcome<'a>,
}

/// The ordinary CI measurement of the macOS CLI candidate.
pub(super) fn macos_cli_candidate_measurement(facts: &MacosCliCandidateFacts<'_>) -> Value {
    let startup = facts.startup_stdout.map_or_else(
        || json!({"status": "not-run"}),
        |stdout| {
            json!({
                "status": "accepted",
                "exit_code": 0,
                "stdout": stdout,
                "environment": STARTUP_ENVIRONMENT,
            })
        },
    );
    json!({
        "schema_version": SCHEMA_VERSION,
        "kind": MEASUREMENT_KIND,
        "asset": facts.canonical.asset().artifact().name,
        "source_commit": facts.canonical.commit().as_str(),
        "host": host_measurement(facts.host),
        "apple_developer_tools": facts.apple.measurement(),
        "js_handoff": facts.js_handoff["js_handoff"],
        "boundary": "macos-sandbox-exec",
        "descendant_cleanup": facts.canonical.descendant_cleanup().to_string(),
        "canonical": cargo_measurement(facts.canonical),
        "macho": macho_measurement(facts.macho),
        "remap_scan": {
            "forbidden": ["host build roots", "fixture root", "RUNNER_TEMP"],
            "found": facts.remap_found,
        },
        "operator_values": {
            "removed_by_preflight": facts.operator_values_removed,
            "found_in_final_bytes": 0,
        },
        "startup": startup,
        "reproduction": reproduction_measurement(&facts.reproduction),
        "release_qualification": false,
    })
}

#[test]
fn darwin_cli_asset_follows_the_host_architecture_and_target() {
    for (architecture, triple) in [
        ("aarch64", "aarch64-apple-darwin"),
        ("x86_64", "x86_64-apple-darwin"),
    ] {
        let asset = darwin_cli_asset(architecture, triple).expect("should select the host asset");
        assert_eq!(
            asset.artifact().target.triple(),
            triple,
            "should build the asset for the host target"
        );
        assert_eq!(asset.artifact().bin, "ts", "should select the ts binary");
    }
    for (architecture, triple) in [
        ("aarch64", "x86_64-apple-darwin"),
        ("x86_64", "aarch64-apple-darwin"),
        ("x86_64", "x86_64-unknown-linux-gnu"),
        ("riscv64", "riscv64-example-darwin"),
    ] {
        assert!(
            darwin_cli_asset(architecture, triple).is_err(),
            "should reject {architecture} on a job asserting {triple}"
        );
    }
}

#[test]
fn macos_measurement_states_unqualified_reaped_unauthenticated_evidence() {
    let bytes = darwin_executable();
    let mut build = built("ts-aarch64-apple-darwin", Role::Canonical, &bytes);
    let stdout = build.stdout(|_| {});
    let canonical = build
        .finish(stdout)
        .expect("should complete the Darwin fixture");
    let macho = inspect_macho(&bytes, &MachoPolicy::macos(MachoCpu::Arm64))
        .expect("should inspect the canonical bytes");
    let apple = Fixture::new(COMMIT).apple_tools();
    let host = HostIdentity {
        product_version: "15.5".to_owned(),
        architecture: "aarch64".to_owned(),
        proc_translated: Some(0),
        hw_optional_arm64: Some(1),
    };
    let js_handoff = json!({
        "js_handoff": {
            "origin": "linux-ordinary-ci-artifact",
            "authenticated": false,
            "source_commit": COMMIT,
        }
    });
    let removed = vec!["TRUSTED_SERVER__PUBLISHER__DOMAIN".to_owned()];

    let measurement = macos_cli_candidate_measurement(&MacosCliCandidateFacts {
        canonical: &canonical,
        macho: &macho,
        apple: &apple,
        host: &host,
        js_handoff: &js_handoff,
        remap_found: &["/trusted-server"],
        operator_values_removed: &removed,
        startup_stdout: Some("ts example\n"),
        reproduction: ReproductionOutcome::BuildFailed("reproduction unavailable".to_owned()),
    });

    assert_eq!(measurement["kind"], "ordinary-ci-macos-cli-candidate");
    assert_eq!(
        measurement["release_qualification"], false,
        "should never claim release qualification"
    );
    assert_eq!(
        measurement["descendant_cleanup"], "post-run-reaped",
        "should state the weaker macOS cleanup"
    );
    assert_eq!(measurement["asset"], "ts-aarch64-apple-darwin");
    assert_eq!(measurement["js_handoff"]["authenticated"], false);
    assert_eq!(
        measurement["js_handoff"]["origin"],
        "linux-ordinary-ci-artifact"
    );
    assert_eq!(measurement["host"]["architecture"], "aarch64");
    assert_eq!(measurement["host"]["proc_translated"], 0);
    assert_eq!(
        measurement["apple_developer_tools"]["xcode_version"],
        apple.measurement()["xcode_version"],
        "should record the measured Xcode"
    );
    assert_eq!(
        measurement["apple_developer_tools"]["clang_sha256"],
        apple.measurement()["clang_sha256"],
        "should record the tool hashes"
    );
    assert_eq!(measurement["macho"]["signature"], "linker-signed-ad-hoc");
    assert_eq!(
        measurement["canonical"]["artifact"]["sha256"],
        canonical.artifact().sha256(),
        "should record the accepted canonical artifact"
    );
    assert_eq!(measurement["startup"]["status"], "accepted");
    assert_eq!(measurement["startup"]["stdout"], "ts example\n");
    assert_eq!(
        measurement["reproduction"]["status"], "build-failed",
        "should record an unavailable reproduction as a measurement"
    );

    let pending = macos_cli_candidate_measurement(&MacosCliCandidateFacts {
        startup_stdout: None,
        reproduction: ReproductionOutcome::NotAttempted,
        canonical: &canonical,
        macho: &macho,
        apple: &apple,
        host: &host,
        js_handoff: &js_handoff,
        remap_found: &[],
        operator_values_removed: &removed,
    });
    assert_eq!(pending["startup"]["status"], "not-run");
    assert_eq!(pending["reproduction"]["status"], "not-attempted");

    let deferred = macos_cli_candidate_measurement(&MacosCliCandidateFacts {
        startup_stdout: Some("ts example\n"),
        reproduction: ReproductionOutcome::Deferred,
        canonical: &canonical,
        macho: &macho,
        apple: &apple,
        host: &host,
        js_handoff: &js_handoff,
        remap_found: &[],
        operator_values_removed: &removed,
    });
    assert_eq!(
        deferred["reproduction"]["status"], "deferred-to-scheduled-run",
        "should record a deferred reproduction explicitly"
    );
    assert_eq!(
        deferred["reproduction"]["gate"], "measurement-only",
        "should keep a deferred reproduction a measurement"
    );
    assert_eq!(
        deferred["release_qualification"], false,
        "should never claim release qualification when deferring"
    );
}
