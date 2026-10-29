use serde_json::json;
use sha2::Sha256;

use super::*;

const COHORT: &[u8] = include_bytes!("../../../../scripts/release/release-cohort.json");
const TOOLS: &[u8] = include_bytes!("../../../../scripts/release/release-tools.json");
const CLI_POLICY: &[u8] =
    include_bytes!("../../../../crates/trusted-server-cli/release-policy.json");

#[test]
fn pending_tool_versions_match_the_source_selected_toolchain() {
    let tools = value(TOOLS);
    let source = include_str!("../../../../.tool-versions");
    for (release_name, source_name) in [
        ("node", "nodejs"),
        ("rust", "rust"),
        ("fastly", "fastly"),
        ("viceroy", "viceroy"),
        ("wasmtime", "wasmtime"),
    ] {
        let version = source
            .lines()
            .find_map(|line| {
                let mut fields = line.split_whitespace();
                (fields.next() == Some(source_name))
                    .then(|| fields.next())
                    .flatten()
            })
            .expect("should contain the source tool version");
        for tool in tools["tools"].as_array().expect("should hold tool pins") {
            if tool["name"]
                .as_str()
                .is_some_and(|name| name.split(':').next() == Some(release_name))
            {
                assert_eq!(
                    tool["version"], version,
                    "should keep pending {release_name} pins aligned with source selection"
                );
            }
        }
    }
}

fn value(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes).expect("should parse tracked fixture input")
}

fn encode(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).expect("should encode mutated fixture input")
}

fn report_text(report: &Report<ReleaseError>) -> String {
    format!("{report:?}")
}

fn cohort_with(mutate: impl FnOnce(&mut Value)) -> Result<ReleaseInputs, Report<ReleaseError>> {
    let mut cohort = value(COHORT);
    mutate(&mut cohort);
    check_inputs(&encode(&cohort), TOOLS, CLI_POLICY)
}

fn tools_with(mutate: impl FnOnce(&mut Value)) -> Result<ReleaseInputs, Report<ReleaseError>> {
    let mut tools = value(TOOLS);
    mutate(&mut tools);
    check_inputs(COHORT, &encode(&tools), CLI_POLICY)
}

fn policy_with(mutate: impl FnOnce(&mut Value)) -> Result<ReleaseInputs, Report<ReleaseError>> {
    let mut policy = value(CLI_POLICY);
    mutate(&mut policy);
    check_inputs(COHORT, TOOLS, &encode(&policy))
}

fn artifact_index(document: &Value, name: &str) -> usize {
    document["artifacts"]
        .as_array()
        .expect("should contain artifacts")
        .iter()
        .position(|artifact| artifact["name"] == name)
        .expect("should contain named artifact")
}

fn expect_rejected(result: Result<ReleaseInputs, Report<ReleaseError>>, needle: &str) {
    let report = result.err().expect("should reject mutated release input");
    let text = report_text(&report);
    assert!(
        text.contains(needle),
        "should explain rejection with `{needle}`, got: {text}"
    );
}

#[test]
fn accepts_tracked_inputs_and_preserves_raw_digests() {
    let inputs = check_inputs(COHORT, TOOLS, CLI_POLICY).expect("should accept tracked inputs");
    let digests = inputs
        .input_digests()
        .map(|(path, sha256)| (path, sha256.to_owned()));
    assert_eq!(
        digests,
        [
            (COHORT_PATH, hex::encode(Sha256::digest(COHORT))),
            (TOOLS_PATH, hex::encode(Sha256::digest(TOOLS))),
            (CLI_POLICY_PATH, hex::encode(Sha256::digest(CLI_POLICY))),
        ],
        "should preserve exact raw-byte digests of each input"
    );
}

