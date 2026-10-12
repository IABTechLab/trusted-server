# Design Spec: A middleware may run on a media type other than HTML

**Status:** Proposed, 2026-10-12. Implemented on the fork as jwrosewell/trusted-server#14, ready for review, answering IABTechLab/trusted-server#1277. This pull request (#1084) carries every design document of the stack in one place.
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** `2026-08-27-integration-provider-seam-design.md`
**Related PRs:** #1094, jwrosewell/trusted-server#14
**Last updated:** 2026-10-12

> **Why this spec exists.** Every middleware in the stack changes HTML, and `[[fetch]]` and `[[serve]]` refuse an entry for any other media type.

## 1. Problem

Every middleware in the stack changes HTML, and `[[fetch]]` and `[[serve]]` refuse an entry for any other media type. A publisher's `ads.txt`, `robots.txt`, a script or a stylesheet are text that passes through the same path, and a module that has something to add to one of them has nowhere to do it, although the entries already say where and in what order.

## 2. Design

```toml
[[fetch]]
media_type = "text/plain"
path = "/ads.txt"
middleware = ["example.sellers"]
```

An entry may cover any media type core streams as text: `text/html`, another `text/` type other than `text/x-component`, `application/javascript`, `application/json` or `image/svg+xml`, written as a bare `type/subtype` in lower case. A response takes the entry for the type and subtype of its content type, without the parameters, so `text/plain; charset=utf-8` takes the entry for `text/plain`. An image or a length prefixed stream passes through untouched and no entry can cover it.

**Why.** Every middleware in the stack changes HTML, and `[[fetch]]` and `[[serve]]` refused any other media type. A publisher's `ads.txt`, `robots.txt` beside it, a script or a stylesheet are text that passes through the same path, and a module that has something to add to one of them had no place to do it. A text file is the next thing a module wants to change, and the entries already say where and in what order.

```rust
pub trait Middleware: Send + Sync {
    fn middleware_id(&self) -> &'static str;
    fn handles_media_type(&self, media_type: &str) -> bool {
        media_type == HTML_MEDIA_TYPE
    }
    fn phases(&self) -> &[MiddlewarePhase];
    fn create(&self, context: &MiddlewareContext<'_>) -> MiddlewareAction;
}
```

- **A middleware says what it works on.** HTML alone unless it says otherwise, so every module in the stack keeps its behavior with no change to its code. An entry naming a middleware under a media type it does not handle is refused when the registry is built, naming the entry and the middleware.
- **A chain carries its media type.** On one other than HTML its plan refuses an action that decides about HTML, being head markup or an element or text handler, by the middleware's name. A stream processor is the one thing that applies to any body.
- **The publisher runs the stream processors on the body** after it has moved the origin's addresses in it: the fetch entry's on what is stored, and the serve entry's on each reader's copy, whether assembled from the store or fetched for the request, as on a page.

| Configuration                                                                              | Refused with                                                        |
| ------------------------------------------------------------------------------------------ | ------------------------------------------------------------------- |
| An entry for `image/png`, `text/x-component`, `*/*`, `TEXT/HTML` or a type with parameters | "is not a media type a middleware runs on", and the types to write  |
| An entry naming a middleware under a media type it does not handle                         | The entry, the media type and the middleware's name                 |
| A middleware's head markup or handler on a body that is not HTML                           | The middleware's name and the media type, when the chain is planned |

## 3. What is not in this change

- A middleware that uses it. An `ads.txt` middleware that adds a publisher's sellers to the origin's file is the first, and follows as an addition of its own.
- Headers. A middleware changes the body alone, and a response's headers are the adapters' finalize step's as before.

## 4. Acceptance

The pull request's description records how it was checked, with every commit linted and tested on its own, in order, the full local check on its head and CI green on all 19 checks, and 8 deliberate faults made one at a time and each caught by a test.

## Revision record

| Date       | Change                                                          |
| ---------- | --------------------------------------------------------------- |
| 2026-10-12 | First draft, from the pull request and its issue as they stand. |
