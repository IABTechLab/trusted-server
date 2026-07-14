# Auction Parity Phase 1: Security and Identity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make every auction entry path enforce the same admission, privacy, identity, cookie-forwarding, logging, and browser-consent boundary before provider work begins.

**Architecture:** Add core-owned admission and identity modules, then make `/auction`, page-bids, and initial navigation consume those decisions. Adapters remain transport shims, PBS receives an allowlisted consent-cookie header plus consent-gated OpenRTB identity, and browser code receives a fail-closed auction/identity decision without receiving KV-resolved EIDs.

**Tech Stack:** Rust 2024, `http`, `serde`, `url`, `error-stack`, EdgeZero adapter routers, TypeScript, Prebid.js, Vitest, VitePress.

**Reference spec:** `docs/superpowers/specs/2026-07-14-server-client-auction-parity-design.md` sections 7.1-7.3, 8, 14.4, 17 Phase 1, and 18.

**Depends on:** Commit `26df29738` or a later commit containing the frozen parity spec.

**Execution workspace:** Use the existing `feat/auction-parity-foundation` branch; do not create a worktree unless the user changes that decision.

---

## File map

### Create

- `crates/trusted-server-core/src/auction/admission.rs` — typed source, request admission, canonical public origin checks, error precedence, and response privacy helper.
- `crates/trusted-server-core/src/auction/identity.rs` — bounded client EID parsing, cookie fallback, KV resolution, deterministic merge, and consent gating.
- `crates/trusted-server-core/src/auction/logging.rs` — redacted provider request/response summaries.

### Modify

- `crates/trusted-server-core/src/auction/mod.rs` — export the three new modules.
- `crates/trusted-server-core/src/auction/telemetry.rs` — import the shared `AuctionSource` after relocating its existing definition.
- `crates/trusted-server-core/src/auction/endpoints.rs` — use shared admission/identity and enforce the kill switch before provider work.
- `crates/trusted-server-core/src/auction/formats.rs` — remove browser EID response headers and attach privacy headers to every response.
- `crates/trusted-server-core/src/auction/types.rs` — correct `UserInfo` identity provenance documentation.
- `crates/trusted-server-core/src/publisher.rs` — replace page-bids-specific admission and identity logic with shared helpers; pass browser permissions to HTML processing.
- `crates/trusted-server-core/src/cookies.rs` — construct an allowlisted outbound consent-cookie header.
- `crates/trusted-server-core/src/integrations/prebid.rs` — use cookie allowlisting, redacted logs, and inject auction/identity permission.
- `crates/trusted-server-core/src/html_processor.rs` — carry immutable auction/identity decisions into integration head injectors.
- `crates/trusted-server-core/src/integrations/registry.rs` — expose those decisions through `IntegrationHtmlContext`.
- `crates/trusted-server-core/src/constants.rs` and `crates/trusted-server-core/src/ec/eids.rs` — delete response-only EID header machinery once compiler-confirmed unused.
- `crates/trusted-server-core/src/ec/finalize.rs` — retain defensive stripping by literal name or remove dead references after producers disappear.
- `crates/trusted-server-adapter-fastly/src/app.rs`, `crates/trusted-server-adapter-axum/src/app.rs`, `crates/trusted-server-adapter-cloudflare/src/app.rs`, and `crates/trusted-server-adapter-spin/src/app.rs` — make route adapters pass trusted request metadata and use core admission results.
- Inline tests in `crates/trusted-server-adapter-fastly/src/app.rs`, plus `crates/trusted-server-adapter-axum/tests/routes.rs`, `crates/trusted-server-adapter-cloudflare/tests/routes.rs`, and `crates/trusted-server-adapter-spin/tests/routes.rs` — adapter-specific route assertions.
- `crates/trusted-server-integration-tests/tests/parity.rs` — cross-adapter admission and privacy parity.
- `crates/trusted-server-js/lib/src/integrations/prebid/index.ts` — fail-closed `requestBids` and EID persistence behavior.
- `crates/trusted-server-js/lib/test/integrations/prebid/index.test.ts` — browser consent/identity tests.
- `docs/guide/edge-cookies.md`, `docs/guide/ec-setup-guide.md`, `docs/guide/integrations/prebid.md`, and `docs/guide/integration-guide.md` — deployed identity flow and removed-header migration.

