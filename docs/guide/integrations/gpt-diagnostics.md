# GPT Runtime Diagnostics

**Category**: Ad Serving

**Status**: Development
**Type**: Local browser diagnostics

## Overview

GPT Runtime Diagnostics is an opt-in browser console for documented Google Publisher Tag (GPT) lifecycle callbacks. It organizes observed callbacks into per-slot request cycles, displays directly observed timings and render facts, binds slots to exact DOM elements, and downloads the same information as versioned JSON.

The console reports **GPT-observed facts, not creative provenance**. **Filled** means only that GPT emitted `slotRenderEnded` with `isEmpty === false`. It does not mean that Trusted Server, Prebid, Google Ad Manager, a particular bidder, or any other demand source supplied the creative.

The diagnostics integration is independent of the [GPT first-party script integration](./gpt.md). Either integration can be enabled without the other.

## Deployment Configuration

The module is unavailable unless explicitly enabled for the deployment:

```toml
[integrations.gpt_diagnostics]
enabled = true
```

Deployment configuration only makes the module available. Every browser tab remains inactive until explicitly activated.

## Activate or Deactivate a Tab

Open a page with one of these exact, case-sensitive query directives:

| Directive          | Effect              |
| ------------------ | ------------------- |
| `ts_console=1`     | Activate this tab   |
| `ts_console=true`  | Activate this tab   |
| `ts_console=0`     | Deactivate this tab |
| `ts_console=false` | Deactivate this tab |

For example:

```text
https://publisher.example.com/article?ts_console=true
```

A recognized directive is stored in `sessionStorage` and removed from the visible URL. The path, other query parameters, and fragment are preserved. Activation follows full-page and SPA navigation in the same origin and tab. A separately opened tab starts inactive.

If `sessionStorage` is unavailable, a recognized directive applies only to the current document. Unrecognized values are ignored and remain in the URL.

## What the Console Shows

The panel opens expanded after document startup and provides filters for All, Visible, Filled, Empty, Pending/Incomplete, and Unbound/Ambiguous slots.

Each slot may show:

- Exact GPT slot element ID and ad unit path.
- Initial request and numbered refresh cycles.
- Requesting, Response received, Filled, or Empty lifecycle state.
- Loaded and Viewable augmentations when GPT emits those callbacks.
- Incomplete sequence only when an observed event proves a missing or invalid earlier step.
- Valid request-to-response, response-to-render, render-to-load, and render-to-viewable timings.
- Rendered size, backfill, slot-content-change, and GPT visibility facts when exposed by GPT.
- Current DOM binding status and viewport visibility.

Elapsed time alone never changes a pending request to Incomplete. A filled render without a load callback remains Filled with **Load not observed**; missing viewability is not classified as a failure.

### Callback Coverage

Coverage is reported independently for each documented callback:

```text
observed = matched + unmatched + ambiguous
```

Unmatched callbacks have no compatible retained request cycle. Ambiguous callbacks have more than one compatible cycle, such as overlapping refreshes. The console preserves these issues instead of guessing. A uniquely correlated out-of-order callback remains matched and also records an `invalid_event_order` issue.

Coverage describes callback correlation, not fill rate or revenue.

### Slot Binding and Badges

A binding is valid only when one connected DOM element has the exact GPT slot element ID and one retained GPT slot claims that ID. Prefixes, container IDs, and likely-looking elements are never guessed.

A concise viewport badge appears only when a slot:

- Has at least one observed request.
- Has a unique, connected exact binding.
- Has a non-zero rectangle intersecting the viewport.

Missing elements and duplicate DOM or GPT slot IDs remain visible in the panel as Unbound or Ambiguous and receive no badge. Framework replacement of an element with a new unique element using the same ID is rebound automatically.

Badges and the panel live in a closed Shadow DOM. Diagnostics do not add attributes, classes, or inline styles to publisher slot elements.

## Presentation Lifecycle

- **Collapse** reduces the panel while preserving capture.
- **Close** dismisses the presentation for the current document.
- External removal by hydration or DOM reconciliation triggers a debounced remount.
- Explicit Close or `hide()` prevents remount until `show()` is called.
- Capture continues while the panel is hidden.

