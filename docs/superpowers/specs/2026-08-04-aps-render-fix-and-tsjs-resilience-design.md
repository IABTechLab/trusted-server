# APS Render Fix and TSJS Resilience Architecture — Design

- **Status:** revision 27 — hard-cutover contract with complete `rc/july` TSJS
  adoption
- **Date:** 2026-08-04
- **Baseline:** `origin/rc/july` @ `905984e62` ("Prevent APS renderer document
  clipping"), including `248fe9558` (PUC/MessageChannel and collapsed-shell
  rendering) and `ed38f3e13` (PUC overflow prevention). File-and-line references
  and commit hashes describe that baseline and are evidence, not permanent API
  contracts.
- **Compatibility:** this is a coordinated hard cutover. No backward-compatible
  aliases, dual APIs, or N/N-1 browser/server protocol are required.
- **Decision:** this document covers APS render correctness, the TSJS architecture
  needed to make that correctness durable, and preservation or explicit
  architectural replacement of every TSJS concept present on the baseline. It
  does not add an external telemetry system or release experimentation.

## 0. Scope and constraints

### 0.1 Goals

1. An accepted APS bid renders through every supported Trusted Server path:
   SSAT/GPT, the Trusted Server Prebid adapter through GAM Universal Creative,
   SPA page bids, and direct `/auction` rendering.
2. Render ownership, identity, and completion are explicit. Races, stale SPA
   work, ambiguous GPT events, duplicate creative requests, and timeouts settle
   deterministically instead of failing silently.
3. TSJS has one runtime kernel, bounded state, explicit lifetimes, and enforced
   dependency boundaries. Integration bundles cannot accidentally create
   independent copies of shared state.
4. The Rust auction result, publisher projection, TypeScript parser, Prebid
   registration, GPT targeting, Universal Creative bridge, and direct renderer
   agree on one APS descriptor and identity contract.
5. Security boundaries are testable: untrusted creative messages cannot claim a
   different slot or attempt, replay a consumed capability, or revive work from a
   prior navigation.
6. Existing non-APS rendering behavior remains correct unless this design
   explicitly replaces a shared lifecycle surface.
7. Every TSJS behavior on the exact `rc/july` baseline is either preserved,
   rebuilt behind the new architecture, or explicitly superseded by a named and
   tested replacement contract. No TSJS behavior may disappear silently merely
   because its old global, wrapper, bootstrap, or carrier is deleted.

### 0.2 Non-goals

- No change to analytics/telemetry schemas, durable data systems, billing,
  experimentation, or deployment routing. Those belong to separate designs.
- No change to the APS upstream OpenRTB endpoint contract, including APS's
  deliberate absence of `nurl` and `burl`.
- No rewrite of Prebid.js itself or of the decoupled Prebid strategy.
- No refactor of unrelated integration internals. They receive only the thin
  registration/bootstrap changes required by the new TSJS runtime, plus any
  mechanical disposal or adapter injection needed to preserve their baseline
  behavior.

Existing local render tracing, GPT diagnostics, logging, counters, debug output,
and telemetry integrations remain functional through the cutover. They may move
behind the runtime event bus or the final diagnostics namespace, but their
observable concepts and non-interference guarantees are in scope. Correctness must
not depend on a new event reaching an external sink. Any new analytics contract
requires a separate design.

### 0.3 Architectural rules

- Make invalid states unrepresentable where practical and reject them at the
  boundary otherwise.
- Every asynchronous operation belongs to a runtime, navigation, auction batch,
  or render-attempt lifetime and has a deterministic disposer.
- Every render attempt reaches exactly one terminal result in memory.
- A timeout cancels or fails only the object it owns; shared work is aborted only
  when no live child still needs it.
- Ad-tech globals are accessed only through adapters.
- Cross-window messages are versioned, exact-shaped, capability-bound, and
  source/port checked.
- No correctness path waits for logging, telemetry, notification delivery, or any
  other side effect unrelated to rendering.

### 0.4 `rc/july` TSJS concept-adoption contract

The exact baseline is the whole observable TSJS system at
`origin/rc/july@905984e62`, not only the three commits after this design branch's
merge base. It includes `crates/trusted-server-js/lib/src/**`, its build and test
tooling, the TSJS behavior embedded in `gpt_bootstrap.js` and
`gpt_diagnostics_bootstrap.js`, and the browser tests that exercise those sources.
The in-spec executable manifest in §0.5 records that tree and commit. If the local
`origin/rc/july` ref moves, implementation stops before code
changes, diffs the old and new tips across those paths, and updates this ledger and
its tests deliberately. A moving branch is never absorbed implicitly.

Each ledger entry has one of these dispositions:

- **Preserve:** retain the observable behavior and its failure semantics.
- **Rebuild:** retain the outcome but replace the old mechanism with the runtime,
  adapter, service, or integration module named here.
- **Supersede:** deliberately replace an old mechanism with a stricter named
  contract. The ledger must state the behavioral change and prove either that the
  new owner makes the old compensation unnecessary or that the new terminal
  failure is complete, bounded, and preferable to a partial second runtime.
- **Exclude:** keep the existing feature untouched because it is not TSJS work;
  this disposition cannot be used for a file under the TSJS baseline inventory.

Hard cutover authorizes removal of old mechanisms and names. It does not authorize
silent loss of a ledger outcome. An observable outcome may change only through an
explicit **Supersede** entry that names the old and final behavior, gives the
architectural reason, and has boundary tests for the replacement contract. A source
deletion is complete only when its ledger entry has a passing replacement contract
or that explicit supersession proof.

| ID                | Baseline TSJS concept                                                                                                                                                                                         | Disposition and final owner                                                                                                                                                                                                                                                      |
| ----------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `RCJ-CORE-01`     | Core config/context, callback queue, auction parsing, direct request/rendering, SPA generation checks, and shared helpers continue to serve every enabled integration.                                        | **Rebuild:** kernel, services, and composition root; exact behavioral corpus runs before and after the switch.                                                                                                                                                                   |
| `RCJ-CORE-02`     | Programmatic ad-unit registration can drive direct `/auction`; core also exposes version/queue, placeholder render helpers, mutable generic config, and the local logger.                                     | **Preserve/supersede:** §5.4 defines the exact final API; typed registration/request APIs and immutable config replace placeholders/mutable config; logger methods/default remain, while invalid levels now throw without mutation instead of being retained with warn fallback. |
| `RCJ-BOOT-01`     | The edge-injected `gpt_bootstrap.js` duplicates initial-load tracking, slot handoff, hydration scheduling, GPT definition/targeting/display/refresh, and can render initial ads without the main TSJS bundle. | **Rebuild/supersede:** the committed runtime owns all normal GPT behavior. Missing/partial bundles intentionally settle through the terminal non-rendering fallback in §5.3; no bootstrap may construct a degraded GPT runtime.                                                  |
| `RCJ-TRACE-01`    | Render tracing records one honest impression timeline, bounded history, current-slot state, DOM stamps/badges, local overlay, no stale auction attribution, and emits `tsjs:adRendered`.                      | **Rebuild/supersede:** lifecycle diagnostics subscriber and `tsjs.diagnostics.renderTrace`; its subscription replaces the mutable globals and CustomEvent, and no integration writes trace state directly.                                                                       |
| `RCJ-GPT-01`      | A TS fallback and a later publisher `defineSlot` share one physical GPT slot and one initial request; ownership transfer prevents later TS destruction.                                                       | **Rebuild:** GPT adapter plus slot service handoff record; no integration-owned function sentinels or duplicate wrappers.                                                                                                                                                        |
| `RCJ-GPT-02`      | Responsive/hydrated slot resolution chooses the unique active placement, recovers DOM replacement, and never silently chooses an ambiguous sibling.                                                           | **Rebuild:** navigation-scoped aliases plus runtime-owned DOM binding/reconciliation.                                                                                                                                                                                            |
| `RCJ-GPT-03`      | Native publisher GPT calls, service state, SRA, disabled initial load, refresh options, targeting cleanup, and publisher-owned slots retain their native semantics.                                           | **Preserve/Rebuild:** the sole GPT adapter owns interception and event fan-out; publisher activity never becomes TS-owned work.                                                                                                                                                  |
| `RCJ-GPT-04`      | A TS-owned PUC response may resize only its authenticated still-collapsed ordinary 1×1 GAM shell, never unrelated, anchor, fixed, sticky, or already-expanded frames.                                         | **Preserve/Rebuild:** current render attempt owns one guarded resize after a response is successfully posted.                                                                                                                                                                    |
| `RCJ-PREBID-01`   | The publisher-specific artifact is pure Prebid.js; the Trusted Server shim is a separate TSJS integration module, and the external bundle remains independently useful if that module fails.                  | **Preserve/Rebuild:** external artifact plus Prebid adapter/integration module; TS code is not vendored into the external Prebid artifact.                                                                                                                                       |
| `RCJ-PREBID-02`   | Missing, late, duplicate, older, or partial Prebid artifacts fail safely: publisher queues drain, TS refresh handling is not installed without a real API, and installation is idempotent.                    | **Preserve/Rebuild:** artifact watchdog plus release-matched module transaction and bounded readiness queue.                                                                                                                                                                     |
| `RCJ-PREBID-03`   | Adapter manifests distinguish module names from registered bidder codes/aliases; client-side bidder coverage, user-ID modules, EIDs, native bids, and publisher callbacks keep working.                       | **Preserve:** typed artifact contract and black-box artifact tests; TS-owned bid identities alone are replaced.                                                                                                                                                                  |
| `RCJ-PREBID-04`   | Configured GAM-path exclusions remove only matching slots from the synthetic Prebid refresh auction while clearing stale TS keys and retaining every slot/options in the GPT refresh.                         | **Preserve/Rebuild:** one refresh policy in the Prebid integration module over the GPT adapter; global, explicit, mixed, all-excluded, and fail-open path cases remain exact.                                                                                                    |
| `RCJ-APS-01`      | First-class APS OpenRTB admission, typed descriptor projection, direct rendering, Trusted Server Prebid-adapter rendering, and PUC rendering remain supported.                                                | **Preserve/Rebuild:** Rust admission plus the shared render lifecycle described in §§3–4.                                                                                                                                                                                        |
| `RCJ-APS-02`      | `bid.meta`, generated Prebid `adId`, upstream bid-id fallback, and old `hb_adid` precedence carried APS identity through lossy boundaries.                                                                    | **Supersede:** the server-minted `r1_` reservation is the only TS PUC authority; native Prebid IDs and PBS Cache UUIDs remain byte-preserved for their own purposes.                                                                                                             |
| `RCJ-APS-03`      | PUC uses one-use ports, APS callbacks—not script load—determine success, renderer tombstones are bounded, and lifecycle callbacks cannot corrupt later attempts.                                              | **Preserve/Rebuild:** bridge dispatcher, owner-control channel, reservation service, and terminal latch.                                                                                                                                                                         |
| `RCJ-APS-04`      | The PUC document, renderer document, and descendant creative receive the winning dimensions without default margins, scrollbars, overflow, or clipping.                                                       | **Preserve:** exact CSS/DOM sizing contract in §4.4 and three-level browser assertions.                                                                                                                                                                                          |
| `RCJ-CREATIVE-01` | Auction creative sanitization remains opt-in/default-off, rewriting retains its existing independent setting, and every delivery path observes the same configured processing boundary.                       | **Preserve:** creative integration module and server processing; this design does not silently enable sanitization or broaden rewriting.                                                                                                                                         |
| `RCJ-CREATIVE-02` | Opaque-origin click recovery accepts only validated absolute HTTP(S) navigation, persists the validated URL, rejects non-network schemes, and keeps creative sandbox isolation.                               | **Preserve/Rebuild:** creative integration module over shared origin/DOM helpers, with unit and real-browser sandbox coverage.                                                                                                                                                   |
| `RCJ-CREATIVE-03` | `tscreative.installGuards/setConfig/getConfig`, `tsCreativeConfig`, automatic install, click-guard default-on, and render-guard default-off control the creative browser guards.                              | **Preserve/supersede:** `CreativeBootV1` retains the defaults and the integration module auto-installs transactionally; mutable/install command globals are deleted and immutable `tsjs.boot.creative` is the only inspection/config surface.                                    |
| `RCJ-DIAG-01`     | GPT runtime diagnostics reports raw GPT observations, exact slot binding/replacement, request cycles/timing, bounded export, overlay/badges, and no lifecycle interference.                                   | **Preserve/Rebuild:** diagnostics integration module consumes the GPT adapter event stream and exposes `tsjs.diagnostics.gpt`; it never installs a second GPT control wrapper.                                                                                                   |
| `RCJ-INT-01`      | DataDome, Didomi, Google Tag Manager, Lockr, Osano, Permutive, Sourcepoint, and Testlight retain their current proxy guards, configuration, consent/segment, queue, and timing behavior.                      | **Preserve:** thin transactional integration modules plus complete pre/post-cutover black-box suites; internal feature behavior is otherwise unchanged.                                                                                                                          |
| `RCJ-INT-02`      | Shared script, beacon, DOM-insertion, scheduling, origin, and async helpers retain per-integration matching and failure isolation.                                                                            | **Rebuild where shared:** helper factories with integration-owned configuration; one module failure cannot unwind another integration module or publisher code.                                                                                                                  |
| `RCJ-QUAL-01`     | Lint covers production source, tests, scripts, diagnostics, and build code; TypeScript and artifact checks cover the actual shipped combinations.                                                             | **Preserve/strengthen:** full-package lint/typecheck plus architecture, maximal-bundle, generated-artifact, browser, and retained-heap gates.                                                                                                                                    |

The commit clusters that exposed these concepts include the render-trace series
starting at `966c8569c`; GPT recovery/handoff/responsive/native-behavior commits
`4f45974e5`, `9b1985c8b`, `340d1efb4`, `0fdd13e7d`, `ca678fe69`, and
`b200be53c`; Prebid decoupling/resilience and refresh commits `001ad385c`,
`cdff89706`, `f3dc6ba70`, `60a85e661`, and `a007bd0d0`; creative hardening
commits `1929dc83a`, `fde835110`, `9b21ba450`, `20977105f`, `1db074d4b`, and
`3d9e2b693`; GPT diagnostics `11a4a7d25`; full-package lint `941473407`; APS
admission/rendering commits from `f916ddf90` through `a08bebfbd`; and the final
PUC/sizing chain `248fe9558`, `ed38f3e13`, `905984e62`. The executable tree
inventory, not this illustrative hash list, is the completeness authority.

### 0.5 In-spec baseline mapping manifest

To keep this a one-file design, the completeness manifest is embedded here instead
of creating another repository artifact. The implementation's first contract test
extracts the `rcjuly-tsjs-manifest-v1` JSON block, enumerates each directory
`includeRoot` plus every exact mapped file at the pinned commit with `git ls-tree`,
and requires every enumerated file to match at least one `exact`, `prefix`, or
`prefixes` mapping. A path receives the union of every matching row. Every
file under `lib/src` must receive at least one non-`RCJ-QUAL-01` id. A mapping that
matches no pinned path also fails, preventing stale rows. Moving the baseline runs
this check before implementation and requires an explicit manifest/ledger update for
every new, removed, or renamed path.

```json rcjuly-tsjs-manifest-v1
{
  "version": 1,
  "baseline": "905984e62a0858c53d9f0ff6dd3a1bf190cf311d",
  "includeRoots": [
    "crates/trusted-server-js/lib",
    "crates/trusted-server-integration-tests/browser/tests"
  ],
  "mappings": [
    {
      "exact": [
        "crates/trusted-server-js/lib/.gitignore",
        "crates/trusted-server-js/lib/.prettierignore",
        "crates/trusted-server-js/lib/.prettierrc.json",
        "crates/trusted-server-js/lib/eslint.config.js",
        "crates/trusted-server-js/lib/package-lock.json",
        "crates/trusted-server-js/lib/package.json",
        "crates/trusted-server-js/lib/tsconfig.json",
        "crates/trusted-server-js/lib/vite.config.ts",
        "crates/trusted-server-js/lib/vitest.config.ts"
      ],
      "ids": ["RCJ-QUAL-01"]
    },
    {
      "exact": [
        "crates/trusted-server-js/lib/build-all.mjs",
        "crates/trusted-server-js/lib/src/index.ts"
      ],
      "ids": ["RCJ-CORE-01", "RCJ-QUAL-01"]
    },
    {
      "exact": [
        "crates/trusted-server-js/lib/build-prebid-external.mjs",
        "crates/trusted-server-js/lib/test/build-prebid-external.test.mjs",
        "crates/trusted-server-js/lib/test/prebid-artifact-integration.test.mjs"
      ],
      "ids": ["RCJ-PREBID-01", "RCJ-PREBID-02", "RCJ-PREBID-03", "RCJ-QUAL-01"]
    },
    {
      "prefix": "crates/trusted-server-js/lib/src/core/",
      "ids": ["RCJ-CORE-01"]
    },
    {
      "exact": ["crates/trusted-server-js/lib/src/core/log.ts"],
      "ids": ["RCJ-CORE-02"]
    },
    {
      "exact": [
        "crates/trusted-server-js/lib/src/core/config.ts",
        "crates/trusted-server-js/lib/src/core/index.ts",
        "crates/trusted-server-js/lib/src/core/registry.ts",
        "crates/trusted-server-js/lib/src/core/request.ts"
      ],
      "ids": ["RCJ-CORE-02"]
    },
    {
      "exact": ["crates/trusted-server-js/lib/src/core/trace.ts"],
      "ids": ["RCJ-TRACE-01"]
    },
    {
      "prefix": "crates/trusted-server-js/lib/src/shared/",
      "ids": ["RCJ-CORE-01", "RCJ-INT-02"]
    },
    {
      "prefix": "crates/trusted-server-js/lib/src/integrations/aps/",
      "ids": ["RCJ-APS-01", "RCJ-APS-02", "RCJ-APS-03", "RCJ-APS-04"]
    },
    {
      "prefix": "crates/trusted-server-js/lib/src/integrations/creative/",
      "ids": ["RCJ-CREATIVE-01", "RCJ-CREATIVE-02", "RCJ-CREATIVE-03"]
    },
    {
      "prefix": "crates/trusted-server-js/lib/src/integrations/gpt/",
      "ids": ["RCJ-GPT-01", "RCJ-GPT-02", "RCJ-GPT-03", "RCJ-GPT-04"]
    },
    {
      "prefix": "crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/",
      "ids": ["RCJ-DIAG-01"]
    },
    {
      "prefix": "crates/trusted-server-js/lib/src/integrations/prebid/",
      "ids": [
        "RCJ-PREBID-01",
        "RCJ-PREBID-02",
        "RCJ-PREBID-03",
        "RCJ-PREBID-04"
      ]
    },
    {
      "prefixes": [
        "crates/trusted-server-js/lib/src/integrations/datadome/",
        "crates/trusted-server-js/lib/src/integrations/didomi/",
        "crates/trusted-server-js/lib/src/integrations/google_tag_manager/",
        "crates/trusted-server-js/lib/src/integrations/lockr/",
        "crates/trusted-server-js/lib/src/integrations/osano/",
        "crates/trusted-server-js/lib/src/integrations/permutive/",
        "crates/trusted-server-js/lib/src/integrations/sourcepoint/",
        "crates/trusted-server-js/lib/src/integrations/testlight/"
      ],
      "ids": ["RCJ-INT-01", "RCJ-INT-02"]
    },
    {
      "prefix": "crates/trusted-server-js/lib/test/core/",
      "ids": ["RCJ-CORE-01", "RCJ-CORE-02", "RCJ-QUAL-01"]
    },
    {
      "exact": ["crates/trusted-server-js/lib/test/core/trace.test.ts"],
      "ids": ["RCJ-TRACE-01"]
    },
    {
      "prefix": "crates/trusted-server-js/lib/test/shared/",
      "ids": ["RCJ-INT-02", "RCJ-QUAL-01"]
    },
    {
      "exact": [
        "crates/trusted-server-js/lib/test/fixtures/aps-renderer-v1.json"
      ],
      "ids": ["RCJ-APS-01", "RCJ-APS-03", "RCJ-QUAL-01"]
    },
    {
      "prefix": "crates/trusted-server-js/lib/test/integrations/aps/",
      "ids": ["RCJ-APS-01", "RCJ-APS-02", "RCJ-APS-03", "RCJ-APS-04"]
    },
    {
      "prefix": "crates/trusted-server-js/lib/test/integrations/creative/",
      "ids": ["RCJ-CREATIVE-01", "RCJ-CREATIVE-02", "RCJ-CREATIVE-03"]
    },
    {
      "prefix": "crates/trusted-server-js/lib/test/integrations/gpt/",
      "ids": [
        "RCJ-BOOT-01",
        "RCJ-GPT-01",
        "RCJ-GPT-02",
        "RCJ-GPT-03",
        "RCJ-GPT-04"
      ]
    },
    {
      "prefix": "crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/",
      "ids": ["RCJ-DIAG-01"]
    },
    {
      "prefix": "crates/trusted-server-js/lib/test/integrations/prebid/",
      "ids": [
        "RCJ-PREBID-01",
        "RCJ-PREBID-02",
        "RCJ-PREBID-03",
        "RCJ-PREBID-04"
      ]
    },
    {
      "prefixes": [
        "crates/trusted-server-js/lib/test/integrations/datadome/",
        "crates/trusted-server-js/lib/test/integrations/didomi/",
        "crates/trusted-server-js/lib/test/integrations/google_tag_manager/",
        "crates/trusted-server-js/lib/test/integrations/lockr/",
        "crates/trusted-server-js/lib/test/integrations/osano/",
        "crates/trusted-server-js/lib/test/integrations/permutive/",
        "crates/trusted-server-js/lib/test/integrations/sourcepoint/"
      ],
      "ids": ["RCJ-INT-01", "RCJ-INT-02", "RCJ-QUAL-01"]
    },
    {
      "exact": ["crates/trusted-server-core/src/integrations/gpt_bootstrap.js"],
      "ids": ["RCJ-BOOT-01", "RCJ-GPT-01", "RCJ-GPT-02", "RCJ-GPT-03"]
    },
    {
      "exact": [
        "crates/trusted-server-core/src/integrations/gpt_diagnostics.rs",
        "crates/trusted-server-core/src/integrations/gpt_diagnostics_bootstrap.js"
      ],
      "ids": ["RCJ-DIAG-01"]
    },
    {
      "exact": ["crates/trusted-server-core/src/integrations/gpt.rs"],
      "ids": ["RCJ-GPT-01", "RCJ-GPT-02", "RCJ-GPT-03", "RCJ-GPT-04"]
    },
    {
      "exact": ["crates/trusted-server-core/src/integrations/prebid.rs"],
      "ids": [
        "RCJ-PREBID-01",
        "RCJ-PREBID-02",
        "RCJ-PREBID-03",
        "RCJ-PREBID-04"
      ]
    },
    {
      "exact": ["crates/trusted-server-core/src/integrations/aps.rs"],
      "ids": ["RCJ-APS-01", "RCJ-APS-02", "RCJ-APS-03", "RCJ-APS-04"]
    },
    {
      "exact": [
        "crates/trusted-server-core/src/integrations/datadome.rs",
        "crates/trusted-server-core/src/integrations/datadome/protection.rs",
        "crates/trusted-server-core/src/integrations/datadome/protection_scope.rs",
        "crates/trusted-server-core/src/integrations/didomi.rs",
        "crates/trusted-server-core/src/integrations/google_tag_manager.rs",
        "crates/trusted-server-core/src/integrations/lockr.rs",
        "crates/trusted-server-core/src/integrations/mod.rs",
        "crates/trusted-server-core/src/integrations/osano.rs",
        "crates/trusted-server-core/src/integrations/permutive.rs",
        "crates/trusted-server-core/src/integrations/sourcepoint.rs",
        "crates/trusted-server-core/src/integrations/testlight.rs"
      ],
      "ids": ["RCJ-INT-01", "RCJ-INT-02"]
    },
    {
      "exact": ["crates/trusted-server-core/src/trace_cookie.rs"],
      "ids": ["RCJ-TRACE-01"]
    },
    {
      "exact": ["crates/trusted-server-core/src/tsjs.rs"],
      "ids": ["RCJ-CORE-01", "RCJ-CORE-02", "RCJ-QUAL-01"]
    },
    {
      "exact": [
        "crates/trusted-server-integration-tests/browser/helpers/gpt-stub.ts"
      ],
      "ids": ["RCJ-GPT-01", "RCJ-GPT-02", "RCJ-GPT-03", "RCJ-DIAG-01"]
    },
    {
      "prefix": "crates/trusted-server-integration-tests/browser/tests/",
      "ids": ["RCJ-CORE-01", "RCJ-INT-01", "RCJ-QUAL-01"]
    },
    {
      "exact": [
        "crates/trusted-server-integration-tests/browser/tests/nextjs/gpt-diagnostics.spec.ts"
      ],
      "ids": ["RCJ-DIAG-01"]
    },
    {
      "exact": [
        "crates/trusted-server-integration-tests/browser/tests/nextjs/navigation.spec.ts"
      ],
      "ids": ["RCJ-GPT-01", "RCJ-GPT-02", "RCJ-GPT-03"]
    },
    {
      "exact": [
        "crates/trusted-server-integration-tests/browser/tests/shared/aps-renderer.spec.ts"
      ],
      "ids": ["RCJ-APS-01", "RCJ-APS-02", "RCJ-APS-03", "RCJ-APS-04"]
    },
    {
      "exact": [
        "crates/trusted-server-integration-tests/browser/tests/shared/creative-sandbox.spec.ts"
      ],
      "ids": ["RCJ-CREATIVE-01", "RCJ-CREATIVE-02", "RCJ-CREATIVE-03"]
    }
  ]
}
```

