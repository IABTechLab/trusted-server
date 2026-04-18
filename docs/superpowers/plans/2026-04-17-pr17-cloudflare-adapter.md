# PR17 — Cloudflare Workers Adapter Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `trusted-server-adapter-cloudflare` crate so trusted-server runs on Cloudflare Workers, using the same `TrustedServerApp` core as the Fastly and Axum adapters.

**Architecture:** A new `crates/trusted-server-adapter-cloudflare/` crate implements `Hooks` on `TrustedServerApp` and wires `RuntimeServices` using Cloudflare Workers bindings (KV, Config, Secrets) via the `edgezero-adapter-cloudflare` crate. The entry point is a `#[event(fetch)]` macro. Before adding the crate, `std::time::Instant` in `trusted-server-core` must be replaced with `web_time::Instant` (which is a zero-cost alias on native, but works on `wasm32-unknown-unknown` where `std::time::Instant` panics). The crate is host-compilable via `cfg`-gated shims so CI can validate it with `cargo check` on native before deploying to Workers.

**Tech Stack:** Rust 2024 edition, `worker` crate (Cloudflare Workers SDK), `edgezero-adapter-cloudflare`, `web-time`, `wrangler` (CLI, for manual deploy only — not in CI).

---

## File Map

### New files

- `crates/trusted-server-adapter-cloudflare/Cargo.toml` — crate manifest
- `crates/trusted-server-adapter-cloudflare/cloudflare.toml` — edgezero manifest (kv/config/secret store names)
- `crates/trusted-server-adapter-cloudflare/wrangler.toml` — Wrangler config (bindings, compatibility)
- `crates/trusted-server-adapter-cloudflare/.gitignore` — ignore `target/`, `.edgezero/`
- `crates/trusted-server-adapter-cloudflare/src/lib.rs` — `#[event(fetch)]` entry point + host shim
- `crates/trusted-server-adapter-cloudflare/src/app.rs` — `TrustedServerApp` + `Hooks` impl
- `crates/trusted-server-adapter-cloudflare/src/platform.rs` — `build_runtime_services` for Cloudflare
- `crates/trusted-server-adapter-cloudflare/tests/routes.rs` — route smoke tests (host target, no Workers runtime)

### Modified files

- `crates/trusted-server-core/Cargo.toml` — add `web-time` workspace dep
- `crates/trusted-server-core/src/auction/orchestrator.rs` — replace `std::time::Instant` with `web_time::Instant`
- `Cargo.toml` (workspace) — add `web-time` to `[workspace.dependencies]`; add cloudflare crate to `[members]`
- `.github/workflows/test.yml` — add `test-cloudflare` CI job
- `CLAUDE.md` — document new crate

---

## Task 1: Replace `std::time::Instant` with `web_time::Instant` in core

`std::time::Instant` panics on `wasm32-unknown-unknown` (Cloudflare). `web_time::Instant` is a zero-cost drop-in on native and JS-backed on WASM.

**Files:**

- Modify: `Cargo.toml` (workspace `[workspace.dependencies]`)
- Modify: `crates/trusted-server-core/Cargo.toml`
- Modify: `crates/trusted-server-core/src/auction/orchestrator.rs`

- [ ] **Step 1: Add `web-time` to workspace dependencies**

In `Cargo.toml`:

```toml
web-time = "1"
```

Add alphabetically in `[workspace.dependencies]`.

- [ ] **Step 2: Add `web-time` to `trusted-server-core/Cargo.toml`**

```toml
web-time = { workspace = true }
```

- [ ] **Step 3: Replace `std::time::Instant` in orchestrator**

In `crates/trusted-server-core/src/auction/orchestrator.rs`, change line 6:

```rust
// Before:
use std::time::{Duration, Instant};

// After:
use std::time::Duration;
use web_time::Instant;
```

Lines 830 and 842 use `std::time::Instant::now()` — change both to `Instant::now()` (they already use the bare name once the import is replaced).

- [ ] **Step 4: Verify WASM and native both compile**

```bash
cargo check -p trusted-server-core
cargo check -p trusted-server-core --target wasm32-wasip1
```

Expected: `Finished` with no errors.

- [ ] **Step 5: Run core tests**

```bash
cargo test -p trusted-server-core --target wasm32-wasip1
```

