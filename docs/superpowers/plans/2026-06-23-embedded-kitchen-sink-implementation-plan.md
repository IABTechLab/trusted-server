# Embedded Kitchen Sink Implementation Plan

**Date:** 2026-06-23
**Status:** Proposed
**Spec:** `docs/superpowers/specs/2026-06-23-embedded-kitchen-sink-design.md`

## Definition of done

- `debug.kitchen_sink_enabled` exists, defaults to `false`, and is parsed from
  `trusted-server.toml`.
- A new `trusted-server-kitchen-sink` crate embeds the migrated static site from
  source-controlled files under `site/`.
- The embedded site is served from `/_ts/kitchen-sink/` only when enabled.
- `/_ts/kitchen-sink` redirects to `/_ts/kitchen-sink/`.
- HTML files are processed through Trusted Server's HTML processor before being
  returned.
- Non-HTML assets are served raw with content type, cache, ETag, and security
  headers.
- Disabled kitchen-sink paths return 404 and do not fall through to publisher
  origin or configured asset routes.
- Kitchen-sink dispatch runs after dynamic TSJS and integration routes, but
  before `proxy.asset_routes` and publisher fallback.
- Browser-navigation requests to kitchen-sink HTML participate in EC generation
  and response finalization where practical.
- The site copy no longer refers to Cloudflare Pages as the intended runtime.
- Unit/adapter tests cover config, asset embedding, handler behavior, HTML
  processing, disabled behavior, and dispatch precedence.
- Required Rust formatting, test, clippy, and wasm build checks pass.

## Stage 1 — Add configuration

1. Extend `DebugConfig` in `crates/trusted-server-core/src/settings.rs`:

   ```rust
   pub kitchen_sink_enabled: bool
   ```

   Keep `#[serde(default)]`, `deny_unknown_fields`, and default-off behavior.

2. Add settings tests near the existing settings tests:
   - default config has `debug.kitchen_sink_enabled == false`;
   - TOML with `[debug] kitchen_sink_enabled = true` parses true;
   - unknown debug fields are still rejected.

3. Update `trusted-server.example.toml` only if the project convention is to
   advertise debug flags there. If added, keep it commented or explicitly
   disabled.

## Stage 2 — Create `trusted-server-kitchen-sink` crate

1. Add a workspace member:

   ```text
   crates/trusted-server-kitchen-sink
   ```

2. Add crate files:

   ```text
   crates/trusted-server-kitchen-sink/
     Cargo.toml
     build.rs
     src/lib.rs
     site/
   ```

3. Add a workspace dependency alias in root `Cargo.toml` if desired:

   ```toml
   trusted-server-kitchen-sink = { path = "crates/trusted-server-kitchen-sink" }
   ```

4. Keep the crate dependency-light:
   - use `std::fs` recursion in `build.rs` rather than adding `walkdir`;
   - use `sha2` as a build dependency for stable asset ETags/content hashes;
   - infer content types with a small extension map instead of adding
     `mime_guess`.

5. Expose a small API from `src/lib.rs`:

   ```rust
   pub struct KitchenSinkAsset {
       pub path: &'static str,
       pub body: &'static [u8],
       pub content_type: &'static str,
       pub etag: &'static str,
   }

   pub fn asset_for_path(path: &str) -> Option<&'static KitchenSinkAsset>;
   ```

6. Generated assets should be sorted by path for deterministic output.

## Stage 3 — Implement asset generation

1. `build.rs` should recursively walk `site/` and generate a Rust file in
   `OUT_DIR`, for example `kitchen_sink_assets.rs`.

2. Include every non-dotfile under `site/`:
   - skip dotfiles such as `.DS_Store`;
   - skip dot-directories;
   - normalize generated paths to `/` separators;
   - emit `cargo:rerun-if-changed=...` for included files and relevant
     directories.

3. For each file, generate:
   - site-relative path, e.g. `assets/app.js`;
   - `include_bytes!(...)` body;
   - content type;
   - strong-ish ETag based on the file bytes, e.g. quoted SHA-256 hex.

4. Content type map should include at minimum:
   - `.html` -> `text/html; charset=utf-8`
   - `.css` -> `text/css; charset=utf-8`
   - `.js` -> `application/javascript; charset=utf-8`
   - `.json` -> `application/json; charset=utf-8`
   - `.svg` -> `image/svg+xml`
   - `.png` -> `image/png`
   - `.jpg` / `.jpeg` -> `image/jpeg`
   - `.webp` -> `image/webp`
   - `.ico` -> `image/x-icon`
   - fallback -> `application/octet-stream`

5. Add asset crate tests:
   - `asset_for_path("index.html")` returns an HTML asset;
   - known JS/CSS assets return expected content types;
   - dotfiles are not embedded;
   - missing files return `None`.

