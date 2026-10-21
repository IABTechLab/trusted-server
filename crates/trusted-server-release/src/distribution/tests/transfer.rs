//! Pure tests of the test-only ordinary-CI entry transfer and its collector.
//!
//! Fictional entries are exported to flat files, rebuilt from those bytes and
//! assembled, which must reproduce the committed golden candidate index.

use std::path::Path;

use crate::distribution::transfer::{
    ARTIFACT_MEMBER, EntryDescriptor, ReceivedEntry, assemble_received, check_distinct_uploads,
    member_names, upload_name, write_candidate_index,
};

use super::*;

/// Fictional producing workflow run of every descriptor.
const RUN_ID: u64 = 4242;
/// The committed golden candidate index bytes.
const GOLDEN_INDEX: &[u8] =
    include_bytes!("../../../tests/fixtures/index/candidate/candidate-release-index.json");
const MACOS_ARM: &str = "ts-aarch64-apple-darwin";

fn asset_id(asset: &str) -> AssetId {
    AssetId::parse(asset).expect("should resolve the fixed asset")
}

/// The six flat members of the fictional entry for `asset`.
fn members(asset: &'static str) -> BTreeMap<String, Vec<u8>> {
    let artifact = artifact_bytes(asset);
    entry(asset)
        .transfer_members(&artifact)
        .into_iter()
        .map(|(name, bytes)| (name, bytes.to_vec()))
        .collect()
}

fn sizes(members: &BTreeMap<String, Vec<u8>>) -> BTreeMap<String, u64> {
    members
        .iter()
        .map(|(name, bytes)| (name.clone(), bytes.len() as u64))
        .collect()
}

/// The descriptor JSON the CI describe step would emit for `asset`.
fn descriptor_json(asset: &'static str, artifact_id: u64) -> Value {
    let entry_sha256 = entry(asset)
        .recheck_for_measurement()
        .expect("should recheck the fictional entry")
        .entry_sha256()
        .to_owned();
    json!({
        "asset": asset,
        "entry_sha256": entry_sha256,
        "members": sizes(&members(asset)),
        "artifact_id": artifact_id,
        "artifact_name": format!("candidate-entry-{asset}-{RUN_ID}-1"),
        "archive_sha256": format!("sha256:{}", digest(&format!("fictional archive {asset}"))),
        "run_attempt": 1,
    })
}

fn render_metadata(value: &Value) -> Vec<u8> {
    canonical_json(value, DocumentRole::Metadata).expect("should render the fictional JSON")
}

fn descriptor(asset: &'static str, artifact_id: u64) -> EntryDescriptor {
    EntryDescriptor::parse(
        &render_metadata(&descriptor_json(asset, artifact_id)),
        asset_id(asset),
        RUN_ID,
    )
    .expect("should accept the fictional descriptor")
}

fn received() -> Vec<ReceivedEntry> {
    FIXED_ARTIFACTS
        .iter()
        .zip(1_u64..)
        .map(|(fixed, artifact_id)| ReceivedEntry {
            descriptor: descriptor(fixed.name, artifact_id),
            members: members(fixed.name),
        })
        .collect()
}

fn rebuild(
    asset: &'static str,
    members: &BTreeMap<String, Vec<u8>>,
) -> Result<CandidateArtifactEntry, Report<ReleaseError>> {
    CandidateArtifactEntry::from_ordinary_ci_transfer(asset_id(asset), COMMIT, POLICY, members)
}

/// Rewrites the companion with `suffix` of `asset` through `edit`.
fn edit_companion(
    members: &mut BTreeMap<String, Vec<u8>>,
    asset: &str,
    suffix: &str,
    edit: impl FnOnce(&mut Value),
) {
    let bytes = members
        .get_mut(&format!("{asset}{suffix}"))
        .expect("should carry the companion");
    let mut value: Value = serde_json::from_slice(bytes).expect("should parse the companion");
    edit(&mut value);
    let role = if suffix == ".spdx.json" {
        DocumentRole::Spdx
    } else {
        DocumentRole::Companion
    };
    *bytes = canonical_json(&value, role).expect("should render the edited companion");
}

