use std::collections::BTreeSet;

use docs_parity::integrations::IntegrationInventory;
use docs_parity::markdown::{OwnershipRecord, render_generated_document};
use docs_parity::routes::{
    AdapterSupportManifest, RouteManifest, RouteRecord, RouteShape, RouteSources, RouteStatus,
    documentation_region_path, documentation_regions, extract_cloudflare_routes,
    extract_named_routes, extract_repository_routes, validate_adapter_support,
    validate_documentation_contract, validate_middleware_sources, validate_routes,
};

fn route(
    adapter: &str,
    path: &str,
    methods: &[&str],
    shape: RouteShape,
    predicate: &str,
    status: RouteStatus,
    startup_router: bool,
) -> RouteRecord {
    RouteRecord::new(
        adapter,
        path,
        methods,
        shape,
        predicate,
        status,
        startup_router,
    )
}

fn with_publisher_path_constants(adapter_source: &str) -> String {
    let constants = include_str!("../../../crates/trusted-server-core/src/publisher.rs")
        .lines()
        .filter(|line| {
            line.contains("pub const PAGE_BIDS_PATH:")
                || line.contains("pub const PAGE_BIDS_LEGACY_PATH:")
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!("{adapter_source}\n{constants}")
}

#[test]
fn route_set_equality_rejects_every_semantic_axis() {
    let expected = BTreeSet::from([
        route(
            "fastly",
            "/health",
            &["GET"],
            RouteShape::Literal,
            "always",
            RouteStatus::Real,
            true,
        ),
        route(
            "cloudflare",
            "/{*rest}",
            &["GET", "POST"],
            RouteShape::Template,
            "publisher_fallback",
            RouteStatus::PublisherFallback,
            false,
        ),
    ]);
    validate_routes(&expected, &expected).expect("identical routes should pass");

    for (axis, changed) in [
        (
            "method",
            route(
                "fastly",
                "/health",
                &["POST"],
                RouteShape::Literal,
                "always",
                RouteStatus::Real,
                true,
            ),
        ),
        (
            "shape",
            route(
                "fastly",
                "/health",
                &["GET"],
                RouteShape::Conditional,
                "always",
                RouteStatus::Real,
                true,
            ),
        ),
        (
            "predicate",
            route(
                "fastly",
                "/health",
                &["GET"],
                RouteShape::Literal,
                "settings.debug.enabled",
                RouteStatus::Real,
                true,
            ),
        ),
        (
            "status",
            route(
                "fastly",
                "/health",
                &["GET"],
                RouteShape::Literal,
                "always",
                RouteStatus::Unsupported,
                true,
            ),
        ),
        (
            "startup-router",
            route(
                "fastly",
                "/health",
                &["GET"],
                RouteShape::Literal,
                "always",
                RouteStatus::Real,
                false,
            ),
        ),
    ] {
        let mut observed = expected.clone();
        observed.retain(|record| record.adapter != "fastly");
        observed.insert(changed);
        let error = validate_routes(&expected, &observed)
            .expect_err("every route semantic change must fail exact equality");
        assert!(error.to_string().contains(axis), "{axis}: {error:?}");
    }

    let mut missing = expected.clone();
    missing.pop_first();
    assert!(
        validate_routes(&expected, &missing)
            .expect_err("a missing route must fail")
            .to_string()
            .contains("route")
    );
    let mut extra = expected.clone();
    extra.insert(route(
        "spin",
        "/extra",
        &["GET"],
        RouteShape::Literal,
        "always",
        RouteStatus::Real,
        false,
    ));
    assert!(
        validate_routes(&expected, &extra)
            .expect_err("an extra route must fail")
            .to_string()
            .contains("route")
    );
}

#[test]
fn cloudflare_closed_grammar_expands_constants_loops_methods_and_fallback() {
    let source = r#"
        const PAGE_BIDS_PATH: &str = "/_ts/page-bids";
        const PAGE_BIDS_LEGACY_PATH: &str = "/__ts/page-bids";
        fn publisher_fallback_methods() -> [Method; 2] {
            [Method::GET, Method::POST]
        }
        fn build_router(state: &State) -> RouterService {
            let mut router = RouterService::builder()
                .get("/.well-known/trusted-server.json", discovery)
                .post("/auction", auction);
            for path in [PAGE_BIDS_PATH, PAGE_BIDS_LEGACY_PATH] {
                router = router.route(path, Method::GET, page_bids.clone());
                router = router.route(path, Method::OPTIONS, page_bids_preflight.clone());
            }
            for method in publisher_fallback_methods() {
                router = router.route("/", method.clone(), fallback.clone());
                router = router.route("/{*rest}", method, fallback.clone());
            }
            router.build()
        }
    "#;

    let observed = extract_cloudflare_routes(source).expect("known builder grammar should parse");
    assert_eq!(
        observed,
        BTreeSet::from([
            route(
                "cloudflare",
                "/.well-known/trusted-server.json",
                &["GET"],
                RouteShape::Literal,
                "always",
                RouteStatus::Real,
                false
            ),
            route(
                "cloudflare",
                "/auction",
                &["POST"],
                RouteShape::Literal,
                "always",
                RouteStatus::Real,
                false
            ),
            route(
                "cloudflare",
                "/_ts/page-bids",
                &["GET"],
                RouteShape::Literal,
                "always",
                RouteStatus::Real,
                false
            ),
            route(
                "cloudflare",
                "/_ts/page-bids",
                &["OPTIONS"],
                RouteShape::Literal,
                "always",
                RouteStatus::Guarded,
                false
            ),
            route(
                "cloudflare",
                "/__ts/page-bids",
                &["GET"],
                RouteShape::Literal,
                "always",
                RouteStatus::Real,
                false
            ),
            route(
                "cloudflare",
                "/__ts/page-bids",
                &["OPTIONS"],
                RouteShape::Literal,
                "always",
                RouteStatus::Guarded,
                false
            ),
            route(
                "cloudflare",
                "/",
                &["GET", "POST"],
                RouteShape::Literal,
                "publisher_fallback",
                RouteStatus::PublisherFallback,
                false
            ),
            route(
                "cloudflare",
                "/{*rest}",
                &["GET", "POST"],
                RouteShape::Template,
                "publisher_fallback",
                RouteStatus::PublisherFallback,
                false
            ),
        ])
    );
}

#[test]
fn cloudflare_unknown_builder_construct_fails_closed() {
    let source = r#"
        fn publisher_fallback_methods() -> [Method; 1] { [Method::GET] }
        fn build_router() {
            let mut router = RouterService::builder().get("/known", handler);
            register_hidden_route(&mut router, "/hidden");
            router.build()
        }
    "#;
    assert!(
        extract_cloudflare_routes(source)
            .expect_err("an unknown route builder construct must fail closed")
            .to_string()
            .contains("unsupported Cloudflare")
    );
}

#[test]
fn route_documentation_is_bound_to_checked_route_and_integration_records() {
    let routes = RouteManifest::parse(include_str!("../manifests/routes.toml"))
        .expect("reviewed route manifest should parse");
    let support = AdapterSupportManifest::parse(include_str!("../manifests/adapter-support.toml"))
        .expect("reviewed adapter support should parse");
    let integrations = IntegrationInventory::parse(include_str!("../manifests/integrations.toml"))
        .expect("reviewed integration inventory should parse");
    let pages = include_str!("../manifests/pages.toml");

    let regions = documentation_regions(&routes, &support, &integrations);
    assert_eq!(
        regions.len(),
        9,
        "all reader-facing adapter and integration summaries must be generated"
    );
    for (name, path) in [
        (
            "product-adapter-support",
            "docs/guide/integrations-overview.md",
        ),
        (
            "product-integration-inventory",
            "docs/guide/integrations-overview.md",
        ),
        ("adapter-support-fastly", "docs/guide/fastly.md"),
        ("adapter-support-axum", "docs/guide/axum-dev.md"),
        ("adapter-support-cloudflare", "docs/guide/cloudflare.md"),
        ("adapter-support-spin", "docs/guide/spin.md"),
    ] {
        assert_eq!(
            documentation_region_path(name),
            Some(path),
            "adapter summary must target its deployment guide"
        );
    }

    validate_documentation_contract(&routes, &support, &integrations, pages)
        .expect("the API region declarations should match their checked records");

    let ownership = [OwnershipRecord {
        name: "api-contract-details".to_owned(),
        owner: "documentation-maintainers".to_owned(),
    }];
    let api_reference = include_bytes!("../../../docs/guide/api-reference.md");
    let api_regions = regions
        .into_iter()
        .filter(|region| {
            documentation_region_path(&region.name) == Some("docs/guide/api-reference.md")
        })
        .collect::<Vec<_>>();
    let rendered = render_generated_document(api_reference, &api_regions, &ownership)
        .expect("checked route regions should render");
    assert!(
        rendered == api_reference,
        "the reader-facing route tables must match their generated records"
    );

    let changed_source = include_str!("../manifests/routes.toml").replacen(
        "path = \"/verify-signature\"",
        "path = \"/verify-signature-v2\"",
        1,
    );
    let changed =
        RouteManifest::parse(&changed_source).expect("changed route fixture should parse");
    let changed_render = render_generated_document(
        api_reference,
        &documentation_regions(&changed, &support, &integrations)
            .into_iter()
            .filter(|region| {
                documentation_region_path(&region.name) == Some("docs/guide/api-reference.md")
            })
            .collect::<Vec<_>>(),
        &ownership,
    )
    .expect("changed route fixture should render");
    assert_ne!(
        changed_render, api_reference,
        "a checked route change must reject an unregenerated API reference"
    );

    let overview = include_bytes!("../../../docs/guide/integrations-overview.md");
    let overview_regions = documentation_regions(&routes, &support, &integrations)
        .into_iter()
        .filter(|region| {
            documentation_region_path(&region.name) == Some("docs/guide/integrations-overview.md")
        })
        .collect::<Vec<_>>();
    let rendered = render_generated_document(overview, &overview_regions, &[])
        .expect("checked product regions should render");
    assert_eq!(
        rendered, overview,
        "reader-facing support and integration inventories must match checked records"
    );

    let changed_integrations = include_str!("../manifests/integrations.toml").replacen(
        "id = \"didomi\"\nstatus = \"production\"",
        "id = \"didomi\"\nstatus = \"development\"",
        1,
    );
    let changed_integrations = IntegrationInventory::parse(&changed_integrations)
        .expect("changed integration fixture should parse");
    let changed_render = render_generated_document(
        overview,
        &documentation_regions(&routes, &support, &changed_integrations)
            .into_iter()
            .filter(|region| {
                documentation_region_path(&region.name)
                    == Some("docs/guide/integrations-overview.md")
            })
            .collect::<Vec<_>>(),
        &[],
    )
    .expect("changed integration fixture should render");
    assert_ne!(
        changed_render, overview,
        "checked integration status drift must reject the rendered inventory"
    );
}

#[test]
fn adapter_middleware_documentation_is_bound_to_source_order() {
    let fastly = include_str!("../../../crates/trusted-server-adapter-fastly/src/main.rs");
    let axum = include_str!("../../../crates/trusted-server-adapter-axum/src/app.rs");
    let cloudflare = include_str!("../../../crates/trusted-server-adapter-cloudflare/src/app.rs");
    let spin = include_str!("../../../crates/trusted-server-adapter-spin/src/app.rs");
    validate_middleware_sources(fastly, axum, cloudflare, spin)
        .expect("checked middleware order should pass");

    let changed_fastly = format!(
        "{}\n// compat::resolve_and_sanitize_client_ip(&mut req, trusted_client_ip)\n",
        fastly.replace(
            "compat::resolve_and_sanitize_client_ip(&mut req, trusted_client_ip)",
            "compat::sanitize_only(&mut req)",
        )
    );
    assert!(validate_middleware_sources(&changed_fastly, axum, cloudflare, spin).is_err());

    let changed_axum = format!(
        "{}\n// .middleware(SanitizeRequestMiddleware::new(Arc::clone(&state.settings)))\n// .middleware(FinalizeResponseMiddleware::new(Arc::clone(&state.settings)))\n// .middleware(AuthMiddleware::new(Arc::clone(&state.settings)))\n",
        axum.replace(
            ".middleware(SanitizeRequestMiddleware::new(Arc::clone(&state.settings)))\n        .middleware(FinalizeResponseMiddleware::new(Arc::clone(&state.settings)))\n        .middleware(AuthMiddleware::new(Arc::clone(&state.settings)))",
            ".middleware(FinalizeResponseMiddleware::new(Arc::clone(&state.settings)))\n        .middleware(AuthMiddleware::new(Arc::clone(&state.settings)))\n        .middleware(SanitizeRequestMiddleware::new(Arc::clone(&state.settings)))",
        )
    );
    assert!(validate_middleware_sources(fastly, &changed_axum, cloudflare, spin).is_err());

    let changed_spin = format!(
        "{}\n// .middleware(NormalizeMiddleware::new())\n",
        spin.replace(
            ".middleware(NormalizeMiddleware::new())",
            ".middleware(AuthMiddleware::new(Arc::clone(&state.settings)))",
        )
    );
    assert!(validate_middleware_sources(fastly, axum, cloudflare, &changed_spin).is_err());
}

#[test]
fn cloudflare_follows_the_builder_returned_by_build_router() {
    let source = r#"
        fn build_router() {
            let mut actual = RouterService::builder().get("/actual", actual_handler);
            actual = actual.post("/post", post_handler);
            actual.build()
        }
    "#;
    let routes = extract_cloudflare_routes(source)
        .expect("the returned builder, not a variable spelling, is authoritative");
    assert_eq!(
        routes,
        BTreeSet::from([
            route(
                "cloudflare",
                "/actual",
                &["GET"],
                RouteShape::Literal,
                "always",
                RouteStatus::Real,
                false
            ),
            route(
                "cloudflare",
                "/post",
                &["POST"],
                RouteShape::Literal,
                "always",
                RouteStatus::Real,
                false
            ),
        ])
    );
}

#[test]
fn cloudflare_accepts_a_unique_returned_builder_chain() {
    let source = r#"
        fn build_router() {
            RouterService::builder().get("/actual", handler).build()
        }
    "#;
    assert_eq!(
        extract_cloudflare_routes(source).expect("a returned builder chain is authoritative"),
        BTreeSet::from([route(
            "cloudflare",
            "/actual",
            &["GET"],
            RouteShape::Literal,
            "always",
            RouteStatus::Real,
            false
        )])
    );
}

#[test]
fn cloudflare_rejects_ambiguous_or_hidden_builder_dataflow() {
    for source in [
        r#"fn build_router() { let decoy = RouterService::builder().get("/decoy", handler); let actual = RouterService::builder().get("/actual", handler); actual.build() }"#,
        r#"fn build_router() { let a = RouterService::builder(); let b = RouterService::builder(); a.build() }"#,
        r#"fn build_router() { let actual = RouterService::builder(); let alias = actual; alias.build() }"#,
        r#"fn build_router() { let actual = make_router!(); actual.build() }"#,
        r#"fn build_router() { fn hidden() { let hidden = RouterService::builder(); } let actual = RouterService::builder(); actual.build() }"#,
        r#"fn build_router() { let hidden = || RouterService::builder(); let actual = RouterService::builder(); actual.build() }"#,
        r#"fn build_router() { let actual = RouterService::builder(); loop { break; } actual.build() }"#,
        r#"fn build_router() { let actual = RouterService::builder(); return actual.build(); }"#,
        r#"fn build_router() { RouterService::builder(); let actual = RouterService::builder().get("/actual", handler); actual.build() }"#,
        r#"fn build_router() { let actual = RouterService::builder(); actual = RouterService::builder().get("/replacement", handler); actual.build() }"#,
        r#"fn build_router() { let actual = RouterService::builder(); let _hidden = conceal!(RouterService::builder()); actual.build() }"#,
        r#"fn build_router() { let actual = RouterService::builder(); unused = RouterService::builder(); actual.build() }"#,
    ] {
        assert!(
            extract_cloudflare_routes(source)
                .expect_err("ambiguous or hidden router dataflow must fail closed")
                .to_string()
                .contains("Cloudflare")
        );
    }
}

#[test]
fn route_extractors_reject_handler_swaps_on_every_adapter() {
    let fastly = include_str!("../../../crates/trusted-server-adapter-fastly/src/app.rs");
    let axum = include_str!("../../../crates/trusted-server-adapter-axum/src/app.rs");
    let spin = include_str!("../../../crates/trusted-server-adapter-spin/src/app.rs");
    let cloudflare = include_str!("../../../crates/trusted-server-adapter-cloudflare/src/app.rs");

    let fastly_swap = fastly.replacen(
        "handler: NamedRouteHandler::TrustedServerDiscovery",
        "handler: NamedRouteHandler::VerifySignature",
        1,
    );
    assert!(extract_named_routes("fastly", &fastly_swap).is_err());

    let axum_swap = axum.replacen(
        "handler: NamedRouteHandler::TrustedServerDiscovery",
        "handler: NamedRouteHandler::VerifySignature",
        1,
    );
    assert!(extract_named_routes("axum", &axum_swap).is_err());

    let spin_swap = spin.replacen(
        ".get(\"/.well-known/trusted-server.json\", discovery_handler)",
        ".get(\"/.well-known/trusted-server.json\", verify_handler)",
        1,
    );
    assert!(
        extract_repository_routes(&RouteSources {
            publisher_routes: include_str!("../../../crates/trusted-server-core/src/publisher.rs"),
            admin_routes: include_str!("../../../crates/trusted-server-core/src/ec/admin.rs"),
            fastly_app: fastly,
            fastly_entrypoint: include_str!(
                "../../../crates/trusted-server-adapter-fastly/src/main.rs"
            ),
            axum_app: axum,
            cloudflare_app: cloudflare,
            spin_app: &spin_swap,
        })
        .is_err()
    );

    let cloudflare_swap = cloudflare.replacen(
        "handle_trusted_server_discovery(&s.settings, &services, req)",
        "handle_verify_signature(&s.settings, &services, req)",
        1,
    );
    assert!(extract_cloudflare_routes(&cloudflare_swap).is_err());

    let cloudflare_preflight = cloudflare.replace(
        "router = router.route(path, Method::OPTIONS, page_bids_preflight.clone());",
        "router = router.route(path, Method::OPTIONS, page_bids.clone());",
    );
    assert!(extract_cloudflare_routes(&cloudflare_preflight).is_err());

    let cloudflare_legacy =
        cloudflare.replacen("legacy_admin_deny.clone(),", "fallback.clone(),", 1);
    assert!(extract_cloudflare_routes(&cloudflare_legacy).is_err());
}

#[test]
fn route_extractors_ignore_cfg_test_decoys_and_reject_other_collection_cfgs() {
    let named = r#"
        #[cfg(test)]
        const NAMED_ROUTES: &[NamedRoute] = &[
            NamedRoute { path: "/bad", primary_methods: &[Method::GET], handler: NamedRouteHandler::VerifySignature },
        ];
        const NAMED_ROUTES: &[NamedRoute] = &[
            NamedRoute { path: "/.well-known/trusted-server.json", primary_methods: &[Method::GET], handler: NamedRouteHandler::TrustedServerDiscovery },
        ];
    "#;
    let routes = extract_named_routes("fastly", named)
        .expect("cfg(test) named collections must not own production evidence");
    assert!(routes.iter().all(|route| route.path != "/bad"));
    assert!(
        extract_named_routes("fastly", &named.replace("#[cfg(test)]", "#[cfg(unix)]")).is_err()
    );

    let axum = r#"
        #[cfg(test)]
        fn named_routes() -> [NamedRoute; 1] {
            [NamedRoute { path: "/bad", primary_methods: &[Method::GET], handler: NamedRouteHandler::VerifySignature }]
        }
        fn named_routes() -> [NamedRoute; 1] {
            [NamedRoute { path: "/.well-known/trusted-server.json", primary_methods: &[Method::GET], handler: NamedRouteHandler::TrustedServerDiscovery }]
        }
    "#;
    let routes = extract_named_routes("axum", axum)
        .expect("cfg(test) Axum named collections must be excluded");
    assert!(routes.iter().all(|route| route.path != "/bad"));

    let spin = r#"
        #[cfg(test)]
        fn named_fallback_paths() -> [(&'static str, &'static [Method]); 1] {
            [("/bad", &[Method::GET])]
        }
        fn named_fallback_paths() -> [(&'static str, &'static [Method]); 1] {
            [("/.well-known/trusted-server.json", &[Method::GET])]
        }
    "#;
    let routes = extract_named_routes("spin", spin)
        .expect("cfg(test) Spin named collections must be excluded");
    assert!(routes.iter().all(|route| route.path != "/bad"));

    let cloudflare = r#"
        #[cfg(test)]
        fn build_router() { RouterService::builder().get("/bad", handler).build() }
        fn build_router() { RouterService::builder().get("/.well-known/trusted-server.json", discovery).build() }
    "#;
    let routes = extract_cloudflare_routes(cloudflare)
        .expect("cfg(test) Cloudflare builders must not own production evidence");
    assert!(routes.iter().all(|route| route.path != "/bad"));
    assert!(
        extract_cloudflare_routes(&cloudflare.replace("#[cfg(test)]", "#[cfg(unix)]")).is_err()
    );
}

#[test]
fn repository_route_extraction_ignores_cfg_test_method_inventory_decoys() {
    let fastly_app = include_str!("../../../crates/trusted-server-adapter-fastly/src/app.rs");
    let entry = include_str!("../../../crates/trusted-server-adapter-fastly/src/main.rs");
    let axum = include_str!("../../../crates/trusted-server-adapter-axum/src/app.rs");
    let cloudflare = include_str!("../../../crates/trusted-server-adapter-cloudflare/src/app.rs");
    let spin = include_str!("../../../crates/trusted-server-adapter-spin/src/app.rs");
    let method_decoy =
        "\n#[cfg(test)]\nfn publisher_fallback_methods() -> [Method; 1] { [Method::TRACE] }\n";
    let constant_decoy =
        "\n#[cfg(test)]\nconst LEGACY_ADMIN_DENY_METHODS: &[Method] = &[Method::TRACE];\n";

    for (fastly_variant, axum_variant, cloudflare_variant, spin_variant) in [
        (
            format!("{fastly_app}{method_decoy}{constant_decoy}"),
            axum.to_owned(),
            cloudflare.to_owned(),
            spin.to_owned(),
        ),
        (
            fastly_app.to_owned(),
            format!("{axum}{method_decoy}{constant_decoy}"),
            cloudflare.to_owned(),
            spin.to_owned(),
        ),
        (
            fastly_app.to_owned(),
            axum.to_owned(),
            format!("{cloudflare}{method_decoy}"),
            spin.to_owned(),
        ),
        (
            fastly_app.to_owned(),
            axum.to_owned(),
            cloudflare.to_owned(),
            format!("{spin}{method_decoy}{constant_decoy}"),
        ),
    ] {
        extract_repository_routes(&RouteSources {
            publisher_routes: include_str!("../../../crates/trusted-server-core/src/publisher.rs"),
            admin_routes: include_str!("../../../crates/trusted-server-core/src/ec/admin.rs"),
            fastly_app: &fastly_variant,
            fastly_entrypoint: entry,
            axum_app: &axum_variant,
            cloudflare_app: &cloudflare_variant,
            spin_app: &spin_variant,
        })
        .expect("cfg(test) method inventories must not alter production route evidence");
    }
}

#[test]
fn route_identity_rejects_conflicting_metadata_assignments() {
    let base = r#"
        version = 1
        reviewed = true
        [[routes]]
        adapters = ["fastly"]
        path = "/same"
        methods = ["GET"]
        shape = "literal"
        predicate = "always"
        status = "real"
        startup_router = false
    "#;
    for conflict in [
        "shape = \"template\"",
        "predicate = \"settings.enabled\"",
        "status = \"guarded\"",
    ] {
        let second = base
            .replace("shape = \"literal\"", conflict)
            .replace("version = 1\n        reviewed = true", "");
        let source = format!("{base}\n{second}");
        assert!(
            RouteManifest::parse(&source)
                .expect_err("raw route identity cannot be assigned twice")
                .to_string()
                .contains("duplicate")
        );
    }
}

#[test]
fn route_parsers_enforce_four_mibibyte_input_bounds() {
    const LIMIT: usize = 4 * 1024 * 1024;
    let manifest = "version = 1\nreviewed = true\n";
    let exact_manifest = format!("{manifest}#{}", " ".repeat(LIMIT - manifest.len() - 1));
    RouteManifest::parse(&exact_manifest).expect("an exact-limit manifest must parse");
    assert!(
        RouteManifest::parse(&format!("{exact_manifest} "))
            .expect_err("an oversized manifest must fail")
            .to_string()
            .contains("4 MiB")
    );

    let rust = "fn build_router() { let actual = RouterService::builder().get(\"/ok\", handler); actual.build() }";
    let exact_rust = format!("{rust}/*{}*/", " ".repeat(LIMIT - rust.len() - 4));
    extract_cloudflare_routes(&exact_rust).expect("an exact-limit Rust source must parse");
    assert!(
        extract_cloudflare_routes(&format!("{exact_rust} "))
            .expect_err("an oversized Rust source must fail")
            .to_string()
            .contains("4 MiB")
    );
}

#[test]
fn route_manifest_caps_rows_lists_and_strings() {
    let adapters = (0..=4096)
        .map(|_| "\"fastly\"")
        .collect::<Vec<_>>()
        .join(",");
    let source = format!(
        r#"
        version = 1
        reviewed = true
        [[routes]]
        adapters = [{adapters}]
        path = "/ok"
        methods = ["GET"]
        shape = "literal"
        predicate = "always"
        status = "real"
    "#
    );
    assert!(
        RouteManifest::parse(&source)
            .expect_err("oversized route lists must fail")
            .to_string()
            .contains("cardinality")
    );
    let long = "x".repeat(16 * 1024 + 1);
    let source = source
        .replace(&format!("[{adapters}]"), "[\"fastly\"]")
        .replace("/ok", &long);
    assert!(
        RouteManifest::parse(&source)
            .expect_err("oversized route strings must fail")
            .to_string()
            .contains("string")
    );
}

#[test]
fn cloudflare_route_affecting_local_and_macro_constructs_fail_closed() {
    for source in [
        r#"
            fn build_router() {
                let mut router = RouterService::builder().get("/known", handler);
                let _receipt = register_hidden(&mut router, "/hidden");
                router.build()
            }
        "#,
        r#"
            fn build_router() {
                let mut router = RouterService::builder().get("/known", handler);
                register_routes!(router, "/hidden");
                router.build()
            }
        "#,
    ] {
        assert!(
            extract_cloudflare_routes(source)
                .expect_err("unknown route-affecting syntax must fail closed")
                .to_string()
                .contains("Cloudflare")
        );
    }
}

#[test]
fn repository_cloudflare_builder_is_accepted_by_the_closed_grammar() {
    let source = with_publisher_path_constants(include_str!(
        "../../../crates/trusted-server-adapter-cloudflare/src/app.rs"
    ));
    let routes = extract_cloudflare_routes(&source)
        .expect("the production Cloudflare builder must stay inside the closed grammar");
    assert_eq!(
        routes.len(),
        20,
        "16 named paths, split page-bids semantics, and root/rest fallback"
    );
}

#[test]
fn repository_private_named_collections_have_the_prechange_route_sets() {
    let fastly_source = with_publisher_path_constants(include_str!(
        "../../../crates/trusted-server-adapter-fastly/src/app.rs"
    ));
    let axum_source = with_publisher_path_constants(include_str!(
        "../../../crates/trusted-server-adapter-axum/src/app.rs"
    ));
    let spin_source = with_publisher_path_constants(include_str!(
        "../../../crates/trusted-server-adapter-spin/src/app.rs"
    ));
    let fastly = extract_named_routes("fastly", &fastly_source)
        .expect("Fastly named route collection should parse");
    let axum = extract_named_routes("axum", &axum_source)
        .expect("Axum named route collection should parse");
    let spin = extract_named_routes("spin", &spin_source)
        .expect("Spin named route collection should parse");

    assert_eq!(fastly.len(), 22);
    assert_eq!(axum.len(), 18);
    assert_eq!(spin.len(), 18);
    assert_eq!(
        fastly
            .iter()
            .filter(|record| record.path.starts_with("/_ts/api/v1/")
                || record.path.starts_with("/_ts/set-")
                || record.path.starts_with("/_ts/clear-"))
            .count(),
        4,
        "the Fastly-only EC and tester routes are the exact four-path delta"
    );
    let fastly_shared = fastly
        .iter()
        .filter(|record| {
            !record.path.starts_with("/_ts/api/v1/")
                && !record.path.starts_with("/_ts/set-")
                && !record.path.starts_with("/_ts/clear-")
        })
        .map(|record| (record.path.clone(), record.methods.clone()))
        .collect::<BTreeSet<_>>();
    let axum_paths = axum
        .iter()
        .map(|record| (record.path.clone(), record.methods.clone()))
        .collect::<BTreeSet<_>>();
    let spin_paths = spin
        .iter()
        .map(|record| (record.path.clone(), record.methods.clone()))
        .collect::<BTreeSet<_>>();
    assert_eq!(fastly_shared, axum_paths);
    assert_eq!(axum_paths, spin_paths);
}

#[test]
fn reviewed_route_and_adapter_support_manifests_equal_all_repository_surfaces() {
    let sources = RouteSources {
        publisher_routes: include_str!("../../../crates/trusted-server-core/src/publisher.rs"),
        admin_routes: include_str!("../../../crates/trusted-server-core/src/ec/admin.rs"),
        fastly_app: include_str!("../../../crates/trusted-server-adapter-fastly/src/app.rs"),
        fastly_entrypoint: include_str!(
            "../../../crates/trusted-server-adapter-fastly/src/main.rs"
        ),
        axum_app: include_str!("../../../crates/trusted-server-adapter-axum/src/app.rs"),
        cloudflare_app: include_str!(
            "../../../crates/trusted-server-adapter-cloudflare/src/app.rs"
        ),
        spin_app: include_str!("../../../crates/trusted-server-adapter-spin/src/app.rs"),
    };
    let observed = extract_repository_routes(&sources)
        .expect("all adapter route source shapes should remain supported");
    assert_eq!(
        observed.len(),
        103,
        "the expanded adapter route set is exact"
    );
    let expected = RouteManifest::parse(include_str!("../manifests/routes.toml"))
        .expect("checked route manifest should parse");
    assert_eq!(expected.routes().len(), 103);
    validate_routes(expected.routes(), &observed)
        .expect("source route set should equal the checked inventory");

    let support = AdapterSupportManifest::parse(include_str!("../manifests/adapter-support.toml"))
        .expect("checked adapter support manifest should parse");
    validate_adapter_support(&support).expect("support records must cover all adapters exactly");
}

fn extract_with_mutation(
    fastly_app: &str,
    fastly_entrypoint: &str,
    axum_app: &str,
    cloudflare_app: &str,
    spin_app: &str,
) -> Result<BTreeSet<RouteRecord>, String> {
    extract_repository_routes(&RouteSources {
        publisher_routes: include_str!("../../../crates/trusted-server-core/src/publisher.rs"),
        admin_routes: include_str!("../../../crates/trusted-server-core/src/ec/admin.rs"),
        fastly_app,
        fastly_entrypoint,
        axum_app,
        cloudflare_app,
        spin_app,
    })
    .map_err(|error| error.to_string())
}

fn extract_with_core_mutation(
    publisher_routes: &str,
    admin_routes: &str,
    fastly_app: &str,
    fastly_entrypoint: &str,
    axum_app: &str,
    cloudflare_app: &str,
    spin_app: &str,
) -> Result<BTreeSet<RouteRecord>, String> {
    extract_repository_routes(&RouteSources {
        publisher_routes,
        admin_routes,
        fastly_app,
        fastly_entrypoint,
        axum_app,
        cloudflare_app,
        spin_app,
    })
    .map_err(|error| error.to_string())
}

#[test]
fn fastly_main_health_dispatch_rejects_a_preceding_terminal_branch() {
    let fastly = include_str!("../../../crates/trusted-server-adapter-fastly/src/app.rs");
    let entry = include_str!("../../../crates/trusted-server-adapter-fastly/src/main.rs");
    let axum = include_str!("../../../crates/trusted-server-adapter-axum/src/app.rs");
    let cloudflare = include_str!("../../../crates/trusted-server-adapter-cloudflare/src/app.rs");
    let spin = include_str!("../../../crates/trusted-server-adapter-spin/src/app.rs");
    let changed = entry.replacen(
        "    if let Some(response) = health_response(&req) {",
        "    if req.get_path() == \"/health\" { return; }\n    if let Some(response) = health_response(&req) {",
        1,
    );
    assert_ne!(changed, entry, "fixture must alter Fastly main");
    assert!(
        extract_with_mutation(fastly, &changed, axum, cloudflare, spin).is_err(),
        "health_response must be the first reachable health terminal after receiving req"
    );
}

#[test]
fn router_construction_rejects_reachable_bypasses_on_every_adapter() {
    let fastly = include_str!("../../../crates/trusted-server-adapter-fastly/src/app.rs");
    let entry = include_str!("../../../crates/trusted-server-adapter-fastly/src/main.rs");
    let axum = include_str!("../../../crates/trusted-server-adapter-axum/src/app.rs");
    let cloudflare = include_str!("../../../crates/trusted-server-adapter-cloudflare/src/app.rs");
    let spin = include_str!("../../../crates/trusted-server-adapter-spin/src/app.rs");

    let fastly_healthy = fastly.replacen(
        "        for route in NAMED_ROUTES {",
        "        if true { return router.build(); }\n        for route in NAMED_ROUTES {",
        1,
    );
    assert!(extract_with_mutation(&fastly_healthy, entry, axum, cloudflare, spin).is_err());
    let axum_healthy = axum.replacen(
        "    router = router.route(\"/health\"",
        "    if true { return router.build(); }\n    router = router.route(\"/health\"",
        1,
    );
    assert!(extract_with_mutation(fastly, entry, &axum_healthy, cloudflare, spin).is_err());
    let cloudflare_healthy = cloudflare.replacen(
        "        let page_bids = make_handler(",
        "        if true { return router.build(); }\n        let page_bids = make_handler(",
        1,
    );
    assert!(extract_with_mutation(fastly, entry, axum, &cloudflare_healthy, spin).is_err());
    let spin_healthy = spin.replacen(
        "        for (path, primary_methods) in named_fallback_paths() {",
        "        if true { return builder.build(); }\n        for (path, primary_methods) in named_fallback_paths() {",
        1,
    );
    assert!(extract_with_mutation(fastly, entry, axum, cloudflare, &spin_healthy).is_err());

    let fastly_startup = fastly.replacen(
        "    for method in publisher_fallback_methods() {",
        "    if true { return router.build(); }\n    for method in publisher_fallback_methods() {",
        1,
    );
    assert!(extract_with_mutation(&fastly_startup, entry, axum, cloudflare, spin).is_err());
    let axum_startup = axum.replacen(
        "    for method in publisher_fallback_methods() {",
        "    if true { return router.build(); }\n    for method in publisher_fallback_methods() {",
        1,
    );
    assert!(extract_with_mutation(fastly, entry, &axum_startup, cloudflare, spin).is_err());
    let cloudflare_startup = cloudflare.replacen(
        "    for method in publisher_fallback_methods() {",
        "    if true { return router.build(); }\n    for method in publisher_fallback_methods() {",
        1,
    );
    assert!(extract_with_mutation(fastly, entry, axum, &cloudflare_startup, spin).is_err());
    let spin_startup = spin.replacen(
        "    for method in publisher_fallback_methods() {",
        "    if true { return builder.build(); }\n    for method in publisher_fallback_methods() {",
        1,
    );
    assert!(extract_with_mutation(fastly, entry, axum, cloudflare, &spin_startup).is_err());
}

#[test]
fn named_route_inventories_must_feed_the_live_registration_authority() {
    let fastly = include_str!("../../../crates/trusted-server-adapter-fastly/src/app.rs");
    let entry = include_str!("../../../crates/trusted-server-adapter-fastly/src/main.rs");
    let axum = include_str!("../../../crates/trusted-server-adapter-axum/src/app.rs");
    let cloudflare = include_str!("../../../crates/trusted-server-adapter-cloudflare/src/app.rs");
    let spin = include_str!("../../../crates/trusted-server-adapter-spin/src/app.rs");

    let fastly_empty = fastly.replacen(
        "        for route in NAMED_ROUTES {",
        "        let _unused = NAMED_ROUTES;\n        for route in std::iter::empty::<&NamedRoute>() {",
        1,
    );
    assert!(extract_with_mutation(&fastly_empty, entry, axum, cloudflare, spin).is_err());
    let fastly_wrong_path = fastly.replacen(
        "                    route.path,\n                    method.clone(),\n                    named_route_handler(Arc::clone(state), route.handler),",
        "                    \"/decoy\",\n                    method.clone(),\n                    named_route_handler(Arc::clone(state), route.handler),",
        1,
    );
    assert!(extract_with_mutation(&fastly_wrong_path, entry, axum, cloudflare, spin).is_err());
    let fastly_wrong_handler = fastly.replacen(
        "named_route_handler(Arc::clone(state), route.handler)",
        "named_route_handler(Arc::clone(state), NamedRouteHandler::Auction)",
        1,
    );
    assert!(extract_with_mutation(&fastly_wrong_handler, entry, axum, cloudflare, spin).is_err());
    let axum_empty = axum.replacen(
        "    for route in named_routes() {",
        "    let _unused = named_routes();\n    for route in std::iter::empty::<NamedRoute>() {",
        1,
    );
    assert!(extract_with_mutation(fastly, entry, &axum_empty, cloudflare, spin).is_err());
    let axum_wrong_method = axum.replacen(
        "                route.path,\n                method.clone(),\n                named_route_handler(Arc::clone(state), route.handler),",
        "                route.path,\n                Method::GET,\n                named_route_handler(Arc::clone(state), route.handler),",
        1,
    );
    assert!(extract_with_mutation(fastly, entry, &axum_wrong_method, cloudflare, spin).is_err());
    let axum_wrong_handler = axum.replacen(
        "named_route_handler(Arc::clone(state), route.handler)",
        "named_route_handler(Arc::clone(state), NamedRouteHandler::Auction)",
        1,
    );
    assert!(extract_with_mutation(fastly, entry, &axum_wrong_handler, cloudflare, spin).is_err());
    let spin_empty = spin.replacen(
        "        for (path, primary_methods) in named_fallback_paths() {",
        "        let _unused = named_fallback_paths();\n        for (path, primary_methods) in std::iter::empty::<(&str, &[Method])>() {",
        1,
    );
    assert!(extract_with_mutation(fastly, entry, axum, cloudflare, &spin_empty).is_err());
}

#[test]
fn named_handler_authorities_require_closed_reachable_exact_calls() {
    let fastly = include_str!("../../../crates/trusted-server-adapter-fastly/src/app.rs");
    let entry = include_str!("../../../crates/trusted-server-adapter-fastly/src/main.rs");
    let axum = include_str!("../../../crates/trusted-server-adapter-axum/src/app.rs");
    let cloudflare = include_str!("../../../crates/trusted-server-adapter-cloudflare/src/app.rs");
    let spin = include_str!("../../../crates/trusted-server-adapter-spin/src/app.rs");

    let fastly_early = fastly.replacen(
        "        NamedRouteHandler::TrustedServerDiscovery => {\n            handle_trusted_server_discovery(&state.settings, services, req)",
        "        NamedRouteHandler::TrustedServerDiscovery => {\n            return handle_verify_signature(&state.settings, services, req);\n            handle_trusted_server_discovery(&state.settings, services, req)",
        1,
    );
    assert_ne!(
        fastly_early, fastly,
        "fixture must alter Fastly discovery arm"
    );
    assert!(extract_with_mutation(&fastly_early, entry, axum, cloudflare, spin).is_err());
    let fastly_swapped = fastly.replacen(
        "handle_trusted_server_discovery(&state.settings, services, req)",
        "handle_trusted_server_discovery(services, &state.settings, req)",
        1,
    );
    assert_ne!(fastly_swapped, fastly, "fixture must swap Fastly arguments");
    assert!(extract_with_mutation(&fastly_swapped, entry, axum, cloudflare, spin).is_err());

    let axum_early = axum.replacen(
        "                    NamedRouteHandler::TrustedServerDiscovery => {\n                        handle_trusted_server_discovery(&state.settings, &services, req)",
        "                    NamedRouteHandler::TrustedServerDiscovery => {\n                        return handle_verify_signature(&state.settings, &services, req);\n                        handle_trusted_server_discovery(&state.settings, &services, req)",
        1,
    );
    assert_ne!(axum_early, axum, "fixture must alter Axum discovery arm");
    assert!(extract_with_mutation(fastly, entry, &axum_early, cloudflare, spin).is_err());
    let axum_swapped = axum.replacen(
        "handle_trusted_server_discovery(&state.settings, &services, req)",
        "handle_trusted_server_discovery(&services, &state.settings, req)",
        1,
    );
    assert_ne!(axum_swapped, axum, "fixture must swap Axum arguments");
    assert!(extract_with_mutation(fastly, entry, &axum_swapped, cloudflare, spin).is_err());

    let cloudflare_early = cloudflare.replacen(
        "                    handle_trusted_server_discovery(&s.settings, &services, req)",
        "                    return handle_verify_signature(&s.settings, &services, req);\n                    handle_trusted_server_discovery(&s.settings, &services, req)",
        1,
    );
    assert_ne!(
        cloudflare_early, cloudflare,
        "fixture must alter Cloudflare discovery closure"
    );
    assert!(extract_with_mutation(fastly, entry, axum, &cloudflare_early, spin).is_err());
    let cloudflare_swapped = cloudflare.replacen(
        "handle_trusted_server_discovery(&s.settings, &services, req)",
        "handle_trusted_server_discovery(&services, &s.settings, req)",
        1,
    );
    assert_ne!(
        cloudflare_swapped, cloudflare,
        "fixture must swap Cloudflare arguments"
    );
    assert!(extract_with_mutation(fastly, entry, axum, &cloudflare_swapped, spin).is_err());

    let spin_early = spin.replacen(
        "                Ok(handle_trusted_server_discovery(&s.settings, &services, req)",
        "                return Ok(handle_verify_signature(&s.settings, &services, req).unwrap_or_else(|e| http_error(&e)));\n                Ok(handle_trusted_server_discovery(&s.settings, &services, req)",
        1,
    );
    assert_ne!(
        spin_early, spin,
        "fixture must alter Spin discovery closure"
    );
    assert!(extract_with_mutation(fastly, entry, axum, cloudflare, &spin_early).is_err());
    let spin_swapped = spin.replacen(
        "handle_trusted_server_discovery(&s.settings, &services, req)",
        "handle_trusted_server_discovery(&services, &s.settings, req)",
        1,
    );
    assert_ne!(spin_swapped, spin, "fixture must swap Spin arguments");
    assert!(extract_with_mutation(fastly, entry, axum, cloudflare, &spin_swapped).is_err());
}

#[test]
fn route_blocks_reject_block_scoped_fallback_import_shadows() {
    let fastly = include_str!("../../../crates/trusted-server-adapter-fastly/src/app.rs");
    let entry = include_str!("../../../crates/trusted-server-adapter-fastly/src/main.rs");
    let axum = include_str!("../../../crates/trusted-server-adapter-axum/src/app.rs");
    let cloudflare = include_str!("../../../crates/trusted-server-adapter-cloudflare/src/app.rs");
    let spin = include_str!("../../../crates/trusted-server-adapter-spin/src/app.rs");
    let decoy = "\nmod decoy { fn publisher_fallback_methods() -> [Method; 0] { [] } }";
    let with_decoy = |source: String| format!("{source}{decoy}");

    let fastly_healthy = with_decoy(fastly.replacen(
        "    fn routes_for_state(state: &Arc<AppState>) -> RouterService {\n        let mut router",
        "    fn routes_for_state(state: &Arc<AppState>) -> RouterService {\n        use decoy::publisher_fallback_methods;\n        let mut router",
        1,
    ));
    assert!(extract_with_mutation(&fastly_healthy, entry, axum, cloudflare, spin).is_err());
    let axum_healthy = with_decoy(axum.replacen(
        "fn build_router(state: &Arc<AppState>) -> RouterService {\n    let fallback",
        "fn build_router(state: &Arc<AppState>) -> RouterService {\n    use decoy::publisher_fallback_methods;\n    let fallback",
        1,
    ));
    assert!(extract_with_mutation(fastly, entry, &axum_healthy, cloudflare, spin).is_err());
    let cloudflare_healthy = with_decoy(cloudflare.replacen(
        "fn build_router(state: &Arc<AppState>) -> RouterService {\n    {\n        let state",
        "fn build_router(state: &Arc<AppState>) -> RouterService {\n    {\n        use decoy::publisher_fallback_methods;\n        let state",
        1,
    ));
    assert!(extract_with_mutation(fastly, entry, axum, &cloudflare_healthy, spin).is_err());
    let spin_healthy = with_decoy(spin.replacen(
        "fn build_router(state: &Arc<AppState>) -> RouterService {\n    {\n        let state",
        "fn build_router(state: &Arc<AppState>) -> RouterService {\n    {\n        use decoy::publisher_fallback_methods;\n        let state",
        1,
    ));
    assert!(extract_with_mutation(fastly, entry, axum, cloudflare, &spin_healthy).is_err());

    let fastly_startup = with_decoy(fastly.replacen(
        "    for method in publisher_fallback_methods() {",
        "    use decoy::publisher_fallback_methods;\n    for method in publisher_fallback_methods() {",
        1,
    ));
    assert!(extract_with_mutation(&fastly_startup, entry, axum, cloudflare, spin).is_err());
    let axum_startup = with_decoy(axum.replacen(
        "    for method in publisher_fallback_methods() {",
        "    use decoy::publisher_fallback_methods;\n    for method in publisher_fallback_methods() {",
        1,
    ));
    assert!(extract_with_mutation(fastly, entry, &axum_startup, cloudflare, spin).is_err());
    let cloudflare_startup = with_decoy(cloudflare.replacen(
        "    for method in publisher_fallback_methods() {",
        "    use decoy::publisher_fallback_methods;\n    for method in publisher_fallback_methods() {",
        1,
    ));
    assert!(extract_with_mutation(fastly, entry, axum, &cloudflare_startup, spin).is_err());
    let spin_startup = with_decoy(spin.replacen(
        "    for method in publisher_fallback_methods() {",
        "    use decoy::publisher_fallback_methods;\n    for method in publisher_fallback_methods() {",
        1,
    ));
    assert!(extract_with_mutation(fastly, entry, axum, cloudflare, &spin_startup).is_err());
}

#[test]
fn registered_handler_bodies_reject_preceding_terminal_paths() {
    let fastly = include_str!("../../../crates/trusted-server-adapter-fastly/src/app.rs");
    let entry = include_str!("../../../crates/trusted-server-adapter-fastly/src/main.rs");
    let axum = include_str!("../../../crates/trusted-server-adapter-axum/src/app.rs");
    let cloudflare = include_str!("../../../crates/trusted-server-adapter-cloudflare/src/app.rs");
    let spin = include_str!("../../../crates/trusted-server-adapter-spin/src/app.rs");

    let axum_early = axum.replacen(
        "                    NamedRouteHandler::VerifySignature => {\n                        handle_verify_signature(&state.settings, &services, req)",
        "                    NamedRouteHandler::VerifySignature => {\n                        if true { return Ok(legacy_admin_alias_denied()); }\n                        handle_verify_signature(&state.settings, &services, req)",
        1,
    );
    assert_ne!(axum_early, axum, "fixture must alter Axum verify arm");
    assert!(extract_with_mutation(fastly, entry, &axum_early, cloudflare, spin).is_err());

    let cloudflare_early = cloudflare.replacen(
        "                Ok(page_bids_preflight_denied())",
        "                if true { return Ok(admin_key_management_not_supported()); }\n                Ok(page_bids_preflight_denied())",
        1,
    );
    assert_ne!(
        cloudflare_early, cloudflare,
        "fixture must alter Cloudflare page-bids OPTIONS"
    );
    assert!(extract_with_mutation(fastly, entry, axum, &cloudflare_early, spin).is_err());

    let spin_early = spin.replacen(
        "            Ok::<Response, EdgeError>(page_bids_preflight_denied())",
        "            if true { return Ok::<Response, EdgeError>(admin_key_management_not_supported()); }\n            Ok::<Response, EdgeError>(page_bids_preflight_denied())",
        1,
    );
    assert_ne!(
        spin_early, spin,
        "fixture must alter Spin page-bids OPTIONS"
    );
    assert!(extract_with_mutation(fastly, entry, axum, cloudflare, &spin_early).is_err());
}

#[test]
fn route_ast_receipts_reject_tsjs_or_and_unrelated_predicates() {
    let fastly_app = include_str!("../../../crates/trusted-server-adapter-fastly/src/app.rs");
    let fastly_entrypoint =
        include_str!("../../../crates/trusted-server-adapter-fastly/src/main.rs");
    let axum = include_str!("../../../crates/trusted-server-adapter-axum/src/app.rs");
    let cloudflare = include_str!("../../../crates/trusted-server-adapter-cloudflare/src/app.rs");
    let spin = include_str!("../../../crates/trusted-server-adapter-spin/src/app.rs");
    let get_or_head = axum.replace(
        "method == Method::GET && path.starts_with(\"/static/tsjs=\")",
        "matches!(method, Method::GET | Method::HEAD) && path.starts_with(\"/static/tsjs=\")",
    );
    assert!(
        extract_with_mutation(
            fastly_app,
            fastly_entrypoint,
            &get_or_head,
            cloudflare,
            spin
        )
        .is_err()
    );
    let unrelated = axum.replace(
        "if method == Method::GET && path.starts_with(\"/static/tsjs=\") {",
        "let _unrelated_get = method == Method::GET;\n    if path.starts_with(\"/static/tsjs=\") {",
    );
    assert!(
        extract_with_mutation(fastly_app, fastly_entrypoint, &unrelated, cloudflare, spin).is_err()
    );
}

#[test]
fn route_ast_receipts_reject_health_ja4_asset_and_comment_decoys() {
    let fastly_app = include_str!("../../../crates/trusted-server-adapter-fastly/src/app.rs");
    let entry = include_str!("../../../crates/trusted-server-adapter-fastly/src/main.rs");
    let axum = include_str!("../../../crates/trusted-server-adapter-axum/src/app.rs");
    let cloudflare = include_str!("../../../crates/trusted-server-adapter-cloudflare/src/app.rs");
    let spin = include_str!("../../../crates/trusted-server-adapter-spin/src/app.rs");
    let health_path = entry.replacen(
        "req.get_path() == \"/health\"",
        "req.get_path() == \"/live\" // req.get_path() == \"/health\"",
        1,
    );
    assert!(extract_with_mutation(fastly_app, &health_path, axum, cloudflare, spin).is_err());
    let health_status = entry.replacen("from_status(200)", "from_status(201)", 1);
    assert!(extract_with_mutation(fastly_app, &health_status, axum, cloudflare, spin).is_err());
    let ja4 = entry.replace(
        "Ok(settings) if settings.debug.ja4_endpoint_enabled =>",
        "Ok(settings) if true =>",
    ) + "\nfn unrelated(settings: &Settings) { let _ = settings.debug.ja4_endpoint_enabled; }\n";
    assert!(extract_with_mutation(fastly_app, &ja4, axum, cloudflare, spin).is_err());
    let asset = fastly_app.replacen(
        "matches!(method, Method::GET | Method::HEAD)",
        "matches!(method, Method::GET)",
        1,
    );
    assert!(extract_with_mutation(&asset, entry, axum, cloudflare, spin).is_err());
}

#[test]
fn route_ast_receipts_reject_fallback_handler_path_and_startup_status_mutations() {
    let fastly_app = include_str!("../../../crates/trusted-server-adapter-fastly/src/app.rs");
    let entry = include_str!("../../../crates/trusted-server-adapter-fastly/src/main.rs");
    let axum = include_str!("../../../crates/trusted-server-adapter-axum/src/app.rs");
    let cloudflare = include_str!("../../../crates/trusted-server-adapter-cloudflare/src/app.rs");
    let spin = include_str!("../../../crates/trusted-server-adapter-spin/src/app.rs");
    let fallback_path = cloudflare.replace(
        "router = router.route(\"/\", method.clone(), fallback.clone());",
        "router = router.route(\"/changed\", method.clone(), fallback.clone());",
    );
    assert!(extract_with_mutation(fastly_app, entry, axum, &fallback_path, spin).is_err());
    let fallback_handler = cloudflare.replace(
        "router = router.route(\"/\", method.clone(), fallback.clone());",
        "router = router.route(\"/\", method.clone(), legacy_admin_deny.clone());",
    );
    assert!(extract_with_mutation(fastly_app, entry, axum, &fallback_handler, spin).is_err());
    let startup_status = spin.replace("StatusCode::SERVICE_UNAVAILABLE", "StatusCode::BAD_GATEWAY");
    assert!(extract_with_mutation(fastly_app, entry, axum, cloudflare, &startup_status).is_err());
}

#[test]
fn route_ast_receipts_reject_dead_or_unused_route_registrations() {
    let fastly_app = include_str!("../../../crates/trusted-server-adapter-fastly/src/app.rs");
    let entry = include_str!("../../../crates/trusted-server-adapter-fastly/src/main.rs");
    let axum = include_str!("../../../crates/trusted-server-adapter-axum/src/app.rs");
    let cloudflare = include_str!("../../../crates/trusted-server-adapter-cloudflare/src/app.rs");
    let spin = include_str!("../../../crates/trusted-server-adapter-spin/src/app.rs");

    let dead_health = axum
        .replacen(
            "    router = router.route(\"/health\", Method::GET,",
            "    if false { router = router.route(\"/health\", Method::GET,",
            1,
        )
        .replacen(
            "    });\n\n    for route in named_routes()",
            "    }); }\n\n    for route in named_routes()",
            1,
        );
    assert!(extract_with_mutation(fastly_app, entry, &dead_health, cloudflare, spin).is_err());

    let unused_fallback = cloudflare.replace(
        r#"        for method in publisher_fallback_methods() {
            router = router.route("/", method.clone(), fallback.clone());
            router = router.route("/{*rest}", method, fallback.clone());
        }"#,
        r#"        let _unused = || {
            for method in publisher_fallback_methods() {
                router = router.route("/", method.clone(), fallback.clone());
                router = router.route("/{*rest}", method, fallback.clone());
            }
        };"#,
    );
    assert!(extract_with_mutation(fastly_app, entry, axum, &unused_fallback, spin).is_err());
}

