# Proxy disclosure and cache policy

**Date:** 2026-11-16

**Status:** Implemented locally; release qualification remains open

**Issue:** [#1233](https://github.com/IABTechLab/trusted-server/issues/1233)

**Related:** [#982](https://github.com/IABTechLab/trusted-server/issues/982)

**Source baseline:** Trusted Server main `1810c763add220dd34fd5faaffe2c69aab1d556f`,
EdgeZero `v0.0.11` (`87456c708ffe9edf86b3a0f8b06dfd15d60b07b5`).

## 1. Outcome and boundaries

Creative resources fetched through `/first-party/proxy` receive no automatically
forwarded EC identity by default. Operators explicitly authorize identity
destinations by HTTPS origin and path. Authorization is evaluated before the
initial send and before every followed redirect. A request that performs identity
endpoint delivery bypasses upstream caching at that endpoint, and its final
response cannot be stored by browsers or shared caches.

The implementation introduces a small typed policy in core, shared with CLI
diagnostics. It does not introduce a general policy language, a new transport, or
a replacement for every existing asset-serving integration.

This delivery includes:

- Default-deny EC disclosure for the creative proxy, including carried `ts-ec`.
- Explicit endpoint matching, current-request disclosure authorization, approved
  request headers, per-hop cache behavior, and chain-wide response privacy.
- Fail-closed terminal redirects for this creative-fetch contract.
- Explicit query-identity choices at existing shared proxy call sites.
- The same behavior through Fastly, Axum, Cloudflare, and Spin.
- Typed configuration validation and a read-only CLI policy explanation command.
- Operator documentation, configuration compatibility, and release qualification.

Excluded from this delivery:

- Changing `/first-party/click` identity or navigation behavior.
- Redefining intentional identity in integration payloads, bidder requests, or
  partner synchronization. Testlight's JSON identity remains intentional.
- Adding identity creation, identity-record I/O, provider lookups, or cookie
  refresh to the disclosure policy. Existing adapter-owned withdrawal and marker
  finalization remains supported; see section 6.
- CORS expansion, a parent signing broker, and connection-level private-address
  rejection required by #982. Section 13 defines the extension boundary.
- Automatic identity grants from audit observations, static-file heuristics,
  vendor detection, or MIME type.
- New HTTP caching in Axum, Spin, Viceroy, or the CLI development proxy.
- Proving anonymity against a recipient that already has other identifiers in
  the creative URL, or preventing an authorized recipient redistributing an ID.

## 2. Problem and current behavior

`handle_first_party_proxy` reconstructs and validates a signed destination, then
sets `forward_ec_id: true`. `proxy_request` reads the legacy EC accessor and
upserts `ts-ec` before constructing the upstream request. The accessor accepts
`x-ts-ec` before the cookie and does not generate missing identity.

The same endpoint handles images, scripts, CSS, iframe HTML, and tracking
resources. Consequently an ordinary asset receives an identifier unnecessarily,
and two readers can produce different upstream URLs for identical asset bytes.
Fastly's normal URL-based readthrough cache then cannot share that URL entry
across those readers. Other response/cache policies can still prevent caching;
equal URLs are necessary for this ordinary sharing, not sufficient proof of it.

The main baseline uses the core `proxy_with_redirects` loop. It does not
independently append EC at every redirect, contrary to the issue's description. A `Location` can carry
the injected parameter onward. Skipping injection alone does not stop that case.

There is a second boundary: the signed browser URL does not contain the reader's
automatically added identity, while the generic finalizer preserves upstream
cache headers. A hypothetical public personalized response could be cached under
that browser URL. Caching can also suppress event delivery. These are
source-established risks, not claims about observed production incidents.

### 2.1 Source map and feasibility

| Surface            | Baseline evidence                                                                        | Design consequence                                                 |
| ------------------ | ---------------------------------------------------------------------------------------- | ------------------------------------------------------------------ |
| Creative fetch     | `core/src/proxy.rs`, `handle_first_party_proxy`, `proxy_request`, `proxy_with_redirects` | Enforce at execution, after signature validation                   |
| Signing            | `core/src/creative.rs`, `build_signed_url_for`; `proxy.rs`, signed-target reconstruction | Keep current target signatures valid on the existing route         |
| Canonical identity | `core/src/ec/mod.rs`, `EcContext::read_from_request_with_geo`                            | Use canonical cookie identity and request-local consent            |
| Consent            | `core/src/consent/mod.rs`, `allows_ec_creation`, `allows_eid_persistence`                | Compose existing decisions; do not equate issuance with disclosure |
| Response privacy   | `core/src/response_privacy.rs`                                                           | Reuse terminal no-store enforcement                                |
| Managed JS         | `core/src/integrations/js_asset_proxy.rs`                                                | Preserve selected headers, fixed UA, validators, no redirects      |
| Publisher assets   | `core/src/proxy.rs`, `handle_asset_proxy_request`                                        | Preserve separate S3/optimizer/range execution                     |
| CLI discovery      | `cli/src/commands/audit/generate/{mod,analyzer,browser_collector}.rs`                    | Discovery is advisory; generated assets remain disabled            |
| CLI transport      | `cli/src/commands/dev/proxy/{server,upstream/mod}.rs`                                    | No production disclosure decisions here                            |

Paths abbreviated `core` and `cli` in this table mean
`crates/trusted-server-core` and `crates/trusted-server-cli`.

The baseline's `PlatformHttpRequest::with_cache_bypass` and
`PlatformCacheIntent::{Default,Bypass}` provide the required cache selection.
Fastly maps bypass to pass mode, Cloudflare maps it to no-store, and transports
without intermediary caches need no new cache implementation. Existing adapters
return redirects to core rather than following them invisibly.

Apply the policy inside the existing core loop immediately before each send.
Keep current redirect limit, per-hop backend timeouts, method/body transitions,
and streaming behavior. No new transport, EdgeZero repin, or dependency on the
pending cutover is part of this delivery. Add strict creative userinfo rejection
before initial or redirected sends; the whitelist header profile sends no Cookie
or Authorization at any hop. Existing integration contracts remain unchanged.

Pinned Viceroy 0.21.0 does not implement HTTP readthrough caching. Runtime origin
URL observations there are useful; actual Fastly cache-hit qualification requires
a real Fastly service. Neither source inspection nor agreement on this document
constitutes that runtime qualification.

If the separate EdgeZero outbound cutover lands before implementation,
reconcile the execution task against that new base. Its prepared-request callback
is a suitable alternative attachment point, but must not be treated as an API
available on this baseline. The evaluator, query helper, consent policy and
acceptance remain independent of that transport change.

## 3. Invariants

1. Fetch permission is independent of identity permission. Identity rules never
   widen `proxy.allowed_domains` or route-specific HTTPS restrictions.
2. Empty identity rules authorize no destinations, even though empty
   `allowed_domains` currently permits all valid hosts.
3. Only the creative contract can consult these identity rules. A managed asset
   remains identity-disabled even if its URL matches a rule.
4. Every actual creative send uses a decision for that destination. Prior-hop
   permission is never inherited as recipient permission.
5. A canonical existing cookie EC and an affirmative current-request consent
   decision are both required. No new identity is generated.
6. Identity endpoints bypass upstream caches whether or not an EC is available.
7. Once any identity endpoint is visited, final response privacy never relaxes.
8. Strict creative fetches never return an unprocessed redirect `Location`.
9. Mutable identity/chain state is request-local, never retained in settings,
   globals, thread locals, or reusable app state.
10. CORS permissions remain owned by the route, not granted by an endpoint rule.

## 4. Operator configuration

Add `proxy.identity_endpoints`, a default-empty collection compatible with the
existing sequence/map environment overlay convention:

```toml
[[proxy.identity_endpoints]]
id = "impression-delivery"
origin = "https://measurement.example.com"
path_exact = "/events/impression"

[[proxy.identity_endpoints]]
id = "identity-api"
origin = "https://identity.example.com"
path_prefix = "/v1/identity/"
```

An entry is an explicit grant to use the identity-endpoint contract when the
creative proxy requests a matching destination. There is no separate auto-enabled
role heuristic and no public toggle allowing identity endpoints to be cached.
These endpoints are all GET destinations in this version, matching the current
route. This does not extend route registration to HEAD or POST.

### 4.1 Validation and matching

- `id` is unique, 1–64 ASCII letters/digits/underscore/hyphen, and appears in
  diagnostics. It contains no URL or credential.
- `origin` is an absolute HTTPS origin with DNS hostname and optional explicit
  port, no credentials, non-root path, query, or fragment. A root slash is
  accepted and normalized away. IP literals, wildcard hosts, non-ASCII input,
  trailing-dot hosts, empty labels, and port zero are rejected. Operators use
  punycode for internationalized names.
- Match normalized hostname and effective port. Case and an explicit default
  `:443` do not create a different origin. A different port does.
- Exactly one of `path_exact` and `path_prefix` is required. Paths start with
  `/`, are case-sensitive, and contain no query, fragment, backslash, whitespace,
  control character, percent escape, or literal `.`/`..` segment.
- Prefixes are `/` or end in `/`. `/events/` matches `/events/a` but not
  `/events`, `/eventual`, or `/events-other/a`. Exact paths match byte-for-byte.
- At the initial boundary, evaluate the authenticated reconstructed target's
  serialized path. Percent escapes still present there are ineligible for a
  grant. Earlier creative parsing/signing may already have normalized encoded
  dot segments or backslashes; legacy tokens carry no original-input provenance.
  Classification promises apply to the actual authenticated destination, not
  unavailable pre-sign text. CLI explain uses this same initial canonicalization.
- At redirects, inspect the raw Location path before resolution as well as the
  resolved serialized path. A percent escape or backslash in the raw path makes
  that hop identity-ineligible. Percent escapes in the query alone do not. This
  extra check uses evidence available at this boundary and requires no token
  format change. Relative Locations inherit the current path when appropriate;
  resolved-path checks still apply.
- Query strings never participate in matching. Query-discriminated identity
  endpoints sharing the same path with assets are unsupported and must use
  separate paths/origins before enabling a rule.
- Duplicate normalized selectors are rejected, including entries with different
  IDs. Overlapping prefixes/exact paths are allowed because every rule grants
  the same behavior. For stable diagnostics, exact wins, otherwise the longest
  prefix wins. Matching semantics are independent of configuration order.
- Invalid entries fail settings load and `ts config validate`; they are not
  silently dropped. No regex or general glob syntax is introduced.

`#[serde(default, skip_serializing_if = "Vec::is_empty")]` keeps the empty field
out of stored JSON. The compiled matcher has opaque private internals and is nonserialized state in
`Proxy`, built during settings normalization/validation and shared immutably. Its
serde-skipped storage field is hidden from generated documentation but public to
preserve existing external `Proxy { ..Default::default() }` construction.
Direct programmatic settings construction must call the same compile/validation
entry point before routing; tests must not rely on a stale compiled default.

## 5. Typed architecture and ownership

Add a focused `proxy_policy` core module with configuration/compiled matcher,
decision types, and request-local disclosure/chain types. Keep raw query editing
in a small private query helper module. Do not move unrelated publisher or asset
code out of `proxy.rs` as part of this change.

| Type or responsibility        | Required behavior                                                                                    |
| ----------------------------- | ---------------------------------------------------------------------------------------------------- |
| `ProxyQueryIdentity`          | Explicit `Disabled` or `CreativeEndpoints`; no default-on identity                                   |
| Existing `ProxyRequestConfig` | Retain transport/body/rewrite options; replace the EC boolean with explicit query identity selection |
| `CompiledProxyPolicy`         | Immutable origin/path matchers; no request data                                                      |
| `ProxyDisclosureContext`      | Canonical existing EC, consent eligibility, reason; no minting/store calls                           |
| `ProxyHopDecision`            | Fetch permission and optional matched recipient; a match selects event cache/privacy requirements    |
| `ProxyChainState`             | Request-local sticky identity-endpoint visit and resulting response privacy                          |

The decision stores the matched endpoint rather than separate flags for its fixed
identity, bypass, and response-privacy requirements. This keeps those requirements
coordinated. Raw ambiguous redirect paths withhold identity; a resolved known
event destination still bypasses upstream caching and makes the chain private.

`ProxyRequestConfig::new` becomes identity-disabled. Keep `without_ec_id()` as an
identity-disabled convenience so existing explicit callers retain meaning. The
creative handler alone selects `CreativeEndpoints`. Call sites are migrated and
tested explicitly; this is a documented Rust helper API change, not a change to
the click handler or integration-owned JSON identity.

The creative handler accepts a `&EcContext` built by the adapter's normal
geo-aware request extraction. Fastly reuses its pre-routing context. Axum,
Cloudflare, and Spin use their existing geo-aware helper for this route. Context
construction failure supplies no authorized identity. Unsupported/missing geo
must not be replaced with invented non-regulated jurisdiction.

Other proxy users do not need a disclosure context when identity is disabled.
Use a small input struct if extending a function would exceed seven arguments.

## 6. Current-request disclosure authorization

This version deliberately adopts a conservative technical disclosure policy:

```text
eligible = canonical existing cookie EC
           AND !consent.expired
           AND !consent.gpc
           AND allows_ec_creation(consent)
           AND allows_eid_persistence(consent)
```

Implement a named pure helper in the consent module so proxy callers do not
duplicate consent internals. `allows_eid_persistence` already expresses the
effective TCF Purpose 1 and 4 decision after existing signal conflict handling;
reuse that decision without constructing a fake EID collection. This helper is a
product policy for EC disclosure, not a claim of complete vendor-specific legal
compliance or a reason to modify the policies of unrelated consumers.

Consequences are intentional: unknown jurisdiction, required missing/undecodable
signals, proxy-mode undecoded TCF, expiry, explicit opt-out, or denied Purpose 4
withhold EC. Known non-regulated traffic without signals follows existing
creation/transmission decisions. GPC withholds proxy disclosure in every
jurisdiction. Cookie retention is unchanged; a withheld disclosure is not itself
a withdrawal or record deletion.

Ignore raw `x-ts-ec` on the creative route. Do not fall back to it when the
cookie is absent or invalid. Click and other legacy accessor consumers retain
their existing behavior in this delivery. The new disclosure policy does not issue EC cookies. Preserve existing
adapter-owned finalization: Fastly currently attaches `EcFinalizeState` after
named-route execution, including this route. Explicit withdrawal may expire TS
cookies and read/tombstone a record; marker reconciliation may also act. Do not
suppress those operations to satisfy a false whole-request read-only promise.
Do not enable new finalization or issuance on adapters that did not have it.
Positive/denied disclosure tests distinguish policy I/O from preexisting
withdrawal effects and assert that revocation remains authoritative.

On a matching endpoint without eligible identity, still perform the GET with
the event cache policy and without `ts-ec`. Do not silently skip delivery or
substitute a successful pixel. Consent gates identity here, not whether an
already-supported URL is fetched.

## 7. Per-hop execution and query handling

For the initial creative request:

1. Reconstruct and validate the original signed target and expiry.
2. Validate existing fetch permission and scheme restrictions.
3. Evaluate endpoint rules and current-request eligibility.
4. Remove all reserved EC query occurrences; append exactly one authorized EC
   only for a matching eligible destination.
5. Build approved outbound headers and apply the hop cache decision.
6. Mark an identity-endpoint visit before attempting its send.
7. Dispatch through the existing `PlatformHttpClient` transport.

For every followed redirect, retain the raw Location path eligibility, resolve
against the current effective target, authorize fetch, and apply the same
query/header/cache decisions on the next loop iteration before its send.
Build creative headers only from the approved profile. Keep existing method/body
transitions and remove body-describing headers when the loop drops the body.
Select bypass on the actual `PlatformHttpRequest`, not only diagnostic metadata.
Do not alter unrelated integration additional-header behavior or S3 signing.

The loop's effective current URL must become the transformed URL actually sent,
so subsequent relative Location resolution and response processing use the right
base. Retain current backend timeout and streaming configuration. The baseline
has no shared whole-chain deadline; this design does not claim or add one.

### 7.1 Reserved query semantics

On creative sends, no match or no eligible identity removes `ts-ec`, including
values in the signed original URL or an echoed redirect. Match case-sensitively
after percent-decoding the query key exactly once; `ts%2Dec` is reserved,
`TS-EC` is a different key, and nested encoding is not recursively decoded.
Invalid key escapes do not create a reserved match. Remove all reserved pairs.

Preserve unaffected raw query components, order, escapes, empty values, and
key-only parameters as they exist at the authenticated reconstructed-target or
raw/resolved redirect boundary. Existing TS signing/reconstruction already
form-normalizes initial queries; do not promise recovery of pre-sign byte forms. Do not round-trip the entire query through form encoding.
When nothing changes, retain the original query bytes. Append the canonical EC
using query-safe encoding. Apply before the actual send and after TS signature
validation. Never apply this helper to publisher S3/optimizer URLs or modify
their URL after origin signing.

Removing a parameter can invalidate a vendor's own signature. That incompatibility
is unavoidable when a vendor signature covers a field the new policy forbids;
document it rather than preserving a forbidden identity as a workaround.

## 8. Header profiles

Use named internal profiles, not arbitrary operator-specified header forwarding
for this release:

| Header                         | Unmatched creative resource                | Identity endpoint             |
| ------------------------------ | ------------------------------------------ | ----------------------------- |
| User-Agent                     | Fixed `TrustedServer/1.0`                  | Fixed `TrustedServer/1.0`     |
| Accept                         | Client value if valid/present              | Client value if valid/present |
| Accept-Language                | Client value if valid/present              | Client value if valid/present |
| Accept-Encoding                | Existing supported normalized encoding set | Same                          |
| Cookie, Authorization, x-ts-ec | Never                                      | Never                         |
| Referer, X-Forwarded-For       | Never                                      | Never                         |
| Other browser headers          | Not forwarded                              | Not forwarded                 |

This changes the creative proxy's current UA/referrer/IP exposure. It can affect
UA-specific creatives and referrer-dependent origins; migration documentation and
representative fixtures are required. Do not call this anonymous traffic: Accept
and language are client-derived, and the target can contain vendor identifiers.
Preserve origin `Vary`; do not force public freshness. A future narrowly reviewed
header profile can be added without changing identity grants.

Managed integrations retain their existing selected headers and protocol
contracts. The #982 asset capability will need a stricter profile with no
client-derived negotiation headers.

## 9. Cache and response contract

For unmatched resources, use the existing platform-default upstream cache policy.
For every identity-endpoint hop, set `PlatformCacheIntent::Bypass` using `with_cache_bypass()`, including
missing/denied identity. Never remove EC only from a cache key while sending it
upstream. No new custom Fastly cache key is required.

An identity-endpoint visit makes the entire response terminal-private. Mark it
before dispatch so upstream failure, timeout, body/rewrite failure, and terminal
redirect rejection retain the requirement. The final browser response must have
`Cache-Control: private, no-store`, with validators, expiry/age metadata, and
platform-specific edge-cache directives removed using existing privacy helpers.
Responses must not acquire upstream publisher-domain state-setting headers:
strip `Set-Cookie`, `Strict-Transport-Security`, and `Clear-Site-Data` on this
creative response boundary, following existing publisher asset protection.

All creative-route error responses are private/no-store, which avoids losing the
privacy requirement when a structured error is converted outside the core
handler. Preserve existing error status classification; do not turn all errors
into success or a generic 502. Adapters apply this conversion at the named route
boundary and preserve terminal-private metadata through finalization.

Reassert terminal privacy after operator headers and late integration mutations.
Fastly has a terminal marker hook; native/Worker adapters must prove equivalent
ordering, adding the same enforcement where absent. Do not infer this from the
existence of a shared helper alone. All non-CORS generic proxy responses continue
stripping upstream CORS grants.

This policy guarantees TS uses its platform bypass mechanism for each identity
delivery; it cannot guarantee an external recipient never internally caches or
deduplicates an event. It also does not purge previously cached responses.

## 10. Terminal redirects

The strict creative contract follows only the currently supported redirect
statuses under the existing limit and per-hop timeouts. At every loop exit,
any 3xx response carrying `Location` is rejected with a private/no-store 502;
do not expose that header to the browser. This includes an exhausted limit,
unsupported target scheme, a policy stop, and other unfollowed redirects. A 3xx
without `Location` retains its status and body subject to normal response privacy.

Do not re-sign terminal locations into fresh chains or reset budgets. Keep
supported method/body transitions and existing fetch allowlist enforcement.
The creative whitelist prohibits credentials on every hop; explicitly reject
userinfo on initial and redirected creative targets. This strict behavior belongs
to creative execution, not every no-follow integration or publisher asset route.

## 11. Existing caller preservation

| Caller                           | Required preservation                                                                                             |
| -------------------------------- | ----------------------------------------------------------------------------------------------------------------- |
| `/first-party/click` and rebuild | Existing navigation, identity, signing, and no-store behavior                                                     |
| JS asset proxy                   | Exact routes, no query EC, fixed UA/selected headers, no redirects, MIME protection, validators and TTL semantics |
| External Prebid                  | No query EC/client header copying, constrained HTTPS redirects                                                    |
| GPT                              | No query EC/redirects; existing selected headers and response behavior                                            |
| GTM                              | No query EC; existing explicit forwarding restrictions                                                            |
| Testlight                        | No query EC; preserve intentionally constructed JSON identity                                                     |
| Publisher asset routes           | Existing raw pass-through, Range/conditional headers, S3 signing, query policy, optimizer and cache options       |
| Audit generator                  | Existing disabled asset candidates and review-before-enable workflow                                              |
| CLI development proxy            | Host mapping, trust, forwarding, streaming and connection reuse; no new identity classifier                       |

Use explicit typed selection at each shared-helper production call site. Search
all constructors and struct literals so an omitted migration is a compile failure
or focused test failure rather than silently enabling identity.

## 12. CLI and diagnostics

Add:

```text
ts config proxy explain <URL> [--json] [existing app-config selection options]
```

Use `AppConfigArgs` and its existing manifest/path/environment-overlay semantics.
This command is read-only, performs no URL fetch, and never takes a real EC value
or consent cookie. It evaluates the creative-route policy using the same compiled
matcher as runtime, showing:

- Fetch permission and any configuration error.
- Matched endpoint ID, or the explicit default-deny reason.
- EC eligibility conditional on current-request authorization, never a promise
  that the caller actually has consent.
- Header profile, upstream cache mode, downstream privacy requirement, redirect
  policy, and unchanged CORS restriction.

JSON output is version 1 with stable named fields: `version`, `route`,
`fetch_allowed`, `rule_id`, `identity_eligibility`, `headers`, `upstream_cache`,
`downstream_cache`, and `redirects`. Do not echo the supplied URL query or
fragment; diagnostics use normalized origin and path only. Ordinary allowed and
blocked decisions both exit 0; malformed URL/configuration or command failure
exits nonzero under existing CLI error conventions. Explain is not an assertion
command and never publishes configuration.

`ts config validate/push` use the same typed validation. Audit generation reads
the updated example template but does not add identity grants. Extending Chrome
collection to classify or fetch every asset is outside this delivery. Document
how an operator can use inventory URLs with explain; no automatic configuration
changes from audit evidence.

Runtime debug diagnostics log endpoint ID, normalized origin/path and decision
reason, not EC, cookies, query strings, or complete final URLs. Remove existing
full-URL identity logging on the changed creative path; do not claim repository-
wide log redaction or add per-URL metric cardinality.

## 13. Extensibility and #982 boundary

The shared decision model must not give a route more authority than its contract.
Adding an identity endpoint never enables CORS or makes managed assets disclose EC.
Existing target-only tokens remain accepted only under the existing route's
contract; this delivery adds no caller-controlled purpose query parameter.

For a future separate readable asset capability, the signature must bind version,
route/capability, and target using domain separation. It must not accept a legacy
generic-proxy token as that capability. That future route needs permanent
identity/header restrictions, explicit egress destinations, and private/reserved
address rejection at connection time on every hop, including DNS rebinding
protection. A URL-host comparison alone is insufficient. Verify actual EdgeZero
and adapter connection control before committing that extension's design.

These are extension constraints, not unfinished acceptance for #1233. Implement
neither new capability tokens nor broad CORS nor a broker in this plan.

## 14. Upgrade, rollback, and cache transition

Old configurations loaded by a new binary receive default-deny identity behavior
immediately, including new header/redirect behavior. This is a documented behavior
change. There is no silent legacy default-on mode.

| Writer/configuration                     | Old reader             | New reader                |
| ---------------------------------------- | ---------------------- | ------------------------- |
| Old config without endpoint field        | Existing behavior      | New default-deny behavior |
| New writer, empty endpoint field omitted | Existing behavior      | New default-deny behavior |
| Nonempty endpoint list                   | Rejected unknown field | Validated explicit policy |

Before activation, inventory known identity endpoints and their redirects, run
explain against resource and event samples, and validate vendor behavior with
fictional local fixtures plus authorized staging. Do not auto-populate grants
from all hosts already in `allowed_domains`.

Upgrade: deploy compatible binaries first, then validate and publish the endpoint
configuration. During the binary/config transition identity is withheld; if that
gap is unacceptable use a separately configured blue/green service rather than a
fail-open compatibility flag. Avoid a mixed fleet processing one traffic pool
with different disclosure semantics.

Rollback: publish an old-compatible configuration without the new field before
rolling back binaries. Rolling back restores old forwarding/header behavior;
it is a privacy regression requiring an operator decision. Configuration errors
must never trigger an implementation-owned fallback to legacy forwarding.

Purge relevant shared-cache entries on activation when stale first-party proxy
responses could suppress policy evaluation. Previously stored browser responses
cannot be recalled by a server change. Bound the rollout claim by existing TTLs;
if immediate reevaluation is required, deploy a separately reviewed signed-URL
version/cache-busting transition first. This delivery does not promise immediate
revocation of browser-held responses or add an unbound version parameter.

Existing signed URLs that do reach TS are evaluated against current rules on
every fetch. Policy removal revokes future disclosure without re-signing them.

## 15. Acceptance and preservation matrix

Each test uses the real signed handler or route wherever wiring is material.
Canonical EC fixtures must pass actual cookie validation, and positive consent
must reach the geo-aware context. A legacy arbitrary string or missing geo can
prove absence only and cannot qualify the positive delivery case.

| ID  | Scenario                                                                                 | Required observation                                                                                                  |
| --- | ---------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------- |
| A1  | Two valid reader cookies, same signed static target, no rules                            | Exact identical origin URI; no reserved EC pairs                                                                      |
| A2  | Matching endpoint, valid cookie and permitting consent                                   | Correct EC once; hop bypass; final no-store                                                                           |
| A3  | Same host, unmatched asset path; matched `.gif` event                                    | Asset has no EC; event gets EC independent of extension                                                               |
| A4  | Endpoint without cookie, raw header only, invalid cookie, denied/unknown/expired consent | No EC or policy-generated identity/store work; preserve existing withdrawal finalization; event still bypass/no-store |
| A5  | Conflicting TCF/GPP, missing Purpose 4, GPC, US opt-out                                  | Shared consent decision applied; positive and negative controls                                                       |
| A6  | Static to event; event to static with echoed EC; event to event                          | Every recipient independently decided; chain remains private                                                          |
| A7  | Duplicate/encoded reserved query keys; unusual unrelated escapes                         | All reserved pairs handled; other raw components unchanged                                                            |
| A8  | Origin/path normalization, alternate port, path boundaries and encoded paths             | Exact documented match/withhold behavior                                                                              |
| A9  | Invalid/expired TS signature or forbidden fetch host                                     | No origin request even if an identity rule matches                                                                    |
| A10 | Public personalized event origin; repeated events by one reader                          | Different readers get their own bytes; each event dispatch bypasses; no-store                                         |
| A11 | Event followed by timeout/rewrite error/terminal redirect                                | Error status preserved as specified; final no-store without raw Location                                              |
| A12 | Exhausted redirect limit and non-followable Location                                     | Strict creative 502; no additional origin request or browser escape                                                   |
| A13 | Late public/edge-cache header mutation                                                   | Terminal privacy survives every adapter's final hooks                                                                 |
| A14 | Origin state-setting/CORS headers                                                        | Stripped at creative response boundary                                                                                |
| A15 | Ordinary resource cache policy, compression, HTML/CSS rewrites                           | Origin private/no-store and Vary preserved; rewrites still produce usable signed URLs                                 |
| A16 | Old signed URL after rule removal                                                        | Current fetch withholds EC, independent of token age                                                                  |
| A17 | Click, JS, Prebid, GPT, GTM, Testlight and publisher assets                              | Contracts in section 11 preserved, including JSON identity and S3 signature order                                     |
| A18 | Typed TOML/JSON/environment settings and writer-reader combinations                      | Defaults omitted; invalid rules rejected; compiled policy not serialized                                              |
| A19 | CLI explain with effective overlays, malformed input and blocked host                    | Shared decision parity, documented exits, no network call or sensitive query output                                   |
| A20 | Audit-generated config and CLI-mediated browser request                                  | Disabled candidates preserved; runtime policy unaffected by dev transport                                             |
| A21 | Viceroy two-reader origin fixture                                                        | Identical static origin URLs, intended event URLs and cache selection observations                                    |
| A22 | Real Fastly cacheable static fixture                                                     | Second reader reuses backend cache, origin count remains one under controlled cacheable headers                       |

For meaningful negative controls, demonstrate that tests reject unconditional EC
forwarding, an initial-hop-only policy, absent positive-path wiring, missing event
bypass, and a late public-cache override. Mutations run only in owned disposable
copies during implementation; no production or shared-checkout mutation.

### 15.1 Qualification and verification

Implementation must run the full current AGENTS CI gate list before PR handoff,
plus the new focused policy/handler/CLI/runtime fixtures. Use target-matched aliases
and pinned tool versions. Fastly and Axum provide the first runtime qualification;
native route/terminal tests and target-matched builds cover Cloudflare and Spin,
whose real-runtime qualification must be recorded separately before claiming it.

The new cache/privacy behavior needs a runtime fixture rather than helper-only
tests. Local Viceroy proves network URL behavior, not HTTP cache reuse. Real
Fastly testing requires a separately authorized staging service with a controlled
public origin, no confounding Vary/Set-Cookie, two valid reader cookies, and an
origin counter. Record exact binary/config revision and results. A fake cache or
successful compile is not equivalent evidence.

## 16. Alternatives and trade-offs

| Alternative                                       | Why not selected                                                                      |
| ------------------------------------------------- | ------------------------------------------------------------------------------------- |
| Default-on static extension/host denylist         | Unknown resources still receive identity; pixels and ordinary images share extensions |
| Only add an EC allowlist boolean                  | Leaves event caching, header disclosure and terminal redirect behavior disconnected   |
| Reuse fetch allowlist                             | Grants a second capability unintentionally and cannot distinguish paths               |
| Change cache key but still send EC                | Does not solve disclosure and can mix personalized representations                    |
| One universal asset fetch implementation          | Loses existing S3/optimizer, validator, integration and redirect contracts            |
| New asset/tracker routes for all URLs immediately | Forces signing/producer migration without removing the need for destination rules     |
| General rule-expression engine                    | Unnecessary precedence, validation and operational complexity                         |

The selected design costs more than a one-line default change and deliberately
changes creative header/redirect behavior. It offers bounded extensibility,
operator explanations, and coordinated cache guarantees while keeping existing
specialized integrations and #982's additional capability separate.

## Review log

- Round 1 (Codex, independent): found that a blanket no-store-I/O requirement
  conflicted with existing consent-withdrawal finalization, and that legacy
  signed URLs cannot recover pre-signing URL provenance or query bytes. Preserved
  the existing adapter lifecycle and defined query fidelity at the authenticated
  reconstructed boundary, with raw redirect-path checks where available.
- Round 2 (Codex, independent): reviewed the corrected complete specification
  against main and found no blocking issues. The design uses the existing
  redirect loop and pinned transport API without depending on the outbound
  transport cutover.
