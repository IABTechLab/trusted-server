//! Query-activated, browser-session GPT runtime diagnostics integration.
//!
//! Deployment configuration makes the standalone browser module available.
//! Exact `ts_console` directives establish or clear a host-only session cookie;
//! active documents load the module synchronously without adding diagnostics to
//! the ordinary unified bundle.

use error_stack::{Report, ResultExt};
use http::{HeaderValue, Method, Request, Response, Uri, header, uri::PathAndQuery};
use serde::Deserialize;
use validator::Validate;

use edgezero_core::body::Body as EdgeBody;

use crate::error::TrustedServerError;
use crate::http_util::is_navigation_request;
use crate::response_privacy::enforce_synthesized_html_cache_privacy;
use crate::settings::{IntegrationConfig, Settings};
use crate::tsjs;

use super::IntegrationRegistration;

/// Stable integration identifier.
pub const GPT_DIAGNOSTICS_INTEGRATION_ID: &str = "gpt_diagnostics";
/// Reserved activation query parameter.
pub const GPT_DIAGNOSTICS_QUERY: &str = "ts_console";
/// Host-only browser-session activation cookie.
pub const GPT_DIAGNOSTICS_COOKIE: &str = "__Host-ts-console";

const SET_CONSOLE_COOKIE: &str = "__Host-ts-console=1; Path=/; Secure; HttpOnly; SameSite=Lax";
const CLEAR_CONSOLE_COOKIE: &str =
    "__Host-ts-console=; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age=0";

/// Configuration for the GPT runtime diagnostics integration.
#[derive(Debug, Clone, Deserialize, Validate)]
#[serde(deny_unknown_fields)]
pub struct GptDiagnosticsConfig {
    /// Whether the GPT diagnostics browser module is available.
    #[serde(default)]
    pub enabled: bool,
}

impl IntegrationConfig for GptDiagnosticsConfig {
    fn is_enabled(&self) -> bool {
        self.enabled
    }
}

/// Cookie mutation requested by an activation directive.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GptDiagnosticsCookieAction {
    /// Do not mutate the activation cookie.
    #[default]
    None,
    /// Establish a host-only browser-session activation cookie.
    SetSession,
    /// Clear the activation cookie.
    ClearSession,
}

/// Immutable request-scoped diagnostics decision.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GptDiagnosticsRequestDecision {
    active: bool,
    clean_browser_path_and_query: Option<String>,
    cookie_action: GptDiagnosticsCookieAction,
}

impl GptDiagnosticsRequestDecision {
    /// Whether this document should install diagnostics.
    #[must_use]
    pub fn active(&self) -> bool {
        self.active
    }

    /// Whether the response must be private and non-storeable.
    #[must_use]
    pub fn requires_private_no_store(&self) -> bool {
        self.active
            || self.cookie_action != GptDiagnosticsCookieAction::None
            || self.clean_browser_path_and_query.is_some()
    }

    /// Build the early activation/URL-cleanup bootstrap for an HTML document.
    #[must_use]
    pub fn bootstrap_script(&self) -> Option<String> {
        if !self.active && self.clean_browser_path_and_query.is_none() {
            return None;
        }

        let mut script = String::from("<script>");
        if self.active {
            script.push_str("window.__tsjs_gpt_diagnostics_active=true;");
        }
        if let Some(clean_path) = &self.clean_browser_path_and_query {
            let encoded = serde_json::to_string(clean_path).ok()?;
            script.push_str("history.replaceState(history.state,'',");
            script.push_str(&encoded);
            script.push_str("+location.hash);");
        }
        script.push_str("</script>");
        Some(script)
    }

