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
  present-at-1y); CDN-specific cache fields are **reserved outright, by enumerated
  name in the field registry** — `Surrogate-Control`,
  `CDN-Cache-Control`, `Cloudflare-CDN-Cache-Control`, and
  `Edge-Control`, each individually tested ("host equivalents" was not
  a matching rule four adapters would implement identically) —
  merging them per-directive on unrestricted responses was a hole (an
  unrestricted `CDN-Cache-Control: max-age=60` could become a year), and
  they are additionally stripped from any restricted response; and the
  final `Vary` is the **union of the complete snapshot `Vary` set** —
  origin-supplied members included, not only core-required ones — and
  the mutation. Parsing itself is a **shared core parser with
  fail-closed normalization**, not four adapter interpretations:
  invalid `Cache-Control` syntax normalizes to the most restrictive
  reading; duplicate directives keep the strongest; quoted and unquoted
  forms are equivalent; conflicting `max-age` values keep the smallest;
  unknown extension directives are dropped at merge; and `Vary: *` is
  treated as uncacheable-by-shared-caches (no-store-equivalent for the
  invariant). Conformance fixtures cover each rule. Middle-stage placement also keeps
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
  `Set-Cookie` is fully reserved in v1 (§3 deferral). Violations are
  rejected
  at the operation layer (§2) and
  logged at `warn` with the integration id. The reserved lists are single
  constants next to the definitions they protect, not duplicated in the
  hook.
- For non-reserved headers, the mutator API distinguishes **append** from
  **replace** explicitly; append/replace legality comes from a **core-owned field registry**,
  not adapter judgment — and the v1 registry admits **inert fields
  only**: "headers-only" is not automatically permission-neutral, since
  `Link` (preload/prefetch), `Reporting-Endpoints`/NEL, CSP report
  directives, and `Refresh` cause browser-initiated vendor contact on
  requests that granted nothing. Fields with active egress side effects
  are **rejected in v1**; a follow-up may admit them behind declared
  required permissions gated at mutation time. Within the inert set,
  each field is classified append-legal (genuinely list-valued),
  replace-only (true singletons — e.g. `Content-Location`, `Retry-After`;
  an earlier draft miscited `Content-Language`, which is list-valued),
  or rejected; **unknown extension fields are rejected entirely in v1**
  (neither append nor replace — their side-effect class is unknowable).
  The v1 registry is enumerated here, not delegated: **admitted** —
  `Cache-Control` (monotonic merge per this section), `Vary` (union
  merge), `Content-Language` (append), `X-Robots-Tag` (append),
  `Retry-After` (replace-only), `Content-Location` (replace-only);
  everything else known is classified reserved or rejected by the rules
  above, and growing the admitted set is a spec change to this list (`Set-Cookie` is fully reserved in v1 — neither append nor replace). Replacing a
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
  registration, with the security channel's batch (§4a) ordered **after**
  ordinary response mutators — one global order, core finalization →
  ordinary mutators → security effects → invariant pass — so the
  security layer's precedence over publisher-facing mutations holds
  without a second ordering claim; the current flat effects vector satisfies neither
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

| Response                                          | Hook runs?                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| ------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Processed HTML document (rewritten by TS)         | Yes                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                    |
| Streamed processed document                       | Yes — operations apply to the header block before first byte                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| Pass-through proxy response (not processed)       | No — TS is a transparent proxy for it                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                  |
| Served-from-cache processed document              | Serves the **persisted post-hook finals** captured at cache fill (revision-versioned; mismatch = miss) — mutators run at fill/processing time only, so a normal hit and a conditional hit return identical policy metadata **by construction**, with no purity or determinism assumption about mutators                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                |
| Redirect (3xx)                                    | No                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                     |
| Error responses TS itself generates (4xx/5xx)     | No                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                     |
| `304 Not Modified` for a processed representation | **Persisted-metadata pass**: when the processed 200 was cached, its **final post-hook header set** (the cache-relevant fields) was persisted alongside the representation, **versioned by (integration-registry revision, config revision, invariant revision)**; a 304 — like a **normal cache hit** (§3a), which serves the same persisted finals rather than re-running mutators — re-emits them only when all three match the serving instance; a mismatch is a **cache miss**. Identity of normal-hit and conditional-hit metadata holds by construction (one persisted artifact serves both), not by an unsupported determinism claim about mutators. "Cache-relevant fields" is defined: the registry-admitted mutable set plus the Cache-Control family and `Vary`; `Set-Cookie` and validators follow ordinary 304 rules and are never replayed. Absent metadata → cache miss |
| `HEAD` of a processed document                    | **Yes — header parity with GET is mandatory** (a cache may refresh stored GET metadata from HEAD, and divergent CSP/privacy metadata between the two is a leak)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                        |
| Informational `1xx`, `204`, `205`, `206`          | No — enumerated so adapters do not infer independently                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 |

This deliberately narrows #782's general "outbound response" phrasing to
processed documents (§6).

## 4a. The security channel — normative closed boundary

The security channel (today: DataDome) is not a general exception; every
degree of freedom is closed:

