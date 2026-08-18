# Admin diagnostics review fixes

**PR:** #928
**Date:** 2026-08-18
**Status:** Approved design

## Problem

PR #928 adds authenticated operator diagnostics at `/_ts/admin/ec`,
`/_ts/admin/ec/{id}`, and `/_ts/admin/eids`. Review identified five issues:

1. Startup authentication coverage checks the literal router template
   `/_ts/admin/ec/{id}`, while runtime authentication checks concrete request
   paths. A handler that matches only the literal braces can therefore pass
   startup validation while leaving real EC IDs unauthenticated.
2. Non-GET requests and malformed or trailing diagnostic paths enter the
   publisher fallback after successful authentication. That can forward the
   admin `Authorization` header and request body to the publisher origin.
3. Fastly dispatches the EIDs diagnostic through normal EC setup and attaches
   `EcFinalizeState`. Entry-point finalization can then ingest EID cookies and
   write to KV even though the diagnostic is documented as read-only.
4. Parseable KV bodies and metadata are displayed by serializing typed schema
   values. Unknown fields are dropped, and legacy representations such as
   map-shaped `seen_domains` are normalized instead of being shown as stored.
5. The operator-facing API is missing from the API reference.

## Goals

- Require valid Basic authentication for every recognized admin request at
  runtime, including concrete EC IDs.
- Reject invalid admin handler coverage during startup using a concrete EC ID
  probe while preserving router-template diagnostics in error messages.
- Ensure diagnostic requests never enter publisher fallback, regardless of
  supported method or malformed/trailing path shape.
- Keep `GET /_ts/admin/eids` read-only on Fastly by preventing all EC
  finalization state from being attached.
- Display all parseable KV entry and metadata JSON without dropping or
  normalizing stored fields.
- Preserve typed validation and auction derivation independently of the raw
  display representation.
- Document authentication, requests, responses, status codes, cache policy,
  and adapter limitations.
- Keep changes narrowly scoped to the new admin diagnostics.

## Non-goals

- Do not change authentication behavior for non-admin handler patterns.
- Do not change publisher fallback behavior outside the reserved admin
  namespace.
- Do not add EC lookup support to Axum, Cloudflare, or Spin.
- Do not change live auction EID resolution or cookie-ingestion semantics.
- Do not add a new KV abstraction solely to spy on Fastly writes in tests.
- Do not redesign the admin API payload beyond preserving stored JSON and
  documenting its existing derived fields.
- Do not push commits, reply to GitHub review threads, or resolve review
  conversations as part of implementation.

## Design

### 1. Parameter-aware startup authentication coverage

Keep the canonical admin route templates as the source used for coverage
errors and route-consistency tests. Define one canonical mapping from each
template to its concrete authentication probe. When testing whether a
configured handler covers `/_ts/admin/ec/{id}`, match the handler against a
fixed representative valid EC path instead of the literal template. The
representative ID will use fictional test data and satisfy the production
`{64hex}.{6alnum}` format.

All non-parameterized admin routes continue to use their canonical paths as
their coverage probes. A prefix handler such as `^/_ts/admin` therefore remains
valid, while a regex matching only literal braces is rejected at startup and
reported as failing to cover `/_ts/admin/ec/{id}`.

Use the same template-to-probe mapping everywhere settings validation decides
whether a handler protects an admin endpoint. This includes both uncovered
endpoint detection and placeholder-password rejection. A handler that protects
concrete EC IDs must therefore be recognized as an admin handler for credential
strength validation even when it does not match the literal router template.

### 2. Runtime authentication fails closed for admin paths

`enforce_basic_auth` currently treats a missing matching handler as meaning the
request is public. Add a shared admin-namespace classifier with a segment
boundary: it recognizes `/_ts/admin` and paths beginning `/_ts/admin/`, but not
similar publisher paths such as `/_ts/administrator`.

