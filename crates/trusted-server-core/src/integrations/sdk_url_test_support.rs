//! Shared SDK URL corpus and attribute-rewrite assertions.

use serde::Deserialize;

use super::{AttributeRewriteAction, IntegrationAttributeContext, IntegrationAttributeRewriter};

/// One raw URL and its expected integration owner from the shared corpus.
#[derive(Deserialize)]
pub(super) struct SdkUrlCase {
    /// Descriptive case name for assertion failures.
    pub(super) name: String,
    /// Input before browser DOM URL resolution.
    pub(super) url: String,
    /// Expected integration ID, or none for an unowned URL.
    pub(super) owner: Option<String>,
}

/// Load the URL ownership cases used by both Rust and browser tests.
///
/// # Panics
///
/// Panics if the committed fixture is invalid.
pub(super) fn sdk_url_cases() -> Vec<SdkUrlCase> {
    serde_json::from_str(include_str!(
        "../../../trusted-server-js/lib/test/fixtures/sdk-url-ownership.json"
    ))
    .expect("should parse shared SDK URL ownership corpus")
}

/// Assert SDK rewriting for both script and link attribute contexts.
///
/// # Panics
///
/// Panics if the rewrite result disagrees with the corpus owner and SDK route.
pub(super) fn assert_sdk_url_rewrite(
    integration: &dyn IntegrationAttributeRewriter,
    case: &SdkUrlCase,
    sdk_path: &str,
) {
    let expected = if case.owner.as_deref() == Some(integration.integration_id()) {
        AttributeRewriteAction::Replace(sdk_path.to_string())
    } else {
        AttributeRewriteAction::Keep
    };
    for (attribute, element) in [("src", "script"), ("href", "link")] {
        let context = IntegrationAttributeContext {
            attribute_name: attribute,
            element_name: element,
            request_host: "edge.example.com",
            request_scheme: "https",
            origin_host: "origin.example.com",
        };
        assert_eq!(
            integration.rewrite(attribute, &case.url, &context),
            expected,
            "should apply expected {} {attribute} rewrite for {}: {}",
            integration.integration_id(),
            case.name,
            case.url
        );
    }
}
