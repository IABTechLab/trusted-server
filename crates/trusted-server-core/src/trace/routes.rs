use std::borrow::Cow;

use edgezero_core::body::Body as EdgeBody;
use http::{HeaderValue, Method, Request, Response, StatusCode, header};
use sha2::{Digest as _, Sha256};

use crate::auth::{TraceAuthLogPolicy, enforce_basic_auth};
use crate::integrations::gpt_diagnostics::{GPT_DIAGNOSTICS_INTEGRATION_ID, GptDiagnosticsConfig};
use crate::response_privacy::enforce_terminal_private_cache_privacy;
use crate::settings::Settings;

use super::TraceTerminalResponse;

const NAMESPACE: &str = "/_ts/trace";
const MAX_DECODE_ROUNDS: usize = 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TraceRoute {
    Shell,
    State,
    Enable,
    End,
    Javascript,
    Stylesheet,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Classification {
    NotTrace,
    Route(TraceRoute),
    Rejected(StatusCode),
}

/// Result of authenticated trace-route preflight.
pub enum TracePreflight {
    /// Continue ordinary dispatch without inspecting diagnostics configuration.
    NotTrace,
    /// Terminate with a hardened local challenge or policy error.
    Response(Response<EdgeBody>),
    /// Invoke the route handler and finalize its response with this context.
    Ready(TraceDispatch),
}

/// Authenticated route context for a later trace handler.
///
/// This is a response-policy seam, not a successful action or cookie observation.
pub struct TraceDispatch {
    pub(crate) route: TraceRoute,
    head: bool,
    protected: bool,
}

impl TraceDispatch {
    /// Apply dynamic trace response policy after a handler produces its result.
    ///
    /// Successful assets require [`Self::fixed_asset`] instead. Errors are always
    /// JSON and private; GET and HEAD responses cannot set cookies.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let response = context.respond(handler_response);
    /// ```
    #[must_use]
    pub fn respond(self, mut response: Response<EdgeBody>) -> Response<EdgeBody> {
        if self.route.is_asset() && response.status().is_success() {
            response = error_response(StatusCode::INTERNAL_SERVER_ERROR);
        }
        let mime = if self.route == TraceRoute::Shell && response.status().is_success() {
            "text/html; charset=utf-8"
        } else {
            "application/json; charset=utf-8"
        };
        if !matches!(self.route, TraceRoute::Enable | TraceRoute::End)
            || !response.status().is_success()
        {
            response.headers_mut().remove(header::SET_COOKIE);
        }
        harden_response(&mut response, mime, self.head);
        response
    }

    /// Build an asset response from immutable build bytes.
    ///
    /// This method does not locate an asset or manufacture a missing build.
    /// An asset lookup must supply its verified bytes. Non-asset contexts fail
    /// with a fixed private error.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let response = context.fixed_asset(verified_asset_bytes);
    /// ```
    ///
    /// # Panics
    ///
    /// Panics if a quoted ASCII SHA-256 digest cannot be represented as a header.
    /// The digest representation always satisfies that invariant.
    #[must_use]
    pub fn fixed_asset(self, bytes: &'static [u8]) -> Response<EdgeBody> {
        let mime = match self.route {
            TraceRoute::Javascript => "application/javascript; charset=utf-8",
            TraceRoute::Stylesheet => "text/css; charset=utf-8",
            _ => return self.respond(error_response(StatusCode::INTERNAL_SERVER_ERROR)),
        };
        let mut response = Response::new(EdgeBody::from(bytes));
        harden_response(&mut response, mime, self.head);
        if !self.protected {
            response.headers_mut().insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static("public, max-age=31536000, immutable"),
            );
            response
                .extensions_mut()
                .remove::<crate::response_privacy::TerminalPrivateResponse>();
        }
        // Private assets also retain their byte-derived validator. The shared
        // privacy helper removes validators from dynamic responses first.
        let etag = format!("\"{:x}\"", Sha256::digest(bytes));
        response.headers_mut().insert(
            header::ETAG,
            HeaderValue::from_str(&etag).expect("should encode an ASCII SHA-256 ETag"),
        );
        response
    }
}

impl TraceRoute {
    fn is_asset(self) -> bool {
        matches!(self, Self::Javascript | Self::Stylesheet)
    }

    fn allows(self, method: &Method) -> bool {
        if matches!(self, Self::Enable | Self::End) {
            method == Method::POST
        } else {
            method == Method::GET || method == Method::HEAD
        }
    }

