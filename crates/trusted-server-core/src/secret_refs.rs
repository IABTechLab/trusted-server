//! Secret-store reference resolution for the app-config blob.
//!
//! When `[secrets] enabled = true`, secret-bearing fields in the pushed
//! app-config blob hold secret-store **key names**. This module swaps each
//! key name for the value fetched from the platform secret store, operating
//! on the verified blob JSON before [`crate::settings::Settings`] parsing.
//!
//! Semantics intentionally mirror `edgezero_core`'s `secret_walk` (key names
//! at rest, resolve at load, fail closed) extended with nested and array
//! paths, so the `EdgeZero` Phase 3 nested `#[secret]` derive can replace
//! this module once it ships upstream.

use error_stack::Report;
use serde_json::Value;

use crate::error::TrustedServerError;
use crate::platform::{PlatformSecretStore, StoreName};

/// Default secret store name when the blob does not declare `secrets.store`.
const DEFAULT_SECRET_STORE: &str = "ts_secrets";

/// Dotted paths of secret-bearing fields. `[]` walks every array element.
///
/// `handlers[].username` is intentionally excluded (sensitive, but not
/// secret-store material). Keep in sync with
/// [`crate::settings::Settings::reject_placeholder_secrets`].
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
/// string, when a covered array section (e.g. `ec.partners`, `handlers`) is
/// present in a non-array encoding that cannot be resolved, or when the secret
/// store cannot supply a referenced key. Error messages carry the dotted path
/// and key name, never a secret value.
pub fn resolve_secret_refs(
    data: &mut Value,
    secret_store: &dyn PlatformSecretStore,
) -> Result<(), Report<TrustedServerError>> {
    let store_name = StoreName::from(
        data.pointer("/secrets/store")
            .and_then(Value::as_str)
            .unwrap_or(DEFAULT_SECRET_STORE),
    );
    for path in SECRET_REF_PATHS {
        resolve_path(data, path, path, secret_store, &store_name)?;
    }
    Ok(())
}

fn resolve_path(
    node: &mut Value,
    remaining: &str,
    full_path: &str,
    secret_store: &dyn PlatformSecretStore,
    store_name: &StoreName,
) -> Result<(), Report<TrustedServerError>> {
    match remaining.split_once('.') {
        None => resolve_leaf(node, remaining, full_path, secret_store, store_name),
        Some((head, rest)) => {
            if let Some(field) = head.strip_suffix("[]") {
                match node.get_mut(field) {
                    // Absent section (or explicit null) — nothing to resolve.
                    None | Some(Value::Null) => Ok(()),
                    Some(Value::Array(items)) => {
                        for item in items {
                            resolve_path(item, rest, full_path, secret_store, store_name)?;
                        }
                        Ok(())
                    }
                    // Present but not an array. `Settings` also accepts
                    // map/string encodings of these fields, which would
                    // deserialize the unresolved key names as real secret
                    // values — fail closed instead of silently skipping.
                    Some(_) => Err(Report::new(TrustedServerError::Configuration {
                        message: format!(
                            "secret ref `{full_path}` requires `{field}` to be an array; \
                             found a non-array encoding that cannot be resolved"
                        ),
                    })),
                }
            } else {
                match node.get_mut(head) {
                    Some(child) => resolve_path(child, rest, full_path, secret_store, store_name),
                    None => Ok(()),
                }
            }
        }
    }
}

