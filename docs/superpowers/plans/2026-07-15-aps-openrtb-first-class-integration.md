# Amazon APS OpenRTB First-Class Integration Implementation Plan

> **For agentic workers:** Implement task-by-task with tests first. Do not use bare
> `cargo test --workspace`; use the adapter-aware aliases from `.cargo/config.toml`.
> Commit or push only when the supervising user explicitly requests it.

**Goal:** Replace the legacy APS `/e/dtb/bid` integration with APS OpenRTB,
participate with decoded prices without mediation, and render APS banner creatives in
the direct-auction and GPT/page-bids browser paths.

**Architecture:** Keep APS request/response policy in an APS-specific OpenRTB adapter.
Map the response into the shared `Bid` plus a typed, versioned APS renderer descriptor.
Carry that descriptor through `/auction` and the publisher bid map. TSJS builds a
Trusted Server-owned bootstrap for the fixed APS `prebid-creative.js` runner and
executes it in the approved sandbox. Ordinary `adm` remains on the existing mandatory
sanitize/rewrite path.

**Tech stack:** Rust 2024, generated OpenRTB types, serde/serde_json, base64 0.22,
error-stack, URL validation, TypeScript, Vitest, Prebid Universal Creative messaging.

**Spec:**
`docs/superpowers/specs/2026-07-15-aps-openrtb-first-class-integration-design.md`

**Branch:** `issue-764-aps-openrtb`

---

## Context and guardrails for every task

- APS may be implemented and tested before production edge support is confirmed.
  Broad production rollout remains blocked until the APS account team confirms the
  Fastly/edge contract.
- Send APS SDK identity `{ "source": "prebid", "version": "2.2.0" }`.
- Build the exact renderer allowlist: one bid with only `id`, `price`, `w`, `h`,
  `ext.creativeurl`, and `ext.tagtype`. Do not preserve unknown fields or silently
  expose the complete APS response.
- The outer renderer sandbox must omit `allow-same-origin`.
  `allow_script_creatives` is a server-side capability gate that defaults false;
  disabled script bids must be dropped before candidate reduction/winner selection.
  Enable it only under the staged opaque-origin and controlled-account test policy.
- APS user sync and APS `nurl`/`burl` delivery are out of scope. Do not add either
  browser path.
- Cut over directly. Keep `pub_id` as an alias only; do not add a legacy protocol
  switch.
- When a Trusted Server APS renderer is present, do not call
  `apstag.setDisplayBids()`.
- Banner only. Do not advertise APS video support in this issue.
- Do not weaken `sanitize_creative_html` or its tests.
- Raw APS requests/responses remain TRACE-only. Never log real account IDs, EIDs,
  consent strings, creative URLs, bid tokens, or response bodies at normal levels.
- All committed fixtures/docs use fictional `example` values. Controlled test-account
  values stay out of the repository and terminal transcripts intended for review.
- Use `error-stack`; no anyhow/eyre/thiserror in core code.
- Use `log::` macros, not `println!`; use descriptive `expect("should ...")`, not
  `unwrap()`.
- Run the focused test immediately after each production-code change.

---

## File map

### Rust core

- `crates/trusted-server-core/src/auction/types.rs`: bid identifiers and typed
  renderer contract.
- `crates/trusted-server-core/src/openrtb.rs`: narrowly scoped request/bid extension
  serialization types if they are shared outside APS.
- `crates/trusted-server-core/src/integrations/aps.rs`: configuration, OpenRTB request,
  response parser, exact renderer envelope, candidate reduction, static renderer route,
  diagnostics and provider dispatch.
- `crates/trusted-server-core/src/integrations/mod.rs`: register the APS proxy route as
  well as the separate auction provider.
- `crates/trusted-server-core/src/auction/formats.rs`: page derivation and `/auction`
  renderer extension.
- `crates/trusted-server-core/src/auction/orchestrator.rs`: decoded-price direct-winner
  coverage and stale-comment removal.
- `crates/trusted-server-core/src/integrations/adserver_mock.rs`: preserve renderer and
  IDs through mediation.
- `crates/trusted-server-core/src/publisher.rs`: normal bid-map renderer transport and
  APS `hb_adid`.

### Browser

- `crates/trusted-server-js/lib/src/core/types.ts`: APS renderer wire type.
- `crates/trusted-server-js/lib/src/core/auction.ts`: parse `/auction` renderer ext.
- `crates/trusted-server-js/lib/src/core/request.ts`: direct APS render dispatch.
- New `crates/trusted-server-js/lib/src/integrations/aps/render.ts`: exact envelope
  decoding/cross-checking, opaque renderer frame creation and postMessage dispatch.
- `crates/trusted-server-js/lib/src/integrations/gpt/index.ts`: exact APS Universal
  Creative bridge, APS beacon suppression and native-APS hook removal.
- New `crates/trusted-server-js/lib/test/fixtures/aps-renderer-v1.json`: shared Rust/TS
  golden wire fixture.
- New `crates/trusted-server-js/lib/test/integrations/aps/render.test.ts` plus existing
  core/GPT tests.
