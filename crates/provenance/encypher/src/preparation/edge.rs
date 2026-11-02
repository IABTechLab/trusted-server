//! The delivery instance's side of the preparation protocol: its internal
//! snapshot, result and marker routes, and the public source route.
//!
//! Every control route is authenticated with the publication scope's
//! secret, compared in constant time, and fixed to the one configured
//! deployment and its routes. None accepts a hostname, fetch headers, a
//! source credential or a signer.
//!
//! A snapshot is read through the same reader pipeline every reader takes:
//! the route asks the instance's own public address for the configured
//! target, carrying a short-lived capability for exactly that path. The
//! module's request preparer verifies and removes it before anything can
//! forward it, leaving an observation that changes nothing about how the
//! request is answered, so the same template, cache, origin and
//! transformations serve it. Only this module's own signing and lookup are
//! suppressed for it, and its answer reports what the origin's own headers
//! said, authenticated for the worker's nonce, apart from its private
//! transport headers.
//!
//! A result publication never trusts what the worker says it signed. The
//! route recomputes the identity from the item, recovers the exact
//! completed receipt from the authenticated API without signing, checks it
//! against the actual source bytes and, for embedded media, builds the
//! output itself and requires the worker's to be exactly it. Only then is
//! the record stored, under its immutable identity, and never over a
//! retirement marker.

use std::cell::RefCell;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use bytes::Bytes;
use edgezero_core::body::Body as EdgeBody;
use edgezero_core::error::EdgeError;
use error_stack::Report;
use http::{HeaderValue, Method, Request, Response, StatusCode, header};
use serde_json::json;
use sha2::Digest as _;

use trusted_server_core::error::TrustedServerError;
use trusted_server_core::integrations::{
    IntegrationEndpoint, IntegrationProxy, IntegrationRequestState,
    ensure_integration_backend_with_timeout,
};
use trusted_server_core::media_signing::SnapshotFacts;
use trusted_server_core::middleware::ResponseHeader;
use trusted_server_core::module_context::ModuleCall;
use trusted_server_core::platform::{KvError, PlatformHttpRequest, RuntimeServices};
use trusted_server_core::settings::Settings;

use super::pipeline::{
    self, ApiAnswer, ApiRequest, ApiTransport, Failure, FailureClass, ProbeArtifactCache,
    SourceReader,
};
use super::protocol::{
    CAPABILITY_HEADER, Carrier, EnvelopeItem, FACTS_HEADER, LargeLocator, MARKERS_PREFIX,
    MarkerEnvelope, MediaIdentity, PREPARATION_PROTOCOL, PUBLIC_SOURCE_PREFIX, PublishedRecord,
    PublishedResult, RESULTS_PREFIX, ResultEnvelope, RetirementMarker, SNAPSHOT_META_HEADER,
    SNAPSHOT_PATH, SnapshotMeta, SnapshotMode, SnapshotOrigin, SnapshotRequest, TOKEN_HEADER,
    Target, TextIdentity, WORKER_SOURCE_PREFIX, facts_header, is_digest, marker_key, result_key,
    snapshot_capability, token_matches, verify_facts_header, verify_snapshot_capability,
};
use super::{Execution, PreparationConfig};
use crate::media_api::{canonical_media_type, hex_lower, sha256_hex};
use crate::processor::Signer;
use crate::{EncypherConfig, INTEGRATION_ID, MODULE};

/// The largest result envelope: an embedded item's source and output,
/// base64, with its plan.
const MAX_ENVELOPE_BYTES: usize = 24 * 1024 * 1024;
/// The largest one range a snapshot reads.
const MAX_RANGE_BYTES: u64 = 8 * 1024 * 1024;
/// The aligned block a pinned source answers the probe's small structure
/// reads from.
const PINNED_BLOCK_BYTES: u64 = 64 * 1024;
/// The largest worker answer for a public source.
const MAX_PUBLIC_SOURCE_BYTES: usize = 8 * 1024 * 1024;
/// How long one loopback snapshot read may wait for its first byte.
const LOOPBACK_TIMEOUT: Duration = Duration::from_secs(30);
/// The cache key prefix of verified probe artifacts, the same the large
/// lane keeps them under.
const PROBE_KEY_PREFIX: &str = "encypher:probe:v1";

/// What the module's request preparer leaves on a verified snapshot read:
/// the nonce its facts are authenticated for. Left as an observation, so
/// the read takes every reader's path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SnapshotGrant {
    pub(crate) nonce: String,
}

fn unix_now() -> u64 {
    web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// The asynchronous mode's settings, where the deployment runs it.
fn async_config(settings: &Settings) -> Option<(EncypherConfig, PreparationConfig)> {
    let config = settings
        .module_config::<EncypherConfig>(MODULE)
        .ok()
        .flatten()?;
    if config.execution != Execution::AsyncPrepared {
        return None;
    }
    let preparation = config.preparation.clone()?;
    Some((config, preparation))
}

/// The module's request preparer: removes every snapshot capability from
/// every request, whether or not the module runs, so none ever reaches an
/// origin, and leaves a [`SnapshotGrant`] observation only for one that
/// verifies for exactly this path and query under the publication secret.
/// A client cannot set the grant: it exists only as request state this
/// preparer writes.
///
/// # Errors
///
/// Never; a capability that does not verify is dropped.
pub(crate) fn prepare_request(
    settings: &Settings,
    request: &mut Request<EdgeBody>,
) -> Result<(), Report<TrustedServerError>> {
    let value = {
        let mut values = request.headers().get_all(CAPABILITY_HEADER).iter();
        match (values.next(), values.next()) {
            (Some(value), None) => Some(value.clone()),
            _ => None,
        }
    };
    request.headers_mut().remove(CAPABILITY_HEADER);
    let Some(value) = value else {
        return Ok(());
    };
    let Ok(value) = value.to_str() else {
        return Ok(());
    };
    let Some((_, preparation)) = async_config(settings) else {
        return Ok(());
    };
    let prefix = request
        .extensions()
        .get::<trusted_server_core::addressing::RemovedPathPrefix>()
        .map(|prefix| prefix.as_str().to_owned())
        .unwrap_or_default();
    let path_and_query = match request.uri().query() {
        Some(query) => format!("{prefix}{}?{query}", request.uri().path()),
        None => format!("{prefix}{}", request.uri().path()),
    };
    if let Some(nonce) = verify_snapshot_capability(
        preparation.publication_secret.expose(),
        &path_and_query,
        value,
        unix_now(),
    ) {
        IntegrationRequestState::observe(request, INTEGRATION_ID, SnapshotGrant { nonce });
    }
    Ok(())
}

/// The facts header a snapshot answer carries, authenticated for its
/// grant's nonce.
pub(crate) fn snapshot_facts(
    preparation: &PreparationConfig,
    grant: &SnapshotGrant,
    origin: &SnapshotOrigin,
) -> Option<ResponseHeader> {
    let value = facts_header(
        preparation.publication_secret.expose(),
        &grant.nonce,
        origin,
    );
    Some((
        http::HeaderName::from_static(FACTS_HEADER),
        HeaderValue::from_str(&value).ok()?,
    ))
}

/// The origin facts core reports for a media snapshot, as the protocol
/// carries them.
pub(crate) fn media_snapshot_origin(facts: &SnapshotFacts) -> SnapshotOrigin {
    SnapshotOrigin {
        status: facts.status.as_u16(),
        private: facts.private,
        no_transform: facts.no_transform,
        manifest_link: facts.manifest_link,
        content_encoding: facts.content_encoding.clone(),
        origin_host: facts.origin_host.clone(),
        origin_identity_sha256: facts.origin_identity_sha256.clone(),
        source_reference: facts.source_reference.clone(),
    }
}

/// The Encypher API, as the delivery instance's platform client reaches
/// it, for receipt recovery and probe artifacts only.
pub(crate) struct EdgeApi<'a> {
    pub(crate) config: &'a EncypherConfig,
    pub(crate) services: &'a RuntimeServices,
}