    fn allow(self) -> &'static str {
        if matches!(self, Self::Enable | Self::End) {
            "POST"
        } else {
            "GET, HEAD"
        }
    }
}

fn error_response(status: StatusCode) -> Response<EdgeBody> {
    let body: &'static [u8] = match status {
        StatusCode::BAD_REQUEST => br#"{"error":"invalid trace path"}"#,
        StatusCode::UNAUTHORIZED => br#"{"error":"unauthorized"}"#,
        StatusCode::NOT_FOUND => br#"{"error":"trace route not found"}"#,
        StatusCode::METHOD_NOT_ALLOWED => br#"{"error":"method not allowed"}"#,
        _ => br#"{"error":"trace unavailable"}"#,
    };
    let mut response = Response::new(EdgeBody::from(body));
    *response.status_mut() = status;
    response
}

/// Build the exact observed state from frozen cookie health after preflight.
///
/// Apply [`TraceDispatch::respond`] for dynamic privacy and HEAD body removal.
/// A false result reports no valid observed session, never cookie absence.
pub(crate) fn state_response(cookies: &super::TraceCookies) -> Response<EdgeBody> {
    let body: &'static [u8] = if cookies.observed_active() {
        br#"{"observed_active":true}"#
    } else {
        br#"{"observed_active":false}"#
    };
    Response::new(EdgeBody::from(body))
}

fn harden_response(response: &mut Response<EdgeBody>, mime: &'static str, head: bool) {
    enforce_terminal_private_cache_privacy(response);
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(mime));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static("default-src 'none'; script-src 'self'; style-src 'self'; base-uri 'none'; object-src 'none'; frame-ancestors 'none'; form-action 'none'; connect-src 'self'; img-src data:"));
    headers.insert(
        "permissions-policy",
        HeaderValue::from_static("camera=(), microphone=(), geolocation=(), payment=(), usb=()"),
    );
    response.extensions_mut().insert(TraceTerminalResponse);
    if head {
        *response.body_mut() = EdgeBody::empty();
    }
}

fn reject(mut response: Response<EdgeBody>, head: bool) -> TracePreflight {
    response.headers_mut().remove(header::SET_COOKIE);
    harden_response(&mut response, "application/json; charset=utf-8", head);
    TracePreflight::Response(response)
}

/// Return whether the visible pathname is reserved for local trace handling.
///
/// Encoded aliases are reserved for rejection and never become supported routes.
/// No original-target metadata participates in this decision.
///
/// # Examples
///
/// ```
/// assert!(trusted_server_core::trace::is_trace_path("/_ts/trace"));
/// assert!(!trusted_server_core::trace::is_trace_path("/article"));
/// ```
///
/// # Performance
///
/// Ordinary paths without encoded or ambiguous segments avoid allocations.
#[must_use]
pub fn is_trace_path(path: &str) -> bool {
    classify_path(path) != Classification::NotTrace
}

/// Authenticate and validate a visible trace route before handler dispatch.
///
/// A ready result does not observe cookies, mutate a session, or supply assets.
/// Route handlers implement those contracts in subsequent layers.
///
/// # Examples
///
/// ```ignore
/// match trusted_server_core::trace::preflight(&settings, &mut request) {
///     trusted_server_core::trace::TracePreflight::Response(response) => return response,
///     trusted_server_core::trace::TracePreflight::NotTrace => continue_dispatch(request),
///     trusted_server_core::trace::TracePreflight::Ready(context) => handle_trace(context, request),
/// }
/// ```
#[must_use]
pub fn preflight(settings: &Settings, request: &mut Request<EdgeBody>) -> TracePreflight {
    let classification = classify_path(request.uri().path());
    if classification == Classification::NotTrace {
        return TracePreflight::NotTrace;
    }
    let head = request.method() == Method::HEAD;
    request.extensions_mut().insert(TraceAuthLogPolicy);
    match enforce_basic_auth(settings, request) {
        Ok(Some(mut response)) => {
            *response.body_mut() = error_response(StatusCode::UNAUTHORIZED).into_body();
            return reject(response, head);
        }
        Err(_) => return reject(error_response(StatusCode::INTERNAL_SERVER_ERROR), head),
        Ok(None) => {}
    }
    let enabled =
        match settings.integration_config::<GptDiagnosticsConfig>(GPT_DIAGNOSTICS_INTEGRATION_ID) {
            Ok(Some(config)) => config.trace_page_enabled,
            Ok(None) => false,
            Err(_) => return reject(error_response(StatusCode::INTERNAL_SERVER_ERROR), head),
        };
    if !enabled {
        return reject(error_response(StatusCode::NOT_FOUND), head);
    }
    let route = match classification {
        Classification::Route(route) => route,
        Classification::Rejected(status) => return reject(error_response(status), head),
        Classification::NotTrace => return TracePreflight::NotTrace,
    };
    if !route.allows(request.method()) {
        let mut response = error_response(StatusCode::METHOD_NOT_ALLOWED);
        response
            .headers_mut()
            .insert(header::ALLOW, HeaderValue::from_static(route.allow()));
        return reject(response, head);
    }
    let protected = match settings.handler_for_path(request.uri().path()) {
        Ok(handler) => handler.is_some(),
        Err(_) => return reject(error_response(StatusCode::INTERNAL_SERVER_ERROR), head),
    };
    TracePreflight::Ready(TraceDispatch {
        route,
        head,
        protected,
    })
}

