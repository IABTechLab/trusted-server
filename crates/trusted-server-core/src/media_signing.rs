//! Signed media, which a module makes from the exact bytes of a binary
//! response on its way to a reader.
//!
//! A module supplies a [`MediaSigner`] through
//! [`IntegrationRegistrationBuilder::with_media_signer`](crate::integrations::IntegrationRegistrationBuilder::with_media_signer),
//! and it runs wherever the registration runs. Core asks it about two
//! paths: a binary response the publisher path passes through, and the
//! answer of an asset route. Which responses are signed is decided by the
//! origin's media type, so a path with no file extension is covered as an
//! image, audio or video file is.
//!
//! The origin is asked as the reader asked, so a response that is not
//! signed keeps the origin's own range and conditional handling. Where the
//! origin answers `200` with a type the signer handles, within its limit,
//! core captures the body before anything else reads it and hands those
//! exact bytes to the signer. A request that plausibly asks for media asks
//! the origin for an identity-coded body, the only coding signed. Where the
//! origin answers a `206` or a `HEAD` for such a type, core asks it once
//! more for the whole representation, unconditionally, and signs what that
//! answer carries, but only where the signed whole can be kept, so the
//! origin is not read whole for every range. Where the reader's range or
//! `If-Match` brings a `416` or `412` the longer signed representation may
//! answer, and where the reader's `If-Range` or `If-Match` names a signed
//! representation, the origin is asked for the whole representation too.
//! Core applies no transformation of its own, so the captured source is
//! also the final body.
//!
//! An asset route that uses the platform's image optimizer is the one
//! place a transformation happens, and the optimizer reads the origin
//! itself, so core never holds its input. Core asks the optimizer to keep
//! its input's C2PA manifest and add a manifest of its own recording what
//! it did, which binds the input it read by hash. Only a whole `200` to a
//! `GET` whose body carries a C2PA manifest store is handed to the signer,
//! marked as [`MediaJob::optimizer_output`], so the optimizer's own record
//! is the parent of what is signed and core adds nothing to the history.
//! An optimizer writes such a record only where its input carried a
//! manifest, so an unsigned original's optimized answer is never signed.
//! A route may instead send the optimizer to its own signed original on
//! the serving host, see `AssetImageOptimizerConfig::signed_source`, so
//! the optimizer's input is the exact origin bytes core captured and
//! signed.
//! Nothing of it is kept, since its bytes depend on the transformation and
//! the reader's `Accept`, and its signed answer says it varies by `Accept`.
//!
//! The signed bytes are a representation of their own. Core gives them a
//! strong `ETag` of their own digest, a `Repr-Digest` and a
//! `Content-Length`, and answers the reader's `If-Match`, `If-None-Match`,
//! `If-Range` and `Range` against them, with `304`, `412`, `206` and `416`
//! as each requires. The origin's validators never describe signed bytes,
//! so they are removed. Ranges are offered only on a signed representation
//! that was kept.
//!
//! A signed representation is kept in the `[cache.assets]` loading cache
//! for as long as the origin's own headers made it shareably fresh, under
//! the namespace the signer signed it under, and only for a request whose
//! answer the origin could not have made one reader's own. Before the
//! origin is asked, a request of that kind which plausibly asks for media
//! and asks for no revalidation is looked up under the signer's current
//! [`namespace`](MediaSigner::namespace), so a changed or revoked policy
//! never serves a kept copy. A kept copy carries its `Age`, and one whose
//! body no longer hashes to its tag is dropped.
//!
//! What is never signed: a response the origin marked as one reader's, a
//! request carrying `Authorization`, a body larger than the signer's limit,
//! a content-coded body, a body the origin forbade transforming, a
//! response whose own `Link` already names a C2PA manifest, read from the
//! origin's own headers before core writes a cache policy, and a captured
//! body that is an init or media segment of a fragmented ISO BMFF stream,
//! whose provenance binds the stream's parts together. Such a response
//! goes to the reader as the origin answered it, its `Link` untouched.
//! One core meant to
//! sign and could not goes out without the origin's validators and held
//! only briefly and privately, so the next request can be signed.
//!
//! A signer may also sign a source larger than its limit, unchanged, in
//! the large lane, see [`MediaSigner::sign_large`]. Core never holds such a
//! source whole: it pins one version by the origin's strong `ETag` and
//! length, hashes it in bounded chunks, hands the signer a
//! [`VersionedMediaSource`] whose every read is pinned with `If-Match`, and
//! answers the reader with the origin's own bytes and validators plus the
//! signer's headers and a `Repr-Digest`. Only metadata is kept, under the
//! signer's current namespace and the pinned tag. A source without a strong
//! `ETag` goes to the reader as the origin answered it.

use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use base64::Engine as _;
use bytes::{Bytes, BytesMut};
use edgezero_core::body::Body as EdgeBody;
use futures::StreamExt as _;
use http::{HeaderMap, HeaderName, HeaderValue, Method, Response, StatusCode, header};
use sha2::{Digest as _, Sha256};
use web_time::{SystemTime, UNIX_EPOCH};

use crate::middleware::{ResponseHeader, media_type_of};
use crate::platform::{CacheSpec, LoadError, LoadedValue, RuntimeServices, StoreKey};
use crate::response_privacy::cache_control_forbids_shared_storage;
use crate::settings::{ASSETS_CACHE, Settings};

mod large;

pub(crate) use large::{LargeAnswer, PinnedOrigin};
pub use large::{
    LargeMediaJob, LargeMediaSigning, LargeSignedMedia, MediaSourceError, SourceVersion,
    VersionedMediaSource,
};

/// A module that signs the exact bytes of a binary representation.
///
/// Registered with
/// [`IntegrationRegistrationBuilder::with_media_signer`](crate::integrations::IntegrationRegistrationBuilder::with_media_signer).
/// At most one running registration may supply one.
pub trait MediaSigner: Send + Sync {
    /// The module's name, for logs.
    fn signer_id(&self) -> &'static str;

    /// Whether a `GET` or `HEAD` request for `path` may be signed, asked
    /// before the origin is. Core changes nothing about the origin request,
    /// so this only leaves out paths that never carry media, such as an API.
    /// The media type of the origin's answer decides the rest, so a signer
    /// answers `true` for a path with no file extension.
    fn handles_path(&self, path: &str) -> bool;

    /// Whether this signer signs a body of `media_type`, a bare lower case
    /// `type/subtype` read from the origin's `Content-Type`.
    fn handles_media_type(&self, media_type: &str) -> bool;

    /// The largest body core captures for this signer. A larger one goes to
    /// the reader as the origin answered it, never held whole.
    fn max_source_bytes(&self) -> usize;

    /// The policy and credential context a kept signed representation is
    /// bound to, from a policy check no older than the signer's own policy
    /// lifetime. Core keeps a signed representation under it and serves a
    /// kept one only while this answers the same value. An error keeps
    /// nothing and serves nothing kept for the request. Never a secret.
    fn namespace<'a>(&'a self, services: &'a RuntimeServices) -> MediaNamespacing<'a>;

    /// Signs one captured representation.
    fn sign<'a>(&'a self, services: &'a RuntimeServices, job: &'a MediaJob<'a>)
    -> MediaSigning<'a>;

    /// The largest source this signer signs unchanged in the large lane,
    /// see [`sign_large`](Self::sign_large), or `0`, the default, where it
    /// signs none. Core uses the lane only for a source larger than
    /// [`max_source_bytes`](Self::max_source_bytes) and no larger than this.
    fn max_large_source_bytes(&self) -> u64 {
        0
    }

    /// The one wall-clock budget for a large source: core's hashing of it
    /// and the whole of [`sign_large`](Self::sign_large). Core drops the
    /// work, stalled reads included, when it is spent. `Duration::ZERO`,
    /// the default, keeps the large lane closed, and the lane is also
    /// closed on a platform without a timer.
    fn max_large_media_duration(&self) -> Duration {
        Duration::ZERO
    }

    /// Signs one large source that reaches the reader unchanged, from its
    /// whole-source digest and pinned reads, returning only the headers its
    /// answers carry.
    ///
    /// Core asks only for a shared `GET` whose origin answer is a `200` or
    /// `206` of a type the signer handles, identity coded, not marked as
    /// one reader's, not forbidden from transformation, naming no manifest
    /// of its own, saying `Accept-Ranges: bytes`, with one strong `ETag`
    /// and a known total length above
    /// [`max_source_bytes`](Self::max_source_bytes) and within
    /// [`max_large_source_bytes`](Self::max_large_source_bytes), on a
    /// platform that streams response bodies and offers a timer, and only
    /// where no metadata is kept for that version under the current
    /// [`namespace`](Self::namespace). The default signs nothing.
    fn sign_large<'a>(
        &'a self,
        _services: &'a RuntimeServices,
        _job: &'a LargeMediaJob<'a>,
    ) -> LargeMediaSigning<'a> {
        Box::pin(async {
            Err(MediaSigningFailure {
                reason: "large_media_unsupported",
            })
        })
    }
}

/// What [`MediaSigner::sign`] returns.
pub type MediaSigning<'a> =
    Pin<Box<dyn Future<Output = Result<SignedMedia, MediaSigningFailure>> + 'a>>;

