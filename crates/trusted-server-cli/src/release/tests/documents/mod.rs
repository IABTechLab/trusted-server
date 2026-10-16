//! Strict document, embedded policy, producer-policy identity and approved
//! source policy acquisition tests.

use std::fs;
use std::io::{Read as _, Write as _};
use std::net::TcpListener;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::time::{Duration, Instant};

use error_stack::Report;
use reqwest::StatusCode;
use reqwest::header::{ACCEPT, AUTHORIZATION};
use serde_json::{Value, json};
use url::Url;

use crate::release::bounds::{self, AttemptDeadline, ProcessBounds};
use crate::release::documents::{
    ApprovedCommit, ApprovedRelease, ReleaseIndex, ReleaseTag, Role, parse_json,
    parse_release_index, schema_accepts, sha256_hex, validate_index_schema,
};
use crate::release::environment::{CallerEnvironment, GithubToken};
use crate::release::error::ReleaseError;
use crate::release::policy::{
    AcceptanceRules, AcceptedProducerPolicy, ConsumerPolicy, EMBEDDED_POLICY, GITHUB_HOST,
    HISTORICAL_PRODUCER_POLICIES, HistoricalPolicy, REPOSITORY, SIGNER_WORKFLOW_PREFIX,
    accept_producer_policy, embedded_policy, parse_consumer_policy,
};
use crate::release::source_policy::{
    ApprovedSourcePolicy, ApprovedSourceRoot, BoundedBody, POLICY_CONTENTS_URL,
    check_policy_response, fetch_approved_policy, plain_http_policy_client_for_tests,
    policy_client, policy_request, read_approved_snapshot,
};

mod producer;

const INDEX_FIXTURE: &[u8] =
    include_bytes!("../../../../tests/fixtures/release/release-index.json");
const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
const OTHER_COMMIT: &str = "89abcdef0123456789abcdef0123456789abcdef";
const TOKEN: &str = "fictional-read-only-token-0000";

fn context<T: core::fmt::Debug>(result: Result<T, Report<ReleaseError>>) -> ReleaseError {
    *result.expect_err("should reject input").current_context()
}

fn policy() -> ConsumerPolicy {
    embedded_policy().expect("should accept the embedded consumer policy")
}

fn commit() -> ApprovedCommit {
    ApprovedCommit::parse(COMMIT).expect("should accept fictional approved commit")
}

fn approved_release() -> ApprovedRelease {
    ApprovedRelease::new(
        ReleaseTag::parse("v1.2.0").expect("should accept fictional tag"),
        commit(),
    )
}

fn index_value() -> Value {
    let mut value = parse_json(INDEX_FIXTURE, Role::Index).expect("should parse index fixture");
    value["policy_sha256"] = json!(sha256_hex(EMBEDDED_POLICY));
    value
}

fn encode(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).expect("should encode JSON fixture")
}

fn parse_index(value: &Value) -> Result<ReleaseIndex, Report<ReleaseError>> {
    parse_release_index(&encode(value), &policy(), &approved_release())
}

fn embedded_policy_value() -> Value {
    parse_json(EMBEDDED_POLICY, Role::Policy).expect("should parse embedded policy")
}

fn source_policy(bytes: &[u8]) -> ApprovedSourcePolicy {
    ApprovedSourcePolicy::for_tests(commit(), bytes.to_vec())
}

fn token() -> GithubToken {
    GithubToken::select(&CallerEnvironment::from_pairs([("GH_TOKEN", TOKEN)]))
        .expect("should select explicit token")
}

// Strict JSON.

#[test]
fn strict_json_rejects_duplicate_keys_at_every_depth() {
    for text in [
        r#"{"a":1,"a":2}"#,
        r#"{"outer":{"inner":[{"k":1,"k":1}]}}"#,
        r#"{"a":1,"a":2}"#,
    ] {
        assert_eq!(
            context(parse_json(text.as_bytes(), Role::Index)),
            ReleaseError::Document,
            "should reject duplicate key in {text}"
        );
    }
}

