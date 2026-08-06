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
- Every registration carries a nonzero `behavior_revision: u32`, bumped for
  any change to its response decision, operation semantics, declared read set,
  or security field list. Integration IDs match
  `[a-z0-9][a-z0-9-]{0,63}` and are unique. The registry revision hashes the
  ordered registration list — order is behavior, so the array is never sorted.
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
  personalized response shared-storable. The merge: each of the **six sticky directives** — `no-store`, `no-cache`,
  `private`, `must-revalidate`, `proxy-revalidate`, `no-transform` — is
  present-in-snapshot-or-mutation ⇒ present-in-final (this
  "snapshot or mutation ⇒ final" rule scopes to exactly these six) — with two refinements. First, `must-understand` is the deliberate
  exception: **mutation-introduced `must-understand` is rejected**
  (snapshot-present survives untouched), because under RFC 9111
  §5.2.2.3 a cache that understands the status may then ignore an
  accompanying `no-store` — "adding" it weakens a stored `no-store`, so
  it is not additive. Second, stickiness is **form-preserving, not
  name-presence-only**: an **unqualified directive never becomes
  field-qualified, and a qualified field set never shrinks** —
  `private` → `private="Set-Cookie"` keeps the directive name while
  authorizing shared storage of everything but one field (RFC 9111:
  qualified `private`/`no-cache` have materially weaker semantics), so
  a mutation supplying a qualified form where the snapshot is
  unqualified keeps the snapshot's bare form, and qualified snapshot
  sets may only grow; `public` is
  dropped whenever any restriction is present; **request-side authority
  is part of the invariant** — if the request carried `Authorization`
  and the origin did not itself authorize shared reuse (no `public`,
  `must-revalidate`, or `s-maxage` from the origin), an integration may
  not introduce `public`, `must-revalidate`, or `s-maxage` (RFC 9111
  §3.5 makes those the very directives that unlock shared caching of
  authenticated responses); the invariant pass forces `private,
no-store` in that case regardless of the mutation; `max-age`/`s-maxage` may
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
  the mutation. Ordering is normative so the final `Vary` reaches TS's
  own cache key, not just the wire: **mutation/invariant → final `Vary`
  computation → cache-key construction → body/metadata commit** — with
  three identity rules. Cache matching uses the **exact final publisher
  request** (post-overlay view; keying from the redacted view would
  collapse personalized variants).

  The final `Vary` name list has one cross-adapter grammar. Core collects every
  `Vary` response field line from the snapshot plus mutation, parses each as an
  HTTP comma-list, trims optional whitespace around every member, and requires
  each non-empty member to be an RFC field-name token. It lowercases ASCII,
  removes duplicates, and sorts unique names by unsigned ASCII byte order. An
  empty member or invalid token introduced by a mutation invalidates that
  batch. If the reverted snapshot itself is malformed, the invariant replaces
  the final value with `Vary: *`, forces `no-store`, and writes no cache
  artifact. `*` in either source likewise dominates every other member: the
  normalized result is the single `*` and is uncacheable. For an ordinary
  list, the wire response emits one lowercase `, `-joined value, while the
  variant descriptor stores `vary_names` as the exact sorted JSON array of
  lowercase strings. Thus field-line grouping, case, and input ordering cannot
  produce different cache identities.

  A normalized name nominates one request header. Digest construction obtains
  **all** values for that header from the exact final publisher request after
  overlay, preserving received field-line order and value octets; it does not
  comma-fold, trim, or split those request values. The `<name>` bytes in the
  HMAC input below are always the normalized lowercase ASCII name, and a name
  is digested once even if it appeared repeatedly in `Vary`. Known-answer
  normalization fixture: snapshot/mutation lines `Vary: X-Tenant ,
  Accept-Encoding` and `Vary: accept-encoding` produce wire value
  `accept-encoding, x-tenant` and descriptor value
  `["accept-encoding", "x-tenant"]`, then digest each nominated request
  field's original instances in their received order. Fixtures also cover
  differently grouped lines, case variants, duplicate names, empty members,
  invalid tokens, `*`, absent versus present-empty request fields, and one value
  containing a comma.

  **Every** `Vary`-nominated request value is stored **only as a keyed
  digest** — every value, not a sensitivity classification an unknown
  credential field could slip past: HMAC-SHA-256 with domain tag
  `tsvry1|`, over an input that encodes **presence, instance count, and
  length-prefixed octets** — the earlier comma-join grammar had
  deterministic collisions (absent hashed identically to
  present-but-empty, violating RFC 9111 §4.1's absence-matches-only-
  absence, and two members `a`,`b` collided with one member `a,b`),
  which could select a representation built under a different
  credential or tenant. Absent field → `tsvry1|<name>|a`; present →
  `tsvry1|<name>|p|<count>` with `count` as ASCII decimal, then, per member in received order,
  `|<len>:<octets>` with `len` the ASCII-decimal byte count. Output
  lowercase hex (64 chars). **The key is a deployable contract, not an
  implementation detail**: the setting
  `[cache] vary_digest_key_secret_name` names a platform-secret-store
  entry containing one versioned keyring JSON object:
  `{ "schema_version": 1, "current_key_id": "<id>", "keys":
  [{ "id": "<id>", "key_base64url": "<unpadded>" }] }`. `keys` is
  sorted by `id`, contains 1..4 unique entries, rejects unknown fields,
  and every value decodes to exactly 32 CSPRNG bytes. An id is derived,
  not operator-invented: the first 16 lowercase hex characters of
  SHA-256 over the raw key bytes; a supplied mismatch or one id bound to
  different bytes is fatal. The current id must exist in the array.
  Every cache entry stores its id; raw keys never enter config, cache
  artifacts, logs, or metrics. Startup **fails** when response caching is
  enabled and the keyring/current key does not resolve (digests are never
  computed unkeyed).

  Lookup uses a stable per-representation **variant index** so the key ID is
  discoverable before the variant artifact is addressed. The base index key is
  the cache tuple excluding `Vary` values. Each bounded index descriptor stores
  only the normalized final `Vary` name list, key ID, corresponding keyed
  digests, artifact key, artifact expiry, and artifact revision tuple — never
  raw request values. A reader loads the index, groups descriptors by key ID,
  resolves each referenced key (one atomic keyring refresh if an ID is
  unknown), recomputes the digests from the exact final publisher request, and
  fetches only a descriptor whose complete name/digest tuple matches. A
  still-unknown ID, malformed descriptor, missing artifact, expiry, or revision
  mismatch is a miss for that descriptor, never a comparison under another
  key. Multiple matching descriptors are corruption and make the entire base
  lookup a miss with a metric; index order never chooses a winner.

  Publication writes and verifies the immutable artifact first, then CAS-adds
  or replaces its complete descriptor in the index. Rekeying or a changed
  `Vary` set inserts the new artifact/descriptor before removing the old
  descriptor; a crash may leave a safely unreachable artifact or two
  nonmatching descriptors but cannot point to a partial artifact. Index
  capacity eviction removes expired descriptors first and otherwise the
  least-recently-used complete descriptor; it never rewrites a digest under a
  new key ID. This is the one meaning of “insert-new-then-index-update rekey” in
  the capability matrix and 304 rules.

  Rotation atomically replaces the secret entry with a new valid keyring
  containing the new current key **and all still-live previous keys**. Fleet
  propagation may be mixed only in the safe direction: a process with the old
  keyring can write/read the old id; the variant index exposes that id before
  lookup, so a process seeing an unknown descriptor id refreshes the keyring
  once, then treats a still-unknown descriptor as a cache miss. It never
  guesses, probes with the current key, or computes unkeyed. A previous key may
  be retired only after no unexpired index descriptor references it **and** the
  maximum processed-artifact lifetime plus the adapter's qualified keyring
  refresh bound has elapsed since it stopped being current. Key IDs are never
  reused. Secret replacement atomicity, maximum propagation/refresh time, and
  unknown-id refresh are explicit adapter capability cells and startup gates;
  an unqualified adapter disables response caching rather than weakening the
  grammar. Cross-adapter fixtures cover old→mixed→new propagation, unknown-id
  refresh/miss, premature retirement rejection, and id/material mismatch.
  Known-answer vectors under the all-zero 32-byte test key:
  `tsvry1|authorization|p|1|10:Bearer abc` →
  `c880c5e8c36febc0b1581c92f1d598fded34391626e67372ed63b2857d8a7b6b`;
  absent-field form `tsvry1|x-tenant|a` →
  `a2ae26cf529a5843a25f1448acc4e90016d4c1dce0ffda5662e3ac459433e1ab`. And
  a response derived from a request carrying an **identity-bearing TS
  overlay** (the DataDome ClientID overlay) is forced `private, no-store`
  unless an explicit per-overlay contract says otherwise —
  the `Authorization` rule protects origin credentials, and this rule
  protects the identity TS itself injected. Parsing itself is a **shared core
  parser with fail-closed normalization**, not four adapter interpretations:
  a `Cache-Control` value that fails the shared grammar has an
  **enumerated result, not a "most restrictive reading"** (restrictions
  are independent axes, so no single most-restrictive point exists):
  the response is treated as **uncacheable for the storage decision**
  (`no-store`-equivalent in the invariant) and any mutation batch
  merging against the malformed value is rejected whole; among
  well-formed values, duplicate directives keep the strongest; quoted and unquoted
  forms are equivalent; conflicting `max-age` values keep the smallest;
  unknown extension directives are dropped **from mutations only —
  unknown directives already in the snapshot are preserved verbatim** (a
  downstream cache may honor a restrictive extension TS does not
  recognize; dropping it would weaken origin policy, RFC 9111 §5.2.3);
  `Expires` participates in the freshness bound via **RFC 9111 §4.2.1's
  freshness-lifetime algorithm, referenced directly**: the
  `Expires`-derived lifetime is `Expires − Date` (absent `Date` →
  response receipt time), invalid or duplicate date values are treated
  as already expired (the RFC's conservative option, chosen
  normatively), and `Age` is handled per the RFC — effective freshness
  is the minimum across `max-age`, `s-maxage`, and that derived
  lifetime and a mutation may **not introduce `max-age`/`s-maxage`
  where the snapshot supplied no upper bound** — HTTP prefers `max-age`
  over `Expires` (RFC 9111 §5.3), so introducing one would override an
  origin's shorter or already-expired `Expires`; and `Vary: *` is
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
  the done-when. The registration's `behavior_revision` follows §2's bump
  contract; configuration-dependent behavior is captured separately by the
  effective-config digest. Model-only activation is separate from both, so the
  **one cache revision tuple** contains exactly
  `integration_registry_revision`, `effective_config_revision`,
  `active_policy_digest`, `active_policy_ordinal`, `model_epoch`,
  `activation_generation`, and `hook_invariant_revision` from the strong active
  tuple at publication. Every processed artifact, mutation IR/read-set bundle,
  variant descriptor, and variant-index update stores that complete tuple; a
  lookup, local conditional, HEAD update, or 304 replay requires byte-for-byte
  equality with current active. In particular, the `permissions_v2` model CAS
  misses every `pre_epic_v1` artifact even though config/policy bytes did not
  change. Core also carries a nonzero
  `HOOK_INVARIANT_REVISION: u32`, bumped for every parser, merge, budget,
  cache-artifact, or invariant semantic change. The cache tuple stores all
  fields above, so a deploy cannot silently reuse old finals while a new
  restriction waits for cache expiry. Until then the
  operation set is headers-only, and `Set-Cookie` is fully reserved.
  Violations are rejected at the operation layer (§2) and
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
  above, and growing the admitted set is a spec change to this list (cookies: see the §3 deferral above). Replacing a
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
  ceiling — and those ceilings are the enumerated capability cells **below**,
  with the counting rule fixed exactly: counted bytes = Σ over emitted
  fields of `len(name) + 2 + len(value) + 2` (the `": "` and CRLF
  separators), validated against core's budget at startup so a batch
  that passes core can never fail only on one adapter; a snapshot
  already **over** the core budget before any mutation rejects every
  ordinary batch — the budget bounds additions and never bricks an
  over-budget origin response, which passes through and is counted;
  security follows the replacement/reserve rule below)
  bounds the sum across integrations. The budget has normative priority
  partitions: ordinary mutators may consume at most **112 headers / 24 KiB**,
  reserving 16 headers / 8 KiB for the core-owned security channel. Ordinary
  batches remain registration-ordered within their partition. A security
  `Continue` batch uses the reserve and, if necessary, evicts whole accepted
  ordinary batches in reverse registration order until it fits; it never
  removes half a batch and never drops origin fields. Ordinary output can
  therefore never crowd out security effects. If the immutable origin head
  plus the security batch alone exceeds the full budget, the security batch
  is rejected atomically and the request follows the documented security
  fail-open path with a dedicated metric — origin fields are not silently
  sacrificed. A security `Respond` owns a replacement
  response: all ordinary mutation batches are discarded and the challenge
  is validated against the full 128-header / 32-KiB budget. A base publisher
  response already over the full budget still passes unchanged, but no
  ordinary mutation applies; security `Continue` applies only if the final
  response fits after all ordinary batches are removed. These are separate
  outcomes and metrics.

  | Adapter    | Header-count / total-bytes ceiling (capability cell)                                                                                                   |
  | ---------- | ------------------------------------------------------------------------------------------------------------------------------------------------------ |
  | Axum       | no platform ceiling below the core budget (native HTTP stack) — cell fixed at ≥ 128 headers / ≥ 32 KiB                                                 |
  | Fastly     | **qualification-pending**: the measured platform ceiling is recorded in this cell by the adapter-qualification commit; unrecorded ⇒ hook startup fails |
  | Cloudflare | **qualification-pending**: same rule                                                                                                                   |
  | Spin       | **qualification-pending**: same rule                                                                                                                   |

  A recorded cell below core's 128-header / 32 KiB budget is a startup
  error (shrink the core budget or raise the ceiling — never a silent
  per-adapter divergence).

  Hook/cache eligibility has the following concrete adapter cells; any
  `qualification-pending` cell fails startup when the depending feature is
  selected:

  | Capability                                                                                         | Fastly                                                                                  | Axum (dev)                                          | Cloudflare                     | Spin                             |
  | -------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------- | --------------------------------------------------- | ------------------------------ | -------------------------------- |
  | Runtime secret lookup for Vary-HMAC/DataDome                                                       | wired secret store; qualify key-rotation behavior                                       | dev secret binding required                         | qualify Workers secret binding | qualify component secret binding |
  | Persisted processed artifact + mutation IR/read sets                                               | qualification-pending                                                                   | in-process dev implementation required; non-durable | qualification-pending          | qualification-pending            |
  | Atomic artifact/metadata entry commit                                                              | qualification-pending                                                                   | implementation required                             | qualification-pending          | qualification-pending            |
  | `Vary` variant index + insert-new-then-index-update rekey                                          | qualification-pending                                                                   | implementation required                             | qualification-pending          | qualification-pending            |
  | DataDome field-line order, trusted IP/port, fixed HTTPS backend/no-redirect, and exact form limits | qualification-pending                                                                   | qualification-pending                               | qualification-pending          | qualification-pending            |
  | SecurityUse JA4 request evidence                                                                   | platform value available; exact-field/payload qualification and sign-offs 23/28 pending | unavailable                                         | unavailable                    | unavailable                      |

  The qualification commit records storage lifetime, maximum object size,
  concurrency semantics, torn-write behavior, and fault-injection evidence;
  “platform has KV” is not a qualifying cell.
  `expose_host_fingerprints_to_vendor = true` also requires a qualified
  SecurityUse JA4 cell; unsupported or pending is a startup error, while the
  default `false` remains portable.

  Each mutator receives an **immutable, redacted snapshot of the
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
  Each registration also declares the complete set of response fields its
  decision may read, including status as a distinguished input. Core records
  the union with the accepted operation batch. Undeclared reads are a hard
  conformance failure in tests; an integration unable to declare a complete
  read set marks itself `revalidation = "refetch"`, which forbids IR replay
  after any origin metadata change.
  Operations arrive as **attributed batches bound to a registration
  ID** — one batch per integration per response, ordered by
  registration, with the security channel's batch (§4a) ordered **after**
  ordinary response mutators — one global order, core finalization →
  ordinary mutators → security effects → invariant pass — so the
  security layer's precedence over publisher-facing mutations holds through
  both position and its reserved/response-owning budget rule; the current flat effects vector satisfies neither
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

