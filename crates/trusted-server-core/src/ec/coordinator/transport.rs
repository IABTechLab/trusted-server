//! Sends provider lookups and gathers responses before a shared cutoff.
//!
//! Lookups take the shape of the auction fan-out: every request is built
//! first, then all of them start together in one `EdgeZero` batch with
//! [`HttpClient::start_batch_until`], and collection reads the batch until
//! every slot finishes or the cutoff passes. A slot the cutoff leaves
//! unresolved is a timeout.

use std::time::Duration;

use edgezero_core::error::EdgeError;
use edgezero_core::outbound::{
    HttpClient, OutboundBatch, OutboundBatchNext, OutboundCachePolicy, OutboundRequest,
    OutboundResponse, OutboundSlotResult, ResponseDecoding,
};
use error_stack::Report;
use http::Uri;

use crate::ec::provider::ProviderRequest;
use crate::error::TrustedServerError;
use crate::outbound::{OutboundRequestSource, buffered, outbound_error};
use crate::platform::RuntimeServices;

/// Largest provider response body accepted.
pub const MAX_PROVIDER_RESPONSE_BYTES: usize = 64 * 1024;

/// [`MAX_PROVIDER_RESPONSE_BYTES`] as the response cap `EdgeZero` takes.
const MAX_PROVIDER_RESPONSE_CAP: u64 = 64 * 1024;

/// Sends provider lookups and gathers their responses before a cutoff.
#[allow(async_fn_in_trait)]
pub trait LookupTransport {
    /// Requests started together and not yet gathered.
    type Pending;

    /// Starts every request at once, without waiting for a response.
    ///
    /// The cutoff is `budget` from now, and each request carries `budget` as
    /// its own timeout. A request that cannot be started is reported as
    /// [`TransportError::Send`] by [`Self::gather`].
    fn start(&self, requests: Vec<Tagged<ProviderRequest>>, budget: Duration) -> Self::Pending;

    /// Waits for the started requests until all finish or the cutoff passes.
    ///
    /// Requests still in flight at the cutoff are abandoned and reported as
    /// [`TransportError::Timeout`].
    async fn gather(
        &self,
        pending: Self::Pending,
    ) -> Vec<Tagged<Result<TransportResponse, Report<TransportError>>>>;
}

/// A value labelled with the provider it belongs to.
#[derive(Debug)]
pub struct Tagged<T> {
    /// The provider's position in the coordinator.
    pub provider: ProviderIndex,
    /// The labelled value.
    pub value: T,
}

/// Position of a provider in the coordinator's configured list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProviderIndex(u16);

impl ProviderIndex {
    /// Wraps a position; the coordinator rejects more than `u16::MAX` providers.
    ///
    /// # Panics
    ///
    /// Panics when `index` does not fit in a `u16`.
    pub(crate) fn new(index: usize) -> Self {
        Self(u16::try_from(index).expect("should hold fewer than 65536 providers"))
    }

    /// The position.
    #[must_use]
    pub fn get(self) -> usize {
        usize::from(self.0)
    }
}

/// An owned response; providers parse it by borrowing its body.
#[derive(Debug)]
pub struct TransportResponse {
    /// Response status.
    pub status: http::StatusCode,
    /// Response headers.
    pub headers: http::HeaderMap,
    /// Response body, at most [`MAX_PROVIDER_RESPONSE_BYTES`].
    pub body: bytes::Bytes,
}

/// Why a lookup produced no response.
#[derive(Debug, derive_more::Display)]
pub enum TransportError {
    /// The request could not be sent.
    #[display("lookup request could not be sent")]
    Send,
    /// No response before the cutoff.
    #[display("lookup did not complete before the deadline")]
    Timeout,
    /// The body exceeded [`MAX_PROVIDER_RESPONSE_BYTES`].
    #[display("lookup response exceeded the body limit")]
    BodyTooLarge,
}

impl core::error::Error for TransportError {}

/// [`LookupTransport`] over `EdgeZero`'s outbound client.
///
/// Every lookup always verifies the provider's certificate: `EdgeZero` has no
/// way to turn verification off, and lookups carry the EC ID and the
/// provider's credential. Lookups bypass the cache and decode the response.
/// The cutoff is taken on the client's own clock, the clock its batch
/// records arrivals on.
pub struct OutboundLookupTransport {
    client: HttpClient,
}

