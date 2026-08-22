# APS Render Fix and TSJS Resilience Architecture — Design

- **Status:** revision 43 — hard-cutover contract with a lean first-display owner,
  atomic persistent-runtime takeover, an `rc/202608` implementation base,
  retired-branch concept-gap coverage, rc behavior reconciliation, and
  merge-blocking pre-action transfer remediation
- **Date:** 2026-08-04
- **Implementation baseline:** fetched `origin/rc/202608` at
  `f0825604ec6740111e99dd8a178e3b880e7d772b` on 2026-08-18. The implementation
  branch is created directly from that commit. Existing feature history at
  `ecd78a9d11680deece4d4ec13f84be04fdae6b0d` is integrated as the second parent
  of merge commit `95b562ea820268d6f16da08863dfa9f71076e4d2`.
  `origin/main` at the same fetch was
  `2e85a1cdcfe3d23814bab5b2215dd6b096f871eb` and is already an ancestor of the
  rc baseline; it is not a competing implementation authority.
- **Final release-branch refresh:** fetched `origin/rc/202608` at
  `d4cd2cc823718d64ae73bcb068e5eab03ecd901a` on 2026-08-21. The final overlap
  audit and performance comparison use that exact tip. It already contains the
  `main` ancestry selected by the release branch, so `main` is not merged
  separately.
- **Retired-branch evidence:** the immutable historical snapshot
  `905984e62a0858c53d9f0ff6dd3a1bf190cf311d` from retired `rc/july` is only a
  finite TSJS concept-gap checklist. It is not a baseline, merge source, API
  authority, or reason to preserve retired mechanics.
- **Compatibility:** this is a coordinated hard cutover. No backward-compatible
  aliases, permissive defaults retained only for rollback, dual APIs, or N/N-1
  browser/server protocol are required.
- **Decision:** this document covers APS render correctness, the TSJS architecture
  needed to make that correctness durable, and preservation or explicit
  architectural replacement of behavior in the exact rc baseline plus each
  explicitly retained concept found by the retired-branch audit. It does not add
  an external telemetry system or release experimentation.

## 0. Scope and constraints

### 0.1 Goals

1. An accepted APS bid renders through every supported Trusted Server path:
   SSAT/GPT, the Trusted Server Prebid adapter through GAM Universal Creative,
   SPA page bids, and direct `/auction` rendering.
2. Render ownership, identity, and completion are explicit. Races, stale SPA
   work, ambiguous GPT events, duplicate creative requests, and timeouts settle
   deterministically instead of failing silently.
3. TSJS has one persistent runtime kernel, preceded only by the bounded
   first-display agent in §5.2. The agent is not a second runtime: it publishes no
   runtime API or capability broker, accepts only the immutable initial work selected
   by the server, and retires through the one atomic ownership transfer. Integration
   bundles cannot create independent copies of shared state.
4. The Rust auction result, publisher projection, TypeScript parser, Prebid
   registration, GPT targeting, Universal Creative bridge, and direct renderer
   agree on one APS descriptor and identity contract.
5. Security boundaries are testable: untrusted creative messages cannot claim a
   different slot or attempt, replay a consumed capability, or revive work from a
   prior navigation.
6. Existing non-APS rendering behavior remains correct unless this design
   explicitly replaces a shared lifecycle surface.
7. Every affected TSJS behavior in the exact rc baseline is preserved, rebuilt behind the
   new architecture, or explicitly superseded by a named and tested replacement
   contract. The retired-branch audit additionally identifies required TSJS
   concepts that are not already present in the baseline; the audit never makes retired
   mechanics authoritative. No required behavior may disappear silently merely
   because its old global, wrapper, bootstrap, or carrier is deleted.
8. A server-projected initial display pays only for one server-composed lean
   first-display artifact. The persistent runtime is neither requested nor prepared
   until that immutable batch is terminal and has received its paint opportunity.
   Optional integration behavior, diagnostics presentation, programmatic/direct
   auctions, and later navigation therefore do not delay or compete with the
   protected path. A page without eligible server-projected initial work loads the
   persistent runtime directly; a later programmatic first display remains correct
   but is not claimed to have the lean transfer profile.
9. The inline controller consumes one server-sealed canonical JSON transport rather
   than bundling the complete hostile-object validation graph. Rust remains the
   producer authority, the controller synchronously parses, checks the critical
   release/manifest/outline relationships, copies by parsing, and recursively freezes
   the result before effects, and the first-display or persistent owner performs the
   complete domain validation before using projection or product values.

### 0.2 Non-goals

- No change to analytics/telemetry schemas, durable data systems, billing,
  experimentation, or deployment routing. Those belong to separate designs.
- No DynamoDB, Tinybird, or other persistence/analytics dependency, schema, table,
  sink, credential, workflow, or operational requirement.
- No change to the APS upstream OpenRTB endpoint contract, including APS's
  deliberate absence of `nurl` and `burl`.
- No rewrite of Prebid.js itself or of the decoupled Prebid strategy.
- No refactor of unrelated integration internals. They receive only the thin
  registration/bootstrap changes required by the new TSJS runtime, plus any
  mechanical disposal or adapter injection needed to preserve their baseline
  behavior.

Existing local render tracing, GPT diagnostics, logging, counters, debug output,
and telemetry integrations remain functional through the cutover. They may move
behind the core-owned diagnostics ingress, the GPT-owned fact stream, the trace
owner's private presentation capability, or the final diagnostics namespace, but
their observable concepts and non-interference guarantees are in scope. Correctness
must not depend on a new observation reaching presentation code or an external sink.
Any new analytics contract requires a separate design.

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
- Code that is unnecessary for the initial projected render cannot be imported by
  the first-display entry. The production core is a post-paint takeover artifact,
  not parser-blocking first-display code. A later module may join only the already
  committed persistent runtime through its exact release-bound capability contract;
  it cannot construct another runtime, adapter owner, slot registry, or message
  dispatcher.

### 0.4 `rc/202608` authority and retired-branch TSJS concept audit

The initial `origin/rc/202608` implementation commit recorded above is the normative
starting point, and the final refreshed tip is the release comparison baseline. Their
source, behavior, tests, APIs, CI, dependency state, and performance shape are
authoritative at their respective checkpoints. The implementation branch has the
initial rc commit as its first parent and
the previously reviewed APS/TSJS feature history as its second parent. A conflict
resolution is not itself behavioral evidence: every overlapping rc behavior must be
identified, assigned an owner, and proved after reconciliation. Unless this design
explicitly supersedes an rc behavior, the rc behavior wins.

Before release, the worktree fetches `origin/rc/202608` again. If it advanced, the
new tip is merged, its SHA is recorded, and the same overlap audit and complete test
gates run again. `main` is not separately merged: rc already carries the main
ancestry chosen by the release branch. The PR targets `rc/202608`, not `main`.

The `rc/july` branch is retired. It must not be fetched as an implementation input,
merged, rebased, or cherry-picked into this work. The immutable historical snapshot
`905984e62a0858c53d9f0ff6dd3a1bf190cf311d` is retained only because its TSJS tree,
embedded bootstraps, tooling, and browser tests form a finite audit that can expose a
required concept absent from the rc baseline. The in-spec manifest in §0.5 proves
that this historical checklist is complete. For each ledger row, implementation
first identifies the baseline owner and tests and reuses them when they satisfy
this design. Only a concept explicitly retained by the ledger and missing or
incomplete in the baseline becomes gap work. Historical source shape, names, incidental
semantics, and unrelated retired-branch features are not requirements.

The audit is behavioral, not commit-hash ancestry. The rc baseline may contain a concept
through a squash, reimplementation, or later replacement even when a historical
commit is not its Git ancestor. Every retained ledger row starts **proof-pending**.
The implementation identifies or authors a focused contract and runs it against a
detached, otherwise untouched worktree at the recorded rc baseline SHA:

- **baseline-owned:** baseline source already has an owner and the focused contract
  passes. Existing tests are reused; a newly authored test-only proof is retained in
  the candidate without changing production behavior.
- **implementation-gap:** the focused contract runs and fails because the required
  behavior is absent or incomplete. Only that demonstrated gap becomes production
  implementation work.
- **coverage-gap:** no adequate focused contract exists yet. The next action is to
  author the test alone and rerun it against the untouched rc baseline; this is an
  intermediate blocked classification, never permission to implement or import
  historical production code.

An infrastructure/setup failure remains proof-pending rather than being relabeled as
a behavioral failure. Each final classification records the rc SHA, baseline owner
paths, exact test path/command, result, and disposition. Every row must end as
baseline-owned or implementation-gap before production edits for that row. The
historical patch is never applied merely because its original hash is absent.

The existing checked-in concept-audit classification was captured against historical
main SHA `f6a2fb85ce623bf8a574e3941e1ee349acc3412d`; its `mainSha`, `main-owned`, and
command fields remain immutable provenance only. They do not satisfy this rc gate.
The same audit fixture gains a separate schema-bumped `rcBaseline` classification
for every ledger row, bound to the exact recorded rc SHA and using
`baseline-owned | implementation-gap`. The checker prints and validates both layers,
but only complete passing rc classifications satisfy phase exit. It rejects a row
whose baseline SHA, owner path, command, result, or disposition is missing; it never
rewrites the historical main evidence or treats its old pass as proof of rc behavior.

Authority order is exact: this reviewed hard-cutover contract, then the exact rc
baseline, then the retired snapshot as non-normative discovery evidence only. A
contradiction is never resolved in favor of retired code implicitly. The
implementation plan contains no `rc/july` merge task and no release gate comparing
the candidate to `rc/july`.

The following rc changes receive explicit reconciliation because they overlap this
design or its tests. This is an adoption ledger, not permission to broaden scope:

| Baseline source                                                            | Required disposition in this design                                                                                                                                                                                                                                    |
| -------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| #1008 server-side ad-template switch and inactive HTML cache policy        | **Preserve/supersede:** keep the independent switch, page/page-bids suppression, direct `POST /auction`, and `max-age=60` inactive policy. Because this is a hard cutover, `creative_opportunities.enabled` is a required boolean rather than a compatibility default. |
| #1013 shared C2 template cache, hybrid ESI, and streaming assembly         | **Preserve:** retain the baseline implementation and tests at the publisher/HTML/TSJS seams. This design adds no cache mechanism, policy, backend, or requirement.                                                                                                     |
| #1025 and #1032 GPT diagnostics documentation and slot-size evidence       | **Preserve/rebuild:** retain requested formats, GPT-reported fill, observed outer slot size, documentation, and API/export behavior under the sole diagnostics owner defined in §5.8.                                                                                  |
| #1033 APS opaque-data containment                                          | **Rebuild:** the baseline later reverted the original patch. Reintroduce the security properties through §4.4's hard-cutover protocol, without its legacy APIs or vendored PUC fixture.                                                                                |
| #1034 GAM cohort attribution                                               | **Preserve:** retain the rc behavior and configuration through the new runtime's typed registration/disposal boundaries. This design adds no new cohort, analytics schema, or experiment.                                                                              |
| #992 DataDome exclusion and staging-bypass behavior                        | **Preserve:** retain its exact guard, privacy, and logging behavior through the DataDome integration module.                                                                                                                                                           |
| rc CLI, SSAT debug, admin, response-cache configuration, and EdgeZero work | **Exclude from active changes:** preserve the baseline code and tests where merge overlap requires it; add no APS/TSJS requirement. EdgeZero receives no feature work in this design.                                                                                  |

Each ledger entry has one of these dispositions:

- **Preserve:** retain the observable behavior and its failure semantics.
- **Rebuild:** retain the outcome but replace the old mechanism with the runtime,
  adapter, service, or integration module named here.
- **Supersede:** deliberately replace an old mechanism with a stricter named
  contract. The ledger must state the behavioral change and prove either that the
  new owner makes the old compensation unnecessary or that the new terminal
  failure is complete, bounded, and preferable to a partial second runtime.
- **Exclude:** keep the existing feature untouched because it is not TSJS work;
  this disposition cannot hide affected baseline TSJS behavior or an explicitly
  retained ledger concept.

Hard cutover authorizes removal of old mechanisms and names. It does not authorize
silent loss of baseline behavior or a retained ledger outcome. An observable
outcome may change only through an explicit **Supersede** entry that names the old
and final behavior, gives the architectural reason, and has boundary tests for the
replacement contract. A source deletion is complete only when its baseline
regression tests and ledger replacement/supersession proof pass.

| ID                | Audited TSJS concept                                                                                                                                                                                          | Disposition and final owner                                                                                                                                                                                                                                                                    |
| ----------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `RCJ-CORE-01`     | Core config/context, callback queue, auction parsing, direct request/rendering, SPA generation checks, and shared helpers continue to serve every enabled integration.                                        | **Rebuild:** kernel, services, and composition root; exact behavioral corpus runs before and after the switch.                                                                                                                                                                                 |
| `RCJ-CORE-02`     | Programmatic ad-unit registration can drive direct `/auction`; core also exposes version/queue, placeholder render helpers, mutable generic config, and the local logger.                                     | **Preserve/supersede:** §5.4 defines the exact final API; typed registration/request APIs and immutable config replace placeholders/mutable config; logger methods/default remain, while invalid levels now throw without mutation instead of being retained with warn fallback.               |
| `RCJ-BOOT-01`     | The edge-injected `gpt_bootstrap.js` duplicates initial-load tracking, slot handoff, hydration scheduling, GPT definition/targeting/display/refresh, and can render initial ads without the main TSJS bundle. | **Rebuild/supersede:** the bounded first-display agent owns only the immutable projected display and atomically transfers it to the persistent runtime; it publishes no degraded API/runtime. Missing/partial/takeover failure settles through §5.3 fallback without replay.                   |
| `RCJ-TRACE-01`    | Render tracing records one honest impression timeline, bounded history, current-slot state, DOM stamps/badges, local overlay, no stale auction attribution, and emits `tsjs:adRendered`.                      | **Rebuild/supersede:** first-display facts cross the exact data-only handoff into the core trace reducer, `tsjs.diagnostics.renderTrace`, and deferred presentation attachment; public data subscription replaces mutable globals/CustomEvent, and no integration writes trace state directly. |
| `RCJ-GPT-01`      | A TS fallback and a later publisher `defineSlot` share one physical GPT slot and one initial request; ownership transfer prevents later TS destruction.                                                       | **Rebuild:** the one-use first-display capsule transfers the exact physical object into the persistent GPT adapter/slot service, which then owns publisher handoff; no guessed reconstruction, function sentinel, duplicate wrapper, definition, or request.                                   |
| `RCJ-GPT-02`      | Responsive/hydrated slot resolution chooses the unique active placement, recovers DOM replacement, and never silently chooses an ambiguous sibling.                                                           | **Rebuild:** navigation-scoped aliases plus runtime-owned DOM binding/reconciliation.                                                                                                                                                                                                          |
| `RCJ-GPT-03`      | Native publisher GPT calls, service state, SRA, disabled initial load, refresh options, targeting cleanup, and publisher-owned slots retain their native semantics.                                           | **Preserve/Rebuild:** the sole GPT adapter owns interception and event fan-out; publisher activity never becomes TS-owned work.                                                                                                                                                                |
| `RCJ-GPT-04`      | A TS-owned PUC response may resize only its authenticated still-collapsed ordinary 1×1 GAM shell, never unrelated, anchor, fixed, sticky, or already-expanded frames.                                         | **Preserve/Rebuild:** current render attempt owns one guarded resize after a response is successfully posted.                                                                                                                                                                                  |
| `RCJ-PREBID-01`   | The publisher-specific artifact is pure Prebid.js; the Trusted Server shim is a separate TSJS integration module, and the external bundle remains independently useful if that module fails.                  | **Preserve/Rebuild:** external artifact plus Prebid adapter/integration module; TS code is not vendored into the external Prebid artifact.                                                                                                                                                     |
| `RCJ-PREBID-02`   | Missing, late, duplicate, older, or partial Prebid artifacts fail safely: publisher queues drain, TS refresh handling is not installed without a real API, and installation is idempotent.                    | **Preserve/Rebuild:** artifact watchdog plus release-matched module transaction and bounded readiness queue.                                                                                                                                                                                   |
| `RCJ-PREBID-03`   | Adapter manifests distinguish module names from registered bidder codes/aliases; client-side bidder coverage, user-ID modules, EIDs, native bids, and publisher callbacks keep working.                       | **Preserve:** typed artifact contract and black-box artifact tests; TS-owned bid identities alone are replaced.                                                                                                                                                                                |
| `RCJ-PREBID-04`   | Configured GAM-path exclusions remove only matching slots from the synthetic Prebid refresh auction while clearing stale TS keys and retaining every slot/options in the GPT refresh.                         | **Preserve/Rebuild:** one refresh policy in the Prebid integration module over the GPT adapter; global, explicit, mixed, all-excluded, and fail-open path cases remain exact.                                                                                                                  |
| `RCJ-APS-01`      | First-class APS OpenRTB admission, typed descriptor projection, direct rendering, Trusted Server Prebid-adapter rendering, and PUC rendering remain supported.                                                | **Preserve/Rebuild:** Rust admission plus the shared render lifecycle described in §§3–4.                                                                                                                                                                                                      |
| `RCJ-APS-02`      | `bid.meta`, generated Prebid `adId`, upstream bid-id fallback, and old `hb_adid` precedence carried APS identity through lossy boundaries.                                                                    | **Supersede:** the server-minted `r1_` reservation is the only TS PUC authority; native Prebid IDs and PBS Cache UUIDs remain byte-preserved for their own purposes.                                                                                                                           |
| `RCJ-APS-03`      | PUC uses one-use ports, APS callbacks—not script load—determine success, renderer tombstones are bounded, and lifecycle callbacks cannot corrupt later attempts.                                              | **Preserve/Rebuild:** bridge dispatcher, owner-control channel, reservation service, and terminal latch.                                                                                                                                                                                       |
| `RCJ-APS-04`      | The APS presentation surface and descendant creative receive the winning dimensions without default margins, scrollbars, overflow, or clipping.                                                               | **Preserve/rebuild:** exact top-mount, outer-data, inner-renderer, and creative sizing contract in §4.4 and four-level browser assertions.                                                                                                                                                     |
| `RCJ-CREATIVE-01` | Auction creative sanitization remains opt-in/default-off, rewriting retains its existing independent setting, and every delivery path observes the same configured processing boundary.                       | **Preserve:** creative integration module and server processing; this design does not silently enable sanitization or broaden rewriting.                                                                                                                                                       |
| `RCJ-CREATIVE-02` | Opaque-origin click recovery accepts only validated absolute HTTP(S) navigation, persists the validated URL, rejects non-network schemes, and keeps creative sandbox isolation.                               | **Preserve/Rebuild:** creative integration module over shared origin/DOM helpers, with unit and real-browser sandbox coverage.                                                                                                                                                                 |
| `RCJ-CREATIVE-03` | `tscreative.installGuards/setConfig/getConfig`, `tsCreativeConfig`, automatic install, click-guard default-on, and render-guard default-off control the creative browser guards.                              | **Preserve/supersede:** `CreativeBootV1` retains the defaults and the integration module auto-installs transactionally; mutable/install command globals are deleted and immutable `tsjs.boot.creative` is the only inspection/config surface.                                                  |
| `RCJ-DIAG-01`     | GPT runtime diagnostics reports raw GPT observations, exact slot binding/replacement, request cycles/timing, bounded export, overlay/badges, and no lifecycle interference.                                   | **Preserve/Rebuild:** diagnostics integration module consumes the GPT adapter event stream and exposes `tsjs.diagnostics.gpt`; it never installs a second GPT control wrapper.                                                                                                                 |
| `RCJ-INT-01`      | DataDome, Didomi, Google Tag Manager, Lockr, Osano, Permutive, Sourcepoint, and Testlight retain their current proxy guards, configuration, consent/segment, queue, and timing behavior.                      | **Preserve:** thin transactional integration modules plus complete pre/post-cutover black-box suites; internal feature behavior is otherwise unchanged.                                                                                                                                        |
| `RCJ-INT-02`      | Shared script, beacon, DOM-insertion, scheduling, origin, and async helpers retain per-integration matching and failure isolation.                                                                            | **Rebuild where shared:** helper factories with integration-owned configuration; one module failure cannot unwind another integration module or publisher code.                                                                                                                                |
| `RCJ-QUAL-01`     | Lint covers production source, tests, scripts, diagnostics, and build code; TypeScript and artifact checks cover the actual shipped combinations.                                                             | **Preserve/strengthen:** full-package lint/typecheck plus architecture, maximal-bundle, generated-artifact, browser, and retained-heap gates.                                                                                                                                                  |

The commit clusters that exposed these concepts include the render-trace series
starting at `966c8569c`; GPT recovery/handoff/responsive/native-behavior commits
`4f45974e5`, `9b1985c8b`, `340d1efb4`, `0fdd13e7d`, `ca678fe69`, and
`b200be53c`; Prebid decoupling/resilience and refresh commits `001ad385c`,
`cdff89706`, `f3dc6ba70`, `60a85e661`, and `a007bd0d0`; creative hardening
commits `1929dc83a`, `fde835110`, `9b21ba450`, `20977105f`, `1db074d4b`, and
`3d9e2b693`; GPT diagnostics `11a4a7d25`; full-package lint `941473407`; APS
admission/rendering commits from `f916ddf90` through `a08bebfbd`; and the final
PUC/sizing chain `248fe9558`, `ed38f3e13`, `905984e62`. The executable tree
inventory, not this illustrative hash list, is the completeness authority for the
retired concept checklist only. The exact rc baseline and this design remain the
behavior authorities.

### 0.5 In-spec retired concept-inventory manifest

To keep this a one-file design and make the audit runnable from an ordinary shallow
checkout, the retired concept-inventory manifest embeds the complete materialized
historical path list. No test resolves the retired commit or invokes `git ls-tree`.
A contract test extracts `retired-rcjuly-tsjs-concept-manifest-v1`, requires the
inventory to contain exactly 144 sorted unique paths, recomputes SHA-256 over their
exact UTF-8 text with one LF after every path, and matches the recorded digest. It
then requires every inventory path to match at least one `exact`, `prefix`, or
`prefixes` mapping. A path receives the union of every matching row. Every
`lib/src` path must receive at least one non-`RCJ-QUAL-01` id, and a mapping that
matches no inventory path also fails. This proves only that the audit did not omit a
historical TSJS concept; it needs no retired Git object and makes no historical
source a build or behavior input. Separate ledger evidence maps every retained row
to its rc-baseline owner/test or records a specific gap to implement.

```json retired-rcjuly-tsjs-concept-manifest-v1
{
  "version": 1,
  "authority": "concept-audit-only",
  "retiredSnapshot": "905984e62a0858c53d9f0ff6dd3a1bf190cf311d",
  "inventoryCount": 144,
  "inventorySha256": "b1e28c8b30f0b8d95e38c0f8f57394df4ad43f760ae7abf5631e2054228aef08",
  "inventory": [
    "crates/trusted-server-core/src/integrations/aps.rs",
    "crates/trusted-server-core/src/integrations/datadome.rs",
    "crates/trusted-server-core/src/integrations/datadome/protection.rs",
    "crates/trusted-server-core/src/integrations/datadome/protection_scope.rs",
    "crates/trusted-server-core/src/integrations/didomi.rs",
    "crates/trusted-server-core/src/integrations/google_tag_manager.rs",
    "crates/trusted-server-core/src/integrations/gpt.rs",
    "crates/trusted-server-core/src/integrations/gpt_bootstrap.js",
    "crates/trusted-server-core/src/integrations/gpt_diagnostics.rs",
    "crates/trusted-server-core/src/integrations/gpt_diagnostics_bootstrap.js",
    "crates/trusted-server-core/src/integrations/lockr.rs",
    "crates/trusted-server-core/src/integrations/mod.rs",
    "crates/trusted-server-core/src/integrations/osano.rs",
    "crates/trusted-server-core/src/integrations/permutive.rs",
    "crates/trusted-server-core/src/integrations/prebid.rs",
    "crates/trusted-server-core/src/integrations/sourcepoint.rs",
    "crates/trusted-server-core/src/integrations/testlight.rs",
    "crates/trusted-server-core/src/trace_cookie.rs",
    "crates/trusted-server-core/src/tsjs.rs",
    "crates/trusted-server-integration-tests/browser/helpers/gpt-stub.ts",
    "crates/trusted-server-integration-tests/browser/tests/nextjs/api-passthrough.spec.ts",
    "crates/trusted-server-integration-tests/browser/tests/nextjs/form-rewriting.spec.ts",
    "crates/trusted-server-integration-tests/browser/tests/nextjs/gpt-diagnostics.spec.ts",
    "crates/trusted-server-integration-tests/browser/tests/nextjs/navigation.spec.ts",
    "crates/trusted-server-integration-tests/browser/tests/shared/aps-renderer.spec.ts",
    "crates/trusted-server-integration-tests/browser/tests/shared/creative-sandbox.spec.ts",
    "crates/trusted-server-integration-tests/browser/tests/shared/script-bundle.spec.ts",
    "crates/trusted-server-integration-tests/browser/tests/shared/script-injection.spec.ts",
    "crates/trusted-server-integration-tests/browser/tests/wordpress/admin-injection.spec.ts",
    "crates/trusted-server-js/lib/.gitignore",
    "crates/trusted-server-js/lib/.prettierignore",
    "crates/trusted-server-js/lib/.prettierrc.json",
    "crates/trusted-server-js/lib/build-all.mjs",
    "crates/trusted-server-js/lib/build-prebid-external.mjs",
    "crates/trusted-server-js/lib/eslint.config.js",
    "crates/trusted-server-js/lib/package-lock.json",
    "crates/trusted-server-js/lib/package.json",
    "crates/trusted-server-js/lib/src/core/auction.ts",
    "crates/trusted-server-js/lib/src/core/config.ts",
    "crates/trusted-server-js/lib/src/core/context.ts",
    "crates/trusted-server-js/lib/src/core/global.d.ts",
    "crates/trusted-server-js/lib/src/core/index.ts",
    "crates/trusted-server-js/lib/src/core/log.ts",
    "crates/trusted-server-js/lib/src/core/queue.ts",
    "crates/trusted-server-js/lib/src/core/registry.ts",
    "crates/trusted-server-js/lib/src/core/render.ts",
    "crates/trusted-server-js/lib/src/core/request.ts",
    "crates/trusted-server-js/lib/src/core/styles/normalize.css",
    "crates/trusted-server-js/lib/src/core/templates/iframe.html",
    "crates/trusted-server-js/lib/src/core/trace.ts",
    "crates/trusted-server-js/lib/src/core/types.ts",
    "crates/trusted-server-js/lib/src/core/util.ts",
    "crates/trusted-server-js/lib/src/index.ts",
    "crates/trusted-server-js/lib/src/integrations/aps/render.ts",
    "crates/trusted-server-js/lib/src/integrations/creative/click.ts",
    "crates/trusted-server-js/lib/src/integrations/creative/dynamic_src_guard.ts",
    "crates/trusted-server-js/lib/src/integrations/creative/iframe.ts",
    "crates/trusted-server-js/lib/src/integrations/creative/image.ts",
    "crates/trusted-server-js/lib/src/integrations/creative/index.ts",
    "crates/trusted-server-js/lib/src/integrations/creative/proxy_sign.ts",
    "crates/trusted-server-js/lib/src/integrations/datadome/index.ts",
    "crates/trusted-server-js/lib/src/integrations/datadome/script_guard.ts",
    "crates/trusted-server-js/lib/src/integrations/didomi/index.ts",
    "crates/trusted-server-js/lib/src/integrations/google_tag_manager/index.ts",
    "crates/trusted-server-js/lib/src/integrations/google_tag_manager/script_guard.ts",
    "crates/trusted-server-js/lib/src/integrations/gpt/index.ts",
    "crates/trusted-server-js/lib/src/integrations/gpt/script_guard.ts",
    "crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/api.ts",
    "crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/badges.ts",
    "crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/binding.ts",
    "crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/index.ts",
    "crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/observer.ts",
    "crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/overlay.ts",
    "crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/store.ts",
    "crates/trusted-server-js/lib/src/integrations/lockr/index.ts",
    "crates/trusted-server-js/lib/src/integrations/lockr/script_guard.ts",
    "crates/trusted-server-js/lib/src/integrations/osano/index.ts",
    "crates/trusted-server-js/lib/src/integrations/permutive/index.ts",
    "crates/trusted-server-js/lib/src/integrations/permutive/script_guard.ts",
    "crates/trusted-server-js/lib/src/integrations/permutive/segments.ts",
    "crates/trusted-server-js/lib/src/integrations/prebid/index.ts",
    "crates/trusted-server-js/lib/src/integrations/prebid/prebid_modules/aliases.d.ts",
    "crates/trusted-server-js/lib/src/integrations/prebid/prebid_modules/liveIntentIdSystem.ts",
    "crates/trusted-server-js/lib/src/integrations/prebid/user_id_modules.json",
    "crates/trusted-server-js/lib/src/integrations/prebid/user_id_modules.ts",
    "crates/trusted-server-js/lib/src/integrations/sourcepoint/index.ts",
    "crates/trusted-server-js/lib/src/integrations/sourcepoint/script_guard.ts",
    "crates/trusted-server-js/lib/src/integrations/testlight/index.ts",
    "crates/trusted-server-js/lib/src/shared/async.ts",
    "crates/trusted-server-js/lib/src/shared/beacon_guard.ts",
    "crates/trusted-server-js/lib/src/shared/dom_insertion_dispatcher.ts",
    "crates/trusted-server-js/lib/src/shared/globals.ts",
    "crates/trusted-server-js/lib/src/shared/origin.ts",
    "crates/trusted-server-js/lib/src/shared/scheduler.ts",
    "crates/trusted-server-js/lib/src/shared/script_guard.ts",
    "crates/trusted-server-js/lib/test/build-prebid-external.test.mjs",
    "crates/trusted-server-js/lib/test/core/auction.test.ts",
    "crates/trusted-server-js/lib/test/core/config.test.ts",
    "crates/trusted-server-js/lib/test/core/context.test.ts",
    "crates/trusted-server-js/lib/test/core/index.test.ts",
    "crates/trusted-server-js/lib/test/core/registry.test.ts",
    "crates/trusted-server-js/lib/test/core/render.test.ts",
    "crates/trusted-server-js/lib/test/core/request.test.ts",
    "crates/trusted-server-js/lib/test/core/trace.test.ts",
    "crates/trusted-server-js/lib/test/fixtures/aps-renderer-v1.json",
    "crates/trusted-server-js/lib/test/integrations/aps/render.test.ts",
    "crates/trusted-server-js/lib/test/integrations/creative/click.test.ts",
    "crates/trusted-server-js/lib/test/integrations/creative/helpers.ts",
    "crates/trusted-server-js/lib/test/integrations/creative/iframe.test.ts",
    "crates/trusted-server-js/lib/test/integrations/creative/image.test.ts",
    "crates/trusted-server-js/lib/test/integrations/creative/proxy_sign.test.ts",
    "crates/trusted-server-js/lib/test/integrations/datadome/script_guard.test.ts",
    "crates/trusted-server-js/lib/test/integrations/didomi/index.test.ts",
    "crates/trusted-server-js/lib/test/integrations/google_tag_manager/script_guard.test.ts",
    "crates/trusted-server-js/lib/test/integrations/gpt/ad_init.test.ts",
    "crates/trusted-server-js/lib/test/integrations/gpt/gpt_bootstrap.test.ts",
    "crates/trusted-server-js/lib/test/integrations/gpt/index.test.ts",
    "crates/trusted-server-js/lib/test/integrations/gpt/schedule_initial_ad_init.test.ts",
    "crates/trusted-server-js/lib/test/integrations/gpt/script_guard.test.ts",
    "crates/trusted-server-js/lib/test/integrations/gpt/spa_hook.test.ts",
    "crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/api.test.ts",
    "crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/badges.test.ts",
    "crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/binding.test.ts",
    "crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/bootstrap.test.ts",
    "crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/index.test.ts",
    "crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/observer.test.ts",
    "crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/overlay.test.ts",
    "crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/store.test.ts",
    "crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/types.test.ts",
    "crates/trusted-server-js/lib/test/integrations/lockr/script_guard.test.ts",
    "crates/trusted-server-js/lib/test/integrations/osano/index.test.ts",
    "crates/trusted-server-js/lib/test/integrations/permutive/segments.test.ts",
    "crates/trusted-server-js/lib/test/integrations/prebid/index.test.ts",
    "crates/trusted-server-js/lib/test/integrations/prebid/user_id_modules.test.ts",
    "crates/trusted-server-js/lib/test/integrations/sourcepoint/index.test.ts",
    "crates/trusted-server-js/lib/test/integrations/sourcepoint/script_guard.test.ts",
    "crates/trusted-server-js/lib/test/prebid-artifact-integration.test.mjs",
    "crates/trusted-server-js/lib/test/shared/async.test.ts",
    "crates/trusted-server-js/lib/test/shared/beacon_guard.test.ts",
    "crates/trusted-server-js/lib/test/shared/dom_insertion_dispatcher.test.ts",
    "crates/trusted-server-js/lib/test/shared/scheduler.test.ts",
    "crates/trusted-server-js/lib/tsconfig.json",
    "crates/trusted-server-js/lib/vite.config.ts",
    "crates/trusted-server-js/lib/vitest.config.ts"
  ],
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

### 0.6 Approved review remediation

The initial role-correct implementation exposed material first-display transfer and
request-time work that its own post-change capture could not legitimately approve.
The first mechanical remediation reduced the `[core, render_runtime, creative,
gpt]` response to roughly 395 kB raw, but paired evidence at candidate
`8ef8a40df` still measured 2,163.6 ms p90 against a 473.3 ms historical
`main` reference: about 4.57×, where the release gate permits 1.10×. Co-bundling the
same full ownership graph saves only duplicated module bytes and cannot close that
gap. Both captures remain immutable evidence of reviewed intermediate states, not
release baselines.

Before merge, the implementation therefore replaces the oversized parser-blocking
runtime with the bounded first-display agent and atomic takeover in §5.2. It also
precomputes finite TSJS transport identities outside request handling and passes
both the byte-accounting and network-shaped browser gates in §5.12. The persistent
runtime remains architecturally complete after takeover; the load-time fix neither
deletes resilience behavior nor weakens the gate. A gate captured from an oversized
candidate cannot authorize that same candidate.

This remediation does not reopen the hard-cutover decision. `pub_id`, numeric APS
identifiers, unknown fields, old routes, and old browser APIs remain rejected rather
than aliased. It also does not create an APS runner artifact or cache design: the
runner stays live, unversioned, unvendored, unpinned, and uncached by Trusted Server.
Reserved APS browser routes remain intentionally anonymous and are dispatched before
publisher `[[handlers]]`; operators place admission control, rate limiting, or
request shielding at the platform boundary.

DataDome and the other integrations in `RCJ-INT-01` remain in scope only where their
rc-baseline TSJS implementation is affected by runtime composition or where the
retired audit identifies an explicitly retained TSJS gap. This is preservation
behind the common runtime, not permission to import unrelated retired-branch work or
redesign their server-side behavior. The server-side template switch, inactive HTML
cache policy, and C2/ESI implementation are inherited rc behavior governed by the
adoption ledger; this design adds no new cache requirement.

## 1. Problem statement and evidence

APS demand is integrated server-side, but APS creatives do not render reliably.
Four serial fixes—the `bid.meta` carrier, decoupled shim, `hb_adid` fallback, and
the historical PUC/collapsed-shell fix—each repaired one edge while leaving other
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

| Area           | Failure                                                                                                                                                                                                               |
| -------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Admission      | A configured mediator can discard direct-provider bids; scripts can be rejected by policy; strict dimensions and APS response validation can drop bids without a useful local reason.                                 |
| Identity       | PBS Cache UUID, upstream APS bid id, Prebid `adId`, GAM `hb_adid`, DOM id, and server slot id are different identities and have been conflated. GAM targeting values are capped at 40 characters.                     |
| GPT            | `display()` under disabled initial load does not request; event listeners can be installed too late; multiple refresh wrappers and concurrent requests race; SafeFrame obscures frame ancestry.                       |
| Bridge         | A `Prebid Request` can be duplicated, replayed, sent by a wrong frame, or arrive after navigation. A bare bid id is not enough to establish ownership.                                                                |
| Renderer       | The opaque sandbox cannot observe HTTP failure; descriptor validation exists in Rust, TypeScript, and embedded ES5; nested-document containment, CSP, proxy, or runner loading can fail after the outer iframe loads. |
| Direct auction | One fetch can contain several slots, but current cancellation and result handling are not batch-aware; failures collapse to an empty array.                                                                           |
| Bootstrap      | Server bootstrap and bundle initialization can both believe they own runtime setup; a hung bundle can race the no-bundle fallback.                                                                                    |
| Lifecycle      | There is no shared definition of attempt, ownership, supersession, terminal completion, or disposal.                                                                                                                  |

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
- Direct ADM: the TS-owned iframe fired its first `load` before timeout.
- PUC ADM: the TS-authored PUC owner reported its owned iframe's first
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

#### Server-side template delivery switch

When `[creative_opportunities]` exists, `enabled` is a required boolean. There is no
serde default, omitted-field compatibility path, or serialization elision. Absence
of the table means no configured server-side templates; `enabled = false` retains
the typed slot definitions for operator tooling but disables publisher-HTML and
`/_ts/page-bids` template delivery. The disabled state performs no slot matching,
automatic auction dispatch, browser projection, initial ad-slot injection, or SPA
ad initialization. Page-bids returns the exact valid empty slots/bids result. It does
not disable direct `POST /auction`, whose caller-supplied request and ordinary
auction gates remain authoritative.

The switch does not weaken configuration validation. A present table and all of its
slot/cache/assembly fields still parse, compile, and validate at startup even when
disabled; disabling delivery is not a way to retain malformed configuration. Every
checked-in example and test fixture states the boolean explicitly.

For a successful publisher `GET` HTML response where Trusted Server injects no
per-navigation auction state, the browser-facing policy becomes
`Cache-Control: max-age=60` unless the origin already supplied a directive
containing `private` or `no-store` case-insensitively. Origin validators and
surrogate/CDN cache directives are preserved on this inactive path. A response that
does receive synthesized per-navigation state retains the baseline private/no-store
and validator-stripping policy. Non-HTML responses, failures, and direct auction
responses are unchanged. This is the inherited #1008 behavior at the publisher
boundary, not a new cache implementation.

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
| bootstrap nonce         | outer-document capability        | `b1_` plus 22 base64url characters; one-use and attempt-bound  |
| renderer nonce          | inner-document capability        | `n1_` plus 22 base64url characters; one-use and attempt-bound  |
| GPT trace slot token    | diagnostic physical-object join  | adapter-minted canonical `gt1_` form; runtime-local, nonreused |
| GPT trace cycle ordinal | diagnostic impression join       | 1..2^32-1 per physical object; valid only with its slot token  |

Every TS-owned PUC source introduced by this design—APS or inline ADM—receives a renderer reservation
id, and that id is copied exactly to GAM `hb_adid`. For the Trusted Server Prebid
adapter bid, it also replaces the TS bid's generated `adId` before targeting;
native Prebid bids are untouched. The server creates the id from 16 CSPRNG bytes
encoded as unpadded base64url and prefixed with `r1_`; it retries a response-local
collision at most eight times, then fails the bid with
`identity_generation_failed`. The browser rejects a collision with any live/
tombstoned reservation as `reservation_collision`. A PBS Cache UUID and the
upstream/provider bid id retain their current transport/provenance purposes and are
never fallback credentials for an APS or ADM reservation.

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

APS document nonces use the same eight-draw CSPRNG/collision rule in two separate
live registries capped at 256 entries each. Every active APS attempt owns exactly
one `b1_` bootstrap nonce and one independent `n1_` renderer nonce. The bootstrap
nonce authenticates only the outer bootstrap/window-message/navigation phase; the
renderer nonce authenticates only the inner renderer/port-envelope phase. Their
different prefixes are mandatory, and a value is never converted, reused, or
accepted in the other role. Neither registry needs tombstones: the exact outer or
inner `WindowProxy`, exact port where applicable, attempt id, and generation remain
mandatory, and attempt disposal removes both live entries, removes the bootstrap
listener, and closes the renderer channel before any later attempt can act. Either
registry's capacity failure is `capability_registry_full`; collision exhaustion is
`identity_generation_failed`. The ticket and both nonce registries never fall back
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
The exact frozen successful claim result is also a one-shot internal capability:
`RenderAttempt` consumes that object to receive its already-bound render source and
winner context from the service's bounded internal record. The claim object exposes
neither field, so callers cannot pass, reconstruct, or swap them separately.
Consumption additionally requires the branded reservation service, its live runtime,
the original current attempt and navigation generation, and a time strictly before
the reservation's fixed expiry. Disposal, expiry, or loss of exact attempt authority
invalidates the claim capability.
The tombstone discards the render source and winner context and retains only the id,
original expiry, terminal state, and minimum suppression metadata. Neither the
renderer descriptor nor any capability crossing a browser-context boundary contains
CPM; the one-shot claim object remains internal to the same runtime.

Ids are unique across all live/tombstoned entries, so lookup identifies one entry
and then requires its exact active slot, cycle, and generation. The first compatible
PUC claim acquires its source and winner context; a live or tombstoned TS id is
suppressed before detailed validation.

Direct `/auction` APS/ADM rendering does not round-trip through a PUC reservation.
Its exact winner join creates the `RenderAttempt` with an immutable
`WinnerContext{selectedCpm}` copied from that same validated projected winner before
rendering. Thus the redesigned direct and PUC APS/ADM paths have the same CPM
authority without inventing a bridge capability for direct rendering. Existing PBS
Cache price expansion remains outside this contract and is regression-tested at its
rc-baseline behavior.

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

interface BaselinePbsCacheSourceV1 {
  type: 'pbs_cache'
  version: 1
  cacheId: string
  cacheHost: string
  cachePath: string
  width: number
  height: number
}

type OwnedRenderSourceV1 = ApsRendererV1 | AdmRenderSourceV1
type BidRenderSourceV1 = OwnedRenderSourceV1 | BaselinePbsCacheSourceV1
```