In this table, **persisted post-hook finals** means the cache-safe ordinary
artifact only: origin metadata plus accepted ordinary mutation IR and the
cache/privacy invariant result, with `Set-Cookie`, core request-specific
identity fields, security-channel effects, and origin validators excluded.
The security request filter evaluates every request before cache selection; a
fresh `Respond` bypasses the artifact, while a fresh `Continue` batch is
applied to the persisted ordinary artifact and the invariant pass reruns before
emission. This applies equally to ordinary hits, local conditionals,
origin-revalidation 304s, and HEAD. Therefore "`Set-Cookie` is never replayed"
means never replayed from storage; a freshly validated per-request typed cookie
operation may still emit on that response. Security `Respond` outputs are
always `private, no-store` and never become artifacts.

An **unconditional recovery fetch** first saves the client's preconditions for
later local evaluation, then removes every upstream conditional/range field
(`If-Match`, `If-None-Match`, `If-Modified-Since`, `If-Unmodified-Since`,
`If-Range`, and `Range`) so origin must return full bytes. TS processes that
200 under current revisions, constructs the processed validator, and only then
evaluates the saved client preconditions as the authoritative server for the
transformed representation. Another bodyless origin 304 can never satisfy
recovery.

The 304 **safe-update set** is exactly `Cache-Control`, `Expires`, `Date`,
`Age`, `Vary`, `Surrogate-Control`, `CDN-Cache-Control`,
`Cloudflare-CDN-Cache-Control`, and `Edge-Control`. The phrase
"registry-admitted mutable fields" in the matrix denotes an empty set in v1;
adding any name requires a reviewed spec/registry revision and conformance
fixture, never a runtime wildcard.

