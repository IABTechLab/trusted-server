use std::collections::{BTreeMap, BTreeSet};

use docs_parity::integrations::{
    CapabilityRecord, IntegrationInventory, InventorySources, LoadingMode, OperationalRecord,
    extract_source_inventory, validate_inventory,
};

const VALIDATION_ENTRYPOINTS: &str = r#"
    fn validate_enabled_integrations<T, P>(_: &T, _: &P, _: bool) {}
    fn validate_settings_for_deploy(settings: &Settings) {
        validate_enabled_integrations(settings, &plan, false)?;
    }
    fn validate_settings_for_runtime(settings: &Settings) {
        validate_enabled_integrations(settings, &plan, true)?;
    }
"#;

fn set(values: &[&str]) -> BTreeSet<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

fn minimal_manifest() -> String {
    r#"
        version = 1
        reviewed = true
        deploy_ids = ["deploy"]
        builder_ids = ["builder"]
        plan_registration_ids = ["plan"]
        profile_ids = ["profile"]
        mediator_ids = ["mediator"]
        js_source_module_ids = ["source"]
        js_bundle_ids = ["bundle"]
    "#
    .to_owned()
}

macro_rules! duplicate_static_axis_test {
    ($name:ident, $axis:literal, $value:literal) => {
        #[test]
        fn $name() {
            let source = minimal_manifest().replace(
                concat!($axis, " = [\"", $value, "\"]"),
                concat!($axis, " = [\"", $value, "\", \"", $value, "\"]"),
            );
            let error = IntegrationInventory::parse(&source).expect_err(concat!(
                "duplicate ",
                $axis,
                " values must fail"
            ));
            assert!(
                error.to_string().contains(concat!("duplicate ", $axis)),
                "duplicate {} should identify its static axis: {error:?}",
                $axis
            );
        }
    };
}

duplicate_static_axis_test!(duplicate_deploy_ids_fail_closed, "deploy_ids", "deploy");
duplicate_static_axis_test!(duplicate_builder_ids_fail_closed, "builder_ids", "builder");
duplicate_static_axis_test!(
    duplicate_plan_registration_ids_fail_closed,
    "plan_registration_ids",
    "plan"
);
duplicate_static_axis_test!(duplicate_profile_ids_fail_closed, "profile_ids", "profile");
duplicate_static_axis_test!(
    duplicate_mediator_ids_fail_closed,
    "mediator_ids",
    "mediator"
);
duplicate_static_axis_test!(
    duplicate_js_source_module_ids_fail_closed,
    "js_source_module_ids",
    "source"
);
duplicate_static_axis_test!(
    duplicate_js_bundle_ids_fail_closed,
    "js_bundle_ids",
    "bundle"
);

#[test]
fn source_inventory_extracts_each_authoritative_registration_surface() {
    let sources = InventorySources {
        validation_entrypoints: VALIDATION_ENTRYPOINTS,
        deploy_validation: r#"
            fn validate_enabled_integrations(settings: &Settings, plan: &Plan, resolved_secrets: bool) {
                validate_prebid(settings, plan)?;
                validate_integration::<ApsConfig>(settings, "aps")?;
                if let Some(config) = settings.integration_config::<DataDomeConfig>("datadome")? {
                    if resolved_secrets {
                        crate::integrations::datadome::DataDomeIntegration::validate_config_for_startup(config)?;
                    } else {
                        crate::integrations::datadome::DataDomeIntegration::validate_config_for_deploy(config)?;
                    }
                }
            }
        "#,
        builders: r#"
            fn builders() -> &'static [IntegrationBuilder] {
                &[IntegrationBuilder { id: "testlight", build: testlight::register }]
            }
        "#,
        plan_registrations: include_str!(
            "../../../crates/trusted-server-core/src/integrations/registry.rs"
        ),
        profiles: r#"
            const STANDARD_PROFILE_ID: &str = "standard";
            const APS_PROFILE_ID: &str = "aps";
            const PROFILE_REGISTRATIONS: [Registration; 2] = [
                Registration { id: STANDARD_PROFILE_ID, compile: compile_standard },
                Registration { id: APS_PROFILE_ID, compile: compile_aps },
            ];
        "#,
        mediator: include_str!("../../../crates/trusted-server-core/src/auction/mod.rs"),
        tracked_paths: &[
            "crates/trusted-server-js/lib/src/integrations/creative/index.ts",
            "crates/trusted-server-js/lib/src/integrations/prebid/index.ts",
            "crates/trusted-server-js/lib/src/integrations/aps/render.ts",
        ],
    };

    let inventory = extract_source_inventory(&sources).expect("known source grammar should parse");
    assert_eq!(inventory.deploy_ids, set(&["aps", "datadome", "prebid"]));
    assert_eq!(inventory.builder_ids, set(&["testlight"]));
    assert_eq!(inventory.plan_registration_ids, set(&["aps", "prebid"]));
    assert_eq!(inventory.profile_ids, set(&["aps", "standard"]));
    assert_eq!(inventory.mediator_ids, set(&["adserver_mock"]));
    assert_eq!(inventory.js_source_module_ids, set(&["creative", "prebid"]));
    assert_eq!(
        inventory.js_bundle_ids,
        set(&["core", "creative", "prebid"])
    );
}

fn closed_grammar_sources<'a>(deploy_validation: &'a str) -> InventorySources<'a> {
    InventorySources {
        validation_entrypoints: VALIDATION_ENTRYPOINTS,
        deploy_validation,
        builders: "fn builders() -> &'static [IntegrationBuilder] { &[] }",
        plan_registrations: "impl IntegrationRegistry { fn with_plan() {} }",
        profiles: "const PROFILE_REGISTRATIONS: [Registration; 0] = [];",
        mediator: "fn build_orchestrator_with_plan() {}",
        tracked_paths: &[],
    }
}

fn production_registration_sources<'a>(
    deploy_validation: &'a str,
    plan_registrations: &'a str,
    mediator: &'a str,
) -> InventorySources<'a> {
    InventorySources {
        validation_entrypoints: deploy_validation,
        deploy_validation,
        builders: include_str!("../../../crates/trusted-server-core/src/integrations/mod.rs"),
        plan_registrations,
        profiles: include_str!("../../../crates/trusted-server-core/src/auction/profile.rs"),
        mediator,
        tracked_paths: &[],
    }
}

