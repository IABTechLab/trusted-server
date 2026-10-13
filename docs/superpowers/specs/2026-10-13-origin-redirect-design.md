# Design Spec: An origin's redirect to itself keeps the reader on the address they asked for

**Status:** Proposed, 2026-10-13. Implemented on a local working branch, with no pull request raised yet.
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** `2026-10-13-publisher-object-storage-origin-design.md`
**Related PRs:** none yet
**Last updated:** 2026-10-13

> **Why this spec exists.** The publisher proxy relays an origin's redirect exactly as the origin wrote it, so a redirect the origin sends to itself hands the reader the origin's own address and their browser leaves the deployment.

## 1. Problem

An origin redirects to itself for ordinary reasons, and a request path without its trailing slash is enough. It writes its own name in the `Location` header, because that is the name it was asked for. The publisher proxy relays the header as written, so the reader's browser follows it to the origin. From then on each page is the origin's own answer, with none of the publisher's `[[fetch]]` or `[[serve]]` middleware run on it, at an address the publisher never gave the reader.

## 2. Design

There is no setting. When the origin answers a request with a redirect, the publisher proxy reads the `Location` header, and where it names the origin the reader is sent to the same place on the scheme and host they asked for.

With `origin_url = "https://origin.example"` and a reader who asked for `https://publisher.example/news`:

| The origin answers                                                | The reader receives                                       |
| ----------------------------------------------------------------- | --------------------------------------------------------- |
| `301` with `Location: https://origin.example/news/`               | `Location: https://publisher.example/news/`               |
| `302` with `Location: https://origin.example/news/?page=2#latest` | `Location: https://publisher.example/news/?page=2#latest` |
| `307` with `Location: //origin.example/news/`                     | `Location: https://publisher.example/news/`               |
| `301` with `Location: https://www.origin.example/news/`           | The header as the origin wrote it                         |
| `301` with `Location: https://elsewhere.example/news/`            | The header as the origin wrote it                         |
| `301` with `Location: /news/`                                     | The header as the origin wrote it                         |
| `200` or `201` with `Location: https://origin.example/news/`      | The header as the origin wrote it                         |

**Which responses.** A response with a status from `300` to `399` and a `Location` header. A `Location` on any other status is left as written, so a `201` naming what was created is not touched.

**What names the origin.** The host of the `Location`, with its port, is compared with the `Host` the origin was sent, which is `origin_host_header_override` where one is set and otherwise the host of `origin_url` with its port. Letter case is ignored. A port that is the usual one for the scheme the `Location` is written with counts as no port, so `https://origin.example:443/news` names `origin.example`. An origin on a port and the same host without one are different origins. The scheme of the `Location` is not compared, so an origin asked over plain HTTP that answers with its `https` address still names itself. Only an `http` or `https` address counts. A `Location` that leaves the scheme off, as `//origin.example/news/` does, names a host and is read on the scheme of the request. With an override set, the host in `origin_url` is where the connection goes and not what the origin is called, so a `Location` naming that host is left as written.

**What the reader receives.** The scheme and host of the request, which are the two values the page pipeline already uses to rewrite the origin's addresses in a page, then the path, query and fragment of the `Location`. Where the origin's name is the request's own host, the header comes back naming the same host on the request's scheme. A request that names no host has nowhere to send the reader, so its redirect is left as written.

**The origin path.** Where `origin_url` has a path, the origin is that folder and not the whole host. A `Location` whose path is the origin path, or beneath it, comes back with the origin path removed, which reverses the way the origin address was built. A request for `/news` is sent to `/bucket/site/news`, and a redirect to `/bucket/site/news/` reaches the reader as `/news/`. The origin path counts only where it ends at a `/`, so `/bucket/site` does not come off `/bucket/sitemap.xml`. Dot segments in the `Location` are resolved before the comparison, so a path that climbs out of the folder is outside it. A `Location` on the origin's host and outside the origin path is left as written, because no address on the reader's host leads there.

**What is left alone.** A `Location` naming any other host is the origin sending the reader somewhere else, and a host beneath the origin's, such as `www.origin.example`, is another host. A `Location` that names no host, such as `/news/`, `news/` or `?page=2`, is resolved by the reader's browser against the address it asked for, so it stays on the deployment as it is. A value that cannot be read as an address is relayed as it arrived.

