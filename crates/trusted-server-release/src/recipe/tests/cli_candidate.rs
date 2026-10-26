//! Ordinary CI Linux CLI candidate support: the CLI fixture description and
//! the measurement the real Linux fixture prints.
//!
//! Nothing here is release qualification. The images are acquired without
//! authentication and selected only by local image ID, the toolchain is
//! unqualified, and the measurement says so.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Instant;

use crate::process::Cancellation;

use serde_json::{Value, json};

use super::super::native::NativeRepeatability;
use super::super::startup::{
    StartupClient, StartupDistribution, StartupImage, StartupObserved, is_local_image_id,
};
use super::candidate::{SCHEMA_VERSION, cargo_measurement};
use super::complete::built;
use super::*;
use crate::inspect::ElfFacts;
use crate::inspect::tests::builders::{ElfSpec, elf};
use crate::inspect::{ElfPolicy, inspect_elf};

/// Kind of the CLI fixture description.
const FIXTURE_KIND: &str = "ordinary-ci-cli-fixture";
/// How the fixture images were acquired.
const ACQUISITION: &str = "unauthenticated-ordinary-ci";
/// Every field `cli-fixture.json` must hold, and nothing else.
const FIXTURE_FIELDS: [&str; 6] = [
    "acquisition",
    "builder_glibc",
    "builder_image_id",
    "kind",
    "schema_version",
    "startup_images",
];
/// Base image of the ordinary CI builder; the tracked Dockerfile must use it.
pub(super) const CLI_BUILDER_BASE: &str = "registry.access.redhat.com/ubi9/ubi:9.6";
/// The tracked ordinary CI builder Dockerfile.
const CLI_BUILDER_DOCKERFILE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../scripts/release/tests/cli-builder.Dockerfile"
));
/// Distinctive fictional operator values planted in the CLI build's
/// inherited environment; preflight must remove them and the final bytes
/// must hold none.
pub(super) const OPERATOR_SENTINELS: [(&str, &str); 3] = [
    (
        "TRUSTED_SERVER__PUBLISHER__DOMAIN",
        "ts-operator-sentinel-publisher.example.com",
    ),
    (
        "TRUSTED_SERVER__PUBLISHER__ORIGIN_URL",
        "https://ts-operator-sentinel-origin.example.com",
    ),
    (
        "EDGEZERO__STORES__CONFIG__NAME",
        "ts-operator-sentinel-config-store-0001",
    ),
];

fn fixture_error(detail: impl std::fmt::Display) -> Report<ReleaseError> {
    recipe_error(format!("cli fixture: {detail}"))
}

/// The parsed, unauthenticated ordinary CI CLI fixture.
#[derive(Debug)]
pub(super) struct CliFixture {
    pub(super) builder_image_id: String,
    pub(super) builder_glibc: String,
    pub(super) startup_images: Vec<StartupImage>,
}

impl CliFixture {
    /// Parses `cli-fixture.json`.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Recipe`] for another schema, kind or
    /// acquisition, a missing or extra field, a builder that is not a local
    /// image ID or does not report glibc 2.34, or startup images that are not
    /// exactly the reviewed distributions, in order, at their reviewed
    /// references and selected by local image ID.
    pub(super) fn parse(document: &Value) -> Result<Self, Report<ReleaseError>> {
        let fields: BTreeSet<&str> = document
            .as_object()
            .ok_or_else(|| fixture_error("description is not an object"))?
            .keys()
            .map(String::as_str)
            .collect();
        if fields != BTreeSet::from(FIXTURE_FIELDS) {
            return Err(fixture_error("description fields differ from the schema"));
        }
        if document["schema_version"].as_u64() != Some(SCHEMA_VERSION)
            || document["kind"].as_str() != Some(FIXTURE_KIND)
            || document["acquisition"].as_str() != Some(ACQUISITION)
        {
            return Err(fixture_error("unsupported schema, kind or acquisition"));
        }
        let text = |name: &str| {
            document[name]
                .as_str()
                .ok_or_else(|| fixture_error(format!("{name} is not a string")))
        };
        let builder_image_id = text("builder_image_id")?.to_owned();
        if !is_local_image_id(&builder_image_id) {
            return Err(fixture_error("builder is not a local image ID"));
        }
        let builder_glibc = text("builder_glibc")?.to_owned();
        if !builder_glibc.ends_with(" 2.34") {
            return Err(fixture_error("builder does not report glibc 2.34"));
        }
        let entries = document["startup_images"]
            .as_array()
            .ok_or_else(|| fixture_error("startup_images is not an array"))?;
        if entries.len() != StartupDistribution::ALL.len() {
            return Err(fixture_error("startup images differ from the reviewed set"));
        }
        let startup_images = entries
            .iter()
            .zip(StartupDistribution::ALL)
            .map(|(entry, distribution)| startup_image(entry, distribution))
            .collect::<Result<_, _>>()?;
        Ok(Self {
            builder_image_id,
            builder_glibc,
            startup_images,
        })
    }
}