- `crates/trusted-server-integration-tests/browser/`: restrictive-CSP and opaque-origin
  Playwright coverage.

### Configuration and docs

- `trusted-server.example.toml`
- `docs/guide/integrations/aps.md`
- `CHANGELOG.md`

---

## Task 1: Add typed bid identifiers and renderer state

**What:** Give the shared auction model enough typed state to distinguish OpenRTB bid,
ad, and creative IDs and carry a validated APS renderer without using arbitrary
metadata.

**Files:**

- Modify: `crates/trusted-server-core/src/auction/types.rs`
- Modify: every production/test `Bid { ... }` construction found by
  `rg -n 'Bid \{' crates/trusted-server-core`
- Test: `crates/trusted-server-core/src/auction/types.rs`

- [ ] Add failing serde tests for a bid with:
  - separate `bid_id`, `ad_id`, and `creative_id`;
  - an APS renderer version, account ID, selected bid ID, tag type, creative URL,
    encoded response, and dimensions; and
  - `renderer = None` omission for ordinary bids.
- [ ] Define a strict `ApsTagType` enum with only `Iframe` and `Script`.
- [ ] Define a versioned, typed renderer enum/descriptor. The serialized renderer must
      use camelCase and a discriminator equivalent to:

  ```json
  {
    "type": "aps",
    "version": 1,
    "accountId": "example-account-id",
    "bidId": "fictional-bid-id",
    "creativeId": "fictional-creative-id",
    "tagType": "iframe",
    "creativeUrl": "https://creative.example/render",
    "aaxResponse": "base64-data",
    "width": 300,
    "height": 250
  }
  ```

- [ ] Add to `Bid`:
  - `bid_id: Option<String>`;
  - `creative_id: Option<String>`; and
  - `renderer: Option<...>`.
- [ ] Make descriptor `creativeId` optional and omit it when APS does not return
      `crid`; add a serde test for the absent-`crid` case.
- [ ] Correct stale comments saying APS prices are encoded or that APS has no creative
      delivery mechanism.
- [ ] Update every `Bid` literal. Use `None` outside production parser paths; do not use
      serde defaults to hide missed production mappings.
- [ ] Update Prebid's `parse_bid` to preserve OpenRTB `id` as `bid_id` and `crid` as
      `creative_id`, while retaining the existing strict `adid -> ad_id` semantics.
- [ ] Run the focused tests:

  ```bash
  cargo test-fastly auction::types
  cargo test-fastly integrations::prebid::tests::parse_bid
  ```

- [ ] Run `cargo fmt --all` and `cargo check-fastly` before proceeding.

**Expected result:** Shared bids can represent APS renderer state without overloading
`ad_id` or leaking renderer data through general metadata.

---

## Task 2: Cut APS configuration over to the OpenRTB contract

**What:** Replace the public APS configuration terminology and default endpoint before
replacing request/response behavior.

**Files:**

- Modify: `crates/trusted-server-core/src/integrations/aps.rs`
- Test: existing APS test module in the same file

- [ ] Add failing configuration tests proving:
  - `account_id = "example-account-id"` deserializes;
  - legacy `pub_id` deserializes to the same field;
  - string and integer compatibility is retained;
  - empty and whitespace-only strings fail validation;
  - surrounding whitespace is trimmed deterministically;
  - supplying both names fails as a duplicate field;
  - the default endpoint is `/e/pb/bid`;
  - HTTP, missing-host, and credential-bearing endpoint overrides fail validation; and
  - APS remains disabled by default; and
  - `allow_script_creatives` defaults false.
- [ ] Rename `ApsConfig.pub_id` to `account_id`, using `pub_id` only as a serde alias.
      Trim string values, reject empty-after-trim, normalize integers to strings, and
      rename the custom deserializer/error text accordingly.
- [ ] Add `allow_script_creatives: bool` with a serde default of `false`. Treat it as a
      server-side bid-eligibility capability, not a browser-only rendering preference.
- [ ] Change `default_endpoint()` to:

  ```text
  https://web.ads.aps.amazon-adsystem.com/e/pb/bid
  ```

- [ ] Replace generic URL-only endpoint validation with a custom check requiring HTTPS,
      a non-empty host, and no username/password. Do not add a plaintext test exception:
      the endpoint response is trusted to select executable renderer data.
- [ ] Remove account IDs from registration and request INFO logs. Log only that APS was
      registered and the endpoint/provider-safe state needed operationally.
- [ ] Change `supports_media_type` to banner only.
- [ ] Run:

  ```bash
  cargo test-fastly integrations::aps::tests
  ```

- [ ] Run `cargo fmt --all -- --check`.

**Expected result:** Existing configs continue through `pub_id`, new configs use
`account_id`, and no runtime protocol switch is introduced.

---

## Task 3: Replace the legacy request builder with APS OpenRTB

**What:** Delete the private `/e/dtb/bid` request types and build the request described
by the design spec from trusted auction context.

**Files:**

- Modify: `crates/trusted-server-core/src/integrations/aps.rs`
- Modify only if useful: `crates/trusted-server-core/src/openrtb.rs`
- Modify: `crates/trusted-server-core/src/auction/formats.rs`
- Test: APS and auction-format test modules

