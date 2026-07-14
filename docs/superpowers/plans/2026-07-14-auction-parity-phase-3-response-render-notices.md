# Auction Parity Phase 3: Response, Rendering, and Notice Lifecycle Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Preserve bid identity and render metadata across every response path, execute creatives only in the supported isolation boundary, and fire resolved win/billing notices at the correct lifecycle events exactly once.

**Architecture:** Replace loosely related optional bid fields with canonical bid identity and render-source types. Project the same winning candidate into OpenRTB and browser/GPT envelopes, resolve notice macros before exposure, render through one sandboxed component, and emit typed lifecycle events keyed by auction + bid + event rather than URL.

**Tech Stack:** Rust 2024, OpenRTB structs, `url`, `serde`, HTML streaming/lol-html, TypeScript DOM APIs, sandboxed iframes, Prebid Universal Creative/GPT, Vitest/JSDOM.

**Reference spec:** `docs/superpowers/specs/2026-07-14-server-client-auction-parity-design.md` sections 12-13, 15, 17 Phase 3, and 18.

**Depends on:** Phase 2 canonical request, validated candidate, and structured outcome types.

**Execution workspace:** Continue on `feat/auction-parity-foundation`; do not create a worktree unless the user changes that decision.

---

## File map

### Create

- `crates/trusted-server-core/src/auction/notices.rs` — typed notice event, supported macro resolver, URL validation, and resolved notice set.
- `crates/trusted-server-core/src/auction/projection.rs` — one canonical bid-to-OpenRTB/browser projection layer.
- `crates/trusted-server-js/lib/src/core/lifecycle.ts` — typed render/win/billing/failure events and dedup keys.
- `crates/trusted-server-js/lib/src/core/renderer.ts` — one production isolated creative renderer.
- `crates/trusted-server-js/lib/test/core/lifecycle.test.ts` and `renderer.test.ts`.

### Modify

- `crates/trusted-server-core/src/auction/types.rs`, `outcome.rs`, `formats.rs`, and `orchestrator.rs`.
- `crates/trusted-server-core/src/integrations/prebid.rs` and mediator/provider parsers.
- `crates/trusted-server-core/src/publisher.rs` and `html_processor.rs`.
- `crates/trusted-server-core/src/integrations/gpt.rs` and `gpt_bootstrap.js`.
- `crates/trusted-server-js/lib/src/core/auction.ts`, `core/request.ts`, `core/render.ts`, `core/types.ts`, and `integrations/gpt/index.ts`.
- Existing Rust and Vitest rendering/notice tests plus integration browser fixtures.

---

### Task 1: Model canonical bid identity and render sources

**Files:**

- Modify: `crates/trusted-server-core/src/auction/types.rs:176-250`
- Modify: `crates/trusted-server-core/src/auction/outcome.rs`
- Test: `crates/trusted-server-core/src/auction/types.rs`

- [ ] **Step 1: Write failing construction/serialization tests**

Test inline-primary/cache-fallback, cache-only, provider-opaque, and missing-render
cases, including an inline bid with no optional `adid`. Preserve distinct
original/provider/cache IDs, actual dimensions and media type, clearing and original
provider prices, deal/expiry/net-revenue fields, notices, and bounded public provider
metadata. Ensure a bid cannot silently use one identifier for all roles and a render
plan cannot contain more than one fallback. Assert exactly 1 MiB of inline markup is
accepted and 1 MiB plus one byte is rejected before `RenderPlan` construction.

- [ ] **Step 2: Run focused tests and verify RED**

Run: `cargo test-axum bid_identity_ -- --nocapture`

Expected: current `Bid` option fields cannot enforce the required invariants.

- [ ] **Step 3: Introduce strong identity and render types**

