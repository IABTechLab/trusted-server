# Design Spec: An origin's redirect to itself keeps the reader on the host they asked for

**Status:** Proposed, 2026-10-13. Raised as draft pull request jwrosewell/trusted-server#32, for Tech Lab review.
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** `2026-10-13-publisher-object-storage-origin-design.md` and `2026-10-13-request-addressing-design.md`, both in IABTechLab/trusted-server#1084, which holds every design document of the stack
**Related PRs:** jwrosewell/trusted-server#32
**Issues:** IABTechLab/trusted-server#1330
**Last updated:** 2026-10-15

> **Why this spec exists.** The publisher proxy relays an origin's redirect exactly as the origin wrote it, so a redirect the origin sends to itself hands the reader the origin's own address and their browser leaves the deployment. The same change gives a serve middleware a way to set headers on a response that holds no page, a redirect among them.

## 1. Problem

An origin redirects to itself for ordinary reasons, and a request path without its trailing slash is enough. It writes its own name in the `Location` header, because that is the name it was asked for. The publisher proxy relays the header as written, so the reader's browser follows it to the origin. From then on each page is the origin's own answer, with none of the publisher's `[[fetch]]` or `[[serve]]` middleware run on it, at an address the publisher never gave the reader.

A second gap sits beside the first. A serve middleware works on the page in a response and writes headers through the action it returns for that page, so a response with no page in it, such as most redirects, reaches no middleware. A deployment that wants a policy of its own for the `Location` of a redirect has nowhere to put the policy.

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

**What names the origin.** The host of the `Location`, with its port, is compared with the `Host` the origin was sent, which is `origin_host_header_override` where one is set and otherwise the host of `origin_url` with its port. Letter case in a host is ignored and the scheme of the `Location` is not compared. The table gives the cases, with `origin_url = "https://origin.example"` unless a row says otherwise.

| The `Location` names                                                                    | The origin's name | Because                                                                                 |
| --------------------------------------------------------------------------------------- | ----------------- | --------------------------------------------------------------------------------------- |
| `https://origin.example/news/`                                                          | Yes               | The host and port are the origin's                                                      |
| `https://ORIGIN.example/news/`                                                          | Yes               | Letter case in a host is ignored                                                        |
| `https://origin.example:443/news/`                                                      | Yes               | The usual port for the scheme counts as no port                                         |
| `http://origin.example/news/`                                                           | Yes               | The scheme of the `Location` is not compared                                            |
| `//origin.example/news/`                                                                | Yes               | A `Location` with no scheme names a host and is read on the scheme of the request       |
| `https://origin.example:8443/news/`                                                     | No                | The same host on another port is another origin                                         |
| `https://www.origin.example/news/`                                                      | No                | A host beneath the origin's is another host                                             |
| `https://origin.example.elsewhere.test/news/`                                           | No                | A host that only begins with the origin's name is another host                          |
| `https://origin.example@elsewhere.test/news/`                                           | No                | The text before an `@` is a user name, and the host is `elsewhere.test`                 |
| `ftp://origin.example/news/`                                                            | No                | Only an `http` or `https` address counts                                                |
| `https://origin.example/news/`, with `origin_url = "https://origin.example:8443"`       | No                | The same host without the port is another origin                                        |
| `https://www.example.com/news/`, with `origin_host_header_override = "www.example.com"` | Yes               | The override is the name the origin was sent                                            |
| `https://origin.example/news/`, with `origin_host_header_override = "www.example.com"`  | No                | The host in `origin_url` is where the connection goes and not what the origin is called |

**What the reader receives.** The scheme and host of the request, then the path, query and fragment of the `Location`. The page pipeline writes the same scheme and host in place of the origin's addresses in a page, but the page pipeline takes the host of `origin_url` as the origin's name even where an override is set, whereas this rule takes the override, which is the name the origin was sent. With an override set, the page pipeline and this rule therefore disagree on what the origin is called, and the page pipeline is not changed here. Where the origin's name is the request's own host, the header comes back naming the same host on the request's scheme. A request that names no host has nowhere to send the reader, so its redirect is left as written.