fn exact_route(path: &str) -> Option<TraceRoute> {
    match path {
        "/_ts/trace" => Some(TraceRoute::Shell),
        "/_ts/trace/state" => Some(TraceRoute::State),
        "/_ts/trace/enable" => Some(TraceRoute::Enable),
        "/_ts/trace/end" => Some(TraceRoute::End),
        "/_ts/trace/assets/v1.js" => Some(TraceRoute::Javascript),
        "/_ts/trace/assets/v1.css" => Some(TraceRoute::Stylesheet),
        _ => None,
    }
}

fn has_dot_segments(path: &str) -> bool {
    path.split('/').any(|segment| matches!(segment, "." | ".."))
}

fn reserves_namespace(path: &str) -> bool {
    if path.starts_with(NAMESPACE) {
        return true;
    }
    if !path.contains("//") && !has_dot_segments(path) && !path.contains('\\') {
        return false;
    }
    // Alternative spellings are considered only to reserve and reject them.
    // The resulting path is never used for routing or authentication.
    let mut segments = Vec::new();
    for segment in path.split(['/', '\\']) {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            _ => segments.push(segment),
        }
        // Once a visible alternative spelling reaches the namespace, later
        // dot segments cannot erase its reservation at application dispatch.
        if segments.first() == Some(&"_ts")
            && segments
                .get(1)
                .is_some_and(|segment| segment.starts_with("trace"))
        {
            return true;
        }
    }
    false
}

fn encoded_separator(path: &str) -> bool {
    path.as_bytes().windows(3).any(|bytes| {
        bytes[0] == b'%'
            && (bytes[1] == b'2' && bytes[2].eq_ignore_ascii_case(&b'f')
                || bytes[1] == b'5' && bytes[2].eq_ignore_ascii_case(&b'c'))
    })
}

fn valid_escapes(path: &str) -> bool {
    let mut bytes = path.bytes();
    while let Some(byte) = bytes.next() {
        if byte == b'%'
            && (!bytes.next().is_some_and(|value| value.is_ascii_hexdigit())
                || !bytes.next().is_some_and(|value| value.is_ascii_hexdigit()))
        {
            return false;
        }
    }
    true
}

