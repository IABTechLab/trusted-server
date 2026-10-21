//! Consumer checks over the producer's unsigned candidate index.
//!
//! The committed bytes are produced and golden-tested by the release tool
//! (`trusted-server-release` `distribution::tests::golden`) and included here
//! test-only; neither crate depends on the other. Every document join the
//! consumer runs without signatures must accept them, while the size-0
//! awaiting-signature bundle records must keep the candidate unverifiable.
//!
//! The ignored ordinary-CI test runs the same checks over the index the
//! release tool's collector assembled from four real candidate jobs.

use std::collections::BTreeMap;
use std::io::Read as _;
use std::path::Path;

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
/// The fixed name of every candidate index.
const CANDIDATE_INDEX_NAME: &str = "candidate-release-index.json";
/// The fixed name of the shared JS inventory.
const INVENTORY_NAME: &str = "tsjs-inventory.json";
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

/// The byte-limit role the consumer applies to the candidate file `name`.
fn role_for(name: &str) -> Role {
    if name == CANDIDATE_INDEX_NAME {
        Role::Index
    } else if name == INVENTORY_NAME {
        Role::Inventory
    } else if name.ends_with(".spdx.json") {
        Role::Spdx
    } else {
        Role::Companion
    }
}

/// Reads one regular file of at most `limit` bytes.
fn read_bounded(path: &Path, limit: u64) -> Vec<u8> {
    let metadata = std::fs::symlink_metadata(path).expect("should inspect the candidate file");
    assert!(
        metadata.is_file() && metadata.len() <= limit,
        "should read a regular candidate file within its bound: {}",
        path.display()
    );
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .expect("should open the candidate file")
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .expect("should read the candidate file");
    assert!(
        bytes.len() as u64 <= limit,
        "should stay within the role bound: {}",
        path.display()
    );
    bytes
}

/// The eighteen candidate files a consumer reads and their source commit.
struct CandidateFiles {
    commit: String,
    files: BTreeMap<&'static str, Vec<u8>>,
}

impl CandidateFiles {
    /// The committed fictional golden fixture.
    fn committed() -> Self {
        Self {
            commit: COMMIT.to_owned(),
            files: FIXTURES
                .iter()
                .map(|(name, bytes)| (*name, bytes.to_vec()))
                .collect(),
        }
    }

    /// Exactly the eighteen named files of an assembled index directory.
    fn from_directory(root: &Path, commit: String) -> Self {
        let present = std::fs::read_dir(root)
            .expect("should list the assembled candidate index directory")
            .count();
        assert_eq!(
            present,
            FIXTURES.len(),
            "should hold exactly the index, inventory and sixteen companions"
        );
        let files = FIXTURES
            .iter()
            .map(|(name, _)| {
                (
                    *name,
                    read_bounded(&root.join(name), role_for(name).limit()),
                )
            })
            .collect();
        Self { commit, files }
    }

    fn bytes(&self, name: &str) -> &[u8] {
        self.files
            .get(name)
            .map(Vec::as_slice)
            .expect("should hold the named candidate file")
    }

    fn approved_commit(&self) -> ApprovedCommit {
        ApprovedCommit::parse(&self.commit).expect("should accept the candidate commit")
    }

    fn approved(&self) -> ApprovedRelease {
        ApprovedRelease::new(
            ReleaseTag::parse(CANDIDATE_TAG).expect("should accept the sentinel tag pattern"),
            self.approved_commit(),
        )
    }

    fn present_bytes(&self, record: &FileRecord) -> &[u8] {
        let bytes = self.bytes(&record.name);
        assert_eq!(
            (bytes.len() as u64, sha256_hex(bytes)),
            (record.size, record.sha256.clone()),
            "should match the indexed length and digest of {}",
            record.name
        );
        bytes
    }
}

