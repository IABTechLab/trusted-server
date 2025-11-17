# Integration Guide

This document explains how to integrate a new integration module with the Trusted Server
runtime. The workflow mirrors the built‑in `starlight` sample in
`crates/common/src/integrations/starlight.rs`.

## Architecture Overview

| Component | Purpose |
| --- | --- |
| `crates/common/src/integrations/registry.rs` | Defines the `IntegrationProxy` and `IntegrationAttributeRewriter` traits and hosts the `IntegrationRegistry`, which drives proxy routing and HTML rewrites. |
| `Settings::integrations` (`crates/common/src/settings.rs`) | Free‑form JSON blob keyed by integration ID. Each module deserializes its own config so the core settings schema stays stable. |
| Fastly entrypoint (`crates/fastly/src/main.rs`) | Instantiates the registry once per request, routes `/integrations/<id>/…` requests to the appropriate proxy, and passes the registry to the publisher origin proxy so HTML rewriting remains integration-aware. |
| `html_processor.rs` | Applies first‑party URL rewrites, injects the Trusted Server JS shim, and lets integrations override attribute values (for example to swap script URLs). |

## Step-by-Step Integration

### 1. Define integration configuration

Add a `trusted-server.toml` block and any environment overrides under
`TRUSTED_SERVER__INTEGRATIONS__<ID>__*`. Configuration values are exposed to your module via
`Settings::integration_config(<id>)`.

```toml
[integrations.my_integration]
endpoint = "https://example.com/api"
timeout_ms = 1000
rewrite_scripts = true
```

### 2. Create the integration module

Add a module under `crates/common/src/integrations/<id>/mod.rs` (see
`crates/common/src/integrations/starlight.rs` for reference) and expose it in
`crates/common/src/integrations/mod.rs`.

Key pieces:

```rust
#[derive(Deserialize)]
struct MyIntegrationConfig { /* … */ }

pub struct MyIntegration {
    config: MyIntegrationConfig,
}

pub fn build(settings: &Settings) -> Option<Arc<MyIntegration>> {
    let raw = settings.integration_config("my_integration")?;
    let config: MyIntegrationConfig = serde_json::from_value(raw.clone()).ok()?;
    Some(Arc::new(MyIntegration { config }))
}
```

### 3. Implement `IntegrationProxy` for endpoints

Implement the trait from `registry.rs` when your integration needs its own HTTP entrypoint:

```rust
#[async_trait(?Send)]
impl IntegrationProxy for MyIntegration {
    fn routes(&self) -> Vec<IntegrationEndpoint> {
        vec![
            IntegrationEndpoint::post("/integrations/my-integration/auction"),
            IntegrationEndpoint::get("/integrations/my-integration/status"),
        ]
    }

    async fn handle(
        &self,
        settings: &Settings,
        req: Request,
    ) -> Result<Response, Report<TrustedServerError>> {
        // Parse/generate synthetic IDs, forward upstream, and return the response.
    }
}
```

Routes are matched verbatim in `crates/fastly/src/main.rs`, so stick to stable paths
(`/integrations/<id>/…`) and register whichever HTTP methods you need. The shared context
already injects Trusted Server logging, headers, and error handling; the handler only
needs to deserialize the request, call the upstream endpoint, and stamp integration-specific
headers.

### 4. Implement `IntegrationAttributeRewriter` for shims (optional)

If the integration needs to rewrite script/link tags or inject HTML, implement the
`IntegrationAttributeRewriter` trait:

```rust
impl IntegrationAttributeRewriter for MyIntegration {
    fn integration_id(&self) -> &'static str { "my_integration" }

    fn handles_attribute(&self, attribute: &str) -> bool {
        attribute == "src" || attribute == "href"
    }

    fn rewrite(
        &self,
        attr_name: &str,
        attr_value: &str,
        ctx: &IntegrationAttributeContext<'_>,
    ) -> Option<String> {
        // Return Some(new_value) to replace the attribute or None to leave it unchanged.
    }
}
```

`html_processor.rs` calls this hook after applying the standard origin→first‑party rewrite,
so you can simply swap URLs or append query parameters. Use this to point `<script>` tags
at your own tsjs-managed bundle (for example, `/static/tsjs=tsjs-starlight.min.js`).

### 5. Register the module

Update `IntegrationRegistry::new` (`crates/common/src/integrations/registry.rs`) to call your
`build` helper and push the resulting `Arc` into the `proxies` and/or `html_rewriters`
collections:

```rust
if let Some(integration) = crate::integrations::my_integration::build(settings) {
    inner.proxies.push(integration.clone());
    inner.html_rewriters.push(integration);
}
```

Once registered:

- `crates/fastly/src/main.rs` automatically exposes the declared route(s).
- `handle_publisher_request` receives the same registry so HTML responses get integration
  shims without further code changes.

### 6. Provide static assets (if needed)

Place any integration-specific JavaScript entrypoint under `crates/js/lib/src/integrations/`
(for example, `crates/js/lib/src/integrations/starlight.ts`). The shared `npm run build`
script automatically discovers every file in that directory and produces a bundle named
`tsjs-<entry>.js`, which the Rust crate embeds as `/static/tsjs=tsjs-<entry>.min.js`.
In your Rust module, call `tsjs::script_src("tsjs-<entry>.js")` to obtain the cache-busted
URL for rewrites (see the Starlight example for reference).

### 7. Test locally

1. Add minimal config (`trusted-server.toml` + `.env.*` overrides).
2. Run `cargo fmt && cargo clippy --all-targets --all-features`.
3. Execute targeted tests, e.g. `cargo test -p trusted-server-common html_processor`.
4. Use `fastly compute serve` (with Viceroy installed) to hit `/integrations/<id>/…` and
   fetch HTML from your origin to confirm rewrites are applied.

By following these steps you can ship independent integration modules that plug into the
Trusted Server runtime without modifying the Fastly entrypoint or HTML processor each
time.

## Future Improvements

We plan to expand integration capabilities in several areas:

1. **Declarative Routing & Middleware** – richer endpoints (path params, shared middleware, structured context) beyond simple method/path matching.
2. **Granular HTML Hooks** – ordered selectors, head/body injection points, and DOM-aware helpers so multiple integrations can safely collaborate.
3. **Integration Manifest** – schema describing required bundles, routes, config validation, and feature flags to keep registration data-driven.
4. **Shared Request Utilities** – reusable building blocks for synthetic ID injection, consent enforcement, and OpenRTB shaping.
5. **tsjs Tooling** – auto-generated integration bundles, scaffolding for TS shims/tests, and metadata surfaced back to Rust.
6. **Testing & Observability Hooks** – integration-focused mocks, local harnesses, and telemetry emitters for easier validation and monitoring.

Contributions toward these enhancements are welcome.
