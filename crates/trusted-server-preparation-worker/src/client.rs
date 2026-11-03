//! The worker's configuration and its two HTTP clients: the Encypher API,
//! with the worker credential, and the publisher's delivery instance, with
//! the publication scope's secret. Neither ever carries a reader's state.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use bytes::Bytes;
use serde::Deserialize;

use trusted_server_provenance_encypher::preparation::VariantSchema;
use trusted_server_provenance_encypher::preparation::pipeline::{
    ApiAnswer, ApiRequest, ApiTransport, Failure, FailureClass, ProbeArtifactCache,
};
use trusted_server_provenance_encypher::preparation::protocol::{
    MARKERS_PREFIX, MAX_TARGET_PATH_BYTES, MarkerEnvelope, PREPARATION_PROTOCOL, RESULTS_PREFIX,
    ResultEnvelope, SNAPSHOT_META_HEADER, SNAPSHOT_PATH, SnapshotMeta, SnapshotMode,
    SnapshotRequest, TOKEN_HEADER, Target, credential_context, domain_digest,
};

use crate::store::{Bounds, Store, StoreError};

/// The worker's configuration file, TOML.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerConfig {
    /// The address the worker listens on, such as `0.0.0.0:8112`.
    pub listen: String,
    /// The directory holding the redb store and source objects.
    pub store_dir: PathBuf,
    /// The deployment, as the delivery instance's configuration names it.
    pub deployment_id: String,
    /// The Encypher API origin, the same as the delivery instance's.
    pub api_base_url: String,
    /// The claimed domain, the same as the delivery instance's.
    pub domain: String,
    /// The host prepared addresses name, the same as the delivery
    /// instance's `public_host`.
    pub public_host: String,
    /// The delivery instance's origin the worker reads snapshots from and
    /// publishes results to.
    pub edge_url: String,
    /// The article selector override, the same as the delivery instance's.
    #[serde(default)]
    pub article_selector: String,
    /// The least article text signed, the same as the delivery instance's.
    #[serde(default = "default_min_text_length")]
    pub min_text_length: usize,
    /// The largest bounded media item, the same as the delivery instance's.
    #[serde(default = "default_max_media_bytes")]
    pub max_media_bytes: usize,
    /// The largest large source, `0` to refuse every large source.
    #[serde(default)]
    pub max_large_media_bytes: u64,
    /// The snapshot ceiling, reserved before acquisition even when size is unknown.
    #[serde(default = "default_max_snapshot_bytes")]
    pub max_snapshot_bytes: usize,
    /// Total attempt deadline, including network and probe work.
    #[serde(default = "default_preparation_seconds")]
    pub max_preparation_seconds: u64,
    /// The variant schema, the same as the delivery instance's.
    #[serde(default)]
    pub variants: VariantSchema,
    /// Environment variables naming the secrets: the worker credential and
    /// the three scopes. Secrets are never written in this file.
    pub secrets: SecretNames,
    /// Concurrent preparation tasks.
    #[serde(default = "default_workers")]
    pub workers: usize,
    /// The hard byte quota of retained sources and outputs.
    #[serde(default = "default_quota")]
    pub artifact_quota_bytes: u64,
    /// Targets reconciled periodically and warmed at start.
    #[serde(default)]
    pub inventory: Vec<Target>,
    /// Reconciliation cadence: targets per minute, between 1 and 128.
    #[serde(default = "default_scan_per_minute")]
    pub scan_per_minute: usize,
    /// The longest normal reconciliation interval, in seconds, between 60 and
    /// 900. With `scan_per_minute` it bounds every inventory: configured,
    /// registered and persisted. See [`WorkerConfig::store_bounds`].
    #[serde(default = "default_scan_interval")]
    pub scan_interval_seconds: u64,
}

/// The environment variables holding the worker's secrets.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretNames {
    /// The `cwk_` worker credential.
    pub worker_token: String,
    /// The reader-signal scope.
    pub signal_secret: String,
    /// The snapshot and publication scope.
    pub publication_secret: String,
    /// The operator scope: warming, retirement, status.
    pub operator_secret: String,
    /// An optional staging canary token, sent as `X-Encypher-Canary-Token`
    /// on signing calls only, where the API's admission is canary-gated.
    #[serde(default)]
    pub canary_token: Option<String>,
}

