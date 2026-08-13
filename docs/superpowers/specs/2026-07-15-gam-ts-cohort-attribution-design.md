# GAM `ts=true` attribution for Trusted Server A/B traffic

Date: 2026-07-15

Status: Design

## Problem

A publisher will route a small, cookie-sticky A/B cohort through Trusted Server
while the control cohort continues through the existing production path. The
publisher wants Google Ad Manager (GAM) reporting to identify impressions and
clicks generated on pages served through Trusted Server and compare them with
the unmodified production cohort.

Trusted Server currently adds the slot-level key-value `ts_initial=1` while it
prepares initial GPT slots. That key has a different lifecycle and meaning from
the experiment marker:

- it identifies the initial slot request prepared by Trusted Server;
- it is cleared before later client-side refresh auctions; and
- it is set on matched slots rather than every GPT request on the page.

The experiment needs a document-delivery marker. Every request issued by the
document-local GPT PubAds service after marker installation in an HTML document
successfully rewritten by Trusted Server must contain `ts=true`, including
initial requests, publisher-owned slots, lazy slots, and refreshes. Production
documents cannot be modified, so the control cohort remains unmarked.

## Goals

1. Add `ts=true` to every in-scope GPT PubAds request made during the lifetime
   of an HTML document successfully rewritten by Trusted Server.
2. Leave production/control pages unchanged.
3. Preserve the existing `ts_initial=1` slot-ownership and refresh lifecycle.
4. Support GAM reports that count treatment impressions and clicks and derive
   the control counts within the same experiment scope.
5. Avoid changing auction eligibility, ad delivery, consent behavior, or page
   performance.
6. Define the data-quality checks needed when an unmarked request is used as the
   control baseline.

## Non-goals

- Implement or change the cookie-based A/B router. The experiment infrastructure
  owns sticky cohort assignment and routes only the treatment cohort through
  Trusted Server.
- Prove that a Trusted Server server-side bid won the GAM auction. `ts=true`
  means that the page was delivered through Trusted Server, regardless of
  whether the winning demand was a server-side bid, a direct GAM line item, Ad
  Exchange, or backfill.
- Mark GAM traffic outside a successfully rewritten document's local GPT PubAds
  service. IMA/video SDK requests, direct tags, and server-side GAM requests
  require separate instrumentation and are outside this design. A nested
  document's GPT instance is in scope only when that nested HTML response is
  independently routed through Trusted Server and satisfies the same rewrite,
  ordering, CSP, and audit prerequisites.
- Replace `ts_initial`, `hb_*`, line-item, bidder, or creative reporting.
- Add a client-side analytics beacon or a Trusted Server telemetry event.
- Make GAM click tracking more complete. The marker only segments clicks that
  GAM already records.
- Provide billing-grade or causal experiment analysis from GAM alone.

## Assumptions and prerequisites

- The GPT integration is enabled on every page routed into the treatment cohort.
  A page served through Trusted Server without the GPT integration does not
  receive the GPT head bootstrap and cannot satisfy this design.
- The response enters and successfully completes Trusted Server HTML rewriting,
  contains a literal `<head>` element, and reaches that element before any
  publisher script issues a GAM request. Pass-through or buffered-unmodified
  responses, rewrite failures, and origin markup that omits `<head>` cannot
  satisfy the marker guarantee.
- The publisher Content Security Policy allows Trusted Server's bare inline
  scripts to execute. Initial `adSlots`, the GPT enable flag, the GPT bootstrap,
  and the `bids`/`adInit` invocation are all nonce-less inline scripts; Trusted
  Server does not currently propagate a publisher nonce or update CSP hashes. A
  policy that blocks those scripts makes the initial TS ad stack inert and is
  ineligible at launch even if it allows the synchronous first-party TSJS
  bundle.
- Each in-scope HTML document uses one document-local GPT PubAds service. Nested
  documents are not implicitly covered by a marked parent: each nested HTML
  response must be independently routed through Trusted Server and rewritten to
  receive the marker. IMA/video, direct-tag, server-side GAM, and any nested GPT
  inventory whose document is not independently rewritten must be excluded from
  the experiment and paired reports.
- Treatment and control traffic use the same GAM network and comparable
  inventory. Report filters can isolate the pages and time window eligible for
  the experiment.
- The experiment owner can obtain the expected treatment allocation from the
  cookie router, even though Trusted Server does not read or emit that cookie.
- Publisher code and Trusted Server creative-opportunity slot configuration do
  not reuse `ts` for another meaning, set a slot-level `ts` value, or clear
  page-level targeting after Trusted Server targeting runs. The deployment audit
  must inspect `trusted-server.toml` targeting maps and search publisher code
  for `setTargeting`, `setConfig`, and `clearTargeting` uses that could
  overwrite or remove the reserved key. If such behavior exists, it must be
  resolved before launch; silently filtering operator targeting or wrapping
  publisher GPT APIs is out of scope.

## Existing behavior

The GPT integration has two related pieces:

1. `crates/trusted-server-core/src/integrations/gpt_bootstrap.js` is injected at
   the start of `<head>`. It creates the GPT command queue early and installs
   the minimal `window.tsjs.adInit` implementation used before the richer bundle
   is available.
