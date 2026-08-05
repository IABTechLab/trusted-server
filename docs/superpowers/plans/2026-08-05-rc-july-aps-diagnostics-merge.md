# rc/july APS Diagnostics Merge Correction Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the merged GPT diagnostics evidence accurate for rc/july's APS,
inline, and PBS Cache creative-response paths without changing publisher code or
the public diagnostics enum surface.

**Architecture:** Match opportunity classification to rc/july's fail-closed APS
renderer precedence. Treat successful `MessagePort.postMessage` as the exact
creative-response boundary, record it before shell resizing or other follow-up
work, and describe the cross-render-source fact as a creative response rather than
markup.

**Tech Stack:** TypeScript, Vitest/jsdom, GPT/PUC MessageChannel bridge, APS renderer
validation, Prettier, ESLint, Vite.

**Spec:**
`docs/superpowers/specs/2026-08-05-rc-july-aps-diagnostics-merge-design.md`

---

## File Map

| File                                                                             | Action | Responsibility                                         |
| -------------------------------------------------------------------------------- | ------ | ------------------------------------------------------ |
| `crates/trusted-server-js/lib/src/integrations/gpt/index.ts`                     | Modify | APS opportunity precedence and exact response boundary |
| `crates/trusted-server-js/lib/test/integrations/gpt/ad_init.test.ts`             | Modify | APS/inline/cache seam regressions                      |
| `crates/trusted-server-js/lib/src/core/types.ts`                                 | Modify | General creative-response comments                     |
| `crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/overlay.ts`       | Modify | Evidence-safe operator wording                         |
| `crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/overlay.test.ts` | Modify | Required generic response wording                      |
| `docs/guide/integrations/gpt-diagnostics.md`                                     | Modify | APS render-source and creative-response semantics      |

The obsolete bootstrap deletion remains untouched. No Rust, Prebid, publisher, GAM,
or configuration runtime file changes belong in this correction.

### Task 1: APS opportunity precedence

**Files:**

- Modify: `crates/trusted-server-js/lib/test/integrations/gpt/ad_init.test.ts:177-274`
- Modify: `crates/trusted-server-js/lib/src/integrations/gpt/index.ts:57-66`

- [ ] **Step 1: Add failing opportunity tests.**

Extend the opportunity cases with a valid `apsRenderer()` carrying its matching
`hb_adid` and expect `renderable_candidate`. Add separate cases proving:

- an invalid present renderer is `unrenderable_candidate` even with valid inline
  markup or cache coordinates;
- a valid descriptor with otherwise valid inline markup or cache coordinates is
  `unrenderable_candidate` when a mocked `apsRendererUrl()` is unavailable.

- [ ] **Step 2: Run the focused test and confirm RED.**

```bash
cd crates/trusted-server-js/lib
npx vitest run test/integrations/gpt/ad_init.test.ts -t "opportunity"
```

Expected: valid APS is reported as `unrenderable_candidate`, and the renderer-URL
case fails because the classifier does not inspect the endpoint.

- [ ] **Step 3: Implement fail-closed classifier precedence.**

When `bid.renderer !== undefined`, return `renderable_candidate` only if both
`validateApsRenderer(bid.renderer)` and `apsRendererUrl()` succeed. Do not fall back
to inline/cache for a rejected present renderer. Preserve the existing inline/cache
logic only when the renderer field is absent.

- [ ] **Step 4: Run the focused test and confirm GREEN.**

Run the Step 2 command. Expected: all opportunity cases pass.

### Task 2: Exact creative-response boundary

**Files:**

- Modify: `crates/trusted-server-js/lib/test/integrations/gpt/ad_init.test.ts:2675-3804`
- Modify: `crates/trusted-server-js/lib/src/integrations/gpt/index.ts:1782-1910`

- [ ] **Step 1: Add failing bridge tests.**

Use the existing message-listener helpers and diagnostics spies to require:

1. Valid direct APS calls request once, posts once, then records the same attempt ID
   as a response before resize follow-up.
2. Invalid APS, including an invalid renderer with valid inline/cache fallback data,
   records `missing_render_source`, posts nothing, and records no response.