### 3.1 Request fixture tests

- [ ] Add a failing complete-request test with fictional values. Assert exact JSON for:
  - request `id`, `tmax`, and `cur = ["USD"]`;
  - `ext.account`;
  - `ext.sdk.source = "prebid"` and `version = "2.2.0"`;
  - one impression per eligible slot with direct `imp.id` mapping;
  - banner `format`, first `w`/`h`, `topframe = 0`, floor, floor currency, and
    unconditional `secure = 1`;
  - site page/domain/publisher fields;
  - consent-allowed user ID and EIDs;
  - UA/IP/language/DNT and coarse geo;
  - TCF/USP/GPP registrations with COPPA omitted because no trusted signal exists; and
  - absence of all PBS-only request and impression extensions.
- [ ] Add focused privacy/validation tests:
  - latitude/longitude are omitted while permitted coarse geo remains;
  - gated-out user/EID values stay absent;
  - no user data, keywords, gender, YOB, customdata, cookies, or arbitrary headers;
  - non-banner slots are omitted;
  - dimensions above `i32::MAX` are omitted safely;
  - empty/invalid formats cannot create a malformed impression;
  - configured and effective timeout use the orchestrator-granted budget.

### 3.2 `/auction` page derivation

- [ ] Add failing tests to `auction/formats.rs` proving:
  - a valid same-publisher HTTP(S) `Referer` becomes the `/auction` current page;
  - userinfo, wrong-host, non-HTTP(S), malformed, and oversized URLs are rejected;
  - rejection falls back to the configured publisher origin; and
  - no unrestricted browser body field is accepted as page context.
- [ ] Implement a small private URL-validation helper. Reuse it in APS referrer
      handling if practical, but do not make it a broad URL-policy refactor.

### 3.3 Implementation

- [ ] Remove `ApsBidRequest`, `ApsSlot`, legacy consent wrappers, slot-ID fallback, and
      `to_aps_request`.
- [ ] Add APS request extension types locally in `aps.rs` unless another module must
      serialize them. Implement `ToExt` consistently with existing OpenRTB extension
      types.
- [ ] Implement `build_openrtb_request(&AuctionRequest, &AuctionContext)` using generated
      OpenRTB types and `to_openrtb_i32`.
- [ ] Build registrations from the existing `ConsentContext` placements. Copy only the
      small field mapping needed by APS; do not expose/refactor the PBS builder in this
      task.
- [ ] Derive `site.ref` only from a valid HTTP(S) downstream `Referer` that differs from
      the normalized current page.
- [ ] Change `request_bids` to serialize/send the OpenRTB body with
      `Content-Type: application/json`, the existing bounded auction timeout, and no
      forwarded cookies.
- [ ] Keep serialized request logging at TRACE.
- [ ] Run:

  ```bash
  cargo test-fastly integrations::aps::tests
  cargo test-fastly auction::formats
  cargo check-fastly
  ```

- [ ] Review serialized request fixtures against the immutable upstream request code;
      explicitly confirm no `ext.prebid` or Trusted Server signing extension appears.

**Expected result:** Every eligible APS banner slot produces a standards-based OpenRTB
impression with explicit APS and privacy policy.

---

## Task 4: Parse APS OpenRTB and build the minimized renderer envelope

**What:** Replace `contextual.slots` parsing with strict `seatbid` parsing, decoded CPM,
one APS candidate per impression, and the exact renderer envelope.

**Files:**

- Modify: `crates/trusted-server-core/src/integrations/aps.rs`
- Test: APS test module

### 4.1 Add failing response tests

- [ ] A runner-compatible banner bid maps:
  - `id -> bid_id`;
  - optional `adid -> ad_id` and `crid -> creative_id`;
  - `impid -> slot_id`;
  - decoded price/USD, dimensions and domains;
  - `Bid.bidder = "aps"` regardless of upstream seat; and
  - `ext.creativeurl`/`ext.tagtype` into the typed renderer.
- [ ] Missing, string, numeric, and unexpected seat values never become `Bid.bidder` or
      `hb_bidder` and do not invalidate an otherwise valid bid.
- [ ] A valid runner-compatible bid without `crid` remains renderable with no
      `creativeId` in the descriptor.
- [ ] The immutable official adapter fixture (`ext.bidder` only) is an expected safe
      drop because the live runner requires `creativeurl`/`tagtype`.
- [ ] `iframe` and `script` parse into strict enum variants. With
      `allow_script_creatives = false`, every script bid is safe-dropped before
      per-impression reduction and winner selection; iframe bids remain eligible.
- [ ] With one higher-priced script bid and one lower-priced iframe bid for the same
      impression, the disabled script cannot win and the iframe candidate survives.
      With only disabled script bids, APS returns no bid.
- [ ] An APS bid with `adm` but no valid URL/tag type is dropped. When valid renderer
      metadata coexists with `adm`, all markup is excluded from the browser envelope
      and `Bid.creative`.
