# Cache-Control Header Strategy Implementation Plan

**Date:** 2026-07-06
**Status:** Initial cache-header slice implemented in the current branch
**Spec:** `docs/superpowers/specs/2026-07-06-cache-control-header-design.md`

## Scope

Implement the **initial cache-header slice** from the current spec. The latest
spec resolves the initial-slice open questions and defers the larger dynamic
caching, template caching, streaming, and compression-offload work.

Initial slice goals:

1. Make TS-owned, hash-versioned TSJS responses cache correctly.
2. Make neutralized publisher Prebid compatibility responses safe to cache.
3. Add a structured, runtime-portable cache-policy model.
4. Add a configurable static/rehosted asset cache-rule engine so framework
   assumptions are operator-controlled, not hard-coded.
5. Keep arbitrary publisher-origin assets origin-controlled unless an enabled
   rule proves they are immutable-safe.

Deferred follow-up features are listed separately below and should not be folded
into the initial cache-header PRs.

## Decisions locked for the initial slice

- SSAT-assembled HTML remains `Cache-Control: private, max-age=0` and strips
  runtime edge-cache headers (`Surrogate-Control`, `Fastly-Surrogate-Control`,
  `CDN-Cache-Control`, and `Cloudflare-CDN-Cache-Control`) whenever the ad stack
  can inject per-user slot/bid state.
- TSJS keeps the current `/static/tsjs=...js?v=<hash>` canonical URL shape.
  Matching hash/version requests receive immutable cache headers; missing or
  mismatched hash/version requests keep short TTLs rather than redirecting.
- Runtime cache-key configuration must preserve the `v` query parameter for
  `/static/tsjs=`. Fastly and Cloudflare include query strings in default cache
  keys, but project-specific query normalization must not drop `v`.
- Framework-specific immutable paths, including Next.js `/_next/static/*`, must
  be represented as configurable cache-rule presets. Do not add adapter- or
  proxy-level hard-coded framework path checks.
- Operators decide which framework presets and publisher allowlists are enabled.
  Arbitrary publisher CSS/JS/images remain origin-controlled unless an enabled
  cache rule proves they are immutable-safe.
- TS-owned Prebid delivery is covered by deferred TSJS module URLs. Publisher
  Prebid script URLs neutralized by TS are compatibility shims at stable URLs and
  must use `no-store` or a very short TTL, not a year-long immutable policy.
- Rehosted assets are TS-owned copies once TS rewrites/hosts them. They should
  use explicit normalized policies, with immutable only for TS-fingerprinted
  rehosted URLs.
- Fastly and Cloudflare are the MVP runtime targets. Akamai mapping is deferred
  until Akamai is on the roadmap.
- Dynamic HTML/RSC/API caching, dynamic `Vary`/cache-key normalization,
  origin-template caching, transformed-template caching, true publisher-origin
  streaming, parser-context bid splice, EdgeZero streaming parity, and SSAT HTML
  compression offload are deferred follow-up features.
- All personalized/cookie-bearing response hardening in `response_privacy.rs` and
  adapter middleware stays in place and runs after any new policy application.

## Original baseline before this implementation

| Area                         | Current file(s)                                           | Baseline                                                                                                          |
| ---------------------------- | --------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------- |
| TSJS URL injection           | `crates/trusted-server-core/src/tsjs.rs`                  | Injects `/static/tsjs=...js?v=<hash>`; current branch is moving hash work out of the hot path.                    |
| TSJS serving                 | `publisher.rs`, `http_util.rs`                            | Historically served through `serve_static_with_etag` with 5-minute browser/edge TTLs.                             |
| Cache policy primitives      | `cache_policy.rs`                                         | Current branch adds typed policy rendering; still needs final alignment with no-store and config rules.           |
| Neutralized Prebid shim      | `crates/trusted-server-core/src/integrations/prebid.rs`   | `handle_script_handler` currently returns an empty JS shim with `public, max-age=31536000`; this must be changed. |
| Cache privacy                | `publisher.rs`, `response_privacy.rs`, adapter middleware | Ad-stack HTML and cookie-bearing responses are downgraded to private/shared-uncacheable.                          |
| Rehosted asset cache policy  | `proxy.rs`                                                | Policy is effectively origin-controlled or `no-store, private`; no normalized immutable/SWR policy.               |
| Dynamic HTML/RSC/API caching | none                                                      | Deferred. No initial-slice `Vary` rewriting or Next router-header special casing.                                 |
| Origin-template cache        | none                                                      | Deferred. No cache API/override/template key/surrogate-key implementation exists.                                 |