---

### Task 1: Introduce the typed admission contract

**Files:**

- Create: `crates/trusted-server-core/src/auction/admission.rs`
- Modify: `crates/trusted-server-core/src/auction/mod.rs`
- Modify: `crates/trusted-server-core/src/auction/telemetry.rs`
- Test: `crates/trusted-server-core/src/auction/admission.rs`

- [ ] **Step 1: Write failing unit tests for precedence and public-origin validation**

Cover `413` advertised length, `415` media type, `403`
origin/custom-header/fetch metadata, same-origin HTTPS, and localhost HTTP. Use a
table-driven test whose expected denial is the first applicable header/origin rule.
Assert every admitted or denied attempt receives a fresh UUID that is independent of
EC identity and remains available on the returned value. For admitted/skipped attempts,
assert normalized page/origin, `ConsentContext`, request metadata, three distinct
auction/identity/EID decisions, and the typed decision reason are snapshotted once and
cannot change when the original request object is later mutated.

```rust
#[test]
fn admission_rejects_origin_before_parsing_json() {
    let request = request_with(
        "POST",
        "/auction",
        &[("origin", "https://evil.example"), ("content-type", "application/json")],
        b"not-json",
    );

    let denial = admit_auction_http(&settings(), AuctionSource::AuctionApi, &request)
        .expect_err("should reject cross-origin request");

    assert_eq!(denial.status(), StatusCode::FORBIDDEN);
}
```

- [ ] **Step 2: Run the focused test and verify RED**

Run: `cargo test-axum admission_ -- --nocapture`

Expected: compilation fails because `auction::admission` and `admit_auction_http` do not exist.

- [ ] **Step 3: Implement the minimal admission types and header-only gate**

Relocate the existing `AuctionSource` definition from `auction/telemetry.rs` to
`auction/admission.rs`, then have telemetry import the shared enum. Implement the
remaining public types with no provider dependencies. The header-only function returns
an `AuctionAdmissionDraft`; the snippet below is the finalized shape produced after
Task 2's bounded body/path validation and before identity/provider work:

```rust
pub enum AuctionSource {
    InitialNavigation,
    SpaNavigation,
    AuctionApi,
}

pub struct AuctionAdmission {
    pub auction_id: uuid::Uuid,
    pub source: AuctionSource,
    pub publisher_origin: url::Url,
    pub page_url: url::Url,
    pub telemetry_path: String,
    pub consent: ConsentContext,
    pub request_metadata: RequestMetadataSnapshot,
    pub auction_enabled: bool,
    pub request_allowed: bool,
    pub auction_allowed: bool,
    pub identity_allowed: bool,
    pub eids_allowed: bool,
    pub decision_reason: Option<AuctionDecisionReason>,
}

pub struct AdmissionDenial {
    pub auction_id: uuid::Uuid,
    pub source: AuctionSource,
    pub telemetry_path: Option<String>,
    pub kind: AdmissionDenialKind,
}

pub enum AdmissionDenialKind {
    PayloadTooLarge,
    UnsupportedMediaType,
    ForbiddenOrigin,
    InvalidBody,
}
```

Allocate the attempt UUID at the very start of admission, before any rule can reject or
skip work; both `AuctionAdmission` and `AdmissionDenial` carry that same attempt ID.
Snapshot normalized page/origin, consent, user agent, language, DNT/GPC, trusted client
IP/geo, referer, and allowlisted forwarded metadata once. Store distinct auction,
identity, and EID permissions plus the typed reason even when a valid request is skipped
by the kill switch or consent. Keep body collection and JSON parsing outside this pure
header gate. `AuctionAdmissionDraft` owns the UUID/source/origin/consent/metadata and
decisions available from trusted request data; `finalize_admission(draft,
normalized_page)` consumes it after bounded wire parsing, retains the same UUID and
snapshots, and sets final request/page validation state. No caller may construct the
final struct directly. Expose a `MAX_AUCTION_BODY_BYTES: usize = 256 * 1024` constant
and a denial-to-response mapper.

