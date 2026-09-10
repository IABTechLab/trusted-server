# GPT Runtime Diagnostics Overlay — Implementation Plan

> **Status:** Implemented — live publisher acceptance remains environment-specific.
>
> **For agentic workers:** Execute this plan incrementally and keep the repository
> buildable after each task. Steps use checkbox (`- [ ]`) syntax for tracking.
> Follow `CLAUDE.md`; in particular, use the target-matched Cargo aliases rather
> than bare workspace commands.

**Goal:** Add an opt-in, browser-session console that observes documented
Google Publisher Tag lifecycle callbacks, organizes them into bounded per-slot
request cycles, displays a hydration-safe panel and exact-binding badges, and
exports the same GPT-observed facts without claiming creative provenance.

**Architecture:** A dedicated `gpt_diagnostics` integration parses exact
activation directives before generic cookie handling, stores activation in a
host-only HttpOnly session cookie, and cleans the browser-visible URL. Inactive
HTML omits diagnostics; active HTML injects a content-hashed standalone module
synchronously after core. When active, the module creates an observer, bounded store,
exact binding manager, read-only browser API, and presentation controller. Data
capture does not depend on the overlay. The observer uses only documented
`googletag.cmd` and PubAdsService event listeners; it never patches GPT,
publisher, networking, or history behavior.

**Tech stack:** Rust 2024 (`trusted-server-core` integration registration,
request activation, cache policy, and conditional HTML injection), TypeScript
(`trusted-server-js`), Vitest/jsdom, Playwright, closed Shadow DOM, and
documented GPT PubAdsService callbacks.

**Spec:**
`docs/superpowers/specs/2026-07-28-gpt-runtime-diagnostics-overlay-design.md`

---

## Approved Product and Data Decisions

The implementation must preserve these resolved decisions from the spec:

- The panel opens fully on first mount; the user can collapse it.
- `adUnitPath` is exported whenever GPT exposes it, independent of panel state.
- V1 uses badges only; click-to-highlight is deferred.
- Overlapping cycles are exercised with a deterministic GPT event-bus stub.
- Activation remains `ts_console`; deployment configuration is
  `integrations.gpt_diagnostics`.
- Matching disposition and sequence validity are separate. A uniquely matched,
  out-of-order callback remains matched and also creates an
  `invalid_event_order` callback issue.
- **Incomplete sequence** augments the observed lifecycle state and requires
  affirmative evidence; elapsed time alone never makes a cycle incomplete.
- Duplicate DOM IDs or duplicate retained GPT slot IDs produce an ambiguous
  binding and no badge. The implementation never chooses the first element or
  newest slot heuristically.

No PR #961 tracing implementation is present at this branch's current base.
Do not add cleanup work for auction tracing unless such code is introduced by a
later merge.

---

## Scope Guardrails

### In scope

- Dedicated deployment configuration and conditional standalone JS delivery.
- Browser-session activation, URL cleanup, and strict active-HTML cache privacy.
- Six documented GPT lifecycle listeners.
- Bounded slot, cycle, and callback-issue storage.
- Conservative cycle matching, timings, coverage, and issue reporting.
- Exact and unique DOM binding.
- Read-only API, versioned snapshot, and explicit local JSON download.
- Closed-Shadow-DOM panel and viewport badges.
- Hydration, SPA, element-replacement, and host-remount behavior.
- Rust, Vitest, Playwright, and environment-specific live acceptance coverage.
- Operator documentation and example configuration.

### Explicitly out of scope

- Auction, bid, winner, bidder, targeting, or creative provenance.
- OpenRTB, `/auction`, Prebid, creative renderer, telemetry, Tinybird, or
  diagnostic-record upload. The activation-bit cookie and cache policy are the
  only server response behavior in scope.
- Patching `display`, `defineSlot`, `refresh`, `fetch`, XHR, publisher callbacks,
  `pushState`, or route handlers.
- Resource Timing inference or diagnostic network upload.
- Persistent diagnostic records beyond in-memory document state.
- Changes to the existing `gpt` integration's ad-serving behavior.

Stop and request spec review if implementation appears to require any excluded
area.

---

## Runtime Contracts to Pin in Tests

### Activation

- The dedicated integration is absent unless configured and enabled.
- Recognized `ts_console` values are exactly `1`, `true`, `0`, and `false`.
- One exact directive establishes or clears `__Host-ts-console`, a host-only,
  Secure, HttpOnly, SameSite=Lax browser-session cookie, then removes all
  reserved pairs from origin handling and the visible URL while preserving the
  path, unrelated parameters, and fragment.
- Invalid/duplicate directives and duplicate cookies fail closed for the current response.
- Inactive HTML contains no diagnostics module request. Active/directive HTML is
  `private, no-store`; the cookie-independent standalone module remains public.
- Active module evaluation installs no non-diagnostic network behavior.

### Coverage and issues

For each callback kind:

