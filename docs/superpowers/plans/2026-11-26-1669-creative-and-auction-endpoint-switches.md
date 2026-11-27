# Creative and Auction Endpoint Switches Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The four first-party creative endpoints and `POST /auction` answer a local `404` while the switch that runs them is off. The Prebid shim stops calling `/auction` while the auction is off.

**Architecture:** A helper in `http_util.rs` builds the turned-off response. Each of the five shared core handlers checks its switch as its first statement and returns that response as `Ok`. Every adapter already calls these handlers, so only adapter tests change. The Prebid head injector adds `auctionEnabled` to the injected config, and the shim drops its `trustedServer` bid when the field is `false`.

**Tech Stack:** Rust 2024 (`trusted-server-core`, the Fastly, Axum, Cloudflare and Spin adapters, `trusted-server-integration-tests`), TypeScript with Vitest (`crates/trusted-server-js/lib`), VitePress docs.

**Spec:** [`docs/superpowers/specs/2026-11-24-1669-creative-and-auction-endpoint-switches-design.md`](../specs/2026-11-24-1669-creative-and-auction-endpoint-switches-design.md)

## Global Constraints

- **The turned-off response:**
  - status `404`;
  - body exactly `This endpoint is turned off`;
  - `Content-Type: text/plain; charset=utf-8`;
  - `Cache-Control: no-store, private`.

  Handlers return it as `Ok`, never `Err`. Apply the cache policy with `crate::response_privacy::enforce_terminal_private_cache_privacy`, which also inserts the `TerminalPrivateResponse` marker. Fastly's send boundary uses that marker to restore `no-store, private` after late request-filter header changes.

- **Logging:** exactly one `log::debug!` line per turned-off request, naming the endpoint path and the switch. Nothing at `warn` or `error`.
- **Which switch each endpoint reads:**

  | Endpoint                     | Reads                                                       |
  | ---------------------------- | ----------------------------------------------------------- |
  | `/first-party/proxy`         | `settings.auction.rewrite_creatives`                        |
  | `/first-party/sign`          | `settings.auction.rewrite_creatives`                        |
  | `/first-party/click`         | `settings.auction.rewrites_auction_clicks()`                |
  | `/first-party/proxy-rebuild` | `settings.auction.rewrites_auction_clicks()`                |
  | `POST /auction`              | `orchestrator.is_enabled()`, not `settings.auction.enabled` |

  For `/auction`: production sets the orchestrator's flag from `[auction].enabled` (`compile_auction_plan` calls `with_enabled`). Core tests build the orchestrator separately from `create_test_settings()`, whose `[auction].enabled` is `false`.

- **Check first:** the check is the handler's first statement. It runs before the handler's `Content-Length` check, before the handler reads or parses the body, and before signature validation, trace capture and telemetry.
  - EdgeZero ingress buffers request bodies before routing, and Fastly and Axum answer `500` when that conversion fails. That is out of scope.
- **Fastly request filters keep precedence:** integration filters such as DataDome run before route dispatch. A filter that answers first, with a challenge, `429` or redirect, keeps that answer. Do not move the check ahead of the filters.
- **Methods:** only primary methods change: `GET /first-party/proxy`, `GET` and `POST /first-party/sign`, `GET /first-party/click`, `GET` and `POST /first-party/proxy-rebuild`, and `POST /auction`. Every other method keeps going to the publisher fallback.
- **Scope limits:**
  - no new configuration keys;
  - no adapter dispatch code changes;
  - no changes to `tsjs.requestAds`, `/_ts/page-bids` or `/__ts/page-bids`.
- **Injected Prebid field:** `auctionEnabled`. In Rust it is `auction_enabled: bool`, declared as the last field of `InjectedPrebidClientConfig` so it serializes last. A config without it means the auction is on.
- **Repository rules:**
  - `expect("should ...")` messages and descriptive assertion messages;
  - no em dashes in comments or test titles;
  - example domains only;
  - no local imports;
  - sentence-case commit messages without prefixes or AI trailers.

## Review Focus

