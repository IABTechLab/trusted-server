# Auction Parity Phase 2: Canonical Request and Economics Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make initial navigation, SPA page-bids, direct `requestAds`, and the custom Prebid adapter produce one validated auction request and one deterministic economic outcome.

**Architecture:** Introduce canonical page/request/slot/outcome modules and make path adapters supply only transport-specific inputs. Providers receive a validated immutable request snapshot; the orchestrator records partial failures, validates bids before comparison, filters floors before winner selection, and uses a deterministic comparator independent of completion order.

**Tech Stack:** Rust 2024, `serde`, `url`, `uuid`, `error-stack`, EdgeZero platform traits, TypeScript, Prebid.js, Vitest, cross-adapter Rust fixtures.

**Reference spec:** `docs/superpowers/specs/2026-07-14-server-client-auction-parity-design.md` sections 7.2-7.4, 9-11, 16, 17 Phase 2, and 18.

**Depends on:** Phase 1 security and identity plan completed and verified.

**Execution workspace:** Continue on `feat/auction-parity-foundation`; do not create a worktree unless the user changes that decision.

---

## File map

### Create

- `crates/trusted-server-core/src/auction/request.rs` — `CanonicalPage`, `CanonicalAuctionRequest`, path input types, and one builder.
- `crates/trusted-server-core/src/auction/validation.rs` — `ValidatedAdSlot`, provider bid validation, and bounded typed context.
- `crates/trusted-server-core/src/auction/outcome.rs` — `ProviderOutcome`, `BidCandidate`, deterministic comparison, and canonical winner result.
- `crates/trusted-server-core/src/platform/capabilities.rs` — adapter capability record and startup compatibility validation.
- `crates/trusted-server-integration-tests/fixtures/auction/` — equivalent logical auction fixtures and expected normalized outputs.

### Modify

- `crates/trusted-server-core/src/auction/mod.rs`, `types.rs`, `formats.rs`, `endpoints.rs`, `orchestrator.rs`, `provider.rs`, and `config.rs`.
- `crates/trusted-server-core/src/publisher.rs` and `creative_opportunities.rs`.
- `crates/trusted-server-core/src/integrations/prebid.rs` and `aps.rs`.
- `crates/trusted-server-core/src/platform/http.rs` and `platform/mod.rs`.
- `crates/trusted-server-adapter-fastly/src/platform.rs`, `crates/trusted-server-adapter-axum/src/platform.rs`, `crates/trusted-server-adapter-cloudflare/src/platform.rs`, and `crates/trusted-server-adapter-spin/src/platform.rs`.
- `crates/trusted-server-core/src/settings.rs` and `auction_config_types.rs`.
- `crates/trusted-server-js/lib/src/core/auction.ts`, `core/request.ts`, `core/types.ts`, and `integrations/prebid/index.ts`.
- Corresponding Rust/Vitest tests and `crates/trusted-server-integration-tests/tests/parity.rs`.

---

### Task 1: Define canonical page and request types

**Files:**

- Create: `crates/trusted-server-core/src/auction/request.rs`
- Modify: `crates/trusted-server-core/src/auction/mod.rs`
- Modify: `crates/trusted-server-core/src/auction/types.rs:1-115`
- Test: `crates/trusted-server-core/src/auction/request.rs`

- [ ] **Step 1: Write failing tests for immutable canonical data**

Test transfer of the fresh UUID allocated by Phase 1 admission, public
origin/page/referer normalization, fragment removal, telemetry path query removal,
same-origin enforcement, and independence from EC ID. Construct two admissions for
the same EC and assert their IDs differ; the request builder must preserve each input
ID exactly rather than generate a replacement.

