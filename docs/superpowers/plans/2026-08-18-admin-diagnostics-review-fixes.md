# Admin Diagnostics Review Fixes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close every PR #928 review finding by making admin diagnostics fail closed, preventing publisher/KV side effects, preserving raw KV JSON, and documenting the API.

**Architecture:** Core settings owns the canonical admin-template-to-auth-probe mapping and runtime admin namespace classification. Core EC admin code owns one shared fallback-denial response so each adapter only adds a small guard at its publisher fallback boundary. Fastly dispatches the read-only EIDs diagnostic before EC setup, while raw JSON display and typed interpretation remain separate inside the core handler.

**Tech Stack:** Rust 2024, `http`, `serde_json`, `error-stack`, EdgeZero adapter routers, Fastly/Viceroy, Markdown documentation.

**Design spec:** `docs/superpowers/specs/2026-08-18-admin-diagnostics-review-fixes-design.md`

---

## File map

- Modify `crates/trusted-server-core/src/settings.rs`: canonical admin route/auth
  probes, admin namespace classification, startup validation, and tests.
- Modify `crates/trusted-server-core/src/auth.rs`: runtime fail-closed behavior
  and regression tests.
- Modify `crates/trusted-server-core/src/ec/admin.rs`: shared diagnostic
  fallback denial, lossless raw JSON display, and unit tests.
- Modify `crates/trusted-server-adapter-fastly/src/app.rs`: fallback guard,
  early read-only EIDs dispatch, and adapter tests.
- Modify each portability adapter's `src/app.rs` and `tests/routes.rs`: fallback
  guard and cross-adapter route regressions.
- Modify `docs/guide/api-reference.md`: operator-facing contract.

No new crate, dependency, schema type, or test-only production seam is needed.

### Task 1: Make admin authentication coverage parameter-aware and fail closed

**Files:**

- Modify: `crates/trusted-server-core/src/settings.rs:2194-2279`
- Modify: `crates/trusted-server-core/src/settings.rs:4876-4970`
- Modify: `crates/trusted-server-core/src/auth.rs:29-55`
- Test: `crates/trusted-server-core/src/auth.rs:79-315`

- [ ] **Step 1: Add a failing literal-template startup regression**

Build settings TOML whose first handler covers the four non-parameterized admin
routes and whose second handler covers only literal braces:

```rust
[[handlers]]
path = "^/_ts/admin/(keys/rotate|keys/deactivate|ec|eids)$"
username = "admin"
password = "strong-test-password"

[[handlers]]
path = "^/_ts/admin/ec/[{]id[}]$"
username = "admin"
password = "strong-test-password"
```

Assert `Settings::from_toml` fails and identifies `/_ts/admin/ec/{id}` as
uncovered.

- [ ] **Step 2: Run the regression and verify RED**

```bash
cargo test-fastly from_toml_rejects_literal_parameter_template_auth_coverage
```

Expected: FAIL because current validation accepts the literal template match.

- [ ] **Step 3: Add and run a failing concrete-handler password regression**

Add settings with a concrete-ID handler regex
`^/_ts/admin/ec/[a-f0-9]{64}[.][a-z0-9]{6}$` and placeholder password
`change-me-admin-password`. Assert finalization rejects it as an admin handler.

```bash
cargo test-fastly from_toml_rejects_placeholder_password_for_concrete_admin_ec_handler
```

Expected: FAIL because password validation also uses the literal template.

- [ ] **Step 4: Implement one canonical template-to-auth-probe mapping**

Keep `Settings::ADMIN_ENDPOINTS` as canonical templates. Add a fixed fictional
valid EC probe and a helper used by both coverage and password validation:

```rust
const ADMIN_EC_ID_AUTH_PROBE: &str = concat!(
    "/_ts/admin/ec/",
    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    ".abc123",
);

fn admin_auth_probe(path: &'static str) -> &'static str {
    match path {
        "/_ts/admin/ec/{id}" => ADMIN_EC_ID_AUTH_PROBE,
        path => path,
    }
}
```

Make `uncovered_admin_endpoints` report the template but match its probe. Make
`validate_admin_handler_passwords` use the same helper. Update stale comments.

- [ ] **Step 5: Run both settings regressions and verify GREEN**

```bash
cargo test-fastly literal_parameter_template_auth_coverage
cargo test-fastly placeholder_password_for_concrete_admin_ec_handler
```

Expected: PASS.

- [ ] **Step 6: Add failing runtime fail-closed auth tests**

In `auth.rs`, deserialize settings directly with `toml::from_str` to bypass
startup finalization. With the literal-template-only configuration, send a
concrete valid EC request and assert `enforce_basic_auth` returns a configuration
error rather than `Ok(None)`. Add `/_ts/administrator` as a boundary case that
must remain public when no handler matches.

