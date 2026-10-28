# 51Degrees Integration

**Category**: Device detection

**Status**: Development

**Type**: Client hints middleware, with the table and the service call the 51Degrees modules share

## Overview

A browser sends its high entropy client hints only when the site asks for
them, with `Accept-CH` on the response and `Critical-CH` when the first
navigation should be retried carrying them, and they reach a third party only
when the top level page delegates them, by a `Delegate-CH` tag or a
`Permissions-Policy` header. A `Delegate-CH` tag added by JavaScript is
ignored, so the tag has to be in the page as served.

The `51degrees.client_hints` middleware writes all of that, on each page a
`[[serve]]` entry names it for, from the list the 51Degrees service gives for
the reader's browser. No hint name is written down in the code. As it filters
a page request, the middleware asks the service which hints it wants from
that browser, through the cache the 51Degrees modules share, and leaves the
names in the request's state. On the way out it writes:

1. `Accept-CH` naming the hints, and `Critical-CH` naming the same unless
   `critical` is off, so the browser retries the first navigation of a
   session carrying them.
2. `Permissions-Policy` delegating each hint to the service's origin, as
   `ch-ua-model=(self "https://cloud.51degrees.com")`, which keeps the hint
   for the page's own origin as well.
3. The `Delegate-CH` tag in the head, in the shape 51Degrees publishes, being
   the lower case hint names each followed by the origin and separated by
   semicolons.

Each header keeps every line the response already carries under that name,
with the hints added after them, so a policy the publisher's origin sets is
kept and a hint the origin already asks for is named once. A delegation comes
after the origin's own policy members, so for a hint the service names it
overrides an origin policy that denies the hint, such as `ch-ua-model=()`. A
browser engine the service names no hints for, such as Firefox or Safari, is
given none of them. A request that is not a page navigation asks the service
nothing, and a page navigation is a `GET` or `HEAD` with
`Sec-Fetch-Dest: document` or an `Accept` naming `text/html`.

A browser keeps the hints an origin asked for until a response from that
origin names a different set, so `Accept-CH` and `Critical-CH` are left off
the response to a request that already carries every hint the service names.
They are still written where the response names an `Accept-CH` set of its
own, because that set would replace the one the browser holds.
`Permissions-Policy` and the `Delegate-CH` tag apply to their own page only,
so every page gets them.

The origin the hints are delegated to is the origin of the configured
`endpoint`, so a deployment using a private cloud delegates to that, and an
endpoint on the page's own origin delegates nothing.

The hint names stay in the request's state under the 51Degrees registration,
as `ServiceHints` from `trusted-server-shared-51degrees`, for a 51Degrees
module that wants to know what the browser was asked to send.

## Configuration

```toml
[shared.51degrees]
endpoint = "https://cloud.51degrees.com/api/v4/json"
license_key = "fiftyone_degrees_license_key"
# timeout_ms = 500

# [middleware.51degrees.client_hints]
# critical = true

# [cache.51degrees-answers]
# Refused unless the build links crates/loading-cache/51degrees.
# module = "51degrees"

[[serve]]
media_type = "text/html"
middleware = ["51degrees.client_hints"]
```

The table's presence runs the 51Degrees modules, so there is no `module`
line to write. Every one of them presents the license key on its server side
calls, and never the public resource key, so the table needs one.

| Field         | Type    | Default  | Contract                                                                                                                                  |
| ------------- | ------- | -------- | ----------------------------------------------------------------------------------------------------------------------------------------- |
| `endpoint`    | URL     | Required | The service's JSON endpoint, with any resource key the deployment requires in its path, whose origin the browser's hints are delegated to |
| `license_key` | String  | Required | The name of the license key's entry in the secret store                                                                                   |
| `timeout_ms`  | Integer | `500`    | How long a call to the service waits on the Fastly adapter, `1..=10000` milliseconds                                                      |

The middleware's own table, `[middleware.51degrees.client_hints]`, has one
field, `critical`, on by default. Each table refuses a field it does not
read.

The service's answers load through the cache `51degrees-answers`, and
`[cache.51degrees-answers]` names a module and sizes the copies, as for every
declared cache. A deployment that links one loading cache module holds the
answers there with no line in the document, each for an hour at most. A stock
build links none, so it asks the service on every page navigation the
middleware is placed for.

## Placing the middleware

The middleware writes its headers and tag where a `[[serve]]` entry names
`51degrees.client_hints`, and in no other phase, because the list is the
service's answer for one reader's browser and what the fetch phase leaves is
stored for every reader. The request filter asks the service only about a page
navigation whose path an entry naming the middleware covers, by the same rule
as the headers, so a deployment that names it in no entry sends no client
hints headers and no tag and asks the service nothing. The headers are written
on every response the publisher path sends for a path the entry covers, a
redirect included, and the tag on each page.

A page the middleware leaves names for keeps to the origin path, because the
names are left in the request's state, which is what core does for every
request a module leaves a value on. That page is never read from a shared
template and never stored as one, and is sent `private, no-store`.

The call sends the browser's `User-Agent` and its `Sec-CH-UA` hints, and
neither the reader's address nor a cookie. A call costs one round trip to the
service before the page. Where a loading cache module holds the answers, only
the first call for a `User-Agent` and set of those hints is made, and every
reader whose browser sends the same values then reads the held answer. On
Fastly the call fails after one second if it cannot connect, and
once connected it waits `timeout_ms` for the first byte and again for each gap
between bytes. On Axum, Cloudflare and Spin the adapter's HTTP client applies
no `timeout_ms`, so a call waits up to 30 seconds on Axum and for the runtime's
own limit on Cloudflare and Spin. A service that does not answer leaves the
page with no headers and no tag, and a logged line says why.

## What is checked

The table is refused, at load and by `ts config validate`, when it carries a
field it does not read, when `endpoint` is not a URL, when `timeout_ms` is
out of range, and when it has no `license_key` or an empty one. The
middleware's table is refused when it carries a field other than `critical`.
A `[cache.<name>]` table naming a cache no module declares is refused with
the names that are declared.

## Tests

The module's own tests cover the headers on a document, `critical` off, a
document with no names, every line the response already carried, the tag's
shape and the origin it delegates to, the request filter against a scripted
service for a Chromium, a Gecko and a WebKit browser, an entry's `path`
deciding whether the service is asked, each table refusing a key it does not
know, and that no entry means no headers and no call. The shared crate's tests
cover the call's form, the credential it presents, that no address and no
cookie is sent, what it holds and what it refuses.
