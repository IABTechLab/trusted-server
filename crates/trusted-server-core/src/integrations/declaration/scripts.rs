use std::fmt;
use std::sync::Arc;

use error_stack::Report;
use serde::Serialize;
use url::Url;

use crate::error::TrustedServerError;

/// A script-bearing element checked by the source policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ScriptSourceKind {
    /// The `src` attribute on a script element.
    Script,
    /// The `href` attribute on a script preload link.
    Preload,
}

/// Immutable element facts and the current source during an ordered transition.
///
/// `base_url` is the caller's actual resolution base. Native HTML processing uses
/// its request origin; a browser caller can supply its real document base.
/// Replacements are resolved against that base, never the previous vendor URL.
pub struct ScriptSourceContext<'a> {
    /// HTML element name.
    pub element_name: &'a str,
    /// Attribute being processed.
    pub attribute_name: &'a str,
    /// Current exact source reference, including any preceding replacement.
    pub source: &'a str,
    /// Parsed current reference, absent when URL resolution fails.
    pub parsed_url: Option<&'a Url>,
    /// Actual resolution base supplied by the caller.
    pub base_url: &'a Url,
    /// Space-separated `rel` tokens from the original element.
    pub rel: &'a str,
    /// The original element's `as` token.
    pub as_token: &'a str,
}

impl ScriptSourceContext<'_> {
    /// Identify only a script source or an explicitly script-typed preload.
    ///
    /// # Examples
    ///
    /// ```
    /// use trusted_server_core::integrations::{ScriptSourceContext, ScriptSourceKind};
    /// use url::Url;
    /// let base = Url::parse("https://example.com/").expect("should parse base");
    /// let context = ScriptSourceContext { element_name: "link", attribute_name: "href",
    ///     source: "/asset.js", parsed_url: None, base_url: &base, rel: "preload", as_token: "script" };
    /// assert_eq!(context.kind(), Some(ScriptSourceKind::Preload));
    /// ```
    #[must_use]
    pub fn kind(&self) -> Option<ScriptSourceKind> {
        if self.element_name.eq_ignore_ascii_case("script")
            && self.attribute_name.eq_ignore_ascii_case("src")
        {
            Some(ScriptSourceKind::Script)
        } else if self.element_name.eq_ignore_ascii_case("link")
            && self.attribute_name.eq_ignore_ascii_case("href")
            && self
                .rel
                .split_ascii_whitespace()
                .any(|token| token.eq_ignore_ascii_case("preload"))
            && self.as_token.eq_ignore_ascii_case("script")
        {
            Some(ScriptSourceKind::Preload)
        } else {
            None
        }
    }
}

impl fmt::Debug for ScriptSourceContext<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScriptSourceContext")
            .field("kind", &self.kind())
            .finish_non_exhaustive()
    }
}

/// One pure source transition. Replacement values are never debug-printed.
#[derive(Clone, PartialEq, Eq)]
pub enum ScriptSourceAction {
    /// Keep the current source.
    Unchanged,
    /// Pass this exact replacement reference to the following claims.
    Replace(String),
    /// Remove the element and stop ordinary source transitions.
    Remove,
}

impl fmt::Debug for ScriptSourceAction {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unchanged => formatter.write_str("Unchanged"),
            Self::Replace(_) => formatter.write_str("Replace(<redacted>)"),
            Self::Remove => formatter.write_str("Remove"),
        }
    }
}

/// A stateless, unresolved-secret source rule shared with its executable owner.
///
/// Implementations must not resolve secrets, allocate clients or perform I/O.
/// Native/proxy execution calls this same rule rather than copying its matcher.
pub trait ScriptSourceTransition: Send + Sync {
    /// Apply the owner's rule to the current source and original element facts.
    ///
    /// # Examples
    ///
    /// ```
    /// use trusted_server_core::integrations::{ScriptSourceAction, ScriptSourceContext, ScriptSourceTransition};
    /// use url::Url;
    /// struct Keep;
    /// impl ScriptSourceTransition for Keep {
    ///     fn apply_script_source(&self, _: &ScriptSourceContext<'_>) -> ScriptSourceAction {
    ///         ScriptSourceAction::Unchanged
    ///     }
    /// }
    /// let base = Url::parse("https://example.com/").expect("should parse actual base");
    /// let context = ScriptSourceContext { element_name: "script", attribute_name: "src",
    ///     source: "/asset.js", parsed_url: None, base_url: &base, rel: "", as_token: "" };
    /// assert_eq!(Keep.apply_script_source(&context), ScriptSourceAction::Unchanged,
    ///     "should preserve the current source without side effects");
    /// ```
    fn apply_script_source(&self, context: &ScriptSourceContext<'_>) -> ScriptSourceAction;
}

/// One integration-owned transition and its non-value-bearing source location.
#[derive(Clone)]
pub struct ScriptSourceClaim {
    /// Static catalog integration identifier.
    pub integration_id: &'static str,
    /// Static schema field name, never a configured URL or route path.
    pub source_field: &'static str,
    transition: Arc<dyn ScriptSourceTransition>,
}

impl ScriptSourceClaim {
    /// Bind one pure rule to its catalog owner and schema field.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::sync::Arc;
    /// use trusted_server_core::integrations::{ScriptSourceAction, ScriptSourceClaim,
    ///     ScriptSourceContext, ScriptSourceTransition};
    /// struct Keep;
    /// impl ScriptSourceTransition for Keep {
    ///     fn apply_script_source(&self, _: &ScriptSourceContext<'_>) -> ScriptSourceAction { ScriptSourceAction::Unchanged }
    /// }
    /// let claim = ScriptSourceClaim::new("example", "script_url", Arc::new(Keep));
    /// ```
    #[must_use]
    pub fn new(
        integration_id: &'static str,
        source_field: &'static str,
        transition: Arc<dyn ScriptSourceTransition>,
    ) -> Self {
        Self {
            integration_id,
            source_field,
            transition,
        }
    }
}

impl fmt::Debug for ScriptSourceClaim {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScriptSourceClaim")
            .field("integration_id", &self.integration_id)
            .field("source_field", &self.source_field)
            .finish_non_exhaustive()
    }
}

