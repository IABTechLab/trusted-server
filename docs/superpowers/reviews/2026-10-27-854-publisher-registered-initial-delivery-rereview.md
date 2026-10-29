# Publisher registration specification and plan re-review

Reviewer: `openai-codex/gpt-6.1-sol`, independent read-only worker.

Coordinator disposition: accepted. A1 through A4 are resolved in the design and
implementation instructions. This approves the documents only, not runtime
implementation, deployment or production activation.

Documents: [specification](../specs/2026-10-27-854-publisher-registered-initial-delivery-design.md)
and [implementation plan](../plans/2026-10-27-854-publisher-registered-initial-delivery-plan.md).

Status: complete. Verdict: approve the revised specification and implementation plan. This is documentation approval, not authorization to implement, deploy or activate runtime behavior.

No new architecture or execution-plan findings. No B IDs assigned. A1 through A4 are resolved at the specification/plan level; none is claimed implemented or runtime-proven.

## Target and authority

Worktree branch: `issue-854-publisher-slot-registration`.
HEAD: `5bc38862512c5c71085b5052bc3e528ee9b9987d`.
Both complete untracked documents were read, not just their revisions:

- `docs/superpowers/specs/2026-10-27-854-publisher-registered-initial-delivery-design.md`, 542 lines, called `spec` below.
- `docs/superpowers/plans/2026-10-27-854-publisher-registered-initial-delivery-plan.md`, 756 lines, called `plan` below.

Before/after fingerprint: `12d996037dce8479aaa2e7b13e259b4460c7f2b6a6b95480bb36459e9d5747fe`. Both checks matched the assignment and `/tmp/ts-854-planning-review-v2-target.txt`.

Review executed directly as the assigned worker, without delegation. Read the historical R1 review completely, current `AGENTS.md` and `AGENTS.override.md`, required worker/target/reviewer instructions, engineering references and the earlier slot-handoff specification. Only this report was deliberately written. No repository edits, installs, builds, tests, formatting, browser actions, real provider requests or commits occurred.

## Resolution of the first review

### A1. Resolved: registration records own registered mapping entries

`spec:383-402` makes current-generation registration records the source of registered `divToSlotId` and `prevSlotTargetingKeys` entries. Consumed and cancelled-after-dispatch records retain necessary cleanup/correlation. Automatic passes replace only automatic entries; registered dispatch changes only its entries. No second mutable registered-mapping registry is introduced, and native destruction remains outside TS ownership.

`spec:285-296` and `395-402` require old-generation snapshots before record retirement, deferred cleanup ordered before new delivery, and no merge from old records. `plan:375-389` explicitly carries this into both implementations and requires registered-before-auto, auto-before-registered, registered-only, mixed-SPA, outside-prefix-ID and deferred-cleanup regressions. The framework repetition is at `plan:568-570`.

This addresses the actual source boundary. `crates/trusted-server-js/lib/src/integrations/gpt/index.ts:1277-1279,1412-1415` currently constructs and replaces whole maps; `crates/trusted-server-core/src/integrations/gpt_bootstrap.js:884-885` also replaces them. Runtime cleanup at `index.ts:1074-1108` uses those maps, while exact renderer lookup at `index.ts:167-224` depends on their retained identity. The revised ownership rule removes ordering dependence without delaying registered delivery behind automatic DOM work.

### A2. Resolved: ordinary GAM without TS demand when the real renderer is absent

`spec:360-379` now chooses the approved policy. Runtime publishes `tsRenderBridgeVersion === 1` only after installing the real handler. Bootstrap never advertises or implements that renderer. An absent capability suppresses TS candidate targeting and candidate/render correlation, then allows one ordinary scoped GAM request. GAM fill or explicit no-fill is honest acceptance; guaranteed fill is not promised. Late runtime arrival cannot retarget, refresh or recover the excluded candidate from retained bids. A registration still pending at handler installation can use the candidate at its eventual dispatch.

`plan:390-411` requires actual-handler installation, idempotence, owned candidate-key cleanup and a renderer guard against the retained bid map. `plan:451-464` requires ordinary publisher creative content without a bridge, no late resurrection, and both sides of the pending/consumed boundary. `plan:513-519` keeps this separate from Universal Creative proof.

The policy matches source responsibilities. `crates/trusted-server-core/src/integrations/gpt.rs:479-484` puts creative delivery in the normal JS bridge. Bootstrap `gpt_bootstrap.js:863-866` records correlation for that future bundle, not a renderer. Runtime `index.ts:1954-1970` installs the message listener; `2079-2090` resolves a candidate directly from global bids, and `2157-2210` can return inline ADM and fire beacons. The plan explicitly requires changing that recovery path for excluded registered candidates rather than treating a synthetic marker as a responder. Existing automatic/APS behavior is deliberately preserved.

### A3. Resolved: source-supported static-name transport, with mandatory future preflight

