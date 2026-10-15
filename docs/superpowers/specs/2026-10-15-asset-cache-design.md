# Design Spec: The publisher's assets held through the loading cache

**Status:** Proposed, 2026-10-15. Written after the code, for draft pull request IABTechLab/trusted-server#1375
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** `2026-10-12-loading-cache-design.md`, `2026-09-15-852-template-and-origin-caching-design.md`, `2026-04-28-multi-backend-asset-proxy-design.md`, `2026-05-19-asset-s3-auth-fastly-io-design.md`
**Related PRs:** IABTechLab/trusted-server#1375, reviewed on jwrosewell/trusted-server#29
**Issues:** IABTechLab/trusted-server#1324
**Last updated:** 2026-10-15

> **Why this spec exists.** A file that is not a page, whether the publisher's origin sends it or an asset route fetches it, is asked of its origin on every request and by every instance, so a burst of readers for one file is a burst of fetches.

Every design document of the stack is in IABTechLab/trusted-server#1342, including the specs named above.

## 1. Problem

`handle_publisher_request` in `crates/trusted-server-core/src/publisher.rs` keeps a page as a template in the template cache and sends every other request to the publisher's origin. `handle_asset_proxy_request` in `crates/trusted-server-core/src/proxy.rs` fetches each file of an asset route from the route's origin. Core keeps no copy of either kind of file. A deployment with a host cache in front of core already has its files held there. A deployment with nothing in front of core fetches every file from the origin for every reader, on every instance.

The loading cache that core declares (`2026-10-12-loading-cache-design.md`) loads a missing value once for all the requests that ask for it at the same moment, and gives each of them the value. Each feature that holds answers this way selects its cache in its own `[cache.<name>]` table. The robots.txt answers were the first feature to do so. Assets are the second.

## 2. Design

Core holds the origin's response for a file in two loading caches, `assets-vary` and `assets`, and the one table `[cache.assets]` selects both. The publisher path and the asset routes both call `asset_from_cache` in `publisher.rs`. That function answers with a stored copy, or with the response its own fetch made, or with nothing, and a caller that gets nothing asks the origin as it did before this change.

**Where the code is.**

| File                                                 | What it holds                                                                                                                                                                                                 |
| ---------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `crates/trusted-server-core/src/source_cache.rs`     | `get_or_fetch`, which finds the copy for a request through the record and the copy caches, and the record of which request headers an address varies on                                                       |
| `crates/trusted-server-core/src/asset_cache.rs`      | `AssetCacheKey`, `AssetCopy` and `AssetEntry`, which say what is stored, how it is written and read, and how a copy is served                                                                                 |
| `crates/trusted-server-core/src/publisher.rs`        | The gate in `handle_publisher_request`, then `asset_from_cache`, `load_asset`, `asset_cache_ttl`, `HandBack` and `origin_response_may_be_kept`, which `template_cache_ttl` now calls as well                  |
| `crates/trusted-server-core/src/proxy.rs`            | `asset_route_copy_key`, and the lookup in `handle_asset_proxy_request`                                                                                                                                        |
| `crates/trusted-server-core/src/settings.rs`         | `CacheSettings.assets`, `ASSETS_CACHE`, `ASSETS_VARY_CACHE`, and `CacheSettings::loading_caches`, which now lists both names                                                                                  |
| `crates/trusted-server-core/src/integrations/mod.rs` | `declared_caches`, which declares both names as core's own                                                                                                                                                    |
| `crates/trusted-server-core/src/platform/`           | `hash_length_prefixed` in `template_cache.rs`, which `TemplateCacheKey::to_cache_key` and `AssetCacheKey::to_cache_key` share, and `MemoryLoadingCache::held` and `asset_copy_lifetimes` in `test_support.rs` |

**The key.** `AssetCacheKey` has five inputs, and the two callers fill them differently.