1. **A creative already on a page** sends a correctly signed URL to a turned-off endpoint. The turned-off `404` comes back: no fetch, no redirect, no `403`. Covered by `turned_off_creative_endpoints_ignore_valid_signatures` in Task 1.
2. **Malformed or oversized input reaches a turned-off endpoint:** invalid JSON to `/first-party/sign`, or invalid JSON or an over-limit `Content-Length` to `/auction`. The turned-off `404` comes back, not `400`, `413` or `502`. Covered in Tasks 1 and 2.
3. **A GPT refresh while the auction is off.** The synthetic refresh ad units reach Prebid with no `trustedServer` bid, and their client-side bids are kept. Covered in Task 4.
4. **An ad unit whose only bid is a `trustedServer` bid the publisher configured,** with the auction off. The unit reaches Prebid with an empty `bids` array and nothing throws. Covered in Task 4.
5. **A late `Cache-Control` change on Fastly.** An allowed DataDome verdict sets `Cache-Control` after normal middleware. The turned-off response still leaves with `no-store, private`. Covered by `late_filter_effects_cannot_make_a_turned_off_response_public` in Task 3. The route tests in Task 3 also check the body and header at each adapter's router boundary.

---

### Task 1: Turned-off response and creative endpoint gates

**Files:**

- Modify: `crates/trusted-server-core/src/http_util.rs`, next to `enforce_max_body_size`.
- Modify: `crates/trusted-server-core/src/proxy.rs`:
  - add the gates to `handle_first_party_proxy` (`:1587`), `handle_first_party_click` (`:1626`), `handle_first_party_proxy_sign` (`:1709`) and `handle_first_party_proxy_rebuild` (`:1899`);
  - delete the test `auction_rewrite_setting_does_not_change_proxied_html_or_css_rewriting` (`:3985`).
- Modify: `crates/trusted-server-core/src/creative.rs:1171-1173`, the `rewrite_proxied_html` doc comment.
- Modify: `crates/trusted-server-core/src/test_support.rs`, in the `tests` module.

**Interfaces:**

- Produces:
  - `pub const ENDPOINT_TURNED_OFF_BODY: &str = "This endpoint is turned off";` in `trusted_server_core::http_util`, with a doc comment. Adapter and parity tests import it.
  - `pub fn endpoint_turned_off_response(endpoint: &str, switch: &str) -> Response<EdgeBody>` in `http_util.rs`, with a doc comment. It writes the debug line and builds the response from Global Constraints, including the terminal-private marker.
    - It is `pub` so the Fastly send-boundary test in Task 3 can build one, as it already does with `publisher::page_bids_preflight_denied`.
  - `pub(crate) fn assert_endpoint_turned_off(response: Response<EdgeBody>, label: &str)` in `test_support::tests`. It puts `label` in every message and asserts:
    - the status, the exact body, `Content-Type` and `Cache-Control: no-store, private`;
    - that the `TerminalPrivateResponse` extension is present.

- [ ] **Step 1: Write the failing tests in the `proxy.rs` test module**

  Use the module's existing pattern: `futures::executor::block_on`, `create_test_settings()`, `build_http_request`, `build_http_post_json_request` and `noop_services()`.

```rust
#[test]
fn creative_endpoints_follow_their_switches() {
    // (rewrite_creatives, rewrite_clicks, proxy and sign answer, click and rebuild answer)
    let cases = [
        (false, None, false, false),
        (false, Some(true), false, true),
        (true, Some(false), true, false),
        (true, None, true, true),
    ];
    // For each case send all six requests below. None of them carries the
    // required input, so a handler past its gate returns Err:
    //   GET  https://edge.example/first-party/proxy
    //   GET  https://edge.example/first-party/sign
    //   POST https://edge.example/first-party/sign            body {}
    //   GET  https://edge.example/first-party/click
    //   GET  https://edge.example/first-party/proxy-rebuild
    //   POST https://edge.example/first-party/proxy-rebuild   body {}
    // Answering endpoint: assert!(result.is_err(), "{label}: should pass the gate")
    // Turned-off endpoint: assert_endpoint_turned_off(result.expect("should answer locally"), &label)
}

#[test]
fn turned_off_creative_endpoints_ignore_valid_signatures() {
    // rewrite_creatives = false, rewrite_clicks unset. Each request
    // below gets assert_endpoint_turned_off:
    //   /first-party/proxy?tsurl=...&tstoken=... signed with compute_encrypted_sha256_token
    //   signed_click_target(&settings) sent to /first-party/click
    //   /first-party/proxy-rebuild?tsclick=<url-encoded signed_click_target(&settings)>
    //   POST /first-party/sign with the raw body "not json"
}
```

