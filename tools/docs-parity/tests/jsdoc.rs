use std::collections::BTreeMap;

use docs_parity::jsdoc::{fixture_specs, validate_fixture_inventory};

#[test]
fn fixture_matrix_covers_each_required_jsdoc_rule_surface() {
    let specs = fixture_specs();
    assert_eq!(specs.len(), 10, "fixture matrix should remain exact");

    let names = specs
        .iter()
        .map(|spec| (spec.file_name, spec.expected_rule))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        names,
        BTreeMap::from([
            ("alignment.ts", "jsdoc/check-alignment"),
            ("default-export.ts", "jsdoc/require-jsdoc"),
            ("exported-class.ts", "jsdoc/require-jsdoc"),
            ("exported-function.ts", "jsdoc/require-jsdoc"),
            ("exported-interface.ts", "jsdoc/require-jsdoc"),
            ("exported-type-alias.ts", "jsdoc/require-jsdoc"),
            ("exported-variable.ts", "jsdoc/require-jsdoc"),
            ("file-overview.ts", "jsdoc/require-file-overview"),
            ("re-export.ts", "jsdoc/require-jsdoc"),
            ("types.ts", "jsdoc/check-types"),
        ])
    );
}

#[test]
fn fixture_inventory_fails_on_missing_or_extra_files() {
    let complete = fixture_specs()
        .iter()
        .map(|spec| (spec.file_name.to_owned(), b"fixture\n".to_vec()))
        .collect::<BTreeMap<_, _>>();
    validate_fixture_inventory(&complete).expect("exact fixture inventory should pass");

    let mut missing = complete.clone();
    missing.remove("types.ts");
    assert!(
        validate_fixture_inventory(&missing).is_err(),
        "missing fixture should fail"
    );

    let mut extra = complete;
    extra.insert("extra.ts".to_owned(), b"fixture\n".to_vec());
    assert!(
        validate_fixture_inventory(&extra).is_err(),
        "extra fixture should fail"
    );
}
