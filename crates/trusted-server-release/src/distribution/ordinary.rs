//! Actual ordinary-CI four-job candidate index join, without release authority.
//!
//! The collector downloads the four exact candidate-entry uploads through the
//! bounded real backend, rebuilds each entry from its transferred bytes,
//! derives the policy identity from its own authenticated checkout and
//! assembles the unqualified candidate index. Workflow transfer binding,
//! signing and qualification stay absent.

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::Value;

use super::transfer::{EntryDescriptor, ReceivedEntry, assemble_received, write_candidate_index};
use super::{CANDIDATE_INDEX_NAME, EMPTY_SHA256, sha256_hex};
use crate::collection::transfer::{OrdinaryArtifactSelection, retrieve_ordinary_ci};
use crate::documents::{Role, lower_hex, parse_json};
use crate::policy::CLI_POLICY_PATH;
use crate::preflight::audit_environment;
use crate::process::Cancellation;
use crate::recipe::AssetId;
use crate::source::{CommitId, authenticate};

/// Each producing job's descriptor variable and the asset it must carry.
const DESCRIPTOR_SLOTS: [(&str, &str); 4] = [
    (
        "TS_RELEASE_CANDIDATE_ENTRY_FASTLY",
        "trusted-server-fastly.wasm",
    ),
    (
        "TS_RELEASE_CANDIDATE_ENTRY_LINUX",
        "ts-x86_64-unknown-linux-gnu",
    ),
    (
        "TS_RELEASE_CANDIDATE_ENTRY_MACOS_ARM",
        "ts-aarch64-apple-darwin",
    ),
    (
        "TS_RELEASE_CANDIDATE_ENTRY_MACOS_INTEL",
        "ts-x86_64-apple-darwin",
    ),
];

fn required(name: &str) -> String {
    let value =
        std::env::var(name).expect("should provide the explicitly selected ordinary-CI input");
    assert!(!value.is_empty(), "should provide a non-empty {name}");
    value
}

/// Downloads one exact upload and returns its descriptor and checked members.
fn receive(
    descriptor: EntryDescriptor,
    run_id: u64,
    head: &str,
    token: &str,
    deadline: Instant,
) -> ReceivedEntry {
    let observed = retrieve_ordinary_ci(
        OrdinaryArtifactSelection {
            run_id,
            run_head_commit: head.to_owned(),
            artifact_id: descriptor.artifact_id(),
            artifact_name: descriptor.artifact_name().to_owned(),
            archive_sha256: descriptor.archive_sha256().to_owned(),
        },
        token,
        deadline,
        Cancellation::default(),
    )
    .expect("should retrieve the exact candidate-entry upload through the bounded real backend");
    let metadata = parse_json(observed.metadata(), Role::Metadata)
        .expect("should parse the exact original REST response bytes");
    assert_eq!(
        metadata["id"].as_u64(),
        Some(descriptor.artifact_id()),
        "should corroborate the exact upload ID in raw REST metadata"
    );
    let members = observed
        .members(descriptor.members())
        .expect("should accept the upload under the unchanged flat archive contract");
    ReceivedEntry {
        descriptor,
        members,
    }
}

#[test]
#[ignore = "requires the four ordinary-CI candidate-entry uploads, their exact descriptors, a retained clean clone and a read-only Actions token"]
fn real_four_job_candidate_index() {
    assert_eq!(
        std::env::var("GITHUB_ACTIONS").as_deref(),
        Ok("true"),
        "should run only in the explicit hosted ordinary-CI collector"
    );
    let run_id = required("TS_RELEASE_TRANSFER_RUN_ID")
        .parse::<u64>()
        .expect("should select an exact numeric workflow run ID");
    assert_eq!(
        required("GITHUB_RUN_ID")
            .parse::<u64>()
            .expect("should observe the current workflow run ID"),
        run_id,
        "should select the current ordinary-CI workflow run"
    );
    // REST identifies the PR head; the checkout and every fixture build the
    // synthetic merge commit, which is the candidate source.
    let head = required("TS_RELEASE_TRANSFER_RUN_HEAD_SHA");
    let source_commit = required("TS_RELEASE_CANDIDATE_SOURCE_COMMIT");
    assert!(
        lower_hex(&head, 40) && lower_hex(&source_commit, 40),
        "should provide exact run-head and candidate source identities"
    );
    assert_eq!(
        required("GITHUB_SHA"),
        source_commit,
        "should build the candidate from the checked-out workflow commit"
    );
    let source_root =
        std::fs::canonicalize(PathBuf::from(required("TS_RELEASE_CANDIDATE_SOURCE_ROOT")))
            .expect("should resolve the retained clean clone");
    let commit = CommitId::parse(&source_commit).expect("should parse the candidate commit");
    let audited = audit_environment(&[(OsString::from("PATH"), OsString::from("/usr/bin:/bin"))])
        .expect("should audit the minimal environment");
    let source = authenticate(&source_root, &commit, &audited)
        .expect("should authenticate the retained clean clone at the candidate commit");
    let policy = source
        .read_tracked(CLI_POLICY_PATH, Role::Metadata)
        .expect("should read the tracked consumer policy from the authenticated clone");
    let index_root = PathBuf::from(required("TS_RELEASE_CANDIDATE_INDEX_ROOT"));
    let token = required("GH_TOKEN");
    let descriptors: Vec<EntryDescriptor> = DESCRIPTOR_SLOTS
        .iter()
        .map(|(variable, asset)| {
            let asset = AssetId::parse(asset).expect("should resolve the fixed asset");
            EntryDescriptor::parse(required(variable).as_bytes(), asset, run_id).unwrap_or_else(
                |error| panic!("should accept the {variable} descriptor: {error:?}"),
            )
        })
        .collect();
    super::transfer::check_distinct_uploads(&descriptors.iter().collect::<Vec<_>>())
        .expect("should select four distinct uploads");
    let received: Vec<ReceivedEntry> = descriptors
        .into_iter()
        .map(|descriptor| {
            // Each download is its own bounded transport attempt, which the
            // backend caps at 15 minutes; the largest upload is a ~285 MB CLI.
            let deadline = Instant::now() + Duration::from_secs(10 * 60);
            receive(descriptor, run_id, &head, &token, deadline)
        })
        .collect();
    let index = assemble_received(&received, &source_commit, &policy)
        .unwrap_or_else(|error| panic!("should assemble the four real entries: {error:?}"));
    drop(received);
    write_candidate_index(&index, &index_root).expect("should write the candidate index files");
    let written = std::fs::read(index_root.join(CANDIDATE_INDEX_NAME))
        .expect("should re-read the written candidate index");
    assert_eq!(
        sha256_hex(&written),
        index.sha256(),
        "should write exactly the assembled index bytes"
    );
    let parsed: Value = serde_json::from_slice(&written).expect("should parse the written index");
    for artifact in parsed["artifacts"]
        .as_array()
        .expect("should list artifacts")
    {
        for key in ["provenance", "sbom_attestation"] {
            let record = &artifact["companions"][key];
            assert_eq!(
                (record["size"].as_u64(), record["sha256"].as_str()),
                (Some(0), Some(EMPTY_SHA256)),
                "should keep the awaiting-signature sentinel for {key}"
            );
        }
    }
    println!(
        "{}",
        String::from_utf8(written).expect("should render UTF-8 index bytes")
    );
    println!("{}  {CANDIDATE_INDEX_NAME}", index.sha256());
    println!("unqualified candidate gaps: {}", index.gaps().join(", "));
}
