//! Native bridge regressions. These are transport tests, not full TS route acceptance.

use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use async_trait::async_trait;
use edgezero_adapter_axum::outbound::AxumOutboundClient;
use edgezero_core::body::{Body, BodyContent};
use edgezero_core::error::EdgeError;
use edgezero_core::http::{HeaderMap, Method, StatusCode, header, request_builder};
use edgezero_core::outbound::{
    ContentTypeProvenance, HttpClient, OutboundBatch, OutboundBatchDriverEvent, OutboundBatchItem,
    OutboundHttpClient, OutboundRequest, OutboundResponse, OutboundSlotResult,
};
use edgezero_core::time::{Deadline, MonotonicClock};
use futures::{Stream, StreamExt as _, stream};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use trusted_server_adapter_axum::platform::AxumPlatformHttpClient;
use trusted_server_core::platform::{PlatformHttpClient as _, PlatformHttpRequest};

fn bridge() -> AxumPlatformHttpClient {
    let clock = MonotonicClock::default();
    let client = AxumOutboundClient::try_with_clock(clock.clone())
        .expect("should construct native RC client");
    AxumPlatformHttpClient::new(HttpClient::with_client(client), clock)
}

fn request(url: &str, backend: &str) -> PlatformHttpRequest {
    PlatformHttpRequest::new(
        request_builder()
            .uri(url)
            .body(Body::empty())
            .expect("should build request"),
        backend,
    )
}

async fn body_bytes(body: Body) -> Vec<u8> {
    match body.into_content() {
        BodyContent::Once(bytes) => bytes.to_vec(),
        BodyContent::Stream(mut stream) => {
            let mut bytes = Vec::new();
            while let Some(chunk) = stream.next().await {
                bytes.extend_from_slice(&chunk.expect("should read body chunk"));
            }
            bytes
        }
    }
}

async fn read_request_head(socket: &mut tokio::net::TcpStream) {
    let mut head = Vec::new();
    let mut chunk = [0; 2048];
    loop {
        let count = socket
            .read(&mut chunk)
            .await
            .expect("should read request head");
        assert!(count > 0, "should not close before the request head");
        head.extend_from_slice(&chunk[..count]);
        assert!(
            head.len() <= 64 * 1024,
            "should bound fixture request headers"
        );
        if head.windows(4).any(|window| window == b"\r\n\r\n") {
            return;
        }
    }
}

async fn raw_server(response: &'static [u8]) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("should bind raw origin");
    let address = listener.local_addr().expect("should read origin address");
    tokio::spawn(async move {
        let (mut socket, _) = listener
            .accept()
            .await
            .expect("should accept origin request");
        read_request_head(&mut socket).await;
        socket
            .write_all(response)
            .await
            .expect("should send origin response");
    });
    format!("http://{address}/")
}

#[tokio::test]
async fn redirects_are_exposed_for_core_per_hop_authorization() {
    let url = raw_server(
        b"HTTP/1.1 307 Temporary Redirect\r\nLocation: https://example.com/next\r\nContent-Length: 0\r\n\r\n",
    ).await;
    let response = bridge()
        .send(request(&url, "redirect"))
        .await
        .expect("should return redirect without following it")
        .response;
    assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(
        response.headers()[header::LOCATION],
        "https://example.com/next"
    );
}

#[tokio::test]
async fn duplicate_cookies_survive_and_hop_headers_do_not() {
    let url = raw_server(
        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nSet-Cookie: first=1\r\nSet-Cookie: second=2\r\nConnection: close, x-remove\r\nX-Remove: hidden\r\n\r\nok",
    ).await;
    let response = bridge()
        .send(request(&url, "cookies"))
        .await
        .expect("should return response")
        .response;
    assert_eq!(
        response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .count(),
        2
    );
    assert!(!response.headers().contains_key(header::CONNECTION));
    assert!(!response.headers().contains_key("x-remove"));
    assert_eq!(body_bytes(response.into_body()).await, b"ok");
}

#[tokio::test]
async fn rc_owns_gzip_decoding_and_ts_receives_matching_identity_metadata() {
    let url = raw_server(
        b"HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: 22\r\n\r\n\x1f\x8b\x08\x00\x00\x00\x00\x00\x00\x03\xcb\xcf\x06\x00\x47\xdd\xdc\x79\x02\x00\x00\x00",
    ).await;
    let response = bridge()
        .send(request(&url, "gzip"))
        .await
        .expect("should decode gzip once in RC")
        .response;
    assert!(!response.headers().contains_key(header::CONTENT_ENCODING));
    assert!(!response.headers().contains_key(header::CONTENT_LENGTH));
    assert_eq!(body_bytes(response.into_body()).await, b"ok");
}

