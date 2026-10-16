//! Loopback HTTP exchanges of the runtime gate: the publisher origin fixture,
//! a minimal HTTP/1.1 client and the pure response checks.
//!
//! The client only speaks to loopback addresses the gate chose itself. It
//! requests `identity` encoding with `Connection: close`, reads at most
//! [`RESPONSE_LIMIT`] bytes and decodes `Content-Length` or chunked bodies
//! strictly.

use std::io::{ErrorKind, Read as _, Write as _};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use error_stack::Report;
use sha2::{Digest as _, Sha256};

use super::{RuntimeRejection, runtime_error};
use crate::error::ReleaseError;

/// Maximum bytes read from one response, head and body together.
pub(in crate::recipe) const RESPONSE_LIMIT: u64 = 4 * 1024 * 1024;
/// Maximum bytes the origin fixture reads from one request head.
const REQUEST_HEAD_LIMIT: u64 = 64 * 1024;
/// The attribute that identifies the injected Trusted Server script.
const SCRIPT_ID: &str = "id=\"trustedserver-js\"";
/// The required prefix of the injected script's `src`.
const SCRIPT_SOURCE_PREFIX: &str = "/static/tsjs=";
/// Interval at which the origin fixture checks its stop flag.
const ACCEPT_POLL: Duration = Duration::from_millis(20);

/// A parsed HTTP response with a fully decoded body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::recipe) struct HttpResponse {
    pub(in crate::recipe) status: u16,
    /// Header names in lowercase, in received order.
    pub(in crate::recipe) headers: Vec<(String, String)>,
    pub(in crate::recipe) body: Vec<u8>,
}

impl HttpResponse {
    /// Returns the first value of `name`, compared case-insensitively.
    pub(in crate::recipe) fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(seen, _)| seen.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// What the accepted publisher response showed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PublisherObservation {
    pub(crate) status: u16,
    pub(crate) body_size: usize,
    pub(crate) body_sha256: String,
    pub(crate) script_src: String,
}

fn exchange_error(message: impl std::fmt::Display) -> Report<ReleaseError> {
    runtime_error(RuntimeRejection::Exchange, message)
}

/// Sends one `GET` to a loopback `address` and parses the response.
///
/// # Errors
///
/// Returns [`RuntimeRejection::Exchange`] when the connection, write or read
/// fails or times out, the response exceeds [`RESPONSE_LIMIT`], or it does
/// not parse.
pub(in crate::recipe) fn get(
    address: SocketAddr,
    path: &str,
    headers: &[(&str, &str)],
    timeout: Duration,
) -> Result<HttpResponse, Report<ReleaseError>> {
    if !address.ip().is_loopback() {
        return Err(exchange_error("the gate only contacts loopback addresses"));
    }
    let mut stream = TcpStream::connect_timeout(&address, timeout)
        .map_err(|error| exchange_error(format!("cannot connect to {address}: {error}")))?;
    stream
        .set_read_timeout(Some(timeout))
        .and_then(|()| stream.set_write_timeout(Some(timeout)))
        .map_err(|error| exchange_error(format!("cannot set socket timeouts: {error}")))?;
    let mut request = format!(
        "GET {path} HTTP/1.1\r\nHost: {address}\r\nAccept-Encoding: identity\r\nConnection: close\r\n"
    );
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str("\r\n");
    stream
        .write_all(request.as_bytes())
        .map_err(|error| exchange_error(format!("cannot send the request: {error}")))?;
    let mut raw = Vec::new();
    stream
        .take(RESPONSE_LIMIT + 1)
        .read_to_end(&mut raw)
        .map_err(|error| exchange_error(format!("cannot read the response: {error}")))?;
    if raw.len() as u64 > RESPONSE_LIMIT {
        return Err(exchange_error("response exceeds the byte limit"));
    }
    parse_response(&raw)
}

