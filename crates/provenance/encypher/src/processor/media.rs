//! Media signing through the Encypher media endpoint.
//!
//! One source's exact bytes are sent with the server policy the edge holds,
//! and the answer is kept only after [`media_api::verify_signed_response`]
//! accepts it. Results are cached in the runtime KV store under a namespace
//! that binds the credential context, the policy digest, the delivery
//! address, the source reference, its declared and verified type, its bytes'
//! digest and whether a `Link` can be delivered, so a changed policy,
//! credential or source never reuses another's signature. The replay key the
//! endpoint requires is derived from the same namespace, so a repeated call
//! is answered with the identical bytes.
//!
//! Two callers use it: core's binary delivery seam, through [`EncypherMedia`],
//! and the page and stylesheet processors for embedded `data:` URIs and
//! inline SVG, through [`Signer::sign_embedded`]. Core keeps the signed
//! representations it delivers under [`MediaSigner::namespace`], which the
//! module answers only after a policy check no older than
//! `policy_ttl_seconds`, so a revoked credential or changed policy stops
//! every kept copy from being served.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use bytes::Bytes;
use edgezero_core::body::Body as EdgeBody;
use http::{HeaderMap, HeaderName, HeaderValue, Method, Request, StatusCode, header};
use serde::{Deserialize, Serialize};
use trusted_server_core::media_signing::{
    LargeMediaJob, LargeMediaSigning, MediaJob, MediaNamespacing, MediaSigner, MediaSigning,
    MediaSigningFailure, SignedMedia as DeliveredMedia,
};
use trusted_server_core::middleware::ResponseHeader;
use trusted_server_core::platform::RuntimeServices;

use super::{
    Deadline, Expiring, PageRequest, Reason, SKIP_WRITE_RESERVE, Signer, Surface,
    WORKER_TOKEN_HEADER, is_excluded_prefix, unix_now,
};
use crate::MODULE;
use crate::data_uri::DataUri;
use crate::embedded::{self, Scan};
use crate::media_api::{
    self, Disposition, Expected, MediaMetadata, ResponseError, SignedMedia, SourceMetadata,
    canonical_media_type, safe_source_reference, sha256_hex, valid_origin_host,
};
use crate::svg_equivalence;

/// Verified signed media. Entries carry their own `expires_at`.
const MEDIA_KEY_PREFIX: &str = "encypher:media:v1";
/// Media failures, separate so a delayed failure cannot replace a result.
const MEDIA_SKIP_KEY_PREFIX: &str = "encypher:media-skip:v1";
/// A fresh replay-key salt, written when the endpoint no longer holds or
/// refuses the result for the previous key.
const MEDIA_REPLAY_KEY_PREFIX: &str = "encypher:media-replay:v1";
const SVG_MEDIA_TYPE: &str = "image/svg+xml";

/// The media types the signer signs: images, audio, video and fonts,
/// whatever the path they are delivered on. Whether a format is signable is
/// decided by its verified bytes at the endpoint, which answers an
/// unsupported one `422`, delivered unsigned.
const SIGNABLE_TYPE_PREFIXES: &[&str] = &["image/", "audio/", "video/", "font/"];

/// The media type of one segment of a fragmented ISO BMFF stream, as ISO/IEC
/// 14496-12 registers it. A segment's provenance is bound through its
/// stream's init segment, as with C2PA verifiable segment info, so it is
/// never signed alone and reaches the reader as the origin sent it.
const MEDIA_SEGMENT_TYPE: &str = "video/iso.segment";

const INSTANCE_ID_HEADER: HeaderName = HeaderName::from_static("x-encypher-manifest-instance-id");
const DOCUMENT_ID_HEADER: HeaderName = HeaderName::from_static("x-encypher-document-id");
const VERIFICATION_URL_HEADER: HeaderName = HeaderName::from_static("x-encypher-verification-url");

