# Amazon Publisher Services (APS) OpenRTB Integration

Trusted Server can request banner bids from Amazon Publisher Services (APS) through the APS OpenRTB endpoint and let their decoded USD CPMs compete with other auction providers.

> [!IMPORTANT]
> APS's public adapter metadata describes Prebid Server support as unavailable. Confirm edge/server-originated traffic with your APS account team before a broad production rollout. Start with an isolated cohort and disable publisher-native APS demand for that cohort to avoid duplicate demand.

## Scope

The integration supports:

- banner impressions;
- APS OpenRTB requests to the provider's configured HTTPS endpoint;
- decoded-CPM winner selection with or without a mediator;
- direct `/auction` rendering;
- client-side `trustedServer` Prebid adapter auctions through GAM; and
- initial-navigation and page-bids rendering through GAM/Prebid Universal Creative.

The integration does not implement:

- video or native impressions;
- APS user sync;
- Trusted Server delivery of APS `nurl` or `burl`; or
- native `apstag.setDisplayBids()` handling for Trusted Server APS winners.

## Configuration

APS server ownership is entirely under an auction provider. The optional
`[integrations.aps]` table controls browser-side behavior; it does not own the
APS account, endpoint, timeout, debug behavior, inventory identity, or script
policy. APS renderer support is registered whenever the compiled auction plan
contains an `aps` profile, even if `[integrations.aps]` is absent or disabled.

```toml
[auction]
enabled = true
timeout_ms = 2000

mediator = "adserver_mock"

[auction.providers.aps-main]
protocol = "openrtb-2.6"
profile = "aps"
endpoint = "https://aps.example.com/e/pb/bid"
routing = "all_eligible"

[auction.providers.aps-main.profile_config]
account_id = "example-aps-account"
debug = false
allow_script_creatives = false
# Configure both only when authorized inventory differs from the deployment host.
# inventory_domain = "inventory.example.com"
# inventory_page_origin = "https://www.inventory.example.com"

[integrations.adserver_mock]
enabled = true
endpoint = "https://mediator.example.com/mediate"
timeout_ms = 500
```

The optional browser integration table controls rendering ownership:

```toml
[integrations.aps]
enabled = true
# Default. Set publisher_native only for the controlled friendly-frame experiment below.
rendering_mode = "trusted_server"
```

`rendering_mode` is a strict enum. `trusted_server` (the default) retains the
opaque static renderer route. `publisher_native` disables that route and adds
`data-ts-aps-rendering-mode="publisher_native"` to the server-generated TSJS
bundle tag. TSJS captures this server-owned attribute when the bundle executes,
so markup added later cannot change the mode. The attribute works under a
publisher CSP that blocks inline scripts. Unknown values fail configuration
deserialization.

### Publisher-native runner experiment

`publisher_native` is an opt-in browser experiment, **not** general APS
compatibility proof. No public `apstag` API was found that accepts an externally
selected OpenRTB `aaxResponse`. In controlled browser testing,
`apstag.renderImp(document, bidId)` did not render the Trusted Server bid because
that bid was absent from the SDK's browser-auction state. Trusted Server
therefore does not call `apstag`, `fetchBids`, or `setDisplayBids`, mutate the
publisher's APS SDK, or start a second auction. Instead, this mode reuses the
same `prebid/creative/render` runner contract already used by `trusted_server`
mode, but inside a publisher-origin frame. That observed vendor contract still
requires APS account-team validation.

No publisher JavaScript change is required. After validating and freezing the
exact selected descriptor, Trusted Server JS:

1. resolves the direct-auction slot or its injected GAM div mapping;
2. creates a hidden, publisher-origin friendly iframe sized to the winner;
3. initializes only that fresh frame's account-scoped `_aps` event queue;
4. queues `prebid/creative/render` with the selected `aaxResponse` and bid ID;
5. loads the fixed `https://client.aps.amazon-adsystem.com/prebid-creative.js`
   runner.

The existing publisher content remains visible until the runner script loads.
A runner error, a blocked script, a missing slot, a superseding dispatch, or a
load taking longer than 10 seconds removes the pending frame and declines the
bid. It never falls back to `/integrations/aps/renderer` or sends a Universal
Creative renderer response. Trusted Server treats runner load as successful
handoff; the runner owns subsequent creative completion and resource loading.

Unlike `trusted_server` mode, this friendly frame has no opaque-origin sandbox.
Its initial document inherits the publisher CSP, so the publisher policy
controls whether the APS runner and required creative resources can load.
Trusted Server sets the frame document's referrer policy to `no-referrer`,
matching the static renderer's existing protection. The fixed APS runner and
its creative otherwise execute with publisher-origin privileges, so
`publisher_native` has a larger security surface, especially when
`allow_script_creatives = true`. Use only a controlled cohort.

