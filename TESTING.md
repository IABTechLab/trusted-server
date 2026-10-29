# Testing auction orchestration

## Secret-binding release evidence

App config must contain no removed credential properties. Provision fictional
generated bindings from the same compiler/renderer as runtime; direct resolved
`Settings` unit fixtures are internal, not public TOML examples. See
[provisioning](docs/guide/secrets.md), [CI](docs/guide/secrets-ci.md), and
[migration](docs/guide/secret-binding-migration.md).

### Issue regression coverage

The config-only attacker model assumes the attacker cannot write native secrets,
change deployment bindings, or deploy code. Provisioning a generated binding is
an authorization decision made outside the mutable config blob. A checksum alone
never grants that authority.

Run the production-envelope-to-pull regressions and actual Axum HTTPS transport
checks separately:

```bash
cargo test -p trusted-server-core --lib secret_scoping_
cargo test -p trusted-server-adapter-axum --lib platform::https_tests
```

The pull regression has a successful dispatch control, then recomputes valid
checksums after changing destinations or injecting another role's generated key.
It asserts no original pull credential read, backend creation, or HTTP send.
Rotation tests distinguish retained snapshots from reconstructed state; retiring
a binding blocks replay on fresh state, not use already captured by old state.
The Axum tests use the production client policy with a test-only CA and loopback
DNS. They exercise synchronous and pending sends, trusted HTTPS, untrusted issuer,
wrong hostname, and all five redirect statuses to same-origin and other-origin
sinks. No production TLS bypass or trust configuration is added.

Run native WASM checks against isolated local TLS fixtures:

```bash
./scripts/secret-bindings-runtime.sh fastly
./scripts/secret-bindings-runtime.sh cloudflare
./scripts/secret-bindings-runtime.sh spin
```

The harness compiles fictional native bindings through the production compiler
and loads valid-checksum envelopes through the actual application. Fastly tests
its shipping S3 routes. Cloudflare and Spin do not currently dispatch S3 asset
routes; their `test-utils` builds use a test-only integration to exercise the
same core exporter with the real native stores and HTTP clients. This proves
the native core/adapter contract, not shipping S3 routing on those adapters.
Cases cover GET/HEAD, redirect sinks, bad certificates, and config-only mutations
without provisioning the mutated contexts. Runtime tools must match the repository pins.
See [script prerequisites and coverage](scripts/README.md#secret-binding-runtime-checks).
These local checks do not certify deployed provider permissions or propagation.

### Deployment release checks

Track provider and rollout evidence in [issue #1479](https://github.com/IABTechLab/trusted-server/issues/1479), separately from the config-only attacker fix in #1037.

Host tests, native-name rendering, metadata presence, local runtime tests, and
provider operations are different evidence. Before production rollout, verify
isolated Fastly create/replace/read-through, provider propagation and limits,
config-publisher permissions, protected provisioning CI, and coordinated
migration/retirement/rollback. These deployment checks do not require production
application credentials; use fictional values in isolated resources. Missing
evidence remains a release risk, not a passing result or proof of a code defect.

Rehearse all four native walkthroughs with fictional values in isolated resources.
Test config/plan/overlay/compiler/manifest/service/store drift before input and
with zero mutations, repeated-role selection, unsupported adapter writes,
cancellation, invalid bytes/newlines, provider failures and uncertain outcomes.
Inspect argv/stdin/output without printing sentinel credentials. Test same-context
replacement, fresh versus retained states, S3 maintenance, and old-context replay
after deletion. Do not claim Tinybird response observation or fleet-wide refresh.

## Start the local server

Configure at least one reachable provider in `trusted-server.toml`, then start the
Fastly development server:

```bash
fastly compute serve
```

Provider endpoints must use HTTPS. Fastly and Viceroy also need a backend that
matches the provider host and TLS settings. For a deterministic local bidder,
use `scripts/template-cache-local-test.sh`, which creates a temporary CA and
registers the matching backend.

## Example configuration

```toml
[auction]
enabled = true
timeout_ms = 2000
mediator = "adserver_mock"

[auction.providers.pbs-main]
protocol = "openrtb-2.6"
profile = "prebid-server"
endpoint = "https://prebid.example.com/openrtb2/auction"
routing = "explicit"

[auction.providers.aps-main]
protocol = "openrtb-2.6"
profile = "aps"
endpoint = "https://aps.example.com/e/pb/bid"
routing = "all_eligible"
profile_config = { account_id = "example-aps-account", debug = false }

[auction.bidders.example-server]
provider = "pbs-main"

[integrations.adserver_mock]
enabled = true
endpoint = "https://mediator.example.com/mediate"
timeout_ms = 500
```

Replace the example endpoints and profile values before running the server.
Omit `mediator` to test local highest-bid selection without mediation.

## Send a routed request

The PBS provider uses explicit routing, so the request must include params for a
bidder listed in `[auction.bidders]`:

```bash
curl -X POST http://localhost:7676/auction \
  -H "Content-Type: application/json" \
  -d '{
    "adUnits": [
      {
        "code": "header-banner",
        "mediaTypes": {
          "banner": {
            "sizes": [[728, 90], [970, 250]]
          }
        },
        "bids": [
          {
            "bidder": "example-server",
            "params": {
              "placement": "example-header-placement"
            }
          }
        ]
      },
      {
        "code": "sidebar",
        "mediaTypes": {
          "banner": {
            "sizes": [[300, 250], [300, 600]]
          }
        }
      }
    ]
  }'
```

The first impression routes to `pbs-main` and `aps-main`. The second routes only
to `aps-main` because APS uses `all_eligible` and PBS uses `explicit`.

## Check current logs

Startup logs report plan-backed construction and the provider count:

```text
Building plan-backed auction orchestrator
Auction orchestrator built with 2 bidder providers
```

A launched request logs the configured provider ID, predicted backend, and
budget. Collection logs the pending and immediate response counts:

```text
Dispatching bid request to 'pbs-main' (backend: ..., budget: ...ms)
Dispatching bid request to 'aps-main' (backend: ..., budget: ...ms)
Dispatched 2 SSP request(s) with 0 immediate response(s) (timeout: ...ms)
```

Exact backend names and budgets depend on the adapter and remaining auction
deadline. Provider failures are isolated and appear in response metadata under
the configured provider ID.

## Disabled auction

Set:

```toml
[auction]
enabled = false
```

`POST /auction` returns an immediate no-bid response, emits an
`auction_disabled` skipped telemetry event, and performs no provider or mediator
work. The request log is:

```text
/auction: auction is disabled; returning no-bid response
```

## Automated checks

Use the repository aliases instead of bare `cargo test --workspace`:

```bash
cargo test-fastly
cargo test-axum
cargo test-cloudflare
cargo test-spin
```

For browser integration tests:

```bash
cd crates/trusted-server-js/lib
npx vitest run
```

The template-cache harness exercises plan compilation, HTTPS backend naming,
provider dispatch, local highest-bid winner selection, and both ESI and inline
delivery modes:

```bash
./scripts/template-cache-local-test.sh esi
./scripts/template-cache-local-test.sh inline
```
