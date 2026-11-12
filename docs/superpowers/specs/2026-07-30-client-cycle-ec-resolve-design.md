# Design Spec: Client-Cycle Edge Cookie Modules and the Resolve Endpoint

**Status:** Proposed. The implementation is the series branch
`split/4-client-resolve`, carried by pull request
IABTechLab/trusted-server#1346, and is not merged to main.
The anti-replay reservation of §3.9 and the vendor envelope verification it
serves are deliberately not in v1. They land with the first real vendor
scheme, which brings the concrete envelope format the reservation design must
fit. §8 compares v1 with the 2026-07-31 draft and says why the feature is
normative rather than deferred.
**Author:** Engineering
**Issue references:** #778 (series), successor spec of the 2026-07-31 draft
**Related specs:** `2026-07-30-pluggable-providers-design.md`,
`2026-07-30-permission-model-design.md`
**Related PRs:** IABTechLab/trusted-server#1346 (Tech Lab pull request) carries the
implementation. IABTechLab/trusted-server#1345 (Tech Lab pull request) carries the
permission model that the implementation builds on.
IABTechLab/trusted-server#1342 holds every design document of the stack.
**Last updated:** 2026-10-16

> **Why this spec exists.** A client-cycle Edge Cookie module lets a browser
> post a value to a public endpoint (`POST /_ts/api/v1/ec/resolve`), and the
> endpoint turns that value into the first-party `ts-ec` cookie. The value
> comes from outside the edge, so the endpoint needs a threat model before it
> has an implementation. This spec is that threat model and the requirements
> that follow from it, each marked with what the code on the series branch
> does.

---

## 1. Overview

A **client-cycle** EC module establishes the identifier via a browser
round trip, where server-injected first-party JS obtains or derives a value in the
page (typically a signed envelope from a vendor identity system), posts it to
a Trusted Server endpoint, and the endpoint, after module-specific
verification, sets the first-party `ts-ec` cookie.

A client-cycle module differs from a server-side module in one
security-critical way, which is that **the identifier is
attacker-influenceable input**, not server-derived evidence. Everything in
this spec follows from that.

This feature is normative in the series rather than deferred because the first
vendor integration this project targets works client-side by design, since the
page script talks to the vendor's identity system and hands the result to
the edge, so the server-side path alone cannot carry it. That integration is a
planned vendor module, and v1 builds the endpoint that module will verify
against.

## 2. Threat model

