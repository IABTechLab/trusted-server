# Design Spec: robots.txt as a middleware

**Status:** Proposed, 2026-10-12. Implemented on the fork as jwrosewell/trusted-server#20, ready for review, answering IABTechLab/trusted-server#1283. This pull request (#1084) carries every design document of the stack in one place.
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** `2026-08-27-integration-provider-seam-design.md`
**Related PRs:** #1094, jwrosewell/trusted-server#20
**Last updated:** 2026-10-12

> **Why this spec exists.** `/robots.txt` is answered by a handler of core's own, registered as a route on each of the four adapters with a cache policy of its own, while every other change to what a deployment serves is a middleware named by an entry.

## 1. Problem

`/robots.txt` is answered by a handler of core's own, registered as a route on each of the four adapters with a cache policy of its own, while every other change to what a deployment serves is a middleware named by an entry. A publisher cannot place it, order it beside another change or run it in the serve phase, and every adapter carries a route and tests for one file.

## 2. Design

`/robots.txt` was answered by a handler of core's own, registered as a route on each of the four adapters with a cache policy of its own, while every other change to what a deployment serves is a middleware named by an entry. It is now `robots_txt`, a middleware of core's own, which runs on `text/plain` in either phase and prepares.

```toml
[robots-txt]
modules = ["refuse_all"]
always_allow = ["/ads.txt"]

[[fetch]]
media_type = "text/plain"
path = "/robots.txt"
middleware = ["robots_txt"]
```

Its preparation does what the route did. A refusal of every crawler needs nothing and gives the refusal document, and otherwise each selected contributor is asked through its holding and the file is assembled. The action replaces the body with the file, whatever the origin sent, and where the origin has no file the creating rule (jwrosewell/trusted-server#17) makes it. With no answer held for a contributor that fails, the preparation refuses with `503` and `Retry-After`, and nothing is stored.

The registry makes the middleware itself, once every registration is in, because the contributors it asks are known only then and a middleware cannot reach the registry. `[robots-txt]` with no `[[fetch]]` or `[[serve]]` entry naming `robots_txt` is refused when the settings load, and so by `ts config validate`, because the file would never be made.

**What goes.** - `handle_robots_txt`, the response builder and the route's own `Cache-Control: public, max-age=3600`. The file takes the deployment's cache policy for the path, as every file does.

- The route on the four adapters, with its path constant in each named-route table, the served-check that existed for it alone, and the route tests. Each adapter's tests now prove the path is no route of this server and that a refusal still tags every response.

`[robots-txt]` and every field, `refuse_all` and `allow_all`, the contributors as modules under `crates/robots-txt/<vendor>`, the holding through the loading cache, and `X-Robots-Tag` written by the adapters as they finalize every response.

## 3. What is not in this change

- A `HEAD` for `/robots.txt` is the publisher path's, which answers it as a `GET` (jwrosewell/trusted-server#17).
- The origin is asked for `/robots.txt` once per stored copy's life, where the route never asked it.

## 4. Acceptance

The pull request's description records how it was checked, with every commit linted and tested on its own, in order, the full local check on its head and CI green on all 19 checks, and 7 deliberate faults made one at a time and each caught by a test.

## Revision record

| Date       | Change                                                          |
| ---------- | --------------------------------------------------------------- |
| 2026-10-12 | First draft, from the pull request and its issue as they stand. |
