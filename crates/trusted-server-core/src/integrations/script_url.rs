use url::{Position, Url};

use crate::integrations::IntegrationAttributeContext;

/// Resolve source references using the request origin available to native rewriters.
///
/// The HTML context has no document path or `<base>` URL. A dotted relative
/// reference therefore remains a path on the request host, not a vendor host.
pub(super) fn parse_script_url(
    value: &str,
    context: &IntegrationAttributeContext<'_>,
) -> Option<Url> {
    if value.trim().is_empty() {
        return None;
    }
    let base = Url::parse(&format!(
        "{}://{}/",
        context.request_scheme, context.request_host
    ))
    .ok()?;
    let parsed = base.join(value).ok()?;
    (matches!(parsed.scheme(), "http" | "https") && parsed.host_str().is_some()).then_some(parsed)
}

/// Keep the parsed path, query and fragment together when proxying a source.
pub(super) fn path_and_suffix(url: &Url) -> &str {
    &url[Position::BeforePath..]
}

#[cfg(test)]
pub(super) mod tests {
    use std::collections::BTreeMap;

    use serde::Deserialize;

    use crate::integrations::{
        AttributeRewriteAction, IntegrationAttributeContext, IntegrationAttributeRewriter,
    };

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct OwnershipCase {
        id: String,
        url: String,
        native: BTreeMap<String, String>,
        request_host: Option<String>,
        request_scheme: Option<String>,
    }

    pub(crate) fn context() -> IntegrationAttributeContext<'static> {
        IntegrationAttributeContext {
            attribute_name: "src",
            element_name: "script",
            request_host: "edge.example.com",
            request_scheme: "https",
            origin_host: "origin.example.com",
        }
    }

    pub(crate) fn assert_ownership(owner: &str, rewriter: &dyn IntegrationAttributeRewriter) {
        let cases: Vec<OwnershipCase> = serde_json::from_str(include_str!(
            "../../../trusted-server-js/lib/test/fixtures/script-url-ownership.json"
        ))
        .expect("should deserialize script URL ownership corpus");
        for case in cases {
            let expected = match case.native.get(owner).map(String::as_str) {
                Some("REMOVE") => AttributeRewriteAction::RemoveElement,
                Some(value) => AttributeRewriteAction::Replace(value.to_owned()),
                None => AttributeRewriteAction::Keep,
            };
            for (element_name, attribute_name) in [("script", "src"), ("link", "href")] {
                let context = IntegrationAttributeContext {
                    attribute_name,
                    element_name,
                    request_host: case.request_host.as_deref().unwrap_or("edge.example.com"),
                    request_scheme: case.request_scheme.as_deref().unwrap_or("https"),
                    origin_host: "origin.example.com",
                };
                assert_eq!(
                    rewriter.rewrite(attribute_name, &case.url, &context),
                    expected,
                    "should apply parsed ownership for {owner}, case {}, {element_name}",
                    case.id,
                );
            }
        }
    }
}