fn resolve_leaf(
    node: &mut Value,
    field: &str,
    full_path: &str,
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
            message: format!("secret ref `{full_path}` must be a string secret key name"),
        }));
    };
    let key_name = key_name.to_owned();
    let resolved = secret_store
        .get_string(store_name, &key_name)
        .map_err(|error| {
            Report::new(TrustedServerError::Configuration {
                message: format!(
                    "failed to resolve secret ref `{full_path}` (key `{key_name}`) from secret store `{store_name}`"
                ),
            })
            .attach(error.to_string())
        })?;
    node[field] = Value::String(resolved);
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde_json::json;

    use super::*;
    use crate::platform::test_support::HashMapSecretStore;

    fn test_store() -> HashMapSecretStore {
        HashMapSecretStore::new(HashMap::from([
            ("proxy_secret".to_owned(), b"resolved-proxy-secret".to_vec()),
            (
                "ec_passphrase".to_owned(),
                b"resolved-passphrase-32-bytes-min!".to_vec(),
            ),
            (
                "partner_a_token".to_owned(),
                b"resolved-token-a-32-bytes-minimum".to_vec(),
            ),
            (
                "partner_b_token".to_owned(),
                b"resolved-token-b-32-bytes-minimum".to_vec(),
            ),
            (
                "partner_b_pull".to_owned(),
                b"resolved-pull-token-b".to_vec(),
            ),
            (
                "admin_password".to_owned(),
                b"resolved-admin-password".to_vec(),
            ),
        ]))
    }

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

        resolve_secret_refs(&mut data, &test_store()).expect("should resolve all refs");

        assert_eq!(
            data["publisher"]["proxy_secret"], "resolved-proxy-secret",
            "should resolve nested scalar"
        );
        assert_eq!(
            data["ec"]["passphrase"], "resolved-passphrase-32-bytes-min!",
            "should resolve ec passphrase"
        );
        assert_eq!(
            data["ec"]["partners"][0]["api_token"], "resolved-token-a-32-bytes-minimum",
            "should resolve first partner token"
        );
        assert_eq!(
            data["ec"]["partners"][1]["ts_pull_token"], "resolved-pull-token-b",
            "should resolve optional array leaf"
        );
        assert_eq!(
            data["handlers"][0]["password"], "resolved-admin-password",
            "should resolve handler password"
        );
    }

    #[test]
    fn skips_absent_sections_and_optional_leaves() {
        let mut data = json!({
            "publisher": { "proxy_secret": "proxy_secret" },
            "ec": {
                "passphrase": "ec_passphrase",
                "partners": [ { "api_token": "partner_a_token" } ]
            }
        });

        resolve_secret_refs(&mut data, &test_store())
            .expect("should skip absent handlers and ts_pull_token");

        assert_eq!(
            data["ec"]["partners"][0]["api_token"], "resolved-token-a-32-bytes-minimum",
            "should still resolve present leaves"
        );
        assert!(
            data["ec"]["partners"][0].get("ts_pull_token").is_none(),
            "should not insert absent optional leaves"
        );
    }

    #[test]
    fn present_non_array_section_fails_closed() {
        // `Settings` also accepts a numeric-map encoding of `handlers`; if a
        // blob carried that shape, the array walk must NOT silently skip it
        // (that would leave key names as real passwords). Fail closed.
        let mut data = json!({
            "handlers": { "0": { "password": "admin_password" } }
        });

        let err = resolve_secret_refs(&mut data, &test_store())
            .expect_err("non-array handlers must fail closed");

        assert!(
            err.to_string().contains("handlers"),
            "error should name the offending section: {err}"
        );
    }

    #[test]
    fn present_string_encoded_array_fails_closed() {
        let mut data = json!({
            "ec": { "partners": "[{\"api_token\":\"partner_a_token\"}]" }
        });

        let err = resolve_secret_refs(&mut data, &test_store())
            .expect_err("string-encoded partners array must fail closed");

        assert!(
            err.to_string().contains("partners"),
            "error should name the offending section: {err}"
        );
    }

    #[test]
    fn null_section_is_skipped() {
        let mut data = json!({
            "publisher": { "proxy_secret": "proxy_secret" },
            "ec": { "passphrase": "ec_passphrase", "partners": null }
        });

        resolve_secret_refs(&mut data, &test_store())
            .expect("null array section should be skipped, not rejected");

        assert_eq!(
            data["ec"]["passphrase"],
            "resolved-passphrase-32-bytes-min!"
        );
    }

    #[test]
    fn skips_null_leaf() {
        let mut data = json!({
            "publisher": { "proxy_secret": null }
        });

        resolve_secret_refs(&mut data, &test_store()).expect("should skip null leaf");

        assert!(
            data["publisher"]["proxy_secret"].is_null(),
            "should leave null leaf untouched"
        );
    }

    #[test]
    fn missing_secret_key_fails_with_path_and_key_name() {
        let mut data = json!({
            "publisher": { "proxy_secret": "unknown_key" }
        });

        let err = resolve_secret_refs(&mut data, &test_store())
            .expect_err("should fail on missing secret key");
        let message = err.to_string();

        assert!(
            message.contains("publisher.proxy_secret"),
            "error should mention dotted path: {message}"
        );
    }

    #[test]
    fn non_string_leaf_fails() {
        let mut data = json!({
            "publisher": { "proxy_secret": 42 }
        });

        let err = resolve_secret_refs(&mut data, &test_store())
            .expect_err("should fail on non-string leaf");

        assert!(
            err.to_string().contains("publisher.proxy_secret"),
            "error should mention the offending path"
        );
    }

    #[test]
    fn error_message_never_contains_resolved_values() {
        let mut data = json!({
            "publisher": { "proxy_secret": "proxy_secret" },
            "ec": { "passphrase": "unknown_key" }
        });

        let err = resolve_secret_refs(&mut data, &test_store())
            .expect_err("should fail on missing passphrase key");
        let debug = format!("{err:?}");

        assert!(
            !debug.contains("resolved-proxy-secret"),
            "error output should not leak resolved secrets"
        );
    }

    #[test]
    fn reads_store_name_with_default_fallback() {
        // HashMapSecretStore ignores the store name, so assert the default
        // path works end to end when `secrets` is absent entirely.
        let mut data = json!({
            "ec": { "passphrase": "ec_passphrase" }
        });

        resolve_secret_refs(&mut data, &test_store())
            .expect("should resolve with default store name");

        assert_eq!(
            data["ec"]["passphrase"], "resolved-passphrase-32-bytes-min!",
            "should resolve using default ts_secrets store name"
        );
    }
}