- [ ] **Step 2: Run the tests and see them fail**

  Run: `cargo test -p trusted-server-core --target wasm32-wasip1 -- creative_endpoints_follow_their_switches turned_off_creative_endpoints_ignore_valid_signatures`

  Expected: the build fails because `assert_endpoint_turned_off` and `ENDPOINT_TURNED_OFF_BODY` do not exist.

- [ ] **Step 3: Implement the helper and the four gates**
  - Add the constant, the helper and the test helper from Interfaces. The helper builds the response, then calls `crate::response_privacy::enforce_terminal_private_cache_privacy` on it.
  - Each handler's first statement, before anything it reads from `req`:

    ```rust
    if !settings.auction.rewrite_creatives {
        return Ok(endpoint_turned_off_response("/first-party/proxy", "auction.rewrite_creatives"));
    }
    ```

    - `/first-party/sign` uses the same check and switch name, with its own path.
    - `/first-party/click` and `/first-party/proxy-rebuild` check `rewrites_auction_clicks()` and use the switch name `auction.rewrite_clicks`.

  - Delete `auction_rewrite_setting_does_not_change_proxied_html_or_css_rewriting`. The state it covers can no longer reach `finalize` through the handler.
  - Leave `proxied_html_click_wrapping_follows_explicit_rewrite_clicks` unchanged. It calls `finalize` directly, below the gate.
  - Rewrite the last paragraph of the `rewrite_proxied_html` doc comment to say this pass runs only while `/first-party/proxy` is on (`rewrite_creatives = true`). Click wrapping follows `rewrites_proxied_clicks()`.

- [ ] **Step 4: Run the tests and see them pass**

  Run: `cargo test -p trusted-server-core --target wasm32-wasip1 -- proxy:: creative::`

  Expected: PASS, including every existing `proxy::tests` handler test. Those use `create_test_settings()`, whose defaults keep all four endpoints on.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/http_util.rs crates/trusted-server-core/src/proxy.rs crates/trusted-server-core/src/creative.rs crates/trusted-server-core/src/test_support.rs
git commit -m "Turn off first-party creative endpoints when their switch is off"
```

### Task 2: Turn off `POST /auction`

**Files:**

- Modify: `crates/trusted-server-core/src/auction/endpoints.rs`:
  - `handle_auction` (`:122`);
  - remove the disabled branch at `:198-245`, the `if !orchestrator.is_enabled()` block;
  - tests at `:1126`, `:1300` and `:1423`.

**Interfaces:**

- Consumes: `endpoint_turned_off_response` from Task 1, and `assert_endpoint_turned_off`.

- [ ] **Step 1: Replace and retarget the tests**
  - **Replace `disabled_auction_endpoint_emits_skipped_telemetry_without_provider_work`** with `turned_off_auction_endpoint_answers_404_without_work`.
    - Keep its orchestrator (`enabled: false`, `PanicOnBidProvider` registered) and its `RecordingTelemetrySink`.
    - Enable `gpt_diagnostics` trace capture and freeze the cookie gate the same way the `:1423` test does.
    - Send the raw body `not json`.
    - Assert: `assert_endpoint_turned_off`; `telemetry_sink.batches` is empty ("should emit no telemetry"); `response.extensions().get::<crate::trace::TraceAuctionCarry>()` is `None` ("should capture no trace").
  - **Add `turned_off_auction_endpoint_answers_before_the_size_check`.** Same orchestrator, with a `Content-Length` header of `MAX_AUCTION_BODY_SIZE + 1`. Assert `assert_endpoint_turned_off`, not `413`.
  - **Rename `trace_auction_api_frozen_disabled_capture_delivers_private_evidence`** to `trace_auction_api_frozen_consent_denied_capture_delivers_private_evidence`.
    - Set the orchestrator `enabled: true` and use `make_ec_context(Jurisdiction::Unknown, None)`.
    - The consent gate at `:250` still produces `Skipped` / `policy_skipped`, so keep every assertion.
    - Change "disabled" to "consent-denied" in its `expect` and assertion messages.
  - **Retarget `trace_slot_conversion_api_echoes_only_unique_accepted_refs_when_frozen_active`** the same way: `enabled: true` and `Jurisdiction::Unknown`, all assertions unchanged.

- [ ] **Step 2: Run the tests and see them fail**

  Run: `cargo test -p trusted-server-core --target wasm32-wasip1 -- auction::endpoints::tests`

  Expected: both `turned_off_*` tests fail. Today the handler rejects the `not json` body with an `Err` and answers the oversized request with `413`. The two retargeted trace tests pass.

- [ ] **Step 3: Implement the gate**
  - The first statement of `handle_auction`:

    ```rust
    if !orchestrator.is_enabled() {
        return Ok(endpoint_turned_off_response("/auction", "auction.enabled"));
    }
    ```

  - Delete the old disabled branch.
  - Remove any import the deletion leaves unused. `TraceAuctionTerminalReason::PolicySkipped` and `AuctionTerminalOutcome::Skipped` are still used by the consent branch.

- [ ] **Step 4: Run the tests and see them pass**

  Run: `cargo test -p trusted-server-core --target wasm32-wasip1 -- auction::`

  Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/auction/endpoints.rs
git commit -m "Answer POST /auction with a local 404 when the auction is off"
```

