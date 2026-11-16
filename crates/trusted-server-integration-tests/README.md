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

## Identity conformance

The reusable EC contract lives in core test support, with the current Fastly
runner in `trusted-server-adapter-fastly/src/identity_conformance.rs`. Run from
the repository root with the repository-pinned Rust, Node and Viceroy:

```bash
node --test scripts/test-identity-conformance.test.mjs
node scripts/test-identity-conformance.mjs fastly
cargo test -p trusted-server-core --lib identity_conformance
```

The launcher lists the required Fastly test before executing it and rejects
zero-test success. Selecting `axum`, `cloudflare` or `spin` fails explicitly
until its runner is available. Native core tests check the assertion contract;
they do not claim Axum identity support.

The Fastly application test executes nine isolated scenarios: fresh creation,
rejected writes, missing and failed geo, denied and absent GDPR consent,
subresources, returning readers, and the current missing-browser-metadata gate.
It uses the real request conversion, router, shared production finalization
callback composition and consumed HTML response. Platform metadata, record
storage and the publisher origin are controlled test fixtures. Successful
issuance requires the matching record before response release; failed writes
must be observed before release. Returning scenarios apply cookie expiry rules
and replay the resulting identity. The missing-metadata expectation describes
the current pre-T12 policy and must change when #1592 permits absent JA4.

The suite ends when the finalized response is delivered. Post-delivery work
(queued telemetry and EC pull sync) does not run, and pull sync opens its own
store rather than the recording fixture, so release-ordering assertions cover
writes made during finalization only.

This is application conformance, not fresh-browser qualification through an
unmodified deployed artifact. Viceroy 0.21 does not expose JA4, so the current
production browser gate prevents fresh issuance over ordinary local HTTP.
The existing seeded Fastly runtime tests remain useful for their separate
boundary. Startup capability rejection, removal of adapter-owned identity
calls (#1600), Axum activation (#1574), later adapter integrations and deployed
browser/provider qualification remain outstanding under #1586. Infrastructure
delivery alone must not close that issue.
