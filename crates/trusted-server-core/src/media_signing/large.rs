//! The large-media lane: a representation larger than a signer's bounded
//! limit, signed without core ever holding it whole.
//!
//! The lane signs an unchanged source only. The bytes the reader receives
//! are the origin's own, so the origin's representation keeps its own
//! validators, length and range handling, and the signer only adds headers,
//! such as a manifest `Link`. Core adds a `Repr-Digest` of the whole source.
//!
//! Core admits an origin answer to the lane only where it can pin one
//! version of the source and keep the outcome: a shared request, an
//! identity-coded answer the origin keeps shareable for a while and lets
//! be transformed, a cache that keeps values, a single strong `ETag`, a
//! known total length above the signer's bounded limit and within its
//! large limit, a platform that streams response bodies and offers a
//! timer, and no C2PA manifest `Link` of the origin's own. Before anything
//! is hashed, core also requires a public address to name the source by:
//! the fetched address where it is plain `https` and core does not sign
//! the request for it, or else the reader's query-free `https` address.
//! An answer that fails any of these goes to the reader as the origin sent
//! it, validators and all; an answer with no strong `ETag` is never given
//! an invented version. Only a reader's `GET` of an answer whose origin
//! says `Accept-Ranges: bytes` starts the hashing of a version; a `HEAD`,
//! or a `GET` without that, is answered from a kept outcome or goes as it
//! came.
//!
//! Every later read of a version being signed, by core or the signer, asks
//! the origin through the route's own backend and origin signing for
//! exactly one byte range with `If-Match` on the pinned tag, and refuses an
//! answer whose status, `Content-Range`, `Content-Length`, coding or `ETag`
//! does not describe exactly that range of exactly that version. A changed
//! version therefore fails the read, never mixes into a digest or a signed
//! delivery. A version the lane declined has no signature to protect, so
//! where the origin's first answer can no longer be sent its reader gets
//! one fresh origin answer to the request as core first sent it, held by
//! no cache.
//!
//! What core keeps for the lane is the outcome only, never the bytes: for
//! a signed version the whole-source digest, the length and the signer's
//! headers, for a declined one (refused, failed or out of time) that it was
//! declined, briefly. Both are kept under the publisher's prefix, the
//! signer's current [`namespace`](super::MediaSigner::namespace), the
//! reader's address, the origin and the pinned tag, for no longer than the
//! origin's shared freshness. A changed or revoked policy names another
//! namespace, so it never finds an outcome kept under the old one. The
//! hashing and signing run as one load of the loading cache, which runs a
//! key's load once for every caller asking for it at the same time.

use std::fmt;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use bytes::{Bytes, BytesMut};
use edgezero_core::body::Body as EdgeBody;
use futures::StreamExt as _;
use futures::future::Either;
use http::{HeaderMap, HeaderName, HeaderValue, Method, Request, Response, StatusCode, header};
use sha2::{Digest as _, Sha256};
use web_time::Instant;

use super::{
    Delivery, KeptReader, MediaClaim, MediaSigner, MediaSigningFailure, OriginFacts, Outcome,
    VariantIdentity, carries_manifest_link, digits, evaluate, header_text, is_reserved_header,
    single_entity_tag, source_reference, strip_representation_preconditions, to_u64,
};
use crate::middleware::ResponseHeader;
use crate::origin_auth::OriginSigning;
use crate::platform::{
    LoadError, LoadedValue, Loader, PlatformHttpRequest, RuntimeServices, StoreKey,
};
use crate::settings::ASSETS_CACHE;

/// What [`MediaSigner::sign_large`](super::MediaSigner::sign_large) returns.
pub type LargeMediaSigning<'a> =
    Pin<Box<dyn Future<Output = Result<LargeSignedMedia, MediaSigningFailure>> + 'a>>;

/// One large, unchanged representation a signer is asked to sign.
#[derive(Debug, Clone, Copy)]
pub struct LargeMediaJob<'a> {
    /// The address the reader asked for, `scheme://host/path`, without its
    /// query.
    pub reader_url: &'a str,
    /// The origin's `Content-Type`, as it sent it.
    pub content_type: &'a str,
    /// The pinned version of the source, read at offsets.
    pub source: &'a VersionedMediaSource<'a>,
    /// SHA-256 of every byte of the pinned version, lower case hex, which
    /// core computed in bounded chunks and checked against its length. The
    /// bytes the reader receives are these bytes.
    pub source_sha256: &'a str,
    /// The address core fetched the source from, exactly as fetched, where
    /// it is `https` and carries no user information, query or fragment.
    pub source_reference: Option<&'a str>,
    /// The reader's address, where it is `https` and the reader's request
    /// carried no query. It names the source only because this lane
    /// delivers the source unchanged.
    pub reader_reference: Option<&'a str>,
    /// The lower case host of the address core fetched the source from,
    /// with no scheme, port, path, query or user information.
    pub source_origin_host: &'a str,
    /// What is left of the signer's
    /// [`max_large_media_duration`](super::MediaSigner::max_large_media_duration)
    /// after core's hashing. Core drops the signing when it is spent.
    pub remaining: Duration,
}

/// What a signer returns for a large representation: headers only, since
/// the bytes are the origin's.
#[derive(Debug, Clone)]
pub struct LargeSignedMedia {
    /// Headers written on every answer of the signed version: `200`, `206`,
    /// `HEAD` and `304`. A `Link` is added to the origin's own links, and
    /// any other replaces the origin's header of that name. A name core
    /// owns, see [`is_reserved_header`], fails the signing.
    pub headers: Vec<ResponseHeader>,
    /// The policy and credential context the source was signed under, as
    /// [`MediaSigner::namespace`](super::MediaSigner::namespace) gives it
    /// while that context is current. Core keeps the metadata under it.
    pub namespace: String,
}

/// One version of a source: its strong entity tag and its length.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceVersion {
    /// The strong tag, quoted, as `"opaque"`.
    etag: String,
    length: u64,
}

impl SourceVersion {
    /// The version a single strong entity tag `etag` and a length name, or
    /// `None` where `etag` is weak, empty or not one tag.
    #[must_use]
    pub fn new(etag: &str, length: u64) -> Option<Self> {
        let tag = single_entity_tag(etag)?;
        (!tag.weak && !tag.opaque.is_empty()).then(|| Self {
            etag: format!("\"{}\"", tag.opaque),
            length,
        })
    }

    /// The version an origin answer's headers name, where they carry
    /// exactly one strong `ETag`.
    fn of(headers: &HeaderMap, length: u64) -> Option<Self> {
        let mut tags = headers.get_all(header::ETAG).iter();
        let (Some(tag), None) = (tags.next(), tags.next()) else {
            return None;
        };
        Self::new(tag.to_str().ok()?, length)
    }