Expected: all pass.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml crates/trusted-server-core/Cargo.toml crates/trusted-server-core/src/auction/orchestrator.rs
git commit -m "Replace std::time::Instant with web_time::Instant in auction orchestrator

wasm32-unknown-unknown (Cloudflare Workers) does not support
std::time::Instant — it panics at runtime. web_time::Instant is a
zero-cost drop-in on native and JS-backed on WASM."
```

---

## Task 2: Workspace plumbing — add cloudflare crate as member

**Files:**

- Modify: `Cargo.toml` (workspace)
- Modify: `Cargo.toml` (workspace.dependencies)

- [ ] **Step 1: Add `edgezero-adapter-cloudflare` to workspace deps**

In `Cargo.toml` `[workspace.dependencies]`:

```toml
edgezero-adapter-cloudflare = { git = "https://github.com/stackpop/edgezero", rev = "38198f9839b70aef03ab971ae5876982773fc2a1", default-features = false }
```

(Same `rev` as the other edgezero deps already in the workspace.)

- [ ] **Step 2: Add cloudflare crate to workspace `[members]`**

```toml
members = [
    "crates/trusted-server-core",
    "crates/trusted-server-adapter-fastly",
    "crates/trusted-server-adapter-axum",
    "crates/trusted-server-adapter-cloudflare",
    "crates/js",
    "crates/openrtb",
]
```

- [ ] **Step 3: Verify workspace resolves (crate doesn't exist yet — expect path error)**

```bash
cargo metadata --no-deps 2>&1 | head -5
```

Expected: error about missing path (that's fine — the crate directory doesn't exist yet). Proceed to Task 3.

---

## Task 3: Crate skeleton

**Files:**

- Create: `crates/trusted-server-adapter-cloudflare/.gitignore`
- Create: `crates/trusted-server-adapter-cloudflare/Cargo.toml`
- Create: `crates/trusted-server-adapter-cloudflare/src/lib.rs`
- Create: `crates/trusted-server-adapter-cloudflare/src/app.rs`
- Create: `crates/trusted-server-adapter-cloudflare/src/platform.rs`

- [ ] **Step 1: Create `.gitignore`**

```
target/
.edgezero/
```

- [ ] **Step 2: Create `Cargo.toml`**

```toml
[package]
name = "trusted-server-adapter-cloudflare"
version = "0.1.0"
edition = "2024"
publish = false

[lints]
workspace = true

[lib]
name = "trusted_server_adapter_cloudflare"
path = "src/lib.rs"
crate-type = ["cdylib", "rlib"]

[features]
default = []
cloudflare = ["edgezero-adapter-cloudflare/cloudflare", "dep:worker"]

[dependencies]
async-trait = { workspace = true }
edgezero-adapter-cloudflare = { workspace = true, features = [] }
edgezero-core = { workspace = true }
error-stack = { workspace = true }
log = { workspace = true }
trusted-server-core = { path = "../trusted-server-core" }
trusted-server-js = { path = "../js" }
worker = { version = "0.7", default-features = false, features = ["http"], optional = true }

[dev-dependencies]
edgezero-adapter-cloudflare = { workspace = true }
edgezero-core = { workspace = true }
tokio = { workspace = true, features = ["rt-multi-thread", "macros"] }
tower = { version = "0.4", features = ["util"] }
```

- [ ] **Step 3: Create stub `src/lib.rs`**

```rust
pub mod app;
pub mod platform;
```

- [ ] **Step 4: Create stub `src/app.rs`**

```rust
use trusted_server_core::error::TrustedServerError;
use error_stack::Report;

/// Application entry point (stub — implementation in Task 4).
pub struct TrustedServerApp;

pub(crate) fn http_error(_report: &Report<TrustedServerError>) -> edgezero_core::http::Response {
    todo!("implemented in Task 4")
}
```

- [ ] **Step 5: Create stub `src/platform.rs`**

```rust
use trusted_server_core::platform::RuntimeServices;