/// What [`MediaSigner::namespace`] returns.
pub type MediaNamespacing<'a> =
    Pin<Box<dyn Future<Output = Result<String, MediaSigningFailure>> + 'a>>;

/// One captured representation a signer is asked to sign.
#[derive(Debug, Clone, Copy)]
pub struct MediaJob<'a> {
    /// The address the reader asked for, `scheme://host/path`, without its
    /// query.
    pub reader_url: &'a str,
    /// The origin's `Content-Type`, as it sent it.
    pub content_type: &'a str,
    /// The exact body core received, captured before anything else read
    /// it. Core transforms nothing, so it is also the final body: the
    /// operations core performed are none and the record is complete.
    pub source: &'a Bytes,
    /// SHA-256 of [`source`](Self::source), lower case hex.
    pub source_sha256: &'a str,
    /// The address core fetched the source from, exactly as fetched, or
    /// `None` when that address carries user information, a query or a
    /// fragment, or when the source is
    /// [`optimizer_output`](Self::optimizer_output). It names a mutable
    /// resource, not an immutable version.
    pub source_reference: Option<&'a str>,
    /// The lower case host of the address core fetched the source from,
    /// with no scheme, port, path, query or user information, present even
    /// where [`source_reference`](Self::source_reference) is not. Core
    /// always sets it for a body it captured over HTTP. For
    /// [`optimizer_output`](Self::optimizer_output) it is the origin host
    /// the optimizer was asked to read.
    pub source_origin_host: Option<&'a str>,
    /// The source is the platform image optimizer's answer, not the
    /// origin's bytes. Core hands such a source over only where it carries
    /// a C2PA manifest store, which the optimizer writes to record what it
    /// did to its own input. A signer must have the signature refused
    /// unless that embedded manifest is present and intact, so the
    /// optimizer's own record of its input and transformations is the
    /// parent of what is signed, never replaced by a claim of core's.
    pub optimizer_output: bool,
}

/// The signed representation a signer returns.
#[derive(Debug, Clone)]
pub struct SignedMedia {
    /// The final signed bytes, which core sends as they are.
    pub bytes: Bytes,
    /// The `Content-Type` of the signed bytes.
    pub content_type: HeaderValue,
    /// Headers the signer writes on every response of the signed
    /// representation: `200`, `206`, `304`, `412`, `416` and `HEAD`. A `Link`
    /// is added to the origin's own links, and any other replaces the
    /// origin's header of that name. A name core owns, see
    /// [`is_reserved_header`], fails the signing.
    pub headers: Vec<ResponseHeader>,
    /// The policy and credential context these bytes were actually signed
    /// under, as [`MediaSigner::namespace`] gives it for that context. Core
    /// keeps the signed representation under this value, never under a
    /// namespace asked for again, so bytes signed under one policy are never
    /// served while another is current.
    pub namespace: String,
}

/// Why a signer did not sign, which core logs before it sends the origin's
/// bytes unsigned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, derive_more::Display)]
#[display("{reason}")]
pub struct MediaSigningFailure {
    /// A short code naming the reason.
    pub reason: &'static str,
}

impl core::error::Error for MediaSigningFailure {}

/// Headers core writes on a signed representation, which a signer may not
/// set.
const RESERVED_HEADERS: [&str; 17] = [
    "content-length",
    "content-type",
    "content-encoding",
    "content-range",
    "accept-ranges",
    "etag",
    "last-modified",
    "cache-control",
    "expires",
    "vary",
    "set-cookie",
    "transfer-encoding",
    "repr-digest",
    "content-digest",
    "digest",
    "age",
    "date",
];

/// Origin headers that describe the origin's bytes or one transfer of them,
/// never signed bytes.
const ORIGIN_REPRESENTATION_HEADERS: [&str; 11] = [
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
];

/// Origin headers a kept signed representation carries besides the ones
/// core and the signer write. Core's per-request headers are written again
/// on each reader's copy, so none of them is kept.
const KEPT_ORIGIN_HEADERS: [&str; 6] = [
    "cache-control",
    "expires",
    "content-disposition",
    "content-language",
    "link",
    "x-content-type-options",
];

/// Request headers that make an origin answer with less than the whole
/// representation.
const PRECONDITION_HEADERS: [HeaderName; 6] = [
    header::RANGE,
    header::IF_RANGE,
    header::IF_NONE_MATCH,
    header::IF_MODIFIED_SINCE,
    header::IF_MATCH,
    header::IF_UNMODIFIED_SINCE,
];

/// The first bytes of a kept signed representation, naming its layout.
const KEPT_MAGIC: &[u8; 4] = b"TSM2";

/// How many kept signed representations one process holds in memory. Each
/// may be as large as a signer's limit, so few are held.
const KEPT_MEMORY_ENTRIES: usize = 4;

/// What starts the opaque part of a signed representation's entity tag,
/// so a reader's precondition naming signed bytes is told from one naming
/// the origin's.
const SIGNED_TAG_PREFIX: &str = "sig-";

/// The `Cache-Control` an answer core meant to sign and could not goes out
/// with, so no cache holds unsigned bytes in place of signed ones for long.
const UNSIGNED_FALLBACK_CACHE_CONTROL: &str = "private, max-age=60";

/// `Sec-Fetch-Dest` values a browser sends for media.
const MEDIA_DESTINATIONS: [&str; 7] = [
    "image", "audio", "video", "track", "font", "object", "embed",
];

/// File extensions of media, for a request that names neither a media
/// destination nor a media type it accepts.
const MEDIA_EXTENSIONS: [&str; 33] = [
    "jpg", "jpeg", "png", "gif", "webp", "avif", "heic", "heif", "tif", "tiff", "svg", "bmp",
    "ico", "jxl", "mp4", "m4v", "mov", "m4a", "mp3", "wav", "aac", "flac", "ogg", "oga", "ogv",
    "opus", "webm", "avi", "mkv", "woff", "woff2", "ttf", "otf",
];

/// What a lookup that finds nothing kept answers, never stored.
const NOT_KEPT: &str = "no signed representation kept";

/// Whether `name` is a header core owns on a signed representation.
#[must_use]
pub fn is_reserved_header(name: &HeaderName) -> bool {
    RESERVED_HEADERS.contains(&name.as_str())
}

/// Removes every header that would make the origin answer with less than
/// the whole representation, for the second, whole fetch of a claim.
pub(crate) fn strip_representation_preconditions(headers: &mut HeaderMap) {
    for name in &PRECONDITION_HEADERS {
        headers.remove(name);
    }
}

/// What the reader's request asked of the representation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ReaderPreconditions {
    if_match: Option<String>,
    if_none_match: Option<String>,
    if_range: Option<String>,
    range: Option<String>,
    if_modified_since: Option<String>,
    if_unmodified_since: Option<String>,
}

impl ReaderPreconditions {
    fn of(headers: &HeaderMap) -> Self {
        Self {
            if_match: joined(headers, &header::IF_MATCH),
            if_none_match: joined(headers, &header::IF_NONE_MATCH),
            if_range: joined(headers, &header::IF_RANGE),
            range: joined(headers, &header::RANGE),
            if_modified_since: joined(headers, &header::IF_MODIFIED_SINCE),
            if_unmodified_since: joined(headers, &header::IF_UNMODIFIED_SINCE),
        }
    }

    /// Whether `If-Range` or `If-Match` names a signed representation's
    /// entity tag, see [`SIGNED_TAG_PREFIX`].
    fn names_signed_bytes(&self) -> bool {
        let if_range = self
            .if_range
            .as_deref()
            .and_then(single_entity_tag)
            .is_some_and(|tag| is_signed_tag(&tag));
        let if_match = self
            .if_match
            .as_deref()
            .is_some_and(|value| match TagList::parse(value) {
                TagList::Any => false,
                TagList::Tags(tags) => tags.iter().any(is_signed_tag),
            });
        if_range || if_match
    }
}

/// Whether `tag` is a strong entity tag core gave a signed representation.
fn is_signed_tag(tag: &EntityTag<'_>) -> bool {
    !tag.weak
        && tag
            .opaque
            .strip_prefix(SIGNED_TAG_PREFIX)
            .is_some_and(|digest| {
                digest.len() == 64
                    && digest
                        .bytes()
                        .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
            })
}

/// The strong entity tag of signed bytes whose SHA-256 is `digest`.
fn signed_entity_tag(digest: &[u8]) -> String {
    format!("\"{SIGNED_TAG_PREFIX}{}\"", hex::encode(digest))
}

/// Whether a request plausibly asks for media: the destination a browser
/// names, else the first type it accepts, else the path's file extension.
fn request_looks_like_media(path: &str, headers: &HeaderMap) -> bool {
    if let Some(destination) = headers
        .get("sec-fetch-dest")
        .and_then(|value| value.to_str().ok())
    {
        let destination = destination.trim();
        return MEDIA_DESTINATIONS
            .iter()
            .any(|media| destination.eq_ignore_ascii_case(media));
    }
    let accepted = header_text(headers, &header::ACCEPT)
        .split(',')
        .next()
        .map(media_type_of)
        .unwrap_or_default();
    if ["image/", "audio/", "video/", "font/"]
        .iter()
        .any(|prefix| accepted.starts_with(prefix))
    {
        return true;
    }
    path.rsplit('/')
        .next()
        .and_then(|name| name.rsplit_once('.'))
        .is_some_and(|(_, extension)| {
            MEDIA_EXTENSIONS
                .iter()
                .any(|media| extension.eq_ignore_ascii_case(media))
        })
}

