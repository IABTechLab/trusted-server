# Design Spec: A deployment using 51Degrees gets client hints from one middleware

**Status:** Proposed, 2026-10-14. Raised as pull request IABTechLab/trusted-server#1386 (from jwrosewell/trusted-server#39), for Tech Lab review.
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** `2026-10-12-shared-vendor-tables-design.md` (the `[shared.51degrees]` table), `2026-10-12-loading-cache-design.md` (the `51degrees-answers` cache), `2026-10-12-page-middleware-design.md` (the `[[serve]]` entries), `2026-10-14-source-modules-design.md` (the source a page is fetched from) and `2026-10-17-request-filters-on-every-adapter-design.md` (the request filters every adapter runs), all in IABTechLab/trusted-server#1342, which holds every design document of the stack
**Related PRs:** jwrosewell/trusted-server#39, IABTechLab/trusted-server#1386
**Issues:** IABTechLab/trusted-server#1333
**Last updated:** 2026-10-26

> **Why this spec exists.** A browser sends its high entropy client hints only when the site asks for them and delegates them, and nothing in the stack writes the headers and the tag that do so for a deployment that uses the 51Degrees service, so every first page view of a session is priced on the User-Agent alone.

## 1. Problem

A browser sends its high entropy client hints only when a response asks for
them with `Accept-CH`, retries the first navigation carrying them only when
`Critical-CH` names them, and lets a third party read them only when the top
level page delegates them, by a `Delegate-CH` tag in the page as served or a
`Permissions-Policy` header. The stack has the places to write each of those,
being the serve phase, the headers step and the head inserts, and no module
that does.

The list of hints is the service's. The 51Degrees service names, for each
browser, the hints it wants in three properties, and the list changes as the
service does. A list written into a deployment drifts from it.

In this document a deployment uses 51Degrees when its settings carry the
`[shared.51degrees]` table, because the stack has no 51Degrees device module
yet (section 3) and the table is what runs the modules that call the service.
In this document an origin on its own is a web origin, being a scheme, a host
and a port, and the server a publisher's pages come from is its source, as
`2026-10-14-source-modules-design.md` names it.

## 2. Design

**A serve middleware, `51degrees.client_hints`, in `crates/middleware/51degrees`.**
The crate registers the middleware and a request filter together. As the
filter sees a page navigation whose path a `[[serve]]` entry naming the
middleware covers, it asks the service, through the cache the 51Degrees
modules share, which hints it wants from the request's browser, and leaves the
names in the request's state. Core copies them into the document state the
serve middleware share. From that list the middleware writes
`Accept-CH`, `Critical-CH` and `Permissions-Policy` through
`Middleware::response_headers`, each adding to what the response already
carries under that name as "What the headers keep" says, and the
`Delegate-CH` tag in the head through its action. It reads `critical` from its
own table, `[middleware.51degrees.client_hints]`, on by default.
`Accept-CH` and `Critical-CH` are left off where the request already carries
every hint the service names and the response names no `Accept-CH` set of its
own, because a browser keeps an origin's set until a response names another.

The three parts run in different places.

| Part                                                | Runs                                                                                                   | Decided by                                                                                                 |
| --------------------------------------------------- | ------------------------------------------------------------------------------------------------------ | ---------------------------------------------------------------------------------------------------------- |
| The filter, which asks the service and leaves names | Before routing, on every adapter, and it acts on page navigations only                                 | Any `[[serve]]` entry naming the middleware whose `path` covers the request path, the rule the headers use |
| The `Delegate-CH` tag                               | In the serve phase, on each page                                                                       | The first `[[serve]]` entry that covers the page's media type and path                                     |
| `Accept-CH`, `Critical-CH` and `Permissions-Policy` | In the headers step of the serve phase, on each response the publisher path sends, a redirect included | Any `[[serve]]` entry whose `path` covers the request path, whatever its media type                        |

A page navigation, for the filter, is a `GET` or `HEAD` request whose
`Sec-Fetch-Dest` is `document` or whose `Accept` names `text/html`, so a
request for a frame, or one a script makes for an HTML fragment, qualifies
when it sends such an `Accept`. Any other request asks the service nothing,
and so does a page navigation whose path no `[[serve]]` entry naming the
middleware covers, which is where no headers would be written. A service that
names no hints for a browser, or does not answer, leaves nothing, and the page
gets no headers and no tag. The origin delegated to is
the origin of the configured endpoint, and an endpoint on the page's own
origin delegates nothing.

