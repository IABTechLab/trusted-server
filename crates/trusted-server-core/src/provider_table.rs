//! The table the auction's `[demand]` and `[ad-server]` share.
//!
//! Each is one top-level table. Its `modules` key, or `module` where one
//! runs, selects what runs, and every other key is one named provider's
//! settings table:
//!
//! ```toml
//! [demand]
//! modules = ["pbs_demo"]
//!
//! [demand.pbs_demo]
//! implementation = "auction.prebid-server"
//! endpoint = "https://prebid.example.com/openrtb2/auction"
//! ```
//!
//! A table's `implementation` line names the implementation it configures by
//! its module path, which lets several providers share one implementation
//! under names of their own. Without the line the table's name stands for
//! the implementation, which only a section named by the implementation's
//! type can match, as `mock` in `[ad-server]` is `ad-server.mock`. A provider
//! with nothing to set needs no table. [`ProviderTable::validate`] checks the
//! rules that hold for every type, and the code that reads a type checks what
//! only that type knows, such as which implementations exist.

use std::collections::BTreeMap;
use std::fmt;

use serde::de::{self, MapAccess, Visitor};
use serde::ser::SerializeMap as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};

/// The line naming the implementation a table configures when the table's
/// name is a label rather than the implementation.
pub const IMPLEMENTATION_KEY: &str = "implementation";

/// The longest provider name, in bytes.
const MAX_NAME_BYTES: usize = 63;

/// A provider type whose `modules` names any number of providers.
pub type ProviderList = ProviderTable<Vec<String>>;

/// A provider type whose `module` names at most one provider.
pub type ProviderChoice = ProviderTable<Option<String>>;

/// How a type's selection is written.
pub trait ProviderSelection: Default + Clone + PartialEq + fmt::Debug {
    /// The key the selection is written under, `module` for a type that runs
    /// one and `modules` for a type that runs several.
    const KEY: &'static str;

    /// The selected names, in the order they were written.
    fn names(&self) -> Vec<&str>;

    /// Reads the selection.
    ///
    /// # Errors
    ///
    /// Returns a message when the value has the wrong shape for this type.
    fn from_value(value: Value) -> Result<Self, String>;

    /// Writes the selection, or `None` when nothing is selected.
    fn to_value(&self) -> Option<Value>;
}

impl ProviderSelection for Vec<String> {
    const KEY: &'static str = "modules";

    fn names(&self) -> Vec<&str> {
        self.iter().map(String::as_str).collect()
    }

    fn from_value(value: Value) -> Result<Self, String> {
        match value {
            Value::Array(items) => items
                .into_iter()
                .map(|item| match item {
                    Value::String(name) => Ok(name),
                    other => Err(format!("`modules` must list names, found `{other}`")),
                })
                .collect(),
            Value::String(name) => Err(format!(
                "`modules` is a list for this type, so write [\"{name}\"]"
            )),
            other => Err(format!(
                "`modules` must be a list of names, found `{other}`"
            )),
        }
    }

    fn to_value(&self) -> Option<Value> {
        (!self.is_empty()).then(|| Value::Array(self.iter().cloned().map(Value::String).collect()))
    }
}

impl ProviderSelection for Option<String> {
    const KEY: &'static str = "module";

    fn names(&self) -> Vec<&str> {
        self.iter().map(String::as_str).collect()
    }