/// Applies `edit` to all three informational companions of `asset`.
fn edit_informational(
    members: &mut BTreeMap<String, Vec<u8>>,
    asset: &str,
    edit: impl Fn(&mut Value),
) {
    for suffix in [
        ".build-evidence.json",
        ".build-recipe.json",
        ".rebuild-check.json",
    ] {
        edit_companion(members, asset, suffix, &edit);
    }
}

fn rebuild_rejection(asset: &'static str, members: &BTreeMap<String, Vec<u8>>) -> String {
    rejection(rebuild(asset, members))
}

fn read_directory(directory: &Path) -> BTreeMap<String, Vec<u8>> {
    std::fs::read_dir(directory)
        .expect("should list the directory")
        .map(|item| {
            let item = item.expect("should read the directory entry");
            let name = item
                .file_name()
                .into_string()
                .expect("should use UTF-8 names");
            let bytes = std::fs::read(item.path()).expect("should read the member");
            (name, bytes)
        })
        .collect()
}

// ---- Round trip ----

#[test]
fn exported_entries_rebuild_into_exactly_the_golden_candidate_index() {
    let scratch = tempfile::tempdir().expect("should create a scratch directory");
    let received: Vec<ReceivedEntry> = entries()
        .iter()
        .zip(1_u64..)
        .map(|(entry, artifact_id)| {
            let asset = entry.name();
            let upload = scratch.path().join(format!("{asset}-upload"));
            let transfer = scratch.path().join(format!("{asset}-transfer.json"));
            entry
                .export_flat_for_ordinary_ci(&artifact_bytes(asset), &upload, &transfer)
                .expect("should export the fictional entry");
            let mut description: Value = serde_json::from_slice(
                &std::fs::read(&transfer).expect("should read the transfer file"),
            )
            .expect("should parse the transfer file");
            description["artifact_id"] = json!(artifact_id);
            description["artifact_name"] = json!(format!("candidate-entry-{asset}-{RUN_ID}-1"));
            description["archive_sha256"] = json!(digest(asset));
            description["run_attempt"] = json!(1);
            ReceivedEntry {
                descriptor: EntryDescriptor::parse(
                    &render_metadata(&description),
                    asset_id(asset),
                    RUN_ID,
                )
                .expect("should accept the exported descriptor"),
                members: read_directory(&upload),
            }
        })
        .collect();

    let index =
        assemble_received(&received, COMMIT, POLICY).expect("should assemble rebuilt entries");

    assert_eq!(
        index.bytes(),
        GOLDEN_INDEX,
        "should rebuild exactly the committed candidate index"
    );
    assert_eq!(
        index.bytes(),
        assembled().bytes(),
        "should equal the directly assembled index"
    );
}

#[test]
fn rebuilt_entry_rechecks_to_the_original_entry_digest() {
    for fixed in FIXED_ARTIFACTS {
        let original = entry(fixed.name)
            .recheck_for_measurement()
            .expect("should recheck the original entry");
        let rebuilt = rebuild(fixed.name, &members(fixed.name))
            .expect("should rebuild the fictional entry")
            .recheck_for_measurement()
            .expect("should recheck the rebuilt entry");

        assert_eq!(
            rebuilt.entry_sha256(),
            original.entry_sha256(),
            "should rebuild the identical entry for {}",
            fixed.name
        );
    }
}

// ---- Rebuild rejections ----

#[test]
fn rebuild_rejects_missing_extra_or_misnamed_members() {
    let mut missing = members(FASTLY);
    missing.remove(ARTIFACT_MEMBER);
    assert!(
        rebuild_rejection(FASTLY, &missing).contains("exactly its six flat members"),
        "should reject a missing executable"
    );

    let mut extra = members(FASTLY);
    extra.insert("candidate-build-record.json".to_owned(), b"{}".to_vec());
    assert!(
        rebuild_rejection(FASTLY, &extra).contains("exactly its six flat members"),
        "should reject an extra member"
    );

    let mut misnamed = members(FASTLY);
    let spdx = misnamed
        .remove(&format!("{FASTLY}.spdx.json"))
        .expect("should carry the SPDX companion");
    misnamed.insert(format!("{LINUX}.spdx.json"), spdx);
    assert!(
        rebuild_rejection(FASTLY, &misnamed).contains("exactly its six flat members"),
        "should reject a companion named for another asset"
    );
}

