# Design Spec: Core keeps a publisher's cookies under that publisher's path when one host serves several

**Status:** Proposed, 2026-10-15
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** `2026-10-13-request-addressing-design.md`, `2026-10-12-prefix-module-design.md`
**Issues:** IABTechLab/trusted-server#1328
**Last updated:** 2026-10-15

> **Why this spec exists.** On a host that serves several publishers by path, every cookie core writes has `Path=/`, so the browser sends each publisher's cookies to every other publisher's pages and origin. This change writes the cookies core owns under each publisher's path. Cookies written under `/` by an origin, a module or the page script stay shared, and section 3 lists them.

## 1. Problem

Request addressing lets one host serve several publishers as `https://front.example/<publisher>/<path>`. Each publisher's prefix module gives the cookies core names a name of their own. That keeps the Edge Cookie family, the eids cookie and the pull-sync marker apart by name. It does not keep them apart in the browser, and it does nothing for cookies whose names are fixed.

1. **Core writes every cookie under `/`.** The Edge Cookie, its resolved marker, the pull-sync marker and the tester cookie all carry `Path=/`. A browser sends a cookie on every request whose path is under the cookie's `Path`, so it sends each publisher's cookies on every other publisher's requests.
2. **Some names are fixed.** The consent cookies (`euconsent-v2`, `__gpp`, `__gpp_sid` and `us_privacy`), the preference cookie `__mtm_pref` and `sharedId` are named by the tools that write them, not by core. Consent given on one publisher's pages is read on another's.
3. **The origin receives them all.** `rewrite_origin_request` in `crates/trusted-server-core/src/publisher.rs` forwards the `Cookie` header to the chosen origin as the browser sent it. Each publisher's origin receives the cookies the browser holds for the others. The one exception is an origin whose requests are signed, because `sign_publisher_origin_request` removes the `Cookie` header from every request it signs.
4. **The client script posts to the root.** The script calls root-relative endpoints such as `/auction`, which carry no publisher part, so on a host addressed by path those requests are addressed by host.

## 2. Design

Browsers already keep cookies apart by path. RFC 6265 sections 5.1.4 and 5.4 send a cookie only on a request whose path matches the cookie's `Path`, which means the same path or one below it. A cookie written with `Path=/publisher-a.example` is sent under `/publisher-a.example/` and never under `/publisher-b.example/`. Core uses that rule and adds no setting.

**Every cookie core writes carries the removed prefix as its path.** An addressed request is one the request addressing hook answered with an address, so it carries a `RemovedPathPrefix`, even an empty one. A request addressed by host carries none. On an addressed request, a cookie core writes has `Path=<removed prefix>` in place of `Path=/`. A request with no prefix, or an empty one, writes `Path=/`, exactly as today.

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

| Cookie                          | Written by                                                |
| ------------------------------- | --------------------------------------------------------- |
| Edge Cookie and its expiry      | `format_set_cookie` in `ec/cookies.rs`                    |
| Resolved marker and its expiry  | `set_resolved_marker_cookie`, `expire_ec_resolved_marker` |
| Pull-sync marker and its expiry | `format_marker_cookie` in `ec/pull_sync_marker.rs`        |
| Tester cookie and its clearing  | `format_tester_cookie`, `format_clear_tester_cookie`      |

`EcContext` builds its prefix with `for_request` when it reads the request, so every cookie the finalize step writes takes the request's path. The resolve endpoint writes its cookies under that same prefix, read from the context and no longer from the runtime services. The tester routes write under the prefix their caller passes them, and only the Fastly adapter serves those routes.

**A removed prefix must be a cookie path.** A prefix is written into a `Set-Cookie` header, so `addressing::is_cookie_path` accepts only a prefix that starts with `/` and holds nothing but visible ASCII other than `;`. The check sets no limit on length. `RequestAddress::new` returns `AddressingError::InvalidPath` for a removed prefix that fails the check, and still accepts an empty prefix, which removes nothing and writes its cookies under `/`. `under_path` applies the same check and keeps `/` for a prefix that fails it, with a warning, so a prefix built some other way cannot add an attribute to a cookie.

