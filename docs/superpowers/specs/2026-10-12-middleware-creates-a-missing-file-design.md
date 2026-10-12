# Design Spec: A file the origin does not have is made by the middleware named for its path

**Status:** Proposed, 2026-10-12. Implemented on the fork as jwrosewell/trusted-server#17, ready for review, answering IABTechLab/trusted-server#1280. This pull request (#1084) carries every design document of the stack in one place.
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** `2026-08-27-integration-provider-seam-design.md`
**Related PRs:** #1094, jwrosewell/trusted-server#17
**Last updated:** 2026-10-12

> **Why this spec exists.** A publisher whose origin has no `ads.txt` gets the origin's `404`, however many sellers the settings give, because a middleware named for the path runs on the origin's file and the origin has none.

## 1. Problem

A publisher whose origin has no `ads.txt` gets the origin's `404`, however many sellers the settings give, because a middleware named for the path runs on the origin's file and the origin has none. Every well known file a module can add to has the same gap, and each would otherwise need a route of its own in core and on every adapter, as `robots.txt` has today.

## 2. Design

When the origin answers `404 Not Found` for a path a `[[fetch]]` entry with a `path` names middleware for, core runs that middleware on an empty document of the entry's media type, and its output is the file. The file is stored and served as a fetched one is. So a publisher whose origin has no `ads.txt` gets one made of the sellers lines alone.

```toml
[ads-txt]
modules = ["sellers"]

[ads-txt.sellers]
lines = ["ssp.example, 12345, DIRECT, f08c47fec0942fa0"]

[[fetch]]
media_type = "text/plain"
path = "/ads.txt"
middleware = ["ads-txt.sellers"]
```

**The rule.** A well known file is made where middleware is configured for its path, and is the origin's answer where none is.

- An entry without a path makes nothing, because a page the origin does not have is the origin's answer.
- Any answer from the origin other than `404` stands, an error among them.
- A `GET` and a `HEAD` are answered alike.

The middleware is an ordinary one, given an empty document. A middleware that adds to a file writes the same on an empty one, with nothing before its first line, which the sellers module already did.

## 3. What is not in this change

- `robots.txt` as a middleware made the same way, which is the next pull request.

## 4. Acceptance

The pull request's description records how it was checked, with every commit linted and tested on its own, in order, the full local check on its head and CI green on all 19 checks, and 6 deliberate faults made one at a time and each caught by a test.

## Revision record

| Date       | Change                                                          |
| ---------- | --------------------------------------------------------------- |
| 2026-10-12 | First draft, from the pull request and its issue as they stand. |