    /// The strong entity tag, quoted.
    #[must_use]
    pub fn etag(&self) -> &str {
        &self.etag
    }

    /// The length in bytes.
    #[must_use]
    pub fn length(&self) -> u64 {
        self.length
    }

    /// Whether an answer's headers carry this version's tag, strongly.
    fn matches(&self, headers: &HeaderMap) -> bool {
        let mut tags = headers.get_all(header::ETAG).iter();
        let (Some(tag), None) = (tags.next(), tags.next()) else {
            return false;
        };
        tag.to_str()
            .ok()
            .and_then(single_entity_tag)
            .is_some_and(|tag| {
                !tag.weak && self.etag.get(1..self.etag.len() - 1) == Some(tag.opaque)
            })
    }
}

/// Why a read of a [`VersionedMediaSource`] failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, derive_more::Display)]
pub enum MediaSourceError {
    /// The origin no longer serves the pinned version: it answered `412` to
    /// `If-Match`, or with a missing, weak or different `ETag`. Nothing
    /// read from this source may be published.
    #[display("the origin no longer serves the pinned version")]
    VersionChanged,
    /// The read reaches past the pinned version's length.
    #[display("the read reaches past the pinned version")]
    OutOfBounds,
    /// The origin answered a pinned read with something other than exactly
    /// the bytes asked for.
    #[display("the origin answered a pinned read wrongly: {reason}")]
    InvalidAnswer {
        /// A short code naming what was wrong.
        reason: &'static str,
    },
    /// The read could not be sent, or its body failed or ended early.
    #[display("a pinned read could not be completed")]
    Transport,
}

impl core::error::Error for MediaSourceError {}

impl MediaSourceError {
    /// A short code naming the failure, for logs and refusals.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::VersionChanged => "source_version_changed",
            Self::OutOfBounds => "source_read_out_of_bounds",
            Self::InvalidAnswer { reason } => reason,
            Self::Transport => "source_transport",
        }
    }
}

/// How core asks the origin for one more byte range of a source: the
/// address, the request's headers as they stood before it was signed, the
/// backend and the origin's signing.
#[derive(Clone, Copy)]
pub(crate) struct PinnedOrigin<'a> {
    /// The address, exactly as fetched and signed.
    pub(crate) url: &'a url::Url,
    /// The origin request's headers before signing.
    pub(crate) headers: &'a HeaderMap,
    /// The backend the origin request went to.
    pub(crate) backend_name: &'a str,
    /// The origin's signing, applied fresh to every read.
    pub(crate) auth: Option<&'a OriginSigning>,
    /// Headers a signed read must not carry, removed before core writes its
    /// own conditions and signs.
    pub(crate) unsigned_headers: &'a [HeaderName],
    /// Publisher URL mapping for a fresh unsigned response; asset proxies keep
    /// the origin's navigation targets as on their ordinary response path.
    pub(crate) response_header_rewrite:
        Option<crate::response_header_rewrite::OriginHeaderRewrite<'a>>,
}

/// One pinned version of a source, read at offsets from the origin.
///
/// Every read asks the origin for exactly one byte range with `If-Match`
/// on the pinned tag, through the route's own backend and origin signing,
/// and fails unless the answer is exactly that range of that version.
pub struct VersionedMediaSource<'a> {
    services: &'a RuntimeServices,
    origin: PinnedOrigin<'a>,
    version: SourceVersion,
}

impl fmt::Debug for VersionedMediaSource<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VersionedMediaSource")
            .field("version", &self.version)
            .finish_non_exhaustive()
    }
}

impl<'a> VersionedMediaSource<'a> {
    /// A source read from `url` through `backend_name` with `headers`, an
    /// origin that signs no request, pinned to `version`.
    #[must_use]
    pub fn new(
        services: &'a RuntimeServices,
        url: &'a url::Url,
        headers: &'a HeaderMap,
        backend_name: &'a str,
        version: SourceVersion,
    ) -> Self {
        Self {
            services,
            origin: PinnedOrigin {
                url,
                headers,
                backend_name,
                auth: None,
                unsigned_headers: &[],
                response_header_rewrite: None,
            },
            version,
        }
    }

    /// The version every read is pinned to.
    #[must_use]
    pub fn version(&self) -> &SourceVersion {
        &self.version
    }

    /// The pinned version's length in bytes.
    #[must_use]
    pub fn size(&self) -> u64 {
        self.version.length
    }

    /// Fills `destination` with the bytes at `offset` of the pinned version.
    ///
    /// # Errors
    ///
    /// [`MediaSourceError::OutOfBounds`] where the read reaches past the
    /// version, [`MediaSourceError::VersionChanged`] where the origin no
    /// longer serves it, and the other variants where the origin's answer
    /// is not exactly the range asked for or does not arrive whole.
    ///
    /// # Performance
    ///
    /// One origin request per call, streamed into `destination`.
    pub async fn read_exact_at(
        &self,
        offset: u64,
        destination: &mut [u8],
    ) -> Result<(), MediaSourceError> {
        let length = to_u64(destination.len());
        let mut filled = 0_usize;
        let mut visit = |chunk: &[u8]| {
            let end = filled.saturating_add(chunk.len());
            if let Some(slot) = destination.get_mut(filled..end) {
                slot.copy_from_slice(chunk);
            }
            filled = end;
        };
        self.stream_range(offset, length, &mut visit).await
    }

    /// Hands `visit` the bytes from `offset` for `length` bytes of the
    /// pinned version, in order, in the chunks the origin sends.
    ///
    /// # Errors
    ///
    /// As [`read_exact_at`](Self::read_exact_at). `visit` may have seen
    /// some of the bytes before a read fails.
    ///
    /// # Performance
    ///
    /// One origin request; nothing is held beyond one chunk.
    pub async fn for_each_chunk<F: FnMut(&[u8])>(
        &self,
        offset: u64,
        length: u64,
        mut visit: F,
    ) -> Result<(), MediaSourceError> {
        self.stream_range(offset, length, &mut visit).await
    }

    /// Whether the origin still serves the pinned version, by a pinned
    /// read of its first byte. A signer asks this last before it publishes.
    ///
    /// # Errors
    ///
    /// As [`read_exact_at`](Self::read_exact_at).
    pub async fn confirm_current(&self) -> Result<(), MediaSourceError> {
        let mut first = [0_u8; 1];
        self.read_exact_at(0, &mut first).await
    }

    /// The same source read through `origin`, which may sign its reads.
    pub(crate) fn pinned(
        services: &'a RuntimeServices,
        origin: PinnedOrigin<'a>,
        version: SourceVersion,
    ) -> Self {
        Self {
            services,
            origin,
            version,
        }
    }