## 1. Problem statement and evidence

APS demand is integrated server-side, but APS creatives do not render reliably.
Four serial fixes—the `bid.meta` carrier, decoupled shim, `hb_adid` fallback, and
the baseline PUC/collapsed-shell fix—each repaired one edge while leaving other
independent failure points. The common failure is architectural: identity and
state are copied across loosely coordinated server, GPT, Prebid, PUC, and iframe
code, and many failures are swallowed.

The TSJS library has the same structural problem: two large GPT/Prebid modules,
duplicated ES5 and TypeScript behavior, imports in separately built IIFEs that do
not share module state, global expandos, multiple GPT wrappers, and asynchronous
work with no common owner.

### 1.1 Supported flows

| Flow      | Auction source                                  | Render owner             | APS route                                               |
| --------- | ----------------------------------------------- | ------------------------ | ------------------------------------------------------- |
| SSAT      | `tsjs.boot.auctionProjection`                   | GPT integration          | GAM Universal Creative → TS bridge → APS renderer       |
| Prebid    | Trusted Server Prebid adapter                   | Prebid + GPT integration | GAM Universal Creative → TS bridge → APS renderer       |
| Page bids | `/_ts/page-bids`                                | GPT integration          | GAM Universal Creative → TS bridge → APS renderer       |
| Direct    | `/auction`                                      | render service           | TS-owned iframe → APS renderer                          |
| Fallback  | opt-in child of an attributable empty GAM cycle | render service           | direct APS or direct ADM, according to the returned bid |

### 1.2 Known failure surfaces

| Area           | Failure                                                                                                                                                                                           |
| -------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Admission      | A configured mediator can discard direct-provider bids; scripts can be rejected by policy; strict dimensions and APS response validation can drop bids without a useful local reason.             |
| Identity       | PBS Cache UUID, upstream APS bid id, Prebid `adId`, GAM `hb_adid`, DOM id, and server slot id are different identities and have been conflated. GAM targeting values are capped at 40 characters. |
| GPT            | `display()` under disabled initial load does not request; event listeners can be installed too late; multiple refresh wrappers and concurrent requests race; SafeFrame obscures frame ancestry.   |
| Bridge         | A `Prebid Request` can be duplicated, replayed, sent by a wrong frame, or arrive after navigation. A bare bid id is not enough to establish ownership.                                            |
| Renderer       | The opaque sandbox cannot observe HTTP failure; descriptor validation exists in Rust, TypeScript, and embedded ES5; CSP or runner loading can fail after the iframe loads.                        |
| Direct auction | One fetch can contain several slots, but current cancellation and result handling are not batch-aware; failures collapse to an empty array.                                                       |
| Bootstrap      | Server bootstrap and bundle initialization can both believe they own runtime setup; a hung bundle can race the no-bundle fallback.                                                                |
| Lifecycle      | There is no shared definition of attempt, ownership, supersession, terminal completion, or disposal.                                                                                              |

## 2. Required behavior

### 2.1 Outcome contract

Each render attempt has one of four terminal outcomes:

```ts
type RenderFailureReason =
  | 'auction_timeout'
  | 'auction_disabled'
  | 'consent_denied'
  | 'slot_not_eligible'
  | 'provider_timeout'
  | 'provider_error'
  | 'invalid_provider_response'
  | 'mediation_failed'
  | 'winner_not_renderable'
  | 'internal_error'
  | 'network_error'
  | 'http_error'
  | 'invalid_response'
  | 'slot_unresolved'
  | 'descriptor_invalid'
  | 'invalid_dimensions'
  | 'dimensions_out_of_range'
  | 'no_render_source'
  | 'registry_full'
  | 'capability_registry_full'
  | 'external_queue_full'
  | 'external_ready_timeout'
  | 'external_artifact_incompatible'
  | 'prebid_admission_failed'
  | 'prebid_contract_violation'
  | 'prebid_selection_timeout'
  | 'reservation_collision'
  | 'identity_generation_failed'
  | 'cycle_unattributable'
  | 'slot_quarantined'
  | 'gpt_request_failed'
  | 'gpt_request_timeout'
  | 'gpt_completion_timeout'
  | 'reconciliation_capacity'
  | 'gam_empty'
  | 'bridge_claim_timeout'
  | 'bridge_id_mismatch'
  | 'owner_registration_timeout'
  | 'owner_insertion_timeout'
  | 'renderer_document_no_load'
  | 'runner_no_load'
  | 'runner_failed'
  | 'cache_network_error'
  | 'cache_http_error'
  | 'cache_invalid_response'
  | 'adm_document_no_load'
  | 'abi_mismatch'
  | 'bundle_partial'

type RenderOutcome =
  | { outcome: 'accepted' }
  | { outcome: 'no_bid' }
  | { outcome: 'failed'; reason: RenderFailureReason }
  | {
      outcome: 'cancelled'
      reason: 'caller_aborted' | 'superseded' | 'navigation_disposed'
    }
```

`accepted` means the path-specific completion authority reported success:

- APS: the sandboxed renderer document accepted the descriptor and the queued APS
  `prebid/creative/render` event invoked its success callback. APS owns the runner and
  its promise that it invokes this callback only after committing the nested creative
  iframe; Trusted Server cannot independently prove that promise for mutable upstream
  bytes. Loading the runner script alone is not acceptance.
- Direct ADM/cache: the TS-owned iframe fired its first `load` before timeout.
- PUC ADM/cache: the TS-authored PUC owner reported its owned iframe's first
  `load` over the bound owner channel.

It does not claim that pixels were viewable. `slotRenderEnded{isEmpty:false}` is
evidence that GAM injected a creative, not evidence that APS completed.

`no_bid` is reserved for an explicit, successfully parsed server auction decision
with no valid winner for that slot. An attributable GPT
`slotRenderEnded{isEmpty:true}` is
`failed{reason:'gam_empty'}` so an opt-in fallback can name its exact parent cause.
Network, timeout, HTTP, parse, descriptor, bridge, and renderer failures are not
converted to `no_bid`.

Every attempt owns one terminal latch. All competing callbacks, ports, iframe
events, timers, aborts, and navigation disposal race through that latch. The first
valid terminal transition wins, disposes attempt resources, and makes every later
signal inert.

### 2.2 Identity model

The implementation keeps these identities distinct:

| Identity                | Purpose                          | Rules                                                          |
| ----------------------- | -------------------------------- | -------------------------------------------------------------- |
| server slot id          | auction and publisher projection | exact, case-sensitive, 1–256 UTF-8 bytes, no NUL/control       |
| programmatic slot id    | direct-auction registration      | validated `code`; same bound; exact request/result identity    |
| DOM/container alias     | locating a page element          | separate collision-detecting index; ambiguous aliases fail     |
| upstream bid id         | provider provenance              | 1–64 UTF-8 bytes, no NUL/control, unique in provider response  |
| candidate id            | mediator round-trip              | server-minted opaque 12-character token; never an ordering key |
| PBS Cache UUID          | cache transport lookup           | preserved byte-for-byte as `cacheId`; never bridge authority   |
| native Prebid `adId`    | non-TS Prebid renderer lookup    | untouched for native bids; never entered in the TS store       |
| renderer reservation id | every TS-owned PUC capability    | `r1_` plus 22 base64url characters; exact `hb_adid`/TS `adId`  |
| attempt id              | in-page lifecycle ownership      | `a1_` plus 22 base64url characters; navigation-unique          |
| lifecycle ticket        | cross-window capability          | `t1_` plus 22 base64url characters; one-use and attempt-bound  |
| renderer nonce          | renderer-document capability     | `n1_` plus 22 base64url characters; one-use and attempt-bound  |

Every TS-owned PUC source—APS, inline ADM, or cache—receives a renderer reservation
id, and that id is copied exactly to GAM `hb_adid`. For the Trusted Server Prebid
adapter bid, it also replaces the TS bid's generated `adId` before targeting;
native Prebid bids are untouched. The server creates the id from 16 CSPRNG bytes
encoded as unpadded base64url and prefixed with `r1_`; it retries a response-local
collision at most eight times, then fails the bid with
`identity_generation_failed`. The browser rejects a collision with any live/
tombstoned reservation as `reservation_collision`. Cache UUID and upstream/provider
bid id stay inside the tagged render source/provenance and are never fallback bridge
credentials.

Attempt ids require no unbounded issued-id set. At `NavigationSession` creation the
browser obtains eight CSPRNG bytes with `crypto.getRandomValues` and keeps one
unsigned 64-bit attempt ordinal as two 32-bit words. For each new attempt it
increments the ordinal, concatenates the navigation prefix with the big-endian
ordinal, base64url-encodes those 16 bytes without padding, and prefixes `a1_`. The
fixed navigation prefix plus never-reused ordinal guarantees navigation-local
uniqueness. Prefix-generation failure or ordinal exhaustion refuses the new attempt
with `identity_generation_failed`; the ordinal never wraps. The active-attempt index
contains at most one attempt per admitted slot and is therefore capped by
`MAX_ACTIVE_SLOT_RECORDS = 256`; terminal settlement removes the strong entry, while
generation plus the nonreused ordinal makes stale callbacks inert without retaining
old ids.

One runtime-owned capability registry stores lifecycle tickets and their tombstones
with a shared capacity of 320. Before minting it prunes entries whose fixed
three-second lifetime has expired; unexpired entries are never evicted. A ticket is
16 fresh CSPRNG bytes encoded as the fixed `t1_` form and checked against every live/
tombstoned ticket. A collision retries at most eight total draws, then fails the
attempt with `identity_generation_failed`. Capacity exhaustion before a successful
draw refuses the outer PUC response and fails the attempt with
`capability_registry_full`. Consumption/disposal replaces the live entry with a
tombstone carrying the same original expiry; it never extends the lifetime.

Renderer nonces use the same eight-draw CSPRNG/collision rule in a separate live
registry capped at 256, at most one `n1_` value per active attempt. They need no
tombstone: the exact frame, port, attempt id, and generation remain mandatory, and
attempt disposal closes the channel and removes the live nonce before any later
attempt can act. Nonce capacity fails the attempt with `capability_registry_full`;
collision exhaustion is `identity_generation_failed`. Neither registry falls back
to timestamps, `Math.random`, truncation, or eviction.

### 2.3 Slot registry and bounded reservations

One runtime-scoped slot service owns the registry:

- `WeakMap<googletag.Slot, SlotRecord>` for GPT object identity;
- exact registered-slot-id index covering server and programmatic registrations;
- exact GPT ad-unit-code index;
- separate DOM alias index that rejects collisions;
- active render reservation map plus consumed/stale tombstones;
- request-intent and active-cycle state per slot.

`MAX_ACTIVE_SLOT_RECORDS` is 256 across server-projected and programmatically
registered records in one `NavigationSession`. The immutable server projection is
validated against that total before the kernel commits; an oversized projection is
an `abi_mismatch` boot failure. `addAdUnits` reserves capacity for its whole input
before mutation and rejects the whole call with
`AdUnitRegistrationError{code:'registry_capacity'}` when the remaining capacity is
insufficient. Navigation disposal synchronously removes every record and secondary
index owned by that navigation; reservations/tombstones retain only their separate
bounded lifecycle below.

Registration rejects a missing, empty, or greater-than-256-UTF-8-byte server or
programmatic slot id before indexing and assigns each valid slot a monotonically
increasing, navigation-local ordinal. An exact registered-slot-id collision is
rejected rather than overwritten. GPT ad-unit-code and DOM-alias indexes retain
collision state: a lookup must resolve exactly one record, and zero or multiple
matches fail `slot_unresolved`; registration order is never used to choose among
collisions.

The reservation map and tombstones share a capacity of 320. An SSAT/page-bid render
reservation has a fixed 15-minute lifetime measured by the runtime's monotonic clock
from browser registration; consumption does not extend it. A Prebid bid awaiting
client-side selection instead receives a ten-second admission lease. Exact selection
atomically promotes that lease to a render reservation with a new fixed 15-minute
lifetime measured from promotion; this occurs before targeting can expose the id and
is the only expiry replacement. An admission lease is suppress-only and cannot
satisfy a PUC claim; a request carrying that id before selection is refused,
tombstones the lease, and records `prebid_contract_violation`. Unselected, aborted,
or selection-timed-out entries
become tombstones only through their original ten-second lease expiry. Expired
entries are pruned.
Unexpired entries are never evicted because eviction would allow a late creative
request to escape TS ownership. At capacity, registration fails with
`registry_full`. A consumed, stale, or disposed reservation remains a tombstone
until its original expiry and never produces a second response. While live, each
admission lease or render reservation records its exact slot, render source,
immutable `WinnerContext{selectedCpm}`, navigation generation, expiry, and state.
`selectedCpm` is copied from the fully validated selected projected bid and is
finite and nonnegative. Prebid admission verifies that the frozen bid's `cpm` is
exactly this stored value, and selection promotion preserves the same context
rather than reconstructing it from Prebid. A successful PUC claim transfers the
context into the `RenderAttempt` before replacing the live entry with a tombstone.
The tombstone discards the render source and winner context and retains only the id,
original expiry, terminal state, and minimum suppression metadata. Neither the
descriptor nor any capability contains CPM.

Ids are unique across all live/tombstoned entries, so lookup identifies one entry
and then requires its exact active slot, cycle, and generation. The first compatible
PUC claim acquires its source and winner context; a live or tombstoned TS id is
suppressed before detailed validation.

Direct `/auction` rendering does not round-trip through a PUC reservation. Its exact
winner join creates the `RenderAttempt` with an immutable
`WinnerContext{selectedCpm}` copied from that same validated projected winner before
any rendering or cache fetch. Thus direct and PUC paths have the same CPM authority
without inventing a bridge capability for direct rendering.

The current `__tsRenderGeneration`, `__tsRenderBid`, and function-sentinel
expandos are removed.

### 2.4 GPT request-cycle ownership

GPT events describe physical requests and are not promises. The runtime therefore
tracks request intent separately from physical cycles:

1. A TS operation records an intent before calling `display()` or `refresh()`.
2. A TS operation never treats `display()` as request-capable while initial load is
   disabled. It uses `display()` only to register the slot, then invokes exactly one
   `refresh([slot], {changeCorrelator:false})`; the request intent and three-second
   start deadline attach only to that refresh. If the adapter cannot invoke refresh
   or the call throws, the attempt fails `gpt_request_failed`. A publisher-owned
   `display()` remains publisher activity and starts no TS attempt or fallback.
3. A physical cycle opens only on `slotRequested` and closes on the corresponding
   `slotRenderEnded`.
4. One TS-owned cycle may be outstanding per slot, with at most one queued
   replacement.
5. Attribution requires exactly one live compatible intent. Overlap or a later
   request-capable intent that makes ownership ambiguous fails the affected TS
   cycle with `cycle_unattributable`; it is never guessed from timing.
6. `responseIdentifier` deduplicates completion but is not an ownership token.
7. A cycle is re-armed only by counted completion or safe TS-owned
   destroy/redefine—never by a timeout or navigation disposal that merely hopes GPT
   has finished.

`slotRequested` and `slotRenderEnded` listeners are installed unconditionally
before any TS display/refresh call. SRA opens one logical cycle per participating
slot. Publisher-initiated GPT activity remains publisher-owned and cannot trigger
TS fallback.

An operation waiting for GPT or Prebid readiness has its own fixed ten-second
deadline measured from enqueue and fails `external_ready_timeout`; public
`requestAds.timeoutMs` never shortens or extends it. After a request-capable
`display()` or `refresh()` is
invoked, `slotRequested` must arrive within three seconds or the attempt fails
`gpt_request_timeout`. Its matching `slotRenderEnded` must arrive within ten seconds
of that same request invocation or the attempt fails `gpt_completion_timeout`.
A timeout tombstones the reservation, closes owned ports,
and settles the attempt, but does not pretend the physical GPT cycle completed.

At `gpt_request_timeout`, no attributable physical cycle exists. The adapter
immediately invokes the transactional TS-owned destroy/redefine contract in §5.7
and permanently retires the old object. Failure defines no replacement and leaves
the path quarantined; later TS work fails `gpt_request_failed`. A publisher-owned
object enters page-lifetime quarantine and cannot
accept new TS work until the publisher explicitly destroys that object or the page
reloads; no later `slotRequested` or `slotRenderEnded` may release that quarantine
because it cannot be attributed to the timed-out invocation. At
`gpt_completion_timeout`, the already-open exact physical cycle stays retired or
quarantined until its matching real completion, safe TS-owned destroy/redefine,
publisher destruction, or reload. Late GPT events only drain an already attributable
completion-timeout cycle; they cannot revive an attempt, start fallback, or re-arm a
request-timeout quarantine.

