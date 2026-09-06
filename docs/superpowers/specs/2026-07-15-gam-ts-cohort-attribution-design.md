# GAM `ts=true` attribution for Trusted Server A/B traffic

Date: 2026-07-15

Updated: 2026-08-14

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

The experiment needs a document-delivery marker. When attribution is explicitly
enabled, every request issued by the document-local GPT PubAds service after
Trusted Server rewrites and emits the document head must contain `ts=true`,
including initial requests, publisher-owned slots, lazy slots, and refreshes.
Production documents cannot be modified, so the control cohort remains
unmarked.

## Goals

1. Add `ts=true` to every in-scope GPT PubAds request made after Trusted Server
   rewrites and emits the document head.
2. Leave production/control pages unchanged.
3. Preserve the existing `ts_initial=1` slot-ownership and refresh lifecycle.
4. Support GAM reports that count treatment impressions and clicks and derive
   the control counts within the same experiment scope.
5. Avoid changing auction eligibility, ad delivery, consent behavior, or page
   performance.
6. Define the data-quality checks needed when an unmarked request is used as the
   control baseline.
7. Keep attribution disabled by default and independently reversible without
   disabling the GPT integration.

## Non-goals

- Implement or change the cookie-based A/B router. The experiment infrastructure
  owns sticky cohort assignment and routes only the treatment cohort through
  Trusted Server.
- Prove that a Trusted Server server-side bid won the GAM auction. For an
  in-scope document satisfying the non-cloned activation prerequisite, `ts=true`
  means that Trusted Server emitted the document head through its publisher
  pipeline, regardless of whether the winning demand was a server-side bid, a
  direct GAM line item, Ad Exchange, or backfill.
- Mark GAM traffic outside a document-local GPT PubAds service whose head was
  processed by the enabled publisher attribution pipeline. IMA/video SDK
  requests, direct tags, and server-side GAM requests require separate
  instrumentation and are outside this design. Nested inventory is eligible
  only when its own response is independently routed, rewritten, and validated;
  the activation attribute is not proof of those facts because publisher code
  can copy it into `srcdoc` or `document.write` markup.
- Replace `ts_initial`, `hb_*`, line-item, bidder, or creative reporting.
- Add a client-side analytics beacon or a Trusted Server telemetry event.
- Make GAM click tracking more complete. The marker only segments clicks that
  GAM already records.
- Provide billing-grade or causal experiment analysis from GAM alone.

## Assumptions and prerequisites

- The GPT integration and its separate `gam_attribution_enabled` setting are
  enabled on every Trusted Server deployment receiving treatment traffic. The
  attribution setting defaults to `false`; enabling GPT alone does not emit the
  marker or its activation signals.
- The response enters Trusted Server HTML rewriting, contains a literal `<head>`
  element, and Trusted Server rewrites and emits that head before any publisher
  script issues a GAM request. Pass-through or buffered-unmodified responses and
  origin markup that omits `<head>` cannot satisfy the marker guarantee.
- On the existing no-post-processor Fastly streaming path, the rewritten head
  can reach the browser before origin EOF or rewriter finalization. `ts=true`
  therefore certifies successful head rewrite and emission, not successful
  completion of the remaining body. A later origin, decode, or rewrite failure
  can truncate an already marked page; the request remains treatment traffic and
  the delivery failure is monitored separately. This design adds no response
  buffering; configurations with HTML post-processors retain their existing
  buffering behavior.
- The publisher Content Security Policy allows Trusted Server's bare inline
  scripts to execute. Initial `adSlots`, the GPT enable flag, the GPT bootstrap,
  and the `bids`/`adInit` invocation are all nonce-less inline scripts; Trusted
  Server does not currently propagate a publisher nonce or update CSP hashes. A
  policy that blocks those scripts makes the initial TS ad stack inert and is
  ineligible at launch even if it allows the synchronous first-party TSJS
  bundle.
- Each in-scope HTML document uses one document-local GPT PubAds service. A
  marked parent does not mark a nested GPT instance. The publisher-only bundle
  attribute is deliberately non-secret and can be copied, so the implementation
  cannot guarantee that an unrewritten nested document never activates the
  fallback. IMA/video, direct-tag, server-side GAM, and nested inventory not
  independently routed, rewritten, and validated must be excluded from the
  experiment and paired reports. A copied activation attribute is a measurement
  contamination incident, not evidence that the nested document is eligible.
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

The marker contract is fixed: the target GAM network rejected the longer
`trusted_server` request name under its enforced 10-character limit. Trusted
Server therefore emits exactly `ts=true`, with no configurable key or value and
no alias or dual-write path.

Attribution has a separate, disabled-by-default GPT setting:

```toml
[integrations.gpt]
gam_attribution_enabled = false
```

