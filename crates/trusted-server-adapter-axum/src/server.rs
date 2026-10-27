//! The dev server's HTTP front end, which delivers streamed responses as
//! they are produced.
//!
//! `EdgeZero`'s own Axum service collects every `Body::Stream` response into
//! one buffer before Hyper sees it, so a large origin response streamed
//! through the platform client would be held whole in memory on its way to
//! the client. This front end dispatches through the same `EdgeZero` router
//! and request conversion, and hands a streamed body to Hyper chunk by
//! chunk instead.
//!
//! The router's futures and a streamed body are `!Send`, while Hyper needs a
//! `Send` body. Each request therefore runs on a Tokio blocking-pool thread,
//! which drives the router through the runtime handle, as `EdgeZero`'s
//! service does with `block_in_place`. That thread then passes the body's
//! chunks to Hyper through a bounded channel, so a slow client holds back
//! the origin rather than letting chunks pile up. A failed chunk is passed on
//! as a body error, which makes Hyper abort the response instead of ending
//! it as though it were complete. A client that leaves closes the channel,
//! which stops the router or the body read at once and drops the origin
//! request.
//!
//! At most [`MAX_CONCURRENT_REQUESTS`] requests hold a blocking thread at a
//! time. Later requests wait for a permit in their own connection task, so
//! no work is queued behind the blocking pool without a bound.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body as AxumBody;
use axum::extract::Request as AxumRequest;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::Response as AxumResponse;
use bytes::Bytes;
use edgezero_adapter_axum::dev_server::AxumDevServerConfig;
use edgezero_adapter_axum::request::into_core_request;
use edgezero_core::body::Body as EdgeBody;
use edgezero_core::key_value_store::KvHandle;
use edgezero_core::router::RouterService;
use edgezero_core::store_registry::KvRegistry;
use futures::StreamExt as _;
use tokio::sync::{Semaphore, mpsc, oneshot};

/// Chunks a streamed body may hold between the router and Hyper before the
/// producer waits for the client.
const STREAMED_CHUNKS_IN_FLIGHT: usize = 8;

/// Requests that may hold a blocking-pool thread at once.
///
/// Half of Tokio's default 512 blocking threads, so the KV store's own
/// blocking operations, which a request waits on, always find a thread.
const MAX_CONCURRENT_REQUESTS: usize = 256;

/// The store id `EdgeZero` binds a single KV handle under.
const DEFAULT_KV_STORE_ID: &str = "default";

/// Run the dev server on its own multi-threaded Tokio runtime.
///
/// `kv_handle` is bound as the `default` KV store of every request, as
/// `EdgeZero`'s dev server binds it.
///
/// # Errors
///
/// Returns an error when the runtime cannot start, the address cannot be
/// bound, or the server's accept loop fails.
pub fn run(
    router: RouterService,
    config: &AxumDevServerConfig,
    kv_handle: Option<KvHandle>,
) -> std::io::Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let addr = config.addr;
    let enable_ctrl_c = config.enable_ctrl_c;
    runtime.block_on(async move {
        let listener = tokio::net::TcpListener::bind(addr).await.map_err(|error| {
            std::io::Error::new(
                error.kind(),
                format!("failed to bind dev server to {addr}: {error}"),
            )
        })?;
        let kv_registry =
            kv_handle.map(|handle| KvRegistry::single_id(DEFAULT_KV_STORE_ID.to_owned(), handle));
        if enable_ctrl_c {
            serve(listener, router, kv_registry, async {
                if let Err(error) = tokio::signal::ctrl_c().await {
                    log::error!("failed to listen for ctrl-c: {error}");
                    std::future::pending::<()>().await;
                }
            })
            .await
        } else {
            serve(listener, router, kv_registry, std::future::pending()).await
        }
    })
}

/// Serve requests from `listener` until `shutdown` completes.
async fn serve<F>(
    listener: tokio::net::TcpListener,
    router: RouterService,
    kv_registry: Option<KvRegistry>,
    shutdown: F,
) -> std::io::Result<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    let permits = Arc::new(Semaphore::new(MAX_CONCURRENT_REQUESTS));
    let app = axum::Router::new().fallback(move |request: AxumRequest| {
        dispatch(
            router.clone(),
            kv_registry.clone(),
            Arc::clone(&permits),
            request,
        )
    });
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown)
    .await
}