Navigation disposal does not manufacture a GPT completion. If the open slot is
TS-owned, the adapter invokes the §5.7 transaction; a replacement is defined only
when the current navigation still needs it and exact destruction succeeded. The
retired object remains in the runtime `WeakMap` until its late completion drains and
can never be matched to a replacement. Destroy failure leaves no second object and
quarantines later TS work. If it is publisher-owned, the physical cycle
is quarantined: new TS work for that GPT object fails `slot_quarantined` until the
matching `slotRenderEnded`, publisher destruction, or full page reload. There is no
timeout-based re-arm. In particular, an old completion after navigation but before
the replacement's completion settles only the retired/quarantined cycle.

### 2.5 SPA and concurrent work

- `RuntimeSession` survives SPA navigations and owns the global lifetime plus
  injected adapter/service disposers. Runtime-scoped slot and reservation services
  own the slot-object map, bridge listener, reservations/tombstones, and physical
  GPT cycle state; kernel code knows them only through interfaces.
- `NavigationSession` owns route-specific slot aliases, request intents, auction
  batches, attempts, timers, targeting history, and one internal immutable current
  auction-projection snapshot.
- A new navigation atomically replaces the prior `NavigationSession`; disposal
  cancels its live attempts and prevents late callbacks from mutating the new one.
- The initial session seeds its internal projection from the recursively frozen
  `tsjs.boot.auctionProjection`. A later SPA session begins with no current
  projection. Its page-bids controller accepts only one exact, fully validated
  `BrowserAuctionProjectionV1` for the current navigation generation, deep-copies
  and freezes it, transactionally registers all projected slots against the shared
  256-slot cap, then commits it to the session. A stale, duplicate, malformed, or
  over-cap response commits no slot, targeting, bid, or projection and cannot retain
  the prior navigation's data. Programmatic registrations admitted before that
  response count against the same transaction. The immutable public boot object is
  document-generation input and is never rewritten into a mutable current-state
  carrier.
- An `AuctionBatch` owns one `/auction` fetch and one child attempt per requested
  slot. Supersession cancels children individually. The shared fetch is aborted
  only when every child is terminal, the caller aborts all children, the batch
  response deadline expires, or navigation disposes.
- After a parsed response is processed, every still-live child receives the exact
  server decision for its slot; missing, duplicate, or inconsistent decisions are
  `invalid_response`, never inferred as `no_bid`.

### 2.6 Fallback

`SlotOperation` owns the public per-slot result and one primary `RenderAttempt`.
Fallback is opt-in and, when eligible, becomes a second child attempt. It may start
only after an attributable TS-owned GAM cycle terminates empty. Publisher-owned,
ambiguous, timed-out, quarantined, or stale cycles never trigger fallback. Each
child has its own immutable terminal result and local history. The operation
settles once: with the primary result when no fallback runs, or with the fallback
child result and `path:'fallback'` after the primary `gam_empty`. A child never
overwrites its parent or sibling.

## 3. APS wire and server contracts

### 3.1 One descriptor

The only APS render descriptor is:

```ts
interface ApsRendererV1 {
  type: 'aps'
  version: 1
  accountId: string
  bidId: string
  creativeId?: string
  tagType: 'iframe' | 'script'
  creativeUrl: string
  width: number
  height: number
  aaxResponse: string
}

interface AdmRenderSourceV1 {
  type: 'adm'
  version: 1
  adm: string
  width: number
  height: number
}

interface CacheRenderSourceV1 {
  type: 'cache'
  version: 1
  cacheId: string
  fetchUrl: string
  width: number
  height: number
}

type BidRenderSourceV1 = ApsRendererV1 | AdmRenderSourceV1 | CacheRenderSourceV1

interface CacheFetchPolicyV1 {
  version: 1
  baseUrl: string
}
```

`BidRenderSourceV1` is the only browser render-source union. A selected internal
Rust `Bid` carries exactly one corresponding enum member; separate optional
creative, renderer, and cache fields are removed. APS markup is not smuggled
through `adm`, `meta`, or debug fields. Each tagged object rejects unknown keys.
Limits are defined once and shared by the Rust producer, TypeScript parser, and
embedded renderer validator:

- nonempty `accountId` and optional nonempty `creativeId`, each at most 1,024
  UTF-8 bytes;
- nonempty `bidId` of at most 64 UTF-8 bytes;
- numeric, finite, integral `width` and `height`, each in the inclusive shared
  `RENDER_DIMENSION_MIN = 1` through `RENDER_DIMENSION_MAX = 4096` CSS-pixel range;
- `creativeUrl` at most 4,096 UTF-8 bytes, HTTPS, no credentials, and not the
  publisher origin;
- canonical standard-base64 `aaxResponse`, decoded size at most 256 KiB;
- exactly one decoded seat and one decoded bid;
- decoded bid id, dimensions, `creativeurl`, and `tagtype` exactly match the
  duplicated descriptor fields;
- finite, nonnegative decoded price.

A checked-in schema/corpus is the cross-language conformance source. Rust,
TypeScript, and the ES5 renderer validator run the same positive and adversarial
vectors. CI fails when generated ES5/schema output is stale. Semantic validation
that cannot be represented in JSON Schema remains in small handwritten validators
covered by the same corpus.

For `adm`, markup is nonempty and at most 512 KiB. For `cache`, `cacheId` is the
exact validated PBS Cache UUID. When cache rendering is enabled, the server emits
one trusted `CacheFetchPolicyV1` at `tsjs.boot.cachePolicy` before core; `baseUrl` is
the same immutable configuration snapshot used for auction projection and is an
absolute HTTPS URL of at most 4,096 UTF-8 bytes with a host and fixed nonempty path
but no credentials, query, or fragment. Core validates and freezes it before any
integration module prepares. A cache source without a valid boot policy is `descriptor_invalid`
and is never fetched.

`fetchUrl` is constructed server-side from that exact base URL, is at most 4,096
UTF-8 bytes, and has exactly one query parameter,
`uuid=<percent-encoded cacheId>`; it has no credentials, fragment, or other query
parameter. The browser parses both values and requires exact origin,
port, and pathname equality with the frozen base, empty username/password/hash,
exactly one `uuid`, decoded UUID equality with `cacheId`, and canonical search text
`?uuid=${encodeURIComponent(cacheId)}`. It then fetches with `redirect:'error'`,
`credentials:'omit'`, and `referrerPolicy:'no-referrer'`, enforces CORS, a
five-second deadline, a 512 KiB body limit, successful HTTP status, and a JSON
object response. The response requires an own, nonempty `adm` string of at most
512 KiB. Optional `w` and `h` must occur together as integral numbers within the
same 1–4096 range and must equal the source dimensions. Optional `price` must be a
finite nonnegative number but is never rendering authority. Other OpenRTB bid keys
are allowed and ignored; raw markup bodies, arrays/primitives, `width`/`height`
aliases, accessors, and alternate wrapper shapes are rejected. The response need
not echo `cacheId` because the exact request URL is its transport binding.
`${AUCTION_PRICE}` in the ADM is expanded only as
`String(attempt.winnerContext.selectedCpm)` from the context transferred by the
consumed reservation or installed by exact direct-winner admission, never from the
cached response's `price`, current projection, targeting, or a later winner. Only
the exact token is replaced;
`${AUCTION_PRICE:B64}` remains untouched. The resulting ADM enters the direct ADM
lifecycle.
Transport, status, or shape failure is respectively `cache_network_error`,
`cache_http_error`, or `cache_invalid_response`; none becomes `no_bid`.

### 3.2 Per-slot auction decisions

Every server auction entry point produces exactly one decision for every requested
slot, in request order:

```ts
type AuctionSlotFailureReason =
  | 'auction_disabled'
  | 'consent_denied'
  | 'slot_not_eligible'
  | 'provider_timeout'
  | 'provider_error'
  | 'invalid_provider_response'
  | 'mediation_failed'
  | 'winner_not_renderable'
  | 'identity_generation_failed'
  | 'internal_error'

type SlotAuctionDecisionV1 =
  | { slot: string; outcome: 'winner'; candidateId: string }
  | { slot: string; outcome: 'no_bid' }
  | { slot: string; outcome: 'failed'; reason: AuctionSlotFailureReason }

interface AuctionDecisionSetV1 {
  version: 1
  auctionId: string
  results: SlotAuctionDecisionV1[]
}

interface BrowserAuctionProjectionV1 {
  version: 1
  auction: AuctionDecisionSetV1
  bids: Array<{
    candidateId: string
    slot: string
    provider: string
    upstreamBidId: string
    cpm: number
    currency: 'USD'
    targeting: Record<string, string>
    rendererReservationId: string
    renderSource: BidRenderSourceV1
  }>
}
```

`BrowserAuctionProjectionV1` is exact, deny-unknown, and bounded before any slot,
reservation, targeting, or bid mutation. Its canonical UTF-8 JSON is at most
`MAX_BROWSER_AUCTION_PROJECTION_BYTES = 8 * 1024 * 1024`; `auction.results` and
`bids` each contain at most 256 entries; and all objects are plain own-data objects
with no accessors. Canonical serialization uses the interface field order shown,
request order for results, matching result order for bids, lexically sorted targeting
keys, and no insignificant whitespace. `auctionId` matches
`^[A-Za-z0-9._:-]{1,128}$`; candidate ids use
the exact 12-character base64url form from §3.4 and are unique; result slots are
unique, follow the §2.2 bound, and contain no NUL or ASCII control; every winner has
exactly one bid with the same slot/candidate and non-winners have none;
`rendererReservationId` uses the exact unique `r1_` form from §2.2; provider matches
`^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$`; and upstream bid ids are 1–64 UTF-8 bytes with
no NUL or ASCII control. CPM is a finite nonnegative number and currency is exactly
`USD`.

Each bid's `targeting` member is a plain own-data object with at most 32 entries. A key
matches `^[A-Za-z0-9_]{1,20}$`, is unique and case-sensitive, and cannot be
`hb_adid`, which the runtime alone synthesizes from the reservation. A value is
nonempty, contains at most 40 Unicode scalar values and 160 UTF-8 bytes, and contains
no NUL or ASCII control. The producer applies the same rules. A winning candidate
that cannot be projected becomes `winner_not_renderable`.

Aggregate overflow has one exact server outcome. The producer first constructs and
measures the complete canonical projection. If it exceeds 8 MiB, it transactionally
converts every `winner` result in that auction to
`failed{reason:'winner_not_renderable'}`, emits no projected winner bids, and retains
the existing no-bid/failed results in request order. For `/auction`, the corresponding
TS winner bids are likewise absent from `seatbid`; no unmatched decision or bid is
emitted. The reduced projection is guaranteed to fit from the 256-result/id bounds.
Initial HTML, page-bids, and direct response production use this same all-winners
rule, never a completion-order or first-fit subset. Boot rejects any independently
malformed or oversized value as `abi_mismatch`; page-bids and direct response
admission reject it transactionally as `invalid_response` with no partial slot,
reservation, targeting, or bid state.

Wrong type, nonfinite, fractional, zero, or negative render dimensions are
`invalid_dimensions`; an otherwise integral dimension outside 1–4096 is
`dimensions_out_of_range`. This exact distinction and range apply in the Rust
producer, TypeScript projection/source parser, ES5 APS renderer validator,
programmatic banner sizes, cache `w`/`h`, PUC/renderer DOM construction, and every
CSS/attribute layout assertion. No adapter may clamp an accepted dimension.

Each provider response is normalized internally to exactly one
`ProviderSlotOutcome`—candidate, no-bid, or typed failure—for every slot dispatched
to that provider. A successful response that omits a dispatched slot is a provider
no-bid; launch, transport, timeout, HTTP, parsing, validation, and mediation errors
are failures for the affected dispatched slots. Final aggregation is deterministic:

1. Pre-dispatch gating returns `auction_disabled`, `consent_denied`, or
   `slot_not_eligible` directly. A slot with zero eligible providers is exactly
   `failed{reason:'slot_not_eligible'}`, never a no-bid. Provider currency rejection
   is `invalid_provider_response`; there is no separate render-layer currency reason.
2. A selected deliverable candidate is `winner`, even if another provider failed.
3. Failure to mint a unique renderer reservation for the selected candidate is
   `failed{reason:'identity_generation_failed'}`. Any other failure to validate or
   project the selected candidate is `failed{reason:'winner_not_renderable'}`.
4. With no winner, the slot is `no_bid` only when at least one provider was
   dispatched and every eligible/dispatched provider
   completed successfully with no candidate.
5. With no winner and any provider or mediation failure, the slot is `failed` with
   the first applicable reason in this closed priority order: `internal_error`,
   `mediation_failed`, `invalid_provider_response`, `provider_error`,
   `provider_timeout`, `consent_denied`, `auction_disabled`, `slot_not_eligible`.
   `winner_not_renderable` and `identity_generation_failed` are selected directly by
   rule 3 and do not participate in multi-provider priority. Completion order is
   irrelevant.

`/auction` keeps ordinary OpenRTB winners in `seatbid` and places the decision set
at `ext.trusted_server.slot_results`. Every TS winner bid has this exact nested
extension; unknown keys inside `trusted_server` are invalid:

```ts
interface TrustedServerOpenRtbBidExtV1 {
  candidate_id: string
  slot_id: string
  render_source: BidRenderSourceV1
}
```

The bid's standard `id` is the server-minted renderer reservation id from §2.2;
the provider's upstream id remains provenance and, for APS, the descriptor's
`bidId`. The bid's standard `impid` must equal the request impression id mapped to
`slot_id`. A winner decision joins by exact `slot`, `candidateId`, `impid`, and
`slot_id` to exactly one bid. A no-bid or failed decision joins none. Missing,
duplicate, extra, or mismatched joins make the entire response invalid to TSJS.
TSJS renders only `render_source`; it never infers a source from standard OpenRTB
fields. If a bid also carries standard `adm`, it is permitted only for an ADM source
and must equal `render_source.adm` byte-for-byte; an `adm` on APS/cache or a mismatch
is invalid. `/_ts/page-bids` returns `BrowserAuctionProjectionV1`, and initial HTML
stores that same value at `tsjs.boot.auctionProjection`. They do not carry a second
legacy `{slots,bids}` interpretation. The deprecated `/__ts/page-bids` endpoint and
its JS fallback are deleted at cutover.

### 3.3 APS response admission

APS response handling is deterministic and reports typed local drop reasons:

- upstream bid id is required, bounded to 64 UTF-8 bytes, and unique within the
  provider response;
- malformed `contextual`, missing `creativeurl`, invalid tag type, invalid URL,
  invalid dimensions, disallowed script, and malformed price are rejected per bid
  where safe; one bad bid does not discard unrelated valid bids;
- dimensions must exactly match one requested size; values are never clamped;
- script creatives remain default-off and require an explicit security-approved
  setting; iframe creatives remain supported by default;
- the validated AAX projection used in `aaxResponse` is derived from the accepted
  bid, not re-parsed from a later lossy structure.

Drop reasons must remain visible in the existing debug/log surfaces for all auction
entry points. This is not a new external telemetry contract.

APS configuration accepts only canonical `account_id`; the `pub_id` deserialization
alias and its integer coercion are deleted at the hard cutover.

### 3.4 Mediation provenance

This work preserves the configured mediator's existing candidate selection and
timeout fallback behavior. It changes only the unsafe reconstruction boundary for
renderer-bearing source candidates:

1. Candidate provenance is `(provider_name, upstream_bid_id)`. Missing, duplicate,
   or oversized upstream ids are rejected.
2. Every candidate receives a response-unique, opaque, server-minted 12-character
   base64url `candidate_id` from 9 CSPRNG bytes. Generation retries a
   response-local collision at most eight times, then fails the affected slot with
   `internal_error`; the id is never an ordering key.
3. The mediation request carries it only at
   `ext.trusted_server.candidate_id`. A mediator-selected source candidate must echo
   exactly one known id. Missing, unknown, or duplicate echoes are
   `mediation_failed` and cannot borrow render data from another candidate.
4. The resolved candidate takes only the mediator-selected price and existing
   selection metadata from the mediator. Provider, upstream id, render source,
   dimensions, currency, and notifications come from the stored source candidate.
   Mediator-native render sources are rejected as out of scope.
5. Direct no-mediator selection keeps the repository's current highest-CPM rule;
   an exact CPM tie is resolved deterministically by provider name and upstream bid
   id. An APS bid explicitly declaring a non-USD currency is invalid. This design
   adds no auction currency or winner-selection configuration requirement.

### 3.5 Publisher projection and GAM targeting

The publisher bid projection carries `renderSource: BidRenderSourceV1` intact on
every path, plus the exact `candidateId` and `rendererReservationId`. Initial HTML
stores the document-generation input at
`tsjs.boot.auctionProjection: BrowserAuctionProjectionV1`. The initial
`NavigationSession` seeds its internal current projection from that immutable boot
value; an SPA page-bids response replaces only the new session's internal projection
through the transaction in §2.5. It never mutates `tsjs.boot`. A winner decision
must join exactly one projected bid and a no-bid/failed decision must join none.
Targeting applies one identity rule from §2.2 and never truncates a value to fit GAM.
If the chosen value cannot satisfy the 40-character targeting limit, the bid is
rejected before targeting with an explicit local reason.

Targeting cleanup is owner-and-value checked, not value-only. One runtime-owned
journal stack per physical GPT slot and targeting key records an internal owner id,
the exact installed string, and the predecessor value/owner for every TS write.
The sole GPT adapter observes each live slot's `setTargeting` and `clearTargeting`
calls and uses a closure-private reentrancy marker for TS-originated writes. Before
forwarding any publisher-originated mutation it invalidates the affected key's TS
restoration chain—or every chain for clear-all—regardless of whether the publisher
writes the same string. This bookkeeping never changes the publisher call's
arguments, return, throw, or order.
Before a write, a mismatch between the actual GPT value and the current TS frame
means publisher code changed the key; the runtime drops its restoration chain and
preserves that publisher value. Otherwise the new attempt pushes a distinct owner
frame before setting the value, even when its string equals the predecessor's.

Supersession, empty render, terminal failure, and navigation disposal mutate GPT
only when the disposing frame is current owner and the actual string still equals
its installed string. A current provisional frame then restores its immediate live
predecessor, or the original publisher value/absence. Disposing an older frame below
a newer owner performs no GPT write and rebases the successor to the removed frame's
predecessor. Acceptance promotes the new attempt's frames into its
`CommittedRenderArtifact`; disposal of the prior accepted artifact uses that same
non-top rebase. Thus two generations installing an identical string remain distinct:
newer success cannot be cleared by older disposal, newer failure can reveal the
still-live older value, and a publisher mutation is never overwritten.

Publishing a TS-owned PUC bid is one ordered transaction. The browser first
validates the winner, tagged source, slot join, and server-minted reservation id and
prepares all targeting/bid objects without exposing them. It then inserts the
reservation as live in the bounded store. Store capacity or collision therefore
fails before a creative can observe the id.

For SSAT/page-bids, only after successful insertion may it expose that same id as
GAM `hb_adid`, publish other targeting, record GPT intent, and invoke a
request-capable GPT operation—in that order. Any failure before request invocation
tombstones the reservation, compare-restores targeting, and settles the attempt.

For the Trusted Server Prebid adapter, the supported artifact is the content-addressed
external bundle built from exactly lockfile-resolved Prebid.js 10.26.0. The external
artifact contains no Trusted Server auction, admission, render, or refresh behavior.
The TS-owned `PrebidAdapter` exposes one internal version-pinned
`admitTrustedBid(preparedBid)` boundary:

```ts
interface PreparedTrustedBid {
  readonly auctionId: string
  readonly adUnitCode: string
  readonly bid: Readonly<{
    requestId: string
    adId: string
    cpm: number
    width: number
    height: number
    ad: ''
    ttl: 300
    creativeId: string
    netRevenue: true
    currency: 'USD'
    bidderCode: string
    meta: Readonly<{
      advertiserDomains: readonly string[]
      tsAuctionId: string
      tsBidId: string
      tsAdmHash?: string
    }>
  }>
}

interface PrebidAdapter {
  admitTrustedBid(
    preparedBid: Readonly<PreparedTrustedBid>
  ): 'admitted' | 'not_admitted'
}
```

All strings and numbers already satisfy their canonical auction/projection bounds;
`adId` is the exact admitted `r1_` reservation, and no renderer descriptor, ADM, cache
coordinate, or other capability crosses this boundary. The empty `ad` is deliberate:
the TS bridge resolves the already-stored tagged render source only after the PUC
claim. The adapter owns the bound Prebid object, registered `trustedServer` bidder
adapter, and the exact 10.26.0 response-admission callback; neither the external
artifact stamp nor publisher code exposes this method.

All validation, reservation insertion, exact replacement of the TS bid's `adId`, and
frozen bid-object construction complete before the call; no targeting or other
publisher-visible mutation precedes it. The boundary returns exactly
`admitted | not_admitted`, and the 10.26.0 artifact fixture proves `not_admitted`
leaves no bid/event/targeting state. `not_admitted` tombstones the reservation and settles
`failed{reason:'prebid_admission_failed'}`. A throw settles the same failure; detected
partial publication tombstones the id, suppresses every later PUC request, settles
`failed{reason:'prebid_contract_violation'}`, and fails the artifact-conformance
gate. Runtime handling is fail-closed even though the same violation blocks future
release. A Prebid version change requires a reviewed contract-fixture update.

