# Didomi Geo Forwarding Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add trusted country and region parameters to Didomi notice-loader URLs with browser-safe cache partitioning on Fastly.

**Architecture:** Add an opt-in field to the typed Didomi integration configuration. For eligible loader requests, resolve Fastly geo through `RuntimeServices`, canonicalize the query with authoritative `country` and `region` values, redirect geo-less or non-canonical browser URLs to that same-origin canonical URL, and proxy canonical requests to Didomi. Preserve Didomi SDK cache headers, bypass cache for Didomi API requests, and fail closed when complete trusted geo is unavailable.

**Tech Stack:** Rust 2024, `edgezero_core`, `http`, `url`, `serde`, `validator`, Fastly Compute, repository test aliases, Markdown/Prettier.

---

### Task 1: Add and round-trip the opt-in configuration

**Files:**

- Modify: `crates/trusted-server-core/src/integrations/didomi.rs:24-96,365-654`
- Modify: `crates/trusted-server-core/src/config_payload.rs:50-150`

- [x] **Step 1: Write failing configuration tests**

Add tests proving that an omitted `geo_query_parameters` field is `false`, an explicit `true` is retained by `Settings::integration_config::<DidomiIntegrationConfig>()`, and `true` survives `Settings` serialization through `BlobEnvelope` and `settings_from_config_blob`.

The Didomi parsing test should use the real settings path:

```rust
let settings = Settings::from_toml(&format!(
    "{}\n[integrations.didomi]\nenabled = true\ngeo_query_parameters = true\n",
    crate_test_settings_str()
))
.expect("should parse Didomi geo configuration");
let config = settings
    .integration_config::<DidomiIntegrationConfig>(DIDOMI_INTEGRATION_ID)
    .expect("should read Didomi configuration")
    .expect("should enable Didomi");
assert!(config.geo_query_parameters, "should retain geo opt-in");
```

- [x] **Step 2: Run the focused tests and verify RED**

Run:

```bash
cargo test --package trusted-server-core --target aarch64-apple-darwin didomi
cargo test --package trusted-server-core --target aarch64-apple-darwin config_payload
```

Expected: compilation or assertions fail because `DidomiIntegrationConfig` has no `geo_query_parameters` field.

- [x] **Step 3: Add the configuration field**

Add the field without a custom default function so missing values deserialize to `false`:

```rust
/// Add trusted country and region parameters to notice-loader URLs.
#[serde(default)]
pub geo_query_parameters: bool,
```

Update every `DidomiIntegrationConfig` literal in tests. Keep the default test helper disabled and add a small helper or explicit assignment for enabled geo tests.

- [x] **Step 4: Run the focused tests and verify GREEN**

Run the two commands from Step 2. Expected: all matching tests pass.

- [x] **Step 5: Commit the configuration slice**

```bash
git add crates/trusted-server-core/src/integrations/didomi.rs crates/trusted-server-core/src/config_payload.rs
git commit -m "Add Didomi geo forwarding configuration"
```

### Task 2: Define loader matching, geo normalization, and canonical query behavior

**Files:**

- Modify: `crates/trusted-server-core/src/integrations/didomi.rs:98-229,365-654`

- [x] **Step 1: Write failing pure-behavior tests**

Add separate tests for:

- matching exactly `/<public-key>/loader.js` and rejecting missing keys, API paths, POST handling inputs, suffixes, trailing slashes, and extra segments;
- trimming and uppercasing `us` / `ca` to `US` / `CA`;
- accepting `US-CA` and removing only the matching `US-` prefix;
- rejecting missing region, mismatched prefixes, `XX`/`ZZ`, non-ASCII country, and region characters outside one to three ASCII alphanumerics;
- removing all case-insensitive and percent-decoded `country`/`region` pairs;
- preserving unrelated decoded pairs, order, duplicates, and empty values;
- appending exactly `country` then `region` and producing an idempotent canonical path/query.

Use a table for invalid paths and geo pairs. Include a query such as:

```text
target_type=notice&x=1&Country=gb&%72egion=lnd&x=2&empty=&space=a+b&plus=%2B
```

and expect canonical WHATWG form serialization with only `country=US&region=CA` as the final geo pairs.

- [x] **Step 2: Run the Didomi tests and verify RED**

Run `cargo test --package trusted-server-core --target aarch64-apple-darwin didomi`. Expected: compilation fails because the matcher, normalized geo type, and canonicalization helpers do not exist.

- [x] **Step 3: Implement private pure helpers**

Add a private normalized pair:

```rust
#[derive(Debug, Clone, Eq, PartialEq)]
struct DidomiGeo {
    country: String,
    region: String,
}
```

Implement private helpers that:

1. recognize the exact loader shape from `consent_path`;
2. normalize `GeoInfo.country` and `GeoInfo.region` under the spec rules;
3. rebuild the browser path/query using `url::form_urlencoded::parse` and `Serializer`;
4. return both the relative canonical browser target and canonical query used for the upstream URL.