- **Typed security-cookie operation with a concrete lifecycle, not
  header strings.** The channel emits cookies only through a typed
  operation, and the registration is not a placeholder — for DataDome
  it pins, **aligned to documented vendor behavior where hardening was
  not intended**: cookie name exactly `datadome`; `Domain` per
  DataDome's own guidance (the module sets it; TS validates it does not
  exceed the registrable domain, computed against the **vendored
  Mozilla PSL snapshot** `docs/superpowers/specs/psl-snapshot-ref.md` —
  ICANN + private sections, IDNA-mapped; IP-literal or single-label
  hosts fall back to host-only), path `/`; `Secure` mandatory;
  `SameSite` configurable `Lax` (default) / `Strict` / `None`
  (`None` requires `Secure`), matching the vendor's endpoint options;
  the returned `Domain` must additionally **domain-match the current
  request host** per RFC 6265 (domain-match and the PSL boundary check
  are separate requirements) and a vendor cookie using `Expires` is
  normalized to its Max-Age equivalent (both present → `Max-Age` wins,
  per RFC 6265); a normalized lifetime exceeding the ceiling rejects
  the whole operation batch;
  `Max-Age` at most **31,536,000 seconds** (the vendor's one-year cap —
  the earlier 396-day figure exceeded it); size ≤ **512 bytes**
  (DataDome's current Fastly-module limit; 4 KiB was ours, not theirs).
  Where the contract **is** deliberately narrower than the vendor — the
  spec-pinned pointer allowlist starting at ClientID-only against
  DataDome's mandatory response-directed mapping set — that reduction
  needs explicit product **and vendor** acceptance: sign-off item 28;
  a violating operation is rejected whole (the batch rule). Every
  `ts-*` name is rejected. **Read is owner-only, and the strip inventory is exhaustive, not
  integration-scoped** — the browser sends `datadome` in the ordinary
  `Cookie` header, so it is removed from **every non-DataDome surface**:
  other integrations' request views, publisher-origin proxy forwarding,
  proxy/click/Testlight upstreams, auction/page-bids request
  serialization, and logs (redaction list) — each surface a tested row
  of the inventory; only the security channel itself observes it; vendor egress goes only to DataDome
  endpoints; deletion is always possible; and whether TS's own
  destructive withdrawal also expires it is exactly the open half of
  **sign-off item 23** — the carve-out is _pending ratification_, not
  ratified, and the permission inventory's cookie deferral stands until
  it closes. No other request filter inherits the cookie capability.
- **The incoming `X-DataDome-ClientID` request header is owner-only,
  like the cookie.** DataDome prioritizes the header over the cookie,
  so leaving it in the shared request would hand other integrations and
  upstream routing the same identifier the cookie boundary strips: core
  **extracts it into the DataDome-only view and removes it from the
  shared request** before integrations and upstream routing run —
  it joins `RedactedRequestView`'s enumerated strip set (providers
  spec) — and only DataDome-returned overlay data reaches the
  publisher, never the raw browser-supplied header.
- **Request-header pointers are a positive, enumerated allowlist.**
  "Documented enrichment headers" is not enforceable; the registration
  enumerates the exact names from the **checked-in allowlist file
  `docs/superpowers/specs/datadome-header-allowlist.md`** — spec-pinned
  today to exactly **`X-DataDome-ClientID`**; every other `X-DataDome-*`
  field is rejected until a reviewed commit adds it to that file
  ("documented enrichment set, listed one by one" without an actual list
  was a wildcard whose contents could change outside the spec) —
  resolving what was a contradiction:
  ClientID propagation is required by the existing DataDome contract
  and test, and its identity-class nature is precisely why it applies
  only to an **owner-scoped publisher-upstream overlay**, never the
  shared request that later integrations read, with its vendor egress
  ratified under sign-off 23. Everything else — authentication,
  `Cookie`, `Forwarded`/`X-Forwarded-*`, other identity, consent, and
  routing-authority fields — is rejected by name and by class: a
  compromised endpoint must not replace origin credentials, inject
  `ts-ec`, or spoof client location.
- **Browser-response headers are a decision-scoped positive allowlist
  too** — request pointers were enumerated, response headers were not,
  leaving either the six-field ordinary registry (which would reject a
  challenge's `Location`) or an open door. Normatively, per decision:
  a _Respond_ (challenge/deny) may set exactly `Location` (replace;
  3xx only), `Content-Type` (its own body, per the representation rule
  below), `Cache-Control`/`Pragma` (through the restricted merge; the
  invariant pass still runs last), the typed security cookie (above),
  and the vendor response headers enumerated in the **response section
  of `datadome-header-allowlist.md`**; a _Continue_ may set only the
  typed cookie and those enumerated vendor headers. Everything else is
  rejected — the atomic-302 example's `Location` is hereby admitted
  rather than assumed.
- **Representation rules are decision-scoped.** A _Respond_ decision
  (challenge/deny) **owns its body** and may set representation headers
  (`Content-Type`, encoding, validators) for it — the hook's
  representation reservation exists because ordinary mutators do not own
  the body, and this one does. A _Continue_ decision may not touch
  representation metadata of publisher bytes.
- **One global order:** core finalization → ordinary mutators →
  security effects → **final cache/privacy invariant pass,
  unconditionally last**. Security precedence over publisher-facing
  mutations comes from its position, not a "wins" rule; nothing outranks
  the invariant pass, or a challenge could combine `Set-Cookie` with
  public caching. The older DataDome spec's "applies last, after
  finalization" wording is **superseded by this order** — updating that
  document is a done-when item, since as written it would place DataDome
  after the invariant pass and reopen the public-cache-plus-cookie bug.
- The channel adopts the shared layers: structured attributed batches
  (§3, atomic per batch — a 302 must never lose `Location` to a budget
  while keeping its cookie), reserved header names, budgets, and the
  invariant pass — with one sequencing rule fail-open depends on: the
  complete challenge batch is **validated and budgeted before the
  Respond decision commits**, so a rejection converts to Continue while
  the publisher route is still available; discovering the rejection
  after Respond has short-circuited routing would leave nothing to fail
  open _to_.

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
   dropping core-required values nor bypassing the snapshot; each of the four enumerated CDN fields (`Surrogate-Control`,
   `CDN-Cache-Control`, `Cloudflare-CDN-Cache-Control`, `Edge-Control`)
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