/// Seconds since the Unix epoch now, from the clock every runtime offers.
fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// An answer core meant to sign and could not, made safe to send unsigned:
/// without the origin's validators, which a later conditional request would
/// use to pin the unsigned bytes, and held only briefly and only by the
/// reader, so the next request can be signed.
fn unsigned_fallback(mut response: Response<EdgeBody>) -> Response<EdgeBody> {
    crate::cache_policy::remove_owned_cache_headers(&mut response);
    let headers = response.headers_mut();
    for name in ["etag", "last-modified", "expires"] {
        headers.remove(name);
    }
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(UNSIGNED_FALLBACK_CACHE_CONTROL),
    );
    response
}

/// Every line of header `name`, joined as one list. A value that is not
/// text stays present and matches nothing.
fn joined(headers: &HeaderMap, name: &HeaderName) -> Option<String> {
    let mut values = headers.get_all(name).iter().peekable();
    values.peek()?;
    Some(
        values
            .map(|value| String::from_utf8_lossy(value.as_bytes()).into_owned())
            .collect::<Vec<_>>()
            .join(", "),
    )
}

/// What names one signed representation before the origin is asked: the
/// reader's address, the origin core asks and the address it asks there.
#[derive(Debug, Clone, Copy)]
pub(crate) struct VariantIdentity<'a> {
    /// `scheme://host/path` the reader asked for, without its query.
    pub reader_url: &'a str,
    /// What tells this origin apart from another at one address.
    pub origin_identity: &'a str,
    /// The address core fetches, exactly as fetched.
    pub fetched_url: &'a str,
    /// The reader's request carried no query, so
    /// [`reader_url`](Self::reader_url) is the whole address it asked for.
    pub reader_query_free: bool,
}

/// Independent remaining browser and shared-cache lifetimes.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OriginFreshness {
    pub browser: Duration,
    pub shared: Duration,
}

/// What the origin's own headers said about its answer, read before core
/// changed or removed any of them, so a cache policy core writes never
/// stands in for the origin's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OriginFacts {
    /// The origin marked the answer as one reader's, by a cookie, a
    /// `Cache-Control` that forbids shared storage or a `Vary` on the
    /// reader.
    private: bool,
    /// The origin forbade transforming the body.
    no_transform: bool,
    /// How long a signed representation of the answer may be kept.
    keep_for: Option<Duration>,
    /// Freshness at receipt, before any response processing.
    freshness: OriginFreshness,
}

impl OriginFacts {
    /// What `headers`, the origin's as it sent them, say.
    pub(crate) fn of(headers: &HeaderMap) -> Self {
        Self::of_at(headers, SystemTime::now())
    }

    /// Read against an explicit receipt time, also used by deterministic tests.
    pub(crate) fn of_at(headers: &HeaderMap, received_at: SystemTime) -> Self {
        let freshness = origin_freshness(headers, received_at);
        Self {
            private: headers.contains_key(header::SET_COOKIE)
                || cache_control_forbids_shared_storage(headers)
                || varies_by_reader(headers),
            no_transform: cache_control_has(headers, "no-transform"),
            keep_for: (!freshness.shared.is_zero()).then_some(freshness.shared),
            freshness,
        }
    }

    /// The origin marked the answer as one reader's, by a cookie, a
    /// `Cache-Control` that forbids shared storage or a `Vary` on the
    /// reader.
    pub(crate) fn is_private(&self) -> bool {
        self.private
    }

    /// The origin forbade transforming the body, by `no-transform`.
    pub(crate) fn forbids_transform(&self) -> bool {
        self.no_transform
    }

    /// Freshness after processing, without borrowing the shared lifetime for
    /// the browser or granting a new lifetime when origin dates are removed.
    pub(crate) fn remaining_freshness(&self, elapsed: Duration) -> OriginFreshness {
        OriginFreshness {
            browser: self.freshness.browser.saturating_sub(elapsed),
            shared: self.freshness.shared.saturating_sub(elapsed),
        }
    }
}

/// Where the response handed to [`MediaClaim::deliver`] came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Answered {
    /// The origin, to the request as core sent it, with what its own
    /// headers said.
    Origin(OriginFacts),
    /// A kept signed representation [`MediaClaim::kept`] found.
    Kept,
}

/// What [`MediaClaim::deliver`] decided.
pub(crate) enum Delivery {
    /// The response to send.
    Send(Response<EdgeBody>),
    /// The origin answered with a part, a `HEAD`, or a `412` or `416` a
    /// reader's signed-length range or signed tag brings, for a
    /// representation that may be signed. The caller asks the origin once
    /// more for the whole representation, see
    /// [`strip_representation_preconditions`], and hands the answer to
    /// [`MediaClaim::deliver_whole`], with this response to send where that
    /// fails.
    FetchWhole(Response<EdgeBody>),
    /// The origin answered with a source larger than the signer's bounded
    /// limit that the large lane admits, pinned to its version. The caller
    /// hands it to [`MediaClaim::deliver_large`] with how the origin is
    /// asked again.
    Large(LargeAnswer),
}

/// A request the services' [`MediaSigner`] may sign the answer to.
pub(crate) struct MediaClaim {
    signer: Arc<dyn MediaSigner>,
    reader_method: Method,
    preconditions: ReaderPreconditions,
    spec: CacheSpec,
    /// The answer may be kept for, and a kept one served to, other
    /// readers: the request carries nothing that could make the origin's
    /// answer this reader's own.
    shared: bool,
    /// The request lets a kept signed representation answer it: it is
    /// shared, does not ask for revalidation, and plausibly asks for media.
    reads_kept: bool,
    /// The request plausibly asks for media.
    looks_like_media: bool,
    /// The reader's `If-Range` or `If-Match` names a signed
    /// representation, which the origin cannot judge.
    names_signed_bytes: bool,
    /// The answer is the platform image optimizer's, see
    /// [`answered_by_optimizer`](Self::answered_by_optimizer).
    optimizer_output: bool,
    /// The claim answered the reader's preconditions against the
    /// representation it delivered, see
    /// [`answer_unsigned`](Self::answer_unsigned).
    conditions_answered: AtomicBool,
}

impl MediaClaim {
    /// The claim the request's services' signer makes on a request, or
    /// `None` where no signer is registered, the method is neither `GET`
    /// nor `HEAD`, the request carries `Authorization`, or the signer leaves
    /// `path` alone.
    ///
    /// `shared` says whether nothing the request carries beyond these
    /// headers could make the origin's answer this reader's own, being a
    /// cookie sent to the origin, a module's request state or a
    /// personalized delivery. Only a shared request reads or writes a kept
    /// signed representation.
    pub(crate) fn of_request(
        settings: &Settings,
        services: &RuntimeServices,
        method: &Method,
        path: &str,
        headers: &HeaderMap,
        shared: bool,
    ) -> Option<Self> {
        let signer = services.media_signer()?;
        let claimed = (*method == Method::GET || *method == Method::HEAD)
            && !headers.contains_key(header::AUTHORIZATION)
            && signer.handles_path(path);
        if !claimed {
            return None;
        }
        let preconditions = ReaderPreconditions::of(headers);
        let looks_like_media = request_looks_like_media(path, headers);
        let names_signed_bytes = preconditions.names_signed_bytes();
        Some(Self {
            signer: Arc::clone(signer),
            reader_method: method.clone(),
            preconditions,
            spec: CacheSpec {
                name: ASSETS_CACHE,
                lifetimes: settings
                    .cache
                    .assets
                    .lifetimes(crate::publisher::ASSET_DEFAULT_LIFETIMES),
                memory_entries: KEPT_MEMORY_ENTRIES,
            },
            shared,
            reads_kept: shared
                && looks_like_media
                && !crate::publisher::request_asks_for_revalidation(headers),
            looks_like_media,
            names_signed_bytes,
            optimizer_output: false,
            conditions_answered: AtomicBool::new(false),
        })
    }

    /// The same claim on a request the platform's image optimizer answers.
    ///
    /// The optimizer reads the origin itself, so core never holds its
    /// input. Its answer is signed only where it carries a C2PA manifest
    /// store, the optimizer's own record of the input it read and what it
    /// did, and the signer must refuse unless that manifest is present and
    /// intact, see [`MediaJob::optimizer_output`]. Its answer depends on
    /// the transformation and the reader's `Accept`, neither of which names
    /// a kept representation, so nothing is read from or written to the
    /// kept representations, and only a whole `200` to a `GET` is signed:
    /// a part or `HEAD` is never asked for again past the optimizer.
    pub(crate) fn answered_by_optimizer(mut self) -> Self {
        self.shared = false;
        self.reads_kept = false;
        self.optimizer_output = true;
        self
    }

    /// Whether the request plausibly asks for media, by the destination
    /// the browser names, the type it accepts first, or the file extension.
    /// The origin is then asked for an identity-coded body, the only coding
    /// signed.
    pub(crate) fn looks_like_media(&self) -> bool {
        self.looks_like_media
    }

