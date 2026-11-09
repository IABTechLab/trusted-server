//! Outbound request settings and failure mapping for `EdgeZero`'s HTTP client.
//!
//! Every outbound request goes through [`buffered`] or [`streamed`]. Both set a
//! one second connect timeout and a constant total timeout, never one computed
//! from a remaining budget, so Fastly backend identity stays stable. Every
//! failure from the client, or from building the request, goes through
//! [`outbound_error`], which keeps the site's own error except for the three
//! statuses the client caused.

use std::time::Duration;

use edgezero_core::error::{BudgetSource, EdgeError};
use edgezero_core::error_detail::ProviderFailureDetail;
use edgezero_core::outbound::{OutboundRequest, ResponseDecoding};
use error_stack::Report;
use http::StatusCode;

use crate::error::TrustedServerError;

/// Connect timeout every outbound request carries.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(1);

/// Total timeout for a streamed request whose site names no timeout.
pub const STREAMED_TIMEOUT: Duration = Duration::from_secs(60);

/// Total timeout for a buffered request whose site names no timeout.
pub const BUFFERED_TIMEOUT: Duration = Duration::from_secs(15);

/// Deadline for a whole redirect chain, measured from its first request.
pub const REDIRECT_CHAIN_DEADLINE: Duration = Duration::from_secs(60);

/// Response cap for a buffered site that has no Trusted Server cap of its own.
///
/// It keeps the 10 MiB platform limit those sites had before `EdgeZero`'s
/// smaller default.
pub const DEFAULT_BUFFERED_RESPONSE_BYTES: u64 = 10 * 1024 * 1024;

/// Cap on a publisher request body forwarded to the origin.
pub const PUBLISHER_UPLOAD_BYTES: u64 = 16 * 1024 * 1024;

/// Applies the buffered-response settings every buffered site carries.
///
/// Sets [`CONNECT_TIMEOUT`], the constant total `timeout`, the response cap
/// `max_response_bytes`, and the response `decoding`.
///
/// # Examples
///
/// ```
/// use edgezero_core::outbound::{OutboundRequest, ResponseDecoding, ResponseMode};
/// use trusted_server_core::outbound::{
///     BUFFERED_TIMEOUT, CONNECT_TIMEOUT, DEFAULT_BUFFERED_RESPONSE_BYTES, buffered,
/// };
///
/// let request = OutboundRequest::get("https://example.com/config.json")
///     .expect("should build a GET request");
/// let parts = buffered(
///     request,
///     BUFFERED_TIMEOUT,
///     DEFAULT_BUFFERED_RESPONSE_BYTES,
///     ResponseDecoding::Decode,
/// )
/// .into_parts();
/// assert_eq!(parts.connect_timeout, Some(CONNECT_TIMEOUT));
/// assert_eq!(parts.timeout, Some(BUFFERED_TIMEOUT));
/// assert_eq!(
///     parts.response_mode,
///     ResponseMode::Buffered {
///         max_bytes: DEFAULT_BUFFERED_RESPONSE_BYTES
///     }
/// );
/// ```
#[must_use]
pub fn buffered(
    request: OutboundRequest,
    timeout: Duration,
    max_response_bytes: u64,
    decoding: ResponseDecoding,
) -> OutboundRequest {
    request
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(timeout)
        .max_response_bytes(max_response_bytes)
        .response_decoding(decoding)
}

/// Applies the streamed-response settings every streamed site carries.
///
/// Sets [`CONNECT_TIMEOUT`], the constant total `timeout`, a streamed
/// response, and [`ResponseDecoding::Original`], so the bytes reach the
/// caller in the coding the upstream sent.
///
/// # Examples
///
/// ```
/// use edgezero_core::outbound::{OutboundRequest, ResponseDecoding, ResponseMode};
/// use trusted_server_core::outbound::{CONNECT_TIMEOUT, STREAMED_TIMEOUT, streamed};
///
/// let request = OutboundRequest::get("https://example.com/article")
///     .expect("should build a GET request");
/// let parts = streamed(request, STREAMED_TIMEOUT).into_parts();
/// assert_eq!(parts.connect_timeout, Some(CONNECT_TIMEOUT));
/// assert_eq!(parts.timeout, Some(STREAMED_TIMEOUT));
/// assert_eq!(parts.response_mode, ResponseMode::Streamed);
/// assert_eq!(parts.response_decoding, ResponseDecoding::Original);
/// ```
#[must_use]
pub fn streamed(request: OutboundRequest, timeout: Duration) -> OutboundRequest {
    request
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(timeout)
        .stream_response()
        .response_decoding(ResponseDecoding::Original)
}

