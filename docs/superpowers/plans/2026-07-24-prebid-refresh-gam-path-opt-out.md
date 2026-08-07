# Prebid Refresh GAM-Path Opt-Out Implementation Plan

**Design:** `docs/superpowers/specs/2026-07-24-prebid-refresh-gam-path-opt-out-design.md`

**Goal:** Let operators exclude selected GAM ad-unit-path suffixes from Trusted
Server's Prebid refresh auctions without suppressing the corresponding GAM refresh.

## Scope and constraints

- Do not change publisher source, slot div IDs, or GAM configuration.
- Do not edit `dist` output, minified assets, or an externally hosted Prebid bundle
  by hand. `build-prebid-external.mjs`/`ts prebid bundle` are the supported build
  path.
- The mechanism is literal, case-sensitive GAM-path suffix matching; it is not a
  size-based rule and does not add a div-ID fallback.
- Preserve the current `adInitRefreshInProgress` direct pass-through before all new
  work, including targeting cleanup.
- Use only fictional paths and hostnames in checked-in tests and documentation.

## Files

**Modify:**

- `crates/trusted-server-core/src/integrations/prebid.rs`
- `crates/trusted-server-js/lib/src/integrations/prebid/index.ts`
- `crates/trusted-server-js/lib/test/integrations/prebid/index.test.ts`
- `trusted-server.example.toml`
- `docs/guide/integrations/prebid.md`

**Update as needed for this work record only:**

- `docs/superpowers/specs/2026-07-24-prebid-refresh-gam-path-opt-out-design.md`
- this plan

## Task 1 — Add and inject canonical Prebid configuration

**Files:** `crates/trusted-server-core/src/integrations/prebid.rs`

1. Add `excluded_gam_ad_unit_path_suffixes: Vec<String>` to
   `PrebidIntegrationConfig`, defaulting to an empty vector. Initialize it in
   `base_config()` and every other explicit `PrebidIntegrationConfig` literal.
2. Implement one shared validation/canonicalization path used by both `build()` and
   `validate_config_for_startup()`:
   - reject leading/trailing whitespace;
   - reject empty/whitespace-only values;
   - require a leading `/`;
   - reject `/` itself;
   - retain every other value literally;
   - deduplicate exact valid entries, preserving first declaration order.
     Put schema-level validation on the field so typed settings parsing produces a
     useful configuration error, and retain the normalizer so the runtime payload is
     canonical rather than merely valid.
3. Extend the local `InjectedPrebidClientConfig` with a borrowed
   `excluded_gam_ad_unit_path_suffixes` field. Use the existing camel-case serde
   convention and omit it when empty, producing
   `excludedGamAdUnitPathSuffixes` only for a non-empty list.
4. Add Rust tests next to the existing Prebid tests:
   - omitted field defaults to empty;
   - valid values parse and duplicate values inject once in first-occurrence order;
   - empty, whitespace-padded, missing-leading-slash, and `/` inputs are rejected;
   - the head injector includes the expected camel-case JSON when configured and
     omits it by default.
5. Run focused Prebid tests while iterating, then run the target-matched Rust test
   commands required for the changed core configuration:

   ```bash
   cargo test-fastly
   cargo test-axum
   cargo test-cloudflare
   cargo test-spin
   ```

## Task 2 — Filter only refresh-auction slots in the browser

**Files:** `crates/trusted-server-js/lib/src/integrations/prebid/index.ts`

1. Add `excludedGamAdUnitPathSuffixes?: string[]` to `InjectedPrebidConfig` and
   `getAdUnitPath?: () => string` to `RefreshGptSlot`.
2. Add a small private predicate that receives a slot and configured suffix list. It
   must call `getAdUnitPath()` only when it exists, catch a getter exception, require
   a string result, and use `path.endsWith(suffix)` for any configured suffix. Filter
   injected values to non-root slash-prefixed strings so malformed direct injection
   fails open rather than excluding every slot.
3. In `installRefreshHandler()`, leave the
   `window.tsjs?.adInitRefreshInProgress` branch byte-for-byte behaviorally first.
   Keep target-slot resolution unchanged.
4. Clear targeting from independent slots before filtering. Build `auctionSlots`
   from those independent slots by applying the new predicate.
5. When `auctionSlots` is empty, call `originalRefresh(slots, opts)` immediately.
   Do not call `pbjs.requestBids()` or `pbjs.setTargetingForGPTAsync()`.
6. Otherwise, build synthetic ad units and `refreshAdUnitCodes` from
   `auctionSlots` only. In the bids-back handler, target only those eligible codes,
   then call `originalRefresh(slots, opts)` so the publisher's explicit or bare GPT
   refresh form is preserved.