#[test]
fn strict_json_rejects_depth_beyond_sixty_four_including_root() {
    let accepted = format!("{}{}", "[".repeat(64), "]".repeat(64));
    let rejected = format!("{}{}", "[".repeat(65), "]".repeat(65));

    assert!(
        parse_json(accepted.as_bytes(), Role::Index).is_ok(),
        "should accept depth 64"
    );
    assert_eq!(
        context(parse_json(rejected.as_bytes(), Role::Index)),
        ReleaseError::Document,
        "should reject depth 65"
    );
}

#[test]
fn strict_json_rejects_oversize_before_parsing() {
    let mut oversized = vec![b' '; usize::try_from(bounds::INDEX_BYTES).expect("should fit")];
    oversized.extend_from_slice(b"{}");

    assert_eq!(
        context(parse_json(&oversized, Role::Index)),
        ReleaseError::Document,
        "should reject index over 1 MiB"
    );
    assert!(
        parse_json(&oversized, Role::Bundle).is_ok(),
        "should accept the same bytes under the larger bundle bound"
    );
}

#[test]
fn strict_json_role_limits_match_bounds() {
    assert_eq!(Role::Index.limit(), bounds::INDEX_BYTES, "index bound");
    assert_eq!(Role::Policy.limit(), bounds::POLICY_BYTES, "policy bound");
    assert_eq!(
        Role::Inventory.limit(),
        bounds::INVENTORY_BYTES,
        "inventory bound"
    );
    assert_eq!(Role::Bundle.limit(), bounds::BUNDLE_BYTES, "bundle bound");
    assert_eq!(
        Role::Companion.limit(),
        bounds::COMPANION_BYTES,
        "companion bound"
    );
    assert_eq!(Role::Spdx.limit(), bounds::SPDX_BYTES, "SPDX bound");
    assert_eq!(
        Role::GhOutput.limit(),
        bounds::STDOUT_BYTES,
        "gh stdout bound"
    );
}

#[test]
fn strict_json_rejects_bom_invalid_utf8_and_trailing_data() {
    for bytes in [
        b"\xef\xbb\xbf{}".as_slice(),
        b"{\"a\":\"\xff\"}".as_slice(),
        b"{} {}".as_slice(),
        b"{}x".as_slice(),
        br#"{"a":"\ud800"}"#.as_slice(),
    ] {
        assert_eq!(
            context(parse_json(bytes, Role::Index)),
            ReleaseError::Document,
            "should reject {bytes:?}"
        );
    }
}

#[test]
fn strict_json_rejects_integer_overflow_lexically() {
    for text in [
        "18446744073709551616",
        "-9223372036854775809",
        r#"{"deep":[[{"n":18446744073709551616}]]}"#,
        "1e999",
        "-1e999",
    ] {
        assert_eq!(
            context(parse_json(text.as_bytes(), Role::Index)),
            ReleaseError::Document,
            "should reject out-of-range number {text}"
        );
    }
    for text in [
        "18446744073709551615",
        "-9223372036854775808",
        r#"{"text":"18446744073709551616 1e999 \" \\"}"#,
    ] {
        assert!(
            parse_json(text.as_bytes(), Role::Index).is_ok(),
            "should accept boundary or quoted text {text}"
        );
    }
}

#[test]
fn sha256_matches_known_vector() {
    assert_eq!(
        sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        "should compute SHA-256 of exact bytes"
    );
}

// Embedded consumer policy.

#[test]
fn embedded_policy_identity_is_external_exact_byte_digest() {
    let policy = policy();

    assert_eq!(policy.version(), 1, "should be consumer policy v1");
    assert_eq!(
        policy.sha256(),
        sha256_hex(EMBEDDED_POLICY),
        "should hash the exact embedded bytes"
    );
    assert!(
        !String::from_utf8_lossy(EMBEDDED_POLICY).contains(policy.sha256()),
        "should not contain its own digest"
    );
    assert!(
        HISTORICAL_PRODUCER_POLICIES.is_empty(),
        "should start with an empty reviewed historical allowlist"
    );
}

fn reject_policy(mutate: impl FnOnce(&mut Value), reason: &str) {
    let mut value = embedded_policy_value();
    mutate(&mut value);

    assert_eq!(
        context(parse_consumer_policy(&encode(&value))),
        ReleaseError::Policy,
        "should reject policy: {reason}"
    );
}