- [ ] **Step 4: Run the test and verify GREEN**

Run: `cargo test-axum admission_ -- --nocapture`

Expected: all admission unit tests pass.

- [ ] **Step 5: Commit the admission primitive**

```bash
git add crates/trusted-server-core/src/auction/admission.rs crates/trusted-server-core/src/auction/mod.rs crates/trusted-server-core/src/auction/telemetry.rs
git commit -m "Add shared auction admission contract"
```

### Task 2: Apply admission and response privacy to `/auction`

**Files:**

- Modify: `crates/trusted-server-core/src/auction/endpoints.rs:108`
- Modify: `crates/trusted-server-core/src/auction/formats.rs:227`
- Modify: `crates/trusted-server-js/lib/src/core/auction.ts:150-190`
- Modify: `crates/trusted-server-js/lib/src/integrations/prebid/index.ts:236-242,514-530`
- Test: `crates/trusted-server-core/src/auction/endpoints.rs`
- Test: `crates/trusted-server-js/lib/test/core/auction.test.ts`
- Test: `crates/trusted-server-js/lib/test/integrations/prebid/index.test.ts`

- [ ] **Step 1: Write failing endpoint tests**

Add tests proving:

- custom header `X-TSJS-Auction: 1` is mandatory;
- `Sec-Fetch-Site: cross-site` and a mismatched `Origin` return `403`;
- missing/incorrect JSON content type returns `415`;
- advertised and collected bodies over 256 KiB return `413`;
- malformed admitted JSON returns `400`;
- disabled auctions return an empty `200` only after admission and make zero provider/KV calls;
- the UUID allocated during header admission is retained through body collection,
  parsing, disabled no-bid, and telemetry instead of being regenerated;
- every success, no-bid, denial, and internal error has `Cache-Control: private, no-store` and `Pragma: no-cache`.

- [ ] **Step 2: Run the endpoint and browser tests and verify RED**

Run:

```bash
cargo test-axum handle_auction_ -- --nocapture
cd crates/trusted-server-js/lib
npx vitest run test/core/auction.test.ts test/integrations/prebid/index.test.ts
```

Expected: existing endpoint behavior accepts at least the missing-header or
disabled-before-validation case, and one or both browser callers omit the required
header.

- [ ] **Step 3: Route `/auction` through admission before parsing or provider work**

Add one response decorator and call it from every return path:

```rust
pub fn apply_auction_response_privacy(response: &mut Response<EdgeBody>) {
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    response
        .headers_mut()
        .insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
}
```

Do not add CORS headers. `OPTIONS /auction` must use the shared `403` response and never dispatch.
Collect/parse the bounded request and call `finalize_admission` before the kill switch,
identity resolver, KV, or provider dispatch. Body/path failure converts the draft into
`AdmissionDenial` without allocating another UUID or re-reading consent/metadata.
Have both browser callers send the required header: direct `fetch` adds
`X-TSJS-Auction: 1`, while the custom Prebid adapter returns it through the adapter
request's `options.customHeaders` field.

- [ ] **Step 4: Run endpoint tests and verify GREEN**

Run:

```bash
cargo test-axum handle_auction_ -- --nocapture
cd crates/trusted-server-js/lib
npx vitest run test/core/auction.test.ts test/integrations/prebid/index.test.ts
```

Expected: the new admission/privacy tests pass and both browser paths attach the header.

- [ ] **Step 5: Commit the `/auction` boundary**

```bash
git add crates/trusted-server-core/src/auction/endpoints.rs crates/trusted-server-core/src/auction/formats.rs crates/trusted-server-js/lib/src/core/auction.ts crates/trusted-server-js/lib/src/integrations/prebid/index.ts crates/trusted-server-js/lib/test/core/auction.test.ts crates/trusted-server-js/lib/test/integrations/prebid/index.test.ts
git commit -m "Enforce auction endpoint admission"
```

### Task 3: Unify page-bids admission and adapter route behavior

**Files:**