/// One media source to sign, as it is delivered.
struct MediaSource<'a> {
    /// The reader-facing address the media is delivered on or in.
    page_url: &'a str,
    /// A safe canonical address of the source, or none.
    reference: Option<&'a str>,
    /// The hostname the source was fetched from, or none for media embedded
    /// in a document.
    origin_host: Option<&'a str>,
    /// The declared type, sent as a hint.
    mime: &'a str,
    /// The bare type the signed bytes must keep.
    media_type: &'a str,
    bytes: &'a [u8],
    sha256: &'a str,
    /// Whether every response of the representation can carry a `Link`.
    link_deliverable: bool,
    /// The bytes are the platform image optimizer's answer, so the endpoint
    /// must refuse them unless they carry an intact embedded manifest, the
    /// optimizer's own record of its input and what it did.
    require_embedded_manifest: bool,
}

/// A cached media result.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum MediaEntry {
    /// The verified answer: its base64 bytes and its signing headers, which
    /// are verified again on every read.
    Signed {
        bytes: String,
        headers: Vec<(String, String)>,
        expires_at: u64,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        ttl_less: bool,
    },
    Skip {
        reason: Reason,
        expires_at: u64,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        ttl_less: bool,
    },
    Replay {
        salt: String,
        expires_at: u64,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        ttl_less: bool,
    },
}

impl Expiring for MediaEntry {
    fn expires_at(&self) -> u64 {
        match self {
            Self::Signed { expires_at, .. }
            | Self::Skip { expires_at, .. }
            | Self::Replay { expires_at, .. } => *expires_at,
        }
    }

    fn ttl_less(&self) -> bool {
        match self {
            Self::Signed { ttl_less, .. }
            | Self::Skip { ttl_less, .. }
            | Self::Replay { ttl_less, .. } => *ttl_less,
        }
    }

    fn mark_ttl_less(&mut self) {
        match self {
            Self::Signed { ttl_less, .. }
            | Self::Skip { ttl_less, .. }
            | Self::Replay { ttl_less, .. } => *ttl_less = true,
        }
    }
}

impl MediaEntry {
    fn signed(signed: &SignedMedia, headers: &HeaderMap, expires_at: u64) -> Self {
        let headers = media_api::SIGNING_HEADERS
            .iter()
            .filter_map(|name| {
                let value = headers.get(*name)?.to_str().ok()?;
                Some(((*name).to_owned(), value.to_owned()))
            })
            .collect();
        Self::Signed {
            bytes: STANDARD.encode(&signed.bytes),
            headers,
            expires_at,
            ttl_less: false,
        }
    }
}

/// A cached answer, verified as if it had just arrived.
fn verify_cached(
    bytes: &str,
    headers: &[(String, String)],
    expected: &Expected<'_>,
) -> Result<SignedMedia, Reason> {
    let body = STANDARD
        .decode(bytes)
        .map_err(|_| Reason::CachedEntryInvalid)?;
    let mut map = HeaderMap::with_capacity(headers.len());
    for (name, value) in headers {
        let name =
            HeaderName::from_bytes(name.as_bytes()).map_err(|_| Reason::CachedEntryInvalid)?;
        let value = HeaderValue::from_str(value).map_err(|_| Reason::CachedEntryInvalid)?;
        map.append(name, value);
    }
    media_api::verify_signed_response(&map, Bytes::from(body), expected)
        .map_err(|_| Reason::CachedEntryInvalid)
}

fn response_reason(error: ResponseError) -> Reason {
    match error {
        ResponseError::ContentType => Reason::SignedTypeMismatch,
        ResponseError::PolicyDigest => Reason::PolicyDigestMismatch,
        ResponseError::SignedDigest => Reason::SignedDigestMismatch,
        ResponseError::Malformed => Reason::InvalidSignedResponse,
        ResponseError::LinkUndeliverable => Reason::ManifestUndeliverable,
        ResponseError::Unsigned => Reason::UnsignedResponse,
    }
}

