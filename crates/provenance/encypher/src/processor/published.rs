//! Reader delivery in the asynchronous prepared mode: published results
//! only, never a signing call.
//!
//! Each item's identity is computed from the exact projection or bytes this
//! reader is about to receive, through the same functions the preparation
//! worker runs on its snapshot. A result is used only when the delivery
//! instance published one under exactly that identity and no retirement
//! marker stands for it; a marker that cannot be read refuses the result.
//! The lookup is a plain read of the runtime KV store: it never starts or
//! joins a loader, and never waits for preparation.
//!
//! Every item without a result is delivered as it is, unsigned, and on a
//! deployment with `reader_trigger` the worker is handed one bounded hint:
//! within `admission_ms` on a platform with a timer, or after the response
//! on a platform that declared that lifetime. A hint is acknowledged only
//! when the worker answers that it committed it; it is never durable on
//! the edge, and publication events, warming and reconciliation stay the
//! recovery path for a hint that was lost.

use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use bytes::Bytes;
use futures::future::{Either, select};
use http::{HeaderName, HeaderValue, Method, Request, StatusCode, header};
use trusted_server_core::integrations::ensure_integration_backend_with_timeout;
use trusted_server_core::media_signing::{LargeLookupJob, MediaJob, PublishedLarge, SignedMedia};
use trusted_server_core::middleware::ResponseHeader;
use trusted_server_core::platform::{
    AfterResponseRequest, KvError, PlatformHttpRequest, PlatformKvStore, RuntimeServices,
    hand_over_after_response, platform_offers_after_response_handoff,
};

use super::{
    Article, Deadline, Reason, SKIP_WRITE_RESERVE, Signer, Surface, Target, embedded,
    is_excluded_prefix, log_outcome, render, render_reason,
};
use crate::INTEGRATION_ID;
use crate::api;
use crate::media_api::{canonical_media_type, sha256_hex, valid_origin_host};
use crate::preparation::items::{
    DocumentKind, EmbeddedItem, ItemContext, embedded_edit, embedded_items, text_item,
};
use crate::preparation::protocol::{
    Carrier, ItemKind, LargeLocator, MediaIdentity, ObservedItem, PREPARATION_PROTOCOL,
    PublishedRecord, PublishedResult, SIGNALS_PATH, SignalRequest, TOKEN_HEADER, Target as Goal,
    marker_key, result_key,
};
use crate::preparation::{Execution, PreparationConfig};

/// What one lookup found.
enum Lookup {
    /// A published result for exactly the identity.
    Found(PublishedResult),
    /// No result: the item is unprepared.
    Missing,
}

impl Signer {
    /// The module's settings.
    pub(crate) fn config(&self) -> &crate::EncypherConfig {
        &self.config
    }

    /// The host prepared addresses name, see
    /// [`PreparationConfig::public_host`].
    pub(crate) fn public_host(&self) -> &str {
        self.preparation()
            .map_or(self.config.domain.as_str(), |preparation| {
                preparation.public_host.as_str()
            })
    }

    /// Whether the module runs in the asynchronous prepared mode.
    pub(crate) fn is_async(&self) -> bool {
        self.config.execution == Execution::AsyncPrepared && self.config.preparation.is_some()
    }

    /// The asynchronous mode's settings.
    pub(crate) fn preparation(&self) -> Option<&PreparationConfig> {
        self.config
            .preparation
            .as_ref()
            .filter(|_| self.config.execution == Execution::AsyncPrepared)
    }

    /// The credential context identities bind.
    pub(crate) fn credential_context(&self) -> &str {
        &self.cache_context
    }

