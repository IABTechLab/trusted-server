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
  `append(name, value)` and `replace(name, value)` (v1 is headers-only;
  the cookie operation arrives with the deferred cookie surface, §3) —
  which **core validates and applies**,
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
  "Equal or stronger" is a defined merge over **independent sticky
  directives, not a totally ordered lattice** — `no-cache` and `private`
  are orthogonal constraints (RFC 9111: `no-cache` permits shared
  storage subject to revalidation; `private` forbids shared storage), so
  "replace `private` with the stronger `no-cache`" would make a
  personalized response shared-storable. The merge: each of `no-store`,
  `no-cache`, `private`, `must-revalidate`, `proxy-revalidate`,
  `must-understand`, and `no-transform` is **sticky** — present in the snapshot or the
  mutation ⇒ present in the final response, independently; `public` is
  dropped whenever any restriction is present; `max-age`/`s-maxage` may
  only shrink relative to the snapshot; `stale-while-revalidate`/
  `stale-if-error` may appear only if the snapshot had them **and their
  durations may only shrink** (present-at-1s must not become
  present-at-1y); CDN-specific cache fields (`Surrogate-Control`,
  `CDN-Cache-Control`, host equivalents) are **reserved outright** —
  merging them per-directive on unrestricted responses was a hole (an
  unrestricted `CDN-Cache-Control: max-age=60` could become a year), and
  they are additionally stripped from any restricted response; and the
  final `Vary` is the **union of the complete snapshot `Vary` set** —
  origin-supplied members included, not only core-required ones — and
  the mutation. Middle-stage placement also keeps
  the earlier property: an integration mutation is not silently stripped
  by ordinary core handling — only by the invariant pass, which logs the
  downgrade it applies.

## 3. Collision policy

- Mutators may not touch **reserved surface**, which is defined at two
  granularities because `Set-Cookie` is multi-valued: (a) reserved header
  _names_ — HTTP framing and hop-by-hop headers (`Content-Length`,
  `Transfer-Encoding`, `Connection`, `Trailer`, `Upgrade`, `TE`,
  `Keep-Alive`), **freshness metadata** (`Age`, `Date`, `Expires` — replacing `Age: 59`
  with `Age: 0` on a cached `max-age=60` response, or pushing `Expires`
  into the future, extends downstream freshness in exactly the way the
  monotonic merge forbids for `max-age`, so these are reserved outright
  rather than merged), **representation headers coupled to body bytes
  the hook cannot see** (`Content-Encoding`, `Content-Range`,
  `Content-Type`, `ETag`, `Last-Modified`, `Accept-Ranges`, and digest
  headers —
  relabeling uncompressed bytes as Brotli, or advertising a validator or
  digest for bytes the hook never saw, corrupts responses or poisons
  caches), the `x-ts-*` namespace, and the
  consent/privacy headers core emits; (b) reserved cookie _names_ within `Set-Cookie` — `ts-ec`,
  `ts-eids`, and the other `ts-*` cookies core owns. **Cookie operations are deferred out of the v1 hook — headers only.**
  The write-side gate alone ("persistent cookies require P1") was shown
  insufficient: it never modeled reading, using, forwarding, or
  withdrawing the cookie — a P1-granted-then-withdrawn integration
  cookie would keep arriving on every request with nothing required to
  expire, hide, or stop egressing it, and an advertising-identifier
  cookie needs P4 the contract never expressed. Rather than ship
  "inside the permission model" as a claim the model does not back,
  `append_set_cookie` and the typed cookie builder are **deferred** to a
  follow-up spec whose entry bar is: declared per-cookie required
  permissions, a typed authorized request-side view, stripping from
  unauthorized integration/proxy inputs, mandatory expiry on destructive
  P1 withdrawal, and startup-unique (name, domain, path) ownership.
  Integration IDs are **startup-unique, enforced**: registry
  construction rejects a duplicate ID (current code silently coalesces,
  which corrupts attribution and budgets), with a duplicate-ID test in
  the done-when. Until then the
  operation set is headers-only, and `Set-Cookie` is fully reserved.
  reserved cookie name — in v1 that is every cookie name, since
  `Set-Cookie` is fully reserved (§3 deferral). Violations are rejected
  at the operation layer (§2) and
  logged at `warn` with the integration id. The reserved lists are single
  constants next to the definitions they protect, not duplicated in the
  hook.
