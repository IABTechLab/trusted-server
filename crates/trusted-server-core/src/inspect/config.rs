//! The configuration a deployment is running, served to anyone at
//! [`CONFIG_PAGE_PATH`] and [`CONFIG_JSON_PATH`].
//!
//! What is shown is the settings the handler holds, after defaults have been
//! applied, with values masked as [`MASK`]:
//!
//! 1. Every secret, meaning a value the loader filled in from the secret
//!    store or a leaf declared as one, whatever the publisher's document says.
//! 2. Every value sensitive by default, being a settings field serialized
//!    with [`crate::redacted::sensitive`] and every
//!    [`Redacted`](crate::redacted::Redacted) value, unless `[inspect] show`
//!    names it.
//! 3. Everything `[inspect] hide` names.

use std::collections::HashSet;
use std::fmt::{self, Write as _};

use edgezero_core::app_config::{AppConfigMeta as _, SecretPathSegment};
use edgezero_core::body::Body as EdgeBody;
use error_stack::Report;
use http::{HeaderValue, Request, Response, StatusCode, header};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value, json};

use crate::config::TrustedServerAppConfig;
use crate::error::TrustedServerError;
use crate::integrations::IntegrationBuilder;
use crate::redacted::{SENSITIVE_MARKER, to_marked_value};
use crate::secret_resolution::unresolved_secrets;
use crate::settings::Settings;

use super::{MASK, render_page};

/// The page form of the configuration.
pub const CONFIG_PAGE_PATH: &str = "/_ts/config";

/// The JSON form of the configuration.
pub const CONFIG_JSON_PATH: &str = "/_ts/config.json";

/// Both addresses, the page first.
pub const CONFIG_PATHS: [&str; 2] = [CONFIG_PAGE_PATH, CONFIG_JSON_PATH];

const NOT_PUBLISHED: &str = "This publisher does not publish its configuration.";

const PATTERN_FORM: &str = "A pattern is keys joined by `.`, with `[]` for every element of a \
                            list or `[N]` for one, such as `handlers[0].path`";

/// The settings table that names the domain's permission policy.
const PERMISSIONS_TABLE: &str = "permissions";

/// The policy file's SHA-256, which the push writes into every blob.
const POLICY_SHA256_KEY: &str = "sha256";

/// The policy file itself, which the push writes into the blob on the
/// adapters that read it from there, and which the runtime loader takes out of
/// the settings when it loads the policy.
const POLICY_TEXT_KEY: &str = "text";

/// What the pattern checks see in place of a SHA-256 the push has not written
/// yet, so a pattern naming it is judged as it is once the push has run.
const SHA256_BEFORE_THE_PUSH: &str =
    "0000000000000000000000000000000000000000000000000000000000000000";

/// The `[inspect]` section, which says what the configuration page shows.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InspectConfig {
    /// Whether the configuration is published. `false` answers not found.
    #[serde(default = "default_config")]
    pub config: bool,
    /// Values masked by default that are shown instead. A secret cannot be.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub show: Vec<PathPattern>,
    /// Values masked as well as the defaults.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hide: Vec<PathPattern>,
}

fn default_config() -> bool {
    true
}

impl Default for InspectConfig {
    fn default() -> Self {
        Self {
            config: default_config(),
            show: Vec::new(),
            hide: Vec::new(),
        }
    }
}

impl InspectConfig {
    /// Whether the section says nothing the defaults do not.
    #[must_use]
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// One step of a concrete path through the view.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum PathStep {
    /// A key of an object.
    Key(String),
    /// An index into a list.
    Index(usize),
}