| Threat                           | Vector                                                                                                                                                             | Consequence if unmitigated                                                                                                                                                                                                                                       |
| -------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Cross-site identity fixation** | `text/plain` POST is a CORS-simple request: any page on the web can `fetch(resolveUrl, {method: "POST", credentials: "include", body: payload})` with no preflight | An attacker pins a chosen identity onto a victim's first-party cookie jar, a login-CSRF for the ad-identity layer, and the victim's activity accretes to an attacker-controlled ID                                                                               |
| **Replay**                       | A captured valid payload (from the attacker's own session or a leak) replayed against another browser                                                              | Same as fixation, without needing to create payloads                                                                                                                                                                                                             |
| **Phantom identity**             | Endpoint sets the cookie without an identity-graph row                                                                                                             | Later requests carry an EC that the KV graph has never seen. Downstream sync and withdrawal logic operate on an identity that half-exists (the organic generation path explicitly refuses to write a cookie when the graph write fails, for exactly this reason) |
| **Un-tombstoneable identity**    | Core does not recognize the module's identifier shape                                                                                                              | Withdrawal cannot expire or tombstone the identity, which is a compliance failure and not only a bug                                                                                                                                                             |
| **Amplification**                | The page script cannot observe an HttpOnly cookie, so it cannot know the cookie is already set                                                                     | A POST on every page view of every session, for example from a script that gates on reading a cookie its own server marked HttpOnly, which makes the guard permanently false                                                                                     |

## 3. Requirements on the endpoint

`POST /_ts/api/v1/ec/resolve` MUST:

1. **Reject cross-site requests with an origin check.** Implemented. v1
   authorizes a request only when its `Origin` header is the same origin as an
   accepted origin under RFC 6454, meaning the scheme, host and port triple of
   RFC 6454 section 4 compared by the rule of its section 5, with a missing
   port standing for the scheme's default, and with a value that is not a
   serialized origin under its section 6.2 never matching. The default
   accepted set is the single origin `https://{publisher.domain}` and nothing
   else, so a sibling subdomain, the `http://` scheme and a non-default port
   are all refused. A missing, unreadable or foreign `Origin`, the opaque value
   `null` included, is rejected with `403`. Browsers always send `Origin` on
   POST `fetch`, so its absence means a non-browser caller, which has no
   business on a page-script endpoint.

   An operator may accept further origins with `[ec] resolve_allowed_origins`,
   a list that is empty by default, for a publisher whose pages are served
   from `www` or from another domain. Each entry is compared by the same RFC
   6454 rule and must itself be a serialized origin, being `http://` or
   `https://` and a host with an optional port, and nothing after it. An entry
   with a path (a trailing slash included), a query, a fragment, a user name or
   another scheme could never match a request, so it is refused when the
   settings load and again when the configuration is validated for deploy,
   with a message naming the entry. The list is exact and there is no suffix
   match on `publisher.domain`, because a suffix match would admit every
   subdomain of the apex, including ones the publisher may not control, and
   the Edge Cookie is scoped to the apex.

   The origin check is defense in depth and not the primary control. The
   primary control is the module's envelope verification with audience and
   session binding, which §3.9 parks and which still has to land with the
   vendor scheme.

2. **Verify the payload per module.** The endpoint hands the posted payload to
   the selected module's `resolve_from_client` and creates only what the module
   returns. Whether the payload is trustworthy is the module's responsibility,
   stated on the trait. A real vendor module verifies a signed, audience-bound,
   expiring envelope. The draft's session-binding and replay analysis (see
   §3.9) is the bar that verification must clear when the vendor scheme lands.
   The demo module verifies a fixed constant and is compiled in only under the
   `client-fixed-demo` feature and in test builds (§5). `resolve_from_client`
   is normative in v1, with a no-op default, so a server-side module needs no
   change of its own. Deployments that select a server-side module are still
   affected in other ways, which the list after the table of checks gives.

3. **Preserve the identity-graph invariant.** Implemented, with differences
   from organic creation that bear on withdrawal. The graph row is written
   before the cookie is set, under the key made from the module's `{code}~`
   prefix and its `normalize_id_for_kv` canonical form, which is the key the
   organic path uses.
   - The write is `create_or_revive`, where organic creation uses
     `create_if_absent`. With no row for the key, both write one. With a live
     row, resolve writes nothing, so the row keeps its original expiry while
     the cookie and the marker are issued again for a full year. With a
     withdrawal tombstone (the row a withdrawal leaves behind for 24 hours,
     marked as not consented, which batch sync reads to refuse partner
     updates), resolve overwrites the tombstone with a live row and deletes the
     record that marks the withdrawal as finished. Resolve revives a tombstone
     whenever the requester's resolved permissions pass the gate. Organic
     creation never revives one, because it discards a candidate whose key
     already has a row and asks the module for another. Whether resolve should
     revive a tombstone is an open question (§7.7).
   - With no graph available the endpoint creates nothing, sets no cookie and
     answers `204`. Resolve is stricter than organic creation, which with no
     graph still commits the identifier for the rest of the request and skips
     only the row write, although its response then carries no cookie.
   - A graph write failure answers `503`, with no cookie.
   - The new row holds the same fields organic creation stores, taken from the
     resolve request itself. The fields are the raw TCF and GPP consent strings
     that a permission signal module accepted, the country, region, network
     number (ASN) and metro code from the geo lookup, the publisher domain, and
     the device class signals the adapter derived for the request.

4. **Round-trip through the lifecycle contract.** Implemented. Read-back goes
   through the selected module's `accepts_id`, the KV key through
   `normalize_id_for_kv`, and withdrawal finds the row through the owning
   module's key, as it does for any other identity. A round-trip test drives
   an opaque client identifier through organic deferral, resolve, cookie set,
   and verbatim read-back, using a test-only module that overrides
   `accepts_id`. The demo module `client_fixed` accepts its own fixed word,
   and a test withdraws its identity (§5). For 24 hours after a withdrawal a
   later resolve can revive the row (item 3).