- [ ] HTTP 204, empty/missing `seatbid`, and seat bids with no bids are normal no-bids.
- [ ] Legacy `{ "contextual": ... }` is `Error`/`unexpected_response_shape`.
- [ ] Drop individual bids with unknown/missing `impid`, invalid price/currency/media,
      missing/zero/out-of-range/incompatible `w` or `h`, missing bid ID, invalid tag
      type, disabled script capability, or invalid creative URL.
- [ ] Creative URLs using HTTP, credentials, excessive length, malformed syntax, or the
      configured publisher origin are rejected.
- [ ] One structurally malformed bid does not discard another valid bid. Invalid JSON or
      non-representable numeric literals fail the complete response.
- [ ] APS `nurl`/`burl` are intentionally discarded, and top-level `ext.userSyncs`
      reaches neither `Bid`, diagnostics, nor renderer data.
- [ ] Diagnostics contain only fixed reason/count keys and never IDs, URLs, payloads,
      account values, seats, or raw extension data.

### 4.2 Exact envelope and collision tests

- [ ] Add `test/fixtures/aps-renderer-v1.json` containing exactly:
  - root `seatbid`;
  - one seat object containing only `bid`;
  - one bid containing only `id`, `price`, `w`, `h`, and `ext`; and
  - `ext` containing only `creativeurl` and `tagtype`.
- [ ] Assert Rust serialization equals that golden JSON semantically and that decoded
      `aaxResponse` has no top-level ID/currency, seat, impid, ad/crid/domain,
      notifications, markup, syncs, sibling bids, or unknown extensions.
- [ ] Remove unknown fields after transient parsing; do not copy an upstream extension
      map wholesale into the renderer envelope.
- [ ] Add a response over the 256 KiB pre-base64 cap and assert
      `render_payload_too_large`.
- [ ] Add two valid APS bids with the same impression/seat. Assert the parser returns
      only the highest price; for a tie, lexicographically lowest bid ID wins with its
      own matching renderer payload.

### 4.3 Implementation

- [ ] Remove all legacy request/response types, slot maps and tests.
- [ ] Special-case HTTP 204 before body collection/JSON parsing.
- [ ] Parse the response as `serde_json::Value`, validate response-level fields, then
      validate each bid independently so a wrong-typed bid cannot discard valid
      siblings.
- [ ] Set `Bid.bidder = "aps"`; discard upstream seat/network ID unless a bounded typed
      internal consumer is introduced. Set APS `nurl = None` and `burl = None`.
- [ ] Require present, positive, compatible `w` and `h`. Apply
      `allow_script_creatives` before adding a candidate to the per-impression group so
      a disabled script cannot reach floors, mediation, or winner selection.
- [ ] Validate creative URL against the configured publisher origin and build the exact
      allowlisted envelope from new values rather than cloning upstream objects.
- [ ] Group accepted candidates by `impid`, reduce deterministically to one, then build
      the final `AuctionResponse` and aggregate diagnostics.
- [ ] Preserve `collect_response_bounded(..., UPSTREAM_RTB_MAX_RESPONSE_BYTES, "aps")`,
      TRACE-only raw body logging, and safe aggregate normal logs.
- [ ] Run:

  ```bash
  cargo test-fastly integrations::aps::tests
  cargo fmt --all -- --check
  cargo clippy-fastly
  ```

**Expected result:** APS returns at most one priced, renderer-compatible candidate per
impression, with an exact browser envelope and stable `bidder = "aps"`.

---

## Task 5: Emit APS winners through `/auction` and prove direct selection

**What:** Use the new identifiers and renderer in the OpenRTB response without
manufacturing an empty creative, then replace the old mediation assumptions.

**Files:**

- Modify: `crates/trusted-server-core/src/auction/formats.rs`
- Modify: `crates/trusted-server-core/src/openrtb.rs` if a typed bid extension helper is
  useful
- Modify: `crates/trusted-server-core/src/auction/orchestrator.rs`
- Test: format and orchestrator test modules

### 5.1 Failing tests

- [ ] `/auction` maps `Bid.bid_id`, `ad_id`, and optional `creative_id` to OpenRTB
      `id`, `adid`, and optional `crid` instead of fabricating APS identifiers.
- [ ] An APS renderer winner has no `adm` key and has
      `ext.trusted_server.renderer` with the complete versioned descriptor—not a
      `{type,version}` abbreviation.
- [ ] A normal `Bid.creative` is still sanitized and rewritten before becoming `adm`.
- [ ] A winning bid with neither creative nor renderer fails explicitly; it does not
      serialize `adm: ""`.
- [ ] A renderer descriptor does not enter the HTML sanitizer.
- [ ] APS decoded price wins with no mediator when it is highest.
- [ ] APS loses to a higher clear-price bid.
- [ ] APS is removed below a slot floor.
- [ ] Optional mediator presence does not make decoded APS pricing special.

### 5.2 Implementation

- [ ] Add a typed bid-extension serializer for `trusted_server.renderer`.
- [ ] In `convert_to_openrtb_response`:
  - preserve real bid/ad/creative IDs;
  - process `creative` only through the existing sanitize/rewrite branch;
  - emit renderer ext only for a typed renderer; and
  - return an auction error for a winner with no render source.
