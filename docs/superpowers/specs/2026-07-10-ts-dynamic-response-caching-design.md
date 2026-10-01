# Design TS-owned dynamic response caching

**Issue:** #859  
**Status:** Draft  
**Area:** Trusted Server runtime / publisher origin / Fastly cache

## Summary

Trusted Server should cache reusable, anonymous publisher-origin HTML templates
without ever shared-caching the final server-side ad template (SSAT) response.
The origin template is fetched through a canonical, cookie-free request, stored
under an explicit versioned key, and passed through the existing HTML rewrite and
per-request auction assembly pipeline on every request.

The first implementation is Fastly-only and disabled by default. It uses
Fastly's HTTP readthrough cache so complete origin responses retain their normal
HTTP metadata while Fastly provides request collapsing, revalidation,
stale-while-revalidate (SWR), and surrogate-key purging. Axum, Cloudflare, and
Spin keep the current uncached publisher-origin behavior.

This design intentionally excludes transformed-template caching and dynamic
React Server Component (RSC) or API caching. Those response families have
additional configuration and cardinality requirements and remain later phases.

## Context and scope split

The initial cache-header work in
`docs/superpowers/specs/2026-07-06-cache-control-header-design.md` controls how
browsers and shared caches treat TS-owned/static responses. It does not create a
TS-owned dynamic object cache.

The adjacent work remains separate:

- #857 covers parser-safe SSAT late binding and publisher streaming.
- #858 covers optional SSAT compression offload.
- #859 covers the dynamic origin/template cache described here.

The order matters. The origin-template cache sits before HTML transformation and
personalization. Parser-safe late binding and response privacy continue to run
after a template hit exactly as they do after an origin fetch.

## Production-audit evidence

A read-only audit of a representative production publisher used a stock headed
browser and a valid bot-management session. The site was a Next.js App Router
frontend backed by a headless CMS. Findings are intentionally anonymized.

### HTML size and existing cache behavior

| Response family | Decoded size observed | Compressed size observed | Existing upstream result |
| --------------- | --------------------: | -----------------------: | ------------------------ |
| Homepage HTML   |         about 1.4 MiB |            about 235 KiB | Shared-cache hit         |
| Section HTML    |         about 720 KiB |            about 145 KiB | Shared-cache hit         |
| Article HTML    |         about 635 KiB |            about 100 KiB | Shared-cache hit         |

The publisher already cached HTML upstream. A TS origin-template cache would
still remove the upstream network round trip, but its origin-load reduction may
be smaller for publishers that already have an effective origin CDN. For those
publishers, transformed-template caching may eventually provide the larger CPU
benefit.

### Cookie behavior

An anonymous browser accumulated many first-party advertising, analytics,
identity, geography, and experiment cookies. Bypassing every request that has a
`Cookie` header would therefore eliminate most useful cache hits.

Visible article content remained stable across two anonymous cookie states, but
embedded App Router data and related-content modules were not byte-identical.
The audit also observed per-request bot, geography, and experiment cookies on
responses that otherwise came from a shared cache. This demonstrates that a
cacheable body and per-request response side effects can exist at different edge
layers.

Consequences:

- a cacheable template request must deliberately request an anonymous
  representation instead of forwarding arbitrary cookies;
- TS must not cache or replay origin `Set-Cookie` fields in phase one;
- bot-management and other per-request edge behavior must run outside the
  template-cache boundary or the origin connection must be service-authenticated.

### Query behavior

Adding a non-content tracking query created a separate upstream cache object and
the query appeared in the returned App Router representation. Removing such a
query from only the cache key would therefore collapse byte-distinct responses.
Phase one bypasses every query-bearing request.

### `Vary` and RSC behavior

HTML responses varied on content encoding, an experiment dimension, and several
router-state headers. Initial document responses embedded substantial flight
data rather than fetching it separately.

A client-side navigation produced an RSC body of roughly 220-260 KiB decoded
and 35-40 KiB compressed. Repeating the same source-route to target-route
navigation produced the same deterministic `_rsc` query, byte-identical body,
and a cache hit. Navigating to the same target from a different source route
produced a different `_rsc` query and different body.

A practical RSC key can therefore include at least:

```text
target route × source router state × experiment × encoding
```

This evidence supports deferring RSC caching until key cardinality, body impact,
and hit ratio are measured for each configured response family.

### Audit limitations

The audit is evidence, not proof that any route is universally anonymous. It did
not cover authenticated accounts, preview mode, every geography, every
experiment assignment, or the publisher's private origin endpoint. Enabling a
template rule remains an explicit operator assertion that its canonical origin
request is safe.

## Terminology and cache boundaries

### Origin template

The unmodified response returned by the publisher origin for a canonical
anonymous request. Phase one caches this object.

An origin template may be compressed. It includes only origin response metadata
that is safe to reuse. It never includes response cookies.

### Transformed template

Origin HTML after auction-independent Trusted Server rewriting and head
injection, but before per-user slot state and bids are bound. This is deferred.

### Assembled SSAT response

The browser-facing HTML after route-specific slot state, per-request auction
results, consent-dependent behavior, and other response finalization. This
response remains private and is never entered into a shared cache.

### Dynamic response family

A configured class of non-HTML dynamic responses, such as selected RSC or JSON
API responses. Dynamic response families are deferred until their key
cardinality and body impact are measured.

## Goals

- Cache anonymous origin HTML templates for explicitly configured routes.
- Keep personalized SSAT output `private, max-age=0` and outside shared caches.
- Build template requests from a canonical, cookie-free request profile.
- Use a deterministic, versioned, collision-resistant 32-byte Fastly cache key.
- Treat query strings, authorization, ranges, prefetches, and configured
  personalization signals as bypass conditions.
- Require every origin `Vary` dimension to be canonicalized or included in the
  explicit bounded key policy.
- Admit only safe statuses, content types, encodings, headers, and bounded bodies.
- Use Fastly request collapsing, TTL, SWR, revalidation, and surrogate keys.
- Define a generic purge contract suitable for CMS webhooks and operator tools.
- Make unsupported runtimes retain the current request and origin behavior.
- Expose bounded, privacy-safe hit, miss, stale, rejection, and bypass
  observability.
- Provide an explicit kill switch through disabled-by-default rules.

## Non-goals

