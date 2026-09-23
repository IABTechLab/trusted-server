//! Benchmarks the per-request cost Fix 2 (JS-bundling dedup) targets:
//! `IntegrationRegistry::js_module_ids_immediate`/`_deferred` (memoized via
//! `OnceLock` as of this fix) and the `trusted_server_js` bundle functions
//! they feed into.
//!
//! Run with: `cargo bench -p trusted-server-core --bench registry_bundle_bench`

use std::sync::Arc;

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use trusted_server_core::auction::compile_auction_plan;
use trusted_server_core::config::TrustedServerAppConfig;
use trusted_server_core::integrations::IntegrationRegistry;

// Reuses the integration-tests' readable app-config fixture (multiple real
// integrations enabled: testlight, didomi, sourcepoint, permutive, lockr,
// datadome, gpt, gpt_diagnostics, google_tag_manager, prebid, nextjs) so
// `enabled_integration_ids` is representative rather than empty — the
// `#[cfg(test)]`-gated registry constructors elsewhere in this crate can't
// populate that list, and building a fixture from scratch here would risk
// drifting out of sync with what `Settings` actually requires.
const APP_CONFIG_TOML: &str = include_str!(
    "../../trusted-server-integration-tests/fixtures/configs/trusted-server.integration.toml"
);

fn build_registry() -> IntegrationRegistry {
    let app_config: TrustedServerAppConfig =
        toml::from_str(APP_CONFIG_TOML).expect("fixture should parse as TrustedServerAppConfig");
    let settings = app_config.into_settings();
    let plan = Arc::new(compile_auction_plan(&settings).expect("should compile auction plan"));
    IntegrationRegistry::with_plan(&settings, plan).expect("should build registry from plan")
}

fn bench_registry_and_bundle(c: &mut Criterion) {
    let mut group = c.benchmark_group("registry_bundle");

    // A fresh registry per iteration: this is what actually happens on
    // Fastly (a new `IntegrationRegistry` is built every request), so the
    // memoization only pays off within that one instance's lifetime, not
    // across iterations here.
    group.bench_function("js_module_ids_immediate", |b| {
        b.iter(|| {
            let registry = build_registry();
            black_box(registry.js_module_ids_immediate())
        });
    });
    group.bench_function("js_module_ids_deferred", |b| {
        b.iter(|| {
            let registry = build_registry();
            black_box(registry.js_module_ids_deferred())
        });
    });
    // Both immediate and deferred lists computed from the same registry, as
    // the head-injection path in html_processor.rs does per document.
    group.bench_function("js_module_ids_immediate_and_deferred", |b| {
        b.iter(|| {
            let registry = build_registry();
            black_box((
                registry.js_module_ids_immediate(),
                registry.js_module_ids_deferred(),
            ))
        });
    });

    // Isolates the memoization effect from registry-build cost: one registry
    // built once, then called repeatedly. `js_module_ids()` always
    // recomputes (unmemoized, matching every call's pre-fix cost); the
    // immediate+deferred pair now only recomputes once total, on whichever
    // runs first, since both draw from the same registry instance's cache.
    let built_once = build_registry();
    group.bench_function("js_module_ids_raw_called_twice", |b| {
        b.iter(|| {
            black_box(built_once.js_module_ids());
            black_box(built_once.js_module_ids())
        });
    });
    group.bench_function("js_module_ids_immediate_then_deferred_memoized", |b| {
        b.iter(|| {
            black_box(built_once.js_module_ids_immediate());
            black_box(built_once.js_module_ids_deferred())
        });
    });

    let module_ids = build_registry().js_module_ids_immediate();
    group.bench_function("concatenate_modules", |b| {
        b.iter(|| black_box(trusted_server_js::concatenate_modules(&module_ids)));
    });
    group.bench_function("concatenated_hash", |b| {
        b.iter(|| black_box(trusted_server_js::concatenated_hash(&module_ids)));
    });

    group.finish();
}

criterion_group!(benches, bench_registry_and_bundle);
criterion_main!(benches);