```rust
pub struct BidIdentity {
    pub provider: String,
    pub bidder_or_seat: String,
    pub impression_id: SlotId,
    pub original_bid_id: String,
    pub ad_id: Option<String>,
    pub creative_id: Option<String>,
}

pub enum RenderSource {
    InlineAdm { markup: String },
    PrebidCache { id: String, https_url: url::Url },
    ProviderOpaque { provider: String, value: String },
}

pub struct RenderPlan {
    pub primary: RenderSource,
    pub fallback: Option<RenderSource>,
}

pub struct CanonicalBid {
    pub identity: BidIdentity,
    pub clearing_price_usd: PositiveFiniteF64,
    pub original_provider_price_usd: Option<PositiveFiniteF64>,
    pub currency: Currency,
    pub dimensions: CreativeDimensions,
    pub media_type: MediaType,
    pub render: RenderPlan,
    pub deal_id: Option<String>,
    pub expires_in_seconds: u32,
    pub net_revenue: bool,
    pub notices: NoticeTemplates,
    pub event_trackers: Vec<EventTrackerTemplate>,
    pub advertiser_domains: Vec<String>,
    pub public_metadata: BoundedPublicMetadata,
}
```

Do not synthesize an optional ad ID. Require the original bid ID and actual positive
dimensions. Enforce the 512-byte identity/domain bounds, 4096-byte HTTPS URL bound,
32-key/32-KiB public-metadata bounds, and an exact 1 MiB inline-markup limit before
constructing `InlineAdm`. When both inline markup and cache material exist, inline is
primary and cache is the sole fallback. Migrate the phase-2 `BidCandidate` in this same
task so it owns this `CanonicalBid`, selection fields such as `provider_rank`, and typed
`ProviderPrivateMetadata` used only by mediation; update `AuctionOutcome` winners and
candidates accordingly. Provider-private metadata must never enter a browser/public
projection. Do not introduce a second parallel bid model.

- [ ] **Step 4: Run focused tests and verify GREEN**

Run: `cargo test-axum bid_identity_ -- --nocapture`

Expected: all identity/render construction tests pass.

- [ ] **Step 5: Commit canonical bid modeling**

```bash
git add crates/trusted-server-core/src/auction/types.rs crates/trusted-server-core/src/auction/outcome.rs
git commit -m "Model canonical bid render identity"
```

### Task 2: Preserve provider fields during parsing and mediation

**Files:**

- Modify: `crates/trusted-server-core/src/integrations/prebid.rs:1760-2000`
- Modify: `crates/trusted-server-core/src/integrations/aps.rs`
- Modify: `crates/trusted-server-core/src/integrations/adserver_mock.rs`
- Modify: `crates/trusted-server-core/src/auction/orchestrator.rs:300-350`
- Modify: `crates/trusted-server-core/src/auction/config.rs`
- Modify: `crates/trusted-server-core/src/settings.rs`
- Test: provider parser and mediator tests

- [ ] **Step 1: Write failing parser round-trip tests**

Use fixtures containing original bid ID, `adid`, `crid`, `impid`, `dealid`, `exp`,
`w/h`, media type, inline `adm`, cache URL/UUID, `nurl`, `burl`, advertiser
domains, net-revenue/TTL inputs, provider extension metadata, clearing price, and
original provider price. Mediation must select a candidate without dropping the
winning provider's canonical fields or price provenance.

- [ ] **Step 2: Run parser tests and verify RED**

Run: `cargo test-axum parse_bid_preserves_ -- --nocapture`

Expected: at least original bid ID or one render/notice field is absent from the canonical output.

- [ ] **Step 3: Parse directly into canonical bid drafts**

Keep provider parsing responsible for protocol extraction and canonical validation
responsible for eligibility. Parse notices and event trackers into templates, retain
currency and both prices, and remove restoration-by-matching heuristics once the
mediator returns a stable candidate key. Mediation updates only clearing-price fields
on the winning `CanonicalBid`; it must not reconstruct or replace the bid.
Derive expiry/TTL and `net_revenue` from modeled provider response fields when present,
otherwise from explicit provider configuration defaults. Startup validation rejects an
enabled provider for which a positive bounded TTL or `net_revenue` cannot be
established. Preserve provider extensions only inside `ProviderPrivateMetadata`.

- [ ] **Step 4: Run provider and mediator tests and verify GREEN**

Run: `cargo test-axum preserves_ -- --nocapture`

Expected: inline/cache/mediated fixtures retain the full winning bid identity.

- [ ] **Step 5: Commit field preservation**

```bash
git add crates/trusted-server-core/src/integrations crates/trusted-server-core/src/auction/orchestrator.rs crates/trusted-server-core/src/auction/config.rs crates/trusted-server-core/src/settings.rs
git commit -m "Preserve canonical bid fields through mediation"
```

