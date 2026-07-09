# Secrets to Secret Store (Issue #846) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop `ts config push` from persisting secret values in the config store: secret-bearing `Settings` fields hold secret-store **key names** at rest, resolved from the platform secret store at startup — Fastly first, other adapters later.

**Architecture:** Opt-in `[secrets]` section gates a "key names at rest" mode. A small resolver in core walks the verified app-config blob JSON (between envelope verification and `Settings` parse) and swaps each secret-ref leaf for the value fetched via `PlatformSecretStore`. Validation splits like EdgeZero spec 3.3.8: push-side validation skips value-shape checks on secret fields (they hold key names); runtime validates fully against resolved values and fails closed. Semantics intentionally mirror `edgezero_core`'s `secret_walk` so EdgeZero Phase 3 (#843) can later delete this code and replace it with the nested `#[secret]` derive.

**Tech Stack:** Rust 2024, `error-stack`, `validator`, `serde_json`, existing `PlatformSecretStore` trait, Viceroy for Fastly tests.

**Branch:** `846-secrets-to-secret-store`

**Commit policy:** every commit requires explicit user approval before running `git commit` (standing user rule). Commit steps below are checkpoints to _request_ approval, not to commit unprompted.

---

## Background (read first — zero-context summary)

- `Redacted<T>` is `#[serde(transparent)]` (`crates/trusted-server-core/src/redacted.rs:29`), so `ts config push` serializes real secret values into the `app_config` config-store blob. Config stores are not secret-grade storage.
- Secret fields (all consumers take sync `&Settings`, so resolution must happen at settings load):
  | TOML path | Struct field | Value-shape validation today |
  |---|---|---|
  | `publisher.proxy_secret` | `Publisher::proxy_secret` (`settings.rs:45`) | non-empty |
  | `ec.passphrase` | `Ec::passphrase` (`settings.rs:449`) | non-empty + **min 32 chars** (`Ec::validate_passphrase`) |
  | `ec.partners[].api_token` | `EcPartner::api_token` (`settings.rs:313`) | non-empty + min 32 bytes, at registry build (`ec/registry.rs:189-207`) |
  | `ec.partners[].ts_pull_token` | `EcPartner::ts_pull_token` (`settings.rs:347`, `Option`) | required when `pull_sync_enabled` |
  | `handlers[].password` | `Handler::password` (`settings.rs:584`) | non-empty + admin placeholder check (`settings.rs:2255-2274`) |