/// Parses one startup entry, which must be `distribution` at its reference.
fn startup_image(
    entry: &Value,
    distribution: StartupDistribution,
) -> Result<StartupImage, Report<ReleaseError>> {
    let fields: Option<BTreeSet<&str>> = entry
        .as_object()
        .map(|object| object.keys().map(String::as_str).collect());
    if fields != Some(BTreeSet::from(["distribution", "image_id", "reference"])) {
        return Err(fixture_error("startup entry fields differ from the schema"));
    }
    if entry["distribution"].as_str() != Some(distribution.label())
        || entry["reference"].as_str() != Some(distribution.reference())
    {
        return Err(fixture_error(format!(
            "startup entry is not {} at {}",
            distribution.label(),
            distribution.reference()
        )));
    }
    let id = entry["image_id"]
        .as_str()
        .ok_or_else(|| fixture_error("startup image_id is not a string"))?;
    StartupImage::new(distribution, id)
}

/// The ELF facts the measurement records.
fn elf_measurement(facts: &ElfFacts) -> Value {
    let debug: Vec<Value> = facts
        .debug_sections()
        .iter()
        .map(|section| json!({"name": section.name(), "size": section.size()}))
        .collect();
    json!({
        "sha256": facts.sha256(),
        "interpreter": facts.interpreter(),
        "needed": facts.needed(),
        "max_glibc": facts.max_glibc().to_string(),
        "version_requirements": facts.version_requirements().len(),
        "debug_sections": debug,
    })
}

/// The startup runs the measurement records.
fn startup_measurement(runs: &[StartupObserved]) -> Value {
    runs.iter()
        .map(|run| {
            let distribution = run.image.distribution();
            json!({
                "distribution": distribution.label(),
                "reference": distribution.reference(),
                "image_id": run.image.id(),
                "exit_code": 0,
                "stdout": run.stdout,
                "stderr_sha256": run.stderr_sha256,
                "elapsed_seconds": run.elapsed.as_secs_f64(),
            })
        })
        .collect()
}

/// Selects whether a native CLI candidate job measures its reproduction build.
///
/// The ordinary CI workflow always sets it: `measure` on scheduled and
/// dispatched runs (and on a labelled pull request), `defer` otherwise.
pub(super) const NATIVE_REPRODUCTION_VARIABLE: &str = "TS_RELEASE_NATIVE_REPRODUCTION";
/// The GitHub Actions variable naming the event that started the run.
const GITHUB_EVENT_VARIABLE: &str = "GITHUB_EVENT_NAME";
/// Events whose run exists to measure native reproduction, so may not defer it.
const MEASURING_EVENTS: [&str; 2] = ["schedule", "workflow_dispatch"];

/// Whether a native CLI candidate job builds and measures its reproduction.
///
/// The reproduction is a measurement, never a gate, for the native CLI
/// candidates only; the Fastly equality step never reads this selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum NativeReproduction {
    /// Build the reproduction and record how it compares.
    Measure,
    /// Skip the reproduction build and record that a scheduled run measures it.
    Defer,
}

/// Parses the native reproduction selection and the GitHub event name.
///
/// # Errors
///
/// Returns a message for a missing or unknown selection (only the exact
/// lowercase `measure` and `defer` are accepted), or for `defer` on a
/// scheduled or dispatched run, which exists to measure the reproduction.
pub(super) fn parse_native_reproduction(
    value: Option<&str>,
    event: Option<&str>,
) -> Result<NativeReproduction, String> {
    let selected = match value {
        Some("measure") => NativeReproduction::Measure,
        Some("defer") => NativeReproduction::Defer,
        Some(other) => {
            return Err(format!(
                "{NATIVE_REPRODUCTION_VARIABLE} must be `measure` or `defer`, not {other:?}"
            ));
        }
        None => {
            return Err(format!(
                "{NATIVE_REPRODUCTION_VARIABLE} must be set to `measure` or `defer`"
            ));
        }
    };
    if selected == NativeReproduction::Defer
        && let Some(event) = event.filter(|event| MEASURING_EVENTS.contains(event))
    {
        return Err(format!(
            "{NATIVE_REPRODUCTION_VARIABLE}=defer is not allowed on a {event} run, which exists to measure native reproduction"
        ));
    }
    Ok(selected)
}

