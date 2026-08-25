//! Runtime resolution of `EdgeZero` app-config secret references.
//!
//! Config blobs carry secret-store key names at rest. This module walks the
//! public `EdgeZero` metadata contract and replaces those names only in the
//! in-memory value used to build runtime [`crate::settings::Settings`].

use edgezero_core::app_config::{AppConfigMeta, SecretField, SecretKind, SecretPathSegment};
use error_stack::Report;
use serde_json::Value;

use crate::error::TrustedServerError;
use crate::platform::{PlatformSecretStore, StoreName};

/// Resolve all secret references in a serialized Trusted Server app config.
///
/// The input is mutated in memory; the verified envelope is never rewritten.
/// Secret values are not included in structural or platform errors.
///
/// # Errors
///
/// Returns [`TrustedServerError::Configuration`] when a required path or key
/// is malformed, a secret is unavailable, is not valid UTF-8, or resolves to an
/// empty value.
pub fn resolve_secret_references<C: AppConfigMeta>(
    data: &mut Value,
    secret_store: &dyn PlatformSecretStore,
    default_store_name: &StoreName,
) -> Result<(), Report<TrustedServerError>> {
    let mut resolved_data = data.clone();
    for field in C::secret_fields() {
        if matches!(field.kind, SecretKind::StoreRef) {
            continue;
        }
        resolve_field(
            &mut resolved_data,
            &field,
            &field.path,
            "",
            secret_store,
            default_store_name,
        )?;
    }
    *data = resolved_data;
    Ok(())
}

fn resolve_field(
    node: &mut Value,
    field: &SecretField,
    remaining: &[SecretPathSegment],
    rendered_path: &str,
    secret_store: &dyn PlatformSecretStore,
    default_store_name: &StoreName,
) -> Result<(), Report<TrustedServerError>> {
    match remaining.split_first() {
        Some((SecretPathSegment::Field(name), [])) => resolve_leaf(
            node,
            field,
            name.as_ref(),
            rendered_path,
            secret_store,
            default_store_name,
        ),
        Some((SecretPathSegment::OptionalField(name), [])) => {
            if matches!(node.get(name.as_ref()), None | Some(Value::Null)) {
                return Ok(());
            }
            resolve_leaf(
                node,
                field,
                name.as_ref(),
                rendered_path,
                secret_store,
                default_store_name,
            )
        }
        Some((SecretPathSegment::Field(name), rest)) => {
            let next_path = join_field(rendered_path, name.as_ref());
            let child = node
                .as_object_mut()
                .and_then(|object| object.get_mut(name.as_ref()))
                .ok_or_else(|| missing_path(&next_path))?;
            if child.is_null() {
                return Err(missing_path(&next_path));
            }
            resolve_field(
                child,
                field,
                rest,
                &next_path,
                secret_store,
                default_store_name,
            )
        }
        Some((SecretPathSegment::OptionalField(name), rest)) => {
            let next_path = join_field(rendered_path, name.as_ref());
            let Some(child) = node
                .as_object_mut()
                .and_then(|object| object.get_mut(name.as_ref()))
            else {
                return Ok(());
            };
            if child.is_null() {
                return Ok(());
            }
            resolve_field(
                child,
                field,
                rest,
                &next_path,
                secret_store,
                default_store_name,
            )
        }
        Some((SecretPathSegment::ArrayEach, rest)) => {
            let items = node.as_array_mut().ok_or_else(|| {
                configuration_error(format!("expected an array at `{rendered_path}`"))
            })?;
            for (index, item) in items.iter_mut().enumerate() {
                let indexed_path = format!("{rendered_path}[{index}]");
                resolve_field(
                    item,
                    field,
                    rest,
                    &indexed_path,
                    secret_store,
                    default_store_name,
                )?;
            }
            Ok(())
        }
        None => Ok(()),
    }
}

fn resolve_leaf(
    parent: &mut Value,
    field: &SecretField,
    key: &str,
    rendered_parent: &str,
    secret_store: &dyn PlatformSecretStore,
    default_store_name: &StoreName,
) -> Result<(), Report<TrustedServerError>> {
    let leaf_path = join_field(rendered_parent, key);
    let object = parent.as_object_mut().ok_or_else(|| {
        configuration_error(format!("expected an object containing `{leaf_path}`"))
    })?;

    let key_name = match object.get(key) {
        Some(Value::String(value)) if !value.is_empty() => value.clone(),
        Some(Value::Null) | None if field.optional => return Ok(()),
        Some(Value::String(_)) => {
            return Err(configuration_error(format!(
                "secret key reference at `{leaf_path}` must not be empty"
            )));
        }
        _ => {
            return Err(configuration_error(format!(
                "secret key reference at `{leaf_path}` must be a string"
            )));
        }
    };

    let resolved = secret_store
        .get_string(default_store_name, &key_name)
        .map_err(|_| {
            configuration_error(format!(
                "failed to resolve secret reference at `{leaf_path}`"
            ))
        })?;
    if resolved.is_empty() {
        return Err(configuration_error(format!(
            "resolved secret at `{leaf_path}` must not be empty"
        )));
    }

    object.insert(key.to_owned(), Value::String(resolved));
    Ok(())
}

fn join_field(prefix: &str, field: &str) -> String {
    if prefix.is_empty() {
        field.to_owned()
    } else {
        format!("{prefix}.{field}")
    }
}

fn missing_path(path: &str) -> Report<TrustedServerError> {
    configuration_error(format!("missing required secret path `{path}`"))
}

