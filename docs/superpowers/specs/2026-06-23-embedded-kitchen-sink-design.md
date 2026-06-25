# Embedded Kitchen Sink Site

**Date:** 2026-06-23
**Status:** Proposed

## Problem

Trusted Server currently has a separate static kitchen-sink site deployed as a
Cloudflare Pages project from
`/Users/christian/Projects/stackpop/trusted-server-coolify/static-site`. The
site is useful for exercising auction, Prebid, identity, and basic browser
fixture flows, but it depends on external Cloudflare Pages hosting and must be
configured as the publisher origin to test the full Trusted Server path.

We want the same diagnostic fixture to be available directly inside Trusted
Server, with no Cloudflare Pages dependency, and enabled by operator
configuration. The embedded site should live under a Trusted Server-owned path
and exercise the same HTML head injection and integration rewriting behavior as
publisher pages.

## Goals

- Embed the kitchen-sink static site in the Trusted Server build.
- Serve it from `/_ts/kitchen-sink/` when explicitly enabled by config.
- Keep the feature disabled by default.
- Remove the Cloudflare Pages runtime dependency.
- Process all kitchen-sink HTML through the Trusted Server HTML processing
  pipeline so TSJS injection, head inserts, script replacement, and integration
  HTML behavior can be tested.
- Serve non-HTML assets raw.
- Keep implementation platform-neutral where practical by putting handler logic
  in `trusted-server-core` and keeping Fastly adapter changes limited to route
  dispatch wiring.
- Make the static bundle easy to grow without manually editing an asset list for
  every new file.

## Non-goals

- No standalone Cloudflare Pages deployment path in v1.
- No Node, Wrangler, or local standalone preview tooling in v1.
- No server-side templating or dynamic config rendering into the pages.
- No SPA-style fallback routing.
- No new authentication layer beyond the explicit config flag.
- No Cargo feature gate in v1; the bundle is compiled in and exposure is
  controlled by runtime config.
- No publisher-origin network fetch for the embedded site.

## Decisions from Design Discussion

1. **Mount path:** serve the site under `/_ts/kitchen-sink/`, not `/_ts/debug/`,
   to avoid colliding semantically or technically with endpoints such as
   `/_ts/debug/ja4`.
2. **Config:** add `debug.kitchen_sink_enabled`, defaulting to `false`.
3. **Crate split:** add a separate asset crate named
   `trusted-server-kitchen-sink`.
4. **Handler location:** put request handling and HTML processing integration in
   `trusted-server-core`; the Fastly adapter only wires dispatch.
5. **Static bundle:** migrate the current site into a smaller v1 fixture, with
   only the pages needed for auction, Prebid, and identity checks.
6. **Path style:** make site-local navigation and asset references relative so
   the site works below `/_ts/kitchen-sink/`. Keep real Trusted Server endpoint
   probes absolute, such as `/auction` and `/_ts/set-tester`.
7. **HTML processing:** process every `.html` page through the Trusted Server
   HTML pipeline. Serve CSS, JS, and other non-HTML assets raw.
8. **Origin context:** use the configured publisher origin host as the HTML
   processor's origin context and the actual inbound request host/scheme as the
   request context.
9. **EC/finalization:** keep the kitchen-sink route inside the normal dispatch
   flow so pre-route filters and response finalization apply. Mirror publisher
   navigation EC generation where practical.
10. **Dispatch precedence:** detect kitchen-sink paths inside fallback dispatch
    after `/static/tsjs=...` and registered integration routes, but before
    `proxy.asset_routes` and publisher fallback.
11. **Methods:** support only `GET` and `HEAD` for kitchen-sink assets. The site
    can still issue real probe requests to existing endpoints such as
    `POST /auction`.
12. **Trailing slash:** redirect `/_ts/kitchen-sink` to `/_ts/kitchen-sink/` so
    relative asset URLs resolve correctly.
13. **Routing:** exact file routing only, except `/_ts/kitchen-sink/` maps to
    `index.html`. Missing files return 404.
14. **Disabled behavior:** when `debug.kitchen_sink_enabled = false`,
    kitchen-sink paths return 404 and do not fall through to publisher origin.
15. **Asset embedding:** use a `build.rs` in `trusted-server-kitchen-sink` to
    recursively embed every non-dotfile under `site/`; do not require a manual
    manifest.
16. **Copy updates:** rewrite site copy to describe embedded Trusted Server use,
    not Cloudflare Pages deployment.
17. **Caching/security headers:** HTML is `no-cache`; non-HTML assets get cache
    headers and ETags. Apply basic security headers, but no CSP in v1 because
    TS may inject inline configuration/scripts.
18. **Auth:** the explicit config flag is the only gate in v1.

## Proposed Configuration

```toml
[debug]
kitchen_sink_enabled = true
```

The field defaults to `false`.

## Proposed Route Behavior

```text
GET  /_ts/kitchen-sink           -> redirect to /_ts/kitchen-sink/
HEAD /_ts/kitchen-sink           -> redirect to /_ts/kitchen-sink/
GET  /_ts/kitchen-sink/          -> processed index.html
HEAD /_ts/kitchen-sink/          -> index headers, empty body
GET  /_ts/kitchen-sink/index.html -> processed index.html
GET  /_ts/kitchen-sink/prebid.html -> processed prebid.html
GET  /_ts/kitchen-sink/assets/app.js -> raw JavaScript asset
GET  /_ts/kitchen-sink/not-real  -> 404
POST /_ts/kitchen-sink/...       -> 405 Method Not Allowed
```