impl OutboundLookupTransport {
    /// Sends through `client`.
    #[must_use]
    pub fn new(client: &HttpClient) -> Self {
        Self {
            client: client.clone(),
        }
    }

    /// Sends through the request's outbound client.
    #[must_use]
    pub fn for_services(services: &RuntimeServices) -> Self {
        Self::new(services.outbound_client())
    }
}

/// Lookups an [`OutboundLookupTransport`] started and has not gathered.
///
/// Dropping it cancels every request still in flight.
pub struct InFlightLookups {
    batch: Option<OutboundBatch>,
    /// The provider of each batch slot, by slot position.
    slots: Vec<ProviderIndex>,
    /// Providers whose request could not be built.
    not_started: Vec<ProviderIndex>,
}

impl std::fmt::Debug for InFlightLookups {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InFlightLookups")
            .field("slots", &self.slots.len())
            .field("not_started", &self.not_started.len())
            .finish_non_exhaustive()
    }
}

impl LookupTransport for OutboundLookupTransport {
    type Pending = InFlightLookups;

    fn start(&self, requests: Vec<Tagged<ProviderRequest>>, budget: Duration) -> InFlightLookups {
        let mut slots = Vec::with_capacity(requests.len());
        let mut outbound = Vec::with_capacity(requests.len());
        let mut not_started = Vec::new();
        for Tagged { provider, value } in requests {
            match lookup_request(value, budget) {
                Ok(request) => {
                    slots.push(provider);
                    outbound.push(request);
                }
                Err(error) => {
                    log::warn!(
                        "Identity lookup: request could not be built: {:?}",
                        lookup_failure(error)
                    );
                    not_started.push(provider);
                }
            }
        }
        // Every lookup leaves together, as auction bidders do. The cutoff is
        // taken here so it starts with the batch.
        let batch = (!outbound.is_empty()).then(|| {
            let cutoff = self.client.time().clock().deadline_after(budget);
            self.client.start_batch_until(outbound, cutoff)
        });
        InFlightLookups {
            batch,
            slots,
            not_started,
        }
    }

    async fn gather(
        &self,
        pending: InFlightLookups,
    ) -> Vec<Tagged<Result<TransportResponse, Report<TransportError>>>> {
        let InFlightLookups {
            batch,
            slots,
            not_started,
        } = pending;
        let mut results: Vec<_> = not_started
            .into_iter()
            .map(|provider| Tagged {
                provider,
                value: Err(Report::new(TransportError::Send)),
            })
            .collect();
        let Some(mut batch) = batch else {
            return results;
        };
        let mut unresolved: Vec<Option<ProviderIndex>> = slots.into_iter().map(Some).collect();
        let failed = loop {
            match batch.next().await {
                Ok(OutboundBatchNext::Item(item)) => {
                    let (index, result) = item.into_parts();
                    let Some(provider) = unresolved.get_mut(index).and_then(Option::take) else {
                        log::warn!(
                            "Identity lookup: batch reported unknown slot {index}, ignoring"
                        );
                        continue;
                    };
                    results.push(Tagged {
                        provider,
                        value: read_slot(result).await,
                    });
                }
                Ok(OutboundBatchNext::Finished(_termination)) => break false,
                Err(error) => {
                    log::warn!("Identity lookup: batch failed: {:?}", lookup_failure(error));
                    break true;
                }
            }
        };
        // A cutoff leaves a late lookup unresolved, and a failed batch leaves
        // every slot it had not reported.
        results.extend(unresolved.into_iter().flatten().map(|provider| Tagged {
            provider,
            value: Err(Report::new(if failed {
                TransportError::Send
            } else {
                TransportError::Timeout
            })),
        }));
        results
    }
}