```text
observed = matched + unmatched + ambiguous
```

A callback issue is secondary diagnostic information, not a fourth matching
category. `callbackIssues` may therefore include:

- `disposition: "unmatched"` with a no-compatible-cycle reason.
- `disposition: "ambiguous"` with `overlapping_request_cycles`.
- `disposition: "matched"` with `invalid_event_order`.

### Lifecycle state

Primary state is one of Waiting for request, Requesting, Response received,
Rendered (fill unknown), Filled, or Empty. Loaded, Viewable, Incomplete sequence,
Unbound, and Ambiguous binding are independent facts or issues. Pending work is
never converted to Incomplete by a timer.

### API

Use these public semantics:

- `snapshot()` returns a fresh `GptDiagnosticsExportV1` object.
- `export()` creates and clicks a local Blob download, then revokes its object
  URL; it performs no network request.
- `subscribe(listener)` schedules the listener with fresh snapshots after
  coalesced store or binding changes and returns an unsubscribe function.
- `hide()` has the same dismissal semantics as the Close control.
- `show()` clears dismissal and remounts presentation without restarting or
  resetting capture.

### Binding

A binding is valid only when exactly one connected DOM element has the exact
`slotElementId` and exactly one retained GPT slot record claims that ID.
Ambiguity remains visible in the panel/export but never produces a badge.
Element replacement with the same unique ID rebinds the same retained slot.

---

## File Map

Paths under the new TypeScript integration may be consolidated if doing so
makes the code materially simpler, but the observer, store, and presentation
must remain independently testable.

| File                                                                                              | Action  | Responsibility                                                                           |
| ------------------------------------------------------------------------------------------------- | ------- | ---------------------------------------------------------------------------------------- |
| `crates/trusted-server-core/src/integrations/gpt_diagnostics.rs`                                  | Create  | Config, request activation/cookie gate, cache policy, and standalone tag                 |
| `crates/trusted-server-core/src/integrations/gpt_diagnostics_bootstrap.js`                        | Removed | Superseded by server-recognized activation and request-scoped inline cleanup             |
| `crates/trusted-server-core/src/integrations/mod.rs`                                              | Modify  | Export and register `gpt_diagnostics` builder                                            |
| `crates/trusted-server-core/src/config.rs`                                                        | Modify  | Include diagnostics in deploy-time typed integration validation                          |
| `crates/trusted-server-core/src/migration_guards.rs`                                              | Modify  | Include the new platform-neutral Rust source in migration guards                         |
| `crates/trusted-server-core/src/html_processor.rs`                                                | Modify  | Inject request-scoped activation and standalone module tags in early synchronous order   |
| `trusted-server.example.toml`                                                                     | Modify  | Add disabled-by-default diagnostics example                                              |
| `crates/trusted-server-js/lib/src/core/types.ts`                                                  | Modify  | Public V1 export/API types and optional `TsjsApi.gptDiagnostics`                         |
| `crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/index.ts`                          | Create  | Active-path composition and idempotent installation                                      |
| `crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/store.ts`                          | Create  | Slot identity, cycles, matching, timings, coverage, issues, bounds, subscriptions        |
| `crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/observer.ts`                       | Create  | Narrow GPT interfaces, command-queue installation, normalized callbacks, safety boundary |
| `crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/binding.ts`                        | Create  | Exact lookup, uniqueness checks, rebinding, visibility, binding snapshots                |
| `crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/api.ts`                            | Create  | Snapshot composition, subscription, JSON download, show/hide wiring                      |
| `crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/overlay.ts`                        | Create  | Closed shadow host, panel, filters, controls, lifecycle/remount manager                  |
| `crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/badges.ts`                         | Create  | Non-layout-changing badges and scheduled geometry updates                                |
| `crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/*.test.ts`                        | Create  | Activation, store, observer, binding, API, panel, badge, and composition tests           |
| `crates/trusted-server-integration-tests/fixtures/configs/trusted-server.integration.toml`        | Modify  | Enable module availability for browser tests; activation remains query-gated             |
| `crates/trusted-server-integration-tests/fixtures/frameworks/nextjs/app/gpt-diagnostics/page.tsx` | Create  | Controlled slot DOM, hydration, replacement, scrolling, and refresh fixture              |
| `crates/trusted-server-integration-tests/browser/helpers/gpt-stub.ts`                             | Create  | Pre-document deterministic GPT command queue and event bus                               |
| `crates/trusted-server-integration-tests/browser/tests/nextjs/gpt-diagnostics.spec.ts`            | Create  | Real-browser activation, lifecycle, UI lifecycle, binding, export, and non-interference  |
| `crates/trusted-server-integration-tests/README.md`                                               | Modify  | Document the new browser scenario                                                        |
| `docs/guide/integrations/gpt-diagnostics.md`                                                      | Create  | Operator activation, UI, export, limits, privacy, and troubleshooting guide              |
| `docs/guide/integrations-overview.md`                                                             | Modify  | Add the diagnostics integration                                                          |
| `docs/.vitepress/config.mts`                                                                      | Modify  | Add diagnostics guide navigation                                                         |

