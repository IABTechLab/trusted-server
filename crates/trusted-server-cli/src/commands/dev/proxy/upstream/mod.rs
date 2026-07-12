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
use hyper::body::Incoming;
use hyper::{Request, Response};

use self::body::{PooledResponseBody, RequestUploadBody};
use self::connect::UpstreamSender;
use self::manager::{Acquired, Manager};
use super::ProxyError;
use super::metrics::ProxyMetrics;
use super::rewrite::{RewriteOutcome, Rule};

pub struct UpstreamClient {
    manager: Arc<Manager<UpstreamSender>>,
    metrics: Arc<ProxyMetrics>,
    connect_timeout: Duration,
    dns: Arc<dns::DnsCache>,
}

impl UpstreamClient {
    #[must_use]
    pub fn new(metrics: Arc<ProxyMetrics>, connect_timeout: Duration) -> Self {
        let perf_variant = std::env::var("TS_PERF_VARIANT").unwrap_or_default();
        let pool_disabled = matches!(
            perf_variant.as_str(),
            "baseline" | "dns_no_cache" | "dns_cache"
        );
        let limits = if pool_disabled {
            manager::PoolLimits {
                per_origin_live: 64,
                per_origin_idle: 0,
                ..manager::PoolLimits::default()
            }
        } else if perf_variant == "cap20" {
            manager::PoolLimits {
                per_origin_live: 20,
                ..manager::PoolLimits::default()
            }
        } else {
            manager::PoolLimits::default()
        };
        Self {
            manager: Manager::start(limits),
            metrics,
            connect_timeout,
            dns: Arc::new(dns::DnsCache::new(perf_variant != "dns_no_cache")),
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
        rule: &Rule,
        outcome: &RewriteOutcome,
    ) -> Result<Response<BoxBody<Bytes, hyper::Error>>, Report<ProxyError>> {
        let request_started = tokio::time::Instant::now();
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

            let (mut candidate, start) = match acquired {
                Acquired::Reused(lease) => {
                    self.metrics.record_pool_hit();
                    (lease, None)
                }
                Acquired::Open(reservation) => {
                    self.metrics.record_pool_miss();
                    let opened = connect::open(
                        rule.origin_key(),
                        outcome.sni.clone(),
                        self.connect_timeout,
                        Arc::clone(&self.metrics),
                        Arc::clone(&self.manager),
                        Arc::clone(&self.dns),
                        reservation.id(),
                    )
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

        let known_empty = !request
            .headers()
            .contains_key(hyper::header::CONTENT_LENGTH)
            && !request
                .headers()
                .contains_key(hyper::header::TRANSFER_ENCODING);
        let (parts, body) = request.into_parts();
        let (body, upload_state) = RequestUploadBody::new(body, known_empty);
        let request = Request::from_parts(parts, body);
        let response = match lease.connection.value.send_request(request).await {
            Ok(response) => response,
            Err(error) => {
                lease.connection.abort.abort();
                self.metrics.record_request_failed();
                return Err(Report::new(ProxyError::Server).attach(error.to_string()));
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
}
