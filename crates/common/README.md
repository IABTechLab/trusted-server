# trusted-server-common

Utilities shared by Trusted Server components. This crate contains HTML/CSS rewriting helpers used to normalize ad creative assets to proxy endpoints.

## Creative Rewriting

The `creative` module rewrites external asset URLs in creative markup to a unified first‑party proxy so the publisher controls egress.

Key rules:

- Proxy absolute/protocol‑relative URLs (http/https or `//`) to `/first-party/proxy?tsurl=<base-url>&<original-query-params>&tstoken=<sig>`
- Leave relative URLs unchanged (e.g., `/path`, `../path`, `local/file`)
- Ignore non‑network schemes: `data:`, `javascript:`, `mailto:`, `tel:`, `blob:`, `about:`

Rewritten locations:

- `<img src>`, `data-src`, `[srcset]`, `[imagesrcset]`
- `<script src>`
- `<video src>`, `<audio src>`, `<source src>`
- `<object data>`, `<embed src>`
- `<input type="image" src>`
- SVG: `<image href|xlink:href>`, `<use href|xlink:href>`
- `<iframe src>`
- `<link rel~="stylesheet|preload|prefetch" href>` and `imagesrcset`
- Inline styles (`[style]`) and `<style>` blocks: `url(...)` values are rewritten

Additional behavior:

- Injects a lightweight client helper into creative HTML once per document to preserve first‑party click URLs even if runtime scripts mutate anchors:
  - Injected at the top of `<body>`: `<script src="/static/tsjs=tsjs-creative.min.js" async></script>`
  - The bundle guards anchor clicks by restoring the originally rewritten first‑party link at click time.
  - Served through the unified endpoint described below.

Helpers:

- `rewrite_creative_html(settings, markup) -> String` — rewrite an HTML fragment
- `rewrite_css_body(settings, css) -> String` — rewrite a CSS body (`url(...)` entries)
- `rewrite_srcset(settings, srcset) -> String` — proxy absolute candidates; preserve descriptors (`1x`, `1.5x`, `100w`)
- `split_srcset_candidates(srcset) -> Vec<&str>` — robust splitting for commas with/without spaces; avoids splitting the first `data:` mediatype comma

JS bundles (served by publisher module):

- Dynamic endpoint: `/static/tsjs=tsjs-unified.min.js?v=<hash>`
  - At build time, each integration is compiled as a separate IIFE (`tsjs-core.js`, `tsjs-prebid.js`, `tsjs-creative.js`, etc.)
  - At runtime, the server concatenates `tsjs-core.js` + enabled integration modules based on `IntegrationRegistry` config
  - The URL filename is fixed for backward compatibility; the `?v=` hash changes when modules change

Behavior is covered by an extensive test suite in `crates/common/src/creative.rs`.

## Synthetic Identifier Propagation

- `synthetic.rs` generates a synthetic identifier per user request and exposes helpers:
  - `generate_synthetic_id` — creates a fresh HMAC-based ID using request signals and appends a short random suffix (format: `64hex.6alnum`).
  - `get_synthetic_id` — extracts an existing ID from the `x-synthetic-id` header or `synthetic_id` cookie.
  - `get_or_generate_synthetic_id` — reuses the existing ID when present, otherwise creates one.
- `publisher.rs::handle_publisher_request` stamps proxied origin responses with `x-synthetic-id`, and (when absent) issues the `synthetic_id` cookie so the browser keeps the identifier on subsequent requests.
- `proxy.rs::handle_first_party_proxy` replays the identifier to third-party creative origins by appending `synthetic_id=<value>` to the reconstructed target URL, follows redirects (301/302/303/307/308) up to four hops, and keeps downstream fetches linked to the same user scope.
- `proxy.rs::handle_first_party_click` adds `synthetic_id=<value>` to outbound click redirect URLs so analytics endpoints can associate clicks with impressions without third-party cookies.
