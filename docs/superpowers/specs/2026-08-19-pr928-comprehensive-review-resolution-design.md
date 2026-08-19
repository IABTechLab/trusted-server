# PR #928 Comprehensive Review Resolution

**PR:** #928  
**Date:** 2026-08-19  
**Status:** Approved design

## Problem

PR #928 adds authenticated EC and EID diagnostic endpoints. Follow-up review
found three blocking correctness issues and six related quality gaps:

1. Fastly's EC lookup still enters the mutating EC request/finalization
   lifecycle, so a diagnostic GET can ingest browser cookies, mint identity
   state, write a withdrawal tombstone, or trigger pull sync.
2. Startup authentication coverage checks only one lowercase-suffix EC ID. A
   narrower handler regex can pass startup while valid mixed-case IDs fail
   closed at runtime.
3. The API guide describes every response as JSON with `no-store`, although
   the shared Basic-auth rejection is plaintext and has no cache header.
4. The EID preview omits configured sources whose UID list contains no value
   accepted by the real ingestion path.
5. Core contains duplicate request-cookie extraction helpers.
6. Admin JSON responses lack `X-Content-Type-Options: nosniff`.
7. The tombstone field documentation omits typed deserialization failures.
8. New diagnostic routes lack explicit unauthenticated adapter regressions.
9. Operators are not warned that narrow pre-existing admin handler patterns
   must expand to cover the new routes.

## Goals

- Make both Fastly diagnostic GET handlers read-only by construction.
- Detect common under-coverage of the full valid EC ID suffix alphabet during
  settings finalization while retaining runtime fail-closed protection.
- Make the documented response contract accurately distinguish authentication
  failures from successfully authenticated diagnostic-handler responses.
- Report every parsed EID source that ingestion drops, with an operator-useful
  reason.
- Consolidate identical core cookie-header parsing without changing semantics.
- Apply browser-safe response headers consistently to admin diagnostic JSON.
- Pin the new routes' authentication behavior on every adapter.
- Document the intentional configuration compatibility impact.

## Non-goals

- Do not change Basic-auth middleware response bodies or headers. That shared
  behavior predates the diagnostic endpoints and is outside this PR's scope.
- Do not attempt formal regex-language inclusion. Arbitrary configured regexes
  make exhaustive proof impractical; runtime authentication remains the final
  fail-closed invariant.
- Do not change live EID ingestion, partner matching, UID validation, or
  deduplication behavior.
- Do not add EC lookup support to Axum, Cloudflare, or Spin.
- Do not introduce a test-only Fastly KV abstraction solely to spy on writes.
- Do not alter unrelated response builders or cookie parsing behavior.
- Do not push the branch, reply to GitHub threads, or resolve review
  conversations as part of implementation.

## Design

### 1. Read-only Fastly diagnostic dispatch

Move `AdminEcLookup` into the same `execute_named` early-dispatch branch as
`AdminEidsLookup`, before GPT diagnostic preparation, EC request-state
construction, and request filters. Construct the partner registry once, match
the requested diagnostic handler, and for EC lookup construct its KV identity
graph directly from settings, as the existing handler already does. Return the
handler response immediately without `attach_dispatch_extensions`.

Keep exhaustive `run_named_route` arms for both diagnostic variants as
`unreachable!`, documenting that they must be handled before EC setup. Basic
authentication and normal outer response middleware continue to run because
they wrap `execute_named`.

The regression uses an authenticated, browser-shaped EC request containing
`ts-ec`, `ts-eids`, and `sharedId` cookies plus device signals. It asserts a
successful diagnostic response has neither `EcFinalizeState` nor EC cookie
mutation headers. The Fastly entry point invokes EC finalization and all of its
KV writes only when `EcFinalizeState` is present, so absence tests the
production write gate without adding a test-only storage seam. Existing core
lookup tests continue to establish that the handler performs reads only.

### 2. Dynamic authentication probe corpus

Retain `Settings::ADMIN_ENDPOINTS` as the canonical operator-facing route list,
but map `/_ts/admin/ec/{id}` to a small fixed corpus of concrete valid IDs. The
corpus contains at least:

- a lowercase-and-digit suffix such as `.abc123`; and
- a mixed-case-and-digit suffix such as `.Ab12Z9`.

All probes use a valid 64-character lowercase hexadecimal hash. An endpoint is
covered only when every probe has a matching configured handler. Different
handlers may collectively cover the corpus because every matched handler still
requires Basic authentication. Placeholder-password validation classifies a
handler as protecting the dynamic admin route when it matches any probe, so a
narrow handler cannot escape credential-strength checks.

Add a startup regression in which a lowercase-only suffix regex covers the
first probe but not the mixed-case probe. The configuration must be rejected
and continue to report the canonical `/_ts/admin/ec/{id}` template. Existing
runtime fail-closed authentication remains unchanged and protects valid IDs
outside the representative corpus.

### 3. Accurate API and upgrade documentation

Change the Admin Diagnostic Endpoints introduction to say that responses
produced after successful authentication are JSON with
`Cache-Control: no-store`. Explicitly note that missing or invalid credentials
use the shared plaintext `401 Unauthorized` challenge contract.

Document the reason-tagged EID drop objects described below. Add an Unreleased
changelog entry stating that configurations which protected only the older key
management routes now fail startup and must broaden their authenticated handler
coverage to all `/_ts/admin` diagnostic routes. Recommend a namespace-wide
pattern such as `^/_ts/admin(?:/|$)` while preserving the existing warning
against accidentally protecting non-admin browser endpoints.

