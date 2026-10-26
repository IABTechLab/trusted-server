//! What a provider sends and receives. The coordinator does all I/O.

use core::fmt;

use error_stack::Report;
use http::header::{AUTHORIZATION, CONTENT_TYPE, COOKIE};
use http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use serde::Serialize;
use serde::de::DeserializeOwned;
use url::Url;

use super::resolution::IdentityProviderError;
use crate::redacted::Redacted;

/// Maximum JSON bytes of a call context.
pub const MAX_CALL_CONTEXT_BYTES: usize = 1024;

/// One call a provider wants made.
#[derive(Clone)]
pub struct ProviderRequest {
    method: Method,
    url: Url,
    headers: HeaderMap,
    body: Vec<u8>,
    call_context: Option<String>,
}

impl fmt::Debug for ProviderRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderRequest")
            .field("method", &self.method)
            .field("host", &self.url.host_str())
            .field("path", &self.url.path())
            .field("headers", &self.headers.keys().collect::<Vec<_>>())
            .field("body_len", &self.body.len())
            .finish_non_exhaustive()
    }
}

impl ProviderRequest {
    /// Any method, with no body.
    #[must_use]
    pub fn new(method: Method, url: &Url) -> Self {
        Self {
            method,
            url: url.clone(),
            headers: HeaderMap::new(),
            body: Vec::new(),
            call_context: None,
        }
    }

    /// A `GET`.
    #[must_use]
    pub fn get(url: &Url) -> Self {
        Self::new(Method::GET, url)
    }

    /// A `POST` with a JSON body.
    ///
    /// # Errors
    ///
    /// `Config` when `body` does not serialize.
    pub fn post_json(
        url: &Url,
        body: &impl Serialize,
    ) -> Result<Self, Report<IdentityProviderError>> {
        let bytes = serde_json::to_vec(body)
            .map_err(|_| IdentityProviderError::config("request body does not serialize"))?;
        Self::new(Method::POST, url).with_body("application/json", bytes)
    }

    /// A `POST` with a `text/plain` body.
    #[must_use]
    pub fn post_text(url: &Url, body: &str) -> Self {
        let mut request = Self::new(Method::POST, url);
        request
            .headers
            .insert(CONTENT_TYPE, HeaderValue::from_static("text/plain"));
        request.body = body.as_bytes().to_vec();
        request
    }

    /// Sets the body and its `Content-Type`.
    ///
    /// # Errors
    ///
    /// `Config` when `content_type` is not a valid header value.
    pub fn with_body(
        mut self,
        content_type: &str,
        body: Vec<u8>,
    ) -> Result<Self, Report<IdentityProviderError>> {
        let value = HeaderValue::from_str(content_type)
            .map_err(|_| IdentityProviderError::config("invalid content type"))?;
        self.headers.insert(CONTENT_TYPE, value);
        self.body = body;
        Ok(self)
    }

    /// Appends a query parameter, URL-encoded.
    #[must_use]
    pub fn with_query(mut self, name: &str, value: &str) -> Self {
        self.url.query_pairs_mut().append_pair(name, value);
        self
    }

    /// Sets a header. A `Cookie` header makes the coordinator refuse the call.
    #[must_use]
    pub fn with_header(mut self, name: HeaderName, value: HeaderValue) -> Self {
        self.headers.insert(name, value);
        self
    }