`admitted` moves the reservation into an `awaiting_prebid_selection` state; it does
not create a render attempt or transfer permanent ownership. The Prebid adapter's
early synchronous `auctionEnd` listener uses the supported artifact's exact
auction-id/ad-unit winner query before publisher targeting callbacks. It promotes
only the exact selected TS `adId` to a render attempt and atomically tombstones every
other TS reservation admitted for that auction/ad unit as unselected. A ten-second
watchdog from admission performs the same tombstoning and records
`prebid_selection_timeout` if no matching `auctionEnd` arrives. Navigation or auction
abort tombstones the whole admitted set immediately. Subsequent GPT/render failure
follows the normal terminal lifecycle for the selected reservation. A fast Universal
Creative request can never race ahead of reservation lookup, and a losing bid cannot
hold capacity until the 15-minute tombstone expiry as a live entry.

### 3.6 Static renderer and APS runner proxy endpoints

`/integrations/aps/renderer/v1` and `/integrations/aps/runner.js` are always-reserved
Trusted Server routes. When APS is enabled, `GET` returns the local static renderer
or proxies the APS-hosted creative runner respectively. When APS is disabled, `GET`
returns a local `404 no-store`; neither route ever falls through to a publisher
origin. The family is dispatched before publisher auth, EC, and generic integration
filters. All adapters expose the same method, routing, security-header, and failure
semantics. Unsupported methods return local `405` with `Allow: GET`; unknown
renderer versions and the abandoned `/integrations/aps/runner/v1.js` shape return a
local `404 no-store`.

The renderer v1 body and headers are immutable and served with a long-lived immutable
cache policy; a renderer-body or CSP semantic change requires a new renderer route
version. The document is static and contains no descriptor data. Its iframe
`sandbox` attribute and response CSP `sandbox` directive contain exactly this token
set, serialized in this order:

```text
allow-forms allow-pointer-lock allow-popups allow-popups-to-escape-sandbox allow-scripts allow-top-navigation-by-user-activation
```

They omit `allow-same-origin`, `allow-top-navigation`, downloads, modals,
presentation, orientation lock, and storage-access escape. The exact renderer v1 CSP
header is:

```text
default-src 'none'; sandbox allow-forms allow-pointer-lock allow-popups allow-popups-to-escape-sandbox allow-scripts allow-top-navigation-by-user-activation; base-uri 'none'; object-src 'none'; script-src 'unsafe-inline' 'self' https:; connect-src https:; frame-src https: data: blob:; img-src https: data: blob:; media-src https: data: blob:; style-src 'unsafe-inline' https:; font-src https: data:; worker-src https: blob:; form-action https:;
```

The broad HTTPS resource directives are confined below the opaque outer sandbox and
are required for APS and bidder resources; `'self'` permits the local runner proxy in
HTTP-based hermetic adapters as well as production HTTPS. The response also has
exactly `Content-Type: text/html; charset=utf-8`,
`Cache-Control: public, max-age=31536000, immutable`,
`X-Content-Type-Options: nosniff`, and `Referrer-Policy: no-referrer`. It deliberately
omits both `X-Frame-Options` and a CSP `frame-ancestors` directive so publisher and GAM
creative ancestors can embed it; the iframe/CSP sandbox, nonce, source-bound port,
and descriptor validation are the embedding boundary. It does not forward publisher
credentials or authorization in TS-owned fetches. Runner-created APS creative
resources may use APS-origin cookies under ordinary browser policy. No cookie is
authority. Any header or CSP change requires renderer v2; policy reporting is a
separate observability design.

The runner endpoint is a thin runtime transport proxy, not an artifact store. Its
only upstream is the fixed APS URL
`https://client.aps.amazon-adsystem.com/prebid-creative.js`; no request field,
configuration value, query parameter, or header can select another target. Trusted
Server issues a credential-free, referrer-free `GET`, does not forward browser
cookies, authorization, client IP, or publisher headers, requests
`Accept-Encoding: identity`, and disables redirect following in the platform client.

The platform HTTP abstraction exposes an internal `ProxyResponseEvidenceV1` policy
used only by this route. It preserves upstream status and security-relevant evidence
for every occurrence of `Content-Type`, `Content-Encoding`, and `Content-Length`
before the generic proxy removes headers or consumes the body. An adapter that
exposes duplicate fields separately returns every raw value. An adapter runtime that
combines duplicates may return its exact combined value only when the combination is
visible and cannot be mistaken for a valid singleton; the closed grammars below
reject any comma/list form for all three headers. It is forbidden to split a combined
value or normalize it into an apparently valid singleton. All other upstream headers
are irrelevant because the successful response drops them.

The policy requests identity encoding, prevents redirect following, and returns the
identity body as a bounded stream or bounded buffer without transforming bytes.
Cloudflare inspects the initial Workers `Headers` values before the generic adapter
strips encoding/length; concatenated duplicate values remain combined and therefore
fail the singleton grammars. If the runtime erases encoding evidence or decodes a
non-identity response without exposing that fact, the evidence is `unavailable` and
the proxy rejects it. Fastly and Axum apply the same evidence/no-decompression
contract. No adapter may reconstruct erased evidence.

The entire upstream operation—from dispatch through the final response-body byte—has
a five-second monotonic deadline. Deadline expiry cancels the platform pending
request, discards every collected byte, and makes any unavoidable late platform
continuation self-discard without constructing a response. It returns the same local
failure as any other proxy error and cannot outlive the ten-second APS-completion
deadline. Spin does not use its current eager `spin_sdk::http::send` path for this
policy: it calls the WASI HTTP outgoing handler with supported request options and
polls the response/body stream against a monotonic-clock pollable for the total
deadline. Failure to set the transport timeouts or cancel/drop the pending resources
is `unavailable` evidence and prevents APS from being enabled on that build.

The proxy accepts only status `200`. `Content-Encoding` must be absent or exactly one
case-insensitive `identity` token; lists and every other coding are rejected. Exactly
one parseable `Content-Type` header is required. Its case-insensitive essence must be
`application/javascript` or `text/javascript`, with either no parameters or only one
case-insensitive `charset=utf-8` parameter; duplicate or unknown parameters and every
other media type or charset are rejected. The exact body cap is
`APS_RUNNER_MAX_RESPONSE_BYTES = 8 MiB` on every adapter. If exactly one canonical
decimal `Content-Length` matching `^(0|[1-9][0-9]*)$` is present, it is rejected
before collection when above the cap and must equal the final identity-body byte
count. Missing length is allowed; duplicate, malformed, mismatched, or conflicting
values fail. Collection stops and cancels the request as soon as streamed or buffered
bytes exceed the cap. The complete body must decode as UTF-8 without replacement;
validation never transforms the bytes.

A successful response relays the upstream body bytes unchanged while replacing
transport headers with
`Content-Type: application/javascript; charset=utf-8`,
`Access-Control-Allow-Origin: *`,
`Cross-Origin-Resource-Policy: cross-origin`,
`X-Content-Type-Options: nosniff`, and `Referrer-Policy: no-referrer`. Upstream
cookies and all other response headers are dropped. Upstream fetch, status, media
type, content encoding, redirect, length, size, or deadline failure returns a local
empty `502 no-store`; vendor bodies are never exposed in logs or error responses.
The adversarial parity corpus executes through each real adapter transport, including
Cloudflare and Spin wasm, rather than injecting already-normalized core responses. An
adapter that cannot preserve the raw evidence, common cap, or total deadline cannot
enable APS and blocks the release.

No APS runner bytes, digest, vendor-version record, redistribution license, update
script, generated artifact, or offline fallback is stored in Trusted Server source or
release artifacts. This design does not define runner caching behavior. The runner
route is intentionally unversioned because it represents a live APS-owned dependency,
not immutable TS-owned bytes.

Proxying does not make the runner trusted TS code or prove that it rendered. APS
remains the runtime owner of those executable bytes and its resolve/reject semantics
are a narrow external trust dependency. The outer opaque iframe, validated
descriptor, one-shot lifecycle port, and completion deadline contain execution and
reject missing, late, or misbound signals; they cannot determine whether APS told the
truth when it invoked `resolve`. The proxy never rewrites, inspects, or repairs the
JavaScript body.

The renderer accepts one parent-provided, nonce-bound descriptor plus the publisher
origin captured by the kernel before iframe creation. Because its opaque origin has
`location.origin === "null"`, it validates the supplied origin's shape and uses it
only to repeat the descriptor's not-publisher-origin check. It clears the nonce from
its URL and implements the TS side of `ApsRunnerContractV1`. Before runner load, it
creates a one-shot Promise and queues exactly:

```ts
new CustomEvent('prebid/creative/render', {
  detail: {
    aaxResponse: renderer.aaxResponse,
    seatBidId: renderer.bidId,
    source: 'internal',
    resolve,
    reject,
  },
})
```

`resolve` and `reject` are the Promise's one-shot functions and are the only
non-serializable fields. The renderer resolves `/integrations/aps/runner.js` against
its own absolute Trusted Server document URL and loads only that route. It creates the
script with `crossOrigin='anonymous'` and `referrerPolicy='no-referrer'`; it does not
set SRI because the proxy relays a live APS-owned artifact rather than immutable
TS-owned bytes. Under the APS conformance contract, the runner consumes the queued
event and promises to call `resolve` only after its asynchronous handler commits the
nested creative iframe, and to call `reject` for validation, load, or render failure.
Promise resolution sends one completed result; rejection, proxy/CORS error, runner
error, or script-load error sends one failed result over the transferred lifecycle
port. Runner `load` is intermediate progress only. The static renderer owns no APS
completion timer. Proxy/CORS/script-load failure maps to `runner_no_load`; callback
rejection maps to `runner_failed`.

A locally authored fictional fixture implements the exact proxy and
queue/resolve/reject behavior; it is not a copy, transformation, or derivative of the
APS body. Real-GAM tests exercise the live proxied APS dependency in all three
browsers and are a release prerequisite. The APS runner is allowed to change
upstream. Load failure, explicit rejection, and silence fail closed through the
existing APS-completion deadline. A changed or compromised APS runner can invoke
`resolve` prematurely or incorrectly; this is an accepted external-dependency risk
that the outer renderer cannot detect. Real-browser DOM/network conformance reduces
but does not eliminate it. V1 never loads the APS URL directly from the browser,
executes a stored fallback, treats script `load` as completion, or uses a reusable
global `postMessage` acknowledgement.

## 4. Render lifecycle protocol

### 4.1 State machine

```text
created
  -> waiting_for_gam_and_claim | rendering_direct
waiting_for_gam_and_claim
  -> waiting_for_owner | failed | cancelled
waiting_for_owner
  -> waiting_for_insertion | failed | cancelled
waiting_for_insertion
  -> waiting_for_document | waiting_for_adm | failed | cancelled
rendering_direct
  -> waiting_for_document | waiting_for_adm | failed | cancelled
waiting_for_document
  -> waiting_for_aps_completion | failed | cancelled
waiting_for_aps_completion
  -> accepted | failed | cancelled
waiting_for_adm
  -> accepted | failed | cancelled
```

Transitions are methods on `RenderAttempt`, not ad-hoc flag mutation. Each method
checks the expected state and terminal latch. Timers are created at the transition
whose deadline they enforce and are cleared by the transition that settles them.

An accepted transition first atomically promotes durable DOM/targeting ownership
from the attempt into one `CommittedRenderArtifact` owned by the exact slot and
navigation. The attempt disposer removes only uncommitted resources; promotion
detaches the committed iframe, targeting snapshot, and physical-slot metadata before
the terminal latch disposes the attempt. Direct TS iframes are removed by artifact
replacement or navigation disposal. PUC content remains owned by its physical GPT
slot: for a TS-owned slot the artifact may dispose it only through safe GPT
destroy/redefine, while publisher-owned slot DOM remains publisher-controlled and
the artifact releases only TS metadata and compare-restorable targeting. Before a
slot publishes another accepted artifact it disposes the prior artifact. Navigation
disposes artifacts according to those same ownership/quarantine rules. Claim,
registration, owner-control, and renderer-document ports/listeners that are no
longer needed close after terminal settlement and are never promoted.

### 4.2 Universal Creative claim

The supported GAM creative pins Prebid Universal Creative 1.17.2 by exact artifact,
not `latest` or a publisher-selectable version. Its cross-domain request is a JSON
string decoding to exactly
`{message:"Prebid Request",adId,adServerDomain}` and carries exactly one transferred
response port. All three values are strings; `adId` and `adServerDomain` are
nonempty. Object-form or extended payloads are rejected. Universal Creative owns
this shape, so it cannot carry a TS nonce. The checked-in hermetic PUC fixture is
generated from or pinned byte-for-byte to the supported source behavior.

The bridge is one capture-phase dispatcher installed as the first reversible core
effect in the synchronous activation barrier, before any integration-module
activation and before any TS-owned GPT or Prebid script injection. This guarantees
it precedes native non-capture listeners installed later by TS while leaving no
dispatcher active during asynchronous preparation;
publisher capture listeners that already ran are inside the publisher trust
boundary. The runtime's dispatcher owns both the initial-request branch below and
the owner-registration branch in §4.3; no second global listener exists. It performs
this order:

1. Perform only minimal, side-effect-free recognition. For a JSON string or a
   clone-safe plain object, inspect own data properties only and extract string
   `message`, `adId`, and an optional string `lifecycleTicket`. Do not read accessors
   or traverse prototypes. Malformed or unrecognizable data is ignored.
2. Route `message === 'TS Render Owner Register'` to §4.3. If
   `message !== 'Prebid Request'`, ignore it. For `Prebid Request`, look up the
   extracted `adId` in the global reservation/tombstone store before exact parsing,
   port, or slot-local checks.
3. For a non-TS id, do not suppress native Prebid. For a live or tombstoned TS id,
   immediately call `stopImmediatePropagation()` so an extended/object-form request,
   wrong port count, stolen capability, or replay cannot fall through.
4. Only after suppression, require the supported exact JSON-string shape and exactly
   one transferred port. A recognized TS id with invalid shape/port is generically
   refused when a usable port exists; all available ports are closed. It never
   reaches native Prebid.
5. The first exact live claim during the compatible active GPT cycle acquires the
   authoritative PUC `WindowProxy` from `MessageEvent.source` and stores that source
   with its response port. This is the only source acquisition step; SafeFrame
   ancestry is neither inspected nor guessed. Later owner messages must come from
   this exact source. The unguessable reservation id, exact active slot/generation,
   and one-time consumption authorize the first claim. Same-realm publisher code is
   already inside the documented trust boundary.
6. Join the buffered claim with an attributable nonempty `slotRenderEnded`. A claim
   that arrives first is bounded by the owning GPT-cycle/attempt deadline, discloses
   no render data, and holds only its source and port. A nonempty GAM result that
   arrives first starts a three-second claim deadline. Only when both conditions are
   true does the runtime atomically revalidate and consume the reservation.
7. Empty GAM, navigation disposal, supersession, an incompatible cycle, or the
   attempt deadline closes a buffered port, tombstones the reservation, and settles
   the attempt. A second claim is generically refused; it never replaces the first.

The successful outer response is an exact JSON string:

```ts
{
  message: 'Prebid Response'
  adId: string
  renderer: string
  rendererVersion: '3'
  tsOwner: {
    version: 1
    status: 'ready'
    kind: 'aps' | 'adm'
    lifecycleTicket: string
  }
}
```

`renderer` is the checked-in TS dynamic-owner program. `lifecycleTicket` is
`t1_` plus 22 unpadded base64url characters from 16 CSPRNG bytes, bound to the
attempt, generation, source, and reservation, with a fixed three-second TTL from
posting the outer response. No descriptor or ADM appears in the outer response. A
recognized TS id that cannot be served receives the same response shape with
`tsOwner:{version:1,status:'refused'}` and no other `tsOwner` keys; the dynamic owner
rejects immediately, causing PUC to emit its ordinary `adRenderFailed`. If no usable
response port exists, the listener can only suppress and close available ports.
The claim deadline expiry is `bridge_claim_timeout`.

### 4.3 Owner-control registration

PUC executes a dynamic renderer in a hidden `__pb_renderer__` iframe, so a global
message posted by that hidden frame cannot satisfy source binding. The TS owner must
instead call PUC's supplied `h.sendMessage(type,payload,onResponse)` helper. PUC
adds `adId`, serializes the request, sends it from the original PUC frame, and
creates the response channel.

The owner calls:

```ts
h.sendMessage(
  'TS Render Owner Register',
  { version: 1, lifecycleTicket },
  onRegistrationResponse
)
```

The kernel therefore receives exact JSON
`{message:"TS Render Owner Register",adId,version:1,lifecycleTicket}` from the
captured PUC `WindowProxy` plus exactly one helper-created response port. It
atomically consumes a live ticket and checks the exact `adId`, source, attempt, and
generation. Success posts exact JSON
`{message:"TS Render Owner Registered",adId,version:1,lifecycleTicket}` on that
response port and transfers exactly one newly-created owner-control port. Refusal
posts exact JSON `{message:"TS Render Owner Refused",adId,version:1}` with no
transferred port. These are the only registration responses.

This message is received by the same capture-phase dispatcher from §4.2. Its
registration branch first looks up the minimally extracted `lifecycleTicket` in the
runtime ticket/tombstone map. An unknown ticket is ignored for native/publisher
listeners. For a live or tombstoned TS ticket it immediately calls
`stopImmediatePropagation()` before exact JSON-shape or port validation, then checks
the captured source, exact `adId`, ticket, attempt, generation, and exactly one port.
A recognized invalid/replayed request is generically refused when a usable port
exists and all available ports are closed. Ticket settlement, consumption, or
attempt disposal replaces its live entry with a tombstone through the original fixed
ticket expiry; expiry prunes either live or tombstoned state. The dispatcher itself
is runtime-owned; attempt disposal performs that registry transition and removes
attempt handlers, not the global dispatcher.

The owner starts a three-second watchdog before invoking `h.sendMessage`, calls the
helper's returned stop-listening disposer after the first response, and rejects on
timeout, refusal, malformed data, or a port count other than one. The kernel owns
the opposite control port and the owner owns the transferred port. Replay, wrong
source, wrong port count, stale generation, or expiry cannot bind a channel. Kernel
and owner each have a terminal latch; late registration, acknowledgement,
settlement, or watchdog callbacks close their ports and remain inert.

### 4.4 APS document and runner acknowledgement

After registration, the kernel posts this exact control message and transfers
exactly one renderer-document port:

```ts
{
  message: 'TS APS Start'
  version: 1
  lifecycleTicket: string
  rendererUrl: string
  envelope: {
    version: 1
    nonce: string
    publisherOrigin: string
    renderer: ApsRendererV1
  }
}
```

The kernel owns the opposite document port. The owner creates exactly one iframe at
the versioned renderer URL with the nonce in its fragment, reports exact
`{message:"TS Owner Inserted",version:1,lifecycleTicket}` on the control port, and
on iframe load transfers the envelope and document port once to that exact
`contentWindow`. Direct APS uses the same document channel and envelope but has no
PUC owner-control channel. The nonce is 128-bit CSPRNG, attempt-bound, and one-use.

The static document sends only these exact document-port messages:

- `{message:"TS APS Document Accepted",version:1,nonce}` after nonce and descriptor
  validation;
- `{message:"TS APS Runner Loaded",version:1,nonce}` when the runner script loads,
  as nonterminal progress;
- `{message:"TS APS Render Completed",version:1,nonce}` when the queued APS render
  event invokes its one-shot success callback;
- `{message:"TS APS Render Failed",version:1,nonce,reason}` where `reason` is
  `descriptor_invalid | runner_no_load | runner_failed`.

Insertion has a one-second deadline, document acceptance has a three-second
deadline from iframe insertion, and the kernel is the sole owner of the ten-second
APS-completion deadline beginning at document acceptance. Callback silence at that
deadline maps to `runner_failed`; the static renderer never starts a competing
completion timer. Script load never accepts. Failure, timeout, port error,
supersession, or navigation disposal settles once. On a direct path the kernel
removes its exact pending iframe. On a PUC path the iframe is remote DOM owned only
by the dynamic owner; the kernel never claims it can remove that node and instead
posts exactly one owner-control settlement:

```ts
type OwnerSettlementV1 =
  | {
      message: 'TS Owner Settled'
      version: 1
      lifecycleTicket: string
      outcome: 'accepted'
    }
  | {
      message: 'TS Owner Settled'
      version: 1
      lifecycleTicket: string
      outcome: 'failed'
      reason: RenderFailureReason
    }
  | {
      message: 'TS Owner Settled'
      version: 1
      lifecycleTicket: string
      outcome: 'cancelled'
      reason: 'caller_aborted' | 'superseded' | 'navigation_disposed'
    }
```

The PUC owner owns the remote node, its DOM handlers, and its side of the control
port. An accepted settlement promotes that exact iframe as committed, removes its
temporary handlers, closes the control port, and resolves the renderer Promise once.
A failed or cancelled settlement removes the exact uncommitted iframe, removes its
handlers, closes the port, and rejects once so PUC emits its ordinary render-failure
event. The same cleanup applies to the PUC ADM/cache owner in §4.5.