### 4. Reason-tagged EID ingestion drops

Replace `ingest.unmatched: string[]` with a list of objects containing:

- `source`: the EID source string; and
- `reason`: `no_partner` or `no_valid_uid`.

The production OpenRTB conversion intentionally removes structured entries
whose UID list becomes empty, so it cannot be the only diagnostic parse. Decode
the cookie once into the existing legacy-or-structured wire representation,
then return one shared analysis result with three derived views, without
changing live behavior:

- the existing filtered `Vec<Eid>` used by ingestion and returned in `eids`;
- a private diagnostic source view that retains non-empty source names even
  when all supplied UIDs are empty or otherwise unusable; and
- the partner updates selected by the same lookup and first-valid-UID rules as
  live ingestion.

Classify retained `ts-eids` sources using the same partner lookup and valid-UID
predicate as live ingestion:

- no configured partner for the source becomes `no_partner`;
- a configured partner with no non-empty UID within the ingestion size limit
  becomes `no_valid_uid`;
- a configured partner with a valid UID is represented by the existing
  deduplicated `matched` output and is not dropped.

Group duplicate cookie entries by source for drop reporting. If any entry for
a configured source has a valid UID, emit no `no_valid_uid` drop for that
source; otherwise emit exactly one. An unconfigured source emits exactly one
`no_partner` drop. `sharedId` does not suppress a `ts-eids` drop because the
preview is explaining that source's own cookie input.

Malformed `ts-eids` remains represented by `parse_error`, because there is no
parsed source to classify. `sharedId` behavior remains unchanged. This response
schema is safe to establish now because the endpoint is new in this unmerged
PR; the API guide and tests change in the same commit.

Refactor decoding behind private helpers in the existing Prebid EID ingestion
module so the public production parser retains identical output. Add one
crate-visible analysis function used by the admin handler; make the production
update collector reuse the same analysis and extract only its updates. Expose
the UID-validity predicate within the crate as needed. This keeps one decode per
caller and prevents preview classification and live ingestion from drifting.

### 5. Shared core cookie extraction

Add a generic request-cookie value helper to the existing core `cookies`
module. It accepts `&http::Request<B>` so both current core call sites can use
it regardless of body type, and preserves the existing behavior exactly:
inspect only the value selected by `headers().get(COOKIE)`; return `None` when
that selected value is absent or invalid UTF-8; otherwise split
semicolon-delimited pairs, trim whitespace, split only on the first `=`, and
return an owned value for the requested name. It does not scan later repeated
Cookie header values.

Use it from `ec/admin.rs` and `auction/endpoints.rs`, deleting both local
copies. The Fastly adapter's separate helper operates on Fastly's platform
request type and remains local; forcing it through an incompatible abstraction
would expand scope without removing meaningful duplication.

### 6. Diagnostic response hardening and field documentation

Add `X-Content-Type-Options: nosniff` to the shared admin diagnostic
`json_response` builder. This covers EC lookup, EID preview, unsupported-adapter
responses, and local diagnostic fallback denials. Tests assert the header on
representative success and error responses.

Update the `tombstone` field comment to state that it is absent when the body
cannot be parsed as JSON or deserialized as the typed `KvEntry` schema. No
runtime behavior changes.

### 7. Cross-adapter authentication regressions

Add one unauthenticated `GET /_ts/admin/ec` test to each adapter's established
route-test layer: Fastly, Axum, Cloudflare, and Spin. Each test asserts `401`
and the Basic `WWW-Authenticate` challenge. Keep existing authenticated route
tests unchanged so the pair establishes authentication-before-handler ordering
for both the Fastly implementation and portability adapters' `501` response.

## Error Handling

No new public failure mode is introduced. Partner-registry and KV-graph
construction errors continue through the existing `http_error` conversion.
Probe validation continues to return `TrustedServerError::Configuration` and
reports canonical route templates. EID preview classification is infallible
after cookie parsing. Shared cookie extraction intentionally ignores malformed
header encoding exactly as the removed helpers do.

## Testing Strategy

Implementation follows red-green-refactor, one review concern at a time:

1. Add the cookie-bearing Fastly EC lookup regression, observe
   `EcFinalizeState`, then early-dispatch the handler and verify its absence.
2. Add the lowercase-only auth-handler regression, observe startup success,
   then require the mixed-case probe and verify rejection.
3. Add EID preview cases for `no_partner`, empty UID, and over-limit UID before
   implementing the reason-tagged drop type and shared validity helper.
4. Add shared cookie-helper unit coverage, migrate the two core callers, and
   run their focused tests.
5. Add `nosniff` assertions and the four adapter unauthenticated regressions.
6. Update API and changelog text and run documentation formatting.
7. Run the affected target suites, all repository-required format checks, and
   target-matched clippy commands before claiming completion.

## Expected Files

- `crates/trusted-server-adapter-fastly/src/app.rs`
- `crates/trusted-server-adapter-axum/tests/routes.rs`
- `crates/trusted-server-adapter-cloudflare/tests/routes.rs`
- `crates/trusted-server-adapter-spin/tests/routes.rs`
- `crates/trusted-server-core/src/settings.rs`
- `crates/trusted-server-core/src/cookies.rs`
- `crates/trusted-server-core/src/auction/endpoints.rs`
- `crates/trusted-server-core/src/ec/admin.rs`
- `crates/trusted-server-core/src/ec/prebid_eids.rs`
- `docs/guide/api-reference.md`
- `CHANGELOG.md`