The visual host mounts only after the document is complete and two animation frames have elapsed. GPT callback capture can begin earlier.

## Browser API

When active, the integration exposes a read-only API:

```js
const diagnostics = window.tsjs.gptDiagnostics

diagnostics.snapshot()
diagnostics.export()
const unsubscribe = diagnostics.subscribe((snapshot) => {
  console.log(snapshot.version, snapshot.slots.length)
})
diagnostics.hide()
diagnostics.show()
unsubscribe()
```

| Method                | Semantics                                                                                            |
| --------------------- | ---------------------------------------------------------------------------------------------------- |
| `snapshot()`          | Returns a fresh V1 snapshot from current store and binding facts                                     |
| `export()`            | Downloads the current V1 snapshot as local JSON; no upload occurs                                    |
| `subscribe(listener)` | Delivers fresh snapshots after coalesced data or binding changes and returns an unsubscribe function |
| `hide()`              | Dismisses presentation without stopping capture                                                      |
| `show()`              | Clears dismissal and remounts presentation without resetting data                                    |

## V1 Export

The allowlisted export contains:

- `version: 1` and an ISO `capturedAt` timestamp.
- Current page origin and pathname, excluding query parameters and fragments.
- Retained slots, binding facts, visibility, and request cycles.
- Directly observed timestamps and render facts.
- Non-negative derived durations only.
- Callback issues and coverage counters.
- Retention eviction counters.

It does not contain auction payloads, targeting, bid values, bidder or winner identity, creative markup, cookies, user identifiers, query strings, or URL fragments.

## Storage and Privacy

Diagnostics data is memory-only and local to the current document. Nothing is automatically transmitted, and no diagnostics endpoint exists.

Initial bounds are:

- 64 retained GPT slot objects.
- 10 retained request cycles per slot.
- 128 retained callback issues.

The oldest eligible record is evicted when a bound is exceeded. Export metadata reports `evictedSlots`, `evictedRequestCycles`, and `droppedCallbacks`.

The integration does not use cookies, `localStorage`, IndexedDB, or server upload for diagnostic records. `sessionStorage` contains only the tab activation state.

## Troubleshooting

### The API or panel is absent

1. Confirm `[integrations.gpt_diagnostics]` is enabled in the deployed configuration.
2. Activate the tab with an exact recognized `ts_console` value.
3. Confirm the Trusted Server script bundle loaded successfully.
4. Use `ts_console=false` and then `ts_console=true` on a new document to reset tab activation explicitly.

### The panel says Waiting for GPT

GPT was not observed after listener installation. Confirm GPT initializes and executes queued `googletag.cmd` callbacks. Diagnostics do not create GPT, poll for it, or patch publisher request behavior.

### Initial callbacks are missing

The integration can observe only callbacks emitted after its listeners execute. Confirm the Trusted Server bundle precedes publisher GPT request code. The console reports coverage gaps rather than reconstructing unobserved activity.

### A slot is Unbound

Confirm `slot.getSlotElementId()` returns a non-empty ID and a connected element with that exact ID exists. Lazy or framework-created elements can bind later without losing request history.

### A slot has Ambiguous binding

Remove duplicate DOM IDs or ensure only one retained GPT Slot object claims the ID. Diagnostics intentionally do not choose one candidate.

### Callbacks are Ambiguous

Overlapping requests for the same GPT Slot object cannot be correlated safely because documented callbacks do not expose a request-cycle identifier. Avoid overlap in controlled tests, or use the issue record as evidence that correlation was not possible.

## Limits

The integration observes six documented PubAdsService events. It does not intercept GPT display, slot definition, refresh, targeting, auction, network, history, or rendering methods. It cannot identify the demand source of a filled creative and should not be used as attribution evidence.

## Related

- [Google Publisher Tags Integration](./gpt.md)
- [Integrations Overview](/guide/integrations-overview)
- [Ad Serving](/guide/ad-serving)
