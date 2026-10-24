use serde::{Deserialize, Serialize};

/// A 2 MiB cap accommodates roughly 600 KiB surrogate-escaped plans at the 50,000-scalar input limit.
pub const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
/// Maximum visible-text length sent for signing, measured in Unicode scalars.
pub const MAX_TEXT_SCALARS: usize = 50_000;
/// Policy responses contain only one digest; cap them independently of plans.
pub const MAX_POLICY_RESPONSE_BYTES: usize = 1024;

/// JSON request body for the Encypher CDN signing endpoint.
#[derive(Serialize)]
pub struct SignRequest<'a> {
    /// Canonical visible article text.
    pub text: &'a str,
    /// Absolute page URL without its query string.
    pub page_url: &'a str,
    /// Optional page title capped at 500 Unicode scalars.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document_title: Option<&'a str>,
    /// Boundary detection label used for the article.
    pub boundary_selector: &'a str,
    /// Server policy used to select this article's cache namespace.
    pub expected_policy_digest: &'a str,
}

/// One invisible-marker insertion returned by the signing endpoint.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanOperation {
    /// Visible codepoint index after which to insert, or -1 for before the first.
    pub insert_after_index: i64,
    /// Invisible marker characters to insert.
    pub marker: String,
}

/// A complete marker embedding plan.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EmbeddingPlan {
    /// Index unit, which must be `codepoint`.
    pub index_unit: String,
    /// Ordered marker insertion operations.
    pub operations: Vec<PlanOperation>,
}

/// Parsed response from the Encypher CDN signing endpoint.
#[derive(Debug, Deserialize)]
pub struct SignResponse {
    /// Whether signing succeeded.
    pub success: bool,
    /// Marker plan when signing succeeded.
    #[serde(default)]
    pub embedding_plan: Option<EmbeddingPlan>,
    /// C2PA manifest URL when signing succeeded.
    #[serde(default)]
    pub manifest_url: Option<String>,
    /// Stable failure reason when signing did not succeed.
    #[serde(default)]
    pub error: Option<String>,
    /// Actual policy used by the server; required before a successful plan is used.
    #[serde(default)]
    pub policy_digest: Option<String>,
}

/// Fail-closed signing-response parse errors.
#[derive(Debug, PartialEq)]
pub enum ApiError {
    /// The response exceeded [`MAX_RESPONSE_BYTES`].
    TooLarge,
    /// The response was not valid JSON for [`SignResponse`].
    Malformed,
}

/// Parses a bounded signing response.
///
/// # Errors
///
/// Returns [`ApiError::TooLarge`] above the response limit and
/// [`ApiError::Malformed`] when JSON decoding fails.
pub fn parse_sign_response(body: &[u8]) -> Result<SignResponse, ApiError> {
    if body.len() > MAX_RESPONSE_BYTES {
        return Err(ApiError::TooLarge);
    }
    serde_json::from_slice(body).map_err(|_| ApiError::Malformed)
}

/// A policy digest is exactly a lowercase SHA-256 hex value.
pub fn valid_policy_digest(digest: &str) -> bool {
    digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Parse the policy endpoint's strict, bounded response.
///
/// # Errors
///
/// Rejects oversized JSON, extra or duplicate fields, and invalid digests.
pub fn parse_policy_response(body: &[u8]) -> Result<String, ApiError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct PolicyResponse {
        policy_digest: String,
    }
    if body.len() > MAX_POLICY_RESPONSE_BYTES {
        return Err(ApiError::TooLarge);
    }
    let response: PolicyResponse = serde_json::from_slice(body).map_err(|_| ApiError::Malformed)?;
    if !valid_policy_digest(&response.policy_digest) {
        return Err(ApiError::Malformed);
    }
    Ok(response.policy_digest)
}

/// Reasons an embedding plan is unsafe or cannot address the visible text.
#[derive(Debug, PartialEq)]
pub enum PlanError {
    /// The plan does not use Unicode codepoint indices.
    IndexUnit,
    /// An insertion index is outside the visible-text range.
    IndexOutOfRange,
    /// A marker is empty.
    EmptyMarker,
    /// A marker contains a character outside the edge-core invisible set.
    DisallowedMarkerChar,
    /// The plan has no operations.
    Empty,
    /// The manifest URL is present but not an absolute `https` URL.
    ManifestUrl,
}

/// Validates a signing result before use: index bounds and the invisible marker
/// alphabet for every operation, and a manifest URL that is absent or `https`.
///
/// This is the only check between the API or KV and the page, so callers must
/// pass the scalar count of the exact text that was signed.
///
/// # Errors
///
/// Returns a [`PlanError`] for the first invalid property.
pub fn validate_plan(
    plan: &EmbeddingPlan,
    manifest_url: Option<&str>,
    visible_scalars: usize,
) -> Result<(), PlanError> {
    if plan.index_unit != "codepoint" {
        return Err(PlanError::IndexUnit);
    }
    if plan.operations.is_empty() {
        return Err(PlanError::Empty);
    }
    for operation in &plan.operations {
        if operation.insert_after_index < -1 {
            return Err(PlanError::IndexOutOfRange);
        }
        if operation.insert_after_index >= 0 {
            let Ok(index) = usize::try_from(operation.insert_after_index) else {
                return Err(PlanError::IndexOutOfRange);
            };
            if index >= visible_scalars {
                return Err(PlanError::IndexOutOfRange);
            }
        }
        if operation.marker.is_empty() {
            return Err(PlanError::EmptyMarker);
        }
        if !operation.marker.chars().all(is_allowed_marker_char) {
            return Err(PlanError::DisallowedMarkerChar);
        }
    }
    if let Some(url) = manifest_url {
        let https = url::Url::parse(url).is_ok_and(|url| {
            url.scheme() == "https" && url.host_str().is_some_and(|h| !h.is_empty())
        });
        if !https {
            return Err(PlanError::ManifestUrl);
        }
    }
    Ok(())
}