7. Do not alter `TS_REFRESH_TARGETING_KEYS`, size fallback logic, injected-slot
   lookup, bidder-parameter recovery, or client-side-bid recovery except to ensure
   they run only for eligible auction slots.

## Task 3 — Cover refresh behavior with Vitest

**Files:** `crates/trusted-server-js/lib/test/integrations/prebid/index.test.ts`

Add tests in the existing `prebid/installRefreshHandler` suite. Make the mocked
`requestBids` invoke its callback synchronously only where post-auction behavior is
being asserted.

| Case                          | Assertions                                                                                                                                       |
| ----------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------ |
| Normal explicit slot          | Nonmatching path still creates an ad unit, requests bids, scopes targeting to its code, and refreshes after callback.                            |
| Explicit excluded slot        | Each TS/Prebid key is cleared; no Prebid request/targeting call; original GPT refresh receives the same slot list and options immediately.       |
| All-excluded global refresh   | `getSlots()` supplies the working list; all targets are cleaned; no Prebid call; the original bare GPT refresh remains bare with its options.    |
| Mixed global refresh          | Both slots are cleaned; only the normal slot occurs in ad units and scoped targeting; the original bare GPT refresh remains bare after callback. |
| Missing getter                | Slot is auctioned and wrapper does not throw.                                                                                                    |
| Throwing or non-string getter | Slot is auctioned and wrapper does not throw.                                                                                                    |
| Malformed injected suffixes   | Empty suffixes and non-array suffix lists fail open and do not suppress the auction.                                                             |
| Literal semantics             | Case mismatch and trailing-slash mismatch do not exclude.                                                                                        |
| Regression                    | Existing `adInitRefreshInProgress` direct-pass-through test and normal client-side bidder/param-recovery tests remain unchanged and green.       |

Run the JS suite after each change and format it:

```bash
cd crates/trusted-server-js/lib
npx vitest run
npm run format
```

## Task 4 — Document the operator contract

**Files:** `trusted-server.example.toml`, `docs/guide/integrations/prebid.md`

1. Add a commented example of
   `excluded_gam_ad_unit_path_suffixes = ["/trackingonly"]` to the example TOML.
2. Add the field to the Prebid guide's configuration table and explain:
   - it excludes only Trusted Server's GPT-refresh Prebid auction;
   - GAM still refreshes the matching slot;
   - matching is case-sensitive literal suffix matching via `getAdUnitPath()`;
   - invalid/empty/root suffixes are rejected and a missing getter fails open;
   - use a specific terminal path rather than a size or div-ID rule;
   - mixed global refreshes still auction normal slots;
   - excluded slots in mixed refreshes wait for the auction or refresh watchdog before
     GAM refresh, while an all-excluded refresh passes through immediately.
3. Explain that the server-served shim and injected configuration deploy together
   for this option; retain the pre-split external-bundle migration caveat and the
   direct-Prebid/APS non-goals. Do not add real inventory names, production domains,
   or tracking IDs.
4. Format docs:

   ```bash
   cd docs && npm run format
   ```

## Task 5 — Final validation and rollout verification

1. Inspect the complete diff; verify it contains no generated bundle edits and no
   publisher-source changes.
2. Run project gates relevant to the changed Rust, TypeScript, and docs surfaces:

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
   cd crates/trusted-server-js/lib && npx vitest run && npm run format
   cd docs && npm run format
   ```

3. Deploy the Trusted Server application/config carrying the suffix list. This
   browser-shim option does not require an external bundle regeneration; follow the
   existing bundle workflow only for the pre-split migration or external adapter/User
   ID changes.
4. On a controlled staging page, use browser request instrumentation to verify:
   - the injected browser config contains the expected suffixes;
   - a matching path still produces a GAM request/impression but no refresh
     `/auction` request;
   - a normal display slot in a mixed global refresh does produce `/auction`, gets
     scoped refreshed targeting, and GAM refreshes both slots;
   - `disableInitialLoad()`/initial Trusted Server loading still uses the existing
     `adInitRefreshInProgress` direct GPT path.
5. Record the deployed application/configuration version, test page, and
   observations in the implementation handoff. Do not treat an unstable external
   page as the sole verification environment.

## Acceptance criteria

- With no new configuration, refresh behavior is unchanged.
- A configured matching GAM path is never included in a synthetic refresh auction,
  but remains in the GPT refresh call.
- An all-excluded refresh produces no `requestBids()` or targeting call.
- A mixed refresh auctions and targets only eligible slots while refreshing every
  requested GPT slot.
- Stale Trusted Server/Prebid targeting is cleared from every independent slot,
  including excluded slots, while publisher delivery-slot targeting and the
  initial-load bypass remain untouched.
- The deployed server shim and injected configuration are version-compatible.
