# Trusted Server AI Crawler Licensing (RSL-compliant)

*April 2026*

---

## 1. Product Positioning & Scope

### 1.1 What It Is

An edge-deployed AI crawler detection and RSL licensing enforcement layer for
Trusted Server publishers. TS classifies incoming requests against published AI
crawler fingerprints, serves publisher-defined RSL licensing terms as
machine-readable XML, and enforces per-route access decisions with
standards-compliant HTTP responses (402 Payment Required for honest crawlers,
403 Forbidden for stealth or prohibited crawlers).

### 1.2 What It Is Not

- Not a payment/billing system (phase 2)
- Not a proprietary licensing protocol — RSL-native, standards-aligned
- Not a reverse proxy — runs in-line at the edge, no redirect, no subdomain
- Not a bot-blocking product in general — scoped to AI crawler licensing

### 1.3 Audience

**Primary buyer:** Large publishers and publisher consortiums (Hearst, Arena
Group, Condé Nast-tier) who already run TS or are evaluating it.

**Secondary audience:** Entitlement platforms that want to transact with these
publishers at scale without bespoke per-publisher integrations.

### 1.4 Value Proposition

1. **RSL-native, standards-compliant** — publishes `license.xml`, honors OLP
   where applicable, interoperates with any compliant entitlement platform.
2. **Edge-native, low-latency** — classification and enforcement at the CDN
   layer, no proxy hop, no subdomain redirect.
3. **Multi-signal detection including JA4** — TLS-layer fingerprinting catches
   crawlers that spoof user-agent strings.
4. **Publisher-owned config** — single `license.toml` file, version-controlled,
   no lock-in to a vendor's dashboard.
5. **Open source** — publishers can audit the enforcement behavior.

### 1.5 Reference to IAB Tech Lab CoMP

The IAB Tech Lab Content Monetization Protocol (CoMP) is a complementary
commercial framework for AI content licensing. It is currently in working group
formation. This spec references CoMP only as a future-compatible framework;
this POC does not build against CoMP endpoints or schemas. When CoMP
stabilizes, the TS RSL implementation can extend to emit or consume CoMP
commercial signals without changing the core detection or enforcement layers.

---

## 2. Scope Decisions

### 2.1 In Scope (POC / MVP)

1. AI crawler detection via six signals (see Section 4)
2. Publisher-authored public RSL terms via `license.toml`
3. Publisher-authored private enforcement rules via `license.private.toml`
4. `/license.xml` generation served from publisher's first-party domain
5. `/robots.txt` augmentation with `License:` directive
6. `Link: rel="license"` HTTP header on all responses
7. Enforcement actions: 200 (allow), 402 (honest crawler blocked), 403 (stealth
   or prohibited)
8. Debug endpoints: `/_ts/debug/rsl/summary`, `/_ts/debug/rsl/recent`,
   `/_ts/debug/rsl/license`
9. Structured logging of every classified request
10. Permissive-by-default mode with per-route or publisher-wide Strict override
11. Crawler-specific overrides (allow/deny/enforce-default) via private config
12. Integration with existing TS architecture without disrupting other
    integrations (Monetize, Edge Cookie, consent, PBS, etc.)

### 2.2 Out of Scope (Deferred)

1. **OLP license server** — token issuance, `/token`/`/introspect`/`/key`
   endpoints. Phase 2.
2. **Billing, invoicing, payment rails** — no money moves in the POC. RSL 402
   responses point to the publisher's contact information for out-of-band
   negotiation.
3. **Encrypted Media Standard (EMS)** — content encryption requires the OLP
   `/key` endpoint. Phase 2.
4. **Behavioral anomaly detection** — request rate/pattern analysis, path-depth
   heuristics, referer-chain analysis. Future phase if POC metrics justify it.
5. **Publisher SaaS dashboard** — structured logs + debug endpoints only.
   Publishers render their own visualizations from the log stream.
6. **AI-company cooperation agreements** — TS publishes standards-compliant
   signals and assumes honest actors will respect them. TS does not negotiate
   deals on publishers' behalf.
7. **Multi-publisher consortium management** — single publisher at a time for
   POC. Consortium config patterns (Hearst-style) come post-POC.
8. **CoMP framework integration** — referenced only; not built against.
9. **Creative / content transformation for AI consumers** — TS does not rewrite
   content for AI, generate summaries, or serve different versions.
10. **CAPTCHA / proof-of-human challenges** — Apple Private Access Tokens are
    available in TS if a publisher wants cryptographic human attestation, but
    not required for this POC. JA4 + IP + UA signals are sufficient for AI
    crawler classification.

---

## 3. Architecture

### 3.1 High-Level