/// Builds the outbound request for one lookup.
///
/// # Errors
///
/// Returns the [`EdgeError`] that parsing the URL or building the request
/// raised.
fn lookup_request(
    request: ProviderRequest,
    budget: Duration,
) -> Result<OutboundRequest, EdgeError> {
    let ProviderRequest {
        method,
        url,
        headers,
        body,
        context: _,
    } = request;
    let uri = url
        .as_str()
        .parse::<Uri>()
        .map_err(|_| EdgeError::bad_request("lookup URL is not a valid URI"))?;
    let mut outbound = OutboundRequest::new(method, uri)?.body(body);
    *outbound.headers_mut() = headers;
    Ok(buffered(
        outbound,
        budget,
        MAX_PROVIDER_RESPONSE_CAP,
        ResponseDecoding::Decode,
    )
    .cache_policy(OutboundCachePolicy::Bypass))
}

/// A printable report of an outbound failure that never carries its message,
/// which can hold the lookup URL and so the EC ID.
fn lookup_failure(error: EdgeError) -> Report<TrustedServerError> {
    outbound_error(
        error,
        TrustedServerError::EdgeCookie {
            message: "identity lookup failed".to_owned(),
        },
        OutboundRequestSource::Internal,
    )
}

/// Turns one batch slot into a lookup response.
async fn read_slot(
    result: OutboundSlotResult,
) -> Result<TransportResponse, Report<TransportError>> {
    let OutboundSlotResult { outcome, .. } = result;
    match outcome {
        Ok(response) => read_response(response).await,
        Err(error) => Err(transport_error(error)),
    }
}

/// Classifies a failed lookup, keeping a message-free report of the cause.
fn transport_error(error: EdgeError) -> Report<TransportError> {
    let kind = match &error {
        EdgeError::GatewayTimeout { .. } => TransportError::Timeout,
        EdgeError::ResponseTooLarge { .. } => TransportError::BodyTooLarge,
        _ => TransportError::Send,
    };
    lookup_failure(error).change_context(kind)
}