#[test]
fn source_inventory_resolves_the_exact_owner_and_rejects_ambiguous_production_symbols() {
    let decoy = closed_grammar_sources(
        r#"
            mod decoy { fn validate_enabled_integrations() { validate_integration::<GptConfig>(settings, "gpt")?; } }
            fn validate_enabled_integrations() { validate_integration::<ApsConfig>(settings, "aps")?; }
        "#,
    );
    assert_eq!(
        extract_source_inventory(&decoy)
            .expect("a nested decoy must not own the production inventory")
            .deploy_ids,
        set(&["aps"])
    );

    let mut impl_decoy = closed_grammar_sources("fn validate_enabled_integrations() {}");
    impl_decoy.plan_registrations = concat!(
        "impl Decoy { fn with_plan() { bad::register_for_plan()?; } }\n",
        include_str!("../../../crates/trusted-server-core/src/integrations/registry.rs")
    );
    assert_eq!(
        extract_source_inventory(&impl_decoy)
            .expect("only IntegrationRegistry owns plan registration")
            .plan_registration_ids,
        set(&["aps", "prebid"])
    );

    for source in [
        r#"
            fn validate_enabled_integrations() {}
            fn validate_enabled_integrations() {}
        "#,
        r#"
            #[cfg(unix)]
            fn validate_enabled_integrations() {}
        "#,
    ] {
        assert!(
            extract_source_inventory(&closed_grammar_sources(source))
                .expect_err("ambiguous or conditionally compiled owners must fail closed")
                .to_string()
                .contains("deploy")
        );
    }

    let mut duplicate_plan = closed_grammar_sources("fn validate_enabled_integrations() {}");
    duplicate_plan.plan_registrations = r#"
        impl IntegrationRegistry { fn with_plan() {} }
        impl IntegrationRegistry { fn with_plan() {} }
    "#;
    assert!(
        extract_source_inventory(&duplicate_plan)
            .expect_err("duplicate exact-owner methods must fail")
            .to_string()
            .contains("plan")
    );

    let mut duplicate_mediator = closed_grammar_sources("fn validate_enabled_integrations() {}");
    duplicate_mediator.mediator = r#"
        fn build_orchestrator_with_plan() {}
        fn build_orchestrator_with_plan() {}
    "#;
    assert!(
        extract_source_inventory(&duplicate_mediator)
            .expect_err("duplicate mediator owners must fail")
            .to_string()
            .contains("mediator")
    );
}

#[test]
fn source_inventory_excludes_cfg_test_decoys_and_rejects_hidden_registration_constructs() {
    let cfg_test = closed_grammar_sources(
        r#"
            #[cfg(test)]
            fn validate_enabled_integrations() { validate_integration::<Bad>(settings, "bad")?; }
            fn validate_enabled_integrations() { validate_integration::<ApsConfig>(settings, "aps")?; }
        "#,
    );
    assert_eq!(
        extract_source_inventory(&cfg_test)
            .expect("cfg(test) decoys must be excluded")
            .deploy_ids,
        set(&["aps"])
    );

    for hidden in [
        "register_hidden!();",
        "let hidden = || validate_integration::<Bad>(settings, \"bad\");",
        "if false { validate_integration::<Bad>(settings, \"bad\")?; }",
    ] {
        let source = format!(
            "fn validate_enabled_integrations() {{ validate_integration::<ApsConfig>(settings, \"aps\")?; {hidden} }}"
        );
        assert!(
            extract_source_inventory(&closed_grammar_sources(&source))
                .expect_err("hidden registration constructs must fail closed")
                .to_string()
                .contains("deploy")
        );
    }

    for (axis, plan, mediator) in [
        (
            "plan",
            "impl IntegrationRegistry { fn with_plan() { register_hidden!(); } }",
            "fn build_orchestrator_with_plan() {}",
        ),
        (
            "plan",
            "impl IntegrationRegistry { fn with_plan() { if false { bad::register_for_plan()?; } } }",
            "fn build_orchestrator_with_plan() {}",
        ),
        (
            "mediator",
            "impl IntegrationRegistry { fn with_plan() {} }",
            "fn build_orchestrator_with_plan() { register_hidden!(); }",
        ),
        (
            "mediator",
            "impl IntegrationRegistry { fn with_plan() {} }",
            "fn build_orchestrator_with_plan() { if false { bad::register_providers()?; } }",
        ),
    ] {
        let mut sources = closed_grammar_sources("fn validate_enabled_integrations() {}");
        sources.plan_registrations = plan;
        sources.mediator = mediator;
        assert!(
            extract_source_inventory(&sources)
                .expect_err("plan and mediator hidden constructs must fail closed")
                .to_string()
                .contains(axis)
        );
    }

    let mut dropped_plan = closed_grammar_sources("fn validate_enabled_integrations() {}");
    dropped_plan.plan_registrations = r#"
        impl IntegrationRegistry {
            fn with_plan() {
                if let Some(registration) = good::register_for_plan()? { drop(registration); }
            }
        }
    "#;
    assert!(extract_source_inventory(&dropped_plan).is_err());

    let mut dropped_mediator = closed_grammar_sources("fn validate_enabled_integrations() {}");
    dropped_mediator.mediator = r#"
        fn build_orchestrator_with_plan() {
            let mediator = if let Some(expected) = plan.mediator() {
                let provider = good::register_providers()?;
                None
            } else { None };
            let orchestrator = AuctionOrchestrator::from_plan(plan, mediator);
            Ok(orchestrator)
        }
    "#;
    assert!(extract_source_inventory(&dropped_mediator).is_err());
}