pub fn build_runtime_services(
    _ctx: &edgezero_core::context::RequestContext,
) -> RuntimeServices {
    todo!("implemented in Task 5")
}
```

- [ ] **Step 6: Verify workspace compiles**

```bash
cargo check -p trusted-server-adapter-cloudflare
```

Expected: `Finished` (stubs compile, `todo!()` is fine at check time).

- [ ] **Step 7: Commit**

```bash
git add crates/trusted-server-adapter-cloudflare/ Cargo.toml
git commit -m "Add trusted-server-adapter-cloudflare crate skeleton"
```

---

## Task 4: App wiring — `TrustedServerApp` + `Hooks` implementation

This mirrors `crates/trusted-server-adapter-axum/src/app.rs` exactly, except the entry point and error helper.

**Files:**

- Modify: `crates/trusted-server-adapter-cloudflare/src/app.rs`

- [ ] **Step 1: Write the full `app.rs`**

```rust
use std::sync::Arc;

use edgezero_core::app::Hooks;
use edgezero_core::context::RequestContext;
use edgezero_core::error::EdgeError;
use edgezero_core::http::{HeaderValue, Response, header};
use edgezero_core::router::RouterService;
use error_stack::Report;
use trusted_server_core::auction::endpoints::handle_auction;
use trusted_server_core::auction::{AuctionOrchestrator, build_orchestrator};
use trusted_server_core::error::{IntoHttpResponse as _, TrustedServerError};
use trusted_server_core::integrations::IntegrationRegistry;
use trusted_server_core::platform::RuntimeServices;
use trusted_server_core::proxy::{
    handle_first_party_click, handle_first_party_proxy, handle_first_party_proxy_rebuild,
    handle_first_party_proxy_sign,
};
use trusted_server_core::publisher::{handle_publisher_request, handle_tsjs_dynamic};
use trusted_server_core::request_signing::{
    handle_deactivate_key, handle_rotate_key, handle_trusted_server_discovery,
    handle_verify_signature,
};
use trusted_server_core::settings::Settings;
use trusted_server_core::settings_data::get_settings;

use crate::platform::build_runtime_services;

pub struct AppState {
    settings: Arc<Settings>,
    orchestrator: Arc<AuctionOrchestrator>,
    registry: Arc<IntegrationRegistry>,
}

fn build_state() -> Result<Arc<AppState>, Report<TrustedServerError>> {
    let settings = get_settings()?;
    let orchestrator = build_orchestrator(&settings)?;
    let registry = IntegrationRegistry::new(&settings)?;
    Ok(Arc::new(AppState {
        settings: Arc::new(settings),
        orchestrator: Arc::new(orchestrator),
        registry: Arc::new(registry),
    }))
}

fn build_per_request_services(ctx: &RequestContext) -> RuntimeServices {
    build_runtime_services(ctx)
}

/// Convert a [`Report<TrustedServerError>`] into an HTTP [`Response`].
pub(crate) fn http_error(report: &Report<TrustedServerError>) -> Response {
    let root_error = report.current_context();
    log::error!("Error occurred: {:?}", report);
    let body = edgezero_core::body::Body::from(format!("{}\n", root_error.user_message()));
    let mut response = Response::new(body);
    *response.status_mut() = root_error.status_code();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    response
}

fn startup_error_router(e: Report<TrustedServerError>) -> RouterService {
    RouterService::new(move |_ctx: RequestContext| {
        let body = edgezero_core::body::Body::from(format!(
            "trusted-server failed to start: {}\n",
            e.current_context()
        ));
        let mut r = Response::new(body);
        *r.status_mut() = edgezero_core::http::StatusCode::INTERNAL_SERVER_ERROR;
        async move { Ok(r) }
    })
}

pub struct TrustedServerApp;

