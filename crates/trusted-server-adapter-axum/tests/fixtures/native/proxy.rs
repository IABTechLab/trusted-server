use std::net::TcpListener;
use std::thread::JoinHandle;
use std::time::Duration;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::{Request, State};
use axum::http::{Response, header};
use tokio::sync::oneshot;

#[derive(Clone)]
struct ProxyState {
    upstream: String,
    public_host: &'static str,
    client: &'static str,
}

/// A local gateway that replaces visitor assertions with its own fixed facts.
pub struct Proxy {
    pub base_url: String,
    stop: Option<oneshot::Sender<()>>,
    worker: Option<JoinHandle<()>>,
}

impl Proxy {
    pub fn start(upstream: &str, public_host: &'static str, client: &'static str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("should bind test gateway");
        let base_url = format!(
            "http://{}",
            listener.local_addr().expect("should get gateway address")
        );
        listener
            .set_nonblocking(true)
            .expect("should make gateway nonblocking");
        let state = ProxyState {
            upstream: upstream.to_owned(),
            public_host,
            client,
        };
        let (stop, stopped) = oneshot::channel();
        let worker = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("should build gateway runtime");
            runtime.block_on(async move {
                let listener = tokio::net::TcpListener::from_std(listener)
                    .expect("should adopt gateway listener");
                let router = Router::new().fallback(relay).with_state(state);
                tokio::select! {
                    result = axum::serve(listener, router) => result.expect("should serve gateway"),
                    _ = stopped => {}
                }
            });
        });
        Self {
            base_url,
            stop: Some(stop),
            worker: Some(worker),
        }
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(worker) = self.worker.take() {
            worker.join().expect("should stop gateway");
        }
    }
}

async fn relay(State(state): State<ProxyState>, request: Request) -> Response<Body> {
    let (parts, body) = request.into_parts();
    let body = to_bytes(body, 65536)
        .await
        .expect("should read bounded gateway request");
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(3))
        .build()
        .expect("should build gateway HTTP client");
    let mut headers = parts.headers;
    headers.remove("forwarded");
    let forwarding_names = headers
        .keys()
        .filter(|name| name.as_str().starts_with("x-forwarded-"))
        .cloned()
        .collect::<Vec<_>>();
    for name in forwarding_names {
        headers.remove(name);
    }
    headers.remove(header::HOST);
    headers.remove(header::CONNECTION);
    headers.insert(
        "forwarded",
        format!(
            "for={};host=\"{}\";proto=https",
            state.client, state.public_host,
        )
        .parse()
        .expect("should construct gateway assertion"),
    );
    let url = format!(
        "{}{}",
        state.upstream,
        parts
            .uri
            .path_and_query()
            .map_or("/", |value| value.as_str())
    );
    let response = client
        .request(parts.method, url)
        .headers(headers)
        .body(body)
        .send()
        .await
        .expect("should relay request through real TS listener");
    let status = response.status();
    let headers = response.headers().clone();
    let body = response
        .bytes()
        .await
        .expect("should read bounded test page");
    assert!(
        body.len() < 2_000_000,
        "should keep gateway fixture responses small"
    );
    let mut response = Response::builder()
        .status(status)
        .body(Body::from(body))
        .expect("should construct relayed page");
    *response.headers_mut() = headers;
    response
}
