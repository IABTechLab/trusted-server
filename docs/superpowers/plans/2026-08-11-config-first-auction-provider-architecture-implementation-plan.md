# Config-First Auction Provider Architecture Implementation Plan

**Date:** 2026-08-11
**Status:** Draft for implementation review
**Spec:** `docs/superpowers/specs/2026-08-10-config-first-auction-provider-architecture-design.md`
**Implementation baseline:** `main` at `af67d8c2`, including merged APS PR #918

## Baseline and branch requirement

The design-spec branch predates the merged APS OpenRTB implementation. Implementation must start from current `main` at or after `af67d8c2`, not from the current documentation branch's Rust tree. Rebase or create a fresh implementation worktree after the specification is merged so the work preserves the current APS OpenRTB request, response, renderer, diagnostics, and parity tests.

The implementation must not restore the pre-#918 APS `/e/dtb/bid` protocol or use the older encoded-price mediator-only path as its baseline.

## Decisions locked for this plan

- The generic `standard` compatibility endpoint is a fictional local mock available only under automated tests. It adds no runtime endpoint, profile, authentication option, or production-support claim.
- Provider ID, returned upstream seat, and browser-facing delivery bidder code are separate identities.
  - Provider ID owns configuration, backend correlation, health, and telemetry.
  - A valid string `seatbid.seat` is retained as `returned_seat`.
  - Existing `Bid.bidder` remains the delivery bidder code for compatibility.
  - PBS uses the returned seat or `unknown`; APS continues to use `aps`.
- Disabled global signing removes only signature-bearing fields. PBS retains its existing `request_host`/`request_scheme` object; APS and `standard` omit `ext.trusted_server`.
- Enabled global signing loads one auction-local signer before dispatch and applies the existing version 1.1 extension to every OpenRTB provider after profile augmentation.
- On an adapter with a future enforceable total-request deadline, late completions are timeouts and are discarded. No current adapter claims that capability; an already-launched completed response remains eligible, but no further provider or mediator network work starts after logical budget exhaustion.
- Existing mock mediation remains a statically registered, separately selected path. It is not represented by `ProviderPlan`, a profile, or a new generic mediator trait.
- `BTreeMap` or sorted vectors provide deterministic plan, route, request, and validation order. Runtime correctness must not depend on TOML or `HashMap` iteration order.
- Provider IDs use the spec's lowercase ASCII grammar and 63-byte limit.
- First-version bounds introduced by this work are constants with tests:
  - at most 128 `notifications.suppress_seats` entries, each at most 128 UTF-8 bytes;
  - at most 128 bidder entries in one `trustedServer.bidderParams` object;
  - bidder IDs at most 128 UTF-8 bytes and `trustedServer.zone` at most 256 UTF-8 bytes;
  - each static `request_ext` or `imp_ext` object at most 16 KiB serialized, at most eight object/array levels deep, and at most 256 keys at any one object level.
- The existing 256 KiB `/auction` body limit remains authoritative. Header snapshots use the exact first `HeaderMap` value already selected by the request layer and add no truncation behavior.
- No temporary old/new public configuration compatibility mode ships. Internal staging adapters may exist while the branch is under development, but they must be removed before the configuration schema switch is merged.

## Definition of done

- `[auction.providers]` and `[auction.bidders]` are the only server-side bidder-provider inventory and routing source.
- `standard`, `prebid-server`, and `aps` are compiled through one Rust registry into an immutable `AuctionPlan`.
- Deploy validation and every adapter startup use the same target-independent compiler; target-aware paths also run the same adapter capability and backend-name validation.
- Runtime request handling uses normalized auction data, a transport-owned Prebid header snapshot, and filtered `ProviderAuctionInput` values. Profiles do not receive the raw HTTP request or unrestricted runtime services.
- A generic OpenRTB 2.6 driver owns standard fields, common response decoding, request finalization, signing, and common notification suppression.
- PBS and APS singleton bidder-provider registration is removed after parity fixtures pass.
- Multiple instances of one profile dispatch and correlate by provider ID.
- Empty trusted Prebid envelopes, explicit bidder routing, APS `all_eligible`, unknown bidders, no-slot skips, and mixed browser demand follow the specification.
- PBS and APS request, response, privacy, creative, timeout, debug, and diagnostics behavior matches `main`, except for the intentional all-provider signing expansion.
- Provider ID, returned seat, and delivery bidder code remain independent through ranking, mediation, delivery, and telemetry.
- Existing ranking, floors, USD assumptions, mock mediation, creative sanitization, APS rendering, Prebid Cache, and telemetry remain intact.
- Example configuration, operator docs, and browser-injected Prebid configuration use the new ownership model.
- All target-specific tests, JS tests, integration parity tests, formatting, and clippy gates pass.

## Proposed architecture

### Raw and compiled configuration

Add raw serde types under `auction_config_types.rs` and compile them into types that cannot represent unresolved references:

```rust
pub struct AuctionConfig {
    pub enabled: bool,
    pub timeout_ms: u32,
    pub providers: BTreeMap<ProviderId, ProviderConfig>,
    pub bidders: BTreeMap<BidderId, BidderRouteConfig>,
    pub mediator: Option<String>,
}

pub struct AuctionPlan {
    providers: Vec<ProviderPlan>,
    bidder_routes: BTreeMap<BidderId, ProviderIndex>,
    signing_enabled: bool,
}

pub struct ProviderPlan {
    id: ProviderId,
    endpoint: CanonicalProviderEndpoint,
    timeout_ms: u32,
    routing: RoutingMode,
    notifications: NotificationPolicy,
    protocol: ProtocolPlan,
    profile: CompiledOpenRtbProfile,
}
```