**Where it happens.** In one place, `handle_publisher_request` in `publisher.rs`, at the point the origin's response is received and before anything else reads it. The streamed page, the buffered page, a `HEAD` and every other method return that one response, so each carries the rewritten header. The header is written for each request from that request's own scheme and host, after the origin's response has been returned to core. The rule itself is `rewritten_origin_redirect`, which takes the `Location`, the origin's host and path and the request's scheme and host, and returns the new value or nothing.

**Why core.** The redirect answers the request core made to the one origin `[publisher]` names, and the rule uses only that origin's own name and path and the request's own scheme and host. It names no vendor and reads no module's settings.

**Why no setting.** A `Location` naming the origin takes the reader off the deployment that is serving them, which no deployment asks for by naming an origin. A deployment whose origin sends no redirect to itself sees no change.

## 3. What is not in this change

- **Another name for the same site.** An origin at `https://publisher.example` that redirects to `https://www.publisher.example` names a host that is not the one it was asked for, so the header is relayed and the reader follows it. A deployment whose origin does so names the host the origin redirects to, in `origin_url` or in `origin_host_header_override`.
- **An origin that redirects away from the scheme it is asked on.** The scheme of the `Location` is not compared, and the reader is sent to their own scheme. An origin that `origin_url` reaches over plain HTTP, and that answers each request with the same address on `https`, sends the reader back to the address they asked for, which gets the same answer again. `origin_url` names the scheme the origin serves its pages on.
- **A `Location` that names no host, from an origin in a folder.** An origin under a path in `origin_url` that answers `Location: /bucket/site/news/` has the header relayed as written, and the reader's browser then asks for a path with the folder in it.
- **Following a redirect.** The publisher proxy follows no redirect itself. The reader's browser makes the next request.
- **Any header but `Location`.** An address in `Content-Location`, `Link` or `Refresh` is relayed as before.
- **Asset routes and the first-party proxy.** Their redirects are handled in `proxy.rs` and are unchanged.

## 4. Acceptance

Tests in `trusted-server-core`, where the code is:

- A `Location` naming the origin comes back on the request's scheme and host with its path, query and fragment, for an origin on `https` and on plain HTTP, with and without a port, named by an address in brackets, in any letter case, and with the scheme left off.
- With `origin_host_header_override` set, a `Location` naming the override is rewritten and one naming the host of `origin_url` is not.
- A `Location` naming another host, a host beneath the origin's, a host that only starts or ends with the origin's name, or the origin's name on a different port is left as written. So is a path, a query or a fragment alone, an address that is not `http` or `https`, a value that is no address, and any `Location` when the request names no host.
- With a path in `origin_url`, a `Location` at or beneath that path comes back without it. One beside it, above it or climbing out of it is left as written, and the origin path comes off only where it ends at a `/`.
- Through `handle_publisher_request` with a scripted origin, the reader receives the rewritten `Location` on a buffered answer and on a streamed page, for a `GET`, a `HEAD`, a `POST`, a `PUT` and a `DELETE`, for `301`, `302`, `303`, `307` and `308`, and on the request's own scheme.
- Through the same handler, a redirect to another host, a redirect naming no host, a `Location` on a `200`, a `201`, a `404` and a `503`, and a redirect with no `Location` each reach the reader as the origin sent them.

Sixteen deliberate faults were made, one in each run, and each was caught by a test: a `Location` rewritten on any status, the port left out of the comparison, the origin path not removed, the origin path removed where it does not end at a `/`, a `Location` outside the origin path rewritten with its path whole, the fragment dropped, the query dropped, an address that is not `http` or `https` rewritten, a request with no host rewritten, a `Location` naming no host resolved against the origin, the host of `origin_url` compared where an override is set, a `Location` with the scheme left off not read, the host compared in exact letter case, the reader always sent to `https`, the origin path not passed by the handler, and the origin path itself given an empty path.

## Revision record

| Date       | Change       |
| ---------- | ------------ |
| 2026-10-13 | First draft. |
