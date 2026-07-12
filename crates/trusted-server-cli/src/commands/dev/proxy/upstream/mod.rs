pub mod body;
pub mod connect;
pub mod dns;
pub mod key;
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
use self::manager::{Acquired, Manager};
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
    pub fn upload_initially_complete(self) -> bool {
        self.upload_initially_complete
    }

    #[must_use]
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

pub struct UpstreamClient {
    manager: Arc<Manager<UpstreamSender>>,
    metrics: Arc<ProxyMetrics>,
    connect_policy: connect::ConnectPolicy,
    dns: Arc<dns::DnsCache>,
}

#[derive(Debug, Clone, Copy)]
pub struct UpstreamOptions {
    pub limits: manager::PoolLimits,
    pub dns_cache: bool,
    pub connect_delay: Duration,
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
    pub fn new(metrics: Arc<ProxyMetrics>, connect_timeout: Duration) -> Self {
        Self::with_options(metrics, connect_timeout, UpstreamOptions::default())
    }

    #[must_use]
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
        let request_started = tokio::time::Instant::now();
        let replay_template = ReplayTemplate::capture(&request, metadata);
        let mut stale_retry = false;
        let mut lease = loop {
            let acquisition_started = tokio::time::Instant::now();
            let acquired = self
                .manager
                .acquire(rule.origin_key().clone())
                .await
                .map_err(|error| {
                    Report::new(ProxyError::Server)
                        .attach(format!("pool acquire failed: {error:?}"))
                })?;
            self.metrics
                .record_pool_acquisition(acquisition_started.elapsed());
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
                    let connector = connect::PendingConnection::spawn(
                        rule.origin_key().clone(),
                        outcome.sni.clone(),
                        self.connect_policy,
                        Arc::clone(&self.metrics),
                        Arc::clone(&self.manager),
                        Arc::clone(&self.dns),
                        reservation.id(),
                    );
                    self.manager
                        .register_connector(reservation.id(), connector.abort_handle());
                    let opened = connector
                        .finish()
                        .await
                        .change_context(ProxyError::Server)?;
                    let start = opened.start;
                    let lease = reservation.register(&self.manager, opened.sender, opened.abort);
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
                    self.metrics.record_pool_stale();
                    self.metrics.record_pool_retry();
                    candidate.connection.abort.abort();
                }
                Err(error) => {
                    candidate.connection.abort.abort();
                    self.metrics.record_request_failed();
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
                    self.metrics.record_request_failed();
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
        Ok(Response::from_parts(parts, pooled.boxed()))
    }

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
        let acquired = self
            .manager
            .acquire_fresh(rule.origin_key().clone())
            .await
            .map_err(|error| {
                Report::new(ProxyError::Server).attach(format!("pool acquire failed: {error:?}"))
            })?;
        self.metrics
            .record_pool_acquisition(acquisition_started.elapsed());
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
                let connector = connect::PendingConnection::spawn(
                    rule.origin_key().clone(),
                    outcome.sni.clone(),
                    self.connect_policy,
                    Arc::clone(&self.metrics),
                    Arc::clone(&self.manager),
                    Arc::clone(&self.dns),
                    reservation.id(),
                );
                self.manager
                    .register_connector(reservation.id(), connector.abort_handle());
                let opened = connector
                    .finish()
                    .await
                    .change_context(ProxyError::Server)?;
                let start = opened.start;
                let lease = reservation.register(&self.manager, opened.sender, opened.abort);
                Ok((lease, Some(start)))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use http_body_util::Empty;

    use super::*;

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
}