- Shared-caching final SSAT HTML.
- Transformed-template caching in phase one.
- Dynamic RSC or API caching in phase one.
- Query normalization or ignored query parameters in phase one.
- Response-cookie stripping or replay in phase one.
- Authenticated, preview, paywall, or account-specific templates.
- Arbitrary raw request-header values as unbounded cache-key dimensions.
- Hard-coded Next.js router-header behavior in core or adapters.
- Cloudflare, Axum, or Spin dynamic cache implementations in phase one.
- Akamai support.
- Replacing the existing static/fingerprinted asset cache rules.
- Changing publisher streaming or compression-offload scope.
- Inferring CMS content identifiers by parsing HTML.

## Privacy and correctness invariants

The following invariants are non-negotiable:

1. Only the origin response from a canonical anonymous request may enter the
   template cache.
2. Client cookies, authorization, EC identifiers, consent strings, client IP
   headers, and auction data never enter a template key or cached body.
3. A request carrying a configured login, preview, or personalization signal
   bypasses template mode before request canonicalization.
4. An origin response carrying `Set-Cookie`, `private`, or `no-store` is not
   stored.
5. Unknown or unaccounted `Vary` dimensions prevent storage.
6. Variant values are bounded by configuration; unlisted values bypass rather
   than create arbitrary keys.
7. Cache hits still run the existing integration rewrite, auction, late-binding,
   response-header finalization, and response-privacy pipeline.
8. SSAT HTML remains `Cache-Control: private, max-age=0` and strips shared-cache
   control headers regardless of template cache outcome.
9. Unsupported runtimes do not canonicalize the request and do not pretend to
   cache it.
10. Cache or purge diagnostics never log cookie values, authorization values,
    consent strings, EC IDs, full query strings, or cached bodies.

## Threat model

### Cross-user response leakage

**Risk:** A personalized origin response is stored and replayed to another user.

**Controls:** Canonical cookie-free request construction, authorization bypass,
configured personalization bypasses, bounded variants, strict response
admission, and final SSAT privacy.

### Cache poisoning

**Risk:** A client supplies a host, header, path representation, or variant value
that collides with another representation.

**Controls:** Trusted host/scheme extraction, exact path framing, normalized
variant values, explicit allowed values, versioned length-delimited key
material, and SHA-256.

### Cache-key cardinality denial of service

**Risk:** Arbitrary query strings, cookies, router state, or headers create
unbounded cache objects.

**Controls:** Query bypass, no cookie keying, no raw arbitrary header keying,
allowed-value variants, per-rule body limits, and deferred RSC/API support.

### Response-header replay

**Risk:** A cached origin response replays cookies, hop-by-hop metadata, or stale
payload validators after HTML transformation.

**Controls:** Reject `Set-Cookie`, rely on Fastly's HTTP cache framing rules, and
normalize transformed-response headers before browser delivery.

### Stale content after CMS publication

**Risk:** TTL/SWR serves an old article, homepage, or section after a CMS update.

**Controls:** Explicit TTL/SWR, path and group surrogate keys, authenticated
soft/hard purge hooks, and a site-wide purge fallback.

### Purge abuse

**Risk:** An unauthenticated or oversized purge request evicts large portions of
the cache.

**Controls:** Existing admin authentication, strict request limits, canonical
path validation, configured group names, Fastly-only capability checks, and
rate limiting at the administrative boundary.

## Proposed configuration model

Template rules extend the existing `[cache]` settings while remaining separate
from `cache.asset_rules`.

Illustrative configuration:

```toml
[cache.template_purge]
allow_hard_purge = false

[[cache.template_rules]]
id = "anonymous-html"
enabled = false
anonymous_only = true
path_globs = ["/", "/articles/**"]
ttl_seconds = 60
stale_while_revalidate_seconds = 300
max_object_bytes = 2097152
bypass_cookie_names = ["example_session", "example_preview"]
bypass_cookie_prefixes = ["example_auth_"]
surrogate_groups = ["home", "articles"]

[[cache.template_rules.header_rules]]
name = "x-example-router-state"
action = "bypass-if-present"

[[cache.template_rules.header_rules]]
name = "x-example-experiment"
action = "vary"
allowed_values = ["control", "treatment"]
```

All examples are fictional. Real route, cookie, header, and CMS values remain in
operator-owned configuration.

### Rule matching

Template rules use the existing matcher vocabulary where practical:

- `path_prefix`
- `path_glob`
- `path_globs`
- `path_regex`

An enabled rule configures exactly one matcher family. Rules are ordered and the
first enabled path match wins. If that rule subsequently bypasses because of
request state, matching does not fall through to a later, potentially weaker
rule.

Phase one has no built-in framework preset. A framework preset may be added only
later as a convenience that expands into the same generic rule model.

### Required fields

An enabled rule requires:

- a unique diagnostic-safe `id` matching
  `[a-z0-9][a-z0-9_-]{0,63}`;
- `anonymous_only = true` as an explicit operator assertion;
- exactly one path matcher family;
- a positive `ttl_seconds`;
- a nonzero `max_object_bytes` within the Fastly/platform response limit.

`stale_while_revalidate_seconds` is optional and defaults to zero. There is no
hidden nonzero TTL or SWR default. `max_object_bytes` caps the actual encoded
body written into the internal cache; the existing decoded/processed HTML caps
remain separate.

### Configuration bounds

Configuration validation enforces phase-one cardinality limits before serving
traffic:

- at most 16 header rules per template rule;
- no duplicate header-rule names after case-insensitive normalization;
- no duplicate allowed values within one `vary` rule;
- at most 4 `vary` header rules;
- at most 16 allowed values per `vary` rule;
- at most 64 total Cartesian variant combinations, including an allowed missing
  value;
- at most 128 bytes per fixed or allowed header value;
- at most 64 exact cookie names and 64 cookie prefixes;
- diagnostic IDs and surrogate group slugs no longer than 64 bytes.

The Cartesian limit is the product of each `vary` rule's allowed values plus its
optional missing state. Several individually small lists cannot combine into an
unbounded attacker-selected keyspace.

### Cookie bypass fields

Before removing the `Cookie` header, core inspects every `Cookie` field and
checks:

- `bypass_cookie_names`: exact, case-sensitive cookie-name matches;
- `bypass_cookie_prefixes`: case-sensitive cookie-name prefixes.

