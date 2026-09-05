use std::env;
use std::path::PathBuf;
use std::process::Command;

use docs_parity::settings::{
    CompanionManifest, CompanionReceipt, ContainerDefault, DefaultValue, KeyIdentity, Lifecycle,
    RuntimeDisposition, SecretDisposition, SerializationDisposition, extract_schema,
};

fn empty_companions() -> CompanionManifest {
    CompanionManifest::parse("version = 1\nreviewed = true\n")
        .expect("should parse empty companions")
}

#[test]
fn extracts_field_container_and_validation_semantics() {
    let source = r#"
        #[derive(serde::Deserialize, serde::Serialize)]
        #[serde(rename_all = "kebab-case", deny_unknown_fields)]
        struct Fixture {
            #[serde(rename = "public-name", alias = "legacy_name", default)]
            #[validate(range(min = 1, max = 9))]
            value_name: Option<u16>,
            #[serde(flatten)]
            extra: Extra,
            #[serde(skip)]
            hidden: String,
            #[serde(skip_serializing)]
            accepted_only: String,
        }
    "#;

    let schema = extract_schema("fixture.rs", source, &empty_companions())
        .expect("supported attributes should extract");
    let fixture = schema
        .type_named("Fixture")
        .expect("should extract Fixture");

    assert!(fixture.deny_unknown_fields);
    assert_eq!(fixture.rename_all.as_deref(), Some("kebab-case"));
    assert_eq!(fixture.fields.len(), 3, "serde(skip) should be omitted");
    let value = fixture
        .field_named("public-name")
        .expect("renamed field should be canonical");
    assert_eq!(value.rust_name, "value_name");
    assert_eq!(value.aliases, ["legacy_name"]);
    assert!(value.optional);
    assert_eq!(value.default, Some(DefaultValue::Trait));
    assert_eq!(value.range.as_ref().and_then(|range| range.min), Some(1));
    assert_eq!(value.range.as_ref().and_then(|range| range.max), Some(9));
    assert!(fixture.field_named("extra").expect("flatten field").flatten);
    assert!(
        fixture
            .field_named("accepted-only")
            .expect("rename_all should apply")
            .skip_serializing
    );
}

#[test]
fn extracts_variant_tag_content_untagged_and_rename_semantics() {
    let source = r#"
        #[derive(serde::Deserialize)]
        #[serde(tag = "kind", content = "payload", rename_all = "snake_case")]
        enum Tagged {
            FirstValue,
            #[serde(rename = "second", alias = "legacy_second")]
            SecondValue { enabled: bool },
        }

        #[derive(serde::Deserialize)]
        #[serde(untagged)]
        enum Untagged { Text(String), Number(u64) }
    "#;

    let schema = extract_schema("fixture.rs", source, &empty_companions())
        .expect("supported enum attributes should extract");
    let tagged = schema.type_named("Tagged").expect("should extract Tagged");
    assert_eq!(tagged.tag.as_deref(), Some("kind"));
    assert_eq!(tagged.content.as_deref(), Some("payload"));
    assert_eq!(
        tagged
            .variant_named("first_value")
            .expect("rename_all should apply")
            .rust_name,
        "FirstValue"
    );
    assert_eq!(
        tagged
            .variant_named("second")
            .expect("explicit rename should apply")
            .aliases,
        ["legacy_second"]
    );
    assert!(
        schema
            .type_named("Untagged")
            .expect("should extract")
            .untagged
    );
}