| Response                                          | Hook runs?                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                     |
| ------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Processed HTML document (rewritten by TS)         | Yes                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            |
| Streamed processed document                       | Yes — operations apply to the header block before first byte                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                   |
| Pass-through proxy response (not processed)       | No — TS is a transparent proxy for it                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                          |
| Served-from-cache processed document              | Serves the **persisted post-hook finals** captured at cache fill (revision-versioned; mismatch = miss) — mutators run at fill/processing time only, so a normal hit and a conditional hit return identical policy metadata **by construction**, with no purity or determinism assumption about mutators                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                        |
| Redirect (3xx)                                    | No                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| Error responses TS itself generates (4xx/5xx)     | No                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| `304 Not Modified` for a processed representation | **Persisted-metadata pass**: a cached processed 200 stores its final post-hook headers, accepted mutation-operation batches, and the union of every mutator's declared response-field read set (the persisted mutation IR), versioned by §3's complete cache revision tuple, including model epoch and logical activation generation. A local conditional hit re-emits persisted finals only when the complete tuple matches current active. An origin-revalidation 304 is staged and diffed against separately stored origin-side metadata. (a) Any byte-coupled field changed (`Content-Encoding`, `Content-Type`, validators, digests) → invalidate and fetch/process a full 200. (b) A changed metadata field that intersects any persisted mutator read set, or an artifact/mutator lacking a complete read-set declaration, is also unsafe → full 200 refetch and ordinary hook execution; deterministic replay of old operations cannot stand in for re-evaluating a decision made from changed inputs. (c) If every changed field is outside every declared read set and belongs to the enumerated safe-update set (`Cache-Control`, reserved CDN cache fields, `Expires`, `Date`, `Age`, `Vary`, registry-admitted mutable fields), replay the persisted deterministic operations over updated origin metadata and rerun invariants. Updated origin metadata, finals, IR, read sets, and complete revision tuple publish in one atomic entry commit; changed `Vary` uses insert-new-entry-then-index-update ordering. (d) No change → re-emit persisted finals. Artifact absence or any tuple mismatch triggers an unconditional recovery fetch so TS obtains bytes. For processed-document GET/HEAD routes, TS is explicitly the authoritative server for the transformed representation: after processing the full 200 it evaluates RFC 9110 §13 preconditions against **processed** validators; this is not evaluation of origin validators by an intermediary cache. Other methods are never eligible for this recovery path. `Set-Cookie` and origin validators are never replayed. Absent metadata → cache miss |
| `HEAD` of a processed document                    | **Serves the persisted GET artifact when one exists** — parity with GET/304 by construction, since RFC 9111 §4.3.5 lets HEAD metadata update a stored GET response and mutators are not required to be deterministic; with no persisted artifact, HEAD is processed like a GET (headers only) and its finals stored as a **distinct head-only artifact type that never satisfies a later GET** — when a stored GET artifact exists a HEAD may **update** it only when the comparison — made against the separately stored **origin-side** metadata, never the processed artifact's (origin and rewritten lengths differ by construction, so the processed length is never the comparand and HEAD metadata never touches processed-side headers) — finds validators and origin `Content-Length` matching and no byte-coupled representation field changed (RFC 9111 §4.3.5); qualifying updates land origin-side under the same atomic-commit discipline as the 304 rules, and any mismatch **invalidates** the stored GET artifact rather than updating it                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                     |
| Informational `1xx`, `204`, `205`, `206`          | No — enumerated so adapters do not infer independently                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                         |