/// A concrete path written the way a pattern is, such as `handlers[0].path`.
#[must_use]
pub fn render_path(steps: &[PathStep]) -> String {
    let mut out = String::new();
    for step in steps {
        match step {
            PathStep::Key(key) => {
                if !out.is_empty() {
                    out.push('.');
                }
                out.push_str(key);
            }
            PathStep::Index(index) => {
                let _ = write!(out, "[{index}]");
            }
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PatternStep {
    Key(String),
    Each,
    Index(usize),
}

/// Values in the view named by keys joined by `.`, with `[]` for every
/// element of a list and `[N]` for one, such as
/// `proxy.asset_routes[].origin_url` or `handlers[0].path`.
///
/// A pattern names the values at exactly its own depth. A key holding `.`,
/// `[` or `]` cannot be named.
#[derive(Clone, PartialEq, Eq)]
pub struct PathPattern {
    text: String,
    steps: Vec<PatternStep>,
}

impl PathPattern {
    /// Reads a pattern.
    ///
    /// # Errors
    ///
    /// A message naming `text` and what is wrong with it.
    pub fn parse(text: &str) -> Result<Self, String> {
        let malformed =
            |why: &str| format!("`{text}` is not a path pattern, because {why}. {PATTERN_FORM}");
        let mut steps = Vec::new();
        let mut rest = text;
        loop {
            let end = rest.find(['.', '[', ']']).unwrap_or(rest.len());
            let (key, after_key) = rest.split_at(end);
            if key.is_empty() {
                return Err(malformed("a key is empty"));
            }
            if key.chars().any(|c| c.is_whitespace() || c.is_control()) {
                return Err(malformed("a key holds a space or a control character"));
            }
            steps.push(PatternStep::Key(key.to_owned()));
            rest = after_key;
            while let Some(open) = rest.strip_prefix('[') {
                let Some((inside, after)) = open.split_once(']') else {
                    return Err(malformed("a `[` is not closed"));
                };
                let step = if inside.is_empty() {
                    PatternStep::Each
                } else if inside.bytes().all(|b| b.is_ascii_digit()) {
                    PatternStep::Index(
                        inside
                            .parse()
                            .map_err(|_| malformed("an index is too large"))?,
                    )
                } else {
                    return Err(malformed("only a number may sit between `[` and `]`"));
                };
                steps.push(step);
                rest = after;
            }
            if rest.is_empty() {
                break;
            }
            match rest.strip_prefix('.') {
                Some(after) => rest = after,
                None if rest.starts_with(']') => return Err(malformed("a `]` has no `[`")),
                None => return Err(malformed("a `]` is followed by neither `.` nor `[`")),
            }
        }
        Ok(Self {
            text: text.to_owned(),
            steps,
        })
    }

    /// The pattern a secret or sensitive declaration names, or `None` for a
    /// segment kind a pattern cannot express.
    #[must_use]
    pub fn from_segments(segments: &[SecretPathSegment]) -> Option<Self> {
        let mut steps = Vec::with_capacity(segments.len());
        let mut text = String::new();
        for segment in segments {
            match segment {
                SecretPathSegment::Field(name) | SecretPathSegment::OptionalField(name) => {
                    if !text.is_empty() {
                        text.push('.');
                    }
                    text.push_str(name);
                    steps.push(PatternStep::Key(name.to_string()));
                }
                SecretPathSegment::ArrayEach => {
                    text.push_str("[]");
                    steps.push(PatternStep::Each);
                }
                _ => return None,
            }
        }
        Some(Self { text, steps })
    }

    /// The pattern as written.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// Whether the pattern names the policy file in `permissions.text`, or
    /// anything beneath it.
    fn names_policy_text(&self) -> bool {
        matches!(
            self.steps.as_slice(),
            [PatternStep::Key(table), PatternStep::Key(key), ..]
                if table == PERMISSIONS_TABLE && key == POLICY_TEXT_KEY
        )
    }

    /// Whether the pattern is `permissions.text` itself.
    fn is_policy_text(&self) -> bool {
        self.names_policy_text() && self.steps.len() == 2
    }

    /// Whether the concrete path `path` is one the pattern names.
    #[must_use]
    pub fn matches(&self, path: &[PathStep]) -> bool {
        self.steps.len() == path.len()
            && self
                .steps
                .iter()
                .zip(path)
                .all(|(pattern, step)| match (pattern, step) {
                    (PatternStep::Key(want), PathStep::Key(got)) => want == got,
                    (PatternStep::Each, PathStep::Index(_)) => true,
                    (PatternStep::Index(want), PathStep::Index(got)) => want == got,
                    _ => false,
                })
    }
}

impl fmt::Debug for PathPattern {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.text)
    }
}

impl fmt::Display for PathPattern {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.text)
    }
}