### Task 3: Adapter route tests and parity

**Files:**

- Modify: `crates/trusted-server-adapter-fastly/src/app.rs`, in the `tests` module (`test_settings` at `:1512`, `test_router` at `:1558`, `route` at `:1508`).
- Modify: `crates/trusted-server-adapter-fastly/src/main.rs`, in the tests next to `late_filter_effects_cannot_make_a_page_bids_response_public` (`:1361`).
- Modify: `crates/trusted-server-adapter-axum/tests/routes.rs` (`test_settings` at `:23`).
- Modify: `crates/trusted-server-adapter-cloudflare/tests/routes.rs` (`test_router` at `:24`).
- Modify: `crates/trusted-server-adapter-spin/tests/routes.rs` (`test_router` at `:23`).
- Modify: `crates/trusted-server-integration-tests/tests/parity.rs` (`test_settings` at `:40`, routers at `:62-79`).

**Interfaces:**

- Consumes: `trusted_server_core::http_util::ENDPOINT_TURNED_OFF_BODY` from Task 1.

- [ ] **Step 1: Keep the existing `/auction` routing tests meaningful**

  These tests post `/auction` against settings with no `[auction]` table, so after Task 2 they get the turned-off `404`:
  - Axum and Cloudflare: `auction_endpoint_does_not_require_auth`;
  - Spin: `auction_is_routed`, `auction_endpoint_does_not_require_auth` and `auth_middleware_runs_in_chain_for_protected_routes`;
  - parity: `auction_not_challenged_by_auth_parity` and `spin_auction_ignores_spoofed_forwarded_headers`.

  Add this to the shared settings TOML those tests use: Axum `test_settings`, Cloudflare and Spin `test_router`, and parity `test_settings`.

  ```toml
  [auction]
  enabled = true
  ```

  - An enabled auction with no providers returns a `200` no-bid answer: the orchestrator's empty `planned_providers` path.
  - Fastly's `test_settings` already enables the auction.

- [ ] **Step 2: Write the turned-off route test in each adapter**
  - **Settings:** each test builds its router from a copy of that file's shared settings TOML, with the `[auction]` table set to:

    ```toml
    [auction]
    enabled = false
    rewrite_creatives = false
    ```

    - Axum, Cloudflare, Spin and parity: the minimal shared TOML (handlers, publisher, ec) plus this table.
    - Fastly: a copy of `test_settings()` with `enabled = false` and `rewrite_creatives = false` in its existing `[auction]` table. Keep its providers and Prebid integration so `build_state_from_settings` accepts it.

  - **Routers:**
    - Fastly: `build_state_from_settings` and `TrustedServerApp::routes_for_state`;
    - the others: `TrustedServerApp::routes_with_settings`.
  - **Name:** `turned_off_endpoints_answer_404_locally` in every adapter.

```rust
const TURNED_OFF_REQUESTS: &[(&str, &str)] = &[
    ("GET", "/first-party/proxy"),
    ("GET", "/first-party/sign"),
    ("POST", "/first-party/sign"),
    ("GET", "/first-party/click"),
    ("GET", "/first-party/proxy-rebuild"),
    ("POST", "/first-party/proxy-rebuild"),
    ("POST", "/auction"),
];
// For each request, labelled "{method} {path}":
//   status == 404
//   body == ENDPOINT_TURNED_OFF_BODY
//   cache-control == "no-store, private"
```