## Definition of done for the initial slice

- TSJS hash-version-matching requests emit one-year immutable browser cache and
  one-year edge cache headers.
- TSJS missing/mismatched hash requests keep short TTL behavior.
- TSJS injected hash generation no longer concatenates and hashes the full
  bundle on every page view.
- Neutralized publisher Prebid shim responses use `no-store` or a very short TTL.
- Cache policy is represented as structured data and can emit Fastly
  `Surrogate-Control`, generic `CDN-Cache-Control`, Cloudflare-specific
  `Cloudflare-CDN-Cache-Control`, and `s-maxage` fallback headers.
- Cache policy can represent `no-store`/uncacheable responses as well as public
  and private TTL policies.
- TS config expresses static/rehosted cache policy through structured rules with
  match criteria, policy fields, and `enabled` flags.
- Built-in framework presets, including Next.js `/_next/static/*`, are
  implemented through the shared rule engine and can be disabled/overridden.
- Arbitrary publisher-origin assets remain origin-controlled unless matched by an
  enabled preset or publisher allowlist.
- TS-owned rehosted assets have explicit normalized policies instead of blindly
  passing through third-party defaults.
- MVP adapters emit the correct edge-cache header from shared policy: Fastly
  `Surrogate-Control`, Cloudflare `CDN-Cache-Control` /
  `Cloudflare-CDN-Cache-Control`, or portable `s-maxage` fallback.
- Deferred features are documented as deferred and are not accidentally
  implemented as hard-coded Next.js/dynamic-cache behavior.
- Tests and target-matched checks pass for touched crates/adapters.

## Proposed PR sequence

### PR 1 — Structured cache policy primitives

Status: implemented in the current branch.

#### Code changes

- Keep/add a core module such as `crates/trusted-server-core/src/cache_policy.rs`.
- Define structured policy types:
  - `CacheVisibility::{Public, Private}`
  - `CachePolicy { visibility, browser_ttl, edge_ttl, stale_while_revalidate,
stale_if_error, immutable }`
  - a `no-store` / uncacheable representation, either as a policy mode or a
    dedicated helper, so neutralized shims and error responses do not need
    ad-hoc strings;
  - `EdgeCacheHeader::{SurrogateControl, CdnCacheControl,
CloudflareCdnCacheControl, SMaxageFallback, None}`.
- Add helpers that render policy into headers:
  - browser `Cache-Control`
  - Fastly `Surrogate-Control`
  - generic `CDN-Cache-Control`
  - Cloudflare-specific `Cloudflare-CDN-Cache-Control`
  - portable `s-maxage` fallback.
- Keep helpers side-effect-limited: they should only mutate cache headers they
  own and should not bypass `response_privacy` hardening. When applying private
  or no-store policies, remove any existing edge-cache headers owned by the
  helper so stale `Surrogate-Control`/CDN cache headers cannot survive.
- Add default policy constructors/constants for:
  - immutable static;
  - short TSJS fallback;
  - neutralized Prebid shim (`no-store` or very short TTL);
  - uncacheable private.

#### Tests

- Unit-test exact header rendering for immutable, short edge/browser split,
  private, no-store, SWR/SIE, generic CDN, Cloudflare-specific CDN, and fallback
  `s-maxage` policies.
- Test that `immutable` is omitted when browser TTL is absent or zero.
- Test that edge-header output is disabled for private/no-store responses, and
  that applying private/no-store removes any pre-existing edge-cache header the
  helper owns.

### PR 2 — TSJS immutable hash-version serving

Status: implemented in the current branch with runtime-specific edge-header
selection.

#### Code changes

- Extend `crates/trusted-server-js/build.rs` generated metadata with per-module
  SHA-256 hashes.
