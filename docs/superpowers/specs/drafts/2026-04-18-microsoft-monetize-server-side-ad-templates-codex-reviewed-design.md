---
status: draft
---

# Microsoft Monetize Integration with Server-Side Ad Templates - Codex Reviewed Spec

_April 2026_

---

## Codex Review Status

This is a Codex-reviewed rewrite of
`2026-04-16-microsoft-monetize-server-side-ad-templates-design.md`.

The original concept is preserved: Trusted Server should move auction and ad
server decisioning off the page-rendering path, call Microsoft Monetize
server-side, and render creatives through lightweight browser-side iframe
injection.

This reviewed version tightens the product and technical claims in four areas:

1. `srcdoc` iframe isolation is not sufficient by itself. Creative rendering
   must use an explicit sandbox policy or a separate creative origin.
2. `defer` scripts can delay `DOMContentLoaded`. The ad bundle should be
   loaded with `async` and use a DOM-ready/slot-ready guard before injection.
3. Fast first-byte streaming of a JavaScript response is not the user-visible
   performance win. The value is early request start plus server-side decisioning.
4. Page HTML is cache-compatible, not universally cacheable by URL path. Real
   cache eligibility must account for publisher personalization, cookies,
   query strings, consent, auth state, and page invalidation.

This spec is recommended for a proof-of-concept, not yet a full production ad
stack replacement.

---

## 1. Executive Summary

Trusted Server will support a Microsoft Monetize server-side ad delivery path
for a single-publisher proof-of-concept.

When enabled, the page response does no auction work. Trusted Server injects one
early-loading ad-bundle script tag into `<head>`. That script request runs the
batched server-side auction, sends the auction evidence and slot context to
Microsoft Monetize, receives final creative markup, and injects sandboxed
iframes into known ad slots.

The goal is to prove that above-the-fold display ads can become visible in under
1 second on eligible pages while removing Prebid.js, GPT, APS tags, and other
client-side ad SDKs from the rendering path.

The POC should be measured against real publisher traffic with explicit
guardrails: fill rate, revenue impact, viewability, time to iframe insertion,
time to iframe load, creative paint/visible signal where available,
`DOMContentLoaded` impact, CLS, and error/no-fill rates.

---

## 2. Product Goals

Enable Trusted Server to:

1. Serve eligible page HTML without waiting for ad auctions or ad-server calls.
2. Start ad decisioning early through one browser-requested ad-bundle endpoint.
3. Run all configured demand sources in one batched edge-side auction.
4. Pass full auction evidence and slot context to Microsoft Monetize for final
   ad-server decisioning.
5. Render returned creatives into sandboxed iframes at pre-defined slot
   positions.
6. Preserve publisher revenue safety through a kill switch, rollout controls,
   and observable fallback behavior.

Target POC outcome:

- P75 above-the-fold iframe insertion under 1 second on cache-eligible pages.
- No material regression to `DOMContentLoaded`, CLS, or content rendering.
- No material revenue or fill regression versus the control cohort.
- Clear evidence that removing browser-side ad SDK work reduces ad render time.

---

## 3. Non-Goals

- Replacing the publisher's full ad stack in the first launch.
- Supporting mixed-mode pages where some slots are Monetize-rendered and others
  are GPT-rendered.
- Dynamic DOM slot discovery.
- Building Microsoft Monetize wire-format support before Microsoft confirms the
  endpoint, authentication model, request format, response format, and creative
  execution requirements.
- Guaranteeing that all publisher HTML can be cached by path alone.
- Guaranteeing creative viewability measurement unless Microsoft confirms the
  returned creatives are self-contained inside the iframe.

---

## 4. Dependency: Server-Side Ad Templates

This design depends on the server-side ad template work:

- `creative-opportunities.toml`
- URL pattern matching against the incoming page path
- `CreativeOpportunitySlot`
- conversion from template slots into auction `AdSlot` values

The Monetize path should reuse that slot definition layer, but it should not
reuse the ad-template spec's browser-facing `window.__ts_bids`,
`window.__ts_ad_slots`, or `__tsAdInit` model.

When `ad_server.provider = "microsoft_monetize"`, the ad-template output is:

- one ad-bundle script tag in `<head>`
- no browser-visible bid objects
- no GPT bootstrap for the matched slots

When the provider is absent or configured for the existing GAM path, the
ad-template behavior remains unchanged.

