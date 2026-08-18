# Prebid Refresh GAM-Path Opt-Out Design

**Date:** 2026-07-24 · **Status:** Proposed

**Scope:** Exclude selected GAM inventory from Trusted Server's Prebid refresh auctions while preserving GAM refreshes.

## 1. Problem statement

Trusted Server wraps publisher calls to `googletag.pubads().refresh()` and creates a
fresh synthetic Prebid ad unit for every refreshed GPT slot. That is correct for
display inventory, but it also sends tracking-only GAM slots to `/auction`. A
representative hidden 1×1 slot can have an ad-unit path ending in
`/trackingonly`; its element ID may be unstable and cannot be used as configuration.

Operators need a configuration-only way to identify that inventory by its supported
GAM ad-unit path and prevent **this refresh wrapper** from running a Trusted Server
or native-Prebid refresh auction for it. GPT must still refresh the slot so GAM can
record the tracking impression. Publishers must not change component source.

A controlled browser capture confirmed that GPT exposes `slot.getAdUnitPath()` for a
tracking slot such as `/123456/example-news/trackingonly`, and that the wrapper can
include the slot in a `/auction` payload as a `[1, 1]` banner. The behavior is
covered by deterministic tests rather than an unstable external page.

## 2. Goals and non-goals

### Goals

- Add an operator-configured, suffix-based GAM-path exclusion mechanism.
- Support explicit `refresh([slot], options)` and bare/global `refresh(options)`
  calls.
- Keep excluded slots in the final GPT refresh list and preserve refresh options.
- Continue refreshing and auctioning non-excluded display slots, including in mixed
  global refreshes.
- Clear stale Trusted Server/Prebid targeting from every independent slot before its
  GAM refresh, including excluded slots, while preserving targeting on publisher
  delivery slots.
- Preserve the slot service's protected initial-display decision before optional
  refresh-auction work.

### Non-goals

- Filtering by size, especially a broad 1×1 rule.
- Matching or configuring div IDs, ad-slot IDs, CSS selectors, labels, or arbitrary
  GPT targeting.
- Suppressing GAM requests, GAM impressions, slot definitions, or initial ad loads.
- Changing unrelated publisher Prebid, APS, or direct `/auction` flows.
- Editing generated or minified Prebid bundles directly.
- Changing Prebid adapter selection, bid-param overrides, or GAM line-item setup.

## 3. Current call flow

1. The critical Prebid registration validates injected configuration and publishes
   one frozen `prebid.v1` capability (`module.ts`).
2. The deferred `prebid_later` registration installs one optional policy into the
   sole GPT publisher-call observer (`later.ts`, `gpt/startup.ts`).
3. The slot service evaluates protected publisher refresh ownership first. Only
   forwardable or replaced slots reach `createPrebidRefreshPolicy()`.
4. The policy clears the fixed Trusted Server/Prebid targeting keys, filters exact
   GAM-path suffix matches, and runs one synthetic auction for the remaining slots
   (`refresh.ts`).
5. Deferred GPT and Prebid adapters own vendor calls; navigation replacement or
   disposal cancels pending work and ignores late completion.
6. Rust reads `integrations.prebid` into `PrebidIntegrationConfig`
   (`crates/trusted-server-core/src/integrations/prebid.rs:203-343`) and injects
   browser config through the head injector. The critical Prebid registrar consumes
   that boot-owned value once and publishes the validated capability.

## 4. Configuration contract

### 4.1 Operator API

Add this optional field to `[integrations.prebid]`:

```toml
[integrations.prebid]
# Keep GAM tracking inventory out of Trusted Server's Prebid refresh auctions.
# GPT still refreshes these slots.
excluded_gam_ad_unit_path_suffixes = ["/trackingonly", "/measurement-only"]
```

The field is an array because publishers may have multiple tracking-only paths and
because selecting one suffix must not preclude a future suffix. Its default is `[]`.
An omitted or empty array preserves today's behavior: every resolved refresh slot is
auction-eligible.

The configuration is intentionally in the existing Prebid integration rather than
in GPT configuration: it controls whether the **Prebid refresh auction** runs and
is serialized with the rest of the Prebid browser configuration.

### 4.2 Validation and canonicalization

Validate enabled Prebid configuration at startup and in `ts config validate` using
a shared normalization helper before constructing `PrebidIntegration`:

| Input                                     | Result                                                                                          |
| ----------------------------------------- | ----------------------------------------------------------------------------------------------- |
| Omitted or `[]`                           | Valid; no exclusions.                                                                           |
| A suffix with leading/trailing whitespace | Reject; do not silently alter a path matcher.                                                   |
| Empty or whitespace-only suffix           | Reject.                                                                                         |
| Suffix not beginning with `/`             | Reject.                                                                                         |
| `/`                                       | Reject; it would opt every slash-prefixed GAM path out and is not an inventory-specific suffix. |
| Any other slash-prefixed string           | Valid literal suffix; trailing slashes, repeated slashes, and case are not normalized.          |
| Exact duplicate valid strings             | Retain the first occurrence and remove later duplicates before injection.                       |