Compare geo names with `eq_ignore_ascii_case` after form decoding. Do not add an ISO registry dependency or decode the path a second time.

- [x] **Step 4: Run the Didomi tests and verify GREEN**

Run `cargo test --package trusted-server-core --target aarch64-apple-darwin didomi`. Expected: all Didomi tests pass.

- [x] **Step 5: Commit the pure canonicalization slice**

```bash
git add crates/trusted-server-core/src/integrations/didomi.rs
git commit -m "Canonicalize Didomi loader geo parameters"
```

### Task 3: Redirect non-canonical loaders and proxy canonical loaders

**Files:**

- Modify: `crates/trusted-server-core/src/integrations/didomi.rs:153-332,365-654`

- [x] **Step 1: Add request-level failing tests**

Create a local `PlatformGeo` stub in the Didomi test module and build `RuntimeServices` with the existing `StubHttpClient` and `StubBackend`. Add tests proving:

- disabled mode preserves the existing geo-less upstream request;
- enabled mode redirects a geo-less eligible loader with status 307, a relative `Location`, and `Cache-Control: private, no-store` without calling the HTTP client;
- caller-provided mixed-case or duplicate geo is replaced in the redirect;
- a canonical loader makes exactly one SDK request with the same canonical query;
- canonical loader headers `X-Geo-Country`, `X-Geo-Region`, and `CloudFront-Viewer-Country` all come from normalized platform geo, even when conflicting Fastly-style headers are supplied by the request;
- an unrelated SDK asset retains the existing behavior;
- unavailable, failed, invalid, and country-only geo return 503, remain private/no-store, and do not call upstream.

Queue a stub response only for canonical proxy tests. Assert call count through `recorded_backend_names`, the URI through `recorded_request_uris`, and outbound headers through `recorded_request_headers`.

- [x] **Step 2: Run request-level tests and verify RED**

Run `cargo test --package trusted-server-core --target aarch64-apple-darwin didomi`. Expected: redirect, authoritative-header, or failure assertions fail because `handle` still proxies every request directly.

- [x] **Step 3: Implement the loader decision in `handle`**

Before backend registration or request body collection:

1. check `geo_query_parameters`, `GET`, SDK backend, and exact loader shape;
2. call `services.geo().lookup(services.client_info().client_ip)`;
3. return a generic terminal-private 503 on lookup error or incomplete/invalid geo;
4. construct the canonical relative target;
5. return a terminal-private 307 with the relative `Location` when the incoming path/query is non-canonical;
6. carry the normalized pair into canonical SDK header construction and use its canonical query for `build_target_url`.

Use `crate::response_privacy::enforce_terminal_private_cache_privacy` on generated redirects and failures so operator response headers cannot restore shared caching. Log only the integration name and a bounded reason such as `lookup_failed`, `missing_region`, or `invalid_geo`.

Refactor `copy_headers` to accept an optional authoritative `DidomiGeo`. On an enabled canonical loader, set all three compatibility headers from that pair. On other SDK requests, retain the existing header-copy behavior.

- [x] **Step 4: Run the Didomi tests and verify GREEN**

Run `cargo test --package trusted-server-core --target aarch64-apple-darwin didomi`. Expected: all request-level and existing Didomi tests pass.

- [x] **Step 5: Commit the request behavior**

```bash
git add crates/trusted-server-core/src/integrations/didomi.rs
git commit -m "Forward trusted geo to Didomi loaders"
```

### Task 4: Enforce the Didomi API no-cache contract

**Files:**

- Modify: `crates/trusted-server-core/src/integrations/didomi.rs:278-332,365-654`

- [x] **Step 1: Write failing API cache tests**

Add tests proving an API request records `with_cache_bypass()` and its response is terminal-private with `Cache-Control: private, no-store` after upstream `Cache-Control`, `Expires`, `ETag`, `Last-Modified`, `Age`, `Surrogate-Control`, and `CDN-Cache-Control` headers are supplied. Add a paired SDK test proving the same origin cache headers remain unchanged there.

- [x] **Step 2: Run the tests and verify RED**

Run `cargo test --package trusted-server-core --target aarch64-apple-darwin didomi`. Expected: the API cache-bypass flag is `false` and upstream cache headers remain.

- [x] **Step 3: Implement API cache bypass and response privacy**

Build the outbound wrapper as follows:

```rust
let platform_request = PlatformHttpRequest::new(proxy_req, backend_name);
let platform_request = if matches!(backend, DidomiBackend::Api) {
    platform_request.with_cache_bypass()
} else {
    platform_request
};
```

After receiving an API response, call `enforce_terminal_private_cache_privacy`. Continue adding CORS only to SDK responses and leave SDK origin cache headers intact.

- [x] **Step 4: Run the tests and verify GREEN**