---

## 5. Reviewed Architecture

### 5.1 Two-Request Model

Request 1: page HTML

- Match request URL to configured creative opportunities.
- If Monetize is enabled and at least one slot matches, inject one ad-bundle
  script tag into `<head>`.
- Do not run an auction on the page request.
- Do not block `<head>`, `<body>`, or origin streaming on ad decisioning.
- Preserve compatibility with existing HTML rewriting and integration hooks.

Request 2: ad bundle script

- Validate the requested page and slot set.
- Build auction request context from the original browser request.
- Run all configured auction providers in one batched server-side auction.
- Send full auction evidence and slot context to Microsoft Monetize.
- Return JavaScript that injects sandboxed iframes into matching slot divs.

No KV store is required between requests.

However, the ad-bundle request must not trust arbitrary client-provided slot
IDs or page paths. It should either recompute slots from the page path on the
server or verify a stateless signed token emitted by the page response.

### 5.2 Script Loading Model

The reviewed design uses `async`, not `defer`:

```html
<script
  async
  src="/ts/ad-bundle?page=%2F2024%2Farticle%2F&slots=atf_sidebar_ad,below-content-ad&token=..."
></script>
```

Rationale:

- `async` starts the fetch early without blocking HTML parsing.
- `async` execution does not hold `DOMContentLoaded`.
- The ad-bundle code can wait until the target slot elements exist before
  injection.

The generated script must be safe to execute before or after the slot divs are
available. It should use a small helper:

- try `document.getElementById(slot_id)` immediately
- if missing and document is still loading, retry on `DOMContentLoaded`
- optionally use a short bounded `MutationObserver` for late-rendered slots
- never throw if a slot is absent

This is a material change from the original spec. The product goal is not just
fast ad rendering; it is fast ad rendering without delaying core page lifecycle
events.

### 5.3 Request Validation

The original design passes `slots` and `page` as query parameters. That is
convenient but insufficient as a trust boundary.

The endpoint must implement one of these validation models:

Preferred for POC:

1. Page request emits `page`, `slots`, and `token`.
2. `token` is an HMAC over:
   - publisher ID
   - request path
   - slot IDs
   - creative-opportunities config version
   - expiry timestamp
3. `/ts/ad-bundle` rejects requests with invalid, expired, or mismatched tokens.

Acceptable alternative:

1. `/ts/ad-bundle` accepts only `page`.
2. The endpoint recomputes matching slots from `creative-opportunities.toml`.
3. Any client-provided `slots` parameter is ignored or used only as a hint.

Do not allow arbitrary browsers to request arbitrary slot IDs against arbitrary
page contexts. That weakens auction integrity, reporting quality, and abuse
resistance.

### 5.4 Sequence

Cached or cache-eligible page:

```text
t=0ms     Browser requests page
t=5ms     Trusted Server serves eligible edge-cached or fast-streamed HTML
t=15ms    Browser parses <head> and starts /ts/ad-bundle async request
t=25ms    /ts/ad-bundle validates token and builds auction context
t=25ms    Batched PBS/APS auction starts
t=525ms   Auction deadline reached or all providers complete
t=525ms   Microsoft Monetize request starts with slot context and auction evidence
t=625ms   Monetize returns final creative markup
t=635ms   Ad bundle completes; browser injects sandboxed iframes
t=650ms   ATF iframe inserted; creative load/paint measured separately
```

Uncached page:

```text
t=0ms     Browser requests page
t=150ms   Origin HTML arrives or streams through Trusted Server
t=160ms   Browser parses <head> and starts /ts/ad-bundle async request
t=170ms   /ts/ad-bundle validates token and starts batched auction
t=670ms   Auction deadline reached or all providers complete
t=770ms   Monetize returns final creative markup
t=785ms   ATF iframe inserted; creative load/paint measured separately
```

These numbers are POC assumptions, not guarantees. The measurement plan must
separate:

- ad-bundle request start
- auction complete
- Monetize complete
- iframe inserted
- iframe load event
- creative visible or measurable event, if available

### 5.5 Streaming Script Response

Streaming the first bytes of the JavaScript response may still be useful for
transport behavior and observability, but it is not the primary user-visible
performance win because the browser cannot execute an external script until the
full script is received.

The endpoint may stream:

Phase 1:

```javascript
(function(){var c=
```

Phase 2:

```javascript
{"slot":{"m":"...","w":300,"h":250}};/* injection code */})();
```

But the spec must not claim that Phase 1 makes ads visible sooner. The real
performance value comes from:

- moving ad work out of the page request
- starting the ad-bundle fetch early
- running auction/ad-server calls from the edge
- eliminating large browser-side SDK load and parse work

Implementation note: the current Fastly adapter should be validated for true
client streaming before depending on chunked response behavior. If the adapter
still returns buffered `Response` values, the POC can still prove most of the
product value, but it should not claim immediate ad-bundle TTFB until
`stream_to_client()` support is implemented for this route.

---

## 6. Ad Decisioning Contract

### 6.1 Decisioning Principle

Microsoft Monetize should be treated as the final ad server decisioner, not
only as a creative wrapper around Trusted Server's pre-selected winning bids.

The original spec sends only `winning_bids` to Monetize. That may be too narrow.
It can prevent Monetize from correctly considering:

- direct-sold campaigns
- guaranteed and sponsorship priorities
- deal priority
- pacing
- frequency constraints
- APS encoded-price demand
- no-bid slots that Monetize can fill directly
- future auction/provider diagnostics

The reviewed design passes the full auction result and the slot list to the ad
server client. Monetize can then decide how much of that evidence it supports.

### 6.2 Reviewed `AdServerClient` Trait

```rust
/// Client for server-side ad-server decisioning.
///
/// Implementations receive the full auction result and slot context so the ad
/// server can perform final decisioning across direct-sold and programmatic
/// demand.
pub trait AdServerClient: Send + Sync {
    /// Request final ad creatives from the ad server.
    ///
    /// # Errors
    ///
    /// Returns [`AdServerError`] when the ad-server request fails, times out,
    /// or returns an unsupported payload.
    fn request_creatives(
        &self,
        auction: &OrchestrationResult,
        slots: &[CreativeOpportunitySlot],
        context: &AdServerRequestContext,
    ) -> Result<AdServerResponse, Report<AdServerError>>;
}

/// Context passed alongside auction evidence and slots.
pub struct AdServerRequestContext {
    /// Full page URL that triggered the ad-bundle request.
    pub page_url: String,
    /// Publisher identifier in Trusted Server configuration.
    pub publisher_id: String,
    /// Microsoft member, seat, or placement identifier as configured.
    pub monetize_member_id: String,
    /// Edge Cookie ID when consent permits use.
    pub ec_id: Option<String>,
    /// Request user-agent.
    pub user_agent: Option<String>,
    /// Client IP or platform-provided client address when consent and policy allow.
    pub ip: Option<String>,
    /// Referrer from the browser request.
    pub referrer: Option<String>,
    /// Consent strings and decoded consent context.
    pub consent: Option<ConsentContext>,
    /// Geo information from the platform lookup.
    pub geo: Option<GeoInfo>,
    /// Whether this request is part of a test cohort.
    pub test_mode: bool,
}
```

The exact wire format remains dependent on Microsoft documentation. If Microsoft
supports OpenRTB 2.6, `MicrosoftMonetizeClient` should map the context and
auction evidence into OpenRTB fields and extensions. If Microsoft expects
Prebid-style key-values, the implementation should perform that mapping behind
the trait.

### 6.3 Creative Response Shape

```rust
pub struct AdServerResponse {
    /// Creative results keyed by slot ID.
    pub creatives: HashMap<String, CreativeResult>,
    /// Total ad-server response time in milliseconds.
    pub response_time_ms: u64,
    /// Provider-specific diagnostics safe for logs and metrics.
    pub diagnostics: HashMap<String, serde_json::Value>,
}

pub struct CreativeResult {
    /// HTML creative markup to render inside the sandboxed iframe.
    pub markup: String,
    /// Creative width.
    pub width: u32,
    /// Creative height.
    pub height: u32,
    /// Impression tracking URLs if they are not already embedded in markup.
    pub impression_urls: Vec<String>,
    /// Click-through URL if it is separate from markup.
    pub click_url: Option<String>,
    /// Optional creative ID for logging and diagnostics.
    pub creative_id: Option<String>,
    /// Optional line item, campaign, or deal identifier.
    pub decision_id: Option<String>,
}
```

---

## 7. Creative Rendering and Isolation

### 7.1 Required Iframe Policy

Creatives must not be injected as raw HTML into the publisher DOM.