- Modify: `crates/trusted-server-core/src/publisher.rs:2126-2320`
- Modify: `crates/trusted-server-adapter-fastly/src/app.rs:600-650`
- Modify: `crates/trusted-server-adapter-axum/src/app.rs:380-440`
- Modify: `crates/trusted-server-adapter-cloudflare/src/app.rs:450-510`
- Modify: `crates/trusted-server-adapter-spin/src/app.rs:520-570`
- Test: inline tests in `crates/trusted-server-adapter-fastly/src/app.rs`
- Test: `crates/trusted-server-adapter-axum/tests/routes.rs`
- Test: `crates/trusted-server-adapter-cloudflare/tests/routes.rs`
- Test: `crates/trusted-server-adapter-spin/tests/routes.rs`
- Test: `crates/trusted-server-integration-tests/tests/parity.rs`

- [ ] **Step 1: Add failing cross-adapter fixtures**

Extend parity helpers so requests can carry arbitrary method, headers, and body. For
both routes test valid same-origin, missing custom header, cross-site fetch metadata,
mismatched origin, `OPTIONS`, disabled auction, and spoofed
`Forwarded`/`X-Forwarded-*` values that conflict with adapter-attested
scheme/host/client metadata. Test wrong/missing JSON content type only for
`POST /auction`. Explicitly assert `GET /__ts/page-bids` is admitted without a request
`Content-Type`; its response remains JSON.

- [ ] **Step 2: Run parity tests and verify RED**

Run: `cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity auction_admission -- --nocapture`

Expected: at least one adapter differs in status or privacy headers.

- [ ] **Step 3: Replace path-specific page-bids checks with shared admission**

Use `AuctionSource::SpaNavigation` and `X-TSJS-Page-Bids: 1`. Normalize the supplied path against the trusted publisher origin and reject credentials, fragments after normalization, cross-origin URLs, invalid UTF-8, and values longer than 2048 bytes. Adapter code must only provide platform-attested scheme/host/client metadata.

- [ ] **Step 4: Run adapter and parity tests and verify GREEN**

Run:

```bash
cargo test-axum page_bids
cargo test-fastly page_bids
cargo test-cloudflare page_bids
cargo test-spin page_bids
cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity auction_admission
```

Expected: all applicable tests pass with identical status and privacy headers.

- [ ] **Step 5: Commit route parity**

```bash
git add crates/trusted-server-core/src/publisher.rs crates/trusted-server-adapter-fastly/src/app.rs crates/trusted-server-adapter-axum/src/app.rs crates/trusted-server-adapter-axum/tests/routes.rs crates/trusted-server-adapter-cloudflare/src/app.rs crates/trusted-server-adapter-cloudflare/tests/routes.rs crates/trusted-server-adapter-spin/src/app.rs crates/trusted-server-adapter-spin/tests/routes.rs crates/trusted-server-integration-tests/tests/parity.rs
git commit -m "Unify auction route admission"
```

### Task 4: Extract and share the identity resolver

**Files:**

- Create: `crates/trusted-server-core/src/auction/identity.rs`
- Modify: `crates/trusted-server-core/src/auction/endpoints.rs:228-535`
- Modify: `crates/trusted-server-core/src/publisher.rs:1790-1840`
- Modify: `crates/trusted-server-core/src/auction/types.rs:79-100`
- Test: `crates/trusted-server-core/src/auction/identity.rs`

- [ ] **Step 1: Move existing tests first and add the missing contract cases**

Test body-over-cookie precedence, malformed input, limits, KV miss, no EC, no registry, no KV adapter, multiple UIDs per source, exact dedupe, server metadata precedence, and identity denial. Add one path-parity test that feeds equivalent sources through `/auction` and page-bids resolution.

- [ ] **Step 2: Run focused tests and verify RED**

Run: `cargo test-axum auction::identity -- --nocapture`

Expected: compilation fails until the resolver module is implemented and callers migrate.

- [ ] **Step 3: Implement one identity input and resolver**

