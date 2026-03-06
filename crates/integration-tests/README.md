# Integration Tests

End-to-end tests that verify the trusted server against real frontend
containers using [Testcontainers](https://testcontainers.com/).

## Prerequisites

- **Docker** — running and accessible
- **Viceroy** — Fastly local simulator (`cargo install viceroy`)
- **wasm32-wasip1 target** — `rustup target add wasm32-wasip1`

## Quick start

```bash
./scripts/integration-tests.sh
```

This script handles everything:

1. Builds the WASM binary with test-specific config (origin URL pointing to
   Docker containers)
2. Builds the WordPress and Next.js Docker images
3. Runs all integration tests sequentially

### Run a single test

```bash
./scripts/integration-tests.sh test_wordpress_fastly
./scripts/integration-tests.sh test_nextjs_fastly
```

### Verbose output

```bash
./scripts/integration-tests.sh --nocapture
```

## Docker images

Two test images are built from fixtures in `fixtures/frameworks/`:

| Image | Dockerfile | Description |
|---|---|---|
| `test-wordpress:latest` | `fixtures/frameworks/wordpress/Dockerfile` | PHP built-in server with a minimal test theme |
| `test-nextjs:latest` | `fixtures/frameworks/nextjs/Dockerfile` | Next.js 14 standalone app with test pages |

Both images include test fixtures with absolute origin URLs (`ORIGIN_HOST` env
var) so the trusted server's URL rewriting can be verified.

### Build images manually

```bash
docker build -t test-wordpress:latest \
  crates/integration-tests/fixtures/frameworks/wordpress/

docker build -t test-nextjs:latest \
  crates/integration-tests/fixtures/frameworks/nextjs/
```

## Test scenarios

### Standard (all frameworks)

| Scenario | What it tests |
|---|---|
| `HtmlInjection` | `<script src="/static/tsjs=...">` is present in proxied HTML |
| `ScriptServing` | `/static/tsjs=tsjs-unified.min.js` returns JavaScript with bundle markers |
| `AttributeRewriting` | `href`/`src` URLs with origin host are rewritten to proxy host |

### Next.js custom

| Scenario | What it tests |
|---|---|
| `NextJsRscFlight` | RSC Flight responses are not corrupted (no HTML, no script injection) |
| `NextJsServerActions` | POST requests pass through the proxy to the origin |

## Architecture

```
tests/
  integration.rs       # Test entry point — runs framework x runtime matrix
  common/
    assertions.rs      # HTML assertion helpers (script tag, attribute rewriting)
    runtime.rs         # Error types, RuntimeEnvironment trait, env var helpers
  environments/
    mod.rs             # Runtime registry, port allocation, health checking
    fastly.rs          # Viceroy-based Fastly Compute runtime
  frameworks/
    mod.rs             # Framework registry and FrontendFramework trait
    scenarios.rs       # Standard and custom test scenarios
    wordpress.rs       # WordPress container config
    nextjs.rs          # Next.js container config
fixtures/
  configs/
    viceroy-template.toml  # Viceroy local_server config (KV stores, secrets)
  frameworks/
    wordpress/             # WordPress Docker image source
    nextjs/                # Next.js Docker image source
```

### How it works

1. A Docker container starts for the frontend framework, mapped to a fixed
   origin port (default 8888)
2. The WASM binary is pre-built with `TRUSTED_SERVER__PUBLISHER__ORIGIN_URL`
   pointing to `http://127.0.0.1:8888` so the proxy knows where to forward
3. Viceroy spawns with the WASM binary on a random port
4. HTTP requests go to Viceroy (proxy) which forwards to the Docker container
   (origin) and processes the response
5. Assertions verify the proxied response has script injection, URL rewriting,
   etc.

### Why `--test-threads=1`

All tests share the same fixed origin port (8888). The trusted server config is
baked into the WASM binary at compile time with this port, so only one Docker
container can be bound to it at a time.

## CI

Integration tests run in a separate workflow (`.github/workflows/integration-tests.yml`)
triggered by:

- Push to `main`
- PR approval
- Manual dispatch

They are **not** part of `cargo test --workspace` because the crate requires a
native target while the workspace default is `wasm32-wasip1`.

## Not tested (out of scope)

- **GDPR consent propagation** — the consent field exists in `AuctionRequest`
  but is not yet populated or forwarded. Requires implementation first.
- **Client-side navigation** — requires a real browser (Playwright/Selenium).
  HTTP-level tests cannot verify JavaScript execution or route transitions.
