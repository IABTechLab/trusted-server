# Design Spec: A module writes Google's tags on the page from the settings

**Status:** Proposed, 2026-10-12. Implemented on the fork as jwrosewell/trusted-server#25, ready for review, answering IABTechLab/trusted-server#1288. This pull request (#1084) carries every design document of the stack in one place.
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** `2026-08-27-integration-provider-seam-design.md`
**Related PRs:** #1094, jwrosewell/trusted-server#25
**Last updated:** 2026-10-12

> **Why this spec exists.** A publisher whose page templates forbid third-party scripts cannot put Google's tags on the page, so they have to come from the proxy.

## 1. Problem

A publisher whose page templates forbid third-party scripts cannot put Google's tags on the page, so they have to come from the proxy. Nothing does that. The Tag Manager module re-serves a tag the page already carries, which needs a page that already loads Google, and a page that loads Google's tags itself sends measurement data before any consent default has been set, which Google's consent guide forbids.

## 2. Design

```toml
[tag]
modules = ["google-tag"]

[tag.google-tag]
analytics_tag_id = "G-XXXXXXXXXX"
container_id = "GTM-EXAMPLE1"

[[serve]]
media_type = "text/html"
middleware = ["tag.google-tag"]
```

A module, `crates/tag/google-tag`, that writes Google's own snippets from the settings. At the start of head, the consent mode defaults, then Tag Manager's container script, then the gtag.js snippet, then the AdSense loader. Immediately after the opening body tag, Tag Manager's noscript iframe. Each snippet is copied from Google's documentation rather than paraphrased and carries the page it came from, so a reader can compare the two. Consent defaults come first because Google's consent guide requires the default command before anything that sends measurement data, and all four consent types default to denied, because no Google identifier may be stored before a permission signal has been processed.

Core gains one place: a middleware may write markup immediately after the opening body tag, `body_open_inserts` on its action, once per document, beside the places it had for the head and for straight after the script bundle. A deployment whose middleware write nothing there registers no handler for it.

Where each loader points is settled at startup, because head and body insertions never reach the attribute rewriters. When the Tag Manager module serves the same tag or container, the loader names the first-party path, otherwise Google's own address, and the Tag Manager crate offers the two paths and the two id shapes it accepts, so the two modules cannot disagree about an id. The noscript iframe always names Google, because `ns.html` has no first-party route behind it. A page also loading gtag.js for the configured tag has that duplicate loader removed, which is why the module registers before Tag Manager in the stock list, since a removal short-circuits the rewriter chain. An inline configuration call in the publisher's own page is not removed and still sends a second page view, which the guide says plainly, because removing it would mean buffering every response.

**Why.** A publisher whose page templates forbid third-party scripts cannot put a Google tag on the page, so it has to come from the proxy. Nothing did that: the Tag Manager module re-serves a tag the page already carries, which needs a page that already loads Google. And a page that loads Google's tags itself sends measurement data before any consent default is set.

## 3. What is not in this change

- An AdSense unit. The per-unit `<ins>` markup is a property of the publisher's own page, not of this configuration.
- Removing an inline `gtag('config', ...)` from the publisher's page, which would mean buffering every response.

## 4. Acceptance

The seam probe's middleware writes a marker immediately after the opening body tag, and its page test reads it back once, directly after `<body>`, through the fetch processor.

The pull request's description records how it was checked, with every commit linted and tested on its own, in order, the full local check on its head and CI green on all 19 checks, and 5 deliberate faults made one at a time and each caught by a test.

## Revision record

| Date       | Change                                                          |
| ---------- | --------------------------------------------------------------- |
| 2026-10-12 | First draft, from the pull request and its issue as they stand. |