The equivalent environment override is
`TRUSTED_SERVER__INTEGRATIONS__GPT__GAM_ATTRIBUTION_ENABLED`. Trusted Server's
environment overlay only replaces leaves that are present in the TOML source,
so an operator relying on that override must retain
`gam_attribution_enabled = false` under `[integrations.gpt]` in the base file.
When the setting is `false`, enabling GPT preserves current behavior: the dormant
gated bootstrap code may still be present, but no attribution callback is
enqueued or executed and no inline attribution flag, bundle activation
attribute, or fallback is activated. This setting is the attribution kill switch
and does not disable GPT proxying, script rewriting, the GPT shim, `adInit`, or
`ts_initial`.

When attribution is enabled, `head_inserts` adds
`window.__tsjs_gam_attribution_enabled=true` to the integration's existing first
inline head insert; it does not add a third insert. That inline flag authorizes
the raw bootstrap's primary marker path. It is deliberately not the bundle
fallback signal because CSP can block the inline insert that defines it.

The early GPT bootstrap will enqueue page-level targeting before publisher GPT
commands execute only when the inline attribution flag is exactly `true`. The
enqueue must occur after `window.tsjs` is initialized but before the existing
`if (ts.adInit) return;` guard:

```text
(function () {
  if (typeof window === "undefined") return;
  var ts = (window.tsjs = window.tsjs || {});
  var tag;
  if (window.__tsjs_gam_attribution_enabled === true) {
    tag = (window.googletag = window.googletag || { cmd: [] });
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
  }

  if (ts.adInit) return;
  // Existing initial-load detector and adInit stub follow, reusing `tag` when
  // attribution initialized it and preserving their current path otherwise.
})();
```

The exact implementation must follow the repository's JavaScript formatting and
defensive checks. The important contract is that the page-level targeting
command is queued by the head bootstrap before the origin page can queue its GPT
setup or request ads when attribution is enabled. Page attribution is
independent of whether the bootstrap needs to install `ts.adInit`, so the
existing guard may skip only the ad-init stub and detector setup, never an
enabled marker enqueue. An unavailable targeting API may skip only the marker
callback; it must not prevent later queued publisher or Trusted Server callbacks
from running.

The existing initial-load detector immediately below the new marker reuses the
initialized `tag.cmd` reference when attribution created it and follows its
current initialization path otherwise. In particular, attribution disabled plus
a pre-existing `ts.adInit` must still return without creating
`window.googletag`; the new default-off setting cannot change current runtime
behavior.

Moving queue initialization above the `ts.adInit` guard intentionally creates a
standard `window.googletag` command-queue stub on every attribution-enabled page,
including a page where `ts.adInit` already exists and GPT never loads. The stub
is inert by itself and preserves the marker-before-guard guarantee; it is not an
accidental behavior to remove during implementation review.

The TypeScript GPT bundle will defensively enqueue the same page-level targeting
at module initialization after the existing flag-gated shim block and before
`installTsAdInit()`, only when the executing publisher-page bundle tag carries
the non-executable `data-ts-gam-attribution="true"` attribute. The HTML pipeline
adds that attribute only when GPT and `gam_attribution_enabled` are both enabled.
A pre-existing `ts.adInit` is already covered by placing the bootstrap marker
before the guard and is not a reason for the fallback.

The fallback exists to preserve delivery-path attribution if the inline
bootstrap unexpectedly stops executing while the synchronous first-party bundle
still runs before publisher GPT. It does not recover `adSlots`, bids, the
`adInit` invocation, or the initial TS auction: those are also nonce-less inline
scripts. On an eligible publisher document whose activation tag was not copied,
a fallback-only marker therefore still truthfully means "document head emitted
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

For an in-scope document that satisfies the non-cloned activation prerequisite,
the marker means:

> Trusted Server rewrote and emitted the containing document's `<head>` through
> the enabled publisher attribution pipeline before the in-scope GPT request.

It does not mean:

- the complete streamed response reached the browser or finished rewriting;
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

| Key            | Scope      | Lifetime                     | Meaning                                             |
| -------------- | ---------- | ---------------------------- | --------------------------------------------------- |
| `ts=true`      | Page-level | Entire browser page lifetime | Eligible, non-cloned TS pipeline emitted page head  |
| `ts_initial=1` | Slot-level | Initial TS-managed request   | Initial slot request was prepared by Trusted Server |

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
       -> deployment has `gam_attribution_enabled = true`
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
not covered merely because the containing document is marked. Nested inventory
is eligible only when Trusted Server separately routes and rewrites that
document and validation proves the same request and report contract. Because a
publisher can copy the non-secret activation tag into `srcdoc` or
`document.write` content, marker presence alone does not prove that a nested
document was independently rewritten.

A full browser navigation creates a new page and repeats cookie-based routing.
The new page receives the marker only when that navigation is routed through a
Trusted Server deployment with `gam_attribution_enabled = true`, its head is
rewritten and emitted, and a marker callback successfully applies page-level
targeting before its first in-scope GPT request.

## Component changes