Generated `crates/trusted-server-js/dist/` output is build evidence, not a
hand-edited source. Follow the repository's existing tracked/ignored behavior;
do not manually edit generated bundles.

---

## Task 1: Add Deployment Configuration and Conditional Activation Delivery

**Files:** Rust integration/config and request-delivery files, example TOML, and
a minimal diagnostics `index.ts` so build discovery remains green.

- [x] **Step 1: Add failing Rust registration/configuration tests.** Cover:
  - no `gpt_diagnostics` section → no registration;
  - `enabled = false` → no registration;
  - `enabled = true` → standalone conditional module availability;
  - the module is excluded from immediate/deferred unified sets and has no proxy/routes/rewriters;
  - deploy validation recognizes every registered builder, including
    `gpt_diagnostics`.
- [x] **Step 2: Add a failing HTML processor ordering test.** Process active HTML
      and assert activation precedes `script#trustedserver-js`, followed by one
      synchronous content-hashed standalone diagnostics module. Assert inactive HTML omits it.
- [x] **Step 3: Define `GptDiagnosticsConfig`.** Use a boolean `enabled` with a
      false default, implement `IntegrationConfig`, and keep the integration
      independent of `[integrations.gpt]`.
- [x] **Step 4: Register the integration.** Add the module and builder entry in
      `integrations/mod.rs`; mark its JS as standalone so ordinary unified and deferred
      bundles never include it.
- [x] **Step 5: Add the server activation gate and early bootstrap.** It must:
  - parse only exact, single `ts_console` directives and fail closed otherwise;
  - establish/clear only the host-only HttpOnly browser-session activation cookie;
  - strip the directive and cookie before generic request handling;
  - write one private document activation flag without exposing the public API;
  - clean the visible URL while preserving state, pathname, unrelated parameters, and fragment;
  - keep active/directive HTML private no-store while standalone static JS stays public;
  - avoid wrapping or retaining any history method.
- [x] **Step 6: Add a minimal `gpt_diagnostics/index.ts`.** It should perform
      only the private activation-flag check and otherwise have no active behavior.
      Later tasks replace the active branch with composition code.
- [x] **Step 7: Add request-gate and browser behavior tests.** In Rust, cover
      enable, disable, clean-cookie persistence, all four recognized values,
      case-sensitive and duplicate rejection, URI sanitation, cookie stripping,
      cache privacy, and idempotent preparation. In Playwright, cover visible URL
      cleanup, query/fragment preservation, cross-tab browser-session persistence,
      deactivation, and a separate browser context starting inactive.
- [x] **Step 8: Add `[integrations.gpt_diagnostics] enabled = false` to
      `trusted-server.example.toml`.** Place it near the existing GPT configuration.
- [x] **Step 9: Add the new Rust source and config validation coverage.** Update
      `migration_guards.rs` and the `config.rs` imports, validation calls, and test
      constants.
- [x] **Step 10: Run focused validation.** Expected: all pass.

```bash
cargo fmt --all -- --check
cargo test-fastly gpt_diagnostics
cd crates/trusted-server-js/lib
npx vitest run test/integrations/gpt_diagnostics
node build-all.mjs
```

- [ ] **Step 11: Commit.** Suggested message:
      `Register GPT diagnostics browser-session activation`.

---

## Task 2: Define the Public Export and Browser API Types

**Files:** `core/types.ts` and a new diagnostics test/type fixture as needed.

- [x] **Step 1: Add the V1 public types.** Define:
  - request-cycle timestamps, valid durations, render facts, and issue flag;
  - slot export with runtime number, optional exact element ID, optional
    `adUnitPath`, visibility facts, binding status/reason, and retained cycles;
  - callback issue with kind, slot identity, timestamp, disposition, and reason;
  - coverage counters and eviction metadata;
  - `GptDiagnosticsExportV1` with `version: 1`;
  - `GptDiagnosticsApi` with the API semantics pinned above.
- [x] **Step 2: Add optional `gptDiagnostics?: GptDiagnosticsApi` to `TsjsApi`.**
      Do not add mutation methods or auction/bid/creative fields.
- [x] **Step 3: Add compile-time/type assertions or a small Vitest fixture.** It
      should construct a valid V1 snapshot and reject provenance fields where
      practical.
- [x] **Step 4: Run TypeScript tests and build.**

```bash
cd crates/trusted-server-js/lib
npx vitest run test/integrations/gpt_diagnostics
node build-all.mjs
```

- [ ] **Step 5: Commit.** Suggested message:
      `Define GPT diagnostics browser API and export schema`.

---

## Task 3: Implement the Bounded Diagnostics Store