/// Runs every unsigned consumer join over `candidate`.
fn check_unsigned_consumer_joins(candidate: &CandidateFiles) {
    let policy = embedded_policy().expect("should accept the embedded consumer policy");
    let index = parse_release_index(
        candidate.bytes(CANDIDATE_INDEX_NAME),
        &policy,
        &candidate.approved(),
    )
    .expect("should parse the candidate index under the consumer policy");
    let source =
        ApprovedSourcePolicy::for_tests(candidate.approved_commit(), EMBEDDED_POLICY.to_vec());
    assert_eq!(
        accept_producer_policy(&index, &source, &AcceptanceRules::compiled(&policy))
            .expect("should accept the producer policy"),
        AcceptedProducerPolicy::Current,
        "should bind the producer policy to the current consumer policy"
    );
    let inventory = candidate.present_bytes(&index.tsjs_inventory);
    let modules = joins::parse_inventory(inventory, &candidate.commit)
        .expect("should parse the candidate inventory");
    for entry in &index.artifacts {
        let companions = &entry.companions;
        let evidence = candidate.present_bytes(&companions.build_evidence);
        joins::check_build_evidence(evidence, entry, &index, &modules)
            .expect("should join build evidence to the index and inventory");
        let spdx = candidate.present_bytes(&companions.sbom);
        let predicate = parse_json(spdx, Role::Spdx).expect("should parse the candidate SPDX");
        joins::check_spdx(&predicate, spdx, &entry.sha256)
            .expect("should bind the SPDX root to the executable digest");
        let outcome = joins::parse_comparison(candidate.present_bytes(&companions.rebuild_check))
            .expect("should parse the rebuild outcome");
        let expected = if entry.name == "trusted-server-fastly.wasm" {
            ComparisonOutcome::Matched
        } else {
            ComparisonOutcome::Unavailable
        };
        assert_eq!(outcome, expected, "should record {}", entry.name);
        let recipe = joins::parse_informational(candidate.present_bytes(&companions.build_recipe))
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

/// Requires every bundle slot of `candidate` to be the empty-file sentinel
/// that full verification rejects.
fn check_empty_bundle_sentinels(candidate: &CandidateFiles) {
    let policy = embedded_policy().expect("should accept the embedded consumer policy");
    let index = parse_release_index(
        candidate.bytes(CANDIDATE_INDEX_NAME),
        &policy,
        &candidate.approved(),
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

#[test]
fn candidate_index_passes_every_unsigned_consumer_join() {
    check_unsigned_consumer_joins(&CandidateFiles::committed());
}

#[test]
fn candidate_bundles_are_empty_sentinels_that_verification_rejects() {
    check_empty_bundle_sentinels(&CandidateFiles::committed());
}

#[test]
fn directory_reader_reads_exactly_the_eighteen_candidate_files() {
    let scratch = tempfile::tempdir().expect("should create a scratch directory");
    for (name, bytes) in FIXTURES {
        std::fs::write(scratch.path().join(name), bytes).expect("should write the fixture");
    }

    let candidate = CandidateFiles::from_directory(scratch.path(), COMMIT.to_owned());

    assert_eq!(
        candidate.files,
        CandidateFiles::committed().files,
        "should read exactly the committed bytes back"
    );
    check_unsigned_consumer_joins(&candidate);
    check_empty_bundle_sentinels(&candidate);
}

#[test]
#[ignore = "requires the candidate index the ordinary-CI collector assembled from four real candidate jobs"]
fn ordinary_ci_assembled_candidate_passes_unsigned_consumer_joins() {
    let root = std::env::var_os("TS_RELEASE_CANDIDATE_INDEX_ROOT")
        .expect("should explicitly supply the assembled candidate index directory");
    let commit = std::env::var("TS_RELEASE_CANDIDATE_SOURCE_COMMIT")
        .expect("should explicitly supply the candidate source commit");
    let candidate = CandidateFiles::from_directory(Path::new(&root), commit);

    check_unsigned_consumer_joins(&candidate);
    check_empty_bundle_sentinels(&candidate);
}