impl Serialize for PathPattern {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.text)
    }
}

impl<'de> Deserialize<'de> for PathPattern {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let text = String::deserialize(deserializer)?;
        Self::parse(&text).map_err(serde::de::Error::custom)
    }
}

/// The configuration as the endpoint shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct ConfigView {
    /// Every masked path, concrete and sorted.
    pub masked: Vec<String>,
    /// The settings, each masked value replaced by [`MASK`] and every
    /// object's keys sorted.
    pub settings: Value,
}

/// Builds the view of `settings`.
///
/// # Errors
///
/// When the settings cannot be represented as JSON.
pub fn build_view(settings: &Settings) -> Result<ConfigView, Report<TrustedServerError>> {
    let marked = marked_settings(settings)?;
    Ok(Rules::new(settings).render(&marked))
}

fn marked_settings(settings: &Settings) -> Result<Value, Report<TrustedServerError>> {
    // The serializer's error is left out, because nothing may carry a value
    // out of the settings.
    to_marked_value(settings).map_err(|_| {
        Report::new(TrustedServerError::Configuration {
            message: "the settings could not be represented for the configuration view".to_owned(),
        })
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum List {
    Show,
    Hide,
}

impl fmt::Display for List {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Show => "show",
            Self::Hide => "hide",
        })
    }
}

/// Everything that decides whether a value is masked.
#[derive(Clone)]
struct Rules<'a> {
    secret_paths: &'a [Vec<PathStep>],
    secret_patterns: Vec<PathPattern>,
    secret_values: &'a [String],
    show: Vec<&'a PathPattern>,
    hide: Vec<&'a PathPattern>,
}

impl<'a> Rules<'a> {
    fn new(settings: &'a Settings) -> Self {
        let secrets = settings.resolved_secrets();
        // Core's own declarations are added here as well as recorded by the
        // loader, so settings built any other way still mask them.
        let secret_patterns = secrets
            .patterns()
            .iter()
            .cloned()
            .chain(
                TrustedServerAppConfig::secret_fields()
                    .iter()
                    .filter_map(|field| PathPattern::from_segments(&field.path)),
            )
            .collect();
        Self {
            secret_paths: secrets.paths(),
            secret_patterns,
            secret_values: secrets.values(),
            show: settings.inspect.show.iter().collect(),
            hide: settings.inspect.hide.iter().collect(),
        }
    }

    /// These rules less one of the publisher's patterns.
    fn without(&self, list: List, index: usize) -> Self {
        let mut rules = self.clone();
        let _removed = match list {
            List::Show => rules.show.remove(index),
            List::Hide => rules.hide.remove(index),
        };
        rules
    }

    fn is_secret(&self, path: &[PathStep]) -> bool {
        self.secret_paths.iter().any(|secret| secret == path)
            || self
                .secret_patterns
                .iter()
                .any(|pattern| pattern.matches(path))
    }

    fn render(&self, marked: &Value) -> ConfigView {
        let mut masked = Vec::new();
        let mut path = Vec::new();
        let mut settings = self.mask(marked, &mut path, &mut masked);
        self.scan(&mut settings, &mut path, &mut masked);
        masked.sort();
        masked.dedup();
        ConfigView {
            masked: masked.iter().map(|steps| render_path(steps)).collect(),
            settings,
        }
    }