#[test]
fn route_ast_receipts_bind_health_handlers_and_cross_surface_identity() {
    let fastly_app = include_str!("../../../crates/trusted-server-adapter-fastly/src/app.rs");
    let entry = include_str!("../../../crates/trusted-server-adapter-fastly/src/main.rs");
    let axum = include_str!("../../../crates/trusted-server-adapter-axum/src/app.rs");
    let cloudflare = include_str!("../../../crates/trusted-server-adapter-cloudflare/src/app.rs");
    let spin = include_str!("../../../crates/trusted-server-adapter-spin/src/app.rs");

    let axum_status = axum.replacen(".status(StatusCode::OK)", ".status(StatusCode::CREATED)", 1);
    assert!(extract_with_mutation(fastly_app, entry, &axum_status, cloudflare, spin).is_err());
    let spin_body = spin.replacen(
        "Response::new(edgezero_core::body::Body::from(\"ok\"))",
        "Response::new(edgezero_core::body::Body::from(\"changed\"))",
        1,
    );
    assert!(extract_with_mutation(fastly_app, entry, axum, cloudflare, &spin_body).is_err());

    let duplicate_named_health = fastly_app.replacen(
        "const NAMED_ROUTES: &[NamedRoute] = &[",
        r#"const NAMED_ROUTES: &[NamedRoute] = &[
    NamedRoute {
        path: "/health",
        primary_methods: &[Method::GET],
        handler: NamedRouteHandler::TrustedServerDiscovery,
    },"#,
        1,
    );
    assert!(
        extract_with_mutation(&duplicate_named_health, entry, axum, cloudflare, spin).is_err(),
        "a named route must not duplicate the identity of a synthetic route"
    );
}