**Files:** `store.ts`, public/internal types, and `store.test.ts`.

- [x] **Step 1: Write deterministic failing tests for slot identity.** Verify:
  - the GPT `Slot` object is primary identity through a `WeakMap`;
  - two objects with the same element ID remain separate runtime slots;
  - missing/throwing slot methods produce safe optional display metadata;
  - runtime slot numbers are monotonic and synthetic labels are never exported
    as DOM IDs.
- [x] **Step 2: Write failing lifecycle tests.** Cover:
  - initial request → response → filled/empty render;
  - load and viewability on filled cycles;
  - visibility current/maximum values;
  - sequential refresh numbering (`1`, `2`, `3`) and retained history;
  - callback without a request;
  - response/render/load/viewability compatibility rules;
  - missing response, render, load, and viewability behavior.
- [x] **Step 3: Write failing overlap and order tests.** For one Slot object emit
      request 1, request 2, then response/render. Assert both later callbacks are
      ambiguous with `overlapping_request_cycles` and no cycle is guessed. Also
      verify a uniquely matched out-of-order callback stays matched, adds
      `invalid_event_order`, marks the cycle incomplete, and produces no negative
      duration.
- [x] **Step 4: Write failing coverage tests.** Assert every observed callback is
      exactly one of matched/unmatched/ambiguous and matched invalid-order issues do
      not break the coverage equation.
- [x] **Step 5: Write failing retention tests.** Pin constants at 64 slots, 10
      cycles per slot, and 128 callback issues. Verify least-recently-active slot
      eviction, latest-cycle retention, monotonic numbering, and metadata counters.
      An evicted Slot re-enters only on a future `slotRequested`; earlier non-request
      callbacks remain unmatched without creating a synthetic cycle.
- [x] **Step 6: Implement the store with an injected clock.** Production uses
      `performance.now`; tests use a deterministic clock. Callback mutation is
      synchronous and bounded. Subscriber notifications are scheduled/coalesced,
      not run recursively inside callback mutation.
- [x] **Step 7: Implement valid derived timings only when both endpoints exist
      and are ordered.** Preserve raw timestamps and issues even when a duration is
      suppressed.
- [x] **Step 8: Run focused tests.**

```bash
cd crates/trusted-server-js/lib
npx vitest run test/integrations/gpt_diagnostics/store.test.ts
```

- [ ] **Step 9: Commit.** Suggested message:
      `Add bounded GPT lifecycle diagnostics store`.

---

## Task 4: Install the GPT Observer Through the Command Queue

**Files:** `observer.ts` and `observer.test.ts`.

- [x] **Step 1: Build a narrow GPT test stub.** It must preserve command-array
      identity, provide a PubAdsService event bus, expose stable fake Slot objects,
      and make listener counts observable.
- [x] **Step 2: Write failing tests for listener installation.** Verify exactly
      one listener for each documented event:
      `slotRequested`, `slotResponseReceived`, `slotRenderEnded`, `slotOnload`,
      `impressionViewable`, and `slotVisibilityChanged`.
- [x] **Step 3: Verify installation is queued and idempotent.** Cover GPT absent
      at module evaluation, delayed queue execution, already-loaded custom
      `cmd.push`, repeated installation, and GPT never becoming available. Do not
      poll.
- [x] **Step 4: Verify normalized event forwarding.** Render events retain only
      allowed fields: `isEmpty`, rendered size, `isBackfill`, and
      `slotContentChanged`. Visibility events retain only percentage. No creative,
      line-item, targeting, bidder, or price field may reach the store.
- [x] **Step 5: Add exception-boundary tests.** Throw from slot methods, event
      accessors, and the store. The listener must catch at its top level, warn
      through the existing TSJS logger, and never throw into the fake GPT emitter.
- [x] **Step 6: Add non-interference assertions.** Capture identities for
      `display`, `defineSlot`, `refresh`, `fetch`, XHR, `pushState`, and publisher
      listener functions before installation and assert they remain unchanged.
- [x] **Step 7: Implement the observer and run focused tests.**

```bash
cd crates/trusted-server-js/lib
npx vitest run test/integrations/gpt_diagnostics/observer.test.ts
```

- [ ] **Step 8: Commit.** Suggested message:
      `Observe documented GPT lifecycle callbacks`.

---

## Task 5: Add Exact, Unique Slot Binding

**Files:** `binding.ts` and `binding.test.ts`.

- [x] **Step 1: Write failing exact-binding tests.** Verify only the exact
      `slotElementId` is considered; no prefix/container/inner-ID guessing occurs.
- [x] **Step 2: Write duplicate tests.** Cover:
  - two retained GPT slot records claiming one ID;
  - two connected DOM elements with one ID;
  - one duplicate later removed;
  - inability to verify uniqueness.
    All ambiguous cases must remain unbadged and export an explicit reason.
