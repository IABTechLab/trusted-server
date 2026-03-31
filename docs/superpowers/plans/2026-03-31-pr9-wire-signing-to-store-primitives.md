# PR 9: Wire Request-Signing to Platform Store Primitives

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove `api_client.rs` from `trusted-server-core`, move Fastly management API transport to the adapter as `management_api.rs`, and replace all direct `Fastly*Store` / `FastlyApiClient` usage in `request_signing/` with `RuntimeServices` store primitives.

**Architecture:** Core `request_signing/` code calls platform-agnostic `services.config_store()` and `services.secret_store()` for all reads and writes. The Fastly adapter's `management_api.rs` absorbs the HTTP transport (calls to `api.fastly.com`) and backs the `put`/`delete`/`create` write methods in `FastlyPlatformConfigStore` and `FastlyPlatformSecretStore`. No signing-specific trait is introduced — adapters only implement store CRUD, and core owns all signing business logic.

**Tech Stack:** Rust 2024 edition, `error-stack`, `derive_more::Display`, `fastly`, `ed25519-dalek`, `serde_json`, `urlencoding`

---

## Background: What the Current Code Does

Before touching anything, read these files to understand the current state:

| File | Status | Notes |
|------|--------|-------|
| `crates/trusted-server-core/src/storage/api_client.rs` | **Delete** | Contains `FastlyApiClient` — HTTP calls to `api.fastly.com`. Used only by `rotation.rs`. |
| `crates/trusted-server-core/src/request_signing/rotation.rs` | **Migrate** | Uses `FastlyConfigStore` (reads) + `FastlyApiClient` (writes). Main migration target. |
| `crates/trusted-server-core/src/request_signing/signing.rs` | **Migrate** | Uses `FastlyConfigStore` + `FastlySecretStore` in 3 places. |
| `crates/trusted-server-core/src/request_signing/endpoints.rs` | **Update** | `handle_verify_signature`, `handle_rotate_key`, `handle_deactivate_key` don't receive `&RuntimeServices`. |
| `crates/trusted-server-core/src/request_signing/jwks.rs` | Already migrated ✓ | Uses `RuntimeServices`. No changes needed. |
| `crates/trusted-server-adapter-fastly/src/platform.rs` | **Update** | `FastlyPlatformConfigStore::put/delete` and `FastlyPlatformSecretStore::create/delete` return `PlatformError::NotImplemented`. |
| `crates/trusted-server-adapter-fastly/src/main.rs` | **Update** | Three call sites pass handlers without `runtime_services`. |

## File Map

### Delete
- `crates/trusted-server-core/src/storage/api_client.rs`

### Modify (core)
- `crates/trusted-server-core/src/storage/mod.rs` — remove `api_client` submodule + re-export
- `crates/trusted-server-core/src/platform/test_support.rs` — add `build_services_with_config_and_secret`
- `crates/trusted-server-core/src/request_signing/rotation.rs` — replace `FastlyConfigStore`/`FastlyApiClient` with `RuntimeServices`
- `crates/trusted-server-core/src/request_signing/signing.rs` — replace `FastlyConfigStore`/`FastlySecretStore` with `RuntimeServices`
- `crates/trusted-server-core/src/request_signing/endpoints.rs` — add `&RuntimeServices` to three handlers

### Create (adapter)
- `crates/trusted-server-adapter-fastly/src/management_api.rs` — Fastly management API transport (absorbs `api_client.rs` logic, returns `PlatformError`)

### Modify (adapter)
- `crates/trusted-server-adapter-fastly/src/platform.rs` — implement `put`/`delete` for config, `create`/`delete` for secrets
- `crates/trusted-server-adapter-fastly/src/main.rs` — pass `runtime_services` to three handlers

---

## Tasks

### Task 1: Add `build_services_with_config_and_secret` to `test_support.rs`

**Why:** Tasks 4 and 5 need a `RuntimeServices` with both a custom config store AND a custom secret store. The current `build_services_with_config` only customises the config store.

**Files:**
- Modify: `crates/trusted-server-core/src/platform/test_support.rs`

- [ ] **Step 1: Write a failing test that calls `build_services_with_config_and_secret`**

Add to the `#[cfg(test)]` block at the bottom of `test_support.rs`:

```rust
#[test]
fn build_services_with_config_and_secret_uses_provided_stores() {
    // Arrange: noop stores
    let services = build_services_with_config_and_secret(NoopConfigStore, NoopSecretStore);

    // Act: both stores return Unsupported (confirming the injected impls are active)
    let config_result = services.config_store().get(&StoreName::from("s"), "k");
    let secret_result = services.secret_store().get_bytes(&StoreName::from("s"), "k");

    assert!(config_result.is_err(), "should delegate to injected config store");
    assert!(secret_result.is_err(), "should delegate to injected secret store");
}
```

- [ ] **Step 2: Run to confirm it fails to compile**

```bash
cargo test --package trusted-server-core platform::test_support 2>&1 | head -20
```

Expected: compile error — `build_services_with_config_and_secret` not found.

- [ ] **Step 3: Add the function above the existing `build_services_with_config`**

```rust
/// Build a [`RuntimeServices`] instance with a custom config store and a custom secret store.
///
/// Use this when a test exercises code that reads from config AND secret stores,
/// such as `request_signing::signing` and `request_signing::rotation`.
pub(crate) fn build_services_with_config_and_secret(
    config_store: impl PlatformConfigStore + 'static,
    secret_store: impl PlatformSecretStore + 'static,
) -> RuntimeServices {
    RuntimeServices::builder()
        .config_store(Arc::new(config_store))
        .secret_store(Arc::new(secret_store))
        .kv_store(Arc::new(edgezero_core::key_value_store::NoopKvStore))
        .backend(Arc::new(NoopBackend))
        .http_client(Arc::new(NoopHttpClient))
        .geo(Arc::new(NoopGeo))
        .client_info(ClientInfo {
            client_ip: None,
            tls_protocol: None,
            tls_cipher: None,
        })
        .build()
}
```

- [ ] **Step 4: Run the test to confirm it passes**

```bash
cargo test --package trusted-server-core platform::test_support::tests::build_services_with_config_and_secret
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/platform/test_support.rs
git commit -m "Add build_services_with_config_and_secret to test_support"
```

---

### Task 2: Create `management_api.rs` in the adapter

**Why:** Move the Fastly management API transport (currently in `api_client.rs` in core) to the adapter, where Fastly SDK usage is appropriate. Returns `PlatformError` instead of `TrustedServerError`.

**Credential security note (from spec):** The Fastly API token is read from the `api-keys` secret store, key `api_key`. Log store IDs and operation names only — never the token or secret value.

**Files:**
- Create: `crates/trusted-server-adapter-fastly/src/management_api.rs`
- Modify: `crates/trusted-server-adapter-fastly/src/main.rs` — add `mod management_api;`

- [ ] **Step 1: Write the new module**

Create `crates/trusted-server-adapter-fastly/src/management_api.rs`:

