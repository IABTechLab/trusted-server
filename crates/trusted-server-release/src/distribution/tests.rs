//! Pure candidate index tests over fictional entry facts.
//!
//! Entries come from [`EntryParts`] with fictional bytes and identities; a
//! real completed build cannot be produced without process execution, so the
//! completed-build wiring rejections live in the recipe tests.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::entry::{
    CompatibilityTest, EntryParts, ModuleObservation, RebuildComparison, RecipeFacts,
    policy_identity, render_entry,
};
use super::*;
use crate::documents::{validate_release_index_artifact, validate_spdx};
use crate::policy::{CLI_POLICY_VERSION, FixedArtifact};

mod golden;
mod summary;
mod transfer;

/// Fictional approved source commit of every entry.
const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
/// Another fictional commit.
const OTHER_COMMIT: &str = "89abcdef0123456789abcdef0123456789abcdef";
/// The actual tracked consumer policy bytes, so the CLI accepts them as current.
const POLICY: &[u8] = include_bytes!("../../../trusted-server-cli/release-policy.json");
/// The CLI's structural positive index fixture.
const CLI_INDEX_FIXTURE: &[u8] =
    include_bytes!("../../../trusted-server-cli/tests/fixtures/release/release-index.json");
const FASTLY: &str = "trusted-server-fastly.wasm";
const LINUX: &str = "ts-x86_64-unknown-linux-gnu";

fn digest(text: &str) -> String {
    sha256_hex(text.as_bytes())
}

fn module(id: &str, embedded: bool) -> ModuleObservation {
    let body = format!("fictional {id} module body");
    ModuleObservation {
        id: id.to_owned(),
        filename: format!("tsjs-{id}.js"),
        size: body.len() as u64,
        sha256: digest(&body),
        embedded,
    }
}

fn inventory() -> Vec<u8> {
    let modules: Vec<Value> = [module("core", true), module("prebid", true)]
        .iter()
        .map(|module| {
            json!({"id": module.id, "filename": module.filename, "size": module.size,
                "sha256": module.sha256, "contributions": [{"package": "fictional-js"}]})
        })
        .collect();
    canonical_json(
        &json!({"schema_version": 1, "source_commit": COMMIT, "modules": modules, "tools": []}),
        DocumentRole::Metadata,
    )
    .expect("should render the fictional inventory")
}

fn artifact_bytes(asset: &str) -> Vec<u8> {
    format!("fictional {asset} executable bytes").into_bytes()
}

fn spdx(asset: &str, artifact_sha256: &str) -> Vec<u8> {
    let document = json!({
        "spdxVersion": "SPDX-2.3",
        "SPDXID": "SPDXRef-DOCUMENT",
        "name": asset,
        "dataLicense": "CC0-1.0",
        "documentNamespace": format!("https://example.com/spdx/candidate/{asset}"),
        "creationInfo": {
            "creators": ["Tool: fictional-candidate-fixture"],
            "created": "2026-01-01T00:00:00Z",
            "comment": "Unqualified candidate fixture; gaps: fictional-coverage-gap",
        },
        "documentDescribes": ["SPDXRef-Artifact"],
        "packages": [{
            "SPDXID": "SPDXRef-Artifact",
            "name": asset,
            "downloadLocation": "NOASSERTION",
            "filesAnalyzed": false,
            "licenseConcluded": "NOASSERTION",
            "licenseDeclared": "NOASSERTION",
            "copyrightText": "NOASSERTION",
            "checksums": [{"algorithm": "SHA256", "checksumValue": artifact_sha256}],
        }],
        "relationships": [{
            "spdxElementId": "SPDXRef-DOCUMENT",
            "relationshipType": "DESCRIBES",
            "relatedSpdxElement": "SPDXRef-Artifact",
        }],
    });
    validate_spdx(&document).expect("should keep the fictional SPDX schema-valid");
    canonical_json(&document, DocumentRole::Spdx).expect("should render the fictional SPDX")
}

