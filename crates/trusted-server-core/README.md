# trusted-server-core

Portable application core shared by every Trusted Server adapter. It targets
both supported WASM environments and native adapter tests, so it must not depend
on an edge SDK, Tokio runtime, filesystem, socket, or host-only process API.

## Responsibilities

- `settings` and `settings_data` define typed application configuration,
  validation, secret references, and runtime normalization.
- `platform` defines the HTTP, backend, store, geo, client-info, and telemetry
  service boundary adapters implement.
- `publisher`, `router`, `handlers`, and `response` dispatch publisher and
  administrative requests through platform-neutral request/response types.
- `auction` builds plans, invokes providers and mediators, selects bids, and
  emits bounded telemetry events.
- `integrations` registers explicit proxy, rewrite, injection, filter,
  post-processing, provider, and browser-module capabilities.
- `ec` owns edge-cookie generation, consent decisions, identity graph access,
  and partner synchronization.
- `html_processor`, `host_rewrite`, `streaming_processor`, and `rsc_flight`
  transform eligible publisher responses without corrupting non-HTML or RSC
  payloads.
- `proxy`, `creative`, `image_optimizer`, and `asset_routes` implement bounded
  first-party asset and creative handling.
- `auth`, `request_signing`, `key_manager`, and `jwk` implement authentication
  and signing contracts.
- `cache_policy`, `template_cache`, and `template_assembly` define portable
  cache and assembly decisions; adapters supply storage and streaming I/O.
- `tsjs` selects embedded browser modules supplied by `trusted-server-js`.
- `openrtb` connects auction logic to the checked OpenRTB data model.

Adapter crates own runtime startup, SDK conversion, concrete storage, outbound
transport, and target-specific limitations. Adding an integration should use
the narrowest registration hook and the core-neutral `RuntimeServices`
boundary; see the [Integration Guide](../../docs/guide/integration-guide.md).

## Build and test

From the repository root:

```bash
cargo build-fastly
cargo test-fastly
```

The Fastly aliases compile and test the core for `wasm32-wasip1` through
Viceroy. Cloudflare and native adapter suites exercise the same core under
their target configurations. Use [TESTING.md](../../TESTING.md) for the complete
target matrix and [Architecture](../../docs/guide/architecture.md) for the
request-level system view.