#[test]
fn compiled_contract_maps_exactly_four_fixed_assets() {
    let names: Vec<_> = FIXED_ARTIFACTS.iter().map(|fixed| fixed.name).collect();
    assert_eq!(
        names,
        [
            "trusted-server-fastly.wasm",
            "ts-aarch64-apple-darwin",
            "ts-x86_64-apple-darwin",
            "ts-x86_64-unknown-linux-gnu",
        ],
        "should retain exactly the four controlled destination identities"
    );
    let fastly = &FIXED_ARTIFACTS[0];
    assert_eq!(fastly.package, "trusted-server-adapter-fastly");
    assert_eq!(fastly.bin, "trusted-server-adapter-fastly");
    assert_eq!(fastly.target, ReleaseTarget::Wasm32Wasip1);
    assert_eq!(fastly.assurance_profile, "provenance-sbom-repeatable");
    for cli in &FIXED_ARTIFACTS[1..] {
        assert_eq!(cli.package, "trusted-server-cli", "should build the CLI");
        assert_eq!(cli.bin, "ts", "should build the ts binary");
        assert_eq!(cli.assurance_profile, "provenance-sbom");
    }
    assert_eq!(ELIGIBLE_BRANCHES, ["main"], "should only allow main");
}

#[test]
fn ordinary_seven_asset_cohort_does_not_select_controlled_axum_recipes() {
    check_inputs(COHORT, TOOLS, CLI_POLICY).expect("should validate all seven ordinary asset rows");
    let cohort = value(COHORT);
    let rows = cohort["artifacts"]
        .as_array()
        .expect("should list cohort rows");
    assert_eq!(rows.len(), 7, "should validate the actual ordinary cohort");
    for asset in rows.iter().skip(4) {
        let name = asset["name"].as_str().expect("should name the Axum asset");
        assert!(
            crate::recipe::AssetId::parse(name).is_err(),
            "should not grant controlled recipe selection for {name}"
        );
    }
    assert_eq!(
        value(CLI_POLICY)["artifacts"]
            .as_array()
            .expect("should list controlled consumer rows")
            .len(),
        4,
        "should preserve the separately gated index destination contract"
    );
}

#[test]
fn ordinary_axum_rows_reject_every_changed_identity_and_missing_row() {
    for index in 4..7 {
        for field in [
            "package",
            "bin",
            "target",
            "role",
            "kind",
            "assurance_profile",
            "variant",
        ] {
            let mut cohort = value(COHORT);
            cohort["artifacts"][index][field] = json!("example-invalid");
            assert!(
                check_inputs(&encode(&cohort), TOOLS, CLI_POLICY).is_err(),
                "should reject Axum identity change {index}/{field}"
            );
        }
        for field in ["architecture", "minimum_version", "runtime"] {
            let mut cohort = value(COHORT);
            cohort["artifacts"][index]["compatibility"][field] = json!("example-invalid");
            assert!(
                check_inputs(&encode(&cohort), TOOLS, CLI_POLICY).is_err(),
                "should reject Axum compatibility change {index}/{field}"
            );
        }
        let mut cohort = value(COHORT);
        cohort["artifacts"]
            .as_array_mut()
            .expect("should list cohort rows")
            .remove(index);
        assert!(
            check_inputs(&encode(&cohort), TOOLS, CLI_POLICY).is_err(),
            "should require Axum row {index}"
        );
    }
}

#[test]
fn rejects_missing_artifact() {
    expect_rejected(
        cohort_with(|cohort| {
            let index = artifact_index(cohort, "ts-x86_64-apple-darwin");
            cohort["artifacts"]
                .as_array_mut()
                .expect("should contain artifacts")
                .remove(index);
        }),
        "missing asset ts-x86_64-apple-darwin",
    );
}

#[test]
fn rejects_extra_artifact() {
    expect_rejected(
        cohort_with(|cohort| {
            let mut extra = cohort["artifacts"][0].clone();
            extra["name"] = json!("trusted-server-fastly-reuse.wasm");
            cohort["artifacts"]
                .as_array_mut()
                .expect("should contain artifacts")
                .push(extra);
        }),
        "unknown asset trusted-server-fastly-reuse.wasm",
    );
}