When registration accepts the transferred control port, before waiting for an APS or
ADM start message, the owner arms one fail-closed 20-second settlement/channel
watchdog. Start does not extend or rearm it. The deadline is longer than every
kernel-owned insertion/document/render deadline and also covers registration-to-
start loss. The owner cancels it on settlement. A malformed control message,
`messageerror`, local owner disposal, or watchdog expiry performs the failed/
cancelled cleanup above and rejects once; a silently closed or lost control channel
is therefore bounded. This watchdog is remote resource cleanup only and cannot
report acceptance to the kernel or change its already-terminal outcome. A
settlement-post throw is isolated in the kernel because the remote watchdog owns
this failure path.

A caller `AbortSignal` remains attempt-owned after owner registration. If it wins
the terminal latch, the kernel closes its renderer-document channel and sends the
exact cancelled/`caller_aborted` settlement; direct rendering also removes the
kernel-owned iframe. The PUC owner performs the remote cleanup just specified. A
later insert, load, document message, APS callback, settlement, or watchdog is inert.

The winning descriptor dimensions are also the exact layout contract across all
three nested documents. Before inserting its renderer iframe, the PUC owner sets its
own document root and body to zero margin/padding with hidden overflow, then creates
one block iframe with matching positive width/height attributes and CSS pixels, zero
border, and no scrollbars. The static renderer document has the same zero
margin/padding and hidden-overflow root/body contract before loading the APS runner.
The runner-created descendant creative is expected to occupy the same viewport. A
300×250 winner therefore has 300×250 `clientWidth`, `clientHeight`, `scrollWidth`,
and `scrollHeight` in the PUC owner and renderer documents, and a 300×250 descendant
viewport, with no default eight-pixel body margin, clipping, or overflow. Equivalent
assertions run for every boundary fixture dimension. This is layout correctness, not
render completion; the callback contract above remains the acceptance authority.

### 4.5 ADM/cache ownership

Cache first resolves to bounded, validated ADM through the transport in §3.1. Direct
and PUC ADM/cache use one TS-authored iframe constructor and this exact ordered
sandbox value:

```text
allow-forms allow-popups allow-popups-to-escape-sandbox allow-scripts allow-top-navigation-by-user-activation
```

It omits `allow-same-origin`, downloads, modals, presentation, pointer lock, and
storage-access escape. The iframe has `referrerPolicy='no-referrer'`, exact positive
source dimensions in both attributes and CSS pixels, zero border/margin, hidden
overflow, `display:block`, `scrolling='no'`, title `Ad content`, and aria-label
`Advertisement`.

The constructor creates the iframe detached, installs one-shot `load`/`error`
handlers and disposal state, builds the complete ADM document, assigns exactly one
`srcdoc`, and only then appends the iframe once. It never appends an empty iframe,
sets `src`, or performs a TS-authored replacement navigation. A `load` is accepted
only when the exact frame is still the pending frame for the current attempt and
navigation generation, the intended `srcdoc` was assigned before append, and the
terminal latch is open. Initial `about:blank`, pre-assignment, removed-frame,
replaced-frame, stale-generation, post-disposal, duplicate, and error events cannot
accept. Error, removal before acceptance, or the five-second deadline fails
`adm_document_no_load`. Real-browser tests force initial-blank, replacement,
removal, error, timeout, supersession, and stale-load orderings.

For PUC ADM/cache, the trusted TS-authored owner—not bidder creative code—uses the
same registration protocol. The kernel sends exact
`{message:"TS ADM Start",version:1,lifecycleTicket,source:AdmRenderSourceV1}` on the
owner-control port with no transferred port. The owner reports
`TS Owner Inserted` only after the shared constructor appends exactly one owned
iframe, and reports exact
`{message:"TS ADM Loaded",version:1,lifecycleTicket}` or
`{message:"TS ADM Failed",version:1,lifecycleTicket}`. The kernel answers with one
`OwnerSettlementV1`; only that settlement resolves or rejects the PUC renderer
Promise. Insertion has a one-second deadline and load has a five-second deadline.
Direct ADM uses the same constructor and accepts through its own terminal latch on
the intended load. No secret or capability is injected into bidder-controlled
markup.

### 4.6 Channel ownership and parsing

| Channel               | Creator                   | Retained endpoint   | Transferred endpoint                      | Lifetime                                          |
| --------------------- | ------------------------- | ------------------- | ----------------------------------------- | ------------------------------------------------- |
| outer PUC response    | PUC `prebidMessenger`     | original PUC frame  | kernel global listener                    | one ready/refused response or claim disposal      |
| registration response | PUC `h.sendMessage`       | original PUC helper | kernel global listener                    | one registered/refused response or owner watchdog |
| owner control         | kernel after registration | kernel attempt      | hidden TS dynamic owner                   | insertion through final owner settlement          |
| renderer document     | kernel before APS start   | kernel attempt      | exact static APS renderer `contentWindow` | document acceptance through APS completion        |

Global window messages are JSON strings with the exact keys specified above. Port
payloads are structured-clone objects with the exact keys specified above. Every
parser rejects accessors, wrong prototypes, unknown keys, wrong literal/version,
wrong port counts, oversized strings, and already-consumed capabilities before
performing a state transition. The disposer clears handlers, closes both locally
owned ports where possible, and makes queued callbacks generation-inert.

The shared protocol corpus fixes these bounds and encodings:

- before `JSON.parse`, an inbound global-dispatcher string is at most 4,096 UTF-8
  bytes; a larger value is unrecognizable and causes no property access or state
  lookup;
- a TS `adId` is exactly the 25-character `r1_` reservation form; a lifecycle ticket,
  renderer nonce, and attempt id are exactly the respective 25-character `t1_`,
  `n1_`, and `a1_` forms from §2.2;
- `adServerDomain` is nonempty and at most 2,048 UTF-8 bytes. It is retained only for
  exact PUC-shape conformance and is never a fetch target or authority;
- `publisherOrigin` and `rendererUrl` are at most 2,048 UTF-8 bytes. The former must
  serialize an exact HTTP(S) origin with no path/query/fragment; the latter must equal
  the current generation's absolute `/integrations/aps/renderer/v1` URL with no query
  or fragment, after which the owner appends the exact `n1_` nonce fragment;
- a navigation/refresh generation is a nonnegative safe integer; and
- the generated dynamic-owner `renderer` program is at most 64 KiB UTF-8 and the
  complete successful outer-response JSON is at most 72 KiB. Build tests enforce
  both; refusal responses contain no renderer.

Structured-clone port payloads use their exact field-level limits: APS descriptor
256 KiB decoded AAX; ADM 512 KiB; `creativeUrl`, cache `baseUrl`, and cache
`fetchUrl` 4,096 UTF-8 bytes; `publisherOrigin`, `rendererUrl`, and
`adServerDomain` 2,048 UTF-8 bytes; server slot id 1–256 UTF-8 bytes with no NUL or
ASCII control; and the fixed
capability forms above. No generic unbounded string remains. Boundary-minus-one,
boundary, boundary-plus-one, multi-byte UTF-8, duplicate-key, and malformed-encoding
cases run through the producer plus both the global dispatcher and port parsers.

### 4.7 Notifications

APS has no `nurl` or `burl`; none is synthesized. Existing notifications on other
bid formats remain nonblocking and exactly-once per accepted lifecycle transition.
Notification transport failure cannot change a render outcome. Redesigning or
measuring notification delivery is outside this spec.

## 5. TSJS target architecture

### 5.1 Layers

```text
kernel/        boot, registry, queue, event bus, sessions, disposal, logging
adapters/      googletag, prebid, messaging
services/      slots, auction batches, render lifecycle, consent
integrations/  gpt, prebid, aps, creative, and existing publisher integrations
composition/   the sole construction root for adapters, services, and integration modules
```

- Kernel imports no adapter, service, or integration.
- Adapters import kernel contracts only and are the sole readers/writers of GPT,
  Prebid, and cross-window messaging globals.
- Services import kernel and adapter interfaces.
- Integrations compose services and never import another integration.
- Composition imports all layers, constructs one `RuntimeSession`, and injects
  adapters/services through interfaces. Kernel sessions never import or construct a
  concrete adapter or service.
- Layering is enforced by ESLint restricted paths.
- A custom scope-aware lint rule rejects GPT/Prebid global access outside adapters,
  including same-file aliases of `window`, `globalThis`, `self`, `googletag`, or
  `pbjs`.

### 5.2 One runtime across IIFE bundles

Each shipped integration remains a separately built IIFE with imports inlined, so
module singletons cannot be the shared-runtime mechanism. `tsjs-core` installs the
only runtime and keeps its service registry in the composition closure. During boot,
the temporary `_registerIntegration` handshake collects each accepted module; the
composition root alone invokes its exact frozen preparation/activation contexts.
After commit the handshake is permanently refusing and `tsjs._internal` exposes only
the frozen status described in §5.4.

Every other bundle registers through:

```ts
tsjs._registerIntegration({ id, release, prepare })
```

This is a release-internal bundle handshake, not a publisher extension API. An
**integration** remains the product capability; an **integration module** is only
that integration's transactional TSJS implementation unit. The design introduces
no separately installed or third-party plugin system.

The build first emits every production bundle with the same fixed release sentinel,
then computes one `releaseId`: 64 lowercase hexadecimal SHA-256 characters over a
canonical ordered manifest containing every bundle id and its sentinel-normalized
bytes. It replaces exactly one sentinel in each bundle and verifies none remains.
This avoids a self-referential hash while changing the id for any logical bundle or
ordering change. The same value is embedded in core and every integration bundle.
Before core is injected, the server emits this exact manifest:

```ts
interface BootManifestV1 {
  readonly version: 1
  readonly releaseId: string
  readonly integrations: readonly {
    readonly id: string
    readonly required: true
  }[]
}
```

Integration ids match `^[a-z0-9][a-z0-9_-]{0,63}$`, are unique, and appear in the actual
server injection order. The list contains exactly the enabled integration bundles;
all listed integrations are required for that page and there are at most 16. Core is injected first, then
integration modules in manifest order. Registration requires exact id membership and `release`
equality. Duplicate id, unknown id, wrong release, missing module, or registration
after the boot deadline fails with `abi_mismatch` or `bundle_partial`.
Integrations obtain stateful services from the registry; they never construct a
second runtime, slot registry, GPT adapter, or bridge listener.

Integration-module startup is a two-phase transaction. Registration stores code but
does not execute it. In manifest order, core calls a synchronous or asynchronous
`prepare(ctx)` whose only legal effects are validating frozen configuration,
obtaining injected service interfaces, allocating private inert data/closures, and
registering private-memory disposers. Preparation cannot read or write ad-tech
globals, attach a listener/observer/wrapper, touch live DOM, inject a script, start a
timer/fetch, schedule detached work, invoke publisher code, or call a stateful
adapter/service method. The one Promise returned to and awaited by core, including
its ordinary `await`/settlement continuations, is permitted; no continuation may be
detached from that Promise or survive its settlement/abort. Preparation returns
exactly one prepared module with a synchronous `activate(ctx)` function.

After every required module prepares, core enters one synchronous activation barrier
in manifest order. `activate(ctx)` may install only synchronously compare-restorable
wrappers, listeners, observers, guards, and service subscriptions. It registers the
disposer before each mutation and may stage bounded post-commit work through
`ctx.afterCommit(fn)`, but cannot inject/load a script, start network/timers, schedule
work, drain a publisher queue, or invoke publisher callbacks directly. The core
dispatcher and correctness-critical GPT listeners are the first reversible core
activations in this same barrier, not effects left live during asynchronous
preparation. If any activation throws, core synchronously runs every activated and
prepared disposer once in reverse order before committing fallback. Since the
barrier never yields and activation cannot call publisher code, no publisher task
can observe a partial generation.

The activation barrier checks the same monotonic boot deadline immediately before
and after every `activate` call and once more before kernel handoff. Elapsed time
greater than or equal to 10,000 ms synchronously unwinds and commits fallback even
when the timer task has not run. JavaScript cannot preempt an activation function
that never returns; a malicious/nonreturning same-realm module can freeze the page
and is an accepted platform limitation, not a second-runtime recovery case.

After all activations succeed, core commits the complete kernel API, runs staged
`afterCommit` callbacks in manifest order, and only then drains the preload queue.
Those callbacks may synchronously start scripts, timers, readiness work, and baseline
DOM scans; publisher code they intentionally invoke therefore sees the complete
kernel. A callback throw is isolated to its module, runs that module's remaining
disposers, records a bounded local runtime failure, and makes affected operations
fail through their typed readiness/render result; it cannot roll back an already
published kernel or create a fallback generation.

`ctx.signal` aborts pending preparation. `ctx.onDispose(fn)` is the only disposal
registration mechanism; a disposer registered after disposal runs immediately, and
one failing disposer does not prevent the rest. Each module may call
`ctx.afterCommit` at most once, so the at-most-16 pending modules stage at most 16
callbacks. A second call by one module throws during activation, unwinds the barrier,
and commits `bundle_partial`. The bootstrap deadline below is the only preparation
deadline; modules share its remaining budget and do not start independent ten-second
preparation clocks.

### 5.3 Bootstrap ownership

Bootstrap uses a generation-scoped state machine:

```text
unclaimed -> installing -> kernel
                     \-> failed -> fallback
```

Initial namespace capture is field-wise and does not replace a publisher-created
`window.tsjs` object: `window.tsjs ||= {}; tsjs.que ||= []; tsjs.boot ||= {}`. The
kernel remains externally inert and commits ownership only after all manifest
integration modules prepare and synchronously activate in order. Before module
work, bootstrap normalizes `que` to one actual Array and defines the `tsjs.que` data
property as writable false/configurable true for the installing generation. It keeps
the ingress Array's native `push`
throughout `installing`. Thus ordinary assignment cannot redirect the queue, and
callbacks pushed at any point during the shared deadline append to the same ingress
Array instead of a one-time snapshot. A preexisting non-Array `que` contributes no
callbacks and is replaced; an Array's existing own data entries are retained in
index order.

Kernel and fallback use the same synchronous, non-interleavable commit handoff in one
JavaScript task. Its order is exact:

1. create an empty actual-Array final executor, install its own immediate-execution
   `push`, and freeze the Array;
2. snapshot the ingress Array's callable own data entries in ascending index order;
3. clear the ingress Array and replace its `push` with a forwarder to the final
   executor for publishers retaining the old reference;
4. redefine `tsjs.que` as the final executor with a writable-false,
   configurable-false descriptor while installing all other complete committed
   `tsjs` fields;
5. for a kernel commit, run every staged `afterCommit` callback in manifest order;
   fallback has none; and
6. drain the snapshot FIFO.

No browser task or microtask can interleave steps 1–6. Code intentionally invoked by
an `afterCommit` callback sees the complete API and committed queue. A callback is therefore either
in the snapshot or reaches the final executor through one of the two queue
references, never lost or invoked twice. A callback that pushes while the snapshot
drains executes immediately through the committed queue before draining continues;
one throw is isolated. Both ingress and final values satisfy
`Array.isArray(...) === true`. The old ingress identity remains a live forwarding
queue; the public `tsjs.que` identity changes exactly once at commit.

One ten-second watchdog begins immediately before core injection and covers core,
registration, preparation, and the synchronous activation barrier for every required
integration module. A preparation throw/rejection, activation throw, ABI mismatch,
or deadline aborts the installing generation, synchronously unwinds registered
disposers in reverse order, and then commits the generated no-bundle fallback.
Bundles that arrive after
fallback are rejected and quarantined; they cannot register into or replace the
fallback generation. Late continuations verify their owner generation and
self-discard.

`gpt_bootstrap.js` becomes a queue-and-boot-data stub. The old bootstrap's
initial-load hooks, handoff wrappers, hydration scheduler, slot definition,
targeting, display, and refresh are deleted. Those behaviors run only after the
complete runtime commits. This intentionally changes the missing/partial-bundle
case: it no longer attempts a best-effort GPT render through a duplicated degraded
runtime, and instead settles every known slot through the terminal fallback below.
The fallback is generated from one TypeScript source and pinned by a staleness test;
behavior is not hand-maintained in both ES5 and TypeScript.

The fallback is a terminal, non-rendering shell, not a reduced second runtime. Its
commit atomically records one immutable boot failure reason:

- `abi_mismatch` for invalid manifest shape, duplicate/unknown integration id, wrong
  release, duplicate registration, or incompatible ABI; or
- `bundle_partial` for a missing required integration module, preparation
  throw/rejection, activation throw, or the shared boot deadline.

Before draining user work it installs `version:'1.0.0'`, the embedded `releaseId`, a
safe frozen `TsjsBootV1`, the final `tsjs.requestAds` input validator, the
validating-then-refusing `tsjs.addAdUnits`, the local `tsjs.log`, the
immediate-executor `tsjs.que`, a permanently refusing internal
`_registerIntegration`, and a frozen
`tsjs._internal` value containing only `{state:'fallback',releaseId,reason}`. It
constructs no runtime session, slot registry, GPT/Prebid adapter, bridge dispatcher,
timer, listener, port, or iframe. It never exposes a compatibility API.

The safe fallback boot uses the embedded release and
`manifest:{version:1,releaseId,integrations:[]}`, independently retains the server
auction projection only when that projection passes its exact shape/256-slot bounds,
field grammars, render limits, and 8 MiB aggregate cap from §§3.1–3.2, and otherwise substitutes exactly
`{version:1,auction:{version:1,auctionId:'fallback',results:[]},bids:[]}`. It
retains a valid cache policy or omits it, and substitutes the creative/diagnostics
disabled safe defaults from §§5.4/5.8 because no integration module commits. It never copies an accessor or
unknown property. Fallback batch membership comes only from exact server slot ids in
that validated immutable `tsjs.boot.auctionProjection` snapshot. Explicit valid ids present in that snapshot,
and every omitted-slot snapshot entry in projection order, resolve once as
`failed{path:'primary',reason:<boot failure reason>}`. An explicit id absent from the
projection resolves `slot_unresolved`; an already-aborted signal resolves each known
member as `cancelled{reason:'caller_aborted'}`. An empty projection plus omitted slots
resolves `{slots:[]}`. Input-shape errors still reject with `RequestAdsInputError`.

After installing those surfaces, fallback drains the preexisting callback queue FIFO
exactly once with `this === tsjs`; one callback throw does not prevent later callbacks.
Subsequent `que.push(fn)` executes a callable immediately once and ignores non-callable
values. Every late module registration is refused without invoking integration code, and
every late bundle continuation self-discards. Browser tests cover each failure
checkpoint, queued and later `requestAds`, callback throws, already-aborted signals,
and late bundles; no valid call remains pending.

### 5.4 Public surface after cutover

There are no compatibility aliases:

| Baseline surface                         | Final surface                                                                   |
| ---------------------------------------- | ------------------------------------------------------------------------------- |
| scattered `window.__tsjs_*` flags/config | `tsjs.boot.*`                                                                   |
| `tsjs.adSlots`/`tsjs.bids`               | initial `tsjs.boot.auctionProjection`; internal navigation projection after SPA |
| `tsjs.version === '0.1.0'`               | `tsjs.version === '1.0.0'` plus `tsjs.releaseId`                                |
| `globalThis.tscreative`                  | no callable equivalent; automatic creative module                               |
| `globalThis.tsCreativeConfig`            | `tsjs.boot.creative`                                                            |
| void/callback `requestAds`               | `tsjs.requestAds(options): Promise<RequestAdsResult>`                           |
| placeholder `renderAdUnit`               | `tsjs.requestAds({slots:[id]})`                                                 |
| placeholder `renderAllAdUnits`           | `tsjs.requestAds()`                                                             |
| generic mutable `setConfig`/`getConfig`  | immutable `tsjs.boot.*` plus typed integration config                           |
| `tsjs.renders`/`renderLog`/`renderSeq`   | `tsjs.diagnostics.renderTrace`                                                  |
| `window` event `tsjs:adRendered`         | `tsjs.diagnostics.renderTrace.subscribe(listener)`                              |
| `tsjs.gptDiagnostics`                    | `tsjs.diagnostics.gpt`                                                          |
| `window.__tsjs_prebid_bundle`            | exact own `pbjs.__trustedServerArtifactV1` stamp                                |
| integration install/patch sentinels      | kernel integration registry/`WeakSet`                                           |
| GPT slot expandos                        | `SlotRecord`                                                                    |

`window.tsjs.que` remains the pre-load command queue because it is the bootstrap
transport, not a legacy behavior alias.

The complete committed public API is the following union; the pre-load
`{que,boot}` transport is not a committed API generation:

```ts
interface CreativeBootV1 {
  readonly version: 1
  readonly enabled: boolean
  readonly clickGuard: boolean
  readonly renderGuard: boolean
}

interface TsjsBootV1 {
  readonly abi: 1
  readonly releaseId: string
  readonly manifest: Readonly<BootManifestV1>
  readonly auctionProjection: Readonly<BrowserAuctionProjectionV1>
  readonly cachePolicy?: Readonly<CacheFetchPolicyV1>
  readonly creative: Readonly<CreativeBootV1>
  readonly diagnostics: Readonly<DiagnosticsBootV1>
}

interface TsjsCommandQueue {
  readonly length: 0
  push(callback: unknown): 0
}

interface TsjsApiBase {
  readonly version: '1.0.0'
  readonly releaseId: string
  readonly boot: Readonly<TsjsBootV1>
  readonly que: TsjsCommandQueue
  readonly log: TsjsLog
  /** Release-internal late-bundle sink; always returns false after commit. */
  readonly _registerIntegration: (registration: unknown) => false
  addAdUnits(
    units: ProgrammaticAdUnit | readonly ProgrammaticAdUnit[]
  ): AddAdUnitsResult
  requestAds(options?: RequestAdsOptions): Promise<RequestAdsResult>
}

interface TsjsKernelApi extends TsjsApiBase {
  readonly diagnostics: Readonly<TsjsDiagnostics>
  readonly _internal: Readonly<{ state: 'kernel'; releaseId: string }>
}

interface TsjsFallbackApi extends TsjsApiBase {
  readonly diagnostics?: never
  readonly _internal: Readonly<{
    state: 'fallback'
    releaseId: string
    reason: 'abi_mismatch' | 'bundle_partial'
  }>
}

type TsjsApi = TsjsKernelApi | TsjsFallbackApi
```

`version` is the semantic public-API generation and changes only with a reviewed API
contract; `releaseId` identifies the exact bundle set and equals
`boot.releaseId`/`boot.manifest.releaseId`. Core recursively freezes the boot value
before installing integrations. Integration-specific configuration is not a public
mutable bag: the composition root validates each server-projected, deny-unknown
config against that integration's typed schema and passes the frozen value only in
its preparation/activation contexts. `_internal` is a frozen, non-enumerable status
value; the service registry remains in the composition closure and is available to
integration modules only through those contexts during startup.

The final queue is the frozen actual empty Array from the commit handoff and contains
no retained callbacks. Its own `push` invokes one callable
immediately and exactly once with `this === tsjs`, returns `0`, ignores a
non-callable, and isolates/logs a throw. Pre-load callbacks are snapshotted and
drained FIFO only after the committed API is installed, so publisher callbacks never
run against a half-installed generation. Native mutators, borrowed Array mutators,
index assignment, `length` assignment, deletion, and property definition cannot
change the frozen executor or retain a callback; failure follows ordinary strict- or
sloppy-mode JavaScript semantics and `length` remains `0`.

#### 5.4.1 Programmatic ad-unit registration

The clean public core surface retains programmatic direct-auction registration:

```ts
interface ProgrammaticAdUnit {
  code: string
  mediaTypes: {
    banner: { sizes: readonly (readonly [number, number])[] }
  }
  bids?: readonly {
    bidder: string
    params?: Readonly<Record<string, unknown>>
  }[]
}

interface AddAdUnitsResult {
  readonly registered: readonly string[]
}

type AdUnitRegistrationErrorCode =
  | 'invalid_units'
  | 'invalid_unit'
  | 'invalid_code'
  | 'duplicate_code'
  | 'slot_collision'
  | 'invalid_media_types'
  | 'invalid_dimensions'
  | 'dimensions_out_of_range'
  | 'invalid_bids'
  | 'invalid_bidder'
  | 'invalid_params'
  | 'request_body_too_large'
  | 'registry_capacity'

class AdUnitRegistrationError extends Error {
  readonly code: AdUnitRegistrationErrorCode
  readonly unitIndex?: number
}

class TsjsUnavailableError extends Error {
  readonly code: 'runtime_unavailable'
  readonly releaseId: string
  readonly reason: 'abi_mismatch' | 'bundle_partial'
}
```

Registration is synchronous, all-or-nothing, and navigation-scoped. `code` becomes
the exact slot id and must satisfy the server slot-id UTF-8 bound from §4.6. The
argument is one unit or a nonempty array of at most 256 plain data objects. Codes are
nonempty and unique in the call; banner sizes are nonempty integral number pairs in
the shared 1–4096 renderer range; bidder names are nonempty and at most 64 UTF-8
bytes; params are plain JSON-compatible data with the same request-body cap as
`/auction`. Accessors,
unknown media types, duplicate codes, or a collision with any server-projected or
already registered slot reject the whole call with a typed
`AdUnitRegistrationError` before state changes. There is no merge-by-code behavior.
The outer shape/count maps to `invalid_units`; a non-plain unit or unknown/accessor
unit field to `invalid_unit`; code shape, in-call duplicate, and registry collision to
`invalid_code`, `duplicate_code`, and `slot_collision`; media/banner shape to
`invalid_media_types`; a nonnumeric/nonfinite/fractional/nonpositive dimension to
`invalid_dimensions`; an integral dimension outside 1–4096 to
`dimensions_out_of_range`; bid-array and bidder-name shape to
`invalid_bids`/`invalid_bidder`; non-JSON, cyclic, accessor-bearing, or otherwise
unserializable params to `invalid_params`; and encoded body overflow to
`request_body_too_large`. `unitIndex` is the lowest failing input index when the
failure belongs to a unit and is absent for outer/capacity errors. Validation order
is the order just listed, then capacity reservation; repeated runs return the same
code/index without reading publisher accessors.

Successful units receive registration ordinals after the immutable server projection
and participate in later `requestAds` snapshots. They use the direct auction/render
path unless an explicit future design gives them a GPT mapping; registration alone
never defines, displays, refreshes, or targets GPT. The old placeholder-writing
methods are deleted rather than aliased. `tsjs.log` retains the existing bounded
level/method surface, while runtime configuration is immutable boot data or typed
integration-owned configuration. Fallback validates `addAdUnits` input and then throws
`TsjsUnavailableError` with the committed boot failure; it constructs no registry.

The retained logger has this exact hard-cutover surface:

```ts
type TsjsLogLevel = 'silent' | 'error' | 'warn' | 'info' | 'debug'

interface TsjsLog {
  setLevel(level: TsjsLogLevel): void
  getLevel(): TsjsLogLevel
  error(...values: readonly unknown[]): void
  warn(...values: readonly unknown[]): void
  info(...values: readonly unknown[]): void
  debug(...values: readonly unknown[]): void
}
```

The initial level is `warn`. `setLevel` accepts only the five exact strings above;
an invalid runtime value throws `TypeError` without changing the current level.
Missing/throwing console methods are swallowed at the logger boundary, log failures
never change ad behavior, and the logger does not retain argument arrays. The
fallback exposes the same logger and level behavior.

### 5.5 Direct auction API

```ts
interface RequestAdsOptions {
  slots?: readonly string[]
  timeoutMs?: number
  signal?: AbortSignal
}

type RequestAdsInputErrorCode =
  | 'invalid_options'
  | 'invalid_slots'
  | 'empty_slots'
  | 'duplicate_slot'
  | 'invalid_timeout'
  | 'invalid_signal'

class RequestAdsInputError extends Error {
  readonly code: RequestAdsInputErrorCode
}

type RequestAdsSlotResult =
  | { slot: string; path: 'primary' | 'fallback'; outcome: 'accepted' }
  | { slot: string; path: 'primary' | 'fallback'; outcome: 'no_bid' }
  | {
      slot: string
      path: 'primary' | 'fallback'
      outcome: 'failed'
      reason: RenderFailureReason
    }
  | {
      slot: string
      path: 'primary' | 'fallback'
      outcome: 'cancelled'
      reason: 'caller_aborted' | 'superseded' | 'navigation_disposed'
    }

interface RequestAdsResult {
  slots: RequestAdsSlotResult[]
}
```

Every `RequestAdsOptions.slots` entry is an exact, case-sensitive registered slot id:
either a server slot id from §2.2 or a programmatic `code` admitted by §5.4.1. The
public API never interprets an entry as a GPT ad-unit path, DOM id, or DOM alias. An
explicit id absent from the invocation snapshot resolves individually as
`failed{reason:'slot_unresolved'}` while valid siblings proceed. Internal GPT-path or
DOM-alias lookup that has zero or multiple matches also fails the affected slot as
`slot_unresolved`; it never selects the first registration.

When `slots` is omitted, `requestAds` synchronously snapshots every server-projected
and programmatic slot registered in the current `NavigationSession`, ordered by its
navigation-local registration ordinal. That immutable snapshot is the batch
membership and result order; a slot registered after invocation is excluded. When
`slots` is present, its validated input order is the result order. The `slot` field in
every result is always the exact registered slot id.

When `timeoutMs` is omitted, the shared auction-response deadline is exactly 10,000
milliseconds. An explicit value replaces that default and must satisfy the bounds
below; renderer/GPT path deadlines remain independently fixed by their lifecycle
transitions.

Omitted/`undefined` options are valid. Otherwise the argument must be a non-null
plain object whose prototype is this realm's `Object.prototype` or `null`, containing
only `slots`, `timeoutMs`, and `signal` as own enumerable data properties. Accessors
or unknown keys are `invalid_options`; non-array slots, non-string/empty/>256-byte
slot values, and more than 256 values are `invalid_slots`; an explicitly empty array
is `empty_slots`; an exact duplicate is `duplicate_slot`; a noninteger timeout
outside `100..30_000` is `invalid_timeout`; and a value that fails the platform
`AbortSignal.prototype.aborted` brand getter is `invalid_signal`. These reject
before attempts with `RequestAdsInputError`. Once an attempt is created, the
returned promise resolves with per-slot terminal results and does not reject for
auction or render failures. Unknown requested slots fail individually while valid
siblings proceed. Omitted slots with an empty registry resolve an empty `slots`
array. A registration collision cannot alter the snapshot or result ordering.

The response deadline governs only the shared auction fetch. Once a response parses,
path-specific renderer deadlines take over. The caller signal remains active until
all child attempts settle.

### 5.6 Adapters and external readiness

GPT and Prebid adapters expose `present | pending | timed_out | incompatible`.
`timed_out` and `incompatible` are not permanent global failures: a later valid
external replacement can satisfy later operations.
Each queued operation owns its own deadline and disposal; an expired operation is
removed instead of running unexpectedly when the external library appears later.
Each adapter queue holds at most 64 live operations, drains FIFO, and fails only the
overflowing operation with `external_queue_full`. The operation deadline is exactly
ten seconds from enqueue and is independent of the auction-fetch response deadline;
expiry is `external_ready_timeout`. Readiness at the boundary races through the
operation's terminal latch, so exactly one of dispatch or timeout wins.

The GPT adapter owns early event subscription, command-queue interaction,
`display`, `refresh`, targeting, and service-state inspection. The Prebid adapter
owns commands, event subscription, bid-response registration, and Universal
Creative integration. Tests use adapter fakes rather than mutable global objects.

The decoupled Prebid artifact remains pure Prebid.js and retains its independent
five-second queue-drain watchdog. The generated wrapper arms that watchdog as its
first statement, before stamp inspection or module initialization. At 5,000 ms it
looks up the then-current real Prebid object and calls its idempotent `processQueue()`
at most once for that wrapper, whether or not the TS integration installed, so
publisher callbacks are not held hostage by stamp conflict, a missing TS bundle, or
a partial/duplicate artifact. Duplicate wrappers may reach the idempotent API, but
black-box tests require each queued publisher callback to execute once. A later TS integration module may call the idempotent API again.
The module first verifies the real Prebid API, then installs transactionally. It does
not install the synthetic-refresh policy, clear targeting, or mutate publisher bids
when only the injected `{que,cmd}` stub exists. The artifact manifest carries both
module stems and runtime bidder codes, including aliases.

The artifact exposes this frozen plain-data runtime stamp; the separately emitted
build manifest contains the same fields plus filename/integrity metadata:

```ts
interface ExternalPrebidArtifactV1 {
  readonly abi: 1
  readonly artifactReleaseId: string
  readonly prebidVersion: '10.26.0'
  readonly moduleStems: readonly string[]
  readonly bidderCodes: readonly string[]
  readonly bidderAliases: readonly {
    readonly code: string
    readonly moduleStem: string
  }[]
  readonly userIdModules: readonly {
    readonly moduleName: string
    readonly configNames: readonly string[]
    readonly eidSources: readonly string[]
  }[]
}
```

After arming the watchdog and before executing embedded Prebid module factories, the
wrapper inspects the current `window.pbjs` and its own descriptor for
`__trustedServerArtifactV1`. If that object already has the required real API plus a
valid recursively frozen stamp, an exact same-release/content duplicate reuses it and
skips module initialization/redefinition; a different valid release refuses only the
new wrapper and likewise leaves the already-working object untouched. A different
artifact can become active only by replacing the whole `window.pbjs` object.

Otherwise the wrapper initializes its embedded pure Prebid modules. It then attempts
to define one own non-enumerable, non-writable, non-configurable data property whose
value is the recursively frozen `ExternalPrebidArtifactV1` only when the descriptor
is absent. Define failure is caught locally. An accessor, inherited-only value,
invalid stamp, or hostile/different non-configurable value is left untouched and
records at most one bounded console warning when possible. None of these paths
throws, cancels the already-armed watchdog, or prevents publisher Prebid from
initializing; they make only TS readiness incompatible.

This inert build description is the artifact's only Trusted Server handshake. Apart
from the independent Prebid queue self-start watchdog specified above, the generated
wrapper performs no auction, admission, render, targeting, or refresh behavior. The
Prebid adapter reads the property only through `Object.getOwnPropertyDescriptor`,
rejects an accessor or inherited value, and captures both the `pbjs` object and stamp
identities in one `PrebidArtifactBinding`. Every operation rechecks both identities;
replacement of `window.pbjs` invalidates only that binding and later readiness may
bind the new object if it carries a valid stamp. No `window.__tsjs_*` stamp or
fallback lookup exists.

The artifact build contains exactly one 64-zero-character release sentinel in that
runtime stamp. It hashes the emitted JavaScript after normalizing that one field back
to the sentinel, writes the resulting 64 lowercase hexadecimal SHA-256 characters
into the field, and verifies that no sentinel remains. The separately emitted build
manifest records that same `artifactReleaseId` plus the ordinary SHA-256/SRI of the
final bytes. Thus the embedded id has a non-self-referential preimage; it is
diagnostic artifact identity, not a requirement to match the TSJS `releaseId`.

`prebidVersion` must be exactly `10.26.0`; changing it requires the reviewed
artifact-contract fixture update in §3.5. Module/code/config names are nonempty,
unique in their array, and at most 128
UTF-8 bytes; EID sources are lowercase, nonempty, unique per module, and at most 256
UTF-8 bytes. The manifest admits at most 256 module stems, 512 bidder codes, 512
alias rows, and 128 user-ID modules with at most 64 config names and 64 EID sources
each. Every alias code appears in `bidderCodes`, every alias module appears in
`moduleStems`, and each configured `client_side_bidder` must appear in
`bidderCodes`. Arrays are lexically sorted so build/runtime fixtures compare exact
content.

A missing stamp, wrong `abi`, invalid release/version, malformed/oversized member,
missing configured bidder, missing required user-ID module/EID mapping, or a real API
missing a required method makes the current readiness operation
`external_artifact_incompatible`. It records one bounded local diagnostic and does
not install TS refresh interception or mutate publisher state. An older unstamped
artifact remains ordinary publisher Prebid: its own 5,000 ms watchdog drains its
queue exactly once, and the later TS module does not replay publisher callbacks.
Replacement by a valid artifact can satisfy later operations. Compatibility requires
both `abi:1` and exact Prebid 10.26.0; external artifacts are not pinned to a TSJS
release id.

### 5.7 GPT correctness retained during decomposition

- Subscribe to `slotRequested` and `slotRenderEnded` before any TS request.
- Pass `changeCorrelator: false` for TS refreshes unless the explicit configuration
  says otherwise.
- Call `enableSingleRequest()` only before services are enabled; never reconfigure a
  publisher-owned GPT service after `enableServices()`.
- Restore the intended initial-load behavior represented by issue #922/PR #997 and
  pin it with tests.
- Responsive-size ambiguity fails `slot_unresolved` and never silently skips or
  chooses an arbitrary container.
- A TS fallback slot is defined on the resolved inner div, never its outer
  `-container`. An exact later publisher `defineSlot` receives that same live slot
  even when its path/formats differ, with a local mismatch warning, because defining
  a second physical slot would violate the one-placement invariant. A
  hydration-renamed alias is accepted only when the original element is gone and
  exactly one live, unclaimed TS fallback shares the configured prefix, exact GAM
  path, and normalized formats. Ambiguity remains native and cannot transfer TS
  ownership.
- Successful handoff synchronously transfers ownership, removes the slot from the
  TS destroy set, suppresses exactly the publisher's duplicate initial `display`,
  and under disabled initial load suppresses exactly its duplicate first refresh.
  A global refresh expands the live GPT slot list, filters only one-shot suppressed
  slots, and forwards unrelated slots with the original options.
- Publisher calls are not held until TS targeting is ready. A publisher-owned
  display/refresh remains publisher work, and its failures cannot start TS fallback.
- Every TS-owned GPT destroy/redefine uses one adapter transaction. It first marks
  the exact old object and cycle retired, then calls `destroySlots([old])` and
  requires a successful return before attempting `defineSlot` for a replacement.
  A throw/false destroy leaves the old identity retired and its path/aliases
  quarantined, defines no second physical slot, and makes current or next TS work
  fail `gpt_request_failed` until publisher destruction or reload. If destroy
  succeeds but replacement definition fails, the slot stays unbound and the same
  failure is returned; bindings and ownership commit only after one replacement is
  successfully defined. A stale generation after either call disposes any newly
  created TS-owned replacement and cannot bind it. Request-timeout, completion-
  timeout recovery, navigation replacement, and DOM reconciliation all call this
  transaction rather than open-coding destroy/redefine.
- Runtime-owned DOM reconciliation detects when a framework replaces the element of
  a TS-owned live slot. It debounces changes, retires/destroys only the orphaned
  TS-owned GPT object, resolves the unique current element, and rebinds within the
  current navigation. One `MutationObserver` per `NavigationSession` watches
  `childList` changes under `document.documentElement`; it is disconnected on
  navigation disposal. Once an exact owned element becomes disconnected, that slot
  opens a 5,000 ms reconciliation window on the monotonic clock. The first resolution
  pass runs after 250 ms without another relevant mutation; if it is unresolved or
  ambiguous, exactly one final pass runs at the 5,000 ms boundary. A pass that finds
  one unique current element may win the slot's terminal reconciliation latch only
  after the destroy/redefine transaction commits its replacement. Destroy or define
  failure settles current work as `gpt_request_failed`; it is not counted as a
  successful rebind. The window expiry racing a successful final pass goes through
  the same latch: success wins only if the unique replacement was committed first;
  otherwise the slot records `slot_unresolved` and runs the failed-reconciliation
  disposer. That disposer
  cancels the slot's active TS request cycle, tombstones its live render reservation,
  compare-restores only targeting still equal to TS-installed values, clears every
  exact/alias binding to the orphan, and asks the GPT adapter to destroy that exact
  still-TS-owned object. Before the destroy call it marks the object/cycle retired in
  weak identity state, so a throw or later GPT callback is quarantined and cannot
  re-enter selection, fallback, trace attribution, or targeting. It then releases
  the timer, candidate set, and all strong references. Successful destruction
  settles a nonterminal current attempt as `failed{reason:'slot_unresolved'}`;
  throw/false destruction settles it as `failed{reason:'gpt_request_failed'}` and
  adds one bounded local warning. Neither outcome restores ownership.

  A second disconnect after one successful rebind may open one final window; two
  successful rebinds is the per-slot, per-navigation maximum. A further disconnect
  immediately runs that same disposer with
  `failed{reason:'reconciliation_capacity'}` and cannot rebind again before the next
  `NavigationSession`. Navigation disposal uses the same physical-object/targeting/
  cycle cleanup without emitting a new failure. Reconciliation never destroys a
  transferred or otherwise publisher-owned slot, and ownership transfer racing any
  pass wins the latch, cancels TS reconciliation state, removes TS destroy ownership,
  and leaves physical/targeting cleanup to the publisher.

- The Prebid refresh policy preserves the exact `excluded_gam_ad_unit_path_suffixes`
  behavior: path matching is literal/case-sensitive suffix matching; missing,
  non-string, or throwing `getAdUnitPath()` fails open; stale TS/Prebid keys are
  cleared from every target; only eligible slots enter the synthetic auction; and
  the complete target slot list plus original options still reaches GPT.
- Use one adapter-level refresh interception and one slot-service request path;
  remove the three independent integration wrappers without removing handoff or
  exclusion semantics.
- Preserve the baseline collapsed-shell resize as a guarded exception tied to the
  current attempt. It runs only after a TS PUC response is posted and only when the
  source is the exact connected iframe, width/height attributes and computed size
  are still at most one pixel, dimensions are finite/positive, the frame/wrapper are
  ordinary non-fixed/non-sticky display shells, and no anchor container is present.
  Only that iframe and its still-collapsed immediate wrapper may be resized.

### 5.8 Local diagnostics

The kernel owns a bounded, failure-isolated diagnostics bus. Render attempts and the
GPT adapter publish immutable observations after their correctness transition; a
diagnostics subscriber can never delay, reject, retry, or mutate the source
operation. Subscriber throws are logged locally and isolated from later subscribers.
The internal bus admits at most 16 integration-module subscriptions, one for each
manifest member; publisher code cannot register on that internal bus.

