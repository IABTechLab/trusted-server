//! EC integration test helpers.
//!
//! Provides a cookie-aware HTTP client and request builders for the EC
//! identity lifecycle endpoints: partner registration, pixel sync,
//! identify, and batch sync.
//!
//! Also provides a minimal origin server that satisfies organic route
//! proxying so the trusted-server can generate and set EC cookies.

use crate::common::runtime::{TestError, TestResult};
use error_stack::{Report, ResultExt};
use reqwest::blocking::{Client, Response};
use serde_json::Value;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::thread;
use std::thread::JoinHandle;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Cookie-aware HTTP client
// ---------------------------------------------------------------------------

/// HTTP client that manually tracks the `ts-ec` cookie value.
///
/// Reqwest's built-in cookie jar respects domain matching, but the EC
/// cookie is set with `Domain=.test-publisher.com` while tests run
/// against `127.0.0.1`. This client extracts and replays the `ts-ec`
/// cookie manually via the `Cookie` header.
pub struct EcTestClient {
    client: Client,
    pub base_url: String,
    /// The active `ts-ec` cookie value, updated after each response.
    ec_cookie: std::cell::RefCell<Option<String>>,
}

impl EcTestClient {
    /// Creates a new client. Redirects are disabled so tests can inspect
    /// 302 responses from `/sync`.
    pub fn new(base_url: &str) -> Self {
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("should build reqwest client");

        Self {
            client,
            base_url: base_url.to_owned(),
            ec_cookie: std::cell::RefCell::new(None),
        }
    }

    /// Updates the tracked EC cookie from a response's `Set-Cookie` headers.
    fn track_ec_cookie(&self, resp: &Response) {
        for value in resp.headers().get_all("set-cookie") {
            if let Ok(cookie_str) = value.to_str() {
                if cookie_str.starts_with("ts-ec=") {
                    if cookie_str.contains("Max-Age=0") {
                        // Cookie deletion
                        *self.ec_cookie.borrow_mut() = None;
                    } else if let Some(val) = cookie_str
                        .split(';')
                        .next()
                        .and_then(|s| s.strip_prefix("ts-ec="))
                    {
                        if !val.is_empty() {
                            *self.ec_cookie.borrow_mut() = Some(val.to_owned());
                        }
                    }
                }
            }
        }
    }

    /// Builds a request with the tracked EC cookie attached.
    fn attach_ec_cookie(
        &self,
        builder: reqwest::blocking::RequestBuilder,
    ) -> reqwest::blocking::RequestBuilder {
        if let Some(ref ec) = *self.ec_cookie.borrow() {
            builder.header("cookie", format!("ts-ec={ec}"))
        } else {
            builder
        }
    }

    /// `GET {base_url}{path}` with tracked EC cookie.
    pub fn get(&self, path: &str) -> TestResult<Response> {
        let builder = self.client.get(format!("{}{path}", self.base_url));
        let resp = self
            .attach_ec_cookie(builder)
            .send()
            .change_context(TestError::HttpRequest)
            .attach(format!("GET {path}"))?;
        self.track_ec_cookie(&resp);
        Ok(resp)
    }

    /// `GET {base_url}{path}` with extra headers.
    pub fn get_with_headers(&self, path: &str, headers: &[(&str, &str)]) -> TestResult<Response> {
        let mut builder = self.client.get(format!("{}{path}", self.base_url));
        for (key, value) in headers {
            builder = builder.header(*key, *value);
        }
        let resp = self
            .attach_ec_cookie(builder)
            .send()
            .change_context(TestError::HttpRequest)
            .attach(format!("GET {path}"))?;
        self.track_ec_cookie(&resp);
        Ok(resp)
    }

    /// `POST {base_url}{path}` with JSON body.
    pub fn post_json(&self, path: &str, body: &Value) -> TestResult<Response> {
        let builder = self
            .client
            .post(format!("{}{path}", self.base_url))
            .json(body);
        let resp = self
            .attach_ec_cookie(builder)
            .send()
            .change_context(TestError::HttpRequest)
            .attach(format!("POST {path}"))?;
        self.track_ec_cookie(&resp);
        Ok(resp)
    }

