//! Application hooks through the RC closed lifecycle with an injected writer.
//!
//! The writer is simulated so these tests can inspect terminal ownership. They
//! do not claim to exercise Fastly network disconnects.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use edgezero_adapter_fastly::request::FastlyService;
use edgezero_core::app::App;
use edgezero_core::body::Body;
use edgezero_core::context::RequestContext;
use edgezero_core::error::EdgeError;
use edgezero_core::http::{Response, header};
use edgezero_core::ingress::AdmissionDecision;
use edgezero_core::response_egress::{
    ResponseEgressFallbackDisposition, ResponseEgressObserver, ResponseEgressOutcome,
    ResponseEgressReport,
};
use edgezero_core::router::RouterService;
use futures::executor::block_on;

use super::{DeliveryObservation, apply_terminal_response_effects};

#[derive(Clone)]
struct TerminalObserver {
    delivered: DeliveryObservation,
    count: Arc<AtomicUsize>,
}

impl ResponseEgressObserver for TerminalObserver {
    fn complete(&self, report: &ResponseEgressReport) {
        self.count.fetch_add(1, Ordering::SeqCst);
        self.delivered.complete(report);
    }
}

#[derive(Clone, Copy)]
enum WriterResult {
    Application,
    Fallback,
    SourceFailure,
}

fn router(source_failure: bool) -> RouterService {
    RouterService::builder()
        .get("/", move |_ctx: RequestContext| async move {
            let body = if source_failure {
                Body::from_external_stream(futures::stream::iter([
                    Ok(bytes::Bytes::from_static(b"prefix")),
                    Err(std::io::Error::other("synthetic body source error")),
                ]))
            } else {
                Body::stream(futures::stream::iter([
                    bytes::Bytes::from_static(b"first"),
                    bytes::Bytes::from_static(b"second"),
                ]))
            };
            let mut response = Response::new(body);
            response.headers_mut().append(
                header::SET_COOKIE,
                "fixture_a=one".parse().expect("should parse cookie"),
            );
            response.headers_mut().append(
                header::SET_COOKIE,
                "fixture_b=two".parse().expect("should parse cookie"),
            );
            response.headers_mut().insert(
                header::CACHE_CONTROL,
                "public, max-age=60"
                    .parse()
                    .expect("should parse cache policy"),
            );
            Ok::<_, EdgeError>(response)
        })
        .build()
}

fn dispatch(case: WriterResult) -> (Result<Option<u8>, fastly::Error>, TerminalObserver) {
    let observer = TerminalObserver {
        delivered: DeliveryObservation::default(),
        count: Arc::new(AtomicUsize::new(0)),
    };
    let mut app = App::new(router(matches!(case, WriterResult::SourceFailure)));
    app.set_response_egress_observer(observer.clone());
    observer.delivered.reset();
    let before_delivery = observer.clone();
    let result = FastlyService::new(&app).run_request_with_hooks_for_test(
        fastly::Request::get("http://publisher.example.com/"),
        |raw, _extensions| {
            raw.remove_header("x-forwarded-host");
        },
        |response| {
            apply_terminal_response_effects(response, None);
            assert_eq!(
                response
                    .headers()
                    .get_all(header::SET_COOKIE)
                    .iter()
                    .count(),
                2
            );
            assert!(
                !response.headers()[header::CACHE_CONTROL]
                    .to_str()
                    .expect("should read policy")
                    .contains("public")
            );
            assert!(
                !before_delivery.delivered.completed(),
                "should not complete at handler return"
            );
            7
        },
        |envelope| {
            let (prepared, _, mut attempt, clock) = envelope
                .begin()
                .map_err(|_| fastly::Error::msg("synthetic egress begin failure"))?;
            if matches!(case, WriterResult::Fallback) {
                assert!(attempt.begin_fallback(ResponseEgressOutcome::ConversionError));
                assert!(attempt.begin_writing());
                assert!(
                    attempt
                        .finish_fallback(ResponseEgressFallbackDisposition::Completed, clock.now())
                );
            } else {
                assert!(attempt.begin_writing());
                let body = block_on(
                    prepared
                        .into_response()
                        .into_body()
                        .into_bytes_bounded(1024),
                );
                if matches!(case, WriterResult::SourceFailure) {
                    assert!(body.is_err(), "should observe the actual body source error");
                    assert!(attempt.terminate(ResponseEgressOutcome::SourceError, clock.now()));
                    assert!(
                        !attempt.complete(clock.now()),
                        "should reject a second terminal transition"
                    );
                    return Err(fastly::Error::msg("synthetic postcommit source failure"));
                }
                assert_eq!(
                    body.expect("should collect streamed response").as_ref(),
                    b"firstsecond"
                );
                assert!(attempt.complete(clock.now()));
            }
            assert!(
                !attempt.complete(clock.now()),
                "should complete at most once"
            );
            Ok(())
        },
    );
    (result, observer)
}