```text
┌────────────────────────────────────────────────────────────────────┐
│  Trusted Server — Edge Compute (WASM on Fastly today)              │
│  Planned: Akamai EdgeWorkers, Cloudflare Workers                   │
│                                                                    │
│  ┌───────────────────────────────────────────────────────────┐     │
│  │  Request arrives (human or crawler, any path)             │     │
│  └──────┬────────────────────────────────────────────────────┘     │
│         │                                                          │
│         ▼                                                          │
│  ┌──────────────────────┐   ┌──────────────────────┐               │
│  │ RSL Classifier       │   │ License.toml Loader  │               │
│  │ (JA4 + UA + IP + ASN)│   │ (route → terms)      │               │
│  └──────┬───────────────┘   └──────┬───────────────┘               │
│         │                          │                               │
│         ▼                          ▼                               │
│  ┌──────────────────────────────────────────────────────────┐      │
│  │  Enforcement Decision                                    │      │
│  │  - classified as: {human | honest_ai | stealth_ai}       │      │
│  │  - mode: {permissive | strict}                           │      │
│  │  - license terms for this route                          │      │
│  └──────┬───────────────────────────────────────────────────┘      │
│         │                                                          │
│         ├──> [allow] pass through to origin/integration            │
│         ├──> [402] RSL-compliant license-required response         │
│         └──> [403] forbidden (stealth or prohibited)               │
│                                                                    │
│  Special routes (always served by TS):                             │
│  /license.xml           — generated from license.toml              │
│  /robots.txt            — augmented with License: directive        │
│  /_ts/debug/rsl/summary — dashboard JSON                           │
│  /_ts/debug/rsl/recent  — recent classifications                   │
│  /_ts/debug/rsl/license — verify published terms                   │
└────────────────────────────────────────────────────────────────────┘
```

### 3.2 Functional Units

**RSL Classifier.** Given a request, returns a classification verdict:
`{category, confidence, signals_matched, crawler_identity}`. Pure function over
request features (TLS fingerprint, headers, source IP, ASN lookup, UA string).
Consults static allowlists of published AI crawler fingerprints refreshed
periodically from `openai.com/gptbot.json`, Anthropic published ranges, and
equivalent.

**License Resolver.** Given a request path, returns the applicable RSL terms
from `license.toml` (default + most-specific route override). Produces a
`LicenseTerms` struct used by both the enforcement layer and the
`/license.xml` generator.

**Enforcement Layer.** Takes the classification verdict, license terms, and
publisher mode; produces an `Action`: `{Allow, Challenge402(reason, link),
Forbid403(reason)}`. Applied to the response before hitting origin.

### 3.3 Data Flow

```text
Request → Classifier → Verdict ──┐
                                 ├──> Enforcement → Action → Response
License.toml → Resolver → Terms ─┘                          │
                                                            ▼
                                                    Structured log
                                                    + Debug endpoint state
```

### 3.4 State Footprint at Edge

- AI crawler IP/UA/JA4 allowlists: compiled into the WASM binary at build
  time, ~tens of KB, refreshed when TS is rebuilt with updated allowlists.
- `license.toml` and `license.private.toml`: compiled into the WASM binary at
  build time (same mechanism as other TS configs).
- Per-request classification: emitted as structured log lines, optionally
  counted in a small in-memory ring buffer for debug endpoints (no KV writes
  on the hot path).

### 3.5 Integration with Existing TS Infrastructure

| Existing capability | How RSL uses it |
|---|---|
| `IntegrationRegistration` builder | New hook types: `with_request_classifier`, `with_special_route_augmenter` |
| JA4 signal from edge TLS | Input to `classifier::classify()` |
| Bot gate (H2 + JA4) | Supporting signal for stealth detection |
| `/robots.txt` handling | Integration augments existing `robots.txt` response |
| `/_ts/debug/*` auth pattern | Debug endpoints reuse existing token auth |
| Structured logging (`log-fastly`) | Classification events emitted as structured log lines |
| Settings (`trusted-server.toml`) | RSL config block added to existing settings parser |

**No changes required to:** Edge Cookie, auction orchestrator / PBS integration,
Monetize ad-server client, consent handling, existing integrations (Permutive,
Lockr, Datadome, etc.), HTML processor, cache layer.

### 3.6 Request-Path Insertion Point

RSL classification runs **after** the bot gate (so RSL sees classified
bot-or-not state) but **before** any integration that might touch the response
body (so RSL can 402/403 before wasted work).

Integration registration:

```rust
IntegrationRegistration::builder("rsl")
    .with_request_classifier()       // runs classifier, attaches verdict to request context
    .with_special_route("/license.xml")
    .with_special_route_augmenter("/robots.txt")
    .with_response_modifier()        // adds Link header on all responses
    .with_debug_routes(&[
        "/_ts/debug/rsl/summary",
        "/_ts/debug/rsl/recent",
        "/_ts/debug/rsl/license",
    ])
    .build()
```

`with_request_classifier()` and `with_special_route_augmenter()` are new hook
types added to the integration framework. All other mechanisms are existing
patterns.

### 3.7 Module Structure