| Input             | Publisher path                                                                                                  | Asset route                                                 |
| ----------------- | --------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------- |
| `url`             | The URL asked of the origin, with its query                                                                     | The route's target URL, after the route's query policy      |
| `request_host`    | The host the reader asked                                                                                       | The host of the request URI, else its `Host` header         |
| `request_scheme`  | The scheme the reader asked over                                                                                | The scheme of the request URI, `https` when it has none     |
| `origin_identity` | The publisher's `origin_url` and the `Host` sent to the origin, joined by a NUL byte                            | The route's `origin_url` and `prefix`, joined by a NUL byte |
| `fingerprint`     | `template_fingerprint`, a digest of the build, the script bundle, the public prefix and the serialized settings | Empty                                                       |

`AssetCacheKey::to_cache_key` writes a schema version and the five inputs into one SHA-256, each with its length before it, and returns `ts-asset-cache-v1-` and the digest in hex. No input appears in the key. The route's `origin_identity` carries the route's prefix, so two routes to one address keep their own copies. The reader's host and scheme are in the key although the copy is kept as the origin sent it, so two hosts that serve one publisher each keep a copy.

`asset_address` joins that digest to the publisher's prefix (`services.server_key_prefix()`) as a `SourceAddress`. Every key in the store starts with the prefix, so two publishers sharing a store never share a record or a copy.

An address has one record, in `assets-vary`, and any number of copies, in `assets`. `SourceVary::of` reads the origin's `Vary` into the record as lowercase header names, sorted and each named once. A copy's key is the address and a 128-bit digest (`variant` in `source_cache.rs`) of the request's values for exactly those headers, as RFC 9111 section 4.1 describes. The digest tells apart an absent header, an empty one, and repeated values in a different order. The request headers are those sent to the origin. On the publisher path they are the headers after `rewrite_origin_request` has restricted `Accept-Encoding`. On an asset route they are the headers built in `handle_asset_proxy_request`, which include the route's own credentials when the route has `auth`.

**When a stored asset is used.** The publisher path looks for a copy only when every one of these holds, as `request_can_use_asset_copy` in `handle_publisher_request` computes them.

1. The method is `GET`.
2. The request is not for a document. `is_html_document_request` reads `Sec-Fetch-Dest`, where `document`, `embed`, `fencedframe`, `frame`, `iframe` and `object` count as documents, and falls back to the navigation check when the header is absent.
3. The request names a host.
4. The request carries no `Authorization` header.
5. `request_bypasses_template_cache` finds none of `Range`, `If-Range`, `If-Match`, `If-None-Match`, `If-Modified-Since` or `If-Unmodified-Since`, and no `Cache-Control` that asks for the origin, being `no-cache`, `no-store`, `min-fresh`, an unreadable value, or a `max-age` that is not zero.
6. No module left state on the request, and no integration marked the response as personalized.
7. The loading cache that `[cache.assets]` selects keeps values (`PlatformLoadingCache::keeps_values`). `NoLoadingCache` answers false.

An asset route looks for a copy when `asset_route_copy_key` finds the method `GET`, no `Authorization`, none of the conditions in item 5, a host in the request URI or the `Host` header, and a cache that keeps values, and when no image optimizer applies to the request. The route does not check items 2 and 6. A request that fails any check is served exactly as before this change.

**What is stored.** `load_asset` fetches the file with `GET` and the prepared request's headers, then tests the response in this order. The first test the response fails ends the checks. The response is then not stored for other readers, and the request whose load fetched it is served that response.

1. `Vary` is `*`, names `Cookie`, or cannot be read.
2. `asset_cache_ttl` refuses the response, which happens when the status is not `200`, when the response sets a cookie, when it carries an edge cache header other than `Surrogate-Control`, when its content type is HTML, or when `origin_shared_ttl` finds no usable lifetime. That function refuses `private`, `no-store` and `no-cache`, a repeated or unreadable `max-age` or `s-maxage`, and a response with no lifetime left. Otherwise the lifetime comes from `Surrogate-Control`, else `s-maxage`, else `max-age`, else `Expires` measured from `Date`. The current age is taken off, being the larger of the `Age` header and the age `Date` implies, and `store_seconds` caps the result. The response does not need `public`.
3. `Content-Length` is over 1 MiB (`MAX_ASSET_COPY_BYTES`).
4. A header value is not visible text and so cannot be written down (`AssetCopy::kept_headers`).
5. The body outgrows 1 MiB, or fails, while `read_whole_or_hand_back` reads it.