Cookie-name inspection is stricter than ordinary application cookie extraction.
Invalid header bytes, malformed cookie pairs, duplicate cookie names, or any
other ambiguous syntax bypass template mode with the original request. A
configured bypass cookie in any repeated `Cookie` field must be found; the
implementation must not use only the first field.

A match bypasses template mode to preserve authenticated, preview, paywall, or
otherwise personalized origin behavior. If every field is valid and no name
matches, every `Cookie` field is removed from the canonical origin request.
Unknown cookies are therefore stripped, not keyed.

Enabling a rule asserts that serving the canonical anonymous representation to a
request with an unknown cookie is acceptable. The safe failure is anonymous
content, not another user's personalized content. Operators that cannot make
that assertion must leave the rule disabled.

Cookie values are never logged, hashed into the key, or retained for cache
processing.

### Header rules

Header rules are generic and case-insensitive by header name. Phase one supports
these actions:

| Action              | Request behavior                                               | Key behavior                               |
| ------------------- | -------------------------------------------------------------- | ------------------------------------------ |
| `bypass-if-present` | Bypass if any field with this name is present                  | Eligible requests have one absent variant  |
| `remove`            | Remove every field with this name                              | One canonical absent variant               |
| `fixed`             | Replace every field with one configured fixed value            | Fixed value is in the policy fingerprint   |
| `vary`              | Forward one configured allowed scalar; bypass all other shapes | Canonical allowed value is framed into key |

A `fixed` rule requires one validated `value`. A `vary` rule requires a finite,
non-empty `allowed_values` list and may set `allow_missing = true`. Missing is a
distinct framed value only when explicitly allowed.

Custom `vary` headers are singleton scalar fields in phase one. Eligibility
requires exactly one valid field, strips optional whitespace, rejects comma
lists and repeated fields, and matches configured values case-sensitively. The
canonical configured value—not an independently interpreted raw value—is used
both for origin forwarding and key construction. Invalid bytes, duplicates,
comma-combined values, missing disallowed values, and unlisted values bypass.

`Accept-Encoding` remains a built-in, grammar-aware exception. Multiple
`Accept-Encoding` fields bypass; one field is parsed by the existing encoding
normalizer and its normalized finite result is forwarded and keyed.

Phase one does not support unkeyed `forward` behavior for arbitrary
payload-affecting headers. A value is canonicalized, bounded and keyed, or it
bypasses.

Configuration cannot weaken built-in handling for `Cookie`, `Authorization`,
range headers, forwarding/client-IP headers, client cache-bypass directives, or
hop-by-hop headers.

### Surrogate groups

`surrogate_groups` assigns one or more operator-defined, low-cardinality groups
to every object stored by the rule. Group names use a conservative ASCII slug
format and have configured count and length limits.

Groups support cases where a single CMS update affects an article path plus a
homepage or section listing. Phase one does not inspect HTML or API payloads to
derive groups.

### Purge settings

`cache.template_purge.allow_hard_purge` defaults to `false`. It is independent
of rule enablement and does not grant access by itself; the purge endpoint still
requires the existing admin authentication. The setting only permits an already
authenticated caller to choose hard rather than soft purge.

## Request eligibility

The decision runs against the original client request before it is mutated for
the publisher origin.

An origin-template request is eligible only when all of the following hold:

- the adapter declares Fastly HTTP readthrough cache support;
- the method is `GET`;
- the request has no query string;
- the request is a document navigation according to the existing navigation
  classifier;
- one enabled template rule matches the exact request path;
- every `Authorization` field is absent;
- range and all `If-*` conditional headers are absent;
- client `Cache-Control`/`Pragma` does not request no-store, no-cache, or
  immediate revalidation;
- the request does not request `only-if-cached`, which has separate local
  handling described below;
- the request is not a prefetch according to existing request classification;
- every Cookie field is valid and no configured bypass cookie is present;
- every configured header rule accepts the request;
- every variant value is listed and bounded.

Phase one does not share `GET` and `HEAD` cache objects. `HEAD` bypasses until
its body and metadata semantics are designed and tested explicitly.

### Bypass order

The implementation records the first stable bypass reason from this sequence:

1. runtime unsupported;
2. feature/rule disabled or no path match;
3. method;
4. query present;
5. not a document navigation;
6. authorization;
7. range or conditional request;
8. `only-if-cached` local response;
9. client cache-bypass directive;
10. prefetch;
11. invalid/ambiguous cookie syntax;
12. configured cookie;
13. configured header;
14. unlisted or ambiguous variant.

Request cache directives are parsed across every repeated `Cache-Control` and
`Pragma` field. Directive names are case-insensitive and comma-list aware.
`no-cache`, `no-store`, `max-age=0`, and `Pragma: no-cache` bypass template mode
with the original request. Malformed or ambiguous syntax also bypasses.

Phase one does not implement cache-only template reads. Parsing checks every
field for an exact `only-if-cached` directive before applying any ordinary
cache-bypass directive. If present, it returns `504 Gateway Timeout` locally
without an origin dispatch even when combined with `no-cache`, `no-store`,
`max-age=0`, or malformed directives. It must not fall through to the ordinary
publisher proxy, which could contact origin contrary to the request directive.

Every other bypass executes the existing publisher-origin path with the original
request headers. It does not partially canonicalize an unsupported or
personalized request.

## Canonical anonymous origin request

Core constructs a new origin request for an eligible cache operation instead of
incrementally deleting fields from an arbitrary browser request.

### Canonical fields

The canonical request contains:

- method `GET`;
- the configured publisher-origin URI with the exact eligible path and no query;
- the configured origin `Host` behavior;
- trusted public host and scheme values where the publisher-origin contract
  requires them;
- `Accept` fixed to an HTML-capable value;
- the existing normalized supported `Accept-Encoding` representation;
- fixed or bounded variant headers accepted by the matched rule;
- no request body.

The public scheme and host participate in the cache key because they can affect
origin-generated absolute URLs and later rewrite context.

### Removed fields

The canonical request does not carry:

- `Cookie`;
- `Authorization` or proxy authorization;
- `Forwarded`, `X-Forwarded-For`, or client-IP headers;
- client hints or raw user-agent values unless represented by a future bounded
  variant policy;
