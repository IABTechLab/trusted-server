use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use super::{Archives, Checked, check};
use crate::ordinary::archive::tests::zip;
use crate::ordinary::{ASSET, run, tests};

const WASM: &[u8] = crate::ordinary::normalization::TEST_WASM;

fn canonical_observation() -> Value {
    with_artifact(tests::fixture())
}

fn reproduction_observation() -> Value {
    with_artifact(tests::reproduction())
}

fn with_artifact(mut value: Value) -> Value {
    value["artifact"] = json!({"size":WASM.len(),"sha256":hex::encode(Sha256::digest(WASM))});
    value
}

fn encoded(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).expect("should encode fixture")
}

struct Fixture {
    request: Value,
    jobs: Value,
    artifacts: Value,
    archives: [Vec<u8>; 4],
}

impl Fixture {
    fn new() -> Self {
        let build = zip(
            &[(ASSET, WASM), (&format!("{ASSET}.spdx.json"), b"{}")],
            false,
        );
        let mut fixture = Self {
            request: run::tests::request(),
            jobs: run::tests::jobs(),
            artifacts: run::tests::artifacts(),
            archives: [
                build.clone(),
                zip(
                    &[(
                        "fastly-observation.json",
                        &encoded(&canonical_observation()),
                    )],
                    false,
                ),
                build,
                zip(
                    &[(
                        "fastly-observation.json",
                        &encoded(&reproduction_observation()),
                    )],
                    false,
                ),
            ],
        };
        fixture.refresh();
        fixture
    }

    fn refresh(&mut self) {
        for (index, bytes) in self.archives.iter().enumerate() {
            let digest = hex::encode(Sha256::digest(bytes));
            let role = if index < 2 {
                "canonical"
            } else {
                "reproduction"
            };
            let key = if index % 2 == 0 {
                "artifact_digest"
            } else {
                "observation_digest"
            };
            self.request[role][key] = json!(digest);
            self.artifacts["artifacts"][index]["digest"] = json!(format!("sha256:{digest}"));
            self.artifacts["artifacts"][index]["size_in_bytes"] = json!(bytes.len());
        }
    }

    fn checked(&self) -> Result<Checked, error_stack::Report<crate::error::ReleaseError>> {
        let bindings = run::bind(
            &encoded(&self.request),
            &encoded(&self.jobs),
            &encoded(&self.artifacts),
        )?;
        check(
            bindings,
            &Archives(self.archives.each_ref().map(Vec::as_slice)),
        )
    }

    fn error(&self) -> String {
        match self.checked() {
            Ok(_) => panic!("should reject the altered transfer fixture"),
            Err(error) => format!("{error:?}"),
        }
    }
}

pub(in crate::ordinary) fn checked_fixture() -> Checked {
    Fixture::new()
        .checked()
        .expect("should construct an unqualified ordinary test comparison")
}

pub(in crate::ordinary) fn command_fixture(
    source: &std::path::Path,
) -> ([Vec<u8>; 3], [Vec<u8>; 4]) {
    let mut fixture = Fixture::new();
    let identity = crate::ordinary::inputs::source_identity(source, &"a".repeat(40))
        .expect("should derive fixture recipe from independent comparison source");
    for (index, mut observed) in [
        (1, canonical_observation()),
        (3, reproduction_observation()),
    ] {
        observed["recipe_sha256"] = json!(identity.recipe_sha256);
        observed["cargo_lock_sha256"] = json!(identity.cargo_lock_sha256);
        observed["npm_lock_sha256"] = json!(identity.npm_lock_sha256);
        observed["tools"]["rustc"]["version"] = json!("rustc 1.98.1 (abcdef123 2026-01-01)");
        fixture.archives[index] = zip(&[("fastly-observation.json", &encoded(&observed))], false);
    }
    fixture.refresh();
    (
        [
            encoded(&fixture.request),
            encoded(&fixture.jobs),
            encoded(&fixture.artifacts),
        ],
        fixture.archives,
    )
}