Creatives also must not be placed in an unrestricted `srcdoc` iframe. The
generated iframe must use an explicit sandbox policy.

Baseline POC policy:

```javascript
f.setAttribute(
  'sandbox',
  'allow-scripts allow-popups allow-popups-to-escape-sandbox allow-forms'
)
f.setAttribute('referrerpolicy', 'strict-origin-when-cross-origin')
f.srcdoc = c[id].m
```

Do not include `allow-same-origin` unless Microsoft confirms it is required and
Trusted Server serves the creative from a separate origin. `allow-scripts` plus
`allow-same-origin` on a same-origin `srcdoc` iframe can undermine the sandbox.

### 7.2 Sanitization and Rewriting

Before serializing creative markup into the ad-bundle script, Trusted Server
should apply the existing creative processing path where compatible:

- sanitize unsafe creative markup
- rewrite external resource URLs through the first-party proxy when required by
  publisher policy
- preserve known-good impression and click tracking behavior

If sanitization or URL rewriting breaks Microsoft creative behavior, the POC
must document the tradeoff explicitly and use a separate creative origin or
another containment strategy instead of unrestricted parent-origin `srcdoc`.

### 7.3 Layout Reservation

`creative-opportunities.toml` should include enough information to reserve slot
space before the ad arrives. At minimum:

- expected ATF width and height
- allowed responsive sizes
- collapse behavior for no-fill
- whether the slot is above-the-fold or lazy

The injector should set stable iframe dimensions from the ad-server response and
the publisher template should reserve the expected space to avoid layout shift.

---

## 8. `/ts/ad-bundle` Endpoint

### 8.1 Route

```text
GET /ts/ad-bundle?page={url_path}&slots={slot_ids}&token={signed_token}
```

For the recompute-only validation model:

```text
GET /ts/ad-bundle?page={url_path}&token={signed_token}
```

### 8.2 Behavior

1. Validate request method and query parameters.
2. Validate the stateless token or recompute slots from page path.
3. Look up matched `CreativeOpportunitySlot` records.
4. Build an `AuctionRequest` from slot config and browser request context.
5. Extract EC ID and consent using the same server-side path as `/auction`.
6. Run `AuctionOrchestrator::run_auction()` with a POC-specific timeout.
7. Call `AdServerClient::request_creatives()` with the full orchestration
   result, slots, and ad-server context.
8. Sanitize/rewrite creative markup according to publisher policy.
9. Serialize creative results into a generated JavaScript response.
10. The generated script injects sandboxed iframes when slots are available.

### 8.3 Error Behavior

- Invalid token: return a small no-op JavaScript response with 403 or 200,
  depending on monitoring preference. For POC observability, prefer 403.
- No matching slots: return a no-op JavaScript response.
- Auction timeout: call Monetize with the bids and provider responses available
  before timeout.
- Monetize timeout: return a no-op JavaScript response and log the failure.
- Creative parse/sanitization failure: omit that creative, keep other slots.
- Slot not found in DOM: no-op for that slot.
- Script fails to load: page renders without ads.

### 8.4 Response Headers

```text
Content-Type: application/javascript; charset=utf-8
Cache-Control: no-store
X-Content-Type-Options: nosniff
```

Use `Server-Timing` diagnostics where available:

```text
Server-Timing: auction;dur=500, monetize;dur=100, serialize;dur=2
```

If true streaming is implemented:

```text
Transfer-Encoding: chunked
```

Do not set `Transfer-Encoding` manually unless the platform requires it. Prefer
the platform streaming API to manage transfer details.

### 8.5 Generated JavaScript Shape

The generated JavaScript should be compact but explicit:

```javascript
;(function () {
  var c = CREATIVE_JSON
  function render() {
    Object.keys(c).forEach(function (id) {
      var r = c[id]
      var el = document.getElementById(id)
      if (!el || el.getAttribute('data-ts-ad-rendered') === '1') return
      var f = document.createElement('iframe')
      f.width = String(r.w)
      f.height = String(r.h)
      f.style.border = '0'
      f.scrolling = 'no'
      f.setAttribute(
        'sandbox',
        'allow-scripts allow-popups allow-popups-to-escape-sandbox allow-forms'
      )
      f.setAttribute('referrerpolicy', 'strict-origin-when-cross-origin')
      f.srcdoc = r.m
      el.setAttribute('data-ts-ad-rendered', '1')
      el.appendChild(f)
    })
  }
  render()
  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', render, { once: true })
  }
})()
```

