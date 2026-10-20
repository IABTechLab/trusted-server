use std::io;
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use axum::Router;
use axum::body::{Body, Bytes, to_bytes};
use axum::extract::{Request, State};
use axum::http::Response;
use tokio::sync::oneshot;
use trusted_server_core::test_support::nextjs_auction;

#[derive(Clone, Debug)]
pub struct Received {
    pub path: String,
    pub body: Vec<u8>,
    pub next_action: Option<String>,
}

#[derive(Clone)]
struct OriginState {
    base_url: String,
    label: String,
    received: Arc<Mutex<Vec<Received>>>,
}

pub struct Origin {
    pub base_url: String,
    pub received: Arc<Mutex<Vec<Received>>>,
    stop: Option<oneshot::Sender<()>>,
    worker: Option<JoinHandle<()>>,
}

impl Origin {
    pub fn start(label: &str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("should bind fixture origin");
        let base_url = format!(
            "http://{}",
            listener.local_addr().expect("should get origin address")
        );
        listener
            .set_nonblocking(true)
            .expect("should set nonblocking listener");
        let received = Arc::new(Mutex::new(Vec::new()));
        let state = OriginState {
            base_url: base_url.clone(),
            label: label.to_owned(),
            received: Arc::clone(&received),
        };
        let (stop, stopped) = oneshot::channel();
        let worker = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("should build fixture runtime");
            runtime.block_on(async move {
                let listener = tokio::net::TcpListener::from_std(listener)
                    .expect("should adopt fixture listener");
                let router = Router::new().fallback(origin).with_state(state);
                tokio::select! {
                    result = axum::serve(listener, router) => result.expect("should serve fixture"),
                    _ = stopped => {}
                }
            });
        });
        Self {
            base_url,
            received,
            stop: Some(stop),
            worker: Some(worker),
        }
    }
}

impl Drop for Origin {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(worker) = self.worker.take() {
            worker.join().expect("should stop fixture runtime");
        }
    }
}

async fn origin(State(state): State<OriginState>, request: Request) -> Response<Body> {
    let path = request.uri().path().to_owned();
    let next_action = request.headers().get("next-action").map(|value| {
        value
            .to_str()
            .expect("should read action header")
            .to_owned()
    });
    let body = to_bytes(request.into_body(), 65536).await;
    let Ok(body) = body else {
        return Response::builder()
            .status(400)
            .body(Body::empty())
            .expect("should reject bad upload");
    };
    state
        .received
        .lock()
        .expect("should lock origin observations")
        .push(Received {
            path: path.clone(),
            body: body.to_vec(),
            next_action: next_action.clone(),
        });
    match path.as_str() {
        // Force a response-framing failure after a fully received POST. The
        // origin stays alive on the same port for the following action/API flow.
        "/truncate-response" => Response::builder()
            .header("content-type", "application/json")
            .header("content-length", "1024")
            .body(Body::from_stream(futures::stream::iter([
                Ok(Bytes::from_static(b"{\"partial\":")),
                Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "synthetic truncated POST response",
                )),
            ])))
            .expect("should build truncated response"),
        "/api/data" => Response::builder()
            .header("content-type", "application/json")
            .body(Body::from(format!("{{\"native\":\"{}\"}}", state.label)))
            .expect("should build API response"),
        "/action" if next_action.is_some() => Response::builder()
            .header("content-type", "text/x-component")
            .body(Body::from("0:{\"accepted\":true}\n"))
            .expect("should build action response"),
        _ => {
            let html = nextjs_auction::origin_html()
                .replace("https://origin.test-publisher.example.com", &state.base_url)
                .replace(
                    "<p>prefix</p>",
                    &format!(
                        "<p>{}</p><a href=\"{}/asset\">asset</a>",
                        state.label, state.base_url,
                    ),
                );
            Response::builder()
                .header("content-type", "text/html")
                .body(Body::from(html))
                .expect("should build publisher page")
        }
    }
}