If no handler matches a recognized admin path, return a configuration error
instead of `Ok(None)`. Existing adapter middleware converts that error into a
local server response, so the request cannot reach a route handler or publisher
origin. If a handler matches, credential extraction and constant-time
comparison remain unchanged.

This runtime invariant is defense in depth. Startup validation catches known
route misconfiguration, while runtime classification also protects malformed,
trailing, and future admin paths.

### 3. Local denial before publisher fallback

Add a shared core classifier for the EC/EIDs diagnostic path family and use it
at the top of each adapter's fallback dispatcher, before integration or
publisher handling. This avoids a repetitive route matrix and covers path
shapes that a fixed route table can miss.

For the seven methods supported by publisher fallback (`GET`, `POST`, `HEAD`,
`OPTIONS`, `PUT`, `PATCH`, and `DELETE`), apply this contract after successful
authentication:

- A non-GET request to `/_ts/admin/ec`, a single-segment
  `/_ts/admin/ec/{id}`, or `/_ts/admin/eids` returns local `405 Method Not
  Allowed` with `Allow: GET`.
- A path with a trailing slash, an extra segment, a missing segment structure,
  or an EIDs suffix returns local `404 Not Found` and never reaches the
  publisher.
- Existing GET handling remains unchanged: Fastly validates an explicit EC ID
  and can return `400`, while portability adapters return their existing local
  `501` for the two EC lookup forms.

All denial responses use `Cache-Control: no-store`. Unsupported methods such as
`TRACE` continue to receive the router's local `405`; they are not registered
for publisher fallback and therefore cannot leak credentials upstream.

The guard is duplicated only at the four adapter fallback entry points. Path
classification and response construction remain shared so status, headers,
and behavior cannot drift.

### 4. Fastly EIDs dispatch skips EC setup and finalization

Handle `NamedRouteHandler::AdminEidsLookup` in `execute_named` before request
filters and `build_ec_request_state`, next to the existing early batch-sync
branch. Build only the partner registry, call `handle_admin_eids_lookup`, map
errors through the existing HTTP error conversion, and return the response
without calling `attach_dispatch_extensions`.

Basic authentication and standard response-header middleware remain outside
this dispatch function and continue to run. Because the returned response has
no `EcFinalizeState`, the Fastly entry point cannot call EC finalization,
`ingest_eid_cookies`, pull sync, or any EC KV write for this endpoint.

The normal `run_named_route` EIDs arm will become unreachable or be removed in
the smallest form that keeps the enum dispatch exhaustive and clear.

### 5. Separate raw display parsing from typed interpretation

For entry bodies, perform two independent parses:

1. Parse `lookup.body` as `serde_json::Value` for `payload.entry`.
2. Parse the same bytes as `KvEntry` only for tombstone calculation, schema
   validation, and auction derivation.

Add derived `created_iso` and `consent.updated_iso` fields directly to the raw
JSON object using its stored numeric timestamps. Never overwrite a stored field
with the same derived-field name. Unknown fields and legacy nested shapes stay
unchanged.

The entry outcomes are:

- Invalid JSON: omit `entry`; include `entry_error` and lossy UTF-8 `raw_body`;
  omit tombstone and auction.
- Valid JSON but invalid `KvEntry`: include the raw `entry`; include
  `entry_error`; omit tombstone and auction.
- Valid typed entry that fails validation: include raw `entry` and tombstone;
  include the validation error; omit auction.
- Valid and validated typed entry: include raw `entry`, tombstone, and the
  existing derived auction view.

For metadata, parse bytes as `serde_json::Value` for display and independently
as `KvMetadata` for existing schema diagnostics. Parseable raw metadata remains
visible even if typed metadata parsing reports an error. Invalid JSON remains
omitted and its raw/error detail stays in `metadata_error`.

Auction derivation continues to use `KvEntry`, `resolve_partner_ids`, and
`to_eids`, preserving production semantics. Live request consent remains a
documented limitation of the diagnostic view.

### 6. Operator API documentation