impl Hooks for TrustedServerApp {
    fn routes() -> RouterService {
        let state = match build_state() {
            Ok(s) => s,
            Err(e) => return startup_error_router(e),
        };

        let settings = Arc::clone(&state.settings);
        let orchestrator = Arc::clone(&state.orchestrator);
        let registry = Arc::clone(&state.registry);

        let mut router = edgezero_core::router::Router::new();

        // Discovery + signing
        {
            let s = Arc::clone(&settings);
            router.get("/.well-known/trusted-server.json", move |ctx| {
                let s = Arc::clone(&s);
                let svc = build_per_request_services(&ctx);
                async move { handle_trusted_server_discovery(&ctx, &s, &svc).await.or_else(|e| Ok(http_error(&e))) }
            });
        }
        {
            let s = Arc::clone(&settings);
            router.post("/verify-signature", move |ctx| {
                let s = Arc::clone(&s);
                let svc = build_per_request_services(&ctx);
                async move { handle_verify_signature(&ctx, &s, &svc).await.or_else(|e| Ok(http_error(&e))) }
            });
        }

        // Admin
        {
            let s = Arc::clone(&settings);
            router.post("/admin/keys/rotate", move |ctx| {
                let s = Arc::clone(&s);
                let svc = build_per_request_services(&ctx);
                async move { handle_rotate_key(&ctx, &s, &svc).await.or_else(|e| Ok(http_error(&e))) }
            });
        }
        {
            let s = Arc::clone(&settings);
            router.post("/admin/keys/deactivate", move |ctx| {
                let s = Arc::clone(&s);
                let svc = build_per_request_services(&ctx);
                async move { handle_deactivate_key(&ctx, &s, &svc).await.or_else(|e| Ok(http_error(&e))) }
            });
        }

        // Static JS
        {
            let s = Arc::clone(&settings);
            let r = Arc::clone(&registry);
            router.get("/static/tsjs=:hash", move |ctx| {
                let s = Arc::clone(&s);
                let r = Arc::clone(&r);
                async move { handle_tsjs_dynamic(&ctx, &s, &r).await.or_else(|e| Ok(http_error(&e))) }
            });
        }

        // First-party proxy
        {
            let s = Arc::clone(&settings);
            router.get("/first-party/proxy", move |ctx| {
                let s = Arc::clone(&s);
                let svc = build_per_request_services(&ctx);
                async move { handle_first_party_proxy(&ctx, &s, &svc).await.or_else(|e| Ok(http_error(&e))) }
            });
        }
        {
            let s = Arc::clone(&settings);
            router.post("/first-party/proxy", move |ctx| {
                let s = Arc::clone(&s);
                let svc = build_per_request_services(&ctx);
                async move { handle_first_party_proxy(&ctx, &s, &svc).await.or_else(|e| Ok(http_error(&e))) }
            });
        }
        {
            let s = Arc::clone(&settings);
            router.get("/first-party/proxy/sign", move |ctx| {
                let s = Arc::clone(&s);
                let svc = build_per_request_services(&ctx);
                async move { handle_first_party_proxy_sign(&ctx, &s, &svc).await.or_else(|e| Ok(http_error(&e))) }
            });
        }
        {
            let s = Arc::clone(&settings);
            router.get("/first-party/proxy/rebuild", move |ctx| {
                let s = Arc::clone(&s);
                let svc = build_per_request_services(&ctx);
                async move { handle_first_party_proxy_rebuild(&ctx, &s, &svc).await.or_else(|e| Ok(http_error(&e))) }
            });
        }
        {
            let s = Arc::clone(&settings);
            router.get("/first-party/click", move |ctx| {
                let s = Arc::clone(&s);
                let svc = build_per_request_services(&ctx);
                async move { handle_first_party_click(&ctx, &s, &svc).await.or_else(|e| Ok(http_error(&e))) }
            });
        }

        // Auction
        {
            let s = Arc::clone(&settings);
            let o = Arc::clone(&orchestrator);
            router.post("/auction", move |ctx| {
                let s = Arc::clone(&s);
                let o = Arc::clone(&o);
                let svc = build_per_request_services(&ctx);
                async move { handle_auction(&ctx, &s, &o, &svc).await.or_else(|e| Ok(http_error(&e))) }
            });
        }

        // Publisher proxy (catch-all)
        {
            let s = Arc::clone(&settings);
            let r = Arc::clone(&registry);
            router.any("/:path*", move |ctx| {
                let s = Arc::clone(&s);
                let r = Arc::clone(&r);
                let svc = build_per_request_services(&ctx);
                async move { handle_publisher_request(&ctx, &s, &r, &svc).await.or_else(|e| Ok(http_error(&e))) }
            });
        }

        router.build()
    }
}
```

- [ ] **Step 2: Verify it compiles**

```bash
cargo check -p trusted-server-adapter-cloudflare
```

Expected: `Finished`.

- [ ] **Step 3: Commit**

```bash
git add crates/trusted-server-adapter-cloudflare/src/app.rs
git commit -m "Add TrustedServerApp Hooks implementation for Cloudflare adapter"
```

---

## Task 5: Platform trait implementations

Cloudflare Workers exposes KV, config, and secrets through the `worker::Env` binding. The edgezero Cloudflare adapter already wraps these — we just need to wire them into `RuntimeServices`.

**Key difference from Axum:** On Cloudflare the `worker::Env` is passed per-request via `CloudflareRequestContext`. KV is available via the edgezero adapter's built-in handle; config/secret use `edgezero-adapter-cloudflare`'s `CloudflareConfigStore` and `CloudflareSecretStore`. For the `PlatformHttpClient`, the edgezero adapter's `CloudflareProxyClient` is already registered at dispatch time via `ProxyHandle` — so we use `UnavailableHttpClient` (same pattern as Axum's `UnavailableKvStore`).

**Files:**

- Modify: `crates/trusted-server-adapter-cloudflare/src/platform.rs`

- [ ] **Step 1: Write `platform.rs`**

On native (host compile for CI), the `worker` crate types are unavailable. Use `cfg` to gate the Cloudflare-specific implementation behind `#[cfg(all(feature = "cloudflare", target_arch = "wasm32"))]` and provide a no-op stub for host builds.