- **Fastly send boundary:** add `late_filter_effects_cannot_make_a_turned_off_response_public` to `main.rs`, modelled on `late_filter_effects_cannot_make_a_page_bids_response_public`.
  - Build the response with `trusted_server_core::http_util::endpoint_turned_off_response("/auction", "auction.enabled")`.
  - Apply `RequestFilterEffects` that set `cache-control: public, s-maxage=3600`, `surrogate-control: max-age=3600` and `cdn-cache-control: public, max-age=3600`, through `apply_terminal_response_effects`.
  - Assert: `cache-control` is `no-store, private`, `surrogate-control` and `cdn-cache-control` are absent, and the body is unchanged.

- **Parity:** add `turned_off_endpoints_parity`.
  - Add `turned_off_settings()` and turn `axum_router`, `cf_router` and `spin_router` into `*_router_with(settings: Settings)` wrappers. Keep the existing zero-argument names calling them with `test_settings()`.
  - The existing request helpers build their own router, so add `axum_send`, `cf_send` and `spin_send`. Each takes `(router: RouterService, method: &str, uri: &str, body: &str)` and returns `(u16, HeaderMap, bytes::Bytes)`, copying the request code of `axum_post`, `cf_post` and `spin_post`.
  - Send `TURNED_OFF_REQUESTS` to all three routers through those helpers.
  - Assert the three-part check above for each adapter, and equal status and body across adapters.

- [ ] **Step 3: Run the adapter suites and parity**

  Run: `cargo test-fastly && cargo test-axum && cargo test-cloudflare && cargo test-spin && cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity`

  Expected:
  - PASS.
  - If a test other than the ones listed in Step 1 depends on the auction being off, give that test its own settings instead of reverting Step 1.

- [ ] **Step 4: Commit**

```bash
git add crates/trusted-server-adapter-fastly/src/app.rs crates/trusted-server-adapter-fastly/src/main.rs crates/trusted-server-adapter-axum/tests/routes.rs crates/trusted-server-adapter-cloudflare/tests/routes.rs crates/trusted-server-adapter-spin/tests/routes.rs crates/trusted-server-integration-tests/tests/parity.rs
git commit -m "Cover turned-off creative and auction endpoints on every adapter"
```

### Task 4: Prebid stops calling `/auction` while the auction is off

**Files:**

- Modify: `crates/trusted-server-core/src/integrations/prebid.rs`:
  - `InjectedPrebidClientConfig` at `:1573` and its `From` impl at `:1589`;
  - `head_inserts_for_plan` at `:1117`;
  - the test `prepared_browser_injection_uses_only_plan_routes_and_browser_timeout_debug` at `:5613`.
- Modify: `crates/trusted-server-js/lib/src/integrations/prebid/index.ts`:
  - `InjectedPrebidConfig` at `:186`;
  - the helpers near `injectedServerSideBidderCodes` at `:238`;
  - the `requestBids` shim loop at `:2545`.
- Test: `crates/trusted-server-js/lib/test/integrations/prebid/index.test.ts`, in the `requestBids shim` describe (`:2750`) and `prebid/installRefreshHandler` (`:3421`).

**Interfaces:**

- Produces:
  - The injected JSON field `auctionEnabled: boolean`, serialized last, `false` exactly when the compiled plan is disabled.
  - The TS helper `function injectedAuctionEnabled(config = getInjectedConfig()): boolean`, which returns `config?.auctionEnabled !== false`.

- [ ] **Step 1: Write the failing Rust assertions**

  In `prepared_browser_injection_uses_only_plan_routes_and_browser_timeout_debug`:
  - assert that `script` contains `"auctionEnabled":true` ("should mark an enabled auction");
  - assert that `disabled_inserts[0]` contains `"auctionEnabled":false` ("should mark a disabled auction").