fn default_min_text_length() -> usize {
    50
}

fn default_max_media_bytes() -> usize {
    5_000_000
}

fn default_max_snapshot_bytes() -> usize {
    64 * 1024 * 1024
}

fn default_preparation_seconds() -> u64 {
    120
}

fn default_workers() -> usize {
    4
}

fn default_quota() -> u64 {
    10 * 1024 * 1024 * 1024
}

fn default_scan_per_minute() -> usize {
    128
}

fn default_scan_interval() -> u64 {
    900
}

/// The secrets, read from the environment once at start.
#[derive(Clone)]
pub struct Secrets {
    /// The worker credential.
    pub worker_token: String,
    /// The reader-signal scope.
    pub signal: String,
    /// The publication scope.
    pub publication: String,
    /// The operator scope.
    pub operator: String,
    /// The staging canary token.
    pub canary: Option<String>,
}

impl std::fmt::Debug for Secrets {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Secrets([REDACTED])")
    }
}

/// Why the configuration is not usable.
#[derive(Debug, Clone, PartialEq, Eq, derive_more::Display)]
#[display("preparation worker configuration: {_0}")]
pub struct ConfigError(pub String);

impl core::error::Error for ConfigError {}

impl WorkerConfig {
    /// Reads and checks the configuration at `path`.
    ///
    /// # Errors
    ///
    /// A [`ConfigError`] naming what is wrong.
    pub fn load(path: &Path) -> Result<(Self, Secrets), ConfigError> {
        let text = std::fs::read_to_string(path)
            .map_err(|error| ConfigError(format!("cannot read {}: {error}", path.display())))?;
        let config: Self = toml::from_str(&text).map_err(|error| ConfigError(error.to_string()))?;
        config.check()?;
        let secrets = config.secrets()?;
        Ok((config, secrets))
    }

