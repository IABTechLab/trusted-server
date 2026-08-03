# Design Spec: Client-Cycle Edge Cookie Providers and the Resolve Endpoint

**Status:** **Deferred — informative draft, not part of the epic's
normative set.** No production adapter has the required CAS-class
primitive (providers spec §7 matrix), the feature has no concrete
consumer, and successive reviews keep finding open protocol questions
(§7). It re-enters the epic only through its own dedicated issue, with
this document as the starting bar — findings against this spec do not
block ratification of the core specs.
**Author:** Engineering
**Issue references:** none yet (this spec exists to force one; #778 does not
cover this feature)
**Related specs:** `2026-07-30-pluggable-providers-design.md`
**Last updated:** 2026-07-31

> **Context.** PR #838 shipped, undeclared and unspec'd, a second provider
> _type_: a "client-cycle" EC provider whose identifier is established by a
> browser POST to a new public endpoint (`POST /_ts/api/v1/ec/resolve`),
> plus a demo provider (`client-fixed`) and a JS bundle. Review found the
> endpoint accepted cross-origin identity-setting posts with no origin
> check, minted cookies with no identity-graph row (violating an invariant
> the organic path enforces explicitly), was registered on only one of four
> adapters, and could never round-trip because the core did not recognize
> non-HMAC identifiers. None of that is an argument the feature is a bad
> idea — vendor identity systems with a browser leg (e.g. signed-envelope
> schemes) are a real integration target. It is an argument that the feature
> needs a threat model before an implementation. This spec is that threat
> model and the bar an implementation must clear.

---

## 1. Overview

A **client-cycle** EC provider establishes the identifier via a browser
round trip: server-injected first-party JS obtains or derives a value in the
page (typically a signed envelope from a vendor identity system), posts it to
a Trusted Server endpoint, and the endpoint — after provider-specific
verification — sets the first-party `ts-ec` cookie.

This differs from server-side providers in one security-critical way: **the
identifier is attacker-influenceable input**, not server-derived evidence.
Everything in this spec follows from that.

## 2. Threat model