#[test]
fn pull_sync_requires_application_delivery_not_successful_fallback() {
    let (success, observer) = dispatch(WriterResult::Application);
    assert_eq!(success.expect("should deliver application"), Some(7));
    assert!(observer.delivered.completed());
    assert_eq!(observer.count.load(Ordering::SeqCst), 1);

    let (fallback, observer) = dispatch(WriterResult::Fallback);
    assert_eq!(fallback.expect("should deliver fallback"), Some(7));
    assert!(
        !observer.delivered.completed(),
        "successful terminal call alone must not authorize pull sync"
    );
    assert_eq!(observer.count.load(Ordering::SeqCst), 1);

    let (failure, observer) = dispatch(WriterResult::SourceFailure);
    assert!(failure.is_err());
    assert!(!observer.delivered.completed());
    assert_eq!(observer.count.load(Ordering::SeqCst), 1);
}

#[test]
fn detached_refusal_skips_routed_state_even_if_its_own_body_completes() {
    let observer = TerminalObserver {
        delivered: DeliveryObservation::default(),
        count: Arc::new(AtomicUsize::new(0)),
    };
    let mut app = App::new(router(false));
    app.set_response_egress_observer(observer.clone());
    let refusal_completions = Arc::new(AtomicUsize::new(0));
    let completed_refusals = Arc::clone(&refusal_completions);
    app.set_ingress_admission_policy(move |_head| {
        let count = Arc::clone(&completed_refusals);
        AdmissionDecision::Refuse {
            completion: edgezero_core::ResponseEgressCompletion::new(move |_report| {
                count.fetch_add(1, Ordering::SeqCst);
            }),
            response: Response::new(Body::from("refused")),
        }
    });
    observer.delivered.0.store(true, Ordering::Release);
    observer.delivered.reset();
    assert!(
        !observer.delivered.completed(),
        "should clear the previous serial dispatch"
    );
    let finalized = AtomicUsize::new(0);
    let result = FastlyService::new(&app)
        .run_request_with_hooks_for_test(
            fastly::Request::get("http://publisher.example.com/"),
            |_raw, _extensions| {},
            |_response| {
                finalized.fetch_add(1, Ordering::SeqCst);
                7
            },
            |envelope| {
                let (prepared, _, mut attempt, clock) = envelope
                    .begin()
                    .map_err(|_| fastly::Error::msg("synthetic refusal begin failure"))?;
                assert!(attempt.begin_writing());
                assert_eq!(
                    block_on(
                        prepared
                            .into_response()
                            .into_body()
                            .into_bytes_bounded(1024)
                    )
                    .expect("should read refusal")
                    .as_ref(),
                    b"refused"
                );
                assert!(attempt.complete(clock.now()));
                Ok(())
            },
        )
        .expect("should finish refusal");
    assert_eq!(result, None, "no routed hook state means no post-send work");
    assert_eq!(finalized.load(Ordering::SeqCst), 0);
    assert!(
        !observer.delivered.completed(),
        "detached refusal must not enter routed observation"
    );
    assert_eq!(observer.count.load(Ordering::SeqCst), 0);
    assert_eq!(refusal_completions.load(Ordering::SeqCst), 1);
}