```text
crates/trusted-server-core/src/rsl/
├── mod.rs               # public API + IntegrationRegistration
├── classifier.rs        # AI crawler detection logic
├── config/
│   ├── public.rs        # license.toml parser → LicenseTerms
│   └── private.rs       # license.private.toml parser → EnforcementRules
├── xml_generator.rs     # LicenseTerms → RSL XML document
├── fingerprints/
│   ├── ua_patterns.rs   # static honest-UA matchers
│   ├── ip_allowlists.rs # compiled-in crawler operator IP ranges
│   └── ja4_db.rs        # JA4 fingerprint database for LLM fetchers
├── enforcement.rs       # verdict + terms + mode → Action
├── endpoints.rs         # /license.xml, /robots.txt augmentation, debug routes
└── logging.rs           # structured log emission
```

### 3.8 Dependencies

- `quick-xml` (or existing XML crate if one is already present) for
  `license.xml` generation
- No new heavy dependencies — the classifier is pure pattern-matching against
  compiled-in data structures; no ML, no external services

### 3.9 Binary Size Impact

- IP allowlists compiled in: ~30-50 KB (a few thousand CIDR ranges from major
  operators)
- JA4 fingerprint database: ~5-10 KB (a few hundred common LLM fetcher
  fingerprints)
- UA pattern table: negligible
- Total new code: estimated 2-3K lines of Rust, well within existing crate
  structure

---

## 4. Bot Detection Signals

### 4.1 Six Signals

| # | Signal | Source | Strength | Coverage |
|---|---|---|---|---|
| 1 | **Honest User-Agent match** | HTTP `User-Agent` header | Definitive when paired with #2 | GPTBot, ClaudeBot, Claude-User, Claude-SearchBot, PerplexityBot, Perplexity-User, Google-Extended, CCBot, Bytespider, Amazonbot, Applebot-Extended, OAI-SearchBot, ChatGPT-User, Meta-ExternalAgent |
| 2 | **Published IP allowlist match** | JSON lists from crawler operators | Definitive when paired with #1 | openai.com/gptbot.json, openai.com/searchbot.json, openai.com/chatgpt-user.json, Anthropic published ranges, Perplexity ranges |
| 3 | **JA4 TLS fingerprint match** | TLS ClientHello at edge | Strong (catches spoofed UAs) | Common LLM fetcher libraries: Python `requests`, `aiohttp`, `httpx`, Go `net/http`, Node `fetch`, cURL, Scrapy, Playwright, Puppeteer |
| 4 | **ASN classification** | IP → ASN lookup | Supporting signal only (never decisive alone) | Datacenter/hosting ASNs (AWS, GCP, Azure, DigitalOcean, Hetzner, OVH), VPN/proxy ASNs, residential ASNs |
| 5 | **H2 handshake presence** | Edge TLS/HTTP layer | Supporting signal (humans nearly always H2; many scrapers still H1) | All traffic |
| 6 | **`/robots.txt` and `/license.xml` fetch correlation** | TS request logs | Supporting signal (honest bots fetch before crawling) | All traffic |

### 4.2 Classification Categories

```rust
pub enum Classification {
    /// Confirmed human or browser-class traffic.
    /// Signals: browser JA4 + H2 + consistent browsing patterns
    Human,

    /// Confirmed AI crawler with honest identity.
    /// Signals: UA match AND IP allowlist match (or JA4 match for known library)
    HonestAiCrawler {
        operator: String,      // "openai", "anthropic", "perplexity", etc.
        bot_name: String,      // "gptbot", "claude-user", "perplexitybot", etc.
        purpose: AiPurpose,    // training, search, in-conversation, index
    },

    /// Strong AI crawler suspicion without honest identity.
    /// Signals: datacenter ASN + LLM-library JA4 + no H2 or irregular headers
    StealthAiCrawler {
        signals: Vec<Signal>,
        confidence: Confidence, // High | Medium | Low
    },

    /// Cannot classify with enough confidence.
    /// Default action depends on publisher mode (Permissive vs Strict).
    Ambiguous {
        signals: Vec<Signal>,
    },
}
```

### 4.3 Signal Refresh Cadence

- **IP allowlists from crawler operators:** fetched by a control-plane job
  from each operator's published JSON endpoint (e.g., `openai.com/gptbot.json`).
  Bundled into TS releases. Publishers pick up new IP ranges when they update
  to a newer TS version. Recommended publisher refresh cadence: weekly.
- **UA patterns:** static, updated via TS release.
- **JA4 fingerprint database:** static, updated via TS release
  (community-maintained list).
- **ASN database:** updated via Maxmind or equivalent on publisher's own
  schedule.

### 4.4 Example Log Entry for a Classified Request

```json
{
  "request_id": "01HXYZ...",
  "timestamp": "2026-04-22T14:32:11Z",
  "path": "/article/some-slug",
  "classification": "honest_ai_crawler",
  "operator": "anthropic",
  "bot_name": "claudebot",
  "purpose": "ai_train",
  "signals_matched": ["ua_honest", "ip_allowlist:anthropic"],
  "action": "403_forbidden",
  "action_reason": "license.toml prohibits ai-train for this route",
  "license_terms_applied": "default",
  "mode": "permissive"
}
```

### 4.5 Stealth Classification Example

A scraper running on AWS using Python `requests` with a Chrome user-agent
gets classified as `StealthAiCrawler`:

