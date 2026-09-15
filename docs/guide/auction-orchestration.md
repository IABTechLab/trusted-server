# Auction Orchestration

Learn how Trusted Server coordinates multiple demand sources in parallel to maximize revenue and minimize latency.

## Overview

The auction orchestrator is the core system that manages server-side ad auctions. It launches bid requests to multiple demand providers simultaneously, collects responses, and selects winners.

Key capabilities:

- **Parallel execution** — Bid requests to all providers launch concurrently using Fastly's `select()` API
- **Strategy-based winner selection** — Automatic strategy detection based on configuration
- **Ad server support**. An optional external ad server makes the final winner selection and applies unified floor pricing
- **Provider abstraction** — Pluggable provider interface for adding new demand sources
- **Creative processing** — Winning creatives are rewritten to first-party proxy URLs by default, with opt-in sanitization

## System Flow (Prebid + APS)

The following diagram shows the full auction flow when both a Prebid Server and an APS demand source are configured with an ad server:

```mermaid
%%{init: {
  "theme": "base",
  "themeVariables": {
    "background": "#ffffff",
    "primaryColor": "#dbeafe",
    "primaryTextColor": "#1e3a8a",
    "primaryBorderColor": "#2563eb",
    "lineColor": "#334155",
    "secondaryColor": "#fef3c7",
    "tertiaryColor": "#d1fae5",
    "actorBkg": "#eff6ff",
    "actorBorderColor": "#3b82f6",
    "actorTextColor": "#1e40af",
    "actorLineColor": "#64748b",
    "signalColor": "#1e293b",
    "signalTextColor": "#0f172a",
    "labelBoxBkgColor": "#f1f5f9",
    "labelBoxBorderColor": "#cbd5e1",
    "labelTextColor": "#1e293b",
    "loopTextColor": "#1e293b",
    "noteBkgColor": "#fef3c7",
    "noteBorderColor": "#d97706",
    "noteTextColor": "#78350f",
    "activationBorderColor": "#059669",
    "activationBkgColor": "#d1fae5",
    "sequenceNumberColor": "#0f172a"
  },
  "themeCSS": ".sequenceNumber{font-size:26px!important;font-weight:900!important;fill:#ffffff!important;paint-order:stroke fill;stroke:#1e293b;stroke-width:1px;} .sequenceNumber circle{r:32px!important;stroke-width:3px!important;stroke:#1e293b!important;fill:#2563eb!important;} .mermaid svg{background:#ffffff!important;border-radius:8px;box-shadow:0 2px 4px rgba(0,0,0,0.06);} .actor{font-weight:600!important;} .messageText{font-weight:600!important;font-size:16px!important;} .activation0{stroke-width:3px!important;} .messageLine0,.messageLine1{stroke-width:3px!important;} .messageText tspan{font-size:16px!important;} path.messageLine0,path.messageLine1{stroke-width:3px!important;} marker#arrowhead path,marker#crosshead path{stroke-width:2px!important;}"
}}%%
sequenceDiagram
  autonumber

  participant Client as Browser/TSJS
  participant TS as Trusted Server
  participant Orch as Orchestrator
  participant APS as APS Provider
  participant Prebid as Prebid Provider
  participant Med as Ad Server
  participant Mock as Mocktioneer

  %% === Auction Request Initiation ===
  rect rgb(243,244,246)
    Note over Client,Mock: Auction Request Initiation
    activate Client
    activate TS
    Client->>TS: POST /auction<br/>AdRequest with adUnits[]
    Note right of Client: { "adUnits": [{ "code": "header-banner",<br/>  "mediaTypes": { "banner": { "sizes": [[728,90]] } } }] }

    TS->>TS: Parse AdRequest<br/>Transform to AuctionRequest<br/>Generate user IDs<br/>Build context
    deactivate Client
    deactivate TS
  end

  %% === Orchestrator Strategy Detection ===
  rect rgb(239,246,255)
    Note over Client,Mock: Auction Strategy Detection
    activate TS
    activate Orch
    TS->>Orch: orchestrator.run_auction()
    Orch->>Orch: Detect strategy<br/>ad server? parallel_adserver : parallel_only
    deactivate TS

    Note over Orch: Strategy determined by config:<br/>[adserver]<br/>provider = "adserver_mock" → parallel_adserver<br/>No [adserver] → parallel_only
  end

  %% === Parallel Provider Execution ===
  rect rgb(243,232,255)
    Note over Client,Mock: Parallel Provider Execution
    activate APS
    activate Prebid
    activate Mock

    par Parallel Provider Calls
      Orch->>APS: POST /e/pb/bid<br/>APS OpenRTB
      Note right of Orch: { "id": "request",<br/>  "imp": [{ "id": "header-banner",<br/>    "banner": { "w": 728, "h": 90 } }],<br/>  "ext": { "account": "example-account" } }

      APS->>Mock: APS OpenRTB request
      Mock-->>APS: OpenRTB bid response<br/>(decoded price and renderer URL)
      Note right of Mock: { "seatbid": [{ "bid": [{<br/>  "impid": "header-banner", "price": 2.50,<br/>  "ext": { "creativeurl": "https://creative.example/render",<br/>    "tagtype": "iframe" } }] }] }

      APS-->>Orch: AuctionResponse<br/>(decoded price and typed renderer)
    and
      Orch->>Prebid: POST /openrtb2/auction<br/>OpenRTB 2.x format
      Note right of Orch: { "id": "request",<br/>  "imp": [{ "id": "header-banner",<br/>    "banner": { "w": 728, "h": 90 } }] }

      Prebid->>Mock: OpenRTB request
      Mock-->>Prebid: OpenRTB response<br/>(decoded price with creative)
      Note right of Mock: { "seatbid": [{ "seat": "prebid",<br/>  "bid": [{ "price": 2.00, "adm": "<html>..." }] }] }

      Prebid-->>Orch: AuctionResponse<br/>(Prebid bids)
    end

    Note over Orch: Collected decoded-price bids<br/>APS: typed renderer, no adm<br/>Prebid: sanitized creative or cache source
    deactivate Mock
    deactivate APS
    deactivate Prebid
  end

  %% === Winner Selection Strategy ===
  alt Ad Server Configured (parallel_adserver)
    rect rgb(236,253,245)
      Note over Client,Mock: Ad Server Flow
      activate Med
      Orch->>Med: POST the ad server endpoint<br/>Decoded-price bids for final selection
      Note right of Orch: APS price: 2.50<br/>Prebid price: 2.00

      Med->>Med: Apply ad server policy and floors<br/>Select highest CPM per slot
      Med-->>Orch: OpenRTB response with winners
      Note right of Med: APS renderer state is restored from<br/>the reduced source bid after the ad server answers
      deactivate Med
    end
  else No Ad Server (parallel_only)
    rect rgb(253,243,235)
      Note over Client,Mock: Direct Winner Selection
      Orch->>Orch: Compare decoded prices<br/>Apply slot floor<br/>Select highest CPM
      Note right of Orch: Winner: APS at $2.50 vs Prebid at $2.00
    end
  end

  %% === Response Assembly ===
  rect rgb(243,244,246)
    Note over Client,Mock: Response Assembly
    activate TS
    activate Client
    Orch->>Orch: Transform to OpenRTB response<br/>Preserve typed render source<br/>Optionally sanitize creative HTML<br/>Optionally rewrite creative URLs<br/>Add orchestrator metadata

    Orch-->>TS: OpenRTB BidResponse
    Note right of Orch: APS winner carries ext.trusted_server.renderer<br/>with no adm; ordinary winners retain sanitized adm/cache data

    TS-->>Client: 200 OpenRTB response<br/>with winning render capability
    deactivate Orch
    deactivate TS
  end

  %% === Creative Rendering ===
  rect rgb(239,246,255)
    Note over Client,Mock: Creative Rendering
    alt APS winner
      Client->>Client: Validate renderer descriptor<br/>Create opaque sandbox iframe<br/>Load /integrations/aps/renderer
      Note right of Client: Fragment-bound nonce and one-time acknowledgement<br/>No allow-same-origin on the outer frame
    else Ordinary creative
      Client->>Client: Inject winning creative<br/>Render iframe<br/>Load creative resources
      Note right of Client: Default: first-party proxy/click URLs<br/>rewrite_creatives=false: accepted external URLs remain direct
    end
    deactivate Client
  end
```

