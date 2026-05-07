# Routing Test Harness Design

**Date:** 2026-04-17  
**Branch:** arena-vcl-replacement  
**Status:** Approved

## Goal

Add integration tests for multi-backend routing to the existing `crates/integration-tests/` harness. Tests verify that incoming requests are routed to the correct origin based on `Host` header (domain routing), `www.` prefix stripping, URL path prefix (path routing), and unknown-host fallback.

## Architecture

```
scripts/integration-tests.sh  (existing — routing build step appended)
  ↓ cargo build --features routing-tests  →  routing WASM
  ↓ ROUTING_WASM_PATH=... cargo test -p integration-tests --test routing
      ↓
      MockOrigin × 4  (TcpListener threads, fixed response bodies)
      ↓
      FastlyViceroy::spawn(routing.wasm)
      ↓
      reqwest requests with spoofed Host headers
      ↓
      assert response body == expected origin identifier
```

The routing WASM is built with `ROUTING_TEST_BACKENDS=1` set in the environment, which tells `build.rs` (in `trusted-server-core`) to embed `test-backends.toml` instead of `backends.toml`. An env var is used rather than a Cargo feature because `build.rs` lives in `trusted-server-core` — a feature on `trusted-server-adapter-fastly` would not set `CARGO_FEATURE_*` in a dependency's build script.

If `ROUTING_WASM_PATH` is not set, all tests return early — `cargo test --workspace` compiles and passes silently without the dedicated build step.

## New Files

### `crates/trusted-server-adapter-fastly/test-backends.toml`

```toml
[[backends]]
id = "site-a"
origin_url = "http://127.0.0.1:19091"
domains = ["site-a.test", "www.site-a.test"]

[[backends]]
id = "site-b"
origin_url = "http://127.0.0.1:19092"
domains = ["site-b.test"]

[[backends]]
id = "api"
origin_url = "http://127.0.0.1:19093"

  [[backends.path_patterns]]
  host = "*"
  path_prefix = "/.api/"
```

Default publisher origin → `http://127.0.0.1:19090` (set via `TRUSTED_SERVER__PUBLISHER__ORIGIN_URL` at build time).

Ports 19090–19093 are in the ephemeral-adjacent range, unlikely to conflict in CI.

### `crates/integration-tests/tests/routing.rs`

Shared `RoutingHarness` via `OnceLock`:

```rust
struct RoutingHarness {
    _origins: Vec<MockOrigin>,  // keeps listener threads alive
    _viceroy: ViceroyProcess,
    base_url: String,
}
```

`MockOrigin` — wraps a `TcpListener` on a fixed port. Each connection: drain request headers, write a minimal HTTP/1.1 200 response with a fixed body (`"site-a"`, `"site-b"`, `"api"`, `"default"`). Stopped on `Drop`. No new dependencies.

## Modified Files

| File                                  | Change                                                                                              |
| ------------------------------------- | --------------------------------------------------------------------------------------------------- |
| `crates/trusted-server-core/build.rs` | When `ROUTING_TEST_BACKENDS=1` env var is set, read `test-backends.toml` instead of `backends.toml` |
| `scripts/integration-tests.sh`        | Append routing WASM build + test run                                                                |

## Test Scenarios

| Test                      | Host              | Path          | Expected body |
| ------------------------- | ----------------- | ------------- | ------------- |
| `domain_routes_to_site_a` | `site-a.test`     | `/`           | `"site-a"`    |
| `domain_routes_to_site_b` | `site-b.test`     | `/`           | `"site-b"`    |
| `www_prefix_stripped`     | `www.site-a.test` | `/`           | `"site-a"`    |
| `path_routes_to_api`      | `site-b.test`     | `/.api/users` | `"api"`       |
| `unknown_host_falls_back` | `unknown.test`    | `/`           | `"default"`   |

## CI Integration

Appended to `scripts/integration-tests.sh`:

```bash
# Routing tests
ROUTING_TEST_BACKENDS=1 \
TRUSTED_SERVER__PUBLISHER__ORIGIN_URL="http://127.0.0.1:19090" \
  cargo build --package trusted-server-adapter-fastly \
              --release --target wasm32-wasip1

ROUTING_WASM_PATH="target/wasm32-wasip1/release/trusted-server-adapter-fastly.wasm" \
  cargo test -p integration-tests --test routing
```

## Out of Scope

- TLS/`certificate_check` routing — covered by unit tests in `backend_router.rs`
- Path regex patterns — `path_prefix` covers the core routing logic; regex is an extension of the same code path
- Browser-level routing assertions — not needed; routing is server-side only
