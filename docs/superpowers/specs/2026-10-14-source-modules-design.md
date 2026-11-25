# Design Spec: A source module says where the publisher path and the asset routes fetch from

**Status:** Proposed, 2026-10-14. Raised as pull request IABTechLab/trusted-server#1380 (from jwrosewell/trusted-server#34), for Tech Lab review.
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** `2026-10-13-publisher-object-storage-origin-design.md`, `2026-10-13-origin-redirect-design.md`, `2026-10-13-request-addressing-design.md`, `2026-04-28-multi-backend-asset-proxy-design.md`
**Related PRs:** jwrosewell/trusted-server#34, IABTechLab/trusted-server#1380
**Issues:** IABTechLab/trusted-server#1332
**Last updated:** 2026-10-16

> **Why this spec exists.** Core's publisher path fetches from the origin `[publisher]` names and each asset route from the origin it names, so no module can say where a request's content comes from, and where it comes from is written in two places in two shapes.

## 1. Problem

Where a deployment's content comes from is written twice. `[publisher]` carries `origin_url`, `origin_host_header_override`, `origin_auth` and `directory_index` beside who the publisher is, and every `[[proxy.asset_routes]]` entry carries its own `origin_url` and `auth`. Both are fetched by code in core that knows one kind of source, an HTTP origin.

A deployment whose origin depends on the request has no place for the code that chooses it. An origin chosen from the request's own address, as on a host that serves many publishers by path, or by a rule of the deployer's own, would otherwise be one more branch in core's fetch, running for every deployment.

The fetch and serve phases change a response. They do not choose where it comes from, so a middleware is the wrong place too.

## 2. Design

**A source module says where content comes from.** Core declares the interface. The HTTP origin fetch is the one source core supplies, as the module `origin`. A module a build links offers another under its own name, as every other module type is offered.

**One shape for where content comes from.** A source is selected in a module section, `module` naming it and a table beneath it holding its settings, the shape `[geo]` and `[prefix]` already use. The publisher's source is `[source]`. An asset route's is the route's own `source` table.

```toml
[publisher]
domain = "www.example.com"
cookie_domain = ".example.com"
proxy_secret = "proxy_secret_name"

[source]
module = "origin"

[source.origin]
url = "https://origin.example.com"
host_header = "www.example.com"

[[proxy.asset_routes]]
prefix = "/static/"

[proxy.asset_routes.source]
module = "origin"

[proxy.asset_routes.source.origin]
url = "https://bucket.object-store.example/site"
auth = { type = "s3_sigv4", region = "eu-west-1", access_key_id = "id_name", secret_access_key = "key_name" }
```

`[publisher]` keeps who the publisher is and how its pages are processed. Every field that said where the pages come from moves to the `origin` source's table, under the names in this table:

| Was                                       | Is                                        |
| ----------------------------------------- | ----------------------------------------- |
| `[publisher] origin_url`                  | `[source.origin] url`                     |
| `[publisher] origin_host_header_override` | `[source.origin] host_header`             |
| `[publisher] origin_auth`                 | `[source.origin] auth`                    |
| `[publisher] directory_index`             | `[source.origin] directory_index`         |
| `[[proxy.asset_routes]] origin_url`       | `[proxy.asset_routes.source.origin] url`  |
| `[[proxy.asset_routes]] auth`             | `[proxy.asset_routes.source.origin] auth` |

The two earlier designs for the origin, `2026-10-13-publisher-object-storage-origin-design.md` and `2026-10-13-origin-redirect-design.md`, name these settings by the places they had in `[publisher]`.

An asset route keeps `prefix`, `path_pattern`, `target_path` and `image_optimizer`, which say which requests it answers and what path it asks for. A document that writes a moved field where it used to be is refused at startup, with the new place named in the message, and so is a configuration blob, being the stored copy of the settings that a running service loads. A document with no `[source]` is refused, and so is an asset route with no `source`, naming the route. A source table naming a module the build does not link is refused with the names on offer, as every other module selection is.

The configuration page masks `url` and `host_header` by default, as it masked the fields they replace, and the signing secrets are declared at their new paths.

**What an asset route gains.** A route's source takes the same settings as the publisher's, so a route gains the two it had no place for, `host_header` and `directory_index`. A path in a route's `url`, which was refused before, now names a folder the target path is added under, as a path in the publisher's `url` does. Where the address has a folder, the route refuses a target path with a `.` or `..` segment, written plainly or percent-encoded, and answers `400 Bad Request` without sending anything, as the publisher path does for a reader's path. The route checks the target path after its rewrite, and the publisher path checks the reader's path. A target path ending in `/` has the source's `directory_index` added.

**Configuration.** Core's source is selected with `module = "origin"`, and its settings sit under `[source.origin]`, or under `[proxy.asset_routes.source.origin]` for a route. An unknown key in either table is refused, and the message names the key.