`ApsRendererV1` and `AdmRenderSourceV1` are the redesigned, reservation-owned
sources. A selected internal Rust APS/ADM bid carries exactly one corresponding enum
member; APS markup is not smuggled through `adm`, `meta`, or debug fields. Each
tagged object rejects unknown keys. Limits are defined once and shared by the Rust
producer, TypeScript parser, and embedded renderer validator:

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

For `adm`, markup is nonempty and at most 512 KiB. `BaselinePbsCacheSourceV1` is only
a hard-cutover carrier for the rc-baseline cache coordinates
occupied `hb_adid`, `hb_cache_host`, and `hb_cache_path`. It introduces no cache
policy, URL construction, response validation, price authority, dimensions,
deadline, error taxonomy, direct-cache feature, or PUC protocol. The GPT integration
delegates it to the preserved rc-baseline cache implementation behind a thin
generation/disposal boundary, and the rc-baseline black-box corpus is the authority
for its behavior. A PBS bid with accepted ADM uses the ADM source even when cache
coordinates coexist; that documents the current ADM-over-cache
precedence. `pbs_cache` is used only when accepted ADM is absent. It retains its
native cache UUID identity and never enters the `r1_` reservation registry.

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

interface BrowserProjectedBidBaseV1 {
  candidateId: string
  slot: string
  provider: string
  upstreamBidId: string
  cpm: number
  currency: 'USD'
  targeting: Record<string, string>
}

type BrowserProjectedBidV1 =
  | (BrowserProjectedBidBaseV1 & {
      rendererReservationId: string
      renderSource: OwnedRenderSourceV1
    })
  | (BrowserProjectedBidBaseV1 & {
      renderSource: BaselinePbsCacheSourceV1
    })