Run `cargo test --package trusted-server-core --target aarch64-apple-darwin didomi`. Expected: all Didomi cache tests pass.

- [x] **Step 5: Commit the cache behavior**

```bash
git add crates/trusted-server-core/src/integrations/didomi.rs
git commit -m "Enforce Didomi API cache privacy"
```

### Task 5: Verify canonical URI conversion on Fastly

**Files:**

- Modify: `crates/trusted-server-adapter-fastly/src/platform.rs:394-430,1009-1025`

- [x] **Step 1: Write the Fastly conversion test**

Add a unit test that builds an EdgeZero request with the canonical URI emitted by the Didomi serializer, including spaces, literal plus values, percent escapes, duplicates, empty values, and an apostrophe. Convert it through `edge_request_to_fastly` and assert `get_url().query()` or the equivalent Fastly request accessor has the same decoded pairs in the same order and exactly one country/region pair.

- [x] **Step 2: Run the focused Fastly test**

Run:

```bash
cargo test-fastly edge_request_to_fastly_preserves_query_encoding_order_and_duplicates
```

Expected: pass if the existing adapter conversion is transparent. If it fails, first add a regression assertion showing the exact normalization difference, then make the smallest adapter correction that preserves existing request semantics.

- [x] **Step 3: Commit the adapter regression test**

```bash
git add crates/trusted-server-adapter-fastly/src/platform.rs
git commit -m "Test Didomi query conversion on Fastly"
```

### Task 6: Update operator configuration and Didomi documentation

**Files:**

- Modify: `trusted-server.example.toml:457-462`
- Modify: `crates/trusted-server-integration-tests/fixtures/configs/trusted-server.integration.toml:53-56`
- Modify: `docs/guide/integrations/didomi.md:23-158,197-240,296-302`
- Modify: `docs/superpowers/specs/2026-09-07-didomi-geo-design.md`

- [x] **Step 1: Update the TOML template and fixture**

Add `geo_query_parameters = false` to the active Didomi blocks. Keep TOML as the primary configuration instructions. If documenting the derived environment override, describe it only as a `ts config validate`/`ts config push` overlay that requires the scalar TOML leaf and does not affect a running deployment.

- [x] **Step 2: Rewrite the Didomi guide around actual behavior**

Document:

- the new option and Fastly-only support;
- the 307 canonical loader flow and exact loader path;
- trusted platform geo precedence and replacement of caller geo parameters;
- the 503 boundary for missing/invalid/country-only geo;
- full path/query cache keys and preservation of Didomi SDK cache headers;
- API cache bypass and downstream no-store;
- no cookie or publisher `Authorization` forwarding;
- rollout validation and cache purge guidance.

Remove the current incorrect statement that `Authorization` is forwarded and the hard-coded SDK `Cache-Control: public, max-age=3600` recommendation. Link the current Didomi reverse-proxy page at `https://developers.didomi.io/api-and-platform/domains/reverse-proxy`.

- [x] **Step 3: Format and check documentation**

Run:

```bash
cd docs && npm run format
```

Expected: Prettier completes successfully.

- [x] **Step 4: Commit configuration and documentation**

```bash
git add trusted-server.example.toml crates/trusted-server-integration-tests/fixtures/configs/trusted-server.integration.toml docs/guide/integrations/didomi.md docs/superpowers/specs/2026-09-07-didomi-geo-design.md
git commit -m "Document Didomi geo forwarding"
```

### Task 7: Run repository validation and inspect the completed branch

**Files:**

- Verify all files changed by Tasks 1-6

- [x] **Step 1: Run format checks**

```bash
cargo fmt --all -- --check
cd docs && npm run format
```

Expected: both commands exit successfully with no formatting changes left.

- [x] **Step 2: Run target-matched tests**

```bash
cargo test-fastly
cargo test-axum
cargo test-cloudflare
cargo test-spin
```

Expected: every target-matched test suite passes. Do not substitute bare `cargo test --workspace`.

- [x] **Step 3: Run target-matched clippy**

```bash
cargo clippy-fastly
cargo clippy-axum
cargo clippy-cloudflare
cargo clippy-cloudflare-wasm
cargo clippy-spin-native
cargo clippy-spin-wasm
```

Expected: every command exits successfully without warnings.

- [x] **Step 4: Review the final diff against issue 85 and the spec**

```bash
git status --short
git diff main...HEAD --check
git diff --stat main...HEAD
```

Confirm each acceptance criterion in the spec has code, automated coverage where possible, or an explicit staging requirement. Confirm no Cloudflare, Axum, Spin, EdgeZero, or production JavaScript behavior changed beyond shared core behavior remaining disabled on unsupported adapters.

- [x] **Step 5: Prepare the branch for review**

Do not merge or push without the user's direction. Report the branch name, commits, validation evidence, and any staging-only checks that remain.