- Update `trusted-server-js/src/bundle.rs`:
  - `single_module_hash(id)` returns generated hash instead of hashing content;
  - `concatenated_hash(ids)` hashes incrementally without concatenating a full
    `String`, or caches the result per normalized module-id set;
  - `concatenate_modules(ids)` can remain for serving the response body.
- Update `handle_tsjs_dynamic` in `publisher.rs`:
  - parse `?v=` from the request URI;
  - compare it with the canonical hash for the requested bundle;
  - if it matches, apply immutable static policy plus `Vary: Accept-Encoding`,
    ETag, and `X-Compress-Hint: on`;
  - if missing/mismatched, keep short TTL policy plus ETag and
    `X-Compress-Hint: on`.
- Keep the current canonical path shape (`/static/tsjs=...js?v=<hash>`).
- Document/verify that runtime cache-key configuration preserves the `v` query
  parameter for `/static/tsjs=`.

#### Tests

- `tsjs_script_src` and deferred script tests still produce `?v=<sha256>`.
- Matching `?v=` returns:
  - `Cache-Control: public, max-age=31536000, immutable`
  - runtime edge header via policy helper;
  - `Vary: Accept-Encoding`;
  - ETag.
- Missing/mismatched `?v=` returns short TTL and no `immutable`.
- Deferred disabled module still 404s.
- Hash helpers do not allocate the concatenated body just to hash it.

### PR 3 — Neutralized publisher Prebid shim cache safety

Fix the stable publisher Prebid compatibility route separately from TS-owned
Prebid delivery.

#### Code changes

- Update `PrebidIntegration::handle_script_handler` in
  `crates/trusted-server-core/src/integrations/prebid.rs`.
- Replace the current year-long `public, max-age=31536000` response with either:
  - `Cache-Control: no-store`, preferred for compatibility when integration
    enablement/config can change; or
  - a very short TTL if no-store is too conservative.
- Ensure no `Surrogate-Control`/CDN edge header is emitted for the neutralized
  stable URL.
- Keep TS-owned Prebid bundle delivery on the deferred TSJS module path, where
  matching `?v=<hash>` remains immutable.

#### Tests

- Neutralized Prebid script handler returns the empty compatibility script with
  `no-store` or the chosen short TTL.
- Neutralized Prebid shim does not emit immutable or year-long cache headers.
- TSJS deferred Prebid still receives immutable headers when `?v=` matches.

### PR 4 — Configurable static asset cache-rule engine

Introduce operator-configurable static asset rules before applying immutable
upgrades to publisher-origin assets.

#### Code changes

- Add cache-rule settings rather than hard-coded path checks. Suggested shape:
  - `CacheAssetRule { id, enabled, matcher, policy }`
  - `CacheAssetMatcher::{PathPrefix, Glob, Regex, Extension, Preset}`
  - `CacheAssetPreset::NextJsStatic` expands to `/_next/static/*` when enabled.
- Add cache settings under `Settings` (and `trusted-server.example.toml`) with
  `#[serde(deny_unknown_fields)]` validation consistent with the rest of the
  config model.
- Add a shared rule evaluator with deterministic precedence. Prefer an ordered
  rule list where the first enabled match wins; reject duplicate rule IDs and
  invalid matcher combinations during settings validation.
- Ship framework presets as data/config defaults or documented examples, not as
  special cases in proxy/adapters.
- The Next.js preset may be present in example config, but operators must be able
  to disable/override it. Do not silently apply it through a hard-coded branch.
- Support publisher-defined allowlist rules for other frameworks or
  publisher-specific fingerprinted paths.
- Apply immutable policy only when an enabled rule/preset says the URL is
  content-addressed, or for TS-owned validated hash URLs such as TSJS.

#### Tests

- With the Next.js preset enabled, `/_next/static/*` gets immutable policy.
- With the Next.js preset disabled, the same `/_next/static/*` remains
  origin-controlled.
- Publisher-defined allowlist rule can mark a non-Next fingerprinted path
  immutable.
- Non-matching publisher asset remains origin-controlled.
- Rule precedence is deterministic.
- Invalid regex/glob/config fails validation clearly.

### PR 5 — MVP runtime edge-header mapping and docs