For a client-side Prebid APS capability, Trusted Server consumes the one-shot
capability before starting the runner and calls `markWinningBidAsUsed` only
after the runner loads. For server/GPT ownership, it similarly claims the
slot/ad ID first. This prevents native and Trusted Server rendering from both
owning the same response.

Disable or coordinate existing publisher-native APS demand for every
`publisher_native` cohort. Otherwise the publisher's normal APS auction and
this server-selected bid can duplicate demand. Validate the exact account,
inventory, CSP, iframe/script creative behavior, impression reporting, and
click-through behavior with the APS account team before production rollout.

The common provider `endpoint` is required and must be an absolute HTTPS URL
with a host and no credentials or fragment. The legacy `/e/dtb/bid` path is
rejected. `timeout_ms` belongs beside `endpoint`; when omitted, the `aps`
profile default is 800 ms. Runtime caps it by the remaining auction budget.

`profile_config.account_id` is required, nonempty, and at most 1024 bytes. It is
the canonical field; integration-owned `account_id`, `pub_id`, endpoint, and
timeout fields are not part of the public schema. `debug` and
`allow_script_creatives` both default to `false`.

Enable `debug` only on controlled test sites because it includes the raw APS
request and response—including identity, consent, device, page, account, bid,
and creative data—in client-visible `/auction` metadata.

Set `inventory_domain` and `inventory_page_origin` together only when the public
deployment hostname differs from APS-authorized inventory. The domain becomes
`site.domain`. The HTTPS page origin replaces the current page's scheme and host
while preserving its path; query and fragment are removed. The origin must be
the inventory domain or a subdomain and cannot contain credentials, a port,
path, query, or fragment.

`routing = "all_eligible"` is the usual APS configuration: every
banner-compatible slot is eligible without a synthetic APS bidder entry. It
does not expose bidder parameters routed to another provider. Use
`routing = "explicit"` only when APS participation should require a central
bidder route:

```toml
[auction.providers.aps-main]
protocol = "openrtb-2.6"
profile = "aps"
endpoint = "https://aps.example.com/e/pb/bid"
routing = "explicit"

[auction.providers.aps-main.profile_config]
account_id = "example-aps-account"

[auction.bidders.aps]
provider = "aps-main"
```

The optional mediator stays separate under `[auction].mediator`; never declare
it under `[auction.providers]` or `[auction.bidders]`.

APS uses ordinary auction slot IDs and banner formats. Legacy creative-
opportunity APS `slot_id` values are ignored, and `bidders.aps.slotID` is not
required.

## OpenRTB request

Trusted Server builds the APS request independently from its Prebid Server request. The request includes:

- `ext.account` from `account_id`;
- `ext.sdk = { "source": "prebid", "version": "2.2.0" }`;
- secure banner impressions and configured floors;
- page, site, device, and consent fields allowed by the existing privacy gates; and
- eligible EIDs only when consent policy permits them.

Precise latitude/longitude, disallowed identifiers, unsupported media types, and Trusted Server/Prebid-only extensions are not forwarded. The page URL is a validated publisher-owned URL with query and fragment removed; the raw browser `Referer` is not forwarded as `site.ref`. Unsafe or oversized page URLs are omitted or replaced by the validated publisher fallback.

Raw outbound and inbound payloads are logged only at TRACE level. With debug disabled, auction metadata contains only aggregate counts and drop reasons.

## Debug mode

Set `debug = true` under
`[auction.providers.<id>.profile_config]` to include the direct APS HTTP
exchange in that provider's summary returned by `POST /auction`:

```json
{
  "metadata": {
    "debug": {
      "httpcalls": {
        "aps": [
          {
            "requestbody": "{...}",
            "requestheaders": { "content-type": ["application/json"] },
            "responsebody": "{...}",
            "responseheaders": { "content-type": ["application/json"] },
            "status": 200,
            "uri": "https://aps.example.com/e/pb/bid"
          }
        ]
      }
    }
  }
}
```

This follows the Prebid Server `metadata.debug.httpcalls` representation. APS makes one direct HTTP call per provider per auction, and the map preserves the legacy `aps` key with one entry. Request and captured response bodies are strings, and header values are arrays so repeated headers are preserved. If a non-success response body cannot be read within the existing 2 MiB upstream limit, `responsebody` is omitted rather than reported as an empty body. APS does not add PBS-only `resolvedrequest` or `bidstatus` fields.

The debug exchange is emitted for successful responses, `204 No Content`, malformed response bodies, and non-success HTTP statuses. Transport failures and auction timeouts happen before an HTTP response reaches the parser and continue to use the orchestrator's normal error metadata.

> [!WARNING]
> APS debug metadata is unredacted and client-visible. Use it only on controlled test sites, and disable it before production rollout.

