use url::{Position, Url};

use crate::integration::{
    AttributeRewriteAction, IntegrationAttributeContext, ScriptSourceAction, ScriptSourceContext,
    ScriptSourceTransition,
};

/// Resolve source references using the request origin available to native rewriters.
///
/// The HTML context has no document path or `<base>` URL. A dotted relative
/// reference therefore remains a path on the request host, not a vendor host.
pub(super) fn parse_script_url(
    value: &str,
    context: &IntegrationAttributeContext<'_>,
) -> Option<Url> {
    resolve_script_source(value, context).and_then(|(_, parsed)| parsed)
}

fn is_http_source(url: &Url) -> bool {
    matches!(url.scheme(), "http" | "https") && url.host_str().is_some()
}

// Retain the actual origin even when this source cannot be resolved, so a pure
// rule receives the same optional parsed reference as the native bridge did.
fn resolve_script_source(
    value: &str,
    context: &IntegrationAttributeContext<'_>,
) -> Option<(Url, Option<Url>)> {
    if value.trim().is_empty() {
        return None;
    }
    let base = Url::parse(&format!(
        "{}://{}/",
        context.request_scheme, context.request_host
    ))
    .ok()?;
    let parsed = base.join(value).ok().filter(is_http_source);
    Some((base, parsed))
}

/// Apply the declared pure rule with the native request-origin resolution base.
/// Legacy attribute candidate selection stays with its owner; the source-policy
/// chain separately selects only script and script-preload subjects.
pub(super) fn rewrite_source(
    rule: &dyn ScriptSourceTransition,
    value: &str,
    context: &IntegrationAttributeContext<'_>,
) -> AttributeRewriteAction {
    let Some((base, parsed)) = resolve_script_source(value, context) else {
        return AttributeRewriteAction::Keep;
    };
    let source = ScriptSourceContext {
        element_name: context.element_name,
        attribute_name: context.attribute_name,
        source: value,
        parsed_url: parsed.as_ref(),
        base_url: &base,
        rel: "",
        as_token: "",
    };
    match rule.apply_script_source(&source) {
        ScriptSourceAction::Unchanged => AttributeRewriteAction::Keep,
        ScriptSourceAction::Replace(value) => AttributeRewriteAction::Replace(value),
        ScriptSourceAction::Remove => AttributeRewriteAction::RemoveElement,
    }
}

/// Borrow only URL references accepted by the existing native ownership parser.
pub(super) fn http_source_url<'a>(context: &'a ScriptSourceContext<'_>) -> Option<&'a Url> {
    context.parsed_url.filter(|url| is_http_source(url))
}

/// Keep the parsed path, query and fragment together when proxying a source.
pub(super) fn path_and_suffix(url: &Url) -> &str {
    &url[Position::BeforePath..]
}

#[cfg(test)]
pub(super) mod tests {
    use std::collections::BTreeMap;

    use serde::Deserialize;
    use url::Url;

    use crate::integration::{
        AttributeRewriteAction, IntegrationAttributeContext, IntegrationAttributeRewriter,
        ScriptSourceAction, ScriptSourceClaim, ScriptSourceContext, evaluate_script_sources,
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

    pub(crate) fn assert_claim_ownership(owner: &str, claim: &ScriptSourceClaim) {
        let cases: Vec<OwnershipCase> = serde_json::from_str(include_str!(
            "../../../trusted-server-js/lib/test/fixtures/script-url-ownership.json"
        ))
        .expect("should deserialize script URL ownership corpus");
        for case in cases {
            let expected = match case.native.get(owner).map(String::as_str) {
                Some("REMOVE") => ScriptSourceAction::Remove,
                Some(value) => ScriptSourceAction::Replace(value.to_owned()),
                None => ScriptSourceAction::Unchanged,
            };
            let native = IntegrationAttributeContext {
                request_host: case.request_host.as_deref().unwrap_or("edge.example.com"),
                request_scheme: case.request_scheme.as_deref().unwrap_or("https"),
                ..context()
            };
            let base = Url::parse(&format!(
                "{}://{}/",
                native.request_scheme, native.request_host
            ))
            .expect("should parse actual request origin");
            let parsed = super::parse_script_url(&case.url, &native);
            for (element_name, attribute_name, rel, as_token) in [
                ("script", "src", "", ""),
                ("link", "href", "preload", "script"),
            ] {
                let input = ScriptSourceContext {
                    element_name,
                    attribute_name,
                    source: &case.url,
                    parsed_url: parsed.as_ref(),
                    base_url: &base,
                    rel,
                    as_token,
                };
                assert_eq!(
                    evaluate_script_sources(&input, std::slice::from_ref(claim)).action,
                    expected,
                    "should share declaration ownership for {owner}, case {}, {element_name}",
                    case.id
                );
            }
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