#[test]
fn models_container_defaults_skip_deserializing_and_variant_field_rename_rules() {
    let source = r#"
        #[derive(serde::Deserialize)]
        #[serde(rename = "defaulted", default, rename_all = "camelCase")]
        struct Defaulted {
            first_value: String,
            #[serde(skip_deserializing)]
            output_only: String,
            #[serde(skip_serializing_if = "String::is_empty")]
            conditional_output: String,
        }

        #[derive(serde::Deserialize)]
        #[serde(rename_all = "SCREAMING_SNAKE_CASE", rename_all_fields = "kebab-case")]
        enum Choice {
            StructValue { field_name: String },
            #[serde(rename_all = "camelCase", skip_serializing, skip_deserializing, untagged)]
            OverrideValue { other_field: String },
        }
    "#;

    let schema = extract_schema("fixture.rs", source, &empty_companions())
        .expect("supported site-specific attributes should extract");
    let defaulted = schema
        .type_named("Defaulted")
        .expect("should extract struct");
    assert_eq!(defaulted.serialized_name, "defaulted");
    assert_eq!(defaulted.container_default, Some(ContainerDefault::Trait));
    assert!(defaulted.field_named("firstValue").is_some());
    assert!(
        defaulted
            .field_named("outputOnly")
            .expect("should retain output-only field semantics")
            .skip_deserializing
    );
    assert_eq!(
        defaulted
            .field_named("conditionalOutput")
            .expect("should retain conditional serialization")
            .skip_serializing_if
            .as_deref(),
        Some("String::is_empty")
    );

    let choice = schema.type_named("Choice").expect("should extract enum");
    assert_eq!(choice.rename_all_fields.as_deref(), Some("kebab-case"));
    let inherited = choice
        .variant_named("STRUCT_VALUE")
        .expect("container variant rename should apply");
    assert!(inherited.field_named("field-name").is_some());
    let overridden = choice
        .variant_named("OVERRIDE_VALUE")
        .expect("container variant rename should apply");
    assert_eq!(overridden.rename_all.as_deref(), Some("camelCase"));
    assert!(overridden.skip_serializing);
    assert!(overridden.skip_deserializing);
    assert!(overridden.untagged);
    assert!(overridden.field_named("otherField").is_some());
}

#[test]
fn applies_only_the_complete_serde_rename_rule_set() {
    for (rule, field, variant) in [
        ("lowercase", "sample_value", "samplevalue"),
        ("UPPERCASE", "SAMPLE_VALUE", "SAMPLEVALUE"),
        ("PascalCase", "SampleValue", "SampleValue"),
        ("camelCase", "sampleValue", "sampleValue"),
        ("snake_case", "sample_value", "sample_value"),
        ("SCREAMING_SNAKE_CASE", "SAMPLE_VALUE", "SAMPLE_VALUE"),
        ("kebab-case", "sample-value", "sample-value"),
        ("SCREAMING-KEBAB-CASE", "SAMPLE-VALUE", "SAMPLE-VALUE"),
    ] {
        let source = format!(
            "#[serde(rename_all = \"{rule}\")] struct Fields {{ sample_value: String }}\n\
             #[serde(rename_all = \"{rule}\")] enum Variants {{ SampleValue }}"
        );
        let schema = extract_schema("fixture.rs", &source, &empty_companions())
            .unwrap_or_else(|error| panic!("{rule} should extract: {error:?}"));
        assert!(
            schema
                .type_named("Fields")
                .expect("should extract struct")
                .field_named(field)
                .is_some(),
            "field rule {rule} should produce {field}"
        );
        assert!(
            schema
                .type_named("Variants")
                .expect("should extract enum")
                .variant_named(variant)
                .is_some(),
            "variant rule {rule} should produce {variant}"
        );
    }

    for rule in ["snake", "SnakeCase", "", "SCREAMING_KEBAB_CASE"] {
        for source in [
            format!("#[serde(rename_all = \"{rule}\")] struct Fixture {{ value: String }}"),
            format!(
                "#[serde(rename_all_fields = \"{rule}\")] enum Fixture {{ Value {{ field: String }} }}"
            ),
            format!(
                "enum Fixture {{ #[serde(rename_all = \"{rule}\")] Value {{ field: String }} }}"
            ),
        ] {
            let error = extract_schema("fixture.rs", &source, &empty_companions())
                .expect_err("invalid Serde rename rules should fail closed");
            assert!(error.to_string().contains("invalid rename_all"));
        }
    }
}