/// Parses a complete HTTP/1.x response read until the connection closed.
///
/// # Errors
///
/// Returns [`RuntimeRejection::Exchange`] for a malformed head or header
/// name, a non-identity content encoding, a transfer encoding other than
/// exactly `chunked`, duplicate or conflicting framing headers, a body
/// shorter or longer than its `Content-Length`, or a malformed chunked body.
pub(in crate::recipe) fn parse_response(raw: &[u8]) -> Result<HttpResponse, Report<ReleaseError>> {
    let split = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| exchange_error("response head is incomplete"))?;
    let head = std::str::from_utf8(&raw[..split])
        .ok()
        .filter(|head| head.is_ascii())
        .ok_or_else(|| exchange_error("response head is not ASCII"))?;
    let rest = &raw[split + 4..];
    let mut lines = head.split("\r\n");
    let status = parse_status_line(lines.next().unwrap_or_default())?;
    let headers = lines
        .map(parse_header_line)
        .collect::<Result<Vec<_>, _>>()?;
    let mut response = HttpResponse {
        status,
        headers,
        body: Vec::new(),
    };
    if let Some(encoding) = response.header("content-encoding")
        && !encoding.eq_ignore_ascii_case("identity")
    {
        return Err(exchange_error(format!(
            "unexpected content encoding {encoding:?}"
        )));
    }
    let count = |name: &str| {
        response
            .headers
            .iter()
            .filter(|(seen, _)| seen == name)
            .count()
    };
    let transfer_encodings = count("transfer-encoding");
    let content_lengths = count("content-length");
    if transfer_encodings > 1 || content_lengths > 1 {
        return Err(exchange_error("duplicate framing header"));
    }
    if transfer_encodings == 1 && content_lengths == 1 {
        return Err(exchange_error(
            "transfer encoding and content length are both present",
        ));
    }
    response.body = if let Some(encoding) = response.header("transfer-encoding") {
        if !encoding.eq_ignore_ascii_case("chunked") {
            return Err(exchange_error(format!(
                "unsupported transfer encoding {encoding:?}"
            )));
        }
        decode_chunked(rest)?
    } else if let Some(length) = response.header("content-length") {
        let length: usize = length
            .bytes()
            .all(|byte| byte.is_ascii_digit())
            .then(|| length.parse().ok())
            .flatten()
            .ok_or_else(|| exchange_error(format!("malformed content length {length:?}")))?;
        if rest.len() != length {
            return Err(exchange_error(format!(
                "body holds {} bytes, content length says {length}",
                rest.len()
            )));
        }
        rest.to_vec()
    } else {
        rest.to_vec()
    };
    Ok(response)
}

/// Whether `byte` may appear in an HTTP header name (an RFC 9110 token).
fn is_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte)
}

/// Parses one `name: value` header line, lowercasing the name.
fn parse_header_line(line: &str) -> Result<(String, String), Report<ReleaseError>> {
    line.split_once(':')
        .filter(|(name, _)| !name.is_empty() && name.bytes().all(is_token_byte))
        .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_owned()))
        .ok_or_else(|| exchange_error(format!("malformed header line {line:?}")))
}

/// Parses `HTTP/1.x NNN reason` into its status code.
fn parse_status_line(line: &str) -> Result<u16, Report<ReleaseError>> {
    let mut parts = line.splitn(3, ' ');
    let version = parts.next().unwrap_or_default();
    let code = parts.next().unwrap_or_default();
    if !matches!(version, "HTTP/1.1" | "HTTP/1.0")
        || code.len() != 3
        || !code.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(exchange_error(format!("malformed status line {line:?}")));
    }
    code.parse()
        .map_err(|_| exchange_error(format!("malformed status line {line:?}")))
}

