use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::task::{Context, Poll};

use bytes::Bytes;
use http_body_util::{BodyExt as _, combinators::BoxBody};
use hyper::body::{Body, Frame, Incoming, SizeHint};

use super::connect::UpstreamSender;
use super::manager::{Lease, Manager};
use crate::commands::dev::proxy::metrics::ProxyMetrics;

const STREAMING: u8 = 0;
const COMPLETE: u8 = 1;
const FAILED: u8 = 2;

#[derive(Debug, derive_more::Display)]
pub enum ProxyBodyError {
    #[display("upstream request body failed")]
    Hyper(hyper::Error),
}

impl core::error::Error for ProxyBodyError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Hyper(error) => Some(error),
        }
    }
}

pub type ProxyRequestBody = BoxBody<Bytes, ProxyBodyError>;

pub struct RequestUploadBody {
    inner: ProxyRequestBody,
    state: Arc<AtomicU8>,
}

impl RequestUploadBody {
    #[must_use]
    pub fn new(inner: Incoming, known_empty: bool) -> (Self, Arc<AtomicU8>) {
        Self::from_boxed(inner.map_err(ProxyBodyError::Hyper).boxed(), known_empty)
    }

    #[must_use]
    pub fn empty() -> Self {
        let body = http_body_util::Empty::<Bytes>::new()
            .map_err(|never| match never {})
            .boxed();
        Self::from_boxed(body, true).0
    }

    fn from_boxed(inner: ProxyRequestBody, known_empty: bool) -> (Self, Arc<AtomicU8>) {
        let state = Arc::new(AtomicU8::new(if known_empty || inner.is_end_stream() {
            COMPLETE
        } else {
            STREAMING
        }));
        (
            Self {
                inner,
                state: Arc::clone(&state),
            },
            state,
        )
    }
}