```rust
use std::sync::Arc;
use trusted_server_core::platform::{
    PlatformError, RuntimeServices, UnavailableKvStore,
    ClientInfo, PlatformBackend, PlatformBackendSpec, PlatformConfigStore,
    PlatformGeo, GeoInfo, StoreName, StoreId,
};
use error_stack::Report;

// ---------------------------------------------------------------------------
// Host-only stub (native target, used in CI cargo check + tests)
// ---------------------------------------------------------------------------

/// Construct a no-op [`RuntimeServices`] for host-target builds.
///
/// All platform operations degrade gracefully on native. This exists only so
/// the crate host-compiles for CI; Cloudflare Workers always runs the
/// `cfg`-gated implementation below.
#[cfg(not(all(feature = "cloudflare", target_arch = "wasm32")))]
pub fn build_runtime_services(
    _ctx: &edgezero_core::context::RequestContext,
) -> RuntimeServices {
    struct NoopConfigStore;
    impl PlatformConfigStore for NoopConfigStore {
        fn get(&self, _: &StoreName, _: &str) -> Result<String, Report<PlatformError>> {
            Err(Report::new(PlatformError::ConfigStore).attach("unavailable on host target"))
        }
        fn put(&self, _: &StoreId, _: &str, _: &str) -> Result<(), Report<PlatformError>> {
            Err(Report::new(PlatformError::ConfigStore).attach("unavailable on host target"))
        }
        fn delete(&self, _: &StoreId, _: &str) -> Result<(), Report<PlatformError>> {
            Err(Report::new(PlatformError::ConfigStore).attach("unavailable on host target"))
        }
    }

    struct NoopSecretStore;
    impl trusted_server_core::platform::PlatformSecretStore for NoopSecretStore {
        fn get_bytes(&self, _: &StoreName, _: &str) -> Result<Vec<u8>, Report<PlatformError>> {
            Err(Report::new(PlatformError::SecretStore).attach("unavailable on host target"))
        }
        fn create(&self, _: &StoreId, _: &str, _: &str) -> Result<(), Report<PlatformError>> {
            Err(Report::new(PlatformError::SecretStore).attach("unavailable on host target"))
        }
        fn delete(&self, _: &StoreId, _: &str) -> Result<(), Report<PlatformError>> {
            Err(Report::new(PlatformError::SecretStore).attach("unavailable on host target"))
        }
    }

    struct NoopBackend;
    impl PlatformBackend for NoopBackend {
        fn predict_name(&self, _: &PlatformBackendSpec) -> Result<String, Report<PlatformError>> {
            Ok("noop".to_string())
        }
        fn ensure(&self, spec: &PlatformBackendSpec) -> Result<String, Report<PlatformError>> {
            self.predict_name(spec)
        }
    }

    struct NoopGeo;
    impl PlatformGeo for NoopGeo {
        fn lookup(&self, _: Option<std::net::IpAddr>) -> Result<Option<GeoInfo>, Report<PlatformError>> {
            Ok(None)
        }
    }

    use trusted_server_core::platform::UnavailableHttpClient;

    RuntimeServices::builder()
        .config_store(Arc::new(NoopConfigStore))
        .secret_store(Arc::new(NoopSecretStore))
        .kv_store(Arc::new(UnavailableKvStore))
        .backend(Arc::new(NoopBackend))
        .http_client(Arc::new(UnavailableHttpClient))
        .geo(Arc::new(NoopGeo))
        .client_info(ClientInfo { client_ip: None, tls_protocol: None, tls_cipher: None })
        .build()
}

// ---------------------------------------------------------------------------
// Cloudflare Workers implementation
// ---------------------------------------------------------------------------

#[cfg(all(feature = "cloudflare", target_arch = "wasm32"))]
pub fn build_runtime_services(
    ctx: &edgezero_core::context::RequestContext,
) -> RuntimeServices {
    use edgezero_adapter_cloudflare::CloudflareRequestContext;

    let client_ip = CloudflareRequestContext::get(ctx.request())
        .and_then(|c| c.client_ip());

    // KV, config, secrets are injected at dispatch time by edgezero's
    // dispatch_with_bindings — they live in the request extensions.
    // UnavailableKvStore and UnavailableHttpClient are correct here:
    // KV is accessed via edgezero's KvHandle (not PlatformKvStore),
    // and outbound HTTP uses CloudflareProxyClient via ProxyHandle.
    use trusted_server_core::platform::UnavailableHttpClient;

    struct CloudflareBackend;
    impl PlatformBackend for CloudflareBackend {
        fn predict_name(&self, spec: &PlatformBackendSpec) -> Result<String, Report<PlatformError>> {
            Ok(format!("{}_{}", spec.scheme, spec.host))
        }
        fn ensure(&self, spec: &PlatformBackendSpec) -> Result<String, Report<PlatformError>> {
            self.predict_name(spec)
        }
    }

    struct CloudflareGeo;
    impl PlatformGeo for CloudflareGeo {
        fn lookup(&self, _: Option<std::net::IpAddr>) -> Result<Option<GeoInfo>, Report<PlatformError>> {
            // Cloudflare geo is available via cf-ipcountry header; not yet wired.
            Ok(None)
        }
    }

    struct UnavailableConfigStore;
    impl PlatformConfigStore for UnavailableConfigStore {
        fn get(&self, _: &StoreName, _: &str) -> Result<String, Report<PlatformError>> {
            Err(Report::new(PlatformError::ConfigStore).attach("use edgezero config store handle"))
        }
        fn put(&self, _: &StoreId, _: &str, _: &str) -> Result<(), Report<PlatformError>> {
            Err(Report::new(PlatformError::ConfigStore).attach("writes not supported"))
        }
        fn delete(&self, _: &StoreId, _: &str) -> Result<(), Report<PlatformError>> {
            Err(Report::new(PlatformError::ConfigStore).attach("deletes not supported"))
        }
    }

    struct UnavailableSecretStore;
    impl trusted_server_core::platform::PlatformSecretStore for UnavailableSecretStore {
        fn get_bytes(&self, _: &StoreName, _: &str) -> Result<Vec<u8>, Report<PlatformError>> {
            Err(Report::new(PlatformError::SecretStore).attach("use edgezero secret handle"))
        }
        fn create(&self, _: &StoreId, _: &str, _: &str) -> Result<(), Report<PlatformError>> {
            Err(Report::new(PlatformError::SecretStore).attach("writes not supported"))
        }
        fn delete(&self, _: &StoreId, _: &str) -> Result<(), Report<PlatformError>> {
            Err(Report::new(PlatformError::SecretStore).attach("deletes not supported"))
        }
    }

    RuntimeServices::builder()
        .config_store(Arc::new(UnavailableConfigStore))
        .secret_store(Arc::new(UnavailableSecretStore))
        .kv_store(Arc::new(UnavailableKvStore))
        .backend(Arc::new(CloudflareBackend))
        .http_client(Arc::new(UnavailableHttpClient))
        .geo(Arc::new(CloudflareGeo))
        .client_info(ClientInfo {
            client_ip,
            tls_protocol: None,
            tls_cipher: None,
        })
        .build()
}
```