- `Referer` or `Origin`;
- range, conditional, and client cache-bypass headers;
- prefetch/purpose headers;
- router-state headers classified as removed or bypass-only;
- hop-by-hop headers;
- internal edge signals not explicitly required by the platform adapter.

If a publisher requires device-specific, language-specific, geographic, or
experiment-specific server HTML, the operator must model that input as a
bounded variant or leave the route uncached. Client IP is not a supported
phase-one key dimension.

### Bot-management prerequisite

The canonical request has no end-user bot-management cookie. Before enabling a
rule, the operator must ensure the TS-to-origin connection uses one of:

- a private or allowlisted origin endpoint;
- service authentication independent of the browser;
- bot management outside the TS template-cache boundary.

A route that requires each visitor's bot cookie at the publisher origin is not
eligible.

## Cache-key design

### Primary key

Fastly accepts an exact 32-byte custom cache key. Core constructs canonical
binary key material and hashes it with SHA-256.

The key material includes:

1. object namespace, `origin-template`;
2. cache-key schema version;
3. stable publisher/site identity;
4. normalized publisher-origin identity, including host-override behavior;
5. matched rule ID and policy fingerprint;
6. trusted public scheme and normalized public host/port;
7. method representation (`GET` in phase one);
8. exact URI path bytes;
9. normalized origin `Accept-Encoding` value;
10. configured variant names and normalized values in sorted header-name order.

It never includes cookies, authorization, client IP, consent, EC IDs, auction
state, or query strings.

### Framing

Fields are not concatenated with delimiters. Each field is encoded as a type/name
identifier plus an explicit byte length and value. Lists include an item count,
and variant names are normalized before sorting.

Conceptually:

```text
magic | schema_version |
field_count |
  field_name_length | field_name | field_value_length | field_value |
  ...
```

The SHA-256 digest of this framed material is the 32-byte Fastly key. Tests use
adversarial values to prove that different field boundaries cannot collide
before hashing.

### Policy fingerprint

The policy fingerprint is computed during settings preparation, not by
serializing and hashing the entire settings object on every request. It includes
every setting that can change:

- eligibility and canonical request behavior;
- route matching semantics;
- variant normalization;
- response admission;
- TTL and SWR;
- body cap;
- surrogate groups;
- origin URL/Host behavior relevant to representation.

Changing one of these settings produces a new key namespace immediately. Old
objects expire naturally and remain purgeable through stable site/path keys.

### Path handling

Phase one uses the exact URI path representation supplied by the HTTP request
type after edge host/forwarding sanitization. It does not decode, reorder, or
collapse percent-encoded path forms. Treating equivalent paths as separate keys
may reduce hit ratio but cannot cross-contaminate representations.

### Encoding variants

Phase one retains the existing normalized supported `Accept-Encoding` behavior
and includes that normalized value in the key. This creates a small bounded set
of origin representation variants without refactoring the current
input/output-compression pipeline.

A future implementation may use one canonical stored encoding, but that is not
required for the origin-template phase.

## Fastly architecture

### Why HTTP readthrough cache

Fastly's HTTP cache API is preferred over the lower-level Core Cache API for
phase one because it already provides:

- complete HTTP response storage;
- custom 32-byte keys;
- request collapsing;
- stale revalidation and 304 update handling;
- SWR;
- response admission hooks;
- surrogate keys and soft/hard purge;
- streaming between backend and cache without inventing a response envelope.

Using Core Cache would require Trusted Server to define and version its own
status/header metadata envelope, insertion writer abstraction, revalidation
protocol, and cross-runtime body streaming contract. That flexibility is not
needed for the first Fastly-only implementation.

### Shared platform contract

`PlatformHttpRequest` gains optional, platform-neutral readthrough cache
metadata. The exact Rust names remain implementation details, but the contract
contains:

- 32-byte primary key;
- TTL;
- SWR;
- surrogate keys;
- maximum stored object bytes;
- allowed/canonical `Vary` disposition;
- response admission settings;
- rule ID or equivalent diagnostic label.

The Fastly `PlatformHttpClient` converts this metadata into Fastly request cache
options and an after-send candidate-response hook.

`PlatformResponse` also gains optional bounded cache-result metadata. The
phase-one response outcome is deliberately coarse: fresh hit, stale hit, stored
miss, or uncacheable miss, together with optional age and the diagnostic rule
ID. Cache/send failures have no `PlatformResponse` and are recorded from the
adapter's existing `Result` error path. Background SWR revalidation errors are
swallowed by the pinned SDK and cannot be reported individually. The metadata
does not claim that a particular stale request was selected for revalidation
unless the SDK exposes that fact reliably. Core does not infer
correctness-critical outcomes by parsing `X-Cache`. The Fastly adapter derives
this metadata before converting and discarding SDK-specific response state.

Axum, Cloudflare, and Spin do not receive cache metadata in phase one. The
publisher handler checks an explicit adapter capability before matching or
canonicalizing. If an unsupported adapter receives cache metadata because of a
programming error, it returns a typed unsupported-contract error rather than
silently pretending to cache.

This follows the existing explicit capability pattern used for delivery
compression.

### High-level request flow

```text
original client request
  -> derive request info, cookies, consent, EC, slots, and auction inputs
  -> match template rule and evaluate bypasses
  -> if bypassed:
       existing origin request path, unchanged
  -> if eligible:
       build canonical anonymous origin request
       build versioned 32-byte key and surrogate keys
       attach Fastly HTTP cache policy
  -> PlatformHttpClient::send
       Fastly cache lookup
         hit/stale -> reusable origin response
         miss      -> collapsed backend fetch
                      -> candidate response admission
                      -> store or reject
  -> existing response classification and HTML transformation
  -> existing parser-safe auction late binding
  -> transformed-response header normalization
  -> final response privacy
  -> browser
```

Cookie parsing and auction dispatch use the original request before the
canonical origin request is built. A template hit therefore does not remove
client state needed by the auction or EC lifecycle.

### Request collapsing

Fastly's HTTP cache transaction owns miss collapsing. One eligible request
fetches a missing key while concurrent requests for the same complete key wait
for response metadata and then consume the inserted object.

Collapsing must use the full 32-byte key. Requests with different site, path,
encoding, policy fingerprint, or bounded variants never collapse together.

