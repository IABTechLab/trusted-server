# Prebid User ID Module support

**Date:** 2026-04-16
**Status:** Design
**Scope:** JS bundle (`crates/js/lib/src/integrations/prebid/index.ts`)

## Problem

The Trusted Server Prebid integration strips each publisher's origin `prebid.js`
and replaces it with a server-bundled build. That bundle imports the consent
management modules but does **not** import Prebid's User ID core module or any
ID submodules. As a result `pbjs.getUserIdsAsEids` is `undefined` at runtime,
the `syncPrebidEidsCookie()` helper early-returns, and the `ts-eids` cookie is
never written — even when the publisher's origin-side code has a fully
configured `userSync.userIds` list.

Downstream, `crates/trusted-server-core/src/ec/prebid_eids.rs` never receives a
cookie to ingest, so matched partner UIDs never land in the KV identity graph.

## Goal

Bundle Prebid's User ID core module and a broad, widely-deployed set of ID
submodules so publishers' existing `pbjs.setConfig({ userSync: { userIds: ... } })`
calls activate real ID resolution. After first auction completes, `ts-eids`
cookie is written and the backend ingestion path (already implemented) takes
over.

## Non-goals

- No Rust changes. No new `trusted-server.toml` fields.
- No runtime config injection from the server (`window.__tsjs_prebid.userIds`)
  — deferred to a follow-up.
- No build-time env-var toggle for the bundled set (e.g. `TSJS_PREBID_USER_IDS`
  mirroring `TSJS_PREBID_ADAPTERS`) — deferred to a follow-up.
- No automatic alignment between bundled ID submodules and configured
  `[[ec.partners]]` — operators must keep those in sync themselves.

## Design

### Bundled modules

Exactly one file changes: `crates/js/lib/src/integrations/prebid/index.ts`.
Add static imports near the existing `consentManagement*.js` imports.

**Core (required):**

- `prebid.js/modules/userId.js`

**Zero-config / auto-populating submodules** (resolve without publisher params):

- `prebid.js/modules/sharedIdSystem.js`
- `prebid.js/modules/criteoIdSystem.js`
- `prebid.js/modules/33acrossIdSystem.js`
- `prebid.js/modules/pubProvidedIdSystem.js`
- `prebid.js/modules/quantcastIdSystem.js`

**Param-based submodules** (inert until the publisher's `setConfig` supplies
the relevant params):

- `prebid.js/modules/id5IdSystem.js`
- `prebid.js/modules/identityLinkIdSystem.js`
- `prebid.js/modules/liveIntentIdSystem.js`
- `prebid.js/modules/uid2IdSystem.js`
- `prebid.js/modules/euidIdSystem.js`
- `prebid.js/modules/intentIqIdSystem.js`
- `prebid.js/modules/lotamePanoramaIdSystem.js`
- `prebid.js/modules/connectIdSystem.js`
- `prebid.js/modules/merkleIdSystem.js`

Total: 1 core + 14 submodules = 15 new imports.

> **Note (2026-04-16, during implementation):** `pubCommonIdSystem.js`, which
> was originally planned as a legacy/compatibility submodule, was removed from
> Prebid.js in 10.x (consolidated into `sharedIdSystem`). It is not importable
> from our pinned Prebid 10.26.0 and has been dropped from this plan.

No changes to `installPrebidNpm`, no changes to the `bidsBackHandler` shim, no
changes to `syncPrebidEidsCookie`. The existing cookie-writing path is already
correct — it was only silent because `pbjs.getUserIdsAsEids` did not exist.

### Runtime flow

No new runtime logic. The sequence below is what will light up once the
submodules are present:

1. Rust `IntegrationHeadInjector` emits the `window.pbjs` / `window.pbjs.que`
   / `window.__tsjs_prebid` bootstrap before any publisher-origin script runs.
2. Publisher origin code queues its existing config:
   `pbjs.que.push(() => pbjs.setConfig({ userSync: { userIds: [...] } }))`.
3. Our bundle loads. `installPrebidNpm()` registers the `trustedServer`
   adapter, shims `requestBids` (already appends a chained `bidsBackHandler`
   calling `syncPrebidEidsCookie`), then calls `pbjs.processQueue()` — the
   publisher's queued `setConfig` runs at this point and activates the
   configured submodules (each self-registered at import time).
4. User ID Module resolves IDs per its own rules (TCF/GPP/USP-gated, async).
5. First `requestBids` fires. Auction completes. Chained `bidsBackHandler`
   calls `syncPrebidEidsCookie()`.
6. `syncPrebidEidsCookie` calls `pbjs.getUserIdsAsEids()` (now a real
   function), flattens `[{source, id, atype}]`, base64-encodes JSON, writes
   `document.cookie = "ts-eids=..."`.
