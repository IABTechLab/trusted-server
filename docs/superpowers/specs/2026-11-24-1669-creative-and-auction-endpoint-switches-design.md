# Turn off creative and auction endpoints when their switch is off

- Issue: [#1669](https://github.com/IABTechLab/trusted-server/issues/1669)
- Date: 2026-11-24
- Status: Proposed

## Problem

Every adapter registers four first-party creative endpoints and `POST /auction`, and each one answers whatever `[auction]` says. An operator who turns creative rewriting or auctions off still leaves these endpoints reachable, even though nothing on the page calls them any more.

Only creative rewriting puts first-party creative URLs on a page:

- auction creatives on `POST /auction` and inline server-side rendering (`creative::process_auction_creative`, `process_inline_auction_creative`);
- HTML and CSS fetched through `/first-party/proxy` (`CreativeHtmlProcessor`, `CreativeCssProcessor`);
- `/first-party/sign`, which hands the creative runtime's render guard a signed `/first-party/proxy` URL.

No other feature emits them. The public `rewrite_creative_html` and `rewrite_inline_creative_html` wrappers have no production callers.

## Goals

- Each endpoint answers only while the switch that emits its URLs, or that runs it, is on.
- A turned-off endpoint is answered by Trusted Server itself, the same way on Fastly, Axum, Cloudflare and Spin.
- The defaults keep today's behavior for the creative endpoints.

## Non-goals

- `/_ts/page-bids` and its `/__ts/page-bids` alias. With auctions off they keep returning their current "no slots" answer, which the SPA hook relies on to skip GPT slot creation.
- Non-primary methods on these paths (`HEAD`, `OPTIONS`, `PUT` and so on). They keep going to the publisher fallback.
- Client changes. The creative runtime is still injected when only click wrapping is on; a page that enables the render guard gets a `404` from `/first-party/sign` and loads the original URL.
- New configuration keys.

## Anti-goals

The change fails, even if each endpoint stops working, when:

- a turned-off request reaches the publisher origin;
- a turned-off request produces a `5xx` or an `error`-level log line;
- the adapters disagree on the response;
- a default configuration loses a creative endpoint.

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
- `Cache-Control: no-store`, so a cached `404` cannot outlive turning the switch back on. This is a design call, open to review.
- One `debug`-level log line naming the endpoint and the switch.

### Check before any work

Each handler checks its switch first: before reading the body, validating a signature, building an auction request, capturing a trace or emitting telemetry. For `POST /auction` this replaces today's disabled branch in `handle_auction` (`auction/endpoints.rs`), which returns an empty OpenRTB response, records a skipped trace and emits an `auction_disabled` telemetry event.

## Approaches considered

1. **Check at the top of each shared core handler (picked).** `handle_first_party_proxy`, `handle_first_party_proxy_sign`, `handle_first_party_click` and `handle_first_party_proxy_rebuild` in `proxy.rs`, and `handle_auction` in `auction/endpoints.rs`. All four adapters already call these handlers, so they behave the same with no adapter code changes.
2. **Check in each adapter's dispatch.** The same five checks would be written four times and drift apart over time.
3. **Skip route registration when a switch is off.** An unregistered path falls through to the publisher origin, which breaks the first anti-goal.

## Consequences

- **Creatives already on a page** keep their rewritten URLs and stop loading proxied assets or redirecting clicks once the matching switch is turned off.
- **`POST /auction` with auctions off** returns `404` instead of an empty bid list. The browser's `sendAuction` already treats a non-OK response as no bids: it logs a warning, renders nothing and records an API failure in its own trace. What goes away is the server-side `auction_disabled` telemetry event and the server-side trace evidence for disabled auctions.
- **No configuration schema change**, so there is no upgrade ordering. Rolling the binary back restores today's behavior.

## What changes besides the handlers

- **Docs that state the opposite today:**
  - `docs/guide/creative-processing.md` says proxied HTML and CSS "are rewritten independently of the auction setting", and that "`/first-party/sign` itself is not gated by `rewrite_creatives`".
  - The `rewrite_proxied_html` doc comment in `creative.rs` says proxied HTML is rewritten "regardless of `rewrite_creatives`".
  - `docs/guide/configuration.md` and `docs/guide/api-reference.md` wherever they describe these endpoints, the two creative switches or a disabled auction.
- **Tests that lock in today's behavior:**
  - `proxy.rs`: the test that proxies HTML with `rewrite_creatives = false`.
  - `auction/endpoints.rs`: `disabled_auction_endpoint_emits_skipped_telemetry_without_provider_work` and `trace_auction_api_frozen_disabled_capture_delivers_private_evidence` assert the disabled branch's `200`, telemetry and trace facts. `trace_slot_conversion_api_echoes_only_unique_accepted_refs_when_frozen_active` uses a disabled auction only as a harness for trace slot-reference echoing, so it moves to an enabled auction with no bidding provider.
  - Route and parity tests that use "not `404`" to mean "routed", among them `auction_not_challenged_by_auth_parity` in `crates/trusted-server-integration-tests/tests/parity.rs`. `/auction` is off under a default config, so these need a config with the auction on.
- **`CHANGELOG.md`:** a **Breaking:** entry under Changed covering the five endpoints and the reversal of #1234's routing.

## Acceptance criteria

- [ ] With `rewrite_creatives = false` and `rewrite_clicks` unset, all four creative endpoints return the turned-off `404` on their primary methods, on every adapter. Nothing reaches the publisher origin, and nothing is logged at `error` level.
- [ ] With `rewrite_creatives = false` and `rewrite_clicks = true`, `/first-party/click` and `/first-party/proxy-rebuild` work, and `/first-party/proxy` and `/first-party/sign` return the turned-off `404`.
- [ ] With `rewrite_creatives = true` and `rewrite_clicks = false`, `/first-party/proxy` and `/first-party/sign` work, and `/first-party/click` and `/first-party/proxy-rebuild` return the turned-off `404`.
- [ ] With `[auction].enabled = false`, `POST /auction` returns the turned-off `404` on every adapter without reading the body, capturing a trace or emitting telemetry, and the creative endpoints follow only their own switches.
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