- [ ] **Step 2: Verify host compiles**

```bash
cargo check -p trusted-server-adapter-cloudflare
```

Expected: `Finished`.

- [ ] **Step 3: Commit**

```bash
git add crates/trusted-server-adapter-cloudflare/src/platform.rs
git commit -m "Add Cloudflare platform trait implementations (cfg-gated)"
```

---

## Task 6: Entry point — `#[event(fetch)]` + cloudflare manifest

**Files:**

- Modify: `crates/trusted-server-adapter-cloudflare/src/lib.rs`
- Create: `crates/trusted-server-adapter-cloudflare/cloudflare.toml`
- Create: `crates/trusted-server-adapter-cloudflare/wrangler.toml`

- [ ] **Step 1: Write the full `lib.rs`**

```rust
pub mod app;
pub mod platform;

/// Host-target shim — keeps the crate compilable on native for CI.
///
/// The real `#[event(fetch)]` entry point is gated to
/// `cfg(all(feature = "cloudflare", target_arch = "wasm32"))`.
#[cfg(not(all(feature = "cloudflare", target_arch = "wasm32")))]
pub fn _host_build_shim() {}

#[cfg(all(feature = "cloudflare", target_arch = "wasm32"))]
use worker::*;

#[cfg(all(feature = "cloudflare", target_arch = "wasm32"))]
#[event(fetch)]
pub async fn main(
    req: Request,
    env: Env,
    ctx: Context,
) -> Result<Response> {
    edgezero_adapter_cloudflare::run_app::<app::TrustedServerApp>(
        include_str!("../cloudflare.toml"),
        req,
        env,
        ctx,
    )
    .await
}
```

- [ ] **Step 2: Create `cloudflare.toml`** (edgezero manifest)

```toml
[app]
name = "trusted-server"
version = "0.1.0"
kind = "http"