#[test]
fn rejects_supported_serde_attributes_at_unsupported_sites() {
    for source in [
        "#[serde(rename_all = \"snake_case\")] struct Fixture { #[serde(rename_all = \"camelCase\")] value: String }",
        "#[serde(rename_all_fields = \"snake_case\")] struct Fixture { value: String }",
        "#[serde(content = \"value\")] struct Fixture { value: String }",
        "#[serde(untagged)] struct Fixture { value: String }",
        "#[serde(alias = \"Old\")] struct Fixture { value: String }",
        "enum Fixture { #[serde(default)] Value }",
        "enum Fixture { #[serde(tag = \"kind\")] Value }",
        "enum Fixture { #[serde(flatten)] Value }",
        "#[serde(skip_deserializing)] struct Fixture { value: String }",
    ] {
        let error = extract_schema("fixture.rs", source, &empty_companions())
            .expect_err("site-invalid Serde attributes should fail closed");
        assert!(
            error.to_string().contains("invalid serde"),
            "site diagnostic should identify Serde misuse: {error:?}"
        );
    }
}

#[test]
fn resolves_literal_defaults_and_requires_companions_for_nonliteral_defaults() {
    let source = r#"
        #[derive(serde::Deserialize)]
        struct Fixture {
            #[serde(default = "literal_default")]
            literal: u16,
            #[serde(default = "computed_default")]
            computed: usize,
        }
        fn literal_default() -> u16 { 7 }
        fn computed_default() -> usize { 1 << 4 }
    "#;

    let error = extract_schema("fixture.rs", source, &empty_companions())
        .expect_err("nonliteral default should require a checked companion");
    assert!(error.to_string().contains("computed_default"));

    let companions = CompanionManifest::parse(
        r#"
        version = 1
        reviewed = true

        [[companions]]
        source = "fixture.rs"
        symbol = "computed_default"
        kind = "default"
        value = "16"
        positive_probe = "task7_fixture_rs_computed_default_default_positive"
        negative_probe = "task7_fixture_rs_computed_default_default_negative"
        "#,
    )
    .expect("should parse companion");
    let schema = extract_schema("fixture.rs", source, &companions)
        .expect("checked companion should resolve nonliteral default");
    let fixture = schema
        .type_named("Fixture")
        .expect("should extract Fixture");
    assert_eq!(
        fixture
            .field_named("literal")
            .expect("literal field")
            .default,
        Some(DefaultValue::Literal("7".to_owned()))
    );
    assert_eq!(
        fixture
            .field_named("computed")
            .expect("computed field")
            .default,
        Some(DefaultValue::Companion("16".to_owned()))
    );
}

#[test]
fn custom_deserializers_and_validators_require_exact_companions() {
    let source = r#"
        #[derive(serde::Deserialize, validator::Validate)]
        struct Fixture {
            #[serde(deserialize_with = "parse_value")]
            #[validate(custom(function = "validate_value"))]
            value: String,
        }
    "#;
    let missing = extract_schema("fixture.rs", source, &empty_companions())
        .expect_err("custom behavior without companions should fail closed");
    let diagnostic = missing.to_string();
    assert!(diagnostic.contains("parse_value") || diagnostic.contains("validate_value"));

    let companions = CompanionManifest::parse(
        r#"
        version = 1
        reviewed = true

        [[companions]]
        source = "fixture.rs"
        symbol = "parse_value"
        kind = "deserializer"
        positive_probe = "task7_fixture_rs_parse_value_deserializer_positive"
        negative_probe = "task7_fixture_rs_parse_value_deserializer_negative"

        [[companions]]
        source = "fixture.rs"
        symbol = "validate_value"
        kind = "validator"
        positive_probe = "task7_fixture_rs_validate_value_validator_positive"
        negative_probe = "task7_fixture_rs_validate_value_validator_negative"
        "#,
    )
    .expect("should parse companions");
    let schema = extract_schema("fixture.rs", source, &companions)
        .expect("exact companions should classify custom behavior");
    let value = schema
        .type_named("Fixture")
        .expect("should extract Fixture")
        .field_named("value")
        .expect("should extract value");
    assert_eq!(value.deserializer.as_deref(), Some("parse_value"));
    assert_eq!(value.validators, ["validate_value"]);
}