**The origin path.** Where `origin_url` has a path, the origin is that folder and not the whole host. A `Location` whose path is the origin path, or beneath it, comes back with the origin path removed, which reverses the way the origin address was built. A request for `/news` is sent to `/bucket/site/news`, and a redirect to `/bucket/site/news/` reaches the reader as `/news/`. A `Location` that is the origin path itself reaches the reader as `/`. The origin path counts only where it ends at a `/`, so `/bucket/site` does not come off `/bucket/sitemap.xml`. Paths are compared exactly, so `/Bucket/Site` is not `/bucket/site`. Dot segments in the `Location` are resolved before the comparison, so a path that climbs out of the folder is outside it. A `Location` on the origin's host and outside the origin path is left as written, because no address on the reader's host leads there.

**A publisher addressed by a path prefix.** The request addressing design (`2026-10-13-request-addressing-design.md`) lets one host serve several publishers, each under a prefix of its own. The prefix is removed from the path before the request is routed, so the origin is asked for the path without it, and core never writes the removed prefix into a response. The rewritten `Location` is made from the request's scheme and host and the path of the `Location`, and nothing in `publisher.rs` reads the removed prefix, so the reader of such a publisher is sent to an address without the prefix. With the example hook in the request addressing design, on a Fastly service whose `__KEY` selector is `{host}`, a reader of `https://front.example/publisher-a.example/news` whose origin answers `Location: https://origin.example/news/` receives `Location: https://front.example/news/`. The hook reads `news` as the publisher, so the reader is answered `421 Misdirected Request` unless a publisher has that name. Before this change the same reader followed the origin's own address to the origin. A serve middleware puts the prefix back, through the headers step (section 3), and the step is given the rewritten `Location` and the request state a module's request preparer left. Core does not. No module in this repository reads the removed prefix, so none puts it back yet, and no test sends a request addressed by a path prefix through the rule. A root-relative `Location`, such as `/news/`, is resolved by the reader's browser against the root of the host, so it loses the prefix in the same way with or without this change.

**What is left alone.** A `Location` naming any other host is the origin sending the reader somewhere else. A `Location` that names no host, such as `/news/`, `news/` or `?page=2`, is resolved by the reader's browser against the address it asked for, so it stays on the deployment as it is. A value that cannot be read as an address is relayed as it arrived.

**When the header cannot be rewritten.** A `Location` with bytes outside printable ASCII, and a rewritten value that is not a valid header value, are each relayed as the origin wrote them. When a response carries several `Location` headers, only the first is read, and when the first names the origin the rewritten value replaces all of them. None of these three cases has a test.

**Where it happens.** In one place, `handle_publisher_request` in `publisher.rs`, as soon as the origin's response is received. Two things come first and neither reads the `Location`. They are the removal of the three headers that describe the deployment (`X-Cache`, `X-Cache-Hits` and `X-Served-By`) and one debug log line that names the status and the number of headers. The streamed page, the buffered page, a `HEAD` and every other method return that one response, so each carries the rewritten header. A copy of an asset from the asset cache enters at the same point as the origin's own answer. Neither the template cache nor the asset cache keeps a response other than `200`, so no stored copy holds a redirect. The header is written for each request from that request's own scheme and host, after the platform has returned the origin's response to core. The rule itself is `rewritten_origin_redirect`, which takes the `Location`, the origin's host and path and the request's scheme and host, and returns the new value or nothing.

**Adapters.** No adapter changes. The rule needs a `3xx` to reach core with its `Location` as the origin wrote it, and that holds on all four adapters. The scheme and host the rule writes are the two values `RequestInfo::from_request` reads for the page pipeline, and how each adapter fills them differs.

| Adapter    | A `3xx` reaches core because                   | The scheme comes from                                                                                                  | The host comes from                                                               |
| ---------- | ---------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------- |
| Fastly     | The client has no mode that follows a redirect | The connection's TLS signal, with `Forwarded`, `X-Forwarded-Host`, `X-Forwarded-Proto` and `Fastly-SSL` stripped first | The `Host` header                                                                 |
| Spin       | The client has no mode that follows a redirect | The runtime's own URL for the request, with the same headers stripped                                                  | The runtime's own URL for the request                                             |
| Axum       | The client's redirect policy is none           | `Forwarded`, `X-Forwarded-Proto` or `Fastly-SSL` where the request carries one, otherwise `http`                       | `Forwarded` or `X-Forwarded-Host` where the request carries one, otherwise `Host` |
| Cloudflare | The client asks for manual redirect handling   | As Axum                                                                                                                | As Axum                                                                           |

