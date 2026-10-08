//! The secret settings a module declares in its own table.
//!
//! A module's builder lists the settings in its table that hold the name of
//! a secret, each with a rule for whether the table as written puts the
//! setting to use. As the settings load, the ones not in use are cleared and
//! the others are looked up in the default secret store, and at deploy time
//! each one in use is checked to name a key. None of that needs to know
//! which module declared them.

use std::borrow::Cow;

use edgezero_core::app_config::{SecretField, SecretKind, SecretPathSegment};
use serde_json::{Map, Value};

use crate::integrations::IntegrationBuilder;
use crate::module_name::short_form;

/// The section a module is selected in and the names its table can sit
/// under there: the name with the section's type folder left off, and the
/// name in full where the two differ.
fn written_forms(builder: &IntegrationBuilder) -> Option<(&'static str, Vec<&'static str>)> {
    let name = builder.module_name()?;
    let section = builder.section()?;
    let short = short_form(section, name);
    let mut forms = vec![short];
    if short != name {
        forms.push(name);
    }
    Some((section, forms))
}

/// The form `section` selects the module by in a serialized configuration,
/// when it selects it.
fn selected_as<'a>(data: &Value, section: &str, forms: &[&'a str]) -> Option<&'a str> {
    let section = data.get(section)?;
    let named = |value: &Value| {
        let written = value.as_str()?;
        forms.iter().copied().find(|form| *form == written)
    };
    section.get("module").and_then(named).or_else(|| {
        section
            .get("modules")
            .and_then(Value::as_array)
            .and_then(|modules| modules.iter().find_map(named))
    })
}

/// The table of a name as written in a section, found by the name's parts.
fn table_mut<'a>(
    data: &'a mut Value,
    section: &str,
    written: &str,
) -> Option<&'a mut Map<String, Value>> {
    let mut node = data.get_mut(section)?;
    for part in written.split('.') {
        node = node.get_mut(part)?;
    }
    node.as_object_mut()
}

/// The value at `path` inside a module's table.
#[must_use]
pub(crate) fn value_at<'a>(table: &'a Map<String, Value>, path: &[&str]) -> Option<&'a Value> {
    let (first, rest) = path.split_first()?;
    let mut node = table.get(*first)?;
    for part in rest {
        node = node.get(*part)?;
    }
    Some(node)
}

fn remove_at(table: &mut Map<String, Value>, path: &[&str]) {
    let Some((leaf, parents)) = path.split_last() else {
        return;
    };
    let mut node = table;
    for parent in parents {
        let Some(next) = node.get_mut(*parent).and_then(Value::as_object_mut) else {
            return;
        };
        node = next;
    }
    node.remove(*leaf);
}

/// Clears every declared secret setting that is not in use, in a serialized
/// configuration whose secrets have not been looked up yet.
///
/// A setting is in use when its section selects the module and the module's
/// rule for the setting says its table uses it. A table the section does not
/// select is refused once the settings are deserialized, and clearing its
/// references first means that refusal is what an operator sees, rather than
/// a secret lookup failing.
pub(crate) fn clear_unused(data: &mut Value, builders: &[IntegrationBuilder]) {
    for builder in builders {
        let secrets = builder.secret_settings();
        if secrets.is_empty() {
            continue;
        }
        let Some((section, forms)) = written_forms(builder) else {
            continue;
        };
        let selected = selected_as(data, section, &forms);
        for form in &forms {
            let Some(table) = table_mut(data, section, form) else {
                continue;
            };
            let runs = selected == Some(*form);
            let unused = secrets
                .iter()
                .filter(|secret| !(runs && (secret.in_use)(table)))
                .map(|secret| secret.path)
                .collect::<Vec<_>>();
            for path in unused {
                remove_at(table, path);
            }
        }
    }
}