interface BrowserAuctionProjectionV1 {
  version: 1
  auction: AuctionDecisionSetV1
  slots: Array<{
    slot: string
    gamUnitPath: string
    divId: string
    formats: Array<readonly [number, number]>
    targeting: Record<string, string>
  }>
  bids: BrowserProjectedBidV1[]
}
```

`BrowserAuctionProjectionV1` is exact, deny-unknown, and bounded before any slot,
reservation, targeting, or bid mutation. Its canonical UTF-8 JSON is at most
`MAX_BROWSER_AUCTION_PROJECTION_BYTES = 8 * 1024 * 1024`; `auction.results`, `slots`,
and `bids` each contain at most 256 entries; and all objects are plain own-data objects
with no accessors. Canonical serialization uses the interface field order shown,
request order for results, the same order for slots, matching result order for bids,
lexically sorted targeting keys, and no insignificant whitespace. `auctionId` matches
`^[A-Za-z0-9._:-]{1,128}$`; candidate ids use
the exact 12-character base64url form from §3.4 and are unique; result slots are
unique, follow the §2.2 bound, and contain no NUL or ASCII control; every winner has
exactly one bid with the same slot/candidate and non-winners have none. An APS/ADM
bid has one `rendererReservationId` using the exact unique `r1_` form from §2.2; a
`pbs_cache` bid has no such field and retains the current cache UUID identity. Provider matches
`^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$`; and upstream bid ids are 1–64 UTF-8 bytes with
no NUL or ASCII control. CPM is a finite nonnegative number and currency is exactly
`USD`.

For initial HTML and `/_ts/page-bids`, `slots.length` equals
`auction.results.length` exactly. Entry `slots[i].slot` equals
`auction.results[i].slot`; slot ids are unique; and the entire projection is rejected
if any placement is missing, duplicated, extra, or out of order. `gamUnitPath` and
`divId` are nonempty, contain no NUL or ASCII control, and are each at most 256 UTF-8
bytes. `formats` contains 1–64 exact two-number tuples and every width and height is
an integer in 1–4096. Placement `targeting` uses the same exact key/value grammar and
32-entry cap as bid targeting and cannot contain `hb_adid`.

The direct `/auction` response does not expose this browser projection shape. Its
internal use of the canonical decision/bid serializer supplies `slots:[]` because
there is no server-rendered GAM placement to bind; the wire response remains the
exact OpenRTB response plus decision extension. Rust canonicalization therefore
accepts either full ordered placement coverage or the direct-only empty placement
vector, while the browser boot/page-bids parser accepts only full ordered coverage.
No browser consumer interprets an empty placement vector for a nonempty decision set.

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
rule, never a completion-order or first-fit subset. The aggregate measurement includes
the complete ordered placement vector for browser projections. Boot rejects any independently
malformed or oversized value as `abi_mismatch`; page-bids and direct response
admission reject it transactionally as `invalid_response` with no partial slot,
reservation, targeting, or bid state.

Wrong type, nonfinite, fractional, zero, or negative render dimensions are
`invalid_dimensions`; an otherwise integral dimension outside 1–4096 is
`dimensions_out_of_range`. This exact distinction and range apply in the Rust
producer, TypeScript projection/source parser, ES5 APS renderer validator,
programmatic banner sizes, PUC/renderer DOM construction, and every
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

For an APS/ADM source, the bid's standard `id` is the server-minted renderer
reservation id from §2.2. An rc-baseline `pbs_cache` source retains its cache-id
identity instead and never enters the reservation service. The provider's upstream
id remains provenance and, for APS, the descriptor's `bidId`. The bid's standard `impid` must equal the request impression id mapped to
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

APS configuration accepts only canonical string `account_id`. Integer account IDs,
`pub_id`-only configuration, and mixed `account_id` + `pub_id` shapes are rejected at
the hard cutover; unknown APS configuration fields are not silently ignored.

Deployment is ordered so the old serving binary receives the canonical configuration
first: replace `pub_id` with quoted string `account_id`, quote numeric identifiers,
remove legacy and unknown APS keys, run `ts config validate`, and push while the old
binary that accepts `account_id` is still active. Only then deploy the new binary.
No alias or coercion is added to make an out-of-order deployment succeed.

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
every path, plus the exact `candidateId` and, for APS/ADM only,
`rendererReservationId`. Initial HTML
stores the document-generation input at
`tsjs.boot.auctionProjection: BrowserAuctionProjectionV1`. The initial
`NavigationSession` seeds its internal current projection from that immutable boot
value; an SPA page-bids response replaces only the new session's internal projection
through the transaction in §2.5. It never mutates `tsjs.boot`. A winner decision
must join exactly one projected bid and a no-bid/failed decision must join none.
Every decision also joins its exact ordered `slots` placement. Static placement
targeting is applied first, bid targeting overrides a duplicate static key, and the
runtime alone synthesizes `hb_adid` from `rendererReservationId` for APS/ADM or the
native cache UUID for `pbs_cache`; neither server targeting object may provide that
key.
Targeting uses the APS/ADM reservation id or the rc-baseline PBS Cache UUID according to
the discriminated source and never truncates a value to fit GAM.
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

The composition resolves each projected `divId` to the exact element first, then to
one unambiguous responsive/hydrated prefix match; container-shell aliases are not
treated as creative roots and ambiguity fails `slot_unresolved`. If GPT already owns
exactly one live slot for the resolved element, the slot service adopts that publisher
object and publishes with `refresh`. Otherwise the sole GPT adapter performs a
transactional `defineSlot`/`addService`/adoption and publishes with `display`.
Staleness destroys the unadopted candidate, and no path may leave a second physical
slot. Initial boot and every successfully committed page-bids replacement use this
same publisher. `pushState`, `replaceState`, and `popstate` share pathname-plus-query
identity; identical routes are suppressed, a current failed/rejected response rolls
back to the last committed path so the same route can retry, and an older response
cannot roll back or publish over a newer navigation generation.

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

`/integrations/aps/renderer/v2` and `/integrations/aps/runner.js` are always-reserved
Trusted Server routes. When APS is enabled, `GET` returns the local static bootstrap
or proxies the APS-hosted creative runner respectively. When APS is disabled, `GET`
returns a local `404 no-store`; neither route ever falls through to a publisher
origin. The family is dispatched before publisher auth, EC, and generic integration
filters. All adapters expose the same method, routing, security-header, and failure
semantics. Unsupported methods return local `405` with `Allow: GET`; unknown
renderer versions and the abandoned `/integrations/aps/runner/v1.js` shape return a
local `404 no-store`.

Because the two routes are loaded by browser documents, they are intentionally
anonymous. A configured `[[handlers]]` pattern that matches
`/integrations/aps/*` does not apply and must never be represented as protecting the
renderer or runner. Operator-required admission control, rate limiting, and request
shielding live at the deployment platform.

The renderer v2 response is an immutable, descriptor-free materialization bootstrap
served with a long-lived immutable cache policy. It embeds the TS-authored outer and
inner document templates, the generated ES5 descriptor validator, and the fixed
same-origin runner-proxy path. It contains no attempt descriptor, concrete bid or
creative value, vendor bytes, publisher DOM authority, or pre-navigation network
operation. It is not the document that receives the bid or executes the runner. Its
TS-created iframe initially has exactly this sandbox token set, serialized in this
order:

```text
allow-forms allow-pointer-lock allow-popups allow-popups-to-escape-sandbox allow-scripts allow-top-navigation-by-user-activation
```

It omits `allow-same-origin`, `allow-top-navigation`, downloads, modals,
presentation, orientation lock, and storage-access escape. The bootstrap's exact
response CSP is:

```text
default-src 'none'; base-uri 'none'; object-src 'none'; script-src 'unsafe-inline' http: https:; connect-src http: https:; frame-src data: https:; img-src data: blob: http: https:; media-src blob: http: https:; style-src 'unsafe-inline' http: https:; font-src data: http: https:; worker-src blob: http: https:; frame-ancestors 'self'; form-action https:;
```

The browser inherits this response policy across the bootstrap's `data:`
navigation. It is therefore an intentional transport superset and has no CSP
`sandbox` directive: otherwise it would block the nested data frame, same-origin
runner proxy, inline layout, and permanent creative sandbox tokens required by the
generated documents. The descriptor-free bootstrap has no pre-configuration
network operation. After configuration, the generated outer and inner meta CSPs
intersect with this response policy and narrow every network-capable directive to
the exact Trusted Server and validated creative origins for that attempt.

The bootstrap validates its exact `b1_` fragment nonce, sends readiness only to its
actual `parent`, and accepts one canonical JSON configuration from that exact
`WindowProxy`, with an exact browser-reported HTTP(S) sender origin and no transferred
ports. It has no `document.referrer` dependency, so a publisher's stricter referrer
policy cannot disable the handshake:

```ts
{
  message: 'TS APS Bootstrap Configure'
  version: 2
  bootstrapNonce: string
  rendererNonce: string
  creativeOrigin: string
  tagType: 'iframe' | 'script'
}
```

The complete configuration is capped at 16,384 UTF-8 bytes. `creativeOrigin` must
be an exact HTTPS origin distinct from the parent, and the two nonces must have their
exact independent `b1_` and `n1_` forms. The bootstrap then substitutes only those
policy values into the checked templates, enforces the separate 65,536-byte document
caps and 196,663-byte encoded-container cap, and replaces its own location with the
fresh outer `data:text/html;charset=utf-8,` URL. It receives no descriptor and cannot
make a runner or creative request before that navigation. The response also has
exactly `Content-Type: text/html; charset=utf-8`,
`Cache-Control: public, max-age=31536000, immutable`,
`X-Content-Type-Options: nosniff`, and `Referrer-Policy: no-referrer`. It deliberately
omits `X-Frame-Options`; `frame-ancestors 'self'` is mandatory because both direct and
PUC paths mount the bootstrap from the publisher top page, never beneath GAM,
Universal Creative, or bidder content. The TS-authored container and inner renderer
remain `data:` documents rather than separately addressable HTTP artifacts and
contain no third-party bytes. The checked-in generator materializes their source into
the server-owned v2 response at build time; neither the first-display nor persistent
TSJS production graph imports the document templates or generated ES5 validator.
The abandoned renderer-v1 route is a local `404`, with no compatibility branch. Any
shipped bootstrap body/header or data-document protocol change before this release
updates the single v2 contract; after release it requires a new route/protocol
version.

When APS is enabled, final response privacy appends a separate
`Content-Security-Policy: frame-ancestors 'self'` policy after operator headers. CSP
policies intersect, so operator configuration cannot weaken it. This prevents an
APS creative from framing a publisher document beneath the opaque data container
and using a publisher-origin executable gadget to regain top-page access. It also
means an APS-enabled publisher cannot itself be embedded cross-origin; an external
embedder allowlist is a separate security design, not a compatibility exception.
The policy is independent of cookies and cacheability.

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

Runtime-harness readiness is distinct from endpoint acceptance. In particular, the
Cloudflare/Wrangler harness must not declare the service ready merely because one
worker answers an unrelated `GET` while a service-binding worker can still return a
transient startup `503`. Before the adversarial corpus begins, that harness requires
two consecutive requests to the actual renderer route using the deliberately
unsupported `PROPFIND` method to return the complete local `405` contract, including
`Allow: GET` and `Cache-Control: no-store`; any other result resets the consecutive
count. This readiness probe does not replace or relax the corpus's own independent
`PROPFIND` assertion, and production request handling remains unchanged.

No APS runner bytes, digest, vendor-version record, redistribution license, update
script, generated artifact, or offline fallback is stored in Trusted Server source or
release artifacts. The runner route is live, unversioned, unvendored, unpinned, and
uncached by Trusted Server because it represents a mutable APS-owned dependency, not
immutable TS-owned bytes. A successful relay adds no Trusted Server `Cache-Control`
requirement. Platform shielding may bound upstream exposure operationally, but it
cannot turn the runner into a repository or release artifact.

Proxying does not make the runner trusted TS code or prove that it rendered. APS
remains the runtime owner of those executable bytes and its resolve/reject semantics
are a narrow external trust dependency. The nested opaque data documents, validated
descriptor, one-shot lifecycle port, and completion deadline contain execution and
reject missing, late, or misbound signals; they cannot determine whether APS told the
truth when it invoked `resolve`. The proxy never rewrites, inspects, or repairs the
JavaScript body.

The inner data renderer accepts one port-delivered, nonce-bound descriptor plus the publisher
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
non-serializable fields. The server-owned v2 materializer encodes the exact validated HTTP(S)
Trusted Server origin and absolute `/integrations/aps/runner.js` URL into the
TS-authored inner data document before navigation. The inner CSP permits that exact
origin as its only external script source, and the renderer loads only that route. It creates the
script with `crossOrigin='anonymous'` and `referrerPolicy='no-referrer'`; it does not
set SRI because the proxy relays a live APS-owned artifact rather than immutable
TS-owned bytes. Under the APS conformance contract, the runner consumes the queued
event and promises to call `resolve` only after its asynchronous handler commits the
nested creative iframe, and to call `reject` for validation, load, or render failure.
Promise resolution sends one completed result; rejection, proxy/CORS error, runner
error, or script-load error sends one failed result over the transferred lifecycle
port. Runner `load` is intermediate progress only. The inner renderer owns no APS
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
but does not eliminate it. This design never loads the APS URL directly from the browser,
executes a stored fallback, treats script `load` as completion, or uses a reusable
global `postMessage` acknowledgement.

## 4. Render lifecycle protocol

The **ActiveRenderOwner** is one logical owner with non-overlapping agent and
persistent epochs. On an eligible initial page, the agent epoch instantiates the
§4 dispatcher, reservation/ticket/bootstrap-nonce/renderer-nonce registries, terminal latches, provisional
GPT adapter/listeners, and APS/ADM channels for only the immutable projected batch.
Every “kernel” or “runtime” action in this section is performed by that active epoch.
At §5.2.1 takeover, all attempts and ports are terminal, tombstones/counters/facts
cross in the exact handoff, live physical/committed objects cross only in the
one-use capsule, and fresh persistent dispatcher/GPT listeners become the sole
active epoch. On a no-agent page, persistent core is the first and only epoch.

The agent records the correctness-required GPT `slotRequested` and
`slotRenderEnded` facts before issuing the initial request. When diagnostics are
enabled it also records all six bounded §5.8 observations, trace tokens/cycles, and
overflow counters into the handoff; persistent diagnostics adopts and replays those
facts before live delivery. No diagnostic listener is counted twice and loss of a
diagnostic fact cannot change lifecycle authority. Any §4 wording that assigns the
initial dispatcher/channel exclusively to “core” means the current
`ActiveRenderOwner`, not necessarily the post-paint persistent artifact.

### 4.1 State machine

```text
created
  -> no_bid | waiting_for_gam_and_claim | rendering_direct | failed | cancelled
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

`no_bid` is terminal and is valid only as `created -> no_bid`: it records the exact,
successfully parsed server decision that the slot has no winner before any render path
starts. It is invalid from every later state; GAM empty, transport, timeout, parse,
descriptor, and renderer failures retain their explicit failure outcome.
`failed` and `cancelled` remain valid from `created` so an auction child can settle
when its exact server decision fails or its caller/batch/navigation cancels before a
render path begins.

Transitions are methods on `RenderAttempt`, not ad-hoc flag mutation. Each method
checks the expected state and terminal latch. Timers are created at the transition
whose deadline they enforce and are cleared by the transition that settles them.
`waiting_for_document` accepts only an APS source; `waiting_for_adm` accepts only an
ADM source. Direct and PUC APS stage only a top-page `aps_mount` artifact; PUC APS
also stages Promise/control metadata but no remote renderer node. Direct ADM stages
only a `direct_iframe` artifact, while PUC ADM stages only a `puc` artifact.
The baseline PBS Cache path remains outside this new attempt-state expansion and is
covered by §4.5 parity tests.
Construction owns an already-issued attempt scope: every rejection after scope
issuance best-effort disposes that exact scope so session indexes cannot retain a
failed construction or block a same-slot retry.

An accepted transition first atomically promotes durable DOM/targeting ownership
from the attempt into one `CommittedRenderArtifact` owned by the exact slot and
navigation. The attempt disposer removes only uncommitted resources; promotion
detaches the committed iframe, targeting snapshot, and physical-slot metadata before
the terminal latch disposes the attempt. Direct APS mounts retire on exact artifact
replacement, owned-node/binding loss, or navigation disposal. PUC APS mounts also
retire on the publisher/competing/ambiguous `slotRequested` and successful
publisher-`destroySlots` paths defined below. Every retirement shares the same
artifact exact-once latch. The physical GPT/PUC surface remains
owned by its GPT slot: for a TS-owned slot the artifact may dispose it only through
safe GPT destroy/redefine, while publisher-owned slot DOM remains publisher-controlled
and the artifact releases only TS metadata and compare-restorable targeting. Before a
slot publishes another accepted artifact it disposes the prior artifact. Navigation
disposes artifacts according to those same ownership/quarantine rules.

A committed GPT/PUC APS overlay is also bound to the exact physical GPT object and
binding epoch that produced it. The sole early `slotRequested` listener classifies
the new physical cycle before any later GPT listener runs. An exactly attributable
TS-owned replacement cycle retains the prior accepted artifact until a new TS
artifact commits, so a failed TS replacement does not blank a valid prior result. A
publisher-owned, competing, or ambiguous cycle instead synchronously retires the
prior TS artifact before that `slotRequested` callback returns: it removes only the
TS overlay, releases compare-owned host style and targeting, and clears artifact
metadata without destroying the GPT object, cancelling publisher work, or sending a
second PUC settlement. An ambiguous/competing cycle separately fails any affected
live TS attempt under §2.4. Thus native publisher refresh/display can never render
behind a stale accepted overlay.

Artifact DOM identity is guarded independently of GPT callbacks. The existing
navigation binding observer treats a disconnected/replaced host, an overlay no
longer parented by its exact host, or an overlay whose exact node was removed as
artifact retirement. Its idempotent disposer removes the owned node wherever it was
moved, compare-restores only the original host values still owned by TS, and drops
the metadata; it never traverses or removes a publisher/GPT child. The GPT adapter
also snapshots the exact targeted physical objects before a publisher-originated
`destroySlots` call and, only when that call returns success, retires their artifacts
before returning to the publisher. A throw/false result leaves a still-connected
artifact intact, while the DOM guard still cleans up anything GPT actually removed.
All these paths share the artifact's exact-once latch and race safely with TS
replacement commit, navigation disposal, and late callbacks.

Claim,
registration, owner-control, and renderer-document ports/listeners that are no
longer needed close after terminal settlement and are never promoted.
Artifact disposal is synchronous and exact-once. A disposer that throws, returns a
Promise/thenable, or otherwise violates the synchronous contract fails closed and
cannot authorize publication of a replacement or later republication of the disposed
artifact object.

### 4.2 Universal Creative claim

The supported GAM creative selects Prebid Universal Creative 1.17.2 outside the
Trusted Server source tree, never `latest` or a publisher-selectable version. No PUC
bytes, checksum, or distributable artifact is vendored into this repository. Its
cross-domain request is a JSON
string decoding to exactly
`{message:"Prebid Request",adId,adServerDomain}` and carries exactly one transferred
response port. All three values are strings; `adId` and `adServerDomain` are
nonempty. Object-form or extended payloads are rejected. Universal Creative owns
this shape, so it cannot carry a TS nonce. Hermetic unit and browser tests exercise a
locally authored contract harness limited to that public message/helper behavior;
the pre-production real-GAM conformance gate exercises the actual PUC release. The
protected page's frozen test API exposes `pucRelease`, and the gate accepts only the
exact value `1.17.2`; a missing, publisher-selected, or `latest` value blocks cutover.

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
  rendererVersion: '4'
  tsOwner: {
    version: 1
    status: 'ready'
    kind: 'aps' | 'adm'
    lifecycleTicket: string
  }
}
```

`rendererVersion:'4'` is the hard-cutover top-mount owner protocol; the old
renderer-version value is not accepted or aliased. `renderer` is the checked-in TS
dynamic-owner program, which owns only PUC registration and Promise settlement.
It is returned directly in this response and remains self-contained: it does not
fetch, import, inject, or evaluate another TSJS asset. Its implementation may be
mechanically compacted and may share a smaller internal parser, but exact v4
messages, source/port binding, watchdogs, ADM insertion, APS informational control,
Promise settlement, and failure behavior are unchanged.
`lifecycleTicket` is
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

Direct and PUC APS use one top-page mount service owned by the current
`ActiveRenderOwner`. The PUC dynamic owner never creates, contains, or navigates an
APS iframe; it retains only its PUC Promise, registration disposer, watchdog, and
owner-control port. After PUC registration the kernel posts this informational
control message with no descriptor, URL, nonce, or transferred port:

```ts
{
  message: 'TS APS Top Mount Started'
  version: 1
  lifecycleTicket: string
}
```

The PUC claim's authenticated `WindowProxy` authorizes only the reservation claim
and later owner messages; it is never mapped back to an iframe element and is never
used to locate DOM. The reservation already joins one exact `SlotRecord`, physical
GPT object, active GPT cycle, and binding epoch. For PUC, the mount service asks the
slot service for that record's one connected, uniquely bound top-page slot element,
then revalidates the exact record, physical object, cycle, generation, element, and
binding epoch before insertion and again before commit. It appends one TS-owned
overlay child inside that element. For direct rendering it appends one ordinary
child inside the exact registered caller container. An absent, disconnected,
ambiguous, changed, or multiply claimed binding fails `slot_unresolved` before
publication; registration order and the PUC frame tree are never fallback locators.

The PUC overlay uses compare-restorable top-page style ownership: when computed
`position` is `static`, it sets the host's inline `position:relative` only while the
artifact owns that exact prior inline value. The outer mount iframe is the absolute,
initially `visibility:hidden` child with `inset:0`, `z-index:2147483647`, the exact
winning pixel width/height, and zero margin/border/scrolling/overflow. Document
acceptance and runner load leave it hidden; only the valid `TS APS Render Completed`
terminal transition changes its own visibility and publishes it above the existing
GPT/PUC content. It never hides, removes, reparents, or
traverses the GAM, PUC, SafeFrame, or bidder-controlled surface; that surface stays
connected and inert behind the TS-owned overlay while the dynamic owner retains only
Promise settlement authority. Direct rendering does not acquire overlay styling.
Failure removes only the pending TS child, compare-restores only style values still
owned by the attempt, and rejects the PUC Promise. Accepted style/DOM ownership is
promoted to the `CommittedRenderArtifact` and follows the complete §4.1 exact-once
retirement contract: replacement, publisher/competing/ambiguous cycle, successful
publisher destruction, DOM-integrity loss, or navigation disposal as applicable.

The mount has three document phases:

1. Create one iframe at the immutable renderer-v2 bootstrap URL with the attempt's
   `b1_` bootstrap nonce in its fragment and the initial sandbox from §3.6. The
   bootstrap is publisher-origin by URL but opaque because `allow-same-origin` is
   absent. It can only send exact
   `{message:'TS APS Bootstrap Ready',version:1,bootstrapNonce}` to its checked
   parent.
2. After exact element, parent, `src`, `WindowProxy`, generation, and bootstrap-nonce
   checks,
   add `allow-same-origin` to the sandbox and post exact
   `{message:'TS APS Bootstrap Configure',version:2,bootstrapNonce,rendererNonce,creativeOrigin,tagType}`.
   No descriptor or generated document URL crosses this channel. The bootstrap
   verifies the exact parent, configuration, nonces, creative origin, and tag type,
   materializes the attempt-owned outer and inner data documents, and performs
   `location.replace`. The previously loaded bootstrap remains opaque while it
   performs that navigation; the destination is naturally opaque because it is
   `data:`.
3. The materialized outer data container creates one inner data renderer whose `data:` URL
   fragment is the independent `n1_` renderer nonce and whose permanent sandbox
   contains the §3.6 tokens plus
   `allow-same-origin`. Both final documents remain naturally opaque. After the
   exact inner `WindowProxy` sends
   `{message:'TS APS Inner Ready',version:1,rendererNonce}` to the outer container,
   the container creates a `MessageChannel`, transfers `port1` to that exact inner
   window in `{message:'TS APS Inner Bind',version:1,rendererNonce}`, and transfers
   `port2` to the top page in exact
   `{message:'TS APS Container Ready',version:1,bootstrapNonce,rendererNonce}`. The
   top page accepts that message only from the exact outer `WindowProxy`, for its
   live `b1_` and `n1_` entries, with exactly one port, then retains `port2` and sends
   the descriptor envelope once over it. Neither the bootstrap nor outer container
   receives the descriptor. The inner accepts only `port1` from its exact outer
   parent and only once.

The descriptor envelope sent over retained `port2` is the exact structured-clone
object below. `publisherOrigin` is the same canonical trusted-document HTTP(S)
origin captured before mount creation, and `nonce` is the attempt's live `n1_`
renderer nonce. The inner document validates all four own data properties and the
complete §3.1 descriptor before acknowledging; it accepts no message discriminator,
reservation id, attempt id, lifecycle ticket, CPM, callback, or extra field.

```ts
interface ApsDocumentEnvelopeV1 {
  readonly version: 1
  readonly nonce: string
  readonly publisherOrigin: string
  readonly renderer: Readonly<ApsRendererV1>
}
```

After bootstrap readiness, both the outer mount iframe and its inner renderer iframe
use this exact permanent sandbox order:

```text
allow-forms allow-pointer-lock allow-popups allow-popups-to-escape-sandbox allow-same-origin allow-scripts allow-top-navigation-by-user-activation
```

`<TS_ORIGIN>` is the canonical ASCII HTTP(S) origin of the bootstrap URL and
`<CREATIVE_ORIGIN>` is the canonical ASCII HTTPS origin of the validated creative
URL. They contain no path, query, fragment, credentials, whitespace, quote, semicolon,
or control character. For `tagType:'iframe'`, the outer data container carries this
exact meta-CSP after sentinel substitution:

```text
default-src 'none'; base-uri 'none'; object-src 'none'; script-src 'unsafe-inline' <TS_ORIGIN>; connect-src https: <TS_ORIGIN>; frame-src data: <CREATIVE_ORIGIN>; img-src https: data: blob:; media-src https: blob:; style-src 'unsafe-inline' https:; font-src https: data:; worker-src https: blob:; form-action https:;
```

Its inner data renderer carries this intersecting exact meta-CSP:

```text
default-src 'none'; base-uri 'none'; object-src 'none'; script-src 'unsafe-inline' <TS_ORIGIN>; connect-src https: <TS_ORIGIN>; frame-src <CREATIVE_ORIGIN>; img-src https: data: blob:; media-src https: blob:; style-src 'unsafe-inline' https:; font-src https: data:; worker-src https: blob:; form-action https:;
```

For the explicitly enabled `tagType:'script'` case, the generator instead uses the
following two exact policies. The only difference is the additional validated
`<CREATIVE_ORIGIN>` in `script-src` at both inheritance levels:

```text
default-src 'none'; base-uri 'none'; object-src 'none'; script-src 'unsafe-inline' <TS_ORIGIN> <CREATIVE_ORIGIN>; connect-src https: <TS_ORIGIN>; frame-src data: <CREATIVE_ORIGIN>; img-src https: data: blob:; media-src https: blob:; style-src 'unsafe-inline' https:; font-src https: data:; worker-src https: blob:; form-action https:;
```

```text
default-src 'none'; base-uri 'none'; object-src 'none'; script-src 'unsafe-inline' <TS_ORIGIN> <CREATIVE_ORIGIN>; connect-src https: <TS_ORIGIN>; frame-src <CREATIVE_ORIGIN>; img-src https: data: blob:; media-src https: blob:; style-src 'unsafe-inline' https:; font-src https: data:; worker-src https: blob:; form-action https:;
```

The descriptor's already validated `tagType` selects exactly one outer/inner pair
before template generation or DOM mutation. The generator cannot emit the union for
an iframe creative, omit `<CREATIVE_ORIGIN>` for a script creative, or accept a
redirect/final script origin outside that exact source. Script bytes execute only in
the naturally opaque inner data document; no creative script is inserted into the
outer container or publisher realm.

The selected outer policy must permit the inner document's runner/resource contract
because it is inherited by a `data:` child; the selected inner policy narrows
creative-frame authority and, for iframe creatives, external script authority again.
Neither policy permits publisher origin as a frame
target unless it is also the validated creative origin, which descriptor validation
forbids. A loopback HTTP `<TS_ORIGIN>` is named explicitly for hermetic tests and does
not broaden `https:` production sources.

The generated outer-container document contains only the exact validated creative
origin, exact Trusted Server origin, the `b1_` bootstrap nonce, the independent
`n1_` renderer nonce, the permanent sandbox string,
and the percent-encoded TS-authored inner renderer template. It contains no response
envelope, bid id, creative path, price, reservation, attempt id, or lifecycle ticket.
Its CSP permits child frames only from `data:` and the exact creative origin. The
inner document's intersecting CSP permits inline TS bootstrap code, the exact
Trusted Server runner-proxy origin, and the resource categories required below the
creative; it never permits publisher origin as a frame target. The outer policy
therefore blocks the inner frame from replacing itself with a publisher document,
while the independent publisher `frame-ancestors 'self'` policy blocks a
network-loaded creative from framing the publisher beneath itself.

`MAX_APS_CONTAINER_DOCUMENT_BYTES` and `MAX_APS_INNER_DOCUMENT_BYTES` are each
65,536 UTF-8 bytes before percent encoding. Template substitution must replace each
sentinel exactly once, leave no sentinel, escape values by context, and pass the cap
before any DOM mutation. The exact Trusted Server origin is HTTP only for a loopback
hermetic harness and HTTPS otherwise; the creative origin is always HTTPS and must
equal the already validated descriptor origin. Any generation, cap, URL, CSP, nonce,
or substitution failure is `descriptor_invalid`.

The publisher realm remains trusted for top-page DOM availability because TSJS
executes in that realm; publisher code can always remove or sabotage TSJS-owned DOM.
No GAM, PUC, SafeFrame, bidder, or creative realm is trusted as an APS embedding
ancestor. The exact top-page slot-host mount and nested-data CSP remove those
third-party ancestor navigation paths from the authority chain. A separately
operated renderer origin is not required by this design.

The inner renderer sends only these exact document-port messages:

- `{message:"TS APS Document Accepted",version:1,nonce}` after the envelope's exact
  `n1_` renderer nonce and descriptor validate;
- `{message:"TS APS Runner Loaded",version:1,nonce}` with that renderer nonce when
  the runner script loads, as nonterminal progress;
- `{message:"TS APS Render Completed",version:1,nonce}` with that renderer nonce
  when the queued APS render event invokes its one-shot success callback;
- `{message:"TS APS Render Failed",version:1,nonce,reason}` with that renderer nonce,
  where `reason` is
  `descriptor_invalid | runner_no_load | runner_failed`.

Insertion has a one-second deadline. Bootstrap readiness, configuration/materialization navigation,
container-channel creation, descriptor transfer, and inner document acceptance share
one three-second deadline from outer iframe insertion. The kernel is the sole owner
of the ten-second APS-completion deadline beginning at inner document acceptance.
Callback silence at that deadline maps to `runner_failed`; no document starts a competing
completion timer. Script load never accepts. Failure, timeout, port error,
supersession, or navigation disposal settles once. On a direct path the kernel
removes its exact pending iframe. On a PUC path the iframe is top-page DOM owned only
by the mount service; it removes that exact pending node and posts exactly
one owner-control settlement:

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

The top-page mount service owns the APS node, its DOM handlers, bootstrap listener,
document port, and timers. An accepted settlement promotes that exact iframe into
the slot's `CommittedRenderArtifact`; a failed or cancelled settlement removes it.
The promoted PUC overlay then follows §4.1's exact publisher-cycle, successful
publisher-destroy, DOM-integrity, replacement, and navigation retirement rules; PUC
Promise settlement is already terminal and is never repeated during retirement.
The PUC owner owns no APS node: it closes its control port and resolves or rejects
the renderer Promise exactly once so PUC emits its ordinary completion/failure
event. The same `render_owner_initial` dynamic program's PUC ADM branch remains
governed by §4.5 and contains no APS DOM authority.

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
the terminal latch, the kernel closes its renderer-document channel, removes the
top-page mount, and sends the exact cancelled/`caller_aborted` settlement. The PUC
owner performs its Promise/control cleanup just specified. A
later insert, load, document message, APS callback, settlement, or watchdog is inert.

The winning descriptor dimensions are also the exact layout contract across the
top-page mount iframe, outer data container, inner data renderer, and runner-created
creative. The mount iframe is a block with matching positive width/height attributes
and CSS pixels, zero border/margin, and no scrollbars. Both data documents set their
root/body and sole child iframe to full width/height with zero margin/padding/border
and hidden overflow. A 300×250 winner therefore has a 300×250 outer mount box,
300×250 `clientWidth`, `clientHeight`, `scrollWidth`, and `scrollHeight` in both data
documents, and a 300×250 descendant viewport, with no default body margin, clipping,
or overflow. Equivalent assertions run for every boundary fixture dimension. This
is layout correctness, not render completion; the callback contract above remains
the acceptance authority.

### 4.5 ADM ownership and cache non-regression

Direct and PUC ADM use one TS-authored iframe constructor and this exact ordered
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

For PUC ADM, the trusted TS-authored owner—not bidder creative code—uses the
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

PBS Cache is not routed through this new owner protocol. The GPT integration retains
the rc-baseline cache request/parse/macro/PUC-response/collapsed-resize behavior
behind the runtime's generation check and disposer only. Cache success/failure,
identity, price selection, response shapes, and timing are not redefined here. The
black-box parity corpus runs the exact rc-baseline fixtures before and after cutover and
fails any observable change; APS/ADM reservation ids are never accepted as cache
UUIDs and cache UUIDs never claim APS/ADM work.

### 4.6 Channel ownership and parsing

| Channel                  | Creator                                | Retained endpoint             | Transferred endpoint                     | Lifetime                                          |
| ------------------------ | -------------------------------------- | ----------------------------- | ---------------------------------------- | ------------------------------------------------- |
| outer PUC response       | PUC `prebidMessenger`                  | original PUC frame            | kernel global listener                   | one ready/refused response or claim disposal      |
| registration response    | PUC `h.sendMessage`                    | original PUC helper           | kernel global listener                   | one registered/refused response or owner watchdog |
| owner control            | kernel after registration              | kernel attempt                | hidden TS dynamic owner                  | insertion through final owner settlement          |
| bootstrap window channel | bootstrap/outer documents via `parent` | exact top-page mount listener | no `MessagePort`; source-bound messages  | bootstrap readiness through container readiness   |
| renderer document        | outer data container after inner ready | top-page kernel retains port2 | exact inner data renderer receives port1 | document acceptance through APS completion        |

Global window messages are JSON strings with the exact keys specified above. Port
payloads are structured-clone objects with the exact keys specified above. Every
parser rejects accessors, wrong prototypes, unknown keys, wrong literal/version,
wrong port counts, oversized strings, and already-consumed capabilities before
performing a state transition. The disposer clears handlers, closes both locally
owned ports where possible, and makes queued callbacks generation-inert.

The shared protocol corpus fixes these bounds and encodings:

- before `JSON.parse`, an inbound top-page global-dispatcher string is at most 4,096
  UTF-8 bytes; a larger value is unrecognizable and causes no property access or
  state lookup. The distinct top-page-to-bootstrap configuration parser accepts only
  its exact expected parent, exact `b1_`/`n1_` nonces, exact HTTPS creative origin,
  and `iframe | script` tag type, and caps the complete canonical JSON instruction at
  16,384 UTF-8 bytes. The bootstrap-created outer `containerUrl` never crosses that
  channel; it is capped internally at 196,663 UTF-8 bytes (29-byte data prefix +
  worst-case 3 × 65,536-byte encoded document + 26-byte `#b1_…` fragment). No other
  global message receives a larger allowance;
- a TS `adId` is exactly the 25-character `r1_` reservation form; a lifecycle ticket,
  bootstrap nonce, renderer nonce, and attempt id are exactly the respective
  25-character `t1_`, `b1_`, `n1_`, and `a1_` forms from §2.2;
- `adServerDomain` is nonempty and at most 2,048 UTF-8 bytes. It is retained only for
  exact PUC-shape conformance and is never a fetch target or authority;
- `publisherOrigin` is at most 2,048 UTF-8 bytes and must serialize an exact HTTP(S)
  origin with no path/query/fragment. The mount service locally derives the absolute
  `/integrations/aps/renderer/v2` URL from that trusted origin, verifies no query or
  fragment, and appends the exact `b1_` bootstrap-nonce fragment. `rendererUrl` is
  never serialized in a v4 window or port message. The generated inner `data:` URL
  independently appends its exact `n1_` renderer-nonce fragment;
- a navigation/refresh generation is a nonnegative safe integer; and
- the generated self-contained dynamic-owner `renderer` program is at most 64 KiB
  UTF-8 and the
  complete successful outer-response JSON is at most 72 KiB. Build tests enforce
  both; refusal responses contain no renderer. The program has no external loader,
  URL, cache dependency, or second pre-paint artifact.

Across the exact global-message and structured-clone port corpora, fields use their
own carrier's exact shape and these field-level limits: APS descriptor 256 KiB
decoded AAX; ADM 512 KiB; `creativeUrl` 4,096 UTF-8 bytes;
`publisherOrigin` and `adServerDomain` 2,048 UTF-8 bytes; server slot id 1–256 UTF-8 bytes with no NUL or
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
kernel/        boot, phase registry, queue, sessions, disposal, logging
adapters/      googletag, prebid, messaging
services/      slots, auction batches, render lifecycle, consent
integrations/  gpt, prebid, aps, creative, and existing publisher integrations
composition/   small production core plus test-only composition seams
```

- Kernel imports no adapter, service, or integration.
- Adapters import kernel contracts only and are the sole readers/writers of GPT,
  Prebid, and cross-window messaging globals.
- Services import kernel and adapter interfaces.
- Integrations compose services and never import another integration.
- The production core entry imports only the kernel, immutable boot/projection
  contracts, queue/logger, and the minimum direct-auction functions that the public
  API requires before any integration exists. It does not import every adapter,
  service, integration runtime, diagnostics UI, no-op implementation, or test hook.
- Each provider IIFE contains only the concrete adapter/service implementation it
  owns; a consumer slice does not inline that implementation again. Every module
  registers one inert factory. The kernel constructs one `RuntimeSession`, invokes
  those factories transactionally, and retains their exact frozen interfaces in a
  closure-private capability broker. An integration consumes only capabilities that
  the embedded release catalog allows for that integration; it never imports another
  integration or reads a public service locator.
- The capability broker admits one provider per exact key, rejects undeclared keys,
  validates frozen exact own-data-property interface objects with no accessors,
  unknown keys, or custom prototype, removes a provider on disposal, and
  is never exposed on `window.tsjs` or `_internal`. A takeover provider must exist
  as a staged result of its provider's preparation before any takeover consumer
  prepares, and provider-before-consumer order is enforced by the catalog. A
  deferred consumer may bind only an already committed takeover provider.
- Production and test composition entries are separate. Production output cannot
  retain `*ForTest` accessors, injectable no-op adapters, corpus helpers, or fake
  scheduler branches merely because tests need them.
- Layering is enforced by ESLint restricted paths.
- A custom scope-aware lint rule rejects GPT/Prebid global access outside adapters,
  including same-file aliases of `window`, `globalThis`, `self`, `googletag`, or
  `pbjs`.

### 5.2 One runtime across IIFE bundles

#### 5.2.1 Two non-overlapping ownership epochs

The browser lifecycle has two ordered ownership epochs, never two independently
live runtimes:

```text
bootstrap installing
  -> first-display agent -> transferring -> persistent runtime
  |                     \-> failed -----------------> fallback
  \-> persistent runtime (no eligible server-projected batch)
  \-> failed ---------------------------------------> fallback
```

The **first-display agent** is a release-owned provisional owner, not the legacy
`gpt_bootstrap.js` and not a reduced public runtime. It is one parser-blocking,
server-composed artifact containing only the fixed slices selected for the
immutable server projection and parser-time obligations on that page. It may:

- validate the boot manifest and initial projection;
- install the one provisional GPT/message/guard interception needed before the
  initial action;
- define, target, request, and settle only the server-projected initial GPT batch;
- render an APS or ADM winner through the exact §4 protocols, including an
  attributable empty-GAM fallback for that batch; and
- record the first-action, terminal, and protected-paint marks.

It cannot publish `TsjsApi`, expose a capability broker, accept programmatic ad
units or direct-auction calls, process SPA/page-bids work outside the immutable
initial batch, load deferred modules, present diagnostics, refresh an accepted ad,
or start later navigation/reconciliation. Publisher callbacks remain in the one
bootstrap-owned ingress Array until persistent-runtime or fallback commit. The old
bootstrap renderer, legacy API, and any second fallback runtime remain deleted.

The server selects the agent only when every member of the projected initial batch
is representable by the first-display slices: no bid, GPT-mediated ADM, or
GPT-mediated APS/PUC (including its attributable empty-GAM fallback). PBS Cache,
programmatic/direct `/auction`, an unknown source, or any initial behavior outside
that closed set selects direct persistent boot instead; this design adds no new cache
implementation or cache-path phase split. A page with no projected initial batch also
uses direct persistent boot. Parser-time obligations join the agent only when that
agent is already selected; otherwise their takeover modules activate in the one
parser-blocking runtime artifact before upstream publisher activity. This is a
server-owned manifest choice derived from frozen
configuration and projection, not a publisher switch, experiment, or compatibility
path. A programmatic `requestAds` invoked on such a page becomes usable only after
the persistent runtime commits. The protected load-time claim applies to the
server-projected agent path; subsequent programmatic work remains correct but is not
relabeled as that measurement.

The release build independently enumerates all 3,584 reachable masks: the base is
always present, GPT may be absent for a closed no-bid batch, APS or Prebid
participation requires GPT, and the shared render owner is present exactly when the
batch contains an ADM or APS reservation. It measures and hashes every reachable composition with
the frozen raw/gzip/Brotli algorithms, then emits the exact ordered subset satisfying
all three first-display ceilings as `permittedFirstDisplayMasks`. Server selection
must be both semantically eligible and present in that generated allowlist. A closed
but size-unadmitted configuration takes direct persistent boot; it is never served an
oversized agent and the request path never calculates a new size or relaxes a limit.
The minimal, exact five-slice reference, and exact APS-plus-default-creative masks are
mandatory: failure to admit any of those three fails the release build rather than
routing the named performance cases around the gate.

The agent artifact has one base entry plus only these build-catalogued slices. A
slice absent from this table cannot enter the artifact. “Initial” means the exact
immutable batch; it does not authorize later work from the same product:

The conditional `render_owner_initial` slice owns the source-neutral attempt
collection, terminal latch, reservation/ticket registry, v4 PUC claim and owner-
control protocol, self-contained dynamic-owner program, committed-artifact journal,
and handoff/retirement primitives shared by initial PUC ADM and APS. It is selected
for an ADM or APS reservation and is absent from a no-bid/nonrendering mask; ADM-only
therefore has the same v4 owner without importing APS. `aps_initial` requires that
slice, composes its frozen release-private interface, and remains the sole owner of
APS descriptors, bootstrap and renderer nonce roles, top-page mount, and renderer-
document protocol. The only APS-specific literal or branch permitted in
`render_owner_initial` is the exact informational `TS APS Top Mount Started` control
message received after the APS kernel/top mount has started; it carries no descriptor,
URL, nonce, port, or DOM authority. No other APS message, descriptor parser, nonce
registry, renderer-document parser, mount implementation, or top-mount policy enters
that slice, and neither slice imports or instantiates a second complete render bridge.

| Slice id                     | Include iff                                                                        | Bounded obligation before transfer                                                                |
| ---------------------------- | ---------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------- |
| `first_display`              | an eligible initial batch exists                                                   | manifest/projection validation, provisional lifetime, timing, queue ingress, transfer coordinator |
| `render_owner_initial`       | the initial GPT batch contains an ADM or APS reservation                           | source-neutral reservation/ticket, v4 PUC owner, render journal, and handoff/retirement contract  |
| `aps_initial`                | the initial GPT batch can contain APS                                              | thin APS-specific descriptor, nonce/top-mount, and renderer-document protocol                     |
| `creative_initial`           | an enabled creative guard has a parser-time obligation                             | current guard defaults and initial creative observation only                                      |
| `datadome_initial`           | DataDome is enabled                                                                | initial script/preload route guard                                                                |
| `didomi_initial`             | Didomi is enabled                                                                  | initial configured SDK-path installation                                                          |
| `google_tag_manager_initial` | Google Tag Manager is enabled                                                      | initial script/preload/beacon/fetch guards                                                        |
| `gpt_initial`                | GPT owns/may receive the initial batch or has a parser-time attribution obligation | sole provisional GPT adapter, page targeting, listeners, slot targeting/request, handoff capture  |
| `lockr_initial`              | Lockr is enabled                                                                   | initial script guard and bounded readiness needed before SDK use                                  |
| `osano_initial`              | Osano is enabled                                                                   | initial consent mirrors required by the initial batch                                             |
| `permutive_initial`          | Permutive is enabled                                                               | initial guard/readiness and normalized segments                                                   |
| `sourcepoint_initial`        | Sourcepoint is enabled                                                             | initial SDK guard and GPP mirror                                                                  |
| `prebid_initial`             | Prebid participates in the initial batch                                           | artifact admission, publisher queue, bidder/user-ID/EID setup, initial TS bid/PUC path            |
| `testlight_initial`          | Testlight is enabled                                                               | capture preexisting callbacks before publisher replacement/drain                                  |

The canonical low-order examples are part of the server/build contract: no-bid is
`0001`, GPT attribution-only is `0081`, ADM is `0083`, creative-guarded ADM is
`008b`, APS is `0087`, creative-guarded APS is `008f`, and Prebid-plus-ADM is
`1083`. These examples do not create aliases: the route accepts only the exact
generated mask/hash pair admitted for the current configuration. APS participation
without enabled APS configuration is invalid rather than a reason to select the
render owner, and `render_owner_initial` has no independently routable production
asset; its bytes exist only inside the selected first-display composition.

Every slice registers into the bootstrap's release-private first-display sink from
the expected parser-inserted artifact and `document.currentScript`. The build fixes
their order, interfaces, and allowed imports; there is no public service locator or
third-party extension surface. The agent rejects an unknown, duplicate, omitted,
misordered, wrong-release, accessor-backed, or late slice before effects. Agent
activation is one synchronous transaction with reverse-order rollback, the same
inert-prepare/effectful-activate discipline used by the persistent catalog, and the
same ten-second bootstrap deadline. All agent-owned timers, listeners, wrappers,
observers, ports, nodes, and provisional GPT objects have named exact-once
disposers.

The persistent artifact may be requested only after every attempt in the protected
initial batch is terminal and the §5.2 paint gate has recorded
`tsjs:first-display-paint`. On an agent page, no persistent/deferred TSJS preload,
fetch, preparation, or evaluation may begin earlier. The bootstrap creates the one
authenticated classic same-origin runtime script using the manifest URL, expected
element identity, `document.currentScript`, CSP nonce, and Trusted Types rules
defined below. Independent ordinary deferred modules remain later than persistent
runtime commit.

Recording protected paint also **seals first-display TS admission** before the
persistent request begins. From that boundary until persistent or fallback commit,
the agent accepts no new TS bidder invocation, admission lease, render reservation,
attempt, direct-auction request, refresh, or navigation work. A later invocation of
the Trusted Server Prebid bidder fails before minting authority, invokes its bidder
completion exactly once with no bid, and records the internal terminal reason
`prebid_admission_failed`; it is never held for or replayed into the persistent
runtime. Native publisher Prebid/GPT calls and non-TS bidder traffic remain immediate
pass-through and may update the agent's bounded observational handoff facts, but
cannot create TS authority. Because the immutable initial batch is the only TS work
the agent may ever admit and every member is terminal before sealing, the seal must
leave zero live TS admission lease, live render reservation, unconsumed ticket,
attempt, port, or request-capable callback. Consumed/expired/stale entries that §2.3
has already converted to unexpired terminal tombstones are required suppress-only
state, not live authority: they remain until their original expiry and cross only as
the bounded terminal tombstones below. Discovery of any other live TS entry is
`bundle_partial`, not transferable state.

If the initial batch has no accepted artifact—every slot is `no_bid`, failed, or
cancelled—the same terminal/paint gate applies and takeover may proceed with an empty
committed-artifact set. The candidate performance fixture continues to require an
actual request action; an empty batch cannot manufacture `tsjs:first-display` or be
included in the timing distribution.

The sealed transport always carries this server-authored integrity value, including
on a no-agent direct-runtime page:

```ts
interface ServerBootIntegrityV1 {
  readonly version: 1
  readonly projectionDigest: string
  readonly integrationConfigDigest: string
}
```

When an agent is selected, the sibling outline has this complete v1 contract; it is
`null` on a direct-runtime page and there is no object-form compatibility shape:

```ts
interface TakeoverOutlineV1 {
  readonly version: 1
  readonly releaseId: string
  readonly generation: number
  readonly projectionDigest: string
  readonly integrationConfigDigest: string
  readonly slices: readonly FirstDisplaySliceId[]
  readonly slotCount: number
  readonly outcomeCount: number
  readonly capabilities: readonly []
  readonly objectKinds: readonly [] | readonly ['gpt_slot', 'dom_artifact']
}
```

`releaseId`, both digests, and the selected slices equal the sibling boot manifest,
always-present `ServerBootIntegrityV1`, and payload values exactly. `generation` is an
integer in `1..2^32-1` and begins at `1` for the server projection. `slotCount` is the projection `slots.length`,
`outcomeCount` is `auction.results.length`, both are in `1..256`, and complete
ordered placement coverage makes them equal. `capabilities` is exactly empty in v1;
live authority never crosses static preparation. `objectKinds` is exactly empty
when `bids` is empty and otherwise exactly the ordered pair shown above; it is a
capacity declaration, not evidence that an object already exists.

Rust computes `integrity.projectionDigest` as lowercase SHA-256 over the exact
canonical UTF-8 JSON bytes returned by the coordinated-cutover projection
canonicalizer and embedded as `boot.auctionProjection`. It computes
`integrity.integrationConfigDigest` as lowercase SHA-256 over the exact UTF-8
`JSON.stringify`-equivalent serialization of the ordered `IntegrationConfigsV1`
embedded as `boot.integrations`; object insertion order is the wire order and no
insignificant whitespace is present. Rust must normalize both admitted trees to
ECMAScript `JSON.stringify` spellings and enumeration order before hashing and
embedding; hashing `serde_json` output directly is invalid because negative zero,
fixed/exponent thresholds, IEEE-754 rounding, and integer-index object keys differ.
A shared Rust/JavaScript corpus pins those edge cases and nested combinations. On an
agent page Rust copies those exact values into the outline rather than hashing again.
The compact controller checks integrity
and outline digest form and, when the outline is non-null, their exact equality, but
does not bundle SHA-256 or the full domain validators. The first-display owner
completely validates the batch, retains the always-present sealed integrity values,
and copies them into handoff. Before direct runtime use or final adoption, persistent
core completely validates the boot and recomputes both digests from that frozen
snapshot with the same UTF-8 SHA-256 helper; any mismatch with the sibling integrity
value is `abi_mismatch` before persistent effects. Handoff adoption additionally
requires exact integrity/outline/handoff digest equality. Thus no component invents a
second canonicalization and no digest is treated as a publisher capability.

The runtime bundle first performs an effect-inert **static takeover preparation**
against only the bootstrap controller's exact immutable `TsjsBootV1` and
`ServerBootIntegrityV1` snapshots and a frozen `TakeoverOutlineV1`: exact release/generation, projection digest, canonical
integration-config digest, selected slice ids, counts, and the
capabilities/object kinds that final adoption must support. The outline contains no
slot outcome, mutable publisher/GPT state, artifact object, wrapper, observer, or
time-sensitive expiry. Preparation constructs generic inert persistent owners and
validates capacity; it cannot snapshot or depend on live agent state.

The agent increments one unsigned 32-bit `mutationRevision` after every admitted
publisher GPT/Prebid call, GPT event, DOM mutation/rebind, targeting or ownership
change, parser-guard observation, consent/segment update, and terminal/tombstone
change while runtime bytes download and prepare. It continues passing through and
observing native publisher/external activity normally, subject to the sealed TS
admission boundary above; no call is held, replayed, or allowed to act through a
prepared persistent owner. Revision exhaustion fails takeover instead of wrapping.
When static preparation is ready, the agent enters the synchronous task below,
closes its own work ingress, records the final revision, drains all already-running
synchronous mutations, and only then mints the final immutable
`FirstDisplayHandoffV1` plus one-use capsule. Because JavaScript is run-to-completion,
no GPT/Prebid/DOM/publisher task can mutate between that final snapshot and owner
activation. The persistent owner validates the snapshot and revision during the same
task; any mutation callback that arrives afterward sees only the new epoch.

`FirstDisplayHandoffV1` is an exact-shaped, recursively frozen data tree containing:

- release/generation identity plus the canonical initial-projection and
  integration-config digests;
- each slot's canonical id, aliases, DOM id, GAM path, normalized formats,
  TS/publisher ownership, request-cycle outcome, and installed targeting snapshot;
- every terminal attempt and reservation/ticket tombstone still inside its original
  bounded expiry, without descriptor, ADM, capability, or creative payload bytes;
- committed-artifact kind and ownership metadata; parser-time integration snapshot
  data needed for its persistent owner; the ordered bounded GPT-diagnostics fact
  buffer and its overflow count when diagnostics are active; and
- the exact once-only first-display timing/paint facts;
- the navigation attempt-prefix and next attempt ordinal, slot-registration-order
  next ordinal, reservation/ticket monotonic-clock epoch and remaining expiries;
- the next global GPT trace-slot ordinal; for each adopted object, its token, next
  cycle ordinal, `unknownPriorCycle`, and all retained open/completed/retired cycle
  records and quarantines permitted by the existing ten-record cap; and
- the next trace sequence, per-slot impression counters, retained trace bindings,
  and the final `mutationRevision`.

The integration configuration is not recopied from the agent into the handoff. The
bootstrap controller passes the same closure-retained, recursively frozen
`TsjsBootV1` snapshot to the agent and prepared runtime; the handoff digest must equal
that snapshot's canonical config digest before adoption. No slice or module re-reads
`window.tsjs.boot`, and replacing the public pre-load object therefore cannot change
the retained snapshot. The eventual committed `tsjs.boot` exposes that same frozen
snapshot for inspection.

The handoff contains no function, accessor, custom prototype, Promise, listener, timer,
observer, `MessagePort`, `WindowProxy`, network handle, or mutable collection. A
release-private one-use `FirstDisplayOwnershipCapsuleV1` accompanies it only during
the same synchronous takeover call. The capsule may carry the exact already-
committed GPT slot and DOM artifact object identities that cannot be reconstructed
from data. It carries no live port, timer, listener, observer, in-flight attempt, or
callable publisher surface. The capsule is generation/release bound, can be consumed
once by the authenticated prepared runtime, and is cleared by agent rollback,
fallback, or successful adoption. It is never stored on `window.tsjs`, `_internal`,
boot data, diagnostics, a log, or an analytics event.

The handoff contains at most the existing 256 initial slots/outcomes. Every `next`
counter is strictly above every value ever minted in that generation, including a
retired/pruned value absent from retained rows; adoption never derives a high-water
mark from visible rows. Each reservation/ticket entry is an unexpired terminal
tombstone and retains only the opaque value and
expiry required to suppress replay; no live authority, descriptor, ADM, or creative
payload survives. Each copied string/targeting collection retains its source grammar
and capacity. The canonical non-diagnostics data-tree encoding is at most the
existing 8 MiB boot-projection cap; the normalized diagnostics-fact subsection has
its separate 512 KiB cap from §5.8, so the complete canonical handoff is at most
8.5 MiB. The capsule has
at most one physical GPT identity and one committed artifact identity per slot.
Overflow or any nonterminal attempt/port makes takeover preparation fail; it cannot
truncate, evict a live fact, or silently lose replay suppression.

Takeover is one non-yielding JavaScript task after static preparation succeeds:

1. Revalidate the current generation, exact runtime script, outline, terminal batch,
   paint gate, and prepared runtime; close agent work ingress and record the final
   mutation revision.
2. Mint and validate the final handoff/capsule from current state. Synchronously
   quiesce agent handlers and compare-restore every provisional wrapper. The
   bootstrap callback Array remains the one append-only ingress until step 5; no
   publisher callback is invoked in this interval.
3. Detach committed artifacts from agent disposal, then dispose every remaining
   agent listener, timer, observer, port, readiness waiter, registry entry, and
   uncommitted node in reverse order.
4. Activate fresh persistent wrappers/listeners/observers and owners in catalog
   order, adopting capsule objects and handoff facts. A parser guard transfers only
   bounded data such as installed configuration and seen-node identifiers, never its
   wrapper/listener/observer. The new owner performs one bounded post-commit rescan
   so records discarded while the old observer disconnects cannot be lost.
   `adoptInitialDisplay:true` forbids parsing the projection as new
   work, redefining an adopted GPT slot, reinstalling accepted targeting, issuing
   `display`/`refresh`, creating a render iframe, or emitting any first-display mark.
5. Revalidate that the generation/revision did not change, commit the complete
   `TsjsApi`, permanently close both private registration sinks,
   transfer committed artifacts to persistent slot/navigation ownership, run
   persistent `afterCommit` work, and drain the single bootstrap queue exactly once.

No task or microtask can observe the synchronous listener/wrapper transition.
Momentary stack-local objects during that task do not constitute a second live
owner; at every task boundary exactly one of agent, persistent runtime, or fallback
owns ingress and side effects. Successful takeover leaves no agent timer, listener,
port, observer, wrapper, registry, request authority, or strong reference except the
committed objects now owned by the persistent runtime. Physical GPT identity and
publisher handoff remain exact because the object itself, rather than a guessed
path/DOM lookup, moves through the one-use capsule.

Runtime download, authentication, or effect-inert preparation failure leaves the
agent in control only until the bounded persistent-load deadline. It then settles
the terminal fallback while preserving already committed ad DOM as inert publisher-
visible output. A failure after step 2 rolls back partial persistent effects, does
not resurrect the agent, and commits that same terminal shell. It never replays the
projection, requests GAM again, removes an accepted publisher-owned ad, or constructs
a degraded runtime. Failed takeover therefore sacrifices later TSJS behavior, not
the correctness or exactly-once status of the completed first display.

During this post-paint load window the bootstrap Array remains the only TSJS
publisher-work ingress. Callable pushes are appended and run against neither owner;
they are drained once only after persistent or fallback commit. The provisional
transport intentionally has no `requestAds` or `addAdUnits`, so an attempted direct
call before commit is ordinary use-before-ready and creates no accepted operation or
Promise. On successful takeover, queued callbacks run against the complete kernel.
On authentication, load, preparation, activation, or ten-second-deadline failure,
fallback first classifies and freezes the exact `fallbackReason` under §5.3 and
freezes `initialDisplayCommitted` to whether the completed protected batch contained
at least one `accepted` result, then drains those same callbacks. Wrong
release/source/manifest/ABI identity is `abi_mismatch`; transport/load/deadline,
preparation/activation failure, or a live TS entry at the seal is `bundle_partial`.
Every `requestAds` made by a drained or later callback is a new post-paint call against
the empty safe fallback projection defined below: omitted selection resolves
`{slots:[]}`, while an explicit valid id resolves `slot_unresolved` (or
`caller_aborted` for an already-aborted signal). It does not re-report, remove, or
replay the completed initial display. Every such `addAdUnits` throws
`TsjsUnavailableError{code:'runtime_unavailable',reason:fallbackReason}` before
mutation, and `_internal.reason` is that same frozen value. No queued callback or API
call remains pending, and no post-paint transient failure retries the runtime artifact
in that document generation.

Every installed effect is a repository-owned primitive with a synchronous,
nonthrowing, identity-checked disposer. Rollback attempts physical removal/restoration
for every effect even after an earlier disposer reports failure. The mandatory
security/correctness postcondition is generation-latched inertness: a publisher who
replaced a global after agent installation may prevent literal restoration, but the
old wrapper/listener/observer cannot authorize, request, render, or mutate TS state.
Tests require literal removal where the platform operation succeeds and zero
surviving authority in every case; “no second listener/wrapper survives” means no
live TSJS authority, not control over publisher replacements.

The atomic **takeover** transaction runs on an agent page; the ordinary bootstrap
transaction runs on a page where the agent is omitted. Takeover module bytes never
enter the first-display artifact. The agent path follows the terminal, paint, and
takeover sequence above; the ten-second no-attempt guard remains only for a
direct-to-runtime page with no server-projected agent batch.

Each shipped integration remains a separately built IIFE with imports inlined, so
module singletons cannot be the shared-runtime mechanism. `tsjs-core` installs the
only runtime and keeps the capability broker in its composition closure. During
boot or takeover, `_registerIntegration` collects exact release-bound takeover modules from the
same server-composed script. After kernel commit it accepts only a currently loading,
manifest-declared deferred module from the exact core-created script element. It is
permanently refusing for unknown, duplicate, wrong-release, wrong-phase, publisher-
invoked, replaced-node, or already-terminal registrations. `tsjs._internal` exposes
only the frozen status described in §5.4, never the broker or phase loader.

Every integration module registers through:

```ts
interface IntegrationPrepareContextV1 {
  readonly config: unknown
  readonly interfaces: Readonly<Record<string, unknown>>
  readonly signal: AbortSignal
  readonly onDispose: (callback: () => void) => void
}

interface IntegrationActivationContextV1 {
  readonly signal: AbortSignal
  readonly onDispose: (callback: () => void) => void
  readonly afterCommit: (callback: () => void) => void
  readonly adoption?: unknown
}

interface PreparedIntegrationV1 {
  readonly activate: (ctx: Readonly<IntegrationActivationContextV1>) => void
  readonly interfaces?: Readonly<Record<string, unknown>>
}

type IntegrationRegistrationV1 =
  | Readonly<{
      abi: 1
      id: string
      phase: 'takeover'
      releaseId: string
      prepareSync: (
        ctx: Readonly<IntegrationPrepareContextV1>
      ) => PreparedIntegrationV1
      prepare: (
        ctx: Readonly<IntegrationPrepareContextV1>
      ) => PreparedIntegrationV1 | Promise<PreparedIntegrationV1>
    }>
  | Readonly<{
      abi: 1
      id: string
      phase: 'deferred'
      releaseId: string
      prepare: (
        ctx: Readonly<IntegrationPrepareContextV1>
      ) => PreparedIntegrationV1 | Promise<PreparedIntegrationV1>
    }>

tsjs._registerIntegration(registration)
```

Every takeover module supplies both entry points because the same release-owned
implementation has two boot contexts. A no-agent parser-blocking runtime calls only
`prepareSync`; returning a Promise/thenable, throwing, or crossing the owning
monotonic deadline fails the transaction before the parser resumes. An agent-page
post-paint takeover calls only `prepare`, which may await effect-inert work under the
same deadline. A deferred module has only `prepare`; supplying `prepareSync` is an
unknown key and fails registration. Shared pure construction may sit behind the two
takeover entry points, but neither entry point may activate twice or retain a
detached continuation.

This is a release-internal bundle handshake, not a publisher extension API. An
**integration** remains the product capability; an **integration module** is only
that integration's transactional TSJS implementation unit. The design introduces
no separately installed or third-party plugin system.

The build first emits the bootstrap controller, first-display base, every
first-display slice, core, and every production integration module with the same
fixed release sentinel,
then computes one `releaseId`: 64 lowercase hexadecimal SHA-256 characters over a
canonical ordered release inventory containing every artifact id, role, phase, and
its sentinel-normalized
bytes. It replaces exactly one sentinel in each bundle and verifies none remains.
This avoids a self-referential hash while changing the id for any logical bundle or
ordering/role/phase change. First-display base/slice role, order, mask bit, or byte
changes therefore change the release identity. The same value is embedded in the
bootstrap controller, every agent base/slice, core, and every integration bundle.
The bootstrap controller, agent base/slices, and core use reserved artifact ids and
never appear as persistent integration entries.
Before core is injected, the server emits this exact manifest. This is the first and
only `BootManifestV1` shape; the unreleased all-required draft has no compatibility
status:

```ts
type BootManifestIntegrationV1 =
  | Readonly<{
      id: string
      phase: 'takeover'
    }>
  | Readonly<{
      id: string
      phase: 'deferred'
      trigger: 'first_display_or_idle'
      src: string
    }>

interface BootManifestV1 {
  readonly version: 1
  readonly releaseId: string
  readonly firstDisplay: null | Readonly<{
    src: string
    slices: readonly string[]
  }>
  readonly runtimeSrc: string
  readonly integrations: readonly BootManifestIntegrationV1[]
}

type IntegrationConfigIdV1 =
  | 'aps'
  | 'datadome'
  | 'didomi'
  | 'google_tag_manager'
  | 'gpt'
  | 'lockr'
  | 'osano'
  | 'permutive'
  | 'prebid'
  | 'sourcepoint'
  | 'testlight'

type BootJsonPrimitiveV1 = null | boolean | number | string
type BootJsonValueV1 =
  | BootJsonPrimitiveV1
  | readonly BootJsonValueV1[]
  | Readonly<{ readonly [key: string]: BootJsonValueV1 }>

interface IntegrationConfigEntryV1 {
  readonly id: IntegrationConfigIdV1
  readonly config: Readonly<{ readonly [key: string]: BootJsonValueV1 }>
}

interface IntegrationConfigsV1 {
  readonly version: 1
  readonly entries: readonly Readonly<IntegrationConfigEntryV1>[]
}
```

`IntegrationConfigsV1` is the only generic browser configuration carrier. Its
entries appear once each in the union order above, contain exactly the enabled
product integrations for the document, and are capped at 11. APS emits `{}` when it
is enabled but has no browser-selectable setting; an absent/disabled integration has
no entry. The `gpt_later`, `prebid_later`, and consent/lifecycle deferred modules use
their product's one existing entry rather than receiving a second config. Creative
and diagnostics remain the dedicated typed `creative` and `diagnostics` boot fields
because they are also stable public inspection surfaces. `render_runtime` and
`diagnostics_presentation` have no independent config entry.

The server serializes each integration's explicit browser-safe projection of its
existing typed rc configuration into this carrier and rejects a manifest/config
predicate mismatch before emitting HTML. Server credentials, secrets, auth headers,
cookies, private endpoint policy, and unprojected configuration fields have no
browser type and cannot enter the carrier.

The production inline transport is one server-sealed canonical JSON string, not a
browser-visible mutable object graph. Rust serializes exact
`{version:1,boot,integrity,outline}` bytes only after the complete projection, manifest,
configuration, creative, diagnostics, selection, digest, and count contracts pass;
it then escapes that string for one inline-script lexical constant without changing
the decoded JSON. The existing 8 MiB projection and 512 KiB integration-carrier
limits remain authoritative, and the complete decoded transport is capped at 10 MiB
UTF-8. The controller reads no boot global and accepts no object-form production
transport or compatibility alias.

The controller's production transport parser performs `JSON.parse` synchronously,
which creates a fresh acyclic tree of ordinary objects, dense Arrays, and data
properties and therefore cannot preserve a source accessor, Proxy, symbol, custom
prototype, sparse hole, or repeated object identity. Before any DOM, timer, queue,
GPT, attribution, or registration effect, the parser requires the exact root keys
and version, embedded release equality, exact critical boot keys, manifest release
and URL/mask/slice relationships, at most 14 takeover entries, every deferred source
bound to its declared module id, the always-present exact integrity shape and digest
forms, null/non-null first-display and outline parity, outline
release/slice/count/digest forms and digest equality with integrity, dedicated
creative/diagnostics booleans, and the configured decoded-size bound. It recursively freezes the parsed tree,
retains that exact snapshot in the controller closure, and never again reads the
lexical string or a public global. Parse, critical validation, or freeze failure is
`abi_mismatch` before effects.

This compact production parser is deliberately not a second domain validator. The
first-display base validates its complete immutable projection/batch before
activation; every selected slice validates its attenuated product value before its
effect; and persistent core performs the complete manifest, projection, carrier,
creative, and diagnostics validation before a direct boot or takeover uses them.
If that post-claim validation fails, persistent core returns `abi_mismatch` through
the one-use completion capability returned by the one-use,
exact-source-authenticated closure claim during the current runtime script task; the
controller commits fallback immediately instead of allowing the load watchdog to
relabel the failure `bundle_partial`. Takeover preparation or activation failure is
reported through that same capability. In takeover mode only the bootstrap owner may
publish terminal fallback: it preserves accepted DOM, snapshots the agent's
`initialDisplayCommitted` state, disposes the provisional agent, and commits once.
The persistent runtime never publishes a competing takeover fallback. On a direct
page, the persistent runtime remains the terminal-fallback owner after a valid claim.
The full object-form validators retain their existing hostile-object contract for
runtime/public/test boundaries: exact `version`/`entries`, unique ordered ids,
plain configs, finite JSON values, dense Arrays, own enumerable data properties, no
accessors/symbols/custom prototypes/cycles/aliases, depth 16, 4,096 values, 4,096-byte
keys/strings, 65,536-byte entries, and the 524,288-byte carrier. The inline
bootstrap artifact must not import those full validators or the complete auction-
projection parser merely to revalidate the trusted lexical producer.
The closure claim atomically reserves a private `claiming` state before it touches
any mutable DOM or realm authentication surface and rolls back only after a failed
authentication. Its completion capability admits outcomes through primitive string
comparisons, consumes its one-use guard before invoking the selected handler, and
therefore remains one-shot under reentrant claim and completion attempts.
The compact fallback's `addAdUnits` surface still runs the exact public registration
validator before it refuses a valid call. That validator imports its bounded-string
primitive from an effect-free bootstrap-safe leaf; it does not make bootstrap reach
the auction-projection parser.
The persistent core receives the manifest-entry capacity as a generated numeric
build constant; it does not import the build-time release catalog merely to learn
that bound. Substitution may tree-shake the declaration module completely, and the
release metafile must prove that the persistent core graph excludes the catalog.

The release catalog binds every module id to exactly one config source: its product
entry, `creative`, `diagnostics`, or none. The integration's generated exact typed
validator runs during preparation and rejects unknown/missing fields or a
manifest/config inclusion mismatch as `abi_mismatch`; it does not silently default
browser data. `IntegrationPrepareContextV1.config` is only that module's frozen value,
never the complete map, and deferred modules receive the same frozen product value
captured at bootstrap. The first-display base likewise supplies each selected slice
only its own attenuated value.

Integration ids match `^[a-z0-9][a-z0-9_-]{0,63}$`, are unique, and appear in the actual
server phase/injection order. Takeover entries precede deferred entries, the list
contains exactly the enabled persistent-runtime modules for that page, and there are
at most 20. `firstDisplay` is either exact `null` or the server-selected agent
artifact and its canonical ordered subset of the 14 slice ids in §5.2.1; ids are
unique and a list contains at most 14. The list and encoded mask represent exactly
the same set. Its `src` is the exact same-origin
`/static/tsjs=tsjs-first-display.min.js?m=<sliceMask>&v=<firstDisplayHash>` URL.
`sliceMask` is exactly four lowercase hexadecimal digits encoding the 14 catalog
rows in order; bit 0 (`first_display`) is required and unused upper bits are zero.
The server configuration resolver enumerates the finite masks permitted by its
enabled integration set and precomputes each exact body identity outside request
handling. Since APS/GPT/Prebid participation is projection-dependent, a mask may
omit an enabled slice but cannot add a disabled one; `first_display` is always set.
The hash names the exact uncompressed base agent plus selected slice bytes in catalog
order. A mask not permitted by current trusted configuration is not served even when
its hash is otherwise valid.
`runtimeSrc` is the exact same-origin
`/static/tsjs=tsjs-unified.min.js?v=<runtimeHash>` URL emitted immediately after
the bootstrap controller only when `firstDisplay` is `null`, and otherwise loaded by
the controller after the agent's protected paint. `runtimeHash` is 64 lowercase
hexadecimal characters equal to SHA-256 over the exact uncompressed UTF-8 response
bytes. Those bytes are core followed by the manifest's takeover IIFEs in manifest
order with the build's exact `;\n` separator; their embedded registrations and `releaseId` bind the
URL to the catalog, phase, order, and release. Manifest URLs are transported only as
canonical root-relative strings. During validation, the bootstrap controller and
then core resolve each once against the trusted document origin—never
`document.baseURI`—require the same
origin and exact path/query round-trip, and freeze one absolute URL per entry. All
DOM, Trusted Types, and current-script comparisons use only those canonical absolute
values. With an agent, the controller accepts only one parser-inserted
`script#trustedserver-js` whose resolved `src` equals the absolute first-display
`src`; without an agent it accepts only that element with absolute `runtimeSrc`.
The dynamically inserted runtime node uses the reserved
`trustedserver-js-runtime` id. An absent, duplicate, or mismatched expected node
fails the owning transaction. Redirect refusal is enforced by the local transport
below.
For an ordinary publisher document, the trusted document origin is the captured
exact HTTP(S) `window.location.origin`. A sandboxed `srcdoc` creative has the opaque
origin `"null"`; its server-owned parent therefore defines one own, non-enumerable,
non-configurable, non-writable data property `window.__tsCreativeOrigin` containing
the parent's exact HTTP(S) origin before any bidder markup or TSJS tag. Core accepts
that stamp only when the document origin is exactly `"null"` and rejects an absent,
accessor-backed, mutable, enumerable, credentialed, non-origin, or inherited value.
The stamp authenticates only the local content-addressed TSJS URL; it is not a
publisher API or a general cross-origin capability. Opaque creative documents have
no deferred entries, so the phase loader never creates a dynamic script in that
realm.
Every deferred `src` is an exact same-origin `/static/tsjs=tsjs-<id>.min.js?v=<hash>`
URL generated by the server for that release; accessors, arbitrary hosts, fragments,
duplicate URLs, and mismatched ids/hashes fail manifest validation. That local
static route must return the exact release artifact directly and never redirect.
The persistent registration value must be a non-null plain object with exactly the
six own enumerable data properties shown for takeover or the five shown for
deferred; accessors, unknown/missing/inherited keys, a custom prototype, wrong
literals/types, or a non-callable required entry point are rejected before the
factory is retained. Registration requires exact id membership, phase, `releaseId` equality, expected script
element identity, and `document.currentScript` identity. Integrations obtain stateful
capabilities from the closure-private broker; they never construct a second runtime
or replace an existing adapter, slot registry, dispatcher, or provider.

The server emits exactly one parser-blocking TSJS artifact: the first-display agent
when selected, otherwise the persistent runtime artifact. Agent bytes are its base
followed by every selected slice in catalog order. Runtime bytes are core followed
by every `phase:'takeover'` IIFE in manifest order. Each transaction therefore
incurs one request and no integration waterfall. The server emits no parser-time tag
for the other artifact or for a deferred module. The bootstrap alone creates the
post-paint runtime node; after commit, core alone creates ordinary deferred nodes.
`defer`, `async`, preload, or a post-commit callback attached to an already
downloaded monolith does not satisfy this contract. In particular, transfer
remediation cannot add a PUC-owner script, dynamic import, fetch, preload, worker,
blob program, or other auxiliary TSJS asset before protected paint. The existing APS
renderer-v2 iframe navigation remains the sole post-action renderer materialization
request and does not become a code-loader for the top-page owner.

The static transport is exact and shared by Fastly, Axum, Cloudflare, and Spin.
Only `GET` and `HEAD` for
`/static/tsjs=tsjs-first-display.min.js?m=<sliceMask>&v=<firstDisplayHash>`,
`/static/tsjs=tsjs-unified.min.js?v=<runtimeHash>` and
`/static/tsjs=tsjs-<deferred-id>.min.js?v=<moduleHash>` are admitted. The
first-display query has exactly canonical `m` then `v`; the other routes have
exactly one `v` and no other field. `firstDisplayHash` and `runtimeHash` use
the exact composition rules above; `moduleHash` is SHA-256 over that deferred
artifact's exact uncompressed UTF-8 bytes. The handler derives the enabled ordered
first-display set from the validated mask plus trusted configuration, or the
takeover/deferred catalog entry, recomputes the hash, and returns the current
artifact only on an exact match. HTML composition chooses only a precomputed
release-size-admitted mask from the immutable projection; the later static request never needs
request-local projection state or a dynamic artifact cache. `HEAD` returns
the same status and metadata as `GET` with an empty body. An unconditional success
is `200`, `Content-Type: application/javascript; charset=utf-8`, and
`X-Content-Type-Options: nosniff`; the existing strong-ETag/static-cache behavior,
including a valid conditional `304`, is preserved without adding a new cache
requirement. A missing/malformed/stale hash, unknown or disabled id, wrong method,
unsupported method, legacy filename, or extra query field receives the adapter-local `404 no-store` and
never falls through to publisher origin. Redirects are forbidden. Compression may
change transfer bytes but not the uncompressed bytes named by `v`.

This hard cutover serves only artifacts embedded in the active binary. It does not
add an N/N-1 asset store or retain a previous release's TSJS routes. A page carrying
an old manifest across deployment can receive the typed artifact/deferred failure
defined here and must reload; the retained prior _binary_ in §8 is solely the whole-
deployment rollback artifact. After rollback, that binary again serves its own
release. This accepted stale-page break is not a cache or compatibility project.

Upstream-library loading is orthogonal to TSJS bundling. After the bootstrap
controller, the server may emit the existing fixed/configured live GPT tag as a non-
parser-blocking fetch early enough to overlap the first-display TSJS request when GPT is
required for the first projected display. When Prebid integration is enabled, its
external artifact tag is always emitted through that early overlap path because the
rc-baseline client readiness, bidder/user-ID/EID configuration, publisher queue, and
initial auction contract are required before the first action. It remains
an external script, never a TSJS source input or TSJS generated artifact. Its adapter installs
and owns all request-capable actions only after the agent transaction commits, so
an early library load cannot race a TS-owned display before correctness listeners.
An optional/later upstream script follows its owning deferred module's trigger and
cannot be prefetched by this path. The APS runner is never boot-preloaded: only a
winning APS renderer document loads it through the live fixed-target proxy specified
in §§3.6 and 4.4.

Phase assignment is a release-catalog decision, not a publisher input or browser
heuristic. Where one product supports initial and persistent behavior, the build
defines the fixed first-display slice from §5.2.1 and the catalogued takeover module
below. The server selects an exact first-display subset from trusted configuration
and the immutable initial projection; it never relabels a module id or moves an
unbounded implementation into the agent:

- **first display** contains only the agent/slices required before the initial
  request action, terminal result, and protected paint. APS/ADM protocol handling,
  initial GPT/Prebid admission, and parser-time guards live here only when selected;
- **takeover** contains the complete persistent owner for enabled behavior, including
  programmatic/direct auctions, ongoing lifecycle state, publisher APIs, refresh,
  later navigation, reconciliation, diagnostics data, rc-baseline behavior, and
  retained audited concept gaps. On an agent page it prepares after paint and adopts initial state;
  on a no-agent page it is the ordinary runtime boot transaction; and
- **deferred** remains restricted to independently loadable behavior that has no
  ownership or parser-time obligation at persistent-runtime commit, such as
  presentation UI and a genuinely optional later lifecycle slice. A deferred
  consumer binds only the already committed broker.

A product integration may therefore have a first-display slice, one takeover module,
and a deferred module. Those are release-owned implementation units of one product,
not separately configurable products. The handoff record/capsule is the only bridge
from the provisional slice; persistent/deferred sharing uses exact frozen broker
capabilities from the one runtime.

This is the canonical release catalog and its order. Capability lists use the exact
keys shown; `—` means none. The inclusion predicate is server-owned and
deny-unknown. A module id, phase, trigger, predicate, provider/consumer list, or
obligation absent from this table is not a production module:

| Order | Module id                  | Product     | Phase / trigger                    | Include iff                                         | Provides                                                                                               | Consumes                                                                                                     | Takeover obligation or deferred scope                                                              |
| ----: | -------------------------- | ----------- | ---------------------------------- | --------------------------------------------------- | ------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------ | -------------------------------------------------------------------------------------------------- |
|     1 | `render_runtime`           | runtime     | takeover                           | always                                              | `slots.v1`, `auction.v1`, `render.v1`, `messages.v1`, `trace.v1`, `trace.presentation.v1`, `direct.v1` | `runtime.v1`                                                                                                 | Adopt initial facts/artifacts; own every later direct render, lifecycle, dispatcher, and trace     |
|     2 | `aps`                      | APS         | takeover                           | APS integration enabled                             | `aps.v1`                                                                                               | `runtime.v1`, `slots.v1`, `render.v1`, `messages.v1`, `trace.v1`                                             | Adopt initial APS tombstones/artifacts; own all later APS and PUC work                             |
|     3 | `creative`                 | creative    | takeover                           | `creative.enabled && (clickGuard \|\| renderGuard)` | —                                                                                                      | `runtime.v1`                                                                                                 | Adopt parser-time guard state and own later creative behavior                                      |
|     4 | `datadome`                 | DataDome    | takeover                           | DataDome enabled                                    | —                                                                                                      | `runtime.v1`                                                                                                 | Adopt the initial guard state and own later route rewriting                                        |
|     5 | `didomi`                   | Didomi      | takeover                           | Didomi enabled                                      | —                                                                                                      | `runtime.v1`                                                                                                 | Adopt the configured SDK path and own later integration behavior                                   |
|     6 | `google_tag_manager`       | GTM/GA      | takeover                           | Google Tag Manager enabled                          | —                                                                                                      | `runtime.v1`                                                                                                 | Adopt the initial guards and own later matching traffic                                            |
|     7 | `gpt`                      | GPT         | takeover                           | GPT integration enabled                             | `gpt.v1`, `gpt.events.v1`, `pbs_cache.baseline.v1`                                                     | `runtime.v1`, `slots.v1`, `auction.v1`, `render.v1`, `messages.v1`, `trace.v1`, and `aps.v1` iff APS enabled | Sole persistent GPT adapter/listeners; adopt initial physical slots and own handoff/reconciliation |
|     8 | `gpt_diagnostics`          | diagnostics | takeover                           | `diagnostics.gpt.active`                            | `gpt_diag.v1`                                                                                          | `runtime.v1`, `gpt.events.v1`                                                                                | Consume persistent GPT facts and commit the final data-only public API                             |
|     9 | `lockr`                    | Lockr       | takeover                           | Lockr enabled                                       | —                                                                                                      | `runtime.v1`                                                                                                 | Adopt initial guard/readiness state and own later API-host rewriting                               |
|    10 | `osano_consent`            | Osano       | takeover                           | Osano enabled                                       | `osano_consent.v1`                                                                                     | `runtime.v1`                                                                                                 | Adopt the initial consent mirror and own later consent-dependent work                              |
|    11 | `permutive_context`        | Permutive   | takeover                           | Permutive enabled                                   | `permutive_context.v1`                                                                                 | `runtime.v1`                                                                                                 | Adopt initial normalized segments and own later context                                            |
|    12 | `sourcepoint_consent`      | Sourcepoint | takeover                           | Sourcepoint enabled                                 | `sourcepoint_consent.v1`                                                                               | `runtime.v1`                                                                                                 | Adopt the initial GPP mirror/guard and own later consent work                                      |
|    13 | `prebid`                   | Prebid      | takeover                           | Prebid integration enabled                          | `prebid.v1`                                                                                            | `runtime.v1`, `slots.v1`, `render.v1`, `messages.v1`, and `aps.v1` iff APS enabled                           | Adopt initial artifact/queue/bid state and own subsequent Prebid admission                         |
|    14 | `testlight`                | Testlight   | takeover                           | Testlight enabled                                   | —                                                                                                      | `runtime.v1`                                                                                                 | Adopt initial callback capture and own later bridging                                              |
|    15 | `diagnostics_presentation` | diagnostics | deferred / `first_display_or_idle` | `renderTraceOverlay \|\| diagnostics.gpt.active`    | —                                                                                                      | `runtime.v1`, `trace.presentation.v1`, and `gpt_diag.v1` iff active                                          | DOM overlay, badges, formatting, clipboard/download interaction                                    |
|    16 | `gpt_later`                | GPT         | deferred / `first_display_or_idle` | GPT enabled                                         | —                                                                                                      | `runtime.v1`, `slots.v1`, `auction.v1`, `render.v1`, `gpt.v1`, `trace.v1`                                    | Post-first-display refresh, SPA navigation, and later reconciliation only                          |
|    17 | `osano_lifecycle`          | Osano       | deferred / `first_display_or_idle` | Osano enabled                                       | —                                                                                                      | `runtime.v1`, `osano_consent.v1`                                                                             | Later retry/event/focus/visibility/clear maintenance                                               |
|    18 | `permutive_lifecycle`      | Permutive   | deferred / `first_display_or_idle` | Permutive enabled                                   | —                                                                                                      | `runtime.v1`, `permutive_context.v1`                                                                         | Later SDK/segment refresh maintenance                                                              |
|    19 | `prebid_later`             | Prebid      | deferred / `first_display_or_idle` | Prebid and GPT enabled                              | —                                                                                                      | `runtime.v1`, `slots.v1`, `gpt.v1`, `prebid.v1`                                                              | Synthetic refresh and GAM-path exclusion; never initial admission                                  |
|    20 | `sourcepoint_lifecycle`    | Sourcepoint | deferred / `first_display_or_idle` | Sourcepoint enabled                                 | —                                                                                                      | `runtime.v1`, `sourcepoint_consent.v1`                                                                       | Later retry/visibility/focus/update/safe-clear maintenance                                         |

`runtime.v1` is the kernel's only built-in capability: generation, disposal,
clock/scheduler, queue, logger, validated boot data, capability access, and phase
loading. All other keys have exactly the provider above. Optional consumption is
allowed only where the table says `iff active`; absence in that case is never
silently substituted. The maximal manifest therefore has 14 takeover plus six
deferred entries: `MAX_TAKEOVER_MODULES = 14` and `MAX_MANIFEST_MODULES = 20`.
The serializer, parser, registry, callback staging, tests, and fuzz/capacity fixtures
derive 13/14/15 and 19/20/21 boundaries from this table rather than retaining a
hand-written 16. The kernel diagnostics ingress has no integration-module
subscription surface or subscription-capacity constant. GPT diagnostics consumes
the separately bounded `gpt.events.v1` capability, while deferred diagnostics
presentation alone consumes `trace.presentation.v1`. `attachPresentation` is absent
from `trace.v1`, so APS, GPT, and `gpt_later` cannot obtain presentation authority
even though they publish or consume trace facts. Public diagnostic subscriber limits
remain separate.

The release catalog records, for every module id, its product integration, phase,
trigger, provided/consumed capability keys, and whether parser-time activation is a
proved rc-baseline or retained-gap obligation. The build rejects dependency cycles, a deferred
provider consumed by another module, two providers for one key, a phase override
from server or publisher data, provider-after-consumer manifest order, and any
takeover entry that imports a catalogued deferred source area. GPT, the APS runner,
the external Prebid artifact, PUC, and all
other upstream script bytes remain remote/live and are never copied into a TSJS
bundle; only Trusted Server-owned adapters, contracts, and lifecycle code may be in
these artifacts.

Both persistent phases use the same module-transaction rules. Registration stores
code but does not execute it. On an agent page, the post-paint takeover transaction
calls `prepare(ctx)` in manifest order. On a no-agent page, the one parser-blocking
runtime calls `prepareSync(ctx)` in manifest order and never opens an asynchronous
preparation gap. Each deferred transaction calls its own `prepare(ctx)` independently
after that module's accepted registration and `load` checkpoint, without awaiting or
ordering against a deferred sibling. `prepare` may be synchronous or asynchronous;
`prepareSync` must be synchronous. Their only legal effects are validating frozen configuration,
obtaining declared capability interfaces, allocating private inert data/closures,
and registering private-memory disposers. Preparation cannot read or write ad-tech
globals, attach a listener/observer/wrapper, touch live DOM, inject a script, start a
timer/fetch, schedule detached work, invoke publisher code, or call a stateful
adapter/service method. For `prepare` only, the one Promise returned to and awaited
by the phase owner, including its ordinary `await`/settlement continuations, is
permitted; no continuation may be detached from that Promise or survive its
settlement/abort. `prepareSync` returning a Promise or any thenable is an ABI failure.
Preparation returns exactly one prepared module with a synchronous `activate(ctx)`
function and its declared frozen capability interfaces. Core validates and stages a
provider's interfaces immediately after that provider prepares, so later consumers
can receive them in their preparation context; the interfaces remain effect-inert
until provider activation and are removed during rollback. Preparation code cannot
call a staged stateful capability. Takeover activation order is the same
provider-before-consumer order, so no consumer becomes live first.

After every takeover module prepares, core enters one synchronous takeover
activation barrier in manifest order. On the no-agent path this barrier runs in the
same classic parser-blocking script evaluation as every `prepareSync` call and the
kernel commit; no Promise, microtask, timer, network wait, or publisher task can occur
between preparation, activation, and commit. Every catalogued parser-time obligation
therefore installs before the script returns. In particular, the GPT module installs
correctness listeners and, when `gamAttributionEnabled`, synchronously enqueues the
one `googletag.setConfig({targeting:{ts:'true'}})` command during activation. If the
GPT command queue is already live it executes before activation returns; otherwise
it precedes publisher parser work after the TSJS tag. Async SDK readiness and all
request-capable work start only through `afterCommit`. On an agent page the
first-display slice already owns those parser-time obligations; post-paint takeover
adopts them through §5.2.1 instead of installing a competing guard.

`activate(ctx)` may install only synchronously
compare-restorable wrappers, listeners, observers, guards, provider live-state
transitions, and service subscriptions. It registers the disposer before each mutation and may stage bounded
post-commit work through `ctx.afterCommit(fn)`, but cannot inject/load a script,
start network/timers, schedule work, drain a publisher queue, or invoke publisher
callbacks directly. The persistent message-dispatcher and GPT-adapter provider
modules occupy the catalogued provider positions before their consumers, and their
correctness listeners are activated there rather than left live during asynchronous
preparation. If any takeover activation throws, core synchronously
runs every activated and prepared disposer once in reverse order before committing
fallback. Since the barrier never yields and activation cannot call publisher code,
no publisher task can observe a partial persistent generation.

The takeover activation barrier checks its monotonic deadline immediately
before and after every `activate` call and once more before kernel handoff. Elapsed
time greater than or equal to 10,000 ms synchronously unwinds and commits fallback
even when the timer task has not run. JavaScript cannot preempt an activation
function that never returns; a malicious/nonreturning same-realm module can freeze
the page and is an accepted platform limitation, not a second-runtime recovery case.

After all takeover activations succeed, core commits the kernel API, runs takeover
`afterCommit` callbacks in manifest order, and only then drains the bootstrap queue.
Those callbacks may synchronously start required upstream scripts, timers, readiness
work, and rc-baseline DOM scans; publisher code they intentionally invoke therefore
sees the complete kernel. A callback throw is isolated to its module, runs that
module's remaining disposers, records a bounded local runtime failure, and makes
affected operations fail through their existing typed readiness/render result; it
cannot roll back an already published kernel or create a fallback generation.

Every deferred module uses the sole `first_display_or_idle` phase gate. Core arms a
10,000 ms **attempt-creation** guard at kernel commit on every page, including one
with no server projection, so an immediate programmatic `addAdUnits`/`requestAds`
first display is protected. The first render batch to create an attempt during that
window becomes the immutable protected first-display batch. The guard is cancelled,
and deferred work waits without another fixed cutoff until every attempt in that
batch reaches its terminal latch. Readiness plus GPT/renderer completion may
therefore exceed ten seconds without a deferred race. The guard fires only when no
server-projected or programmatic attempt was created by its boundary. Firing is the
explicit runtime decision that no _startup-protected_ display exists; it releases the
deferred phase even though publisher code may create a later first attempt. That
post-window attempt uses the same correct persistent owners but may overlap already
released later work. The design makes no absolute load-contention guarantee for a
first display initiated after 10,000 ms.

Terminal/no-attempt is not itself permission to fetch. On a visible document, the
phase loader waits through two owned `requestAnimationFrame` callbacks, guaranteeing
one intervening paint opportunity, and records the internal paint gate only in the
second callback. If the document is hidden, it waits for the first of visibility
return through that same two-frame gate or a 2,000 ms owned idle timeout; there is no
visible paint to contend with while hidden. Only after that gate does it schedule
deferred loading in `requestIdleCallback({timeout:2_000})`. The
non-idle fallback is one owned 50 ms timer created after the paint/hidden gate, never
a zero-delay task before paint.

The catalog has no on-demand network trigger. A takeover provider may expose a
bounded readiness facade for behavior implemented by a deferred slice. Each caller's
existing deadline begins at its original enqueue time and may expire while the phase
gate is closed. The shared module receives a separate ten-second load/transaction
deadline only when `first_display_or_idle` actually triggers; only still-live
waiters observe readiness. Expiring one waiter never aborts the shared module while
another waiter or the catalogued background load still owns it.

Core creates a classic same-origin script element with the canonical absolute form of
the exact manifest `src`,
stores its identity before insertion, and authenticates registration with that
identity plus `document.currentScript`. It uses neither dynamic `import()` nor
`eval`. After the common paint/idle gate, it starts every included deferred module's
independent transaction in manifest order without awaiting another module. Each
dynamically created classic script is `async = true`; network, evaluation, and local
transaction completion may finish in any order. This is safe because the catalog
forbids deferred-to-deferred capability edges. A hung, failed, or slow module cannot
consume another module's deadline or delay its fetch, preparation, or activation.

Deferred insertion preserves the publisher's script policy; it never rewrites a
Content-Security-Policy header or meta element and never adds `unsafe-inline`,
`unsafe-eval`, a source host, or a default Trusted Types policy. The parser-inserted
bootstrap and parser-inserted first-display/runtime tags remain subject to the publisher's existing CSP and are
a deployment precondition just as TSJS injection is on the rc baseline. When those
tags carry a CSP nonce, they must carry the same response-local value, and core
copies the authenticated parser-inserted element's `nonce` IDL value to the runtime
and every deferred script before
insertion. This supports nonce-only policies and preserves the trusted-root chain
under `strict-dynamic`; an absent nonce is never synthesized or copied from an
unrelated publisher element.

`HTMLScriptElement.src` is a Trusted Types script-URL sink. When the browser exposes
`trustedTypes`, core attempts once per document runtime to create the closure-private
policy `trusted-server#tsjs-v1`. Its `createScriptURL` callback returns its canonical
absolute argument only when that value is exact membership in the frozen absolute
deferred URL set; it rejects every other string and is never exposed. If publisher CSP does
not permit that policy name or the name is unavailable, core may assign the raw
manifest string so a publisher's existing default policy or a non-enforcing browser
can process it, but it immediately verifies that the element's resolved `src` is
still byte-for-byte the expected canonical absolute URL before insertion. A
synchronous Trusted Types throw, empty value, mutation, or policy result outside the
exact manifest is `policy_blocked`; TSJS does
not try `setAttribute`, another policy name, a blob/data URL, `eval`, or a remote
fallback. Under enforcing Trusted Types with neither the named policy nor a
publisher default policy that preserves the exact URL, the affected deferred module
therefore becomes unavailable while the committed runtime stays live. Publishers
that restrict the `trusted-types` directive and require deferred TSJS behavior must
allow the fixed `trusted-server#tsjs-v1` name; this grants only the exact release
URLs already selected by the server. A nonce/source CSP rejection after insertion is
observed through the script's `error` event and is `load_error`, not
`policy_blocked`; the runtime installs no `SecurityPolicyViolationEvent` listener.
A node removed or replaced after insertion may already have initiated a request, but
its disconnected/replaced identity cannot register and settles as
`registration_rejected` (or `load_error` if fetch rejection wins).

Full document-runtime disposal aborts pending module readiness, clears idle/
timer/load listeners, removes an uncommitted script node, and makes late registration
inert. SPA navigation disposal cancels only waiters and state owned by that
`NavigationSession`; the runtime-owned fetch/transaction may complete for the next
session and cannot retain or act on the disposed one.
No deferred request, preload, preparation, or execution may begin before its trigger,
and a `first_display_or_idle` module must not begin before the reference fixture's
`tsjs:first-display-paint` gate.

Each deferred entry follows
`not_triggered -> loading -> registered -> preparing -> activating -> ready`, with
any nonterminal state able to move once to `unavailable`. Registration only stores
the factory. The exact script `load` must then observe exactly that one accepted
registration before preparation begins; `load` without registration, `error`, a
second registration, or deadline/disposal wins the terminal latch and never invokes
the factory. The load/error listeners and inert script node are removed as soon as
that fetch/registration checkpoint becomes terminal; executing bytes remain owned
only by the registered factory/module closure. The bounded internal unavailable reason distinguishes `load_error`,
`load_without_registration`, `registration_rejected`, `prepare_failed`,
`activation_failed`, `after_commit_failed`, `policy_blocked`, `module_timeout`, and
`disposed` for local diagnostics/tests. It is not added to `TsjsApi` or an analytics
schema.

Each deferred module gets one fixed ten-second deadline from its trigger through
script fetch, accepted registration, preparation, activation, and `afterCommit`.
Queued dependent operations retain the independent original deadlines above and
share the module readiness Promise only while live. With no waiting operation, the
module deadline still retires its script and state rather than leaking indefinitely. The module's
preparation and synchronous activation run against the already committed
`RuntimeSession` and broker, after which its `afterCommit` callback runs. Failure,
timeout, wrong bytes, or disposal marks only that module terminal-unavailable and
settles dependent work through its existing exact typed reason (for example
`external_ready_timeout` or `external_artifact_incompatible`). It does not roll back
the kernel, activate the no-bundle fallback, retry indefinitely, or construct a
second adapter/runtime. Once a deferred module reaches ready or unavailable, its
registration is permanently closed. A capability provider removed during module or
full-runtime disposal is never replaced within that generation; navigation disposal
clears only navigation-scoped state and does not remove the document-runtime
provider needed by the next `NavigationSession`.

`ctx.signal` aborts pending preparation. `ctx.onDispose(fn)` is the only disposal
registration mechanism; a disposer registered after disposal runs immediately, and
one failing disposer does not prevent the rest. Each module may call
`ctx.afterCommit` at most once. A second call by a takeover module unwinds the
takeover barrier and commits `bundle_partial`; a second call by a deferred module
marks only that module unavailable. Direct-to-runtime takeover shares the bootstrap
deadline; post-paint agent takeover owns the separate deadline in §5.2.1. Deferred
modules do not start another global boot clock.

### 5.3 Bootstrap ownership

Bootstrap uses a generation-scoped state machine:

```text
unclaimed -> installing -> agent -> transferring -> kernel
                     |                         \-> fallback
                     |-> kernel (no agent)
                     \-> failed --------------------> fallback
```

Initial namespace capture is field-wise and does not replace a publisher-created
`window.tsjs` object: `window.tsjs ||= {}; tsjs.que ||= []; tsjs.boot ||= {}`. The
kernel remains externally inert and commits ownership only after all takeover
integration modules prepare and synchronously activate in order. The agent also
leaves the public runtime surface uncommitted. Deferred modules are not part of the
bootstrap/takeover transaction. Before first-display or takeover module work, bootstrap
normalizes `que` to one actual Array and defines the `tsjs.que` data
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
5. for a kernel commit, run every staged takeover `afterCommit` callback in takeover
   manifest order; fallback has none; and
6. drain the snapshot FIFO.

No browser task or microtask can interleave steps 1–6. Code intentionally invoked by
an `afterCommit` callback sees the complete API and committed queue. A callback is therefore either
in the snapshot or reaches the final executor through one of the two queue
references, never lost or invoked twice. A callback that pushes while the snapshot
drains executes immediately through the committed queue before draining continues;
one throw is isolated. Both ingress and final values satisfy
`Array.isArray(...) === true`. The old ingress identity remains a live forwarding
queue; the public `tsjs.que` identity changes exactly once at commit.

One ten-second watchdog begins immediately before the parser-blocking artifact and
covers agent registration/activation plus initial-action start, or direct-to-runtime
registration, preparation, and synchronous takeover activation when no agent is
selected. A preparation/activation failure, ABI mismatch, or deadline aborts the
installing generation, synchronously unwinds registered disposers in reverse order,
and then commits fallback. The completed agent batch uses the bounded per-attempt
deadlines in §4; persistent loading/takeover has its own fixed ten-second deadline
starting only after protected paint. A late artifact continuation that arrives after
fallback is rejected and quarantined. Deferred loading never starts before kernel
commit, so a deferred failure cannot select or replace fallback. Every late
continuation verifies its owner generation and self-discards.

The server-owned inline bootstrap controller is deliberately smaller than a runtime.
It installs only the queue ingress, immutable boot/manifest inputs, generation latch,
artifact error/watchdog observation, both release-internal registration sinks,
User Timing start mark, and terminal fallback commit. It owns no adapter, slot,
auction, renderer, integration feature, upstream loader, DOM scan, or publisher
callback execution before handoff. The server follows it with only any
first-display-required live upstream tags described in §5.2 and the one
parser-blocking agent-or-runtime tag. There are no separate integration-configuration
script tags or globals: all configuration is already inside the captured
`TsjsBootV1` value.

The old `gpt_bootstrap.js` asset and its initial-load hooks, handoff wrappers,
hydration scheduler, slot definition, targeting, display, and refresh are deleted
rather than retained as another runtime. The minimal controller/fallback is generated
from one TypeScript source, embedded by the server, included in the release hash and
its own §5.12 budget, and pinned by a staleness test; behavior is not hand-maintained
in both ES5 and TypeScript. On an agent page, later GPT/render behaviors run only after the persistent
runtime commits. This intentionally changes the missing/partial-artifact case: it no
longer attempts a best-effort GPT render through a duplicated degraded runtime and no
longer retains known-slot membership merely to report failure; it commits the empty
terminal fallback below.

The fallback is a terminal, non-rendering shell, not a reduced second runtime. Its
commit atomically records one immutable boot failure reason:

- `abi_mismatch` for invalid manifest shape, duplicate/unknown integration id, wrong
  release, invalid phase/catalog/source binding, duplicate registration,
  or incompatible ABI; or
- `bundle_partial` for a missing first-display/takeover module, preparation
  throw/rejection, activation/takeover throw, nonterminal TS state at the admission
  seal, or the owning deadline.

Before draining user work it installs `version:'1.0.0'`, the embedded `releaseId`, a
safe frozen `TsjsBootV1`, the final `tsjs.requestAds` input validator, the
validating-then-refusing `tsjs.addAdUnits`, the local `tsjs.log`, the
immediate-executor `tsjs.que`, a permanently refusing internal
`_registerIntegration`, and a frozen
`tsjs._internal` value containing only
`{state:'fallback',releaseId,reason,initialDisplayCommitted}`. It
constructs no runtime session, slot registry, GPT/Prebid adapter, bridge dispatcher,
timer, listener, port, or iframe. It never exposes a compatibility API.

The safe fallback boot uses the independently embedded release and critically checked
selected URLs in
`manifest:{version:1,releaseId,firstDisplay,runtimeSrc,integrations:[]}` and uses
`integrations:{version:1,entries:[]}` because fallback activates no integration. It
always substitutes exactly
`auctionProjection:{version:1,auction:{version:1,auctionId:'fallback',results:[]},slots:[],bids:[]}`
and the creative/diagnostics disabled safe defaults from §§5.4/5.8. The controller
does not retain or partially trust the server projection on a failure path and does
not import §§3.1–3.2 merely to improve fallback reporting. Consequently
`requestAds()` resolves `{slots:[]}`; each explicit valid id resolves
`slot_unresolved`, or `cancelled{reason:'caller_aborted'}` when its signal is already
aborted. Input-shape errors still reject with `RequestAdsInputError`. This deliberate
terminal-shell behavior is not a compatibility promise or a render recovery path.

After installing those surfaces, fallback drains the preexisting callback queue FIFO
exactly once with `this === tsjs`; one callback throw does not prevent later callbacks.
Subsequent `que.push(fn)` executes a callable immediately once and ignores non-callable
values. Every module registration is refused without invoking integration code, and
every late bundle continuation self-discards. Browser tests cover each failure
checkpoint, queued and later `requestAds`, callback throws, already-aborted signals,
and late bundles; no valid call remains pending.

### 5.4 Public surface after cutover

There are no compatibility aliases:

| Baseline surface                         | Final surface                                                                                                      |
| ---------------------------------------- | ------------------------------------------------------------------------------------------------------------------ |
| scattered `window.__tsjs_*` flags/config | `tsjs.boot.*`                                                                                                      |
| `tsjs.adSlots`/`tsjs.bids`               | initial `tsjs.boot.auctionProjection` including exact ordered placements; internal navigation projection after SPA |
| `tsjs.version === '0.1.0'`               | `tsjs.version === '1.0.0'` plus `tsjs.releaseId`                                                                   |
| `globalThis.tscreative`                  | no callable equivalent; automatic creative module                                                                  |
| `globalThis.tsCreativeConfig`            | `tsjs.boot.creative`                                                                                               |
| void/callback `requestAds`               | `tsjs.requestAds(options): Promise<RequestAdsResult>`                                                              |
| placeholder `renderAdUnit`               | `tsjs.requestAds({slots:[id]})`                                                                                    |
| placeholder `renderAllAdUnits`           | `tsjs.requestAds()`                                                                                                |
| generic mutable `setConfig`/`getConfig`  | immutable `tsjs.boot.*` plus typed integration config                                                              |
| `tsjs.renders`/`renderLog`/`renderSeq`   | `tsjs.diagnostics.renderTrace`                                                                                     |
| `window` event `tsjs:adRendered`         | `tsjs.diagnostics.renderTrace.subscribe(listener)`                                                                 |
| `tsjs.gptDiagnostics`                    | `tsjs.diagnostics.gpt`                                                                                             |
| `window.__tsjs_prebid_bundle`            | exact own `pbjs.__trustedServerArtifactV1` stamp                                                                   |
| integration install/patch sentinels      | kernel integration registry/`WeakSet`                                                                              |
| GPT slot expandos                        | `SlotRecord`                                                                                                       |

`window.tsjs.que` remains the pre-load command queue because it is the bootstrap
transport, not a legacy behavior alias.

The complete committed public API is the following union; the pre-load
`{que,boot,_registerIntegration}` transport and closure-private controller state are
not a committed API generation:

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
  readonly integrations: Readonly<IntegrationConfigsV1>
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
  /** Release-internal sink; true only for the exact active module load. */
  readonly _registerIntegration: (registration: unknown) => boolean
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
    initialDisplayCommitted: boolean
  }>
}