The Axum and Cloudflare adapters set no TLS signal and strip only the configured client IP headers, so a client of either can choose the host its own redirect names, as it can already choose the host the page pipeline writes into a page. Each adapter already has a test that a redirect from its client is left for core to see, and those tests are not part of this change.

**Consent and privacy.** The rewritten `Location` is made from the request's scheme and host and the path, query and fragment of the `Location`. The rule adds nothing to the address, reads no consent state and makes no request, so it has no effect on consent. Its debug log line is a fixed sentence that prints neither address.

**Why core and not a middleware.** The redirect answers the request core made to the one origin `[publisher]` names, and the rule uses only that origin's own name and path and the request's own scheme and host. The rule names no vendor and reads no module's settings. Rewriting the origin's own address is part of the origin fetch, the step that turns the origin's answer into the deployment's. Core already moves the origin's address in a page to the reader's host before any middleware sees the page, and a `Location` naming the origin is the same address in a header. The headers step in section 3 lets a middleware change a header, and three things keep this rule out of a middleware. A middleware is told the host of `origin_url` and no origin path, and with an override set that host is not the name the origin was sent. A middleware runs only where a `[[serve]]` entry covers the request path, so a deployment that named none would still send its readers off the deployment. And the headers step is not asked about every response (section 3). What a deployment decides for itself, such as where a redirect to another name of the site goes or whether to put a removed path prefix back, is a middleware's, through that step.

**Why no setting, and what changes for a deployment.** A `Location` naming the origin takes the reader off the deployment that is serving them, which no deployment asks for by naming an origin. A deployment whose origin sends no redirect to itself sees no change. For a deployment whose origin does, the reader stays on the deployment where before the reader's browser left it. Section 4 gives the two cases where the reader's browser loops instead. No setting is added or changed. `origin_host_header_override` gains a second job, being the name a redirect is compared with, and the configuration guide says so under `origin_url` and under the override. Loading the settings already refuses an override that is not a host name, an IPv4 address or a bracketed IPv6 address with an optional port, so the comparison never reads anything else.

## 3. The serve middleware headers step

A serve middleware may set headers on a response the publisher path sends, a redirect included, except for the responses listed under "Where the step is called" below. The redirect rule in section 2 does not use the step. A deployment's own policy for a redirect does, such as where a `Location` for another name of the site goes, or putting a removed path prefix back.

**What it adds.** A method on `Middleware`, with a default that returns no headers, and the read only view of the response that the method is given, both in `crates/trusted-server-core/src/middleware.rs`. A middleware written before the method compiles and behaves as before, and a request path no `[[serve]]` entry covers sees no change.

```rust
pub trait Middleware: Send + Sync {
    fn response_headers(
        &self,
        context: &MiddlewareContext<'_>,
        response: &ResponseView<'_>,
    ) -> Vec<ResponseHeader> {
        Vec::new()
    }
}

pub struct ResponseView<'a> {
    pub status: StatusCode,
    pub headers: &'a http::HeaderMap,
}
```

`ResponseView` holds the status of the response and a borrowed view of its headers.

**Which middleware are asked.** Every middleware named by an entry of `[[serve]]` whose path covers the request path, whatever the entry's media type, in entry order and each once. A header is not about a body, so the media type, which says what the body is, is not compared. `PhaseEntry::covers_path` says whether an entry covers a path, `PhaseEntries::for_path` gives the names, and `IntegrationRegistry::middleware_for_path` gives the middleware, leaving out a name no running module supplies. Where no entry covers the path, nothing is built and nothing is asked.

**What a middleware is given.** A `MiddlewareContext` for the serve phase, made as for a page. The context carries the request's host and scheme, the host of `origin_url`, the platform identity, and the request state the modules' request preparers left, copied into a document state of its own. Nothing a middleware prepared is available, because the step runs no preparation.

**What a middleware may do.** Return headers. Each is written on the response in middleware order, replacing any header of the same name, so a later middleware replaces an earlier one's header of that name. The method cannot change the status or the body. A header the same middleware sets for a page through the action it returns from `create` is written later, in the page pass, and replaces the header set here.