A stored copy is the origin's response, headers and body, as the origin sent it, with its `Content-Encoding` left on. `AssetCopy::decoded` is always false. The headers `age`, `connection`, `content-length`, `keep-alive`, `proxy-authenticate`, `proxy-authorization`, `proxy-connection`, `set-cookie`, `te`, `trailer`, `transfer-encoding` and `upgrade` are never written. The copy and the record are held for the lifetime from item 2, less the time the fetch took. A response that may not be shared is held as the marker `AssetEntry::NotShareable` for 60 seconds (`ASSET_NOT_SHAREABLE_LIFETIME`), so the requests that follow do not wait on a load that would end the same way. Each of those requests asks the origin itself.

**What is served.** `AssetCopy::into_response` builds a `200` response from the kept headers, then sets `Age` to the origin's age at the time of the fetch plus the seconds since, and sets `Content-Length`. The kept headers include `Vary`, so a cache further along separates readers as the origin asked. On both paths the copy takes the place of the origin's response, and everything after that point is unchanged. On the publisher path the copy is therefore processed as an origin response is, and the copy is the origin's bytes and not a rewritten body.

**One fetch for many requests.** `get_or_fetch` loads the address's record through `assets-vary`. The request that leads that load fetches the origin and writes its own copy under its own values before the record is published, so a waiting request with the same values finds the copy. A waiting request with other values loads its own copy, and the requests waiting on those values share that load. Both caches are built with `detachable: false`, so a load runs in the request that started it.

**Serving the request whose load fetched.** A response that cannot be shared is not stored for others, yet the request that fetched it must not fetch again. `HandBack` carries the response from the load to that request. `asset_from_cache` makes one `HandBack` for each lookup, `load_asset` puts a response that cannot be shared in the slot by its id, and the lookup ends by taking the slot's response when it finds `AssetEntry::NotShareable` or `Found::NoCopies`. A request that did not lead the load finds the slot empty and asks the origin. Two other cases use the same path. A body that outgrows a copy while it is read is handed back as what was read so far followed by the rest of the stream. A response whose `Vary` no longer matches its record is handled by `copy_loader`, which removes the record, writes the marker with no time to live, and serves the request what its own fetch returned, so the next request learns the new `Vary`.

**Configuration.** One optional table selects and sizes both caches.

```toml
[cache.assets]
module = "51degrees"
memory_seconds = 5
store_seconds = 3600
store_idle_seconds = 86400
```

| Field                | Default         | Range in the guide | Meaning                                                    |
| -------------------- | --------------- | ------------------ | ---------------------------------------------------------- |
| `module`             | none            |                    | The loading cache module that holds both caches            |
| `memory_seconds`     | `5`             | 0 to 31536000      | How long a copy in process memory is used. Zero keeps none |
| `store_seconds`      | `3600`, an hour | 1 to 31536000      | The longest a copy in the shared store is used             |
| `store_idle_seconds` | `86400`, a day  | 1 to 31536000      | How long a copy in the shared store lasts unused           |

The defaults are `ASSET_DEFAULT_LIFETIMES` in `publisher.rs`. The memory layer holds at most 256 records and 32 copies of at most 1 MiB each. Those sizes are constants in `asset_cache` and not settings. `LoadingCacheConfig` refuses a field it does not know.

`loading_caches_from` in `integrations/mod.rs` makes the selection. `CacheSettings::loading_caches` lists `robots-txt`, then `assets` and `assets-vary`, both with the settings of `[cache.assets]`, then the tables modules declare. A table that names a module takes that module, by short or full name. A table that names none, and an absent table, take the one loading cache module the build links, and hold nothing when the build links none or several. A table that names a module no linked crate offers is refused when the service starts, with the modules the build has. `declared_caches` declares `assets` and `assets-vary` as core's own, so a module that declares either name is refused, and the error that lists a build's caches now reads `robots-txt, assets, assets-vary` first.