impl ApiTransport for EdgeApi<'_> {
    async fn exchange(&self, request: ApiRequest) -> Result<ApiAnswer, Failure> {
        let backend = ensure_integration_backend_with_timeout(
            self.services,
            &self.config.api_base_url,
            INTEGRATION_ID,
            Duration::from_millis(self.config.timeout_ms),
        )
        .map_err(|_| Failure::new(FailureClass::Transient, "backend"))?;
        let mut builder = Request::builder().method(request.method).uri(format!(
            "{}{}",
            self.config.api_base_url.trim_end_matches('/'),
            request.path_and_query
        ));
        if request.credentialed {
            let mut token = HeaderValue::from_str(self.config.worker_token.expose().trim())
                .map_err(|_| Failure::new(FailureClass::Transient, "request_build"))?;
            token.set_sensitive(true);
            builder = builder.header("x-encypher-worker-token", token);
        }
        if let Some(key) = &request.idempotency_key {
            builder = builder.header("idempotency-key", key.as_str());
        }
        let body = match request.body {
            Some(body) => {
                builder = builder.header(header::CONTENT_TYPE, "application/json");
                EdgeBody::from(body)
            }
            None => EdgeBody::empty(),
        };
        let http_request = builder
            .body(body)
            .map_err(|_| Failure::new(FailureClass::Transient, "request_build"))?;
        let limit = request.max_response_bytes.saturating_add(1);
        let answer = self
            .services
            .http_client()
            .send(
                PlatformHttpRequest::new(http_request, backend)
                    .with_cache_bypass()
                    .with_max_response_body_bytes(limit),
            )
            .await
            .map_err(|_| Failure::new(FailureClass::Transient, "http_error"))?;
        let status = answer.response.status().as_u16();
        let body = answer
            .response
            .into_body()
            .into_bytes_bounded(limit)
            .await
            .map_err(|_| Failure::new(FailureClass::Transient, "response_too_large"))?;
        Ok(ApiAnswer { status, body })
    }
}

/// Verified probe artifacts kept in the runtime KV store.
pub(crate) struct KvProbeCache<'a>(pub(crate) &'a RuntimeServices);

impl ProbeArtifactCache for KvProbeCache<'_> {
    async fn get(&self, sha256: &str) -> Option<Bytes> {
        self.0
            .kv_store()
            .get_bytes(&format!("{PROBE_KEY_PREFIX}:{sha256}"))
            .await
            .ok()
            .flatten()
    }

    async fn put(&self, sha256: &str, artifact: Bytes) -> Result<(), Failure> {
        // The edge keeps artifacts only to avoid fetching them again; the
        // load goes on from the verified bytes in hand when the write fails.
        if self
            .0
            .kv_store()
            .put_bytes(&format!("{PROBE_KEY_PREFIX}:{sha256}"), artifact)
            .await
            .is_err()
        {
            log::debug!("encypher: probe release cache write failed");
        }
        Ok(())
    }
}

/// The module's preparation routes on the delivery instance.
pub(crate) struct PreparationRoutes {
    signer: std::sync::Arc<Signer>,
}

impl PreparationRoutes {
    /// The routes of `signer`, holding releases to the production key.
    pub(crate) fn new(signer: std::sync::Arc<Signer>) -> Self {
        Self { signer }
    }
}

impl std::fmt::Debug for PreparationRoutes {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreparationRoutes")
            .finish_non_exhaustive()
    }
}

fn json_response(status: StatusCode, value: &serde_json::Value) -> Response<EdgeBody> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CACHE_CONTROL, "private, no-store")
        .body(EdgeBody::from(value.to_string()))
        .expect("should build a JSON response")
}

fn refusal(status: StatusCode, code: &str) -> Response<EdgeBody> {
    json_response(status, &json!({"error": {"code": code}}))
}

fn failure_response(failure: &Failure) -> Response<EdgeBody> {
    let status = match failure.class {
        FailureClass::Transient => StatusCode::SERVICE_UNAVAILABLE,
        FailureClass::Missing => StatusCode::GONE,
        FailureClass::Privacy => StatusCode::FORBIDDEN,
        FailureClass::PolicyChanged => StatusCode::CONFLICT,
        FailureClass::Integrity | FailureClass::Unsupported | FailureClass::ExistingProvenance => {
            StatusCode::UNPROCESSABLE_ENTITY
        }
    };
    log::info!(
        "encypher: preparation publication refused code={}",
        failure.code
    );
    refusal(status, &failure.code)
}

async fn read_body(request: Request<EdgeBody>, limit: usize) -> Option<Bytes> {
    request
        .into_body()
        .into_bytes_bounded(limit.saturating_add(1))
        .await
        .ok()
        .filter(|body| body.len() <= limit)
}