**What the headers keep.** For each of `Accept-CH`, `Critical-CH` and
`Permissions-Policy` the middleware reads every line the response carries
under that name, joins them in order into one list, and adds the hints after
it. `Accept-CH` and `Critical-CH` keep the response's hints and name each hint
once. `Permissions-Policy` keeps the response's members and adds one
delegation after them for each hint, as
`ch-ua-model=(self "https://cloud.51degrees.com")`. A later member of a name
replaces an earlier one in a policy, so for each hint the service names, the
delegation overrides a member of the publisher's own that denies the hint,
such as `ch-ua-model=()`. Core then writes each header with
`write_response_headers` (`crates/trusted-server-core/src/middleware.rs`),
which uses `HeaderMap::insert` and so replaces every line of that name with
the one line written, which already holds what each of them said.

**What the 51Degrees modules share, in `crates/shared/51degrees`.** The
vendor's table, `[shared.51degrees]`, being `endpoint`, `license_key` and
`timeout_ms`, whose presence runs the 51Degrees builder. The license key
every server side call presents, and the refusal when the table has none.
The one call to the service for a request's evidence, a form POST to the
service's `json` endpoint over the platform's HTTP client, naming the
properties each module registered so one call carries them all, and its
answer loaded through the loading cache, so that where a deployment links a
loading cache module a question many requests ask at once is asked once, and
in a stock build, which links none, every call asks the service
(`2026-10-12-loading-cache-design.md`, section 7). The evidence a request
gives, being the `User-Agent` and the `Sec-CH-UA` hints and nothing else. The
names the service asked for, as the type a module reads them from the
request's state by.

**The cache is a declared one.** The builder declares `51degrees-answers`,
and a deployment selects the loading cache that holds it and sizes its copies
in `[cache.51degrees-answers]`, as every declared cache is selected. A build
that links one loading cache module holds the answers without a line in the
document. A build that links no loading cache module, or several with no
`[cache.51degrees-answers]` table naming one, holds nothing, so every page
navigation an entry covers calls the service, and every stock build links
none. On Fastly the copies are held in the host's value cache, shared by every
instance at a location, with a brief
copy in process memory in front. An answer is held for an hour at most. The
key is a hash of the endpoint, the license key, the properties and the
evidence sent, being the `User-Agent` and the `Sec-CH-UA` hints, so one held
answer serves every reader whose browser sends the same values, and only the
first navigation from each such browser waits for the service.

**The builder is run by the table, not selected by a section.** It carries
the vendor's shared table, declares the license key as a secret setting in
use whenever the table is present, and registers the filter and the
middleware together. The stock list ships it, so every adapter and the `ts`
command line tool take it, and every adapter runs its filter.

```toml
[shared.51degrees]
endpoint = "https://cloud.51degrees.com/api/v4/json"
license_key = "fiftyone_degrees_license_key"

[[serve]]
media_type = "text/html"
middleware = ["51degrees.client_hints"]
```

**The settings.**

| Setting                                        | Default                                                                    | Contract                                                                                                                                                                                                                                          |
| ---------------------------------------------- | -------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `[shared.51degrees] endpoint`                  | Required                                                                   | A URL. The call goes to the endpoint with its last path segment replaced by `json`, so a resource key written in that segment is never sent, and a query or fragment on the endpoint is dropped. The hints are delegated to the endpoint's origin |
| `[shared.51degrees] license_key`               | Required                                                                   | The name of the license key's entry in the secret store. A table with none, or an empty one, is refused by the configuration check and when the application state is built                                                                        |
| `[shared.51degrees] timeout_ms`                | `500`                                                                      | `1` to `10000` milliseconds. Only the Fastly adapter applies it, see "What bounds the call on each adapter"                                                                                                                                       |
| `[middleware.51degrees.client_hints] critical` | `true`                                                                     | `true` or `false`                                                                                                                                                                                                                                 |
| `[cache.51degrees-answers]`                    | `memory_seconds` 60, `store_seconds` 604800 and `store_idle_seconds` 86400 | The four settings every declared cache takes. An answer ends after an hour whatever `store_seconds` says                                                                                                                                          |

