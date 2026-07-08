# Define Cache-Control strategy for TS edge environment

**Labels:** `enhancement`, `edge`, `performance`, `caching`
**Area:** Trusted Server runtime (Fetch from Origin / Edge to Clients)

## Summary

Trusted Server needs a structured cache policy that separates browser caching from edge/shared caching. Today several TS-owned responses use short, hard-coded cache headers even when the URL is content-versioned. The initial implementation should focus on TS-owned and explicitly fingerprinted assets, especially TSJS bundles.

Dynamic HTML/template caching, streaming fixes, SSAT compression offload, and Akamai-specific cache behavior are follow-up work and are not part of this cache-header slice.

## Goals

- Express cache policy as structured data instead of ad hoc header strings.
- Independently control browser TTL and edge/shared-cache TTL.
- Serve TSJS bundles with immutable caching when the requested URL hash matches the served bytes.
- Keep non-versioned or config-dependent stable URLs out of long-lived immutable browser caches.
- Provide configurable cache-rule presets for known fingerprinted framework paths, starting with Next.js `/_next/static/*`.
- Keep SSAT-assembled ad-stack HTML private and out of shared caches.
- Emit the correct MVP runtime edge-cache headers from the shared policy.

## Non-goals for this slice

These are tracked separately:

- True SSAT publisher streaming and parser-safe assembly: #857.
- SSAT HTML compression offload via `Accept-Encoding: identity` and `X-Compress-Hint`: #858.
- Origin-template caching, transformed-template caching, and dynamic HTML/RSC/API cache-key design: #859.
- Akamai-specific `CDN-Cache-Control` / `Edge-Control` / Property Manager behavior.

## Background: two cache tiers

The request path has two cacheable hops:

```text
Origin ──▶ TS edge/shared cache ──▶ Browser cache
```

- **Edge/shared cache:** controlled by `s-maxage` or runtime-specific edge headers.
  - Fastly: `Surrogate-Control`
  - Cloudflare: `CDN-Cache-Control` / `Cloudflare-CDN-Cache-Control`
  - Portable fallback: `s-maxage` inside `Cache-Control`
- **Browser cache:** controlled by `max-age` and related `Cache-Control` directives.

A single `max-age` cannot express “hold at the edge for a year, but revalidate in the browser daily” or the reverse. TS should model these tiers separately and let adapters render the appropriate headers.

## Policy model

Add a shared cache-policy model along these lines:

```text
match
edge_ttl
browser_ttl
stale_while_revalidate
stale_if_error
immutable
visibility: public | private
enabled
```

Rules should be configurable. Built-in framework presets, such as Next.js `/_next/static/*`, should be represented as default rules in this same model rather than hard-coded in adapters. Operators must be able to disable or override presets and add publisher-specific allowlists.

## Target behavior by response class

| Response class                                                  | Target policy                                                                 | Notes                                                                                                              |
| --------------------------------------------------------------- | ----------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------ |
| TSJS with matching `?v=<hash>`                                  | `Cache-Control: public, max-age=31536000, immutable` plus runtime edge header | The serving path must validate that `v` matches the bytes served.                                                  |
| TSJS missing/mismatched `?v=`                                   | Short TTL or redirect to canonical hashed URL                                 | Do not mark immutable.                                                                                             |
| TSJS deferred modules, including Prebid                         | Same as TSJS hash-matching policy                                             | Example: `/static/tsjs=tsjs-prebid.min.js?v=<hash>`.                                                               |
| Publisher Prebid URL neutralized by TS                          | `no-store` or very short TTL                                                  | The empty compatibility shim is config-dependent and served at a stable publisher URL. Do not cache it for a year. |
| Enabled framework preset static, e.g. Next.js `/_next/static/*` | `Cache-Control: public, max-age=31536000, immutable`                          | Applied through configurable preset/allowlist rules.                                                               |
| TS-fingerprinted rehosted asset                                 | `Cache-Control: public, max-age=31536000, immutable`                          | Safe only when TS owns the fingerprinted URL.                                                                      |
| Stable TS-owned/rehosted URL                                    | Conservative short browser TTL, optional longer edge TTL                      | Do not use `immutable`.                                                                                            |
| Arbitrary publisher-origin CSS/JS/images                        | Origin-controlled by default                                                  | TS may upgrade only via enabled framework preset or publisher allowlist.                                           |
| SSAT-assembled ad-stack HTML                                    | `Cache-Control: private, max-age=0`; strip runtime edge-cache headers         | Must never enter shared cache because it can contain per-user slot/bid data.                                       |
| Dynamic HTML/RSC/API                                            | Origin-controlled in this slice                                               | Future dynamic caching belongs to #859.                                                                            |