### Task 3: Project one winner into every response shape

**Files:**

- Create: `crates/trusted-server-core/src/auction/projection.rs`
- Modify: `crates/trusted-server-core/src/auction/formats.rs:227-330`
- Modify: `crates/trusted-server-core/src/publisher.rs:841-900,1932-2030`
- Modify: `crates/trusted-server-core/src/integrations/gpt.rs`
- Test: projection module, formats, and publisher tests

- [ ] **Step 1: Write failing projection equivalence tests**

Project inline-primary/cache-fallback and cache-only winners to OpenRTB and browser
bid maps. Assert auction, bid, impression, creative, and deal identity; actual
dimensions/media type; clearing price/currency and original price provenance; TTL;
advertiser domains; render plan; resolved notices/event trackers; net-revenue flag;
and bounded public DSA/category/attribute metadata remain semantically identical.
Inject sentinel provider-private mediation metadata and assert it appears in no public
projection. Missing render source must be a validation error, not empty `adm`.

- [ ] **Step 2: Run projection tests and verify RED**

Run: `cargo test-axum canonical_projection_ -- --nocapture`

Expected: current `/auction` emits empty `adm` or browser projection conditionally omits production markup.

- [ ] **Step 3: Implement a single projection API**

```rust
pub fn to_openrtb_response(
    outcome: &AuctionOutcome,
) -> Result<OpenRtbResponse, Report<TrustedServerError>>;
pub fn to_browser_bid_map(
    outcome: &AuctionOutcome,
) -> Result<BrowserBidMap, Report<TrustedServerError>>;
pub fn build_browser_bid_script(
    outcome: &AuctionOutcome,
) -> Result<String, Report<TrustedServerError>>;
```

Every projection reads auction ID/source and selected `CanonicalBid` values from the
same `AuctionOutcome`; no path reconstructs identity or economics. Remove
`inject_adm_for_testing` as a production contract switch. Debug annotations may add
diagnostics but cannot change whether a valid render source is deliverable.

- [ ] **Step 4: Run projection tests and verify GREEN**

Run: `cargo test-axum canonical_projection_ -- --nocapture`

Expected: both response paths preserve the modeled fields.

- [ ] **Step 5: Commit shared response projection**

```bash
git add crates/trusted-server-core/src/auction/projection.rs crates/trusted-server-core/src/auction/mod.rs crates/trusted-server-core/src/auction/formats.rs crates/trusted-server-core/src/publisher.rs crates/trusted-server-core/src/integrations/gpt.rs
git commit -m "Share canonical bid projection"
```

### Task 4: Resolve and validate notice macros on the server

**Files:**

- Create: `crates/trusted-server-core/src/auction/notices.rs`
- Modify: `crates/trusted-server-core/src/auction/types.rs`
- Modify: `crates/trusted-server-core/src/auction/projection.rs`
- Modify: `crates/trusted-server-core/src/auction/formats.rs`
- Modify: `crates/trusted-server-core/src/publisher.rs`
- Test: `crates/trusted-server-core/src/auction/notices.rs`

- [ ] **Step 1: Write failing macro and URL tests**

Cover supported price/currency/auction/bid/impression/seat/ad-ID macros, repeated
macros, percent-encoded values, unknown macros, malformed URLs, HTTP and non-HTTP(S)
schemes, credentials, fragments, and length bounds. For creative markup, assert all
recognized `${AUCTION_*}` macros resolve, other `${...}` text is preserved, and any
remaining `${AUCTION_*}` invalidates inline markup and promotes the validated cache
fallback. Assert raw notice macros never reach browser output.

- [ ] **Step 2: Run focused tests and verify RED**

Run: `cargo test-axum notice_macro_ -- --nocapture`

Expected: no shared resolver exists and raw templates are currently projected.

- [ ] **Step 3: Implement a closed macro resolver**

```rust
pub struct NoticeFacts<'a> {
    pub auction_id: &'a str,
    pub bid_id: &'a str,
    pub impression_id: &'a str,
    pub bidder_or_seat: &'a str,
    pub ad_id: Option<&'a str>,
    pub price_usd: f64,
    pub currency: Currency,
}

pub fn resolve_notice_url(
    template: &str,
    facts: &NoticeFacts<'_>,
) -> Result<url::Url, NoticeError>;

pub fn resolve_inline_adm(
    markup: &str,
    facts: &NoticeFacts<'_>,
) -> Result<String, CreativeMacroError>;
```