### GPT configuration and head activation

`crates/trusted-server-core/src/integrations/gpt.rs` will add
`gam_attribution_enabled: bool` to `GptConfig` with Serde's ordinary `false`
default. Configuration tests must cover an omitted field, explicit `false`,
explicit `true`, and an environment override whose base TOML includes the leaf.
The example configuration will show the field as disabled.

`GptIntegration::head_inserts` keeps its current number and order of scripts.
When attribution is enabled, it appends the inline attribution flag to the
existing GPT enable/shim insert; when disabled, that insert remains byte-for-byte
equivalent to its current behavior apart from formatting that does not affect
execution.

The same parsed `GptConfig` instance must authorize the publisher bundle-tag
attribute without reparsing settings or relying on head-insert side effects.
Add a default-empty `IntegrationHeadInjector::tsjs_script_tag_attributes()`
hook, override it in `GptIntegration` to return only
`data-ts-gam-attribution="true"` when attribution is enabled, and aggregate the
attributes through `IntegrationRegistry`. `html_processor.rs` passes that
registry-owned attribute list to a new publisher-only
`tsjs_script_tag_with_attributes` helper. Keep the existing
`tsjs_script_tag` and `tsjs_unified_script_tag` output unchanged for creative,
test, and other generic callers. This makes the bootstrap and fallback derive
from one integration-owned setting while avoiding a GPT-specific
`HtmlProcessorConfig` field or order-dependent document-state mutation.

### Early GPT bootstrap

`crates/trusted-server-core/src/integrations/gpt_bootstrap.js` owns the
behavior. It will set the page-level key in its earliest GPT command callback,
before the `ts.adInit` early-return guard, only when the inline attribution flag
is exactly `true`. The targeting code stays inside the existing raw bootstrap
script returned by `head_inserts`; it must not add a third head insert.

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
read the experiment cookie or wait for an auction. The existing optional
`GoogleTag.setConfig` method and `GoogleTagConfig extends Record<string,
unknown>` types already cover this call and must be reused rather than extended.

The bootstrap remains the primary path because it is injected first. The bundle
call is a redundant fallback and must not delay module initialization, create a
request, or add slot-level targeting. The attribution helper must not create
`window.googletag` independently when the activation attribute is absent. A
standalone module import with neither the existing GPT-enable flag nor the new
attribute must preserve the current runtime-gating contract and leave
`window.googletag` untouched.

### Non-executable bundle activation

The publisher HTML pipeline in
`crates/trusted-server-core/src/html_processor.rs`, using a separate,
publisher-page-only tag helper in `crates/trusted-server-core/src/tsjs.rs`, will
add a `data-ts-gam-attribution="true"` attribute to the existing synchronous
`#trustedserver-js` bundle tag only when GPT and `gam_attribution_enabled` are
both enabled. The attribute is data, not an inline executable, so CSP can block
the inline GPT head inserts while still allowing the external bundle to detect
that it owns page attribution.

At module initialization, the GPT bundle captures `document.currentScript` and
requires that executing synchronous script to carry
`data-ts-gam-attribution="true"` before the fallback may create a GPT stub. It
must fail closed when the executing script cannot be identified. Do not
authorize activation through a global `#trustedserver-js` lookup: the generic
unified tag uses the same ID in creative and test contexts, and duplicate IDs
could select the wrong element. Binding the signal to the executing tag keeps
the activation decision explicit and testable without relying on an inline
global flag.

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
publisher-page bundle tag gains the activation attribute only when GPT and
`gam_attribution_enabled` are both enabled, remains a single external tag, and
omits the attribute for attribution-disabled, non-GPT, and generic all-modules
bundles. Bundle tests must also prove that an unrelated or duplicate element
with `id="trustedserver-js"` cannot activate the fallback.

### GPT Rust integration tests

`crates/trusted-server-core/src/integrations/gpt.rs` already tests the embedded
bootstrap returned by `head_inserts`. Extend those tests to prove that:

- omitted and explicit-false configuration emit neither the inline attribution
  flag nor publisher-tag attribute metadata;
- explicit-true configuration emits the inline attribution flag and exposes the
  publisher-tag attribute metadata;
- the bootstrap contains the flag-gated page-level `ts=true` targeting;
- the marker enqueue appears before the `if (ts.adInit) return;` guard;
- the targeting setup is queued before `ts.adInit` can issue `display` or
  `refresh`;
- the existing `ts_initial` marker remains present; and
- the attribution-enabled integration without `slim_prebid_url` still emits
  exactly the
  existing two head inserts, proving the marker was added to the bootstrap
  instead of a new tag.

### Bootstrap execution tests

Extend the existing
`crates/trusted-server-js/lib/test/integrations/gpt/gpt_bootstrap.test.ts`
Vitest/jsdom behavioral test for the raw bootstrap. Preserve its established
`process.cwd()` source resolution and `new Function(BOOTSTRAP_SOURCE)` execution
strategy unless a separate test-harness change demonstrates a concrete need to
replace them. The tests continue to execute the checked-in raw source rather
than a copied fixture and add no JavaScript runtime dependency to Rust.