fn parts(asset: &'static str) -> EntryParts {
    let asset_id = AssetId::parse(asset).expect("should resolve the fixed asset");
    let bytes = artifact_bytes(asset);
    let artifact_sha256 = sha256_hex(&bytes);
    let fastly = asset == FASTLY;
    let (comparison, test) = if fastly {
        (
            RebuildComparison::Matched {
                reproduction_sha256: artifact_sha256.clone(),
                reproduction_size: bytes.len() as u64,
            },
            CompatibilityTest {
                name: "fastly-final-byte-publisher-request",
                facts: BTreeMap::from([
                    ("viceroy_version", "0.0.0-fictional".to_owned()),
                    ("health_status", "200".to_owned()),
                ]),
            },
        )
    } else {
        (
            RebuildComparison::Unavailable,
            CompatibilityTest {
                name: "native-startup",
                facts: BTreeMap::from([("startup_kind", "fictional-startup".to_owned())]),
            },
        )
    };
    EntryParts {
        asset: asset_id,
        source_commit: COMMIT.to_owned(),
        policy_version: CLI_POLICY_VERSION,
        policy_sha256: sha256_hex(POLICY),
        spdx: spdx(asset, &artifact_sha256),
        artifact_sha256,
        artifact_size: bytes.len() as u64,
        inventory: inventory(),
        modules: vec![module("prebid", fastly), module("core", true)],
        spdx_gaps: vec!["fictional-coverage-gap".to_owned()],
        recipe: RecipeFacts {
            definition_sha256: digest("fictional recipe definition"),
            invocation_sha256: digest("fictional invocation"),
            cargo_lock_sha256: digest("fictional Cargo.lock"),
            materials_digest: digest("fictional materials"),
            rust_tools_digest: digest("fictional Rust tools"),
        },
        comparison,
        compatibility_tests: vec![test],
    }
}

fn entry(asset: &'static str) -> CandidateArtifactEntry {
    render_entry(parts(asset)).expect("should render the fictional entry")
}

fn entries() -> Vec<CandidateArtifactEntry> {
    FIXED_ARTIFACTS
        .iter()
        .map(|fixed| entry(fixed.name))
        .collect()
}

fn assembled() -> CandidateIndex {
    assemble_candidate_index(&entries()).expect("should assemble the candidate index")
}

fn rejection<T: core::fmt::Debug>(result: Result<T, Report<ReleaseError>>) -> String {
    format!("{:?}", result.expect_err("should reject the candidate"))
}

fn companion(entry: &CandidateArtifactEntry, role: CompanionRole) -> Value {
    let file = entry
        .companions
        .iter()
        .find(|file| file.role == role)
        .expect("should carry the companion");
    serde_json::from_slice(&file.bytes).expect("should parse the companion")
}

fn rendered_artifacts(index: &CandidateIndex) -> (Value, Vec<Value>) {
    let value: Value = serde_json::from_slice(index.bytes()).expect("should parse the index");
    let artifacts = value["artifacts"]
        .as_array()
        .expect("should list artifacts")
        .clone();
    (value, artifacts)
}

// ---- Step 1: retained schema validators ----

#[test]
fn release_index_validator_accepts_the_existing_positive_fixture() {
    let value = parse_json(CLI_INDEX_FIXTURE, DocumentRole::Metadata)
        .expect("should parse the CLI index fixture");
    validate_release_index(&value).expect("should accept the structural positive fixture");
    validate_release_index_artifact(&value["artifacts"][0])
        .expect("should accept a valid fixture artifact entry");
}

#[test]
fn artifact_validator_rejects_a_fastly_entry_with_the_cli_capability() {
    let value = parse_json(CLI_INDEX_FIXTURE, DocumentRole::Metadata)
        .expect("should parse the CLI index fixture");
    let mut fastly = value["artifacts"]
        .as_array()
        .expect("should list artifacts")
        .iter()
        .find(|artifact| artifact["name"] == FASTLY)
        .expect("should contain the Fastly entry")
        .clone();
    fastly["capabilities"] = json!(["cli-execute"]);
    assert!(
        validate_release_index_artifact(&fastly).is_err(),
        "should reject cli-execute on the Fastly asset"
    );
    fastly["capabilities"] = json!(["fastly-production", "unknown-capability"]);
    assert!(
        validate_release_index_artifact(&fastly).is_err(),
        "should reject an unknown capability"
    );
}

