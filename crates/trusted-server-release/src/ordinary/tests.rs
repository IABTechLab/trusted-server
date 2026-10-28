use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use std::path::Path;

use super::{Observation, compare, observation, pair};

const WASM: &[u8] = b"\0asm\x01\0\0\0";

pub(super) fn fixture() -> Value {
    json!({
        "version": 1,
        "profile": "ordinary-release-fastly-byte-equality-v1",
        "release_qualification": false,
        "source_commit": "a".repeat(40),
        "asset": "trusted-server-fastly.wasm",
        "target": "wasm32-wasip1",
        "variant": "default",
        "role": "canonical",
        "recipe_sha256": "b".repeat(64),
        "cargo_lock_sha256": "c".repeat(64),
        "npm_lock_sha256": "d".repeat(64),
        "js_inventory_sha256": "e".repeat(64),
        "rustflags_sha256": super::normalization::plan(Path::new("/example/workspace/ordinary-canonical-source"), Path::new("/example/temp/ordinary-fastly-canonical-target"), "canonical").expect("should derive actual argument identity").sha256,
        "tools": {
            "rustc": {"version": "rustc 1.98.1 (example)", "sha256": "f".repeat(64)},
            "node": {"version": "v24.20.0", "sha256": "1".repeat(64)},
            "acquisition": "ordinary-hosted-toolchain"
        },
        "environment": {
            "checkout_root": "/example/workspace/ordinary-canonical-source",
            "target_root": "/example/temp/ordinary-fastly-canonical-target",
            "started_at_seconds": 1700000000,
            "umask": 18,
            "jobs": 4,
            "locale": "C",
            "timezone": "UTC",
            "uid": 1001
        },
        "artifact": {"size": WASM.len(), "sha256": hex::encode(Sha256::digest(WASM))},
        "gaps": [
            "alternate-user-unqualified",
            "boundary-unqualified",
            "materials-unqualified",
            "tools-unqualified"
        ]
    })
}

fn parse(value: &Value) -> Observation {
    observation(&serde_json::to_vec(value).expect("should encode fixture"))
        .expect("should validate the observation fixture")
}

pub(super) fn reproduction() -> Value {
    let mut value = fixture();
    value["role"] = json!("reproduction");
    value["environment"]["checkout_root"] =
        json!("/example/workspace/ordinary-reproduction-source");
    value["environment"]["target_root"] =
        json!("/example/temp/ordinary-fastly-reproduction-target");
    value["environment"]["started_at_seconds"] = json!(1700000900);
    value["environment"]["umask"] = json!(23);
    value["environment"]["jobs"] = json!(2);
    value
}

#[test]
fn accepts_matching_inputs_with_observed_environment_variation() {
    pair(&parse(&fixture()), &parse(&reproduction()))
        .expect("should accept matching ordinary inputs with measured variation");
}

#[test]
fn rejects_unbound_raw_arguments_or_different_derived_sibling_maps() {
    let mut value = reproduction();
    value["rustflags_sha256"] = json!("9".repeat(64));
    assert!(
        pair(&parse(&fixture()), &parse(&value)).is_err(),
        "should reject arguments not bound to the stated physical roots"
    );
    let mut value = reproduction();
    value["environment"]["checkout_root"] = json!("/other/workspace/ordinary-reproduction-source");
    let flags = super::normalization::plan(
        Path::new(
            value["environment"]["checkout_root"]
                .as_str()
                .expect("should name source"),
        ),
        Path::new(
            value["environment"]["target_root"]
                .as_str()
                .expect("should name target"),
        ),
        "reproduction",
    )
    .expect("should derive individually valid different maps");
    value["rustflags_sha256"] = json!(flags.sha256);
    assert!(
        pair(&parse(&fixture()), &parse(&value)).is_err(),
        "should reject individually valid but unequal raw arguments"
    );
}

#[test]
fn compares_complete_final_bytes_for_an_ordinary_pair() {
    let matched = compare(parse(&fixture()), parse(&reproduction()), WASM, WASM)
        .expect("should compare the complete identical ordinary final bytes");
    assert_eq!(
        matched.sha256(),
        hex::encode(Sha256::digest(WASM)),
        "should retain the actual compared digest"
    );
    assert_eq!(
        matched.size(),
        8,
        "should retain the complete compared size"
    );
}

fn with_bytes(mut value: Value, bytes: &[u8]) -> Observation {
    value["artifact"] = json!({
        "size": bytes.len(),
        "sha256": hex::encode(Sha256::digest(bytes)),
    });
    parse(&value)
}

