//! Existing native HTTP regressions through the shared request-scoped SDK client.
//! The retired batch-driver protocol remains an SDK responsibility.

use std::time::Duration;

use edgezero_adapter_axum::outbound::AxumOutboundClient;
use edgezero_core::body::Body;
use edgezero_core::http::{Method, StatusCode, header, request_builder};
use edgezero_core::outbound::{HttpClient, ResponseDecoding};
use futures::{StreamExt as _, stream};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::task::LocalSet;
use trusted_server_core::platform::{
    AuctionTargetId, EdgeZeroHttpClient, PlatformHttpClient as _, PlatformHttpRequest,
};

fn bridge() -> EdgeZeroHttpClient {
    let client = AxumOutboundClient::try_new().expect("should construct native RC client");
    EdgeZeroHttpClient::new(HttpClient::with_client(client), AuctionTargetId::Axum)
        .response_decoding(ResponseDecoding::Decode)
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
    body.into_bytes_bounded(16 * 1024 * 1024)
        .await
        .expect("should collect the complete fixture response with its owner")
        .to_vec()
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

#[tokio::test(flavor = "current_thread")]
async fn redirects_are_exposed_for_core_per_hop_authorization() {
    LocalSet::new().run_until(async {

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

    }).await;
}

#[tokio::test(flavor = "current_thread")]
async fn duplicate_cookies_survive_and_hop_headers_do_not() {
    LocalSet::new().run_until(async {

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

    }).await;
}

#[tokio::test(flavor = "current_thread")]
async fn rc_owns_gzip_decoding_and_ts_receives_matching_identity_metadata() {
    LocalSet::new().run_until(async {

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

    }).await;
}

#[tokio::test(flavor = "current_thread")]
async fn dropping_streamed_response_releases_the_origin_connection() {
    LocalSet::new()
        .run_until(async {
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
                .send(
                    request(&format!("http://{address}/"), "dropped-stream").with_stream_response(),
                )
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
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn caller_response_bound_applies_to_send_and_pending() {
    LocalSet::new()
        .run_until(async {
            for pending in [false, true] {
                let url = raw_server(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nbody").await;
                let mut outbound = request(&url, "bounded");
                outbound.max_response_body_bytes = Some(3);
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
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn streamed_upload_is_bounded_before_network_work() {
    LocalSet::new()
        .run_until(async {
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
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn pending_post_preserves_streamed_upload_and_duplicate_request_headers() {
    LocalSet::new()
        .run_until(async {
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
                    if let Some(head_end) =
                        request.windows(4).position(|window| window == b"\r\n\r\n")
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
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn expired_caller_deadline_is_rejected_before_network_work() {
    LocalSet::new()
        .run_until(async {
            for pending in [false, true] {
                let transport =
                    AxumOutboundClient::try_new().expect("should construct native transport");
                let sdk = HttpClient::with_client(transport);
                let clock = sdk.time().clock().clone();
                let client = EdgeZeroHttpClient::new(sdk, AuctionTargetId::Axum);
                let mut outbound = request("http://example.com/", "expired");
                outbound.deadline = Some(clock.deadline_after(Duration::ZERO));
                let rejected = if pending {
                    client.send_async(outbound).await.is_err()
                } else {
                    client.send(outbound).await.is_err()
                };
                assert!(rejected, "should reject expired absolute caller deadline");
            }
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn bodyless_streaming_response_keeps_head_representation_length() {
    LocalSet::new()
        .run_until(async {
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
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn total_timeout_covers_waiting_for_response_headers() {
    LocalSet::new()
        .run_until(async {
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
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn streamed_body_retains_total_timeout_after_response_head() {
    LocalSet::new()
        .run_until(async {
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
        })
        .await;
}