Use newtypes for `ProviderId` and `BidderId`. Sort compiled providers by provider ID and store bidder routes as validated provider indexes or IDs. `AuctionPlan` stores only the enabled signing policy; it never stores loaded keys.

The first-version protocol representation is a closed `ProtocolPlan::OpenRtb26` enum. Do not introduce scripting, dynamic loading, or a generalized protocol trait before a second protocol exists.

### Profile registry and dispatch

Use a small compile-time registration table rather than runtime plugins:

```rust
pub struct OpenRtbProfileRegistration {
    id: &'static str,
    default_timeout: ProfileTimeoutDefault,
    compile: fn(&serde_json::Value) -> Result<CompiledOpenRtbProfile, Report<TrustedServerError>>,
}

enum CompiledOpenRtbProfile {
    Standard(StandardProfilePlan),
    PrebidServer(PrebidProfilePlan),
    Aps(ApsProfilePlan),
}
```

The enum supplies typed methods for field policy, request augmentation, request-local parse state, response interpretation, and renderer capabilities. This is intentionally simpler than boxed `Any` state or a public capability-composition system. Adding a repository-owned profile requires a registration entry and enum variant, which is acceptable for the first version.

Keep PBS-specific compilation and behavior in `integrations/prebid.rs` initially and APS-specific behavior in `integrations/aps.rs`. Expose narrow `pub(crate)` registration/compile hooks to the auction module. Do not convert either large integration file into a directory tree solely for this refactor.

### Generic provider execution

Replace singleton bidder providers with one generic planned OpenRTB provider execution path. It owns endpoint selection, backend specification, one-request-per-provider dispatch, common driver invocation, and response association. It dispatches profile behavior through `CompiledOpenRtbProfile` without matching profile names in the orchestrator.

Adapt the existing `AuctionProvider` dispatch/parse seam rather than replacing the split dispatch/collect mechanism wholesale:

- change static provider-name APIs to borrow the validated dynamic provider ID;
- let the generic planned provider carry one `Arc<ProviderPlan>`;
- route and filter slots before invoking it;
- retain request-local profile parse state in the existing pending response token;
- split bidder-provider storage from the statically selected mock mediator so the mediator never enters the compiled plan.

Delete PBS and APS singleton registration only after their planned profiles pass parity tests.

### Admission and execution context

Introduce an admitted request representation that separates canonical data from transport-only data:

```rust
struct AdmittedAuction {
    request: AuctionRequest,
    prebid_headers: PrebidTransportHeaders,
    signer: Option<RequestSigner>,
}

struct ProviderAuctionInput {
    provider_id: ProviderId,
    slots: Vec<ProviderSlotInput>,
    canonical: Arc<CanonicalAuctionData>,
    logical_budget_ms: u32,
}
```

The exact ownership can use references or `Arc` to avoid copying publisher/user/device data. Profiles receive `ProviderAuctionInput`, never the current raw-request-bearing `AuctionContext`. Common transport receives the private Prebid header snapshot. The existing mediator may continue receiving the broader legacy context until a separate mediation design changes it.

Keep timeout values explicit and separate:

- `logical_budget_ms = min(provider_timeout_ms, remaining_auction_ms)` controls launch and OpenRTB `tmax`.
- `transport_timeout_ms = backend.canonicalize_transport_timeout_ms(remaining_auction_ms, provider_timeout_ms)` controls backend identity and the adapter's available transport timers.

Fastly quantization of `transport_timeout_ms` must never replace or shorten `logical_budget_ms`.

### Bid identity

Preserve `Bid.bidder` as the serialized delivery bidder code and add:

```rust
pub returned_seat: Option<String>
```

`AuctionResponse.provider` remains the provider ID. Update comments and constructors so these three identities cannot be accidentally interchanged. Keep `returned_seat` out of unchanged external mediator/client wire shapes where necessary and restore it from the original provider bid after mock mediation. Notification suppression matches only `returned_seat`; response serialization and the APS browser renderer continue to use `Bid.bidder`. The existing telemetry seat carrier uses `returned_seat` when present and falls back to `Bid.bidder` when absent.

### Target validation

Add a pure adapter validation description passed into the compiler's second stage:

```rust
pub struct AuctionTargetCapabilities<'a> {
    pub target_name: &'static str,
    pub supports_concurrent_fanout: bool,
    pub enforces_total_request_deadline: bool,
    pub backend_name_predictor: &'a dyn PlatformBackend,
}
```

Reuse `PlatformBackend::predict_name` for startup validation. Where CLI validation cannot instantiate the runtime backend, extract one shared pure naming helper/descriptor and make both `predict_name` and CLI validation call it; do not create a second independently implemented codec. Cloudflare and Spin skip registration but still use their real deterministic names containing canonical backend-spec fields and the provider discriminator.

Target-independent `ts config validate` runs stage one and emits an explicit message that adapter checks are deferred. Adapter startup always runs both stages.

