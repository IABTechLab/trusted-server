//! Sends provider lookups and gathers responses before a shared deadline.

use std::cell::Cell;
use std::time::Duration;

use edgezero_core::body::Body as EdgeBody;
use error_stack::{Report, ResultExt as _};

use crate::ec::provider::ProviderRequest;
use crate::platform::{
    PlatformBackendSpec, PlatformHttpRequest, PlatformPendingRequest, PlatformResponse,
    RuntimeServices,
};

/// Largest provider response body accepted.
pub const MAX_PROVIDER_RESPONSE_BYTES: usize = 64 * 1024;

/// Smallest transport timeout registered for a lookup backend, in
/// milliseconds; the shared deadline still bounds collection.
const MIN_TRANSPORT_TIMEOUT_MS: u32 = 50;

/// Sends provider lookups and gathers their responses before a deadline.
#[allow(async_fn_in_trait)]
pub trait LookupTransport {
    /// An in-flight request.
    type Pending;

    /// Starts one request without waiting for its response.
    ///
    /// # Errors
    ///
    /// Returns [`TransportError::Send`] when the request cannot be started.
    async fn start(
        &self,
        request: ProviderRequest,
        deadline: Deadline,
    ) -> Result<Self::Pending, Report<TransportError>>;

    /// Waits for in-flight requests until all finish or `deadline` passes.
    ///
    /// Requests still in flight at the deadline are abandoned and reported as
    /// [`TransportError::Timeout`].
    async fn gather(
        &self,
        pending: Vec<Tagged<Self::Pending>>,
        deadline: Deadline,
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

/// End of the shared lookup budget.
#[derive(Debug, Clone, Copy)]
pub struct Deadline(web_time::Instant);

impl Deadline {
    /// A deadline `budget` from now.
    #[must_use]
    pub fn after(budget: Duration) -> Self {
        Self(web_time::Instant::now() + budget)
    }

    /// The deadline instant.
    #[must_use]
    pub fn instant(self) -> web_time::Instant {
        self.0
    }

    /// Time left before the deadline; zero once it has passed.
    #[must_use]
    pub fn remaining(self) -> Duration {
        self.0.saturating_duration_since(web_time::Instant::now())
    }
}

/// Why a lookup produced no response.
#[derive(Debug, derive_more::Display)]
pub enum TransportError {
    /// The request could not be sent.
    #[display("lookup request could not be sent")]
    Send,
    /// No response before the deadline.
    #[display("lookup did not complete before the deadline")]
    Timeout,
    /// The body exceeded [`MAX_PROVIDER_RESPONSE_BYTES`].
    #[display("lookup response exceeded the body limit")]
    BodyTooLarge,
}

impl core::error::Error for TransportError {}

/// [`LookupTransport`] over the platform HTTP client.
///
/// Each request gets its own backend name (a per-transport sequence number is
/// the backend discriminator), so a ready response maps back to its provider
/// by backend name. Names repeat across requests and the transport timeout is
/// canonicalized, keeping the number of registered dynamic backends bounded.
pub struct PlatformLookupTransport<'s> {
    services: &'s RuntimeServices,
    next: Cell<u32>,
}

impl<'s> PlatformLookupTransport<'s> {
    /// Borrows the request's runtime services.
    ///
    /// Lookups always verify the provider's certificate: they carry the EC ID
    /// and the provider's credential, so `proxy.certificate_check` (meant for
    /// development origins) does not apply to them.
    #[must_use]
    pub fn new(services: &'s RuntimeServices) -> Self {
        Self {
            services,
            next: Cell::new(0),
        }
    }

    fn transport_timeout(&self, deadline: Deadline) -> Duration {
        let remaining_ms = u32::try_from(deadline.remaining().as_millis()).unwrap_or(u32::MAX);
        let canonical = self
            .services
            .backend()
            .canonicalize_transport_timeout_ms(remaining_ms, u32::MAX);
        Duration::from_millis(u64::from(canonical.max(MIN_TRANSPORT_TIMEOUT_MS)))
    }
}

impl LookupTransport for PlatformLookupTransport<'_> {
    type Pending = PlatformPendingRequest;

