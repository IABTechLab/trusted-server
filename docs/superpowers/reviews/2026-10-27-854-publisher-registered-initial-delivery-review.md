# Publisher registration specification and plan review

Reviewer: `openai-codex/gpt-6.1-sol`, one independent read-only worker.

Historical first-review snapshot. Its line references and fingerprint describe
the original draft, not the revised documents. The coordinator verified all four
findings, and the user approved addressing A1 through A4, including ordinary-GAM
fallback for A2. The specification and plan have now been revised for re-review.
This record preserves the original findings and does not approve runtime
implementation; resolution requires the separate second-review result.

Documents: [specification](../specs/2026-10-27-854-publisher-registered-initial-delivery-design.md)
and [implementation plan](../plans/2026-10-27-854-publisher-registered-initial-delivery-plan.md).

Status: complete. Four plan/specification findings. Request changes before runtime implementation.

Target: the two complete untracked files below, at HEAD `5bc38862512c5c71085b5052bc3e528ee9b9987d` in this worktree.

- `docs/superpowers/specs/2026-10-27-854-publisher-registered-initial-delivery-design.md`, abbreviated `spec` below.
- `docs/superpowers/plans/2026-10-27-854-publisher-registered-initial-delivery-plan.md`, abbreviated `plan` below.

Before/after fingerprint: `8a445ee3a3281b61388d6cb2a0107331304c909008f60a5b611c1ece08e44bce`. Matches the assignment and `/tmp/ts-854-planning-review-target.txt`. No repository files changed. Review executed locally as the assigned architecture worker, without delegation.

## Verdict on the overall shape

Explicit registration is a reasonable smallest readiness contract for the stated problem. A slot configuration flag is necessary to stop automatic delivery before the publisher component exists. An exact native object and element avoid pretending an old GPT object can bind to a replacement div. Joining registration with the existing admitted payload and navigation generation is preferable to another fetch or hydration scheduler.

Disabled-load-only v1 is a defensible restriction, given the stated SRA constraint and the assignment's evidence that the affected publisher already disables initial loading. It deliberately gives up enabled-load adoption rather than changing global publisher policy. I would not expand v1 to enabled load. The effective getter/legacy observation and real scoped-SRA behavior still need execution and real-GPT proof; inspection here did not independently validate Google's documentation or the prior live observations.

This is an opt-in API and publisher-adapter project, not a transparent fix for unmodified sites. The specification correctly says so. Confirming that the actual publisher can register before its first display/auction is a deployment blocker. Its current disabled-load setting alone does not establish adoption feasibility. Keeping the real-GPT gate and separate approvals is appropriate. Nothing reviewed supports closing all CWV work in #854.

## Findings

### A1. Assign ownership of shared mapping writes before allowing registered delivery to precede automatic initialization

- Issue: `plan:267-269` tells registered delivery to reuse `divToSlotId` and targeting cleanup. `spec:259-261` allows payload admission as soon as the scheduler accepts it, while automatic initialization retains its later load/frame schedule. Thus a registered delivery can populate the shared maps before that automatic pass. The plan requires skipping registered slots in the automatic loop, but never changes that loop's whole-map replacement contract. Registered-only pages have the same problem when the scheduled automatic pass produces empty maps.
- Evidence: runtime `crates/trusted-server-js/lib/src/integrations/gpt/index.ts:1277-1279` creates fresh maps, and `:1412-1415` replaces the shared maps. Bootstrap does the same at `crates/trusted-server-core/src/integrations/gpt_bootstrap.js:884-885`. Navigation cleanup derives every touched element and configured targeting key from those maps at runtime `index.ts:1091-1108`. Renderer lookup uses the exact mapping plus a configured-prefix fallback at `index.ts:167-224`.
- Consequence: the later automatic pass can erase a registered slot's exact render correlation and targeting-cleanup record. A final publisher ID outside the configured prefix is accepted by the specified API but then has no renderer lookup fallback. Even when the prefix still resolves, navigation can leave stale TS targeting on a publisher-owned slot because its cleanup entry was erased. Correctness would depend on which drain ran last.
- Scope: both initializers, the registration dispatcher, render correlation and navigation cleanup.
- Intent and contracts: preserve independent automatic delivery and exact registered identity without assigning publisher slots to `prevGptSlots` destruction ownership.
- Proposed change: state that an automatic pass replaces only automatic entries and retains current-generation registered entries. Use the registration records to derive those entries or preserve the explicitly tagged subset. Navigation still clears both. Require tests for registered dispatch before automatic frames, the reverse order, a registered-only page, a mixed SPA route and subsequent targeting cleanup. Do not fix this by requiring registered delivery to wait for unrelated automatic DOM resolution.
- Tradeoff: automatic initialization no longer owns whole-map replacement on opted-in pages. No new public API or second registry is required.
- Severity: P2, a concrete shared-state ordering gap to resolve in the plan before coding.

### A2. Choose a bootstrap-only creative policy instead of leaving a known unrenderable candidate as a labeled limitation

