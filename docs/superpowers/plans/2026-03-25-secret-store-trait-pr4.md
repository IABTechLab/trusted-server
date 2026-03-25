# PR 4: Secret Store Trait (Read-Only) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add three targeted tests to `FastlyPlatformSecretStore` in the Fastly adapter to prove the read path and write stubs satisfy issue #485's "Done when" criteria.

**Architecture:** All implementation code (`FastlyPlatformSecretStore::get_bytes`, `create`, `delete`) already exists in `crates/trusted-server-adapter-fastly/src/platform.rs`. This PR adds no new implementation — only three tests that exercise existing code paths. The `PlatformSecretStore` trait and `RuntimeServices` wiring are already complete in `trusted-server-core`.

**Tech Stack:** Rust 1.91.1, `error-stack` for `Report<PlatformError>`, `cargo test --workspace` via Viceroy (Fastly local simulator).

---

## File Map

| File | Change |
|---|---|
| `crates/trusted-server-adapter-fastly/src/platform.rs` | Add three tests in the existing `#[cfg(test)]` block |

No other files are modified.

---

## Background: What Already Exists

Before writing tests, understand the shape of the existing code in `platform.rs`:

**`get_secret_bytes` free function (lines ~105–135):**
```rust
fn get_secret_bytes<S, Open, OpenError>(
    store_name: &str,
    key: &str,
    open_store: Open,
) -> Result<Vec<u8>, Report<PlatformError>>
where
    S: SecretStoreReader,
    Open: FnOnce() -> Result<S, OpenError>,
    OpenError: Display,
```
Three failure branches: (1) `open_store()` returns `Err` → `PlatformError::SecretStore`, (2) `try_get_bytes` returns `SecretReadError::Lookup` → same, (3) `try_get_bytes` returns `SecretReadError::Decrypt` → same. Branch (3) already has a test.

**`FastlyPlatformSecretStore` (lines ~183–207):**
```rust
pub struct FastlyPlatformSecretStore;
impl PlatformSecretStore for FastlyPlatformSecretStore {
    fn get_bytes(&self, store_name, key) -> ... { /* delegates to get_secret_bytes */ }
    fn create(&self, _store_id, _name, _value) -> ... { Err(Report::new(PlatformError::NotImplemented)) }
    fn delete(&self, _store_id, _name) -> ... { Err(Report::new(PlatformError::NotImplemented)) }
}
```

**Existing test stubs available inside `#[cfg(test)] mod tests`** (via `use super::*`):
- `StubSecretStore` — implements `SecretStoreReader`; models decrypt errors
- `StubConfigStore` — unrelated
- `get_secret_bytes` free function — directly callable in tests

New tests insert **after** line 534 (after `get_secret_bytes_returns_error_when_decrypt_fails`) and **before** `fastly_platform_http_client_reports_not_implemented`.

---

## Task 1: Test — `get_secret_bytes` open-failure path

**Files:**
- Modify: `crates/trusted-server-adapter-fastly/src/platform.rs:534`

This test calls `get_secret_bytes` with a closure that returns `Err` before constructing any store, simulating a failed `SecretStore::open()`. `StubSecretStore` is never constructed.

- [ ] **Step 1.1: Add the test**

Insert after the closing `}` of `get_secret_bytes_returns_error_when_decrypt_fails` (after line 534):

```rust
    #[test]
    fn get_secret_bytes_returns_error_when_open_fails() {
        let err = get_secret_bytes::<StubSecretStore, _, _>("signing_keys", "active", || {
            Err::<StubSecretStore, &'static str>("permission denied")
        })
        .expect_err("should return an error when the secret store cannot be opened");

        assert!(
            matches!(err.current_context(), &PlatformError::SecretStore),
            "should surface as PlatformError::SecretStore"
        );
    }
```

- [ ] **Step 1.2: Run the test and confirm it passes**

```bash
cargo test --workspace -- get_secret_bytes_returns_error_when_open_fails
```

Expected: 1 test, PASSED. If it fails, re-read `get_secret_bytes` to verify the open-failure arm maps to `PlatformError::SecretStore`.

- [ ] **Step 1.3: Commit**

```bash
git add crates/trusted-server-adapter-fastly/src/platform.rs
git commit -m "Add test for get_secret_bytes open-failure path"
```

---

## Task 2: Tests — `FastlyPlatformSecretStore` write stubs

**Files:**
- Modify: `crates/trusted-server-adapter-fastly/src/platform.rs` (same test block)

Two tests prove `create` and `delete` return `PlatformError::NotImplemented`. Follow the pattern of the existing `fastly_platform_http_client_reports_not_implemented` test (line 536).

- [ ] **Step 2.1: Add the `create` stub test**

Insert after the test from Task 1:

```rust
    #[test]
    fn fastly_platform_secret_store_create_returns_not_implemented() {
        let store = FastlyPlatformSecretStore;
        let store_id = StoreId::from("test-store-id");
        let err = store
            .create(&store_id, "my-secret", "value")
            .expect_err("should return an error for unimplemented create");

        assert!(
            matches!(err.current_context(), &PlatformError::NotImplemented),
            "should report NotImplemented while secret store write is not yet implemented"
        );
    }
```

- [ ] **Step 2.2: Run the test and confirm it passes**

```bash
cargo test --workspace -- fastly_platform_secret_store_create_returns_not_implemented
```

Expected: 1 test, PASSED.

- [ ] **Step 2.3: Add the `delete` stub test**

Insert after the `create` test:

```rust
    #[test]
    fn fastly_platform_secret_store_delete_returns_not_implemented() {
        let store = FastlyPlatformSecretStore;
        let store_id = StoreId::from("test-store-id");
        let err = store
            .delete(&store_id, "my-secret")
            .expect_err("should return an error for unimplemented delete");

        assert!(
            matches!(err.current_context(), &PlatformError::NotImplemented),
            "should report NotImplemented while secret store write is not yet implemented"
        );
    }
```

- [ ] **Step 2.4: Run both new tests and confirm they pass**

```bash
cargo test --workspace -- fastly_platform_secret_store
```

Expected: 2 tests, both PASSED.

- [ ] **Step 2.5: Commit**

```bash
git add crates/trusted-server-adapter-fastly/src/platform.rs
git commit -m "Add NotImplemented tests for FastlyPlatformSecretStore write stubs"
```

---

## Task 3: CI gates

- [ ] **Step 3.1: Run the full test suite**

```bash
cargo test --workspace
```

Expected: all tests pass.

- [ ] **Step 3.2: Run clippy**

```bash
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Expected: no warnings.

- [ ] **Step 3.3: Run fmt check**

```bash
cargo fmt --all -- --check
```

Expected: exit 0 (no formatting changes needed). If it fails, run `cargo fmt --all` then re-check.

- [ ] **Step 3.4: Final commit (if fmt required a fix)**

Only needed if `cargo fmt` changed anything:

```bash
git add crates/trusted-server-adapter-fastly/src/platform.rs
git commit -m "Apply cargo fmt"
```

---

## Done When

- [ ] Three new tests exist in `platform.rs` and all pass
- [ ] `cargo test --workspace` passes
- [ ] `cargo clippy --workspace --all-targets --all-features -- -D warnings` passes
- [ ] `cargo fmt --all -- --check` passes