impl PreparationRoutes {
    fn config(&self) -> Option<(&EncypherConfig, &PreparationConfig)> {
        let config = self.signer.config();
        Some((config, self.signer.preparation()?))
    }

    async fn route(
        &self,
        request: Request<EdgeBody>,
        services: &RuntimeServices,
    ) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
        let Some((config, preparation)) = self.config() else {
            return Ok(refusal(StatusCode::NOT_FOUND, "not_found"));
        };
        let path = request.uri().path().to_owned();
        if request.method() == Method::GET {
            if let Some(sha256) = path.strip_prefix(PUBLIC_SOURCE_PREFIX) {
                return Ok(self.public_source(services, preparation, sha256).await);
            }
            return Ok(refusal(StatusCode::NOT_FOUND, "not_found"));
        }
        let presented = request
            .headers()
            .get(TOKEN_HEADER)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        if !token_matches(preparation.publication_secret.expose(), presented) {
            return Ok(refusal(StatusCode::UNAUTHORIZED, "unauthorized"));
        }
        if request.method() == Method::POST && path == SNAPSHOT_PATH {
            return Ok(self.snapshot(services, config, preparation, request).await);
        }
        if request.method() == Method::PUT {
            if let Some(identity) = path.strip_prefix(RESULTS_PREFIX).map(str::to_owned) {
                return Ok(self
                    .publish(services, config, preparation, &identity, request)
                    .await);
            }
            if let Some(identity) = path.strip_prefix(MARKERS_PREFIX).map(str::to_owned) {
                return Ok(self.retire(services, preparation, &identity, request).await);
            }
        }
        Ok(refusal(StatusCode::NOT_FOUND, "not_found"))
    }

    /// Reads one configured target through the reader pipeline.
    async fn snapshot(
        &self,
        services: &RuntimeServices,
        config: &EncypherConfig,
        preparation: &PreparationConfig,
        request: Request<EdgeBody>,
    ) -> Response<EdgeBody> {
        let Some(body) = read_body(request, 4096).await else {
            return refusal(StatusCode::PAYLOAD_TOO_LARGE, "request_too_large");
        };
        let Ok(asked) = serde_json::from_slice::<SnapshotRequest>(&body) else {
            return refusal(StatusCode::BAD_REQUEST, "malformed_request");
        };
        if asked.protocol != PREPARATION_PROTOCOL
            || asked.deployment_id != preparation.deployment_id
        {
            return refusal(StatusCode::FORBIDDEN, "wrong_deployment");
        }
        if !asked.target.is_well_formed()
            || !preparation.admits_path(&asked.target.path)
            || preparation.variants.admit(&asked.target.variant).as_deref()
                != Some(asked.target.variant.as_str())
        {
            return refusal(StatusCode::FORBIDDEN, "target_not_configured");
        }
        if asked.nonce.len() != 32 || !asked.nonce.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return refusal(StatusCode::BAD_REQUEST, "nonce_invalid");
        }
        let (limit, range) = match &asked.read {
            SnapshotMode::Whole => (config.max_document_bytes.max(config.max_media_bytes), None),
            SnapshotMode::Head => (0, None),
            SnapshotMode::Range {
                first,
                last,
                if_match,
            } => {
                let Some(length) = last
                    .checked_sub(*first)
                    .and_then(|span| span.checked_add(1))
                else {
                    return refusal(StatusCode::BAD_REQUEST, "range_invalid");
                };
                if length > MAX_RANGE_BYTES {
                    return refusal(StatusCode::BAD_REQUEST, "range_too_large");
                }
                (
                    usize::try_from(length).unwrap_or(usize::MAX),
                    Some((*first, *last, if_match.clone())),
                )
            }
        };
        let path_and_query = asked.target.path_and_query();
        let capability = snapshot_capability(
            preparation.publication_secret.expose(),
            &path_and_query,
            &asked.nonce,
            unix_now() + super::protocol::CAPABILITY_LIFETIME_SECONDS,
        );
        let answer = match loopback_read(
            services,
            preparation,
            &path_and_query,
            &capability,
            range.as_ref(),
            limit,
        )
        .await
        {
            Ok(answer) => answer,
            Err(code) => return refusal(StatusCode::BAD_GATEWAY, code),
        };
        let (parts, body) = answer.into_parts();
        let Some(origin) = parts
            .headers
            .get(FACTS_HEADER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| {
                verify_facts_header(preparation.publication_secret.expose(), &asked.nonce, value)
            })
        else {
            // The module did not observe the read: the target is not one a
            // [[serve]] entry names it for, or the answer is not this read's.
            return refusal(StatusCode::UNPROCESSABLE_ENTITY, "snapshot_unobserved");
        };
        if origin.private {
            return refusal(StatusCode::FORBIDDEN, "source_private");
        }
        if origin.manifest_link {
            return refusal(
                StatusCode::UNPROCESSABLE_ENTITY,
                "source_has_provenance_link",
            );
        }
        if origin.no_transform {
            return refusal(StatusCode::UNPROCESSABLE_ENTITY, "no_transform");
        }
        if !origin.content_encoding.is_empty() {
            return refusal(StatusCode::UNPROCESSABLE_ENTITY, "content_encoded");
        }
        let expected_status = if range.is_some() {
            StatusCode::PARTIAL_CONTENT
        } else {
            StatusCode::OK
        };
        if parts.status != expected_status && !(range.is_some() && parts.status == StatusCode::OK) {
            return refusal(StatusCode::UNPROCESSABLE_ENTITY, "source_status");
        }
        let etag = single_strong_etag(&parts.headers);
        if let Some((_, _, if_match)) = &range
            && etag.as_deref() != Some(if_match.as_str())
        {
            return refusal(StatusCode::PRECONDITION_FAILED, "source_version_changed");
        }
        let length = if let Some((first, last, _)) = &range {
            let Some(total) = pinned_range_length(parts.status, &parts.headers, *first, *last)
            else {
                return refusal(StatusCode::UNPROCESSABLE_ENTITY, "source_range_invalid");
            };
            Some(total)
        } else {
            parts
                .headers
                .get(header::CONTENT_LENGTH)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.trim().parse().ok())
        };
        let meta = SnapshotMeta {
            protocol: PREPARATION_PROTOCOL.to_owned(),
            page_url: format!("https://{}{}", self.signer.public_host(), asked.target.path),
            content_type: parts
                .headers
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_owned(),
            etag,
            length,
            origin,
        };
        // The read's headers arrive before its body, so nothing above has
        // read a byte of it. A head snapshot drops the body unread; a whole
        // one declared past the bound is refused unread; every other body is
        // read only up to the bound. A failed read stays a failed read and is
        // never reported as an oversized source.
        let bytes = if limit == 0 {
            Bytes::new()
        } else if range.is_none()
            && length.is_some_and(|length| length > u64::try_from(limit).unwrap_or(u64::MAX))
        {
            return refusal(StatusCode::PAYLOAD_TOO_LARGE, "snapshot_too_large");
        } else {
            match body.into_bytes_bounded(limit).await {
                Ok(bytes) if bytes.len() <= limit => bytes,
                Ok(_) | Err(EdgeError::BadRequest { .. }) => {
                    return refusal(StatusCode::PAYLOAD_TOO_LARGE, "snapshot_too_large");
                }
                Err(_) => return refusal(StatusCode::BAD_GATEWAY, "loopback_failed"),
            }
        };
        if let Some((first, last, _)) = &range
            && u64::try_from(bytes.len()).ok() != Some(last - first + 1)
        {
            return refusal(StatusCode::BAD_GATEWAY, "range_short");
        }
        let encoded = STANDARD.encode(serde_json::to_vec(&meta).unwrap_or_default());
        Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/octet-stream")
            .header(header::CACHE_CONTROL, "private, no-store")
            .header(SNAPSHOT_META_HEADER, encoded)
            .body(EdgeBody::Once(bytes))
            .expect("should build a snapshot response")
    }

    /// Verifies one result through the authenticated API and stores it
    /// under its immutable identity.
    async fn publish(
        &self,
        services: &RuntimeServices,
        config: &EncypherConfig,
        preparation: &PreparationConfig,
        identity: &str,
        request: Request<EdgeBody>,
    ) -> Response<EdgeBody> {
        if !is_digest(identity) {
            return refusal(StatusCode::BAD_REQUEST, "identity_invalid");
        }
        let Some(body) = read_body(request, MAX_ENVELOPE_BYTES).await else {
            return refusal(StatusCode::PAYLOAD_TOO_LARGE, "result_too_large");
        };
        let Ok(envelope) = serde_json::from_slice::<ResultEnvelope>(&body) else {
            return refusal(StatusCode::BAD_REQUEST, "malformed_request");
        };
        if envelope.protocol != PREPARATION_PROTOCOL
            || envelope.deployment_id != preparation.deployment_id
            || envelope.identity != identity
        {
            return refusal(StatusCode::FORBIDDEN, "wrong_deployment");
        }
        if preparation.variants.admit(&envelope.variant).as_deref()
            != Some(envelope.variant.as_str())
        {
            return refusal(StatusCode::FORBIDDEN, "variant_not_admitted");
        }
        let kv = services.kv_store();
        match kv.get_bytes(&marker_key(identity)).await {
            Ok(None) => {}
            Ok(Some(_)) => return refusal(StatusCode::CONFLICT, "retired"),
            Err(_) => return refusal(StatusCode::SERVICE_UNAVAILABLE, "marker_unavailable"),
        }
        let result = match self
            .verify_envelope(services, config, preparation, &envelope)
            .await
        {
            Ok(result) => result,
            Err(failure) => return failure_response(&failure),
        };
        // A marker written while the receipt was being checked still wins.
        // Readers refuse a retired identity whatever its result says, so
        // this only keeps a known-retired result from being written at all.
        match kv.get_bytes(&marker_key(identity)).await {
            Ok(None) => {}
            Ok(Some(_)) => return refusal(StatusCode::CONFLICT, "retired"),
            Err(_) => return refusal(StatusCode::SERVICE_UNAVAILABLE, "marker_unavailable"),
        }
        let record = PublishedRecord {
            protocol: PREPARATION_PROTOCOL.to_owned(),
            identity: identity.to_owned(),
            retirement_epoch: envelope.retirement_epoch,
            result,
        };
        let Ok(encoded) = serde_json::to_vec(&record) else {
            return refusal(StatusCode::INTERNAL_SERVER_ERROR, "encode");
        };
        match kv
            .put_bytes(&result_key(identity), Bytes::from(encoded))
            .await
        {
            Ok(()) => {
                log::info!("encypher: preparation result published");
                json_response(
                    StatusCode::OK,
                    &json!({"protocol": PREPARATION_PROTOCOL, "published": identity}),
                )
            }
            Err(KvError::Unavailable) => refusal(StatusCode::SERVICE_UNAVAILABLE, "kv_unavailable"),
            Err(_) => refusal(StatusCode::SERVICE_UNAVAILABLE, "kv_error"),
        }
    }

    /// The record an envelope supports, recomputing its identity and
    /// checking the API's own receipt.
    async fn verify_envelope(
        &self,
        services: &RuntimeServices,
        config: &EncypherConfig,
        preparation: &PreparationConfig,
        envelope: &ResultEnvelope,
    ) -> Result<PublishedResult, Failure> {
        let api = EdgeApi { config, services };
        let context = self.signer.credential_context();
        let refused = |code: &str| Failure::new(FailureClass::Integrity, code);
        match &envelope.item {
            EnvelopeItem::Text {
                request,
                idempotency_key,
            } => {
                self.require_page_url(preparation, &request.page_url)?;
                let identity = TextIdentity {
                    credential_context: context,
                    policy_digest: &request.expected_policy_digest,
                    page_url: &request.page_url,
                    variant: &envelope.variant,
                    text: &request.text,
                    document_title: request.document_title.as_deref(),
                    boundary_selector: &request.boundary_selector,
                }
                .digest();
                if identity != envelope.identity {
                    return Err(refused("identity_mismatch"));
                }
                let signed = pipeline::recover_text(&api, request, idempotency_key).await?;
                Ok(PublishedResult::Text {
                    plan: signed.plan,
                    manifest_url: signed.manifest_url,
                    scalars: request.text.chars().count(),
                })
            }
            EnvelopeItem::Media {
                planned,
                source_b64,
            } => {
                let request = &planned.request;
                self.require_page_url(preparation, &request.page_url)?;
                if request.carrier() != Carrier::External {
                    return Err(refused("carrier_mismatch"));
                }
                let identity = MediaIdentity {
                    credential_context: context,
                    policy_digest: &request.expected_policy_digest,
                    page_url: &request.page_url,
                    variant: &envelope.variant,
                    carrier: Carrier::External,
                    mime: &canonical_media_type(&request.source.mime),
                    source_sha256: &request.source.sha256,
                }
                .digest();
                if identity != envelope.identity {
                    return Err(refused("identity_mismatch"));
                }
                let source = STANDARD
                    .decode(source_b64)
                    .map_err(|_| refused("source_invalid"))?;
                if sha256_hex(&source) != request.source.sha256
                    || u64::try_from(source.len()).ok() != Some(request.source.byte_length)
                {
                    return Err(refused("source_digest_mismatch"));
                }
                let recovered =
                    pipeline::recover_media(&api, request, &planned.idempotency_key).await?;
                let probe =
                    pipeline::load_release(&api, &KvProbeCache(services), &planned.release).await?;
                // Provenance the source carries is kept, whatever the
                // worker's disclosure left out.
                pipeline::inspect_external_source(&probe, request, source.as_slice()).await?;
                pipeline::verify_external_receipt(&probe, planned, &recovered).await
            }
            EnvelopeItem::Embedded {
                planned,
                source_b64,
                output_b64,
            } => {
                let request = &planned.request;
                self.require_page_url(preparation, &request.page_url)?;
                if request.carrier() != Carrier::Embedded {
                    return Err(refused("carrier_mismatch"));
                }
                let identity = MediaIdentity {
                    credential_context: context,
                    policy_digest: &request.expected_policy_digest,
                    page_url: &request.page_url,
                    variant: &envelope.variant,
                    carrier: Carrier::Embedded,
                    mime: &request.source.mime,
                    source_sha256: &request.source.sha256,
                }
                .digest();
                if identity != envelope.identity {
                    return Err(refused("identity_mismatch"));
                }
                let source = STANDARD
                    .decode(source_b64)
                    .map_err(|_| refused("source_invalid"))?;
                let output = STANDARD
                    .decode(output_b64)
                    .map_err(|_| refused("output_invalid"))?;
                if source.len() > config.max_media_bytes
                    || output.len() > config.max_media_bytes.saturating_mul(2)
                {
                    return Err(refused("output_too_large"));
                }
                // The bytes the worker sent are the source only where they
                // are exactly the ones the identity binds; checked before the
                // API is asked anything.
                if sha256_hex(&source) != request.source.sha256
                    || u64::try_from(source.len()).ok() != Some(request.source.byte_length)
                {
                    return Err(refused("source_digest_mismatch"));
                }
                let recovered =
                    pipeline::recover_media(&api, request, &planned.idempotency_key).await?;
                let probe =
                    pipeline::load_release(&api, &KvProbeCache(services), &planned.release).await?;
                pipeline::verify_embedded_receipt(&probe, planned, &recovered, &source, &output)
                    .await
            }
            EnvelopeItem::Large {
                planned,
                etag,
                origin_identity_sha256,
            } => {
                let request = &planned.request;
                self.require_page_url(preparation, &request.page_url)?;
                // The worker's digest is only what it says it read; every
                // pinned read below must report the same configured source
                // in its own authenticated facts.
                if !is_digest(origin_identity_sha256) {
                    return Err(refused("origin_identity_invalid"));
                }
                let identity = LargeLocator {
                    credential_context: context,
                    policy_digest: &request.expected_policy_digest,
                    reader_url: &request.page_url,
                    variant: &envelope.variant,
                    origin_host: &request.source.origin_host,
                    origin_identity_sha256,
                    mime: &request.source.mime,
                    etag,
                    length: request.source.byte_length,
                }
                .digest();
                if identity != envelope.identity {
                    return Err(refused("identity_mismatch"));
                }
                let path = url::Url::parse(&request.page_url)
                    .map(|url| url.path().to_owned())
                    .map_err(|_| refused("page_url_invalid"))?;
                let target = Target {
                    path,
                    variant: envelope.variant.clone(),
                };
                if !target.is_well_formed()
                    || !preparation.admits_path(&target.path)
                    || preparation.variants.admit(&target.variant).as_deref()
                        != Some(target.variant.as_str())
                {
                    return Err(refused("target_invalid"));
                }
                if request.source.byte_length > config.max_large_media_bytes {
                    return Err(Failure::new(FailureClass::Unsupported, "media_too_large"));
                }
                let path_and_query = target.path_and_query();
                let version = PinnedVersion::new(
                    services,
                    preparation,
                    request,
                    &path_and_query,
                    etag,
                    origin_identity_sha256,
                )?;
                // Provenance the pinned version carries is kept, whatever the
                // worker's disclosure left out. It is inspected first, through
                // the same pinned reads, so such a source costs no full read.
                let probe =
                    pipeline::load_release(&api, &KvProbeCache(services), &planned.release).await?;
                pipeline::inspect_external_source(&probe, request, &PinnedSource::new(&version))
                    .await?;
                // The worker's digest is never taken on trust: every byte of
                // the version this tag names is read again here, in pinned
                // ranges through the reader pipeline, and hashed. A changed
                // version or a different digest refuses the result. This is
                // the control path; readers never hash.
                let digest = version.digest().await?;
                if digest != request.source.sha256 || planned.digest != request.source.sha256 {
                    return Err(refused("source_digest_mismatch"));
                }
                let recovered =
                    pipeline::recover_media(&api, request, &planned.idempotency_key).await?;
                match pipeline::verify_external_receipt(&probe, planned, &recovered).await? {
                    PublishedResult::External {
                        source_sha256,
                        headers,
                    } => Ok(PublishedResult::Large {
                        sha256: source_sha256,
                        length: request.source.byte_length,
                        headers,
                    }),
                    _ => Err(refused("receipt_invalid")),
                }
            }
        }
    }

    /// Refuses a page address outside this deployment's public host and
    /// configured routes.
    fn require_page_url(
        &self,
        preparation: &PreparationConfig,
        page_url: &str,
    ) -> Result<(), Failure> {
        let refused = || Failure::new(FailureClass::Integrity, "page_url_outside_deployment");
        let parsed = url::Url::parse(page_url).map_err(|_| refused())?;
        let host = parsed.host_str().ok_or_else(refused)?;
        if parsed.scheme() != "https"
            || !self.signer.claims_host(host)
            || parsed.query().is_some()
            || parsed.fragment().is_some()
            || !preparation.admits_path(parsed.path())
        {
            return Err(refused());
        }
        Ok(())
    }

    /// Writes an identity's retirement marker and drops its result. The
    /// marker is a key of its own, which no result publication touches.
    async fn retire(
        &self,
        services: &RuntimeServices,
        preparation: &PreparationConfig,
        identity: &str,
        request: Request<EdgeBody>,
    ) -> Response<EdgeBody> {
        if !is_digest(identity) {
            return refusal(StatusCode::BAD_REQUEST, "identity_invalid");
        }
        let Some(body) = read_body(request, 4096).await else {
            return refusal(StatusCode::PAYLOAD_TOO_LARGE, "request_too_large");
        };
        let Ok(envelope) = serde_json::from_slice::<MarkerEnvelope>(&body) else {
            return refusal(StatusCode::BAD_REQUEST, "malformed_request");
        };
        if envelope.protocol != PREPARATION_PROTOCOL
            || envelope.deployment_id != preparation.deployment_id
            || envelope.identity != identity
        {
            return refusal(StatusCode::FORBIDDEN, "wrong_deployment");
        }
        let kv = services.kv_store();
        let existing = match kv.get_bytes(&marker_key(identity)).await {
            Ok(existing) => existing
                .and_then(|bytes| serde_json::from_slice::<RetirementMarker>(&bytes).ok())
                .map_or(0, |marker| marker.epoch),
            Err(_) => return refusal(StatusCode::SERVICE_UNAVAILABLE, "marker_unavailable"),
        };
        let marker = RetirementMarker {
            epoch: existing.max(envelope.epoch),
        };
        let Ok(encoded) = serde_json::to_vec(&marker) else {
            return refusal(StatusCode::INTERNAL_SERVER_ERROR, "encode");
        };
        if kv
            .put_bytes(&marker_key(identity), Bytes::from(encoded))
            .await
            .is_err()
        {
            return refusal(StatusCode::SERVICE_UNAVAILABLE, "marker_write_failed");
        }
        if kv.delete(&result_key(identity)).await.is_err() {
            log::debug!("encypher: retired result could not be deleted; the marker refuses it");
        }
        json_response(
            StatusCode::OK,
            &json!({"protocol": PREPARATION_PROTOCOL, "acknowledged": identity, "epoch": marker.epoch}),
        )
    }

    /// Serves one explicitly public source snapshot by its digest, from the
    /// worker, which holds it only after page and asset admission. Exact
    /// stored bytes, never rendered or sniffed by a browser.
    async fn public_source(
        &self,
        services: &RuntimeServices,
        preparation: &PreparationConfig,
        sha256: &str,
    ) -> Response<EdgeBody> {
        if !is_digest(sha256) {
            return refusal(StatusCode::NOT_FOUND, "not_found");
        }
        let Ok(backend) = ensure_integration_backend_with_timeout(
            services,
            &preparation.worker_url,
            INTEGRATION_ID,
            LOOPBACK_TIMEOUT,
        ) else {
            return refusal(StatusCode::SERVICE_UNAVAILABLE, "worker_unavailable");
        };
        let Ok(mut token) = HeaderValue::from_str(preparation.publication_secret.expose().trim())
        else {
            return refusal(StatusCode::SERVICE_UNAVAILABLE, "worker_unavailable");
        };
        token.set_sensitive(true);
        let Ok(request) = Request::builder()
            .method(Method::GET)
            .uri(format!(
                "{}{WORKER_SOURCE_PREFIX}{sha256}",
                preparation.worker_url.trim_end_matches('/')
            ))
            .header(TOKEN_HEADER, token)
            .body(EdgeBody::empty())
        else {
            return refusal(StatusCode::SERVICE_UNAVAILABLE, "worker_unavailable");
        };
        let limit = MAX_PUBLIC_SOURCE_BYTES.saturating_add(1);
        let Ok(answer) = services
            .http_client()
            .send(
                PlatformHttpRequest::new(request, backend)
                    .with_cache_bypass()
                    .with_max_response_body_bytes(limit),
            )
            .await
        else {
            return refusal(StatusCode::SERVICE_UNAVAILABLE, "worker_unavailable");
        };
        if answer.response.status() != StatusCode::OK {
            return refusal(StatusCode::NOT_FOUND, "not_found");
        }
        let content_type = answer
            .response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(canonical_media_type)
            .filter(|media_type| crate::data_uri::is_media_type(media_type));
        let Some(content_type) = content_type else {
            return refusal(StatusCode::NOT_FOUND, "not_found");
        };
        let Ok(bytes) = answer.response.into_body().into_bytes_bounded(limit).await else {
            return refusal(StatusCode::NOT_FOUND, "not_found");
        };
        // The worker answers only an admitted digest; the bytes must still
        // be exactly the ones the address names.
        if bytes.len() > MAX_PUBLIC_SOURCE_BYTES || sha256_hex(&bytes) != sha256 {
            return refusal(StatusCode::NOT_FOUND, "not_found");
        }
        Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, content_type)
            .header("x-content-type-options", "nosniff")
            .header(
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{sha256}\""),
            )
            .header(
                "content-security-policy",
                "sandbox; default-src 'none'; base-uri 'none'; form-action 'none'",
            )
            .header(header::CACHE_CONTROL, "public, max-age=300")
            .body(EdgeBody::Once(bytes))
            .expect("should build a public source response")
    }
}