The normalized list preserves first-occurrence declaration order. There is no
case-folding, path parsing, URL decoding, slash collapsing, or trailing-slash
normalization. This produces an auditable literal match and avoids accidental
cross-inventory matching.

The Rust schema remains a `Vec<String>` with `#[serde(default)]`; the implementation
adds a custom validator plus one reusable normalizer so invalid values fail typed
configuration validation and a valid duplicate list has one canonical runtime form.
`build()` and `validate_config_for_startup()` must both use the normalizer, so the
runtime and validation command cannot disagree. Existing Rust config literals must
set the new field to `Vec::new()`.

### 4.3 Browser injection

Add an optional camel-case property to the serialized head-injected payload and to
`InjectedPrebidConfig`:

```ts
interface InjectedPrebidConfig {
  // existing fields
  excludedGamAdUnitPathSuffixes?: string[]
}
```

For a non-empty normalized list the server injects:

```html
<script>
  window.tsjs._integrationConfig.prebid={"excludedGamAdUnitPathSuffixes":["/trackingonly","/measurement-only"],...};
</script>
```

For `[]`, omit the property with `skip_serializing_if`, matching the existing
`clientSideBidders` convention. The registrar treats an absent property as an empty
list, so every otherwise eligible refresh slot remains auctionable.

## 5. Matching and refresh behavior

### 5.1 Match predicate

At each publisher refresh, snapshot the validated readonly array from the
`prebid.v1` capability. A slot is excluded only
when all of the following hold:

1. The array is non-empty.
2. The deferred GPT adapter can read the slot's GAM ad-unit path.
3. Reading it returns a string.
4. The returned GAM path `endsWith()` at least one configured suffix, using exact,
   case-sensitive JavaScript string comparison.

Do not derive paths from the element ID or projected slot metadata. Do not use sizes
as a fallback. A missing getter, a non-string return value, an empty path, or a
getter that throws is **fail-open**: the slot remains auction-eligible.

A matching path is excluded only from the synthetic refresh auction. It is not
removed from GPT's target list.

### 5.2 Required algorithm

The slot service's publisher-refresh decision must run before the optional policy:

```text
slotDecision = slotService.preparePublisherRefresh(call)
if slotDecision is suppress:
    suppress the publisher call
    return

targetSlots = slotDecision replacement slots, otherwise observed call slots
if targetSlots is invalid or empty:
    preserve slotDecision
    return

defer the publisher call
clear TS/Prebid refresh-targeting keys from every target slot
auctionSlots = targetSlots excluding suffix-matched slots

if auctionSlots is empty:
    settle the deferred call immediately
    return

run one adapter-backed synthetic auction for auctionSlots
settle after completion, the 1.5-second watchdog, disposal, or navigation replacement
```

Build candidate codes, recover publisher bidder params, and recover client-side bids
only for `auctionSlots`; excluded slots must not be represented in `adUnits` at all.
The adapter-backed auction scopes targeting to eligible synthetic codes only.

### 5.3 Refresh sequences

| Call and slot set                                        | Prebid behavior                                                                   | GPT behavior                                                 |
| -------------------------------------------------------- | --------------------------------------------------------------------------------- | ------------------------------------------------------------ |
| `refresh([normal], options)`                             | Auction `normal`, target its synthetic code after bids return.                    | Refresh `[normal]` with the same options after the callback. |
| `refresh([excluded], options)`                           | Clear TS/Prebid keys; do not call `requestBids()` or `setTargetingForGPTAsync()`. | Immediately refresh `[excluded]` with the same options.      |
| Bare `refresh(options)`; all slots excluded              | Clear observed targets; skip Prebid.                                              | Resume when policy cleanup settles.                          |
| Bare `refresh(options)`; mixed normal and excluded slots | Clear targets; auction eligible slots only.                                       | Resume after auction completion or watchdog.                 |
| Slot-service protected initial display                   | No cleanup, match, auction, or targeting.                                         | Preserve the slot-service decision.                          |
| Missing/throwing `getAdUnitPath()`                       | Treat the slot as normal and auction it.                                          | Existing post-auction refresh behavior.                      |

The sole GPT adapter retains the publisher call and options while the policy owns
only its asynchronous completion latch.

### 5.4 Targeting and initial-load invariants

The cleanup step remains before filtering and is limited to the existing
`TS_REFRESH_TARGETING_KEYS`. It removes stale Trusted Server/Prebid winner data
from excluded slots so GAM cannot serve using an obsolete header-bid winner, while
preserving GAM path metadata and every unrelated publisher targeting key.

The slot service continues to protect initial Trusted Server targeting handoff before
the Prebid policy runs, so it cannot be converted into a client-side refresh auction.

## 6. Implementation areas

