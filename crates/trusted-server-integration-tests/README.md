# trusted-server-integration-tests

Native test package for cross-adapter parity, documentation compilation, and
end-to-end publisher behavior. It is not linked into an adapter artifact.

## Test surfaces

- `tests/parity.rs` serves the Axum, Cloudflare, and Spin apps on EdgeZero's
  `AxumTestServer`, a loopback listener that applies each app's production
  policies, and compares their shared route behavior. A scripted outbound
  client in router state answers their upstream calls.
- `tests/documentation_snippets.rs` extracts the checked integration-guide
  fixture and compiles it in an isolated offline crate.
- `tests/integration.rs` exercises the Fastly (Viceroy), Axum, Cloudflare
  (workerd), and Spin runtimes against local origins, loopback bidders, and the
  WordPress and Next.js fixture containers.
- `browser/` uses Playwright and Chromium to verify script loading, navigation,
  rewriting, APS rendering, and GPT diagnostics in real pages.

The package uses the native host target. Fastly application artifacts are
compiled separately for `wasm32-wasip1`; Docker, Viceroy, Node, and Chromium
are required only by the end-to-end surfaces that invoke them.

## Run

From the repository root:

```bash
cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity
cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test documentation_snippets
./scripts/integration-tests.sh
./scripts/integration-tests-browser.sh
```

The orchestration scripts build artifacts and Docker images and write generated
Viceroy configuration under `target/integration-test-artifacts`. The browser
script stops its matching test containers on exit; build products, images, npm
dependencies, and generated configuration remain for reuse. Tests use an
isolated readable app-config fixture and do not consume an operator's
gitignored `trusted-server.toml`.

See [Testing](../../TESTING.md) for the complete gate matrix and
[`scripts/README.md`](../../scripts/README.md) for inputs and cleanup behavior.