For late-rendered slots, the POC may add a bounded `MutationObserver`. Keep the
observer scoped and time-limited to avoid a permanent page-wide observer.

---

## 9. HTML Injection

The HTML processor should inject the ad-bundle tag only when all are true:

- Monetize ad-server path is enabled.
- The page URL matches one or more creative opportunity slots.
- The request is eligible for this POC cohort.
- The existing page is not excluded by route, auth state, or cache policy.

The injected tag should be placed early in `<head>`:

```html
<script async src="/ts/ad-bundle?page=...&slots=...&token=..."></script>
```

If other non-ad Trusted Server integrations are enabled, the normal `tsjs` bundle
can still be injected. The Monetize ad rendering path must not depend on `tsjs`.

For implementation in the current repo, this likely fits best as a dedicated
head injector or a small extension to `html_processor.rs`, but the slot-matching
context must be available to the HTML processor. Avoid spreading ad-server
state across unrelated integration modules.

---

## 10. Cache Strategy

The reviewed position: page HTML is cache-compatible, not automatically
cacheable by path.

Cache eligibility must account for:

- publisher route type
- query string behavior
- auth/paywall state
- cookies that personalize page content
- consent/CMP state
- geo/device variants
- A/B testing
- preview/editor routes
- page content freshness
- `creative-opportunities.toml` version

Recommended POC cache policy:

1. Start with a narrow allowlist of article routes.
2. Exclude logged-in, paywalled, preview, admin, checkout, and personalized
   pages.
3. Include query strings in the cache key unless the publisher confirms they do
   not vary content.
4. Vary only on the smallest necessary set of headers/cookies.
5. Purge HTML when publisher content changes or when creative opportunity config
   changes.
6. Keep `/ts/ad-bundle` `no-store` for every page view.

The page response should not be the only place EC cookies are established,
because cached HTML may bypass per-user origin-like response behavior. The
ad-bundle endpoint and existing Trusted Server routes can still set EC headers
and cookies when consent permits.

---

## 11. Configuration

```toml
[ad_server]
provider = "microsoft_monetize"
enabled = true
timeout_ms = 150
auction_timeout_ms = 500
rollout_percent = 5
kill_switch = false

[ad_server.microsoft_monetize]
endpoint = "https://..."
member_id = "..."
auth_mode = "tbd"
test_mode = true

[creative_opportunities]
path = "creative-opportunities.toml"
config_version = "2026-04-18-poc-1"
```

Open items for implementation:

- whether `creative_opportunities` belongs in `[ad_server]`, `[auction]`, or a
  top-level section
- whether `auction_timeout_ms` should override `settings.auction.timeout_ms` only
  for ad-bundle requests
- how rollout cohorts are assigned and logged
- whether token signing uses the existing request-signing infrastructure or a
  separate HMAC secret

---

## 12. POC Guardrails

The POC must have:

- global kill switch
- rollout percentage
- publisher route allowlist
- no-fill-safe behavior
- timeout budgets for auction and Monetize separately
- error logging with sampled diagnostics
- explicit control cohort
- daily revenue/fill monitoring
- fast rollback to existing GAM/GPT path

Recommended default budget:

```text
auction_timeout_ms = 500
monetize_timeout_ms = 150
total_ad_bundle_budget_ms = 700
```

If the ad bundle exceeds the total budget, return no-fill JavaScript rather than
letting connections hang.

---

## 13. Measurement Plan

Server-side metrics:

- ad-bundle requests
- token validation failures
- matched slot count
- auction provider timings
- auction bid count
- winning bid count
- Monetize request timing
- Monetize fill count
- creative sanitization/rewrite failures
- no-fill count by reason
- generated script byte size

Client-side marks emitted by generated script:

- `ts-ad-bundle-exec`
- `ts-ad-render-start`
- `ts-ad-iframe-inserted:{slot_id}`
- `ts-ad-iframe-load:{slot_id}` where browser permits
- `ts-ad-slot-missing:{slot_id}` sampled or aggregated

Browser/page metrics:

- `DOMContentLoaded`
- LCP
- CLS
- INP where available
- ad iframe insertion time
- ad iframe load time
- viewability signal where available from Microsoft/verification vendors

Business metrics:

- fill rate
- CPM/RPM
- viewability
- click-through rate
- revenue per session
- ad-blocker interaction if measurable
- discrepancy versus Microsoft reporting

Success criteria should be set before live traffic. Suggested POC success:

- P75 ATF iframe insertion under 1 second on eligible pages
- P75 iframe load materially faster than control
- no statistically meaningful regression in revenue per eligible page view
- no material CLS regression
- no material `DOMContentLoaded` regression

---

## 14. Edge Cases

No matching slots:

- no ad-bundle tag is injected, or the endpoint returns no-op JavaScript.

Slot exists in config but not DOM:

- injector no-ops for that slot and emits sampled diagnostic.

Auction returns no bids:

- call Monetize with empty/zero programmatic evidence for those slots so direct
  sold demand can still fill, if Microsoft supports that model.

APS returns encoded prices:

- pass APS evidence through to Monetize if Microsoft can interpret it, or require
  configured mediation before APS participates in this POC.

Monetize returns creative requiring parent-page SDK:

- reject for the initial POC or add the companion SDK explicitly to scope. Do
  not silently claim SDK-free rendering if a parent SDK is required.

Creative requires same-origin iframe access:

- do not use unrestricted `srcdoc`. Use a separate creative origin or decline
  that creative class for the POC.

JavaScript disabled:

- no ads render. This is equivalent to most JavaScript-based ad stacks.

Ad blocker blocks `/ts/ad-bundle`:

- page renders without ads. Consider route naming if blockers target obvious ad
  paths, but do not obscure behavior in a way that violates publisher or user
  expectations.

---

## 15. Implementation Scope

### New

- `crates/trusted-server-core/src/ad_server/mod.rs`
- `crates/trusted-server-core/src/ad_server/config.rs`
- `crates/trusted-server-core/src/ad_server/endpoints.rs`
- `crates/trusted-server-core/src/ad_server/microsoft_monetize.rs`
- `crates/trusted-server-core/src/creative_opportunities.rs`
- token signing/validation helper for ad-bundle URLs
- tests for generated JavaScript escaping and sandbox attributes

### Modified

- `crates/trusted-server-core/src/settings.rs`
- `crates/trusted-server-core/src/html_processor.rs`
- `crates/trusted-server-adapter-fastly/src/main.rs`
- `trusted-server.toml`
- docs and POC runbook

### Validate Before Depending On

- true Fastly response streaming for `/ts/ad-bundle`
- interaction with current `#[fastly::main]` return-based adapter model
- whether creative sanitization/rewrite is compatible with Microsoft creatives
- whether Microsoft supports server-side OpenRTB decisioning with the required
  direct-sold and programmatic semantics

---

## 16. Open Questions for Microsoft

1. What exact server-side endpoint should Trusted Server call?
2. Is the endpoint standard OpenRTB 2.6, Microsoft-specific OpenRTB extensions,
   or a proprietary request format?
3. What authentication model is required?
4. Can Monetize perform final decisioning with upstream auction evidence from
   PBS and APS?
5. Should Trusted Server send all bids, winning bids only, or ad-server
   key-values?
6. How should APS encoded-price bids be represented?
7. Are returned banner creatives fully self-contained inside an iframe?
8. Do creatives require a parent-page SDK, SafeFrame, or Microsoft JavaScript
   library?
9. Can creatives run inside a sandboxed iframe without `allow-same-origin`?
10. Are impression, click, and viewability trackers embedded in `adm`, or
    returned separately?
11. What reporting identifiers should Trusted Server persist in logs?
12. Does Microsoft require win-notification, billing-notification, or render
    notification calls from Trusted Server?

---

## 17. Product Recommendation

Proceed with the POC, but position it as an ATF display speed experiment rather
than a complete ad stack replacement.

The highest-confidence parts of the concept are:

- edge-side batched auction
- early ad-bundle request
- removing browser-side SDK load/parse
- server-side consent and identity handling
- single publisher, narrow route allowlist

The main risks to retire before live traffic are:

- Microsoft API and creative execution requirements
- iframe sandbox compatibility
- ad-bundle request validation
- cache eligibility
- real creative load/viewability timing
- revenue and fill impact

The original direction is strong. The POC becomes much more defensible when the
claims shift from "full isolation and fully cacheable HTML" to "validated,
cache-compatible, SDK-free server-side ad rendering with sandboxed creative
delivery and explicit measurement."