#[test]
fn rejects_duplicate_artifact() {
    expect_rejected(
        cohort_with(|cohort| {
            let duplicate = cohort["artifacts"][0].clone();
            cohort["artifacts"]
                .as_array_mut()
                .expect("should contain artifacts")
                .push(duplicate);
        }),
        "duplicate asset",
    );
}

#[test]
fn rejects_profile_downgraded_after_failure() {
    expect_rejected(
        cohort_with(|cohort| {
            let index = artifact_index(cohort, "trusted-server-fastly.wasm");
            cohort["artifacts"][index]["assurance_profile"] = json!("provenance-sbom");
        }),
        "assurance_profile",
    );
}

#[test]
fn rejects_unknown_target() {
    expect_rejected(
        cohort_with(|cohort| {
            let index = artifact_index(cohort, "ts-x86_64-unknown-linux-gnu");
            cohort["artifacts"][index]["target"] = json!("example-unknown-linux-gnu");
        }),
        "unknown target example-unknown-linux-gnu",
    );
}

#[test]
fn rejects_reassigned_known_target() {
    expect_rejected(
        cohort_with(|cohort| {
            let index = artifact_index(cohort, "ts-x86_64-apple-darwin");
            cohort["artifacts"][index]["target"] = json!("aarch64-apple-darwin");
        }),
        "target",
    );
}

#[test]
fn rejects_unknown_or_reuse_feature() {
    for feature in ["reusable-sandbox", "fictional-feature"] {
        expect_rejected(
            cohort_with(|cohort| {
                let index = artifact_index(cohort, "trusted-server-fastly.wasm");
                cohort["artifacts"][index]["features"] = json!([feature]);
            }),
            "unsupported features",
        );
    }
}

#[test]
fn rejects_disabled_default_features_and_changed_variant() {
    expect_rejected(
        cohort_with(|cohort| {
            cohort["artifacts"][0]["default_features"] = json!(false);
        }),
        "default_features",
    );
    expect_rejected(
        cohort_with(|cohort| {
            cohort["artifacts"][0]["variant"] = json!("reuse");
        }),
        "variant",
    );
}

#[test]
fn rejects_changed_package_bin_capability_and_compatibility() {
    for (field, replacement) in [
        ("package", json!("trusted-server-adapter-axum")),
        ("bin", json!("other")),
        ("capabilities", json!(["cli-execute"])),
        ("kind", json!("native-executable")),
        ("role", json!("operator-cli")),
    ] {
        expect_rejected(
            cohort_with(|cohort| {
                let index = artifact_index(cohort, "trusted-server-fastly.wasm");
                cohort["artifacts"][index][field] = replacement;
            }),
            field,
        );
    }
    expect_rejected(
        cohort_with(|cohort| {
            let index = artifact_index(cohort, "ts-x86_64-unknown-linux-gnu");
            cohort["artifacts"][index]["compatibility"]["minimum_version"] = json!("glibc-2.39");
        }),
        "compatibility",
    );
}

#[test]
fn rejects_candidate_cohort_listing_its_own_rc_branch() {
    for branches in [
        json!(["main", "rc/202610"]),
        json!(["rc/202610"]),
        json!([]),
    ] {
        expect_rejected(
            cohort_with(|cohort| {
                cohort["eligible_branches"] = branches;
            }),
            "eligible_branches",
        );
    }
}

#[test]
fn rejects_changed_cohort_identity_and_schema_version() {
    expect_rejected(
        cohort_with(|cohort| cohort["cohort"] = json!("phase2")),
        "cohort",
    );
    expect_rejected(
        cohort_with(|cohort| cohort["schema_version"] = json!(2)),
        "schema_version",
    );
}