`spec:509-524` and `plan:76-99` choose isolated Viceroy 0.17.0 static backend configuration. The application keeps `https://auction.example.com/api/fixture-auction` and certificate checking. Only generated test configuration maps its predicted backend names to a plaintext loopback origin with no duplicated path prefix.

`plan:105-125` requires reconstruction of the production provider spec from public compiled fields and use of the shared `BackendNamingPolicy::Fastly` prediction/canonicalization policy. It covers every reachable positive reduced-budget bucket as well as the configured timeout, not one copied hash or assumed budget. `plan:127-140` requires an actual baseline-WASM Rust provider request, intended local route, winner admission, observed backend evidence and reduced-budget coverage before delivery implementation. Failure stops dependent work rather than weakening validation. `plan:574-585` reuses this arrangement at the framework gate.

I independently inspected this source chain:

- `crates/trusted-server-core/src/auction/plan.rs:565-582` enforces HTTPS. `376-400` builds the backend specification with endpoint host/port, certificate checking, provider discriminator and both request-local timers. Public compiled provider fields are at `243-258`; the public endpoint string accessor is at `208-215`. A test helper need not export a private production backend constructor.
- `crates/trusted-server-core/src/platform/backend_naming.rs:79-103,352-420,482-501` provides shared prediction and bucket policy. `crates/trusted-server-core/src/auction/orchestrator.rs:454-477` shows why reduced request-local timers must be mapped.
- `crates/trusted-server-adapter-fastly/src/backend.rs:192-197,222-275` predicts the same name, attempts registration and reuses the existing name on the SDK's NameInUse message.
- Installed Fastly SDK 0.12.1, `fastly-0.12.1/src/backend/builder.rs:81-96`, maps ERROR to NameInUse with the matching "already in use" display text. Its `361-364` documents using the pre-existing backend after that result.
- Pinned upstream `https://github.com/fastly/Viceroy/blob/v0.17.0/src/session.rs#L625-L655` looks up static entries and rejects static/dynamic name collisions. `src/wiggle_abi/req_impl.rs:555-574` converts that collision to BackendNameRegistryError. `src/error.rs:216-247` maps it to ERROR.
- Pinned `src/upstream.rs:129-169` connects using the backend URI and its scheme; `275-314` builds the request URI using that scheme and combines the backend base path with the original request path. `src/config/backends.rs:101-110` parses the static backend URL. Thus a root loopback HTTP backend can retain the provider request path without requiring a production HTTP exception.
- The existing plaintext fixture is confirmed by `crates/trusted-server-integration-tests/browser/helpers/infra.ts:64` and `fixtures/frameworks/nextjs/Dockerfile:42`. The generator accepts isolated app-config/template/origin/output inputs at `src/bin/generate-viceroy-config.rs:60-100,124-143`; appending named local backends does not require changing its production validation.

These are Source/Path evidence, not executed runtime success. Task 0 honestly says so and makes the missing proof a preflight stop condition. Missing execution today is residual risk, not an unresolved A3 or new finding.

### A4. Resolved: synchronous publisher-owned eligibility replacement before dispatch

`spec:161-167,185-188` requires the adapter to keep pending eligibility current, synchronously cancel/re-register from its own mapping/visibility handlers, and not rely on a later passive React effect. Set-equivalent reorderings remain idempotent. `spec:404-425` requires per-record token checks, stale callback/handle rejection, and a consumed boundary after dispatch. TS gains neither viewport polling nor another responsive size engine.

`plan:279-284,334-339` covers unchanged native/element identity with changed eligibility, waiting-payload and hidden-document replacement, forced old callbacks and old-handle cancellation, one eventual current-eligibility request and no reopening after dispatch. `plan:562-567` requires the actual framework adapter and corresponding lifecycle tests.

This is consistent with the source timing problem. Runtime scheduler `crates/trusted-server-js/lib/src/integrations/gpt/index.ts:802-824` admits then defers work through load/frames; SPA `1699-1734` joins asynchronous route data. Existing physical-element claims can be pruned after identity loss at `crates/trusted-server-js/lib/src/core/first_impression.ts:53-69`, so the planned logical consumed marker and separate record tokens remain necessary. The revisions assign responsiveness to the publisher that owns the mapping, not to TS.

## Whole-plan assessment and coverage

The opt-in API remains a narrow solution to a publisher-readiness race. It does not claim that load, frame count, temporary DOM stability or size-array aliasing proves readiness. Accepted payload and registration join through existing navigation identity; no second fetch scheduler is added. Native object/element identity, logical consumed identity, cancellation and renderer selection have distinct responsibilities.

