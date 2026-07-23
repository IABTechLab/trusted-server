//! Query-activated, session-scoped auction trace integration.

use edgezero_core::body::Body as EdgeBody;
use error_stack::{Report, ResultExt};
use http::{HeaderValue, Method, Request, Response, Uri, header, uri::PathAndQuery};
use serde::Deserialize;
use validator::Validate;

use crate::constants::{COOKIE_TS_CONSOLE, QUERY_TS_CONSOLE};
use crate::error::TrustedServerError;
use crate::http_util::is_navigation_request;
use crate::integrations::IntegrationRegistration;
use crate::settings::{IntegrationConfig, Settings};

/// Stable integration identifier.
pub const AD_TRACE_INTEGRATION_ID: &str = "ad_trace";

const SET_CONSOLE_COOKIE: &str = "__Host-ts-console=1; Path=/; Secure; HttpOnly; SameSite=Lax";
const CLEAR_CONSOLE_COOKIE: &str =
    "__Host-ts-console=; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age=0";

/// Configuration for the optional browser console.
#[derive(Debug, Default, Deserialize, Validate)]
#[serde(deny_unknown_fields)]
pub struct AdTraceConfig {
    /// Enable the optional ad trace browser module and console activation.
    #[serde(default)]
    pub enabled: bool,
}

impl IntegrationConfig for AdTraceConfig {
    fn is_enabled(&self) -> bool {
        self.enabled
    }
}

/// Cookie mutation attached to an eligible console-navigation response.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ConsoleCookieAction {
    #[default]
    None,
    SetSession,
    ClearSession,
}

/// Immutable request-scoped console decision.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AdTraceRequestDecision {
    enabled: bool,
    browser_bootstrap: bool,
    private_response: bool,
    clean_browser_path_and_query: Option<String>,
    cookie_action: ConsoleCookieAction,
}

impl AdTraceRequestDecision {
    /// Whether browser-visible trace fields and targeting are enabled.
    #[must_use]
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Whether this response must be private and non-storeable.
    #[must_use]
    pub fn requires_private_no_store(&self) -> bool {
        self.private_response
            || self.cookie_action != ConsoleCookieAction::None
            || self.clean_browser_path_and_query.is_some()
    }