#[test]
fn companion_manifest_must_equal_the_exact_ast_discovery_set() {
    let companions = CompanionManifest::parse(
        r#"
        version = 1
        reviewed = true

        [[companions]]
        source = "fixture.rs"
        symbol = "stale_validator"
        kind = "validator"
        positive_probe = "task7_fixture_rs_stale_validator_validator_positive"
        negative_probe = "task7_fixture_rs_stale_validator_validator_negative"
        "#,
    )
    .expect("should parse stale companion fixture");
    let error = extract_schema(
        "fixture.rs",
        "struct Fixture { value: String }",
        &companions,
    )
    .expect_err("stale companion entries should fail exact AST equality");
    assert!(error.to_string().contains("companion set"));
}

#[test]
fn compiled_receipts_bind_exact_values_and_probe_names_to_each_symbol() {
    let source = r#"
        struct Fixture {
            #[serde(default = "computed_default")]
            value: usize,
        }
        fn computed_default() -> usize { 1 << 4 }
    "#;
    let manifest = r#"
        version = 1
        reviewed = true

        [[companions]]
        source = "fixture.rs"
        symbol = "computed_default"
        kind = "default"
        value = "16"
        positive_probe = "task7_fixture_rs_computed_default_default_positive"
        negative_probe = "task7_fixture_rs_computed_default_default_negative"
    "#;
    let companions = CompanionManifest::parse(manifest).expect("should parse companion fixture");
    extract_schema("fixture.rs", source, &companions)
        .expect("exact discovered companion should extract");

    let wrong_value = [CompanionReceipt {
        symbol: "computed_default".to_owned(),
        kind: "default".to_owned(),
        value: Some("15".to_owned()),
        positive_probe: "task7_fixture_rs_computed_default_default_positive".to_owned(),
        negative_probe: "task7_fixture_rs_computed_default_default_negative".to_owned(),
        positive_passed: true,
        negative_passed: true,
    }];
    let error = companions
        .verify_compiled_receipts("fixture.rs", &wrong_value)
        .expect_err("wrong compiled default value should fail");
    assert!(error.to_string().contains("compiled companion receipt"));

    let wrong_probe = [CompanionReceipt {
        value: Some("16".to_owned()),
        positive_probe: "task7_default_other_symbol_positive".to_owned(),
        ..wrong_value[0].clone()
    }];
    let error = companions
        .verify_compiled_receipts("fixture.rs", &wrong_probe)
        .expect_err("probe-symbol mismatch should fail");
    assert!(error.to_string().contains("compiled companion receipt"));
}

#[test]
fn manifest_probe_names_are_unique_per_source_symbol_and_kind() {
    let error = CompanionManifest::parse(
        r#"
        version = 1
        reviewed = true

        [[companions]]
        source = "fixture.rs"
        symbol = "computed_default"
        kind = "default"
        value = "16"
        positive_probe = "task7_settings_companion_positive"
        negative_probe = "task7_settings_companion_negative"
        "#,
    )
    .expect_err("broad reusable probe names should fail closed");
    assert!(error.to_string().contains("probe names"));
}

#[test]
fn unknown_shape_changing_serde_attributes_fail_closed() {
    for attribute in [
        "#[serde(from = \"Wire\")]",
        "#[serde(try_from = \"Wire\")]",
        "#[serde(into = \"Wire\")]",
        "#[serde(remote = \"Remote\")]",
        "#[serde(transparent)]",
        "#[serde(with = \"opaque\")]",
    ] {
        let source = format!(
            "#[derive(serde::Deserialize)]\n{attribute}\nstruct Fixture {{ value: String }}"
        );
        let error = extract_schema("fixture.rs", &source, &empty_companions())
            .expect_err("unknown shape-changing attribute should fail closed");
        assert!(
            error.to_string().contains("unsupported serde attribute"),
            "diagnostic should identify unsupported shape-changing behavior: {error:?}"
        );
    }
}

