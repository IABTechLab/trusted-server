# Design Spec: A deployment using 51Degrees gets client hints from one middleware

**Status:** Proposed, 2026-10-14. Implemented on a working branch of the fork, with no pull request raised yet.
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** `2026-10-13-middleware-settings-design.md`, `2026-10-14-source-modules-design.md`
**Related PRs:** none yet
**Issues:** IABTechLab/trusted-server#1333
**Last updated:** 2026-10-14

> **Why this spec exists.** A browser sends its high entropy client hints only when the site asks for them and delegates them, and nothing in the stack writes the headers and the tag that do so for a deployment whose device detection is 51Degrees, so every first page view of a session is priced on the User-Agent alone.

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

## 2. Design

**A serve middleware, `51degrees.client_hints`, in `crates/middleware/51degrees`.**
It runs where a `[[serve]]` entry names it. As it filters a page request it
asks the service, through the cache the 51Degrees modules share, which hints
it wants from the request's browser, and leaves the names in the request's
state. Core copies them into the document state the serve middleware share.
From that list the middleware writes `Accept-CH`, `Critical-CH` and
`Permissions-Policy` through `Middleware::response_headers`, each keeping
what the response already said under that name, and the `Delegate-CH` tag in
the head through its action. It reads `critical` from its own table,
`[middleware.51degrees.client_hints]`, on by default.

A request that is not a page navigation asks the service nothing. A service
that names no hints for a browser, or does not answer, leaves nothing, and
the page gets no headers and no tag. The origin delegated to is the origin of
the configured endpoint, and an endpoint on the page's own origin delegates
nothing.

**What the 51Degrees modules share, in `crates/shared/51degrees`.** The
vendor's table, `[shared.51degrees]`, being `endpoint`, `license_key` and
`timeout_ms`, whose presence runs the 51Degrees builder. The license key
every server side call presents, and the refusal when the table has none.
The one call to the service for a request's evidence, a form POST to the
service's `json` endpoint over the platform's HTTP client, naming the
properties each module registered so one call carries them all, and its
answer held in the loading cache so a question many requests ask at once is
asked once. The evidence a request gives, being the address, the `User-Agent`
and what the browser gathered into its `51D_` cookies. The names the service
asked for, as the type a module reads them from the request's state by.

**The cache is a declared one.** The builder declares `51degrees-answers`,
and a deployment selects the loading cache that holds it and sizes its copies
in `[cache.51degrees-answers]`, as every declared cache is selected. A stock
build holds the answers with its one loading cache module without a line in
the document. An answer is held for an hour at most, because the address in
its evidence can be reassigned.

**The builder is run by the table, not selected by a section.** It carries
the vendor's shared table, declares the license key as a secret setting in
use whenever the table is present, and registers the filter and the
middleware together. The stock list ships it, so every adapter and the `ts`
command line tool take it.

```toml
[shared.51degrees]
endpoint = "https://cloud.51degrees.com/api/v4/json"
license_key = "fiftyone_degrees_license_key"

[[serve]]
media_type = "text/html"
middleware = ["51degrees.client_hints"]
```

## 3. What is not in this change

- A 51Degrees device module. The names the filter leaves are there for one to
  read, and the shared crate's pipeline takes the properties it registers,
  and nothing reads them yet.
- Any change to core. The middleware uses the serve phase, the headers step,
  the request state and the middleware and shared tables the stack already
  has.

## 4. Tests

The middleware crate's tests cover the three headers on a document, `critical`
off, a document with no names, what the response already carried being kept,
the tag's shape and the origin it delegates to, a name that is not a token
reaching neither, the request filter against a scripted service for an asset
request and for a Chromium, a Gecko and a WebKit browser, each table refusing
a key it does not know, a table with no license key being refused, that no
entry means the headers step asks no middleware, and that one shared table
runs the middleware while the stock loading cache holds its answers. The
shared crate's tests cover the table and its defaults, the license key's
refusals, the call's form and credential, what it holds across instances and
stores, concurrent requests making one call, the refusals it reports, and the
hint list read out of an answer. The stock list's tests hold the new row,
the fixture settings and the hook order.
