//! EC integration test helpers.
//!
//! Provides a cookie-aware HTTP client and request builders for the EC
//! identity lifecycle endpoints: batch sync, identify, and organic requests.
//!
//! Also provides a minimal origin server that satisfies organic route
//! proxying so the trusted-server can generate and set EC cookies.

use crate::common::runtime::{TestError, TestResult};
use error_stack::{Report, ResultExt};
use reqwest::blocking::{Client, Response};
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::thread;
use std::thread::JoinHandle;
use std::time::Duration;
pub use trusted_server_core::ec::normalize_ec_id_for_kv as normalize_ec_id;

// ---------------------------------------------------------------------------
// Cookie-aware HTTP client
// ---------------------------------------------------------------------------

/// HTTP client that manually tracks cookies used by EC tests.
///
/// Reqwest's built-in cookie jar respects domain matching, but the EC
/// cookie is set with `Domain=.test-publisher.com` while tests run
/// against `127.0.0.1`. This client extracts and replays cookies manually
/// via the `Cookie` header so scenarios can combine consent cookies with
/// the `ts-ec` cookie.
pub struct EcTestClient {
    client: Client,
    pub base_url: String,
    /// Tracked cookies replayed on subsequent requests.
    cookies: std::cell::RefCell<BTreeMap<String, String>>,
}

impl EcTestClient {
    /// Creates a new client. Redirects are disabled so tests can inspect
    /// 302 responses from sync endpoints.
    pub fn new(base_url: &str) -> Self {
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("should build reqwest client");

        Self {
            client,
            base_url: base_url.to_owned(),
            cookies: std::cell::RefCell::new(BTreeMap::new()),
        }
    }

    /// Persist a cookie value for subsequent requests.
    pub fn set_cookie(&self, name: &str, value: &str) {
        self.cookies
            .borrow_mut()
            .insert(name.to_owned(), value.to_owned());
    }

    /// Returns a tracked cookie value, if any.
    pub fn cookie_value(&self, name: &str) -> Option<String> {
        self.cookies.borrow().get(name).cloned()
    }

    /// Updates tracked cookies from a response's `Set-Cookie` headers.
    fn track_response_cookies(&self, resp: &Response) {
        for value in resp.headers().get_all("set-cookie") {
            let Ok(cookie_str) = value.to_str() else {
                continue;
            };
            let Some((name, raw_value)) =
                cookie_str.split(';').next().and_then(|s| s.split_once('='))
            else {
                continue;
            };

            if cookie_str.contains("Max-Age=0") {
                self.cookies.borrow_mut().remove(name);
            } else if !raw_value.is_empty() {
                self.cookies
                    .borrow_mut()
                    .insert(name.to_owned(), raw_value.to_owned());
            }
        }
    }

    /// Builds a request with all tracked cookies attached.
    fn attach_cookies(
        &self,
        builder: reqwest::blocking::RequestBuilder,
    ) -> reqwest::blocking::RequestBuilder {
        let cookie_header = self
            .cookies
            .borrow()
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join("; ");

        if cookie_header.is_empty() {
            builder
        } else {
            builder.header("cookie", cookie_header)
        }
    }

    /// `GET {base_url}{path}` with extra headers (plus navigation headers).
    pub fn get_with_headers(&self, path: &str, headers: &[(&str, &str)]) -> TestResult<Response> {
        let mut builder = self
            .client
            .get(format!("{}{path}", self.base_url))
            .header("sec-fetch-dest", "document")
            .header("accept", "text/html");
        for (key, value) in headers {
            builder = builder.header(*key, *value);
        }
        let resp = self
            .attach_cookies(builder)
            .send()
            .change_context(TestError::HttpRequest)
            .attach(format!("GET {path}"))?;
        self.track_response_cookies(&resp);
        Ok(resp)
    }

    /// `POST {base_url}{path}` with JSON body.
    pub fn post_json(&self, path: &str, body: &Value) -> TestResult<Response> {
        let builder = self
            .client
            .post(format!("{}{path}", self.base_url))
            .json(body);
        let resp = self
            .attach_cookies(builder)
            .send()
            .change_context(TestError::HttpRequest)
            .attach(format!("POST {path}"))?;
        self.track_response_cookies(&resp);
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
            .attach_cookies(builder)
            .send()
            .change_context(TestError::HttpRequest)
            .attach(format!("POST {path} (bearer auth)"))?;
        self.track_response_cookies(&resp);
        Ok(resp)
    }

    /// `GET {base_url}{path}` with bearer token auth (no navigation headers).
    pub fn get_with_bearer(&self, path: &str, token: &str) -> TestResult<Response> {
        let builder = self
            .client
            .get(format!("{}{path}", self.base_url))
            .bearer_auth(token);
        let resp = self
            .attach_cookies(builder)
            .send()
            .change_context(TestError::HttpRequest)
            .attach(format!("GET {path} (bearer auth)"))?;
        self.track_response_cookies(&resp);
        Ok(resp)
    }