fn configuration_error(message: String) -> Report<TrustedServerError> {
    Report::new(TrustedServerError::Configuration { message })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::{PlatformError, StoreId};
    use std::collections::BTreeMap;

    struct MemorySecretStore {
        values: BTreeMap<String, Vec<u8>>,
    }

    impl PlatformSecretStore for MemorySecretStore {
        fn get_bytes(
            &self,
            _store_name: &StoreName,
            key: &str,
        ) -> Result<Vec<u8>, Report<PlatformError>> {
            self.values.get(key).cloned().ok_or_else(|| {
                Report::new(PlatformError::SecretStore).attach("missing test secret")
            })
        }

        fn create(
            &self,
            _store_id: &StoreId,
            _name: &str,
            _value: &str,
        ) -> Result<(), Report<PlatformError>> {
            Ok(())
        }

        fn delete(&self, _store_id: &StoreId, _name: &str) -> Result<(), Report<PlatformError>> {
            Ok(())
        }
    }

    struct Fixture;

    impl AppConfigMeta for Fixture {
        fn secret_fields() -> Vec<SecretField> {
            vec![
                SecretField {
                    kind: SecretKind::KeyInDefault,
                    optional: false,
                    path: vec![
                        SecretPathSegment::Field("outer".into()),
                        SecretPathSegment::ArrayEach,
                        SecretPathSegment::Field("token".into()),
                    ],
                },
                SecretField {
                    kind: SecretKind::KeyInDefault,
                    optional: true,
                    path: vec![
                        SecretPathSegment::Field("outer".into()),
                        SecretPathSegment::ArrayEach,
                        SecretPathSegment::Field("optional".into()),
                    ],
                },
                SecretField {
                    kind: SecretKind::KeyInDefault,
                    optional: false,
                    path: vec![
                        SecretPathSegment::OptionalField("feature".into()),
                        SecretPathSegment::Field("credential".into()),
                    ],
                },
            ]
        }
    }

    fn store() -> MemorySecretStore {
        MemorySecretStore {
            values: BTreeMap::from([
                ("token-a".to_owned(), b"resolved-a".to_vec()),
                ("token-b".to_owned(), b"resolved-b".to_vec()),
                ("feature-key".to_owned(), b"resolved-feature".to_vec()),
            ]),
        }
    }

    #[test]
    fn resolves_nested_array_values_and_skips_optional_nulls() {
        let mut data = serde_json::json!({
            "outer": [
                {"token": "token-a", "optional": null},
                {"token": "token-b"}
            ]
        });

        resolve_secret_references::<Fixture>(&mut data, &store(), &StoreName::from("secrets"))
            .expect("should resolve nested array secrets");

        assert_eq!(data["outer"][0]["token"], "resolved-a");
        assert_eq!(data["outer"][1]["token"], "resolved-b");
        assert!(data["outer"][0]["optional"].is_null());
    }

    #[test]
    fn resolves_present_and_skips_absent_optional_intermediate() {
        let mut absent = serde_json::json!({
            "outer": [{"token": "token-a"}]
        });
        resolve_secret_references::<Fixture>(&mut absent, &store(), &StoreName::from("secrets"))
            .expect("should skip absent optional intermediate");

        let mut present = serde_json::json!({
            "outer": [{"token": "token-a"}],
            "feature": {"credential": "feature-key"}
        });
        resolve_secret_references::<Fixture>(&mut present, &store(), &StoreName::from("secrets"))
            .expect("should resolve present optional intermediate");

        assert_eq!(present["feature"]["credential"], "resolved-feature");
    }

    #[test]
    fn rejects_missing_required_path_without_secret_values() {
        let mut data = serde_json::json!({"outer": [{}]});
        let err =
            resolve_secret_references::<Fixture>(&mut data, &store(), &StoreName::from("secrets"))
                .expect_err("should reject missing required secret path");

        assert!(err.to_string().contains("outer[0].token"));
        assert!(!err.to_string().contains("resolved-a"));
    }

    #[test]
    fn rejects_malformed_array_path_without_resolving_values() {
        let mut data = serde_json::json!({"outer": {"token": "token-a"}});
        let err =
            resolve_secret_references::<Fixture>(&mut data, &store(), &StoreName::from("secrets"))
                .expect_err("should reject a non-array intermediate path");

        assert!(err.to_string().contains("expected an array"));
        assert!(!err.to_string().contains("resolved-a"));
    }

    #[test]
    fn rejects_invalid_utf8_and_empty_resolved_values() {
        let mut invalid = store();
        invalid.values.insert("token-a".to_owned(), vec![0xff]);
        let mut data = serde_json::json!({"outer": [{"token": "token-a"}]});
        let err =
            resolve_secret_references::<Fixture>(&mut data, &invalid, &StoreName::from("secrets"))
                .expect_err("should reject invalid UTF-8");
        assert!(err.to_string().contains("outer[0].token"));

        let empty = MemorySecretStore {
            values: BTreeMap::from([("token-a".to_owned(), Vec::new())]),
        };
        let mut data = serde_json::json!({"outer": [{"token": "token-a"}]});
        let err =
            resolve_secret_references::<Fixture>(&mut data, &empty, &StoreName::from("secrets"))
                .expect_err("should reject empty resolved value");
        assert!(err.to_string().contains("outer[0].token"));
    }

    #[test]
    fn does_not_mutate_data_when_resolution_fails() {
        let mut data = serde_json::json!({"outer": [{"token": "missing"}]});
        let original = data.clone();
        let result =
            resolve_secret_references::<Fixture>(&mut data, &store(), &StoreName::from("secrets"));
        assert!(result.is_err(), "should fail for missing secret key");
        assert_eq!(data, original, "should preserve unresolved data on failure");
    }
}
