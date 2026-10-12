//! Shared, policy-free response inspection for registered identity capture modules.
//! Original response parts and bytes are never decoded or rewritten for the client.

use std::io::Read as _;

use bytes::Bytes;
use edgezero_core::body::Body as EdgeBody;
use flate2::read::MultiGzDecoder;
use futures::{StreamExt as _, stream};
use http::{Method, Response, StatusCode, header};

const CAPTURE_WIRE_LIMIT: usize = 64 * 1024;
const CAPTURE_DECODED_LIMIT: usize = 64 * 1024;

/// An integration's response capture capability.
///
/// The registry selects matching upstream routes and supplies only bounded,
/// successfully decoded response bytes to [`Self::observe`].
pub trait IdentityCapture: Send + Sync {
    /// Return the finite EID sources claimed by this integration.
    fn sources(&self) -> &'static [&'static str];
    /// Return whether response observation is active, without removing claims.
    fn enabled(&self) -> bool;
    /// Select an exact method and proxy path for inspection.
    fn matches(&self, method: &Method, path: &str) -> bool;
    /// Interpret a selected upstream response without changing forwarding.
    fn observe(
        &self,
        input: IdentityCaptureInput<'_>,
    ) -> Option<crate::ec::identity::IdentityObservation>;
}

/// Selected upstream response and already-buffered forwarding request bytes.
pub struct IdentityCaptureInput<'a> {
    /// Actual proxy request path used to distinguish token and revoke routes.
    pub path: &'a str,
    /// Borrowed POST forwarding buffer; inspection never reads it twice.
    pub request_body: &'a [u8],
    /// Upstream HTTP response status.
    pub status: StatusCode,
    /// Decoded response body, bounded separately from original wire bytes.
    pub decoded_body: &'a [u8],
}

/// Shared forwarding bytes, attached only to selected capture requests.
#[derive(Clone)]
pub(crate) struct IdentityRequestBody(pub Bytes);

/// Inspect a selected upstream response without changing what the client sees.
///
/// A stream is read only while its wire prefix fits the limit. On overflow or
/// upstream read error, the copied prefix precedes the untouched oversized
/// chunk or error and unread tail. An inspected stream can be returned buffered;
/// the response bytes remain identical.
/// `Content-Length` is deliberately not trusted for either bound.
pub(crate) async fn inspect_response(
    response: Response<EdgeBody>,
) -> (Response<EdgeBody>, Option<Vec<u8>>) {
    let (parts, body) = response.into_parts();
    let mut encodings = parts.headers.get_all(header::CONTENT_ENCODING).iter();
    let encoding = match (encodings.next(), encodings.next()) {
        (None, _) => Some(Encoding::Identity),
        (Some(value), None) if value.as_bytes().eq_ignore_ascii_case(b"identity") => {
            Some(Encoding::Identity)
        }
        (Some(value), None) if value.as_bytes().eq_ignore_ascii_case(b"gzip") => {
            Some(Encoding::Gzip)
        }
        _ => None,
    };
    let Some(encoding) = encoding else {
        return (Response::from_parts(parts, body), None);
    };

    match body {
        EdgeBody::Once(bytes) => {
            let decoded = (bytes.len() <= CAPTURE_WIRE_LIMIT)
                .then(|| decode(&bytes, encoding))
                .flatten();
            (Response::from_parts(parts, EdgeBody::Once(bytes)), decoded)
        }
        EdgeBody::Stream(mut upstream) => {
            // Only copy chunks that fit. A single oversized chunk stays in
            // its original Bytes allocation and is replayed untouched.
            let mut prefix = Vec::new();
            while let Some(item) = upstream.next().await {
                match item {
                    Ok(chunk) => {
                        if chunk.len() > CAPTURE_WIRE_LIMIT - prefix.len() {
                            let replay = stream::once(async { Ok(Bytes::from(prefix)) })
                                .chain(stream::once(async { Ok(chunk) }))
                                .chain(upstream);
                            return (
                                Response::from_parts(parts, EdgeBody::from_stream(replay)),
                                None,
                            );
                        }
                        prefix.extend_from_slice(&chunk);
                    }
                    Err(error) => {
                        let replay = stream::once(async { Ok(Bytes::from(prefix)) })
                            .chain(stream::once(async { Err(error) }))
                            .chain(upstream);
                        return (
                            Response::from_parts(parts, EdgeBody::from_stream(replay)),
                            None,
                        );
                    }
                }
            }

            let decoded = decode(&prefix, encoding);
            (Response::from_parts(parts, EdgeBody::from(prefix)), decoded)
        }
    }
}