Target-aware `ts config push --adapter <target>` must validate before any remote read, prompt, or write. Add an EdgeZero typed-push callback API that receives the already deserialized, environment-overlaid `TrustedServerAppConfig` plus the selected adapter ID between EdgeZero's ordinary typed validation and its first remote operation. Trusted Server maps that ID to the same target capability/name-prediction descriptor used at startup and runs stage two in the callback. Pin the EdgeZero revision containing this hook. This keeps config loading, overlays, platform writes, and adapter selection inside EdgeZero while making target validation mandatory rather than duplicating a loader in Trusted Server.

## Stage 1 — Pin behavioral parity before refactoring

Files:

- `crates/trusted-server-core/src/integrations/prebid.rs`
- `crates/trusted-server-core/src/integrations/aps.rs`
- `crates/trusted-server-core/src/auction/orchestrator.rs`
- `crates/trusted-server-core/src/auction/formats.rs`
- `crates/trusted-server-js/lib/test/integrations/prebid/index.test.ts`

Steps:

1. Add or consolidate golden helpers that serialize current PBS and APS requests from the same canonical fixture.
2. Pin PBS behavior for:
   - standard field ownership differences;
   - raw `Referer` in both HTTP forwarding and `site.ref`;
   - `User-Agent`, `Accept-Language`, and platform-attested `X-Forwarded-For`;
   - every Cookie mode, malformed/non-UTF-8 Cookie fallback, and removal-to-empty;
   - TCF, jurisdiction-derived GDPR, USP, GPP/SID, Google Additional Consent, EIDs, and KV/policy body fallback;
   - stored requests, bidder-parameter merge precedence, override rules, test/debug fields, Cache coordinates, and response diagnostics;
   - exact signing-enabled and signing-disabled JSON, including the disabled host/scheme-only object and absence of every signature-bearing key.
3. Pin APS behavior from merged main for:
   - request field differences and account/SDK extensions;
   - inventory identity overrides;
   - consent and EID placement;
   - response shape validation and sibling-bid isolation;
   - renderer envelopes and script policy;
   - highest-price-per-impression reduction and bid-ID tie-breaking;
   - default 800 ms timeout and debug diagnostics;
   - signing-disabled absence of `ext.trusted_server` before the intentional coverage expansion.
4. Pin synchronous and split dispatch/collect behavior, including current late completions and mediator fallback.
5. Use only fictional request, endpoint, account, bidder, seat, and creative data.

This stage establishes the comparison baseline; it must not change production behavior.

Verification:

```bash
cargo test-fastly
cargo test-axum
cd crates/trusted-server-js/lib && npx vitest run
```

## Stage 2 — Add config-first types, profile registry, and plan compiler

Files:

- `crates/trusted-server-core/src/auction_config_types.rs`
- `crates/trusted-server-core/src/auction/mod.rs`
- new `crates/trusted-server-core/src/auction/plan.rs`
- new `crates/trusted-server-core/src/auction/profile.rs`
- internal compiler test modules beside the new plan/profile code

Steps:

1. Add `ProviderId`, `BidderId`, `ProviderConfig`, `BidderRouteConfig`, `RoutingMode`, `NotificationConfig`, and typed common validation.
2. Parse profile configuration as an object and immediately hand it to the selected registered profile compiler. Do not retain untyped `serde_json::Value` in the runtime plan.
3. Add the compile-time `standard`, `prebid-server`, and `aps` registration table independent of browser integration enablement.
4. Implement target-independent compilation:
   - provider ID grammar and uniqueness;
   - only `openrtb-2.6`;
   - profile resolution and typed profile config;
   - HTTPS endpoint parsing, canonicalization, credentials/fragment rejection, and profile-specific endpoint restrictions;
   - profile timeout defaults and explicit overrides;
   - bidder-to-provider resolution;
   - static extension type, size, depth, key-count, and reserved-field ownership checks;
   - notification bounds and duplicate rejection;
   - signing configuration structure;
   - deterministic provider and route order.
5. Keep `[auction].mediator` validation separate and restricted to the existing static mock mediator.
6. Add compiler tests for every rejection, defaults, two same-profile instances, and browser integration independence.

This stage is internal scaffolding only: do not replace `AuctionConfig.providers`, change `Settings`, or wire `validate_settings_for_deploy`. Compiler tests construct raw provider maps directly. Public schema and validation switch together in Stage 11, so no committed state exposes dual schemas or makes deploy validation disagree with the live runtime.

Verification:

```bash
cargo test-fastly
```

## Stage 3 — Add adapter capability and backend-name validation

Files:

- `crates/trusted-server-core/src/platform/traits.rs`
- `crates/trusted-server-core/src/platform/types.rs`
- `crates/trusted-server-core/src/integrations/mod.rs`
- `crates/trusted-server-core/src/auction/plan.rs`
- `crates/trusted-server-adapter-fastly/src/backend.rs`
- `crates/trusted-server-adapter-fastly/src/platform.rs`
- `crates/trusted-server-adapter-fastly/src/app.rs`
- `crates/trusted-server-adapter-axum/src/platform.rs`
- `crates/trusted-server-adapter-axum/src/app.rs`
- `crates/trusted-server-adapter-cloudflare/src/platform.rs`
- `crates/trusted-server-adapter-cloudflare/src/app.rs`
- `crates/trusted-server-adapter-spin/src/platform.rs`
- `crates/trusted-server-cli/src/run.rs`
- `crates/trusted-server-cli` target-validation tests
- root EdgeZero dependency pin/lockfile
- coordinated EdgeZero CLI typed-push callback API

Steps:

1. Add `AuctionTargetCapabilities` and shared pure backend-name prediction input to target validation.
2. Prepare provider ID as every planned `PlatformBackendSpec.discriminator`; production backend naming switches in Stage 11.
3. Reuse or extract the algorithms behind `PlatformBackend::predict_name` so target validation and runtime construction cannot drift:
   - Fastly's canonical backend specification and digest naming;
   - Axum's environment/backend normalization;
   - Cloudflare's deterministic no-registration backend name;
   - Spin's deterministic no-registration backend name.
4. Reject two provider plans whose predicted names collide before either can register or overwrite a correlation map.
5. Declare capabilities from current behavior:
   - Fastly: concurrent fan-out, first-byte/between-byte transport timers, and no enforceable total-request deadline;
   - Axum: concurrent fan-out and no provider-specific total-request deadline;
   - Cloudflare: no concurrent fan-out and no provider-specific total-request deadline;
   - Spin: no concurrent fan-out and no provider-specific total-request deadline.
6. Reject more than one active provider for Cloudflare and Spin at target validation. Do not infer that only one will be requested at runtime.
7. Keep existing runtime collision assertions as defense in depth.
8. Add the EdgeZero typed-push validation callback, update the dependency pin, and test that the callback runs after overlays/typed validation but before any remote read or write.
9. Add unit tests that map each CLI adapter ID to the same capability/prediction descriptor used by startup and validate directly constructed plans. End-to-end map-shaped config-push rejection waits for the public schema cutover in Stage 11.
10. Add target prediction/capability tests and same-profile/same-endpoint independent-correlation tests. Actual adapter startup and target-aware push wiring remains part of the atomic Stage 11 cutover.

Verification:

```bash
cargo test-fastly
cargo test-axum
cargo test-cloudflare
cargo test-spin
./scripts/test-cli.sh
```

## Stage 4 — Normalize admission and route provider-local inputs

Files:

- new `crates/trusted-server-core/src/auction/routing.rs`
- `crates/trusted-server-core/src/auction/types.rs`
- `crates/trusted-server-core/src/auction/formats.rs`
- `crates/trusted-server-core/src/auction/endpoints.rs`
- `crates/trusted-server-core/src/creative_opportunities.rs`
- `crates/trusted-server-core/src/publisher.rs`

Steps:

1. Split canonical slot demand into bidder parameters, trusted provider routes, and bounded Prebid `zone` facts.
2. Normalize the reserved `trustedServer` envelope before central routing:
   - missing/null/empty `bidderParams` produces stored-request intent;
   - non-object, bad key/value, partial malformed, and bound violations reject admission and never fan out stored requests;
   - valid unknown bidders route to `unroutable_bidder` and do not trigger fallback;
   - usable direct objects win collisions, while unusable direct values cannot overwrite usable envelope objects;
   - deterministic merge order is independent of map iteration.
3. Snapshot the first accepted `Cookie`, `User-Agent`, `Referer`, and `Accept-Language` values into a private `PrebidTransportHeaders`. Ignore client-supplied XFF and preserve the platform-attested IP separately.
4. Build one `ProviderAuctionInput` per provider:
   - remove non-banner formats before routing;
   - `explicit` receives only centrally routed or trusted slots;
   - `all_eligible` receives every banner-compatible slot;
   - parameters assigned to another provider are never copied;
   - no valid banner formats means no provider input;
   - no eligible slots produces a skip result without dispatch.
5. Convert creative-opportunity construction:
   - explicit bidder parameters remain bidder IDs and use the central registry;
   - empty Prebid stored-request intent expands to every compiled PBS plan;
   - remove hard-coded APS provider selection and rely on `all_eligible` or explicit central/trusted routing.
6. Use the same admission/router helper for `/auction`, initial navigation, refresh/page-bids, and other server-generated auction entry points.
7. Add table-driven tests for routing, mixed providers, unknown bidders, empty/malformed envelopes, collisions, bounds, non-banner slots, and no parameter leakage.

Verification:

```bash
cargo test-fastly
cargo test-axum
```

## Stage 5 — Build the common OpenRTB driver and test-only standard profile

Files:

- new `crates/trusted-server-core/src/auction/openrtb.rs`
- `crates/trusted-server-core/src/openrtb.rs`
- `crates/trusted-server-core/src/auction/profile.rs`
- `crates/trusted-server-core/src/request_signing/signing.rs`
- `crates/trusted-server-core/src/auction/types.rs`

Steps:

1. Extract common banner request construction into a driver that owns:
   - request/impression IDs and banner formats;
   - site, publisher, user, device, consent, EID, floor, secure, currency, and `tmax` fields;
   - application of a typed per-profile field policy;
   - common request extension ownership and collision checks.
2. Implement fixed standard/PBS/APS field policies. Treat Stage 1 golden fixtures as normative, especially for consent and privacy differences.
3. Implement `StandardProfilePlan` with bounded static `request.ext` and `imp.ext`. It does not invent a wire location for bidder params.
4. Build response decoding around independent bid validation and preserve current profile-specific whole-response versus sibling-bid behavior.
5. Treat response `id` as informational for PBS/APS parity; keep transport association as the correlation boundary.
6. Add `Bid.returned_seat` while retaining `Bid.bidder` as delivery code.
7. Move signing to common finalization:
   - profiles finish augmentation first;
   - finalization freezes request ID and signing-owned fields;
   - enabled signing inserts the existing v1.1 extension;
   - disabled PBS can retain host/scheme only;
   - disabled APS/standard omit the object.