## Stage 4 — Migrate and adjust the static site

1. Copy only deployable/static source files from:

   ```text
   /Users/christian/Projects/stackpop/trusted-server-coolify/static-site/public
   ```

   into:

   ```text
   crates/trusted-server-kitchen-sink/site
   ```

2. Do not copy:
   - `node_modules/`;
   - `package-lock.json`;
   - `package.json` unless later standalone tooling is intentionally added;
   - `wrangler.toml`;
   - Cloudflare Pages `_headers` as runtime config.

3. Convert site-local absolute paths to relative paths:
   - `/assets/styles.css` -> `assets/styles.css`
   - `/assets/app.js` -> `assets/app.js`
   - `/auction.html` -> `auction.html`
   - `/prebid.html` -> `prebid.html`
   - `/creative-proxy.html` -> `creative-proxy.html`
   - `/identity.html` -> `identity.html`
   - `/prebid.js` -> `prebid.js`
   - brand/home link `/` -> `./` or `index.html`

4. Keep actual Trusted Server endpoint probes absolute:
   - `/auction`
   - `/_ts/set-tester`
   - any future Trusted Server route probes.

5. Rewrite copy to describe embedded Trusted Server usage:
   - remove Cloudflare Pages deployment instructions from runtime-facing pages;
   - describe the site as available at `/_ts/kitchen-sink/` when enabled;
   - keep examples fictional and avoid real customer/domain/secret values.

6. Preserve the current page set for v1:
   - `index.html`
   - `auction.html`
   - `prebid.html`
   - `creative-proxy.html`
   - `identity.html`
   - `prebid.js`
   - `assets/*`

## Stage 5 — Add core handler

1. Add a new core module, for example:

   ```text
   crates/trusted-server-core/src/kitchen_sink.rs
   ```

   and export it from `crates/trusted-server-core/src/lib.rs`.

2. Add `trusted-server-kitchen-sink` as a dependency of
   `trusted-server-core`.

3. Suggested handler signature:

   ```rust
   pub fn handle_kitchen_sink_request(
       settings: &Settings,
       integration_registry: &IntegrationRegistry,
       services: &RuntimeServices,
       req: &Request<EdgeBody>,
   ) -> Result<Response<EdgeBody>, Report<TrustedServerError>>
   ```

   If ownership is easier, accept `Request<EdgeBody>` by value, but avoid
   consuming request bodies for GET/HEAD.

4. Handler behavior:
   - if `settings.debug.kitchen_sink_enabled` is false, return 404;
   - accept only `GET` and `HEAD`; return 405 for other methods when enabled;
   - redirect exact `/_ts/kitchen-sink` to `/_ts/kitchen-sink/`;
   - require paths to start with `/_ts/kitchen-sink/`;
   - strip the prefix;
   - map empty relative path to `index.html`;
   - reject path traversal-ish segments such as `.` and `..`;
   - do not percent-decode path segments in v1;
   - resolve the asset through `trusted_server_kitchen_sink::asset_for_path`;
   - return 404 for missing assets.

5. HTML processing:
   - identify HTML by content type or `.html` path;
   - construct `HtmlProcessorConfig::from_settings(...)` with: - `origin_host = settings.publisher.origin_host()`; - request host/scheme from `RequestInfo::from_request(req,
services.client_info())`; - the current `IntegrationRegistry`;
   - call `create_html_processor(config)` and process the full static HTML body;
   - set `Content-Length` from the processed bytes;
   - do not reuse the static file ETag for processed HTML unless the ETag is
     computed from the processed bytes.

6. Non-HTML assets:
   - serve the embedded bytes unchanged;
   - attach the static ETag from the asset crate;
   - support `If-None-Match` if straightforward by returning 304 with no body.

7. Headers:
   - HTML: `Cache-Control: no-cache`;
   - non-HTML: `Cache-Control: public, max-age=300`;
   - all kitchen-sink responses:
     - `X-Content-Type-Options: nosniff`;
     - `Referrer-Policy: strict-origin-when-cross-origin`;
     - `Permissions-Policy: camera=(), geolocation=(), microphone=()`;
     - optional diagnostic header such as
       `X-Trusted-Server-Kitchen-Sink: processed|raw` for tests/debugging.

8. `HEAD` responses should carry the same headers as `GET` but have an empty
   body. Preserve the `Content-Length` of the representation if that is easy;
   otherwise be consistent with existing project conventions.

## Stage 6 — Wire adapter dispatch

1. Import the core handler in `crates/trusted-server-adapter-fastly/src/app.rs`.

