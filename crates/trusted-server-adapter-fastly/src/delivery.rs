//! What the Fastly entry point does once `EdgeZero` has delivered a request.
//!
//! `EdgeZero` owns the response from the routed handler to the client and
//! reports how its delivery ended as a [`FastlyRequestDelivery`]. Every
//! delivery is handled here the same way, including the one
//! `send_assembly_error` returns. Each outcome selects exactly one
//! [`DeliveryAction`]: a native 500 when nothing reached the client,
//! post-delivery work after a committed application body, or nothing more.
//! Every failure log carries the sandbox counters, so it can be matched to
//! the response that carried them.

use edgezero_adapter_fastly::delivery::{
    DeliveryError, FastlyNoAttemptReason, FastlyRequestDelivery, FastlyResponseCommitState,
};
use edgezero_core::response_egress::ResponseEgressBodyKind;
use fastly::Response as FastlyResponse;
use fastly::http::StatusCode;

use crate::sandbox::SandboxCounters;

/// What the entry point still has to do for one delivered request.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum DeliveryAction<S> {
    /// Nothing reached the client, so it gets a native 500.
    SendNativeError,
    /// The application's body was committed, so post-delivery work runs with `S`.
    RunPostDelivery(S),
    /// The request is finished. No second response and no post-delivery work.
    Done,
}

/// Handles one delivery: logs its outcome, then sends a native 500 or runs `post`.
///
/// `post` runs only after a committed application body, with the routed
/// finalize state, even when the body failed after commitment.
pub(crate) fn handle_delivery<S>(
    delivery: FastlyRequestDelivery<S>,
    counters: &SandboxCounters,
    post: impl FnOnce(S),
) {
    match delivery_action(delivery, counters) {
        DeliveryAction::SendNativeError => {
            FastlyResponse::from_status(StatusCode::INTERNAL_SERVER_ERROR)
                .with_body_text_plain("Internal Server Error")
                .send_to_client();
        }
        DeliveryAction::RunPostDelivery(state) => post(state),
        DeliveryAction::Done => {}
    }
}