`[shared.51degrees]` and `[middleware.51degrees.client_hints]` each refuse a
key they do not read.

**What bounds the call on each adapter.** Every adapter runs request filters
(`2026-10-17-request-filters-on-every-adapter-design.md`), so the filter asks
the service on each of them. A module's time limit reaches an adapter only as
the timeouts of the backend it registers for the call, and only the Fastly
adapter applies them, so `timeout_ms` bounds the call there alone.

| Adapter                     | What bounds the call                                                                                                                                                                           |
| --------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Fastly                      | `timeout_ms` sets the first byte and between bytes timeouts of the backend, and the connection timeout is fixed at one second (`crates/trusted-server-adapter-fastly/src/backend.rs`)          |
| Axum, a development adapter | The adapter's HTTP client ignores `timeout_ms` and allows 5 seconds to connect and 30 in all (`crates/trusted-server-adapter-axum/src/platform.rs`)                                            |
| Cloudflare                  | The adapter's fetch sets no timeout, and the adapter's own note says only the Workers runtime's limit bounds a call (`crates/trusted-server-adapter-cloudflare/src/platform.rs`)               |
| Spin                        | `spin_sdk::http::send` takes no timeout, so `timeout_ms` is ignored, and the adapter's own note says the Spin runtime's default applies (`crates/trusted-server-adapter-spin/src/platform.rs`) |

**What each navigation sends.** For a page navigation an entry covers, the
filter asks the cache for the answer to one question, and where none is held
it makes one `POST` to the service. The form carries the license key, the
three properties that name the hints, and the evidence, being the
`User-Agent` and each `Sec-CH-UA` header the request carries, that is
`Sec-CH-UA` and every header whose name begins `Sec-CH-UA-`, under their own
names (`crates/shared/51degrees/src/evidence.rs`). Neither the visitor's
address nor a cookie nor any other header is sent. A request with no
`User-Agent` asks nothing. The filter reads only the three hint properties
from the answer.

**Consent.** Every adapter hands every request filter the permissions
resolved for the request, so a filter can skip or narrow what it does when a
permission it depends on is unset (`docs/guide/permission-model.md`). The
51Degrees filter does not read them and the middleware declares no
permission, so the call is made whatever the permissions resolved for the
request say. The hints the browser then sends can also reach DataDome, where
its protection filter is enabled, because that filter builds its request to
DataDome's service from the `Sec-CH-UA`, `Sec-CH-UA-Arch`,
`Sec-CH-UA-Full-Version-List`, `Sec-CH-UA-Mobile`, `Sec-CH-UA-Model`,
`Sec-CH-UA-Platform` and `Sec-CH-Device-Memory` headers a request carries
(`crates/bot-protection/datadome/src/protection.rs`).

**Open questions for the reviewer.** This change takes no position on two
things.

The first is whether the call to the service needs a permission, and if it
does which one, with no call, no names and so no headers where it is not set.
A module names the data use it needs by an IAB Tech Lab Privacy Taxonomy Data
Use (`Permission` in `crates/trusted-server-core/src/permissions.rs`), and
none of them names device detection, client hints or a call that describes the
browser. The nearest are general ones, which the sample policy
(`config/permissions/sample.yaml`) treats in opposite ways.
`functional.performance` and `necessary.operations.website_use` are granted in
each of its three groups, so a check on either would never refuse there,
`advertising_marketing.first_party.contextual` needs a signal in `gdpr-eu` and
`gdpr-uk`, and the umbrella `functional` is denied in all three, so the call
would never be made. Choosing one is therefore a policy decision rather than a
reading of the taxonomy, which is what `docs/guide/permission-model.md` asks a
declaration to be. The nearest precedent in the stack declares none, because
the built-in and host device modules require no permission and a device module
a module supplies is refused at startup if it declares one
(`crates/trusted-server-core/src/integrations/registry.rs`). A request filter
has no `required_permissions` of its own (`IntegrationRequestFilter` in the
same file), so this one would read `RequestFilterInput::permissions` itself,
which no shipped filter does yet.

The second is how long an answer is held. The hour was chosen because the
visitor's address was in the evidence and an address can be reassigned. The
evidence now carries no address, so the hour only bounds how long an answer is
kept before the service is asked again, and whether it should be longer is
open.

