# Prebid Refresh GAM-Path Opt-Out Design

**Date:** 2026-07-24 · **Status:** Proposed

**Scope:** Exclude selected GAM inventory from Trusted Server's Prebid refresh auctions while preserving GAM refreshes.

## 1. Problem statement

Trusted Server wraps publisher calls to `googletag.pubads().refresh()` and creates a
fresh synthetic Prebid ad unit for every refreshed GPT slot. That is correct for
display inventory, but it also sends tracking-only GAM slots to `/auction`. The
production motivating case is a hidden 1×1 slot whose GAM ad-unit path ends in
`/trackingonly`; its element ID is unstable and cannot be used as configuration.

Operators need a configuration-only way to identify that inventory by its supported
GAM ad-unit path and prevent **this refresh wrapper** from running a Trusted Server
or native-Prebid refresh auction for it. GPT must still refresh the slot so GAM can
record the tracking impression. Publishers must not change component source.

The successful 2026-07-23 browser capture established that GPT exposes
`slot.getAdUnitPath()` and returned `/88059007/autoblog/trackingonly` for the
tracking slot. The capture also showed that the current wrapper included that slot
in a `/auction` payload as a `[1, 1]` banner. The later 2026-07-24 failed probe is
not a stable production test environment and is not used as behavioral evidence.

## 2. Goals and non-goals

### Goals

- Add an operator-configured, suffix-based GAM-path exclusion mechanism.
- Support explicit `refresh([slot], options)` and bare/global `refresh(options)`
  calls.
- Keep excluded slots in the final GPT refresh list and preserve refresh options.
- Continue refreshing and auctioning non-excluded display slots, including in mixed
  global refreshes.
- Clear stale Trusted Server/Prebid targeting from every target slot before its GAM
  refresh, including excluded slots.
- Preserve the existing `adInitRefreshInProgress` direct-GAM bypass exactly.

### Non-goals

- Filtering by size, especially a broad 1×1 rule.
- Matching or configuring div IDs, ad-slot IDs, CSS selectors, labels, or arbitrary
  GPT targeting.
- Suppressing GAM requests, GAM impressions, slot definitions, or initial ad loads.
- Changing unrelated publisher Prebid, APS, or direct `/auction` flows.
- Editing generated or minified Prebid bundles directly.
- Changing Prebid adapter selection, bid-param overrides, or GAM line-item setup.

## 3. Current call flow

1. `installPrebidNpm()` registers the `trustedServer` Prebid adapter and wraps
   `pbjs.requestBids()` so server-side bidder requests flow through it
   (`crates/trusted-server-js/lib/src/integrations/prebid/index.ts:514-669`).
2. The adapter's `buildRequests()` serializes auction inputs and returns a POST to
   `auctionEndpoint`, which defaults to `/auction`
   (`index.ts:524-535`).
3. `installRefreshHandler()` wraps `googletag.pubads().refresh()` after GPT loads
   (`index.ts:726-818`). The wrapper:
   - immediately passes through `adInitRefreshInProgress` requests;
   - resolves explicit slots, or `pubads.getSlots()` for bare refreshes;
   - clears `ts_initial`, `hb_pb`, `hb_bidder`, `hb_adid`, `hb_cache_host`, and
     `hb_cache_path` from every resolved slot (`index.ts:43-50, 461-467`);
   - creates one synthetic refresh ad unit per target slot;
   - calls `pbjs.requestBids()`;
   - scopes `setTargetingForGPTAsync()` to the synthetic codes; and
   - calls the original GPT refresh after bids return.
4. The refresh-slot type currently represents only element ID, targeting cleanup,
   and sizes (`index.ts:260-265`). It does not expose GAM path metadata.
5. Rust reads `integrations.prebid` into `PrebidIntegrationConfig`
   (`crates/trusted-server-core/src/integrations/prebid.rs:203-343`) and injects
   browser config into `window.__tsjs_prebid` through the head injector
   (`prebid.rs:1006-1041`). The injected TypeScript shape is declared at
   `index.ts:62-87`.

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
  window.__tsjs_prebid={"excludedGamAdUnitPathSuffixes":["/trackingonly","/measurement-only"],...};
