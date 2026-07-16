# Auction Orchestration

Learn how Trusted Server coordinates multiple demand sources in parallel to maximize revenue and minimize latency.

## Overview

The auction orchestrator is the core system that manages server-side ad auctions. It launches bid requests to multiple demand providers simultaneously, collects responses, and selects winners.

Key capabilities:

- **Parallel execution** — Bid requests to all providers launch concurrently using Fastly's `select()` API
- **Strategy-based winner selection** — Automatic strategy detection based on configuration
- **Mediator support** — Optional external mediator for final winner selection and unified floor pricing
- **Provider abstraction** — Pluggable provider interface for adding new demand sources
- **Creative rewriting** — Winning creatives are sanitized and rewritten with first-party proxy URLs by default

## System Flow (Prebid + APS)

The following diagram shows the full auction flow when both Prebid and APS providers are configured with a mediator:

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
  participant Med as AdServer Mediator
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
    Orch->>Orch: Detect strategy<br/>mediator? parallel_mediation : parallel_only
    deactivate TS

    Note over Orch: Strategy determined by config:<br/>[auction]<br/>mediator = "adserver_mock" → parallel_mediation<br/>No mediator → parallel_only
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
  alt Mediator Configured (parallel_mediation)
    rect rgb(236,253,245)
      Note over Client,Mock: Mediation Flow
      activate Med
      Orch->>Med: POST /adserver/mediate<br/>Decoded-price bids for final selection
      Note right of Orch: APS price: 2.50<br/>Prebid price: 2.00

      Med->>Med: Apply mediation policy and floors<br/>Select highest CPM per slot
      Med-->>Orch: OpenRTB response with winners
      Note right of Med: APS renderer state is restored from<br/>the reduced source bid after mediation
      deactivate Med
    end
  else No Mediator (parallel_only)
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
    Orch->>Orch: Transform to OpenRTB response<br/>Preserve typed render source<br/>Sanitize ordinary creative HTML<br/>Optionally rewrite creative URLs<br/>Add orchestrator metadata

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
  ├─ Detect strategy (parallel_only or parallel_mediation)
  ├─ Launch all providers in parallel via select()
  ├─ Collect responses as they complete
  │
  ├─[parallel_only]─── Select highest decoded CPM per slot
  └─[parallel_mediation]─── Forward decoded-price bids to mediator for final selection
  │
  ▼
Convert OrchestrationResult → OpenRTB 2.x Response
  │
  ├─ Sanitize creative HTML
  ├─[rewrite_creatives=true] Rewrite URLs and inject creative TSJS
  ├─ Add ext.orchestrator metadata
  └─ Set consent and optional EID response headers
```

### Key Components

The orchestrator is composed of several modules:

| Module            | Path                                      | Purpose                                     |
| ----------------- | ----------------------------------------- | ------------------------------------------- |
| `orchestrator.rs` | `crates/trusted-server-core/src/auction/` | Core parallel execution and bid selection   |
| `provider.rs`     | `crates/trusted-server-core/src/auction/` | `AuctionProvider` trait definition          |
| `types.rs`        | `crates/trusted-server-core/src/auction/` | Data structures (AuctionRequest, Bid, etc.) |
| `formats.rs`      | `crates/trusted-server-core/src/auction/` | Format conversions (TSJS ↔ OpenRTB)         |
| `endpoints.rs`    | `crates/trusted-server-core/src/auction/` | HTTP handler for `POST /auction`            |
| `config.rs`       | `crates/trusted-server-core/src/auction/` | Auction configuration types                 |

### Provider Auto-Discovery

Providers register themselves at startup via builder functions. The `build_orchestrator()` function in `auction/mod.rs` iterates all registered builders, passes the application settings, and each builder returns zero or more providers depending on whether its config section is present and enabled:

```rust
// Each integration registers its own builder
fn provider_builders() -> &'static [ProviderBuilder] {
    &[
        prebid::register_auction_provider,
        aps::register_providers,
        adserver_mock::register_providers,
    ]
}
```

This means you only need to add a config section to `trusted-server.toml` for a provider to be automatically discovered and registered.

## Auction Strategies

The orchestrator automatically selects a strategy based on whether a `mediator` is configured.

### Parallel Only

When no mediator is set, the orchestrator runs all providers in parallel and selects winners by comparing decoded prices directly. This is the simplest strategy.

```toml
[auction]
enabled = true
providers = ["prebid", "aps"]
# No mediator — direct price comparison
timeout_ms = 2000
```

**How winner selection works:**

1. Collect bids from all providers.
2. Group bids by slot ID.
3. Skip bids without a decoded numeric price.
4. Select the highest CPM for each slot.
5. Apply floor prices and drop winners below the slot's floor.

APS OpenRTB supplies decoded prices, so eligible APS bids participate directly without requiring a mediator.

### Parallel Mediation

When a `mediator` is configured, provider responses are forwarded to the mediator service for final winner selection and unified floor pricing.

```toml
[auction]
enabled = true
providers = ["prebid", "aps"]
mediator = "adserver_mock"  # Enables mediation
timeout_ms = 2000
```

**How mediation works:**

1. Run all providers in parallel (same as parallel_only).
2. Collect all responses.
3. Forward bids with decoded numeric prices to the mediator.
4. Let the mediator apply policy and choose a winner.
5. Restore render/accounting state from the selected source bid.
6. Filter any mediator winner without a decoded price.

Mediation is optional for APS. APS reduces to one candidate per impression before mediation so the selected renderer can be restored without same-slot ambiguity.

## Providers

### Provider Interface

All demand sources implement the `AuctionProvider` trait:

```rust
pub trait AuctionProvider: Send + Sync {
    fn provider_name(&self) -> &'static str;