    /// Streams `length` bytes from `offset` to `visit`.
    async fn stream_range<F: FnMut(&[u8])>(
        &self,
        offset: u64,
        length: u64,
        visit: &mut F,
    ) -> Result<(), MediaSourceError> {
        if length == 0 {
            return Ok(());
        }
        let last = offset
            .checked_add(length - 1)
            .filter(|last| *last < self.version.length)
            .ok_or(MediaSourceError::OutOfBounds)?;
        let body = self.send_pinned(offset, last).await?;
        drain(body, length, visit).await
    }

    /// Asks the origin for bytes `first` to `last` of the pinned version
    /// and returns the body of an answer that is exactly that.
    async fn send_pinned(&self, first: u64, last: u64) -> Result<EdgeBody, MediaSourceError> {
        let request = self.pinned_request(first, last)?;
        // The platform's error may name the address, which can carry a
        // credential, so it is not logged.
        let answer = self
            .services
            .http_client()
            .send(request)
            .await
            .map_err(|_| MediaSourceError::Transport)?;
        let (parts, body) = answer.response.into_parts();
        check_pinned(&self.version, parts.status, &parts.headers, first, last)?;
        Ok(body)
    }

    /// The origin request for bytes `first` to `last`: the request as it
    /// stood before signing, with every condition the reader sent removed,
    /// and only core's own `If-Match`, `Range` and identity coding written
    /// before it is signed afresh.
    fn pinned_request(
        &self,
        first: u64,
        last: u64,
    ) -> Result<PlatformHttpRequest, MediaSourceError> {
        let mut headers = self.origin.base_headers();
        strip_representation_preconditions(&mut headers);
        headers.insert(
            header::ACCEPT_ENCODING,
            HeaderValue::from_static("identity"),
        );
        headers.insert(
            header::IF_MATCH,
            HeaderValue::from_str(&self.version.etag).map_err(|_| {
                MediaSourceError::InvalidAnswer {
                    reason: "etag_not_a_header",
                }
            })?,
        );
        headers.insert(
            header::RANGE,
            HeaderValue::from_str(&format!("bytes={first}-{last}"))
                .expect("should build a byte range"),
        );
        let request = self
            .origin
            .signed_request(&Method::GET, headers)
            .map_err(|()| MediaSourceError::Transport)?;
        Ok(
            if self.services.http_client().supports_streaming_responses() {
                request.with_stream_response()
            } else {
                request.with_max_response_body_bytes(
                    usize::try_from(last - first + 1).unwrap_or(usize::MAX),
                )
            },
        )
    }
}

impl PinnedOrigin<'_> {
    /// The origin request's headers as they stood before signing, less
    /// the headers a signed request must not carry where core signs it.
    fn base_headers(&self) -> HeaderMap {
        let mut headers = self.headers.clone();
        if self.auth.is_some() {
            for name in self.unsigned_headers {
                headers.remove(name);
            }
        }
        headers
    }

    /// A `method` request to the origin carrying `headers`, signed afresh
    /// where the origin signs requests. Nothing is written after the
    /// signature.
    fn signed_request(
        &self,
        method: &Method,
        mut headers: HeaderMap,
    ) -> Result<PlatformHttpRequest, ()> {
        if let Some(auth) = self.auth {
            auth.remove_namespace_headers(&mut headers);
            auth.sign(method, self.url, &mut headers).map_err(|_| ())?;
        }
        let mut request = Request::builder()
            .method(method.clone())
            .uri(self.url.as_str())
            .body(EdgeBody::empty())
            .map_err(|_| ())?;
        *request.headers_mut() = headers;
        Ok(PlatformHttpRequest::new(request, self.backend_name))
    }
}

/// Checks that an answer to a pinned read of bytes `first` to `last` is
/// exactly that range of `version`.
fn check_pinned(
    version: &SourceVersion,
    status: StatusCode,
    headers: &HeaderMap,
    first: u64,
    last: u64,
) -> Result<(), MediaSourceError> {
    if status == StatusCode::PRECONDITION_FAILED {
        return Err(MediaSourceError::VersionChanged);
    }
    if status != StatusCode::OK && status != StatusCode::PARTIAL_CONTENT {
        return Err(MediaSourceError::InvalidAnswer {
            reason: "pinned_read_status",
        });
    }
    if !version.matches(headers) {
        return Err(MediaSourceError::VersionChanged);
    }
    let encoding = header_text(headers, &header::CONTENT_ENCODING).trim();
    if !encoding.is_empty() && !encoding.eq_ignore_ascii_case("identity") {
        return Err(MediaSourceError::InvalidAnswer {
            reason: "pinned_read_content_encoded",
        });
    }
    let whole = first == 0 && last.checked_add(1) == Some(version.length);
    if status == StatusCode::PARTIAL_CONTENT {
        if content_range(headers) != Some((first, last, version.length)) {
            return Err(MediaSourceError::InvalidAnswer {
                reason: "pinned_read_content_range",
            });
        }
    } else if !whole {
        return Err(MediaSourceError::InvalidAnswer {
            reason: "pinned_read_status",
        });
    }
    let declared = header_text(headers, &header::CONTENT_LENGTH).trim();
    if !declared.is_empty() && digits(declared) != Some(last - first + 1) {
        return Err(MediaSourceError::InvalidAnswer {
            reason: "pinned_read_content_length",
        });
    }
    Ok(())
}

/// The first byte, last byte and complete length a `206`'s
/// `Content-Range: bytes first-last/length` names.
fn content_range(headers: &HeaderMap) -> Option<(u64, u64, u64)> {
    let value = header_text(headers, &header::CONTENT_RANGE).trim();
    let (unit, range) = value.split_once(' ')?;
    if !unit.eq_ignore_ascii_case("bytes") {
        return None;
    }
    let (span, total) = range.trim().split_once('/')?;
    let (first, last) = span.split_once('-')?;
    Some((
        digits(first.trim())?,
        digits(last.trim())?,
        digits(total.trim())?,
    ))
}

/// Hands `visit` every chunk of `body`, which must hold exactly `expected`
/// bytes. A chunk that would pass `expected` is refused before `visit`
/// sees it. A stream that fails or ends early is a
/// [`MediaSourceError::Transport`].
async fn drain<F: FnMut(&[u8])>(
    body: EdgeBody,
    expected: u64,
    visit: &mut F,
) -> Result<(), MediaSourceError> {
    match body {
        EdgeBody::Once(bytes) => {
            if to_u64(bytes.len()) != expected {
                return Err(MediaSourceError::InvalidAnswer {
                    reason: "pinned_read_body_length",
                });
            }
            visit(&bytes);
            Ok(())
        }
        EdgeBody::Stream(mut stream) => {
            let mut seen = 0_u64;
            while let Some(item) = stream.next().await {
                let Ok(chunk) = item else {
                    return Err(MediaSourceError::Transport);
                };
                let next = seen.saturating_add(to_u64(chunk.len()));
                if next > expected {
                    return Err(MediaSourceError::InvalidAnswer {
                        reason: "pinned_read_overlong_body",
                    });
                }
                visit(&chunk);
                seen = next;
            }
            if seen == expected {
                Ok(())
            } else {
                Err(MediaSourceError::Transport)
            }
        }
    }
}

