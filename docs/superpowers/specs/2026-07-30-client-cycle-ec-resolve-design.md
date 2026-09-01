# Design Spec: Client-Cycle Edge Cookie Providers and the Resolve Endpoint

**Status:** Proposed. PR #1046 carries the implementation (hardened v1, on
the threat model below) and is not yet merged to main.
The full anti-replay reservation machinery (§3.9) and the vendor envelope
verification it serves are deliberately not in v1: they land with the first
real vendor scheme, which brings the concrete envelope format the
reservation design must fit. §8 records exactly what v1 implements, what it
defers, and why the feature is normative rather than deferred.
**Author:** Engineering (revised against the implementation, 2026-08-25)
**Issue references:** #778 (series), successor spec of the 2026-07-31 draft
**Related specs:** `2026-07-30-pluggable-providers-design.md`
**Last updated:** 2026-09-01

> **Context.** PR #838 shipped, undeclared and unspec'd, a second provider
> _type_: a "client-cycle" EC provider whose identifier is established by a
> browser POST to a new public endpoint (`POST /_ts/api/v1/ec/resolve`),
> plus a demo provider (`client-fixed`) and a JS bundle. Review found the
> endpoint accepted cross-origin identity-setting posts with no origin
> check, created cookies with no identity-graph row (violating an invariant
> the organic path enforces explicitly), was registered on only one of four
> adapters, and could never round-trip because the core did not recognize
> non-HMAC identifiers. None of that is an argument the feature is a bad
> idea. Vendor identity systems with a browser leg (for example
> signed-envelope schemes) are a real integration target, and for this
> project the client-side path is the preferred route for the first vendor
> integration. It is an argument that the feature needs a threat model
> before an implementation. This spec is that threat model; PR #1046 is the
> implementation measured against it.

---

## 1. Overview

A **client-cycle** EC provider establishes the identifier via a browser
round trip, where server-injected first-party JS obtains or derives a value in the
page (typically a signed envelope from a vendor identity system), posts it to
a Trusted Server endpoint, and the endpoint, after provider-specific
verification, sets the first-party `ts-ec` cookie.

This differs from server-side providers in one security-critical way: **the
identifier is attacker-influenceable input**, not server-derived evidence.
Everything in this spec follows from that.

This feature is normative in the series rather than deferred because the first
vendor integration this project targets works client-side by design, since the
page script talks to the vendor's identity system and hands the result to
the edge, so the server-side path alone cannot carry it. The 2026-07-31
draft deferred the feature for lack of a consumer. The consumer now exists
as a planned vendor provider, and v1 builds the endpoint that provider will
verify against.

## 2. Threat model