type TsjsApi = TsjsKernelApi | TsjsFallbackApi
```

`version` is the semantic public-API generation and changes only with a reviewed API
contract; `releaseId` identifies the exact bundle set and equals
`boot.releaseId`/`boot.manifest.releaseId`. The bootstrap parses, critically validates,
copies, and recursively freezes the sealed transport before effects. The selected
first-display or persistent owner completes domain validation before using projection
or integration values; fallback publishes only the independent safe boot above.
`boot.integrations` is immutable public inspection data,
not a mutable service locator: the composition root passes only the owning frozen
entry through each module's preparation/activation context. The hard cutover emits
no `window.__tsjs_*` integration-config value and deletes the old per-integration
bootstrap globals/templates in the same release; no module aliases or falls back to
them. `_internal` is a frozen, non-enumerable status
value; the service registry remains in the composition closure and is available to
integration modules only through those contexts during startup.

Fallback `initialDisplayCommitted` is `true` only when takeover failed after the
agent had already accepted at least one initial artifact; the fallback still reports
later TSJS operations unavailable and owns no artifact control. It is `false` for
every pre-display/bootstrap failure. This local status does not change a render
result, remove accepted DOM, or create a recovery path.

`_registerIntegration` is deliberately present only because separately downloaded
release-owned IIFEs need one handshake. During takeover installation it accepts the
next takeover registration from the server-composed runtime artifact. After kernel
commit it returns `true` only while an expected deferred script element created by
that kernel is the exact `document.currentScript`, and only for that element's
declared id, phase, and `releaseId`. It returns `false` without invoking supplied code
for publisher calls, unsolicited tags, wrong/replaced nodes, duplicates, terminal
modules, fallback, or disposal. It is not a general extension API, and neither it
nor `_internal` exposes readiness Promises, the broker, module state, or capability
objects.

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
- The sole GPT adapter marks TS-originated `display`/`refresh`/`destroySlots` calls
  with its closure-private reentrancy token and observes every other call as
  publisher-originated without changing its receiver, arguments, order, return, or
  throw. Its already-first `slotRequested` listener joins the exact physical object
  to that request-intent evidence. Before returning from a publisher-owned,
  competing, or ambiguous `slotRequested`, it invokes the §4.1 exact-once committed-
  overlay retirement; an exactly attributable TS replacement leaves the prior
  artifact visible until replacement commit. For publisher `destroySlots`, it
  snapshots the exact requested live objects before forwarding and retires their
  artifacts only after a successful return. Omitted-slot destruction snapshots all
  then-live GPT objects. False/throw does not claim destruction, and DOM-integrity
  retirement remains the backstop if GPT mutated the tree anyway.
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

- Preserve the rc `gam_attribution_enabled` behavior as one typed GPT boot field and
  one owner. It defaults to `false` independently of GPT enablement. When true, the
  parser-time GPT owner enqueues exactly one idempotent
  `googletag.setConfig({targeting:{ts:'true'}})` command before publisher GPT commands
  can issue a request; it does so even when `tsjs.adInit` already exists. If no
  server-projected batch selects the lean agent, the one parser-blocking persistent
  runtime performs that same obligation before publisher activity. The command
  applies to initial, lazy, publisher-owned, refreshed, and SPA-route requests in the
  document-local PubAds service. The slot-level `ts_initial` key keeps its existing
  lifecycle, and neither Prebid refresh nor SPA cleanup clears page-level `ts`.
  Targeting API absence or throw is isolated and cannot stop later queue work.
  Hard cutover deletes the old raw-bootstrap global flag, bundle activation
  attribute, and duplicate fallback enqueue; there is no dual marker path. Tests
  prove enabled-before-publisher order, disabled zero side effects, pre-existing
  `adInit`, takeover idempotence, refresh/SPA persistence, publisher-owned slots, and
  error isolation. Existing GAM reporting semantics and documentation remain; this
  design adds no router, key/value variant, beacon, telemetry schema, or analysis.
- The Prebid refresh policy preserves the exact `excluded_gam_ad_unit_path_suffixes`
  behavior: path matching is literal/case-sensitive suffix matching; missing,
  non-string, or throwing `getAdUnitPath()` fails open; stale TS/Prebid keys are
  cleared from every target; only eligible slots enter the synthetic auction; and
  the complete target slot list plus original options still reaches GPT.
- Use one adapter-level refresh interception and one slot-service request path;
  remove the three independent integration wrappers without removing handoff or
  exclusion semantics.
- Preserve the rc-baseline collapsed-shell resize as a guarded exception tied to the
  current attempt. It runs only after a TS PUC response is posted and only when the
  source is the exact connected iframe, width/height attributes and computed size
  are still at most one pixel, dimensions are finite/positive, the frame/wrapper are
  ordinary non-fixed/non-sticky display shells, and no anchor container is present.
  Only that iframe and its still-collapsed immediate wrapper may be resized.

### 5.8 Local diagnostics

The kernel owns one bounded, failure-isolated diagnostics ingress. Render attempts
and the GPT adapter call its closure-bound `publish(candidate: unknown): boolean`
only after their correctness transition. An active ingress accepts only a data tree
whose root is an ordinary or null-prototype record and whose descendants are null,
booleans, finite numbers, strings, dense arrays, or ordinary/null-prototype records.
Records must contain only own enumerable string data properties. Arrays must contain
only their exact own `length` plus dense own data elements `0..length-1`; extra
properties are invalid. Symbols, accessors, functions, `undefined`, bigint,
non-finite numbers, sparse arrays, custom prototypes, cycles, repeated object
references, and any proxy/trap failure are invalid. Runtime-local GPT slot identity
therefore crosses this boundary as a bounded string token, never as retained object
identity.

The executable ingress limits are:

```ts
const MAX_DIAGNOSTICS_OBSERVATION_DEPTH = 16
const MAX_DIAGNOSTICS_OBSERVATION_NODES = 512
const MAX_DIAGNOSTICS_PROPERTY_NAME_BYTES = 128
const MAX_DIAGNOSTICS_STRING_BYTES = 4096
```

The root has depth zero. Every encountered root, property value, or array element,
including a primitive, consumes one node, so a flat record with 511 scalar values is
the widest accepted record and one with 512 is rejected. Property-name and string
limits count UTF-8 bytes independently. At or below every limit, ingress builds a
fresh tree from own data descriptors, uses null-prototype objects for copied records,
deep-freezes the copy, and retains no producer-owned array or record. A limit,
descriptor, copy, encoding, or freeze failure returns `false` without invoking the
reducer and never throws into the source operation.

After a successful snapshot, ingress invokes the closure-private core trace reducer
exactly once, synchronously. Reducer or local error-reporter failure is caught and
cannot alter the source transition; because transport acceptance already succeeded,
`publish` returns `true`. The ingress has no integration-module or publisher
subscription API, listener identity, pending-delivery queue, scheduler/timer,
overflow callback, or subscription-capacity constant. Its exact returned facade is
frozen and contains only `publish` and `dispose`. `dispose` is idempotent, clears
retained owner callbacks, and makes the bound `publish` return `false`; a retained
facade from a disposed/replaced runtime epoch is likewise inert. Navigation-scoped
producers must pass their owner-generation check before publication, and the reducer
also ignores a semantically stale fact without treating diagnostics as authority.

The core-ingress representation of one physical GPT slot is the exact branded data
token `GptSlotTokenV1`. The sole GPT adapter owns its mint and no publisher input can
select it:

```ts
type GptSlotTokenV1 = string & { readonly __brand: 'GptSlotTokenV1' }
type GptTraceCycleOrdinalV1 = number & {
  readonly __brand: 'GptTraceCycleOrdinalV1'
}
const MAX_GPT_SLOT_TOKEN_ORDINAL = 4_294_967_295
const MAX_GPT_SLOT_TOKEN_BYTES = 11
const MAX_GPT_TRACE_CYCLE_ORDINAL = 4_294_967_295
const MAX_GPT_TRACE_CYCLES_PER_SLOT = 10
// Exact wire grammar: /^gt1_(?:[1-9a-z][0-9a-z]{0,6})$/ plus decoded value <= 0xffffffff.
```

The adapter starts a runtime-local unsigned ordinal at one and emits `gt1_` plus its
lower-case canonical base-36 form, with no leading zero. It increments only after a
successful mint, stores the string beside the adapter's private opaque identity in
the `WeakMap` entry for that exact GPT slot object, and copies it into any runtime
`SlotRecord` that adopts that object as its exact optional own-data field
`readonly traceToken?: GptSlotTokenV1`. Re-observing or handing off the same physical
object returns the same string. A newly defined/replacement object receives a new
ordinal even when it has the same element id, ad-unit path, registered slot id, or
publisher owner. Ordinals are never reused within one runtime; physical destruction,
retirement, navigation disposal, map pruning, and garbage collection do not rewind
the counter. Runtime disposal clears the weak/map state and makes every old publisher
inert before a later runtime may begin again at one.

The adapter's `gpt.events.v1` facts retain their separate frozen opaque object token
because that direct bounded stream never crosses the generic ingress and takeover
GPT diagnostics needs same-object identity. Before publishing a GPT fact to
`trace.v1`, the GPT integration creates a data-only projection that replaces the
opaque token with the exact own-data field
`slot:{token:GptSlotTokenV1,cycleOrdinal:GptTraceCycleOrdinalV1,elementId?:string}`;
no object-valued identity enters the snapshot. Neither identity is public or render
authority.

The adapter owns a separate unsigned trace-cycle ordinal in each physical object's
`WeakMap` state. The first unambiguous `slotRequested` for that object mints one and
each later unambiguous physical request cycle increments it. It starts at one,
increments only after the sole lifecycle adapter has opened that exact cycle, never
wraps or reuses a value for the object, and survives publisher handoff. Every
projected value must be an integer from 1 through 4,294,967,295; the reducer rejects
zero, fractions, non-finite values, and larger values. A duplicate or overlapping
`slotRequested` that the §2.4 lifecycle owner cannot attribute opens no trace cycle
and emits no trace projection. Exhaustion at 4,294,967,296 latches new trace-cycle
projection unavailable for only that physical object; its GPT lifecycle and
`gpt.events.v1` stream continue unchanged.

The adapter retains at most ten trace-cycle records per physical object, keyed by
the ordinal, with request state, optional `responseIdentifier`, and per-event seen
state. At most one cycle is open. Before opening another, it prunes the oldest
completed/retired record only when the ten-record cap is full; it never evicts an
open record. Pruning sets a permanent `unknownPriorCycle` latch for that physical
object; the latch retains no old id but participates in future ambiguity checks and
is cleared only when that object/runtime is disposed.

A non-request GPT fact receives a cycle ordinal only from the §2.4 lifecycle owner's
exact already-attributed cycle handle, or when response identity and retained
per-event state leave exactly one eligible record and `unknownPriorCycle` cannot be
the source. It is never assigned to the newest cycle by timing, element id, or slot
token alone. A late `slotResponseReceived`, `slotRenderEnded`, `slotOnload`,
`impressionViewable`, or `slotVisibilityChanged` that could belong to both a prior
cycle and a newer started/completed cycle is ambiguous and produces no core-ingress
projection. A uniquely matched old fact keeps the old ordinal even after a newer
cycle starts. Eviction makes an otherwise unmatched later fact a diagnostics-only
drop rather than a candidate for the current cycle; no callback can recreate an
evicted ordinal. Destroy, redefine, and navigation disposal retire all retained
cycles for the affected physical object, while runtime disposal clears the weak
state.

If the slot-token ordinal is exhausted, token/cycle construction or validation
fails, or an injected test mint collides, the adapter emits no affected core-ingress
projection and continues the GPT operation plus `gpt.events.v1` delivery unchanged.
Already minted slot tokens and unambiguous cycles on other objects remain usable.
The failure is reported at most once per failure class through the local logger and
cannot fail a display, handoff, destroy, refresh, or diagnostics callback.

On the first accepted `slotRequested` projection, the core trace reducer resolves
the projection's bounded element id to exactly one current registered slot and keys
its 256-entry diagnostics-only physical-impression map by the exact pair
`{token,cycleOrdinal}`. The binding is
`{slotId,navigationGeneration,baselineSeq?,historySeq?,state}` where `state` is
`open`, `completed`, or `retired`. An unresolved, ambiguous, duplicate-active,
stale-generation, or over-capacity binding is dropped without evicting an open entry.
Later facts join only by the exact pair; token alone, element id, ad-unit path, and
registered slot id are never substitutes. `slotRenderEnded` stores the exact created
or enriched `historySeq` and completes the binding. Handoff of the same physical
object keeps its token but each refresh receives a distinct cycle binding;
redefine/replacement changes both the physical token and its per-object cycle
sequence. Destroy or navigation disposal retires affected bindings. A late fact for
a uniquely retained retired pair may enrich only its already-recorded, still-retained
old `historySeq`; it cannot create a row, rebind to a new cycle/generation, or mutate
new `current` state. Completed/retired bindings are pruned oldest-first before
admitting a new binding, open bindings are never evicted, and runtime disposal clears
the map. Exhaustion, collision, stability, handoff, repeated refresh, replacement,
retirement, late-event, and navigation behavior remain diagnostics-only and never
participate in GPT lifecycle authority.

The takeover `gpt_diagnostics` module does not subscribe to that ingress. It consumes
only the separately bounded GPT-owned `gpt.events.v1` fact stream. The broker values
for `trace.v1` and `trace.presentation.v1` are different frozen exact interfaces:
`trace.v1` contains correctness fact operations and ingress publication but no
presentation attachment, while `trace.presentation.v1` contains only
`attachPresentation`. The release catalog grants the latter only to deferred
`diagnostics_presentation`; APS, GPT, `gpt_later`, publishers, and the public API
cannot obtain it.

The private capability has this complete interface; no other own key is permitted:

```ts
interface RenderTracePresentationSourceV1 {
  current(): Readonly<Record<string, Readonly<RenderTraceRecord>>>
  history(): readonly Readonly<RenderTraceRecord>[]
  subscribe(listener: () => void): () => void
}

