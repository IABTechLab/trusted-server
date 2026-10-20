//! Golden byte equality between the producer's candidate rendering and the
//! committed fixtures the CLI consumer verifies.
//!
//! The committed files under `tests/fixtures/index/candidate/` are fictional
//! except for `policy_sha256`, which is the digest of the actual tracked
//! `crates/trusted-server-cli/release-policy.json`, so the consumer accepts
//! the producer policy as current. A policy change therefore requires
//! regenerating these fixtures in the same change.

use super::*;

/// Every committed candidate fixture file and its exact bytes.
const FIXTURES: [(&str, &[u8]); 18] = [
    (
        "candidate-release-index.json",
        include_bytes!("../../../tests/fixtures/index/candidate/candidate-release-index.json"),
    ),
    (
        "tsjs-inventory.json",
        include_bytes!("../../../tests/fixtures/index/candidate/tsjs-inventory.json"),
    ),
    (
        "trusted-server-fastly.wasm.spdx.json",
        include_bytes!(
            "../../../tests/fixtures/index/candidate/trusted-server-fastly.wasm.spdx.json"
        ),
    ),
    (
        "trusted-server-fastly.wasm.build-evidence.json",
        include_bytes!(
            "../../../tests/fixtures/index/candidate/trusted-server-fastly.wasm.build-evidence.json"
        ),
    ),
    (
        "trusted-server-fastly.wasm.build-recipe.json",
        include_bytes!(
            "../../../tests/fixtures/index/candidate/trusted-server-fastly.wasm.build-recipe.json"
        ),
    ),
    (
        "trusted-server-fastly.wasm.rebuild-check.json",
        include_bytes!(
            "../../../tests/fixtures/index/candidate/trusted-server-fastly.wasm.rebuild-check.json"
        ),
    ),
    (
        "ts-aarch64-apple-darwin.spdx.json",
        include_bytes!("../../../tests/fixtures/index/candidate/ts-aarch64-apple-darwin.spdx.json"),
    ),
    (
        "ts-aarch64-apple-darwin.build-evidence.json",
        include_bytes!(
            "../../../tests/fixtures/index/candidate/ts-aarch64-apple-darwin.build-evidence.json"
        ),
    ),
    (
        "ts-aarch64-apple-darwin.build-recipe.json",
        include_bytes!(
            "../../../tests/fixtures/index/candidate/ts-aarch64-apple-darwin.build-recipe.json"
        ),
    ),
    (
        "ts-aarch64-apple-darwin.rebuild-check.json",
        include_bytes!(
            "../../../tests/fixtures/index/candidate/ts-aarch64-apple-darwin.rebuild-check.json"
        ),
    ),
    (
        "ts-x86_64-apple-darwin.spdx.json",
        include_bytes!("../../../tests/fixtures/index/candidate/ts-x86_64-apple-darwin.spdx.json"),
    ),
    (
        "ts-x86_64-apple-darwin.build-evidence.json",
        include_bytes!(
            "../../../tests/fixtures/index/candidate/ts-x86_64-apple-darwin.build-evidence.json"
        ),
    ),
    (
        "ts-x86_64-apple-darwin.build-recipe.json",
        include_bytes!(
            "../../../tests/fixtures/index/candidate/ts-x86_64-apple-darwin.build-recipe.json"
        ),
    ),
    (
        "ts-x86_64-apple-darwin.rebuild-check.json",
        include_bytes!(
            "../../../tests/fixtures/index/candidate/ts-x86_64-apple-darwin.rebuild-check.json"
        ),
    ),
    (
        "ts-x86_64-unknown-linux-gnu.spdx.json",
        include_bytes!(
            "../../../tests/fixtures/index/candidate/ts-x86_64-unknown-linux-gnu.spdx.json"
        ),
    ),
    (
        "ts-x86_64-unknown-linux-gnu.build-evidence.json",
        include_bytes!(
            "../../../tests/fixtures/index/candidate/ts-x86_64-unknown-linux-gnu.build-evidence.json"
        ),
    ),
    (
        "ts-x86_64-unknown-linux-gnu.build-recipe.json",
        include_bytes!(
            "../../../tests/fixtures/index/candidate/ts-x86_64-unknown-linux-gnu.build-recipe.json"
        ),
    ),
    (
        "ts-x86_64-unknown-linux-gnu.rebuild-check.json",
        include_bytes!(
            "../../../tests/fixtures/index/candidate/ts-x86_64-unknown-linux-gnu.rebuild-check.json"
        ),
    ),
];

fn fixture(name: &str) -> &'static [u8] {
    FIXTURES
        .iter()
        .find(|(fixture, _)| *fixture == name)
        .map(|(_, bytes)| *bytes)
        .expect("should commit the named fixture")
}

#[test]
fn producer_rendering_equals_the_committed_candidate_fixtures() {
    let index = assembled();
    assert_eq!(
        index.bytes(),
        fixture(CANDIDATE_INDEX_NAME),
        "should render the committed candidate index bytes"
    );
    assert_eq!(
        index.inventory(),
        fixture(INVENTORY_NAME),
        "should name the committed inventory bytes"
    );
    assert_eq!(
        index.companions().len() + 2,
        FIXTURES.len(),
        "should commit exactly the index, inventory and present companions"
    );
    for (name, bytes) in index.companions() {
        assert_eq!(
            bytes.as_slice(),
            fixture(name),
            "should render the committed companion {name}"
        );
    }
}

#[test]
fn committed_fixture_names_the_actual_tracked_consumer_policy() {
    let index: Value = serde_json::from_slice(fixture(CANDIDATE_INDEX_NAME))
        .expect("should parse the committed index");
    let policy = parse_json(POLICY, DocumentRole::Metadata).expect("should parse the policy");
    assert_eq!(
        index["policy_sha256"],
        sha256_hex(POLICY),
        "should bind the exact tracked policy bytes"
    );
    assert_eq!(
        index["policy_version"], policy["policy_version"],
        "should name the tracked policy version"
    );
    assert_eq!(
        policy["policy_version"], CLI_POLICY_VERSION,
        "should match the compiled policy version"
    );
}
