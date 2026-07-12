use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::task::{Context, Poll};

use bytes::Bytes;
use hyper::body::{Body, Frame, Incoming, SizeHint};

use super::connect::UpstreamSender;
use super::manager::{Lease, Manager};
use crate::commands::dev::proxy::metrics::ProxyMetrics;

const STREAMING: u8 = 0;
const COMPLETE: u8 = 1;
const FAILED: u8 = 2;

pub struct RequestUploadBody {
    inner: Incoming,
    state: Arc<AtomicU8>,
}

impl RequestUploadBody {
    #[must_use]
    pub fn new(inner: Incoming, known_empty: bool) -> (Self, Arc<AtomicU8>) {
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
    type Error = hyper::Error;

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
            Poll::Ready(Some(Ok(frame))) => {
                if self.inner.is_end_stream() {
                    self.state.store(COMPLETE, Ordering::Release);
                }
                Poll::Ready(Some(Ok(frame)))
            }
            Poll::Pending => Poll::Pending,
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
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
        let reusable = response_complete
            && self.upload_state.load(Ordering::Acquire) == COMPLETE
            && !self.close_intent
            && !lease.connection.abort.is_finished();
        if reusable {
            self.metrics.record_request_completed();
            self.manager.return_idle(lease.connection);
        } else {
            self.metrics.record_request_failed();
            lease.connection.abort.abort();
        }
    }
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