    /// The published result for `identity`, after its retirement marker is
    /// checked: present refuses it, unreadable refuses it.
    async fn published(
        &self,
        kv: &dyn PlatformKvStore,
        identity: &str,
        work: &mut Deadline<'_>,
    ) -> Result<Lookup, Reason> {
        match work.race(kv.get_bytes(&marker_key(identity))).await {
            None => return Err(Reason::Timeout),
            Some(Err(KvError::Unavailable)) => return Err(Reason::KvUnavailable),
            Some(Err(_)) => return Err(Reason::MarkerUnavailable),
            Some(Ok(Some(_))) => return Err(Reason::Retired),
            Some(Ok(None)) => {}
        }
        let bytes = match work.race(kv.get_bytes(&result_key(identity))).await {
            None => return Err(Reason::Timeout),
            Some(Err(KvError::Unavailable)) => return Err(Reason::KvUnavailable),
            Some(Err(_)) => return Err(Reason::KvError),
            Some(Ok(None)) => return Ok(Lookup::Missing),
            Some(Ok(Some(bytes))) => bytes,
        };
        let record: PublishedRecord =
            serde_json::from_slice(&bytes).map_err(|_| Reason::PublishedRecordInvalid)?;
        if record.protocol != PREPARATION_PROTOCOL || record.identity != identity {
            return Err(Reason::PublishedRecordInvalid);
        }
        Ok(Lookup::Found(record.result))
    }

    /// The admitted variant of a reader's `query`, or `None` where it names
    /// anything the schema does not admit.
    fn admitted_variant(&self, query: &str) -> Option<String> {
        self.preparation()
            .and_then(|preparation| preparation.variants.admit(query))
    }

    /// Looks up every item of a held document and applies what was
    /// published: the article's plan first, then embedded media, each by
    /// its own identity, so one unprepared item leaves its siblings signed.
    /// Returns the bytes to emit.
    pub(super) async fn resolve_published_document(
        &self,
        target: &Target<'_>,
        services: &RuntimeServices,
        document: &str,
        article: Option<Article>,
        media: embedded::Scan,
        work: &mut Deadline<'_>,
    ) -> Vec<u8> {
        let request = target.request;
        let kv = services.kv_store();
        let Some(page_url) = self.canonical_page_url(&request.page_url) else {
            return document.as_bytes().to_vec();
        };
        let variant = self.admitted_variant(&request.query);
        // A query outside the schema is looked up as the plain address:
        // every identity binds the exact text or bytes, so a result found
        // is exactly this content's. Only a hint needs an admitted variant.
        let lookup_variant = variant.as_deref().unwrap_or_default();
        let mut misses: Vec<ObservedItem> = Vec::new();
        let mut output = document.to_owned();

        if let Some(article) = &article {
            let policy_digest = match self.policy_digest(&page_url, services, kv, work).await {
                Ok(policy_digest) => Some(policy_digest),
                Err(reason) => {
                    log_outcome(false, reason);
                    None
                }
            };
            if let Some(policy_digest) = policy_digest {
                let context = self.item_context(&page_url, &policy_digest, lookup_variant);
                let item = text_item(&context, document, article);
                match self.published(kv, &item.identity, work).await {
                    Ok(Lookup::Found(PublishedResult::Text {
                        plan,
                        manifest_url,
                        scalars,
                    })) if scalars == article.scalars
                        && api::validate_plan(&plan, manifest_url.as_deref(), article.scalars)
                            .is_ok() =>
                    {
                        match render(
                            document,
                            article,
                            &plan,
                            manifest_url.as_deref(),
                            self.max_document_bytes,
                        ) {
                            Ok(signed) => {
                                log_outcome(true, Reason::CacheHit);
                                output = signed;
                            }
                            Err(error) => log_outcome(false, render_reason(error)),
                        }
                    }
                    Ok(Lookup::Found(_)) => log_outcome(false, Reason::PublishedRecordInvalid),
                    Ok(Lookup::Missing) => {
                        log_outcome(false, Reason::NotPrepared);
                        misses.push(ObservedItem {
                            kind: ItemKind::Text,
                            identity: item.identity,
                        });
                    }
                    Err(reason) => log_outcome(false, reason),
                }
            }
        }

        if !media.is_empty() {
            // Media is looked up only under the policy the service answers
            // now, as in the synchronous mode, so a revoked credential stops
            // every published copy.
            match self.request_policy(&page_url, services, work).await {
                Ok(policy_digest) => {
                    let context = self.item_context(&page_url, &policy_digest, lookup_variant);
                    let kind = match target.surface {
                        Surface::Page => DocumentKind::Page,
                        Surface::Stylesheet => DocumentKind::Stylesheet,
                    };
                    let (items, refused) = embedded_items(&context, &output, kind, target.policies);
                    for reason in refused {
                        log::info!("encypher: media outcome=unsigned reason={reason}");
                    }
                    let mut edits = Vec::new();
                    for item in &items {
                        match self.published_embedded(kv, &output, item, work).await {
                            Ok(Some(edit)) => {
                                log::info!("encypher: media outcome=signed reason=cache_hit");
                                edits.push(edit);
                            }
                            Ok(None) => {
                                log::info!("encypher: media outcome=unsigned reason=not_prepared");
                                misses.push(ObservedItem {
                                    kind: ItemKind::Embedded,
                                    identity: item.identity.clone(),
                                });
                            }
                            Err(reason) => log::info!(
                                "encypher: media outcome=unsigned reason={}",
                                reason.code()
                            ),
                        }
                    }
                    if !edits.is_empty() {
                        match embedded::splice(&output, &edits, self.max_document_bytes) {
                            Some(spliced) => output = spliced,
                            None => log::info!(
                                "encypher: media outcome=unsigned reason=output_too_large"
                            ),
                        }
                    }
                }
                Err(reason) => super::media::log_unsigned(&media, reason),
            }
        }

        self.hint(services, &page_url, variant.as_deref(), &misses)
            .await;
        output.into_bytes()
    }