    /// Sets a header from strings.
    ///
    /// # Errors
    ///
    /// `Config` when the name or value is invalid.
    pub fn try_with_header(
        self,
        name: &str,
        value: &str,
    ) -> Result<Self, Report<IdentityProviderError>> {
        let name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| IdentityProviderError::config("invalid header name"))?;
        let value = HeaderValue::from_str(value)
            .map_err(|_| IdentityProviderError::config("invalid header value"))?;
        Ok(self.with_header(name, value))
    }

    /// Adds `Authorization: Bearer <token>`.
    #[must_use]
    pub fn with_bearer(mut self, token: &Redacted<String>) -> Self {
        if let Ok(mut value) = HeaderValue::from_str(&format!("Bearer {}", token.expose())) {
            value.set_sensitive(true);
            self.headers.insert(AUTHORIZATION, value);
        }
        self
    }

    /// Data `response` needs from this call. Kept in memory, never sent.
    ///
    /// # Errors
    ///
    /// `Config` when it does not serialize or is over 1,024 bytes of JSON.
    pub fn try_with_call_context(
        mut self,
        context: &impl Serialize,
    ) -> Result<Self, Report<IdentityProviderError>> {
        let json = serde_json::to_string(context)
            .map_err(|_| IdentityProviderError::config("call context does not serialize"))?;
        if json.len() > MAX_CALL_CONTEXT_BYTES {
            return Err(IdentityProviderError::config(
                "call context exceeds 1024 bytes",
            ));
        }
        self.call_context = Some(json);
        Ok(self)
    }

    /// The method.
    #[must_use]
    pub fn method(&self) -> &Method {
        &self.method
    }

    /// The URL.
    #[must_use]
    pub fn url(&self) -> &Url {
        &self.url
    }

    /// The headers.
    #[must_use]
    pub fn headers(&self) -> &HeaderMap {
        &self.headers
    }

    /// The body.
    #[must_use]
    pub fn body(&self) -> &[u8] {
        &self.body
    }

    /// The call context JSON.
    #[must_use]
    // Temporary: first used by a later identity task.
    #[allow(dead_code)]
    pub(crate) fn call_context_json(&self) -> Option<&str> {
        self.call_context.as_deref()
    }

    /// Whether the provider set a `Cookie` header.
    #[must_use]
    // Temporary: first used by a later identity task.
    #[allow(dead_code)]
    pub(crate) fn has_cookie_header(&self) -> bool {
        self.headers.contains_key(COOKIE)
    }
}

/// A buffered, decoded provider response.
#[derive(Clone)]
pub struct ProviderResponse {
    status: StatusCode,
    headers: HeaderMap,
    body: Vec<u8>,
    call_context: Option<String>,
}

impl fmt::Debug for ProviderResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderResponse")
            .field("status", &self.status)
            .field("body_len", &self.body.len())
            .finish_non_exhaustive()
    }
}

impl ProviderResponse {
    /// Built by the coordinator after buffering and decoding.
    // Temporary: the dispatcher, the first non-test caller, arrives in a later task.
    #[allow(dead_code)]
    #[must_use]
    pub(crate) fn new(
        status: StatusCode,
        headers: HeaderMap,
        body: Vec<u8>,
        call_context: Option<String>,
    ) -> Self {
        Self {
            status,
            headers,
            body,
            call_context,
        }
    }

    /// A response for tests.
    #[cfg(any(test, feature = "test-utils"))]
    #[must_use]
    pub fn for_test(status: StatusCode, body: &[u8]) -> Self {
        Self::new(status, HeaderMap::new(), body.to_vec(), None)
    }

    /// The status.
    #[must_use]
    pub fn status(&self) -> StatusCode {
        self.status
    }

    /// A header as a string, if present and visible ASCII.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|value| value.to_str().ok())
    }

    /// The decoded body.
    #[must_use]
    pub fn body(&self) -> &[u8] {
        &self.body
    }

    /// Parses the body as JSON, whatever the status.
    ///
    /// # Errors
    ///
    /// `Malformed` when it does not parse.
    pub fn json<T: DeserializeOwned>(&self) -> Result<T, Report<IdentityProviderError>> {
        serde_json::from_slice(&self.body).map_err(|_| IdentityProviderError::malformed())
    }

    /// Requires 2xx, then parses JSON.
    ///
    /// # Errors
    ///
    /// `Rejected` for a non-2xx status, `Malformed` for a bad body.
    pub fn success_json<T: DeserializeOwned>(&self) -> Result<T, Report<IdentityProviderError>> {
        self.require_success()?;
        self.json()
    }

    /// Requires 2xx and a UTF-8 body.
    ///
    /// # Errors
    ///
    /// `Rejected` for a non-2xx status, `Malformed` for non-UTF-8.
    pub fn success_text(&self) -> Result<&str, Report<IdentityProviderError>> {
        self.require_success()?;
        core::str::from_utf8(&self.body).map_err(|_| IdentityProviderError::malformed())
    }

    /// The context the request carried, decoded.
    ///
    /// # Errors
    ///
    /// `Config` when it does not decode as `T`.
    pub fn call_context<T: DeserializeOwned>(
        &self,
    ) -> Result<Option<T>, Report<IdentityProviderError>> {
        self.call_context
            .as_deref()
            .map(|json| {
                serde_json::from_str(json)
                    .map_err(|_| IdentityProviderError::config("call context does not decode"))
            })
            .transpose()
    }

    fn require_success(&self) -> Result<(), Report<IdentityProviderError>> {
        if self.status.is_success() {
            Ok(())
        } else {
            Err(IdentityProviderError::rejected(self.status))
        }
    }
}