#[test]
fn policy_rejects_unknown_kind() {
    reject_policy(
        |value| value["artifacts"][0]["kind"] = json!("archive"),
        "unknown kind",
    );
}

#[test]
fn policy_rejects_kind_swapped_between_artifacts() {
    reject_policy(
        |value| value["artifacts"][1]["kind"] = json!("wasm-core"),
        "native CLI declared as wasm-core",
    );
}

#[test]
fn policy_rejects_unknown_profile() {
    reject_policy(
        |value| value["artifacts"][1]["assurance_profile"] = json!("provenance-only"),
        "unknown profile",
    );
}

#[test]
fn policy_rejects_profile_downgrade() {
    reject_policy(
        |value| value["artifacts"][0]["assurance_profile"] = json!("provenance-sbom"),
        "server without repeatable rebuild",
    );
}

#[test]
fn policy_rejects_unknown_action() {
    reject_policy(
        |value| value["artifacts"][0]["actions"] = json!(["readonly-verify", "deploy"]),
        "unknown action",
    );
    reject_policy(
        |value| value["public_actions"] = json!(["deploy"]),
        "unknown public action",
    );
}

#[test]
fn policy_rejects_fastly_staging_capability() {
    reject_policy(
        |value| {
            value["artifacts"][0]["capabilities"] = json!(["fastly-production", "fastly-staging"]);
        },
        "structurally possible fastly-staging",
    );
}

#[test]
fn policy_rejects_unknown_capability_and_fields() {
    reject_policy(
        |value| value["artifacts"][1]["capabilities"] = json!(["cli-admin"]),
        "unknown capability",
    );
    reject_policy(
        |value| value["artifacts"][1]["extra"] = json!(true),
        "unknown artifact field",
    );
    reject_policy(
        |value| value["extra"] = json!(true),
        "unknown top-level field",
    );
}

#[test]
fn policy_rejects_changed_identity_and_bounds() {
    reject_policy(|value| value["repository_id"] = json!("1"), "repository id");
    reject_policy(
        |value| value["identity"]["issuer"] = json!("https://issuer.example.com"),
        "issuer",
    );
    reject_policy(
        |value| value["bounds"]["child_seconds"] = json!(121),
        "child bound",
    );
    reject_policy(
        |value| value["github_host"] = json!("ghe.example.com"),
        "github host",
    );
}

#[test]
fn policy_rejects_unsupported_tool_version() {
    reject_policy(
        |value| value["tool_versions"]["gh"] = json!("2.98.0"),
        "gh version",
    );
    reject_policy(
        |value| value["tool_versions"]["actions_attest_revision"] = json!("0".repeat(40)),
        "attest revision",
    );
}

#[test]
fn policy_rejects_nonempty_json_historical_list_not_compiled() {
    reject_policy(
        |value| {
            value["historical_producer_policies"] =
                json!([{"policy_version": 1, "policy_sha256": "d".repeat(64)}]);
        },
        "historical list that is not the compiled allowlist",
    );
}

// Governing JSON Schema.

#[test]
fn governing_schema_accepts_fixture() {
    validate_index_schema(&index_value()).expect("should accept the closed fixture");
}

