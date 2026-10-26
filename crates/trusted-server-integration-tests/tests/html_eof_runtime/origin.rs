use std::fs;
use std::io;
use std::net::{Ipv4Addr, TcpListener};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{DefaultBodyLimit, OriginalUri, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Json, Router};
use bytes::Bytes;
use error_stack::{Report, ResultExt as _};
use futures::stream;
use serde_json::{Value, json};
use tokio::runtime::Runtime;
use tokio::task::JoinHandle;

use super::{HarnessError, HarnessResult};

const PREFIX: &[u8] =
    b"<!doctype html><html><head></head><body><p>EXAMPLE_UNSIGNED</p></body></html>";
const EOF: &[u8] = b"<!--example-document-eof-->";

#[derive(Clone)]
pub(super) struct Event {
    pub kind: &'static str,
    pub at: Instant,
    pub data: Value,
}

#[derive(Clone, Default)]
struct Events(Arc<Mutex<Vec<Event>>>);

impl Events {
    fn record(&self, kind: &'static str, data: Value) {
        self.0
            .lock()
            .expect("should lock fixture events")
            .push(Event {
                kind,
                at: Instant::now(),
                data,
            });
    }
}

pub(super) struct Origin {
    pub port: u16,
    pub started: Instant,
    pub runtime: Runtime,
    events: Events,
    server: JoinHandle<()>,
}

impl Origin {
    pub(super) fn start() -> HarnessResult<Self> {
        let started = Instant::now();
        let listener =
            TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).change_context(HarnessError::Io)?;
        let port = listener
            .local_addr()
            .change_context(HarnessError::Io)?
            .port();
        listener
            .set_nonblocking(true)
            .change_context(HarnessError::Io)?;
        let runtime = Runtime::new().change_context(HarnessError::Runtime)?;
        let _entered = runtime.enter();
        let listener =
            tokio::net::TcpListener::from_std(listener).change_context(HarnessError::Io)?;
        let events = Events::default();
        let app = Router::new()
            .route("/health", axum::routing::head(|| async { StatusCode::OK }))
            .route("/article", get(article))
            .route("/success", post(lookup))
            .route("/hung", post(lookup))
            .layer(DefaultBodyLimit::max(2048))
            .with_state(events.clone());
        let server = runtime.spawn(async move {
            if let Err(error) = axum::serve(listener, app).await {
                log::error!("EOF origin fails: {error}");
            }
        });
        drop(_entered);
        Ok(Self {
            port,
            started,
            runtime,
            events,
            server,
        })
    }

    pub(super) fn events(&self) -> Vec<Event> {
        self.events
            .0
            .lock()
            .expect("should lock fixture events")
            .clone()
    }

    pub(super) fn write_events(&self, path: &Path) -> HarnessResult<()> {
        let events = self.events().iter().map(|event| json!({
            "event": event.kind,
            "seconds_from_origin_start": event.at.duration_since(self.started).as_secs_f64(),
            "data": event.data,
        })).collect::<Vec<_>>();
        fs::write(path, json!(events).to_string()).change_context(HarnessError::Io)
    }
}

impl Drop for Origin {
    fn drop(&mut self) {
        // Runtime teardown cancels pending lookup handlers, including the hung control.
        self.server.abort();
    }
}

async fn article(State(events): State<Events>) -> Response {
    let body = stream::unfold(0, move |index| {
        let events = events.clone();
        async move {
            let bytes = match index {
                0 => Bytes::from_static(PREFIX),
                1 => {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    events.record("origin_eof", json!({}));
                    Bytes::from_static(EOF)
                }
                _ => return None,
            };
            Some((Ok::<_, io::Error>(bytes), index + 1))
        }
    });
    Response::builder()
        .header("content-type", "text/html")
        .header("cache-control", "public, max-age=60")
        .header("content-length", PREFIX.len() + EOF.len())
        .body(Body::from_stream(body))
        .expect("should build the controlled origin response")
}

async fn lookup(
    State(events): State<Events>,
    OriginalUri(uri): OriginalUri,
    Json(mut payload): Json<Value>,
) -> &'static str {
    payload["path"] = Value::String(uri.path().to_owned());
    events.record("lookup_start", payload);
    let delay = if uri.path() == "/hung" {
        Duration::from_secs(30)
    } else {
        Duration::from_millis(150)
    };
    tokio::time::sleep(delay).await;
    events.record("lookup_complete", json!({"path": uri.path()}));
    "EXAMPLE_APPROVED"
}

pub(super) fn lookup_for_request(
    events: &[Event],
    started: Instant,
    completed: Instant,
    enabled: bool,
) -> HarnessResult<Option<&Event>> {
    let lookups: Vec<_> = events
        .iter()
        .filter(|event| {
            event.kind == "lookup_start" && started <= event.at && event.at <= completed
        })
        .collect();
    if lookups.len() != usize::from(enabled) {
        return Err(Report::new(HarnessError::Runtime)
            .attach("each request must issue exactly one enabled EOF lookup"));
    }
    let Some(lookup) = lookups.first().copied() else {
        return Ok(None);
    };
    if !events
        .iter()
        .any(|event| event.kind == "origin_eof" && started <= event.at && event.at <= lookup.at)
    {
        return Err(Report::new(HarnessError::Runtime)
            .attach("EOF lookup starts before its own origin EOF"));
    }
    Ok(Some(lookup))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn earlier_request_eof_cannot_satisfy_lookup_ordering() {
        let start = Instant::now();
        let event = |kind, millis| Event {
            kind,
            at: start + Duration::from_millis(millis),
            data: json!({}),
        };
        let events = [
            event("origin_eof", 1),
            event("lookup_start", 20),
            event("origin_eof", 25),
        ];
        assert!(
            lookup_for_request(
                &events,
                start + Duration::from_millis(10),
                start + Duration::from_millis(30),
                true
            )
            .is_err(),
            "should reject an EOF from an earlier request"
        );
    }

    #[test]
    fn request_scoped_lookup_requires_exactly_one_enabled_call() {
        let start = Instant::now();
        let events = [
            Event {
                kind: "origin_eof",
                at: start,
                data: json!({}),
            },
            Event {
                kind: "lookup_start",
                at: start,
                data: json!({}),
            },
        ];
        assert!(
            lookup_for_request(&events, start, start, true)
                .expect("should match the current EOF")
                .is_some(),
            "should return the current lookup"
        );
        assert!(
            lookup_for_request(&events, start, start, false).is_err(),
            "should reject lookup with disabled capability"
        );
        assert!(
            lookup_for_request(&[], start, start, true).is_err(),
            "should reject missing lookup"
        );
        assert!(
            lookup_for_request(&[], start, start, false)
                .expect("should accept disabled work")
                .is_none(),
            "should accept no disabled lookup"
        );
    }
}
