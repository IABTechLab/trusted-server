# Turn off creative and auction endpoints when their switch is off

- Issue: [#1669](https://github.com/IABTechLab/trusted-server/issues/1669)
- Date: 2026-11-24
- Status: Proposed

## Problem

Every adapter registers four first-party creative endpoints and `POST /auction`, and each one answers whatever `[auction]` says. An operator who turns creative rewriting or auctions off still leaves these endpoints reachable, even though they no longer serve anything.

Only creative rewriting puts first-party creative URLs on a page:

- auction creatives on `POST /auction` and inline server-side rendering (`creative::process_auction_creative`, `process_inline_auction_creative`);
- HTML and CSS fetched through `/first-party/proxy` (`CreativeHtmlProcessor`, `CreativeCssProcessor`);
- `/first-party/sign`, which hands the creative runtime's render guard a signed `/first-party/proxy` URL.

No other feature emits them. The public `rewrite_creative_html` and `rewrite_inline_creative_html` wrappers have no production callers.

`POST /auction` has two browser callers: `tsjs.requestAds` (`core/request.ts`) and the Prebid shim's `trustedServer` bid adapter (`integrations/prebid/index.ts`). The shim adds a `trustedServer` bid to every ad unit whatever `[auction].enabled` says, and its `isBidRequestValid` always returns `true`. Turning the auction off only empties the injected `serverSideBidders` list. So with the auction off, every `pbjs.requestBids`, refreshes included, still sends `POST /auction`.

## Goals

- Each endpoint answers only while the switch that emits its URLs, or that runs it, is on.
- A turned-off endpoint is answered by Trusted Server itself, the same way on Fastly, Axum, Cloudflare and Spin.
- The defaults keep today's behavior for the creative endpoints.
- With the auction off, the Prebid shim stops sending `POST /auction`.

## Non-goals

- `/_ts/page-bids` and its `/__ts/page-bids` alias. With auctions off they keep returning their current "no slots" answer, which the SPA hook relies on to skip GPT slot creation.
- Non-primary methods on these paths (`HEAD`, `OPTIONS`, `PUT` and so on). They keep going to the publisher fallback.
- Client changes other than the Prebid shim. The creative runtime is still injected when only click wrapping is on; a page that enables the render guard gets a `404` from `/first-party/sign` and loads the original URL. `tsjs.requestAds` is unchanged: it already treats a non-OK answer as no bids and logs a warning.
- New configuration keys.

## Anti-goals

The change fails, even if each endpoint stops working, when:

- a turned-off request reaches the publisher origin;
- a turned-off request that reaches Trusted Server's router produces a `5xx` or an `error`-level log line;
- the adapters disagree on the response;
- a default configuration loses a creative endpoint;
- a page served with the auction off sends `POST /auction` from Prebid.

## Decisions

### Which switch controls which endpoint

| Endpoint                                 | Answers while                                                                                                          |
| ---------------------------------------- | ---------------------------------------------------------------------------------------------------------------------- |
| `GET /first-party/proxy`                 | `[auction].rewrite_creatives = true`                                                                                   |
| `GET`, `POST /first-party/sign`          | `[auction].rewrite_creatives = true`                                                                                   |
| `GET /first-party/click`                 | click wrapping is on: `AuctionConfig::rewrites_auction_clicks()` (`rewrite_clicks`, or `rewrite_creatives` when unset) |
| `GET`, `POST /first-party/proxy-rebuild` | same as `/first-party/click`                                                                                           |
| `POST /auction`                          | `[auction].enabled = true`                                                                                             |

The creative switches control the creative endpoints, and `[auction].enabled` controls only `POST /auction`. A deployment with auctions off and the default `rewrite_creatives = true` keeps all four creative endpoints on.

The click endpoints follow click wrapping rather than `rewrite_creatives`, because `rewrite_clicks = true` wraps links while asset rewriting is off. Gating them on `rewrite_creatives` would wrap links in that state and then answer every click with a `404`.

Proxied HTML wraps clicks unless `rewrite_clicks = false` (`rewrites_proxied_clicks()`). Proxied HTML only exists while `/first-party/proxy` is on, which requires `rewrite_creatives = true`, and in that state `rewrites_auction_clicks()` and `rewrites_proxied_clicks()` agree. One condition therefore covers both paths.

### This reverses #1234 for the click endpoints

The [#1234 design](2026-10-07-1234-creative-click-rewrite-switch-design.md) kept `/first-party/click` and `/first-party/proxy-rebuild` "routed and ungated", so that click URLs signed before a switch-off keep working. This design reverses that on purpose. After click wrapping is turned off, clicks on creatives already on a page land on a `404` instead of the advertiser. The same holds for proxied assets after `rewrite_creatives` is turned off. Operators accept that when they turn a switch off; the docs say so.

### The turned-off response

- Status `404` with a short plain-text body, returned as `Ok`, never `Err`. An `Err` becomes a `502` and an `error` log line on every adapter (see #1394).
- Not `403`. The creative runtime treats a `403` from `/first-party/sign` as "blocked" and drops the element; on a `404` it loads the original URL, which is the right result with rewriting off.
- `Cache-Control: no-store, private`, so a cached `404` cannot outlive turning the switch back on. This is a design call, open to review.
- The policy is applied through Trusted Server's terminal-private policy (`response_privacy::enforce_terminal_private_cache_privacy`), which also marks the response with `TerminalPrivateResponse`. Fastly applies late request-filter header changes after normal middleware (an allowed DataDome verdict can set `Cache-Control`), then restores `no-store, private` only on marked responses. Without the marker the `404` could leave Fastly cacheable.
- One `debug`-level log line naming the endpoint and the switch.

### Check before any work

Each handler checks its switch as its first statement. That is before it reads or parses the body, validates a signature, builds an auction request, captures a trace or emits telemetry. For `POST /auction` this replaces today's disabled branch in `handle_auction` (`auction/endpoints.rs`), which returns an empty OpenRTB response, records a skipped trace and emits an `auction_disabled` telemetry event.

The guarantee starts at the handler. At the pinned EdgeZero tag, adapter ingress buffers request bodies before routing on every route: Fastly, Cloudflare and Spin always, and Axum for JSON bodies. Fastly and Axum answer `500` when that conversion fails. This happens before a route is chosen, today and after this change. Moving it would be an EdgeZero change, so it is out of scope.

### Fastly request filters keep precedence

On Fastly, integration request filters run before route dispatch. DataDome protects `/auction` by default. After its outbound verdict call it can answer with a challenge: `403`, `429` or a redirect. That answer keeps precedence over the turned-off `404`, for two reasons:

- The filter decides for every protected path before any route is chosen.
- Moving the switch check ahead of the filters would need Fastly dispatch code.

So on a DataDome-protected Fastly service, a turned-off `/auction` can still cost one verdict call per request. Only pages cached before the switch-off still make that request. `/first-party/` is excluded from DataDome by default. This is a design call, open to review.

On Fastly, the adapter's `/auction` arm tags the request for EID syncing before it calls `handle_auction`, and the shared response finalization runs after the handler returns. A turned-off `/auction` can therefore still record the browser's `ts-eids` cookie EIDs to KV, as today's disabled branch does. Stopping that would need Fastly adapter code, so this design keeps it. The other adapters pass no KV graph. This is a design call, open to review.

### Prebid stops calling `/auction` when the auction is off

- The server adds an explicit auction-on flag to the injected Prebid client config (`InjectedPrebidClientConfig` in `integrations/prebid.rs`). It is set from the compiled auction plan, the same source as `serverSideBidders`.
- An empty `serverSideBidders` is not the signal. It is built from the plan's bidder routes, so on its own it does not say whether the auction runs.
- When the flag says off, the shim leaves no `trustedServer` bid in any ad unit: it adds none and drops one the publisher configured, so Prebid builds no request for it. The publisher's other bids run as they do today.
- A config without the flag means the auction is on, so a page without it behaves as today.
- The shim treats an empty `adUnitCodes` list as every global ad unit, as Prebid does (`filterAdUnits` in `prebid.ts`). Today it selects none. With the auction off, that would leave a publisher-configured `trustedServer` bid in place, and Prebid would still call `/auction`. The change also aligns the auction-on case with Prebid's selection.
- Pages cached before the switch was turned off still call `/auction` and get the `404` until they expire.

## Approaches considered

1. **Check at the top of each shared core handler (picked).** `handle_first_party_proxy`, `handle_first_party_proxy_sign`, `handle_first_party_click` and `handle_first_party_proxy_rebuild` in `proxy.rs`, and `handle_auction` in `auction/endpoints.rs`. All four adapters already call these handlers, so they behave the same with no adapter code changes.
2. **Check in each adapter's dispatch.** The same five checks would be written four times and drift apart over time.
3. **Skip route registration when a switch is off.** An unregistered path falls through to the publisher origin, which breaks the first anti-goal.

For the Prebid caller, the two other options were rejected:

- **Let Prebid keep calling.** Each `404` becomes a Prebid bidder failure: a "Server call for trustedServer failed" console error and a `bidderError` event on every bid request and refresh.
- **Keep the `200` no-bid answer for `/auction`.** That leaves the endpoint on while the auction is off.

## Consequences

- **Creatives already on a page** keep their rewritten URLs and stop loading proxied assets or redirecting clicks once the matching switch is turned off.
- **`POST /auction` with auctions off** returns `404` instead of an empty bid list. Pages served after the switch-off send nothing from Prebid. The callers that still reach it are pages cached before the switch-off, where Prebid reports a bidder failure for each call, and `tsjs.requestAds`, whose `sendAuction` treats a non-OK response as no bids: it logs a warning, renders nothing and records an API failure in its own trace. What goes away is the server-side `auction_disabled` telemetry event and the server-side trace evidence for disabled auctions.
- **No configuration schema change**, so there is no upgrade ordering. Rolling the binary back restores today's behavior.

## What changes besides the handlers

- **Docs that state the opposite today:**
  - `docs/guide/creative-processing.md` says proxied HTML and CSS "are rewritten independently of the auction setting", and that "`/first-party/sign` itself is not gated by `rewrite_creatives`".
  - `docs/guide/auction-orchestration.md` says HTML and CSS returned by `/first-party/proxy` are "rewritten independently of `rewrite_creatives`". It should say proxy processing needs `rewrite_creatives = true`, and is still independent of `[auction].enabled`.
  - `docs/guide/integrations/datadome.md`, which should say that a protected `/auction` still goes through protection first while the auction is off.
  - The `rewrite_proxied_html` doc comment in `creative.rs` says proxied HTML is rewritten "regardless of `rewrite_creatives`".
  - `docs/guide/configuration.md` and `docs/guide/api-reference.md` wherever they describe these endpoints, the two creative switches or a disabled auction.
  - `TESTING.md`'s "Disabled auction" section, which describes the no-bid response, the `auction_disabled` telemetry event and the log line.
  - `docs/guide/integrations/prebid.md`, which should say that the shim sends no `trustedServer` bid while the auction is off.
- **Prebid:** the injected flag in `integrations/prebid.rs` and the head-injection test that checks the auction kill switch (`prepared_browser_injection_uses_only_plan_routes_and_browser_timeout_debug`, which asserts `"serverSideBidders":[]` for a disabled plan), and the shim in `crates/trusted-server-js/lib/src/integrations/prebid/index.ts` with Vitest coverage.
- **Tests that lock in today's behavior:**
  - `proxy.rs`: the test that proxies HTML with `rewrite_creatives = false`.
  - `auction/endpoints.rs`: `disabled_auction_endpoint_emits_skipped_telemetry_without_provider_work` asserts the disabled branch's `200` and `auction_disabled` telemetry, so a turned-off test replaces it. `trace_auction_api_frozen_disabled_capture_delivers_private_evidence` and `trace_slot_conversion_api_echoes_only_unique_accepted_refs_when_frozen_active` use a disabled auction only to reach a skipped trace. They move to an enabled auction with consent denied, whose branch in `handle_auction` also finishes the trace as skipped with `policy_skipped`, so their assertions stay. An enabled auction with no provider would not keep them, because only the disabled and consent-denied branches finish a trace with `policy_skipped`.
  - Route and parity tests that use "not `404`" to mean "routed", among them `auction_not_challenged_by_auth_parity` in `crates/trusted-server-integration-tests/tests/parity.rs`. `/auction` is off under a default config, so these need a config with the auction on.
- **`CHANGELOG.md`:**
  - a **Breaking:** entry under Changed covering the five endpoints, the Prebid shim no longer calling `/auction` while the auction is off, and the reversal of #1234's routing;
  - a Fixed entry for `adUnitCodes: []` selecting every global ad unit.

## Acceptance criteria

- [ ] With `rewrite_creatives = false` and `rewrite_clicks` unset, all four creative endpoints return the turned-off `404` on their primary methods, on every adapter. Nothing reaches the publisher origin, and nothing is logged at `error` level.
- [ ] With `rewrite_creatives = false` and `rewrite_clicks = true`, `/first-party/click` and `/first-party/proxy-rebuild` work, and `/first-party/proxy` and `/first-party/sign` return the turned-off `404`.
- [ ] With `rewrite_creatives = true` and `rewrite_clicks = false`, `/first-party/proxy` and `/first-party/sign` work, and `/first-party/click` and `/first-party/proxy-rebuild` return the turned-off `404`.
- [ ] With `[auction].enabled = false`, `POST /auction` returns the turned-off `404` on every adapter, and the creative endpoints follow only their own switches.
  - The handler does not parse the body, capture a trace or emit telemetry. EdgeZero's ingress buffering before routing is outside this guarantee.
  - A Fastly request filter that answers first, such as a DataDome challenge, keeps precedence.
- [ ] On Fastly, a late request-filter `Cache-Control` change cannot make a turned-off response cacheable. It leaves with `Cache-Control: no-store, private`.
- [ ] With `[auction].enabled = false`, the injected Prebid config says the auction is off, and `pbjs.requestBids` sends no `POST /auction`.
  - It leaves no `trustedServer` bid in the ad units, including one the publisher configured, for explicit ad units, `adUnitCodes` selections and `adUnitCodes: []`.
  - The publisher's other bids still run.
  - With the auction on, or with no flag in the config, the shim behaves as today, except that `adUnitCodes: []` now selects every global ad unit.
  - Rust tests cover the injected flag, and Vitest covers both shim states.
- [ ] The defaults (`rewrite_creatives = true`, `rewrite_clicks` unset) keep all four creative endpoints working.
- [ ] Core tests cover each switch combination for each endpoint, and the route tests in the Fastly, Axum, Cloudflare and Spin adapters cover the turned-off case.
- [ ] The docs and `CHANGELOG.md` changes above land, including the warning that creatives already on a page stop working after a switch is turned off.

## Dependencies

- None blocking.
- Related: #1394 (a sign decline by rewrite policy returns `502`; independent of this change), and #982 (a signing broker for opaque-origin iframes, which calls `/first-party/sign` and inherits its switch).

## Left to the builder

- Names and placement of the shared check and the `404` response helper.
- The exact log text and response body.
- How the route tests tell the turned-off `404` from an unrouted path.
- The name of the auction-on flag in the injected Prebid config.
