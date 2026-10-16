use sha2::{Digest as _, Sha256};

use super::super::ambient::AmbientClaim;
use super::super::compare::{
    FASTLY_ASSET, check_fastly_equality, describe_mismatch, first_difference,
};
use super::super::completed::SuccessFacts;
use super::complete::{Built, built, built_with, wasm_with};
use super::fixture::OTHER_COMMIT;
use super::*;

/// A minimal Wasm module that keeps the debug sections a build must retain.
fn module(info: &[u8]) -> Vec<u8> {
    wasm_with(&[(".debug_info", info), (".debug_line", b"line")])
}

/// A completed fictional build with the fixture roots its output lives in.
struct Completed {
    _built: Built,
    completed: RecipeCompleted,
}

impl std::ops::Deref for Completed {
    type Target = RecipeCompleted;

    fn deref(&self) -> &RecipeCompleted {
        &self.completed
    }
}

/// Completes a fictional build of `asset` for `role` with exactly `bytes`.
fn completed(asset: &'static str, role: Role, bytes: &[u8], derived: bool) -> Completed {
    finish(built_with(asset, role, bytes, derived))
}

fn finish(mut built: Built) -> Completed {
    let stdout = built.stdout(|_| {});
    let completed = built.finish(stdout).expect("should complete the build");
    Completed {
        _built: built,
        completed,
    }
}

fn rejection(a: &RecipeCompleted, b: &RecipeCompleted) -> String {
    match check_fastly_equality(a, b) {
        Ok(_) => panic!("should reject the pair"),
        Err(error) => format!("{error:?}"),
    }
}

#[test]
fn identical_a_and_b_fastly_bytes_are_accepted_with_the_standard_claim() {
    let bytes = module(b"info");
    let a = completed(FASTLY_ASSET, Role::A, &bytes, false);
    let b = completed(FASTLY_ASSET, Role::B, &bytes, false);
    let checked = check_fastly_equality(&a, &b).expect("should accept identical final bytes");
    assert_eq!(
        checked.asset().artifact().name,
        FASTLY_ASSET,
        "should name Fastly"
    );
    assert_eq!(
        checked.sha256(),
        hex::encode(Sha256::digest(&bytes)),
        "should report the shared digest"
    );
    assert_eq!(checked.size(), bytes.len() as u64, "should report the size");
    assert_eq!(
        checked.ambient().claim(),
        &AmbientClaim::standard(),
        "should bind the honest ambient placement"
    );

    let measurement = super::candidate::equality_measurement(&a, &b, &checked);
    assert_eq!(
        (
            &measurement["kind"],
            &measurement["equality"],
            &measurement["release_qualification"],
        ),
        (
            &serde_json::json!("ordinary-ci-fastly-ab-equality"),
            &serde_json::json!(true),
            &serde_json::json!(false),
        ),
        "should report candidate equality without release qualification"
    );
    for (role, umask, jobs) in [("A", "022", 4), ("B", "027", 2)] {
        let build = &measurement["builds"][role];
        assert_eq!(
            build["artifact"]["sha256"],
            checked.sha256(),
            "should report the {role} digest"
        );
        assert_eq!(
            (&build["ambient"]["umask"], &build["ambient"]["jobs"]),
            (&serde_json::json!(umask), &serde_json::json!(jobs)),
            "should report the {role} ambient record"
        );
        assert_eq!(
            (
                &build["ambient"]["child_locale"],
                &build["ambient"]["child_timezone"]
            ),
            (&serde_json::json!("C"), &serde_json::json!("UTC")),
            "should report the normalized {role} child locale and timezone"
        );
    }
    assert_eq!(
        measurement["ambient_claim"]["unsupported"],
        serde_json::json!(["ExecutionUser"]),
        "should not claim the execution user varied"
    );
}

#[test]
fn one_byte_difference_reports_sizes_digests_and_first_offset() {
    let a = completed(FASTLY_ASSET, Role::A, &module(b"info"), false);
    let b = completed(FASTLY_ASSET, Role::B, &module(b"inFo"), false);
    let message = rejection(&a, &b);
    let offset =
        first_difference(&module(b"info"), &module(b"inFo")).expect("should find the changed byte");
    for part in [
        "final Fastly bytes differ".to_owned(),
        format!("first differing offset {offset}"),
        format!("A size {}", module(b"info").len()),
        a.artifact().sha256().to_owned(),
        b.artifact().sha256().to_owned(),
        "custom section \".debug_info\"".to_owned(),
        "1 differing bytes".to_owned(),
    ] {
        assert!(message.contains(&part), "should report {part}: {message}");
    }
}

