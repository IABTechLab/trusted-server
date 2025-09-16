# Trusted Server JS (tsjs)

tsjs is the browser-side library for Trusted Server. It ships as small IIFE bundles:

- Core (`tsjs-core.js`) — always loaded
- Prebid.js extension (`tsjs-ext.js`) — optional shim
- Creative helper (`tsjs-creative.js`) — injected into proxied creatives to guard click URLs

This Rust crate builds those bundles (via Vite) and embeds them so other crates can serve them as first‑party assets.

## What It Provides

- Core `tsjs` API and queue (`window.tsjs.que`) always available
- Optional Prebid extension that aliases `window.pbjs` to `window.tsjs` and flushes `pbjs.que`
- `addAdUnits(units)` to register ad units
- `renderAllAdUnits()` to render all registered ad units
- `renderAdUnit(code)` to render a single unit by `code`
- `setConfig(cfg)` and `getConfig()` to control logging, etc.
- `requestAds({ bidsBackHandler })` calls the callback synchronously.
  - In `firstParty` mode (default): inserts a sandboxed iframe per ad unit that loads `/first-party/ad?slot=<code>&w=<w>&h=<h>`
  - In `thirdParty` mode: posts to `/third-party/ad` and renders returned creatives
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
- `lib/src/core/` — core library (bootstrap, config, log, registry, render, request, types, queue)
- `lib/src/ext/` — optional extensions (PrebidJS shim: `prebidjs.ts`, entry: `index.ts`)
- `dist/tsjs-core.js` — core bundle (IIFE, via Vite library mode)
- `dist/tsjs-ext.js` — PrebidJS shim extension (IIFE)
- `dist/tsjs-creative.js` — creative click‑guard bundle (IIFE)
- Rust crate exposes `TSJS_CORE_BUNDLE`, `TSJS_EXT_BUNDLE`, and `TSJS_CREATIVE_BUNDLE`
- `build.rs` — runs `npm run build` inside `ts/` if Node is available

## Build the JS Bundle

- Requires Node >=18
- From repo root: `cd crates/js/lib && npm ci && npm run build`
- Or simply `cargo build` — the build script will run `npm install` and `npm run build`, and then copy the outputs to `OUT_DIR/tsjs-core.js` and `OUT_DIR/tsjs-ext.js` (failing if core cannot be found).

## Run Tests (TypeScript)

- `cd crates/js/lib && npm test` (vitest + jsdom)

## Serving From Rust

```rust
use trusted_server_js::{
    TSJS_CORE_BUNDLE, TSJS_CORE_FILENAME,
    TSJS_EXT_BUNDLE, TSJS_EXT_FILENAME,
    TSJS_CREATIVE_BUNDLE, TSJS_CREATIVE_FILENAME,
};
// Recommend serving via a unified endpoint your router handles:
//   /static/tsjs=<filename>
// where <filename> is one of TSJS_CORE_FILENAME, TSJS_EXT_FILENAME, TSJS_CREATIVE_FILENAME.
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
- Proxied creative HTML injects the creative helper once at the top of `<body>`: `/static/tsjs=tsjs-creative.min.js`.

## Notes

- By default, the build fails if `tsjs-core.js` cannot be produced. To change behavior:
  - `TSJS_SKIP_BUILD=1`: skip running npm; requires `dist/tsjs-core.js` to exist so it can be copied to `OUT_DIR`.
  - `TSJS_ALLOW_FALLBACK=1`: allow using a checked‑in `dist/tsjs-core.js` if the npm build didn’t produce an output.
  - `TSJS_TEST=1`: run `npm test` during the build.