#[test]
fn route_ast_receipts_require_special_routes_to_update_returned_authorities() {
    let fastly_app = include_str!("../../../crates/trusted-server-adapter-fastly/src/app.rs");
    let entry = include_str!("../../../crates/trusted-server-adapter-fastly/src/main.rs");
    let axum = include_str!("../../../crates/trusted-server-adapter-axum/src/app.rs");
    let cloudflare = include_str!("../../../crates/trusted-server-adapter-cloudflare/src/app.rs");
    let spin = include_str!("../../../crates/trusted-server-adapter-spin/src/app.rs");

    let axum_unused = axum.replacen(
        "router = router.route(\"/health\", Method::GET,",
        "unused = router.route(\"/health\", Method::GET,",
        1,
    );
    assert!(extract_with_mutation(fastly_app, entry, &axum_unused, cloudflare, spin).is_err());

    let spin_unused = spin.replacen(
        "builder = builder.get(\"/health\", |_ctx: RequestContext| async {",
        "unused = builder.get(\"/health\", |_ctx: RequestContext| async {",
        1,
    );
    assert!(extract_with_mutation(fastly_app, entry, axum, cloudflare, &spin_unused).is_err());

    let startup_unbound = cloudflare.replacen(
        "*resp.status_mut() = status;",
        "let _unbound = status; *resp.status_mut() = StatusCode::OK;",
        1,
    );
    assert!(extract_with_mutation(fastly_app, entry, axum, &startup_unbound, spin).is_err());
}