```rust
#[test]
fn canonical_request_preserves_fresh_admission_id() {
    let first_admission = admitted_attempt_with_ec("ec-value");
    let second_admission = admitted_attempt_with_ec("ec-value");
    let first = build_canonical_request(&first_admission, input_with_ec("ec-value"))
        .expect("should build first request");
    let second = build_canonical_request(&second_admission, input_with_ec("ec-value"))
        .expect("should build second request");

    assert_ne!(first.auction_id, second.auction_id);
    assert_eq!(first.auction_id, first_admission.auction_id);
    assert_ne!(first.auction_id.to_string(), "ec-value");
}
```

- [ ] **Step 2: Run the focused test and verify RED**

Run: `cargo test-axum canonical_request_ -- --nocapture`

Expected: compilation fails because canonical request types do not exist.

- [ ] **Step 3: Implement the minimal domain types**

```rust
pub struct CanonicalPage {
    pub publisher_origin: url::Url,
    pub page_url: url::Url,
    pub telemetry_path: String,
    pub referer: Option<url::Url>,
}

pub struct CanonicalAuctionRequest {
    pub auction_id: uuid::Uuid,
    pub source: AuctionSource,
    pub page: CanonicalPage,
    pub publisher_domain: String,
    pub account_id: Option<String>,
    pub user: UserInfo,
    pub device: Option<DeviceInfo>,
    pub slots: Vec<ValidatedAdSlot>,
    pub context: BTreeMap<String, ContextValue>,
    pub currency: Currency,
}
```

Copy `auction_id` and `source` from `AuctionAdmission`; this builder must never
regenerate an attempt identity. Use a USD-only `Currency` enum/newtype and `BTreeMap`
wherever deterministic serialization matters. Keep compatibility conversions temporary
and private.

- [ ] **Step 4: Run focused tests and verify GREEN**

Run: `cargo test-axum canonical_request_ -- --nocapture`

Expected: canonical-page and auction-ID tests pass.

- [ ] **Step 5: Commit the canonical request types**

```bash
git add crates/trusted-server-core/src/auction/request.rs crates/trusted-server-core/src/auction/mod.rs crates/trusted-server-core/src/auction/types.rs
git commit -m "Add canonical auction request types"
```

### Task 2: Add the versioned client request contract and real page URL

**Files:**

- Modify: `crates/trusted-server-core/src/auction/formats.rs:30-100`
- Modify: `crates/trusted-server-js/lib/src/core/auction.ts:10-115`
- Modify: `crates/trusted-server-js/lib/src/core/request.ts:20-60`
- Modify: `crates/trusted-server-js/lib/src/integrations/prebid/index.ts:500-535`
- Test: Rust format tests and `crates/trusted-server-js/lib/test/core/auction.test.ts`

- [ ] **Step 1: Add failing Rust/TypeScript tests**

Assert that new direct-core and Prebid-adapter payloads include `version: 2` and
`pageUrl: window.location.href`. Assert Rust treats a missing version as version 1
during migration, parses both version 1 and version 2 through explicit branches,
and rejects unsupported versions. For the page URL, assert Rust strips fragments
and rejects cross-origin, credential-bearing, malformed, and over-2048-byte values.
Test migration fallback order: validated body `pageUrl`, same-origin `Referer`,
publisher root.

- [ ] **Step 2: Run both suites and verify RED**

Run:

```bash
cargo test-axum page_url -- --nocapture
cd crates/trusted-server-js/lib
npx vitest run test/core/auction.test.ts
```

Expected: current client payload omits `pageUrl` and Rust uses the publisher root.

- [ ] **Step 3: Implement the versioned wire fields**

```ts
export interface AdRequest {
  version: 2
  pageUrl: string
  adUnits: AdRequestUnit[]
  config?: Record<string, unknown>
  eids?: AuctionEid[]
}
```

Model the Rust wire field as `version: Option<u8>` and dispatch explicitly:
`None | Some(1)` follows the compatibility parser, `Some(2)` follows the canonical
parser, and every other value is a typed bad request. The TypeScript writer always
emits the literal version 2. Have `buildAdRequest` receive an explicit page URL
rather than reading globals internally, so tests and non-window callers stay
deterministic.