    /// Build the synchronous bootstrap inserted before the unified TSJS bundle.
    #[must_use]
    pub fn bootstrap_script(&self) -> Option<String> {
        if !self.browser_bootstrap && self.clean_browser_path_and_query.is_none() {
            return None;
        }

        let mut script = String::from("<script>");
        if self.browser_bootstrap {
            script.push_str("window.__tsjs_adTraceActive=true;");
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

#[derive(Clone, Copy, Debug, Default)]
struct AdTraceCookieApplied;

/// Register the optional browser module.
///
/// # Errors
///
/// Returns a configuration error when the integration settings are invalid.
pub fn register(
    settings: &Settings,
) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
    let Some(_config) = settings.integration_config::<AdTraceConfig>(AD_TRACE_INTEGRATION_ID)?
    else {
        return Ok(None);
    };
    Ok(Some(
        IntegrationRegistration::builder(AD_TRACE_INTEGRATION_ID).build(),
    ))
}

/// Evaluate and sanitize the console request before routing or downstream use.
///
/// The original query and cookie are inspected first. Every reserved query pair
/// and console cookie is then removed from the request. The immutable decision
/// is stored in request extensions for handlers to consume after sanitation.
///
/// # Errors
///
/// Returns an error when integration configuration or URI reconstruction fails.
pub fn prepare_request(
    settings: &Settings,
    request: &mut Request<EdgeBody>,
) -> Result<AdTraceRequestDecision, Report<TrustedServerError>> {
    let integration_enabled = settings
        .integration_config::<AdTraceConfig>(AD_TRACE_INTEGRATION_ID)?
        .is_some();
    let (directive, clean_path, had_reserved_query) = console_query(request.uri());
    let cookie_state = console_cookie_state(request);
    let eligible_navigation = is_eligible_console_navigation(request);

    sanitize_console_cookie(request);
    if had_reserved_query {
        replace_path_and_query(request, &clean_path)?;
    }

    let mut decision = AdTraceRequestDecision::default();
    if integration_enabled && eligible_navigation && had_reserved_query {
        decision.clean_browser_path_and_query = Some(clean_path);
        match directive {
            QueryDirective::Enable => {
                decision.enabled = true;
                decision.browser_bootstrap = true;
                decision.cookie_action = ConsoleCookieAction::SetSession;
            }
            QueryDirective::Disable => {
                decision.cookie_action = ConsoleCookieAction::ClearSession;
            }
            QueryDirective::Invalid | QueryDirective::Absent => {}
        }
    } else if integration_enabled
        && directive == QueryDirective::Absent
        && cookie_state.occurrences == 1
        && cookie_state.canonical
    {
        decision.enabled = true;
        decision.browser_bootstrap = eligible_navigation;
    }

    decision.private_response =
        decision.enabled && trace_payload_request(request, eligible_navigation);
    request.extensions_mut().insert(decision.clone());
    Ok(decision)
}

/// Read the previously prepared request decision.
#[must_use]
pub fn request_decision(request: &Request<EdgeBody>) -> AdTraceRequestDecision {
    request
        .extensions()
        .get::<AdTraceRequestDecision>()
        .cloned()
        .unwrap_or_default()
}

/// Return whether browser-visible trace output is active for this request.
#[must_use]
pub fn browser_trace_enabled(request: &Request<EdgeBody>) -> bool {
    request_decision(request).enabled()
}

/// Copy the prepared request decision onto a response for outer finalization.
pub fn attach_response_decision(
    decision: &AdTraceRequestDecision,
    response: &mut Response<EdgeBody>,
) {
    response.extensions_mut().insert(decision.clone());
}

/// Apply the response-side session mutation and cache policy.
///
/// Safe to call more than once. The cookie is appended once, while the
/// private/no-store policy is reasserted so later adapter cache policy cannot
/// weaken it.
pub fn finalize_response(response: &mut Response<EdgeBody>) {
    let Some(decision) = response
        .extensions()
        .get::<AdTraceRequestDecision>()
        .cloned()
    else {
        return;
    };

    if decision.cookie_action != ConsoleCookieAction::None
        && response
            .extensions()
            .get::<AdTraceCookieApplied>()
            .is_none()
    {
        let value = match decision.cookie_action {
            ConsoleCookieAction::None => None,
            ConsoleCookieAction::SetSession => Some(HeaderValue::from_static(SET_CONSOLE_COOKIE)),
            ConsoleCookieAction::ClearSession => {
                Some(HeaderValue::from_static(CLEAR_CONSOLE_COOKIE))
            }
        };
        if let Some(value) = value {
            response.headers_mut().append(header::SET_COOKIE, value);
            response.extensions_mut().insert(AdTraceCookieApplied);
        }
    }

    if decision.requires_private_no_store() {
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("private, no-store"),
        );
        for name in crate::response_privacy::SURROGATE_CACHE_HEADERS {
            response.headers_mut().remove(*name);
        }
    }
}

fn trace_payload_request(request: &Request<EdgeBody>, eligible_navigation: bool) -> bool {
    eligible_navigation
        || request.uri().path() == "/auction"
        || request.uri().path() == "/__ts/page-bids"
}

fn is_eligible_console_navigation(request: &Request<EdgeBody>) -> bool {
    request.method() == Method::GET
        && is_navigation_request(request)
        && !crate::publisher::is_prefetch_request(request)
        && !crate::publisher::is_bot_user_agent(request)
}

fn console_query(uri: &Uri) -> (QueryDirective, String, bool) {
    let mut console_values = Vec::new();
    let mut retained = Vec::new();
    for pair in uri.query().unwrap_or_default().split('&') {
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        if name == QUERY_TS_CONSOLE {
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
                Some((name, value)) if name.trim() == COOKIE_TS_CONSOLE => {
                    state.occurrences += 1;
                    state.canonical |= value.trim() == "1";
                }
                None if cookie == COOKIE_TS_CONSOLE => state.occurrences += 1,
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
            Some((name, _)) => name.trim() != COOKIE_TS_CONSOLE,
            None => *cookie != COOKIE_TS_CONSOLE,
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
                message: "ad trace console query produced invalid URI".to_owned(),
            })?,
    );
    *request.uri_mut() = Uri::from_parts(parts).change_context(TrustedServerError::Proxy {
        message: "ad trace console query produced invalid URI".to_owned(),
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use http::{Request, Response, header};

    use crate::test_support::tests::create_test_settings;

    use super::*;

    fn settings(enabled: bool) -> Settings {
        let mut settings = create_test_settings();
        settings.integrations.insert(
            AD_TRACE_INTEGRATION_ID.to_owned(),
            serde_json::json!({ "enabled": enabled }),
        );
        settings
    }

    fn request(uri: &str, cookie: Option<&str>) -> Request<EdgeBody> {
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
    fn rejects_unknown_gate_configuration() {
        let mut settings = create_test_settings();
        settings.integrations.insert(
            AD_TRACE_INTEGRATION_ID.to_owned(),
            serde_json::json!({ "enabled": true, "enabledd": true }),
        );

        let error = settings
            .integration_config::<AdTraceConfig>(AD_TRACE_INTEGRATION_ID)
            .expect_err("should reject unknown gate field");
        assert!(
            error.to_string().contains("could not be parsed"),
            "should reject invalid configuration: {error}"
        );
    }

    #[test]
    fn query_enables_first_response_and_sanitizes_request() {
        let mut req = request(
            "https://publisher.example/page?x=%2F&ts_console=1&y=2",
            Some("session=abc; __Host-ts-console=1"),
        );
        let decision = prepare_request(&settings(true), &mut req).expect("should prepare");

        assert!(decision.enabled());
        assert_eq!(decision.cookie_action, ConsoleCookieAction::SetSession);
        assert_eq!(
            req.uri().to_string(),
            "https://publisher.example/page?x=%2F&y=2"
        );
        assert_eq!(
            req.headers()
                .get(header::COOKIE)
                .expect("should retain unrelated cookie"),
            "session=abc"
        );
        assert!(browser_trace_enabled(&req));
        let script = decision.bootstrap_script().expect("should bootstrap");
        assert!(script.contains("__tsjs_adTraceActive=true"));
        assert!(script.contains("/page?x=%2F&y=2"));

        let mut separators = request(
            "https://publisher.example/page?a=1&&ts_console=1&b=2&",
            None,
        );
        prepare_request(&settings(true), &mut separators).expect("should prepare");
        assert_eq!(separators.uri().query(), Some("a=1&&b=2&"));
    }

    #[test]
    fn exact_enable_and_disable_values_are_supported() {
        for value in ["true", "1"] {
            let mut req = request(
                &format!("https://publisher.example/?ts_console={value}"),
                None,
            );
            let decision = prepare_request(&settings(true), &mut req).expect("should prepare");
            assert!(decision.enabled(), "{value} should enable");
            assert_eq!(decision.cookie_action, ConsoleCookieAction::SetSession);
        }
        for value in ["false", "0"] {
            let mut req = request(
                &format!("https://publisher.example/?ts_console={value}"),
                Some("__Host-ts-console=1"),
            );
            let decision = prepare_request(&settings(true), &mut req).expect("should prepare");
            assert!(!decision.enabled(), "{value} should disable");
            assert_eq!(decision.cookie_action, ConsoleCookieAction::ClearSession);
        }
    }

    #[test]
    fn invalid_or_duplicate_query_fails_closed_without_cookie_mutation() {
        for query in [
            "ts_console=True",
            "ts_console=",
            "ts_console=1&ts_console=true",
        ] {
            let mut req = request(
                &format!("https://publisher.example/?{query}&keep=1"),
                Some("__Host-ts-console=1"),
            );
            let decision = prepare_request(&settings(true), &mut req).expect("should prepare");
            assert!(!decision.enabled(), "{query} should fail closed");
            assert_eq!(decision.cookie_action, ConsoleCookieAction::None);
            assert_eq!(req.uri().query(), Some("keep=1"));
        }
    }

    #[test]
    fn disabled_config_sanitizes_but_never_activates() {
        let mut req = request(
            "https://publisher.example/?ts_console=1&keep=1",
            Some("__Host-ts-console=1; other=value; ts-tester=true"),
        );
        let decision = prepare_request(&settings(false), &mut req).expect("should prepare");
        assert!(!decision.enabled());
        assert_eq!(decision.cookie_action, ConsoleCookieAction::None);
        assert_eq!(decision.clean_browser_path_and_query, None);
        assert_eq!(req.uri().query(), Some("keep=1"));
        assert_eq!(
            req.headers()
                .get(header::COOKIE)
                .expect("should retain unrelated cookies"),
            "other=value; ts-tester=true"
        );
    }

    #[test]
    fn exact_session_cookie_gates_api_but_query_cannot_activate_it() {
        let mut active = Request::builder()
            .method(Method::POST)
            .uri("https://publisher.example/auction")
            .header(header::COOKIE, "__Host-ts-console=1")
            .body(EdgeBody::empty())
            .expect("should build request");
        assert!(
            prepare_request(&settings(true), &mut active)
                .expect("should prepare")
                .enabled()
        );

        let mut query_only = Request::builder()
            .method(Method::POST)
            .uri("https://publisher.example/auction?ts_console=1")
            .body(EdgeBody::empty())
            .expect("should build request");
        let decision = prepare_request(&settings(true), &mut query_only).expect("should prepare");
        assert!(!decision.enabled());
        assert_eq!(decision.cookie_action, ConsoleCookieAction::None);
        assert_eq!(query_only.uri().query(), None);
    }

    #[test]
    fn active_session_does_not_make_static_bundle_response_private() {
        let mut req = Request::builder()
            .method(Method::GET)
            .uri("https://publisher.example/static/tsjs=tsjs-unified.min.js")
            .header("sec-fetch-dest", "script")
            .header(header::COOKIE, "__Host-ts-console=1")
            .body(EdgeBody::empty())
            .expect("should build request");
        let decision = prepare_request(&settings(true), &mut req).expect("should prepare");
        assert!(decision.enabled());
        assert!(!decision.requires_private_no_store());
        assert_eq!(decision.bootstrap_script(), None);
    }

    #[test]
    fn invalid_api_query_fails_closed_even_with_session_cookie() {
        let mut req = Request::builder()
            .method(Method::POST)
            .uri("https://publisher.example/auction?ts_console=invalid")
            .header(header::COOKIE, "__Host-ts-console=1")
            .body(EdgeBody::empty())
            .expect("should build request");
        let decision = prepare_request(&settings(true), &mut req).expect("should prepare");
        assert!(!decision.enabled());
        assert_eq!(req.uri().query(), None);
        assert!(!req.headers().contains_key(header::COOKIE));
    }

    #[test]
    fn duplicate_console_cookie_fails_closed_and_all_copies_are_removed() {
        let mut req = request(
            "https://publisher.example/",
            Some("__Host-ts-console=1; a=b; __Host-ts-console=1"),
        );
        let decision = prepare_request(&settings(true), &mut req).expect("should prepare");
        assert!(!decision.enabled());
        assert_eq!(
            req.headers()
                .get(header::COOKIE)
                .expect("should retain unrelated cookie"),
            "a=b"
        );

        let mut bare = request(
            "https://publisher.example/",
            Some("__Host-ts-console=1; __Host-ts-console; a=b"),
        );
        let decision = prepare_request(&settings(true), &mut bare).expect("should prepare");
        assert!(!decision.enabled());
        assert_eq!(
            bare.headers()
                .get(header::COOKIE)
                .expect("should retain unrelated cookie"),
            "a=b"
        );
    }

    #[test]
    fn ts_tester_cookie_no_longer_activates_console() {
        let mut req = request("https://publisher.example/", Some("ts-tester=true"));
        let decision = prepare_request(&settings(true), &mut req).expect("should prepare");
        assert!(!decision.enabled());
    }

    #[test]
    fn response_finalization_appends_cookie_once_and_reasserts_no_store() {
        let mut req = request("https://publisher.example/?ts_console=1", None);
        let decision = prepare_request(&settings(true), &mut req).expect("should prepare");
        let mut response = Response::builder()
            .header(header::SET_COOKIE, "existing=value")
            .header(header::CACHE_CONTROL, "public, max-age=60")
            .header("surrogate-control", "max-age=60")
            .header("cloudflare-cdn-cache-control", "public, max-age=60")
            .body(EdgeBody::empty())
            .expect("should build response");
        attach_response_decision(&decision, &mut response);

        finalize_response(&mut response);
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("public, max-age=60"),
        );
        finalize_response(&mut response);

        assert_eq!(
            response
                .headers()
                .get_all(header::SET_COOKIE)
                .iter()
                .count(),
            2
        );
        assert_eq!(
            response.headers()[header::CACHE_CONTROL],
            "private, no-store"
        );
        assert!(!response.headers().contains_key("surrogate-control"));
        assert!(
            !response
                .headers()
                .contains_key("cloudflare-cdn-cache-control")
        );
    }
}
