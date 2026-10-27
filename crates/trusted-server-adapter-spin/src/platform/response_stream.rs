//! Origin response bodies passed on as Spin's outbound HTTP delivers them.
//!
//! A streamed response never holds more than the chunk the runtime handed
//! over last. The caller's byte limit and the origin's declared length are
//! checked chunk by chunk, so an oversized, truncated or failed origin body
//! ends the stream with an error instead of reaching the caller as a
//! complete one.

use std::fmt::Display;
use std::pin::Pin;

use bytes::Bytes;
use futures::{Stream, StreamExt as _};

/// The source and the number of bytes already yielded from it.
struct Progress<S> {
    source: Pin<Box<S>>,
    yielded: usize,
}

/// A byte count in the unit of a declared `Content-Length`. A count no
/// `u64` can hold is past any declared length.
fn declared_units(bytes: usize) -> u64 {
    u64::try_from(bytes).unwrap_or(u64::MAX)
}

/// Wrap a runtime body stream as a [`Body::Stream`](edgezero_core::body::Body::Stream).
///
/// `limit` is the caller's `max_response_body_bytes`. The chunk that would
/// carry the running total past it is replaced by an error, and nothing
/// after it is read.
///
/// `expected_length` is the origin's `Content-Length`. Spin hands the body
/// over exactly as the origin encoded it, so the declared length describes
/// the bytes this stream yields. A body that ends short of the declared
/// length, or runs past it, ends with an error rather than reaching the
/// caller as a complete one, whatever the host's own framing checks report.
/// Pass `None` when the origin declared no length.
///
/// A source error is passed on as a stream error and ends the stream.
pub(super) fn bounded_origin_stream<S, B, E>(
    source: S,
    limit: Option<usize>,
    expected_length: Option<u64>,
) -> edgezero_core::body::Body
where
    S: Stream<Item = Result<B, E>> + 'static,
    B: Into<Bytes>,
    E: Display,
{
    let initial = Progress {
        source: Box::pin(source),
        yielded: 0,
    };
    let stream = futures::stream::unfold(Some(initial), move |state| async move {
        let mut progress = state?;
        match progress.source.next().await {
            None => {
                if let Some(expected) = expected_length
                    && declared_units(progress.yielded) != expected
                {
                    return Some((
                        Err(std::io::Error::other(format!(
                            "Spin outbound response body ended after {} of its declared {expected} bytes",
                            progress.yielded
                        ))),
                        None,
                    ));
                }
                None
            }
            Some(Err(error)) => Some((
                Err(std::io::Error::other(format!(
                    "failed to read Spin outbound response body: {error}"
                ))),
                None,
            )),
            Some(Ok(chunk)) => {
                let chunk: Bytes = chunk.into();
                let Some(next_yielded) = progress.yielded.checked_add(chunk.len()) else {
                    return Some((
                        Err(std::io::Error::other(
                            "Spin outbound response body byte count overflowed",
                        )),
                        None,
                    ));
                };
                if let Some(limit) = limit
                    && next_yielded > limit
                {
                    return Some((
                        Err(std::io::Error::other(format!(
                            "Spin outbound response body exceeded caller limit of {limit} bytes"
                        ))),
                        None,
                    ));
                }
                if let Some(expected) = expected_length
                    && declared_units(next_yielded) > expected
                {
                    return Some((
                        Err(std::io::Error::other(format!(
                            "Spin outbound response body ran past its declared {expected} bytes"
                        ))),
                        None,
                    ));
                }
                progress.yielded = next_yielded;
                Some((Ok(chunk), Some(progress)))
            }
        }
    });
    edgezero_core::body::Body::from_stream(stream)
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::FutureExt as _;

    type ChunkResult = Result<Vec<u8>, std::io::Error>;

    fn into_stream(
        body: edgezero_core::body::Body,
    ) -> futures::stream::LocalBoxStream<'static, Result<Bytes, anyhow::Error>> {
        body.into_stream()
            .expect("a streamed origin body should stay a stream")
    }

    #[tokio::test]
    async fn each_chunk_is_yielded_before_the_origin_finishes() {
        let (sender, receiver) = futures::channel::mpsc::unbounded::<ChunkResult>();
        sender
            .unbounded_send(Ok(b"ab".to_vec()))
            .expect("should queue the first chunk");
        let mut stream = into_stream(bounded_origin_stream(receiver, None, None));

        let first = stream
            .next()
            .await
            .expect("the first chunk should arrive")
            .expect("the first chunk should be readable");
        assert_eq!(
            first.as_ref(),
            b"ab",
            "should pass the first chunk on as it arrived"
        );
        assert!(
            stream.next().now_or_never().is_none(),
            "should wait for the origin rather than report an end it has not sent"
        );

        sender
            .unbounded_send(Ok(b"cd".to_vec()))
            .expect("should queue the second chunk");
        drop(sender);
        let second = stream
            .next()
            .await
            .expect("the second chunk should arrive")
            .expect("the second chunk should be readable");
        assert_eq!(second.as_ref(), b"cd", "should pass the later chunk on");
        assert!(stream.next().await.is_none(), "should end with the origin");
    }

    #[tokio::test]
    async fn an_unbounded_stream_passes_more_than_the_buffered_ceiling() {
        // Eleven 1 MiB chunks cross the 8 MiB ceiling the buffered path
        // enforces. The chunks share one allocation, so the test holds 1 MiB.
        let chunk = Bytes::from(vec![7u8; 1024 * 1024]);
        let chunks = std::iter::repeat_n(chunk, 11).map(Ok::<_, std::io::Error>);
        let mut stream = into_stream(bounded_origin_stream(
            futures::stream::iter(chunks),
            None,
            None,
        ));

        let mut total = 0usize;
        while let Some(chunk) = stream.next().await {
            total += chunk.expect("an unbounded chunk should be readable").len();
        }
        assert_eq!(total, 11 * 1024 * 1024, "should pass every byte on");
    }

    #[tokio::test]
    async fn the_chunk_that_crosses_the_caller_limit_ends_the_stream() {
        let source = futures::stream::iter([
            Ok::<_, std::io::Error>(b"ab".to_vec()),
            Ok(b"cd".to_vec()),
            Ok(b"e".to_vec()),
            Ok(b"never read".to_vec()),
        ]);
        let mut stream = into_stream(bounded_origin_stream(source, Some(4), None));

        for expected in [b"ab", b"cd"] {
            let chunk = stream
                .next()
                .await
                .expect("a chunk within the limit should arrive")
                .expect("a chunk within the limit should be readable");
            assert_eq!(
                chunk.as_ref(),
                expected,
                "should pass chunks up to the exact limit"
            );
        }
        let error = stream
            .next()
            .await
            .expect("the crossing chunk should surface")
            .expect_err("the crossing chunk must be refused");
        assert!(
            error.to_string().contains("limit of 4 bytes"),
            "should report the enforced limit: {error}"
        );
        assert!(
            stream.next().await.is_none(),
            "should read nothing after the refused chunk"
        );
    }

    #[tokio::test]
    async fn an_origin_failure_is_passed_on_and_ends_the_stream() {
        let source = futures::stream::iter([
            Ok(b"ab".to_vec()),
            Err(std::io::Error::other("connection reset by origin")),
            Ok(b"never read".to_vec()),
        ]);
        let mut stream = into_stream(bounded_origin_stream(source, Some(64), None));

        let first = stream
            .next()
            .await
            .expect("the chunk before the failure should arrive")
            .expect("the chunk before the failure should be readable");
        assert_eq!(first.as_ref(), b"ab");
        let error = stream
            .next()
            .await
            .expect("the failure should surface")
            .expect_err("the origin failure must not look like data");
        assert!(
            error.to_string().contains("connection reset by origin"),
            "should carry the origin's failure: {error}"
        );
        assert!(stream.next().await.is_none(), "should end at the failure");
    }

    #[tokio::test]
    async fn a_body_cut_short_of_its_declared_length_is_an_error() {
        // An origin connection that closes early must not end the body cleanly.
        let source = futures::stream::iter([Ok::<_, std::io::Error>(b"ab".to_vec())]);
        let mut stream = into_stream(bounded_origin_stream(source, None, Some(4)));

        let first = stream
            .next()
            .await
            .expect("the delivered bytes should arrive")
            .expect("the delivered bytes should be readable");
        assert_eq!(first.as_ref(), b"ab");
        let error = stream
            .next()
            .await
            .expect("the early end should surface")
            .expect_err("a truncated body must not end cleanly");
        assert!(
            error.to_string().contains("2 of its declared 4 bytes"),
            "should report how much of the declared body arrived: {error}"
        );
    }

    #[tokio::test]
    async fn a_body_running_past_its_declared_length_is_an_error() {
        let source =
            futures::stream::iter([Ok::<_, std::io::Error>(b"abc".to_vec()), Ok(b"de".to_vec())]);
        let mut stream = into_stream(bounded_origin_stream(source, None, Some(4)));

        stream
            .next()
            .await
            .expect("the bytes within the declared length should arrive")
            .expect("the bytes within the declared length should be readable");
        let error = stream
            .next()
            .await
            .expect("the overrun should surface")
            .expect_err("bytes past the declared length must be refused");
        assert!(
            error.to_string().contains("declared 4 bytes"),
            "should report the declared length: {error}"
        );
    }

    #[tokio::test]
    async fn a_body_matching_its_declared_length_ends_cleanly() {
        let source =
            futures::stream::iter([Ok::<_, std::io::Error>(b"ab".to_vec()), Ok(b"cd".to_vec())]);
        let mut stream = into_stream(bounded_origin_stream(source, Some(4), Some(4)));

        let mut body = Vec::new();
        while let Some(chunk) = stream.next().await {
            body.extend_from_slice(&chunk.expect("a declared-length body should be readable"));
        }
        assert_eq!(body, b"abcd", "should deliver the whole declared body");
    }
}