#[test]
fn directional_field_dispositions_remain_independent() {
    let companions = CompanionManifest::parse(
        r#"
        version = 1
        reviewed = true

        [[fields]]
        path = "Fixture.legacy_secret"
        lifecycle = "deprecated"
        key_identity = "canonical"
        serialization = "skipped"
        runtime = "normalized_away"
        secret = "store_resolved"
        "#,
    )
    .expect("should parse independent dispositions");

    let axes = companions
        .field_disposition("Fixture.legacy_secret")
        .expect("should retain every axis");
    assert_eq!(axes.lifecycle, Lifecycle::Deprecated);
    assert_eq!(axes.key_identity, KeyIdentity::Canonical);
    assert_eq!(axes.serialization, SerializationDisposition::Skipped);
    assert_eq!(axes.runtime, RuntimeDisposition::NormalizedAway);
    assert_eq!(axes.secret, SecretDisposition::StoreResolved);
    assert!(axes.alias_of.is_none());
}

#[test]
fn aliases_require_an_exact_canonical_target() {
    let missing_target = CompanionManifest::parse(
        r#"
        version = 1
        reviewed = true

        [[fields]]
        path = "Fixture.old_name"
        lifecycle = "deprecated"
        key_identity = "alias"
        serialization = "skipped"
        runtime = "deserialization_only"
        secret = "none"
        "#,
    )
    .expect_err("alias without alias_of should fail closed");
    assert!(missing_target.to_string().contains("alias_of"));

    let canonical_with_target = CompanionManifest::parse(
        r#"
        version = 1
        reviewed = true

        [[fields]]
        path = "Fixture.name"
        lifecycle = "canonical"
        key_identity = "canonical"
        alias_of = "Fixture.other"
        serialization = "serialized"
        runtime = "active"
        secret = "none"
        "#,
    )
    .expect_err("canonical field with alias_of should fail closed");
    assert!(canonical_with_target.to_string().contains("alias_of"));
}

#[test]
fn checked_companions_cover_the_real_settings_sources() {
    let companions =
        CompanionManifest::parse(include_str!("../manifests/settings-companions.toml"))
            .expect("checked companion manifest should parse");
    for (path, source, required_type) in [
        (
            "crates/trusted-server-core/src/settings.rs",
            include_str!("../../../crates/trusted-server-core/src/settings.rs"),
            "Settings",
        ),
        (
            "crates/trusted-server-core/src/auction/profile.rs",
            include_str!("../../../crates/trusted-server-core/src/auction/profile.rs"),
            "StandardProfileConfig",
        ),
        (
            "crates/trusted-server-core/src/integrations/aps.rs",
            include_str!("../../../crates/trusted-server-core/src/integrations/aps.rs"),
            "ApsProfileConfig",
        ),
        (
            "crates/trusted-server-core/src/integrations/prebid.rs",
            include_str!("../../../crates/trusted-server-core/src/integrations/prebid.rs"),
            "PrebidIntegrationConfig",
        ),
    ] {
        let schema = extract_schema(path, source, &companions)
            .unwrap_or_else(|error| panic!("{path} should be covered: {error:?}"));
        assert!(
            schema.type_named(required_type).is_some(),
            "{path} should include {required_type}"
        );
    }
}

#[test]
fn settings_check_executes_the_complete_repository_contract() {
    let output = Command::new(PathBuf::from(env!("CARGO_BIN_EXE_docs-parity")))
        .args(["settings", "--check"])
        .current_dir(env::current_dir().expect("should read current directory"))
        .output()
        .expect("should execute docs-parity settings check");

    assert!(
        output.status.success(),
        "real settings contract should pass: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