- [ ] **Step 2: Write the failing Vitest tests**
  - Add these to `requestBids shim`. Each sets `testWindow.__tsjs_prebid`, calls `installPrebidNpm()` and then `pbjs.requestBids({ adUnits: [unit] })`:
    - **`adds no trustedServer bid when the auction is off`:**
      - config `{ auctionEnabled: false, serverSideBidders: [] }`;
      - unit bids `[{ bidder: 'exampleBidder', params: { placementId: 1 } }]`;
      - expect the bids to be unchanged and `mockRequestBids` to have been called.
    - **`drops a publisher-configured trustedServer bid when the auction is off`:**
      - unit bids `[{ bidder: 'trustedServer', params: {} }, { bidder: 'exampleBidder', params: {} }]`;
      - expect `[{ bidder: 'exampleBidder', params: {} }]`.
    - **`leaves an empty bid list when the only bid was trustedServer and the auction is off`:**
      - unit bids `[{ bidder: 'trustedServer', params: {} }]`;
      - expect `[]`, `mockRequestBids` called, and nothing thrown.
    - **`keeps adding the trustedServer bid when auctionEnabled is absent or true`:**
      - configs `{ serverSideBidders: ['appnexus'] }` and `{ auctionEnabled: true, serverSideBidders: ['appnexus'] }`;
      - unit bids `[{ bidder: 'appnexus', params: { placementId: 1 } }]`;
      - expect `bids.map((bid) => bid.bidder)` to equal `['trustedServer']`.
    - **`treats an empty adUnitCodes list as every global ad unit`.** Pinned Prebid's `filterAdUnits` selects every ad unit for `adUnitCodes: []`. The shim selects none today. Set `mockPbjs.adUnits` and call `pbjs.requestBids({ adUnitCodes: [] })`, then check two cases:
      - **Auction on:** config `{ serverSideBidders: ['appnexus'] }` and a global unit with `[{ bidder: 'appnexus', params: { placementId: 1 } }]`. Expect `['trustedServer']`.
      - **Auction off:** config `{ auctionEnabled: false, serverSideBidders: [] }` and a global unit with `[{ bidder: 'trustedServer', params: {} }, { bidder: 'exampleBidder', params: {} }]`. Expect `[{ bidder: 'exampleBidder', params: {} }]`.
    - **`drops the trustedServer bid from adUnitCodes-selected global units when the auction is off`:**
      - global units `div-selected` and `div-other`, each with `[{ bidder: 'trustedServer', params: {} }]`;
      - call `requestBids({ adUnitCodes: ['div-selected'] })`;
      - expect `div-selected` to end with `[]` and `div-other` to be untouched.
  - Add one test to `prebid/installRefreshHandler`, modelled on `includes every browser-owned bidder in refresh ad units` (`:3611`):
    - **`refresh auctions with the auction off request no trustedServer bid`:**
      - The describe's `beforeEach` points `mockPbjs.requestBids` straight at `mockRequestBids`, and the model test installs only `installRefreshHandler()`. Neither runs the shim.
      - So set `testWindow.__tsjs_prebid = { auctionEnabled: false, serverSideBidders: [], clientSideBidders: ['rubicon'] }`, then call `installPrebidNpm()`, then `installRefreshHandler(750)`, then `pubads.refresh()`.
      - Expect the refresh ad units passed to `mockRequestBids` to carry no `trustedServer` bid and to keep the `rubicon` bid.

- [ ] **Step 3: Run the tests and see them fail**

  Run: `cargo test -p trusted-server-core --target wasm32-wasip1 -- prepared_browser_injection_uses_only_plan_routes_and_browser_timeout_debug`, then `cd crates/trusted-server-js/lib && npx vitest run test/integrations/prebid/index.test.ts`

  Expected:
  - the Rust assertions fail;
  - every test that turns the auction off fails, because the bids still contain `trustedServer`;
  - both cases of the empty-`adUnitCodes` test fail, because the shim selects no units;
  - the absent-or-true test passes.