    /// The address identities name for a reader's `page_url` on a claimed
    /// host: the configured public host and the path, `https`, without the
    /// query. `None` for an address outside the claim.
    pub(crate) fn canonical_page_url(&self, page_url: &str) -> Option<String> {
        let parsed = url::Url::parse(page_url).ok()?;
        let host = parsed.host_str()?;
        if !self.claims_host(host) {
            return None;
        }
        Some(format!("https://{}{}", self.public_host(), parsed.path()))
    }

    /// What every identity of a document at `page_url` binds.
    fn item_context<'a>(
        &'a self,
        page_url: &'a str,
        policy_digest: &'a str,
        variant: &'a str,
    ) -> ItemContext<'a> {
        ItemContext {
            credential_context: &self.cache_context,
            policy_digest,
            page_url,
            variant,
            article_selector: &self.config.article_selector,
            min_text_length: self.config.min_text_length,
            max_media_bytes: self.config.max_media_bytes,
        }
    }

    /// The edit an embedded item's published output makes, `None` where
    /// none is published. The output must be exactly the one its record
    /// names, made from exactly this item's source.
    async fn published_embedded(
        &self,
        kv: &dyn PlatformKvStore,
        document: &str,
        item: &EmbeddedItem,
        work: &mut Deadline<'_>,
    ) -> Result<Option<(usize, usize, String)>, Reason> {
        match self.published(kv, &item.identity, work).await? {
            Lookup::Missing => Ok(None),
            Lookup::Found(PublishedResult::Embedded {
                source_sha256,
                output_sha256,
                content_type,
                output_b64,
                ..
            }) => {
                if source_sha256 != item.sha256 || content_type != item.mime {
                    return Err(Reason::PublishedRecordInvalid);
                }
                let output = STANDARD
                    .decode(output_b64)
                    .map_err(|_| Reason::PublishedRecordInvalid)?;
                if sha256_hex(&output) != output_sha256 {
                    return Err(Reason::PublishedRecordInvalid);
                }
                if item.mime == "image/svg+xml" {
                    embedded::verify_svg_carrier(&item.source, &output)?;
                }
                embedded_edit(document, item, &output)
                    .map(Some)
                    .ok_or(Reason::PublishedRecordInvalid)
            }
            Lookup::Found(_) => Err(Reason::PublishedRecordInvalid),
        }
    }

    /// The published representation of one captured standalone item: the
    /// origin's own bytes, which the record binds by digest, with the
    /// manifest `Link` and ids it names. Never a signing call.
    pub(super) async fn published_delivery(
        &self,
        services: &RuntimeServices,
        job: &MediaJob<'_>,
    ) -> Result<SignedMedia, Reason> {
        let reader_url = url::Url::parse(job.reader_url).map_err(|_| Reason::OutsideClaim)?;
        let host = reader_url.host_str().ok_or(Reason::OutsideClaim)?;
        if !matches!(reader_url.scheme(), "https" | "http")
            || !self.claims_host(host)
            || is_excluded_prefix(reader_url.path())
        {
            return Err(Reason::OutsideClaim);
        }
        let page_url = self
            .canonical_page_url(job.reader_url)
            .ok_or(Reason::OutsideClaim)?;
        if job.optimizer_output {
            return Err(Reason::OptimizerOutputUnsupported);
        }
        if job.source.len() > self.config.max_media_bytes {
            return Err(Reason::MediaTooLarge);
        }
        let sha256 = sha256_hex(job.source);
        if sha256 != job.source_sha256 {
            return Err(Reason::SourceDigestMismatch);
        }
        let variant = self.admitted_variant(job.reader_query);
        let mut work = Deadline::new(
            services.timer(),
            Duration::from_millis(self.config.max_hold_ms).saturating_sub(SKIP_WRITE_RESERVE),
        );
        work.start().await;
        let policy_digest = self.request_policy(&page_url, services, &mut work).await?;
        let mime = canonical_media_type(job.content_type);
        let identity = MediaIdentity {
            credential_context: &self.cache_context,
            policy_digest: &policy_digest,
            page_url: &page_url,
            variant: variant.as_deref().unwrap_or_default(),
            carrier: Carrier::External,
            mime: &mime,
            source_sha256: &sha256,
        }
        .digest();
        match self
            .published(services.kv_store(), &identity, &mut work)
            .await?
        {
            Lookup::Found(PublishedResult::External {
                source_sha256,
                headers,
            }) if source_sha256 == sha256 => Ok(SignedMedia {
                bytes: job.source.clone(),
                content_type: HeaderValue::from_str(job.content_type)
                    .map_err(|_| Reason::InvalidSignedResponse)?,
                headers: response_headers(&headers)?,
                namespace: self.kept_namespace(&policy_digest),
            }),
            Lookup::Found(_) => Err(Reason::PublishedRecordInvalid),
            Lookup::Missing => {
                self.hint(
                    services,
                    &page_url,
                    variant.as_deref(),
                    &[ObservedItem {
                        kind: ItemKind::Media,
                        identity,
                    }],
                )
                .await;
                Err(Reason::NotPrepared)
            }
        }
    }

    /// The published headers of one large version, by its validator
    /// locator. The record carries the whole-source digest the worker and
    /// the delivery instance verified for that version.
    pub(super) async fn published_large(
        &self,
        services: &RuntimeServices,
        job: &LargeLookupJob<'_>,
    ) -> Result<PublishedLarge, Reason> {
        let reader_url = url::Url::parse(job.reader_url).map_err(|_| Reason::OutsideClaim)?;
        let host = reader_url.host_str().ok_or(Reason::OutsideClaim)?;
        if !self.claims_host(host) || is_excluded_prefix(reader_url.path()) {
            return Err(Reason::OutsideClaim);
        }
        let page_url = self
            .canonical_page_url(job.reader_url)
            .ok_or(Reason::OutsideClaim)?;
        let variant = self
            .admitted_variant(job.reader_query)
            .ok_or(Reason::NotPrepared)?;
        // The origin saw the raw query. Ordering and encoding aliases need
        // not name the same resource, even when their strong ETags match.
        if variant != job.reader_query {
            return Err(Reason::NotPrepared);
        }
        // Core names the configured source as well as its host: the same
        // host can serve another source under equal validators.
        if !valid_origin_host(job.source_origin_host) || job.origin_identity.is_empty() {
            return Err(Reason::InvalidOriginHost);
        }
        let mut work = Deadline::new(
            services.timer(),
            Duration::from_millis(self.config.max_hold_ms).saturating_sub(SKIP_WRITE_RESERVE),
        );
        work.start().await;
        let policy_digest = self.request_policy(&page_url, services, &mut work).await?;
        let mime = canonical_media_type(job.content_type);
        let origin_identity_sha256 = sha256_hex(job.origin_identity.as_bytes());
        let identity = LargeLocator {
            credential_context: &self.cache_context,
            policy_digest: &policy_digest,
            reader_url: &page_url,
            variant: &variant,
            origin_host: job.source_origin_host,
            origin_identity_sha256: &origin_identity_sha256,
            mime: &mime,
            etag: job.etag,
            length: job.length,
        }
        .digest();
        match self
            .published(services.kv_store(), &identity, &mut work)
            .await?
        {
            Lookup::Found(PublishedResult::Large {
                sha256,
                length,
                headers,
            }) => {
                let digest = decode_digest(&sha256).ok_or(Reason::PublishedRecordInvalid)?;
                if job.length != 0 && length != job.length {
                    return Err(Reason::PublishedRecordInvalid);
                }
                Ok(PublishedLarge {
                    headers: response_headers(&headers)?,
                    sha256: digest,
                    length,
                })
            }
            Lookup::Found(_) => Err(Reason::PublishedRecordInvalid),
            Lookup::Missing => {
                self.hint(
                    services,
                    &page_url,
                    Some(&variant),
                    &[ObservedItem {
                        kind: ItemKind::Large,
                        identity,
                    }],
                )
                .await;
                Err(Reason::NotPrepared)
            }
        }
    }

    /// Hands the worker one bounded hint about `items` of the target at
    /// `page_url`, where the deployment asked for reader hints and the
    /// target is a configured route with an admitted variant. Never awaited
    /// past `admission_ms`, never retried, never a signing call.
    async fn hint(
        &self,
        services: &RuntimeServices,
        page_url: &str,
        variant: Option<&str>,
        items: &[ObservedItem],
    ) {
        let Some(preparation) = self.preparation().filter(|config| config.reader_trigger) else {
            return;
        };
        if items.is_empty() {
            return;
        }
        let Some(variant) = variant else {
            log::info!("encypher: hint outcome=not_sent reason=variant_not_admitted");
            return;
        };
        let Some(path) = url::Url::parse(page_url)
            .ok()
            .map(|url| url.path().to_owned())
            .filter(|path| preparation.admits_path(path))
        else {
            log::info!("encypher: hint outcome=not_sent reason=route_not_configured");
            return;
        };
        let target = Goal {
            path,
            variant: variant.to_owned(),
        };
        if !target.is_well_formed() {
            log::info!("encypher: hint outcome=not_sent reason=target_invalid");
            return;
        }
        let Some(body) = SignalRequest::encode_bounded(&preparation.deployment_id, target, items)
        else {
            log::info!("encypher: hint outcome=not_sent reason=hint_too_large");
            return;
        };
        let budget = Duration::from_millis(preparation.admission_ms);
        let Ok(backend) = ensure_integration_backend_with_timeout(
            services,
            &preparation.worker_url,
            INTEGRATION_ID,
            budget,
        ) else {
            log::info!("encypher: hint outcome=not_sent reason=backend");
            return;
        };
        let Some(request) = signal_request(preparation, Bytes::from(body)) else {
            log::info!("encypher: hint outcome=not_sent reason=request_build");
            return;
        };
        if let Some(timer) = services.timer() {
            let (parts, body) = request.into_parts();
            let request = Request::from_parts(parts, edgezero_core::body::Body::from_bytes(body));
            let send = services.http_client().send(
                PlatformHttpRequest::new(request, backend)
                    .with_cache_bypass()
                    .with_max_response_body_bytes(1024),
            );
            let outcome = match select(Box::pin(send), timer.sleep(budget)).await {
                Either::Left((Ok(answer), _)) => match answer.response.status() {
                    StatusCode::ACCEPTED | StatusCode::OK => "admitted",
                    StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE => {
                        "refused_retryable"
                    }
                    _ => "refused",
                },
                Either::Left((Err(_), _)) => "unavailable",
                // No acknowledgement within the budget: the hint may or may
                // not have been committed, and a later identical one is safe.
                Either::Right(_) => "admission_unknown",
            };
            log::info!("encypher: hint outcome={outcome} items={}", items.len());
        } else if platform_offers_after_response_handoff() {
            let handed = hand_over_after_response(AfterResponseRequest {
                request,
                backend_name: backend,
            });
            log::info!(
                "encypher: hint outcome={} items={}",
                if handed {
                    "handed_over_after_response"
                } else {
                    "handoff_full"
                },
                items.len()
            );
        } else {
            log::info!("encypher: hint outcome=not_sent reason=reader_trigger_unavailable");
        }
    }
}