This deliberately narrows #782's general "outbound response" phrasing to
processed documents (§6).

## 4a. The security channel — normative closed boundary

The security channel (today: DataDome) runs under a distinct, typed
`SecurityUse` authority rather than the advertising permissions P1/P4.
`SecurityUse` permits bot/fraud evaluation and challenge continuity only;
it never authorizes TS-controlled advertising identity, graph linkage, partner egress,
other integrations, or general raw-value observability. Request-scoped raw
security evidence may be disclosed only to the fixed DataDome Protection API
endpoint and only from that integration's explicit field allowlist; it is not
persisted in the identity graph, exposed to publisher origin or other
integrations, or emitted in logs. It carries its own configured retention and
deletion path. An advertising opt-out does not erase a
strictly security-scoped identifier, while an authenticated deletion request
or expiry under the security retention policy does. This is not a general
exception. The path-only `Request` remains publisher-originated data and is
explicitly covered by vendor retention/DSR sign-off; query strings and full
referrers are never in the security view. Every degree of freedom is closed:

- **Host evidence is not a back door to the deferred device provider.**
  `[device] provider = "fastly"` remains startup-rejected and no JA4-derived
  classification is stored. If DataDome's Protection API is allowed to receive
  request-scoped `TlsProtocol`/`JA4` evidence, its registration enumerates each field,
  proves vendor necessity and payload bounds, and keeps it ephemeral under
  `SecurityUse`; those exact consumers and fields are part of product/vendor
  sign-offs 23/28. `TlsCipher` is omitted because the platform value has
  different semantics from the vendor field, `H2Fingerprint` is not in the
  vendor contract, and all other host evidence is omitted.