#[test]
fn joins_exact_archives_and_emits_only_an_ordinary_matched_receipt() {
    let receipt = Fixture::new()
        .checked()
        .expect("should join and compare exact retained bytes")
        .receipt();
    assert_eq!(
        receipt["outcome"], "matched",
        "should record the actual comparison"
    );
    assert!(
        receipt.get("rustflags_sha256").is_none(),
        "should keep the physical argument digest private and preserve the public v1 fields"
    );
    assert_eq!(
        receipt["release_qualification"], false,
        "should never imply controlled qualification"
    );
    assert_eq!(
        receipt["canonical"]["job_id"], 100,
        "should retain the authenticated-context join"
    );
    assert_eq!(
        receipt["canonical"]["artifact"]["id"], 200,
        "should retain the direct upload identity"
    );
    assert_eq!(
        receipt["reproduction"]["observation_artifact"]["id"], 203,
        "should retain all four transfer identities"
    );
    assert_eq!(
        receipt["canonical_sha256"],
        hex::encode(Sha256::digest(WASM)),
        "should bind the complete actual binary digest"
    );
}

#[test]
fn rejects_changed_whole_archives_before_zip_parsing() {
    for index in 0..4 {
        let mut fixture = Fixture::new();
        fixture.archives[index][0] ^= 1;
        assert!(
            fixture.error().contains("archive digest or size"),
            "should check service/upload bytes before parsing ZIP {index}"
        );
    }
}

#[test]
fn rejects_archive_length_disagreeing_with_service_metadata() {
    let mut fixture = Fixture::new();
    fixture.artifacts["artifacts"][0]["size_in_bytes"] = json!(1);
    assert!(
        fixture.error().contains("archive digest or size"),
        "should bind compressed archive length as well as digest"
    );
}

#[test]
fn rejects_role_swaps_even_with_valid_archive_digests() {
    let mut fixture = Fixture::new();
    for job in fixture.jobs["jobs"]
        .as_array_mut()
        .expect("should list jobs")
    {
        job["started_at"] = json!("2023-11-14T22:13:00Z");
        job["completed_at"] = json!("2023-11-14T22:29:00Z");
    }
    fixture.archives.swap(1, 3);
    fixture.refresh();
    assert!(
        fixture.error().contains("canonical then reproduction"),
        "should validate retained observation roles after verifying each archive"
    );
}

#[test]
fn rejects_observed_source_disagreeing_with_current_run() {
    let mut fixture = Fixture::new();
    for (index, mut observation) in [
        (1, canonical_observation()),
        (3, reproduction_observation()),
    ] {
        observation["source_commit"] = json!("f".repeat(40));
        fixture.archives[index] = zip(
            &[("fastly-observation.json", &encoded(&observation))],
            false,
        );
    }
    fixture.refresh();
    assert!(
        fixture.error().contains("observation source or start"),
        "should reject equally forged source observations against current-run identity"
    );
}

#[test]
fn rejects_observed_start_outside_its_producing_job() {
    let mut fixture = Fixture::new();
    let mut observation = reproduction_observation();
    observation["environment"]["started_at_seconds"] = json!(1_700_000_000);
    fixture.archives[3] = zip(
        &[("fastly-observation.json", &encoded(&observation))],
        false,
    );
    fixture.refresh();
    assert!(
        fixture.error().contains("observation source or start"),
        "should join observed time to the current producing job window"
    );
}

#[test]
fn rejects_binary_mutation_after_observation() {
    let mut fixture = Fixture::new();
    fixture.archives[2] = zip(
        &[
            (ASSET, b"different"),
            (&format!("{ASSET}.spdx.json"), b"{}"),
        ],
        false,
    );
    fixture.refresh();
    assert!(
        fixture
            .error()
            .contains("executable changed after observation"),
        "should check final bytes against the transferred measurement"
    );
}

#[test]
fn rejects_real_mismatch_even_when_both_observations_are_accurate() {
    let mut fixture = Fixture::new();
    let bytes = [WASM, b"\0\x02\x01x"].concat();
    fixture.archives[2] = zip(
        &[(ASSET, &bytes), (&format!("{ASSET}.spdx.json"), b"{}")],
        false,
    );
    let mut observation = reproduction_observation();
    observation["artifact"] =
        json!({"size":bytes.len(), "sha256":hex::encode(Sha256::digest(&bytes))});
    fixture.archives[3] = zip(
        &[("fastly-observation.json", &encoded(&observation))],
        false,
    );
    fixture.refresh();
    assert!(
        fixture.error().contains("final-byte mismatch"),
        "should reject actual nondeterminism without exclusions"
    );
}