/// One loopback read of `path_and_query` through the instance's own
/// public address, carrying the capability and, for a range, `If-Match`.
///
/// A client that streams answers with the head before any body byte, so
/// the caller judges the authenticated facts first and reads at most
/// `limit` bytes; no transport bound applies, which would refuse a large
/// declared length before its head arrived. A buffered client holds at most
/// `limit` bytes: a longer answer fails the read rather than being held.
async fn loopback_read(
    services: &RuntimeServices,
    preparation: &PreparationConfig,
    path_and_query: &str,
    capability: &str,
    range: Option<&(u64, u64, String)>,
    limit: usize,
) -> Result<Response<EdgeBody>, &'static str> {
    let backend = ensure_integration_backend_with_timeout(
        services,
        &preparation.loopback_url,
        INTEGRATION_ID,
        LOOPBACK_TIMEOUT,
    )
    .map_err(|_| "loopback_backend")?;
    let mut builder = Request::builder()
        .method(Method::GET)
        .uri(format!(
            "{}{path_and_query}",
            preparation.loopback_url.trim_end_matches('/')
        ))
        .header(header::ACCEPT_ENCODING, "identity")
        .header(CAPABILITY_HEADER, capability);
    if let Some((first, last, if_match)) = range {
        builder = builder
            .header(header::RANGE, format!("bytes={first}-{last}"))
            .header(header::IF_MATCH, if_match.as_str());
    }
    let request = builder
        .body(EdgeBody::empty())
        .map_err(|_| "request_build")?;
    let client = services.http_client();
    let read = PlatformHttpRequest::new(request, backend).with_cache_bypass();
    let read = if client.supports_streaming_responses() {
        read.with_stream_response()
    } else {
        read.with_max_response_body_bytes(limit)
    };
    let answer = client.send(read).await.map_err(|_| "loopback_failed")?;
    Ok(answer.response)
}

