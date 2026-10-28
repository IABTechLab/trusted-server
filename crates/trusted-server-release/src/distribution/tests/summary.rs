//! Pure tests of the test-only entry re-check and its measurement shape.

use super::*;

/// A fictional entry whose companion with `role` carries `bytes`, re-recorded
/// so only the joins, not the digest records, can reject it.
fn with_companion(
    asset: &'static str,
    role: CompanionRole,
    bytes: Vec<u8>,
) -> CandidateArtifactEntry {
    let mut tampered = entry(asset);
    let file = tampered
        .companions
        .iter_mut()
        .find(|file| file.role == role)
        .expect("should carry the companion");
    *file = CompanionFile::new(role, asset, bytes);
    tampered
}

#[test]
fn fastly_entry_summary_records_matched_outcome_and_recomputed_companions() {
    let fastly = entry(FASTLY);

    let summary = fastly
        .recheck_for_measurement()
        .expect("should recheck the fictional Fastly entry");
    let measurement = summary.measurement();

    assert_eq!(
        summary.rebuild_outcome(),
        "matched",
        "should record the checked equality"
    );
    assert_eq!(
        summary.inventory_sha256(),
        sha256_hex(&inventory()),
        "should name the exact inventory bytes"
    );
    let keys: Vec<&String> = measurement
        .as_object()
        .expect("should render an object")
        .keys()
        .collect();
    assert_eq!(
        keys,
        [
            "artifact",
            "artifact_schema",
            "asset",
            "companions",
            "entry",
            "gaps",
            "js_inventory_sha256",
            "kind",
            "rebuild_check_outcome",
            "release_qualification",
        ],
        "should render exactly the measurement fields"
    );
    assert_eq!(measurement["asset"], FASTLY, "should name the asset");
    assert_eq!(
        measurement["release_qualification"],
        json!(false),
        "should grant no qualification"
    );
    assert_eq!(
        measurement["gaps"],
        json!(fastly.gaps()),
        "should record the entry gaps"
    );
    for file in &fastly.companions {
        assert_eq!(
            measurement["companions"][file.role.key()],
            json!({"name": file.name, "size": file.bytes.len(), "sha256": sha256_hex(&file.bytes)}),
            "should record the {} identity recomputed from bytes",
            file.role.key()
        );
    }
    let rendered = canonical_json(
        &fastly.index_entry(&sha256_hex(&inventory())),
        DocumentRole::Metadata,
    )
    .expect("should render the artifact entry");
    assert_eq!(
        measurement["entry"],
        json!({"sha256": sha256_hex(&rendered), "size": rendered.len()}),
        "should record the rendered artifact entry identity"
    );
}

#[test]
fn native_entry_summary_records_unavailable_outcome_and_reproduction_gap() {
    let summary = entry(LINUX)
        .recheck_for_measurement()
        .expect("should recheck the fictional Linux entry");

    assert_eq!(
        summary.rebuild_outcome(),
        "unavailable",
        "should record no native comparison"
    );
    assert!(
        summary.measurement()["gaps"]
            .as_array()
            .expect("should list gaps")
            .contains(&json!("native-reproduction-unavailable")),
        "should state the native reproduction gap"
    );
}

#[test]
fn recheck_rejects_companion_bytes_differing_from_their_record() {
    let mut tampered = entry(LINUX);
    tampered.companions[1].bytes.push(b' ');

    let error = rejection(tampered.recheck_for_measurement());

    assert!(
        error.contains("differ from its recorded size or digest"),
        "should reject the stale record: {error}"
    );
}

#[test]
fn recheck_reruns_joins_on_re_recorded_companions() {
    let evidence = companion(&entry(FASTLY), CompanionRole::BuildEvidence);
    let mut wrong_inventory = evidence.clone();
    wrong_inventory["js_inventory_sha256"] = json!(digest("another inventory"));
    let bytes = canonical_json(&wrong_inventory, DocumentRole::Companion)
        .expect("should render the tampered evidence");

    let error = rejection(
        with_companion(FASTLY, CompanionRole::BuildEvidence, bytes).recheck_for_measurement(),
    );

    assert!(
        error.contains("`js_inventory_sha256` differs from the index entry"),
        "should rerun the build evidence join: {error}"
    );
}

#[test]
fn recheck_rejects_a_companion_stating_other_gaps() {
    let mut recipe = companion(&entry(LINUX), CompanionRole::BuildRecipe);
    recipe["gaps"] = json!(["fictional-other-gap"]);
    let bytes = canonical_json(&recipe, DocumentRole::Companion).expect("should render the recipe");

    let error = rejection(
        with_companion(LINUX, CompanionRole::BuildRecipe, bytes).recheck_for_measurement(),
    );

    assert!(
        error.contains(
            "build_recipe companion must be unqualified and state exactly the entry gaps"
        ),
        "should reject inconsistent gaps: {error}"
    );
}