- [ ] **Step 4: Run both suites and verify GREEN**

Run the commands from Step 2.

Expected: page URL and migration tests pass in Rust and Vitest.

- [ ] **Step 5: Commit the client wire revision**

```bash
git add crates/trusted-server-core/src/auction/formats.rs crates/trusted-server-js/lib/src/core/auction.ts crates/trusted-server-js/lib/src/core/request.ts crates/trusted-server-js/lib/src/integrations/prebid/index.ts crates/trusted-server-js/lib/test
git commit -m "Carry canonical page in auction requests"
```

### Task 3: Validate slots and preserve typed economics/context

**Files:**

- Create: `crates/trusted-server-core/src/auction/validation.rs`
- Modify: `crates/trusted-server-core/src/auction/formats.rs:56-200`
- Modify: `crates/trusted-server-core/src/auction/context.rs`
- Modify: `crates/trusted-server-js/lib/src/core/auction.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/prebid/index.ts`
- Test: `crates/trusted-server-core/src/auction/validation.rs`
- Test: `crates/trusted-server-js/lib/test/core/auction.test.ts`
- Test: `crates/trusted-server-js/lib/test/integrations/prebid/index.test.ts`

- [ ] **Step 1: Write failing validation tables**

Cover blank/oversized/duplicate IDs, missing formats, zero or overflowing dimensions, duplicate format tuples, unknown media type, NaN/infinite/negative floors, duplicate/blank bidder names, over-limit bidder JSON, disallowed targeting/context keys, and valid multi-size banner/native/video preservation.
Lock every canonical limit at its boundary and one-over value: 100 ad units; 20 formats
per slot; 50 bidders per slot; 128-byte bidder names; 16 KiB bidder params; 64 targeting
entries with 64-byte keys and 4 KiB serialized values; 32 context entries; 1 KiB
context text; and 100-item context string lists with 256-byte items. Verify targeting
accepts only scalar strings/finite numbers/booleans or arrays of them and rejects nested
objects.

- [ ] **Step 2: Run validation tests and verify RED**

Run: `cargo test-axum validated_ad_slot -- --nocapture`

Expected: current conversion accepts at least empty IDs, duplicates, or zero sizes.

- [ ] **Step 3: Implement validated slot construction**

```rust
pub struct ValidatedAdSlot {
    pub id: SlotId,
    pub formats: Vec<AdFormat>,
    pub floor_usd: Option<FiniteNonNegativeF64>,
    pub targeting: BTreeMap<String, TargetingValue>,
    pub bidders: BTreeMap<String, serde_json::Value>,
}

pub fn validate_slots(
    raw: Vec<RawAdSlot>,
    limits: &AuctionInputLimits,
) -> Result<Vec<ValidatedAdSlot>, Report<TrustedServerError>>;
```

Use strong types and total validation; do not silently coerce unsupported media to banner. Extend TS wire types to carry floor, targeting, media requests, and bounded typed context without accepting arbitrary `ortb2` objects.

- [ ] **Step 4: Run Rust and Vitest validation tests and verify GREEN**

Run:

```bash
cargo test-axum validated_ad_slot -- --nocapture
cd crates/trusted-server-js/lib
npx vitest run test/core/auction.test.ts test/integrations/prebid/index.test.ts
```

Expected: all accepted/rejected fixture cases match.

- [ ] **Step 5: Commit slot validation**

```bash
git add crates/trusted-server-core/src/auction/validation.rs crates/trusted-server-core/src/auction/formats.rs crates/trusted-server-core/src/auction/context.rs crates/trusted-server-js/lib/src crates/trusted-server-js/lib/test
git commit -m "Validate canonical auction slots"
```

### Task 4: Build all entry paths through one request builder

**Files:**