/// Refuses a pinned read whose origin facts do not admit it as the source
/// `request` names: one reader's answer, an encoded body, provenance the
/// origin already carries, a body the origin forbade transforming, another
/// origin host, another configured source than `origin_identity_sha256`
/// or another media type. A read the module did not observe reports no
/// facts and is refused as well.
fn admit_pinned_read(
    facts: Option<SnapshotOrigin>,
    content_type: Option<&str>,
    request: &pipeline::PrepareRequest,
    origin_identity_sha256: &str,
) -> Result<(), Failure> {
    let Some(origin) = facts else {
        return Err(Failure::new(FailureClass::Integrity, "snapshot_unobserved"));
    };
    if origin.private {
        return Err(Failure::new(FailureClass::Privacy, "source_private"));
    }
    if origin.manifest_link {
        return Err(Failure::new(
            FailureClass::ExistingProvenance,
            "source_has_provenance_link",
        ));
    }
    if origin.no_transform {
        return Err(Failure::new(FailureClass::Unsupported, "no_transform"));
    }
    if !origin.content_encoding.is_empty() {
        return Err(Failure::new(FailureClass::Unsupported, "content_encoded"));
    }
    if origin.origin_host != request.source.origin_host {
        return Err(Failure::new(
            FailureClass::Integrity,
            "source_origin_mismatch",
        ));
    }
    // The same host can serve another configured source, such as a remapped
    // folder, under equal per-resource validators.
    if !is_digest(&origin.origin_identity_sha256)
        || origin.origin_identity_sha256 != origin_identity_sha256
    {
        return Err(Failure::new(
            FailureClass::Integrity,
            "source_identity_mismatch",
        ));
    }
    if content_type.map(canonical_media_type).as_deref() != Some(request.source.mime.as_str()) {
        return Err(Failure::new(
            FailureClass::Integrity,
            "source_type_mismatch",
        ));
    }
    Ok(())
}