Reject unknown notice tokens instead of forwarding them. Require HTTPS both before and
after substitution. For `adm`, resolve every recognized auction macro, preserve other
template/JavaScript expressions, and return a typed unresolved-auction-macro error so
`RenderPlan` construction can promote cache fallback or reject the bid. Resolve notice
URLs, event trackers, and inline markup before any browser projection.

- [ ] **Step 4: Run resolver and projection tests and verify GREEN**

Run: `cargo test-axum notice_ -- --nocapture`

Expected: valid notices resolve and invalid/raw templates are excluded with typed reasons.

- [ ] **Step 5: Commit notice resolution**

```bash
git add crates/trusted-server-core/src/auction/notices.rs crates/trusted-server-core/src/auction/mod.rs crates/trusted-server-core/src/auction/types.rs crates/trusted-server-core/src/auction/projection.rs crates/trusted-server-core/src/auction/formats.rs crates/trusted-server-core/src/publisher.rs
git commit -m "Resolve auction notice macros"
```

### Task 5: Build one isolated production renderer

**Files:**

- Create: `crates/trusted-server-js/lib/src/core/renderer.ts`
- Modify: `crates/trusted-server-js/lib/src/core/request.ts:80-150`
- Modify: `crates/trusted-server-js/lib/src/core/render.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/gpt/index.ts`
- Test: `crates/trusted-server-js/lib/test/core/renderer.test.ts`

- [ ] **Step 1: Write failing renderer security/behavior tests**

Assert executable inline markup runs only in a newly created iframe whose exact default
sandbox is `allow-scripts allow-popups allow-forms`; `allow-same-origin`, top
navigation, and publisher-DOM access are absent. Dimensions are required. Rejected or
missing sources do not clear existing content. Inline markup over exactly 1 MiB is
rejected before projection. Cache markup is fetched successfully through a response
capped at exactly 1 MiB before iframe creation. The primary is attempted first and
an inline runtime failure attempts its declared cache fallback once. `RenderConfirmed`
requires the first iframe `load` within 5000 ms. Error, removal, timeout, or absent PUC
inner-renderer acknowledgement yields `RenderFailed`; merely posting `Prebid Response`
does not acknowledge rendering. Test cache bodies at exactly 1 MiB and 1 MiB plus one
byte, including a chunked response that crosses the cap.

- [ ] **Step 2: Run renderer tests and verify RED**

Run:

```bash
cd crates/trusted-server-js/lib
npx vitest run test/core/renderer.test.ts
```

Expected: renderer module is absent and current direct path sanitizes scripts into non-executable HTML.

- [ ] **Step 3: Implement the minimal isolated renderer**

```ts
export type RenderResult =
  | { kind: 'rendered'; iframe: HTMLIFrameElement }
  | { kind: 'failed'; reason: RenderFailureReason }

export function renderBid(
  container: HTMLElement,
  bid: BrowserBid
): Promise<RenderResult>
```

Use `srcdoc` only inside a TS-owned sandbox and never write bidder markup into the
publisher document or reuse an unsandboxed GAM iframe. Fetch cache content with an
exact 1 MiB byte cap before creating the iframe, then render cache and inline markup
through the same path. Execute the primary and then at most its declared fallback.
Start a 5000 ms first-load timer after assigning `srcdoc`/URL. If PUC `mkFrame` cannot
prove the sandbox contract, TS creates the inner iframe; its MessageChannel bridge must
acknowledge after that iframe's load. Centralize cache/PUC/provider-opaque selection
rather than maintaining direct/GPT-specific renderers.

- [ ] **Step 4: Run renderer tests and verify GREEN**

Run the command from Step 2.

Expected: isolation and render-result tests pass.

- [ ] **Step 5: Commit the renderer**

```bash
git add crates/trusted-server-js/lib/src/core/renderer.ts crates/trusted-server-js/lib/src/core/request.ts crates/trusted-server-js/lib/src/core/render.ts crates/trusted-server-js/lib/src/integrations/gpt/index.ts crates/trusted-server-js/lib/test/core/renderer.test.ts
git commit -m "Add isolated auction creative renderer"
```