    fn request_bids(
        &self,
        request: &AuctionRequest,
        context: &AuctionContext<'_>,
    ) -> Result<PendingRequest, Report<TrustedServerError>>;

    fn parse_response(
        &self,
        response: fastly::Response,
        response_time_ms: u64,
    ) -> Result<AuctionResponse, Report<TrustedServerError>>;

    fn supports_media_type(&self, media_type: &MediaType) -> bool;
    fn timeout_ms(&self) -> u32;
    fn is_enabled(&self) -> bool;
    fn backend_name(&self) -> Option<String>;
}
```

The trait uses a two-phase design:

1. **`request_bids()`** — Builds and sends the HTTP request, returning a `PendingRequest` (Fastly's async handle)
2. **`parse_response()`** — Called once the response arrives, parses the provider-specific format into a unified `AuctionResponse`

This split enables true parallel execution: all requests launch first, then the orchestrator uses `select()` to process responses as they arrive.

### Prebid Provider

Transforms auction requests into OpenRTB 2.x format and sends them to a Prebid Server instance.

**Request transformation:**

- `AdSlot` → `Imp` with `Banner { format: [Format { w, h }] }`
- Bidder params from slot config → `ext.prebid.bidder` map
- EC and fresh user IDs injected into `User` object
- Device info, geo data, and GPC signals included
- Optional Ed25519 request signing (see [Request Signing](/guide/request-signing))

**Response parsing:**

- Bids include decoded `price` (clear decimal CPM)
- Creative HTML provided in `adm` field
- Winning creative URLs rewritten to first-party proxy format by default when the `/auction` response is assembled
- Per-bidder timing (`responsetimemillis`), errors, and warnings always attached as response metadata
- When `debug` is enabled, PBS debug payload and per-bid status (`bidstatus`) also included

```toml
[integrations.prebid]
enabled = true
server_url = "https://prebid-server.example.com"
timeout_ms = 1000
bidders = ["appnexus", "rubicon"]
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
[integrations.aps]
enabled = true
account_id = "example-account"
endpoint = "https://web.ads.aps.amazon-adsystem.com/e/pb/bid"
timeout_ms = 800
allow_script_creatives = false
```

See [APS OpenRTB Integration](/guide/integrations/aps) for rollout and rendering requirements.

### AdServer Mock Mediator

An external mediation service that receives decoded-price bidder responses and performs final winner selection. APS prices are already decoded at the provider boundary.

**Mediation request format:**

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

**Mediation response:** Standard OpenRTB with decoded prices and selected winners.

```toml
[integrations.adserver_mock]
enabled = true
endpoint = "https://your-mediator.example.com/adserver/mediate"
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

The `price` field remains optional so missing-price bids fail closed. APS supplies a decoded price and a typed renderer instead of creative HTML; the renderer is retained through direct winner selection and mediation.

### OrchestrationResult

The complete result of an auction:

```rust
pub struct OrchestrationResult {
    pub provider_responses: Vec<AuctionResponse>,       // All provider results
    pub mediator_response: Option<AuctionResponse>,     // Mediator result (if used)
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
      "strategy": "parallel_mediation",
      "providers": 2,
      "total_bids": 3,
      "time_ms": 145
    }
  }
}
```

EC identity is maintained with the `ts-ec` cookie; auction responses do not emit EC ID headers.

## Creative Processing

Winning creatives returned by `POST /auction` are always passed through the
server-side sanitizer. By default, the sanitized HTML is then processed by an
HTML rewriter (`lol_html`) that converts eligible external resource and click
URLs to signed first-party paths, adds `data-tsclick`, rewrites inline CSS
`url(...)` values, and injects the unified creative TSJS runtime when a `<body>`
exists.

```toml
[auction]
rewrite_creatives = true
```

| Setting           | Winning-bid `adm` behavior                                                                                                                                          |
| ----------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Omitted or `true` | Sanitize, rewrite eligible URLs, add click-guard attributes, and inject creative TSJS.                                                                              |
| `false`           | Sanitize, then return sanitized but unre-written HTML. Accepted asset and click URLs remain direct, so the browser may contact third-party hosts without mediation. |

Disabling rewriting does not disable or reverse sanitization. Scripts,
stylesheets, style blocks, forms, event handlers, dangerous URL schemes, and
other rejected content remain removed. It also removes the injected creative
runtime and first-party proxy/click mediation from the resulting `adm`.
Sanitizer-accepted hosts are not allowlisted or trusted merely because their
URLs remain in the output.

The setting applies only to the shared `POST /auction` response converter.
HTML/CSS returned by `/first-party/proxy` continues to be rewritten. The
separate debug-only `[debug].inject_adm_for_testing` publisher and page-bids
path is unchanged and may include raw `adm` for non-production diagnostics.

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

The rewrite pass leaves relative URLs and non-network schemes unchanged. On the
auction path, mandatory sanitization runs first and strips dangerous schemes,
so only sanitizer-accepted values reach this pass. Domains in the
`rewrite.exclude_domains` config list (supports wildcards like
`*.cdn.example.com`) are also skipped.

Each proxied URL includes a `tstoken` HMAC signature for tamper protection. See [Proxy Signing](/guide/proxy-signing) for details.

## Configuration

### Full Example

```toml
[auction]
enabled = true
rewrite_creatives = true
providers = ["prebid", "aps"]
mediator = "adserver_mock"    # Remove for parallel_only strategy
timeout_ms = 2000

[integrations.prebid]
enabled = true
server_url = "https://prebid-server.example.com"
timeout_ms = 1000
bidders = ["appnexus", "rubicon"]
auto_configure = true
debug = false

[integrations.aps]
enabled = true
account_id = "example-account"
endpoint = "https://web.ads.aps.amazon-adsystem.com/e/pb/bid"
timeout_ms = 800
allow_script_creatives = false

[integrations.adserver_mock]
enabled = true
endpoint = "https://your-mediator.example.com/adserver/mediate"
timeout_ms = 500
price_floor = 0.50
```

### Configuration Reference

#### `[auction]`

| Field               | Type     | Default | Description                                                       |
| ------------------- | -------- | ------- | ----------------------------------------------------------------- |
| `enabled`           | bool     | `false` | Enable the auction system                                         |
| `rewrite_creatives` | bool     | `true`  | Rewrite sanitized winning-bid `adm` through first-party endpoints |
| `providers`         | string[] | `[]`    | Ordered list of provider names to call                            |
| `mediator`          | string?  | `null`  | Provider name to use as mediator (enables `parallel_mediation`)   |
| `timeout_ms`        | u32      | `2000`  | Overall auction timeout in milliseconds                           |

#### `[integrations.prebid]`

| Field            | Type     | Default           | Description                                                                            |
| ---------------- | -------- | ----------------- | -------------------------------------------------------------------------------------- |
| `enabled`        | bool     | `true`            | Enable Prebid provider                                                                 |
| `server_url`     | string   | —                 | Prebid Server URL (required)                                                           |
| `timeout_ms`     | u32      | `1000`            | Request timeout                                                                        |
| `bidders`        | string[] | `["mocktioneer"]` | Default bidders when not specified per-slot                                            |
| `auto_configure` | bool     | `true`            | Auto-remove client-side prebid.js scripts                                              |
| `debug`          | bool     | `false`           | Enable Prebid debug mode (sets `ext.prebid.debug` and `ext.prebid.returnallbidstatus`) |
| `test_mode`      | bool     | `false`           | Set OpenRTB `test: 1` for non-billable test traffic                                    |