/// SHA-256 of every byte of `source`'s pinned version, read once by one
/// pinned read in bounded chunks. A read that fails fails the digest; it is
/// never restarted or resumed.
async fn whole_digest(source: &VersionedMediaSource<'_>) -> Result<[u8; 32], MediaSourceError> {
    let mut hasher = Sha256::new();
    let mut visit = |chunk: &[u8]| hasher.update(chunk);
    source.stream_range(0, source.size(), &mut visit).await?;
    let mut digest = [0_u8; 32];
    digest.copy_from_slice(&hasher.finalize());
    Ok(digest)
}

/// `body` as sent to the reader, refused where it holds more or fewer than
/// `expected` bytes, so a reader never receives bytes of another length
/// under headers that describe this one.
fn exact_body(body: EdgeBody, expected: u64) -> Result<EdgeBody, MediaSourceError> {
    match body {
        EdgeBody::Once(bytes) if to_u64(bytes.len()) == expected => Ok(EdgeBody::Once(bytes)),
        EdgeBody::Once(_) => Err(MediaSourceError::InvalidAnswer {
            reason: "pinned_read_body_length",
        }),
        EdgeBody::Stream(stream) => Ok(EdgeBody::from_stream(futures::stream::unfold(
            (stream, 0_u64, false),
            move |(mut stream, sent, finished)| async move {
                if finished {
                    return None;
                }
                match stream.next().await {
                    Some(Ok(chunk)) => {
                        let sent = sent.saturating_add(to_u64(chunk.len()));
                        if sent > expected {
                            let error = MediaSourceError::InvalidAnswer {
                                reason: "pinned_read_overlong_body",
                            };
                            Some((Err(error), (stream, sent, true)))
                        } else {
                            Some((Ok(chunk), (stream, sent, false)))
                        }
                    }
                    Some(Err(_)) => Some((Err(MediaSourceError::Transport), (stream, sent, true))),
                    None if sent == expected => None,
                    None => Some((Err(MediaSourceError::Transport), (stream, sent, true))),
                }
            },
        ))),
    }
}

/// The first bytes of kept metadata for a signed version, naming its
/// layout.
const SIGNED_MAGIC: &[u8; 4] = b"TSL1";

/// The whole of the kept record for a version the lane declined: refused
/// by the signer, failed to read or hash, or out of time.
const DECLINED_MAGIC: &[u8; 4] = b"TSL0";

/// The longest metadata for a signed version is kept, whatever the origin
/// allows.
const SIGNED_MAX_LIFETIME: Duration = Duration::from_secs(3600);

/// The longest a declined version is kept, so a source, policy or signer
/// that changes is tried again soon, while a burst of requests for one
/// version never hashes it once per request.
const DECLINED_MAX_LIFETIME: Duration = Duration::from_secs(60);

/// How long a lookup may take before the origin's own answer, held unread
/// meanwhile, is no longer trusted to stream, and the bytes are read again
/// pinned. A lookup that waits on another caller's load can take as long as
/// that load.
const ORIGIN_BODY_MAX_WAIT: Duration = Duration::from_secs(1);

/// The most signer headers kept metadata may hold.
const LARGE_MAX_HEADERS: usize = 64;

/// What core keeps for a signed large version: never its bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LargeRecord {
    length: u64,
    sha256: [u8; 32],
    headers: Vec<ResponseHeader>,
}

/// What core keeps for one large version under one namespace.
#[derive(Debug, Clone, PartialEq, Eq)]
enum KeptOutcome {
    /// The version was signed; its answers carry these headers.
    Signed(LargeRecord),
    /// The version was not signed; its answers go as the origin made them.
    Declined,
}

/// Metadata for a signed version as written: [`SIGNED_MAGIC`], the length,
/// the digest, the header count, then each header's name and value with
/// their lengths.
fn encode_large(record: &LargeRecord) -> Bytes {
    let mut out = BytesMut::with_capacity(128);
    out.extend_from_slice(SIGNED_MAGIC);
    out.extend_from_slice(&record.length.to_be_bytes());
    out.extend_from_slice(&record.sha256);
    let count = record.headers.len().min(LARGE_MAX_HEADERS);
    out.extend_from_slice(&u32::try_from(count).unwrap_or(u32::MAX).to_be_bytes());
    for (name, value) in record.headers.iter().take(count) {
        for field in [name.as_str().as_bytes(), value.as_bytes()] {
            out.extend_from_slice(&u32::try_from(field.len()).unwrap_or(u32::MAX).to_be_bytes());
            out.extend_from_slice(field);
        }
    }
    out.freeze()
}

/// A kept outcome as [`encode_large`] or [`DECLINED_MAGIC`] wrote it, or
/// `None` for any other bytes.
fn decode_kept_outcome(bytes: &[u8]) -> Option<KeptOutcome> {
    if bytes == DECLINED_MAGIC.as_slice() {
        return Some(KeptOutcome::Declined);
    }
    let mut reader = KeptReader {
        rest: bytes.strip_prefix(SIGNED_MAGIC.as_slice())?,
    };
    let length = reader.number()?;
    let sha256: [u8; 32] = reader.take(32)?.try_into().ok()?;
    let count = reader.length()?;
    if count > LARGE_MAX_HEADERS {
        return None;
    }
    let mut headers = Vec::with_capacity(count);
    for _ in 0..count {
        let name_length = reader.length()?;
        let name = HeaderName::from_bytes(reader.take(name_length)?).ok()?;
        let value_length = reader.length()?;
        let value = HeaderValue::from_bytes(reader.take(value_length)?).ok()?;
        if is_reserved_header(&name) {
            return None;
        }
        headers.push((name, value));
    }
    reader
        .rest
        .is_empty()
        .then_some(KeptOutcome::Signed(LargeRecord {
            length,
            sha256,
            headers,
        }))
}

/// Writes a signed version's headers on an answer of it: the signer's,
/// with a `Link` added to the origin's, and the whole source's
/// `Repr-Digest`.
fn decorate(headers: &mut HeaderMap, record: &LargeRecord) {
    for (name, value) in &record.headers {
        if name == header::LINK {
            headers.append(name.clone(), value.clone());
        } else {
            headers.insert(name.clone(), value.clone());
        }
    }
    let digest = format!(
        "sha-256=:{}:",
        base64::engine::general_purpose::STANDARD.encode(record.sha256)
    );
    headers.insert(
        HeaderName::from_static("repr-digest"),
        HeaderValue::from_str(&digest).expect("should build a base64 digest"),
    );
}