fn is_allowed_marker_char(ch: char) -> bool {
    matches!(ch, '\u{fe00}'..='\u{fe0f}' | '\u{e0100}'..='\u{e01ef}' | '\u{feff}')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn single_op_plan() -> EmbeddingPlan {
        EmbeddingPlan {
            index_unit: "codepoint".to_string(),
            operations: vec![PlanOperation {
                insert_after_index: 0,
                marker: "\u{fe00}".to_string(),
            }],
        }
    }

    #[test]
    fn policy_response_requires_only_a_lowercase_sha256_digest() {
        let digest = "ab".repeat(32);
        let body = format!(r#"{{"policy_digest":"{digest}"}}"#);
        assert_eq!(parse_policy_response(body.as_bytes()), Ok(digest.clone()));
        for body in [
            "{}".to_owned(),
            r#"{"policy_digest":null}"#.to_owned(),
            r#"{"policy_digest":42}"#.to_owned(),
            format!(r#"{{"policy_digest":"{}"}}"#, digest.to_uppercase()),
            format!(r#"{{"policy_digest":"{}"}}"#, "g".repeat(64)),
            format!(r#"{{"policy_digest":"{}"}}"#, "a".repeat(63)),
            format!(r#"{{"policy_digest":"{digest}","extra":true}}"#),
            format!(r#"{{"policy_digest":"{digest}","policy_digest":"{digest}"}}"#),
            format!(r#"{{"policy_digest":"{digest}"}} trailing"#),
        ] {
            assert_eq!(
                parse_policy_response(body.as_bytes()),
                Err(ApiError::Malformed)
            );
        }
        assert_eq!(
            parse_policy_response(&vec![b' '; MAX_POLICY_RESPONSE_BYTES + 1]),
            Err(ApiError::TooLarge)
        );
    }

    #[test]
    fn parses_and_validates_a_response_plan() {
        let body = r#"{"success":true,"embedding_plan":{"index_unit":"codepoint","operations":[{"insert_after_index":0,"marker":"︀"}]},"manifest_url":"https://manifest.example.com/x"}"#;
        let response = parse_sign_response(body.as_bytes()).expect("should parse");
        let plan = response.embedding_plan.expect("should contain plan");
        assert_eq!(
            validate_plan(&plan, response.manifest_url.as_deref(), 1),
            Ok(())
        );
    }

    #[test]
    fn manifest_url_must_be_absent_or_https() {
        let plan = single_op_plan();
        assert_eq!(validate_plan(&plan, None, 1), Ok(()));
        for url in [
            "",
            "http://manifest.example.com/x",
            "javascript:alert(1)",
            "data:text/html,<script>alert(1)</script>",
            "/api/v1/public/cdn/manifest/1",
            "https://",
        ] {
            assert_eq!(
                validate_plan(&plan, Some(url), 1),
                Err(PlanError::ManifestUrl),
                "{url} should be rejected"
            );
        }
    }

    #[test]
    fn rejects_markup_and_bad_ranges() {
        let markup = EmbeddingPlan {
            index_unit: "codepoint".to_string(),
            operations: vec![PlanOperation {
                insert_after_index: 0,
                marker: "<b>".to_string(),
            }],
        };
        assert_eq!(
            validate_plan(&markup, None, 1),
            Err(PlanError::DisallowedMarkerChar)
        );
        assert_eq!(
            validate_plan(&single_op_plan(), None, 0),
            Err(PlanError::IndexOutOfRange)
        );
    }

    #[test]
    fn parses_realistic_large_marker_response() {
        let escaped_marker = r"\uDB40\uDD00".repeat(50_000);
        let body = format!(
            r#"{{"success":true,"embedding_plan":{{"index_unit":"codepoint","operations":[{{"insert_after_index":0,"marker":"{escaped_marker}"}}]}}}}"#
        );
        assert!(body.len() > 600_000);
        let response =
            parse_sign_response(body.as_bytes()).expect("large valid response should parse");
        let plan = response
            .embedding_plan
            .expect("response should contain a plan");
        assert_eq!(plan.operations[0].marker.chars().count(), 50_000);
    }

    #[test]
    fn enforces_size_and_empty_plan_limits() {
        assert_eq!(
            parse_sign_response(&vec![b' '; MAX_RESPONSE_BYTES + 1]).map(|_| ()),
            Err(ApiError::TooLarge)
        );
        let empty = EmbeddingPlan {
            index_unit: "codepoint".to_string(),
            operations: Vec::new(),
        };
        assert_eq!(validate_plan(&empty, None, 1), Err(PlanError::Empty));
    }
}
