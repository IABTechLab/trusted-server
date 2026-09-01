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
an exportable diagnostic report.

The endpoint is both a setup page and a report viewer. On the first visit it
enables the existing GPT diagnostics browser session and explains how to
reproduce the problem. The user then returns to the real publisher page and
reloads it. Trusted Server supplies redacted request context, while the existing
TS Console records GPT, auction, and render evidence. A `View trace results`
action creates one bounded, allowlisted snapshot in same-tab `sessionStorage`
and navigates to `/_ts/trace`. The endpoint reads that snapshot, presents a
mobile-first HTML report, and offers JSON export, copy, and progressive Web
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
  slot and request cycle.
- Support a full report in a narrow mobile viewport without developer tools.
- Export the same allowlisted model as formatted JSON.
- Keep capture bounded, same-tab, temporary, and inactive by default.
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

### 5.1 Public, redacted endpoint

`/_ts/trace` is public when explicitly enabled by deployment configuration. It
is not placed under `/_ts/admin`, because the intended user is a layperson on a
phone and the existing Basic Authentication flow is unsuitable for that
journey.

Public access is safe only because both the page and export use a strict
allowlist. The activation cookie is a feature toggle, not authentication. No
field becomes eligible merely because tracing is active.

### 5.2 Reuse the existing diagnostics session

The endpoint reuses `__Host-ts-console` and the existing GPT diagnostics
activation semantics rather than creating a second `ts-trace` session. The
cookie remains host-only, `Secure`, `HttpOnly`, `SameSite=Lax`, and
browser-session scoped.

`/_ts/trace?enabled=false` clears the activation cookie and browser snapshot.
Other values, duplicate `enabled` parameters, and malformed directives fail
closed and do not mutate session state.

### 5.3 Browser-local, explicit handoff

TS Console remains memory-only during observation. It writes a report to
`sessionStorage` only after the user selects `View trace results`. The action:

1. Builds the same versioned allowlisted snapshot used by export.
2. Adds the redacted request-context envelope.
3. Serializes and validates the size.
4. Stores it under one versioned key in the current tab.
5. Navigates the same tab to `/_ts/trace`.

Continuous persistence is prohibited. Opening the endpoint in another tab does
not retrieve the snapshot. Closing the tab deletes it according to browser
session-storage semantics.

### 5.4 Forward reproduction, not historical diagnosis

The first endpoint visit enables tracing for subsequent eligible document
navigations. The setup page must say plainly that the user needs to return to
the affected page, reload it, and reproduce the problem.

If the user replaced the affected URL in the address bar with `/_ts/trace`, the
page offers a `Return to previous page` action backed by browser history and
then instructs the user to reload once. The design does not claim that
back-forward-cache restoration caused a new server request.

Support should preferably give the user the trace URL before reproduction. The
product does not attempt to discover the previous URL through `Referer`, because
address-bar navigation commonly omits it and relying on it would create
inconsistent behavior.

### 5.5 Separate issue ownership

#1050 defines the report shell, request context, mobile flow, browser-local
handoff, and export. #1081 remains the owner of creative numbering, auction
classification, bidder/price policy, terminology, and normalized auction/render
timing.

The trace report consumes TS Console's public versioned export contract. It
does not read TS Console internals or create an alternate slot correlation
engine.

## 6. User experience

### 6.1 First visit: no captured report

`GET /_ts/trace` returns a mobile-first HTML page with:

- Title: `Trusted Server ad diagnostics`.
- State: `Trace ready` after the response establishes the session cookie.
- A short explanation that no previous ad failure can be recovered.
- Network and cookie health for the setup request, labeled `Setup request`.
- Primary action: `Return to previous page` when browser history permits.
- Secondary instructions: return to the affected page, reload once, reproduce
  the problem, then select `View trace results`.
- Action to disable tracing.

The page must not imply that setup-request network facts or an empty auction
section describe the affected page.

### 6.2 Active publisher page

The existing TS Console remains available. On mobile it gains a prominent
`View trace results` action. Selecting it never changes ad behavior; it only
snapshots retained observations and navigates after serialization succeeds.

If the snapshot cannot be stored, the page remains in place, announces the
failure, and keeps the existing direct JSON export available.

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
First GET /_ts/trace
    |
    |-- core route builds setup request context
    |-- response sets __Host-ts-console
    |-- HTML explains forward reproduction
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
Second GET /_ts/trace
    |
    |-- static report shell reads and validates TraceReportV1
    |-- mobile HTML renders sections
    |-- local JSON/copy/share actions