**Where it runs.** In `handle_publisher_request`, once the origin's answer is in hand, after the redirect rule, after a file that core makes where the origin has none has replaced the origin's answer, and before core's cache rules, the modules' `finalize_response` hooks and the adapter's finalizing. A middleware therefore sees the rewritten `Location`, and a cache header it sets can be replaced by the rules that follow, such as the `private, no-store` that core gives a page it has changed for one reader.

**Where the step is called.** The step is called at three places in `handle_publisher_request`:

1. The response the handler goes on with, being the origin's answer, a copy of an asset from the asset cache, or the file core makes where the origin has none.
2. The `502` core sends in place of an unexpected `304` from the origin.
3. The refusal of a streamed page by a middleware's preparation.

The step is not called for three kinds of response:

1. A refusal made before the origin is asked, being the `405` for a method a signed origin does not take and the `400` for a path with a dot segment under an origin folder.
2. A page served from the template cache, and the refusal of such a page by a middleware's preparation. That page has its headers built from the stored template, which replays a fixed list of policy headers, so a header the step set outside that list is missing from the page. The template cache is used only in the `Esi` assembly mode, so a deployment in the default `Inline` mode has no page the step misses on this account.
3. An answer made from an error, such as a failure to reach the origin, which `handle_publisher_request` returns to the adapter as an error.

## 4. What is not in this change

- **Another name for the same site.** An origin at `https://publisher.example` that redirects to `https://www.publisher.example` names a host that is not the one it was asked for, so the header is relayed and the reader follows it. A deployment whose origin does so names the host the origin redirects to, in `origin_url` or in `origin_host_header_override`. A deployment that serves both names, and wants the reader kept on it across that redirect, decides where such a `Location` goes with a serve middleware of its own, through `Middleware::response_headers` (section 3). The step is asked about a redirect with no body, though not about every response the publisher path sends.
- **An origin that redirects away from the scheme it is asked on.** The scheme of the `Location` is not compared, and the reader is sent to their own scheme. An origin that `origin_url` reaches over plain HTTP, and that answers each request with the same address on `https`, sends the reader back to the address they asked for, which gets the same answer again. `origin_url` names the scheme the origin serves its pages on.
- **An origin that redirects an index file back to its folder.** With `directory_index` set, the index file is added to every request path that ends in `/`, so a request for `/news/` is sent to the origin as `/news/index.html`. An origin that answers that request with a redirect to `/news/` sends the reader back to the address they asked for, which gets the same answer again. Before this change the reader followed the origin's own address and left the deployment, and now the reader's browser asks again each time.
- **A `Location` that names no host, from an origin in a folder.** An origin under a path in `origin_url` that answers `Location: /bucket/site/news/` has the header relayed as written, and the reader's browser then asks for a path with the folder in it.
- **Putting a removed path prefix back.** A serve middleware does so through the headers step, and no module in this repository reads the removed prefix yet. See "A publisher addressed by a path prefix" in section 2.
- **The name the page pipeline gives the origin.** With an override set, the page pipeline still moves the host of `origin_url` in a page and not the override. This change leaves the page pipeline as it was.
- **Following a redirect.** The publisher proxy follows no redirect itself. The reader's browser makes the next request.
- **Any header but `Location`.** An address in `Content-Location`, `Link` or `Refresh` is relayed as before.
- **Asset routes and the first-party proxy.** Their redirects are handled in `proxy.rs` and are unchanged.

## 5. Acceptance

Tests in `trusted-server-core`, where the code is, for the redirect rule:

- A `Location` naming the origin comes back on the request's scheme and host with its path, query and fragment, for an origin on `https` and on plain HTTP, with and without a port, named by an address in brackets, in any letter case, and with the scheme left off.
- With `origin_host_header_override` set, a `Location` naming the override is rewritten and one naming the host of `origin_url` is not.
- A `Location` naming another host, a host beneath the origin's, a host that only starts or ends with the origin's name, or the origin's name on a different port is left as written. So is a path, a query or a fragment alone, an address that is not `http` or `https`, a value that is no address, and any `Location` when the request names no host.
- With a path in `origin_url`, a `Location` at or beneath that path comes back without it, and the origin path itself comes back as `/`. One beside it, above it, in another letter case or climbing out of it is left as written, and the origin path comes off only where it ends at a `/`.
- Through `handle_publisher_request` with a scripted origin, the reader receives the rewritten `Location` on a buffered answer and on a streamed page, for a `GET`, a `HEAD`, a `POST`, a `PUT` and a `DELETE`, for `301`, `302`, `303`, `307` and `308`, and on the request's own scheme.
- Through the same handler, a redirect to another host, a redirect naming no host, a `Location` on a `200`, a `201`, a `404` and a `503`, and a redirect with no `Location` each reach the reader as the origin sent them.