- Modify: `crates/trusted-server-core/src/auction/request.rs`
- Modify: `crates/trusted-server-core/src/auction/endpoints.rs:180-285`
- Modify: `crates/trusted-server-core/src/publisher.rs:1400-1515,2195-2320`
- Modify: `crates/trusted-server-core/src/creative_opportunities.rs`
- Test: request module and path-parity fixtures

- [ ] **Step 1: Add failing equivalent-input path fixtures**

For the same logical page, user, device, slot, floor, targeting, bidders, and context, build through initial navigation, page-bids, and `/auction`. Ignore only the declared source and fresh UUID; assert every other canonical field equals.

- [ ] **Step 2: Run path fixture and verify RED**

Run: `cargo test-axum equivalent_paths_build_same_request -- --nocapture`

Expected: page URL, floors, account ID, context, or request snapshots differ.

- [ ] **Step 3: Introduce path input adapters and delete duplicate builders**

```rust
pub enum AuctionPathInput<'a> {
    InitialNavigation(InitialNavigationInput<'a>),
    PageBids(PageBidsInput<'a>),
    AuctionApi(AuctionApiInput<'a>),
}

pub fn build_canonical_request(
    admission: &AuctionAdmission,
    identity: AuctionIdentity,
    input: AuctionPathInput<'_>,
) -> Result<CanonicalAuctionRequest, Report<TrustedServerError>>;
```

Snapshot headers/device/page data before split dispatch. Remove provider access to path-specific mutable request data where the canonical snapshot supplies it.
Copy canonical page, `ConsentContext`, decision reason, and request/device metadata from
the immutable `AuctionAdmission`; do not re-read headers, cookies, forwarded values, or
consent in this builder. The supplied `AuctionIdentity` has already been gated by that
same admission.

- [ ] **Step 4: Run path fixture and verify GREEN**

Run: `cargo test-axum equivalent_paths_build_same_request -- --nocapture`

Expected: equivalent normalized fields match.

- [ ] **Step 5: Commit the shared builder migration**

```bash
git add crates/trusted-server-core/src/auction/request.rs crates/trusted-server-core/src/auction/endpoints.rs crates/trusted-server-core/src/publisher.rs crates/trusted-server-core/src/creative_opportunities.rs
git commit -m "Share canonical auction request builder"
```

### Task 5: Serialize complete canonical fields to PBS

**Files:**

- Modify: `crates/trusted-server-core/src/integrations/prebid.rs:1400-1690`
- Modify: `crates/trusted-server-core/src/openrtb.rs`
- Create: `crates/trusted-server-integration-tests/fixtures/auction/ts-to-pbs-request.json`
- Create: `crates/trusted-server-integration-tests/fixtures/auction/pbs-to-openrtb26-bidder-request.json`
- Modify: `crates/trusted-server-integration-tests/tests/parity.rs`
- Test: Prebid integration unit tests

- [ ] **Step 1: Add failing OpenRTB mapping tests**

Assert current page/ref, `site.publisher.id`, floor/floor currency, media formats,
device snapshot, USD, consent/EIDs, typed context, effective `tmax`, and inline/stored
bidder parameters. Assert raw TS-to-PBS placement remains `user.ext.eids` and
`imp.ext.prebid.bidder`, PBS's OpenRTB 2.6 bidder fixture normalizes those to
`user.eids` and `imp.ext.bidder`, and downstream headers contain no `ts-ec`,
`ts-eids`, or publisher cookies.
The second shape is a pinned PBS-owned normalization contract fixture, not output from
Trusted Server. The test must compare TS serialization only to the first fixture and
must never add PBS's downstream transformation to TS code.

- [ ] **Step 2: Run Prebid tests and verify RED**

Run:

```bash
cargo test-axum to_openrtb_ -- --nocapture
cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity identity_wire_hops -- --nocapture
```

Expected: account ID and at least client-path floor/page assertions fail.

- [ ] **Step 3: Map only canonical fields**

