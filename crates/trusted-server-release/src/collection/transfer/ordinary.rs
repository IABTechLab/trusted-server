//! Actual ordinary-CI upload/archive interoperability, without release authority.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use super::{OrdinaryArtifactSelection, retrieve_ordinary_ci};
use crate::documents::{Role, lower_hex, parse_json};
use crate::process::Cancellation;

const MEMBERS: [(&str, &[u8]); 4] = [
    ("artifact", b"fictional transfer executable bytes\n"),
    (
        "build-record.json",
        b"{\"kind\":\"fictional-build-record\",\"release_qualification\":false}\n",
    ),
    (
        "composition.json",
        b"{\"kind\":\"fictional-composition\",\"complete\":false}\n",
    ),
    (
        "tsjs-inventory.json",
        b"{\"kind\":\"fictional-javascript-inventory\",\"modules\":[]}\n",
    ),
];

fn required(name: &str) -> String {
    std::env::var(name).expect("should provide the explicitly selected ordinary-CI fixture input")
}

#[test]
#[ignore = "requires the explicit ordinary-CI four-file upload, exact upload outputs and read-only Actions token"]
fn real_uploaded_archive() {
    assert_eq!(
        std::env::var("GITHUB_ACTIONS").as_deref(),
        Ok("true"),
        "should run only in the explicit hosted ordinary-CI fixture"
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
    let head = required("TS_RELEASE_TRANSFER_RUN_HEAD_SHA");
    let artifact_id = required("TS_RELEASE_TRANSFER_ARTIFACT_ID")
        .parse::<u64>()
        .expect("should select the exact numeric upload artifact ID");
    let name = required("TS_RELEASE_TRANSFER_ARTIFACT_NAME");
    let digest = required("TS_RELEASE_TRANSFER_ARCHIVE_SHA256");
    let digest = digest.strip_prefix("sha256:").unwrap_or(&digest);
    assert!(
        lower_hex(&head, 40) && lower_hex(digest, 64),
        "should provide exact run-head and original archive identities"
    );
    let token = required("GH_TOKEN");
    let expected: BTreeMap<String, u64> = MEMBERS
        .iter()
        .map(|(name, bytes)| ((*name).to_owned(), bytes.len() as u64))
        .collect();
    let deadline = Instant::now() + Duration::from_secs(5 * 60);
    for _ in 0..2 {
        // Each call creates an independent transport and downloads the original
        // selected ZIP again; no action-extracted directory substitutes for it.
        let observed = retrieve_ordinary_ci(
            OrdinaryArtifactSelection {
                run_id,
                run_head_commit: head.clone(),
                artifact_id,
                artifact_name: name.clone(),
                archive_sha256: digest.to_owned(),
            },
            &token,
            deadline,
            Cancellation::default(),
        )
        .expect("should retrieve the exact ordinary-CI upload through the bounded real backend");
        let metadata = parse_json(observed.metadata(), Role::Metadata)
            .expect("should parse the exact original REST response bytes");
        assert_eq!(
            metadata["id"].as_u64(),
            Some(artifact_id),
            "should corroborate the exact upload ID in raw REST metadata"
        );
        let members = observed.members(&expected).expect(
            "should accept the original upload ZIP under the unchanged flat archive contract",
        );
        assert_eq!(
            members.len(),
            MEMBERS.len(),
            "should retain exactly four fictional flat members"
        );
        for (name, bytes) in MEMBERS {
            assert_eq!(
                members.get(name).map(Vec::as_slice),
                Some(bytes),
                "should retain the exact fictional bytes for {name}"
            );
        }
    }
}
