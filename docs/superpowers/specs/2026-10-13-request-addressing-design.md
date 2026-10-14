# Design Spec: A deployment may address a request by its path before its configuration is chosen

**Status:** Proposed, 2026-10-13. Raised as draft pull request jwrosewell/trusted-server#30, for Tech Lab review.
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** `2026-10-12-config-by-host-design.md`, `2026-10-12-middleware-prepare-design.md`, `2026-08-27-integration-provider-seam-design.md`
**Related PRs:** jwrosewell/trusted-server#30
**Issues:** IABTechLab/trusted-server#1328
**Last updated:** 2026-10-14

> **Why this spec exists.** An adapter chooses the app-config blob that serves a request by the request's host alone, before any settings exist, so one host cannot serve several publishers under paths of their own.

## 1. Problem

A Fastly service reads the app-config blob its `__KEY` selector names, and a selector that carries `{host}` names a blob for each host. Every publisher therefore needs a host of its own. A deployment that serves several publishers from one host, as `https://front.example/<publisher>/<path>`, each from its own settings, has no way to say which blob a request is for. The blob is chosen from the host before any settings exist, and the routes, the modules and the origin then see the publisher's part at the start of every path.

A middleware cannot do this. A middleware is named by settings, so it runs after a blob has been chosen and after the request has been routed, which is too late for both.

## 2. Design

Core offers one extension point, `RequestAddressing`, in `crates/trusted-server-core/src/addressing.rs`. The adapter asks it about each request before it chooses the blob and before it routes. It reads the request's host and path and answers either "by host", which is how every request is addressed today, or an address made of three things and nothing else.

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
3. **The path prefix it removed**, as a string. It travels on the core request as a `RemovedPathPrefix` extension, where a module's request preparer or a handler reads it.

`ByHostAddressing` answers "by host" for every request, and an adapter given no hook uses it, so a deployment that supplies none is served exactly as before.

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

| A request for                                        | Reads the blob at          | Is routed as                                | Carries the prefix     |
| ---------------------------------------------------- | -------------------------- | ------------------------------------------- | ---------------------- |
| `https://front.example/publisher-a.example/news?p=2` | `publisher-a.example`      | `/news?p=2`                                 | `/publisher-a.example` |
| `https://front.example/publisher-b.example/`         | `publisher-b.example`      | `/`                                         | `/publisher-b.example` |
| `https://front.example/unknown.example/news`         | None                       | `421 Misdirected Request`, not to be stored | None                   |
| `https://front.example/not_a_host/news`              | None, nothing is looked up | `421 Misdirected Request`                   | None                   |
| `https://front.example/health` on Fastly             | None                       | `200`, before the hook is asked             | None                   |

**The rules.**

- A hook that answers "by host" leaves the request exactly as it was, and its blob is chosen by its host as before.
- A miss on every selector value is refused `421`, exactly as a host with no blob is. The request is never served from a key the hook did not name, which includes the logical store ID and the blob of the host the request arrived on.
- A selector value is keyed as a host is, lowercased with a port and one trailing dot removed, and a value that is not a host name is passed over without a lookup. A value therefore cannot name the logical store ID or a part of a stored blob, each of which carries an underscore.
- A lookup that fails refuses the request `500` and no later value is tried, because a later value must not serve while it is unknown whether an earlier one has a blob.
- A selector without `{host}` reads its one blob and the values are ignored. The path and the prefix still apply.
- The continuing path starts with `/`, is percent-encoded ASCII and carries no query or fragment, or `RequestAddress::new` refuses it, so every adapter routes the same path.
- The values and the path come from the request, so no log line prints either. A request refused for want of a blob logs how many values the hook named and the selector.

**Why it is in core and not a middleware.** No settings exist when the blob is chosen, and the router has already matched a path by the time a middleware runs. Only a step the adapter takes before both can choose the blob and change the path the router sees, so the extension point is in core, and the policy that uses it is the deployment's.

**Why the values take the place of `{host}`.** The selector already carries the placeholder, and a staged deploy already replaces the selector with one that carries none. A staged version of a service that addresses requests by path therefore reads its one staging blob for every request and never a production one, as a service that chooses by host does. The values also pass the same host name check, so a value reaches no key a host could not. A sandbox that serves several requests still keeps each application under its blob's key. Two publishers on one host do not share a stored page, because the stored template's key carries the origin and a digest of the blob's settings as well as the host and the path.