/// Decodes a complete chunked body, ignoring chunk extensions.
///
/// The last chunk must be exactly `0\r\n\r\n` with nothing after it, so
/// trailers and trailing bytes are rejected.
fn decode_chunked(mut rest: &[u8]) -> Result<Vec<u8>, Report<ReleaseError>> {
    let mut body = Vec::new();
    loop {
        let line_end = rest
            .windows(2)
            .position(|window| window == b"\r\n")
            .ok_or_else(|| exchange_error("chunk size line is incomplete"))?;
        let size_text = std::str::from_utf8(&rest[..line_end])
            .ok()
            .map(|line| line.split(';').next().unwrap_or_default())
            .filter(|size| {
                !size.is_empty() && size.len() <= 8 && size.bytes().all(|b| b.is_ascii_hexdigit())
            })
            .ok_or_else(|| exchange_error("malformed chunk size"))?;
        let size = usize::from_str_radix(size_text, 16)
            .map_err(|_| exchange_error(format!("malformed chunk size {size_text:?}")))?;
        rest = &rest[line_end + 2..];
        if size == 0 {
            if rest != b"\r\n" {
                return Err(exchange_error(
                    "last chunk is not followed by exactly one empty line",
                ));
            }
            return Ok(body);
        }
        let data = rest
            .get(..size)
            .filter(|_| rest.get(size..size + 2) == Some(b"\r\n"))
            .ok_or_else(|| exchange_error("chunk data is truncated"))?;
        body.extend_from_slice(data);
        rest = &rest[size + 2..];
    }
}

/// Returns whether `response` is the settings-free health answer.
pub(in crate::recipe) fn is_health_ok(response: &HttpResponse) -> bool {
    response.status == 200 && response.body.trim_ascii() == b"ok"
}

/// Requires a successful publisher page proxied from this run's origin with
/// exactly one injected Trusted Server script ahead of the origin content.
///
/// # Errors
///
/// Returns [`RuntimeRejection::Status`] for any status but 200,
/// [`RuntimeRejection::Injection`] for a non-HTML or non-UTF-8 page or a
/// missing, duplicated, malformed or misplaced script, and
/// [`RuntimeRejection::OriginMarker`] when the page does not hold this run's
/// origin marker exactly once.
pub(in crate::recipe) fn check_publisher_response(
    response: &HttpResponse,
    marker: &str,
) -> Result<PublisherObservation, Report<ReleaseError>> {
    if response.status != 200 {
        return Err(runtime_error(
            RuntimeRejection::Status,
            format!("publisher page answered {}", response.status),
        ));
    }
    let injection = |message: &str| runtime_error(RuntimeRejection::Injection, message);
    if !response
        .header("content-type")
        .is_some_and(|value| value.to_ascii_lowercase().starts_with("text/html"))
    {
        return Err(injection("publisher page is not text/html"));
    }
    let body = std::str::from_utf8(&response.body)
        .map_err(|_| injection("publisher page is not UTF-8"))?;
    if body.matches(marker).count() != 1 {
        return Err(runtime_error(
            RuntimeRejection::OriginMarker,
            "publisher page does not hold this run's origin marker exactly once",
        ));
    }
    let scripts = body.matches(SCRIPT_ID).count();
    if scripts != 1 {
        return Err(injection(&format!(
            "publisher page holds {scripts} Trusted Server scripts, not one"
        )));
    }
    let id_at = body.find(SCRIPT_ID).unwrap_or_default();
    let open = body[..id_at]
        .rfind("<script")
        .filter(|open| !body[*open..id_at].contains('>'))
        .ok_or_else(|| injection("Trusted Server script id is not on a script tag"))?;
    let close = body[id_at..]
        .find('>')
        .map(|offset| id_at + offset)
        .ok_or_else(|| injection("Trusted Server script tag is unterminated"))?;
    let source = body[open..close]
        .split_once("src=\"")
        .and_then(|(_, rest)| rest.split_once('"'))
        .map(|(source, _)| source)
        .filter(|source| source.starts_with(SCRIPT_SOURCE_PREFIX))
        .ok_or_else(|| injection("Trusted Server script does not load the tsjs bundle"))?;
    if body.find(marker).is_some_and(|marker_at| marker_at < open) {
        return Err(injection(
            "Trusted Server script follows the origin content",
        ));
    }
    Ok(PublisherObservation {
        status: response.status,
        body_size: response.body.len(),
        body_sha256: hex::encode(Sha256::digest(&response.body)),
        script_src: source.to_owned(),
    })
}