/// The reason for an error answer, by its status and failure code.
fn status_reason(status: StatusCode, code: Option<&str>) -> Reason {
    match status.as_u16() {
        403 | 404 => Reason::Forbidden,
        409 if code == Some("SIGNING_IDEMPOTENCY_CONFLICT") => Reason::IdempotencyConflict,
        409 => Reason::PolicyChanged,
        410 => Reason::ResultPurged,
        413 => Reason::PayloadTooLarge,
        422 => Reason::Unprocessable,
        429 => Reason::RateLimited,
        500..=599 => Reason::ServerError,
        _ => Reason::UnexpectedStatus,
    }
}

fn log_media(signed: bool, reason: Reason) {
    log::info!(
        "encypher: media outcome={} reason={}",
        if signed { "signed" } else { "unsigned" },
        reason.code()
    );
}

/// Reports every media item of `scan` unsigned for `reason`.
pub(super) fn log_unsigned(scan: &Scan, reason: Reason) {
    for refused in &scan.refused {
        log_media(false, *refused);
    }
    for _ in 0..scan.found.len() + scan.svgs.len() {
        log_media(false, reason);
    }
}

/// The document an embedded media pass works for.
pub(super) struct EmbeddedJob<'a> {
    pub(super) request: &'a PageRequest,
    pub(super) surface: Surface,
    pub(super) policy_digest: &'a str,
}

/// One item's outcome in an embedded pass: signed, from the cache or fresh,
/// or unsigned with why.
type Outcome = Result<Reason, Reason>;

impl Signer {
    fn max_media_bytes(&self) -> usize {
        self.config.max_media_bytes
    }

    /// The cache namespace of `source` under `policy_digest`: every input the
    /// signed result depends on.
    fn media_namespace(&self, source: &MediaSource<'_>, policy_digest: &str) -> String {
        sha256_hex(
            format!(
                "{}\n{policy_digest}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}",
                self.cache_context,
                source.page_url,
                source.reference.unwrap_or_default(),
                source.origin_host.unwrap_or_default(),
                source.mime,
                source.media_type,
                source.sha256,
                source.link_deliverable,
                source.require_embedded_manifest
            )
            .as_bytes(),
        )
    }