#[test]
fn source_inventory_rejects_registration_dataflow_and_macro_token_decoys() {
    let mut cases = Vec::new();

    let mut cleared_plan = closed_grammar_sources("fn validate_enabled_integrations() {}");
    cleared_plan.plan_registrations = r#"
        impl IntegrationRegistry {
            fn with_plan() {
                if let Some(registration) = good::register_for_plan()? {
                    registrations.push(registration);
                }
                registrations.clear();
            }
        }
    "#;
    cases.push(cleared_plan);

    let mut macro_plan = closed_grammar_sources("fn validate_enabled_integrations() {}");
    macro_plan.plan_registrations = r#"
        impl IntegrationRegistry {
            fn with_plan() {
                debug_assert_eq!(bad::register_for_plan(), None);
            }
        }
    "#;
    cases.push(macro_plan);

    let mut extra_plan_statement = closed_grammar_sources("fn validate_enabled_integrations() {}");
    extra_plan_statement.plan_registrations = r#"
        impl IntegrationRegistry {
            fn with_plan() {
                if let Some(registration) = good::register_for_plan()? {
                    registrations.push(registration);
                    drop(registration);
                }
            }
        }
    "#;
    cases.push(extra_plan_statement);

    for sources in cases {
        assert!(
            extract_source_inventory(&sources).is_err(),
            "unbound, hidden, or subsequently discarded plan registrations must fail"
        );
    }

    let mut false_mediator = closed_grammar_sources("fn validate_enabled_integrations() {}");
    false_mediator.mediator = r#"
        fn build_orchestrator_with_plan() {
            let mediator = if false {
                let provider = good::register_providers()?;
                Some(provider)
            } else { None };
            let orchestrator = AuctionOrchestrator::from_plan(plan, mediator);
            Ok(orchestrator)
        }
    "#;
    assert!(
        extract_source_inventory(&false_mediator).is_err(),
        "mediator registration must be controlled by the exact plan predicate"
    );

    let mut macro_mediator = closed_grammar_sources("fn validate_enabled_integrations() {}");
    macro_mediator.mediator = r#"
        fn build_orchestrator_with_plan() {
            log::info!("{:?}", evil());
        }
    "#;
    assert!(
        extract_source_inventory(&macro_mediator).is_err(),
        "allowlisted diagnostic macros must not hide registrations"
    );
}

#[test]
fn source_inventory_binds_registration_paths_types_receivers_and_arguments() {
    let deploy = include_str!("../../../crates/trusted-server-core/src/config.rs");
    let plan = include_str!("../../../crates/trusted-server-core/src/integrations/registry.rs");
    let mediator = include_str!("../../../crates/trusted-server-core/src/auction/mod.rs");

    let deploy_mutations = [
        deploy.replacen(
            "validate_integration::<ApsConfig>(settings, \"aps\")?;",
            "validate_integration::<GptConfig>(settings, \"aps\")?;",
            1,
        ),
        deploy.replacen(
            "validate_integration::<ApsConfig>(settings, \"aps\")?;",
            "decoy::validate_integration::<ApsConfig>(settings, \"aps\")?;",
            1,
        ),
        deploy.replacen(
            "validate_integration::<ApsConfig>(settings, \"aps\")?;",
            "validate_integration::<ApsConfig>(other_settings, \"aps\")?;",
            1,
        ),
        deploy.replacen(
            "validate_prebid(settings, plan)?;",
            "validate_prebid(settings, other_plan)?;",
            1,
        ),
        deploy.replace(
            "settings.integration_config::<DataDomeConfig>(\"datadome\")?",
            "other_settings.integration_config::<DataDomeConfig>(\"datadome\")?",
        ),
        deploy.replace(
            "crate::integrations::datadome::DataDomeIntegration::validate_config_for_startup",
            "decoy::DataDomeIntegration::validate_config_for_startup",
        ),
    ];
    for changed in &deploy_mutations {
        assert!(
            extract_source_inventory(&production_registration_sources(changed, plan, mediator))
                .is_err(),
            "deploy registration authority mutation must fail"
        );
    }

    let plan_mutations = [
        plan.replacen(
            "crate::integrations::prebid::register_for_plan(settings, &plan)?",
            "decoy::prebid::register_for_plan(settings, &plan)?",
            1,
        ),
        plan.replacen(
            "crate::integrations::prebid::register_for_plan(settings, &plan)?",
            "crate::integrations::prebid::register_for_plan(settings, &other_plan)?",
            1,
        ),
        plan.replacen("crate::integrations::builders()", "decoy::builders()", 1),
    ];
    for changed in &plan_mutations {
        assert!(
            extract_source_inventory(&production_registration_sources(deploy, changed, mediator))
                .is_err(),
            "plan registration authority mutation must fail"
        );
    }

    let mediator_mutations = [
        mediator.replacen(
            "crate::integrations::adserver_mock::register_providers(settings)?",
            "decoy::adserver_mock::register_providers(settings)?",
            1,
        ),
        mediator.replacen(
            "crate::integrations::adserver_mock::register_providers(settings)?",
            "crate::integrations::adserver_mock::register_providers(other_settings)?",
            1,
        ),
        mediator.replacen(
            "    } else {\n        None\n    };",
            "    } else {\n        non_register_helper();\n        None\n    };",
            1,
        ),
    ];
    for changed in &mediator_mutations {
        assert!(
            extract_source_inventory(&production_registration_sources(deploy, plan, changed))
                .is_err(),
            "mediator registration authority mutation must fail"
        );
    }
}

#[test]
fn source_inventory_binds_both_validation_entrypoints_to_the_exact_validator_call() {
    let configuration = include_str!("../../../crates/trusted-server-core/src/config.rs");
    let plan = include_str!("../../../crates/trusted-server-core/src/integrations/registry.rs");
    let mediator = include_str!("../../../crates/trusted-server-core/src/auction/mod.rs");
    let deploy_call = "    validate_enabled_integrations(settings, &plan, false)?;";
    let runtime_call = "    validate_enabled_integrations(settings, &plan, true)?;";
    let changes = [
        configuration.replacen(deploy_call, "", 1),
        configuration.replacen(
            deploy_call,
            "    validate_enabled_integrations(settings, &plan, false)?;\n    validate_enabled_integrations(settings, &plan, false)?;",
            1,
        ),
        configuration.replacen(
            deploy_call,
            "    if true { validate_enabled_integrations(settings, &plan, false)?; }",
            1,
        ),
        configuration.replacen(
            deploy_call,
            "    { validate_enabled_integrations(settings, &plan, false)?; }",
            1,
        ),
        configuration.replacen(
            deploy_call,
            "    decoy::validate_enabled_integrations(settings, &plan, false)?;",
            1,
        ),
        configuration.replacen(runtime_call, "", 1),
        configuration.replacen(
            runtime_call,
            "    validate_enabled_integrations(settings, &plan, false)?;",
            1,
        ),
    ];

    for changed in &changes {
        assert_ne!(
            changed, configuration,
            "fixture must alter configuration source"
        );
        assert!(
            extract_source_inventory(&production_registration_sources(changed, plan, mediator))
                .is_err(),
            "deploy and runtime entrypoints must each contain one exact live validator call"
        );
    }
}

