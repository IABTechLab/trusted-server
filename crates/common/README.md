# trusted-server-common

Utilities shared by Trusted Server components. This crate contains HTML/CSS rewriting helpers used to normalize ad creative assets to first‑party proxy endpoints.

## Creative Rewriting

The `creative` module rewrites external asset URLs in creative markup to a unified first‑party proxy so the publisher controls egress.

Key rules:

- Proxy absolute/protocol‑relative URLs (http/https or `//`) to `/first-party/proxy?u=<encoded>`
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

Helpers:

- `rewrite_creative_html(markup, settings) -> String` — rewrite an HTML fragment
- `rewrite_css_body(css, settings) -> String` — rewrite a CSS body (`url(...)` entries)
- `rewrite_srcset(srcset, settings) -> String` — proxy absolute candidates; preserve descriptors (`1x`, `1.5x`, `100w`)
- `split_srcset_candidates(srcset) -> Vec<&str>` — robust splitting for commas with/without spaces; avoids splitting the first `data:` mediatype comma

Behavior is covered by an extensive test suite in `crates/common/src/creative.rs`.

