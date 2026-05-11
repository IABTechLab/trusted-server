# Multi-backend Asset Proxy Design

> Proposed design for path-based first-party asset proxy routing.
> Date: 2026-04-28.

---

## Goal

Allow Trusted Server to proxy selected first-party asset paths to a different
backend origin than `publisher.origin_url`.

Example:

- incoming URL: `https://www.example.com/.images/foo.jpg?w=1200`
- matched rule: `prefix = "/.images/"`
- asset origin: `https://some.fastly-service.com`
- upstream URL: `https://some.fastly-service.com/.images/foo.jpg?w=1200`

This should happen transparently for normal inbound requests, without requiring
`/first-party/proxy` signed URLs.

---

## Problem

Today, unknown routes fall through to the publisher proxy path and always go to
one backend:

- `settings.publisher.origin_url`

That works for HTML and general publisher-origin traffic, but it does not allow
specific first-party asset namespaces to be served by a separate backend such
as an image CDN, Fastly service, or dedicated asset origin.

Publishers need to keep asset URLs on their first-party domain while routing
certain path prefixes to a different backend.

---

## Scope

### In scope

- Path-prefix-based routing for first-party asset requests
- Multiple configured asset-route rules
- Per-rule alternate `origin_url`
- Transparent proxying for ordinary inbound `GET`/`HEAD` requests
- Preservation of the incoming path and query string by default
- Optional regex-based path rewrite after prefix route selection
- Raw response pass-through from the matched asset origin, except unsafe publisher-domain state/security headers
- Deterministic longest-prefix route selection
- Request routing that happens after built-in and integration routes, but
  before publisher-origin fallback

### Out of scope

- Regex-based route selection
- Cookie, consent, HTML, CSS, or JS rewriting on asset-route responses
- Redirect following for asset routes
- Special cache policy overrides
- Non-`GET` / non-`HEAD` methods
- Per-route header customization
- Health checks, fallback chains, or origin failover

---

## Product Requirements

### 1. Transparent inbound routing

The feature applies to normal inbound requests handled by Trusted Server.

It is **not** an extension of `/first-party/proxy` and does **not** require URL
signing.

If an incoming request path matches a configured asset route, Trusted Server
proxies it directly to that route's configured origin.

### 2. Match on simple path prefixes

Routes are selected by simple prefixes, not regexes. Optional regexes may rewrite the upstream path only after a prefix route has already matched.

Examples:

- valid: `/.images/`
- valid: `/static/`
- invalid: `.images/`
- invalid: `images/`

Rule matching is performed against the request path only. Query strings are
ignored for matching.

### 3. Preserve path and query by default, with optional path rewrite

When a rule matches without rewrite settings, Trusted Server replaces only the
upstream origin (scheme/host/port) and preserves the rest of the request URL
exactly.

Example:

- inbound: `/.images/foo/bar.jpg?auto=webp&width=1200`
- upstream path/query: `/.images/foo/bar.jpg?auto=webp&width=1200`

Routes may optionally configure `path_pattern` and `target_path` together. In
that case, `path_pattern` is matched against the incoming request path after the
prefix route has been selected, and `target_path` is used as the regex
replacement for the upstream path. The incoming query string is still preserved.

Example:

- inbound: `/.images/foo/bar.jpg?auto=webp&width=1200`
- `path_pattern`: `^/\.images/(.*)$`
- `target_path`: `/cdn/$1`
- upstream path/query: `/cdn/foo/bar.jpg?auto=webp&width=1200`

### 4. Multiple rules supported

Configuration supports multiple asset-route entries.

Example use cases:

- `/.images/` → image CDN
- `/static/assets/` → static asset backend
- `/_next/image/` → specialized image transformer

### 5. Longest matching prefix wins

If multiple routes match a path, the most specific route wins.

Example:

- `/.images/` → backend A
- `/.images/special/` → backend B
- request `/.images/special/x.jpg` → backend B

### 6. Only `GET` and `HEAD`

Asset-route matching only applies to `GET` and `HEAD` requests.

All other methods continue through existing route handling and publisher
fallback behavior unchanged.

### 7. Explicit routes win first

Built-in Trusted Server routes and registered integration routes must retain
higher precedence than asset-route matching.