#[test]
fn rejects_unknown_and_missing_cohort_fields() {
    assert!(
        cohort_with(|cohort| cohort["commands"] = json!(["cargo build"])).is_err(),
        "should reject JSON-supplied commands"
    );
    assert!(
        cohort_with(|cohort| cohort["artifacts"][0]["command"] = json!("sh -c true")).is_err(),
        "should reject artifact-level commands"
    );
    assert!(
        cohort_with(|cohort| {
            cohort["artifacts"][0]
                .as_object_mut()
                .expect("should be an artifact object")
                .remove("variant");
        })
        .is_err(),
        "should reject a missing artifact field"
    );
    assert!(
        cohort_with(|cohort| {
            cohort
                .as_object_mut()
                .expect("should be a cohort object")
                .remove("eligible_branches");
        })
        .is_err(),
        "should reject a missing cohort field"
    );
}

#[test]
fn rejects_duplicate_keys_through_strict_parser() {
    let duplicated = String::from_utf8(COHORT.to_vec())
        .expect("should be UTF-8")
        .replacen(
            "\"cohort\": \"phase1-fastly-cli\"",
            "\"cohort\": \"phase1-fastly-cli\", \"cohort\": \"x\"",
            1,
        );
    assert!(
        check_inputs(duplicated.as_bytes(), TOOLS, CLI_POLICY).is_err(),
        "should reject duplicate keys before semantic checks"
    );
}

#[test]
fn joins_cli_policy_artifacts_to_cohort_with_readonly_actions() {
    expect_rejected(
        policy_with(|policy| {
            policy["artifacts"][0]["actions"] = json!(["readonly-verify", "deploy"])
        }),
        "actions",
    );
    expect_rejected(
        policy_with(|policy| {
            policy["artifacts"][1]
                .as_object_mut()
                .expect("should be a policy artifact")
                .remove("actions");
        }),
        "actions",
    );
    expect_rejected(
        policy_with(|policy| policy["artifacts"][1]["assurance_profile"] = json!("provenance")),
        "release-policy",
    );
    expect_rejected(
        policy_with(|policy| {
            policy["artifacts"]
                .as_array_mut()
                .expect("should contain artifacts")
                .pop();
        }),
        "release-policy",
    );
    assert!(
        policy_with(|policy| policy["override_url"] = json!("https://example.com/policy.json"))
            .is_err(),
        "should reject unknown policy fields"
    );
    expect_rejected(
        policy_with(|policy| policy["tool_versions"]["gh"] = json!("2.96.0")),
        "tool_versions",
    );
    expect_rejected(
        policy_with(|policy| policy["cohort"] = json!("phase2")),
        "cohort",
    );
}

#[test]
fn preserves_exact_tool_versions_and_attest_revision() {
    expect_rejected(
        tools_with(|tools| {
            let tool = tools["tools"]
                .as_array_mut()
                .expect("should contain tools")
                .iter_mut()
                .find(|tool| tool["name"] == "gh:x86_64-unknown-linux-gnu")
                .expect("should contain gh");
            tool["version"] = json!("2.96.0");
        }),
        "gh:x86_64-unknown-linux-gnu",
    );
    expect_rejected(
        tools_with(|tools| {
            tools["tools"][0]["revision"] = json!("0".repeat(40));
        }),
        "actions/attest:github-action",
    );
    expect_rejected(
        tools_with(|tools| {
            tools["tools"]
                .as_array_mut()
                .expect("should contain tools")
                .retain(|tool| tool["name"] != "rust:x86_64-apple-darwin");
        }),
        "missing tool rust:x86_64-apple-darwin",
    );
    expect_rejected(
        tools_with(|tools| {
            let mut extra = tools["tools"][1].clone();
            extra["name"] = json!("fastly:aarch64-unknown-linux-gnu");
            tools["tools"]
                .as_array_mut()
                .expect("should contain tools")
                .push(extra);
        }),
        "unknown tool fastly:aarch64-unknown-linux-gnu",
    );
}