/// Answer one request, returning as soon as the response head is ready.
async fn dispatch(
    router: RouterService,
    kv_registry: Option<KvRegistry>,
    permits: Arc<Semaphore>,
    request: AxumRequest,
) -> AxumResponse {
    let Ok(permit) = permits.acquire_owned().await else {
        return plain_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "server is shutting down".to_owned(),
        );
    };
    let (head_sender, head) = oneshot::channel();
    let runtime = tokio::runtime::Handle::current();
    let worker = tokio::task::spawn_blocking(move || {
        runtime.block_on(produce(router, kv_registry, request, head_sender));
        drop(permit);
    });
    match head.await {
        Ok(response) => response,
        Err(_) => {
            let reason = match worker.await {
                Err(error) => format!("request worker failed: {error}"),
                Ok(()) => "request worker ended without a response".to_owned(),
            };
            log::error!("{reason}");
            plain_response(StatusCode::INTERNAL_SERVER_ERROR, reason)
        }
    }
}

/// Run the router for `request`, send the response head, then feed a
/// streamed body to Hyper until it ends, fails, or the client leaves.
///
/// The error answers match `EdgeZero`'s Axum service: a request that cannot
/// be converted answers 500 with the conversion error, and a router error
/// answers 500 with `internal error: <error>`.
async fn produce(
    router: RouterService,
    kv_registry: Option<KvRegistry>,
    request: AxumRequest,
    mut head: oneshot::Sender<AxumResponse>,
) {
    let mut core_request = match into_core_request(request).await {
        Ok(core_request) => core_request,
        Err(error) => {
            drop(head.send(plain_response(StatusCode::INTERNAL_SERVER_ERROR, error)));
            return;
        }
    };
    if let Some(registry) = kv_registry {
        core_request.extensions_mut().insert(registry);
    }
    let routed = tokio::select! {
        routed = router.oneshot(core_request) => routed,
        // Hyper dropped the request future: the client left before the head.
        () = head.closed() => return,
    };
    let core_response = match routed {
        Ok(core_response) => core_response,
        Err(error) => {
            drop(head.send(plain_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("internal error: {error}"),
            )));
            return;
        }
    };

    let (parts, body) = core_response.into_parts();
    let mut stream = match body {
        EdgeBody::Once(bytes) => {
            drop(head.send(AxumResponse::from_parts(parts, AxumBody::from(bytes))));
            return;
        }
        EdgeBody::Stream(stream) => stream,
    };

    let (chunk_sender, chunk_receiver) =
        mpsc::channel::<Result<Bytes, std::io::Error>>(STREAMED_CHUNKS_IN_FLIGHT);
    let body = AxumBody::from_stream(futures::stream::unfold(
        chunk_receiver,
        |mut receiver| async move { receiver.recv().await.map(|chunk| (chunk, receiver)) },
    ));
    if head.send(AxumResponse::from_parts(parts, body)).is_err() {
        return;
    }
    loop {
        let next = tokio::select! {
            next = stream.next() => next,
            // Hyper dropped the body: the client left mid-stream.
            () = chunk_sender.closed() => return,
        };
        let Some(chunk) = next else {
            return;
        };
        let failed = chunk.is_err();
        let chunk = chunk.map_err(|error| {
            log::warn!("streamed response failed mid-body: {error:#}");
            std::io::Error::other(format!("{error:#}"))
        });
        // A send waits while the channel is full, so a slow client slows
        // the origin read. It fails once Hyper has dropped the body.
        if chunk_sender.send(chunk).await.is_err() || failed {
            return;
        }
    }
}

