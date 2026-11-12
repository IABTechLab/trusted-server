# Design Spec: A deployment may address a request by its path before its configuration is chosen

**Status:** Proposed, 2026-10-13. Raised as pull request IABTechLab/trusted-server#1376 (from jwrosewell/trusted-server#30, which is closed), for Tech Lab review.
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** `2026-10-12-config-by-host-design.md`, `2026-10-12-middleware-prepare-design.md`, `2026-10-12-prefix-module-design.md`, `2026-08-27-integration-provider-seam-design.md` and `2026-10-15-cookies-by-path-design.md`, all in IABTechLab/trusted-server#1342, which holds every design document of the stack
**Related PRs:** IABTechLab/trusted-server#1376, jwrosewell/trusted-server#30 (closed)
**Issues:** IABTechLab/trusted-server#1328, IABTechLab/trusted-server#1118
**Last updated:** 2026-10-17

> **Why this spec exists.** An adapter chooses the app-config blob that serves a request by the request's host alone, before any settings exist, so one host cannot serve several publishers under paths of their own.

## 1. Problem

A Fastly service reads the app-config blob its `__KEY` selector names, and a selector that carries `{host}` names a blob for each host. Every publisher therefore needs a host of its own. A deployment that serves several publishers from one host, as `https://front.example/<publisher>/<path>`, each from its own settings, has no way to say which blob a request is for. The blob is chosen from the host before any settings exist, and the routes, the modules and the origin then see the publisher's part at the start of every path.

A middleware cannot do this. A middleware is named by settings, so it runs after a blob has been chosen and after the request has been routed, which is too late for both.

## 2. Design

Core offers one extension point, `RequestAddressing`, in `crates/trusted-server-core/src/addressing.rs`. A deployment supplies a hook, meaning its own implementation of `RequestAddressing`, and the adapter asks the hook about each request before it chooses the blob and before it routes. The hook is given the request's host and path and answers either "by host", which is how every request is addressed today, or an address made of three things and nothing else.

```rust
pub trait RequestAddressing: Send + Sync {
    fn address(&self, host: Option<&str>, path: &str) -> Addressing;
}

pub enum Addressing {
    ByHost,
    Addressed(RequestAddress),
}

impl RequestAddress {
    pub fn new(
        selector_values: Vec<String>,
        path: String,
        removed_prefix: String,
    ) -> Result<Self, Report<AddressingError>>;
}
```

1. **The selector values**, in order. Each takes the place of `{host}` in the `__KEY` selector in turn, and the first whose blob exists is read.
2. **The path the request continues with**, which is set on the request before routing. The query is kept.
3. **The removed path prefix**, as a string. It travels on the core request as a `RemovedPathPrefix` extension, where a module's request preparer or a handler reads it.

`ByHostAddressing` answers "by host" for every request. The Fastly adapter uses it when a deployment registers no hook, and the other three adapters ask a hook only when a deployment builds its router with `routes_with_addressing`, so a deployment that supplies none is served exactly as before.

A deployment's policy, such as which part of the path names a publisher, lives in the deployment and not in this repository. On a Fastly service whose `__KEY` selector is `{host}`, the hook below gives the results in the table.

```rust
impl RequestAddressing for FirstSegment {
    fn address(&self, _host: Option<&str>, path: &str) -> Addressing {
        let rest = path.strip_prefix('/').unwrap_or(path);
        let (segment, tail) = rest.split_once('/').unwrap_or((rest, ""));
        RequestAddress::new(vec![segment.to_owned()], format!("/{tail}"), format!("/{segment}"))
            .map_or(Addressing::ByHost, Addressing::Addressed)
    }
}
```