</script>
```

For `[]`, omit the property with `skip_serializing_if`, matching the existing
`clientSideBidders` convention. A browser that receives no property treats it as an
empty list. This makes upgrade and rollback backwards compatible: old configuration
has no behavior change; an older external bundle safely ignores the extra injected
property; and a newer bundle with old configuration has no exclusions.

## 5. Matching and refresh behavior

### 5.1 Match predicate

Extend `RefreshGptSlot` with:

```ts
getAdUnitPath?: () => string;
```

At each publisher refresh, derive a `Set` from
`getInjectedConfig()?.excludedGamAdUnitPathSuffixes ?? []`. A slot is excluded only
when all of the following hold:

1. The normalized set is non-empty.
2. `slot.getAdUnitPath` is a function.
3. Calling it returns a string.
4. The returned GAM path `endsWith()` at least one configured suffix, using exact,
   case-sensitive JavaScript string comparison.

Do not derive paths from the element ID or injected `adSlots` metadata. Do not use
`getSizes()` as a fallback. A missing getter, a non-string return value, an empty
path, or a getter that throws is **fail-open**: the slot remains auction-eligible.
The implementation catches only the getter failure around that call; it neither
suppresses the GPT refresh nor broadens an exclusion because telemetry is absent.

A matching path is excluded only from the synthetic refresh auction. It is not
removed from GPT's target list.

### 5.2 Required algorithm

Keep the existing `adInitRefreshInProgress` check as the first branch, before slot
resolution, targeting cleanup, and path inspection:

```text
if adInitRefreshInProgress:
    originalRefresh(slots, options)
    return

targetSlots = explicit slots, or pubads.getSlots() for bare refresh
if targetSlots is empty:
    originalRefresh(slots, options)
    return

clear TS/Prebid refresh-targeting keys from every target slot
auctionSlots = targetSlots excluding suffix-matched slots

if auctionSlots is empty:
    originalRefresh(targetSlots, options)
    return

adUnits = synthetic refresh ad units for auctionSlots only
pbjs.requestBids({ adUnits, timeout, bidsBackHandler })
bidsBackHandler:
    pbjs.setTargetingForGPTAsync(auction-slot codes only)
    originalRefresh(targetSlots, options)
