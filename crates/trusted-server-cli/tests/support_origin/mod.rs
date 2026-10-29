//! A portable HTTP fixture origin for the shareability probe tests.
//!
//! Deliberately **not** built on the `support` module next door: that one is
//! `tokio` + `tokio-rustls` + the dev proxy, all of which are scoped to macOS in
//! `Cargo.toml`, and the probe has to be testable on Linux CI as well. This is plain
//! `std::net` and `std::thread`, so it builds anywhere the CLI does.
//!
//! **It loop-accepts.** A single-accept fixture caused a CI flake in this repo before
//! (fixed in PR #823): clients open more sockets than they send requests on. The probe
//! opens one connection per arm and per `--repeat`, so a one-shot server would hang the
//! second fetch rather than fail it.

#![allow(dead_code)]

use std::collections::HashMap;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;

/// One request as the fixture saw it.
pub struct FixtureRequest {
    /// Request method, uppercased as sent.
    pub method: String,
    /// Request target, for example `/article`.
    pub path: String,
    /// Request body, empty when none was sent.
    pub body: Vec<u8>,
    /// Header names lowercased; every instance kept, in arrival order.
    pub headers: HashMap<String, Vec<String>>,
    /// How many requests this server had already answered, starting at 0.
    pub request_index: u64,
}

impl FixtureRequest {
    /// First value of a header, matched case-insensitively.
    ///
    /// First and not last on purpose: a duplicated request header is resolved differently
    /// by different origins, and the ones that read the first instance are the ones a
    /// probe arm can fail to reach. Modelling that here keeps the axis tests honest.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .get(&name.to_ascii_lowercase())
            .and_then(|values| values.first())
            .map(String::as_str)
    }

    /// How many times a header was sent, matched case-insensitively.
    #[must_use]
    pub fn header_count(&self, name: &str) -> usize {
        self.headers
            .get(&name.to_ascii_lowercase())
            .map_or(0, Vec::len)
    }

    /// Whether a cookie with this name was sent.
    #[must_use]
    pub fn has_cookie(&self, name: &str) -> bool {
        self.header("cookie").is_some_and(|cookies| {
            cookies
                .split(';')
                .filter_map(|pair| pair.split('=').next())
                .any(|candidate| candidate.trim() == name)
        })
    }
}

/// What the fixture should answer with.
pub struct FixtureResponse {
    status: u16,
    headers: Vec<(String, Vec<u8>)>,
    body: Vec<u8>,
}

impl FixtureResponse {
    /// A `200` HTML response.
    #[must_use]
    pub fn html(body: impl Into<Vec<u8>>) -> Self {
        Self {
            status: 200,
            headers: vec![("content-type".to_owned(), b"text/html".to_vec())],
            body: body.into(),
        }
    }

    /// Replace the status.
    #[must_use]
    pub fn with_status(mut self, status: u16) -> Self {
        self.status = status;
        self
    }

    /// Append a response header. Repeatable.
    #[must_use]
    pub fn with_header(mut self, name: &str, value: &str) -> Self {
        self.headers
            .push((name.to_owned(), value.as_bytes().to_vec()));
        self
    }

    /// Append a header containing bytes that cannot be represented as text.
    #[must_use]
    pub fn with_raw_header(mut self, name: &str, value: &[u8]) -> Self {
        self.headers.push((name.to_owned(), value.to_vec()));
        self
    }

    /// Remove every instance of a response header.
    #[must_use]
    pub fn without_header(mut self, name: &str) -> Self {
        self.headers
            .retain(|(existing, _)| !existing.eq_ignore_ascii_case(name));
        self
    }

    /// Replace the body without touching headers, for encoding arms.
    #[must_use]
    pub fn with_body(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.body = body.into();
        self
    }
}

/// A fixture origin bound to an ephemeral loopback port.
///
/// Stops when dropped.
pub struct FixtureServer {
    addr: SocketAddr,
    shutdown: Arc<AtomicBool>,
    requests: Arc<AtomicU64>,
    worker: Option<JoinHandle<()>>,
}