    /// Build the synchronous standalone diagnostics module tag.
    #[must_use]
    pub fn module_script_tag(&self) -> Option<String> {
        self.active.then(|| {
            format!(
                "<script src=\"{}\"></script>",
                tsjs::tsjs_single_module_script_src(GPT_DIAGNOSTICS_INTEGRATION_ID)
            )
        })
    }
    /// An active decision, for tests in other modules that need one.
    ///
    /// The fields are private and built by `prepare_request` from a cookie or query
    /// parameter; there is no other way to obtain an active decision across a module
    /// boundary.
    #[cfg(test)]
    pub(crate) fn active_for_tests() -> Self {
        Self {
            active: true,
            clean_browser_path_and_query: None,
            cookie_action: GptDiagnosticsCookieAction::None,
        }
    }
}

#[cfg(test)]
mod head_seam_invariant_tests {
    use super::*;

    /// Every combination of the three fields the decision carries.
    fn all_decisions() -> Vec<GptDiagnosticsRequestDecision> {
        let mut out = Vec::new();
        for active in [false, true] {
            for clean in [None, Some("/clean".to_string())] {
                for cookie_action in [
                    GptDiagnosticsCookieAction::None,
                    GptDiagnosticsCookieAction::SetSession,
                    GptDiagnosticsCookieAction::ClearSession,
                ] {
                    out.push(GptDiagnosticsRequestDecision {
                        active,
                        clean_browser_path_and_query: clean.clone(),
                        cookie_action,
                    });
                }
            }
        }
        out
    }

    #[test]
    fn requires_private_no_store_is_a_superset_of_injection() {
        // Load-bearing relationship, not an incidental one. Whenever this decision
        // injects anything into `<head>`, the response must also be stamped
        // `private, no-store` — which is what keeps request-scoped diagnostics out
        // of a shared cache if the explicit assembly-mode gate in
        // `create_html_stream_processor` is ever removed or bypassed.
        //
        // If a future change makes a script emit without also requiring the stamp,
        // this fails here rather than silently in a cached template.
        for decision in all_decisions() {
            let injects =
                decision.bootstrap_script().is_some() || decision.module_script_tag().is_some();
            if injects {
                assert!(
                    decision.requires_private_no_store(),
                    "decision injects into <head> but does not require private/no-store: \
                     {decision:?}"
                );
            }
        }
    }

