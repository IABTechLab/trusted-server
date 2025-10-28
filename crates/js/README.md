# Trusted Server JS (tsjs)

tsjs is the browser-side library for Trusted Server. It ships as small IIFE bundles:

- Core (`tsjs-core.js`) — always loaded
- Prebid.js extension (`tsjs-ext.js`) — optional shim
- Creative helper (`tsjs-creative.js`) — injected into proxied creatives to guard and repair click URLs

This Rust crate builds those bundles (via Vite) and embeds them so other crates can serve them as first‑party assets.

## What It Provides

- Core `tsjs` API and queue (`window.tsjs.que`) always available
- Optional Prebid extension that aliases `window.pbjs` to `window.tsjs` and flushes `pbjs.que`
- `addAdUnits(units)` to register ad units
- `renderAllAdUnits()` to render all registered ad units
- `renderAdUnit(code)` to render a single unit by `code`
- `setConfig(cfg)` and `getConfig()` to control logging, etc.
- `requestAds({ bidsBackHandler })` calls the callback synchronously.
  - In `firstParty` mode (default): schedules insertion of a sandboxed iframe per ad unit that loads `/first-party/ad?slot=<code>&w=<w>&h=<h>` (retries until the slot exists)
  - In `thirdParty` mode: posts to `/third-party/ad`, renders simple placeholders immediately, and swaps in returned creatives when the JSON response arrives
  - The Prebid extension also adds `pbjs.getHighestCpmBids(adUnitCodes?)`
- `version`

## Logging

- Prebid-style logging via `tsjs.log` (aliased on `pbjs.log` when the extension is loaded):
  - Levels: `silent`, `error`, `warn`, `info`, `debug` (default `warn`)
  - Methods: `log.info()`, `log.warn()`, `log.error()`, `log.debug()`, `log.setLevel()`, `log.getLevel()`
  - `setConfig({ debug: true })` sets level to `debug`, or set explicit `logLevel` in config
  - Key lifecycle logs: init, queue flushes, `addAdUnits`, `renderAdUnit`, `renderAllAdUnits`, `requestBids`
  - In browsers, logs show a colored `[tsjs]` prefix

## Project Layout

- `ts/` — TypeScript source, tooling (Vite, Vitest, ESLint, Prettier)
- `lib/src/core/` — core library (bootstrap, config, log, registry, render, request, queue, types)
- `lib/src/ext/` — optional extensions (PrebidJS shim: `prebidjs.ts`, entry: `index.ts`)
- `lib/src/creative/` — creative click guard bundle
- `lib/src/shared/` — shared helpers (async scheduling, global detection, mutation batching)
- `dist/tsjs-core.js` — core bundle (IIFE, via Vite library mode)
- `dist/tsjs-ext.js` — PrebidJS shim extension (IIFE)
- `dist/tsjs-creative.js` — creative click‑guard bundle (IIFE)
- Rust crate exposes `TsjsBundle`, `bundle_for_filename`, and `bundle_hash`
- `build.rs` — runs `npm run build` inside `ts/` if Node is available

## Build the JS Bundle

- Requires Node >=18
- From repo root: `cd crates/js/lib && npm ci && npm run build`
- Or simply `cargo build` — the build script will run `npm install` and `npm run build`, and then copy the outputs to `OUT_DIR/tsjs-core.js` and `OUT_DIR/tsjs-ext.js` (failing if core cannot be found).

## Run Tests (TypeScript)

- `cd crates/js/lib && npm test` (vitest + jsdom). In sandboxed environments, use `npm run test -- --run` to execute once without watch mode.
- `npm run lint` for ESLint checks.

## Serving From Rust

```rust
use trusted_server_js::{bundle_for_filename, bundle_hash, TsjsBundle};

// Recommend serving via a unified endpoint your router handles:
//   /static/tsjs=<filename>
// `bundle_for_filename` accepts the plain `.js` filename and returns the bundle contents.
let filename = "tsjs-core.js";
let bundle = bundle_for_filename(filename).expect("unknown bundle");
let hash = bundle_hash(TsjsBundle::Core);
```

## HTML Usage