### Task 6: Introduce typed lifecycle events and stable deduplication

**Files:**

- Create: `crates/trusted-server-js/lib/src/core/lifecycle.ts`
- Modify: `crates/trusted-server-js/lib/src/core/types.ts`
- Modify: `crates/trusted-server-js/lib/src/core/renderer.ts`
- Test: `crates/trusted-server-js/lib/test/core/lifecycle.test.ts`

- [ ] **Step 1: Write failing lifecycle tests**

Cover `CandidateSelected`, `RenderConfirmed`, `RenderFailed`, `WinConfirmed`, and
`BillingConfirmed`. Repeat one URL across two auctions and require both to fire; repeat
the full same event key within one auction and require dedupe. Include two providers or
impressions that reuse an original bid ID and assert they do not collide. Concurrent
attempts must reserve the key before asynchronous cache/render completion.

- [ ] **Step 2: Run lifecycle tests and verify RED**

Run:

```bash
cd crates/trusted-server-js/lib
npx vitest run test/core/lifecycle.test.ts
```

Expected: URL-based/global dedupe fails the two-auction or concurrent case.

- [ ] **Step 3: Implement event-keyed lifecycle state**

```ts
export interface CanonicalEventIdentity {
  auctionId: string
  provider: string
  bidderOrSeat: string
  originalBidId: string
  impressionId: string
}

export function eventKey(
  identity: CanonicalEventIdentity,
  event: AuctionLifecycleEvent
): string
```

Keep selected-candidate telemetry distinct from rendered/win/billing confirmation. Reserve in-flight keys synchronously and mark terminal state after completion.

- [ ] **Step 4: Run lifecycle tests and verify GREEN**

Run the command from Step 2.

Expected: per-auction dedupe and concurrency tests pass.

- [ ] **Step 5: Commit lifecycle state**

```bash
git add crates/trusted-server-js/lib/src/core/lifecycle.ts crates/trusted-server-js/lib/src/core/types.ts crates/trusted-server-js/lib/src/core/renderer.ts crates/trusted-server-js/lib/test/core/lifecycle.test.ts
git commit -m "Add auction render lifecycle events"
```

### Task 7: Fire win and billing notices from proven render events

**Files:**

- Modify: `crates/trusted-server-js/lib/src/integrations/gpt/index.ts:340-930`
- Modify: `crates/trusted-server-js/lib/src/integrations/prebid/index.ts`
- Modify: `crates/trusted-server-core/src/integrations/gpt.rs:470-490`
- Modify: `crates/trusted-server-core/src/integrations/gpt_bootstrap.js`
- Modify: `crates/trusted-server-js/lib/test/integrations/gpt/ad_init.test.ts`
- Modify: `crates/trusted-server-js/lib/test/integrations/prebid/index.test.ts`
- Test: `crates/trusted-server-js/lib/test/core/renderer.test.ts`

- [ ] **Step 1: Write failing notice-timing tests**

Assert candidate selection/targeting alone never fires notices. A matching GAM
`slotRenderEnded` or Prebid `onBidWon` fires `nurl` once; the renderer's defined load
confirmation fires `burl` and render trackers once. Losing GAM line items never fire TS
notices. Direct `requestAds` fires `nurl` when it claims the matching slot before iframe
load, so a later render failure retains the win notice but never fires billing. A second
auction using the same URL still fires. All OpenRTB notice calls are GET-compatible.
For provider event trackers with an explicit modeled event type, assert each fires only
at its mapped lifecycle event and is deduplicated with the same full canonical key.

- [ ] **Step 2: Run GPT tests and verify RED**

Run:

```bash
cd crates/trusted-server-js/lib
npx vitest run test/integrations/gpt/ad_init.test.ts
```

Expected: at least URL-based dedupe or combined win/billing timing fails.

- [ ] **Step 3: Connect resolved notices to lifecycle confirmation**

Use the shared lifecycle service for direct, Prebid, and GPT/PUC renders. Invoke
OpenRTB notices with an image tracker or `fetch` using HTTP `GET`; do not use
`sendBeacon` POST or keepalive POST. Fire `nurl` at `WinConfirmed`, and fire `burl` plus
render event trackers only at `RenderConfirmed`. Route every other typed provider event
tracker through its explicit lifecycle mapping; unknown/unmodeled event types are not
invoked. Never invoke unresolved templates.