5. **Exist on every adapter, where parity means identical behavior, including
   identical refusal.** Partially implemented, documented.
   - The Fastly adapter routes `POST` on the path in every deployment,
     whichever module is selected. The adapter passes the same bot-gated identity
     graph as organic generation, so unrecognized clients cannot create through
     resolve either. A test pins that a POST with no `Origin` reaches the
     handler and gets its `403`.
   - The Axum, Cloudflare and Spin adapters deliberately do not route the path,
     matching `identify` and `batch-sync`, which need platform KV those
     adapters do not wire. A POST to the path there falls through to the
     integration proxy and the publisher fallback like any unmatched path, so
     the posted value goes to the publisher origin. The only record is a
     comment in the Spin adapter's route list, which covers all three, and no
     test pins the non-routing.
   - Nothing rejects a client-cycle selection at startup on those three
     adapters. The integration registry in core adds the page script to the
     bundle by module name whichever adapter runs, so the bundle carries the
     script on those adapters too, and the script posts to a path they send to
     the origin.
   - The draft's stronger ask, identical startup rejection of the client-cycle
     selection on adapters that cannot serve it, is the agreed follow-up when
     the portability adapters gain KV (§7.4).

6. **Be uncacheable and permission-gated on the module's full declaration.**
   Implemented. Every response the handler builds carries
   `Cache-Control: no-store`. An error that propagates out of the handler
   becomes the adapter's own error response instead, which on Fastly is built
   with a plain-text body and no `Cache-Control` header of its own. The gate is
   the selected module's complete `required_permissions()` through the same
   resolved permission state as organic creating, not a hard-coded storage
   check. With no module selected the gate is closed.

7. **Bound every input.** Implemented. The request body is at most 65,536
   bytes, where an advertised `Content-Length` over the limit answers `413`
   before the read, and the read body is re-checked so a missing or false
   length does not bypass the bound. The `Content-Type` allowlist is
   `text/plain` and `application/json`, matched on the media type alone,
   case-insensitively, ignoring parameters (the browser's default
   `text/plain;charset=UTF-8` passes). A missing type or any other type answers
   `415`. Core first wraps the identifier the module returns in the module's
   registered code (`provider-code-registry.md`), so the cookie and the
   identity-graph key carry `{code}~value` and a client-set identity is
   namespaced to its module exactly like an edge-created one (the demo's cookie
   value is `cfix~an-ec`). The global identifier bounds then apply to the
   wrapped identifier. The wrapped identifier must be at most 256 bytes, which
   include the five bytes of `{code}~`, and use only the cookie-safe alphabet
   of letters, digits and the characters `.`, `_`, `~` and `-`, the same bounds
   as every other create path. A violation answers `400` and the identifier is never
   rewritten. The answers, and the order of the checks that give them, are in
   the table after item 9.

8. **Define behavior against an existing identity, with no silent
   replacement.** Implemented. "The same identity" means the same cookie value,
   compared as written with the `{code}~` envelope on both sides, and not
   through `normalize_id_for_kv`. Resolving to the same identity writes nothing
   to a live row and issues the cookie and the marker again. Resolving to a
   different identity while the request carries an identity the selected module
   owns is rejected with `409`. A `ts-ec` the selected module does not own is
   not an existing identity, so a resolve replaces it, which is the module
   switch of §4. Any legitimate re-identification flow (account link, vendor
   migration) is an explicit linking design this spec does not authorize.