    async fn start(
        &self,
        request: ProviderRequest,
        deadline: Deadline,
    ) -> Result<PlatformPendingRequest, Report<TransportError>> {
        let ProviderRequest {
            method,
            url,
            headers,
            body,
            context: _,
        } = request;
        let host = url
            .host_str()
            .ok_or_else(|| Report::new(TransportError::Send))?
            .to_owned();
        let timeout = self.transport_timeout(deadline);
        let sequence = self.next.get();
        self.next.set(sequence.wrapping_add(1));
        let backend = self
            .services
            .backend()
            .ensure(&PlatformBackendSpec {
                scheme: url.scheme().to_owned(),
                host,
                port: url.port(),
                host_header_override: None,
                certificate_check: true,
                first_byte_timeout: timeout,
                between_bytes_timeout: timeout,
                discriminator: Some(format!("ts-identity-{sequence}")),
            })
            .change_context(TransportError::Send)?;
        let mut builder = http::Request::builder().method(method).uri(url.as_str());
        if let Some(target) = builder.headers_mut() {
            *target = headers;
        }
        let edge_request = builder
            .body(EdgeBody::from(body.to_vec()))
            .change_context(TransportError::Send)?;
        self.services
            .http_client()
            .send_async(PlatformHttpRequest::new(edge_request, backend).with_cache_bypass())
            .await
            .change_context(TransportError::Send)
    }

    async fn gather(
        &self,
        pending: Vec<Tagged<PlatformPendingRequest>>,
        deadline: Deadline,
    ) -> Vec<Tagged<Result<TransportResponse, Report<TransportError>>>> {
        // Names are captured before the first select: some adapters drop
        // them from the requests that remain in flight.
        let mut outstanding: Vec<(String, ProviderIndex)> = Vec::with_capacity(pending.len());
        let mut remaining = Vec::with_capacity(pending.len());
        let mut results = Vec::with_capacity(pending.len());
        for Tagged { provider, value } in pending {
            match value.backend_name() {
                Some(name) => {
                    outstanding.push((name.to_owned(), provider));
                    remaining.push(value);
                }
                None => results.push(Tagged {
                    provider,
                    value: Err(Report::new(TransportError::Send)),
                }),
            }
        }
        while !remaining.is_empty() {
            match self
                .services
                .http_client()
                .select_until(remaining, deadline.instant())
                .await
            {
                Ok(Some(selected)) => {
                    remaining = selected.remaining;
                    let (name, value) = match selected.ready {
                        Ok(response) => {
                            (response.backend_name.clone(), read_response(response).await)
                        }
                        Err(report) => (
                            selected.failed_backend_name,
                            Err(report.change_context(TransportError::Send)),
                        ),
                    };
                    match name.and_then(|name| take_provider(&mut outstanding, &name)) {
                        Some(provider) => results.push(Tagged { provider, value }),
                        None => {
                            log::warn!("Identity lookup: dropping response with unknown backend");
                        }
                    }
                }
                Ok(None) => break,
                Err(report) => {
                    log::warn!("Identity lookup: select failed: {report:?}");
                    break;
                }
            }
        }
        // Every request not yet answered timed out or was abandoned.
        results.extend(outstanding.into_iter().map(|(_, provider)| Tagged {
            provider,
            value: Err(Report::new(TransportError::Timeout)),
        }));
        results
    }
}

/// Removes the first outstanding request with backend `name`.
fn take_provider(
    outstanding: &mut Vec<(String, ProviderIndex)>,
    name: &str,
) -> Option<ProviderIndex> {
    let index = outstanding
        .iter()
        .position(|(candidate, _)| candidate == name)?;
    Some(outstanding.remove(index).1)
}