    pub(crate) fn check(&self) -> Result<(), ConfigError> {
        let https_or_loopback = |value: &str| {
            url::Url::parse(value).is_ok_and(|url| {
                (url.scheme() == "https"
                    || (url.scheme() == "http"
                        && matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))))
                    && url.host_str().is_some()
                    && url.username().is_empty()
                    && url.password().is_none()
                    && url.query().is_none()
                    && url.fragment().is_none()
                    && matches!(url.path(), "" | "/")
            })
        };
        if !https_or_loopback(&self.api_base_url) || !https_or_loopback(&self.edge_url) {
            return Err(ConfigError(
                "api_base_url and edge_url must be https, or http on a loopback host".to_owned(),
            ));
        }
        if self
            .public_host
            .strip_prefix("www.")
            .unwrap_or(&self.public_host)
            != self.domain
        {
            return Err(ConfigError(
                "public_host must be the domain or its www. name".to_owned(),
            ));
        }
        let public = url::Url::parse(&format!("https://{}/", self.public_host))
            .map_err(|_| ConfigError("public_host is invalid".to_owned()))?;
        if public.host_str() != Some(self.public_host.as_str())
            || public.port().is_some()
            || !public.username().is_empty()
            || public.password().is_some()
            || public.path() != "/"
            || public.query().is_some()
            || public.fragment().is_some()
            || self.deployment_id.is_empty()
            || self.deployment_id.len() > 256
        {
            return Err(ConfigError(
                "public_host or deployment_id is invalid".to_owned(),
            ));
        }
        if !self.variants.is_bounded() {
            return Err(ConfigError("variants exceed their bounds".to_owned()));
        }
        if self.workers == 0 || self.workers > 4 {
            return Err(ConfigError("workers must be between 1 and 4".to_owned()));
        }
        if self.inventory.len() > Bounds::default().max_inventory
            || !self.inventory.iter().all(|target| {
                target.is_well_formed()
                    && self.variants.admit(&target.variant).as_deref()
                        == Some(target.variant.as_str())
            })
        {
            return Err(ConfigError(
                "inventory targets are malformed or too many".to_owned(),
            ));
        }
        if self.max_snapshot_bytes == 0
            || self.max_snapshot_bytes > 64 * 1024 * 1024
            || self.max_media_bytes == 0
            || self.max_media_bytes > self.max_snapshot_bytes
            || self.artifact_quota_bytes == 0
            || self.max_preparation_seconds == 0
            || self.max_preparation_seconds > 3600
            || self.scan_per_minute == 0
            || self.scan_per_minute > 128
            || self.scan_interval_seconds < 60
            || self.scan_interval_seconds > 900
            || self
                .inventory
                .iter()
                .map(|target| (target.path.as_str(), target.variant.as_str()))
                .collect::<HashSet<_>>()
                .len()
                > self.store_bounds().max_inventory
        {
            return Err(ConfigError(
                "size, deadline, quota or reconciliation bounds are invalid".to_owned(),
            ));
        }
        Ok(())
    }

    /// The store bounds this configuration admits. The scheduler visits
    /// `scan_per_minute` targets once a minute, so an inventory larger than
    /// `scan_per_minute` times the whole minutes of `scan_interval_seconds`
    /// would miss its interval; the default store's inventory cap still
    /// applies. Configured, registered and persisted inventory share it.
    pub(crate) fn store_bounds(&self) -> Bounds {
        let defaults = Bounds::default();
        let minutes = usize::try_from(self.scan_interval_seconds / 60).unwrap_or(usize::MAX);
        Bounds {
            max_inventory: defaults
                .max_inventory
                .min(self.scan_per_minute.saturating_mul(minutes)),
            quota_bytes: self.artifact_quota_bytes,
            ..defaults
        }
    }

    pub(crate) fn damping_context(&self, worker_token: &str) -> String {
        let credential = credential_context(&self.api_base_url, &self.domain, worker_token);
        let selector = serde_json::json!({
            "deployment": self.deployment_id, "host": self.public_host, "edge": self.edge_url,
            "article_selector": self.article_selector, "min_text_length": self.min_text_length,
            "max_media_bytes": self.max_media_bytes, "max_large_media_bytes": self.max_large_media_bytes,
            "variants": self.variants,
        }).to_string();
        domain_digest(
            "encypher-preparation/1:worker-config",
            &[credential.as_bytes(), selector.as_bytes()],
        )
    }

    fn secrets(&self) -> Result<Secrets, ConfigError> {
        let read = |name: &str| {
            std::env::var(name)
                .ok()
                .filter(|value| {
                    !value.trim().is_empty()
                        && value.len() <= 4096
                        && !value.bytes().any(|byte| byte.is_ascii_control())
                })
                .ok_or_else(|| ConfigError(format!("environment variable {name} is not set")))
        };
        let secrets = Secrets {
            worker_token: read(&self.secrets.worker_token)?,
            signal: read(&self.secrets.signal_secret)?,
            publication: read(&self.secrets.publication_secret)?,
            operator: read(&self.secrets.operator_secret)?,
            canary: match &self.secrets.canary_token {
                Some(name) => Some(read(name)?),
                None => None,
            },
        };
        if !secrets.worker_token.starts_with("cwk_") {
            return Err(ConfigError(
                "worker_token must be a CDN worker credential".to_owned(),
            ));
        }
        let scopes = [
            &secrets.worker_token,
            &secrets.signal,
            &secrets.publication,
            &secrets.operator,
        ];
        if scopes
            .iter()
            .enumerate()
            .any(|(index, scope)| scopes[index + 1..].contains(scope))
        {
            return Err(ConfigError(
                "the worker credential and three scope secrets must differ".to_owned(),
            ));
        }
        Ok(secrets)
    }
}

fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(120))
        .build()
        .expect("should build an HTTP client")
}

/// The Encypher API with the worker credential.
#[derive(Clone)]
pub struct Api {
    client: reqwest::Client,
    base: String,
    token: String,
    canary: Option<String>,
}

impl Api {
    /// The API at `base`.
    #[must_use]
    pub fn new(base: &str, secrets: &Secrets) -> Self {
        Self {
            client: http_client(),
            base: base.trim_end_matches('/').to_owned(),
            token: secrets.worker_token.clone(),
            canary: secrets.canary.clone(),
        }
    }
}