- Signal: `asn:aws` ✓
- Signal: `ja4:python_requests` ✓
- Signal: `ua_spoofed_chrome` (UA claims Chrome but JA4 says Python) ✓
- Confidence: High
- Action depends on mode: Permissive → allow through (but logged for publisher
  review); Strict → 403.

### 4.6 Detection Posture (Default vs. Override)

**Default:** Permissive. Block only confirmed crawlers whose license terms
prohibit access. Stealth crawlers and ambiguous traffic are allowed through
but logged for publisher review.

**Override:** Strict, configurable per-publisher or per-route. Stealth and
ambiguous traffic get 403. Use for high-value routes (premium content, APIs).

### 4.7 Transparency Model

- **Transparent to the publisher:** full classification detail in logs and
  debug endpoints (which signals matched, what the decision was, why).
- **Opaque to the crawler:** crawlers receive standards-compliant RSL responses
  (401/402/403 with `WWW-Authenticate: License` and `Link` header pointing to
  `license.xml`). Crawlers are not told which signals flagged them — this
  prevents adversarial training against TS detection.

---

## 5. Configuration

### 5.1 Two-File Split: Public vs Private

**`license.toml`** — PUBLIC RSL terms. Safe to commit to a public git repo.
Everything here ends up in `/license.xml`.

**`license.private.toml`** — PRIVATE enforcement rules. Never exposed via any
endpoint. Contains per-crawler commercial overrides, enforcement mode
configuration, and any NDA-bound IP allowlist extensions.

Both files are compiled into the WASM binary at build time (same mechanism as
existing TS configs). Any change requires a rebuild and redeploy via the
standard TS deploy pipeline. Deploy time is fast (standard
`fastly compute publish` flow — typically under a minute for the publish,
plus CI build time).

**Optional future enhancement (not POC):** runtime config loading from edge KV
or Fastly Config Store so terms can be updated without a redeploy. Deferred
because it introduces failure modes (KV availability, eventual consistency,
auth) that add complexity.

### 5.2 Public `license.toml` — Minimal Example

```toml
# license.toml — publisher's RSL terms, read by Trusted Server
# Served as /license.xml (auto-generated), referenced in /robots.txt

[publisher]
name = "Example Publisher, Inc."
contact = "licensing@example.com"
contact_url = "https://example.com/licensing"
copyright_holder = "Example Publisher, Inc."
copyright_type = "organization"

# Default terms for all content not matching a more specific route
[default]
# What uses are allowed (RSL usage vocabulary)
permits = ["search", "ai-input"]
# What uses are explicitly prohibited
prohibits = ["ai-train"]
# Default payment model for permitted uses
payment = "attribution"
```

### 5.3 Public `license.toml` — Full Example With Route Overrides

```toml
[publisher]
name = "Example Publisher, Inc."
contact = "licensing@example.com"
contact_url = "https://example.com/licensing"
copyright_holder = "Example Publisher, Inc."
copyright_type = "organization"

# Default terms — homepage, category pages, public articles
[default]
permits = ["search", "ai-input"]
prohibits = ["ai-train", "ai-index"]
payment = "attribution"

# Premium/paywalled content — strict, contact for licensing
[routes."/premium/*"]
permits = []
prohibits = ["ai-all", "search"]
payment = "subscription"
amount = "10.00"
currency = "USD"

# News archive — crawl fees apply
[routes."/archive/*"]
permits = ["ai-input", "ai-index"]
prohibits = ["ai-train"]
payment = "crawl"
amount = "0.005"
currency = "USD"

# API endpoints — no AI use at all
[routes."/api/*"]
permits = []
prohibits = ["ai-all"]
payment = "free"
```

### 5.4 Private `license.private.toml` — Example

```toml
# Enforcement mode per route (not published — operational decision)
[enforcement]
default_mode = "permissive"

[[enforcement.routes]]
pattern = "/premium/*"
mode = "strict"

[[enforcement.routes]]
pattern = "/api/*"
mode = "strict"

# Per-crawler commercial overrides — commercial secrets
[[crawler_overrides]]
bot_name = "gptbot"
action = "allow"
reason_internal = "Direct license agreement - contract #2026-OAI-001"

[[crawler_overrides]]
bot_name = "perplexitybot"
action = "deny"
reason_internal = "Pending commercial agreement"

[[crawler_overrides]]
bot_name = "claudebot"
action = "enforce_default"  # apply license.toml terms as-is

# Per-operator IP allowlist extensions (e.g., publisher has a direct feed from
# a crawler operator not in the default public list)
[[ip_allowlist_extensions]]
operator = "example_ai_partner"
cidrs = ["203.0.113.0/24", "198.51.100.0/24"]
note = "Partner under NDA — not publicly disclosed"
```

### 5.5 Key Config Design Points

1. **`permits` / `prohibits` use RSL's usage vocabulary** — `search`, `ai-all`,
   `ai-train`, `ai-input`, `ai-index`. Prohibition always wins when both apply
   (per RSL spec).
2. **`payment` types match RSL's payment vocabulary** — `purchase`,
   `subscription`, `training`, `crawl`, `use`, `contribution`, `attribution`,
   `free`.