- [x] **Step 3: Write replacement/disconnection tests.** Replace a uniquely bound
      element with a new connected element using the same ID and verify rebinding.
      Disconnect it and verify the record remains but becomes unbound.
- [x] **Step 4: Write geometry/visibility tests.** Mock rectangles, viewport
      dimensions, and scrolling to distinguish bound, non-zero, and intersecting
      elements. Keep GPT visibility percentage separate from DOM viewport
      intersection.
- [x] **Step 5: Implement targeted uniqueness verification.** Start with
      `getElementById`; verify exact DOM uniqueness with a targeted escaped-ID query.
      If selector escaping/querying is unavailable or throws, degrade to an
      unbound/ambiguous result rather than guessing.
- [x] **Step 6: Add a debounced mutation refresh and a coalesced change
      notification.** It must inspect only observed exact IDs, not arbitrary ad
      patterns or prefix scans.
- [x] **Step 7: Assert the binding manager never mutates publisher slot
      elements.** It must not write attributes, classes, or inline styles.
- [x] **Step 8: Run focused tests.**

```bash
cd crates/trusted-server-js/lib
npx vitest run test/integrations/gpt_diagnostics/binding.test.ts
```

- [ ] **Step 9: Commit.** Suggested message:
      `Bind GPT diagnostics to unique exact slot elements`.

---

## Task 6: Implement Snapshot, Subscription, and JSON Export

**Files:** `api.ts` and `api.test.ts`.

- [x] **Step 1: Write failing snapshot tests.** Combine store and current binding
      facts into V1. Assert:
  - `version` is exactly `1`;
  - `capturedAt` is ISO wall-clock time;
  - page contains current origin/pathname only;
  - query and fragment are excluded;
  - optional `adUnitPath` is included whenever captured;
  - valid durations only, coverage equation, issues, and metadata are preserved;
  - no target, bid, winner, creative, cookie, user, or auction field exists.
- [x] **Step 2: Write failing subscription tests.** Store and binding changes
      should coalesce into fresh snapshots. Unsubscribe prevents later calls, and
      one throwing subscriber does not block others.
- [x] **Step 3: Write failing export tests.** Stub `Blob`, object URL creation,
      anchor click, and revocation. Assert one explicit local download and zero
      fetch/XHR/beacon calls.
- [x] **Step 4: Implement the API factory.** Presentation callbacks are injected
      so API logic does not depend on panel internals. Return new snapshot objects;
      never leak mutable store records.
- [x] **Step 5: Run focused tests.**

```bash
cd crates/trusted-server-js/lib
npx vitest run test/integrations/gpt_diagnostics/api.test.ts
```

- [ ] **Step 6: Commit.** Suggested message:
      `Expose versioned GPT diagnostics snapshots and export`.

---

## Task 7: Build the Closed-Shadow-DOM Panel and Lifecycle Manager

**Files:** `overlay.ts` and `overlay.test.ts`.

- [x] **Step 1: Write failing mount-timing tests.** The host must not mount until
      `document.readyState === "complete"` and two animation frames have elapsed.
      Capture may already contain callbacks before this point.
- [x] **Step 2: Write failing host/isolation tests.** Assert one stable host ID,
      one closed shadow root, scoped styles, and no publisher DOM classes/styles or
      diagnostic attributes.
- [x] **Step 3: Write failing panel-state tests using an injected rendering test
      handle.** Production uses `mode: "closed"`; tests may retain the created root
      reference without exposing it on `window`. Cover:
  - expanded by default and collapse/expand;
  - GPT observed vs Waiting for GPT;
  - callback coverage and issue counts;
  - latest cycle plus expandable history;
  - Filled, Empty, pending, Loaded, Viewable, Incomplete sequence;
  - timings, rendered size, backfill, ad unit path, binding and visibility;
  - All, Visible, Filled, Empty, Pending/Incomplete, Unbound/Ambiguous filters;
  - Export and Close controls.
- [x] **Step 4: Write failing lifecycle tests.** External host removal triggers
      one debounced remount. Framework root/body-child replacement also remounts.
      Close or `hide()` sets document dismissal and prevents remount. `show()` clears
      dismissal without clearing data. Repeated show/hide/removal never creates two
      hosts.
- [x] **Step 5: Implement scheduled rendering.** Store callbacks only request a
      coalesced UI update; no DOM rendering occurs synchronously inside GPT
      callbacks.
- [x] **Step 6: Add basic accessibility and bounded layout.** Include accessible
      names, button semantics, keyboard focus, bounded panel dimensions, and
      scrollable history without expanding product scope.
- [x] **Step 7: Run focused tests.**

```bash
cd crates/trusted-server-js/lib
npx vitest run test/integrations/gpt_diagnostics/overlay.test.ts
```

- [ ] **Step 8: Commit.** Suggested message:
      `Add hydration-safe GPT diagnostics panel`.

---

## Task 8: Add Viewport Badges Without Publisher Layout Mutation