2. `crates/trusted-server-js/lib/src/integrations/gpt/index.ts` installs the
   richer GPT integration and applies slot-level auction targeting.

Both initial-render paths set `ts_initial=1` on slots handled by `adInit`. The
Prebid refresh integration includes `ts_initial` in its list of stale
slot-targeting keys and clears it before subsequent client-side refresh
auctions. SPA cleanup also clears stale `ts_initial` targeting before applying
new route state.

This behavior is correct for `ts_initial` and must not change. It is not
sufficient for a page-level treatment marker because it does not cover all GPT
slots and intentionally does not persist across refreshes.

## Decision

Add a separate page-level GPT key-value:

```text
ts=true
```

The early GPT bootstrap will enqueue page-level targeting before publisher GPT
commands execute. The enqueue must occur after `window.tsjs` is initialized but
before the existing `if (ts.adInit) return;` guard:

```text
(function () {
  if (typeof window === "undefined") return;
  var ts = (window.tsjs = window.tsjs || {});
  var tag = (window.googletag = window.googletag || { cmd: [] });
  tag.cmd = tag.cmd || [];
  tag.cmd.push(function () {
    try {
      if (typeof googletag.setConfig === "function") {
        googletag.setConfig({ targeting: { ts: "true" } });
      }
    } catch (_) {
      // Attribution must not interrupt the existing bootstrap.
    }
  });

  if (ts.adInit) return;
  // Existing initial-load detector and adInit stub follow.
})();
```

The exact implementation must follow the repository's JavaScript formatting and
defensive checks. The important contract is that the page-level targeting
command is queued by the head bootstrap before the origin page can queue its GPT
setup or request ads. Page attribution is independent of whether the bootstrap
needs to install `ts.adInit`, so the existing guard may skip only the ad-init
stub and detector setup, never the marker enqueue. An unavailable targeting API
may skip only the marker callback; it must not prevent later queued publisher or
Trusted Server callbacks from running.

The existing initial-load detector immediately below the new marker must reuse
the initialized `tag.cmd` reference rather than repeat its current
`(window.googletag = window.googletag || { cmd: [] }).cmd` expression. This
keeps queue initialization and both bootstrap callbacks on one consistent path.

Moving queue initialization above the `ts.adInit` guard intentionally creates a
standard `window.googletag` command-queue stub on every rewritten, GPT-enabled
page, including a page where `ts.adInit` already exists and GPT never loads. The
stub is inert by itself and preserves the marker-before-guard guarantee; it is
not an accidental behavior to remove during implementation review.

The TypeScript GPT bundle will defensively enqueue the same page-level targeting
at module initialization after the existing flag-gated shim block and before
`installTsAdInit()`, only when the existing publisher-page bundle tag carries a
non-executable GPT activation attribute. The HTML pipeline adds that attribute
when the GPT integration is enabled. A pre-existing `ts.adInit` is already
covered by placing the bootstrap marker before the guard and is not a reason for
the fallback.

The fallback exists to preserve delivery-path attribution if the inline
bootstrap unexpectedly stops executing while the synchronous first-party bundle
still runs before publisher GPT. It does not recover `adSlots`, bids, the
`adInit` invocation, or the initial TS auction: those are also nonce-less inline
scripts. A fallback-only marker therefore still truthfully means "page delivered
through Trusted Server," but it also indicates a deployment state that was
ineligible at launch. Synthetic validation must treat that state as a
measurement incident, pause interpretation, and exclude the affected time window
from both paired reports if the incident contaminates collected results. GAM
cannot distinguish fallback-only pages from normally executing treatment pages
because both intentionally use the same marker.

Neither targeting path can cover a response that was not HTML-rewritten, markup
without `<head>`, a policy that blocks both injected paths, or a publisher GPT
request issued before the injected head content runs. Those are deployment
eligibility and coverage-validation concerns, not runtime conditions the
targeting code can repair.

