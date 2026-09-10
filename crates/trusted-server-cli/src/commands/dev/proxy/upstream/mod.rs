/// Streaming upload and pooled response-body adapters.
pub mod body;
/// DNS/TCP/TLS/HTTP connection establishment.
pub mod connect;
/// Bounded TTL DNS cache with concurrent-miss coalescing.
pub mod dns;
/// Strong upstream origin identity types.
pub mod key;
/// Bounded connection-pool actor and lifecycle types.
pub mod manager;

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use error_stack::{Report, ResultExt as _};
use http_body_util::{BodyExt as _, combinators::BoxBody};
use hyper::body::{Body, Incoming};
use hyper::{Request, Response};

use self::body::{PooledResponseBody, RequestUploadBody};
use self::connect::UpstreamSender;
use self::manager::{AcquireError, Acquired, Manager};
use super::ProxyError;
use super::metrics::ProxyMetrics;
use super::rewrite::{RewriteOutcome, Rule};

/// Immutable body/framing facts captured before hop-by-hop sanitation.
#[derive(Debug, Clone, Copy)]
pub struct RequestMetadata {
    upload_initially_complete: bool,
    replayable: bool,
}

impl RequestMetadata {
    #[must_use]
    /// Captures framing and replay facts before hop-by-hop sanitation.
    pub fn capture<B: Body>(request: &Request<B>) -> Self {
        let unframed = !request
            .headers()
            .contains_key(hyper::header::CONTENT_LENGTH)
            && !request
                .headers()
                .contains_key(hyper::header::TRANSFER_ENCODING);
        let replayable_method = matches!(
            *request.method(),
            hyper::Method::GET | hyper::Method::HEAD | hyper::Method::OPTIONS
        );
        Self {
            // The browser leg is HTTP/1.1. An unframed request has no message body.
            upload_initially_complete: unframed,
            replayable: replayable_method
                && unframed
                && request.body().size_hint().exact() == Some(0)
                && request.body().is_end_stream()
                && request.extensions().is_empty(),
        }
    }

    #[must_use]
    /// Returns whether the browser upload is already known to be complete.
    pub fn upload_initially_complete(self) -> bool {
        self.upload_initially_complete
    }

    #[must_use]
    /// Returns whether the request can be reconstructed for one safe stale retry.
    pub fn replayable(self) -> bool {
        self.replayable
    }
}

#[derive(Clone)]
struct ReplayTemplate {
    method: hyper::Method,
    uri: hyper::Uri,
    version: hyper::Version,
    headers: hyper::HeaderMap,
}

impl ReplayTemplate {
    fn capture<B>(request: &Request<B>, metadata: RequestMetadata) -> Option<Self> {
        metadata.replayable().then(|| Self {
            method: request.method().clone(),
            uri: request.uri().clone(),
            version: request.version(),
            headers: request.headers().clone(),
        })
    }

    fn build(&self) -> Request<RequestUploadBody> {
        let mut request = Request::new(RequestUploadBody::empty());
        *request.method_mut() = self.method.clone();
        *request.uri_mut() = self.uri.clone();
        *request.version_mut() = self.version;
        *request.headers_mut() = self.headers.clone();
        request
    }
}

/// Process-shared bounded upstream HTTP/1 client and DNS cache.
pub struct UpstreamClient {
    manager: Arc<Manager<UpstreamSender>>,
    metrics: Arc<ProxyMetrics>,
    connect_policy: connect::ConnectPolicy,
    dns: Arc<dns::DnsCache>,
}

struct SendOutcomeGuard<'a> {
    metrics: &'a ProxyMetrics,
    handed_to_response_body: bool,
}

impl<'a> SendOutcomeGuard<'a> {
    fn new(metrics: &'a ProxyMetrics) -> Self {
        Self {
            metrics,
            handed_to_response_body: false,
        }
    }

    fn hand_to_response_body(&mut self) {
        self.handed_to_response_body = true;
    }
}

