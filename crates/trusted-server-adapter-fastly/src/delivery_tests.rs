//! Managed Fastly finalization with an injected terminal writer.
//!
//! These fixtures exercise managed request conversion, finalizers and terminal reports.
//! The injected writer does not qualify SDK response writes or network disconnects.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use edgezero_adapter_fastly::delivery::{
    DeliveryError, FastlyRequestDelivery, FastlyResponseCommitState, FastlyResponseDelivery,
    testing::response_delivery,
};
use edgezero_adapter_fastly::request::FastlyService;
use edgezero_core::app::App;
use edgezero_core::body::Body;
use edgezero_core::context::RequestContext;
use edgezero_core::error::EdgeError;
use edgezero_core::http::{Response, StatusCode, header};
use edgezero_core::ingress::AdmissionDecision;
use edgezero_core::response_egress::{
    ResponseEgressEnvelope, ResponseEgressFallbackDisposition, ResponseEgressObserver,
    ResponseEgressOutcome, ResponseEgressReport,
};
use edgezero_core::response_egress_framing::ResponseEgressFramingMode;
use edgezero_core::router::RouterService;
use futures::executor::block_on;

use super::{DeliveryObservation, apply_terminal_response_effects};

#[derive(Clone)]
struct Observer {
    delivered: DeliveryObservation,
    count: Arc<AtomicUsize>,
}

impl ResponseEgressObserver for Observer {
    fn complete(&self, report: &ResponseEgressReport) {
        self.count.fetch_add(1, Ordering::SeqCst);
        self.delivered.complete(report);
    }
}

#[derive(Clone, Copy)]
enum WriterResult {
    Application,
    Fallback,
    FallbackFailure,
    SourceFailure,
}

fn router(source_failure: bool) -> RouterService {
    RouterService::builder()
        .get("/", move |_context: RequestContext| async move {
            let body = if source_failure {
                Body::from_external_stream(futures::stream::iter([
                    Ok(bytes::Bytes::from_static(b"prefix")),
                    Err(std::io::Error::other("fictional body source failure")),
                ]))
            } else {
                Body::stream(futures::stream::iter([
                    bytes::Bytes::from_static(b"first"),
                    bytes::Bytes::from_static(b"second"),
                ]))
            };
            let mut response = Response::new(body);
            for cookie in ["fixture_a=one", "fixture_b=two"] {
                response.headers_mut().append(
                    header::SET_COOKIE,
                    cookie.parse().expect("should parse fixture cookie"),
                );
            }
            response.headers_mut().insert(
                header::CACHE_CONTROL,
                "public, max-age=60"
                    .parse()
                    .expect("should parse fixture cache policy"),
            );
            Ok::<_, EdgeError>(response)
        })
        .build()
}

fn write(envelope: ResponseEgressEnvelope, case: WriterResult) -> FastlyResponseDelivery {
    let (prepared, _, mut attempt, clock) = envelope
        .begin(ResponseEgressFramingMode::HostAutomatic)
        .map_err(|_| ())
        .expect("should begin fixture egress");
    let result = if matches!(case, WriterResult::Fallback | WriterResult::FallbackFailure) {
        assert!(
            attempt.begin_fallback(ResponseEgressOutcome::ConversionError),
            "should select one fallback"
        );
        assert!(attempt.begin_writing(), "should begin fallback writing");
        let failed = matches!(case, WriterResult::FallbackFailure);
        let disposition = if failed {
            ResponseEgressFallbackDisposition::Aborted
        } else {
            ResponseEgressFallbackDisposition::Completed
        };
        assert!(
            attempt.finish_fallback(disposition, clock.now()),
            "should complete fallback once"
        );
        if failed {
            Err(DeliveryError::Transport)
        } else {
            Ok(())
        }
    } else {
        assert!(attempt.begin_writing(), "should begin application writing");
        let body = block_on(
            prepared
                .into_response()
                .into_body()
                .into_bytes_bounded(1024),
        );
        if matches!(case, WriterResult::SourceFailure) {
            assert!(body.is_err(), "should observe the original body failure");
            assert!(
                attempt.terminate(ResponseEgressOutcome::SourceError, clock.now()),
                "should terminate source failure once"
            );
            Err(DeliveryError::Source)
        } else {
            assert_eq!(
                body.expect("should collect fixture body").as_ref(),
                b"firstsecond",
                "should retain streamed application bytes"
            );
            assert!(
                attempt.complete(clock.now()),
                "should complete application once"
            );
            Ok(())
        }
    };
    assert!(
        !attempt.complete(clock.now()),
        "should reject another terminal transition"
    );
    response_delivery(
        FastlyResponseCommitState::Committed,
        attempt.into_terminal_report(),
        result,
    )
}