3. **Route patterns are RFC 9309-compliant** (same syntax as robots.txt) —
   wildcards supported, more specific paths override less specific.
4. **`mode` per route lives in the private config** — permissive vs strict is
   an operational decision, not a published term.
5. **`[crawler_overrides]` in private config** — per-bot exceptions for
   publishers with direct commercial deals. `"allow"` bypasses enforcement
   entirely (they've paid via a separate contract); `"deny"` blocks regardless
   of terms; `"enforce_default"` applies the route's public terms (default
   behavior).
6. **No secrets in the public file** — `license.toml` can live in a public git
   repo if the publisher wants.

### 5.6 Trusted Server Settings

```toml
# trusted-server.toml
[integrations.rsl]
enabled = true
public_config = "license.toml"
private_config = "license.private.toml"
```

---

## 6. HTTP Response Behavior

### 6.1 Allowed Request (GPTBot on Route That Permits `ai-input`)

```http
GET /article/hello-world HTTP/2
User-Agent: Mozilla/5.0 AppleWebKit/537.36 (KHTML, like Gecko); compatible; GPTBot/1.1; +https://openai.com/gptbot
```

```http
HTTP/2 200 OK
Link: <https://example.com/license.xml>; rel="license"; type="application/rsl+xml"
Content-Type: text/html
...
<article>...full content...</article>
```

TS adds the `Link` header on every response so honest crawlers can discover
license terms on any request, not just by fetching `robots.txt` first.

### 6.2 Blocked Honest Crawler (ClaudeBot on Route That Prohibits `ai-train`)

```http
GET /premium/report HTTP/2
User-Agent: Mozilla/5.0 AppleWebKit/537.36 (KHTML, like Gecko); compatible; ClaudeBot/1.0; +http://www.anthropic.com/claudebot
```

```http
HTTP/2 402 Payment Required
Link: <https://example.com/license.xml>; rel="license"; type="application/rsl+xml"
WWW-Authenticate: License realm="example.com", terms_url="https://example.com/licensing"
Content-Type: application/rsl+xml
Cache-Control: no-store

<?xml version="1.0" encoding="UTF-8"?>
<rsl xmlns="https://rslstandard.org/rsl">
  <content url="/premium/*">
    <license>
      <prohibits type="usage">ai-all</prohibits>
      <payment type="subscription">
        <amount currency="USD">10.00</amount>
      </payment>
    </license>
  </content>
</rsl>
```

The body is an inline RSL fragment describing exactly the terms the crawler
needs to satisfy. The RSL spec recommends this — crawlers don't have to
re-fetch `/license.xml` to understand what's required.

### 6.3 Blocked Stealth Crawler (Python Scraper With Spoofed Chrome UA)

```http
GET /article/hello-world HTTP/2
User-Agent: Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/122.0.0.0 Safari/537.36
```

```http
HTTP/2 403 Forbidden
Link: <https://example.com/license.xml>; rel="license"; type="application/rsl+xml"
Content-Type: text/plain

Forbidden.
```

No `WWW-Authenticate`, no explanation, no RSL fragment. Stealth crawlers get
minimum information. The `Link` header is still included for standards
compliance, but there's no negotiation invited.

### 6.4 Crawler-Specific Override — Direct License Deal

GPTBot with `[[crawler_overrides]] bot_name = "gptbot"` `action = "allow"` in
private config:

```http
GET /premium/report HTTP/2
User-Agent: Mozilla/5.0 ... GPTBot/1.1 ...
```

```http
HTTP/2 200 OK
Link: <https://example.com/license.xml>; rel="license"; type="application/rsl+xml"
Content-Type: text/html
...
```

GPTBot gets through even on the `/premium/*` route that prohibits `ai-all`,
because the private config has an override. The public `/license.xml` doesn't
reveal this — it still shows `/premium/*` as `prohibits: ai-all, subscription
$10`. Only the publisher's internal logs record that GPTBot was allowed due
to an override.

### 6.5 `/robots.txt` Response

```http
GET /robots.txt HTTP/2
```

```http
HTTP/2 200 OK
Link: <https://example.com/license.xml>; rel="license"; type="application/rsl+xml"
Content-Type: text/plain

License: https://example.com/license.xml

User-agent: *
Disallow: /admin/
Sitemap: https://example.com/sitemap.xml
```

TS preserves the publisher's existing `robots.txt` content and prepends the
`License:` directive. If the publisher doesn't have a `robots.txt` at origin,
TS generates a minimal one with just the License directive.

### 6.6 `/license.xml` Response (Public RSL Terms)

```http
GET /license.xml HTTP/2
```

