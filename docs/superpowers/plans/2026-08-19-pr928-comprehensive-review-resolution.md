# PR #928 Comprehensive Review Resolution Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Resolve all actionable PR #928 review findings while preserving existing authentication, cookie-ingestion, and adapter behavior outside the new admin diagnostics.

**Architecture:** Fastly handles both diagnostic routes before the EC lifecycle; core settings checks the parameterized admin route against a representative valid-ID corpus; core EID parsing exposes a source-preserving diagnostic view alongside unchanged production output. Shared core cookie parsing and admin JSON headers remove duplication and response drift, while adapter regressions and documentation pin the external contract.

**Tech Stack:** Rust 2024, `http`, `serde`, `serde_json`, `error-stack`, EdgeZero adapter routers, Fastly/Viceroy, Markdown/VitePress.

**Design spec:** `docs/superpowers/specs/2026-08-19-pr928-comprehensive-review-resolution-design.md`

---

## File Map

- Modify `crates/trusted-server-adapter-fastly/src/app.rs`: early EC diagnostic dispatch and Fastly authentication/finalization regressions.
- Modify `crates/trusted-server-core/src/settings.rs`: dynamic-route authentication probe corpus and settings regression.
- Modify `crates/trusted-server-core/src/ec/prebid_eids.rs`: source-preserving diagnostic parse view and shared UID-validity rule.
- Modify `crates/trusted-server-core/src/ec/admin.rs`: reason-tagged EID drops, shared cookie helper use, `nosniff`, and unit tests.
- Modify `crates/trusted-server-core/src/cookies.rs`: generic request-cookie extraction helper and focused tests.
- Modify `crates/trusted-server-core/src/auction/endpoints.rs`: use the shared cookie helper.
- Modify `crates/trusted-server-adapter-axum/tests/routes.rs`: unauthenticated diagnostic regression.
- Modify `crates/trusted-server-adapter-cloudflare/tests/routes.rs`: unauthenticated diagnostic regression.
- Modify `crates/trusted-server-adapter-spin/tests/routes.rs`: unauthenticated diagnostic regression.
- Modify `docs/guide/api-reference.md`: accurate authentication response contract and reason-tagged preview schema.
- Modify `CHANGELOG.md`: Unreleased configuration compatibility warning.

No dependency, public configuration schema, or test-only production seam is added.

### Task 1: Keep Fastly EC diagnostics outside the mutating lifecycle

**Files:**

- Modify: `crates/trusted-server-adapter-fastly/src/app.rs:527-610`
- Test: `crates/trusted-server-adapter-fastly/src/app.rs:2200-2250`

- [ ] **Step 1: Add the failing cookie-bearing EC diagnostic regression**

Add `admin_ec_diagnostic_skips_ec_finalization` next to the EIDs equivalent.
Build an authenticated `GET /_ts/admin/ec/{valid_id}` request carrying
`ts-ec`, base64-encoded `ts-eids`, and `sharedId`, plus browser-shaped
`DeviceSignals`. Assert the response has no `EcFinalizeState` and no
`Set-Cookie` header.

- [ ] **Step 2: Run the focused test and verify RED**

```bash
cargo test-fastly admin_ec_diagnostic_skips_ec_finalization
```

Expected: FAIL because the current response carries `EcFinalizeState`.

- [ ] **Step 3: Early-dispatch both diagnostic handlers**

Change the current `AdminEidsLookup` early branch to match both variants:

```rust
if matches!(
    handler,
    NamedRouteHandler::AdminEcLookup | NamedRouteHandler::AdminEidsLookup
) {
    let response = PartnerRegistry::from_config(&state.settings.ec.partners)
        .and_then(|registry| match handler {
            NamedRouteHandler::AdminEcLookup => {
                let kv = crate::maybe_identity_graph(&state.settings);
                handle_admin_ec_lookup(kv.as_ref(), &registry, &req)
            }
            NamedRouteHandler::AdminEidsLookup => handle_admin_eids_lookup(&registry, &req),
            _ => unreachable!("admin diagnostics should match an early-dispatch handler"),
        })
        .unwrap_or_else(|error| http_error(&error));
    return Ok(response);
}
```

Replace the later `AdminEcLookup` implementation arm with `unreachable!`, like
the EIDs arm. Update comments to describe both diagnostics.

- [ ] **Step 4: Run both finalization regressions and verify GREEN**

```bash
cargo test-fastly diagnostic_skips_ec_finalization
```

Expected: both EC and EIDs tests PASS.

- [ ] **Step 5: Commit Task 1**