#[derive(Clone, Copy)]
enum Encoding {
    Identity,
    Gzip,
}

fn decode(wire: &[u8], encoding: Encoding) -> Option<Vec<u8>> {
    match encoding {
        Encoding::Identity => (wire.len() <= CAPTURE_DECODED_LIMIT).then(|| wire.to_vec()),
        Encoding::Gzip => {
            // MultiGzDecoder also checks concatenated members instead of
            // accepting only the first gzip member and ignoring trailing data.
            let mut decoder = MultiGzDecoder::new(wire);
            let mut decoded = Vec::new();
            let mut scratch = [0_u8; 4096];
            loop {
                // One byte past the cap distinguishes an exact-size response
                // from one with more decoded data without allocating it.
                let remaining = CAPTURE_DECODED_LIMIT + 1 - decoded.len();
                let window = scratch.len().min(remaining);
                let count = decoder.read(&mut scratch[..window]).ok()?;
                if count == 0 {
                    return Some(decoded);
                }
                if count > CAPTURE_DECODED_LIMIT - decoded.len() {
                    return None;
                }
                decoded.extend_from_slice(&scratch[..count]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use bytes::Bytes;
    use flate2::{Compression, write::GzEncoder};
    use futures::{StreamExt as _, stream};
    use http::{Response, StatusCode, header};

    use super::{
        CAPTURE_DECODED_LIMIT, CAPTURE_WIRE_LIMIT, EdgeBody, IdentityCaptureInput, inspect_response,
    };

    #[test]
    fn capture_input_exposes_the_actual_route() {
        let input = IdentityCaptureInput {
            path: "/publisher/app/v2/identityLockr/revoke-consent",
            request_body: b"{}",
            status: StatusCode::OK,
            decoded_body: b"{}",
        };
        assert_eq!(input.path, "/publisher/app/v2/identityLockr/revoke-consent");
    }

    fn response(
        body: EdgeBody,
        encoding: Option<&str>,
        content_length: &str,
    ) -> Response<EdgeBody> {
        let mut builder = Response::builder()
            .status(StatusCode::CREATED)
            .header(header::CONTENT_LENGTH, content_length)
            .header("x-provider-header", "original");
        if let Some(encoding) = encoding {
            builder = builder.header(header::CONTENT_ENCODING, encoding);
        }
        builder.body(body).expect("should build test response")
    }

    async fn collect(body: EdgeBody) -> Vec<u8> {
        match body {
            EdgeBody::Once(bytes) => bytes.to_vec(),
            EdgeBody::Stream(mut chunks) => {
                let mut result = Vec::new();
                while let Some(chunk) = chunks.next().await {
                    result.extend_from_slice(&chunk.expect("should read replayed body chunk"));
                }
                result
            }
        }
    }

    fn gzip(input: &[u8]) -> Vec<u8> {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(input).expect("should encode gzip input");
        encoder.finish().expect("should finish gzip encoding")
    }

    #[tokio::test]
    async fn wire_exact_limit_and_one_more_preserve_original_parts() {
        for size in [CAPTURE_WIRE_LIMIT, CAPTURE_WIRE_LIMIT + 1] {
            let bytes = vec![b'a'; size];
            let original = response(EdgeBody::from(bytes.clone()), None, "1");
            let (returned, decoded) = inspect_response(original).await;
            assert_eq!(returned.status(), StatusCode::CREATED);
            assert_eq!(returned.headers()[header::CONTENT_LENGTH], "1");
            assert_eq!(returned.headers()["x-provider-header"], "original");
            assert_eq!(
                decoded,
                (size == CAPTURE_WIRE_LIMIT).then_some(bytes.clone())
            );
            assert_eq!(collect(returned.into_body()).await, bytes);
        }
    }

    #[tokio::test]
    async fn absent_length_and_duplicate_encoding_are_safe() {
        let plain = b"synthetic";
        let response = Response::builder()
            .status(StatusCode::ACCEPTED)
            .body(EdgeBody::from(plain.to_vec()))
            .expect("should build lengthless response");
        let (returned, decoded) = inspect_response(response).await;
        assert_eq!(returned.status(), StatusCode::ACCEPTED);
        assert!(!returned.headers().contains_key(header::CONTENT_LENGTH));
        assert_eq!(decoded.as_deref(), Some(plain.as_slice()));
        assert_eq!(collect(returned.into_body()).await, plain);

        let response = Response::builder()
            .header(header::CONTENT_ENCODING, "gzip")
            .header(header::CONTENT_ENCODING, "identity")
            .body(EdgeBody::from(gzip(plain)))
            .expect("should build duplicate-encoding response");
        let (returned, decoded) = inspect_response(response).await;
        assert!(decoded.is_none());
        assert_eq!(
            returned
                .headers()
                .get_all(header::CONTENT_ENCODING)
                .iter()
                .count(),
            2
        );
        assert_eq!(collect(returned.into_body()).await, gzip(plain));
    }

    #[tokio::test]
    async fn decoded_exact_limit_and_one_more_with_compressed_expansion() {
        for size in [CAPTURE_DECODED_LIMIT, CAPTURE_DECODED_LIMIT + 1] {
            let plain = vec![b'a'; size];
            let wire = gzip(&plain);
            assert!(wire.len() < CAPTURE_WIRE_LIMIT);
            let (returned, decoded) = inspect_response(response(
                EdgeBody::from(wire.clone()),
                Some("gzip"),
                "999999",
            ))
            .await;
            assert_eq!(decoded, (size == CAPTURE_DECODED_LIMIT).then_some(plain));
            assert_eq!(returned.headers()[header::CONTENT_ENCODING], "gzip");
            assert_eq!(returned.headers()[header::CONTENT_LENGTH], "999999");
            assert_eq!(collect(returned.into_body()).await, wire);
        }
    }

    #[tokio::test]
    async fn streaming_gzip_crosses_chunk_boundary_and_retains_headers() {
        let plain = b"a valid synthetic token";
        let wire = gzip(plain);
        let chunks = vec![
            Bytes::copy_from_slice(&wire[..7]),
            Bytes::copy_from_slice(&wire[7..]),
        ];
        let body = EdgeBody::stream(stream::iter(chunks));
        let (returned, decoded) = inspect_response(response(body, Some("gzip"), "1")).await;
        assert_eq!(decoded.as_deref(), Some(plain.as_slice()));
        assert_eq!(returned.headers()[header::CONTENT_LENGTH], "1");
        assert_eq!(collect(returned.into_body()).await, wire);
    }

    #[tokio::test]
    async fn unsupported_and_broken_encodings_do_not_change_bytes() {
        for (encoding, bytes) in [
            ("br", b"not gzip".as_slice()),
            ("gzip", b"broken gzip".as_slice()),
            ("gzip, br", b"not gzip".as_slice()),
        ] {
            let (returned, decoded) = inspect_response(response(
                EdgeBody::from(bytes.to_vec()),
                Some(encoding),
                "0",
            ))
            .await;
            assert!(decoded.is_none());
            assert_eq!(returned.headers()[header::CONTENT_ENCODING], encoding);
            assert_eq!(returned.headers()[header::CONTENT_LENGTH], "0");
            assert_eq!(collect(returned.into_body()).await, bytes);
        }
    }

    #[tokio::test]
    async fn streaming_oversized_chunk_replays_prefix_and_tail_without_copy() {
        let prefix = Bytes::from_static(b"prefix");
        let oversized = Bytes::from(vec![b'X'; CAPTURE_WIRE_LIMIT + 1]);
        let pointer = oversized.as_ptr();
        let tail = Bytes::from_static(b"tail");
        let body = EdgeBody::stream(stream::iter(vec![
            prefix.clone(),
            oversized.clone(),
            tail.clone(),
        ]));
        let (returned, decoded) = inspect_response(response(body, None, "2")).await;
        assert!(decoded.is_none());
        assert_eq!(returned.headers()[header::CONTENT_LENGTH], "2");
        let EdgeBody::Stream(mut replay) = returned.into_body() else {
            panic!("expected stream");
        };
        assert_eq!(
            replay
                .next()
                .await
                .expect("should replay prefix")
                .expect("should read prefix"),
            prefix
        );
        let replayed_large = replay
            .next()
            .await
            .expect("should replay oversized chunk")
            .expect("should read oversized chunk");
        assert_eq!(replayed_large.as_ptr(), pointer);
        assert_eq!(replayed_large, oversized);
        assert_eq!(
            replay
                .next()
                .await
                .expect("should replay tail")
                .expect("should read tail"),
            tail
        );
        assert!(replay.next().await.is_none());
    }

    #[tokio::test]
    async fn many_single_byte_chunks_use_one_prefix_and_return_original_bytes() {
        let chunks = (0..CAPTURE_WIRE_LIMIT).map(|_| Bytes::from_static(b"x"));
        let body = EdgeBody::stream(stream::iter(chunks));
        let (returned, decoded) = inspect_response(response(body, None, "0")).await;
        let expected = vec![b'x'; CAPTURE_WIRE_LIMIT];
        assert_eq!(decoded.as_deref(), Some(expected.as_slice()));
        assert!(matches!(returned.body(), EdgeBody::Once(_)));
        assert_eq!(collect(returned.into_body()).await, expected);
    }

    #[tokio::test]
    async fn streaming_total_overflow_at_plus_one_replays_every_byte() {
        let first = Bytes::from(vec![b'X'; CAPTURE_WIRE_LIMIT]);
        let tail = Bytes::from_static(b"Y");
        let body = EdgeBody::stream(stream::iter(vec![first.clone(), tail.clone()]));
        let (returned, decoded) = inspect_response(response(body, None, "0")).await;
        assert!(decoded.is_none());
        let EdgeBody::Stream(mut replay) = returned.into_body() else {
            panic!("expected stream");
        };
        assert_eq!(
            replay
                .next()
                .await
                .expect("should replay prefix")
                .expect("should read prefix"),
            first
        );
        assert_eq!(
            replay
                .next()
                .await
                .expect("should replay overflowing byte")
                .expect("should read overflowing byte"),
            tail
        );
        assert!(replay.next().await.is_none());
    }

    #[tokio::test]
    async fn streaming_read_error_is_replayed_in_order_with_remaining_tail() {
        let body = EdgeBody::from_stream(stream::iter(vec![
            Ok(Bytes::from_static(b"before")),
            Err(std::io::Error::other("upstream read failed")),
            Ok(Bytes::from_static(b"after")),
        ]));
        let (returned, decoded) = inspect_response(response(body, None, "0")).await;
        assert!(decoded.is_none());
        let EdgeBody::Stream(mut replay) = returned.into_body() else {
            panic!("expected stream");
        };
        assert_eq!(
            replay
                .next()
                .await
                .expect("should replay prefix")
                .expect("should read prefix"),
            "before"
        );
        assert!(
            replay
                .next()
                .await
                .expect("should replay upstream error")
                .expect_err("should retain upstream error")
                .to_string()
                .contains("upstream read failed")
        );
        assert_eq!(
            replay
                .next()
                .await
                .expect("should replay tail")
                .expect("should read tail"),
            "after"
        );
        assert!(replay.next().await.is_none());
    }

    #[tokio::test]
    async fn truncated_gzip_and_trailing_garbage_skip_capture() {
        let mut truncated = gzip(b"test");
        truncated.pop();
        let mut trailing = gzip(b"test");
        trailing.extend_from_slice(b"garbage");
        for wire in [truncated, trailing] {
            let (returned, decoded) =
                inspect_response(response(EdgeBody::from(wire.clone()), Some("gzip"), "0")).await;
            assert!(decoded.is_none());
            assert_eq!(collect(returned.into_body()).await, wire);
        }
    }
}
