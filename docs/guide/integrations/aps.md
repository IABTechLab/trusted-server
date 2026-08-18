# Amazon Publisher Services (APS) OpenRTB Integration

Trusted Server can request banner bids from Amazon Publisher Services (APS) through the APS OpenRTB endpoint and let their decoded USD CPMs compete with other auction providers.

> [!IMPORTANT]
> APS's public adapter metadata describes Prebid Server support as unavailable. Confirm edge/server-originated traffic with your APS account team before a broad production rollout. Start with an isolated cohort and disable publisher-native APS demand for that cohort to avoid duplicate demand.

## Scope

The integration supports:

- banner impressions;
- APS OpenRTB requests to the integration's built-in production endpoint;
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

```toml
[integrations.aps]
enabled = true
account_id = "example-aps-account-id"
timeout_ms = 800
# Include raw APS request/response data in /auction metadata on test sites only.
debug = false
# Set both when the deployment hostname differs from APS-authorized inventory.
# inventory_domain = "publisher.example"
# inventory_page_origin = "https://www.publisher.example"
allow_script_creatives = false

[auction]
enabled = true
providers = ["aps", "prebid"]
timeout_ms = 2000
```

`account_id` is the canonical field. `pub_id` remains a compatibility alias for migration, including integer values, but new configuration should not use it. Supplying both names is an error.

`debug` defaults to `false`. Enable it only on controlled test sites because it includes the raw APS request and response, including identity, consent, device, page, account, bid, and creative data, in the client-visible `/auction` response.