Tests in `trusted-server-core` for the headers step:

- `PhaseEntries::for_path` names every entry that covers a path whatever its media type, each middleware once in entry order, leaves out an entry whose path does not cover the request, and names nothing when there are no entries (`for_path_names_every_entry_covering_the_path_whatever_its_media_type`, in `middleware/tests.rs`).
- `IntegrationRegistry::middleware_for_path` gives the middleware those names stand for, once each and in entry order, and leaves out a name no module supplies (`middleware_for_path_gives_every_covering_entry_s_middleware_once_in_entry_order`, in `integrations/registry.rs`).
- Through `handle_publisher_request`, a middleware named for the path sets the `Location` of a redirect with no body and keeps the header it set, leaves a `Location` it does not set as written, and is not asked when its entry covers another path (`a_serve_middleware_named_for_the_path_sets_a_redirects_location`, in `publisher.rs`).
- The file core makes where the origin has none carries the headers a middleware sets, and so does the `502` for an unexpected origin `304` (`a_made_file_carries_the_headers_a_serve_middleware_sets` and `the_502_for_an_unexpected_origin_304_carries_a_serve_middlewares_headers`, in `publisher.rs`).

Nothing tests the following:

- The step on the refusal of a streamed page by a middleware's preparation, which is the third place the step is called.
- That the step is not called for a refusal made before the origin is asked, for a page from the template cache, or for an answer made from an error.
- Several middleware setting a header of one name on one response.
- A `Location` with bytes outside printable ASCII, a rewritten value that is not a valid header value, and several `Location` headers on one response.
- A request addressed by a path prefix, sent through the redirect rule.
- Any adapter, because no adapter changes, although each already tests that its client leaves a redirect for core to see.

Sixteen deliberate faults were made, one in each run, and each was caught by a test: a `Location` rewritten on any status, the port left out of the comparison, the origin path not removed, the origin path removed where it does not end at a `/`, a `Location` outside the origin path rewritten with its path whole, the fragment dropped, the query dropped, an address that is not `http` or `https` rewritten, a request with no host rewritten, a `Location` naming no host resolved against the origin, the host of `origin_url` compared where an override is set, a `Location` with the scheme left off not read, the host compared in exact letter case, the reader always sent to `https`, the origin path not passed by the handler, and a `Location` that is the origin path itself rewritten with an empty path and not `/`. All sixteen are in the redirect rule and the handler's use of the rule, and none is recorded for the headers step.

## Review log

1. **Round 1, 2026-10-15.** A review in a fresh session, against `split/37-origin-redirect` at `73801ddb7`. Issues found, three of them blocking. The design called the serve middleware headers step outside the change though the series adds it, said the step is asked about every response the publisher path sends though three kinds of response skip it, and said nothing of the reader of a publisher served under a path prefix, who is sent to an address without the prefix. This revision adds a section for the step with its tests, lists the responses the step skips, says a serve middleware and not core puts the prefix back, corrects the partly true claims, fills the gaps in coverage, takes the advice the round gave, and corrects the same sentence in the changelog and the configuration guide.

## Revision record

| Date       | Change                                                                                                                                                                                                                                                                                                                                                   |
| ---------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 2026-10-13 | First draft.                                                                                                                                                                                                                                                                                                                                             |
| 2026-10-14 | Names its pull request and issue, and points a deployment that serves two names of the site at the serve middleware headers step.                                                                                                                                                                                                                        |
| 2026-10-15 | Fixes from review round 1. The headers step as a section with its tests and the responses it skips, who puts a removed path prefix back, the adapters, the setting, consent, failure and the index file loop, a table of what names the origin, the title now saying host, and the same sentence corrected in the changelog and the configuration guide. |
