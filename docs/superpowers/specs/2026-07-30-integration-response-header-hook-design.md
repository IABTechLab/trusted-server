# Design Spec: Integration Response-Header Hook

**Status:** Draft
**Author:** Engineering
**Issue references:** #782
**Related specs:** `2026-07-30-pluggable-providers-design.md`
**Last updated:** 2026-07-31

> **Context.** Issue #782 already specifies this feature well; its done-when
> is the contract. PR #838 shipped the trait and registry wiring with **no
> adapter call site** — `apply_response_headers` had zero production
> callers, so the feature existed only in its own unit test. This short spec
> restates the contract plus the two details the issue left open (ordering
> and collision policy), and adds the rule that prevents a repeat: the hook
> lands with a consumer or not at all.

---

## 1. Overview

Integrations can today rewrite request-path behavior (proxies, attribute
rewriters, head injectors) but cannot mutate **response** headers. The hook
adds that: an integration registers a response-header mutator via its
`IntegrationRegistration` builder, and every adapter applies all registered
mutators to the outbound response for HTML document responses it processed.

## 2. Contract

- `IntegrationRegistration::builder(ID).with_response_mutator(...)` registers
  a mutator; `IntegrationRegistry::apply_response_headers(...)` applies all
  registered mutators in registration order.
- **The mutator API is structured operations, not header-map access.** A
  mutator returns (or is handed a recorder for) typed operations —
  `append(name, value)`, `replace(name, value)`,
  `append_set_cookie(cookie)` — which **core validates and applies**,
  attributing each to its integration id. PR #838's shape handed the
  integration an unrestricted `&mut HeaderMap`, which makes §3's collision
  policy unenforceable by construction: core cannot validate or attribute
  writes it never sees. An API that cannot express a violation beats one
  that promises to catch it.
- **Every adapter calls the apply point** on its outbound-response path for
  processed documents. The call site lives in shared response-finalization
  code where one exists; where adapters finalize independently, each adapter
  gains the call and a test proving it.