#[test]
fn rebuild_rejects_evidence_for_another_asset_target_or_commit() {
    let mut asset = members(FASTLY);
    edit_companion(&mut asset, FASTLY, ".build-evidence.json", |evidence| {
        evidence["asset"] = json!(LINUX);
    });
    assert!(
        rebuild_rejection(FASTLY, &asset).contains("unqualified candidate evidence"),
        "should reject evidence naming another asset"
    );

    let mut target = members(LINUX);
    edit_companion(&mut target, LINUX, ".build-evidence.json", |evidence| {
        evidence["target"] = json!("x86_64-apple-darwin");
    });
    assert!(
        rebuild_rejection(LINUX, &target).contains("unqualified candidate evidence"),
        "should reject evidence naming another target"
    );

    let commit = CandidateArtifactEntry::from_ordinary_ci_transfer(
        asset_id(LINUX),
        OTHER_COMMIT,
        POLICY,
        &members(LINUX),
    );
    assert!(
        rejection(commit).contains("expected source"),
        "should reject evidence from another source commit"
    );
}

#[test]
fn rebuild_rejects_qualified_or_workflow_bound_companions() {
    let mut evidence = members(LINUX);
    edit_companion(&mut evidence, LINUX, ".build-evidence.json", |document| {
        document["release_qualification"] = json!(true);
    });
    assert!(
        rebuild_rejection(LINUX, &evidence).contains("unqualified candidate evidence"),
        "should reject qualified build evidence"
    );

    let mut recipe = members(LINUX);
    edit_companion(&mut recipe, LINUX, ".build-recipe.json", |document| {
        document["release_qualification"] = json!(true);
    });
    assert!(
        rebuild_rejection(LINUX, &recipe).contains("must be unqualified"),
        "should reject a qualified build recipe"
    );

    let mut bound = members(LINUX);
    edit_companion(&mut bound, LINUX, ".build-evidence.json", |document| {
        document["transfer_artifact_id"] = json!(7);
    });
    assert!(
        rebuild_rejection(LINUX, &bound).contains("unqualified candidate evidence"),
        "should reject a claimed transfer binding"
    );
}

#[test]
fn rebuild_rejects_differing_or_incomplete_gaps() {
    let mut differing = members(LINUX);
    edit_companion(&mut differing, LINUX, ".build-recipe.json", |document| {
        document["gaps"] = json!(["fictional-other-gap"]);
    });
    assert!(
        rebuild_rejection(LINUX, &differing).contains("exactly the entry gaps"),
        "should reject gaps differing between companions"
    );

    let mut incomplete = members(LINUX);
    edit_informational(&mut incomplete, LINUX, |document| {
        let gaps: Vec<Value> = document["gaps"]
            .as_array()
            .expect("should list gaps")
            .iter()
            .filter(|gap| *gap != "trusted-workflow-transfer-not-bound")
            .cloned()
            .collect();
        document["gaps"] = json!(gaps);
    });
    assert!(
        rebuild_rejection(LINUX, &incomplete).contains("omit the fixed gap"),
        "should reject gaps omitting a fixed entry gap"
    );

    let mut unsorted = members(LINUX);
    edit_informational(&mut unsorted, LINUX, |document| {
        let mut gaps = document["gaps"]
            .as_array()
            .expect("should list gaps")
            .clone();
        gaps.reverse();
        document["gaps"] = json!(gaps);
    });
    assert!(
        rebuild_rejection(LINUX, &unsorted).contains("sorted and unique"),
        "should reject unsorted gaps"
    );

    let mut fastly_native_gap = members(FASTLY);
    edit_informational(&mut fastly_native_gap, FASTLY, |document| {
        let mut gaps: Vec<String> =
            serde_json::from_value(document["gaps"].clone()).expect("should list gap strings");
        gaps.push("native-reproduction-unavailable".to_owned());
        gaps.sort();
        document["gaps"] = json!(gaps);
    });
    assert!(
        rebuild_rejection(FASTLY, &fastly_native_gap).contains("exactly for native assets"),
        "should reject the native reproduction gap on the Fastly asset"
    );
}