- [ ] Remove stale “mediation should have provided creative” APS assumptions and encoded
      price comments.
- [ ] Replace old orchestrator APS tests with decoded-price/floor tests. Do not change the
      winner algorithm itself.
- [ ] Run:

  ```bash
  cargo test-fastly auction::formats
  cargo test-fastly auction::orchestrator
  cargo fmt --all -- --check
  ```

**Expected result:** Direct clients receive a priced APS winner with an explicit render
capability and never mistake an empty `adm` for a creative.

---

## Task 6: Preserve APS renderer state through mediation and publisher bid maps

**What:** Ensure both publisher paths expose the same APS descriptor and a mediator
cannot accidentally strip it.

**Files:**

- Modify: `crates/trusted-server-core/src/integrations/adserver_mock.rs`
- Modify: `crates/trusted-server-core/src/publisher.rs`
- Test: tests in both files

### 6.1 Mediation tests

- [ ] Add a regression test documenting the current collision: two unreduced APS bids
      with the same provider/slot/bidder overwrite the `(provider, slot, bidder)` index
      and can restore the wrong renderer.
- [ ] Feed the parser's reduced APS response into mediation and prove only the selected
      source bid reaches the index/request and its bid ID/renderer survive reconstruction.
- [ ] Keep the broader mediator index unchanged in this issue; APS avoids its existing
      ambiguity by returning one candidate per impression. Extend restoration only for
      `bid_id`, optional `creative_id`, and renderer while preserving existing non-APS
      notification/cache behavior.
- [ ] Run:

  ```bash
  cargo test-fastly integrations::adserver_mock
  ```

### 6.2 Bid-map tests

- [ ] Add failing `build_bid_map` tests proving:
  - APS renderer is present when `include_adm = false`;
  - renderer data is identical for initial-page and page-bids serialization;
  - `hb_bidder` is always `aps`, never upstream seat/network ID;
  - `hb_adid` uses APS selected `bid_id` ahead of OpenRTB `adid`;
  - PBS Cache UUID remains highest priority for PBS bids;
  - APS `nurl`/`burl` are absent while existing non-APS notifications are unchanged;
  - no general metadata is exposed in normal mode;
  - `aaxResponse` and account data survive `build_bids_script` escaping without
    breaking out of the script; and
  - a non-APS bid has no renderer property.
- [ ] Update `build_bid_map` to serialize typed renderer data normally while retaining
      `adm`/`debug_bid` only under the existing debug flag.
- [ ] Set APS `hb_adid` from the descriptor's selected bid ID. Preserve current cache and
      ad-ID fallbacks.
- [ ] Update `debug_bid` deliberately: IDs may be shown under existing debug behavior,
      but do not duplicate the potentially large encoded renderer envelope there.
- [ ] Run:

  ```bash
  cargo test-fastly publisher
  cargo fmt --all -- --check
  ```

**Expected result:** Direct, mediated, initial-navigation, and SPA auction paths retain
one typed APS render contract without exposing losing bids or arbitrary metadata.

---

## Task 7: Add the opaque APS renderer endpoint and direct-auction integration

**What:** Serve a static renderer document with its own CSP, validate the exact data the
vendor consumes, and keep all APS/bidder execution below an outer opaque-origin
sandbox.

**Files:**

- Modify: `crates/trusted-server-core/src/integrations/aps.rs`
- Modify: `crates/trusted-server-core/src/integrations/mod.rs`
- New: `crates/trusted-server-js/lib/src/integrations/aps/render.ts`
- New: `crates/trusted-server-js/lib/test/integrations/aps/render.test.ts`
- New: `crates/trusted-server-js/lib/test/fixtures/aps-renderer-v1.json`
- Modify: `crates/trusted-server-js/lib/src/core/types.ts`
- Modify: `crates/trusted-server-js/lib/src/core/auction.ts`
- Modify: `crates/trusted-server-js/lib/src/core/request.ts`
- Modify: `crates/trusted-server-js/lib/test/core/auction.test.ts`
- Modify/add: `crates/trusted-server-integration-tests/browser/`

### 7.1 Shared wire contract

- [ ] Add TypeScript discriminated types matching the Rust descriptor, with optional
      `creativeId`.
- [ ] Make the shared fictional JSON fixture the source of truth: Rust serialization
      equals it and TS tests import it.
- [ ] Add `/auction` parser tests for valid renderer ext, absent APS `adm`, optional
      creative ID, ordinary non-APS `adm`, unrelated ext, and malformed descriptors.
- [ ] Keep parsing structural; complete trust validation happens before DOM/message
      side effects.

### 7.2 Static renderer endpoint

- [ ] Add APS to `integrations::builders()` as a proxy registration using the same
      validated `ApsConfig`, `.with_proxy(...)`, and `.without_js()`.
- [ ] Register only `GET /integrations/aps/renderer` and return a static document—no
      account, bid, URL, or response data in HTML.
