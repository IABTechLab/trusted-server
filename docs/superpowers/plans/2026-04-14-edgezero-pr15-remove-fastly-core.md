# Remove Fastly from Core Crate — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove every `fastly` crate import and the runtime `tokio` dependency from `trusted-server-core`, relocating Fastly-specific code to `trusted-server-adapter-fastly`.

**Architecture:** Core becomes fully platform-agnostic — it owns domain types, platform traits, and business logic. Adapter owns all Fastly SDK interactions. Four concrete moves: (1) move compat conversion functions inline to adapter and delete core's `compat.rs`; (2) move `geo_from_fastly` from core's `geo.rs` into adapter's `platform.rs`; (3) move `backend.rs` wholesale to the adapter; (4) delete the legacy `storage` module (`FastlyConfigStore`, `FastlySecretStore`) whose call sites have already migrated to platform traits. Finally, move `tokio` to `[dev-dependencies]` (test-only usage) and drop `fastly` from core's `Cargo.toml`.

**Tech Stack:** Rust 2024 edition, `fastly` 0.11.12, `edgezero-adapter-fastly`, `error-stack`, `derive_more`.

**Resolves:** [IABTechLab/trusted-server#496](https://github.com/IABTechLab/trusted-server/issues/496). Blocked by PR 14. Part of #480.

---

## Pre-flight: Code Locations to Understand

Read these before starting — do not guess:

| What to read              | Path                                                         | Why                                             |
| ------------------------- | ------------------------------------------------------------ | ----------------------------------------------- |
| Core Cargo.toml           | `crates/trusted-server-core/Cargo.toml`                      | Exact dep names to remove                       |
| Core lib.rs               | `crates/trusted-server-core/src/lib.rs`                      | Module declarations to remove                   |
| Adapter main.rs           | `crates/trusted-server-adapter-fastly/src/main.rs`           | `compat::` call sites (lines 12, 159, 169, 182) |
| Core compat.rs            | `crates/trusted-server-core/src/compat.rs`                   | Functions to port                               |
| Core geo.rs               | `crates/trusted-server-core/src/geo.rs`                      | `geo_from_fastly` impl (lines 25–35)            |
| Core backend.rs           | `crates/trusted-server-core/src/backend.rs`                  | Entire module to port                           |
| Adapter platform.rs       | `crates/trusted-server-adapter-fastly/src/platform.rs`       | Import lines to update (17, 18, 362)            |
| Adapter management_api.rs | `crates/trusted-server-adapter-fastly/src/management_api.rs` | `BackendConfig` import (line 55)                |
| Core consent/kv.rs        | `crates/trusted-server-core/src/consent/kv.rs`               | Verify any `fastly::kv_store` usage             |

---

## File Map

### Files to **delete** from `crates/trusted-server-core/src/`

- `compat.rs` — Fastly conversion scaffolding, scheduled for deletion in PR 15
- `backend.rs` — Fastly-coupled backend builder, moved to adapter
- `storage/config_store.rs` — Legacy `FastlyConfigStore` (call sites migrated to platform traits)
- `storage/secret_store.rs` — Legacy `FastlySecretStore` (call sites migrated to platform traits)
- `storage/mod.rs` — Empty after above deletions

### Files to **modify** in `crates/trusted-server-core/src/`

- `lib.rs` — Remove `pub mod compat;`, `pub mod backend;`, `pub mod storage;`
- `geo.rs` — Remove `use fastly::geo::Geo;` and `pub fn geo_from_fastly`

### Files to **create** in `crates/trusted-server-adapter-fastly/src/`

- `compat.rs` — The 3 conversion functions that adapter's `main.rs` needs
- `backend.rs` — Full `BackendConfig` moved from core

### Files to **modify** in `crates/trusted-server-adapter-fastly/src/`

- `main.rs` — Add `mod compat;`, update import from `trusted_server_core::compat` to `crate::compat`
- `platform.rs` — Remove `use trusted_server_core::geo::geo_from_fastly;`, add inline private function; remove `use trusted_server_core::backend::BackendConfig;`, add `use crate::backend::BackendConfig;`
- `management_api.rs` — Update `use trusted_server_core::backend::BackendConfig` → `use crate::backend::BackendConfig`

### Files to **modify** (Cargo.toml)

- `crates/trusted-server-core/Cargo.toml` — Remove `fastly`, move `tokio` → `[dev-dependencies]`

---

## Task 1: Create the PR15 Branch

**Files:** none (git only)

- [ ] **Step 1.1: Verify you are on the PR14 branch**

```bash
git branch --show-current
# Expected: feature/edgezero-pr14-entry-point-dual-path
```

- [ ] **Step 1.2: Create and checkout PR15 branch**

```bash
git checkout -b feature/edgezero-pr15-remove-fastly-core
```

- [ ] **Step 1.3: Verify baseline build passes**

```bash
cargo check --workspace 2>&1 | tail -5
```

Expected: `Finished` with no errors.

---

## Task 2: Move `compat` Functions to Adapter, Delete Core's `compat.rs`

**Context:** Adapter's `main.rs` uses `trusted_server_core::compat` for 3 functions in `legacy_main()`: `sanitize_fastly_forwarded_headers`, `from_fastly_request`, and `to_fastly_response`. All three deal with `fastly::Request` / `fastly::Response` — they belong in the adapter. The remaining ~8 functions in core's `compat.rs` are unused by the adapter and can be dropped entirely.

**Files:**

- Create: `crates/trusted-server-adapter-fastly/src/compat.rs`
- Modify: `crates/trusted-server-adapter-fastly/src/main.rs`
- Delete: `crates/trusted-server-core/src/compat.rs`
- Modify: `crates/trusted-server-core/src/lib.rs`

- [ ] **Step 2.1: Read core's `compat.rs` fully**

Read `crates/trusted-server-core/src/compat.rs` lines 1–560. You need the exact implementations of:

- `sanitize_fastly_forwarded_headers` — strips spoofable forwarded headers from a `fastly::Request`
- `from_fastly_request` — converts owned `fastly::Request` → `http::Request<EdgeBody>`
- `to_fastly_response` — converts `http::Response<EdgeBody>` → `fastly::Response`

Copy their `use` imports too (they use `fastly::http::header`, `edgezero_core::body::Body as EdgeBody`, `http`, etc.).

- [ ] **Step 2.2: Create `crates/trusted-server-adapter-fastly/src/compat.rs`**

Create the file with ONLY the 3 functions the adapter needs, plus their imports. Do not port the unused conversion functions. Pattern:

```rust
//! Fastly ↔ http type conversion helpers used by the adapter entry point.

use edgezero_core::body::Body as EdgeBody;
use fastly::http::header;
use http::{Request, Response};

// ... (copy exact implementations from core's compat.rs for the 3 functions)

pub(crate) fn sanitize_fastly_forwarded_headers(req: &mut fastly::Request) {
    // ... (copy from core)
}

pub(crate) fn from_fastly_request(req: fastly::Request) -> Request<EdgeBody> {
    // ... (copy from core)
}

pub(crate) fn to_fastly_response(response: Response<EdgeBody>) -> fastly::Response {
    // ... (copy from core)
}
```

- [ ] **Step 2.3: Declare the module in adapter's `main.rs`**

Add `mod compat;` near the top of `crates/trusted-server-adapter-fastly/src/main.rs` (after other `mod` declarations). Update the import line:

```rust
// Remove:
use trusted_server_core::compat;

// After adding `mod compat;` above, the existing call sites
// `compat::sanitize_fastly_forwarded_headers`, `compat::from_fastly_request`,
// `compat::to_fastly_response` continue to work unchanged — they now resolve
// to the local module.
```

- [ ] **Step 2.4: `cargo check` the adapter to verify compat compiles**

```bash
cargo check -p trusted-server-adapter-fastly --target wasm32-wasip1 2>&1 | grep -E "^error"
```

Expected: no errors related to `compat`.

- [ ] **Step 2.5: Delete core's `compat.rs`**

```bash
rm crates/trusted-server-core/src/compat.rs
```

- [ ] **Step 2.6: Remove `pub mod compat;` from core's `lib.rs`**

Find and remove the line `pub mod compat;` in `crates/trusted-server-core/src/lib.rs`.

- [ ] **Step 2.7: `cargo check` workspace**

```bash
cargo check --workspace 2>&1 | grep -E "^error"
```

Expected: no errors.

- [ ] **Step 2.8: Commit**

```bash
git add crates/trusted-server-adapter-fastly/src/compat.rs \
        crates/trusted-server-adapter-fastly/src/main.rs \
        crates/trusted-server-core/src/lib.rs
git rm crates/trusted-server-core/src/compat.rs
git commit -m "Move compat conversion fns to adapter, delete core compat.rs"
```

---

## Task 3: Move `geo_from_fastly` to Adapter

**Context:** Core's `geo.rs` imports `fastly::geo::Geo` solely for `geo_from_fastly`. The adapter's `platform.rs` (line 18) imports this function from core and calls it at line 362. Moving it inline into `platform.rs` as a `pub(crate)` or private function is the minimal change — no new file required.

**Files:**

- Modify: `crates/trusted-server-core/src/geo.rs`
- Modify: `crates/trusted-server-adapter-fastly/src/platform.rs`

- [ ] **Step 3.1: Read core `geo.rs` lines 1–50**

Capture the exact implementation of `geo_from_fastly` (lines 25–35):

```rust
pub fn geo_from_fastly(geo: &Geo) -> GeoInfo {
    GeoInfo {
        city: geo.city().to_string(),
        country: geo.country_code().to_string(),
        continent: format!("{:?}", geo.continent()),
        latitude: geo.latitude(),
        longitude: geo.longitude(),
        metro_code: geo.metro_code(),
        region: geo.region().map(str::to_string),
    }
}
```

- [ ] **Step 3.2: Add `geo_from_fastly` as a private function in adapter's `platform.rs`**

In `crates/trusted-server-adapter-fastly/src/platform.rs`, directly above the existing `FastlyPlatformGeo` impl block that calls `geo_from_fastly` (around line 362), add:

```rust
use fastly::geo::Geo;

fn geo_from_fastly(geo: &Geo) -> GeoInfo {
    GeoInfo {
        city: geo.city().to_string(),
        country: geo.country_code().to_string(),
        continent: format!("{:?}", geo.continent()),
        latitude: geo.latitude(),
        longitude: geo.longitude(),
        metro_code: geo.metro_code(),
        region: geo.region().map(str::to_string),
    }
}
```

Then remove the import line `use trusted_server_core::geo::geo_from_fastly;` (line 18 of `platform.rs`).

- [ ] **Step 3.3: Remove `geo_from_fastly` and the fastly import from core's `geo.rs`**

In `crates/trusted-server-core/src/geo.rs`:

- Remove: `use fastly::geo::Geo;`
- Remove: the entire `pub fn geo_from_fastly(geo: &Geo) -> GeoInfo { ... }` function and its doc comment

Keep `GeoInfo` re-export, header injection helpers, and all tests.

- [ ] **Step 3.4: `cargo check` workspace**

```bash
cargo check --workspace 2>&1 | grep -E "^error"
```

Expected: no errors.

- [ ] **Step 3.5: Commit**

```bash
git add crates/trusted-server-core/src/geo.rs \
        crates/trusted-server-adapter-fastly/src/platform.rs
git commit -m "Move geo_from_fastly from core to adapter platform"
```

---

## Task 4: Move `BackendConfig` to Adapter

**Context:** Core's `backend.rs` exists solely to create dynamic Fastly backends (`fastly::backend::Backend`). Both `platform.rs` (line 17) and `management_api.rs` (line 55) in the adapter import `BackendConfig` from core. Moving the entire module to the adapter is a clean cut with minimal ripple.

**Files:**

- Create: `crates/trusted-server-adapter-fastly/src/backend.rs`
- Modify: `crates/trusted-server-adapter-fastly/src/main.rs` (add `mod backend;`)
- Modify: `crates/trusted-server-adapter-fastly/src/platform.rs`
- Modify: `crates/trusted-server-adapter-fastly/src/management_api.rs`
- Delete: `crates/trusted-server-core/src/backend.rs`
- Modify: `crates/trusted-server-core/src/lib.rs`

- [ ] **Step 4.1: Read core's `backend.rs` fully**

Read `crates/trusted-server-core/src/backend.rs` (lines 1–465). Note the imports — it uses `fastly::backend::Backend`, `error_stack`, `url::Url`, and `crate::error::TrustedServerError`. The last import becomes `trusted_server_core::error::TrustedServerError` after the move.

- [ ] **Step 4.2: Create `crates/trusted-server-adapter-fastly/src/backend.rs`**

Copy the entire content of core's `backend.rs` verbatim, then update the one internal import:

```rust
// Change:
use crate::error::TrustedServerError;
// To:
use trusted_server_core::error::TrustedServerError;
```

No other changes needed.

- [ ] **Step 4.3: Declare the module in adapter's `main.rs`**

Add `mod backend;` to `crates/trusted-server-adapter-fastly/src/main.rs`.

- [ ] **Step 4.4: Update imports in `platform.rs` and `management_api.rs`**

In `crates/trusted-server-adapter-fastly/src/platform.rs` (line 17):

```rust
// Remove:
use trusted_server_core::backend::BackendConfig;
// Add:
use crate::backend::BackendConfig;
```

In `crates/trusted-server-adapter-fastly/src/management_api.rs` (line 55):

```rust
// Remove:
use trusted_server_core::backend::BackendConfig;
// Add:
use crate::backend::BackendConfig;
```

- [ ] **Step 4.5: `cargo check` the adapter**

```bash
cargo check -p trusted-server-adapter-fastly --target wasm32-wasip1 2>&1 | grep -E "^error"
```

Expected: no errors.

- [ ] **Step 4.6: Delete core's `backend.rs` and remove module declaration**

```bash
git rm crates/trusted-server-core/src/backend.rs
```

Remove `pub mod backend;` from `crates/trusted-server-core/src/lib.rs`.

- [ ] **Step 4.7: `cargo check` workspace**

```bash
cargo check --workspace 2>&1 | grep -E "^error"
```

Expected: no errors.

- [ ] **Step 4.8: Commit**

```bash
git add crates/trusted-server-adapter-fastly/src/backend.rs \
        crates/trusted-server-adapter-fastly/src/main.rs \
        crates/trusted-server-adapter-fastly/src/platform.rs \
        crates/trusted-server-adapter-fastly/src/management_api.rs \
        crates/trusted-server-core/src/lib.rs
git rm crates/trusted-server-core/src/backend.rs
git commit -m "Move BackendConfig from core to adapter backend module"
```

---

## Task 5: Delete Legacy Storage Module

**Context:** `crates/trusted-server-core/src/storage/` exports `FastlyConfigStore` and `FastlySecretStore`. The adapter does not import either — it uses the platform traits (`PlatformConfigStore`, `PlatformSecretStore`) directly. Core's `platform/mod.rs` is also trait-only and has no dependency on these legacy types. The storage doc comment confirms: "will be removed once all call sites have migrated to platform traits."

**Files:**

- Delete: `crates/trusted-server-core/src/storage/config_store.rs`
- Delete: `crates/trusted-server-core/src/storage/secret_store.rs`
- Delete: `crates/trusted-server-core/src/storage/mod.rs`
- Modify: `crates/trusted-server-core/src/lib.rs`

- [ ] **Step 5.1: Confirm no external callers before deleting**

```bash
grep -r "FastlyConfigStore\|FastlySecretStore\|trusted_server_core::storage" \
    crates/trusted-server-adapter-fastly/src/ \
    crates/trusted-server-core/src/
```

Expected: zero results (or only the definitions themselves). If any callers appear outside `storage/`, stop and investigate before continuing.

- [ ] **Step 5.2: Delete the storage module**

```bash
git rm crates/trusted-server-core/src/storage/config_store.rs \
       crates/trusted-server-core/src/storage/secret_store.rs \
       crates/trusted-server-core/src/storage/mod.rs
```

- [ ] **Step 5.3: Remove `pub mod storage;` from core's `lib.rs`**

- [ ] **Step 5.4: `cargo check` workspace**

```bash
cargo check --workspace 2>&1 | grep -E "^error"
```

Expected: no errors.

- [ ] **Step 5.5: Commit**

```bash
git add crates/trusted-server-core/src/lib.rs
git rm crates/trusted-server-core/src/storage/config_store.rs \
       crates/trusted-server-core/src/storage/secret_store.rs \
       crates/trusted-server-core/src/storage/mod.rs
git commit -m "Delete legacy FastlyConfigStore and FastlySecretStore from core"
```

---

## Task 6: Audit and Fix `consent/kv.rs` Fastly Usage

**Context:** The initial audit flagged possible `fastly::kv_store::KVStore` usage at line 230 of `consent/kv.rs`. The top of the file (lines 1–50) shows no fastly imports — the reference may be via fully-qualified path or may have been a hallucination. Verify before removing `fastly` from Cargo.toml.

**Files:**

- Inspect: `crates/trusted-server-core/src/consent/kv.rs`
- Possibly modify: same file

- [ ] **Step 6.1: Search for fastly usage in consent/kv.rs**

```bash
grep -n "fastly" crates/trusted-server-core/src/consent/kv.rs
```

- [ ] **Step 6.2a (if grep returns nothing): No action needed.** The file is clean. Proceed to Task 7.

- [ ] **Step 6.2b (if fastly:: appears): Investigate and move**

Read the lines around each match. The KV store usage in consent likely goes through the `PlatformKvStore` trait (from `edgezero-core`). If raw `fastly::kv_store::KVStore` calls exist:

- Understand what function uses it (likely `open_store` or `fingerprint_unchanged`)
- Move that function to adapter's consent integration or abstract via a trait closure / callback passed in from the adapter
- The goal is zero `fastly::` references in core

- [ ] **Step 6.3: `cargo check` after any changes**

```bash
cargo check --workspace 2>&1 | grep -E "^error"
```

- [ ] **Step 6.4: Commit if changes were made**

```bash
git add crates/trusted-server-core/src/consent/kv.rs
git commit -m "Remove fastly::kv_store usage from core consent module"
```

---

## Task 7: Move Tokio to Dev-Dependencies

**Context:** `tokio` appears in `[dependencies]` (line 45 of core's `Cargo.toml`). The audit found zero tokio usage in production code — all 30 uses are `#[tokio::test]` attributes in test modules. Moving it to `[dev-dependencies]` removes it from the production dependency graph for wasm builds.

**Files:**

- Modify: `crates/trusted-server-core/Cargo.toml`

- [ ] **Step 7.1: Confirm no production tokio usage**

```bash
grep -n "tokio::" crates/trusted-server-core/src/*.rs \
    crates/trusted-server-core/src/**/*.rs 2>/dev/null | \
    grep -v "#\[cfg(test\|#\[tokio::test"
```

Expected: no results. If any appear, investigate and refactor before proceeding.

- [ ] **Step 7.2: Move `tokio` from `[dependencies]` to `[dev-dependencies]`**

In `crates/trusted-server-core/Cargo.toml`:

Remove from `[dependencies]`:

```toml
tokio = { workspace = true }
```

Add to `[dev-dependencies]` (alongside `tokio-test`):

```toml
tokio = { workspace = true }
```

The `tokio-test` entry should already be in `[dev-dependencies]`. The result is both under `[dev-dependencies]`.

- [ ] **Step 7.3: `cargo check` workspace (native)**

```bash
cargo check --workspace 2>&1 | grep -E "^error"
```

- [ ] **Step 7.4: `cargo test` to verify tests still compile and run**

```bash
cargo test --workspace 2>&1 | tail -20
```

Expected: all tests pass.

- [ ] **Step 7.5: Commit**

```bash
git add crates/trusted-server-core/Cargo.toml
git commit -m "Move tokio to dev-dependencies in core (test-only usage)"
```

---

## Task 8: Remove `fastly` from Core's `Cargo.toml`

**Context:** After Tasks 2–6, core should have zero `fastly::` references. Now remove the dependency.

**Files:**

- Modify: `crates/trusted-server-core/Cargo.toml`

- [ ] **Step 8.1: Confirm zero remaining fastly references in core**

```bash
grep -rn "fastly" crates/trusted-server-core/src/ --exclude=migration_guards.rs
```

Expected: zero results. `migration_guards.rs` is deliberately excluded — it contains `"fastly::Request"` etc. as **string literals** in a `#[test]` function (guard patterns), not actual imports. Any matches in that file are expected and not a failure.

Also check for `log-fastly` (spec says to remove it if present):

```bash
grep "log-fastly" crates/trusted-server-core/Cargo.toml
```

If `log-fastly` appears, remove it alongside `fastly` in the next step.

- [ ] **Step 8.2: Remove `fastly` (and `log-fastly` if present) from core's `Cargo.toml`**

In `crates/trusted-server-core/Cargo.toml`, remove:

```toml
fastly = { workspace = true }
# Also remove if present:
# log-fastly = { workspace = true }
```

- [ ] **Step 8.3: `cargo check` workspace (native)**

```bash
cargo check --workspace 2>&1 | grep -E "^error"
```

- [ ] **Step 8.4: `cargo check` for wasm target**

```bash
cargo check -p trusted-server-adapter-fastly --target wasm32-wasip1 2>&1 | grep -E "^error"
```

Expected: no errors on either target.

- [ ] **Step 8.5: Commit**

```bash
git add crates/trusted-server-core/Cargo.toml
git commit -m "Remove fastly dependency from trusted-server-core"
```

---

## Task 9: Full Verification

- [ ] **Step 9.1: Run clippy**

```bash
cargo clippy --workspace --all-targets --all-features -- -D warnings 2>&1 | grep -E "^error"
```

Fix any warnings that become errors.

- [ ] **Step 9.2: Run all tests**

```bash
cargo test --workspace 2>&1 | tail -30
```

Expected: all tests pass.

- [ ] **Step 9.3: Run JS tests**

```bash
cd crates/trusted-server-js/lib && npx vitest run
```

- [ ] **Step 9.4: Verify the "done when" criteria**

```bash
# Zero fastly imports in core:
grep -rn "fastly" crates/trusted-server-core/src/ && echo "FAIL: fastly refs remain" || echo "PASS: core is fastly-free"

# Zero tokio in core [dependencies]:
grep "tokio" crates/trusted-server-core/Cargo.toml

# compat.rs deleted:
ls crates/trusted-server-core/src/compat.rs 2>/dev/null && echo "FAIL: compat.rs still exists" || echo "PASS: compat.rs deleted"
```

- [ ] **Step 9.5: Final commit if any lint fixes were needed**

```bash
git add -p  # stage only lint fixes
git commit -m "Fix clippy warnings after fastly removal"
```

---

## Done When

- `grep -rn "use fastly" crates/trusted-server-core/src/` → zero results
- `grep -rn "fastly::" crates/trusted-server-core/src/` → zero results
- `tokio` no longer in `[dependencies]` section of core's `Cargo.toml` (only `[dev-dependencies]`)
- `crates/trusted-server-core/src/compat.rs` does not exist
- `cargo test --workspace` passes
- `cargo clippy --workspace --all-targets --all-features -- -D warnings` passes
- `cargo check -p trusted-server-adapter-fastly --target wasm32-wasip1` passes