2. Add a helper:

   ```rust
   fn uses_kitchen_sink_fallback(path: &str) -> bool {
       path == "/_ts/kitchen-sink" || path.starts_with("/_ts/kitchen-sink/")
   }
   ```

3. In `dispatch_fallback`, insert kitchen-sink handling after:
   - dynamic TSJS fallback;
   - registered integration routes;

   and before:
   - `proxy.asset_routes`;
   - publisher fallback.

4. Keep this branch inside the `result` flow rather than returning early, so
   `attach_dispatch_extensions(response, ec, effects)` still runs.

5. Mirror publisher EC generation for browser navigation requests before serving
   kitchen-sink HTML. Prefer factoring the existing publisher branch logic into a
   small helper to avoid duplication:

   ```rust
   fn generate_navigation_ec_if_needed(...)
   ```

6. Ensure disabled kitchen-sink requests return 404 from this branch and do not
   continue to asset routes or publisher fallback.

7. Check the legacy `main.rs` route path if it is still used in tests or rollout.
   If legacy dispatch remains reachable, add equivalent kitchen-sink handling or
   explicitly document that this feature is EdgeZero-router-only. Prefer parity
   unless the current branch has intentionally retired legacy routing.

## Stage 7 — Tests

### Core/config tests

1. Add settings parse/default tests for `debug.kitchen_sink_enabled`.
2. Add kitchen-sink handler tests using test settings and a test registry:
   - disabled `/_ts/kitchen-sink/` returns 404;
   - enabled `/_ts/kitchen-sink` redirects to trailing slash;
   - enabled `/_ts/kitchen-sink/` returns HTML;
   - enabled `/_ts/kitchen-sink/index.html` returns HTML;
   - missing file returns 404;
   - unsupported method returns 405;
   - `HEAD` returns no body;
   - HTML includes the trusted-server JS injection marker;
   - JS/CSS assets do not include injected HTML markers.

3. If ETag support is implemented:
   - asset GET includes ETag;
   - matching `If-None-Match` returns 304 for raw assets.

### Asset crate tests

1. Verify generated asset lookup and content types.
2. Verify skipped dotfiles are absent.
3. Verify deterministic path normalization by checking a nested asset path.

### Adapter dispatch tests

1. Enabled kitchen-sink index is served without invoking publisher fallback.
2. Disabled kitchen-sink path returns 404 without invoking publisher fallback.
3. Kitchen-sink path wins before a broad asset route such as `/_ts/` or `/`.
4. Existing `/_ts/debug/ja4` behavior is unchanged.
5. Existing `/static/tsjs=...`, `/auction`, and integration routes retain
   precedence.

## Stage 8 — Documentation updates

1. Keep the design spec as the authoritative design artifact.
2. Add a short operator-facing note where debug settings are documented, if such
   a page exists:

   ```toml
   [debug]
   kitchen_sink_enabled = true
   ```

3. If `trusted-server.example.toml` includes debug settings, add commented
   guidance that this is public when enabled and intended for test/diagnostic
   environments.

4. Do not add Cloudflare Pages deployment docs for the embedded path.

## Stage 9 — Verification

Run at minimum after implementation:

```bash
cargo fmt --all -- --check
cargo test --package trusted-server-kitchen-sink
cargo test --package trusted-server-core
cargo test --package trusted-server-adapter-fastly
cargo test --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo build --package trusted-server-adapter-fastly --release --target wasm32-wasip1
```

If JS/TS source is not touched, JS tests are not required. If any files under
`crates/trusted-server-js/lib` are touched, also run:

```bash
cd crates/trusted-server-js/lib && npx vitest run
cd crates/trusted-server-js/lib && npm run format
```

For docs formatting after this plan/spec work or any guide updates:

```bash
cd docs && npm run format
```

## Risks and watch points

- **Processed HTML ETags:** static asset ETags are not valid for processed HTML
  if injected output varies by config, integration registry, host, or scheme.
  Omit ETag for processed HTML or compute it after processing.
- **WASM size:** compiling the site into the binary is intentional for v1, but
  site growth can increase Wasm size. Revisit compression or feature-gating if
  size becomes a problem.
- **Route shadowing:** kitchen-sink dispatch must not shadow existing Trusted
  Server internal routes outside the exact `/_ts/kitchen-sink` prefix.
- **HTML processor drift:** avoid duplicating publisher HTML processing logic.
  Use `HtmlProcessorConfig::from_settings` and `create_html_processor` directly.
- **EC parity:** kitchen-sink pages should be close enough to publisher pages for
  diagnostics. If exact publisher fallback behavior becomes important, factor
  shared publisher-navigation setup rather than copying more logic.
- **Public exposure:** config flag only means the site is public once enabled.
  Do not add sensitive config dumps, secrets, customer domains, or admin actions
  to the fixture.