Make `to_openrtb` accept `&CanonicalAuctionRequest`. Remove request reconstruction and config-derived substitutes. Keep USD explicit and omit `bidfloorcur` when no floor exists. Continue dual placement of consent only where the approved forwarding mode requires it.

- [ ] **Step 4: Run Prebid tests and verify GREEN**

Run the commands from Step 2.

Expected: all OpenRTB mapping and wire-hop tests pass.

- [ ] **Step 5: Commit provider serialization parity**

```bash
git add crates/trusted-server-core/src/integrations/prebid.rs crates/trusted-server-core/src/openrtb.rs crates/trusted-server-integration-tests/fixtures/auction/ts-to-pbs-request.json crates/trusted-server-integration-tests/fixtures/auction/pbs-to-openrtb26-bidder-request.json crates/trusted-server-integration-tests/tests/parity.rs
git commit -m "Serialize canonical auction fields to Prebid"
```

### Task 6: Declare provider and adapter capabilities at startup

**Files:**

- Create: `crates/trusted-server-core/src/platform/capabilities.rs`
- Modify: `crates/trusted-server-core/src/platform/mod.rs` and `http.rs`
- Modify: `crates/trusted-server-adapter-fastly/src/platform.rs`
- Modify: `crates/trusted-server-adapter-axum/src/platform.rs`
- Modify: `crates/trusted-server-adapter-cloudflare/src/platform.rs`
- Modify: `crates/trusted-server-adapter-spin/src/platform.rs`
- Modify: `crates/trusted-server-core/src/auction/provider.rs`
- Modify: `crates/trusted-server-core/src/auction/mod.rs`
- Modify: `crates/trusted-server-core/src/integrations/registry.rs`
- Modify: `crates/trusted-server-core/src/settings.rs`
- Test: `crates/trusted-server-core/src/platform/capabilities.rs`
- Test: `crates/trusted-server-core/src/settings.rs`
- Test: the four adapter `src/platform.rs` unit-test modules

- [ ] **Step 1: Write failing configuration tests**

Reject multi-provider fan-out on sequential adapters, unsupported media routed to a
provider, missing KV when configuration requires it, duplicate providers, mediator
also listed as bidder, and the same bidder configured both server- and client-side.
Also reject globally duplicated creative IDs, div IDs, GAM paths, and provider routing
IDs used as map keys, including APS `slotID`. Accept explicitly degraded
configurations only when the spec permits them.

- [ ] **Step 2: Run startup tests and verify RED**

Run: `cargo test-axum auction_capabilities -- --nocapture`

Expected: unsupported configurations currently build or fail only at request time.

- [ ] **Step 3: Implement capability records and startup validation**

```rust
pub struct AdapterCapabilities {
    pub concurrent_dispatch: bool,
    pub background_work: bool,
    pub kv_identity: bool,
    pub trusted_client_ip: bool,
    pub trusted_geo: bool,
    pub trusted_forwarded_headers: bool,
}

pub struct ProviderCapabilities {
    pub media: BTreeSet<MediaType>,
    pub requires_mediation: bool,
    pub supports_split_dispatch: bool,
}
```

Validate once while building the app/orchestrator. Runtime checks remain defense in depth, not the primary failure mode. Correct APS's declared/requested media support to match actual implementation.

- [ ] **Step 4: Run startup and adapter tests and verify GREEN**

Run:

```bash
cargo test-axum auction_capabilities -- --nocapture
cargo test-fastly auction_capabilities -- --nocapture
cargo test-cloudflare auction_capabilities -- --nocapture
cargo test-spin auction_capabilities -- --nocapture
```

Expected: supported configurations start and unsupported ones fail with typed configuration errors.

- [ ] **Step 5: Commit capability validation**

```bash
git add crates/trusted-server-core/src/platform crates/trusted-server-core/src/auction crates/trusted-server-core/src/integrations crates/trusted-server-core/src/settings.rs crates/trusted-server-adapter-fastly/src/platform.rs crates/trusted-server-adapter-axum/src/platform.rs crates/trusted-server-adapter-cloudflare/src/platform.rs crates/trusted-server-adapter-spin/src/platform.rs
git commit -m "Validate auction capabilities at startup"
```