## TSJS-specific requirements

Current TSJS URLs already include a content hash query string, for example:

```text
/static/tsjs=tsjs-unified.min.js?v=<hash>
/static/tsjs=tsjs-prebid.min.js?v=<hash>
```

The current serving path still emits a short cache policy. Update it so that:

- hash-matching requests emit one-year immutable browser caching;
- hash-matching requests emit the runtime edge header with equivalent long edge TTL;
- missing or mismatched hash requests do not receive immutable caching;
- cache-key configuration preserves the `v` query parameter;
- TSJS hashes used in injected URLs are generated at build time or cached so HTML injection does not re-concatenate and re-hash large bundles per pageview;
- `Vary: Accept-Encoding` remains on compressed/static responses;
- ETags may remain as a fallback for clients or intermediaries that revalidate anyway.

Fastly and Cloudflare include query strings in default cache keys, but TS must still avoid any project-specific query normalization that drops `v` for `/static/tsjs=`.

## SSAT HTML privacy requirement

SSAT-assembled ad-stack HTML can contain per-user data such as slot state or bid data. It must remain:

```http
Cache-Control: private, max-age=0
```

and must strip runtime edge-cache headers, including:

```http
Surrogate-Control
Fastly-Surrogate-Control
CDN-Cache-Control
Cloudflare-CDN-Cache-Control
```

This requirement applies to the browser-facing assembled response. Origin-template caching is separate follow-up work in #859.

## Runtime header mapping for MVP

Adapters should render the shared policy as follows:

| Runtime           | Edge/shared-cache header                             |
| ----------------- | ---------------------------------------------------- |
| Fastly            | `Surrogate-Control`                                  |
| Cloudflare        | `CDN-Cache-Control` / `Cloudflare-CDN-Cache-Control` |
| Portable fallback | `s-maxage` in `Cache-Control`                        |

Akamai mapping is deferred until Akamai is on the roadmap.

## Acceptance criteria

- [ ] Cache policy is represented as structured fields, not hard-coded header strings.
- [ ] Built-in framework presets, including a disableable/overrideable Next.js `/_next/static/*` rule, are implemented through the shared cache-policy rule engine.
- [ ] TSJS hash-matching requests for unified and deferred modules emit `public, max-age=31536000, immutable` plus the runtime edge header.
- [ ] TSJS missing/mismatched hash requests do not get immutable caching.
- [ ] TSJS hash generation is build-time or cached enough that HTML injection does not re-concatenate/re-hash the bundle per pageview.
- [ ] Runtime cache-key configuration preserves the `v` query parameter for `/static/tsjs=`.
- [ ] Neutralized publisher Prebid shim responses use `no-store` or a short TTL, not a year-long policy.
- [ ] Arbitrary publisher-origin assets remain origin-controlled unless covered by an enabled framework preset or publisher allowlist.
- [ ] TS-owned rehosted assets have explicit normalized cache policy; immutable is used only for TS-fingerprinted rehosted URLs.
- [ ] SSAT-assembled ad-stack HTML continues to emit `private, max-age=0` and strips all runtime edge-cache headers.
- [ ] Fastly and Cloudflare adapters emit the correct edge-cache header from the shared policy, with portable `s-maxage` fallback where needed.
- [ ] Dynamic HTML/RSC/API Vary/cache-key normalization is not hard-coded in this PR and is deferred to #859.
- [ ] SSAT streaming fixes and compression offload are not included in this PR and remain tracked by #857 and #858.