interface RenderTracePresentationControlsV1 {
  dispose(): void
}

type RenderTracePresentationFactoryV1 = (
  source: Readonly<RenderTracePresentationSourceV1>
) => Readonly<RenderTracePresentationControlsV1>

interface TracePresentationCapabilityV1 {
  attachPresentation(factory: RenderTracePresentationFactoryV1): () => void
}
```

`attachPresentation(factory)` is synchronous and admits at most one live attachment.
A non-callable factory, a reentrant/duplicate call, or a call after trace-owner
disposal throws `TypeError` without disturbing an existing attachment. Callability
is checked before attachment state, so a non-callable duplicate still leaves the
first attachment untouched. The callable receives one exact frozen source with only
`current`, `history`, and `subscribe`. While attached, `current()` and `history()`
return the same frozen copies as the public trace API. After detach/owner disposal,
retained source methods are inert: `current()` returns a frozen null-prototype empty
record, `history()` returns a frozen empty array, and no runtime state is retained.

`source.subscribe` checks listener callability first and throws `TypeError` for a
non-callable listener. It then requires the attachment to be attaching or live and
requires that no private listener is already live; either failure throws `TypeError`
without replacing or unsubscribing the first listener. Success returns an idempotent
zero-argument unsubscribe function that removes only that exact listener. A later
subscribe may succeed after it unsubscribes while the attachment remains live; a
retained subscribe call after detach/owner disposal throws `TypeError`, and a retained
unsubscribe is a no-op. The factory must end its synchronous call with one live
listener, perform its initial snapshot render in the attaching task, and return an
exact frozen own-data `{dispose}` controls object. No presentation callback runs
during attachment, so that initial snapshot precedes all live delivery.

If the factory throws, returns malformed controls, or returns without a live
listener, attachment rolls back the candidate listener and scheduled work, invokes
an own callable candidate `dispose` once when safely available, reports any cleanup
failure locally, and rethrows so only the deferred module transaction fails. The
attachment slot is then reusable. Once attached, every trace commit updates the
data store first and coalesces presentation notification into at most one owned
zero-delay task. The callback's return value is ignored; a throw is isolated and
reported, and the next commit can schedule again. The returned detach function is
idempotent: it invalidates the attachment generation, cancels pending work, clears
the private listener, and invokes controls `dispose` exactly once. Trace-owner
disposal performs the same sequence. Late scheduler callbacks and retained source,
unsubscribe, detach, or controls references are inert. This private attachment is
not counted against the 32 live subscribers per public diagnostics surface.

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
check. The production core commits and freezes one stable `tsjs.diagnostics` facade
with the kernel. Correctness facts required from the first display, bounded snapshot
state, subscriptions, and the final public diagnostics APIs are produced by the
takeover lifecycle/GPT path after adopting initial facts from the handoff. DOM
presentation, badges, overlay, formatting, and
clipboard/download interaction may attach behind that facade after its deferred
module commits. Deferred diagnostics failure leaves the facade safe and bounded and
cannot affect rendering. Fallback exposes no diagnostics namespace because it
constructs no runtime.

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
`abi_mismatch`, not silent diagnostics disablement. The bootstrap copies the validated
data and recursively freezes that copy before agent/takeover preparation; copy/freeze
failure is also `abi_mismatch`. `gpt.active:true` requires exactly one catalogued
takeover GPT-diagnostics collector/public API in `BootManifestV1`; `false` requires
that collector to be absent. Exactly one catalogued deferred diagnostics-presentation
module is required iff `renderTraceOverlay || gpt.active`; it replays bounded facts
into the enabled overlay/badge/export-interaction model. The inverse mismatch is also
`abi_mismatch` before any GPT diagnostics listener, buffer, or presentation module
exists.

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

The GPT diagnostics integration consumes raw facts from the sole GPT adapter rather
than registering another control wrapper. This document is the complete normative
contract; the earlier GPT-diagnostics design is historical rationale only.

```ts
type GptDiagnosticsCallbackKind =
  | 'slotRequested'
  | 'slotResponseReceived'
  | 'slotRenderEnded'
  | 'slotOnload'
  | 'impressionViewable'
  | 'slotVisibilityChanged'