#[tokio::test]
async fn dropping_streamed_response_releases_the_origin_connection() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("should bind streaming origin");
    let address = listener.local_addr().expect("should read address");
    let origin = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("should accept request");
        let mut buffer = [0; 4096];
        read_request_head(&mut socket).await;
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1000000\r\n\r\nx")
            .await
            .expect("should write response head");
        tokio::time::timeout(Duration::from_secs(1), socket.read(&mut buffer)).await
    });
    let response = bridge()
        .send(request(&format!("http://{address}/"), "dropped-stream").with_stream_response())
        .await
        .expect("should receive stream");
    drop(response);
    let closed = origin
        .await
        .expect("should join origin")
        .expect("should release origin promptly");
    assert!(
        matches!(closed, Ok(0)) || closed.is_err(),
        "should close rather than leak the unread response"
    );
}

#[tokio::test]
async fn caller_response_bound_applies_to_send_and_pending() {
    for pending in [false, true] {
        let url = raw_server(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nbody").await;
        let mut outbound = request(&url, "bounded");
        outbound.max_response_body_bytes = 3;
        let client = bridge();
        let result = if pending {
            let handle = client
                .send_async(outbound)
                .await
                .expect("should start bounded request");
            client.wait(handle).await
        } else {
            client.send(outbound).await
        };
        assert!(
            result.is_err(),
            "should reject a response over the caller's bound"
        );
    }
}

#[tokio::test]
async fn streamed_upload_is_bounded_before_network_work() {
    for pending in [false, true] {
        let source = stream::iter([Ok(Body::from("oversized")
            .into_bytes()
            .expect("should have bytes"))]);
        let edge_request = request_builder()
            .method(Method::POST)
            .uri("http://example.com/")
            .body(Body::from_stream(source))
            .expect("should build streaming upload");
        let mut outbound = PlatformHttpRequest::new(edge_request, "upload");
        outbound.max_request_body_bytes = 3;
        let rejected = if pending {
            bridge().send_async(outbound).await.is_err()
        } else {
            bridge().send(outbound).await.is_err()
        };
        assert!(rejected, "should bound upload before network work");
    }
}

#[tokio::test]
async fn pending_post_preserves_streamed_upload_and_duplicate_request_headers() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("should bind POST origin");
    let address = listener.local_addr().expect("should read origin address");
    let origin = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("should accept POST");
        let mut request = Vec::new();
        let mut buffer = [0; 4096];
        loop {
            let read = socket
                .read(&mut buffer)
                .await
                .expect("should read POST bytes");
            assert_ne!(read, 0, "should not truncate the uploaded body");
            request.extend_from_slice(&buffer[..read]);
            if let Some(head_end) = request.windows(4).position(|window| window == b"\r\n\r\n")
                && request.len() >= head_end + 4 + 7
            {
                break;
            }
        }
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
            .await
            .expect("should respond to POST");
        request
    });
    let source = stream::iter([
        Ok(Body::from("one")
            .into_bytes()
            .expect("should have first chunk")),
        Ok(Body::from("-two")
            .into_bytes()
            .expect("should have second chunk")),
    ]);
    let outbound = request_builder()
        .method(Method::POST)
        .uri(format!("http://{address}/"))
        .header(header::HOST, "publisher.example.com")
        .header("x-example", "first")
        .header("x-example", "second")
        .body(Body::from_stream(source))
        .expect("should build streamed POST");
    let client = bridge();
    let pending = client
        .send_async(PlatformHttpRequest::new(outbound, "post"))
        .await
        .expect("should start POST batch");
    tokio::time::timeout(Duration::from_secs(1), client.wait(pending))
        .await
        .expect("should finish POST promptly")
        .expect("should complete POST");
    let received = origin.await.expect("should join POST origin");
    let received = String::from_utf8(received).expect("should have textual fixture bytes");
    assert!(received.starts_with("POST / HTTP/1.1\r\n"));
    assert!(received.ends_with("\r\n\r\none-two"));
    assert!(received.contains("host: publisher.example.com\r\n"));
    assert!(received.contains("x-example: first\r\n"));
    assert!(received.contains("x-example: second\r\n"));
}

#[tokio::test]
async fn expired_caller_deadline_is_rejected_before_network_work() {
    for pending in [false, true] {
        let clock = MonotonicClock::default();
        let transport = AxumOutboundClient::try_with_clock(clock.clone())
            .expect("should construct native transport");
        let client = AxumPlatformHttpClient::new(HttpClient::with_client(transport), clock.clone());
        let mut outbound = request("http://example.com/", "expired");
        outbound.deadline = Some(clock.deadline_after(Duration::ZERO));
        let rejected = if pending {
            client.send_async(outbound).await.is_err()
        } else {
            client.send(outbound).await.is_err()
        };
        assert!(rejected, "should reject expired absolute caller deadline");
    }
}