| A request for                                        | Reads the blob at          | Is routed as                                | Carries the removed path prefix |
| ---------------------------------------------------- | -------------------------- | ------------------------------------------- | ------------------------------- |
| `https://front.example/publisher-a.example/news?p=2` | `publisher-a.example`      | `/news?p=2`                                 | `/publisher-a.example`          |
| `https://front.example/publisher-b.example/`         | `publisher-b.example`      | `/`                                         | `/publisher-b.example`          |
| `https://front.example/unknown.example/news`         | None                       | `421 Misdirected Request`, not to be stored | None                            |
| `https://front.example/not_a_host/news`              | None, nothing is looked up | `421 Misdirected Request`                   | None                            |
| `https://front.example/health` on Fastly             | None                       | `200`, before the hook is asked             | None                            |

A selector that carries no `{host}` ignores the selector values, so given a hook it serves every addressed request from its one blob, as a staged version does.

**The rules** are those the guide gives under "One blob for each path" in `docs/guide/configuration.md`. How a value is keyed and why a failed lookup stops the search are in the rustdoc of `resolve_addressed_key`, and what a continuing path may be is in that of `RequestAddress::new`. The values and the path come from the request, so no log line prints either. Each value costs one store lookup until one is found, so a hook names as few values as it can, usually one. `RequestAddress::new` refuses a continuing path that does not start with `/`, carries a query or a fragment, or is not a valid URI path, but it does not refuse `.` or `..` segments, so a hook returns a path without them.

**When it fails.** Every value missing is answered `421 Misdirected Request` with `Cache-Control: private, no-store`, as the table shows. A store that cannot be read is answered `500` on Fastly, and on the other three adapters `addressed_router` answers `500` when the continuing path cannot be set on the request. Neither is served from another publisher's blob.

**Rollout.** A deployment that supplies no hook changes nothing. One that adopts a hook on a Fastly service whose selector carries `{host}` writes each publisher's blob before it deploys the build that carries the hook, because a request addressed to a value with no blob is answered `421`. The other three adapters ignore the selector values, so a hook there needs no blob written first. The Fastly log line for a failed lookup now names the config store but not the key, because the key is built from the request.

**Why it is in core and not a middleware.** No settings exist when the blob is chosen, and the router has already matched a path by the time a middleware runs. Only a step the adapter takes before both can choose the blob and change the path the router sees, so the extension point is in core, and the policy that uses it is the deployment's.

**Why the values take the place of `{host}`.** The selector already carries the placeholder, and a staged deploy already replaces the selector with one that carries none. A staged version of a service that addresses requests by path therefore reads its one staging blob for every request and never a production one, as a service that chooses by host does. The values also pass the same host name check, so a value reaches no key a host could not. A sandbox that serves several requests still keeps each application under its blob's key. Two publishers on one host do not share a stored page, because the stored template's key carries the origin and a digest of the blob's settings as well as the host and the path, and the asset cache's key carries the origin as well as the host and the URL (`crates/trusted-server-core/src/asset_cache.rs`).

**How each adapter takes the hook.** Each adapter takes it beside the integration builders a deployment offers, in the same call.

| Adapter    | Takes the hook through                                                          | Gives the hook the host from               | Chooses a blob for each request     |
| ---------- | ------------------------------------------------------------------------------- | ------------------------------------------ | ----------------------------------- |
| Fastly     | `trusted_server_adapter_fastly::run_with_addressing(integrations, addressing)`  | The `Host` header                          | Yes, by the selector values         |
| Axum       | `TrustedServerApp::routes_with_addressing(settings, &integrations, addressing)` | The `Host` header, or else the authority   | No, the selector values are ignored |
| Cloudflare | `TrustedServerApp::routes_with_addressing(settings, &integrations, addressing)` | The `Host` header, or else the authority   | No, the selector values are ignored |
| Spin       | `TrustedServerApp::routes_with_addressing(settings, &integrations, addressing)` | Usually nothing, as the next lines explain | No, the selector values are ignored |

On Spin the hook is usually given no host, because Spin rarely passes the `Host` header and its request URI carries only the path. A `Host` header that is present is the client's own, because the hook is asked before `NormalizeMiddleware` sets the trusted host, so a hook on Spin reads the path alone. The shipped entry points of the Axum, Cloudflare and Spin adapters build their router from `TrustedServerApp::routes()`, so a deployment that supplies a hook there builds its own entry point that calls `routes_with_addressing`.