    /// `GET {base_url}{path}` with bearer token auth and extra headers.
    pub fn get_with_bearer_and_headers(
        &self,
        path: &str,
        token: &str,
        headers: &[(&str, &str)],
    ) -> TestResult<Response> {
        let mut builder = self
            .client
            .get(format!("{}{path}", self.base_url))
            .bearer_auth(token);
        for (key, value) in headers {
            builder = builder.header(*key, *value);
        }
        let resp = self
            .attach_cookies(builder)
            .send()
            .change_context(TestError::HttpRequest)
            .attach(format!("GET {path} (bearer auth + headers)"))?;
        self.track_response_cookies(&resp);
        Ok(resp)
    }

    /// Returns the currently tracked EC cookie value, if any.
    pub fn ec_cookie_value(&self) -> Option<String> {
        self.cookie_value("ts-ec")
    }
}

// ---------------------------------------------------------------------------
// Identify
// ---------------------------------------------------------------------------

/// Calls `GET /_ts/api/v1/identify` with Bearer token auth.
pub fn identify(client: &EcTestClient, api_token: &str) -> TestResult<Response> {
    client.get_with_bearer("/_ts/api/v1/identify", api_token)
}

/// Calls `GET /_ts/api/v1/identify` with Bearer token and extra headers.
pub fn identify_with_headers(
    client: &EcTestClient,
    api_token: &str,
    headers: &[(&str, &str)],
) -> TestResult<Response> {
    client.get_with_bearer_and_headers("/_ts/api/v1/identify", api_token, headers)
}

// ---------------------------------------------------------------------------
// Batch sync
// ---------------------------------------------------------------------------

/// Calls `POST /_ts/api/v1/batch-sync` with bearer auth and the given mappings.
pub fn batch_sync(
    client: &EcTestClient,
    api_key: &str,
    mappings: &[BatchMapping],
) -> TestResult<Response> {
    let body = serde_json::json!({ "mappings": mappings_to_json(mappings) });
    client.post_json_with_bearer("/_ts/api/v1/batch-sync", &body, api_key)
}

/// Calls `POST /_ts/api/v1/batch-sync` without any auth header.
pub fn batch_sync_no_auth(
    client: &EcTestClient,
    mappings: &[BatchMapping],
) -> TestResult<Response> {
    let body = serde_json::json!({ "mappings": mappings_to_json(mappings) });
    client.post_json("/_ts/api/v1/batch-sync", &body)
}

/// Single mapping in a batch sync request.
pub struct BatchMapping {
    pub ec_id: String,
    pub partner_uid: String,
    pub timestamp: u64,
}

fn mappings_to_json(mappings: &[BatchMapping]) -> Vec<Value> {
    mappings
        .iter()
        .map(|m| {
            serde_json::json!({
                "ec_id": m.ec_id,
                "partner_uid": m.partner_uid,
                "timestamp": m.timestamp,
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Assertion helpers
// ---------------------------------------------------------------------------

/// Hard-asserts the deterministic `EdgeZero` entry-point response header.
///
/// `main()` silently falls back to the legacy entry point when the config store
/// cannot be opened or read, and the EC lifecycle scenarios pass on either path.
/// The `EdgeZero` entry point marks every normal response with a stable header so
/// the `EdgeZero` CI job fails immediately when rollout accidentally falls back to
/// `legacy_main`, without relying on method/status behavior.
pub fn assert_edgezero_entry_point(base_url: &str) -> TestResult<()> {
    let client = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("should build EdgeZero canary client");
    let response = client
        .request(
            reqwest::Method::OPTIONS,
            format!("{base_url}/_ts/api/v1/batch-sync"),
        )
        .send()
        .change_context(TestError::HttpRequest)
        .attach("OPTIONS /_ts/api/v1/batch-sync (EdgeZero entry-point probe)")?;
    let header_value = response
        .headers()
        .get("x-ts-entry-point")
        .and_then(|value| value.to_str().ok());
    if header_value != Some("edgezero") {
        return Err(Report::new(TestError::UnexpectedContent).attach(format!(
            "expected x-ts-entry-point: edgezero from EdgeZero entry point, got {header_value:?}; status was {}",
            response.status()
        )));
    }
    Ok(())
}

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

/// Checks whether the response expires (deletes) the `ts-ec` cookie.
pub fn is_ec_cookie_expired(resp: &Response) -> bool {
    for value in resp.headers().get_all("set-cookie") {
        if let Ok(cookie_str) = value.to_str()
            && cookie_str.starts_with("ts-ec=")
            && cookie_str.contains("Max-Age=0")
        {
            return true;
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Minimal origin server
// ---------------------------------------------------------------------------

/// A minimal HTTP origin server that returns `200 OK` with a simple HTML body
/// for any request. Required for organic route proxying.
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
                        let mut buf = [0u8; 4096];
                        let _ = stream.read(&mut buf);

                        let body = "<html><body><h1>Test Origin</h1></body></html>";
                        let response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
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