```bash
git add crates/trusted-server-adapter-fastly/src/app.rs
git commit -m "Keep Fastly admin EC lookups read only"
```

### Task 2: Validate dynamic admin authentication with a suffix corpus

**Files:**

- Modify: `crates/trusted-server-core/src/settings.rs:2200-2300`
- Test: `crates/trusted-server-core/src/settings.rs:4960-5035`

- [ ] **Step 1: Add a failing mixed-case coverage regression**

Add `from_toml_rejects_lowercase_only_dynamic_admin_ec_auth_coverage`. Configure
the static admin routes with strong credentials and the parameterized route
with:

```toml
[[handlers]]
path = "^/_ts/admin/ec/[a-f0-9]{64}[.][a-z0-9]{6}$"
username = "admin"
password = "strong-test-password"
```

Assert `Settings::from_toml` returns a configuration error naming
`/_ts/admin/ec/{id}`.

- [ ] **Step 2: Run the regression and verify RED**

```bash
cargo test-fastly from_toml_rejects_lowercase_only_dynamic_admin_ec_auth_coverage
```

Expected: FAIL because `.abc123` is the only probe and the settings load.

- [ ] **Step 3: Replace the single probe with a fixed corpus**

Define lowercase and mixed-case concrete paths, and make the mapping return a
slice:

```rust
const ADMIN_EC_ID_AUTH_PROBES: &[&str] = &[
    concat!("/_ts/admin/ec/", "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", ".abc123"),
    concat!("/_ts/admin/ec/", "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", ".Ab12Z9"),
];

fn admin_auth_probes(path: &'static str) -> &'static [&'static str] {
    match path {
        "/_ts/admin/ec/{id}" => Self::ADMIN_EC_ID_AUTH_PROBES,
        path => core::slice::from_ref(&path),
    }
}
```

If `core::slice::from_ref` cannot produce the required static lifetime for the
match binding, use static one-element probe arrays for the non-parameterized
routes rather than allocating.

In `uncovered_admin_endpoints`, require every probe to match at least one
handler. In `validate_admin_handler_passwords`, classify a handler as admin
when it matches any probe for any canonical endpoint. Preserve canonical
template reporting.

- [ ] **Step 4: Run the focused and existing coverage tests**

```bash
cargo test-fastly dynamic_admin_ec_auth_coverage
cargo test-fastly literal_parameter_template_auth_coverage
cargo test-fastly placeholder_password_for_concrete_admin_ec_handler
cargo test-fastly uncovered_admin_endpoints
```

Expected: PASS.

- [ ] **Step 5: Commit Task 2**

```bash
git add crates/trusted-server-core/src/settings.rs
git commit -m "Validate mixed-case admin EC auth coverage"
```

### Task 3: Report reason-tagged EID ingestion drops

**Files:**

- Modify: `crates/trusted-server-core/src/ec/prebid_eids.rs:25-110,178-230,275-340`
- Modify: `crates/trusted-server-core/src/ec/admin.rs:410-515`
- Test: `crates/trusted-server-core/src/ec/prebid_eids.rs:400-620`
- Test: `crates/trusted-server-core/src/ec/admin.rs:1075-1185`

- [ ] **Step 1: Add failing admin preview regressions**

Update the existing unmatched assertion to expect:

```json
{ "source": "unknown.example", "reason": "no_partner" }
```

Add a test whose configured source has only whitespace/empty and over-limit
UID candidates; expect one `no_valid_uid` drop. Add a duplicate-source test
where one entry has no valid UID and a later entry has a valid UID; expect the
source in `matched` and no drop.

- [ ] **Step 2: Run the focused tests and verify RED**

```bash
cargo test-fastly eids_lookup_
```

Expected: existing string-shaped unmatched output fails and invalid-only
sources are absent.

- [ ] **Step 3: Refactor cookie decoding without changing production output**

In `prebid_eids.rs`, introduce a private decoded wire enum holding
`Vec<LegacyCookieEid>` or `Vec<StructuredCookieEid>`. Move size/base64/JSON
selection into one decoder. Keep `parse_prebid_eids_cookie` public and map the
decoded wire representation through the existing conversion functions so all
current parser tests remain unchanged.

Add a crate-visible `PrebidEidAnalysis` and analysis function that decode once
and derive all data the admin handler needs: the filtered `Vec<Eid>`, retained
diagnostic sources with raw UID strings, and `Vec<PartnerIdUpdate>` selected by
the live partner/UID rules. Make `collect_prebid_eid_updates` call the same
analysis function and extract only `updates`. Add or expose a crate-visible
predicate implementing the existing live rule:

```rust
pub(crate) fn is_valid_eid_uid(uid: &str) -> bool {
    !uid.trim().is_empty() && !eid_id_exceeds_size_limit(uid)
}
```

Make `first_valid_uid` call this predicate.

- [ ] **Step 4: Implement deterministic drop classification**

In `admin.rs`, replace `Vec<String>` with:

```rust
#[derive(Debug, Serialize)]
struct DroppedEidSource {
    source: String,
    reason: DroppedEidReason,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum DroppedEidReason {
    NoPartner,
    NoValidUid,
}
```

Group diagnostic sources in a `BTreeMap` for stable output. Emit one
`NoPartner` for an unconfigured source. For a configured source, emit one
`NoValidUid` only when no entry contains a UID satisfying
`is_valid_eid_uid`. Do not let `sharedId` suppress a `ts-eids` drop. Preserve
the existing matched-update collection and deduplication.

Replace the admin handler's separate `parse_prebid_eids_cookie` and
`collect_prebid_eid_updates` calls with one `analyze_prebid_eids_cookie` call.
On success, move its filtered EIDs into the response, classify its diagnostic
sources, and extend the matched-update list from its updates. On failure, set
the existing `parse_error` and produce no EIDs, drops, or Prebid updates.

- [ ] **Step 5: Run parser, preview, and ingestion tests**

```bash
cargo test-fastly prebid_eids
cargo test-fastly eids_lookup_
```

Expected: PASS, including unchanged live-ingestion cases.

- [ ] **Step 6: Commit Task 3**

```bash
git add crates/trusted-server-core/src/ec/prebid_eids.rs crates/trusted-server-core/src/ec/admin.rs
git commit -m "Explain dropped admin EID preview sources"
```

### Task 4: Consolidate core request-cookie extraction

**Files:**

- Modify: `crates/trusted-server-core/src/cookies.rs:1-120`
- Modify: `crates/trusted-server-core/src/ec/admin.rs:20-45,225-245,445-525`
- Modify: `crates/trusted-server-core/src/auction/endpoints.rs:1-35,240-255,400-430`
- Test: `crates/trusted-server-core/src/cookies.rs`

- [ ] **Step 1: Add focused helper tests**

Add tests for missing header, whitespace trimming, a value containing `=`, and
multiple cookie pairs. Add a request with two Cookie header values where the
selected `headers().get` value is invalid UTF-8 and assert `None`, pinning the
old helper semantics rather than scanning later values.

- [ ] **Step 2: Run the focused test and verify RED**

```bash
cargo test-fastly extract_cookie_value
```

Expected: compilation FAIL because the shared helper does not exist.

- [ ] **Step 3: Add the generic shared helper**

Add a documented crate-public function in `cookies.rs`:

```rust
#[must_use]
pub fn extract_cookie_value<B>(req: &Request<B>, name: &str) -> Option<String> {
    let cookie_header = req.headers().get(header::COOKIE)?.to_str().ok()?;
    cookie_header.split(';').find_map(|pair| {
        let (key, value) = pair.trim().split_once('=')?;
        (key.trim() == name).then(|| value.trim().to_owned())
    })
}
```

- [ ] **Step 4: Migrate both core callers and delete local copies**

Import `crate::cookies::extract_cookie_value` in `ec/admin.rs` and
`auction/endpoints.rs`. Remove their byte-identical local helpers and any
imports made unused.

- [ ] **Step 5: Run focused caller tests**

```bash
cargo test-fastly extract_cookie_value
cargo test-fastly eids_lookup_
cargo test-fastly auction
```

Expected: PASS.

- [ ] **Step 6: Commit Task 4**

```bash
git add crates/trusted-server-core/src/cookies.rs crates/trusted-server-core/src/ec/admin.rs crates/trusted-server-core/src/auction/endpoints.rs
git commit -m "Share core request cookie extraction"
```

### Task 5: Harden diagnostic JSON and pin adapter authentication

**Files:**

- Modify: `crates/trusted-server-core/src/ec/admin.rs:90-130,530-550`
- Modify: `crates/trusted-server-adapter-fastly/src/app.rs:1800-1875`
- Modify: `crates/trusted-server-adapter-axum/tests/routes.rs:245-325`
- Modify: `crates/trusted-server-adapter-cloudflare/tests/routes.rs:275-330`
- Modify: `crates/trusted-server-adapter-spin/tests/routes.rs:105-165`