// ---- Step 2: fixed identity accessor ----

fn schema_row(name: &str) -> Value {
    let schema = parse_json(
        include_bytes!("../../../../docs/superpowers/specs/schemas/release-index-v1.schema.json"),
        DocumentRole::Metadata,
    )
    .expect("should parse the retained schema");
    schema["$defs"]["artifact"]["oneOf"]
        .as_array()
        .expect("should list identity rows")
        .iter()
        .find(|row| row["properties"]["name"]["const"] == name)
        .expect("should have a row per asset")["properties"]
        .clone()
}

fn minimum_version(runtime: &str) -> Value {
    let schema = parse_json(
        include_bytes!("../../../../docs/superpowers/specs/schemas/release-index-v1.schema.json"),
        DocumentRole::Metadata,
    )
    .expect("should parse the retained schema");
    schema["$defs"]["compatibility"]["oneOf"]
        .as_array()
        .expect("should list compatibility rows")
        .iter()
        .find(|row| row["properties"]["runtime"]["const"] == runtime)
        .expect("should have a compatibility row")["properties"]["minimum_version"]["const"]
        .clone()
}

fn check_identity_row(fixed: &FixedArtifact) {
    let identity = fixed.index_identity();
    let row = schema_row(fixed.name);
    for key in [
        "name",
        "role",
        "kind",
        "package",
        "target",
        "variant",
        "assurance_profile",
    ] {
        assert_eq!(
            identity[key], row[key]["const"],
            "should render the schema {key} of {}",
            fixed.name
        );
    }
    let capabilities = &row["capabilities"];
    let expected = if capabilities["const"].is_array() {
        capabilities["const"].clone()
    } else {
        json!([capabilities["contains"]["const"]])
    };
    assert_eq!(
        identity["capabilities"], expected,
        "should render only the required capability of {}",
        fixed.name
    );
    let compatibility = &identity["compatibility"];
    for key in ["runtime", "architecture"] {
        assert_eq!(
            compatibility[key], row["compatibility"]["properties"][key]["const"],
            "should render the schema compatibility {key} of {}",
            fixed.name
        );
    }
    assert_eq!(
        compatibility["minimum_version"],
        minimum_version(compatibility["runtime"].as_str().unwrap_or_default()),
        "should render the schema minimum version of {}",
        fixed.name
    );
    assert_eq!(
        identity.as_object().map(serde_json::Map::len),
        Some(9),
        "should render exactly the nine identity fields"
    );
}

#[test]
fn fixed_identity_accessor_equals_the_schema_row_for_every_asset() {
    FIXED_ARTIFACTS.iter().for_each(check_identity_row);
}

// ---- Step 3: valid four-entry join ----

#[test]
fn four_valid_entries_assemble_a_schema_valid_candidate_index() {
    let index = assembled();
    let (value, artifacts) = rendered_artifacts(&index);
    validate_release_index(&value).expect("should satisfy the retained schema");
    assert_eq!(
        index.name(),
        CANDIDATE_INDEX_NAME,
        "should use the fixed name"
    );
    assert_ne!(
        index.name(),
        "release-index.json",
        "should never be named as a release"
    );
    assert_eq!(value["tag"], CANDIDATE_TAG, "should carry the sentinel tag");
    assert_eq!(value["source_commit"], COMMIT, "should name the source");
    assert_eq!(
        value["recipe_source_commit"], COMMIT,
        "recipe commit should equal source"
    );
    assert_eq!(
        value["cohort_source_commit"], COMMIT,
        "cohort commit should equal source"
    );
    assert_eq!(
        value["policy_sha256"],
        sha256_hex(POLICY),
        "should name the policy"
    );
    assert_eq!(
        index.sha256(),
        sha256_hex(index.bytes()),
        "should hash exact bytes"
    );
    let names: Vec<&str> = artifacts
        .iter()
        .filter_map(|a| a["name"].as_str())
        .collect();
    let mut sorted = names.clone();
    sorted.sort_unstable();
    assert_eq!(names, sorted, "should emit artifacts sorted by name");
    for artifact in &artifacts {
        let name = artifact["name"].as_str().unwrap_or_default();
        for (key, suffix) in BUNDLE_SUFFIXES {
            assert_eq!(
                artifact["companions"][key],
                json!({"name": format!("{name}{suffix}"), "size": 0, "sha256": sha256_hex(b"")}),
                "should render the true empty-file record for {name} {key}"
            );
        }
    }
    assert_eq!(
        index.companions().len(),
        16,
        "should carry sixteen companions"
    );
    assert!(
        index
            .gaps()
            .iter()
            .any(|gap| gap == "candidate-tag-sentinel"),
        "should state the sentinel gap"
    );
}