| Threat                           | Vector                                                                                                                                                             | Consequence if unmitigated                                                                                                                                                                                                                                       |
| -------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Cross-site identity fixation** | `text/plain` POST is a CORS-simple request: any page on the web can `fetch(resolveUrl, {method: "POST", credentials: "include", body: payload})` with no preflight | An attacker pins a chosen identity onto a victim's first-party cookie jar — login-CSRF for the ad-identity layer; the victim's activity accretes to an attacker-controlled ID                                                                                    |
| **Replay**                       | A captured valid payload (from the attacker's own session or a leak) replayed against another browser                                                              | Same as fixation, without needing to mint payloads                                                                                                                                                                                                               |
| **Phantom identity**             | Endpoint sets the cookie without an identity-graph row                                                                                                             | Later requests carry an EC that the KV graph has never seen; downstream sync and withdrawal logic operate on an identity that half-exists (the organic generation path explicitly refuses to write a cookie when the graph write fails, for exactly this reason) |
| **Un-tombstoneable identity**    | Core does not recognize the provider's identifier shape                                                                                                            | Withdrawal cannot expire or tombstone the identity — a compliance failure, not just a bug                                                                                                                                                                        |
| **Amplification**                | The page script cannot observe an HttpOnly cookie, so it cannot know the cookie is already set                                                                     | A POST on every page view of every session (PR #838's JS gated on reading a cookie its own server marked HttpOnly, making the guard permanently false)                                                                                                           |

## 3. Requirements on the endpoint

`POST /_ts/api/v1/ec/resolve` (final path TBD) MUST:

1. **Reject cross-site requests with an exact origin check.** The request
   is authorized only by **exact membership of the `Origin` header value in
   the publisher's origin allowlist** — configuration that does not exist
   yet and must be defined by this feature (§7, question 5) — or by a
   **session-bound CSRF token**. `Sec-Fetch-Site` is **defense-in-depth
   only, never an authorizing alternative**: it carries no origin value to
   compare against an allowlist, and `same-site` admits every sibling
   subdomain — one compromised or attacker-registered subdomain would be
   enough to set identity. Requests with no `Origin` and no valid token are
   rejected.
2. **Verify the payload cryptographically per provider — including against
   replay.** `resolve_from_client` belongs to the client-resolve acquisition
   surface that was **removed from the normative provider contract with
   this feature's deferral** (providers spec §4) — it, the acquisition
   enum, and the reservation schemas live only in this informative
   draft, to be renormalized (against the current trait and the
   delimiter-free key grammar, which superseded the `resv/…` sketch
   here) when the feature gets its issue. It
   accepts only payloads
   that are signed by an expected party, **audience-bound** to this
   publisher, and **expiring**. Audience binding and expiry alone do not
   mitigate replay — a captured token installs in another browser for the
   whole validity window. **Session binding is required, with no ownerless
   escape hatch** (a server-issued nonce the payload must embed):
   one-time consumption alone limits multiplicity but proves nothing
   about _which_ browser redeems first — a captured bearer payload can
   simply win the race — so it is defense-in-depth, not the mitigation.
   An earlier draft allowed a "first-presenter, product-accepted"
   ownerless mode; it is **removed**: risk acceptance does not make a
   security invariant true, and the promised orphan cleanup was
   unimplementable anyway — the server cannot observe `Set-Cookie`
   acceptance, so it cannot distinguish a lost-response orphan from a
   successful-but-dormant identity. A scheme that cannot embed the
   session nonce cannot ship. "Single-use where the scheme allows" is
   not a mitigation.
3. **Preserve the identity-graph invariant.** The cookie is set only after
   the corresponding graph row is written, mirroring the organic path. Graph
   unavailable → no cookie, same as organic generation.
4. **Round-trip through the lifecycle contract.** The identifier set here
   must be parseable, graph-keyed, and tombstonable by the selected provider
   (providers spec §3). The conformance suite runs against every
   client-cycle provider.
5. **Exist on every adapter — where parity means identical behavior,
   including identical refusal.** Route registration goes through shared
   route wiring. On adapters whose capability matrix rows are green
   (today **no adapter** has the required CAS class — the normative
   matrix marks even Axum's storage unavailable; providers spec §7), the parity suite asserts identical endpoint behavior; on
   adapters without them, parity means **identical startup rejection of
   the client-cycle selection** — not a proxied 404 (PR #838's failure
   mode: Fastly-only registration let the Axum dev server proxy the POST
   to the publisher origin), and not a silently absent route.
6. **Be uncacheable and permission-gated on the provider's full
   declaration.** `Cache-Control: no-store`; before any cookie is set,
   the endpoint enforces the selected provider's complete
   `required_permissions()` — not a hard-coded `store-on-device` check; a
   client-resolve provider declaring more than P1 gets all of it
   enforced, exactly as organic minting does (providers spec §5).
7. **Bound every input — exact values, testable boundaries.** Request
   body: at most **65,536 bytes** (inclusive; byte 65,537 → `413`),
   enforced by a bounded read independent of `Content-Length` — a
   missing, false, or chunked length must not bypass it. `Content-Type`
   allowlist: `text/plain` and `application/json`, matched on the media
   type alone — case-insensitively, ignoring parameters, so the browser's
   default `text/plain;charset=UTF-8` passes; duplicate `Content-Type`
   headers → `400`; anything else → `415`.
   The resulting identifier: at most **256 bytes**, cookie-safe alphabet
   (providers spec §3 global bounds); violation → `400`. Reservation key
   (payload unique id): at most **128 bytes**. Status codes are part of
   the contract: `400` malformed payload/identifier, `403` origin/token
   rejection, `409` different-identity or revoked-family conflict (§3.8),
   `413` body, `415` content type. Tests exercise each boundary at its
   exact edge, including the missing/false/chunked-length cases.
8. **Define behavior against an existing identity — no silent
   replacement.** When the request already carries a recognized EC:
   resolving to the **same** identity is an idempotent no-op (cookie
   refreshed, same response); resolving to a **different** identity is
   **rejected** — replacing a live identity via an unauthenticated POST is
   identity takeover, and any legitimate re-identification flow (account
   link, vendor migration) is an explicit linking design this spec does
   not authorize (own open question, §7).
9. **Make replay consumption and graph persistence one idempotent
   sequence — without letting idempotency reinstall the identity
   elsewhere.** Neither naive order works: consume-the-nonce-first makes a
   subsequent graph failure unretryable (the token is spent, the identity
   never existed); graph-first lets the losers of a replay race leave
   residual rows. Required shape: consumption is an **atomic single-key
   reservation** (CAS — an adapter capability the composition root checks,
   providers spec §7) keyed by the payload's unique id, with explicit
   states: `pending` → `committed` | `failed`, each carrying an **owner
   hash** (the session binding) and a **monotonic lease epoch**. Takeover
   of an expired `pending` lease increments the epoch, and every state
   transition is a fenced CAS on (state, epoch) — a stale owner resuming
   after its lease expired cannot commit over the takeover's work, because
   its epoch no longer matches. `failed` is retryable: the same owner may
   supersede it with a fresh `pending` at a higher epoch. The graph write
   happens under the reservation and is deterministic under its key, so
   any retry converges on the same row; reservations are retained at
   least through the token's expiry. Reservation keys are **namespaced**
   per the providers spec §6.3 grammar
   (`resv/<publisher-origin-hash>/<provider>/<version>/<payload-id>`), so
   payloads cannot collide across publishers, providers, or versions.
   Ownership conflicts are terminal per state: a non-owner hitting
   `pending` gets `409` (retry only after lease expiry); a non-owner
   hitting `committed`/`failed` gets the no-cookie terminal response; an
   owner hitting `failed` may supersede it (higher epoch); cleanup
   deletes reservations after retention, never before token expiry.

   **A duplicate must never receive the cookie unless it proves the
   original session binding.** "Duplicates observe the recorded outcome"
   cannot mean replaying `Set-Cookie` — that would hand a captured token's
   identity to a second browser, recreating the fixation §2 exists to
   prevent. A requester matching the reservation's owner hash **does**
   have the `Set-Cookie` re-emitted — which is precisely how a legitimate
   browser whose original response was lost recovers on retry, so a
   committed graph row never strands as an orphan for the intended
   browser; anyone else gets a terminal response with no cookie. The **same-identity no-op of §3.8 first checks the family revocation
   record**, and "revocation wins" is enforced by an explicit
   linearization point, not by read strength alone — a linearizable read
   followed by a separate commit still loses the race (read "not
   revoked" → withdrawal commits → resolve emits a cookie for a revoked
   family). The family record carries an **epoch** (providers spec
   §6.3), bumped by every revocation-state change; the resolve reads
   epoch _e_ at the start, and its cookie-emitting commit is a **CAS in
   the strong class conditioned on the family epoch still being _e_**.
   A withdrawal landing between read and commit bumps the epoch, the
   commit fails, and the resolve is rejected — the CAS is the
   linearization point. A resolve against an already-revoked family is
   rejected, never refreshed. Tests cover crash-between-steps, lease
   takeover with a stale-epoch commit attempt, two concurrent requests
   with the same payload, a duplicate from a second client receiving no
   cookie, owner-hash recovery receiving the cookie, and
   resolve-vs-revocation races.

## 4. Requirements on the page script

**The page leg is permission-gated before it executes.** The browser
module obtains or derives a vendor identity — vendor contact, stable
identifier in hand — so injecting it whenever the provider is merely
_selected_ would run identity code for a visitor who denied everything,
with only the later POST refused. The module is injected/activated only
when the request's resolved permissions already satisfy the provider's
complete `required_permissions()`, and the page leg is a listed row in
the permission spec's §7 enforcement inventory (deferred alongside this
feature). Injection-time gating alone has a TOCTOU gap: consent can be
withdrawn between document delivery and asynchronous vendor contact, or
the document can be restored from BFCache long after its permissions
were resolved — so the module must additionally perform a **live
CMP/permission check immediately before vendor contact** and abort on
permission change or BFCache restoration; endpoint rejection is too late
to undo browser-side identity derivation or vendor egress.

- The re-post guard must not depend on reading an HttpOnly cookie. Either
  the server injects a "resolved" marker the script _can_ read (a
  non-identity companion cookie or an injected page variable), or the
  endpoint is cheap-idempotent and rate-limited per session; the design must
  state which and test it.
- The JS module ships through the standard integration bundle mechanism,
  loaded only when a client-cycle provider is the selected EC provider. Note
  the consequence: bundle content becomes a function of EC configuration,
  which interacts with the bundle's content-hash/SRI pinning and caching —
  the mechanism today keys off the integration registry, not EC provider
  selection (open question, §7).
- Any constant shared between Rust and TS (endpoint path, marker name) is
  asserted equal by a test, not "kept in sync by hand".

## 5. Demo providers

A demonstration provider (fixed identifier, no verification) fails §3.2 by
design and therefore MUST NOT be selectable in a production build: gate it
behind a cargo feature or `#[cfg(test)]` so the settings validator does not
accept its key in release artifacts. PR #838's `client-fixed` was selectable
in any production config, giving every visitor the same identity, with a doc
sentence as the only guardrail.

## 6. Testing

- Endpoint: origin-rejection (including `Sec-Fetch-Site`-only requests,
  which must fail), expired/replayed/foreign-audience payload rejection,
  graph-unavailable refusal, permission-gate refusal, and the §3.7 body /
  content-type / identifier limits at their exact boundaries — each as an
  integration test, not only unit tests.
- Browser round trip (JS → POST → Set-Cookie → next request recognized) in
  the integration suite; PR #838 shipped the JS with in-process unit tests
  only, including one asserting a state (reading the HttpOnly cookie) that
  cannot occur in a real browser.
- Parity: all four adapters.

## 7. Open questions — to be settled in the feature's issue before any code

0. The commit path spans keys (reservation, identity row, family-epoch
   CAS): the cross-key atomicity or saga/compensation design is
   **undefined** — the single-key CAS steps are specified, their
   composition is not.

1. Which concrete vendor scheme is the first real consumer, and does its
   envelope format satisfy §3.2 (audience binding, expiry)? If no concrete
   consumer exists, the feature waits — the demo provider is not a
   consumer.
2. Does the resolve flow need consent-state echo in its response (so the
   page can react), and if so what is the minimal disclosure?
3. Rate limiting / abuse posture at the edge for an unauthenticated POST.
4. Whether the endpoint should be versioned separately from the identify
   API family it sits beside.
5. The shape of the publisher **origin allowlist** that §3.1 validates
   against — no such configuration exists today (`publisher.origin_url` is
   a single upstream and the cookie domain is a cookie scope, not an origin
   set), so it is new settings surface this feature must define.
6. How JS module selection keyed off EC provider configuration coexists
   with content-hashed/SRI-pinned bundles (§4) — per-config hashes, cache
   keying, and the config-push story for them.