    /// `POST {base_url}{path}` with JSON body and basic auth.
    pub fn post_json_with_basic_auth(
        &self,
        path: &str,
        body: &Value,
        username: &str,
        password: &str,
    ) -> TestResult<Response> {
        let builder = self
            .client
            .post(format!("{}{path}", self.base_url))
            .basic_auth(username, Some(password))
            .json(body);
        let resp = self
            .attach_ec_cookie(builder)
            .send()
            .change_context(TestError::HttpRequest)
            .attach(format!("POST {path} (basic auth)"))?;
        self.track_ec_cookie(&resp);
        Ok(resp)
    }

    /// `POST {base_url}{path}` with JSON body and bearer token auth.
    pub fn post_json_with_bearer(
        &self,
        path: &str,
        body: &Value,
        token: &str,
    ) -> TestResult<Response> {
        let builder = self
            .client
            .post(format!("{}{path}", self.base_url))
            .bearer_auth(token)
            .json(body);
        let resp = self
            .attach_ec_cookie(builder)
            .send()
            .change_context(TestError::HttpRequest)
            .attach(format!("POST {path} (bearer auth)"))?;
        self.track_ec_cookie(&resp);
        Ok(resp)
    }

    /// Returns the currently tracked EC cookie value, if any.
    #[allow(dead_code)]
    pub fn ec_cookie_value(&self) -> Option<String> {
        self.ec_cookie.borrow().clone()
    }
}

// ---------------------------------------------------------------------------
// Partner registration
// ---------------------------------------------------------------------------

/// Admin credentials matching `trusted-server.toml` `[[handlers]]` for `/_ts/admin`.
const ADMIN_USER: &str = "admin";
const ADMIN_PASS: &str = "changeme";