/// The answer where a signed or meant-to-be-signed version can no longer
/// be read: nothing of another version is sent in its place.
fn refusal() -> Response<EdgeBody> {
    Response::builder()
        .status(StatusCode::SERVICE_UNAVAILABLE)
        .header(header::CACHE_CONTROL, "no-store")
        .header(header::RETRY_AFTER, "1")
        .header(header::CONTENT_LENGTH, 0)
        .body(EdgeBody::empty())
        .expect("should build a refusal")
}

/// The headers a fresh origin answer for an unsigned version brings: its
/// status framing, type, coding, validators and redirect/authentication
/// semantics. Every other header stays as core prepared it for the reader.
const FRESH_REPRESENTATION_HEADERS: [&str; 16] = [
    "content-type",
    "content-location",
    "content-length",
    "content-encoding",
    "content-range",
    "accept-ranges",
    "etag",
    "last-modified",
    "repr-digest",
    "content-digest",
    "digest",
    "content-md5",
    "transfer-encoding",
    "location",
    "www-authenticate",
    "retry-after",
];

/// Whether the origin says it serves byte ranges of this representation,
/// which every pinned read relies on.
fn advertises_byte_ranges(headers: &HeaderMap) -> bool {
    headers
        .get_all(header::ACCEPT_RANGES)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .any(|unit| unit.trim().eq_ignore_ascii_case("bytes"))
}

/// The answer where the origin could not be asked again for an unsigned
/// version.
fn origin_unreachable() -> Response<EdgeBody> {
    Response::builder()
        .status(StatusCode::BAD_GATEWAY)
        .header(header::CACHE_CONTROL, "no-store")
        .header(header::CONTENT_LENGTH, 0)
        .body(EdgeBody::empty())
        .expect("should build a gateway failure")
}

/// An origin answer admitted to the large lane, with the version it pins.
pub(crate) struct LargeAnswer {
    response: Response<EdgeBody>,
    version: SourceVersion,
    /// How long the origin let its answer be kept, shared, which bounds
    /// how long its outcome is kept.
    keep_for: Duration,
}

impl LargeAnswer {
    /// The origin's answer as it came, for a caller that cannot say how to
    /// ask the origin again, sent unsigned.
    pub(crate) fn into_origin_response(self) -> Response<EdgeBody> {
        self.response
    }
}

/// The one wall-clock budget for a large source's namespace resolution,
/// cache waits, hashing and signing.
#[derive(Debug, Clone, Copy)]
struct LargeBudget {
    started: Instant,
    total: Duration,
}

impl LargeBudget {
    /// What is left of the budget now.
    fn remaining(self) -> Duration {
        self.total.saturating_sub(self.started.elapsed())
    }
}

/// The public addresses a large source may be named by, worked out before
/// anything is hashed.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LargeReferences {
    /// The fetched address, where it is `https`, carries no user
    /// information, query or fragment, and core does not sign the request
    /// for it: an address core signs is a private one and is never named.
    source: Option<String>,
    /// The reader's address, where it is `https` and the reader's request
    /// carried no query.
    reader: Option<String>,
}

impl LargeReferences {
    fn of(identity: &VariantIdentity<'_>, origin_signed: bool) -> Self {
        let source = if origin_signed {
            None
        } else {
            source_reference(identity.fetched_url)
                .filter(|reference| reference.starts_with("https://"))
                .map(str::to_owned)
        };
        let reader = (identity.reader_query_free && identity.reader_url.starts_with("https://"))
            .then(|| identity.reader_url.to_owned());
        Self { source, reader }
    }

    fn any(&self) -> bool {
        self.source.is_some() || self.reader.is_some()
    }
}

/// Everything the hashing and signing of one large version owns, because
/// the loading cache keeps the load with its key rather than borrowing the
/// request, and runs it once for every caller asking for that key at the
/// same time.
struct LargeLoad {
    signer: Arc<dyn MediaSigner>,
    /// The request's services without its connection, since the outcome
    /// serves every caller waiting on the load.
    services: RuntimeServices,
    url: url::Url,
    headers: HeaderMap,
    backend_name: String,
    auth: Option<OriginSigning>,
    unsigned_headers: Vec<HeaderName>,
    version: SourceVersion,
    reader_url: String,
    content_type: String,
    references: LargeReferences,
    /// The namespace the outcome is kept under, which the signer must sign
    /// under.
    namespace: String,
    budget: LargeBudget,
    keep_for: Duration,
}

impl LargeLoad {
    fn into_loader(self) -> Loader {
        Box::new(move || Box::pin(async move { Ok(self.run().await) }))
    }

    /// Hashes and signs within the budget, giving the outcome to keep. A
    /// declined outcome is kept briefly too, so the version is not hashed
    /// again for every request while it stands.
    async fn run(self) -> LoadedValue {
        let signer_id = self.signer.signer_id();
        let remaining = self.budget.remaining();
        let outcome = match self.services.timer() {
            Some(_) if remaining.is_zero() => {
                log::warn!(
                    "media signing: outcome=unsigned lane=large signer={signer_id} reason=timeout"
                );
                None
            }
            Some(timer) => {
                match futures::future::select(Box::pin(self.sign()), timer.sleep(remaining)).await {
                    Either::Left((outcome, _)) => outcome,
                    Either::Right(((), _)) => {
                        log::warn!(
                            "media signing: outcome=unsigned lane=large signer={signer_id} reason=timeout"
                        );
                        None
                    }
                }
            }
            None => {
                log::warn!(
                    "media signing: outcome=unsigned lane=large signer={signer_id} reason=no_timer"
                );
                None
            }
        };
        match outcome {
            Some(record) => LoadedValue::new(encode_large(&record))
                .with_lifetime(self.keep_for.min(SIGNED_MAX_LIFETIME)),
            None => LoadedValue::new(Bytes::from_static(DECLINED_MAGIC))
                .with_lifetime(self.keep_for.min(DECLINED_MAX_LIFETIME)),
        }
    }