fn classify_path(path: &str) -> Classification {
    if let Some(route) = exact_route(path) {
        return Classification::Route(route);
    }
    let mut reserved = reserves_namespace(path);
    if reserved && (has_dot_segments(path) || path.contains('\\')) {
        return Classification::Rejected(StatusCode::BAD_REQUEST);
    }
    let mut current = Cow::Borrowed(path);
    let mut invalid_encoding = false;
    for _ in 0..MAX_DECODE_ROUNDS {
        if !current.contains('%') {
            break;
        }
        invalid_encoding |= !valid_escapes(&current);
        // Decode bytes so an unreadable suffix cannot hide an ASCII namespace
        // prefix. Lossy text is used exclusively for reservation, never routing.
        let decoded_bytes = urlencoding::decode_binary(current.as_bytes());
        invalid_encoding |= core::str::from_utf8(&decoded_bytes).is_err();
        let decoded = String::from_utf8_lossy(&decoded_bytes);
        let decoded_reserved = reserves_namespace(&decoded);
        if (reserved || decoded_reserved)
            && (invalid_encoding
                || encoded_separator(&current)
                || has_dot_segments(&decoded)
                || !reserved
                || exact_route(&decoded).is_some())
        {
            return Classification::Rejected(StatusCode::BAD_REQUEST);
        }
        reserved |= decoded_reserved;
        if decoded == current {
            break;
        }
        current = Cow::Owned(decoded.into_owned());
    }
    if !reserved {
        Classification::NotTrace
    } else if current.contains('%') {
        Classification::Rejected(StatusCode::BAD_REQUEST)
    } else {
        Classification::Rejected(StatusCode::NOT_FOUND)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::{Mutex, Once};

    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use http::{HeaderValue, header};
    use log::{LevelFilter, Log, Metadata, Record};
    use serde_json::json;

    use crate::auth::EdgeTerminatedAuthorization;
    use crate::cache_policy::EDGE_CACHE_HEADER_NAMES;
    use crate::settings::Handler;
    use crate::test_support::tests::create_test_settings;
    use crate::trace::TraceTerminalResponse;

    fn handler(pattern: &str, username: &str, password: &str) -> Handler {
        serde_json::from_value(json!({"path": pattern, "username": username, "password": password}))
            .expect("should deserialize the example authentication handler")
    }

    fn settings(enabled: bool, pattern: Option<&str>) -> Settings {
        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config(
                "gpt_diagnostics",
                &json!({"enabled": true, "trace_page_enabled": enabled}),
            )
            .expect("should insert trace configuration");
        if let Some(pattern) = pattern {
            settings
                .handlers
                .insert(0, handler(pattern, "example-user", "example-password"));
        }
        settings
    }

    fn request(method: Method, path: &str, credentials: bool) -> Request<EdgeBody> {
        let mut builder = Request::builder()
            .method(method)
            .uri(format!("https://publisher.example.com{path}"));
        if credentials {
            builder = builder.header(
                header::AUTHORIZATION,
                format!("Basic {}", STANDARD.encode("example-user:example-password")),
            );
        }
        builder
            .body(EdgeBody::empty())
            .expect("should build trace request")
    }

    fn rejected(settings: &Settings, request: &mut Request<EdgeBody>) -> Response<EdgeBody> {
        match preflight(settings, request) {
            TracePreflight::Response(response) => response,
            _ => panic!("should terminate the rejected trace request"),
        }
    }

    fn ready(settings: &Settings, request: &mut Request<EdgeBody>) -> TraceDispatch {
        match preflight(settings, request) {
            TracePreflight::Ready(dispatch) => dispatch,
            _ => panic!("should dispatch the accepted trace request"),
        }
    }

    fn assert_private(response: &Response<EdgeBody>) {
        assert_eq!(
            response.headers()[header::CACHE_CONTROL],
            "no-store, private",
            "should enforce private non-storable responses"
        );
        for name in EDGE_CACHE_HEADER_NAMES {
            assert!(
                !response.headers().contains_key(*name),
                "should remove shared cache directive {name}"
            );
        }
        assert_eq!(
            response.headers()[header::X_CONTENT_TYPE_OPTIONS],
            "nosniff",
            "should disable MIME sniffing"
        );
        assert_eq!(
            response.headers()[header::REFERRER_POLICY],
            "no-referrer",
            "should suppress referrers"
        );
        assert_eq!(
            response.headers()[header::CONTENT_SECURITY_POLICY],
            "default-src 'none'; script-src 'self'; style-src 'self'; base-uri 'none'; object-src 'none'; frame-ancestors 'none'; form-action 'none'; connect-src 'self'; img-src data:",
            "should retain the exact trace CSP"
        );
        assert_eq!(
            response.headers()["permissions-policy"],
            "camera=(), microphone=(), geolocation=(), payment=(), usb=()",
            "should disable unrelated browser capabilities"
        );
        assert!(
            response
                .extensions()
                .get::<TraceTerminalResponse>()
                .is_some(),
            "should bypass ordinary finalizers"
        );
        assert!(
            !response.headers().contains_key(header::SET_COOKIE),
            "should not mutate cookies during preflight"
        );
    }

    #[test]
    fn authentication_precedes_disabled_path_and_method_policy() {
        for enabled in [false, true] {
            let settings = settings(enabled, Some("^/"));
            for path in [
                "/_ts/trace",
                "/_ts/trace/state",
                "/_ts/trace/enable",
                "/_ts/trace/end",
                "/_ts/trace/assets/v1.js",
                "/_ts/trace/assets/v1.css",
                "/%5Fts/trace",
                "/_ts/trace/../secret-example",
            ] {
                let mut request = request(
                    Method::from_bytes(b"TRACE-EXAMPLE").expect("should parse extension method"),
                    path,
                    false,
                );
                let response = rejected(&settings, &mut request);
                assert_eq!(
                    response.status(),
                    StatusCode::UNAUTHORIZED,
                    "should authenticate before trace policies"
                );
                assert_eq!(
                    response.headers()[header::WWW_AUTHENTICATE],
                    "Basic realm=\"Trusted Server\"",
                    "should preserve the configured challenge"
                );
                assert_eq!(
                    response.headers()[header::CONTENT_TYPE],
                    "application/json; charset=utf-8",
                    "should harden challenge MIME"
                );
                assert_private(&response);
            }
        }
    }

    #[test]
    fn literal_path_authentication_and_first_match_are_preserved() {
        let mut request = request(Method::GET, "/%5Fts/trace?secret=example-sentinel", false);
        let response = rejected(&settings(true, Some("^/_ts")), &mut request);
        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "should reject the alias without decoding the auth path"
        );
        assert!(
            !response.headers().contains_key(header::WWW_AUTHENTICATE),
            "should not manufacture an auth rule"
        );
        assert_private(&response);
        let body = response
            .into_body()
            .into_bytes()
            .expect("should return a fixed error body");
        assert!(
            !String::from_utf8_lossy(&body).contains("example-sentinel"),
            "should not reflect query or path data"
        );

        let mut request = self::request(Method::GET, "/%5Fts/trace", false);
        let response = rejected(&settings(false, Some("^/_ts")), &mut request);
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "should apply disabled feature before path errors"
        );

        let mut settings = settings(true, Some("^/"));
        settings
            .handlers
            .insert(0, handler("^/_ts/trace$", "narrow-user", "narrow-password"));
        let mut request = self::request(Method::GET, "/_ts/trace", true);
        let response = rejected(&settings, &mut request);
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "should enforce the first matching handler"
        );
    }

    #[test]
    fn method_matrix_and_head_errors_are_local_and_bodyless() {
        let settings = settings(true, None);
        for (path, allow, supported) in [
            ("/_ts/trace", "GET, HEAD", Method::GET),
            ("/_ts/trace/state", "GET, HEAD", Method::GET),
            ("/_ts/trace/enable", "POST", Method::POST),
            ("/_ts/trace/end", "POST", Method::POST),
            ("/_ts/trace/assets/v1.js", "GET, HEAD", Method::GET),
            ("/_ts/trace/assets/v1.css", "GET, HEAD", Method::GET),
        ] {
            let mut request = request(supported, path, false);
            ready(&settings, &mut request);
            for method in [
                Method::PUT,
                Method::from_bytes(b"TRACE-EXAMPLE").expect("should parse extension method"),
            ] {
                let mut request = self::request(method, path, false);
                let response = rejected(&settings, &mut request);
                assert_eq!(
                    response.status(),
                    StatusCode::METHOD_NOT_ALLOWED,
                    "should reject unsupported visible methods locally"
                );
                assert_eq!(
                    response.headers()[header::ALLOW],
                    allow,
                    "should retain path-specific allowed methods"
                );
                assert_private(&response);
            }
        }
        for (path, settings) in [
            ("/_ts/trace/enable", self::settings(true, None)),
            ("/_ts/trace/end", self::settings(true, None)),
            ("/_ts/trace/extra", self::settings(true, None)),
            ("/_ts/trace", self::settings(false, None)),
            ("/_ts/trace", self::settings(true, Some("^/"))),
        ] {
            let mut request = request(Method::HEAD, path, false);
            let response = rejected(&settings, &mut request);
            assert_private(&response);
            assert!(
                response
                    .into_body()
                    .into_bytes()
                    .expect("should return a fixed body")
                    .is_empty(),
                "should suppress every HEAD error body"
            );
        }
    }

    #[test]
    fn dynamic_finalization_preserves_response_contract_and_head_semantics() {
        let settings = settings(true, Some("^/"));
        for (path, mime) in [
            ("/_ts/trace", "text/html; charset=utf-8"),
            ("/_ts/trace/state", "application/json; charset=utf-8"),
        ] {
            for method in [Method::GET, Method::HEAD] {
                let mut request = request(method.clone(), path, true);
                let dispatch = ready(&settings, &mut request);
                assert!(
                    request
                        .extensions()
                        .get::<EdgeTerminatedAuthorization>()
                        .is_some(),
                    "should preserve successful authorization marker"
                );
                let response = Response::builder()
                    .header(header::CACHE_CONTROL, "public, max-age=60")
                    .header("surrogate-control", "max-age=60")
                    .header(header::ETAG, "\"example\"")
                    .header(header::LAST_MODIFIED, "Wed, 12 Aug 2026 00:00:00 GMT")
                    .header(header::SET_COOKIE, "unrelated=example")
                    .body(EdgeBody::from("example content"))
                    .expect("should build handler response");
                let response = dispatch.respond(response);
                assert_private(&response);
                assert_eq!(
                    response.headers()[header::CONTENT_TYPE],
                    mime,
                    "should use route-appropriate MIME"
                );
                assert!(
                    !response.headers().contains_key(header::ETAG),
                    "should remove dynamic validators"
                );
                assert!(
                    !response.headers().contains_key(header::LAST_MODIFIED),
                    "should remove dynamic validators"
                );
                if method == Method::HEAD {
                    assert!(
                        response
                            .into_body()
                            .into_bytes()
                            .expect("should return a fixed body")
                            .is_empty(),
                        "should suppress successful HEAD body"
                    );
                }
            }
        }
    }

    #[test]
    fn fixed_asset_policy_depends_on_auth_coverage_and_build_bytes() {
        for protected in [false, true] {
            let settings = settings(true, protected.then_some("^/_ts"));
            for (path, mime) in [
                (
                    "/_ts/trace/assets/v1.js",
                    "application/javascript; charset=utf-8",
                ),
                ("/_ts/trace/assets/v1.css", "text/css; charset=utf-8"),
            ] {
                let mut request = request(Method::GET, path, protected);
                let response = ready(&settings, &mut request).fixed_asset(b"example build bytes");
                assert_eq!(
                    response.headers()[header::CONTENT_TYPE],
                    mime,
                    "should serve fixed assets with exact MIME"
                );
                assert_eq!(
                    response.headers()[header::X_CONTENT_TYPE_OPTIONS],
                    "nosniff",
                    "should retain asset MIME protection"
                );
                let etag = response.headers()[header::ETAG]
                    .to_str()
                    .expect("should return an ASCII ETag");
                assert!(
                    etag.starts_with('"') && etag.ends_with('"') && !etag.starts_with("W/"),
                    "should derive a strong quoted ETag"
                );
                if protected {
                    assert_eq!(
                        response.headers()[header::CACHE_CONTROL],
                        "no-store, private",
                        "should keep protected assets private"
                    );
                } else {
                    assert_eq!(
                        response.headers()[header::CACHE_CONTROL],
                        "public, max-age=31536000, immutable",
                        "should cache only unprotected fixed assets"
                    );
                }
                let mut other = self::request(Method::HEAD, path, protected);
                let head = ready(&settings, &mut other).fixed_asset(b"example build bytes");
                assert_eq!(
                    head.headers(),
                    response.headers(),
                    "should preserve GET headers on HEAD"
                );
                assert!(
                    head.into_body()
                        .into_bytes()
                        .expect("should return a fixed body")
                        .is_empty(),
                    "should suppress asset HEAD bodies"
                );
                let mut other = self::request(Method::GET, path, protected);
                let changed =
                    ready(&settings, &mut other).fixed_asset(b"changed example build bytes");
                assert_ne!(
                    changed.headers()[header::ETAG],
                    response.headers()[header::ETAG],
                    "should derive ETag from actual build bytes"
                );
            }
        }
    }

    struct CapturedLogs(Mutex<Vec<String>>);

    impl Log for CapturedLogs {
        fn enabled(&self, _metadata: &Metadata<'_>) -> bool {
            true
        }
        fn log(&self, record: &Record<'_>) {
            let message = record.args().to_string();
            if message == "trace_auth_failed"
                || message.contains("fictional-secret-sentinel")
                || message == "Basic auth failed for path: /ordinary-example-sentinel"
            {
                let mut logs = self.0.lock().expect("should capture log messages");
                if logs.len() < 3 && !logs.contains(&message) {
                    logs.push(message);
                }
            }
        }
        fn flush(&self) {}
    }

    static LOGS: CapturedLogs = CapturedLogs(Mutex::new(Vec::new()));
    static LOG_INIT: Once = Once::new();

    #[test]
    fn trace_auth_failure_logs_a_fixed_category_without_the_path() {
        LOG_INIT.call_once(|| {
            log::set_logger(&LOGS).expect("should install the test logger");
            log::set_max_level(LevelFilter::Warn);
        });
        let mut request = request(Method::GET, "/_ts/trace/fictional-secret-sentinel", false);
        request.headers_mut().insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!(
                "Basic {}",
                STANDARD.encode("example-user:incorrect-example")
            ))
            .expect("should build incorrect credentials"),
        );
        let response = rejected(&settings(true, Some("^/")), &mut request);
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "should authenticate before reporting the reserved path error"
        );
        let logs = LOGS.0.lock().expect("should inspect captured messages");
        assert!(
            logs.iter().any(|message| message == "trace_auth_failed"),
            "should emit only a fixed diagnostic category"
        );
        assert!(
            logs.iter()
                .all(|message| !message.contains("fictional-secret-sentinel")),
            "should not log the confidential trace path"
        );
        drop(logs);
        let mut ordinary = self::request(Method::GET, "/ordinary-example-sentinel", false);
        ordinary.headers_mut().insert(
            header::AUTHORIZATION,
            request.headers()[header::AUTHORIZATION].clone(),
        );
        assert!(
            enforce_basic_auth(&settings(true, Some("^/")), &mut ordinary)
                .expect("should evaluate ordinary auth")
                .is_some(),
            "should retain ordinary credential rejection"
        );
        assert!(
            LOGS.0
                .lock()
                .expect("should inspect ordinary auth messages")
                .iter()
                .any(|message| message == "Basic auth failed for path: /ordinary-example-sentinel"),
            "should retain ordinary path logging outside trace"
        );
        let body = response
            .into_body()
            .into_bytes()
            .expect("should return a fixed challenge");
        assert!(
            !String::from_utf8_lossy(&body).contains("fictional-secret-sentinel"),
            "should not reflect the trace path in a challenge"
        );
    }

    #[test]
    fn visible_trace_prefix_cannot_be_erased_by_dot_segments() {
        for path in [
            "/_ts//trace/../ordinary",
            "//_ts/trace/../../health",
            "/%5Fts//trace/../ordinary",
            "/_ts%2Ftrace/../ordinary",
            "/ordinary/../_ts//trace/../../health",
        ] {
            assert_eq!(
                classify_path(path),
                Classification::Rejected(StatusCode::BAD_REQUEST),
                "should retain the visible reserved prefix before later dot segments {path}"
            );
            assert!(
                is_trace_path(path),
                "should suppress native shortcuts for visible reserved ambiguity {path}"
            );
            let mut request = request(Method::GET, path, false);
            let response = rejected(&settings(true, None), &mut request);
            assert_eq!(
                response.status(),
                StatusCode::BAD_REQUEST,
                "should terminate the visible ambiguity locally"
            );
            assert_private(&response);
        }
    }

    #[test]
    fn encoded_namespace_with_unreadable_suffix_never_falls_through() {
        for path in [
            "/%5Fts/trace/%GG",
            "/%255Fts/trace/%GG",
            "/%5Fts/trace/%FF",
            "/%255Fts/trace/%FF",
        ] {
            assert_eq!(
                classify_path(path),
                Classification::Rejected(StatusCode::BAD_REQUEST),
                "should reserve the encoded namespace despite an unreadable suffix {path}"
            );
        }
    }

    #[test]
    fn unrelated_preflight_leaves_the_request_untouched() {
        let mut settings = settings(true, None);
        settings.integrations.insert(
            "gpt_diagnostics".to_string(),
            json!({"enabled": "invalid-example"}),
        );
        let mut request = request(Method::GET, "/article?ts_console=1", true);
        request.headers_mut().insert(
            header::COOKIE,
            HeaderValue::from_static("__Host-ts-console=1; ts-ec=example"),
        );
        let before_uri = request.uri().clone();
        let before_headers = request.headers().clone();
        assert!(
            matches!(preflight(&settings, &mut request), TracePreflight::NotTrace),
            "should continue ordinary dispatch without parsing trace configuration"
        );
        assert_eq!(request.uri(), &before_uri, "should retain the ordinary URI");
        assert_eq!(
            request.headers(),
            &before_headers,
            "should retain ordinary headers"
        );
        assert!(
            request.extensions().get::<TraceAuthLogPolicy>().is_none(),
            "should retain ordinary auth logging"
        );
        assert!(
            request
                .extensions()
                .get::<EdgeTerminatedAuthorization>()
                .is_none(),
            "should not authenticate ordinary requests early"
        );
    }

    #[test]
    fn authentication_failure_does_not_read_the_body() {
        let mut request = request(Method::POST, "/_ts/trace/enable", false);
        *request.body_mut() = EdgeBody::stream(futures::stream::poll_fn(
            |_| -> std::task::Poll<Option<bytes::Bytes>> {
                panic!("should never inspect a body before authentication");
            },
        ));
        let response = rejected(&settings(true, Some("^/")), &mut request);
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "should authenticate without consuming the stream"
        );
    }

    #[test]
    fn trace_actions_state_response_uses_only_frozen_session_health() {
        for (field, active) in [
            ("__Host-ts-console=1", true),
            ("__Host-ts-console=invalid-example", false),
            ("__Host-ts-console", false),
            ("__Host-ts-console=1; __Host-ts-console=1", false),
            ("__Host-ts-console=1; unrelated=a,b", false),
            ("__Host-ts-console=1; unrelated=�", false),
            ("", false),
        ] {
            for method in [Method::GET, Method::HEAD] {
                let mut request = request(method.clone(), "/_ts/trace/state?ts_console=1", false);
                request.headers_mut().insert(
                    header::COOKIE,
                    HeaderValue::from_bytes(field.as_bytes())
                        .expect("should construct frozen runtime-visible Cookie text"),
                );
                let frozen = super::super::inspect_cookies(request.headers(), None);
                request.headers_mut().remove(header::COOKIE);
                let context = ready(&settings(true, None), &mut request);
                let response = context.respond(state_response(&frozen));
                assert_eq!(
                    response.status(),
                    StatusCode::OK,
                    "should observe the frozen state locally"
                );
                assert_private(&response);
                assert!(
                    !response.headers().contains_key(header::SET_COOKIE),
                    "should never refresh or clear a cookie on state requests"
                );
                let body = response.into_body();
                if method == Method::HEAD {
                    assert!(
                        body.into_bytes()
                            .expect("should return a bounded HEAD response")
                            .is_empty(),
                        "should remove every state HEAD body"
                    );
                } else {
                    assert_eq!(
                        body.to_json::<serde_json::Value>()
                            .expect("should parse exact observed state JSON"),
                        json!({"observed_active":active}),
                        "should emit only the frozen observed_active boolean"
                    );
                }
            }
        }
    }

    #[test]
    fn fixed_asset_queries_do_not_affect_bytes_or_headers() {
        let settings = settings(true, None);
        let mut plain = request(Method::GET, "/_ts/trace/assets/v1.js", false);
        let mut queried = request(
            Method::GET,
            "/_ts/trace/assets/v1.js?secret=fictional-query-sentinel",
            false,
        );
        let plain = ready(&settings, &mut plain).fixed_asset(b"example build bytes");
        let queried = ready(&settings, &mut queried).fixed_asset(b"example build bytes");
        assert_eq!(
            queried.headers(),
            plain.headers(),
            "should exclude query data from static headers"
        );
        assert_eq!(
            queried.into_body().into_bytes(),
            plain.into_body().into_bytes(),
            "should exclude query data from static bytes"
        );
    }

    #[test]
    fn exact_routes_and_reserved_shapes_are_classified_without_normalization() {
        for (path, route) in [
            ("/_ts/trace", TraceRoute::Shell),
            ("/_ts/trace/state", TraceRoute::State),
            ("/_ts/trace/enable", TraceRoute::Enable),
            ("/_ts/trace/end", TraceRoute::End),
            ("/_ts/trace/assets/v1.js", TraceRoute::Javascript),
            ("/_ts/trace/assets/v1.css", TraceRoute::Stylesheet),
        ] {
            assert_eq!(
                classify_path(path),
                Classification::Route(route),
                "should classify the exact route {path}"
            );
        }
        for path in [
            "/_ts/trace/",
            "/_ts/trace/state/extra",
            "/_ts/trace-extra",
            "/_ts/tracer",
            "/_ts/trace/assets/v2.js",
            "//_ts/trace",
            "/_ts//trace",
            "/_ts/trace//state",
        ] {
            assert_eq!(
                classify_path(path),
                Classification::Rejected(StatusCode::NOT_FOUND),
                "should reserve the unsupported spelling {path}"
            );
        }
        for path in [
            "/%5Fts/trace",
            "/_ts/%74race",
            "/_ts/trace%2Fend",
            "/_ts/trace%252Fend",
            "/_ts/trace/../ordinary",
            "/_ts/./trace",
            "/ordinary/../_ts/trace",
            "/_ts/trace/%2e%2e/ordinary",
            "/_ts/trace%GG",
            "/_ts/trace%FF",
            "/_ts/trace%252525252Fend",
        ] {
            assert_eq!(
                classify_path(path),
                Classification::Rejected(StatusCode::BAD_REQUEST),
                "should reject the ambiguous spelling {path}"
            );
        }
        for path in [
            "/",
            "/article",
            "/_ts/debug/ja4",
            "/health",
            "/article%2Fother",
            "/article%GG",
            "/%FF",
            "/_ts/not-trace",
            "/_ts/../ordinary",
        ] {
            assert_eq!(
                classify_path(path),
                Classification::NotTrace,
                "should preserve unrelated traffic {path}"
            );
        }
    }
}