The harness must prove that:

- attribution disabled preserves the existing pre-installed-`ts.adInit` behavior
  without creating `window.googletag`;
- attribution enabled queues the callback before a publisher callback added
  after the injected bootstrap;
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
or throwing `setConfig` does not stop the remaining GPT module installers. The
attribution helper must not create a stub independently when its attribute is
absent: a standalone import with neither the existing GPT-enable flag nor the
new attribute leaves `window.googletag` untouched. An attribution-disabled but
GPT-enabled deployment preserves today's flag-gated shim and stub behavior; it
only omits the attribution callback and fallback. A duplicate call after the
bootstrap must remain safe and must not create another script or network
request.

### Slot cleanup constraints

No refresh-lifecycle change is required. In particular:

- do not add `ts` to `TS_REFRESH_TARGETING_KEYS`;
- do not add `ts` to `TS_BASE_TARGETING_KEYS`;
- do not rename or remove `TS_INITIAL_TARGETING_KEY`; and
- do not copy `ts` onto individual slots.

Leaving these components unchanged is part of the design: slot cleanup cannot
remove a page-level key set through `googletag.setConfig`.

No new `CreativeOpportunitySlot` parsing, filtering, or startup validation is
added. Trusted Server continues accepting and forwarding operator targeting
maps verbatim. The external launch audit—not runtime code—must reject an
effective configuration containing slot-level `ts`.

### Documentation

Document the distinction between page-level `ts=true` and slot-level
`ts_initial=1` in `docs/guide/integrations/gpt.md`, near the existing command
queue documentation. Document `gam_attribution_enabled`, its default-off and
kill-switch behavior, the fixed marker contract, and the GAM setup and reporting
preconditions below. Add the disabled field to `trusted-server.example.toml`; do
not add this current integration to a planned-future GAM document.

## GAM configuration

GAM configuration is a deployment prerequisite and must be completed before the
experiment starts because key-value reporting is not retroactive.

The request contract is finalized as key name `ts`, predefined value `true`.
The target-network preflight rejected `trusted_server` (14 characters) under
the enforced 10-character request-name limit, consistent with the SOAP/REST
`CustomTargetingKey` contract. Because `ts` is short, it carries a real collision
risk with common publisher timestamp or cache-buster keys, which makes the
cross-system collision audit a hard launch gate, not a formality. If any
publisher, Trusted Server configuration, Prebid targeting source, or GAM object
already uses `ts`, the experiment must stop until the collision is removed.
Changing the finalized contract or silently filtering established targeting is
out of scope.