/// Who shaped a failed outbound request, which decides what its 400 means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutboundRequestSource {
    /// The request forwards the client's own request, so a 400 is the client's.
    ClientForward,
    /// Trusted Server built the request, so a 400 is the site's own failure.
    Internal,
}

/// Budget input that selected the deadline a failed outbound request reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq, derive_more::Display)]
#[display("outbound budget source: {_0:?}")]
struct OutboundBudgetSource(BudgetSource);

/// Text-free provider evidence for a failed outbound request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, derive_more::Display)]
#[display("outbound provider failure detail: {_0:?}")]
struct OutboundFailureDetail(ProviderFailureDetail);

/// Maps an outbound client or request-construction failure onto a Trusted
/// Server error.
///
/// The mapping follows [`EdgeError::status`]:
///
/// - 413 becomes [`TrustedServerError::RequestTooLarge`].
/// - 408 becomes [`TrustedServerError::RequestTimeout`].
/// - 400 becomes [`TrustedServerError::BadRequest`] only for
///   [`OutboundRequestSource::ClientForward`].
/// - Every other failure keeps `context`, the site's own error, which is a
///   502 for every outbound site.
///
/// The report carries `error`, the budget source of a timeout, and the
/// error's provider failure detail. The detail is logged at warn with
/// `{:?}`. The log never carries the error's message, which can hold provider
/// text or a URL.
///
/// # Examples
///
/// ```
/// use edgezero_core::error::EdgeError;
/// use trusted_server_core::error::{IntoHttpResponse as _, TrustedServerError};
/// use trusted_server_core::outbound::{OutboundRequestSource, outbound_error};
///
/// let site = TrustedServerError::Proxy {
///     message: "example origin request failed".to_owned(),
/// };
/// let report = outbound_error(
///     EdgeError::bad_gateway("upstream connection failed"),
///     site,
///     OutboundRequestSource::Internal,
/// );
/// assert_eq!(report.current_context().status_code().as_u16(), 502);
/// assert!(report.downcast_ref::<EdgeError>().is_some());
/// ```
pub fn outbound_error(
    error: EdgeError,
    context: TrustedServerError,
    source: OutboundRequestSource,
) -> Report<TrustedServerError> {
    let status = error.status();
    let budget_source = match &error {
        EdgeError::GatewayTimeout { cause, .. } => Some(*cause),
        _ => None,
    };
    let detail = error.provider_failure_detail().copied();
    log::warn!(
        "Outbound request failed with {status} ({kind}): budget source {budget_source:?}, provider detail {detail:?}",
        kind = error.kind()
    );

    let mapped = match (status, source) {
        (StatusCode::PAYLOAD_TOO_LARGE, _) => Some(TrustedServerError::RequestTooLarge {
            message: "request body exceeds the outbound upload limit".to_owned(),
        }),
        (StatusCode::REQUEST_TIMEOUT, _) => Some(TrustedServerError::RequestTimeout {
            message: "request body was not read before the read deadline".to_owned(),
        }),
        (StatusCode::BAD_REQUEST, OutboundRequestSource::ClientForward) => {
            Some(TrustedServerError::BadRequest {
                message: "request could not be forwarded".to_owned(),
            })
        }
        _ => None,
    };

    let report = match mapped {
        // Keep the replaced site error readable, so the log still names the site.
        Some(mapped) => Report::new(mapped).attach(format!("in place of: {context}")),
        None => Report::new(context),
    };
    let mut report = report.attach(error);
    if let Some(budget_source) = budget_source {
        report = report.attach(OutboundBudgetSource(budget_source));
    }
    if let Some(detail) = detail {
        report = report.attach(OutboundFailureDetail(detail));
    }
    report
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::time::Duration;

    use edgezero_core::error::{BudgetSource, EdgeError, RequestBodyOverflowOrigin};
    use edgezero_core::error_detail::{
        FastlyFailureCause, ProviderFailureDetail, ProviderFailurePhase,
    };
    use edgezero_core::outbound::{OutboundRequest, ResponseDecoding, ResponseMode};
    use http::StatusCode;

    use super::*;
    use crate::error::IntoHttpResponse as _;
    use crate::test_support::logging::WarningCapture;

    const BOTH_SOURCES: [OutboundRequestSource; 2] = [
        OutboundRequestSource::ClientForward,
        OutboundRequestSource::Internal,
    ];

    fn site_error() -> TrustedServerError {
        TrustedServerError::Proxy {
            message: "example origin request failed".to_owned(),
        }
    }

    fn attached_edge_error(report: &Report<TrustedServerError>) -> String {
        report
            .downcast_ref::<EdgeError>()
            .expect("should attach the EdgeError")
            .to_string()
    }

    fn example_request() -> OutboundRequest {
        OutboundRequest::get("https://example.com/resource").expect("should build a GET request")
    }

    #[test]
    fn outbound_error_maps_payload_too_large_to_413() {
        for source in BOTH_SOURCES {
            let report = outbound_error(
                EdgeError::payload_too_large(RequestBodyOverflowOrigin::OutboundUpload),
                site_error(),
                source,
            );

            assert!(
                matches!(
                    report.current_context(),
                    TrustedServerError::RequestTooLarge { .. }
                ),
                "should map a {source:?} upload overflow to RequestTooLarge"
            );
            assert_eq!(
                report.current_context().status_code(),
                StatusCode::PAYLOAD_TOO_LARGE,
                "should answer a {source:?} upload overflow with 413"
            );
            assert_eq!(
                attached_edge_error(&report),
                "request body too large",
                "should attach the {source:?} overflow EdgeError"
            );
        }
    }

    #[test]
    fn outbound_error_maps_request_timeout_to_408() {
        for source in BOTH_SOURCES {
            let report = outbound_error(
                EdgeError::request_timeout("inbound read deadline passed"),
                site_error(),
                source,
            );

            assert!(
                matches!(
                    report.current_context(),
                    TrustedServerError::RequestTimeout { .. }
                ),
                "should map a {source:?} read deadline to RequestTimeout"
            );
            assert_eq!(
                report.current_context().status_code(),
                StatusCode::REQUEST_TIMEOUT,
                "should answer a {source:?} read deadline with 408"
            );
            assert_eq!(
                attached_edge_error(&report),
                "inbound read deadline passed",
                "should attach the {source:?} read deadline EdgeError"
            );
        }
    }

    #[test]
    fn outbound_error_maps_bad_request_to_400_for_client_forward() {
        let report = outbound_error(
            EdgeError::bad_request("GET request carries a body"),
            site_error(),
            OutboundRequestSource::ClientForward,
        );

        assert!(
            matches!(
                report.current_context(),
                TrustedServerError::BadRequest { .. }
            ),
            "should map a forwarded client's bad request to BadRequest"
        );
        assert_eq!(
            report.current_context().status_code(),
            StatusCode::BAD_REQUEST,
            "should answer a forwarded client's bad request with 400"
        );
        assert_eq!(
            attached_edge_error(&report),
            "GET request carries a body",
            "should attach the bad request EdgeError"
        );
    }

    #[test]
    fn outbound_error_maps_bad_request_to_site_502_for_internal() {
        let report = outbound_error(
            EdgeError::bad_request("request URI carries userinfo"),
            site_error(),
            OutboundRequestSource::Internal,
        );

        assert!(
            matches!(
                report.current_context(),
                TrustedServerError::Proxy { message } if message == "example origin request failed"
            ),
            "should keep the site context for a request Trusted Server built"
        );
        assert_eq!(
            report.current_context().status_code(),
            StatusCode::BAD_GATEWAY,
            "should answer an internal bad request with the site's 502"
        );
        assert_eq!(
            attached_edge_error(&report),
            "request URI carries userinfo",
            "should attach the internal bad request EdgeError"
        );
    }

    #[test]
    fn outbound_error_keeps_site_502_for_gateway_timeout_bad_gateway_response_too_large_and_internal()
     {
        let failures: [fn() -> EdgeError; 4] = [
            || {
                EdgeError::gateway_timeout_caused(
                    "upstream timed out",
                    BudgetSource::PerCallTimeout,
                )
            },
            || EdgeError::bad_gateway("upstream connection failed"),
            || EdgeError::response_too_large("upstream response exceeds its cap"),
            || EdgeError::internal(io::Error::other("provider send failed")),
        ];

        for failure in failures {
            for source in BOTH_SOURCES {
                let expected_display = failure().to_string();
                let report = outbound_error(failure(), site_error(), source);

                assert!(
                    matches!(
                        report.current_context(),
                        TrustedServerError::Proxy { message } if message == "example origin request failed"
                    ),
                    "should keep the site context for {source:?} `{expected_display}`"
                );
                assert_eq!(
                    report.current_context().status_code(),
                    StatusCode::BAD_GATEWAY,
                    "should answer {source:?} `{expected_display}` with the site's 502"
                );
                assert_eq!(
                    attached_edge_error(&report),
                    expected_display,
                    "should attach the {source:?} EdgeError"
                );
            }
        }
    }

    #[test]
    fn outbound_error_attaches_edge_error_detail() {
        let detail = ProviderFailureDetail::Fastly {
            phase: ProviderFailurePhase::Send,
            cause: FastlyFailureCause::ConnectionTimeout,
        };
        let report = outbound_error(
            EdgeError::gateway_timeout_with_detail(
                "upstream timed out",
                BudgetSource::ConnectTimeout,
                detail,
            ),
            site_error(),
            OutboundRequestSource::Internal,
        );

        assert_eq!(
            report.current_context().status_code(),
            StatusCode::BAD_GATEWAY,
            "should answer a connect timeout with the site's 502"
        );
        assert_eq!(
            attached_edge_error(&report),
            "upstream timed out",
            "should attach the timeout EdgeError"
        );
        assert_eq!(
            report.downcast_ref::<OutboundBudgetSource>(),
            Some(&OutboundBudgetSource(BudgetSource::ConnectTimeout)),
            "should attach the budget source that selected the deadline"
        );
        assert_eq!(
            report.downcast_ref::<OutboundFailureDetail>(),
            Some(&OutboundFailureDetail(detail)),
            "should attach the provider failure detail"
        );
    }

    #[test]
    fn outbound_error_logs_provider_detail_without_provider_text() {
        let detail = ProviderFailureDetail::Fastly {
            phase: ProviderFailurePhase::Send,
            cause: FastlyFailureCause::ConnectionRefused,
        };
        let capture = WarningCapture::start();

        let _report = outbound_error(
            EdgeError::bad_gateway_with_detail(
                "provider said no to https://example.com/secret-path",
                edgezero_core::error::BadGatewayReason::Transport,
                detail,
            ),
            site_error(),
            OutboundRequestSource::Internal,
        );

        let warnings = capture.take();
        assert_eq!(warnings.len(), 1, "should log one warning per failure");
        let message = &warnings[0].message;
        assert!(
            message.contains(&format!("{detail:?}")),
            "should log the provider detail with Debug, got `{message}`"
        );
        assert!(
            !message.contains("provider said no") && !message.contains("example.com"),
            "should never log provider text or URLs, got `{message}`"
        );
    }

    #[test]
    fn buffered_sets_one_second_connect_timeout_and_constant_timeout() {
        let site_timeout = Duration::from_millis(2_000);

        for (timeout, max_response_bytes, decoding) in [
            (
                BUFFERED_TIMEOUT,
                DEFAULT_BUFFERED_RESPONSE_BYTES,
                ResponseDecoding::Decode,
            ),
            (site_timeout, 64 * 1024, ResponseDecoding::Original),
        ] {
            let parts =
                buffered(example_request(), timeout, max_response_bytes, decoding).into_parts();

            assert_eq!(
                parts.connect_timeout,
                Some(Duration::from_secs(1)),
                "should bound every buffered connect at one second"
            );
            assert_eq!(
                parts.timeout,
                Some(timeout),
                "should set the constant total timeout"
            );
            assert!(
                parts.deadline.is_none(),
                "should never derive a deadline from a remaining budget"
            );
            assert_eq!(
                parts.response_mode,
                ResponseMode::Buffered {
                    max_bytes: max_response_bytes
                },
                "should buffer the response under the site cap"
            );
            assert_eq!(
                parts.response_decoding, decoding,
                "should apply the site's response decoding"
            );
        }
        assert_eq!(
            BUFFERED_TIMEOUT,
            Duration::from_secs(15),
            "should default buffered sites to 15 s"
        );
        assert_eq!(
            DEFAULT_BUFFERED_RESPONSE_BYTES,
            10 * 1024 * 1024,
            "should keep today's 10 MiB platform cap for uncapped sites"
        );
    }

    #[test]
    fn streamed_sets_original_decoding_and_stream_response() {
        let parts = streamed(example_request(), STREAMED_TIMEOUT).into_parts();

        assert_eq!(
            parts.connect_timeout,
            Some(Duration::from_secs(1)),
            "should bound every streamed connect at one second"
        );
        assert_eq!(
            parts.timeout,
            Some(Duration::from_secs(60)),
            "should default streamed sites to 60 s"
        );
        assert!(
            parts.deadline.is_none(),
            "should never derive a deadline from a remaining budget"
        );
        assert_eq!(
            parts.response_mode,
            ResponseMode::Streamed,
            "should stream the response"
        );
        assert_eq!(
            parts.response_decoding,
            ResponseDecoding::Original,
            "should deliver streamed bytes in their original coding"
        );
    }
}