#[test]
fn source_inventory_rejects_shadowed_or_bypassed_validation_entrypoints() {
    let configuration = include_str!("../../../crates/trusted-server-core/src/config.rs");
    let plan = include_str!("../../../crates/trusted-server-core/src/integrations/registry.rs");
    let mediator = include_str!("../../../crates/trusted-server-core/src/auction/mod.rs");
    let deploy_call = "    validate_enabled_integrations(settings, &plan, false)?;";
    let runtime_call = "    validate_enabled_integrations(settings, &plan, true)?;";
    let local_shadow = "    fn validate_enabled_integrations<T, P>(_: &T, _: &P, _: bool) -> Result<(), Report<TrustedServerError>> { Ok(()) }\n";
    let changes = [
        configuration.replacen(
            deploy_call,
            &format!("    if settings.auction.enabled {{ return Ok(()); }}\n{deploy_call}"),
            1,
        ),
        configuration.replacen(
            runtime_call,
            &format!("    if settings.auction.enabled {{ return Ok(()); }}\n{runtime_call}"),
            1,
        ),
        configuration.replacen(deploy_call, &format!("{local_shadow}{deploy_call}"), 1),
        configuration.replacen(runtime_call, &format!("{local_shadow}{runtime_call}"), 1),
    ];

    for changed in &changes {
        assert_ne!(changed, configuration, "fixture must alter configuration");
        assert!(
            extract_source_inventory(&production_registration_sources(changed, plan, mediator))
                .is_err(),
            "validation entrypoints must reach the module-level validator without a bypass or lexical shadow"
        );
    }

    let mut missing_module_owner = closed_grammar_sources("fn validate_enabled_integrations() {}");
    missing_module_owner.validation_entrypoints = r#"
        fn validate_settings_for_deploy(settings: &Settings) {
            validate_enabled_integrations(settings, &plan, false)?;
        }
        fn validate_settings_for_runtime(settings: &Settings) {
            validate_enabled_integrations(settings, &plan, true)?;
        }
    "#;
    assert!(
        extract_source_inventory(&missing_module_owner).is_err(),
        "entrypoint calls must resolve to a module-level validator in the same source"
    );
}

#[test]
fn source_inventory_rejects_block_scoped_validator_import_shadows() {
    let configuration = include_str!("../../../crates/trusted-server-core/src/config.rs");
    let plan = include_str!("../../../crates/trusted-server-core/src/integrations/registry.rs");
    let mediator = include_str!("../../../crates/trusted-server-core/src/auction/mod.rs");
    let deploy_call = "    validate_enabled_integrations(settings, &plan, false)?;";
    let runtime_call = "    validate_enabled_integrations(settings, &plan, true)?;";
    let changes = [
        configuration.replacen(
            deploy_call,
            &format!("    use decoy::validate_enabled_integrations;\n{deploy_call}"),
            1,
        ),
        configuration.replacen(
            runtime_call,
            &format!("    use decoy::validate_enabled_integrations;\n{runtime_call}"),
            1,
        ),
        configuration.replacen(
            deploy_call,
            &format!("    use decoy::noop as validate_enabled_integrations;\n{deploy_call}"),
            1,
        ),
        configuration.replacen(
            runtime_call,
            &format!("    use decoy::noop as validate_enabled_integrations;\n{runtime_call}"),
            1,
        ),
    ];
    for changed in &changes {
        assert_ne!(changed, configuration, "fixture must alter configuration");
        assert!(
            extract_source_inventory(&production_registration_sources(changed, plan, mediator))
                .is_err(),
            "entrypoint calls must not resolve through block-scoped imports"
        );
    }
}

#[test]
fn source_inventory_requires_exact_plan_initializers_and_return_constructors() {
    let configuration = include_str!("../../../crates/trusted-server-core/src/config.rs");
    let plan = include_str!("../../../crates/trusted-server-core/src/integrations/registry.rs");
    let mediator = include_str!("../../../crates/trusted-server-core/src/auction/mod.rs");
    let changes = [
        plan.replacen(
            "let mut inner = IntegrationRegistryInner::default();",
            "let mut inner = decoy::IntegrationRegistryInner::default();",
            1,
        ),
        plan.replacen(
            "let mut inner = IntegrationRegistryInner::default();",
            "let mut inner = IntegrationRegistryInner::default::<()>();",
            1,
        ),
        plan.replacen(
            "let mut inner = IntegrationRegistryInner::default();",
            "let mut inner = IntegrationRegistryInner::default(evil());",
            1,
        ),
        plan.replacen(
            "let mut registrations = Vec::new();",
            "let mut registrations = decoy::Vec::new();",
            1,
        ),
        plan.replacen(
            "let mut registrations = Vec::new();",
            "let mut registrations = Vec::new::<()>();",
            1,
        ),
        plan.replacen(
            "let mut registrations = Vec::new();",
            "let mut registrations = Vec::new(evil());",
            1,
        ),
        plan.replacen("inner: Arc::new(inner)", "inner: decoy::Arc::new(inner)", 1),
        plan.replacen("inner: Arc::new(inner)", "inner: Arc::new::<()>(inner)", 1),
        plan.replacen(
            "inner: Arc::new(inner)",
            "inner: Arc::new(inner, evil())",
            1,
        ),
        plan.replacen("plan: Some(plan)", "plan: decoy::Some(plan)", 1),
        plan.replacen("plan: Some(plan)", "plan: Some::<()>(plan)", 1),
        plan.replacen("plan: Some(plan)", "plan: Some(plan, evil())", 1),
    ];

    for changed in &changes {
        assert_ne!(changed, plan, "fixture must alter plan registration source");
        assert!(
            extract_source_inventory(&production_registration_sources(
                configuration,
                changed,
                mediator,
            ))
            .is_err(),
            "plan initializers and returned constructors must use exact paths and arity"
        );
    }
}

#[test]
fn source_inventory_requires_the_exact_mediator_error_closure() {
    let configuration = include_str!("../../../crates/trusted-server-core/src/config.rs");
    let plan = include_str!("../../../crates/trusted-server-core/src/integrations/registry.rs");
    let mediator = include_str!("../../../crates/trusted-server-core/src/auction/mod.rs");
    let changes = [
        mediator.replacen(
            ".ok_or_else(|| {\n                Report::new(",
            ".ok_or_else(|| {\n                evil();\n                Report::new(",
            1,
        ),
        mediator.replacen(
            "Report::new(TrustedServerError::Configuration",
            "decoy_report::new(TrustedServerError::Configuration",
            1,
        ),
        mediator.replacen(
            "Report::new(TrustedServerError::Configuration",
            "Report::new(decoy::TrustedServerError::Configuration",
            1,
        ),
        mediator.replacen("message: format!(", "message: decoy::format!(", 1),
        mediator.replacen(
            "with the exact same ID\"\n                    ),",
            "with the exact same ID\", evil()\n                    ),",
            1,
        ),
    ];

    for changed in &changes {
        assert_ne!(changed, mediator, "fixture must alter mediator source");
        assert!(
            extract_source_inventory(&production_registration_sources(
                configuration,
                plan,
                changed,
            ))
            .is_err(),
            "mediator error closure must be one exact Report construction expression"
        );
    }
}