    /// `node` with every sensitive marker resolved, every masked value
    /// replaced and every object's keys sorted.
    fn mask(
        &self,
        node: &Value,
        path: &mut Vec<PathStep>,
        masked: &mut Vec<Vec<PathStep>>,
    ) -> Value {
        let (inner, marked_sensitive) = match unmark(node) {
            Some(inner) => (inner, true),
            None => (node, false),
        };
        if is_empty(inner) {
            return inner.clone();
        }
        let shown = self.show.iter().any(|p| p.matches(path));
        if self.is_secret(path)
            || self.hide.iter().any(|p| p.matches(path))
            || (marked_sensitive && !shown)
        {
            masked.push(path.clone());
            return Value::String(MASK.to_owned());
        }
        match inner {
            Value::Object(map) => {
                let mut entries: Vec<(&String, &Value)> = map.iter().collect();
                entries.sort_unstable_by(|left, right| left.0.cmp(right.0));
                let mut out = Map::new();
                for (key, value) in entries {
                    path.push(PathStep::Key(key.clone()));
                    out.insert(key.clone(), self.mask(value, path, masked));
                    path.pop();
                }
                Value::Object(out)
            }
            Value::Array(items) => {
                let mut out = Vec::with_capacity(items.len());
                for (index, item) in items.iter().enumerate() {
                    path.push(PathStep::Index(index));
                    out.push(self.mask(item, path, masked));
                    path.pop();
                }
                Value::Array(out)
            }
            other => other.clone(),
        }
    }

    /// Masks every string, and every key, that contains a secret value,
    /// wherever it sits. A key holding a secret is renamed [`MASK`], or
    /// `XXXX-2` and on where that name is taken, and every path already
    /// recorded beneath it is replaced by the renamed one, so no recorded path
    /// carries the secret.
    fn scan(&self, node: &mut Value, path: &mut Vec<PathStep>, masked: &mut Vec<Vec<PathStep>>) {
        match node {
            Value::String(text) => {
                if self.holds_secret(text) {
                    masked.push(path.clone());
                    *node = Value::String(MASK.to_owned());
                }
            }
            Value::Array(items) => {
                for (index, item) in items.iter_mut().enumerate() {
                    path.push(PathStep::Index(index));
                    self.scan(item, path, masked);
                    path.pop();
                }
            }
            Value::Object(map) => {
                let entries = std::mem::take(map);
                let kept: HashSet<String> = entries
                    .keys()
                    .filter(|key| !self.holds_secret(key))
                    .cloned()
                    .collect();
                let mut sorted = std::collections::BTreeMap::new();
                for (key, mut value) in entries {
                    if self.holds_secret(&key) {
                        let mut renamed = MASK.to_owned();
                        let mut copy = 1;
                        while kept.contains(&renamed) || sorted.contains_key(&renamed) {
                            copy += 1;
                            renamed = format!("{MASK}-{copy}");
                        }
                        path.push(PathStep::Key(key));
                        masked.retain(|recorded| !recorded.starts_with(path));
                        path.pop();
                        path.push(PathStep::Key(renamed.clone()));
                        masked.push(path.clone());
                        path.pop();
                        sorted.insert(renamed, Value::String(MASK.to_owned()));
                        continue;
                    }
                    path.push(PathStep::Key(key.clone()));
                    self.scan(&mut value, path, masked);
                    path.pop();
                    sorted.insert(key, value);
                }
                map.extend(sorted);
            }
            Value::Null | Value::Bool(_) | Value::Number(_) => {}
        }
    }

    fn holds_secret(&self, text: &str) -> bool {
        self.secret_values
            .iter()
            .any(|secret| !secret.is_empty() && text.contains(secret.as_str()))
    }
}

/// The value a sensitive marker wraps, or `None` when `node` is not one.
fn unmark(node: &Value) -> Option<&Value> {
    match node {
        Value::Object(map) if map.len() == 1 => map.get(SENSITIVE_MARKER),
        _ => None,
    }
}

/// Whether there is nothing in `value` to hide.
fn is_empty(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(text) => text.is_empty(),
        Value::Array(items) => items.is_empty(),
        Value::Object(map) => map.is_empty(),
        Value::Bool(_) | Value::Number(_) => false,
    }
}