async fn read_response(
    response: PlatformResponse,
) -> Result<TransportResponse, Report<TransportError>> {
    let (parts, body) = response.response.into_parts();
    let body = match body {
        EdgeBody::Once(bytes) => bytes,
        stream => stream
            .into_bytes_bounded(MAX_PROVIDER_RESPONSE_BYTES)
            .await
            .change_context(TransportError::BodyTooLarge)?,
    };
    if body.len() > MAX_PROVIDER_RESPONSE_BYTES {
        return Err(Report::new(TransportError::BodyTooLarge));
    }
    Ok(TransportResponse {
        status: parts.status,
        headers: parts.headers,
        body,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::test_support::{StubHttpClient, build_services_with_http_client};
    use std::sync::Arc;

    fn request(path: &str) -> ProviderRequest {
        ProviderRequest {
            method: http::Method::GET,
            url: url::Url::parse(&format!("https://ids.example.com/{path}")).expect("should parse"),
            headers: http::HeaderMap::new(),
            body: bytes::Bytes::new(),
            context: None,
        }
    }

    fn gather_one(
        transport: &PlatformLookupTransport<'_>,
        deadline: Deadline,
    ) -> Vec<Tagged<Result<TransportResponse, Report<TransportError>>>> {
        futures::executor::block_on(async {
            let pending = transport
                .start(request("a"), deadline)
                .await
                .expect("should start");
            transport
                .gather(
                    vec![Tagged {
                        provider: ProviderIndex::new(0),
                        value: pending,
                    }],
                    deadline,
                )
                .await
        })
    }

    #[test]
    fn gather_returns_tagged_responses() {
        let client = Arc::new(StubHttpClient::new());
        client.push_response(200, b"{\"a\":1}".to_vec());
        client.push_response(204, Vec::new());
        let services = build_services_with_http_client(client.clone());
        let transport = PlatformLookupTransport::new(&services);
        let deadline = Deadline::after(Duration::from_millis(200));

        let results = futures::executor::block_on(async {
            let first = transport
                .start(request("a"), deadline)
                .await
                .expect("should start");
            let second = transport
                .start(request("b"), deadline)
                .await
                .expect("should start");
            transport
                .gather(
                    vec![
                        Tagged {
                            provider: ProviderIndex::new(0),
                            value: first,
                        },
                        Tagged {
                            provider: ProviderIndex::new(1),
                            value: second,
                        },
                    ],
                    deadline,
                )
                .await
        });

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
        assert_eq!(
            client.recorded_request_uris(),
            [
                "https://ids.example.com/a".to_owned(),
                "https://ids.example.com/b".to_owned()
            ],
            "should send both requests"
        );
    }

    #[test]
    fn gather_reports_timeout_for_requests_past_the_deadline() {
        let client = Arc::new(StubHttpClient::new());
        client.push_response(200, Vec::new());
        client.push_select_delay(Duration::from_millis(50));
        let services = build_services_with_http_client(client.clone());
        let transport = PlatformLookupTransport::new(&services);

        let results = gather_one(&transport, Deadline::after(Duration::from_millis(1)));

        let error = results[0].value.as_ref().expect_err("should time out");
        assert!(
            matches!(error.current_context(), TransportError::Timeout),
            "should report timeout"
        );
    }

    #[test]
    fn gather_rejects_oversized_bodies() {
        let client = Arc::new(StubHttpClient::new());
        client.push_response(200, vec![b'x'; MAX_PROVIDER_RESPONSE_BYTES + 1]);
        let services = build_services_with_http_client(client.clone());
        let transport = PlatformLookupTransport::new(&services);

        let results = gather_one(&transport, Deadline::after(Duration::from_millis(200)));

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
        let client = Arc::new(StubHttpClient::new());
        let services = build_services_with_http_client(client.clone());
        let transport = PlatformLookupTransport::new(&services);

        let results = futures::executor::block_on(
            transport.gather(Vec::new(), Deadline::after(Duration::from_millis(1))),
        );

        assert!(results.is_empty(), "should return nothing");
        assert_eq!(
            client.recorded_select_count(),
            0,
            "should not select an empty list"
        );
    }
}