fn dispatch(case: WriterResult) -> (FastlyRequestDelivery<u8>, Observer) {
    let observer = Observer {
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
                2,
                "should retain both cookies"
            );
            assert!(
                !response.headers()[header::CACHE_CONTROL]
                    .to_str()
                    .expect("should read cache policy")
                    .contains("public"),
                "should enforce response privacy"
            );
            assert!(
                !before_delivery.delivered.completed(),
                "should not complete at handler return"
            );
            7
        },
        |envelope| write(envelope, case),
    );
    (result, observer)
}

#[test]
fn pull_sync_requires_application_delivery_not_successful_fallback() {
    for case in [
        WriterResult::Application,
        WriterResult::Fallback,
        WriterResult::FallbackFailure,
        WriterResult::SourceFailure,
    ] {
        let (result, observer) = dispatch(case);
        let FastlyRequestDelivery::Routed { state, delivery } = result else {
            panic!("should retain routed finalization state");
        };
        assert_eq!(
            state, 7,
            "should retain routed state even when delivery fails"
        );
        assert_eq!(
            delivery.error(),
            match case {
                WriterResult::SourceFailure => Some(DeliveryError::Source),
                WriterResult::FallbackFailure => Some(DeliveryError::Transport),
                _ => None,
            },
            "should retain the terminal send result"
        );
        assert_eq!(
            observer.delivered.completed(),
            matches!(case, WriterResult::Application),
            "should authorize post-send only after successful application egress"
        );
        assert_eq!(
            observer.count.load(Ordering::SeqCst),
            1,
            "should notify once"
        );
    }
}

#[test]
fn detached_refusal_skips_routed_state_and_application_observer() {
    let observer = Observer {
        delivered: DeliveryObservation::default(),
        count: Arc::new(AtomicUsize::new(0)),
    };
    let mut app = App::new(router(false));
    app.set_response_egress_observer(observer.clone());
    let refused = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&refused);
    app.set_ingress_admission_policy(move |_head| {
        let count = Arc::clone(&count);
        AdmissionDecision::Refuse {
            completion: edgezero_core::ResponseEgressCompletion::new(move |_report| {
                count.fetch_add(1, Ordering::SeqCst);
            }),
            response: edgezero_core::http::response_builder()
                .status(StatusCode::FORBIDDEN)
                .body(Body::from("refused"))
                .expect("should construct refusal"),
        }
    });
    let finalized = AtomicUsize::new(0);
    let result = FastlyService::new(&app).run_request_with_hooks_for_test(
        fastly::Request::get("http://publisher.example.com/"),
        |_raw, _extensions| {},
        |_response| {
            finalized.fetch_add(1, Ordering::SeqCst);
            7
        },
        |envelope| {
            let (prepared, _, mut attempt, clock) = envelope
                .begin(ResponseEgressFramingMode::HostAutomatic)
                .map_err(|_| ())
                .expect("should begin refused egress");
            assert!(attempt.begin_writing(), "should begin refused writing");
            assert_eq!(
                block_on(
                    prepared
                        .into_response()
                        .into_body()
                        .into_bytes_bounded(1024)
                )
                .expect("should collect refusal")
                .as_ref(),
                b"refused",
                "should retain bounded refusal bytes"
            );
            assert!(attempt.complete(clock.now()), "should complete refusal");
            response_delivery(
                FastlyResponseCommitState::Committed,
                attempt.into_terminal_report(),
                Ok(()),
            )
        },
    );
    assert!(
        matches!(result, FastlyRequestDelivery::Detached(_)),
        "should retain detached disposition"
    );
    assert_eq!(
        finalized.load(Ordering::SeqCst),
        0,
        "should not finalize a routed response"
    );
    assert!(
        !observer.delivered.completed(),
        "should not authorize application post-send work"
    );
    assert_eq!(
        observer.count.load(Ordering::SeqCst),
        0,
        "should not invoke application observer"
    );
    assert_eq!(
        refused.load(Ordering::SeqCst),
        1,
        "should complete the refusal once"
    );
}