// ---- Step 4: join rejections ----

#[test]
fn join_rejects_missing_extra_and_duplicate_assets() {
    let mut three = entries();
    three.pop();
    assert!(
        rejection(assemble_candidate_index(&three)).contains("missing asset"),
        "should reject three entries"
    );
    let mut five = entries();
    five.push(entry(LINUX));
    assert!(
        rejection(assemble_candidate_index(&five)).contains("more than once"),
        "should reject five entries"
    );
    let mut duplicate = entries();
    duplicate[0] = entry(LINUX);
    assert!(
        assemble_candidate_index(&duplicate).is_err(),
        "should reject four entries with one asset twice"
    );
}

#[test]
fn join_rejects_mixed_source_policy_or_inventory() {
    let mut source = entries();
    source[2].source_commit = OTHER_COMMIT.to_owned();
    assert!(
        rejection(assemble_candidate_index(&source)).contains("different source commits"),
        "should reject mixed source commits"
    );
    let mut policy = entries();
    policy[1].policy_sha256 = "f".repeat(64);
    assert!(
        rejection(assemble_candidate_index(&policy)).contains("different producer policies"),
        "should reject mixed policy identities"
    );
    let mut inventory = entries();
    inventory[3].inventory.push(b' ');
    assert!(
        rejection(assemble_candidate_index(&inventory)).contains("different JS inventories"),
        "should reject mixed inventory bytes"
    );
}

#[test]
fn rendered_check_rejects_fastly_staging_and_non_sentinel_bundles() {
    let index = assembled();
    let (value, artifacts) = rendered_artifacts(&index);
    let mut fastly = artifacts
        .iter()
        .find(|artifact| artifact["name"] == FASTLY)
        .expect("should render Fastly")
        .clone();
    fastly["capabilities"] = json!(["fastly-production", "fastly-staging"]);
    validate_release_index_artifact(&fastly)
        .expect("should be structurally compatible with the schema");
    assert!(
        rejection(check_rendered_artifact(&fastly, &value["tsjs_inventory"]))
            .contains("fixed identity"),
        "should reject the currently forbidden fastly-staging capability"
    );
    let mut bundle = artifacts[1].clone();
    bundle["companions"]["provenance"]["size"] = json!(1);
    assert!(
        rejection(check_rendered_artifact(&bundle, &value["tsjs_inventory"]))
            .contains("awaiting-signature sentinel"),
        "should reject a non-sentinel bundle record"
    );
}

#[test]
fn join_rejects_unsafe_colliding_or_misnamed_companions() {
    for name in [
        "ts-x86_64-unknown-linux-gnu/../evil.spdx.json",
        "ts-x86_64-unknown-linux-gnu\n.spdx.json",
        "TS-X86_64-UNKNOWN-LINUX-GNU.spdx.json",
    ] {
        let mut altered = entries();
        let linux = altered
            .iter_mut()
            .find(|entry| entry.name() == LINUX)
            .expect("should hold the Linux entry");
        linux.companions[0].name = name.to_owned();
        assert!(
            assemble_candidate_index(&altered).is_err(),
            "should reject companion name {name:?}"
        );
    }
    let mut names = BTreeSet::new();
    insert_name(&mut names, "tsjs-inventory.json").expect("should accept the inventory name");
    assert!(
        insert_name(&mut names, "TSJS-Inventory.json").is_err(),
        "should reject a case-colliding name"
    );
    assert!(
        insert_name(&mut names, "line\nbreak.json").is_err(),
        "should reject a newline in a name"
    );
}