9. **Replay consumption and graph persistence as one sequence that is safe to
   repeat.** Not in v1, by decision rather than omission. The draft's
   reservation design (a reservation written to a single key only if that key
   is absent, with an owner hash, lease epochs, fenced transitions, and a
   family epoch that makes a revocation win over a resolve) presupposes a
   payload with a unique id, a session binding, and a validity window, which
   are properties of a concrete vendor envelope format that does not exist yet.
   The design also presupposes a reservation record (a state, an owner hash and
   a lease epoch, written under the payload's unique id) that nothing in core
   writes today. The conditional storage write is not what is missing. The Fastly
   adapter's store, the only production store, already writes a key only if
   the key is absent (`EcKvWriteMode::Add`) or only if the stored generation
   still matches (`IfGenerationMatch`), and core already uses the second to
   revive a withdrawn row (`create_or_revive`). Designing the reservation
   against a hypothetical envelope would repeat the mistake this series exists
   to fix. v1's stance is that the endpoint is safe without it for the modules
   v1 ships (the demo creates a constant and is compiled out of production
   builds), and the reservation lands with the first vendor scheme, designed
   against its real envelope, with the draft's §3.9 as the starting bar. Until
   then the draft's text is preserved below as the agreed requirement.

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

### Order of checks

The handler runs these checks in this order and answers at the first one that
fails. An adapter that serves the endpoint must give the same answers in the
same order. Every answer the handler builds carries `Cache-Control: no-store`.

| Step | Check                                                                                                                        | Answer                                                                        |
| ---- | ---------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------- |
| 1    | `Origin` is missing, unreadable or not an accepted origin                                                                    | `403`                                                                         |
| 2    | `Content-Type` is missing, or is not `text/plain` or `application/json`                                                      | `415`                                                                         |
| 3    | The selected module's required permissions are not all set, or no module is selected                                         | `204`, no cookie                                                              |
| 4    | An advertised `Content-Length`, or the body once read, is over 65,536 bytes                                                  | `413`                                                                         |
| 5    | The module's `resolve_from_client` returns an error, or the module asks for a response header inside core's reserved surface | The adapter's error response, which is `500` for an Edge Cookie error         |
| 6    | The module returns no identifier, as when it cannot verify the payload                                                       | `204`, no Edge Cookie, with the module's own response headers                 |
| 7    | The wrapped identifier is over 256 bytes or outside the alphabet                                                             | `400`                                                                         |
| 8    | The request carries a different identity that the selected module owns                                                       | `409`                                                                         |
| 9    | No identity graph is available, because `[ec] ec_store` is not set or the client is not recognized as a browser              | `204`, no cookie                                                              |
| 10   | The graph write fails                                                                                                        | `503`, no cookie                                                              |
| 11   | None of the above                                                                                                            | `200`, with `ts-ec`, `ts-ecr=1` and any response headers the module asked for |

The code tests the gate and then the module, and a request with no module
selected has a closed gate, so both give the answer of step 3. A module's
response headers may not set a cookie whose name starts with `ts-`, a header
whose name starts with `x-ts-`, or any of `cache-control`, `connection`,
`content-length`, `keep-alive`, `proxy-authenticate`, `proxy-authorization`,
`te`, `trailer`, `transfer-encoding` and `upgrade`. A module that asks for any
of these has broken its contract, so the request fails at step 5 and the
header is not dropped, on the path that creates an identifier and on the path
that does not. Each answer in the table has a test. For the `500`, the test checks the error
that refusing a reserved response header returns, which `error.rs` maps to
`500`.

### Effects on deployments that select a server-side module

The no-op default of `resolve_from_client` leaves server-side modules alone.
Three other changes reach every deployment, whatever module is selected.

1. The Fastly adapter registers the route in every deployment. A POST that
   passes the origin, content-type and size checks answers `204`, because the
   default `resolve_from_client` creates nothing, and no POST to the path
   reaches the publisher origin.
2. Every withdrawal response also expires `ts-ecr`, because `expire_ec_cookie`
   calls `expire_ec_resolved_marker`.
3. The marker rule of §4 runs on every response to a request that has an open
   gate and carries a `ts-ec` the selected module does not own.

The API reference entry for the endpoint (`docs/guide/api-reference.md` on the
series branch) lists only `200`, `204` and `413` and says `Auth: None`. The
entry has no `Origin` rule and none of the `400`, `403`, `409`, `415`, `500`
and `503` answers.

## 4. Requirements on the page script

- **The re-post guard must not depend on reading an HttpOnly cookie.**
  Implemented as the draft's first option. The resolve response sets a
  non-HttpOnly companion marker cookie (`ts-ecr=1`) carrying no identity,
  which is the only signal the page has that a resolve succeeded. The
  marker shares the Edge Cookie's scope and lifetime and is expired
  together with it on withdrawal, so a visitor who later re-establishes
  the permission can resolve again. Only a `200` sets the marker. The demo
  page script does not read the status of the answer, so after any other
  answer (a `403` for an origin the operator has not listed, a `204` for a
  deployment with no `[ec] ec_store`, a `409` or a `503`) it posts again on
  every later page view, and the amplification of §2 returns until a resolve
  succeeds (§7.9).
- **The marker must not outlive the module that set it.** The marker
  carries no identity, so unlike the `ts-ec` cookie it is not namespaced
  by the module code envelope, and a long `Max-Age` that only
  withdrawal expires would leave it standing across a module switch.
  The visitor would then hold a marker saying a resolve has already
  succeeded with no identity behind it, and the page script would
  suppress the re-post that would start a new one, so the visitor sits
  with no identity rather than a restarted one. Core therefore expires
  the marker, in the finalization step that runs after the handler, on any
  request whose permission gate is open and that carries a `ts-ec` the
  selected module does not own, which is the same `{code}~` ownership test
  core already applies to the identifier itself. A switch then restarts the
  client cycle instead of stalling it. The finalization step also runs after
  the resolve handler. So the first resolve after a switch sets `ts-ecr=1`,
  the finalization step then appends an expiry for the same cookie, and the
  browser, which applies the two in order, ends without a marker. The page
  script posts once more on its next view, and that post carries the new
  identity, which the module owns, so the marker then stays. The marker stays
  only for a module that owns its own cookie, as the demo module does (§5).
  No test runs the handler and the finalization step together after a module
  switch (§7.8).
- **The page leg is permission-gated before vendor contact.** The demo
  module contacts no vendor (it posts a constant), but it follows the rule
  a vendor module will follow, which is to declare the Data Use its
  server-side module requires (`necessary.operations.storage`, kept in step
  by a Rust test), wait on `tsjs.whenPermissions()`, and post only when that
  Data Use is in the set. With no permission state on the page it does not
  post. The draft's live-CMP re-check binds the first vendor module rather
  than v1, because the demo has no vendor contact to re-check before. A
  vendor module must not derive identity or
  contact the vendor for a visitor whose resolved permissions do not
  satisfy the module's declaration, must re-check immediately before
  vendor contact (consent can change between document delivery and
  asynchronous vendor contact, including BFCache restoration), and its
  injection is keyed off the module selection exactly as the demo's is
  today.
- **How the resolved permissions reach the browser.** The requirement above
  assumes the page can read the server's decision. The binding constraint is
  that the JavaScript bundle is composed at startup and served under a content
  hash, so one body is shared across every visitor and cannot carry
  per-visitor permission state, which means the signal has to be per request.
  The permission model (IABTechLab/trusted-server#1345), which this branch
  builds on, picks the injected-value carrier and delivers it. The page
  receives `window.tsjs.permissions`, an object of four lists,
  `{"awaiting": [], "set": [], "signals": [], "tdls": []}`. The `set` list
  names the Data Uses set for the request, using the same keys as
  `permissions.yaml` and `Permission::as_str()`. The `awaiting` list names the
  Data Uses still waiting for a signal, `signals` carries the signals the
  modules read and found valid, and `tdls` carries the terms documents the data
  is available under. The page always receives all four, and an empty state is
  `{"awaiting":[],"set":[],"signals":[],"tdls":[]}`. Under inline assembly the
  object is injected as a `<script>` at the open of `<head>`, before the tsjs
  bundle, on every HTML document that has a `<head>`. Under shared-template
  (ESI) assembly the `<head>` is part of the cached template shared across
  visitors and can carry nothing request-scoped, so the value is spliced into
  the per-request `</body>` seam script instead, and a permissions-only seam
  script is emitted even when the ad stack did not run, so a bot-classified
  visitor, or one for whom the auction was gated off, still receives the state
  resolved for the request rather than nothing. Because the arrival point
  moves, a page module waits on `tsjs.whenPermissions()`, a promise that
  resolves when the value arrives, or with the empty state at
  `DOMContentLoaded` if nothing arrived, which a module reads as nothing set,
  and does nothing with identity and contacts no vendor before it resolves.
  That promise is the point a vendor module gates on. The server's resolved
  permission decision remains the authority, and an in-page CMP read is only a
  withdrawal re-check layered under it and never a substitute for it, because a
  page-side read can narrow what the server resolved and must never widen it.
  No other integration page script waits on the promise yet, and wiring them up
  to it is follow-up work.
- The demo script ships through the standard integration bundle mechanism.
  Core's integration registry adds its module, `ec_client_fixed`, when
  `[ec] module` selects the name `client_fixed` and for no other name, so a
  vendor client-cycle module needs a registration of its own. The interaction
  between module-keyed bundle content and content-hash / SRI pinning remains
  open (§7.6).
- **Any constant shared between Rust and TS is asserted equal by a test.**
  Partly implemented. A Rust test
  (`the_fixed_word_and_marker_name_match_the_page_script`) reads the
  page-script source and asserts that the fixed word, the marker cookie name
  and the required Data Use match their Rust constants, so a rename on either
  side fails the build instead of silently breaking the round trip. The
  endpoint path in the script (`RESOLVE_ENDPOINT`) is a fourth shared
  constant, and no test pins it to the route in the Fastly adapter.

## 5. Demo modules

Implemented as required. The `client_fixed` demonstration module (fixed
identifier, constant-equality verification) is compiled only behind the
`client-fixed-demo` cargo feature and in test builds, through
`cfg(any(test, feature = "client-fixed-demo"))`. In a production build the
settings validator rejects the selection at startup with a direct message, and
the module builder rejects it again as defense in depth. Both refusals are
compiled only into a build with neither the feature nor `cfg(test)`, so no test
run reaches them. A fixed shared word is not an identity. The demo exists to
exercise verify-before-create end to end in tests and demonstrations.

The demo module's `accepts_id` accepts its fixed word and nothing else, so when
the `cfix~an-ec` cookie comes back, core finds that the selected module owns
it. Three things follow from that in the code.

- The request context holds the identity.
- The finalization step leaves `ts-ecr` alone (§4), so the demo page script
  posts once and not on later page views.
- Withdrawal tombstones the row the resolve wrote and expires the cookie and
  the marker in the browser.

## 6. Testing

- Endpoint unit tests (`ec/resolve.rs`) cover origin rejection (missing,
  foreign, a sibling subdomain of the publisher apex, the `http://` scheme, and
  a non-default port), acceptance of an operator-configured extra origin, the
  origin matching rules (default ports, case, and values that are not
  serialized origins), content-type rejection of a form body, the body bound
  (an oversized body, and an advertised length refused before the read), the
  identifier bound, the different-identity conflict, the closed permission
  gate, the missing module, the missing graph, the unverified payload, the
  `503` for a failed graph write, the refusal of a reserved response header
  with and without an identifier, the module's headers added to the handler's
  own, and the marker, cache-control, and graph-row effects of a success.
- Settings tests (`settings.rs` and `config.rs`) cover the entries of
  `[ec] resolve_allowed_origins` that load, the ones refused at load, and the
  one refused at deploy.
- A round-trip test drives a client identifier through organic deferral,
  resolve, cookie set, and verbatim read-back recognition.
- Two finalization tests (`ec/finalize.rs`) assert that a request carrying a
  `ts-ec` the selected module does not own expires the `ts-ecr` marker, and
  that a `ts-ec` the module owns leaves the marker alone.
- Three demo tests (`ec/resolve.rs`) run the `client_fixed` module through
  permission assembly, the handler and the finalization step, with a browser
  cookie jar between requests. They assert that the cookie comes back
  recognized with its marker kept, that the page script posts on the first of
  three views only, and that a withdrawal tombstones the row and expires both
  cookies. A module test (`ec/module.rs`) pins that `client_fixed` owns
  `cfix~an-ec` and not the bare word, another word under its code, its word
  under another code or an HMAC-shaped value under its code.
- The cross-language constant test pins the fixed word, the marker cookie name
  and the required Data Use to the page-script source.
- A Fastly adapter test pins that a POST to the path reaches the handler.
  Registry tests pin that the demo page script is added when `client_fixed` is
  selected and not otherwise. The page script has its own tests
  (`index.test.ts`).
- No test on the series branch covers a repeat resolve for the same identity,
  an `application/json` body, a `Content-Type` with a charset parameter or
  capital letters, the handler and the finalization step together after a
  module switch (§4), the endpoint path constant, the non-routing on Axum,
  Cloudflare and Spin, or the two production refusals of §5.
- Remaining for the vendor scheme are a real-browser integration round trip
  (JS → POST → Set-Cookie → next request recognized) in the browser suite, and
  the §3.9 reservation tests (crash-between-steps, lease takeover, concurrent
  duplicates, owner-hash recovery, resolve-vs-revocation races).

## 7. Open questions

Items 0 to 6 are carried to the vendor-scheme issue. Items 7 to 10 are about the
code on `split/4-client-resolve`, and each states what the code does and asks
whether that is intended.

0. The commit path spans keys (reservation, identity row, family-epoch CAS), and
   the cross-key atomicity or saga/compensation design is undefined, since the
   single-key CAS steps are specified and their composition is not.
1. The first vendor scheme's envelope format, measured against §3.2 and
   §3.9 (audience binding, expiry, unique id, session binding).
2. Whether the resolve flow needs consent-state echo in its response, and
   the minimal disclosure if so.
3. Rate limiting / abuse posture at the edge for an unauthenticated POST.
4. Startup rejection of the client-cycle selection on adapters that cannot
   route the endpoint, once the portability adapters gain platform KV. Nothing
   rejects it on Axum, Cloudflare or Spin today, and no test pins that those
   adapters do not route the path (§3.5).
5. **Answered, and kept here for the record.** An explicit multi-origin
   allowlist for publishers whose pages run on domains other than the
   configured apex. §3.1 requires exact serialized-origin comparison,
   with a default accepted set of the single origin
   `https://{publisher.domain}` and the operator-configured list
   `[ec] resolve_allowed_origins` of further exact origins for the `www` or
   other-domain case, so the question is settled rather than carried.
6. How JS module selection keyed off EC module configuration coexists
   with content-hashed/SRI-pinned bundles, meaning per-config hashes, cache
   keying, and the config-push story for them.
7. Whether resolve should revive a withdrawal tombstone. Resolve calls
   `create_or_revive`, whose own documentation says it is reserved for explicit
   same-key revival. Organic creation calls `create_if_absent`, which leaves
   any existing row alone (§3.3). So for 24 hours after a withdrawal, a
   resolve for the same identity from a requester whose resolved permissions
   pass the gate turns the tombstone back into a live row and deletes the
   record that marks the withdrawal as finished. A repeat resolve for a live row
   also leaves the row's expiry alone while the cookie is issued again for a
   year, so the cookie can outlive its row. The code question is whether both
   are intended, or whether resolve should leave an existing row alone as
   organic creation does and answer something specific when the row is a
   tombstone.
8. Whether the finalization step should leave the marker alone on the response
   of a resolve that has just set it. The first resolve after a module switch
   sets `ts-ecr=1` and the finalization step then expires it (§4), so one more
   post is needed, and no test covers the two together after a module switch.
9. What the demo page script should do after an answer other than `200`. It
   ignores the status and posts again on every later page view (§4).
10. Whether a client-cycle selection with no `[ec] ec_store` should fail as
    `identify` does, with a `503` and a log line saying the store is not
    configured, and not answer `204`. Every post that reaches step 9 of the table in §3 answers
    `204` and logs a warning.

## 8. Differences from the 2026-07-31 draft

| Draft position                                                                  | v1                                                                                                                                                                                                                                                                               | Why                                                                                                                                                                                     |
| ------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Feature deferred, `resolve_from_client` de-normalized                           | Feature normative. Trait method kept with a no-op default                                                                                                                                                                                                                        | The first vendor integration works client-side by design, so the endpoint is on the critical path for the series' first real module                                                     |
| Origin check via new allowlist config + CSRF token                              | RFC 6454 same-origin comparison of the scheme, host and port triple. Default accepted set is the single origin `https://{publisher.domain}`, plus the optional list `[ec] resolve_allowed_origins` of further exact origins. A missing or foreign `Origin` is refused with `403` | The draft's exact allowlist is adopted, which closes §7.5. A suffix match on the apex would admit every subdomain, including ones the publisher may not control                         |
| Marker cookie survives whatever happens to the identity it marks                | Core expires the `ts-ecr` marker on any response to a request whose gate is open and that carries a `ts-ec` the selected module does not own                                                                                                                                     | The marker is not namespaced by the module code envelope, so without this a module switch leaves a visitor with a marker, no identity, and a page script that will not re-post          |
| §3.9 reservation/replay machinery required before code                          | Deferred to the vendor scheme. Draft text retained verbatim as the bar                                                                                                                                                                                                           | The design needs the real envelope's unique id and session binding, and a reservation record that nothing in core writes. The Fastly store already offers the conditional writes (§3.9) |
| Identical behavior or identical startup refusal, 4 ways                         | Fastly routes it (bot-gated graph). Portability adapters documented as deliberately not routing, like identify, with no startup refusal and no test of the non-routing                                                                                                           | Same platform-KV constraint as the existing EC API routes. Startup rejection follow-up recorded (§7.4)                                                                                  |
| Marker cookie or injected variable (design must pick)                           | Marker cookie (`ts-ecr`), expired with the EC cookie                                                                                                                                                                                                                             | The draft's own first option. Testable and observable                                                                                                                                   |
| Demo gated by cargo feature or `#[cfg(test)]`                                   | Compiled under `cfg(any(test, feature = "client-fixed-demo"))`, with startup rejection in the settings validator in a build with neither                                                                                                                                         | Defense in depth                                                                                                                                                                        |
| Everything else in §3 (graph row, bounds, 409, no-store, full-declaration gate) | Implemented as specified. The graph row differs from organic creation as §3.3 says                                                                                                                                                                                               | (none)                                                                                                                                                                                  |

## Revision record

| Date       | Change                                                                                                                                                                                                                                                                                                                                                                                        |
| ---------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 2026-08-25 | Revised against the implementation.                                                                                                                                                                                                                                                                                                                                                           |
| 2026-10-06 | Said module where it said provider, as the code does.                                                                                                                                                                                                                                                                                                                                         |
| 2026-10-15 | Fixes from review round 1. The conditional storage write claim, the graph write and its effect on withdrawal, the order of the checks and their answers, the configuration key, the adapters, the effects on other deployments, the marker and page script behavior, the demo module's cookie, the tests, the pull request and RFC section references, the open questions and the review log. |
| 2026-10-16 | The demo module `client_fixed` accepts its own fixed word in `accepts_id`, so core recognizes its cookie, keeps the marker and tombstones the row on withdrawal (§5).                                                                                                                                                                                                                         |
| 2026-10-16 | Review round 2, approved, with the RFC 6454 section and the 500 test wording corrected.                                                                                                                                                                                                                                                                                                       |

## Review log

1. **Round 1, 2026-10-15.** A review in a fresh session, against `split/4-client-resolve` at `a70e9d20c`. Issues found, two of them blocking. The two were the statement that no production adapter offers a conditional storage write, which the Fastly store and the revival of withdrawn rows disprove, and the statement that resolve writes the graph row exactly as organic creation does, when resolve leaves a live row alone and revives a withdrawal tombstone. This revision states what the code does in both cases, adds the order of the checks, the configuration key, the adapters, the effects on other deployments, the demo module's cookie and the untested paths, and leaves five code questions under Open questions until the code changes.
2. **Round 2, 2026-10-16.** A lighter review in a fresh session, checking the round 1 fixes and the demo module's fix against `split/4-client-resolve` at `7ab530225`. Approved, none blocking. This revision cites RFC 6454 section 6.2 for the opaque origin, as the code does, and says the `500` is tested as the error that `error.rs` maps to `500`.
