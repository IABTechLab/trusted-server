# Design Spec: Headers that describe the deployment stay off a publisher's pages

**Status:** Proposed, 2026-10-12. Raised as pull request [IABTechLab/trusted-server#1356](https://github.com/IABTechLab/trusted-server/pull/1356), row 15 of the stack in [IABTechLab/trusted-server#777](https://github.com/IABTechLab/trusted-server/issues/777), for Tech Lab review. The two earlier pull requests for the same commits, [jwrosewell/trusted-server#10](https://github.com/jwrosewell/trusted-server/pull/10) and [IABTechLab/trusted-server#1302](https://github.com/IABTechLab/trusted-server/pull/1302), are closed.
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** `2026-08-27-integration-provider-seam-design.md`, `2026-10-12-page-middleware-design.md`, `2026-10-12-inspection-endpoints-design.md` and `2026-10-13-middleware-settings-design.md` (row 38, built after this row), in [IABTechLab/trusted-server#1342](https://github.com/IABTechLab/trusted-server/pull/1342), which holds every design document of the stack, and `2026-09-25-git-version-header-design.md`, which main already holds
**Related PRs:** [IABTechLab/trusted-server#1356](https://github.com/IABTechLab/trusted-server/pull/1356), [IABTechLab/trusted-server#1355](https://github.com/IABTechLab/trusted-server/pull/1355) (row 14, robots.txt from modules, which this row stacks on), [IABTechLab/trusted-server#1351](https://github.com/IABTechLab/trusted-server/pull/1351) (row 10, page changes as middleware), [IABTechLab/trusted-server#1350](https://github.com/IABTechLab/trusted-server/pull/1350) (row 9, inspection endpoints), [IABTechLab/trusted-server#1379](https://github.com/IABTechLab/trusted-server/pull/1379) (row 38, middleware settings), [IABTechLab/trusted-server#1213](https://github.com/IABTechLab/trusted-server/pull/1213) (the git version header, merged to main), [jwrosewell/trusted-server#10](https://github.com/jwrosewell/trusted-server/pull/10) (closed), [IABTechLab/trusted-server#1302](https://github.com/IABTechLab/trusted-server/pull/1302) (closed)
**Issues:** [IABTechLab/trusted-server#1273](https://github.com/IABTechLab/trusted-server/issues/1273)
**Last updated:** 2026-10-27

> **Why this spec exists.** The server passed a reader of any page three headers from the publisher's origin and wrote version headers of its own, so anyone could learn which node answered, whether a cache did and which release ran, without anyone having decided to tell them. This spec says which headers now stay off a response, where the removal runs, what a deployment writes to put the versions back on its pages, and what the change leaves as it was.

## 1. Problem

Five headers told a reader of any page how the site is built.

| Header                | What it tells whoever reads it                         | Where it came from                                                                                      |
| --------------------- | ------------------------------------------------------ | ------------------------------------------------------------------------------------------------------- |
| `X-Served-By`         | Which node answered                                    | The publisher's origin, or a cache between this server and the origin                                   |
| `X-Cache`             | Whether a cache answered                               | The same                                                                                                |
| `X-Cache-Hits`        | How many times that cache has answered with the object | The same                                                                                                |
| `X-TS-Version`        | The git version the build was made from                | This server, written by all four adapters since the git version change (IABTechLab/trusted-server#1213) |
| `X-TS-Fastly-Version` | The Fastly service's version number                    | This server, written by the Fastly adapter                                                              |

Nothing on a page needs any of the five headers. The five describe how the site is built and which release runs the site, to anyone who asks for any page, without that being a decision anyone made.

## 2. Design

**The rule.** A response leaves this server without `X-Served-By`, `X-Cache` or `X-Cache-Hits` (called the deployment headers here and in the code). The one exemption is a response to a request for a path in the namespace `/_ts`, meaning `/_ts` itself and every path that starts with `/_ts/`, from which the finalize step of each adapter does not remove the three. The finalize step is where an adapter writes headers such as `X-Geo-Info-Available` and applies the operator's `[response_headers]` to a response. The origin's own answer loses the three on that path too, so the exemption keeps a header only when something other than the origin put the header on the response before the finalize step. No adapter writes `X-TS-Version` or `X-TS-Fastly-Version` on any response. The git version and the service's version appear only on the HTML pages a deployment opts into, by naming `service_version` in a `[[serve]]` entry, and no page beneath `/_ts` shows either of them.

**Where a deployment is asked about itself.** The inspection pages (row 9) answer beneath `/_ts`, and the one version they give is that of the core crate that answered, under `version`. What each shows is the publisher's decision.

**The commits.** The branch `split/15-diagnostic-headers` holds eight commits and six merges of the row below. Taken together against the row below, the eight commits change 28 files, adding 1,279 lines and removing 443. These figures and the first eight rows of the table are those of the series head `a43a05a33`. The last row is the commit "Keep the platform identity in the platform module", made on this row after that head and before row 14 is merged into it, so that the merge of row 14 only deletes `attestation.rs`.

| Commit      | What the commit does                                                                                                                                                                                            |
| ----------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `f1b6fef7d` | Core gains the rule and `ROUTE_NAMESPACE`                                                                                                                                                                       |
| `506727fad` | The finalize middleware of each of the four adapters calls the rule before it finalizes a response                                                                                                              |
| `5b632c401` | The configuration guide and the changelog                                                                                                                                                                       |
| `450dc2d88` | The publisher fallback removes the three headers from the origin's answer on every path, the function is renamed from `strip_diagnostic_headers` to `strip_deployment_headers`, and each adapter keeps one test |
| `eacc12004` | The Fastly adapter stops writing `X-TS-Version` on every response                                                                                                                                               |
| `5cc8fe77a` | A serve middleware may write a response header, and every middleware is told the platform                                                                                                                       |
| `09910474d` | Core supplies the `service_version` middleware, which a `[[serve]]` entry names to write the versions                                                                                                           |
| `a43a05a33` | No adapter writes a version on a response, core's `version_header` module is removed, and `service_version` writes the git version as well as the platform's                                                    |
| New commit  | `PlatformIdentity`, `PlatformIdentity::named` and `UNKNOWN` move into core's platform module, and each adapter's report of its platform leaves the attestation code, before row 14 is merged in                 |

The change was first called diagnostic headers, and the name of this file and of the branch still say so. The title, the code, the pull request and the configuration guide say deployment headers, and this spec does too.

**Two places run the removal.** Both call into core's `http_util`, where the three names are the one list `DEPLOYMENT_HEADERS`.

1. The publisher fallback, which answers every request no other route takes, calls `remove_deployment_headers` on whatever the origin answers, on every path. A request for an unrouted `/_ts/anything` goes to the origin, and the origin's answer does not bring the three headers back. A page served from the store is built from the stored copy and replays a fixed list of the origin's policy headers, such as `Content-Security-Policy` and `X-Frame-Options`. The list is `REPLAYABLE_POLICY_HEADERS` in `crates/trusted-server-core/src/platform/template_cache.rs` and names none of the three, so the fallback has nothing to remove from that page.
2. The finalize middleware of each adapter calls `strip_deployment_headers` with the request's path and `ROUTE_NAMESPACE`, on the response of every route the router serves. A response that this server fetched from a third party for a proxied script, image or creative loses the three headers on any path outside `/_ts` as well, because the proxy passes an upstream's headers on apart from a short list. The proxy that fetches scripts, images and creatives takes off the CORS headers that let other sites read the response, in `strip_cors_policy`, and the asset proxy, which serves a configured path from an asset origin, takes off `Set-Cookie`, `Strict-Transport-Security` and `Clear-Site-Data`, in `strip_asset_proxy_response_headers`. Both functions are in `crates/trusted-server-core/src/proxy.rs` and neither list holds any of the three. Responses that skip the finalize middleware are described under "Not in this change".

```rust
pub const ROUTE_NAMESPACE: &str = "/_ts";

pub fn strip_deployment_headers<T>(path: &str, namespace: &str, response: &mut Response<T>)
pub fn remove_deployment_headers<T>(response: &mut Response<T>)
```

`strip_deployment_headers` calls `remove_deployment_headers` unless `path` is in `namespace`. `remove_deployment_headers` takes every value of each of the three names off the response.

| Adapter    | Finalize middleware that reads the path and removes the headers before `apply_finalize_headers` |
| ---------- | ----------------------------------------------------------------------------------------------- |
| Fastly     | `FinalizeResponseMiddleware` in `crates/trusted-server-adapter-fastly/src/middleware.rs`        |
| Axum       | `FinalizeResponseMiddleware` in `crates/trusted-server-adapter-axum/src/middleware.rs`          |
| Cloudflare | `FinalizeResponseMiddleware` in `crates/trusted-server-adapter-cloudflare/src/middleware.rs`    |
| Spin       | `FinalizeResponseMiddleware` in `crates/trusted-server-adapter-spin/src/middleware.rs`          |

Spin reads the path before its `NormalizeMiddleware` runs. Normalizing keeps the path and query while it makes the URI absolute, so the path read is the path the handler sees.

- **A path is in the namespace only at a boundary.** `/_ts` and `/_ts/permissions` are. `/_tsx` and `/_ts-archive` are a publisher's pages, and lose the headers like any other. A prefix match would let a page keep the headers by its name alone.
- **The removal comes before the finalize step.** A header that finalizing writes is kept, an operator's among them. An operator can set a fixed value for `X-Cache` on every page in `[response_headers]`, and cannot bring back the origin's own value, because the origin's header is gone by then.
- **The namespace is a parameter**, so a test proves the boundary with a namespace of its own. Each adapter passes `ROUTE_NAMESPACE`. The inspection paths (`/_ts/permissions`, `/_ts/config` and `/_ts/data`) are separate literals that nothing ties to the constant.
- **The exemption keeps nothing today.** No code in the repository sets any of the three on a response, and the publisher fallback removes them from the origin's answer, so a response beneath `/_ts` reaches the finalize step without them. Open questions asks whether the exemption should stay.

**No adapter writes a version.** Before this change all four adapters wrote `X-TS-Version`, the git version the build compiled in, and the Fastly adapter also wrote `X-TS-Fastly-Version`, the Fastly service's version number. The `GET /health` probe of Axum, Fastly and Spin carried `X-TS-Version` as well. None of the four adapters writes either header now, on any response. Core's `version_header` module and the constant `HEADER_X_TS_FASTLY_VERSION` are removed. The build still compiles in the git version from `TRUSTED_SERVER__GIT_VERSION` or local git, as `2026-09-25-git-version-header-design.md` describes, and that spec's goals of sending the header from every adapter and of naming the Fastly version `X-TS-Fastly-Version` no longer hold.

The versions stay readable on the HTML pages that a `[[serve]]` entry covers, for a deployment that names core's `service_version` middleware in the entry.

```toml
[[serve]]
media_type = "text/html"
middleware = ["service_version"]
```

**`service_version`.** Core supplies this middleware and registers it under that name after every module's middleware. The stock middleware list of `trusted-server-modules` and `trusted-server.example.toml` name `service_version` last.

- No section selects `service_version`, so naming the middleware needs no module. A middleware that no entry names normally draws a startup warning that it changes no page, and `service_version` draws none because core offers the middleware to every deployment.
- `service_version` runs in the serve phase only, which is the pass over each reader's copy of a page. A `[[fetch]]` entry that names the middleware refuses startup, with a message that the middleware runs in `[[serve]]`.
- On every page the entry covers, and on nothing else, the middleware writes `X-TS-Version`, the git version the build compiled in, when the build knows one, and `X-TS-Service-Version`, the version the adapter reports for the service, when the adapter reports one. A page takes the first entry that covers the page, so a later entry that names the middleware does not reach that page.
- Only the Fastly adapter reports a service version, from `FASTLY_SERVICE_VERSION`. Axum, Cloudflare and Spin report their name and nothing else, so on those three only `X-TS-Version` can be written.
- The middleware works on HTML alone, because an entry takes `text/html` only and the serve pass runs on HTML responses only.
- Each header the middleware writes replaces any header of the same name already on the response. A version that a header cannot carry is left out, with a warning in the log.

**A serve middleware may write a response header, and every middleware is told the platform.** These are the two additions to the middleware interface that `service_version` uses.

```rust
pub type ResponseHeader = (HeaderName, HeaderValue);

pub struct MiddlewareAction {
    pub response_headers: Vec<ResponseHeader>,
    // ...
}

pub struct MiddlewareContext<'a> {
    pub platform: &'a PlatformIdentity,
    // ...
}
```

- Core writes the headers a serve chain returns on the reader's response, each replacing a header of the same name, at each place a serve pass runs, being the assembled copy of a stored page, the streamed copy and the inline copy of a page that is not stored. When several middleware write one name, the one named last wins. A stored page's serve middleware write their headers on each reader's response, on the fill and on the hit.
- A fetch middleware works on the origin's copy of a page, which may be stored for every reader, and a header goes on one reader's response. A fetch middleware that writes a header is therefore refused when the chain is planned for a page, which is when a request for that page is handled and not at startup.
- The platform is what the adapter reports about the platform it runs on, being a name, a service id, a service version, a staging flag and the location that answered (`PlatformIdentity`). This change adds `PlatformIdentity`, with `PlatformIdentity::named` and the `UNKNOWN` constant, to core's platform module (`crates/trusted-server-core/src/platform/identity.rs`). Each adapter tells the registry with `IntegrationRegistry::with_platform` when it builds its state, and core hands the same value to every middleware from `IntegrationRegistry::platform`. Fastly reports through `fastly_platform_identity`, which reads `FASTLY_SERVICE_ID`, `FASTLY_SERVICE_VERSION`, `FASTLY_IS_STAGING` and `FASTLY_POP`, and the other three report their name only.

**Consent and privacy.** The removal reads the request's path and nothing else from the request, and it only takes headers off. No consent signal, cookie or identifier takes part in the removal, so consent is not affected. The `service_version` middleware reads nothing about the reader and writes two version strings. A deployment that names the middleware shows the git version to every reader of those pages, and the getting started guide advises a neutral value such as the commit hash when a branch name is sensitive.

**Failure.** The removal cannot fail. It takes names off a header map and returns nothing, so the removal neither refuses nor delays a response. A handler error becomes a response that this server builds from a status and a short message, and that response carries nothing from an origin. A version that a header cannot carry is left out with a warning in the log, and the page is served.

## 3. What is on by default, and what is not in this change

**Defaults.** Nothing selects the removal and no setting turns it off. The three names and the namespace are constants in `crates/trusted-server-core/src/http_util.rs`, no setting is added or changed, and a stored configuration document reads as before. Upgrading therefore starts the removal on all four adapters at once, and ends the version headers at once. A deployment asks for something back by its own configuration, in one of two ways.

- `[response_headers]` sets a fixed value for one of the three names on every page.
- A `[[serve]]` entry that names `service_version` writes the versions on the pages the entry covers.

**What breaks.**

- An operator who reads `X-Cache` or `X-Served-By` from a page fetched through this server, to see whether the origin's cache answered, no longer sees them. The origin still sends the two headers, which can be read at the origin.
- A check that read `X-TS-Version` or `X-TS-Fastly-Version` from a response no longer finds either, whichever adapter served the response. The probe at `GET /health` carries neither, because `service_version` runs on HTML pages only. The Fastly service's version now has the name `X-TS-Service-Version`. Such a check can read the versions from an HTML page once the deployment names `service_version` in a `[[serve]]` entry that covers the page.

**Rollout.** For a deployment that is already running:

1. The removal and the end of the version headers start on upgrade, on all four adapters, and no setting delays them.
2. A build from the row below refuses at startup a configuration document that names `service_version`, with a message that no module that runs supplies the name. Push such a document only after every instance runs this change.
3. A deployment that wants the versions on its pages names `service_version` in the first `[[serve]]` entry that covers them.

**Not in this change.**

- `X-TS-ENV`, which the Fastly adapter writes as `staging` on every response it finalizes when `FASTLY_IS_STAGING` is `1`, that is on a staging deployment. The finalize step writes the header after the removal.
- `X-Geo-Info-Available`, which all four adapters write after the removal.
- The six `x-ts-sandbox-*` counters (instance, ordinal, builds, request id, vCPU time and heap size). The Fastly entry point adds them to a private, no-store response after the finalize step when `[debug] sandbox_metrics_enabled` is true, and the setting defaults to false.
- `Age`, `CF-Cache-Status` and `X-Cache-Status`, which say the same about a cache as the three removed names. The `ts origin probe-shareability` command reads all three, with `X-Cache`, as signs that a cache answered.
- A response the router makes itself for a request it does not route, such as the `405` for a method no route takes, and the page Fastly's startup error router makes. Such a response does not pass the finalize middleware and carries nothing from an origin. On Fastly the entry point finalizes such a response, once the entry point has the settings, with `apply_entry_point_finalize_headers`, which does not remove the three headers.
- The responses the Fastly entry point sends before the router is built, being the health probe, the refusal for a host with no configuration or a store that cannot be read, the sandbox counters endpoint (when the build has the `reusable-sandbox` feature) and the JA4 debug endpoint. The entry point sends each without finalizing it and builds each from a status and a short body, so none carries a header from an origin.
- A version on `GET /health`, or on any media type other than HTML.

**Open questions.** Each of these is a question about the code. This spec records what the code does and leaves the questions open.

1. Should the exemption for `/_ts` stay while no code beneath it writes the three headers, and if it stays, which code is meant to write them there? The origin's answer beneath `/_ts` already loses them, so the exemption keeps nothing today.
2. Why these three names? `Age`, `CF-Cache-Status` and `X-Cache-Status` say the same about a cache and are not removed. The list is `DEPLOYMENT_HEADERS` in `http_util.rs`, a name is added there, and the code gives no reason for the three.
3. Should `X-TS-ENV` and the six `x-ts-sandbox-*` counters stay on pages? Fastly writes `X-TS-ENV: staging` on every response it finalizes on a staging deployment, so the header describes the deployment on every page, as the version headers did. The counters appear only when sandbox metrics are on, and the entry point adds them after the finalize step, so the removal never sees them.
4. Should an origin's own `X-TS-Version`, `X-TS-Fastly-Version` and `X-TS-Service-Version` be removed? The `version_header` module replaced or removed an origin's `X-TS-Version`, and the Fastly adapter did the same for `X-TS-Fastly-Version`, so that an origin's value was never reported as this server's. No code removes any of the three now, and `service_version` replaces an origin's `X-TS-Version` or `X-TS-Service-Version` only when the middleware has that version to write.
5. Should each adapter's test also cover a path beneath `/_ts` and check the namespace the adapter passes? The four tests use a page path, so an adapter that always removes the headers, or that passes another namespace, passes all four.
6. Should a test send a header name more than once? A chain of caches can send the same name several times and every test inserts one value. The code relies on `HeaderMap::remove` taking every value of a name, which is what the `http` crate version in the lockfile (1.4.2) does.
7. Should a test tie `ROUTE_NAMESPACE` to the inspection paths, which are separate literals?
8. Should one test run a real router with the finalize middleware and the publisher fallback together, with a proxied third-party response among the responses? Today the adapter tests call the finalize middleware with a canned response, and the one fallback test calls the fallback directly.
9. Should `2026-09-25-git-version-header-design.md`, which still reads `Implemented`, and its plan carry a note that this change replaces what they describe?
10. Should the changelog's Security entry for the three headers carry **Breaking:**, as the entry for the version headers under Changed does, given that the pull request description lists the removal under what breaks?
11. Under the stack's rule that only middleware changes a response, a separate audit of the stack is listing the headers that core or an adapter still writes directly, such as `X-TS-ENV`, `X-Geo-Info-Available` and the six `x-ts-sandbox-*` counters, so should each of those headers move into a serve middleware?

## 4. Tests

- Core's `http_util` has five tests of the rule. A publisher's page loses all three headers. `/_ts`, `/_ts/permissions` and `/_ts/debug/ja4` keep them. Headers outside the list (`x-ts-version`, `x-geo-info-available` and `cache-control`) are left alone. `/_tsx`, `/_ts-archive` and `/_tsfoo/bar` lose `x-served-by` as pages, the only one of the three headers that the test of these paths sends. The boundary is the namespace passed in.
- One publisher test sends a `404` that carries the three headers for `/_ts/anything` and checks that none reaches the reader.
- Each adapter has one test of its finalize middleware. A page path loses `x-served-by` and `x-cache-hits`, and an operator's `X-Cache` from `[response_headers]` is sent in place of the origin's.
- `service_version` has seven tests of its own. The git version is written alone, the platform's version is written alone, both are written in that order, neither writes nothing, a version a header cannot carry is left out, the default carries the compiled-in git version, and the middleware runs in the serve phase under its name.
- Publisher tests check that both headers reach a buffered page, that the platform's version reaches a streamed page, that a page no entry names the middleware for carries neither header, that a platform reporting no version writes no `X-TS-Service-Version`, and that a stored page's serve middleware write their headers on the fill and on the hit.
- Middleware tests check that a fetch middleware that writes a header is refused, that a serve chain's headers are written on the response with a later middleware replacing an earlier one, that the inline path hands the headers back, that an action with only a header is not a pass, and that a middleware is told the platform.
- The health tests of Axum, Fastly and Spin check that the probe carries no version.

The paths these tests leave out are listed as questions under Open questions.

## 5. Acceptance

The fork's CI passed on the series head, `a43a05a33`, all nineteen checks, and the description of IABTechLab/trusted-server#1356 records that, with a link to jwrosewell/trusted-server#10. The same description has every check command of its test plan ticked, being the Fastly and Axum tests and lints, the JavaScript tests, the WebAssembly build of the Fastly adapter and the format commands for the Rust code, the JavaScript and the documentation. A last line for the full workflow set of the repository, run on jwrosewell/trusted-server#10, is ticked as well, and manual testing with `fastly compute serve` is not. The description of IABTechLab/trusted-server#1356 does not say whether anyone also ran the commands by hand on the head. The descriptions of IABTechLab/trusted-server#1356, of IABTechLab/trusted-server#1302 and of jwrosewell/trusted-server#10 hold no record of a lint and a test of each commit on its own or of deliberate faults made one at a time, so this spec claims neither. The description of IABTechLab/trusted-server#1356 also says that the responses the Fastly entry point sends outside the middleware are not exercised, and those are listed under "Not in this change".

## Revision record

| Date       | Change                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| ---------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 2026-10-12 | First draft, from the pull request and its issue as they stand.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                    |
| 2026-10-15 | Fixes from review round 1. The five commits since 9 October, being the fallback removal, the response headers a serve middleware may write, the platform in the middleware context, the `service_version` middleware and the end of the version headers on every adapter, the function names, the paths beneath `/_ts` and what the exemption keeps, the adapters and the responses that skip the finalize step, the defaults and the rollout, what breaks, the failure and privacy answers, the tests, the open questions, the header and the acceptance lines brought to the series head, and the review log.                    |
| 2026-10-16 | Fixes from review round 2. The acceptance says what the description of the pull request for this row records, being the fork's run of all nineteen checks on the head, the ticked check commands and the manual test left unticked, in place of saying that no description records a run of the whole check. Three clauses that were only partly true are qualified, being the headers a stored page replays from the origin, the headers each proxy takes off and the one header the prefix test sends. A new open question asks whether the headers that core or an adapter writes directly should move into a serve middleware. |
| 2026-10-27 | Row 12 no longer adds the signed document that reported the build and the service's version, so the versions are readable only where `service_version` writes them, and this change adds `PlatformIdentity` to core's platform module. The header, the pages beneath `/_ts`, the namespace note, the versions, the platform a middleware is told, what breaks, the rollout, `X-TS-ENV` and open questions 3 and 7 say so.                                                                                                                                                                                                          |
| 2026-10-27 | The rule says where the git version and the service's version appear, the inspection pages give the core crate's version, the commits table names the series head `a43a05a33` and gains the commit that keeps `PlatformIdentity` in the platform module, the platform bullet names `fastly_platform_identity` and when each adapter reports its platform, and the header no longer calls the pull request a draft.                                                                                                                                                                                                                 |

## Review log

1. **Round 1, 2026-10-15.** A review in a fresh session, against `split/15-diagnostic-headers` at `a43a05a33`. Issues found, four of them blocking. The four were that the spec covered the three commits of 9 October and none of the five since, that the spec left `X-TS-Version` as it was and left out the breaking change the pull request lists for it, that the rule for paths beneath `/_ts` was wider than the code for an origin's answer while the inspection pages read as if they showed headers nothing writes, and that the acceptance cited a record the pull request descriptions do not hold. This revision states what the code does at that head for all eight commits, the paths beneath `/_ts`, the adapters, the version headers and the `service_version` middleware, the defaults, the rollout, what breaks, the failure and privacy answers, the tests and the acceptance, and lists under open questions what waits for code changes, being the exemption, the list of names, the headers still sent, an origin's version headers, the untested paths and the documents.
2. **Round 2, 2026-10-16.** A lighter review in a fresh session, checking the round 1 fixes against `split/15-diagnostic-headers` at `a43a05a33`. Issues found, none blocking. This revision corrects the acceptance, which said the pull request descriptions hold no record of a run of the whole check on the head although the description of the pull request for this row (IABTechLab/trusted-server#1356) has every check command ticked, and it qualifies three clauses that were only partly true, being what a stored page replays from the origin, what the proxies take off a response and which header the prefix test sends.
3. **Round 3.** A review in a fresh session, against `split/15-diagnostic-headers`, of the changes that follow attestation leaving row 12. Issues found, none blocking. The design said that what describes a deployment is answered where the deployment is asked about itself, which stops holding once row 12 no longer adds the signed document that reported the build and the service's version, because no page beneath `/_ts` then gives either version, and the design left out `fastly_platform_identity`, which this row adds with `PlatformIdentity`. This revision says that the git version and the service's version appear only on the HTML pages a deployment opts into through `service_version`, that the inspection pages give the core crate's version, and that Fastly reports its platform through `fastly_platform_identity`.
4. **Round 4.** A review in a fresh session, against `split/15-diagnostic-headers`, of the round 3 fixes and the new text. Issues found, none blocking. This revision says that the commit table and its counts are those of the series head, adds a row for the commit that keeps `PlatformIdentity` in core's platform module, and says that each adapter reports its platform when it builds its state.