8. Add exact serialized signing-on and signing-off fixtures for all three profiles. Assert each signature-bearing key separately: disabled PBS retains only host/scheme, disabled APS/standard omit the object, and every enabled request contains the full v1.1 extension after augmentation.
9. Add a `#[cfg(test)]` fictional standard endpoint mock using the existing stub HTTP-client machinery. Test ordinary bid, no-bid/204, malformed response isolation, static extensions, signed request compatibility, and no-auth behavior. No fixture handler or endpoint is compiled into production.
10. Add a common-transport redirect fixture: return 3xx with `Location`, assert no second request occurs, classify the original provider outcome, and prove the canonical endpoint used for request dispatch is the same one used for backend prediction/construction.

Verification:

```bash
cargo test-fastly
cargo test-axum
```

## Stage 6 — Build plan-backed orchestrator execution under test construction

Files:

- `crates/trusted-server-core/src/auction/provider.rs`
- `crates/trusted-server-core/src/auction/orchestrator.rs`
- `crates/trusted-server-core/src/auction/mod.rs`
- `crates/trusted-server-core/src/auction/endpoints.rs`

Steps:

1. Change static provider identity APIs to dynamic validated provider IDs.
2. Add one generic planned OpenRTB provider that combines a `ProviderPlan`, common driver, profile enum, and existing platform transport.
3. Add an internal/test constructor through which the orchestrator owns immutable plan-backed bidder providers and routes filtered inputs before launch. Production startup remains on the legacy constructor until Stage 11.
4. Preserve the existing split request/parse token and carry typed profile-local parse state without exposing another provider's state.
5. Prepare split mock mediator registration/lookup from bidder-provider storage. Keep its invocation and fallback unchanged, and restore `returned_seat` from the selected original provider bid rather than deriving it from mediator output.
6. Compute exact `logical_budget_ms` once for launch and `tmax`, then separately compute adapter-canonicalized `transport_timeout_ms` for backend naming/timers. Add Fastly tests proving timeout quantization never changes `tmax`.
7. Load the current signer once during common admission before routing/dispatch. A load failure returns before any `send_async` call. Do not cache signer keys at startup.
8. Ensure one request per eligible provider, no request for skipped providers, and no zero-impression request.
9. Keep provider failures isolated and preserve current local ranking/floor logic after normalization.
10. Add multi-instance tests for two providers with the same profile, endpoint, and timeout, proving independent backend correlation and outcome metadata.

Stages 2 through 10 use internal construction scaffolding and tests without changing the public configuration/runtime path. They are development order, not independently mergeable public migrations. Stage 11 performs the one atomic cutover; the merged result has one config-first runtime path and no public legacy provider list.

Verification:

```bash
cargo test-fastly
cargo test-axum
cargo test-cloudflare
cargo test-spin
```

## Stage 7 — Extract and migrate the Prebid Server profile

Files:

- `crates/trusted-server-core/src/integrations/prebid.rs`
- `crates/trusted-server-core/src/auction/openrtb.rs`
- `crates/trusted-server-core/src/auction/profile.rs`
- `crates/trusted-server-core/src/auction/orchestrator.rs`

Steps:

1. Define typed `PrebidProfileConfig` for debug, test mode, debug query params, override fields/rules, and consent forwarding. Prepare server endpoint, timeout, bidders, and notification suppression for common ownership, but keep the live legacy config fields until Stage 11.
2. Compile the current override engine once into `PrebidProfilePlan`.
3. Move PBS-only request behavior behind the profile:
   - `imp.ext.prebid.bidder` from routed params;
   - stored-request fallback for trusted routes;
   - zone and generic override merge behavior;
   - test/debug request fields and page query behavior;
   - PBS-specific consent and Google Additional Consent policy;
   - disabled-signing host/scheme extension fields.
4. Keep raw HTTP values out of the profile. Common Prebid transport applies the snapshot matrix for Cookie, UA, Referer, Accept-Language, and attested XFF.
5. Move PBS-only response behavior behind the profile:
   - Prebid Cache coordinates;
   - existing bid-status/debug metadata;
   - request-local parse facts;
   - current valid-sibling behavior and delivery bidder fallback.
6. Apply common notification suppression after PBS normalization and before ranking/mediation.
7. Exercise the profile through the internal planned-provider constructor while the live singleton remains unchanged.
8. Run the Stage 1 golden matrix against the new profile and compare serialized requests plus normalized outcomes.
9. Mark singleton registration and static backend discriminator for atomic removal in Stage 11.

Verification:

```bash
cargo test-fastly
cargo test-axum
```

## Stage 8 — Extract and migrate the APS profile

Files:

- `crates/trusted-server-core/src/integrations/aps.rs`
- `crates/trusted-server-core/src/auction/openrtb.rs`
- `crates/trusted-server-core/src/auction/profile.rs`
- `crates/trusted-server-core/src/auction/types.rs`
- APS renderer registration call sites under adapter/core startup

Steps:

1. Define typed `ApsProfileConfig` for account ID, debug, script opt-in, and inventory identity overrides. Prepare endpoint and timeout for common provider ownership, but keep the live legacy config fields until Stage 11.
2. Express current APS request differences as its fixed field policy and owned account/SDK extensions.
3. Preserve current response validation, exact minimized renderer envelope, script gate, creative URL policy, diagnostics, and deterministic candidate reduction.
4. Capture a valid string `seatbid.seat` as `returned_seat`; use no returned seat for missing/non-string values. Keep `Bid.bidder = "aps"` so the existing renderer path remains active.
5. Preserve current APS removal/non-exposure of notification URLs before common configurable suppression.
6. Add a narrow validated-plan query for APS renderer activation and test it through the internal constructor; production registry wiring switches in Stage 11.
7. Make `all_eligible` the parity migration configuration and test optional `explicit` routing separately.
8. Exercise the profile through the internal planned-provider constructor while the live singleton remains unchanged.
9. Run the Stage 1 APS matrix against the profile, including exact enabled/disabled signing fixtures.
10. Mark singleton registration and static backend discriminator for atomic removal in Stage 11.

Verification:

```bash
cargo test-fastly
cargo test-axum
cd crates/trusted-server-js/lib && npx vitest run
```

## Stage 9 — Prepare browser Prebid separation and shim migration

Files:

- `crates/trusted-server-core/src/integrations/prebid.rs`
- `crates/trusted-server-js/lib/src/integrations/prebid/index.ts`
- `crates/trusted-server-js/lib/test/integrations/prebid/index.test.ts`
- `crates/trusted-server-core/src/creative_opportunities.rs`
- `crates/trusted-server-core/src/publisher.rs`

Steps:

1. Define the post-cutover browser-only `[integrations.prebid]` shape, retaining `timeout_ms` and `debug` with 1000 ms/false defaults.
2. Prepare removal of server endpoint, server bidder allowlist, server debug, overrides, consent-forwarding, and suppression ownership, but do not change the live serde shape before Stage 11.
3. Keep browser bundle, account injection, script patterns, client-side bidders, and excluded GAM suffixes under `[integrations.prebid]`.
4. Add an injection path that consumes validated server-side bidder codes from `AuctionPlan`, not a second Prebid allowlist. Provider timeout/profile debug must never affect `pbjs.setConfig`; production startup passes the plan in Stage 11.
5. Preserve the current requestBids shim's initial and refresh snapshots, folding only server-side entries into `trustedServer.bidderParams` while leaving configured client-side bidders in the browser.
6. Add JS tests for:
   - browser timeout/debug independence with multiple PBS plans;
   - browser integration disabled while PBS profile compilation remains valid;
   - mixed client-side, PBS, APS, and standard-provider demand;
   - initial and refresh parameter preservation;
   - empty stored-request envelopes;
   - alternate returned bidder codes and APS renderer alias.
7. Update creative-opportunity tests to prove no hard-coded provider ID reaches client-controlled input.

Verification:

```bash
(cd crates/trusted-server-js/lib && npx vitest run)
(cd crates/trusted-server-js/lib && node build-all.mjs)
cargo test-fastly
```

## Stage 10 — Finalize diagnostics, notifications, deadlines, and mediation parity

Files:

- `crates/trusted-server-core/src/auction/orchestrator.rs`
- `crates/trusted-server-core/src/auction/types.rs`
- `crates/trusted-server-core/src/auction/telemetry.rs`
- `crates/trusted-server-core/src/auction/formats.rs`
- `crates/trusted-server-core/src/integrations/adserver_mock.rs`
- all adapter platform test modules

Steps:

1. Add the fixed `routing` metadata object:
   - auction-level saturating `unroutable_bidder_count` in `OrchestrationResult.metadata` plus bounded structured logging;
   - provider-level `skipped_no_eligible_slots = true`;
   - provider-level saturating `unused_bidder_params_count`.
2. Carry only booleans/counts. Do not include parameter values or bidder-ID lists.
3. Apply `suppress_all` and exact `returned_seat` suppression after profile normalization and before ranking/mediation.
4. Verify that missing/non-string seats do not match suppression, PBS still serializes `unknown`, and APS still serializes `aps`.
5. Restore `returned_seat` from the original provider bid after mock mediation. Make the existing telemetry seat field prefer `returned_seat` and fall back to delivery bidder code; add direct and mediated APS tests proving provider ID, upstream seat, and `aps` remain distinct.
6. Implement capability-dependent late completion behavior:
   - a future hard-total-deadline adapter would discard/classify timeout;
   - every current adapter accepts an already-completed late response because none claims an enforceable total-request deadline;
   - neither path launches new provider or mediator network work at zero remaining budget;
   - local ranking/delivery still finishes;
   - synchronous and split dispatch/collect agree.
7. Preserve response times as actual elapsed times and document wall-clock overrun limitations in adapter docs/tests.
8. Run regression tests for no mediator, mock mediation, mediator timeout/fallback, floors, winner selection, creative sanitization, Prebid Cache, APS renderer delivery, and telemetry provider IDs/seats.
9. Confirm routing metadata uses existing maps and does not add OpenRTB response or telemetry schema fields.

Verification:

```bash
cargo test-fastly
cargo test-axum
cargo test-cloudflare
cargo test-spin
cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity
```

## Stage 11 — Perform the atomic schema and runtime cutover

Stages 2 through 10 prepare and test the new architecture internally. This stage is one non-partial cutover: do not commit or merge a state in which public settings, deploy validation, runtime startup, browser injection, examples, or fixtures disagree.

Files:

- `crates/trusted-server-core/src/auction_config_types.rs`
- `crates/trusted-server-core/src/config.rs`
- `crates/trusted-server-core/src/settings.rs`
- `crates/trusted-server-core/src/auction/mod.rs`
- `crates/trusted-server-core/src/integrations/registry.rs`
- `crates/trusted-server-core/src/integrations/prebid.rs`
- `crates/trusted-server-core/src/integrations/aps.rs`
- all four adapter `app.rs` startup files
- `crates/trusted-server-cli/src/run.rs` and target-validation tests
- `crates/trusted-server-integration-tests/fixtures/configs/trusted-server.integration.toml`
- relevant adapter startup/route fixtures
- `trusted-server.example.toml`
- `docs/guide/configuration.md`
- `docs/guide/integrations/prebid.md`
- `docs/guide/integrations/aps.md`
- relevant README/getting-started pages
- configuration/env-overlay/config-push tests

Steps:

1. Replace the public legacy provider list with map-shaped `[auction.providers]` and `[auction.bidders]` settings.
2. Compile one `Arc<AuctionPlan>` per process startup, run target validation once, and pass that same plan to both `AuctionOrchestrator` and `IntegrationRegistry`. Do not recompile or inspect unresolved provider config in either consumer.
3. Switch all four adapters to the generic plan-backed provider constructor and provider-ID backend discriminator.
4. Make `validate_settings_for_deploy` run the same target-independent compiler. Make target-aware `config push` run the EdgeZero pre-write callback with the selected adapter capabilities; keep `config validate` target-independent with an explicit deferred-check message. Add end-to-end tests proving Cloudflare/Spin multi-provider configs fail before any remote read, prompt, or write.
5. Remove PBS/APS singleton bidder-provider builders and their static backend discriminators. Retain the mock mediator in its separate static registration path.
6. Apply the prepared Prebid browser/server config split. Pass validated browser server-side bidder codes from the shared plan into integration injection.
7. Use `AuctionPlan::has_profile(ProfileId::Aps)` or an equivalent narrow query to register APS renderer support even when `[integrations.aps]` is disabled. Test renderer presence on every adapter.
8. Remove obsolete integration-owned server fields and update all serde, env-overlay, config-push, adapter, integration, and browser fixtures in the same cutover.
9. Replace examples/docs with `[auction.providers.*]`, `profile_config`, and `[auction.bidders.*]`; keep `[auction].mediator` separate.
10. Document browser/server Prebid ownership, APS `all_eligible`, provider ID and extension bounds, target validation, and current no-hard-total-deadline limitations. Use only fictional/example values.
11. Run the complete cutover gate before committing:

```bash
cargo test-fastly
cargo test-axum
cargo test-cloudflare
cargo test-spin
./scripts/test-cli.sh
cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity
(cd crates/trusted-server-js/lib && npx vitest run)
(cd crates/trusted-server-js/lib && node build-all.mjs)
(cd docs && npm run format)
```

No committed stage may expose both public schemas.

## Stage 12 — Full verification and cleanup