    fn from_value(value: Value) -> Result<Self, String> {
        match value {
            Value::String(name) => Ok(Some(name)),
            Value::Array(_) => {
                Err("`module` names one provider for this type, not a list".to_owned())
            }
            other => Err(format!("`module` must be a name, found `{other}`")),
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
    /// Returns a message for a name that is not `snake_case`, a name selected
    /// twice, an `implementation` line that is not a module name, or a table
    /// the selection does not name.
    pub fn validate(&self, type_name: &str) -> Result<(), String> {
        let selected = self.selected.names();
        for (position, name) in selected.iter().enumerate() {
            check_name(type_name, "module name", name)?;
            if selected[..position].contains(name) {
                return Err(format!(
                    "[{type_name}] {} names `{name}` more than once",
                    S::KEY
                ));
            }
        }
        for (name, table) in &self.tables {
            check_name(type_name, "table name", name)?;
            if let Some(implementation) = table.get(IMPLEMENTATION_KEY) {
                let valid = implementation
                    .as_str()
                    .is_some_and(crate::module_name::is_valid);
                if !valid {
                    return Err(format!(
                        "[{type_name}.{name}] implementation must be a module name, its \
                         crate's parts joined by `.`, each in lower case letters, digits, \
                         `_` or `-`, such as `auction.example`"
                    ));
                }
            }
            if !selected.contains(&name.as_str()) {
                return Err(format!(
                    "[{type_name}.{name}] is configured, but [{type_name}] {key} does not select `{name}`. Add it to {key}, or remove the table",
                    key = S::KEY
                ));
            }
        }
        Ok(())
    }
}

/// Checks that a module's name within its type is `snake_case`.
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
                formatter.write_str("a provider table with its selection key and named tables")
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut table = ProviderTable::<S>::default();
                while let Some(key) = map.next_key::<String>()? {
                    let value = map.next_value::<Value>()?;
                    if key == S::KEY {
                        table.selected = S::from_value(value).map_err(de::Error::custom)?;
                        continue;
                    }
                    if key == "provider" {
                        return Err(de::Error::custom(format!(
                            "`provider` is `{}` here. Name what runs with `{}`",
                            S::KEY,
                            S::KEY
                        )));
                    }
                    match value {
                        Value::Object(settings) => {
                            table.tables.insert(key, settings);
                        }
                        _ => {
                            return Err(de::Error::custom(format!(
                                "`{key}` is not a setting of this table. Only `{}` and named provider tables belong here",
                                S::KEY
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
            map.serialize_entry(S::KEY, &selection)?;
        }
        for (name, settings) in &self.tables {
            map.serialize_entry(name, settings)?;
        }
        map.end()
    }
}

/// Which key a section's selection was written under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SelectionKey {
    Module,
    Modules,
}

impl SelectionKey {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Module => "module",
            Self::Modules => "modules",
        }
    }
}

/// The modules one section selects, and each one's settings table.
///
/// `modules` selects several, in the order they run, and `module` one, and a
/// section carries at most one of the two. Every other table in the section is
/// the settings of a selected module, at the name written in the selection
/// with `.` between its parts, so `modules = ["testing.example"]` reads its
/// settings from `[<section>.testing.example]`.
#[derive(Clone, Default, PartialEq)]
pub struct SectionModules {
    key: Option<SelectionKey>,
    selected: Vec<String>,
    tables: Map<String, Value>,
}

impl fmt::Debug for SectionModules {
    // The tables can hold secrets, so only the selection and the tables'
    // names are shown.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SectionModules")
            .field("selected", &self.selected)
            .field("tables", &self.tables.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl SectionModules {
    /// Reads a section's entries, in a section that holds nothing else.
    ///
    /// # Errors
    ///
    /// Returns a message for a selection of the wrong shape, for both keys at
    /// once, and for an entry that is neither a selection nor a table.
    pub fn from_entries(entries: Map<String, Value>) -> Result<Self, String> {
        let mut section = Self::default();
        for (key, value) in entries {
            match key.as_str() {
                "module" | "modules" => {
                    let key = if key == "module" {
                        SelectionKey::Module
                    } else {
                        SelectionKey::Modules
                    };
                    if section.key.is_some_and(|written| written != key) {
                        return Err(
                            "carries both `module` and `modules`. Write `module` for one or \
                             `modules` for several"
                                .to_owned(),
                        );
                    }
                    section.key = Some(key);
                    section.selected = match key {
                        SelectionKey::Module => {
                            <Option<String> as ProviderSelection>::from_value(value)?
                                .into_iter()
                                .collect()
                        }
                        SelectionKey::Modules => {
                            <Vec<String> as ProviderSelection>::from_value(value)?
                        }
                    };
                }
                _ => match value {
                    Value::Object(_) => {
                        section.tables.insert(key, value);
                    }
                    _ => {
                        return Err(format!(
                            "has `{key}`, which is not a setting it reads. Only `module` or \
                             `modules` and the settings tables of the modules they select \
                             belong there"
                        ));
                    }
                },
            }
        }
        Ok(section)
    }

    /// The selected names, as written, in the order they run.
    #[must_use]
    pub fn selected(&self) -> &[String] {
        &self.selected
    }

    /// Whether the section selects nothing and holds no table.
    #[must_use]
    pub fn is_unset(&self) -> bool {
        self.selected.is_empty() && self.tables.is_empty()
    }

    /// The settings table of a name as written in the selection, found by
    /// its parts.
    #[must_use]
    pub fn settings_of(&self, written: &str) -> Option<&Map<String, Value>> {
        let mut parts = written.split('.');
        let mut table = self.tables.get(parts.next()?)?.as_object()?;
        for part in parts {
            table = table.get(part)?.as_object()?;
        }
        Some(table)
    }

    /// The settings table of a name as written in the selection, to change.
    pub fn settings_of_mut(&mut self, written: &str) -> Option<&mut Map<String, Value>> {
        let mut parts = written.split('.');
        let mut table = self.tables.get_mut(parts.next()?)?.as_object_mut()?;
        for part in parts {
            table = table.get_mut(part)?.as_object_mut()?;
        }
        Some(table)
    }

    /// Selects `written` and stores its settings table, which is what writing
    /// both in a document does.
    pub fn insert(&mut self, written: &str, settings: Map<String, Value>) {
        if !self.selected.iter().any(|name| name == written) {
            self.selected.push(written.to_owned());
        }
        if self.key.is_none() {
            self.key = Some(SelectionKey::Modules);
        }
        let mut parts: Vec<&str> = written.split('.').collect();
        let last = parts.pop().unwrap_or(written);
        let mut table = &mut self.tables;
        for part in parts {
            let entry = table
                .entry(part.to_owned())
                .or_insert_with(|| Value::Object(Map::new()));
            if !entry.is_object() {
                *entry = Value::Object(Map::new());
            }
            let Value::Object(next) = entry else {
                return;
            };
            table = next;
        }
        table.insert(last.to_owned(), Value::Object(settings));
    }

    /// Selects `written` with no settings table of its own.
    pub fn select(&mut self, written: &str) {
        if !self.selected.iter().any(|name| name == written) {
            self.selected.push(written.to_owned());
        }
        if self.key.is_none() {
            self.key = Some(SelectionKey::Modules);
        }
    }

    /// Stops selecting `written` and drops its table.
    pub fn remove(&mut self, written: &str) {
        self.selected.retain(|name| name != written);
        let mut parts: Vec<&str> = written.split('.').collect();
        let Some(last) = parts.pop() else {
            return;
        };
        let mut table = &mut self.tables;
        for part in parts {
            let Some(Value::Object(next)) = table.get_mut(part) else {
                return;
            };
            table = next;
        }
        table.remove(last);
    }

    /// Stops selecting everything and drops every table.
    pub fn clear(&mut self) {
        self.selected.clear();
        self.tables.clear();
    }

    /// Checks the rules every section's selection follows, naming the section.
    ///
    /// # Errors
    ///
    /// Returns a message for a name that is not a module name, a name selected
    /// twice, and a table that is not the settings of a selected name.
    pub fn validate(&self, section: &str) -> Result<(), String> {
        let key = self.key.unwrap_or(SelectionKey::Modules).as_str();
        for (position, name) in self.selected.iter().enumerate() {
            if !crate::module_name::is_valid(name) {
                return Err(format!(
                    "[{section}] {key} names `{name}`, which is not a module name, being parts \
                     joined by `.`, each of lower case letters, digits, `_` or `-`"
                ));
            }
            if self.selected[..position].contains(name) {
                return Err(format!("[{section}] {key} names `{name}` more than once"));
            }
        }
        check_tables(section, key, &self.selected, &self.tables, "")
    }
}

/// Checks that every table under `prefix` is a selected name's settings, or
/// lies on the way to one.
fn check_tables(
    section: &str,
    key: &str,
    selected: &[String],
    tables: &Map<String, Value>,
    prefix: &str,
) -> Result<(), String> {
    for (name, value) in tables {
        let path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}.{name}")
        };
        if selected.contains(&path) {
            continue;
        }
        let leads_to_one = selected
            .iter()
            .any(|written| written.starts_with(&format!("{path}.")));
        match value {
            Value::Object(inner) if leads_to_one => {
                check_tables(section, key, selected, inner, &path)?;
            }
            _ => {
                return Err(format!(
                    "[{section}.{path}] is configured, but [{section}] {key} does not select \
                     `{path}`. Add it to {key}, or remove the table"
                ));
            }
        }
    }
    Ok(())
}

impl<'de> Deserialize<'de> for SectionModules {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let entries = Map::<String, Value>::deserialize(deserializer)?;
        Self::from_entries(entries)
            .map_err(|message| serde::de::Error::custom(format!("the section {message}")))
    }
}

impl Serialize for SectionModules {
    fn serialize<Z>(&self, serializer: Z) -> Result<Z::Ok, Z::Error>
    where
        Z: Serializer,
    {
        let selection =
            (!self.selected.is_empty()).then(|| match self.key.unwrap_or(SelectionKey::Modules) {
                SelectionKey::Module if self.selected.len() == 1 => (
                    SelectionKey::Module.as_str(),
                    Value::String(self.selected[0].clone()),
                ),
                _ => (
                    SelectionKey::Modules.as_str(),
                    Value::Array(self.selected.iter().cloned().map(Value::String).collect()),
                ),
            });
        let mut map =
            serializer.serialize_map(Some(self.tables.len() + usize::from(selection.is_some())))?;
        if let Some((key, value)) = selection {
            map.serialize_entry(key, &value)?;
        }
        for (name, table) in &self.tables {
            map.serialize_entry(name, table)?;
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
            "modules": ["main_source", "second_source"],
            "main_source": { "implementation": "exchange", "endpoint": "https://exchange.example" }
        }))
        .expect("should read a module list");

