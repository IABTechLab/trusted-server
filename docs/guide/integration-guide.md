# Integration Guide

This document explains how to integrate a new integration module with the Trusted Server runtime. The workflow mirrors the built-in `testlight` sample in `crates/common/src/integrations/testlight.rs`.

## Architecture Overview

| Component | Purpose |
| --- | --- |
| `crates/common/src/integrations/registry.rs` | Defines the `IntegrationProxy`, `IntegrationAttributeRewriter`, and `IntegrationScriptRewriter` traits and hosts the `IntegrationRegistry`, which drives proxy routing and HTML/text rewrites. |
| `Settings::integrations` (`crates/common/src/settings.rs`) | Free-form JSON blob keyed by integration ID. Use `IntegrationSettings::insert_config` to seed configs; each module deserializes and validates (`validator::Validate`) its own config and exposes an `enabled` flag so the core settings schema stays stable. |
| Fastly entrypoint (`crates/fastly/src/main.rs`) | Instantiates the registry once per request, routes `/integrations/<id>/…` requests to the appropriate proxy, and passes the registry to the publisher origin proxy so HTML rewriting remains integration-aware. |
| `html_processor.rs` | Applies first-party URL rewrites, injects the Trusted Server JS shim, and lets integrations override attribute values (for example to swap script URLs). |

## Step-by-Step Integration

### 1. Define Integration Configuration

Add a `trusted-server.toml` block and any environment overrides under `TRUSTED_SERVER__INTEGRATIONS__<ID>__*`. Configuration values are exposed to your module via `Settings::integration_config(<id>)`.

```toml
[integrations.my_integration]
endpoint = "https://example.com/api"
timeout_ms = 1000
rewrite_scripts = true
```

### 2. Create the Integration Module

Add a module under `crates/common/src/integrations/<id>/mod.rs` (see `crates/common/src/integrations/testlight.rs` for reference) and expose it in `crates/common/src/integrations/mod.rs`.

Key pieces:

```rust
#[derive(Deserialize, Validate)]
struct MyIntegrationConfig {
    #[serde(default = "default_enabled")]
    enabled: bool,
    // …
}

impl IntegrationConfig for MyIntegrationConfig {
    fn is_enabled(&self) -> bool { self.enabled }
}

pub struct MyIntegration {
    config: MyIntegrationConfig,
}

pub fn build(settings: &Settings) -> Option<Arc<MyIntegration>> {
    let config = settings
        .integration_config::<MyIntegrationConfig>("my_integration")
        .ok()
        .flatten()?;
    Some(Arc::new(MyIntegration { config }))
}

// Tests or scaffolding code can seed configs without hand-writing JSON:
settings
    .integrations
    .insert_config(
        "my_integration",
        &serde_json::json!({
            "enabled": true,
            "endpoint": "https://example.com/api"
        }),
    )?;
```