impl FixtureServer {
    /// Start answering every connection from `handler`.
    ///
    /// # Panics
    ///
    /// Panics if the loopback listener cannot be bound, which in a test means the
    /// environment is unusable rather than the code under test being wrong.
    pub fn start<H>(handler: H) -> Self
    where
        H: Fn(&FixtureRequest) -> FixtureResponse + Send + Sync + 'static,
    {
        let listener =
            TcpListener::bind("127.0.0.1:0").expect("should bind a loopback fixture listener");
        let addr = listener
            .local_addr()
            .expect("should read the fixture listener address");
        // Non-blocking accept plus a short sleep, so the worker notices the shutdown flag
        // instead of parking forever in accept() after the last request.
        listener
            .set_nonblocking(true)
            .expect("should set the fixture listener non-blocking");

        let shutdown = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(AtomicU64::new(0));
        let worker = {
            let shutdown = Arc::clone(&shutdown);
            let requests = Arc::clone(&requests);
            let handler = Arc::new(handler);
            std::thread::spawn(move || {
                while !shutdown.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let index = requests.fetch_add(1, Ordering::SeqCst);
                            // One connection at a time is enough: the probe is sequential,
                            // and serving inline keeps request_index deterministic.
                            serve_one(stream, handler.as_ref(), index);
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(std::time::Duration::from_millis(5));
                        }
                        Err(_) => break,
                    }
                }
            })
        };

        Self {
            addr,
            shutdown,
            requests,
            worker: Some(worker),
        }
    }

    /// Absolute URL for a path on this fixture.
    #[must_use]
    pub fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    /// How many requests have been answered.
    #[must_use]
    pub fn request_count(&self) -> u64 {
        self.requests.load(Ordering::SeqCst)
    }
}

impl Drop for FixtureServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn serve_one<H>(mut stream: TcpStream, handler: &H, request_index: u64)
where
    H: Fn(&FixtureRequest) -> FixtureResponse,
{
    stream
        .set_nonblocking(false)
        .expect("should set the accepted fixture stream blocking");
    let Some(request) = read_request(&stream, request_index) else {
        return;
    };
    let response = handler(&request);

    let mut out = Vec::new();
    let reason = if response.status == 200 {
        "OK"
    } else {
        "Status"
    };
    out.extend_from_slice(format!("HTTP/1.1 {} {reason}\r\n", response.status).as_bytes());
    for (name, value) in &response.headers {
        out.extend_from_slice(format!("{name}: ").as_bytes());
        out.extend_from_slice(value);
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(format!("content-length: {}\r\n", response.body.len()).as_bytes());
    // No keep-alive: one request per connection keeps the fixture trivial, and the probe
    // opens a fresh connection per arm anyway.
    out.extend_from_slice(b"connection: close\r\n\r\n");
    out.extend_from_slice(&response.body);

    let _ = stream.write_all(&out);
    let _ = stream.flush();
}

fn read_request(stream: &TcpStream, request_index: u64) -> Option<FixtureRequest> {
    let mut reader = BufReader::new(stream);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).ok()? == 0 {
        return None;
    }
    let mut request_parts = request_line.split_whitespace();
    let method = request_parts.next()?.to_owned();
    let path = request_parts.next()?.to_owned();

    let mut headers = HashMap::new();
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).ok()? == 0 {
            break;
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            let name = name.trim().to_ascii_lowercase();
            let value = value.trim().to_owned();
            if name == "content-length" {
                content_length = value.parse().unwrap_or(0);
            }
            headers.entry(name).or_insert_with(Vec::new).push(value);
        }
    }

    // Read any body, which also drains it so the client is not left writing into a closed
    // socket.
    let mut body = vec![0u8; content_length];
    if content_length > 0 && reader.read_exact(&mut body).is_err() {
        body.clear();
    }

    Some(FixtureRequest {
        method,
        path,
        body,
        headers,
        request_index,
    })
}
