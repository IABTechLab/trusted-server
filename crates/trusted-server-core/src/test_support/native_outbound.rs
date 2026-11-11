//! Real native provider and disposable loopback origins for crate tests.

use std::io::Write as _;
use std::time::Duration;

use edgezero_adapter_axum::outbound::AxumOutboundClient;
use edgezero_core::body::Body;
use edgezero_core::outbound::HttpClient;
use flate2::{Compression, write::GzEncoder};
use http::Request;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::platform::{AuctionTargetId, EdgeZeroHttpClient, PlatformHttpRequest};

#[derive(Clone, Copy)]
pub(crate) enum Reply {
    Complete,
    Html,
    Gzip,
    TrickleBody,
    Redirect,
    HeldHeaders,
    HeldBody,
    Failed,
}

pub(crate) struct Origin {
    pub(crate) url: String,
    seen: oneshot::Receiver<String>,
    closed: oneshot::Receiver<()>,
    task: JoinHandle<()>,
}

impl Origin {
    pub(crate) fn is_closed(&mut self) -> bool {
        match self.closed.try_recv() {
            Ok(()) => true,
            Err(oneshot::error::TryRecvError::Empty) => false,
            Err(oneshot::error::TryRecvError::Closed) => {
                panic!("should retain the connection observer")
            }
        }
    }
}

impl Drop for Origin {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub(crate) async fn origin(reply: Reply) -> Origin {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("should bind a fixture origin");
    let address = listener
        .local_addr()
        .expect("should read the fixture address");
    let (seen, incoming) = oneshot::channel();
    let (closed, closure) = oneshot::channel();
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener
            .accept()
            .await
            .expect("should accept the fixture request");
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            let count = socket
                .read(&mut byte)
                .await
                .expect("should read a request byte");
            assert_ne!(count, 0, "should receive the complete request head");
            head.push(byte[0]);
        }
        seen.send(String::from_utf8(head).expect("should read the fixture head as UTF8"))
            .expect("should retain the request observer");
        match reply {
            Reply::Complete => {
                socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close, x-private-hop\r\nX-Private-Hop: remove\r\nSet-Cookie: a=one\r\nSet-Cookie: b=two\r\n\r\nhello").await.expect("should write the complete fixture response");
            }
            Reply::Html => {
                let body = "<html><head></head><body>fixture</body></html>";
                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.expect("should write the publisher origin");
            }
            Reply::Gzip => {
                let body = gzip_hello();
                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).as_bytes()).await.expect("should write the coded response head");
                socket
                    .write_all(&body)
                    .await
                    .expect("should write the original coded bytes");
            }
            Reply::Redirect => {
                socket.write_all(b"HTTP/1.1 302 Found\r\nContent-Length: 0\r\nConnection: close\r\nLocation: /followed\r\n\r\n").await.expect("should write a redirect without following it");
            }
            Reply::TrickleBody => {
                socket
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100000\r\n\r\n")
                    .await
                    .expect("should write a trickling response head");
                loop {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    if socket.write_all(b"a").await.is_err() {
                        break;
                    }
                }
            }
            Reply::HeldBody => {
                socket
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\na")
                    .await
                    .expect("should write an incomplete response body");
                let mut byte = [0];
                while socket.read(&mut byte).await.unwrap_or_default() != 0 {}
            }
            Reply::HeldHeaders => {
                let mut byte = [0];
                while socket.read(&mut byte).await.unwrap_or_default() != 0 {}
            }
            Reply::Failed => {}
        }
        drop(socket);
        let _ = closed.send(());
    });
    Origin {
        url: format!("http://{address}/"),
        seen: incoming,
        closed: closure,
        task,
    }
}

pub(crate) fn clients() -> (HttpClient, EdgeZeroHttpClient) {
    let provider = AxumOutboundClient::try_new().expect("should build the real native provider");
    let sdk = HttpClient::with_client(provider);
    let client = EdgeZeroHttpClient::new(sdk.clone(), AuctionTargetId::Axum);
    (sdk, client)
}

pub(crate) fn client() -> EdgeZeroHttpClient {
    clients().1
}

pub(crate) fn gzip_hello() -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder
        .write_all(b"hello")
        .expect("should encode the fixture body");
    encoder.finish().expect("should finish the fixture coding")
}

pub(crate) fn request(url: &str, backend: &str) -> PlatformHttpRequest {
    PlatformHttpRequest::new(
        Request::builder()
            .uri(url)
            .body(Body::empty())
            .expect("should build the fixture request"),
        backend,
    )
    .with_cache_bypass()
}

pub(crate) async fn observed(origin: &mut Origin) -> String {
    tokio::time::timeout(Duration::from_secs(3), &mut origin.seen)
        .await
        .expect("should reach the real origin")
        .expect("should observe the request")
}

pub(crate) async fn closed(origin: &mut Origin) {
    tokio::time::timeout(Duration::from_secs(3), &mut origin.closed)
        .await
        .expect("should close canceled native work")
        .expect("should observe connection closure");
}