fn plain_response(status: StatusCode, message: String) -> AxumResponse {
    let mut response = AxumResponse::new(AxumBody::from(message));
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use edgezero_core::context::RequestContext;
    use edgezero_core::error::EdgeError;

    /// Start the front end on a free local port and return its base URL.
    async fn start(router: RouterService) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("should bind a local port");
        let addr = listener
            .local_addr()
            .expect("should read the bound address");
        tokio::spawn(serve(listener, router, None, std::future::pending()));
        format!("http://{addr}")
    }

    /// A body that yields `first`, waits for `release`, then yields `second`.
    fn gated_body(
        first: &'static [u8],
        release: Arc<tokio::sync::Notify>,
        second: Result<&'static [u8], &'static str>,
    ) -> EdgeBody {
        let stream = futures::stream::unfold(0u8, move |step| {
            let release = Arc::clone(&release);
            async move {
                match step {
                    0 => Some((Ok(Bytes::from_static(first)), 1)),
                    1 => {
                        release.notified().await;
                        let item = second
                            .map(Bytes::from_static)
                            .map_err(std::io::Error::other);
                        Some((item, 2))
                    }
                    _ => None,
                }
            }
        });
        EdgeBody::from_stream(stream)
    }

    /// Records that the body it travels with was dropped.
    struct DropFlag(Arc<std::sync::atomic::AtomicBool>);

    impl Drop for DropFlag {
        fn drop(&mut self) {
            self.0.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_client_that_leaves_stops_the_body_read() {
        let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let handler_dropped = Arc::clone(&dropped);
        let router = RouterService::builder()
            .get("/media", move |_ctx: RequestContext| {
                let flag = DropFlag(Arc::clone(&handler_dropped));
                async move {
                    // An origin body that never ends, 64 KiB at a time.
                    let chunk = Bytes::from(vec![1u8; 64 * 1024]);
                    let stream = futures::stream::unfold(flag, move |flag| {
                        let chunk = chunk.clone();
                        async move {
                            tokio::task::yield_now().await;
                            Some((Ok::<_, std::io::Error>(chunk), flag))
                        }
                    });
                    Ok::<_, EdgeError>(EdgeBody::from_stream(stream))
                }
            })
            .build();
        let base = start(router).await;

        let mut response = reqwest::get(format!("{base}/media"))
            .await
            .expect("the head should arrive");
        response
            .chunk()
            .await
            .expect("the first chunk should be readable")
            .expect("the first chunk should arrive");
        drop(response);

        tokio::time::timeout(Duration::from_secs(5), async {
            while !dropped.load(std::sync::atomic::Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the body, and the origin read it holds, should be dropped once the client leaves");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_streamed_body_reaches_the_client_before_it_finishes() {
        let release = Arc::new(tokio::sync::Notify::new());
        let handler_release = Arc::clone(&release);
        let router = RouterService::builder()
            .get("/media", move |_ctx: RequestContext| {
                let release = Arc::clone(&handler_release);
                async move {
                    let response = edgezero_core::http::response_builder()
                        .status(200)
                        .header("x-origin", "kept")
                        .header(header::CONTENT_TYPE, "video/mp4")
                        .body(gated_body(b"first-", release, Ok(b"second")))
                        .expect("should build the streamed response");
                    Ok::<_, EdgeError>(response)
                }
            })
            .build();
        let base = start(router).await;

        let mut response = tokio::time::timeout(
            Duration::from_secs(5),
            reqwest::get(format!("{base}/media")),
        )
        .await
        .expect("the head should arrive while the body is still open")
        .expect("the request should succeed");
        assert_eq!(response.status().as_u16(), 200);
        assert_eq!(
            response
                .headers()
                .get("x-origin")
                .and_then(|value| value.to_str().ok()),
            Some("kept"),
            "should deliver the router's headers"
        );
        let first = tokio::time::timeout(Duration::from_secs(5), response.chunk())
            .await
            .expect("the first chunk should arrive before the body finishes")
            .expect("the first chunk should be readable")
            .expect("the body should not end after the first chunk");
        assert_eq!(first.as_ref(), b"first-");

        release.notify_one();
        let mut rest = Vec::new();
        while let Some(chunk) = tokio::time::timeout(Duration::from_secs(5), response.chunk())
            .await
            .expect("the rest of the body should arrive once released")
            .expect("the rest of the body should be readable")
        {
            rest.extend_from_slice(&chunk);
        }
        assert_eq!(rest, b"second", "should deliver the released chunk and end");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_body_that_fails_mid_stream_is_not_delivered_as_complete() {
        let release = Arc::new(tokio::sync::Notify::new());
        let handler_release = Arc::clone(&release);
        let router = RouterService::builder()
            .get("/media", move |_ctx: RequestContext| {
                let release = Arc::clone(&handler_release);
                async move {
                    Ok::<_, EdgeError>(gated_body(
                        b"partial",
                        release,
                        Err("origin connection reset"),
                    ))
                }
            })
            .build();
        let base = start(router).await;

        let mut response = reqwest::get(format!("{base}/media"))
            .await
            .expect("the head should arrive");
        let first = response
            .chunk()
            .await
            .expect("the first chunk should be readable")
            .expect("the first chunk should arrive");
        assert_eq!(first.as_ref(), b"partial");

        release.notify_one();
        let outcome = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match response.chunk().await {
                    Ok(Some(_)) => {}
                    Ok(None) => return Ok(()),
                    Err(error) => return Err(error),
                }
            }
        })
        .await
        .expect("the failure should surface promptly");
        assert!(
            outcome.is_err(),
            "a failed stream must abort the response rather than end it cleanly"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_buffered_body_and_an_unknown_route_answer_as_before() {
        async fn hello(_ctx: RequestContext) -> Result<&'static str, EdgeError> {
            Ok("hello")
        }
        let router = RouterService::builder().get("/hello", hello).build();
        let base = start(router).await;

        let hello = reqwest::get(format!("{base}/hello"))
            .await
            .expect("the buffered route should answer");
        assert_eq!(hello.status().as_u16(), 200);
        assert_eq!(
            hello.text().await.expect("the body should be readable"),
            "hello"
        );

        let missing = reqwest::get(format!("{base}/missing"))
            .await
            .expect("an unknown route should answer");
        assert_eq!(
            missing.status().as_u16(),
            404,
            "the router's own not-found answer should pass through"
        );
    }
}
