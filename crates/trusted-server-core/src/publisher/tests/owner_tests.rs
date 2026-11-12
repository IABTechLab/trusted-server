//! Publisher ownership through real native origin/probe exchanges and managed egress.
//! This does not qualify a live server's downstream socket writer.

use std::cell::RefCell;
use std::sync::atomic::{AtomicUsize, Ordering};

use edgezero_core::app::App;
use edgezero_core::context::RequestContext;
use edgezero_core::error::EdgeError;
use edgezero_core::ingress::{IngressDispatchOutcome, IngressFraming, IngressHeadAccounting};
use edgezero_core::outbound::{
    OutboundCachePolicy, OutboundRequest, OutboundScope, ResponseDecoding,
};
use edgezero_core::response_egress::{ResponseEgressObserver, ResponseEgressReport};
use edgezero_core::response_egress_framing::ResponseEgressFramingMode;
use edgezero_core::router::RouterService;
use tokio::task::LocalSet;

use crate::platform::{PlatformHttpClient as _, PlatformPendingRequest};
use crate::test_support::native_outbound::{Reply, clients, closed, observed, origin, request};

use super::*;

struct Retained {
    scope: OutboundScope,
    _pending: PlatformPendingRequest,
}

thread_local! {
    static RETAINED: RefCell<Option<Retained>> = const { RefCell::new(None) };
}

struct RetainedGuard;

impl Drop for RetainedGuard {
    fn drop(&mut self) {
        RETAINED.with(|retained| retained.borrow_mut().take());
    }
}

fn assert_scope_open(open: bool) {
    RETAINED.with(|retained| {
        assert_eq!(
            retained
                .borrow()
                .as_ref()
                .expect("should retain the original owner scope")
                .scope
                .check()
                .is_ok(),
            open,
            "should retain the exact producer scope until managed delivery terminates"
        );
    });
}

#[derive(Clone, Copy)]
enum Shape {
    Buffered,
    PassThrough,
    Processed,
    Template,
}

#[derive(Clone, Copy)]
enum Terminal {
    Complete,
    DropAttempt,
    DropEnvelope,
}

struct Observer(Arc<AtomicUsize>);

