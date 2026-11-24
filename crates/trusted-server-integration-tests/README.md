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

## Geo and consent conformance

Part of #1518. The shared location table is
`trusted-server-core/src/test_support/fixtures/geo-consent.json`; its Cartesian
product with eight consent signals, six entry points and new/returning readers
runs through the application. It covers DE, GB, US-CA, US-WY, country-only US,
JP, missing/invalid/partial geography, lowercase input, untrusted ingress,
provider failure and native provider selection being off. The current configured
country/state policy is preserved, including country-only US being non-regulated.

```bash
cargo test-axum --features geo-cloudfront geo_consent
node scripts/test-geo-consent.mjs /path/to/isolated/viceroy
```

The Fastly launcher requires Viceroy 0.21.0. It discovers the adapter test WASM
artifact from Cargo JSON, generates a temporary geolocation mapping from the
shared table, disables default loopback geo, runs the real Fastly lookup and
requires both exact test names to pass. These two runtime tests have an explicit
`ignore` reason because an ordinary Cargo invocation has no mapping; the launcher
runs them with `--ignored`, and the Fastly CI job runs the launcher. An unmapped
peer makes rejected forwarding inputs distinguishable from authenticated reader
IPs. Separate controls cover duplicate IP/auth headers, a malformed IP list,
missing authentication and authenticated IPv6 with a forged forwarding header.
Production falls back to the actual peer, which can have its own valid geography.

The Axum matrix uses managed native ingress with protected CloudFront selected,
the actual TS router/middleware/finalizer and request-derived runtime services.
The test-only initializer binds a recording store and capturing HTTP transport;
it never supplies jurisdiction, decoded consent or a fixed geo service. A supplied
provider returning failure exercises the managed error projection; untrusted and
unselected sources exercise their actual ingress outcomes. The separate
`startup::managed_geo_consumers::geo_consent_runtime_axum` integration test starts
the production Axum binary with disposable profiles and observes real HTTP bidder
requests. CloudFront admission relies on the declared protected peer network;
credential verification belongs to that protected ALB, not header-value matching
inside Trusted Server.

The assertions observe Set-Cookie, response geo headers and actual bidder JSON
(country, subdivision, GDPR, GPP sections and US Privacy), after consuming the
response body. New EC creation is limited to document/EC POST responses. Unknown
geo never creates an EC, even with TCF Purpose 1, while that consent permits the
auction. Returning ECs remain live with unknown geography. Withdrawal follows the
existing jurisdiction-specific policy: GDPR TCF denial and applicable US opt-outs
expire the cookie and tombstone its record; Unknown and NonRegulated retain it.
EC GET is read-only and does not finalize withdrawal. Record liveness is the one
internal observation: response cookie retention cannot prove whether a backend
row was silently tombstoned.

Axum normalizes valid lowercase country/subdivision input before consumers see
it. Fastly preserves the SDK's representation for valid values, as
`geo_from_fastly` does; both select the same jurisdiction for lowercase US/CA.
Unusable Fastly country codes are rejected at that conversion boundary, before
any consumer receives a location. Fastly still uses its own local lookup rather
than `ctx.geo()`.

To add an adapter, implement
`trusted_server_core::test_support::geo_consent::Adapter`: send the raw request
through real routing, ingress trust and finalization; consume the body; return
the finalized status/headers and bidder bodies actually dispatched; expose an
authoritative fixture-record liveness check. Support the table's producer states
without injecting expected policy. Set the producer's established casing mode,
and document genuinely unavailable states rather than silently skipping them.
Keep separate runtime acceptance for the shipping adapter/provider path.

Evidence categories remain separate: upstream EdgeZero contract tests do not
qualify this application; the shared matrix qualifies the TS request paths; the
Viceroy mapping and production Axum process qualify local runtime integration.
No live cloud deployment, vendor service or browser/CMP qualification is implied.

API/KV timeouts are N/A here: CloudFront resolves local request headers and Fastly
uses a synchronous local host lookup. Cloudflare and Spin adoption after EdgeZero
#480/#486 and R3, running this suite there, API/KV failure/timeout cases, operator
documentation and the decision on migrating Fastly to `ctx.geo()` remain #1518
follow-ups.