- For non-reserved headers, the mutator API distinguishes **append** from
  **replace** explicitly; append/replace legality comes from a **core-owned field registry**,
  not adapter judgment: each known field is classified
  append-legal (genuinely list-valued: `Link`, CSP report groups, …),
  replace-only (singletons: `Content-Language`, …), or rejected;
  **unknown extension fields reject append by default** (replace only) —
  "genuinely list-valued" is not a decision four adapters can make
  independently and identically (`Set-Cookie` is fully reserved in v1 — neither append nor replace). Replacing a
  header the origin set is a deliberate act, visible in the mutator's code.
- Later registrations see earlier mutations (order = registration order,
  which is deterministic).
- **Operation-layer hygiene:** generic `append`/`replace` reject the
  `Set-Cookie` header name outright — cookies go only through
  the deferred cookie builder (when it exists), so cookie validation
  cannot be bypassed by spelling
  the header name in a generic op. Per-integration limits bound total
  operations (≤ 32), added headers (≤ 16), and added bytes (≤ 8 KiB), and
  a **cumulative final-response budget** (≤ 128 headers / ≤ 32 KiB total,
  counting `name: value` plus separators, within any lower adapter
  ceiling) bounds the sum across integrations — enforced in registration
  order, so which operations are rejected when a budget trips is
  deterministic. Each mutator receives an **immutable, redacted snapshot of the
  response head** (status and headers as of its turn, prior integrations'
  accepted operations applied) as its read context; it never holds a
  mutable reference (§2). Redaction is a security boundary, not
  tidiness: the hook runs after core queues the EC `Set-Cookie`, so an
  unredacted view would hand a mutator the raw EC to copy into
  `X-Vendor-Identity` or its own cookie — walking around
  `AuthorizedIdentity<PartnerEgress>` entirely. The snapshot therefore
  **excludes every `Set-Cookie` value and every reserved identity,
  consent, and privacy header value** (names may be listed as present;
  values are withheld).
  Operations arrive as **attributed batches bound to a registration
  ID** — one batch per integration per response, ordered by
  registration, with the security channel's batch (§4a) ordered before
  response mutators; the current flat effects vector satisfies neither
  attribution nor budgets and is restructured accordingly. Validation
  and budgeting are **atomic per batch**: a batch that exceeds its
  budget is rejected whole (logged, attributed), never partially
  applied — item-by-item rejection could apply a security 302's
  `Set-Cookie` while dropping its `Location`. The response itself is
  never rejected. A mutator that returns an error is skipped in full — its
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

| Response                                      | Hook runs?                                                                                                                                                      |
| --------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Processed HTML document (rewritten by TS)     | Yes                                                                                                                                                             |
| Streamed processed document                   | Yes — operations apply to the header block before first byte                                                                                                    |
| Pass-through proxy response (not processed)   | No — TS is a transparent proxy for it                                                                                                                           |
| Served-from-cache processed document          | Yes, applied at serve time (mutations are not cached)                                                                                                           |
| Redirect (3xx)                                | No                                                                                                                                                              |
| Error responses TS itself generates (4xx/5xx) | No                                                                                                                                                              |
| `304 Not Modified`                            | No                                                                                                                                                              |
| `HEAD` of a processed document                | **Yes — header parity with GET is mandatory** (a cache may refresh stored GET metadata from HEAD, and divergent CSP/privacy metadata between the two is a leak) |
| Informational `1xx`, `204`, `205`, `206`      | No — enumerated so adapters do not infer independently                                                                                                          |

This deliberately narrows #782's general "outbound response" phrasing to
processed documents (§6).

## 4a. The security channel — normative closed boundary

The security channel (today: DataDome) is not a general exception; every
degree of freedom is closed:

- **Typed security-cookie operation, not header strings.** The channel
  emits cookies only through a typed operation whose cookie **names come
  from the integration's registered ownership list** (for DataDome, its
  documented cookie); every `ts-*` name is rejected; domain/path scope,
  attributes, size, and lifetime are constrained by the registration.
  Read/vendor-egress/withdrawal semantics of the resulting identifier
  are a ratified security-purpose carve-out — **sign-off item 23** —
  because the tag-injection → cookie/ClientID read → vendor-send
  lifecycle otherwise hands a permission-denied visitor a stable,
  exported identifier. No other request filter inherits the cookie
  capability.
