# Integration Tests

End-to-end tests that verify the trusted server against real frontend
containers using [Testcontainers](https://testcontainers.com/) and
[Playwright](https://playwright.dev/).

## Prerequisites

- **Docker** — running and accessible
- **Viceroy** — Fastly local simulator (`cargo install viceroy`)
- **wasm32-wasip1 target** — `rustup target add wasm32-wasip1`
- **Node.js** (LTS) — for browser tests only

## Quick start

### HTTP-level tests

```bash
./scripts/integration-tests.sh
```

This script handles everything:

1. Builds the WASM binary with test-specific config (origin URL pointing to
   Docker containers)
2. Builds the WordPress and Next.js Docker images
3. Runs all integration tests sequentially

### Browser tests

```bash
./scripts/integration-tests-browser.sh
```

This script:

1. Builds the WASM binary and Docker images (same as above)
2. Installs Playwright and Chromium
3. Runs browser tests for Next.js and WordPress sequentially

### Run a single test

```bash
# HTTP-level
./scripts/integration-tests.sh test_wordpress_fastly
./scripts/integration-tests.sh test_nextjs_fastly

# Browser — single framework
cd crates/integration-tests/browser
TEST_FRAMEWORK=nextjs npx playwright test
TEST_FRAMEWORK=wordpress npx playwright test
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
| `test-nextjs:latest` | `fixtures/frameworks/nextjs/Dockerfile` | Next.js 14 standalone app with 4 pages, API routes, forms, shared navigation, and deferred scripts |

Both images include test fixtures with absolute origin URLs (`ORIGIN_HOST` env
var) so the trusted server's URL rewriting can be verified.

### Build images manually

```bash
docker build -t test-wordpress:latest \
  crates/integration-tests/fixtures/frameworks/wordpress/

docker build \
  --build-arg NODE_VERSION="$(grep '^nodejs ' .tool-versions | awk '{print $2}')" \
  -t test-nextjs:latest \
  crates/integration-tests/fixtures/frameworks/nextjs/
```

## Test scenarios

### HTTP-level — standard (all frameworks)

| Scenario | What it tests |
|---|---|
| `HtmlInjection` | Exactly one `<script id="trustedserver-js" src="/static/tsjs=...">` in proxied HTML |
| `ScriptServing` | `/static/tsjs=tsjs-unified.min.js` returns JavaScript with bundle markers |
| `AttributeRewriting` | `href`/`src` URLs with origin host are rewritten to proxy host (including inside ad slots) |
| `ScriptServingUnknownFile404` | Unknown `/static/tsjs=...` paths return 404, not HTML fallback |

### HTTP-level — Next.js custom

| Scenario | What it tests |
|---|---|
| `NextJsRscFlight` | RSC Flight responses are not corrupted (no HTML, no script injection) |
| `NextJsServerActions` | POST requests pass through proxy; unknown actions return 404/soft-404 |
| `NextJsApiRoute` | API routes (`/api/hello`) return JSON without HTML injection |
| `NextJsFormAction` | `<form action>` URLs rewritten from origin host to proxy host |

### HTTP-level — WordPress custom

| Scenario | What it tests |
|---|---|
| `WordPressAdminInjection` | `/wp-admin/` pages receive script injection (documents current behavior) |

### Browser-level — shared (all frameworks)

| Spec | What it tests |
|---|---|
| `script-injection` | `script#trustedserver-js` present in live DOM, no console errors |
| `script-bundle` | JS bundle loads with 200, no parse/runtime errors, correct content type |

### Browser-level — Next.js

| Spec | What it tests |
|---|---|
| `navigation` | 4-page SPA navigation chain preserves injection without full reload, back button works, deferred route script executes after SPA transition |
| `api-passthrough` | API routes return JSON without script injection (`/api/hello`, `/api/data`) |
| `form-rewriting` | `<form action>` URL rewritten from origin to proxy on `/contact` page |

### Browser-level — WordPress

| Spec | What it tests |
|---|---|
| `admin-injection` | `/wp-admin/` has script tag in live DOM |

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
browser/
  playwright.config.ts # Playwright configuration (chromium, workers: 1)
  global-setup.ts      # Starts Docker container + Viceroy before tests
  global-teardown.ts   # Stops container + Viceroy after tests
  helpers/
    infra.ts           # Docker + Viceroy spawn/kill logic
    wait-for-ready.ts  # Health check polling
    state.ts           # Reads shared state file between setup/tests/teardown
  tests/
    shared/            # Tests that run for all frameworks
    nextjs/            # Next.js-specific browser tests
    wordpress/         # WordPress-specific browser tests
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
4. **HTTP tests**: reqwest sends requests to Viceroy and asserts on responses
5. **Browser tests**: Playwright opens Chromium pointing at Viceroy and verifies
   script injection, bundle loading, and client-side navigation in a real browser

### Why `--test-threads=1` / `workers: 1`

All tests share the same fixed origin port (8888). The trusted server config is
baked into the WASM binary at compile time with this port, so only one Docker
container can be bound to it at a time.

## CI

Integration tests run in a separate workflow (`.github/workflows/integration-tests.yml`)
triggered by:

- Push to `main`
- PR approval
- Manual dispatch

Two jobs run in parallel:

1. **integration-tests** — HTTP-level tests (Rust + testcontainers)
2. **browser-tests** — Playwright tests (Node.js + Chromium)

They are **not** part of `cargo test --workspace` because the integration-tests
crate requires a native target while the workspace default is `wasm32-wasip1`.

## Dependency maintenance

`crates/integration-tests` is intentionally excluded from the workspace, so it
keeps its own `Cargo.lock`.

Shared direct dependency versions are checked in CI by
`scripts/check-integration-dependency-versions.sh`. When updating a dependency
that exists in both manifests:

1. Update the version in both `Cargo.toml` files.
2. Regenerate the nested lockfile with
   `cargo generate-lockfile --manifest-path crates/integration-tests/Cargo.toml`.
3. Ensure the workspace and integration-test lockfiles resolve the same version
   for that shared dependency.

## Known gaps

- **GDPR consent propagation** — the consent field exists in `AuctionRequest`
  but is not yet populated or forwarded. Requires implementation first.
- **Next.js integration features** — the WASM binary is built without
  `integrations.nextjs` enabled, so Next.js-specific rewriters/post-processors
  are not exercised. RSC Flight/Server Actions tests are compatibility smoke
  tests only.
- **GTM integration** — not enabled in test config. Has unit coverage in
  `google_tag_manager.rs`.