**Files:** `badges.ts`, badge tests, and panel integration.

- [x] **Step 1: Write failing eligibility tests.** A badge exists only when the
      slot has at least one request, a unique connected binding, a non-zero
      rectangle, and viewport intersection. Unbound, ambiguous, zero-size,
      offscreen, or never-requested slots receive none.
- [x] **Step 2: Write failing content tests.** Badge text uses only GPT-observed
      facts: Filled/Empty, rendered size, valid response/render durations, and
      optional viewable timing. Add assertions forbidding Trusted Server, GAM
      winner, Prebid, bidder, or provenance labels.
- [x] **Step 3: Write failing positioning tests.** Badges live in the overlay
      layer, use viewport coordinates, and update after scroll, window resize,
      element resize, relevant mutation, and callback state changes.
- [x] **Step 4: Implement one-animation-frame throttling.** Use `ResizeObserver`
      when available; degrade cleanly when it, `MutationObserver`, or other optional
      layout signals are missing.
- [x] **Step 5: Assert badge handling never mutates publisher elements.** No
      publisher element may receive attributes, classes, or styles before, during,
      or after badge creation/removal.
- [x] **Step 6: Run focused tests.**

```bash
cd crates/trusted-server-js/lib
npx vitest run test/integrations/gpt_diagnostics/badges.test.ts
```

- [ ] **Step 7: Commit.** Suggested message:
      `Add non-interfering GPT slot diagnostic badges`.

---

## Task 9: Compose the Active Integration and Pin the Inactive Path

**Files:** `index.ts`, composition tests, and all diagnostics modules.

- [x] **Step 1: Write failing composition tests.** With the private activation
      flag absent/false, importing the module must expose no API, create no
      `googletag`, install no listeners/observers, and create no host. With the flag
      true, one idempotent runtime is installed.
- [x] **Step 2: Compose in dependency order:** store → observer → binding manager
      → overlay/badges → API. Attach only `window.tsjs.gptDiagnostics`; preserve all
      existing `window.tsjs` fields and methods.
- [x] **Step 3: Put a final exception boundary around active installation.** Log
      a warning and leave publisher behavior untouched if setup fails. Do not expose
      a half-initialized API.
- [x] **Step 4: Verify presentation removal does not stop observer/store
      capture.** Hide, emit callbacks, show, and assert the hidden-period events are
      present.
- [x] **Step 5: Add a full deterministic lifecycle test.** Initial request plus
      refresh, filled/empty outcomes, overlap ambiguity, element replacement,
      snapshot, panel state, and export counts must agree.
- [x] **Step 6: Run the full diagnostics suite, all JS tests, format, and build.**

```bash
cd crates/trusted-server-js/lib
npx vitest run test/integrations/gpt_diagnostics
npx vitest run
npm run format
node build-all.mjs
```

- [x] **Step 7: Run focused Rust/adapter validation after the rebuilt module is
      embedded.**

```bash
cargo test-fastly gpt_diagnostics
cargo check-fastly
```

- [ ] **Step 8: Commit.** Suggested message:
      `Compose the opt-in GPT diagnostics runtime`.

---

## Task 10: Add Controlled Real-Browser Coverage

**Files:** integration-test config, Next.js fixture page, GPT stub helper,
Playwright spec, and integration-test README.

- [x] **Step 1: Enable `[integrations.gpt_diagnostics]` in the source-controlled
      integration-test config.** This only makes the module available; pages remain
      inactive without a directive or tab state.
- [x] **Step 2: Create a dedicated Next.js fixture page.** Include:
  - uniquely identified, sized, scrollable slot elements;
  - controls or fixture hooks for replacement/removal and duplicate DOM IDs;
  - enough route/hydration behavior to exercise host survival;
  - only fictional/example ad unit paths and data.
- [x] **Step 3: Add a pre-document GPT stub helper with `page.addInitScript`.** It
      should expose a command queue, PubAdsService listener registry, stable Slot
      objects, event emitter, method-reference capture, and inspection counters.
      It must not emulate auction behavior.
- [x] **Step 4: Test inactive behavior.** Without activation, assert no public
      API, host, listener registrations, diagnostics-created `googletag`, or
      diagnostic request.
- [x] **Step 5: Test activation/deactivation and URL cleanup.** Cover true/false,
      unrelated query/fragment preservation, persistence across full navigation,
      SPA navigation, and another tab in the same browser context, plus a separate
      browser context starting inactive.
- [x] **Step 6: Test listener and lifecycle behavior.** Emit initial and refresh
      callbacks, assert request numbering, direct render facts, non-negative
      timings, load/viewability augmentations, coverage equation, and panel host
      presence.
- [x] **Step 7: Test deterministic overlap.** Emit two requests for one Slot
      before response/render and assert ambiguous issues with no forced cycle.
