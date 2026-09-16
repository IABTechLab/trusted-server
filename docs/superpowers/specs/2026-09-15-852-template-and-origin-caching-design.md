# Closing out #852: Origin readthrough, purge, and cache observability

**Date:** 2026-09-15
**Issue:** IABTechLab/trusted-server#852
**Branch:** `852-template-and-origin-caching`
**Status:** Draft, revision 3 (rewritten after two rounds of independent review — seven reviewers total)

## Why this document exists

Issue #852 was filed on 2026-07-05, before #1009 shipped. Its opening premise — "there is
no `CacheOverride`, `set_ttl`, or cache API call anywhere in the repo" — is no longer true,
and two of its four numbered items are substantially built. The issue is nonetheless not
closeable: the part that shipped does not produce a cache hit in a default production
deployment, the part that did not ship was mis-described, and the review that produced this
spec surfaced a pre-existing condition the issue never anticipated.

## Glossary

Three distinct caches sit on the publisher path. Earlier documents numbered them C1/C2/C3;
the 2026-08-19 terminology migration retired those labels for active prose, and this
document uses the names it chose. The distinction is load-bearing — conflating them is what
produced the original wrong conclusion in the #1009 design doc, and, as recorded below,
this spec's own first revision conflated two of them again.

| Name                         | Contents                                                                  | Owner                 | Status                                                                         |
| ---------------------------- | ------------------------------------------------------------------------- | --------------------- | ------------------------------------------------------------------------------ |
| **Origin readthrough cache** | Raw origin bytes, pre-transform                                           | The platform (Fastly) | Exists. Ad-stack requests opt out; **everything else already uses it**         |
| **Template cache**           | Post-`lol_html`, pre-assembly, reader-neutral HTML with a bid-shaped hole | Trusted Server        | Built, Fastly-only, off by default                                             |
| **Assembled-response cache** | Final per-reader output                                                   | Nobody                | **Must never exist.** Naming it is how we keep it from being built by accident |

If a change appears to require the assembled-response cache, that is the leak, not a design
option.

## Current state, per issue item

**Item 1 — cache the origin HTML template.** Mis-described, in both directions. See the two
corrections below.

**Item 2 — cache the transformed HTML.** Built.
`crates/trusted-server-core/src/platform/template_cache.rs` stores the post-transform
template; `AD_ASSEMBLY_SEAM` (`publisher.rs:1284`) is the late-bound hole; a hit returns
before the origin fetch (the `TemplateCacheLookup::Hit` arm at `publisher.rs:4617`).
Fastly-only, stamped spike-only, off by default.

**Item 3 — surrogate keys and purge hooks.** Half, and the half that exists covers the wrong
cache for item 1's purposes. Keys are emitted at template-cache insert
(`platform/template_cache.rs:138`) and `purge_url`/`purge_all` work inside Compute via
`fastly::http::purge::purge_surrogate_key` (`adapter-fastly/src/template_cache.rs:280`).
Nothing operator- or CMS-facing calls them, and none of it touches readthrough objects.

**Item 4 — leave `response_privacy.rs` alone.** Held. Unchanged, and the template-cache hit
path stamps `private, no-store` itself because a hit returns before the normal stamp point.

## Two corrections to the original analysis

These were found by review, are verified against the code, and each invalidates a claim an
earlier revision of this document made.

### Correction 1 — the readthrough cache is already shared, for most traffic

The bypass is `if should_run_ad_stack` (`publisher.rs:4415`, `:4716`). That flag comes from
`should_run_server_side_ad_stack` (`publisher.rs:3077`), which requires GET, navigation,
non-prefetch, non-bot, matched slots, consent, `ad_templates_enabled`, and
`auction_enabled` — **all** of them.

Every request failing any one of those conditions already reaches origin with no
`set_pass`, and therefore already participates in the shared readthrough cache with no
eligibility check whatsoever: bots, prefetches, consent-denied readers, any page with no
matched slot, and all traffic while either kill switch is off.

Calibration, so this is neither ignored nor overstated: what gets stored is still governed
by the origin's own `Cache-Control`, exactly as for any CDN, and the premise of #852 is that
origins mark HTML private. So today this is most likely storing nothing. It is not a live
incident and this spec does not treat it as one. But two consequences follow:

1. Origin readthrough is **not** "the first time publisher HTML can enter a shared cache". It makes
   deliberate a sharing decision that is currently made by omission for the majority of
   requests.
2. Without a fix, the change below would let an _eligible_ ad-serving request read an object stored
   by an _unchecked_ bot request. Opting a second population in while leaving the first
   unguarded is worse than either state alone.

The change below therefore governs the whole population under one rule. That is a tightening
for ineligible bot and prefetch traffic and a widening for cookieless traffic; the gate section states both
sides, and the response-side gap explains why the resulting controls are preconditions rather than code.

### Correction 2 — stripping TS-owned cookies would buy almost nothing

An earlier revision rejected "strip TS cookies from the origin fetch" on correctness-risk
grounds. That reasoning was weak — `strip_cookies` already exists and is used on the
partner-forwarding path (`cookies.rs:89-103`), so the mechanism is precedented.

The decisive argument is different: `cookie_disqualifies` keys on **header presence**, not
cookie identity — `req.headers().contains_key(header::COOKIE)` (`publisher.rs:4283`), feeding
`:4292`. Stripping TS-owned cookies only clears the gate for a reader carrying
_exclusively_ TS cookies. Any publisher analytics, session, or consent-vendor cookie leaves
the header present and the request disqualified. On a real publisher that is close to zero
traffic.

So the rejected alternative trades a correctness risk for a hit-rate win that does not
exist. Same conclusion as before, on a foundation that survives someone reading `:4283`.

## The actual blocker