```

Build candidate codes, recover publisher bidder params, and recover client-side bids
only for `auctionSlots`; excluded slots must not be represented in `adUnits` at all.
The existing scoped targeting behavior therefore continues to affect only eligible
slots.

### 5.3 Refresh sequences

| Call and slot set                                        | Prebid behavior                                                                     | GPT behavior                                                                      |
| -------------------------------------------------------- | ----------------------------------------------------------------------------------- | --------------------------------------------------------------------------------- |
| `refresh([normal], options)`                             | Auction `normal`, target its synthetic code after bids return.                      | Refresh `[normal]` with the same options after the callback.                      |
| `refresh([excluded], options)`                           | Clear TS/Prebid keys; do not call `requestBids()` or `setTargetingForGPTAsync()`.   | Immediately refresh `[excluded]` with the same options.                           |
| Bare `refresh(options)`; all slots excluded              | Resolve `pubads.getSlots()`, clear their TS/Prebid keys, then make no Prebid calls. | Immediately refresh the resolved complete slot list with the same options.        |
| Bare `refresh(options)`; mixed normal and excluded slots | Clear every target slot; auction and target only normal slots.                      | After the callback, refresh the complete resolved list, including excluded slots. |
| Any refresh while `adInitRefreshInProgress` is true      | No cleanup, match, auction, or targeting.                                           | Directly pass through the original `slots` and options unchanged.                 |
| Missing/throwing `getAdUnitPath()`                       | Treat the slot as normal and auction it.                                            | Existing post-auction refresh behavior.                                           |

Passing the resolved list in the all-excluded and mixed cases is deliberate: it is
the same concrete list used for cleanup and makes the final GAM refresh list
explicit. The original options object is passed through unchanged.

### 5.4 Targeting and initial-load invariants

The cleanup step remains before filtering and is limited to the existing
`TS_REFRESH_TARGETING_KEYS`. It removes stale Trusted Server/Prebid winner data
from excluded slots so GAM cannot serve using an obsolete header-bid winner, while
preserving GAM path metadata and every unrelated publisher targeting key.

`adInitRefreshInProgress` continues to bypass cleanup and auctioning directly. This
preserves `disableInitialLoad()` and the initial Trusted Server targeting handoff:
that one internal refresh must deliver already-applied targeting to GAM instead of
being converted into a client-side refresh auction.

## 6. Implementation areas

| File                                                                          | Planned change                                                                                                      |
| ----------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------- |
| `crates/trusted-server-core/src/integrations/prebid.rs`                       | Add config field, validation/canonicalization, head-injected camel-case array, and Rust tests.                      |
| `crates/trusted-server-js/lib/src/integrations/prebid/index.ts`               | Add injected config/type support, guarded path matcher, and filter `targetSlots` into `auctionSlots` after cleanup. |
| `crates/trusted-server-js/lib/test/integrations/prebid/index.test.ts`         | Add explicit/global/mixed/fail-open refresh tests.                                                                  |
| `trusted-server.example.toml`                                                 | Add a commented fictional configuration example.                                                                    |
| `docs/guide/integrations/prebid.md`                                           | Document the field, exact matcher semantics, and GAM-preservation caveat.                                           |
| `docs/superpowers/specs/2026-07-24-prebid-refresh-gam-path-opt-out-design.md` | Update only if implementation exposes a necessary design correction.                                                |
| `docs/superpowers/plans/2026-07-24-prebid-refresh-gam-path-opt-out.md`        | Mark implementation evidence/status only if project practice requires it.                                           |

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
- Global all-excluded slots resolve through `getSlots()`, clear every target, make
  no Prebid calls, and refresh the complete resolved list with options.
- Global mixed slots clear both categories, auction only eligible slots, scope
  targeting to eligible synthetic codes, and refresh the complete list after bids
  return.
- Missing `getAdUnitPath`, non-string path, and a throwing getter each fail open to
  the normal auction path without throwing from the wrapper.
- Case mismatch and a trailing-slash mismatch do not exclude, proving literal
  case-sensitive suffix behavior.
- Existing `adInitRefreshInProgress` test still proves direct pass-through without
  cleanup or auction; existing normal refresh and client-side-bid recovery tests
  remain green.

## 8. External bundle and browser verification

`crates/trusted-server-js/lib/build-prebid-external.mjs` is the supported source
build path for the immutable external Prebid bundle; `build-all.mjs` intentionally
does not build Prebid. Implementers must change TypeScript source and regenerate a
new external bundle through the supported `ts prebid bundle` workflow (or its
underlying supported generator), not edit a generated/minified asset.

Roll out the application/config and the regenerated external bundle together:

1. Build and test the source change.
2. Generate and upload the new external bundle.
3. Update the operator bundle URL/hash/SRI metadata as required by the existing
   bundle workflow and deploy the Trusted Server application/config containing the
   suffix list.
4. Verify the first-party bundle URL resolves to the new bytes and the injected
   `window.__tsjs_prebid.excludedGamAdUnitPathSuffixes` has the expected values.
5. In browser instrumentation, verify a matching slot calls GPT refresh without a
   corresponding Trusted Server refresh `/auction` request, while a normal display
   slot in the same global refresh still produces `/auction` and receives refreshed
   Prebid targeting.
6. Verify GAM records the excluded slot's request/impression. Use a controlled
   staging page or harness rather than relying on the unstable production host.

A config-only deployment with an old cached external bundle cannot apply the browser
filter; a bundle-only deployment without the injected configuration remains a no-op.

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