- **Request-header pointers are direction-scoped allowlists.** Values a
  security response names for copying into the request (DataDome's
  header-pointer mechanism) are accepted only from a **documented
  enrichment-header allowlist**; authentication, `Cookie`,
  `Forwarded`/`X-Forwarded-*`, identity, consent, and routing-authority
  fields are rejected by name and by class — a compromised endpoint must
  not replace origin credentials, inject `ts-ec`, or spoof client
  location — and accepted values apply to a **narrowly scoped upstream
  overlay**, never the shared request that later integrations read.
- **Representation rules are decision-scoped.** A _Respond_ decision
  (challenge/deny) **owns its body** and may set representation headers
  (`Content-Type`, encoding, validators) for it — the hook's
  representation reservation exists because ordinary mutators do not own
  the body, and this one does. A _Continue_ decision may not touch
  representation metadata of publisher bytes.
- **One global order, no "wins" exception:** core finalization →
  hook/security effects → **final cache/privacy invariant pass,
  unconditionally last**. The prior DataDome contract's "applies last
  and wins" holds only _within_ the effects layer; nothing outranks the
  invariant pass, or a challenge could combine `Set-Cookie` with public
  caching.
- The channel adopts the shared layers: structured attributed batches
  (§3, atomic per batch — a 302 must never lose `Location` to a budget
  while keeping its cookie; on rejection the channel follows DataDome's
  specified fail-open), reserved header names, budgets, and the
  invariant pass.

## 4. Done-when (from #782, sharpened)

1. Trait + builder + registry application, each public item documented.
2. **The pre-existing `RequestFilterEffects.response_headers` channel
   remains a distinct, core-owned security channel — §4a defines its
   closed boundary.** Folding it into this hook would break its one real
   consumer: DataDome sets headers **and cookies** on 200, 301/302, 401,
   403, and 429 responses — response classes (§3a) this hook never runs
   on, with cookie emission v1 reserves.
3. **At least one real consumer ships in the same PR** — an existing
   integration registering a mutator for a real need (or, failing a real
   need, the feature waits; scaffolding with only self-referential tests is
   dead code and will be removed).
4. Every adapter applies mutations on its outbound path, with a per-adapter
   route test asserting an integration-set header appears in the response.
5. A parity-suite case asserts identical mutation behavior across adapters.
6. Reserved-surface, append/replace, operation-limit, and erroring-mutator
   semantics covered by unit tests.
7. **Every row of the §3a eligibility matrix has a test** — streaming,
   cache-hit, pass-through, redirect, error, and 304 each proven to run or
   not run the hook — not merely one positive header test per adapter.
8. Cache/privacy invariant tests, one per restriction source and shape:
   a **core-owned** cookie already queued before the hook + an
   integration's public `Cache-Control` replacement → private/no-store,
   surrogate stripped; **core-private cookieless** processed HTML +
   public replacement → restriction preserved; **origin-private cookieless** processed HTML that retained the
   origin's cache restrictions + public replacement → restriction
   preserved (pass-through responses never run the hook, §3a); a cache-hit serve re-applying mutations without
   weakening the stored classification; a `Vary` mutation neither
   dropping core-required values nor bypassing the snapshot; each CDN
   directive (`Surrogate-Control`, `CDN-Cache-Control`, host equivalents)
   individually stripped; and a rejected `Content-Encoding` mutation.

## 5. Size and sequencing

This is a modest feature plus tests with zero coupling to the provider
architecture or, in its v1 headers-only form (§3), to the permission
model. It lands whenever its first real consumer is identified (§4.2);
cookie operations arrive only with their own follow-up spec (§3) and its
permission-model coupling. If no consumer
materializes, it does not land; being unblocked is not a reason to ship
scaffolding.

## 6. Divergences from issue #782

This spec supersedes #782 on the following points; the issue is updated to
reference this spec when the PR merges:

| #782 says                                          | This spec says                                                                                                 | Why                                                                                                                  |
| -------------------------------------------------- | -------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------- |
| Mutations apply to the outbound response generally | Eligibility is the explicit §3a matrix, centered on processed documents                                        | Pass-through/error/304 mutation has different semantics per adapter; enumerating beats implying                      |
| Ship the trait + registry + adapter application    | Additionally: structured operations API (§2), reserved surface (§3), and a real consumer in the same PR (§4.2) | PR #838 shipped the trait with zero call sites; an unrestricted `&mut HeaderMap` cannot enforce any collision policy |
