use std::collections::BTreeSet;

use docs_parity::routes::{
    AdapterSupportManifest, RouteManifest, RouteRecord, RouteShape, RouteSources, RouteStatus,
    extract_cloudflare_routes, extract_named_routes, extract_repository_routes,
    validate_adapter_support, validate_routes,
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
    let routes = extract_cloudflare_routes(include_str!(
        "../../../crates/trusted-server-adapter-cloudflare/src/app.rs"
    ))
    .expect("the production Cloudflare builder must stay inside the closed grammar");
    assert_eq!(
        routes.len(),
        20,
        "16 named paths, split page-bids semantics, and root/rest fallback"
    );
}

#[test]
fn repository_private_named_collections_have_the_prechange_route_sets() {
    let fastly = extract_named_routes(
        "fastly",
        include_str!("../../../crates/trusted-server-adapter-fastly/src/app.rs"),
    )
    .expect("Fastly named route collection should parse");
    let axum = extract_named_routes(
        "axum",
        include_str!("../../../crates/trusted-server-adapter-axum/src/app.rs"),
    )
    .expect("Axum named route collection should parse");
    let spin = extract_named_routes(
        "spin",
        include_str!("../../../crates/trusted-server-adapter-spin/src/app.rs"),
    )
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
}