    /// Sign one source, from the cache when a verified result is held.
    /// Returns the result and whether it was signed now or read back. A call
    /// to the endpoint is made only while `calls` is below
    /// `self.config.max_media_sign_calls_per_document`, and counted there.
    async fn sign_media(
        &self,
        services: &RuntimeServices,
        source: &MediaSource<'_>,
        policy_digest: &str,
        work: &mut Deadline<'_>,
        calls: &mut usize,
    ) -> Result<(SignedMedia, Reason), Reason> {
        if source.bytes.len() > self.max_media_bytes() {
            return Err(Reason::MediaTooLarge);
        }
        // Once the budget is spent no call is made and no skip written, so
        // the reserve stays for the document's own writes.
        if work.expired() {
            return Err(Reason::Timeout);
        }
        let kv = services.kv_store();
        let namespace = self.media_namespace(source, policy_digest);
        let key = format!("{MEDIA_KEY_PREFIX}:{namespace}");
        let skip_key = format!("{MEDIA_SKIP_KEY_PREFIX}:{namespace}");
        let expected = Expected {
            policy_digest,
            media_type: source.media_type,
            unsigned_sha256: source.sha256,
            link_deliverable: source.link_deliverable,
        };
        let outcome = match self.read::<MediaEntry>(kv, &key, work).await {
            Ok(Some(MediaEntry::Signed { bytes, headers, .. })) => {
                return match verify_cached(&bytes, &headers, &expected) {
                    Ok(signed) => Ok((signed, Reason::CacheHit)),
                    Err(reason) => {
                        self.delete_invalid_entry(kv, &key, work).await;
                        Err(reason)
                    }
                };
            }
            Ok(Some(_)) => {
                self.delete_invalid_entry(kv, &key, work).await;
                return Err(Reason::CachedEntryInvalid);
            }
            Err(Reason::Timeout) => Err(Reason::Timeout),
            Err(reason) => return Err(reason),
            Ok(None) => match self.read::<MediaEntry>(kv, &skip_key, work).await {
                Ok(Some(MediaEntry::Skip { .. })) => return Err(Reason::CachedSkip),
                Ok(Some(_)) => {
                    self.delete_invalid_entry(kv, &skip_key, work).await;
                    return Err(Reason::CachedEntryInvalid);
                }
                Err(Reason::Timeout) => Err(Reason::Timeout),
                Err(reason) => return Err(reason),
                Ok(None) if *calls >= self.config.max_media_sign_calls_per_document => {
                    return Err(Reason::Deferred);
                }
                Ok(None) => {
                    *calls += 1;
                    self.request_media(services, source, policy_digest, &namespace, &expected, work)
                        .await
                }
            },
        };
        match outcome {
            Ok((signed, headers)) => {
                // A result write that runs out of time is abandoned; the media
                // is still signed.
                let entry = MediaEntry::signed(
                    &signed,
                    &headers,
                    Self::expires_at(self.config.plan_ttl_seconds),
                );
                self.write(kv, &key, entry, self.config.plan_ttl_seconds, work)
                    .await;
                Ok((signed, Reason::Signed))
            }
            Err(reason @ (Reason::ResultPurged | Reason::IdempotencyConflict)) => {
                // The next view asks again under a new replay key; no skip,
                // and never a retry within this view.
                let entry = MediaEntry::Replay {
                    salt: format!("{}", unix_now()),
                    expires_at: Self::expires_at(self.config.plan_ttl_seconds),
                    ttl_less: false,
                };
                let replay_key = format!("{MEDIA_REPLAY_KEY_PREFIX}:{namespace}");
                self.write(
                    kv,
                    &replay_key,
                    entry,
                    self.config.plan_ttl_seconds,
                    &mut Self::skip_deadline(services),
                )
                .await;
                Err(reason)
            }
            Err(reason) => {
                let mut deadline = Self::skip_deadline(services);
                if matches!(reason, Reason::PolicyChanged | Reason::PolicyDigestMismatch) {
                    self.evict_policy(kv, &mut deadline).await;
                }
                let ttl = self.config.negative_ttl_seconds;
                let entry = MediaEntry::Skip {
                    reason,
                    expires_at: Self::expires_at(ttl),
                    ttl_less: false,
                };
                self.write(kv, &skip_key, entry, ttl, &mut deadline).await;
                Err(reason)
            }
        }
    }