#[test]
fn source_inventory_rejects_plan_consumption_statement_replacement() {
    let production =
        include_str!("../../../crates/trusted-server-core/src/integrations/registry.rs");
    let changed = production.replace(
        "inner.script_rewriters.extend(registration.script_rewriters);",
        "let _decoy = &registration.script_rewriters;",
    );
    let mut sources = closed_grammar_sources("fn validate_enabled_integrations() {}");
    sources.plan_registrations = &changed;

    assert!(
        extract_source_inventory(&sources).is_err(),
        "every registration-consumption statement must preserve exact dataflow"
    );
}

#[test]
fn source_inventory_rejects_nested_plan_route_dataflow_replacements() {
    let production =
        include_str!("../../../crates/trusted-server-core/src/integrations/registry.rs");
    let changes = [
        (
            "let value = (proxy.clone(), registration.integration_id);",
            "let value = ();",
        ),
        (
            "let matchit_path = if route.path.ends_with(\"/*\") {",
            "let matchit_path = if false {",
        ),
        (
            "let router = match route.method {",
            "let router = match Method::GET {",
        ),
        (
            "if let Err(e) = router.insert(&matchit_path, value) {",
            "if false {",
        ),
        (
            "inner.routes.push((route, registration.integration_id));",
            "inner.routes.push((route, \"decoy\"));",
        ),
    ];

    for (original, replacement) in changes {
        let changed = production.replacen(original, replacement, 1);
        assert_ne!(
            changed, production,
            "fixture must alter the production source"
        );
        let mut sources = closed_grammar_sources("fn validate_enabled_integrations() {}");
        sources.plan_registrations = &changed;
        assert!(
            extract_source_inventory(&sources).is_err(),
            "nested route-registration dataflow replacement must fail: {replacement}"
        );
    }
}

#[test]
fn source_inventory_requires_the_exact_datadome_validation_branch() {
    let source = r#"
        fn validate_enabled_integrations(settings: &Settings, resolved_secrets: bool) {
            if let Some(config) = settings.integration_config::<DataDomeConfig>("datadome")? {
                if resolved_secrets {
                    crate::integrations::datadome::DataDomeIntegration::validate_config_for_startup(config)?;
                } else {
                    crate::integrations::datadome::DataDomeIntegration::validate_config_for_deploy(config)?;
                }
            }
        }
    "#;
    extract_source_inventory(&closed_grammar_sources(source))
        .expect("the exact DataDome deploy/startup validation branch should parse");

    for changed in [
        source.replace("if resolved_secrets", "if false"),
        source.replace(
            "crate::integrations::datadome::DataDomeIntegration::validate_config_for_deploy(config)?;",
            "drop(config);",
        ),
        source.replace(
            "crate::integrations::datadome::DataDomeIntegration::validate_config_for_startup(config)?;",
            "crate::integrations::datadome::DataDomeIntegration::validate_config_for_deploy(config)?;",
        ),
    ] {
        assert!(
            extract_source_inventory(&closed_grammar_sources(&changed)).is_err(),
            "DataDome predicate and both validation branches are authoritative"
        );
    }
}

#[test]
fn source_inventory_rejects_oversize_rust_inputs_before_parsing() {
    const LIMIT: usize = 4 * 1024 * 1024;
    let base = "fn validate_enabled_integrations() {}";
    let exact = format!("{base}/*{}*/", " ".repeat(LIMIT - base.len() - 4));
    extract_source_inventory(&closed_grammar_sources(&exact))
        .expect("an exact-limit source must be accepted");
    let oversized = format!("{exact} ");
    assert!(
        extract_source_inventory(&closed_grammar_sources(&oversized))
            .expect_err("an over-limit source must fail before parsing")
            .to_string()
            .contains("4 MiB")
    );
}

#[test]
fn source_inventory_rejects_unknown_builder_shape() {
    let sources = InventorySources {
        validation_entrypoints: VALIDATION_ENTRYPOINTS,
        deploy_validation: "fn validate_enabled_integrations() { validate_prebid(settings, plan)?; }",
        builders: r#"
            fn builders() -> &'static [IntegrationBuilder] {
                &[make_builder("testlight")]
            }
        "#,
        plan_registrations: "impl IntegrationRegistry { fn with_plan() {} }",
        profiles: "const PROFILE_REGISTRATIONS: [Registration; 0] = [];",
        mediator: "fn build_orchestrator_with_plan() {}",
        tracked_paths: &[],
    };

    assert!(
        extract_source_inventory(&sources)
            .expect_err("unknown builder expressions must fail closed")
            .to_string()
            .contains("builder")
    );
}

#[test]
fn source_inventory_rejects_an_unclassified_deploy_validator() {
    let sources = InventorySources {
        validation_entrypoints: VALIDATION_ENTRYPOINTS,
        deploy_validation: r#"
            fn validate_enabled_integrations() {
                validate_prebid(settings, plan)?;
                validate_integration::<ApsConfig>(settings, "aps")?;
                validate_future_integration(settings)?;
            }
        "#,
        builders: "fn builders() -> &'static [IntegrationBuilder] { &[] }",
        plan_registrations: "impl IntegrationRegistry { fn with_plan() {} }",
        profiles: "const PROFILE_REGISTRATIONS: [Registration; 0] = [];",
        mediator: "fn build_orchestrator_with_plan() {}",
        tracked_paths: &[],
    };

    assert!(
        extract_source_inventory(&sources)
            .expect_err("an unclassified deploy validator must fail closed")
            .to_string()
            .contains("deploy validator")
    );
}