3. An unavailable renderer endpoint has the same missing-source result.
4. Throwing APS `postMessage` records exactly one `response_post_failed` on the
   existing attempt and no response.
5. A successful post followed by a forced collapsed-frame resize exception still
   records the response and no `response_post_failed`.
6. Throwing request/response/failure diagnostics writers cannot suppress a valid APS
   response.
7. Inline and asynchronous cache paths record their response before collapsed-frame
   resize work.
8. The registered client-Prebid APS branch calls none of the direct request,
   response, or failure writers.

- [ ] **Step 2: Run the bridge tests and confirm RED.**

```bash
cd crates/trusted-server-js/lib
npx vitest run test/integrations/gpt/ad_init.test.ts -t "creative|APS|response"
```

Expected: APS has no diagnostics assertions to satisfy, and resize-boundary tests
show response recording occurs too late or is misclassified.

- [ ] **Step 3: Implement the minimal boundary correction.**

For direct APS, inline, and cache branches:

- keep only `port.postMessage` in the `response_post_failed` try/catch;
- call `safelyRecordCreativeResponse(attemptId)` immediately after that try succeeds;
- run collapsed-frame resizing and later work only afterward;
- isolate a resize exception so it cannot escape the message handler or alter
  response/failure evidence.

Do not add diagnostics to the registered client-Prebid APS branch.

- [ ] **Step 4: Run the bridge tests and confirm GREEN.**

Run the Step 2 command. Expected: all direct APS, inline, and cache boundary cases
pass.

### Task 3: Generalize the evidence wording

**Files:**

- Modify: `crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/overlay.test.ts`
- Modify: `crates/trusted-server-js/lib/src/core/types.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/overlay.ts`
- Modify: `docs/guide/integrations/gpt-diagnostics.md`

- [ ] **Step 1: Change overlay tests first.**

Require `Trusted Server selected; creative response sent to PUC`,
`Trusted Server selected; no creative response confirmed`, and
`Trusted Server creative response sent at ...`. Reject the generic phrase "markup
response" across both shared selected/response-sent delivery states.

- [ ] **Step 2: Run the overlay test and confirm RED.**

```bash
cd crates/trusted-server-js/lib
npx vitest run test/integrations/gpt_diagnostics/overlay.test.ts
```

- [ ] **Step 3: Update comments, UI, and guide.**

Use "creative response" for the shared evidence boundary. Document a validated APS
descriptor plus renderer endpoint as a render source, its fail-closed precedence,
and that a successful response may contain markup or a renderer descriptor. Keep
markup-specific failure explanations where they describe only markup/cache work.

- [ ] **Step 4: Run overlay tests and document formatting.**

```bash
cd crates/trusted-server-js/lib
npx vitest run test/integrations/gpt_diagnostics/overlay.test.ts
npm run format
cd ../../../docs
npm run format
```

### Task 4: Full verification and review

- [ ] **Step 1: Run affected tests.**

```bash
cd crates/trusted-server-js/lib
npx vitest run \
  test/integrations/gpt/ad_init.test.ts \
  test/integrations/gpt_diagnostics/store.test.ts \
  test/integrations/gpt_diagnostics/api.test.ts \
  test/integrations/gpt_diagnostics/overlay.test.ts \
  test/integrations/gpt_diagnostics/badges.test.ts
```

- [ ] **Step 2: Run the full JS gates.**

```bash
cd crates/trusted-server-js/lib
npm test
npm run lint
npm run format
npm run build
```

- [ ] **Step 3: Run docs and repository formatting gates.**

```bash
cd docs
npm run format
npm run lint
npm run build
cd ..
cargo fmt --all -- --check
git diff --check
```

- [ ] **Step 4: Request independent code review.**

Review APS fail-closed parity, postMessage evidence order, no direct evidence on
client-Prebid APS, exception isolation, wording, and bootstrap-deletion scope.

- [ ] **Step 5: Commit only after all gates and review pass.**

```bash
git add <exact changed files>
git commit -m "Fix APS creative response diagnostics"
```