`tsjs.diagnostics.renderTrace` replaces the mutable `tsjs.renders`, `renderLog`, and
`renderSeq` globals and the `tsjs:adRendered` CustomEvent with read-only snapshot and
subscription methods. The final schema is:

```ts
type RenderTracePathV1 = 'auction' | 'ssat' | 'gam-refresh'
type RenderTraceServedFromV1 =
  | 'inline'
  | 'gam'
  | 'debug-adm'
  | 'pbs-cache'
  | 'prebid'

interface RenderTraceRecord {
  readonly slotId: string
  readonly path: RenderTracePathV1
  readonly rendered: boolean
  readonly elementId?: string
  readonly auctionId?: string
  readonly bidder?: string
  readonly adId?: string
  readonly bidId?: string
  readonly creativeId?: string
  readonly admHash?: string
  readonly servedFrom?: RenderTraceServedFromV1
  readonly gamEmpty?: boolean
  readonly injected?: boolean
  readonly visible?: boolean
  readonly count: number
  readonly seq: number
  readonly at: number
}

interface RenderTraceDiagnostics {
  current(): Readonly<Record<string, Readonly<RenderTraceRecord>>>
  history(): readonly Readonly<RenderTraceRecord>[]
  subscribe(listener: (record: Readonly<RenderTraceRecord>) => void): () => void
}

interface TsjsDiagnostics {
  readonly renderTrace: RenderTraceDiagnostics
  readonly gpt?: GptDiagnosticsApi
}

class DiagnosticsSubscriberLimitError extends Error {
  readonly code: 'subscriber_capacity'
  readonly surface: 'renderTrace' | 'gpt'
}
```

Snapshots are frozen copies, not references to the runtime store. Subscription is
FIFO; unsubscribe is idempotent; a listener registered during dispatch begins with
the next observation. `count` is the positive per-slot impression ordinal, `seq` is
the positive runtime-global observation ordinal, and `at` is the initial record's
`Date.now()` epoch milliseconds; enrichment retains all three. The initial record and
every later enrichment commit to `current`/`history` first and return to the
correctness/publisher stack without invoking public code. After commit, the
diagnostics service snapshots the current subscriber ids and enqueues one frozen
full-record copy in a 200-entry FIFO keyed by `seq`; another enrichment pending for
that `seq` replaces both its queued snapshot and captured subscriber-id set without
changing order. A listener added after the initial commit may therefore receive a
later enrichment commit, but never the earlier snapshot. Overflow drops the oldest
pending notification and increments a diagnostics-only counter. One owned
zero-delay task drains the queue in observation order. A listener registered after a
commit cannot receive that committed observation; unsubscribe before delivery
suppresses its captured id; registration during delivery starts with the next
observation. A slow/non-returning listener can block only that later diagnostics task,
never the render/GPT transition that scheduled it, and a throw is isolated from later
listeners. This asynchronous frozen delivery is the timing/detail replacement
contract for the removed `tsjs:adRendered` event; no CustomEvent or compatibility
alias is emitted.

Each public diagnostics surface admits at most 32 live subscribers. A 33rd
subscription throws `DiagnosticsSubscriberLimitError{code:'subscriber_capacity'}`
without adding the listener; unsubscribe immediately returns capacity. The
argument must be callable or `subscribe` throws `TypeError` before the capacity
check. The
composition root freezes `tsjs.diagnostics` only after all manifest diagnostics
integration modules have registered their surfaces. Fallback exposes no diagnostics
namespace because it constructs no runtime.

- sequence numbers are runtime-global across separately built IIFEs;
- current state is keyed by exact registered slot id and therefore capped by the
  256-record navigation registry. Slot/navigation disposal synchronously prunes its
  current entry; history is document-runtime scoped, capped at 200, and evicts the
  oldest row before append;
- one physical impression is one history row; a later bridge/GPT/visibility signal
  enriches that row in place and cannot weaken prior `rendered` or `injected` truth;
- a publisher/GAM refresh with no current TS auction has no TS attribution;
- `gam-only` means GAM reported fill without proof TS placed the creative, while
  `ok` requires TS placement plus visibility;
- DOM `data-ts-*` stamps remove absent/stale fields on every update; an old badge is
  removed before the new status is considered; and
- the boot-armed local overlay remains bounded, newest-first, click-to-export,
  and noninteractive with the creative. Overlay/export failure cannot affect ads.

Diagnostics enablement is resolved before core preparation and transported only
through frozen boot data:

```ts
interface DiagnosticsBootV1 {
  readonly version: 1
  readonly renderTraceOverlay: boolean
  readonly gpt: { readonly active: boolean }
}
```

The server always emits this complete value, defaulting to
`{version:1,renderTraceOverlay:false,gpt:{active:false}}`. Both objects must be
non-null plain objects with exactly the shown own enumerable data properties;
accessors, unknown/missing keys, or wrong prototypes/literals/types are
`abi_mismatch`, not silent diagnostics disablement. The kernel copies the validated
data and recursively freezes that copy before module preparation; copy/freeze
failure is also `abi_mismatch`. `gpt.active:true` requires exactly one required `gpt_diagnostics`
integration id in `BootManifestV1`, and `false` requires that module to be absent;
the inverse mismatch is also `abi_mismatch` before any GPT diagnostics listener or
buffer exists.

The existing render-trace server toggle resolves
`tsjs.boot.diagnostics.renderTraceOverlay`; TSJS does not read or mutate its cookie.
GPT diagnostics remains deployment-disabled by default. When configured, one exact
`ts_console=1|true` directive on an eligible GET document navigation enables the
host session and `ts_console=0|false` disables it; values are case-sensitive and
duplicate/unrecognized directives fail closed for that response. The server owns the
host-only HttpOnly session cookie, removes the reserved directive before publisher or
origin handling, preserves unrelated path/query/fragment data, and emits only the
resolved `gpt.active` boolean. The old
`window.__tsjs_gpt_diagnostics_active` flag and browser storage bootstrap are deleted.

The GPT diagnostics integration module preserves the behavioral contract in
`docs/superpowers/specs/2026-07-28-gpt-runtime-diagnostics-overlay-design.md` unless
this design explicitly changes ownership or activation transport. It consumes raw
facts from the sole GPT adapter rather than registering another control wrapper.

When `gpt.active` is true, core installs the six documented GPT observations
(`slotRequested`, `slotResponseReceived`, `slotRenderEnded`, `slotOnload`,
`impressionViewable`, and `slotVisibilityChanged`) before any TS-owned GPT request.
It starts a 512-entry FIFO pre-module fact buffer and replays it in order when the
diagnostics module activates. Overflow evicts the oldest fact and increments one
diagnostics-only counter; after replay, live facts fan out directly and the buffer is
released. When inactive, no diagnostics buffer or four diagnostics-only listeners
exist. The GPT adapter may still own `slotRequested` and `slotRenderEnded` listeners
required for ordinary ad correctness under §5.7; inactive zero-side-effect tests
measure that baseline and require zero diagnostics-added listeners, DOM, timers,
observers, API, or network work.

The GPT diagnostics store retains at most 64 observed GPT slot objects, ten request
cycles per slot, and 128 callback-issue records. It evicts the
least-recently-active slot or oldest cycle/issue before insert and increments the
corresponding export counter. An evicted GPT slot can re-enter only on a future
`slotRequested`; its monotonic request number is retained in a `WeakMap`, and earlier
non-request callbacks remain unmatched. Its public API shares the 32-subscriber cap
above. Exact slot identity/binding and element replacement, physical request cycles,
callback truth, timing fields, frozen bounded export, Shadow DOM overlay, badge
layers, SPA behavior, privacy, and non-interference remain. Diagnostic records are
memory-only; neither diagnostics surface writes localStorage, sessionStorage,
IndexedDB, or uploads data. The hard-cutover API is `tsjs.diagnostics.gpt`; the old
flag/runtime expandos and `tsjs.gptDiagnostics` alias are deleted.

GPT public subscriptions use a separate one-entry latest-snapshot notifier and never
run from a GPT callback, adapter fan-out, store mutation, or binding observer. After
each committed store/binding change, the controller builds one frozen
`GptDiagnosticsExportV1`, snapshots current subscriber ids, and schedules one owned
zero-delay task. A later change before delivery replaces that pending snapshot and
subscriber-id set; this API signals current state rather than promising one callback
per raw GPT fact. Registration after a commit cannot receive that commit unless a
later change replaces the pending snapshot; unsubscribe before delivery suppresses
the captured id. Listener throws are isolated, and a slow/non-returning listener can
block only the diagnostics task. Module disposal cancels the task, clears the one
pending snapshot and subscriber set, and delivers nothing later. `snapshot()` remains
a synchronous frozen read with no subscriber invocation. Tests apply the same
subscribe/unsubscribe/slow/throw rules as render trace plus 0/1/2-update coalescing.

### 5.9 Creative and remaining integration preservation

`CreativeBootV1` in §5.4 is exact plain boot data. The server always emits it. A
disabled integration is exactly
`{version:1,enabled:false,clickGuard:false,renderGuard:false}` and has no `creative`
manifest member. When enabled but its config is absent, the server emits
`{version:1,enabled:true,clickGuard:true,renderGuard:false}`; explicit configuration
replaces the two guard booleans. `enabled:true` requires exactly one required
`creative` manifest id and `false` requires its absence. An accessor, non-plain
prototype, missing/unknown key, wrong literal/version/type, disabled non-false guard,
or manifest mismatch is an `abi_mismatch` before any creative guard installs. The recursively
frozen `tsjs.boot.creative` is the only final inspection/configuration surface.
`globalThis.tscreative`, `globalThis.tsCreativeConfig`, `installGuards`, `setConfig`,
and `getConfig` are deleted, not aliased; changing guard policy requires a new boot/
document generation.

The creative integration module prepares inertly and activates transactionally
exactly once in the kernel barrier. Activation installs the click guard when
`clickGuard` is true and the image/iframe dynamic-source guards when `renderGuard` is
true, but performs no baseline DOM rewrite. Only when
`clickGuard || renderGuard` is true, activation gives a still-loading document one
owned `DOMContentLoaded` callback to perform the baseline idempotent rescan after the
initial DOM completes; an already interactive/complete document gets no listener and
performs that scan from one staged `afterCommit` callback. Its
disposer removes that listener, observers, and owned DOM state and compare-restores a
patched constructor/property/function only if the current value is still the exact
wrapper installed by this generation. Disabled guards install no wrapper, observer,
listener, scan, or DOM state, whether the integration itself is disabled or it is
enabled with both guard booleans false. SPA navigation retains the document-scoped
guards and their dynamic-node behavior; failed preparation/activation or full runtime
disposal removes them once.

Creative processing keeps its current independent policy controls. Auction
sanitization remains explicit opt-in/default-off; rewriting retains its existing
setting/default and still runs on every delivery path where that setting applies.
The runtime click guard resolves and stores one validated absolute HTTP(S) URL before
navigation, rejects `javascript:`, `data:`, `blob:`, malformed, and credentialed
targets, and uses the established `/first-party/proxy-rebuild` GET redirect path to
recover clicks from the opaque sandbox. Dynamic resource/click rewriting, iframe
sandbox attributes, font/CORS handling, body/base handling, and the direct/SSAT/cache
delivery boundaries remain covered by unit plus real-browser tests. This APS/TSJS
work neither enables sanitization nor broadens creative privileges.

Every other enabled TSJS integration becomes a thin transactional integration module without an
internal feature rewrite. Its complete current unit suite runs unchanged against a
pre-cutover fixture and a module-composed fixture. At minimum the parity corpus
proves:

| Integration        | Required preserved behavior                                                                                                                                            |
| ------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| DataDome           | dynamic script/preload matching and fixed first-party route rewriting preserve the upstream path                                                                       |
| Didomi             | configured proxy path becomes the absolute `didomiConfig.sdkPath` without clobbering unrelated publisher config                                                        |
| Google Tag Manager | script/preload rewriting plus Google Analytics `sendBeacon`/`fetch` rewriting preserve method, body, and unrelated traffic                                             |
| Lockr              | script guard plus bounded SDK readiness polling rewrites only the initialized Lockr API host                                                                           |
| Osano              | USP/GPP/TCF cookie mirroring retains marker ownership, timeout, readiness, retry, event, focus/visibility, clear, and non-clobber semantics                            |
| Permutive          | script guard, bounded SDK readiness, API-host rewriting, and at-most-100 normalized local segment values continue to feed auction context                              |
| Sourcepoint        | optional SDK guard and Sourcepoint-owned GPP cookie mirroring retain localStorage shapes, marker ownership, initial retry, visibility/focus updates, and safe clearing |
| Testlight          | preexisting and later callbacks bridge once into the final TSJS queue; invalid entries and one throwing callback do not block later work                               |

The shared script/beacon/DOM-insertion guards keep integration-owned matchers and
routes. A shared helper may centralize interception, but it cannot broaden one
integration's matcher, reorder another integration's startup, stack interception,
or leave a timer/listener after module disposal. Maximal-bundle tests load every
server-declared integration module in real manifest order and assert both its behavior and
exactly-once disposal, not merely successful registration.

### 5.10 Error handling and bounded state

No empty `catch` remains in the migrated kernel, adapters, or APS/GPT/Prebid paths.
Boundary failures become typed results and a concise local warning; disposer and
late-callback failures cannot escape into publisher code. Logs redact descriptors,
AAX payloads, account ids, creative URLs, auction bodies, and capability values.

Every collection has a named owner, capacity, and pruning rule. Tests exercise
capacity, TTL, duplicate registration, replay, timeout, navigation replacement, and
late continuation behavior with fake timers.

### 5.11 Decomposition targets

The implementation extracts cohesive behavior rather than mechanically splitting
by line count:

| Current area                 | Target responsibility                                                                                                        |
| ---------------------------- | ---------------------------------------------------------------------------------------------------------------------------- |
| `gpt/index.ts`               | thin integration composition over GPT adapter, slot service, initial-load policy, handoff, SPA navigation, and render bridge |
| `prebid/index.ts`            | thin integration composition over Prebid adapter, shim, refresh handler, eids, and APS registration                          |
| `core/request.ts`            | validation plus `AuctionBatch`; rendering delegated to render service                                                        |
| `core/render.ts`             | path-independent DOM helpers only; lifecycle lives in render service                                                         |
| `core/trace.ts`              | diagnostics subscriber over lifecycle/GPT observations; no mutable integration-owned trace registry                          |
| APS maps in globals          | runtime-owned bounded reservation service                                                                                    |
| duplicated `script_guard.ts` | shared factory plus integration configuration                                                                                |

Other integrations receive an integration-module wrapper and service lookup where required, but
their internal behavior is not otherwise rewritten.

### 5.12 TypeScript and performance gates

Before the coordinated runtime implementation proceeds, the TSJS direct development
toolchain is upgraded to the newest stable, mutually compatible versions supported
by the repository-pinned Node major. TypeScript advances to the newest stable release
inside the latest `typescript-eslint` parser's declared support range; an unsupported
compiler/parser pairing is not accepted merely to claim a higher version. The
external artifact dependency remains exactly `prebid.js@10.26.0`, and Node type
declarations remain on the pinned Node major. Those are explicit compatibility and
artifact-contract constraints, not permission to leave the rest of the toolchain
stale. The upgrade must pass a clean `npm ci`, a peer-clean `npm ls --all`, complete
build/lint/typecheck/tests, and exact Prebid artifact verification.

After that upgrade, the lockfile compiler is the authority. CI runs a checked-in
`typecheck` script with `strict`, `noUncheckedIndexedAccess`,
`exactOptionalPropertyTypes`, `verbatimModuleSyntax`, `noImplicitOverride`, and
`useUnknownInCatchVariables`. Production bundles contain no dynamic imports.

Before implementation, CI records deterministic gzip/Brotli baselines for minimal,
reference, and maximal integration sets; each may grow at most 5% unless separately
approved. Boot-to-first-display p90 uses a pinned Chromium version, CI runner class,
fixture, warmup count, and sample count and must stay within 10% of that pre-change
baseline. Retained heap uses Chromium CDP only, with forced-GC checkpoints after
boot, first render, refresh, and SPA navigation, and the same 10% limit. Correctness
runs independently in Chromium, Firefox, and WebKit. Correctness failures are never
waived by a performance pass.

## 6. Security and privacy

1. Renderer iframes omit `allow-same-origin`; cross-origin target `"*"` is permitted
   only when transferring a one-use port to an exact, already-checked
   `contentWindow`.
2. The initial global PUC request contains the opaque renderer reservation
   capability but no descriptor, ADM, lifecycle ticket, or nonce, and it establishes
   no success. The first compatible claim acquires the PUC source; render authority
   begins only after exact reservation/slot lookup, attributable nonempty GAM,
   current generation, source binding, and atomic consumption.
3. Lifecycle tickets and nonces are CSPRNG, one-use, TTL-bounded, never logged, and
   invalidated on supersession/navigation.
4. Exact-key message parsing prevents confused-deputy extensions. Unknown versions
   are ignored or failed closed according to whether the message claims a TS
   capability.
5. Native Prebid messages with non-TS ids continue to native listeners. Any message
   carrying a live or tombstoned TS id is suppressed before later validation.
6. The upstream APS runner URL, every creative URL, and production renderer/proxy
   routes must be HTTPS. HTTP is permitted only for loopback hermetic adapters; their
   fixed local proxy route remains covered by CSP `'self'`. The runner executes only
   through the fixed-target, anonymous-CORS Trusted Server proxy. The proxy relays the
   APS body unchanged but never stores it in source, forwards publisher credentials,
   accepts a caller-selected target, or executes a fallback. Runner-created APS-origin
   resources may use their own origin cookies under browser policy. The renderer
   document accepts no cookie as authority.
7. Script creatives remain opt-in because they materially broaden executable
   behavior. Enabling them requires a documented security review of the fixed
   renderer CSP.
8. The design adds no persistent identifier and no external event pipeline.

## 7. Verification and acceptance

### 7.1 Required test layers

| Layer                 | Required proof                                                                                                                                                                                                                                                                            |
| --------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Rust unit             | APS parsing/admission, dimensions, scripts, AAX projection, mediation provenance/order/timeouts, targeting identity, descriptor serialization, endpoint headers/body                                                                                                                      |
| `rc/july` parity      | executable `905984e62` TSJS source/test/browser inventory; every `RCJ-*` row maps to pre-cutover evidence, a final owner, focused tests, and either retained or deliberately replaced observable behavior                                                                                 |
| Cross-language corpus | every positive/adversarial descriptor has the same Rust, TS, and embedded ES5 result; stale generation fails                                                                                                                                                                              |
| TS unit               | kernel ownership, integration-module transaction/unwind, terminal fallback, sessions, registries, selection/cycle/batch/latch APIs, adapter readiness, GPT handoff/reconciliation, Prebid artifact/refresh, creative security, diagnostics, and every remaining integration parity corpus |
| Hermetic browser      | all render paths, PUC bridge, three-level APS sizing, direct iframe races, owner/port/runner behavior, fallback, SafeFrame-shaped nesting, GPT handoff/hydration, creative clicks, diagnostics, and duplicate/replay/wrong-source/stale cases                                             |
| Real-GAM test network | SSAT APS-PUC, Prebid-adapter APS-PUC, page-bids APS-PUC, direct APS, direct ADM/cache, fallback after attributable empty GAM, SRA, refresh, SPA, handoff, hydrated DOM replacement, and collapsed shell                                                                                   |
| Adapter parity        | exact renderer sandbox/CSP/header bytes plus runner-proxy routing, five-second deadline, closed response parsing, bounded relay, header filtering, and failures match on all adapters                                                                                                     |
| Regression            | non-APS Cache/ADM and notifications, pure external Prebid/native bids/EIDs/user IDs/refresh exclusions, publisher GPT/handoff/SRA/SPA, creative processing/click recovery, render trace/GPT diagnostics, and every remaining integration remain correct                                   |
| Quality               | full-package TypeScript/lint including tests/scripts/build code, ESLint boundaries, format, clippy, Rust adapter suites, Vitest, artifact integration, Playwright, deterministic bundle/performance budgets                                                                               |

### 7.2 Mandatory race matrix

Tests must cover at least:

- duplicate simultaneous `Prebid Request` for the same id;
- claim before/after attributable nonempty GAM, claim followed by empty GAM, and
  navigation/supersession at each side of that two-condition join;
- replay after consumption and after tombstone expiry boundary;
- attempt-id navigation prefix failure, ordinal uniqueness and exhaustion without an
  issued-id set; forced lifecycle-ticket/renderer-nonce collisions through the
  eighth draw; 255/256/257 live nonces and 319/320/321 ticket/tombstone entries;
  capacity versus expiry pruning; and proof that neither overflow path posts a usable
  capability;
- valid id from wrong slot/source, altered id from the expected source, and a native
  Prebid id;
- PUC registration before/after timeout, wrong source, zero/two ports, replay,
  caller abort before/after registration/insertion/document acceptance, and owner
  watchdog racing a late kernel response; every winner produces exactly one
  encodable `OwnerSettlementV1` and one PUC Promise settlement; channel loss before
  start and after insertion, settlement-post throw, and the 20-second remote cleanup
  boundary prove the owner removes only its uncommitted iframe while accepted DOM
  remains;