#[cfg(test)]
mod tests {
    use http::{HeaderName, HeaderValue, Method, StatusCode};
    use serde_json::json;

    use super::*;

    fn url() -> Url {
        Url::parse("https://api.example.com/v1/fetch").expect("should parse")
    }

    #[test]
    fn post_json_sets_body_and_content_type() {
        let request = ProviderRequest::post_json(&url(), &json!({ "a": 1 }))
            .expect("should build a JSON request");
        assert_eq!(request.method(), Method::POST, "should use POST");
        assert_eq!(
            request.headers()["content-type"],
            "application/json",
            "should set the JSON content type"
        );
        assert_eq!(request.body(), br#"{"a":1}"#, "should serialize the body");
    }

    #[test]
    fn query_bearer_and_headers() {
        let request = ProviderRequest::get(&url())
            .with_query("pid", "1 2")
            .with_bearer(&Redacted::new("token".to_owned()))
            .try_with_header("x-example", "v")
            .expect("should accept a valid header");
        assert_eq!(
            request.url().as_str(),
            "https://api.example.com/v1/fetch?pid=1+2",
            "should encode the query"
        );
        assert_eq!(
            request.headers()["authorization"],
            "Bearer token",
            "should set the bearer token"
        );
        assert_eq!(request.headers()["x-example"], "v", "should set the header");
        let _ = ProviderRequest::get(&url())
            .try_with_header("bad header", "v")
            .expect_err("should reject a bad name");
        let _ = ProviderRequest::get(&url())
            .try_with_header("x", "bad\nvalue")
            .expect_err("should reject a bad value");
    }

    #[test]
    fn cookie_headers_are_flagged_from_either_helper() {
        let typed = ProviderRequest::get(&url()).with_header(
            HeaderName::from_static("cookie"),
            HeaderValue::from_static("a=1"),
        );
        let dynamic = ProviderRequest::get(&url())
            .try_with_header("Cookie", "a=1")
            .expect("should accept a Cookie header from strings");
        assert!(
            typed.has_cookie_header(),
            "should flag a Cookie header from the typed helper"
        );
        assert!(
            dynamic.has_cookie_header(),
            "should flag a Cookie header from the string helper"
        );
        assert!(
            !ProviderRequest::get(&url()).has_cookie_header(),
            "should not flag a request without one"
        );
    }

    #[test]
    fn call_context_round_trips_and_is_capped() {
        let request = ProviderRequest::get(&url())
            .try_with_call_context(&json!({ "nonce": [1, 2] }))
            .expect("should accept a small call context");
        let response = ProviderResponse::new(
            StatusCode::OK,
            http::HeaderMap::new(),
            Vec::new(),
            request.call_context_json().map(str::to_owned),
        );
        let context: serde_json::Value = response
            .call_context()
            .expect("should decode the call context")
            .expect("should carry the call context");
        assert_eq!(context, json!({ "nonce": [1, 2] }), "should round-trip");

        let error = ProviderRequest::get(&url())
            .try_with_call_context(&"x".repeat(MAX_CALL_CONTEXT_BYTES))
            .expect_err("should reject an oversized call context");
        assert!(
            matches!(
                error.current_context(),
                IdentityProviderError::Config { .. }
            ),
            "should report an oversized call context as Config"
        );
    }

    #[test]
    fn success_helpers_check_status_and_encoding() {
        let ok = ProviderResponse::for_test(StatusCode::OK, br#"{"uid":"u"}"#);
        let value: serde_json::Value = ok.success_json().expect("should parse a 2xx JSON body");
        assert_eq!(value["uid"], "u", "should read the uid");

        let rejected = ProviderResponse::for_test(StatusCode::FORBIDDEN, br#"{}"#);
        let error = rejected
            .success_json::<serde_json::Value>()
            .expect_err("should reject a non-2xx status");
        assert!(
            matches!(error.current_context(), IdentityProviderError::Rejected { status } if *status == StatusCode::FORBIDDEN),
            "should report Rejected with the status"
        );
        rejected
            .json::<serde_json::Value>()
            .expect("should parse JSON whatever the status");

        let binary = ProviderResponse::for_test(StatusCode::OK, &[0xff, 0xfe]);
        let error = binary.success_text().expect_err("should reject non-UTF-8");
        assert!(
            matches!(error.current_context(), IdentityProviderError::Malformed),
            "should report non-UTF-8 as Malformed"
        );
    }
}