The template cache disqualifies any request carrying a `Cookie` header (`cookie_disqualifies`,
`publisher.rs:4292`; `TemplateCacheBypassReason::CookieForwarded`). Trusted Server sets its
own EC cookie, so essentially every repeat visitor is excluded. Hit rate in a default
deployment is approximately zero.

The escape hatch is `creative_opportunities.origin_is_cookie_independent`, documented as
"unsafe unless independently verified" — with no tooling to do the verifying. An operator is
asked to assert a byte-level property of their origin on faith.

This is the critical path. Every other item in this spec is inert until it is resolved.

## Goals

1. One explicit, auditable rule governing which requests may share an origin response,
   applied to the whole request population rather than half of it.
2. `origin_is_cookie_independent` settable on evidence rather than faith.
3. Operator and CMS purge surfaces, with an honest statement of what each can and cannot
   purge.
4. Cache outcomes measurable — for both caches, not just the template cache.
5. Documentation that matches the behavior, including a rollback procedure that works.

## Non-goals

- Porting the template cache backing to Cloudflare or Spin. The trait seam exists; separate
  issue.
- Promoting the template cache out of spike status. Moved to successor issue B — it hides
  unsettled design decisions that deserve their own review. See "Successor issues".
- The 2026-08-19 terminology cleanup. Moved to successor issue A.
- Any change to `response_privacy.rs` or the `private, max-age=0` downgrade.
- Stripping cookies from the origin fetch. Rejected in Correction 2.
- TTL and stale-while-revalidate overrides on the origin fetch, despite being explicit in #852
  item 1. They are buildable but unsafe as request-side knobs; see the mechanism section. Successor issue D.
- Fixing the pre-existing `/_ts/admin` credential forwarding. Worked around locally in the admin endpoint;
  successor issue E.
- An assembled-response cache, in any form.

---

## Origin readthrough

This is the only change here with new runtime blast radius. Review it as security-sensitive code.

### Current

```rust
// publisher.rs:4415 and :4716
if should_run_ad_stack {
    platform_request = platform_request.with_cache_bypass();
}
```

`should_run_ad_stack` means "this page serves ads", which is unrelated to whether the origin
response may be shared — and, per Correction 1, leaves every non-ad-stack request sharing by
default.

### Splitting the predicate

Today's `request_can_use_shared_template` (`publisher.rs:4325`) mixes two kinds of condition:

```rust
let request_can_use_shared_template = method_is_cacheable
    && matches!(assembly_mode, AssemblyMode::Esi)   // template-only
    && !request_host.is_empty()
    && !authorization_disqualifies
    && !cookie_disqualifies
    && !request_requires_origin
    && reader_supports_assembly;                    // template-only
```

`assembly_mode` and `reader_supports_assembly` say nothing about whether the origin's bytes
are shareable; they say whether _this_ pipeline can use a shared template. Gating readthrough
on them would mean an operator running default `inline` mode with a verified
cookie-independent origin gets no readthrough caching, ever, for no safety reason — and would
make `assembly_mode = "inline"` silently re-enable the origin bypass, which is the documented
rollback lever.

Split them:

```rust
// Necessary for either cache to share this request's origin response.
let origin_response_is_shareable = method_is_cacheable
    && !request_host.is_empty()
    && !authorization_disqualifies
    && !cookie_disqualifies
    && !request_requires_origin;

// Additionally required to use a shared *template*.
let request_can_use_shared_template = origin_response_is_shareable
    && matches!(assembly_mode, AssemblyMode::Esi)
    && reader_supports_assembly;
```

This is a pure refactor with no behavior change, which is why it is the first commit rather than
part of the gate: the `origin_cache_shareable` telemetry field needs the binding to exist.

### Gating the bypass, at both sites

```rust
if !origin_response_is_shareable {
    platform_request = platform_request.with_cache_bypass();
}
```

`should_run_ad_stack` is **gone**, per Correction 1.

**Both `:4415` and `:4716` change.** An earlier revision claimed `:4415` was dead code. That
was true of the old template-only predicate and is false of this weaker one. The two sites are
not independent: `:4415` populates `pending_origin` inside the EC-preload fan-out block, and
`:4716` is the `else` branch of `if let Some(pending) = pending_origin` (`publisher.rs:4703`).
They are alternative paths for the same fetch. Changing only one makes readthrough eligibility
depend on whether EC preload fired — and `should_preload_ec_snapshot` (`publisher.rs:2955`) is
`is_navigation && is_get && has_ec_id && has_kv`, which is much of the population this change
exists for. In default `inline` mode `template_cache_key` is always `None`, so the preload
branch is the one most eligible navigations take.

Test the invariant directly: identical inputs must produce the same bypass flag on the preload
and non-preload paths.

**Correcting a claim from revision 2.** That revision said dropping `should_run_ad_stack` is
"strictly more conservative". It is not, and the accurate statement is two-sided:

- **Tightening** for cookie-bearing, `Authorization`-bearing, or otherwise ineligible bot and
  prefetch traffic, which gets `set_pass` where today it does not.
- **Widening** for cookieless traffic: eligible ad-serving requests begin reading objects that
  cookieless bots and prefetchers store. Today those objects are written and read only by
  non-ad-stack requests.

The widening is the reason the preconditions below are blocking rather than advisory.

### Readthrough is enabled by omitting `set_pass`, not by a TTL override

Revision 2 proposed adding `with_cache_ttl(Duration)` to `PlatformHttpRequest` to make the
decision "explicit rather than inherited from service defaults". **That was wrong and is
withdrawn.** The pinned `fastly` 0.12.1 crate documents the semantics
(`fastly-0.12.1/src/http/request.rs:2381-2392`, and the shared snippet
`docs/snippets/set-pass-override.md`):

> This overrides any previous `Request::set_pass` call and sets the `pass` behavior to `false`.
>
> …calling any of those methods on Request _after_ calling `set_pass(true)` will reverse the
> effect of the `set_pass` call, and any response received when the request is sent may become
> cacheable.