/// Reads the native reproduction selection from the test environment.
///
/// # Panics
///
/// Panics when [`parse_native_reproduction`] rejects the selection or either
/// variable is not UTF-8, so a misconfigured job fails before any build.
pub(super) fn native_reproduction_from_env() -> NativeReproduction {
    let read = |name: &str| {
        std::env::var_os(name).map(|value| {
            value
                .into_string()
                .unwrap_or_else(|_| panic!("should name {name} in UTF-8"))
        })
    };
    let value = read(NATIVE_REPRODUCTION_VARIABLE);
    let event = read(GITHUB_EVENT_VARIABLE);
    parse_native_reproduction(value.as_deref(), event.as_deref()).unwrap_or_else(|error| {
        panic!("should select the native reproduction mode explicitly: {error}")
    })
}

/// What the optional reproduction build produced.
pub(super) enum ReproductionOutcome<'a> {
    /// Canonical evidence is retained before the optional build starts.
    NotAttempted,
    /// The job selected `defer`; a scheduled or dispatched run measures it.
    Deferred,
    /// A completed pair violated its input invariants.
    InvariantFailed(String),
    /// Both builds completed and were compared.
    Measured(&'a NativeRepeatability),
    /// The reproduction build itself failed; recorded, not a gate.
    BuildFailed(String),
}

/// The reproduction outcome the measurement records.
pub(super) fn reproduction_measurement(outcome: &ReproductionOutcome<'_>) -> Value {
    match outcome {
        ReproductionOutcome::NotAttempted => {
            json!({"status": "not-attempted", "gate": "measurement-only"})
        }
        ReproductionOutcome::Deferred => json!({
            "status": "deferred-to-scheduled-run",
            "gate": "measurement-only",
            "selector": format!("{NATIVE_REPRODUCTION_VARIABLE}=defer"),
            "measured_by": "scheduled/dispatched Run Tests on main",
        }),
        ReproductionOutcome::InvariantFailed(error) => {
            json!({"status": "invariant-failed", "error": error, "gate": "fatal-input-invariant"})
        }

        ReproductionOutcome::Measured(measured) => {
            let difference = measured.difference().map(|difference| {
                json!({
                    "first_offset": difference.first_offset,
                    "differing_bytes": difference.differing_bytes,
                    "canonical_section": difference.canonical_section,
                })
            });
            json!({
                "status": if measured.identical() { "identical" } else { "differing" },
                "canonical": {
                    "sha256": measured.canonical().sha256,
                    "size": measured.canonical().size,
                },
                "reproduction": {
                    "sha256": measured.reproduction().sha256,
                    "size": measured.reproduction().size,
                },
                "difference": difference,
                "gate": "measurement-only",
            })
        }
        ReproductionOutcome::BuildFailed(error) => json!({
            "status": "build-failed",
            "error": error,
            "gate": "measurement-only",
        }),
    }
}

/// Everything the Linux CLI candidate measurement states.
pub(super) struct CliCandidateFacts<'a> {
    pub(super) canonical: &'a RecipeCompleted,
    pub(super) builder: &'a CliFixture,
    pub(super) elf: &'a ElfFacts,
    pub(super) remap_found: &'a [&'static str],
    pub(super) operator_values_removed: &'a [String],
    pub(super) startup: &'a [StartupObserved],
    pub(super) reproduction: ReproductionOutcome<'a>,
}

