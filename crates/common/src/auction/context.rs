//! Context query-parameter forwarding for auction providers.
//!
//! Provides a config-driven mechanism for ad-server / mediator providers to
//! forward integration-supplied data (e.g. audience segments) as URL query
//! parameters without hard-coding integration-specific knowledge.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

/// A strongly-typed context value forwarded from the JS client payload.
///
/// Replaces raw `serde_json::Value` so that consumers get compile-time
/// exhaustiveness checks. The `#[serde(untagged)]` attribute preserves
/// wire-format compatibility — the JS client sends plain JSON arrays, strings,
/// or numbers which serde maps to the matching variant in declaration order.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum ContextValue {
    /// A list of string values (e.g. audience segment IDs).
    StringList(Vec<String>),
    /// A single string value.
    Text(String),
    /// A numeric value.
    Number(f64),
}

/// Mapping from auction-request context keys to query-parameter names.
///
/// Used by ad-server / mediator providers to forward integration-supplied data
/// (e.g. audience segments) as URL query parameters without hard-coding
/// integration-specific knowledge.
///
/// ```toml
/// [integrations.adserver_mock.context_query_params]
/// permutive_segments = "permutive"
/// lockr_ids          = "lockr"
/// ```
pub type ContextQueryParams = BTreeMap<String, String>;

/// Build a URL by appending context values as query parameters according to the
/// provided mapping.
///
/// For each entry in `mapping`, if the corresponding key exists in `context`:
/// - **Arrays** are serialised as a comma-separated string.
/// - **Strings / numbers** are serialised as-is.
/// - Other JSON types are skipped.
///
/// The [`url::Url`] crate is used for construction so all values are
/// percent-encoded, preventing query-parameter injection.
///
/// Returns the original `base_url` unchanged when no parameters are appended.
#[must_use]
pub fn build_url_with_context_params(
    base_url: &str,
    context: &HashMap<String, ContextValue>,
    mapping: &ContextQueryParams,
) -> String {
    let Ok(mut url) = url::Url::parse(base_url) else {
        log::warn!("build_url_with_context_params: failed to parse base URL, returning as-is");
        return base_url.to_string();
    };

    let mut appended = 0usize;

    for (context_key, param_name) in mapping {
        if let Some(value) = context.get(context_key) {
            let serialized = serialize_context_value(value);
            if !serialized.is_empty() {
                url.query_pairs_mut().append_pair(param_name, &serialized);
                appended += 1;
            }
        }
    }

    if appended > 0 {
        log::info!(
            "build_url_with_context_params: appended {} context query params",
            appended
        );
    }

    url.to_string()
}

/// Serialise a single [`ContextValue`] into a string suitable for a query
/// parameter value.  String lists are joined with commas; strings and numbers
/// are returned directly.
fn serialize_context_value(value: &ContextValue) -> String {
    match value {
        ContextValue::StringList(items) => items.join(","),
        ContextValue::Text(s) => s.clone(),
        ContextValue::Number(n) => n.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_url_with_context_params_appends_array() {
        let context = HashMap::from([(
            "permutive_segments".to_string(),
            ContextValue::StringList(vec!["10000001".into(), "10000003".into(), "adv".into()]),
        )]);
        let mapping = BTreeMap::from([("permutive_segments".to_string(), "permutive".to_string())]);

        let url = build_url_with_context_params(
            "http://localhost:6767/adserver/mediate",
            &context,
            &mapping,
        );
        assert_eq!(
            url,
            "http://localhost:6767/adserver/mediate?permutive=10000001%2C10000003%2Cadv"
        );
    }

    #[test]
    fn test_build_url_with_context_params_preserves_existing_query() {
        let context = HashMap::from([(
            "permutive_segments".to_string(),
            ContextValue::StringList(vec!["123".into(), "adv".into()]),
        )]);
        let mapping = BTreeMap::from([("permutive_segments".to_string(), "permutive".to_string())]);

        let url = build_url_with_context_params(
            "http://localhost:6767/adserver/mediate?debug=true",
            &context,
            &mapping,
        );
        assert_eq!(
            url,
            "http://localhost:6767/adserver/mediate?debug=true&permutive=123%2Cadv"
        );
    }

    #[test]
    fn test_build_url_with_context_params_no_matching_keys() {
        let context = HashMap::new();
        let mapping = BTreeMap::from([("permutive_segments".to_string(), "permutive".to_string())]);

        let url = build_url_with_context_params(
            "http://localhost:6767/adserver/mediate",
            &context,
            &mapping,
        );
        assert_eq!(url, "http://localhost:6767/adserver/mediate");
    }

    #[test]
    fn test_build_url_with_context_params_empty_array_skipped() {
        let context = HashMap::from([(
            "permutive_segments".to_string(),
            ContextValue::StringList(vec![]),
        )]);
        let mapping = BTreeMap::from([("permutive_segments".to_string(), "permutive".to_string())]);

        let url = build_url_with_context_params(
            "http://localhost:6767/adserver/mediate",
            &context,
            &mapping,
        );
        assert!(!url.contains("permutive="));
    }

    #[test]
    fn test_build_url_with_context_params_multiple_mappings() {
        let context = HashMap::from([
            (
                "permutive_segments".to_string(),
                ContextValue::StringList(vec!["seg1".into()]),
            ),
            (
                "lockr_ids".to_string(),
                ContextValue::Text("lockr-abc-123".into()),
            ),
        ]);
        let mapping = BTreeMap::from([
            ("lockr_ids".to_string(), "lockr".to_string()),
            ("permutive_segments".to_string(), "permutive".to_string()),
        ]);

        let url = build_url_with_context_params(
            "http://localhost:6767/adserver/mediate",
            &context,
            &mapping,
        );
        assert!(url.contains("permutive=seg1"));
        assert!(url.contains("lockr=lockr-abc-123"));
    }

    #[test]
    fn test_build_url_with_context_params_scalar_number() {
        let context = HashMap::from([("count".to_string(), ContextValue::Number(42.0))]);
        let mapping = BTreeMap::from([("count".to_string(), "n".to_string())]);

        let url = build_url_with_context_params(
            "http://localhost:6767/adserver/mediate",
            &context,
            &mapping,
        );
        assert_eq!(url, "http://localhost:6767/adserver/mediate?n=42");
    }

    #[test]
    fn test_serialize_context_value_string_list() {
        assert_eq!(
            serialize_context_value(&ContextValue::StringList(vec![
                "a".into(),
                "b".into(),
                "3".into()
            ])),
            "a,b,3"
        );
    }

    #[test]
    fn test_serialize_context_value_text() {
        assert_eq!(
            serialize_context_value(&ContextValue::Text("hello".into())),
            "hello"
        );
    }

    #[test]
    fn test_serialize_context_value_number() {
        assert_eq!(serialize_context_value(&ContextValue::Number(99.0)), "99");
    }

    #[test]
    fn test_context_value_deserialize_array() {
        let v: ContextValue = serde_json::from_str(r#"["a","b"]"#).unwrap();
        assert_eq!(v, ContextValue::StringList(vec!["a".into(), "b".into()]));
    }

    #[test]
    fn test_context_value_deserialize_string() {
        let v: ContextValue = serde_json::from_str(r#""hello""#).unwrap();
        assert_eq!(v, ContextValue::Text("hello".into()));
    }

    #[test]
    fn test_context_value_deserialize_number() {
        let v: ContextValue = serde_json::from_str("42").unwrap();
        assert_eq!(v, ContextValue::Number(42.0));
    }
}