/// A value-free action suitable for diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ScriptSourceActionKind {
    /// No source mutation.
    Unchanged,
    /// Source replaced, without recording the value.
    Replace,
    /// Element removed.
    Remove,
}

impl From<&ScriptSourceAction> for ScriptSourceActionKind {
    fn from(action: &ScriptSourceAction) -> Self {
        match action {
            ScriptSourceAction::Unchanged => Self::Unchanged,
            ScriptSourceAction::Replace(_) => Self::Replace,
            ScriptSourceAction::Remove => Self::Remove,
        }
    }
}

/// One non-disclosing step in the ordered claim chain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScriptSourceStep {
    /// Participating catalog owner.
    pub integration_id: &'static str,
    /// Participating schema field.
    pub source_field: &'static str,
    /// Action without any URL value.
    pub action: ScriptSourceActionKind,
}

/// The exact ordinary outcome and a separate value-free diagnostic trace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptSourceOutcome {
    /// Final ordinary source mutation. `Remove` is terminal.
    pub action: ScriptSourceAction,
    /// Steps in original registration order, ending at a terminal removal.
    pub steps: Vec<ScriptSourceStep>,
}

/// Evaluate source claims in order while retaining the original element context.
///
/// An unrelated attribute is not a script-policy subject. A removal terminates
/// the ordinary chain; later rules cannot resurrect a removed element.
///
/// # Examples
///
/// ```
/// use trusted_server_core::integrations::{evaluate_script_sources, ScriptSourceAction, ScriptSourceContext};
/// use url::Url;
/// let base = Url::parse("https://example.com/").expect("should parse base");
/// let context = ScriptSourceContext { element_name: "script", attribute_name: "src",
///     source: "/asset.js", parsed_url: None, base_url: &base, rel: "", as_token: "" };
/// assert_eq!(evaluate_script_sources(&context, &[]).action, ScriptSourceAction::Unchanged);
/// ```
#[must_use]
pub fn evaluate_script_sources(
    context: &ScriptSourceContext<'_>,
    claims: &[ScriptSourceClaim],
) -> ScriptSourceOutcome {
    let mut outcome = ScriptSourceOutcome {
        action: ScriptSourceAction::Unchanged,
        steps: Vec::new(),
    };
    if context.kind().is_none() {
        return outcome;
    }
    let mut replacement = None::<String>;
    for claim in claims {
        let source = replacement.as_deref().unwrap_or(context.source);
        let resolved;
        let parsed_url = if replacement.is_some() {
            resolved = context.base_url.join(source).ok();
            resolved.as_ref()
        } else {
            context.parsed_url
        };
        let current = ScriptSourceContext {
            source,
            parsed_url,
            ..*context
        };
        let action = claim.transition.apply_script_source(&current);
        outcome.steps.push(ScriptSourceStep {
            integration_id: claim.integration_id,
            source_field: claim.source_field,
            action: ScriptSourceActionKind::from(&action),
        });
        match action {
            ScriptSourceAction::Unchanged => {}
            ScriptSourceAction::Replace(value) => replacement = Some(value),
            ScriptSourceAction::Remove => {
                outcome.action = ScriptSourceAction::Remove;
                return outcome;
            }
        }
    }
    if let Some(value) = replacement {
        outcome.action = ScriptSourceAction::Replace(value);
    }
    outcome
}

