# Design Spec: Headers that describe the deployment stay off a publisher's pages

**Status:** Proposed, 2026-10-12. Implemented on the fork as jwrosewell/trusted-server#10, ready for review, answering IABTechLab/trusted-server#1273. This pull request (#1084) carries every design document of the stack in one place.
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** `2026-08-27-integration-provider-seam-design.md`
**Related PRs:** #1094, jwrosewell/trusted-server#10
**Last updated:** 2026-10-12

> **Why this spec exists.** `X-Served-By`, `X-Cache` and `X-Cache-Hits` arrive on a response from the publisher's origin, or from a cache between this server and the origin, and the server passes them on with every page.

## 1. Problem

`X-Served-By`, `X-Cache` and `X-Cache-Hits` arrive on a response from the publisher's origin, or from a cache between this server and the origin, and the server passes them on with every page. Nothing on a page needs them. They describe how the site is built, to anyone who asks for any page, without that being a decision anyone made.

## 2. Design

A response no longer carries `X-Served-By`, `X-Cache` or `X-Cache-Hits`, unless the request was for a path beneath `/_ts`.

| Header         | What it tells whoever reads it                         |
| -------------- | ------------------------------------------------------ |
| `X-Served-By`  | Which node answered                                    |
| `X-Cache`      | Whether a cache answered                               |
| `X-Cache-Hits` | How many times that cache has answered with the object |

**Why.** The three arrive on a response from the publisher's origin, or from a cache between this server and the origin, and this server passed them on with every page. Nothing on a page needs them. What they give is a description of how the site is built, to anyone who asks for any page, without that being a decision anyone made.

A question about a deployment has addresses of its own. The inspection pages (jwrosewell/trusted-server#4) answer beneath `/_ts`, and so does attestation (jwrosewell/trusted-server#7) unless its endpoint is moved, and what each shows is the publisher's decision. The rule here is the same one. What describes the deployment is answered where a deployment is asked about itself, and is left off everything else.

```rust
pub const ROUTE_NAMESPACE: &str = "/_ts";

pub fn strip_diagnostic_headers<T>(path: &str, namespace: &str, response: &mut Response<T>)
```

Core's `http_util` gains the function, and the finalize middleware of each of the four adapters calls it with the request's path before it finalizes the response.

- **A path is in the namespace only at a boundary.** `/_ts` and `/_ts/permissions` are. `/_tsx` and `/_ts-archive` are a publisher's pages, and are stripped like any other. A prefix match would let a page keep the headers by its name alone.
- **The removal comes before the finalize step.** A header that finalizing writes is kept, an operator's `[response_headers]` among them. An operator who wants `X-Cache` on every page can still set it there.
- **The namespace is a parameter**, so a test proves the boundary with a namespace of its own.

**What breaks.** An operator who reads `X-Cache` or `X-Served-By` from a page fetched through this server, to see whether the origin's cache answered, no longer sees them. The origin still sends them, and they can be read at the origin.

## 3. What is not in this change

- `X-TS-Version` and `X-TS-ENV`, which the Fastly adapter writes on every response when it finalizes it, and `X-Geo-Info-Available`, which every adapter writes. They are written after the removal and are left as they are.
- A response the router makes itself for a request it does not route. It does not pass the finalize middleware, and carries nothing from an origin.

## 4. Acceptance

The pull request's description records how it was checked, with every commit linted and tested on its own, in order, the full local check on its head and CI green on all 19 checks, and 13 deliberate faults made one at a time and each caught by a test.

## Revision record

| Date       | Change                                                          |
| ---------- | --------------------------------------------------------------- |
| 2026-10-12 | First draft, from the pull request and its issue as they stand. |