- [ ] **Step 7: Run runtime tests and verify RED**

```bash
cargo test-fastly concrete_admin_path_without_matching_handler_fails_closed
```

Expected: FAIL because current auth returns `Ok(None)`.

- [ ] **Step 8: Implement the runtime namespace invariant**

Add:

```rust
#[must_use]
pub fn is_admin_path(path: &str) -> bool {
    path == "/_ts/admin" || path.starts_with("/_ts/admin/")
}
```

When `handler_for_path` returns `None`, make `enforce_basic_auth` return
`TrustedServerError::Configuration` for an admin path and retain `Ok(None)` for
all other paths.

- [ ] **Step 9: Run core tests and target-matched suite**

```bash
cargo test-fastly admin_path
cargo test-fastly uncovered_admin_endpoints
cargo test-fastly
```

Expected: PASS without warnings.

- [ ] **Step 10: Commit Task 1**

```bash
git add crates/trusted-server-core/src/settings.rs crates/trusted-server-core/src/auth.rs
git commit -m "Fail closed for concrete admin routes"
```

### Task 2: Add a shared local denial for diagnostic fallback requests

**Files:**

- Modify: `crates/trusted-server-core/src/ec/admin.rs:20-45`
- Modify: `crates/trusted-server-core/src/ec/admin.rs:432-464`
- Test: `crates/trusted-server-core/src/ec/admin.rs:464-925`

- [ ] **Step 1: Add failing table-driven fallback tests**

Test a new
`deny_admin_diagnostic_fallback(&Request<EdgeBody>) -> Option<Response<EdgeBody>>`
helper. For bare EC, single-ID EC, and EIDs, every non-GET publisher fallback
method must return `405`, `Allow: GET`, and `Cache-Control: no-store`. GET and
non-GET requests to trailing/extra-segment EC/EIDs forms must return `404` and
`no-store`. An unrelated publisher path must return `None`.

- [ ] **Step 2: Run focused tests and verify RED**

```bash
cargo test-fastly admin_diagnostic_fallback
```

Expected: compilation FAIL because the helper does not exist.

- [ ] **Step 3: Implement classification and response construction**

Add a private path-shape classifier and documented public helper. Its core
response logic is:

```rust
let mut response = if shape.is_valid_resource() && req.method() != Method::GET {
    json_error(StatusCode::METHOD_NOT_ALLOWED, "method not allowed")
} else {
    json_error(StatusCode::NOT_FOUND, "admin diagnostic route not found")
};
if response.status() == StatusCode::METHOD_NOT_ALLOWED {
    response
        .headers_mut()
        .insert(header::ALLOW, HeaderValue::from_static("GET"));
}
```

Reuse `json_error`/`json_response`, which already set JSON and `no-store`.
Classify only the EC/EIDs families. EC bare and exactly one non-empty ID segment
are valid resource shapes; EIDs exact is valid; suffix/trailing forms are
malformed. A valid GET that somehow reaches fallback returns local `404`.

- [ ] **Step 4: Run focused and target-matched tests**

```bash
cargo test-fastly admin_diagnostic_fallback
cargo test-fastly
```

Expected: PASS.

- [ ] **Step 5: Commit Task 2**

```bash
git add crates/trusted-server-core/src/ec/admin.rs
git commit -m "Deny admin diagnostics in publisher fallback"
```

### Task 3: Wire the denial guard into every adapter

**Files:**

- Modify/Test: `crates/trusted-server-adapter-fastly/src/app.rs`
- Modify: `crates/trusted-server-adapter-axum/src/app.rs`
- Test: `crates/trusted-server-adapter-axum/tests/routes.rs`
- Modify: `crates/trusted-server-adapter-cloudflare/src/app.rs`
- Test: `crates/trusted-server-adapter-cloudflare/tests/routes.rs`
- Modify: `crates/trusted-server-adapter-spin/src/app.rs`
- Test: `crates/trusted-server-adapter-spin/tests/routes.rs`

- [ ] **Step 1: Add Fastly adapter regressions and verify RED**

Using authenticated router requests, test the valid diagnostic shapes against
`POST`, `HEAD`, `OPTIONS`, `PUT`, `PATCH`, and `DELETE`, asserting `405`,
`Allow: GET`, and `no-store`. Test authenticated GET and POST requests for
trailing/extra-segment forms, asserting `404` and `no-store`.

```bash
cargo test-fastly authenticated_admin_diagnostic_fallback
```

Expected: FAIL because requests enter publisher fallback.

- [ ] **Step 2: Wire Fastly and verify GREEN**

Import the helper and call it at the start of `dispatch_fallback`, before GPT
preparation, filters, integration routing, or publisher handling:

```rust
if let Some(response) = deny_admin_diagnostic_fallback(&req) {
    return response;
}
```

```bash
cargo test-fastly authenticated_admin_diagnostic_fallback
cargo test-fastly
```

Expected: PASS.

- [ ] **Step 3: Add Axum regressions and verify RED**

Add the same matrices in `tests/routes.rs` using `make_service()`.

```bash
cargo test-axum authenticated_admin_diagnostic_fallback
```

Expected: FAIL.

- [ ] **Step 4: Wire Axum and verify GREEN**

Call the helper at the start of Axum's fallback `dispatch` before publisher
handling.

```bash
cargo test-axum authenticated_admin_diagnostic_fallback
cargo test-axum
```

Expected: PASS.

- [ ] **Step 5: Add Cloudflare regressions and verify RED**

Use `request_builder()` plus `route(test_router(), req)`.

```bash
cargo test-cloudflare authenticated_admin_diagnostic_fallback
```

Expected: FAIL.

- [ ] **Step 6: Wire Cloudflare and verify GREEN**

Call the helper before Cloudflare integration/publisher dispatch.

```bash
cargo test-cloudflare authenticated_admin_diagnostic_fallback
cargo test-cloudflare
```

Expected: PASS.

- [ ] **Step 7: Add Spin regressions and verify RED**

Use the existing Spin router helpers with the same matrices.

```bash
cargo test-spin authenticated_admin_diagnostic_fallback
```

Expected: FAIL.

- [ ] **Step 8: Wire Spin and verify GREEN**

Call the helper before Spin integration/publisher dispatch.

```bash
cargo test-spin authenticated_admin_diagnostic_fallback
cargo test-spin
```

Expected: PASS.

- [ ] **Step 9: Commit Task 3**

```bash
git add crates/trusted-server-adapter-fastly/src/app.rs \
  crates/trusted-server-adapter-axum/src/app.rs \
  crates/trusted-server-adapter-axum/tests/routes.rs \
  crates/trusted-server-adapter-cloudflare/src/app.rs \
  crates/trusted-server-adapter-cloudflare/tests/routes.rs \
  crates/trusted-server-adapter-spin/src/app.rs \
  crates/trusted-server-adapter-spin/tests/routes.rs
git commit -m "Keep admin diagnostics out of publisher fallback"
```

### Task 4: Make Fastly EIDs diagnostics structurally read-only

**Files:**

- Modify/Test: `crates/trusted-server-adapter-fastly/src/app.rs:501-595`
- Reference: `crates/trusted-server-adapter-fastly/src/main.rs:184-247`
- Reference: `crates/trusted-server-core/src/ec/finalize.rs:77-106`

- [ ] **Step 1: Add and run a failing finalization-state regression**

Create an authenticated browser-shaped `GET /_ts/admin/eids` request with valid
EC, EID, and shared-ID cookies. Assert `200` and:

```rust
assert!(
    response.extensions().get::<EcFinalizeState>().is_none(),
    "admin EIDs diagnostics should not attach EC finalization state"
);
```

```bash
cargo test-fastly admin_eids_diagnostic_skips_ec_finalization
```

Expected: FAIL because `execute_named` attaches `EcFinalizeState`.

- [ ] **Step 2: Add the early EIDs dispatch**

Before GPT preparation or EC setup, build the registry, call the handler, map
errors through `http_error`, and return without `attach_dispatch_extensions`:

```rust
if matches!(handler, NamedRouteHandler::AdminEidsLookup) {
    let result = PartnerRegistry::from_config(&state.settings.ec.partners)
        .and_then(|registry| handle_admin_eids_lookup(&registry, &req));
    return Ok(result.unwrap_or_else(|error| http_error(&error)));
}
```

Make the normal route arm explicitly unreachable or remove it cleanly. Update
module lifecycle comments.

- [ ] **Step 3: Run focused and Fastly suites**

```bash
cargo test-fastly admin_eids_diagnostic_skips_ec_finalization
cargo test-fastly
```

Expected: PASS.

- [ ] **Step 4: Commit Task 4**

```bash
git add crates/trusted-server-adapter-fastly/src/app.rs
git commit -m "Keep admin EID diagnostics read only"
```

### Task 5: Preserve raw parseable KV entries and metadata

**Files:**

- Modify/Test: `crates/trusted-server-core/src/ec/admin.rs:47-278`
- Modify/Test: `crates/trusted-server-core/src/ec/admin.rs:464-925`

- [ ] **Step 1: Add and run a failing lossless-display regression**

Seed a valid raw entry with unknown top-level, consent, and partner fields;
legacy map-shaped `seen_domains`; valid auction partner data; and metadata with
an unknown field. Assert the final response preserves all raw values and shape,
keeps numeric timestamps, adds ISO companions, preserves metadata, and still
derives auction EIDs.