**What a refused prefix becomes.** `RequestAddress::new` hands its error to the hook, and the hook decides what the request becomes. `Addressing` has only `ByHost` and `Addressed`, so a hook cannot refuse a request, and no adapter calls `new`. The hook in the rustdoc of `addressing.rs`, and the one in the request addressing spec, map the error to `ByHost`. A request such as `/a;b/news` is then addressed by its host, keeps its whole path and has no removed prefix. On a Fastly service whose key selector carries `{host}`, that request is served from the front host's own blob when the front host has one. Before this change the same request carried the selector value `a;b`, which `normalized_host` drops because `a;b` is not a host name, so no blob was found and the Fastly adapter answered `421 Misdirected Request`. That 421 is the adapter's answer to a missing blob, and no adapter answers 421 because a prefix is not a cookie path. `AddressingError::InvalidPath` still describes only the continuing path.

**Reading the cookie a publisher's path holds.** A browser that holds two cookies of one name, one under `/` (a root cookie) and one under `/publisher-a.example`, sends both on a request under `/publisher-a.example/`. RFC 6265 section 5.4 step 2 says a browser SHOULD list the cookie with the longer path first, and adds a note that not all browsers sort the list. The reads below rely on that order. A browser that does not sort may list the older root cookie first, and the request then reads the root cookie in place of the one under the publisher's path.

- The consent cookies are read through `RequestInfo::cookie` in `crates/trusted-server-core/src/evidence.rs`, which returns the first pair of that name in the `Cookie` header. The permission signal modules read `euconsent-v2`, `__gpp`, `__gpp_sid`, `us_privacy` and `__mtm_pref` through it. A consent cookie written under a publisher's path therefore already wins over a root one, on an addressed request or any other, and this change leaves that read as it is.
- The Edge Cookie and the pull-sync marker are read through `handle_request_cookies` in `crates/trusted-server-core/src/cookies.rs`, whose jar keeps the last pair of a name. The `cookie` crate's `CookieJar::add_original` replaces an entry of the same name (`cookie-0.18.1`, `src/jar.rs` line 153, keyed by name in `src/delta.rs`). On an addressed request the jar now keeps the first pair of each name, so a cookie under the publisher's path wins over a root one left from before. A request with no prefix keeps the last, as today. A request whose removed prefix is empty, or is `/`, is still an addressed request, so its jar keeps the first pair while its cookies are written under `/`.
- `sharedId` is written by the page's SharedID module. Core asks only whether the cookie is present, in `publisher.rs`, where order does not matter. The Fastly adapter reads its value with `extract_cookie_value`, which returns the first pair as `RequestInfo::cookie` does, and passes the value to `ec_finalize_response`, which uses it to collect partner identifiers.

Consent cookies are written by a consent tool in the browser, not by core, and each keeps the `Path` its tool gave it. A tool that lets the deployment choose the path can be set to a publisher's path, and core has no way to check that the tool was set that way. The client script's Osano mirror writes `us_privacy`, `__gpp`, `__gpp_sid` and `euconsent-v2`, and its Sourcepoint mirror writes `__gpp` and `__gpp_sid`, each with `Path=/` fixed in `integrations/osano/index.ts` and `integrations/sourcepoint/index.ts`, so no setting can move those cookies. Core reads what the browser sends and keeps no list of which tool writes which cookie.

**The `Cookie` header sent to the origin.** A browser sends a cookie only on requests under its path, so a cookie core wrote under one publisher's path is not in the header of a request under another's, and `rewrite_origin_request` still forwards the header as the browser sent it. An origin whose requests are signed receives no `Cookie` header at all (`READER_HEADERS_NOT_SIGNED` in `publisher.rs`). Cookies under `/` still reach every origin that receives the header. They are the cookies core wrote before this change, the consent cookies the Osano and Sourcepoint mirrors write, and any cookie a page, a module or an origin writes under `/`. Core cannot tell which publisher wrote one, so it forwards them all. This is the remaining limit, stated in section 3.