`set_ttl` overrides the origin's `Cache-Control`, `private` and `no-store` included. Adding it
would convert the hazard this section exists to close — a response the origin refused to let us
share becoming shared — from a service-config accident into a first-class TS API. It also
defeats the fail-safe this spec relies on elsewhere. `set_ttl(0)` does not help either: it
caches with zero TTL, it does not bypass.

**The correct mechanism is the absence of a call.** Not calling `set_pass` leaves Fastly to
honor the origin's own freshness. If the origin marks HTML private, nothing is stored and
Origin readthrough is inert — which is the right failure. The only lever TS applies is `set_pass(true)`
on ineligible requests.

Consequences, stated rather than worked around:

- **TTL and stale-while-revalidate as overrides are out of scope**, and this is a deliberate
  drop from #852 item 1 rather than an oversight. `set_stale_while_revalidate` does exist
  (`request.rs:2395`), so it is buildable — but it carries the identical override semantics,
  so a safe version needs a post-response decision, which means the Core Cache API. That is a
  separate piece of work with its own review. Successor issue D.
- **`after_send` / `CandidateResponse` is not available as an alternative.** The snippet above
  recommends it for exactly this problem, and it is unreachable here: Viceroy 0.17 stubs the
  HTTP Cache ABI and the SDK converts that into a send error, so setting `after_send` makes
  every publisher origin fetch fail under `fastly compute serve`, `cargo test-fastly`, and the
  parity suite. Recorded at `adapter-fastly/src/template_cache.rs:6-15`; do not re-propose it.

### The response-side gap cannot be closed request-side

`template_cache_ttl` (`publisher.rs:6129`) refuses on response state: absent positive freshness
(`:6100`), `Set-Cookie` (`:6147`), foreign edge-cache headers (`:6153`), non-200 (`:6190`),
non-HTML (`:6198`), CSP nonce (`:6117`), uncovered `Vary` (`:6184`).

The readthrough decision is made before the origin is contacted. Per the previous section, there is no reachable post-response hook. **So none of those refusals can be applied to the readthrough path.** An
earlier revision said "either the TTL knob is set to zero for that class or the case is
documented as accepted"; only the second branch exists.

The sharpest case is `Set-Cookie`, and the cookie gate makes it more likely rather than less.
`cookie_disqualifies` keys on request-side header presence (`publisher.rs:4283`, `:4292`), so
`origin_response_is_shareable` is true precisely for readers carrying **no cookie at all** —
first-time visitors, which is exactly when an origin issues a session cookie. Fastly Compute's
readthrough has no VCL `hit-for-pass` boilerplate, so `Set-Cookie: sid=…` alongside
`Cache-Control: max-age=60` is a cacheable shared representation, and the stored object replays
that cookie to every subsequent cookieless reader for the TTL. That is cross-reader session
fixation, and it passes a freshness check cleanly.

**Therefore the controls are preconditions, not code.** Readthrough may be enabled only against
an origin the probe has verified on every blocking axis — self-identity, cookie,
`User-Agent`, absence of `Set-Cookie`, absence of CSP nonce, and positive shared freshness. The
runbook states this, the config doc states this, and the probe's verdict is pass/fail rather
than informational.

This is a weaker guarantee than the template cache's, and the spec says so plainly rather than
implying parity. An operator who enables readthrough against an unverified origin can cross-serve.
If that is judged unacceptable, the honest alternative is to drop the readthrough change and close #852 item
1 as won't-do — see Open risks.

### Rollback

`purge_surrogate_key` is documented as purging "a surrogate key for the current service"
(`fastly-0.12.1/src/http/purge.rs:12`), not a Core-Cache-scoped operation, and
`Request::set_surrogate_key` (`request.rs:2462`) is the request-side surrogate-key surface for
the readthrough cache. Revision 2 called this an unverified spike; **the API question is
settled** — what remains is empirical, and Viceroy cannot model it, so it needs a staging
service.

Two constraints the implementation must respect, both from the same override snippet:

- `set_surrogate_key` **also** cancels `set_pass`. Stamping `ts-origin` unconditionally would
  disable the bypass for every request, including disqualified ones. It may be applied only on
  the `origin_response_is_shareable` branch.
- `set_pass` and `set_surrogate_key` are mutually exclusive on one request, and the interaction
  is order-dependent. The platform layer must make this unrepresentable rather than relying on
  call ordering — one enum, not two booleans.

If the staging check shows readthrough objects are not purgeable this way, readthrough rollback
is flag-flip plus origin TTL, and the runbook must say so.

### Tests

- Table test over the inputs of `origin_response_is_shareable`, asserting the recorded bypass
  flag. `recorded_cache_bypass_flags()` (`platform/test_support.rs:450`) captures what is needed.
- Preload and non-preload paths produce the same bypass flag for identical inputs.
- The predicate split is behavior-neutral: same inputs, same `request_can_use_shared_template`, for
  every combination.
- Regression: cookie-bearing, `Authorization`-bearing, and `request_requires_origin` requests
  still bypass.
- New behavior: an ineligible bot or prefetch request now bypasses where it previously did not.
- The platform layer cannot represent `set_pass` and a surrogate key simultaneously.

---

## Origin shareability probe

```
ts origin probe-shareability --url <url> [--repeat N] [--cookie <name=value>]...
```

Renamed from "probe-cache-independence": cookies are one axis of several, and naming it for one
axis is how an operator ends up with a clean verdict on an origin that still cross-serves.

Per the response-side gap, this probe is not a convenience. It is the only control standing
between readthrough and cross-serving, so its verdicts are blocking and its output is the artifact an operator keeps.

### Axes, all blocking