```bash
cargo test-fastly parseable_legacy_entry_and_metadata_preserve_raw_json
```

Expected: FAIL because typed reserialization drops and normalizes data.

- [ ] **Step 2: Add and run failing schema/collision tests**

For valid JSON that cannot deserialize as `KvEntry`, assert raw `entry` remains
present with `entry_error` and no `raw_body`. For stored `created_iso` and
`consent.updated_iso`, assert neither is overwritten.

```bash
cargo test-fastly valid_json_with_invalid_kv_schema_remains_visible
cargo test-fastly stored_iso_fields_are_not_overwritten
```

Expected: FAIL.

- [ ] **Step 3: Separate raw display from typed interpretation**

Parse `lookup.body` as `JsonValue` for `payload.entry`, then independently as
`KvEntry` for tombstone, validation, and auction. Invalid JSON sets
`entry_error` plus lossy `raw_body`; valid JSON with an invalid schema remains
visible and sets only `entry_error`.

Replace `entry_json_with_iso_timestamps(&KvEntry)` with a helper accepting
`&mut JsonValue`. Read raw `created` and `consent.updated` as `u64`, and use
`entry(...).or_insert(...)` for ISO companions so stored collisions win.

Parse metadata independently as `JsonValue` for display and `KvMetadata` only
for diagnostics. Typed metadata failure must not erase parseable raw metadata;
invalid JSON remains in `metadata_error`.

- [ ] **Step 4: Run admin and Fastly suites**

```bash
cargo test-fastly ec::admin::tests
cargo test-fastly
```

Expected: PASS, including existing corrupt-entry, validation, timestamp, and
auction tests.

- [ ] **Step 5: Commit Task 5**

```bash
git add crates/trusted-server-core/src/ec/admin.rs
git commit -m "Preserve raw admin EC diagnostic records"
```

### Task 6: Document the operator API contract

**Files:**

- Modify: `docs/guide/api-reference.md:1-20`
- Modify: `docs/guide/api-reference.md:497-590`
- Modify: `docs/guide/api-reference.md:746-772`

- [ ] **Step 1: Add the Admin Diagnostic Endpoints section**

Document Basic Auth and sensitive data; both EC lookup forms; response fields
and the raw/typed error matrix; `401`, `400`, `404`, `405`, and `501`; Fastly
support and portability `501`; EIDs cookie inputs, payload, always-`200`
post-auth semantics, and all-adapter support; JSON/no-store behavior; `Allow:
GET`; and the live-consent limitation. Use only fictional/example data.

- [ ] **Step 2: Update navigation and protected endpoints**

Add the section to the API category list and the three routes to Protected
Endpoints.

- [ ] **Step 3: Format and inspect documentation**

```bash
cd docs && npm run format
git diff --check
git diff -- docs/guide/api-reference.md
```

Expected: formatting passes, no whitespace errors, and the contract matches
implemented statuses and headers.

- [ ] **Step 4: Commit Task 6**

```bash
git add docs/guide/api-reference.md
git commit -m "Document admin EC and EID diagnostics"
```

### Task 7: Verify the complete review resolution

**Files:** Verify all files changed by Tasks 1-6.

- [ ] **Step 1: Run formatting**

```bash
cargo fmt --all -- --check
cd docs && npm run format
```

Expected: PASS.

- [ ] **Step 2: Run all adapter tests**

```bash
cargo test-fastly
cargo test-axum
cargo test-cloudflare
cargo test-spin
```

Expected: PASS.

- [ ] **Step 3: Run parity tests**

```bash
cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity
```

Expected: PASS.

- [ ] **Step 4: Run all target-matched lint gates**

```bash
cargo clippy-fastly
cargo clippy-axum
cargo clippy-cloudflare
cargo clippy-cloudflare-wasm
cargo clippy-spin-native
cargo clippy-spin-wasm
```

Expected: PASS with `-D warnings`.

- [ ] **Step 5: Inspect final branch state**

```bash
git diff main...HEAD --check
git status --short
git log --oneline --decorate -10
```

Expected: no uncommitted implementation changes and a focused commit sequence.

- [ ] **Step 6: Request code review**

Invoke `superpowers:requesting-code-review` with the approved spec and plan.
Address only verified findings and rerun affected tests after corrections.

- [ ] **Step 7: Verify before completion**

Invoke `superpowers:verification-before-completion`, confirm fresh output for
every claimed gate, and report environmental limitations rather than claiming
success.

- [ ] **Step 8: Prepare review-thread resolution notes**

Map each of the five findings to its implementing commit and test evidence. Do
not post or resolve GitHub threads without separate user authorization.
