//! The table every provider type shares.
//!
//! A provider type is one top-level table. Its `provider` key selects what
//! runs, and every other key is one named provider's settings table:
//!
//! ```toml
//! [demand]
//! provider = ["pbs_demo"]
//!
//! [demand.pbs_demo]
//! implementation = "prebid_server"
//! endpoint = "https://prebid.example.com/openrtb2/auction"
//! ```
//!
//! A table's name is the implementation it configures, unless the table has an
//! `implementation` line, which lets several providers share one
//! implementation under names of their own. A provider with nothing to set
//! needs no table. [`ProviderTable::validate`] checks the rules that hold for
//! every type, and the code that reads a type checks what only that type
//! knows, such as which implementations exist.

use std::collections::BTreeMap;
use std::fmt;

use serde::de::{self, MapAccess, Visitor};
use serde::ser::SerializeMap as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};

/// The key that selects which providers of a type run.
pub const SELECTOR_KEY: &str = "provider";

/// The line naming the implementation a table configures when the table's
/// name is a label rather than the implementation.
pub const IMPLEMENTATION_KEY: &str = "implementation";

/// The longest provider name or implementation id, in bytes.
const MAX_NAME_BYTES: usize = 63;

/// A provider type whose `provider` names any number of providers.
pub type ProviderList = ProviderTable<Vec<String>>;

/// A provider type whose `provider` names at most one provider.
pub type ProviderChoice = ProviderTable<Option<String>>;

/// How a type's `provider` value is written.
pub trait ProviderSelection: Default + Clone + PartialEq + fmt::Debug {
    /// The selected names, in the order they were written.
    fn names(&self) -> Vec<&str>;

    /// Reads the `provider` value.
    ///
    /// # Errors
    ///
    /// Returns a message when the value has the wrong shape for this type.
    fn from_value(value: Value) -> Result<Self, String>;

    /// Writes the `provider` value, or `None` when nothing is selected.
    fn to_value(&self) -> Option<Value>;
}

impl ProviderSelection for Vec<String> {
    fn names(&self) -> Vec<&str> {
        self.iter().map(String::as_str).collect()
    }

    fn from_value(value: Value) -> Result<Self, String> {
        match value {
            Value::Array(items) => items
                .into_iter()
                .map(|item| match item {
                    Value::String(name) => Ok(name),
                    other => Err(format!("`provider` must list names, found `{other}`")),
                })
                .collect(),
            Value::String(name) => Err(format!(
                "`provider` is a list for this type, so write [\"{name}\"]"
            )),
            other => Err(format!(
                "`provider` must be a list of names, found `{other}`"
            )),
        }
    }

    fn to_value(&self) -> Option<Value> {
        (!self.is_empty()).then(|| Value::Array(self.iter().cloned().map(Value::String).collect()))
    }
}

impl ProviderSelection for Option<String> {
    fn names(&self) -> Vec<&str> {
        self.iter().map(String::as_str).collect()
    }

    fn from_value(value: Value) -> Result<Self, String> {
        match value {
            Value::String(name) => Ok(Some(name)),
            Value::Array(_) => {
                Err("`provider` names one provider for this type, not a list".to_owned())
            }
            other => Err(format!("`provider` must be a name, found `{other}`")),
        }
    }

    fn to_value(&self) -> Option<Value> {
        self.clone().map(Value::String)
    }
}

/// One provider type's table.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProviderTable<S> {
    selected: S,
    tables: BTreeMap<String, Map<String, Value>>,
}

impl<S: ProviderSelection> ProviderTable<S> {
    /// Builds a table from its selection and its named settings tables.
    #[must_use]
    pub fn new(selected: S, tables: BTreeMap<String, Map<String, Value>>) -> Self {
        Self { selected, tables }
    }

    /// The selected provider names, in the order they were written.
    #[must_use]
    pub fn selected(&self) -> Vec<&str> {
        self.selected.names()
    }