If a non-304 response is rejected from storage, its body is not cached. The
after-send hook returns `Ok(())` after calling
`CandidateResponse::set_uncacheable(false)`; it does not return a hook error,
which would discard the fetched response. This abandons the transaction without
recording hit-for-pass metadata. Fastly may wake collapsed waiters serially for
a consistently rejected key; that fail-safe behavior is preferable to an
origin-age-dependent uncacheable marker.

## Response admission

The Fastly after-send hook evaluates origin response metadata before storage.
Admission is deterministic and side-effect free.

### Admission matrix

| Property                                      | Phase-one requirement                                                                     |
| --------------------------------------------- | ----------------------------------------------------------------------------------------- |
| Status                                        | `200 OK` for a new/replacement object; validated `304 Not Modified` only for revalidation |
| Method                                        | Eligible `GET` transaction                                                                |
| Content type                                  | Parsed HTML media type for a 200 response                                                 |
| Content encoding                              | Identity or an encoding supported by the existing HTML pipeline for a 200 response        |
| Body length                                   | Valid declared `Content-Length` for a 200 response, followed by an actual streaming cap   |
| `Set-Cookie`                                  | Must be absent                                                                            |
| `Cache-Control: private`                      | Reject                                                                                    |
| `Cache-Control: no-store`                     | Reject                                                                                    |
| `Cache-Control: no-cache` / `must-revalidate` | Reject in phase one                                                                       |
| `Vary: *`                                     | Reject                                                                                    |
| Other `Vary` fields                           | Every field must be canonicalized or represented by a bounded key rule                    |
| Partial/range metadata                        | Reject partial responses and `Content-Range`                                              |
| Payload                                       | Must remain below the existing decoded/processed response caps during later processing    |

An absent origin `Cache-Control` is not by itself a rejection. The explicit
operator rule supplies the internal template TTL after all safety checks pass.

Admission reads every header field. Scalar fields such as `Content-Type`,
`Content-Encoding`, and `Content-Length` must have one unambiguous valid value;
repeated/conflicting values reject. Every `Set-Cookie` field rejects, and
`Cache-Control`/`Vary` retain their defined multi-field comma-list parsing.

Phase one requires `Content-Length` as an early rejection check, but treats it
as advisory. For admitted 200 responses, the hook installs a counting no-op
body transform that copies the encoded backend body into the cache writer while
enforcing `max_object_bytes` against the actual bytes. This sacrifices
host-only insertion on misses but does not buffer the entire body in guest
memory.

If the actual body exceeds the cap or the copy fails, the transform returns an
error before calling `StreamingBody::finish()`. The current request fails; the
consumed origin response cannot also be recovered and served. The implementation
must prove in Fastly staging that an unfinished writer never becomes a reusable
partial cache object. If that guarantee cannot be demonstrated, phase one must
switch to a lower-level bounded insertion design rather than weakening the cap.

Supporting bounded chunked insertion without a declared length remains
deferred even though the counting transform could detect its eventual size.

### `Vary` validation

Origin `Vary` values are parsed across all header fields and comma-separated
tokens. Matching is case-insensitive and directive-exact.

A token is accounted for when the matched rule or a built-in invariant gives it
one of these dispositions:

- canonical fixed value;
- canonical absence/removal;
- bypass whenever present;
- finite allowed variant included in the key.

Unknown tokens reject storage. Phase one does not rewrite an unknown `Vary`
into a smaller set.

The Fastly HTTP cache may retain the validated origin vary rule as secondary
HTTP metadata. The TS primary key already contains every bounded forwarded
variant, so retaining the rule is redundant but safe and preserves normal HTTP
cache semantics.

### Revalidation

Fastly may revalidate a stale admitted object with origin validators. A `304 Not
Modified` updates only an existing object and never creates a new body.

The 200 content-type, content-encoding, length, and body-transform requirements
do not apply to a valid 304. The 304 branch validates only replacement metadata:
it must not introduce `Set-Cookie`, forbidden cache-control semantics, wildcard
`Vary`, or unaccounted vary dimensions. It preserves the SDK-suggested update
action and the previously admitted body.

An unsafe 304 is not handled like a rejected 200 response. Calling
`set_uncacheable` could return an empty 304 to an originally unconditional
request instead of the cached body. The hook therefore returns a typed error for
an unsafe 304 and abandons the update, with no automatic origin retry. During
SWR, the stale response may already have been served and the pinned SDK swallows
the background update error; during foreground revalidation, the request fails
through the existing typed proxy path. Unsafe metadata never refreshes or
replaces the admitted object.

### Rejected responses

A non-304 response rejected from storage still proceeds through the existing
publisher response path for that request. The hook calls
`set_uncacheable(false)`, returns `Ok`, and allows the response to be transformed
and sent to the browser without storage.

If it contains `Set-Cookie` or private directives, the existing final response
privacy layer continues to strip shared-cache control and enforce private
browser behavior.

## Freshness and stale behavior

### Rule-owned freshness

Every enabled rule supplies a positive TTL. SWR is optional and zero when
omitted.

After admission, the Fastly hook applies the rule TTL and SWR instead of
extending an origin response implicitly. The policy fingerprint includes these
values so a config change does not silently reuse an object with old metadata.

### SWR behavior

Within TTL, Fastly serves the fresh template. During SWR, Fastly may serve the
stale template while designating one request to refresh it. All responses still
run per-request TS transformation and personalization after lookup.

The pinned SDK attaches the selected background revalidation transaction to the
returned `fastly::Response` and completes it when that value is dropped. The
current `fastly_response_to_platform` conversion consumes the body and drops the
Fastly response before returning to core. Therefore the one request selected to
revalidate may block on origin revalidation at the adapter boundary; other
stale hits can still use the stale object. Phase one documents and measures this
limitation instead of claiming fully non-blocking SWR. Preserving the SDK
response lifetime through downstream commit is a future optimization.

A stale template is anonymous. Serving it cannot replay another user's state,
but it can serve old publisher content. Operators choose TTL/SWR together with
their CMS purge guarantees.

### Stale-if-error

Phase one does not add a separate configurable stale-if-error override. SWR is
not an error fallback after its window expires; it solves a different freshness
problem. A later implementation may add stale-if-error only after its runtime
semantics and purge interaction are specified.

## Failure behavior

