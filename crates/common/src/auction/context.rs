//! Context query-parameter forwarding for auction providers.
//!
//! Provides a config-driven mechanism for ad-server / mediator providers to
//! forward integration-supplied data (e.g. audience segments) as URL query
//! parameters without hard-coding integration-specific knowledge.

use std::collections::HashMap;

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
pub type ContextQueryParams = HashMap<String, String>;

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
    context: &HashMap<String, serde_json::Value>,
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

/// Serialise a single [`serde_json::Value`] into a string suitable for a query
/// parameter value.  Arrays are joined with commas; strings and numbers are
/// returned directly; anything else yields an empty string (skipped).
fn serialize_context_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Array(arr) => arr
            .iter()
            .filter_map(|v| match v {
                serde_json::Value::String(s) => Some(s.clone()),
                serde_json::Value::Number(n) => Some(n.to_string()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(","),
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_build_url_with_context_params_appends_array() {
        let context = HashMap::from([(
            "permutive_segments".to_string(),
            json!(["10000001", "10000003", "adv"]),
        )]);
        let mapping = HashMap::from([("permutive_segments".to_string(), "permutive".to_string())]);

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
        let context = HashMap::from([("permutive_segments".to_string(), json!(["123", "adv"]))]);
        let mapping = HashMap::from([("permutive_segments".to_string(), "permutive".to_string())]);

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
        let mapping = HashMap::from([("permutive_segments".to_string(), "permutive".to_string())]);

        let url = build_url_with_context_params(
            "http://localhost:6767/adserver/mediate",
            &context,
            &mapping,
        );
        assert_eq!(url, "http://localhost:6767/adserver/mediate");
    }

    #[test]
    fn test_build_url_with_context_params_empty_array_skipped() {
        let context = HashMap::from([("permutive_segments".to_string(), json!([]))]);
        let mapping = HashMap::from([("permutive_segments".to_string(), "permutive".to_string())]);

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
            ("permutive_segments".to_string(), json!(["seg1"])),
            ("lockr_ids".to_string(), json!("lockr-abc-123")),
        ]);
        let mapping = HashMap::from([
            ("permutive_segments".to_string(), "permutive".to_string()),
            ("lockr_ids".to_string(), "lockr".to_string()),
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
        let context = HashMap::from([("count".to_string(), json!(42))]);
        let mapping = HashMap::from([("count".to_string(), "n".to_string())]);

        let url = build_url_with_context_params(
            "http://localhost:6767/adserver/mediate",
            &context,
            &mapping,
        );
        assert_eq!(url, "http://localhost:6767/adserver/mediate?n=42");
    }

    #[test]
    fn test_serialize_context_value_array() {
        assert_eq!(serialize_context_value(&json!(["a", "b", 3])), "a,b,3");
    }

    #[test]
    fn test_serialize_context_value_string() {
        assert_eq!(serialize_context_value(&json!("hello")), "hello");
    }

    #[test]
    fn test_serialize_context_value_number() {
        assert_eq!(serialize_context_value(&json!(99)), "99");
    }

    #[test]
    fn test_serialize_context_value_object_returns_empty() {
        assert_eq!(serialize_context_value(&json!({"a": 1})), "");
    }
}