## Architecture

### Request Flow

The auction system processes requests through a pipeline of transformations:

```
POST /auction (AdRequest in Prebid.js format)
  │
  ├─ Parse body → AdRequest { adUnits[] }
  ├─ Generate EC + fresh user IDs
  ├─ Convert adUnits → AdSlots with formats and bidder params
  ├─ Extract device info (User-Agent, geo)
  │
  ▼
AuctionOrchestrator.run_auction()
  │
  ├─ Detect strategy (parallel_only or parallel_adserver)
  ├─ Launch all providers in parallel via select()
  ├─ Collect responses as they complete
  │
  ├─[parallel_only]─── Select highest decoded CPM per slot
  └─[parallel_adserver]─── Forward decoded-price bids to the ad server for final selection
  │
  ▼
Convert OrchestrationResult → OpenRTB 2.x Response
  │
  ├─[sanitize_creatives=true] Strip executable markup
  ├─[rewrite_creatives=true] Rewrite URLs and inject creative TSJS
  ├─ Add ext.orchestrator metadata
  └─ Set consent and optional EID response headers
```

### Key Components

The orchestrator is composed of several modules:

| Module            | Path                                      | Purpose                                      |
| ----------------- | ----------------------------------------- | -------------------------------------------- |
| `orchestrator.rs` | `crates/trusted-server-core/src/auction/` | Parallel execution and bid selection         |
| `plan.rs`         | `crates/trusted-server-core/src/auction/` | Plan compilation and validation              |
| `demand.rs`       | `crates/trusted-server-core/src/auction/` | The demand and ad server implementation seam |
| `routing.rs`      | `crates/trusted-server-core/src/auction/` | Bidder ownership and demand routing          |
| `openrtb.rs`      | `crates/trusted-server-core/src/auction/` | Shared OpenRTB request and response handling |
| `provider.rs`     | `crates/trusted-server-core/src/auction/` | `AuctionProvider` trait and planned provider |
| `telemetry.rs`    | `crates/trusted-server-core/src/auction/` | Auction event construction                   |
| `types.rs`        | `crates/trusted-server-core/src/auction/` | Auction request, response, and bid types     |
| `formats.rs`      | `crates/trusted-server-core/src/auction/` | TSJS and OpenRTB format conversions          |
| `endpoints.rs`    | `crates/trusted-server-core/src/auction/` | HTTP handler for `POST /auction`             |
| `config.rs`       | `crates/trusted-server-core/src/auction/` | Auction configuration types                  |

