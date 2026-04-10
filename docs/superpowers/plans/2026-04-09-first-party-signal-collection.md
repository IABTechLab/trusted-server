# First-Party Signal Collection Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Harvest partner UIDs from first-party cookies on Chrome requests and write them to the KV identity graph so Safari/Firefox household members arrive with a fully populated entry.

**Architecture:** Core crate exposes `extract_fp_signals()` (pure, pre-send) and `write_fp_signals()` (batched CAS, post-send). Adapter orchestrates timing: extraction during request handling, writing after response is sent. New `_fp_signal_enabled` denormalized index on `PartnerStore` provides all partner configs in a single KV read.

**Tech Stack:** Rust (2024 edition), `serde_json` for JSON path walking, `cookie::CookieJar` for cookie access, Fastly KV Store for persistence, `error-stack` for error handling.

---

## File Structure

| File                                               | Responsibility                                                                                                                                                                                                                                     |
| -------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `crates/trusted-server-core/src/ec/fp_signals.rs`  | **New.** Types (`FpSignalPartnerConfig`, `FpSignal`, `FpSignalError`), extraction logic, JSON path walker, batched CAS write, all unit tests.                                                                                                      |
| `crates/trusted-server-core/src/ec/mod.rs`         | Declare `pub mod fp_signals`. Add `cookie_jar` field to `EcContext`, store it during `read_from_request_with_geo`, expose via `cookie_jar()` accessor.                                                                                             |
| `crates/trusted-server-core/src/ec/partner.rs`     | Add 3 FP signal fields to `PartnerRecord`. Add `FP_SIGNAL_INDEX_KEY` constant. Add `fp_signal_configs()` accessor. Add `update_fp_signal_index()` and call it from `upsert()`. Skip new index key in `list_registered()`. Add validation function. |
| `crates/trusted-server-core/src/ec/admin.rs`       | Add 3 FP signal fields to `RegisterPartnerRequest`. Wire them into `PartnerRecord` construction and validation.                                                                                                                                    |
| `crates/trusted-server-adapter-fastly/src/main.rs` | Expand `RouteOutcome` to carry `fp_signals` and `fp_signal_configs`. Extract signals pre-send. Write signals post-send via `run_fp_signal_collection_after_send()`.                                                                                |

---

### Task 1: Add `FpSignalPartnerConfig` and `FpSignal` types

**Files:**

- Create: `crates/trusted-server-core/src/ec/fp_signals.rs`
- Modify: `crates/trusted-server-core/src/ec/mod.rs:30-43`

- [ ] **Step 1: Create `fp_signals.rs` with types and error**

Create `crates/trusted-server-core/src/ec/fp_signals.rs`:

```rust
//! First-party signal collection.
//!
//! Harvests partner UIDs from first-party cookies on incoming requests
//! and writes them to the KV identity graph in a single batched CAS
//! operation. Extraction runs pre-send (cheap string ops); writing runs
//! post-send (KV I/O off the critical path).

use serde::{Deserialize, Serialize};

/// Denormalized per-partner extraction config stored in the
/// `_fp_signal_enabled` KV index.
///
/// Contains everything needed to extract a partner UID from the cookie
/// jar without reading the full [`super::partner::PartnerRecord`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FpSignalPartnerConfig {
    /// Partner identifier (matches [`super::partner::PartnerRecord::id`]).
    pub partner_id: String,
    /// Cookie names to check, in priority order. First match wins.
    pub cookie_names: Vec<String>,
    /// Dot-notation JSON path to extract the UID from a JSON cookie
    /// value. When `None`, the raw cookie value is used.
    pub json_path: Option<String>,
    /// Minimum seconds between re-collection writes for this partner.
    pub ttl_sec: u64,
}

/// A partner UID extracted from a first-party cookie.
///
/// Produced by [`extract_fp_signals`] pre-send and consumed by
/// [`write_fp_signals`] post-send.
#[derive(Debug, Clone, PartialEq)]
pub struct FpSignal {
    /// Partner identifier.
    pub partner_id: String,
    /// The extracted UID value.
    pub uid: String,
}

/// Errors specific to first-party signal collection.
#[derive(Debug, derive_more::Display)]
pub enum FpSignalError {
    /// KV store operation failed during batched write.
    #[display("KV write failed: {message}")]
    KvWrite { message: String },
}

impl core::error::Error for FpSignalError {}
```

- [ ] **Step 2: Declare the module in `ec/mod.rs`**

In `crates/trusted-server-core/src/ec/mod.rs`, add after `pub mod pull_sync;` (line 42):

```rust
pub mod fp_signals;
```

- [ ] **Step 3: Verify compilation**

Run: `cargo check --workspace`
Expected: compiles cleanly.

- [ ] **Step 4: Commit**

```bash
git add crates/trusted-server-core/src/ec/fp_signals.rs crates/trusted-server-core/src/ec/mod.rs
git commit -m "Add FpSignalPartnerConfig, FpSignal, and FpSignalError types"
```

---

### Task 2: Implement JSON path extraction with tests

**Files:**

- Modify: `crates/trusted-server-core/src/ec/fp_signals.rs`
- Test: `crates/trusted-server-core/src/ec/fp_signals.rs` (inline `#[cfg(test)]`)

- [ ] **Step 1: Write failing tests for JSON path extraction**

Append to `crates/trusted-server-core/src/ec/fp_signals.rs`:

```rust
/// Extracts a string value from a JSON string using dot-notation path.
///
/// Splits `path` on `.` and walks the `serde_json::Value` tree.
/// Returns `Some(string)` if the leaf is a JSON string, `None` on any
/// failure (invalid JSON, wrong type, missing key).
fn extract_json_path(value: &str, path: &str) -> Option<String> {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_path_extracts_top_level_key() {
        let json = r#"{"universal_uid":"ID5*abc123","version":1}"#;
        let result = extract_json_path(json, "universal_uid");
        assert_eq!(
            result.as_deref(),
            Some("ID5*abc123"),
            "should extract top-level string key"
        );
    }

    #[test]
    fn json_path_extracts_nested_key() {
        let json = r#"{"v":{"userId":"d8f4e2a1"}}"#;
        let result = extract_json_path(json, "v.userId");
        assert_eq!(
            result.as_deref(),
            Some("d8f4e2a1"),
            "should extract nested key via dot path"
        );
    }

    #[test]
    fn json_path_returns_none_for_missing_key() {
        let json = r#"{"universal_uid":"ID5*abc123"}"#;
        let result = extract_json_path(json, "nonexistent");
        assert!(result.is_none(), "should return None for missing key");
    }

    #[test]
    fn json_path_returns_none_for_non_string_leaf() {
        let json = r#"{"count":42}"#;
        let result = extract_json_path(json, "count");
        assert!(
            result.is_none(),
            "should return None when leaf is not a string"
        );
    }

    #[test]
    fn json_path_returns_none_for_invalid_json() {
        let result = extract_json_path("not-json", "key");
        assert!(result.is_none(), "should return None for invalid JSON");
    }

    #[test]
    fn json_path_returns_none_for_empty_path() {
        let json = r#"{"key":"value"}"#;
        let result = extract_json_path(json, "");
        assert!(result.is_none(), "should return None for empty path");
    }

    #[test]
    fn json_path_returns_none_for_partial_path() {
        let json = r#"{"v":{"userId":"d8f4e2a1"}}"#;
        let result = extract_json_path(json, "v.nonexistent");
        assert!(
            result.is_none(),
            "should return None when nested key is missing"
        );
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --package trusted-server-core --lib ec::fp_signals::tests -- --no-capture 2>&1 | head -30`
Expected: FAIL — `todo!()` panics.