#[test]
fn route_ast_receipts_reject_disconnected_fastly_health_ja4_asset_and_tsjs_facts() {
    let fastly_app = include_str!("../../../crates/trusted-server-adapter-fastly/src/app.rs");
    let entry = include_str!("../../../crates/trusted-server-adapter-fastly/src/main.rs");
    let axum = include_str!("../../../crates/trusted-server-adapter-axum/src/app.rs");
    let cloudflare = include_str!("../../../crates/trusted-server-adapter-cloudflare/src/app.rs");
    let spin = include_str!("../../../crates/trusted-server-adapter-spin/src/app.rs");

    let dead_health = entry.replacen(
        "if req.get_method() == FastlyMethod::GET && req.get_path() == \"/health\" {",
        "if req.get_method() == FastlyMethod::GET && req.get_path() == \"/health\" { return None;",
        1,
    );
    assert!(extract_with_mutation(fastly_app, &dead_health, axum, cloudflare, spin).is_err());

    let disconnected_health_call = entry.replace(
        r#"if let Some(response) = health_response(&req) {
        response.send_to_client();
        return;
    }"#,
        "if false { return; }",
    );
    assert!(
        extract_with_mutation(
            fastly_app,
            &disconnected_health_call,
            axum,
            cloudflare,
            spin,
        )
        .is_err()
    );

    let dead_ja4_predicate = entry.replace(
        "Ok(settings) if settings.debug.ja4_endpoint_enabled => {",
        "Ok(settings) if true => { let _unused = settings.debug.ja4_endpoint_enabled;",
    );
    assert!(
        extract_with_mutation(fastly_app, &dead_ja4_predicate, axum, cloudflare, spin).is_err()
    );

    let disconnected_tsjs = fastly_app.replacen(
        "if uses_dynamic_tsjs_fallback(&method, &path) {",
        "if false {",
        1,
    );
    assert!(extract_with_mutation(&disconnected_tsjs, entry, axum, cloudflare, spin).is_err());

    let disconnected_asset = fastly_app.replace(
        r#"let matched_asset_route = matches!(method, Method::GET | Method::HEAD)
            .then(|| state.settings.asset_route_for_path(&path))
            .flatten();"#,
        r#"let matched_asset_route = {
            let _unused = matches!(method, Method::GET | Method::HEAD)
                .then(|| state.settings.asset_route_for_path(&path))
                .flatten();
            None
        };"#,
    );
    assert!(extract_with_mutation(&disconnected_asset, entry, axum, cloudflare, spin).is_err());
}

#[test]
fn route_health_receipts_require_the_exact_returned_response() {
    let fastly = include_str!("../../../crates/trusted-server-adapter-fastly/src/app.rs");
    let entry = include_str!("../../../crates/trusted-server-adapter-fastly/src/main.rs");
    let axum = include_str!("../../../crates/trusted-server-adapter-axum/src/app.rs");
    let cloudflare = include_str!("../../../crates/trusted-server-adapter-cloudflare/src/app.rs");
    let spin = include_str!("../../../crates/trusted-server-adapter-spin/src/app.rs");

    let fastly_tuple = entry.replacen(
        "return Some(FastlyResponse::from_status(200).with_body_text_plain(\"ok\"));",
        "return Some((FastlyResponse::from_status(200).with_body_text_plain(\"ok\"), FastlyResponse::from_status(201).with_body_text_plain(\"changed\")).1);",
        1,
    );
    assert!(extract_with_mutation(fastly, &fastly_tuple, axum, cloudflare, spin).is_err());

    let axum_decoy = axum
        .replacen(".status(StatusCode::OK)", ".status(StatusCode::CREATED)", 1)
        .replacen(
            ".body(edgezero_core::body::Body::from(\"ok\"))",
            ".body(edgezero_core::body::Body::from(\"changed\"))",
            1,
        )
        .replacen(
            "        Ok::<Response, EdgeError>(\n            edgezero_core::http::response_builder()",
            "        let _unused = edgezero_core::http::response_builder().status(StatusCode::OK).body(edgezero_core::body::Body::from(\"ok\"));\n        Ok::<Response, EdgeError>(\n            edgezero_core::http::response_builder()",
            1,
        );
    assert!(extract_with_mutation(fastly, entry, &axum_decoy, cloudflare, spin).is_err());

    let spin_tuple = spin.replacen(
        "let mut resp = Response::new(edgezero_core::body::Body::from(\"ok\"));",
        "let mut resp = Response::new((edgezero_core::body::Body::from(\"ok\"), edgezero_core::body::Body::from(\"changed\")).1);",
        1,
    );
    assert!(extract_with_mutation(fastly, entry, axum, cloudflare, &spin_tuple).is_err());
}