**Why core writes the prefix into these cookies.** The request addressing spec leaves writing the prefix into a response to a deployment's middleware. Core is the only writer of its own cookies, and a middleware that rewrote their `Path` would have to recognise core's cookies by name in every response. The prefix is written only into the `Path` of the cookies core itself writes. Links in pages and cookies an origin or a module writes remain a middleware's job.

**Each adapter.** The request gets its `RemovedPathPrefix` in two places, being `addressed_router` in core, which the Axum, Cloudflare and Spin adapters wrap their routers with, and the Fastly entry point, which inserts the extension when it converts the request. The writers in core read that extension, so no adapter needs a change for those writers. The tester routes differ, because the Fastly adapter builds their prefix and so calls `for_request` itself. Axum, Cloudflare and Spin have no tester routes. Of the four adapters, only Fastly calls `ec_finalize_response` or routes the resolve endpoint, so Axum, Cloudflare and Spin reach none of the four writers today.

| Adapter    | Where the request gets its prefix                                 | Change in the adapter                                                 |
| ---------- | ----------------------------------------------------------------- | --------------------------------------------------------------------- |
| Fastly     | The entry point inserts `RemovedPathPrefix` after asking the hook | `run_named_route` builds the tester routes' prefix with `for_request` |
| Axum       | `addressed_router` in core                                        | None, and no tester routes                                            |
| Cloudflare | `addressed_router` in core                                        | None, and no tester routes                                            |
| Spin       | `addressed_router` in core                                        | None, and no tester routes                                            |

**Configuration.** None. The path comes from the request, and a deployment that addresses no request by path writes `Path=/` as today. No setting is added or changed.

**Consent and privacy.** Writing core's own cookies under a publisher's path stops the browser sending a publisher's Edge Cookie, resolved marker, pull-sync marker and tester cookie on requests under another publisher's path, so another publisher's origin does not receive the ones written after this change. The change does not isolate consent, because core writes no consent cookie and each consent cookie keeps the `Path` its tool gave it, as above. `Path` is also no security boundary, because every publisher on the host is one origin to the browser. RFC 6265 section 8.5 says a resource from one path might be able to access cookies stored for another, and section 8.6 says the `Path` attribute gives no integrity protection, because a response can set a cookie with any `Path`. A withdrawal expires the Edge Cookie, the resolved marker and the pull-sync marker under the request's removed prefix, the path they were written under, so the expiry reaches the cookies the browser holds for that publisher. The expiry does not reach a root Edge Cookie written before this change. That cookie stays in the browser after a withdrawal and is still sent to every origin on the host that receives the `Cookie` header, while the identity graph still records the withdrawal.

**Failure handling.** A removed prefix that is not a cookie path is refused by `RequestAddress::new` and kept off a cookie by `under_path`, as above, and the hook decides what the refused request becomes. A `Set-Cookie` value that fails to render is skipped with a warning, as today, except on the tester routes, which return `TrustedServerError::InvalidHeaderValue`.

**Rollout.** A request with no prefix writes `Path=/` and reads its jar as today, so a deployment that supplies no addressing hook, or one whose hook answers "by host", is unchanged. On a deployment already addressing by path, core writes a cookie under the publisher's path only in a response where it writes that cookie at all. For the Edge Cookie that is when core creates an identifier for a visitor who has none, or replaces an orphaned one, which is an identifier whose row the identity graph has lost. A returning visitor's response writes no Edge Cookie. The root Edge Cookie and resolved marker written before this change therefore stay in that visitor's browser for up to a year (`COOKIE_MAX_AGE` in `ec/cookies.rs`), are still sent to every publisher's pages and origin on the host, and are still read, so the identity continues. Nothing moves that cookie under the publisher's path. A browser can hold a root cookie and a cookie under the publisher's path of one name, for example after core writes a new Edge Cookie for a visitor who still has the root one. The jar's first-pair rule then picks the cookie under the publisher's path, provided the browser lists the longer path first.