| Failure                                                    | Behavior                                                                                                                                     |
| ---------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------- |
| No rule or request bypass                                  | Use the existing origin path with the original request                                                                                       |
| Runtime unsupported                                        | Use the existing origin path with the original request                                                                                       |
| Key/policy construction failure before request mutation    | Log a bounded reason and use the existing origin path                                                                                        |
| Fastly cache/send failure                                  | Preserve typed proxy failure behavior; do not retry because the SDK cannot generally prove that origin dispatch did not occur                |
| Origin transport failure                                   | Preserve current publisher proxy error behavior; do not insert                                                                               |
| Non-304 response admission rejection                       | Call `set_uncacheable(false)`, return `Ok`, and serve the fetched response through the existing pipeline                                     |
| Unsafe 304 revalidation metadata                           | Return a typed hook error, abandon the update, and do not retry or return an empty 304 as the template response                              |
| Actual body exceeds `max_object_bytes` during insertion    | Abort the unfinished cache writer and fail the request; do not retry the consumed origin response                                            |
| Other insertion/finalization failure after origin dispatch | Do not repeat the origin request; return the recoverable origin response only when the SDK explicitly preserves one, otherwise proxy failure |
| Stale object within SWR                                    | Serve stale and allow one Fastly revalidation transaction                                                                                    |
| Purge unsupported on adapter                               | Return an explicit unsupported/`501` administrative result                                                                                   |

No automatic retry occurs after entering `Request::send`, even for `GET`, because
`SendError` does not expose a general pre-origin-dispatch guarantee. An unfinished
counting-transform writer must not be committed as a partial object; this is a
mandatory Fastly staging assertion. Phase one handles only `GET`.

## Surrogate keys

Surrogate keys are attached to the internal origin-template object through the
Fastly cache API. They do not make the final assembled response shared-cacheable
and do not need to be exposed to the browser.

Every object receives:

- a schema/global template key;
- a stable site key;
- a policy/rule key;
- a stable path key;
- zero or more configured group keys.

Conceptually:

```text
ts-tpl-v1
ts-tpl-site-<site-hash>
ts-tpl-policy-<site-hash>-<policy-hash>
ts-tpl-path-<site-hash>-<path-hash>
ts-tpl-group-<site-hash>-<group-slug>
```

Hashes are lowercase fixed-length hexadecimal or another explicitly specified
URL/ASCII-safe encoding. Raw hosts, paths, customer names, and query values do
not appear in surrogate keys.

The path key intentionally excludes policy fingerprints and representation
variants so one path purge removes old/new policy revisions and every encoding
or bounded header variant still resident in cache.

Phase one does not inherit arbitrary origin `Surrogate-Key` values. The
after-send hook calls `CandidateResponse::set_surrogate_keys` with the complete
TS-generated set, replacing the SDK's suggested/origin key set. Merely adding a
request cache override would extend origin keys and would violate this
invariant. Origin key namespacing and trust require a separate allowlist design.

## CMS purge contract

The design reserves `POST /_ts/admin/cache/purge`, implemented in a later
origin-cache PR or a tightly coupled purge PR, with a payload conceptually like:

```json
{
  "mode": "soft",
  "paths": ["/articles/example-story"],
  "groups": ["home", "articles"],
  "purge_site": false
}
```

The endpoint is `POST`-only, requires the expected JSON media type, and
authenticates before parsing the body or computing purge keys. Its path must be
included in `Settings::ADMIN_ENDPOINTS` so configuration validation and
cross-adapter route tests guarantee authentication coverage.

The endpoint uses the existing administrative authentication boundary and:

- accepts only Fastly when purge capability is available;
- allows authenticated soft purge by default;
- allows authenticated hard purge only when the separate default-disabled
  `cache.template_purge.allow_hard_purge` setting is true;
- requires absolute paths beginning with `/` and rejects schemes, hosts,
  fragments, queries, control characters, and traversal-like invalid forms;
- accepts only configured group slugs;
- caps path count, group count, body size, and total purge operations;
- computes surrogate keys server-side;
- never accepts arbitrary raw surrogate keys from any caller;
- returns per-key success/failure without echoing sensitive configuration.

### CMS publication strategy

For a content update, the CMS or operator should purge:

1. the canonical content path;
2. every listing group affected by the update, such as a homepage or section;
3. the site key only when dependency mapping is unavailable or a global element
   changes.

Soft purge is the normal mode so a stale anonymous template can remain available
while Fastly revalidates. Hard purge is reserved for privacy, legal, or urgent
content-removal events where stale delivery is unacceptable.

The first implementation may expose operator-triggered path/group purge before
a framework-specific CMS webhook. The contract remains generic and does not
hard-code WordPress or Next.js behavior.

## Transformed-response normalization

A cached template's headers describe the origin bytes, not the transformed
browser representation. Any processed HTML response must remove payload-derived
metadata unless recomputed.

At minimum, processed HTML removes or recomputes:

- `Content-Length`;
- `ETag` and `Last-Modified`;
- `Content-MD5`, `Digest`, and `Repr-Digest`;
- `Content-Range` and `Accept-Ranges`;
- stale origin `Transfer-Encoding` framing where the adapter does not already
  normalize it;
- origin cache-age/debug headers that would describe the template rather than
  the assembled response.

`Content-Encoding` follows the existing processing/compression-offload path.
`Vary: Accept-Encoding` remains where delivery can vary by encoding.

This normalization applies whether the template came from cache or origin and
must occur before final response commit.

## Final response privacy

Template caching does not alter the final SSAT privacy rule:

```http
Cache-Control: private, max-age=0
```

The final response strips shared-cache control headers, including:

```http
Surrogate-Control
Fastly-Surrogate-Control
CDN-Cache-Control
Cloudflare-CDN-Cache-Control
```

The existing `response_privacy.rs` finalizer remains the last safety net for
cookie-bearing responses and operator-configured headers. A cache hit must not
skip it.

Internal template surrogate keys tag only the origin-template object. They are
not evidence that assembled HTML may be stored.

## Observability

### Request outcome

Each publisher request records one bounded template-cache outcome:

- disabled/no matching rule;
- bypass with stable reason;
- hit;
- stale hit;
- miss/store;
- uncacheable miss with stable rejection reason;
- cache error.

The optional `PlatformResponse` cache-result metadata is the source for these
outcomes. If the pinned SDK cannot distinguish two states reliably, telemetry
uses a coarser truthful outcome rather than inferring one from timing or an
upstream `X-Cache` string.