type GptDiagnosticsCallbackDisposition = 'matched' | 'unmatched' | 'ambiguous'
type GptDiagnosticsSize = readonly [number, number]

type GptDiagnosticsBindingReason =
  | 'missing_slot_element_id'
  | 'missing_element'
  | 'duplicate_dom_id'
  | 'dom_uniqueness_unverifiable'
  | 'duplicate_gpt_slot_id'

interface GptDiagnosticsBinding {
  readonly status: 'bound' | 'unbound' | 'ambiguous'
  readonly reason?: GptDiagnosticsBindingReason
}

interface GptDiagnosticsDurations {
  readonly requestToResponseMs?: number
  readonly responseToRenderMs?: number
  readonly requestToRenderMs?: number
  readonly renderToLoadMs?: number
  readonly renderToViewableMs?: number
}

interface GptDiagnosticsAdManagerIdentity {
  readonly lineItemId?: number
  readonly creativeId?: number
  readonly campaignId?: number
  readonly advertiserId?: number
  readonly sourceAgnosticLineItemId?: number
  readonly sourceAgnosticCreativeId?: number
  readonly yieldGroupIds?: readonly number[]
  readonly companyIds?: readonly number[]
}

type GptDiagnosticsResponseClass =
  | 'empty'
  | 'backfill'
  | 'reservation'
  | 'unclassified_non_empty'

type GptDiagnosticsRequestPath =
  | 'trusted_server_direct'
  | 'prebid_refresh'
  | 'publisher_refresh'
  | 'competing'
  | 'unattributed'

type GptDiagnosticsTrustedServerOpportunity =
  | 'renderable_candidate'
  | 'unrenderable_candidate'
  | 'no_candidate'

type GptDiagnosticsCreativeFailure =
  | 'missing_render_source'
  | 'cache_fetch_failed'
  | 'invalid_cache_payload'
  | 'response_post_failed'

type GptDiagnosticsDelivery =
  | 'trusted_server_response_sent'
  | 'trusted_server_selected'
  | 'candidate_unconfirmed'
  | 'no_candidate'
  | 'unknown'
  | 'pending'
  | 'not_applicable'

interface GptDiagnosticsRequestCycle {
  readonly requestNumber: number
  readonly requestedAtMs?: number
  readonly responseAtMs?: number
  readonly renderAtMs?: number
  readonly loadAtMs?: number
  readonly viewableAtMs?: number
  readonly durations: Readonly<GptDiagnosticsDurations>
  readonly requestedSlotSizes?: readonly GptDiagnosticsSize[]
  readonly size?: GptDiagnosticsSize
  readonly observedSlotSize?: GptDiagnosticsSize
  readonly isEmpty?: boolean
  readonly isBackfill?: boolean
  readonly slotContentChanged?: boolean
  readonly incompleteSequence: boolean
  readonly adManager?: Readonly<GptDiagnosticsAdManagerIdentity>
  readonly responseClass?: GptDiagnosticsResponseClass
  readonly requestPath?: GptDiagnosticsRequestPath
  readonly requestIntentId?: number
  readonly trustedServerAuctionId?: string
  readonly opportunityToRequestMs?: number
  readonly replacedRequestNumber?: number
  readonly previousRenderToRequestMs?: number
  readonly creativeChanged?: boolean
  readonly previousCreativeId?: number
  readonly loadObservedBeforeRender?: boolean
  readonly trustedServerOpportunity?: GptDiagnosticsTrustedServerOpportunity
  readonly trustedServerCreativeRequestAtMs?: number
  readonly trustedServerCreativeResponseAtMs?: number
  readonly trustedServerCreativeFailures?: readonly GptDiagnosticsCreativeFailure[]
  readonly delivery?: GptDiagnosticsDelivery
}

interface GptDiagnosticsSlotExport {
  readonly runtimeSlotNumber: number
  readonly slotElementId?: string
  readonly adUnitPath?: string
  readonly binding: Readonly<GptDiagnosticsBinding>
  readonly currentVisibilityPercentage?: number
  readonly maximumVisibilityPercentage?: number
  readonly requests: readonly Readonly<GptDiagnosticsRequestCycle>[]
}

interface GptDiagnosticsCallbackIssue {
  readonly kind: GptDiagnosticsCallbackKind
  readonly runtimeSlotNumber: number
  readonly slotElementId?: string
  readonly timestampMs: number
  readonly disposition: GptDiagnosticsCallbackDisposition
  readonly reason: GptDiagnosticIssueReasonV1
}

type GptDiagnosticsAttributionIssueReason =
  | 'creative_request_without_slot'
  | 'creative_request_without_cycle'
  | 'creative_request_ambiguous_cycle'
  | 'creative_request_on_empty_cycle'
  | 'creative_attempt_capacity'
  | 'creative_attempt_unknown'
  | 'creative_attempt_expired'
  | 'creative_attempt_evicted'

interface GptDiagnosticsAttributionIssue {
  readonly reason: GptDiagnosticsAttributionIssueReason
  readonly timestampMs: number
  readonly runtimeSlotNumber?: number
  readonly slotElementId?: string
}

interface GptDiagnosticsCoverageCounters {
  readonly observed: number
  readonly matched: number
  readonly unmatched: number
  readonly ambiguous: number
}

interface GptDiagnosticsExportV1 {
  readonly version: 1
  readonly capturedAt: string
  readonly page: Readonly<{ origin: string; pathname: string }>
  readonly slots: readonly Readonly<GptDiagnosticsSlotExport>[]
  readonly callbackIssues: readonly Readonly<GptDiagnosticsCallbackIssue>[]
  readonly attributionIssues: readonly Readonly<GptDiagnosticsAttributionIssue>[]
  readonly coverage: Readonly<
    Record<GptDiagnosticsCallbackKind, Readonly<GptDiagnosticsCoverageCounters>>
  >
  readonly metadata: Readonly<{
    droppedCallbacks: number
    droppedAttributionIssues: number
    evictedSlots: number
    evictedRequestCycles: number
  }>
}

interface GptDiagnosticsApi {
  snapshot(): Readonly<GptDiagnosticsExportV1>
  export(): void
  subscribe(
    listener: (snapshot: Readonly<GptDiagnosticsExportV1>) => void
  ): () => void
  show(): void
  hide(): void
}
```

All arrays, tuples, nested objects, and the root returned by `snapshot()` are fresh
recursively frozen copies. `capturedAt` is a valid UTC ISO timestamp generated for
that snapshot; `page` contains only the current origin and pathname, never query,
fragment, referrer, cookies, targeting, bid payload, or creative markup. Optional
fields mean the fact was unavailable or inapplicable, never that a false/zero value
was elided. Public counters are nonnegative safe integers and saturate rather than
wrap; timestamps/durations/percentages and exported numeric identifiers are finite
and preserve zero. The store normalizes and bounds strings/arrays before retention,
and every snapshot reuses only copied primitive data.

The frozen `tsjs.diagnostics.gpt` API exists iff `diagnostics.gpt.active` is true.
Before the deferred presentation attaches, `show`, `hide`, and `export` are safe
no-ops; they do not queue a later UI action. After attachment, `show`/`hide` control
only the Shadow DOM overlay and `export()` downloads the current snapshot as local
JSON using a short-lived object URL that is always revoked; none performs a network
request or changes ad state. The internal evidence recorder is a closure-private
`gpt.events.v1` consumer and is never a property of this API.

The buffer does not retain GPT event objects or arbitrary publisher data. Before
admission, the current owner normalizes each observation to one exact ordinary-data
`FirstDisplayGptFactV1` record:

```ts
type GptDiagnosticEventV1 =
  | 'slotRequested'
  | 'slotResponseReceived'
  | 'slotRenderEnded'
  | 'slotOnload'
  | 'impressionViewable'
  | 'slotVisibilityChanged'

type GptDiagnosticDispositionV1 = 'matched' | 'unmatched' | 'ambiguous'

type GptDiagnosticIssueReasonV1 =
  | 'no_request_cycle'
  | 'overlapping_request_cycles'
  | 'unknown_prior_cycle'
  | 'invalid_event_order'

interface FirstDisplayGptFactV1 {
  readonly version: 1
  readonly event: GptDiagnosticEventV1
  readonly token: GptSlotTokenV1
  readonly runtimeSlotNumber: number
  readonly cycleOrdinal: GptTraceCycleOrdinalV1 | null
  readonly disposition: GptDiagnosticDispositionV1
  readonly issueReason: GptDiagnosticIssueReasonV1 | null
  readonly capturedAtMs: number
  readonly elementId: string | null
  readonly adUnitPath: string | null
  readonly requestedSlotSizes: readonly (readonly [number, number])[] | null
  readonly isEmpty: boolean | null
  readonly renderedSize: readonly [number, number] | null
  readonly isBackfill: boolean | null
  readonly slotContentChanged: boolean | null
  readonly visibilityPercent: number | null
}

const MAX_FIRST_DISPLAY_GPT_FACTS = 512
const MAX_FIRST_DISPLAY_GPT_FACT_BYTES = 1000
const MAX_FIRST_DISPLAY_GPT_FACT_SECTION_BYTES = 524_288
const MAX_FIRST_DISPLAY_GPT_FACT_COUNTER = 4_294_967_295
```

All shown keys are required and no other own key is permitted. The GPT adapter mints
`runtimeSlotNumber` once per exact physical slot from the global unsigned 32-bit
trace-slot ordinal already transferred in `FirstDisplayHandoffV1`; it is nonzero,
never reused, and remains paired with that physical token through takeover.
`capturedAtMs` is a finite nonnegative `performance.now()` value; `elementId` and
`adUnitPath` are null or the adapter's copied own 1–256-UTF-8-byte values with no
NUL/control characters. GPT getter throws, nonstrings, empty values, and oversized
values normalize to null. `requestedSlotSizes` is non-null only for the matched
`slotRequested` fact carrying Trusted Server request-intent evidence, uses the
16-entry copied/frozen 1–4096 contract below, and is null for publisher/native or
event-inapplicable facts. Rendered dimensions are null or integral 1–4096 values;
visibility is null or finite 0–100. Event-inapplicable fields are null. A matched
request fact has its newly minted cycle ordinal; later cycle-bound facts use the exact
uniquely attributed ordinal. A matched
`slotVisibilityChanged` fact may use `cycleOrdinal:null` because visibility is
slot-level; any deliberately unmatched/ambiguous fact uses `cycleOrdinal:null`.
`disposition` is exclusively the callback-coverage dimension. `issueReason` is null
when the fact adds no issue; otherwise it is the exact independent sequence/matching
reason, so a uniquely correlated invalid-order callback remains `matched` with
`issueReason:'invalid_event_order'`. Unmatched and ambiguous facts use the applicable
`no_request_cycle`, `overlapping_request_cycles`, or `unknown_prior_cycle` reason.
The token and cycle retain the grammars/caps above.
On replay, `requestedSlotSizes` becomes the public request cycle's field and
`renderedSize` becomes its GPT-reported `size`; the names remain distinct at the
handoff and public boundaries.
The canonical UTF-8 encoding of the complete normalized record must be at most 1,000
bytes. The handoff subsection is exactly
`{facts:readonly FirstDisplayGptFactV1[],overflowCount:number,dropCount:number}`;
both counters are unsigned 32-bit integers that saturate at their maximum. Its
complete canonical UTF-8 encoding, including array/object punctuation, keys, and
counters, must be at most 524,288 bytes. Invalid, oversized, accessor-backed, or
unnormalizable observations are diagnostics-only drops and never enter the buffer or
affect GPT/render authority. A valid observation that would exceed the 512-entry
FIFO evicts the oldest fact and increments `overflowCount`; any normalization or
section-byte-cap refusal increments `dropCount`. Counter saturation does not affect
admission or takeover.

When `gpt.active` is true, the current `ActiveRenderOwner` installs the six
documented GPT observations (`slotRequested`, `slotResponseReceived`,
`slotRenderEnded`, `slotOnload`, `impressionViewable`, and
`slotVisibilityChanged`) before any TS-owned GPT request. It owns one 512-entry FIFO
pre-collector fact buffer. Overflow evicts the oldest fact and increments one
diagnostics-only counter. On a direct-to-runtime page, persistent core creates this
buffer and replays it in order when the takeover diagnostics collector activates.
On an agent page, the agent creates it before the protected batch, maps every fact to
the canonical trace token/cycle owned by that epoch, and includes the final ordered
normalized `FirstDisplayGptFactV1` records plus overflow/drop counts in
`FirstDisplayHandoffV1`. That complete at-most-524,288-byte subsection occupies the
separate 512 KiB diagnostics allowance of the handoff's 8.5 MiB total cap.
Normalization and the FIFO enforce that bound before handoff; a fact that cannot fit is a counted
diagnostics-only drop and cannot consume replay-suppression or correctness space.
During the non-yielding takeover task, the agent disconnects its six listeners; the
persistent GPT adapter adopts the exact physical slot identities from the capsule,
reconstructs its private identity map from the transferred canonical tokens,
runtime-slot numbers, ad-unit paths, cycles, and next ordinal, replays the bounded
facts into the collector in original order, and installs one fresh six-listener set
before the task ends. Replay updates the same slot/cycle facts, callback-coverage
counters, and separately reasoned issue rows that continuous ownership would have
produced; it cannot renumber a slot or reinterpret a disposition as an issue. A GPT
callback can therefore run before or after, but never inside, the
owner transition. It is processed once by one epoch and no diagnostic fact is
duplicated. After replay, live facts fan out directly and the pre-collector buffer is
released. The later presentation module consumes the already bounded collector/store;
it never registers GPT listeners or recreates the raw-fact buffer.

When inactive, neither epoch creates a diagnostics buffer or the four
diagnostics-only listeners. The sole current GPT adapter may still own
`slotRequested` and `slotRenderEnded` listeners required for ordinary ad correctness
under §5.7; inactive zero-side-effect tests measure that baseline and require zero
diagnostics-added listeners, DOM, timers, observers, API, or network work. At every
task boundary, active and inactive pages alike have listener ownership in exactly one
epoch.

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

Each exported `GptDiagnosticsRequestCycle` keeps three size concepts distinct:

- `requestedSlotSizes?: readonly (readonly [number,number])[]` is the exact
  Trusted Server placement-format evidence associated with that request intent,
  capped at 16 entries. Each dimension is an integer in 1–4096. Admission copies and
  freezes every tuple and the outer list; malformed entries are omitted and an empty
  result is absent. Publisher/native requests do not inherit stale TS evidence.
- `size?: readonly [number,number]` is GPT's reported filled creative size from the
  exact matched `slotRenderEnded`. It retains the existing GPT normalization and is
  never inferred from DOM geometry.
- `observedSlotSize?: readonly [number,number]` is the finite, nonnegative CSS-pixel
  width/height of the uniquely bound outer slot element measured after an explicitly
  nonempty rendered cycle. Fractions and zero are valid observations; this is layout
  evidence, not creative size or render acceptance.

When GPT diagnostics is active, one dedicated slot-size observer subscribes to the
bounded store and binding manager. It observes only connected, uniquely bound
elements whose latest cycle is nonempty and has `renderAtMs`. Every scheduled
measurement captures `{runtimeSlotNumber,requestNumber,element}` and revalidates the
same exact binding, connected element, latest cycle, nonempty result, and render fact
before committing. A refresh, rebind, ambiguity, disconnection, eviction, navigation,
or disposal therefore makes a late measurement inert. `ResizeObserver` updates and
an initial animation-frame measurement share this guard; absence of
`ResizeObserver` leaves the initial measurement only. Disposal unsubscribes both
sources, disconnects the observer, and makes scheduled callbacks inert.

Snapshots, subscriptions, overlay, badges, and export use the exact field names and
label the values independently as `Requested`, `GPT fill`, and `Outer box`; no UI or
consumer may collapse them into one ambiguous “size.” Copy/freeze rules prevent a
caller from mutating stored tuples. Focused tests cover all three present together,
malformed/capped requested formats, refresh and rebind races, unavailable observers,
fractional/zero boxes, unchanged-measurement deduplication, and teardown.

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
replaces the two guard booleans. The `creative_initial` slice and `phase:'takeover'`
creative module are required iff `enabled && (clickGuard || renderGuard)`; an enabled
configuration with both guards false has no browser module because it has no browser work. `enabled:false`
also requires its absence. An accessor, non-plain
prototype, missing/unknown key, wrong literal/version/type, disabled non-false guard,
or manifest mismatch is an `abi_mismatch` before any creative guard installs. The recursively
frozen `tsjs.boot.creative` is the only final inspection/configuration surface.
`globalThis.tscreative`, `globalThis.tsCreativeConfig`, `installGuards`, `setConfig`,
and `getConfig` are deleted, not aliased; changing guard policy requires a new boot/
document generation.

The initial slice installs the selected compare-restorable guard before publisher
creative activity. It transfers only bounded configuration/seen-node facts; §5.2.1
compare-restores the provisional wrapper/observer, installs one fresh persistent
guard, and performs the bounded post-commit rescan. The creative takeover module
prepares inertly and activates transactionally exactly once in the kernel barrier.
Activation installs directly on a no-agent page and never stacks an agent guard. It enables the click guard when
`clickGuard` is true and the image/iframe dynamic-source guards when `renderGuard` is
true, but performs no rc-baseline DOM rewrite. Only when
`clickGuard || renderGuard` is true, activation gives a still-loading document one
owned `DOMContentLoaded` callback to perform the rc-baseline idempotent rescan after the
initial DOM completes; an already interactive/complete document gets no listener and
performs that scan from one staged `afterCommit` callback. Its
disposer removes that listener, observers, and owned DOM state and compare-restores a
patched constructor/property/function only if the current value is still the exact
wrapper installed by this generation. An absent creative module installs no wrapper,
observer, listener, scan, or DOM state when the integration is disabled or enabled
with both guard booleans false. SPA navigation retains the document-scoped
guards and their dynamic-node behavior; failed preparation/activation or full runtime
disposal removes them once.

Creative processing keeps its current independent policy controls. Auction
sanitization remains explicit opt-in/default-off; rewriting retains its existing
setting/default and still runs on every delivery path where that setting applies.
When rewriting injects browser guards into an independent creative document, the
server emits a complete document-local boot controller followed by exactly one
content-addressed direct persistent artifact containing core, `render_runtime`, and
`creative`—and no publisher-page integration, agent slice, or deferred module. The
tag is the sole `script#trustedserver-js` and uses the same release identity as the
page runtime. Since no projected batch exists in that document, there is no agent,
attempt, or synthetic paint trigger. If creative is disabled or both guards are
false, rewriting injects no TSJS boot or artifact. A body-less fragment receives the same pair once at its
start; a document body receives it once at the start of the body. Boot construction
failure rejects rewriting rather than emitting a script-only or unauthenticated
creative.
The runtime click guard resolves and stores one validated absolute HTTP(S) URL before
navigation, rejects `javascript:`, `data:`, `blob:`, malformed, and credentialed
targets, and uses the established `/first-party/proxy-rebuild` GET redirect path to
recover clicks from the opaque sandbox. Dynamic resource/click rewriting, iframe
sandbox attributes, font/CORS handling, body/base handling, and the direct/SSAT/cache
delivery boundaries remain covered by unit plus real-browser tests. This APS/TSJS
work neither enables sanitization nor broadens creative privileges.

Every other enabled TSJS integration becomes a thin transactional integration module without an
internal feature rewrite. Its complete current unit suite runs unchanged against a
rc-baseline fixture and a module-composed fixture. At minimum the parity corpus
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
integration's matcher, reorder another integration's first-display/takeover startup, stack interception,
or leave a timer/listener after module disposal. Maximal-bundle tests load every
server-declared integration module through its real phase/trigger and deterministic
catalog-order initiation. They allow independent deferred completion order and
assert both behavior and exactly-once disposal, not merely successful registration.

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

| Current area                         | Target responsibility                                                                                                                                                                            |
| ------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| production `composition/browser`     | deleted as a catch-all root; first-display composition owns only §5.2.1 slices, while persistent core constructs kernel/session/broker, API, diagnostics facade, and direct-auction coordination |
| `first_display/**`                   | fixed agent coordinator plus exact initial APS/GPT/Prebid/creative/parser-time slices, handoff serializer/capsule, protected paint, and no public runtime API                                    |
| test composition seams               | separate unshipped entry containing fake/no-op adapters, schedulers, corpus hooks, and `*ForTest` accessors                                                                                      |
| `gpt/index.ts`                       | persistent GPT owner that can adopt exact initial slot/cycle identities; the separate `gpt_initial` slice owns only the immutable projected request and transfer facts                           |
| `prebid/index.ts`                    | persistent Prebid owner that adopts initial artifact/queue/admission facts; the separate `prebid_initial` slice owns only initial readiness, bidder/user-ID/EID setup, and PUC admission         |
| `prebid/later.ts`                    | deferred synthetic refresh and GAM-path exclusion only; it owns no initial admission, artifact-readiness, bidder, user-ID, EID, or publisher-queue behavior                                      |
| `core/request.ts`                    | public validation, immutable selection, and thin `AuctionBatch` coordination; path implementations live behind injected capabilities                                                             |
| `core/render.ts`                     | only minimum path-independent first-display DOM/lifecycle helpers; APS/ADM live with their owner, while cache stays the rc-baseline GPT-integration implementation                               |
| `kernel/diagnostics.ts`              | bounded data-tree snapshot ingress and one closure-private reducer callback; no integration subscriptions, pending queue, scheduler, timer, or presentation authority                            |
| `core/trace.ts`                      | bounded correctness-fact reducer/store, public snapshots/subscriptions, and separately attenuated `trace.presentation.v1`; no DOM presentation code                                              |
| render-reservation maps in globals   | bounded initial capability owned by `render_owner_initial` and bounded persistent capability owned by the render service; APS contributes only descriptor/top-mount state                        |
| diagnostics overlay/UI               | deferred owner of the sole private `trace.presentation.v1` attachment; never imported by production core or correctness producers                                                                |
| duplicated `script_guard.ts`         | small per-integration factory compiled into the owning module; no central production root imports every matcher                                                                                  |
| optional integration implementations | remain in their integration IIFEs and register inert factories; they are absent from core bytes                                                                                                  |

The first-display and later slices of one product integration must retain the same
observable ownership and disposal contract. Splitting files or artifacts cannot
omit a correctness-required listener from its first-display slice, duplicate an adapter,
or turn a bounded typed failure into a readiness hang. Conversely, code used only
for refresh, later navigation, diagnostics presentation, test injection, or an
optional integration cannot remain in the first-display dependency graph for convenience.

Source may be shared at authoring time only through effect-free leaf contracts whose
metafile contribution fits the agent budget. The first-display build must not import
the persistent core, capability broker, generic slot/auction/trace registries, later
GPT/Prebid module, or any deferred entry. It may use a dedicated compact parser and
state machine generated from the same neutral schema/corpus; parity tests, rather
than a production import from the persistent implementation, prevent drift. The
persistent build likewise imports no agent coordinator or provisional singleton.

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
`useUnknownInCatchVariables`. Production bundles contain no dynamic `import()`;
the deferred loader uses only authenticated classic same-origin script elements as
specified in §5.2.

The first complete paired run on candidate
`a0d8c0631a9774b5c8da8b25794a8836aa2e62f5` against exact rc base
`d4cd2cc823718d64ae73bcb068e5eab03ecd901a` proved that correctness, load ordering,
heap, GPT first-action timing, and every APS absolute action/completion/paint deadline
passed, while semantic pre-action transfer still failed. The candidate GPT interval
was 92,931 raw / 28,802 gzip / 25,779 Brotli bytes against 70,943 / 21,571 / 18,804;
the APS interval was 128,769 / 39,645 / 34,758 against the allowed rc × 1.10 values
78,659.9 / 24,098.8 / 21,006.7. APS first-action p90 was 616.3 ms against an allowed
562.0 ms, while its 900 ms absolute action ceiling and every downstream deadline
passed. This is immutable diagnostic evidence, not authority to change a comparator,
membership rule, fixture, threshold, or baseline.

Revision 43 closes that demonstrated source-graph gap without changing network
semantics:

1. the inline bootstrap uses the compact server-sealed JSON transport above and its
   production graph cannot reach the full object-form boot, auction-projection, or
   generic integration-carrier validators;
2. `render_owner_initial` owns one source-neutral PUC/render journal and lifecycle
   primitive set for ADM and APS reservations, with no duplicate full bridge, while
   APS-only descriptor, nonce, top-mount, and document behavior stays in
   `aps_initial` and absent from non-APS masks;
3. the checked-in PUC dynamic owner remains the exact self-contained v4 response
   program but is compacted in place, with no external asset or reduced adversarial
   behavior; and
4. source-ownership tests, focused lifecycle suites, every admitted-mask budget,
   semantic rc comparison, and the complete paired performance run must all pass on
   one clean pushed replacement SHA.

An optimization is rejected if it merely moves bytes into an auxiliary request,
stores executable source in boot data, weakens the top-page owner, removes mixed
APS/ADM support, reopens a handoff/retirement race, or relies on unsafe property
mangling across independently built protocol components.

The checked-in pre-change fixture is immutable historical evidence and is never
regenerated or rewritten. Its original `bundles` values measured a different
artifact model: minimal contained only the old core, reference omitted the now-
mandatory render owner, and maximal contained thirteen unsplit files. Those values
cannot be used as like-for-like ceilings or any other pass/fail decision. Their raw,
gzip, Brotli, provenance, and historical deltas remain report-only diagnostic
evidence.

The existing `roleCorrectTransfer` subtree records the first role-correct capture
from the exact clean, pushed parent after Task 18D. Review established that this was
an oversized intermediate implementation, so its provenance and bytes stay
immutable but its self-derived 5% ceilings are not release acceptance. After
mechanical runtime-graph remediation, the implementation appended a distinct
`reviewRemediationTransfer` subtree to the same JSON without changing either earlier
subtree. That second immutable checkpoint records graph de-duplication and its
historical timing failure; neither fact authorizes or blocks the final release.
Both intermediate subtrees are report-only and define no size ceiling. After
first-display-agent remediation, the candidate evidence records its clean pushed
source SHA, toolchain/compression identity, release inventory, per-artifact hashes,
and these semantic sets:

- **minimal first display** is `first_display` plus no optional slice;
- **reference first display** is the one served agent artifact for the semantic
  reference `[first_display, creative_initial, gpt_initial, prebid_initial,
datadome_initial]`; it does not include core or any persistent takeover module;
- **APS first display** is the served artifact for
  `[first_display, render_owner_initial, aps_initial, creative_initial, gpt_initial]`
  and drives a real fictional APS/PUC contract fixture through the first request
  action;
- **largest permitted first display** is the largest raw, gzip, and Brotli body
  among the generated size-admitted masks that trusted configuration can serve (the
  maximizing mask may differ per encoding); the capture enumerates all reachable
  masks with their admission result, hashes, sizes, and slice membership rather than
  checking only the two named examples;
- **persistent runtime** is `[core]` plus all catalogued takeover modules for the
  reference configuration, served after protected paint; and
- **maximal non-bootstrap total** is every first-display base/slice, production core, takeover,
  and deferred TSJS module in
  the release, each exactly once. This gate prevents phase splitting from hiding
  total growth.

The mandatory render implementation is physically co-bundled into `tsjs-core.js`
with the sole runtime so their shared dependency graph is emitted once. The
catalogued `tsjs-render_runtime.js` transport member is a release-stamped marker:
it preserves the logical provider row, manifest ordering, inventory accounting, and
capability contract but contains no second implementation, listener, timer, port,
or runtime. Servers still compose the catalogued `[core, render_runtime]` sequence;
the marker is not a second request and does not create a compatibility path.
The evidence capture records logical provider sources separately from physical
artifact ownership (`render_runtime` is physically owned by `core`) and rejects a
provider source in every other artifact even if a stale source-owner inventory tries
to authorize the duplicate. It also freezes the twenty largest rendered-source
contributions and every repeated attribution so review can see, rather than infer,
where transfer growth and shared-source duplication remain.