/// A named mutation of the index fixture.
type Mutation = (&'static str, fn(&mut Value));

#[test]
fn governing_schema_rejects_structural_violations() {
    let mutations: [Mutation; 8] = [
        ("CLI asset as server", |value| {
            value["artifacts"][3]["role"] = json!("server");
        }),
        ("unknown kind", |value| {
            value["artifacts"][0]["kind"] = json!("archive");
        }),
        ("fifth artifact", |value| {
            let first = value["artifacts"][0].clone();
            value["artifacts"]
                .as_array_mut()
                .expect("should hold artifacts")
                .push(first);
        }),
        ("traversal companion name", |value| {
            value["artifacts"][0]["companions"]["sbom"]["name"] = json!("../escape.spdx.json");
        }),
        ("uppercase digest", |value| {
            value["artifacts"][0]["sha256"] = json!("A".repeat(64));
        }),
        ("oversize SBOM", |value| {
            value["artifacts"][0]["companions"]["sbom"]["size"] = json!(bounds::SPDX_BYTES + 1);
        }),
        ("extra field", |value| {
            value["artifacts"][1]["extra"] = json!(true);
        }),
        ("wrong compatibility pairing", |value| {
            value["artifacts"][1]["compatibility"]["minimum_version"] = json!("glibc-2.34");
        }),
    ];

    for (reason, mutate) in mutations {
        let mut value = index_value();
        mutate(&mut value);

        assert_eq!(
            context(validate_index_schema(&value)),
            ReleaseError::Document,
            "should reject {reason} in the schema step"
        );
    }
}

#[test]
fn draft_202012_prefix_items_and_items_are_enforced() {
    let schema = json!({
        "type": "array",
        "prefixItems": [{"const": "first"}, {"type": "integer"}],
        "items": false
    });

    assert!(
        schema_accepts(&schema, &json!(["first", 2])).expect("should compile 2020-12 schema"),
        "should accept prefix items"
    );
    for value in [
        json!(["wrong", 2]),
        json!(["first", "wrong"]),
        json!(["first", 2, 3]),
    ] {
        assert!(
            !schema_accepts(&schema, &value).expect("should compile 2020-12 schema"),
            "should reject invalid prefix or additional item {value}"
        );
    }
}

#[test]
fn schema_evaluation_never_retrieves_remote_references() {
    let schema = json!({"$ref": "https://schemas.example.com/remote.json"});

    assert!(
        schema_accepts(&schema, &json!({})).is_err(),
        "should refuse to resolve an external reference offline"
    );
}

// Release index.

#[test]
fn index_fixture_matches_closed_contract() {
    let index = parse_index(&index_value()).expect("should accept the closed fixture");

    assert_eq!(index.artifacts.len(), 4, "should keep four artifacts");
    assert_eq!(
        index.policy_version, 1,
        "should keep producer policy version"
    );
}

fn reject_index(mutate: impl FnOnce(&mut Value), expected: ReleaseError, reason: &str) {
    let mut value = index_value();
    mutate(&mut value);

    assert_eq!(
        context(parse_index(&value)),
        expected,
        "should reject index: {reason}"
    );
}

#[test]
fn index_rejects_unknown_schema_version_and_fields() {
    reject_index(
        |value| value["schema_version"] = json!(2),
        ReleaseError::Policy,
        "v2",
    );
    reject_index(
        |value| value["extra"] = json!(1),
        ReleaseError::Document,
        "extra field",
    );
    reject_index(
        |value| value["artifacts"][0]["companions"]["extra"] = value["tsjs_inventory"].clone(),
        ReleaseError::Document,
        "extra companion",
    );
}

#[test]
fn index_rejects_cli_as_server_substitution() {
    reject_index(
        |value| {
            value["artifacts"][3]["role"] = json!("server");
            value["artifacts"][3]["capabilities"] = json!(["fastly-production"]);
        },
        ReleaseError::Policy,
        "ts asset claiming server role and fastly-production",
    );
    reject_index(
        |value| value["artifacts"][3]["capabilities"] = json!(["fastly-production"]),
        ReleaseError::Policy,
        "ts asset claiming fastly-production",
    );
}

#[test]
fn index_rejects_role_only_mismatch() {
    reject_index(
        |value| value["artifacts"][3]["role"] = json!("server"),
        ReleaseError::Policy,
        "ts asset claiming server role with its own cli-execute capability",
    );
}

#[test]
fn index_rejects_fastly_staging_capability() {
    reject_index(
        |value| {
            value["artifacts"][0]["capabilities"] = json!(["fastly-production", "fastly-staging"]);
        },
        ReleaseError::Policy,
        "structurally valid but policy-forbidden fastly-staging",
    );
}

#[test]
fn index_rejects_unknown_kind_profile_and_capability() {
    reject_index(
        |value| value["artifacts"][0]["kind"] = json!("archive"),
        ReleaseError::Policy,
        "unknown kind",
    );
    reject_index(
        |value| value["artifacts"][1]["assurance_profile"] = json!("none"),
        ReleaseError::Policy,
        "unknown profile",
    );
    reject_index(
        |value| value["artifacts"][1]["capabilities"] = json!(["cli-admin"]),
        ReleaseError::Policy,
        "unknown capability",
    );
    reject_index(
        |value| value["artifacts"][1]["target"] = json!("x86_64-apple-darwin"),
        ReleaseError::Policy,
        "target swap",
    );
}

#[test]
fn index_rejects_duplicate_and_unknown_artifact_names() {
    reject_index(
        |value| value["artifacts"][2] = value["artifacts"][1].clone(),
        ReleaseError::Policy,
        "duplicate artifact",
    );
    reject_index(
        |value| value["artifacts"][1]["name"] = json!("ts-extra"),
        ReleaseError::Policy,
        "unknown artifact",
    );
    reject_index(
        |value| {
            value["artifacts"]
                .as_array_mut()
                .expect("should hold artifacts")
                .pop();
        },
        ReleaseError::Policy,
        "three artifacts",
    );
}

#[test]
fn index_rejects_unsafe_names_and_companion_naming() {
    for name in [
        "../escape.spdx.json",
        "dir/name.spdx.json",
        "dir\\name.spdx.json",
        "line\nbreak.spdx.json",
        ".hidden.spdx.json",
    ] {
        reject_index(
            |value| value["artifacts"][0]["companions"]["sbom"]["name"] = json!(name),
            ReleaseError::Policy,
            "unsafe companion name",
        );
    }
    reject_index(
        |value| {
            value["artifacts"][0]["companions"]["sbom"]["name"] =
                json!("ts-aarch64-apple-darwin.spdx.json");
        },
        ReleaseError::Policy,
        "companion naming another artifact",
    );
    reject_index(
        |value| value["tsjs_inventory"]["name"] = json!("inventory.json"),
        ReleaseError::Policy,
        "inventory name",
    );
}

#[test]
fn index_rejects_bad_digests_lengths_and_commits() {
    reject_index(
        |value| value["artifacts"][0]["sha256"] = json!("A".repeat(64)),
        ReleaseError::Policy,
        "uppercase digest",
    );
    reject_index(
        |value| value["artifacts"][0]["size"] = json!(-1),
        ReleaseError::Document,
        "negative length",
    );
    reject_index(
        |value| value["artifacts"][0]["size"] = json!(1.5),
        ReleaseError::Document,
        "fractional length",
    );
    reject_index(
        |value| value["artifacts"][0]["size"] = json!(9_007_199_254_740_992_u64),
        ReleaseError::Document,
        "unsafe integer",
    );
    reject_index(
        |value| value["artifacts"][0]["companions"]["sbom"]["size"] = json!(bounds::SPDX_BYTES + 1),
        ReleaseError::Policy,
        "SBOM over 16 MiB",
    );
    reject_index(
        |value| value["recipe_source_commit"] = json!(OTHER_COMMIT),
        ReleaseError::Policy,
        "recipe source differs",
    );
    reject_index(
        |value| value["source_commit"] = json!(OTHER_COMMIT),
        ReleaseError::Policy,
        "source not approved",
    );
    reject_index(
        |value| value["tag"] = json!("v1.2.1"),
        ReleaseError::Policy,
        "tag not approved",
    );
    reject_index(
        |value| value["artifacts"][1]["js_inventory_sha256"] = json!("e".repeat(64)),
        ReleaseError::Policy,
        "mismatched JS inventory",
    );
}

#[test]
fn approved_inputs_reject_malformed_commit_and_tag() {
    for commit in ["0123", &COMMIT.to_uppercase(), &format!("{COMMIT}\n")] {
        assert!(
            ApprovedCommit::parse(commit).is_err(),
            "should reject commit {commit:?}"
        );
    }
    for tag in ["1.2.0", "v1.2", "v01.2.0", "v1.2.0\n", "-v1.2.0"] {
        assert!(ReleaseTag::parse(tag).is_err(), "should reject tag {tag:?}");
    }
    assert!(
        ReleaseTag::parse("v1.2.0-rc.1+build.5").is_ok(),
        "should accept SemVer tag"
    );
}
