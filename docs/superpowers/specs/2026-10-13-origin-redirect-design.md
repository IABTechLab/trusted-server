# Design Spec: A folder origin's addresses in response headers, and a headers step for serve middleware

**Status:** Proposed, 2026-10-13. Raised as draft pull request IABTechLab/trusted-server#1378 (from jwrosewell/trusted-server#32, which is closed), for Tech Lab review.
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** `2026-10-13-publisher-object-storage-origin-design.md` and `2026-10-13-request-addressing-design.md`, both in IABTechLab/trusted-server#1342, which holds every design document of the stack
**Related PRs:** IABTechLab/trusted-server#1378, IABTechLab/trusted-server#1238 (merged, the header rewrite this change extends), jwrosewell/trusted-server#32 (closed)
**Issues:** IABTechLab/trusted-server#1330, IABTechLab/trusted-server#1128
**Last updated:** 2026-10-19

> **Why this spec exists.** The header rewrite merged in main (#1238) maps an address on the origin in a response header to the serving host, and knows the origin only as a host. Since the object storage design, the origin may be a folder of its host, and an address in that folder keeps the folder when it is mapped, so the reader is sent to an address that does not exist. The same change gives a serve middleware a way to set headers on a response that holds no page, a redirect among them.

## 1. Problem

The header rewrite (#1238) maps a URL on the origin in `Location`, `Content-Location`, `Refresh`, `Link` and the two Content Security Policy headers to the serving host, keeping its path, on every route of `handle_publisher_request`. The object storage design (`2026-10-13-publisher-object-storage-origin-design.md`) lets `origin_url` name a folder, such as `https://storage.example.com/bucket/site`, and a request for `/news` is sent to the origin as `/bucket/site/news`. An origin that redirects that request to `https://storage.example.com/bucket/site/news/` has the redirect mapped to `https://publisher.example/bucket/site/news/`. The reader's browser asks for that address, which the publisher proxy sends to the origin as `/bucket/site/bucket/site/news/`, an object that does not exist. An address on the origin's host outside the folder, such as `/bucket/other/`, is mapped the same way, though no address on the serving host leads there. The two exceptions the header rewrite makes to avoid a redirect loop compare the path the reader asked for with the path in the header, folder and all, so neither matches under a folder.

A second gap sits beside the first. A serve middleware works on the page in a response and writes headers through the action it returns for that page, so a response with no page in it, such as most redirects, reaches no middleware. A deployment that wants a policy of its own for the `Location` of a redirect has nowhere to put the policy.

## 2. Design

There is no setting. The header rewrite is given the path of `origin_url`, the folder the object storage design sends requests into, and maps a URL on the origin only when the URL is in that folder, without the folder. With no path in `origin_url`, or a path of `/`, the mapping is the header rewrite's as merged.

With `origin_url = "https://storage.example.com/bucket/site"` and a reader who asked for `https://publisher.example/news`:

| The origin answers                                                                                             | The reader receives                                                                                                                                  |
| -------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------- |
| `301` with `Location: https://storage.example.com/bucket/site/news/`                                           | `Location: https://publisher.example/news/`                                                                                                          |
| `302` with `Location: https://storage.example.com/bucket/site/news/?page=2#latest`                             | `Location: https://publisher.example/news/?page=2#latest`                                                                                            |
| `301` with `Location: https://storage.example.com/bucket/site`                                                 | `Location: https://publisher.example/`                                                                                                               |
| `301` with `Location: https://storage.example.com/bucket/other/news/`                                          | The header as the origin wrote it                                                                                                                    |
| `301` with `Location: https://storage.example.com/bucket/sitemap.xml`                                          | The header as the origin wrote it                                                                                                                    |
| `200` with `Link: <https://storage.example.com/bucket/site/style.css>; rel=preload`                            | `Link: <https://publisher.example/style.css>; rel=preload`                                                                                           |
| `200` with `Content-Security-Policy: img-src https://storage.example.com storage.example.com/bucket/site/img/` | `Content-Security-Policy: img-src https://storage.example.com https://publisher.example storage.example.com/bucket/site/img/ publisher.example/img/` |

**Which addresses are in the folder.** A URL on the origin, being one whose host and port the header rewrite already takes as the origin's, is in the folder when its path is the folder or beneath it. Dot segments are resolved first, as the reader's browser resolves them, so `/bucket/site/about/../news/` is `/bucket/site/news/` and in the folder, and `/bucket/site/../other/` is outside it. The folder counts only where it ends at a `/`, so `/bucket/site` does not come off `/bucket/sitemap.xml` or `/bucket/site-two/`. Paths are compared exactly, so `/Bucket/Site/` is not the folder. A folder written with a closing `/` is the same folder. The folder itself, with or without its closing `/`, is the serving host's root, `/`. A URL that stops at the origin's host names the host's root, which is outside the folder.

**What the reader receives.** The URL as the header rewrite maps it, with the folder taken off the path and the query and fragment kept. A protocol-relative URL stays protocol-relative.

**What is left alone.** A URL on the origin's host outside the folder is left as the origin wrote it, because no address on the serving host leads there, and the reader's browser follows it to the origin's host.

**Content Security Policy sources.** A source expression with no path permits every path on the origin's host, so its serving-host sibling has no path either and permits the whole serving host, as without a folder. A source with a path in the folder gains a sibling without the folder. A source with a path outside the folder gains no sibling.

**The loop exceptions.** The header rewrite leaves a `Location` or `Refresh` target unchanged where following it would bring the reader back to the address they asked for (#1238, and "Origin URLs in proxied response headers" in the configuration guide). With a folder, the path the reader asked for is compared with the path of the target without the folder, so both exceptions apply under a folder as they do without one.

**What names the origin.** The header rewrite decides, and this change does not alter it. The origin is the host of `origin_url` with its port, in any letter case, with or without a `Host` override, and the header rewrite maps a `Location` on a response of any status.

**A publisher addressed by a path prefix.** The request addressing design (`2026-10-13-request-addressing-design.md`) lets one host serve several publishers, each under a prefix of its own. The prefix is removed from the path before the request is routed, so the origin is asked for the path without it, and core never writes the removed prefix into a response. The header rewrite writes the request's scheme and host and the path of the URL, and nothing in `publisher.rs` reads the removed prefix, so the reader of such a publisher is sent to an address without the prefix, with or without a folder. With the example hook in the request addressing design, on a Fastly service whose `__KEY` selector is `{host}`, a reader of `https://front.example/publisher-a.example/news` whose origin answers `Location: https://origin.example/news/` receives `Location: https://front.example/news/`. The hook reads `news` as the publisher, so the reader is answered `421 Misdirected Request` unless a publisher has that name. A serve middleware puts the prefix back, through the headers step (section 3), and the step is given the rewritten `Location` and the request state a module's request preparer left. Core does not. No module in this repository reads the removed prefix, so none puts it back yet, and no test sends a request addressed by a path prefix through the header rewrite. A root-relative `Location`, such as `/news/`, is resolved by the reader's browser against the root of the host, so it loses the prefix in the same way.

**Where it happens.** In the header rewrite, `rewrite_origin_urls_in_headers` in `response_header_rewrite.rs`, which `handle_publisher_request` in `publisher.rs` calls where #1238 placed it. The folder is the `origin_path` field this change adds to `OriginHeaderRewrite`, which the handler fills from the path of `origin_url`, the path the object storage design builds the origin address from. The call comes after a file that core makes where the origin has none has replaced the origin's answer, and before the template cache gate, so every route the handler returns an origin answer through passes it. A copy of an asset from the asset cache enters at the same point as the origin's own answer. The `502` core sends in place of an unexpected `304` returns before the call, and carries no header from the origin. The serve middleware headers step (section 3) runs straight after the header rewrite.

**Adapters.** No adapter changes. The header rewrite needs a `3xx` to reach core with its `Location` as the origin wrote it, and that holds on all four adapters. The scheme and host it writes are the two values `RequestInfo::from_request` reads for the page pipeline, and how each adapter fills them differs.

| Adapter    | A `3xx` reaches core because                   | The scheme comes from                                                                                                  | The host comes from                                                               |
| ---------- | ---------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------- |
| Fastly     | The client has no mode that follows a redirect | The connection's TLS signal, with `Forwarded`, `X-Forwarded-Host`, `X-Forwarded-Proto` and `Fastly-SSL` stripped first | The `Host` header                                                                 |
| Spin       | The client has no mode that follows a redirect | The runtime's own URL for the request, with the same headers stripped                                                  | The runtime's own URL for the request                                             |
| Axum       | The client's redirect policy is none           | `Forwarded`, `X-Forwarded-Proto` or `Fastly-SSL` where the request carries one, otherwise `http`                       | `Forwarded` or `X-Forwarded-Host` where the request carries one, otherwise `Host` |
| Cloudflare | The client asks for manual redirect handling   | As Axum                                                                                                                | As Axum                                                                           |

The Axum and Cloudflare adapters set no TLS signal and strip only the configured client IP headers, so a client of either can choose the host its own redirect names, as it can already choose the host the page pipeline writes into a page. Each adapter already has a test that a redirect from its client is left for core to see, and those tests are not part of this change.

**Consent and privacy.** The mapped URL is made from the request's scheme and host and the path, query and fragment of the origin's URL, and the folder only shortens the path. Nothing is added to the address, no consent state is read and no request is made, so the change has no effect on consent.

**Why core and not a middleware.** The folder is part of the origin `[publisher]` names, and the header rewrite that maps the origin's addresses is in core (#1238). A middleware is told the host of `origin_url` and no origin path, and runs only where a `[[serve]]` entry covers the request path, so a deployment that named none would still send its readers to an address that does not exist. What a deployment decides for itself, such as where a redirect to another name of the site goes or whether to put a removed path prefix back, is a middleware's, through the headers step in section 3.

**What changes for a deployment.** A deployment whose `origin_url` has no path sees no change. For one with a path, a URL in the folder reaches the reader without the folder, where the header rewrite alone kept it, and a URL on the origin's host outside the folder is relayed as the origin wrote it. No setting is added or changed.

## 3. The serve middleware headers step

A serve middleware may set headers on a response the publisher path sends, a redirect included, except for the responses listed under "Where the step is called" below. The header rewrite in section 2 does not use the step. A deployment's own policy for a redirect does, such as where a `Location` for another name of the site goes, or putting a removed path prefix back.

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

**Where it runs.** In `handle_publisher_request`, once the origin's answer is in hand, straight after the header rewrite (section 2), which itself follows a file that core makes where the origin has none, and before core's cache rules, the modules' `finalize_response` hooks and the adapter's finalizing. A middleware therefore sees the headers as the header rewrite left them, and a header it sets is not rewritten. A cache header it sets can be replaced by the rules that follow, such as the `private, no-store` that core gives a page it has changed for one reader.

**Where the step is called.** The step is called at three places in `handle_publisher_request`:

1. The response the handler goes on with, being the origin's answer, a copy of an asset from the asset cache, or the file core makes where the origin has none.
2. The `502` core sends in place of an unexpected `304` from the origin.
3. The refusal of a streamed page by a middleware's preparation.

The step is not called for three kinds of response:

1. A refusal made before the origin is asked, being the `405` for a method a signed origin does not take and the `400` for a path with a dot segment under an origin folder.
2. A page served from the template cache, and the refusal of such a page by a middleware's preparation. That page has its headers built from the stored template, which replays a fixed list of policy headers, so a header the step set outside that list is missing from the page. The template cache is used only in the `Esi` assembly mode, so a deployment in the default `Inline` mode has no page the step misses on this account.
3. An answer made from an error, such as a failure to reach the origin, which `handle_publisher_request` returns to the adapter as an error.

## 4. What is not in this change

- **Another name for the same site.** An origin at `https://publisher.example` that redirects to `https://www.publisher.example` names a host that is not the origin's, so the header rewrite relays it and the reader follows it. A deployment whose origin does so names the host the origin redirects to in `origin_url`. A deployment that serves both names, and wants the reader kept on it across that redirect, decides where such a `Location` goes with a serve middleware of its own, through `Middleware::response_headers` (section 3). The step is asked about a redirect with no body, though not about every response the publisher path sends.
- **An origin that redirects an index file back to its folder.** With `directory_index` set, the index file is added to every request path that ends in `/`, so a request for `/news/` is sent to the origin as `/bucket/site/news/index.html`. An origin that answers that request with a redirect to `/bucket/site/news/` names the address the reader asked for. Under the header rewrite's second loop exception, a `GET` or `HEAD` redirect that sets no cookie is relayed as the origin wrote it, so the reader's browser leaves the deployment for the origin's address. One that sets a cookie is mapped, so the reader's browser asks again each time.
- **A `Location` that names no host, from an origin in a folder.** An origin under a path in `origin_url` that answers `Location: /bucket/site/news/` has the header relayed as written, as the header rewrite relays any relative URL, and the reader's browser then asks for a path with the folder in it.
- **Putting a removed path prefix back.** A serve middleware does so through the headers step, and no module in this repository reads the removed prefix yet. See "A publisher addressed by a path prefix" in section 2.
- **Following a redirect.** The publisher proxy follows no redirect itself. The reader's browser makes the next request.
- **Asset routes and the first-party proxy.** Their redirects are handled in `proxy.rs` and are unchanged.

## 5. Acceptance

Tests in `trusted-server-core`, where the code is, for the folder, in `response_header_rewrite.rs`:

- A URL in the folder loses the folder and keeps its query and fragment, the folder itself with or without its closing `/` becomes `/`, a dot segment that stays in the folder is resolved, a protocol-relative URL stays protocol-relative, a folder written with a closing `/` is the same folder, and an origin path of `/` takes nothing off (`folder_origin_maps_urls_in_the_folder_without_it`).
- Another folder of the host, a folder whose name only begins with the folder's, a name that begins as the folder does but does not end at a `/`, the host's root, a URL that stops at the host, a dot segment that climbs out of the folder and a path in another letter case are each left unchanged (`folder_origin_leaves_urls_outside_the_folder_unchanged`).
- Under a folder, a scheme change to the address the reader asked for and a cookieless `GET` redirect to it are kept, and a redirect to another address in the folder is mapped (`folder_origin_keeps_redirects_to_the_current_request_that_would_loop`).
- A source with no path gains the whole serving host, a source in the folder gains a sibling without the folder, and a source outside the folder gains none (`folder_origin_csp_sources_follow_the_folder`).

Tests through `handle_publisher_request` with a scripted origin, in `publisher.rs`:

- The reader receives the mapped `Location` on a buffered answer and on a streamed page, for a `GET`, a `HEAD`, a `POST`, a `PUT` and a `DELETE`, for `301`, `302`, `303`, `307` and `308` (`the_reader_receives_the_rewritten_location_on_every_route`), and on the request's own scheme (`the_reader_s_own_scheme_is_used`).
- With a folder in `origin_url`, the origin is asked under the folder, a redirect in the folder reaches the reader without it, and one outside it reaches the reader as written (`a_redirect_under_the_origin_path_reaches_the_reader_without_it`).
- An origin on plain HTTP or on a port is named for the reader, and the same host on another port is not (`an_origin_on_plain_http_or_on_a_port_is_named_for_the_reader`).
- Under a `Host` override, a `Location` naming the host of `origin_url` is mapped and one naming the override is not (`under_a_host_override_the_host_of_origin_url_is_the_origin_s_name`).
- A `Location` on a `200`, a `201`, a `404` and a `503` is mapped on the pass-through, streamed and buffered routes (`a_location_on_an_answer_that_is_no_redirect_is_named_for_the_reader`).
- A redirect to another host and one naming no host reach the reader as the origin wrote them, and a redirect with no `Location` gains none (`a_redirect_that_is_not_the_origin_s_own_reaches_the_reader_unchanged` and `a_redirect_with_no_location_gains_none`).
- The header rewrite's own tests pass on this branch, both its handler tests for the two loop exceptions and its tests in `response_header_rewrite.rs`.

Tests in `trusted-server-core` for the headers step:

- `PhaseEntries::for_path` names every entry that covers a path whatever its media type, each middleware once in entry order, leaves out an entry whose path does not cover the request, and names nothing when there are no entries (`for_path_names_every_entry_covering_the_path_whatever_its_media_type`, in `middleware/tests.rs`).
- `IntegrationRegistry::middleware_for_path` gives the middleware those names stand for, once each and in entry order, and leaves out a name no module supplies (`middleware_for_path_gives_every_covering_entry_s_middleware_once_in_entry_order`, in `integrations/registry.rs`).
- Through `handle_publisher_request`, a middleware named for the path sets the `Location` of a redirect with no body and keeps the header it set, leaves a `Location` it does not set as written, and is not asked when its entry covers another path (`a_serve_middleware_named_for_the_path_sets_a_redirects_location`, in `publisher.rs`).
- The file core makes where the origin has none carries the headers a middleware sets, and so does the `502` for an unexpected origin `304` (`a_made_file_carries_the_headers_a_serve_middleware_sets` and `the_502_for_an_unexpected_origin_304_carries_a_serve_middlewares_headers`, in `publisher.rs`).

Nothing tests the following:

- The step on the refusal of a streamed page by a middleware's preparation, which is the third place the step is called.
- That the step is not called for a refusal made before the origin is asked, for a page from the template cache, or for an answer made from an error.
- Several middleware setting a header of one name on one response.
- The folder on `Content-Location`, `Refresh` and `Link` through the handler. Each takes the same mapping the folder tests call directly.
- A request addressed by a path prefix, sent through the header rewrite.
- Any adapter, because no adapter changes, although each already tests that its client leaves a redirect for core to see.

Five deliberate faults were made, one in each run, and each was caught by a test: the handler giving the header rewrite no folder (`a_redirect_under_the_origin_path_reaches_the_reader_without_it`), the folder counted where it does not end at a `/` (`folder_origin_leaves_urls_outside_the_folder_unchanged`), a source with no path taking the folder rule (`folder_origin_csp_sources_follow_the_folder`), the loop exceptions comparing the path with the folder still in it (`folder_origin_keeps_redirects_to_the_current_request_that_would_loop`), and the folder compared before dot segments are resolved (`folder_origin_maps_urls_in_the_folder_without_it`).

## Revision record

| Date       | Change                                                                                                                                                                                                                                                                                                                                                   |
| ---------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 2026-10-13 | First draft.                                                                                                                                                                                                                                                                                                                                             |
| 2026-10-14 | Names its pull request and issue, and points a deployment that serves two names of the site at the serve middleware headers step.                                                                                                                                                                                                                        |
| 2026-10-15 | Fixes from review round 1. The headers step as a section with its tests and the responses it skips, who puts a removed path prefix back, the adapters, the setting, consent, failure and the index file loop, a table of what names the origin, the title now saying host, and the same sentence corrected in the changelog and the configuration guide. |
| 2026-10-15 | Corrections from review round 2. The two loops and the `421` under a removed path prefix, and the three kinds of response the headers step is not called for, named in the changelog and the configuration guide.                                                                                                                                        |
| 2026-10-19 | Follows the header rewrite merged in main (#1238) in place of a `Location` rule of its own, and gives the header rewrite the origin folder. The title, sections 1, 2, 4 and 5 and where the headers step runs change to match, and so do the changelog and the configuration guide.                                                                      |

## Review log

1. **Round 1, 2026-10-15.** A review in a fresh session, against `split/37-origin-redirect` at `73801ddb7`. Issues found, three of them blocking. The design called the serve middleware headers step outside the change though the series adds it, said the step is asked about every response the publisher path sends though three kinds of response skip it, and said nothing of the reader of a publisher served under a path prefix, who is sent to an address without the prefix. This revision adds a section for the step with its tests, lists the responses the step skips, says a serve middleware and not core puts the prefix back, corrects the partly true claims, fills the gaps in coverage, takes the advice the round gave, and corrects the same sentence in the changelog and the configuration guide.
2. **Round 2, 2026-10-15.** A lighter review in a fresh session, against this page at `9dafb089e`, of the round 1 fixes and the new text. Approved, with no blocking issues. The changelog and the configuration guide now name the two loops and the `421` for a reader under a removed path prefix, and name all three kinds of response the headers step is not called for.