### Task 7: Preserve partial provider outcomes and normalize no-bid behavior

**Files:**

- Create: `crates/trusted-server-core/src/auction/outcome.rs`
- Modify: `crates/trusted-server-core/src/auction/orchestrator.rs:225-1120`
- Modify: `crates/trusted-server-core/src/auction/provider.rs`
- Modify: `crates/trusted-server-core/src/integrations/prebid.rs` and `aps.rs`
- Test: orchestrator/provider test modules

- [ ] **Step 1: Write failing outcome tests**

Cover `204` as no-bid, one launch failure plus one successful provider, all launch
failures, timeout/no-response, mediator failure with direct bids retained, and
split/synchronous equivalence. Assert no provider result is erased merely because
another stage fails. For mediated winners, assert the clearing price remains
distinct from the original provider price.
Use a table covering every failure kind: `LaunchFailed`, `TimedOut`,
`TransportFailed`, `HttpError { status }`, `InvalidResponse`, and
`RejectedByCapability`, alongside `NoBid` and `Bids`.

- [ ] **Step 2: Run orchestrator tests and verify RED**

Run: `cargo test-axum partial_provider -- --nocapture`

Expected: mediator or all-launch failure behavior differs from the canonical expected outcome.

- [ ] **Step 3: Implement structured provider outcomes**

```rust
pub enum ProviderOutcome {
    Bids { provider: String, bids: Vec<BidCandidate>, elapsed_ms: u64 },
    NoBid { provider: String, elapsed_ms: u64 },
    Failed { provider: String, kind: ProviderFailure, elapsed_ms: u64 },
}

pub struct AuctionOutcome {
    pub auction_id: uuid::Uuid,
    pub source: AuctionSource,
    pub request_summary: CanonicalRequestSummary,
    pub providers: Vec<ProviderOutcome>,
    pub candidates: Vec<BidCandidate>,
    pub winners: BTreeMap<SlotId, BidCandidate>,
    pub mediator: Option<ProviderOutcome>,
    pub rejections: Vec<CandidateRejection>,
    pub total_time_ms: u64,
}
```

Make launch failures first-class in both dispatch styles. Provider response parsers map HTTP `204` before JSON parsing.
`BidCandidate` must carry the configured `provider_rank`, canonical
`clearing_price_usd`, and optional `original_provider_price_usd`; mediation changes
the clearing price without destroying provider-price provenance. It is the Phase 2
validated selection model. Phase 3 Task 1 evolves its preserved provider fields into
`CanonicalBid` and makes `BidCandidate` own that value; it does not create a parallel
winner model. Populate `AuctionOutcome` identity/source/summary/rejections here so all
later projections and telemetry consume the outcome alone.

- [ ] **Step 4: Run outcome tests and verify GREEN**

Run: `cargo test-axum partial_provider -- --nocapture`

Expected: partial success and no-bid cases pass in both dispatch styles.

- [ ] **Step 5: Commit structured outcomes**

```bash
git add crates/trusted-server-core/src/auction/outcome.rs crates/trusted-server-core/src/auction/orchestrator.rs crates/trusted-server-core/src/auction/provider.rs crates/trusted-server-core/src/integrations
git commit -m "Preserve partial auction outcomes"
```

### Task 8: Validate candidates and select deterministic winners

**Files:**

- Modify: `crates/trusted-server-core/src/auction/validation.rs`
- Modify: `crates/trusted-server-core/src/auction/outcome.rs`
- Modify: `crates/trusted-server-core/src/auction/orchestrator.rs`
- Test: validation/outcome modules

- [ ] **Step 1: Write failing economics tests**