/// The concrete paths of the values in `marked` that `pattern` names.
fn select(marked: &Value, pattern: &PathPattern) -> Vec<Vec<PathStep>> {
    fn walk(
        node: &Value,
        steps: &[PatternStep],
        path: &mut Vec<PathStep>,
        out: &mut Vec<Vec<PathStep>>,
    ) {
        let node = unmark(node).unwrap_or(node);
        let Some((step, rest)) = steps.split_first() else {
            out.push(path.clone());
            return;
        };
        match (step, node) {
            (PatternStep::Key(key), Value::Object(map)) => {
                if let Some(child) = map.get(key) {
                    path.push(PathStep::Key(key.clone()));
                    walk(child, rest, path, out);
                    path.pop();
                }
            }
            (PatternStep::Each, Value::Array(items)) => {
                for (index, item) in items.iter().enumerate() {
                    path.push(PathStep::Index(index));
                    walk(item, rest, path, out);
                    path.pop();
                }
            }
            (PatternStep::Index(index), Value::Array(items)) => {
                if let Some(item) = items.get(*index) {
                    path.push(PathStep::Index(*index));
                    walk(item, rest, path, out);
                    path.pop();
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(marked, &pattern.steps, &mut Vec::new(), &mut out);
    out
}

/// Refuses an `[inspect]` pattern that would not be honored as written,
/// being one that matches nothing, a `show` naming a secret, one another
/// pattern of its list already covers, a `show` a `hide` covers, one that
/// changes nothing the view shows, one under `permissions.text`, which the
/// view never shows, and any pattern beside `config = false`. Costs nothing
/// when the section has no patterns.
///
/// The settings are checked as the view sees a domain the push wrote, with
/// `permissions.sha256` present and `permissions.text` gone, so a pattern is
/// judged alike before the push, by the push and when the settings load.
///
/// # Errors
///
/// Naming the first pattern at fault.
pub(crate) fn validate_patterns(settings: &Settings) -> Result<(), Report<TrustedServerError>> {
    let inspect = &settings.inspect;
    if inspect.show.is_empty() && inspect.hide.is_empty() {
        return Ok(());
    }
    if !inspect.config {
        return Err(configuration(
            "[inspect] show and hide say what the configuration page shows, and config = false \
             publishes no page, so they change nothing. Remove them, or publish the page"
                .to_owned(),
        ));
    }
    for (list, patterns) in [(List::Show, &inspect.show), (List::Hide, &inspect.hide)] {
        if let Some(pattern) = patterns.iter().find(|pattern| pattern.names_policy_text()) {
            let under = if pattern.is_policy_text() {
                ""
            } else {
                ", which is under `permissions.text`"
            };
            return Err(configuration(format!(
                "[inspect] {list} names `{pattern}`{under}, the policy file the push writes. The \
                 configuration page never shows the file, so the pattern changes nothing"
            )));
        }
    }
    let mut marked = marked_settings(settings)?;
    as_the_view_sees_a_pushed_domain(&mut marked);
    let rules = Rules::new(settings);
    let full = rules.render(&marked);
    for (list, patterns) in [(List::Show, &inspect.show), (List::Hide, &inspect.hide)] {
        for (index, pattern) in patterns.iter().enumerate() {
            let selected = select(&marked, pattern);
            if selected.is_empty() {
                return Err(configuration(format!(
                    "[inspect] {list} names `{pattern}`, which matches no value in this \
                     configuration, so it changes nothing"
                )));
            }
            if list == List::Show && selected.iter().any(|path| rules.is_secret(path)) {
                return Err(configuration(format!(
                    "[inspect] show names `{pattern}`, which is a secret. A secret is always \
                     masked, so it cannot be shown"
                )));
            }
            let covers = |other: &PathPattern| {
                let covered = select(&marked, other);
                selected.iter().all(|path| covered.contains(path))
            };
            if let Some(other) = patterns
                .iter()
                .enumerate()
                .find(|(other_index, other)| *other_index != index && covers(other))
                .map(|(_, other)| other)
            {
                return Err(configuration(format!(
                    "[inspect] {list} names `{pattern}`, which changes nothing, because \
                     `{other}` in the same list already names everything it does"
                )));
            }
            if list == List::Show
                && let Some(hide) = inspect.hide.iter().find(|hide| covers(hide))
            {
                return Err(configuration(format!(
                    "[inspect] show names `{pattern}`, which changes nothing, because hide \
                     names it as `{hide}` and hide wins over show"
                )));
            }
            if rules.without(list, index).render(&marked) == full {
                let reason = match list {
                    List::Show => "nothing it names is masked by default",
                    List::Hide => "everything it names is masked or empty already",
                };
                return Err(configuration(format!(
                    "[inspect] {list} names `{pattern}`, which changes nothing, because {reason}"
                )));
            }
        }
    }
    Ok(())
}

/// `marked` with `permissions.sha256` present and `permissions.text` gone,
/// as the view shows a domain the push wrote and the runtime loader loaded.
fn as_the_view_sees_a_pushed_domain(marked: &mut Value) {
    let Value::Object(settings) = marked else {
        return;
    };
    let permissions = settings
        .entry(PERMISSIONS_TABLE)
        .or_insert_with(|| Value::Object(Map::new()));
    if let Value::Object(permissions) = permissions {
        permissions.remove(POLICY_TEXT_KEY);
        permissions
            .entry(POLICY_SHA256_KEY)
            .or_insert_with(|| Value::String(SHA256_BEFORE_THE_PUSH.to_owned()));
    }
}

/// [`validate_patterns`] for a deployment's settings before they are pushed,
/// when their secret fields still hold key names. The view is handed every
/// secret leaf the load will look for, the ones only the configuration and
/// `extra_integrations` name included, so a pattern the load refuses is
/// refused here.
///
/// # Errors
///
/// As [`validate_patterns`].
pub(crate) fn validate_patterns_for_deploy(
    settings: &Settings,
    extra_integrations: &[IntegrationBuilder],
) -> Result<(), Report<TrustedServerError>> {
    if settings.inspect.show.is_empty() && settings.inspect.hide.is_empty() {
        return Ok(());
    }
    // The serializer's error is left out, as in `marked_settings`.
    let data = serde_json::to_value(settings).map_err(|_| {
        configuration("the settings could not be represented for the configuration view".to_owned())
    })?;
    let builders = crate::integrations::all_builders(extra_integrations).collect::<Vec<_>>();
    let mut settings = settings.clone();
    settings.set_resolved_secrets(unresolved_secrets::<TrustedServerAppConfig>(
        &data,
        crate::module_secrets::secret_fields(&builders),
    ));
    validate_patterns(&settings)
}

fn configuration(message: String) -> Report<TrustedServerError> {
    Report::new(TrustedServerError::Configuration { message })
}

/// Answers [`CONFIG_PAGE_PATH`] with the page and [`CONFIG_JSON_PATH`] with
/// the JSON, or not found when the publisher does not publish its
/// configuration.
#[must_use]
pub fn handle_config(settings: &Settings, req: &Request<EdgeBody>) -> Response<EdgeBody> {
    let json = req.uri().path() == CONFIG_JSON_PATH;
    if !settings.inspect.config {
        let payload = json!({ "error": NOT_PUBLISHED });
        return answer(StatusCode::NOT_FOUND, json, &payload);
    }
    match build_view(settings) {
        Ok(view) => answer(StatusCode::OK, json, &view_payload(&view)),
        Err(_) => {
            log::error!("the configuration view could not be built");
            let payload = json!({ "error": "The configuration could not be shown." });
            answer(StatusCode::INTERNAL_SERVER_ERROR, json, &payload)
        }
    }
}

/// What the endpoint serves for `view`, with the version of the core crate
/// that answered.
#[must_use]
pub fn view_payload(view: &ConfigView) -> Value {
    json!({
        "version": env!("CARGO_PKG_VERSION"),
        "masked": view.masked,
        "settings": view.settings,
    })
}

fn answer(status: StatusCode, json: bool, payload: &Value) -> Response<EdgeBody> {
    let (content_type, body) = if json {
        ("application/json", payload.to_string())
    } else {
        (
            "text/html; charset=utf-8",
            render_page("Configuration", payload),
        )
    };
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, HeaderValue::from_static(content_type))
        .header(header::CACHE_CONTROL, HeaderValue::from_static("no-store"))
        .header(
            header::ACCESS_CONTROL_ALLOW_ORIGIN,
            HeaderValue::from_static("*"),
        )
        .body(EdgeBody::from(body.into_bytes()))
        .expect("should build configuration response")
}

#[cfg(test)]
mod tests;