```rust
pub struct AuctionIdentityInput<'a> {
    pub admission: &'a AuctionAdmission,
    pub request_eids: Option<&'a serde_json::Value>,
    pub ts_eids_cookie: Option<&'a str>,
    pub kv: Option<&'a KvIdentityGraph>,
    pub registry: Option<&'a PartnerRegistry>,
    pub ec_context: &'a EcContext,
}

pub struct AuctionIdentity {
    pub ec_id: Option<String>,
    pub eids: Option<Vec<Eid>>,
}

pub fn resolve_auction_identity(input: AuctionIdentityInput<'_>) -> AuctionIdentity;
```

Preserve current limits. Parse body EIDs first and use the cookie only when body EIDs
are absent. Resolve KV by the existing EC, merge KV first, then apply
`admission.identity_allowed` and `admission.eids_allowed` exactly once. The resolver
must not recompute consent or permission from raw headers/cookies. Add GPC, US opt-out,
missing-consent fail-closed, and consent-allowed cases across `/auction` and page-bids.
Update `UserInfo` docs to describe body, cookie, and KV provenance.

- [ ] **Step 4: Run identity and caller tests and verify GREEN**

Run: `cargo test-axum auction_eid -- --nocapture`

Expected: resolver and both caller suites pass.

- [ ] **Step 5: Commit shared identity resolution**

```bash
git add crates/trusted-server-core/src/auction/identity.rs crates/trusted-server-core/src/auction/mod.rs crates/trusted-server-core/src/auction/endpoints.rs crates/trusted-server-core/src/publisher.rs crates/trusted-server-core/src/auction/types.rs
git commit -m "Share auction identity resolution"
```

### Task 5: Allowlist consent cookies sent to PBS

**Files:**

- Modify: `crates/trusted-server-core/src/cookies.rs:18-130`
- Modify: `crates/trusted-server-core/src/integrations/prebid.rs:1265-1315`
- Test: `crates/trusted-server-core/src/cookies.rs`
- Test: `crates/trusted-server-core/src/integrations/prebid.rs`

- [ ] **Step 1: Write failing cookie transport tests**

Use an inbound header containing `ts-ec`, `ts-eids`, an auth/session cookie, all four consent cookies, an unknown cookie, and malformed/non-UTF-8 values. Assert:

- `OpenrtbOnly` sends no `Cookie` header;
- `CookiesOnly` and `Both` send only valid `euconsent-v2`, `__gpp`, `__gpp_sid`, and `usprivacy` pairs;
- parsing failure omits the outbound header rather than forwarding the original bytes.

- [ ] **Step 2: Run focused tests and verify RED**

Run: `cargo test-axum copy_request_headers_ -- --nocapture`

Expected: the current full-cookie forwarding assertion fails.

- [ ] **Step 3: Implement allowlist reconstruction**

Replace the strip-list model with an allowlist helper:

```rust
pub fn retain_cookies(cookie_header: &str, cookie_names: &[&str]) -> Option<String>;
```

Only call it when the configured mode includes cookies. Do not forward non-UTF-8 input. Keep body consent fallback for KV/policy-sourced consent in `CookiesOnly` mode.

- [ ] **Step 4: Run cookie and Prebid tests and verify GREEN**

Run: `cargo test-axum cookie -- --nocapture`

Expected: all cookie and Prebid transport tests pass.

- [ ] **Step 5: Commit cookie allowlisting**

```bash
git add crates/trusted-server-core/src/cookies.rs crates/trusted-server-core/src/integrations/prebid.rs
git commit -m "Allowlist consent cookies for Prebid"
```

### Task 6: Remove browser-facing merged-EID response headers

**Files:**

- Modify: `crates/trusted-server-core/src/auction/formats.rs:227-365`
- Modify: `crates/trusted-server-core/src/constants.rs:1-60`
- Modify: `crates/trusted-server-core/src/ec/eids.rs:1-115`
- Modify: `crates/trusted-server-core/src/ec/finalize.rs:20-35`
- Test: `crates/trusted-server-core/src/auction/formats.rs`

- [ ] **Step 1: Invert the existing response-header test**

Rename `response_includes_eid_headers_when_eids_present` and assert both EID headers are absent while provider serialization still contains EIDs.

- [ ] **Step 2: Run the focused test and verify RED**