/// Registers a test partner via `POST /_ts/admin/partners/register`.
pub fn register_test_partner(
    client: &EcTestClient,
    partner_id: &str,
    api_key: &str,
    return_domain: &str,
) -> TestResult<()> {
    let body = serde_json::json!({
        "id": partner_id,
        "name": format!("Test Partner {partner_id}"),
        "api_key": api_key,
        "allowed_return_domains": [return_domain],
        "source_domain": format!("{partner_id}.example.com"),
        "bidstream_enabled": true,
    });

    let resp = client.post_json_with_basic_auth(
        "/_ts/admin/partners/register",
        &body,
        ADMIN_USER,
        ADMIN_PASS,
    )?;

    let status = resp.status().as_u16();
    if !resp.status().is_success() {
        let body_text = resp.text().unwrap_or_default();
        return Err(Report::new(TestError::PartnerRegistrationFailed)
            .attach(format!("Expected 2xx, got {status}; body: {body_text}")));
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Pixel sync
// ---------------------------------------------------------------------------

/// Calls `GET /sync` with the required query parameters.
///
/// Returns the raw response (typically a 302 redirect).
pub fn pixel_sync(
    client: &EcTestClient,
    partner: &str,
    uid: &str,
    return_url: &str,
) -> TestResult<Response> {
    let path = format!(
        "/sync?partner={partner}&uid={uid}&return={}",
        urlencoding::encode(return_url)
    );
    client.get(&path)
}

// ---------------------------------------------------------------------------
// Identify
// ---------------------------------------------------------------------------

/// Calls `GET /identify` and returns the raw response.
pub fn identify(client: &EcTestClient) -> TestResult<Response> {
    client.get("/identify")
}

// ---------------------------------------------------------------------------
// Batch sync
// ---------------------------------------------------------------------------

/// Calls `POST /_ts/api/v1/sync` with bearer auth and the given mappings.
pub fn batch_sync(
    client: &EcTestClient,
    api_key: &str,
    mappings: &[BatchMapping],
) -> TestResult<Response> {
    let body = serde_json::json!({ "mappings": mappings_to_json(mappings) });
    client.post_json_with_bearer("/_ts/api/v1/sync", &body, api_key)
}

/// Calls `POST /_ts/api/v1/sync` without any auth header.
pub fn batch_sync_no_auth(
    client: &EcTestClient,
    mappings: &[BatchMapping],
) -> TestResult<Response> {
    let body = serde_json::json!({ "mappings": mappings_to_json(mappings) });
    client.post_json("/_ts/api/v1/sync", &body)
}

/// Single mapping in a batch sync request.
pub struct BatchMapping {
    pub ec_hash: String,
    pub partner_uid: String,
    pub timestamp: u64,
}

fn mappings_to_json(mappings: &[BatchMapping]) -> Vec<Value> {
    mappings
        .iter()
        .map(|m| {
            serde_json::json!({
                "ec_hash": m.ec_hash,
                "partner_uid": m.partner_uid,
                "timestamp": m.timestamp,
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Assertion helpers
// ---------------------------------------------------------------------------

/// Asserts the response has a specific HTTP status code.
pub fn assert_status(resp: &Response, expected: u16) -> TestResult<()> {
    let actual = resp.status().as_u16();
    if actual != expected {
        return Err(Report::new(TestError::UnexpectedStatusCode {
            expected,
            actual,
        }));
    }
    Ok(())
}

/// Asserts the response status and returns the parsed JSON body.
pub fn assert_json_response(resp: Response, expected_status: u16) -> TestResult<Value> {
    let actual = resp.status().as_u16();
    if actual != expected_status {
        let body_text = resp.text().unwrap_or_default();
        return Err(Report::new(TestError::UnexpectedStatusCode {
            expected: expected_status,
            actual,
        })
        .attach(format!("body: {body_text}")));
    }

    let body = resp
        .text()
        .change_context(TestError::ResponseParse)
        .attach("failed to read response body")?;

    serde_json::from_str(&body)
        .change_context(TestError::ResponseParse)
        .attach(format!("invalid JSON: {body}"))
}

/// Extracts the `ts-ec` cookie value from a `Set-Cookie` response header.
///
/// Returns `None` if no `ts-ec` cookie was set.
pub fn extract_ec_cookie_from_response(resp: &Response) -> Option<String> {
    for value in resp.headers().get_all("set-cookie") {
        let cookie_str = value.to_str().ok()?;
        if cookie_str.starts_with("ts-ec=") {
            let value = cookie_str
                .split(';')
                .next()?
                .strip_prefix("ts-ec=")?
                .to_owned();
            if !value.is_empty() {
                return Some(value);
            }
        }
    }
    None
}

/// Checks whether the response expires (deletes) the `ts-ec` cookie.
pub fn is_ec_cookie_expired(resp: &Response) -> bool {
    for value in resp.headers().get_all("set-cookie") {
        if let Ok(cookie_str) = value.to_str() {
            if cookie_str.starts_with("ts-ec=") && cookie_str.contains("Max-Age=0") {
                return true;
            }
        }
    }
    false
}

/// Extracts the stable 64-char hex prefix from an EC ID (`{64hex}.{6alnum}`).
pub fn ec_hash(ec_id: &str) -> &str {
    match ec_id.find('.') {
        Some(pos) => &ec_id[..pos],
        None => ec_id,
    }
}

// ---------------------------------------------------------------------------
// Minimal origin server
// ---------------------------------------------------------------------------

/// A minimal HTTP origin server that returns `200 OK` with a simple HTML body
/// for any request. Required for organic route proxying — without a running
/// origin, the trusted-server returns an error and never sets the EC cookie.
///
/// Runs on the given port in a background thread. Dropped when the handle
/// goes out of scope via explicit shutdown + thread join.
pub struct MinimalOrigin {
    shutdown_tx: mpsc::Sender<()>,
    handle: Option<JoinHandle<()>>,
}

impl MinimalOrigin {
    /// Starts a minimal origin server on `127.0.0.1:{port}`.
    ///
    /// # Panics
    ///
    /// Panics if the port is already in use.
    pub fn start(port: u16) -> Self {
        let listener =
            TcpListener::bind(format!("127.0.0.1:{port}")).expect("should bind origin port");
        listener
            .set_nonblocking(true)
            .expect("should set listener nonblocking");
        let (shutdown_tx, shutdown_rx) = mpsc::channel::<()>();

        let handle = thread::spawn(move || {
            loop {
                if shutdown_rx.try_recv().is_ok() {
                    break;
                }

                match listener.accept() {
                    Ok((mut stream, _addr)) => {
                        // Read one chunk to consume the request line/headers.
                        let mut buf = [0u8; 4096];
                        let _ = stream.read(&mut buf);

                        let body = "<html><body><h1>Test Origin</h1></body></html>";
                        let response = format!(
                            "HTTP/1.1 200 OK\r\n\
                             Content-Type: text/html\r\n\
                             Content-Length: {}\r\n\
                             Connection: close\r\n\
                             \r\n\
                             {body}",
                            body.len()
                        );
                        let _ = stream.write_all(response.as_bytes());
                        let _ = stream.flush();
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });

        Self {
            shutdown_tx,
            handle: Some(handle),
        }
    }
}

impl Drop for MinimalOrigin {
    fn drop(&mut self) {
        let _ = self.shutdown_tx.send(());
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}