Make the shared policy output explicit per runtime before wiring the rule engine
into more routes. This prevents new code from copying the current core
Fastly-specific `Surrogate-Control` behavior.

#### Code changes

- Stop requiring core helpers such as `handle_tsjs_dynamic` or
  `serve_static_with_etag` to hard-code Fastly's `Surrogate-Control`.
- Choose one adapter boundary pattern and use it consistently:
  - pass the runtime `EdgeCacheHeader`/policy emitter into core handlers; or
  - return cache-policy metadata in response extensions and let adapters render
    runtime-specific headers after route handling.
- Fastly adapter emits `Surrogate-Control` for edge TTLs.
- Cloudflare adapter emits `CDN-Cache-Control` or
  `Cloudflare-CDN-Cache-Control`, depending on the chosen adapter convention.
- Portable/local fallback can use `s-maxage` inside `Cache-Control` when no
  runtime-specific edge header is available.
- Akamai mapping remains absent/deferred; do not add untested Akamai behavior.
- Update `trusted-server.example.toml` and docs with disabled framework preset
  examples and operator-owned allowlist examples.

#### Tests

- Fastly TSJS/static policy application emits `Surrogate-Control`.
- Cloudflare TSJS/static policy application emits the selected Cloudflare CDN
  cache header and does not emit Fastly-only `Surrogate-Control`.
- Fallback policy emits `s-maxage` only for public/shared-cacheable responses.
- Private/no-store responses remove or avoid all edge-cache headers.

### PR 6 — Apply static/rehosted policies to proxy responses

Wire the rule engine into the routes that emit publisher-origin or rehosted
assets, using the runtime edge-header mapping from PR 5.

#### Code changes

- Extend `AssetProxyCachePolicy` in `proxy.rs` beyond
  `OriginControlled`/`NoStorePrivate`, for example:
  - `OriginControlled`
  - `NoStorePrivate`
  - `Normalized(CachePolicy)` from a matched enabled rule.
- Apply normalized policy after route finalization but before final response
  privacy hardening.
- Preserve existing no-store/private handling for errors, signed failures, or
  responses that set cookies/security headers.
- Ensure operator `response_headers` cannot weaken protected private/no-store
  decisions.
- For TS-owned rehosted copies:
  - use immutable only for fingerprinted TS-owned URLs;
  - use conservative edge/browser TTLs for stable rehosted URLs;
  - keep dynamic/personalized endpoints uncached.

#### Tests

- Rehosted/fingerprinted route matched by an enabled rule gets immutable policy.
- Stable rehosted route gets the configured conservative policy, not a borrowed
  third-party `no-store` unless configured.
- Rehosted error responses keep `no-store, private`.
- `Set-Cookie` response remains private/no-store and loses surrogate headers.
- Operator response headers cannot re-enable shared caching for protected
  responses.

## Initial config sketch

Exact names can change during implementation, but keep the shape structured and
operator-controlled.

```toml
[[cache.asset_rules]]
id = "nextjs-static"
enabled = false # operators may enable for Next.js publishers
preset = "nextjs-static"
visibility = "public"
browser_ttl_seconds = 31536000
edge_ttl_seconds = 31536000
immutable = true

[[cache.asset_rules]]
id = "publisher-fingerprinted-assets-example"
enabled = false
path_globs = [
  "/assets/**/*.js",
  "/assets/**/*.css",
  "/assets/**/*.png",
  "/assets/**/*.jpg",
  "/assets/**/*.webp",
  "/assets/**/*.avif",
]
requires_hash_in_filename = true
visibility = "public"
browser_ttl_seconds = 31536000
edge_ttl_seconds = 31536000
immutable = true

[cache.tsjs.versioned]
visibility = "public"
browser_ttl_seconds = 31536000
edge_ttl_seconds = 31536000
immutable = true

[cache.tsjs.fallback]
visibility = "public"
browser_ttl_seconds = 300
edge_ttl_seconds = 300
stale_while_revalidate_seconds = 60
stale_if_error_seconds = 86400

[cache.prebid_neutralized]
mode = "no-store"
```

Defaults should preserve current behavior unless a rule is explicitly enabled or
unless the response is TS-owned and hash-validated, such as TSJS.