#[test]
fn source_inventory_rejects_duplicate_registrations_on_every_rust_axis() {
    let cases = [
        InventorySources {
            validation_entrypoints: VALIDATION_ENTRYPOINTS,
            deploy_validation: r#"
                fn validate_enabled_integrations() {
                    validate_integration::<ApsConfig>(settings, "aps")?;
                    validate_integration::<ApsConfig>(settings, "aps")?;
                }
            "#,
            builders: "fn builders() -> &'static [IntegrationBuilder] { &[] }",
            plan_registrations: "impl IntegrationRegistry { fn with_plan() {} }",
            profiles: "const PROFILE_REGISTRATIONS: [Registration; 0] = [];",
            mediator: "fn build_orchestrator_with_plan() {}",
            tracked_paths: &[],
        },
        InventorySources {
            validation_entrypoints: VALIDATION_ENTRYPOINTS,
            deploy_validation: "fn validate_enabled_integrations() {}",
            builders: r#"
                fn builders() -> &'static [IntegrationBuilder] {
                    &[
                        IntegrationBuilder { id: "same", build: a::register },
                        IntegrationBuilder { id: "same", build: b::register },
                    ]
                }
            "#,
            plan_registrations: "impl IntegrationRegistry { fn with_plan() {} }",
            profiles: "const PROFILE_REGISTRATIONS: [Registration; 0] = [];",
            mediator: "fn build_orchestrator_with_plan() {}",
            tracked_paths: &[],
        },
        InventorySources {
            validation_entrypoints: VALIDATION_ENTRYPOINTS,
            deploy_validation: "fn validate_enabled_integrations() {}",
            builders: "fn builders() -> &'static [IntegrationBuilder] { &[] }",
            plan_registrations: r#"
                impl IntegrationRegistry {
                    fn with_plan() {
                        if true {
                            let first = crate::integrations::prebid::register_for_plan(settings, &plan)?;
                            let second = crate::integrations::prebid::register_for_plan(settings, &plan)?;
                        }
                    }
                }
            "#,
            profiles: "const PROFILE_REGISTRATIONS: [Registration; 0] = [];",
            mediator: "fn build_orchestrator_with_plan() {}",
            tracked_paths: &[],
        },
        InventorySources {
            validation_entrypoints: VALIDATION_ENTRYPOINTS,
            deploy_validation: "fn validate_enabled_integrations() {}",
            builders: "fn builders() -> &'static [IntegrationBuilder] { &[] }",
            plan_registrations: "impl IntegrationRegistry { fn with_plan() {} }",
            profiles: r#"
                const PROFILE_REGISTRATIONS: [Registration; 2] = [
                    Registration { id: "same", compile: compile_a },
                    Registration { id: "same", compile: compile_b },
                ];
            "#,
            mediator: "fn build_orchestrator_with_plan() {}",
            tracked_paths: &[],
        },
        InventorySources {
            validation_entrypoints: VALIDATION_ENTRYPOINTS,
            deploy_validation: "fn validate_enabled_integrations() {}",
            builders: "fn builders() -> &'static [IntegrationBuilder] { &[] }",
            plan_registrations: "impl IntegrationRegistry { fn with_plan() {} }",
            profiles: "const PROFILE_REGISTRATIONS: [Registration; 0] = [];",
            mediator: r#"
                fn build_orchestrator_with_plan() {
                    let mediator = if let Some(expected) = plan.mediator() {
                        let provider = same::register_providers()?;
                        let duplicate = same::register_providers()?;
                        Some(provider)
                    } else { None };
                    let orchestrator = AuctionOrchestrator::from_plan(plan, mediator);
                    Ok(orchestrator)
                }
            "#,
            tracked_paths: &[],
        },
    ];

    for sources in cases {
        assert!(
            extract_source_inventory(&sources)
                .expect_err("duplicate Rust registrations must fail")
                .to_string()
                .contains("duplicate")
        );
    }
}

#[test]
fn repository_source_inventory_has_the_exact_checked_cardinality() {
    let js_paths = [
        "crates/trusted-server-js/lib/src/integrations/creative/index.ts",
        "crates/trusted-server-js/lib/src/integrations/datadome/index.ts",
        "crates/trusted-server-js/lib/src/integrations/didomi/index.ts",
        "crates/trusted-server-js/lib/src/integrations/google_tag_manager/index.ts",
        "crates/trusted-server-js/lib/src/integrations/gpt/index.ts",
        "crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/index.ts",
        "crates/trusted-server-js/lib/src/integrations/lockr/index.ts",
        "crates/trusted-server-js/lib/src/integrations/osano/index.ts",
        "crates/trusted-server-js/lib/src/integrations/permutive/index.ts",
        "crates/trusted-server-js/lib/src/integrations/prebid/index.ts",
        "crates/trusted-server-js/lib/src/integrations/sourcepoint/index.ts",
        "crates/trusted-server-js/lib/src/integrations/testlight/index.ts",
    ];
    let sources = InventorySources {
        validation_entrypoints: include_str!("../../../crates/trusted-server-core/src/config.rs"),
        deploy_validation: include_str!("../../../crates/trusted-server-core/src/config.rs"),
        builders: include_str!("../../../crates/trusted-server-core/src/integrations/mod.rs"),
        plan_registrations: include_str!(
            "../../../crates/trusted-server-core/src/integrations/registry.rs"
        ),
        profiles: include_str!("../../../crates/trusted-server-core/src/auction/profile.rs"),
        mediator: include_str!("../../../crates/trusted-server-core/src/auction/mod.rs"),
        tracked_paths: &js_paths,
    };

    let inventory = extract_source_inventory(&sources).expect("repository grammar should parse");
    assert_eq!(inventory.deploy_ids.len(), 14);
    assert_eq!(inventory.builder_ids.len(), 11);
    assert_eq!(inventory.plan_registration_ids, set(&["aps", "prebid"]));
    assert_eq!(
        inventory.profile_ids,
        set(&["aps", "prebid-server", "standard"])
    );
    assert_eq!(inventory.mediator_ids, set(&["adserver_mock"]));
    assert_eq!(inventory.js_source_module_ids.len(), 12);
    assert_eq!(inventory.js_bundle_ids.len(), 13);
}

fn inventory() -> IntegrationInventory {
    IntegrationInventory {
        deploy_ids: set(&["aps", "prebid"]),
        builder_ids: set(&["testlight"]),
        plan_registration_ids: set(&["aps", "prebid"]),
        profile_ids: set(&["aps", "prebid-server", "standard"]),
        mediator_ids: set(&["adserver_mock"]),
        js_source_module_ids: set(&["creative", "prebid"]),
        js_bundle_ids: set(&["core", "creative", "prebid"]),
        loading_modes: BTreeMap::from([
            ("creative".to_owned(), LoadingMode::Bundled),
            ("prebid".to_owned(), LoadingMode::Deferred),
        ]),
        capabilities: BTreeSet::from([
            CapabilityRecord::new(
                "creative",
                "always",
                &[] as &[&str],
                &[] as &[&str],
                &[] as &[&str],
                &[] as &[&str],
                &[] as &[&str],
                &[] as &[&str],
                &[] as &[&str],
                "bundled",
            ),
            CapabilityRecord::new(
                "prebid",
                "enabled",
                ["GET /integrations/prebid/bundle.js"],
                ["attribute"],
                &[] as &[&str],
                ["head"],
                &[] as &[&str],
                &[] as &[&str],
                &[] as &[&str],
                "deferred",
            ),
        ]),
        operational: BTreeSet::from([
            OperationalRecord {
                id: "creative".to_owned(),
                status: "development".to_owned(),
                owner: "reviewer".to_owned(),
                reviewed_at: "2026-09-05".to_owned(),
            },
            OperationalRecord {
                id: "prebid".to_owned(),
                status: "production".to_owned(),
                owner: "reviewer".to_owned(),
                reviewed_at: "2026-09-05".to_owned(),
            },
        ]),
    }
}