| Threat                           | Vector                                                                                                                                                             | Consequence if unmitigated                                                                                                                                                                                                                                       |
| -------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Cross-site identity fixation** | `text/plain` POST is a CORS-simple request: any page on the web can `fetch(resolveUrl, {method: "POST", credentials: "include", body: payload})` with no preflight | An attacker pins a chosen identity onto a victim's first-party cookie jar, a login-CSRF for the ad-identity layer, and the victim's activity accretes to an attacker-controlled ID                                                                               |
| **Replay**                       | A captured valid payload (from the attacker's own session or a leak) replayed against another browser                                                              | Same as fixation, without needing to create payloads                                                                                                                                                                                                             |
| **Phantom identity**             | Endpoint sets the cookie without an identity-graph row                                                                                                             | Later requests carry an EC that the KV graph has never seen. Downstream sync and withdrawal logic operate on an identity that half-exists (the organic generation path explicitly refuses to write a cookie when the graph write fails, for exactly this reason) |
| **Un-tombstoneable identity**    | Core does not recognize the provider's identifier shape                                                                                                            | Withdrawal cannot expire or tombstone the identity, which is a compliance failure and not only a bug                                                                                                                                                             |
| **Amplification**                | The page script cannot observe an HttpOnly cookie, so it cannot know the cookie is already set                                                                     | A POST on every page view of every session (PR #838's JS gated on reading a cookie its own server marked HttpOnly, making the guard permanently false)                                                                                                           |

## 3. Requirements on the endpoint

`POST /_ts/api/v1/ec/resolve` MUST:

1. **Reject cross-site requests with an origin check.** v1 authorizes a
   request only when its `Origin` header is the same origin as an accepted
   origin under RFC 6454, meaning the scheme, host and port triple of §4
   compared by the §5 rule, with a missing port standing for the scheme's
   default, and with a value that is not a serialized origin under §6.1
   never matching. The
   default accepted set is the single origin `https://{publisher.domain}`
   and nothing else, so a sibling subdomain, the `http://` scheme and a
   non-default port are all refused. A missing or foreign `Origin` is
   rejected with `403`. Browsers always send `Origin` on POST `fetch`, so
   its absence means a non-browser caller, which has no business on a
   page-script endpoint. An operator may configure further accepted
   origins, each of them compared by the same RFC 6454 rule, for a publisher
   whose pages are served from `www` or from another domain. This is the
   exact allowlist the 2026-07-31 draft asked for, and it replaces the
   suffix match on `publisher.domain` an earlier revision of this spec
   described, which admitted every subdomain of the apex including ones
   the publisher may not control. The origin check is defense in depth
   and not the primary control. The primary control is the provider's
   envelope verification with audience and session binding, which §3.9
   parks and which still has to land with the vendor scheme.
2. **Verify the payload per provider.** The endpoint hands the posted
   payload to the selected provider's `resolve_from_client` and creates only
   what the provider returns. Whether the payload is trustworthy is the
   provider's responsibility, stated on the trait. A real vendor provider
   verifies a signed, audience-bound, expiring envelope. The draft's
   session-binding and replay analysis (see §3.9) is the bar that
   verification must clear when the vendor scheme lands. The demo provider
   verifies a fixed constant and is compiled out of production builds
   (§5). `resolve_from_client` is normative in v1, with a no-op default so
   server-side providers are untouched.
3. **Preserve the identity-graph invariant.** Implemented. The graph row is
   written before the cookie is set, keyed by the provider's
   `normalize_id_for_kv` canonical form, exactly like the organic create
   path. No graph available → no create (`204`), same as organic generation;
   a graph write failure → `503`, no cookie.
4. **Round-trip through the lifecycle contract.** Implemented. Read-back
   goes through the selected provider's `accepts_id`, the KV key through
   `normalize_id_for_kv`, and withdrawal reaches the row like any other
   identity. A round-trip test drives an opaque client identifier through
   organic deferral, resolve, cookie set, and verbatim read-back.
5. **Exist on every adapter, where parity means identical behavior,
   including identical refusal.** Partially implemented, documented. The
   Fastly adapter routes the endpoint (passing the same bot-gated identity
   graph as organic generation, so unrecognized clients cannot create
   through resolve either). The Axum, Cloudflare, and Spin adapters
   deliberately do not route it, matching `identify` and `batch-sync`,
   which need the same platform KV wiring those adapters do not have. The
   route list in the Spin adapter documents all three together. The
   draft's stronger ask, identical startup rejection of the client-cycle
   selection on adapters that cannot serve it, is the agreed follow-up
   when the portability adapters gain KV (§7.4).
6. **Be uncacheable and permission-gated on the provider's full
   declaration.** Implemented. Every response carries `Cache-Control:
no-store`, and the gate is the selected provider's complete
   `required_permissions()` through the same resolved permission state as
   organic creating, not a hard-coded storage check.
7. **Bound every input.** Implemented. Request body at most 65,536 bytes, where
   an advertised `Content-Length` over the limit answers `413` before the
   read, and the read body is re-checked so a missing or false length does
   not bypass the bound. `Content-Type` allowlist: `text/plain` and
   `application/json`, matched on the media type alone, case-insensitively,
   ignoring parameters (the browser's default `text/plain;charset=UTF-8`
   passes); anything else → `415`. The created identifier must fit the
   global identifier bounds (at most 256 bytes, cookie-safe alphabet,
   shared with every other create path); violation → `400`, never a rewrite.
   Core then applies the provider's registered code envelope
   (`provider-code-registry.md`): the cookie and the identity-graph key
   carry `{code}~value`, so a client-set identity is namespaced to its
   provider exactly like an edge-created one (the demo's cookie value is
   `cfix~an-ec`). Status codes are part of the contract: `400` out-of-bounds identifier,
   `403` origin rejection, `409` different-identity conflict, `413` body,
   `415` content type, `503` graph-write failure, `204` closed gate / no
   provider / no graph / unverified payload. Tests exercise each rejection.
8. **Define behavior against an existing identity, with no silent
   replacement.** Implemented. Resolving to the same identity refreshes
   idempotently. Resolving to a different identity while the request
   carries a recognized EC is rejected with `409`. Any legitimate
   re-identification flow (account link, vendor migration) is an explicit
   linking design this spec does not authorize (§7).
9. **Replay consumption and graph persistence as one idempotent
   sequence.** Not in v1, by decision rather than omission. The draft's
   reservation design (atomic single-key CAS reservation with owner hash,
   lease epochs, fenced transitions, and family-epoch revocation
   linearization) presupposes a payload with a unique id, a session
   binding, and a validity window, which are properties of a concrete vendor
   envelope format that does not exist yet, and a CAS-class storage
   primitive no production adapter exposes today. Designing the
   reservation against a hypothetical envelope would repeat the mistake
   this series exists to fix. v1's stance is that the endpoint is safe without it
   for the providers v1 ships (the demo creates a constant, feature-gated
   out of production), and the reservation lands with the first vendor
   scheme, designed against its real envelope, with the draft's §3.9 as
   the starting bar. Until then the draft's text is preserved below as the
   agreed requirement.

The 2026-07-31 draft's §3.9 reservation requirement is retained verbatim as
the bar for the vendor-scheme implementation:

> Neither naive order works: consume-the-nonce-first makes a subsequent
> graph failure unretryable (the token is spent, the identity never
> existed); graph-first lets the losers of a replay race leave residual
> rows. Required shape: consumption is an atomic single-key reservation
> (CAS) keyed by the payload's unique id, with explicit states: `pending` →
> `committed` | `failed`, each carrying an owner hash (the session binding)
> and a monotonic lease epoch. Takeover of an expired `pending` lease
> increments the epoch, and every state transition is a fenced CAS on
> (state, epoch). `failed` is retryable by the same owner at a higher
> epoch. The graph write happens under the reservation and is deterministic
> under its key. A duplicate must never receive the cookie unless it proves
> the original session binding; a requester matching the reservation's
> owner hash has the `Set-Cookie` re-emitted (lost-response recovery),
> anyone else gets a terminal response with no cookie. The same-identity
> no-op first checks the family revocation record, and "revocation wins" is
> enforced by a CAS conditioned on the family epoch read at the start.

## 4. Requirements on the page script

- **The re-post guard must not depend on reading an HttpOnly cookie.**
  Implemented as the draft's first option. The resolve response sets a
  non-HttpOnly companion marker cookie (`ts-ecr=1`) carrying no identity,
  which is the only signal the page has that a resolve succeeded. The
  marker shares the Edge Cookie's scope and lifetime and is expired
  together with it on withdrawal, so a visitor who later re-establishes
  the permission can resolve again.
- **The marker must not outlive the provider that set it.** The marker
  carries no identity, so unlike the `ts-ec` cookie it is not namespaced
  by the provider code envelope, and a long `Max-Age` that only
  withdrawal expires would leave it standing across a provider switch.
  The visitor would then hold a marker saying a resolve has already
  succeeded with no identity behind it, and the page script would
  suppress the re-post that would start a new one, so the visitor sits
  with no identity rather than a restarted one. Core therefore expires
  the marker on any request that carries a `ts-ec` the selected provider
  does not own, which is the same `{code}~` ownership test core already
  applies to the identifier itself. A switch then restarts the client
  cycle instead of stalling it.
- **The page leg is permission-gated before vendor contact.** The demo
  module contacts no vendor (it posts a constant), so the draft's
  injection-time and live-CMP gating requirements bind the first vendor
  module rather than v1. A vendor module must not derive identity or
  contact the vendor for a visitor whose resolved permissions do not
  satisfy the provider's declaration, must re-check immediately before
  vendor contact (consent can change between document delivery and
  asynchronous vendor contact, including BFCache restoration), and its
  injection is keyed off the provider selection exactly as the demo's is
  today.
- **How the resolved permissions reach the browser is now chosen.** The
  requirement above assumes the page can read the server's decision, and
  v1 had no carrier for it. The binding constraint is that the JavaScript
  bundle is composed at startup and served under a content hash, so one
  body is shared across every visitor and cannot carry per-visitor
  permission state, which means the signal has to be per request. The
  permission-model PR (#1045) picks the injected-value carrier and
  delivers it. The page receives `window.tsjs.permissions`, an object
  `{"set": ["necessary.operations.storage", "..."]}` naming the Data Uses
  set for the request, using the same keys as `permissions.yaml` and
  `Permission::as_str()`. Under inline assembly it is injected as a
  `<script>` at the open of `<head>`, before the tsjs bundle, on every
  HTML document. Under shared-template (ESI) assembly the `<head>` is part
  of the cached template shared across visitors and can carry nothing
  request-scoped, so the value is spliced into the per-request `</body>`
  seam script instead, and a permissions-only seam script is emitted even
  when the ad stack did not run, so a consent-denied or bot-classified
  visitor still receives the empty state. Because the arrival point moves,
  a page module waits on `tsjs.whenPermissions()`, a promise that resolves
  when the value arrives, and does nothing with identity and contacts no
  vendor before it resolves. That promise is the point a vendor module
  gates on. The server's resolved permission decision remains the
  authority, and an in-page CMP read is only a withdrawal re-check layered
  under it and never a substitute for it, because a page-side read can
  narrow what the server resolved and must never widen it. Wiring the
  existing integration page scripts to wait on the promise is follow-up
  work once these PRs are on main.
- The JS module ships through the standard integration bundle mechanism,
  loaded only when a client-cycle provider is the selected EC provider.
  The interaction between provider-keyed bundle content and content-hash /
  SRI pinning remains open (§7.6).
- **Any constant shared between Rust and TS is asserted equal by a test.**
  Implemented. A Rust test reads the page-script source and asserts the
  fixed word and the marker cookie name match their Rust constants, so a
  rename on either side fails the build instead of silently breaking the
  round trip.

## 5. Demo providers

Implemented as required. The `client-fixed` demonstration provider (fixed
identifier, constant-equality verification) is compiled only behind the
`client-fixed-demo` cargo feature. In a production build the settings
validator rejects the selection at startup with a direct message, and the
provider builder rejects it again as defense in depth. A fixed shared word
is not an identity. The demo exists to exercise verify-before-create end to
end in tests and demonstrations.

## 6. Testing

- Endpoint unit tests cover origin rejection (missing, foreign, a sibling
  subdomain of the publisher apex, the `http://` scheme, and a non-default
  port), acceptance of an operator-configured extra origin, content-type
  rejection, the body bound, the identifier bound, the different-identity
  conflict, the no-graph refusal, the closed permission gate, the
  unverified payload, and the marker, cache-control, and graph-row effects
  of a success.
- A round-trip test drives a client identifier through organic deferral,
  resolve, cookie set, and verbatim read-back recognition.
- A test asserts that a request carrying a `ts-ec` the selected provider
  does not own expires the `ts-ecr` marker.
- The cross-language constant test pins the endpoint's shared constants to
  the page-script source.
- Remaining for the vendor scheme: a real-browser integration round trip
  (JS → POST → Set-Cookie → next request recognized) in the browser suite,
  and the §3.9 reservation tests (crash-between-steps, lease takeover,
  concurrent duplicates, owner-hash recovery, resolve-vs-revocation
  races).

## 7. Open questions, carried to the vendor-scheme issue

0. The commit path spans keys (reservation, identity row, family-epoch
   CAS): the cross-key atomicity or saga/compensation design is undefined, since
   the single-key CAS steps are specified, their composition is not.
1. The first vendor scheme's envelope format, measured against §3.2 and
   §3.9 (audience binding, expiry, unique id, session binding).
2. Whether the resolve flow needs consent-state echo in its response, and
   the minimal disclosure if so.
3. Rate limiting / abuse posture at the edge for an unauthenticated POST.
4. Startup rejection of the client-cycle selection on adapters that cannot
   route the endpoint, once the portability adapters gain platform KV.
5. **Answered, and kept here for the record.** An explicit multi-origin
   allowlist for publishers whose pages run on domains other than the
   configured apex. §3.1 now requires exact serialized-origin comparison,
   with a default accepted set of the single origin
   `https://{publisher.domain}` and an optional operator-configured list
   of further exact origins for the `www` or other-domain case, so the
   question is settled rather than carried.
6. How JS module selection keyed off EC provider configuration coexists
   with content-hashed/SRI-pinned bundles, meaning per-config hashes, cache
   keying, and the config-push story for them.

## 8. Revision record vs the 2026-07-31 draft

| Draft position                                                                  | v1 (PR #1046)                                                                                                                                                                                                                                     | Why                                                                                                                                                                                                                                              |
| ------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Feature deferred, `resolve_from_client` de-normalized                           | Feature normative. Trait method kept with a no-op default                                                                                                                                                                                         | The first vendor integration works client-side by design, so the endpoint is on the critical path for the series' first real provider                                                                                                            |
| Origin check via new allowlist config + CSRF token                              | RFC 6454 same-origin comparison of the scheme, host and port triple. Default accepted set is the single origin `https://{publisher.domain}`, plus an optional operator-configured list of further exact origins. Missing/foreign `Origin` → `403` | The draft's exact allowlist is adopted. An earlier revision of this spec allowed any suffix match on the apex, which admitted every subdomain including ones the publisher may not control, and its justification did not hold. This closes §7.5 |
| Marker cookie survives whatever happens to the identity it marks                | Core expires the `ts-ecr` marker on any request carrying a `ts-ec` the selected provider does not own                                                                                                                                             | The marker is not namespaced by the provider code envelope, so without this a provider switch leaves a visitor with a marker, no identity, and a page script that will not re-post                                                               |
| §3.9 reservation/replay machinery required before code                          | Deferred to the vendor scheme. Draft text retained verbatim as the bar                                                                                                                                                                            | The design needs the real envelope's unique id and session binding, and a CAS-class primitive no production adapter exposes today                                                                                                                |
| Identical behavior or identical startup refusal, 4 ways                         | Fastly routes it (bot-gated graph). Portability adapters documented as deliberately not routing, like identify                                                                                                                                    | Same platform-KV constraint as the existing EC API routes. Startup rejection follow-up recorded (§7.4)                                                                                                                                           |
| Marker cookie or injected variable (design must pick)                           | Marker cookie (`ts-ecr`), expired with the EC cookie                                                                                                                                                                                              | The draft's own first option. Testable and observable                                                                                                                                                                                            |
| Demo gated by cargo feature or `#[cfg(test)]`                                   | Both: `client-fixed-demo` feature + startup rejection in the settings validator                                                                                                                                                                   | Defense in depth                                                                                                                                                                                                                                 |
| Everything else in §3 (graph row, bounds, 409, no-store, full-declaration gate) | Implemented as specified                                                                                                                                                                                                                          | (none)                                                                                                                                                                                                                                           |
