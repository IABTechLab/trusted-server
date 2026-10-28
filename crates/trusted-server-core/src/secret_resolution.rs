//! Runtime resolution of `EdgeZero` app-config secret references.
//!
//! Config blobs carry secret-store key names at rest. This module walks the
//! public `EdgeZero` metadata contract, together with the leaves
//! [`ConfiguredSecretFields`] reads out of the configuration itself, and
//! replaces those names only in the in-memory value used to build runtime
//! [`crate::settings::Settings`].

use std::fmt;

use edgezero_core::app_config::{AppConfigMeta, SecretField, SecretKind, SecretPathSegment};
use error_stack::Report;
use serde_json::Value;

use crate::error::TrustedServerError;
use crate::inspect::config::{PathPattern, PathStep};
use crate::platform::{PlatformSecretStore, StoreName};

/// Where one resolution pass wrote secrets, what it wrote, and every leaf it
/// looked for, which the configuration view masks.
///
/// Never serialized, and [`fmt::Debug`] prints only how many there are.
#[derive(Clone, Default)]
pub struct ResolvedSecrets {
    paths: Vec<Vec<PathStep>>,
    values: Vec<String>,
    patterns: Vec<PathPattern>,
}

impl ResolvedSecrets {
    /// The concrete path of every leaf a secret was written into.
    pub(crate) fn paths(&self) -> &[Vec<PathStep>] {
        &self.paths
    }

    /// The secret values written.
    pub(crate) fn values(&self) -> &[String] {
        &self.values
    }

    /// Every secret leaf the pass looked for, found or not.
    pub(crate) fn patterns(&self) -> &[PathPattern] {
        &self.patterns
    }

    /// A record of `paths` and `values`, for a test of what uses one.
    #[cfg(test)]
    pub(crate) fn recorded(paths: Vec<Vec<PathStep>>, values: Vec<String>) -> Self {
        Self {
            paths,
            values,
            patterns: Vec::new(),
        }
    }
}

impl fmt::Debug for ResolvedSecrets {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResolvedSecrets")
            .field("paths", &self.paths.len())
            .field("values", &self.values.len())
            .field("patterns", &self.patterns.len())
            .finish()
    }
}

/// Secret leaves whose paths come from the configuration rather than the type.
///
/// [`AppConfigMeta::secret_fields`] is an associated function of the type, so
/// every path it can return is fixed, and `EdgeZero`'s path segments have no
/// way to say "whatever name the operator chose". A configuration can hold
/// secrets under such a name, as an Edge Cookie module block under a label
/// does, and an implementation of this trait finds those by reading the
/// configuration being loaded.
pub trait ConfiguredSecretFields: AppConfigMeta {
    /// The secret leaves `data` holds under names the operator chose, each
    /// with its full path from the configuration root.
    fn configured_secret_fields(data: &Value) -> Vec<SecretField>;
}

/// Resolve all secret references in a serialized Trusted Server app config.
///
/// Both the fixed leaves [`AppConfigMeta::secret_fields`] lists and the ones
/// [`ConfiguredSecretFields::configured_secret_fields`] reads out of `data`
/// are resolved in one pass, so a failure in either leaves the configuration
/// as it was.
///
/// The input is mutated in memory; the verified envelope is never rewritten.
/// Secret values are not included in structural or platform errors.
///
/// # Errors
///
/// Returns [`TrustedServerError::Configuration`] when a required path or key
/// is malformed, a secret is unavailable, is not valid UTF-8, or resolves to an
/// empty value.
pub fn resolve_secret_references<C: ConfiguredSecretFields>(
    data: &mut Value,
    secret_store: &dyn PlatformSecretStore,
    default_store_name: &StoreName,
) -> Result<(), Report<TrustedServerError>> {
    resolve_secret_references_with::<C>(data, secret_store, default_store_name, Vec::new())
        .map(|_| ())
}