#[test]
fn rebuild_enforces_the_comparison_rule_per_asset() {
    let mut fastly_unavailable = members(FASTLY);
    edit_companion(
        &mut fastly_unavailable,
        FASTLY,
        ".rebuild-check.json",
        |document| {
            document["outcome"] = json!("unavailable");
            document["reproduction_sha256"] = Value::Null;
        },
    );
    assert!(
        rebuild_rejection(FASTLY, &fastly_unavailable).contains("checked Fastly equality"),
        "should reject an unavailable Fastly comparison"
    );

    let mut native_matched = members(LINUX);
    let artifact_sha256 = sha256_hex(&artifact_bytes(LINUX));
    edit_companion(
        &mut native_matched,
        LINUX,
        ".rebuild-check.json",
        |document| {
            document["outcome"] = json!("matched");
            document["reproduction_sha256"] = json!(artifact_sha256);
        },
    );
    assert!(
        rebuild_rejection(LINUX, &native_matched).contains("checked Fastly equality"),
        "should reject a matched native comparison"
    );

    let mut fastly_other = members(FASTLY);
    edit_companion(
        &mut fastly_other,
        FASTLY,
        ".rebuild-check.json",
        |document| {
            document["reproduction_sha256"] = json!(digest("fictional other reproduction"));
        },
    );
    assert!(
        rebuild_rejection(FASTLY, &fastly_other).contains("checked Fastly equality"),
        "should reject a Fastly reproduction digest other than canonical"
    );
}

#[test]
fn rebuild_rejects_changed_executable_inventory_or_spdx_root() {
    let mut executable = members(MACOS_ARM);
    executable.insert(
        ARTIFACT_MEMBER.to_owned(),
        b"fictional tampered executable".to_vec(),
    );
    assert!(
        rebuild_rejection(MACOS_ARM, &executable).contains("differs from its build evidence"),
        "should reject executable bytes other than the recorded artifact"
    );

    let mut inventory_change = members(MACOS_ARM);
    let mut value: Value = serde_json::from_slice(&inventory_change[INVENTORY_NAME])
        .expect("should parse the inventory");
    value["tools"] = json!([{"name": "fictional-tool"}]);
    inventory_change.insert(INVENTORY_NAME.to_owned(), render_metadata(&value));
    assert!(
        rebuild_rejection(MACOS_ARM, &inventory_change).contains("js_inventory_sha256"),
        "should reject an inventory other than the evidence digest names"
    );

    let mut spdx_root = members(MACOS_ARM);
    edit_companion(&mut spdx_root, MACOS_ARM, ".spdx.json", |document| {
        document["packages"][0]["checksums"][0]["checksumValue"] =
            json!(digest("fictional other executable"));
    });
    assert!(
        rebuild_rejection(MACOS_ARM, &spdx_root).contains("SPDX root package checksum"),
        "should reject an SPDX root naming another executable"
    );

    let mut embedded = members(FASTLY);
    edit_companion(&mut embedded, FASTLY, ".build-evidence.json", |document| {
        let first = document["embedded_modules"][0].clone();
        document["embedded_modules"] = json!([first]);
    });
    assert!(
        rebuild_rejection(FASTLY, &embedded).contains("embed every prepared JS module"),
        "should reject a Fastly executable omitting a prepared module"
    );
}

#[test]
fn rebuild_rejects_another_policy_version_or_an_oversized_companion() {
    let mut policy = parse_json(POLICY, DocumentRole::Metadata).expect("should parse the policy");
    policy["policy_version"] = json!(CLI_POLICY_VERSION + 1);
    let other_policy = render_metadata(&policy);
    let result = CandidateArtifactEntry::from_ordinary_ci_transfer(
        asset_id(LINUX),
        COMMIT,
        &other_policy,
        &members(LINUX),
    );
    assert!(
        rejection(result).contains("differs from compiled version"),
        "should reject a policy version other than the compiled one"
    );

    let mut oversized = members(LINUX);
    let limit = usize::try_from(DocumentRole::Spdx.limit()).expect("should fit the SPDX limit");
    oversized.insert(format!("{LINUX}.spdx.json"), vec![b' '; limit + 1]);
    assert!(
        rebuild_rejection(LINUX, &oversized).contains("role byte limit"),
        "should reject a companion over its role limit"
    );
}

