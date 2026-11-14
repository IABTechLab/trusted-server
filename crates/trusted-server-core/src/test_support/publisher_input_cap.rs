//! Spec test 21: the publisher input-cap and error matrix of the adapters that buffer.
//!
//! Axum, Cloudflare, and Spin collect each publisher response before egress, under an origin
//! input cap. [`assert_input_cap_matrix`] scripts every case on one origin and checks what the
//! adapter's handler and finalizer serve for each, so the three adapters run the same matrix.
//! [`assert_output_bound`] checks that the processed output keeps its own
//! `max_buffered_body_bytes` bound, apart from the input cap.

// This module builds under the `test-utils` feature, which the crate-root `cfg(test)` allow does
// not cover, and it reads the bodies the adapters already buffered.
#![allow(clippy::disallowed_methods)]

use std::future::Future;
use std::io::Read as _;

use bytes::Bytes;
use edgezero_core::error::EdgeError;
use edgezero_core::http::{Method, Response, StatusCode, header};
use edgezero_core::outbound::ResponseDecoding;

use super::gzip;
use super::scripted_client::{ScriptedHttpClient, ScriptedReply};

/// Origin of the route test settings each adapter runs the matrix with.
pub const ORIGIN: &str = "https://origin.test-publisher.example.com";

/// Where the rewrite points links to [`ORIGIN`].
///
/// The route tests send no TLS signal, so the publisher's request scheme is `http`.
const PUBLIC_ORIGIN: &str = "http://test-publisher.example.com";

/// The `max_buffered_body_bytes` an adapter sets for [`assert_output_bound`].
pub const OUTPUT_BOUND_BYTES: usize = 64 * 1024;

/// Links to [`ORIGIN`] in each page [`assert_input_cap_matrix`] serves.
///
/// The rewrite points each link at the shorter publisher origin. The bytes that saves exceed the
/// markup Trusted Server injects, so a page of `cap` bytes processes to fewer than `cap` bytes.
/// The processed output then stays under its own bound, and only the input cap can fail a page
/// one byte longer.
const ORIGIN_LINKS: usize = 512;

/// Bytes a failing origin sends before its body fails.
const PARTIAL_BODY: &[u8] = b"example-partial-origin-bytes";

/// What an adapter served for one publisher GET.
pub struct Served {
    /// Response status code.
    pub status: u16,
    /// The `Content-Encoding` value, if any.
    pub content_encoding: Option<String>,
    /// The response body as served, still in its content coding.
    pub body: Vec<u8>,
}

impl Served {
    /// Reads a response a route test got from the adapter's router.
    ///
    /// # Panics
    ///
    /// Panics if the body is still a stream, which means the adapter did not collect it before
    /// egress.
    #[must_use]
    pub fn from_router_response(response: Response) -> Self {
        let (parts, body) = response.into_parts();
        Self {
            status: parts.status.as_u16(),
            content_encoding: parts
                .headers
                .get(header::CONTENT_ENCODING)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned),
            body: body
                .into_bytes()
                .expect("should collect the publisher body before egress")
                .to_vec(),
        }
    }
}

/// How the adapter served one case.
enum Expected {
    /// `status`, in `content_encoding`, with exactly `body`.
    Relayed {
        status: u16,
        content_encoding: Option<&'static str>,
        body: Vec<u8>,
    },
    /// A processed 200 page in `content_encoding`.
    ///
    /// Decoded and stripped of the scripts Trusted Server injects, it equals `page`.
    Processed {
        content_encoding: Option<&'static str>,
        page: Vec<u8>,
    },
    /// The publisher's 502, carrying no origin bytes.
    BadGateway,
}