The implementation uses GPT's current page-level `googletag.setConfig` API
rather than the deprecated `pubads().setTargeting()` API. See
[GPT configuration API migration](https://developers.google.com/publisher-tag/guides/config-migration).
Page-level targeting is the right scope because GPT applies it to all slots
associated with the `pubads` service. Once installed, it remains effective for
initial, lazy, and refreshed requests for the life of the page. Existing slot
targeting may add or override other keys without requiring Trusted Server to
discover every publisher slot.

GPT merges page-level targeting per key across `setConfig` calls. Enqueuing
`ts=true` from both Trusted Server paths is therefore idempotent, and a
publisher call that sets an unrelated targeting key preserves `ts`. The explicit
clear operations are a per-key `null`, a whole-targeting `null`, or the
equivalent legacy `pubads().clearTargeting()` calls. See
[GPT key-value targeting](https://developers.google.com/publisher-tag/guides/key-value-targeting).

`ts` is intentionally not added to the slot-targeting cleanup arrays. Those
arrays manage per-auction state. Clearing page-level `ts` during refresh or SPA
navigation would incorrectly move a treatment page into the unmarked control
cohort.

## Attribution contract

### Treatment

An in-scope GAM request is in the treatment cohort when it contains:

```text
ts=true
```

The marker means:

> The containing page was delivered through Trusted Server.

It does not mean:

- a Trusted Server bidder returned a bid;
- a Trusted Server bid won;
- Trusted Server rendered the winning creative; or
- the request was the first impression for the slot.

### Control

The production path cannot be changed. Within the exact experiment inventory,
time window, and publisher scope, an unmarked GAM request is treated as control.

This is an inference rather than an explicit `ts=false` assertion. A treatment
request that loses its marker would be misclassified as control. The rollout
therefore requires coverage checks that compare the observed GAM treatment share
with the A/B router's expected cookie cohort share.

### Relationship to `ts_initial`

| Key            | Scope      | Lifetime                     | Meaning                                   |
| -------------- | ---------- | ---------------------------- | ----------------------------------------- |
| `ts=true`      | Page-level | Entire browser page lifetime | Page was delivered through Trusted Server |
| `ts_initial=1` | Slot-level | Initial TS-managed request   | Initial slot request was prepared by TS   |

The two keys answer different questions and coexist. No code or report should
infer that one is an alias for the other. The value contract is exactly
`ts=true`: do not emit, accept, or report any other value (for example `ts=1`),
and do not dual-write an alternative key name such as `trusted_server`.

## Request lifecycle

```text
Sticky A/B cookie
  -> control: browser receives production page
       -> publisher GPT runs without the `ts` key
  -> treatment: request is routed through Trusted Server
       -> GPT head bootstrap queues page-level `ts=true`
       -> GPT bundle defensively queues the same marker
       -> GPT library drains the command queue
       -> publisher and TS define/display/refresh slots
       -> every in-scope PubAds request carries `ts=true`
```

The marker covers:

- Trusted Server-defined initial slots;
- publisher-defined slots reused by Trusted Server;
- publisher slots that are not part of a Trusted Server creative opportunity;
- slots created lazily after initial page load;
- publisher-initiated refreshes;
- Prebid-managed refreshes; and
- SPA route changes within the same browser document.

All bullets refer to slots using the same document-local GPT PubAds service.
Requests from IMA/video SDKs, direct tags, or server-side GAM integrations are
not covered merely because the containing document is marked. A nested GPT
instance is marked only when Trusted Server separately rewrites that nested
document and injects the attribution paths into its own `<head>`.

A full browser navigation creates a new page and repeats cookie-based routing.
The new page receives the marker only when that navigation is routed through
Trusted Server.

## Component changes

### Early GPT bootstrap

`crates/trusted-server-core/src/integrations/gpt_bootstrap.js` owns the
behavior. It will set the page-level key in its earliest GPT command callback,
before the `ts.adInit` early-return guard. The targeting code stays inside the
existing raw bootstrap script returned by `head_inserts`; it must not add a
third head insert.

The operation must be idempotent. Calling
`googletag.setConfig({ targeting: { ts: 'true' } })` more than once with the
same value is harmless, but the bootstrap should avoid adding a new global state
machine solely for deduplication.

The bootstrap already binds the local variable `ts` to the `window.tsjs`
namespace, so the targeting key `ts` and that variable are unrelated names that
sit only a few lines apart. Add a clarifying comment at the targeting call so a
maintainer does not read the key as the namespace. The value is the string
`'true'`, never the boolean `true`: GPT targeting values must be strings.

### TypeScript GPT bundle fallback

`crates/trusted-server-js/lib/src/integrations/gpt/index.ts` will add a small
`installTrustedServerPageTargeting()` helper and call it during GPT module
initialization after the existing flag-gated `installGptShim()` block and before
`installTsAdInit()` when the publisher-page bundle's activation attribute is
present. The helper creates or reuses the standard GPT command queue, enqueues
the same defensive `setConfig({ targeting: { ts: 'true' } })` call, and does not
read the experiment cookie or wait for an auction. Extend the local `GoogleTag`
interface with optional `setConfig?(config: Record<string, unknown>): void` so
the fallback remains defensive when the API is unavailable.

The bootstrap remains the primary path because it is injected first. The bundle
call is a redundant fallback and must not delay module initialization, create a
request, or add slot-level targeting. A plain GPT module import without the
activation attribute must preserve the existing runtime-gating contract and must
not create `window.googletag`.

### Non-executable bundle activation

The publisher HTML pipeline in
`crates/trusted-server-core/src/html_processor.rs`, using a separate,
publisher-page-only tag helper in `crates/trusted-server-core/src/tsjs.rs`, will
add a `data-ts-gpt-enabled="true"` attribute to the existing synchronous
`#trustedserver-js` bundle tag when GPT is enabled. The attribute is data, not
an inline executable, so CSP can block the inline GPT head inserts while still
allowing the external bundle to detect that it owns page attribution.

At module initialization, the GPT bundle captures `document.currentScript` and
requires that executing synchronous script to carry `data-ts-gpt-enabled="true"`
before the fallback may create a GPT stub. It must fail closed when the
executing script cannot be identified. Do not authorize activation through a
global `#trustedserver-js` lookup: the generic unified tag uses the same ID in
creative and test contexts, and duplicate IDs could select the wrong element.
Binding the signal to the executing tag keeps the activation decision explicit
and testable without relying on an inline global flag.

The existing `window.__tsjs_gpt_enabled` flag continues to activate
`installGptShim()` when inline scripts run. It cannot activate the CSP fallback
because the server sets it from an inline head insert—the execution path CSP may
block. Migrating shim activation to the data attribute is out of scope; module
initialization preserves the current flag-gated shim installation, then runs the
attribute-gated page-targeting helper, then installs `ts.adInit` and the
remaining GPT bundle hooks.

This signal must be limited to the publisher-page bundle generated from the
enabled integration registry. Do not infer activation merely because the GPT
module exists in an all-modules bundle: creative and test tooling can load that
bundle outside the publisher GPT integration. Do not add a new script tag or
change the integration's existing head-insert count.

Extend the `tsjs.rs` and `html_processor.rs` tests to prove that the existing
publisher-page bundle tag gains the activation attribute only when GPT is in the
enabled immediate module set, remains a single external tag, and omits the
attribute for non-GPT publisher bundles and generic all-modules tags. Bundle
tests must also prove that an unrelated or duplicate element with
`id="trustedserver-js"` cannot activate the fallback.

### GPT Rust integration tests

`crates/trusted-server-core/src/integrations/gpt.rs` already tests the embedded
bootstrap returned by `head_inserts`. Extend those tests to prove that:

- the bootstrap contains page-level `ts=true` targeting;
- the marker enqueue appears before the `if (ts.adInit) return;` guard;
- the targeting setup is queued before `ts.adInit` can issue `display` or
  `refresh`;
- the existing `ts_initial` marker remains present; and
- the enabled integration without `slim_prebid_url` still emits exactly the
  existing two head inserts, proving the marker was added to the bootstrap
  instead of a new tag.

### Bootstrap execution tests

Add `crates/trusted-server-js/lib/test/integrations/gpt/gpt_bootstrap.test.ts`
as a Vitest/jsdom behavioral test for the raw bootstrap. The test reads
`crates/trusted-server-core/src/integrations/gpt_bootstrap.js` with Node's
`readFileSync`, resolves the source path relative to `import.meta.url` rather
than the process working directory, and creates an isolated
`new JSDOM(html, { runScripts: 'outside-only' })` realm. Before evaluating the
exact source with that realm's `window.eval`, the test must assert that
`globalThis === window` and `typeof global === 'undefined'` inside the realm.
This is required because Vitest's default jsdom `window.eval` runs in Node's
realm under the current configuration. The harness supplies a minimal mocked GPT
command queue and `pubads` service. It must not evaluate the source in Node's
global context, copy the bootstrap into a test fixture, or add a JavaScript
runtime dependency to Rust.

The harness must prove that:

- the attribution callback is queued before a publisher callback added after the
  injected bootstrap;
- draining the queue calls `googletag.setConfig` with page-level `ts=true`
  before the publisher callback runs;
- a pre-existing `ts.adInit` does not prevent the attribution callback from
  being queued or executed;
- a publisher callback queued after the bootstrap still runs when
  `googletag.setConfig` throws;
- an unavailable or throwing `googletag.setConfig` does not prevent the existing
  `disableInitialLoad` wrapper from being installed;
- `ts.adInit` remains installed when attribution setup is unavailable or throws;
  and
- calling the wrapped `disableInitialLoad` still records
  `ts.gptInitialLoadDisabled`.

### Bundle fallback tests

Extend `crates/trusted-server-js/lib/test/integrations/gpt/index.test.ts` using
its existing dynamic-import and `vi.resetModules()` pattern. Prove that module
initialization with the bundle activation attribute queues page-level `ts=true`
after any existing flag-gated shim installation and before installing
`ts.adInit`, that it reuses an existing GPT command queue, and that unavailable
or throwing `setConfig` does not stop the remaining GPT module installers.
Retain the existing assertion that a plain module import without an activation
signal does not create `window.googletag`. A duplicate call after the bootstrap
must remain safe and must not create another script or network request.

### Slot cleanup constraints

No refresh-lifecycle change is required. In particular:

- do not add `ts` to `TS_REFRESH_TARGETING_KEYS`;
- do not add `ts` to `TS_BASE_TARGETING_KEYS`;
- do not rename or remove `TS_INITIAL_TARGETING_KEY`; and
- do not copy `ts` onto individual slots.

Leaving these components unchanged is part of the design: slot cleanup cannot
remove a page-level key set through `googletag.setConfig`.

### Documentation

Document the distinction between page-level `ts=true` and slot-level
`ts_initial=1` in `docs/guide/integrations/gpt.md`, near the existing command
queue documentation. Include the GAM setup and reporting preconditions below; do
not add this current integration to a planned-future GAM document.

## GAM configuration

GAM configuration is a deployment prerequisite and must be completed before the
experiment starts because key-value reporting is not retroactive.

The request contract is `ts=true`: key name `ts`, predefined value `true`. The
key name is provisional pending a GAM preflight (see issue #1027). The GPT/GAM
`CustomTargetingKey.name` (the code sent in the ad request) is documented as
limited to 10 characters in the SOAP/REST API, which would rule out a fuller
name such as `trusted_server` (14); other Help Center material implies 20, and
these describe different provisioning surfaces, so the enforced limit must be
confirmed in the target network before the contract is fixed. If the confirmed
limit permits a longer name, a more descriptive, less collision-prone key is
preferred over `ts`. Because `ts` is short, it carries a real collision risk with
common publisher timestamp or cache-buster keys, which makes the cross-system
collision audit a hard launch gate, not a formality. If any publisher, Trusted
Server configuration, or GAM object already uses `ts`, the experiment must stop
until the collision is removed or the contract is explicitly revised everywhere
before treatment traffic begins.

1. In **Inventory > Key-values**, create or verify a key whose request name is
   the finalized marker key (`ts` pending the preflight in issue #1027). When the
   key is created, confirm the enforced key-name and value length limits on the
   provisioning surface actually used: the SOAP/REST
   [`CustomTargetingKey`](https://developers.google.com/ad-manager/api/reference/v202511/CustomTargetingService.CustomTargetingKey)
   documents a 10-character key-`name` limit and a 40-character value limit, but
   the UI surface may differ, so verify against the target network rather than
   assuming.
2. Use a predefined value named `true`.
3. Enable `ts` as a dedicated reportable Enhanced key-value dimension. If the
   network does not support Enhanced key-value dimensions, use a report filtered
   to the single legacy key-value `ts=true`; never sum unfiltered legacy
   **Key-values** dimension rows.
4. Reserve `ts` for Trusted Server page attribution.
5. Audit existing publisher GPT code and every GAM object that consumes custom
   targeting for an existing `ts` key before deployment. This includes line
   items, proposal line items, rules, protections, yield configuration, and any
   network-specific custom-targeting surface.
6. Audit every `CreativeOpportunitySlot.targeting` map from all effective
   `trusted-server.toml` configuration sources. The arbitrary operator-supplied
   map is copied to GPT slots, where a slot-level `ts` value would override the
   page-level marker. Any occurrence is a launch blocker; do not silently
   discard it because that could change established operator targeting.
7. Audit publisher code for every operation that can remove or supersede the
   marker after initial GPT setup. Search for `setConfig({ targeting: null })`,
   a `ts: null` or different `ts` value, `pubads().clearTargeting()` with no key
   or with `ts`, and slot-level `ts` targeting. Account for equivalent calls
   assembled dynamically.

The audit is a hard precondition. If `ts` already has another meaning, or any
GAM object targets or acts on `ts=true`, the experiment owner must resolve the
collision before deployment. The measurement marker is not intended to change ad
eligibility, pricing, protection, or routing. A pre-existing targeting consumer
for `ts=true` would make the A/B test measure a traffic or demand change at the
same time as Trusted Server delivery.

Undefined values do not appear in standard key-value reports even when the key
is reportable, so value `true` must exist before treatment traffic begins. See
[Add key-values](https://support.google.com/admanager/answer/9796369) and
[Report on targeting keys](https://support.google.com/admanager/answer/14528835).

## Reporting and comparison

### Report scope

Every comparison must apply identical filters for:

- publisher/network;
- experiment start and end time;
- sites or inventory included in the cookie experiment;
- ad units and formats;
- geography and device categories, when used; and
- any consent or traffic-quality exclusions.

Do not compare the TS cohort with all unmarked network traffic unless all that
traffic is eligible for the same experiment. Likewise, exclude smoke tests,
direct hits, operations traffic, and any other TS-served page outside the cookie
experiment. The marker identifies the delivery path, not the router's cohort
assignment, so all such requests also carry `ts=true` when the GPT integration
runs.

The route owner must use router or access logs to prove that non-experiment TS
traffic is absent from the eligible inventory during the measurement window. If
such traffic cannot be prevented and has no independent inventory or reportable
dimension, GAM cannot remove it from Report B because its marker is identical to
the cohort marker; the experiment must not launch. Record the owner, query,
expected zero threshold, and response procedure in the experiment runbook.

### Cohort calculations

Use two reports with identical date boundaries, time zone, inventory filters,
traffic-quality filters, and metric definitions:

1. **Report A — experiment total.** Do not include **Placement**, legacy
   **Key-values**, **Targeting**, **Yield group**, or another dimension that can
   represent one event more than once. This report provides one non-duplicated
   total for every metric in the eligible experiment scope.
2. **Report B — TS treatment.** Use the dedicated Enhanced `ts` dimension
   filtered to `ts=true`. If Enhanced key-value dimensions are unavailable, use
   the legacy **Key-values** dimension filtered to exactly `ts=true` and do not
   sum any other key-value rows. Do not add **Placement**, **Targeting**,
   **Yield group**, or any unrelated dimension that can represent the filtered
   treatment event more than once.

The legacy **Key-values** dimension can emit the same impression or click on
multiple rows when a request contains multiple key-values. It therefore cannot
provide Report A or a summable totals row. See
[Avoid double counting report totals](https://support.google.com/admanager/answer/7642799).

For this paired report scope, define:

```text
total_impressions = Report A impressions
ts_impressions = Report B impressions
prod_impressions = total_impressions - ts_impressions

total_clicks = Report A GAM-recorded clicks
ts_clicks = Report B GAM-recorded clicks
prod_clicks = total_clicks - ts_clicks
```

If the selected GAM report exposes an explicit unassigned or `(not set)` row,
that row may be used only as a cross-check. The paired Report A minus Report B
calculation remains the control definition because production cannot send an
explicit value. The experiment owner must retain both report definitions with
the results so later analysis can verify that their filters and metrics match.
Export both reports after the same GAM reporting-latency and invalid-traffic
adjustment window. If GAM restates one report, rerun the pair before applying
the subtraction.

Use total metrics when the goal includes all GAM demand sources. GAM's
`Ad server impressions` and `Ad server clicks` metrics exclude Ad Exchange and
AdSense, so those narrower metrics should only be used when that exclusion is
intentional. GAM counts impressions and clicks according to its own tracking
rules; adding `ts=true` does not create new impression or click trackers. See
[Counting impressions and clicks](https://support.google.com/admanager/answer/2521337).

Both reports must use the same non-targeted impression and click metric names.
Do not use targeted-impression or targeted-click metrics for this attribution:
`ts` is intentionally forbidden from line-item targeting, so metrics limited to
keys used for targeting do not represent the requested delivery-path cohort.
Record the exact selected GAM metric names with the saved report definitions
before launch.

### Unequal cohort sizes

The treatment cohort is intentionally small, so raw TS and production totals are
not directly comparable. Reports should show the raw counts, but experiment
conclusions should compare normalized measures where compatible metrics are
available:

- impressions per GAM ad request;
- fill rate;
- clicks per impression (CTR); and
- revenue per thousand impressions or requests.

Impressions per routed pageview or per assigned visitor require a denominator
from the A/B router or site analytics. GAM alone cannot identify unmarked
production pageviews that made no ad request. Any cross-system experiment
analysis is outside the implementation but should use the same time and
eligibility filters.

### Data-quality checks

During the experiment, monitor:

1. observed `ts=true` ad-request or impression share versus the router's
   expected treatment allocation;
2. scheduled synthetic marker presence on initial, lazy, and refreshed treatment
   requests;
3. scheduled synthetic marker absence on production requests;
4. non-experiment traffic served through Trusted Server;
5. unexpected `ts` values or line-item targeting;
6. report freshness and GAM invalid-traffic adjustments.

A gap between expected and observed treatment share is a measurement incident,
not evidence of production performance, until missing-marker and request-volume
differences are ruled out. Because router assignment and GAM delivery normally
use page or visitor counts versus ad-request or impression counts, this share
comparison is a diagnostic rather than direct proof of marker coverage. It can
measure coverage directly only when the router or site analytics supplies a
matched request- or page-level denominator.

Checks 2–3 use a scheduled synthetic browser crawl of representative experiment
URLs. The crawler supplies known treatment and control cookies, captures GAM
network requests, and triggers initial, lazy, and refreshed slots. A failed
marker assertion is an operational measurement incident. This is external
validation rather than a site beacon or Trusted Server telemetry event; if the
experiment owner cannot operate the crawl, checks 2–3 become documented manual
samples and must not be represented as continuous production metrics.

On treatment URLs with matched creative opportunities, the crawler must also
detect the fallback-only CSP state: capture CSP violations and verify that the
injected `adSlots`, `bids`, and initial `adInit` handoff executed. A page that
has `ts=true` only because the external bundle ran, while those inline scripts
were blocked, remains correctly marked as TS-delivered but raises a measurement
incident. Since GAM cannot separate those requests afterward, the incident owner
must pause interpretation and exclude the affected time range from both reports
when clean boundaries can be established; otherwise the experiment result is
invalid.

## Failure handling

The marker is best-effort instrumentation and must never block ads or page
delivery.

- If GPT never loads, there is no GAM request to classify.
- If a response is not successfully HTML-rewritten, has no literal `<head>`, or
  issues an in-scope GPT request before the injected head content runs, neither
  targeting path can mark that request. Such traffic is ineligible for the
  experiment and must be detected before launch or excluded from analysis.
- If CSP blocks Trusted Server's nonce-less inline scripts, the initial TS ad
  stack is inert and the page is ineligible even when the first-party bundle
  queues the attribution marker. The fallback prevents a TS-delivered page from
  leaking into the inferred control cohort; it does not make the deployment
  healthy. If CSP blocks both inline scripts and the bundle, attribution also
  fails.
- If `googletag.setConfig` is unavailable when a queued command runs, the
  targeting step is a defensive no-op and must not throw. Supported treatment
  deployments must use a GPT version with the configuration API; browser/GAM
  validation detects an unsupported or missing API before experiment launch.
- If publisher code or a Trusted Server creative-opportunity targeting map
  applies slot-level `ts`, GPT gives the slot-level value precedence. The
  deployment audit prevents this collision; runtime filtering or interception is
  out of scope because it could silently alter established targeting behavior.
- If publisher code calls `setConfig({ targeting: null })`, sets `ts: null` or a
  different value, calls legacy `pubads().clearTargeting()` for all keys or for
  `ts`, or applies slot-level `ts`, the effective marker can be removed or
  superseded. The publisher-code audit and refresh validation are required
  because this design deliberately does not intercept those APIs.
- If the marker is absent on a treatment request, GAM classifies it with the
  unmarked baseline. Coverage monitoring is the mitigation.
- GAM configuration or reporting failures do not affect ad serving.

No retry, beacon, cookie read, backend request, or persistent client state is
added by this feature.

## Privacy and consent

`ts=true` contains no unique user identifier, cookie value, page URL, or auction
data. It describes only the delivery path of the current document. Because only
the cookie-sticky treatment cohort is routed through Trusted Server for this
experiment, the value also reveals treatment-path membership for that GAM
request. It is therefore cohort information even though it does not expose the
assignment cookie or identify a person by itself.

The implementation does not read the experiment cookie. Routing happens before
Trusted Server handles the request. Existing consent gates continue to decide
whether GAM requests or auctions occur. The marker does not create an ad request
that would otherwise be suppressed. Before enabling the key, the experiment
owner must complete the publisher's privacy/data-governance review for sending
this treatment-path attribute to GAM and confirm that existing consent and
data-use terms cover it.

## Testing strategy

### Automated tests

1. Extend Rust GPT head-insert tests to assert page-level `ts=true` targeting is
   in the existing raw bootstrap, occurs before the `ts.adInit` guard and any
   bootstrap `display()` or `refresh()` call, and does not change the expected
   head-insert count.
2. Extend the TSJS tag and HTML processor tests to prove the non-executable GPT
   activation attribute appears only on the enabled publisher-page bundle and
   adds no script tag.
3. Add the Vitest/jsdom raw-bootstrap harness described above. Exercise the
   bootstrap with `googletag.setConfig` available, unavailable, and throwing,
   and prove a later publisher callback still runs in every case. Set
   `window.tsjs.adInit` before evaluation and prove the marker still runs.
4. Extend the existing GPT bundle tests to prove module initialization queues
   the fallback marker and remains non-blocking when `setConfig` is unavailable
   or throws.
5. Retain assertions for `ts_initial=1` to prevent accidental replacement.
6. Retain refresh tests proving stale `ts_initial` and `hb_*` slot targeting is
   cleared. Add an explicit assertion or source-level invariant that page-level
   `ts` is not included in slot cleanup lists.
7. Extend creative-opportunity configuration tests to demonstrate that an
   operator targeting map is forwarded verbatim, documenting why the deployment
   audit must reject a configured `ts` key rather than assuming the client
   overwrites or filters it.
8. Run the project-required target-matched Rust and JavaScript checks for the
   touched files.

### Browser/GAM validation

Before experiment launch:

1. Load a treatment page using a known treatment cookie.
2. Confirm the initial in-scope GAM request contains `ts=true` using GPT
   Publisher Console, Delivery Inspector, or the browser network panel.
3. Trigger a lazy slot and a refresh; confirm both requests still contain
   `ts=true`.
4. Load the equivalent production page with a control cookie and confirm the key
   is absent.
5. Confirm `ts_initial=1` remains limited to its existing initial-slot
   lifecycle.
6. Validate the deployed CSP by proving `adSlots`, the GPT bootstrap, `bids`,
   and the initial `adInit` handoff execute on a representative page with
   matched slots. A page that runs only the external bundle is ineligible even
   if the fallback marker appears.
7. Set an unrelated page-level targeting key after `ts=true` and confirm both
   keys remain on a later request. Treat an explicit page-level or per-key clear
   as a failed publisher-code audit, not supported behavior.
8. Validate that IMA/video, direct-tag, server-side GAM, and nested GPT
   inventory without an independently TS-rewritten document is absent from the
   experiment and paired report scope. Directly validate any independently
   rewritten nested documents that are intentionally included.
9. Run a short GAM report and verify treatment totals appear under `ts=true`
   while overall totals remain unchanged apart from normal reporting latency.

## Rollout

1. Audit response eligibility, including HTML rewriting, `<head>` ordering, CSP,
   and publisher GPT calls that could precede or remove the marker.
2. Audit the `ts` key across publisher GPT code, effective `trusted-server.toml`
   creative-opportunity targeting maps, and every GAM custom-targeting consumer.
3. Prove through router or access logs that non-experiment traffic is excluded
   from the TS route or independently separable in both paired GAM reports.
4. Exclude IMA/video, direct-tag, server-side GAM, and nested GPT inventory
   whose document is not independently rewritten. Inventory in intentionally
   rewritten nested documents must pass the same request and report validation
   as the top-level document.
5. Create and enable the reportable GAM key and predefined value.
6. Deploy the Trusted Server marker before assigning experiment traffic.
7. Provision the scheduled synthetic crawl, assign an incident owner, and obtain
   one successful treatment/control run covering initial, lazy, refreshed, and
   CSP execution checks.
8. Validate treatment and control requests manually.
9. Start the small cookie-sticky cohort.
10. Compare observed GAM treatment share with the router allocation before using
    the results for performance decisions.
11. Monitor normalized metrics over a sufficiently large window; do not infer a
    treatment effect from unequal raw totals.

Rollback stops adding the page-level key to newly loaded documents. Already-open
documents—including long-lived SPA sessions—retain page-level targeting and may
continue issuing marked lazy or refreshed requests after the code rollback.
Record the rollback timestamp, end the experiment reports at the last clean
pre-rollback boundary, and exclude the post-rollback drain interval from both
cohorts. The drain ends only after router/access logs and GAM show no remaining
`ts=true` traffic for one complete agreed reporting interval and a fresh
synthetic navigation confirms that new documents are unmarked. If marked traffic
persists, the interval remains excluded rather than being inferred as control.
Historical GAM rows recorded while the key was active remain valid, and the GAM
key may stay defined and reportable for historical analysis.

## Alternatives considered

### Reuse `ts_initial=1`

Rejected because the key is slot-level, covers only TS-managed initial slots,
and is deliberately cleared on refresh. Changing its lifecycle would also break
its existing ownership semantics.

### Add slot-level `ts=true` in `adInit`

Rejected because it would miss publisher-owned or lazy slots that do not pass
through `adInit`, and existing refresh cleanup could remove it. It would measure
auction participation rather than page delivery.

### Set `ts=true` only in the bootstrap

Rejected as the sole path. Placing the enqueue before the existing `ts.adInit`
guard correctly handles a pre-installed ad-init implementation. The bundle is
not needed for that case. It remains useful when the inline script unexpectedly
stops executing but the synchronous first-party bundle still runs: without the
fallback, a TS-delivered treatment page would be silently inferred as control.
The fallback does not rescue the simultaneously blocked TS ad-stack scripts, so
that state is an incident rather than an eligible deployment mode.

### Use a longer descriptive key name such as `trusted_server`

A descriptive name would lower the collision risk that the short `ts` key
carries. Rejected because GAM limits a custom-targeting key name to 10
characters, so `trusted_server` (14) cannot be created as a reportable key. The
short `ts` name is therefore mandatory, and the cross-system collision audit is
the compensating control. Do not dual-write `ts` alongside any longer alias: two
names for one cohort would increase GAM setup and audit surface and permit
silent drift between reports. Only `ts=true` is valid.

### Configure `ts=true` in creative-opportunity slot targeting

Rejected because creative-opportunity targeting applies only to matched slots.
The experiment requirement covers every request from each successfully rewritten
document's local GPT PubAds service.

### Rewrite `cust_params` on GAM network requests

Rejected because it depends on GPT's internal request construction and encoding,
adds interception risk, and duplicates a supported GPT targeting API.

### Mark production explicitly with `ts=false`

Preferred in a fully controlled experiment, but unavailable because the
production path cannot be changed. The design documents the resulting unmarked
baseline limitation and requires coverage checks.

## Acceptance criteria

1. For a deployment that satisfies the documented GPT-integration, HTML-rewrite,
   `<head>` ordering, CSP, reserved-key, request-scope, and targeting-cleanup
   prerequisites—and in which a Trusted Server marker callback runs before the
   first request—every request from that rewritten document's local GPT PubAds
   service carries `ts=true`, including initial, lazy, refreshed,
   publisher-owned, and SPA-route requests. A nested document is covered only
   when its own HTML response independently satisfies the same prerequisites.
2. Production/control pages remain unmodified and do not carry `ts` from this
   feature.
3. `ts_initial=1` retains its current slot-level initial-request lifecycle.
4. The new key is not cleared by Prebid refresh or SPA slot cleanup.
5. No publisher code, effective Trusted Server creative-opportunity targeting
   map, or GAM custom-targeting consumer changes ad eligibility, pricing,
   protection, routing, or the marker value because of the measurement key.
6. The marker adds no unique identifier, cookie value, network request, or
   blocking work. Its disclosure of treatment-path membership to GAM has passed
   the publisher's privacy/data-governance review.
7. GAM can report treatment impressions and clicks under `ts=true`, and the
   control counts can be derived within the same experiment scope using
   identical non-targeted metrics.
8. Known treatment requests are validated directly for marker presence, and the
   observed GAM treatment share is compared diagnostically with the router's
   expected cohort allocation before experiment results are interpreted.
9. Non-experiment TS traffic is absent from the route during the measurement
   window or excluded from both paired GAM reports with identical filters.
10. Only `ts=true` is emitted and reported; no other value (for example `ts=1`)
    and no alternative key name (for example `trusted_server`) alias or
    dual-write compatibility path exists.
11. CSP-compatible inline execution is proven before launch. A fallback-only
    page remains attributed to treatment but raises an incident and cannot be
    treated as a healthy experiment page.
12. Rollback reporting excludes already-open documents until observed marked
    traffic has drained according to the documented boundary rule.