- **Ordering is three stages, and the last one is inviolable:** core
  response-header handling (EC Set-Cookie emission, EC header clearing,
  privacy headers) → integration operations → **final cache/privacy
  invariant enforcement**, which no integration operation can override.
  Running the hook dead-last would be wrong: current `main` deliberately
  runs cookie-cache protection _after_ arbitrary header changes, stripping
  surrogate caching and forcing private/no-store on any response that sets
  a cookie — a hook applied after that recheck could combine an appended
  `Set-Cookie` with a replaced public `Cache-Control` into a
  **shared-cacheable cookie response**. The invariant pass therefore runs
  after all mutations, unconditionally — and it enforces more than the
  cookie rule. Core **snapshots the complete pre-hook cache restriction
  state** — whether the restriction came from core's own classification
  (processed auction HTML is marked private even when no cookie is
  emitted; today's final helper returns early without `Set-Cookie`) **or
  from the origin** (an origin-supplied `private, no-store` that core
  merely passed through) — and the post-hook response may only be
  **equal or stronger** on the privacy axis: integrations can tighten
  caching, never loosen it, regardless of which header they replaced.
  Every CDN/surrogate cache directive (`Surrogate-Control`,
  `CDN-Cache-Control`, host-specific equivalents) is stripped from any
  restricted response. Middle-stage placement also keeps
  the earlier property: an integration mutation is not silently stripped
  by ordinary core handling — only by the invariant pass, which logs the
  downgrade it applies.

## 3. Collision policy

- Mutators may not touch **reserved surface**, which is defined at two
  granularities because `Set-Cookie` is multi-valued: (a) reserved header
  _names_ — HTTP framing and hop-by-hop headers (`Content-Length`,
  `Transfer-Encoding`, `Connection`, `Trailer`, `Upgrade`, `TE`,
  `Keep-Alive`), **representation headers coupled to body bytes the hook
  cannot see** (`Content-Encoding`, `Content-Range` — relabeling
  uncompressed bytes as Brotli, or stripping the encoding from compressed
  bytes, corrupts the response), the `x-ts-*` namespace, and the
  consent/privacy headers core emits; (b) reserved cookie _names_ within `Set-Cookie` — `ts-ec`,
  `ts-eids`, and the other `ts-*` cookies core owns. An integration may
  append its own `Set-Cookie` values; it may not set or expire a reserved
  cookie name. Violations are rejected at the operation layer (§2) and
  logged at `warn` with the integration id. The reserved lists are single
  constants next to the definitions they protect, not duplicated in the
  hook.
- For non-reserved headers, the mutator API distinguishes **append** from
  **replace** explicitly; the default is append (for `Set-Cookie`, append is
  the only non-reserved operation — replace is not offered). Replacing a
  header the origin set is a deliberate act, visible in the mutator's code.
- Later registrations see earlier mutations (order = registration order,
  which is deterministic).
- **Operation-layer hygiene:** generic `append`/`replace` reject the
  `Set-Cookie` header name outright — cookies go only through
  `append_set_cookie`, so its validation cannot be bypassed by spelling
  the header name in a generic op. Per-integration limits bound total
  operations, added header count, and added header bytes, and a
  **cumulative final-response budget** (total header count and bytes)
  bounds the sum across integrations — enforced in registration order, so
  which operations are rejected when the budget trips is deterministic.
  Exceeding a limit rejects the excess operations (logged, attributed),
  never the response. A mutator that returns an error is skipped in full — its
  operations are all-or-nothing — and the response proceeds without it.
  **Panics are forbidden and fatal, not recoverable**: the primary target
  (`wasm32-wasip1`) builds with `panic = "abort"`, so there is no unwind
  boundary to catch at — a spec that promised panic recovery would be
  unimplementable there. Mutators are infallible-by-construction or
  return `Result`; a panic is a bug that takes the instance down, same as
  anywhere else in the request path.

## 3a. Response eligibility — normative

Which responses the hook runs on, enumerated so two implementations cannot
diverge silently:

| Response                                      | Hook runs?                                                   |
| --------------------------------------------- | ------------------------------------------------------------ |
| Processed HTML document (rewritten by TS)     | Yes                                                          |
| Streamed processed document                   | Yes — operations apply to the header block before first byte |
| Pass-through proxy response (not processed)   | No — TS is a transparent proxy for it                        |
| Served-from-cache processed document          | Yes, applied at serve time (mutations are not cached)        |
| Redirect (3xx)                                | No                                                           |
| Error responses TS itself generates (4xx/5xx) | No                                                           |
| `304 Not Modified`                            | No                                                           |

This deliberately narrows #782's general "outbound response" phrasing to
processed documents (§6).

## 4. Done-when (from #782, sharpened)

1. Trait + builder + registry application, each public item documented.
2. **At least one real consumer ships in the same PR** — an existing
   integration registering a mutator for a real need (or, failing a real
   need, the feature waits; scaffolding with only self-referential tests is
   dead code and will be removed).
3. Every adapter applies mutations on its outbound path, with a per-adapter
   route test asserting an integration-set header appears in the response.
4. A parity-suite case asserts identical mutation behavior across adapters.
5. Reserved-surface, append/replace, operation-limit, and erroring-mutator
   semantics covered by unit tests.
6. **Every row of the §3a eligibility matrix has a test** — streaming,
   cache-hit, pass-through, redirect, error, and 304 each proven to run or
   not run the hook — not merely one positive header test per adapter.
7. Cache/privacy invariant tests, one per restriction source and shape:
   cookie appended + public `Cache-Control` replacement → private/no-store,
   surrogate stripped; **core-private cookieless** processed HTML +
   public replacement → restriction preserved; **origin-private
   cookieless** pass-through-classified content + public replacement →
   restriction preserved; a cache-hit serve re-applying mutations without
   weakening the stored classification; a `Vary` mutation neither
   dropping core-required values nor bypassing the snapshot; each CDN
   directive (`Surrogate-Control`, `CDN-Cache-Control`, host equivalents)
   individually stripped; and a rejected `Content-Encoding` mutation.

## 5. Size and sequencing

This is a ~150-line feature plus tests, with zero coupling to the provider
architecture or the permission model. It lands as its own small PR **when
its first real consumer is identified** (§4.2) — at any point in the epic's
sequence, blocking nothing and blocked by nothing. If no consumer
materializes, it does not land; being unblocked is not a reason to ship
scaffolding.

## 6. Divergences from issue #782

This spec supersedes #782 on the following points; the issue is updated to
reference this spec when the PR merges:

| #782 says                                          | This spec says                                                                                                 | Why                                                                                                                  |
| -------------------------------------------------- | -------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------- |
| Mutations apply to the outbound response generally | Eligibility is the explicit §3a matrix, centered on processed documents                                        | Pass-through/error/304 mutation has different semantics per adapter; enumerating beats implying                      |
| Ship the trait + registry + adapter application    | Additionally: structured operations API (§2), reserved surface (§3), and a real consumer in the same PR (§4.2) | PR #838 shipped the trait with zero call sites; an unrestricted `&mut HeaderMap` cannot enforce any collision policy |