#[test]
fn rejects_absent_nullable_pin_fields_and_unknown_tool_fields() {
    assert!(
        tools_with(|tools| {
            tools["tools"][1]
                .as_object_mut()
                .expect("should be a tool object")
                .remove("sha256");
        })
        .is_err(),
        "should treat an absent digest as invalid rather than null"
    );
    assert!(
        tools_with(|tools| tools["tools"][1]["qualified"] = json!(true)).is_err(),
        "should reject serialized authority booleans"
    );
    assert!(
        tools_with(|tools| tools["environments"][0]["authenticated"] = json!(true)).is_err(),
        "should reject unknown environment fields"
    );
    assert!(
        tools_with(|tools| tools["tools"][1]["qualification"] = json!("approved")).is_err(),
        "should reject unknown qualification states"
    );
    assert!(
        tools_with(|tools| tools["tools"][1]["sha256"] = json!("not-a-digest")).is_err(),
        "should reject malformed digests"
    );
}

#[test]
fn rejects_recipe_script_and_environment_changes() {
    expect_rejected(
        tools_with(|tools| tools["recipes"][0]["script"] = json!("scripts/release/other.sh")),
        "script",
    );
    expect_rejected(
        tools_with(|tools| tools["recipes"][1]["environment_id"] = json!("gnu-builder-x86_64")),
        "environment_id",
    );
    expect_rejected(
        tools_with(|tools| tools["recipes"][0]["settings"]["aws_lc_use_system"] = json!(true)),
        "settings",
    );
    expect_rejected(
        tools_with(|tools| tools["recipes"][0]["settings"]["rust_toolchain"] = json!("1.96.0")),
        "settings",
    );
    expect_rejected(
        tools_with(|tools| {
            tools["environments"][1]["glibc_maximum"] = json!("2.39");
        }),
        "gnu-builder-x86_64",
    );
}

#[test]
fn production_selector_rejects_tracked_pending_pins() {
    let inputs = check_inputs(COHORT, TOOLS, CLI_POLICY).expect("should accept tracked inputs");
    let report = select_qualified(&inputs, &[])
        .err()
        .expect("should reject pending pins");
    let text = report_text(&report);
    assert!(text.contains("pending"), "should name pending pins: {text}");
}

fn qualified_looking_tools() -> Value {
    let digest = "ab".repeat(32);
    let mut tools = value(TOOLS);
    for section in ["environments", "recipes", "tools"] {
        for entry in tools[section]
            .as_array_mut()
            .expect("should contain entries")
        {
            let object = entry.as_object_mut().expect("should be an object");
            for (key, field) in object.iter_mut() {
                if key == "qualification" {
                    *field = json!("qualified");
                } else if field.is_null() && key == "revision" {
                    continue;
                } else if field.is_null() && key.ends_with("sha256") {
                    *field = json!(digest);
                } else if field.is_null() {
                    *field = json!("fictional.example.com/qualified");
                }
            }
        }
    }
    tools
}

#[test]
fn production_selector_rejects_qualified_looking_json_with_fake_digests() {
    let inputs = check_inputs(COHORT, &encode(&qualified_looking_tools()), CLI_POLICY)
        .expect("should parse structurally complete fictional pins");
    let report = select_qualified(&inputs, &[])
        .err()
        .expect("should refuse JSON qualification without authenticated receipts");
    let text = report_text(&report);
    assert!(
        text.contains("authenticated acquisition receipts")
            && text.contains("production selection remains closed"),
        "should explain the closed selector: {text}"
    );
}

#[test]
fn audit_lists_pending_items_without_authorization() {
    let inputs = check_inputs(COHORT, TOOLS, CLI_POLICY).expect("should accept tracked inputs");
    let audit = audit_inputs(&inputs);
    assert!(!audit.is_complete(), "should report incomplete inputs");
    let report = audit.to_json();
    let pending = report["pending"]
        .as_array()
        .expect("should list pending items");
    assert!(
        pending.contains(&json!(
            "tool gh:x86_64-unknown-linux-gnu: qualification pending"
        )),
        "should list pending tool qualification"
    );
    assert!(
        pending.contains(&json!("tool gh:x86_64-unknown-linux-gnu: sha256 is null")),
        "should list null tool digest"
    );
    assert!(
        pending.contains(&json!("environment macos15-aarch64: sdk_sha256 is null")),
        "should list null environment SDK digest"
    );
    assert_eq!(
        report["inputs"][0],
        json!({"path": COHORT_PATH, "sha256": hex::encode(Sha256::digest(COHORT))}),
        "should report raw input digests"
    );
    assert_eq!(report["status"], json!("incomplete"));
}

