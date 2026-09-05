use std::collections::BTreeSet;
use std::env;
use std::path::PathBuf;
use std::process::Command;

use docs_parity::settings::{
    CompanionManifest, DefaultValue, HarnessObservation, KeyIdentity, Lifecycle,
    RuntimeDisposition, SecretDisposition, SerializationDisposition, TemplateContract,
    extract_schema, validate_harness_observation,
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
        positive_probe = "assert_eq!(computed_default(), 16);"
        negative_probe = "assert_ne!(computed_default(), 15);"
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
        positive_probe = "assert!(parse_value(true).is_ok());"
        negative_probe = "assert!(parse_value([]).is_err());"

        [[companions]]
        source = "fixture.rs"
        symbol = "validate_value"
        kind = "validator"
        positive_probe = "assert!(validate_value(\"ok\").is_ok());"
        negative_probe = "assert!(validate_value(\"\").is_err());"
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

fn harness_contract() -> TemplateContract {
    TemplateContract {
        placeholder_paths: set(&[
            "publisher.domain",
            "publisher.cookie_domain",
            "publisher.origin_url",
        ]),
        integration_ids: set(&["alpha", "beta"]),
        profile_ids: set(&["standard", "special"]),
        consumer_literals: set(&[
            "handler_password",
            "ec_passphrase",
            "publisher_proxy_secret",
        ]),
        expected_failure_diagnostic: "template placeholders remain".to_owned(),
    }
}

fn valid_observation() -> HarnessObservation {
    HarnessObservation {
        placeholder_paths: harness_contract().placeholder_paths,
        known_integration_ids: set(&["alpha", "beta"]),
        enabled_integration_probes: set(&["alpha", "beta"]),
        profile_compiler_probes: set(&["standard", "special"]),
        consumed_literals: set(&[
            "handler_password",
            "ec_passphrase",
            "publisher_proxy_secret",
        ]),
        unknown_keys: BTreeSet::new(),
        invalid_profiles: BTreeSet::new(),
        unresolved_secrets: BTreeSet::new(),
        inactive_shortcuts: BTreeSet::new(),
        failure_diagnostic: "template placeholders remain".to_owned(),
    }
}

fn set(values: &[&str]) -> BTreeSet<String> {
    values.iter().map(ToString::to_string).collect()
}

#[test]
fn unmodified_template_requires_the_exact_placeholder_set() {
    validate_harness_observation(&harness_contract(), &valid_observation())
        .expect("exact placeholder failure should be accepted");

    let mut typo = valid_observation();
    typo.placeholder_paths.remove("publisher.origin_url");
    typo.placeholder_paths
        .insert("publisher.orgin_url".to_owned());
    let error = validate_harness_observation(&harness_contract(), &typo)
        .expect_err("placeholder typo should not pass");
    assert!(error.to_string().contains("placeholder"));
}

#[test]
fn unknown_disabled_integration_key_does_not_pass() {
    let mut observation = valid_observation();
    observation
        .unknown_keys
        .insert("integrations.typo".to_owned());
    let error = validate_harness_observation(&harness_contract(), &observation)
        .expect_err("unknown key should fail even in a disabled block");
    assert!(error.to_string().contains("unknown"));
}

#[test]
fn bad_profile_config_does_not_pass() {
    let mut observation = valid_observation();
    observation.invalid_profiles.insert("special".to_owned());
    let error = validate_harness_observation(&harness_contract(), &observation)
        .expect_err("bad profile should fail");
    assert!(error.to_string().contains("profile"));
}

#[test]
fn unresolved_secret_does_not_pass() {
    let mut observation = valid_observation();
    observation
        .unresolved_secrets
        .insert("publisher.proxy_secret".to_owned());
    let error = validate_harness_observation(&harness_contract(), &observation)
        .expect_err("unresolved secret should fail");
    assert!(error.to_string().contains("secret"));
}

#[test]
fn stranded_literal_substitution_does_not_pass() {
    let mut observation = valid_observation();
    observation.consumed_literals.remove("handler_password");
    let error = validate_harness_observation(&harness_contract(), &observation)
        .expect_err("stranded literal should fail");
    assert!(error.to_string().contains("literal"));
}

#[test]
fn inactive_block_shortcut_does_not_pass() {
    let mut observation = valid_observation();
    observation.enabled_integration_probes.remove("beta");
    observation.inactive_shortcuts.insert("beta".to_owned());
    let error = validate_harness_observation(&harness_contract(), &observation)
        .expect_err("inactive shortcut should fail");
    assert!(error.to_string().contains("enabled"));
}

#[test]
fn missing_profile_compiler_probe_does_not_pass() {
    let mut observation = valid_observation();
    observation.profile_compiler_probes.remove("special");
    let error = validate_harness_observation(&harness_contract(), &observation)
        .expect_err("missing compiler probe should fail");
    assert!(error.to_string().contains("profile"));
}

#[test]
fn wrong_failure_diagnostic_does_not_pass() {
    let mut observation = valid_observation();
    observation.failure_diagnostic = "some other error".to_owned();
    let error = validate_harness_observation(&harness_contract(), &observation)
        .expect_err("wrong diagnostic should fail");
    assert!(error.to_string().contains("diagnostic"));
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