Asset routes act only inside the fallback proxy space. They must not shadow:

- `/auction`
- `/first-party/*`
- `/.well-known/*`
- admin routes
- registered integration routes

### 8. Raw pass-through behavior

Matched asset routes bypass the publisher-page processing pipeline.

Specifically, asset-route handling does **not** perform:

- EC generation / consent pipeline work
- cookie mutation
- HTML rewriting
- CSS rewriting
- URL rewriting
- RSC processing
- post-processing
- redirect following

The route behaves as a lean transport proxy.

### 9. Upstream errors are not masked

If the matched asset origin returns a response, that response is returned to the
client as-is.

If the asset origin cannot be reached or backend setup fails, Trusted Server
returns the existing error behavior for that failure class.

It must **not** silently fall back to `publisher.origin_url`.

### 10. Preserve upstream cache semantics

Trusted Server passes through upstream cache headers unchanged, including:

- `Cache-Control`
- `ETag`
- `Last-Modified`
- `Expires`
- `Vary`

There is no v1 cache override layer.

### 11. Preserve redirect semantics

If the asset origin returns a redirect (`301`, `302`, `303`, `307`, `308`),
Trusted Server returns that redirect to the client as-is.

It does not follow redirects server-side.

### 12. Preserve `HEAD` semantics

A `HEAD` request to a matched asset route is proxied upstream as `HEAD` and
returned without body synthesis.

---

## Configuration Design

Asset routes live under `[proxy]` in `trusted-server.toml`.

### Proposed shape

```toml
[proxy]
certificate_check = true

[[proxy.asset_routes]]
prefix = "/.images/"
origin_url = "https://some.fastly-service.com"

[[proxy.asset_routes]]
prefix = "/static/assets/"
origin_url = "https://assets.example.net"
```

### Field definitions

#### `prefix`

- required
- string
- must start with `/`
- matched against the request path only
- case-sensitive, using normal request-path semantics

#### `origin_url`

- required
- string
- absolute `http` or `https` URL
- must not include a trailing slash
- used as the upstream scheme/host/port base
- request query is preserved from the incoming request
- request path is preserved unless `path_pattern` / `target_path` rewrite it

#### `path_pattern`

- optional
- string
- regex matched against the incoming request path after prefix route selection
- must be configured together with `target_path`
- does not participate in route selection

#### `target_path`

- optional
- string
- regex replacement applied to `path_pattern` matches
- must be configured together with `path_pattern`
- replacement output must start with `/`

### Validation rules

#### Hard validation errors

These should fail configuration loading:

- `prefix` missing
- `prefix` does not start with `/`
- `origin_url` missing
- `origin_url` is not an absolute `http`/`https` URL
- `origin_url` has a trailing slash
- `path_pattern` is configured without `target_path`, or vice versa
- `path_pattern` does not compile as a regex
- `target_path` rewrite output does not start with `/`

#### Warning-only validation

Duplicate exact prefixes should not fail startup.

Instead:

- log a warning for later duplicates
- keep behavior deterministic
- exact duplicate prefixes use the **first configured rule**

This preserves production availability while surfacing misconfiguration.

---

## Proposed Data Model

Add a new route type under proxy settings.

```rust
pub struct ProxyAssetRoute {
    pub prefix: String,
    pub origin_url: String,
}

pub struct Proxy {
    pub certificate_check: bool,
    pub allowed_domains: Vec<String>,
    pub asset_routes: Vec<ProxyAssetRoute>,
}
```

### Runtime helper behavior

A helper should normalize and validate asset routes during settings preparation.

Recommended responsibilities:

- validate each route
- warn on duplicate exact prefixes
- provide longest-prefix matching for a path
- provide deterministic duplicate behavior

---

## Request Routing Design

### Current baseline

Today the top-level request router behaves roughly as follows:

1. match built-in routes
2. match integration routes
3. otherwise proxy to `publisher.origin_url`

### Proposed routing order

1. match built-in Trusted Server routes
2. match integration routes
3. if method is `GET` or `HEAD`, try asset-route match
4. if asset route matched, proxy to that asset origin
5. otherwise fall through to existing publisher-origin proxy path