- [ ] **Step 3: Implement `extract_json_path`**

Replace the `todo!()` body:

```rust
fn extract_json_path(value: &str, path: &str) -> Option<String> {
    if path.is_empty() {
        return None;
    }

    let parsed: serde_json::Value = serde_json::from_str(value).ok()?;
    let mut current = &parsed;

    for segment in path.split('.') {
        current = current.get(segment)?;
    }

    current.as_str().map(str::to_owned)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --package trusted-server-core --lib ec::fp_signals::tests`
Expected: all 7 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/ec/fp_signals.rs
git commit -m "Implement JSON path extraction for first-party signal cookies"
```

---

### Task 3: Implement `extract_fp_signals` with tests

**Files:**

- Modify: `crates/trusted-server-core/src/ec/fp_signals.rs`

- [ ] **Step 1: Write failing tests for cookie extraction**

Add these tests to the existing `mod tests` block in `fp_signals.rs`:

```rust
    use cookie::{Cookie, CookieJar};

    fn jar_with(cookies: &[(&str, &str)]) -> CookieJar {
        let mut jar = CookieJar::new();
        for &(name, value) in cookies {
            jar.add_original(Cookie::new(name.to_owned(), value.to_owned()));
        }
        jar
    }

    #[test]
    fn extract_raw_cookie_value() {
        let jar = jar_with(&[("lockr_tracking_id", "16d913a7-d56c-4b2f")]);
        let configs = vec![FpSignalPartnerConfig {
            partner_id: "lockr".to_owned(),
            cookie_names: vec!["lockr_tracking_id".to_owned()],
            json_path: None,
            ttl_sec: 86400,
        }];

        let signals = extract_fp_signals(&jar, &configs, 1_700_000_000_000);

        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0].partner_id, "lockr");
        assert_eq!(signals[0].uid, "16d913a7-d56c-4b2f");
    }

    #[test]
    fn extract_json_cookie_value() {
        let jar = jar_with(&[(
            "id5id",
            r#"{"universal_uid":"ID5*abc","version":1}"#,
        )]);
        let configs = vec![FpSignalPartnerConfig {
            partner_id: "id5".to_owned(),
            cookie_names: vec!["id5id".to_owned()],
            json_path: Some("universal_uid".to_owned()),
            ttl_sec: 86400,
        }];

        let signals = extract_fp_signals(&jar, &configs, 1_700_000_000_000);

        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0].uid, "ID5*abc");
    }

    #[test]
    fn extract_first_match_wins() {
        let jar = jar_with(&[("_sharedid", "shared-uid-456")]);
        let configs = vec![FpSignalPartnerConfig {
            partner_id: "prebid_sharedid".to_owned(),
            cookie_names: vec![
                "sharedId".to_owned(),
                "_sharedid".to_owned(),
                "_sharedID".to_owned(),
            ],
            json_path: None,
            ttl_sec: 86400,
        }];

        let signals = extract_fp_signals(&jar, &configs, 1_700_000_000_000);

        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0].uid, "shared-uid-456");
    }

    #[test]
    fn extract_skips_missing_cookie() {
        let jar = jar_with(&[]);
        let configs = vec![FpSignalPartnerConfig {
            partner_id: "lockr".to_owned(),
            cookie_names: vec!["lockr_tracking_id".to_owned()],
            json_path: None,
            ttl_sec: 86400,
        }];

        let signals = extract_fp_signals(&jar, &configs, 1_700_000_000_000);

        assert!(signals.is_empty(), "should skip when cookie is missing");
    }

    #[test]
    fn extract_skips_empty_uid() {
        let jar = jar_with(&[("lockr_tracking_id", "   ")]);
        let configs = vec![FpSignalPartnerConfig {
            partner_id: "lockr".to_owned(),
            cookie_names: vec!["lockr_tracking_id".to_owned()],
            json_path: None,
            ttl_sec: 86400,
        }];

        let signals = extract_fp_signals(&jar, &configs, 1_700_000_000_000);

        assert!(signals.is_empty(), "should skip empty/whitespace UID");
    }

    #[test]
    fn extract_skips_failed_json_path() {
        let jar = jar_with(&[("id5id", "not-json")]);
        let configs = vec![FpSignalPartnerConfig {
            partner_id: "id5".to_owned(),
            cookie_names: vec!["id5id".to_owned()],
            json_path: Some("universal_uid".to_owned()),
            ttl_sec: 86400,
        }];

        let signals = extract_fp_signals(&jar, &configs, 1_700_000_000_000);

        assert!(signals.is_empty(), "should skip when JSON path fails");
    }

    #[test]
    fn extract_nested_json_path() {
        let jar = jar_with(&[("krg_uid", r#"{"v":{"userId":"d8f4e2a1"}}"#)]);
        let configs = vec![FpSignalPartnerConfig {
            partner_id: "kargo".to_owned(),
            cookie_names: vec!["krg_uid".to_owned()],
            json_path: Some("v.userId".to_owned()),
            ttl_sec: 86400,
        }];

        let signals = extract_fp_signals(&jar, &configs, 1_700_000_000_000);

        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0].uid, "d8f4e2a1");
    }

    #[test]
    fn extract_multiple_partners() {
        let jar = jar_with(&[
            ("lockr_tracking_id", "lockr-uid"),
            ("panoramaId", "lotame-uid"),
        ]);
        let configs = vec![
            FpSignalPartnerConfig {
                partner_id: "lockr".to_owned(),
                cookie_names: vec!["lockr_tracking_id".to_owned()],
                json_path: None,
                ttl_sec: 86400,
            },
            FpSignalPartnerConfig {
                partner_id: "lotame".to_owned(),
                cookie_names: vec!["panoramaId".to_owned()],
                json_path: None,
                ttl_sec: 86400,
            },
        ];

        let signals = extract_fp_signals(&jar, &configs, 1_700_000_000_000);

        assert_eq!(signals.len(), 2, "should extract both partners");
    }
```

- [ ] **Step 2: Write the `extract_fp_signals` function stub**

Add above the `#[cfg(test)]` block:

```rust
use cookie::CookieJar;

/// Extracts partner UIDs from first-party cookies.
///
/// Iterates partner configs, checks the cookie jar for each partner's
/// cookie names, and extracts UIDs using optional JSON path notation.
/// UID2 tokens are only extracted if they have at least 5 minutes of
/// validity remaining.
///
/// This is a pure function with no I/O — safe to call on the hot path.
///
/// # Arguments
///
/// * `jar` — the parsed cookie jar from the incoming request
/// * `configs` — partner extraction configs from the `_fp_signal_enabled` index
/// * `now_ms` — current time in milliseconds (for UID2 expiry check)
pub fn extract_fp_signals(
    jar: &CookieJar,
    configs: &[FpSignalPartnerConfig],
    now_ms: u64,
) -> Vec<FpSignal> {
    todo!()
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test --package trusted-server-core --lib ec::fp_signals::tests -- --no-capture 2>&1 | head -30`
Expected: FAIL — `todo!()` panics.

- [ ] **Step 4: Implement `extract_fp_signals`**

Replace the `todo!()` body:

```rust
pub fn extract_fp_signals(
    jar: &CookieJar,
    configs: &[FpSignalPartnerConfig],
    now_ms: u64,
) -> Vec<FpSignal> {
    let mut signals = Vec::new();

    for config in configs {
        let cookie_value = config
            .cookie_names
            .iter()
            .find_map(|name| jar.get(name.as_str()))
            .map(|c| c.value());

        let Some(raw_value) = cookie_value else {
            continue;
        };

        // UID2 expiry check: if the cookie contains `identity_expires`,
        // only extract when the token has >= 5 minutes of validity.
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(raw_value) {
            if let Some(expires) = parsed.get("identity_expires").and_then(|v| v.as_u64()) {
                if expires <= now_ms + 300_000 {
                    log::debug!(
                        "Skipping expired UID2 token for partner '{}' (expires={}, now={})",
                        config.partner_id,
                        expires,
                        now_ms,
                    );
                    continue;
                }
            }
        }

        let uid = match &config.json_path {
            Some(path) => match extract_json_path(raw_value, path) {
                Some(uid) => uid,
                None => {
                    log::debug!(
                        "JSON path '{}' extraction failed for partner '{}'",
                        path,
                        config.partner_id,
                    );
                    continue;
                }
            },
            None => raw_value.to_owned(),
        };

        let trimmed = uid.trim();
        if trimmed.is_empty() {
            continue;
        }

        signals.push(FpSignal {
            partner_id: config.partner_id.clone(),
            uid: trimmed.to_owned(),
        });
    }

    signals
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --package trusted-server-core --lib ec::fp_signals::tests`
Expected: all tests PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/trusted-server-core/src/ec/fp_signals.rs
git commit -m "Implement first-party signal extraction from cookie jar"
```

---

### Task 4: Add UID2 expiry tests

**Files:**

- Modify: `crates/trusted-server-core/src/ec/fp_signals.rs`

- [ ] **Step 1: Write UID2 expiry tests**

Add to `mod tests`:

```rust
    #[test]
    fn extract_uid2_with_valid_token() {
        let now_ms = 1_775_000_000_000;
        let expires = now_ms + 600_000; // 10 min remaining
        let cookie_value = format!(
            r#"{{"advertising_token":"A4AAADA","refresh_token":"secret","identity_expires":{expires}}}"#,
        );
        let jar = jar_with(&[("__uid2_advertising_token", &cookie_value)]);
        let configs = vec![FpSignalPartnerConfig {
            partner_id: "uid2".to_owned(),
            cookie_names: vec!["__uid2_advertising_token".to_owned()],
            json_path: Some("advertising_token".to_owned()),
            ttl_sec: 3600,
        }];

        let signals = extract_fp_signals(&jar, &configs, now_ms);

        assert_eq!(signals.len(), 1, "should extract valid UID2 token");
        assert_eq!(signals[0].uid, "A4AAADA");
    }

    #[test]
    fn extract_uid2_skips_expired_token() {
        let now_ms = 1_775_000_000_000;
        let expires = now_ms + 200_000; // Only 200s remaining (< 300s threshold)
        let cookie_value = format!(
            r#"{{"advertising_token":"A4AAADA","refresh_token":"secret","identity_expires":{expires}}}"#,
        );
        let jar = jar_with(&[("__uid2_advertising_token", &cookie_value)]);
        let configs = vec![FpSignalPartnerConfig {
            partner_id: "uid2".to_owned(),
            cookie_names: vec!["__uid2_advertising_token".to_owned()],
            json_path: Some("advertising_token".to_owned()),
            ttl_sec: 3600,
        }];

        let signals = extract_fp_signals(&jar, &configs, now_ms);

        assert!(signals.is_empty(), "should skip UID2 token with <5 min validity");
    }

    #[test]
    fn extract_non_uid2_json_without_identity_expires() {
        let jar = jar_with(&[(
            "id5id",
            r#"{"universal_uid":"ID5*abc","version":1}"#,
        )]);
        let configs = vec![FpSignalPartnerConfig {
            partner_id: "id5".to_owned(),
            cookie_names: vec!["id5id".to_owned()],
            json_path: Some("universal_uid".to_owned()),
            ttl_sec: 86400,
        }];

        let signals = extract_fp_signals(&jar, &configs, 1_700_000_000_000);

        assert_eq!(signals.len(), 1, "should extract non-UID2 JSON cookie normally");
        assert_eq!(signals[0].uid, "ID5*abc");
    }
```

- [ ] **Step 2: Run tests**

Run: `cargo test --package trusted-server-core --lib ec::fp_signals::tests`
Expected: all tests PASS (UID2 logic is already implemented in Task 3).

- [ ] **Step 3: Commit**

```bash
git add crates/trusted-server-core/src/ec/fp_signals.rs
git commit -m "Add UID2 expiry check tests for first-party signal extraction"
```

---

### Task 5: Expose `CookieJar` from `EcContext`

**Files:**

- Modify: `crates/trusted-server-core/src/ec/mod.rs:139-161` (struct), `193-238` (read_from_request_with_geo), `407-464` (test helpers)

- [ ] **Step 1: Add `cookie_jar` field to `EcContext` struct**

In `crates/trusted-server-core/src/ec/mod.rs`, add after the `device_signals` field (line 160):

```rust
    /// The parsed cookie jar from the incoming request.
    /// Retained for first-party signal extraction in the post-EC phase.
    cookie_jar: Option<CookieJar>,
```

- [ ] **Step 2: Store the jar in `read_from_request_with_geo`**

In the `Ok(Self { ... })` block at line 228, add:

```rust
            cookie_jar: parsed.jar,
```

- [ ] **Step 3: Add `cookie_jar()` accessor**

After the `ec_allowed()` method (line 379), add:

```rust
    /// Returns a reference to the parsed cookie jar from the incoming request.
    ///
    /// Used by first-party signal extraction to read partner cookies without
    /// re-parsing the `Cookie` header.
    #[must_use]
    pub fn cookie_jar(&self) -> Option<&CookieJar> {
        self.cookie_jar.as_ref()
    }
```

- [ ] **Step 4: Update test helpers to include `cookie_jar: None`**

In `new_for_test` (line 410), `new_for_test_with_ip` (line 426), and `new_for_test_with_cookie` (line 447), add `cookie_jar: None` to each `Self { ... }` block.

- [ ] **Step 5: Run tests**

Run: `cargo test --package trusted-server-core --lib ec::tests`
Expected: all existing `EcContext` tests PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/trusted-server-core/src/ec/mod.rs
git commit -m "Expose parsed CookieJar from EcContext for signal extraction"
```

---

### Task 6: Add FP signal fields to `PartnerRecord`

**Files:**

- Modify: `crates/trusted-server-core/src/ec/partner.rs:62-115` (struct), `707-984` (tests)

- [ ] **Step 1: Add 3 fields to `PartnerRecord`**

In `crates/trusted-server-core/src/ec/partner.rs`, add after the `ts_pull_token` field (line 114), before the closing `}`:

```rust
    /// First-party cookie names that may carry this partner's UID.
    /// Checked in order; first match wins.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fp_signal_cookie_names: Vec<String>,
    /// Dot-notation JSON path to extract the UID from a JSON cookie
    /// value. When `None`, the raw cookie value is used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fp_signal_json_path: Option<String>,
    /// Minimum seconds between re-collection writes for this partner.
    /// Defaults to 86400 (24 hours).
    #[serde(default = "PartnerRecord::default_fp_signal_ttl_sec")]
    pub fp_signal_ttl_sec: u64,
```

- [ ] **Step 2: Add default method**

Add an `impl PartnerRecord` block (or extend if one exists) after the struct definition:

```rust
impl PartnerRecord {
    fn default_fp_signal_ttl_sec() -> u64 {
        86400
    }
}
```

- [ ] **Step 3: Update every `PartnerRecord { ... }` literal in tests**

Every existing `PartnerRecord` construction in `partner.rs` tests and `admin.rs` tests needs the 3 new fields. Add to each:

```rust
            fp_signal_cookie_names: vec![],
            fp_signal_json_path: None,
            fp_signal_ttl_sec: 86400,
```

Also search all other files that construct `PartnerRecord` literals (use `grep -rn "PartnerRecord {" crates/` to find them all). Each must include the new fields.

- [ ] **Step 4: Run full workspace tests**

Run: `cargo test --workspace`
Expected: all tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/ec/partner.rs
git commit -m "Add first-party signal fields to PartnerRecord"
```

---

### Task 7: Add FP signal validation

**Files:**

- Modify: `crates/trusted-server-core/src/ec/partner.rs`

- [ ] **Step 1: Write failing validation tests**

Add to `mod tests` in `partner.rs`:

```rust
    #[test]
    fn validate_fp_signal_config_accepts_valid() {
        let result = validate_fp_signal_config(&PartnerRecord {
            id: "id5".to_owned(),
            name: "ID5".to_owned(),
            allowed_return_domains: vec!["id5.com".to_owned()],
            api_key_hash: String::new(),
            bidstream_enabled: false,
            source_domain: "id5-sync.com".to_owned(),
            openrtb_atype: 3,
            sync_rate_limit: 100,
            batch_rate_limit: 60,
            pull_sync_enabled: false,
            pull_sync_url: None,
            pull_sync_allowed_domains: vec![],
            pull_sync_ttl_sec: 86400,
            pull_sync_rate_limit: 10,
            ts_pull_token: None,
            fp_signal_cookie_names: vec!["id5id".to_owned()],
            fp_signal_json_path: Some("universal_uid".to_owned()),
            fp_signal_ttl_sec: 86400,
        });
        assert!(result.is_ok());
    }

    #[test]
    fn validate_fp_signal_config_ok_when_empty() {
        let result = validate_fp_signal_config(&PartnerRecord {
            id: "test".to_owned(),
            name: "Test".to_owned(),
            allowed_return_domains: vec![],
            api_key_hash: String::new(),
            bidstream_enabled: false,
            source_domain: "test.com".to_owned(),
            openrtb_atype: 3,
            sync_rate_limit: 100,
            batch_rate_limit: 60,
            pull_sync_enabled: false,
            pull_sync_url: None,
            pull_sync_allowed_domains: vec![],
            pull_sync_ttl_sec: 86400,
            pull_sync_rate_limit: 10,
            ts_pull_token: None,
            fp_signal_cookie_names: vec![],
            fp_signal_json_path: None,
            fp_signal_ttl_sec: 86400,
        });
        assert!(result.is_ok(), "should accept empty fp signal config");
    }

    #[test]
    fn validate_fp_signal_rejects_empty_cookie_name() {
        let result = validate_fp_signal_config(&PartnerRecord {
            id: "test".to_owned(),
            name: "Test".to_owned(),
            allowed_return_domains: vec![],
            api_key_hash: String::new(),
            bidstream_enabled: false,
            source_domain: "test.com".to_owned(),
            openrtb_atype: 3,
            sync_rate_limit: 100,
            batch_rate_limit: 60,
            pull_sync_enabled: false,
            pull_sync_url: None,
            pull_sync_allowed_domains: vec![],
            pull_sync_ttl_sec: 86400,
            pull_sync_rate_limit: 10,
            ts_pull_token: None,
            fp_signal_cookie_names: vec!["".to_owned()],
            fp_signal_json_path: None,
            fp_signal_ttl_sec: 86400,
        });
        let err = result.unwrap_err();
        assert!(err.contains("empty"), "got: {err}");
    }

    #[test]
    fn validate_fp_signal_rejects_too_many_cookie_names() {
        let result = validate_fp_signal_config(&PartnerRecord {
            id: "test".to_owned(),
            name: "Test".to_owned(),
            allowed_return_domains: vec![],
            api_key_hash: String::new(),
            bidstream_enabled: false,
            source_domain: "test.com".to_owned(),
            openrtb_atype: 3,
            sync_rate_limit: 100,
            batch_rate_limit: 60,
            pull_sync_enabled: false,
            pull_sync_url: None,
            pull_sync_allowed_domains: vec![],
            pull_sync_ttl_sec: 86400,
            pull_sync_rate_limit: 10,
            ts_pull_token: None,
            fp_signal_cookie_names: vec![
                "a".to_owned(), "b".to_owned(), "c".to_owned(),
                "d".to_owned(), "e".to_owned(), "f".to_owned(),
            ],
            fp_signal_json_path: None,
            fp_signal_ttl_sec: 86400,
        });
        let err = result.unwrap_err();
        assert!(err.contains("5"), "got: {err}");
    }

    #[test]
    fn validate_fp_signal_rejects_cookie_name_with_semicolon() {
        let result = validate_fp_signal_config(&PartnerRecord {
            id: "test".to_owned(),
            name: "Test".to_owned(),
            allowed_return_domains: vec![],
            api_key_hash: String::new(),
            bidstream_enabled: false,
            source_domain: "test.com".to_owned(),
            openrtb_atype: 3,
            sync_rate_limit: 100,
            batch_rate_limit: 60,
            pull_sync_enabled: false,
            pull_sync_url: None,
            pull_sync_allowed_domains: vec![],
            pull_sync_ttl_sec: 86400,
            pull_sync_rate_limit: 10,
            ts_pull_token: None,
            fp_signal_cookie_names: vec!["bad;name".to_owned()],
            fp_signal_json_path: None,
            fp_signal_ttl_sec: 86400,
        });
        assert!(result.is_err(), "should reject semicolon in cookie name");
    }

    #[test]
    fn validate_fp_signal_rejects_ttl_too_low() {
        let result = validate_fp_signal_config(&PartnerRecord {
            id: "test".to_owned(),
            name: "Test".to_owned(),
            allowed_return_domains: vec![],
            api_key_hash: String::new(),
            bidstream_enabled: false,
            source_domain: "test.com".to_owned(),
            openrtb_atype: 3,
            sync_rate_limit: 100,
            batch_rate_limit: 60,
            pull_sync_enabled: false,
            pull_sync_url: None,
            pull_sync_allowed_domains: vec![],
            pull_sync_ttl_sec: 86400,
            pull_sync_rate_limit: 10,
            ts_pull_token: None,
            fp_signal_cookie_names: vec!["cookie".to_owned()],
            fp_signal_json_path: None,
            fp_signal_ttl_sec: 10,
        });
        let err = result.unwrap_err();
        assert!(err.contains("60"), "got: {err}");
    }

    #[test]
    fn validate_fp_signal_rejects_invalid_json_path() {
        let result = validate_fp_signal_config(&PartnerRecord {
            id: "test".to_owned(),
            name: "Test".to_owned(),
            allowed_return_domains: vec![],
            api_key_hash: String::new(),
            bidstream_enabled: false,
            source_domain: "test.com".to_owned(),
            openrtb_atype: 3,
            sync_rate_limit: 100,
            batch_rate_limit: 60,
            pull_sync_enabled: false,
            pull_sync_url: None,
            pull_sync_allowed_domains: vec![],
            pull_sync_ttl_sec: 86400,
            pull_sync_rate_limit: 10,
            ts_pull_token: None,
            fp_signal_cookie_names: vec!["cookie".to_owned()],
            fp_signal_json_path: Some("a.b.c.d.e".to_owned()),
            fp_signal_ttl_sec: 86400,
        });
        let err = result.unwrap_err();
        assert!(err.contains("4"), "got: {err}");
    }
```

- [ ] **Step 2: Implement `validate_fp_signal_config`**

Add after `validate_pull_sync_config` in `partner.rs`:

```rust
/// Validates first-party signal collection configuration.
///
/// When `fp_signal_cookie_names` is empty, all FP signal fields are
/// ignored (no validation needed). When non-empty:
/// - Each cookie name must be non-empty, ASCII, no `;` or `=`.
/// - At most 5 cookie names per partner.
/// - `fp_signal_json_path` (if present) must be non-empty, only
///   alphanumeric + `.` + `_`, max 4 dot-separated segments.
/// - `fp_signal_ttl_sec` must be between 60 and 604800.
///
/// # Errors
///
/// Returns a descriptive error string on validation failure.
pub fn validate_fp_signal_config(record: &PartnerRecord) -> Result<(), String> {
    if record.fp_signal_cookie_names.is_empty() {
        return Ok(());
    }

    if record.fp_signal_cookie_names.len() > 5 {
        return Err(format!(
            "fp_signal_cookie_names must have at most 5 entries, got {}",
            record.fp_signal_cookie_names.len()
        ));
    }

    for name in &record.fp_signal_cookie_names {
        if name.is_empty() {
            return Err("fp_signal_cookie_names entries must not be empty".to_owned());
        }
        if !name.is_ascii() {
            return Err(format!(
                "fp_signal_cookie_names entry '{name}' contains non-ASCII characters"
            ));
        }
        if name.contains(';') || name.contains('=') {
            return Err(format!(
                "fp_signal_cookie_names entry '{name}' contains invalid characters (';' or '=')"
            ));
        }
    }

    if let Some(ref path) = record.fp_signal_json_path {
        if path.is_empty() {
            return Err("fp_signal_json_path must not be empty when present".to_owned());
        }
        if !path
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_')
        {
            return Err(format!(
                "fp_signal_json_path '{path}' contains invalid characters \
                 (only alphanumeric, '.', '_' allowed)"
            ));
        }
        let depth = path.split('.').count();
        if depth > 4 {
            return Err(format!(
                "fp_signal_json_path '{path}' exceeds max depth of 4 segments (got {depth})"
            ));
        }
    }

    if record.fp_signal_ttl_sec < 60 {
        return Err(format!(
            "fp_signal_ttl_sec must be at least 60, got {}",
            record.fp_signal_ttl_sec
        ));
    }
    if record.fp_signal_ttl_sec > 604_800 {
        return Err(format!(
            "fp_signal_ttl_sec must be at most 604800 (7 days), got {}",
            record.fp_signal_ttl_sec
        ));
    }

    Ok(())
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test --package trusted-server-core --lib ec::partner::tests`
Expected: all tests PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/trusted-server-core/src/ec/partner.rs
git commit -m "Add first-party signal validation for PartnerRecord"
```

---

### Task 8: Add `_fp_signal_enabled` index to `PartnerStore`

**Files:**

- Modify: `crates/trusted-server-core/src/ec/partner.rs`

- [ ] **Step 1: Add the index constant and skip it in `list_registered`**

In `partner.rs`, add after `PULL_ENABLED_INDEX_KEY` (line 47):

```rust
/// Key for the first-party signal partner config secondary index.
///
/// Stores a JSON array of [`FpSignalPartnerConfig`](super::fp_signals::FpSignalPartnerConfig)
/// for partners with non-empty `fp_signal_cookie_names`.
const FP_SIGNAL_INDEX_KEY: &str = "_fp_signal_enabled";
```

In `list_registered()` at line 314, update the skip condition:

```rust
                if key.starts_with(APIKEY_INDEX_PREFIX)
                    || key == PULL_ENABLED_INDEX_KEY
                    || key == FP_SIGNAL_INDEX_KEY
                {
                    continue;
                }
```

- [ ] **Step 2: Add `update_fp_signal_index` method**

Add after `update_pull_enabled_index` in `impl PartnerStore`:

```rust
    /// Best-effort update of the `_fp_signal_enabled` secondary index.
    ///
    /// Stores denormalized [`FpSignalPartnerConfig`](super::fp_signals::FpSignalPartnerConfig)
    /// entries for O(1) reads during extraction. Same self-healing pattern as
    /// [`update_pull_enabled_index`](Self::update_pull_enabled_index).
    fn update_fp_signal_index(&self, store: &KVStore, record: &PartnerRecord) {
        use super::fp_signals::FpSignalPartnerConfig;

        let mut configs: Vec<FpSignalPartnerConfig> = match store.lookup(FP_SIGNAL_INDEX_KEY) {
            Ok(mut resp) => {
                let bytes = resp.take_body_bytes();
                serde_json::from_slice(&bytes).unwrap_or_else(|err| {
                    log::warn!(
                        "Failed to deserialize fp-signal index, starting fresh: {err}"
                    );
                    Vec::new()
                })
            }
            Err(_) => Vec::new(),
        };

        // Remove existing entry for this partner.
        configs.retain(|c| c.partner_id != record.id);

        // Add new entry if partner has FP signal config.
        if !record.fp_signal_cookie_names.is_empty() {
            configs.push(FpSignalPartnerConfig {
                partner_id: record.id.clone(),
                cookie_names: record.fp_signal_cookie_names.clone(),
                json_path: record.fp_signal_json_path.clone(),
                ttl_sec: record.fp_signal_ttl_sec,
            });
        }

        let body = match serde_json::to_string(&configs) {
            Ok(b) => b,
            Err(err) => {
                log::warn!(
                    "Failed to serialize fp-signal index after updating partner '{}': {err}",
                    record.id,
                );
                return;
            }
        };

        if let Err(err) = store.build_insert().execute(FP_SIGNAL_INDEX_KEY, body) {
            log::warn!(
                "Failed to write fp-signal index after updating partner '{}': {err:?}",
                record.id,
            );
        }
    }
```

- [ ] **Step 3: Call `update_fp_signal_index` from `upsert`**

In `upsert()`, after the `update_pull_enabled_index` call (line 515), add:

```rust
        // 5. Update _fp_signal_enabled secondary index (best-effort).
        self.update_fp_signal_index(&store, record);
```

- [ ] **Step 4: Add `fp_signal_configs` accessor**

Add to `impl PartnerStore`:

```rust
    /// Returns first-party signal partner configs from the `_fp_signal_enabled` index.
    ///
    /// Returns an empty vec if the index is missing or unreadable (degraded-behavior
    /// policy — collection is best-effort).
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::KvStore`] on store open failure.
    pub fn fp_signal_configs(
        &self,
    ) -> Result<Vec<super::fp_signals::FpSignalPartnerConfig>, Report<TrustedServerError>> {
        use super::fp_signals::FpSignalPartnerConfig;

        let store = self.open_store()?;
        match store.lookup(FP_SIGNAL_INDEX_KEY) {
            Ok(mut resp) => {
                let bytes = resp.take_body_bytes();
                let configs: Vec<FpSignalPartnerConfig> =
                    serde_json::from_slice(&bytes).unwrap_or_else(|err| {
                        log::warn!(
                            "Failed to deserialize fp-signal index, returning empty: {err}"
                        );
                        Vec::new()
                    });
                Ok(configs)
            }
            Err(fastly::kv_store::KVStoreError::ItemNotFound) => Ok(Vec::new()),
            Err(err) => {
                log::warn!("Failed to read fp-signal index, returning empty: {err:?}");
                Ok(Vec::new())
            }
        }
    }
```

- [ ] **Step 5: Verify compilation**

Run: `cargo check --workspace`
Expected: compiles cleanly.

- [ ] **Step 6: Commit**

```bash
git add crates/trusted-server-core/src/ec/partner.rs
git commit -m "Add _fp_signal_enabled index to PartnerStore"
```

---

### Task 9: Add FP signal fields to admin registration endpoint

**Files:**

- Modify: `crates/trusted-server-core/src/ec/admin.rs`

- [ ] **Step 1: Add fields to `RegisterPartnerRequest`**

In `admin.rs`, add after the `ts_pull_token` field in `RegisterPartnerRequest` (line 50):

```rust
    #[serde(default)]
    pub fp_signal_cookie_names: Vec<String>,
    #[serde(default)]
    pub fp_signal_json_path: Option<String>,
    #[serde(default = "default_fp_signal_ttl_sec")]
    pub fp_signal_ttl_sec: u64,
```

Add the default function near the other defaults:

```rust
fn default_fp_signal_ttl_sec() -> u64 {
    86400
}
```

- [ ] **Step 2: Wire fields into `PartnerRecord` construction**

In `handle_register_partner`, destructure the new fields from the request (after `ts_pull_token` at line 201):

```rust
        fp_signal_cookie_names,
        fp_signal_json_path,
        fp_signal_ttl_sec,
```

And include them in the `PartnerRecord { ... }` construction (after `ts_pull_token` at line 240):

```rust
        fp_signal_cookie_names,
        fp_signal_json_path,
        fp_signal_ttl_sec,
```

- [ ] **Step 3: Add validation call**

After `validate_pull_sync_config(&record).map_err(bad_request)?;` (line 244), add:

```rust
    validate_fp_signal_config(&record).map_err(bad_request)?;
```

And update the import at the top to include:

```rust
use super::partner::{
    hash_api_key, validate_fp_signal_config, validate_partner_id, validate_pull_sync_config,
    PartnerRecord, PartnerStore,
};
```

- [ ] **Step 4: Update admin tests**

In the `request_deserializes_with_defaults` test, add assertions:

```rust
        assert!(req.fp_signal_cookie_names.is_empty(), "should default to empty");
        assert!(req.fp_signal_json_path.is_none(), "should default to None");
        assert_eq!(req.fp_signal_ttl_sec, 86400, "should default to 86400");
```

In the `request_deserializes_full_payload` test, add the new fields to the JSON and assertions:

```json
            "fp_signal_cookie_names": ["id5id"],
            "fp_signal_json_path": "universal_uid",
            "fp_signal_ttl_sec": 3600
```

```rust
        assert_eq!(req.fp_signal_cookie_names, vec!["id5id"]);
        assert_eq!(req.fp_signal_json_path.as_deref(), Some("universal_uid"));
        assert_eq!(req.fp_signal_ttl_sec, 3600);
```

- [ ] **Step 5: Run tests**

Run: `cargo test --workspace`
Expected: all tests PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/trusted-server-core/src/ec/admin.rs
git commit -m "Add first-party signal fields to partner registration endpoint"
```

---

### Task 10: Implement `write_fp_signals` with tests

**Files:**

- Modify: `crates/trusted-server-core/src/ec/fp_signals.rs`

- [ ] **Step 1: Write batched write tests**

These tests exercise the logic without real KV — we'll test the TTL filtering and signal merging logic via a helper that simulates the write decision. Add to `mod tests` in `fp_signals.rs`:

```rust
    use std::collections::HashMap;
    use super::super::kv_types::KvPartnerId;

    #[test]
    fn filter_signals_skips_fresh_ids() {
        let mut existing_ids = HashMap::new();
        existing_ids.insert(
            "lockr".to_owned(),
            KvPartnerId {
                uid: "old-uid".to_owned(),
                synced: 1_000_000,
            },
        );

        let signals = vec![FpSignal {
            partner_id: "lockr".to_owned(),
            uid: "new-uid".to_owned(),
        }];

        let configs = vec![FpSignalPartnerConfig {
            partner_id: "lockr".to_owned(),
            cookie_names: vec!["lockr_tracking_id".to_owned()],
            json_path: None,
            ttl_sec: 86400,
        }];

        let now = 1_000_100; // Only 100s since last sync (< 86400 TTL)
        let to_write = filter_stale_signals(&signals, &existing_ids, &configs, now);

        assert!(to_write.is_empty(), "should skip fresh signal");
    }

    #[test]
    fn filter_signals_includes_stale_ids() {
        let mut existing_ids = HashMap::new();
        existing_ids.insert(
            "lockr".to_owned(),
            KvPartnerId {
                uid: "old-uid".to_owned(),
                synced: 1_000_000,
            },
        );

        let signals = vec![FpSignal {
            partner_id: "lockr".to_owned(),
            uid: "new-uid".to_owned(),
        }];

        let configs = vec![FpSignalPartnerConfig {
            partner_id: "lockr".to_owned(),
            cookie_names: vec!["lockr_tracking_id".to_owned()],
            json_path: None,
            ttl_sec: 86400,
        }];

        let now = 1_100_000; // 100000s since last sync (> 86400 TTL)
        let to_write = filter_stale_signals(&signals, &existing_ids, &configs, now);

        assert_eq!(to_write.len(), 1, "should include stale signal");
        assert_eq!(to_write[0].partner_id, "lockr");
    }

    #[test]
    fn filter_signals_includes_new_ids() {
        let existing_ids = HashMap::new(); // No existing IDs

        let signals = vec![FpSignal {
            partner_id: "lockr".to_owned(),
            uid: "new-uid".to_owned(),
        }];

        let configs = vec![FpSignalPartnerConfig {
            partner_id: "lockr".to_owned(),
            cookie_names: vec!["lockr_tracking_id".to_owned()],
            json_path: None,
            ttl_sec: 86400,
        }];

        let to_write = filter_stale_signals(&signals, &existing_ids, &configs, 1_000_000);

        assert_eq!(to_write.len(), 1, "should include new signal");
    }

    #[test]
    fn filter_signals_mixed_fresh_and_stale() {
        let mut existing_ids = HashMap::new();
        existing_ids.insert(
            "lockr".to_owned(),
            KvPartnerId {
                uid: "lockr-uid".to_owned(),
                synced: 1_000_000,
            },
        );
        existing_ids.insert(
            "lotame".to_owned(),
            KvPartnerId {
                uid: "lotame-uid".to_owned(),
                synced: 900_000,
            },
        );

        let signals = vec![
            FpSignal {
                partner_id: "lockr".to_owned(),
                uid: "new-lockr".to_owned(),
            },
            FpSignal {
                partner_id: "lotame".to_owned(),
                uid: "new-lotame".to_owned(),
            },
        ];

        let configs = vec![
            FpSignalPartnerConfig {
                partner_id: "lockr".to_owned(),
                cookie_names: vec!["lockr_tracking_id".to_owned()],
                json_path: None,
                ttl_sec: 86400,
            },
            FpSignalPartnerConfig {
                partner_id: "lotame".to_owned(),
                cookie_names: vec!["panoramaId".to_owned()],
                json_path: None,
                ttl_sec: 86400,
            },
        ];

        let now = 1_050_000; // lockr: 50000s (fresh), lotame: 150000s (stale)
        let to_write = filter_stale_signals(&signals, &existing_ids, &configs, now);

        assert_eq!(to_write.len(), 1, "should only include stale signal");
        assert_eq!(to_write[0].partner_id, "lotame");
    }
```

- [ ] **Step 2: Implement `filter_stale_signals` helper**

Add above the tests:

```rust
use std::collections::HashMap;

use error_stack::Report;

use super::kv::KvIdentityGraph;
use super::kv_types::KvPartnerId;
use super::log_id;

/// Filters signals down to those whose existing KV entry is missing or stale.
///
/// Used by [`write_fp_signals`] to determine which signals need writing.
fn filter_stale_signals<'a>(
    signals: &'a [FpSignal],
    existing_ids: &HashMap<String, KvPartnerId>,
    configs: &[FpSignalPartnerConfig],
    now: u64,
) -> Vec<&'a FpSignal> {
    let ttl_map: HashMap<&str, u64> = configs
        .iter()
        .map(|c| (c.partner_id.as_str(), c.ttl_sec))
        .collect();

    signals
        .iter()
        .filter(|signal| {
            let ttl = ttl_map
                .get(signal.partner_id.as_str())
                .copied()
                .unwrap_or(86400);

            match existing_ids.get(&signal.partner_id) {
                Some(existing) => now.saturating_sub(existing.synced) >= ttl,
                None => true,
            }
        })
        .collect()
}
```

- [ ] **Step 3: Run filter tests**

Run: `cargo test --package trusted-server-core --lib ec::fp_signals::tests`
Expected: all tests PASS.

- [ ] **Step 4: Implement `write_fp_signals`**

Add the main write function:

```rust
/// Writes extracted first-party signals to the KV identity graph.
///
/// Performs a single batched read-modify-write cycle:
/// 1. Read the KV entry for `ec_id`.
/// 2. Skip if missing or tombstoned.
/// 3. Filter signals to those that are missing or stale (TTL check).
/// 4. Insert all qualifying signals and CAS write.
/// 5. Retry on conflict up to 3 times.
///
/// # Errors
///
/// Returns [`FpSignalError::KvWrite`] on KV store failure or CAS exhaustion.
pub fn write_fp_signals(
    kv: &KvIdentityGraph,
    ec_id: &str,
    signals: &[FpSignal],
    configs: &[FpSignalPartnerConfig],
) -> Result<(), Report<FpSignalError>> {
    use error_stack::ResultExt;

    const MAX_RETRIES: u32 = 3;
    let now = super::current_timestamp();

    for attempt in 0..MAX_RETRIES {
        let (mut entry, generation) = match kv.get(ec_id) {
            Ok(Some(pair)) => pair,
            Ok(None) => {
                log::debug!(
                    "FP signal write: no entry for '{}', skipping",
                    log_id(ec_id),
                );
                return Ok(());
            }
            Err(err) => {
                return Err(err
                    .change_context(FpSignalError::KvWrite {
                        message: format!("Failed to read entry for '{}'", log_id(ec_id)),
                    }));
            }
        };

        if !entry.consent.ok {
            log::debug!(
                "FP signal write: entry for '{}' is tombstoned, skipping",
                log_id(ec_id),
            );
            return Ok(());
        }

        let to_write = filter_stale_signals(signals, &entry.ids, configs, now);
        if to_write.is_empty() {
            log::debug!(
                "FP signal write: all signals fresh for '{}', no write needed",
                log_id(ec_id),
            );
            return Ok(());
        }

        for signal in &to_write {
            entry.ids.insert(
                signal.partner_id.clone(),
                KvPartnerId {
                    uid: signal.uid.clone(),
                    synced: now,
                },
            );
        }

        match kv.cas_write(ec_id, &entry, generation) {
            Ok(()) => {
                log::debug!(
                    "FP signal write: wrote {} signals for '{}'",
                    to_write.len(),
                    log_id(ec_id),
                );
                return Ok(());
            }
            Err(err) if is_cas_conflict(&err) => {
                log::debug!(
                    "FP signal write: CAS conflict on attempt {}/{MAX_RETRIES} for '{}'",
                    attempt + 1,
                    log_id(ec_id),
                );
                // Loop will re-read on next iteration.
            }
            Err(err) => {
                return Err(err
                    .change_context(FpSignalError::KvWrite {
                        message: format!(
                            "Failed to write signals for '{}' on attempt {}",
                            log_id(ec_id),
                            attempt + 1,
                        ),
                    }));
            }
        }
    }

    Err(Report::new(FpSignalError::KvWrite {
        message: format!(
            "CAS conflict after {MAX_RETRIES} retries writing signals for '{}'",
            log_id(ec_id),
        ),
    }))
}
```

**Note:** This function depends on `KvIdentityGraph::cas_write()` and an `is_cas_conflict()` helper that may not exist yet. The next step addresses this.

- [ ] **Step 5: Add `cas_write` method to `KvIdentityGraph`**

In `crates/trusted-server-core/src/ec/kv.rs`, add a public method for CAS writes that `write_fp_signals` can call:

```rust
    /// CAS-writes a full entry using the given generation marker.
    ///
    /// Returns `Ok(())` on success. On precondition failure, returns
    /// the `ItemPreconditionFailed` error for the caller to retry.
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::KvStore`] on serialization or store failure.
    pub fn cas_write(
        &self,
        ec_id: &str,
        entry: &KvEntry,
        generation: u64,
    ) -> Result<(), Report<TrustedServerError>> {
        let store = self.open_store()?;
        let (body, meta_str) = Self::serialize_entry(entry, &self.store_name)?;

        store
            .build_insert()
            .if_generation_match(generation)
            .metadata(&meta_str)
            .time_to_live(ENTRY_TTL)
            .execute(ec_id, body.as_str())
            .change_context(TrustedServerError::KvStore {
                store_name: self.store_name.clone(),
                message: format!("CAS write failed for key '{ec_id}'"),
            })
    }