Logs include rule ID, outcome, object-size bucket, and a short cache-key digest
prefix where useful. They do not include cookie values, authorization, query
strings, body data, consent data, EC IDs, or raw variant values.

### Metrics

The implementation should expose counters/histograms for:

- eligible and bypassed requests by rule/reason;
- hit, stale, miss, and insertion outcomes, plus revalidation only when the
  adapter can prove it;
- admission rejection reason;
- compressed stored object size;
- origin latency on miss versus lookup latency on hit;
- rewrite/assembly time after hit versus miss;
- purge requests and failures;
- cache/send errors.

These measurements determine whether origin-template caching removes meaningful
latency when the publisher already has an upstream CDN and whether
transformed-template caching should be prioritized next.

### Debug headers

A debug-only response header may expose a coarse outcome such as `hit`, `miss`,
`stale`, or `bypass`. It must be gated by existing debug/operator controls and
must not expose the primary key, variant values, origin details, or purge keys.

## Runtime behavior

| Runtime    | Phase-one behavior                                                                              |
| ---------- | ----------------------------------------------------------------------------------------------- |
| Fastly     | HTTP readthrough template cache, custom key, admission hook, collapse, TTL/SWR, surrogate purge |
| Axum       | Existing uncached publisher request; no canonicalization                                        |
| Cloudflare | Existing uncached publisher request; no canonicalization                                        |
| Spin       | Existing uncached publisher request; no canonicalization                                        |

The non-Fastly behavior is an explicit fallback, not a local in-memory cache.
An in-process cache would have different eviction, isolation, collapse, and
purge semantics and would provide misleading parity.

## Rollout strategy

1. Land this design without runtime behavior changes.
2. Implement pure rule matching, eligibility, canonicalization, key material,
   and response-admission tests.
3. Implement Fastly HTTP cache metadata and hooks behind disabled rules.
4. Validate bot-management/service-origin prerequisites in staging.
5. Enable one low-risk anonymous route with a short explicit TTL and no SWR.
6. Verify hit/miss, canonical origin headers, response privacy, body limits, and
   purge behavior.
7. Enable bounded SWR and CMS purge.
8. Expand route rules gradually while monitoring bypass ratio, rejection ratio,
   hit rate, origin latency, and rewrite CPU.
9. Keep `enabled = false` as the immediate per-rule kill switch.

Deploy a binary that understands `cache.template_rules` before publishing an
enabled rule. Disable and republish rules before rolling back to a binary that
does not recognize the new configuration fields.

## Test strategy

### Configuration and matching

- Rules default disabled.
- Enabled rules require `anonymous_only = true`.
- IDs are unique diagnostic-safe slugs and matchers compile eagerly.
- Exactly one matcher family is configured.
- First matching rule wins.
- A bypass on the first matching rule does not fall through.
- TTL/body/variant/group bounds reject invalid config.
- Duplicate header-rule names after case folding and duplicate allowed values
  reject config.
- Variant Cartesian products over the configured limit reject config.

### Request eligibility

- GET document navigation is eligible when all policy inputs match.
- HEAD, POST, range, prefetch, RSC, API-like, and query-bearing requests bypass.
- Authorization and every conditional request field bypass.
- Repeated/mixed-case `Cache-Control` and `Pragma` fields detect `no-cache`,
  `no-store`, `max-age=0`, and malformed/ambiguous syntax and bypass unchanged.
- `only-if-cached` returns local 504 without origin dispatch, including when
  mixed with `no-cache`, `no-store`, `max-age=0`, or malformed directives.
- Each exact/prefix login, preview, paywall, and personalization cookie bypass
  works.
- A bypass cookie in any repeated Cookie field is found.
- Malformed, invalid, or duplicate cookie syntax bypasses with the unchanged
  request.
- Unknown valid cookies are stripped from an otherwise eligible canonical
  request.
- Unsupported runtimes retain the original cookie/header request.
- Fixed, removed, bypass, and bounded-vary header actions work as specified.
- Repeated fields, comma lists, invalid bytes, and reordered mixed allowed/
  disallowed values cannot create key/origin mismatches.
- Missing/unlisted variant values cannot create arbitrary keys.

### Canonical request

- Cookie, authorization, IP/forwarding, client hints, referer, ranges,
  conditionals, and internal signals do not reach the template origin.
- Trusted host/scheme and origin Host override remain correct.
- Accept and encoding normalization are deterministic.
- Auction/EC logic still sees the original client request before
  canonicalization.

### Key construction

- Same normalized request and policy produce the same 32-byte key.
- Site, origin, path, scheme, host, policy, encoding, and variant changes alter
  the key.
- Cookie values, EC IDs, consent, auction data, and queries are absent.
- Length framing prevents ambiguous concatenation.
- Variant ordering does not change the key.
- Policy changes produce a new fingerprint.

### Response admission

- Safe 200 HTML with supported encoding and bounded length is admitted.
- Missing/oversized/invalid declared `Content-Length` rejects.
- The counting body transform admits exact-limit bodies and aborts actual
  over-limit or truncated/mismatched bodies without committing a partial object.
- Non-HTML, partial, redirect, 4xx, and 5xx responses reject.
- `Set-Cookie`, `private`, `no-store`, `no-cache`, and `must-revalidate` reject.
- `Vary: *` and unknown `Vary` reject.
- Canonicalized and bounded variant `Vary` fields admit.
- A valid 304 follows its metadata-only branch without 200 body requirements.
- Unsafe 304 metadata cannot refresh a stale object.
- Non-304 admission rejection calls `set_uncacheable(false)`, returns `Ok`, and
  serves the current response without hit-for-pass metadata.
- Unsafe 304 metadata produces a typed update failure and can never return an
  empty 304 in place of the admitted body.
- The hook replaces, rather than extends, origin surrogate keys.

### Fastly behavior

- Cache options map to the 32-byte key, TTL, SWR, surrogate keys, and
  after-send hook.
- Pure tests cover cache-option and hook construction without invoking cache
  hostcalls.
- Fastly staging proves same-key miss collapse, insertion, actual body-cap abort,
  SWR/revalidation, 304 update, and purge because Viceroy cannot emulate them.
- Different variants do not collapse.
- Fresh hit, stale hit, inserted miss, uncacheable miss, and rejection outcomes
  are observable; staging origin counts verify revalidation when the SDK does
  not expose selection directly.
