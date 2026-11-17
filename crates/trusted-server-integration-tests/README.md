# trusted-server-integration-tests

Native test package for cross-adapter parity, documentation compilation, and
end-to-end publisher behavior. It is not linked into an adapter artifact.

## Test surfaces

- `tests/parity.rs` calls Axum, Cloudflare, and Spin routers in process and
  compares their shared route behavior.
- `tests/documentation_snippets.rs` extracts the checked integration-guide
  fixture and compiles it in an isolated offline crate.
- `tests/integration.rs` exercises the Fastly/Viceroy and Axum paths against the
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

## Creative proxy policy qualification

The Fastly network fixture is separate from native route parity:

```bash
node crates/trusted-server-adapter-fastly/tests/proxy-policy-origin.mjs "$(command -v viceroy)"
```

Use pinned Node 24.20.0 and Viceroy 0.21.0. This fixture observes actual HTTP
origin URLs through the signed core handler and real Fastly transport. It checks
two-reader static URL equivalence, removal of carried EC on redirects, and
rejection of exhausted redirects. These HTTP destinations cannot match HTTPS
identity rules. Positive identity delivery and terminal adapter privacy are
covered by separate route fixtures; this command does not qualify them. Viceroy
does not implement HTTP readthrough caching. HTTPS delivery and real Fastly
cache reuse need separately authorized staging with a controlled cacheable
origin and origin counter; record the exact binary/config revision and results.
Native parity or target builds alone also do not establish real Cloudflare or
Spin runtime qualification.

See [Testing](../../TESTING.md) for the complete gate matrix and
[`scripts/README.md`](../../scripts/README.md) for inputs and cleanup behavior.