**When it fails.** A service that cannot be reached, answers a status that is
not a success, answers a list of errors, answers something that is not JSON,
or answers more than 1 MiB leaves no names. The page is then served as it would
be without the middleware, a warning is logged, and the failure is not held, so
the next navigation with the same evidence asks again. An answer that names no
hints is held like any other, for up to an hour. On Fastly a call that cannot
connect fails after one second, and one that connects waits `timeout_ms` for
the first byte and for each gap between bytes, so while the service is down
every page navigation an entry covers with no held answer waits for those
limits before its page is built. On Axum, Cloudflare and Spin only the
adapter's HTTP client bounds the call. On Axum a service that cannot be
reached fails within the client's 5 second connection limit, a refused
connection fails at once, and a service that accepts the connection and then
stalls holds the call for up to 30 seconds in all. On Cloudflare and Spin the
runtime's own limit bounds it. So while the service is slow or cannot be
reached, each such navigation waits up to that bound.

**Rollout.** A deployment without `[shared.51degrees]` is unchanged. On every
adapter, adding the table and an entry naming the middleware changes the
pages that entry covers, served to a browser the service
names hints for. The filter leaves state on the request, so core keeps the
request to the origin
path (`IntegrationRequestState` in
`crates/trusted-server-core/src/integrations/registry.rs`). The page is then
never read from a stored template or stored as one, so each reader's copy is
built from the response of the source `[source]` names
(`2026-10-14-source-modules-design.md`), and an HTML response with a body is
sent `private, no-store`. The fetch from the source goes past the host's cache
only where `[creative_opportunities] origin_readthrough_enabled` is on or the
ad stack runs for the request. A browser the service names no hints for leaves
no state and gets its pages as before, although the service is still asked (the
tests script that answer for a Gecko and a WebKit browser). `critical` makes the
browser repeat its first navigation to carry the hints, so that page is built
twice. The license key has to be in the secret store before the deploy that
carries the table, because the settings look it up as they load. An entry's
`path` limits where the service is asked as well as where the headers and the
tag are written, so the effect on pages reaches only the paths an entry naming
the middleware covers.

## 3. What is not in this change

- A 51Degrees device module. The names the filter leaves are there for one to
  read, and the shared crate's pipeline takes the properties it registers,
  and nothing reads them yet.
- Any change to core. The middleware uses the serve phase, the headers step,
  the request state and the middleware and shared tables the stack already
  has.
- **`timeout_ms` on Axum, Cloudflare and Spin.** Those adapters' HTTP clients
  apply no module's time limit, and
  `2026-10-17-request-filters-on-every-adapter-design.md` says why that is
  left to a change of its own.
- **Asking permission first, or holding answers longer.** The two open
  questions under "Open questions for the reviewer" decide those.

## 4. Tests

The middleware crate's tests cover the three headers on a document, `critical`
off, a document with no names, what the response already carried being kept,
every line of it when a response sends each header twice, the tag's shape and
the origin it delegates to, a name that is not a token reaching neither, the
request filter against a scripted service for an asset request, for a table
with no entry and for a path an entry's `path` does not cover, none of which
asks the service, for a path that entry covers and for a Chromium, a Gecko and
a WebKit browser, that the filter sends neither the reader's address nor a
cookie, the middleware's own table refusing a key it does not know, a table
with no license key being refused, that no entry means the headers step asks
no middleware, and that one shared table runs the middleware while the stock
loading cache holds its answers. The shared crate's tests cover the table and
its defaults and its refusal of a key it does not know, the license key's
refusals, the call's form and credential, that the evidence is the
`User-Agent` and the `Sec-CH-UA` hints in name order, that no address and no
cookie reaches the request the client builds, that two readers on the same
browser at different addresses make one call, what it holds across instances
and stores, concurrent requests making one call, the refusals it reports, and
the hint list read out of an answer. The Axum adapter's tests plant an unknown
key in the shared table for
the builder that table runs, check that the example template's `[[serve]]` entry
and the guide's table of middleware name `51degrees.client_hints`, and check
that both crates name their maintainers. Each adapter's list test holds the new
row and the hook order.

Six things have no test.