    /// Call the media endpoint once with the source's exact bytes. Every
    /// failure maps to a reason code.
    async fn request_media(
        &self,
        services: &RuntimeServices,
        source: &MediaSource<'_>,
        policy_digest: &str,
        namespace: &str,
        expected: &Expected<'_>,
        work: &mut Deadline<'_>,
    ) -> Result<(SignedMedia, HeaderMap), Reason> {
        let kv = services.kv_store();
        let replay_key = format!("{MEDIA_REPLAY_KEY_PREFIX}:{namespace}");
        let salt = match self.read::<MediaEntry>(kv, &replay_key, work).await {
            Ok(Some(MediaEntry::Replay { salt, .. })) => salt,
            Ok(Some(_)) => {
                self.delete_invalid_entry(kv, &replay_key, work).await;
                return Err(Reason::CachedEntryInvalid);
            }
            Ok(None) => String::new(),
            Err(reason) => return Err(reason),
        };
        let idempotency_key = sha256_hex(format!("{namespace}\n{salt}").as_bytes());
        let metadata = serde_json::to_vec(&MediaMetadata::unmodified(
            source.page_url,
            policy_digest,
            SourceMetadata {
                reference: source.reference,
                origin_host: source.origin_host,
                mime: source.mime,
                sha256: source.sha256,
                byte_length: source.bytes.len(),
                require_embedded_manifest: source.require_embedded_manifest,
            },
            source.link_deliverable,
        ))
        .map_err(|_| Reason::RequestBuild)?;
        let multipart = media_api::multipart(&metadata, source.bytes);
        if multipart.body.len() > media_api::MAX_REQUEST_BYTES {
            return Err(Reason::PayloadTooLarge);
        }
        let http_request = Request::builder()
            .method(Method::POST)
            .uri(&self.media_sign_url)
            .header(header::CONTENT_TYPE, multipart.content_type)
            .header(WORKER_TOKEN_HEADER, self.worker_token()?)
            .header(media_api::IDEMPOTENCY_KEY_HEADER, idempotency_key)
            .body(EdgeBody::from_bytes(multipart.body))
            .map_err(|_| Reason::RequestBuild)?;
        let max_body = source
            .bytes
            .len()
            .saturating_add(media_api::MAX_MANIFEST_OVERHEAD_BYTES);
        let response_limit = max_body.saturating_add(1);
        let response = self
            .send_api(services, http_request, response_limit, work)
            .await?;
        let status = response.status();
        let headers = response.headers().clone();
        if status != StatusCode::OK {
            let code = work
                .race(
                    response
                        .into_body()
                        .into_bytes_bounded(media_api::MAX_ERROR_BODY_BYTES),
                )
                .await
                .and_then(Result::ok)
                .and_then(|body| media_api::error_code(&body));
            log::info!(
                "encypher: media sign answered status={} code={}",
                status.as_u16(),
                code.as_deref().unwrap_or("none")
            );
            return Err(status_reason(status, code.as_deref()));
        }
        let body = match work
            .race(response.into_body().into_bytes_bounded(response_limit))
            .await
        {
            None => return Err(Reason::Timeout),
            Some(Err(_)) => return Err(Reason::ResponseTooLarge),
            Some(Ok(body)) => body,
        };
        if body.len() > max_body {
            return Err(Reason::ResponseTooLarge);
        }
        let signed =
            media_api::verify_signed_response(&headers, body, expected).map_err(response_reason)?;
        Ok((signed, headers))
    }

    /// Sign the editable media embedded in `document`: each `data:` URI
    /// found, then each inline SVG last as the equivalent standalone document.
    /// The scan excludes entire original provenance-bearing or uncertain SVG
    /// ranges before either pass, so child signing cannot invalidate them.
    /// Returns the new document, or `None` when nothing in it was signed.
    pub(super) async fn sign_embedded(
        &self,
        services: &RuntimeServices,
        job: &EmbeddedJob<'_>,
        document: &str,
        scan: &Scan,
        work: &mut Deadline<'_>,
    ) -> Option<String> {
        for reason in &scan.refused {
            log_media(false, *reason);
        }
        let mut outcomes: Vec<Outcome> = Vec::new();
        let mut edits: Vec<(usize, usize, String)> = Vec::new();
        let mut signed_sources: Vec<(String, String, Result<SignedMedia, Reason>)> = Vec::new();
        let mut calls = 0_usize;
        for found in &scan.found {
            let Some(uri) = document
                .get(found.start..found.end)
                .and_then(DataUri::parse)
            else {
                continue;
            };
            let Ok(bytes) = uri.decode() else {
                outcomes.push(Err(Reason::MalformedDataUri));
                continue;
            };
            let media_type = uri.media_type();
            let sha256 = sha256_hex(&bytes);
            let cached = signed_sources
                .iter()
                .find(|(sha, kind, _)| *sha == sha256 && *kind == media_type)
                .map(|(_, _, result)| result.clone());
            let result = match cached {
                Some(result) => result.map(|signed| (signed, Reason::CacheHit)),
                None => {
                    let source = MediaSource {
                        page_url: &job.request.page_url,
                        reference: None,
                        origin_host: None,
                        mime: uri.declared_type(),
                        media_type: &media_type,
                        bytes: &bytes,
                        sha256: &sha256,
                        link_deliverable: false,
                        require_embedded_manifest: false,
                    };
                    let result = self
                        .sign_media(services, &source, job.policy_digest, work, &mut calls)
                        .await
                        .and_then(|(signed, reason)| {
                            if media_type == SVG_MEDIA_TYPE {
                                embedded::verify_svg_carrier(&bytes, &signed.bytes)?;
                            }
                            Ok((signed, reason))
                        });
                    signed_sources.push((
                        sha256.clone(),
                        media_type.clone(),
                        result
                            .as_ref()
                            .map(|(signed, _)| signed.clone())
                            .map_err(|reason| *reason),
                    ));
                    result
                }
            };
            match result {
                Ok((signed, reason)) => {
                    edits.push((found.start, found.end, uri.encode(&signed.bytes)));
                    outcomes.push(Ok(reason));
                }
                Err(reason) => outcomes.push(Err(reason)),
            }
        }
        let mut output = None;
        if !edits.is_empty() {
            output = embedded::splice(document, &edits, self.max_document_bytes);
            if output.is_none() {
                revoke(&mut outcomes);
            }
        }
        if job.surface == Surface::Page {
            let current = output.as_deref().unwrap_or(document);
            let svgs = if output.is_some() {
                embedded::scan_html(current, None).svgs
            } else {
                scan.svgs.clone()
            };
            let mut svg_outcomes = Vec::with_capacity(svgs.len());
            let mut svg_edits = Vec::new();
            for (start, end) in svgs {
                let Some(fragment) = current.get(start..end) else {
                    continue;
                };
                match self
                    .sign_inline_svg(services, job, fragment, work, &mut calls)
                    .await
                {
                    Ok((signed, reason)) => {
                        svg_edits.push((start, end, signed));
                        svg_outcomes.push(Ok(reason));
                    }
                    Err(reason) => svg_outcomes.push(Err(reason)),
                }
            }
            if !svg_edits.is_empty() {
                match embedded::splice(current, &svg_edits, self.max_document_bytes) {
                    Some(signed) => output = Some(signed),
                    None => revoke(&mut svg_outcomes),
                }
            }
            outcomes.extend(svg_outcomes);
        }
        for outcome in &outcomes {
            match outcome {
                Ok(reason) => log_media(true, *reason),
                Err(reason) => log_media(false, *reason),
            }
        }
        output
    }