**Tests.** In `crates/trusted-server-core`:

- In `prefix.rs`, a prefix with no path or an empty one gives `/`, `for_request` takes the removed prefix, and a prefix holding `;`, a space or non-ASCII, or not starting with `/`, keeps `/`.
- In `ec/cookies.rs`, the Edge Cookie, its expiry and the resolved marker carry `Path=/publisher-a.example` under that prefix, and `Path=/` without it.
- In `ec/pull_sync_marker.rs` and `tester_cookie.rs`, the marker and the tester cookie carry the prefix path.
- In `cookies.rs`, on an addressed request the first of two pairs of one name is read, and with no prefix the last is read, as today.
- In `evidence.rs`, `RequestInfo::cookie` returns the first of two consent cookies of one name.
- In `addressing.rs`, `RequestAddress::new` accepts an empty prefix or a cookie path, and refuses a removed prefix holding `;`, a space or non-ASCII, or not starting with `/`.

In `crates/trusted-server-adapter-fastly`, `app.rs` holds `dispatch_set_tester_writes_under_the_removed_prefix`, which sends `/_ts/set-tester` through the router with a removed prefix and checks the cookie's path. The clear route has no addressed test of its own.

Two pieces of production code build the prefix the writers use, being `for_request` in `EcContext::read_from_request` (`ec/mod.rs`) and the use of the context's prefix in `handle_ec_resolve` (`ec/resolve.rs`). No listed test fails if either is removed, because no test sends a request with a removed prefix through either. No test covers an empty removed prefix reading the jar.

**Open questions.** Each of these waits for a decision and for a code change, and this revision changes no code.