#[test]
fn rejects_custom_section_differences_with_bounded_exact_diagnostics() {
    let left = [WASM, b"\0\x03\x01x\0"].concat();
    let right = [WASM, b"\0\x03\x01x\x01"].concat();
    let error = compare(
        with_bytes(fixture(), &left),
        with_bytes(reproduction(), &right),
        &left,
        &right,
    )
    .err()
    .expect("should reject changed custom-section bytes without exclusions");
    // The owned mismatch evidence is bounded independently of optional backtraces.
    let diagnostic = error
        .downcast_ref::<String>()
        .expect("should retain bounded mismatch evidence");
    assert!(
        diagnostic.len() < 2048
            && diagnostic.contains("final-byte mismatch")
            && diagnostic.contains("first_offset=12")
            && diagnostic.contains(&hex::encode(Sha256::digest(&left)))
            && diagnostic.contains(&hex::encode(Sha256::digest(&right))),
        "should retain bounded exact-byte mismatch evidence"
    );
}

#[test]
fn rejects_final_bytes_changed_since_the_observation() {
    let changed = [WASM, &[0]].concat();
    let error = compare(parse(&fixture()), parse(&reproduction()), WASM, &changed)
        .err()
        .expect("should reject mutation after measurement");
    assert!(
        format!("{error:?}").contains("executable changed after observation"),
        "should fail the retained measurement binding before comparison"
    );
}

#[test]
fn rejects_size_only_differences_without_a_prefix_match_fallback() {
    let longer = [WASM, b"\0\x02\x01x"].concat();
    let error = compare(
        with_bytes(fixture(), WASM),
        with_bytes(reproduction(), &longer),
        WASM,
        &longer,
    )
    .err()
    .expect("should reject equal prefix with a longer final file");
    assert!(
        format!("{error:?}").contains("first_offset=8"),
        "should identify the exact end-of-file difference"
    );
}

#[test]
fn rejects_equal_artifact_claims_over_different_build_inputs() {
    for field in [
        "source_commit",
        "recipe_sha256",
        "cargo_lock_sha256",
        "npm_lock_sha256",
        "js_inventory_sha256",
    ] {
        let mut changed = reproduction();
        changed[field] = json!("3".repeat(if field == "source_commit" { 40 } else { 64 }));
        let error = pair(&parse(&fixture()), &parse(&changed))
            .expect_err("should reject changed input identities even with equal artifact claims");
        assert!(
            format!("{error:?}").contains("build inputs differ"),
            "should reach the input identity join for {field}"
        );
    }
    for tool in ["rustc", "node"] {
        let mut changed = reproduction();
        changed["tools"][tool]["sha256"] = json!("3".repeat(64));
        assert!(
            pair(&parse(&fixture()), &parse(&changed)).is_err(),
            "should reject a changed tool identity"
        );
    }
}

#[test]
fn rejects_false_environment_variation_and_swapped_roles() {
    for (field, value) in [
        ("started_at_seconds", json!(1700000000)),
        ("umask", json!(18)),
        ("jobs", json!(4)),
        ("uid", json!(2002)),
    ] {
        let mut changed = reproduction();
        changed["environment"][field] = value;
        let error = pair(&parse(&fixture()), &parse(&changed))
            .expect_err("should reject false or unsupported variation");
        assert!(
            format!("{error:?}").contains("environment pairing rejected"),
            "should reach the pairing rule for {field}"
        );
    }
    assert!(
        pair(&parse(&reproduction()), &parse(&fixture())).is_err(),
        "should reject reversed canonical/reproduction roles"
    );
}

#[test]
fn rejects_overlapping_cross_build_roots() {
    let mut changed = reproduction();
    changed["environment"]["checkout_root"] =
        json!("/example/workspace/ordinary-canonical-source/child");
    let error = pair(&parse(&fixture()), &parse(&changed))
        .expect_err("should reject overlapping roots instead of treating spelling as independence");
    assert!(
        format!("{error:?}").contains("build roots overlap"),
        "should reach the cross-build root check"
    );
}

