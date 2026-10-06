//! Deliberate, authenticated trace cookie mutations.

use edgezero_core::body::Body as EdgeBody;
use edgezero_core::request::RequestIngress;
use futures::StreamExt as _;
use http::{HeaderMap, HeaderName, HeaderValue, Request, Response, StatusCode, header};
use url::Url;

use crate::integrations::gpt_diagnostics::GptDiagnosticsCookieAction;

use super::routes::TraceRoute;

const ACTION_HEADER: HeaderName = HeaderName::from_static("x-ts-trace-action");
const FETCH_SITE_HEADER: HeaderName = HeaderName::from_static("sec-fetch-site");

/// Validate one deliberate mutation after authenticated route preflight.
///
/// The caller must apply [`super::TraceDispatch::respond`] to every result.
/// Cookie health never prevents a valid enable or end request.
///
/// # Performance
///
/// Rejected controls and framing never poll the body. Streaming validation
/// skips empty chunks and stops on the first nonempty chunk or read error;
/// it does not allocate a body buffer or introduce a transport deadline.
pub(crate) async fn action_response(
    request: &mut Request<EdgeBody>,
    route: TraceRoute,
) -> Response<EdgeBody> {
    let (action, cookie_action) = match route {
        TraceRoute::Enable => (b"enable".as_slice(), GptDiagnosticsCookieAction::SetSession),
        TraceRoute::End => (b"end".as_slice(), GptDiagnosticsCookieAction::ClearSession),
        _ => return failure(StatusCode::INTERNAL_SERVER_ERROR),
    };
    if request.uri().query().is_some()
        || single_header(request.headers(), ACTION_HEADER).map(HeaderValue::as_bytes)
            != Some(action)
        || single_header(request.headers(), FETCH_SITE_HEADER).map(HeaderValue::as_bytes)
            != Some(b"same-origin".as_slice())
        || !matches_trusted_origin(request)
    {
        return failure(StatusCode::FORBIDDEN);
    }
    if !accepts_empty_framing(request.headers()) {
        return failure(StatusCode::PAYLOAD_TOO_LARGE);
    }
    let body = std::mem::replace(request.body_mut(), EdgeBody::empty());
    if let Some(status) = empty_body_rejection(body).await {
        return failure(status);
    }
    let Some(cookie) = cookie_action.set_cookie_header() else {
        return failure(StatusCode::INTERNAL_SERVER_ERROR);
    };
    let mut response = Response::new(EdgeBody::from(br#"{"mutation_requested":true}"#.as_slice()));
    response.headers_mut().insert(header::SET_COOKIE, cookie);
    response
}

fn failure(status: StatusCode) -> Response<EdgeBody> {
    let body: &'static [u8] = match status {
        StatusCode::FORBIDDEN => br#"{"error":"trace action rejected"}"#,
        StatusCode::PAYLOAD_TOO_LARGE => br#"{"error":"trace body must be empty"}"#,
        StatusCode::BAD_REQUEST => br#"{"error":"trace body unavailable"}"#,
        _ => br#"{"error":"trace action unavailable"}"#,
    };
    let mut response = Response::new(EdgeBody::from(body));
    *response.status_mut() = status;
    response
}

fn single_header(headers: &HeaderMap, name: HeaderName) -> Option<&HeaderValue> {
    let mut fields = headers.get_all(name).iter();
    let value = fields.next()?;
    fields.next().is_none().then_some(value)
}

fn matches_trusted_origin(request: &Request<EdgeBody>) -> bool {
    let Some(trusted) = request
        .extensions()
        .get::<RequestIngress>()
        .and_then(RequestIngress::origin)
    else {
        return false;
    };
    let Some(expected) = canonical_authority_origin(trusted.scheme(), trusted.authority()) else {
        return false;
    };
    let Some(origin) = single_header(request.headers(), header::ORIGIN)
        .and_then(|origin| core::str::from_utf8(origin.as_bytes()).ok())
        .and_then(canonical_origin)
    else {
        return false;
    };
    if origin != expected
        || request
            .uri()
            .scheme_str()
            .is_some_and(|scheme| !scheme.eq_ignore_ascii_case(trusted.scheme()))
    {
        return false;
    }
    let authority = request.uri().authority();
    if authority.is_some_and(|authority| {
        canonical_authority_origin(trusted.scheme(), authority.as_str()).as_ref() != Some(&expected)
    }) {
        return false;
    }
    match single_header(request.headers(), header::HOST) {
        Some(host) => {
            core::str::from_utf8(host.as_bytes())
                .ok()
                .and_then(|host| canonical_authority_origin(trusted.scheme(), host))
                .as_ref()
                == Some(&expected)
        }
        None if request.headers().contains_key(header::HOST) => false,
        None => request.uri().scheme().is_some() && authority.is_some(),
    }
}

fn canonical_origin(value: &str) -> Option<String> {
    let (scheme, authority) = value.split_once("://")?;
    canonical_authority_origin(scheme, authority)
}

fn canonical_authority_origin(scheme: &str, authority: &str) -> Option<String> {
    if !(scheme.eq_ignore_ascii_case("https") || scheme.eq_ignore_ascii_case("http"))
        || authority.is_empty()
        || authority.chars().any(|character| {
            character.is_whitespace()
                || character.is_control()
                || matches!(character, '/' | '?' | '#' | '\\' | '@' | ',')
        })
        || !valid_port(authority)
    {
        return None;
    }
    let url = Url::parse(&format!("{scheme}://{authority}")).ok()?;
    if url.host_str().is_none_or(str::is_empty) {
        return None;
    }
    Some(url.origin().ascii_serialization())
}

fn valid_port(authority: &str) -> bool {
    let port = if authority.starts_with('[') {
        let Some(end) = authority.find(']') else {
            return false;
        };
        let suffix = &authority[end + 1..];
        if suffix.is_empty() {
            return true;
        }
        let Some(port) = suffix.strip_prefix(':') else {
            return false;
        };
        Some(port)
    } else {
        authority.split_once(':').map(|(_, port)| port)
    };
    port.is_none_or(|port| {
        !port.is_empty()
            && port.bytes().all(|byte| byte.is_ascii_digit())
            && port.parse::<u16>().is_ok()
    })
}

fn accepts_empty_framing(headers: &HeaderMap) -> bool {
    if headers.contains_key(header::TRANSFER_ENCODING) {
        return false;
    }
    let mut lengths = headers.get_all(header::CONTENT_LENGTH).iter();
    let Some(length) = lengths.next() else {
        return true;
    };
    lengths.next().is_none()
        && !length.as_bytes().is_empty()
        && length.as_bytes().iter().all(|byte| *byte == b'0')
}

async fn empty_body_rejection(body: EdgeBody) -> Option<StatusCode> {
    match body {
        EdgeBody::Once(bytes) if bytes.is_empty() => None,
        EdgeBody::Once(_) => Some(StatusCode::PAYLOAD_TOO_LARGE),
        EdgeBody::Stream(mut stream) => {
            while let Some(chunk) = stream.next().await {
                match chunk {
                    Ok(bytes) if bytes.is_empty() => {}
                    Ok(_) => return Some(StatusCode::PAYLOAD_TOO_LARGE),
                    Err(_) => return Some(StatusCode::BAD_REQUEST),
                }
            }
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::io;
    use std::rc::Rc;
    use std::task::Poll;

    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use bytes::Bytes;
    use edgezero_core::body::Body as EdgeBody;
    use edgezero_core::request::{
        CapturedTarget, HeaderFidelity, InboundOrigin, OriginSource, RequestIngress,
        TargetUnavailable,
    };
    use futures::{executor::block_on, stream};
    use http::{HeaderName, HeaderValue, Method, Request, Response, StatusCode, header};
    use serde_json::{Value, json};

    use crate::settings::{Handler, Settings};
    use crate::test_support::tests::create_test_settings;
    use crate::trace::{TracePreflight, TraceTerminalResponse, inspect_cookies, preflight};

    fn settings(enabled: bool, auth: bool) -> Settings {
        let mut settings = create_test_settings();
        settings.integrations.insert(
            "gpt_diagnostics".to_owned(),
            json!({"enabled":true, "trace_page_enabled":enabled}),
        );
        if auth {
            let handler: Handler = serde_json::from_value(
                json!({"path":"^/", "username":"example-user", "password":"example-password"}),
            )
            .expect("should create an example authentication rule");
            settings.handlers.insert(0, handler);
        }
        settings
    }

    fn metadata(scheme: &str, authority: &str) -> RequestIngress {
        RequestIngress::new(
            CapturedTarget::Unavailable(TargetUnavailable::NotExposed),
            Some(
                InboundOrigin::parse(scheme, authority, OriginSource::RuntimeUri)
                    .expect("should validate the trusted fictional origin"),
            ),
            HeaderFidelity::default(),
            vec![],
        )
        .expect("should construct origin-only ingress metadata")
    }

    fn request(action: &str) -> Request<EdgeBody> {
        let mut request = Request::builder()
            .method(Method::POST)
            .uri(format!("/_ts/trace/{action}"))
            .header(header::HOST, "publisher.example")
            .header(header::ORIGIN, "https://publisher.example")
            .header("sec-fetch-site", "same-origin")
            .header("x-ts-trace-action", action)
            .body(EdgeBody::empty())
            .expect("should create a deliberate action request");
        request
            .extensions_mut()
            .insert(metadata("https", "publisher.example"));
        request
    }

    fn respond(settings: &Settings, mut request: Request<EdgeBody>) -> Response<EdgeBody> {
        match preflight(settings, &mut request) {
            TracePreflight::Response(response) => response,
            TracePreflight::Ready(context) => {
                let response = block_on(super::super::action_response(&mut request, context.route));
                context.respond(response)
            }
            TracePreflight::NotTrace => panic!("should preflight every exact action route locally"),
        }
    }

    fn private_error(response: Response<EdgeBody>, expected: StatusCode) {
        assert_eq!(
            response.status(),
            expected,
            "should return the exact bounded action failure status"
        );
        assert_eq!(
            response.headers()[header::CACHE_CONTROL],
            "no-store, private",
            "should harden action errors locally"
        );
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "application/json; charset=utf-8",
            "should use bounded JSON errors"
        );
        assert_eq!(
            response.headers()[header::X_CONTENT_TYPE_OPTIONS],
            "nosniff",
            "should preserve content type hardening"
        );
        assert_eq!(
            response
                .headers()
                .get_all(header::SET_COOKIE)
                .iter()
                .count(),
            0,
            "should never mutate cookies on rejection"
        );
        assert!(
            response
                .extensions()
                .get::<TraceTerminalResponse>()
                .is_some(),
            "should bypass ordinary lifecycle finalization"
        );
        let body = response
            .into_body()
            .into_bytes()
            .expect("should return a bounded buffered error");
        assert!(
            body.len() < 128 && !String::from_utf8_lossy(&body).contains("fictional-secret"),
            "should expose no controls, stream errors or parser text"
        );
    }

    fn unpolled_body(polls: Rc<Cell<usize>>) -> EdgeBody {
        EdgeBody::stream(stream::poll_fn(move |_| -> Poll<Option<Bytes>> {
            polls.set(polls.get() + 1);
            panic!("should reject unsafe headers before polling a body");
        }))
    }

    fn assert_mutation(response: Response<EdgeBody>, action: &str) {
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "should accept only the complete deliberate action"
        );
        let values: Vec<_> = response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .collect();
        assert_eq!(
            values.len(),
            1,
            "should request exactly one host-only cookie mutation"
        );
        let expected = if action == "enable" {
            "__Host-ts-console=1; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age=1800"
        } else {
            "__Host-ts-console=; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age=0"
        };
        assert_eq!(
            values[0], expected,
            "should reuse the exact shared cookie lifetime and scope"
        );
        assert_eq!(
            response.headers()[header::CACHE_CONTROL],
            "no-store, private",
            "should keep mutation responses private"
        );
        let value: Value = response
            .into_body()
            .to_json()
            .expect("should parse a bounded mutation result");
        assert_eq!(
            value,
            json!({"mutation_requested":true}),
            "should claim only the requested mutation"
        );
    }

    #[test]
    fn trace_actions_accept_enable_and_end_independently_of_cookie_health() {
        for action in ["enable", "end"] {
            for field in [
                None,
                Some("__Host-ts-console=invalid-example"),
                Some("__Host-ts-console=1; __Host-ts-console=1"),
                Some("__Host-ts-console=1; unrelated=a,b"),
                Some("__Host-ts-console=1; unrelated=�"),
            ] {
                let mut request = request(action);
                if let Some(field) = field {
                    request.headers_mut().insert(
                        header::COOKIE,
                        HeaderValue::from_bytes(field.as_bytes())
                            .expect("should construct runtime-visible Cookie text"),
                    );
                }
                let frozen = inspect_cookies(
                    request.headers(),
                    request.extensions().get::<RequestIngress>(),
                );
                assert!(
                    !frozen.observed_active(),
                    "should keep invalid or ambiguous follow-up observation inactive"
                );
                assert_mutation(respond(&settings(true, false), request), action);
                for _ in 0..2 {
                    let state = super::super::state_response(&frozen);
                    assert_eq!(
                        state
                            .into_body()
                            .to_json::<Value>()
                            .expect("should decode the separate observation"),
                        json!({"observed_active":false}),
                        "should not infer browser activation from a successful mutation request"
                    );
                }
            }
        }
    }

    #[test]
    fn trace_actions_origin_canonicalization_accepts_case_default_ports_and_ipv6() {
        for (scheme, authority, origin, host) in [
            (
                "https",
                "publisher.example",
                "HTTPS://PUBLISHER.EXAMPLE:443",
                "PUBLISHER.EXAMPLE:443",
            ),
            (
                "http",
                "publisher.example",
                "http://PUBLISHER.EXAMPLE:80",
                "publisher.example:80",
            ),
            (
                "https",
                "publisher.example:8443",
                "https://PUBLISHER.EXAMPLE:8443",
                "publisher.example:8443",
            ),
            (
                "https",
                "[2001:db8::1]",
                "https://[2001:0DB8:0:0:0:0:0:1]:443",
                "[2001:DB8::1]:443",
            ),
        ] {
            let mut request = request("enable");
            request.extensions_mut().insert(metadata(scheme, authority));
            request.headers_mut().insert(
                header::ORIGIN,
                HeaderValue::from_str(origin)
                    .expect("should construct a canonical-equivalent origin"),
            );
            request.headers_mut().insert(
                header::HOST,
                HeaderValue::from_str(host).expect("should construct a canonical-equivalent host"),
            );
            assert_mutation(respond(&settings(true, false), request), "enable");
        }
    }

    #[test]
    fn trace_actions_origin_syntax_is_rejected_before_url_normalization_and_body_polling() {
        for origin in [
            "https://publisher.example/",
            "https://publisher.example/path",
            "https://publisher.example?",
            "https://publisher.example?fictional-secret=1",
            "https://publisher.example#",
            "https://publisher.example#fictional-secret",
            "https://@publisher.example",
            "https://example-user@publisher.example",
            "https://publisher.example\\",
            "https://publisher.example,https://publisher.example",
            " https://publisher.example",
            "https://publisher.example ",
            "https://publisher.example\t",
            "https://publisher.example:",
            "https://publisher.example:+443",
            "https://publisher.example:65536",
            "null",
            "file://publisher.example",
            "https://",
            "https:///publisher.example",
            "https:publisher.example",
            "http://publisher.example",
            "https://other.example",
            "https://publisher.example:8443",
        ] {
            let polls = Rc::new(Cell::new(0));
            let mut request = request("enable");
            request.headers_mut().insert(
                header::ORIGIN,
                HeaderValue::from_str(origin)
                    .expect("should construct the visible malformed origin"),
            );
            *request.body_mut() = unpolled_body(Rc::clone(&polls));
            private_error(
                respond(&settings(true, false), request),
                StatusCode::FORBIDDEN,
            );
            assert_eq!(
                polls.get(),
                0,
                "should reject the entire Origin value before body inspection"
            );
        }
    }

    #[test]
    fn trace_actions_controls_require_exact_single_values_and_no_query() {
        for name in ["origin", "sec-fetch-site", "x-ts-trace-action"] {
            let mut request = request("enable");
            request.headers_mut().remove(name);
            private_error(
                respond(&settings(true, false), request),
                StatusCode::FORBIDDEN,
            );
            for second in [
                if name == "origin" {
                    "https://publisher.example"
                } else if name == "sec-fetch-site" {
                    "same-origin"
                } else {
                    "enable"
                },
                "fictional-secret-conflict",
            ] {
                let mut request = self::request("enable");
                request.headers_mut().append(
                    HeaderName::from_bytes(name.as_bytes())
                        .expect("should construct the control name"),
                    HeaderValue::from_str(second).expect("should construct a repeated control"),
                );
                private_error(
                    respond(&settings(true, false), request),
                    StatusCode::FORBIDDEN,
                );
            }
        }
        for (name, value) in [
            ("sec-fetch-site", "same-site"),
            ("sec-fetch-site", "Same-Origin"),
            ("sec-fetch-site", "same-origin, same-origin"),
            ("sec-fetch-site", " same-origin"),
            ("x-ts-trace-action", "end"),
            ("x-ts-trace-action", "Enable"),
            ("x-ts-trace-action", "enable, enable"),
            ("x-ts-trace-action", "enable "),
        ] {
            let mut request = request("enable");
            request.headers_mut().insert(
                HeaderName::from_bytes(name.as_bytes()).expect("should construct the control name"),
                HeaderValue::from_str(value).expect("should construct a visible control value"),
            );
            private_error(
                respond(&settings(true, false), request),
                StatusCode::FORBIDDEN,
            );
        }
        for target in [
            "/_ts/trace/enable?",
            "/_ts/trace/enable?ts_console=1",
            "/_ts/trace/enable?fictional-secret=1",
        ] {
            let mut request = request("enable");
            *request.uri_mut() = target.parse().expect("should retain the visible query");
            private_error(
                respond(&settings(true, false), request),
                StatusCode::FORBIDDEN,
            );
        }
    }

    #[test]
    fn trace_actions_host_is_single_unfolded_valid_and_matches_trusted_authority() {
        for value in [
            "other.example",
            "publisher.example:8443",
            "publisher.example:",
            "publisher.example:+443",
            "publisher.example:65536",
            "publisher.example, publisher.example",
            "publisher.example/",
            "publisher.example?",
            "publisher.example#",
            "@publisher.example",
            "publisher.example\\",
            " publisher.example",
            "publisher.example ",
            "",
        ] {
            let mut request = request("enable");
            request.headers_mut().insert(
                header::HOST,
                HeaderValue::from_str(value).expect("should construct the visible malformed Host"),
            );
            private_error(
                respond(&settings(true, false), request),
                StatusCode::FORBIDDEN,
            );
        }
        for value in ["publisher.example", "other.example"] {
            let mut request = request("enable");
            request.headers_mut().append(
                header::HOST,
                HeaderValue::from_str(value).expect("should construct the second Host"),
            );
            private_error(
                respond(&settings(true, false), request),
                StatusCode::FORBIDDEN,
            );
        }
        let mut request = request("enable");
        request.headers_mut().remove(header::HOST);
        private_error(
            respond(&settings(true, false), request),
            StatusCode::FORBIDDEN,
        );
    }

    #[test]
    fn trace_actions_absolute_uri_requires_all_present_facts_to_agree() {
        for host in [None, Some("PUBLISHER.EXAMPLE:443")] {
            let mut request = request("enable");
            *request.uri_mut() = "https://PUBLISHER.EXAMPLE:443/_ts/trace/enable"
                .parse()
                .expect("should build a matching absolute URI");
            if let Some(host) = host {
                request.headers_mut().insert(
                    header::HOST,
                    HeaderValue::from_str(host).expect("should construct the matching Host"),
                );
            } else {
                request.headers_mut().remove(header::HOST);
            }
            assert_mutation(respond(&settings(true, false), request), "enable");
        }
        for target in [
            "http://publisher.example/_ts/trace/enable",
            "https://other.example/_ts/trace/enable",
            "https://publisher.example:8443/_ts/trace/enable",
            "ftp://publisher.example/_ts/trace/enable",
            "https://@publisher.example/_ts/trace/enable",
        ] {
            let mut request = request("enable");
            *request.uri_mut() = target
                .parse()
                .expect("should construct conflicting visible absolute URI facts");
            private_error(
                respond(&settings(true, false), request),
                StatusCode::FORBIDDEN,
            );
        }
        let mut request = request("enable");
        *request.uri_mut() = "https://publisher.example/_ts/trace/enable"
            .parse()
            .expect("should construct an absolute request");
        request
            .headers_mut()
            .insert(header::HOST, HeaderValue::from_static("other.example"));
        private_error(
            respond(&settings(true, false), request),
            StatusCode::FORBIDDEN,
        );
    }

    #[test]
    fn trace_actions_forwarded_headers_and_runtime_compatibility_cannot_supply_trust() {
        for missing_origin in [false, true] {
            let mut request = request("enable");
            request.extensions_mut().remove::<RequestIngress>();
            if !missing_origin {
                request.extensions_mut().insert(
                    RequestIngress::new(
                        CapturedTarget::Unavailable(TargetUnavailable::NotExposed),
                        None,
                        HeaderFidelity::default(),
                        vec![],
                    )
                    .expect("should construct metadata without origin trust"),
                );
            }
            for (name, value) in [
                ("forwarded", "proto=https;host=publisher.example"),
                ("x-forwarded-proto", "https"),
                ("x-forwarded-host", "publisher.example"),
                ("x-ts-original-scheme", "https"),
            ] {
                request.headers_mut().insert(
                    HeaderName::from_bytes(name.as_bytes())
                        .expect("should construct an untrusted compatibility header name"),
                    HeaderValue::from_static(value),
                );
            }
            private_error(
                respond(&settings(true, false), request),
                StatusCode::FORBIDDEN,
            );
        }
    }

    #[test]
    fn trace_actions_header_failures_never_poll_a_hostile_stream() {
        for (name, value, status) in [
            ("origin", "https://other.example", StatusCode::FORBIDDEN),
            (
                "x-ts-trace-action",
                "invalid-example",
                StatusCode::FORBIDDEN,
            ),
            ("sec-fetch-site", "cross-site", StatusCode::FORBIDDEN),
            ("host", "other.example", StatusCode::FORBIDDEN),
            ("transfer-encoding", "", StatusCode::PAYLOAD_TOO_LARGE),
            (
                "transfer-encoding",
                "chunked",
                StatusCode::PAYLOAD_TOO_LARGE,
            ),
            ("content-length", "1", StatusCode::PAYLOAD_TOO_LARGE),
            ("content-length", "01", StatusCode::PAYLOAD_TOO_LARGE),
            ("content-length", "+0", StatusCode::PAYLOAD_TOO_LARGE),
            ("content-length", "-0", StatusCode::PAYLOAD_TOO_LARGE),
            ("content-length", "0,0", StatusCode::PAYLOAD_TOO_LARGE),
            ("content-length", " 0", StatusCode::PAYLOAD_TOO_LARGE),
            ("content-length", "0 ", StatusCode::PAYLOAD_TOO_LARGE),
            ("content-length", "", StatusCode::PAYLOAD_TOO_LARGE),
            ("content-length", "zero", StatusCode::PAYLOAD_TOO_LARGE),
        ] {
            let polls = Rc::new(Cell::new(0));
            let mut request = request("enable");
            request.headers_mut().insert(
                HeaderName::from_bytes(name.as_bytes())
                    .expect("should construct the visible header name"),
                HeaderValue::from_str(value).expect("should construct the malformed visible field"),
            );
            *request.body_mut() = unpolled_body(Rc::clone(&polls));
            private_error(respond(&settings(true, false), request), status);
            assert_eq!(
                polls.get(),
                0,
                "should reject headers without polling the stream"
            );
        }
        for second in ["0", "1"] {
            let polls = Rc::new(Cell::new(0));
            let mut request = request("enable");
            request
                .headers_mut()
                .append(header::CONTENT_LENGTH, HeaderValue::from_static("0"));
            request
                .headers_mut()
                .append(header::CONTENT_LENGTH, HeaderValue::from_static(second));
            *request.body_mut() = unpolled_body(Rc::clone(&polls));
            private_error(
                respond(&settings(true, false), request),
                StatusCode::PAYLOAD_TOO_LARGE,
            );
            assert_eq!(
                polls.get(),
                0,
                "should reject visible repeated lengths without body polls"
            );
        }
    }

    #[test]
    fn trace_actions_empty_buffered_body_accepts_absent_and_all_zero_lengths() {
        for length in [None, Some("0"), Some("00"), Some("000")] {
            let mut request = request("end");
            if let Some(length) = length {
                request
                    .headers_mut()
                    .insert(header::CONTENT_LENGTH, HeaderValue::from_static(length));
            }
            assert_mutation(respond(&settings(true, false), request), "end");
        }
    }

    #[test]
    fn trace_actions_nonempty_buffered_body_rejects_even_with_zero_framing() {
        for length in [None, Some("0"), Some("000")] {
            let mut request = request("end");
            *request.body_mut() = EdgeBody::from("fictional-secret-body");
            if let Some(length) = length {
                request
                    .headers_mut()
                    .insert(header::CONTENT_LENGTH, HeaderValue::from_static(length));
            }
            private_error(
                respond(&settings(true, false), request),
                StatusCode::PAYLOAD_TOO_LARGE,
            );
        }
    }

    #[test]
    fn trace_actions_stream_requires_clean_eof_after_empty_chunks() {
        let polls = Rc::new(Cell::new(0));
        let captured = Rc::clone(&polls);
        let mut request = request("enable");
        *request.body_mut() = EdgeBody::stream(stream::poll_fn(move |_| {
            captured.set(captured.get() + 1);
            Poll::Ready(if captured.get() <= 2 {
                Some(Bytes::new())
            } else {
                None
            })
        }));
        assert_mutation(respond(&settings(true, false), request), "enable");
        assert_eq!(
            polls.get(),
            3,
            "should skip empty chunks and prove clean EOF"
        );
    }

    #[test]
    fn trace_actions_stream_stops_on_first_nonempty_chunk_without_later_polling() {
        let polls = Rc::new(Cell::new(0));
        let captured = Rc::clone(&polls);
        let mut request = request("enable");
        *request.body_mut() = EdgeBody::stream(stream::poll_fn(move |_| {
            captured.set(captured.get() + 1);
            Poll::Ready(Some(match captured.get() {
                1 => Bytes::new(),
                2 => Bytes::from_static(b"x"),
                _ => panic!("should not poll after the first nonempty body chunk"),
            }))
        }));
        private_error(
            respond(&settings(true, false), request),
            StatusCode::PAYLOAD_TOO_LARGE,
        );
        assert_eq!(polls.get(), 2, "should stop at the first real byte");
    }

    #[test]
    fn trace_actions_stream_error_is_bounded_bad_request_without_later_polling() {
        let polls = Rc::new(Cell::new(0));
        let captured = Rc::clone(&polls);
        let mut request = request("end");
        *request.body_mut() = EdgeBody::from_stream(stream::poll_fn(
            move |_| -> Poll<Option<Result<Bytes, io::Error>>> {
                captured.set(captured.get() + 1);
                Poll::Ready(Some(match captured.get() {
                    1 => Ok(Bytes::new()),
                    2 => Err(io::Error::other("fictional-secret-stream-error")),
                    _ => panic!("should not poll after the stream error"),
                }))
            },
        ));
        private_error(
            respond(&settings(true, false), request),
            StatusCode::BAD_REQUEST,
        );
        assert_eq!(
            polls.get(),
            2,
            "should require successful EOF and stop after error"
        );
    }

    #[test]
    fn trace_actions_preflight_authentication_precedes_controls_flag_and_body() {
        for enabled in [false, true] {
            for credentials in [
                None,
                Some("example-user:incorrect-example"),
                Some("example-user:example-password"),
            ] {
                let polls = Rc::new(Cell::new(0));
                let mut request = request("enable");
                request.headers_mut().remove(header::ORIGIN);
                *request.body_mut() = unpolled_body(Rc::clone(&polls));
                if let Some(credentials) = credentials {
                    request.headers_mut().insert(
                        header::AUTHORIZATION,
                        HeaderValue::from_str(&format!("Basic {}", STANDARD.encode(credentials)))
                            .expect("should create fictional credentials"),
                    );
                }
                let response = respond(&settings(enabled, true), request);
                let expected = if credentials != Some("example-user:example-password") {
                    StatusCode::UNAUTHORIZED
                } else if !enabled {
                    StatusCode::NOT_FOUND
                } else {
                    StatusCode::FORBIDDEN
                };
                assert_eq!(
                    response.headers().contains_key(header::WWW_AUTHENTICATE),
                    expected == StatusCode::UNAUTHORIZED,
                    "should preserve challenges only when auth rejects"
                );
                private_error(response, expected);
                assert_eq!(
                    polls.get(),
                    0,
                    "should not read bodies before successful auth and header validation"
                );
            }
        }
        let mut request = request("enable");
        *request.method_mut() = Method::HEAD;
        request.headers_mut().remove(header::ORIGIN);
        let response = respond(&settings(true, false), request);
        assert_eq!(
            response.status(),
            StatusCode::METHOD_NOT_ALLOWED,
            "should reject unsupported methods before action controls"
        );
        assert_eq!(
            response.headers()[header::ALLOW],
            "POST",
            "should retain path-specific Allow"
        );
        assert!(
            response
                .into_body()
                .into_bytes()
                .expect("should return a local HEAD error")
                .is_empty(),
            "should keep HEAD action errors bodyless"
        );
    }
}