    /// Hashes the pinned version once and asks the signer for it.
    async fn sign(&self) -> Option<LargeRecord> {
        let signer_id = self.signer.signer_id();
        let declined = |reason: &str| {
            log::warn!(
                "media signing: outcome=unsigned lane=large signer={signer_id} reason={reason}"
            );
        };
        let origin = PinnedOrigin {
            url: &self.url,
            headers: &self.headers,
            backend_name: &self.backend_name,
            auth: self.auth.as_ref(),
            unsigned_headers: &self.unsigned_headers,
            response_header_rewrite: None,
        };
        let source = VersionedMediaSource::pinned(&self.services, origin, self.version.clone());
        let digest = whole_digest(&source)
            .await
            .map_err(|error| declined(error.code()))
            .ok()?;
        let Some(origin_host) = self.url.host_str().map(str::to_ascii_lowercase) else {
            declined("no_origin_host");
            return None;
        };
        let remaining = self.budget.remaining();
        if remaining.is_zero() {
            declined("timeout");
            return None;
        }
        let source_sha256 = hex::encode(digest);
        let job = LargeMediaJob {
            reader_url: &self.reader_url,
            content_type: &self.content_type,
            source: &source,
            source_sha256: &source_sha256,
            source_reference: self.references.source.as_deref(),
            reader_reference: self.references.reader.as_deref(),
            source_origin_host: &origin_host,
            remaining,
        };
        let signed = match self.signer.sign_large(&self.services, &job).await {
            Ok(signed)
                if signed
                    .headers
                    .iter()
                    .any(|(name, _)| is_reserved_header(name)) =>
            {
                declined("reserved_header");
                return None;
            }
            Ok(signed) if signed.headers.len() > LARGE_MAX_HEADERS => {
                declined("too_many_headers");
                return None;
            }
            Ok(signed) if signed.namespace != self.namespace => {
                declined("namespace_changed");
                return None;
            }
            Ok(signed) => signed,
            Err(failure) => {
                declined(failure.reason);
                return None;
            }
        };
        log::info!(
            "media signing: outcome=signed lane=large signer={signer_id} source_bytes={}",
            source.size()
        );
        Some(LargeRecord {
            length: source.size(),
            sha256: digest,
            headers: signed.headers,
        })
    }
}

impl MediaClaim {
    /// Whether this claim may use the large lane at all: the signer signs
    /// large sources and the request is shared.
    fn large_lane_open(&self) -> bool {
        self.shared && self.signer.max_large_source_bytes() > 0
    }