    /// Whether no provider is selected.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.selected.names().is_empty()
    }

    /// Whether the table sets nothing at all, neither a selection nor any named
    /// table, so a serialized configuration can leave it out.
    #[must_use]
    pub fn is_unset(&self) -> bool {
        self.is_empty() && self.tables.is_empty()
    }

    /// Every named settings table, selected or not.
    #[must_use]
    pub fn tables(&self) -> &BTreeMap<String, Map<String, Value>> {
        &self.tables
    }

    /// The implementation a selected name uses, being its `implementation`
    /// line or else the name itself.
    #[must_use]
    pub fn implementation_of<'a>(&'a self, name: &'a str) -> &'a str {
        self.tables
            .get(name)
            .and_then(|table| table.get(IMPLEMENTATION_KEY))
            .and_then(Value::as_str)
            .unwrap_or(name)
    }

    /// A name's settings, without its `implementation` line. A name with no
    /// table has no settings.
    #[must_use]
    pub fn settings_of(&self, name: &str) -> Map<String, Value> {
        let mut settings = self.tables.get(name).cloned().unwrap_or_default();
        settings.remove(IMPLEMENTATION_KEY);
        settings
    }

    /// Checks the rules every provider type shares, naming the type in each
    /// message.
    ///
    /// # Errors
    ///
    /// Returns a message for a name or implementation id that is not
    /// `snake_case`, a name selected twice, an `implementation` line that is
    /// not a name, or a table that `provider` does not select.
    pub fn validate(&self, type_name: &str) -> Result<(), String> {
        let selected = self.selected.names();
        for (position, name) in selected.iter().enumerate() {
            check_name(type_name, "provider name", name)?;
            if selected[..position].contains(name) {
                return Err(format!(
                    "[{type_name}] provider names `{name}` more than once"
                ));
            }
        }
        for (name, table) in &self.tables {
            check_name(type_name, "table name", name)?;
            if let Some(implementation) = table.get(IMPLEMENTATION_KEY) {
                let Some(implementation) = implementation.as_str() else {
                    return Err(format!(
                        "[{type_name}.{name}] implementation must be an implementation id"
                    ));
                };
                check_name(type_name, "implementation", implementation)?;
            }
            if !selected.contains(&name.as_str()) {
                return Err(format!(
                    "[{type_name}.{name}] is configured, but [{type_name}] provider does not select `{name}`. Add it to provider, or remove the table"
                ));
            }
        }
        Ok(())
    }
}

/// Checks that a name or implementation id is `snake_case`.
fn check_name(type_name: &str, what: &str, name: &str) -> Result<(), String> {
    let mut bytes = name.bytes();
    let valid = name.len() <= MAX_NAME_BYTES
        && bytes.next().is_some_and(|first| first.is_ascii_lowercase())
        && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_');
    if valid {
        Ok(())
    } else {
        Err(format!(
            "[{type_name}] {what} `{name}` must be snake_case: a lowercase letter followed by up to {} lowercase letters, digits or underscores",
            MAX_NAME_BYTES - 1
        ))
    }
}

impl<'de, S: ProviderSelection> Deserialize<'de> for ProviderTable<S> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct TableVisitor<S>(core::marker::PhantomData<S>);

        impl<'de, S: ProviderSelection> Visitor<'de> for TableVisitor<S> {
            type Value = ProviderTable<S>;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a provider table with a `provider` key and named tables")
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut table = ProviderTable::<S>::default();
                while let Some(key) = map.next_key::<String>()? {
                    let value = map.next_value::<Value>()?;
                    if key == SELECTOR_KEY {
                        table.selected = S::from_value(value).map_err(de::Error::custom)?;
                        continue;
                    }
                    match value {
                        Value::Object(settings) => {
                            table.tables.insert(key, settings);
                        }
                        _ => {
                            return Err(de::Error::custom(format!(
                                "`{key}` is not a setting of this table. Only `provider` and named provider tables belong here"
                            )));
                        }
                    }
                }
                Ok(table)
            }
        }

        deserializer.deserialize_map(TableVisitor(core::marker::PhantomData))
    }
}