7. Subsequent `/auction` requests carry `Cookie: ts-eids=...`.
8. Backend (`crates/trusted-server-core/src/ec/prebid_eids.rs`) parses the
   cookie, matches `source` against `[[ec.partners]]`, syncs partner UIDs to
   KV.

The first `/auction` request after a cold page load still will not carry
`ts-eids`, because the cookie is written in the post-auction handler. This
matches preexisting behavior.

### Error handling

All failure modes are already covered by existing code. No new error paths.

- **Publisher has no `userSync.userIds` configured** →
  `pbjs.getUserIdsAsEids()` returns `[]` → early-return at `index.ts:380-382`.
  No cookie written. Silent. Correct.
- **Submodule fails to resolve** (no consent, no third-party ID, network
  error) → handled inside Prebid; `getUserIdsAsEids()` returns only the
  resolved subset. Cookie reflects what resolved.
- **Cookie payload exceeds 3072 bytes** → existing trim-and-retry loop at
  `index.ts:404-411` drops entries from the tail until it fits. If a single
  entry alone exceeds the cap, no cookie is written.
- **Unexpected exception in sync path** → caught by the existing `try/catch`
  at `index.ts:417-419`, logged via `log.warn`, does not break the auction.
- **Module import failure at build time** → esbuild fails the build. This
  catches missing or renamed Prebid modules before they ship.

### Known caveats

- **Backend pairing** — an EID whose `source` has no matching `[[ec.partners]]`
  entry is dropped at the backend (with a debug log). Bundling
  `id5IdSystem.js` is inert for EC identity-graph purposes unless the
  operator also adds an `[[ec.partners]]` entry with
  `source_domain = "id5-sync.com"`. Operators must keep the two lists in
  sync. Not a code change here; documented as an operator concern.
- **Bundle size** — adding 15 modules increases the shipped `tsjs-prebid.js`
  by an estimated ~100-150kb gzipped. Not gated on a build-time toggle in
  this change.

## Testing

### Automated (Vitest)

Add tests under `crates/js/lib/src/integrations/prebid/`:

- **Import smoke test** — import `./index.ts` and assert
  `typeof pbjs.getUserIdsAsEids === 'function'`. Guards against the exact
  regression that motivated this work.
- **`syncPrebidEidsCookie` unit tests** (new or expanded) — mock
  `pbjs.getUserIdsAsEids` to return a fixed `[{source, uids: [{id, atype}]}]`
  array and assert the cookie is written with base64-encoded
  `[{source, id, atype}]`. Cover:
  - empty array → no cookie written
  - normal payload → cookie written with expected value
  - oversize payload → trimmed to fit; partial entries persisted
  - single oversize entry → no cookie written

### Manual (after deploy to a dev publisher)

- DevTools console: `typeof pbjs.getUserIdsAsEids === 'function'` returns
  `true`.
- `pbjs.getUserIdsAsEids()` returns a non-empty array for a publisher with
  configured `userIds`.
- After the first auction: `document.cookie` contains `ts-eids=...`. Decoded
  payload (base64 → JSON) matches the raw EIDs.
- Network tab: second `/auction` request carries `Cookie: ts-eids=...`.

### Explicitly out of scope

- Each individual ID submodule's resolution behavior — that is Prebid's
  responsibility and covered by Prebid's own test suite.
- Backend ingestion of `ts-eids` — already covered by `prebid_eids.rs`
  tests; no new backend code.
- Bundle-size regression gating — noted as a caveat, not enforced.

## Rollout

This is a bundle change only. No migration, no feature flag, no staged
rollout beyond normal deploy.

On first deploy, publishers with active origin-side `userSync.userIds`
configuration will begin emitting `ts-eids` cookies after their first
auction. Publishers without `userSync.userIds` configured see no change.

## Follow-ups

1. **Build-time configurability** — introduce `_user_ids.generated.ts`
   driven by a `TSJS_PREBID_USER_IDS` env var, mirroring the existing
   `TSJS_PREBID_ADAPTERS` / `_adapters.generated.ts` pattern. Allows
   operators to slim the bundle per deployment.
2. **Server-injected `userSync.userIds`** — extend `trusted-server.toml`
   with a `[[integrations.prebid.user_ids]]` array. Rust serializes into
   `window.__tsjs_prebid.userIds`. JS applies via `pbjs.setConfig` before
   `processQueue()`. Supports publishers who do not run their own Prebid
   config on origin.
3. **Partner alignment tooling** — a startup-time check that warns when a
   bundled ID submodule has no matching `[[ec.partners]]` entry, or vice
   versa.