- [ ] Add Rust tests for exact route registration, method rejection, content type,
      `nosniff`, `Referrer-Policy: no-referrer`, and the explicit renderer CSP. The CSP
      must repeat the approved sandbox tokens without `allow-same-origin` so direct
      embedding cannot bypass the opaque boundary.
- [ ] Before loading the vendor runner, the static initializer reads and strictly
      validates the expected nonce from the iframe URL fragment, stores it, removes the
      fragment from visible history, and installs its one-message listener.
- [ ] The static script accepts one parent message, verifies `event.source`, version,
      exact structure, and equality with the independently fragment-bound nonce, then
      removes its listener, initializes the official account queue, dispatches
      `prebid/creative/render`, and dynamically loads the fixed runner. Reply with the
      accepted nonce only after runner load (or report runner failure). Because the
      sandbox origin is opaque, use `"*"` as the target origin in both directions but
      require the exact child/source window and one-time nonce.
- [ ] Load only the fixed Amazon runner URL; do not interpolate renderer data into
      script text, HTML, `srcdoc`, `document.write`, or another executable sink.
- [ ] Run:

  ```bash
  cargo test-fastly integrations::aps
  cargo test-fastly integrations::tests
  ```

### 7.3 Decode and cross-check consumed data

- [ ] `validateApsRenderer` must base64-decode UTF-8 JSON and require the exact allowlist:
      one seat, one bid, exact bid/ext keys, finite price, dimensions, URL, and tag type.
- [ ] Cross-check decoded ID/dimensions/URL/tag type against descriptor fields.
- [ ] Reject unknown keys, markup, notifications, syncs, invalid base64/UTF-8/JSON,
      non-HTTPS or credential-bearing URLs, and URLs matching `location.origin`.
- [ ] Validate before slot clearing, `stopImmediatePropagation`, iframe creation, or
      `postMessage`.
- [ ] Repeat message/envelope validation inside the static renderer document.

### 7.4 Opaque direct renderer

- [ ] `renderApsCreative` creates a sized iframe with `src` set to
      `/integrations/aps/renderer`; it never uses outer `srcdoc`.
- [ ] Apply exactly these tokens and assert `allow-same-origin` is absent:

  ```text
  allow-forms
  allow-pointer-lock
  allow-popups
  allow-popups-to-escape-sandbox
  allow-scripts
  allow-top-navigation-by-user-activation
  ```

- [ ] Generate at least 128 random bits per frame with `crypto.getRandomValues`, encode
      them as strict base64url, and set `iframe.src` to the trusted renderer URL with
      that nonce in the fragment before insertion/navigation. After load, post the
      validated descriptor with the same nonce. Clear existing content and reveal the
      frame only after an exact source- and nonce-bound runner-ready acknowledgement;
      remove an unacknowledged or failed hidden frame after a bounded timeout.
- [ ] Reject a missing/malformed fragment, nonce mismatch, repeat, or stale message; a
      nonce carried only in the posted message is not sufficient.
- [ ] Leave existing slot content intact on validation/load failure and log no payload,
      account, URL or bid ID.
- [ ] Dispatch valid APS renderer bids before ordinary non-APS `adm`; do not change the
      generic sanitizer/renderer.

### 7.5 Restrictive-CSP and origin proof

- [ ] Add Playwright coverage using a restrictive publisher CSP that permits only
      `frame-src 'self'` for the renderer route.
- [ ] Use a local fictional runner/creative implementation matching observed APS
      iframe/script behavior; CI must not depend on the live Amazon script.
- [ ] Prove iframe and fetched-HTML script creatives cannot read or modify
      `top.document`, even though the nested vendor frame requests
      `allow-same-origin`. Also embed the renderer without an iframe sandbox attribute
      and prove the response-level CSP sandbox still keeps it opaque.
- [ ] Prove the renderer route's CSP permits required HTTPS ad resources, the outer
      frame stays opaque, malformed/mismatched envelopes or fragment/message nonce
      mismatches do not clear slots, replay is rejected, and the runner can size/render
      without parent-origin `frameElement` access.
- [ ] Keep `allow_script_creatives = false` for normal traffic during local browser
      proof. If script rendering/resizing fails, keep the server gate false and stop for
      an APS-supported isolated renderer contract. Never add `allow-same-origin` to the
      outer frame.
- [ ] Run:

  ```bash
  cd crates/trusted-server-js/lib
  npx vitest run test/integrations/aps/render.test.ts test/core/auction.test.ts
  node build-all.mjs
  npm run format

  cd ../../../crates/trusted-server-integration-tests/browser
  npx playwright test
  ```

**Expected result:** Direct APS rendering uses a separately served document below an
opaque outer sandbox with a pre-bound, one-time nonce; script bids cannot win while the
default-off server capability is disabled.

---

## Task 8: Integrate APS with the GPT/Prebid Universal Creative bridge

**What:** Serve APS descriptors through the existing source-checked `Prebid Request`
message path and stop signaling the native APS SDK for TS winners.

**Files:**

- Modify: `crates/trusted-server-js/lib/src/integrations/gpt/index.ts`
- Modify: `crates/trusted-server-js/lib/test/integrations/gpt/ad_init.test.ts`
- Modify if needed: `crates/trusted-server-js/lib/test/integrations/gpt/index.test.ts`
- Reuse: `src/integrations/aps/render.ts`