/// Required final behavior of one configured operator asset.
///
/// Exact paths remain internal policy values and are never formatted in errors.
#[derive(Clone, PartialEq, Eq)]
pub enum ScriptAssetPolicy {
    /// Retain the definition without imposing a source outcome.
    Disabled,
    /// The complete chain must end at this exact first-party reference.
    Enabled { path: String },
    /// The complete chain must remove the element.
    Blocked,
}

impl fmt::Debug for ScriptAssetPolicy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Disabled => "Disabled",
            Self::Enabled { .. } => "Enabled(<redacted>)",
            Self::Blocked => "Blocked",
        })
    }
}

/// Validate an operator asset against the complete enabled source chain.
///
/// Schema-one callers do not impose this schema-two operator policy. Disabled
/// definitions impose no outcome; an earlier compatible removal satisfies a
/// blocked definition. This function does not apply the later runtime drift clamp.
///
/// # Errors
///
/// Returns a configuration error on a final-policy mismatch. It reports only
/// field/index, owner IDs, element kind and action kinds, never source values.
///
/// # Examples
///
/// ```
/// use trusted_server_core::integrations::{validate_script_asset_policy, ScriptAssetPolicy, ScriptSourceContext};
/// use url::Url;
/// let base = Url::parse("https://example.com/").expect("should parse base");
/// let context = ScriptSourceContext { element_name: "script", attribute_name: "src",
///     source: "/asset.js", parsed_url: None, base_url: &base, rel: "", as_token: "" };
/// validate_script_asset_policy(&context, &[], &ScriptAssetPolicy::Disabled, "assets", 0)
///     .expect("should impose no policy for disabled asset");
/// ```
pub fn validate_script_asset_policy(
    context: &ScriptSourceContext<'_>,
    claims: &[ScriptSourceClaim],
    policy: &ScriptAssetPolicy,
    source_field: &'static str,
    asset_index: usize,
) -> Result<ScriptSourceOutcome, Report<TrustedServerError>> {
    let outcome = evaluate_script_sources(context, claims);
    let accepted = match policy {
        ScriptAssetPolicy::Disabled => true,
        ScriptAssetPolicy::Enabled { path } => match &outcome.action {
            ScriptSourceAction::Unchanged => context.source == path,
            ScriptSourceAction::Replace(value) => value == path,
            ScriptSourceAction::Remove => false,
        },
        ScriptAssetPolicy::Blocked => matches!(outcome.action, ScriptSourceAction::Remove),
    };
    if !accepted {
        let steps = outcome
            .steps
            .iter()
            .map(|step| {
                format!(
                    "{}:{}={:?}",
                    step.integration_id, step.source_field, step.action
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        return Err(Report::new(TrustedServerError::Configuration {
            message: format!(
                "Script-source policy conflict at {source_field}[{asset_index}] for {:?}: [{steps}]; final {:?}",
                context.kind(),
                ScriptSourceActionKind::from(&outcome.action)
            ),
        }));
    }
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    struct Replace(&'static str);
    impl ScriptSourceTransition for Replace {
        fn apply_script_source(&self, _: &ScriptSourceContext<'_>) -> ScriptSourceAction {
            ScriptSourceAction::Replace(self.0.to_owned())
        }
    }
    struct Remove;
    impl ScriptSourceTransition for Remove {
        fn apply_script_source(&self, _: &ScriptSourceContext<'_>) -> ScriptSourceAction {
            ScriptSourceAction::Remove
        }
    }
    struct Observe {
        calls: Arc<AtomicUsize>,
    }
    impl ScriptSourceTransition for Observe {
        fn apply_script_source(&self, context: &ScriptSourceContext<'_>) -> ScriptSourceAction {
            self.calls.fetch_add(1, Ordering::Relaxed);
            assert_eq!(
                context.source, "/first-party.js?credential-sentinel",
                "should receive exact previous replacement"
            );
            assert_eq!(
                context.parsed_url.and_then(Url::host_str),
                Some("example.com"),
                "should resolve relative replacement against publisher base"
            );
            assert_eq!(context.rel, "alternate PRELOAD", "should retain rel tokens");
            assert_eq!(context.as_token, "script", "should retain as token");
            ScriptSourceAction::Unchanged
        }
    }

    fn context<'a>(base: &'a Url, source: &'a Url) -> ScriptSourceContext<'a> {
        ScriptSourceContext {
            element_name: "script",
            attribute_name: "src",
            source: source.as_str(),
            parsed_url: Some(source),
            base_url: base,
            rel: "alternate PRELOAD",
            as_token: "script",
        }
    }

    #[test]
    fn replacements_flow_in_order_without_losing_element_context() {
        let base = Url::parse("https://example.com/").expect("should parse publisher base");
        let original = Url::parse("https://vendor.example/credential-sentinel.js")
            .expect("should parse original");
        let calls = Arc::new(AtomicUsize::new(0));
        let claims = [
            ScriptSourceClaim::new(
                "first",
                "url",
                Arc::new(Replace("/first-party.js?credential-sentinel")),
            ),
            ScriptSourceClaim::new(
                "second",
                "url",
                Arc::new(Observe {
                    calls: Arc::clone(&calls),
                }),
            ),
        ];
        let outcome = evaluate_script_sources(&context(&base, &original), &claims);
        assert_eq!(
            outcome.action,
            ScriptSourceAction::Replace("/first-party.js?credential-sentinel".to_owned()),
            "should preserve final exact replacement"
        );
        assert_eq!(
            calls.load(Ordering::Relaxed),
            1,
            "should call later claim once"
        );
        assert!(
            !format!("{outcome:?}").contains("credential-sentinel"),
            "should redact source values from debug output"
        );
    }

    #[test]
    fn removal_is_terminal_and_satisfies_blocked_policy() {
        let base = Url::parse("https://example.com/").expect("should parse base");
        let original =
            Url::parse("https://vendor.example/asset.js").expect("should parse original");
        let calls = Arc::new(AtomicUsize::new(0));
        let claims = [
            ScriptSourceClaim::new("remove", "url", Arc::new(Remove)),
            ScriptSourceClaim::new(
                "later",
                "url",
                Arc::new(Observe {
                    calls: Arc::clone(&calls),
                }),
            ),
        ];
        let outcome = validate_script_asset_policy(
            &context(&base, &original),
            &claims,
            &ScriptAssetPolicy::Blocked,
            "assets",
            0,
        )
        .expect("should accept compatible removal");
        assert_eq!(
            outcome.action,
            ScriptSourceAction::Remove,
            "should preserve terminal removal"
        );
        assert_eq!(
            calls.load(Ordering::Relaxed),
            0,
            "should not resurrect removed element"
        );
    }

    #[test]
    fn non_script_attributes_never_run_script_claims() {
        let base = Url::parse("https://example.com/").expect("should parse base");
        let original =
            Url::parse("https://vendor.example/asset.js").expect("should parse original");
        let claims = [ScriptSourceClaim::new("remove", "url", Arc::new(Remove))];
        for (element_name, attribute_name, rel, as_token) in [
            ("img", "src", "", ""),
            ("link", "href", "stylesheet", ""),
            ("link", "href", "preload", "style"),
            ("a", "href", "preload", "script"),
        ] {
            let input = ScriptSourceContext {
                element_name,
                attribute_name,
                rel,
                as_token,
                ..context(&base, &original)
            };
            let outcome = evaluate_script_sources(&input, &claims);
            assert_eq!(
                outcome.action,
                ScriptSourceAction::Unchanged,
                "should ignore unrelated element context"
            );
            assert!(
                outcome.steps.is_empty(),
                "should not execute source claims for unrelated attributes"
            );
        }
        let preload = ScriptSourceContext {
            element_name: "link",
            attribute_name: "href",
            ..context(&base, &original)
        };
        assert_eq!(
            evaluate_script_sources(&preload, &claims).action,
            ScriptSourceAction::Remove,
            "should recognize script preload tokens"
        );
    }

    #[test]
    fn policy_checks_final_outcome_not_an_intermediate_match() {
        let base = Url::parse("https://example.com/").expect("should parse base");
        let original = Url::parse("https://user-credential-sentinel@vendor.example/path-credential-sentinel?q=query-credential-sentinel").expect("should parse credential fixture");
        let claims = [
            ScriptSourceClaim::new(
                "asset-owner",
                "assets",
                Arc::new(Replace("/configured-credential-sentinel")),
            ),
            ScriptSourceClaim::new(
                "later-owner",
                "url",
                Arc::new(Replace(
                    "https://replacement-credential-sentinel@example.com/final-credential-sentinel",
                )),
            ),
        ];
        let input = context(&base, &original);
        let error = validate_script_asset_policy(
            &input,
            &claims,
            &ScriptAssetPolicy::Enabled {
                path: "/configured-credential-sentinel".to_owned(),
            },
            "assets",
            7,
        )
        .expect_err("should reject later incompatible replacement");
        let diagnostic = format!("{error:?}");
        assert!(
            diagnostic.contains("assets[7]")
                && diagnostic.contains("asset-owner")
                && diagnostic.contains("later-owner"),
            "should identify source location and owners"
        );
        assert!(diagnostic.contains("Replace"), "should report action kinds");
        for value in ["credential-sentinel", "vendor.example", "example.com"] {
            assert!(
                !diagnostic.contains(value),
                "should omit original, replacement and configured URL values"
            );
        }
        assert!(
            !format!("{input:?}").contains("credential-sentinel"),
            "should not print context URL values"
        );
        validate_script_asset_policy(&input, &claims, &ScriptAssetPolicy::Disabled, "assets", 7)
            .expect("should not impose a disabled asset outcome");
    }
}