    /// Sign one inline SVG as the standalone document
    /// [`signable_svg`](svg_equivalence::signable_svg) proves equivalent to
    /// the browser's tree: the original bytes where they already are one,
    /// else the browser's tree serialized as XML. Those bytes are hashed,
    /// sent, cached and compared with the answer, and the result is kept
    /// only when the signer added its manifest and changed nothing else.
    async fn sign_inline_svg(
        &self,
        services: &RuntimeServices,
        job: &EmbeddedJob<'_>,
        fragment: &str,
        work: &mut Deadline<'_>,
        calls: &mut usize,
    ) -> Result<(String, Reason), Reason> {
        let svg = svg_equivalence::signable_svg(fragment, self.max_media_bytes())?;
        let bytes = svg.as_bytes();
        let sha256 = sha256_hex(bytes);
        let source = MediaSource {
            page_url: &job.request.page_url,
            reference: None,
            origin_host: None,
            mime: SVG_MEDIA_TYPE,
            media_type: SVG_MEDIA_TYPE,
            bytes,
            sha256: &sha256,
            link_deliverable: false,
            require_embedded_manifest: false,
        };
        let (signed, reason) = self
            .sign_media(services, &source, job.policy_digest, work, calls)
            .await?;
        embedded::verify_svg_carrier(bytes, &signed.bytes)?;
        let signed =
            String::from_utf8(signed.bytes.to_vec()).map_err(|_| Reason::InlineSvgAltered)?;
        Ok((signed, reason))
    }

    /// The namespace core keeps this credential's signed representations
    /// under: the module version, the policy digest the service answers now,
    /// and the credential context hash. Never a secret. The policy is asked
    /// for the claim's root, being the policy of the credential whatever the
    /// asset, and asked live on every call, never from the article path's
    /// cache, so a revoked credential or changed policy stops every kept copy
    /// at once.
    async fn delivery_namespace(&self, services: &RuntimeServices) -> Result<String, Reason> {
        let mut work = Deadline::new(
            services.timer(),
            Duration::from_millis(self.config.max_hold_ms).saturating_sub(SKIP_WRITE_RESERVE),
        );
        work.start().await;
        let claim_root = format!("https://{}/", self.config.domain);
        let policy_digest = self
            .request_policy(&claim_root, services, &mut work)
            .await?;
        Ok(self.kept_namespace(&policy_digest))
    }