/// The version `etag` names at `path`, read in pinned ranges through the
/// reader pipeline. Every answer must be exactly its range of exactly that
/// version and length, observed by the module as a snapshot read, and
/// admitted by its origin facts as `request`'s source from the configured
/// source `origin_identity_sha256` names.
struct PinnedVersion<'a> {
    services: &'a RuntimeServices,
    preparation: &'a PreparationConfig,
    request: &'a pipeline::PrepareRequest,
    path: &'a str,
    etag: &'a str,
    origin_identity_sha256: &'a str,
}

impl<'a> PinnedVersion<'a> {
    /// The version of `request`'s source, refused where it names no bytes.
    fn new(
        services: &'a RuntimeServices,
        preparation: &'a PreparationConfig,
        request: &'a pipeline::PrepareRequest,
        path: &'a str,
        etag: &'a str,
        origin_identity_sha256: &'a str,
    ) -> Result<Self, Failure> {
        if request.source.byte_length == 0 {
            return Err(Failure::new(FailureClass::Integrity, "source_empty"));
        }
        Ok(Self {
            services,
            preparation,
            request,
            path,
            etag,
            origin_identity_sha256,
        })
    }

    fn length(&self) -> u64 {
        self.request.source.byte_length
    }

    /// The bytes `first..=last`, at most [`MAX_RANGE_BYTES`] of them.
    async fn range(&self, first: u64, last: u64) -> Result<Bytes, Failure> {
        let length = self.length();
        if first > last || last >= length || last - first >= MAX_RANGE_BYTES {
            return Err(Failure::new(FailureClass::Integrity, "read_out_of_bounds"));
        }
        let (path, etag) = (self.path, self.etag);
        let secret = self.preparation.publication_secret.expose();
        let nonce =
            sha256_hex(format!("{path}\n{etag}\n{first}\n{last}\n{}", unix_now()).as_bytes());
        let nonce = &nonce[..32];
        let capability = snapshot_capability(
            secret,
            path,
            nonce,
            unix_now() + super::protocol::CAPABILITY_LIFETIME_SECONDS,
        );
        let range = (first, last, etag.to_owned());
        let expected = usize::try_from(last - first + 1).unwrap_or(usize::MAX);
        let answer = loopback_read(
            self.services,
            self.preparation,
            path,
            &capability,
            Some(&range),
            expected,
        )
        .await
        .map_err(|code| Failure::new(FailureClass::Transient, code))?;
        let facts = answer
            .headers()
            .get(FACTS_HEADER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| verify_facts_header(secret, nonce, value));
        admit_pinned_read(
            facts,
            answer
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            self.request,
            self.origin_identity_sha256,
        )?;
        if single_strong_etag(answer.headers()).as_deref() != Some(etag) {
            return Err(Failure::new(
                FailureClass::Integrity,
                "source_version_changed",
            ));
        }
        if pinned_range_length(answer.status(), answer.headers(), first, last) != Some(length) {
            return Err(Failure::new(
                FailureClass::Integrity,
                "source_range_invalid",
            ));
        }
        // Read only once the facts, version and range admit the answer, and
        // never past the range: a longer body is refused as soon as it is, a
        // failed read stays a transient failure.
        let bytes = match answer.into_body().into_bytes_bounded(expected).await {
            Ok(bytes) => bytes,
            Err(EdgeError::BadRequest { .. }) => {
                return Err(Failure::new(
                    FailureClass::Integrity,
                    "source_range_invalid",
                ));
            }
            Err(_) => return Err(Failure::new(FailureClass::Transient, "loopback_failed")),
        };
        if bytes.len() != expected {
            return Err(Failure::new(
                FailureClass::Integrity,
                "source_range_invalid",
            ));
        }
        Ok(bytes)
    }