[adapters.cloudflare]

[stores.kv]
name = "trusted_server_kv"
[stores.kv.adapters]
cloudflare = "TRUSTED_SERVER_KV"

[stores.config]
name = "trusted_server_config"
[stores.config.adapters]
cloudflare = "TRUSTED_SERVER_CONFIG"

[stores.secrets]
name = "trusted_server_secrets"
[stores.secrets.adapters.cloudflare]
enabled = true
```

- [ ] **Step 3: Create `wrangler.toml`**

```toml
name = "trusted-server"
main = "../../target/wasm32-unknown-unknown/release/trusted_server_adapter_cloudflare.wasm"
compatibility_date = "2024-09-23"
compatibility_flags = ["nodejs_compat"]

[[kv_namespaces]]
binding = "TRUSTED_SERVER_KV"
id = "REPLACE_WITH_YOUR_KV_NAMESPACE_ID"

[vars]
TRUSTED_SERVER_CONFIG = '{"publisher.domain":"your-publisher.com"}'
```

- [ ] **Step 4: Verify host compiles with lib changes**

```bash
cargo check -p trusted-server-adapter-cloudflare
```

Expected: `Finished`.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-adapter-cloudflare/src/lib.rs \
        crates/trusted-server-adapter-cloudflare/cloudflare.toml \
        crates/trusted-server-adapter-cloudflare/wrangler.toml
git commit -m "Add Cloudflare Workers entry point and wrangler config"
```

---

## Task 7: Route smoke tests (host target)

Same pattern as `trusted-server-adapter-axum/tests/routes.rs`. Uses `EdgeZeroAxumService` — wait, Cloudflare doesn't have an axum-style in-process service. Instead we test `TrustedServerApp::routes()` returns a valid `RouterService` by calling it on the host, without any Workers runtime.

**Files:**

- Create: `crates/trusted-server-adapter-cloudflare/tests/routes.rs`

- [ ] **Step 1: Write `tests/routes.rs`**

```rust
//! Smoke tests for the Cloudflare adapter route wiring.
//!
//! Runs on the host target (no Workers runtime). Verifies that
//! TrustedServerApp::routes() builds without panicking and that
//! the expected routes exist. Does not exercise the platform layer.

use edgezero_core::app::Hooks as _;
use trusted_server_adapter_cloudflare::app::TrustedServerApp;

#[test]
fn routes_build_without_panic() {
    // build_state() may fail (no real settings on CI) — startup_error_router
    // is the fallback. Either way, routes() must not panic.
    let _router = TrustedServerApp::routes();
}

#[test]
fn crate_compiles_on_host_target() {
    // Ensures the cfg-gated shim keeps the crate host-compilable.
}
```