- Issue: `plan:313-325` requires matching candidate filtering/scoped delivery in bootstrap-only mode and functional GAM delivery when runtime fails, but treats the missing TS creative bridge as a limitation to label. `spec:310-324` otherwise applies eligible TS bid targeting unconditionally. The documents do not decide what bootstrap does with a qualifying TS candidate whose inline creative can only be served by that missing bridge.
- Evidence: `crates/trusted-server-core/src/integrations/gpt.rs:479-484` explicitly locates the bridge in the JS bundle. Bootstrap `gpt_bootstrap.js:863-866` likewise records maps for a future bundle rather than serving creatives. Runtime `index.ts:1954-1970`, `:2079-2087`, `:2157-2192` and `:2338` installs the message handler and serves the inline `adm` held in `tsjs.bids`. A PUC request for that server bid has no such responder when the GPT runtime never loads.
- Consequence: in this supported failure case GAM can select the targeted TS line item, consume the one initial delivery, and leave an empty creative. Merely documenting the absent bridge does not reconcile the promised functional fallback with the visible-delivery acceptance at `spec:402-405`. This concerns the newly promised registered path, not a request to repair all existing automatic degradation behavior.
- Scope: bootstrap-only registration delivery, bootstrap-to-runtime adoption and browser acceptance expectations.
- Intent and contracts: keep one initial delivery and meaningful GAM fallback without presenting a count-only blank as success.
- Proposed change: choose the contract explicitly. The smallest implementation option is to omit TS candidate targeting in registered bootstrap-only delivery when its required renderer is unavailable, perform one ordinary scoped GAM request, and preserve consumed ownership when runtime later arrives. Test real ordinary-GAM creative content for this case. If preserving TS demand is required, specify the actual available renderer and prove PUC completion without loading the normal runtime. Do not use a synthetic marker as the responder.
- Tradeoff: the smaller option gives up TS bid participation for deliveries made before a compatible renderer is available. The alternative adds bootstrap rendering responsibility and code. Either choice must be reflected in the parity and acceptance rules.
- Severity: P2, an unresolved supported-path contract rather than missing evidence alone.

### A3. Task 8's provider endpoint cannot directly use the existing plaintext Next.js fixture

- Issue: `plan:424-432` requires the existing prebid-server profile and validation, a loopback Next.js API auction route, and the existing Viceroy template. No TLS or test backend mapping is planned. Those pieces do not currently connect as specified.
- Evidence: `crates/trusted-server-core/src/auction/plan.rs:565-582` rejects every provider endpoint whose scheme is not HTTPS. Its backend specification at `:388-397` derives that scheme from the endpoint and sets `certificate_check: true`. Existing framework infra waits on `http://127.0.0.1:${ORIGIN_PORT}` at `crates/trusted-server-integration-tests/browser/helpers/infra.ts:64`. The Next.js Dockerfile runs plain standalone `node server.js` at `fixtures/frameworks/nextjs/Dockerfile:42`, with no TLS setup. The existing Viceroy template has an empty backend table; the generator only injects config stores and optionally overrides publisher origin, not provider transport.
- Consequence: an HTTP loopback endpoint fails configuration compilation; changing only its URL to HTTPS makes the provider attempt TLS against a plaintext server. The required real-Rust winner and targeting gate cannot run through this proposed connection. `[proxy].certificate_check = false` in the existing app fixture does not change the provider's explicit certificate-check field.
- Scope: Task 8 fixture transport, launcher and isolated Viceroy config.
- Intent and contracts: retain the real provider profile and actual Rust admission path without weakening production endpoint validation or assigning bids by hand.
- Proposed change: resolve and source-check a test-only transport arrangement before starting the dependent runtime tasks. For example, investigate whether the Fastly/Viceroy adapter can use a test static backend mapping for a canonical HTTPS provider endpoint to the plaintext loopback fixture. If not, specify a local TLS endpoint and how provider certificate validation will succeed, or review a different real-provider test arrangement. Add the required files/process ownership and a config-plus-provider smoke check to Task 8. Do not leave the first discovery of this incompatibility to the final framework gate.
- Tradeoff: adds a small explicit fixture transport setup. Preserves production validation. Feasibility of the static-backend option is not proven by this review.
- Severity: P2, verified source-level incompatibility in the execution plan.

### A4. Define how pending registrations handle a breakpoint change