/// The secret leaves the builders' modules declare, at each name a module's
/// table can be written under.
///
/// Every level is optional, so a module that is not configured, or a setting
/// [`clear_unused`] removed, is passed over.
#[must_use]
pub(crate) fn secret_fields(builders: &[IntegrationBuilder]) -> Vec<SecretField> {
    let optional = |name: &'static str| SecretPathSegment::OptionalField(Cow::Borrowed(name));
    let mut fields = Vec::new();
    for builder in builders {
        let Some((section, forms)) = written_forms(builder) else {
            continue;
        };
        for secret in builder.secret_settings() {
            let Some((leaf, parents)) = secret.path.split_last() else {
                continue;
            };
            for form in &forms {
                let mut path = vec![optional(section)];
                path.extend(form.split('.').map(optional));
                path.extend(parents.iter().copied().map(optional));
                path.push(SecretPathSegment::Field(Cow::Borrowed(*leaf)));
                fields.push(SecretField {
                    kind: SecretKind::KeyInDefault,
                    optional: true,
                    path,
                });
            }
        }
    }
    fields
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::integrations::ModuleSecretSetting;
    use crate::integrations::registry_test_support::{probe_registration, validate_nothing};

    fn uses_key(table: &Map<String, Value>) -> bool {
        table.get("lock").and_then(Value::as_bool) == Some(true)
    }

    fn uses_spare(table: &Map<String, Value>) -> bool {
        uses_key(table)
            && table
                .get("spare")
                .and_then(|spare| spare.get("enabled"))
                .and_then(Value::as_bool)
                == Some(true)
    }

    const SECRETS: &[ModuleSecretSetting] = &[
        ModuleSecretSetting {
            path: &["key_name"],
            in_use: uses_key,
        },
        ModuleSecretSetting {
            path: &["spare", "key_name"],
            in_use: uses_spare,
        },
    ];

    fn builder() -> IntegrationBuilder {
        IntegrationBuilder::new("probe", "seam-probe", probe_registration, validate_nothing)
            .with_module_name("testing.probe")
            .with_secret_settings(SECRETS)
    }

    fn table(lock: bool, spare: bool) -> Value {
        json!({
            "lock": lock,
            "key_name": "main-key",
            "spare": { "enabled": spare, "key_name": "spare-key" },
        })
    }

    #[test]
    fn a_setting_in_use_is_kept_and_one_not_in_use_is_cleared() {
        let mut data = json!({ "testing": { "modules": ["probe"], "probe": table(true, false) } });

        clear_unused(&mut data, &[builder()]);

        assert_eq!(
            data.pointer("/testing/probe/key_name"),
            Some(&json!("main-key")),
            "should keep the setting the table puts to use"
        );
        assert_eq!(
            data.pointer("/testing/probe/spare/key_name"),
            None,
            "should clear the setting the table does not put to use"
        );
        assert_eq!(
            data.pointer("/testing/probe/spare/enabled"),
            Some(&json!(false)),
            "should leave the rest of the table as written"
        );
    }

    #[test]
    fn every_setting_of_a_module_no_section_selects_is_cleared() {
        let mut data = json!({ "testing": { "probe": table(true, true) } });

        clear_unused(&mut data, &[builder()]);

        assert_eq!(data.pointer("/testing/probe/key_name"), None);
        assert_eq!(data.pointer("/testing/probe/spare/key_name"), None);
    }

    #[test]
    fn a_module_selected_by_its_full_name_keeps_the_settings_in_the_table_at_that_name() {
        let mut data = json!({
            "testing": {
                "module": "testing.probe",
                "testing": { "probe": table(true, true) },
                "probe": table(true, true),
            }
        });

        clear_unused(&mut data, &[builder()]);

        assert_eq!(
            data.pointer("/testing/testing/probe/key_name"),
            Some(&json!("main-key")),
            "should keep the settings of the table the selection names"
        );
        assert_eq!(
            data.pointer("/testing/testing/probe/spare/key_name"),
            Some(&json!("spare-key"))
        );
        assert_eq!(
            data.pointer("/testing/probe/key_name"),
            None,
            "should clear the settings of a table under the name not selected"
        );
    }

    #[test]
    fn the_declared_leaves_are_listed_under_both_written_names() {
        let paths = secret_fields(&[builder()])
            .iter()
            .map(|field| (field.dotted_path(), field.optional))
            .collect::<Vec<_>>();

        assert_eq!(
            paths,
            vec![
                ("testing.probe.key_name".to_owned(), true),
                ("testing.testing.probe.key_name".to_owned(), true),
                ("testing.probe.spare.key_name".to_owned(), true),
                ("testing.testing.probe.spare.key_name".to_owned(), true),
            ],
            "should list each declared leaf at the short name and at the full one"
        );
    }

    #[test]
    fn a_builder_that_declares_no_secret_setting_lists_none() {
        let plain =
            IntegrationBuilder::new("probe", "seam-probe", probe_registration, validate_nothing)
                .with_module_name("testing.probe");
        let mut data = json!({ "testing": { "probe": table(false, false) } });
        let before = data.clone();

        clear_unused(&mut data, &[plain]);

        assert_eq!(data, before, "should leave the table alone");
        assert!(secret_fields(&[plain]).is_empty());
    }

    #[test]
    fn value_at_reads_a_nested_setting() {
        let Value::Object(table) = table(true, true) else {
            panic!("should build a table");
        };

        assert_eq!(value_at(&table, &["key_name"]), Some(&json!("main-key")));
        assert_eq!(
            value_at(&table, &["spare", "key_name"]),
            Some(&json!("spare-key"))
        );
        assert_eq!(value_at(&table, &["absent", "key_name"]), None);
    }
}