impl<S: ProviderSelection> Serialize for ProviderTable<S> {
    fn serialize<Z>(&self, serializer: Z) -> Result<Z::Ok, Z::Error>
    where
        Z: Serializer,
    {
        let selection = self.selected.to_value();
        let mut map =
            serializer.serialize_map(Some(self.tables.len() + usize::from(selection.is_some())))?;
        if let Some(selection) = selection {
            map.serialize_entry(SELECTOR_KEY, &selection)?;
        }
        for (name, settings) in &self.tables {
            map.serialize_entry(name, settings)?;
        }
        map.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(json: Value) -> Result<ProviderList, serde_json::Error> {
        serde_json::from_value(json)
    }

    #[test]
    fn reads_the_selection_and_named_tables() {
        let table = list(serde_json::json!({
            "provider": ["pbs_demo", "aps"],
            "pbs_demo": { "implementation": "prebid_server", "endpoint": "https://pbs.example" }
        }))
        .expect("should read a provider list");

        assert_eq!(table.selected(), vec!["pbs_demo", "aps"]);
        assert_eq!(table.implementation_of("pbs_demo"), "prebid_server");
        assert_eq!(
            table.implementation_of("aps"),
            "aps",
            "a name with no table should be its own implementation"
        );
        assert!(
            !table
                .settings_of("pbs_demo")
                .contains_key(IMPLEMENTATION_KEY),
            "settings should not carry the implementation line"
        );
        table.validate("demand").expect("should accept the table");
    }

    #[test]
    fn rejects_a_table_that_is_not_selected() {
        let table = list(serde_json::json!({
            "provider": ["aps"],
            "pbs_demo": { "endpoint": "https://pbs.example" }
        }))
        .expect("should read the table");

        let error = table
            .validate("demand")
            .expect_err("should reject an unselected table");
        assert!(
            error.contains("[demand.pbs_demo]") && error.contains("does not select"),
            "should name the table and the fix: {error}"
        );
    }

    #[test]
    fn rejects_a_name_selected_twice() {
        let table =
            list(serde_json::json!({ "provider": ["aps", "aps"] })).expect("should read the table");

        let error = table
            .validate("demand")
            .expect_err("should reject a duplicate");
        assert!(error.contains("more than once"), "should say why: {error}");
    }

    #[test]
    fn rejects_names_that_are_not_snake_case() {
        for name in ["pbs-demo", "PbsDemo", "1pbs", ""] {
            let table = list(serde_json::json!({ "provider": [name] })).expect("should read");
            let error = table
                .validate("demand")
                .expect_err("should reject a name that is not snake_case");
            assert!(
                error.contains("snake_case"),
                "should say why for `{name}`: {error}"
            );
        }
    }

    #[test]
    fn rejects_a_value_that_is_not_a_table() {
        let error = list(serde_json::json!({ "provider": ["aps"], "timeout_ms": 500 }))
            .expect_err("should reject a stray value");
        assert!(
            error.to_string().contains("`timeout_ms` is not a setting"),
            "should name the stray key: {error}"
        );
    }

    #[test]
    fn a_choice_takes_one_name_and_a_list_takes_a_list() {
        let choice: ProviderChoice =
            serde_json::from_value(serde_json::json!({ "provider": "adserver_mock" }))
                .expect("should read a single name");
        assert_eq!(choice.selected(), vec!["adserver_mock"]);

        serde_json::from_value::<ProviderChoice>(serde_json::json!({ "provider": ["a"] }))
            .expect_err("a single-provider type should refuse a list");
        serde_json::from_value::<ProviderList>(serde_json::json!({ "provider": "a" }))
            .expect_err("a list type should refuse a single name");
    }

    #[test]
    fn round_trips_through_serialization() {
        let table = list(serde_json::json!({
            "provider": ["pbs_demo"],
            "pbs_demo": { "implementation": "prebid_server", "timeout_ms": 900 }
        }))
        .expect("should read the table");

        let value = serde_json::to_value(&table).expect("should serialize");
        let again: ProviderList = serde_json::from_value(value).expect("should read it back");
        assert_eq!(table, again);
    }
}