    /// Whether the reader's `If-Range` or `If-Match` names a signed
    /// representation. The origin cannot judge such a precondition, so the
    /// caller asks it for the whole representation, see
    /// [`strip_representation_preconditions`], and core answers the
    /// reader's range and preconditions against the signed bytes.
    pub(crate) fn names_signed_bytes(&self) -> bool {
        self.names_signed_bytes
    }

    /// Whether the answer is the platform image optimizer's, see
    /// [`answered_by_optimizer`](Self::answered_by_optimizer).
    pub(crate) fn signs_optimizer_output(&self) -> bool {
        self.optimizer_output
    }

    /// The signed representation kept for `identity` under the signer's
    /// current namespace, as an origin answer to hand to
    /// [`deliver`](Self::deliver) with [`Answered::Kept`], or `None`. Only
    /// a shared request that plausibly asks for media and asks for no
    /// revalidation is looked up, so the signer's namespace is not resolved
    /// for any other.
    pub(crate) async fn kept(
        &self,
        services: &RuntimeServices,
        identity: &VariantIdentity<'_>,
    ) -> Option<Response<EdgeBody>> {
        if !self.reads_kept || !services.loading_cache_for(ASSETS_CACHE).keeps_values() {
            return None;
        }
        let namespace = match self.signer.namespace(services).await {
            Ok(namespace) => namespace,
            Err(failure) => {
                log::debug!(
                    "media signing: nothing kept looked up signer={} reason={failure}",
                    self.signer.signer_id()
                );
                return None;
            }
        };
        let key = self.store_key(services, &namespace, identity);
        let bytes = services
            .load_cached(
                &self.spec,
                &key,
                Box::new(|| Box::pin(async { Err(LoadError::new(NOT_KEPT)) })),
            )
            .await
            .ok()?;
        let kept = decode_kept(&bytes, unix_seconds());
        if kept.is_none() {
            log::warn!(
                "media signing: kept representation unreadable or altered signer={}",
                self.signer.signer_id()
            );
            services
                .loading_cache_for(ASSETS_CACHE)
                .remove(&services.clone().without_connection(), &self.spec, &key)
                .await;
        }
        kept
    }

    /// Answers the reader from `response`: a kept signed representation, or
    /// the origin's answer, signed where the signer signs it.
    pub(crate) async fn deliver(
        &self,
        services: &RuntimeServices,
        identity: &VariantIdentity<'_>,
        answered: Answered,
        response: Response<EdgeBody>,
    ) -> Delivery {
        let facts = match answered {
            Answered::Kept => return Delivery::Send(self.respond_kept(response)),
            Answered::Origin(facts) => facts,
        };
        if self.optimizer_output {
            return Delivery::Send(
                self.deliver_optimized(services, identity, &facts, response)
                    .await,
            );
        }
        let signer_id = self.signer.signer_id();
        let status = response.status();
        let limit = to_u64(self.signer.max_source_bytes());
        match status {
            StatusCode::OK if self.reader_method != Method::HEAD => {
                if let Some(reason) = self.unsignable_reason(response.headers(), &facts) {
                    log::debug!(
                        "media signing: outcome=unsigned signer={signer_id} reason={reason}"
                    );
                    return Delivery::Send(response);
                }
                let total = declared_length(response.headers());
                if total.is_some_and(|total| total > limit) {
                    return self.large_or_unsigned(services, response, total, &facts);
                }
                let (parts, body) = response.into_parts();
                let source = match self.capture_within_limit(&parts.headers, body).await {
                    Ok(source) => source,
                    Err(body) => return Delivery::Send(Response::from_parts(parts, body)),
                };
                if is_fragmented_stream_part(&source) {
                    log::info!(
                        "media signing: outcome=unsigned signer={signer_id} reason=fragmented_stream_part"
                    );
                    return Delivery::Send(Response::from_parts(parts, EdgeBody::Once(source)));
                }
                match self.sign(services, identity, &parts.headers, &source).await {
                    Ok(signed) => Delivery::Send(
                        self.respond_signed(services, identity, parts, signed, facts.keep_for)
                            .await,
                    ),
                    Err(()) => Delivery::Send(unsigned_fallback(Response::from_parts(
                        parts,
                        EdgeBody::Once(source),
                    ))),
                }
            }
            StatusCode::OK | StatusCode::PARTIAL_CONTENT => {
                if let Some(reason) = self.unsignable_reason(response.headers(), &facts) {
                    log::debug!(
                        "media signing: outcome=unsigned signer={signer_id} reason={reason}"
                    );
                    return Delivery::Send(response);
                }
                let total = if status == StatusCode::PARTIAL_CONTENT {
                    content_range_total(response.headers())
                } else {
                    declared_length(response.headers())
                };
                let too_large = if status == StatusCode::PARTIAL_CONTENT {
                    total.is_none_or(|total| total > limit)
                } else {
                    total.is_some_and(|total| total > limit)
                };
                if too_large {
                    return self.large_or_unsigned(services, response, total, &facts);
                }
                // A part or a `HEAD` is signed only where the signed whole
                // can be kept, so the reader's next range is answered from
                // it and the origin is not read whole for every range.
                if !self.retains(services, &facts) {
                    log::info!(
                        "media signing: outcome=unsigned signer={signer_id} reason=part_not_kept"
                    );
                    return Delivery::Send(response);
                }
                Delivery::FetchWhole(response)
            }
            StatusCode::PRECONDITION_FAILED | StatusCode::RANGE_NOT_SATISFIABLE
                if self.preconditions.range.is_some() || self.preconditions.if_match.is_some() =>
            {
                // The reader's range or `If-Match` may describe the signed
                // representation, which is longer than the origin's and has
                // a tag of its own, so the origin's refusal is not the
                // answer until the signed bytes are asked.
                let too_large =
                    content_range_total(response.headers()).is_some_and(|total| total > limit);
                if facts.private || too_large {
                    return Delivery::Send(response);
                }
                Delivery::FetchWhole(response)
            }
            StatusCode::NOT_MODIFIED => Delivery::Send(
                self.decorate_not_modified(services, identity, &facts, response)
                    .await,
            ),
            _ => Delivery::Send(response),
        }
    }

    /// Answers the reader from the platform image optimizer's answer: a
    /// whole `200` to a `GET` that carries a C2PA manifest store is signed,
    /// and every other answer goes as the optimizer made it.
    async fn deliver_optimized(
        &self,
        services: &RuntimeServices,
        identity: &VariantIdentity<'_>,
        facts: &OriginFacts,
        response: Response<EdgeBody>,
    ) -> Response<EdgeBody> {
        let signer_id = self.signer.signer_id();
        if response.status() != StatusCode::OK || self.reader_method != Method::GET {
            return response;
        }
        if let Some(reason) = self.unsignable_reason(response.headers(), facts) {
            log::debug!("media signing: outcome=unsigned signer={signer_id} reason={reason}");
            return response;
        }
        let (parts, body) = response.into_parts();
        let source = match self.capture_within_limit(&parts.headers, body).await {
            Ok(source) => source,
            Err(body) => return Response::from_parts(parts, body),
        };
        // The optimizer writes a manifest only where its input carried one.
        // Without it nothing records what the optimizer read or did, so the
        // answer is not signed.
        if !carries_c2pa_manifest_store(&source) {
            log::info!(
                "media signing: outcome=unsigned signer={signer_id} reason=image_optimizer_output_without_c2pa"
            );
            return Response::from_parts(parts, EdgeBody::Once(source));
        }
        // The optimizer may choose its output format from the reader's
        // `Accept`, and it removes the origin's `Vary`, so a cache past core
        // is told the signed bytes depend on it.
        let mut parts = parts;
        let varies_by_accept = parts
            .headers
            .get_all(header::VARY)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .flat_map(|value| value.split(','))
            .any(|name| name.trim() == "*" || name.trim().eq_ignore_ascii_case("accept"));
        if !varies_by_accept {
            parts
                .headers
                .append(header::VARY, HeaderValue::from_static("Accept"));
        }
        match self.sign(services, identity, &parts.headers, &source).await {
            Ok(signed) => {
                self.respond_signed(services, identity, parts, signed, None)
                    .await
            }
            Err(()) => unsigned_fallback(Response::from_parts(parts, EdgeBody::Once(source))),
        }
    }

    /// Answers the reader from the origin's answer to the whole,
    /// unconditional request [`Delivery::FetchWhole`] asked for, or sends
    /// `fallback` where it cannot be signed.
    ///
    /// `whole` is the origin's answer as it came, whose own headers decide
    /// what is signed and how long it is kept. The signed response is
    /// written on `fallback`'s headers, which carry what core wrote for
    /// this reader.
    pub(crate) async fn deliver_whole(
        &self,
        services: &RuntimeServices,
        identity: &VariantIdentity<'_>,
        whole: Response<EdgeBody>,
        fallback: Response<EdgeBody>,
    ) -> Response<EdgeBody> {
        let signer_id = self.signer.signer_id();
        // The answer sent where the whole cannot be signed already declares
        // a manifest of the origin's own, which core never competes with.
        if carries_manifest_link(fallback.headers()) {
            log::info!(
                "media signing: outcome=unsigned signer={signer_id} reason=origin_manifest_link"
            );
            return fallback;
        }
        let facts = OriginFacts::of(whole.headers());
        if whole.status() != StatusCode::OK {
            log::info!(
                "media signing: outcome=unsigned signer={signer_id} reason=whole_fetch_status_{}",
                whole.status().as_u16()
            );
            return unsigned_fallback(fallback);
        }
        if let Some(reason) = self.unsignable_reason(whole.headers(), &facts) {
            log::info!("media signing: outcome=unsigned signer={signer_id} reason={reason}");
            return fallback;
        }
        let (whole_parts, whole_body) = whole.into_parts();
        let Ok(source) = self
            .capture_within_limit(&whole_parts.headers, whole_body)
            .await
        else {
            return fallback;
        };
        // A part of a fragmented stream goes as the origin answered it.
        if is_fragmented_stream_part(&source) {
            log::info!(
                "media signing: outcome=unsigned signer={signer_id} reason=fragmented_stream_part"
            );
            return fallback;
        }
        match self
            .sign(services, identity, &whole_parts.headers, &source)
            .await
        {
            Ok(signed) => {
                let (parts, _) = fallback.into_parts();
                self.respond_signed(services, identity, parts, signed, facts.keep_for)
                    .await
            }
            Err(()) => unsigned_fallback(fallback),
        }
    }