    /// The namespace of signed representations made under `policy_digest`
    /// with this credential, as [`Self::delivery_namespace`] answers it.
    pub(super) fn kept_namespace(&self, policy_digest: &str) -> String {
        format!("{NAMESPACE_VERSION}:{policy_digest}:{}", self.cache_context)
    }

    /// Sign one binary representation core captured, for every response of
    /// it: the reader-facing address must be on the claimed domain, the
    /// bytes the ones core hashed.
    async fn sign_delivery(
        &self,
        services: &RuntimeServices,
        job: &MediaJob<'_>,
    ) -> Result<DeliveredMedia, Reason> {
        let reader_url = url::Url::parse(job.reader_url).map_err(|_| Reason::OutsideClaim)?;
        let host = reader_url.host_str().ok_or(Reason::OutsideClaim)?;
        if !matches!(reader_url.scheme(), "https" | "http")
            || !self.claims_host(host)
            || is_excluded_prefix(reader_url.path())
        {
            return Err(Reason::OutsideClaim);
        }
        if job.source.len() > self.max_media_bytes() {
            return Err(Reason::MediaTooLarge);
        }
        let sha256 = sha256_hex(job.source);
        if sha256 != job.source_sha256 {
            return Err(Reason::SourceDigestMismatch);
        }
        // The host core fetched from, which the endpoint checks the claim's
        // managed origins and the reference against.
        let origin_host = job
            .source_origin_host
            .filter(|host| valid_origin_host(host))
            .ok_or(Reason::InvalidOriginHost)?;
        let media_type = canonical_media_type(job.content_type);
        // No address serves the optimizer's answer, so it is bound by its
        // own bytes and never by the address the optimizer read.
        let reference = job.source_reference.filter(|reference| {
            !job.optimizer_output && safe_source_reference(reference, &self.config.domain)
        });
        let mut work = Deadline::new(
            services.timer(),
            Duration::from_millis(self.config.max_hold_ms).saturating_sub(SKIP_WRITE_RESERVE),
        );
        work.start().await;
        // Asked live, as for the namespace: a cached digest could outlive a
        // revocation and reuse a signed result under it.
        let policy_digest = self
            .request_policy(job.reader_url, services, &mut work)
            .await?;
        let source = MediaSource {
            page_url: job.reader_url,
            reference,
            origin_host: Some(origin_host),
            mime: job.content_type,
            media_type: &media_type,
            bytes: job.source,
            sha256: &sha256,
            link_deliverable: true,
            require_embedded_manifest: job.optimizer_output,
        };
        // One representation, one call at most.
        let mut calls = 0_usize;
        let (signed, reason) = self
            .sign_media(services, &source, &policy_digest, &mut work, &mut calls)
            .await?;
        if media_type == SVG_MEDIA_TYPE && signed.disposition == Disposition::Embedded {
            embedded::verify_svg_carrier(job.source, &signed.bytes)?;
        }
        let mut headers: Vec<ResponseHeader> = Vec::with_capacity(4);
        if signed.disposition == Disposition::External
            && let Some(link) = signed.link()
        {
            headers.push((
                header::LINK,
                HeaderValue::from_str(&link).map_err(|_| Reason::InvalidSignedResponse)?,
            ));
        }
        for (name, value) in [
            (INSTANCE_ID_HEADER, &signed.instance_id),
            (DOCUMENT_ID_HEADER, &signed.document_id),
            (VERIFICATION_URL_HEADER, &signed.verification_url),
        ] {
            headers.push((
                name,
                HeaderValue::from_str(value).map_err(|_| Reason::InvalidSignedResponse)?,
            ));
        }
        log_media(true, reason);
        Ok(DeliveredMedia {
            bytes: signed.bytes,
            content_type: signed.content_type,
            headers,
            // The policy this representation was signed under, so core keeps
            // it where only that policy finds it.
            namespace: self.kept_namespace(&policy_digest),
        })
    }
}