```http
HTTP/2 200 OK
Content-Type: application/rsl+xml
Cache-Control: public, max-age=2592000
ETag: "v1-abc123"

<?xml version="1.0" encoding="UTF-8"?>
<rsl xmlns="https://rslstandard.org/rsl" max-age="30">
  <content url="/">
    <license>
      <permits type="usage">search</permits>
      <permits type="usage">ai-input</permits>
      <prohibits type="usage">ai-train</prohibits>
      <prohibits type="usage">ai-index</prohibits>
      <payment type="attribution"/>
    </license>
    <copyright type="organization" contactEmail="licensing@example.com">
      Example Publisher, Inc.
    </copyright>
  </content>

  <content url="/premium/*">
    <license>
      <prohibits type="usage">ai-all</prohibits>
      <prohibits type="usage">search</prohibits>
      <payment type="subscription">
        <amount currency="USD">10.00</amount>
      </payment>
    </license>
  </content>

  <content url="/archive/*">
    <license>
      <permits type="usage">ai-input</permits>
      <permits type="usage">ai-index</permits>
      <prohibits type="usage">ai-train</prohibits>
      <payment type="crawl">
        <amount currency="USD">0.005</amount>
      </payment>
    </license>
  </content>

  <content url="/api/*">
    <license>
      <prohibits type="usage">ai-all</prohibits>
      <payment type="free"/>
    </license>
  </content>
</rsl>
```

30-day cache — matches RSL's `max-age` default. Crawlers cache this and only
re-fetch when the ETag changes.

### 6.7 Full Response Matrix

| Classification | Route permits? | Action | Status | Body |
|---|---|---|---|---|
| Human | (n/a) | Allow | 200 | Normal content + `Link` header |
| Honest AI crawler | Yes | Allow | 200 | Normal content + `Link` header |
| Honest AI crawler | No | Block (polite) | 402 | RSL fragment with terms |
| Honest AI crawler | Private override: deny | Block | 403 | Minimal "Forbidden" |
| Honest AI crawler | Private override: allow | Allow | 200 | Normal content + `Link` header |
| Stealth AI crawler (strict mode) | (n/a) | Block | 403 | Minimal "Forbidden" |
| Stealth AI crawler (permissive mode) | (n/a) | Allow + log | 200 | Normal content, flagged for publisher review |
| Ambiguous (permissive mode) | (n/a) | Allow + log | 200 | Normal content |
| Ambiguous (strict mode) | (n/a) | Block | 403 | Minimal "Forbidden" |

---

## 7. Debug Endpoints & Structured Logs

### 7.1 Auth Model

All debug endpoints require `Authorization: Bearer $TS_DEBUG_TOKEN` — same
pattern as existing `/_ts/debug/*` routes. Publisher configures the token via
existing TS settings.

### 7.2 `GET /_ts/debug/rsl/summary`

Rolled-up classification counts for a configurable window.

```http
GET /_ts/debug/rsl/summary?window=24h HTTP/2
Authorization: Bearer $TS_DEBUG_TOKEN
```

```json
{
  "window": "last_24h",
  "generated_at": "2026-04-22T14:32:11Z",
  "totals": {
    "requests": 1843201,
    "human": 1832847,
    "honest_ai_crawler": 8932,
    "stealth_ai_crawler": 1217,
    "ambiguous": 205
  },
  "honest_crawlers": {
    "openai.gptbot":            {"requests": 3412, "allowed": 3412, "blocked_402": 0, "blocked_403": 0},
    "openai.chatgpt-user":      {"requests": 891,  "allowed": 891,  "blocked_402": 0, "blocked_403": 0},
    "anthropic.claudebot":      {"requests": 2104, "allowed": 0,    "blocked_402": 2104, "blocked_403": 0},
    "anthropic.claude-user":    {"requests": 612,  "allowed": 612,  "blocked_402": 0, "blocked_403": 0},
    "perplexity.perplexitybot": {"requests": 1205, "allowed": 0,    "blocked_402": 0, "blocked_403": 1205},
    "google.extended":          {"requests": 478,  "allowed": 478,  "blocked_402": 0, "blocked_403": 0},
    "bytedance.bytespider":     {"requests": 230,  "allowed": 0,    "blocked_402": 0, "blocked_403": 230}
  },
  "stealth_signals": {
    "ja4:python_requests_asn:aws": {"requests": 687, "action": "allowed_permissive"},
    "ja4:scrapy_asn:gcp":          {"requests": 312, "action": "allowed_permissive"},
    "ja4:httpx_asn:digitalocean":  {"requests": 218, "action": "blocked_403_strict"}
  },
  "top_paths_crawled": [
    {"path": "/article/*", "requests": 5831},
    {"path": "/archive/*", "requests": 2104},
    {"path": "/premium/*", "requests": 897}
  ]
}
```

### 7.3 `GET /_ts/debug/rsl/recent`

Last N classified requests, newest first. Backed by an in-process ring buffer
(no KV writes on hot path). Default 1000 entries, configurable.

```http
GET /_ts/debug/rsl/recent?limit=50&filter=honest_ai_crawler HTTP/2
Authorization: Bearer $TS_DEBUG_TOKEN
```