1. In **Inventory > Key-values**, create or verify the `ts` key and retain the
   target-network preflight result with the experiment runbook. The SOAP/REST
   [`CustomTargetingKey`](https://developers.google.com/ad-manager/api/reference/v202605/CustomTargetingService.CustomTargetingKey)
   documents the enforced 10-character request-name limit and a 40-character
   value limit.
2. Use a predefined value named `true`.
3. Preflight whether the target network has Enhanced key-value reporting and
   confirm the publisher has approved its Premium reporting activation and
   displayed CPM terms, minimum/maximum charge, and non-prorated monthly billing.
   Record the result. If approved and compatible with the selected metrics,
   enable `ts` as a dedicated Enhanced dimension. Otherwise, enable `ts` for
   legacy reporting and prove with a target-network dry run that the exact
   legacy `ts=true` filter and chosen metrics are available; never sum unfiltered
   legacy **Key-values** rows. See
   [Access Premium reporting](https://support.google.com/admanager/answer/16176700)
   and [Report on targeting keys](https://support.google.com/admanager/answer/14528835).
4. Reserve `ts` for Trusted Server page attribution.
5. Audit existing publisher GPT code and every GAM object that consumes custom
   targeting for an existing `ts` key before deployment. This includes line
   items, proposal line items, rules, protections, yield configuration, and any
   network-specific custom-targeting surface.
6. Audit Prebid-generated GPT targeting, including `pbjs.bidderSettings`, each
   bidder's and standard key's `adserverTargeting`, and every call to
   `setTargetingForGPTAsync`. Record every path capable of producing a slot-level
   `ts`; any occurrence is a launch blocker because GPT slot targeting overrides
   the page-level value.
7. Audit every `CreativeOpportunitySlot.targeting` map from all effective
   `trusted-server.toml` configuration sources. The arbitrary operator-supplied
   map is copied to GPT slots, where a slot-level `ts` value would override the
   page-level marker. Any occurrence is a launch blocker; do not silently
   discard it because that could change established operator targeting.
8. Audit publisher code for every operation that can remove or supersede the
   marker after initial GPT setup. Search for `setConfig({ targeting: null })`,
   a `ts: null` or different `ts` value, `pubads().clearTargeting()` with no key
   or with `ts`, and slot-level `ts` targeting. Account for equivalent calls
   assembled dynamically.

The runbook records the audit owner, exact queries or search procedures, result
artifact, timestamp, and re-audit trigger. A deployment, publisher GPT, Prebid,
or GAM targeting change affecting the audited surfaces invalidates the prior
result and blocks attribution until the audit is repeated.

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

Before launch, the experiment owner freezes an authoritative scope manifest
containing those values, the eligible route/site inventory, exact ad-unit list,
owner, version, and activation timestamp. Report A, Report B, router/access-log
queries, synthetic URLs, and any external denominators must all reference that
same manifest. A scope change closes the current reporting window and requires a
new manifest version and fresh preflight.

Do not compare the TS cohort with all unmarked network traffic unless all that
traffic is eligible for the same experiment. Likewise, exclude smoke tests,
direct hits, operations traffic, and any other TS-served page outside the cookie
experiment. The marker identifies the delivery path, not the router's cohort
assignment, so all such requests also carry `ts=true` when attribution is
enabled for their Trusted Server deployment.

The route owner must use router or access logs to prove that non-experiment TS
traffic is absent from the eligible inventory during the measurement window. If
such traffic cannot be prevented and has no independent inventory or reportable
dimension, GAM cannot remove it from Report B because its marker is identical to
the cohort marker; the experiment must not launch. Record the owner, query,
expected zero threshold, and response procedure in the experiment runbook.

Before treatment routing begins, run and retain a zero-count query for every
excluded path: non-experiment Trusted Server traffic, smoke/direct/operations
traffic, IMA/video, direct tags, server-side GAM, and excluded nested inventory.
Any non-zero result blocks launch unless an independent dimension excludes the
same traffic from both saved reports.

### Cohort calculations

Create and retain two saved reports with identical date boundaries, time zone,
inventory filters, traffic-quality filters, and metric definitions:

1. **Report A — experiment total.** Do not include **Placement**, legacy
   **Key-values**, **Targeting**, **Yield group**, or another dimension that can
   represent one event more than once. This report provides one non-duplicated
   total for every metric in the eligible experiment scope.
2. **Report B — TS treatment.** Use the dedicated Enhanced `ts` dimension
   filtered to `ts=true`. If Enhanced key-value dimensions are unavailable,
   unapproved, or incompatible with the saved metric pair, use the legacy
   **Key-values** dimension filtered to exactly `ts=true` and do not sum any
   other key-value rows. Do not add **Placement**, **Targeting**, **Yield
   group**, or any unrelated dimension that can represent the filtered treatment
   event more than once.

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

For every paired metric and reporting window, validate:

```text
0 <= Report B <= Report A
derived_control = Report A - Report B
```

A negative derived control, Report B greater than Report A, mismatched report
definition, incompatible metric, missing saved definition, or non-zero excluded
traffic is a fail-closed reporting incident. Do not publish or interpret that
window; correct the inputs and rerun the complete pair after the same reporting
latency and invalid-traffic adjustment window.

Use total metrics when the goal includes all GAM demand sources. GAM's
`Ad server impressions` and `Ad server clicks` metrics exclude Ad Exchange and
AdSense, so those narrower metrics should only be used when that exclusion is
intentional. GAM counts impressions and clicks according to its own tracking
rules; adding `ts=true` does not create new impression or click trackers. See
[Counting impressions and clicks](https://support.google.com/admanager/answer/2521337).

Both reports must use the same metric names, and the target-network dry run must
prove that each metric is compatible with the chosen Enhanced or legacy
dimension and filters. Prefer non-targeted impression and click metrics because
`ts` is forbidden from line-item targeting; targeted metrics limited to keys
used for targeting do not represent this delivery-path cohort. If GAM cannot
produce an identical compatible metric in both reports, omit that metric rather
than substitute different definitions. Record the exact selected metric names
and successful dry-run exports before launch.

### Descriptive rates for unequal cohort sizes

The treatment cohort is intentionally small, so raw TS and production totals are
not directly comparable. GAM may show raw counts and descriptive normalized
rates where compatible metrics are available:

- impressions per GAM ad request;
- fill rate;
- clicks per impression (CTR); and
- revenue per thousand impressions or requests.

These rates describe GAM delivery; they do not estimate a causal treatment
effect. Impressions per routed pageview or per assigned visitor require a
denominator from the A/B router or site analytics because GAM cannot identify
unmarked production pageviews that made no ad request. Any causal analysis is
outside this implementation and requires a separately approved design defining
the eligible population, router/site denominators, analysis window, and stopping
rules.

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
comparison is a diagnostic rather than direct proof of marker coverage. A
request-correlated router or site denominator can strengthen the aggregate
coverage estimate, but without new request-correlated telemetry it still cannot
prove marker presence on every production request.

Neither aggregate share nor a scheduled synthetic sample proves that every
production response or ad request carried the expected marker. Automated tests
establish code-path invariants; production checks provide sampled operational
evidence. The runbook must not describe either diagnostic as per-response
coverage telemetry.

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
were blocked, remains correctly marked as having a TS-emitted head but raises a
measurement incident. Since GAM cannot separate those requests afterward, the
incident owner must pause interpretation and exclude the affected time range
from both reports when clean boundaries can be established; otherwise the
experiment result is invalid.

## Failure handling

The marker is best-effort instrumentation and must never block ads or page
delivery.

- If GPT never loads, there is no GAM request to classify.
- If attribution is disabled, the dormant gated bootstrap source may remain, but
  no attribution callback is enqueued or executed and no activation flag or
  attribute is emitted; GPT otherwise keeps its current behavior.
- If Trusted Server does not rewrite and emit a literal `<head>`, or an in-scope
  GPT request occurs before the injected head content runs, neither targeting
  path can mark that request. Such traffic is ineligible for the experiment and
  must be detected before launch or excluded from analysis.
- If an origin, decoder, or rewriter failure occurs after Fastly emitted the
  rewritten head, the browser may already have queued `ts=true`. Any resulting
  request remains treatment because the marker represents head delivery, not
  full-response completion. The client may receive a truncated document; the
  delivery failure is logged and investigated separately rather than being
  reclassified as control.
- If CSP blocks Trusted Server's nonce-less inline scripts, the initial TS ad
  stack is inert and the page is ineligible even when the first-party bundle
  queues the attribution marker. The fallback prevents a page whose head was
  emitted through Trusted Server from leaking into the inferred control cohort;
  it does not make the deployment
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
- If Prebid `bidderSettings`, bidder or standard `adserverTargeting`, or
  `setTargetingForGPTAsync` applies slot-level `ts`, the slot value can supersede
  the page marker. The launch audit and characterization tests cover these paths;
  Trusted Server does not filter or intercept them.
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
data. Its intended meaning is the head-delivery path of an eligible document;
outside that scope, copied activation is contamination rather than proof of
delivery. Because only the cookie-sticky treatment cohort is routed through
Trusted Server for this experiment, the value also reveals treatment-path
membership for that GAM request. It is therefore cohort information even though
it does not expose the assignment cookie or identify a person by itself.

The implementation does not read the experiment cookie. Routing happens before
Trusted Server handles the request. Existing consent gates continue to decide
whether GAM requests or auctions occur. The marker does not create an ad request
that would otherwise be suppressed. Before enabling
`gam_attribution_enabled`, the experiment owner must complete the publisher's
privacy/data-governance review for sending this treatment-path attribute to GAM
and confirm that existing consent and data-use terms cover it.

## Testing strategy

### Automated tests

1. Extend GPT configuration and head-insert tests for omitted, false, and true
   `gam_attribution_enabled` values. Assert that the true case authorizes
   page-level `ts=true` before the `ts.adInit` guard and any bootstrap `display()`
   or `refresh()` call without changing the expected head-insert count; false
   must preserve current output and behavior.
2. Extend the TSJS tag and HTML processor tests to prove the non-executable GPT
   activation attribute appears only when GPT and attribution are both enabled
   on the publisher-page bundle and adds no script tag.
3. Extend the existing Vitest/jsdom raw-bootstrap harness described above.
   Exercise the
   bootstrap with `googletag.setConfig` available, unavailable, and throwing,
   and prove a later publisher callback still runs in every enabled case. Prove
   the disabled case preserves existing behavior, then set `window.tsjs.adInit`
   before evaluation and prove only the enabled marker still runs.
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
8. Add Prebid characterization tests covering `bidderSettings`, custom
   `adserverTargeting`, and `setTargetingForGPTAsync` so a generated slot-level
   `ts` collision is visible and remains a launch-audit responsibility rather
   than being silently filtered.
9. Extend no-post-processor streaming regression coverage to prove an
   attribution-enabled Fastly response can emit the marked rewritten head before
   origin EOF. Preserve the existing later-error/truncation behavior,
   documenting that the marker does not certify complete response delivery and
   adds no buffering.
10. Add focused Vitest/jsdom bundle-DOM coverage for the synchronous publisher
    tag, proving `document.currentScript` is the attributed element and
    false/non-publisher/duplicate-tag cases fail closed. Characterize a publisher
    copying the attributed tag through `srcdoc` or `document.write`: the clone can
    activate the fallback, so tests must not encode the false guarantee that only
    independently rewritten nested documents can be marked. Manual browser/GAM
    validation below covers the real deployed bundle without expanding the
    repository's single-config Playwright harness.
11. Run the project-required target-matched Rust and JavaScript checks for the
    touched files.

### Browser/GAM validation

Before experiment launch, with `gam_attribution_enabled = true` on the treatment
deployment:

1. Load a treatment page using a known treatment cookie.
2. Confirm the initial in-scope GAM request contains `ts=true` using GPT
   Publisher Console, Delivery Inspector, or the browser network panel.
3. Trigger a lazy slot and a refresh; confirm both requests still contain
   `ts=true`.
4. Load the equivalent production page with a control cookie and confirm the key
   is absent.
5. Disable `gam_attribution_enabled` on a Trusted Server validation deployment
   and confirm GPT still functions while the inline flag, activation attribute,
   and `ts` request key are absent.
6. Confirm `ts_initial=1` remains limited to its existing initial-slot
   lifecycle.
7. Validate the deployed CSP by proving `adSlots`, the GPT bootstrap, `bids`,
   and the initial `adInit` handoff execute on a representative page with
   matched slots. A page that runs only the external bundle is ineligible even
   if the fallback marker appears.
8. Set an unrelated page-level targeting key after `ts=true` and confirm both
   keys remain on a later request. Treat an explicit page-level or per-key clear
   as a failed publisher-code audit, not supported behavior.
9. Validate that IMA/video, direct-tag, server-side GAM, and nested GPT
   inventory without an independently TS-rewritten document is absent from the
   experiment and paired report scope. Directly validate any independently
   rewritten nested documents that are intentionally included.
10. Run a short GAM report and verify treatment totals appear under `ts=true`
    while overall totals remain unchanged apart from normal reporting latency.

## Rollout

1. Create the fixed reportable `ts=true` GAM key/value and record the intended
   Enhanced or legacy reporting path, compatibility requirements, and billing
   decision.
2. Freeze the eligible-scope manifest and draft the saved Report A/Report B
   definitions and excluded-path queries from that manifest.
3. Audit response eligibility, including head rewrite/emission, ordering, CSP,
   and publisher GPT calls that could precede or remove the marker.
4. Audit `ts` across publisher GPT code, Prebid-generated targeting, effective
   `trusted-server.toml` creative-opportunity targeting maps, and every GAM
   custom-targeting consumer. Retain the owner, evidence, timestamp, and re-audit
   trigger.
5. Exclude IMA/video, direct-tag, server-side GAM, and nested GPT inventory not
   independently routed, rewritten, and validated. Treat copied activation in an
   excluded nested document as contamination.
6. Deploy `gam_attribution_enabled = true` while treatment routing remains
   stopped.
7. Provision the scheduled synthetic crawl, assign an incident owner, and obtain
   one successful treatment/control run covering initial, lazy, refreshed, CSP,
   fallback, and disabled-setting checks.
8. On the validation deployment, validate treatment and control requests
   manually, retain zero-count results for every excluded path, and save a short
   paired-report dry run that proves the selected dimensions, filters, metrics,
   and `0 <= Report B <= Report A` invariants.
9. Start the small cookie-sticky treatment cohort only after every configuration,
   collision, privacy, CSP, synthetic, exclusion, and reporting gate passes.
10. Compare observed GAM treatment share with router allocation only as a
    diagnostic before interpreting descriptive delivery results.

Rollback is ordered so newly routed treatment traffic cannot become unmarked
control. First stop new treatment assignment/routing and verify through router
or access logs that routing stopped, then record the last clean reporting
boundary. Keep `gam_attribution_enabled = true` while already-open
documents—including long-lived SPA sessions and any marked document restored
from a cache—drain; they retain page-level targeting and may continue issuing
marked lazy or refreshed requests. Exclude the entire post-boundary drain
interval from both cohorts. The drain ends only after router/access logs and GAM
show no remaining `ts=true` traffic for one complete, runbook-defined reporting
interval. Then set `gam_attribution_enabled = false`, deploy the kill switch,
and use a fresh synthetic Trusted Server navigation to confirm normal GPT
behavior while the marker is absent. If marked traffic persists, keep
attribution enabled and the interval excluded rather than inferring it as
control. Historical GAM rows remain valid, and the GAM key may stay defined and
reportable for historical analysis.

If an active privacy, targeting-collision, or ad-delivery incident requires an
immediate kill, deploy `gam_attribution_enabled = false` without waiting for
router verification. Treat the affected window and subsequent drain interval as
invalid for both cohorts, then stop and verify treatment routing and follow the
same drain-completion rule. The emergency path prioritizes serving safety over a
clean experiment boundary; it must never reinterpret newly unmarked treatment
traffic as control.

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
fallback, a treatment page with a TS-emitted head would be silently inferred as
control.
The fallback does not rescue the simultaneously blocked TS ad-stack scripts, so
that state is an incident rather than an eligible deployment mode.

### Enable attribution whenever GPT is enabled

Rejected because an upgrade would begin disclosing cohort/path information and
reserving the short `ts` key on every existing GPT deployment before its
collision, privacy, and reporting prerequisites were complete. The independent,
default-off setting permits deliberate rollout and rollback without removing the
GPT proxy, shim, or auction behavior.

### Buffer the existing Fastly streaming HTML path until the complete rewrite succeeds

Rejected because the marker must run from the rewritten head before publisher
GPT requests, while the no-post-processor Fastly path deliberately streams that
head before origin EOF. Adding full-response buffering to that path would change
latency, memory use, and first contentful paint. On that in-scope path, the
marker therefore certifies successful head rewrite and emission; later stream
completion is a separate delivery concern. Existing configurations that
register an HTML post-processor retain their current buffering behavior; this
feature adds none.

### Use a longer descriptive key name such as `trusted_server`

A descriptive name would lower the collision risk that the short `ts` key
carries. Rejected because the target-network provisioning preflight enforced a
10-character request-name limit and rejected `trusted_server` (14), even though
other GAM help surfaces describe a 20-character limit. The short `ts` name is
therefore mandatory for this deployment, and the cross-system collision audit
is the compensating control. Do not dual-write `ts` alongside any longer alias:
two names for one cohort would increase GAM setup and audit surface and permit
silent drift between reports. Only `ts=true` is valid.

### Configure `ts=true` in creative-opportunity slot targeting

Rejected because creative-opportunity targeting applies only to matched slots.
The experiment requirement covers every request after the enabled publisher
pipeline emits the document head for its local GPT PubAds service.

### Rewrite `cust_params` on GAM network requests

Rejected because it depends on GPT's internal request construction and encoding,
adds interception risk, and duplicates a supported GPT targeting API.

### Mark production explicitly with `ts=false`

Preferred in a fully controlled experiment, but unavailable because the
production path cannot be changed. The design documents the resulting unmarked
baseline limitation and requires coverage checks.

## Acceptance criteria

1. `gam_attribution_enabled` defaults to `false`. With omitted or false
   configuration, dormant gated bootstrap source may remain present, but no
   attribution callback is enqueued or executed and no inline attribution flag,
   bundle activation attribute, or fallback is activated. Current GPT, shim,
   `adInit`, and `ts_initial` behavior is preserved, and setting the field to
   `false` independently disables attribution.
2. For an enabled deployment that satisfies the documented head-rewrite,
   ordering, CSP, reserved-key, request-scope, and targeting-cleanup
   prerequisites—and in which at least one callback successfully applies
   page-level targeting before the first request, with no later clear or
   override—every request from that document's local GPT PubAds service carries
   `ts=true`, including initial, lazy, refreshed, publisher-owned, and SPA-route
   requests.
3. For an in-scope document satisfying the non-cloned activation prerequisite,
   the marker means the Trusted Server publisher pipeline rewrote and emitted
   that document's head before the request. It does not certify complete streamed
   response delivery. This feature adds no buffering; on the existing streaming
   Fastly path, a later truncation does not reclassify an already marked request
   as control.
4. Production/control pages remain unmodified and do not carry `ts` from this
   feature.
5. `ts_initial=1` retains its current slot-level initial-request lifecycle, and
   `ts` is not cleared by Prebid refresh or SPA slot cleanup.
6. No publisher code, Prebid-generated targeting, effective Trusted Server
   creative-opportunity targeting map, or GAM custom-targeting consumer changes
   ad eligibility, pricing, protection, routing, or the marker value because of
   the measurement key.
7. The marker adds no unique identifier, cookie value, network request,
   response buffering, or blocking work. Its disclosure of treatment-path
   membership to GAM has passed the publisher's privacy/data-governance review.
8. The target-network preflight has fixed the only contract as `ts=true`; no
   other value, alternative key name, alias, configurable marker, or dual-write
   path exists.
9. The exact Enhanced or legacy reporting path, billing approval, dimensions,
   filters, and compatible metrics pass a target-network dry run. Saved Report A
   and Report B use the same frozen eligible-scope manifest and satisfy
   `0 <= Report B <= Report A` for every interpreted metric; violations invalidate
   the pair rather than being clamped or reported.
10. GAM output is described as delivery attribution, not a causal treatment
    effect. Known treatment/control requests are sampled directly, while
    aggregate marker share is compared diagnostically with router allocation;
    neither check is represented as proof of per-response production coverage.
11. Every excluded traffic path has a retained zero-count preflight or an
    independent filter applied identically to both saved reports.
12. Nested inventory is included only when independently routed, rewritten, and
    validated. Because the activation tag is clonable, marker presence alone is
    not proof of eligibility; copied activation in excluded inventory is a
    contamination incident.
13. CSP-compatible inline execution is proven before launch. A fallback-only
    page remains attributed to treatment but raises an incident and cannot be
    treated as a healthy experiment page.
14. Normal rollback stops and verifies new treatment routing, records the clean
    boundary, and keeps attribution enabled until already-open or cached marked
    documents drain according to the documented rule. Only then is the kill
    switch deployed. An emergency kill invalidates the affected and drain
    windows instead of treating newly unmarked traffic as control.