The range check, `LoadingCacheConfig::check`, runs in `CacheSettings::prepare_runtime` for `[cache.robots-txt]` and for each table a module declares. The check does not run for `[cache.assets]`. The ranges in the table are those the guide gives, and nothing in this change enforces them for `[cache.assets]`. There is no `[cache.assets-vary]` field. A table of that name would be read as one a module declares, and its lifetimes would not be read.

**How each adapter takes it.** No adapter code changes. Each adapter calls `loading_caches_from` when it builds its application state and hands the result to each request's services, so the selection reaches `asset_from_cache` through `RuntimeServices::loading_cache_for`.

| Adapter    | Where the selection is made and handed on                                                   | Shared store the stock module uses         | How the origin body arrives           |
| ---------- | ------------------------------------------------------------------------------------------- | ------------------------------------------ | ------------------------------------- |
| Fastly     | `crates/trusted-server-adapter-fastly/src/app.rs`, handed on with `.loading_caches(...)`    | Fastly's cache, through `FastlyValueCache` | Streamed, and read up to 1 MiB        |
| Axum       | `crates/trusted-server-adapter-axum/src/app.rs`, handed on with `.with_loading_caches(...)` | The host's key-value store                 | Whole, as the adapter does not stream |
| Cloudflare | `crates/trusted-server-adapter-cloudflare/src/app.rs`, with `.with_loading_caches(...)`     | The host's key-value store                 | Whole, as the adapter does not stream |
| Spin       | `crates/trusted-server-adapter-spin/src/app.rs`, handed on with `.with_loading_caches(...)` | The host's key-value store                 | Whole, as the adapter does not stream |

The stock module, `crates/loading-cache/51degrees`, uses a host's value cache when the services carry one and the key-value store otherwise (`through_store` in `stores.rs`). Only the Fastly adapter registers a value cache. The module's memory layer lives as long as the application state, except on Cloudflare, where one cache lives as long as the isolate. Only the Fastly adapter overrides `supports_streaming_responses`, so the others hand `load_asset` a body that is already whole.

**Consent and privacy.** The asset caches read no consent signal and set none, and nothing in them identifies a reader.

- **What is stored** is the origin's response for a `200`, the record of header names, and the marker. No reader's request is stored. The values of the request headers that `Vary` names enter the key only as the 128-bit digest, and no URL, host or header value appears in a key.
- **What is never stored for another reader** is a response that sets a cookie, a response whose policy is `private`, `no-store` or `no-cache` or gives no lifetime, a response that varies on `*` or on `Cookie`, and any response to a request that carries `Authorization`, a conditional header, a range, or a `Cache-Control` that asks for the origin, and on the publisher path any response to a request that carries state a module left or a personalized marker. Those requests also never read a copy.
- **An asset route sends no cookie.** `ASSET_PROXY_FORWARD_HEADERS` lists the eleven request headers an asset origin receives, and `Cookie` and `Authorization` are not among them. An answer from a route's origin therefore cannot depend on a cookie that origin never received. Any reader who asks the route for a path is already given the origin's answer for that path, so a shared copy gives no reader a file the route would not.
- **The publisher path sends the reader's headers on.** `rewrite_origin_request` removes `fastly-ssl`, restricts `Accept-Encoding` and sets `Host`, and `handle_publisher_request` itself does not remove `Cookie`. The key holds only the headers that `Vary` names. An origin that answers a cookie or a header differently without naming it in `Vary`, and gives that answer a lifetime another reader may share, has one reader's answer kept and served to others until the lifetime ends. The cache cannot see such an input, and this change has no setting to declare one (see section 3).
- **Logs.** The lines `asset_cache bypass`, `asset_cache stored` and `asset_cache invalid entry` carry a fixed reason and a byte count, and no URL, key or header. The line `asset_cache load failed` carries the error text of the failed fetch, which includes the platform's error report as the platform formats it, and this change does not filter that text.