    /// SHA-256 of every byte of the version, read in ranges of at most
    /// [`MAX_RANGE_BYTES`].
    async fn digest(&self) -> Result<String, Failure> {
        let length = self.length();
        let mut hasher = sha2::Sha256::new();
        let mut first = 0_u64;
        while first < length {
            let last = (first + MAX_RANGE_BYTES).min(length) - 1;
            hasher.update(&self.range(first, last).await?);
            first = last + 1;
        }
        Ok(hex_lower(&hasher.finalize()))
    }
}

/// A [`PinnedVersion`] as the probe reads it, at offsets. A read inside one
/// aligned block of [`PINNED_BLOCK_BYTES`] is answered from that block,
/// read once and kept until a read needs another, so a structure walk's
/// many small reads cost one range read per block rather than one each.
/// Any other read is read exactly, in ranges of at most [`MAX_RANGE_BYTES`].
/// The probe's own read limits bound what it may ask for in all.
struct PinnedSource<'a> {
    version: &'a PinnedVersion<'a>,
    block: RefCell<Option<(u64, Bytes)>>,
}

impl<'a> PinnedSource<'a> {
    fn new(version: &'a PinnedVersion<'a>) -> Self {
        Self {
            version,
            block: RefCell::new(None),
        }
    }

    /// The aligned block starting at `start`, read once.
    async fn block(&self, start: u64) -> Result<Bytes, Failure> {
        let kept = self
            .block
            .borrow()
            .as_ref()
            .filter(|(kept_start, _)| *kept_start == start)
            .map(|(_, bytes)| bytes.clone());
        if let Some(bytes) = kept {
            return Ok(bytes);
        }
        let last = (start + PINNED_BLOCK_BYTES).min(self.version.length()) - 1;
        let bytes = self.version.range(start, last).await?;
        *self.block.borrow_mut() = Some((start, bytes.clone()));
        Ok(bytes)
    }
}