- Issue: `spec:154-159` defines `eligibleFormats` as the sizes for the selected breakpoint, but registration stores a snapshot and may wait indefinitely for payload or foreground delivery. The publisher preconditions at `spec:165-176` keep the element and ID stable, not the size eligibility. Dispatch checks at `spec:286-297` never revalidate that snapshot against the publisher's current breakpoint. `plan:270-275` filters against the original snapshot; browser narrowing is explicitly before registration at `plan:367-368`.
- Evidence: the documents support both publisher size mapping and registration-before-payload. They also deliberately hold hidden documents until visible at `spec:299-300`. A publisher can therefore register while a tall format is eligible, cross to a narrower mapping while waiting, and dispatch with the old eligibility without violating any stated precondition.
- Consequence: the new compatibility filter can accept the broad-config winner that the publisher's current mapping excludes. That defeats the reason for adding finite publisher eligibility without requiring any element replacement or duplicate registration.
- Scope: the publisher adapter lifecycle, pending cancellation/replacement and responsive verification.
- Intent and contracts: preserve publisher-owned size mapping and avoid turning TS into another responsive-auction engine.
- Proposed change: document that the adapter must cancel and explicitly re-register pending work when its effective eligibility changes, including a viewport change while payload/foreground delivery is pending. Define the boundary after dispatch, where the initial request is consumed and later lifecycle is publisher-owned. Add a deterministic waiting/hidden resize case with cancellation, replacement and old-token rejection. An alternative is a publisher-owned current-eligibility callback, but that expands the API and is not needed if the cancellation contract is workable.
- Tradeoff: the smaller contract requires a responsive publisher adapter to observe its own mapping changes. TS retains only a snapshot and no new size-mapping engine.
- Severity: P2, a missing lifecycle contract exposed by the asynchronous join.

## Reviewed coverage

Read both target documents completely, including every task, command block, rollout rule and acceptance criterion. Read repository `AGENTS.md` and `AGENTS.override.md`, required worker/reviewer instructions, engineering references, and the linked earlier slot-handoff design.

Inspected the relevant source boundaries and existing tests:

- Configuration and wire construction: `creative_opportunities.rs`, `publisher.rs` initial/seam/`build_slot_json` code, JS `core/types.ts`.
- Delivery: GPT initial-load detector, initial scheduler, changed-ID matcher/wrappers, automatic initializer/fallback, targeting snapshots, source-frame lookup and TS render bridge; bootstrap equivalents.
- Navigation and ownership: SPA accepted-navigation generation, endpoint fallback, automatic DOM wait, shared payload application, `core/first_impression.ts` claim pruning/leases and physical-element identity.
- Prebid: native initial-refresh bypass, publisher claims, scoped subsequent refresh and targeting restoration; existing regression cases for bypass, later auctions, stale navigation and element replacement.
- Existing GPT tests: scheduler acceptance/handoff, SPA generation/empty-response handling, actual bootstrap execution, request/render/bridge tests. These are existing proof locations, not passing results from this review.
- Browser/CI feasibility: initial-render runner, transport fixture, real Prebid/PUC setup and evidence boundaries; Playwright configuration/global setup/teardown, infra helpers, Next.js Dockerfile/config/layout, Viceroy generator/template, integration app configuration and CI workflow.
- Command definitions: JS lint/format/build scripts and Cargo target aliases exist. Proposed new test files and launcher are future artifacts, not claimed available now. The plan already permits locating the authoritative slot configuration guide instead of assuming the suggested path exists.

No architecture finding against the explicit admission gate, generation identity, consumed logical marker, exact native liveness checks, publisher teardown ownership, default-off rollout, excluded-candidate bridge guard or scoped Prebid bypass as specified. These are requirements to prove, not already-verified implementation behavior.

## Checks actually run

Only read-only inspection and target-integrity checks ran. No installs, builds, tests, browser actions, formatting, commits or repository edits.

The exact integrity check ran before and after inspection:

```sh
{ git rev-parse HEAD; git status --short -- docs/superpowers/specs/2026-10-27-854-publisher-registered-initial-delivery-design.md docs/superpowers/plans/2026-10-27-854-publisher-registered-initial-delivery-plan.md; sha256sum docs/superpowers/specs/2026-10-27-854-publisher-registered-initial-delivery-design.md docs/superpowers/plans/2026-10-27-854-publisher-registered-initial-delivery-plan.md; } | sha256sum
```

Result both times: expected fingerprint. Final `git status --short` lists only the two approved untracked documents. Inspection used `read`, `fffind`, `ffgrep` and narrow `rg` commands. Final inspection timestamp was `2026-10-27T15:36:59-05:00`; fingerprint command elapsed time reported zero seconds. Overall reading time was not separately timed.

## Proof level, blockers and next actions

- Effective disabled loading plus explicit refresh isolation: Source/contract evidence only, unproven by execution here. Next checks are the planned strict SRA fixture and separately approved real-GPT canary.
- One request plus a visible compatible creative through bootstrap/runtime/real Prebid: Source/path inspection only, unproven. A1, A2 and A4 need contract decisions, then executable parity and browser checks on the implemented artifacts.
- Real Rust/framework winner path: Source establishes the HTTPS mismatch in A3. Resolve transport first, then execute the dedicated launcher against actual inline and shared-template injection.
- Publisher adoption, production causality, real GAM selection and performance: deferred external gates. No production publisher source or private captures were inspected, and none were assumed to be present in this checkout.

Actionable verdict: keep runtime implementation unapproved until the coordinator resolves A1-A4 in the frozen specification/plan through the user's review process. Then implement the opt-in contract with the listed gates. Real-GPT/publisher adoption can remain a deployment blocker for an explicitly approved default-off feature merge, but cannot become a live-site fix claim.