**Failure handling.** Each failure ends with the caller asking the origin, as before this change, or with the reader served the response the request's own fetch made.

- **The origin cannot be reached during a load.** `load_asset` returns a `LoadError`. Every request waiting on that load receives it, nothing is stored, `asset_from_cache` logs `asset_cache load failed` at warn level, and each of those requests then asks the origin itself. A failing origin is therefore asked once more by each request that waited.
- **A stored entry cannot be read.** `AssetEntry::decode` refuses bytes that are short, unreadable or written in another format. A record this build did not write is refused in the same way. In both cases the lookup logs at warn level, removes the entry, and the request asks the origin. A format version is in the key, in the entry and in the record, so a build that changes the format does not read the old entries.
- **The shared store fails.** The key-value layer treats a failed read as a miss, so the value is loaded, and logs a failed read, write or removal at debug level. The value cache that Fastly provides reports no errors to the module.
- **An error status.** Only a `200` is stored, so an origin outage or a `404` is not held.
- **A body that cannot be read whole.** The reader is served the whole body from the one fetch, as described above.

**Rollout for existing deployments.**

1. `[cache.assets]` is optional and is left out when written back (`skip_serializing_if = "LoadingCacheConfig::is_default"`), so settings written before this change read as they did.
2. A build that links no loading cache module holds nothing, and a build that links several and names none holds nothing. Both behave as before.
3. A stock build links exactly one loading cache module, `loading-cache.51degrees`, so it starts holding shareable assets with no change to its settings. A change at the origin then shows after the lifetime the origin gave the file, and never later than `store_seconds`, which defaults to one hour.
4. No setting turns the cache off while exactly one module is linked. `loading_caches_from` has no spelling for "none" and refuses a module name that is not linked. A deployment that does not want its assets held builds its module list without the loading cache module.
5. Every change to the settings and every new build changes the fingerprint, so a publisher path copy made earlier is not found again and ends by its lifetime. Nothing purges it. A route's key has no fingerprint, so a route's copy outlives a settings change, including a change to the route's credentials.
6. The deploy check lists the new cache names. The error that names a misspelt table now includes `assets` and `assets-vary`.

## 3. What is not in this change

- **The publisher's declared rule.** An origin can vary on an input it does not name in `Vary`, and only the publisher knows which. `Lookup::declared` in `source_cache.rs` takes a token that enters the copy's key, and one `source_cache` test covers it. Both callers pass `None`, and no setting supplies a value. The rule, as `[publisher.origin_personalization]`, is a later change with its own tests, and until then the origin's own `Vary` alone separates copies.
- **A rewritten body kept once.** The fetch phase runs on each reader's copy. `AssetCopy::decoded` and `SourceVary::without` are for a later change that keeps the rewritten body. Production code leaves `decoded` false and calls `without` nowhere.
- **Pages.** A page stays on the template cache.
- **The `/proxy` first-party route.** The route is unchanged and does not use the asset caches.
- **Answers made by the platform's image optimizer.** The platform holds those answers, so core does not keep them.
- **Requests a copy cannot answer.** `HEAD`, other methods, `Authorization`, ranges and conditional requests always go to the origin.
- **Files over 1 MiB.** A file over 1 MiB is served as it streams from the origin.
- **A way to purge one asset.** This change adds no purge for the asset caches.
- **A setting that turns the asset caches off**, and settings for the memory layer's sizes.
- **Enforcing the ranges for `[cache.assets]`**, as section 2 records.

## 4. Acceptance

The tests below exist and use `MemoryLoadingCache` from `crates/trusted-server-core/src/platform/test_support.rs` as the loading cache and `StubHttpClient` as the origin. The pull request records the fork's CI at `f6e598fda` as 19 of 19 checks green.