    /// The version a signable origin answer of `total` bytes, larger than
    /// the bounded limit, pins in the large lane, or why it cannot. The
    /// answer's coding, privacy, transform rule, type and any manifest
    /// `Link` of the origin's own were already checked, see
    /// `unsignable_reason`. Every check here is cheap and runs before
    /// anything is hashed.
    fn large_version(
        &self,
        services: &RuntimeServices,
        headers: &HeaderMap,
        total: Option<u64>,
        facts: &OriginFacts,
    ) -> Result<SourceVersion, &'static str> {
        let ceiling = self.signer.max_large_source_bytes();
        if ceiling == 0 {
            return Err("exceeds_limit");
        }
        if !self.shared {
            return Err("large_request_not_shared");
        }
        // An outcome that cannot be kept would be worked out again for
        // every request, each range of one playback included.
        if facts.keep_for.is_none() || !services.loading_cache_for(ASSETS_CACHE).keeps_values() {
            return Err("large_outcome_not_kept");
        }
        let Some(length) = total else {
            return Err("unknown_length");
        };
        if length > ceiling {
            return Err("exceeds_large_limit");
        }
        if !services.http_client().supports_streaming_responses() {
            return Err("no_streaming_transport");
        }
        if self.signer.max_large_media_duration().is_zero() {
            return Err("no_large_media_budget");
        }
        if services.timer().is_none() {
            return Err("no_timer");
        }
        SourceVersion::of(headers, length).ok_or("no_strong_etag")
    }

    /// What a signable origin answer of `total` bytes, larger than the
    /// bounded limit, becomes: a [`Delivery::Large`] where the large lane
    /// admits it, else the origin's answer as it came.
    pub(super) fn large_or_unsigned(
        &self,
        services: &RuntimeServices,
        response: Response<EdgeBody>,
        total: Option<u64>,
        facts: &OriginFacts,
    ) -> Delivery {
        match self.large_version(services, response.headers(), total, facts) {
            Ok(version) => Delivery::Large(LargeAnswer {
                response,
                version,
                keep_for: facts.keep_for.unwrap_or_default(),
            }),
            Err(reason) => {
                log::info!(
                    "media signing: outcome=unsigned signer={} reason={reason}",
                    self.signer.signer_id()
                );
                Delivery::Send(response)
            }
        }
    }

    /// Answers the reader from an origin answer admitted to the large lane.
    ///
    /// A source no public address can name, or whose signer's namespace
    /// cannot be resolved, goes as the origin answered it, before anything
    /// is hashed. An outcome kept for the pinned version under the signer's
    /// current namespace answers next: a signed one decorates the origin's
    /// own answer, a declined one leaves it as it came. A `HEAD`, or an
    /// answer whose origin does not say it serves byte ranges, never starts
    /// a load and goes as it came. Otherwise the version is hashed and
    /// signed once, through the loading cache, so callers asking at the
    /// same time share that one load. A signed version is answered by
    /// pinned reads, refused where the origin no longer serves it; a
    /// declined one by one fresh origin answer to the request as core first
    /// sent it, since no signature binds its bytes. `origin` is how the
    /// origin is asked again.
    pub(crate) async fn deliver_large(
        &self,
        services: &RuntimeServices,
        identity: &VariantIdentity<'_>,
        origin: PinnedOrigin<'_>,
        answer: LargeAnswer,
    ) -> Response<EdgeBody> {
        let signer_id = self.signer.signer_id();
        let budget = LargeBudget {
            started: Instant::now(),
            total: self.signer.max_large_media_duration(),
        };
        let LargeAnswer {
            response,
            version,
            keep_for,
        } = answer;
        let references = LargeReferences::of(identity, origin.auth.is_some());
        if !references.any() {
            log::info!(
                "media signing: outcome=unsigned lane=large signer={signer_id} reason=no_public_reference"
            );
            return response;
        }
        let Some(timer) = services.timer() else {
            log::warn!(
                "media signing: outcome=unsigned lane=large signer={signer_id} reason=no_timer"
            );
            return response;
        };
        let remaining = budget.remaining();
        if remaining.is_zero() {
            log::warn!(
                "media signing: outcome=unsigned lane=large signer={signer_id} reason=timeout"
            );
            return response;
        }
        let namespace = match futures::future::select(
            self.signer.namespace(services),
            timer.sleep(remaining),
        )
        .await
        {
            Either::Left((Ok(namespace), _)) => namespace,
            Either::Left((Err(failure), _)) => {
                log::warn!(
                    "media signing: outcome=unsigned lane=large signer={signer_id} reason={failure}"
                );
                return response;
            }
            Either::Right(((), _)) => {
                log::warn!(
                    "media signing: outcome=unsigned lane=large signer={signer_id} reason=timeout"
                );
                return response;
            }
        };
        let key = self.large_store_key(services, &namespace, identity, version.etag());
        let looked_up = Instant::now();
        let kept = match futures::future::select(
            Box::pin(self.large_kept(services, &key, Some(version.length()))),
            timer.sleep(budget.remaining()),
        )
        .await
        {
            Either::Left((kept, _)) => kept,
            Either::Right(((), _)) => {
                log::warn!(
                    "media signing: outcome=unsigned lane=large signer={signer_id} reason=timeout"
                );
                if looked_up.elapsed() <= ORIGIN_BODY_MAX_WAIT {
                    return response;
                }
                let (parts, _) = response.into_parts();
                return self.deliver_fresh(services, origin, parts).await;
            }
        };
        if let Some(kept) = kept {
            if looked_up.elapsed() <= ORIGIN_BODY_MAX_WAIT {
                log::debug!("media signing: lane=large signer={signer_id} from=kept_outcome");
                return match kept {
                    KeptOutcome::Signed(record) => {
                        let (mut parts, body) = response.into_parts();
                        decorate(&mut parts.headers, &record);
                        Response::from_parts(parts, body)
                    }
                    KeptOutcome::Declined => response,
                };
            }
            // The origin's answer waited unread for the lookup, so it is no
            // longer trusted to stream.
            let (parts, _) = response.into_parts();
            return match kept {
                KeptOutcome::Signed(record) => {
                    let source = VersionedMediaSource::pinned(services, origin, version);
                    self.deliver_pinned(&source, parts, &record).await
                }
                KeptOutcome::Declined => self.deliver_fresh(services, origin, parts).await,
            };
        }
        // Only a reader's own `GET` of a version the origin serves in byte
        // ranges starts the work of hashing it.
        if self.reader_method == Method::HEAD {
            log::debug!(
                "media signing: outcome=unsigned lane=large signer={signer_id} reason=head_starts_no_load"
            );
            return response;
        }
        if !advertises_byte_ranges(response.headers()) {
            log::info!(
                "media signing: outcome=unsigned lane=large signer={signer_id} reason=origin_ranges_not_advertised"
            );
            return response;
        }
        if budget.remaining().is_zero() {
            log::warn!(
                "media signing: outcome=unsigned lane=large signer={signer_id} reason=timeout"
            );
            if looked_up.elapsed() <= ORIGIN_BODY_MAX_WAIT {
                return response;
            }
            let (parts, _) = response.into_parts();
            return self.deliver_fresh(services, origin, parts).await;
        }
        // The origin's own body is not read: the load reads the version
        // pinned, so it serves every caller waiting on it.
        let (parts, _) = response.into_parts();
        let load = LargeLoad {
            signer: Arc::clone(&self.signer),
            services: services.clone().without_connection(),
            url: origin.url.clone(),
            headers: origin.headers.clone(),
            backend_name: origin.backend_name.to_owned(),
            auth: origin.auth.cloned(),
            unsigned_headers: origin.unsigned_headers.to_vec(),
            version: version.clone(),
            reader_url: identity.reader_url.to_owned(),
            content_type: header_text(&parts.headers, &header::CONTENT_TYPE).to_owned(),
            references,
            namespace,
            budget,
            keep_for,
        };
        let loaded = match futures::future::select(
            Box::pin(services.load_cached(&self.spec, &key, load.into_loader())),
            timer.sleep(budget.remaining()),
        )
        .await
        {
            Either::Left((loaded, _)) => loaded.ok().and_then(|bytes| decode_kept_outcome(&bytes)),
            Either::Right(((), _)) => {
                log::warn!(
                    "media signing: outcome=unsigned lane=large signer={signer_id} reason=timeout"
                );
                None
            }
        };
        match loaded {
            Some(KeptOutcome::Signed(record)) if record.length == version.length() => {
                let source = VersionedMediaSource::pinned(services, origin, version);
                self.deliver_pinned(&source, parts, &record).await
            }
            _ => self.deliver_fresh(services, origin, parts).await,
        }
    }

    /// Answers the reader of a version the lane does not sign with one
    /// fresh origin answer to the request as core first sent it, the
    /// reader's own range and conditions included, its origin signing
    /// renewed. Nothing is retried.
    ///
    /// The status, body and every header describing the representation
    /// are the fresh answer's, so a changed version goes with its own
    /// validators. The rest are `parts`, the headers core and its caller
    /// prepared for this reader, to which the fresh answer's `Link` and
    /// `Content-Security-Policy` values are added, never replacing the
    /// caller's: provenance the origin now declares is kept, and several
    /// policies only narrow what a reader allows. The caller's cache policy
    /// describes the first answer, not this one, so the answer is held by
    /// no cache at all. A cookie or any other header the fresh answer
    /// sets is never passed on, since the caller's own handling never saw
    /// it.
    async fn deliver_fresh(
        &self,
        services: &RuntimeServices,
        origin: PinnedOrigin<'_>,
        mut parts: http::response::Parts,
    ) -> Response<EdgeBody> {
        let signer_id = self.signer.signer_id();
        let Ok(mut request) = origin.signed_request(&self.reader_method, origin.base_headers())
        else {
            log::warn!(
                "media signing: outcome=unanswered lane=large signer={signer_id} reason=origin_request_unsigned"
            );
            return origin_unreachable();
        };
        if services.http_client().supports_streaming_responses() {
            request = request.with_stream_response();
        }
        // The platform's error may name the address, which can carry a
        // credential, so it is not logged.
        let Ok(fresh) = services.http_client().send(request).await else {
            log::warn!(
                "media signing: outcome=unanswered lane=large signer={signer_id} reason=origin_unreachable"
            );
            return origin_unreachable();
        };
        let (mut fresh, body) = fresh.response.into_parts();
        // Fresh cookies are not delivered. The existing redirect-loop guard
        // must not assume they will change the reader's next request.
        fresh.headers.remove(header::SET_COOKIE);
        if let Some(rewrite) = origin.response_header_rewrite {
            crate::response_header_rewrite::rewrite_origin_urls_in_headers(
                &mut fresh.headers,
                &rewrite,
            );
        }
        parts.status = fresh.status;
        for name in FRESH_REPRESENTATION_HEADERS {
            parts.headers.remove(name);
            for value in fresh.headers.get_all(name) {
                parts.headers.append(name, value.clone());
            }
        }
        for name in [header::LINK, header::CONTENT_SECURITY_POLICY] {
            for value in fresh.headers.get_all(&name) {
                if !parts
                    .headers
                    .get_all(&name)
                    .iter()
                    .any(|kept| kept == value)
                {
                    parts.headers.append(name.clone(), value.clone());
                }
            }
        }
        let mut response = Response::from_parts(parts, body);
        crate::cache_policy::remove_owned_cache_headers(&mut response);
        let headers = response.headers_mut();
        crate::cache_policy::apply_no_store_private_to_headers(headers);
        for name in [header::EXPIRES, header::AGE] {
            headers.remove(name);
        }
        response
    }

    /// Decorates the origin's `304` for a large version kept as signed
    /// under the signer's current namespace, so a reader revalidating it is
    /// told of its manifest. Any other `304` goes as it came.
    pub(super) async fn decorate_not_modified(
        &self,
        services: &RuntimeServices,
        identity: &VariantIdentity<'_>,
        facts: &OriginFacts,
        response: Response<EdgeBody>,
    ) -> Response<EdgeBody> {
        // A `304` whose own `Link` already names a manifest keeps the
        // origin's provenance as it came; no kept outcome is added to it.
        if !self.large_lane_open()
            || !self.looks_like_media
            || facts.private
            || carries_manifest_link(response.headers())
        {
            return response;
        }
        let Some(version) = SourceVersion::of(response.headers(), 0) else {
            return response;
        };
        let Some(timer) = services.timer() else {
            return response;
        };
        let budget = LargeBudget {
            started: Instant::now(),
            total: self.signer.max_large_media_duration(),
        };
        let remaining = budget.remaining();
        if remaining.is_zero() {
            return response;
        }
        let namespace =
            match futures::future::select(self.signer.namespace(services), timer.sleep(remaining))
                .await
            {
                Either::Left((Ok(namespace), _)) => namespace,
                _ => return response,
            };
        let key = self.large_store_key(services, &namespace, identity, version.etag());
        let remaining = budget.remaining();
        if remaining.is_zero() {
            return response;
        }
        let record = match futures::future::select(
            Box::pin(self.large_kept(services, &key, None)),
            timer.sleep(remaining),
        )
        .await
        {
            Either::Left((Some(KeptOutcome::Signed(record)), _)) => record,
            _ => return response,
        };
        let (mut parts, body) = response.into_parts();
        decorate(&mut parts.headers, &record);
        Response::from_parts(parts, body)
    }

    /// Answers the reader's preconditions and range against the pinned,
    /// signed version, reading the bytes it needs by pinned reads, on
    /// `parts`, the origin's headers as core prepared them, and adds the
    /// signed version's headers from `record`. A version the origin no
    /// longer serves is refused, never replaced, since the headers bind
    /// these exact bytes.
    async fn deliver_pinned(
        &self,
        source: &VersionedMediaSource<'_>,
        mut parts: http::response::Parts,
        record: &LargeRecord,
    ) -> Response<EdgeBody> {
        let length = source.size();
        let outcome = evaluate(
            &self.reader_method,
            &self.preconditions,
            Some(source.version().etag()),
            None,
            length,
        );
        let carries_body = self.reader_method != Method::HEAD;
        let range = match outcome {
            Outcome::Full if carries_body => length.checked_sub(1).map(|last| (0, last)),
            Outcome::Partial { first, last } => Some((first, last)),
            _ => None,
        };
        let body = match range {
            Some((first, last)) => {
                let read = source.send_pinned(first, last).await;
                match read.and_then(|body| exact_body(body, last - first + 1)) {
                    Ok(body) => body,
                    Err(error) => {
                        log::warn!(
                            "media signing: outcome=refused lane=large signer={} reason={}",
                            self.signer.signer_id(),
                            error.code()
                        );
                        return refusal();
                    }
                }
            }
            None => EdgeBody::empty(),
        };
        let headers = &mut parts.headers;
        for name in [
            "content-range",
            "content-length",
            "transfer-encoding",
            "content-encoding",
        ] {
            headers.remove(name);
        }
        match outcome {
            Outcome::Full => {
                parts.status = StatusCode::OK;
                headers.insert(header::CONTENT_LENGTH, HeaderValue::from(length));
            }
            Outcome::Partial { first, last } => {
                parts.status = StatusCode::PARTIAL_CONTENT;
                headers.insert(
                    header::CONTENT_RANGE,
                    HeaderValue::from_str(&format!("bytes {first}-{last}/{length}"))
                        .expect("should build a content range"),
                );
                headers.insert(header::CONTENT_LENGTH, HeaderValue::from(last - first + 1));
            }
            Outcome::NotModified => {
                parts.status = StatusCode::NOT_MODIFIED;
            }
            Outcome::PreconditionFailed => {
                parts.status = StatusCode::PRECONDITION_FAILED;
                headers.remove(header::CONTENT_TYPE);
                headers.insert(header::CONTENT_LENGTH, HeaderValue::from(0_u64));
            }
            Outcome::Unsatisfiable => {
                parts.status = StatusCode::RANGE_NOT_SATISFIABLE;
                headers.insert(
                    header::CONTENT_RANGE,
                    HeaderValue::from_str(&format!("bytes */{length}"))
                        .expect("should build a content range"),
                );
                headers.remove(header::CONTENT_TYPE);
                headers.insert(header::CONTENT_LENGTH, HeaderValue::from(0_u64));
            }
        }
        if matches!(
            outcome,
            Outcome::Full | Outcome::Partial { .. } | Outcome::NotModified
        ) {
            decorate(headers, record);
        }
        Response::from_parts(parts, body)
    }

    /// The outcome kept under `key`, or `None`. A signed outcome whose
    /// length is not `length`, where an answer states one, or a record that
    /// does not read back, is dropped. The lookup itself runs no load, but
    /// waits on one another caller is running for `key`.
    async fn large_kept(
        &self,
        services: &RuntimeServices,
        key: &StoreKey,
        length: Option<u64>,
    ) -> Option<KeptOutcome> {
        if !services.loading_cache_for(ASSETS_CACHE).keeps_values() {
            return None;
        }
        let bytes = services
            .load_cached(
                &self.spec,
                key,
                Box::new(|| Box::pin(async { Err(LoadError::new(super::NOT_KEPT)) })),
            )
            .await
            .ok()?;
        let kept = decode_kept_outcome(&bytes).filter(|kept| match kept {
            KeptOutcome::Signed(record) => length.is_none_or(|length| record.length == length),
            KeptOutcome::Declined => true,
        });
        if kept.is_none() {
            log::warn!(
                "media signing: lane=large kept outcome unreadable or for another length signer={}",
                self.signer.signer_id()
            );
            services
                .loading_cache_for(ASSETS_CACHE)
                .remove(&services.clone().without_connection(), &self.spec, key)
                .await;
        }
        kept
    }

    /// The key the outcome for `identity`'s version `etag` is kept under,
    /// bound to the publisher's prefix, the signer and its `namespace`.
    fn large_store_key(
        &self,
        services: &RuntimeServices,
        namespace: &str,
        identity: &VariantIdentity<'_>,
        etag: &str,
    ) -> StoreKey {
        let mut digest = Sha256::new();
        for part in [
            self.signer.signer_id(),
            namespace,
            identity.reader_url,
            identity.origin_identity,
            identity.fetched_url,
            etag,
        ] {
            digest.update(part.as_bytes());
            digest.update([0]);
        }
        StoreKey::new(
            services.server_key_prefix(),
            ASSETS_CACHE,
            format!("media-large-{}", hex::encode(digest.finalize().as_slice())),
        )
    }
}

#[cfg(test)]
mod tests;
