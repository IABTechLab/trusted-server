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

## Identity conformance

The reusable EC contract lives in core test support. Fastly and Axum execute
identical navigation and HTTP scenarios through their actual application
routers. Run from the repository root with pinned Rust, Node and Viceroy:

```bash
node --test scripts/test-identity-conformance.test.mjs
node scripts/test-identity-conformance.mjs fastly
node scripts/test-identity-conformance.mjs axum
cargo test -p trusted-server-core --lib identity_conformance
```

The launcher lists the adapter's exact test before executing it and rejects
zero-test success. Cloudflare and Spin fail explicitly until their runners are
available; their production startup rejects unsupported identity storage.

The navigation scenarios cover record-before-cookie creation, rejected writes,
missing/failed geography, denied/absent GDPR consent, subresources, returning
readers and creation without trusted JA4. Platform client and location facts,
record storage and the publisher origin are controlled fixtures; consent goes
through real parsing and the finalized body is consumed before response release.

The shared HTTP suite adds local identify/batch APIs with and without storage,
auction body EIDs with bidding enabled or disabled, EC POST/GET, failed
withdrawal reporting, SharedID restoration, mirror privacy, origin/payload
validation, and cookie scope. A capturing publisher observes fallback controls
and never receives partner requests. Core negative fixtures check that the
assertions reject early-cookie issuance and broken returning-record behavior.

The suite ends at response delivery. It does not qualify native enabled-provider
scheduling, after-response task drain, live vendor APIs, CMP client ordering or
Safari cookie lifetime; these remain broader feature/runtime qualification.
The production DynamoDB image fixture separately exercises a pending record
write during SIGTERM. Later adapter integration remains separate from this
Fastly/Axum application conformance.