`run_with` and `routes_with_registrations` are unchanged, so no existing caller changes. The Fastly adapter records the hook before any request is served, as it records the builders. It answers `/health` first, then asks the hook, then serves the debug probes, so the probes read the blob and the path the request is addressed to. The other three adapters read one app-config blob, so `addressed_router` in core wraps the router they build. It routes every path for each method the inner router registers, applies the hook, and hands the request on, so a path or method the inner router refuses is refused with its own answer. There the hook is asked about every path, `/health` included where the adapter routes it, so a hook that wants the probe answered at the root answers "by host" for `/health`. The wrapped router reports its own catch-all routes as its route table, not the inner router's.

**Putting the removed path prefix back.** Core never writes the removed path prefix into a response. A deployment whose pages must carry it, in their links for example, does that with a middleware of its own, whose module's request preparer reads `RemovedPathPrefix` and leaves it for the middleware in `IntegrationRequestState`. That middleware runs in the serve phase. This has a cost. A request that carries any value in `IntegrationRequestState` keeps to the origin path, so its page is never read from a stored template or stored as one, and its HTML is sent `private, no-store` (see `IntegrationRequestState` in `crates/trusted-server-core/src/integrations/registry.rs`). Every page whose removed path prefix is put back this way is therefore made for its reader alone. The request to the origin for that page bypasses the platform's cache only when origin readthrough is on or the ad stack runs (`apply_origin_cache_intent` in `crates/trusted-server-core/src/publisher.rs`). Origin readthrough is off by default (`origin_readthrough_enabled`, see "Origin readthrough caching" in `docs/guide/configuration.md`), so with no ad stack the platform's cache can still answer under the origin's own `Cache-Control`. The stored template is keyed by the request's host and continuing path and by its blob's settings, not by the removed path prefix, so a page changed in the fetch phase could be stored with one removed path prefix and served under another that names the same blob.

**The host core processes a request for.** Once a request is routed, core reads the host it rewrites a page for and tells each middleware from `Forwarded`, then `X-Forwarded-Host`, then `Host`, and last from the URI's authority, as `address_request` gives the hook the authority where no `Host` is sent (`RequestInfo::from_request` in `crates/trusted-server-core/src/http_util.rs`). An HTTP/2 client may send its `:authority` and no `Host` header, and before the authority was read such a request had no host, so its page was returned unprocessed (IABTechLab/trusted-server#1118).

## 3. What is not in this change

- **The deployment's policy.** Which part of a path names a publisher, and how a value is spelled, belong to the deployment that supplies the hook.
- **Writing the removed path prefix into pages**, which is a middleware outside core.
- **Choosing a blob for each request on Axum, Cloudflare and Spin.** Those adapters read one app-config blob, as `2026-10-12-config-by-host-design.md` records.
- **Purging one page by its address.** `ts cache purge --page` derives its key from the address it is given, and a page stored for an addressed request is keyed by the front host and its continuing path. Such a page is purged by that address, which also purges the page at the same continuing path of every other publisher on the host.
- **Keeping publishers on one host apart in the browser.** Publishers addressed by path on one host are one browser origin, so they share its cookies and first-party storage. A deployment's prefix module names apart only the cookies core names through `cookie_name`, such as the Edge Cookie family and the `eids` cookie (`2026-10-12-prefix-module-design.md`). The consent cookies and the `sharedId` cookie have fixed names that core reads, so consent given on one publisher's pages is read on another's. The Cookie header also reaches the chosen origin unchanged, so each publisher's origin receives the cookies the browser holds for the others. `2026-10-15-cookies-by-path-design.md` keeps those cookies with the publisher.
- **The auction under a removed path prefix.** The client script posts to the root-relative `/auction` (`crates/trusted-server-js/lib/src/core/request.ts`), which carries no publisher's part, so on a host addressed by path the hook cannot tell which publisher an auction is for. Taking the script's requests under the publisher's path is taken up in `2026-10-15-cookies-by-path-design.md`.
- **`/health` under a removed path prefix on Fastly.** The Fastly adapter answers `/health` before the hook is asked, so the probe stays free of settings and of the hook, and `/<publisher>/health` is addressed like any other path.