### 8.1 Add failing bridge tests

- [ ] A matching APS `hb_adid` and renderer:
  - is accepted only from the owning slot's message source;
  - is fully decoded/cross-checked before `stopImmediatePropagation()`;
  - does not fetch PBS Cache or fire generic nurl/burl beacons;
  - posts one exact serializable `Prebid Response` using the deployed Universal
    Creative renderer version; and
  - creates only the opaque renderer-route iframe before posting the descriptor.
- [ ] Two concurrent requests for the same APS ad ID do not double-render.
- [ ] An APS descriptor requested from another slot is ignored.
- [ ] Invalid renderer data does not stop another legitimate handler and does not clear
      a slot.
- [ ] Existing debug-ADM and PBS Cache tests remain green.
- [ ] `adInit()` does not call `apstag.setDisplayBids()` for a bid with a Trusted Server
      APS renderer, even if `window.apstag` exists.
- [ ] A publisher-owned native APS SDK remains otherwise untouched; TS does not delete,
      reinitialize, or monkey-patch it.

### 8.2 Implement bridge support

- [ ] Import the shared envelope validator/opaque-frame helper without importing the
      full Prebid bundle.
- [ ] Add the APS branch after source-slot ownership and full renderer validation, before
      debug ADM/PBS Cache branches.
- [ ] Define and fixture-test the exact Universal Creative message object, including
      `rendererVersion`; do not describe or send a non-cloneable function.
- [ ] Compute an absolute `/integrations/aps/renderer` URL from the trusted publisher
      page origin before crossing into GAM; a relative URL in the creative frame would
      resolve against GAM. Validate that URL separately from APS data.
- [ ] Keep renderer source static and limited to creating the sandboxed iframe with that
      absolute renderer URL plus a fresh nonce fragment, then post validated data and
      the matching nonce after load and resolve only after the source- and nonce-bound
      renderer-ready acknowledgement.
- [ ] Preserve `renderingAdIds` and live `window.tsjs.bids`. Do not call
      `fireWinBillingBeacons` for APS; existing non-APS beacon deduplication is unchanged.
- [ ] Remove the `apstag?.setDisplayBids?.()` block that treats every APS targeting bid
      as a native SDK bid. Update comments accordingly.
- [ ] Run:

  ```bash
  cd crates/trusted-server-js/lib
  npx vitest run \
    test/integrations/aps/render.test.ts \
    test/integrations/gpt/ad_init.test.ts \
    test/integrations/gpt/index.test.ts \
    test/integrations/gpt/spa_hook.test.ts
  npm run format
  ```

**Expected result:** Initial navigation and SPA page-bids render the selected APS bid
through the same typed bootstrap, without relying on native `apstag` state.

---

## Task 9: Update operator configuration, migration docs, and changelog

**What:** Replace all legacy public guidance and document test/rollout dependencies.

**Files:**

- Modify: `trusted-server.example.toml`
- Rewrite: `docs/guide/integrations/aps.md`
- Modify: `CHANGELOG.md`
- Review references found by:

  ```bash
  rg -n 'e/dtb/bid|pub_id|amznbid|amznp|setDisplayBids|APS.*mediat' \
    --glob '!target/**' .
  ```

- [ ] Change the example to `account_id` and `/e/pb/bid`, with fictional values.
- [ ] Document `pub_id` as a compatibility alias and duplicate-name failure.
- [ ] Remove any claim that `slot_id`/`bidders.aps.slotID` is required for the OpenRTB
      provider.
- [ ] Document banner-only scope, USD comparison, decoded-price direct winners, and
      aggregate diagnostics.
- [ ] Document both rendering paths, static renderer endpoint, fixed APS runner URL,
      exact envelope allowlist, fragment-bound nonce, outer sandbox without
      `allow-same-origin`, renderer endpoint/publisher CSP requirements, and the
      default-off `allow_script_creatives` server gate.
- [ ] State that user sync and Trusted Server firing of APS `nurl`/`burl` are not
      implemented.
- [ ] State that public APS metadata says PBS unsupported and production edge traffic
      still requires account-team confirmation.
- [ ] Add a cohort rollout checklist:
  - TS APS enabled;
  - native APS demand disabled for that cohort;
  - GAM line item/Universal Creative prepared for `hb_bidder=aps` and selected
    `hb_adid`;
  - iframe rendering observed first while `allow_script_creatives = false`;
  - script enabled only for the isolated cohort after local browser proof, then observed
    in a real browser; and
  - no real IDs/tokens captured in docs or fixtures.
- [ ] Document rollback as disabling APS, restoring native APS for the cohort, or binary
      rollback—not a legacy config switch.
- [ ] Add a breaking/migration changelog entry for endpoint and canonical field changes.
- [ ] Run:

  ```bash
  cd docs
  npx prettier --write \
    superpowers/specs/2026-07-15-aps-openrtb-first-class-integration-design.md \
    superpowers/plans/2026-07-15-aps-openrtb-first-class-integration.md \
    guide/integrations/aps.md
  npm run format
  ```