#[test]
fn join_rejects_companion_size_or_digest_mismatch() {
    let mut size = entries();
    size[0].companions[1].size += 1;
    assert!(
        rejection(assemble_candidate_index(&size)).contains("recorded size or digest"),
        "should reject an altered companion length"
    );
    let mut sha = entries();
    sha[0].companions[2].sha256 = "0".repeat(64);
    assert!(
        rejection(assemble_candidate_index(&sha)).contains("recorded size or digest"),
        "should reject an altered companion digest"
    );
    let mut bytes = entries();
    bytes[0].companions[3].bytes.push(b' ');
    assert!(
        assemble_candidate_index(&bytes).is_err(),
        "should reject changed companion bytes"
    );
}

// ---- Step 5: signature bundle slots ----

/// Compile-time proof that no signed bundle can be constructed.
fn signed_bundles_are_uninhabited(signed: &SignedCompanions) -> ! {
    match *signed {}
}

#[test]
fn awaiting_signature_renders_the_true_empty_file_record() {
    let _proof: fn(&SignedCompanions) -> ! = signed_bundles_are_uninhabited;
    assert_eq!(
        BundleSlot::AwaitingSignature.record("example.sbom.sigstore.json"),
        json!({"name": "example.sbom.sigstore.json", "size": 0, "sha256": EMPTY_SHA256}),
        "should render the empty-file record"
    );
    assert_eq!(
        EMPTY_SHA256,
        sha256_hex(b""),
        "should be the SHA-256 of zero bytes"
    );
}

// ---- Step 6: companion rendering rules ----

#[test]
fn build_evidence_lists_sorted_prepared_modules_and_the_embedded_subset() {
    let linux = entry(LINUX);
    let evidence = companion(&linux, CompanionRole::BuildEvidence);
    let ids = |key: &str| -> Vec<Value> {
        evidence[key]
            .as_array()
            .expect("should list modules")
            .iter()
            .map(|module| module["id"].clone())
            .collect()
    };
    assert_eq!(
        ids("prepared_modules"),
        [json!("core"), json!("prebid")],
        "should sort"
    );
    assert_eq!(
        ids("embedded_modules"),
        [json!("core")],
        "should list the embedded subset"
    );
    let mut extra = parts(LINUX);
    extra.modules.push(module("unlisted", true));
    assert!(
        rejection(render_entry(extra)).contains("differ from the shared inventory"),
        "should reject prepared modules outside the inventory"
    );
}

#[test]
fn fastly_entry_rejects_a_js_build_material_module() {
    let mut fastly = parts(FASTLY);
    fastly.modules[0].embedded = false;
    assert!(
        rejection(render_entry(fastly)).contains("embed every prepared JS module"),
        "should reject a Fastly module classified as build material"
    );
}

#[test]
fn rebuild_check_is_matched_only_with_fastly_equality() {
    let fastly = companion(&entry(FASTLY), CompanionRole::RebuildCheck);
    assert_eq!(
        fastly["outcome"], "matched",
        "should record Fastly equality"
    );
    let linux = companion(&entry(LINUX), CompanionRole::RebuildCheck);
    assert_eq!(
        linux["outcome"], "unavailable",
        "should record native unavailable"
    );
    assert_eq!(
        linux["reproduction_sha256"],
        Value::Null,
        "should not invent a digest"
    );
    let mut unavailable = parts(FASTLY);
    unavailable.comparison = RebuildComparison::Unavailable;
    assert!(
        render_entry(unavailable).is_err(),
        "Fastly should require equality"
    );
    let mut matched = parts(LINUX);
    matched.comparison = RebuildComparison::Matched {
        reproduction_sha256: matched.artifact_sha256.clone(),
        reproduction_size: matched.artifact_size,
    };
    assert!(
        render_entry(matched).is_err(),
        "native should never claim matched"
    );
    let mut different = parts(FASTLY);
    different.comparison = RebuildComparison::Matched {
        reproduction_sha256: "0".repeat(64),
        reproduction_size: different.artifact_size,
    };
    assert!(
        render_entry(different).is_err(),
        "should reject other equality bytes"
    );
}