Run: `cargo test-axum response_omits_eid_headers -- --nocapture`

Expected: fails because `convert_to_openrtb_response` currently inserts `x-ts-eids`.

- [ ] **Step 3: Remove response production and compiler-confirmed dead helpers**

Do not set `ts-eids` from KV and do not add a JS consumer. Retain consent denial defense for any legacy integration-specific header producer, but delete constants/encoders that become globally unused.

- [ ] **Step 4: Run core tests and verify GREEN**

Run: `cargo test-axum response_ -- --nocapture`

Expected: response and EC-finalization tests pass.

- [ ] **Step 5: Commit the response boundary**

```bash
git add crates/trusted-server-core/src/auction/formats.rs crates/trusted-server-core/src/constants.rs crates/trusted-server-core/src/ec/eids.rs crates/trusted-server-core/src/ec/finalize.rs
git commit -m "Stop exposing merged EIDs to browsers"
```

### Task 7: Replace full protocol dumps with redacted summaries

**Files:**

- Create: `crates/trusted-server-core/src/auction/logging.rs`
- Modify: `crates/trusted-server-core/src/integrations/prebid.rs:2025-2055`
- Modify: `crates/trusted-server-core/src/integrations/aps.rs`
- Modify: `crates/trusted-server-core/src/integrations/adserver_mock.rs`
- Test: `crates/trusted-server-core/src/auction/logging.rs`

- [ ] **Step 1: Write a failing redaction test with sentinel secrets**

First run `rg -n 'to_string_pretty|OpenRTB request|response body|request body' crates/trusted-server-core/src` to confirm the three listed provider files remain the complete inventory; if it discovers another provider logger, add its exact path to this task before editing. Build a request containing sentinel EC, EID, IP, consent, bidder parameter, query string, and creative values. Serialize the proposed summary and assert none appear.

- [ ] **Step 2: Run the focused test and verify RED**

Run: `cargo test-axum auction_log_summary -- --nocapture`

Expected: compilation fails because the redacted summary does not exist.

- [ ] **Step 3: Implement and adopt the summary**

```rust
pub struct AuctionLogSummary<'a> {
    pub auction_id: &'a str,
    pub provider: &'a str,
    pub slot_count: usize,
    pub bidder_names: Vec<&'a str>,
    pub body_bytes: usize,
    pub elapsed_ms: Option<u64>,
}
```

Log only this structure at debug level. If a dangerous full-dump switch is retained, give it a separate setting defaulting false, a startup warning, and bounded output; do not reuse `ext.prebid.debug`.

- [ ] **Step 4: Run logging/provider tests and verify GREEN**

Run: `cargo test-axum auction_log_summary -- --nocapture`

Expected: sentinel values never appear.

- [ ] **Step 5: Commit redacted logging**

```bash
git add crates/trusted-server-core/src/auction/logging.rs crates/trusted-server-core/src/auction/mod.rs crates/trusted-server-core/src/integrations
git commit -m "Redact auction provider logging"
```

### Task 8: Inject and enforce browser auction/identity permission

**Files:**

- Modify: `crates/trusted-server-core/src/html_processor.rs:159-215`
- Modify: `crates/trusted-server-core/src/integrations/registry.rs:543-580`
- Modify: `crates/trusted-server-core/src/integrations/prebid.rs:880-925`
- Modify: `crates/trusted-server-core/src/publisher.rs:1400-1510`
- Modify: `crates/trusted-server-js/lib/src/integrations/prebid/index.ts:487-690,814-925`
- Test: Rust Prebid head-injector tests and `crates/trusted-server-js/lib/test/integrations/prebid/index.test.ts`

- [ ] **Step 1: Write failing server and browser tests**

Server tests must assert a JSON object such as the following is escaped and injected
per request, and that a disabled initial-navigation auction allocates an attempt UUID
but makes zero provider calls:

```json
{ "auctionAllowed": false, "identityAllowed": false, "decision": "denied" }
```

Vitest must assert that denied/unknown fail-closed decisions do not call the original `pbjs.requestBids`, do invoke the supplied callback once with an empty outcome, do not call user-ID startup, and clear/avoid `ts-eids`. An allowed decision must preserve existing behavior.