| Where                                                      | Tests                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            |
| ---------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `asset_cache.rs`                                           | Every input changes the key and none appears in it. An entry reads back as written. A short, foreign or other-format entry is not read. A copy keeps no connection, cookie or length header. A served copy counts its age from when it was made                                                                                                                                                                                                                                                                                                  |
| `source_cache.rs`                                          | Record and copy keys follow the publisher. `Vary` names are lowercase, sorted and named once. `*`, `Cookie` and an unreadable `Vary` allow no copies. A record reads back, and a foreign one does not. A variant tells absent, empty and reordered values apart. The first response records its `Vary` and each value gets its own copy. A response that allows no copies has every request fetch its own. A source that changes its `Vary` has its record dropped. A declared token separates copies                                            |
| `publisher.rs`, module `asset_cache_tests`                 | An image is kept as sent and one fetch serves three requests. A stylesheet is served from one fetch with its age. `Vary: *`, `Vary: Cookie`, `private`, `no-store` and `public, no-store` are never kept. `s-maxage` then `max-age` set the lifetime. A header named in `Vary` separates copies. A body over 1 MiB is served whole from one fetch, with the stub streaming and not. A response whose `Vary` changed is served from its one fetch. A request carrying `Authorization` is fetched for itself. A page navigation never reads a copy |
| `proxy.rs`                                                 | A route's answer is held and serves the next request, with its type, an `Age`, its bytes and the origin's lifetime. An answer with `Set-Cookie` is fetched each time. A range request is fetched. `no-cache`, `no-store` and `min-fresh=10` are fetched. Nothing is held while no cache keeps values                                                                                                                                                                                                                                             |
| `settings.rs`, `integrations/mod.rs`, Axum `seam_probe.rs` | The list of core's caches now holds `assets` and `assets-vary`, in the settings listing, in the refusal that names a misspelt table, and in the Axum deploy check                                                                                                                                                                                                                                                                                                                                                                                |

Joins with no test:

1. **Concurrent requests.** Every test of the asset paths sends requests one after another. The loading cache module has tests for many callers loading a missing key once (`many_callers_on_many_threads_load_a_missing_key_once_in_the_caller`) and for a failed load reaching every waiter (`a_failed_load_reaches_every_waiter_and_is_not_kept`), and those load a generic value and not an asset.
2. **A real shared store.** No test runs the asset caches against Fastly's cache, a key-value store, or two instances. The Axum, Cloudflare and Spin adapters have no test of asset holding.
3. **Selecting `[cache.assets]`.** No test names the table, or checks that both `assets` and `assets-vary` take the one linked module when the table is absent.
4. **The range check.** The code does not run the range check for `[cache.assets]`, and no test covers an out-of-range value.
5. **The publisher path with no loading cache.** No test in `asset_cache_tests` names that case.
6. **Publisher path cases.** No test covers an HTML response to a non-document request, a `Content-Length` over the limit, a response with `Set-Cookie`, a lifetime from `Expires` or `Surrogate-Control`, a request with a conditional header, a range or `Cache-Control`, request state left by a module, a personalized response, a request with no host, or a header that is not visible text.
7. **Asset route cases.** No test covers an image optimizer route, a route with `auth`, a request with `Authorization`, a method other than `GET`, or a request with no host.
8. **Streaming with a stored copy.** The route tests turn streaming off (`held_asset_stub`), and the publisher tests leave it off except in the oversized body test, which stores nothing. A stored copy served on an adapter that streams has no test.
9. **Two publishers in one cache.** The key test in `source_cache.rs` and the module's `two_publishers_never_share_an_entry` cover the prefix, and no test serves two publishers through either asset path.
10. **Reordered page checks.** `origin_response_may_be_kept` moves the status check ahead of the cookie and edge header checks in `template_cache_ttl`. A page response with a non-`200` status and a fault found by a later check is now reported as a non-`200` status, and no test was added for the changed order.

## Review log

No review rounds yet.

## Revision record

| Date       | Change                               |
| ---------- | ------------------------------------ |
| 2026-10-15 | First draft, written after the code. |