- [ ] **Step 4: Run GPT and direct renderer tests and verify GREEN**

Run:

```bash
cd crates/trusted-server-js/lib
npx vitest run test/integrations/gpt/ad_init.test.ts test/integrations/prebid/index.test.ts test/core/renderer.test.ts test/core/lifecycle.test.ts
```

Expected: notice ownership/timing/dedup tests pass.

- [ ] **Step 5: Commit notice lifecycle integration**

```bash
git add crates/trusted-server-js/lib/src crates/trusted-server-js/lib/test crates/trusted-server-core/src/integrations/gpt.rs crates/trusted-server-core/src/integrations/gpt_bootstrap.js
git commit -m "Fire auction notices after confirmed render"
```

### Task 8: Guarantee exactly-once bid injection at EOF

**Files:**

- Modify: `crates/trusted-server-core/src/publisher.rs:600-1250`
- Modify: `crates/trusted-server-core/src/html_processor.rs`
- Test: publisher streaming tests

- [ ] **Step 1: Add failing streaming matrix tests**

Cover explicit `</body>` in one chunk, split closing tag, implicit close, missing close tag, malformed HTML, empty body, compressed/uncompressed paths, and auction completion before/after EOF. Assert one bids script and one finalization, never zero or two.

- [ ] **Step 2: Run streaming tests and verify RED**

Run: `cargo test-axum auction_injection_ -- --nocapture`

Expected: at least implicit/missing-close behavior lacks the canonical injection point.

- [ ] **Step 3: Add a single injection state machine with EOF fallback**

Track `NotInjected | Injected` in the processor/stream context. The body-close handler injects when seen; final EOF injects only if still pending. Do not duplicate bid-state serialization between branches.

- [ ] **Step 4: Run streaming tests and verify GREEN**

Run: `cargo test-axum auction_injection_ -- --nocapture`

Expected: the full matrix injects exactly once.

- [ ] **Step 5: Commit EOF-safe injection**

```bash
git add crates/trusted-server-core/src/publisher.rs crates/trusted-server-core/src/html_processor.rs
git commit -m "Inject auction results exactly once"
```

### Task 9: Add browser integration coverage, docs, and run the Phase 3 gate

**Files:**

- Create: `crates/trusted-server-integration-tests/browser/tests/shared/auction-render.spec.ts`
- Modify: `docs/guide/creative-processing.md`
- Modify: `docs/guide/auction-orchestration.md`
- Modify: `docs/guide/integrations/gpt.md`
- Modify: `docs/guide/integrations/prebid.md`
- Modify: `crates/trusted-server-core/src/settings.rs`
- Modify: `crates/trusted-server-js/lib/src/core/types.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/gpt/index.ts`

- [ ] **Step 1: Add browser scenarios**

Exercise one inline and one cache creative through initial navigation and `/auction`, confirm sandbox attributes, rendered dimensions, lifecycle beacons, losing-bid suppression, and two-auction dedupe behavior.

- [ ] **Step 2: Remove stale debug-only and empty-creative contracts**

Use `rg -n "debug.*adm|adm.*debug|empty adm|missing creative" crates docs` and align every comment/type with production renderer policy.

- [ ] **Step 3: Update public lifecycle documentation**

Document candidate versus rendered impression, renderer/PUC requirements, cache versus inline delivery, notice macro ownership, and event timing.

- [ ] **Step 4: Run the complete Phase 3 verification set**

Run all master-spec Rust/JS/docs gates plus applicable browser integration tests. Every command must exit zero before handoff.

- [ ] **Step 5: Commit integration fixtures and docs**

```bash
git add crates/trusted-server-integration-tests crates/trusted-server-core crates/trusted-server-js docs
git commit -m "Lock auction render notice lifecycle"
```

---

## Phase 3 completion checkpoint

Do not start browser refresh/media work until original bid identity and render source survive every projection, invalid render sources are rejected, inline/cache creatives render only in the verified sandbox, notice macros are resolved server-side, win/billing events require render evidence, dedupe keys include auction identity, and all streaming close/EOF shapes inject exactly once.