- [ ] **Step 4: Implement**
  - **Rust:**
    - add `auction_enabled: bool` as the last field of `InjectedPrebidClientConfig`;
    - set it to `true` in the `From` impl;
    - set `auction_enabled: plan.enabled()` in `head_inserts_for_plan`.
    - If an existing test pins the injected JSON up to its closing brace, add `"auctionEnabled":...` to that expected string. Change nothing else in it.
  - **TS:**
    - add `auctionEnabled?: boolean` to `InjectedPrebidConfig`, with the doc comment "False when `[auction].enabled` is off. Absent means on.";
    - add `injectedAuctionEnabled`.
    - In the `requestBids` wrapper, read it once before the ad-unit loop.
    - Make `requestedAdUnitCodes` `undefined` when `opts.adUnitCodes` is an empty array, so an empty list selects every global unit, as Prebid's `filterAdUnits` does. A non-empty list keeps today's string filter.
    - When it is `false`, do this inside the loop right after `unit.bids` is ensured to be an array: set `unit.bids` to the bids whose `bidder` is not `ADAPTER_CODE`, then `continue`. The publisher snapshot capture above that point stays as it is.
    - Refresh units pass through the same wrapper (`pbjs.requestBids` at `:2942`), so the refresh path needs no separate change.

- [ ] **Step 5: Run the tests and see them pass**

  Run: `cargo test -p trusted-server-core --target wasm32-wasip1 -- integrations::prebid`, then `cd crates/trusted-server-js/lib && npx vitest run && npm run format && node build-all.mjs`

  Expected: PASS, with formatting clean and the build succeeding.

- [ ] **Step 6: Commit**

```bash
git add crates/trusted-server-core/src/integrations/prebid.rs crates/trusted-server-js/lib/src/integrations/prebid/index.ts crates/trusted-server-js/lib/test/integrations/prebid/index.test.ts
git commit -m "Stop the Prebid shim calling /auction while the auction is off"
```

### Task 5: Docs and changelog

**Files:**

- Modify: `docs/guide/creative-processing.md`:
  - `:61` says proxied HTML and CSS "are rewritten independently of the auction setting";
  - `:152` says "`/first-party/sign` itself is not gated by `rewrite_creatives`".
- Modify: `docs/guide/api-reference.md`:
  - the Route availability table at `:33`, the Predicate column of the five rows;
  - the sections `POST /auction` (`:168`), `GET /first-party/proxy` (`:428`), `GET /first-party/click` (`:486`), `GET/POST /first-party/sign` (`:526`), `POST /first-party/proxy-rebuild` (`:586`) and `GET /first-party/proxy-rebuild` (`:635`).
- Modify: `docs/guide/configuration.md`, wherever it describes `rewrite_creatives`, `rewrite_clicks` or `[auction].enabled`.
- Modify: `docs/guide/auction-orchestration.md:682-684`, which says `/first-party/proxy` HTML and CSS are "rewritten independently of `rewrite_creatives`".
- Modify: `docs/guide/integrations/prebid.md`, near `:163`.
- Modify: `docs/guide/integrations/datadome.md:175`, "Auction traffic at `/auction` is protected by default".
- Modify: `TESTING.md:113-128`, the "Disabled auction" section.
- Modify: `CHANGELOG.md`, `## [Unreleased]` → `### Changed` and `### Fixed`.

- [ ] **Step 1: Edit the docs**
  - **Route availability Predicate values**, in the tester-cookie rows' format:
    - `settings.auction.rewrite_creatives` for `/first-party/proxy` and `/first-party/sign`;
    - `settings.auction.rewrites_auction_clicks()` for `/first-party/click` and `/first-party/proxy-rebuild`;
    - `settings.auction.enabled` for `/auction`.
  - **Each endpoint section:** add a line saying it returns `404` with `Cache-Control: no-store, private` while its switch is off.
  - **`POST /auction`:** replace the disabled no-bid description at `:182`. Consent denial keeps the no-bid form.
  - **`creative-processing.md` and `configuration.md`:**
    - proxied HTML and CSS are rewritten only while `/first-party/proxy` is on;
    - warn that creatives already on a page stop loading proxied assets, or stop redirecting clicks, once the matching switch is turned off.
  - **`auction-orchestration.md`:** proxy processing needs `rewrite_creatives = true`, and is still independent of `[auction].enabled`. An explicit `rewrite_clicks` still changes its link wrapping.
  - **`prebid.md`:**
    - while the auction is off, the shim sends no `trustedServer` bid and every other bidder runs as before;
    - `adUnitCodes: []` selects every global ad unit, as in Prebid.
  - **`datadome.md`:** while the auction is off, a protected `/auction` request still goes through protection first. A challenge or block answers before the turned-off `404`.
  - **`TESTING.md`:** `POST /auction` returns the turned-off `404` (status, body, `Cache-Control`). It reads no body and emits no telemetry, and the old log line is gone.
  - **`CHANGELOG.md`:**
    - add one `**Breaking:**` entry under Changed covering the five endpoints and their switches, the Prebid shim change, and the reversal of #1234's "routed and ungated" click endpoints;
    - add an entry under Fixed: the Prebid shim treats `adUnitCodes: []` as every global ad unit, matching Prebid.