#[test]
fn route_tsjs_receipts_require_terminal_flow_and_unshadowed_bindings() {
    let fastly = include_str!("../../../crates/trusted-server-adapter-fastly/src/app.rs");
    let entry = include_str!("../../../crates/trusted-server-adapter-fastly/src/main.rs");
    let axum = include_str!("../../../crates/trusted-server-adapter-axum/src/app.rs");
    let cloudflare = include_str!("../../../crates/trusted-server-adapter-cloudflare/src/app.rs");
    let spin = include_str!("../../../crates/trusted-server-adapter-spin/src/app.rs");

    let axum_override = axum.replacen(
        "        return handle_tsjs_dynamic(&req, &state.registry, EdgeCacheHeader::SMaxageFallback);",
        "        handle_tsjs_dynamic(&req, &state.registry, EdgeCacheHeader::SMaxageFallback);",
        1,
    );
    assert!(extract_with_mutation(fastly, entry, &axum_override, cloudflare, spin).is_err());

    let axum_early = axum.replacen(
        "    if method == Method::GET && path.starts_with(\"/static/tsjs=\") {",
        "    return Err(Report::new(TrustedServerError::BadRequest { message: String::new() }));\n    if method == Method::GET && path.starts_with(\"/static/tsjs=\") {",
        1,
    );
    assert!(extract_with_mutation(fastly, entry, &axum_early, cloudflare, spin).is_err());

    let cloudflare_shadow = cloudflare.replacen(
        "            let allow_tsjs = method == Method::GET;",
        "            let allow_tsjs = method == Method::GET;\n            let allow_tsjs = false;",
        1,
    );
    assert!(extract_with_mutation(fastly, entry, axum, &cloudflare_shadow, spin).is_err());

    let cloudflare_reassign = cloudflare.replacen(
        "            let allow_tsjs = method == Method::GET;",
        "            let mut allow_tsjs = method == Method::GET;\n            allow_tsjs = false;",
        1,
    );
    assert!(extract_with_mutation(fastly, entry, axum, &cloudflare_reassign, spin).is_err());

    let spin_cfg_decoy = spin.replacen(
        "        async fn dispatch(",
        "        #[cfg(test)]\n        async fn dispatch(",
        1,
    );
    assert!(extract_with_mutation(fastly, entry, axum, cloudflare, &spin_cfg_decoy).is_err());
}

#[test]
fn cloudflare_rejects_shadowed_authorities_and_opaque_route_initializers() {
    for source in [
        r#"
            fn build_router() {
                let mut router = RouterService::builder().get("/actual", handler);
                let spare_router = make_router();
                {
                    let mut router = spare_router;
                    router = router.get("/shadow", shadow_handler);
                }
                router.build()
            }
        "#,
        r#"
            fn build_router() {
                let mut router = RouterService::builder().get("/actual", handler);
                let mut spare_router = make_router();
                spare_router = spare_router.get("/shadow", shadow_handler);
                router.build()
            }
        "#,
        r#"
            fn build_router() {
                let mut router = RouterService::builder().get("/actual", handler);
                let _hidden = hidden_route!(router);
                router.build()
            }
        "#,
    ] {
        assert!(
            extract_cloudflare_routes(source).is_err(),
            "shadowed, competing, or opaque route authority must fail"
        );
    }
}

#[test]
fn fastly_asset_receipt_rejects_shadowed_selected_route() {
    let fastly = include_str!("../../../crates/trusted-server-adapter-fastly/src/app.rs");
    let entry = include_str!("../../../crates/trusted-server-adapter-fastly/src/main.rs");
    let axum = include_str!("../../../crates/trusted-server-adapter-axum/src/app.rs");
    let cloudflare = include_str!("../../../crates/trusted-server-adapter-cloudflare/src/app.rs");
    let spin = include_str!("../../../crates/trusted-server-adapter-spin/src/app.rs");
    let shadowed = fastly.replacen(
        "            .flatten();\n        if let Some(asset_route) = matched_asset_route {",
        "            .flatten();\n        let matched_asset_route = None;\n        if let Some(asset_route) = matched_asset_route {",
        1,
    );
    assert!(extract_with_mutation(&shadowed, entry, axum, cloudflare, spin).is_err());
}

#[test]
fn route_handler_receipts_bind_registered_identifiers_to_behavior() {
    let fastly = include_str!("../../../crates/trusted-server-adapter-fastly/src/app.rs");
    let entry = include_str!("../../../crates/trusted-server-adapter-fastly/src/main.rs");
    let axum = include_str!("../../../crates/trusted-server-adapter-axum/src/app.rs");
    let cloudflare = include_str!("../../../crates/trusted-server-adapter-cloudflare/src/app.rs");
    let spin = include_str!("../../../crates/trusted-server-adapter-spin/src/app.rs");

    let fastly_tester = fastly.replacen(
        "NamedRouteHandler::SetTester => handle_set_tester(&state.settings)",
        "NamedRouteHandler::SetTester => handle_clear_tester(&state.settings)",
        1,
    );
    assert!(extract_with_mutation(&fastly_tester, entry, axum, cloudflare, spin).is_err());

    let fastly_discovery = fastly.replacen(
        "handle_trusted_server_discovery(&state.settings, services, req)",
        "handle_verify_signature(&state.settings, services, req)",
        1,
    );
    assert!(extract_with_mutation(&fastly_discovery, entry, axum, cloudflare, spin).is_err());

    let axum_discovery = axum.replacen(
        "handle_trusted_server_discovery(&state.settings, &services, req)",
        "handle_verify_signature(&state.settings, &services, req)",
        1,
    );
    assert!(extract_with_mutation(fastly, entry, &axum_discovery, cloudflare, spin).is_err());

    let spin_discovery = spin.replacen(
        "handle_trusted_server_discovery(&s.settings, &services, req)",
        "handle_verify_signature(&s.settings, &services, req)",
        1,
    );
    assert!(extract_with_mutation(fastly, entry, axum, cloudflare, &spin_discovery).is_err());

    let cloudflare_preflight = cloudflare.replacen(
        "                Ok(page_bids_preflight_denied())\n            });",
        "                Ok(handle_verify_signature(&s.settings, &_services, _req)?)\n            });",
        1,
    );
    assert!(extract_with_mutation(fastly, entry, axum, &cloudflare_preflight, spin).is_err());

    let cloudflare_page_bids = cloudflare.replacen(
        "handle_page_bids(&s.settings, &services, None, auction, &ec_context, req).await",
        "handle_verify_signature(&s.settings, &services, req)",
        1,
    );
    assert!(extract_with_mutation(fastly, entry, axum, &cloudflare_page_bids, spin).is_err());
}

#[test]
fn route_handler_receipts_bind_denials_and_unsupported_statuses() {
    let fastly = include_str!("../../../crates/trusted-server-adapter-fastly/src/app.rs");
    let entry = include_str!("../../../crates/trusted-server-adapter-fastly/src/main.rs");
    let axum = include_str!("../../../crates/trusted-server-adapter-axum/src/app.rs");
    let cloudflare = include_str!("../../../crates/trusted-server-adapter-cloudflare/src/app.rs");
    let spin = include_str!("../../../crates/trusted-server-adapter-spin/src/app.rs");

    let fastly_preflight = fastly.replacen(
        "                return Ok(page_bids_preflight_denied());",
        "                page_bids_preflight_denied();\n                return handle_verify_signature(&state.settings, services, req);",
        1,
    );
    assert!(extract_with_mutation(&fastly_preflight, entry, axum, cloudflare, spin).is_err());

    let axum_status = axum.replacen(
        "*resp.status_mut() = StatusCode::NOT_IMPLEMENTED;",
        "*resp.status_mut() = StatusCode::OK;",
        1,
    );
    assert!(extract_with_mutation(fastly, entry, &axum_status, cloudflare, spin).is_err());

    let cloudflare_status = cloudflare.replacen(
        "*response.status_mut() = StatusCode::NOT_IMPLEMENTED;",
        "*response.status_mut() = StatusCode::OK;",
        1,
    );
    assert!(extract_with_mutation(fastly, entry, axum, &cloudflare_status, spin).is_err());

    let spin_status = spin.replacen(
        "*response.status_mut() = StatusCode::NOT_IMPLEMENTED;",
        "*response.status_mut() = StatusCode::OK;",
        1,
    );
    assert!(extract_with_mutation(fastly, entry, axum, cloudflare, &spin_status).is_err());
}

#[test]
fn fastly_named_handlers_bind_every_early_and_method_specific_behavior() {
    let fastly = include_str!("../../../crates/trusted-server-adapter-fastly/src/app.rs");
    let entry = include_str!("../../../crates/trusted-server-adapter-fastly/src/main.rs");
    let axum = include_str!("../../../crates/trusted-server-adapter-axum/src/app.rs");
    let cloudflare = include_str!("../../../crates/trusted-server-adapter-cloudflare/src/app.rs");
    let spin = include_str!("../../../crates/trusted-server-adapter-spin/src/app.rs");
    let changes = [
        fastly.replacen(
            "return Ok(run_batch_sync(&state, &services, req));",
            "return Ok(legacy_admin_alias_denied());",
            1,
        ),
        fastly.replacen(
            "handle_admin_ec_lookup(kv.as_ref(), &registry, &req)",
            "legacy_admin_alias_denied()",
            1,
        ),
        fastly.replacen(
            "handle_admin_eids_lookup(&registry, &req)",
            "legacy_admin_alias_denied()",
            1,
        ),
        fastly.replacen(
            "cors_preflight_identify(&state.settings, &req)",
            "Ok(legacy_admin_alias_denied())",
            1,
        ),
        fastly.replacen("handle_identify(", "legacy_admin_alias_denied(", 1),
    ];

    for changed in changes {
        assert_ne!(changed, fastly, "fixture must alter Fastly source");
        assert!(
            extract_with_mutation(&changed, entry, axum, cloudflare, spin).is_err(),
            "every Fastly early and Identify dispatch behavior must be source-bound"
        );
    }
}

#[test]
fn fastly_fallback_result_authority_is_unique_and_terminal() {
    let fastly = include_str!("../../../crates/trusted-server-adapter-fastly/src/app.rs");
    let entry = include_str!("../../../crates/trusted-server-adapter-fastly/src/main.rs");
    let axum = include_str!("../../../crates/trusted-server-adapter-axum/src/app.rs");
    let cloudflare = include_str!("../../../crates/trusted-server-adapter-cloudflare/src/app.rs");
    let spin = include_str!("../../../crates/trusted-server-adapter-spin/src/app.rs");
    let changes = [
        fastly.replacen(
            "let result = if uses_dynamic_tsjs_fallback(&method, &path) {",
            "let result = if uses_dynamic_tsjs_fallback(&method, &path) { let result = legacy_admin_alias_denied();",
            1,
        ),
        fastly.replacen(
            "let response = result.unwrap_or_else(|e| http_error(&e));",
            "result = Err(Report::new(TrustedServerError::BadRequest { message: String::new() }));\n    let response = result.unwrap_or_else(|e| http_error(&e));",
            1,
        ),
        fastly.replacen(
            "let response = result.unwrap_or_else(|e| http_error(&e));",
            "let response = result.unwrap_or_else(|e| legacy_admin_alias_denied());",
            1,
        ),
        fastly.replacen(
            "attach_dispatch_extensions(response, ec, effects)\n}",
            "legacy_admin_alias_denied()\n}",
            1,
        ),
    ];

    for changed in changes {
        assert_ne!(changed, fastly, "fixture must alter Fastly source");
        assert!(
            extract_with_mutation(&changed, entry, axum, cloudflare, spin).is_err(),
            "Fastly result must be unique, unassigned, unshadowed, and returned through the exact terminal pipeline"
        );
    }
}

#[test]
fn nested_fallback_dispatch_is_lexical_unique_and_directly_called() {
    let fastly = include_str!("../../../crates/trusted-server-adapter-fastly/src/app.rs");
    let entry = include_str!("../../../crates/trusted-server-adapter-fastly/src/main.rs");
    let axum = include_str!("../../../crates/trusted-server-adapter-axum/src/app.rs");
    let cloudflare = include_str!("../../../crates/trusted-server-adapter-cloudflare/src/app.rs");
    let spin = include_str!("../../../crates/trusted-server-adapter-spin/src/app.rs");

    for changed in [
        format!("{cloudflare}\nasync fn dispatch(state: Arc<AppState>, ctx: RequestContext) {{}}"),
        cloudflare.replacen("dispatch(s, ctx)", "decoy::dispatch(s, ctx)", 1),
    ] {
        assert!(
            extract_with_mutation(fastly, entry, axum, &changed, spin).is_err(),
            "Cloudflare fallback must call its one direct lexical dispatch"
        );
    }
    for changed in [
        format!("{spin}\nasync fn dispatch(state: Arc<AppState>, ctx: RequestContext) {{}}"),
        spin.replacen("dispatch(s, ctx)", "decoy::dispatch(s, ctx)", 1),
    ] {
        assert!(
            extract_with_mutation(fastly, entry, axum, cloudflare, &changed).is_err(),
            "Spin fallback must call its one direct lexical dispatch"
        );
    }
}

#[test]
fn dynamic_tsjs_branch_rejects_prior_terminating_control_flow() {
    let fastly = include_str!("../../../crates/trusted-server-adapter-fastly/src/app.rs");
    let entry = include_str!("../../../crates/trusted-server-adapter-fastly/src/main.rs");
    let axum = include_str!("../../../crates/trusted-server-adapter-axum/src/app.rs");
    let cloudflare = include_str!("../../../crates/trusted-server-adapter-cloudflare/src/app.rs");
    let spin = include_str!("../../../crates/trusted-server-adapter-spin/src/app.rs");
    let prefixes = [
        "if true { return legacy_admin_alias_denied(); }",
        "match true { _ => return legacy_admin_alias_denied() }",
        "loop { return legacy_admin_alias_denied(); }",
    ];

    for prefix in prefixes {
        let changed = axum.replacen(
            "    if method == Method::GET && path.starts_with(\"/static/tsjs=\") {",
            &format!("    {prefix}\n    if method == Method::GET && path.starts_with(\"/static/tsjs=\") {{"),
            1,
        );
        assert!(extract_with_mutation(fastly, entry, &changed, cloudflare, spin).is_err());

        let changed = cloudflare.replacen(
            "            let result = if allow_tsjs && path.starts_with(\"/static/tsjs=\") {",
            &format!("            {prefix}\n            let result = if allow_tsjs && path.starts_with(\"/static/tsjs=\") {{"),
            1,
        );
        assert!(extract_with_mutation(fastly, entry, axum, &changed, spin).is_err());

        let changed = spin.replacen(
            "            let result = if method == Method::GET && path.starts_with(\"/static/tsjs=\") {",
            &format!("            {prefix}\n            let result = if method == Method::GET && path.starts_with(\"/static/tsjs=\") {{"),
            1,
        );
        assert!(extract_with_mutation(fastly, entry, axum, cloudflare, &changed).is_err());
    }
}

#[test]
fn fastly_special_routes_require_live_request_and_settings_receivers() {
    let fastly = include_str!("../../../crates/trusted-server-adapter-fastly/src/app.rs");
    let entry = include_str!("../../../crates/trusted-server-adapter-fastly/src/main.rs");
    let axum = include_str!("../../../crates/trusted-server-adapter-axum/src/app.rs");
    let cloudflare = include_str!("../../../crates/trusted-server-adapter-cloudflare/src/app.rs");
    let spin = include_str!("../../../crates/trusted-server-adapter-spin/src/app.rs");
    let entry_changes = [
        entry.replacen(
            "req.get_method()",
            "FastlyRequest::from_client().get_method()",
            1,
        ),
        entry.replacen(
            "req.get_path()",
            "FastlyRequest::from_client().get_path()",
            1,
        ),
        entry.replacen(
            "health_response(&req)",
            "health_response(&FastlyRequest::from_client())",
            1,
        ),
        entry.replacen(
            "build_ja4_debug_response(&req)",
            "build_ja4_debug_response(&FastlyRequest::from_client())",
            1,
        ),
    ];
    for changed in entry_changes {
        assert!(extract_with_mutation(fastly, &changed, axum, cloudflare, spin).is_err());
    }

    let wrong_settings = fastly.replacen(
        "state.settings.asset_route_for_path(&path)",
        "other.settings.asset_route_for_path(&path)",
        1,
    );
    assert!(extract_with_mutation(&wrong_settings, entry, axum, cloudflare, spin).is_err());
}