`allow_script_creatives` defaults to `false`. While disabled, APS script bids are rejected before per-impression reduction, floors, mediation, and winner selection. Enable it only for a controlled cohort after the browser-security checks in [Rollout](#rollout) pass.

Set `inventory_domain` and `inventory_page_origin` together only when the public deployment hostname differs from the inventory identity authorized by APS. The domain becomes `site.domain`. The HTTPS page origin replaces the current page's scheme and host while preserving its path; query and fragment data are removed before forwarding. The origin must be the inventory domain or one of its subdomains and cannot include credentials, a port, path, query, or fragment. These values come only from operator configuration; Trusted Server never accepts APS inventory identity from the client auction payload.

APS uses ordinary auction slot IDs and banner formats. Legacy creative-opportunity APS `slot_id` configuration is accepted for compatibility but ignored, and `bidders.aps.slotID` is not required. Remove both during migration.

The APS provider may also participate through a configured mediator:

```toml
[auction]
enabled = true
providers = ["aps", "prebid"]
mediator = "adserver_mock"
timeout_ms = 2000
```

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

Set `debug = true` under `[integrations.aps]` to include the direct APS HTTP exchange in the APS provider summary returned by `POST /auction`:

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

This follows the Prebid Server `metadata.debug.httpcalls` representation. APS makes one direct HTTP call per auction, so the map uses the provider key `aps` with one entry. Request and captured response bodies are strings, and header values are arrays so repeated headers are preserved. If a non-success response body cannot be read within the existing 2 MiB upstream limit, `responsebody` is omitted rather than reported as an empty body. APS does not add PBS-only `resolvedrequest` or `bidstatus` fields.

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

Both rendering paths use a publisher-origin bootstrap followed by two nested `data:` documents. TSJS first loads `GET /integrations/aps/renderer?mode=data-bootstrap` with a sandbox that omits `allow-same-origin`. The bootstrap is therefore opaque and cannot read or modify the publisher document. After a nonce-bound readiness message, TSJS adds `allow-same-origin` and asks the bootstrap to navigate itself to a per-impression `data:` container. The container then creates the static inner `data:` renderer under the same permanent sandbox.

Both data documents are naturally opaque even with `allow-same-origin`, so the publisher cannot access them. Keeping that token on both frames also avoids WebKit propagating an opaque origin to the HTTPS creative: the creative and its same-origin descendants retain their real origin in Chromium, Firefox, and WebKit.

The outer container's CSP allows child frames from `data:` and only the fully validated creative URL's exact origin. That policy is inherited by the inner data renderer and intersects with the renderer's own CSP. It permits the expected creative frame but blocks the inner renderer from navigating itself to the publisher origin before a request is made. This exact-origin boundary intentionally blocks an immediate creative-frame redirect or same-frame navigation to a different origin; validate real APS inventory for intermediate or redirect origins before rollout. User-activated top navigation and popups remain governed by the sandbox tokens.

A network-loaded HTTPS creative does not inherit its ancestor's CSP and can create further frames. To prevent it from using a publisher-origin descendant plus an executable publisher gadget to regain access to the top page, enabling APS appends a separate `Content-Security-Policy: frame-ancestors 'self'` policy to every Trusted Server response. Browsers require every ancestor to match, so publisher documents cannot load below the opaque container or third-party creative. This secure default also prevents the APS-enabled publisher from being embedded cross-origin; publishers that require trusted external embedders need a separately reviewed ancestor-allowlist feature before enabling APS.

Trusted Server generates independent 128-bit nonces for the container and renderer. Once the inner renderer is ready, the container transfers a dedicated `MessagePort` between trusted top-page TSJS and the inner renderer. The complete descriptor travels only over that port; the inner renderer revalidates it, rejects the publisher origin, and requires the creative origin to equal the origin bound in the container CSP. The container URL contains only that exact origin, static renderer source, and nonces—never the response envelope, bid ID, price, or creative URL path.

The inner document initializes the account-keyed APS queue and loads only the fixed runner at `https://client.aps.amazon-adsystem.com/prebid-creative.js`. A `renderer-ready` acknowledgement still means that Amazon's runner loaded, not that the final creative painted. Existing slot content remains until that acknowledgement. The query-free `GET /integrations/aps/renderer` response remains available with its original stricter CSP and legacy message shape for cached clients and rollback. If bootstrap readiness is delayed, TSJS posts that exact two-field legacy descriptor after a brief compatibility wait while continuing to accept a later modern bootstrap acknowledgement.

### Direct `/auction`

The TSJS auction client validates the typed renderer descriptor and mounts the nested data renderer in the winning slot. Ordinary non-APS `adm` continues through the existing sanitizer and generic creative iframe.

### GAM and Universal Creative

For initial navigation and page-bids, Trusted Server publishes the same descriptor in `window.tsjs.bids`. The source-checked Prebid Universal Creative bridge accepts requests only from the iframe that owns the matching `hb_adid`, validates the complete envelope, and returns a static dynamic-renderer program with a one-shot mount capability. That program asks trusted top-page TSJS to mount the nested data renderer as a sibling of the Universal Creative iframe, outside inherited GAM and Universal Creative sandbox restrictions.

For client-side `trustedServer` adapter auctions, Prebid generates its own `hb_adid`. Trusted Server binds that generated ID to the validated APS descriptor in a bounded, expiring browser registry before GAM refresh. The bridge verifies that the requesting Universal Creative iframe belongs to the same ad unit, consumes both the renderer and mount capabilities once, and passes the APS bid ID separately to the Amazon runner.

After a Universal Creative APS mount commits, TSJS keeps the controller connected but hidden and records its prior inline display value. GPT's next `slotRequested` event removes only the committed APS frame, restores the controller's exact display value, and revokes unconsumed mount work for that slot before the refreshed creative lifecycle begins.

These paths do not fetch PBS Cache, fire generic APS win/billing beacons, or call `apstag.setDisplayBids()` for the Trusted Server winner. Publisher-owned native APS objects are otherwise left untouched.

## Publisher CSP

The publisher policy must permit the same-origin renderer route, for example:

```text
frame-src 'self' data:
```

The `data:` source is required for the bootstrap's self-navigation to the opaque container. APS also adds `frame-ancestors 'self'` as an independent response policy; do not override or remove that policy. Do not weaken or bypass the initial bootstrap sandbox. TSJS adds `allow-same-origin` only when navigating away from the publisher-origin bootstrap; both final data documents remain naturally opaque. The bootstrap response supplies the resource CSP for the fixed runner and HTTPS creative resources, while the container narrows `frame-src` to the validated creative origin. The same-origin bootstrap route inherits the publisher page scheme; use HTTPS in production. APS endpoints and third-party creative URLs always require HTTPS.

Before enabling script creatives, verify under the publisher's actual CSP that both iframe and script-tag creatives:

- render and size correctly;
- cannot read or modify `top.document`;
- cannot restore publisher-origin execution;
- reject malformed descriptors, nonce mismatches, and replay; and
- work through both direct and GAM/Universal Creative paths.

If script rendering requires weakening the outer sandbox, leave `allow_script_creatives = false` and consult APS instead.

## Migration from the legacy APS integration

This release is a direct protocol cutover:

1. Replace the legacy `/e/dtb/bid` endpoint with `/e/pb/bid`.
2. Rename `pub_id` to `account_id`.
3. Remove APS-specific slot ID configuration and remove `aps` from Prebid Server bidder lists. Trusted Server also filters APS from PBS requests for this path.
4. Prepare GAM line items and Universal Creative for `hb_bidder=aps` and the selected APS `hb_adid`.
5. Disable publisher-native APS demand for the Trusted Server test cohort.

There is no legacy runtime switch. Roll back by disabling `[integrations.aps]`, restoring native APS for the cohort, or deploying the prior binary.

## Rollout

Use fictional values in source-controlled configuration and fixtures. Supply controlled account details out of band.

1. Obtain APS account-team confirmation for edge-originated OpenRTB traffic.
2. Enable Trusted Server APS only for an isolated cohort and disable native APS demand there.
3. Keep `allow_script_creatives = false` and observe iframe bids through direct and GAM paths.
4. Confirm outbound privacy fields, aggregate diagnostics, decoded-price competition, line-item targeting, dimensions, click-throughs, and opaque-origin isolation.
5. Run the restrictive-CSP browser proof for script behavior.
6. Only then enable script creatives for the isolated cohort and validate them in a real browser.
7. Expand traffic only after APS confirmation and successful controlled validation.

## Troubleshooting

### No APS bids

- Confirm `account_id` and account eligibility with APS.
- Confirm the endpoint is `/e/pb/bid` and uses HTTPS without credentials.
- If the deployment hostname differs from APS-authorized inventory, configure both `inventory_domain` and `inventory_page_origin` with the APS-approved identity.
- Ensure `aps` appears in `auction.providers`.
- Check aggregate APS drop reasons for currency, dimensions, render source, URL, tag type, or script-gate rejection.
- Confirm the provider timeout fits inside the auction timeout.
- On a controlled test site, set `debug = true` and inspect `ext.orchestrator.provider_details[].metadata.debug.httpcalls.aps` in the `/auction` response.

### Winner targets but does not render

- Confirm `GET /integrations/aps/renderer?mode=data-bootstrap` returns HTML with its bootstrap CSP and `Referrer-Policy: no-referrer`.
- Confirm publisher CSP permits `frame-src 'self' data:`.
- Confirm the GAM creative uses the supported Prebid Universal Creative bridge and the winning `hb_adid`.
- For client-side `trustedServer` adapter auctions, confirm Prebid's `bidResponse` contains a generated `adId` and that the corresponding capability appears briefly in `window.tsjs.apsPrebidRenderers` before rendering.
- Ensure no native APS path is trying to handle the same cohort.
- Keep script creatives disabled while diagnosing iframe rendering.

## Verification

```bash
cargo test-fastly integrations::aps
cargo test-fastly auction::orchestrator
cargo test-fastly integrations::adserver_mock

cd crates/trusted-server-js/lib
npx vitest run test/integrations/aps/render.test.ts test/core/auction.test.ts
```

See `crates/trusted-server-core/src/integrations/aps.rs` for the request/parser implementation and `crates/trusted-server-js/lib/src/integrations/aps/render.ts` for the browser renderer contract.