    #[test]
    fn a_default_decision_injects_nothing() {
        let decision = GptDiagnosticsRequestDecision::default();
        assert_eq!(
            decision.bootstrap_script(),
            None,
            "should not inject a bootstrap for an inert decision"
        );
        assert_eq!(
            decision.module_script_tag(),
            None,
            "should not inject a module for an inert decision"
        );
        assert!(
            !decision.requires_private_no_store(),
            "an inert decision should not force the response private"
        );
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum QueryDirective {
    Absent,
    Enable,
    Disable,
    Invalid,
}

#[derive(Clone, Copy, Debug, Default)]
struct ConsoleCookieState {
    occurrences: usize,
    canonical: bool,
}

/// Register GPT diagnostics when explicitly enabled.
///
/// # Errors
///
/// Returns an error when the integration configuration cannot be parsed or
/// fails validation.
pub fn register(
    settings: &Settings,
) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
    let Some(_config) =
        settings.integration_config::<GptDiagnosticsConfig>(GPT_DIAGNOSTICS_INTEGRATION_ID)?
    else {
        return Ok(None);
    };

    Ok(Some(
        IntegrationRegistration::builder(GPT_DIAGNOSTICS_INTEGRATION_ID)
            .without_js()
            .build(),
    ))
}

/// Whether the diagnostics integration is present and enabled in configuration.
///
/// This is the deployment-level switch, not the per-document activation state:
/// callers that only need to know whether diagnostics could consume a value use
/// this, while document behaviour uses [`GptDiagnosticsRequestDecision::active`].
/// A configuration that cannot be parsed reads as disabled.
#[must_use]
pub fn is_enabled(settings: &Settings) -> bool {
    settings
        .integration_config::<GptDiagnosticsConfig>(GPT_DIAGNOSTICS_INTEGRATION_ID)
        .ok()
        .flatten()
        .is_some()
}

/// Evaluate activation and sanitize the request before generic cookie handling.
///
/// The reserved query and cookie are always removed before the request reaches
/// publisher, auction, or origin logic. Activation directives apply only to
/// eligible GET document navigations. Duplicate/invalid directives and duplicate
/// cookies fail closed for the current response.
///
/// # Errors
///
/// Returns an error when configuration parsing or URI reconstruction fails.
pub fn prepare_request(
    settings: &Settings,
    request: &mut Request<EdgeBody>,
) -> Result<GptDiagnosticsRequestDecision, Report<TrustedServerError>> {
    if let Some(existing) = request.extensions().get::<GptDiagnosticsRequestDecision>() {
        return Ok(existing.clone());
    }

    let integration_enabled = settings
        .integration_config::<GptDiagnosticsConfig>(GPT_DIAGNOSTICS_INTEGRATION_ID)?
        .is_some();
    let (directive, clean_path, had_reserved_query) = console_query(request.uri());
    let cookie_state = console_cookie_state(request);
    let eligible_navigation = request.method() == Method::GET
        && is_navigation_request(request)
        && !crate::publisher::is_prefetch_request(request)
        && !crate::publisher::is_bot_user_agent(request);

    sanitize_console_cookie(request);
    if had_reserved_query {
        replace_path_and_query(request, &clean_path)?;
    }

    let mut decision = GptDiagnosticsRequestDecision::default();
    if integration_enabled && eligible_navigation && had_reserved_query {
        decision.clean_browser_path_and_query = Some(clean_path);
        match directive {
            QueryDirective::Enable => {
                decision.active = true;
                decision.cookie_action = GptDiagnosticsCookieAction::SetSession;
            }
            QueryDirective::Disable => {
                decision.cookie_action = GptDiagnosticsCookieAction::ClearSession;
            }
            QueryDirective::Invalid | QueryDirective::Absent => {}
        }
    } else if integration_enabled
        && directive == QueryDirective::Absent
        && cookie_state.occurrences == 1
        && cookie_state.canonical
        && eligible_navigation
    {
        decision.active = true;
    }

    request.extensions_mut().insert(decision.clone());
    Ok(decision)
}

/// Read the request decision, defaulting to inactive when not prepared.
#[must_use]
pub fn request_decision(request: &Request<EdgeBody>) -> GptDiagnosticsRequestDecision {
    request
        .extensions()
        .get::<GptDiagnosticsRequestDecision>()
        .cloned()
        .unwrap_or_default()
}

/// Apply activation cookie and strict cache privacy to an origin response.
pub fn finalize_response(
    decision: &GptDiagnosticsRequestDecision,
    response: &mut Response<EdgeBody>,
) {
    let cookie = match decision.cookie_action {
        GptDiagnosticsCookieAction::None => None,
        GptDiagnosticsCookieAction::SetSession => {
            Some(HeaderValue::from_static(SET_CONSOLE_COOKIE))
        }
        GptDiagnosticsCookieAction::ClearSession => {
            Some(HeaderValue::from_static(CLEAR_CONSOLE_COOKIE))
        }
    };
    if let Some(cookie) = cookie {
        response.headers_mut().append(header::SET_COOKIE, cookie);
    }

    if decision.requires_private_no_store() {
        // Marks the response terminal-private as well as stamping it. Stamping alone
        // left the policy at the mercy of whatever ran later: a late
        // `RequestFilterEffects` mutation such as `Cache-Control: public` replaced it,
        // and the adapter's terminal guard had no marker to re-enforce from, so
        // request-scoped diagnostics HTML became shared-cacheable.
        enforce_synthesized_html_cache_privacy(response);
    }
}

fn console_query(uri: &Uri) -> (QueryDirective, String, bool) {
    let mut console_values = Vec::new();
    let mut retained = Vec::new();
    for pair in uri.query().unwrap_or_default().split('&') {
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        if name == GPT_DIAGNOSTICS_QUERY {
            console_values.push(value);
        } else {
            retained.push(pair);
        }
    }

    let directive = match console_values.as_slice() {
        [] => QueryDirective::Absent,
        ["true" | "1"] => QueryDirective::Enable,
        ["false" | "0"] => QueryDirective::Disable,
        _ => QueryDirective::Invalid,
    };
    let mut clean = uri.path().to_owned();
    let retained_query = retained.join("&");
    if !retained_query.is_empty() {
        clean.push('?');
        clean.push_str(&retained_query);
    }
    (directive, clean, !console_values.is_empty())
}

fn console_cookie_state(request: &Request<EdgeBody>) -> ConsoleCookieState {
    let mut state = ConsoleCookieState::default();
    for value in request.headers().get_all(header::COOKIE) {
        let Ok(value) = value.to_str() else {
            continue;
        };
        for cookie in value.split(';') {
            let cookie = cookie.trim();
            match cookie.split_once('=') {
                Some((name, value)) if name.trim() == GPT_DIAGNOSTICS_COOKIE => {
                    state.occurrences += 1;
                    state.canonical |= value.trim() == "1";
                }
                None if cookie == GPT_DIAGNOSTICS_COOKIE => state.occurrences += 1,
                _ => {}
            }
        }
    }
    state
}

fn sanitize_console_cookie(request: &mut Request<EdgeBody>) {
    let retained = request
        .headers()
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .map(str::trim)
        .filter(|cookie| match cookie.split_once('=') {
            Some((name, _)) => name.trim() != GPT_DIAGNOSTICS_COOKIE,
            None => *cookie != GPT_DIAGNOSTICS_COOKIE,
        })
        .filter(|cookie| !cookie.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();

    request.headers_mut().remove(header::COOKIE);
    if !retained.is_empty() {
        let value = HeaderValue::from_str(&retained.join("; "))
            .expect("should preserve already-valid cookie header values");
        request.headers_mut().insert(header::COOKIE, value);
    }
}

fn replace_path_and_query(
    request: &mut Request<EdgeBody>,
    clean_path_and_query: &str,
) -> Result<(), Report<TrustedServerError>> {
    let mut parts = request.uri().clone().into_parts();
    parts.path_and_query = Some(
        clean_path_and_query
            .parse::<PathAndQuery>()
            .change_context(TrustedServerError::Proxy {
                message: "GPT diagnostics query produced invalid URI".to_owned(),
            })?,
    );
    *request.uri_mut() = Uri::from_parts(parts).change_context(TrustedServerError::Proxy {
        message: "GPT diagnostics query produced invalid URI".to_owned(),
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integrations::IntegrationRegistry;
    use crate::test_support::tests::create_test_settings;
    use serde_json::json;

    fn settings(enabled: bool) -> Settings {
        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config(
                GPT_DIAGNOSTICS_INTEGRATION_ID,
                &json!({ "enabled": enabled }),
            )
            .expect("should insert diagnostics config");
        settings
    }

    fn navigation(uri: &str, cookie: Option<&str>) -> Request<EdgeBody> {
        let mut builder = Request::builder()
            .method(Method::GET)
            .uri(uri)
            .header("sec-fetch-dest", "document");
        if let Some(cookie) = cookie {
            builder = builder.header(header::COOKIE, cookie);
        }
        builder
            .body(EdgeBody::empty())
            .expect("should build request")
    }

    #[test]
    fn register_excludes_diagnostics_from_unified_and_deferred_bundles() {
        let registry = IntegrationRegistry::new(&settings(true)).expect("should build registry");

        assert!(registry.integration_enabled(GPT_DIAGNOSTICS_INTEGRATION_ID));
        assert!(
            !registry
                .js_module_ids_immediate()
                .contains(&GPT_DIAGNOSTICS_INTEGRATION_ID)
        );
        assert!(
            !registry
                .js_module_ids_deferred()
                .contains(&GPT_DIAGNOSTICS_INTEGRATION_ID)
        );
    }

    #[test]
    fn exact_directive_activates_cleans_and_strips_cookie() {
        let mut request = navigation(
            "https://publisher.example/page?keep=%2F&ts_console=true#fragment",
            Some("other=value; __Host-ts-console=1"),
        );

        let decision = prepare_request(&settings(true), &mut request).expect("should prepare");

        assert!(decision.active());
        assert_eq!(
            decision.cookie_action,
            GptDiagnosticsCookieAction::SetSession
        );
        assert_eq!(
            request.uri().to_string(),
            "https://publisher.example/page?keep=%2F"
        );
        assert_eq!(request.headers()[header::COOKIE], "other=value");
        let bootstrap = decision.bootstrap_script().expect("should bootstrap");
        assert!(bootstrap.contains("__tsjs_gpt_diagnostics_active=true"));
        assert!(bootstrap.contains("/page?keep=%2F"));
    }

    #[test]
    fn prefetch_directive_is_sanitized_without_activating_session() {
        let mut request = navigation("https://publisher.example/page?ts_console=1&keep=1", None);
        request
            .headers_mut()
            .insert("purpose", HeaderValue::from_static("prefetch"));

        let decision = prepare_request(&settings(true), &mut request).expect("should prepare");

        assert!(
            !decision.active(),
            "prefetch should not activate diagnostics"
        );
        assert_eq!(decision.cookie_action, GptDiagnosticsCookieAction::None);
        assert_eq!(request.uri().query(), Some("keep=1"));
    }

    #[test]
    fn active_cookie_enables_clean_navigation_but_duplicates_fail_closed() {
        let mut active = navigation(
            "https://publisher.example/page",
            Some("__Host-ts-console=1; other=value"),
        );
        let decision = prepare_request(&settings(true), &mut active).expect("should prepare");
        assert!(decision.active());
        assert_eq!(active.headers()[header::COOKIE], "other=value");

        let mut duplicate = navigation(
            "https://publisher.example/page",
            Some("__Host-ts-console=1; __Host-ts-console=1; other=value"),
        );
        let decision = prepare_request(&settings(true), &mut duplicate).expect("should prepare");
        assert!(!decision.active());
        assert_eq!(duplicate.headers()[header::COOKIE], "other=value");
    }

    #[test]
    fn invalid_duplicate_and_disable_directives_fail_closed() {
        for query in [
            "ts_console=True",
            "ts_console=",
            "ts_console=1&ts_console=true",
        ] {
            let mut request = navigation(
                &format!("https://publisher.example/page?{query}&keep=1"),
                Some("__Host-ts-console=1"),
            );
            let decision = prepare_request(&settings(true), &mut request).expect("should prepare");
            assert!(!decision.active(), "{query} should fail closed");
            assert_eq!(decision.cookie_action, GptDiagnosticsCookieAction::None);
            assert_eq!(request.uri().query(), Some("keep=1"));
        }

        let mut request = navigation(
            "https://publisher.example/page?ts_console=false&keep=1",
            Some("__Host-ts-console=1"),
        );
        let decision = prepare_request(&settings(true), &mut request).expect("should prepare");
        assert!(!decision.active());
        assert_eq!(
            decision.cookie_action,
            GptDiagnosticsCookieAction::ClearSession
        );
    }

    #[test]
    fn finalization_sets_cookie_and_strips_shared_cache_headers() {
        let mut request = navigation("https://publisher.example/?ts_console=1", None);
        let decision = prepare_request(&settings(true), &mut request).expect("should prepare");
        let mut response = Response::builder()
            .header(header::CACHE_CONTROL, "public, max-age=60")
            .header(header::ETAG, "\"origin\"")
            .header(header::LAST_MODIFIED, "Wed, 12 Aug 2026 00:00:00 GMT")
            .header("surrogate-control", "max-age=60")
            .header("fastly-surrogate-control", "max-age=60")
            .header("cloudflare-cdn-cache-control", "public, max-age=60")
            .body(EdgeBody::empty())
            .expect("should build response");

        finalize_response(&decision, &mut response);

        assert_eq!(
            response.headers()[header::CACHE_CONTROL],
            "no-store, private",
            "should stamp diagnostics responses non-storable"
        );
        assert_eq!(response.headers()[header::SET_COOKIE], SET_CONSOLE_COOKIE);
        assert!(!response.headers().contains_key("surrogate-control"));
        assert!(!response.headers().contains_key("fastly-surrogate-control"));
        assert!(
            !response
                .headers()
                .contains_key("cloudflare-cdn-cache-control")
        );
        assert!(
            !response.headers().contains_key(header::ETAG),
            "should drop the origin validator with the shared-cache policy"
        );
        assert!(
            !response.headers().contains_key(header::LAST_MODIFIED),
            "should drop the origin validator with the shared-cache policy"
        );
    }

    #[test]
    fn an_active_no_cookie_action_response_is_marked_terminal_private() {
        // The session-cookie activation path: active, but nothing new to set. Stamping
        // `Cache-Control` alone left this response defenceless against a later mutation,
        // because the adapter's terminal guard keys on the marker, not on the stamp, and
        // the `Set-Cookie` privacy net never sees a response that sets no cookie.
        let mut request = navigation("https://publisher.example/", Some("__Host-ts-console=1"));
        let decision = prepare_request(&settings(true), &mut request).expect("should prepare");
        assert!(decision.active(), "the session cookie should activate");
        assert_eq!(
            decision.cookie_action,
            GptDiagnosticsCookieAction::None,
            "an already-established session sets no new cookie"
        );
        let mut response = Response::builder()
            .body(EdgeBody::empty())
            .expect("should build response");

        finalize_response(&decision, &mut response);

        assert!(
            !response.headers().contains_key(header::SET_COOKIE),
            "should not set a cookie for an established session"
        );
        assert!(
            response
                .extensions()
                .get::<crate::response_privacy::TerminalPrivateResponse>()
                .is_some(),
            "should mark request-scoped diagnostics HTML for terminal re-enforcement"
        );
    }

    #[test]
    fn an_inactive_decision_leaves_the_origin_cache_policy_alone() {
        let mut request = navigation("https://publisher.example/", None);
        let decision = prepare_request(&settings(true), &mut request).expect("should prepare");
        assert!(!decision.requires_private_no_store());
        let mut response = Response::builder()
            .header(header::CACHE_CONTROL, "public, max-age=60")
            .header(header::ETAG, "\"origin\"")
            .body(EdgeBody::empty())
            .expect("should build response");

        finalize_response(&decision, &mut response);

        assert_eq!(
            response.headers()[header::CACHE_CONTROL],
            "public, max-age=60",
            "should not downgrade a response the integration did not touch"
        );
        assert!(
            response
                .extensions()
                .get::<crate::response_privacy::TerminalPrivateResponse>()
                .is_none(),
            "should not mark an untouched response terminal-private"
        );
    }

    #[test]
    fn config_rejects_unknown_fields() {
        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config(
                GPT_DIAGNOSTICS_INTEGRATION_ID,
                &json!({ "enabled": true, "typo": true }),
            )
            .expect("should insert diagnostics config");

        let error = settings
            .integration_config::<GptDiagnosticsConfig>(GPT_DIAGNOSTICS_INTEGRATION_ID)
            .expect_err("should reject unknown diagnostics config fields");
        let error_text = format!("{error:?}");
        assert!(error_text.contains("typo") || error_text.contains("unknown field"));
    }
}