| Setting           | Required | When omitted                                                                                     | Accepted                                                                                                                                                                     |
| ----------------- | -------- | ------------------------------------------------------------------------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `url`             | Yes      | The settings are refused                                                                         | An absolute `http` or `https` address with a host, and no user name, password, query, fragment or trailing `/`. A path names a folder. Spaces around the address are trimmed |
| `host_header`     | No       | The `Host` sent is the host of `url`, with its port unless that is the usual port for the scheme | A host, or a host and port, and nothing else                                                                                                                                 |
| `directory_index` | No       | No file is added, and every path is sent as it arrived                                           | One file name, with no `/`, `%`, space, query or fragment, and not `.` or `..`                                                                                               |
| `auth`            | No       | Requests are sent unsigned                                                                       | A signing table the signer can use, beside a `url` that uses `https`                                                                                                         |

Environment variables name the settings of the publisher's source with the prefix `TRUSTED_SERVER` and `__` between the parts, such as `TRUSTED_SERVER__SOURCE__ORIGIN__URL`. When the `ts` command reads a document it applies the environment variables as an overlay, which replaces the value of a setting the document already holds and adds no setting, so a variable cannot create a table the document lacks. The overlay replaces single values and reaches no element of a list, so no variable sets an asset route's source.

**What a source answers.** For each request a source answers where the content comes from, as a location made of the origin's address (scheme, host, port and folder), the `Host` header to send, how the request is signed, and the index file added to a path ending in `/`. Whether the reader's query goes to the origin is not a separate answer. The signing's `origin_query` setting says so, and for an asset route the route's Image Optimizer settings and its default say so where the signing does not. Core uses the location in five ways:

1. It fetches the address, sending the `Host` and the signing.
2. It keys the stored pages and assets on the address and the `Host`.
3. It looks for the page's own addresses under the `Host` and rewrites them to the request's host.
4. It compares the `Location` of a redirect with the `Host`, to find a redirect the origin sends to itself.
5. It tells every middleware the `Host` as the page's origin host.

A source that answers differently for each request is kept apart in all five.

Core fetches the location with the same HTTP client as before, streaming the response where the platform offers it, and reads and writes the same stored pages and stored assets. Where no stored page or stored asset has to be looked up first, core starts the origin request in the background while the Edge Cookie record is read, as before.

**What the stored copies are keyed on.** The origin part of each key is made from the location in the form the old settings gave, so it does not change for an existing deployment. For a page and for the publisher's assets it is the address and the `Host` sent. For a copy held for an asset route it is the address and the route's prefix, and the `Host` as well where the source sets one, which no route could before. The two forms differ, so the publisher path and an asset route never share a stored copy, even for one address.

The key of a stored page and of a stored publisher asset also holds a digest of the settings as written out and of core's source files. A digest is a short code worked out from content, so it changes whenever the content does. This series changes both, because the settings now write out a `[source]` section where `[publisher]` held the origin settings, and core's source changed. Every stored page and every stored asset of the publisher is therefore fetched from the origin again on its first request after the upgrade, as it is after any release that changes core's source. The key of an asset route's copy holds no digest, so the copies of asset routes survive the upgrade.

**The host a page names itself by.** A source may fetch through one address and send the publisher's own name as the `Host`, as for an origin behind a proxy at another address. The page then names the `Host` and never the address's host. Before this series core looked for the address's host, so for a deployment that set `origin_host_header_override` the page pipeline found none of the page's own addresses to rewrite, and every middleware was told the address's host as the page's origin host. Core now hands the page pipeline and every middleware the `Host` the source sends, for a page fetched for the request and for one served from the store alike. The page's own addresses are pointed at the request's host, and `MiddlewareContext::origin_host` holds that `Host`. An HTML page whose links name only the address's host, and not the `Host`, was rewritten before and is not now. Where a source sends no other `Host` the two names are the same and nothing changes. The fetch still goes to the address, and the redirect rule compared a `Location` with the `Host` before and still does.

**One call for every path.** The publisher's page, the publisher's assets held in the asset cache, and each asset route ask their source with the same call, `locate`, and the same description of the request. The rules after that call are not all shared. The keys of their stored copies differ, as the paragraphs above set out. The publisher path answers `405` to a request that is not `GET` or `HEAD` when its location signs, where the Fastly adapter sends an asset route only `GET` and `HEAD`. Both refuse a `.` or `..` segment where the address has a folder, though the publisher path checks the reader's path and an asset route checks its target path after the rewrite.

**The request a source is given.** The method, the path and query the request continues with, the request's headers, and the prefix request addressing removed, if any. That lets a source take where to fetch from out of the address. The same prefix is on the module request that every module is given, through `ModuleRequest::removed_prefix`, so a module that names things after the tenant a path addressed, such as a prefix module naming cookies after the publisher, can tell which tenant it is. No module in this repository reads that accessor yet, and only a test does.