- [x] **Step 8: Test binding and badges.** Verify unique bindings in the public
      snapshot and badge geometry through the fixture/test rendering handle or
      browser screenshot evidence. Verify missing/duplicate IDs are unbound or
      ambiguous and have no badge. Do not weaken the production closed shadow root
      solely to make Playwright selectors convenient.
- [x] **Step 9: Test hydration and presentation lifecycle.** Remove the host,
      replace relevant framework DOM, scroll, and wait for settling. Assert external
      removal remounts, Close/hide does not, show does, and capture continues while
      hidden.
- [x] **Step 10: Test JSON export and non-interference.** Capture the download,
      parse V1, compare counts/status with `snapshot()`, and assert captured GPT,
      browser networking, and history method references remain unchanged. The
      one-time cleanup may invoke `replaceState` but must never replace or wrap the
      method reference.
- [x] **Step 11: Test page errors and diagnostic networking.** Collect console
      and page errors, ignore only existing documented fixture noise, and assert no
      diagnostics endpoint/request exists.
- [x] **Step 12: Update the integration-test README scenario table.**
- [x] **Step 13: Run browser integration tests.** The wrapper is authoritative
      because it builds WASM/images, generates config, installs Playwright, and runs
      both framework suites.

```bash
./scripts/integration-tests-browser.sh
```

For iterative runs after prerequisites are prepared:

```bash
cd crates/trusted-server-integration-tests/browser
VICEROY_CONFIG_PATH=../../../target/integration-test-artifacts/configs/viceroy.toml \
TEST_FRAMEWORK=nextjs npx playwright test tests/nextjs/gpt-diagnostics.spec.ts
```

- [ ] **Step 14: Commit.** Suggested message:
      `Cover GPT diagnostics in the browser integration harness`.

---

## Task 11: Document Configuration, Operation, and Limits

**Files:** diagnostics guide, integrations overview, VitePress navigation, and
possibly the existing GPT guide for one cross-link only.

- [x] **Step 1: Add the operator guide.** Document:
  - deployment configuration and independence from `[integrations.gpt]`;
  - all activation/deactivation directives and browser-session behavior;
  - Filled/Empty semantics and explicit non-provenance warning;
  - callback coverage, pending/incomplete distinction, bindings, refresh
    numbering, and badges;
  - API method semantics and V1 export shape;
  - storage bounds and eviction counters;
  - privacy/non-upload behavior;
  - hydration, missing GPT, unbound slots, duplicate IDs, and overlap
    troubleshooting.
- [x] **Step 2: Add the integration to the overview and VitePress Ad Serving
      navigation.** Keep the existing GPT proxy integration distinct from GPT
      diagnostics.
- [x] **Step 3: Use only fictional/example values and domains.**
- [x] **Step 4: Run docs formatting and build/check if available.**

```bash
cd docs
npm run format
npm run build
```

- [ ] **Step 5: Commit.** Suggested message:
      `Document the GPT runtime diagnostics console`.

---

## Task 12: Full Verification and Live-Site Acceptance

### Automated repository verification

Run from a clean worktree after installing the pinned JS/docs dependencies.
Do not substitute bare `cargo test --workspace` or bare workspace clippy.

- [x] **Rust formatting**

```bash
cargo fmt --all -- --check
```

- [x] **Rust tests**

```bash
cargo test-fastly
cargo test-axum
cargo test-cloudflare
cargo test-spin
```

- [x] **Target-matched clippy**

```bash
cargo clippy-fastly
cargo clippy-axum
cargo clippy-cloudflare
cargo clippy-cloudflare-wasm
cargo clippy-spin-native
cargo clippy-spin-wasm
```

- [x] **Integration parity**

```bash
cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity
```

- [x] **JS tests, formatting, and module build**

```bash
cd crates/trusted-server-js/lib
npx vitest run
npm run format
node build-all.mjs
```

- [x] **Docs formatting/build**

```bash
cd docs
npm run format
npm run build
```

- [x] **HTTP integration harness**

```bash
./scripts/integration-tests.sh
```

- [x] **Browser integration harness**

```bash
./scripts/integration-tests-browser.sh
```

- [x] **Optional CLI validation because deploy-time integration validation
      changed**

```bash
./scripts/test-cli.sh
```

### Verification limitations

The browser integration harness passed for Next.js and WordPress. The HTTP
integration harness also passed after restarting Docker, installing Wrangler,
prebuilding the Cloudflare Workers bundle, and running the local wrapper with
`CI=1` so it generated the same integration configuration used in CI. No
source-controlled live publisher URL or credentials are available, so the
environment-specific live publisher checklist remains pending.

### Live publisher acceptance

The repository currently contains the generic integration Playwright harness,
but no source-controlled live publisher URL or credentials. Confirm the
existing environment-specific live harness invocation before this step. Do not
commit a real publisher domain, credentials, storage state, or captured user
data. If no reusable harness exists, execute the checklist with an ephemeral
Playwright script outside source control and attach sanitized counts/results to
the PR.