/// The publisher page the origin fixture serves for `marker`.
pub(in crate::recipe) fn origin_page(marker: &str) -> String {
    format!(
        "<!DOCTYPE html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\
         <title>Example publisher</title></head>\n<body><main><h1>Example publisher</h1>\
         <p id=\"origin-fixture\">{marker}</p></main></body></html>\n"
    )
}

/// A loopback publisher origin that serves [`origin_page`] for `GET /` and
/// records every request line it receives.
pub(in crate::recipe) struct OriginFixture {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    requests: Arc<Mutex<Vec<String>>>,
    thread: Option<JoinHandle<()>>,
}

impl OriginFixture {
    /// Binds an ephemeral loopback port and starts serving.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeRejection::Fixture`] when the listener cannot be
    /// bound or configured.
    pub(in crate::recipe) fn start(marker: &str) -> Result<Self, Report<ReleaseError>> {
        let fixture = |error: std::io::Error| {
            runtime_error(
                RuntimeRejection::Fixture,
                format!("cannot start the origin fixture: {error}"),
            )
        };
        let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(fixture)?;
        listener.set_nonblocking(true).map_err(fixture)?;
        let address = listener.local_addr().map_err(fixture)?;
        let stop = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let page = origin_page(marker);
        let thread = {
            let stop = Arc::clone(&stop);
            let requests = Arc::clone(&requests);
            std::thread::spawn(move || serve(&listener, &stop, &requests, &page))
        };
        Ok(Self {
            address,
            stop,
            requests,
            thread: Some(thread),
        })
    }

    /// The bound loopback address.
    pub(in crate::recipe) fn address(&self) -> SocketAddr {
        self.address
    }

    /// Stops serving and returns every request line received.
    pub(in crate::recipe) fn stop(mut self) -> Vec<String> {
        self.halt();
        self.requests
            .lock()
            .map(|requests| requests.clone())
            .unwrap_or_default()
    }

    fn halt(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for OriginFixture {
    fn drop(&mut self) {
        self.halt();
    }
}

/// Accepts connections until `stop` is set, answering each in turn.
fn serve(listener: &TcpListener, stop: &AtomicBool, requests: &Mutex<Vec<String>>, page: &str) {
    while !stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _)) => {
                if let Some(line) = answer(stream, page)
                    && let Ok(mut requests) = requests.lock()
                {
                    requests.push(line);
                }
            }
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                std::thread::sleep(ACCEPT_POLL);
            }
            Err(_) => std::thread::sleep(ACCEPT_POLL),
        }
    }
}

/// Reads one request head and answers it; returns its request line.
fn answer(mut stream: TcpStream, page: &str) -> Option<String> {
    stream.set_nonblocking(false).ok()?;
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .ok()?;
    stream
        .set_write_timeout(Some(Duration::from_secs(10)))
        .ok()?;
    let mut head = Vec::new();
    let mut limited = (&stream).take(REQUEST_HEAD_LIMIT);
    let mut byte = [0_u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        match limited.read(&mut byte) {
            Ok(1) => head.push(byte[0]),
            _ => return None,
        }
    }
    let line = String::from_utf8_lossy(&head)
        .lines()
        .next()
        .unwrap_or_default()
        .to_owned();
    let (status, content_type, body) = if line.starts_with("GET / ") {
        ("200 OK", "text/html; charset=utf-8", page)
    } else {
        ("404 Not Found", "text/plain; charset=utf-8", "not found\n")
    };
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
    Some(line)
}