async fn read_response(
    response: OutboundResponse,
) -> Result<TransportResponse, Report<TransportError>> {
    let status = response.status();
    let headers = response.headers().clone();
    let body = response
        .into_bytes_bounded(MAX_PROVIDER_RESPONSE_CAP)
        .await
        .map_err(transport_error)?;
    if body.len() > MAX_PROVIDER_RESPONSE_BYTES {
        return Err(Report::new(TransportError::BodyTooLarge));
    }
    Ok(TransportResponse {
        status,
        headers,
        body,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::scripted_client::{ScriptedHttpClient, ScriptedReply, TestClock};
    use edgezero_core::outbound::ResponseMode;

    const BUDGET: Duration = Duration::from_millis(200);

    fn request(path: &str) -> ProviderRequest {
        ProviderRequest {
            method: http::Method::GET,
            url: url::Url::parse(&format!("https://ids.example.com/{path}")).expect("should parse"),
            headers: http::HeaderMap::new(),
            body: bytes::Bytes::new(),
            context: None,
        }
    }

    fn tagged(index: usize, path: &str) -> Tagged<ProviderRequest> {
        Tagged {
            provider: ProviderIndex::new(index),
            value: request(path),
        }
    }

    fn gather_one(
        client: &ScriptedHttpClient,
        clock: &TestClock,
        budget: Duration,
    ) -> Vec<Tagged<Result<TransportResponse, Report<TransportError>>>> {
        let transport = OutboundLookupTransport::new(&client.http_client());
        let pending = transport.start(vec![tagged(0, "a")], budget);
        clock.drive(transport.gather(pending))
    }

    #[test]
    fn gather_returns_tagged_responses() {
        let clock = TestClock::new();
        let client = ScriptedHttpClient::new(&clock);
        client.respond_to(
            http::Method::GET,
            "https://ids.example.com/a",
            ScriptedReply::status(http::StatusCode::OK).body(&b"{\"a\":1}"[..]),
        );
        client.respond_to(
            http::Method::GET,
            "https://ids.example.com/b",
            ScriptedReply::status(http::StatusCode::NO_CONTENT),
        );
        let transport = OutboundLookupTransport::new(&client.http_client());

        let pending = transport.start(vec![tagged(0, "a"), tagged(1, "b")], BUDGET);
        let results = clock.drive(transport.gather(pending));

        assert_eq!(results.len(), 2, "should return one result per request");
        let first = results
            .iter()
            .find(|result| result.provider == ProviderIndex::new(0))
            .expect("should tag provider 0");
        let first = first.value.as_ref().expect("should succeed");
        assert_eq!(first.status, http::StatusCode::OK, "should map provider 0");
        assert_eq!(first.body.as_ref(), b"{\"a\":1}", "should carry the body");
        let second = results
            .iter()
            .find(|result| result.provider == ProviderIndex::new(1))
            .expect("should tag provider 1");
        assert_eq!(
            second.value.as_ref().expect("should succeed").status,
            http::StatusCode::NO_CONTENT,
            "should map provider 1"
        );
        let sent = client.requests();
        assert_eq!(
            sent.iter()
                .map(|request| request.uri.to_string())
                .collect::<Vec<_>>(),
            [
                "https://ids.example.com/a".to_owned(),
                "https://ids.example.com/b".to_owned()
            ],
            "should send both requests"
        );
        for request in &sent {
            assert_eq!(
                request.mode,
                edgezero_core::outbound::OutboundSendMode::Batch,
                "should send every lookup in one batch"
            );
            assert_eq!(
                request.timeout,
                Some(BUDGET),
                "should time out at the budget"
            );
            assert_eq!(
                request.response_mode,
                ResponseMode::Buffered {
                    max_bytes: MAX_PROVIDER_RESPONSE_CAP
                },
                "should cap the buffered response"
            );
            assert_eq!(
                request.cache_policy,
                OutboundCachePolicy::Bypass,
                "should bypass the cache"
            );
        }
    }

    #[test]
    fn gather_reports_timeout_for_requests_past_the_deadline() {
        let clock = TestClock::new();
        let client = ScriptedHttpClient::new(&clock);
        client.respond_to(
            http::Method::GET,
            "https://ids.example.com/a",
            ScriptedReply::status(http::StatusCode::OK).after(Duration::from_millis(50)),
        );

        let results = gather_one(&client, &clock, Duration::from_millis(1));

        let error = results[0].value.as_ref().expect_err("should time out");
        assert!(
            matches!(error.current_context(), TransportError::Timeout),
            "should report timeout"
        );
    }

    #[test]
    fn gather_rejects_oversized_bodies() {
        let clock = TestClock::new();
        let client = ScriptedHttpClient::new(&clock);
        client.respond_to(
            http::Method::GET,
            "https://ids.example.com/a",
            ScriptedReply::status(http::StatusCode::OK)
                .body(vec![b'x'; MAX_PROVIDER_RESPONSE_BYTES + 1]),
        );

        let results = gather_one(&client, &clock, BUDGET);

        let error = results[0]
            .value
            .as_ref()
            .expect_err("should reject the body");
        assert!(
            matches!(error.current_context(), TransportError::BodyTooLarge),
            "should report an oversized body"
        );
    }

    #[test]
    fn gather_with_no_requests_returns_nothing() {
        let clock = TestClock::new();
        let client = ScriptedHttpClient::new(&clock);
        let transport = OutboundLookupTransport::new(&client.http_client());

        let pending = transport.start(Vec::new(), BUDGET);
        let results = futures::executor::block_on(transport.gather(pending));

        assert!(results.is_empty(), "should return nothing");
        client.assert_no_requests();
    }

    #[test]
    fn a_request_that_cannot_be_built_reports_a_send_failure() {
        let clock = TestClock::new();
        let client = ScriptedHttpClient::new(&clock);
        let transport = OutboundLookupTransport::new(&client.http_client());
        let mut unbuildable = request("a");
        unbuildable.url = url::Url::parse("https://user:secret@ids.example.com/a")
            .expect("should parse a URL with userinfo");

        let pending = transport.start(
            vec![Tagged {
                provider: ProviderIndex::new(0),
                value: unbuildable,
            }],
            BUDGET,
        );
        let results = futures::executor::block_on(transport.gather(pending));

        let error = results[0]
            .value
            .as_ref()
            .expect_err("should not send the request");
        assert!(
            matches!(error.current_context(), TransportError::Send),
            "should report a send failure"
        );
        client.assert_no_requests();
    }
}