#### `[integrations.aps]`

| Field                    | Type   | Default                                            | Description                                      |
| ------------------------ | ------ | -------------------------------------------------- | ------------------------------------------------ |
| `enabled`                | bool   | `false`                                            | Enable APS provider                              |
| `account_id`             | string | —                                                  | APS account ID (required; `pub_id` is an alias)  |
| `endpoint`               | string | `https://web.ads.aps.amazon-adsystem.com/e/pb/bid` | APS OpenRTB endpoint                             |
| `timeout_ms`             | u32    | `800`                                              | Request timeout                                  |
| `allow_script_creatives` | bool   | `false`                                            | Admit script bids before APS candidate reduction |

#### `[integrations.adserver_mock]`

| Field         | Type   | Default                                  | Description               |
| ------------- | ------ | ---------------------------------------- | ------------------------- |
| `enabled`     | bool   | `false`                                  | Enable mediator           |
| `endpoint`    | string | `http://localhost:6767/adserver/mediate` | Mediator service endpoint |
| `timeout_ms`  | u32    | `500`                                    | Request timeout           |
| `price_floor` | f64?   | `null`                                   | Global price floor CPM    |

### Timeout Tuning

The orchestrator timeout should exceed the sum of provider timeouts to allow all providers to respond. Providers that exceed their individual timeouts are collected as they finish — the orchestrator doesn't wait indefinitely.

```toml
[auction]
timeout_ms = 2000              # Overall ceiling

[integrations.prebid]
timeout_ms = 1000              # Prebid Server budget

[integrations.aps]
timeout_ms = 800               # APS budget

[integrations.adserver_mock]
timeout_ms = 500               # Mediator budget (called after providers)
```

### Environment Variable Overrides

All auction configuration can be overridden via environment variables:

```bash
TRUSTED_SERVER__AUCTION__ENABLED=true
TRUSTED_SERVER__AUCTION__REWRITE_CREATIVES=true
TRUSTED_SERVER__AUCTION__PROVIDERS=prebid,aps
TRUSTED_SERVER__AUCTION__MEDIATOR=adserver_mock
TRUSTED_SERVER__AUCTION__TIMEOUT_MS=2000
TRUSTED_SERVER__INTEGRATIONS__PREBID__SERVER_URL=https://pbs.example.com
TRUSTED_SERVER__INTEGRATIONS__APS__ACCOUNT_ID=example-account
```

## Floor Prices

Floor prices can be set per-slot in the auction request. The orchestrator enforces floors after winner selection:

- In **parallel_only** mode: bids below the floor are dropped after selection
- In **parallel_mediation** mode: the floor is sent to the mediator in `ext.config.price_floor`, and also enforced locally as a safety net
- Bids without a decoded numeric price are dropped before delivery in both strategies

## Error Handling

The orchestrator is designed to be resilient:

- **Provider launch failure** — If a provider fails to launch its request (e.g., missing backend), it is skipped with a warning. Other providers continue.
- **Provider parse failure** — If a response can't be parsed, an `AuctionResponse::error()` is recorded. Other results are unaffected.
- **No providers configured** — Returns an error: `"No providers configured"`
- **All providers fail** — Returns an empty `OrchestrationResult` with zero winning bids
- **Mediator returns bids without decoded prices** — Those bids are filtered out with a warning

## Observability

### Logging

The auction system logs at multiple levels throughout execution:

| Level   | Examples                                                                                |
| ------- | --------------------------------------------------------------------------------------- |
| `info`  | Auction request received, provider launch, bid counts, winner selection, total timing   |
| `debug` | Bid-drop reasons, mediation restoration notes, creative processing mode and byte counts |
| `warn`  | Provider launch failures, parse failures, mediator bids without decoded prices          |

### Response Metadata

Every auction response includes structured metadata in `ext.orchestrator`:

```json
{
  "strategy": "parallel_mediation",
  "providers": 2,
  "total_bids": 3,
  "time_ms": 145
}
```