```

Also add the `is_cas_conflict` helper function in `fp_signals.rs`:

```rust
/// Checks if an error report contains a CAS conflict.
fn is_cas_conflict(err: &Report<crate::error::TrustedServerError>) -> bool {
    err.to_string().contains("ItemPreconditionFailed")
        || err.to_string().contains("CAS write failed")
}
```

- [ ] **Step 6: Verify compilation**

Run: `cargo check --workspace`
Expected: compiles cleanly.

- [ ] **Step 7: Commit**

```bash
git add crates/trusted-server-core/src/ec/fp_signals.rs crates/trusted-server-core/src/ec/kv.rs
git commit -m "Implement batched CAS write for first-party signals"
```

---

### Task 11: Wire extraction and writing into the adapter

**Files:**

- Modify: `crates/trusted-server-adapter-fastly/src/main.rs`

- [ ] **Step 1: Update imports**

Add to the import block at the top of `main.rs`:

```rust
use trusted_server_core::ec::fp_signals::{
    extract_fp_signals, write_fp_signals, FpSignal, FpSignalPartnerConfig,
};
```

- [ ] **Step 2: Expand `RouteOutcome`**

Add two fields to `RouteOutcome`:

```rust
#[must_use]
struct RouteOutcome {
    response: Response,
    pull_sync_context: Option<PullSyncContext>,
    fp_signals: Vec<FpSignal>,
    fp_signal_configs: Vec<FpSignalPartnerConfig>,
}
```

- [ ] **Step 3: Add pre-send extraction in `route_request`**

After the `ec_finalize_response` call (around line 351) and before building the `pull_sync_context`, add:

```rust
    // Extract first-party signals from cookies for post-send KV write.
    let (fp_signals, fp_signal_configs) =
        if is_real_browser && organic_route && route_succeeded && ec_context.ec_allowed() {
            if let Some(ec_id) = ec_context.ec_value() {
                let partner_store = require_partner_store(settings).ok();
                let configs = partner_store
                    .as_ref()
                    .and_then(|s| s.fp_signal_configs().ok())
                    .unwrap_or_default();
                if configs.is_empty() {
                    (vec![], vec![])
                } else {
                    let now_ms = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_millis() as u64)
                        .unwrap_or(0);
                    let jar = ec_context.cookie_jar();
                    let signals = jar
                        .map(|j| extract_fp_signals(j, &configs, now_ms))
                        .unwrap_or_default();
                    (signals, configs)
                }
            } else {
                (vec![], vec![])
            }
        } else {
            (vec![], vec![])
        };
