# Design Spec: Core keeps a publisher's cookies under that publisher's path when one host serves several

**Status:** Proposed, 2026-10-15
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** `2026-10-13-request-addressing-design.md`, `2026-10-12-prefix-module-design.md`
**Issues:** IABTechLab/trusted-server#1328
**Last updated:** 2026-10-15

> **Why this spec exists.** On a host that serves several publishers by path, every cookie core writes has `Path=/`, so the browser sends each publisher's cookies to every other publisher's pages and origin.

## 1. Problem

Request addressing lets one host serve several publishers as `https://front.example/<publisher>/<path>`. Each publisher's prefix module gives the cookies core names a name of their own. That keeps the Edge Cookie family, the eids cookie and the pull-sync marker apart by name. It does not keep them apart in the browser, and it does nothing for cookies whose names are fixed.

1. **Core writes every cookie under `/`.** The Edge Cookie, its resolved marker, the pull-sync marker and the tester cookie all carry `Path=/`. A browser sends a cookie on every request whose path is under the cookie's `Path`, so it sends each publisher's cookies on every other publisher's requests.
2. **Some names are fixed.** The consent cookies (`euconsent-v2`, `__gpp`, `__gpp_sid`, `us_privacy`) and `sharedId` are named by the tools that write them, not by core. Consent given on one publisher's pages is read on another's.
3. **The origin receives them all.** `rewrite_origin_request` in `crates/trusted-server-core/src/publisher.rs` forwards the `Cookie` header to the chosen origin as the browser sent it. Each publisher's origin receives the cookies the browser holds for the others.
4. **The client script posts to the root.** The script calls root-relative endpoints such as `/auction`, which carry no publisher part, so on a host addressed by path those requests are addressed by host.

## 2. Design

Browsers already keep cookies apart by path. RFC 6265 section 5.1.4 sends a cookie only on a request whose path is the cookie's `Path` or below it. A cookie written with `Path=/publisher-a.example` is sent under `/publisher-a.example/` and never under `/publisher-b.example/`. Core uses that rule and adds no setting.

**Every cookie core writes carries the removed prefix as its path.** On a request that carries a `RemovedPathPrefix`, a cookie core writes has `Path=<removed prefix>` in place of `Path=/`. A request with no prefix writes `Path=/`, exactly as today.

Every writer already takes the publisher's `PublicPrefix`, because the prefix names the cookie. The prefix now also carries the path its cookies live under, so a writer reads the name and the path from one value and cannot write one without the other.

```rust
impl PublicPrefix {
    /// The same prefix, its cookies written under `path`.
    pub fn under_path(self, path: Option<&str>) -> Self;

    /// The prefix for `request`, its cookies under the prefix request
    /// addressing removed, or under `/` when none was removed.
    pub fn for_request<B>(&self, request: &http::Request<B>) -> Self;

    /// The `Path` attribute of every cookie written under this prefix.
    pub fn cookie_path(&self) -> &str;
}
```

The writers, each of which now writes `Path={public.cookie_path()}`, are these.

| Cookie                                  | Written by                                                          |
| --------------------------------------- | ------------------------------------------------------------------- |
| Edge Cookie and its expiry              | `format_set_cookie` in `ec/cookies.rs`                              |
| Resolved marker and its expiry          | `set_resolved_marker_cookie`, `expire_ec_resolved_marker`           |
| Pull-sync marker and its expiry         | `format_marker_cookie` in `ec/pull_sync_marker.rs`                  |
| Tester cookie and its clearing          | `format_tester_cookie`, `format_clear_tester_cookie`                |

The Edge Cookie context builds its prefix with `for_request` when it reads the request, so every cookie the finalize step writes takes the request's path. The resolve endpoint writes its cookies under that same prefix, read from the context and no longer from the runtime services. The tester routes write under the prefix their caller passes them.

**A removed prefix must be a cookie path.** A prefix is written into a `Set-Cookie` header, so `RequestAddress::new` now refuses a removed prefix that does not start with `/` or that holds anything but visible ASCII other than `;`. An empty prefix removes nothing and is still accepted, and its cookies are written under `/`. Such a request is refused as an invalid address, which the adapters answer `421 Misdirected Request`, before any settings are read. `under_path` applies the same check and keeps `/` for a prefix that fails it, with a warning, so a prefix built some other way cannot add an attribute to a cookie.

**Reading the cookie a publisher's path holds.** A browser that holds two cookies of one name, one under `/` and one under `/publisher-a.example`, sends both on a request under `/publisher-a.example/`. RFC 6265 section 5.4 step 2 lists the one with the longer path first.

- The consent cookies are read through `RequestInfo::cookie` in `crates/trusted-server-core/src/evidence.rs`, which returns the first pair of that name in the `Cookie` header. The permission signal modules read `euconsent-v2`, `__gpp`, `__gpp_sid` and `us_privacy` through it. A publisher's consent written under its path therefore already wins over a root one, on every request, and this change keeps that.
- The Edge Cookie and the pull-sync marker are read through `handle_request_cookies` in `crates/trusted-server-core/src/cookies.rs`, whose jar keeps the last pair of a name. The `cookie` crate's `CookieJar::add_original` replaces an entry of the same name (`cookie-0.18.1`, `src/jar.rs` line 153, keyed by name in `src/delta.rs`). On an addressed request the jar now keeps the first pair of each name, so a cookie under the publisher's path wins over a root one left from before. A request with no prefix keeps the last, as today.
- `sharedId` is read in core only to ask whether it is present, in `publisher.rs`, where order does not matter. Its value is read by the Fastly adapter's `extract_cookie_value`, which returns the first pair, as `RequestInfo::cookie` does.