1. `crates/middleware/51degrees/src/fixtures/page-change.settings.toml` is
   read by no test, and unlike the other modules' it has no input page or
   recorded page beside it.
2. Nothing runs the filter through the registry and the publisher path, so no
   test shows that a page the filter leaves names for skips the stored
   template or is sent `private, no-store`. The tests seed the document state
   by hand where core would copy it from the request's state.
3. Nothing shows that `shared.51degrees.license_key` is among the secret
   settings a push treats as naming a key, because the Axum adapter's test
   lists DataDome's two secrets and no others.
4. No test uncomments the example template's `[shared.51degrees]` and
   `[middleware.51degrees.client_hints]` tables to check that they load, as the
   test for the Permutive, lockr and Sourcepoint tables does.
5. No adapter test runs the filter, and the filter's tests pass
   `permissions: None`, so nothing shows what the filter does with permission
   state.
6. The two crates' tests run natively under `cargo test-axum`, and their lints
   under `cargo clippy-axum`. No alias runs their tests for an adapter's
   WebAssembly target, being `wasm32-wasip1` for Fastly and Spin and
   `wasm32-unknown-unknown` for Cloudflare, because the Fastly aliases do not
   list them and `cargo test-cloudflare` and `cargo test-spin` test the
   adapter crate alone, on the host.

## Revision record

| Date       | Change                                                                                                                                                                                                                                                                                                                                                                                |
| ---------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 2026-10-14 | First draft.                                                                                                                                                                                                                                                                                                                                                                          |
| 2026-10-15 | Fixes from review round 1. What the filter runs on and that only the Fastly adapter runs it, the cost to pages, what each navigation sends and the open questions on consent, how each adapter bounds the call, the answers on failure, the settings, what the headers keep, the definition of a page navigation, the untested paths, the related specs and the review log.           |
| 2026-10-15 | Fixes from review round 2. The pull request that holds the design documents, the wait for the first call in the guide, and the code comments left for a later change.                                                                                                                                                                                                                 |
| 2026-10-15 | Code fixes for the call's scope, what it sends and header merging. The permission question stays open, with what was found, and the answer lifetime is a new open question.                                                                                                                                                                                                           |
| 2026-10-18 | Every adapter runs the filter, from `2026-10-17-request-filters-on-every-adapter-design.md` and its first review round. The table of parts, the builder, the `timeout_ms` setting, what bounds the call on each adapter, consent, failure, rollout, section 3 and the tests say so. Fixes from review round 3, the wait on Axum for a service that cannot be reached and test item 6. |
| 2026-10-26 | The service's answer is held only where a deployment links a loading cache module, and a stock build, which links none, calls the service on every page navigation an entry covers (`2026-10-12-loading-cache-design.md`, section 7). The table of parts and the declared cache say so.                                                                                               |

## Review log

1. **Round 1, 2026-10-15.** A review in a fresh session, against `split/44-client-hints` at `1f6b83d0c`. Issues found, four of them blocking. The four were that the request filter runs wherever the table is present and not only where an entry covers the page, that the cost to existing pages was not stated, that failure handling and the timeout of each adapter were not stated, and that nothing said what each navigation sends to the service or what consent it needs. This revision states each from the code, which also shows that only the Fastly adapter runs request filters, and it corrects the claims about page navigations, the headers and the tests, replaces a related spec that exists in no branch, and corrects the guide, the changelog and the middleware readme.
2. **Round 2, 2026-10-15.** A lighter review in a fresh session, against this page at `126347b00`, of the round 1 fixes and the new text. No blocking issues once the pull request number was corrected. This revision names the pull request that now holds every design document, makes the guide's description of the wait for the first call match the one second connection limit and the limit on each gap between bytes, and lists in section 3 the code comments that still say more than the design does.
3. **Round 3, 2026-10-18.** A review in a fresh session, against `split/52-request-filters` at `17136d04b`, of the change that has every adapter run the filter, made with the request filters design's first round. Issues found, none blocking. The two were that When it fails gave 30 seconds on Axum as the wait for a service that cannot be reached, which fails within the client's 5 second connection limit, and that test item 6 named only the Fastly adapter's target, when no alias runs the two crates' tests for the WebAssembly target of any adapter, all of which now run the filter. Fixed in `e7417c898`.