#[test]
fn first_difference_covers_prefixes_and_equal_inputs() {
    assert_eq!(first_difference(b"abc", b"abc"), None, "should find none");
    assert_eq!(
        first_difference(b"abc", b"abd"),
        Some(2),
        "should find byte 2"
    );
    assert_eq!(
        first_difference(b"ab", b"abc"),
        Some(2),
        "should report the shorter length for a strict prefix"
    );
    let message = describe_mismatch(b"ab", b"abc");
    assert!(
        message.contains("A size 2") && message.contains("B size 3"),
        "should report both sizes: {message}"
    );
    assert!(
        message.contains("1 differing bytes") && message.contains("unparsed module"),
        "should count the extra byte and survive a non-module: {message}"
    );
    let long = vec![0_u8; 4096];
    let mut changed = long.clone();
    changed[100] = 1;
    assert!(
        describe_mismatch(&long, &changed).len() < 600,
        "should bound the diagnostic window"
    );
}

#[test]
fn same_role_or_swapped_pairs_are_rejected() {
    let bytes = module(b"info");
    let a = completed(FASTLY_ASSET, Role::A, &bytes, false);
    let other_a = completed(FASTLY_ASSET, Role::A, &bytes, false);
    let b = completed(FASTLY_ASSET, Role::B, &bytes, false);
    for (first, second) in [(&a, &other_a), (&b, &a)] {
        assert!(
            rejection(first, second).contains("role A then a role B"),
            "should require exactly role A then role B"
        );
    }
}

#[test]
fn mismatched_inputs_or_another_asset_are_rejected() {
    let bytes = module(b"info");
    let a = completed(FASTLY_ASSET, Role::A, &bytes, false);
    let derived = completed(FASTLY_ASSET, Role::B, &bytes, true);
    assert!(
        rejection(&a, &derived).contains("derived Git manifests"),
        "should reject builds over different derived manifests"
    );
    let native = finish(built(
        "ts-aarch64-apple-darwin",
        Role::A,
        b"fictional executable",
    ));
    let b = completed(FASTLY_ASSET, Role::B, &bytes, false);
    assert!(
        rejection(&native, &b).contains("compares only"),
        "should compare only the Fastly asset"
    );
}

/// One forged input of role B and the label its rejection must name.
type Forge = (&'static str, fn(&mut SuccessFacts));

#[test]
fn each_mismatched_input_is_rejected_by_name() {
    let bytes = module(b"info");
    let a = completed(FASTLY_ASSET, Role::A, &bytes, false);
    let cases: [Forge; 8] = [
        ("source commits", |facts| {
            facts.commit = CommitId::parse(OTHER_COMMIT).expect("should parse commit");
        }),
        ("Cargo.lock files", |facts| {
            facts.cargo_lock_sha256 = "e".repeat(64);
        }),
        ("vendored materials", |facts| {
            facts.materials_digest = "e".repeat(64);
        }),
        ("JS handoffs", |facts| {
            facts.js_inventory_sha256 = "e".repeat(64);
        }),
        ("JS handoffs", |facts| {
            facts.js_settings_sha256 = "e".repeat(64)
        }),
        ("JS handoffs", |facts| {
            facts.js_tools_sha256 = "e".repeat(64)
        }),
        ("Rust tools", |facts| {
            facts.rust_tools_digest = "e".repeat(64);
        }),
        ("compares only", |facts| {
            facts.asset = AssetId::parse("ts-aarch64-apple-darwin").expect("should resolve");
        }),
    ];
    for (label, forge) in cases {
        let mut b = completed(FASTLY_ASSET, Role::B, &bytes, false);
        forge(b.completed.facts_mut_for_tests());
        let message = rejection(&a, &b);
        assert!(
            message.contains(label),
            "should reject a B build with mismatched {label}: {message}"
        );
    }
}

#[test]
fn artifact_changed_after_completion_is_rejected() {
    let bytes = module(b"info");
    let a = completed(FASTLY_ASSET, Role::A, &bytes, false);
    let b = completed(FASTLY_ASSET, Role::B, &bytes, false);
    std::fs::write(b.artifact().path(), module(b"inFo")).expect("should tamper with B");
    assert!(
        rejection(&a, &b).contains("artifact bytes changed"),
        "should rehash both artifacts before comparing"
    );
}