| Axis                  | Comparison                                        | Why it blocks                                                                                                                                                                                                                                                                                          |
| --------------------- | ------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| **Self-identity**     | Same request twice                                | An origin whose HTML differs per request (timestamps, CSRF nonces, A/B assignment) cannot be shared on any axis. Distinct from, and more common than, cookie personalization                                                                                                                           |
| **Cookie**            | Bare vs. representative TS + publisher cookie jar | The `origin_is_cookie_independent` question                                                                                                                                                                                                                                                            |
| **`Accept-Encoding`** | `gzip` vs. `identity`, compared after decode      | `STRUCTURALLY_COVERED = ["accept-encoding"]` (`platform/template_cache.rs:207`) assumes encoding variants differ only by content coding. Its own doc says operators "must leave ESI disabled if an origin changes document semantics instead" — an obligation shipped in prose with no way to check it |
| **`User-Agent`**      | Desktop vs. mobile UA                             | An origin serving distinct mobile or prerendered HTML without `Vary: User-Agent` is cross-served, since readthrough keys on URL plus origin `Vary` only                                                                                                                                                |
| **RSC**               | Bare vs. an `RSC` flight-fetch header             | RSC fetches already flow through the readthrough cache while HTML navigations are passed, so removing the bypass puts both representations under one cache key for the first time. An origin that varies on these without declaring it can serve a flight payload to an HTML navigation                |

### Response-header verdicts, all blocking

- **No fronting cache.** A positive `Age` or a vendor hit header means a cache answered for the
  origin, so every axis may have compared one stored object with itself and the whole run says
  nothing. Judged first. Detected rather than defeated: cache-busting would change either the
  cache key or the origin's own caching behaviour, and perturbing the measurement to rescue it
  would make a green result mean less.
- **Positive shared freshness.** No positive `Cache-Control`/`Surrogate-Control` freshness means
  readthrough must not be enabled.
- **No `Set-Cookie`.** Per the response-side gap, this is the session-fixation vector and there is no runtime guard.
- **No CSP `nonce`.** The template cache refuses these (`publisher.rs:6117`) because a shared
  nonce silently defeats the origin's own XSS defence; readthrough cannot.
- **`Vary` coverage.** Report which varying axes the origin's declared `Vary` fails to cover.

`--repeat N` runs the self-identity check N times.

### Stated limits

The probe must print, and the docs must repeat, what it cannot see: it runs from one client IP,
so personalization keyed on the forwarded client address (geo, rate-class) is undetectable. And
a verdict covers the sampled URLs only, not the origin as a whole.

### Implementation notes

`crates/trusted-server-cli/src/commands/origin/`, new `Origin` variant in `run.rs`'s `Command`
enum, following the `audit`/`dev` pattern. Human-readable output by default, `--json` for CI,
and a non-zero exit on any blocking failure so it can gate a deploy.

**The CLI has no HTTP client on Linux.** `hyper`, `rustls`, `tokio` with `net` are under
`[target.'cfg(target_os = "macos")'.dependencies]` (`crates/trusted-server-cli/Cargo.toml:42`);
the non-wasm block has `tokio` without `net`. `chromiumoxide` drives a browser and cannot give
raw origin bytes. Add `reqwest` — already a workspace dependency with `rustls-tls`
(`Cargo.toml:93`), already built natively by the Axum adapter and the integration-tests crate —
to the `cfg(not(target_arch = "wasm32"))` block, which exists precisely to keep the
`wasm32-wasip1` default from building native networking.

**The fixture server is its own task.** The only precedent,
`crates/trusted-server-cli/tests/support/mod.rs`, is reachable only from `tests/proxy_e2e.rs`,
which is `#![cfg(target_os = "macos")]` for that same dependency reason — and CI runs the CLI
suite on Linux too. It must loop-accept: a single-accept fixture already caused a CI flake here
(fixed in PR #823), and `--repeat N` opens N connections by design.

---

## Purge

### Admin endpoint and key plumbing

```
POST /_ts/admin/cache/purge
Content-Type: application/json

{"scope": "all"}  |  {"scope": "url", "url": "https://example.com/page"}
```

Registered in the Fastly `NamedRoute` table (`adapter-fastly/src/app.rs:1128`), inheriting the
`^/_ts/admin` basic-auth middleware — which runs before route matching (`app.rs:1305` →
`core/src/auth.rs:79`) and fails closed when no handler regex covers the path
(`auth.rs:93-100`).

**Reader-facing surrogate key.** `TemplateCacheKey.url` is the origin-rewritten target URI
(`publisher.rs:4372`, built at `:4182`), not the URL an operator types. Rather than have callers
replay origin rewriting — the seam that rots silently, since a wrong input yields a well-formed
key that purges nothing — add a **third** surrogate key derived from the reader-facing request.

Correcting revision 2: it claimed `request_scheme` + `request_host` + path were "all already
fields" on `TemplateCacheKey`. There is **no path field** (`platform/template_cache.rs:53-59`
has `url`, `request_host`, `request_scheme`). Add `request_path`, populated pre-rewrite. Without
it the "reader-facing" key would be reconstructed from the origin path, reintroducing the
coupling it exists to remove.

Both the endpoint and the CLI then hash the same reader-facing string, and neither needs origin
logic.

**Canonicalization is required, not optional.** `url_surrogate_key` is a raw SHA-256 over exact
bytes (`template_cache.rs:147`) and `punctuation_distinct_urls_have_distinct_surrogate_keys`
(`:961`) makes byte-exactness load-bearing. Trailing slash, default port, host case, percent
encoding, and query string must each have a stated rule, implemented once in the extracted free
function `url_surrogate_key(url: &str) -> String` and shared by insert and purge. Every mismatch
is a silent no-op purge — a 200 response and an uninvalidated object — in the mechanism rollback relies
on. Test round-trip from an operator-typed string to the stored key in both
directions.

Give the reader-facing key a distinct prefix (`ts-template-readerurl-`), asserted distinct from
`ts-template-url-` in the existing surrogate-key test, so the two derivations cannot alias when
a staging edge host equals the configured origin host. The failure mode would be over-purge
rather than a read leak — `to_cache_key` still includes scheme, host, and origin identity
(`template_cache.rs:95-105`) — but an aliased purge reports success against an unrelated object.