// ---- Descriptor parser ----

fn descriptor_rejection(value: &Value, asset: &str) -> String {
    rejection(EntryDescriptor::parse(
        &render_metadata(value),
        asset_id(asset),
        RUN_ID,
    ))
}

#[test]
fn descriptor_accepts_the_exact_upload_outputs() {
    let parsed = descriptor(FASTLY, 9);

    assert_eq!(parsed.artifact_id(), 9, "should keep the exact upload ID");
    assert_eq!(
        parsed.artifact_name(),
        upload_name(asset_id(FASTLY), RUN_ID, 1),
        "should keep the exact upload name"
    );
    assert_eq!(
        parsed.archive_sha256(),
        digest(&format!("fictional archive {FASTLY}")),
        "should strip the optional sha256 prefix"
    );
    assert_eq!(
        parsed.members().keys().cloned().collect::<BTreeSet<_>>(),
        member_names(asset_id(FASTLY)),
        "should declare exactly the six flat members"
    );
}

#[test]
fn descriptor_rejects_unknown_or_missing_keys_and_another_asset() {
    let mut unknown = descriptor_json(FASTLY, 1);
    unknown["workflow_run_id"] = json!(1);
    assert!(
        descriptor_rejection(&unknown, FASTLY).contains("exactly its transfer and upload keys"),
        "should reject an unknown key"
    );

    let mut missing = descriptor_json(FASTLY, 1);
    missing
        .as_object_mut()
        .expect("should be an object")
        .remove("run_attempt");
    assert!(
        descriptor_rejection(&missing, FASTLY).contains("exactly its transfer and upload keys"),
        "should reject a missing key"
    );

    assert!(
        descriptor_rejection(&descriptor_json(FASTLY, 1), LINUX).contains("another asset"),
        "should reject a descriptor in another asset's slot"
    );
}

#[test]
fn descriptor_rejects_bad_identity_digest_or_name() {
    let cases: [(&str, Value, &str); 6] = [
        ("artifact_id", json!(0), "upload ID must be positive"),
        ("run_attempt", json!(0), "run attempt must be positive"),
        ("archive_sha256", json!("sha256:ABC"), "archive digest"),
        (
            "entry_sha256",
            json!(digest("x").to_uppercase()),
            "entry digest",
        ),
        (
            "artifact_name",
            json!(format!("candidate-entry-{FASTLY}-1-1")),
            "upload name must be",
        ),
        ("run_attempt", json!(2), "upload name must be"),
    ];
    for (key, replacement, expected) in cases {
        let mut value = descriptor_json(FASTLY, 1);
        value[key] = replacement;
        let message = descriptor_rejection(&value, FASTLY);
        assert!(
            message.contains(expected),
            "should reject a bad {key}: {message}"
        );
    }
}

#[test]
fn descriptor_rejects_wrong_member_names_or_sizes() {
    let mut renamed = descriptor_json(FASTLY, 1);
    let members = renamed["members"]
        .as_object_mut()
        .expect("should list members");
    let size = members
        .remove(ARTIFACT_MEMBER)
        .expect("should declare the executable");
    members.insert("executable".to_owned(), size);
    assert!(
        descriptor_rejection(&renamed, FASTLY).contains("exactly the six flat members"),
        "should reject a renamed member"
    );

    let mut empty = descriptor_json(FASTLY, 1);
    empty["members"][INVENTORY_NAME] = json!(0);
    assert!(
        descriptor_rejection(&empty, FASTLY).contains("empty or exceeds its bound"),
        "should reject an empty member"
    );

    let mut oversized = descriptor_json(FASTLY, 1);
    oversized["members"][format!("{FASTLY}.build-recipe.json")] =
        json!(DocumentRole::Companion.limit() + 1);
    assert!(
        descriptor_rejection(&oversized, FASTLY).contains("empty or exceeds its bound"),
        "should reject a member over its role bound"
    );

    let mut executable = descriptor_json(FASTLY, 1);
    executable["members"][ARTIFACT_MEMBER] = json!(DocumentRole::Executable.limit() + 1);
    assert!(
        descriptor_rejection(&executable, FASTLY).contains("empty or exceeds its bound"),
        "should reject an executable over its bound"
    );
}