#[test]
fn handler_behavior_paths_must_be_exact_and_unqualified() {
    let fastly = include_str!("../../../crates/trusted-server-adapter-fastly/src/app.rs");
    let entry = include_str!("../../../crates/trusted-server-adapter-fastly/src/main.rs");
    let axum = include_str!("../../../crates/trusted-server-adapter-axum/src/app.rs");
    let cloudflare = include_str!("../../../crates/trusted-server-adapter-cloudflare/src/app.rs");
    let spin = include_str!("../../../crates/trusted-server-adapter-spin/src/app.rs");

    let fastly_decoy = fastly.replacen(
        "handle_trusted_server_discovery(&state.settings, services, req)",
        "decoy::handle_trusted_server_discovery(&state.settings, services, req)",
        1,
    );
    assert!(extract_with_mutation(&fastly_decoy, entry, axum, cloudflare, spin).is_err());
    let axum_decoy = axum.replacen(
        "handle_trusted_server_discovery(&state.settings, &services, req)",
        "decoy::handle_trusted_server_discovery(&state.settings, &services, req)",
        1,
    );
    assert!(extract_with_mutation(fastly, entry, &axum_decoy, cloudflare, spin).is_err());
    let cloudflare_decoy = cloudflare.replacen(
        "admin_key_management_not_supported()",
        "decoy::admin_key_management_not_supported()",
        1,
    );
    assert!(extract_with_mutation(fastly, entry, axum, &cloudflare_decoy, spin).is_err());
}

#[test]
fn fallback_loops_require_the_unqualified_authoritative_method_iterator() {
    let fastly = include_str!("../../../crates/trusted-server-adapter-fastly/src/app.rs");
    let entry = include_str!("../../../crates/trusted-server-adapter-fastly/src/main.rs");
    let axum = include_str!("../../../crates/trusted-server-adapter-axum/src/app.rs");
    let cloudflare = include_str!("../../../crates/trusted-server-adapter-cloudflare/src/app.rs");
    let spin = include_str!("../../../crates/trusted-server-adapter-spin/src/app.rs");
    let decoy =
        "\nmod decoy { fn publisher_fallback_methods() -> [Method; 1] { [Method::GET] } }\n";

    let changed = format!(
        "{}{decoy}",
        fastly.replace(
            "for method in publisher_fallback_methods()",
            "for method in decoy::publisher_fallback_methods()"
        )
    );
    assert!(extract_with_mutation(&changed, entry, axum, cloudflare, spin).is_err());
    let changed = format!(
        "{}{decoy}",
        axum.replace(
            "for method in publisher_fallback_methods()",
            "for method in decoy::publisher_fallback_methods()"
        )
    );
    assert!(extract_with_mutation(fastly, entry, &changed, cloudflare, spin).is_err());
    let changed = format!(
        "{}{decoy}",
        cloudflare.replace(
            "for method in publisher_fallback_methods()",
            "for method in decoy::publisher_fallback_methods()"
        )
    );
    assert!(extract_with_mutation(fastly, entry, axum, &changed, spin).is_err());
    let changed = format!(
        "{}{decoy}",
        spin.replace(
            "for method in publisher_fallback_methods()",
            "for method in decoy::publisher_fallback_methods()"
        )
    );
    assert!(extract_with_mutation(fastly, entry, axum, cloudflare, &changed).is_err());
}

#[test]
fn route_extractors_do_not_invent_imported_page_bid_constants() {
    let fastly = include_str!("../../../crates/trusted-server-adapter-fastly/src/app.rs");
    let cloudflare = include_str!("../../../crates/trusted-server-adapter-cloudflare/src/app.rs");
    assert!(extract_named_routes("fastly", fastly).is_err());
    assert!(extract_cloudflare_routes(cloudflare).is_err());
}

#[test]
fn repository_routes_resolve_page_bid_paths_only_from_the_core_publisher() {
    let publisher = include_str!("../../../crates/trusted-server-core/src/publisher.rs");
    let fastly = include_str!("../../../crates/trusted-server-adapter-fastly/src/app.rs");
    let entry = include_str!("../../../crates/trusted-server-adapter-fastly/src/main.rs");
    let axum = include_str!("../../../crates/trusted-server-adapter-axum/src/app.rs");
    let cloudflare = include_str!("../../../crates/trusted-server-adapter-cloudflare/src/app.rs");
    let spin = include_str!("../../../crates/trusted-server-adapter-spin/src/app.rs");
    let extract = |publisher_routes: &str| {
        extract_repository_routes(&RouteSources {
            publisher_routes,
            admin_routes: include_str!("../../../crates/trusted-server-core/src/ec/admin.rs"),
            fastly_app: fastly,
            fastly_entrypoint: entry,
            axum_app: axum,
            cloudflare_app: cloudflare,
            spin_app: spin,
        })
    };

    let changed = publisher.replacen(
        "pub const PAGE_BIDS_PATH: &str = \"/_ts/page-bids\";",
        "pub const PAGE_BIDS_PATH: &str = \"/_ts/page-bids-v2\";",
        1,
    );
    let routes = extract(&changed).expect("changed authoritative path should be extracted");
    assert!(routes.iter().any(|route| route.path == "/_ts/page-bids-v2"));
    assert!(
        validate_routes(
            RouteManifest::parse(include_str!("../manifests/routes.toml"))
                .expect("reviewed routes should parse")
                .routes(),
            &routes,
        )
        .is_err(),
        "an authoritative path change must cause manifest drift"
    );

    let duplicate = format!("{publisher}\npub const PAGE_BIDS_PATH: &str = \"/_ts/page-bids\";");
    assert!(extract(&duplicate).is_err());
    let test_decoy =
        format!("{publisher}\n#[cfg(test)] pub const PAGE_BIDS_PATH: &str = \"/test-decoy\";");
    extract(&test_decoy).expect("cfg(test) publisher constant must not own production evidence");
    let cfg_unix = publisher.replacen(
        "pub const PAGE_BIDS_PATH: &str = \"/_ts/page-bids\";",
        "#[cfg(unix)] pub const PAGE_BIDS_PATH: &str = \"/_ts/page-bids\";",
        1,
    );
    assert!(extract(&cfg_unix).is_err());
    let cfg_attr = publisher.replacen(
        "pub const PAGE_BIDS_PATH: &str = \"/_ts/page-bids\";",
        "#[cfg_attr(test, allow(dead_code))] pub const PAGE_BIDS_PATH: &str = \"/_ts/page-bids\";",
        1,
    );
    assert!(extract(&cfg_attr).is_err());
}

#[test]
fn route_authorities_reject_resets_shadows_and_dead_closure_registrations() {
    let fastly = include_str!("../../../crates/trusted-server-adapter-fastly/src/app.rs");
    let entry = include_str!("../../../crates/trusted-server-adapter-fastly/src/main.rs");
    let axum = include_str!("../../../crates/trusted-server-adapter-axum/src/app.rs");
    let cloudflare = include_str!("../../../crates/trusted-server-adapter-cloudflare/src/app.rs");
    let spin = include_str!("../../../crates/trusted-server-adapter-spin/src/app.rs");

    let fastly_reset = fastly.replacen(
        "        router.build()\n    }\n}\n\nimpl Hooks",
        "        router = RouterService::builder();\n        router.build()\n    }\n}\n\nimpl Hooks",
        1,
    );
    assert!(extract_with_mutation(&fastly_reset, entry, axum, cloudflare, spin).is_err());

    let axum_reset = axum.replacen(
        "    router.build()\n}\n\n#[cfg(test)]\nmod task8_route_tests",
        "    router = RouterService::builder();\n    router.build()\n}\n\n#[cfg(test)]\nmod task8_route_tests",
        1,
    );
    assert!(extract_with_mutation(fastly, entry, &axum_reset, cloudflare, spin).is_err());

    let spin_reset = spin.replacen(
        "        builder.build()\n    }\n}\n\n#[cfg(test)]",
        "        builder = RouterService::builder();\n        builder.build()\n    }\n}\n\n#[cfg(test)]",
        1,
    );
    assert!(extract_with_mutation(fastly, entry, axum, cloudflare, &spin_reset).is_err());

    let fastly_dead = fastly.replacen(
        "        router.build()\n    }\n}\n\nimpl Hooks",
        "        let _dead = || { router = router.route(\"/hidden\", Method::GET, fallback_handler.clone()); };\n        router.build()\n    }\n}\n\nimpl Hooks",
        1,
    );
    assert!(extract_with_mutation(&fastly_dead, entry, axum, cloudflare, spin).is_err());

    let axum_dead = axum.replacen(
        "    router.build()\n}\n\n#[cfg(test)]\nmod task8_route_tests",
        "    let _dead = || { router = router.route(\"/hidden\", Method::GET, fallback.clone()); };\n    router.build()\n}\n\n#[cfg(test)]\nmod task8_route_tests",
        1,
    );
    assert!(extract_with_mutation(fastly, entry, &axum_dead, cloudflare, spin).is_err());

    let spin_dead = spin.replacen(
        "        builder.build()\n    }\n}\n\n#[cfg(test)]",
        "        let _dead = || { builder = builder.route(\"/hidden\", Method::GET, fallback.clone()); };\n        builder.build()\n    }\n}\n\n#[cfg(test)]",
        1,
    );
    assert!(extract_with_mutation(fastly, entry, axum, cloudflare, &spin_dead).is_err());

    let cloudflare_dead = cloudflare.replacen(
        "        router.build()\n    }\n}\n\n#[cfg(test)]",
        "        let _dead = || { router = router.route(\"/hidden\", Method::GET, fallback.clone()); };\n        router.build()\n    }\n}\n\n#[cfg(test)]",
        1,
    );
    assert!(extract_with_mutation(fastly, entry, axum, &cloudflare_dead, spin).is_err());

    let axum_discarded = axum.replacen(
        "router = router.route(\"/\", method.clone(), fallback.clone());",
        "discarded = router.route(\"/\", method.clone(), fallback.clone());",
        1,
    );
    assert!(extract_with_mutation(fastly, entry, &axum_discarded, cloudflare, spin).is_err());
}

#[test]
fn startup_fallback_receipts_bind_status_and_loop_routes_to_returned_values() {
    let fastly = include_str!("../../../crates/trusted-server-adapter-fastly/src/app.rs");
    let entry = include_str!("../../../crates/trusted-server-adapter-fastly/src/main.rs");
    let axum = include_str!("../../../crates/trusted-server-adapter-axum/src/app.rs");
    let cloudflare = include_str!("../../../crates/trusted-server-adapter-cloudflare/src/app.rs");
    let spin = include_str!("../../../crates/trusted-server-adapter-spin/src/app.rs");

    let fastly_status_receiver = fastly.replacen(
        "*resp.status_mut() = status;",
        "*decoy.status_mut() = status;",
        1,
    );
    assert!(extract_with_mutation(&fastly_status_receiver, entry, axum, cloudflare, spin).is_err());

    let axum_status_source = axum.replacen(
        "let status = e.current_context().status_code();",
        "let status = other.current_context().status_code();",
        1,
    );
    assert!(extract_with_mutation(fastly, entry, &axum_status_source, cloudflare, spin).is_err());

    let spin_status_receiver = spin.replacen(
        "*resp.status_mut() = StatusCode::SERVICE_UNAVAILABLE;",
        "*decoy.status_mut() = StatusCode::SERVICE_UNAVAILABLE;",
        1,
    );
    assert!(extract_with_mutation(fastly, entry, axum, cloudflare, &spin_status_receiver).is_err());

    let cloudflare_discarded = cloudflare.replacen(
        "router = router.route(\"/\", method.clone(), make(Arc::clone(&message)));",
        "discarded = router.route(\"/\", method.clone(), make(Arc::clone(&message)));",
        1,
    );
    assert!(extract_with_mutation(fastly, entry, axum, &cloudflare_discarded, spin).is_err());
}

#[test]
fn fastly_healthy_fallback_handler_binds_to_the_selected_dispatch() {
    let fastly = include_str!("../../../crates/trusted-server-adapter-fastly/src/app.rs");
    let entry = include_str!("../../../crates/trusted-server-adapter-fastly/src/main.rs");
    let axum = include_str!("../../../crates/trusted-server-adapter-axum/src/app.rs");
    let cloudflare = include_str!("../../../crates/trusted-server-adapter-cloudflare/src/app.rs");
    let spin = include_str!("../../../crates/trusted-server-adapter-spin/src/app.rs");

    let fastly_handler = fastly.replacen(
        "Box::pin(execute_fallback(state, ctx))",
        "Box::pin(execute_named(state, ctx, NamedRouteHandler::Auction))",
        1,
    );
    assert!(extract_with_mutation(&fastly_handler, entry, axum, cloudflare, spin).is_err());
}

#[test]
fn axum_healthy_fallback_handler_binds_to_the_selected_dispatch() {
    let fastly = include_str!("../../../crates/trusted-server-adapter-fastly/src/app.rs");
    let entry = include_str!("../../../crates/trusted-server-adapter-fastly/src/main.rs");
    let axum = include_str!("../../../crates/trusted-server-adapter-axum/src/app.rs");
    let cloudflare = include_str!("../../../crates/trusted-server-adapter-cloudflare/src/app.rs");
    let spin = include_str!("../../../crates/trusted-server-adapter-spin/src/app.rs");
    let axum_handler = axum.replacen(
        "dispatch_fallback(&state, &services, req).await",
        "handle_verify_signature(&state.settings, &services, req)",
        1,
    );
    assert!(extract_with_mutation(fastly, entry, &axum_handler, cloudflare, spin).is_err());
}

#[test]
fn cloudflare_healthy_fallback_handler_binds_to_the_selected_dispatch() {
    let fastly = include_str!("../../../crates/trusted-server-adapter-fastly/src/app.rs");
    let entry = include_str!("../../../crates/trusted-server-adapter-fastly/src/main.rs");
    let axum = include_str!("../../../crates/trusted-server-adapter-axum/src/app.rs");
    let cloudflare = include_str!("../../../crates/trusted-server-adapter-cloudflare/src/app.rs");
    let spin = include_str!("../../../crates/trusted-server-adapter-spin/src/app.rs");
    let cloudflare_handler = cloudflare.replacen("dispatch(s, ctx)", "other(s, ctx)", 1);
    assert!(extract_with_mutation(fastly, entry, axum, &cloudflare_handler, spin).is_err());
}

#[test]
fn spin_healthy_fallback_handler_binds_to_the_selected_dispatch() {
    let fastly = include_str!("../../../crates/trusted-server-adapter-fastly/src/app.rs");
    let entry = include_str!("../../../crates/trusted-server-adapter-fastly/src/main.rs");
    let axum = include_str!("../../../crates/trusted-server-adapter-axum/src/app.rs");
    let cloudflare = include_str!("../../../crates/trusted-server-adapter-cloudflare/src/app.rs");
    let spin = include_str!("../../../crates/trusted-server-adapter-spin/src/app.rs");
    let spin_handler = spin.replacen("dispatch(s, ctx)", "other(s, ctx)", 1);
    assert!(extract_with_mutation(fastly, entry, axum, cloudflare, &spin_handler).is_err());
}