- [ ] Open a valid publisher session with `?ts_console=true` and a trace-off
      control in the same browser session when access is session-sensitive.
- [ ] Confirm the normal page loads and diagnostics creates no attributable page
      error.
- [ ] Confirm API and host survive load, hydration, scrolling, and a settle
      period.
- [ ] Confirm callback counts are non-zero when the page serves ads and every
      callback satisfies the coverage equation.
- [ ] Confirm all displayed/exported durations are non-negative and consistent
      with callback order.
- [ ] Confirm at least one usable unique visible slot receives a badge; unbound
      or ambiguous slots remain in the panel without one.
- [ ] Trigger or observe a refresh and confirm a new request number rather than
      replacement of the initial cycle.
- [ ] Export V1 and compare slot, cycle, status, coverage, issue, and eviction
      counts with the API/panel.
- [ ] Confirm there are no provenance labels and no auction, bidder, targeting,
      creative-markup, user, query-string, or fragment fields.
- [ ] Confirm `?ts_console=false` on the next document leaves no API, host,
      badges, listeners, or diagnostics-created requests.
- [ ] Record sanitized acceptance evidence and any environmental limitations in
      the PR description.

---

## Suggested Review Checkpoints

- [x] **After Task 1:** Review activation and cache behavior before building the
      larger browser feature. Confirm the active HTML variant is private/no-store,
      inactive HTML omits diagnostics, and the standalone static module remains
      public and cookie-independent.
- [x] **After Task 4:** Review callback matching and listener non-interference.
      This is the data-truth boundary.
- [x] **After Task 6:** Review V1 export for privacy and schema stability.
- [x] **After Task 9:** Review the active/inactive side-effect boundary and
      bundle-size impact.
- [x] **After Task 10:** Review hydration, duplicate-ID, closed-shadow testing,
      and no-network evidence.
- [x] **Before merge:** Compare the final diff against the smaller-PR boundary in
      the spec and remove any auction/provenance work that slipped in.

---

## Risks and Mitigations

| Risk                                                                 | Mitigation                                                                                                                                          |
| -------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------- |
| Initial GPT callback occurs before diagnostics listener installation | Inject activation before core and the standalone diagnostics module synchronously after core at `<head>` start; never patch GPT                     |
| Overlapping refreshes cannot be correlated                           | Preserve ambiguous callbacks with `overlapping_request_cycles`; never guess                                                                         |
| Out-of-order callbacks create negative timings                       | Keep matched disposition, add `invalid_event_order`, and suppress invalid durations                                                                 |
| Framework removes the overlay                                        | Keep capture independent; use a debounced document-level lifecycle manager and distinguish external removal from dismissal                          |
| Duplicate IDs point at the wrong slot                                | Require unique DOM and retained-slot claims; report ambiguous and omit badge                                                                        |
| Closed Shadow DOM is hard to inspect in Playwright                   | Keep production closed; unit-test rendering through retained internal test handles and use public API/geometry/screenshot evidence in browser tests |
| Mutation/scroll work affects publisher performance                   | Inspect observed exact IDs only; debounce mutations and coalesce layout work to one animation frame                                                 |
| Optional browser APIs are missing                                    | Degrade badge/remount updates without stopping GPT capture                                                                                          |
| Diagnostics accidentally affects auction behavior                    | No GPT/browser method replacement, no request gating, no targeting reads/writes, and explicit identity tests for publisher/GPT methods              |
| Export leaks sensitive data                                          | Versioned allowlist schema, origin/pathname only, banned-field tests, explicit local download only                                                  |
| Retention churn becomes unbounded                                    | Fixed limits, deterministic eviction, weak slot identity mapping, bounded callback issues, and counters                                             |
| Inline cleanup/bootstrap conflicts with CSP                          | Keep it minimal and request-scoped; validate on the live environment and report CSP limitations without weakening cookie or cache safety            |
| Diagnostics payload reaches inactive visitors                        | Exclude it from the unified bundle and inject the content-hashed standalone module only into active documents; keep the static response public      |

---

## Definition of Done

- [ ] All spec acceptance criteria are covered by an automated test or explicit
      live acceptance evidence.
- [x] The integration is deployment-disabled by default and browser-session
      inactive by default.
- [x] No callback is silently forced into a request cycle.
- [x] Store, observer, binding, API, panel, and badge behavior are bounded and
      independently tested.
- [x] Inactive pages have no diagnostics listener/API/observer/host side effect.
- [x] Active pages make no diagnostic network request and do not replace GPT,
      browser, or publisher methods.
- [x] Export is V1, allowlisted, local-only, and free of provenance/sensitive
      fields.
- [x] Full target-matched Rust, JS, docs, integration, and browser verification
      passes.
- [ ] Sanitized live-site evidence confirms hydration safety and normal ad
      behavior.
- [x] The final diff remains within the smaller-PR boundary.