1. Delete all internal staging adapters, duplicate legacy server configuration fields, singleton provider builders, unused static provider constants, and obsolete tests.
2. Confirm no bidder-provider profile receives `AuctionContext.request` or unrestricted `RuntimeServices`.
3. Confirm only common transport handles endpoint, backend, headers, redirects, response bounds, and dispatch.
4. Confirm the test-only standard endpoint mock is under `#[cfg(test)]` or integration-test code and absent from production binaries/config schema.
5. Run the complete repository gates:

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
./scripts/test-cli.sh
cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity
(cd crates/trusted-server-js/lib && npx vitest run)
(cd crates/trusted-server-js/lib && npm run format)
(cd crates/trusted-server-js/lib && node build-all.mjs)
(cd docs && npm run format)
```

6. Run `git diff --check` and verify no generated, staged, or `.pi-subagents` artifacts are included.

## Test matrix summary

| Area               | Required coverage                                                                                                                                                      |
| ------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Config compiler    | IDs, protocol/profile lookup, typed profile config, defaults, routes, endpoint canonicalization, static ownership/bounds, notifications, signing structure.            |
| Target validation  | shared backend-name prediction parity, encoded collisions, fan-out rejection, deadline capability claims, all four adapters.                                           |
| Admission          | envelope missing/null/empty/malformed/partial/unknown/collision/bounds, zone, raw header snapshot, attested XFF.                                                       |
| Routing            | explicit, all-eligible, trusted routes, mixed providers, unknown bidders, no banners, no parameter leakage, deterministic order.                                       |
| Standard profile   | request/response, static extensions, unused params, 204, malformed bids, exact signing on/off, redirects, test-only no-auth endpoint.                                  |
| PBS parity         | all standard-field differences, consent matrix, headers/Cookies, stored requests, overrides, debug/test, Cache, diagnostics, seats, suppression, exact signing on/off. |
| APS parity         | account/SDK fields, inventory identity, consent, renderer/script policy, response validation, candidate reduction, seats versus `aps`, exact signing on/off.           |
| Orchestration      | one request/provider, skip/no dispatch, partial failures, dynamic correlation, synchronous/split, logical/hard/late deadlines.                                         |
| Decision/delivery  | local ranking, floors, USD, mock mediation/fallback, sanitization, PBS Cache, APS renderer.                                                                            |
| Browser JS         | browser config ownership, mixed demand, folding, client-side preservation, refresh snapshots, aliases/renderers.                                                       |
| Diagnostics        | fixed metadata carrier, saturating counts, no params/IDs, provider IDs in telemetry, no schema expansion.                                                              |
| Configuration/docs | TOML maps, env overlays, example-only values, CLI validation, adapter limitations.                                                                                     |

## Primary file checklist

### Core control plane

- [ ] `auction_config_types.rs`: raw provider/bidder schema and newtypes.
- [ ] `auction/plan.rs`: compiler, immutable plan, target validation.
- [ ] `auction/profile.rs`: profile registration and typed compiled enum.
- [ ] `config.rs`: shared deploy validation.
- [ ] `auction/mod.rs`: registry wiring and mediator separation.

### Core runtime

- [ ] `auction/routing.rs`: envelope normalization and provider-local inputs.
- [ ] `auction/openrtb.rs`: common request/response driver and standard profile.
- [ ] `auction/provider.rs`: dynamic generic planned provider seam.
- [ ] `auction/orchestrator.rs`: plan execution, deadlines, diagnostics, unchanged decision flow.
- [ ] `auction/types.rs`: admitted input and returned-seat identity.
- [ ] `auction/formats.rs`: normalized admission and delivery alias serialization.
- [ ] `auction/endpoints.rs`: header snapshot and auction-local signer load.
- [ ] `auction/telemetry.rs`: provider-ID parity, returned-seat preference with delivery-code fallback, and existing metadata consumption only.
- [ ] `integrations/adserver_mock.rs`: restore original returned seat without generalizing mediation.

### Profiles and browser integration

- [ ] `integrations/prebid.rs`: typed profile, browser/server split, header/Cookie parity.
- [ ] `integrations/aps.rs`: typed profile, renderer activation, seat/alias parity.
- [ ] `creative_opportunities.rs`: config-neutral demand and trusted stored intent.
- [ ] `publisher.rs`: shared admission/plan use for page auctions.
- [ ] `integrations/registry.rs`: consume the shared compiled plan for renderer and browser capability queries.
- [ ] `trusted-server-js/lib/src/integrations/prebid/index.ts`: browser config and routing shim.

### Platforms and tooling

- [ ] Core platform types/traits: capability contract and shared backend-name prediction.
- [ ] Fastly backend/platform/app: provider discriminator, logical/quantized timeout separation, and no total-deadline claim.
- [ ] Axum platform/app: name prediction, fan-out, and no total-deadline claim.
- [ ] Cloudflare platform/app: deterministic name prediction, one-provider validation, and late completion behavior.
- [ ] Spin platform/app: deterministic name prediction, one-provider validation, and late completion behavior.
- [ ] EdgeZero callback/dependency pin plus CLI tests: target-aware pre-write push validation, target-independent validation, and map-shaped overlays.

### Documentation

- [ ] `trusted-server.example.toml`.
- [ ] Configuration guide.
- [ ] Prebid guide.
- [ ] APS guide.
- [ ] Adapter timeout/fan-out notes where maintained.

## Risk register

| Risk                                                                 | Mitigation                                                                                                                         |
| -------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------- |
| Implementing from the stale spec branch regresses merged APS         | Require implementation worktree from `main` at/after `af67d8c2`; run APS goldens first.                                            |
| Common field extraction broadens privacy exposure                    | Fixed typed policies plus exhaustive consent/header golden matrices; profiles can omit but never restore centrally removed data.   |
| Dynamic profile state becomes an unsafe type-erasure layer           | Use a closed typed enum and typed request-state variants for v1.                                                                   |
| Multiple provider instances collide in an adapter                    | Provider ID discriminator, shared prediction parity tests, compile-time target collision rejection, runtime guard retained.        |
| Browser Prebid behavior silently follows one server profile          | Keep browser timeout/debug explicit; derive only server bidder identities from the central registry.                               |
| Empty or malformed envelope unexpectedly fans out stored requests    | Exact fallback table and admission tests; malformed input never triggers fallback.                                                 |
| Header refactor truncates or broadens Cookie/Referer disclosure      | Snapshot the same first accepted header values with no new truncation; transport-only access; parity fixtures for malformed bytes. |
| APS seat preservation breaks renderer activation                     | Keep `Bid.bidder = "aps"`; add independent `returned_seat`; test direct and page delivery.                                         |
| Disabled signing changes PBS wire shape                              | Preserve host/scheme-only object and pin exact enabled/disabled JSON fixtures.                                                     |
| Non-abortable adapters claim a wall-clock guarantee they cannot meet | Capability-specific semantics, adapter docs, synchronous/split late-response tests.                                                |
| Mock mediator is accidentally generalized or routed                  | Separate storage/construction; retain existing `[auction].mediator` path and regression fixtures.                                  |
| Public diagnostics leak bidder data or expand telemetry schema       | Fixed existing metadata maps with booleans/saturating counts only.                                                                 |
| Temporary dual architecture survives the refactor                    | Final cleanup stage and searches for singleton builders, legacy list fields, and static PBS/APS backend IDs.                       |
| Plan grows into unrelated privacy/auth/network policy work           | Keep consent behavior, auth omission, and endpoint-network deferrals exactly as specified.                                         |

## Explicitly deferred

Do not add during implementation:

- real generic endpoint onboarding or endpoint credentials;
- a generic mediator/profile system;
- new signing protocol fields or body binding;
- new consent minimization or Cookie behavior;
- video/native support;
- currency conversion;
- label/group/multi-route routing;
- request splitting;
- new adapter abort/deadline mechanisms;
- new telemetry fields;
- broader private-network, custom-port, or DNS-rebinding policy.