#[test]
fn audit_stays_incomplete_for_qualified_looking_json() {
    let inputs = check_inputs(COHORT, &encode(&qualified_looking_tools()), CLI_POLICY)
        .expect("should parse structurally complete fictional pins");
    let audit = audit_inputs(&inputs);
    assert!(
        audit.to_json()["pending"]
            .as_array()
            .expect("should list pending items")
            .is_empty(),
        "should find no pending JSON fields"
    );
    assert!(
        !audit.is_complete(),
        "should remain incomplete without authenticated receipts"
    );
}

fn recipe_index(tools: &Value, id: &str) -> usize {
    tools["recipes"]
        .as_array()
        .expect("should contain recipes")
        .iter()
        .position(|recipe| recipe["id"] == id)
        .expect("should contain recipe")
}

#[test]
fn requires_exact_recipe_settings() {
    let mutations: [(&str, &str, Value); 4] = [
        ("tsjs", "mandatory_tests", json!(false)),
        ("tsjs", "npm_lifecycle", json!("unrestricted")),
        (
            "trusted-server-fastly.wasm",
            "cargo_args",
            json!(["--offline"]),
        ),
        ("ts-aarch64-apple-darwin", "release_strip", json!("symbols")),
    ];
    for (id, key, replacement) in mutations {
        expect_rejected(
            tools_with(|tools| {
                let index = recipe_index(tools, id);
                tools["recipes"][index]["settings"][key] = replacement;
            }),
            "settings",
        );
    }
    expect_rejected(
        tools_with(|tools| {
            let index = recipe_index(tools, "ts-x86_64-unknown-linux-gnu");
            tools["recipes"][index]["settings"]["ambient_profiles"]["B"]["umask"] = json!("000");
        }),
        "settings",
    );
    expect_rejected(
        tools_with(|tools| {
            let index = recipe_index(tools, "tsjs");
            tools["recipes"][index]["settings"]
                .as_object_mut()
                .expect("should be settings")
                .remove("npm_offline");
        }),
        "settings",
    );
}

#[test]
fn requires_exact_consumer_policy_fields() {
    for (pointer, replacement) in [
        ("/repository_id", json!("1")),
        ("/repository_owner_id", json!("2")),
        ("/repository", json!("example/fictional")),
        ("/github_host", json!("github.example.com")),
        ("/identity/issuer", json!("https://issuer.example.com")),
        ("/identity/builder_id", json!("https://example.com/builder")),
        ("/predicates/sbom", json!("https://example.com/sbom")),
        ("/predicates/build_type", json!("https://example.com/build")),
        ("/bounds/child_seconds", json!(121)),
        (
            "/historical_producer_policies",
            json!([{"policy_sha256": "a".repeat(64)}]),
        ),
        ("/policy_version", json!(2)),
    ] {
        expect_rejected(
            policy_with(|policy| {
                *policy
                    .pointer_mut(pointer)
                    .expect("should address a tracked policy field") = replacement;
            }),
            "release-policy",
        );
    }
}

#[test]
fn audit_completion_stays_false_while_facts_are_not_established() {
    assert!(
        !NOT_ESTABLISHED.is_empty(),
        "should still list facts this tool cannot establish"
    );
    let inputs = check_inputs(COHORT, &encode(&qualified_looking_tools()), CLI_POLICY)
        .expect("should parse structurally complete fictional pins");
    assert!(
        !audit_inputs(&inputs).is_complete(),
        "should never report completion while any fact is not established"
    );
}