```rust
//! Fastly management API transport for store write operations.
//!
//! Provides [`FastlyManagementApiClient`], which wraps the Fastly REST
//! management API for write operations on config and secret stores.
//! Used by [`super::platform::FastlyPlatformConfigStore`] and
//! [`super::platform::FastlyPlatformSecretStore`] to back store write methods.
//!
//! # Credentials
//!
//! The Fastly API token is read from the `api-keys` secret store under the
//! `api_key` entry. The token must have config-store write and secret-store
//! write permissions only — no service-level admin or purge permissions.
//!
//! # Security
//!
//! Credential values are never logged. Log messages include store IDs and
//! operation names only.

use std::io::Read;

use error_stack::{Report, ResultExt};
use fastly::{Request, Response};
use http::StatusCode;
use trusted_server_core::backend::BackendConfig;
use trusted_server_core::platform::{PlatformError, PlatformSecretStore, StoreName};

use crate::platform::FastlyPlatformSecretStore;

const FASTLY_API_HOST: &str = "https://api.fastly.com";
const API_KEYS_STORE: &str = "api-keys";
const API_KEY_ENTRY: &str = "api_key";

pub(crate) fn build_config_item_payload(value: &str) -> String {
    format!("item_value={}", urlencoding::encode(value))
}

/// HTTP client for Fastly management API write operations.
///
/// Backs the `put`/`delete` methods of [`FastlyPlatformConfigStore`] and
/// the `create`/`delete` methods of [`FastlyPlatformSecretStore`].
pub(crate) struct FastlyManagementApiClient {
    api_key: Vec<u8>,
    base_url: &'static str,
    backend_name: String,
}

impl FastlyManagementApiClient {
    /// Initialize the client by reading the API token from the `api-keys` secret store.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError::Backend`] if the management API backend cannot
    /// be registered, or [`PlatformError::SecretStore`] if the API key cannot
    /// be read.
    pub(crate) fn new() -> Result<Self, Report<PlatformError>> {
        let backend_name = BackendConfig::from_url(FASTLY_API_HOST, true)
            .change_context(PlatformError::Backend)
            .attach("failed to register Fastly management API backend")?;

        let api_key = FastlyPlatformSecretStore
            .get_bytes(&StoreName::from(API_KEYS_STORE), API_KEY_ENTRY)
            .change_context(PlatformError::SecretStore)
            .attach("failed to read Fastly API key from secret store")?;

        log::debug!("FastlyManagementApiClient: initialized for management API operations");

        Ok(Self {
            api_key,
            base_url: FASTLY_API_HOST,
            backend_name,
        })
    }

    fn make_request(
        &self,
        method: &str,
        path: &str,
        body: Option<String>,
        content_type: &str,
    ) -> Result<Response, Report<PlatformError>> {
        let url = format!("{}{}", self.base_url, path);
        let api_key_str = String::from_utf8_lossy(&self.api_key).to_string();

        let mut request = match method {
            "GET" => Request::get(&url),
            "POST" => Request::post(&url),
            "PUT" => Request::put(&url),
            "DELETE" => Request::delete(&url),
            _ => {
                return Err(Report::new(PlatformError::ConfigStore)
                    .attach(format!("unsupported HTTP method: {}", method)))
            }
        };

        request = request
            .with_header("Fastly-Key", api_key_str)
            .with_header("Accept", "application/json");

        if let Some(body_content) = body {
            request = request
                .with_header("Content-Type", content_type)
                .with_body(body_content);
        }

        request.send(&self.backend_name).map_err(|e| {
            Report::new(PlatformError::ConfigStore)
                .attach(format!("management API request failed: {}", e))
        })
    }

    /// Update or create a config store item.
    ///
    /// # Errors
    ///
    /// Returns an error if the API request fails or returns a non-OK status.
    pub(crate) fn update_config_item(
        &self,
        store_id: &str,
        key: &str,
        value: &str,
    ) -> Result<(), Report<PlatformError>> {
        let path = format!("/resources/stores/config/{}/item/{}", store_id, key);
        let payload = build_config_item_payload(value);

        let mut response = self.make_request(
            "PUT",
            &path,
            Some(payload),
            "application/x-www-form-urlencoded",
        )?;

        let mut buf = String::new();
        response
            .get_body_mut()
            .read_to_string(&mut buf)
            .change_context(PlatformError::ConfigStore)?;

        if response.get_status() == StatusCode::OK {
            log::debug!(
                "FastlyManagementApiClient: updated config key '{}' in store '{}'",
                key,
                store_id
            );
            Ok(())
        } else {
            Err(Report::new(PlatformError::ConfigStore).attach(format!(
                "config item update failed with HTTP {} for key '{}' in store '{}'",
                response.get_status(),
                key,
                store_id
            )))
        }
    }

    /// Delete a config store item.
    ///
    /// # Errors
    ///
    /// Returns an error if the API request fails or returns an unexpected status.
    pub(crate) fn delete_config_item(
        &self,
        store_id: &str,
        key: &str,
    ) -> Result<(), Report<PlatformError>> {
        let path = format!("/resources/stores/config/{}/item/{}", store_id, key);

        let mut response = self.make_request("DELETE", &path, None, "application/json")?;

        let mut buf = String::new();
        response
            .get_body_mut()
            .read_to_string(&mut buf)
            .change_context(PlatformError::ConfigStore)?;

        if response.get_status() == StatusCode::OK
            || response.get_status() == StatusCode::NO_CONTENT
        {
            log::debug!(
                "FastlyManagementApiClient: deleted config key '{}' from store '{}'",
                key,
                store_id
            );
            Ok(())
        } else {
            Err(Report::new(PlatformError::ConfigStore).attach(format!(
                "config item delete failed with HTTP {} for key '{}' in store '{}'",
                response.get_status(),
                key,
                store_id
            )))
        }
    }

    /// Create or overwrite a secret store entry.
    ///
    /// # Errors
    ///
    /// Returns an error if the API request fails or returns a non-OK status.
    pub(crate) fn create_secret(
        &self,
        store_id: &str,
        secret_name: &str,
        secret_value: &str,
    ) -> Result<(), Report<PlatformError>> {
        let path = format!("/resources/stores/secret/{}/secrets", store_id);
        let payload = serde_json::json!({
            "name": secret_name,
            "secret": secret_value
        });

        let mut response =
            self.make_request("POST", &path, Some(payload.to_string()), "application/json")?;

        let mut buf = String::new();
        response
            .get_body_mut()
            .read_to_string(&mut buf)
            .change_context(PlatformError::SecretStore)?;

        if response.get_status() == StatusCode::OK {
            log::debug!(
                "FastlyManagementApiClient: created secret '{}' in store '{}'",
                secret_name,
                store_id
            );
            Ok(())
        } else {
            Err(Report::new(PlatformError::SecretStore).attach(format!(
                "secret create failed with HTTP {} for name '{}' in store '{}'",
                response.get_status(),
                secret_name,
                store_id
            )))
        }
    }

    /// Delete a secret store entry.
    ///
    /// # Errors
    ///
    /// Returns an error if the API request fails or returns an unexpected status.
    pub(crate) fn delete_secret(
        &self,
        store_id: &str,
        secret_name: &str,
    ) -> Result<(), Report<PlatformError>> {
        let path = format!(
            "/resources/stores/secret/{}/secrets/{}",
            store_id, secret_name
        );

        let mut response = self.make_request("DELETE", &path, None, "application/json")?;

        let mut buf = String::new();
        response
            .get_body_mut()
            .read_to_string(&mut buf)
            .change_context(PlatformError::SecretStore)?;

        if response.get_status() == StatusCode::OK
            || response.get_status() == StatusCode::NO_CONTENT
        {
            log::debug!(
                "FastlyManagementApiClient: deleted secret '{}' from store '{}'",
                secret_name,
                store_id
            );
            Ok(())
        } else {
            Err(Report::new(PlatformError::SecretStore).attach(format!(
                "secret delete failed with HTTP {} for name '{}' in store '{}'",
                response.get_status(),
                secret_name,
                store_id
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_config_item_payload_url_encodes_reserved_characters() {
        let payload =
            build_config_item_payload(r#"value with spaces + symbols &= {"kid":"a+b"}"#);

        assert_eq!(
            payload,
            "item_value=value%20with%20spaces%20%2B%20symbols%20%26%3D%20%7B%22kid%22%3A%22a%2Bb%22%7D",
            "should URL-encode config item values in form payloads"
        );
    }
}
```

- [ ] **Step 2: Add `mod management_api;` to `main.rs`**

In `crates/trusted-server-adapter-fastly/src/main.rs`, add near the top (alongside the other `mod` declarations):

```rust
mod management_api;
```

- [ ] **Step 3: Run the payload test**

```bash
cargo test --package trusted-server-adapter-fastly management_api -- --nocapture
```

Expected: `build_config_item_payload_url_encodes_reserved_characters` passes.

- [ ] **Step 4: Commit**

```bash
git add crates/trusted-server-adapter-fastly/src/management_api.rs crates/trusted-server-adapter-fastly/src/main.rs
git commit -m "Add FastlyManagementApiClient to adapter"
```

---

### Task 3: Implement `FastlyPlatformConfigStore` write methods

**Why:** Replace the `NotImplemented` stubs in `platform.rs` with real calls to `FastlyManagementApiClient`. The existing `NotImplemented` test for secret store (`fastly_platform_secret_store_create_returns_not_implemented`, `fastly_platform_secret_store_delete_returns_not_implemented`) must be deleted now that the real implementation lands. Check if there are equivalent config store tests to delete too.

**Files:**
- Modify: `crates/trusted-server-adapter-fastly/src/platform.rs`

- [ ] **Step 1: Delete `NotImplemented` tests for secret store writes**

In `platform.rs` tests, find and delete these two tests (they assert the old stub behavior that no longer holds):
- `fastly_platform_secret_store_create_returns_not_implemented`
- `fastly_platform_secret_store_delete_returns_not_implemented`

There are no analogous `NotImplemented` tests for `FastlyPlatformConfigStore::put/delete` — only the secret store stubs have them. No config-store equivalent to search for.

- [ ] **Step 2: Update `FastlyPlatformConfigStore::put` and `delete`**

In `platform.rs`, replace:

```rust
fn put(
    &self,
    _store_id: &StoreId,
    _key: &str,
    _value: &str,
) -> Result<(), Report<PlatformError>> {
    Err(Report::new(PlatformError::NotImplemented))
}

fn delete(&self, _store_id: &StoreId, _key: &str) -> Result<(), Report<PlatformError>> {
    Err(Report::new(PlatformError::NotImplemented))
}
```

With:

```rust
fn put(
    &self,
    store_id: &StoreId,
    key: &str,
    value: &str,
) -> Result<(), Report<PlatformError>> {
    let client = crate::management_api::FastlyManagementApiClient::new()?;
    client.update_config_item(store_id.as_ref(), key, value)
}

fn delete(&self, store_id: &StoreId, key: &str) -> Result<(), Report<PlatformError>> {
    let client = crate::management_api::FastlyManagementApiClient::new()?;
    client.delete_config_item(store_id.as_ref(), key)
}
```

- [ ] **Step 3: Update `FastlyPlatformSecretStore::create` and `delete`**

Replace:

```rust
fn create(
    &self,
    _store_id: &StoreId,
    _name: &str,
    _value: &str,
) -> Result<(), Report<PlatformError>> {
    Err(Report::new(PlatformError::NotImplemented))
}

fn delete(&self, _store_id: &StoreId, _name: &str) -> Result<(), Report<PlatformError>> {
    Err(Report::new(PlatformError::NotImplemented))
}
```

With:

```rust
fn create(
    &self,
    store_id: &StoreId,
    name: &str,
    value: &str,
) -> Result<(), Report<PlatformError>> {
    let client = crate::management_api::FastlyManagementApiClient::new()?;
    client.create_secret(store_id.as_ref(), name, value)
}

fn delete(&self, store_id: &StoreId, name: &str) -> Result<(), Report<PlatformError>> {
    let client = crate::management_api::FastlyManagementApiClient::new()?;
    client.delete_secret(store_id.as_ref(), name)
}
```

- [ ] **Step 4: Verify adapter compiles and remaining tests pass**

```bash
cargo test --package trusted-server-adapter-fastly -- --nocapture
```

Expected: all tests pass (the `NotImplemented` tests were deleted; remaining tests still pass).

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-adapter-fastly/src/platform.rs
git commit -m "Implement FastlyPlatformConfigStore and FastlyPlatformSecretStore write methods via management API"
```

---

### Task 4: Migrate `rotation.rs` to `RuntimeServices`

**Why:** `KeyRotationManager` currently holds `FastlyConfigStore` (reads) and `FastlyApiClient` (writes) as fields. Replace both with `&RuntimeServices` passed to each method.

**New design:**
- Drop `config_store: FastlyConfigStore` and `api_client: FastlyApiClient` fields
- Keep `config_store_id: StoreId` and `secret_store_id: StoreId` (passed to write methods)
- `new()` is now infallible (no API key fetch at construction time)
- All `rotate_key`, `list_active_keys`, `deactivate_key`, `delete_key` accept `services: &RuntimeServices`
- Reads use `JWKS_STORE_NAME` (edge-visible name); writes use the stored `StoreId` values

**Files:**
- Modify: `crates/trusted-server-core/src/request_signing/rotation.rs`

- [ ] **Step 1: Write failing tests that define the new API**

Replace the `#[cfg(test)]` module in `rotation.rs` with:

```rust
#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use error_stack::Report;

    use crate::platform::test_support::build_services_with_config_and_secret;
    use crate::platform::{
        PlatformConfigStore, PlatformError, PlatformSecretStore, StoreId, StoreName,
    };
    use crate::request_signing::Keypair;

    use super::*;

    // ---------------------------------------------------------------------------
    // Spy stores: record put/create/delete calls, serve preset get values
    // ---------------------------------------------------------------------------

    struct SpyConfigStore {
        data: Mutex<HashMap<String, String>>,
        puts: Mutex<Vec<(String, String, String)>>,
        deletes: Mutex<Vec<(String, String)>>,
    }

    impl SpyConfigStore {
        fn new(initial: HashMap<String, String>) -> Self {
            Self {
                data: Mutex::new(initial),
                puts: Mutex::new(vec![]),
                deletes: Mutex::new(vec![]),
            }
        }
    }

    impl PlatformConfigStore for SpyConfigStore {
        fn get(&self, _: &StoreName, key: &str) -> Result<String, Report<PlatformError>> {
            self.data
                .lock()
                .expect("should lock data")
                .get(key)
                .cloned()
                .ok_or_else(|| Report::new(PlatformError::ConfigStore))
        }

        fn put(
            &self,
            store_id: &StoreId,
            key: &str,
            value: &str,
        ) -> Result<(), Report<PlatformError>> {
            self.puts.lock().expect("should lock puts").push((
                store_id.to_string(),
                key.to_string(),
                value.to_string(),
            ));
            self.data
                .lock()
                .expect("should lock data")
                .insert(key.to_string(), value.to_string());
            Ok(())
        }

        fn delete(&self, store_id: &StoreId, key: &str) -> Result<(), Report<PlatformError>> {
            self.deletes
                .lock()
                .expect("should lock deletes")
                .push((store_id.to_string(), key.to_string()));
            self.data
                .lock()
                .expect("should lock data")
                .remove(key);
            Ok(())
        }
    }

    struct SpySecretStore {
        creates: Mutex<Vec<(String, String, String)>>,
        deletes: Mutex<Vec<(String, String)>>,
    }

    impl SpySecretStore {
        fn new() -> Self {
            Self {
                creates: Mutex::new(vec![]),
                deletes: Mutex::new(vec![]),
            }
        }
    }

    impl PlatformSecretStore for SpySecretStore {
        fn get_bytes(&self, _: &StoreName, _: &str) -> Result<Vec<u8>, Report<PlatformError>> {
            Err(Report::new(PlatformError::SecretStore))
        }

        fn create(
            &self,
            store_id: &StoreId,
            name: &str,
            value: &str,
        ) -> Result<(), Report<PlatformError>> {
            self.creates.lock().expect("should lock creates").push((
                store_id.to_string(),
                name.to_string(),
                value.to_string(),
            ));
            Ok(())
        }

        fn delete(&self, store_id: &StoreId, name: &str) -> Result<(), Report<PlatformError>> {
            self.deletes
                .lock()
                .expect("should lock deletes")
                .push((store_id.to_string(), name.to_string()));
            Ok(())
        }
    }

    // ---------------------------------------------------------------------------
    // Tests
    // ---------------------------------------------------------------------------

    #[test]
    fn generate_date_based_kid_has_correct_format() {
        let kid = generate_date_based_kid();
        assert!(kid.starts_with("ts-"), "should start with 'ts-'");
        assert!(kid.len() >= 13, "should be at least 13 characters");
        let parts: Vec<&str> = kid.split('-').collect();
        assert_eq!(parts.len(), 4, "should have 4 dash-separated parts");
        assert_eq!(parts[0], "ts", "first part should be 'ts'");
    }

    #[test]
    fn new_is_infallible_and_stores_ids() {
        let manager = KeyRotationManager::new("cfg-store-123", "sec-store-456");
        assert_eq!(
            manager.config_store_id.as_ref(),
            "cfg-store-123",
            "should store config_store_id"
        );
        assert_eq!(
            manager.secret_store_id.as_ref(),
            "sec-store-456",
            "should store secret_store_id"
        );
    }

    #[test]
    fn rotate_key_stores_private_key_via_secret_store_create() {
        let config_store = SpyConfigStore::new(HashMap::new());
        let secret_store = SpySecretStore::new();
        let services =
            build_services_with_config_and_secret(config_store, secret_store);

        let manager = KeyRotationManager::new("cfg-id", "sec-id");
        let result = manager.rotate_key(&services, Some("new-kid".to_string()));

        assert!(result.is_ok(), "should succeed when stores accept writes");
        let rotation = result.expect("should produce rotation result");
        assert_eq!(rotation.new_kid, "new-kid", "should use the provided kid");
        assert!(
            rotation.active_kids.contains(&"new-kid".to_string()),
            "should include new kid in active kids"
        );
    }

    #[test]
    fn deactivate_key_fails_when_only_one_key_remains() {
        let mut data = HashMap::new();
        data.insert("active-kids".to_string(), "only-key".to_string());
        let config_store = SpyConfigStore::new(data);
        let secret_store = SpySecretStore::new();
        let services =
            build_services_with_config_and_secret(config_store, secret_store);

        let manager = KeyRotationManager::new("cfg-id", "sec-id");
        let result = manager.deactivate_key(&services, "only-key");

        assert!(
            result.is_err(),
            "should fail to deactivate the last active key"
        );
    }

    #[test]
    fn key_rotation_result_structure_is_valid() {
        let jwk = Keypair::generate().get_jwk("test-key".to_string());
        let result = KeyRotationResult {
            new_kid: "ts-2024-01-01".to_string(),
            previous_kid: Some("ts-2023-12-31".to_string()),
            active_kids: vec![
                "ts-2023-12-31".to_string(),
                "ts-2024-01-01".to_string(),
            ],
            jwk: jwk.clone(),
        };

        assert_eq!(result.new_kid, "ts-2024-01-01");
        assert_eq!(result.previous_kid, Some("ts-2023-12-31".to_string()));
        assert_eq!(result.active_kids.len(), 2);
        assert_eq!(result.jwk.prm.kid, Some("test-key".to_string()));
    }
}
```

- [ ] **Step 2: Run tests to confirm they fail (expected compile error)**

```bash
cargo test --package trusted-server-core request_signing::rotation 2>&1 | head -30
```

Expected: compile error — `KeyRotationManager::new` still returns `Result`, and `rotate_key` doesn't take `services`.

- [ ] **Step 3: Rewrite `rotation.rs`**

Replace the entire file with the following (preserving `generate_date_based_kid` and `KeyRotationResult`):

```rust
//! Key rotation management for request signing.
//!
//! This module provides functionality for rotating signing keys, managing key
//! lifecycle, and storing keys via platform store primitives through
//! [`RuntimeServices`].

use std::sync::LazyLock;

use base64::{engine::general_purpose, Engine};
use ed25519_dalek::SigningKey;
use error_stack::{Report, ResultExt};
use jose_jwk::Jwk;

use crate::error::TrustedServerError;
use crate::platform::{RuntimeServices, StoreId, StoreName};
use crate::request_signing::JWKS_CONFIG_STORE_NAME;

use super::Keypair;

static JWKS_STORE_NAME: LazyLock<StoreName> =
    LazyLock::new(|| StoreName::from(JWKS_CONFIG_STORE_NAME));

#[derive(Debug, Clone)]
pub struct KeyRotationResult {
    pub new_kid: String,
    pub previous_kid: Option<String>,
    pub active_kids: Vec<String>,
    pub jwk: Jwk,
}

/// Manages signing key lifecycle using platform store primitives.
///
/// Reads use the edge-visible store name ([`JWKS_CONFIG_STORE_NAME`]).
/// Writes use the management API store identifiers supplied at construction.
pub struct KeyRotationManager {
    /// Management API store ID for config store writes.
    config_store_id: StoreId,
    /// Management API store ID for secret store writes.
    secret_store_id: StoreId,
}

impl KeyRotationManager {
    /// Creates a new key rotation manager.
    ///
    /// The `config_store_id` and `secret_store_id` are platform management API
    /// identifiers used for write operations. Edge reads use the store names
    /// defined in [`JWKS_CONFIG_STORE_NAME`] and
    /// [`crate::request_signing::SIGNING_SECRET_STORE_NAME`].
    #[must_use]
    pub fn new(
        config_store_id: impl Into<String>,
        secret_store_id: impl Into<String>,
    ) -> Self {
        Self {
            config_store_id: StoreId::from(config_store_id.into()),
            secret_store_id: StoreId::from(secret_store_id.into()),
        }
    }

    /// Rotates the signing key by generating a new keypair and storing it.
    ///
    /// # Errors
    ///
    /// Returns an error if key storage or update operations fail.
    pub fn rotate_key(
        &self,
        services: &RuntimeServices,
        kid: Option<String>,
    ) -> Result<KeyRotationResult, Report<TrustedServerError>> {
        let new_kid = kid.unwrap_or_else(generate_date_based_kid);

        let keypair = Keypair::generate();
        let jwk = keypair.get_jwk(new_kid.clone());
        let previous_kid = services
            .config_store()
            .get(&JWKS_STORE_NAME, "current-kid")
            .ok();

        self.store_private_key(services, &new_kid, &keypair.signing_key)?;
        self.store_public_jwk(services, &new_kid, &jwk)?;

        let active_kids = match &previous_kid {
            Some(prev) if prev != &new_kid => vec![prev.clone(), new_kid.clone()],
            _ => vec![new_kid.clone()],
        };

        self.update_current_kid(services, &new_kid)?;
        self.update_active_kids(services, &active_kids)?;

        Ok(KeyRotationResult {
            new_kid,
            previous_kid,
            active_kids,
            jwk,
        })
    }

    fn store_private_key(
        &self,
        services: &RuntimeServices,
        kid: &str,
        signing_key: &SigningKey,
    ) -> Result<(), Report<TrustedServerError>> {
        let key_b64 = general_purpose::STANDARD.encode(signing_key.as_bytes());

        services
            .secret_store()
            .create(&self.secret_store_id, kid, &key_b64)
            .change_context(TrustedServerError::Configuration {
                message: format!("failed to store private key '{}'", kid),
            })
    }

    fn store_public_jwk(
        &self,
        services: &RuntimeServices,
        kid: &str,
        jwk: &Jwk,
    ) -> Result<(), Report<TrustedServerError>> {
        let jwk_json = serde_json::to_string(jwk).map_err(|e| {
            Report::new(TrustedServerError::Configuration {
                message: format!("failed to serialize JWK: {}", e),
            })
        })?;

        services
            .config_store()
            .put(&self.config_store_id, kid, &jwk_json)
            .change_context(TrustedServerError::Configuration {
                message: format!("failed to store public JWK '{}'", kid),
            })
    }

    fn update_current_kid(
        &self,
        services: &RuntimeServices,
        kid: &str,
    ) -> Result<(), Report<TrustedServerError>> {
        services
            .config_store()
            .put(&self.config_store_id, "current-kid", kid)
            .change_context(TrustedServerError::Configuration {
                message: "failed to update current-kid".into(),
            })
    }

    fn update_active_kids(
        &self,
        services: &RuntimeServices,
        active_kids: &[String],
    ) -> Result<(), Report<TrustedServerError>> {
        let active_kids_str = active_kids.join(",");

        services
            .config_store()
            .put(&self.config_store_id, "active-kids", &active_kids_str)
            .change_context(TrustedServerError::Configuration {
                message: "failed to update active-kids".into(),
            })
    }

    /// Lists all active key IDs.
    ///
    /// # Errors
    ///
    /// Returns an error if the active keys cannot be retrieved from the config store.
    pub fn list_active_keys(
        &self,
        services: &RuntimeServices,
    ) -> Result<Vec<String>, Report<TrustedServerError>> {
        let active_kids_str = services
            .config_store()
            .get(&JWKS_STORE_NAME, "active-kids")
            .change_context(TrustedServerError::Configuration {
                message: "failed to read active-kids from config store".into(),
            })?;

        Ok(active_kids_str
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect())
    }

    /// Deactivates a key by removing it from the active keys list.
    ///
    /// # Errors
    ///
    /// Returns an error if this would deactivate the last active key, or if the update fails.
    pub fn deactivate_key(
        &self,
        services: &RuntimeServices,
        kid: &str,
    ) -> Result<(), Report<TrustedServerError>> {
        let mut active_kids = self.list_active_keys(services)?;
        active_kids.retain(|k| k != kid);

        if active_kids.is_empty() {
            return Err(Report::new(TrustedServerError::Configuration {
                message: "cannot deactivate the last active key".into(),
            }));
        }

        self.update_active_kids(services, &active_kids)
    }

    /// Deletes a key by deactivating it and removing it from storage.
    ///
    /// # Errors
    ///
    /// Returns an error if deactivation fails or if the key cannot be deleted from storage.
    pub fn delete_key(
        &self,
        services: &RuntimeServices,
        kid: &str,
    ) -> Result<(), Report<TrustedServerError>> {
        self.deactivate_key(services, kid)?;

        services
            .config_store()
            .delete(&self.config_store_id, kid)
            .change_context(TrustedServerError::Configuration {
                message: "failed to delete JWK from config store".into(),
            })?;

        services
            .secret_store()
            .delete(&self.secret_store_id, kid)
            .change_context(TrustedServerError::Configuration {
                message: "failed to delete signing key from secret store".into(),
            })?;

        Ok(())
    }
}

#[must_use]
pub fn generate_date_based_kid() -> String {
    use chrono::Utc;
    format!("ts-{}", Utc::now().format("%Y-%m-%d"))
}
```

(Append the test module from Step 1 at the bottom.)

- [ ] **Step 4: Run rotation tests**

```bash
cargo test --package trusted-server-core request_signing::rotation -- --nocapture
```

Expected: all tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/request_signing/rotation.rs
git commit -m "Migrate KeyRotationManager from FastlyApiClient to RuntimeServices store primitives"
```

---

### Task 5: Migrate `signing.rs` to `RuntimeServices`

**Why:** Three items in `signing.rs` still construct `FastlyConfigStore`/`FastlySecretStore` directly. Replace all three with `RuntimeServices`. The existing viceroy-dependent tests are replaced with proper unit tests using stub stores.

**Changed signatures:**
- `get_current_key_id()` → `get_current_key_id(services: &RuntimeServices)`
- `RequestSigner::from_config()` → `RequestSigner::from_services(services: &RuntimeServices)` (rename to make the break explicit)
- `verify_signature(payload, sig, kid)` → `verify_signature(payload, sig, kid, services: &RuntimeServices)`

**Files:**
- Modify: `crates/trusted-server-core/src/request_signing/signing.rs`

- [ ] **Step 1: Write failing tests for the new API**

Replace the entire `#[cfg(test)]` module in `signing.rs` with the following (before updating the production code, so the tests fail to compile):

```rust
#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use error_stack::Report;

    use crate::platform::test_support::build_services_with_config_and_secret;
    use crate::platform::{PlatformConfigStore, PlatformError, PlatformSecretStore, StoreId, StoreName};

    use super::*;

    // ---------------------------------------------------------------------------
    // Stub stores with preset data
    // ---------------------------------------------------------------------------

    struct StubConfigStore(HashMap<String, String>);

    impl PlatformConfigStore for StubConfigStore {
        fn get(&self, _: &StoreName, key: &str) -> Result<String, Report<PlatformError>> {
            self.0
                .get(key)
                .cloned()
                .ok_or_else(|| Report::new(PlatformError::ConfigStore))
        }

        fn put(&self, _: &StoreId, _: &str, _: &str) -> Result<(), Report<PlatformError>> {
            Err(Report::new(PlatformError::Unsupported))
        }

        fn delete(&self, _: &StoreId, _: &str) -> Result<(), Report<PlatformError>> {
            Err(Report::new(PlatformError::Unsupported))
        }
    }

    struct StubSecretStore(HashMap<String, Vec<u8>>);

    impl PlatformSecretStore for StubSecretStore {
        fn get_bytes(&self, _: &StoreName, key: &str) -> Result<Vec<u8>, Report<PlatformError>> {
            self.0
                .get(key)
                .cloned()
                .ok_or_else(|| Report::new(PlatformError::SecretStore))
        }

        fn create(&self, _: &StoreId, _: &str, _: &str) -> Result<(), Report<PlatformError>> {
            Err(Report::new(PlatformError::Unsupported))
        }

        fn delete(&self, _: &StoreId, _: &str) -> Result<(), Report<PlatformError>> {
            Err(Report::new(PlatformError::Unsupported))
        }
    }

    /// Build `RuntimeServices` with a real Ed25519 keypair pre-loaded in the
    /// stub stores.  Returns the `kid` used so callers can reference it.
    fn build_signing_services() -> crate::platform::RuntimeServices {
        use base64::{engine::general_purpose, Engine};
        use ed25519_dalek::SigningKey;
        use rand::rngs::OsRng;

        let signing_key = SigningKey::generate(&mut OsRng);
        let key_b64 = general_purpose::STANDARD.encode(signing_key.as_bytes());
        let verifying_key = signing_key.verifying_key();
        let x_b64 = general_purpose::URL_SAFE_NO_PAD.encode(verifying_key.as_bytes());
        let jwk_json = format!(
            r#"{{"kty":"OKP","crv":"Ed25519","x":"{}","kid":"test-kid","alg":"EdDSA"}}"#,
            x_b64
        );

        let mut config_data = HashMap::new();
        config_data.insert("current-kid".to_string(), "test-kid".to_string());
        config_data.insert("test-kid".to_string(), jwk_json);

        let mut secret_data = HashMap::new();
        secret_data.insert("test-kid".to_string(), key_b64.into_bytes());

        build_services_with_config_and_secret(
            StubConfigStore(config_data),
            StubSecretStore(secret_data),
        )
    }

    // ---------------------------------------------------------------------------
    // Tests
    // ---------------------------------------------------------------------------

    #[test]
    fn from_services_loads_kid_from_config_store() {
        let services = build_signing_services();
        let signer = RequestSigner::from_services(&services)
            .expect("should create signer from services");

        assert_eq!(signer.kid, "test-kid", "should load kid from config store");
    }

    #[test]
    fn sign_produces_non_empty_url_safe_base64_signature() {
        let services = build_signing_services();
        let signer = RequestSigner::from_services(&services)
            .expect("should create signer from services");

        let signature = signer
            .sign(b"these pretzels are making me thirsty")
            .expect("should sign payload");

        assert!(!signature.is_empty(), "should produce non-empty signature");
        assert!(signature.len() > 32, "should produce a full-length signature");
    }

    #[test]
    fn sign_and_verify_roundtrip_succeeds() {
        let services = build_signing_services();
        let signer = RequestSigner::from_services(&services)
            .expect("should create signer from services");
        let payload = b"test payload for verification";

        let signature = signer.sign(payload).expect("should sign payload");
        let verified = verify_signature(payload, &signature, &signer.kid, &services)
            .expect("should attempt verification");

        assert!(verified, "should verify a valid signature");
    }

    #[test]
    fn verify_returns_false_for_wrong_payload() {
        let services = build_signing_services();
        let signer = RequestSigner::from_services(&services)
            .expect("should create signer from services");
        let signature = signer.sign(b"original").expect("should sign");

        let verified = verify_signature(b"wrong payload", &signature, &signer.kid, &services)
            .expect("should attempt verification");

        assert!(!verified, "should not verify signature for wrong payload");
    }

    #[test]
    fn verify_errors_for_unknown_kid() {
        let services = build_signing_services();
        let signer = RequestSigner::from_services(&services)
            .expect("should create signer from services");
        let signature = signer.sign(b"payload").expect("should sign");

        let result = verify_signature(b"payload", &signature, "nonexistent-kid", &services);

        assert!(result.is_err(), "should error for unknown kid");
    }

    #[test]
    fn verify_errors_for_malformed_signature() {
        let services = build_signing_services();
        let signer = RequestSigner::from_services(&services)
            .expect("should create signer from services");

        let result = verify_signature(b"payload", "not-valid-base64!!!", &signer.kid, &services);

        assert!(result.is_err(), "should error for malformed signature");
    }

    #[test]
    fn signing_params_build_payload_serializes_all_fields() {
        let params = SigningParams {
            request_id: "req-123".to_string(),
            request_host: "example.com".to_string(),
            request_scheme: "https".to_string(),
            timestamp: 1706900000,
        };

        let payload = params.build_payload("kid-abc").expect("should build payload");
        let parsed: serde_json::Value =
            serde_json::from_str(&payload).expect("should be valid JSON");

        assert_eq!(parsed["version"], SIGNING_VERSION);
        assert_eq!(parsed["kid"], "kid-abc");
        assert_eq!(parsed["host"], "example.com");
        assert_eq!(parsed["scheme"], "https");
        assert_eq!(parsed["id"], "req-123");
        assert_eq!(parsed["ts"], 1706900000);
    }

    #[test]
    fn signing_params_new_creates_recent_timestamp() {
        let params = SigningParams::new(
            "req-123".to_string(),
            "example.com".to_string(),
            "https".to_string(),
        );

        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("should get system time")
            .as_millis() as u64;

        assert!(
            params.timestamp <= now_ms,
            "timestamp should not be in the future"
        );
        assert!(
            params.timestamp >= now_ms - 60_000,
            "timestamp should be within the last minute"
        );
    }

    #[test]
    fn sign_request_enhanced_produces_verifiable_signature() {
        let services = build_signing_services();
        let signer = RequestSigner::from_services(&services)
            .expect("should create signer from services");
        let params = SigningParams::new(
            "auction-123".to_string(),
            "publisher.com".to_string(),
            "https".to_string(),
        );

        let signature = signer.sign_request(&params).expect("should sign request");
        let payload = params.build_payload(&signer.kid).expect("should build payload");

        let verified =
            verify_signature(payload.as_bytes(), &signature, &signer.kid, &services)
                .expect("should verify");

        assert!(verified, "enhanced request signature should be verifiable");
    }

    #[test]
    fn sign_request_different_hosts_produce_different_signatures() {
        let services = build_signing_services();
        let signer = RequestSigner::from_services(&services)
            .expect("should create signer from services");

        let params1 = SigningParams {
            request_id: "req-1".to_string(),
            request_host: "host1.com".to_string(),
            request_scheme: "https".to_string(),
            timestamp: 1706900000,
        };
        let params2 = SigningParams {
            request_id: "req-1".to_string(),
            request_host: "host2.com".to_string(),
            request_scheme: "https".to_string(),
            timestamp: 1706900000,
        };

        let sig1 = signer.sign_request(&params1).expect("should sign params1");
        let sig2 = signer.sign_request(&params2).expect("should sign params2");

        assert_ne!(
            sig1, sig2,
            "different hosts should produce different signatures"
        );
    }
}
```

- [ ] **Step 2: Run to confirm compile failure**

```bash
cargo test --package trusted-server-core request_signing::signing 2>&1 | head -20
```

Expected: compile error — `from_services`, `verify_signature` with 4 args not found.

- [ ] **Step 3: Rewrite `signing.rs` production code**

Replace the imports, `LazyLock` statics, and function bodies. Key changes:

**Imports — replace:**
```rust
use crate::storage::{FastlyConfigStore, FastlySecretStore};
```
**With:**
```rust
use std::sync::LazyLock;

use crate::platform::{RuntimeServices, StoreName};
```

**Add after imports:**
```rust
static JWKS_STORE_NAME: LazyLock<StoreName> =
    LazyLock::new(|| StoreName::from(JWKS_CONFIG_STORE_NAME));

static SIGNING_STORE_NAME: LazyLock<StoreName> =
    LazyLock::new(|| StoreName::from(SIGNING_SECRET_STORE_NAME));
```

**Replace `get_current_key_id`:**
```rust
pub fn get_current_key_id(
    services: &RuntimeServices,
) -> Result<String, Report<TrustedServerError>> {
    services
        .config_store()
        .get(&JWKS_STORE_NAME, "current-kid")
        .change_context(TrustedServerError::Configuration {
            message: "failed to read current-kid from config store".into(),
        })
}
```

**Replace `RequestSigner::from_config` with `from_services`:**
```rust
pub fn from_services(
    services: &RuntimeServices,
) -> Result<Self, Report<TrustedServerError>> {
    let key_id = services
        .config_store()
        .get(&JWKS_STORE_NAME, "current-kid")
        .change_context(TrustedServerError::Configuration {
            message: "failed to get current-kid".into(),
        })?;

    let key_bytes = services
        .secret_store()
        .get_bytes(&SIGNING_STORE_NAME, &key_id)
        .change_context(TrustedServerError::Configuration {
            message: format!("failed to get signing key for kid: {}", key_id),
        })?;

    let signing_key = parse_ed25519_signing_key(key_bytes)?;

    Ok(Self {
        key: signing_key,
        kid: key_id,
    })
}
```

**Replace `verify_signature` — add `services: &RuntimeServices` parameter and replace `FastlyConfigStore::new(...)` with `services.config_store().get(&JWKS_STORE_NAME, kid)`.**

Full new signature:
```rust
pub fn verify_signature(
    payload: &[u8],
    signature_b64: &str,
    kid: &str,
    services: &RuntimeServices,
) -> Result<bool, Report<TrustedServerError>> {
    let jwk_json = services
        .config_store()
        .get(&JWKS_STORE_NAME, kid)
        .change_context(TrustedServerError::Configuration {
            message: format!("failed to get JWK for kid: {}", kid),
        })?;
    // ... rest of verification logic unchanged ...
}
```

- [ ] **Step 4: Run signing tests**

```bash
cargo test --package trusted-server-core request_signing::signing -- --nocapture
```

Expected: all tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/request_signing/signing.rs
git commit -m "Migrate signing.rs from FastlyConfigStore/FastlySecretStore to RuntimeServices"
```

---

### Task 6: Update `endpoints.rs` to accept `&RuntimeServices`

**Why:** Three handlers don't receive `&RuntimeServices`: `handle_verify_signature`, `handle_rotate_key`, `handle_deactivate_key`. They need it to pass to `verify_signature`, `KeyRotationManager` methods, and (for verify) `RequestSigner::from_services`.

**Note:** `fastly::{Request, Response}` and `fastly::mime` remain — type migration is Phase 2 (PR 12).

**Files:**
- Modify: `crates/trusted-server-core/src/request_signing/endpoints.rs`

- [ ] **Step 1: Update `handle_verify_signature` signature and body**

Change:
```rust
pub fn handle_verify_signature(
    _settings: &Settings,
    mut req: Request,
) -> Result<Response, Report<TrustedServerError>> {
```

To:
```rust
pub fn handle_verify_signature(
    _settings: &Settings,
    services: &RuntimeServices,
    mut req: Request,
) -> Result<Response, Report<TrustedServerError>> {
```

Update the `verify_signature` call:
```rust
let verification_result = signing::verify_signature(
    verify_req.payload.as_bytes(),
    &verify_req.signature,
    &verify_req.kid,
    services,
);
```

- [ ] **Step 2: Update `handle_rotate_key` signature and body**

Change signature to add `services: &RuntimeServices` as second parameter. Update the `KeyRotationManager` usage:

```rust
// Before:
let manager = KeyRotationManager::new(config_store_id, secret_store_id).change_context(...)?;
match manager.rotate_key(rotate_req.kid) { ... }
manager.list_active_keys().unwrap_or_else(...)

// After:
let manager = KeyRotationManager::new(config_store_id, secret_store_id);
match manager.rotate_key(services, rotate_req.kid) { ... }
manager.list_active_keys(services).unwrap_or_else(...)
```

Remove the `.change_context(...)` on `KeyRotationManager::new(...)` — it's now infallible.

- [ ] **Step 3: Update `handle_deactivate_key` signature and body**

Same pattern: add `services: &RuntimeServices`, update all `manager.*` calls to pass `services`:
- `manager.delete_key(&deactivate_req.kid)` → `manager.delete_key(services, &deactivate_req.kid)`
- `manager.deactivate_key(&deactivate_req.kid)` → `manager.deactivate_key(services, &deactivate_req.kid)`
- `manager.list_active_keys()` → `manager.list_active_keys(services)`

Remove the `.change_context(...)` on `KeyRotationManager::new(...)`.

- [ ] **Step 4: Update `endpoints.rs` tests**

The tests in `endpoints.rs` that call `handle_verify_signature`, `handle_rotate_key`, `handle_deactivate_key` must be updated to pass a `&RuntimeServices`. Use `noop_services()` (from `test_support`) for rotation/deactivation tests (they test error paths that don't reach the stores). For `test_handle_verify_signature_valid` and `test_handle_verify_signature_invalid`, build a `RuntimeServices` with actual key material using `build_signing_services` (inline the helper or import logic).

Also update `RequestSigner::from_config()` calls in test helpers to `RequestSigner::from_services(&services)`.

**Add this helper to the `#[cfg(test)]` block at the top of `endpoints.rs` tests** (it cannot be imported from `signing.rs` because that function lives inside a `#[cfg(test)]` private module):

```rust
/// Build `RuntimeServices` pre-loaded with a real Ed25519 keypair for
/// testing signature creation and verification in endpoint handlers.
fn build_signing_services_for_test() -> crate::platform::RuntimeServices {
    use std::collections::HashMap;

    use base64::{engine::general_purpose, Engine};
    use ed25519_dalek::SigningKey;
    use error_stack::Report;
    use rand::rngs::OsRng;

    use crate::platform::test_support::build_services_with_config_and_secret;
    use crate::platform::{
        PlatformConfigStore, PlatformError, PlatformSecretStore, StoreId, StoreName,
    };

    struct MapConfigStore(HashMap<String, String>);
    impl PlatformConfigStore for MapConfigStore {
        fn get(&self, _: &StoreName, key: &str) -> Result<String, Report<PlatformError>> {
            self.0.get(key).cloned().ok_or_else(|| Report::new(PlatformError::ConfigStore))
        }
        fn put(&self, _: &StoreId, _: &str, _: &str) -> Result<(), Report<PlatformError>> {
            Err(Report::new(PlatformError::Unsupported))
        }
        fn delete(&self, _: &StoreId, _: &str) -> Result<(), Report<PlatformError>> {
            Err(Report::new(PlatformError::Unsupported))
        }
    }

    struct MapSecretStore(HashMap<String, Vec<u8>>);
    impl PlatformSecretStore for MapSecretStore {
        fn get_bytes(&self, _: &StoreName, key: &str) -> Result<Vec<u8>, Report<PlatformError>> {
            self.0.get(key).cloned().ok_or_else(|| Report::new(PlatformError::SecretStore))
        }
        fn create(&self, _: &StoreId, _: &str, _: &str) -> Result<(), Report<PlatformError>> {
            Err(Report::new(PlatformError::Unsupported))
        }
        fn delete(&self, _: &StoreId, _: &str) -> Result<(), Report<PlatformError>> {
            Err(Report::new(PlatformError::Unsupported))
        }
    }

    let signing_key = SigningKey::generate(&mut OsRng);
    let key_b64 = general_purpose::STANDARD.encode(signing_key.as_bytes());
    let x_b64 = general_purpose::URL_SAFE_NO_PAD.encode(signing_key.verifying_key().as_bytes());
    let jwk_json = format!(
        r#"{{"kty":"OKP","crv":"Ed25519","x":"{}","kid":"test-kid","alg":"EdDSA"}}"#,
        x_b64
    );

    let mut cfg = HashMap::new();
    cfg.insert("current-kid".to_string(), "test-kid".to_string());
    cfg.insert("test-kid".to_string(), jwk_json);

    let mut sec = HashMap::new();
    sec.insert("test-kid".to_string(), key_b64.into_bytes());

    build_services_with_config_and_secret(MapConfigStore(cfg), MapSecretStore(sec))
}
```

Pattern for verify tests:
```rust
// In test_handle_verify_signature_valid:
let services = build_signing_services_for_test();
let signer = crate::request_signing::RequestSigner::from_services(&services)
    .expect("should create signer from services");
// ... build req as before ...
let mut resp = handle_verify_signature(&settings, &services, req)
    .expect("should handle verification request");
```

For rotation/deactivation tests, `noop_services()` is fine — these tests use the `match result { Ok => log, Err => log }` pattern and do not assert against store state. The `noop_services()` causes `KeyRotationManager` methods to fail at the store read/write level, which is the expected behavior in a test environment without real stores:
```rust
let services = noop_services();
let result = handle_rotate_key(&settings, &services, req);
// existing match-and-log pattern works unchanged
```

- [ ] **Step 5: Run endpoints tests**

```bash
cargo test --package trusted-server-core request_signing::endpoints -- --nocapture
```

Expected: all tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/trusted-server-core/src/request_signing/endpoints.rs
git commit -m "Add RuntimeServices parameter to handle_verify_signature, handle_rotate_key, handle_deactivate_key"
```

---

### Task 7: Update `main.rs` to pass `runtime_services` to updated handlers

**Why:** The adapter `main.rs` calls the three handlers without `runtime_services`. Add it. `runtime_services` is already in scope in all call sites.

**Files:**
- Modify: `crates/trusted-server-adapter-fastly/src/main.rs`

- [ ] **Step 1: Update the three handler call sites**

Find (approximate lines, verify exact lines before editing):

```rust
(Method::POST, "/verify-signature") => handle_verify_signature(settings, req),
(Method::POST, "/admin/keys/rotate") => handle_rotate_key(settings, req),
(Method::POST, "/admin/keys/deactivate") => handle_deactivate_key(settings, req),
```

Replace with:

```rust
(Method::POST, "/verify-signature") => {
    handle_verify_signature(settings, runtime_services, req)
}
(Method::POST, "/admin/keys/rotate") => {
    handle_rotate_key(settings, runtime_services, req)
}
(Method::POST, "/admin/keys/deactivate") => {
    handle_deactivate_key(settings, runtime_services, req)
}
```

- [ ] **Step 2: Verify the full workspace compiles**

```bash
cargo check --workspace
```

Expected: no errors.

- [ ] **Step 3: Commit**

```bash
git add crates/trusted-server-adapter-fastly/src/main.rs
git commit -m "Pass runtime_services to signing endpoint handlers in main.rs"
```

---

### Task 8: Delete `api_client.rs` and clean up `storage/mod.rs`

**Why:** `api_client.rs` is now fully superseded by `management_api.rs` in the adapter. No core code references `FastlyApiClient` anymore (verified by rotation.rs migration).

**Files:**
- Delete: `crates/trusted-server-core/src/storage/api_client.rs`
- Modify: `crates/trusted-server-core/src/storage/mod.rs`

- [ ] **Step 1: Verify zero legacy storage imports remain in `request_signing/`**

```bash
grep -r "FastlyApiClient\|FastlyConfigStore\|FastlySecretStore" crates/trusted-server-core/src/request_signing/
```

Expected: no output. If any matches appear, fix those call sites before continuing.

Also verify no remaining references to `FastlyApiClient` anywhere in core:

```bash
grep -r "FastlyApiClient" crates/trusted-server-core/src/
```

Expected: no output.

- [ ] **Step 2: Delete `api_client.rs`**

```bash
rm crates/trusted-server-core/src/storage/api_client.rs
```

- [ ] **Step 3: Update `storage/mod.rs`**

Remove the `api_client` module declaration and re-export. Change from:

```rust
//! Legacy Fastly-backed store types.
//!
//! These types predate the [`crate::platform`] abstraction and will be removed
//! once all call sites have migrated to the platform traits. New code should
//! use [`crate::platform::PlatformConfigStore`],
//! [`crate::platform::PlatformSecretStore`], and the management write methods
//! via [`crate::platform::RuntimeServices`].

pub(crate) mod api_client;
pub(crate) mod config_store;
pub(crate) mod secret_store;

pub use api_client::FastlyApiClient;
pub use config_store::FastlyConfigStore;
pub use secret_store::FastlySecretStore;
```

To:

```rust
//! Legacy Fastly-backed store types.
//!
//! These types predate the [`crate::platform`] abstraction and will be removed
//! once all call sites have migrated to the platform traits. New code should
//! use [`crate::platform::PlatformConfigStore`] and
//! [`crate::platform::PlatformSecretStore`] via [`crate::platform::RuntimeServices`].

pub(crate) mod config_store;
pub(crate) mod secret_store;

pub use config_store::FastlyConfigStore;
pub use secret_store::FastlySecretStore;
```

- [ ] **Step 4: Run the full workspace test suite**

```bash
cargo test --workspace
```

Expected: all tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/storage/mod.rs
git rm crates/trusted-server-core/src/storage/api_client.rs
git commit -m "Delete storage/api_client.rs from core; remove FastlyApiClient"
```

---

### Task 9: Run CI gates

- [ ] **Step 1: Format check**

```bash
cargo fmt --all -- --check
```

If it fails, fix with `cargo fmt --all` and re-run.

- [ ] **Step 2: Clippy**

```bash
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Fix any lints before proceeding.

- [ ] **Step 3: Full test suite**

```bash
cargo test --workspace
```

Expected: all tests pass.

- [ ] **Step 4: Commit any lint/format fixes**

```bash
git add -A
git commit -m "Fix clippy lints and formatting"
```

Only create this commit if there are actual changes.

---

## Acceptance Checklist

Verify all of the following before considering PR 9 complete:

- [ ] `crates/trusted-server-core/src/storage/api_client.rs` no longer exists
- [ ] `crates/trusted-server-adapter-fastly/src/management_api.rs` exists
- [ ] `grep -r "FastlyApiClient\|from crate::storage::api" crates/trusted-server-core/src/request_signing/` returns no matches
- [ ] `grep -r "FastlyConfigStore\|FastlySecretStore" crates/trusted-server-core/src/request_signing/` returns no matches
- [ ] `cargo test --workspace` passes
- [ ] `cargo clippy --workspace --all-targets --all-features -- -D warnings` passes
- [ ] `cargo fmt --all -- --check` passes
- [ ] `handle_verify_signature`, `handle_rotate_key`, `handle_deactivate_key` in `endpoints.rs` all accept `&RuntimeServices`
- [ ] `FastlyPlatformConfigStore::put/delete` and `FastlyPlatformSecretStore::create/delete` in `platform.rs` no longer return `PlatformError::NotImplemented`