Add an Admin Diagnostic Endpoints section to
`docs/guide/api-reference.md` covering:

- Basic authentication and the sensitivity of returned data.
- `GET /_ts/admin/ec` with ID resolution from the `ts-ec` cookie.
- `GET /_ts/admin/ec/{id}` and its explicit ID format.
- EC success fields, raw/typed error outcomes, auction derivation, `400`,
  `401`, `404`, `405`, and `501` responses.
- Fastly-only EC lookup support and authenticated `501` responses from Axum,
  Cloudflare, and Spin.
- `GET /_ts/admin/eids`, its cookie inputs, always-`200` diagnostic payload,
  and support on every adapter.
- `Content-Type` and `Cache-Control: no-store` behavior.
- The three diagnostic paths in the protected-endpoint list.

Examples use only reserved domains and fictional IDs.

## Testing strategy

Implementation follows red-green-refactor, one behavior at a time.

### Core authentication and settings

- A template-only handler configuration fails startup coverage for the
  parameterized EC route.
- A concrete-ID handler with a placeholder password is still rejected as an
  admin handler through the shared template-to-probe mapping.
- A concrete valid EC request under that configuration cannot be treated as
  public by runtime auth.
- Existing broad and exact non-parameterized handler coverage remains valid.
- Similar non-admin prefixes remain public unless configured otherwise.

### Cross-adapter routing

For Fastly, Axum, Cloudflare, and Spin, authenticated requests verify:

- Wrong supported methods on the bare EC, single-ID EC, and EIDs routes return
  local `405` with `Allow: GET` and `no-store`.
- Trailing and extra-segment EC/EIDs paths return local `404` with `no-store`.
- Marker bodies and credentials do not reach publisher handling; deterministic
  local status provides the existing adapter test seam for this invariant.
- Valid GET behavior remains `200`/domain error on Fastly, `501` for EC lookup
  on portability adapters, and `200` for EIDs on every adapter.

### Fastly read-only behavior

An authenticated browser-shaped EIDs request carrying valid EC, EID, and
shared-ID cookies returns `200` without an `EcFinalizeState` response extension.
The Fastly entry point only performs EC writes when that extension is present,
so its absence proves the diagnostic cannot invoke the KV write path without
introducing test-only production seams.

### Raw KV display

- A parseable entry with unknown top-level and nested fields preserves those
  exact values.
- Legacy map-shaped `seen_domains` remains a map with its nested history data.
- Parseable metadata preserves unknown fields.
- Stored timestamps remain unchanged and ISO companions are added without
  overwriting stored collisions.
- Typed parsing still produces the expected auction view from the same entry.
- Valid JSON with an invalid typed schema remains visible with `entry_error`.

### Verification

After targeted tests pass, run the repository-required checks relevant to all
touched crates and documentation:

- `cargo fmt --all -- --check`
- `cargo test-fastly`
- `cargo test-axum`
- `cargo test-cloudflare`
- `cargo test-spin`
- `cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity`
- `cargo clippy-fastly`
- `cargo clippy-axum`
- `cargo clippy-cloudflare`
- `cargo clippy-cloudflare-wasm`
- `cargo clippy-spin-native`
- `cargo clippy-spin-wasm`
- `cd docs && npm run format`

JS sources are untouched; JS build, test, and format gates are not required for
the implementation-specific verification unless another change introduces a
JS dependency.

## Risks and mitigations

- **Overbroad runtime auth classification:** use an exact namespace segment
  boundary and add a similar-prefix regression.
- **Adapter behavior drift:** share path classification and denial response
  construction; keep only the fallback entry-point call adapter-local.
- **Route precedence regressions:** preserve named GET handlers and guard only
  requests that reached fallback.
- **Accidental raw-data normalization:** assert unknown fields and legacy
  structures on the final serialized handler response, not only helper values.
- **Fastly write regression:** assert the structural finalization gate is absent
  from the response.