**Trait change.** `PlatformTemplateCache::purge_url` takes `&TemplateCacheKey`
(`platform/template_cache.rs:675`), which a handler holding only a URL cannot construct. Add
`purge_url_surrogate_key(&self, key: &str)` across all five implementors:
`UnavailableTemplateCache` (`template_cache.rs:698`), `adapter-fastly/src/template_cache.rs:163`,
`adapter-fastly/src/app.rs:2840`, and the two test doubles at `publisher.rs:8843` and `:9112`.

**Guards**, each with a specific reason:

- Register the path as a **string literal** in `NAMED_ROUTES`. The
  `admin_endpoints_match_fastly_router` check (`settings.rs:7727-7741`) scans literal `path:`
  entries only; a named constant silently skips coverage.
- Add the path to `Settings::ADMIN_ENDPOINTS` (`settings.rs:3214`). That constant feeds live
  config validation at `:3265`, so an operator `trusted-server.toml` whose handler regexes do
  not cover the new path will start failing validation — a migration note for the release.
- **Register for all methods and return 405 in-handler.** Revision 2 asserted "`GET` rejected";
  the router does not do that. Non-primary methods on a named path fall through to the publisher
  (`app.rs:42`), and `enforce_basic_auth` deliberately leaves the `Authorization` header in place
  so it "still reaches the publisher origin" (`auth.rs:60-66`). A `GET` would therefore
  authenticate, fall through, and ship the shared admin credential to the publisher backend.
  Follow the `/auction` `OPTIONS` precedent (`app.rs:1209-1213`) and test that a non-POST does
  not reach the origin. This credential-forwarding behavior is pre-existing for every `/_ts/admin`
  route and deserves its own issue — successor issue E.
- No non-`/_ts` alias, ever. The legacy `/admin/keys/*` aliases bypassed the auth regex and had
  to be denied locally (`app.rs:1168`).
- Reject any `Content-Type` that is not exactly `application/json`. Basic-auth credentials are
  attached automatically by browsers, so a cross-origin form POST with `enctype="text/plain"`
  sends no preflight; method alone does not stop CSRF, enforced content type does.
- Cap the request body. The `url` field is attacker-supplied and only ever hashed.
- `{"scope":"all"}` is privileged: audit-log the authenticated principal, and rate-limit it or
  record the accepted risk explicitly. The spec calls it an unbounded cache-flush and
  origin-stampede lever behind one shared static credential, and there is no HTTP-handler
  rate-limit primitive in this repo (`ec/rate_limiter.rs` is partner batch/pull-sync only).
- Replay is not a concern — purge is idempotent. Say so rather than leaving it unaddressed.
- Define the partial-failure answer. `purge_all` returns a single `Result` from one
  `purge_surrogate_key` call (`adapter-fastly/src/template_cache.rs:285`); an operator mid-incident
  needs to know whether to retry.
- Response is `private, no-store`.
- Non-Fastly adapters return 501, not 404 or 200.

**Four route tables, not one.** Fastly (`adapter-fastly/src/app.rs:1128`), Axum (`app.rs:310`,
the fixed `[NamedRoute; 16]` array becomes 17, plus the entry), Cloudflare (`app.rs:515-545`),
and Spin (the route list at `app.rs:211-215` and the router at `:854-861`). An unregistered path
falls through to origin and 404s, which reads to a CMS webhook as "endpoint does not exist"
rather than "not supported here".

### Purge CLI command

```
ts cache purge --all
ts cache purge --url <url>
```

New `Cache` variant in `run.rs`'s `Command` enum, `crates/trusted-server-cli/src/commands/cache/`.
With the reader-facing key and canonicalization landed with the endpoint, this is a thin wrapper: hash the
typed URL, call the Fastly purge API. No config load, no origin logic.

**Open dependency, and why this ships after the endpoint.**
`adapter-fastly/src/management_api.rs:12` records that today's token is write-scoped with no
Purge permission, and `edgezero_cli` exposes no credential helper — only whole-command runners.
Read `FASTLY_API_TOKEN` with a `--token` override, document the required scope, fail with an
actionable message when it is missing. If the scope cannot be granted, the endpoint stands alone and this
PR is dropped.

### What purge does not cover

Template-cache objects are purgeable today. Readthrough objects are purgeable only if the staging check in rollback confirms `set_surrogate_key` behaves as documented. Until then the runbook says
readthrough rollback is flag-flip plus origin TTL. Do not ship a runbook whose rollback step
does not affect the cache being rolled back — revision 1 did, and this is the correction.

### Tests

- Fastly adapter: Purge-all and purge-url succeed; unauthenticated rejected; non-POST returns
  405 **and does not reach the origin**; wrong `Content-Type` rejected; oversized body rejected;
  no legacy alias resolves.
- Surrogate-key round-trip: operator-typed string → stored key, both directions, across the
  canonicalization cases.
- Parity suite: 501 on Cloudflare, Spin, Axum. **Needs new authenticated POST helpers** —
  `parity.rs:16-17` sets `^/_ts/admin` basic auth and the existing `axum_post`/`cf_post`/
  `spin_post` helpers send no credentials, so an unauthenticated probe returns 401 and never
  reaches the handler. `spin_post_with_headers` (`:200`) is a usable template.
- CLI: both flags produce the expected keys; missing token produces the actionable error.
- `scripts/template-cache-local-test.sh` gains a purge leg — store, hit, purge, confirm miss.
  Viceroy 0.17 implements `purge_surrogate_key` against the same in-process cache it serves reads
  from (`2026-08-08-1009-measurement-findings.md:156-158`), so this is end-to-end testable
  without a Fastly service. The script accepts only `inline|esi` today (`:17-18`) and CI invokes
  those literals, so a new mode needs a matching workflow step and a gate-list entry.