impl Body for RequestUploadBody {
    type Data = Bytes;
    type Error = ProxyBodyError;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        match Pin::new(&mut self.inner).poll_frame(cx) {
            Poll::Ready(None) => {
                self.state.store(COMPLETE, Ordering::Release);
                Poll::Ready(None)
            }
            Poll::Ready(Some(Err(error))) => {
                self.state.store(FAILED, Ordering::Release);
                Poll::Ready(Some(Err(error)))
            }
            Poll::Ready(Some(Ok(frame))) => Poll::Ready(Some(Ok(frame))),
            Poll::Pending => Poll::Pending,
        }
    }

    fn is_end_stream(&self) -> bool {
        self.state.load(Ordering::Acquire) == COMPLETE
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

impl Drop for RequestUploadBody {
    fn drop(&mut self) {
        let _ = self
            .state
            .compare_exchange(STREAMING, FAILED, Ordering::AcqRel, Ordering::Acquire);
    }
}

pub struct PooledResponseBody {
    inner: Incoming,
    lease: Option<Lease<UpstreamSender>>,
    manager: Arc<Manager<UpstreamSender>>,
    upload_state: Arc<AtomicU8>,
    close_intent: bool,
    metrics: Arc<ProxyMetrics>,
    finalized: bool,
}

impl PooledResponseBody {
    pub fn new(
        inner: Incoming,
        lease: Lease<UpstreamSender>,
        manager: Arc<Manager<UpstreamSender>>,
        upload_state: Arc<AtomicU8>,
        close_intent: bool,
        metrics: Arc<ProxyMetrics>,
    ) -> Self {
        Self {
            inner,
            lease: Some(lease),
            manager,
            upload_state,
            close_intent,
            metrics,
            finalized: false,
        }
    }

    fn finalize(&mut self, response_complete: bool) {
        if self.finalized {
            return;
        }
        self.finalized = true;
        let Some(lease) = self.lease.take() else {
            return;
        };
        let reusable = can_reuse(
            response_complete,
            self.upload_state.load(Ordering::Acquire),
            self.close_intent,
            lease.connection.abort.is_finished(),
        );
        if reusable {
            self.metrics.record_request_completed();
            self.manager.return_idle(lease.connection);
        } else {
            self.metrics.record_request_failed();
            lease.connection.abort.abort();
        }
    }
}

fn can_reuse(
    response_complete: bool,
    upload_state: u8,
    close_intent: bool,
    driver_finished: bool,
) -> bool {
    response_complete && upload_state == COMPLETE && !close_intent && !driver_finished
}

impl Body for PooledResponseBody {
    type Data = Bytes;
    type Error = hyper::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        match Pin::new(&mut self.inner).poll_frame(cx) {
            Poll::Ready(None) => {
                self.finalize(true);
                Poll::Ready(None)
            }
            Poll::Ready(Some(Err(error))) => {
                self.finalize(false);
                Poll::Ready(Some(Err(error)))
            }
            Poll::Ready(Some(Ok(frame))) => {
                if self.inner.is_end_stream() {
                    self.finalize(true);
                }
                Poll::Ready(Some(Ok(frame)))
            }
            Poll::Pending => Poll::Pending,
        }
    }

    fn is_end_stream(&self) -> bool {
        self.finalized
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

impl Drop for PooledResponseBody {
    fn drop(&mut self) {
        self.finalize(false);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    use hyper::HeaderMap;

    use super::*;

    struct ScriptedBody {
        frames: VecDeque<Frame<Bytes>>,
        polls: Arc<AtomicUsize>,
    }

    impl Body for ScriptedBody {
        type Data = Bytes;
        type Error = ProxyBodyError;

        fn poll_frame(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
            self.polls.fetch_add(1, AtomicOrdering::Relaxed);
            Poll::Ready(self.frames.pop_front().map(Ok))
        }

        fn is_end_stream(&self) -> bool {
            self.frames.is_empty()
        }
    }

    #[tokio::test]
    async fn upload_completes_only_after_terminal_eos_following_trailers() {
        let polls = Arc::new(AtomicUsize::new(0));
        let mut trailers = HeaderMap::new();
        trailers.insert("x-trailer", hyper::header::HeaderValue::from_static("done"));
        let scripted = ScriptedBody {
            frames: VecDeque::from([
                Frame::data(Bytes::from_static(b"data")),
                Frame::trailers(trailers),
            ]),
            polls,
        };
        let (mut body, state) = RequestUploadBody::from_boxed(scripted.boxed(), false);

        assert!(body.frame().await.is_some());
        assert_eq!(state.load(Ordering::Acquire), STREAMING);
        assert!(body.frame().await.is_some());
        assert_eq!(
            state.load(Ordering::Acquire),
            STREAMING,
            "trailers are not terminal until the following EOS poll"
        );
        assert!(body.frame().await.is_none());
        assert_eq!(state.load(Ordering::Acquire), COMPLETE);
    }

    #[tokio::test]
    async fn upload_adapter_does_not_prepoll_or_buffer_frames() {
        let polls = Arc::new(AtomicUsize::new(0));
        let scripted = ScriptedBody {
            frames: VecDeque::from([
                Frame::data(Bytes::from_static(b"one")),
                Frame::data(Bytes::from_static(b"two")),
            ]),
            polls: Arc::clone(&polls),
        };
        let (mut body, _state) = RequestUploadBody::from_boxed(scripted.boxed(), false);

        assert_eq!(polls.load(AtomicOrdering::Relaxed), 0);
        assert!(body.frame().await.is_some());
        assert_eq!(polls.load(AtomicOrdering::Relaxed), 1);
        assert!(body.frame().await.is_some());
        assert_eq!(polls.load(AtomicOrdering::Relaxed), 2);
    }

    #[test]
    fn dropping_upload_before_eos_marks_it_failed() {
        let polls = Arc::new(AtomicUsize::new(0));
        let scripted = ScriptedBody {
            frames: VecDeque::from([Frame::data(Bytes::from_static(b"pending"))]),
            polls,
        };
        let (body, state) = RequestUploadBody::from_boxed(scripted.boxed(), false);
        drop(body);
        assert_eq!(state.load(Ordering::Acquire), FAILED);
    }

    #[test]
    fn response_eos_while_upload_streams_is_never_reusable() {
        assert!(!can_reuse(true, STREAMING, false, false));
        assert!(!can_reuse(true, FAILED, false, false));
        assert!(!can_reuse(true, COMPLETE, true, false));
        assert!(!can_reuse(true, COMPLETE, false, true));
        assert!(can_reuse(true, COMPLETE, false, false));
    }
}