Consent cookies are written by the publisher's consent tool in the browser, not by core. A deployment that serves several publishers from one host configures each publisher's consent tool to write its cookies under that publisher's path. Core reads what the browser sends and keeps no list of which tool writes which cookie.

**The `Cookie` header sent to the origin.** With every core cookie under its publisher's path, and each consent tool configured the same way, the browser never sends a cookie held under another publisher's path on this publisher's request. Such a cookie cannot reach this publisher's origin, so `rewrite_origin_request` still forwards the header as the browser sent it. Cookies written under `/` remain shared, and core cannot tell which publisher wrote one, so it forwards them to every origin on the host. This is the remaining limit, stated in section 3.

**Why core writes the prefix into these cookies.** The request addressing spec leaves writing the prefix into a response to a deployment's middleware. Core is the only writer of its own cookies, and a middleware that rewrote their `Path` would have to recognise core's cookies by name in every response. The prefix is written only into the `Path` of the cookies core itself writes. Links in pages and cookies an origin writes remain a middleware's job.

**Each adapter.** The extension is set in one place for all four adapters, so none changes.

| Adapter    | Where the request gets its prefix                                  | Change in the adapter                     |
| ---------- | ------------------------------------------------------------------ | ----------------------------------------- |
| Fastly     | The entry point inserts `RemovedPathPrefix` after asking the hook  | None, see section 3 for the tester routes |
| Axum       | `addressed_router` in core                                          | None                                      |
| Cloudflare | `addressed_router` in core                                          | None                                      |
| Spin       | `addressed_router` in core                                          | None                                      |

**Configuration.** None. The path comes from the request, and a deployment that addresses no request by path writes `Path=/` as today.

**Consent and privacy.** Keeping each publisher's cookies under its path stops one publisher's Edge Cookie, markers and consent reaching another publisher's pages and origin. A withdrawal expires the Edge Cookie under the same path it was written under, so the expiry reaches the cookie the browser holds for that publisher. A root Edge Cookie left from before this change is not expired by a request under a publisher's path, and it is no longer read there once a path-scoped one exists.

**Failure handling.** A removed prefix that cannot be a cookie path is refused when the address is made, as above. A `Set-Cookie` value that fails to render is skipped with a warning, as today.

**Rollout.** A request with no prefix writes `Path=/` and reads its jar as today, so a deployment that supplies no addressing hook, or one whose hook answers "by host", is unchanged. On a deployment already addressing by path, the first response after the change writes each core cookie under the publisher's path. The root cookie written before remains in the browser until it expires, and on that publisher's requests the path-scoped cookie wins because the jar keeps the first.

**Tests.** In `crates/trusted-server-core`:

- `prefix.rs`: a prefix with no path or an empty one gives `/`, `for_request` takes the removed prefix, and a prefix holding `;`, a space or non-ASCII, or not starting with `/`, keeps `/`.
- `ec/cookies.rs`: the Edge Cookie, its expiry and the resolved marker carry `Path=/publisher-a.example` under that prefix, and `Path=/` without it.
- `ec/pull_sync_marker.rs` and `tester_cookie.rs`: the marker and the tester cookie carry the prefix path.
- `cookies.rs`: on an addressed request the first of two pairs of one name is read, and with no prefix the last is read, as today.
- `evidence.rs`: `RequestInfo::cookie` returns the first of two consent cookies of one name.
- `addressing.rs`: `RequestAddress::new` accepts an empty prefix or a cookie path, and refuses a removed prefix holding `;`, a space or non-ASCII, or not starting with `/`.

## 3. What is not in this change

- **Cookies under `/`.** A cookie written under `/`, by an origin, by a tool not configured for the publisher's path, or by core before this change, is sent under every publisher's path and forwarded to every origin on the host. Core cannot tell which publisher wrote it. A deployment that needs an origin's cookies kept apart rewrites their `Path` with a middleware of its own, as it puts the prefix into links.
- **The client script's endpoints and cookies.** The script calls root-relative endpoints, `/auction` in `crates/trusted-server-js/lib/src/core/request.ts`, `/_ts/api/v1/ec/resolve` in `integrations/ec_client_fixed`, `/_ts/page-bids` in `integrations/gpt`, and the auction endpoint default in `integrations/prebid`. Several integrations also write cookies with `Path=/` from the page, in `integrations/prebid`, `integrations/osano` and `integrations/sourcepoint`. On a host addressed by path these requests are addressed by host and those cookies are shared. The fix is for the server to tell the script the removed prefix in the page, as it tells it the public prefix with `data-ts-public-prefix`, and for the script to put the prefix before each endpoint and use it as each cookie's path. That change touches the script's core and four integrations and is left for a change of its own.
- **The tester cookie on Fastly.** The Fastly adapter passes the runtime services' prefix to the tester routes, so on an addressed request the tester cookie is still written under `/`. Passing `services.public_prefix().for_request(&req)` there is a one-line change in the adapter, left out because this change builds and tests core alone.
- **Expiring root cookies left from before.** Core expires a cookie under the path of the request it answers, so a root cookie written before this change stays until its `Max-Age` ends.
- **Partitioning other first-party storage.** Local storage and other storage the page script keeps belong to the host, not the path.

## 4. Acceptance

The tests listed under "Tests" in section 2 pass. `cargo test -p trusted-server-core`, `cargo clippy -p trusted-server-core --all-targets -- -D warnings` and `cargo fmt --all -- --check` pass. With no `RemovedPathPrefix` on a request, every `Set-Cookie` core writes is byte for byte as before, which the existing cookie tests check unchanged.

## Review log

No review rounds yet.

## Revision record

| Date       | Change       |
| ---------- | ------------ |
| 2026-10-15 | First draft. |