### Why this placement

This preserves current application route behavior while allowing targeted
origin overrides for fallback asset paths.

Asset routes should not become a general-purpose top-level router that can
interfere with core product endpoints.

---

## Matching Algorithm

### Inputs

- HTTP method
- request path
- configured `asset_routes`

### Matching rules

1. Ignore all asset routes unless method is `GET` or `HEAD`
2. Compare request path against each configured `prefix`
3. A route matches when `request_path.starts_with(prefix)`
4. Select the match with the longest `prefix`
5. If multiple routes have the same exact prefix, the first configured route
   wins and later duplicates only warn

### Examples

#### Example 1: simple match

Rules:

- `/.images/` → `https://img.fastly.example`

Request:

- `GET /.images/photo.jpg?w=1000`

Result:

- proxy to `https://img.fastly.example/.images/photo.jpg?w=1000`

#### Example 2: longest prefix

Rules:

- `/.images/` → A
- `/.images/special/` → B

Request:

- `GET /.images/special/banner.png`

Result:

- route B wins

#### Example 3: wrong method

Rules:

- `/.images/` → A

Request:

- `POST /.images/upload`

Result:

- no asset-route match; continue existing routing behavior

---

## Proxy Behavior

### Upstream URL construction

For a matched asset route:

1. take the matched rule's `origin_url`
2. preserve the incoming request path exactly
3. preserve the incoming query string exactly
4. build the upstream request URL from those components

Example:

- origin: `https://some.fastly-service.com`
- path: `/.images/foo.jpg`
- query: `auto=webp&width=800`
- upstream: `https://some.fastly-service.com/.images/foo.jpg?auto=webp&width=800`

### Backend selection

The route should use the existing dynamic-backend mechanism already used
elsewhere in Trusted Server.

Backend creation should be derived from the matched `origin_url` and
`settings.proxy.certificate_check`.

### Host header

The upstream `Host` header must be set to the matched asset origin host,
not the original first-party host.

This is necessary for CDN and origin correctness.

### Method forwarding

- incoming `GET` → upstream `GET`
- incoming `HEAD` → upstream `HEAD`

No method rewriting.

### Header forwarding

Forward a minimal curated set of request headers, aligned with existing proxy
helper behavior where possible.

Recommended v1 header set:

- `Accept`
- `Accept-Encoding`
- `Accept-Language`
- `User-Agent`
- `Referer`
- `X-Forwarded-For`

Avoid broad header tunneling in v1.

### Redirects

Do not follow redirects.

If upstream returns a redirect, return it to the client.

### Response handling

Treat the response as raw pass-through except for publisher-domain state and
security headers that asset origins must not control:

- preserve status code
- preserve response body bytes
- preserve response headers, including cache headers
- strip `Set-Cookie`
- strip `Strict-Transport-Security`
- strip `Clear-Site-Data`
- do not inspect content type for rewriting
- do not run creative, HTML, CSS, or RSC processors

---

## Interaction with Existing Publisher Proxy

The existing publisher proxy path is HTML-aware and consent-aware. It includes:

- cookie parsing
- EC generation / forwarding
- consent context construction
- response rewriting and post-processing
- origin fallback through `publisher.origin_url`

The new asset-route path is intentionally separate.

### Design principle

Use the publisher proxy for pages and general publisher-origin traffic.
Use asset-route proxying for configured static/asset namespaces.

This separation keeps the asset path lean and avoids introducing page-proxy
behavior into CDN-style traffic.

---

## Failure Semantics

### Upstream returns HTTP response

Return it as-is.

Examples:

- `404 Not Found` → return `404`
- `500 Internal Server Error` → return `500`
- `302 Found` → return `302`

### Upstream unreachable / backend failure

Return the normal Trusted Server error behavior for backend/proxy failure.

Do **not** retry against `publisher.origin_url`.
Do **not** silently fall back.

### Misconfiguration

- invalid `prefix` / invalid `origin_url` → configuration error
- duplicate exact `prefix` → warning only

---

## Observability

At minimum, log enough information to diagnose routing decisions.

Recommended log points:

- asset route matched: request path, matched prefix, target origin
- duplicate exact prefix detected at startup
- asset proxy backend creation failure
- asset upstream request failure
- asset route skipped due to unsupported method

Logging should use the project's normal `log` macros.

---

## Security Considerations

### 1. Limited scope

This feature is not an arbitrary open proxy. It only routes to origins that are
statically configured in `trusted-server.toml`.

### 2. No redirect following

Returning redirects as-is avoids introducing redirect-chain SSRF concerns for
this feature.

### 3. Minimal header forwarding

Forwarding a curated header set reduces risk from hop-by-hop headers or
unexpected application headers being tunneled upstream.

### 4. No signed-URL trust expansion

This feature does not reuse `/first-party/proxy` URL-signing behavior. It is a
separate static routing mechanism.

### 5. Asset origins cannot mutate publisher browser state

Because responses are served on the publisher first-party domain, asset origins
must not be able to set cookies, alter transport-security policy, or clear
publisher storage. Asset responses therefore strip `Set-Cookie`,
`Strict-Transport-Security`, and `Clear-Site-Data`.

---

## Acceptance Criteria

### Configuration

- `trusted-server.toml` accepts `[[proxy.asset_routes]]`
- each route requires `prefix` and `origin_url`
- invalid `prefix` fails config load
- invalid `origin_url` fails config load
- duplicate exact prefixes log warnings but do not fail startup

### Routing

- built-in routes still win over asset routes
- integration routes still win over asset routes
- asset routes are evaluated before publisher-origin fallback
- only `GET` and `HEAD` requests participate
- longest matching prefix wins
- exact duplicate prefixes resolve deterministically to the first configured rule

### Proxy semantics

- matched requests preserve path and query exactly unless optional rewrite settings change the path
- matched requests use the asset origin's scheme/host/port
- upstream `Host` header matches asset origin host
- redirects are returned to the client, not followed
- cache headers pass through unchanged
- no fallback to `publisher.origin_url` on asset origin failure
- `HEAD` remains `HEAD`

### Response processing

- matched asset routes bypass publisher consent/cookie/rewriting logic
- matched asset routes behave as raw pass-through except unsafe publisher-domain state/security headers are stripped

---

## Recommended Tests

### Settings tests

- parses multiple `[[proxy.asset_routes]]` entries
- rejects prefix without leading `/`
- rejects `origin_url` with trailing slash
- rejects non-absolute `origin_url`
- warns on duplicate exact prefixes
- rejects invalid `path_pattern` / `target_path` combinations

### Route-selection tests

- no match for unsupported method
- match by prefix
- longest-prefix wins
- exact duplicate prefix resolves to first rule
- query string does not affect matching

### Adapter/router tests

- built-in route precedence over asset route
- integration route precedence over asset route
- unmatched path still falls through to publisher proxy

### Proxy-construction tests

- path preserved exactly without rewrite settings
- path rewritten when `path_pattern` and `target_path` are configured
- query preserved exactly
- upstream host header uses asset origin host
- `HEAD` preserved
- redirect response returned as-is

---

## Implementation Notes

A minimal implementation should avoid changing the existing publisher proxy
behavior more than necessary.

Recommended implementation outline:

1. Add `ProxyAssetRoute` and `Proxy.asset_routes` to settings
2. Add normalization / validation / duplicate-warning logic
3. Add a path-matching helper that selects the longest prefix
4. Add a lean asset-proxy handler that:
   - builds a backend from matched `origin_url`
   - preserves path + query by default
   - applies optional path rewrite while preserving query
   - forwards a minimal header set
   - does not follow redirects
   - returns raw upstream response after stripping unsafe publisher-domain state/security headers
5. Insert asset-route handling into top-level routing after explicit routes and
   before publisher fallback
6. Add focused tests for config, matching, precedence, and proxy construction

---

## Future Extensions

Potential future work, intentionally excluded from v1:

- regex route selection
- per-route custom headers
- per-route cache overrides
- per-route certificate-check options
- per-route method allowlists
- route metrics / counters
- fallback chains across multiple origins

---

## Open Questions

None blocking for v1.

The only follow-up item already identified is broader project-wide work to make
misconfiguration handling more consistent across Trusted Server, but that is not
required to implement this feature.