## 4. Acceptance

Tests in `crates/trusted-server-core/src/addressing.rs` cover the default answer, the path rules, a request left as it was when addressed by host, a rewritten path with its query and authority kept and its removed path prefix carried, the host offered to the hook, and `addressed_router` routing the continuing path, routing as before with no hook or a "by host" hook, and refusing a method the inner router refuses.

Tests in `crates/trusted-server-adapter-fastly/src/config_selection.rs` cover a value taking the placeholder's place, the first existing value winning with no later lookup, every value missing refused and not served from the logical store ID or another host's blob, a value that is not a host name passed over without a lookup, a selector without the placeholder ignoring the values, and a failed lookup refusing and trying no later value. Tests in `crates/trusted-server-adapter-fastly/src/lib.rs` drive the entry point's selection under Viceroy, with no hook and with a "by host" hook choosing as before, the registered hook being the one asked, a rewritten request converted with its path, query and removed path prefix, the first existing value read from a real local store, and every value missing answered `421` with the request left as it was. Each of the Axum, Cloudflare and Spin route tests serves `/<publisher>/_ts/config.json` as the configuration endpoint through `routes_with_addressing`, and routes a "by host" request as the router with no hook does.

Tests in `crates/trusted-server-core/src/http_util.rs` cover the host read from the URI's authority, with its port, where no header names one, no host where nothing names one, and each of `Host`, `X-Forwarded-Host` and `Forwarded` taking precedence over the authority. `a_request_naming_its_host_only_in_its_uri_reaches_the_middleware`, in `crates/trusted-server-core/src/publisher.rs`, sends a request with no `Host` header through the publisher path and finds that the entry's middleware ran for the host the URI names.

Two paths have no test. No test drives `edgezero_main` with a hook registered through `run_with_addressing`, which appears only in a doctest that is not run, and no test covers the `500` that `addressed_router` answers when the continuing path cannot be set on the request.

## Revision record

| Date       | Change                                                                                                                                                                                                                                         |
| ---------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 2026-10-13 | First draft.                                                                                                                                                                                                                                   |
| 2026-10-15 | Fixes from review round 1. The cost of putting the removed path prefix back, the shared browser origin as a limit, the host each adapter gives the hook, failure handling, rollout, the untested paths and the review log.                     |
| 2026-10-15 | Corrections from review round 2. The origin fetch of a page whose removed path prefix is put back, the rollout order on Fastly, where the copy on `split/6-integration-seam` is brought up to date, and the removed path prefix named in full. |
| 2026-10-17 | The host core processes a request for falls back to the URI's authority where no header names one (IABTechLab/trusted-server#1118), with its tests.                                                                                            |

## Review log

1. **Round 1, 2026-10-15.** A review in a fresh session, against `split/35-request-addressing` at `0af81b181`. Issues found, two of them blocking. "Putting the removed path prefix back" left out that a page whose removed path prefix is put back is never stored and is sent `private, no-store`, and section 3 claimed publishers on one host could be kept apart in the browser when the consent cookies, the `sharedId` cookie and the forwarded Cookie header are shared. Both are fixed in this revision, with two of the three partly true claims, the coverage gaps and the advice the round gave. The third partly true claim concerned the copy of this page on `split/6-integration-seam`, which still lists the rules in full where this page points to the guide and the rustdoc, and that copy is brought up to date in the specs pull request IABTechLab/trusted-server#1342.
2. **Round 2, 2026-10-15.** A lighter review in a fresh session, against this page at `d58b3f404`, of the round 1 fixes and the new text. No blocking issues. This revision corrects the claim that every page whose removed path prefix is put back is fetched from the origin for each reader, limits the rollout order to a Fastly service whose selector carries `{host}`, says in the round 1 entry that the copy of this page on `split/6-integration-seam` is brought up to date in the specs pull request, and writes "removed path prefix" wherever the text means it.