- Soft and hard purge cover every encoding/variant for a path.
- Cache/send failures do not trigger an unsafe automatic origin retry.
- Response-side cache-result metadata reports only outcomes the adapter can
  prove.

### Privacy and finalization

- Cached templates never contain request cookie/authorization data.
- SSAT output remains private on hit, stale hit, miss, and revalidation.
- Edge-cache control headers cannot be reintroduced by operator headers.
- Origin validators/ranges/length do not describe transformed bytes.
- Set-Cookie responses are never inserted.
- Logged-in, preview, paywall, and personalized signals bypass.

### Cross-adapter behavior

- Axum, Cloudflare, and Spin make the original origin request without template
  cache metadata.
- Unsupported adapters do not strip cookies or canonicalize headers.
- `POST /_ts/admin/cache/purge` authenticates before body parsing on every
  adapter, requires POST plus JSON, and rejects oversized/invalid payloads.
- Unsupported adapters return `501` only after authentication.
- Hard purge is rejected unless `allow_hard_purge` is enabled; arbitrary raw
  surrogate keys are always rejected.

### Manual/staging verification

- Confirm the service-authenticated or allowlisted origin works without browser
  bot cookies.
- Compare origin request counts before and after a controlled miss burst.
- Verify object age, SWR, revalidation, and purge with representative HTML.
- Confirm publisher content updates invalidate article and listing groups.
- Confirm final HTML never becomes shared-cacheable.
- Measure origin RTT saved and rewrite CPU remaining on hits.

## Future phase: transformed-template cache

The transformed cache may store auction-independent rewritten/head-injected HTML
and late-bind only per-request slot/bid data. It is not a trivial reuse of the
origin-template key.

A future design must resolve:

- a stable cached placeholder representation; the current marker is deliberately
  random per request;
- transformation inputs such as public host/scheme, integration configuration,
  TSJS module hashes, route-matched slots, and post-processors;
- a transformation-policy fingerprint;
- whether full-document post-processors are safe and deterministic;
- payload validator and compression representation;
- transformed object body limits and rewrite CPU measurement;
- purge fan-out between origin and transformed object namespaces.

The production audit suggests this phase may provide substantial value when the
publisher origin is already behind an effective CDN, because it can remove
per-request rewrite CPU rather than only upstream RTT.

## Future phase: RSC/API dynamic families

RSC/API caching requires a separate configured family with measured evidence.
Before implementation, each family must record:

- request count and candidate hit ratio;
- unique normalized key count;
- `Vary` and router/header cardinality;
- decoded/compressed body sizes;
- whether source router state changes the target body;
- cookie/auth/personalization behavior;
- query semantics;
- purge relationship to HTML and CMS content.

A framework preset may later populate generic header/query rules, but core and
adapters must not branch on Next.js header names. Unbounded router state must
not be copied into a generic dynamic key without explicit measurement and caps.

## Implementation prerequisites and open deployment inputs

The design is complete without hard-coding deployment-specific values, but each
enabled publisher must supply:

- exact anonymous route matchers;
- login/preview/personalization cookie names or prefixes;
- any bounded header variant values;
- confirmation that the canonical request works without end-user bot cookies;
- a TTL/SWR choice;
- a compressed object-size cap;
- CMS path/group purge mapping;
- a staging validation result.

The pinned Fastly SDK 0.12.1 exposes the required HTTP cache primitives. Pinned
Viceroy 0.17.0 does not support the HTTP readthrough cache hostcalls, so local
Viceroy tests cannot prove lookup, collapse, insertion, SWR, 304 update, or
purge behavior. The implementation PR must test pure policy/hook construction
locally and execute a documented Fastly staging verification matrix for runtime
semantics.

## Acceptance-criteria mapping

| Issue #859 criterion                                                                 | Design coverage                                                     |
| ------------------------------------------------------------------------------------ | ------------------------------------------------------------------- |
| Design covers key, bypass, cookie/header policy, purge, runtime support, and privacy | Dedicated sections above                                            |
| Personalized SSAT HTML remains private and never enters shared cache                 | Cache boundaries, invariants, final response privacy                |
| Origin-template cache stores only proven-safe routes/responses                       | Disabled rules, operator assertion, eligibility, response admission |
| Template cache emits purgeable keys and has a CMS purge strategy                     | Surrogate keys and CMS purge contract                               |
| Dynamic Vary/key normalization is generic/configurable                               | Generic header actions and bounded variants; no framework branch    |
| RSC/API caching is gated by measurement                                              | Production evidence and deferred RSC/API phase                      |
| Tests cover logged-in/preview/paywall/personalized bypass                            | Eligibility, privacy, and cross-adapter test matrices               |

## Locked decisions

1. Fastly is the first cache runtime; all other adapters preserve current origin
   behavior.
2. The first PR for #859 is this design only.
3. Rules are disabled by default and explicitly allowlist anonymous HTML routes.
4. Eligible origin requests are canonical and cookie-free.
5. Configured login/preview/personalization cookies bypass before all cookies are
   stripped.
6. Query-bearing requests bypass in phase one.
7. Authorization, ranges, prefetches, and non-document families bypass.
8. Every `Vary` field is canonicalized or represented by a bounded generic
   policy; unknown fields reject storage.
9. Any origin `Set-Cookie`, `private`, `no-store`, `no-cache`, or
   `must-revalidate` rejects storage in phase one.
10. Fastly HTTP readthrough cache provides storage, collapsing, revalidation,
    SWR, and purge.
11. TTL is required per rule; SWR defaults to zero.
12. Site, policy, path, and configured group surrogate keys are attached to
    internal template objects.
13. RSC/API, transformed templates, query normalization, and response-cookie
    stripping are deferred.
14. Cookie and custom-header parsing fail closed on malformed, repeated, or
    ambiguous values.
15. The actual stored body cap is enforced by a counting insertion transform;
    over-limit insertion fails closed and must not commit a partial object.
16. Non-304 admission rejection returns the fetched response through
    `set_uncacheable(false)`; unsafe 304 metadata is a distinct typed update
    failure.
17. Fastly cache/send errors are not retried without a provable pre-dispatch
    signal.
18. Hard purge is disabled unless separately enabled for authenticated admins.
19. SSAT-assembled HTML remains `private, max-age=0` regardless of cache outcome.
