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

Fastly and Axum support multi-provider fan-out. Cloudflare and Spin reject an
enabled plan with more than one provider at startup, so their negative tests
are part of the contract rather than skipped coverage.

See [Auction orchestration](/guide/auction-orchestration) for configuration and
response details.