/// Resolve all secret references in a serialized Trusted Server app config,
/// together with `extra`, the leaves known only where the configuration is
/// loaded, such as the ones a deployment's modules declare in their own
/// tables.
///
/// A leaf listed more than once is looked up once, because the second lookup
/// would read the secret itself as the name of a key. What comes back says
/// where the secrets went, which the configuration view masks.
///
/// # Errors
///
/// As [`resolve_secret_references`].
pub fn resolve_secret_references_with<C: ConfiguredSecretFields>(
    data: &mut Value,
    secret_store: &dyn PlatformSecretStore,
    default_store_name: &StoreName,
    extra: Vec<SecretField>,
) -> Result<ResolvedSecrets, Report<TrustedServerError>> {
    let fields = listed_fields::<C>(data, extra);
    let mut resolved_data = data.clone();
    let mut resolver = Resolver {
        secret_store,
        default_store_name,
        resolved: ResolvedSecrets::default(),
    };
    for field in fields {
        if matches!(field.kind, SecretKind::StoreRef) {
            continue;
        }
        resolver
            .resolved
            .patterns
            .extend(PathPattern::from_segments(&field.path));
        resolver.resolve_field(&mut resolved_data, &field, &field.path, "", &[])?;
    }
    *data = resolved_data;
    Ok(resolver.resolved)
}

/// The secret leaves a resolution pass over `data` with `extra` looks for,
/// with nothing resolved, for a check that runs while the secret fields still
/// hold key names.
pub(crate) fn unresolved_secrets<C: ConfiguredSecretFields>(
    data: &Value,
    extra: Vec<SecretField>,
) -> ResolvedSecrets {
    ResolvedSecrets {
        patterns: listed_fields::<C>(data, extra)
            .iter()
            .filter(|field| !matches!(field.kind, SecretKind::StoreRef))
            .filter_map(|field| PathPattern::from_segments(&field.path))
            .collect(),
        ..ResolvedSecrets::default()
    }
}

/// Every secret leaf of `C` and `extra` in `data`, each listed once.
fn listed_fields<C: ConfiguredSecretFields>(
    data: &Value,
    extra: Vec<SecretField>,
) -> Vec<SecretField> {
    let mut listed = std::collections::BTreeSet::new();
    C::secret_fields()
        .into_iter()
        .chain(C::configured_secret_fields(data))
        .chain(extra)
        .filter(|field| listed.insert(field.dotted_path()))
        .collect()
}

/// One resolution pass, recording each secret it writes.
struct Resolver<'a> {
    secret_store: &'a dyn PlatformSecretStore,
    default_store_name: &'a StoreName,
    resolved: ResolvedSecrets,
}