```json
{
  "generated_at": "2026-04-22T14:32:11Z",
  "entries": [
    {
      "request_id": "01HXYZ...",
      "timestamp": "2026-04-22T14:32:05Z",
      "path": "/article/hello-world",
      "classification": "honest_ai_crawler",
      "operator": "openai",
      "bot_name": "gptbot",
      "purpose": "ai_train",
      "signals_matched": ["ua_honest", "ip_allowlist:openai", "ja4:openai_fetcher"],
      "action": "200_allowed",
      "action_reason_public": "license permits ai-train on this route",
      "route_matched": "default",
      "mode": "permissive"
    },
    {
      "request_id": "01HXYZ...",
      "timestamp": "2026-04-22T14:32:03Z",
      "path": "/premium/q3-report",
      "classification": "honest_ai_crawler",
      "operator": "anthropic",
      "bot_name": "claudebot",
      "purpose": "ai_train",
      "signals_matched": ["ua_honest", "ip_allowlist:anthropic"],
      "action": "402_payment_required",
      "action_reason_public": "license prohibits ai-train on /premium/*",
      "route_matched": "/premium/*",
      "mode": "strict"
    }
  ]
}
```

### 7.4 `GET /_ts/debug/rsl/license`

Returns what TS is actually serving as `/license.xml`. Useful for debugging
"why isn't my route-specific term being applied?" without exposing the private
config.

```json
{
  "generated_at": "2026-04-22T14:32:11Z",
  "license_xml_etag": "v1-abc123",
  "source_file": "license.toml",
  "source_hash_sha256": "a1b2c3...",
  "rendered_xml": "<?xml version=\"1.0\"...",
  "rendered_bytes": 1847,
  "cache_max_age_seconds": 2592000,
  "route_patterns_compiled": ["/", "/premium/*", "/archive/*", "/api/*"]
}
```

### 7.5 Structured Log Format

Emitted on every classified request, pipes into existing edge log streams
(Fastly log streaming → S3/BigQuery/Datadog/etc.):

```json
{
  "event": "rsl_classification",
  "request_id": "01HXYZ...",
  "timestamp": "2026-04-22T14:32:05Z",
  "publisher_id": "example-com",
  "path": "/premium/q3-report",
  "method": "GET",
  "classification": "honest_ai_crawler",
  "operator": "anthropic",
  "bot_name": "claudebot",
  "purpose": "ai_train",
  "signals_matched": ["ua_honest", "ip_allowlist:anthropic"],
  "asn": 14618,
  "country": "US",
  "ja4_fingerprint": "t13d1516h2_8daaf6152771_02713d6af862",
  "action": "402_payment_required",
  "action_reason_public": "license prohibits ai-train on /premium/*",
  "action_reason_internal": null,
  "route_matched": "/premium/*",
  "mode": "strict"
}
```

### 7.6 Security-Aware Omissions

The debug endpoints never expose:

- Private config contents (`license.private.toml`)
- Secret values
- `action_reason_internal` — only appears in structured logs (publisher's own
  log pipeline), never in debug endpoint JSON
- Raw request bodies
- Sensitive client IPs (abbreviated or hashed depending on publisher config)

---

## 8. Publisher Onboarding Flow

### 8.1 Assumptions

- Publisher already runs Trusted Server at their edge.
- Edge platform status:
  - **Live today:** Fastly Compute (WASM)
  - **Planned:** Akamai EdgeWorkers, Cloudflare Workers
  - The RSL integration is pure Rust with no Fastly-specific dependencies in
    the classifier, resolver, or XML generator. Porting follows the same
    pattern as the rest of TS.

### 8.2 Five-Step Flow

**Step 1 — Enable the RSL integration** (~1 minute)

```toml
# trusted-server.toml
[integrations.rsl]
enabled = true
public_config = "license.toml"
private_config = "license.private.toml"
```

**Step 2 — Write `license.toml`** (~30 minutes to a few hours depending on
legal review)

Publisher creates `license.toml` from the provided template. Most time is
internal alignment on what the terms should be, not technical work. Simple
enough that non-engineers (legal + product) can own it.

**Step 3 — Write `license.private.toml` if commercial overrides exist**
(~5 minutes)

Only needed if the publisher has direct deals with AI companies. Skip if no
direct deals.

**Secret storage:**

- **Default:** compiled into the TS binary at build time (same mechanism as
  other TS config files). Works identically across all edge providers.
- **Optional:** Fastly Secret Store, Azure Key Vault, AWS Secrets Manager if
  the publisher prefers runtime secret loading. Cross-provider secret
  management has operational complexity — not required, not recommended for
  POC.

**Step 4 — Deploy TS with the new config** (~2 minutes)

Standard TS deploy pipeline. RSL integration activates automatically because
the config flag is enabled.

**Step 5 — Verify end-to-end** (~10 minutes)

```bash
# Verify license.xml is being served
curl https://example.com/license.xml

# Verify robots.txt has the License: directive
curl https://example.com/robots.txt | grep License

# Verify Link header is present on any page
curl -I https://example.com/ | grep -i link

# Verify classification is working — simulate a GPTBot request
curl -A "Mozilla/5.0 ... GPTBot/1.1 ..." https://example.com/ -v

# Verify dashboard is populating
curl https://example.com/_ts/debug/rsl/summary \
  -H "Authorization: Bearer $TS_DEBUG_TOKEN"
```