#[test]
fn collector_rejects_repeated_upload_ids() {
    let first = descriptor(FASTLY, 5);
    let second = descriptor(LINUX, 5);

    assert!(
        rejection(check_distinct_uploads(&[&first, &second])).contains("distinct IDs"),
        "should reject two slots naming the same upload"
    );
}

// ---- Collector assembly ----

#[test]
fn collector_rejects_members_or_entry_digest_differing_from_the_descriptor() {
    let mut changed_member = received();
    changed_member[0]
        .members
        .insert(ARTIFACT_MEMBER.to_owned(), b"short".to_vec());
    assert!(
        rejection(assemble_received(&changed_member, COMMIT, POLICY))
            .contains("differ from their descriptor"),
        "should reject members differing from the declared sizes"
    );

    let mut other_entry = received();
    let asset = FIXED_ARTIFACTS[1].name;
    assert_eq!(
        other_entry[1].descriptor.asset(),
        asset_id(asset),
        "should receive entries in cohort order"
    );
    let mut value = descriptor_json(asset, 2);
    value["entry_sha256"] = json!(digest("fictional other entry"));
    other_entry[1].descriptor =
        EntryDescriptor::parse(&render_metadata(&value), asset_id(asset), RUN_ID)
            .expect("should accept the descriptor shape");
    assert!(
        rejection(assemble_received(&other_entry, COMMIT, POLICY))
            .contains("differs from the producing job's entry"),
        "should reject an entry other than the producing job checked"
    );

    let mut three = received();
    three.pop();
    assert!(
        rejection(assemble_received(&three, COMMIT, POLICY)).contains("missing asset"),
        "should never assemble from fewer than four entries"
    );
}

/// Inventory bytes like the fictional one, from `commit` with `tools`.
fn inventory_from(commit: &str, tools: Value) -> Vec<u8> {
    let mut value: Value = serde_json::from_slice(&inventory()).expect("should parse inventory");
    value["source_commit"] = json!(commit);
    value["tools"] = tools;
    render_metadata(&value)
}

/// Rebuilds the fictional entry of `asset` rendered from `parts`.
fn rebuilt_from(parts: EntryParts, commit: &str) -> CandidateArtifactEntry {
    let asset = parts.asset;
    let artifact = artifact_bytes(asset.artifact().name);
    let entry = render_entry(parts).expect("should render the varied entry");
    let members: BTreeMap<String, Vec<u8>> = entry
        .transfer_members(&artifact)
        .into_iter()
        .map(|(name, bytes)| (name, bytes.to_vec()))
        .collect();
    CandidateArtifactEntry::from_ordinary_ci_transfer(asset, commit, POLICY, &members)
        .expect("should rebuild the varied entry")
}

/// Rebuilt entries for every asset except Linux, plus `linux`.
fn rebuilt_with_linux(linux: CandidateArtifactEntry) -> Vec<CandidateArtifactEntry> {
    FIXED_ARTIFACTS
        .iter()
        .filter(|fixed| fixed.name != LINUX)
        .map(|fixed| rebuilt_from(parts(fixed.name), COMMIT))
        .chain([linux])
        .collect()
}