- **Deletion and retention name the system boundary honestly.** TS stores no
  server-side DataDome identifier mapping. On an authenticated TS deletion
  request it excludes the route from vendor validation, emits a typed
  `datadome` cookie deletion, and sends no ClientID to DataDome for that
  request; re-presentation retries deletion. A lost browser response can leave
  the cookie until its configured `security_cookie_max_age`, which is the
  bounded residual sign-off 23 accepts. This operation does **not** claim to
  erase data already held by DataDome: vendor-side retention and data-subject
  deletion require a named contractual/API procedure in the decision record.
  If no such vendor procedure exists, operator documentation says so and may
  not describe TS cookie deletion as vendor-data deletion.

- **Typed security-cookie operation with a concrete lifecycle, not
  header strings.** The channel emits cookies only through a typed
  operation, and the registration is not a placeholder — for DataDome
  it pins, **aligned to documented vendor behavior where hardening was
  not intended**: cookie name exactly `datadome`; one configured ownership
  tuple for every set and deletion: path exactly `/` and
  `security_cookie_domain = "host-only"` (default) or one explicit normalized
  ASCII domain. Host-only mode requires the vendor `Domain` attribute to be
  absent; explicit-domain mode requires it to equal the configured domain
  exactly — TS never accepts a different domain and never rewrites one scope
  into another. The explicit value cannot exceed the registrable domain,
  computed against the **vendored
  Mozilla PSL snapshot** `docs/superpowers/specs/psl-snapshot-ref.md` —
  ICANN + private sections, IDNA-mapped; IP-literal or single-label
  hosts fall back to host-only), path `/`; `Secure` mandatory;
  `SameSite` configurable `Lax` (default) / `Strict` / `None`
  (`None` requires `Secure`), matching the vendor's endpoint options;
  the configured/returned `Domain` must additionally **domain-match the current
  request host** per RFC 10025, the current cookie specification obsoleting RFC 6265 (domain-match and the PSL boundary check
  are separate requirements) and a vendor cookie using `Expires` is
  normalized to its Max-Age equivalent (both present → `Max-Age` wins,
  per RFC 10025); a normalized lifetime exceeding the ceiling rejects
  the whole operation batch; and the parser is total: repeated `Cookie`
  request fields are joined with `"; "` (semicolon-space — the
  order-preserving join of RFC 10025) before parsing, and
  **duplicate `datadome` pairs after the join make the request-side
  identity ambiguous: treated as cookie-absent for the vendor call and
  counted**, while cookies under other names pass through untouched;
  `Set-Cookie` fields are **never combined**, and a vendor response
  carrying more than one `datadome` `Set-Cookie` field, a `Set-Cookie`
  for any other name (the typed operation pins the name exactly), an
  **`HttpOnly` attribute** (the vendor requires script readability —
  its presence is a protocol anomaly), duplicate attributes, duplicate
  cookies in one field, an unparseable `Expires`, or unknown attributes
  each reject the operation batch (the vendor cookie is well-formed;
  strictness is safe). `Expires` normalizes as
  `Max-Age = max(0, floor(expires − now))` whole seconds on the server
  wall clock at parse time (the shared skew-bounded basis; a result of
  0 is a deletion), and the 512-byte limit measures the **normalized**
  serialized `name=value` plus attributes in bytes;
  `Max-Age` is capped by required operator configuration
  `security_cookie_max_age` in the vendor-supported range 7 days through
  **31,536,000 seconds** (one year); the returned cookie may be shorter but
  never longer. There is no silent one-year default. Size ≤ **512 bytes**
  (DataDome's current Fastly-module limit; 4 KiB was ours, not theirs).
  Where the contract **is** deliberately narrower than the vendor — the
  spec-pinned pointer allowlist starting at ClientID-only against
  DataDome's mandatory response-directed mapping set — that reduction
  needs explicit product **and vendor** acceptance: sign-off item 28;
  a violating operation is rejected whole (the batch rule). **Both
  sessionByHeader is startup-rejected in v1 — one state, not three**:
  TS never sends `X-DataDome-X-Set-Cookie: true`; a vendor
  `X-Set-Cookie` **invalidates the batch → Continue** (its matrix cell —
  a session mode the fleet never requested must not half-apply); and an incoming
  browser `X-DataDome-ClientID` is **not forwarded to the vendor**
  (cookie-only session identity — an earlier revision translated
  `X-Set-Cookie` into an ordinary cookie, which is not equivalent:
  header-session clients expect JavaScript to receive `X-Set-Cookie`
  and `X-DD-B`, and the higher-priority header session would never see
  a cookie update; the older DataDome spec's "always send
  X-DataDome-X-Set-Cookie when the header ID is used" is superseded for
  v1 by its banner). Supporting header mode later means the full vendor
  protocol — typed owner-scoped `X-Set-Cookie`/`X-DD-B` forwarding,
  CORS exposure, and a JavaScript/local-storage identifier observer —
  as an explicit opt-in under **sign-off 23**. Every `ts-*`
  name is rejected. **Read is owner-only across every _server-side_ surface — and the
  browser side is explicitly not ownable**: DataDome requires the
  cookie to be readable by its JavaScript and warns against `HttpOnly`,
  so **every same-origin page script can observe it**, and a Respond
  serves vendor-owned HTML under the publisher origin (vendor scripts
  with same-origin access to cookies, storage, and APIs; publisher CSP
  can conversely break the challenge). Those browser-side observers,
  same-origin vendor code, CSP interaction, and challenge redirects
  enter **sign-offs 23/28** for ratification (both decision records
  still open), not implied. The server-side strip
  inventory is exhaustive, not integration-scoped — the browser sends `datadome` in the ordinary
  `Cookie` header, so it is removed from **every non-DataDome surface**:
  other integrations' request views, publisher-origin proxy forwarding,
  proxy/click/Testlight upstreams, auction/page-bids request
  serialization, and logs (redaction list) — each surface a tested row
  of the inventory; only the security channel itself observes it; vendor
  egress goes only to the fixed DataDome Protection API authority and path;
  redirects are not followed; deletion is always possible
  through the `SecurityUse` lifecycle; and advertising withdrawal never
  grants access to or reuses the identifier. No other request filter
  inherits the cookie capability.
- **Cookie ownership makes deletion total for the scope TS creates.** While
  DataDome is enabled, `datadome` is a security-owned name across the final
  response: before the security batch applies, core removes and meters every
  origin, core, cached, or ordinary-mutator `Set-Cookie` for that name;
  unrelated cookie names remain separate field lines. Only the typed security
  operation may emit it. Authenticated deletion emits the same configured
  `(name, domain mode/domain, path)` tuple with `Max-Age=0`; it does not guess a
  scope from the request cookie, whose wire form carries no Domain or Path.
  Candidate validation rejects a change of domain mode/domain while the
  previous active DataDome configuration can still have a live cookie. The
  supported migration is disable + wait at least the previous
  `security_cookie_max_age` + activate the new scope; a faster scope change
  requires a separate bounded deletion-fan-out design. The permission spec
  §5.5 whole-settings serve fence applies before cookie processing. A bounded
  old-generation admission validation may survive only during the pre-promotion
  drain; the register's promotion-not-before plus member quiescence proves it
  and every admitted effect ended before the activation CAS. After that CAS,
  no instance may emit, refresh, or delete a `datadome` cookie until it has
  loaded and leased the exact new active tuple. A stale instance stops at serve
  admission rather than extending the old scope. Fixtures cover origin
  collision, host-only vs explicit-domain set/delete, attempted domain change,
  and duplicate request cookies. The deletion claim therefore covers every
  cookie this contract can create, not arbitrary pre-contract scopes.
- **The incoming `X-DataDome-ClientID` request header is owner-only,
  like the cookie.** DataDome prioritizes the header over the cookie,
  so leaving it in the shared request would hand other integrations and
  upstream routing the same identifier the cookie boundary strips: core
  **removes it from the shared request** before integrations and
  upstream routing run — it joins `RedactedRequestView`'s enumerated
  strip set (providers spec) — **and in v1 it is stripped for the
  vendor too: the Protection API request's ClientID derives only from
  the `datadome` cookie, never from the incoming header** (DataDome's
  contract requires `X-DataDome-X-Set-Cookie: true` whenever a
  header-supplied ClientID is forwarded, so forwarding the header under
  cookie-only mode would misdeclare the session mode); observed header
  occurrences are counted, and a **fixture pins the path**: a request
  carrying both cookie and header produces a vendor payload whose
  ClientID equals the cookie value, with no header-derived identity
  sent. Only DataDome-returned overlay data reaches the publisher,
  never the raw browser-supplied header.
- **The pointer protocol has a total parser contract** — adapters
  cannot differ where malformed batches fail open: the pointer list is
  tokenized by the vendor's documented space separation — repeated
  pointer header fields are concatenated with a single SP before
  tokenizing, tokens split on runs of SP/HTAB, empty tokens ignored —
  then names are ASCII-lowercased before duplicate detection; duplicate
  names after normalization, invalid names, more than 16 pointers, or
  more than 4 KiB of pointer payload render the batch invalid
  (→ Continue, the vendor's fail-open). **Pointed-field multiplicity is
  closed**: for singleton fields (`Location`, `Content-Type`,
  `X-DataDome`, `X-DD-B`, `X-Set-Cookie`) more than one instance in the
  vendor response invalidates the batch atomically — never a
  first/last/join choice an adapter makes; list-valued fields
  (`Cache-Control`, `Pragma`) are joined per RFC 9110 §5.3 before their
  matrix outcome applies; `Set-Cookie` multiplicity follows the typed
  cookie rule (exactly one `datadome` field, above). No both-source
  priority rule exists in v1: the header session form (`X-Set-Cookie`)
  is matrix-governed as batch-invalid, so "header form wins" is
  unreachable and deleted.
- **Request-header pointers are a positive, enumerated allowlist with no
  default publisher-origin identifier exposure.**
  "Documented enrichment headers" is not enforceable; the registration
  enumerates the exact names from the **checked-in allowlist file
  `docs/superpowers/specs/datadome-header-allowlist.md`** — spec-pinned
  today to exactly **`X-DataDome-ClientID`**, admitted only when the
  operator explicitly sets
  `[integrations.datadome] expose_client_id_to_origin = true` (default
  `false`); every other `X-DataDome-*`
  field is rejected until a reviewed commit adds it to that file
  ("documented enrichment set, listed one by one" without an actual list
  was a wildcard whose contents could change outside the spec) —
  resolving what was a contradiction. When the opt-in is false, the
  vendor-returned ClientID is discarded and the publisher origin is not
  an identifier observer. When true, it applies only to an owner-scoped
  publisher-upstream overlay, never the shared request; startup logs the
  additional consumer, operator documentation must disclose its purpose
  and retention, and a fixture proves no other surface can read it.
  Everything else — authentication,
  `Cookie`, `Forwarded`/`X-Forwarded-*`, other identity, consent, and
  routing-authority fields — is rejected by name and by class: a
  compromised endpoint must not replace origin credentials, inject
  `ts-ec`, or spoof client location.
- **Browser-response headers are decision-scoped through the single
  matrix, and only there.** This spec carries **no per-decision field
  list**: every pointer's outcome per decision and session mode —
  `Location`'s Respond-only 3xx admission, `Pragma`'s
  drop-individually middle path (response `Pragma: no-cache` has no
  standardized meaning, RFC 9111 §5.4), and the batch-invalidation
  default for unlisted names — is exactly one cell of the matrix in
  `datadome-header-allowlist.md`. The fail-open consequence of batch
  invalidation stays within sign-off 28's scope, and both documented
  vendor responses are verbatim fixtures at the matrix.
- **Representation rules are decision-scoped and narrow.** A _Respond_
  decision (challenge/deny) owns its body but may describe it with
  **`Content-Type` only** — encoding and validator fields
  (`Content-Encoding`, `ETag`, `Last-Modified`, digests) stay reserved
  even for Respond: challenge bodies are simple and uncacheable, the
  allowlist does not admit those fields, and ambiguity here decides
  whether a challenge enforces or silently fails open (batch rejection →
  Continue). If the vendor ever requires more, it arrives as a reviewed
  allowlist-file addition. A _Continue_ decision may not touch
  representation metadata of publisher bytes.
- **Respond transport is bounded, with exact measurement points.** The
  challenge body has a maximum size (64 KiB) and a **complete-response
  deadline of 3000 ms on the instance's monotonic clock, measured from
  immediately before vendor-backend acquisition/dispatch to the final
  body byte** — connection setup and request send are inside the
  window; the 1500 ms first-byte bound (the older spec's figure, now
  first-byte only) runs from the same origin on the same clock. At
  expiry the decision is final (batch fails → Continue) and the vendor
  request is cancelled; cancellation and resource cleanup complete
  asynchronously and never delay the response. TS sends
  `Accept-Encoding: identity`, and because that does not _guarantee_
  identity coding, a response arriving with any `Content-Encoding` is
  itself batch-invalid; `Content-Length` is recomputed from the actual
  bytes before Respond commits. **On a HEAD request, Respond validates
  the challenge body exactly as for GET (size, deadline, encoding) but
  emits no body**: the outward response **omits `Content-Length`** and
  carries no content — RFC 9110 §9.3.2 permits `Content-Length` on
  HEAD only when it equals the equivalent GET body's length, and the
  vendor does not guarantee method-invariant challenge bodies, so the
  validated HEAD bytes cannot establish the GET length (a
  vendor-guaranteed equivalence, if ever ratified, may restore the
  field as a reviewed change; the older DataDome spec's HEAD handling
  remains superseded). Exceeding
  size, first-byte, or total deadline fails the batch → Continue.
- **One pointer contract, one place.** The single normative
  decision × session-mode × pointer matrix lives in
  **`datadome-header-allowlist.md`** — this spec's earlier inline
  decision-scoped list and outcome list are deleted in its favor
  (duplicated lists disagreed about `X-Set-Cookie`, `X-DataDome`,
  `X-DD-*`, and `Pragma`, letting one conforming implementation accept
  the vendor's documented `Set-Cookie X-DD-B` allow-example while
  another invalidated the whole batch). No `X-DD-*` wildcard exists:
  every name is enumerated, `X-DD-B` included and forwarded exactly once
  as the vendor's documented cookie-mode browser-response signal; it is
  never copied into publisher-upstream or another integration. The
  documented vendor responses (both the challenge example and the
  `Set-Cookie X-DD-B` allow example) are **verbatim fixtures asserting
  the decision survives** and exactly the mapped fields emit.
- **Every security Respond ends uncacheable, unconditionally.** After
  the decision's fields are applied, the invariant pass forces
  `Cache-Control: private, no-store` and strips all CDN cache fields on
  **every** Respond regardless of status, vendor pointers, or cookie
  emission — a cookie-less `301` challenge with no effective vendor
  cache header could otherwise be heuristically stored and served to
  unrelated clients.
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
2. **The old generic `RequestFilterEffects.response_headers` channel is
   removed.** DataDome uses the separate sealed, core-registered
   `DataDomeSecurityRequestFilter` and typed `DataDomeSecurityEffects` defined
   by its design; §4a defines that closed boundary. Generic request filters
   receive only `RedactedRequestView` and ordinary attributed effects and
   cannot express the security view, owner overlay, cookie operation, or
   reserved security header. The dedicated channel is necessary because
   DataDome sets headers **and cookies** on 200, 301/302, 401, 403, and 429
   responses — response classes (§3a) the ordinary response hook never runs
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
   dropping core-required values nor bypassing the snapshot; the §3
   normalization fixture proves field-line grouping/case/order collapse to one
   sorted lowercase descriptor, repeated names digest once, request value
   instances retain octet/order boundaries, malformed/empty mutation members
   reject, a malformed snapshot becomes `Vary: *` plus `no-store`, and `*`
   writes no artifact; a `pre_epic_v1` artifact/index descriptor is a miss
   immediately after the model-only `permissions_v2` activation CAS even when
   config and policy digests are unchanged; each of the four enumerated CDN fields (`Surrogate-Control`,
   `CDN-Cache-Control`, `Cloudflare-CDN-Cache-Control`, `Edge-Control`)
   individually stripped; and a rejected `Content-Encoding` mutation.

## 5. Size and sequencing

This is a modest feature plus tests with zero coupling to the provider
architecture or, in its v1 headers-only form (§3), to the permission
model. It lands whenever its first real consumer is identified (§4, item 3);
cookie operations arrive only with their own follow-up spec (§3) and its
permission-model coupling. If no consumer
materializes, it does not land; being unblocked is not a reason to ship
scaffolding.

## 6. Divergences from issue #782

This spec supersedes #782 on the following points; the issue is updated to
reference this spec when the PR merges:

| #782 says                                          | This spec says                                                                                                       | Why                                                                                                                  |
| -------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------- |
| Mutations apply to the outbound response generally | Eligibility is the explicit §3a matrix, centered on processed documents                                              | Pass-through/error/304 mutation has different semantics per adapter; enumerating beats implying                      |
| Ship the trait + registry + adapter application    | Additionally: structured operations API (§2), reserved surface (§3), and a real consumer in the same PR (§4, item 3) | PR #838 shipped the trait with zero call sites; an unrestricted `&mut HeaderMap` cannot enforce any collision policy |