/// The hint request, authenticated with the reader-signal scope only.
fn signal_request(preparation: &PreparationConfig, body: Bytes) -> Option<Request<Bytes>> {
    let mut token = HeaderValue::from_str(preparation.signal_secret.expose().trim()).ok()?;
    token.set_sensitive(true);
    Request::builder()
        .method(Method::POST)
        .uri(format!(
            "{}{SIGNALS_PATH}",
            preparation.worker_url.trim_end_matches('/')
        ))
        .header(header::CONTENT_TYPE, "application/json")
        .header(TOKEN_HEADER, token)
        .body(body)
        .ok()
}

/// A record's headers as response headers, refusing any that does not
/// parse.
fn response_headers(headers: &[(String, String)]) -> Result<Vec<ResponseHeader>, Reason> {
    headers
        .iter()
        .map(|(name, value)| {
            Ok((
                HeaderName::from_bytes(name.as_bytes())
                    .map_err(|_| Reason::PublishedRecordInvalid)?,
                HeaderValue::from_str(value).map_err(|_| Reason::PublishedRecordInvalid)?,
            ))
        })
        .collect()
}

/// A lower case hex SHA-256 as its 32 bytes.
fn decode_digest(hex: &str) -> Option<[u8; 32]> {
    if !crate::preparation::protocol::is_digest(hex) {
        return None;
    }
    let mut digest = [0_u8; 32];
    for (index, slot) in digest.iter_mut().enumerate() {
        *slot = u8::from_str_radix(hex.get(index * 2..index * 2 + 2)?, 16).ok()?;
    }
    Some(digest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn published_digest_and_headers_refuse_anything_malformed() {
        let hex = "ab".repeat(32);
        assert_eq!(
            decode_digest(&hex),
            Some([0xab; 32]),
            "should decode a lower case digest"
        );
        assert!(
            decode_digest(&"AB".repeat(32)).is_none(),
            "should refuse upper case"
        );
        assert!(
            decode_digest("abc").is_none(),
            "should refuse a short digest"
        );
        let headers = response_headers(&[(
            "link".to_owned(),
            "<https://api.example.com/m>; rel=\"c2pa-manifest\"".to_owned(),
        )])
        .expect("should parse a valid header");
        assert_eq!(headers.len(), 1);
        assert_eq!(
            response_headers(&[("bad name".to_owned(), "v".to_owned())]),
            Err(Reason::PublishedRecordInvalid),
            "a record with a header that does not parse is refused whole"
        );
    }
}