When disabled, all `/_ts/kitchen-sink` and `/_ts/kitchen-sink/*` requests return
404 and do not proxy to the publisher origin.

## Proposed Architecture

### 1. Asset crate

Add:

```text
crates/trusted-server-kitchen-sink/
  Cargo.toml
  build.rs
  src/lib.rs
  site/
    index.html
    auction.html
    prebid.html
    identity.html
    prebid.js
    assets/...
```

The build script should:

- recursively walk `site/`;
- exclude dotfiles and dot-directories, including `.DS_Store`;
- generate an asset table into `OUT_DIR`;
- include file bytes with `include_bytes!`;
- infer content type from extension;
- produce stable ETag or content-hash metadata.

Suggested public API:

```rust
pub struct KitchenSinkAsset {
    pub path: &'static str,
    pub body: &'static [u8],
    pub content_type: &'static str,
    pub etag: &'static str,
}

pub fn asset_for_path(path: &str) -> Option<KitchenSinkAsset>;
```

`asset_for_path` should accept normalized site-relative paths, not inbound
Trusted Server route paths.

### 2. Core handler

Add a core handler that:

1. checks `settings.debug.kitchen_sink_enabled`;
2. validates method support;
3. handles `/_ts/kitchen-sink` trailing-slash redirect;
4. strips the `/_ts/kitchen-sink/` prefix;
5. maps an empty relative path to `index.html`;
6. resolves the embedded asset;
7. applies ETag and cache/security headers;
8. processes `.html` assets through the HTML pipeline;
9. returns raw bodies for non-HTML assets;
10. suppresses response bodies for `HEAD`.

HTML processing should use the same integration registry and settings as
publisher HTML processing. It should not perform a publisher-origin fetch.

### 3. Dispatch integration

In Fastly adapter fallback dispatch, add kitchen-sink handling after dynamic
TSJS and integration routes, before asset-route and publisher-origin fallback:

```text
/static/tsjs=...
registered integration routes
/_ts/kitchen-sink...      # new
proxy.asset_routes
publisher fallback
```

The kitchen-sink branch should preserve the existing pre-route filter and
finalization behavior. For browser navigation requests, mirror the publisher
fallback's EC generation behavior where practical.

### 4. Site migration

Copy the current Cloudflare Pages experiment into the new crate's `site/`
directory and update it for embedded use:

- convert site-local absolute references to relative references;
- keep actual Trusted Server endpoint probes absolute;
- update text that references Cloudflare Pages deployment;
- keep page content minimal and focused on one primary action per page;
- keep the tiny `prebid.js` placeholder as a site-local relative asset;
- avoid introducing customer-specific domains, credentials, or real production
  values in examples.

## Headers

Recommended v1 headers:

For HTML:

```text
Content-Type: text/html; charset=utf-8
Cache-Control: no-cache
ETag: <asset hash or processed hash strategy>
X-Content-Type-Options: nosniff
Referrer-Policy: strict-origin-when-cross-origin
Permissions-Policy: camera=(), geolocation=(), microphone=()
```

For non-HTML assets:

```text
Content-Type: <inferred content type>
Cache-Control: public, max-age=300
ETag: <asset hash>
X-Content-Type-Options: nosniff
Referrer-Policy: strict-origin-when-cross-origin
Permissions-Policy: camera=(), geolocation=(), microphone=()
```

Do not add CSP in v1.

## Testing Plan

### Config tests

- `debug.kitchen_sink_enabled` defaults to `false`.
- TOML parses `debug.kitchen_sink_enabled = true`.
- Unknown debug fields remain rejected by `deny_unknown_fields`.

### Asset crate tests

- `index.html` is present.
- known JS/CSS assets are present with expected content types.
- dotfiles are not present.
- missing paths return `None`.

### Core handler tests

- disabled kitchen-sink paths return 404.
- enabled `/_ts/kitchen-sink` redirects to `/_ts/kitchen-sink/`.
- enabled `/_ts/kitchen-sink/` serves `index.html`.
- `HEAD` returns headers without a body.
- missing files return 404.
- unsupported methods return 405.
- HTML responses include a TSJS injection marker when the registry enables head
  injection.
- non-HTML assets are not HTML-processed.
- kitchen-sink paths do not fall through to publisher origin.

### Adapter/dispatch tests

- kitchen-sink routing happens before configured asset routes.
- disabled kitchen-sink paths do not fall through to publisher fallback.
- pre-route/finalization behavior remains consistent with other fallback-path
  routes.

## Risks and Follow-ups

- **WASM size:** always compiling the site into the binary may become costly as
  the site grows. If this becomes a problem, consider compression, a Cargo
  feature, or a separate asset store.
- **Processed ETags:** HTML processing can vary by config and request host. If
  static asset ETags are reused for processed HTML, they may not represent the
  final bytes. The implementation should either omit ETag for processed HTML or
  compute it after processing.
- **Fixture accuracy:** the embedded fixture simulates publisher HTML processing
  but does not fetch from a publisher origin. If origin-fetch behavior itself
  needs coverage, keep separate publisher-origin integration tests.
- **Security exposure:** the site is disabled by default and gated by config, but
  once enabled it is public. Avoid adding sensitive config dumps or admin
  behavior to the static pages.