**Consent and privacy.** A source is not governed by the permission model. A module declares the permissions its data use needs, and core runs the module only when each is set for the request (see "Module Permissions" in `docs/guide/configuration.md`). A source declares none, so core asks the publisher's source about every request, and an asset route's source about every request the route answers, whatever the request's permissions are. The request a source is given holds the headers of the request as core received it, which include `Cookie` and `Authorization`. Core's own `origin` source reads none of them. A source from a module can read all of them, and core does not limit which it reads. A source that sends a header to another service to decide where content comes from therefore answers for itself whether the reader's permissions allow that.

**How each adapter serves a source.** All four adapters read `[source]` and `[[proxy.asset_routes]]`, and build every source when they build the integration registry, which holds the modules a build links.

| Adapter    | Asks the publisher's source for a page | Serves asset routes and asks each route's source |
| ---------- | -------------------------------------- | ------------------------------------------------ |
| Fastly     | Yes                                    | Yes, for `GET` and `HEAD`                        |
| Axum       | Yes                                    | No                                               |
| Cloudflare | Yes                                    | No                                               |
| Spin       | Yes                                    | No                                               |

Only the Fastly adapter dispatched asset routes before this change, and it still does. This change altered production code in that adapter alone, in `dispatch_fallback` and `dispatch_asset_fallback`, which now match a route to its position in the list and take that route's source from `IntegrationRegistry::route_source`. The Axum, Cloudflare and Spin adapters changed only in the settings their tests write, so on those three a `[[proxy.asset_routes]]` entry is read and never served, as before.

**When it fails.** A source that cannot be built from its settings fails the building of the integration registry, and every adapter builds the registry. The Fastly adapter then answers each request with the startup error, being `500` for a configuration error. A source that cannot say where one request's content comes from returns an error from `locate`, and core answers that request with the status of the error, being `400` for a bad request, `502` for a proxy error and `500` for a configuration error. A bad request puts its own message in the body, and the other errors answer with a short generic one. On the publisher path `locate` runs before any stored page is read, any auction starts and any origin request is sent. On the Fastly adapter an asset route's error response is marked so that nothing stores it. Core fetches the location as returned and does not check again what core's own `origin` checks when it reads its settings. A location that carries signing is signed even when its address is `http`, where `origin` refuses `auth` beside an `http` address, so a source that signs keeps to `https` itself.

**Rollout.** This change is breaking, and the changelog labels it so. Every deployment's settings change shape, so the build and the settings go live together. A new build refuses a configuration blob of the old shape, naming the new place, and an old build refuses a blob of the new shape, because that blob has no `publisher.origin_url`. Neither order of the two works, and a rollback restores the old build and the old blob together, as the changelog entry for the move of the auction providers also sets out. Four things change with the move:

1. A variable that set a moved setting under its old name, such as `TRUSTED_SERVER__PUBLISHER__ORIGIN_URL`, names a setting the moved document no longer holds, so the overlay the `ts` command applies ignores it without a message. The new name is `TRUSTED_SERVER__SOURCE__ORIGIN__URL`. The settings refuse the template's placeholder address when a deployment is validated, which stops a pipeline that sets the old name over an unedited template. A pipeline that sets the old name over a document holding a real address uses the document's address and not the variable's, with no message.
2. An `[inspect]` pattern in `show` or `hide` that names an old path, such as `publisher.origin_url` or `proxy.asset_routes[].origin_url`, matches no value in the new settings, because the moved fields are no longer written out. The section refuses a pattern that matches no value (`validate_patterns` in `crates/trusted-server-core/src/inspect/config.rs`), so the configuration is refused until the pattern names the new path.
3. Stored pages and the publisher's stored assets are fetched again after the upgrade, as the paragraph on the stored copies sets out. The stored copies of asset routes keep their keys.
4. A deployment that set `origin_host_header_override` has its pages rewritten under the `Host`, as the paragraph on the host a page names itself by sets out.

**Also in this series.** `handle_proxy` hands an integration route its request's evidence, being the client address, the headers after the caller's Edge Cookie header is removed and the address asked for. A route that asked for the request as evidence was refused before and answered `500`. The fix was made on another working branch of the fork and is carried here, and its test goes through `handle_proxy`.

## 3. What is not in this change