    /// Whether a signed whole made for this request can be kept: the request
    /// is shared, the origin gave its answer a shared freshness, and the
    /// deployment's asset cache keeps values.
    fn retains(&self, services: &RuntimeServices, facts: &OriginFacts) -> bool {
        self.shared
            && facts.keep_for.is_some()
            && services.loading_cache_for(ASSETS_CACHE).keeps_values()
    }

    /// The whole body of a signable `200`, or the body as it stands where
    /// it is larger than the signer's limit or could not be read, so the
    /// reader still receives every byte the origin sent.
    async fn capture_within_limit(
        &self,
        headers: &HeaderMap,
        body: EdgeBody,
    ) -> Result<Bytes, EdgeBody> {
        let signer_id = self.signer.signer_id();
        let limit = self.signer.max_source_bytes();
        if declared_length(headers).is_some_and(|length| length > to_u64(limit)) {
            log::info!("media signing: outcome=unsigned signer={signer_id} reason=exceeds_limit");
            return Err(body);
        }
        match capture(body, limit).await {
            Captured::Whole(source) => Ok(source),
            Captured::Partial(body) => {
                log::info!(
                    "media signing: outcome=unsigned signer={signer_id} reason=exceeds_limit_or_unreadable"
                );
                Err(body)
            }
        }
    }

    /// Why a response is not handed to the signer, or `None` when it is.
    /// `facts` are the origin's own, read before core wrote its cache
    /// policy on `headers`.
    fn unsignable_reason(&self, headers: &HeaderMap, facts: &OriginFacts) -> Option<&'static str> {
        if facts.private || headers.contains_key(header::SET_COOKIE) {
            return Some("origin_private");
        }
        // Provenance the origin already links to describes these bytes, so
        // they are neither signed again nor given a competing manifest.
        if carries_manifest_link(headers) {
            return Some("origin_manifest_link");
        }
        if facts.no_transform {
            return Some("no_transform");
        }
        let encoding = header_text(headers, &header::CONTENT_ENCODING).trim();
        if !encoding.is_empty() && !encoding.eq_ignore_ascii_case("identity") {
            return Some("content_encoded");
        }
        let media_type = media_type_of(header_text(headers, &header::CONTENT_TYPE));
        if media_type.is_empty() || !self.signer.handles_media_type(&media_type) {
            return Some("media_type");
        }
        None
    }

    /// Asks the signer for the signed representation of `source`, logging
    /// the outcome with no address, since a reader's or origin's address
    /// may carry a credential.
    async fn sign(
        &self,
        services: &RuntimeServices,
        identity: &VariantIdentity<'_>,
        headers: &HeaderMap,
        source: &Bytes,
    ) -> Result<SignedMedia, ()> {
        let signer_id = self.signer.signer_id();
        let source_sha256 = hex::encode(Sha256::digest(source));
        let origin_host = url::Url::parse(identity.fetched_url)
            .ok()
            .and_then(|url| url.host_str().map(str::to_ascii_lowercase));
        let job = MediaJob {
            reader_url: identity.reader_url,
            content_type: header_text(headers, &header::CONTENT_TYPE),
            source,
            source_sha256: &source_sha256,
            // No address serves the optimizer's answer.
            source_reference: if self.optimizer_output {
                None
            } else {
                source_reference(identity.fetched_url)
            },
            source_origin_host: origin_host.as_deref(),
            optimizer_output: self.optimizer_output,
        };
        let checked = match self.signer.sign(services, &job).await {
            Ok(signed) if signed.bytes.is_empty() => Err(MediaSigningFailure {
                reason: "empty_signed_body",
            }),
            Ok(signed)
                if signed
                    .headers
                    .iter()
                    .any(|(name, _)| is_reserved_header(name)) =>
            {
                Err(MediaSigningFailure {
                    reason: "reserved_header",
                })
            }
            Ok(signed) if signed.namespace.is_empty() => Err(MediaSigningFailure {
                reason: "no_namespace",
            }),
            other => other,
        };
        match checked {
            Ok(signed) => {
                log::info!(
                    "media signing: outcome=signed signer={signer_id} source_bytes={} signed_bytes={}",
                    source.len(),
                    signed.bytes.len()
                );
                Ok(signed)
            }
            Err(failure) => {
                log::warn!("media signing: outcome=unsigned signer={signer_id} reason={failure}");
                Err(())
            }
        }
    }

    /// The signed representation, kept for `keep_for` where this request is
    /// shared, and answered against the reader's request. Ranges are offered
    /// only on a representation that was kept, so a reader's later range is
    /// answered from it and never from a part of the origin's own bytes.
    async fn respond_signed(
        &self,
        services: &RuntimeServices,
        identity: &VariantIdentity<'_>,
        mut parts: http::response::Parts,
        signed: SignedMedia,
        keep_for: Option<Duration>,
    ) -> Response<EdgeBody> {
        let origin_age = digits(header_text(&parts.headers, &header::AGE).trim()).unwrap_or(0);
        for name in ORIGIN_REPRESENTATION_HEADERS {
            parts.headers.remove(name);
        }
        let digest = Sha256::digest(&signed.bytes);
        let etag = signed_entity_tag(digest.as_slice());
        let repr_digest = format!(
            "sha-256=:{}:",
            base64::engine::general_purpose::STANDARD.encode(digest.as_slice())
        );
        let mut representation = HeaderMap::new();
        representation.insert(header::CONTENT_TYPE, signed.content_type);
        representation.insert(
            header::ETAG,
            HeaderValue::from_str(&etag).expect("should build a hex entity tag"),
        );
        representation.insert(
            HeaderName::from_static("repr-digest"),
            HeaderValue::from_str(&repr_digest).expect("should build a base64 digest"),
        );
        for name in KEPT_ORIGIN_HEADERS {
            for value in parts.headers.get_all(name) {
                representation.append(HeaderName::from_static(name), value.clone());
            }
        }
        for (name, value) in signed.headers {
            if name == header::LINK {
                representation.append(name, value);
            } else {
                representation.insert(name, value);
            }
        }
        let kept = match keep_for.filter(|_| self.shared) {
            Some(lifetime) => {
                let value = encode_kept(&KeptRecord {
                    kept_at: unix_seconds(),
                    age: origin_age,
                    headers: &representation,
                    body: &signed.bytes,
                });
                self.keep(services, identity, &signed.namespace, value, lifetime)
                    .await
            }
            None => false,
        };
        for name in representation.keys() {
            parts.headers.remove(name);
        }
        for (name, value) in &representation {
            parts.headers.append(name.clone(), value.clone());
        }
        self.answer(parts, signed.bytes, kept)
    }

    /// A kept signed representation, answered against the reader's
    /// request.
    fn respond_kept(&self, response: Response<EdgeBody>) -> Response<EdgeBody> {
        let (parts, body) = response.into_parts();
        let bytes = body.into_bytes().unwrap_or_default();
        self.answer(parts, bytes, true)
    }

    /// Answers the reader's preconditions and range against the signed
    /// `bytes`, whose `ETag` is in `parts`.
    fn answer(
        &self,
        parts: http::response::Parts,
        bytes: Bytes,
        ranges_offered: bool,
    ) -> Response<EdgeBody> {
        let length = to_u64(bytes.len());
        let etag = parts
            .headers
            .get(header::ETAG)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let outcome = evaluate(
            &self.reader_method,
            &self.preconditions,
            etag.as_deref(),
            None,
            length,
        );
        self.conditions_answered.store(true, Ordering::Relaxed);
        self.apply(parts, bytes, outcome, ranges_offered)
    }

    /// `response`, a whole `200` this claim delivered, answered against the
    /// reader's preconditions where the claim has not already answered them,
    /// which it has for every signed representation it delivered. For a
    /// request whose preconditions were not sent to the origin, so the
    /// origin's whole answer is checked against them here, by the
    /// representation's own `ETag` and `Last-Modified`. A range is not
    /// applied, so a passing answer goes whole, as RFC 9110 allows.
    pub(crate) fn answer_unsigned(&self, response: Response<EdgeBody>) -> Response<EdgeBody> {
        if self.conditions_answered.load(Ordering::Relaxed) {
            return response;
        }
        answer_whole_with(&self.reader_method, &self.preconditions, response)
    }
}