- [ ] **Step 1: Add failing `nosniff` response assertions**

In core admin tests, assert representative success and JSON error responses
contain `X-Content-Type-Options: nosniff`.

- [ ] **Step 2: Run the focused test and verify RED**

```bash
cargo test-fastly admin_ec_lookup
```

Expected: FAIL because the header is absent.

- [ ] **Step 3: Add the shared response header and correct field docs**

Add `.header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")` to
`json_response`. Update `tombstone` documentation to say it is absent when the
body fails JSON parsing or typed `KvEntry` deserialization.

- [ ] **Step 4: Add one new-route unauthenticated test per adapter**

For Fastly, Axum, Cloudflare, and Spin, send `GET /_ts/admin/ec` without an
Authorization header. Assert `401 Unauthorized` and the existing Basic
`WWW-Authenticate` realm. Place each test next to the adapter's authenticated
EC diagnostic test and reuse its established router/service helper.

- [ ] **Step 5: Run each adapter's focused authentication test**

```bash
cargo test-fastly admin_ec_route_without_credentials
cargo test-axum admin_ec_route_without_credentials
cargo test-cloudflare admin_ec_route_without_credentials
cargo test-spin admin_ec_route_without_credentials
```

Expected: PASS.

- [ ] **Step 6: Commit Task 5**

```bash
git add crates/trusted-server-core/src/ec/admin.rs crates/trusted-server-adapter-fastly/src/app.rs crates/trusted-server-adapter-axum/tests/routes.rs crates/trusted-server-adapter-cloudflare/tests/routes.rs crates/trusted-server-adapter-spin/tests/routes.rs
git commit -m "Harden admin diagnostic responses"
```

### Task 6: Correct operator-facing documentation

**Files:**

- Modify: `docs/guide/api-reference.md:580-640`
- Modify: `CHANGELOG.md:8-25`

- [ ] **Step 1: Qualify authentication response behavior**

State that successfully authenticated diagnostic-handler responses are JSON
with `Cache-Control: no-store`, while missing or invalid credentials receive
the shared plaintext `401 Unauthorized` Basic challenge.

- [ ] **Step 2: Document reason-tagged EID drops**

Describe `ingest.unmatched` entries as `{source, reason}` objects and define
`no_partner` and `no_valid_uid`. Include a compact example covering one match
and one drop.

- [ ] **Step 3: Add the Unreleased compatibility warning**

Under `CHANGELOG.md`'s Unreleased Changed section, state that startup now
requires authenticated handler coverage for the EC/EID diagnostics in addition
to key management. Tell operators with narrow key-only patterns to broaden
coverage before deploying, preferably to `^/_ts/admin(?:/|$)`.

- [ ] **Step 4: Format documentation**

```bash
cd docs && npm run format
```

Expected: formatter exits 0 with only intended Markdown changes.

- [ ] **Step 5: Commit Task 6**

```bash
git add docs/guide/api-reference.md CHANGELOG.md
git commit -m "Clarify admin diagnostics contracts"
```

### Task 7: Full verification

**Files:**

- Verify all modified files.

- [ ] **Step 1: Check formatting**

```bash
cargo fmt --all -- --check
```

Expected: PASS.

- [ ] **Step 2: Run adapter and core test suites**

```bash
cargo test-fastly
cargo test-axum
cargo test-cloudflare
cargo test-spin
```

Expected: all PASS.

- [ ] **Step 3: Run target-matched clippy checks**

```bash
cargo clippy-fastly
cargo clippy-axum
cargo clippy-cloudflare
cargo clippy-cloudflare-wasm
cargo clippy-spin-native
cargo clippy-spin-wasm
```

Expected: all PASS with warnings denied by repository aliases.

- [ ] **Step 4: Verify documentation and diff hygiene**

```bash
cd docs && npm run format
git diff --check
git status --short
```

Expected: formatter and diff check PASS; status shows only intentional plan or
implementation state.

- [ ] **Step 5: Review the branch diff against the PR base**

```bash
git diff --stat origin/main...HEAD
git diff origin/main...HEAD -- crates/trusted-server-adapter-fastly/src/app.rs crates/trusted-server-core/src/settings.rs crates/trusted-server-core/src/ec/admin.rs crates/trusted-server-core/src/ec/prebid_eids.rs crates/trusted-server-core/src/cookies.rs crates/trusted-server-core/src/auction/endpoints.rs docs/guide/api-reference.md CHANGELOG.md
```

Expected: every change maps to the approved spec; no unrelated refactor or
behavior change is present.
