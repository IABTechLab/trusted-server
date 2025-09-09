Trusted Server JS (tsjs)

Best-practice TypeScript project that builds a browser JS library and embeds the bundle in this Rust crate.

What it provides

- Global `tsjs.que` and `pbjs.que` queues (both flushed on init)
- `tsjs` and `pbjs` refer to the same global object
- `addAdUnits(units)` to register ad units
- `renderAllAdUnits()` to render all registered ad units
- `renderAdUnit(code)` to render a single unit by `code`
- `setConfig(cfg)` and `getConfig()` to accept Prebid-like configs
- `requestBids({ bidsBackHandler })` calls the callback immediately and renders placeholders
- `version`

Logging

- Prebid-style logging via `tsjs.log` (also aliased on `pbjs.log`):
  - Levels: `silent`, `error`, `warn`, `info`, `debug` (default `warn`)
  - Methods: `log.info()`, `log.warn()`, `log.error()`, `log.debug()`, `log.setLevel()`, `log.getLevel()`
  - `setConfig({ debug: true })` sets level to `debug`, or set explicit `logLevel` in config
  - Key lifecycle logs: init, queue flushes, `addAdUnits`, `renderAdUnit`, `renderAllAdUnits`, `requestBids`

Project layout

- `ts/` — TypeScript source, tooling (Vite, Vitest, ESLint, Prettier)
- `dist/tsjs.js` — built bundle (IIFE, via Vite library mode). Build script copies this into `OUT_DIR/tsjs.js` and the Rust crate embeds that.
- `src/lib.rs` — exposes `TSJS_BUNDLE` and `TSJS_FILENAME`
- `build.rs` — runs `npm run build` inside `ts/` if Node is available

Build the JS bundle

- Requires Node >=18
- From repo root: `cd crates/js/ts && npm ci && npm run build`
- Or simply `cargo build` — the build script will run `npm install` and `npm run build`, and then copy the output to `OUT_DIR/tsjs.js` (failing the build if it can’t find the output).

Run tests (TypeScript)

- `cd crates/js/ts && npm test` (vitest + jsdom)

Serve from Rust

```rust
use trusted_server_js::TSJS_BUNDLE;
// Return TSJS_BUNDLE in a response with Content-Type: application/javascript
```

HTML usage

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
    tsjs.renderAllAdUnits();
  });
  // later: <script src="/static/tsjs.min.js"></script>
</script>
```


Notes

- By default, the build fails if `tsjs.js` cannot be produced. To change behavior:
  - `TSJS_SKIP_BUILD=1`: skip running npm; requires `dist/tsjs.js` to exist so it can be copied to `OUT_DIR`.
  - `TSJS_ALLOW_FALLBACK=1`: allow using a checked‑in `dist/tsjs.js` if the npm build didn’t produce an output.
  - `TSJS_TEST=1`: run `npm test` during the build.