**Expected result:** Operators cannot accidentally configure the old protocol or assume
user sync/video/native SDK behavior that no longer exists.

---

## Task 10: Controlled APS and browser verification

**What:** Validate the uncertain external contract without putting account-specific
material into source control.

**Prerequisites:** Controlled APS account/config supplied out of band; native APS demand
disabled for the test cohort; representative GAM line items and publisher CSP; local
restrictive-CSP/opaque-origin script proof already passing.

- [ ] Deploy with APS enabled only in the controlled cohort and
      `allow_script_creatives = false` initially.
- [ ] Prove with parser/orchestrator telemetry and tests that disabled script bids are
      removed before candidate reduction/winner selection and cannot leave a winning
      slot without a renderer.
- [ ] Confirm the outbound body has:
  - `/e/pb/bid` endpoint;
  - `ext.account` from operator config;
  - `ext.sdk = { source: "prebid", version: "2.2.0" }`;
  - expected impressions, floors, page/device/privacy fields; and
  - no latitude/longitude or disallowed identity fields.
- [ ] Confirm a bid returns a decoded price and can beat another provider with no
      mediator.
- [ ] Confirm the browser receives the exact one-bid allowlist in `aaxResponse`, with no
      seat, impid, IDs beyond selected bid ID, domains, notifications, markup, syncs,
      unknown extensions, sibling bids, or losing seats.
- [ ] Confirm the immutable official no-renderer fixture safe-drops and separately
      observe real `creativeurl`/`tagtype` responses.
- [ ] Exercise a real `tagtype=iframe` response through direct `/auction` and
      navigation/page-bids/GAM while the script gate remains false.
- [ ] Only after the local browser proof, set `allow_script_creatives = true` for this
      isolated cohort and exercise a real `tagtype=script` response through both paths.
      Do not enable it for other traffic yet.
- [ ] Verify the exact envelope works with the current APS runner. If it does not, record
      only missing field names/shape, stop rollout, consult APS, and update spec/tests;
      never preserve unknown fields or fall back to the full response.
- [ ] Verify the outer frame has an opaque origin, restrictive publisher CSP succeeds,
      click-through/dimensions work, and bidder content cannot access parent DOM.
- [ ] If the fixed runner cannot render/resize script creatives without outer
      `allow-same-origin`, restore `allow_script_creatives = false` and stop; do not
      weaken the boundary.
- [ ] Verify `apstag.setDisplayBids()` is not called for the TS winner and APS is not
      participating twice.
- [ ] Verify INFO/WARN logs and auction summaries contain no raw response or sensitive
      values.
- [ ] Obtain APS account-team confirmation before enabling broad production traffic.

**Expected result:** Iframe creatives render under the default policy; script creatives
become winner-eligible only for the staged cohort after both browser proof phases pass,
or the server gate returns to false and rollout stops with a bounded contract gap.

---

## Task 11: Final regression and repository verification

Run the complete required matrix from the repository root.

### Rust formatting, tests, and lint

- [ ] Run:

  ```bash
  cargo fmt --all -- --check
  cargo test-fastly
  cargo test-axum
  cargo test-cloudflare
  cargo test-spin
  cargo clippy-fastly
  cargo clippy-axum
  cargo clippy-cloudflare
  cargo clippy-cloudflare-wasm
  cargo clippy-spin-native
  cargo clippy-spin-wasm
  ```

### Cross-adapter parity

- [ ] Run:

  ```bash
  cargo test \
    --manifest-path crates/trusted-server-integration-tests/Cargo.toml \
    --test parity
  ```

### JS and docs

- [ ] Run:

  ```bash
  (cd crates/trusted-server-js/lib && node build-all.mjs)
  (cd crates/trusted-server-js/lib && npx vitest run)
  (cd crates/trusted-server-js/lib && npm run format)
  (cd crates/trusted-server-integration-tests/browser && npx playwright test)
  (cd docs && npm run format)
  ```

### Final review

- [ ] Run:

  ```bash
  git diff --check
  git status --short
  ```

- [ ] Review every `Bid` construction and mediator conversion for the new identifiers
      and renderer field.
- [ ] Review every `adm` assignment and prove it still passes through server
      sanitization.
- [ ] Review every renderer serialization boundary and prove only winning APS data is
      exposed.
- [ ] Review logs for account IDs, URLs, bid IDs, payloads, and consent/EID leakage.
- [ ] Verify no real controlled-account data entered source, tests, docs, comments, or
      snapshots.
- [ ] Compare the final behavior against all acceptance criteria in the design spec.

---

## Suggested implementation checkpoints

If commits are requested, keep reviewable checkpoints aligned with the tasks:

1. `Add typed APS renderer state to auction bids`
2. `Build APS OpenRTB requests`
3. `Parse APS OpenRTB responses`
4. `Carry APS renderer metadata to clients`
5. `Render APS creatives in TSJS and GPT`
6. `Document APS OpenRTB migration`

Do not combine unrelated refactors with these checkpoints.