impl Resolver<'_> {
    /// `rendered_path` names the node for messages, and `steps` is the same
    /// path as the configuration view addresses it.
    fn resolve_field(
        &mut self,
        node: &mut Value,
        field: &SecretField,
        remaining: &[SecretPathSegment],
        rendered_path: &str,
        steps: &[PathStep],
    ) -> Result<(), Report<TrustedServerError>> {
        match remaining.split_first() {
            Some((SecretPathSegment::Field(name), [])) => {
                self.resolve_leaf(node, field, name.as_ref(), rendered_path, steps)
            }
            Some((SecretPathSegment::OptionalField(name), [])) => {
                if matches!(node.get(name.as_ref()), None | Some(Value::Null)) {
                    return Ok(());
                }
                self.resolve_leaf(node, field, name.as_ref(), rendered_path, steps)
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
                let next_steps = with_step(steps, PathStep::Key(name.to_string()));
                self.resolve_field(child, field, rest, &next_path, &next_steps)
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
                let next_steps = with_step(steps, PathStep::Key(name.to_string()));
                self.resolve_field(child, field, rest, &next_path, &next_steps)
            }
            Some((SecretPathSegment::ArrayEach, rest)) => {
                let items = node.as_array_mut().ok_or_else(|| {
                    configuration_error(format!("expected an array at `{rendered_path}`"))
                })?;
                for (index, item) in items.iter_mut().enumerate() {
                    let indexed_path = format!("{rendered_path}[{index}]");
                    let next_steps = with_step(steps, PathStep::Index(index));
                    self.resolve_field(item, field, rest, &indexed_path, &next_steps)?;
                }
                Ok(())
            }
            Some(_) => Err(configuration_error(format!(
                "unsupported secret path segment in `{}`",
                field.dotted_path()
            ))),
            None => Ok(()),
        }
    }

    fn resolve_leaf(
        &mut self,
        parent: &mut Value,
        field: &SecretField,
        key: &str,
        rendered_parent: &str,
        steps: &[PathStep],
    ) -> Result<(), Report<TrustedServerError>> {
        let leaf_path = join_field(rendered_parent, key);
        let object = parent.as_object_mut().ok_or_else(|| {
            configuration_error(format!("expected an object containing `{leaf_path}`"))
        })?;

        let key_name = match object.get(key) {
            Some(Value::String(value)) if !value.is_empty() => value.clone(),
            Some(Value::Null) | None if field.optional => return Ok(()),
            Some(Value::Null) | None => return Err(missing_path(&leaf_path)),
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

        let resolved = self
            .secret_store
            .get_string(self.default_store_name, &key_name)
            .map_err(|_| {
                configuration_error(format!(
                    "failed to resolve secret reference at `{leaf_path}` from secret store \
                     `{}`",
                    self.default_store_name
                ))
            })?;
        if resolved.is_empty() {
            return Err(configuration_error(format!(
                "resolved secret at `{leaf_path}` must not be empty"
            )));
        }

        self.resolved
            .paths
            .push(with_step(steps, PathStep::Key(key.to_owned())));
        if !self.resolved.values.contains(&resolved) {
            self.resolved.values.push(resolved.clone());
        }
        object.insert(key.to_owned(), Value::String(resolved));
        Ok(())
    }
}