- Any source other than `origin`. A test supplies one that answers a different origin for each request, which is what a source reading the address does.
- A source supplying the response itself, for content that has no HTTP origin. That is a second method on the interface, added with the first source that needs it.
- **Selecting a source per path for the publisher's pages.** A deployment that wants one can write an asset route, which asks a source of its own, but only on the Fastly adapter, which alone serves asset routes. An asset route answers `GET` and `HEAD` only and passes the origin's status, body and safe headers through without the page pipeline. The paths it answers therefore lose the page changes of the `[[fetch]]` and `[[serve]]` middleware, the rewrite of the origin's own addresses in a page, the rule that keeps a redirect to the origin on the reader's address, the stored pages, the auction and the Edge Cookie work done on a page.
- **Core's source under its full name.** Write `module = "origin"`, with the settings in `[source.origin]`. The registry also resolves the full name `module = "source.origin"`, which reads its settings from `[source.source.origin]`, but the code that reads core's source by the bare name `origin` does not see such a selection. That code is the early check of the address when the settings load (`check_selection`), `Settings::origin_source` and the checks built on it when a deployment is validated, being the placeholder address and the signing key references, the default masking of `url` and `host_header` on the configuration page (`serialize_selection`), and the declaration of the signing secrets, which names `source.origin.auth` and `proxy.asset_routes[*].source.origin.auth` only. The address is still checked when the registry is built. No test writes the full name.

## 4. Acceptance

Tests in `trusted-server-core`, where the code is, unless a file is named:

- Each moved field is refused naming its new place. In a document all six are tested, the publisher's four in one test and a route's two in another. In a configuration blob only the publisher's `origin_url` is tested, and the other five are refused by the same code with no blob test of their own. A document with no `[source]` is refused, and a section naming no source is refused by the early check.
- `[source.origin]` reads, trims and checks its address, `Host`, index file and signing, and refuses an unknown key, naming the setting.
- Environment variables set the same settings at their new paths, and an old name is refused. These tests read variables with a helper that exists only in test builds, `Settings::from_toml_and_env`. The helper adds a setting the document lacks and reaches the elements of a list, which the overlay the `ts` command applies does not, so the helper refuses an old name and sets a route's settings where the overlay ignores the one and cannot do the other. No test sets an old name through the overlay.
- A location gives the host with its port, the `Host` it sends, a folder from the address's path, and an identity in the form the caches used before, so the origin part of the stored keys does not change. A route's copy is named by its address and prefix, and by the `Host` as well where its source sets one.
- A source answering a different origin for each request sends each request to its own origin and tells the page pipeline that request's origin host. The test keeps no stored copies.
- An unknown source name is refused with the names on offer.
- An asset route reads its source and signs with the source's signing. The function that builds its target address puts the path under the source's folder, adds the source's index file to a path ending in `/`, and refuses a target path with a `.` or `..` segment under a folder with `400`, in four spellings.
- A page fetched through another address, with the `Host` its source sends, names itself by that `Host`, and its own addresses are pointed at the request's host.
- The configuration page masks the origin's address and `Host` by default and declares the signing secrets at their new paths.
- A route is handed its request's evidence through `handle_proxy`, and a module request carries the removed prefix, which a held request keeps.

Nothing tests these paths:

- A source answering a different origin for each request under the stored pages and the stored assets, so no test shows two such origins kept apart in either store.
- A redirect under such a source.
- An asset route under such a source. Every route test uses core's `origin` source.
- A source that returns an error from `locate`, on the publisher path or on a route, and the response each adapter gives.
- The route's `400` and index file as answers of the handler. The tests stop at the function that builds the target address.
- The host a page names itself by for a page served from the store, and the `origin_host` a middleware is told. The test covers a page fetched for the request.
- An asset route with no `source`.
- The full name `module = "source.origin"`.
- An `[inspect]` pattern that names an old path.
- An old environment variable name through the overlay the `ts` command applies.

## Revision record

| Date       | Change                                                                                                                                                                                                                                                                                |
| ---------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 2026-10-14 | First draft.                                                                                                                                                                                                                                                                          |
| 2026-10-15 | Fixes from review round 1. What the stored keys hold, the host a page names itself by, the behavior added to asset routes and modules, configuration, adapters, consent and privacy, failure handling, rollout, the untested paths, the spelling of core's source and the review log. |
| 2026-10-15 | Review round 2 recorded, with no change to the design.                                                                                                                                                                                                                                |

## Review log

1. **Round 1, 2026-10-15.** A review in a fresh session, against `split/39-source-modules` at `ff33fea74`. Issues found, three of them blocking. The claim that stored pages and assets keep their keys was false for pages and the publisher's assets, the change to the host that a page and every middleware are told as the page's origin host was missing, and the behavior the series adds to asset routes and to modules was missing. This revision corrects the claim and states the other two, and adds the configuration, adapter, consent and privacy, failure and rollout paragraphs, the paths that have no test and a known gap in the spelling of core's source.
2. **Round 2, 2026-10-15.** A lighter review in a fresh session, against this page at `6eabcc695`, of the round 1 fixes and the new text. Approved, with no blocking issues. No change was needed.
