//! Consumer checks over the producer's committed unsigned candidate index.
//!
//! The bytes are produced and golden-tested by the release tool
//! (`trusted-server-release` `distribution::tests::golden`) and included here
//! test-only; neither crate depends on the other. Every document join the
//! consumer runs without signatures must accept them, while the size-0
//! awaiting-signature bundle records must keep the candidate unverifiable.

use serde_json::Value;

use super::{check_bundle_records, joins};
use crate::release::documents::{
    ApprovedCommit, ApprovedRelease, FileRecord, ReleaseTag, Role, parse_json, parse_release_index,
    sha256_hex,
};
use crate::release::error::ReleaseError;
use crate::release::evidence::ComparisonOutcome;
use crate::release::policy::{
    AcceptanceRules, AcceptedProducerPolicy, EMBEDDED_POLICY, accept_producer_policy,
    embedded_policy,
};
use crate::release::source_policy::ApprovedSourcePolicy;

/// The fixed sentinel tag of every candidate index.
const CANDIDATE_TAG: &str = "v0.0.0-unqualified-candidate";
/// The fictional source commit of the committed candidate fixture.
const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

macro_rules! candidate_fixture {
    ($name:literal) => {
        (
            $name,
            include_bytes!(concat!(
                "../../../../trusted-server-release/tests/fixtures/index/candidate/",
                $name
            ))
            .as_slice(),
        )
    };
}

/// Every committed candidate file the consumer reads.
const FIXTURES: [(&str, &[u8]); 18] = [
    candidate_fixture!("candidate-release-index.json"),
    candidate_fixture!("tsjs-inventory.json"),
    candidate_fixture!("trusted-server-fastly.wasm.spdx.json"),
    candidate_fixture!("trusted-server-fastly.wasm.build-evidence.json"),
    candidate_fixture!("trusted-server-fastly.wasm.build-recipe.json"),
    candidate_fixture!("trusted-server-fastly.wasm.rebuild-check.json"),
    candidate_fixture!("ts-aarch64-apple-darwin.spdx.json"),
    candidate_fixture!("ts-aarch64-apple-darwin.build-evidence.json"),
    candidate_fixture!("ts-aarch64-apple-darwin.build-recipe.json"),
    candidate_fixture!("ts-aarch64-apple-darwin.rebuild-check.json"),
    candidate_fixture!("ts-x86_64-apple-darwin.spdx.json"),
    candidate_fixture!("ts-x86_64-apple-darwin.build-evidence.json"),
    candidate_fixture!("ts-x86_64-apple-darwin.build-recipe.json"),
    candidate_fixture!("ts-x86_64-apple-darwin.rebuild-check.json"),
    candidate_fixture!("ts-x86_64-unknown-linux-gnu.spdx.json"),
    candidate_fixture!("ts-x86_64-unknown-linux-gnu.build-evidence.json"),
    candidate_fixture!("ts-x86_64-unknown-linux-gnu.build-recipe.json"),
    candidate_fixture!("ts-x86_64-unknown-linux-gnu.rebuild-check.json"),
];

fn fixture(name: &str) -> &'static [u8] {
    FIXTURES
        .iter()
        .find(|(fixture, _)| *fixture == name)
        .map(|(_, bytes)| *bytes)
        .expect("should include the named candidate fixture")
}

fn approved() -> ApprovedRelease {
    ApprovedRelease::new(
        ReleaseTag::parse(CANDIDATE_TAG).expect("should accept the sentinel tag pattern"),
        ApprovedCommit::parse(COMMIT).expect("should accept the fixture commit"),
    )
}

fn present_bytes(record: &FileRecord) -> &'static [u8] {
    let bytes = fixture(&record.name);
    assert_eq!(
        (bytes.len() as u64, sha256_hex(bytes)),
        (record.size, record.sha256.clone()),
        "should match the indexed length and digest of {}",
        record.name
    );
    bytes
}

#[test]
fn candidate_index_passes_every_unsigned_consumer_join() {
    let policy = embedded_policy().expect("should accept the embedded consumer policy");
    let index = parse_release_index(
        fixture("candidate-release-index.json"),
        &policy,
        &approved(),
    )
    .expect("should parse the candidate index under the consumer policy");
    let source = ApprovedSourcePolicy::for_tests(
        ApprovedCommit::parse(COMMIT).expect("should accept the fixture commit"),
        EMBEDDED_POLICY.to_vec(),
    );
    assert_eq!(
        accept_producer_policy(&index, &source, &AcceptanceRules::compiled(&policy))
            .expect("should accept the producer policy"),
        AcceptedProducerPolicy::Current,
        "should bind the producer policy to the current consumer policy"
    );
    let inventory = present_bytes(&index.tsjs_inventory);
    let modules =
        joins::parse_inventory(inventory, COMMIT).expect("should parse the candidate inventory");
    for entry in &index.artifacts {
        let companions = &entry.companions;
        let evidence = present_bytes(&companions.build_evidence);
        joins::check_build_evidence(evidence, entry, &index, &modules)
            .expect("should join build evidence to the index and inventory");
        let spdx = present_bytes(&companions.sbom);
        let predicate = parse_json(spdx, Role::Spdx).expect("should parse the candidate SPDX");
        joins::check_spdx(&predicate, spdx, &entry.sha256)
            .expect("should bind the SPDX root to the executable digest");
        let outcome = joins::parse_comparison(present_bytes(&companions.rebuild_check))
            .expect("should parse the rebuild outcome");
        let expected = if entry.name == "trusted-server-fastly.wasm" {
            ComparisonOutcome::Matched
        } else {
            ComparisonOutcome::Unavailable
        };
        assert_eq!(outcome, expected, "should record {}", entry.name);
        let recipe = joins::parse_informational(present_bytes(&companions.build_recipe))
            .expect("should parse the informational recipe");
        for document in [
            recipe,
            parse_json(evidence, Role::Companion).expect("should parse evidence"),
        ] {
            assert_eq!(
                document["release_qualification"],
                Value::Bool(false),
                "should state the candidate is unqualified"
            );
        }
    }
}

#[test]
fn candidate_bundles_are_empty_sentinels_that_verification_rejects() {
    let policy = embedded_policy().expect("should accept the embedded consumer policy");
    let index = parse_release_index(
        fixture("candidate-release-index.json"),
        &policy,
        &approved(),
    )
    .expect("should parse the candidate index");
    let empty = sha256_hex(b"");
    for entry in &index.artifacts {
        for record in [
            &entry.companions.provenance,
            &entry.companions.sbom_attestation,
        ] {
            assert_eq!(
                (record.size, record.sha256.as_str()),
                (0, empty.as_str()),
                "should record the true empty file for {}",
                record.name
            );
        }
        let error = check_bundle_records(entry).expect_err("should reject empty bundles");
        assert_eq!(
            *error.current_context(),
            ReleaseError::Evidence,
            "should fail closed as an evidence error"
        );
        assert!(
            format!("{error:?}").contains("is empty"),
            "should name the empty bundle: {error:?}"
        );
    }
}