Reject unknown impression IDs, non-finite/non-positive prices, non-USD bids,
unsupported media/dimensions, and missing render source. Include a multi-size banner
whose valid bid matches a non-first configured size. Test below-floor top bid
with above-floor runner-up, equal prices arriving in opposite orders, configured
provider order followed by bidder/seat and original bid ID tie keys, and APS encoded
prices being ineligible without mediation.

- [ ] **Step 2: Run economics tests and verify RED**

Run: `cargo test-axum deterministic_winner -- --nocapture`

Expected: current completion-order or post-selection floor behavior fails.

- [ ] **Step 3: Validate before comparison and implement total ordering**

```rust
fn compare_candidates(left: &BidCandidate, right: &BidCandidate) -> Ordering {
    left.clearing_price_usd
        .total_cmp(&right.clearing_price_usd)
        .then_with(|| right.provider_rank.cmp(&left.provider_rank))
        .then_with(|| right.bidder_or_seat.cmp(&left.bidder_or_seat))
        .then_with(|| right.original_bid_id.cmp(&left.original_bid_id))
}
```

Document that this comparator returns `Greater` for the preferred candidate:
higher clearing price, then lower configured provider rank, then lexicographically
smaller bidder/seat, then lexicographically smaller original bid ID. Use this one
function in direct and mediated fallback selection. Filter invalid/below-floor
candidates first; never substitute provider-name lexicographic order for configured
provider order.

- [ ] **Step 4: Run economics tests and verify GREEN**

Run: `cargo test-axum deterministic_winner -- --nocapture`

Expected: arrival order no longer changes winners and eligible runners-up survive.

- [ ] **Step 5: Commit deterministic economics**

```bash
git add crates/trusted-server-core/src/auction/validation.rs crates/trusted-server-core/src/auction/outcome.rs crates/trusted-server-core/src/auction/orchestrator.rs
git commit -m "Validate bids before deterministic selection"
```

### Task 9: Add cross-path normalized fixtures and run the Phase 2 gate

**Files:**

- Create: `crates/trusted-server-integration-tests/fixtures/auction/canonical-banner-v2.json`
- Create: `crates/trusted-server-integration-tests/fixtures/auction/invalid-cases-v2.json`
- Modify: `crates/trusted-server-integration-tests/tests/parity.rs`
- Modify: `docs/guide/auction-orchestration.md`
- Modify: `docs/guide/configuration.md`
- Modify: `docs/guide/integrations/prebid.md`
- Modify: `docs/guide/integrations/aps.md`

- [ ] **Step 1: Add fixture-driven path and adapter tests**

Include floors, targeting, account ID, page/ref, multiple bidders, identity, a provider
failure, `204`, equal prices, below-floor bids, duplicate creative/div/GAM/routing IDs,
and a multi-size banner selecting a non-first valid size. Compare canonical
requests/outcomes, not path-specific response envelopes.

- [ ] **Step 2: Run the parity fixture and verify behavior**

Run: `cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity canonical_auction -- --nocapture`

Expected: all host-runnable adapters pass; Fastly coverage remains in `cargo test-fastly`.

- [ ] **Step 3: Update configuration documentation and migration examples**

Document request version 2, required page URL behavior, floors/targeting limits, account ID semantics, provider/media capabilities, overlap errors, and USD-only policy.

- [ ] **Step 4: Run the complete Phase 2 verification set**

Run all target-matched Rust tests/clippy, the parity integration test, JS build/Vitest/format, and docs format/build commands listed in the master spec section 18.3.

Expected: every command exits zero.

- [ ] **Step 5: Commit fixtures and docs**

```bash
git add crates/trusted-server-integration-tests docs
git commit -m "Lock canonical auction economics parity"
```

---

## Phase 2 completion checkpoint

Do not start response/render work until fixtures demonstrate that equivalent path inputs produce equivalent canonical requests and deterministic winners, partial provider failures survive, `204` is a no-bid, unsupported configurations fail at startup, and no path silently drops page, floor, targeting, context, account, or identity data.