/// `response`, a whole `200`, answered against the preconditions of a
/// reader's request carrying `reader_headers`, by the representation's own
/// `ETag` and `Last-Modified`, see [`MediaClaim::answer_unsigned`]: `412`
/// or `304` with no body, or the response as it is. A range is not applied.
pub(crate) fn answer_whole(
    method: &Method,
    reader_headers: &HeaderMap,
    response: Response<EdgeBody>,
) -> Response<EdgeBody> {
    answer_whole_with(method, &ReaderPreconditions::of(reader_headers), response)
}

fn answer_whole_with(
    method: &Method,
    preconditions: &ReaderPreconditions,
    response: Response<EdgeBody>,
) -> Response<EdgeBody> {
    if response.status() != StatusCode::OK {
        return response;
    }
    let headers = response.headers();
    let etag = headers
        .get(header::ETAG)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let last_modified = headers
        .get(header::LAST_MODIFIED)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| httpdate::parse_http_date(value.trim()).ok());
    let whole = ReaderPreconditions {
        range: None,
        if_range: None,
        ..preconditions.clone()
    };
    let outcome = evaluate(method, &whole, etag.as_deref(), last_modified, 0);
    let (mut parts, body) = response.into_parts();
    match outcome {
        Outcome::NotModified => {
            parts.status = StatusCode::NOT_MODIFIED;
            for name in ["content-length", "content-range", "transfer-encoding"] {
                parts.headers.remove(name);
            }
            Response::from_parts(parts, EdgeBody::empty())
        }
        Outcome::PreconditionFailed => {
            parts.status = StatusCode::PRECONDITION_FAILED;
            for name in [
                "content-type",
                "content-range",
                "transfer-encoding",
                "content-encoding",
            ] {
                parts.headers.remove(name);
            }
            parts
                .headers
                .insert(header::CONTENT_LENGTH, HeaderValue::from(0_u64));
            Response::from_parts(parts, EdgeBody::empty())
        }
        _ => Response::from_parts(parts, body),
    }
}

/// Removes the headers that describe the origin's bytes or one transfer of
/// them, for a representation core made of them.
pub(crate) fn remove_origin_representation_headers(headers: &mut HeaderMap) {
    for name in ORIGIN_REPRESENTATION_HEADERS {
        headers.remove(name);
    }
}

impl MediaClaim {
    /// Keeps a signed representation for `identity` under `namespace`, the
    /// one it was signed under, so a later request is answered before the
    /// origin is asked. Whether it was kept.
    async fn keep(
        &self,
        services: &RuntimeServices,
        identity: &VariantIdentity<'_>,
        namespace: &str,
        value: Bytes,
        lifetime: Duration,
    ) -> bool {
        if !services.loading_cache_for(ASSETS_CACHE).keeps_values() {
            return false;
        }
        let key = self.store_key(services, namespace, identity);
        let stored = services
            .load_cached(
                &self.spec,
                &key,
                Box::new(move || {
                    Box::pin(async move { Ok(LoadedValue::new(value).with_lifetime(lifetime)) })
                }),
            )
            .await;
        match stored {
            Ok(_) => true,
            Err(error) => {
                log::debug!(
                    "media signing: representation not kept signer={} error={error}",
                    self.signer.signer_id()
                );
                false
            }
        }
    }

    /// The key a signed representation for `identity` is kept under, bound
    /// to `namespace`.
    fn store_key(
        &self,
        services: &RuntimeServices,
        namespace: &str,
        identity: &VariantIdentity<'_>,
    ) -> StoreKey {
        let mut digest = Sha256::new();
        for part in [
            self.signer.signer_id(),
            namespace,
            identity.reader_url,
            identity.origin_identity,
            identity.fetched_url,
        ] {
            digest.update(part.as_bytes());
            digest.update([0]);
        }
        StoreKey::new(
            services.server_key_prefix(),
            ASSETS_CACHE,
            format!("media-{}", hex::encode(digest.finalize().as_slice())),
        )
    }

    /// Writes `outcome` on the response: its status, its framing headers and
    /// the part of `bytes` it carries.
    fn apply(
        &self,
        mut parts: http::response::Parts,
        bytes: Bytes,
        outcome: Outcome,
        ranges_offered: bool,
    ) -> Response<EdgeBody> {
        let length = to_u64(bytes.len());
        let headers = &mut parts.headers;
        for name in [
            "content-range",
            "content-length",
            "transfer-encoding",
            "content-encoding",
        ] {
            headers.remove(name);
        }
        headers.insert(
            header::ACCEPT_RANGES,
            HeaderValue::from_static(if ranges_offered { "bytes" } else { "none" }),
        );
        let carries_body = self.reader_method != Method::HEAD;
        let body = match outcome {
            Outcome::Full => {
                parts.status = StatusCode::OK;
                headers.insert(header::CONTENT_LENGTH, HeaderValue::from(length));
                if carries_body {
                    EdgeBody::Once(bytes)
                } else {
                    EdgeBody::empty()
                }
            }
            Outcome::NotModified => {
                parts.status = StatusCode::NOT_MODIFIED;
                EdgeBody::empty()
            }
            Outcome::PreconditionFailed => {
                parts.status = StatusCode::PRECONDITION_FAILED;
                headers.remove(header::CONTENT_TYPE);
                headers.insert(header::CONTENT_LENGTH, HeaderValue::from(0_u64));
                EdgeBody::empty()
            }
            Outcome::Partial { first, last } => {
                parts.status = StatusCode::PARTIAL_CONTENT;
                headers.insert(
                    header::CONTENT_RANGE,
                    HeaderValue::from_str(&format!("bytes {first}-{last}/{length}"))
                        .expect("should build a content range"),
                );
                headers.insert(header::CONTENT_LENGTH, HeaderValue::from(last - first + 1));
                let from = usize::try_from(first)
                    .unwrap_or(bytes.len())
                    .min(bytes.len());
                let to = usize::try_from(last)
                    .map_or(bytes.len(), |last| last.saturating_add(1))
                    .min(bytes.len());
                EdgeBody::Once(bytes.slice(from..to.max(from)))
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
                EdgeBody::empty()
            }
        };
        Response::from_parts(parts, body)
    }
}

/// Origin freshness at receipt. Revalidation-only answers and reader-varying
/// responses authorize no reusable copy. Shared freshness falls back to
/// `max-age` only when `s-maxage` is absent, never when it is malformed.
fn origin_freshness(headers: &HeaderMap, received_at: SystemTime) -> OriginFreshness {
    if ["no-store", "no-cache", "private"]
        .iter()
        .any(|directive| cache_control_has(headers, directive))
        || headers
            .get_all(header::VARY)
            .iter()
            .flat_map(|value| value.to_str().unwrap_or("*").split(','))
            .map(str::trim)
            .any(|name| !name.is_empty() && !name.eq_ignore_ascii_case("accept-encoding"))
    {
        return OriginFreshness::default();
    }
    let age = match headers.get(header::AGE) {
        Some(value) => match value.to_str().ok().and_then(|value| digits(value.trim())) {
            Some(seconds) => Duration::from_secs(seconds),
            None => return OriginFreshness::default(),
        },
        None => Duration::ZERO,
    };
    let apparent_age = match headers.get(header::DATE) {
        Some(value) => match value
            .to_str()
            .ok()
            .and_then(|value| httpdate::parse_http_date(value.trim()).ok())
        {
            Some(date) => received_at
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .saturating_sub(
                    date.duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default(),
                ),
            None => return OriginFreshness::default(),
        },
        None => Duration::ZERO,
    };
    let current_age = age.max(apparent_age);
    let browser = cache_control_seconds(headers, "max-age");
    let shared = match cache_control_seconds(headers, "s-maxage") {
        Ok(None) => browser,
        value => value,
    };
    let remaining = |seconds: Result<Option<u64>, ()>| {
        Duration::from_secs(seconds.ok().flatten().unwrap_or(0)).saturating_sub(current_age)
    };
    OriginFreshness {
        browser: remaining(browser),
        shared: remaining(shared),
    }
}

/// An absent directive permits fallback; an invalid or repeated one does not.
fn cache_control_seconds(headers: &HeaderMap, directive: &str) -> Result<Option<u64>, ()> {
    let mut seconds = None;
    for line in headers.get_all(header::CACHE_CONTROL) {
        for part in line.to_str().map_err(|_| ())?.split(',') {
            let (name, value) = part.split_once('=').unwrap_or((part, ""));
            if !name.trim().eq_ignore_ascii_case(directive) {
                continue;
            }
            if seconds.is_some() {
                return Err(());
            }
            let value = value.trim();
            let value = if let Some(quoted) = value.strip_prefix('"') {
                quoted.strip_suffix('"').ok_or(())?
            } else {
                value
            };
            seconds = Some(digits(value).ok_or(())?);
        }
    }
    Ok(seconds)
}

/// What a kept signed representation holds besides its key.
struct KeptRecord<'a> {
    /// When it was kept, in seconds since the Unix epoch.
    kept_at: u64,
    /// The `Age` the origin's answer carried when it was kept.
    age: u64,
    headers: &'a HeaderMap,
    body: &'a Bytes,
}