impl SourceReader for PinnedSource<'_> {
    fn size(&self) -> u64 {
        self.version.length()
    }

    async fn read_exact_at(&self, offset: u64, destination: &mut [u8]) -> Result<(), Failure> {
        let out_of_bounds = || Failure::new(FailureClass::Integrity, "read_out_of_bounds");
        let wanted = u64::try_from(destination.len()).map_err(|_| out_of_bounds())?;
        let end = offset
            .checked_add(wanted)
            .filter(|end| *end <= self.size())
            .ok_or_else(out_of_bounds)?;
        if wanted == 0 {
            return Ok(());
        }
        let start = offset - offset % PINNED_BLOCK_BYTES;
        if end <= start + PINNED_BLOCK_BYTES {
            let block = self.block(start).await?;
            let at = usize::try_from(offset - start).map_err(|_| out_of_bounds())?;
            destination.copy_from_slice(&block[at..at + destination.len()]);
            return Ok(());
        }
        let mut first = offset;
        let part_length = usize::try_from(MAX_RANGE_BYTES).unwrap_or(usize::MAX);
        for part in destination.chunks_mut(part_length) {
            let last = first + u64::try_from(part.len()).map_err(|_| out_of_bounds())? - 1;
            part.copy_from_slice(&self.version.range(first, last).await?);
            first = last + 1;
        }
        Ok(())
    }
}

/// The single strong entity tag of an answer, where it has exactly one.
fn single_strong_etag(headers: &http::HeaderMap) -> Option<String> {
    let mut tags = headers.get_all(header::ETAG).iter();
    let (Some(tag), None) = (tags.next(), tags.next()) else {
        return None;
    };
    let tag = tag.to_str().ok()?.trim();
    (tag.starts_with('"') && tag.ends_with('"') && tag.len() >= 3).then(|| tag.to_owned())
}

/// Accepts one exact partial range, or an origin's exact whole-body fallback.
fn pinned_range_length(
    status: StatusCode,
    headers: &http::HeaderMap,
    first: u64,
    last: u64,
) -> Option<u64> {
    let decimal = |value: &str| -> Option<u64> {
        if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        value.parse().ok()
    };
    if status == StatusCode::PARTIAL_CONTENT {
        let mut values = headers.get_all(header::CONTENT_RANGE).iter();
        let (Some(value), None) = (values.next(), values.next()) else {
            return None;
        };
        let value = value.to_str().ok()?.trim().strip_prefix("bytes ")?;
        let (span, total) = value.split_once('/')?;
        let (start, end) = span.split_once('-')?;
        let total = decimal(total)?;
        (decimal(start)? == first && decimal(end)? == last && first <= last && last < total)
            .then_some(total)
    } else if status == StatusCode::OK && first == 0 && !headers.contains_key(header::CONTENT_RANGE)
    {
        let mut values = headers.get_all(header::CONTENT_LENGTH).iter();
        let (Some(value), None) = (values.next(), values.next()) else {
            return None;
        };
        let total = decimal(value.to_str().ok()?.trim())?;
        (last.checked_add(1) == Some(total)).then_some(total)
    } else {
        None
    }
}

#[async_trait(?Send)]
impl IntegrationProxy for PreparationRoutes {
    fn integration_name(&self) -> &'static str {
        INTEGRATION_ID
    }

    fn routes(&self) -> Vec<IntegrationEndpoint> {
        vec![
            IntegrationEndpoint::post(SNAPSHOT_PATH),
            IntegrationEndpoint::put(format!("{RESULTS_PREFIX}*")),
            IntegrationEndpoint::put(format!("{MARKERS_PREFIX}*")),
            IntegrationEndpoint::get(format!("{PUBLIC_SOURCE_PREFIX}*")),
        ]
    }

    async fn handle(
        &self,
        call: ModuleCall<'_>,
        req: Request<EdgeBody>,
    ) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
        call.inject_with(self, req, Self::route)?.await
    }
}

#[cfg(test)]
mod tests;