In the same blocking job, a detached worktree at the exact PR base SHA from
`origin/rc/202608` builds the real production page for the reference configuration
with its baseline artifact model and default creative behavior. `baselineReferenceTransfer`
is the exact raw/gzip/Brotli sum of every Trusted Server JavaScript byte delivered or
embedded from `tsjs:bids-script` through the first responsible GPT action. The
candidate records the same semantic interval for its controller, upstream-independent
TSJS transports, and agent; neither side relabels artifact names to manufacture
membership parity. Candidate reference transfer must be at most the rc-baseline
value for each encoding. This semantic transfer comparison is independent of, and in
addition to, the paired 1.10 timing ratio below.

The candidate evidence is accepted only after the first-display graph contains no public
runtime API, persistent broker/registry, diagnostics, refresh, SPA/navigation,
programmatic/direct-auction work, test seam, duplicate adapter owner, or live object
that cannot be disposed/transferred by §5.2.1. These independent absolute architecture
ceilings apply to the candidate and do not derive from any candidate capture:

| Semantic set                                                  | Raw bytes   | Gzip bytes | Brotli bytes |
| ------------------------------------------------------------- | ----------- | ---------- | ------------ |
| inline bootstrap controller/fallback                          | ≤ 48,000    | ≤ 16,000   | ≤ 14,000     |
| every permitted first-display agent mask                      | ≤ 90,000    | ≤ 30,000   | ≤ 26,000     |
| reference persistent runtime after paint                      | ≤ 524,288   | ≤ 163,840  | ≤ 131,072    |
| maximal non-bootstrap total, every other production role once | ≤ 1,048,576 | ≤ 327,680  | ≤ 262,144    |

The first-display ceilings are selected from the fixed 200,000-byte/s profile; the
post-paint and maximal limits prevent phase splitting or duplicated ownership from
making total release growth unbounded. They are not substitutes for the rc-baseline
semantic transfer or paired timing gates. Changing any ceiling requires a reviewed
design rather than recapturing candidate history.

The build emits one canonical release inventory with each production bundle's id,
role, phase, trigger, inputs, outputs, bytes, and hash. Budget membership is derived
from that catalog rather than an obsolete exact filename list. Candidate evidence
stores the exact raw, gzip, and Brotli values for bootstrap, every reachable
first-display mask and its generated size-admission decision (with named
minimal/reference/APS/largest summaries), persistent runtime, and maximal. The
generated catalog allowlist must exactly equal the measured admitted subset. It is
evidence for that candidate, not a self-created baseline.
After the cutover lands, subsequent work chooses its own actual target-branch base
and the same independent ceilings; no candidate-side capture becomes permanent
authority merely by being checked in.

The inline bootstrap-controller/fallback cannot be used to hide code outside those
sets. It receives its independent ceiling above, appears
exactly once under the `bootstrap` role, and is not counted again in maximal TSJS total. Its
production metafile/import allowlist permits only boot-manifest/queue/fallback
validation, generation/disposal, timing, and local logging primitives.

`npm run check:bundle` builds fresh candidate metrics and runs these parts in CI:

1. the original and two intermediate candidate captures and their digests are
   validated and printed as immutable history, never as pass/fail ceilings;
2. the freshly built exact rc-baseline and candidate reference pages enforce the
   semantic transfer comparison;
3. candidate bootstrap/all-permitted-masks/persistent/maximal values enforce the
   independent absolute ceilings; and
4. the one-time architecture/source-ownership assertions above remain blocking.

A local baseline-less invocation enforces the absolute and architecture parts and
marks the semantic comparison unavailable; CI and release evidence require the exact
PR-base comparison and fail if it is missing, stale, or not reproducible.

The gate also rejects an unclassified or multiply counted artifact, a missing
production artifact, a test artifact, a maximal inventory that omits any split
module, first-display reachability to a persistent/deferred source, takeover
reachability to a deferred source, a consumer that inlines a catalogued provider
implementation, overlapping agent/persistent side-effect ownership, and
production reachability to fake/no-op/test or `*ForTest` sources. It reports the
largest source contributions and repeated production attributions so later work
cannot hide growth inside a passing aggregate. Changing historical evidence,
semantic membership, the rc-baseline comparison procedure, or an absolute ceiling
requires a separate reviewed design; it is not an implementation escape hatch.

Boot-to-first-display uses real User Timing marks, not `__tsjsPerf` or a test-only
placeholder. The bootstrap controller records `tsjs:bids-script` immediately before
the server's first-display head sequence, so the measure includes required upstream
and agent-artifact loading. The provisional owner records
`tsjs:first-display`
exactly once immediately before the first TS-owned request action in the protected
first-display batch: the responsible GPT `display`/`refresh`. Direct `/auction`
uses the no-agent persistent runtime and records the same mark immediately before
its iframe insertion, but is a separate correctness/timing case rather than an agent
mask. A page with no render attempt during the measurement is excluded
explicitly rather than manufacturing a mark. The terminal latch for the attempt that produced that action records
`tsjs:first-display-terminal`; after the complete immutable initial projection batch
settles and the §5.2 paint gate passes, the agent records `tsjs:first-display-paint`.
Each mark is emitted at most once per document runtime. The reference fixture asserts
that no persistent or deferred TSJS request, preload, preparation, or execution precedes
`tsjs:first-display-paint`; p90 remains the exact `tsjs:bids-script` to
`tsjs:first-display` request-action measure so the historical metric does not change
meaning.

The standalone performance job uses pinned Chromium 145.0.7632.6, the
`github-hosted:ubuntu-24.04` runner class, fixture
`tsjs-baseline-paired-network-v3`, five warmups per variant, and 50 measured samples per
variant. It runs automatically when a pull request changes the TSJS build/runtime,
the server controller or projection path, the browser fixture, the evidence
validator, or the workflow itself. Its existing `workflow_dispatch` and
`workflow_call` entrypoints remain available for named pre-switch and post-switch
evidence.

The browser test has a 40-minute safety budget and its Actions job has a 50-minute
safety budget. Each of the 110 paired warmup/measured navigations returns from
navigation at response commit and closes immediately after the common first
observable action used by the timing metric; neither variant waits for the browser
`load` event or post-action lifecycle work inside the timing sample. After sampling,
the same test performs exactly one separate load-complete candidate lifecycle
observation for release identity, candidate marks, paint, takeover, and
deferred-order evidence, followed by the separate paired heap contexts. Release
identity is intentionally asserted there because it belongs to persistent takeover
and need not exist at the earlier first-action measurement boundary. The budgets
absorb hosted-runner variance, checkout, the candidate/rc-baseline builds,
browser setup, validation, finalization, and immutable upload. They do not reduce
the five warmups, 50 measured samples per variant, fixed network profile, assertions,
or evidence requirements, and they are not permission to repeat post-measurement
lifecycle work in every sample. The final budget is evidence-based: after removing
the redundant lifecycle waits and returning timing navigations at response commit,
the declared hosted runner completed the shaped sample, full candidate observation,
and first paired heap context at the former 30-minute boundary, before the second
required heap context and evidence write. The ten-minute inner reserve covers that
remaining required context plus evidence serialization; the outer reserve covers
build/setup, validation, and immutable upload.

The job declares paired GPT-reference and APS first-display cases. Both variants of
each pair use the same projection, enabled behavior,
upstream fictional stubs, page markup, creative policy, warm/cold cache state, and
browser profile. The APS case drives the fictional APS/PUC contract through its
actual first action, terminal result, and paint, and must satisfy the size, mark,
ordering, deadline, and heap contracts. The rc baseline has a first-class APS action,
so its first responsible GPT action and delivered pre-action TSJS bytes are compared
honestly; candidate-only terminal, paint, containment, and heap claims retain the
absolute gates below because the document protocols intentionally differ. A GPT pass
never masks an APS failure.

The APS case is also a blocking absolute quantitative gate, not only
a protocol-deadline check. Its checked-in fictional GPT, GAM creative, proxy, runner,
and PUC bodies and schedules are invariant test inputs; the fictional runner invokes
the real queued `prebid/creative/render` callback 50 ms after its API call. Across the
same five warmups and 50 measured samples, APS must meet all of these ceilings:

| APS metric                                                             | Ceiling                      |
| ---------------------------------------------------------------------- | ---------------------------- |
| `tsjs:bids-script` to first responsible GPT `display`/`refresh` action | p90 ≤ 900 ms                 |
| first action to accepted APS completion                                | p90 ≤ 1,500 ms               |
| accepted APS completion to `tsjs:first-display-paint`                  | p90 ≤ 250 ms                 |
| `tsjs:bids-script` to `tsjs:first-display-paint`                       | p90 ≤ 2,500 ms               |
| forced-GC `usedSize` immediately after protected paint                 | ≤ 3,145,728 bytes (3 MiB)    |
| forced-GC `usedSize` after persistent takeover and queue drain         | ≤ 3,932,160 bytes (3.75 MiB) |

Every timing row is computed from the named real marks/action, never a test-created
substitute. Every sample—not only p90—must still satisfy the narrower applicable
correctness deadline. Changing a fictional dependency body/schedule, a ceiling, a
mark endpoint, or the network profile is a performance-contract change requiring
review rather than a way to recapture a passing baseline. These deliberately broad
absolute guardrails cover the candidate-only document lifecycle. For both GPT and
APS, candidate first-action p90 and pre-action raw/gzip/Brotli transfer must be at
most the rc-baseline value × 1.10; the reference GPT semantic transfer retains the
stricter no-growth rule above.

On a pull request, each run reads `pull_request.base.sha` into the dedicated
`TSJS_PERF_BASE_SHA`, verifies it is a 40-character commit reachable from the fetched
`origin/rc/202608`, creates a detached worktree at that exact SHA, and builds the rc
baseline and candidate independently. A manual/called run requires the same exact
input; a moving branch name or candidate-generated fallback is invalid. The
baseline-side loader feature-detects and consumes the artifact shape that commit
actually produced. While the rc baseline emits the legacy `tsjs-core.js`,
`tsjs-creative.js`, plus `tsjs-gpt.js` model and no release-v1 inventory/controller,
the harness concatenates and serves those real built bytes, enables the same default
creative policy, and drives their real legacy `adInit` surface. After the cutover
reaches the release branch, a later comparison consumes that base commit's release-v1 inventory/controller
and that commit's real server-selected first-display artifact instead. It does not relabel an older phase-aware
capture as the baseline or require unavailable candidate-only metadata from a legacy
commit. The candidate side consumes its generated server controller and release-v1
first-display/takeover/deferred artifacts. One in-process `node:http` server per variant on an
ephemeral `127.0.0.1` port serves that variant's exact page and bytes. Playwright
request interception or fulfillment is outside the instrument.

Before either variant navigates, its page receives the same checked-in Chromium CDP
`Network.emulateNetworkConditions` profile: 150 ms latency, 1.6 Mbit/s download
(200,000 bytes/second), 750 kbit/s upload (93,750 bytes/second), and zero packet
loss. The profile is not selectable by environment input. A common comparison mark
runs immediately before the external first-display script element, and the GPT fixture
records the common terminal mark at its first observable `display` or `refresh`
action. A direct-render comparison records the equivalent iframe insertion. The
interval therefore includes first-display transfer, parse, evaluation, and
runtime work through the first observable request/render action without depending
on a candidate-only mark. Candidate runs additionally prove the real
`tsjs:bids-script`, `tsjs:first-display`, and `tsjs:first-display-paint` marks and
that no persistent or deferred TSJS request, preload, preparation, or execution
precedes paint.

The no-agent direct `/auction` correctness fixture records
`tsjs:first-display-terminal` from its terminal latch and
`tsjs:first-display-paint` through the same two-frame/hidden allowance. Ordinary
deferred loading waits for that paint or the no-attempt guard exactly as specified
for direct-to-runtime pages. It is not counted as an agent-mask sample or used to
claim the agent's ≤90 kB transfer ceiling.

The job alternates baseline then candidate / candidate then baseline in one Chromium
process for every warmup and measured GPT and APS pair. Candidate first-action p90
must be at most the matching rc-baseline p90 × 1.10. GitHub-hosted absolute timing is
not stable enough for a tight fixed shared-regression ceiling, and a historical fixed
comparison SHA is not an honest stand-in for the PR base; neither replaces that
paired gate. The APS absolute ceilings above are separate fixture guardrails. The
schema-6 artifact records the exact baseline and candidate SHAs, each actual artifact model, each exact served
first-display byte count, both full distributions and p90s, the alternating order, and
the exact network profile. The workflow runs each declared pair once and never
selectively reruns, drops, or reclassifies slow samples. Budget assertions are soft
only in the Playwright sense: the run finishes all timing and heap collection and
writes the complete schema-6 evidence before failing. Validation and upload run
with `always()` so a failed gate retains its exact diagnostic artifact; neither the
test nor the validator converts an exceeded budget into success.

On a pull request, both the evidence writer and validator consume the dedicated
`TSJS_PERF_HEAD_SHA` value resolved from `pull_request.head.sha`. They do not read or
attempt to override GitHub's reserved `GITHUB_SHA`, which names the synthetic merge
commit for a pull-request workflow.

The performance workflow invokes checked-in repository scripts, and its
performance-only Playwright configuration is also a checked-in TypeScript file.
Neither workflow YAML nor a shell script synthesizes executable source or
configuration at runtime.

The historical GPT blocking ratio remains request-action latency, so the gate and job
call it **bids-script-to-first-action**, not paint latency. The same evidence records
candidate terminal and paint distributions for GPT and APS. Every sample must remain
inside the unchanged path-specific render deadline and §5.2 paint allowance; this
prevents an agent from improving transfer latency by postponing completion or
takeover without claiming a non-equivalent rc-baseline terminal/paint ratio.

Retained heap for the paired GPT case uses forced-GC Chromium checkpoints after boot,
first render, refresh, and SPA navigation. After the display samples, the job launches
one fresh Chromium process, opens one separate fresh browser context per variant, and
executes the equivalent lifecycle supported by that variant's real artifact shape.
This keeps retained-heap evidence out of the process that has already completed 111
cold navigation contexts. The APS case uses its two candidate-only checkpoints in the
table above. At each checkpoint the job calls Playwright's supported `page.requestGC()`
once followed immediately by CDP `Runtime.getHeapUsage`; each GC, usage, detach, and
cleanup operation has an explicit 30-second local failure boundary. The single
`usedSize` is the checkpoint statistic, with no hidden averaging, maximum selection,
or rerun. Both variants must remain below the immutable 4 MiB hard ceiling. The
schema records rc-baseline values and identity for direct inspection, but does not
turn the smaller legacy runtime shape into an arbitrary multiplier for the
hard-cutover kernel and deferred architecture. Any checkpoint over the absolute limit
fails the one declared run; the job cannot replace only that measurement or rerun only
the heap fixture. Correctness runs independently in Chromium, Firefox, and WebKit.
Correctness failures are never waived by a performance pass.

The paired SPA checkpoint changes the document pathname, not only its query. The rc
baseline and candidate use their respective real navigation hooks, and both must observe
that same pathname transition, request `/_ts/page-bids`, consume the response body,
and finish GPT slot reconciliation before the final forced-GC measurement.

## 6. Security and privacy

1. The publisher-origin bootstrap initially omits `allow-same-origin`; after its
   exact readiness proof, the top-page owner adds that token only for navigation to
   the naturally opaque data container. The final outer and inner documents are
   `data:` documents and remain opaque while their permanent sandbox keeps
   `allow-same-origin` so the HTTPS creative retains its real origin across browsers.
   Target `"*"` is used only with exact source, nonce, shape, generation, element,
   and one-use-port checks because opaque origins cannot be named as `targetOrigin`.
   PUC/GAM/bidder content is never an ancestor of the APS mount.
2. The initial global PUC request contains the opaque renderer reservation
   capability but no descriptor, ADM, lifecycle ticket, or nonce, and it establishes
   no success. The first compatible claim acquires the PUC source; render authority
   begins only after exact reservation/slot lookup, attributable nonempty GAM,
   current generation, source binding, and atomic consumption. Its dynamic owner
   receives settlement authority only; it never receives descriptor data or APS DOM
   authority.
3. Lifecycle tickets and nonces are CSPRNG, one-use, TTL-bounded, never logged, and
   invalidated on supersession/navigation.
4. Exact-key message parsing prevents confused-deputy extensions. Unknown versions
   are ignored or failed closed according to whether the message claims a TS
   capability.
5. Native Prebid messages with non-TS ids continue to native listeners. Any message
   carrying a live or tombstoned TS id is suppressed before later validation.
6. The first-display handoff is not a public trust boundary. Its data record is
   exact-shaped, bounded, recursively frozen, and descriptor/creative-free; its
   object capsule is closure-private, same-task, release/generation-bound, and
   one-use. No agent capability reaches `window.tsjs`, DOM attributes, messages,
   logs, diagnostics, storage, or analytics. A forged/replayed/stale capsule fails
   before persistent activation and cannot claim committed DOM or a GPT object.
7. The upstream APS runner URL, every creative URL, and production renderer/proxy
   routes must be HTTPS. HTTP is permitted only for loopback hermetic adapters; their
   fixed local proxy origin is injected as the inner document's sole external script
   origin. The runner executes only
   through the fixed-target, anonymous-CORS Trusted Server proxy. The proxy relays the
   APS body unchanged but never stores it in source, forwards publisher credentials,
   accepts a caller-selected target, or executes a fallback. No APS, GPT, PUC, or
   other vendor bytes are stored in source or release artifacts; hermetic fixtures
   are locally authored protocol doubles. Runner-created APS-origin
   resources may use their own origin cookies under browser policy. The renderer
   document accepts no cookie as authority.
8. Script creatives remain opt-in because they materially broaden executable
   behavior. Enabling them selects only the reviewed conditional CSP pair in §4.4:
   the validated creative origin is added to `script-src` in both inherited policy
   levels, while iframe creatives retain the narrower pair. Build and browser tests
   reject any tag-type/policy mismatch or redirected script origin.
9. Persistent/deferred module URLs are generated from the local immutable release inventory,
   are same-origin exact content-hash paths, and are authenticated by
   release/id/source plus the core-created element and `document.currentScript`.
   The narrowly scoped Trusted Types policy accepts only those frozen manifest URLs,
   CSP nonces are copied only from the authenticated parser-inserted tag, and policy
   failure cannot select another sink or source. Publisher-created tags and calls
   cannot load or register code, and no deferred URL can select an upstream script
   target.
10. Static TSJS hash mismatches and unknown paths fail locally on every adapter; a
    stale URL never aliases current bytes or falls through to publisher origin.
11. The design adds no persistent identifier and no external event pipeline.

## 7. Verification and acceptance

### 7.1 Required test layers

| Layer                 | Required proof                                                                                                                                                                                                                                                                                                                                                                                                                                                |
| --------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Rust unit             | APS parsing/admission, dimensions, scripts, AAX projection, mediation provenance/order/timeouts, targeting identity, descriptor serialization, endpoint headers/body                                                                                                                                                                                                                                                                                          |
| Baseline/gap audit    | rc-baseline tests are the behavioral oracle; every retained `RCJ-*` row starts proof-pending, identifies/authors a focused contract, runs it on the untouched recorded-rc worktree, and ends baseline-owned or demonstrated implementation-gap with recorded SHA/owner/test command/result; proof-pending/coverage-gap blocks phase exit and no retired source is built or merged                                                                             |
| Cross-language corpus | every positive/adversarial descriptor has the same Rust, TS, and embedded ES5 result; stale generation fails                                                                                                                                                                                                                                                                                                                                                  |
| TS unit               | agent/takeover ownership, exact handoff/capsule admission, takeover/deferred transactions, fallback versus isolated deferred failure, trigger/disposal races, sessions, registries, selection/cycle/batch/latch APIs, adapter readiness, GPT handoff/reconciliation, Prebid artifact/refresh, creative security, diagnostics, and every remaining integration parity corpus                                                                                   |
| Hermetic browser      | one parser-blocking first-display request, no pre-paint persistent/deferred traffic, authenticated atomic takeover into the one persistent runtime, all render paths, PUC bridge, exact-bound-slot top mount, four-level APS sizing, data-document CSP containment, direct iframe races, owner/port/runner behavior, fallback, SafeFrame-shaped isolation, GPT handoff/hydration, creative clicks, diagnostics, and duplicate/replay/wrong-source/stale cases |
| Real-GAM test network | SSAT APS-PUC, Prebid-adapter APS-PUC, page-bids APS-PUC, direct APS, direct ADM plus rc-baseline PBS Cache regression, fallback after attributable empty GAM, SRA, refresh, SPA, handoff, hydrated DOM replacement, and collapsed shell                                                                                                                                                                                                                       |
| Adapter parity        | exact bootstrap sandbox/CSP/header bytes plus runner-proxy routing, five-second deadline, closed response parsing, bounded relay, header filtering, and failures match on all adapters                                                                                                                                                                                                                                                                        |
| Regression            | non-APS Cache/ADM and notifications, pure external Prebid/native bids/EIDs/user IDs/refresh exclusions, publisher GPT/handoff/SRA/SPA, creative processing/click recovery, render trace/GPT diagnostics, and every remaining integration remain correct                                                                                                                                                                                                       |
| Quality               | full-package TypeScript/lint including tests/scripts/build code, ESLint and release-catalog dependency boundaries, production-metafile/test-hook exclusions, format, clippy, Rust adapter suites, Vitest, artifact integration, Playwright, immutable historical bundle reporting, role-correct transfer budgets, performance/heap budgets, and complete maximal inventory                                                                                    |

Feature-owned GitHub Actions YAML is declarative orchestration, not a program
container. Multiline shell programs, generated configuration bodies, validation
branches, loops, release-manifest writers, and evidence scrubbers introduced by this
design live in reviewable repository files under `scripts/ci/`. Workflow `run` steps
may invoke those scripts or use a simple one-command tool invocation such as
`npm ci`, `cargo test-axum`, or an existing `./scripts/...` entrypoint; they must not
embed `node -e`, heredocs, shell loops, generated-file bodies, or folded/multiline
program logic. Shared toolchain extraction and evidence handling are reused rather
than copied between workflows, while workflow-specific orchestration remains in
focused scripts instead of one opaque dispatcher.

Repository contract tests read both the workflow and the referenced script files.
They reject feature-owned embedded workflow programs and missing script targets,
then assert the security, release-binding, exact-rc-baseline, performance, browser-matrix,
adapter-corpus, manifest, and evidence-scrubbing behavior in the scripts that own it.
This source-shape rule changes only how CI logic is maintained; it does not weaken a
gate, broaden secret exposure, alter protected-environment admission, or introduce a
second evidence format.

### 7.2 Mandatory race matrix

Tests must cover at least:

- duplicate simultaneous `Prebid Request` for the same id;
- claim before/after attributable nonempty GAM, claim followed by empty GAM, and
  navigation/supersession at each side of that two-condition join;
- replay after consumption and after tombstone expiry boundary;
- attempt-id navigation prefix failure, ordinal uniqueness and exhaustion without an
  issued-id set; forced lifecycle-ticket, `b1_` bootstrap-nonce, and `n1_`
  renderer-nonce collisions through the eighth draw; independent 255/256/257 live
  boundaries for each nonce registry and 319/320/321 ticket/tombstone entries;
  capacity versus expiry pruning; cross-role nonce substitution; and proof that no
  overflow path posts a usable capability;
- valid id from wrong slot/source, altered id from the expected source, and a native
  Prebid id;
- PUC registration before/after timeout, wrong source, zero/two ports, replay,
  caller abort before/after registration/insertion/document acceptance, and owner
  watchdog racing a late kernel response; every winner produces exactly one
  encodable `OwnerSettlementV1` and one PUC Promise settlement; channel loss before
  start and after top insertion, settlement-post throw, and the 20-second remote
  cleanup boundary prove the PUC owner owns no APS node while the mount service
  removes only its exact uncommitted iframe and leaves accepted DOM;
- bootstrap ready before/after deadline; wrong source/`b1_`/shape; outer frame
  removal, `src` mutation, replacement, and navigation; zero/two container ports;
  wrong-outer `n1_`, inner-ready replay, port swap/reuse, and descriptor transfer
  before/after channel creation; a nested SafeFrame-shaped PUC source proving claim
  authentication never performs WindowProxy-to-DOM mapping; publisher,
  GAM, PUC, and creative attempts to navigate or frame a publisher-origin document;
  exact iframe-versus-script outer/inner CSP selection, iframe-script refusal,
  same-origin script-creative execution, cross-origin redirect refusal, and other
  creative-origin allow/deny cases; data-document 65,535/65,536/65,537-byte
  boundaries; sentinel escaping/completeness; and renderer document success followed
  by runner failure/timeout;
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
- accepted PUC APS overlay followed by publisher `display`, explicit/global/SRA
  `refresh`, competing or ambiguous request intent, and an exactly attributable TS
  replacement that succeeds or fails; prove publisher/ambiguous `slotRequested`
  retires the overlay before later GPT listeners while a TS replacement retains it
  until commit; publisher `destroySlots` explicit/all with true/false/throw, GPT DOM
  removal before/after the call or callback, host replacement/disconnection, owned
  overlay removal/reparenting, navigation disposal, and all pairwise races prove
  exact-once node/style/targeting cleanup, no GPT-object destruction by artifact
  retirement, no repeated PUC settlement, and no refreshed publisher content hidden
  behind an old overlay;
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
- required `[creative_opportunities].enabled` present/absent/wrong-type parsing;
  disabled publisher HTML and page-bids produce no matching/auction/projection/client
  initialization while direct `POST /auction` remains live; disabled configuration
  still fully validates; inactive HTML public/no-header/private/no-store policies;
  case-insensitive protection of private directives; preservation of validators and
  surrogate/CDN fields; synthesized-state privacy; and non-HTML/error noninterference;
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
- `gam_attribution_enabled` false/true with missing and pre-existing `adInit`, GPT
  queue absent/present, targeting API missing/throwing, initial/lazy/publisher-owned/
  refresh/SPA requests, takeover, and direct-persistent boot; prove exactly one
  parser-time `setConfig({targeting:{ts:'true'}})`, no `ts` cleanup, no old global or
  activation attribute, and no behavior change while disabled;
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
  disposal/reuse on the next navigation; Prebid lease promotion; and the pinned
  PBS Cache black-box corpus for ADM-over-cache precedence, cache-only fetch/parse/
  macro/PUC response, collapsed resize, failure, and stale-navigation disposal with
  no new cache input, result, deadline, or identity semantics;
- release-catalog cycles, duplicate providers, undeclared capabilities, takeover
  consumption of a deferred provider, phase overrides, unknown/missing/multiply
  counted production artifacts, deferred/test source pulled into core, and a module
  classified into the agent without its named parser-time or first-display obligation;
  exact per-consumer capability projection proves `attachPresentation` is absent
  from `trace.v1` and denied to APS, GPT, and `gpt_later`, while only
  `diagnostics_presentation` can consume `trace.presentation.v1`;
- exactly one parser-blocking network request and manifest-order registration;
  takeover exact-six-key and deferred exact-five-key registration; no-agent
  `prepareSync` success/throw/thenable/deadline behavior with no task or microtask
  before activation and commit; agent takeover and deferred synchronous/async
  `prepare` success/reject/abort; first-display/takeover module
  missing/wrong-release/duplicate/activation throw at each checkpoint, late
  continuation after fallback, and takeover
  `afterCommit` throw; 9,999/10,000/10,001 ms synchronous activation returns plus
  the pre/post-call and pre-handoff monotonic checks; nonreturning activation
  documented as unpreemptable; duplicate `afterCommit` registration,
  catalog-derived 13/14/15 takeover-callback staging, and 19/20/21 total-manifest
  capacity; publisher GPT activity and
  script/creative DOM activity before/during/after a later takeover failure prove
  preparation is inert, activation cannot yield, rollback is same-task, and
  post-commit work sees only the full persistent kernel; queued and later
  `requestAds`, callback throws, already-aborted signals, publisher/unsolicited
  integration registration refusal, and proof that no second runtime, listener,
  port, timer, request, script, wrapper, guard, or iframe survives fallback;
- takeover download/preparation with interleaved publisher `defineSlot`, `display`,
  explicit/global `refresh`, destroy, targeting mutation, GPT events, DOM
  replacement, guard observations, Prebid queue activity, consent/segment changes,
  and terminal/tombstone expiry proves that static preparation reads no live state;
  the final same-task snapshot sees the last mutation revision, revision exhaustion
  fails closed, and a callback queued after closure reaches only the persistent epoch;
- protected-paint admission sealing immediately before/at/after the boundary proves
  a late TS Prebid bidder call completes once with no bid and no minted lease,
  reservation, ticket, attempt, port, or callback; native publisher GPT/Prebid and
  non-TS bidder calls remain pass-through; discovery of deliberately injected live
  TS authority commits `bundle_partial` rather than transferring or replaying it;
- persistent download/authentication/preparation success, failure, and the exact
  9,999/10,000/10,001 ms post-paint deadline prove callback pushes remain queued
  until one commit; success drains against the full kernel, while failure freezes the
  exact true/false `initialDisplayCommitted`, drains once against fallback, makes
  every new omitted-selection `requestAds` resolve the exact empty result, every
  explicit valid id resolve `slot_unresolved` or already-aborted `caller_aborted`, and
  every `addAdUnits` propagate the same classified
  `abi_mismatch`/`bundle_partial` fallback reason, preserves accepted DOM, and leaves
  no pending work or artifact retry;
- final handoff boundaries for attempt/slot/GPT-token/cycle/trace ordinals at
  maximum-minus-one/maximum/exhaustion; pruned prior cycles with
  `unknownPriorCycle`, open/retired/quarantined GPT cycles, late old-cycle facts,
  monotonic expiry translation, 255/256/257 adopted slots, 8 MiB
  non-diagnostics/512 KiB diagnostics/8.5 MiB total data-tree caps, and one-use
  capsule replay prove that no id/sequence is reused and no event is reattributed;
- provisional creative/DataDome/GTM/Lockr/Testlight/consent guards mutate during
  runtime preparation, then compare-restore and install fresh persistent effects in
  one task; observer-record loss is covered by the bounded rescan, publisher-
  replaced globals leave old effects generation-inert, and no function/listener/
  wrapper/observer enters the handoff or capsule;
- direct persistent boot for rewritten creative documents and direct `/auction`
  proves neither path selects an agent or waits for a nonexistent projected-attempt
  paint; direct timing/deferred release uses the persistent owner, while every
  generated size-admitted agent mask—including GPT reference and APS—passes the exact size,
  source-reachability, action, terminal, and paint assertions;
- first-display-required live GPT/Prebid fetch starting after `tsjs:bids-script` and
  overlapping the first-display TSJS fetch, upstream success before/after adapter
  activation, and proof that no TS-owned display/request occurs before the sole
  adapter's correctness listeners; optional upstream and APS runner traffic is
  absent from boot, TSJS generated-source scans contain no upstream library bytes,
  and the separately generated external Prebid artifact passes its purity scan;
- `first_display_or_idle` at each side of attempt creation and batch settlement, no
  initial slot, the 9,999/10,000/10,001 ms attempt-creation guard, first/second
  animation frame, hidden/visible transition, idle callback, idle timeout, 50 ms
  timer fallback, an explicitly post-window first display overlapping released later
  work, navigation waiter disposal, and full-runtime module disposal;
  concurrent readiness waiters keep their original deadlines while sharing one
  module Promise/script after the gate; all deferred transactions start after the
  common gate without waiting for one another, and a hung/failed first catalog entry
  cannot delay another module's fetch or deadline; no persistent/deferred request/preload/evaluation
  precedes `tsjs:first-display-paint`; exact script-node/currentScript/source/id/
  phase/release authentication rejects publisher tags, replaced nodes, redirects,
  duplicates, and stale generations; deferred prepare/activate/afterCommit/load/
  timeout failures leave the same kernel/adapter/listener/slot/dispatcher identities
  live, settle dependent work with its typed reason, leak no node/listener/timer,
  and neither install fallback nor start a second runtime;
- first-display, takeover, and deferred static routes across Fastly, Axum, Cloudflare, and Spin:
  exact path, one-field hash query, response-byte hash, enabled catalog membership,
  MIME/nosniff headers, conditional `304`, local `404 no-store`, no publisher
  fallthrough, and no redirect; stale-release URLs fail instead of receiving current
  bytes;
- CSP/Trusted Types browser fixtures for same-origin allowlisting, matching nonce,
  nonce-only policy, `strict-dynamic`, allowed `trusted-server#tsjs-v1`, rejected
  named policy with an exact-preserving publisher default, and full policy block;
  mutation by a default policy, a disallowed URL, or a synchronous policy throw
  produces no insertion/request and settles only the affected deferred module as
  `policy_blocked`; missing/invalid nonce or CSP source rejection after insertion is
  `load_error`, while node removal/replacement may have initiated a request but
  cannot register and becomes `registration_rejected` unless load failure wins;