impl ApiTransport for Api {
    async fn exchange(&self, request: ApiRequest) -> Result<ApiAnswer, Failure> {
        let mut builder = self.client.request(
            request.method,
            format!("{}{}", self.base, request.path_and_query),
        );
        if request.credentialed {
            builder = builder.header("x-encypher-worker-token", &self.token);
            if let Some(canary) = &self.canary
                && matches!(
                    request.path_and_query.as_str(),
                    "/api/v1/public/cdn/sign"
                        | "/api/v1/public/cdn/media/hash/prepare"
                        | "/api/v1/public/cdn/media/hash/finalize"
                )
            {
                builder = builder.header("x-encypher-canary-token", canary);
            }
        }
        if let Some(key) = &request.idempotency_key {
            builder = builder.header("idempotency-key", key);
        }
        if let Some(body) = request.body {
            builder = builder
                .header("content-type", "application/json")
                .body(body);
        }
        let response = builder
            .send()
            .await
            .map_err(|_| Failure::new(FailureClass::Transient, "http_error"))?;
        let status = response.status().as_u16();
        let body = bounded(response, request.max_response_bytes.saturating_add(1)).await?;
        Ok(ApiAnswer { status, body })
    }
}

async fn bounded(mut response: reqwest::Response, limit: usize) -> Result<Bytes, Failure> {
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| Failure::new(FailureClass::Transient, "http_error"))?
    {
        if body.len() + chunk.len() > limit {
            return Err(Failure::new(FailureClass::Transient, "response_too_large"));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(Bytes::from(body))
}

/// Verified probe artifacts on disk, by SHA-256, sharing the worker's hard quota.
pub struct DiskProbeCache(Arc<Store>);

impl DiskProbeCache {
    /// Uses the same durable owner and quota as source/output storage.
    #[must_use]
    pub fn new(store: Arc<Store>) -> Self {
        Self(store)
    }
}

impl ProbeArtifactCache for DiskProbeCache {
    async fn get(&self, sha256: &str) -> Option<Bytes> {
        self.0.blob("probe", sha256).ok().map(Bytes::from)
    }

    async fn put(&self, sha256: &str, artifact: Bytes) -> Result<(), Failure> {
        self.0
            .keep_blob("probe", sha256, &artifact)
            .map_err(|error| match error {
                StoreError::Quota => Failure::new(FailureClass::Transient, "quota_exhausted"),
                _ => Failure::new(FailureClass::Transient, "probe_storage"),
            })
    }
}

/// One snapshot answer: its metadata and bytes.
#[derive(Debug, Clone)]
pub struct Snapshot {
    /// What the delivery instance reported.
    pub meta: SnapshotMeta,
    /// The bytes read.
    pub bytes: Bytes,
}

/// The publisher's delivery instance, with the publication scope.
#[derive(Clone)]
pub struct Edge {
    client: reqwest::Client,
    base: String,
    secret: String,
    deployment_id: String,
}

impl Edge {
    /// The delivery instance at `base`.
    #[must_use]
    pub fn new(base: &str, secrets: &Secrets, deployment_id: &str) -> Self {
        Self {
            client: http_client(),
            base: base.trim_end_matches('/').to_owned(),
            secret: secrets.publication.clone(),
            deployment_id: deployment_id.to_owned(),
        }
    }

    /// Reads `target` through the reader pipeline.
    ///
    /// # Errors
    ///
    /// A [`Failure`] for a refused, private or unobserved snapshot.
    pub async fn snapshot(&self, target: &Target, read: SnapshotMode) -> Result<Snapshot, Failure> {
        self.snapshot_bounded(target, read, 64 * 1024 * 1024).await
    }

    /// Reads a snapshot within the already reserved acquisition ceiling.
    ///
    /// # Errors
    /// Refuses an oversized, unobserved or failed snapshot.
    pub async fn snapshot_bounded(
        &self,
        target: &Target,
        read: SnapshotMode,
        ceiling: usize,
    ) -> Result<Snapshot, Failure> {
        let nonce = format!("{:032x}", rand::random::<u128>());
        let request = SnapshotRequest {
            protocol: PREPARATION_PROTOCOL.to_owned(),
            deployment_id: self.deployment_id.clone(),
            target: target.clone(),
            nonce,
            read,
        };
        let response = self
            .client
            .post(format!("{}{SNAPSHOT_PATH}", self.base))
            .header(TOKEN_HEADER, &self.secret)
            .json(&request)
            .send()
            .await
            .map_err(|_| Failure::new(FailureClass::Transient, "edge_unavailable"))?;
        let status = response.status().as_u16();
        let meta = response
            .headers()
            .get(SNAPSHOT_META_HEADER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| STANDARD.decode(value).ok())
            .and_then(|bytes| serde_json::from_slice::<SnapshotMeta>(&bytes).ok());
        let limit = if status != 200 {
            64 * 1024
        } else {
            match &request.read {
                SnapshotMode::Whole => ceiling,
                SnapshotMode::Head => 0,
                SnapshotMode::Range { first, last, .. } => {
                    usize::try_from(last.saturating_sub(*first).saturating_add(1))
                        .unwrap_or(usize::MAX)
                        .min(ceiling)
                }
            }
        };
        let body = bounded(response, limit).await.map_err(|failure| {
            if failure.code == "response_too_large" {
                Failure::new(FailureClass::Unsupported, "snapshot_too_large")
            } else {
                failure
            }
        })?;
        if status != 200 {
            return Err(edge_failure(status, &body));
        }
        let meta =
            meta.ok_or_else(|| Failure::new(FailureClass::Integrity, "snapshot_meta_missing"))?;
        Ok(Snapshot { meta, bytes: body })
    }

    /// Publishes one result envelope. The delivery instance verifies it
    /// through the API before it stores anything.
    ///
    /// # Errors
    ///
    /// A [`Failure`] for a refusal.
    pub async fn publish(&self, envelope: &ResultEnvelope) -> Result<(), Failure> {
        self.put(&format!("{RESULTS_PREFIX}{}", envelope.identity), envelope)
            .await
    }

    /// Publishes one retirement marker.
    ///
    /// # Errors
    ///
    /// A [`Failure`] for a refusal.
    pub async fn retire(&self, marker: &MarkerEnvelope) -> Result<(), Failure> {
        self.put(&format!("{MARKERS_PREFIX}{}", marker.identity), marker)
            .await
    }

    async fn put(&self, path: &str, body: &impl serde::Serialize) -> Result<(), Failure> {
        let response = self
            .client
            .put(format!("{}{path}", self.base))
            .header(TOKEN_HEADER, &self.secret)
            .json(body)
            .send()
            .await
            .map_err(|_| Failure::new(FailureClass::Transient, "edge_unavailable"))?;
        let status = response.status().as_u16();
        let answer = bounded(response, 64 * 1024).await?;
        if status == 200 {
            Ok(())
        } else {
            Err(edge_failure(status, &answer))
        }
    }

    /// Whether the delivery instance accepts the publication secret, by a
    /// snapshot request it must refuse for its target, never for its
    /// credential.
    ///
    /// # Errors
    ///
    /// A [`Failure`] where it refuses the credential or cannot be reached.
    pub async fn check_authorization(&self) -> Result<(), Failure> {
        let probe = Target {
            // Always outside admission, even for a publisher whose routes contain "/".
            path: format!("/{}", "x".repeat(MAX_TARGET_PATH_BYTES)),
            variant: String::new(),
        };
        match self.snapshot(&probe, SnapshotMode::Head).await {
            Err(failure) if failure.code == "target_not_configured" => Ok(()),
            Err(failure) => Err(failure),
            Ok(_) => Ok(()),
        }
    }
}

/// The failure a delivery instance's refusal names.
fn edge_failure(status: u16, body: &[u8]) -> Failure {
    let code = serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|value| value.get("error")?.get("code")?.as_str().map(str::to_owned))
        .unwrap_or_else(|| format!("edge_status_{status}"));
    let class = match status {
        401 => FailureClass::Privacy,
        403 if code == "source_private" => FailureClass::Privacy,
        403 => FailureClass::Unsupported,
        409 if code == "retired" => FailureClass::Integrity,
        409 => FailureClass::PolicyChanged,
        410 => FailureClass::Missing,
        412 => FailureClass::Integrity,
        413 => FailureClass::Unsupported,
        422 if code.contains("provenance") => FailureClass::ExistingProvenance,
        422 => FailureClass::Integrity,
        _ => FailureClass::Transient,
    };
    Failure { code, class }
}