#[test]
fn rebuilt_entries_still_reject_mixed_commits_or_inventories() {
    let mut other = parts(LINUX);
    other.source_commit = OTHER_COMMIT.to_owned();
    other.inventory = inventory_from(OTHER_COMMIT, json!([]));
    let mixed_commit = rebuilt_with_linux(rebuilt_from(other, OTHER_COMMIT));
    assert!(
        rejection(assemble_candidate_index(&mixed_commit)).contains("different source commits"),
        "should reject rebuilt entries from different commits"
    );

    let mut varied = parts(LINUX);
    varied.inventory = inventory_from(COMMIT, json!([{"name": "fictional-tool"}]));
    let mixed_inventory = rebuilt_with_linux(rebuilt_from(varied, COMMIT));
    assert!(
        rejection(assemble_candidate_index(&mixed_inventory)).contains("different JS inventories"),
        "should reject rebuilt entries naming different inventories"
    );
}

// ---- Export and index writers ----

#[test]
fn export_writes_exactly_the_six_declared_members() {
    let scratch = tempfile::tempdir().expect("should create a scratch directory");
    let upload = scratch.path().join("candidate-entry");
    let transfer = scratch.path().join("candidate-entry-transfer.json");

    entry(MACOS_ARM)
        .export_flat_for_ordinary_ci(&artifact_bytes(MACOS_ARM), &upload, &transfer)
        .expect("should export the fictional entry");

    let written = read_directory(&upload);
    let description: Value =
        serde_json::from_slice(&std::fs::read(&transfer).expect("should read the transfer file"))
            .expect("should parse the transfer file");
    assert_eq!(
        written.keys().cloned().collect::<BTreeSet<_>>(),
        member_names(asset_id(MACOS_ARM)),
        "should write exactly the six flat members"
    );
    assert_eq!(
        description["members"],
        json!(sizes(&written)),
        "should declare the written member sizes"
    );
    assert_eq!(
        written,
        members(MACOS_ARM),
        "should write the exact member bytes"
    );
    assert_eq!(
        description["asset"], MACOS_ARM,
        "should name the exported asset"
    );
}

#[test]
fn export_refuses_existing_paths_nested_transfer_or_other_executable_bytes() {
    let scratch = tempfile::tempdir().expect("should create a scratch directory");
    let existing = scratch.path().join("existing");
    std::fs::create_dir(&existing).expect("should create the existing directory");
    let transfer = scratch.path().join("transfer.json");
    let linux = entry(LINUX);
    let artifact = artifact_bytes(LINUX);

    assert!(
        rejection(linux.export_flat_for_ordinary_ci(&artifact, &existing, &transfer))
            .contains("cannot create new transfer directory"),
        "should refuse an existing upload directory"
    );

    let fresh = scratch.path().join("fresh");
    assert!(
        rejection(linux.export_flat_for_ordinary_ci(
            &artifact,
            &fresh,
            &fresh.join("transfer.json")
        ))
        .contains("outside the uploaded directory"),
        "should refuse a transfer file inside the upload"
    );

    assert!(
        rejection(linux.export_flat_for_ordinary_ci(b"fictional other bytes", &fresh, &transfer))
            .contains("differ from the entry's recorded artifact"),
        "should refuse executable bytes other than the recorded artifact"
    );
    assert!(
        !fresh.exists(),
        "should write nothing for a rejected export"
    );
}

#[test]
fn index_writer_writes_the_index_inventory_and_sixteen_companions_once() {
    let scratch = tempfile::tempdir().expect("should create a scratch directory");
    let root = scratch.path().join("candidate-index");
    let index = assembled();

    write_candidate_index(&index, &root).expect("should write the candidate index");

    let written = read_directory(&root);
    assert_eq!(written.len(), 18, "should write exactly eighteen files");
    assert_eq!(
        written.get(CANDIDATE_INDEX_NAME).map(Vec::as_slice),
        Some(index.bytes()),
        "should write the exact index bytes"
    );
    assert_eq!(
        written.get(INVENTORY_NAME).map(Vec::as_slice),
        Some(index.inventory()),
        "should write the exact inventory bytes"
    );
    for (name, bytes) in index.companions() {
        assert_eq!(
            written.get(name),
            Some(bytes),
            "should write the exact companion {name}"
        );
    }
    assert!(
        rejection(write_candidate_index(&index, &root))
            .contains("cannot create new transfer directory"),
        "should refuse an existing index directory"
    );
}