**How each adapter takes the hook.** Each adapter takes it beside the integration builders a deployment offers, in the same call.

| Adapter    | Takes the hook through                                                          | Chooses a blob for each request     |
| ---------- | ------------------------------------------------------------------------------- | ----------------------------------- |
| Fastly     | `trusted_server_adapter_fastly::run_with_addressing(integrations, addressing)`  | Yes, by the selector values         |
| Axum       | `TrustedServerApp::routes_with_addressing(settings, &integrations, addressing)` | No, the selector values are ignored |
| Cloudflare | `TrustedServerApp::routes_with_addressing(settings, &integrations, addressing)` | No, the selector values are ignored |
| Spin       | `TrustedServerApp::routes_with_addressing(settings, &integrations, addressing)` | No, the selector values are ignored |

`run_with` and `routes_with_registrations` are unchanged, so no existing caller changes. The Fastly adapter records the hook before any request is served, as it records the builders. It answers `/health` first, then asks the hook, then serves the debug probes, so the probes read the blob and the path the request is addressed to. The other three adapters read one app-config blob, so `addressed_router` in core wraps the router they build. It routes every path for each method the inner router registers, applies the hook, and hands the request on, so a path or method the inner router refuses is refused with its own answer. There the hook is asked about every path, `/health` included where the adapter routes it, so a hook that wants the probe answered at the root answers "by host" for `/health`. The wrapped router reports its own catch-all routes as its route table, not the inner router's.

**Putting the prefix back.** Core never writes the removed prefix into a response. A deployment whose pages must carry it, in their links for example, does that with a middleware of its own, whose module's request preparer reads `RemovedPathPrefix` and leaves it for the middleware. That middleware runs in the serve phase. The stored template is keyed by the request's host and continuing path and by its blob's settings, not by the prefix, so a page changed in the fetch phase could be stored with one prefix and served under another that names the same blob.

## 3. What is not in this change

- **The deployment's policy.** Which part of a path names a publisher, and how a value is spelled, belong to the deployment that supplies the hook.
- **Writing the prefix into pages**, which is a middleware outside core.
- **Choosing a blob for each request on Axum, Cloudflare and Spin.** Those adapters read one app-config blob, as `2026-10-12-config-by-host-design.md` records.
- **Purging one page by its address.** `ts cache purge --page` derives its key from the address it is given, and a page stored for an addressed request is keyed by the front host and its continuing path. Such a page is purged by that address, which also purges the page at the same continuing path of every other publisher on the host.
- **Keeping publishers on one host apart in the browser.** Cookies and first-party storage belong to the host, so publishers under one host share them. A deployment names them apart through its prefix module, which `2026-10-12-prefix-module-design.md` describes.
- **`/health` under a prefix on Fastly.** The Fastly adapter answers `/health` before the hook is asked, so the probe stays free of settings and of the hook, and `/<publisher>/health` is addressed like any other path.

## 4. Acceptance

Tests in `crates/trusted-server-core/src/addressing.rs` cover the default answer, the path rules, a request left as it was when addressed by host, a rewritten path with its query and authority kept and its prefix carried, the host offered to the hook, and `addressed_router` routing the continuing path, routing as before with no hook or a "by host" hook, and refusing a method the inner router refuses.

Tests in `crates/trusted-server-adapter-fastly/src/config_selection.rs` cover a value taking the placeholder's place, the first existing value winning with no later lookup, every value missing refused and not served from the logical store ID or another host's blob, a value that is not a host name passed over without a lookup, a selector without the placeholder ignoring the values, and a failed lookup refusing and trying no later value. Tests in `crates/trusted-server-adapter-fastly/src/lib.rs` drive the entry point's selection under Viceroy, with no hook and with a "by host" hook choosing as before, the registered hook being the one asked, a rewritten request converted with its path, query and prefix, the first existing value read from a real local store, and every value missing answered `421` with the request left as it was. Each of the Axum, Cloudflare and Spin route tests serves `/<publisher>/_ts/config.json` as the configuration endpoint through `routes_with_addressing`, and routes a "by host" request as the router with no hook does.

## Revision record

| Date       | Change       |
| ---------- | ------------ |
| 2026-10-13 | First draft. |