## Bid eligibility and selection

APS responses must use USD when a response currency is present. Each eligible bid must have:

- a known `impid`;
- a finite decoded price;
- positive `w` and `h` that match a configured banner format;
- an HTTPS, credential-free `ext.creativeurl` on an origin other than the publisher; and
- `ext.tagtype` equal to `iframe`, or `script` when the script gate is enabled.

Trusted Server rejects legacy contextual response shapes and bids with markup-only render sources. It deterministically keeps one eligible APS candidate per impression by highest price, then lexicographically smallest bid ID. This reduction prevents same-slot renderer ambiguity in mediation.

APS bids use `aps` as both the bidder identity and `hb_bidder`, regardless of an upstream seat value. The selected APS bid ID is used for `hb_adid`. APS then competes directly against other decoded-price bids and ordinary slot floors.

## Rendering security model

Trusted Server does not insert APS creative markup into the publisher document. It serializes only the selected bid into a versioned renderer descriptor. The base64 OpenRTB envelope has exactly this shape:

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

Seats, `impid`, markup, notifications, user-sync data, sibling bids, losing seats, and unknown fields are not exposed. The browser decodes this envelope and cross-checks the ID, dimensions, URL, and tag type before any DOM mutation or message suppression.

In `trusted_server` mode, both rendering paths use `GET /integrations/aps/renderer`, a static Trusted Server document with its own restrictive CSP. The document initializes the account-keyed APS queue and then loads only the fixed runner at `https://client.aps.amazon-adsystem.com/prebid-creative.js`.

The outer iframe uses these sandbox permissions:

```text
allow-forms
allow-pointer-lock
allow-popups
allow-popups-to-escape-sandbox
allow-scripts
allow-top-navigation-by-user-activation
```

It deliberately omits `allow-same-origin`, so APS and bidder execution remains below an opaque-origin boundary. The renderer response repeats these restrictions with a CSP `sandbox` directive, preventing another embedding path from restoring publisher-origin execution by omitting the iframe attribute. Trusted Server generates a fresh 128-bit nonce, binds it in the iframe URL fragment before navigation, and requires the same one-time nonce in the parent message and renderer acknowledgement. Existing slot content is retained until the static renderer has accepted the descriptor and loaded the fixed runner.

### Direct `/auction`

In `trusted_server` mode, the TSJS auction client validates the typed renderer descriptor, creates the opaque renderer iframe, and sends the minimized envelope after the frame loads. In `publisher_native` mode it creates the injected friendly iframe and queues the response for the fixed APS Prebid creative runner. Ordinary non-APS `adm` continues through the existing sanitizer and generic creative iframe.

### GAM and Universal Creative

For initial navigation and page-bids, Trusted Server publishes the same descriptor in `window.tsjs.bids`. The source-checked Prebid Universal Creative bridge accepts requests only from the iframe that owns the matching `hb_adid` and validates the complete envelope. In `trusted_server` mode it returns a static dynamic-renderer program that creates the same opaque renderer iframe. In `publisher_native` mode it instead resolves the publisher div and starts the friendly-frame runner without sending a Universal Creative renderer response.

After the native runner loads, Trusted Server replaces the existing children of the resolved publisher div with the friendly frame. This removes the GAM or Universal Creative iframe when it is inside that div. If the runner fails, the existing iframe remains, but its Universal Creative request receives no response because Trusted Server has already claimed the selected bid. This one-owner behavior avoids a second render path, but GAM impression and viewability reporting must be validated with the APS account team for the controlled cohort.

For client-side `trustedServer` adapter auctions, Prebid generates its own `hb_adid`. Trusted Server binds that generated ID to the validated APS descriptor in a bounded, expiring browser registry before GAM refresh. The bridge verifies that the requesting Universal Creative iframe belongs to the same ad unit, consumes the capability once, and passes the APS bid ID separately to the Amazon runner.

These paths do not fetch PBS Cache, fire generic APS win/billing beacons, or call `apstag.setDisplayBids()` for the Trusted Server winner. Publisher-owned native APS objects are otherwise left untouched.

## Publisher CSP

The publisher policy must permit the same-origin renderer route, for example:

```text
frame-src 'self'
```

Do not add `allow-same-origin` to the outer renderer sandbox. The renderer endpoint supplies its own CSP for the fixed runner and HTTPS creative resources. The same-origin renderer route inherits the publisher page scheme; use HTTPS in production. APS endpoints and third-party creative URLs always require HTTPS.

Before enabling script creatives, verify under the publisher's actual CSP that both iframe and script-tag creatives:

- render and size correctly;
- cannot read or modify `top.document`;
- cannot restore publisher-origin execution;
- reject malformed descriptors, nonce mismatches, and replay; and
- work through both direct and GAM/Universal Creative paths.

