# Auction testing

Use controlled upstream fixtures for auction tests. Do not point local smoke
tests at production bidder endpoints.

## Validate the configuration first

The CLI compiles the complete target-independent `AuctionPlan`, including
provider profiles, bidder routes, extension bounds, notification settings, and
mediator selection:

```bash
ts config validate --strict
```

For a local Fastly run, write the validated envelope into Viceroy state before
starting the adapter:

```bash
ts config push --adapter fastly --local
fastly compute serve
```

`fastly compute serve` alone is insufficient: the checked-in config store is
empty, and non-health requests fail until a valid app-config envelope is
available.

## Exercise `POST /auction`

Configure each provider endpoint to a controlled fixture, then send a
Prebid-compatible request:

```bash
curl --fail-with-body \
  --header 'content-type: application/json' \
  --request POST \
  --data '{"adUnits":[{"code":"header-banner","mediaTypes":{"banner":{"sizes":[[728,90]]}}}]}' \
  http://localhost:7676/auction
```

Check the response and logs for these invariants:

- a disabled auction returns a no-bid response without provider dispatch;
- `routing = "explicit"` sends a slot only to its mapped provider;
- `routing = "all_eligible"` sends eligible banner slots without leaking
  another provider's bidder parameters;
- provider-local failures do not fabricate bids;
- with no mediator, the orchestrator selects the highest valid bid per slot;
- with `mediator = "adserver_mock"`, the configured mediator owns final
  selection; and
- APS winners use `ext.trusted_server.renderer` rather than raw `adm`.

## Run automated coverage

The shared auction tests run through the target-specific aliases:

```bash
cargo test-fastly auction
cargo test-axum auction
cargo test-cloudflare auction
cargo test-spin auction
cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity auction
```

Every adapter runs a multi-provider auction as one EdgeZero request batch.
The Cloudflare and Spin startup tests load a multi-provider plan. The
integration tests `multi_provider_auction_runs_cloudflare` and
`multi_provider_auction_runs_spin` run its auction on each runtime. They are
ignored by default, so the commands above do not reach them.
Fastly's tests also cover its startup bound of 160 provider backends.

See [Auction orchestration](/guide/auction-orchestration) for configuration and
response details.

## Absolute deadline regressions

Core tests cover split dispatch/collection and synchronous execution, pending
provider attribution, preserved on-time bids, late collection after an origin
wait, and exhausted-budget mediation. The integration suite loads a navigation
on Fastly, Axum, Cloudflare, and Spin whose 500 ms auction races bidders at 180,
320, and 600 ms behind a 700 ms origin, and checks that the first two bids are
injected and the third is dropped. CI also runs EdgeZero's Cloudflare and
Spin HTTP clients as request batches under local workerd and Spin 4.1, covering
a fast bid, buffered bids after slow local work, delayed headers, and trickling
bodies. Run
`node scripts/test-worker-auction-deadlines.mjs --help` for the same local fixtures.
Fastly also has Viceroy network fixtures for stalled headers, trickling bodies, a
HEAD response's declared length, a fast bid beside a trickling one, and replies
that land before or after the cutoff. Its local origin harness builds the
adapter test binary itself:

```bash
node crates/trusted-server-adapter-fastly/tests/deadline-origin.mjs "$(command -v viceroy)"
```

The harness binds a local ephemeral port and runs the six network tests
excluded from the standard suite; CI runs the same command. To reuse a prebuilt
test artifact, pass its `.wasm` path as a second argument.