#[test]
fn rejects_invalid_digest_bounds_and_missing_assurance_gaps() {
    for (field, value) in [
        ("source_commit", json!("A".repeat(40))),
        ("recipe_sha256", json!("b".repeat(63))),
        ("gaps", json!([])),
        ("profile", json!("controlled-recipe-qualified")),
    ] {
        let mut input = fixture();
        input[field] = value;
        assert!(
            observation(&serde_json::to_vec(&input).expect("should encode fixture")).is_err(),
            "should reject invalid {field}"
        );
    }
    for size in [0, 512 * 1024 * 1024 + 1] {
        let mut input = fixture();
        input["artifact"]["size"] = json!(size);
        assert!(
            observation(&serde_json::to_vec(&input).expect("should encode fixture")).is_err(),
            "should reject invalid artifact size"
        );
    }
}

#[test]
fn accepts_observed_ordinary_inputs_without_qualification() {
    let input = fixture();
    let accepted = observation(&serde_json::to_vec(&input).expect("should encode fixture"))
        .expect("should accept the complete ordinary observation");
    assert_eq!(
        accepted.value(),
        &input,
        "should retain observed facts exactly"
    );
}

#[test]
fn rejects_forged_qualification_and_unknown_observation_fields() {
    let mut qualified = fixture();
    qualified["release_qualification"] = json!(true);
    assert!(
        observation(&serde_json::to_vec(&qualified).expect("should encode fixture")).is_err(),
        "should reject a qualification claim"
    );
    let mut extra = fixture();
    extra["approved_source"] = json!(true);
    assert!(
        observation(&serde_json::to_vec(&extra).expect("should encode fixture")).is_err(),
        "should reject an unapproved observation field"
    );
}

#[test]
fn rejects_environment_root_spelling_aliases() {
    for path in [
        "/example/canonical/source/",
        "/example//canonical/source",
        "/example/./canonical/source",
        "/example/other/../canonical/source",
    ] {
        let mut input = fixture();
        input["environment"]["checkout_root"] = json!(path);
        assert!(
            observation(&serde_json::to_vec(&input).expect("should encode fixture")).is_err(),
            "should reject a root spelling alias: {path}"
        );
    }
}

#[test]
fn invalid_observation_diagnostics_do_not_echo_arbitrary_field_names() {
    let mut input = fixture();
    input["unexpected_observation_key".repeat(2048)] = json!(true);
    let error = observation(&serde_json::to_vec(&input).expect("should encode fixture"))
        .err()
        .expect("should reject the unapproved field");
    // Caller-enabled backtraces are separate from the fixed sanitized payload.
    let diagnostic = error
        .downcast_ref::<&str>()
        .expect("should retain the fixed diagnostic");
    assert_eq!(
        *diagnostic, "ordinary observation fields or types rejected",
        "should discard the original parser diagnostic"
    );
    assert!(
        diagnostic.len() < 2048 && !format!("{error:?}").contains("unexpected_observation_key"),
        "should keep diagnostics bounded without echoing untrusted field names"
    );
}

#[test]
fn typed_failure_stages_distinguish_identity_size_digest_and_complete_bytes() {
    for (stage, code) in [
        ("identity", "identity_mismatch"),
        ("size", "observed_size_mismatch"),
        ("digest", "observed_digest_mismatch"),
        ("bytes", "final_bytes_mismatch"),
    ] {
        let mut right = reproduction();
        let different = [WASM, b"\0\x02\x01x"].concat();
        let bytes = if stage == "bytes" {
            different.as_slice()
        } else {
            WASM
        };
        match stage {
            "identity" => right["source_commit"] = json!("b".repeat(40)),
            "size" => right["artifact"]["size"] = json!(WASM.len() + 1),
            "digest" => right["artifact"]["sha256"] = json!("b".repeat(64)),
            "bytes" => {
                right["artifact"] =
                    json!({"size":bytes.len(),"sha256":hex::encode(Sha256::digest(bytes))})
            }
            _ => unreachable!("should use a fixed test case"),
        }
        let error = compare(parse(&fixture()), parse(&right), WASM, bytes)
            .err()
            .expect("should reject the selected actual failure boundary");
        let value = error
            .downcast_ref::<super::diagnostic::Failure>()
            .expect("should retain typed failure facts rather than parsing stderr")
            .value();
        assert_eq!(
            value["stage"], stage,
            "should identify actual rejecting stage"
        );
        assert_eq!(
            value["code"], code,
            "should retain fixed code for the stage"
        );
        if stage == "bytes" {
            assert_eq!(
                value["outcome"], "mismatched",
                "should recognize nondeterminism"
            );
            assert_eq!(
                value["facts"]["first_difference_offset"],
                WASM.len(),
                "should retain actual first difference"
            );
        }
    }
}