- exact kernel/fallback `TsjsApi` own surfaces; semantic version and release-id
  equality; boot deep-copy/freeze and malformed-field safe fallback; exact ordered
  integration-config id inclusion, manifest/product matching, attenuated per-module
  delivery, bootstrap-snapshot identity through agent takeover/deferred load, raw
  pre-load-object replacement, old-global absence, accessor/symbol/prototype/cycle/
  sparse-array/alias rejection, depth/node/string/key/per-entry/aggregate boundaries,
  and fallback empty config entries; actual-Array
  queue identity; pushes before/during/at activation and commit completion; retained ingress
  references after swap; snapshot-versus-forward exactly-once behavior; nested push
  ordering; frozen final-queue `length:0` under native/borrowed mutators, index and
  length assignment, deletion, and property definition in strict and sloppy callers;
  immediate post-load return values, `this`, non-callables, and callback throws;
- main bundle absence after server GPT projection, proving the old degraded bootstrap
  renderer is deliberately gone: no GPT definition/targeting/display/refresh occurs,
  fallback retains no server-projected slot membership, omitted-selection
  `requestAds` resolves `{slots:[]}`, explicit valid ids resolve `slot_unresolved`, the
  queue drains once, and a late bundle cannot revive rendering;
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
  and deadline orderings, proving only the current intended navigation accepts;
- core diagnostics ingress has the exact frozen `{publish,dispose}` surface and no
  `subscribe`, listener id, capacity, queue, scheduler, or timer; valid ordinary and
  null-prototype records, dense arrays, and UTF-8 multibyte data pass at total-node
  511/512, depth 15/16, 127/128-byte property names, and 4,095/4,096-byte strings,
  while total-node 513, depth 17, 129-byte names, 4,097-byte strings, sparse/extra-
  property arrays, accessors, symbols, custom prototypes, functions, `undefined`,
  bigint, non-finite numbers, cycles, aliases, hostile traps, and copy/freeze failure
  return `false` without reducer entry; accepted snapshots share no producer record
  or array, reducer/reporting throws are isolated with `true` returned, disposal is
  idempotent, and retained stale-runtime publishers remain `false` and inert;
- GPT trace tokens have canonical ordinals 1/35/36/4,294,967,295, exact lower-case
  base-36 grammar and 11-byte maximum, reject zero/leading-zero/upper-case/decoded-
  overflow/collision inputs, and latch only new trace-token minting on ordinal
  4,294,967,296; repeated facts and publisher handoff for one physical slot retain
  one token, distinct/replacement objects sharing every publisher identifier receive
  distinct tokens, and token failure leaves object-identity-bearing `gpt.events.v1`
  delivery and GPT behavior live; per-object trace-cycle ordinal 0/1/4,294,967,295/
  4,294,967,296, fractional/non-finite rejection, 9/10/11 retained-cycle pruning,
  and per-object rather than global exhaustion; compound `{token,cycleOrdinal}` core
  binding covers unresolved/ambiguous/duplicate active facts, live-map totals
  255/256/257, completed/retired oldest-first pruning, destroy/redefine, handoff,
  navigation retirement, no old-pair rebinding or new-current mutation, history-only
  late enrichment, and full runtime disposal; two consecutive refresh cycles on the
  same physical object produce two rows, with prior-cycle `slotResponseReceived`,
  `slotRenderEnded`, `slotOnload`, `impressionViewable`, and visibility callbacks at
  each side of the next cycle's start and completion joining only when uniquely
  attributable and otherwise producing no trace projection or current-row mutation;
  the same ambiguity remains fail-closed after the eleventh cycle prunes an old
  record and sets `unknownPriorCycle`;
- render-trace record/update reordering, one-impression enrichment, weaker-signal
  non-regression, 200-entry pruning, stale attribution/DOM-field/badge removal,
  navigation pruning of `current`, 32/33 subscriber boundaries and capacity reuse,
  199/200/201 pending notification bounds, same-sequence coalescing, post-commit
  asynchronous frozen subscription detail/timing, subscribe/unsubscribe races,
  slow/throwing listeners, absence of `tsjs:adRendered`,
  hidden/gam-only/ok truth, and cross-IIFE sequence order; private presentation
  non-callable/reentrant/duplicate attachment, failed factory/malformed-controls/
  missing-listener rollback and later retry, same-task initial snapshot before live
  delivery, non-callable source listener before state checks, second-subscribe failure
  preserving the first listener, unsubscribe/resubscribe and idempotent unsubscribe,
  commit-during-next-task ordering, update coalescing, ignored return and thrown
  callback, detach/owner-dispose exactly once, late scheduled callbacks, retained
  empty `current`/`history`, rejected retained subscribe and inert unsubscribe/detach/
  controls references, public 32-subscriber capacity unaffected, and overlay/export
  failure that leaves the trace store and public subscribers live;
- GPT diagnostics activation before/after early buffered callbacks, exact raw-event
  replay, exact `tsjs.boot.diagnostics` schema, query/session enable-disable and
  fail-closed inputs, accessor/prototype/unknown/missing/version rejection and
  manifest-activation mismatch, active six-listener versus inactive correctness-listener counts,
  exact `FirstDisplayGptFactV1` key/event/type/nullability rules, physical-token/
  runtime-slot-number/ad-unit-path preservation, separate matched/unmatched/ambiguous
  coverage disposition and nullable no-cycle/overlap/unknown-prior/invalid-order issue
  reason, and byte-for-byte equivalent initial slot/cycle/coverage/issue export before
  versus after takeover,
  maximum-size 999/1,000/1,001-byte facts and 511/512/513 maximum-sized fact
  admissions against the exact 512-entry FIFO, 512 KiB diagnostics subsection, and
  8.5 MiB total caps, including eviction versus byte-cap drops and saturated
  overflow/drop counters,
  63/64/65 slots, 9/10/11 cycles, 127/128/129 issues,
  32/33 public subscribers, 0/1/2-update latest-snapshot coalescing,
  subscribe/unsubscribe/disposal races and slow/throwing listeners; requested-format
  tuple validation/copy/freeze and 15/16/17 cap; simultaneous requested/GPT-fill/
  outer-box evidence and distinct labels; initial measurement, missing
  `ResizeObserver`, resize update, fractional/zero/invalid boxes, unchanged-value
  deduplication, stale-cycle refresh, binding replacement/ambiguity/disconnection,
  eviction/navigation/disposal; slot element replacement, timing/frozen-export bounds,
  overlay disposal, inactive
  zero-diagnostics-side-effect behavior, and diagnostics failure during live ads;
- creative processing across sanitize/rewrite policy combinations and every delivery
  path; exact default/explicit `CreativeBootV1` validation; automatic immediate and
  `DOMContentLoaded` install; disabled and enabled-with-both-guards-false
  zero-side-effect behavior; idempotent rescan;
  exact-wrapper disposal; absence of mutable/install globals; opaque sandbox click
  recovery; absolute HTTP(S), credentials, malformed and non-network schemes;
  dynamic URLs; replaced elements; and redirect/browser navigation failure; and
- every remaining integration alone and in the maximal manifest, including each
  catalogued first-display/takeover/deferred split, parser-time interception when required,
  missing globals, readiness/timeouts, malformed consent/storage, matcher false
  positives, callback throws, startup failure, reverse-order disposal, and
  cross-integration isolation; and
- every protocol string/body limit at boundary-minus-one, boundary, and
  boundary-plus-one UTF-8 bytes, including multibyte, duplicate-key, malformed
  encoding, exact 1/4096 renderer dimension bounds across Rust/TS/ES5/PUC DOM,
  and exact capability-form cases through both dispatcher and port parsers.

### 7.3 Real-GAM pass criteria

The checked-in test-network fixture and hermetic fakes use fictional ids and no
production demand. Real network ids, GAM creative configuration, and secrets are
injected by the protected CI/manual environment and never checked into this
repository.
Each required flow must demonstrate the expected DOM and lifecycle result, not an
analytics row:

- APS paths: one creative request, one bridge claim where applicable, one top mount
  iframe, one APS runner load, one APS render-completion callback, one accepted
  result, no duplicate render. The top mount, outer data container, inner renderer,
  and descendant creative each have the exact winning viewport with zero default
  margin, no clipping, and no overflow. An ensuing native publisher refresh retires
  the accepted overlay at its physical `slotRequested` boundary and displays the new
  GPT result unobscured without a second PUC settlement.
- Empty GAM fallback: parent settles empty/failure before exactly one child render.
- Direct ADM: exact owned iframe reaches one accepted result. Existing PBS Cache
  fixtures retain their rc-baseline observable result without entering the new
  APS/ADM owner protocol.
- Failure fixtures: wrong id, invalid descriptor, missing claim, missing owner,
  missing document acknowledgement, and runner failure each reach the specified
  terminal reason within the specified timeout.
- After SPA replacement, no old attempt mutates the current slot or targeting.
- The initial page loads one same-origin first-display TSJS artifact. Network
  evidence shows no persistent/deferred TSJS request/preload before
  `tsjs:first-display-paint`; takeover transfers exact physical/ad ownership without
  another display, and later refresh/navigation/diagnostics behavior joins the same
  persistent runtime without replacing its GPT/Prebid adapter or lifecycle owners.

The suite records browser console, network metadata, DOM, and GPT-event evidence as
CI artifacts. Network capture excludes APS runner and creative response bodies so a
test artifact cannot become an accidental vendor-code archive. It requires no
external analytics, billing, or experiment result.

## 8. Delivery and rollout

This work is assembled through test-only constructors while being built, then cuts
over once through the existing APS/TSJS release mechanism. No runtime flag,
old/new selector, compatibility branch, or dual protocol is introduced in any
deployable artifact.

Operator configuration moves before the binary cutover. The rc baseline already
accepts canonical APS `account_id` and `[creative_opportunities].enabled`, so both
are validated and pushed as release prerequisites. The new binary requires the
boolean and exposes no legacy alias or omitted-field compatibility path.

1. **Integrate the release base:** fetch and integrate current `origin/rc/202608`, record its
   exact SHA, run the unchanged affected Rust/TS/browser suites, and start every
   retained `RCJ-*` row proof-pending. Identify or author its focused contract, run
   that test against a detached otherwise-untouched worktree at the recorded SHA,
   and record SHA/owner/test command/result. A passing contract is baseline-owned; a
   behavioral failure is an implementation-gap; missing test coverage is a
   coverage-gap that blocks production edits until the test is authored and run.
   No row may remain proof-pending or coverage-gap at phase exit. Do not fetch,
   merge, rebase, or cherry-pick retired `rc/july`.
2. **Contract first:** land descriptor corpus, lifecycle types, adapter interfaces,
   and failing tests without changing production behavior.
3. **Kernel and release catalog:** introduce runtime/integration-module ownership,
   sessions, capability broker, slot registry, auction batch, render lifecycle,
   phase metadata, and production-versus-test composition boundaries behind
   test-only construction.
4. **Server APS path:** make admission, mediation, descriptor projection, targeting,
   bootstrap route, publisher frame policy, and runner proxy conform to the contract.
5. **First-display extraction:** preserve the two immutable intermediate captures,
   build the server-composed first-display artifact plus authenticated post-paint
   takeover, move persistent/later behavior out of its graph, and make inventory,
   ownership, and production-metafile gates green. Record candidate evidence from
   its exact clean pushed parent; all historical/intermediate deltas remain
   report-only, and fresh-rc-baseline semantic transfer, independent absolute size,
   timing, and heap gates must be green before production wiring.
6. **Browser integrations:** migrate APS, GPT, Prebid, direct auction, fallback,
   local diagnostics, creative processing, every remaining affected TSJS
   integration, and bootstrap to the catalogued first-display/takeover/deferred
   modules while rc-baseline regressions and retained `RCJ-*` gap contracts stay
   green.
7. **Delete legacy paths:** remove expandos, duplicate bridge branches, old
   `requestAds`, legacy globals, duplicated bootstrap behavior, and unused flags
   from the release candidate.
8. **Pre-production:** refresh/integrate current `rc/202608` again, then pass all hermetic
   suites and the protected real-GAM network
   in Chromium, Firefox, and WebKit; archive its console, network, DOM, and GPT-event
   evidence with the release artifact.
9. **Binary production cutover:** deploy the verified artifact through the
   repository's normal release mechanism. This design adds no percentage router or
   canary-selection infrastructure. Hold an exclusive production deployment window,
   attest the active immutable artifact, and re-check it immediately before cutover;
   any mismatch blocks and regenerates evidence. Retain that immediately prior
   artifact and roll back the whole cutover on renderer errors, elevated request
   failures, CSP/security errors, or non-APS regressions.
10. **Post-cutover:** monitor existing operational signals for 24 hours. The deployed
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
6. **Trust creative-document callbacks for ADM:** rejected; bidder-controlled
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
12. **Mount APS beneath Universal Creative or another third-party ancestor:**
    rejected. An ancestor with navigation authority can replace an opaque child while
    preserving its `WindowProxy`. PUC receives only settlement authority; the
    top-page owner mounts the nested data renderer as an owned overlay child of the
    exact slot service binding without traversing or hiding the PUC surface.
13. **Vendor PUC, GPT, APS, or another upstream script for deterministic tests:**
    rejected. Hermetic fixtures are independently authored protocol doubles and the
    protected conformance suite exercises the named live external release.
14. **Keep every enabled integration in one atomic boot barrier:** rejected. It makes
    first display pay for refresh, later navigation, diagnostics UI, and optional
    integrations that cannot affect that display. Atomicity remains mandatory for
    the takeover transaction and for each deferred module's local transaction.
15. **Download a monolith early and merely postpone its callbacks:** rejected. It
    preserves transfer/parse contention and does not solve load time; deferred code
    is a separately requested release-owned artifact after its trigger.
16. **Let integrations dynamically import arbitrary modules or upstream scripts:**
    rejected. Core alone inserts exact same-origin release artifacts. GPT, APS,
    Prebid, PUC, and other upstream bytes remain live external dependencies and are
    never vendored into TSJS source or output.
17. **Give each deferred bundle its own runtime or service locator:** rejected. A
    later module can join only the committed session through catalogued frozen
    capabilities and cannot replace an owner.
18. **Keep optimizing the 395 kB full-runtime first-display graph:** rejected by
    measured evidence. Co-bundling removed duplicate emission but still produced a
    4.57× p90 against the historical main reference; no credible local minification or shared-chunk
    change closes that gap under the fixed network profile.
19. **Create provider-specific independent mini-runtimes:** rejected. They reduce
    bytes per path but multiply queue, adapter, message, slot, and fallback ownership.
    The one bounded agent has a single exact transfer into the one persistent runtime.
20. **Relax the 1.10× gate or accept the 220 kB mechanical ceiling:** rejected. The
    automated paired gate is the user-visible load-time acceptance criterion; the
    90 kB agent ceiling is an additional architecture guard, not permission to
    self-baseline or waive the measured result.
21. **Merge or cherry-pick retired `rc/july` to recover TSJS work:** rejected. The rc
    baseline already contains most required behavior, often through later or squashed
    implementations; importing the retired branch would add unrelated work and
    create a second behavioral authority. The immutable snapshot is used only to
    discover and test a specific missing concept.

## 10. Risks and mitigations

| Risk                                                               | Mitigation                                                                                                                                                                                                                                                                                                                                                    |
| ------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| PUC behavior differs from the local contract harness               | keep the harness limited to the public message/helper contract, exercise `h.sendMessage`, and gate the actual externally hosted PUC release on real GAM; do not vendor PUC bytes                                                                                                                                                                              |
| Same-realm publisher code can interfere                            | publisher-realm availability/DOM integrity is an explicit prerequisite; capability checks defend third-party frames, replays, and stale work, not arbitrary same-realm compromise                                                                                                                                                                             |
| Third-party creative regains publisher-origin execution            | top-page mount inside the exact trusted slot binding, naturally opaque nested data documents, outer exact-origin `frame-src`, independent publisher `frame-ancestors 'self'`, and three-browser hostile-navigation fixtures                                                                                                                                   |
| Rc merge silently drops a release-branch behavior                  | first-parent rc ancestry, explicit adoption ledger, conflict inventory, focused baseline-versus-candidate contracts, and full rc suites before feature work                                                                                                                                                                                                   |
| Required template switch breaks an old omitted-field config        | intentional hard cutover; startup rejects omission, examples/fixtures/operators move atomically, and whole-release rollback restores the prior binary/config together                                                                                                                                                                                         |
| Accepted APS overlay obscures later publisher GPT content          | bind the artifact to the exact physical object/host; retire it synchronously for publisher/competing/ambiguous `slotRequested`, after successful publisher destroy, or on exact DOM-integrity loss, while retaining it only for a provably TS-owned replacement until commit                                                                                  |
| Late slot-size observation mutates a newer GPT cycle               | capture runtime-slot/request/element identity, revalidate exact latest filled cycle and live unique binding at commit, disconnect/unsubscribe on disposal, and keep evidence diagnostics-only                                                                                                                                                                 |
| A module activation never returns                                  | activation is generated first-party code with boundary tests; elapsed returning calls fail through monotonic checks, but JavaScript cannot preempt a nonreturning same-thread function                                                                                                                                                                        |
| Strict parsing rejects a future APS field                          | descriptor is versioned; outer transport remains tolerant; add a reviewed version/corpus update rather than silently accepting new semantics                                                                                                                                                                                                                  |
| CSP blocks a legitimate APS creative                               | three-browser real-GAM suite; script creatives remain opt-in; CSP changes are explicit security work                                                                                                                                                                                                                                                          |
| Hard cutover breaks stale pages                                    | accepted compatibility stance; a stale hash fails locally and reload is required; retain the prior deployable binary for whole-release rollback, not N/N-1 routes in the active binary                                                                                                                                                                        |
| Kernel extraction changes unrelated integrations                   | per-integration pre/post behavior corpus, adapter fakes, current full suites, behavioral maximal-bundle test, and exact disposal assertions                                                                                                                                                                                                                   |
| Optional code drifts back into the first-display artifact          | release-catalog phase/dependency validation, production metafile deny paths, semantic first-display budgets, and reviewable named parser-time obligations                                                                                                                                                                                                     |
| Agent and persistent runtime overlap ownership                     | no runtime request before protected paint; effect-inert preparation; one non-yielding quiesce/adopt/commit transaction; exact disposer inventory; browser proof of zero duplicate display/listener/timer/port/wrapper                                                                                                                                         |
| Takeover cannot reconstruct a live GPT slot safely                 | transfer the exact physical object only in the one-use closure-private capsule; validate generation/release and adopt without define/target/display; never serialize or publish that identity                                                                                                                                                                 |
| Persistent takeover fails after a successful first display         | keep accepted DOM inert, reverse partial persistent effects, commit terminal fallback, and never replay the projection or resurrect the provisional agent                                                                                                                                                                                                     |
| A deferred feature is called before its module is ready            | caller deadlines start at original enqueue and may expire while gated; after the paint gate, live waiters share one independently bounded module load with no duplicate/fallback/runtime                                                                                                                                                                      |
| First display never occurs, so deferred work starves               | the owned 10-second post-kernel no-display guard becomes the trigger, followed by bounded idle scheduling or the owned timer fallback                                                                                                                                                                                                                         |
| A page waits past 10 seconds before its first programmatic display | accepted explicit boundary on a no-agent page: correctness still uses persistent owners, but that display may contend with already released later work and is excluded from the protected-load claim                                                                                                                                                          |
| A forged/replaced script registers into the runtime                | exact same-origin catalog URL, release/id/phase match, core-created element identity, `document.currentScript`, single terminal registration, and generation checks                                                                                                                                                                                           |
| Publisher CSP or Trusted Types blocks a later module               | preserve publisher policy; copy only the authenticated parser-inserted nonce, use exact-manifest Trusted Types URLs, and fail to the defined takeover shell or isolated deferred `policy_blocked` state without another sink/runtime                                                                                                                          |
| Phase splitting duplicates product ownership                       | one broker provider per capability, immutable interfaces, takeover-before-deferred dependency rule, agent capsule identity/disposal tests, and no public service locator                                                                                                                                                                                      |
| Phase splitting reduces initial bytes but grows total release size | independent immutable maximal-total raw/gzip/Brotli budget and complete release inventory; splitting alone cannot make the gate pass                                                                                                                                                                                                                          |
| One deferred module stalls unrelated later behavior                | start every independent deferred transaction after the common gate without awaiting siblings; separate deadlines and no deferred-to-deferred capability edges prevent head-of-line blocking                                                                                                                                                                   |
| Retired `rc/july` is mistaken for an implementation source         | never fetch/merge/rebase/cherry-pick its head; validate only immutable `905984e62` as a concept checklist, map retained rows to rc-baseline owners/gaps, and fail the plan/lint if a release or performance gate names `rc/july`                                                                                                                              |
| Diagnostics change ad behavior or overclaim a render               | bounded core-only snapshot ingress, separately bounded GPT fact replay, consumer-specific private presentation capability, isolated public subscribers, honest `gam-only`/`ok` rules, inactive zero-side-effect tests, and no correctness dependency                                                                                                          |
| Bounded registries refuse traffic under extreme churn              | explicit reservation `registry_full` and slot `registry_capacity`, lifecycle pruning, capacity stress tests; never trade correctness for eviction                                                                                                                                                                                                             |
| GPT event attribution remains ambiguous                            | adapter-minted non-reused physical-slot plus per-object cycle identity joins diagnostics only after exact current-slot and unique-cycle resolution; unresolved, stale, or multi-cycle-ambiguous facts are dropped, while lifecycle authority still fails the TS attempt deterministically and never triggers fallback from ambiguous/publisher-owned activity |
| Late async work mutates new SPA state                              | generation checks, owned disposers, terminal latch, and adversarial reversed-order tests                                                                                                                                                                                                                                                                      |
| Browser tests report iframe load but not APS success               | require the bound APS render-completion callback and inspect network/DOM evidence                                                                                                                                                                                                                                                                             |
| APS runner becomes unavailable or stops the callback               | load/rejection/silence fail the attempt; real-browser conformance blocks release and APS disablement is the emergency containment path                                                                                                                                                                                                                        |
| APS runner reports completion incorrectly                          | accepted external trust risk; protected conformance checks DOM/network behavior, but cannot prove future mutable bytes; suspect behavior disables APS                                                                                                                                                                                                         |
| Existing operational signals are weak                              | do not invent telemetry in this spec; hold deployment or write a separate observability design                                                                                                                                                                                                                                                                |

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
9. APS bootstrap and runner-proxy routing, security headers, bounded relay, and failure
   behavior are proven through each real adapter transport, are equivalent across all
   four adapters, and never fall through to publishers. APS runner bytes are neither
   stored in the repository nor required to be identical across different upstream
   fetches.
10. Rust, TypeScript, ESLint, Vitest, hermetic Playwright, adapter parity, and
    real-GAM conformance suites pass.
11. Non-APS Cache/ADM rendering, native Prebid handling, publisher-owned GPT, refresh,
    SRA, and SPA regression suites pass.
12. No new analytics, persistence, billing, experimentation, or deployment-routing
    artifact is added by the implementation plan; rc's existing optional GAM
    attribution behavior is only preserved.
13. `requestAds` accepts only exact server-projected or transactionally registered
    programmatic slot ids, omitted selection is an immutable invocation-time
    registration-order snapshot, and ambiguous internal aliases fail closed without
    affecting valid siblings.
14. Direct and PUC ADM acceptance is possible only for the exact current frame's one
    intended `srcdoc` navigation; initial blank, replacement, removal, stale, late,
    and duplicate events cannot accept.
15. Every retained `RCJ-*` ledger entry finishes `baseline-owned` or demonstrated
    `implementation-gap` with recorded rc-baseline SHA, owner paths, focused test
    path/command/result, one final owner, and a preserved/rebuilt/superseded
    disposition. No proof-pending or coverage-gap row crosses the phase boundary.
    The historical manifest has no unmapped retired TSJS concept, but no retired
    source participates in the build.
16. Late GPT handoff, hydrated/ responsive DOM replacement, Prebid partial-artifact
    recovery and refresh exclusions, creative security, render trace, GPT
    diagnostics, and every remaining TSJS integration pass their complete parity
    suites after the hard cutover.
17. Top mount, outer data container, inner renderer, and descendant creative
    dimensions are exact and unclipped for the winning size, while collapsed-shell correction cannot resize an
    unrelated, anchor, fixed, sticky, disconnected, or already-expanded frame.
18. Programmatic ad units register transactionally into the navigation slot service,
    participate in deterministic direct-auction snapshots, and render through the
    same lifecycle; placeholder rendering and mutable generic runtime configuration
    are absent rather than silently retained as a second path.
19. The committed `TsjsApi` kernel/fallback surfaces, semantic version, exact
    release identity, queue, logger, immutable boot data, exact ordered integration-
    config carrier, attenuated per-module config delivery, and diagnostics presence
    are executable contracts; creative guards auto-install from `CreativeBootV1`
    with no mutable/install global API or separate config transport.
20. Attempt ids require no issued-id history, and reservation/ticket/bootstrap-
    nonce/renderer-nonce registries refuse capacity or collision exhaustion without
    exposing a reusable or cross-role capability.
21. The external Prebid artifact remains free of TS auction/render behavior, exposes
    only its exact own frozen 10.26.0 build stamp, and the TS-owned adapter admits a
    fully prepared bid without partial publication.
22. A page with an eligible server projection requests exactly one parser-blocking,
    server-composed first-display artifact. The immutable batch remains agent-owned
    through terminal settlement and paint, and the reference fixture requests,
    preloads, prepares, and executes no persistent/deferred TSJS artifact before the
    real `tsjs:first-display-paint` mark. The manifest URL hash names the exact
    served agent bytes and stale or malformed hashes fail locally on every adapter.
23. Agent/runtime absence, mismatch, timeout, preparation, or takeover failure
    commits the terminal fallback without replaying or removing an accepted first
    display. Protected paint seals new TS admission; the post-paint callback queue
    drains exactly once against persistent or fallback, with exact
    `initialDisplayCommitted` and no pending/retried work. A deferred module failure
    leaves the same committed persistent kernel and owners alive and settles only
    dependent work through its typed contract.
24. No production-core import graph contains deferred integration/service/UI code,
    no-op/fake/test seams, or `*ForTest` accessors. Every production artifact appears
    exactly once in the release inventory, with the bootstrap role included once and
    every TSJS module included in the maximal non-bootstrap total.
25. The oversized role-correct and mechanical-remediation captures remain immutable
    report-only evidence. Every generated size-admitted first-display mask, including the named
    GPT-reference and APS masks, passes the 90,000/30,000/26,000 raw/gzip/Brotli
    ceilings; all reachable masks are measured and any other closed configuration
    selects direct persistent boot. Bootstrap, persistent reference, and the maximal
    non-bootstrap total pass their independent §5.12 ceilings; and the candidate's semantic pre-action transfer is
    no larger than a fresh rc-baseline build in each encoding.
    Boot-to-first-display passes the automatic fixed-network-profile
    candidate-versus-rc-baseline timing gate, including the candidate's real-mark
    and deferred-order assertions; the APS fixture passes every named
    action/completion/paint and 3/3.75 MiB heap ceiling; rc-baseline and candidate
    retained-heap results are recorded at the same four lifecycle checkpoints and
    each remains within the 4 MiB hard ceiling. The baseline heap shape is
    observability context, not a relative acceptance threshold for the hard-cutover
    kernel and deferred runtime. No gate permits disabled shaping, selective sample reruns,
    candidate self-baselining, or membership loopholes.
    Handoff tests prove a final same-task data snapshot after static preparation,
    monotonic high-water/cycle/trace transfer, one-use capsule, exact physical-slot
    and committed-artifact adoption, zero repeated `display`/`refresh`/iframe insertion,
    and no agent listener, timer, observer, port, wrapper, request authority, or
    strong reference after the synchronous takeover boundary.
26. A deferred module can register only from the exact current core-created local
    release script and can obtain only catalogued frozen capabilities from the one
    runtime. It cannot replace an adapter, slot registry, dispatcher, provider, or
    runtime, including after navigation and failure races. Trusted Types and URL
    failures detected synchronously before insertion isolate as `policy_blocked`;
    nonce/source CSP rejection after insertion is `load_error`, and authenticated-
    node loss is `registration_rejected` unless load failure wins. No policy failure
    widens CSP or selects another execution sink.
    All included deferred transactions begin independently after the shared gate, so
    one module's ten-second deadline cannot delay or consume another's.
27. GPT, APS runner, PUC, and other live upstream script bytes are absent from TSJS
    source, TSJS generated artifacts, fixtures, and browser evidence. The separately
    generated pure external Prebid artifact remains isolated under its §5.6 contract
    and contains no TSJS behavior; Trusted Server's runtime bundles retain only owned
    adapters, proxy/loading contracts, and lifecycle wrappers.
28. The core diagnostics ingress admits and snapshots only the exact bounded data
    tree, exposes no module subscription machinery, and becomes inert on owner
    disposal. `trace.presentation.v1` is a separate consumer-specific capability
    available only to deferred diagnostics presentation; attachment activation,
    replay/live ordering, failure rollback, coalescing, detach, owner disposal, and
    late callbacks cannot affect trace correctness or the 32-live-subscriber public
    limits.
29. Each physical GPT object receives one adapter-minted canonical trace token that
    remains stable through handoff, is never reused within the runtime, and differs
    across redefine/replacement even when publisher identifiers match. Each
    unambiguous physical request admitted to trace projection receives a distinct
    nonreused per-object cycle ordinal, and trace facts join only through that
    compound identity; ambiguous requests are omitted fail-closed. Token/cycle mint,
    ambiguity, collision, map capacity, retirement, late events, navigation, or
    disposal failure can drop only diagnostic projection; it cannot conflate old/new
    impressions or affect GPT behavior and the independently bounded object-identity
    `gpt.events.v1` stream.
30. The implementation branch and replacement PR use the recorded `rc/202608` base;
    the final rc refresh, adoption ledger, conflict inventory, and focused contracts
    prove that no overlapping release-branch behavior was silently lost.
31. `[creative_opportunities].enabled` is required under the hard-cutover schema;
    false disables only publisher/page-bids template delivery, direct `/auction`
    remains functional, all configuration still validates, and inactive HTML gets
    the exact guarded 60-second browser policy without changing surrogate policy.
32. GPT diagnostics expose requested formats, GPT fill, and observed outer size as
    separate copied/frozen fields and labels. Stale cycle, binding, navigation,
    observer, or disposal callbacks cannot alter a newer cycle or ad behavior.
33. Rc GAM attribution remains disabled by default and, when enabled, has exactly one
    parser-time GPT owner that installs `ts=true` before publisher requests without
    a raw-bootstrap flag, activation attribute, duplicate fallback, cleanup on
    refresh/SPA, new telemetry, or effect while disabled.
34. Direct and PUC APS mount only from the trusted top-page owner through the
    bootstrap → outer data container → inner data renderer protocol. No GAM, PUC,
    SafeFrame, bidder, or creative document is an ancestor; the exact-origin CSP and
    publisher `frame-ancestors 'self'` proofs block publisher-origin regain in all
    three browsers. Iframe creatives receive the narrow exact CSP pair; only an
    explicitly enabled script creative adds its one validated creative origin to
    `script-src` at both inherited levels.
35. A no-agent runtime validates and prepares every takeover module synchronously,
    activates parser-time guards and GAM attribution, and commits the one kernel in
    the parser-blocking evaluation before publisher parsing resumes. Agent-page
    takeover may prepare asynchronously only after protected paint and still adopts
    those already-live first-display obligations without overlap.
36. A committed PUC APS overlay retires exactly once before a publisher-owned,
    competing, or ambiguous GPT cycle can expose new content, after successful
    publisher destruction, or when its exact host/node binding is lost. A provably
    TS-owned replacement retains the prior artifact until replacement commit, and no
    retirement path destroys the publisher GPT object or repeats PUC settlement.

## 12. Open implementation decisions

These choices may be resolved in the implementation plan without changing the
architecture:

- exact source-file boundaries inside `kernel/`, `adapters/`, and `services/`;
- whether the canonical descriptor schema is generated from Rust metadata or a
  small neutral schema file, provided all validators share the corpus and staleness
  check;
- exact operational thresholds for the existing binary deployment mechanism.

Rc-baseline authority; the retained `RCJ-*` concept-gap ledger membership and
behavioral dispositions; the public diagnostics namespace; Prebid artifact
independence; integration parity; the one-runtime rule; immutable budgets; and the
first-display/takeover/deferred semantic boundaries are not open implementation
decisions. The implementation plan must map every canonical release-catalog row
above to exact rc-baseline source/build/test steps and preserve each concrete
first-display, parser-time, or later-only obligation. It cannot add, remove, reorder,
or reclassify modules opportunistically to make a budget pass, and it cannot merge or
build retired `rc/july` source.

They may not be resolved by adding compatibility shims, a second runtime, external
telemetry requirements, durable persistence, experiment routing, or a weaker
lifecycle success definition.