- [ ] **Step 2: Run Rust and Vitest tests and verify RED**

Run:

```bash
cargo test-axum head_injector_emits_auction_decision -- --nocapture
cd crates/trusted-server-js/lib
npx vitest run test/integrations/prebid/index.test.ts
```

Expected: the injected config lacks the decision and denied browser execution still calls Prebid.

- [ ] **Step 3: Thread immutable decisions into the head injector and gate JS**

Add typed booleans/enums to `HtmlProcessorConfig` and `IntegrationHtmlContext`; never let publisher JS override them. In TS, check the injected decision before mutating ad units or calling `getUserIdsAsEids`. Complete callbacks asynchronously but exactly once.

- [ ] **Step 4: Run Rust and Vitest tests and verify GREEN**

Run the commands from Step 2.

Expected: all new permission tests pass.

- [ ] **Step 5: Commit browser permission enforcement**

```bash
git add crates/trusted-server-core/src/html_processor.rs crates/trusted-server-core/src/integrations/registry.rs crates/trusted-server-core/src/integrations/prebid.rs crates/trusted-server-core/src/publisher.rs crates/trusted-server-js/lib/src/integrations/prebid/index.ts crates/trusted-server-js/lib/test/integrations/prebid/index.test.ts
git commit -m "Enforce browser auction identity decisions"
```

### Task 9: Update public identity documentation and run the Phase 1 gate

**Files:**

- Modify: `docs/guide/edge-cookies.md`
- Modify: `docs/guide/ec-setup-guide.md`
- Modify: `docs/guide/integrations/prebid.md`
- Modify: `docs/guide/integration-guide.md`

- [ ] **Step 1: Update the deployed-flow documentation**

Document the three wire hops, EC-as-KV-key semantics, body-over-cookie precedence, server/client merge, adapter KV capability, consent gating, cookie allowlisting, and removal of EID response headers. Remove commands instructing users to decode `x-ts-eids`.

- [ ] **Step 2: Search for stale claims**

Run:

```bash
rg -n "x-ts-eids|KV.*ts-eids|ts-eids.*KV|raw.*Cookie|user\.ext\.eids" docs crates/trusted-server-core/src/auction
```

Expected: only migration/history references remain; downstream OpenRTB 2.6 normalization is explained where relevant.

- [ ] **Step 3: Run formatting and complete target-matched verification**

Run:

```bash
cargo fmt --all -- --check
cargo clippy-fastly
cargo clippy-axum
cargo clippy-cloudflare
cargo clippy-cloudflare-wasm
cargo clippy-spin-native
cargo clippy-spin-wasm
cargo test-fastly
cargo test-axum
cargo test-cloudflare
cargo test-spin
cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity
cd crates/trusted-server-js/lib
node build-all.mjs
npx vitest run
npm run format

cd ../../../docs
npm run format
npm run build
```

Expected: every command exits zero. Record environment failures separately; do not call them passes.

- [ ] **Step 4: Inspect the final diff against Phase 1 findings S1-S7 and R6**

Confirm no unrelated economics, rendering, or telemetry refactor leaked into this phase.

- [ ] **Step 5: Commit documentation/verification adjustments**

```bash
git add docs crates/trusted-server-core crates/trusted-server-js crates/trusted-server-adapter-fastly crates/trusted-server-adapter-axum crates/trusted-server-adapter-cloudflare crates/trusted-server-adapter-spin crates/trusted-server-integration-tests
git commit -m "Publish auction identity security contract"
```

---

## Phase 1 completion checkpoint

Do not start Phase 2 until all of the following are evidenced by tests:

- every auction path rejects the same invalid requests in the same precedence;
- disabled auctions make zero provider and KV calls;
- only allowlisted consent cookies can reach PBS;
- EC and EIDs reach PBS only through consent-gated OpenRTB fields;
- `x-ts-eids` and `x-ts-eids-truncated` are absent from browser responses;
- normal logs cannot contain the sentinel sensitive values; and
- denied browser auctions and identity synchronization fail closed.