/// The ordinary CI measurement of the Linux CLI candidate.
pub(super) fn cli_candidate_measurement(facts: &CliCandidateFacts<'_>) -> Value {
    json!({
        "schema_version": SCHEMA_VERSION,
        "kind": "ordinary-ci-linux-cli-candidate",
        "asset": facts.canonical.asset().artifact().name,
        "source_commit": facts.canonical.commit().as_str(),
        "builder": {
            "base": CLI_BUILDER_BASE,
            "image_id": facts.builder.builder_image_id,
            "glibc": facts.builder.builder_glibc,
        },
        "canonical": cargo_measurement(facts.canonical),
        "elf": elf_measurement(facts.elf),
        "remap_scan": {
            "forbidden": ["/boundary/", "host build roots", "fixture roots", "RUNNER_TEMP"],
            "found": facts.remap_found,
        },
        "operator_values": {
            "removed_by_preflight": facts.operator_values_removed,
            "found_in_final_bytes": 0,
        },
        "startup": startup_measurement(facts.startup),
        "reproduction": reproduction_measurement(&facts.reproduction),
        "release_qualification": false,
    })
}

/// Returns the image named by the `FROM` line of `dockerfile`.
fn dockerfile_base(dockerfile: &str) -> Option<&str> {
    let mut bases = dockerfile
        .lines()
        .filter_map(|line| line.strip_prefix("FROM "));
    match (bases.next(), bases.next()) {
        (Some(base), None) => Some(base.trim()),
        _ => None,
    }
}

#[test]
fn builder_dockerfile_uses_the_reviewed_glibc_floor_base() {
    assert_eq!(
        dockerfile_base(CLI_BUILDER_DOCKERFILE),
        Some(CLI_BUILDER_BASE),
        "should build from exactly the reviewed tag-pinned UBI 9 base"
    );
}

#[test]
fn cli_fixture_description_parses_the_reviewed_images() {
    let parsed = CliFixture::parse(&description()).expect("should parse the description");
    assert_eq!(parsed.builder_image_id, image('c'));
    let labels: Vec<&str> = parsed
        .startup_images
        .iter()
        .map(|image| image.distribution().label())
        .collect();
    assert_eq!(labels, ["ubi9", "ubuntu-22.04", "debian-12"]);
}

/// One in-place change to a CLI fixture description.
type Edit = fn(&mut Value);

#[test]
fn cli_fixture_description_rejects_drift_or_unpinned_images() {
    let cases: [(&str, Edit); 8] = [
        ("extra field", |value| value["extra"] = json!(true)),
        ("kind", |value| {
            value["kind"] = json!("release-qualification")
        }),
        ("builder tag", |value| {
            value["builder_image_id"] = json!("example/builder:latest");
        }),
        ("builder glibc", |value| {
            value["builder_glibc"] = json!("ldd (GNU libc) 2.39");
        }),
        ("startup tag", |value| {
            value["startup_images"][1]["image_id"] = json!("ubuntu:22.04");
        }),
        ("reference drift", |value| {
            value["startup_images"][2]["reference"] = json!("docker.io/library/debian:13");
        }),
        ("order", |value| {
            value["startup_images"]
                .as_array_mut()
                .expect("should hold startup images")
                .swap(0, 1);
        }),
        ("missing distribution", |value| {
            value["startup_images"]
                .as_array_mut()
                .expect("should hold startup images")
                .pop();
        }),
    ];
    for (label, edit) in cases {
        let mut value = description();
        edit(&mut value);
        assert!(
            CliFixture::parse(&value).is_err(),
            "should reject a description with a bad {label}"
        );
    }
}

#[test]
fn operator_sentinels_are_removed_by_preflight_and_distinctive() {
    let mut snapshot = vec![(
        std::ffi::OsString::from("PATH"),
        std::ffi::OsString::from("/usr/bin:/bin"),
    )];
    snapshot.extend(
        OPERATOR_SENTINELS
            .iter()
            .map(|(name, value)| ((*name).into(), (*value).into())),
    );
    let audited =
        crate::preflight::audit_environment(&snapshot).expect("should remove, not reject");
    let removed: BTreeSet<&str> = audited.removed_names().iter().map(String::as_str).collect();
    let planted: BTreeSet<&str> = OPERATOR_SENTINELS.iter().map(|(name, _)| *name).collect();
    assert_eq!(
        removed, planted,
        "should remove every planted operator name"
    );
    for (_, value) in OPERATOR_SENTINELS {
        assert!(
            value.contains("example.com") || value.starts_with("ts-operator-sentinel-"),
            "should plant only fictional values"
        );
    }
}

fn image(digit: char) -> String {
    format!("sha256:{}", digit.to_string().repeat(64))
}