#[test]
fn route_path_constants_enforce_production_cfg_and_uniqueness() {
    let base = r#"
        const NAMED_ROUTES: &[NamedRoute] = &[
            NamedRoute { path: ROUTE_PATH, primary_methods: &[Method::GET], handler: NamedRouteHandler::TrustedServerDiscovery },
        ];
    "#;
    for declaration in [
        "#[cfg(test)] const ROUTE_PATH: &str = \"/.well-known/trusted-server.json\";",
        "#[cfg(unix)] const ROUTE_PATH: &str = \"/.well-known/trusted-server.json\";",
        "#[cfg_attr(test, allow(dead_code))] const ROUTE_PATH: &str = \"/.well-known/trusted-server.json\";",
        "const ROUTE_PATH: &str = \"/.well-known/trusted-server.json\"; const ROUTE_PATH: &str = \"/.well-known/trusted-server.json\";",
    ] {
        let source = format!("{declaration}{base}");
        assert!(
            extract_named_routes("fastly", &source).is_err(),
            "conditionally owned or duplicate route constant must fail"
        );
    }
}

#[test]
fn route_ast_receipts_reject_page_bids_and_legacy_handler_swaps_in_all_adapters() {
    let fastly = include_str!("../../../crates/trusted-server-adapter-fastly/src/app.rs");
    let entry = include_str!("../../../crates/trusted-server-adapter-fastly/src/main.rs");
    let axum = include_str!("../../../crates/trusted-server-adapter-axum/src/app.rs");
    let cloudflare = include_str!("../../../crates/trusted-server-adapter-cloudflare/src/app.rs");
    let spin = include_str!("../../../crates/trusted-server-adapter-spin/src/app.rs");

    let fastly_preflight = fastly.replace(
        "if req.method() == Method::OPTIONS {",
        "if false && req.method() == Method::OPTIONS {",
    );
    assert!(extract_with_mutation(&fastly_preflight, entry, axum, cloudflare, spin).is_err());

    let axum_preflight = axum.replacen(
        "if req.method() == Method::OPTIONS {",
        "if false && req.method() == Method::OPTIONS {",
        1,
    );
    assert!(extract_with_mutation(fastly, entry, &axum_preflight, cloudflare, spin).is_err());

    let spin_preflight = spin.replace(
        "Method::OPTIONS, page_bids_options_handler",
        "Method::OPTIONS, page_bids_handler.clone()",
    );
    assert!(extract_with_mutation(fastly, entry, axum, cloudflare, &spin_preflight).is_err());

    let spin_legacy = spin.replacen(
        "method.clone(), legacy_admin_deny",
        "method.clone(), fallback.clone()",
        1,
    );
    assert!(extract_with_mutation(fastly, entry, axum, cloudflare, &spin_legacy).is_err());

    let fastly_legacy = fastly.replacen(
        "handler: NamedRouteHandler::LegacyAdminDenied",
        "handler: NamedRouteHandler::Auction",
        1,
    );
    assert!(extract_with_mutation(&fastly_legacy, entry, axum, cloudflare, spin).is_err());

    let axum_legacy = axum.replacen(
        "handler: NamedRouteHandler::LegacyAdminDenied",
        "handler: NamedRouteHandler::Auction",
        1,
    );
    assert!(extract_with_mutation(fastly, entry, &axum_legacy, cloudflare, spin).is_err());
}

#[test]
fn route_and_support_manifests_reject_duplicates_unknown_fields_and_bad_ownership() {
    let duplicate_routes = r#"
        version = 1
        reviewed = true
        [[routes]]
        adapters = ["fastly"]
        path = "/health"
        methods = ["GET"]
        shape = "literal"
        predicate = "always"
        status = "real"
        [[routes]]
        adapters = ["fastly"]
        path = "/health"
        methods = ["GET"]
        shape = "literal"
        predicate = "always"
        status = "real"
    "#;
    assert!(
        RouteManifest::parse(duplicate_routes)
            .expect_err("duplicate expanded routes must fail")
            .to_string()
            .contains("duplicate")
    );
    assert!(
        RouteManifest::parse(
            &duplicate_routes.replace("status = \"real\"", "status = \"real\"\nunknown = true")
        )
        .expect_err("unknown route fields must fail")
        .to_string()
        .contains("unknown")
    );

    let support = r#"
        version = 1
        reviewed = true
        [[adapters]]
        id = "fastly"
        release_status = "production"
        owner = ""
        reviewed_at = "2026-99-99"
        health = "pre_router"
        startup_status = 500
        startup_health = true
        provider_fanout = "multiple"
        trusted_client_ip = "entry_point_resolve_and_sanitize"
        request_normalization = "none"
    "#;
    assert!(
        AdapterSupportManifest::parse(support)
            .expect_err("blank owner must fail")
            .to_string()
            .contains("ownership")
    );
    assert!(
        AdapterSupportManifest::parse(&support.replace("owner = \"\"", "owner = \"reviewer\""))
            .expect_err("invalid calendar review date must fail")
            .to_string()
            .contains("ownership")
    );
    let experimental = support
        .replace("owner = \"\"", "owner = \"reviewer\"")
        .replace(
            "reviewed_at = \"2026-99-99\"",
            "reviewed_at = \"2026-09-07\"",
        )
        .replace(
            "release_status = \"production\"",
            "release_status = \"experimental\"",
        );
    AdapterSupportManifest::parse(&experimental)
        .expect("experimental is a supported adapter maturity");
}

#[test]
fn adapter_support_manifest_caps_rows_and_every_string() {
    let row = r#"
        [[adapters]]
        id = "fastly"
        release_status = "production"
        owner = "reviewer"
        reviewed_at = "2026-09-05"
        health = "pre_router"
        startup_status = 500
        startup_health = true
        provider_fanout = "multiple"
        trusted_client_ip = "entry_point_resolve_and_sanitize"
        request_normalization = "none"
    "#;
    let too_many = format!("version = 1\nreviewed = true\n{}", row.repeat(4097));
    assert!(AdapterSupportManifest::parse(&too_many).is_err());

    let long = "x".repeat(16 * 1024 + 1);
    for (field, original) in [
        ("id", "fastly"),
        ("release_status", "production"),
        ("owner", "reviewer"),
        ("reviewed_at", "2026-09-05"),
        ("health", "pre_router"),
        ("provider_fanout", "multiple"),
    ] {
        let source = format!("version = 1\nreviewed = true\n{row}").replace(
            &format!("{field} = \"{original}\""),
            &format!("{field} = \"{long}\""),
        );
        assert!(
            AdapterSupportManifest::parse(&source).is_err(),
            "adapter support {field} must observe the string bound"
        );
    }
}

#[test]
fn route_manifest_rejects_duplicate_adapters_and_methods_before_expansion() {
    let source = r#"
        version = 1
        reviewed = true
        [[routes]]
        adapters = ["fastly", "fastly"]
        path = "/health"
        methods = ["GET"]
        shape = "literal"
        predicate = "always"
        status = "real"
    "#;
    assert!(
        RouteManifest::parse(source)
            .expect_err("duplicate adapters must fail before set conversion")
            .to_string()
            .contains("duplicate route adapter")
    );

    let source = source
        .replace("[\"fastly\", \"fastly\"]", "[\"fastly\"]")
        .replace("[\"GET\"]", "[\"GET\", \"GET\"]");
    assert!(
        RouteManifest::parse(&source)
            .expect_err("duplicate methods must fail before set conversion")
            .to_string()
            .contains("duplicate route method")
    );
}

#[test]
fn route_manifest_rejects_overlapping_expanded_method_semantics() {
    let source = r#"
        version = 1
        reviewed = true
        [[routes]]
        adapters = ["fastly"]
        path = "/health"
        methods = ["GET"]
        shape = "literal"
        predicate = "always"
        status = "real"
        [[routes]]
        adapters = ["fastly"]
        path = "/health"
        methods = ["GET", "HEAD"]
        shape = "literal"
        predicate = "always"
        status = "real"
    "#;
    assert!(
        RouteManifest::parse(source)
            .expect_err("overlapping expanded method semantics must fail")
            .to_string()
            .contains("duplicate expanded route semantic")
    );
}

#[test]
fn named_and_cloudflare_sources_reject_duplicate_semantic_routes() {
    let named = r#"
        const NAMED_ROUTES: &[NamedRoute] = &[
            NamedRoute { path: "/.well-known/trusted-server.json", primary_methods: &[Method::GET], handler: NamedRouteHandler::TrustedServerDiscovery },
            NamedRoute { path: "/.well-known/trusted-server.json", primary_methods: &[Method::GET], handler: NamedRouteHandler::TrustedServerDiscovery },
        ];
    "#;
    assert!(
        extract_named_routes("fastly", named)
            .expect_err("duplicate named routes must fail")
            .to_string()
            .contains("duplicate")
    );

    let duplicate_method = r#"
        const NAMED_ROUTES: &[NamedRoute] = &[
            NamedRoute { path: "/.well-known/trusted-server.json", primary_methods: &[Method::GET, Method::GET], handler: NamedRouteHandler::TrustedServerDiscovery },
        ];
    "#;
    assert!(
        extract_named_routes("fastly", duplicate_method)
            .expect_err("duplicate source methods must fail")
            .to_string()
            .contains("duplicate")
    );

    let cloudflare = r#"
        fn build_router() {
            let mut router = RouterService::builder().get("/health", handler);
            router = router.get("/health", handler);
            router.build()
        }
    "#;
    assert!(
        extract_cloudflare_routes(cloudflare)
            .expect_err("duplicate Cloudflare routes must fail")
            .to_string()
            .contains("duplicate")
    );
}

#[test]
fn adapter_support_rejects_duplicate_operational_rows() {
    let row = r#"
        [[adapters]]
        id = "fastly"
        release_status = "production"
        owner = "reviewer"
        reviewed_at = "2026-09-05"
        health = "pre_router"
        startup_status = 500
        startup_health = true
        provider_fanout = "multiple"
        trusted_client_ip = "entry_point_resolve_and_sanitize"
        request_normalization = "none"
    "#;
    let source = format!("version = 1\nreviewed = true\n{row}\n{row}");
    assert!(
        AdapterSupportManifest::parse(&source)
            .expect_err("duplicate adapter operational rows must fail")
            .to_string()
            .contains("duplicate adapter support row")
    );
}

#[test]
fn local_initializers_cannot_divert_registered_handlers_or_tsjs_dispatch() {
    let fastly = include_str!("../../../crates/trusted-server-adapter-fastly/src/app.rs");
    let entry = include_str!("../../../crates/trusted-server-adapter-fastly/src/main.rs");
    let axum = include_str!("../../../crates/trusted-server-adapter-axum/src/app.rs");
    let cloudflare = include_str!("../../../crates/trusted-server-adapter-cloudflare/src/app.rs");
    let spin = include_str!("../../../crates/trusted-server-adapter-spin/src/app.rs");

    let axum_handler = axum.replacen(
        "                        let ec_context = build_ec_context(&state, &services, &req);",
        "                        let _divert = if black_box(true) { return Ok(legacy_admin_alias_denied()); } else { () };\n                        let ec_context = build_ec_context(&state, &services, &req);",
        1,
    );
    assert_ne!(axum_handler, axum, "fixture must alter Axum auction");
    assert!(
        extract_with_mutation(fastly, entry, &axum_handler, cloudflare, spin).is_err(),
        "a local initializer must not hide an earlier Axum handler return"
    );

    let axum_tsjs = axum.replacen(
        "    if method == Method::GET && path.starts_with(\"/static/tsjs=\") {",
        "    let _divert = if black_box(true) { return Ok(legacy_admin_alias_denied()); } else { () };\n    if method == Method::GET && path.starts_with(\"/static/tsjs=\") {",
        1,
    );
    assert_ne!(axum_tsjs, axum, "fixture must alter Axum TSJS flow");
    assert!(extract_with_mutation(fastly, entry, &axum_tsjs, cloudflare, spin).is_err());

    let cloudflare_tsjs = cloudflare.replacen(
        "            let result = if allow_tsjs && path.starts_with(\"/static/tsjs=\") {",
        "            let _divert = if black_box(true) { return Ok(legacy_admin_alias_denied()); } else { () };\n            let result = if allow_tsjs && path.starts_with(\"/static/tsjs=\") {",
        1,
    );
    assert_ne!(
        cloudflare_tsjs, cloudflare,
        "fixture must alter Cloudflare TSJS flow"
    );
    assert!(extract_with_mutation(fastly, entry, axum, &cloudflare_tsjs, spin).is_err());

    let spin_tsjs = spin.replacen(
        "            let result = if method == Method::GET && path.starts_with(\"/static/tsjs=\") {",
        "            let _divert = if black_box(true) { return Ok(legacy_admin_alias_denied()); } else { () };\n            let result = if method == Method::GET && path.starts_with(\"/static/tsjs=\") {",
        1,
    );
    assert_ne!(spin_tsjs, spin, "fixture must alter Spin TSJS flow");
    assert!(extract_with_mutation(fastly, entry, axum, cloudflare, &spin_tsjs).is_err());
}

#[test]
fn preceding_local_try_cannot_divert_a_named_handler() {
    let fastly = include_str!("../../../crates/trusted-server-adapter-fastly/src/app.rs");
    let entry = include_str!("../../../crates/trusted-server-adapter-fastly/src/main.rs");
    let axum = include_str!("../../../crates/trusted-server-adapter-axum/src/app.rs");
    let cloudflare = include_str!("../../../crates/trusted-server-adapter-cloudflare/src/app.rs");
    let spin = include_str!("../../../crates/trusted-server-adapter-spin/src/app.rs");
    let changed = axum.replacen(
        "                    NamedRouteHandler::VerifySignature => {\n                        handle_verify_signature(&state.settings, &services, req)",
        "                    NamedRouteHandler::VerifySignature => {\n                        let _divert = Err::<(), Report<TrustedServerError>>(Report::new(TrustedServerError::BadRequest { message: \"diverted\".to_owned() }))?;\n                        handle_verify_signature(&state.settings, &services, req)",
        1,
    );
    assert_ne!(changed, axum, "fixture must alter Axum VerifySignature");
    assert!(
        extract_with_mutation(fastly, entry, &changed, cloudflare, spin).is_err(),
        "a preceding local try must not divert the registered handler"
    );
}

#[test]
fn preceding_local_call_cannot_divert_a_named_handler() {
    let fastly = include_str!("../../../crates/trusted-server-adapter-fastly/src/app.rs");
    let entry = include_str!("../../../crates/trusted-server-adapter-fastly/src/main.rs");
    let axum = include_str!("../../../crates/trusted-server-adapter-axum/src/app.rs");
    let cloudflare = include_str!("../../../crates/trusted-server-adapter-cloudflare/src/app.rs");
    let spin = include_str!("../../../crates/trusted-server-adapter-spin/src/app.rs");
    let changed = format!(
        "{}\nfn divert() -> ! {{ panic!(\"diverted\") }}",
        axum.replacen(
            "                    NamedRouteHandler::VerifySignature => {\n                        handle_verify_signature(&state.settings, &services, req)",
            "                    NamedRouteHandler::VerifySignature => {\n                        let _divert = divert();\n                        handle_verify_signature(&state.settings, &services, req)",
            1,
        )
    );
    assert_ne!(changed, axum, "fixture must alter Axum VerifySignature");
    assert!(
        extract_with_mutation(fastly, entry, &changed, cloudflare, spin).is_err(),
        "an arbitrary preceding call must not divert the registered handler"
    );
}