#[test]
fn inventory_equality_rejects_missing_and_extra_entries_on_every_axis() {
    type InventoryMutation = (&'static str, Box<dyn Fn(&mut IntegrationInventory)>);

    let expected = inventory();
    validate_inventory(&expected, &expected).expect("identical inventory should pass");

    let mut mutations: Vec<InventoryMutation> = vec![
        (
            "deploy",
            Box::new(|value| {
                value.deploy_ids.insert("extra".to_owned());
            }),
        ),
        ("builder", Box::new(|value| value.builder_ids.clear())),
        (
            "plan",
            Box::new(|value| value.plan_registration_ids.clear()),
        ),
        (
            "profile",
            Box::new(|value| {
                value.profile_ids.insert("extra".to_owned());
            }),
        ),
        ("mediator", Box::new(|value| value.mediator_ids.clear())),
        (
            "js source",
            Box::new(|value| value.js_source_module_ids.clear()),
        ),
        (
            "js bundle",
            Box::new(|value| {
                value.js_bundle_ids.insert("extra".to_owned());
            }),
        ),
        (
            "loading",
            Box::new(|value| {
                value
                    .loading_modes
                    .insert("prebid".to_owned(), LoadingMode::Bundled);
            }),
        ),
        ("capability", Box::new(|value| value.capabilities.clear())),
        ("operational", Box::new(|value| value.operational.clear())),
    ];

    for (axis, mutate) in mutations.drain(..) {
        let mut observed = expected.clone();
        mutate(&mut observed);
        let error = validate_inventory(&expected, &observed)
            .expect_err("inventory drift must fail exact equality");
        assert!(
            error.to_string().contains(axis),
            "{axis} drift should identify its inventory axis: {error:?}"
        );
    }
}

#[test]
fn duplicate_manifest_rows_and_unknown_shapes_fail_closed() {
    let duplicate = r#"
        version = 1
        reviewed = true
        deploy_ids = ["prebid"]
        builder_ids = []
        plan_registration_ids = []
        profile_ids = []
        mediator_ids = []
        js_source_module_ids = []
        js_bundle_ids = []

        [[loading_modes]]
        id = "prebid"
        mode = "deferred"

        [[loading_modes]]
        id = "prebid"
        mode = "bundled"
    "#;
    assert!(
        IntegrationInventory::parse(duplicate)
            .expect_err("duplicate loading rows must fail")
            .to_string()
            .contains("duplicate")
    );

    let unknown = duplicate.replace("reviewed = true", "reviewed = true\nunknown = true");
    assert!(
        IntegrationInventory::parse(&unknown)
            .expect_err("unknown manifest fields must fail")
            .to_string()
            .contains("unknown")
    );
}

#[test]
fn duplicate_capability_members_and_keyed_rows_fail_closed() {
    let base = format!(
        "{}\n[[capabilities]]\nid = \"integration\"\npredicate = \"enabled\"\nproxy_routes = [\"route\"]\nattribute_rewriters = [\"attribute\"]\nscript_rewriters = [\"script\"]\nhead_injectors = [\"head\"]\npost_processors = [\"post\"]\nrequest_filters = [\"filter\"]\nproviders = [\"provider\"]\njs_mode = \"bundled\"\n",
        minimal_manifest()
    );
    for (field, value) in [
        ("proxy_routes", "route"),
        ("attribute_rewriters", "attribute"),
        ("script_rewriters", "script"),
        ("head_injectors", "head"),
        ("post_processors", "post"),
        ("request_filters", "filter"),
        ("providers", "provider"),
    ] {
        let source = base.replace(
            &format!("{field} = [\"{value}\"]"),
            &format!("{field} = [\"{value}\", \"{value}\"]"),
        );
        let error = IntegrationInventory::parse(&source)
            .expect_err("duplicate capability members must fail");
        assert!(
            error.to_string().contains(field),
            "duplicate {field} should identify its capability axis: {error:?}"
        );
    }

    let duplicate_key = format!(
        "{base}\n[[capabilities]]\nid = \"integration\"\npredicate = \"enabled\"\njs_mode = \"none\"\n"
    );
    assert!(
        IntegrationInventory::parse(&duplicate_key)
            .expect_err("duplicate capability id/predicate keys must fail")
            .to_string()
            .contains("duplicate capability key")
    );
}

#[test]
fn duplicate_loading_and_operational_assignments_fail_closed() {
    let duplicate_loading = format!(
        "{}\n[[loading_modes]]\nid = \"source\"\nmode = \"bundled\"\n[[loading_modes]]\nid = \"source\"\nmode = \"bundled\"\n",
        minimal_manifest()
    );
    assert!(
        IntegrationInventory::parse(&duplicate_loading)
            .expect_err("duplicate loading assignments must fail")
            .to_string()
            .contains("duplicate loading")
    );

    let duplicate_operational = format!(
        "{}\n[[operational]]\nid = \"integration\"\nstatus = \"production\"\nowner = \"reviewer\"\nreviewed_at = \"2026-09-05\"\n[[operational]]\nid = \"integration\"\nstatus = \"development\"\nowner = \"reviewer\"\nreviewed_at = \"2026-09-05\"\n",
        minimal_manifest()
    );
    assert!(
        IntegrationInventory::parse(&duplicate_operational)
            .expect_err("duplicate operational assignments must fail")
            .to_string()
            .contains("duplicate operational integration ID")
    );
}

#[test]
fn integration_manifest_caps_list_and_string_cardinality() {
    let too_many = (0..=4096)
        .map(|index| format!("\"id-{index}\""))
        .collect::<Vec<_>>()
        .join(",");
    let source = minimal_manifest().replace(
        "deploy_ids = [\"deploy\"]",
        &format!("deploy_ids = [{too_many}]"),
    );
    assert!(
        IntegrationInventory::parse(&source)
            .expect_err("oversized integration lists must fail")
            .to_string()
            .contains("cardinality")
    );
    let long = "x".repeat(16 * 1024 + 1);
    let source = minimal_manifest().replace("\"deploy\"", &format!("\"{long}\""));
    assert!(
        IntegrationInventory::parse(&source)
            .expect_err("oversized integration strings must fail")
            .to_string()
            .contains("string")
    );
}

#[test]
fn integration_manifest_caps_every_record_string() {
    let long = "x".repeat(16 * 1024 + 1);
    let record_sources = [
        format!(
            "{}\n[[loading_modes]]\nid = \"{long}\"\nmode = \"bundled\"\n",
            minimal_manifest()
        ),
        format!(
            "{}\n[[capabilities]]\nid = \"{long}\"\npredicate = \"enabled\"\njs_mode = \"none\"\n",
            minimal_manifest()
        ),
        format!(
            "{}\n[[capabilities]]\nid = \"integration\"\npredicate = \"{long}\"\njs_mode = \"none\"\n",
            minimal_manifest()
        ),
        format!(
            "{}\n[[capabilities]]\nid = \"integration\"\npredicate = \"enabled\"\njs_mode = \"{long}\"\n",
            minimal_manifest()
        ),
        format!(
            "{}\n[[operational]]\nid = \"integration\"\nstatus = \"production\"\nowner = \"{long}\"\nreviewed_at = \"2026-09-05\"\n",
            minimal_manifest()
        ),
    ];
    for source in record_sources {
        assert!(
            IntegrationInventory::parse(&source).is_err(),
            "every integration record string must observe the same bound"
        );
    }
}

#[test]
fn reviewed_manifest_matches_repository_sources_and_behavior_domains() {
    let expected = IntegrationInventory::parse(include_str!("../manifests/integrations.toml"))
        .expect("reviewed integration manifest should parse");
    let tracked_paths = [
        "crates/trusted-server-js/lib/src/integrations/creative/index.ts",
        "crates/trusted-server-js/lib/src/integrations/datadome/index.ts",
        "crates/trusted-server-js/lib/src/integrations/didomi/index.ts",
        "crates/trusted-server-js/lib/src/integrations/google_tag_manager/index.ts",
        "crates/trusted-server-js/lib/src/integrations/gpt/index.ts",
        "crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/index.ts",
        "crates/trusted-server-js/lib/src/integrations/lockr/index.ts",
        "crates/trusted-server-js/lib/src/integrations/osano/index.ts",
        "crates/trusted-server-js/lib/src/integrations/permutive/index.ts",
        "crates/trusted-server-js/lib/src/integrations/prebid/index.ts",
        "crates/trusted-server-js/lib/src/integrations/sourcepoint/index.ts",
        "crates/trusted-server-js/lib/src/integrations/testlight/index.ts",
    ];
    let source = extract_source_inventory(&InventorySources {
        validation_entrypoints: include_str!("../../../crates/trusted-server-core/src/config.rs"),
        deploy_validation: include_str!("../../../crates/trusted-server-core/src/config.rs"),
        builders: include_str!("../../../crates/trusted-server-core/src/integrations/mod.rs"),
        plan_registrations: include_str!(
            "../../../crates/trusted-server-core/src/integrations/registry.rs"
        ),
        profiles: include_str!("../../../crates/trusted-server-core/src/auction/profile.rs"),
        mediator: include_str!("../../../crates/trusted-server-core/src/auction/mod.rs"),
        tracked_paths: &tracked_paths,
    })
    .expect("repository integration sources should parse");
    let observed = IntegrationInventory {
        deploy_ids: source.deploy_ids,
        builder_ids: source.builder_ids,
        plan_registration_ids: source.plan_registration_ids,
        profile_ids: source.profile_ids,
        mediator_ids: source.mediator_ids,
        js_source_module_ids: source.js_source_module_ids,
        js_bundle_ids: source.js_bundle_ids,
        loading_modes: expected.loading_modes.clone(),
        capabilities: expected.capabilities.clone(),
        operational: expected.operational.clone(),
    };
    validate_inventory(&expected, &observed)
        .expect("every source-derived static set should equal the reviewed inventory");

    assert_eq!(
        expected
            .loading_modes
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>(),
        expected.js_source_module_ids,
        "every browser source module needs one exact loading disposition"
    );
    let capability_ids = expected
        .capabilities
        .iter()
        .map(|record| record.id.clone())
        .collect::<BTreeSet<_>>();
    let mut expected_capability_ids = expected.deploy_ids.clone();
    expected_capability_ids.insert("creative".to_owned());
    assert_eq!(capability_ids, expected_capability_ids);
    assert_eq!(
        expected
            .operational
            .iter()
            .map(|record| record.id.clone())
            .collect::<BTreeSet<_>>(),
        capability_ids,
        "manual owner/status/date rows cover every capability and no extras"
    );
    for capability in &expected.capabilities {
        match expected.loading_modes.get(&capability.id) {
            Some(mode) => assert_eq!(
                capability.js_mode,
                match mode {
                    LoadingMode::Bundled => "bundled",
                    LoadingMode::Deferred => "deferred",
                    LoadingMode::Standalone => "standalone",
                }
            ),
            None => assert_eq!(capability.js_mode, "none"),
        }
    }
}

#[test]
fn deploy_validation_helpers_are_exact_and_cannot_swallow_errors() {
    let deploy = include_str!("../../../crates/trusted-server-core/src/config.rs");
    let plan = include_str!("../../../crates/trusted-server-core/src/integrations/registry.rs");
    let mediator = include_str!("../../../crates/trusted-server-core/src/auction/mod.rs");

    let swallowed = deploy.replacen(
        "settings\n        .integration_config::<T>(integration_id)\n        .map(|config| config.is_some())",
        "Ok(settings.integration_config::<T>(integration_id).is_ok())",
        1,
    );
    assert_ne!(swallowed, deploy, "fixture must alter validate_integration");
    assert!(
        extract_source_inventory(&production_registration_sources(&swallowed, plan, mediator))
            .is_err(),
        "validate_integration must preserve configuration errors"
    );

    let prebid = deploy.replacen(
        "prebid::validate_browser_bidder_ownership(&config, plan)",
        "Ok(())",
        1,
    );
    assert_ne!(prebid, deploy, "fixture must alter validate_prebid");
    assert!(
        extract_source_inventory(&production_registration_sources(&prebid, plan, mediator))
            .is_err(),
        "validate_prebid must preserve browser and ownership validation"
    );
}