/// A kept signed representation, as written: [`KEPT_MAGIC`], when it was
/// kept and the age it had then, the header count, each header's name and
/// value with their lengths, then the body.
fn encode_kept(record: &KeptRecord<'_>) -> Bytes {
    let mut out = BytesMut::with_capacity(record.body.len() + 512);
    out.extend_from_slice(KEPT_MAGIC);
    out.extend_from_slice(&record.kept_at.to_be_bytes());
    out.extend_from_slice(&record.age.to_be_bytes());
    let count = u32::try_from(record.headers.len()).unwrap_or(u32::MAX);
    out.extend_from_slice(&count.to_be_bytes());
    for (name, value) in record
        .headers
        .iter()
        .take(usize::try_from(count).unwrap_or(usize::MAX))
    {
        let name = name.as_str().as_bytes();
        let value = value.as_bytes();
        out.extend_from_slice(&u32::try_from(name.len()).unwrap_or(u32::MAX).to_be_bytes());
        out.extend_from_slice(name);
        out.extend_from_slice(&u32::try_from(value.len()).unwrap_or(u32::MAX).to_be_bytes());
        out.extend_from_slice(value);
    }
    out.extend_from_slice(record.body);
    out.freeze()
}

/// A kept signed representation as a `200` answer at `now`, with the `Age`
/// it has reached, or `None` where the bytes are not one [`encode_kept`]
/// wrote or the body is not the one its entity tag names.
fn decode_kept(bytes: &Bytes, now: u64) -> Option<Response<EdgeBody>> {
    let mut reader = KeptReader {
        rest: bytes.strip_prefix(KEPT_MAGIC.as_slice())?,
    };
    let kept_at = reader.number()?;
    let age = reader.number()?;
    let count = reader.length()?;
    let mut headers = HeaderMap::new();
    for _ in 0..count {
        let name_length = reader.length()?;
        let name = HeaderName::from_bytes(reader.take(name_length)?).ok()?;
        let value_length = reader.length()?;
        let value = HeaderValue::from_bytes(reader.take(value_length)?).ok()?;
        headers.append(name, value);
    }
    let body = bytes.slice(bytes.len() - reader.rest.len()..);
    let expected = signed_entity_tag(Sha256::digest(&body).as_slice());
    if header_text(&headers, &header::ETAG) != expected {
        return None;
    }
    headers.insert(
        header::AGE,
        HeaderValue::from(age.saturating_add(now.saturating_sub(kept_at))),
    );
    let mut response = Response::new(EdgeBody::Once(body));
    *response.headers_mut() = headers;
    Some(response)
}

/// Reads a kept representation's fields in order.
struct KeptReader<'a> {
    rest: &'a [u8],
}

impl<'a> KeptReader<'a> {
    fn take(&mut self, length: usize) -> Option<&'a [u8]> {
        let taken = self.rest.get(..length)?;
        self.rest = self.rest.get(length..)?;
        Some(taken)
    }

    fn length(&mut self) -> Option<usize> {
        let bytes: [u8; 4] = self.take(4)?.try_into().ok()?;
        usize::try_from(u32::from_be_bytes(bytes)).ok()
    }

    fn number(&mut self) -> Option<u64> {
        let bytes: [u8; 8] = self.take(8)?.try_into().ok()?;
        Some(u64::from_be_bytes(bytes))
    }
}

/// The fetched address a signer may name as the source, being the address
/// exactly as fetched when it carries no user information, query or
/// fragment, which could hold a credential or a reader's data.
fn source_reference(fetched_url: &str) -> Option<&str> {
    let parsed = url::Url::parse(fetched_url).ok()?;
    let plain = parsed.username().is_empty()
        && parsed.password().is_none()
        && parsed.query().is_none()
        && parsed.fragment().is_none();
    plain.then_some(fetched_url)
}

/// Whether `Vary` names `*`, `Cookie` or `Authorization`, so the origin's
/// answer may differ between readers.
fn varies_by_reader(headers: &HeaderMap) -> bool {
    headers
        .get_all(header::VARY)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .any(|name| {
            name == "*"
                || name.eq_ignore_ascii_case("cookie")
                || name.eq_ignore_ascii_case("authorization")
        })
}

/// Whether the origin's own `Link` already names a C2PA manifest, so the
/// representation carries provenance core must keep as it came rather
/// than sign again or compete with.
fn carries_manifest_link(headers: &HeaderMap) -> bool {
    headers
        .get_all(header::LINK)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|parameter| {
            let (name, relations) = parameter.trim().split_once('=')?;
            name.trim().eq_ignore_ascii_case("rel").then_some(relations)
        })
        .flat_map(|relations| {
            relations.split(|character: char| {
                character.is_ascii_whitespace() || character == '"' || character == ','
            })
        })
        .any(|relation| relation.eq_ignore_ascii_case("c2pa-manifest"))
}

/// The JUMBF description box of a C2PA manifest store: the `jumd` box type
/// followed by the C2PA manifest store UUID,
/// `63327061-0011-0010-8000-00AA00389B71`. Every embedding of a store, a
/// JPEG `APP11` segment, a PNG `caBX` chunk, a WebP `C2PA` chunk, an ISO
/// BMFF `uuid` box or a JPEG XL `jumb` box, carries these bytes
/// contiguously at the head of the store.
const C2PA_STORE_DESCRIPTION: [u8; 20] = [
    b'j', b'u', b'm', b'd', 0x63, 0x32, 0x70, 0x61, 0x00, 0x11, 0x00, 0x10, 0x80, 0x00, 0x00, 0xAA,
    0x00, 0x38, 0x9B, 0x71,
];

/// Whether `bytes` carry a C2PA manifest store. This is presence only:
/// whether the store is well placed and intact is the signer's to check,
/// see [`MediaJob::optimizer_output`].
fn carries_c2pa_manifest_store(bytes: &[u8]) -> bool {
    bytes
        .windows(C2PA_STORE_DESCRIPTION.len())
        .any(|window| window == C2PA_STORE_DESCRIPTION)
}

/// The most boxes [`bmff_boxes`] reads at one level before it gives up.
const MAX_BMFF_BOXES: usize = 4096;

/// The type and body range of each ISO BMFF box laid end to end in
/// `bytes`, or `None` where they are not well formed, or are more than
/// [`MAX_BMFF_BOXES`]. A size of `1` reads a 64-bit size after the type,
/// and a size of `0` runs the box to the end of `bytes`.
fn bmff_boxes(bytes: &[u8]) -> Option<Vec<([u8; 4], std::ops::Range<usize>)>> {
    let mut boxes = Vec::new();
    let mut offset = 0_usize;
    while offset < bytes.len() {
        if boxes.len() == MAX_BMFF_BOXES {
            return None;
        }
        let header = bytes.get(offset..offset.checked_add(8)?)?;
        let size32 = u32::from_be_bytes(header.get(0..4)?.try_into().ok()?);
        let kind: [u8; 4] = header.get(4..8)?.try_into().ok()?;
        let (header_len, size) = match size32 {
            0 => (8, bytes.len() - offset),
            1 => {
                let large = bytes.get(offset + 8..offset.checked_add(16)?)?;
                let size = u64::from_be_bytes(large.try_into().ok()?);
                (16, usize::try_from(size).ok()?)
            }
            size => (8, usize::try_from(size).ok()?),
        };
        if size < header_len {
            return None;
        }
        let end = offset.checked_add(size)?;
        if end > bytes.len() {
            return None;
        }
        boxes.push((kind, offset + header_len..end));
        offset = end;
    }
    Some(boxes)
}

/// Whether `bytes` are a part of a fragmented ISO BMFF stream, which is
/// never signed alone: its provenance, such as C2PA verifiable segment
/// info, binds the stream's init segment and media segments together, so
/// a manifest added to one part would compete with the stream's.
///
/// A media segment starts with `styp`, or has a `moof` before its first
/// `mdat`, and has no `moov`. An init segment starts with `ftyp`, has a
/// `moov` that holds an `mvex`, and has no `mdat`. A progressive file,
/// whose `moov` holds no `mvex` or which carries an `mdat` beside its
/// `moov`, is not a part, and neither is anything not well formed.
fn is_fragmented_stream_part(bytes: &[u8]) -> bool {
    let Some(boxes) = bmff_boxes(bytes) else {
        return false;
    };
    let Some((first, _)) = boxes.first() else {
        return false;
    };
    let moov = boxes.iter().find(|(kind, _)| kind == b"moov");
    let first_mdat = boxes.iter().position(|(kind, _)| kind == b"mdat");
    let moof_before_mdat = boxes
        .iter()
        .take(first_mdat.unwrap_or(boxes.len()))
        .any(|(kind, _)| kind == b"moof");
    let media_segment = (first == b"styp" || moof_before_mdat) && moov.is_none();
    let init_segment = first == b"ftyp"
        && first_mdat.is_none()
        && moov.is_some_and(|(_, body)| {
            bytes
                .get(body.clone())
                .and_then(bmff_boxes)
                .is_some_and(|children| children.iter().any(|(kind, _)| kind == b"mvex"))
        });
    media_segment || init_segment
}

/// Whether a `Cache-Control` line names `directive`.
fn cache_control_has(headers: &HeaderMap, directive: &str) -> bool {
    headers
        .get_all(header::CACHE_CONTROL)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(|part| part.split('=').next().unwrap_or_default().trim())
        .any(|name| name.eq_ignore_ascii_case(directive))
}