### Configuration-first plan

At startup, Trusted Server compiles `[demand]`, `[adserver]` and
`[auction.bidders]` through one registry into an immutable `AuctionPlan`.
Demand source names, endpoints, implementation defaults, routes, static
extensions and notification policy are resolved once. The same
`Arc<AuctionPlan>` is shared by the orchestrator and the integration registry,
so request handling never reinterprets raw configuration.

Three demand implementations ship in this repository:

- `openrtb` for the common banner subset and bounded static extensions;
- `prebid_server` for PBS request, response, cache, override, and diagnostics
  behavior; and
- `aps` for APS account/SDK fields, response eligibility, and renderer output.

Each `[demand.<name>]` table is one instance of the shared OpenRTB path.
Several tables may name the same implementation or endpoint and stay distinct
through their names, which is what the optional `implementation` line is for.
The `adserver_mock` ad server is selected the same way, by
`[adserver] provider`, and supplies an ad server implementation rather than a
demand one. See [Configuration Rules](/guide/configuration-rules) for the
syntax every provider type shares.

## Auction Strategies

The orchestrator automatically selects a strategy based on whether an ad server is configured.

### Parallel Only

With no ad server, the orchestrator runs all demand sources in parallel and selects winners by comparing decoded prices directly. This is the simplest strategy.

```toml
[auction]
enabled = true
timeout_ms = 2000

[demand]
provider = ["pbs_main", "aps_main"]

[demand.pbs_main]
implementation = "prebid_server"
endpoint = "https://prebid.example.com/openrtb2/auction"
routing = "explicit"

[demand.aps_main]
implementation = "aps"
endpoint = "https://aps.example.com/e/pb/bid"
routing = "all_eligible"
account_id = "example-aps-account"

[auction.bidders.example-server]
provider = "pbs_main"

# No [adserver], so the highest bid wins
```

**How winner selection works:**

1. Collect bids from all demand sources.
2. Group bids by slot ID.
3. Skip bids without a decoded numeric price.
4. Select the highest CPM for each slot.
5. Apply floor prices and drop winners below the slot's floor.

APS OpenRTB supplies decoded prices, so eligible APS bids participate directly without needing an ad server.

### Parallel Ad Server

When an ad server is configured, demand responses are forwarded to it for final winner selection and unified floor pricing.

```toml
[auction]
enabled = true
timeout_ms = 2000

[demand]
provider = ["pbs_main", "aps_main"]

[demand.pbs_main]
implementation = "prebid_server"
endpoint = "https://prebid.example.com/openrtb2/auction"
routing = "explicit"

[demand.aps_main]
implementation = "aps"
endpoint = "https://aps.example.com/e/pb/bid"
routing = "all_eligible"
account_id = "example-aps-account"

[auction.bidders.example-server]
provider = "pbs_main"

[adserver]
provider = "adserver_mock"

[adserver.adserver_mock]
endpoint = "https://adserver.example.com/decide"
timeout_ms = 500
```

**How the ad server strategy works:**

1. Run all demand sources in parallel (same as parallel_only).
2. Collect all responses.
3. Forward bids with decoded numeric prices to the ad server.
4. Let the ad server apply policy and choose a winner.
5. Restore render/accounting state from the selected source bid.
6. Filter any ad server winner without a decoded price.