- `handlers[].username` is deliberately **not** migrated (sensitive but not secret-store material — issue #684).
- Load pipeline: `get_settings_from_config_store` (`settings_data.rs:50`) → chunk-pointer resolution → `settings_from_config_blob` (`config_payload.rs:23`) → `BlobEnvelope::verify` → `Settings::from_json_value` → `finalize_deserialized` (normalize + `validate()` + admin checks) → `reject_placeholder_secrets`.
- Push pipeline: `ts config push` → `edgezero_cli::run_config_push_typed::<TrustedServerAppConfig>` → our `Deserialize` impl (`config.rs:88-98`) → `finalize_deserialized` → `Validate` impl → `validate_settings_for_deploy` (`config.rs:126`).
- Fastly boot: `load_settings_from_config_store` (`adapter-fastly/src/app.rs:164`); on failure Fastly serves `startup_error_router` (fail-closed).
- `Settings` is `#[serde(deny_unknown_fields)]`: an old WASM binary hard-fails on a blob containing the new `[secrets]` section. **Rollout order: deploy new WASM first, then seed secrets, then push store-mode blob.**
- Migration must carry existing secret **values unchanged** into the secret store: regenerating `ec.passphrase` rotates all visitor IDs; regenerating `proxy_secret` invalidates outstanding proxy URL tokens.
- Existing test helpers: `MemoryConfigStore` (`settings_data.rs` tests), `HashMapSecretStore` (`crate::platform::test_support`, `platform/test_support.rs:96`).
- Local dev: `fastly.toml [local_server.secret_stores.ts_secrets]` already exists (Tinybird tokens); Viceroy serves it.

Out of scope (follow-ups, do NOT implement here): Cloudflare/Spin/Axum wiring beyond passing `None`, `ts secrets` CLI verb, `edgezero.toml` store-id alignment, request-signing keys (#686), EdgeZero Phase 3 derive swap (#843).

---

### Task 1: `[secrets]` settings section

**Files:**

- Modify: `crates/trusted-server-core/src/settings.rs` (new struct + `Settings` field)
- Test: same file, `#[cfg(test)] mod tests`

- [x] **Step 1: Write failing tests**

In the settings tests module:

```rust
#[test]
fn secrets_section_defaults_to_disabled_inline_mode() {
    let settings = Settings::from_toml(&crate_test_settings_str())
        .expect("should parse settings without [secrets] section");
    assert!(!settings.secrets.enabled, "should default to inline mode");
    assert_eq!(
        settings.secrets.store, "ts_secrets",
        "should default to ts_secrets store"
    );
}

#[test]
fn secrets_section_parses_enabled_and_store() {
    let mut toml = crate_test_settings_str();
    toml.push_str("\n[secrets]\nenabled = true\nstore = \"custom_secrets\"\n");
    // NOTE: with enabled = true the fixture's inline values are treated as
    // key names; parse must still succeed (KeyNames mode skips value-shape
    // validators — Task 3). If Task 3 is not yet done, use key-name-shaped
    // values long enough to pass validators for this parse test.
    let settings = Settings::from_toml(&toml).expect("should parse [secrets] section");
    assert!(settings.secrets.enabled, "should enable store mode");
    assert_eq!(settings.secrets.store, "custom_secrets", "should read store name");
}
```

- [x] **Step 2: Run tests, verify failure**

Run: `cargo test-fastly -p trusted-server-core secrets_section 2>&1 | tail -20`
Expected: FAIL — no field `secrets` on `Settings`.

- [x] **Step 3: Implement**

Add near the other section structs in `settings.rs`:

```rust
/// Secret-store reference mode for secret-bearing settings fields.
///
/// When [`SecretsSettings::enabled`] is `true`, secret fields in the app
/// config hold secret-store **key names** instead of secret values. The
/// runtime resolves them from the platform secret store at settings load;
/// see [`crate::secret_refs`].
#[derive(Debug, Clone, Deserialize, Serialize, Validate)]
#[serde(deny_unknown_fields)]
pub struct SecretsSettings {
    /// Enables key-name resolution from the platform secret store.
    #[serde(default)]
    pub enabled: bool,
    /// Secret store name holding the referenced secrets.
    #[serde(default = "default_secrets_store")]
    #[validate(length(min = 1))]
    pub store: String,
}

impl Default for SecretsSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            store: default_secrets_store(),
        }
    }
}

fn default_secrets_store() -> String {
    "ts_secrets".to_owned()
}
```

Add to `Settings` (root struct, `settings.rs:1930`):

```rust
    #[serde(default)]
    #[validate(nested)]
    pub secrets: SecretsSettings,
```

- [x] **Step 4: Run tests, verify pass**

Run: `cargo test-fastly -p trusted-server-core secrets_section 2>&1 | tail -5`
Expected: PASS (the `enabled = true` test may need Task 3 — if it fails only on the min-32 passphrase validator, mark it `#[ignore]` with a `// until Task 3` note and un-ignore in Task 3).

- [ ] **Step 5: Checkpoint — request commit approval**

`Add [secrets] settings section for secret-store reference mode`

---

### Task 2: Secret-ref resolver module

**Files:**

- Create: `crates/trusted-server-core/src/secret_refs.rs`
- Modify: `crates/trusted-server-core/src/lib.rs` (add `pub mod secret_refs;`)

- [x] **Step 1: Write failing tests** (in `secret_refs.rs` `#[cfg(test)]`)

Cover, using `crate::platform::test_support::HashMapSecretStore` and `serde_json::json!`:

1. resolves nested scalar (`publisher.proxy_secret`),
2. resolves array element (`ec.partners[].api_token` for two partners),
3. skips absent optional leaf (`ts_pull_token` missing) and absent sections (no `handlers`),
4. skips `null` leaf,
5. missing key in store → `Err` mentioning dotted path and key name, never any secret value,
6. non-string leaf → `Err`,
7. reads store name from `secrets.store`, defaults to `ts_secrets` when absent.

```rust
#[test]
fn resolves_nested_and_array_secret_refs() {
    let mut data = json!({
        "secrets": { "enabled": true, "store": "ts_secrets" },
        "publisher": { "proxy_secret": "proxy_secret" },
        "ec": {
            "passphrase": "ec_passphrase",
            "partners": [
                { "api_token": "partner_a_token" },
                { "api_token": "partner_b_token", "ts_pull_token": "partner_b_pull" }
            ]
        },
        "handlers": [ { "password": "admin_password" } ]
    });
    let store = HashMapSecretStore::new(HashMap::from([
        (("ts_secrets".to_owned(), "proxy_secret".to_owned()), b"resolved-proxy".to_vec()),
        (("ts_secrets".to_owned(), "ec_passphrase".to_owned()), b"resolved-passphrase-32-bytes-min!".to_vec()),
        (("ts_secrets".to_owned(), "partner_a_token".to_owned()), b"resolved-token-a-32-bytes-minimum".to_vec()),
        (("ts_secrets".to_owned(), "partner_b_token".to_owned()), b"resolved-token-b-32-bytes-minimum".to_vec()),
        (("ts_secrets".to_owned(), "partner_b_pull".to_owned()), b"resolved-pull-b".to_vec()),
        (("ts_secrets".to_owned(), "admin_password".to_owned()), b"resolved-admin-password".to_vec()),
    ]));

    resolve_secret_refs(&mut data, &store).expect("should resolve all refs");

    assert_eq!(data["publisher"]["proxy_secret"], "resolved-proxy", "should resolve nested scalar");
    assert_eq!(data["ec"]["partners"][1]["ts_pull_token"], "resolved-pull-b", "should resolve optional array leaf");
}
```

(Adapt the `HashMapSecretStore::new` argument to its real constructor shape in `platform/test_support.rs:96-110` — check before writing.)

- [x] **Step 2: Run, verify fail**

Run: `cargo test-fastly -p trusted-server-core secret_refs 2>&1 | tail -10`
Expected: FAIL — module does not exist.

- [x] **Step 3: Implement resolver**

```rust
//! Secret-store reference resolution for the app-config blob.
//!
//! When `[secrets] enabled = true`, secret-bearing fields in the pushed
//! app-config blob hold secret-store **key names**. This module swaps each
//! key name for the value fetched from the platform secret store, operating
//! on the verified blob JSON before [`crate::settings::Settings`] parsing.
//!
//! Semantics intentionally mirror `edgezero_core`'s `secret_walk` (key names
//! at rest, resolve at load, fail closed) extended with nested and array
//! paths, so the EdgeZero Phase 3 derive can replace this module.

use error_stack::Report;
use serde_json::Value;

use crate::error::TrustedServerError;
use crate::platform::{PlatformSecretStore, StoreName};

/// Dotted paths of secret-bearing fields. `[]` walks every array element.
/// `handlers[].username` is intentionally excluded (sensitive, not
/// secret-store material). Keep in sync with
/// `Settings::reject_placeholder_secrets` and `validate_secret_key_names`.
const SECRET_REF_PATHS: &[&str] = &[
    "publisher.proxy_secret",
    "ec.passphrase",
    "ec.partners[].api_token",
    "ec.partners[].ts_pull_token",
    "handlers[].password",
];

/// Replaces secret-ref key names in `data` with values from `secret_store`.
///
/// Reads the store name from `data.secrets.store` (default `ts_secrets`).
/// Absent sections, absent leaves, and `null` leaves are skipped so optional
/// fields keep their semantics.
///
/// # Errors
///
/// Returns [`TrustedServerError::Configuration`] when a present leaf is not a
/// string, or when the secret store cannot supply a referenced key. Error
/// messages carry the dotted path and key name, never a secret value.
pub fn resolve_secret_refs(
    data: &mut Value,
    secret_store: &dyn PlatformSecretStore,
) -> Result<(), Report<TrustedServerError>> {
    let store_name = StoreName::from(
        data.pointer("/secrets/store")
            .and_then(Value::as_str)
            .unwrap_or("ts_secrets"),
    );
    for path in SECRET_REF_PATHS {
        resolve_path(data, path, secret_store, &store_name)?;
    }
    Ok(())
}

fn resolve_path(
    node: &mut Value,
    path: &str,
    secret_store: &dyn PlatformSecretStore,
    store_name: &StoreName,
) -> Result<(), Report<TrustedServerError>> {
    match path.split_once('.') {
        None => resolve_leaf(node, path, secret_store, store_name),
        Some((head, rest)) => {
            if let Some(stripped) = head.strip_suffix("[]") {
                let Some(items) = node.get_mut(stripped).and_then(Value::as_array_mut) else {
                    return Ok(());
                };
                for item in items {
                    resolve_path(item, rest, secret_store, store_name)?;
                }
                Ok(())
            } else {
                match node.get_mut(head) {
                    Some(child) => resolve_path(child, rest, secret_store, store_name),
                    None => Ok(()),
                }
            }
        }
    }
}

fn resolve_leaf(
    node: &mut Value,
    field: &str,
    secret_store: &dyn PlatformSecretStore,
    store_name: &StoreName,
) -> Result<(), Report<TrustedServerError>> {
    let Some(leaf) = node.get(field) else {
        return Ok(());
    };
    if leaf.is_null() {
        return Ok(());
    }
    let Some(key_name) = leaf.as_str() else {
        return Err(Report::new(TrustedServerError::Configuration {
            message: format!("secret ref `{field}` must be a string secret key name"),
        }));
    };
    let resolved = secret_store
        .get_string(store_name, key_name)
        .map_err(|error| {
            Report::new(TrustedServerError::Configuration {
                message: format!(
                    "failed to resolve secret ref `{field}` (key `{key_name}`) from secret store `{store_name}`"
                ),
            })
            .attach(error.to_string())
        })?;
    node[field] = Value::String(resolved);
    Ok(())
}
```

Note: top-level `resolve_path` splits on the FULL dotted path — the `[]`
segment handling above covers `ec.partners[].api_token` (`head` iteration
happens when the `[]` segment is the current head). Verify with test 2; if
`split_once` ordering fights you, restructure to segment-vector iteration —
tests are the contract, not this sketch.

- [x] **Step 4: Run, verify pass**

Run: `cargo test-fastly -p trusted-server-core secret_refs 2>&1 | tail -5`
Expected: PASS (all resolver tests).

- [ ] **Step 5: Checkpoint — request commit approval**

`Add secret-ref resolver for app-config blob secrets`

---

### Task 3: Validation-mode split (push = key names, runtime = resolved values)

**Files:**

- Modify: `crates/trusted-server-core/src/settings.rs` (`finalize_deserialized`, `from_toml`, `from_json_value`, new mode enum + error-strip helper)
- Modify: `crates/trusted-server-core/src/config.rs` (`Deserialize` impl passes mode)
- Test: `settings.rs` tests

- [x] **Step 1: Write failing tests**

```rust
#[test]
fn key_names_mode_accepts_short_passphrase_ref() {
    let mut toml = crate_test_settings_str();
    // Replace the fixture passphrase with a short key name; enable store mode.
    // (Adjust the replace target to the fixture's actual passphrase value.)
    let toml = toml.replace(
        "test-secret-key-32-bytes-minimum!!",
        "ec_passphrase",
    );
    let toml = format!("{toml}\n[secrets]\nenabled = true\n");
    let settings = Settings::from_toml(&toml)
        .expect("should accept key-name passphrase in store mode");
    assert_eq!(settings.ec.passphrase.expose(), "ec_passphrase");
}

#[test]
fn resolved_values_mode_still_rejects_short_passphrase() {
    let toml = crate_test_settings_str()
        .replace("test-secret-key-32-bytes-minimum!!", "short");
    let err = Settings::from_toml(&toml).expect_err("should reject short passphrase inline");
    assert!(err.to_string().contains("validation failed"), "should fail validation: {err}");
}

#[test]
fn key_names_mode_skips_admin_placeholder_password_check() {
    // handlers[].password holds a key name like "admin_password" in store
    // mode; the admin placeholder check must not fire at parse time.
    // Build a store-mode TOML whose admin handler password is a key name and
    // assert from_toml succeeds.
}
```

(Look up the fixture passphrase in `test_support.rs::crate_test_settings_str` first and use its real value in `replace`.)

- [x] **Step 2: Run, verify fail**

Run: `cargo test-fastly -p trusted-server-core key_names_mode 2>&1 | tail -10`
Expected: `key_names_mode_accepts_short_passphrase_ref` FAILS (min-32 validator fires).

- [x] **Step 3: Implement**

Mode enum (in `settings.rs`, near `Settings`):

```rust
/// How secret-bearing fields should be validated during settings
/// finalization.
///
/// Mirrors the EdgeZero spec 3.3.8 split: push-side tooling sees secret-store
/// key names and must skip value-shape validators; the runtime sees resolved
/// secret values and must run everything.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum SecretFieldMode {
    /// Fields hold resolved secret values — run every validator.
    ResolvedValues,
    /// Fields hold secret-store key names — skip value-shape validators.
    KeyNames,
}
```

Change `finalize_deserialized` signature to `pub(crate) fn finalize_deserialized(settings: Self, validation_label: &str, mode: SecretFieldMode)`. Inside:

- after `settings.validate()` fails, when `mode == SecretFieldMode::KeyNames`, strip only the `ec.passphrase` errors (the sole value-shape serde validator that a key name can trip) via a helper, and succeed if nothing else remains:

```rust
/// Removes `ec.passphrase` value-shape errors from `errors`, returning
/// `None` when nothing else remains. Key-name mode defers passphrase shape
/// checks to runtime, where the resolved value is validated.
fn strip_key_name_validation_errors(mut errors: ValidationErrors) -> Option<ValidationErrors> {
    if let Some(ValidationErrorsKind::Struct(inner)) = errors.errors_mut().get_mut("ec") {
        inner.errors_mut().remove("passphrase");
        let inner_empty = inner.errors().is_empty();
        if inner_empty {
            errors.errors_mut().remove("ec");
        }
    }
    if errors.errors().is_empty() {
        None
    } else {
        Some(errors)
    }
}
```

(Check `validator` 0.x API names — `errors_mut()`/`errors()`/`ValidationErrorsKind` — against the version in `Cargo.lock`; adjust accessors accordingly.)

- gate `validate_admin_handler_passwords` on mode: skip when `KeyNames` (runtime re-runs it on resolved values).
- `from_toml` and the `TrustedServerAppConfig` `Deserialize` impl derive the mode from the parsed value: `if settings.secrets.enabled { KeyNames } else { ResolvedValues }`.
- `from_json_value` always passes `ResolvedValues` — document the invariant: callers resolve secret refs first (`config_payload`).
- `from_toml_and_env` (test-only): `ResolvedValues`.

- [x] **Step 4: Run, verify pass**

Run: `cargo test-fastly -p trusted-server-core 2>&1 | tail -10`
Expected: PASS, including un-ignored Task 1 test and all pre-existing settings tests.

- [ ] **Step 5: Checkpoint — request commit approval**

`Split secret-field validation between key-name and resolved-value modes`

---

### Task 4: Deploy-validation gating + key-name shape checks

**Files:**

- Modify: `crates/trusted-server-core/src/config.rs` (`validate_settings_for_deploy`)
- Modify: `crates/trusted-server-core/src/settings.rs` (new `validate_secret_key_names`)
- Modify: `crates/trusted-server-core/src/ec/registry.rs` (token-mode-aware validation)
- Test: `config.rs` tests

- [x] **Step 1: Write failing tests** (in `config.rs` tests)

```rust
#[test]
fn deploy_validation_accepts_key_name_secrets_in_store_mode() {
    // valid_settings() fixture with secrets.enabled = true and every secret
    // field replaced by a key name (e.g. "ec_passphrase", "proxy_secret",
    // partner tokens, handler password). validate_settings_for_deploy must
    // pass: placeholder/value checks are deferred to runtime.
}

#[test]
fn deploy_validation_rejects_whitespace_key_names_in_store_mode() {
    // secrets.enabled = true, ec.passphrase = "has space" → error mentioning
    // `ec.passphrase` and key-name shape.
}

#[test]
fn deploy_validation_still_rejects_placeholders_inline() {
    // existing behavior: secrets.enabled = false + placeholder proxy_secret
    // → InsecureDefault (this test already exists as
    // deploy_validation_rejects_placeholders — keep it green).
}
```

- [x] **Step 2: Run, verify fail**

Run: `cargo test-fastly -p trusted-server-core deploy_validation 2>&1 | tail -10`

- [x] **Step 3: Implement**

In `validate_settings_for_deploy` (`config.rs:126`), branch on `settings.secrets.enabled`:

- store mode: replace `reject_placeholder_secrets()` with `settings.validate_secret_key_names()`; build the partner registry with token value checks relaxed (see below); keep integration + auction-provider validation unchanged.
- inline mode: unchanged behavior.

`Settings::validate_secret_key_names` (settings.rs): iterate the same five field sets as `reject_placeholder_secrets` (`publisher.proxy_secret`, `ec.passphrase`, `ec.partners[].api_token`, `ec.partners[].ts_pull_token` when present, `handlers[].password`) and reject empty values or values containing whitespace/control characters, collecting offending dotted paths into one `TrustedServerError::Configuration` error. Never include the value in the message beyond the field path.

Registry: add `PartnerRegistry::from_config_with_secret_mode(partners, mode)` where `KeyNames` keeps the non-empty check in `validate_api_token` but skips `MIN_API_TOKEN_LENGTH`; `from_config` delegates with `ResolvedValues`. Runtime registry construction keeps calling `from_config` (resolved values → full checks).

- [x] **Step 4: Run, verify pass**

Run: `cargo test-fastly -p trusted-server-core 2>&1 | tail -5`

- [ ] **Step 5: Checkpoint — request commit approval**

`Gate deploy validation on secret-ref mode with key-name shape checks`

---

### Task 5: Runtime blob resolution in `config_payload`

**Files:**

- Modify: `crates/trusted-server-core/src/config_payload.rs`
- Test: same file

- [x] **Step 1: Write failing tests**

```rust
#[test]
fn store_mode_blob_resolves_secrets_before_parse() {
    // Build settings JSON with secrets.enabled = true and key-name leaves;
    // envelope it; call settings_from_config_blob_with_secrets with a
    // HashMapSecretStore holding 32+-byte values. Assert the returned
    // Settings expose the RESOLVED values and full validation ran.
}

#[test]
fn store_mode_blob_without_secret_store_fails_closed() {
    // secrets.enabled = true, secret_store = None → Configuration error
    // mentioning "[secrets]" and the adapter capability.
}

#[test]
fn store_mode_blob_with_short_resolved_passphrase_fails_validation() {
    // Store returns an 8-byte passphrase → error (min-32 runs on resolved).
}

#[test]
fn inline_blob_ignores_secret_store() {
    // secrets absent → resolves nothing even when a store is provided;
    // existing round-trip behavior intact.
}
```

- [x] **Step 2: Run, verify fail**

Run: `cargo test-fastly -p trusted-server-core config_payload 2>&1 | tail -10`

- [x] **Step 3: Implement**

```rust
/// Reconstruct validated [`Settings`] from a config blob envelope, resolving
/// secret-store references when the blob enables `[secrets]` mode.
///
/// # Errors
///
/// Returns [`TrustedServerError::Configuration`] when the envelope is
/// invalid, when `[secrets]` mode is enabled but `secret_store` is `None`
/// (adapter does not support secret resolution yet), when a referenced
/// secret cannot be resolved, or when the resolved settings fail validation.
pub fn settings_from_config_blob_with_secrets(
    envelope_json: &str,
    secret_store: Option<&dyn PlatformSecretStore>,
) -> Result<Settings, Report<TrustedServerError>> {
    let envelope: BlobEnvelope = /* existing parse */;
    envelope.verify()/* existing */;

    let mut data = envelope.into_data();
    let store_mode = data
        .pointer("/secrets/enabled")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    if store_mode {
        let secret_store = secret_store.ok_or_else(|| {
            Report::new(TrustedServerError::Configuration {
                message: "[secrets] mode is enabled but this adapter has no secret store wired; \
                          disable [secrets] or upgrade the adapter"
                    .to_string(),
            })
        })?;
        resolve_secret_refs(&mut data, secret_store)?;
    }

    let settings = Settings::from_json_value(data)?;
    settings.reject_placeholder_secrets()?;
    Ok(settings)
}

/// Back-compat wrapper: inline mode only.
pub fn settings_from_config_blob(
    envelope_json: &str,
) -> Result<Settings, Report<TrustedServerError>> {
    settings_from_config_blob_with_secrets(envelope_json, None)
}
```

- [x] **Step 4: Run, verify pass**

Run: `cargo test-fastly -p trusted-server-core config_payload 2>&1 | tail -5`

- [ ] **Step 5: Checkpoint — request commit approval**

`Resolve secret-store references when loading store-mode config blobs`

---

### Task 6: Loader plumbing (`settings_data`) + adapter call sites

**Files:**

- Modify: `crates/trusted-server-core/src/settings_data.rs` (`get_settings_from_config_store` signature)
- Modify: `crates/trusted-server-adapter-fastly/src/app.rs:164-168` (pass Fastly secret store)
- Modify: `crates/trusted-server-adapter-axum/src/app.rs:59` (pass `None`)
- Test: `settings_data.rs` tests

- [x] **Step 1: Write failing test**

```rust
#[test]
fn loads_store_mode_settings_resolving_secrets() {
    // MemoryConfigStore with a store-mode envelope + HashMapSecretStore;
    // call get_settings_from_config_store(&config_store, Some(&secret_store),
    // &store_name, key) and assert resolved values.
}
```

- [x] **Step 2: Run, verify fail**

Run: `cargo test-fastly -p trusted-server-core settings_data 2>&1 | tail -10`
Expected: FAIL — wrong arity.

- [x] **Step 3: Implement**

New signature:

```rust
pub fn get_settings_from_config_store(
    config_store: &dyn PlatformConfigStore,
    secret_store: Option<&dyn PlatformSecretStore>,
    store_name: &StoreName,
    key: &str,
) -> Result<Settings, Report<TrustedServerError>>
```

body delegates to `settings_from_config_blob_with_secrets(&envelope_json, secret_store)`. Update ALL call sites:

- `adapter-fastly/src/app.rs:167`: `get_settings_from_config_store(&FastlyPlatformConfigStore, Some(&FastlyPlatformSecretStore), &store_name, &config_key)`
- `adapter-axum/src/app.rs:59`: pass `None` (Axum wiring is a follow-up).
- every `settings_data.rs` test call: add `None` (or the new store for the new test).

- [x] **Step 4: Run, verify pass**

Run: `cargo test-fastly 2>&1 | tail -5` and `cargo test-axum 2>&1 | tail -5`
Expected: PASS both.

- [ ] **Step 5: Checkpoint — request commit approval**

`Wire secret store into config-store settings loading (Fastly first)`

---

### Task 7: Local dev (Viceroy) + operator template

**Files:**

- Modify: `fastly.toml` (`[local_server.secret_stores.ts_secrets]` entries)
- Modify: `trusted-server.example.toml` (commented store-mode example)

- [x] **Step 1: Add Viceroy secrets**

Append to the existing `ts_secrets` local store in `fastly.toml`:

```toml
        [[local_server.secret_stores.ts_secrets]]
            key = "ec_passphrase"
            data = "local-dev-ec-passphrase-32-bytes!!"

        [[local_server.secret_stores.ts_secrets]]
            key = "proxy_secret"
            data = "local-dev-proxy-secret"

        [[local_server.secret_stores.ts_secrets]]
            key = "admin_password"
            data = "local-dev-admin-password-32-bytes!"
```

- [x] **Step 2: Document store mode in the example TOML**

Under the existing secret fields add commented guidance:

```toml
# Store mode: set [secrets] enabled = true and replace each secret value
# below with the NAME of a secret in the platform secret store. Seed the
# store with your EXISTING values first (changing ec.passphrase rotates all
# visitor IDs; changing publisher.proxy_secret invalidates proxy URL tokens):
#   printf %s "$EC_PASSPHRASE" | fastly secret-store-entry create --store-id <ID> --name ec_passphrase --stdin
# Rollout order: deploy new WASM -> seed secrets -> push store-mode config.
#
# [secrets]
# enabled = true
# store = "ts_secrets"
```

- [x] **Step 3: Smoke-check Viceroy config parses**

Run: `fastly compute serve --verbose 2>&1 | head -30` (Ctrl-C after boot) or at minimum `cargo check-fastly`.
Expected: no manifest parse errors.

- [ ] **Step 4: Checkpoint — request commit approval**

`Add local secret-store entries and store-mode config template`

---

### Task 8: Full verification

- [x] **Step 1: Format** — `cargo fmt --all -- --check` → clean.
- [x] **Step 2: Clippy (CI toolchain)** — `cargo +1.95.0 clippy-fastly && cargo +1.95.0 clippy-axum && cargo +1.95.0 clippy-cloudflare && cargo +1.95.0 clippy-cloudflare-wasm && cargo +1.95.0 clippy-spin-native && cargo +1.95.0 clippy-spin-wasm` → no NEW warnings (pre-existing debt exists; compare against `main` if unsure).
- [x] **Step 3: Tests** — `cargo test-fastly && cargo test-axum && cargo test-cloudflare && cargo test-spin` → all pass.
- [x] **Step 4: Parity** — `cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity` → pass (inline-mode fixtures unaffected).
- [ ] **Step 5: Checkpoint — request commit approval for any fixups, then hand off for PR.**

---

## Follow-ups (tracked, not in this branch)

- Cloudflare / Spin / Axum secret-store wiring (pass their `PlatformSecretStore` impls instead of `None`; Spin blocked on spin-sdk/Spin 4.x toolchain incompat).
- `ts secrets set` CLI verb (Fastly management API create exists in `adapter-fastly/src/management_api.rs`; CLI can shell out to `fastly secret-store-entry` for v1).
- `edgezero.toml [stores.secrets]` id alignment (`trusted_server_secrets` vs actual `ts_secrets`).
- Push-time warning when plaintext secrets detected in inline mode (nudge migration).
- EdgeZero Phase 3 (#843): replace `secret_refs.rs` + mode gating with nested `#[secret]` derive once stackpop/edgezero#304 ships; delete this module.

## Status log (keep current)

- 2026-07-09: Plan created on branch `846-secrets-to-secret-store`.
- 2026-07-09: Task 1 done (3 tests green). Commit pending user approval.
- 2026-07-09: Task 2 done — `secret_refs.rs` resolver, 7 tests green. Commit pending approval.
- 2026-07-09: Task 3 done — `SecretFieldMode` split, 5 new tests; settings (130) + config (18) modules green. Commit pending approval.
- 2026-07-09: Task 4 done — deploy validation gated by mode, `validate_secret_key_names`, registry `from_config_with_secret_mode`; 8 deploy tests green. Commit pending approval.
- 2026-07-09: Task 5 done — `settings_from_config_blob_with_secrets`, fail-closed without store; 7 config_payload tests green. Commit pending approval.
- 2026-07-09: Task 6 done — loader takes Option<&dyn PlatformSecretStore>; Fastly passes FastlyPlatformSecretStore, Axum None; settings_data 5 tests + axum suite green, check-fastly clean. Commit pending approval.
- 2026-07-09: Task 7 done — fastly.toml local ts_secrets entries (ec_passphrase/proxy_secret/admin_password), example.toml store-mode guidance; Viceroy manifest parses. Commit pending approval.
- 2026-07-09: Task 8 done — fmt clean; all 6 clippy targets clean on 1.95.0; full Rust + parity + CLI suites green. Committed and pushed; draft PR #873 opened.
- 2026-07-09: Applied two rounds of external review. Round 1: `[secrets]` omitted from serialization when disabled (old-binary wire compat); resolver fails closed on non-array covered sections; registry key-name mode keeps the non-empty check; docs corrected (`fastly … --stdin`, no `--secret` flag). Round 2: added a hard push/deploy guard rejecting `TRUSTED_SERVER__` secret env overrides in store mode; serialization now omits `[secrets]` whenever disabled (not just the exact default); resolver `# Errors` and this plan's `--stdin` command corrected. Regression tests added for each.