Publisher should see classifications in the dashboard within seconds of the
first AI crawler hitting them after deploy.

### 8.3 Total Onboarding Time

Under a day for a publisher already running TS, including legal review on
terms. Pure technical integration time is 30-60 minutes.

### 8.4 What Changes After Onboarding

- AI crawler traffic is classified and logged
- Honest crawlers see license terms via standard discovery (`robots.txt`,
  `Link` header, `/license.xml`)
- Blocked crawlers get RSL-compliant responses (402/403)
- Publisher has live visibility into who's crawling

### 8.5 What Does NOT Change

- Human traffic is unaffected — no latency, no blocking, no header changes
  other than adding `Link`
- Existing integrations (Monetize, Edge Cookie, consent, PBS, Permutive,
  Lockr, etc.) continue working unchanged
- Origin behavior is unchanged for non-blocked requests

---

## 9. Entitlement Platform Story (Phase 2 Preview)

The POC does not build a license server (OLP). Entitlement platforms that
want to transact with TS publishers at scale do so via the public RSL
discovery mechanism in phase 1:

1. Fetch `/license.xml` from each participating publisher
2. Parse terms programmatically (machine-readable XML schema)
3. Negotiate via publisher contact information (human, out-of-band)
4. Execute crawls with agreed-upon identity (UA, IP range, or future token)

**Phase 2 adds the Open License Protocol (OLP):**

- TS publishes `<license server="https://olp.publisher.com">` in license.xml
- OLP server runs separately from the edge (not WASM) — typically a small
  service on Cloud Run, Fly.io, DigitalOcean, or equivalent
- Implements RSL OLP endpoints: `/token`, `/introspect`, `/key`
- Entitlement platform obtains tokens programmatically, presents
  `Authorization: License <token>` on crawl requests
- TS at the edge validates tokens via local HMAC check (fast path) or
  optional `/introspect` callback (stronger security, higher latency)

**Split architecture:**

- **Hot path (WASM at edge):** token validation only. HMAC check against a
  shared signing key. Sub-millisecond. No KV writes.
- **Cold path (separate service):** token issuance, billing, dashboards, key
  management for EMS-encrypted content. Writes to persistent storage.

This split means TS stays lightweight at the edge while the commercial layer
can scale independently. Phase 2 is out of POC scope but the POC's RSL output
already declares future-compatibility via the `server` attribute when enabled.

---

## 10. Success Criteria for POC

Set before running against real traffic:

1. **Classification accuracy:** 100% of honest AI crawlers (OpenAI, Anthropic,
   Perplexity, Google-Extended, CCBot) correctly identified by UA + IP
   allowlist signals. Verified against published crawler documentation.
2. **Zero human-traffic impact:** no latency regression, no CLS, no changes
   to Core Web Vitals, no false-positive blocks on human users.
3. **Standards compliance:** `/license.xml` validates against the RSL 1.0
   schema; 402 responses include `WWW-Authenticate: License` and inline RSL
   fragment per spec.
4. **Publisher visibility:** dashboard summary populates within 5 minutes of
   deploy; recent endpoint shows live classifications.
5. **Deploy time:** onboarding from `[integrations.rsl] enabled = true` to
   live classification in under a day for an existing TS publisher.
6. **Binary size:** total RSL integration adds <100 KB to the WASM binary.

---

## 11. Open Questions

1. **Which JA4 fingerprint database do we bundle?** Community-maintained lists
   exist but vary in quality. Recommend starting with a curated list of
   ~100-200 known LLM fetcher fingerprints and expanding based on POC data.

2. **How should `mode` be represented per-route in the public vs private
   split?** The current design puts `mode` entirely in the private config.
   Alternative: allow publishers to publish `mode` for transparency, under the
   argument that stating "strict mode on /premium/*" is reasonable operational
   disclosure. Default here is private for maximum commercial flexibility.

3. **Should `/license.xml` include a `max-age` derived from config, or always
   30 days?** RSL default is 30 days. Publishers might want shorter (e.g., 1
   day) if they're actively iterating on terms. Recommend configurable with
   30-day default.

4. **Do we include PSP/CoMP signals in license.xml today?** CoMP is in working
   group formation. Safest: no CoMP-specific output today; add when the
   framework stabilizes and there are real consumers of the signal.

5. **How do we handle requests that hit `/license.xml` from obviously-stealth
   clients?** Current design serves `/license.xml` to everyone (public
   endpoint by definition). Consider rate-limiting or special handling if
   abuse patterns emerge.

6. **Which specific crawler operators get IP allowlists compiled into the
   default POC release?** Proposed starting set: OpenAI (GPTBot, ChatGPT-User,
   OAI-SearchBot), Anthropic (ClaudeBot, Claude-User, Claude-SearchBot),
   Perplexity, Google-Extended, CCBot, Bytespider, Amazonbot, Applebot-Extended,
   Meta-ExternalAgent. Others (CohereBot, xAI Grok, etc.) added in subsequent
   releases as they publish verifiable IP ranges.