```

- [ ] **Step 4: Include in `RouteOutcome`**

Update the `RouteOutcome` construction:

```rust
    Ok(RouteOutcome {
        response,
        pull_sync_context,
        fp_signals,
        fp_signal_configs,
    })
```

Also update all early-return `RouteOutcome` constructions (batch sync, auth failure, etc.) to include:

```rust
        fp_signals: vec![],
        fp_signal_configs: vec![],
```

- [ ] **Step 5: Add post-send write in `main()`**

Update the `main()` function's post-send block:

```rust
    let RouteOutcome {
        response,
        pull_sync_context,
        fp_signals,
        fp_signal_configs,
    } = outcome;

    response.send_to_client();

    if !fp_signals.is_empty() {
        if let Some(ref ctx) = pull_sync_context {
            run_fp_signal_collection_after_send(settings, ctx.ec_id(), &fp_signals, &fp_signal_configs);
        }
    }

    if let Some(context) = pull_sync_context {
        run_pull_sync_after_send(&settings, &context);
    }
```

- [ ] **Step 6: Add `run_fp_signal_collection_after_send`**

Add after `run_pull_sync_after_send`:

```rust
fn run_fp_signal_collection_after_send(
    settings: &Settings,
    ec_id: &str,
    signals: &[FpSignal],
    configs: &[FpSignalPartnerConfig],
) {
    let kv = match require_identity_graph(settings) {
        Ok(kv) => kv,
        Err(err) => {
            log::debug!("FP signal collection: identity graph unavailable: {err:?}");
            return;
        }
    };
    if let Err(err) = write_fp_signals(&kv, ec_id, signals, configs) {
        log::warn!("FP signal collection failed: {err:?}");
    }
}
```

- [ ] **Step 7: Verify compilation**

Run: `cargo check --workspace`
Expected: compiles cleanly.

- [ ] **Step 8: Commit**

```bash
git add crates/trusted-server-adapter-fastly/src/main.rs
git commit -m "Wire first-party signal extraction and writing into adapter"
```

---

### Task 12: Run full verification

**Files:** none (verification only)

- [ ] **Step 1: Run full test suite**

Run: `cargo test --workspace`
Expected: all tests PASS.

- [ ] **Step 2: Run clippy**

Run: `cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: no warnings.

- [ ] **Step 3: Run format check**

Run: `cargo fmt --all -- --check`
Expected: no formatting issues.

- [ ] **Step 4: Verify WASM build**

Run: `cargo build --package trusted-server-adapter-fastly --release --target wasm32-wasip1`
Expected: builds successfully.

- [ ] **Step 5: Fix any issues found and re-verify**

If any step above fails, fix the issue and re-run all checks.

- [ ] **Step 6: Final commit if any fixes were needed**

Only if Step 5 produced changes.
