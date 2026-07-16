# Amazon APS OpenRTB First-Class Integration Design

**Date:** 2026-07-15  
**Status:** Approved for implementation and test; production rollout remains subject to APS account-team validation  
**Issue:** [#764 — Add the Amazon APS Prebid Adapter as first class integration](https://github.com/IABTechLab/trusted-server/issues/764)

---

## 1. Goal

Replace Trusted Server's legacy Amazon Publisher Services (APS/TAM) wire contract
with the OpenRTB contract used by the official APS Prebid.js adapter, so APS can:

1. receive a valid OpenRTB request for each eligible banner slot;
2. return a decoded CPM that competes directly without a mediator;
3. render winning `iframe` and `script` creatives through a deliberate APS browser
   renderer; and
4. preserve Trusted Server's existing privacy, sanitization, bounded-I/O, and
   observability invariants.

The new default endpoint is:

```text
https://web.ads.aps.amazon-adsystem.com/e/pb/bid
```

The implementation may proceed against fictional fixtures and a controlled APS test
account before APS confirms Fastly/edge-originated production support. Lack of that
confirmation is a rollout gate, not an implementation blocker.

---

## 2. Approved decisions

| Decision                  | Resolution                                                                                                                                                                                                                                                                               |
| ------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Edge/server support       | Implement and test now; do not claim production support until APS confirms it for the account.                                                                                                                                                                                           |
| APS SDK identity          | Send `ext.sdk.source = "prebid"`, which APS has approved as the starting identity. Match the current adapter version with `ext.sdk.version = "2.2.0"`.                                                                                                                                   |
| Creative response payload | Send an exact allowlisted envelope containing one bid with only `id`, `price`, `w`, `h`, `ext.creativeurl`, and `ext.tagtype`. Do not preserve unknown fields or silently fall back to the full response.                                                                                |
| Sandbox compatibility     | APS confirmed `allow-same-origin` compatibility, but Trusted Server must not grant it on the outer security boundary. The outer renderer iframe has an opaque origin; descendant APS frames cannot relax an ancestor sandbox restriction. `script` remains disabled until browser proof. |
| APS notifications         | Do not expose or fire APS `nurl`/`burl` in this issue. The fixed APS runner owns creative tracking until APS approves a separate notification contract.                                                                                                                                  |
| APS user sync             | Out of scope. Do not expose or execute `ext.userSyncs`.                                                                                                                                                                                                                                  |
| Migration                 | Cut over directly. Keep `pub_id` only as a configuration alias; do not retain a legacy/OpenRTB protocol switch.                                                                                                                                                                          |
| Native APS coexistence    | When a Trusted Server APS renderer descriptor is present, Trusted Server must not call `apstag.setDisplayBids()`. The test cohort must disable native APS demand to avoid duplicate participation.                                                                                       |

---

## 3. Primary upstream evidence

The design follows immutable official APS/Prebid sources:

- [APS endpoint and adapter version](https://github.com/prebid/Prebid.js/blob/cf8537da6b54223aea1bb29c6f939ba3b615a273/modules/apsBidAdapter.js#L16-L21)
- [OpenRTB request enrichment and privacy filtering](https://github.com/prebid/Prebid.js/blob/cf8537da6b54223aea1bb29c6f939ba3b615a273/modules/apsBidAdapter.js#L118-L187)
- [Request dispatch](https://github.com/prebid/Prebid.js/blob/cf8537da6b54223aea1bb29c6f939ba3b615a273/modules/apsBidAdapter.js#L247-L265)
- [Creative construction](https://github.com/prebid/Prebid.js/blob/cf8537da6b54223aea1bb29c6f939ba3b615a273/modules/apsBidAdapter.js#L279-L313)
- [Purpose-1-gated user sync behavior](https://github.com/prebid/Prebid.js/blob/cf8537da6b54223aea1bb29c6f939ba3b615a273/modules/apsBidAdapter.js#L322-L345)
- [Official APS adapter documentation and capability metadata](https://github.com/prebid/prebid.github.io/blob/adc4af1425c3433c81ee85560519cab0f7d17887/dev-docs/aps.md)

The capability metadata says `pbjs: true` and `pbs: false`. The public source proves
browser Prebid behavior; it does not by itself establish an approved production
server-to-server contract for Fastly or another edge runtime.

The immutable adapter test fixture is not a renderer contract: it accepts a banner bid
whose extension contains only `bidder`, then verifies that the adapter builds the
script wrapper. The unversioned live `prebid-creative.js` observed on 2026-07-15
(SHA-256 `ac99774f4d0f6b34aed7584952661007125345b7433970edd832e8451f9a6aef`)
requires `bid.id`, `price`, `w`, `h`, `ext.creativeurl`, and `ext.tagtype`. This
live-runner observation is intentionally treated as unverified behavior: the official
fixture must safe-drop in Trusted Server, while controlled-account responses provide
the separate renderer-compatible test path.

---

## 4. Current-state problems

`crates/trusted-server-core/src/integrations/aps.rs` currently:

- POSTs a private `pubId`/`slots`/`pageUrl` payload to `/e/dtb/bid`;
- requires per-slot `bidders.aps.slotID` or falls back to the incoming slot ID;
- parses `contextual.slots` rather than OpenRTB `seatbid`;
- stores encoded `amznbid`/`amznp` values in metadata;
- sets `Bid.price = None` and `Bid.creative = None`; and
- therefore cannot produce a direct no-mediator winner or a normal creative.

The generic winner selector intentionally ignores price-less bids. The publisher bid
map also starts from `bid.price`, so the current behavior is internally consistent but
cannot satisfy #764.

The existing Prebid provider has a mature OpenRTB builder, but that builder also owns
PBS-specific bidder params, stored requests, request signing, debug behavior,
`ext.prebid`, and consent-forwarding policy. Reusing it wholesale would couple APS to
PBS and increase regression risk.

---

## 5. Scope and non-goals

### 5.1 In scope

- APS banner OpenRTB request construction.
- APS OpenRTB response/no-bid parsing.
- Decoded-price direct winner selection.
- A typed APS renderer descriptor.
- APS creative rendering in both:
  - direct `POST /auction` TSJS rendering; and
  - publisher navigation/page-bids through the GPT/Prebid Universal Creative bridge.
- Safe aggregate diagnostics.
- Configuration and documentation migration.
- Controlled APS account/browser testing for `iframe` and `script` tag types.

### 5.2 Out of scope

- APS video/VAST support. Although the upstream adapter advertises video, this issue
  enables banner only until Trusted Server has a designed video delivery path.
- APS `ext.userSyncs` execution or exposure.
- A generic third-party renderer framework for arbitrary providers.
- Refactoring the entire Prebid OpenRTB builder.
- Retaining the legacy `/e/dtb/bid` request/parser behind a runtime switch.
- Server-side creative URL fetching or caching.
- Changes to the global HTML sanitizer's allowed elements or attributes.
- Production support claims before APS account-team confirmation.

---

## 6. Configuration contract and cutover

The canonical configuration becomes:

```toml
[integrations.aps]
enabled = false
account_id = "example-account-id"
endpoint = "https://web.ads.aps.amazon-adsystem.com/e/pb/bid"
timeout_ms = 800
allow_script_creatives = false
```

### 6.1 Compatibility

- Rename `pub_id` to `account_id` in the Rust type, example configuration, and docs.
- Accept `pub_id` as a serde alias for `account_id` for existing operator configs.
- Continue accepting a string or integer value during migration. Trim strings, reject
  empty-after-trim values, and normalize integers to strings.
- If both names are supplied, deserialization must fail as a duplicate field rather
  than silently selecting one.
- Keep `endpoint` as an operator-owned test/override setting, but require HTTPS with a
  non-empty host and no URL credentials. Its configured endpoint must implement this
  OpenRTB contract; it is not a legacy-protocol switch. There is no plaintext test
  exception because the APS response is trusted to select an executable renderer.
- APS remains disabled by default.
- `allow_script_creatives` is a separate server-side capability gate and defaults to
  `false`. It may be set to `true` only for an isolated test cohort after the
  restrictive-CSP/opaque-origin Playwright proof passes, and may remain enabled for
  production only after controlled-account `tagtype=script` validation.

### 6.2 Slot configuration

The OpenRTB impression ID is the Trusted Server slot ID. APS's official adapter has no
per-impression bidder parameters, so the provider no longer reads
`bidders.aps.slotID`. Existing generic creative-opportunity configuration may continue
to deserialize that value during migration, but APS documentation must mark it unused
and remove it from new examples.

---

## 7. APS OpenRTB request design

### 7.1 Builder boundary

Implement an APS-specific builder in `integrations/aps.rs` using the generated types
re-exported by `openrtb.rs`.

Do not make `PrebidAuctionProvider::to_openrtb` public. Initially duplicate the small
common mappings so APS policy remains explicit. A provider-neutral helper may be
extracted later only when tests prove byte-for-byte parity for the fields it owns.

### 7.2 Request shape

For each eligible slot, emit one `imp`:

- `imp.id = slot.id`;
- banner-only `banner.format` entries for valid dimensions;
- first valid format in `banner.w`/`banner.h` for APS compatibility;
- `bidfloor` and `bidfloorcur = "USD"` when a floor exists;
- `banner.topframe = 0`, matching the official framed-delivery fixture;
- `secure = 1` unconditionally because APS creative delivery is HTTPS-only; and
- no `imp.ext.prebid`, stored request, or bidder-parameter extension.

At request level emit:

- `id = AuctionRequest.id`;
- `tmax = context.timeout_ms`, the same effective budget enforced by the backend;
- `cur = ["USD"]`;
- `site.domain`, `site.page`, publisher data, and a valid referrer when known;
- consent-allowed `user.id` and already-gated EIDs;
- trusted device UA and client IP;
- coarse device geo with latitude and longitude explicitly omitted;
- valid `Accept-Language` and DNT signals from the downstream request;
- TCF, USP, GPP, and GPP SID signals in their existing Trusted Server OpenRTB
  placements. COPPA remains omitted because Trusted Server has no trusted COPPA signal
  source in this issue; and
- APS request extension:

```json
{
  "ext": {
    "account": "example-account-id",
    "sdk": {
      "source": "prebid",
      "version": "2.2.0"
    }
  }
}
```

### 7.3 Page and referrer provenance

- Publisher navigation and page-bids continue using the server-derived scheme, host,
  and matched request path.
- `POST /auction` should use a valid same-publisher `Referer` URL as the current page
  instead of always reducing the page to `https://<configured-domain>`; otherwise it
  falls back to the configured publisher origin.
- A candidate URL must use HTTP(S), have no userinfo, be length-bounded, and match the
  configured publisher host before it can become `site.page`.
- `site.ref` is emitted only when a valid HTTP(S) downstream `Referer` differs from the
  normalized current page. If the endpoint request's `Referer` is simply the current
  page, omit `site.ref` rather than mislabeling it as the document's prior referrer.
- Do not add an unrestricted client-provided URL or copy arbitrary headers.

### 7.4 Privacy rules

The provider consumes the privacy-filtered `AuctionRequest`; it does not independently
weaken endpoint decisions.

- Existing auction consent denial still returns before contacting APS.
- APS only receives `request.user.eids`, which endpoint code has already gated.
- Do not serialize `user.data`, `user.keywords`, `user.gender`, `yob`, `customdata`, or
  arbitrary context into APS.
- Remove precise `device.geo.lat` and `device.geo.lon` even if available.
- Do not forward browser cookies or arbitrary X-\* headers to APS.
- Request and response payloads may be logged only at TRACE.

---

## 8. Response validation and unified bid mapping

Parse the response envelope as `serde_json::Value`, validate response-level fields,
and deserialize or validate each `seatbid[].bid[]` independently. This preserves valid
siblings when one bid has a wrong field type while retaining extension maps needed for
APS creative metadata. Syntactically invalid JSON still fails the whole response.

### 8.1 Response-level validation

- HTTP `204 No Content` is a normal APS no-bid and returns before body collection or
  JSON parsing.
- Other non-success HTTP statuses produce provider `Error` without reflecting the body.
- Body collection remains capped by `UPSTREAM_RTB_MAX_RESPONSE_BYTES`.
- Invalid JSON, including a non-representable numeric literal such as an overflowing
  exponent under the repository's default `serde_json` configuration, or a legacy
  `contextual` envelope is an error with safe reason `unexpected_response_shape`.
- Empty/missing `seatbid`, or seat bids containing no bids, is a normal no-bid.
- Currency must be absent or case-insensitive USD. A non-USD response is dropped
  because Trusted Server has no currency conversion before winner comparison.
- Top-level `ext.userSyncs` is ignored and must not enter diagnostics or renderer data.

### 8.2 Bid-level validation

A bid is eligible only when:

- `impid` matches a slot in the request;
- `price` is finite and non-negative;
- `mtype` is absent or banner-compatible;
- `w` and `h` are both present, non-zero, safely convert to `u32`, and are compatible
  with the requested slot; and
- it has APS `ext.creativeurl` plus a usable bid ID and an eligible `ext.tagtype`:
  `iframe` is eligible, while `script` is eligible only when the server-side
  `allow_script_creatives` capability is enabled. This check happens before candidate
  reduction and winner selection, so a disabled or otherwise unrenderable script bid
  cannot win and blank the slot.

An APS `adm` without a valid renderer descriptor is not independently renderable in the
normal publisher flow and must therefore be dropped. If APS supplies `adm` alongside a
valid descriptor, remove all markup from the minimized envelope and do not copy it into
`Bid.creative`. If controlled testing proves markup is required, stop for a separate
security review rather than forwarding it unsanitized.

`creativeurl` must be an HTTPS URL without userinfo, must not match the configured
publisher origin, and must stay within a bounded serialized length. Validate the same
rules again in the browser. It comes only from the HTTPS APS endpoint response, never
from the client. Unknown tag types and recognized-but-disabled `script` bids are dropped rather than
passed through.

The pinned official adapter response fixture lacks `creativeurl` and `tagtype`; Trusted
Server must include that fixture as an expected safe-drop test. A separate fictional
fixture models the live runner's required extension fields. Controlled APS-account
validation of both observed tag types remains a rollout prerequisite.

### 8.3 Unified fields and APS candidate reduction

Extend `Bid` with generic identifiers and a typed optional renderer:

```text
bid_id       <- OpenRTB bid.id
ad_id        <- OpenRTB bid.adid
creative_id  <- optional OpenRTB bid.crid
renderer     <- typed APS renderer descriptor for every accepted APS bid
```

Set `Bid.bidder = "aps"` unconditionally. Upstream `seat` and `ext.bidder` are APS
network metadata, not the adapter identity; normalize them only into bounded internal
fields if an operational consumer is identified, otherwise discard them. Missing,
numeric, or unexpected seat values do not invalidate an otherwise valid bid and never
become `hb_bidder`.

Map decoded price, currency, `impid`, dimensions, and `adomain`. Do not map APS `nurl`
or `burl` in this issue; the vendor runner owns APS creative tracking until a separate
notification policy is approved.

Before returning `AuctionResponse`, collapse multiple valid APS candidates for the same
impression to one candidate: highest decoded price wins, with lexicographically lowest
bid ID as a deterministic tie-break. This produces the same APS result as direct
winner selection and prevents the current mediator's `(provider, slot, bidder)`
last-write-wins index from attaching another APS bid's renderer. The mediator still
restores `bid_id`, optional `creative_id`, and `renderer` for that single APS candidate.
A two-bids/same-slot/same-seat test is mandatory.

### 8.4 Diagnostics

`AuctionResponse.metadata` may contain only bounded aggregate fields, for example:

```json
{
  "seatbid_count": 1,
  "accepted_bid_count": 1,
  "dropped_bid_count": 0,
  "drop_reasons": {}
}
```

Allowed reason keys are fixed enums such as:

- `empty_seatbid`;
- `unknown_impid`;
- `invalid_price`;
- `unsupported_currency`;
- `unsupported_media_type`;
- `invalid_dimensions`;
- `missing_render_source`;
- `unsupported_tagtype`;
- `script_rendering_disabled`;
- `invalid_creative_url`;
- `render_payload_too_large`; and
- `unexpected_response_shape`.

Do not include account IDs, bid IDs, URLs, tokens, consent strings, raw extensions, or
raw bodies in INFO/WARN diagnostics or client-facing provider summaries.

---

## 9. Exact APS renderer response envelope

The official adapter gives `prebid-creative.js` the base64-encoded complete response
and selected bid ID. Trusted Server instead sends an exact allowlisted envelope based
on the fields required by the live runner observed on 2026-07-15:

```json
{
  "seatbid": [
    {
      "bid": [
        {
          "id": "fictional-selected-bid-id",
          "price": 1.23,
          "w": 300,
          "h": 250,
          "ext": {
            "creativeurl": "https://creative.example/render",
            "tagtype": "iframe"
          }
        }
      ]
    }
  ]
}
```

The allowlist is exact:

- root: `seatbid` only;
- selected seat object: `bid` only;
- selected bid: `id`, `price`, `w`, `h`, and `ext` only; and
- selected bid extension: `creativeurl` and `tagtype` only.

Do not include top-level ID/currency/extensions, upstream seat, `impid`, `adid`, `crid`,
`adomain`, notifications, markup, native data, user syncs, unknown extensions, sibling
bids, or losing seats. Unknown fields may exist transiently while the server validates
an upstream bid, but they are discarded before renderer construction.

Serialize the allowlisted object once, enforce a 256 KiB pre-base64 maximum, then
base64 encode with the standard alphabet. Store it in the typed renderer descriptor,
not general metadata. If the envelope exceeds the cap, drop the bid as non-renderable.
If controlled APS testing proves the exact envelope insufficient, stop and review the
specific missing field with APS; do not widen it or fall back to the full response
automatically.

A single fictional golden fixture under the TSJS test fixtures is the source of truth:
Rust serialization must equal it, and TypeScript parsing/validation/render-dispatch
tests must consume that same file.

---

## 10. Renderer descriptor and browser wire formats

### 10.1 Typed descriptor

The Rust and TypeScript contracts represent the same versioned data:

```json
{
  "type": "aps",
  "version": 1,
  "accountId": "example-account-id",
  "bidId": "fictional-selected-bid-id",
  "creativeId": "fictional-creative-id",
  "tagType": "iframe",
  "creativeUrl": "https://creative.example/render",
  "aaxResponse": "base64-data",
  "width": 300,
  "height": 250
}
```

Only `iframe` and `script` are valid tag-type values. `creativeId` is optional and is
omitted when APS does not return `crid`; the live runner does not consume it. Version
mismatch is fail-closed. The APS account identifier is not a secret, but it must still
be safely serialized and must never be hardcoded from a real account in repository
fixtures.

### 10.2 `POST /auction`

For APS renderer bids, the OpenRTB response bid contains the complete descriptor:

```json
{
  "id": "fictional-selected-bid-id",
  "impid": "fictional-slot",
  "price": 1.23,
  "crid": "fictional-creative-id",
  "w": 300,
  "h": 250,
  "ext": {
    "trusted_server": {
      "renderer": {
        "type": "aps",
        "version": 1,
        "accountId": "example-account-id",
        "bidId": "fictional-selected-bid-id",
        "creativeId": "fictional-creative-id",
        "tagType": "iframe",
        "creativeUrl": "https://creative.example/render",
        "aaxResponse": "eyJzZWF0YmlkIjpbeyJiaWQiOltdfV19",
        "width": 300,
        "height": 250
      }
    }
  }
}
```

`creativeId`/`crid` are omitted together when APS does not return `crid`. `adm` is
omitted, not serialized as an empty string. Ordinary non-APS `adm` behavior is
unchanged.

### 10.3 Publisher navigation and page-bids

`build_bid_map` adds the same complete descriptor as a normal, non-debug `renderer`
property for APS winners:

```json
{
  "fictional-slot": {
    "hb_pb": "1.23",
    "hb_bidder": "aps",
    "hb_adid": "fictional-selected-bid-id",
    "renderer": {
      "type": "aps",
      "version": 1,
      "accountId": "example-account-id",
      "bidId": "fictional-selected-bid-id",
      "creativeId": "fictional-creative-id",
      "tagType": "iframe",
      "creativeUrl": "https://creative.example/render",
      "aaxResponse": "eyJzZWF0YmlkIjpbeyJiaWQiOltdfV19",
      "width": 300,
      "height": 250
    }
  }
}
```

`hb_adid` priority becomes:

1. PBS Cache UUID for cached Prebid bids;
2. APS renderer selected bid ID;
3. existing OpenRTB `adid` fallback.

The complete renderer object is emitted only for the winning bid. It is not gated by
`debug.inject_adm_for_testing`, and general bid metadata remains debug-only.

---

## 11. Browser rendering design

### 11.1 Static renderer endpoint

Do not put the APS initializer and runner into publisher-origin `srcdoc`. Register APS
as a proxy-capable integration (without an automatic JS bundle) and serve a static
renderer document at:

```text
GET /integrations/aps/renderer
```

The document contains only static Trusted Server initialization code and the fixed
`https://client.aps.amazon-adsystem.com/prebid-creative.js` script. Before loading the
vendor runner, the static initializer reads a strictly encoded, cryptographically
random per-frame nonce from the iframe URL fragment, stores it as the expected nonce,
and removes the fragment from visible history. The fragment is not sent in the HTTP
request. It then receives one versioned renderer descriptor from its embedding parent
through `postMessage`, verifies `event.source === parent`, and requires the message
nonce to equal the independently established expected nonce. After the first accepted
message it removes the listener, initializes the account-keyed APS queue, dispatches
`CustomEvent("prebid/creative/render", { detail: { aaxResponse, seatBidId } })`, and only
then dynamically inserts the fixed runner script. On runner load or failure it sends a
source-bound response carrying the accepted nonce. Because the sandboxed document has
an opaque origin, both directions use `"*"` as the `postMessage` target origin and rely
on the exact child/parent window, source checks, one-time fragment-bound nonce, and full
descriptor validation.

The descriptor is data, never generated script text. No APS-provided HTML, script text,
URL, account value, or decoded JSON is concatenated into an executable context. The
vendor runner URL is a compile-time constant. The response carries
`Content-Type: text/html`, `X-Content-Type-Options: nosniff`,
`Referrer-Policy: no-referrer`, and a CSP `sandbox` directive containing the
approved tokens without `allow-same-origin`. The response-level sandbox preserves the
opaque boundary even if another embedding path omits its iframe sandbox attribute.

### 11.2 Opaque outer sandbox

Both direct and GPT flows load the static endpoint through `iframe.src`, not `srcdoc`,
with this outer sandbox:

```text
allow-forms
allow-pointer-lock
allow-popups
allow-popups-to-escape-sandbox
allow-scripts
allow-top-navigation-by-user-activation
```

The outer boundary deliberately omits `allow-same-origin`. The live APS runner grants
`allow-same-origin` to the nested frame it creates, but a descendant cannot relax an
ancestor sandbox's origin restriction. Therefore both `iframe` and fetched-HTML
`script` creatives remain under the outer opaque-origin boundary rather than inheriting
the publisher origin.

This is a compatibility hypothesis, not an assumption: the current runner also touches
`frameElement` while resizing. `tagtype=script` is rejected server-side before winner
selection while `allow_script_creatives` is false. The capability may be enabled for an
isolated test cohort only after a real-browser test proves that the fixed runner renders
and resizes correctly without publisher-origin access, and may be enabled beyond that
cohort only after controlled-account validation. If either test fails, keep the gate
off and obtain an APS-supported isolated renderer page/origin; do not add
`allow-same-origin` back to the outer boundary.

### 11.3 Browser validation of consumed data

Before creating or messaging the renderer iframe, TSJS must decode `aaxResponse` and
validate the exact envelope consumed by the vendor runner:

- root contains only one `seatbid` entry;
- that entry contains only one `bid`;
- the bid has exactly `id`, `price`, `w`, `h`, and `ext`;
- `ext` has exactly `creativeurl` and `tagtype`;
- bid ID, dimensions, creative URL, and tag type equal the duplicated descriptor fields;
- price is finite and non-negative;
- tag type is `iframe`, or `script` from a response the server emitted only after its
  default-off `allow_script_creatives` eligibility gate;
- creative URL is bounded HTTPS without credentials and does not match the publisher
  origin; and
- no markup, notifications, syncs, or unknown fields are present.

Validation happens before slot clearing, `stopImmediatePropagation()`, iframe creation,
or message dispatch. The static renderer document repeats the structural/message
validation as defense in depth.

### 11.4 Direct `/auction` flow

`parseAuctionResponse` accepts either ordinary sanitized `adm` or a structurally typed
renderer. The request loop performs the full decoded-envelope validation, dispatches a
valid APS descriptor to `renderApsCreative`, and sends ordinary markup to
`renderCreativeInline`.

`renderApsCreative` generates at least 128 bits of randomness with
`crypto.getRandomValues`, base64url-encodes it, and binds it to the static renderer URL
as a fragment before assigning `iframe.src`. After load, it posts the validated
descriptor with the same nonce, then waits for an exact source- and nonce-bound runner
ready acknowledgement before clearing existing content or showing the frame. A runner
failure or bounded acknowledgement timeout removes the hidden frame. A malformed
descriptor, malformed fragment, nonce mismatch, replay, stale message, or runner load
failure leaves existing slot content intact and emits a safe warning.

### 11.5 GPT/page-bids flow

Extend `installTsRenderBridge` while retaining the existing `Prebid Request` parsing and
source-slot ownership validation. For a valid APS winner, return the exact serializable
Prebid Universal Creative dynamic-renderer shape, including the supported renderer
version. The static Trusted Server renderer source may only create the opaque outer
iframe, load `/integrations/aps/renderer`, post the already validated descriptor, and
resolve only after the source- and nonce-bound ready acknowledgement; APS or bidder code
never executes in the Universal Creative renderer frame itself.
Fixture-test this message against the deployed Universal Creative protocol rather than
assuming a function can be structured-cloned. Include a trusted absolute
`rendererUrl`, computed from the publisher page origin by TSJS, because a relative URL
inside the GAM creative would resolve against the GAM origin. The dynamic renderer
must accept only that data field and the validated APS descriptor.

Call `stopImmediatePropagation()` only after ownership and complete envelope validation
succeed. Preserve in-flight render deduplication, but do not invoke the generic
win/billing beacon path for APS. Existing debug-ADM and PBS Cache branches remain
unchanged.

When the winning bid has a Trusted Server APS renderer, do not call
`apstag.setDisplayBids()`. The native APS SDK may remain installed by the CMS for other
cohorts, but it is not the renderer or bid source for that TS winner.

### 11.6 CSP and real-browser gate

Because the outer frame navigates to a real response, it uses that response's CSP
instead of inheriting publisher `srcdoc` CSP. The renderer endpoint starts from this
explicit ad-compatible policy, tightened if controlled testing permits:

```text
default-src 'none';
script-src 'unsafe-inline' https:;
connect-src https:;
frame-src https:;
img-src https: data:;
media-src https: blob:;
style-src 'unsafe-inline' https:;
font-src https: data:;
```

The publisher must permit `frame-src 'self'` for the direct renderer endpoint. The GAM
Universal Creative context must also permit framing the absolute publisher renderer
URL. The broad HTTPS resource allowances apply only inside the opaque sandbox and are
required because the APS runner and bidder HTML may load arbitrary HTTPS ad resources.

Add a Playwright test under `crates/trusted-server-integration-tests/browser` with a
restrictive publisher CSP. It must prove the renderer endpoint loads, APS initializes,
both tag types obey the outer opaque-origin boundary, a fictional script creative
cannot access `top.document`, and malformed descriptors do not clear the slot. The server-side `allow_script_creatives` gate stays false during normal winner
selection until this test passes. It may then be enabled only for the controlled test
cohort needed to validate real rendering and remains false elsewhere until that
validation also passes.

---

## 12. Sanitization and security invariants

- `sanitize_creative_html` remains mandatory for every ordinary
  `Bid.creative`/`adm` value.
- Its element, attribute, URI, size, and fail-closed rules do not change.
- APS executable rendering is a separate typed capability; it is never disguised as
  sanitized `adm`.
- Only a server-validated APS descriptor with an exact browser-validated envelope can
  select that capability.
- Unknown providers cannot request the APS renderer through arbitrary metadata.
- The server never fetches `creativeurl`, avoiding an SSRF/redirect/content-type
  service in the auction path.
- Server and browser both require a bounded HTTPS creative URL without credentials and
  reject the publisher origin.
- APS/bidder execution occurs only below an outer opaque-origin sandbox. No test may
  treat absence of `innerHTML` alone as proof; cover `srcdoc`, script text,
  `document.write`, and message-driven sinks.
- Existing same-origin/no-CORS-preflight protection for page-bids remains unchanged.
- Per-user injected HTML stays private/non-shared, and page-bids stays no-store.
- Existing upstream body caps and auction deadlines remain in force.

---

## 13. APS notification policy

Do not map APS `nurl` or `burl` into the shared browser bid map in this issue. The live
APS runner owns creative tracking, the official fixture does not establish that Trusted
Server should fire OpenRTB notifications, and the direct `/auction` path has no matching
notification lifecycle.

This also prevents an APS-controlled URL from causing a credentialed same-origin POST
through the current generic `sendBeacon`/fetch helper. If APS later requires Trusted
Server notification delivery, design it separately with one explicit owner, bounded
HTTPS URLs, publisher-origin rejection, omitted credentials, no referrer, macro policy,
and equivalent direct/GPT tests. Existing non-APS notification behavior is unchanged by
#764.

---

## 14. User sync policy

APS user sync is explicitly out of scope:

- ignore `BidResponse.ext.userSyncs`;
- do not put sync URLs into `AuctionResponse.metadata`;
- do not include them in the minimized renderer envelope;
- do not fetch them at the edge; and
- do not add image/iframe sync behavior to TSJS.

A future implementation requires a separate spec covering Purpose 1, regional policy,
URL allowlisting, deduplication, browser timing, and observability.

---

## 15. Logging and telemetry

- Request/response bodies: TRACE only.
- Routine request log: provider and eligible slot count, without account ID.
- Routine response log: status, elapsed time, accepted count, and bounded dropped
  count.
- WARN: static condition plus slot/request-safe context; never include response body,
  creative URL, bid token, EID, consent string, or account ID.
- Client-facing provider summaries: aggregate reason keys/counts only.
- Existing auction telemetry receives decoded price, currency, ad domain, and ad ID.
  It must not receive the encoded renderer envelope.

---

## 16. Rollout and rollback

### 16.1 Controlled rollout

1. Implement server parsing and exact fictional fixtures with APS disabled and
   `allow_script_creatives = false` by default.
2. Prove disabled script bids are dropped before candidate reduction/winner selection.
3. Pass the restrictive-CSP Playwright and opaque-origin access tests using the local
   fictional script runner while the server gate remains false for normal traffic.
4. Configure a controlled APS test account out of band.
5. Enable Trusted Server APS only for a test cohort, disable native APS demand there,
   and temporarily set `allow_script_creatives = true` only for that isolated cohort.
6. Verify the official fixture safe-drops and a real account returns the unverified
   `creativeurl`/`tagtype` contract.
7. Verify both observed tag types through direct and GPT paths. If script cannot operate
   below the opaque outer sandbox, set the gate false again and stop.
8. Confirm the exact allowlisted envelope works with `prebid-creative.js`.
9. Confirm GAM line items and the Universal Creative route request `hb_bidder=aps` and
   the selected `hb_adid`.
10. Obtain APS confirmation for Fastly/edge-originated production traffic before broad
    rollout.

### 16.2 Rollback

- Immediate traffic rollback: set `[integrations.aps].enabled = false`.
- Restore native APS participation for the affected cohort if required.
- Binary rollback remains available if the old provider must temporarily be restored.
- The new code does not retain a dormant legacy parser or protocol switch.

---

## 17. Required code areas

| File                                                              | Change                                                                                                                                                       |
| ----------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `crates/trusted-server-core/src/integrations/aps.rs`              | Replace legacy wire types, build/parse OpenRTB, reduce one APS candidate per imp, build the exact envelope, and serve the static renderer endpoint with CSP. |
| `crates/trusted-server-core/src/integrations/mod.rs`              | Register the APS proxy integration in addition to the auction provider.                                                                                      |
| `crates/trusted-server-core/src/openrtb.rs`                       | Add only narrowly scoped APS/request/response extension types if needed.                                                                                     |
| `crates/trusted-server-core/src/auction/types.rs`                 | Add generic bid/creative identifiers and typed optional renderer. Remove stale encoded-price comments.                                                       |
| `crates/trusted-server-core/src/auction/formats.rs`               | Preserve IDs; sanitize ordinary `adm`; emit the full APS renderer extension and omit empty `adm`.                                                            |
| `crates/trusted-server-core/src/auction/orchestrator.rs`          | Replace encoded-price assumptions/tests with decoded APS direct-winner coverage.                                                                             |
| `crates/trusted-server-core/src/integrations/adserver_mock.rs`    | Preserve renderer/identifiers for the single reduced APS candidate and test the prior two-bid collision case.                                                |
| `crates/trusted-server-core/src/publisher.rs`                     | Emit APS renderer in the normal bid map, force `hb_bidder=aps`, and use selected bid ID for `hb_adid` without APS notifications.                             |
| `crates/trusted-server-js/lib/test/fixtures/aps-renderer-v1.json` | Shared fictional golden wire fixture consumed by Rust and TypeScript tests.                                                                                  |
| `crates/trusted-server-js/lib/src/core/auction.ts`                | Parse the typed renderer extension.                                                                                                                          |
| `crates/trusted-server-js/lib/src/core/request.ts`                | Validate and dispatch APS descriptors to the opaque renderer endpoint.                                                                                       |
| `crates/trusted-server-js/lib/src/core/types.ts`                  | Define shared APS renderer wire types with optional creative ID.                                                                                             |
| `crates/trusted-server-js/lib/src/integrations/aps/render.ts`     | Decode/cross-check the exact envelope, create the opaque outer iframe, and message the static endpoint.                                                      |
| `crates/trusted-server-js/lib/src/integrations/gpt/index.ts`      | Serve APS through the exact Universal Creative wire shape; skip generic beacons and native `apstag` for TS APS winners.                                      |
| `crates/trusted-server-integration-tests/browser/`                | Add restrictive-CSP, opaque-origin, script/iframe, and parent-DOM isolation coverage.                                                                        |
| `trusted-server.example.toml`                                     | Document canonical OpenRTB APS configuration with fictional values.                                                                                          |
| `docs/guide/integrations/aps.md`                                  | Replace legacy setup, request, response, rendering, privacy, and rollout documentation.                                                                      |
| `CHANGELOG.md`                                                    | Record the APS wire/config migration and support caveat.                                                                                                     |

---

## 18. Acceptance criteria

1. APS receives the specified OpenRTB request for each eligible banner slot with
   `topframe=0` and `secure=1`.
2. Request fixtures prove trimmed account/SDK/USD, floors, trusted page/device context,
   consent/EIDs, existing privacy registrations, and omission of precise coordinates
   and unsupported COPPA claims.
3. HTTP 204 and empty `seatbid` are no-bids; malformed/legacy responses are safely
   diagnosed.
4. The official no-renderer fixture safe-drops; every accepted APS bid has present,
   valid `w`/`h` and the unverified-but-validated URL/tag-type renderer contract.
5. `Bid.bidder`/`hb_bidder` are always `aps`, independent of upstream seat.
6. Multiple APS bids for one impression reduce deterministically to one candidate, and
   mediation cannot restore another candidate's renderer.
7. APS can win without a mediator and is rejected normally below the floor.
8. The browser envelope contains exactly the allowlisted fields and matches the full
   descriptor; Rust and TypeScript share one golden fixture.
9. The outer renderer iframe has no `allow-same-origin`; descendant iframe/script
   creatives cannot access publisher DOM in the restrictive-CSP Playwright test.
10. `allow_script_creatives` defaults false; while false, script bids are dropped before
    candidate reduction and cannot win. It is enabled only under the staged browser and
    controlled-account policy; otherwise script remains fail-closed.
11. The renderer binds a one-time nonce through the iframe URL fragment before
    messaging and rejects malformed, mismatched, replayed, or stale messages before
    clearing content or stopping another renderer handler.
12. Ordinary non-APS `adm` sanitization and PBS Cache rendering remain unchanged.
13. APS user sync and APS `nurl`/`burl` browser delivery remain absent.
14. Native `apstag.setDisplayBids()` is not called for a TS APS renderer winner.
15. Raw request/response bodies remain TRACE-only.
16. All repository CI, adapter, JS, browser, parity, and docs-format gates pass.

---

## 19. Known risks and follow-ups

| Risk                                                                   | Handling                                                                                                                                                                    |
| ---------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| APS has not confirmed production Fastly/edge traffic                   | Continue implementation/testing; block broad rollout pending confirmation.                                                                                                  |
| Official fixture and live runner disagree on creative extension fields | Safe-drop the official fixture, pin the observed runner hash/date as evidence only, and require controlled-account validation.                                              |
| Exact envelope may omit a field used by a future runner                | Stop and review with APS; never preserve unknown fields or fall back to the full response automatically.                                                                    |
| Fixed unversioned vendor script can change independently               | Monitor the real-browser contract and require review before changing URL, envelope, CSP, or sandbox behavior.                                                               |
| Runner may fail without outer `allow-same-origin`                      | Keep the server-side `allow_script_creatives` gate false unless the opaque-boundary Playwright and controlled-account tests pass; never restore publisher-origin execution. |
| Broad ad-compatible CSP permits arbitrary HTTPS resources              | Scope it to the opaque renderer document; publisher CSP only needs to frame the same-origin renderer endpoint.                                                              |
| Existing native APS and TS APS can duplicate demand                    | Cohort-gate and disable native APS participation wherever TS APS is enabled.                                                                                                |
| APS notifications may be required for billing                          | Do not guess; vendor runner owns tracking until APS approves a separately secured notification contract.                                                                    |
| APS video responses could appear                                       | Advertise banner-only support and drop non-banner `mtype` until a separate video design exists.                                                                             |