```

### 7.1 Core responsibilities

- Define configuration and route behavior.
- Register the route before publisher fallback on every supported adapter.
- Define the platform-neutral request-context and report-envelope schemas.
- Build cookie-health facts through read-only parsing.
- Convert `ClientInfo` and available geo data into the public network allowlist.
- Inject request context only into an active private diagnostics document.
- Apply response privacy and security headers.
- Ensure trace requests never reach the publisher origin.

### 7.2 Adapter responsibilities

- Register the named route with exact method handling.
- Populate optional `ClientInfo` fields available on the platform.
- Fastly may supply POP, HTTP version, TLS, JA4, H2 fingerprint, and edge
  server data when the SDK exposes them.
- Other adapters return the same schema with unsupported fields absent.
- Adapter-specific errors omit optional facts rather than failing publisher
  delivery.

### 7.3 JavaScript responsibilities

- Accept the immutable redacted request context at initialization.
- Preserve the existing bounded TS Console observation store.
- Build and validate `TraceReportV1` on explicit user action.
- Store only one report in same-tab `sessionStorage`.
- Render the report shell from the validated model.
- Implement download, copy, progressive Web Share, clearing, expiry, and
  accessible status reporting.
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
- `GET /_ts/trace` returns the setup/report HTML and establishes the session.
- `GET /_ts/trace?enabled=false` returns the shell, clears the cookie, and asks
  the client to clear the stored snapshot.
- `HEAD /_ts/trace` returns the same status and headers without a body but does
  not mutate the cookie.
- All other methods return a local `405 Method Not Allowed` with `Allow: GET,
HEAD`.
- Disabled deployments return a local `404` for the exact route and never fall
  through to the publisher origin.
- Extra path segments, encoded separators, duplicate parameters, and lookalike
  paths do not match.
- The route never creates or refreshes an EC, ingests EIDs, runs an auction,
  fetches the publisher origin, or emits auction telemetry.

The current `?ts_console=1` and `?ts_console=0` activation flow remains
supported for technical users. Both activation surfaces drive the same cookie
and runtime; they must not create two concurrent diagnostic modes.

## 9. Data contracts

### 9.1 Request context

The server injects one immutable `TraceRequestContextV1` into active diagnostic
documents:

```text
TraceRequestContextV1
  schema_version: 1
  captured_at: RFC 3339 UTC timestamp
  page:
    origin: publisher origin
    path: normalized path
  network:
    masked_client_ip?: string
    country?: string
    region?: string
    asn?: u32
    http_version?: string
    tls_protocol?: string
    tls_cipher?: string
    tls_ja4?: string
    h2_fingerprint?: string
    edge_hostname?: string
    edge_region?: string
    edge_pop?: string
  cookies:
    ts_ec: CookieHealth
    ts_eids: CookieHealth
    ts_tester: CookieHealth
    diagnostics_session: CookieHealth
```

The page field omits query and fragment data. It does not contain origin-facing
URLs, referrers, or arbitrary headers.

`masked_client_ip` uses a deterministic display-only mask for the current
request: IPv4 keeps at most the first 24 bits and IPv6 keeps at most the first
48 bits. The full address never enters HTML, JavaScript, browser storage, or
export.

JA4 and H2 fingerprints are optional probabilistic identifiers. They are
included only when the deployment has separately enabled the existing
fingerprint diagnostic capability. Their absence is not an error.

### 9.2 Cookie health

```text
CookieHealth
  state:
    absent | present_valid | present_invalid | duplicate | unavailable
  source: request
  detail?: allowlisted enum
```

Allowed details describe shape, not value, for example `valid_ec_format`,
`malformed`, `oversized`, or `activation_pending_response`.

The parser must inspect the incoming request before any diagnostics-cookie
sanitization, while preserving existing authoritative-cookie and consent
semantics. Inspection is read-only: it must not generate an EC, touch the
identity graph, sync partner IDs, or extend any cookie lifetime.

Only Trusted Server-owned cookie names are reported. Arbitrary cookie names and
values are excluded. The endpoint cannot claim knowledge of browser attributes,
expiry, or cookies the browser withheld from the request.

### 9.3 Report envelope

```text
TraceReportV1
  schema_version: 1
  captured_at: RFC 3339 UTC timestamp
  request_context: TraceRequestContextV1
  gpt_diagnostics: GptDiagnosticsExportV1-or-successor