If script rendering requires weakening the outer sandbox, leave `allow_script_creatives = false` and consult APS instead.

## Migration from the legacy APS integration

This release is a direct configuration and protocol cutover:

1. Move `endpoint` and `timeout_ms` to `[auction.providers.<id>]` and use
   `/e/pb/bid`; `/e/dtb/bid` remains rejected.
2. Move `account_id`, `debug`, `allow_script_creatives`, and inventory overrides
   to the provider's `profile_config`; `pub_id` is not part of the new schema.
3. Remove APS-specific slot ID configuration and any APS entry from old Prebid
   Server bidder lists. Use `routing = "all_eligible"` or an explicit
   `[auction.bidders.aps]` route.
4. Prepare GAM line items and Universal Creative for `hb_bidder=aps` and the
   selected APS `hb_adid`.
5. Disable publisher-native APS demand for the Trusted Server test cohort.

There is no legacy runtime switch. Roll back by disabling `[auction]` or
removing the APS provider, restoring native APS for the cohort, or deploying
the prior binary.

Changing `rendering_mode` does not update pages that are already loaded or stored in an HTML cache. A cached `trusted_server` page can continue requesting `/integrations/aps/renderer` after a native-mode deployment removes that route. A cached `publisher_native` page continues using its captured native mode after rollback. Coordinate the mode change with HTML cache expiry or purge and reload active test sessions before judging the result.

## Rollout

Use fictional values in source-controlled configuration and fixtures. Supply controlled account details out of band.

1. Obtain APS account-team confirmation for edge-originated OpenRTB traffic.
2. Enable Trusted Server APS only for an isolated cohort and disable native APS demand there.
3. Keep the default `trusted_server` mode and `allow_script_creatives = false`; observe iframe bids through direct and GAM paths.
4. Confirm outbound privacy fields, aggregate diagnostics, decoded-price competition, line-item targeting, dimensions, click-throughs, and opaque-origin isolation.
5. In a still-smaller cohort, set `rendering_mode = "publisher_native"` and confirm the fixed runner request, friendly-frame dimensions, iframe creatives, impression reporting, and click-throughs without a request to `/integrations/aps/renderer`.
6. Purge or expire cached HTML and reload active test sessions when changing modes. Confirm the publisher CSP permits the runner but does not need to permit inline Trusted Server scripts.
7. Only after reviewing the friendly-frame security tradeoff, enable script creatives for the isolated native cohort and validate them in a real browser.
8. Expand traffic only after APS confirmation and successful controlled validation.

## Troubleshooting

### No APS bids

- Confirm `account_id` and account eligibility with APS.
- Confirm the endpoint is `/e/pb/bid` and uses HTTPS without credentials.
- If the deployment hostname differs from APS-authorized inventory, configure both `inventory_domain` and `inventory_page_origin` with the APS-approved identity.
- Ensure an `[auction.providers.<id>]` entry selects `profile = "aps"`.
- Check aggregate APS drop reasons for currency, dimensions, render source, URL, tag type, or script-gate rejection.
- Confirm the provider timeout fits inside the auction timeout.
- On a controlled test site, set profile `debug = true` and inspect
  `ext.orchestrator.provider_details[].metadata.debug.httpcalls.aps` in the
  `/auction` response.

### Winner targets but does not render

- In `trusted_server` mode, confirm `GET /integrations/aps/renderer` returns HTML with its CSP and `Referrer-Policy: no-referrer`, and that publisher CSP permits `frame-src 'self'`.
- In `publisher_native` mode, confirm the `#trustedserver-js` bundle tag carries `data-ts-aps-rendering-mode="publisher_native"`, the slot receives a hidden friendly iframe, and publisher CSP does not block `https://client.aps.amazon-adsystem.com/prebid-creative.js` or the selected creative's resources. The static renderer route is intentionally absent in this mode. Runner script load is the handoff signal, not proof that the creative painted.
- Confirm the GAM creative uses the supported Prebid Universal Creative bridge and the winning `hb_adid`.
- For client-side `trustedServer` adapter auctions, confirm Prebid's `bidResponse` contains a generated `adId` and that the corresponding capability appears briefly in `window.tsjs.apsPrebidRenderers` before rendering.
- Ensure no publisher APS auction is trying to handle the same cohort.
- Keep script creatives disabled while diagnosing either rendering mode.

## Verification

```bash
cargo test-fastly integrations::aps
cargo test-fastly auction::orchestrator
cargo test-fastly integrations::adserver_mock

cd crates/trusted-server-js/lib
npx vitest run test/integrations/aps/render.test.ts test/core/auction.test.ts
```

See `crates/trusted-server-core/src/integrations/aps.rs` for the request/parser implementation and `crates/trusted-server-js/lib/src/integrations/aps/render.ts` for the browser renderer contract.