## Deferred follow-up backlog

These remain valuable, but are intentionally outside the initial cache-header
slice.

| Follow-up                                           | Why deferred                                                                                                                                       | Notes                                                                                                                          |
| --------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------ |
| True publisher-origin streaming                     | Requires platform/body boundary changes and adapter streaming semantics                                                                            | Includes avoiding full `take_body_bytes()` materialization on Fastly and documenting/implementing non-Fastly streaming parity. |
| Parser-context bid splice                           | Requires HTML pipeline redesign                                                                                                                    | Replace raw `</body` byte hold with parser-confirmed placeholder/late-bind logic.                                              |
| SSAT HTML compression offload                       | Requires origin behavior tests and CPU/wire-size measurements                                                                                      | Target shape remains `Accept-Encoding: identity` origin fetch plus Fastly `X-Compress-Hint: on`.                               |
| Explicit origin-template cache                      | Requires cache-key design, cookie/header normalization, runtime cache API support, request collapsing, SWR, purge hooks, and streaming integration | Keep assembled SSAT HTML private.                                                                                              |
| Transformed, auction-independent template cache     | Depends on origin-template cache and safe bid/slot late binding                                                                                    | Cache only deterministic template transformation, never per-user bid data.                                                     |
| Dynamic HTML/RSC/API caching and Vary normalization | Needs production cardinality/body-impact data and generic configurable cache-key rules                                                             | Do not hard-code Next.js router headers in the initial slice.                                                                  |
| JSON/API compression normalization                  | Belongs with dynamic-response policy unless handled by a separate explicit API policy                                                              | Add `Vary: Accept-Encoding` when compression is applied.                                                                       |
| Arbitrary publisher asset audit                     | Base policy defers to origin; audit can identify optional allowlist opportunities                                                                  | Output should be operator-owned cache rules.                                                                                   |
| Akamai edge-cache mapping                           | Akamai is not on the MVP roadmap and depends on Property Manager config                                                                            | Revisit with Akamai staging/property tests.                                                                                    |

## Measurement plan

Initial slice:

- TSJS conditional GET rate before/after immutable versioned serving.
- TSJS response header sampling for matching/mismatched `?v=`.
- Neutralized Prebid shim response header sampling.
- Static/rehosted rule hit counts and bypass reasons.

Deferred follow-ups own their own measurement:

- SSAT compression CPU/wire-size measurement.
- Streaming origin-first-byte/client-first-byte/hold-duration measurement.
- Template cache hit/miss/stale/collapsed-fill and purge-key coverage.
- Dynamic cache-key cardinality and hit rate by object class.

## Verification commands

Run target-matched checks for touched code; do not use bare
`cargo test --workspace`.

Minimum per PR:

```bash
cargo fmt --all -- --check
cargo test-fastly
cargo test-axum
cargo test-cloudflare
cargo test-spin
```

Before handoff for code touching cache policy, publisher processing, or adapter
headers:

```bash
cargo clippy-fastly
cargo clippy-axum
cargo clippy-cloudflare
cargo clippy-spin-native
cargo clippy-spin-wasm
```

When touching TSJS build or JS bundles:

```bash
cd crates/trusted-server-js/lib && npx vitest run
cd crates/trusted-server-js/lib && node build-all.mjs
```

When touching docs:

```bash
cd docs && npm run format
```

## Risk controls

- Ship TSJS immutable behavior first because it is already content-versioned.
- Keep short-TTL fallback for any request whose version/hash cannot be verified.
- Preserve the `v` query parameter in any cache-key/query-normalization config
  for `/static/tsjs=`.
- Use configurable asset rules for framework/publisher immutable upgrades; do
  not hard-code framework paths in adapters or proxy code.
- Leave arbitrary publisher-origin assets origin-controlled unless an enabled
  operator rule proves they are immutable-safe.
- Run response privacy hardening after policy application in every adapter.
- Add tests that assert `Set-Cookie` plus public/surrogate headers cannot escape.
- Prefer missing optimization over unsafe shared caching: if eligibility is
  uncertain, bypass the cache or leave origin-controlled.
- Keep dynamic caching, template caching, streaming, and compression offload out
  of the initial cache-header slice.