impl Drop for SendOutcomeGuard<'_> {
    fn drop(&mut self) {
        if !self.handed_to_response_body {
            self.metrics.record_request_failed();
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum AcquisitionMode {
    Normal,
    FreshAfterReadinessFailure,
}

async fn acquire_for_mode<T: Send + 'static>(
    manager: &Manager<T>,
    key: key::OriginKey,
    mode: AcquisitionMode,
) -> Result<Acquired<T>, AcquireError> {
    match mode {
        AcquisitionMode::Normal => manager.acquire(key).await,
        AcquisitionMode::FreshAfterReadinessFailure => manager.acquire_fresh(key).await,
    }
}

#[derive(Debug, Clone, Copy)]
/// Production pool defaults plus harness-only upstream timing controls.
pub struct UpstreamOptions {
    /// Manager connection, idle, waiter, and timeout bounds.
    pub limits: manager::PoolLimits,
    /// Whether successful DNS results are cached for the bounded TTL.
    pub dns_cache: bool,
    /// Harness-only delay before each new TCP connection.
    pub connect_delay: Duration,
    /// Harness-only delay before each new TLS handshake.
    pub tls_delay: Duration,
}

impl Default for UpstreamOptions {
    fn default() -> Self {
        Self {
            limits: manager::PoolLimits::default(),
            dns_cache: true,
            connect_delay: Duration::ZERO,
            tls_delay: Duration::ZERO,
        }
    }
}

impl UpstreamClient {
    #[must_use]
    /// Creates a client with production pool and DNS defaults.
    pub fn new(metrics: Arc<ProxyMetrics>, connect_timeout: Duration) -> Self {
        Self::with_options(metrics, connect_timeout, UpstreamOptions::default())
    }

    #[must_use]
    /// Creates a client with explicit harness or boundary-test options.
    pub fn with_options(
        metrics: Arc<ProxyMetrics>,
        connect_timeout: Duration,
        options: UpstreamOptions,
    ) -> Self {
        Self {
            manager: Manager::start(options.limits),
            metrics,
            connect_policy: connect::ConnectPolicy {
                timeout: connect_timeout,
                connect_delay: options.connect_delay,
                tls_delay: options.tls_delay,
            },
            dns: Arc::new(dns::DnsCache::new(options.dns_cache)),
        }
    }

    /// Sends one mapped request over a bounded reusable HTTP/1 connection.
    ///
    /// # Errors
    ///
    /// Returns an acquisition, connection, handshake, or request-dispatch error.
    pub async fn send(
        &self,
        request: Request<Incoming>,
        metadata: RequestMetadata,
        rule: &Rule,
        outcome: &RewriteOutcome,
    ) -> Result<Response<BoxBody<Bytes, hyper::Error>>, Report<ProxyError>> {
        let mut outcome_guard = SendOutcomeGuard::new(&self.metrics);
        let request_started = tokio::time::Instant::now();
        let replay_template = ReplayTemplate::capture(&request, metadata);
        let mut stale_retry = false;
        let mut acquisition_mode = AcquisitionMode::Normal;
        let mut lease = loop {
            let acquisition_started = tokio::time::Instant::now();
            let acquired =
                acquire_for_mode(&self.manager, rule.origin_key().clone(), acquisition_mode).await;
            self.metrics
                .record_pool_acquisition(acquisition_started.elapsed());
            let acquired = acquired.map_err(|error| {
                Report::new(ProxyError::Server).attach(format!("pool acquire failed: {error:?}"))
            })?;
            if let Some(queue_wait) = acquired.queue_wait() {
                self.metrics.record_queue_wait(queue_wait);
            }

            let (mut candidate, start) = match acquired {
                Acquired::Reused(lease) => {
                    self.metrics.record_pool_hit();
                    (lease, None)
                }
                Acquired::Open(reservation) => {
                    self.metrics.record_pool_miss();
                    let connector_id = reservation.id();
                    let connector = connect::PendingConnection::spawn(
                        rule.origin_key().clone(),
                        outcome.sni.clone(),
                        self.connect_policy,
                        Arc::clone(&self.metrics),
                        Arc::clone(&self.manager),
                        Arc::clone(&self.dns),
                        reservation,
                    );
                    self.manager
                        .register_connector(connector_id, connector.abort_handle());
                    let opened = connector
                        .finish()
                        .await
                        .change_context(ProxyError::Server)?;
                    let (lease, start) = opened.register(&self.manager);
                    (lease, Some(start))
                }
            };
            if let Some(start) = start {
                let _ = start.send(());
            }
            match candidate.connection.value.ready().await {
                Ok(_) => break candidate,
                Err(_error) if candidate.reused && !stale_retry => {
                    stale_retry = true;
                    acquisition_mode = AcquisitionMode::FreshAfterReadinessFailure;
                    self.metrics.record_pool_stale();
                    self.metrics.record_pool_retry();
                    candidate.connection.abort.abort();
                }
                Err(error) => {
                    candidate.connection.abort.abort();
                    return Err(Report::new(ProxyError::Server).attach(error.to_string()));
                }
            }
        };

        let (parts, body) = request.into_parts();
        let (body, upload_state) =
            RequestUploadBody::new(body, metadata.upload_initially_complete());
        let request = Request::from_parts(parts, body);
        let mut request = request;
        let response = loop {
            match lease.connection.value.send_request(request).await {
                Ok(response) => break response,
                // Replaying a non-idempotent or streaming request could submit it
                // twice after an origin processed the first attempt. Return 502
                // instead unless pre-sanitization metadata proved an empty
                // GET/HEAD/OPTIONS request can be reconstructed exactly.
                Err(_error) if lease.reused && !stale_retry && replay_template.is_some() => {
                    stale_retry = true;
                    self.metrics.record_pool_stale();
                    self.metrics.record_pool_retry();
                    lease.connection.abort.abort();
                    let (replacement, start) = self.acquire_connection(rule, outcome).await?;
                    lease = replacement;
                    if let Some(start) = start {
                        let _ = start.send(());
                    }
                    let Some(template) = replay_template.as_ref() else {
                        return Err(Report::new(ProxyError::Server)
                            .attach("replay eligibility lost before retry"));
                    };
                    request = template.build();
                }
                Err(error) => {
                    lease.connection.abort.abort();
                    return Err(Report::new(ProxyError::Server).attach(error.to_string()));
                }
            }
        };
        self.metrics
            .record_request_to_headers(request_started.elapsed());
        let close_intent = response
            .headers()
            .get_all(hyper::header::CONNECTION)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .flat_map(|value| value.split(','))
            .any(|token| token.trim().eq_ignore_ascii_case("close"));
        let (parts, body) = response.into_parts();
        let pooled = PooledResponseBody::new(
            body,
            lease,
            Arc::clone(&self.manager),
            upload_state,
            close_intent,
            Arc::clone(&self.metrics),
        );
        outcome_guard.hand_to_response_body();
        Ok(Response::from_parts(parts, pooled.boxed()))
    }

    /// Aborts connectors and drivers, then waits for lifecycle reconciliation.
    pub async fn shutdown(&self) {
        self.manager.shutdown().await;
    }

    async fn acquire_connection(
        &self,
        rule: &Rule,
        outcome: &RewriteOutcome,
    ) -> Result<
        (
            manager::Lease<UpstreamSender>,
            Option<tokio::sync::oneshot::Sender<()>>,
        ),
        Report<ProxyError>,
    > {
        let acquisition_started = tokio::time::Instant::now();
        let acquired = self.manager.acquire_fresh(rule.origin_key().clone()).await;
        self.metrics
            .record_pool_acquisition(acquisition_started.elapsed());
        let acquired = acquired.map_err(|error| {
            Report::new(ProxyError::Server).attach(format!("pool acquire failed: {error:?}"))
        })?;
        if let Some(queue_wait) = acquired.queue_wait() {
            self.metrics.record_queue_wait(queue_wait);
        }
        match acquired {
            Acquired::Reused(lease) => {
                self.metrics.record_pool_hit();
                Ok((lease, None))
            }
            Acquired::Open(reservation) => {
                self.metrics.record_pool_miss();
                let connector_id = reservation.id();
                let connector = connect::PendingConnection::spawn(
                    rule.origin_key().clone(),
                    outcome.sni.clone(),
                    self.connect_policy,
                    Arc::clone(&self.metrics),
                    Arc::clone(&self.manager),
                    Arc::clone(&self.dns),
                    reservation,
                );
                self.manager
                    .register_connector(connector_id, connector.abort_handle());
                let opened = connector
                    .finish()
                    .await
                    .change_context(ProxyError::Server)?;
                let (lease, start) = opened.register(&self.manager);
                Ok((lease, Some(start)))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use http_body_util::Empty;

    use super::key::{AddressPolicy, OriginKey, ReferenceIdentity, Transport, VerifyMode};
    use super::manager::PoolLimits;
    use super::*;

    fn key() -> OriginKey {
        OriginKey::new(
            Transport::Tls,
            ReferenceIdentity::dns("readiness.example"),
            443,
            VerifyMode::Secure,
            AddressPolicy::Dns,
        )
    }

    fn abort_handle() -> tokio::task::AbortHandle {
        tokio::spawn(std::future::pending::<()>()).abort_handle()
    }

    #[test]
    fn replay_template_reconstructs_only_provably_empty_idempotent_request() {
        let mut request = Request::new(Empty::<Bytes>::new());
        *request.method_mut() = hyper::Method::GET;
        *request.uri_mut() = "/asset?version=1".parse().expect("should parse URI");
        request
            .headers_mut()
            .insert("x-test", hyper::header::HeaderValue::from_static("value"));
        let metadata = RequestMetadata::capture(&request);

        let template =
            ReplayTemplate::capture(&request, metadata).expect("empty GET should be replayable");
        let rebuilt = template.build();

        assert_eq!(rebuilt.method(), hyper::Method::GET);
        assert_eq!(
            rebuilt.uri().path_and_query().expect("path").as_str(),
            "/asset?version=1"
        );
        assert_eq!(rebuilt.headers()["x-test"], "value");
        assert!(rebuilt.body().is_end_stream());
    }

    #[test]
    fn replay_template_rejects_extensions_and_non_idempotent_methods() {
        let mut request = Request::new(Empty::<Bytes>::new());
        request.extensions_mut().insert(7_u8);
        assert!(ReplayTemplate::capture(&request, RequestMetadata::capture(&request)).is_none());

        request.extensions_mut().clear();
        *request.method_mut() = hyper::Method::POST;
        assert!(ReplayTemplate::capture(&request, RequestMetadata::capture(&request)).is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn readiness_retry_waits_for_a_fresh_reservation() {
        let manager = Manager::start(PoolLimits {
            per_origin_live: 2,
            global_live: 2,
            ..PoolLimits::default()
        });
        let mut reservations = Vec::new();
        for _ in 0..2 {
            let Acquired::Open(reservation) = manager.acquire(key()).await.expect("should reserve")
            else {
                panic!("should open connection");
            };
            reservations.push(reservation);
        }
        let mut ids = Vec::new();
        for (reservation, value) in reservations.into_iter().zip([1_u8, 2_u8]) {
            ids.push(reservation.id());
            let lease = reservation.register(&manager, value, abort_handle());
            manager.return_idle(lease.connection);
        }
        tokio::task::yield_now().await;

        let retry = tokio::spawn({
            let manager = Arc::clone(&manager);
            async move {
                acquire_for_mode(&manager, key(), AcquisitionMode::FreshAfterReadinessFailure).await
            }
        });
        tokio::task::yield_now().await;

        assert!(
            !retry.is_finished(),
            "readiness retry must discard idle senders and wait for fresh capacity"
        );
        manager.driver_closed(ids[0]);
        tokio::task::yield_now().await;
        assert!(matches!(
            retry.await.expect("should join retry"),
            Ok(Acquired::Open(_))
        ));
    }
}
