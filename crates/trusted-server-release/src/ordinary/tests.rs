use serde_json::{Value, json};

use super::{Observation, observation, pair};

fn fixture() -> Value {
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
        "tools": {
            "rustc": {"version": "rustc 1.98.1 (example)", "sha256": "f".repeat(64)},
            "node": {"version": "v24.20.0", "sha256": "1".repeat(64)},
            "acquisition": "ordinary-hosted-toolchain"
        },
        "environment": {
            "checkout_root": "/example/canonical/source",
            "target_root": "/example/canonical/target",
            "started_at_seconds": 1700000000,
            "umask": 18,
            "jobs": 4,
            "locale": "C",
            "timezone": "UTC",
            "uid": 1001
        },
        "artifact": {"size": 8, "sha256": "2".repeat(64)},
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

fn reproduction() -> Value {
    let mut value = fixture();
    value["role"] = json!("reproduction");
    value["environment"]["checkout_root"] = json!("/example/reproduction/source");
    value["environment"]["target_root"] = json!("/example/reproduction/target");
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
    changed["environment"]["checkout_root"] = json!("/example/canonical/source/child");
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
    let diagnostic = format!("{error:?}");
    assert!(
        diagnostic.len() < 2048 && !diagnostic.contains("unexpected_observation_key"),
        "should keep diagnostics bounded without echoing untrusted field names"
    );
}