#[tokio::test]
async fn bodyless_streaming_response_keeps_head_representation_length() {
    let url = raw_server(b"HTTP/1.1 200 OK\r\nContent-Length: 9000\r\n\r\n").await;
    let edge_request = request_builder()
        .method(Method::HEAD)
        .uri(url)
        .body(Body::empty())
        .expect("should build HEAD request");
    let response = bridge()
        .send(PlatformHttpRequest::new(edge_request, "head").with_stream_response())
        .await
        .expect("should finish bodyless response")
        .response;
    assert_eq!(response.headers()[header::CONTENT_LENGTH], "9000");
    assert!(body_bytes(response.into_body()).await.is_empty());
}

#[tokio::test]
async fn total_timeout_covers_waiting_for_response_headers() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("should bind stalled origin");
    let address = listener.local_addr().expect("should read address");
    let origin = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("should accept request");
        read_request_head(&mut socket).await;
        tokio::time::sleep(Duration::from_secs(5)).await;
    });
    let mut outbound = request(&format!("http://{address}/"), "timeout");
    outbound.timeout = Some(Duration::from_millis(30));
    let result = tokio::time::timeout(Duration::from_secs(1), bridge().send(outbound))
        .await
        .expect("should enforce total transport timeout");
    origin.abort();
    assert!(result.is_err(), "should reject stalled origin");
}

#[tokio::test]
async fn streamed_body_retains_total_timeout_after_response_head() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("should bind streaming origin");
    let address = listener.local_addr().expect("should read address");
    let origin = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("should accept request");
        read_request_head(&mut socket).await;
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nx")
            .await
            .expect("should write partial response");
        tokio::time::sleep(Duration::from_secs(5)).await;
    });
    let mut outbound =
        request(&format!("http://{address}/"), "stream-timeout").with_stream_response();
    outbound.timeout = Some(Duration::from_millis(100));
    let response = bridge()
        .send(outbound)
        .await
        .expect("should receive response head")
        .response;
    let mut stream = response
        .into_body()
        .into_stream()
        .expect("should preserve streamed response body");
    let result = tokio::time::timeout(Duration::from_secs(1), async {
        while let Some(chunk) = stream.next().await {
            if chunk.is_err() {
                return true;
            }
        }
        false
    })
    .await
    .expect("should enforce total streamed-body timeout");
    origin.abort();
    assert!(result, "should surface a body timeout instead of clean EOF");
}

#[tokio::test]
async fn separate_rc_batches_progress_concurrently_during_select() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("should bind fanout origin");
    let address = listener.local_addr().expect("should read origin address");
    let origin = tokio::spawn(async move {
        let mut sockets = Vec::new();
        for _ in 0..2 {
            let (mut socket, _) = listener
                .accept()
                .await
                .expect("should accept each fanout request");
            read_request_head(&mut socket).await;
            sockets.push(socket);
        }
        // Neither origin answers until both requests arrive. Sequential sends
        // would deadlock here and fail the bounded wait below.
        for mut socket in sockets.into_iter().rev() {
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                .await
                .expect("should answer fanout request");
        }
    });
    let client = bridge();
    let first = client
        .send_async(request(&format!("http://{address}/first"), "first"))
        .await
        .expect("should start first handle");
    let second = client
        .send_async(request(&format!("http://{address}/second"), "second"))
        .await
        .expect("should start second handle");
    let selected = tokio::time::timeout(Duration::from_secs(1), client.select(vec![first, second]))
        .await
        .expect("should progress both requests concurrently")
        .expect("should select a response");
    let completed_backend = selected
        .ready
        .expect("should complete response")
        .backend_name
        .expect("should identify backend");
    assert_eq!(selected.remaining.len(), 1);
    let remaining_backend = selected.remaining[0]
        .backend_name()
        .expect("should retain other backend")
        .to_owned();
    assert_ne!(completed_backend, remaining_backend);
    client
        .wait(
            selected
                .remaining
                .into_iter()
                .next()
                .expect("should retain one handle"),
        )
        .await
        .expect("should complete second response");
    origin.await.expect("should join fanout origin");
}

fn terminal(index: usize) -> OutboundBatchDriverEvent {
    OutboundBatchDriverEvent::Item(OutboundBatchItem::new(
        index,
        OutboundSlotResult::new(
            Duration::ZERO,
            Ok(OutboundResponse::new(
                Method::GET,
                StatusCode::OK,
                HeaderMap::new(),
                Body::from("ok"),
                MonotonicClock::default(),
                ContentTypeProvenance::Unknown,
            )),
        ),
    ))
}

