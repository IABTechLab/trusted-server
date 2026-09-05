use std::collections::{BTreeMap, BTreeSet};

use docs_parity::integrations::{
    CapabilityRecord, IntegrationInventory, InventorySources, LoadingMode, OperationalRecord,
    extract_source_inventory, validate_inventory,
};

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
        deploy_validation: r#"
            fn validate_enabled_integrations(settings: &Settings, plan: &Plan) {
                validate_prebid(settings, plan)?;
                validate_integration::<ApsConfig>(settings, "aps")?;
                if let Some(config) = settings.integration_config::<DataDomeConfig>("datadome")? {
                    validate_datadome(config)?;
                }
            }
        "#,
        builders: r#"
            fn builders() -> &'static [IntegrationBuilder] {
                &[IntegrationBuilder { id: "testlight", build: testlight::register }]
            }
        "#,
        plan_registrations: r#"
            impl IntegrationRegistry {
                fn with_plan(settings: &Settings, plan: &Plan) {
                    prebid::register_for_plan(settings, plan)?;
                    aps::register_for_plan(settings, plan)?;
                }
            }
        "#,
        profiles: r#"
            const STANDARD_PROFILE_ID: &str = "standard";
            const APS_PROFILE_ID: &str = "aps";
            const PROFILE_REGISTRATIONS: [Registration; 2] = [
                Registration { id: STANDARD_PROFILE_ID, compile: compile_standard },
                Registration { id: APS_PROFILE_ID, compile: compile_aps },
            ];
        "#,
        mediator: r#"
            fn build_orchestrator_with_plan(settings: &Settings) {
                crate::integrations::adserver_mock::register_providers(settings)?;
            }
        "#,
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

#[test]
fn source_inventory_rejects_unknown_builder_shape() {
    let sources = InventorySources {
        deploy_validation: "fn validate_enabled_integrations() { validate_prebid()?; }",
        builders: r#"
            fn builders() -> &'static [IntegrationBuilder] {
                &[make_builder("testlight")]
            }
        "#,
        plan_registrations: "impl Registry { fn with_plan() {} }",
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
        deploy_validation: r#"
            fn validate_enabled_integrations() {
                validate_prebid()?;
                validate_integration::<ApsConfig>(settings, "aps")?;
                validate_future_integration(settings)?;
            }
        "#,
        builders: "fn builders() -> &'static [IntegrationBuilder] { &[] }",
        plan_registrations: "impl Registry { fn with_plan() {} }",
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
            deploy_validation: r#"
                fn validate_enabled_integrations() {
                    validate_integration::<A>(settings, "same")?;
                    validate_integration::<B>(settings, "same")?;
                }
            "#,
            builders: "fn builders() -> &'static [IntegrationBuilder] { &[] }",
            plan_registrations: "impl Registry { fn with_plan() {} }",
            profiles: "const PROFILE_REGISTRATIONS: [Registration; 0] = [];",
            mediator: "fn build_orchestrator_with_plan() {}",
            tracked_paths: &[],
        },
        InventorySources {
            deploy_validation: "fn validate_enabled_integrations() {}",
            builders: r#"
                fn builders() -> &'static [IntegrationBuilder] {
                    &[
                        IntegrationBuilder { id: "same", build: a::register },
                        IntegrationBuilder { id: "same", build: b::register },
                    ]
                }
            "#,
            plan_registrations: "impl Registry { fn with_plan() {} }",
            profiles: "const PROFILE_REGISTRATIONS: [Registration; 0] = [];",
            mediator: "fn build_orchestrator_with_plan() {}",
            tracked_paths: &[],
        },
        InventorySources {
            deploy_validation: "fn validate_enabled_integrations() {}",
            builders: "fn builders() -> &'static [IntegrationBuilder] { &[] }",
            plan_registrations: r#"
                impl Registry {
                    fn with_plan() {
                        same::register_for_plan()?;
                        same::register_for_plan()?;
                    }
                }
            "#,
            profiles: "const PROFILE_REGISTRATIONS: [Registration; 0] = [];",
            mediator: "fn build_orchestrator_with_plan() {}",
            tracked_paths: &[],
        },
        InventorySources {
            deploy_validation: "fn validate_enabled_integrations() {}",
            builders: "fn builders() -> &'static [IntegrationBuilder] { &[] }",
            plan_registrations: "impl Registry { fn with_plan() {} }",
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
            deploy_validation: "fn validate_enabled_integrations() {}",
            builders: "fn builders() -> &'static [IntegrationBuilder] { &[] }",
            plan_registrations: "impl Registry { fn with_plan() {} }",
            profiles: "const PROFILE_REGISTRATIONS: [Registration; 0] = [];",
            mediator: r#"
                fn build_orchestrator_with_plan() {
                    same::register_providers()?;
                    same::register_providers()?;
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