---

## Observability

### Current

Two response headers, `x-ts-template-cache` and `x-ts-assembly` (`publisher.rs:122`, `:149`).
A hit rate cannot be computed from a response header without a scraping harness. There is no
access log to extend — `tinybird.access_enabled` is explicitly rejected as unwired
(`settings.rs:1945`).

### Change

Carry outcomes on the auction telemetry rows, wired in `AuctionEventRow::base()`
(`auction/telemetry.rs:347`) so provider and bid rows carry them too rather than the summary
alone (`AuctionEventRow` is at `:277`), which
already flows to Tinybird with `publisher_domain` and `page_path`:

- ~~`template_cache_state`~~ — **dropped during implementation, and it cannot be added
  back without restructuring when telemetry is emitted.** On a cold fill the ordering is
  fixed: `stream_publisher_body_async` collects the auction, takes the observation and emits
  the batch, and only then does `store_template_if_authorized` run and the state get
  stamped. The store cannot move earlier (it needs the transform) and the emit cannot move
  later without giving up collecting during body streaming — which is a latency decision on
  the path this whole issue exists to improve. `hit` was reachable and `miss-stored` was
  not, so the column would have made hit rate compute as roughly 100%. The
  `x-ts-template-cache` header still carries all nine states per response.
- ~~`template_cache_bypass_reason`~~ — **moved to successor issue B during implementation.**
  It diagnoses the _template_ cache's refusals, which is #1009's feature, not this issue's.
  Readthrough has no refusal reasons TS controls — that is the response-side gap — so this
  column says nothing about the change #852 makes. Instrumenting a spike belongs with
  promoting it out of spike status. Deriving it also required new code: the variants that
  matter are structurally unreachable at the existing site, per the note below.
- `origin_cache_shareable: Option<bool>` — whether `origin_response_is_shareable` was true,
  i.e. whether the readthrough gate let this request use the readthrough cache

The third field exists because revision 1 instrumented only the template cache, leaving the
one change with real blast radius shipping with zero observability — no way to tell
"the readthrough gate is working" from "it is inert".

The bypass reason is the field that carries the triage. `TemplateCacheBypassReason` has
sixteen variants today (`publisher.rs:5673`), seventeen once the request-side derivation below
adds one: "cookie-disqualified" is an expected default, "no
positive freshness" is an origin configuration problem, "vary not covered" is a stale
`template_cache_vary` list, "malformed cache policy" is a bug. Without it a zero hit rate is
uninterpretable.

This is the spec's own trim, reached by the code rather than by the approval gate.

The scope line this settles: each issue instruments its own change. `origin_cache_shareable`
measures what #852 changes; the bypass reason measures what #1009 built.

**Retained for issue B — the reason has two sources, and only one of them exists today.** `template_cache_ttl`
(`publisher.rs:6129`) returns `Result<Duration, TemplateCacheBypassReason>`, but it runs inside
`template_cache_reservation.and_then(...)` (`:4775`), and a reservation exists only when
`template_cache_key` was built — which is `request_can_use_shared_template.then(...)` (`:4370`).
So its first three variants, `InlineMode`, `AuthorizedRequest` and `CookieForwarded`, are
structurally unreachable from it: those requests never get a key in the first place. Only
response-derived reasons can fire there.

The request-side bypass carries no reason value at all. It sets
`TemplateCacheResponseState::BypassRequest` (`:4386`) and writes free-text `log::debug!` lines
(`:4360-4369`), and nothing else.

That is a problem for whoever instruments this cache, because "cookie-disqualified" — the
expected default, and the single most important thing an operator needs to see — lives on the
unreachable side. Doing it means **deriving a structured request-side reason** alongside
`template_cache_key`, reusing the existing `TemplateCacheBypassReason` variants rather than
inventing a second vocabulary. That is new code, not a wiring exercise, and **issue B must
budget it** — it is no longer in this issue's scope.

The `#[cfg(test)]` helper named `template_cache_bypass_reason()` at `:5863` is not the hook for
either source.

### Schema migration — this is not free

Revision 1 claimed "no new CI gates". Wrong for this work.
`tinybird/datasources/auction_events_raw.datasource` enumerates all 35 columns explicitly,
and `to_ndjson` (`auction/telemetry.rs:422-425`) uses plain `serde_json::to_string` with no
`skip_serializing_if`, so new fields are always on the wire including as `null`. Rows with
undeclared columns go to Tinybird quarantine, which the repo already tracks
(`tinybird/pipes/quarantine_counts.pipe`).

Required: update the datasource, update `tinybird/fixtures/auction_events_raw.ndjson`, and
sequence the Tinybird deploy **before** the code deploy.

### Carrier

`AuctionObservationContext` (`auction/telemetry.rs:99-125`), the request-scoped
`auction_observation` at `publisher.rs:4446`, also a params field at `:1599`. Two notes the
plan must honor:

- The struct is currently an immutable snapshot built once from an `AuctionRequest`. These
  fields make it a mutable accumulator, because the bypass reason is only known post-fetch.
  Add them via a setter, not `pub` mutation.
- There are **five** write points, not one. The hit path moves the observation out and returns
  at `publisher.rs:4669` before `template_cache_ttl` is reached at `:4776`; the `Hit` state is
  stamped separately at `:2202` — which is unreachable from the publisher path, so the usable
  hook is the `TemplateCacheLookup::Hit` arm at `:4617`; the abandon paths at `:4726` and `:4748`
  also `take()` early, while `:4957` and `:4996` take _after_ the state write and do carry it.
- There are two bindings, and which one a write site holds decides the mechanism.
  `observation` is a plain value built at `:4461`; `auction_observation` is the
  `Option<AuctionObservationContext>` at `:4446`, and `observation` is moved into it at `:4511`.
  A write before `:4511` sets the value directly; a write after it goes through
  `auction_observation.as_mut()`. An earlier revision of this section claimed the value had to be
  stashed in a local because the binding did not yet exist — that is wrong, and the plan overrules
  it: `origin_cache_shareable` is known at `:4325` and construction is at `:4461`, so a setter
  immediately after construction works.