#[test]
fn every_informational_companion_is_unqualified_with_gaps_and_null_transfers() {
    for entry in entries() {
        assert!(!entry.gaps().is_empty(), "should state entry gaps");
        for role in [
            CompanionRole::BuildEvidence,
            CompanionRole::BuildRecipe,
            CompanionRole::RebuildCheck,
        ] {
            let document = companion(&entry, role);
            assert_eq!(
                document["release_qualification"], false,
                "should never claim qualification"
            );
            let gaps = document["gaps"].as_array().expect("should list gaps");
            let mut sorted = gaps.clone();
            sorted.sort_by(|left, right| left.as_str().cmp(&right.as_str()));
            sorted.dedup();
            assert!(
                !gaps.is_empty() && *gaps == sorted,
                "gaps should be sorted unique"
            );
            assert_eq!(
                document["workflow_run_id"],
                Value::Null,
                "run ID is unavailable"
            );
        }
    }
}

#[test]
fn entry_rejects_a_wrong_spdx_root_or_missing_gaps() {
    let mut root = parts(FASTLY);
    root.spdx = spdx(FASTLY, &"0".repeat(64));
    assert!(
        rejection(render_entry(root)).contains("checksum differs"),
        "should reject an SPDX root bound to other bytes"
    );
    let mut gaps = parts(FASTLY);
    gaps.spdx_gaps.clear();
    assert!(
        render_entry(gaps).is_err(),
        "should require candidate SPDX gaps"
    );
    let mut tests = parts(LINUX);
    tests.compatibility_tests.clear();
    assert!(
        render_entry(tests).is_err(),
        "should require a compatibility test"
    );
}

// ---- Step 7: determinism ----

#[test]
fn assembly_is_deterministic_across_entry_order() {
    let first = assembled();
    let second = assembled();
    assert_eq!(
        first.bytes(),
        second.bytes(),
        "should render identical bytes"
    );
    let mut permuted = entries();
    permuted.reverse();
    permuted.swap(0, 2);
    let reordered = assemble_candidate_index(&permuted).expect("should accept any entry order");
    assert_eq!(
        first.bytes(),
        reordered.bytes(),
        "should ignore entry order"
    );
    assert_eq!(
        first.companions(),
        reordered.companions(),
        "should match companions"
    );
    assert_eq!(first.gaps(), reordered.gaps(), "should match gaps");
}

#[test]
fn inventory_join_uses_the_consumer_inventory_byte_limit() {
    assert_eq!(
        super::joins::INVENTORY_ROLE.limit(),
        super::joins::CLI_INVENTORY_BYTES,
        "should parse the inventory under the CLI inventory bound"
    );
}

// ---- Producer policy identity from approved bytes ----

#[test]
fn policy_identity_derives_version_and_digest_from_the_same_bytes() {
    assert_eq!(
        policy_identity(POLICY).expect("should accept the tracked consumer policy"),
        (CLI_POLICY_VERSION, sha256_hex(POLICY)),
        "should read the version and hash the exact approved bytes"
    );
}

#[test]
fn policy_identity_rejects_mismatched_missing_zero_or_non_integer_versions() {
    for (case, bytes) in [
        ("mismatched version", br#"{"policy_version":2}"#.as_slice()),
        ("missing version", br#"{"repository":"example"}"#.as_slice()),
        ("version zero", br#"{"policy_version":0}"#.as_slice()),
        (
            "fractional version",
            br#"{"policy_version":1.0}"#.as_slice(),
        ),
        ("string version", br#"{"policy_version":"1"}"#.as_slice()),
        ("malformed JSON", b"not JSON".as_slice()),
    ] {
        assert!(
            policy_identity(bytes).is_err(),
            "should reject the {case} policy"
        );
    }
}