/// Marks every signed outcome of a batch whose output would exceed the
/// document budget as unsigned: none of its edits is applied.
fn revoke(outcomes: &mut [Outcome]) {
    for outcome in outcomes.iter_mut() {
        if outcome.is_ok() {
            *outcome = Err(Reason::OutputTooLarge);
        }
    }
}

/// The version of the module's signed representations, part of every
/// namespace core keeps them under.
const NAMESPACE_VERSION: &str = concat!("media-v1:", env!("CARGO_PKG_VERSION"));

/// The module's binary media signer, which core runs on publisher and asset
/// proxy responses of the media it captures.
pub(crate) struct EncypherMedia {
    signer: Arc<Signer>,
}

impl EncypherMedia {
    pub(crate) fn new(signer: Arc<Signer>) -> Self {
        Self { signer }
    }
}

impl std::fmt::Debug for EncypherMedia {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EncypherMedia")
            .finish_non_exhaustive()
    }
}

impl MediaSigner for EncypherMedia {
    fn signer_id(&self) -> &'static str {
        MODULE
    }

    /// Every path but an API, feed, admin or well-known path: media is
    /// recognized by the origin's media type, not by a suffix.
    fn handles_path(&self, path: &str) -> bool {
        !is_excluded_prefix(path)
    }

    fn handles_media_type(&self, media_type: &str) -> bool {
        media_type != MEDIA_SEGMENT_TYPE
            && SIGNABLE_TYPE_PREFIXES
                .iter()
                .any(|prefix| media_type.starts_with(prefix) && media_type.len() > prefix.len())
    }

    fn max_source_bytes(&self) -> usize {
        self.signer.max_media_bytes()
    }

    /// Larger media, up to this, is signed through the prepared lane: hashed
    /// where it is delivered and never uploaded.
    fn max_large_source_bytes(&self) -> u64 {
        self.signer.max_large_media_bytes()
    }

    /// The one budget for a large source: core's whole-source hash and the
    /// prepared lane together.
    fn max_large_media_duration(&self) -> Duration {
        self.signer.max_large_media_duration()
    }

    fn sign_large<'a>(
        &'a self,
        services: &'a RuntimeServices,
        job: &'a LargeMediaJob<'a>,
    ) -> LargeMediaSigning<'a> {
        Box::pin(async move {
            match self.signer.sign_large(services, job).await {
                Ok(signed) => {
                    log_media(true, Reason::Signed);
                    Ok(signed)
                }
                Err(refusal) => {
                    // A source with its own embedded credentials is delivered
                    // as it is: its provenance is kept, it is not unsigned
                    // by failure and not newly signed.
                    log::info!(
                        "encypher: media outcome={} lane=large reason={}",
                        if refusal.retains_provenance() {
                            "retained"
                        } else {
                            "unsigned"
                        },
                        refusal.code()
                    );
                    Err(MediaSigningFailure {
                        reason: refusal.code(),
                    })
                }
            }
        })
    }

    fn namespace<'a>(&'a self, services: &'a RuntimeServices) -> MediaNamespacing<'a> {
        Box::pin(async move {
            self.signer
                .delivery_namespace(services)
                .await
                .map_err(|reason| {
                    log_media(false, reason);
                    MediaSigningFailure {
                        reason: reason.code(),
                    }
                })
        })
    }

    fn sign<'a>(
        &'a self,
        services: &'a RuntimeServices,
        job: &'a MediaJob<'a>,
    ) -> MediaSigning<'a> {
        Box::pin(async move {
            self.signer
                .sign_delivery(services, job)
                .await
                .map_err(|reason| {
                    log_media(false, reason);
                    MediaSigningFailure {
                        reason: reason.code(),
                    }
                })
        })
    }
}

#[cfg(test)]
mod tests;