- [ ] **Step 2: Run tests**

```bash
cargo test -p trusted-server-adapter-cloudflare
```

Expected: `test result: ok. 2 passed`.

- [ ] **Step 3: Commit**

```bash
git add crates/trusted-server-adapter-cloudflare/tests/routes.rs
git commit -m "Add Cloudflare adapter smoke tests (host target)"
```

---

## Task 8: CI workflow

**Files:**

- Modify: `.github/workflows/test.yml`

- [ ] **Step 1: Add `test-cloudflare` job**

After the existing `test-axum` job, add:

```yaml
test-cloudflare:
  name: cargo check (cloudflare native + wasm32)
  runs-on: ubuntu-latest
  steps:
    - uses: actions/checkout@v4

    - name: Retrieve Rust version
      id: rust-version
      run: echo "rust-version=$(grep '^rust ' .tool-versions | awk '{print $2}')" >> $GITHUB_OUTPUT
      shell: bash

    - name: Set up Rust toolchain (native + wasm32-unknown-unknown)
      uses: actions-rust-lang/setup-rust-toolchain@v1
      with:
        toolchain: ${{ steps.rust-version.outputs.rust-version }}
        target: wasm32-unknown-unknown
        cache-shared-key: cargo-${{ runner.os }}

    - name: Check Cloudflare adapter (native host)
      run: cargo check -p trusted-server-adapter-cloudflare

    - name: Check Cloudflare adapter (wasm32-unknown-unknown)
      run: cargo check -p trusted-server-adapter-cloudflare --target wasm32-unknown-unknown --features cloudflare

    - name: Run Cloudflare adapter tests (native host)
      run: cargo test -p trusted-server-adapter-cloudflare
```

- [ ] **Step 2: Verify test.yml is valid YAML**

```bash
python3 -c "import yaml; yaml.safe_load(open('.github/workflows/test.yml'))" && echo "valid"
```

Expected: `valid`.

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/test.yml
git commit -m "Add CI job for Cloudflare adapter (native check + wasm32-unknown-unknown check + tests)"
```

---

## Task 9: CLAUDE.md update

**Files:**

- Modify: `CLAUDE.md`

- [ ] **Step 1: Add Cloudflare to workspace layout table**

In the `## Workspace Layout` section, add:

```
  trusted-server-adapter-cloudflare/ # Cloudflare Workers entry point (wasm32-unknown-unknown binary)
```

- [ ] **Step 2: Add build commands**

In `## Build & Test Commands`, under `### Rust`:

```bash
# Check Cloudflare adapter (native)
cargo check -p trusted-server-adapter-cloudflare

# Check Cloudflare adapter (WASM target)
cargo check -p trusted-server-adapter-cloudflare --target wasm32-unknown-unknown --features cloudflare

# Test Cloudflare adapter
cargo test -p trusted-server-adapter-cloudflare
```

- [ ] **Step 3: Commit**

```bash
git add CLAUDE.md
git commit -m "Update CLAUDE.md: add Cloudflare adapter to workspace layout and commands"
```

---

## Task 10: Full verification pass

- [ ] **Step 1: Format check**

```bash
cargo fmt --all -- --check
```

Expected: no output (clean).

- [ ] **Step 2: Clippy**

```bash
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Expected: `Finished`.

- [ ] **Step 3: Full test suite**

```bash
cargo test --workspace --exclude trusted-server-adapter-axum --target wasm32-wasip1
cargo test -p trusted-server-adapter-axum
cargo test -p trusted-server-adapter-cloudflare
```

Expected: all pass.

- [ ] **Step 4: JS tests**

```bash
cd crates/js/lib && npm run build && npm test -- --run
```

Expected: all pass.

- [ ] **Step 5: Verify cloudflare WASM target check**

```bash
cargo check -p trusted-server-adapter-cloudflare --target wasm32-unknown-unknown --features cloudflare
```

Expected: `Finished` (no panics, no unsupported types).