### Known gaps, to be stated in the dashboard docs

- The summary row is emitted only when an auction runs, so the denominator is "ad-serving
  pageviews", not "all requests". A request that bypasses because the ad stack did not run
  produces no row.
- `AuctionObservationContext` is `Clone` and shared with the `/auction` source, where these
  fields are structurally `None`. A dashboard reading `None` as "miss" will be wrong for a
  whole source class.

---

## Configuration, documentation, tests

### Configuration

No new keys. Doc comments change on `origin_is_cookie_independent` — replace "Unsafe unless
independently verified" with a pointer to `ts origin probe-shareability` and a one-line
statement of what the probe does and does not prove — and on the matching Rust doc comments
on `CreativeOpportunitiesConfig`.

### Documentation

`docs/guide/configuration.md` gains the three-cache glossary once, with code comments
pointing here rather than re-teaching it, plus an operator runbook: run the probe → require
a green freshness verdict → set the flag on evidence → watch the bypass-reason breakdown →
confirm hit rate → purge and roll back. The rollback section must state exactly what purge
covers, per purge coverage above.

Docs edits hit the `cd docs && npm run format` gate. Use the pinned `docs/node_modules`
prettier, not `npx` — the npx version reports false format failures.

Each item's docs land in that item's PR, not as a lump, so no PR documents a command
that does not exist yet.

### CI gate list

`AGENTS.md`'s documented gate list is incomplete in more ways than this spec first counted, and
fixing one omission while leaving the rest is how it stayed wrong. Known missing today: the
`scripts/template-cache-local-test.sh` harness (`test.yml:63`, `:66`), two clippy invocations in
`format.yml:83`/`:88` (CLI and `trusted-server-openrtb-codegen`, both hardcoded to
`x86_64-unknown-linux-gnu`), the parity crate's clippy (`test.yml:200`), the openrtb-codegen test
(`test.yml:194`), the integration-tests `cargo fmt` check, `npm run lint` for JS and docs plus
the docs `npm run build` (`format.yml:116`, `:147`, `:153`), the html-processor bench smoke, the
Fastly and Spin release WASM builds, and the whole of `.github/workflows/integration-tests.yml`.

Rather than enumerate a list that will drift again, state in AGENTS.md that the list is the
commonly-run subset and point at `.github/workflows/` as authoritative — then add the entries
above, including the new purge-mode harness step this spec introduces.

**Parity lockfile.** The integration-tests crate has its own lockfile with a shared-direct-dep
alignment gate. If the new authenticated POST helper pulls a dependency, fix with a targeted
`cargo update -p <crate> --precise <version>`, never a full update.

---

## Sequencing

**One PR, by decision.** An earlier revision split this into five. The work is now a single
change set, so the ordering below is commit order within one branch rather than a merge order.

What that costs, recorded rather than glossed: the readthrough gate is the only change here with
new blast radius, and in a single PR it lands and reverts together with the telemetry that would
tell you whether to revert it. A revert takes the instrumentation with it. The mitigations are
that the gate is inert until an operator sets `origin_is_cookie_independent`, and that
`assembly_mode` stays a runtime kill switch — so the practical rollback is a config change, not a
code revert.

Commit order, which still follows the rule that nothing changing cache behavior precedes the
tooling to observe and reverse it:

1. **Predicate split** — pure refactor, no behavior change. First because everything else
   references the binding it creates.
2. **Observability** — the `origin_cache_shareable` telemetry field and the Tinybird datasource
   migration. (Two further fields were scoped out during implementation: see Observability.) The migration must reach Tinybird **before the code
   deploys**, which in a single PR is a deploy-ordering constraint on the release, not on the
   merge.
3. **Probe** — the `reqwest` dependency, the loop-accept fixture server, five axes and five
   response-header verdicts.
4. **Purge plumbing and endpoint** — the `request_path` field, the reader-facing surrogate key
   with canonicalization, the `url_surrogate_key` extraction, the trait change, four route
   registrations, `ADMIN_ENDPOINTS`, parity auth helpers, the harness purge leg and its workflow
   step.
5. **Purge CLI command** — thin wrapper over the key work above. Drop this commit alone if
   purge-token scope cannot be granted; nothing else depends on it.
6. **Readthrough gate** — last, and reviewed as security-sensitive. Requires the `ts-origin`
   staging verdict resolved and the precondition list reflected in the runbook before the PR is
   marked ready.
7. **Documentation** — runbook, glossary, config comments, dashboard caveats, CI gate list.

**The `ts-origin` staging check gates the PR, not a commit.** It cannot be verified under Viceroy
and needs a staging service. Timebox it alongside commit 4, which already touches the purge trait,
and write the verdict into the PR description. If it comes back negative, the runbook says
readthrough rollback is flag-flip plus origin TTL — the PR still ships, with an honest rollback
section.

**Review guidance for a change set this size.** Ask for the readthrough commit to be reviewed on
its own, against the precondition list. It is one condition at two call sites, and the rest of the
diff is instrumentation and tooling that will otherwise bury it.

## Successor issues

Five things are deliberately not in #852. None is in the issue text, and each hides work that
deserves its own review:

- **Issue A — finish the 2026-08-19 terminology migration.** Six active code comments still use
  the retired C1/C2/C3 labels (`platform/template_cache.rs:5-11`, `publisher.rs:1727`, `:2154`,
  `:5860`, `:11025`, `response_privacy.rs:71`), and four shipped #1009 design docs still read
  "Approved for implementation". Editorial; the distinctions those comments draw must survive
  verbatim in substance.