1. Whether `RequestAddress::new` keeps refusing a removed prefix that is not a cookie path, or leaves the check to `under_path`, which alone stops a cookie attribute being injected. The refusal is what changes the answer for `/a;b/news`. If `new` keeps the refusal, `AddressingError::InvalidPath` and its message have to name the removed prefix as well as the continuing path.
2. Whether `handle_request_cookies` treats an empty or `/` removed prefix as no prefix, so that a request whose cookies are written under `/` also reads the last pair of a name, as a request with no prefix does. A test would check it.
3. Whether to add a test that sends a request with a removed prefix through `EcContext::read_from_request` and `ec_finalize_response`, and one that does so through `handle_ec_resolve`, so that removing either piece of code that builds the writers' prefix fails a test.
4. Whether a withdrawal also expires a cookie of the same name under `/`, beside the expiry under the publisher's path, so that the withdrawal reaches an Edge Cookie written before this change. A cookie under `/` is one cookie for every publisher that uses its name, so expiring it affects all of them.
5. Whether to correct, with the code, the comments and pages this change makes untrue. The module comment of `addressing.rs`, the request addressing spec (held with every other design document of the stack in IABTechLab/trusted-server#1342) and the section on addressing in `docs/guide/configuration.md` say that core never writes the removed prefix into a response and that publishers on one host share cookies. The description of the Edge Cookie in `docs/guide/edge-cookies.md` says `Path=/`. The doc of `is_cookie_path` has absorbed the doc of `is_continuing_path`, and the docs of `PublicPrefix` and `EcContext::public_prefix` describe names and not the path.

## 3. What is not in this change

- **Cookies under `/`.** A cookie written under `/`, by an origin, by a module, by a consent tool not set to the publisher's path, or by core before this change, is sent under every publisher's path and forwarded to every origin on the host that is not signed. Core cannot tell which publisher wrote it. A deployment that needs an origin's cookies kept apart rewrites their `Path` with a middleware of its own, as it puts the prefix into links.
- **Cookies a module or an integration writes.** A module's own `Set-Cookie` headers pass through core unchanged unless they name a cookie under the public prefix (`reserved_response_effect` in `ec/module.rs`), so their `Path` is the module's choice. The GPT diagnostics cookie `__Host-ts-console`, which `crates/ad-tag/google/src/diagnostics.rs` writes, has to stay `Path=/`, because browsers accept a cookie with the `__Host-` prefix only with `Path=/` (`draft-ietf-httpbis-rfc6265bis`, section 4.1.3.2).
- **Nested prefixes.** If one publisher's prefix lies inside another's, such as `/news` and `/news/sport`, the cookies under the outer prefix are also sent on requests under the inner one.
- **The client script's endpoints and cookies.** The script calls root-relative endpoints, `/auction` in `crates/trusted-server-js/lib/src/core/request.ts`, `/_ts/api/v1/ec/resolve` in `integrations/ec_client_fixed`, `/_ts/page-bids` in `integrations/gpt`, and the auction endpoint default in `integrations/prebid`. Several integrations also write cookies with `Path=/` from the page, being the eids cookie in `integrations/prebid` and the consent cookies in `integrations/osano` and `integrations/sourcepoint`. On a host addressed by path these requests are addressed by host, so the browser no longer sends them the cookies core wrote under the publisher's path, and the cookies core writes in answer to them, such as the resolve endpoint's, take `Path=/`. The cookies the page writes stay shared. The fix is for the server to tell the script the removed prefix in the page, as the server tells the script the public prefix with `data-ts-public-prefix`, and for the script to put the prefix before each endpoint and use it as each cookie's path. That change touches the script's core and five integrations, being `ec_client_fixed`, `gpt`, `osano`, `prebid` and `sourcepoint`, and is left for a change of its own.
- **Expiring root cookies left from before.** Core expires a cookie under the path of the request it answers, so a root Edge Cookie or resolved marker written before this change stays until its `Max-Age` ends, and a withdrawal does not remove it.
- **Partitioning other first-party storage.** Local storage and other storage the page script keeps belong to the host, not the path.

## 4. Acceptance

The tests listed under "Tests" in section 2 pass, including `dispatch_set_tester_writes_under_the_removed_prefix` in `crates/trusted-server-adapter-fastly/src/app.rs`, which `cargo test-fastly` runs. `cargo test -p trusted-server-core`, `cargo test-fastly`, `cargo clippy -p trusted-server-core --all-targets -- -D warnings`, `cargo clippy-fastly` and `cargo fmt --all -- --check` pass. With no `RemovedPathPrefix` on a request, every `Set-Cookie` core writes is as before. The existing tests check the unchanged output byte for byte for the Edge Cookie and its expiry, the pull-sync marker and its expiry, and the tester cookie and its clearing, and check the resolved marker and its expiry for their `Path` only. The tests that Open questions names join this acceptance when they are written.

## Review log

1. **Round 1, 2026-10-15.** A review in a fresh session, against `split/50-cookies-by-path` at `185557ad2`. Issues found, five of them blocking. The five were the 421 answer for a refused removed prefix, which no adapter gives, the claim that the first response moves visitors' cookies under the publisher's path, an empty removed prefix that writes `Path=/` yet reads the first of two cookies of one name, text the Fastly tester change left stale, and tests that never reach the code that builds the prefix in production. This revision states what the code does today in each case, adds the consent tools that write `Path=/`, the signed origins, the module cookies and the nested prefixes the round found, and leaves the code changes the round asks for under Open questions.

## Revision record

| Date       | Change                                                                                                                                                                                                                                                                                                                                                      |
| ---------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 2026-10-15 | First draft.                                                                                                                                                                                                                                                                                                                                                |
| 2026-10-15 | Fixes from review round 1. What a refused removed prefix becomes, the rollout for returning visitors, the jar rule for an empty prefix, the adapter table, tests and acceptance for the Fastly tester change, the code no test reaches, the consent tools that write `Path=/`, signed origins, the cookies left out, the open questions and the review log. |