impl ResponseEgressObserver for Observer {
    fn complete(&self, _report: &ResponseEgressReport) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

async fn exercise(shape: Shape, buffered: bool, terminal: Terminal) {
    let _retained = RetainedGuard;
    let mut source = origin(Reply::Html).await;
    let mut probe = origin(Reply::HeldHeaders).await;
    let source_url = source.url.clone();
    let probe_url = probe.url.clone();
    let (sdk, client) = clients();
    let client = Arc::new(client);
    let settings = Arc::new(allow_eof_test_settings(create_test_settings()));
    let registry = Arc::new(
        IntegrationRegistry::with_plan(
            &settings,
            Arc::new(
                crate::auction::compile_auction_plan(&settings)
                    .expect("should compile the fixture plan"),
            ),
        )
        .expect("should build fixture integrations"),
    );
    let orchestrator = Arc::new(AuctionOrchestrator::new(settings.auction.clone()));
    let router = RouterService::builder()
        .get("/", move |_context: RequestContext| {
            let client = Arc::clone(&client);
            let sdk = sdk.clone();
            let settings = Arc::clone(&settings);
            let registry = Arc::clone(&registry);
            let orchestrator = Arc::clone(&orchestrator);
            let source_url = source_url.clone();
            let probe_url = probe_url.clone();
            async move {
                let owner = OutboundScopeOwner::response_body();
                let (pending, origin) = owner
                    .run(async {
                        let pending = client
                            .send_async(request(&probe_url, "retained-probe"))
                            .await
                            .map_err(|_| EdgeError::bad_gateway("fixture probe dispatch fails"))?;
                        let origin = sdk
                            .send(
                                OutboundRequest::get(&source_url)?
                                    .cache_policy(OutboundCachePolicy::Bypass)
                                    .response_decoding(ResponseDecoding::Original)
                                    .stream_response(),
                            )
                            .await
                            .map_err(|_| EdgeError::bad_gateway("fixture origin dispatch fails"))?;
                        Ok::<_, EdgeError>((pending, origin.into_response()?))
                    })
                    .await?;
                RETAINED.with(|retained| {
                    assert!(
                        retained
                            .borrow_mut()
                            .replace(Retained {
                                scope: owner.scope(),
                                _pending: pending
                            })
                            .is_none(),
                        "should retain exactly one probe handle"
                    );
                });
                let (mut parts, body) = origin.into_parts();
                if matches!(shape, Shape::Processed | Shape::Template) {
                    parts.headers.remove(header::CONTENT_LENGTH);
                }
                let publisher = match shape {
                    Shape::Buffered => {
                        PublisherResponse::Buffered(Response::from_parts(parts, body))
                    }
                    Shape::PassThrough => PublisherResponse::PassThrough {
                        response: Response::from_parts(parts, EdgeBody::empty()),
                        body,
                    },
                    Shape::Processed => PublisherResponse::Stream {
                        response: Response::from_parts(parts, EdgeBody::empty()),
                        body,
                        params: Box::new(html_stream_params("", None)),
                    },
                    Shape::Template => {
                        body.into_bytes_bounded(1024).await?;
                        PublisherResponse::AssembleTemplate {
                            response: Response::from_parts(parts, EdgeBody::empty()),
                            template: format!(
                                "<html><body>fixture{AD_ASSEMBLY_SEAM}</body></html>"
                            )
                            .into_bytes(),
                            params: Box::new(html_stream_params("", None)),
                        }
                    }
                }
                .with_outbound_owner(owner)
                .map_err(|_| EdgeError::bad_gateway("fixture owner attachment fails"))?;
                let services = noop_services();
                if buffered {
                    buffer_publisher_response_async(
                        publisher,
                        &Method::GET,
                        &settings,
                        &registry,
                        &orchestrator,
                        &services,
                    )
                    .await
                } else {
                    publisher_response_into_streaming_response(
                        publisher,
                        &Method::GET,
                        settings,
                        &registry,
                        orchestrator,
                        services,
                    )
                    .await
                }
                .map_err(|_| EdgeError::bad_gateway("fixture finalization fails"))
            }
        })
        .build();
    let count = Arc::new(AtomicUsize::new(0));
    let mut app = App::new(router);
    app.set_response_egress_observer(Observer(Arc::clone(&count)));
    let callback = OutboundScopeOwner::callback();
    let incoming = HttpRequest::builder()
        .uri("http://publisher.example.com/")
        .body(EdgeBody::empty())
        .expect("should build fixture ingress");
    let outcome = callback
        .run(app.dispatch_ingress(
            incoming,
            Instant::now(),
            IngressHeadAccounting::HostManaged,
            IngressFraming::NoBody,
        ))
        .await
        .expect("should dispatch managed ingress");
    let IngressDispatchOutcome::Response(envelope) = outcome else {
        panic!("should prepare managed egress");
    };
    observed(&mut source).await;
    observed(&mut probe).await;
    assert_scope_open(true);
    assert!(
        !probe.is_closed(),
        "should retain producer work after publisher handler return"
    );
    if matches!(terminal, Terminal::DropEnvelope) {
        drop(envelope);
        assert_scope_open(false);
        closed(&mut probe).await;
        assert_eq!(
            count.load(Ordering::SeqCst),
            0,
            "should not invent a terminal attempt before egress begins"
        );
        drop(callback);
        return;
    }
    let (prepared, _, mut attempt, clock) = envelope
        .begin(ResponseEgressFramingMode::HostAutomatic)
        .map_err(|_| ())
        .expect("should accept the original body owner");
    drop(callback);
    assert_scope_open(true);
    assert!(
        attempt.begin_writing(),
        "should begin exactly one egress attempt"
    );
    let body = prepared
        .into_response()
        .into_body()
        .into_bytes_bounded(1024 * 1024)
        .await
        .expect("should consume the entire publisher output");
    assert!(!body.is_empty(), "should emit publisher bytes");
    assert_scope_open(true);
    assert!(
        !probe.is_closed(),
        "should not cancel unrelated response-owned work at origin or output EOF"
    );
    assert_eq!(
        count.load(Ordering::SeqCst),
        0,
        "should not report completion at body EOF"
    );
    match terminal {
        Terminal::Complete => {
            assert!(attempt.complete(clock.now()), "should complete egress once");
            assert!(
                !attempt.complete(clock.now()),
                "should reject a second terminal transition"
            );
        }
        Terminal::DropAttempt => drop(attempt),
        Terminal::DropEnvelope => unreachable!("should handle envelope drop before begin"),
    }
    assert_scope_open(false);
    closed(&mut probe).await;
    assert_eq!(
        count.load(Ordering::SeqCst),
        1,
        "should notify exactly once after delivery termination"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn publisher_finalizers_preserve_the_original_owner_until_managed_completion() {
    LocalSet::new()
        .run_until(async {
            for shape in [
                Shape::Buffered,
                Shape::PassThrough,
                Shape::Processed,
                Shape::Template,
            ] {
                for buffered in [false, true] {
                    exercise(shape, buffered, Terminal::Complete).await;
                }
            }
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn dropping_egress_attempt_after_eof_cancels_the_original_owner_once() {
    LocalSet::new()
        .run_until(async {
            for buffered in [false, true] {
                exercise(Shape::Processed, buffered, Terminal::DropAttempt).await;
            }
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn dropping_an_unbegun_envelope_cancels_original_work_without_parent_revocation() {
    LocalSet::new()
        .run_until(async {
            for buffered in [false, true] {
                exercise(Shape::Processed, buffered, Terminal::DropEnvelope).await;
            }
        })
        .await;
}