enum Event {
    Terminal(usize),
    Cutoff,
    Failure,
}

impl Event {
    fn into_driver_event(self) -> OutboundBatchDriverEvent {
        match self {
            Self::Terminal(index) => terminal(index),
            Self::Cutoff => OutboundBatchDriverEvent::Cutoff,
            Self::Failure => {
                OutboundBatchDriverEvent::Failed(EdgeError::bad_request("trailing driver failure"))
            }
        }
    }
}

enum Driver {
    Events(Vec<Event>),
    Pending(Arc<AtomicBool>),
}

struct PendingDriver(Arc<AtomicBool>);

impl Stream for PendingDriver {
    type Item = OutboundBatchDriverEvent;
    fn poll_next(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Poll::Pending
    }
}

impl Drop for PendingDriver {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

struct TestClient(Mutex<VecDeque<Driver>>);

#[async_trait(?Send)]
impl OutboundHttpClient for TestClient {
    async fn send(&self, _: OutboundRequest) -> Result<OutboundResponse, EdgeError> {
        Err(EdgeError::bad_request("test client only supports batches"))
    }

    fn start_batch_until(&self, requests: Vec<OutboundRequest>, _: Deadline) -> OutboundBatch {
        assert_eq!(
            requests.len(),
            1,
            "should create one independently cancellable RC batch per TS handle"
        );
        match self
            .0
            .lock()
            .expect("should lock scenarios")
            .pop_front()
            .expect("should have scenario")
        {
            Driver::Events(events) => OutboundBatch::from_driver(
                1,
                stream::iter(events.into_iter().map(Event::into_driver_event)),
            ),
            Driver::Pending(dropped) => OutboundBatch::from_driver(1, PendingDriver(dropped)),
        }
    }
}

fn scenario_bridge(drivers: Vec<Driver>) -> AxumPlatformHttpClient {
    AxumPlatformHttpClient::new(
        HttpClient::with_client(TestClient(Mutex::new(VecDeque::from(drivers)))),
        MonotonicClock::default(),
    )
}

#[tokio::test]
async fn batch_cutoff_premature_eof_bad_index_and_trailing_failure_are_provider_errors() {
    for events in [
        vec![Event::Cutoff],
        vec![],
        vec![Event::Terminal(1)],
        vec![Event::Terminal(0), Event::Failure],
        vec![Event::Terminal(0), Event::Terminal(0)],
    ] {
        let client = scenario_bridge(vec![Driver::Events(events)]);
        let handle = client
            .send_async(request("http://example.com/", "failed"))
            .await
            .expect("should construct pending handle");
        let selected = client
            .select(vec![handle])
            .await
            .expect("should return per-provider error");
        assert!(
            selected.ready.is_err(),
            "should not report success without valid driver EOF"
        );
        assert_eq!(selected.failed_backend_name.as_deref(), Some("failed"));
        assert!(selected.remaining.is_empty());
    }
}

#[tokio::test]
async fn select_keeps_backend_names_when_inputs_are_reordered_and_drop_cancels_remaining() {
    let cancelled = Arc::new(AtomicBool::new(false));
    let client = scenario_bridge(vec![
        Driver::Pending(Arc::clone(&cancelled)),
        Driver::Events(vec![Event::Terminal(0)]),
    ]);
    let slow = client
        .send_async(request("http://example.com/slow", "slow"))
        .await
        .expect("should start slow request");
    let fast = client
        .send_async(request("http://example.com/fast", "fast"))
        .await
        .expect("should start fast request");
    let result = client
        .select(vec![fast, slow])
        .await
        .expect("should select completion");
    assert_eq!(
        result
            .ready
            .expect("should finish original RC slot zero")
            .backend_name
            .as_deref(),
        Some("fast")
    );
    assert_eq!(result.remaining.len(), 1);
    assert_eq!(result.remaining[0].backend_name(), Some("slow"));
    assert!(!cancelled.load(Ordering::SeqCst));
    drop(result.remaining);
    assert!(
        cancelled.load(Ordering::SeqCst),
        "should drop RC transport work with the TS handle"
    );
}

#[tokio::test]
async fn dropping_pending_before_select_also_drops_the_rc_driver() {
    let cancelled = Arc::new(AtomicBool::new(false));
    let client = scenario_bridge(vec![Driver::Pending(Arc::clone(&cancelled))]);
    let pending = client
        .send_async(request("http://example.com/", "cancelled"))
        .await
        .expect("should start pending request");
    drop(pending);
    assert!(cancelled.load(Ordering::SeqCst));
}