| File                                                                          | Planned change                                                                                 |
| ----------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------- |
| `crates/trusted-server-core/src/integrations/prebid.rs`                       | Add config field, validation/canonicalization, head-injected camel-case array, and Rust tests. |
| `crates/trusted-server-js/lib/src/integrations/prebid/module.ts`              | Validate boot config and publish the frozen `prebid.v1` capability.                            |
| `crates/trusted-server-js/lib/src/integrations/prebid/refresh.ts`             | Implement guarded path matching and the cancellable synthetic-refresh policy.                  |
| `crates/trusted-server-js/lib/src/integrations/prebid/later.ts`               | Register the policy through the single GPT observer.                                           |
| `crates/trusted-server-js/lib/test/integrations/prebid/module.test.ts`        | Cover literal filtering, fail-open behavior, cancellation, and adapter-backed auctions.        |
| `trusted-server.example.toml`                                                 | Add a commented fictional configuration example.                                               |
| `docs/guide/integrations/prebid.md`                                           | Document the field, exact matcher semantics, and GAM-preservation caveat.                      |
| `docs/superpowers/specs/2026-07-24-prebid-refresh-gam-path-opt-out-design.md` | Update only if implementation exposes a necessary design correction.                           |
| `docs/superpowers/plans/2026-07-24-prebid-refresh-gam-path-opt-out.md`        | Mark implementation evidence/status only if project practice requires it.                      |

No generated `dist` file, minified external bundle, or publisher source file is a
source-of-truth edit target.

## 7. Test matrix

### Rust configuration and injection

- Default/omitted field yields `Vec::new()` and omits
  `excludedGamAdUnitPathSuffixes` from injected JSON.
- A valid array parses, normalizes exact duplicates to one entry in declaration
  order, and injects the expected camel-case array.
- Empty, whitespace-padded, whitespace-only, missing-leading-slash, and `/` values
  fail enabled Prebid configuration validation with field-specific errors.
- Existing Prebid config/head-injector tests continue to pass with the new empty
  field initialized in helper literals.

### Browser refresh wrapper

- Normal explicit slot with a nonmatching path still calls `requestBids()`, creates
  its ad unit, scopes targeting to its code, and refreshes it after the callback.
- Explicit matching slot clears each existing TS/Prebid key, does not call
  `requestBids()` or `setTargetingForGPTAsync()`, and immediately calls original
  GPT refresh with the exact slot array and options.
- Global all-excluded slots resolve through `getSlots()`, clear independent-slot
  targeting, make no Prebid calls, and pass the original bare refresh and options to
  GPT.
- Global mixed slots preserve publisher delivery-slot targeting, clear independent
  slots, auction only eligible slots, scope targeting to eligible synthetic codes,
  and pass the original bare refresh and options to GPT after bids return.
- Missing `getAdUnitPath`, non-string path, and a throwing getter each fail open to
  the normal auction path without throwing from the wrapper.
- Case mismatch and a trailing-slash mismatch do not exclude, proving literal
  case-sensitive suffix behavior.
- Existing GPT protected-refresh tests still prove bypass without cleanup or auction;
  normal refresh and client-side-bid recovery tests remain green.

## 8. External bundle and browser verification

`crates/trusted-server-js/lib/build-prebid-external.mjs` remains the supported source
build path for the immutable external Prebid bundle; `build-all.mjs` intentionally
does not build Prebid. This feature does not change that external bundle: its refresh
filter lives in the release-bound `prebid_later` registrar module. Implementers
change TypeScript source rather than generated/minified assets.

Roll out the Trusted Server application and its configuration together:

1. Build and test the source change.
2. Deploy the application/config containing the suffix list.
3. Verify the consumed Prebid integration config has the expected suffixes; the
   transient `_integrationConfig` transport must be deleted before runtime commit.
4. In browser instrumentation, verify a matching slot calls GPT refresh without a
   corresponding Trusted Server refresh `/auction` request, while a normal display
   slot in the same global refresh still produces `/auction` and receives refreshed
   Prebid targeting.
5. Verify GAM records the excluded slot's request/impression with a controlled
   staging page or harness.

No new external Prebid bundle is required for this option. Changes to external
adapters or User ID modules still use the normal bundle workflow.

## 9. Operational caveats and risks

- The exclusion is limited to Trusted Server's wrapper around GPT refresh. It does
  not block a publisher's unrelated direct `pbjs.requestBids()`, APS calls, direct
  `/auction` use, or any other auction wrapper.
- The feature relies on GPT's supported `getAdUnitPath()` API. A missing or throwing
  getter deliberately fails open, which may continue auctioning a tracking slot
  rather than risk silently suppressing display inventory.
- Literal suffix matching can be over-broad if an operator chooses a generic suffix
  such as `/only`; use a unique terminal GAM path segment and validate on a staging
  page. `/` is rejected, but other overly broad valid values remain an operator
  responsibility.
- Excluded slots have only Trusted Server/Prebid targeting cleared; unrelated GAM
  targeting and GAM request behavior are intentionally untouched.
- Browser code and injected config must reach the same deployed page. Cache/version
  rollout mistakes are the primary operational risk.