#[test]
fn response_receipts_reject_unchecked_headers_and_preceding_branches() {
    let fastly = include_str!("../../../crates/trusted-server-adapter-fastly/src/app.rs");
    let entry = include_str!("../../../crates/trusted-server-adapter-fastly/src/main.rs");
    let axum = include_str!("../../../crates/trusted-server-adapter-axum/src/app.rs");
    let cloudflare = include_str!("../../../crates/trusted-server-adapter-cloudflare/src/app.rs");
    let spin = include_str!("../../../crates/trusted-server-adapter-spin/src/app.rs");

    let spin_health = spin.replacen(
        "HeaderValue::from_static(\"text/plain\"))",
        "HeaderValue::from_static(\"application/json\"))",
        1,
    );
    assert_ne!(
        spin_health, spin,
        "fixture must alter Spin health content type"
    );
    assert!(extract_with_mutation(fastly, entry, axum, cloudflare, &spin_health).is_err());

    let axum_unsupported = axum.replacen(
        "                        let body = edgezero_core::body::Body::from(",
        "                        if black_box(true) { return Ok(legacy_admin_alias_denied()); }\n                        let body = edgezero_core::body::Body::from(",
        1,
    );
    assert_ne!(
        axum_unsupported, axum,
        "fixture must alter Axum unsupported response"
    );
    assert!(extract_with_mutation(fastly, entry, &axum_unsupported, cloudflare, spin).is_err());

    let cloudflare_unsupported = cloudflare.replacen(
        "fn admin_key_management_not_supported() -> Response {\n",
        "fn admin_key_management_not_supported() -> Response {\n    if black_box(true) { return legacy_admin_alias_denied(); }\n",
        1,
    );
    assert_ne!(
        cloudflare_unsupported, cloudflare,
        "fixture must alter Cloudflare unsupported response"
    );
    assert!(extract_with_mutation(fastly, entry, axum, &cloudflare_unsupported, spin).is_err());

    let spin_unsupported = spin.replacen(
        "fn admin_key_management_not_supported() -> Response {\n",
        "fn admin_key_management_not_supported() -> Response {\n    if black_box(true) { return legacy_admin_alias_denied(); }\n",
        1,
    );
    assert_ne!(
        spin_unsupported, spin,
        "fixture must alter Spin unsupported response"
    );
    assert!(extract_with_mutation(fastly, entry, axum, cloudflare, &spin_unsupported).is_err());
}

#[test]
fn router_blocks_reject_shadows_of_every_consumed_named_collection() {
    let fastly = include_str!("../../../crates/trusted-server-adapter-fastly/src/app.rs");
    let entry = include_str!("../../../crates/trusted-server-adapter-fastly/src/main.rs");
    let axum = include_str!("../../../crates/trusted-server-adapter-axum/src/app.rs");
    let cloudflare = include_str!("../../../crates/trusted-server-adapter-cloudflare/src/app.rs");
    let spin = include_str!("../../../crates/trusted-server-adapter-spin/src/app.rs");

    let fastly_shadow = format!(
        "{}\nmod decoy {{ const NAMED_ROUTES: &[NamedRoute] = &[]; }}",
        fastly.replacen(
            "    fn routes_for_state(state: &Arc<AppState>) -> RouterService {\n",
            "    fn routes_for_state(state: &Arc<AppState>) -> RouterService {\n        use decoy::NAMED_ROUTES;\n",
            1,
        )
    );
    assert!(extract_with_mutation(&fastly_shadow, entry, axum, cloudflare, spin).is_err());

    let axum_shadow = format!(
        "{}\nmod decoy {{ fn named_routes() -> [NamedRoute; 0] {{ [] }} }}",
        axum.replacen(
            "fn build_router(state: &Arc<AppState>) -> RouterService {\n",
            "fn build_router(state: &Arc<AppState>) -> RouterService {\n    use decoy::named_routes;\n",
            1,
        )
    );
    assert!(extract_with_mutation(fastly, entry, &axum_shadow, cloudflare, spin).is_err());

    let spin_shadow = format!(
        "{}\nmod decoy {{ fn named_fallback_paths() -> [(&'static str, &'static [Method]); 0] {{ [] }} }}",
        spin.replacen(
            "fn build_router(state: &Arc<AppState>) -> RouterService {\n    {\n",
            "fn build_router(state: &Arc<AppState>) -> RouterService {\n    {\n        use decoy::named_fallback_paths;\n",
            1,
        )
    );
    assert!(extract_with_mutation(fastly, entry, axum, cloudflare, &spin_shadow).is_err());
}

#[test]
fn named_handlers_bind_exact_arguments_and_fastly_prior_flow() {
    let fastly = include_str!("../../../crates/trusted-server-adapter-fastly/src/app.rs");
    let entry = include_str!("../../../crates/trusted-server-adapter-fastly/src/main.rs");
    let axum = include_str!("../../../crates/trusted-server-adapter-axum/src/app.rs");
    let cloudflare = include_str!("../../../crates/trusted-server-adapter-cloudflare/src/app.rs");
    let spin = include_str!("../../../crates/trusted-server-adapter-spin/src/app.rs");

    let axum_auction = axum.replacen(
        "                            &ec_context,\n                            &services,",
        "                            &EcContext::default(),\n                            &services,",
        1,
    );
    assert_ne!(
        axum_auction, axum,
        "fixture must alter Axum auction context"
    );
    assert!(extract_with_mutation(fastly, entry, &axum_auction, cloudflare, spin).is_err());

    let fastly_admin_arguments = fastly.replacen(
        "handle_admin_eids_lookup(&registry, &req)",
        "handle_admin_eids_lookup(&req, &registry)",
        1,
    );
    assert_ne!(
        fastly_admin_arguments, fastly,
        "fixture must swap Fastly admin EIDs arguments"
    );
    assert!(extract_with_mutation(&fastly_admin_arguments, entry, axum, cloudflare, spin).is_err());

    let fastly_admin = fastly.replacen(
        "    if matches!(\n        handler,\n        NamedRouteHandler::AdminEcLookup | NamedRouteHandler::AdminEidsLookup\n    ) {\n",
        "    if matches!(\n        handler,\n        NamedRouteHandler::AdminEcLookup | NamedRouteHandler::AdminEidsLookup\n    ) {\n        if black_box(true) { return Ok(legacy_admin_alias_denied()); }\n",
        1,
    );
    assert_ne!(
        fastly_admin, fastly,
        "fixture must alter Fastly early admin flow"
    );
    assert!(extract_with_mutation(&fastly_admin, entry, axum, cloudflare, spin).is_err());

    let fastly_identify = fastly.replacen(
        "            } else {\n                let kv = crate::require_identity_graph(&state.settings)?;",
        "            } else {\n                if black_box(true) { return Ok(legacy_admin_alias_denied()); }\n                let kv = crate::require_identity_graph(&state.settings)?;",
        1,
    );
    assert_ne!(
        fastly_identify, fastly,
        "fixture must alter Fastly Identify GET flow"
    );
    assert!(extract_with_mutation(&fastly_identify, entry, axum, cloudflare, spin).is_err());

    let fastly_page_bids = fastly.replacen(
        "            // Like the auction, page-bids reads consent data, so the consent KV",
        "            if black_box(true) { return Ok(legacy_admin_alias_denied()); }\n            // Like the auction, page-bids reads consent data, so the consent KV",
        1,
    );
    assert_ne!(
        fastly_page_bids, fastly,
        "fixture must alter Fastly page-bids GET flow"
    );
    assert!(extract_with_mutation(&fastly_page_bids, entry, axum, cloudflare, spin).is_err());
}

#[test]
fn audited_constructors_status_paths_and_fastly_none_are_exact() {
    let fastly = include_str!("../../../crates/trusted-server-adapter-fastly/src/app.rs");
    let entry = include_str!("../../../crates/trusted-server-adapter-fastly/src/main.rs");
    let axum = include_str!("../../../crates/trusted-server-adapter-axum/src/app.rs");
    let cloudflare = include_str!("../../../crates/trusted-server-adapter-cloudflare/src/app.rs");
    let spin = include_str!("../../../crates/trusted-server-adapter-spin/src/app.rs");

    let cases = [
        (
            "Box::pin",
            fastly.replacen(
                "Box::pin(execute_fallback(state, ctx))",
                "decoy::Box::pin(execute_fallback(state, ctx))",
                1,
            ),
            entry.to_owned(),
            axum.to_owned(),
            cloudflare.to_owned(),
            spin.to_owned(),
        ),
        (
            "Arc::clone",
            fastly.replacen(
                "let state = Arc::clone(&state);\n        Box::pin(execute_fallback",
                "let state = decoy::Arc::clone(&state);\n        Box::pin(execute_fallback",
                1,
            ),
            entry.to_owned(),
            axum.to_owned(),
            cloudflare.to_owned(),
            spin.to_owned(),
        ),
        (
            "Body::from",
            fastly.to_owned(),
            entry.to_owned(),
            axum.replacen(
                "edgezero_core::body::Body::from(\"ok\")",
                "decoy::edgezero_core::body::Body::from(\"ok\")",
                1,
            ),
            cloudflare.to_owned(),
            spin.to_owned(),
        ),
        (
            "Response::new",
            fastly.to_owned(),
            entry.to_owned(),
            axum.to_owned(),
            cloudflare.to_owned(),
            spin.replacen(
                "Response::new(edgezero_core::body::Body::from(\"ok\"))",
                "decoy::Response::new(edgezero_core::body::Body::from(\"ok\"))",
                1,
            ),
        ),
        (
            "response_builder",
            fastly.to_owned(),
            entry.to_owned(),
            axum.replacen(
                "edgezero_core::http::response_builder()\n                .status(StatusCode::OK)",
                "decoy::http::response_builder()\n                .status(StatusCode::OK)",
                1,
            ),
            cloudflare.to_owned(),
            spin.to_owned(),
        ),
        (
            "RouterService::builder",
            fastly.to_owned(),
            entry.to_owned(),
            axum.replacen("RouterService::builder()", "::RouterService::builder()", 1),
            cloudflare.to_owned(),
            spin.to_owned(),
        ),
        (
            "StatusCode::OK",
            fastly.to_owned(),
            entry.to_owned(),
            axum.to_owned(),
            cloudflare.to_owned(),
            spin.replacen(
                "*resp.status_mut() = StatusCode::OK;",
                "*resp.status_mut() = decoy::StatusCode::OK;",
                1,
            ),
        ),
    ];
    for (name, changed_fastly, changed_entry, changed_axum, changed_cloudflare, changed_spin) in
        cases
    {
        assert!(
            extract_with_mutation(
                &changed_fastly,
                &changed_entry,
                &changed_axum,
                &changed_cloudflare,
                &changed_spin,
            )
            .is_err(),
            "suffix-only {name} constructor/status paths must fail"
        );
    }

    let fastly_response = entry.replacen(
        "FastlyResponse::from_status(200)",
        "decoy::FastlyResponse::from_status(200)",
        1,
    );
    assert!(extract_with_mutation(fastly, &fastly_response, axum, cloudflare, spin).is_err());
    let fastly_none = entry.replacen("\n    None\n}", "\n    decoy::None\n}", 1);
    assert_ne!(fastly_none, entry, "fixture must alter Fastly health None");
    assert!(extract_with_mutation(fastly, &fastly_none, axum, cloudflare, spin).is_err());
}

#[test]
fn guarded_response_statuses_are_bound_to_authoritative_bodies() {
    let publisher = include_str!("../../../crates/trusted-server-core/src/publisher.rs");
    let admin = include_str!("../../../crates/trusted-server-core/src/ec/admin.rs");
    let fastly = include_str!("../../../crates/trusted-server-adapter-fastly/src/app.rs");
    let entry = include_str!("../../../crates/trusted-server-adapter-fastly/src/main.rs");
    let axum = include_str!("../../../crates/trusted-server-adapter-axum/src/app.rs");
    let cloudflare = include_str!("../../../crates/trusted-server-adapter-cloudflare/src/app.rs");
    let spin = include_str!("../../../crates/trusted-server-adapter-spin/src/app.rs");

    let publisher_ok = publisher.replacen(
        "*response.status_mut() = StatusCode::FORBIDDEN;",
        "*response.status_mut() = StatusCode::OK;",
        1,
    );
    assert_ne!(
        publisher_ok, publisher,
        "fixture must alter page-bids denial"
    );
    assert!(
        extract_with_core_mutation(&publisher_ok, admin, fastly, entry, axum, cloudflare, spin)
            .is_err()
    );

    let admin_ok = admin.replacen(
        "        StatusCode::NOT_IMPLEMENTED,\n        \"EC identity graph is not configured on this deployment\"",
        "        StatusCode::OK,\n        \"EC identity graph is not configured on this deployment\"",
        1,
    );
    assert_ne!(
        admin_ok, admin,
        "fixture must alter portable admin EC denial"
    );
    assert!(
        extract_with_core_mutation(publisher, &admin_ok, fastly, entry, axum, cloudflare, spin)
            .is_err()
    );

    let cloudflare_ok = cloudflare.replacen(
        "*response.status_mut() = edgezero_core::http::StatusCode::NOT_FOUND;",
        "*response.status_mut() = edgezero_core::http::StatusCode::OK;",
        1,
    );
    assert_ne!(
        cloudflare_ok, cloudflare,
        "fixture must alter Cloudflare legacy denial"
    );
    assert!(
        extract_with_core_mutation(publisher, admin, fastly, entry, axum, &cloudflare_ok, spin)
            .is_err()
    );

    let cloudflare_wrapper = cloudflare.replacen(
        "fn admin_ec_lookup_not_supported() -> Response {\n    core_admin_ec_lookup_not_supported()\n}",
        "fn admin_ec_lookup_not_supported() -> Response {\n    legacy_admin_alias_denied()\n}",
        1,
    );
    assert_ne!(
        cloudflare_wrapper, cloudflare,
        "fixture must alter portable admin EC wrapper"
    );
    assert!(
        extract_with_core_mutation(
            publisher,
            admin,
            fastly,
            entry,
            axum,
            &cloudflare_wrapper,
            spin
        )
        .is_err()
    );
}

#[test]
fn cloudflare_named_handler_aliases_bind_to_exact_local_bodies() {
    let fastly = include_str!("../../../crates/trusted-server-adapter-fastly/src/app.rs");
    let entry = include_str!("../../../crates/trusted-server-adapter-fastly/src/main.rs");
    let axum = include_str!("../../../crates/trusted-server-adapter-axum/src/app.rs");
    let cloudflare = include_str!("../../../crates/trusted-server-adapter-cloudflare/src/app.rs");
    let spin = include_str!("../../../crates/trusted-server-adapter-spin/src/app.rs");

    let cloudflare = cloudflare
        .replacen(
            "        let mut router = RouterService::builder()",
            "        let verify = fallback.clone();\n        let mut router = RouterService::builder()",
            1,
        )
        .replacen(
            "                make_handler(Arc::clone(&state), |s, services, req| async move {\n                    handle_verify_signature(&s.settings, &services, req)\n                }),",
            "                verify,",
            1,
        );
    assert_ne!(
        cloudflare,
        include_str!("../../../crates/trusted-server-adapter-cloudflare/src/app.rs"),
        "fixture must bind Cloudflare verify to fallback"
    );
    assert!(extract_with_mutation(fastly, entry, axum, &cloudflare, spin).is_err());
}

#[test]
fn portable_named_handlers_require_exact_settings_arguments() {
    let fastly = include_str!("../../../crates/trusted-server-adapter-fastly/src/app.rs");
    let entry = include_str!("../../../crates/trusted-server-adapter-fastly/src/main.rs");
    let axum = include_str!("../../../crates/trusted-server-adapter-axum/src/app.rs");
    let cloudflare = include_str!("../../../crates/trusted-server-adapter-cloudflare/src/app.rs");
    let spin = include_str!("../../../crates/trusted-server-adapter-spin/src/app.rs");

    let cloudflare_default = cloudflare.replacen(
        "handle_first_party_proxy(&s.settings, &services, req).await",
        "handle_first_party_proxy(&Settings::default(), &services, req).await",
        1,
    );
    assert_ne!(
        cloudflare_default, cloudflare,
        "fixture must alter Cloudflare first-party settings"
    );
    assert!(extract_with_mutation(fastly, entry, axum, &cloudflare_default, spin).is_err());

    let spin_default = spin.replacen(
        "handle_first_party_proxy(&s.settings, &services, req)",
        "handle_first_party_proxy(&Settings::default(), &services, req)",
        1,
    );
    assert_ne!(
        spin_default, spin,
        "fixture must alter Spin first-party settings"
    );
    assert!(extract_with_mutation(fastly, entry, axum, cloudflare, &spin_default).is_err());
}
