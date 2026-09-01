# Mobile Ad-Rendering Trace Endpoint Design

**Status:** Proposed

**Issue:** [#1050 — Create debug endpoint for mobile user to trace ad rendering](https://github.com/IABTechLab/trusted-server/issues/1050)

**Related work:**

- [#1081 — Improvements to TS_CONSOLE for ad observability](https://github.com/IABTechLab/trusted-server/issues/1081)
- [#1074 — Request-phase timing](https://github.com/IABTechLab/trusted-server/pull/1074)
- [#1076 — Auction timing milestones](https://github.com/IABTechLab/trusted-server/pull/1076)
- [#974 — GPT runtime diagnostics](https://github.com/IABTechLab/trusted-server/pull/974)
- [#997 — Trusted Server delivery attribution](https://github.com/IABTechLab/trusted-server/pull/997)

## 1. Summary

Add a deployment-controlled, public, privacy-safe `GET /_ts/trace` page for a
mobile end user who needs to reproduce an ad-rendering problem and give support
an exportable diagnostic report. Visiting the page is read-only. The user
intentionally enables or ends tracing with a same-origin POST action.

The endpoint is both a setup page and a report viewer. On the first visit it
offers a large `Enable tracing` action and explains how to reproduce the
problem. The user then returns to the real publisher page and reloads it.
Trusted Server supplies redacted request context, while the existing TS Console
records GPT, auction, and render evidence. A `View trace results` action creates
one bounded, allowlisted snapshot in same-tab `sessionStorage` and navigates to
`/_ts/trace`. The endpoint reads that untrusted snapshot, validates it, presents
a mobile-first HTML report, and offers JSON export, copy, and progressive Web
Share actions.

The design introduces no report database, server-side trace store, report ID,
target URL parameter, telemetry query, or publisher-origin change. It cannot
recover an event that happened before tracing was enabled; the user must
reproduce the problem.

## 2. Problem and product interpretation

Issue #1050 names three required data groups:

1. Network information inspired by `fastly-debug.com`.
2. Auction information per ad slot, similar to the TS tracer and auction
   telemetry.
3. End-user cookie information.

The title additionally establishes two product constraints: the experience is
for a mobile user, and it is reached through an endpoint. A mobile user should
not need browser developer tools, Basic Authentication, a copied trace ID, or a
second copy of the affected page URL.

A standalone request cannot know what occurred in a previous document. Exact
render evidence exists only while the publisher page is running in the browser.
The design therefore separates two responsibilities without separating the user
experience:

- `/_ts/trace` owns setup, consolidated presentation, and export.
- TS Console owns observation of the real publisher page.

The browser-local handoff joins them without introducing a backend report
service.

## 3. Goals

- Give a non-technical mobile user one memorable URL:
  `https://<publisher-host>/_ts/trace`.
- Capture evidence from a real publisher-page reproduction, not a synthetic
  auction.
- Display a Fastly-inspired network summary for the traced publisher request.
- Report health for an explicit allowlist of Trusted Server cookies without
  exposing their values.
- Present the versioned, allowlisted TS Console evidence for every retained GPT
  slot and request cycle that fits the public report bounds, with explicit
  omission counts when deterministic size truncation is required.
- Support a full report in a narrow mobile viewport without developer tools.
- Export the same allowlisted model as formatted JSON.
- Keep the supported capture journey bounded, same-tab, temporary, and inactive
  by default without treating browser storage as a security boundary.
- Preserve normal auction, GPT, rendering, origin, and caching behavior whenever
  diagnostics is inactive.
- Keep core behavior platform-neutral while allowing Fastly to provide richer
  optional request fields.

## 4. Non-goals

- Recovering a failure that occurred before tracing was enabled.
- Permanent history, cross-device retrieval, server upload, or support-ticket
  integration.
- A database, distributed trace store, report token, or telemetry lookup.
- A target URL such as `/_ts/trace?target=/article`.
- Replaying an auction or treating a synthetic auction as evidence about a
  publisher page.
- Exact parity with every field or active measurement on `fastly-debug.com`.
- Reading the browser's complete cookie jar, third-party cookies, cookie
  attributes, or cookies withheld from the request.
- Exposing raw cookie values, EC IDs, EIDs, consent strings, unmasked IP
  addresses, internal auction request IDs, creative markup, targeting maps,
  cache URLs, or stack traces.
- Reimplementing TS Console auction and creative observability requested by
  #1081.
- Querying Tinybird to build an interactive report.
- Adding exact provider-by-slot no-bid explanations before the auction model can
  observe those dispositions.
- Direct `POST /auction` browser diagnostics in the first release.

## 5. Decisions

### 5.1 Public, redacted endpoint with intentional activation

`/_ts/trace` is public when explicitly enabled by deployment configuration. It
is not placed under `/_ts/admin`, because the intended user is a layperson on a
phone and the existing Basic Authentication flow is unsuitable for that
journey.

Public access is safe only because both the page and export use a strict
allowlist. The activation cookie is a feature toggle, not authentication. No
field becomes eligible merely because tracing is active.

`GET /_ts/trace` is read-only and never activates or ends tracing. Activation
and deactivation use an in-page same-origin `fetch` POST accepted only when its
fixed custom action header, `Origin`, and Fetch Metadata identify the publisher
origin. Requests with a conflicting or missing signal fail closed. The POST
updates the existing page rather than adding a history entry, so browser Back
can still reach the article. This prevents an unrelated site from silently
toggling diagnostics through a top-level GET while preserving a one-URL,
one-tap mobile workflow. This control does not defend against code already
executing on the publisher origin.

### 5.2 Reuse the existing diagnostics session

The endpoint reuses `__Host-ts-console` and the existing GPT diagnostics
activation semantics rather than creating a second `ts-trace` session. The
cookie remains host-only, `Secure`, `HttpOnly`, and `SameSite=Lax`.
`POST /_ts/trace/enable` sets it with a fixed 30-minute `Max-Age` and does not
refresh that lifetime on publisher requests; `POST /_ts/trace/end` clears it.
Neither action accepts state-changing query parameters. The shorter endpoint
lifetime bounds accidental private/no-store operation if a user forgets to end
tracing; the existing technical query flow keeps its existing session-cookie
semantics.

### 5.3 Browser-local, explicit handoff

TS Console remains memory-only during observation. It writes a report to
`sessionStorage` only after the user selects `View trace results`. The action:

1. Builds the same versioned allowlisted snapshot used by export.
2. Adds the redacted request-context envelope.
3. Serializes and validates the size.
4. Stores it under one versioned key in the current tab.
5. Navigates the same tab to `/_ts/trace`.

Continuous persistence is prohibited. Same-tab navigation is the supported
handoff, not an isolation guarantee: a browser may copy session storage into an
opener-created tab or preserve it during session restore. Same-origin scripts
and service workers can read, replace, or forge the snapshot. The viewer
therefore treats it as untrusted, applies an application-level expiry, and
labels it browser-observed rather than authoritative.

### 5.4 Forward reproduction, not historical diagnosis

The user's explicit activation enables tracing for subsequent eligible document
navigations. The setup page must say plainly that the user needs to return to
the affected page, reload it, and reproduce the problem.

If the user replaced the affected URL in the address bar with `/_ts/trace`, the
page offers a `Return to previous page` action backed by browser history and
then instructs the user to reload once. History is only a convenience: it may
lead to a messaging app, search page, or unrelated site. The page includes a
fallback instruction to reopen the affected article on the same hostname and
in the same tab. The design does not claim that back-forward-cache restoration
caused a new server request.

Support should preferably give the user the trace URL before reproduction. The
product does not attempt to discover the previous URL through `Referer`, because
address-bar navigation commonly omits it and relying on it would create
inconsistent behavior.

### 5.5 Separate issue ownership

#1050 defines the report shell, request context, mobile flow, browser-local
handoff, and export. #1081 remains the owner of creative numbering, auction
classification, bidder/price policy, terminology, and normalized auction/render
timing.

Version one consumes `GptDiagnosticsExportV1` through TS Console's public export
contract and projects it into a distinct redacted `TraceGptDiagnosticsV1`. It
does not read TS Console internals or create an alternate slot correlation
engine. #1081 and #1074/#1076 are additive follow-up work and are not release
gates for this version.

## 6. User experience

### 6.1 First visit: no captured report

`GET /_ts/trace` returns a mobile-first HTML page with:

- Title: `Trusted Server ad diagnostics`.
- State derived from the setup request: `Tracing is off` unless the server
  observed a valid existing diagnostics cookie, including one activated through
  the technical query flow.
- A short explanation that no previous ad failure can be recovered.
- Network and cookie health for the setup request, labeled `Setup request`.
- Primary action: `Enable tracing`, implemented as an in-page same-origin fetch
  POST that does not add a history entry.
- After activation and a successful state-verification request, state: `Tracing
is on — cookie observed by server`.
- After activation, primary action: `Return to previous page` when browser
  history permits.
- Secondary instructions: return to the affected page, reload once, reproduce
  the problem, then select `View trace results`.
- A recovery instruction to reopen the affected article on the exact same
  hostname and in the same tab if browser history is not useful.
- Action to end tracing when it is active.

The page must not imply that setup-request network facts or an empty auction
section describe the affected page.

### 6.2 Active publisher page

The existing TS Console remains available. On mobile it gains a prominent
`View trace results` action. Selecting it never changes ad behavior; it only
snapshots retained observations and navigates after serialization succeeds.

If a valid bounded snapshot is built but browser storage rejects it, the page
remains in place, announces the storage failure, and offers a direct download
of that same combined `TraceReportV1` envelope. If projection, validation, or
size bounding fails before a valid report exists, the page reports capture
failure and does not mislabel the existing GPT-only export as an equivalent
fallback.

### 6.3 Report visit

When a valid snapshot exists, `/_ts/trace` renders:

1. Report summary and capture time.
2. Network and request section for the traced publisher document.
3. Trusted Server cookie-health section.
4. Auction and rendering section grouped by numbered slot.
5. Coverage and ambiguity section.
6. Export actions.
7. `Clear report and end tracing` action.

The setup request's facts are not merged into or substituted for missing traced
page facts. Missing fields display `Unavailable`; missing evidence displays
`Not observed` or `Unknown`, following TS Console terminology.

The report begins with `Browser-observed, unverified diagnostic data`. It does
not claim that the snapshot is authentic or suitable as forensic or security
evidence.

`Copy` copies formatted JSON. `Share` supplies the same JSON file to the native
Web Share sheet only after an explicit tap and tells the user that the selected
app will receive it. If file sharing is unsupported or rejected, the viewer
keeps Copy and Download available; it does not silently share a URL or upload
the report.

### 6.4 Mobile and accessibility requirements

- Support viewport widths down to 320 CSS pixels without horizontal page
  scrolling.
- Use a full-document report rather than the current floating 460-pixel panel.
- Use at least 44-by-44 CSS pixel primary touch targets.
- Keep export/end-trace actions reachable without covering report content.
- Use semantic headings, lists, tables only where they remain readable on a
  narrow viewport, visible focus styles, and an `aria-live` status region.
- Do not rely on hover, color alone, badges alone, or precise pointer input.
- Preserve browser zoom and safe-area insets.
- Prefer native text and controls over a framework or new UI dependency.

## 7. Architecture

```text
GET /_ts/trace
    |
    |-- early reserved-route classifier terminates locally
    |-- HTML explains forward reproduction; no state mutation
    v
POST /_ts/trace/enable after explicit user action
    |
    |-- validates same-origin request signals
    |-- response sets __Host-ts-console
    |-- client requests /_ts/trace/state
    |-- server reports whether the new request carried a valid cookie
    v
Real publisher document reload
    |
    |-- adapter supplies optional network facts
    |-- core computes allowlisted cookie health
    |-- core injects redacted TraceRequestContextV1
    |-- existing TS Console observes GPT and TS delivery
    v
User selects "View trace results"
    |
    |-- JS builds bounded TraceReportV1
    |-- same-tab sessionStorage write
    |-- location.assign('/_ts/trace')
    v
Report GET /_ts/trace
    |
    |-- static report shell reads and validates TraceReportV1
    |-- mobile HTML renders sections
    |-- local JSON/copy/share actions
```

### 7.1 Core responsibilities

- Define configuration and route behavior.
- Provide a shared exact-path reserved-route classifier that runs before event
  context, filters, auctions, named routes, or publisher fallback.
- Define the platform-neutral request-context and report-envelope schemas.
- Build cookie-health facts through read-only parsing.
- Convert `ClientInfo` and available geo data into the public network allowlist.
- Inject request context only into an active private diagnostics document.
- Apply response privacy and security headers.
- Ensure trace requests never reach the publisher origin.

### 7.2 Adapter responsibilities

- Invoke the reserved-route classifier at the earliest adapter dispatch point
  with exact path and method handling.
- Populate optional `ClientInfo` fields available on the platform.
- Fastly may supply bounded POP, HTTP version, TLS, and edge-server data when
  the SDK exposes them. JA4 and H2 fingerprints are excluded from version one.
- Other adapters return the same schema with unsupported fields absent.
- Adapter-specific errors omit optional facts rather than failing publisher
  delivery.

### 7.3 JavaScript responsibilities

- Accept the immutable redacted request context at initialization.
- Preserve the existing bounded TS Console observation store.
- Build and validate `TraceReportV1` with a redacted
  `TraceGptDiagnosticsV1` projection on explicit user action.
- Store only one supported report for the same-tab workflow in
  `sessionStorage`, while treating its contents as untrusted.
- Render the report shell from the validated model.
- Implement equivalent combined-report download, formatted-JSON copy,
  progressive JSON-file Web Share, clearing, expiry, and accessible status
  reporting.
- Never upload diagnostic data or issue a telemetry query.

## 8. Route and configuration contract

Add an explicit default-off option to the existing integration:

```toml
[integrations.gpt_diagnostics]
enabled = true
trace_page_enabled = false
```

Rules:

- `trace_page_enabled = true` requires `enabled = true`; invalid combinations
  fail configuration validation.
- Operator documentation beside this option states that the public page makes
  the allowlisted presence/validity of four HttpOnly Trusted Server cookies
  visible to same-origin JavaScript whenever the feature is enabled. It also
  states that masked IP prefixes and coarse geo remain potentially personal or
  pseudonymous network data. Enabling the option is the deployment's explicit
  acceptance of those bounded disclosures.
- `GET /_ts/trace` returns the setup/report shell without changing cookies or
  browser storage at the HTTP layer. After load, the explicitly included viewer
  script may remove a rejected or expired local entry. Unrelated query
  parameters do not activate or deactivate tracing and are not reflected into
  the page or export.
- `HEAD /_ts/trace` returns the GET status and headers without a body or state
  mutation.
- `GET /_ts/trace/state` returns private/no-store JSON containing only
  `observed_active: true|false`, determined from whether that request carried
  exactly one valid diagnostics cookie. `HEAD` returns the same status and
  headers without a body. Other methods return local 405 responses.
- `GET` and `HEAD` on exactly `/_ts/trace/assets/v1.js` and
  `/_ts/trace/assets/v1.css` return fixed versioned assets. They contain no
  request or report data and may use immutable public caching. Other methods
  return local 405 responses. These v1 URLs are immutable byte contracts: any
  JS or CSS byte change requires a new asset-set URL such as `v2.js`/`v2.css`
  and an updated shell reference; a release never replaces bytes at a published
  immutable URL.
- `POST /_ts/trace/enable` accepts no query parameters, validates an empty body,
  the exact `X-TS-Trace-Action: enable` header, and same-origin request signals;
  sets the diagnostics cookie; and returns a small local JSON result. A success
  response means only that the server requested the cookie change.
- `POST /_ts/trace/end` applies the same validation, clears the diagnostics
  cookie using `X-TS-Trace-Action: end`, and returns a small local JSON result.
  After explicit user confirmation, client JavaScript independently attempts
  local report deletion and the end POST. Neither result gates the other.
- For both POST paths, absent or exactly-zero `Content-Length` is accepted,
  `Transfer-Encoding` is rejected, and the adapter reads at most one byte when
  it must verify an absent length. Any body byte or positive/invalid length
  returns local `413 Payload Too Large` without draining or processing an
  unbounded body. The one-byte read inherits a maximum two-second adapter
  request-body deadline; timeout returns local `408 Request Timeout` with no
  mutation.
- State-changing POSTs require an `Origin` exactly matching the canonical
  request origin and `Sec-Fetch-Site: same-origin`. Missing, conflicting,
  malformed, cross-site, or duplicate control values return local `403` without
  mutation. This deliberately targets current supported mobile browsers rather
  than weakening the check for legacy clients.
- The canonical request origin comes from adapter-owned inbound URL/scheme and
  validated authority data, never an arbitrary forwarded header. Both it and
  the single parsed `Origin` header are serialized with lowercase host and
  default ports removed before exact comparison. Invalid or multi-valued host,
  authority, scheme, or origin input fails closed.
- Unsupported methods on a shell or state-changing path return a local 405
  Method Not Allowed response with the path-specific `Allow` header.
- Disabled deployments return a local `404` for the complete trace route set,
  including assets, and never fall through to the publisher origin.
- The `/_ts/trace` namespace is reserved. A trailing slash, extra path segment,
  unsupported asset name, repeated separator, or lookalike beneath that
  namespace returns a local `404`; an encoded separator or ambiguous dot
  segment returns a local `400`. None falls through to the publisher origin.
  The adapter classifies from its canonical parsed path while retaining enough
  raw-path information to reject ambiguous encodings consistently.

Every adapter implements the following order:

1. Parse the method, canonical host/origin, path, query, and bounded headers
   required for route safety.
2. Classify an exact Trusted Server reserved path.
3. For a trace path, terminate locally after only trace-specific validation and
   bounded request-context inspection, including an optional read-only platform
   geo lookup used solely for the displayed setup request.
4. For all other paths, continue through the adapter's ordinary event context,
   authentication, request filters, geo enrichment, EC/EID processing, named
   routes, auction handling, telemetry, and publisher fallback.

Consequently a trace route never creates or finalizes an ordinary event
context, invokes publisher-configured filters, creates or refreshes an EC,
ingests EIDs, runs an auction, fetches the publisher origin, or emits auction
telemetry. Tests must verify ordering in Fastly, Axum, Cloudflare, and Spin;
ordinary named-route registration alone does not satisfy this contract.

After an enable or end POST succeeds, the client performs a no-store state GET.
It claims `Tracing is on — cookie observed by server` only when that separate
request reports active, and `Tracing is off — cookie absent on server request`
only when it reports inactive. A mismatch or failed verification is
`Activation unconfirmed` or `Deactivation unconfirmed` and offers an idempotent
retry. These are server-observation statements, not proof that browser state is
authentic: same-origin service workers can forge or suppress the whole exchange.

Versioned assets may remain in a CDN or browser cache after the feature is
disabled. Cache misses return the configured local 404, but rollback relies on
the uncached shell, state, and action routes being disabled; inert cached assets
alone cannot activate tracing or access a report page.

The current `?ts_console=1` and `?ts_console=0` activation flow remains
supported for technical users. Both activation surfaces drive the same cookie
and runtime; they must not create two concurrent diagnostic modes. That
pre-existing query flow has its existing top-level-navigation activation risk;
#1050 neither expands it to the new trace GET nor claims to remediate it.

## 9. Data contracts

### 9.1 Request context

The server injects one immutable `TraceRequestContextV1` into active diagnostic
documents:

```text
TraceRequestContextV1
  schema_version: 1
  captured_at: RFC 3339 UTC timestamp
  network:
    masked_client_ip?: string
    country?: string
    region?: string
    asn?: u32
    http_version?: string
    tls_protocol?: string
    tls_cipher?: string
    edge_hostname?: string
    edge_region?: string
    edge_pop?: string
  cookies:
    ts_ec: CookieHealth
    ts_eids: CookieHealth
    ts_tester: CookieHealth
    diagnostics_session: CookieHealth
```

The request-context envelope intentionally contains no page URL, path,
referrer, query, or fragment. During the field-by-field trace projection,
`GptDiagnosticsExportV1.page.origin` is retained after validation and its
`pathname` is replaced with the literal `/[redacted]`. The trace viewer accepts
only that literal. Version one therefore does not store or export an exact page
path. Any future route-template policy requires a new schema and privacy review
because paths can contain accounts, emails, preview tokens, and other secrets.

`masked_client_ip` uses a deterministic display-only mask for the current
request: IPv4 keeps at most the first 24 bits and IPv6 keeps at most the first
48 bits. The full address never enters HTML, JavaScript, browser storage, or
export. These prefixes can still be personal or pseudonymous network data; the
report labels them as approximate network identifiers and the operator privacy
decision covers them explicitly.

All platform strings are normalized to printable characters and bounded before
they enter logs, HTML, storage, or export. Country uses at most 2 ASCII
characters; region and POP 32 UTF-8 bytes; HTTP/TLS enumerations 32 bytes; and
edge hostname/region 128 bytes. Values that fail their field contract are
omitted and produce only a bounded error category. JA4 and H2 fingerprints are
not members of `TraceRequestContextV1`.

### 9.2 Cookie health

```text
CookieHealth
  state:
    absent | present_valid | present_invalid | duplicate | unavailable
  source: request
  detail?:
    valid_ec_format | valid_eids_format | valid_tester_value
    | valid_diagnostics_value | malformed | oversized
    | unsupported_value | multiple_values
    | header_too_large | header_not_utf8
```

Details describe shape, never value. `absent` has no detail; `duplicate` uses
`multiple_values`; `unavailable` uses `header_too_large` or `header_not_utf8`;
and a valid state uses its cookie-specific valid detail.

The classifier uses this deterministic contract:

- Inspect all `Cookie` header fields in wire order, up to a combined 16 KiB.
  Exceeding the cap or encountering any non-UTF-8 header makes all four states
  `unavailable`; no partial result is presented as authoritative.
- Split each readable header on semicolons and trim optional ASCII whitespace.
  A valid pair contains a non-empty RFC 6265 token name, one `=`, and the
  remaining bytes as its value; additional `=` bytes belong to the value. Empty
  segments and malformed pairs with an unrelated name are ignored. A segment
  with no `=` counts as one malformed reserved occurrence only when its first
  whitespace-delimited token is exactly a reserved name; a name such as
  `ts-ec-extra` remains unrelated. No malformed unrelated pair poisons a
  reserved-cookie result.
- Count exact, case-sensitive reserved names before passing values to existing
  parsers. Zero occurrences is `absent`; more than one is `duplicate`,
  regardless of whether one value would otherwise be valid. Duplicate
  precedence is therefore diagnostic rather than first- or last-value
  selection.
- Per-value limits are 512 bytes for `ts-ec`, 8 KiB for `ts-eids`, and 16 bytes
  each for `ts-tester` and `__Host-ts-console`. A single value beyond its limit
  is `present_invalid/oversized`; it does not change the other three states.
- One `ts-ec` occurrence is valid only when the canonical EC cookie validator
  accepts its complete value.
- One `ts-eids` occurrence is valid only when the existing bounded Base64/JSON
  EID parser accepts its complete value, including its current 8 KiB value cap.
- One `ts-tester` occurrence is valid only when its value is exactly `true`.
- One `__Host-ts-console` occurrence is valid only when its value is exactly
  `1`.
- A single rejected value is `present_invalid` with exactly one of the public
  details `malformed`, `oversized`, or `unsupported_value`. Parser error text
  and the value itself never enter the report or logs.

The parser inspects the incoming request before diagnostics-cookie sanitation,
while preserving existing authoritative-cookie and consent semantics. It must
scan without using the current lossy `CookieJar` representation, which skips
malformed pairs and cannot preserve duplicate evidence. Inspection is
read-only: it must not generate an EC, touch the identity graph, sync partner
IDs, or extend any cookie lifetime.

Only those four Trusted Server-owned cookie names are reported. Arbitrary
cookie names and values are excluded. The endpoint cannot claim knowledge of
browser attributes, expiry, or cookies the browser withheld from the request.
Because the result reveals presence and validity of HttpOnly cookies to
same-origin JavaScript, enabling this public feature requires an explicit
operator privacy decision documented beside `trace_page_enabled`.

### 9.3 Report envelope

```text
TraceReportV1
  schema_version: 1
  captured_at: RFC 3339 UTC timestamp
  request_context: TraceRequestContextV1
  gpt_diagnostics: TraceGptDiagnosticsV1
  truncation:
    omitted_request_cycles: u16
    omitted_callback_issues: u16
    omitted_attribution_issues: u16
    omitted_nested_values: u16
```

`TraceGptDiagnosticsV1` is a trace-owned projection sourced only from
`GptDiagnosticsExportV1`. It contains:

- `schema_version: 1` and `source_schema_version: 1`;
- the source `capturedAt` value;
- `page.origin` after validation and `page.pathname` fixed to `/[redacted]`;
- field-for-field allowlisted copies of the current v1 slots, requests,
  callback issues, attribution issues, coverage, and metadata, subject to the
  bounds and truncation below.

It is deliberately not named or represented as `GptDiagnosticsExportV1`,
because the fixed pathname and trace-level bounds change the source field
semantics. TS Console continues to own the source schema; the trace envelope
owns its public projection and transport. The initial compatibility matrix is
exactly `TraceReportV1` plus `TraceGptDiagnosticsV1`, sourced from
`GptDiagnosticsExportV1`. The viewer rejects every unknown outer, trace-auction,
or source version with an actionable message rather than guessing. A future TS
Console successor requires an additive source compatibility change and, if the
public projection changes, a new trace-envelope version.

Origin validation requires a parseable HTTP(S) origin whose canonical
serialization exactly equals `window.location.origin`; credentials, paths,
queries, and fragments are rejected. Outer and source capture times require
strict RFC 3339 UTC strings and must be within 60 seconds of `stored_at_ms`.
`TraceRequestContextV1.captured_at` requires strict RFC 3339 UTC but may be older
because it represents the publisher document request rather than snapshot time.

### 9.4 Storage limits and expiry

- Storage key: a namespaced, versioned constant owned by the diagnostics
  module.
- Stored value: `{ stored_at_ms, report }`, where `stored_at_ms` is generated by
  the capture code and is not taken from report content.
- Maximum encoded size: 512 KiB, defined as the byte length of the complete
  compact UTF-8 `{ stored_at_ms, report }` JSON measured with `TextEncoder`
  before storage. Formatted download size and JavaScript UTF-16 string length
  are not used for enforcement.
- Maximum age: 15 minutes from `stored_at_ms`. Non-finite, negative, malformed,
  more than 60 seconds in the future, or older values are rejected. A backward
  wall-clock jump that places the timestamp beyond the tolerated future skew
  also invalidates the entry. Expiry is exposure reduction, not a security
  guarantee.
- One supported report for the current browsing context; a new explicit
  snapshot replaces the old report. Browser opener cloning and session restore
  may copy or retain it.
- The runtime validator accepts only the exact outer and trace-auction v1
  schemas, rejects unknown fields, applies the limits below, and checks compact
  UTF-8 size before rendering.
- Invalid, oversized, expired, unsupported, or hostile reports are removed when
  possible and otherwise ignored. Rendering uses DOM properties and
  `textContent`, never report-derived HTML.
- After confirmation, `Clear report and end tracing` always attempts local
  deletion, the validated end POST, and state verification as independent
  retry-safe steps. Offline or server failure cannot prevent local deletion.
  The UI reports server-observed cookie state and local-report state separately.
  A distinct `Delete local report` action remains available whenever a report
  is displayed, including after an earlier local-deletion failure.

These are product limits, not assumptions about browser quota. A storage write
failure is handled even when the report is below the application limit.

Runtime limits are part of the v1 contract:

| Value                                         | Limit                                                        |
| --------------------------------------------- | ------------------------------------------------------------ |
| Container nesting                             | 8 levels                                                     |
| Slots                                         | 64                                                           |
| Request cycles                                | 10 per slot before total-size truncation                     |
| Callback issues                               | 128                                                          |
| Attribution issues                            | 128                                                          |
| Requested slot sizes                          | 16 per cycle                                                 |
| Ad Manager yield-group or company IDs         | 8 of each per cycle                                          |
| Creative-failure enums                        | 16 per cycle                                                 |
| Origin                                        | 255 UTF-8 bytes                                              |
| GPT pathname in trace projection              | Exact literal `/[redacted]`                                  |
| Slot element ID and ad-unit path              | 512 UTF-8 bytes each                                         |
| Trusted Server auction ID and callback reason | 256 UTF-8 bytes each                                         |
| Any other string                              | 128 UTF-8 bytes                                              |
| Enum                                          | Exact documented value only                                  |
| Identifier, sequence, or counter              | Finite safe integer from 0 through `Number.MAX_SAFE_INTEGER` |
| Browser-relative timestamp or duration        | Finite number from 0 through `Number.MAX_SAFE_INTEGER`       |
| Visibility percentage                         | Finite number from 0 through 100                             |
| Slot dimension                                | Finite integer from 1 through 100,000                        |

Every accepted string must be valid Unicode and must not contain C0/C1 control
characters or bidirectional override/isolate controls. This applies to browser
source fields as well as platform fields and precedes rendering or export.

The snapshot builder creates a new field-by-field projection and rejects an
invalid source value rather than stringifying it. It retains only the first
documented number of requested sizes, yield-group IDs, company IDs, and creative
failure enums, recording discarded entries in `omitted_nested_values`; strings
are never silently shortened. It then measures the complete compact UTF-8
storage wrapper. If it exceeds 512 KiB, it removes the globally oldest request
cycles first while retaining the newest cycle for each slot, then the oldest
callback issues, then the oldest attribution issues, and finally the oldest
remaining request cycles until the report fits. It records every removal in
`truncation`.
A report that still cannot fit after this bounded procedure fails snapshot
creation. The implementation must include a worst-case fixture proving the
result is bounded.

All omission counters use checked addition. If any source collection would
make a counter exceed `u16::MAX`, projection rejects the source instead of
wrapping or saturating the count.

For depth accounting, the `TraceReportV1` object—not its storage wrapper—is
level 1; entering either an object or an array increments the level by one;
primitives do not. No accepted report value may enter a ninth container level.
The storage wrapper is validated separately as the exact two-field object
`{ stored_at_ms, report }`.

For deterministic ordering, a request cycle with no `requestedAtMs` sorts
before a cycle with a timestamp; otherwise cycles sort by `requestedAtMs`, then
`runtimeSlotNumber`, then `requestNumber`. Callback and attribution issues sort
by `timestampMs`, then their original array index. The builder preserves the
relative order of all retained records.

## 10. Auction and rendering evidence

The report uses TS Console's evidence model. It must preserve the distinction
between:

- A Trusted Server opportunity.
- A provider response.
- A selected Trusted Server candidate.
- A GPT request and response.
- A non-empty GPT render.
- Trusted Server creative-bridge evidence.
- Creative load and viewability.
- A publisher or client-side refresh.

The viewer must not infer that Trusted Server rendered an ad merely because GPT
reported a filled slot. Ambiguous and unattributed cycles remain explicit.

Current diagnostics tokens exist only on delivered winning bids. No-bid,
failed, skipped, hidden, unresolved, and direct `/auction` paths can lack server
correlation. The report displays the available observed facts and `Unknown`
rather than manufacturing a correlation.

Provider-call telemetry is auction-wide, while bid rows exist only for returned
bids. Exact `provider X was asked for slot Y` and exact no-bid causality require
a future provider-impression disposition model. That instrumentation is not
silently assumed by this design.

Timing fields introduced by #1074/#1076 are outside the v1 compatibility
matrix. They may be consumed in a later version only after they merge and are
propagated through the public live-diagnostics contract. The report never
queries Tinybird, and it does not combine browser `performance.now()` values
with server-relative timing as though they were one clock.

Bidder and winning price are not added by version one. A later version may
consume them only if #1081 approves them in the public TS Console export
contract. #1050 does not independently weaken the existing privacy policy.

## 11. Network scope

The report is inspired by Fastly Debug, not a clone. Version one exposes only
facts with a defined source and privacy boundary. Every field is optional; an
adapter must omit a value it cannot obtain directly and safely.

Supported categories:

- Masked client address.
- Country, region, and ASN when available.
- HTTP version.
- TLS protocol and cipher.
- Edge hostname, region, and POP.
- Capture time.

Initial provenance and adapter support are:

| Public field                   | Source                                                                                         | Fastly                      | Axum                   | Cloudflare                           | Spin        |
| ------------------------------ | ---------------------------------------------------------------------------------------------- | --------------------------- | ---------------------- | ------------------------------------ | ----------- |
| `masked_client_ip`             | `RuntimeServices.client_info.client_ip`, after trusted-client-IP resolution, then core masking | expected                    | expected               | expected                             | expected    |
| `country`, `region`            | `RuntimeServices.geo.lookup(client_info.client_ip)` projected to `GeoInfo.country/region`      | expected                    | unavailable by default | country expected, region unavailable | unavailable |
| `asn`                          | `GeoInfo.asn`                                                                                  | unavailable until populated | unavailable            | unavailable until populated          | unavailable |
| `http_version`                 | new bounded adapter mapping from inbound protocol metadata                                     | optional                    | expected               | optional                             | optional    |
| `tls_protocol`, `tls_cipher`   | `ClientInfo.tls_protocol/tls_cipher`                                                           | expected                    | unavailable            | unavailable                          | unavailable |
| `edge_hostname`, `edge_region` | `ClientInfo.server_hostname/server_region`                                                     | expected                    | unavailable            | unavailable                          | unavailable |
| `edge_pop`                     | new bounded adapter mapping from documented runtime metadata                                   | optional                    | unavailable            | optional                             | unavailable |

`expected` means the implementation plan must map and test an existing source;
`optional` means the adapter includes it only when its supported SDK exposes a
stable value; `unavailable` means v1 intentionally omits it. In particular,
ASN is currently not populated by the Fastly or Cloudflare geo adapters and
must not be claimed until a concrete source is implemented. New HTTP-version or
POP mappings must be confirmed against the pinned adapter SDK before addition.

Explicitly excluded:

- DNS resolver address and resolver ASN.
- Active bandwidth or speed tests.
- TCP congestion window, next hop, RTT, and retransmit counters.
- DDoS/internal Fastly classifications.
- Arbitrary request headers.
- Full client IP in HTML or export.
- JA4, H2, or other probabilistic client fingerprints.

Unsupported optional fields are omitted rather than populated with fabricated
fallbacks.

## 12. Security and privacy

### 12.1 Allowlist boundary

The report serializer constructs a new public model field by field. It never
serializes request structs, cookie parsers, auction requests, telemetry rows, or
browser objects wholesale.

The deserializer is an equally strict boundary. It validates the complete
outer and nested schema at runtime before any display, export, copy, or share
operation. Unknown properties, overlong strings, non-finite numbers, excessive
arrays, excessive depth, unsupported versions, and invalid timestamps reject
the report. Validation errors expose only bounded categories.

Forbidden data includes:

- Raw `Cookie` and `Set-Cookie` headers.
- EC IDs, EIDs, bidder user IDs, and consent strings.
- Unmasked client IP.
- Query strings and fragments.
- Fastly or internal request identifiers that can join to user-bearing logs.
- Internal `AuctionRequest.id`.
- Bid requests/responses, losing-bid payloads, targeting, creative markup,
  cache URLs, and stack traces.

### 12.2 Same-origin script visibility

Publisher and third-party scripts running on the publisher origin can access
`sessionStorage`. They can also replace it, opener-created tabs may receive a
copy, browser session restore may preserve it, and a same-origin service worker
may intercept navigation. Therefore the stored model must be safe even if read
or forged by any same-origin code. A random storage key, closed shadow root, or
public endpoint does not change this requirement.

Every report view and exported artifact is labeled `Browser-observed,
unverified diagnostic data`. Support documentation says that it helps
troubleshoot rendering but is not proof of a server event, user identity, or
security incident.

### 12.3 Response hardening

The HTML shell, enable/end responses, and every active diagnostic publisher
response are terminally `private, no-store`. The fixed versioned JS/CSS assets
are the sole exception and may be publicly cached because they contain no
request or report data. HTML and JSON endpoint responses also send:

- Path-appropriate `Content-Type`: `text/html; charset=utf-8` for the shell and
  `application/json; charset=utf-8` for enable, end, and state results.
- `X-Content-Type-Options: nosniff`
- `Referrer-Policy: no-referrer`
- The Content Security Policy specified below.
- `Permissions-Policy: camera=(), microphone=(), geolocation=(), payment=(),
usb=()`

```text
default-src 'none'; script-src 'self'; style-src 'self'; base-uri 'none';
object-src 'none'; frame-ancestors 'none'; form-action 'none'; connect-src 'self'
```

The endpoint makes no third-party requests. Its script and stylesheet are fixed
same-origin static assets. Setup-request values are server-rendered as escaped
text nodes, and the viewer obtains the report only from browser storage. Active
publisher-page context continues to use the repository's script-safe serializer
and is never concatenated into executable JavaScript. If inline executable
assets become necessary, they require a per-response nonce or fixed build-time
hash and a corresponding CSP change. Validated report strings enter the
document through `textContent` or equivalent DOM properties, never `innerHTML`.

The JS asset uses `application/javascript; charset=utf-8`; the CSS asset uses
`text/css; charset=utf-8`. Both send `X-Content-Type-Options: nosniff`,
`Cache-Control: public, max-age=31536000, immutable`, and a strong ETag derived
from their build bytes. They accept no dynamic input. `script-src 'self'` is an
origin-level CSP permission, not a path restriction; same-origin script
interference remains inside the stated trust limitation.

### 12.4 Shared templates and ESI

Per-request trace context must never enter a shared template or ESI fragment.
The existing diagnostics private/no-store decision remains a load-bearing gate.
Tests must prove that late response-header handlers cannot make traced content
publicly cacheable.

## 13. Failure handling

- Disabled route: local privacy-safe `404`.
- Unsupported method: local `405`; never publisher fallback.
- Rejected activation/end POST: local `403` with no state mutation.
- Optional platform fact unavailable: omit the field and continue.
- Bounded cookie inspection failure: report the contract-defined invalid or
  unavailable state without a value or parser message.
- Diagnostics context serialization failure: omit the context, log a bounded
  server error, and preserve publisher delivery.
- TS Console capture failure: fail open for advertising and show incomplete
  coverage in diagnostics.
- Storage unavailable or quota exceeded after a valid bounded report exists:
  remain on the publisher page, announce the error, and offer direct download
  of that same combined `TraceReportV1`.
- Invalid projection or a report that remains oversized after deterministic
  truncation: remain on the publisher page, show a bounded capture-failure
  category, and do not claim that a combined trace report exists.
- Missing snapshot on endpoint: show setup state, not an empty successful
  report.
- Expired, malformed, or unknown report schema: clear it and explain that the
  user must reproduce again.
- Clipboard or Web Share unavailable: keep JSON download available.
- Export failure: retain the on-screen report and show an accessible error.
- End POST failure: report that tracing may remain active and offer an
  idempotent server retry; do not undo or block the independent local-deletion
  attempt.
- State verification failure or mismatch: use `unconfirmed` wording and offer
  an idempotent server retry independently of local report state.
- Local deletion failure: report separately that saved browser data could not be
  removed and retain the always-available deletion retry, regardless of the
  server end result.

Diagnostic failures must never suppress, delay, add, remove, or reorder GPT
requests, auctions, targeting, or creative rendering.

## 14. Testing strategy

### 14.1 Core unit tests

- Configuration defaults off and rejects trace-page enablement without GPT
  diagnostics.
- Exact reserved-route classification, canonical-path, query, method, encoded
  path, and fallback behavior.
- Exact versioned asset routes are local and contain no dynamic data; lookalike
  asset paths never reach the publisher origin.
- Same-origin POST validation, cross-site/missing signal rejection, cookie
  set/clear attributes, fixed 30-minute endpoint activation without request
  refresh, and idempotent enable/end behavior.
- Enable/end success requires a separate state request to observe the resulting
  cookie; failed and mismatched verification never displays confirmed state.
- Empty-body enforcement rejects positive/invalid lengths, transfer encoding,
  the first unexpected body byte, and the two-second deadline without an
  unbounded read.
- Endpoint skips EC generation/finalization, EID ingestion, auction, telemetry,
  configured filters, ordinary event context, and origin fetch.
- Cookie-health scanner covers multiple header fields; zero, one, and duplicate
  occurrences; mixed valid/invalid duplicates; non-UTF-8; malformed pairs; and
  per-value and total-header limits without retaining values.
- Request-context serializer masks IPv4/IPv6; enforces every string bound; and
  omits page paths, fingerprints, query, raw headers, IDs, and unsupported
  fields.
- Active responses remain terminally private/no-store under hostile late header
  overrides.
- Dynamic HTML/JSON values cannot close elements or create executable script.

### 14.2 Adapter parity tests

- Fastly early-route ordering and optional field mapping from documented
  sources.
- Axum, Cloudflare, and Spin return the common route/schema with unavailable
  fields omitted.
- Trace-route failures never fall through to publisher origin.
- GET, HEAD, state-changing POST, and unsupported methods obey the same
  lifecycle contract across adapters.
- Every adapter omits JA4/H2 and rejects control characters or overlong platform
  strings.

### 14.3 JavaScript unit tests

- Explicit snapshot only; no continuous `sessionStorage` writes.
- Compact UTF-8 size measurement; exact outer/nested schema validation; unknown
  fields; per-string/array/numeric/depth caps; hostile mutation; expiry;
  future-clock skew; wall-clock rollback; replacement; clearing; and storage
  exceptions.
- Omission counters use checked arithmetic and reject overflow.
- Trace projection replaces the nested GPT pathname with `/[redacted]`, rejects
  any other stored value, emits `TraceGptDiagnosticsV1`, applies deterministic
  ordering/truncation, and records exact omission counts in a worst-case 512 KiB
  fixture.
- Same-tab navigation occurs only after a successful write.
- Viewer handles absent optional network facts and every cookie-health state.
- Forbidden fields never enter storage or export fixtures.
- Download filename and MIME type are deterministic.
- Formatted-JSON copy and JSON-file Web Share success, rejection, absence, and
  download/copy fallback behavior.
- 320-pixel layout, keyboard navigation, focus handling, and accessible status
  announcements.

### 14.4 Browser integration tests

- First endpoint GET is read-only and shows setup state; a user-initiated,
  same-origin enable POST sets the session.
- Successful in-page activation adds no history entry, so Back can return to
  the article when it was the prior same-tab page.
- Cross-site top-level GET, form POST, and fetch attempts cannot enable or end
  tracing.
- A real fixture reload activates diagnostics and captures multiple slots.
- `View trace results` navigates in the same tab and renders the captured
  request context and slot evidence.
- Empty, filled, ambiguous, no-candidate, and unattributed slot states remain
  distinct.
- Reloading the trace page retains an unexpired same-tab report.
- Opener-cloned tabs and browser session restore never bypass validation or
  application expiry; the UI does not promise tab isolation.
- Ending tracing covers successful clearing, offline POST failure, idempotent
  retry, verification mismatch, successful local deletion while offline, and
  local-storage deletion failure without false success messaging.
- Back-forward-cache restoration is not described as a fresh traced request;
  the setup page tells the user to reload.
- Export JSON matches the displayed versioned model.
- A storage-failure direct export matches the combined displayed model rather
  than the GPT-only export.
- Hostname changes between apex, `www`, or another subdomain show the recovery
  guidance rather than claiming the session followed the user.
- A fixture service worker interception is recognized as a same-origin trust
  limitation, and the server endpoint remains correct when the request reaches
  it.
- The delivered CSP blocks inline injection, framing, third-party connections,
  and report-derived executable HTML.
- Immutable asset fixtures prove published v1 bytes never change; changed bytes
  require a new URL referenced by the shell.
- Inactive publisher traffic has no trace assets, storage access, listeners, or
  cache-policy change.

### 14.5 Manual acceptance

Verify on current iOS Safari and Android Chrome using a representative publisher
fixture:

- A layperson can follow the page instructions without developer tools.
- Touch targets, scrolling, zoom, safe areas, download, copy, and native share
  behavior are usable.
- The user can distinguish setup information from captured-page information.
- A failed share or download does not lose the visible report.

## 15. Rollout and observability

- Ship default-off.
- Enable first in a controlled staging publisher configuration.
- Validate response cache headers and CDN behavior before production use.
- Validate with redacted fixtures before real publisher traffic.
- Log only server-observable route outcome, served shell/schema version, and
  bounded error category. The server cannot know whether a browser-local report
  exists and must not add an upload or beacon merely to learn that fact. Never
  log report contents or cookie/network values.
- Roll back by disabling `trace_page_enabled`; existing `?ts_console=1`
  diagnostics remain independently configurable.

## 16. Acceptance criteria

1. With the feature disabled, trace-route origin requests return local `404`
   and ordinary traffic is unchanged. Previously cached inert versioned assets
   may remain until cache eviction, but cannot activate tracing or load a shell.
2. A mobile user can enable tracing by opening only `/_ts/trace` and selecting
   one prominent action; no target URL, credentials, or trace ID is required,
   and a cross-site GET cannot activate tracing.
3. The setup page accurately explains that the problem must be reproduced after
   activation.
4. A subsequent real publisher-page reload captures redacted request context
   and existing TS Console evidence without altering ad behavior.
5. `View trace results` transfers one bounded, runtime-validated snapshot in
   the supported same-tab journey and opens the report page without server-side
   storage or claims of browser-storage isolation.
6. The report separates network, cookie health, auction/render evidence, and
   coverage/unknowns.
7. JSON export contains the same versioned allowlisted information shown on the
   page.
8. No raw cookies, user IDs, full IPs, consent strings, exact page paths, query
   strings, fingerprints, internal auction IDs, targeting, or creative payloads
   appear in trace HTML, browser storage, logs, or export.
9. Trace HTML and active publisher pages remain terminally private/no-store.
10. Missing platform fields, incomplete auction correlation, storage failure,
    and unavailable share APIs degrade honestly without affecting advertising.
11. The full report is usable at 320 CSS pixels and with keyboard/screen-reader
    navigation.
12. Version one accepts exactly `TraceReportV1` with
    `TraceGptDiagnosticsV1`, sourced only from `GptDiagnosticsExportV1`; #1081
    and #1074/#1076 are optional additive follow-ups rather than release gates.
13. Every rendered and exported report is identified as browser-observed and
    unverified, and hostile storage content cannot create executable HTML or
    unbounded DOM output.
14. Enable/end operations expose partial failure honestly and are safe to
    retry; the UI does not claim server-observed cookie state without the
    follow-up state request or claim that local data was cleared when deletion
    fails.

## 17. Implementation sequencing

This design is one product flow, but its implementation is split into three
independently reviewable plans and preferably three PRs:

1. **Reserved route and privacy foundation:** configuration, shared early-route
   classification, same-origin enable/end lifecycle, bounded cookie-health
   inspection, base request-context schema, projection of already populated
   `ClientInfo`/`GeoInfo` fields, response hardening, and adapter parity. Do not
   add speculative new platform fields in this change.
2. **Browser handoff and viewer:** integrate current
   `GptDiagnosticsExportV1`, project `TraceGptDiagnosticsV1`, construct and
   strictly validate `TraceReportV1`, implement the same-tab workflow, combined
   direct/storage exports, mobile viewer, copy/share, expiry, clearing, and
   browser/accessibility tests.
3. **Optional network enrichment and future schemas:** add HTTP-version, POP,
   ASN, or other fields only from SDK-verified platform sources with explicit
   bounds. Adopt #1081 or #1074/#1076 later through a separately reviewed
   versioned compatibility change.

Each plan must include its own adapter, privacy, cache, and failure tests. The
implementation must not claim completion of #1081 or the timing work as part of
#1050. Version one ships with current observed auction/render evidence and
labels unavailable fields honestly.

## 18. Rejected alternatives

### Automatic activation on `GET /_ts/trace`

Rejected because a cross-site top-level navigation can trigger a public GET and
`SameSite=Lax` does not make that activation intentional. A same-origin fetch
POST after one explicit button press preserves the simple mobile journey and
the useful Back history entry without requiring server-side session storage.

### `/_ts/admin/trace?target=/article`

Rejected because it requires the user to supply the affected URL twice, adds
target validation and open-redirect risk, and is unsuitable for a layperson.

### Basic Authentication

Rejected for the mobile end-user workflow. Authentication also would not make
it safe to inject raw secrets into a publisher page containing third-party
JavaScript.

### Server-managed trace sessions

Rejected because they require shared storage, report authorization, expiry,
deletion, and operational infrastructure beyond the issue's needs.

### Synthetic auction on the endpoint

Rejected because it does not reproduce the real page's DOM, GPT lifecycle,
consent context, refresh path, or auction timing and could produce misleading
results.

### Endpoint-only report with no publisher-page integration

Rejected because a request to `/_ts/trace` cannot observe rendering that
occurred in another document.

### Query-only in-page console

The existing `?ts_console=1` flow remains supported, but it is not the complete
answer to #1050: the issue asks for a memorable mobile endpoint and a
Fastly-style consolidated HTML report. The endpoint/viewer builds on rather
than replaces the console.

### Cross-page URL payload

Rejected because fragments or query strings containing the report create URL
length, history, logging, referrer, and accidental-sharing risks.

## 19. Known limitations

- The user must reproduce the problem after enabling tracing.
- Same-tab navigation is the supported workflow, but opener-created tabs and
  browser session restore may copy or retain session storage. JSON export is
  the intentional support handoff artifact.
- Publisher-origin scripts and service workers can read or forge the stored
  public-safe report. A same-origin service worker can also intercept or fake
  the shell, enable/end/state requests, assets, and report navigation. The
  experience is diagnostic evidence, not an authenticity boundary.
- The activation cookie is host-only and session storage is origin-scoped, so
  the workflow does not follow the user across apex, `www`, or other
  subdomains.
- Browser privacy settings may disable storage, clipboard, download, or share
  capabilities.
- Current server/browser correlation does not cover every no-bid, skipped,
  failed, hidden, unresolved, or direct-auction path.
- Fastly-only transport details do not exist on every adapter.
- The current 30-pixel TS Console controls are not sufficient for this mobile
  report; the endpoint uses independent 44-pixel touch targets.
- `fastly-debug.com` fields that require resolver, TCP, or active speed probes
  remain out of scope.

These limitations are displayed in operator documentation and, where relevant,
in the report itself. They are not hidden behind apparently successful empty
states.
