# Auction subsystem

The auction subsystem compiles configuration into an immutable `AuctionPlan`
and uses that plan for provider execution, bidder routing, mediation, and
response assembly. Production adapters do not build an independent provider
registry.

## Source map

| File | Responsibility |
| --- | --- |
| `plan.rs` | Compile `[auction]`, provider instances, bidder routes, notifications, and mediator selection into `AuctionPlan` |
| `profile.rs` | Own `PROFILE_REGISTRATIONS` and compile the `standard`, `prebid-server`, and `aps` profile schemas |
| `provider.rs` | Execute plan-backed OpenRTB providers through `RuntimeServices` |
| `routing.rs` | Select impressions and bidder parameters for each provider |
| `orchestrator.rs` | Launch supported providers and select or mediate valid bids |
| `formats.rs` | Convert the browser request and internal bids to response shapes |
| `openrtb.rs` | Build and validate provider OpenRTB payloads |
| `endpoints.rs` | Implement `handle_auction` for `POST /auction` |
| `telemetry.rs` | Emit bounded auction telemetry where the adapter supports it |
| `config.rs`, `context.rs`, `types.rs` | Shared configuration aliases, request context, and domain types |

Adapters register the public route with their own route tables. The
[API reference](../../../../docs/guide/api-reference.md) documents availability
without duplicating route definitions or source line numbers here.

## Configuration ownership

- `[auction.providers.<provider-id>]` is the complete provider-instance map.
- `[auction.bidders.<bidder-id>]` maps a browser-visible bidder code to exactly
  one provider instance.
- `PROFILE_REGISTRATIONS` is the complete provider-profile registry.
- `[auction].mediator` selects a separately registered mediator. The current
  mediator inventory contains `adserver_mock`.

The `standard` profile handles generic OpenRTB 2.6 endpoints. The
`prebid-server` and `aps` profiles add typed, provider-specific behavior. A
new standards-compatible endpoint normally needs configuration and tests, not
a new Rust provider type.

## Validation and runtime

Deploy validation compiles target-independent invariants. Adapter startup uses
the same plan and adds target capability checks. Fastly and Axum allow multiple
providers; Cloudflare and Spin currently require at most one enabled provider.

Provider timeouts are logical budgets used for launch decisions and OpenRTB
`tmax`. No adapter currently guarantees an abortable provider-wide wall-clock
deadline. Provider errors and malformed responses fail locally and cannot be
converted into fabricated bids.

`handle_auction` returns a no-bid response when auctions are disabled. With no
mediator, the orchestrator selects the highest valid bid per impression. When a
mediator is configured, that mediator owns final selection. APS renderable bids
use the typed renderer contract; ordinary OpenRTB bids carry validated creative
markup.

## Tests

Run the public [auction testing guide](../../../../docs/guide/auction-testing.md)
and the target-specific repository aliases in the root
[`TESTING.md`](../../../../TESTING.md). Unit tests live beside the modules;
cross-adapter behavior is checked by the integration-test parity suite.