An ad server is optional for APS. APS reduces to one candidate per impression first, so the selected renderer can be restored without same-slot ambiguity.

## Providers

### Provider Interface

Demand sources implement the async, platform-neutral
[`AuctionProvider`](https://github.com/IABTechLab/trusted-server/blob/main/crates/trusted-server-core/src/auction/provider.rs).
The trait receives an `AuctionRequest` and `AuctionContext`, launches a request
as a `ProviderRequestOutcome`, and parses a `PlatformResponse` into an
`AuctionResponse`. It also supplies capability, timeout, enablement, and
platform-backend metadata. Providers that need request-local response state use
the context-aware parsing hooks instead of storing mutable state on the shared
provider instance.

The orchestrator launches every request before collecting pending responses, so
providers can run concurrently without depending on a Fastly-specific API.

### Prebid Provider

Transforms auction requests into OpenRTB 2.x format and sends them to a Prebid Server instance.

**Request transformation:**

- `AdSlot` → `Imp` with `Banner { format: [Format { w, h }] }`
- Bidder params from slot config → `ext.prebid.bidder` map
- EC and fresh user IDs injected into `User` object
- Device info, geo data, and GPC signals included
- Optional Ed25519 request signing (see [Request Signing](/guide/request-signing))

**Response parsing:**

- Bids include decoded `price` as a decimal CPM.
- Missing bid dimensions inherit the routed impression size only when that
  impression has one banner format. Ambiguous or mismatched dimensions are
  rejected.
- Creative HTML comes from the `adm` field.
- Winning creative URLs are rewritten to first-party proxy format by default
  when the `/auction` response is assembled.
- Per-bidder timing (`responsetimemillis`), errors, and warnings are attached as
  response metadata.
- `response_admission` reports bounded rejected-bid and reason counts without
  retaining raw bid payloads.
- When `debug` is enabled, PBS debug payload and per-bid status (`bidstatus`) are
  also included.

```toml
[demand]
provider = ["pbs_main"]

[demand.pbs_main]
implementation = "prebid_server"
endpoint = "https://prebid.example.com/openrtb2/auction"
routing = "explicit"
debug = false

[auction.bidders.example-server]
provider = "pbs_main"
```

### APS Provider

Builds an independent banner OpenRTB request for Amazon Publisher Services.

**Request transformation:**

- banner `AdSlot` formats become secure OpenRTB impressions;
- `ext.account` uses canonical `account_id`;
- `ext.sdk` identifies the compatible Prebid contract; and
- existing page, device, consent, identity, and geo privacy gates are preserved.

**Response parsing:**

- decoded USD prices compete directly with other providers;
- positive compatible dimensions and an HTTPS `creativeurl` are required;
- script creatives are rejected before winner selection unless explicitly enabled;
- one candidate per impression is retained deterministically; and
- a minimized typed renderer is preserved instead of creative markup or APS notifications.

```toml
[demand]
provider = ["aps_main"]

[demand.aps_main]
implementation = "aps"
endpoint = "https://aps.example.com/e/pb/bid"
routing = "all_eligible"
account_id = "example-aps-account"
debug = false
allow_script_creatives = false
```

See [APS OpenRTB Integration](/guide/integrations/aps) for rollout and rendering requirements.

### AdServer Mock

An external decision service that receives decoded-price demand responses and performs final winner selection. APS prices are already decoded at the demand boundary.

**Ad server request format:**

```json
{
  "id": "auction-123",
  "imp": [
    { "id": "header-banner", "banner": { "format": [{ "w": 728, "h": 90 }] } }
  ],
  "ext": {
    "bidder_responses": [
      {
        "bidder": "aps",
        "bids": [{ "imp_id": "header-banner", "price": 2.5, "adm": null }]
      },
      {
        "bidder": "prebid",
        "bids": [
          { "imp_id": "header-banner", "price": 2.0, "adm": "<html>..." }
        ]
      }
    ],
    "config": { "price_floor": 0.5 }
  }
}
```

**Ad server response:** Standard OpenRTB with decoded prices and selected winners.

```toml
[adserver]
provider = "adserver_mock"

[adserver.adserver_mock]
endpoint = "https://adserver.example.com/decide"
timeout_ms = 500
price_floor = 0.50
```

## Data Structures

### AuctionRequest

The internal representation of an auction, converted from the incoming `AdRequest`:

```rust
pub struct AuctionRequest {
    pub id: String,                                    // UUID
    pub slots: Vec<AdSlot>,                            // Ad placements
    pub publisher: PublisherInfo,                       // Domain, page URL
    pub user: UserInfo,                                // EC ID, fresh ID, consent
    pub device: Option<DeviceInfo>,                    // UA, IP, geo
    pub site: Option<SiteInfo>,                        // Domain, page
    pub context: HashMap<String, serde_json::Value>,   // Additional metadata
}
```

### AdSlot

Represents a single ad placement on the page:

```rust
pub struct AdSlot {
    pub id: String,
    pub formats: Vec<AdFormat>,                         // Supported sizes
    pub floor_price: Option<f64>,                       // Minimum CPM
    pub targeting: HashMap<String, serde_json::Value>,  // Key-value targeting
    pub bidders: HashMap<String, serde_json::Value>,    // Per-bidder params
}
```

### Bid

The unified bid format used across all providers:

```rust
pub struct Bid {
    pub slot_id: String,
    pub price: Option<f64>,           // Missing prices fail closed
    pub currency: String,
    pub creative: Option<String>,     // APS uses renderer instead of markup
    pub adomain: Option<Vec<String>>,
    pub bidder: String,
    pub width: u32,
    pub height: u32,
    pub nurl: Option<String>,         // Win notification URL
    pub burl: Option<String>,         // Billing URL
    pub renderer: Option<BidRenderer>,
    pub metadata: HashMap<String, serde_json::Value>,
}
```

The `price` field remains optional so missing-price bids fail closed. APS supplies a decoded price and a typed renderer instead of creative HTML, and the renderer is retained through direct winner selection and through the ad server.

### OrchestrationResult

The complete result of an auction:

```rust
pub struct OrchestrationResult {
    pub provider_responses: Vec<AuctionResponse>,       // All provider results
    pub adserver_response: Option<AuctionResponse>,     // Ad server result (if used)
    pub winning_bids: HashMap<String, Bid>,             // Slot ID → winning bid
    pub total_time_ms: u64,
    pub metadata: HashMap<String, serde_json::Value>,
}
```

## Input and Output Formats

### Request Format (TSJS / Prebid.js)

The `POST /auction` endpoint accepts a Prebid.js-compatible `AdRequest`:

```json
{
  "adUnits": [
    {
      "code": "header-banner",
      "mediaTypes": {
        "banner": {
          "sizes": [
            [728, 90],
            [970, 250]
          ]
        }
      },
      "bids": [
        {
          "bidder": "appnexus",
          "params": { "placementId": 12345 }
        }
      ]
    }
  ]
}
```

### Response Format (OpenRTB 2.x)

Auction results are returned in standard OpenRTB format with an `ext.orchestrator` metadata block:

```json
{
  "id": "auction-abc123",
  "seatbid": [
    {
      "seat": "prebid",
      "bid": [
        {
          "id": "bid-1",
          "impid": "header-banner",
          "price": 2.5,
          "adm": "<iframe src=\"/first-party/proxy?tsurl=...&tstoken=sig\">...</iframe>",
          "w": 728,
          "h": 90
        }
      ]
    }
  ],
  "ext": {
    "orchestrator": {
      "strategy": "parallel_adserver",
      "providers": 2,
      "total_bids": 3,
      "time_ms": 145
    }
  }
}
```

APS renderer winners use the same OpenRTB response with a typed renderer extension instead of `adm`:

```json
{
  "id": "auction-abc123",
  "seatbid": [
    {
      "seat": "aps",
      "bid": [
        {
          "id": "upstream-aps-bid-id",
          "impid": "header-banner",
          "price": 2.5,
          "w": 728,
          "h": 90,
          "ext": {
            "trusted_server": {
              "renderer": {
                "type": "aps",
                "version": 1,
                "accountId": "example-account",
                "bidId": "upstream-aps-bid-id",
                "tagType": "iframe",
                "creativeUrl": "https://creative.example/render",
                "aaxResponse": "fictional-base64-envelope",
                "width": 728,
                "height": 90
              }
            }
          }
        }
      ]
    }
  ]
}
```

For these bids, `id` preserves APS's upstream bid ID, `crid` is present only when APS supplies one, and `adm` is absent. TSJS understands this contract; other `/auction` consumers must render `ext.trusted_server.renderer` explicitly.

EC identity is maintained with the `ts-ec` cookie; auction responses do not emit EC ID headers.

## Creative Processing

Winning creatives returned by `POST /auction` pass through two independent
transforms. `sanitize_creatives` (opt-in, default `false`) strips executable
markup with its inner content. `rewrite_creatives` (default `true`) runs an
HTML rewriter (`lol_html`) that converts eligible external resource and click
URLs to signed first-party paths, adds `data-tsclick`, rewrites inline CSS
`url(...)` values, removes bidder-supplied `<base>` elements, and injects the
unified creative TSJS runtime exactly once, whether or not the bidder supplied a
`<body>` element. In every mode, a creative
larger than the 1 MiB per-creative cap is rejected and its `adm` is dropped.

```toml
[auction]
sanitize_creatives = false
rewrite_creatives = true
```

| `sanitize_creatives` | `rewrite_creatives` | Winning-bid `adm` behavior                                                                                                              |
| -------------------- | ------------------- | --------------------------------------------------------------------------------------------------------------------------------------- |
| `false` (default)    | `false`             | Deliver the creative exactly as the bidder returned it (subject to the size cap).                                                       |
| `true`               | `false`             | Strip executable markup, then deliver without rewriting. Accepted asset and click URLs remain direct.                                   |
| `false`              | `true` (default)    | Rewrite eligible URLs, add click-guard attributes, and inject creative TSJS into the raw bidder markup. Executable markup is preserved. |
| `true`               | `true`              | Sanitize first, then rewrite eligible URLs, add click-guard attributes, and inject creative TSJS.                                       |

When sanitization is enabled, scripts, stylesheets, style blocks, forms, event
handlers, dangerous URL schemes, and other rejected content are removed together
with their inner content — which blanks script-based creatives. Disabling
rewriting removes the injected creative runtime and the first-party proxy and
click rewriting from the resulting `adm`, so the browser may contact
third-party hosts directly. Sanitizer-accepted hosts are not allowlisted or trusted
merely because their URLs remain in the output.

Both settings apply to winning-bid `adm` in both the shared `POST /auction`
response converter and the production publisher SSAT/page-bids path. The former
emits root-relative first-party URLs and injects creative TSJS; the latter emits
absolute first-party URLs for its foreign-origin renderer and does not inject
that bundle. HTML/CSS returned by `/first-party/proxy` continues to be
rewritten independently. `[debug].inject_adm_for_testing` adds the diagnostic
`debug_bid` blob and enables a testing-only direct GAM replacement; it does not
control whether processed `adm` is delivered.

**Elements handled by the rewrite pass:**

| Element                          | Attributes                  | Target                         |
| -------------------------------- | --------------------------- | ------------------------------ |
| `<img>`                          | `src`, `data-src`, `srcset` | `/first-party/proxy?tsurl=...` |
| `<script>`                       | `src`                       | `/first-party/proxy?tsurl=...` |
| `<link>`                         | `href`, `imagesrcset`       | `/first-party/proxy?tsurl=...` |
| `<iframe>`                       | `src`                       | `/first-party/proxy?tsurl=...` |
| `<video>`, `<audio>`, `<source>` | `src`                       | `/first-party/proxy?tsurl=...` |
| `<a>`, `<area>`                  | `href`                      | `/first-party/click?tsurl=...` |
| `<style>`, `[style]`             | `url()` references          | `/first-party/proxy?tsurl=...` |
| SVG `<image>`, `<use>`           | `href`, `xlink:href`        | `/first-party/proxy?tsurl=...` |

The rewrite pass leaves relative URLs and non-network schemes unchanged. When
`sanitize_creatives` is also enabled, sanitization runs first and strips
dangerous schemes, so only sanitizer-accepted values reach this pass; with
sanitization disabled, the rewriter operates on the raw bidder markup. Domains
in the `rewrite.exclude_domains` config list (supports wildcards like
`*.cdn.example.com`) are also skipped.

Each proxied URL includes a `tstoken` HMAC signature for tamper protection. See [Proxy Signing](/guide/proxy-signing) for details.

## Configuration

### Full example

```toml
[auction]
enabled = true
sanitize_creatives = false     # Opt-in, blanks script-based creatives when enabled
rewrite_creatives = true
timeout_ms = 2000

[demand]
provider = ["pbs_main", "aps_main"]

[demand.pbs_main]
implementation = "prebid_server"
endpoint = "https://prebid.example.com/openrtb2/auction"
timeout_ms = 900
routing = "explicit"
debug = false
test_mode = false
consent_forwarding = "both"

[demand.pbs_main.notifications]
suppress_all = false
suppress_seats = ["example-seat"]

[demand.aps_main]
implementation = "aps"
endpoint = "https://aps.example.com/e/pb/bid"
routing = "all_eligible"
account_id = "example-aps-account"
debug = false
allow_script_creatives = false

[auction.bidders.example-server]
provider = "pbs_main"

[adserver]
provider = "adserver_mock"

[adserver.adserver_mock]
endpoint = "https://adserver.example.com/decide"
timeout_ms = 500

[integration]
provider = ["prebid"]

[integration.prebid]
timeout_ms = 1000
debug = false
client_side_bidders = ["example-browser"]
external_bundle_url = "https://assets.example.com/prebid/trusted-prebid.js"

[proxy]
allowed_domains = ["assets.example.com"]
```

`[demand] provider` lists the demand sources and each `[demand.<name>]` table
holds one source's settings. The name owns endpoint and backend correlation
and telemetry. `[auction.bidders]` maps each client-visible bidder ID to one
of those names, and a route naming a source the list does not select is
refused. The ad server is selected separately by `[adserver] provider`.

Four settings are common to every `[demand.<name>]` table, whichever
implementation it names:

| Field           | Default                | Meaning                                                        |
| --------------- | ---------------------- | -------------------------------------------------------------- |
| `endpoint`      | Required               | Absolute HTTPS endpoint, or HTTP to a loopback host            |
| `timeout_ms`    | Implementation default | PBS 1000 ms, APS 800 ms, `openrtb` inherits the auction timeout |
| `routing`       | `explicit`             | `explicit`, or `all_eligible` where the implementation allows it |
| `notifications` | No suppression         | Common `nurl`/`burl` suppression by all bids or returned seats |

Every other key in the table belongs to the implementation, which rejects any
key it does not know.

APS normally uses `all_eligible`, which sends every compatible banner slot but
never another source's bidder parameters. An `explicit` source receives only
centrally routed or trusted stored-request demand. The `prebid_server`
implementation rejects `all_eligible` because PBS requires bidder or
stored-request demand on each impression.

Demand source names must match `^[a-z][a-z0-9_]{0,62}$`. Bidder IDs are limited
to 128 UTF-8 bytes and cannot be the exact reserved browser envelope ID
`trustedServer`. Static `openrtb` `request_ext` and `imp_ext` objects are each
limited to 16 KiB, eight container levels, and 256 keys at one object level.
Notification seat lists are limited to 128 unique entries of at most 128 UTF-8
bytes each.

### Validation and target capability

Target-independent `ts config validate` compiles implementations, defaults,
routes, endpoints, bounds, signing structure, and the ad server selection.
Every adapter startup compiles the same plan and then validates backend-name
prediction, fan-out capability, and target resource limits. Fastly and Axum
allow fan-out to several demand sources, whereas Cloudflare and Spin currently
reject an enabled auction with more than one. Fastly reserves 40 of its
default 200 dynamic backend names for non-auction traffic and rejects auction
plans whose demand source names and reachable timeout buckets could require
more than the remaining 160.

This tree does not yet have the EdgeZero callback required to run target-aware
validation before `ts config push --adapter <target>` performs remote work.
Until that callback lands, startup remains the mandatory target-aware gate.

### Timeout behavior

For each demand source, Trusted Server uses the smaller of its resolved timeout
and the remaining auction budget for launch decisions and OpenRTB `tmax`. The
ad server is not called after the logical auction budget is exhausted.

No current adapter claims an abortable total-request deadline across demand
sources.
Already-launched work may complete after the logical budget, and a completed
late response can remain eligible. Local decision and delivery also finish
after network launch closes, so `timeout_ms` is not a hard wall-clock ceiling
and an auction can exceed it.

Browser Prebid `timeout_ms` and `debug` stay under `[integration.prebid]` and
are independent of every demand source value. The server endpoint, timeout,
routes, debug, test mode, overrides, consent forwarding and notification
suppression belong to the `[demand.<name>]` table, not to the browser
integration.

### Environment variable overrides

The typed `ts config validate`, `ts config diff`, and `ts config push` flows can
override existing scalar leaves. The pinned EdgeZero loader does not create
missing leaves or replace arrays, tables, maps, or rules. Existing configs must add
`rewrite_creatives = true` and `sanitize_creatives = false` before relying on
those scalar overrides. Edit and re-push TOML for other values. Every provider
name is snake_case, so a name maps straight onto a path segment:

```bash
export TRUSTED_SERVER__AUCTION__ENABLED=true
export TRUSTED_SERVER__AUCTION__REWRITE_CREATIVES=true
export TRUSTED_SERVER__AUCTION__SANITIZE_CREATIVES=false
export TRUSTED_SERVER__AUCTION__TIMEOUT_MS=2000
export TRUSTED_SERVER__DEMAND__PBS_MAIN__DEBUG=true
ts config validate
```

A `provider` list is an array, so `[demand] provider` and
`[adserver] provider` cannot be set this way. Edit the TOML and push it.

Before rolling back to a binary that does not know a creative-processing field,
remove that field's non-default value (`rewrite_creatives = false` or
`sanitize_creatives = true`), push the default-compatible blob, and then roll
back. See [Configuration](/guide/configuration#auction-configuration) for the
complete migration, upgrade-sequencing, and rollback guidance.

## Floor Prices

Floor prices can be set per-slot in the auction request. The orchestrator enforces floors after winner selection:

- In **parallel_only** mode, bids below the floor are dropped after selection
- In **parallel_adserver** mode, the floor is sent to the ad server in `ext.config.price_floor`, and also enforced locally as a safety net
- Bids without a decoded numeric price are dropped before delivery in both strategies

## Error Handling

The orchestrator is designed to be resilient:

- **Demand launch failure**. The demand source records a `launch_failed` outcome and the others continue. If every eligible provider fails before producing a pending or immediate outcome, direct `/auction` execution returns `502 Bad Gateway`. Split publisher execution records `dispatch_failed` telemetry and continues the origin response without bids.
- **Demand parse failure**. If a response cannot be parsed, an `AuctionResponse::error()` is recorded. Other results are unaffected.
- **No demand sources configured**. Completes as a no-bid without any network call.
- **No demand source produces a valid bid**. Returns an empty `OrchestrationResult` with zero winning bids after recording each outcome.
- **The ad server returns bids without decoded prices**. Those bids are filtered out with a warning.

## Observability

### Logging

The auction system logs at multiple levels throughout execution:

| Level   | Examples                                                                                |
| ------- | --------------------------------------------------------------------------------------- |
| `info`  | Auction request received, provider launch, bid counts, winner selection, total timing   |
| `debug` | Bid-drop reasons, ad server restoration notes, creative processing mode and byte counts |
| `warn`  | Demand launch failures, parse failures, ad server bids without decoded prices           |

### Response Metadata

Every auction response includes structured metadata in `ext.orchestrator`:

```json
{
  "strategy": "parallel_adserver",
  "providers": 2,
  "total_bids": 3,
  "time_ms": 145
}
```

### SSAT HTML Debug Comment

For local server-side auction template (SSAT) investigation, Trusted Server can
insert a `<!-- ts-debug: ... -->` comment before the page's bids script. Enable
it in `trusted-server.toml`, push the local configuration, restart the local
server, and search the page source for `ts-debug`:

```toml
[debug]
auction_html_comment = true

[debug.auction_html_comment_options]
include_provider_responses = true
include_adserver_response = false
include_bids = false
verbosity = "full"
format = "pretty"
```

```bash
ts config validate
ts config push --adapter fastly --local
fastly compute serve
```

This example is useful when investigating raw Prebid Server requests and
responses without spending the dump budget on winning creatives. Raw PBS
`debug.httpcalls` and `resolvedrequest` metadata also require
`debug = true` in the `[demand.<name>]` table of the relevant Prebid Server
demand source.

| Option                       | Default                                | Behavior                                                                                       |
| ---------------------------- | -------------------------------------- | ---------------------------------------------------------------------------------------------- |
| `include_provider_responses` | `true`                                 | Include the provider response array                                                            |
| `include_adserver_response`  | `true`                                 | Include the ad server response when an ad server ran                                           |
| `include_bids`               | `true`                                 | Include bid objects; when `false`, provider status and metadata remain                         |
| `metadata_keys`              | `error_type`, `http_status`, `message` | Subset of the fixed validated keys; gates them in `redacted` and `upstream`, ignored in `full` |
| `verbosity`                  | `redacted`                             | Select `redacted`, `upstream`, or `full` sensitivity                                           |
| `format`                     | `compact`                              | Use compact outer JSON or indented outer JSON with `pretty`                                    |

`metadata_keys` is a subset selector against a fixed allowlist —
`error_type`, `http_status`, and `message` — never a way to add keys. Any other
entry fails config load rather than being silently ignored.

The verbosity modes form an explicit sensitivity ladder:

- `redacted` reconstructs only validated `error_type`, `http_status`, and a
  server-generated `message`, intersected with `metadata_keys`. A successful
  provider response can therefore have `metadata: {}`.
- `upstream` adds provider-controlled errors, warnings, response timings, bid
  statuses, and bounded upstream-message fields. It builds on the redacted
  metadata, so `metadata_keys` still gates the three validated keys, while the
  provider diagnostics are unlocked by `verbosity` alone. It does not include
  raw PBS `httpcalls` or `resolvedrequest`.
- `full` includes raw response metadata and untruncated creatives, ignoring
  `metadata_keys` entirely. It can expose IP addresses, geo data, identifiers,
  consent strings, request signatures, and complete provider request/response
  bodies.

`format = "pretty"` indents only the outer dump. JSON-looking fields such as
`requestbody` and `responsebody` remain strings exactly as captured, so their
contents still appear escaped. Use a local JSON inspection tool when those
nested values need additional formatting.

The summary line's `winning=N` count is computed before section filtering, so
it can be nonzero while `include_bids = false` produces empty bid arrays. Every
mode and format neutralizes HTML-comment terminators and enforces a 256 KiB
total dump cap. A capped dump ends with `…(truncated N bytes)` and is no longer
valid JSON.

::: danger Local debugging only
Do not enable the auction HTML comment in production. Even `redacted` can
contain bid-level data and creative previews, while `upstream` and `full` may
expose identity-bearing request data to anyone who can view the page source.
:::