- **Issue B — promote the template cache out of spike status, and instrument it.** Carries
  the `template_cache_bypass_reason` telemetry column moved out of #852: it diagnoses the
  template cache's refusals, and the variants that matter (`InlineMode`, `AuthorizedRequest`,
  `CookieForwarded`) are structurally unreachable from `template_cache_ttl`, so it needs a
  request-side derivation alongside `template_cache_key`. Note a template-cache hit/miss
  column is **not** available — see the Observability section for why. 30 comment sites across six
  files, but not editorial: three unsettled design decisions sit underneath. The "spike-grade
  choice, not a production one" `Vary`-keying caveat (`platform/template_cache.rs:230`), whose
  drift guard runs only on the cold path — a hit returns before the origin fetch, so a stored
  template never learns the origin added a `Vary`, and correctness is bounded by TTL rather
  than by a check. The `STRUCTURALLY_COVERED` operator obligation (`:207`), which the probe's
  probe now makes checkable. And `VarySpec::new`'s panic (`:240`) on a Wasm request path. Gate
  on first probe results.
- **Issue C — validate readthrough and template hit rates on a production origin.** Metric:
  `origin_cache_shareable` true-rate and `template_cache_state = hit` rate from the observability
  fields, per `page_path`. Window: seven days after enablement. Qualifying deployment: one whose
  probe returned green on every blocking axis. Triggers: a sustained hit rate below a threshold
  agreed at enablement means the readthrough gate is not earning its risk and the flag goes back off;
  any cross-serving report is an immediate rollback and a redesign of the gate.
- **Issue D — safe TTL and stale-while-revalidate control.** #852 item 1 asks for both. They are
  buildable (`set_ttl`, `set_stale_while_revalidate`) but carry override-the-origin semantics
  that make them unsafe as request-side knobs, per the mechanism above. A safe version needs a post-response
  decision, which means the Core Cache API and its own review.
- **Issue E — `/_ts/admin` forwards the admin credential to the publisher origin.** Non-primary
  methods on named admin paths fall through to the publisher (`app.rs:42`) and
  `enforce_basic_auth` leaves the `Authorization` header in place by design (`auth.rs:60-66`).
  Pre-existing for every admin route, not introduced here; the admin endpoint works around it locally
  with an all-methods registration.

Note when filing: `gh issue create --label task` fails in this repo — issue type is GraphQL-only.

## Open risks

**Readthrough safety rests on preconditions, not code.** As set out above, none of the template cache's
response-side refusals can be applied to the readthrough path, because the decision is made
before the origin responds and no post-response hook is reachable. The probe's blocking verdicts
are the only control. An operator who enables readthrough against an unverified origin can
cross-serve, including session fixation via a cached `Set-Cookie`.

**Decided: the readthrough change ships.** This paragraph previously left it open. The
alternative was to close
#852 item 1 as won't-do and keep the unconditional bypass, accepting that every ad-serving
pageview pays a full origin round trip — legitimate, because the template cache already delivers
the same benefit on its hit path and enforces response-side rules readthrough cannot. Its marginal
value is confined to template-cache misses; its marginal risk is a weaker guarantee on a broader
population.

It ships anyway, on the understanding that the probe's blocking verdicts are the control, and that
enablement stays per-operator behind `origin_is_cookie_independent` so nothing changes for anyone
who does not opt in. Review the readthrough commit against that bar specifically.

**The gate is a no-op for cookie-varying origins.** A publisher whose HTML genuinely depends on
publisher cookies gets nothing from this work. The probe tells them quickly, which is the honest
outcome, but the addressable population is unknown until operators run it.

**Origin readthrough's efficacy depends on origin headers we do not control.** #852's premise is that
origins mark HTML private. Where that holds, readthrough stores nothing even with the flag flipped.
That fails safe and is the correct behavior, but it must not be sold as a guaranteed latency win.

**Template cache and streaming pull in opposite directions.** `TemplateEntry.body` is `Vec<u8>`
(`platform/template_cache.rs:687`) and inserts take an owned body, so the template-cache path
cannot stream and `MAX_PLATFORM_RESPONSE_BODY_BYTES` applies (`adapter-fastly/src/platform.rs:425`).
Relevant to issue B's promotion decision and to the streaming work; noted so the interaction is
not rediscovered later.

**Observability is not in #852's text, and shipped in its trimmed form.** _Resolved — this is no
longer an open approval question._ As drafted it was the largest refactor here: three fields, a
mutable accumulator, and a 35-column migration with quarantine risk, close enough to AGENTS.md's
"no large refactors without approval" to need asking. What shipped is the fallback this section
already named — `origin_cache_shareable` alone, one field, one setter, one nullable column.
`template_cache_state` proved structurally unreachable and `template_cache_bypass_reason` was
scoped out to issue B as a template-cache diagnostic rather than a #852 one.

The one field earns its place on relevance: it _is_ `origin_response_is_shareable`, the predicate
#852 introduces, not adjacent instrumentation. It also has to land ahead of the gate rather than
with it — its purpose is to answer "how much traffic would the gate admit" before anyone flips it,
and shipping it alongside the gate would destroy that baseline. Given the response-side gap leaves
an operator's config flag and a one-off probe run as the only runtime controls, going into the
first production window blind is the worse trade.

## What closes #852

All five work items landed, and specifically:

- The rollback staging verdict recorded, with the runbook matching it.
- Probe green against the harness fixture origin on all five axes and all five response-header
  verdicts.
- `origin_cache_shareable` confirmed present on Tinybird rows from a staging deploy, with no
  quarantine.
- The readthrough gate reviewed as its own commit against the precondition list, not as part of
  the wider diff.

Production hit-rate validation is **issue C**, not a condition of this one. Revision 1 required a
recorded measurement from a real deployment, which makes the issue un-closeable by the engineer
who builds it — it depends on an operator with a suitable origin volunteering.
Validation-by-deployment is different work and deserves its own issue and assignee.