        assert_eq!(table.selected(), vec!["main_source", "second_source"]);
        assert_eq!(table.implementation_of("main_source"), "exchange");
        assert_eq!(
            table.implementation_of("second_source"),
            "second_source",
            "a name with no table should be its own implementation"
        );
        assert!(
            !table
                .settings_of("main_source")
                .contains_key(IMPLEMENTATION_KEY),
            "settings should not carry the implementation line"
        );
        table.validate("demand").expect("should accept the table");
    }

    #[test]
    fn rejects_a_table_that_is_not_selected() {
        let table = list(serde_json::json!({
            "modules": ["second_source"],
            "main_source": { "endpoint": "https://exchange.example" }
        }))
        .expect("should read the table");

        let error = table
            .validate("demand")
            .expect_err("should reject an unselected table");
        assert!(
            error.contains("[demand.main_source]") && error.contains("does not select"),
            "should name the table and the fix: {error}"
        );
    }

    #[test]
    fn rejects_a_name_selected_twice() {
        let table = list(serde_json::json!({ "modules": ["second_source", "second_source"] }))
            .expect("should read the table");

        let error = table
            .validate("demand")
            .expect_err("should reject a duplicate");
        assert!(error.contains("more than once"), "should say why: {error}");
    }

    #[test]
    fn rejects_names_that_are_not_snake_case() {
        for name in ["main-source", "MainSource", "1source", ""] {
            let table = list(serde_json::json!({ "modules": [name] })).expect("should read");
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
    fn an_implementation_is_named_by_its_module_path() {
        let table = list(serde_json::json!({
            "modules": ["main_source"],
            "main_source": { "implementation": "auction.example-exchange" }
        }))
        .expect("should read the table");
        table
            .validate("demand")
            .expect("a module path should name an implementation");

        for implementation in [
            serde_json::json!("Auction.Exchange"),
            serde_json::json!("auction/exchange"),
            serde_json::json!("auction..exchange"),
            serde_json::json!(7),
        ] {
            let table = list(serde_json::json!({
                "modules": ["main_source"],
                "main_source": { "implementation": implementation }
            }))
            .expect("should read the table");
            let error = table
                .validate("demand")
                .expect_err("should refuse an implementation that is not a module name");
            assert!(
                error.contains("[demand.main_source] implementation must be a module name"),
                "should say why for {implementation}: {error}"
            );
        }
    }

    #[test]
    fn rejects_a_value_that_is_not_a_table() {
        let error = list(serde_json::json!({ "modules": ["second_source"], "timeout_ms": 500 }))
            .expect_err("should reject a stray value");
        assert!(
            error.to_string().contains("`timeout_ms` is not a setting"),
            "should name the stray key: {error}"
        );
    }

    #[test]
    fn a_choice_takes_one_name_and_a_list_takes_a_list() {
        let choice: ProviderChoice =
            serde_json::from_value(serde_json::json!({ "module": "picker" }))
                .expect("should read a single name");
        assert_eq!(choice.selected(), vec!["picker"]);

        serde_json::from_value::<ProviderChoice>(serde_json::json!({ "module": ["a"] }))
            .expect_err("a single-module type should refuse a list");
        serde_json::from_value::<ProviderList>(serde_json::json!({ "modules": "a" }))
            .expect_err("a list type should refuse a single name");
    }

    #[test]
    fn refuses_the_previous_selection_key() {
        let error = list(serde_json::json!({ "provider": ["main_source"] }))
            .expect_err("should refuse the key the selection was written under before");
        assert!(
            error.to_string().contains("`provider` is `modules` here"),
            "the refusal names the key to use: {error}"
        );

        let error =
            serde_json::from_value::<ProviderChoice>(serde_json::json!({ "provider": "a" }))
                .expect_err("should refuse the previous key for a type that runs one");
        assert!(
            error.to_string().contains("`provider` is `module` here"),
            "the refusal names the key to use: {error}"
        );
    }

    #[test]
    fn round_trips_through_serialization() {
        let table = list(serde_json::json!({
            "modules": ["main_source"],
            "main_source": { "implementation": "exchange", "timeout_ms": 900 }
        }))
        .expect("should read the table");

        let value = serde_json::to_value(&table).expect("should serialize");
        let again: ProviderList = serde_json::from_value(value).expect("should read it back");
        assert_eq!(table, again);
    }
}

#[cfg(test)]
mod section_tests {
    use super::SectionModules;
    use serde_json::{Map, Value, json};

    fn read(json: Value) -> Result<SectionModules, serde_json::Error> {
        serde_json::from_value(json)
    }

    #[test]
    fn reads_the_selection_and_the_selected_tables() {
        let section = read(json!({
            "modules": ["didomi", "sourcepoint"],
            "didomi": { "api_key": "k" }
        }))
        .expect("should read a section");
        assert_eq!(section.selected(), ["didomi", "sourcepoint"]);
        assert_eq!(
            section.settings_of("didomi").and_then(|t| t.get("api_key")),
            Some(&json!("k"))
        );
        assert!(section.settings_of("sourcepoint").is_none());
        section.validate("cmp").expect("should pass validation");
    }

    #[test]
    fn module_selects_one_and_a_list_there_is_refused() {
        let section = read(json!({ "module": "didomi" })).expect("should read one");
        assert_eq!(section.selected(), ["didomi"]);
        let error = read(json!({ "module": ["didomi"] })).expect_err("should refuse a list");
        assert!(error.to_string().contains("`module` names one"), "{error}");
    }

    #[test]
    fn both_keys_at_once_are_refused() {
        let error =
            read(json!({ "module": "a", "modules": ["b"] })).expect_err("should refuse both keys");
        assert!(
            error
                .to_string()
                .contains("carries both `module` and `modules`"),
            "{error}"
        );
    }

    #[test]
    fn a_value_that_is_neither_selection_nor_table_is_refused() {
        let error = read(json!({ "modules": ["a"], "enabled": true }))
            .expect_err("should refuse a stray value");
        assert!(
            error
                .to_string()
                .contains("has `enabled`, which is not a setting it reads"),
            "{error}"
        );
    }

    #[test]
    fn a_table_no_selection_names_is_refused_by_validation() {
        let section = read(json!({ "modules": ["a"], "b": {} })).expect("should read");
        let error = section
            .validate("cmp")
            .expect_err("should refuse the stray table");
        assert_eq!(
            error,
            "[cmp.b] is configured, but [cmp] modules does not select `b`. Add it to modules, or remove the table"
        );
    }

    #[test]
    fn a_name_written_with_dots_reads_a_nested_table() {
        let section = read(json!({
            "modules": ["google", "google.diagnostics"],
            "google": { "timeout_ms": 5, "diagnostics": { "overlay": true } }
        }))
        .expect("should read");
        section
            .validate("ad-tag")
            .expect("the nested table is a selected name's");
        assert_eq!(
            section
                .settings_of("google.diagnostics")
                .and_then(|t| t.get("overlay")),
            Some(&json!(true))
        );
    }

    #[test]
    fn a_name_that_is_not_a_module_name_or_is_repeated_is_refused() {
        let section = read(json!({ "modules": ["Didomi"] })).expect("should read");
        let error = section.validate("cmp").expect_err("should refuse the name");
        assert!(error.contains("is not a module name"), "{error}");
        let section = read(json!({ "modules": ["a", "a"] })).expect("should read");
        let error = section
            .validate("cmp")
            .expect_err("should refuse the repeat");
        assert_eq!(error, "[cmp] modules names `a` more than once");
    }

    #[test]
    fn insert_select_remove_and_clear_keep_the_selection_and_tables_together() {
        let mut section = SectionModules::default();
        assert!(section.is_unset());
        let mut settings = Map::new();
        settings.insert("api_key".to_owned(), json!("k"));
        section.insert("didomi", settings);
        section.select("osano");
        assert_eq!(section.selected(), ["didomi", "osano"]);
        assert!(section.settings_of("didomi").is_some());
        section.remove("didomi");
        assert_eq!(section.selected(), ["osano"]);
        assert!(section.settings_of("didomi").is_none());
        section.clear();
        assert!(section.is_unset());
    }

    #[test]
    fn round_trips_through_serialization() {
        let section =
            read(json!({ "module": "didomi", "didomi": { "api_key": "k" } })).expect("should read");
        let value = serde_json::to_value(&section).expect("should serialize");
        assert_eq!(
            value,
            json!({ "module": "didomi", "didomi": { "api_key": "k" } })
        );
        let several = read(json!({ "modules": ["a", "b"] })).expect("should read");
        assert_eq!(
            serde_json::to_value(&several).expect("should serialize"),
            json!({ "modules": ["a", "b"] })
        );
    }
}