fn header_text<'a>(headers: &'a HeaderMap, name: &HeaderName) -> &'a str {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
}

/// The origin's `Content-Length`, where it sent one that parses.
fn declared_length(headers: &HeaderMap) -> Option<u64> {
    digits(header_text(headers, &header::CONTENT_LENGTH).trim())
}

/// The complete length a `206`'s `Content-Range: bytes a-b/total` names.
fn content_range_total(headers: &HeaderMap) -> Option<u64> {
    let value = header_text(headers, &header::CONTENT_RANGE).trim();
    let (unit, range) = value.split_once(' ')?;
    if !unit.eq_ignore_ascii_case("bytes") {
        return None;
    }
    digits(range.rsplit_once('/')?.1.trim())
}

fn to_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

/// A decimal number of ASCII digits alone, which `str::parse` would also
/// take with a leading `+`.
fn digits(value: &str) -> Option<u64> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value.parse().ok()
}

/// A body read up to a limit.
enum Captured {
    /// The whole body, within the limit.
    Whole(Bytes),
    /// A body over the limit, or one whose stream failed, rebuilt from what
    /// was read and what was not, so the reader receives what it would have.
    Partial(EdgeBody),
}

/// Reads `body` whole when it is no larger than `limit`.
async fn capture(body: EdgeBody, limit: usize) -> Captured {
    match body {
        EdgeBody::Once(bytes) if bytes.len() <= limit => Captured::Whole(bytes),
        once @ EdgeBody::Once(_) => Captured::Partial(once),
        EdgeBody::Stream(mut stream) => {
            let mut chunks = Vec::new();
            let mut total = 0_usize;
            while let Some(item) = stream.next().await {
                match item {
                    Ok(chunk) => {
                        total = total.saturating_add(chunk.len());
                        chunks.push(chunk);
                        if total > limit {
                            let read = futures::stream::iter(chunks.into_iter().map(Ok));
                            return Captured::Partial(EdgeBody::Stream(
                                read.chain(stream).boxed_local(),
                            ));
                        }
                    }
                    Err(error) => {
                        let read = chunks
                            .into_iter()
                            .map(Ok)
                            .chain(std::iter::once(Err(error)));
                        return Captured::Partial(EdgeBody::Stream(
                            futures::stream::iter(read).boxed_local(),
                        ));
                    }
                }
            }
            let mut whole = BytesMut::with_capacity(total);
            for chunk in chunks {
                whole.extend_from_slice(&chunk);
            }
            Captured::Whole(whole.freeze())
        }
    }
}

/// How a request is answered from a signed representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// `200` with the whole representation.
    Full,
    /// `304`.
    NotModified,
    /// `412`.
    PreconditionFailed,
    /// `206` with bytes `first` to `last`, inclusive.
    Partial { first: u64, last: u64 },
    /// `416`.
    Unsatisfiable,
}

/// Evaluates the reader's preconditions and range against a representation
/// in the order RFC 9110 section 13.2.2 gives. `etag` and `last_modified`
/// are the representation's own validators. A signed representation has no
/// modification date, so it passes `None` and `If-Modified-Since` and
/// `If-Unmodified-Since` are ignored; an `If-Range` date never holds. A
/// `Range` is honored on a `GET` alone, for one range. Several ranges are
/// answered with the whole representation, which the RFC allows.
fn evaluate(
    method: &Method,
    preconditions: &ReaderPreconditions,
    etag: Option<&str>,
    last_modified: Option<std::time::SystemTime>,
    length: u64,
) -> Outcome {
    let current = etag.and_then(single_entity_tag);
    let date_of =
        |value: Option<&str>| value.and_then(|value| httpdate::parse_http_date(value.trim()).ok());

    if let Some(value) = preconditions.if_match.as_deref() {
        let matched = match TagList::parse(value) {
            TagList::Any => true,
            TagList::Tags(tags) => current
                .as_ref()
                .is_some_and(|current| tags.iter().any(|tag| tag.strong_eq(current))),
        };
        if !matched {
            return Outcome::PreconditionFailed;
        }
    } else if let (Some(since), Some(modified)) = (
        date_of(preconditions.if_unmodified_since.as_deref()),
        last_modified,
    ) && modified > since
    {
        return Outcome::PreconditionFailed;
    }

    if let Some(value) = preconditions.if_none_match.as_deref() {
        let matched = match TagList::parse(value) {
            TagList::Any => true,
            TagList::Tags(tags) => current
                .as_ref()
                .is_some_and(|current| tags.iter().any(|tag| tag.opaque == current.opaque)),
        };
        if matched {
            return if *method == Method::GET || *method == Method::HEAD {
                Outcome::NotModified
            } else {
                Outcome::PreconditionFailed
            };
        }
    } else if (*method == Method::GET || *method == Method::HEAD)
        && let (Some(since), Some(modified)) = (
            date_of(preconditions.if_modified_since.as_deref()),
            last_modified,
        )
        && modified <= since
    {
        return Outcome::NotModified;
    }

    if *method != Method::GET {
        return Outcome::Full;
    }
    let Some(range) = preconditions.range.as_deref() else {
        return Outcome::Full;
    };
    if let Some(if_range) = preconditions.if_range.as_deref() {
        let holds = single_entity_tag(if_range)
            .zip(current.as_ref())
            .is_some_and(|(tag, current)| tag.strong_eq(current));
        if !holds {
            return Outcome::Full;
        }
    }
    match parse_single_range(range, length) {
        RangeRequest::Ignore => Outcome::Full,
        RangeRequest::Satisfiable { first, last } => Outcome::Partial { first, last },
        RangeRequest::Unsatisfiable => Outcome::Unsatisfiable,
    }
}

/// One entity tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EntityTag<'a> {
    weak: bool,
    opaque: &'a str,
}

impl EntityTag<'_> {
    fn strong_eq(&self, other: &EntityTag<'_>) -> bool {
        !self.weak && !other.weak && self.opaque == other.opaque
    }
}

/// The entity tag at the start of `input` and what follows it.
fn split_entity_tag(input: &str) -> Option<(EntityTag<'_>, &str)> {
    let (weak, quoted) = match input.strip_prefix("W/") {
        Some(rest) => (true, rest),
        None => (false, input),
    };
    let body = quoted.strip_prefix('"')?;
    let end = body.find('"')?;
    Some((
        EntityTag {
            weak,
            opaque: &body[..end],
        },
        &body[end + 1..],
    ))
}

/// A header holding exactly one entity tag, such as `ETag`.
fn single_entity_tag(value: &str) -> Option<EntityTag<'_>> {
    let (tag, rest) = split_entity_tag(value.trim())?;
    rest.trim().is_empty().then_some(tag)
}

/// An `If-Match` or `If-None-Match` value.
enum TagList<'a> {
    Any,
    Tags(Vec<EntityTag<'a>>),
}

impl<'a> TagList<'a> {
    /// The tags of a list, as far as it parses. A tag may hold a comma, so
    /// the list is read tag by tag.
    fn parse(value: &'a str) -> Self {
        if value.trim() == "*" {
            return Self::Any;
        }
        let mut tags = Vec::new();
        let mut rest = value;
        loop {
            rest = rest.trim_start_matches([',', ' ', '\t']);
            if rest.is_empty() {
                break;
            }
            let Some((tag, after)) = split_entity_tag(rest) else {
                break;
            };
            tags.push(tag);
            rest = after;
        }
        Self::Tags(tags)
    }
}

/// What a `Range` header asks of a representation of known length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RangeRequest {
    /// Not one byte range this answers, so the whole representation goes.
    Ignore,
    Satisfiable {
        first: u64,
        last: u64,
    },
    Unsatisfiable,
}

fn parse_single_range(value: &str, length: u64) -> RangeRequest {
    let Some((unit, set)) = value.trim().split_once('=') else {
        return RangeRequest::Ignore;
    };
    if !unit.trim().eq_ignore_ascii_case("bytes") {
        return RangeRequest::Ignore;
    }
    let mut specs = set
        .split(',')
        .map(str::trim)
        .filter(|spec| !spec.is_empty());
    let (Some(spec), None) = (specs.next(), specs.next()) else {
        return RangeRequest::Ignore;
    };
    let Some((first, last)) = spec.split_once('-') else {
        return RangeRequest::Ignore;
    };
    let (first, last) = (first.trim(), last.trim());
    if first.is_empty() {
        let Some(suffix) = digits(last) else {
            return RangeRequest::Ignore;
        };
        if suffix == 0 || length == 0 {
            return RangeRequest::Unsatisfiable;
        }
        return RangeRequest::Satisfiable {
            first: length.saturating_sub(suffix),
            last: length - 1,
        };
    }
    let Some(first) = digits(first) else {
        return RangeRequest::Ignore;
    };
    let last = if last.is_empty() {
        None
    } else {
        match digits(last) {
            Some(last) if last >= first => Some(last),
            _ => return RangeRequest::Ignore,
        }
    };
    if first >= length {
        return RangeRequest::Unsatisfiable;
    }
    RangeRequest::Satisfiable {
        first,
        last: last.map_or(length - 1, |last| last.min(length - 1)),
    }
}

#[cfg(test)]
pub(crate) mod test_support;

#[cfg(test)]
mod tests;