- [ ] **Step 2: Check formatting**

  Run: `cd docs && npm run format`, then `docs/node_modules/.bin/prettier --config docs/.prettierrc --check "*.md"`

  Expected: "All matched files use Prettier code style!"

- [ ] **Step 3: Commit**

```bash
git add docs/guide TESTING.md CHANGELOG.md
git commit -m "Document turned-off creative and auction endpoints"
```

### Task 6: Full CI gate run

- [ ] **Step 1: Run every gate in `AGENTS.md` → CI Gates**

  Run, from the worktree root:

```bash
cargo fmt --all -- --check
cargo clippy-fastly && cargo clippy-axum && cargo clippy-cloudflare && cargo clippy-cloudflare-wasm && cargo clippy-spin-native && cargo clippy-spin-wasm && cargo clippy-cli && cargo clippy-codegen
cargo test-fastly && cargo test-fastly-reuse && cargo test-axum && cargo test-cloudflare && cargo test-spin
cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity
(cd crates/trusted-server-js/lib && npx vitest run && npm run format)
(cd docs && npm run format)
docs/node_modules/.bin/prettier --config docs/.prettierrc --check "*.md" ".claude/**/*.md" ".github/**/*.md" "crates/**/*.md" "scripts/**/*.md" "tinybird/**/*.md"
cargo test -p trusted-server-core --test template_build_digest && cargo clippy -p trusted-server-core --test template_build_digest -- -D warnings
scripts/lint-wasm-clock.sh
shellcheck scripts/docs-proposal/*.sh && node --test scripts/docs-proposal/hunks.test.mjs && scripts/docs-proposal/test.sh
node --test scripts/check-ci-node-pin.test.mjs && node scripts/check-ci-node-pin.mjs
node crates/trusted-server-adapter-fastly/tests/deadline-origin.mjs "$(command -v viceroy)"
cargo build --locked -p trusted-server-adapter-cloudflare --example deadline-runtime --target wasm32-unknown-unknown --features cloudflare && node scripts/test-worker-auction-deadlines.mjs cloudflare --runtime "$(command -v wrangler)"
cargo clippy --locked -p trusted-server-adapter-cloudflare --example deadline-runtime --target wasm32-unknown-unknown --features cloudflare -- -D warnings
cargo build --locked -p trusted-server-adapter-spin --example deadline-runtime --target wasm32-wasip1 --features spin && node scripts/test-worker-auction-deadlines.mjs spin --runtime "$(command -v spin)"
cargo clippy --locked -p trusted-server-adapter-spin --example deadline-runtime --target wasm32-wasip1 --features spin -- -D warnings
node --test scripts/release/phase1.test.mjs && shellcheck scripts/release/phase1-build.sh
CARGO_TARGET_DIR="$TMPDIR/ts-cli-target" cargo run --manifest-path crates/trusted-server-cli/Cargo.toml --target "$(rustc -vV | sed -n 's/^host: //p')" --bin ts -- dev lint domains --changed-vs origin/main
```

Expected:

- Every command exits `0`.
- These are all 16 gates in `AGENTS.md` → CI Gates. The deadline fixtures also get their own Clippy runs, as `worker-auction-deadlines.yml` does, because the WASM Clippy aliases do not cover the `deadline-runtime` examples.
- The domain lint reads committed lines only, so run it after the Task 5 commit.

CI's integration-tests workflow also runs the browser suite. It uses `trusted-server.integration.toml`, where the auction is off, and `trusted-server.trace.toml`, which sets `rewrite_creatives = false`.

- No spec there needs `/auction` or a creative endpoint to answer under those configs.
- `aps-renderer.spec.ts` serves its own page and mocks `/auction`. Its page has no injected Prebid config, so the shim treats the auction as on.

Watch that job on the PR rather than running it locally.

- [ ] **Step 2: Fix and re-run until clean, then commit any fixes**