- renderer document success followed by runner failure/timeout;
- runner proxy stall and slow-drip across the five-second total deadline; redirect;
  absent/duplicate/malformed/mismatched/over-limit `Content-Length`;
  absent/accepted/parameterized/rejected `Content-Type`; absent/identity/listed/other
  `Content-Encoding`; declared and streamed over-limit bodies; byte-preserving
  success; stripped upstream headers; and empty, non-leaking `502 no-store` failure;
- GPT/Prebid readiness at either side of its deadline, `slotRequested` at either
  side of the request-start deadline, and `slotRenderEnded` at either side of the
  completion deadline, including late real completion of an attributable
  completion-timeout cycle and proof that future events never release an
  unattributable request-timeout quarantine;
- iframe `load`, `error`, removal, supersession, and navigation disposal ordering;
- accepted-artifact promotion racing terminal disposal, replacement of an accepted
  direct iframe, TS-owned GPT destroy/redefine, and publisher-owned GPT navigation;
- two consecutive attempts installing identical targeting strings with newer
  success, failure, and supersession; older-artifact disposal after newer promotion;
  publisher different-value and same-value `setTargeting`, per-key clear, and
  clear-all before each cleanup point;
- old-navigation completion after a new slot with the same DOM id exists, both
  before and after the replacement's completion, for TS- and publisher-owned slots;
- two concurrent `/auction` calls with partial slot overlap, reversed responses,
  one caller abort, full abort, and response timeout;
- initial boot projection versus SPA navigation-owned projection, proving boot
  remains recursively frozen; stale/duplicate/malformed page-bids responses;
  page-bids racing programmatic registration at the 255/256/257 combined-slot
  boundary; auction/provider/upstream/currency/CPM/targeting grammar and
  byte/count boundaries; canonical projection just below/at/above 8 MiB with the
  exact all-winners-to-`winner_not_renderable` reduction; and all-or-nothing
  projection/slot/targeting commit;
- `display()` under disabled initial load from TS and publisher callers;
- exact and hydration-renamed late `defineSlot` handoff, mismatch/ambiguity,
  duplicate publisher display, explicit/global initial-load-disabled refresh,
  ownership transfer, SPA disposal, and unrelated slots/options;
- DOM replacement before and after GPT request start, debounced TS-owned orphan
  reconciliation at 249/250 ms and 4,999/5,000 ms, first/final pass success,
  two-success capacity, expiry/rebind latch ordering, ambiguous replacement,
  transfer racing replacement, exact orphan destruction/quarantine, targeting
  compare-restore, cycle/reservation cleanup, throwing/false GPT destroy and failed
  replacement definition in reconciliation/request-timeout/completion-timeout/
  navigation paths, proof no second physical slot is defined, navigation disposal,
  and proof that publisher-owned slots are never destroyed;
- SRA completion ordering, duplicate `responseIdentifier`, missing completion, and
  publisher/TS overlap;
- missing/stub/late/duplicate/older/partial external Prebid artifacts, artifact
  queue-watchdog versus late module activation, ABI and release-identity boundaries,
  every manifest collection/string capacity, malformed/unsorted/duplicate members,
  sentinel-normalized versus final-byte integrity, exact 10.26.0, own data-property
  binding, same-release duplicate reuse without module re-execution, different-valid-
  release refusal without disturbing the active object, absent/accessor/inherited/
  invalid/hostile non-configurable stamp handling after the watchdog arms, publisher
  callback delivery exactly once on every conflict path, bound `pbjs`/stamp replacement,
  `PreparedTrustedBid` admission/non-publication, bidder aliases, client-side
  adapter gaps, user-ID/EID manifest gaps, replacement by a later valid artifact,
  absence of TS auction/admission/render/refresh behavior from the external artifact,
  and native publisher queue/bid
  survival;
- explicit/global Prebid refresh with normal, all-excluded, and mixed slots;
  literal path case/trailing-slash mismatch; missing/non-string/throwing
  `getAdUnitPath`; cleanup of excluded slots; original option identity; and the
  initial-TS-refresh bypass;
- reservation capacity with an unexpired oldest entry and late request for that
  entry; navigation slot capacity across repeated programmatic calls at totals
  255/256/257, mixed server/programmatic records, all-or-nothing overflow, and full
  disposal/reuse on the next navigation; immutable winner-CPM retention across a
  replaced projection before a delayed cache claim, Prebid lease promotion, ignored
  cache-response price/current targeting, and separate direct-cache expansion;
- integration-module prepare throw/reject/abort and activation throw at each
  checkpoint, late preparation continuation after fallback, and `afterCommit` throw;
  9,999/10,000/10,001 ms synchronous activation returns plus the pre/post-call and
  pre-handoff monotonic checks; nonreturning activation documented as unpreemptable;
  duplicate `afterCommit` registration and 15/16-module callback capacity;
  publisher GPT activity and script/creative DOM activity before/during/after a later
  module failure prove preparation is inert, activation cannot yield, rollback is
  same-task, and post-commit work sees only the full kernel; queued and later
  `requestAds`, callback throws, already-aborted signals, refusing late integration
  registration, and proof that no second runtime, listener, port, timer, request,
  script, wrapper, guard, or iframe survives a fallback commit;
- exact kernel/fallback `TsjsApi` own surfaces; semantic version and release-id
  equality; boot deep-copy/freeze and malformed-field safe fallback; actual-Array
  queue identity; pushes before/during/at activation and commit completion; retained ingress
  references after swap; snapshot-versus-forward exactly-once behavior; nested push
  ordering; frozen final-queue `length:0` under native/borrowed mutators, index and
  length assignment, deletion, and property definition in strict and sloppy callers;
  immediate post-load return values, `this`, non-callables, and callback throws;
- main bundle absence after server GPT projection, proving the old degraded bootstrap
  renderer is deliberately gone: no GPT definition/targeting/display/refresh occurs,
  every known slot settles with the committed fallback reason, the queue drains once,
  and a late bundle cannot revive rendering;
- explicit `requestAds` selection with exact server-projected and programmatic slot
  ids, an unknown id beside a valid sibling, GPT-path and DOM-alias collisions, and
  omitted-slot snapshot membership/order while another slot registers after
  invocation;
- programmatic `addAdUnits` single/array registration, boundary sizes/counts,
  accessors/unknown keys/media, malformed params, duplicate/colliding ids,
  all-or-nothing rollback, navigation disposal, registration after a `requestAds`
  snapshot, dimension type and 0/1/4096/4097 boundaries, 63/64/65-byte bidder names,
  direct rendering, fallback refusal, absence
  of placeholder methods, logger default/all levels, invalid-level non-mutation, and
  missing/throwing console methods;
- direct and PUC ADM initial `about:blank`, pre-assignment, intended `srcdoc`, error,
  removal, replacement, duplicate load, supersession, disposal, stale-generation,
  and deadline orderings, proving only the current intended navigation accepts; and
- render-trace record/update reordering, one-impression enrichment, weaker-signal
  non-regression, 200-entry pruning, stale attribution/DOM-field/badge removal,
  navigation pruning of `current`, 32/33 subscriber boundaries and capacity reuse,
  199/200/201 pending notification bounds, same-sequence coalescing, post-commit
  asynchronous frozen subscription detail/timing, subscribe/unsubscribe races,
  slow/throwing listeners, absence of `tsjs:adRendered`,
  hidden/gam-only/ok truth, overlay/export failure, and cross-IIFE sequence order;
- GPT diagnostics activation before/after early buffered callbacks, exact raw-event
  replay, exact `tsjs.boot.diagnostics` schema, query/session enable-disable and
  fail-closed inputs, accessor/prototype/unknown/missing/version rejection and
  manifest-activation mismatch, active six-listener versus inactive correctness-listener counts,
  511/512/513 fact-buffer bounds, 63/64/65 slots, 9/10/11 cycles, 127/128/129 issues,
  32/33 public subscribers, 0/1/2-update latest-snapshot coalescing,
  subscribe/unsubscribe/disposal races and slow/throwing listeners, slot element
  replacement, timing/frozen-export bounds, overlay disposal, inactive
  zero-diagnostics-side-effect behavior, and diagnostics failure during live ads;
- creative processing across sanitize/rewrite policy combinations and every delivery
  path; exact default/explicit `CreativeBootV1` validation; automatic immediate and
  `DOMContentLoaded` install; disabled and enabled-with-both-guards-false
  zero-side-effect behavior; idempotent rescan;
  exact-wrapper disposal; absence of mutable/install globals; opaque sandbox click
  recovery; absolute HTTP(S), credentials, malformed and non-network schemes;
  dynamic URLs; replaced elements; and redirect/browser navigation failure; and
- every remaining integration module alone and in the maximal manifest, including missing globals,
  readiness/timeouts, malformed consent/storage, matcher false positives, callback
  throws, startup failure, reverse-order disposal, and cross-integration isolation; and
- every protocol string/body limit at boundary-minus-one, boundary, and
  boundary-plus-one UTF-8 bytes, including multibyte, duplicate-key, malformed
  encoding, exact 1/4096 renderer dimension bounds across Rust/TS/ES5/cache/PUC DOM,
  and exact capability-form cases through both dispatcher and port parsers.

### 7.3 Real-GAM pass criteria

The checked-in test-network fixture and hermetic fakes use fictional ids and no
production demand. Real network ids, GAM creative configuration, and secrets are
injected by the protected CI/manual environment and never checked into this
repository.
Each required flow must demonstrate the expected DOM and lifecycle result, not an
analytics row:

- APS paths: one creative request, one bridge claim where applicable, one renderer
  iframe, one APS runner load, one APS render-completion callback, one accepted
  result, no duplicate render. The PUC owner, static renderer, and descendant creative
  each have the exact winning viewport with zero default margin, no clipping, and no
  overflow.
- Empty GAM fallback: parent settles empty/failure before exactly one child render.
- Direct ADM/cache: exact owned iframe reaches one accepted result.
- Failure fixtures: wrong id, invalid descriptor, missing claim, missing owner,
  missing document acknowledgement, and runner failure each reach the specified
  terminal reason within the specified timeout.
- After SPA replacement, no old attempt mutates the current slot or targeting.

The suite records browser console, network metadata, DOM, and GPT-event evidence as
CI artifacts. Network capture excludes APS runner and creative response bodies so a
test artifact cannot become an accidental vendor-code archive. It requires no
external analytics, billing, or experiment result.

## 8. Delivery and rollout

This work is assembled through test-only constructors while being built, then cuts
over once through the existing APS/TSJS release mechanism. No runtime flag,
old/new selector, compatibility branch, or dual protocol is introduced in any
deployable artifact.

1. **Contract first:** land descriptor corpus, lifecycle types, adapter interfaces,
   and failing tests without changing production behavior.
2. **Kernel and services:** introduce runtime/integration-module ownership, sessions, slot
   registry, auction batch, and render lifecycle behind test-only construction.
3. **Server APS path:** make admission, mediation, descriptor projection, targeting,
   and renderer route conform to the contract.
4. **Browser integrations:** migrate APS, GPT, Prebid, direct auction, fallback,
   local diagnostics, creative processing, every remaining TSJS integration, and
   bootstrap to the shared services/integration modules while their `RCJ-*` parity corpus stays
   green.
5. **Delete legacy paths:** remove expandos, duplicate bridge branches, old
   `requestAds`, legacy globals, duplicated bootstrap behavior, and unused flags
   from the release candidate.
6. **Pre-production:** pass all hermetic suites and the protected real-GAM network
   in Chromium, Firefox, and WebKit; archive its console, network, DOM, and GPT-event
   evidence with the release artifact.
7. **Binary production cutover:** deploy the verified artifact through the
   repository's normal release mechanism. This design adds no percentage router or
   canary-selection infrastructure. Hold an exclusive production deployment window,
   attest the active immutable artifact, and re-check it immediately before cutover;
   any mismatch blocks and regenerates evidence. Retain that immediately prior
   artifact and roll back the whole cutover on renderer errors, elevated request
   failures, CSP/security errors, or non-APS regressions.
8. **Post-cutover:** monitor existing operational signals for 24 hours. The deployed
   artifact already contains no temporary development selector.

Binary rollback restores Trusted Server code but cannot restore older live APS runner
bytes. If the proxied runner becomes unavailable, incompatible, or produces suspect
completion results, the emergency containment action is to disable
`[integrations.aps]`; this stops new APS admission and makes both reserved APS routes
return their local `404 no-store` response. APS remains disabled until controlled
real-browser conformance passes again.

This document does not prescribe new rollout telemetry. If existing operational
signals are insufficient for a deployment decision, that blocks rollout and is
resolved operationally or in a separate observability spec; it does not justify
adding a hidden analytics subsystem here.

## 9. Decisions and rejected alternatives

1. **Patch only the current GPT bridge:** rejected; it leaves duplicated runtime
   state, SPA races, direct-auction concurrency, and bootstrap ownership unresolved.
2. **Full TSJS rewrite:** rejected; extract the kernel and migrate behavior in tested
   slices so current contracts remain the oracle until cutover.
3. **Use `slotRenderEnded` as render success:** rejected; it observes GAM creative
   injection, not APS runner completion.
4. **Use a bid id as the only bridge credential:** rejected; ids can collide, be
   truncated, replayed, or arrive from a wrong frame.
5. **Put the lifecycle nonce in `Prebid Request`:** impossible; Universal Creative
   owns that message. The nonce is minted only after a validated claim.
6. **Trust creative-document callbacks for ADM/cache:** rejected; bidder-controlled
   markup must not hold the acceptance capability. The TS-authored owner observes
   its iframe.
7. **Evict live reservations at capacity:** rejected; a late request could fall
   through to native Prebid or claim the wrong work. Refuse new registration.
8. **Abort a shared auction fetch when one slot is superseded:** rejected; sibling
   attempts may still need the response.
9. **Keep legacy globals/API aliases:** rejected; backward compatibility is not a
   requirement and dual paths would preserve the architecture defect.
10. **Add analytics or experiment infrastructure to prove rollout:** rejected as a
    separate project. Rendering is proven by contract, browser, and real-GAM
    conformance; release uses a pre-production gate and binary artifact rollback.
11. **Check in or release a pinned APS runner:** rejected. Trusted Server owns only
    the fixed-target transport proxy and renderer contract; APS owns the live runner
    bytes.

## 10. Risks and mitigations

| Risk                                                  | Mitigation                                                                                                                                                                             |
| ----------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| PUC behavior differs from mocks                       | vendor and checksum the exact supported PUC 1.17.2 behavior, exercise its `h.sendMessage` channel, and gate on real GAM                                                                |
| Same-realm publisher code can interfere               | explicitly trust TS-authored owner code; capability checks defend unrelated frames, replays, and stale work, not arbitrary same-realm compromise                                       |
| A module activation never returns                     | activation is generated first-party code with boundary tests; elapsed returning calls fail through monotonic checks, but JavaScript cannot preempt a nonreturning same-thread function |
| Strict parsing rejects a future APS field             | descriptor is versioned; outer transport remains tolerant; add a reviewed version/corpus update rather than silently accepting new semantics                                           |
| CSP blocks a legitimate APS creative                  | three-browser real-GAM suite; script creatives remain opt-in; CSP changes are explicit security work                                                                                   |
| Hard cutover breaks stale pages                       | accepted compatibility stance; verify pre-production and retain the prior immutable artifact for binary rollback                                                                       |
| Kernel extraction changes unrelated integrations      | per-integration pre/post behavior corpus, adapter fakes, current full suites, behavioral maximal-bundle test, and exact disposal assertions                                            |
| `rc/july` moves after the design is approved          | pin `905984e62`; stop before code changes, diff all inventoried TSJS/bootstrap/browser paths, and update the ledger/tests explicitly                                                   |
| Diagnostics change ad behavior or overclaim a render  | one-way observation bus, bounded early-event replay, isolated subscribers, honest `gam-only`/`ok` rules, inactive zero-side-effect tests, and no correctness dependency                |
| Bounded registries refuse traffic under extreme churn | explicit reservation `registry_full` and slot `registry_capacity`, lifecycle pruning, capacity stress tests; never trade correctness for eviction                                      |
| GPT event attribution remains ambiguous               | fail the TS attempt deterministically and never trigger fallback from ambiguous/publisher-owned activity                                                                               |
| Late async work mutates new SPA state                 | generation checks, owned disposers, terminal latch, and adversarial reversed-order tests                                                                                               |
| Browser tests report iframe load but not APS success  | require the bound APS render-completion callback and inspect network/DOM evidence                                                                                                      |
| APS runner becomes unavailable or stops the callback  | load/rejection/silence fail the attempt; real-browser conformance blocks release and APS disablement is the emergency containment path                                                 |
| APS runner reports completion incorrectly             | accepted external trust risk; protected conformance checks DOM/network behavior, but cannot prove future mutable bytes; suspect behavior disables APS                                  |
| Existing operational signals are weak                 | do not invent telemetry in this spec; hold deployment or write a separate observability design                                                                                         |

## 11. Success criteria

The design is complete when all of the following are true:

1. The five supported render flows have explicit owners, identity rules, deadlines,
   and terminal behavior.
2. APS descriptor production and all three validators agree on the full corpus.
3. Mediation cannot detach price from provenance or attach the wrong renderer.
4. GAM receives a valid, non-truncated identity for every accepted TS renderer bid.
5. Duplicate, replayed, wrong-source, stale-navigation, and late lifecycle messages
   cannot render or settle twice.
6. Direct multi-slot auctions settle every requested slot and obey child-versus-batch
   cancellation.
7. The runtime has one slot registry, one bridge listener, one adapter instance per
   external library, and one explicit owner for every timer/listener/port/iframe.
8. Legacy expandos, duplicate bridge branches, old globals, old `requestAds`, and
   duplicated bootstrap logic are absent from the final bundle. The `pub_id` config
   alias, `/__ts/page-bids`, its JS retry, and the unversioned APS renderer path are
   absent from server routes, tests, and documentation. Every bootstrap failure
   checkpoint commits the terminal non-rendering shell, drains work exactly once,
   and cannot construct or admit a second runtime.
9. APS renderer and runner-proxy routing, security headers, bounded relay, and failure
   behavior are proven through each real adapter transport, are equivalent across all
   four adapters, and never fall through to publishers. APS runner bytes are neither
   stored in the repository nor required to be identical across different upstream
   fetches.
10. Rust, TypeScript, ESLint, Vitest, hermetic Playwright, adapter parity, and
    real-GAM conformance suites pass.
11. Non-APS Cache/ADM rendering, native Prebid handling, publisher-owned GPT, refresh,
    SRA, and SPA regression suites pass.
12. No analytics, persistence, billing, experimentation, or deployment-routing
    artifact is added by the implementation plan.
13. `requestAds` accepts only exact server-projected or transactionally registered
    programmatic slot ids, omitted selection is an immutable invocation-time
    registration-order snapshot, and ambiguous internal aliases fail closed without
    affecting valid siblings.
14. Direct and PUC ADM acceptance is possible only for the exact current frame's one
    intended `srcdoc` navigation; initial blank, replacement, removal, stale, late,
    and duplicate events cannot accept.
15. Every `RCJ-*` ledger entry maps to an executable pre-cutover fixture, one final
    owner, focused final tests, and a preserved/rebuilt/superseded disposition; the
    final manifest has no unmapped TSJS source or browser contract.
16. Late GPT handoff, hydrated/ responsive DOM replacement, Prebid partial-artifact
    recovery and refresh exclusions, creative security, render trace, GPT
    diagnostics, and every remaining TSJS integration pass their complete parity
    suites after the hard cutover.
17. PUC owner, renderer document, and descendant creative dimensions are exact and
    unclipped for the winning size, while collapsed-shell correction cannot resize an
    unrelated, anchor, fixed, sticky, disconnected, or already-expanded frame.
18. Programmatic ad units register transactionally into the navigation slot service,
    participate in deterministic direct-auction snapshots, and render through the
    same lifecycle; placeholder rendering and mutable generic runtime configuration
    are absent rather than silently retained as a second path.
19. The committed `TsjsApi` kernel/fallback surfaces, semantic version, exact
    release identity, queue, logger, immutable boot data, and diagnostics presence are
    executable contracts; creative guards auto-install from `CreativeBootV1` with no
    mutable/install global API.
20. Attempt ids require no issued-id history, and reservation/ticket/nonce registries
    refuse capacity or collision exhaustion without exposing a reusable capability.
21. The external Prebid artifact remains free of TS auction/render behavior, exposes
    only its exact own frozen 10.26.0 build stamp, and the TS-owned adapter admits a
    fully prepared bid without partial publication.

## 12. Open implementation decisions

These choices may be resolved in the implementation plan without changing the
architecture:

- exact source-file boundaries inside `kernel/`, `adapters/`, and `services/`;
- whether the canonical descriptor schema is generated from Rust metadata or a
  small neutral schema file, provided all validators share the corpus and staleness
  check;
- the repository's existing feature/config switch used only while the new path is
  under construction;
- exact operational thresholds for the existing binary deployment mechanism.

The `RCJ-*` ledger membership, behavioral dispositions, public diagnostics
namespace, Prebid artifact independence, and integration parity requirement are not
open implementation decisions.

They may not be resolved by adding compatibility shims, a second runtime, external
telemetry requirements, durable persistence, experiment routing, or a weaker
lifecycle success definition.