Inspected configuration/wire construction, runtime and inline automatic delivery, navigation cleanup, first-impression claims/pruning, creative source-frame lookup and bridge delivery, initial-load detection/scheduling, and Prebid initial-refresh bypass. Source `crates/trusted-server-js/lib/src/integrations/prebid/index.ts:2660-2676` supports the proposed scoped initial bypass while preserving later auctions. Read existing bootstrap execution and scheduler test mechanisms and the offline runner's real Prebid/PUC setup. Inspected framework infra/setup/teardown, isolated config generation/template, fixture dependencies, Cargo aliases, JS scripts and browser CI wiring for plan feasibility. Proposed new files/commands are future implementation artifacts, not checks available or passed now.

The larger fixture workload is justified by the actual risk: native request counts alone cannot prove a compatible visible creative or framework element lifetime. The plan keeps transport, deterministic delivery, actual Rust/framework injection and real-GPT evidence separate. Task 0 is correctly ahead of dependent delivery implementation. No new provider implementation, production certificate bypass, bootstrap renderer or build-system migration is required.

External publisher adoption is explicitly outside this repository at `spec:213-216`. Rollout pairs protocol/artifacts/configuration and forbids mid-document ownership switching at `spec:447-462`. `plan:712-743` requires separate staging approval, verifies actual adapter adoption before the first display/auction, and blocks a live-site fix claim when real-GPT validation is unavailable. A default-off merge is not misrepresented as a deployed fix or closure of all #854 work. Runtime implementation remains unapproved under `plan:3-5,755-756`.

## Checks and outcomes

Only read-only inspection and integrity checks ran. No implementation tests or runtime checks were run or reused as proof.

Exact integrity command, run before and after inspection:

```sh
{ git rev-parse HEAD; git status --short -- docs/superpowers/specs/2026-10-27-854-publisher-registered-initial-delivery-design.md docs/superpowers/plans/2026-10-27-854-publisher-registered-initial-delivery-plan.md; sha256sum docs/superpowers/specs/2026-10-27-854-publisher-registered-initial-delivery-design.md docs/superpowers/plans/2026-10-27-854-publisher-registered-initial-delivery-plan.md; } | sha256sum
```

Result both times: expected fingerprint. The final check completed at `2026-10-27T15:56:24-05:00`, elapsed 0.1 seconds for the combined fingerprint/time/status command. Whole review reading time was not separately timed.

Other inspection used `read`, `fffind`, `ffgrep` and narrow `rg` queries. `wc -l` confirmed 542/756 lines. `git diff --stat` and `git diff --cached --stat` showed no tracked changes. Final status retained the two untracked target files and historical untracked reviews directory.

Pinned public upstream retrieval, all successful, used this command separately for each of `src/session.rs`, `src/wiggle_abi/req_impl.rs`, `src/error.rs`, `src/upstream.rs`, `src/config/backends.rs`:

```sh
curl -fsSL --max-time 30 https://raw.githubusercontent.com/fastly/Viceroy/v0.17.0/<path> # allow-domain: raw.githubusercontent.com
```

No requests went to an ad provider. Initial source discovery tried a nonexistent `platform/backend.rs` and a nonexistent local Viceroy registry source directory; corrected to `platform/backend_naming.rs` and the public pinned upstream. Those were lookup misses, not behavioral check failures.

## Residual risk and next checks

1. Static-name reuse must actually carry a real Rust provider win to the local Next.js route across full/reduced transport budgets. Highest evidence here: Source/Path, unproven by execution. Next check: Task 0's native helper tests and `./scripts/test-publisher-registration.sh --transport-smoke`. Do not begin dependent delivery implementation if that gate fails.
2. One admitted initial delivery must preserve identity, eligible targeting and actual creative content across mixed mapping writers, cancellation/hidden responsiveness and bootstrap/runtime handoff, with no excluded-candidate resurrection. Highest evidence here: Source/Path, unproven by execution. Next checks: the planned parity/unit/browser/framework gates, followed by separately approved real-GPT staging and external adapter adoption.

These are explicit implementation/deployment gates, not reasons to reject a source-supported plan before it is authorized for execution. No blocking questions remain for this review. Parent retains edits, checks and authorization decisions.

## Coordinator closeout

After the full review, the coordinator changed only the two Status paragraphs to
link this persisted review, mark the documents re-reviewed and retain the explicit
prohibition on runtime implementation. The reviewer acknowledged that follow-up,
read both paragraphs and independently confirmed unchanged document bodies after
omitting the Status paragraph with the recorded normalization.

Final two-file fingerprint: `a71d53e7e5f0ba71960eee504095c542492e1b3909598ae16b9f744c6d38bfbf`.
The full reviewed-design fingerprint above remains the record of the original
re-review target; the final fingerprint includes only this status metadata change.
Documentation approval stands, with no new findings or unresolved A1-A4 decisions.

Prettier, local Markdown links, untracked-document whitespace and `git diff --check`
passed. Only the specification, plan and two review records are present as worktree
changes. No runtime files, production configuration or original-checkout changes
were made. No implementation tests or provider smoke ran during this revision.
The future transport, parity/framework and real-GPT/adoption gates remain required.