/// `steps` with `step` added to the end.
fn with_step(steps: &[PathStep], step: PathStep) -> Vec<PathStep> {
    let mut next = Vec::with_capacity(steps.len() + 1);
    next.extend_from_slice(steps);
    next.push(step);
    next
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
                Report::new(PlatformError::SecretStore)
                    .attach(format!("missing test secret for key `{key}`"))
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

    impl ConfiguredSecretFields for Fixture {
        /// Every key under `labeled` holds its secret at
        /// `labeled.<name>.secret`, which is the shape a fixed path cannot
        /// name.
        fn configured_secret_fields(data: &Value) -> Vec<SecretField> {
            data.get("labeled")
                .and_then(Value::as_object)
                .into_iter()
                .flatten()
                .map(|(name, _)| SecretField {
                    kind: SecretKind::KeyInDefault,
                    optional: false,
                    path: vec![
                        SecretPathSegment::Field("labeled".into()),
                        SecretPathSegment::Field(name.clone().into()),
                        SecretPathSegment::Field("secret".into()),
                    ],
                })
                .collect()
        }
    }

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
    fn a_leaf_listed_twice_is_looked_up_once() {
        // The second lookup would read the secret itself as the name of a
        // key, which this store does not hold.
        let mut data = serde_json::json!({
            "outer": [{"token": "token-a"}],
            "feature": {"credential": "feature-key"}
        });
        let listed_again = vec![SecretField {
            kind: SecretKind::KeyInDefault,
            optional: true,
            path: vec![
                SecretPathSegment::OptionalField("feature".into()),
                SecretPathSegment::Field("credential".into()),
            ],
        }];

        resolve_secret_references_with::<Fixture>(
            &mut data,
            &store(),
            &StoreName::from("secrets"),
            listed_again,
        )
        .expect("should look a leaf up once however often it is listed");

        assert_eq!(data["feature"]["credential"], "resolved-feature");
    }

    #[test]
    fn resolves_a_leaf_only_the_caller_lists() {
        let mut data = serde_json::json!({
            "outer": [{"token": "token-a"}],
            "added": {"key_name": "token-b"}
        });
        let added = vec![SecretField {
            kind: SecretKind::KeyInDefault,
            optional: true,
            path: vec![
                SecretPathSegment::OptionalField("added".into()),
                SecretPathSegment::Field("key_name".into()),
            ],
        }];

        resolve_secret_references_with::<Fixture>(
            &mut data,
            &store(),
            &StoreName::from("secrets"),
            added,
        )
        .expect("should resolve the leaf the caller listed");

        assert_eq!(data["added"]["key_name"], "resolved-b");
        assert_eq!(data["outer"][0]["token"], "resolved-a");
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
    fn resolves_a_secret_under_a_name_only_the_configuration_holds() {
        let mut data = serde_json::json!({
            "outer": [{"token": "token-a"}],
            "labeled": {"chosen": {"secret": "feature-key"}},
        });

        resolve_secret_references::<Fixture>(&mut data, &store(), &StoreName::from("secrets"))
            .expect("should resolve a secret the fixed paths cannot name");

        assert_eq!(data["outer"][0]["token"], "resolved-a");
        assert_eq!(data["labeled"]["chosen"]["secret"], "resolved-feature");
    }

    #[test]
    fn a_failed_configured_field_leaves_the_fixed_ones_unresolved() {
        // Fixed and configured leaves resolve in one pass, so a failure in
        // either hands back the configuration exactly as it arrived.
        let mut data = serde_json::json!({
            "outer": [{"token": "token-a"}],
            "labeled": {"chosen": {"secret": "missing"}},
        });
        let original = data.clone();

        let err =
            resolve_secret_references::<Fixture>(&mut data, &store(), &StoreName::from("secrets"))
                .expect_err("should reject a missing configured secret");

        assert!(err.to_string().contains("labeled.chosen.secret"));
        assert_eq!(data, original, "should preserve unresolved data on failure");
    }

    #[test]
    fn rejects_missing_required_path_without_secret_values() {
        for mut data in [
            serde_json::json!({"outer": [{}]}),
            serde_json::json!({"outer": [{"token": null}]}),
        ] {
            let err = resolve_secret_references::<Fixture>(
                &mut data,
                &store(),
                &StoreName::from("secrets"),
            )
            .expect_err("should reject missing required secret path");

            assert!(err.to_string().contains("missing required secret path"));
            assert!(err.to_string().contains("outer[0].token"));
            assert!(!err.to_string().contains("resolved-a"));
        }
    }

    #[test]
    fn rejects_non_string_required_leaf() {
        let mut data = serde_json::json!({"outer": [{"token": true}]});
        let err =
            resolve_secret_references::<Fixture>(&mut data, &store(), &StoreName::from("secrets"))
                .expect_err("should reject non-string secret reference");

        assert!(err.to_string().contains("must be a string"));
        assert!(err.to_string().contains("outer[0].token"));
    }

    #[test]
    fn failed_lookup_reports_safe_reference_context_without_secret_values() {
        let plaintext_blob_value = "legacy-plaintext-credential";
        let mut data = serde_json::json!({"outer": [{"token": plaintext_blob_value}]});
        let store = MemorySecretStore {
            values: BTreeMap::from([(
                "fixture-secret-key".to_owned(),
                b"fixture-secret-value".to_vec(),
            )]),
        };

        let err =
            resolve_secret_references::<Fixture>(&mut data, &store, &StoreName::from("secrets"))
                .expect_err("should reject a missing secret key");
        let diagnostic = format!("{err:?}");

        assert!(diagnostic.contains("outer[0].token"));
        assert!(diagnostic.contains("secrets"));
        assert!(!diagnostic.contains(plaintext_blob_value));
        assert!(!diagnostic.contains("missing test secret"));
        assert!(!diagnostic.contains("fixture-secret-value"));
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