/// Runs spec test 21's input-cap and error matrix against one adapter.
///
/// `origin` is the scripted origin the adapter fetches from, `cap` is the adapter's input cap
/// with the default `max_buffered_body_bytes`, and `decoding` is the response decoding its
/// publisher origin requests. `fetch` sends a publisher GET for a path, with
/// `Accept-Encoding: gzip`, through the adapter's handler and finalizer.
///
/// Every case runs through the same scripted origin:
///
/// - A raw body at `cap` is relayed byte for byte and one byte over fails with 502, on the
///   pass-through route (a 200 the publisher cannot process) and on the buffered route that
///   returns a response unmodified (a 404 it cannot process).
/// - A gzip raw body is decoded and capped like an identity one under
///   [`ResponseDecoding::Decode`], and relayed in its coding, whatever its decoded size, under
///   [`ResponseDecoding::Original`].
/// - A gzip raw body whose encoded bytes exceed `cap` fails with 502 though it decodes under
///   `cap`, because the origin request caps the encoded bytes too.
/// - Identity HTML and gzip HTML that decode to `cap` bytes are processed, and one decoded byte
///   more fails with 502. The gzip page is far under `cap` encoded, so only its decoded size
///   fails it.
/// - A source error after part of the body fails with 502 on all three routes, and none of the
///   partial body is served.
///
/// # Panics
///
/// Panics when the adapter serves any case differently.
pub async fn assert_input_cap_matrix<F, Fut>(
    origin: &ScriptedHttpClient,
    cap: usize,
    decoding: ResponseDecoding,
    fetch: F,
) where
    F: Fn(&'static str) -> Fut,
    Fut: Future<Output = Served>,
{
    let decoded = decoding == ResponseDecoding::Decode;
    let processed_coding = if decoded { None } else { Some("gzip") };

    for (path, status, len) in [
        ("/pass-through-at-cap.bin", StatusCode::OK, cap),
        ("/unmodified-at-cap.bin", StatusCode::NOT_FOUND, cap),
    ] {
        let body = raw_body(len);
        origin.respond_to(Method::GET, &uri(path), raw_reply(status, None, &body));
        let expected = Expected::Relayed {
            status: status.as_u16(),
            content_encoding: None,
            body,
        };
        assert_served(path, &fetch(path).await, expected, cap);
    }
    for (path, status) in [
        ("/pass-through-over-cap.bin", StatusCode::OK),
        ("/unmodified-over-cap.bin", StatusCode::NOT_FOUND),
    ] {
        let body = raw_body(cap + 1);
        origin.respond_to(Method::GET, &uri(path), raw_reply(status, None, &body));
        assert_served(path, &fetch(path).await, Expected::BadGateway, cap);
    }

    for (path, len) in [
        ("/gzip-decoded-at-cap.bin", cap),
        ("/gzip-decoded-over-cap.bin", cap + 1),
    ] {
        let body = raw_body(len);
        let encoded = gzip(&body);
        origin.respond_to(
            Method::GET,
            &uri(path),
            raw_reply(StatusCode::OK, Some("gzip"), &encoded),
        );
        let expected = match (decoded, len > cap) {
            (true, true) => Expected::BadGateway,
            (true, false) => Expected::Relayed {
                status: 200,
                content_encoding: None,
                body,
            },
            (false, _) => Expected::Relayed {
                status: 200,
                content_encoding: Some("gzip"),
                body: encoded,
            },
        };
        assert_served(path, &fetch(path).await, expected, cap);
    }

    let path = "/gzip-encoded-over-cap.bin";
    let encoded = gzip(&incompressible_body(cap - 1));
    assert!(
        encoded.len() > cap,
        "should encode the incompressible body over the cap, though it decodes under it"
    );
    origin.respond_to(
        Method::GET,
        &uri(path),
        raw_reply(StatusCode::OK, Some("gzip"), &encoded),
    );
    assert_served(path, &fetch(path).await, Expected::BadGateway, cap);

    for (path, content_encoding) in [
        ("/identity-at-cap.html", None),
        ("/gzip-at-cap.html", Some("gzip")),
    ] {
        let page = html_page(cap, ORIGIN_LINKS);
        let body = match content_encoding {
            Some(_) => gzip(&page),
            None => page.clone(),
        };
        origin.respond_to(Method::GET, &uri(path), html_reply(content_encoding, &body));
        let expected = Expected::Processed {
            content_encoding: content_encoding.and(processed_coding),
            page: rewritten(&page),
        };
        assert_served(path, &fetch(path).await, expected, cap);
    }

    let path = "/identity-over-cap.html";
    origin.respond_to(
        Method::GET,
        &uri(path),
        html_reply(None, &html_page(cap + 1, ORIGIN_LINKS)),
    );
    assert_served(path, &fetch(path).await, Expected::BadGateway, cap);

    let path = "/gzip-over-cap.html";
    let encoded = gzip(&html_page(cap + 1, ORIGIN_LINKS));
    assert!(
        encoded.len() < cap,
        "should encode the over-cap page under the cap, so only its decoded size fails it"
    );
    origin.respond_to(Method::GET, &uri(path), html_reply(Some("gzip"), &encoded));
    assert_served(path, &fetch(path).await, Expected::BadGateway, cap);

    for (path, status, content_type) in [
        (
            "/pass-through-source-error.bin",
            StatusCode::OK,
            "application/octet-stream",
        ),
        (
            "/unmodified-source-error.bin",
            StatusCode::NOT_FOUND,
            "application/octet-stream",
        ),
        ("/processed-source-error.html", StatusCode::OK, "text/html"),
    ] {
        origin.respond_to(
            Method::GET,
            &uri(path),
            ScriptedReply::status(status)
                .header("content-type", content_type)
                .chunks(vec![
                    Bytes::from_static(b"<html><head></head><body>"),
                    Bytes::from_static(PARTIAL_BODY),
                ])
                .body_error(EdgeError::bad_gateway("example origin reset")),
        );
        assert_served(path, &fetch(path).await, Expected::BadGateway, cap);
    }
}

/// Checks that a rewrite growing past `max_buffered_body_bytes` fails with 502.
///
/// The adapter runs with [`OUTPUT_BOUND_BYTES`] as `max_buffered_body_bytes`, which is then
/// also its input cap. An HTML page one byte under that has no origin links, so the injected
/// markup grows its processed output past the bound. A raw body of the same length is relayed,
/// which shows that the input cap admits the page and the output bound fails it.
///
/// # Panics
///
/// Panics when the adapter serves either case differently.
pub async fn assert_output_bound<F, Fut>(origin: &ScriptedHttpClient, fetch: F)
where
    F: Fn(&'static str) -> Fut,
    Fut: Future<Output = Served>,
{
    let len = OUTPUT_BOUND_BYTES - 1;
    let path = "/output-bound-raw.bin";
    let body = raw_body(len);
    origin.respond_to(
        Method::GET,
        &uri(path),
        raw_reply(StatusCode::OK, None, &body),
    );
    let expected = Expected::Relayed {
        status: 200,
        content_encoding: None,
        body,
    };
    assert_served(path, &fetch(path).await, expected, OUTPUT_BOUND_BYTES);

    let path = "/output-bound.html";
    origin.respond_to(
        Method::GET,
        &uri(path),
        html_reply(None, &html_page(len, 0)),
    );
    assert_served(
        path,
        &fetch(path).await,
        Expected::BadGateway,
        OUTPUT_BOUND_BYTES,
    );
}

fn uri(path: &str) -> String {
    format!("{ORIGIN}{path}")
}

/// A raw body of `len` bytes in which each byte follows from its offset, so a lost or reordered
/// byte shows.
fn raw_body(len: usize) -> Vec<u8> {
    (0..=250_u8).cycle().take(len).collect()
}

/// `len` bytes of a fixed xorshift sequence, which gzip cannot shrink.
fn incompressible_body(len: usize) -> Vec<u8> {
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut body = Vec::with_capacity(len + 8);
    while body.len() < len {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        body.extend_from_slice(&state.to_le_bytes());
    }
    body.truncate(len);
    body
}

/// An HTML page exactly `len` bytes long with `origin_links` links to [`ORIGIN`].
fn html_page(len: usize, origin_links: usize) -> Vec<u8> {
    let mut page = b"<html><head><title>Example</title></head><body>".to_vec();
    for index in 0..origin_links {
        page.extend_from_slice(format!("<a href=\"{ORIGIN}/page-{index}\">example</a>").as_bytes());
    }
    let tail = b"</body></html>";
    assert!(
        page.len() + tail.len() <= len,
        "should fit the page's links in {len} bytes"
    );
    page.resize(len - tail.len(), b'a');
    page.extend_from_slice(tail);
    page
}

/// `page` with each link to [`ORIGIN`] pointed at [`PUBLIC_ORIGIN`], as the rewrite does.
fn rewritten(page: &[u8]) -> Vec<u8> {
    String::from_utf8(page.to_vec())
        .expect("should build a UTF-8 page")
        .replace(ORIGIN, PUBLIC_ORIGIN)
        .into_bytes()
}

fn raw_reply(status: StatusCode, content_encoding: Option<&str>, body: &[u8]) -> ScriptedReply {
    let reply = ScriptedReply::status(status).header("content-type", "application/octet-stream");
    with_coding(reply, content_encoding).body(body.to_vec())
}

fn html_reply(content_encoding: Option<&str>, body: &[u8]) -> ScriptedReply {
    let reply = ScriptedReply::status(StatusCode::OK).header("content-type", "text/html");
    with_coding(reply, content_encoding).body(body.to_vec())
}

fn with_coding(reply: ScriptedReply, content_encoding: Option<&str>) -> ScriptedReply {
    match content_encoding {
        Some(coding) => reply.header("content-encoding", coding),
        None => reply,
    }
}

/// Asserts that `served` is what `expected` describes, for the case at `path`.
fn assert_served(path: &str, served: &Served, expected: Expected, cap: usize) {
    match expected {
        Expected::Relayed {
            status,
            content_encoding,
            body,
        } => {
            assert_eq!(
                (served.status, served.content_encoding.as_deref()),
                (status, content_encoding),
                "{path} should be relayed with its status and coding"
            );
            assert_same_bytes(path, &served.body, &body);
        }
        Expected::Processed {
            content_encoding,
            page,
        } => {
            assert_eq!(
                (served.status, served.content_encoding.as_deref()),
                (200, content_encoding),
                "{path} should be processed and served in its coding"
            );
            let decoded = match content_encoding {
                Some(_) => gunzip(&served.body),
                None => served.body.clone(),
            };
            assert!(
                decoded.len() < cap,
                "{path} should process to fewer than {cap} bytes, so only the input cap fails a \
                 page one byte longer; got {} bytes",
                decoded.len()
            );
            let document = String::from_utf8(decoded).expect("should serve a UTF-8 page");
            assert!(
                document.contains("/static/tsjs="),
                "{path} should inject the TSJS tag"
            );
            assert_same_bytes(path, without_scripts(&document).as_bytes(), &page);
        }
        Expected::BadGateway => {
            assert_eq!(
                served.status, 502,
                "{path} should fail with the publisher's 502 before egress"
            );
            assert!(
                !served
                    .body
                    .windows(PARTIAL_BODY.len())
                    .any(|window| window == PARTIAL_BODY),
                "{path} should serve none of the origin's partial body"
            );
        }
    }
}

/// Asserts that `actual` equals `expected`, naming the first offset that differs.
fn assert_same_bytes(path: &str, actual: &[u8], expected: &[u8]) {
    let first_difference = actual
        .iter()
        .zip(expected)
        .position(|(actual, expected)| actual != expected);
    assert!(
        actual.len() == expected.len() && first_difference.is_none(),
        "{path} should keep every origin byte: served {} bytes, expected {}, first difference at \
         {first_difference:?}",
        actual.len(),
        expected.len()
    );
}

/// `document` without its `<script>` elements.
fn without_scripts(document: &str) -> String {
    let mut kept = String::with_capacity(document.len());
    let mut rest = document;
    while let Some(start) = rest.find("<script") {
        kept.push_str(&rest[..start]);
        let end = rest[start..]
            .find("</script>")
            .expect("should close each injected script");
        rest = &rest[start + end + "</script>".len()..];
    }
    kept.push_str(rest);
    kept
}

fn gunzip(bytes: &[u8]) -> Vec<u8> {
    let mut decoded = Vec::new();
    flate2::read::GzDecoder::new(bytes)
        .read_to_end(&mut decoded)
        .expect("should decode the served gzip body");
    decoded
}