/// Logs `delivery`'s outcome and selects its action, one arm per outcome-table row.
pub(crate) fn delivery_action<S>(
    delivery: FastlyRequestDelivery<S>,
    counters: &SandboxCounters,
) -> DeliveryAction<S> {
    let context = counters.log_context();
    let may_send_native_response = delivery.may_send_native_response();
    match delivery {
        FastlyRequestDelivery::NoAttempt(failure) => {
            match failure.reason() {
                FastlyNoAttemptReason::RequestHeadConversion(cause) => log::error!(
                    "request head conversion failed ({cause:?}); sending a native 500{context}"
                ),
                FastlyNoAttemptReason::RequiredStore(reason) => log::error!(
                    "required store failed to open ({reason:?}), although Trusted Server declares none; sending a native 500{context}"
                ),
                reason => log::error!(
                    "request was not attempted: {}; sending a native 500{context}",
                    reason.message()
                ),
            }
            DeliveryAction::SendNativeError
        }
        FastlyRequestDelivery::Routed { state, delivery }
            if delivery.commitment() == FastlyResponseCommitState::Committed
                && delivery.report().body_kind == ResponseEgressBodyKind::Application =>
        {
            if let Some(error) = delivery.error() {
                log::warn!(
                    "application body failed after commitment: {}{context}",
                    error.message()
                );
            }
            DeliveryAction::RunPostDelivery(state)
        }
        FastlyRequestDelivery::Routed { delivery, .. }
        | FastlyRequestDelivery::Detached(delivery)
            if may_send_native_response =>
        {
            log::error!(
                "response was not committed ({}); sending a native 500{context}",
                delivery.error().map_or("no error", DeliveryError::message)
            );
            DeliveryAction::SendNativeError
        }
        FastlyRequestDelivery::Routed { delivery, .. } => {
            log::error!(
                "routed response ended {:?} with a {:?} body ({}); sending nothing more{context}",
                delivery.commitment(),
                delivery.report().body_kind,
                delivery.error().map_or("no error", DeliveryError::message)
            );
            DeliveryAction::Done
        }
        FastlyRequestDelivery::Detached(delivery) => {
            if delivery.error() == Some(DeliveryError::Source) {
                log::warn!(
                    "detached response body source failed: {:?}{context}",
                    delivery.report()
                );
            } else {
                log::debug!(
                    "delivered detached response: {:?}{context}",
                    delivery.report()
                );
            }
            DeliveryAction::Done
        }
        FastlyRequestDelivery::Aborted => {
            log::error!(
                "ingress admission aborted the request, although Trusted Server installs no abort policy{context}"
            );
            DeliveryAction::Done
        }
        _ => {
            log::error!("unrecognized Fastly request delivery{context}");
            DeliveryAction::Done
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use edgezero_adapter_fastly::FASTLY_PLATFORM;
    use edgezero_adapter_fastly::delivery::testing::{
        Dispatch, deliver_with_hooks, response_delivery,
    };
    use edgezero_adapter_fastly::delivery::{
        DeliveryError, FastlyNoAttemptReason, FastlyRequestHeadFailure, FastlyResponseCommitState,
        FastlyResponseDelivery,
    };
    use edgezero_core::app::App;
    use edgezero_core::body::Body;
    use edgezero_core::context::RequestContext;
    use edgezero_core::error::EdgeError;
    use edgezero_core::http::{Method, Response, request_builder};
    use edgezero_core::ingress::{
        IngressBeginOutcome, IngressDispatchOutcome, IngressFraming, IngressHeadAccounting,
        IngressHeadParts,
    };
    use edgezero_core::response_egress::{
        ResponseEgressEnvelope, ResponseEgressFallbackDisposition, ResponseEgressOutcome,
    };
    use edgezero_core::response_egress_framing::ResponseEgressFramingMode;
    use edgezero_core::router::RouterService;
    use edgezero_core::time::MonotonicClock;
    use futures::executor::block_on;

    use super::*;

    /// Finalize state the routed rows carry through to post-delivery work.
    const ROUTED_STATE: u8 = 7;

    pub(crate) fn counters() -> SandboxCounters {
        SandboxCounters {
            ordinal: 3,
            builds: 1,
            request_id: "request-example".to_owned(),
        }
    }

    fn app() -> App {
        let handler = |_ctx: RequestContext| async {
            Ok::<_, EdgeError>(Response::new(Body::from("example page")))
        };
        App::with_name_and_platform(
            RouterService::builder()
                .route("/", Method::GET, handler)
                .build(),
            "delivery-test",
            FASTLY_PLATFORM,
        )
    }

    fn get(path: &str) -> edgezero_core::http::Request {
        request_builder()
            .method(Method::GET)
            .uri(format!("https://publisher.example.com{path}"))
            .body(Body::empty())
            .expect("should build the example request")
    }

    /// A routed envelope from `App::dispatch_admitted`.
    fn routed(app: &App) -> ResponseEgressEnvelope {
        let request = get("/");
        let head = IngressHeadParts::from_request(
            &request,
            IngressHeadAccounting::HostManaged,
            IngressFraming::HostManaged,
        );
        let Ok(IngressBeginOutcome::Admitted(prepared)) =
            app.begin_ingress(head, app.monotonic_now())
        else {
            panic!("should admit the example request");
        };
        block_on(app.dispatch_admitted(prepared, request))
    }

    /// A detached head-limit envelope from `App::detached_ingress_error_egress`.
    fn detached(app: &App) -> ResponseEgressEnvelope {
        let IngressDispatchOutcome::Response(envelope) = app.detached_ingress_error_egress(
            EdgeError::bad_request("example request head over its limit"),
            Method::GET,
            app.monotonic_now(),
        ) else {
            panic!("should answer a detached head-limit error");
        };
        *envelope
    }

    /// A detached admitted-source-error envelope from `App::admitted_error_egress`.
    fn admitted_source_error(app: &App) -> ResponseEgressEnvelope {
        let request = get("/");
        let head = IngressHeadParts::from_request(
            &request,
            IngressHeadAccounting::HostManaged,
            IngressFraming::HostManaged,
        );
        let Ok(IngressBeginOutcome::Admitted(prepared)) =
            app.begin_ingress(head, app.monotonic_now())
        else {
            panic!("should admit the example request");
        };
        app.admitted_error_egress(
            prepared,
            EdgeError::internal(std::io::Error::other("example body source failed")),
        )
    }

    /// Writes `envelope`'s application body to completion.
    pub(crate) fn committed(envelope: ResponseEgressEnvelope) -> FastlyResponseDelivery {
        let Ok((_prepared, _policy, mut attempt, clock)) =
            envelope.begin(ResponseEgressFramingMode::HostAutomatic)
        else {
            panic!("should begin response egress");
        };
        assert!(attempt.begin_writing(), "should start writing");
        assert!(attempt.complete(clock.now()), "should complete the body");
        response_delivery(
            FastlyResponseCommitState::Committed,
            attempt.into_terminal_report(),
            Ok(()),
        )
    }

    /// Commits `envelope`'s application head, then fails its body with `error`.
    pub(crate) fn committed_then_failed(
        envelope: ResponseEgressEnvelope,
        error: DeliveryError,
    ) -> FastlyResponseDelivery {
        let Ok((_prepared, _policy, mut attempt, clock)) =
            envelope.begin(ResponseEgressFramingMode::HostAutomatic)
        else {
            panic!("should begin response egress");
        };
        assert!(attempt.begin_writing(), "should start writing");
        assert!(
            attempt.terminate(error.outcome(), clock.now()),
            "should fail the body"
        );
        response_delivery(
            FastlyResponseCommitState::Committed,
            attempt.into_terminal_report(),
            Err(error),
        )
    }

    /// Replaces `envelope`'s body with the adapter fallback and delivers that.
    pub(crate) fn committed_fallback(envelope: ResponseEgressEnvelope) -> FastlyResponseDelivery {
        let Ok((_prepared, _policy, mut attempt, clock)) =
            envelope.begin(ResponseEgressFramingMode::HostAutomatic)
        else {
            panic!("should begin response egress");
        };
        assert!(
            attempt.begin_fallback(ResponseEgressOutcome::ConversionError),
            "should select the fallback"
        );
        assert!(attempt.begin_writing(), "should start writing the fallback");
        assert!(
            attempt.finish_fallback(ResponseEgressFallbackDisposition::Completed, clock.now()),
            "should finish the fallback"
        );
        response_delivery(
            FastlyResponseCommitState::Committed,
            attempt.into_terminal_report(),
            Ok(()),
        )
    }

    /// Fails `envelope` before its head reaches the host.
    pub(crate) fn not_committed(envelope: ResponseEgressEnvelope) -> FastlyResponseDelivery {
        let Ok((_prepared, _policy, attempt, _clock)) =
            envelope.begin(ResponseEgressFramingMode::HostAutomatic)
        else {
            panic!("should begin response egress");
        };
        response_delivery(
            FastlyResponseCommitState::NotCommitted,
            attempt.into_terminal_report(),
            Err(DeliveryError::Conversion),
        )
    }

    /// Hands `envelope`'s head to a host that rejects the send.
    pub(crate) fn unknown_commit(envelope: ResponseEgressEnvelope) -> FastlyResponseDelivery {
        let Ok((_prepared, _policy, mut attempt, _clock)) =
            envelope.begin(ResponseEgressFramingMode::HostAutomatic)
        else {
            panic!("should begin response egress");
        };
        assert!(attempt.begin_writing(), "should start writing");
        response_delivery(
            FastlyResponseCommitState::Unknown,
            attempt.into_terminal_report(),
            Err(DeliveryError::Transport),
        )
    }

    pub(crate) fn routed_delivery(
        deliver: impl FnOnce(ResponseEgressEnvelope) -> FastlyResponseDelivery,
    ) -> FastlyRequestDelivery<u8> {
        deliver_with_hooks(Dispatch::Routed(routed(&app())), |_| ROUTED_STATE, deliver)
    }

    fn detached_delivery(
        envelope: ResponseEgressEnvelope,
        deliver: impl FnOnce(ResponseEgressEnvelope) -> FastlyResponseDelivery,
    ) -> FastlyRequestDelivery<u8> {
        deliver_with_hooks(
            Dispatch::Detached(envelope),
            |_| panic!("should never finalize a detached response"),
            deliver,
        )
    }

    fn no_attempt(reason: FastlyNoAttemptReason) -> FastlyRequestDelivery<u8> {
        deliver_with_hooks(
            Dispatch::NoAttempt(reason),
            |_| panic!("should never finalize an unattempted request"),
            |_| panic!("should never deliver an unattempted request"),
        )
    }

    #[test]
    fn delivery_no_attempt_head_conversion_sends_native_500() {
        let delivery = no_attempt(FastlyNoAttemptReason::RequestHeadConversion(
            FastlyRequestHeadFailure::InvalidUri,
        ));

        assert_eq!(
            delivery_action(delivery, &counters()),
            DeliveryAction::SendNativeError,
            "should answer an unconvertible request head with a native 500"
        );
    }

    #[test]
    fn delivery_no_attempt_required_store_sends_native_500() {
        let delivery = no_attempt(FastlyNoAttemptReason::RequiredStore(
            edgezero_core::key_value_store::KvStoreOpenFailure::MissingBinding,
        ));

        assert_eq!(
            delivery_action(delivery, &counters()),
            DeliveryAction::SendNativeError,
            "should answer the unreachable required-store failure with a native 500"
        );
    }

    #[test]
    fn delivery_routed_committed_application_body_runs_post_delivery() {
        let delivery = routed_delivery(committed);

        assert_eq!(
            delivery_action(delivery, &counters()),
            DeliveryAction::RunPostDelivery(ROUTED_STATE),
            "should run post-delivery work with the finalize state"
        );
    }

    #[test]
    fn delivery_routed_committed_application_body_with_error_runs_post_delivery() {
        for error in [DeliveryError::Source, DeliveryError::Transport] {
            let delivery = routed_delivery(|envelope| committed_then_failed(envelope, error));

            assert_eq!(
                delivery_action(delivery, &counters()),
                DeliveryAction::RunPostDelivery(ROUTED_STATE),
                "should still run post-delivery work after a {error:?} failure once committed"
            );
        }
    }

    #[test]
    fn delivery_routed_not_committed_sends_native_500() {
        let delivery = routed_delivery(not_committed);

        assert_eq!(
            delivery_action(delivery, &counters()),
            DeliveryAction::SendNativeError,
            "should replace an uncommitted routed response with a native 500"
        );
    }

    #[test]
    fn delivery_detached_not_committed_sends_native_500() {
        let delivery = detached_delivery(detached(&app()), not_committed);

        assert_eq!(
            delivery_action(delivery, &counters()),
            DeliveryAction::SendNativeError,
            "should replace an uncommitted detached response with a native 500"
        );
    }

    #[test]
    fn delivery_routed_unknown_commit_sends_nothing_more() {
        let delivery = routed_delivery(unknown_commit);

        assert_eq!(
            delivery_action(delivery, &counters()),
            DeliveryAction::Done,
            "should neither resend nor run post-delivery work when commitment is unknown"
        );
    }

    #[test]
    fn delivery_routed_committed_fallback_body_sends_nothing_more() {
        let delivery = routed_delivery(committed_fallback);

        assert_eq!(
            delivery_action(delivery, &counters()),
            DeliveryAction::Done,
            "should run no post-delivery work for a delivered fallback"
        );
    }

    #[test]
    fn delivery_detached_committed_sends_nothing_more() {
        for envelope in [detached(&app()), admitted_source_error(&app())] {
            let delivery = detached_delivery(envelope, committed);

            assert_eq!(
                delivery_action(delivery, &counters()),
                DeliveryAction::Done,
                "should only log a delivered detached response"
            );
        }
    }

    #[test]
    fn delivery_detached_source_error_sends_nothing_more() {
        let delivery = detached_delivery(detached(&app()), |envelope| {
            committed_then_failed(envelope, DeliveryError::Source)
        });

        assert_eq!(
            delivery_action(delivery, &counters()),
            DeliveryAction::Done,
            "should only log a detached response whose body source failed"
        );
    }

    #[test]
    fn delivery_aborted_sends_nothing_more() {
        let delivery = deliver_with_hooks::<u8, _, _>(
            Dispatch::Aborted,
            |_| panic!("should never finalize an aborted request"),
            |_| panic!("should never deliver an aborted request"),
        );

        assert_eq!(
            delivery_action(delivery, &counters()),
            DeliveryAction::Done,
            "should never answer a request admission aborted"
        );
    }

    #[test]
    fn assembly_error_delivery_is_handled_like_any_other() {
        let clock = MonotonicClock::default();
        let assembly = || {
            App::assembly_error_egress(
                EdgeError::service_unavailable("example settings unavailable"),
                Method::GET,
                clock.now(),
                clock.clone(),
            )
        };

        let delivered = deliver_with_hooks::<(), _, _>(
            Dispatch::Detached(assembly()),
            |_| panic!("should never finalize an assembly error"),
            committed,
        );
        let not_delivered = deliver_with_hooks::<(), _, _>(
            Dispatch::Detached(assembly()),
            |_| panic!("should never finalize an assembly error"),
            not_committed,
        );

        assert_eq!(
            delivery_action(delivered, &counters()),
            DeliveryAction::Done,
            "should only log a delivered assembly error"
        );
        assert_eq!(
            delivery_action(not_delivered, &counters()),
            DeliveryAction::SendNativeError,
            "should replace an uncommitted assembly error with a native 500"
        );
    }

    #[test]
    fn handle_delivery_runs_post_only_after_a_committed_application_body() {
        let mut ran = Vec::new();

        handle_delivery(routed_delivery(committed), &counters(), |state| {
            ran.push(state);
        });
        handle_delivery(routed_delivery(committed_fallback), &counters(), |state| {
            ran.push(state);
        });
        handle_delivery(routed_delivery(unknown_commit), &counters(), |state| {
            ran.push(state);
        });

        assert_eq!(
            ran,
            vec![ROUTED_STATE],
            "should run post-delivery work once, for the committed application body"
        );
    }
}