`Settings::integration_config::<T>` automatically deserializes the raw JSON blob, runs [`validator`](https://docs.rs/validator/latest/validator/) on the type, and drops configs whose `is_enabled` returns `false`. Always derive/implement `Validate` for schema enforcement and implement `IntegrationConfig` (typically wrapping a `#[serde(default)] enabled` flag) so operators can toggle integrations without code changes.

### 3. Return an IntegrationRegistration

Each integration registers itself via a `register` function that returns an `IntegrationRegistration`. This object describes which HTTP proxies and HTML rewrites the integration exposes:

```rust
pub fn register(settings: &Settings) -> Option<IntegrationRegistration> {
    let integration = build(settings)?;
    Some(
        IntegrationRegistration::builder("my_integration")
            .with_proxy(integration.clone())
            .with_attribute_rewriter(integration.clone())
            .with_script_rewriter(integration)
            .with_asset("my_integration")
            .build(),
    )
}
```

Any combination of the three vectors may be populated. Modules that only need HTML rewrites can skip the `proxies` field altogether, and vice versa. The registry automatically iterates over the static builder list in `crates/common/src/integrations/mod.rs`, so adding the new `register` function is enough to make the integration discoverable.

### 4. Implement IntegrationProxy for Endpoints

Implement the trait from `registry.rs` when your integration needs its own HTTP entrypoint:

```rust
#[async_trait(?Send)]
impl IntegrationProxy for MyIntegration {
    fn integration_name(&self) -> &'static str {
        "my_integration"
    }

    fn routes(&self) -> Vec<IntegrationEndpoint> {
        vec![
            self.post("/auction"),
            self.get("/status"),
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

::: tip Route Helpers
Use the provided helper methods to automatically namespace your routes under `/integrations/{integration_name()}/`. Available helpers: `get()`, `post()`, `put()`, `delete()`, and `patch()`. This lets you define routes with just their relative paths (e.g., `self.post("/auction")` becomes `"/integrations/my_integration/auction"`).
:::

Routes are matched verbatim in `crates/fastly/src/main.rs`, so stick to stable paths and register whichever HTTP methods you need. **New integrations should namespace their routes under `/integrations/{INTEGRATION_NAME}/`** using the helper methods for consistency, but you can define routes manually if needed (e.g., for backwards compatibility).

The shared context already injects Trusted Server logging, headers, and error handling; the handler only needs to deserialize the request, call the upstream endpoint, and stamp integration-specific headers.

#### Proxying Upstream Requests

Use the shared helper in `crates/common/src/proxy.rs` to forward requests so you automatically get the same header copying, redirect handling, HTML/CSS rewrite behavior, and synthetic ID handling the first-party proxy uses:

```rust
use crate::proxy::{proxy_request, ProxyRequestConfig};
use fastly::http::{header, HeaderValue};

let payload = serde_json::to_vec(&my_body)?;
let response = proxy_request(
    settings,
    req,
    ProxyRequestConfig::new(&self.config.endpoint)
        .with_body(payload)
        .with_header(header::CONTENT_TYPE, HeaderValue::from_static("application/json"))
        .with_streaming(), // stream passthrough; disable if you need HTML rewrites
)
.await?;
```

Set `forward_synthetic_id` to `false` if the upstream should not receive the caller's synthetic ID (`Testlight` does this), and disable `follow_redirects` if you need to surface redirects directly to the caller.

**Streaming passthrough example:**

```rust
let response = proxy_request(
    settings,
    req,
    ProxyRequestConfig::new("https://example.com/pixel")
        .with_streaming() // no HTML/CSS rewrites; preserves origin compression
);
```

::: info When to Use Streaming
Use streaming when the upstream response is binary or large and you do not need creative rewrites. Keep the default (non-streaming) mode when you want HTML/CSS content rewritten through the existing creative pipeline.
:::

### 5. Implement HTML Rewrite Hooks (Optional)

If the integration needs to rewrite script/link tags or inject HTML, implement `IntegrationAttributeRewriter` for attribute mutation and `IntegrationScriptRewriter` for inline `<script>` or text content rewrites. Both traits return typed actions (`AttributeRewriteAction`, `ScriptRewriteAction`) so you can keep existing markup, swap values, or drop elements entirely.

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
    ) -> AttributeRewriteAction {
        if attr_value.contains("cdn.example.com/legacy.js") {
            // Drop remote script entirely – unified bundle already contains the logic.
            AttributeRewriteAction::remove_element()
        } else if attr_name == "src" {
            AttributeRewriteAction::replace(tsjs::unified_script_src())
        } else {
            AttributeRewriteAction::keep()
        }
    }
}

impl IntegrationScriptRewriter for MyIntegration {
    fn integration_id(&self) -> &'static str { "my_integration" }
    fn selector(&self) -> &'static str { "script#__NEXT_DATA__" }

    fn rewrite(
        &self,
        content: &str,
        ctx: &IntegrationScriptContext<'_>,
    ) -> ScriptRewriteAction {
        if let Some(rewritten) = try_rewrite_next_payload(content) {
            ScriptRewriteAction::replace(rewritten)
        } else {
            ScriptRewriteAction::keep()
        }
    }
}
```

`html_processor.rs` calls these hooks after applying the standard origin→first-party rewrite, so you can simply swap URLs, append query parameters, or mutate inline JSON. Use this to point `<script>` tags at your own tsjs-managed bundle (for example, `/static/tsjs=tsjs-testlight.min.js`) or to rewrite embedded Next.js payloads.

If you need to inject HTML into `<head>` (for example to enqueue `tsjs.setConfig(...)`), implement `IntegrationHeadInjector` and register it with `.with_head_injector(...)`. Snippets are inserted before the unified TSJS bundle.

::: warning Removing Elements
Returning `AttributeRewriteAction::remove_element()` (or `ScriptRewriteAction::RemoveNode` for inline content) removes the element entirely, so integrations can drop publisher-provided markup when the Trusted Server already injects a safe alternative. Prebid, for example, simply removes `prebid.js` because the unified TSJS bundle is injected automatically at the start of `<head>`.
:::

### 6. Register the Module

Add the module to `crates/common/src/integrations/mod.rs`'s builder list. The registry will call its `register` function automatically. Once registered:

- `crates/fastly/src/main.rs` automatically exposes the declared route(s).
- `handle_publisher_request` receives the same registry so HTML responses get integration shims without further code changes.
- `IntegrationRegistry::registered_integrations()` exposes a machine-readable summary of hooks for tests, tooling, or diagnostics.
- Declared assets are injected automatically into `<head>`; the runtime emits `<script async data-tsjs-integration="<name>">` tags for every bundle discovered through `.with_asset(...)`.

### 7. Provide Static Assets (If Needed)

Place any integration-specific JavaScript entrypoint under `crates/js/lib/src/integrations/` (for example, `crates/js/lib/src/integrations/testlight.ts`). The shared `npm run build` script automatically discovers every file in that directory and produces a bundle named `tsjs-<entry>.js`, which the Rust crate embeds as `/static/tsjs=tsjs-<entry>.min.js`.

Integrations that ship additional JS (such as Testlight) typically expose a `shim_src` config and rewrite publisher tags to point at that URL. Others (like Prebid) can simply drop the legacy tag because the unified bundle is injected automatically at the start of `<head>`.

### 8. Test Locally

1. Add minimal config (`trusted-server.toml` + `.env.*` overrides).
2. Run `cargo fmt && cargo clippy --all-targets --all-features`.
3. Execute targeted tests, e.g. `cargo test -p trusted-server-common html_processor`.
4. Use `fastly compute serve` (with Viceroy installed) to hit `/integrations/<id>/…` and fetch HTML from your origin to confirm rewrites are applied.

::: tip Testing Strategy
For unit tests, prefer exposing helper constructors that accept a synthetic `shim_src` so your tests can point rewriters at a deterministic URL without touching the Tsjs build artifacts.
:::

By following these steps you can ship independent integration modules that plug into the Trusted Server runtime without modifying the Fastly entrypoint or HTML processor each time.

## Existing Integrations

Two built-in integrations demonstrate how the framework pieces fit together:

### Testlight

**Purpose**: Sample partner stub showing request proxying, attribute rewrites, and asset injection.

**Key files**:
- `crates/common/src/integrations/testlight.rs` - Rust implementation
- `crates/js/lib/src/integrations/testlight.ts` - TypeScript shim

### Prebid

**Purpose**: Production Prebid Server bridge that owns `/ad/render` & `/ad/auction`, injects synthetic IDs, rewrites creatives/notification URLs, and removes publisher-supplied Prebid scripts because the shim already ships in the unified TSJS build.

**Key files**:
- `crates/common/src/integrations/prebid.rs` - Rust implementation  
- `crates/js/lib/src/ext/prebidjs.ts` - TypeScript shim

#### Prebid Integration Details

Prebid applies the same steps outlined above with a few notable patterns:

**1. Typed Configuration**

`PrebidIntegrationConfig` lives alongside the integration module (`crates/common/src/integrations/prebid.rs`), implements `IntegrationConfig + Validate`, and exposes an `enabled` flag so operators can toggle it without code changes. Configuration lives under `[integrations.prebid]`:

```toml
[integrations.prebid]
enabled = true
server_url = "https://prebid.example/openrtb2/auction"
timeout_ms = 1200
bidders = ["equativ", "sampleBidder"]
# script_patterns = ["/static/prebid/*"]
```

Tests or scaffolding can inject configs by calling `settings.integrations.insert_config("prebid", &serde_json::json!({...}))`, the same helper that other integrations use.

**2. Routes Owned by the Integration**

`IntegrationProxy::routes` declares the `/ad/render` (GET) and `/ad/auction` (POST) endpoints. Both handlers share helpers that shape OpenRTB payloads, inject synthetic IDs + geo/request-signing context, forward requests via `ensure_backend_from_url`, and run the HTML creative rewrites before responding. These routes are intentionally un-namespaced to match the TSJS client.

**3. HTML Rewrites Through the Registry**

When the integration is enabled, the `IntegrationAttributeRewriter` removes any `<script src="prebid*.js">` or `<link href=…>` references that match `script_patterns`. The unified TSJS bundle is injected at the start of `<head>`, so dropping the publisher assets prevents duplicate downloads and still runs before any inline `pbjs` config.

**4. TSJS Assets & Testing**

The shim implementation lives in `crates/js/lib/src/ext/prebidjs.ts`. Tests typically assert that publisher references disappear, relying on the html processor's unified bundle injection to deliver the shim.

Reusing these patterns makes it straightforward to convert additional legacy flows (for example, Next.js rewrites) into first-class integrations.

## Future Improvements

We plan to expand integration capabilities in several areas:

1. **Declarative Routing & Middleware** - Richer endpoints (path params, shared middleware, structured context) beyond simple method/path matching.
2. **Granular HTML Hooks** - Ordered selectors, head/body injection points, and DOM-aware helpers so multiple integrations can safely collaborate.
3. **Integration Manifest** - Schema describing required bundles, routes, config validation, and feature flags to keep registration data-driven.
4. **Shared Request Utilities** - Reusable building blocks for synthetic ID injection, consent enforcement, and OpenRTB shaping.
5. **tsjs Tooling** - Auto-generated integration bundles, scaffolding for TS shims/tests, and metadata surfaced back to Rust.
6. **Testing & Observability Hooks** - Integration-focused mocks, local harnesses, and telemetry emitters for easier validation and monitoring.

Contributions toward these enhancements are welcome.

## Next Steps

- Learn about [Request Signing](/guide/request-signing) for secure communication
- Review [Architecture](/guide/architecture) for system design
- Set up [Testing](/guide/testing) for your integration