```

The trace envelope owns request context and transport. TS Console continues to
own its nested schema. Compatibility is explicit: the viewer supports a small
documented set of TS Console schema versions and rejects unknown versions with
an actionable message rather than guessing.

### 9.4 Storage limits and expiry

- Storage key: a namespaced, versioned constant owned by the diagnostics
  module.
- Maximum encoded report size: 512 KiB.
- Maximum report age: 15 minutes from `captured_at`.
- One report per tab; a new explicit snapshot replaces the old report.
- Invalid, oversized, expired, or unsupported reports are removed immediately.
- `Clear report and end tracing` removes the storage entry and clears the
  activation cookie.

These are product limits, not assumptions about browser quota. A storage write
failure is handled even when the report is below the application limit.

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

Timing fields introduced by #1074/#1076 are consumed only after they merge and
are propagated through the live diagnostics contract. The report never queries
Tinybird, and it does not combine browser `performance.now()` values with
server-relative timing as though they were one clock.

Bidder and winning price are included only if #1081 approves them in the public
TS Console export contract. #1050 does not independently weaken the existing
privacy policy.

## 11. Network scope

The report is inspired by Fastly Debug, not a clone. Version one uses facts
already present or reasonably addable to the platform request abstraction.

Supported categories:

- Masked client address.
- Country, region, and ASN when available.
- HTTP version.
- TLS protocol and cipher.
- Optional JA4 and H2 fingerprints.
- Edge hostname, region, and POP.
- Capture time.

Explicitly excluded:

- DNS resolver address and resolver ASN.
- Active bandwidth or speed tests.
- TCP congestion window, next hop, RTT, and retransmit counters.
- DDoS/internal Fastly classifications.
- Arbitrary request headers.
- Full client IP in HTML or export.

Unsupported optional fields are omitted rather than populated with fabricated
fallbacks.

## 12. Security and privacy

### 12.1 Allowlist boundary

The report serializer constructs a new public model field by field. It never
serializes request structs, cookie parsers, auction requests, telemetry rows, or
browser objects wholesale.

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
`sessionStorage`. Therefore the stored model must be safe even if read by any
same-origin script. A random storage key, closed shadow root, or public endpoint
does not change this requirement.

### 12.3 Response hardening

Both the endpoint and every active diagnostic publisher response are terminally
`private, no-store`. The endpoint also sends:

- `Content-Type: text/html; charset=utf-8`
- `X-Content-Type-Options: nosniff`
- `Referrer-Policy: no-referrer`
- `Content-Security-Policy` restricting content to the endpoint's own static
  assets and prohibiting framing
- A restrictive `Permissions-Policy`

The endpoint makes no third-party requests. Dynamic JSON embedded in HTML uses
the repository's script-safe serializer and is never concatenated into
executable JavaScript.

### 12.4 Shared templates and ESI

Per-request trace context must never enter a shared template or ESI fragment.
The existing diagnostics private/no-store decision remains a load-bearing gate.
Tests must prove that late response-header handlers cannot make traced content
publicly cacheable.

## 13. Failure handling

- Disabled route: local privacy-safe `404`.
- Unsupported method: local `405`; never publisher fallback.
- Optional platform fact unavailable: omit the field and continue.
- Cookie parser failure: report `present_invalid` without the value.
- Diagnostics context serialization failure: omit the context, log a bounded
  server error, and preserve publisher delivery.
- TS Console capture failure: fail open for advertising and show incomplete
  coverage in diagnostics.
- Storage unavailable, quota exceeded, or serialization oversized: remain on
  the publisher page, announce the error, and offer direct download.
- Missing snapshot on endpoint: show setup state, not an empty successful
  report.
- Expired, malformed, or unknown report schema: clear it and explain that the
  user must reproduce again.
- Clipboard or Web Share unavailable: keep JSON download available.
- Export failure: retain the on-screen report and show an accessible error.

Diagnostic failures must never suppress, delay, add, remove, or reorder GPT
requests, auctions, targeting, or creative rendering.

## 14. Testing strategy

### 14.1 Core unit tests

- Configuration defaults off and rejects trace-page enablement without GPT
  diagnostics.
- Exact route, query, method, encoded-path, and fallback behavior.
- Session cookie set/clear attributes and duplicate-directive fail-closed
  behavior.
- Endpoint skips EC generation/finalization, EID ingestion, auction, telemetry,
  and origin fetch.
- Cookie-health parser covers absent, valid, malformed, duplicate, non-UTF-8,
  and oversized inputs without retaining values.
- Request-context serializer masks IPv4/IPv6 and omits query, raw headers, IDs,
  and unsupported fields.
- Active responses remain terminally private/no-store under hostile late header
  overrides.
- Dynamic HTML/JSON values cannot close elements or create executable script.

### 14.2 Adapter parity tests

- Fastly route registration and optional field mapping.
- Axum, Cloudflare, and Spin return the common route/schema with unavailable
  fields omitted.
- Named route failures never fall through to publisher origin.
- HEAD and unsupported methods behave identically across adapters.
- Fastly fingerprint fields respect the existing fingerprint-debug gate.

### 14.3 JavaScript unit tests

- Explicit snapshot only; no continuous `sessionStorage` writes.
- Size limit, schema validation, expiry, replacement, clearing, and storage
  exceptions.
- Same-tab navigation occurs only after a successful write.
- Viewer handles absent optional network facts and every cookie-health state.
- Forbidden fields never enter storage or export fixtures.
- Download filename and MIME type are deterministic.
- Copy and Web Share success, rejection, absence, and fallback behavior.
- 320-pixel layout, keyboard navigation, focus handling, and accessible status
  announcements.

### 14.4 Browser integration tests

- First endpoint visit sets the session and shows setup state.
- A real fixture reload activates diagnostics and captures multiple slots.
- `View trace results` navigates in the same tab and renders the captured
  request context and slot evidence.
- Empty, filled, ambiguous, no-candidate, and unattributed slot states remain
  distinct.
- Reloading the trace page retains an unexpired same-tab report.
- A new tab cannot access the original tab's report.
- Disabling clears both cookie and report.
- Back-forward-cache restoration is not described as a fresh traced request;
  the setup page tells the user to reload.
- Export JSON matches the displayed versioned model.
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
- Log only route outcome, schema version, report-present boolean, and bounded
  error category. Never log report contents or cookie/network values.
- Roll back by disabling `trace_page_enabled`; existing `?ts_console=1`
  diagnostics remain independently configurable.

## 16. Acceptance criteria

1. With the feature disabled, exact trace routes return local `404` and ordinary
   traffic is unchanged.
2. A mobile user can enable tracing by opening only `/_ts/trace`; no target URL,
   credentials, or trace ID is required.
3. The setup page accurately explains that the problem must be reproduced after
   activation.
4. A subsequent real publisher-page reload captures redacted request context
   and existing TS Console evidence without altering ad behavior.
5. `View trace results` transfers one bounded snapshot in the same tab and opens
   the report page without server-side storage.
6. The report separates network, cookie health, auction/render evidence, and
   coverage/unknowns.
7. JSON export contains the same versioned allowlisted information shown on the
   page.
8. No raw cookies, user IDs, full IPs, consent strings, query strings, internal
   auction IDs, targeting, or creative payloads appear in HTML, browser storage,
   logs, or export.
9. Trace HTML and active publisher pages remain terminally private/no-store.
10. Missing platform fields, incomplete auction correlation, storage failure,
    and unavailable share APIs degrade honestly without affecting advertising.
11. The full report is usable at 320 CSS pixels and with keyboard/screen-reader
    navigation.
12. Auction fields owned by #1081 are consumed through its versioned public
    contract rather than duplicated in #1050.

## 17. Implementation sequencing

This design is one product flow but should be implemented in dependency order:

1. Core request-context schema, cookie-health classification, configuration,
   and endpoint shell.
2. Adapter route parity and Fastly optional network enrichment.
3. TS Console request-context envelope and explicit same-tab snapshot handoff.
4. Mobile viewer, export/copy/share, expiry, and clearing.
5. Integration with the current TS Console schema.
6. Additive adoption of #1081 and #1074/#1076 fields after their contracts
   merge.
7. Browser, privacy, cache, and real-device acceptance.

The implementation plan must not claim completion of #1081 or the open timing
PRs as part of #1050. If those dependencies are unavailable, the report ships
only with current observed auction/render evidence and labels unavailable fields
honestly.

## 18. Rejected alternatives

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
- Same-tab storage prevents cross-device and cross-tab sharing; JSON export is
  the handoff artifact.
- Publisher-origin scripts can read the stored public-safe report.
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