```html
<script>
  window.tsjs = window.tsjs || {};
  tsjs.que = tsjs.que || [];

  const adUnits = [
    { code: 'test-div',  mediaTypes: { banner: { sizes: [[300, 250]] } } },
    { code: 'test-div2', mediaTypes: { banner: { sizes: [[728, 90]] } } }
  ];

  tsjs.que.push(function() {
    tsjs.addAdUnits(adUnits);
    tsjs.setConfig({ mode: 'firstParty' }); // or 'thirdParty'
    tsjs.requestAds({ bidsBackHandler: function() {} });
  });
  // later: load core (served via unified endpoint)
  // <script src="/static/tsjs=tsjs-core.min.js"></script>
  // optionally load Prebid shim when pbjs is present
  // <script>
  //   if (window.pbjs) {
  //     var s=document.createElement('script');
  //     s.src='/static/tsjs=tsjs-ext.min.js';
  //     document.head.appendChild(s);
  //   }
  // </script>
</script>
```

## Auto‑Rewrite (Server)

- When auto-configure is enabled, the HTML processor injects the core loader and rewrites any Prebid script URLs to `/static/tsjs=tsjs-ext.min.js`. The extension aliases `window.pbjs` to `window.tsjs` and flushes `pbjs.que`.
- Proxied creative HTML injects the creative helper once at the top of `<body>`: `/static/tsjs=tsjs-creative.min.js`. The helper monitors anchors for script-driven rewrites and rebuilds first-party click URLs whenever creatives mutate them.

## First-Party Proxy Flows

The Rust services (`trusted-server-common`) expose several proxy entry points that work together to keep all ad traffic on the publisher’s domain while propagating the synthetic identifier generated for the user.

### Publisher Origin Proxy

- Endpoint: `handle_publisher_request` (`crates/common/src/publisher.rs`).
- Retrieves or generates the trusted synthetic identifier before Fastly consumes the request body.
- Always stamps the proxied response with `X-Synthetic-Fresh` and `x-psid-ts` headers and, when the browser does not already present one, sets the `synthetic_id=<value>` cookie (Secure + SameSite=Lax) bound to the configured publisher domain.
- Result: downstream assets fetched through the same first-party origin automatically include the synthetic ID header/cookie so subsequent proxy layers can read it.

### Creative Asset Proxy

- Endpoint: `handle_first_party_proxy` (`crates/common/src/proxy.rs`).
- Accepts the signed `/first-party/proxy?tsurl=...` URLs injected by the HTML rewriter and streams the creative from the third-party origin.
- Extracts the synthetic ID from the inbound cookie or header and forwards it to the creative origin by appending `synthetic_id=<value>` to the rewritten target URL (while preserving existing query parameters).
- Follows HTTP redirects (301/302/303/307/308) up to four hops, re-validating each `Location`, switching to `GET` after 303 responses, and propagating the synthetic ID on every hop.
- Ensures the response body is rewritten when it is HTML/CSS/JS so all nested asset requests loop back through the same first-party proxy.

### Click-Through Proxy

- Endpoint: `handle_first_party_click` (`crates/common/src/proxy.rs`).
- Validates the signed `/first-party/click` URL generated for anchors inside proxied creatives.
- On success, issues an HTTP 302 to the reconstructed destination and appends `synthetic_id=<value>` if the user presented one, letting downstream measurement end points correlate the click with the original synthetic identifier.
- Ensures click responses are never cached (`Cache-Control: no-store, private`).

Together these layers guarantee that the synthetic identifier generated on the publisher response is preserved throughout page loads, asset fetches, and click-throughs without exposing the third-party origins directly to the browser.

## Notes

- By default, the build fails if `tsjs-core.js` cannot be produced. To change behavior:
  - `TSJS_SKIP_BUILD=1`: skip running npm; requires `dist/tsjs-core.js` to exist so it can be copied to `OUT_DIR`.
  - `TSJS_ALLOW_FALLBACK=1`: allow using a checked‑in `dist/tsjs-core.js` if the npm build didn’t produce an output.
  - `TSJS_TEST=1`: run `npm test` during the build.