fn description() -> Value {
    let startup: Vec<Value> = StartupDistribution::ALL
        .iter()
        .zip(['d', 'e', 'f'])
        .map(|(distribution, digit)| {
            json!({
                "distribution": distribution.label(),
                "reference": distribution.reference(),
                "image_id": image(digit),
            })
        })
        .collect();
    json!({
        "schema_version": 1,
        "kind": "ordinary-ci-cli-fixture",
        "acquisition": "unauthenticated-ordinary-ci",
        "builder_image_id": image('c'),
        "builder_glibc": "ldd (GNU libc) 2.34",
        "startup_images": startup,
    })
}

#[test]
fn optional_reproduction_setup_failure_preserves_canonical_evidence() {
    let bytes = elf(&ElfSpec {
        payload: b"/trusted-server/src/main.rs",
        debug_sections: vec![
            (
                ".debug_info",
                object::elf::SHT_PROGBITS,
                b"fictional dwarf info",
            ),
            (
                ".debug_line",
                object::elf::SHT_PROGBITS,
                b"fictional dwarf lines",
            ),
        ],
        ..ElfSpec::conforming()
    });
    let mut build = built(
        super::super::startup::LINUX_CLI_ASSET,
        Role::Canonical,
        &bytes,
    );
    let stdout = build.stdout(|_| {});
    let canonical = build
        .finish(stdout)
        .expect("should complete the canonical fixture");

    let (measured, invariant) = reproduction_result::<(), ()>(
        Err(recipe_error("reproduction materials unavailable")),
        |_| panic!("should never pair a failed setup"),
    );
    assert!(invariant.is_none(), "should keep setup failure optional");
    assert!(
        measured
            .expect_err("should record unavailable reproduction")
            .contains("materials unavailable")
    );
    let fixture = CliFixture::parse(&description()).expect("should parse the CLI fixture");
    let elf = inspect_elf(&bytes, &ElfPolicy::linux_x86_64_gnu())
        .expect("should inspect canonical bytes");
    let measurement = cli_candidate_measurement(&CliCandidateFacts {
        canonical: &canonical,
        builder: &fixture,
        elf: &elf,
        remap_found: &[],
        operator_values_removed: &[],
        startup: &[],
        reproduction: ReproductionOutcome::BuildFailed("materials unavailable".to_owned()),
    });
    assert_eq!(
        measurement["canonical"]["artifact"]["sha256"],
        canonical.artifact().sha256(),
        "should retain the accepted canonical artifact when reproduction setup fails"
    );
    assert_eq!(
        measurement["reproduction"]["status"], "build-failed",
        "should record unavailable reproduction"
    );
    canonical
        .rehash_for_handoff()
        .expect("should preserve the accepted canonical bytes");
}

#[test]
fn native_reproduction_selection_accepts_only_exact_modes() {
    for (value, event, expected) in [
        ("measure", None, NativeReproduction::Measure),
        ("defer", None, NativeReproduction::Defer),
        ("defer", Some("push"), NativeReproduction::Defer),
        ("defer", Some("pull_request"), NativeReproduction::Defer),
        ("measure", Some("schedule"), NativeReproduction::Measure),
        (
            "measure",
            Some("workflow_dispatch"),
            NativeReproduction::Measure,
        ),
    ] {
        assert_eq!(
            parse_native_reproduction(Some(value), event),
            Ok(expected),
            "should accept {value:?} on event {event:?}"
        );
    }
    for value in [
        None,
        Some(""),
        Some("MEASURE"),
        Some("skip"),
        Some(" defer"),
    ] {
        let error = parse_native_reproduction(value, Some("push"))
            .expect_err("should reject a missing or unknown mode");
        assert!(
            error.contains(NATIVE_REPRODUCTION_VARIABLE),
            "should name the selection variable: {error}"
        );
    }
}

#[test]
fn native_reproduction_cannot_be_deferred_by_a_measuring_run() {
    for event in MEASURING_EVENTS {
        let error = parse_native_reproduction(Some("defer"), Some(event))
            .expect_err("should refuse to defer on a scheduled or dispatched run");
        assert!(
            error.contains(event),
            "should name the measuring event: {error}"
        );
    }
}

#[test]
fn deferred_reproduction_is_recorded_explicitly() {
    let recorded = reproduction_measurement(&ReproductionOutcome::Deferred);
    assert_eq!(
        recorded["status"], "deferred-to-scheduled-run",
        "should state the deferral rather than omit the reproduction"
    );
    assert_eq!(
        recorded["gate"], "measurement-only",
        "should keep the reproduction a measurement"
    );
    assert_eq!(
        recorded["selector"], "TS_RELEASE_NATIVE_REPRODUCTION=defer",
        "should name the selection that deferred it"
    );
}

#[test]
fn optional_reproduction_pairing_failure_remains_fatal() {
    let (measured, invariant) = reproduction_result(Ok(()), |_| {
        Err::<(), _>(recipe_error("different source commits"))
    });
    assert!(measured.is_err(), "should record the failed pairing");
    assert!(invariant.is_some(), "should retain the fatal pairing error");
}

#[test]
fn reproduction_deadline_expires_before_setup() {
    let cancellation = Cancellation::default();
    assert!(
        check_build_budget(Instant::now(), &cancellation).is_err(),
        "should return expiration without starting setup"
    );
}

#[test]
fn cleanup_failure_preserves_both_errors() {
    let result = finish_cleanup::<()>(
        Err(recipe_error("setup failed")),
        Err(recipe_error("source cleanup failed")),
    );
    let error = format!("{:?}", result.expect_err("should report cleanup failure"));
    assert!(
        error.contains("setup failed") && error.contains("source cleanup failed"),
        "should report setup and cleanup errors: {error}"
    );
}

/// Separates optional build availability from fatal comparison invariants.
pub(super) fn reproduction_result<T, U>(
    build: Result<T, Report<ReleaseError>>,
    measure: impl FnOnce(T) -> Result<U, Report<ReleaseError>>,
) -> (Result<U, String>, Option<Report<ReleaseError>>) {
    match build {
        Err(error) => (Err(format!("{error:?}")), None),
        Ok(build) => match measure(build) {
            Ok(measured) => (Ok(measured), None),
            Err(error) => (
                Err(format!("pairing invariant failed: {error:?}")),
                Some(error),
            ),
        },
    }
}

/// Refuses unavailable time or cancellation before optional setup starts.
pub(super) fn check_build_budget(
    deadline: Instant,
    cancellation: &Cancellation,
) -> Result<(), Report<ReleaseError>> {
    if Instant::now() >= deadline || cancellation.is_cancelled() {
        return Err(recipe_error(
            "CLI build deadline expired or was cancelled before setup",
        ));
    }
    Ok(())
}

/// Retains a failed operation and every explicit cleanup failure together.
pub(super) fn finish_cleanup<T>(
    operation: Result<T, Report<ReleaseError>>,
    cleanup: Result<(), Report<ReleaseError>>,
) -> Result<T, Report<ReleaseError>> {
    match (operation, cleanup) {
        (result, Ok(())) => result,
        (Ok(_), Err(cleanup)) => Err(cleanup),
        (Err(error), Err(cleanup)) => Err(error.attach(format!("cleanup failed: {cleanup:?}"))),
    }
}

/// Pins startup host inputs in a separate private root outside the mounted copy.
pub(super) fn new_startup_client(
    docker: &Path,
    copy_directory: &Path,
) -> Result<(tempfile::TempDir, StartupClient), Report<ReleaseError>> {
    let state = tempfile::tempdir()
        .map_err(|error| recipe_error(format!("cannot prepare startup host state: {error}")))?;
    let parent = fs::canonicalize(state.path())
        .map_err(|error| recipe_error(format!("cannot prepare startup host state: {error}")))?;
    let client = StartupClient::new(docker, &parent, &[copy_directory])?;
    Ok((state, client))
}

#[test]
fn startup_host_state_is_disjoint_from_the_mounted_copy() {
    let scratch = tempfile::tempdir().expect("should create startup scratch space");
    let base = fs::canonicalize(scratch.path()).expect("should resolve scratch space");
    let copy_directory = base.join("startup-copy");
    fs::create_dir(&copy_directory).expect("should create a fictional mount");
    let docker = base.join("docker");
    fs::write(&docker, b"fictional Docker client bytes")
        .expect("should write a client without executing it");
    assert!(
        StartupClient::new(&docker, &base, &[&copy_directory]).is_err(),
        "should reject host state above the mounted copy"
    );
    let (state, _client) = new_startup_client(&docker, &copy_directory)
        .expect("should configure the same disjoint host state used by startup");
    let state = fs::canonicalize(state.path()).expect("should resolve startup host state");
    assert!(
        !state.starts_with(&copy_directory) && !copy_directory.starts_with(&state),
        "should keep host state and mount pairwise disjoint"
    );
}
