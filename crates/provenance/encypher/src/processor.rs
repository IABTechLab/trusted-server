//! Per-document signing flow: gates, hold, KV cache, sign calls, outcomes, logging.
//!
//! A document moves through four phases:
//!
//! 1. **Admit** (the middleware's preparation, with no I/O): the request
//!    gates decide whether the document is held at all.
//! 2. **Extract** (synchronous): at end of input the held document's article
//!    boundary, visible text and text hash are computed, and its embedded
//!    media found.
//! 3. **Resolve** (the asynchronous finish): policy and plan caches, the policy
//!    and sign calls, and cache writes, then each embedded media item signed
//!    through [`media`]. On runtimes with a timer every call is raced against
//!    one `max_hold_ms` budget, keeping a reserve for the skip write.
//! 4. **Render** (synchronous): the plan's markers and the manifest meta tag are
//!    applied to the held document, then each signed media item replaces its
//!    exact bytes.
//!
//! Every module failure emits the held document unchanged, or, for one
//! media item, leaves that item as written and reports it unsigned.

use std::borrow::Cow;
use std::io;
use std::pin::pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use bytes::Bytes;
use edgezero_core::body::Body as EdgeBody;
use futures::future::{Either, LocalBoxFuture, select};
use http::{HeaderValue, Method, Request, StatusCode, header};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::api::{self, ApiError, EmbeddingPlan, SignRequest};
use crate::boundary::{self, Boundary};
use crate::embedded;
use crate::{EncypherConfig, INTEGRATION_ID, embed, text};
use trusted_server_core::integrations::ensure_integration_backend_with_timeout;
use trusted_server_core::platform::{
    KvError, PlatformHttpRequest, PlatformKvStore, PlatformTimer, RuntimeServices,
};
use trusted_server_core::streaming_processor::{AsyncFinish, StreamProcessor};

mod media;
mod prepared;

pub(crate) use media::EncypherMedia;

const SIGN_PATH: &str = "/api/v1/public/cdn/sign";
const POLICY_PATH: &str = "/api/v1/public/cdn/trusted-server/policy";
const WORKER_TOKEN_HEADER: &str = "x-encypher-worker-token";
/// Successful article plans. Entries carry their own `expires_at`.
const PLAN_KEY_PREFIX: &str = "encypher:plan:v4";
/// Article failures, separate so a delayed failure cannot replace a plan.
const PLAN_SKIP_KEY_PREFIX: &str = "encypher:plan-skip:v4";
/// Successful policy digests. Entries carry their own `expires_at`.
const POLICY_KEY_PREFIX: &str = "encypher:policy:v3";
/// Policy failures, separate so a delayed failure cannot replace a digest.
const POLICY_SKIP_KEY_PREFIX: &str = "encypher:policy-skip:v3";
/// Part of `max_hold_ms` kept back from resolve work so a skip can still be
/// written after the work runs out of time.
const SKIP_WRITE_RESERVE: Duration = Duration::from_millis(200);
/// How far past `max_hold_ms` the core backstop deadline lies. The module
/// bounds itself first; the backstop only catches an overrun.
const BACKSTOP_MARGIN: Duration = Duration::from_millis(1_000);

/// Path prefixes never signed (ported from the Fastly worker's `shouldSkipPath`).
const SKIP_PATH_PREFIXES: &[&str] = &[
    "/api/",
    "/wp-admin/",
    "/wp-json/",
    "/feed/",
    "/sitemap",
    "/.well-known/",
];
/// Asset suffixes the article path never signs (ported from the Fastly
/// worker's `shouldSkipPath`). Media at these paths is signed by [`media`].
const SKIP_PATH_SUFFIXES: &[&str] = &[
    ".xml", ".json", ".rss", ".atom", ".css", ".js", ".woff", ".woff2", ".png", ".jpg", ".gif",
    ".svg", ".ico",
];

/// Set once this instance has warned that the runtime KV store is unbound.
static KV_UNAVAILABLE_WARNED: AtomicBool = AtomicBool::new(false);

/// Why a document was not held. Evaluated in the middleware's preparation,
/// with no I/O.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Gate {
    Method,
    Status,
    OriginPrivate,
    InvalidPageUrl,
    HostMismatch,
    ExcludedPath,
}

impl Gate {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::Method => "method",
            Self::Status => "status",
            Self::OriginPrivate => "origin_private",
            Self::InvalidPageUrl => "invalid_page_url",
            Self::HostMismatch => "host_mismatch",
            Self::ExcludedPath => "excluded_path",
        }
    }
}

/// What the gates are told about the page a document is the body of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PageRequest {
    /// The reader's request method.
    pub(crate) method: Method,
    /// The scheme, host and path the reader asked for, without the query,
    /// which is the address the article is signed for. On a host that
    /// serves several publishers by path it includes the publisher's prefix.
    pub(crate) page_url: String,
    /// The path prefix request addressing removed, such as
    /// `/www.example.com`, or empty. The path gates read the publisher's own
    /// path, without it.
    pub(crate) removed_prefix: String,
    /// The status of the response the document is the body of.
    pub(crate) response_status: u16,
    /// Whether the origin marked its response as one reader's.
    pub(crate) origin_response_is_private: bool,
}

/// Reason code for a held document's or a media item's outcome. Sign
/// failures are also stored as the reason of a negative (`skip`) cache entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Reason {
    Signed,
    CacheHit,
    DocumentTooLarge,
    OutputTooLarge,
    NotUtf8,
    NoBoundary,
    AlreadySigned,
    ManifestPresent,
    TextTooShort,
    KvUnavailable,
    KvError,
    Timeout,
    CachedSkip,
    CachedEntryInvalid,
    CachedPlanInvalid,
    RequestBuild,
    Backend,
    HttpError,
    Forbidden,
    Unprocessable,
    RateLimited,
    ServerError,
    UnexpectedStatus,
    ResponseTooLarge,
    MalformedResponse,
    PolicyChanged,
    PolicyDigestMismatch,
    QuotaExceeded,
    SignRejected,
    NoPlan,
    InvalidPlan,
    AlignmentFailed,
    /// The media is over `max_media_bytes`.
    MediaTooLarge,
    /// The media is delivered outside the claimed domain.
    OutsideClaim,
    /// Core named no single hostname the source was fetched from.
    InvalidOriginHost,
    /// The captured bytes are not the bytes core hashed.
    SourceDigestMismatch,
    /// A `data:` URI's payload does not decode.
    MalformedDataUri,
    /// The endpoint answered `409` to a reused replay key.
    IdempotencyConflict,
    /// The endpoint no longer holds the result for the replay key.
    ResultPurged,
    /// The endpoint refused the request body's size.
    PayloadTooLarge,
    /// The signed bytes are of another media type than the source.
    SignedTypeMismatch,
    /// The signed bytes are not the bytes the answer names.
    SignedDigestMismatch,
    /// A signing header is missing, repeated or malformed.
    InvalidSignedResponse,
    /// The manifest is remote and needs a `Link` the delivery cannot carry.
    ManifestUndeliverable,
    /// The answer carries no manifest and no reference to one.
    UnsignedResponse,
    /// The response's policies are not visible to the module, so an inline
    /// style edit could break a style hash.
    CspUnknown,
    /// A page policy admits the inline style by hash.
    CspStyleHash,
    /// The element carries a subresource integrity value.
    IntegrityAttribute,
    /// The URI is written with an HTML character reference.
    CharacterReference,
    /// The URI is written with a CSS escape.
    CssEscape,
    /// No standalone SVG document is proven to parse to the tree the browser
    /// built from the inline SVG.
    InlineSvgNotEquivalent,
    /// The inline SVG carries provenance of its own and is not, as written,
    /// a standalone SVG equivalent to the browser's tree, so it is never
    /// rewritten.
    SvgProvenanceNotEquivalent,
    /// Original SVG provenance protects the entire range from inline edits
    /// or replacement with a new, unchained signature.
    SvgExistingProvenance,
    /// The signer changed the inline SVG beyond adding its manifest.
    InlineSvgAltered,
    /// The document's media made its most endpoint calls; the item is signed
    /// on a later view.
    Deferred,
}

impl Reason {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::Signed => "signed",
            Self::CacheHit => "cache_hit",
            Self::DocumentTooLarge => "document_too_large",
            Self::OutputTooLarge => "output_too_large",
            Self::NotUtf8 => "not_utf8",
            Self::NoBoundary => "no_boundary",
            Self::AlreadySigned => "already_signed",
            Self::ManifestPresent => "manifest_present",
            Self::TextTooShort => "text_too_short",
            Self::KvUnavailable => "kv_unavailable",
            Self::KvError => "kv_error",
            Self::Timeout => "timeout",
            Self::CachedSkip => "cached_skip",
            Self::CachedEntryInvalid => "cached_entry_invalid",
            Self::CachedPlanInvalid => "cached_plan_invalid",
            Self::RequestBuild => "request_build",
            Self::Backend => "backend",
            Self::HttpError => "http_error",
            Self::Forbidden => "forbidden",
            Self::Unprocessable => "unprocessable",
            Self::RateLimited => "rate_limited",
            Self::ServerError => "server_error",
            Self::UnexpectedStatus => "unexpected_status",
            Self::ResponseTooLarge => "response_too_large",
            Self::MalformedResponse => "malformed_response",
            Self::PolicyChanged => "policy_changed",
            Self::PolicyDigestMismatch => "policy_digest_mismatch",
            Self::QuotaExceeded => "quota_exceeded",
            Self::SignRejected => "sign_rejected",
            Self::NoPlan => "no_plan",
            Self::InvalidPlan => "invalid_plan",
            Self::AlignmentFailed => "alignment_failed",
            Self::MediaTooLarge => "media_too_large",
            Self::OutsideClaim => "outside_claim",
            Self::InvalidOriginHost => "invalid_origin_host",
            Self::SourceDigestMismatch => "source_digest_mismatch",
            Self::MalformedDataUri => "malformed_data_uri",
            Self::IdempotencyConflict => "idempotency_conflict",
            Self::ResultPurged => "result_purged",
            Self::PayloadTooLarge => "payload_too_large",
            Self::SignedTypeMismatch => "signed_type_mismatch",
            Self::SignedDigestMismatch => "signed_digest_mismatch",
            Self::InvalidSignedResponse => "invalid_signed_response",
            Self::ManifestUndeliverable => "manifest_undeliverable",
            Self::UnsignedResponse => "unsigned_response",
            Self::CspUnknown => "csp_unknown",
            Self::CspStyleHash => "csp_style_hash",
            Self::IntegrityAttribute => "integrity_attribute",
            Self::CharacterReference => "character_reference",
            Self::CssEscape => "css_escape",
            Self::InlineSvgNotEquivalent => "inline_svg_not_equivalent",
            Self::SvgProvenanceNotEquivalent => "svg_provenance_not_equivalent",
            Self::SvgExistingProvenance => "svg_existing_provenance",
            Self::InlineSvgAltered => "inline_svg_altered",
            Self::Deferred => "deferred",
        }
    }
}

/// A cache entry that records when it stops being usable.
trait Expiring {
    /// Unix seconds at which the entry expires.
    fn expires_at(&self) -> u64;
    /// Whether the entry was written without a store TTL, so it stays until deleted.
    fn ttl_less(&self) -> bool;
    /// Record that the entry is written without a store TTL.
    fn mark_ttl_less(&mut self);
}

/// An article cache entry, keyed by credential context, policy and text hash.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum CacheEntry {
    Plan {
        plan: EmbeddingPlan,
        manifest_url: Option<String>,
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
}

impl Expiring for CacheEntry {
    fn expires_at(&self) -> u64 {
        match self {
            Self::Plan { expires_at, .. } | Self::Skip { expires_at, .. } => *expires_at,
        }
    }

    fn ttl_less(&self) -> bool {
        match self {
            Self::Plan { ttl_less, .. } | Self::Skip { ttl_less, .. } => *ttl_less,
        }
    }

    fn mark_ttl_less(&mut self) {
        match self {
            Self::Plan { ttl_less, .. } | Self::Skip { ttl_less, .. } => *ttl_less = true,
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum PolicyEntry {
    Digest {
        policy_digest: String,
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
}

impl Expiring for PolicyEntry {
    fn expires_at(&self) -> u64 {
        match self {
            Self::Digest { expires_at, .. } | Self::Skip { expires_at, .. } => *expires_at,
        }
    }

    fn ttl_less(&self) -> bool {
        match self {
            Self::Digest { ttl_less, .. } | Self::Skip { ttl_less, .. } => *ttl_less,
        }
    }

    fn mark_ttl_less(&mut self) {
        match self {
            Self::Digest { ttl_less, .. } | Self::Skip { ttl_less, .. } => *ttl_less = true,
        }
    }
}

/// The one freshness predicate applied to every cache read.
fn is_fresh(expires_at: u64, now: u64) -> bool {
    now < expires_at
}

/// Current Unix time in seconds, from a clock that works on every adapter.
fn unix_now() -> u64 {
    web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// Configuration shared by every document processor.
pub(crate) struct Signer {
    config: EncypherConfig,
    max_document_bytes: usize,
    sign_url: String,
    media_sign_url: String,
    policy_url: String,
    policy_key: String,
    policy_skip_key: String,
    /// First 16 hex characters of `sha256(api_base_url \n domain \n token)`.
    cache_context: String,
    /// Set by the first TTL write the store reports as unsupported, or by reading
    /// an entry written without a TTL; from then on this instance writes without
    /// a TTL and deletes expired entries it reads.
    ttl_less: AtomicBool,
}

impl Signer {
    /// The signer for `config`, holding at most `max_document_bytes` of one
    /// document.
    pub(crate) fn new(config: EncypherConfig, max_document_bytes: usize) -> Self {
        let sign_url = format!("{}{SIGN_PATH}", config.api_base_url.trim_end_matches('/'));
        let media_sign_url = format!(
            "{}{}",
            config.api_base_url.trim_end_matches('/'),
            crate::media_api::MEDIA_SIGN_PATH
        );
        let policy_url = format!("{}{POLICY_PATH}", config.api_base_url.trim_end_matches('/'));
        let context_hash = text::content_hash_hex(&format!(
            "{}\n{}\n{}",
            config.api_base_url,
            config.domain,
            config.worker_token.expose().trim()
        ));
        let cache_context = context_hash
            .get(..16)
            .unwrap_or(context_hash.as_str())
            .to_owned();
        Self {
            config,
            max_document_bytes,
            sign_url,
            media_sign_url,
            policy_url,
            policy_key: format!("{POLICY_KEY_PREFIX}:{cache_context}"),
            policy_skip_key: format!("{POLICY_SKIP_KEY_PREFIX}:{cache_context}"),
            cache_context,
            ttl_less: AtomicBool::new(false),
        }
    }
}

/// What a held document is, which decides what in it is signed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Surface {
    /// An HTML page: its article's text, then its embedded media.
    Page,
    /// A stylesheet: the media its `url()`s embed.
    Stylesheet,
}

/// Holds the document of an admitted page until it ends. `policies` are the
/// response's `Content-Security-Policy` values, `None` when core cannot
/// show them, which decide whether inline style media may be edited.
pub(crate) fn document_processor(
    signer: &Arc<Signer>,
    request: PageRequest,
    policies: Option<Vec<String>>,
) -> DocumentProcessor {
    held_processor(signer, request, Surface::Page, policies)
}

/// Holds an admitted stylesheet until it ends.
pub(crate) fn stylesheet_processor(
    signer: &Arc<Signer>,
    request: PageRequest,
) -> DocumentProcessor {
    held_processor(signer, request, Surface::Stylesheet, None)
}

fn held_processor(
    signer: &Arc<Signer>,
    request: PageRequest,
    surface: Surface,
    policies: Option<Vec<String>>,
) -> DocumentProcessor {
    DocumentProcessor {
        signer: Arc::clone(signer),
        request,
        surface,
        policies,
        held: Vec::new(),
        state: DocumentState::Holding,
    }
}

/// What a held document's signing works for.
struct Target<'a> {
    request: &'a PageRequest,
    surface: Surface,
    /// The response's policy values, `None` when core cannot show them.
    policies: Option<&'a [String]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DocumentState {
    /// Accumulating the document.
    Holding,
    /// Over the size limit: chunks pass through and nothing is signed.
    PassingThrough,
    /// The whole document was emitted as held output; it may be signed in the
    /// asynchronous finish.
    Complete,
}

/// Holds one admitted document until end of input, then signs it in the
/// asynchronous finish or passes it through.
pub(crate) struct DocumentProcessor {
    signer: Arc<Signer>,
    request: PageRequest,
    surface: Surface,
    policies: Option<Vec<String>>,
    held: Vec<u8>,
    state: DocumentState,
}

impl StreamProcessor for DocumentProcessor {
    fn process_chunk(&mut self, chunk: &[u8], is_last: bool) -> Result<Vec<u8>, io::Error> {
        if self.state != DocumentState::Holding {
            return Ok(chunk.to_vec());
        }
        if self.held.len().saturating_add(chunk.len()) > self.signer.max_document_bytes {
            self.state = DocumentState::PassingThrough;
            log_outcome(false, Reason::DocumentTooLarge);
            let mut output = std::mem::take(&mut self.held);
            output.extend_from_slice(chunk);
            return Ok(output);
        }
        self.held.extend_from_slice(chunk);
        if !is_last {
            return Ok(Vec::new());
        }
        // The complete document is the fail-open output; the driver owns it.
        self.state = DocumentState::Complete;
        Ok(std::mem::take(&mut self.held))
    }

    fn reset(&mut self) {
        self.held.clear();
        self.state = DocumentState::Holding;
    }

    fn async_finish<'a>(
        &'a mut self,
        held: &'a [u8],
        services: &'a RuntimeServices,
    ) -> Option<AsyncFinish<'a>> {
        if self.state != DocumentState::Complete {
            return None;
        }
        let Ok(document) = std::str::from_utf8(held) else {
            log_outcome(false, Reason::NotUtf8);
            return None;
        };
        let (article, media) = match self.surface {
            Surface::Page => (
                Some(self.signer.extract(document)),
                embedded::scan_html(document, self.policies.as_deref()),
            ),
            Surface::Stylesheet => (None, embedded::scan_stylesheet(document)),
        };
        let article = match article {
            Some(Ok(article)) => Some(article),
            Some(Err(reason)) => {
                log_outcome(false, reason);
                None
            }
            None => None,
        };
        if article.is_none() && media.is_empty() {
            return None;
        }
        let signer = &*self.signer;
        let target = Target {
            request: &self.request,
            surface: self.surface,
            policies: self.policies.as_deref(),
        };
        let deadline =
            Duration::from_millis(signer.config.max_hold_ms).saturating_add(BACKSTOP_MARGIN);
        Some(AsyncFinish::new(deadline, async move {
            signer
                .resolve_document(&target, services, document, article, media)
                .await
        }))
    }
}

/// The article found in a held document.
struct Article {
    boundary: Boundary,
    /// Visible text sent for signing, truncated to the API limit.
    text: String,
    /// Unicode scalars in `text`.
    scalars: usize,
    /// First structural closing-head offset in the original document.
    head_close: Option<usize>,
    /// First completed structural title's inner byte range.
    title_range: Option<(usize, usize)>,
}

/// Time a document's resolve may still take, on runtimes with a timer.
enum Deadline<'a> {
    /// No timer: calls are bounded only by backend timeouts.
    Unbounded,
    /// The timer has not fired yet.
    Running(LocalBoxFuture<'a, ()>),
    /// The timer fired; every further race loses at once.
    Expired,
}

impl<'a> Deadline<'a> {
    fn new(timer: Option<&'a dyn PlatformTimer>, budget: Duration) -> Self {
        timer.map_or(Self::Unbounded, |timer| Self::Running(timer.sleep(budget)))
    }

    /// Start the clock now. A timer's sleep starts on its first poll, which
    /// would otherwise be the first call that does not complete at once.
    async fn start(&mut self) {
        if let Self::Running(sleep) = self
            && futures::poll!(sleep.as_mut()).is_ready()
        {
            *self = Self::Expired;
        }
    }

    /// Run `work` to completion, or return `None` once the deadline passes.
    async fn race<T>(&mut self, work: impl Future<Output = T>) -> Option<T> {
        match self {
            Self::Unbounded => return Some(work.await),
            Self::Expired => return None,
            Self::Running(sleep) => {
                if let Either::Left((value, _)) = select(pin!(work), sleep.as_mut()).await {
                    return Some(value);
                }
            }
        }
        *self = Self::Expired;
        None
    }

    /// Whether the deadline has passed, so the last race lost.
    fn expired(&self) -> bool {
        matches!(self, Self::Expired)
    }
}

fn log_outcome(signed: bool, reason: Reason) {
    if reason == Reason::KvUnavailable && !KV_UNAVAILABLE_WARNED.swap(true, Ordering::Relaxed) {
        log::warn!(
            "encypher: the runtime KV store is unavailable, so pages pass through unsigned; bind the adapter's KV store to enable signing"
        );
    }
    log::info!(
        "encypher: outcome={} reason={}",
        if signed { "signed" } else { "passed_through" },
        reason.code()
    );
}

/// Whether `path` is one the article path never signs: an API, feed, admin
/// or well-known path, or an asset.
pub(crate) fn is_excluded_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    is_excluded_prefix(&lower)
        || SKIP_PATH_SUFFIXES
            .iter()
            .any(|suffix| lower.ends_with(suffix))
}

/// Whether `path` is an API, feed, admin or well-known path, where no media
/// is signed either. Asset suffixes are not excluded: media is.
pub(crate) fn is_excluded_prefix(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    SKIP_PATH_PREFIXES
        .iter()
        .any(|prefix| lower.starts_with(prefix))
}

impl Signer {
    /// Apply the request gates to a page. Only an admitted document is held.
    pub(crate) fn admit(&self, request: PageRequest) -> Result<PageRequest, Gate> {
        self.gate(request, true)
    }

    /// Apply the request gates to a stylesheet, whose `.css` path the page
    /// gates exclude.
    pub(crate) fn admit_stylesheet(&self, request: PageRequest) -> Result<PageRequest, Gate> {
        self.gate(request, false)
    }

    fn gate(&self, request: PageRequest, page: bool) -> Result<PageRequest, Gate> {
        if request.method != Method::GET {
            return Err(Gate::Method);
        }
        if request.response_status != StatusCode::OK.as_u16() {
            return Err(Gate::Status);
        }
        if request.origin_response_is_private {
            return Err(Gate::OriginPrivate);
        }
        let page_url = url::Url::parse(&request.page_url).map_err(|_| Gate::InvalidPageUrl)?;
        let host = page_url.host_str().ok_or(Gate::InvalidPageUrl)?;
        if !self.claims_host(host) {
            return Err(Gate::HostMismatch);
        }
        // The publisher's own path, as its site names it.
        let path = page_url.path();
        let path = path
            .strip_prefix(request.removed_prefix.as_str())
            .filter(|rest| rest.starts_with('/'))
            .unwrap_or(path);
        let excluded = if page {
            is_excluded_path(path)
        } else {
            is_excluded_prefix(path)
        };
        if excluded {
            return Err(Gate::ExcludedPath);
        }
        Ok(request)
    }

    /// Whether `host` is the claimed domain or its `www.` name.
    pub(crate) fn claims_host(&self, host: &str) -> bool {
        host.strip_prefix("www.").unwrap_or(host) == self.config.domain
    }

    /// Find the article and its signable text, with no I/O.
    fn extract(&self, html: &str) -> Result<Article, Reason> {
        let boundary = boundary::find_article_boundary(html, Some(&self.config.article_selector))
            .ok_or(Reason::NoBoundary)?;
        let article = html
            .get(boundary.start..boundary.end)
            .ok_or(Reason::NoBoundary)?;
        if embed::has_existing_markers(article) {
            return Err(Reason::AlreadySigned);
        }
        let metadata = text::document_metadata(html);
        if metadata.manifest_present {
            return Err(Reason::ManifestPresent);
        }
        let (visible, scalars) = text::visible_text(article, api::MAX_TEXT_SCALARS);
        if scalars < self.config.min_text_length {
            return Err(Reason::TextTooShort);
        }
        Ok(Article {
            boundary,
            text: visible,
            scalars,
            head_close: metadata.head_close,
            title_range: metadata.title_range,
        })
    }

    /// Unix time at which an entry written now with `ttl_seconds` expires.
    fn expires_at(ttl_seconds: u64) -> u64 {
        unix_now().saturating_add(ttl_seconds)
    }

    /// Bound for skip writes made after resolve work stopped.
    fn skip_deadline<'a>(services: &'a RuntimeServices) -> Deadline<'a> {
        Deadline::new(services.timer(), SKIP_WRITE_RESERVE)
    }

    /// Sign what a held document carries: the article's text first, as the
    /// page path always has, then each embedded media item, within one
    /// `max_hold_ms` budget. Returns the bytes to emit.
    async fn resolve_document(
        &self,
        target: &Target<'_>,
        services: &RuntimeServices,
        document: &str,
        article: Option<Article>,
        media: embedded::Scan,
    ) -> Vec<u8> {
        let request = target.request;
        let mut work = Deadline::new(
            services.timer(),
            Duration::from_millis(self.config.max_hold_ms).saturating_sub(SKIP_WRITE_RESERVE),
        );
        work.start().await;
        let policy_digest = match self
            .policy_digest(&request.page_url, services, services.kv_store(), &mut work)
            .await
        {
            Ok(policy_digest) => policy_digest,
            Err(reason) => {
                if article.is_some() {
                    log_outcome(false, reason);
                }
                media::log_unsigned(&media, reason);
                return document.as_bytes().to_vec();
            }
        };
        let mut output = Cow::Borrowed(document);
        if let Some(article) = &article {
            match self
                .resolve(
                    request,
                    services,
                    document,
                    article,
                    &policy_digest,
                    &mut work,
                )
                .await
            {
                Ok((signed, reason)) => {
                    log_outcome(true, reason);
                    output = Cow::Owned(signed);
                }
                Err(reason) => log_outcome(false, reason),
            }
        }
        if media.is_empty() {
            return output.into_owned().into_bytes();
        }
        // Markers go into text and the manifest meta tag into the head, never
        // into an attribute, a style or an SVG, so a rescan finds the same
        // media at its new offsets.
        let media = match (&output, target.surface) {
            (Cow::Owned(signed), Surface::Page) => embedded::scan_html(signed, target.policies),
            _ => media,
        };
        // The text plan may come from the cached policy, but media is signed
        // or reused only under the policy the service answers now, so a
        // revoked credential cannot reuse a cached media result.
        let media_policy = match self
            .request_policy(&request.page_url, services, &mut work)
            .await
        {
            Ok(policy_digest) => policy_digest,
            Err(reason) => {
                media::log_unsigned(&media, reason);
                return output.into_owned().into_bytes();
            }
        };
        let embedded = media::EmbeddedJob {
            request,
            surface: target.surface,
            policy_digest: &media_policy,
        };
        match self
            .sign_embedded(services, &embedded, &output, &media, &mut work)
            .await
        {
            Some(signed) => signed.into_bytes(),
            None => output.into_owned().into_bytes(),
        }
    }

    /// Resolve the plan for `article` under `policy_digest` and render it
    /// into `html`.
    async fn resolve(
        &self,
        request: &PageRequest,
        services: &RuntimeServices,
        html: &str,
        article: &Article,
        policy_digest: &str,
        work: &mut Deadline<'_>,
    ) -> Result<(String, Reason), Reason> {
        let kv = services.kv_store();
        let suffix = format!(
            "{}:{policy_digest}:{}",
            self.cache_context,
            text::content_hash_hex(&article.text)
        );
        let plan_key = format!("{PLAN_KEY_PREFIX}:{suffix}");
        let skip_key = format!("{PLAN_SKIP_KEY_PREFIX}:{suffix}");
        let outcome = match self.read::<CacheEntry>(kv, &plan_key, work).await {
            Ok(Some(CacheEntry::Plan {
                plan, manifest_url, ..
            })) => {
                if api::validate_plan(&plan, manifest_url.as_deref(), article.scalars).is_err() {
                    self.delete_invalid_entry(kv, &plan_key, work).await;
                    return Err(Reason::CachedPlanInvalid);
                }
                return render(
                    html,
                    article,
                    &plan,
                    manifest_url.as_deref(),
                    self.max_document_bytes,
                )
                .map(|signed| (signed, Reason::CacheHit))
                .map_err(render_reason);
            }
            Ok(Some(CacheEntry::Skip { .. })) => {
                self.delete_invalid_entry(kv, &plan_key, work).await;
                return Err(Reason::CachedEntryInvalid);
            }
            Err(Reason::Timeout) => Err(Reason::Timeout),
            Err(reason) => return Err(reason),
            Ok(None) => match self.read::<CacheEntry>(kv, &skip_key, work).await {
                Ok(Some(CacheEntry::Skip { .. })) => return Err(Reason::CachedSkip),
                Ok(Some(CacheEntry::Plan { .. })) => {
                    self.delete_invalid_entry(kv, &skip_key, work).await;
                    return Err(Reason::CachedEntryInvalid);
                }
                Err(Reason::Timeout) => Err(Reason::Timeout),
                Err(reason) => return Err(reason),
                Ok(None) => self
                    .request_plan(request, services, html, article, policy_digest, work)
                    .await
                    .and_then(|(plan, manifest_url)| {
                        api::validate_plan(&plan, manifest_url.as_deref(), article.scalars)
                            .map_err(|_| Reason::InvalidPlan)?;
                        let signed = render(
                            html,
                            article,
                            &plan,
                            manifest_url.as_deref(),
                            self.max_document_bytes,
                        )
                        .map_err(render_reason)?;
                        Ok((signed, plan, manifest_url))
                    }),
            },
        };
        match outcome {
            Ok((signed, plan, manifest_url)) => {
                // A plan write that runs out of time is abandoned; the page is still signed.
                let entry = CacheEntry::Plan {
                    plan,
                    manifest_url,
                    expires_at: Self::expires_at(self.config.plan_ttl_seconds),
                    ttl_less: false,
                };
                self.write(kv, &plan_key, entry, self.config.plan_ttl_seconds, work)
                    .await;
                Ok((signed, Reason::Signed))
            }
            Err(reason) => {
                let mut deadline = Self::skip_deadline(services);
                if matches!(reason, Reason::PolicyChanged | Reason::PolicyDigestMismatch) {
                    self.evict_policy(kv, &mut deadline).await;
                }
                self.write_skip(kv, &skip_key, reason, &mut deadline).await;
                Err(reason)
            }
        }
    }

    async fn policy_digest(
        &self,
        page_url: &str,
        services: &RuntimeServices,
        kv: &dyn PlatformKvStore,
        work: &mut Deadline<'_>,
    ) -> Result<String, Reason> {
        match self.read::<PolicyEntry>(kv, &self.policy_key, work).await {
            Ok(Some(PolicyEntry::Digest { policy_digest, .. }))
                if api::valid_policy_digest(&policy_digest) =>
            {
                return Ok(policy_digest);
            }
            Ok(Some(PolicyEntry::Digest { .. })) | Ok(Some(PolicyEntry::Skip { .. })) => {
                self.delete_invalid_entry(kv, &self.policy_key, work).await;
                return Err(Reason::CachedEntryInvalid);
            }
            // Nothing can be cached without a store.
            Err(Reason::KvUnavailable) => return Err(Reason::KvUnavailable),
            Err(reason) => return Err(reason),
            Ok(None) => {}
        }
        match self
            .read::<PolicyEntry>(kv, &self.policy_skip_key, work)
            .await
        {
            Ok(Some(PolicyEntry::Skip { .. })) => return Err(Reason::CachedSkip),
            Ok(Some(PolicyEntry::Digest { .. })) => {
                self.delete_invalid_entry(kv, &self.policy_skip_key, work)
                    .await;
                return Err(Reason::CachedEntryInvalid);
            }
            Err(Reason::KvUnavailable) => return Err(Reason::KvUnavailable),
            Err(reason) => return Err(reason),
            Ok(None) => {}
        }

        let reason = match self.request_policy(page_url, services, work).await {
            Ok(policy_digest) => {
                let entry = PolicyEntry::Digest {
                    policy_digest: policy_digest.clone(),
                    expires_at: Self::expires_at(self.config.policy_ttl_seconds),
                    ttl_less: false,
                };
                self.write(
                    kv,
                    &self.policy_key,
                    entry,
                    self.config.policy_ttl_seconds,
                    work,
                )
                .await;
                if !work.expired() {
                    return Ok(policy_digest);
                }
                // The digest is not cached, so the next view could not find a
                // plan skip written under it; skip at the policy level instead.
                Reason::Timeout
            }
            Err(reason) => reason,
        };
        let ttl = self.policy_skip_ttl();
        let entry = PolicyEntry::Skip {
            reason,
            expires_at: Self::expires_at(ttl),
            ttl_less: false,
        };
        self.write(
            kv,
            &self.policy_skip_key,
            entry,
            ttl,
            &mut Self::skip_deadline(services),
        )
        .await;
        Err(reason)
    }

    fn policy_skip_ttl(&self) -> u64 {
        self.config
            .policy_ttl_seconds
            .min(self.config.negative_ttl_seconds)
    }

    async fn request_policy(
        &self,
        page_url: &str,
        services: &RuntimeServices,
        work: &mut Deadline<'_>,
    ) -> Result<String, Reason> {
        let mut url = url::Url::parse(&self.policy_url).map_err(|_| Reason::RequestBuild)?;
        url.query_pairs_mut().append_pair("page_url", page_url);
        let body = self
            .fetch(
                services,
                Method::GET,
                url.as_str(),
                EdgeBody::empty(),
                api::MAX_POLICY_RESPONSE_BYTES,
                work,
            )
            .await?;
        api::parse_policy_response(&body).map_err(|error| parse_error_reason(&error))
    }

    async fn evict_policy(&self, kv: &dyn PlatformKvStore, deadline: &mut Deadline<'_>) {
        if !self.delete_cache_key(kv, &self.policy_key, deadline).await {
            log::debug!("encypher: policy digest eviction failed");
        }
        if !self
            .delete_cache_key(kv, &self.policy_skip_key, deadline)
            .await
        {
            log::debug!("encypher: policy skip eviction failed");
        }
    }

    /// Call the sign API once. Every failure maps to a reason code.
    async fn request_plan(
        &self,
        request: &PageRequest,
        services: &RuntimeServices,
        html: &str,
        article: &Article,
        policy_digest: &str,
        work: &mut Deadline<'_>,
    ) -> Result<(EmbeddingPlan, Option<String>), Reason> {
        let title = text::title_from_range(html, article.title_range);
        let body = serde_json::to_vec(&SignRequest {
            text: &article.text,
            page_url: &request.page_url,
            document_title: title.as_deref(),
            boundary_selector: &article.boundary.selector,
            expected_policy_digest: policy_digest,
        })
        .map_err(|_| Reason::RequestBuild)?;
        let body = self
            .fetch(
                services,
                Method::POST,
                &self.sign_url,
                EdgeBody::from(body),
                api::MAX_RESPONSE_BYTES,
                work,
            )
            .await?;
        let parsed = api::parse_sign_response(&body).map_err(|error| parse_error_reason(&error))?;
        if !parsed.success {
            return Err(if parsed.error.as_deref() == Some("quota_exceeded") {
                Reason::QuotaExceeded
            } else {
                Reason::SignRejected
            });
        }
        if parsed.policy_digest.as_deref() != Some(policy_digest) {
            return Err(Reason::PolicyDigestMismatch);
        }
        let plan = parsed.embedding_plan.ok_or(Reason::NoPlan)?;
        Ok((plan, parsed.manifest_url))
    }

    /// The worker token header value, marked sensitive so no log shows it.
    fn worker_token(&self) -> Result<HeaderValue, Reason> {
        let mut token = HeaderValue::from_str(self.config.worker_token.expose().trim())
            .map_err(|_| Reason::RequestBuild)?;
        token.set_sensitive(true);
        Ok(token)
    }

    /// One bounded JSON HTTP call; never retried.
    async fn fetch(
        &self,
        services: &RuntimeServices,
        method: Method,
        url: &str,
        body: EdgeBody,
        max_response_bytes: usize,
        work: &mut Deadline<'_>,
    ) -> Result<Bytes, Reason> {
        let http_request = Request::builder()
            .method(method)
            .uri(url)
            .header(header::CONTENT_TYPE, "application/json")
            .header(WORKER_TOKEN_HEADER, self.worker_token()?)
            .body(body)
            .map_err(|_| Reason::RequestBuild)?;
        let response_limit = max_response_bytes.saturating_add(1);
        let response = self
            .send_api(services, http_request, response_limit, work)
            .await?;
        let status = response.status();
        if status != StatusCode::OK {
            return Err(match status.as_u16() {
                403 => Reason::Forbidden,
                409 => Reason::PolicyChanged,
                422 => Reason::Unprocessable,
                429 => Reason::RateLimited,
                500..=599 => Reason::ServerError,
                _ => Reason::UnexpectedStatus,
            });
        }
        // One byte over the limit is enough for the parser to report TooLarge.
        match work
            .race(response.into_body().into_bytes_bounded(response_limit))
            .await
        {
            None => Err(Reason::Timeout),
            Some(Err(_)) => Err(Reason::ResponseTooLarge),
            Some(Ok(body)) => Ok(body),
        }
    }

    /// Send one request to the API backend, bounded by the first-byte
    /// timeout and `work`, reading at most `response_limit` body bytes.
    async fn send_api(
        &self,
        services: &RuntimeServices,
        http_request: Request<EdgeBody>,
        response_limit: usize,
        work: &mut Deadline<'_>,
    ) -> Result<http::Response<EdgeBody>, Reason> {
        let backend = ensure_integration_backend_with_timeout(
            services,
            &self.config.api_base_url,
            INTEGRATION_ID,
            Duration::from_millis(self.config.timeout_ms),
        )
        .map_err(|_| Reason::Backend)?;
        let platform_request = PlatformHttpRequest::new(http_request, backend)
            .with_cache_bypass()
            .with_max_response_body_bytes(response_limit);
        match work
            .race(services.http_client().send(platform_request))
            .await
        {
            None => Err(Reason::Timeout),
            Some(Err(_)) => Err(Reason::HttpError),
            Some(Ok(response)) => Ok(response.response),
        }
    }

    /// Read a fresh entry. Expired entries are misses; in TTL-less write mode
    /// they are also deleted, best effort, so the store cleans itself.
    async fn read<T: DeserializeOwned + Expiring>(
        &self,
        kv: &dyn PlatformKvStore,
        key: &str,
        deadline: &mut Deadline<'_>,
    ) -> Result<Option<T>, Reason> {
        let bytes = match deadline.race(kv.get_bytes(key)).await {
            None => return Err(Reason::Timeout),
            Some(Err(KvError::Unavailable)) => return Err(Reason::KvUnavailable),
            Some(Err(_)) => return Err(Reason::KvError),
            Some(Ok(None)) => return Ok(None),
            Some(Ok(Some(bytes))) => bytes,
        };
        let entry: T = match serde_json::from_slice(&bytes) {
            Ok(entry) => entry,
            Err(_) => {
                self.delete_invalid_entry(kv, key, deadline).await;
                return Err(Reason::CachedEntryInvalid);
            }
        };
        // An entry written without a TTL shows the mode to an instance that has
        // not written yet, as on Spin, which instantiates the module per request.
        if entry.ttl_less() {
            self.ttl_less.store(true, Ordering::Relaxed);
        }
        if is_fresh(entry.expires_at(), unix_now()) {
            return Ok(Some(entry));
        }
        if self.ttl_less.load(Ordering::Relaxed) && !self.delete_cache_key(kv, key, deadline).await
        {
            log::debug!("encypher: expired cache entry could not be deleted");
        }
        Ok(None)
    }

    /// Best-effort cache deletion, bounded by the caller's existing deadline.
    async fn delete_cache_key(
        &self,
        kv: &dyn PlatformKvStore,
        key: &str,
        deadline: &mut Deadline<'_>,
    ) -> bool {
        matches!(deadline.race(kv.delete(key)).await, Some(Ok(())))
    }

    /// Delete a malformed or structurally invalid entry without masking fail-open.
    async fn delete_invalid_entry(
        &self,
        kv: &dyn PlatformKvStore,
        key: &str,
        deadline: &mut Deadline<'_>,
    ) {
        if !self.delete_cache_key(kv, key, deadline).await {
            log::debug!("encypher: invalid cache entry could not be deleted");
        }
    }

    /// Best-effort cache write; failures are logged and ignored.
    ///
    /// Writes carry a TTL until the store first reports TTL writes as
    /// unsupported; from then on this instance writes without one, and marks
    /// the entry so a later instance that reads it knows the mode too.
    async fn write<E: Serialize + Expiring>(
        &self,
        kv: &dyn PlatformKvStore,
        key: &str,
        mut entry: E,
        ttl_seconds: u64,
        deadline: &mut Deadline<'_>,
    ) {
        if !self.ttl_less.load(Ordering::Relaxed) {
            let Some(value) = encode(&entry) else {
                return;
            };
            let ttl = Duration::from_secs(ttl_seconds);
            match deadline.race(kv.put_bytes_with_ttl(key, value, ttl)).await {
                Some(Ok(())) => return,
                Some(Err(KvError::Unsupported { .. })) => {
                    self.ttl_less.store(true, Ordering::Relaxed);
                    log::info!(
                        "encypher: the KV store does not support TTL writes; entries expire by their expires_at"
                    );
                }
                Some(Err(_)) => {
                    log::debug!("encypher: cache write failed");
                    return;
                }
                None => {
                    log::debug!("encypher: cache write timed out");
                    return;
                }
            }
        }
        entry.mark_ttl_less();
        let Some(value) = encode(&entry) else {
            return;
        };
        match deadline.race(kv.put_bytes(key, value)).await {
            Some(Ok(())) => {}
            Some(Err(_)) => log::debug!("encypher: cache write failed"),
            None => log::debug!("encypher: cache write timed out"),
        }
    }

    /// Write a negative article entry under its separate skip key.
    async fn write_skip(
        &self,
        kv: &dyn PlatformKvStore,
        key: &str,
        reason: Reason,
        deadline: &mut Deadline<'_>,
    ) {
        let ttl = self.config.negative_ttl_seconds;
        let entry = CacheEntry::Skip {
            reason,
            expires_at: Self::expires_at(ttl),
            ttl_less: false,
        };
        self.write(kv, key, entry, ttl, deadline).await;
    }
}

/// Serialize a cache entry; a failure is logged and the write skipped.
fn encode(entry: &impl Serialize) -> Option<Bytes> {
    match serde_json::to_vec(entry) {
        Ok(value) => Some(Bytes::from(value)),
        Err(_) => {
            log::debug!("encypher: cache entry could not be serialized");
            None
        }
    }
}

/// Apply a validated plan and optional manifest in one exact-sized output.
fn render(
    html: &str,
    article: &Article,
    plan: &EmbeddingPlan,
    manifest_url: Option<&str>,
    max_output_bytes: usize,
) -> Result<String, embed::ApplyError> {
    let manifest = manifest_url
        .filter(|url| !url.is_empty())
        .and_then(|url| article.head_close.map(|offset| (url, offset)));
    embed::apply_plan(html, &article.boundary, plan, manifest, max_output_bytes)
}

fn render_reason(error: embed::ApplyError) -> Reason {
    match error {
        embed::ApplyError::Alignment => Reason::AlignmentFailed,
        embed::ApplyError::TooLarge => Reason::OutputTooLarge,
    }
}

fn parse_error_reason(error: &ApiError) -> Reason {
    match error {
        ApiError::TooLarge => Reason::ResponseTooLarge,
        ApiError::Malformed => Reason::MalformedResponse,
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::sync::atomic::AtomicUsize;
    use std::sync::{LazyLock, Mutex};
    use std::task::Poll;

    use error_stack::Report;
    use serde_json::json;

    use super::*;
    use trusted_server_core::platform::test_support::{
        StoppedTimer, StubHttpClient, build_services_with_http_client,
    };
    use trusted_server_core::platform::{
        PlatformCacheIntent, PlatformError, PlatformHttpClient, PlatformPendingRequest,
        PlatformResponse, PlatformSelectResult,
    };

    const TOKEN: &str = "cwk_test_worker_token_value";
    const PAGE_URL: &str = "https://www.publisher.example.com/news/harbor-vote";
    const MARKER: &str = "\u{E0100}\u{E0101}\u{FE01}";
    const MANIFEST_URL: &str = "https://api.example.com/manifests/doc-1";
    const POLICY_DIGEST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const ARTICLE: &str = "<!doctype html><html><head><title>Harbor Council Votes</title></head>\
        <body><nav>Home</nav><article><h1>Harbor council votes on the ferry schedule</h1>\
        <p>The harbor council met on Tuesday evening and approved a revised ferry schedule \
        for the coming season.</p><p>Residents asked for earlier departures on weekdays.</p>\
        </article><footer>Footer</footer></body></html>";
    /// Simulated time that passes each time a [`TickTimer`] sleep is polled.
    const TICK: Duration = Duration::from_millis(100);

    // --- test doubles -------------------------------------------------------

    /// Return `Pending` `times` times, waking the task each time, then `Ready`.
    async fn yield_pending(times: usize) {
        for _ in 0..times {
            let mut yielded = false;
            futures::future::poll_fn(|cx| {
                if yielded {
                    Poll::Ready(())
                } else {
                    yielded = true;
                    cx.waker().wake_by_ref();
                    Poll::Pending
                }
            })
            .await;
        }
    }

    /// A timer on simulated time: each poll of a sleep advances it by [`TICK`].
    /// Sleeps only advance while the work they race is pending, as on a real
    /// runtime where the clock runs while I/O is outstanding.
    #[derive(Default)]
    struct TickTimer {
        requested: Mutex<Vec<Duration>>,
    }

    #[async_trait::async_trait(?Send)]
    impl PlatformTimer for TickTimer {
        async fn sleep(&self, duration: Duration) {
            self.requested
                .lock()
                .expect("should lock requested durations")
                .push(duration);
            let ticks = duration.as_millis().div_ceil(TICK.as_millis());
            yield_pending(usize::try_from(ticks).expect("ticks should fit")).await;
        }
    }

    /// In-memory KV store that counts calls and can fail, stall or refuse TTLs.
    #[derive(Default)]
    struct MemoryKv {
        entries: Mutex<HashMap<String, Bytes>>,
        ttls: Mutex<HashMap<String, Duration>>,
        deletes: Mutex<Vec<String>>,
        reads: AtomicUsize,
        ttl_writes: AtomicUsize,
        /// Plan-key reads that report a miss before reads see the stored entry.
        hidden_reads: AtomicUsize,
        /// Every read fails with [`KvError::Unavailable`], as an unbound store does.
        unavailable: bool,
        /// Every read fails with an error other than unavailability.
        fail_reads: bool,
        /// Every read never completes.
        hung_reads: bool,
        /// Writes to keys with this prefix never complete.
        hung_writes_to: Option<&'static str>,
        /// The first write to this exact key never completes; later writes succeed.
        hung_first_write_to: Option<String>,
        hung_first_write_taken: AtomicBool,
        /// TTL writes fail with [`KvError::Unsupported`], as on Spin.
        ttl_unsupported: bool,
        /// `Pending` polls before every operation completes.
        pending_polls: usize,
        /// Positive entry inserted after a negative writer's final read but before its put.
        concurrent_positive_before_skip_write: Mutex<Option<(String, Bytes)>>,
    }

    impl MemoryKv {
        fn entries(&self) -> HashMap<String, Bytes> {
            self.entries.lock().expect("should lock entries").clone()
        }

        fn entry(&self, key: &str) -> serde_json::Value {
            serde_json::from_slice(&self.entries()[key]).expect("should store JSON")
        }

        fn insert(&self, key: &str, value: &serde_json::Value) {
            self.entries.lock().expect("should lock entries").insert(
                key.to_owned(),
                Bytes::from(serde_json::to_vec(value).expect("should serialize")),
            );
        }

        fn insert_raw(&self, key: &str, value: impl Into<Bytes>) {
            self.entries
                .lock()
                .expect("should lock entries")
                .insert(key.to_owned(), value.into());
        }

        fn entry_bytes(&self, key: &str) -> Bytes {
            self.entries()[key].clone()
        }

        fn remove(&self, key: &str) {
            self.entries
                .lock()
                .expect("should lock entries")
                .remove(key);
        }

        /// The single article entry under either current article namespace.
        fn only_entry(&self) -> (String, serde_json::Value) {
            let entries: Vec<_> = self
                .entries()
                .into_keys()
                .filter(|key| {
                    key.starts_with(PLAN_KEY_PREFIX) || key.starts_with(PLAN_SKIP_KEY_PREFIX)
                })
                .collect();
            assert_eq!(entries.len(), 1, "should hold exactly one article entry");
            let key = entries.into_iter().next().expect("should have an entry");
            let value = self.entry(&key);
            (key, value)
        }

        fn reads(&self) -> usize {
            self.reads.load(Ordering::SeqCst)
        }

        async fn store(&self, key: &str, value: Bytes) -> Result<(), KvError> {
            yield_pending(self.pending_polls).await;
            if self
                .hung_writes_to
                .is_some_and(|prefix| key.starts_with(prefix))
                || (self.hung_first_write_to.as_deref() == Some(key)
                    && !self.hung_first_write_taken.swap(true, Ordering::SeqCst))
            {
                futures::future::pending::<()>().await;
            }
            let writes_skip = serde_json::from_slice::<serde_json::Value>(&value)
                .ok()
                .and_then(|entry| {
                    entry
                        .get("kind")
                        .and_then(|kind| kind.as_str())
                        .map(str::to_owned)
                })
                .as_deref()
                == Some("skip");
            if writes_skip
                && let Some((positive_key, positive)) = self
                    .concurrent_positive_before_skip_write
                    .lock()
                    .expect("should lock concurrent write")
                    .take()
            {
                self.entries
                    .lock()
                    .expect("should lock entries")
                    .insert(positive_key, positive);
            }
            self.entries
                .lock()
                .expect("should lock entries")
                .insert(key.to_owned(), value);
            Ok(())
        }
    }

    #[async_trait::async_trait(?Send)]
    impl PlatformKvStore for MemoryKv {
        async fn get_bytes(&self, key: &str) -> Result<Option<Bytes>, KvError> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            yield_pending(self.pending_polls).await;
            if self.hung_reads {
                futures::future::pending::<()>().await;
            }
            if self.unavailable {
                return Err(KvError::Unavailable);
            }
            if self.fail_reads {
                return Err(KvError::LimitExceeded {
                    message: "test failure".to_owned(),
                });
            }
            if key.starts_with(PLAN_KEY_PREFIX)
                && self
                    .hidden_reads
                    .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                    .is_ok()
            {
                return Ok(None);
            }
            Ok(self
                .entries
                .lock()
                .expect("should lock entries")
                .get(key)
                .cloned())
        }

        async fn put_bytes(&self, key: &str, value: Bytes) -> Result<(), KvError> {
            self.store(key, value).await
        }

        async fn put_bytes_with_ttl(
            &self,
            key: &str,
            value: Bytes,
            ttl: Duration,
        ) -> Result<(), KvError> {
            self.ttl_writes.fetch_add(1, Ordering::SeqCst);
            if self.ttl_unsupported {
                return Err(KvError::Unsupported {
                    operation: "put_bytes_with_ttl".to_owned(),
                });
            }
            self.ttls
                .lock()
                .expect("should lock TTLs")
                .insert(key.to_owned(), ttl);
            self.store(key, value).await
        }

        async fn delete(&self, key: &str) -> Result<(), KvError> {
            yield_pending(self.pending_polls).await;
            self.deletes
                .lock()
                .expect("should lock deletes")
                .push(key.to_owned());
            self.remove(key);
            Ok(())
        }

        async fn list_keys_page(
            &self,
            _prefix: &str,
            _cursor: Option<&str>,
            _limit: usize,
        ) -> Result<edgezero_core::key_value_store::KvPage, KvError> {
            Ok(edgezero_core::key_value_store::KvPage::default())
        }
    }

    /// HTTP client whose send never completes.
    #[derive(Default)]
    struct HungHttpClient {
        sends: AtomicUsize,
    }

    #[async_trait::async_trait(?Send)]
    impl PlatformHttpClient for HungHttpClient {
        async fn send(
            &self,
            _request: PlatformHttpRequest,
        ) -> Result<PlatformResponse, Report<PlatformError>> {
            self.sends.fetch_add(1, Ordering::SeqCst);
            futures::future::pending().await
        }

        async fn send_async(
            &self,
            _request: PlatformHttpRequest,
        ) -> Result<PlatformPendingRequest, Report<PlatformError>> {
            Err(Report::new(PlatformError::HttpClient))
        }

        async fn select(
            &self,
            _pending_requests: Vec<PlatformPendingRequest>,
        ) -> Result<PlatformSelectResult, Report<PlatformError>> {
            Err(Report::new(PlatformError::HttpClient))
        }
    }

    /// Wraps [`StubHttpClient`] so the send and the response body each return
    /// `Pending` before they are ready, as on Cloudflare, Spin and Axum.
    struct YieldingHttpClient {
        inner: Arc<StubHttpClient>,
        pending_polls: usize,
    }

    #[async_trait::async_trait(?Send)]
    impl PlatformHttpClient for YieldingHttpClient {
        async fn send(
            &self,
            request: PlatformHttpRequest,
        ) -> Result<PlatformResponse, Report<PlatformError>> {
            yield_pending(self.pending_polls).await;
            let response = self.inner.send(request).await?;
            let (parts, body) = response.response.into_parts();
            let bytes = body
                .into_bytes_bounded(usize::MAX)
                .await
                .expect("should read the stub body");
            let polls = self.pending_polls;
            let body = EdgeBody::stream(futures::stream::once(async move {
                yield_pending(polls).await;
                bytes
            }));
            Ok(PlatformResponse::new(http::Response::from_parts(
                parts, body,
            )))
        }

        async fn send_async(
            &self,
            _request: PlatformHttpRequest,
        ) -> Result<PlatformPendingRequest, Report<PlatformError>> {
            Err(Report::new(PlatformError::HttpClient))
        }

        async fn select(
            &self,
            _pending_requests: Vec<PlatformPendingRequest>,
        ) -> Result<PlatformSelectResult, Report<PlatformError>> {
            Err(Report::new(PlatformError::HttpClient))
        }
    }

    /// Records log lines, with their level, emitted on the current thread.
    struct ThreadLogger;

    thread_local! {
        static LOGS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    }

    impl log::Log for ThreadLogger {
        fn enabled(&self, _metadata: &log::Metadata<'_>) -> bool {
            true
        }

        fn log(&self, record: &log::Record<'_>) {
            LOGS.with(|logs| {
                logs.borrow_mut()
                    .push(format!("{} {}", record.level(), record.args()));
            });
        }

        fn flush(&self) {}
    }

    static LOGGER: ThreadLogger = ThreadLogger;

    /// A sign-failure case: name and the API response, `None` for a transport error.
    type FailureCase = (&'static str, Option<(u16, Vec<u8>)>);
    /// A gate case: name, signer, and the request mutation that trips the gate.
    type GateCase<'a> = (&'static str, &'a Arc<Signer>, fn(&mut PageRequest));

    /// Runs `run` with this thread's log lines captured. Returns `None`, after
    /// skipping `run`, when another logger already owns the process.
    fn capture_logs(run: impl FnOnce()) -> Option<Vec<String>> {
        static INSTALLED: LazyLock<bool> = LazyLock::new(|| {
            let installed = log::set_logger(&LOGGER).is_ok();
            if installed {
                log::set_max_level(log::LevelFilter::Trace);
            }
            installed
        });
        if !*INSTALLED {
            return None;
        }
        LOGS.with(|logs| logs.borrow_mut().clear());
        run();
        Some(LOGS.with(RefCell::take))
    }

    // --- fixtures -----------------------------------------------------------

    fn config_json(token: &str) -> serde_json::Value {
        json!({
            "api_base_url": "https://api.example.com",
            "domain": "publisher.example.com",
            "worker_token": token,
        })
    }

    fn factory_with(config: serde_json::Value, max_document_bytes: usize) -> Arc<Signer> {
        let config: EncypherConfig = serde_json::from_value(config).expect("should parse config");
        Arc::new(Signer::new(config, max_document_bytes))
    }

    fn factory() -> Arc<Signer> {
        factory_with(config_json(TOKEN), 5_000_000)
    }

    fn article_with_selector(html: &str, selector: Option<&str>) -> Article {
        let boundary =
            boundary::find_article_boundary(html, selector).expect("should find boundary");
        let metadata = text::document_metadata(html);
        let boundary_html = html
            .get(boundary.start..boundary.end)
            .expect("boundary should be valid");
        let (text, scalars) = text::visible_text(boundary_html, api::MAX_TEXT_SCALARS);
        Article {
            boundary,
            text,
            scalars,
            head_close: metadata.head_close,
            title_range: metadata.title_range,
        }
    }

    fn policy_key() -> String {
        factory().policy_key.clone()
    }

    fn policy_skip_key() -> String {
        factory().policy_skip_key.clone()
    }

    /// Services without a timer, as on Fastly.
    fn services(http: Arc<dyn PlatformHttpClient>, kv: &Arc<MemoryKv>) -> RuntimeServices {
        build_services_with_http_client(http)
            .with_kv_store(Arc::clone(kv) as Arc<dyn PlatformKvStore>)
    }

    /// Services with a [`TickTimer`], as on Cloudflare, Spin and Axum.
    fn timed_services(http: Arc<dyn PlatformHttpClient>, kv: &Arc<MemoryKv>) -> RuntimeServices {
        services(http, kv).with_timer(Arc::new(TickTimer::default()))
    }

    fn request() -> PageRequest {
        PageRequest {
            method: Method::GET,
            page_url: PAGE_URL.to_owned(),
            removed_prefix: String::new(),
            response_status: 200,
            origin_response_is_private: false,
        }
    }

    fn plan_json(marker: &str) -> serde_json::Value {
        json!({
            "index_unit": "codepoint",
            "operations": [{ "insert_after_index": 5, "marker": marker }],
        })
    }

    fn sign_response(marker: &str) -> Vec<u8> {
        sign_response_for_policy(marker, POLICY_DIGEST)
    }

    fn policy_response(digest: &str) -> Vec<u8> {
        serde_json::to_vec(&json!({"policy_digest": digest})).expect("should serialize policy")
    }

    fn sign_response_for_policy(marker: &str, digest: &str) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "success": true,
            "policy_digest": digest,
            "embedding_plan": plan_json(marker),
            "manifest_url": MANIFEST_URL,
            "verification_url": "https://api.example.com/verify/doc-1",
            "document_id": "doc-1",
        }))
        .expect("should serialize sign response")
    }

    fn signing_http() -> Arc<StubHttpClient> {
        let http = Arc::new(StubHttpClient::new());
        http.push_response(200, policy_response(POLICY_DIGEST));
        http.push_response(200, sign_response(MARKER));
        http
    }

    fn seed_plan(kv: &Arc<MemoryKv>) -> String {
        view(&factory(), &services(signing_http(), kv))
    }

    /// Drive one document the way the middleware and the publisher driver do:
    /// the gates on the request, when there is one, then two chunks, then the
    /// asynchronous finish with the core backstop. A request the gates refuse,
    /// or none, holds nothing, so the document passes through.
    fn run(
        factory: &Arc<Signer>,
        request: Option<PageRequest>,
        services: &RuntimeServices,
        html: &str,
    ) -> String {
        let Some(request) = request.and_then(|request| factory.admit(request).ok()) else {
            return html.to_owned();
        };
        futures::executor::block_on(async {
            let mut processor = document_processor(factory, request, None);
            let (first, second) = html.as_bytes().split_at(html.len() / 2);
            let mut output = processor
                .process_chunk(first, false)
                .expect("should process first chunk");
            output.extend(
                processor
                    .finish(second, services)
                    .await
                    .expect("should finish the document"),
            );
            String::from_utf8(output).expect("should emit UTF-8")
        })
    }

    /// One view of [`ARTICLE`] at [`PAGE_URL`].
    fn view(factory: &Arc<Signer>, services: &RuntimeServices) -> String {
        run(factory, Some(request()), services, ARTICLE)
    }

    fn assert_signed(output: &str) {
        assert!(output.contains(MARKER), "should embed the plan marker");
        assert!(
            output.contains("c2pa-manifest-url") && output.contains(MANIFEST_URL),
            "should inject the manifest meta tag"
        );
        assert_eq!(
            output.matches(MARKER).count(),
            1,
            "should add the marker once"
        );
    }

    /// Rewrite the `expires_at` of the entry at `key` to `expires_at`.
    fn set_expiry(kv: &MemoryKv, key: &str, expires_at: u64) {
        let mut entry = kv.entry(key);
        entry["expires_at"] = json!(expires_at);
        kv.insert(key, &entry);
    }

    fn expired() -> u64 {
        unix_now().saturating_sub(1)
    }

    // --- policy -------------------------------------------------------------

    #[test]
    fn policy_failures_are_negatively_cached_without_using_an_old_plan() {
        let cases: Vec<FailureCase> = vec![
            ("unavailable", Some((503, Vec::new()))),
            ("forbidden", Some((403, Vec::new()))),
            ("transport", None),
            ("malformed", Some((200, b"not json".to_vec()))),
            ("missing digest", Some((200, b"{}".to_vec()))),
            (
                "uppercase",
                Some((200, policy_response(&POLICY_DIGEST.to_uppercase()))),
            ),
            (
                "oversized",
                Some((200, vec![b' '; api::MAX_POLICY_RESPONSE_BYTES + 2])),
            ),
        ];
        for (case, response) in cases {
            let kv = Arc::new(MemoryKv::default());
            seed_plan(&kv);
            kv.remove(&policy_key());
            let http = Arc::new(StubHttpClient::new());
            if let Some((status, body)) = response {
                http.push_response(status, body);
            }
            let services = services(Arc::clone(&http) as Arc<dyn PlatformHttpClient>, &kv);
            for _ in 0..2 {
                assert_eq!(view(&factory(), &services), ARTICLE, "{case}");
            }
            assert_eq!(http.recorded_request_methods(), ["GET"], "{case}");
            assert_eq!(kv.entry(&policy_skip_key())["kind"], "skip", "{case}");
            assert_eq!(
                kv.ttls.lock().expect("lock")[&policy_skip_key()],
                Duration::from_secs(300)
            );
            // Once the skip is gone, refresh may reuse only the matching old plan.
            kv.remove(&policy_skip_key());
            http.push_response(200, policy_response(POLICY_DIGEST));
            assert_signed(&view(&factory(), &services));
            assert_eq!(http.recorded_request_methods(), ["GET", "GET"]);
        }
    }

    #[test]
    fn malformed_cached_policy_fails_closed_for_one_view_then_recovers() {
        let future = unix_now() + 300;
        for value in [
            json!("not an entry"),
            json!({"kind": "digest", "policy_digest": null, "expires_at": future}),
            json!({"kind": "digest", "policy_digest": "bad", "expires_at": future}),
            json!({
                "kind": "digest", "policy_digest": POLICY_DIGEST, "expires_at": future,
                "extra": true,
            }),
            // `expires_at` is required.
            json!({"kind": "digest", "policy_digest": POLICY_DIGEST}),
        ] {
            let kv = Arc::new(MemoryKv::default());
            seed_plan(&kv);
            let key = policy_key();
            kv.insert(&key, &value);
            let http = Arc::new(StubHttpClient::new());
            let services = services(Arc::clone(&http) as Arc<dyn PlatformHttpClient>, &kv);

            assert_eq!(view(&factory(), &services), ARTICLE, "{value}");
            assert!(http.recorded_request_methods().is_empty(), "{value}");
            assert!(
                kv.deletes.lock().expect("lock deletes").contains(&key),
                "{value}: malformed policy should be evicted"
            );
            assert!(
                !kv.entries().contains_key(&key),
                "{value}: malformed policy should not remain"
            );

            http.push_response(200, policy_response(POLICY_DIGEST));
            assert_signed(&view(&factory(), &services));
            assert_eq!(http.recorded_request_methods(), ["GET"], "{value}");
        }
    }

    #[test]
    fn policy_cache_hit_and_article_or_policy_changes_select_the_correct_plan() {
        let kv = Arc::new(MemoryKv::default());
        seed_plan(&kv);
        let http = Arc::new(StubHttpClient::new());
        http.push_response(200, sign_response(MARKER));
        let services = services(Arc::clone(&http) as Arc<dyn PlatformHttpClient>, &kv);
        let changed = ARTICLE.replace("Tuesday", "Wednesday");
        assert_signed(&run(&factory(), Some(request()), &services, &changed));
        assert_eq!(http.recorded_request_methods(), ["POST"]);

        kv.remove(&policy_key());
        let next_digest = "b".repeat(64);
        http.push_response(200, policy_response(&next_digest));
        http.push_response(200, sign_response_for_policy(MARKER, &next_digest));
        assert_signed(&view(&factory(), &services));
        assert_eq!(http.recorded_request_methods(), ["POST", "GET", "POST"]);
        let count = http.recorded_request_methods().len();
        assert_signed(&view(&factory(), &services));
        assert_eq!(http.recorded_request_methods().len(), count);
        // Returning to the original policy selects its original plan, not the intervening one.
        kv.remove(&policy_key());
        http.push_response(200, policy_response(POLICY_DIGEST));
        assert_signed(&view(&factory(), &services));
        assert_eq!(
            http.recorded_request_methods(),
            ["POST", "GET", "POST", "GET"]
        );
    }

    #[test]
    fn conflict_or_response_digest_mismatch_evicts_policy_without_retrying() {
        let mut missing: serde_json::Value =
            serde_json::from_slice(&sign_response(MARKER)).expect("JSON");
        missing
            .as_object_mut()
            .expect("object")
            .remove("policy_digest");
        for (status, body) in [
            (
                409,
                br#"{"detail":"TRUSTED_SERVER_POLICY_CHANGED"}"#.to_vec(),
            ),
            (409, b"unparseable conflict".to_vec()),
            (200, sign_response_for_policy(MARKER, &"b".repeat(64))),
            (200, sign_response_for_policy(MARKER, "invalid")),
            (200, serde_json::to_vec(&missing).expect("JSON")),
        ] {
            let kv = Arc::new(MemoryKv::default());
            let http = Arc::new(StubHttpClient::new());
            http.push_response(200, policy_response(POLICY_DIGEST));
            http.push_response(status, body);
            let services = services(Arc::clone(&http) as Arc<dyn PlatformHttpClient>, &kv);
            assert_eq!(view(&factory(), &services), ARTICLE);
            assert_eq!(http.recorded_request_methods(), ["GET", "POST"]);
            assert!(!kv.entries().contains_key(&policy_key()));
            assert_eq!(kv.only_entry().1["kind"], "skip");
            // Next request refetches policy, but the failed article remains negatively cached.
            http.push_response(200, policy_response(POLICY_DIGEST));
            assert_eq!(view(&factory(), &services), ARTICLE);
            assert_eq!(http.recorded_request_methods(), ["GET", "POST", "GET"]);
        }
    }

    #[test]
    fn entries_expire_at_their_ttl_and_the_store_ttl_matches() {
        let kv = Arc::new(MemoryKv::default());
        let before = unix_now();
        seed_plan(&kv);
        let after = unix_now();
        let ttls = kv.ttls.lock().expect("lock").clone();
        let (plan_key, plan) = kv.only_entry();
        for (key, entry, ttl) in [
            (policy_key(), kv.entry(&policy_key()), 300),
            (plan_key, plan, 604_800),
        ] {
            assert_eq!(ttls[&key], Duration::from_secs(ttl));
            let expires_at = entry["expires_at"]
                .as_u64()
                .expect("should carry expires_at");
            assert!(
                (before + ttl..=after + ttl).contains(&expires_at),
                "{key} should expire {ttl}s after it was written"
            );
        }
        for (policy_ttl, negative_ttl, expected) in [(60, 300, 60), (300, 60, 60)] {
            let kv = Arc::new(MemoryKv::default());
            let http = Arc::new(StubHttpClient::new());
            http.push_response(503, Vec::new());
            let mut config = config_json(TOKEN);
            config["policy_ttl_seconds"] = json!(policy_ttl);
            config["negative_ttl_seconds"] = json!(negative_ttl);
            let factory = factory_with(config, 5_000_000);
            assert_eq!(view(&factory, &services(http, &kv)), ARTICLE);
            assert_eq!(
                kv.ttls.lock().expect("lock")[&factory.policy_skip_key],
                Duration::from_secs(expected)
            );
        }
    }

    // --- sign path and cache ------------------------------------------------

    #[test]
    fn signs_on_a_miss_with_the_worker_token_and_caches_the_plan() {
        let http = signing_http();
        let kv = Arc::new(MemoryKv::default());
        let services = services(Arc::clone(&http) as Arc<dyn PlatformHttpClient>, &kv);

        let output = view(&factory(), &services);

        assert_signed(&output);
        assert_eq!(http.recorded_request_methods(), ["GET", "POST"]);
        assert_eq!(
            http.recorded_cache_intents(),
            [PlatformCacheIntent::Bypass, PlatformCacheIntent::Bypass],
            "credential-bound policy and sign responses must bypass intermediary caches"
        );
        assert_eq!(
            http.recorded_request_uris(),
            [
                format!(
                    "https://api.example.com/api/v1/public/cdn/trusted-server/policy?page_url={}",
                    url::form_urlencoded::byte_serialize(PAGE_URL.as_bytes()).collect::<String>()
                ),
                "https://api.example.com/api/v1/public/cdn/sign".to_owned(),
            ]
        );
        let headers = &http.recorded_request_headers()[1];
        let header = |name: &str| {
            headers
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.as_str())
        };
        assert_eq!(header("x-encypher-worker-token"), Some(TOKEN));
        assert_eq!(header("content-type"), Some("application/json"));
        assert_eq!(
            header("idempotency-key"),
            None,
            "should send no idempotency key"
        );
        let body: serde_json::Value = serde_json::from_slice(&http.recorded_request_bodies()[1])
            .expect("should send a JSON body");
        assert_eq!(body["page_url"], PAGE_URL);
        assert_eq!(body["expected_policy_digest"], POLICY_DIGEST);
        assert_eq!(body["document_title"], "Harbor Council Votes");
        assert_eq!(body["boundary_selector"], "article");
        let sent_text = body["text"].as_str().expect("should send text");
        assert!(
            sent_text.starts_with("Harbor council votes on the ferry schedule The harbor council"),
            "should send the article's visible text, got {sent_text}"
        );
        assert!(!sent_text.contains("Footer") && !sent_text.contains("Home"));
        let (key, entry) = kv.only_entry();
        assert!(
            key.starts_with("encypher:plan:v4:") && key.contains(POLICY_DIGEST),
            "unexpected cache key {key}"
        );
        assert!(!key.contains(TOKEN), "cache key must not contain the token");
        assert!(
            policy_key().starts_with("encypher:policy:v3:"),
            "unexpected policy key"
        );
        assert_eq!(entry["kind"], "plan");
        assert_eq!(entry["plan"], plan_json(MARKER));
    }

    #[test]
    fn a_fresh_plan_that_expands_past_the_document_budget_passes_through() {
        let http = signing_http();
        let kv = Arc::new(MemoryKv::default());
        let signer = factory_with(config_json(TOKEN), ARTICLE.len());

        let output = view(
            &signer,
            &services(Arc::clone(&http) as Arc<dyn PlatformHttpClient>, &kv),
        );

        assert_eq!(
            output, ARTICLE,
            "the original document fits its exact budget"
        );
        assert_eq!(http.recorded_request_methods(), ["GET", "POST"]);
    }

    #[test]
    fn a_cached_plan_that_expands_past_the_document_budget_passes_through() {
        let kv = Arc::new(MemoryKv::default());
        seed_plan(&kv);
        let http = Arc::new(StubHttpClient::new());
        let signer = factory_with(config_json(TOKEN), ARTICLE.len());

        let output = view(
            &signer,
            &services(Arc::clone(&http) as Arc<dyn PlatformHttpClient>, &kv),
        );

        assert_eq!(
            output, ARTICLE,
            "the original document fits its exact budget"
        );
        assert!(
            http.recorded_request_methods().is_empty(),
            "a cached plan should not call the API"
        );
    }

    #[test]
    fn render_budget_matches_the_exact_escaped_output_size() {
        let article = factory().extract(ARTICLE).expect("should extract article");
        let plan: EmbeddingPlan =
            serde_json::from_value(plan_json(MARKER)).expect("should build a valid plan");
        let manifest_url = r#"https://manifest.example/?a=1&b="x"<y>"#;
        let expected = render(ARTICLE, &article, &plan, Some(manifest_url), usize::MAX)
            .expect("unbounded render should succeed");
        assert!(
            expected.contains("a=1&amp;b=&quot;x&quot;&lt;y&gt;"),
            "fixture should exercise every escaped manifest expansion"
        );

        assert_eq!(
            render(ARTICLE, &article, &plan, Some(manifest_url), expected.len(),)
                .expect("the exact output budget should fit"),
            expected
        );
        assert_eq!(
            render(
                ARTICLE,
                &article,
                &plan,
                Some(manifest_url),
                expected.len() - 1,
            ),
            Err(embed::ApplyError::TooLarge),
            "one byte below the exact output size must fail before rendering"
        );
    }

    #[test]
    fn render_orders_equal_head_offset_markers_before_manifest_meta() {
        let html = "<head>A</head>";
        let article = article_with_selector(html, Some("head"));
        let plan = EmbeddingPlan {
            index_unit: "codepoint".to_owned(),
            operations: vec![
                api::PlanOperation {
                    insert_after_index: 0,
                    marker: "\u{fe00}".to_owned(),
                },
                api::PlanOperation {
                    insert_after_index: 0,
                    marker: "\u{fe01}".to_owned(),
                },
            ],
        };

        assert_eq!(
            render(
                html,
                &article,
                &plan,
                Some(r#"https://example.com/?a=1&b="x""#),
                usize::MAX,
            )
            .expect("render should succeed"),
            concat!(
                "<head>A\u{fe00}\u{fe01}",
                r#"<meta name="c2pa-manifest-url" content="https://example.com/?a=1&amp;b=&quot;x&quot;">"#,
                "</head>"
            )
        );
    }

    #[test]
    fn render_preserves_manifest_placement_around_malformed_head_closes() {
        let plan = EmbeddingPlan {
            index_unit: "codepoint".to_owned(),
            operations: vec![api::PlanOperation {
                insert_after_index: 0,
                marker: "\u{fe00}".to_owned(),
            }],
        };
        let meta = r#"<meta name="c2pa-manifest-url" content="https://manifest.example.com/m">"#;
        for (html, expected) in [
            (
                "</head><article>A</article>",
                format!("{meta}</head><article>A\u{fe00}</article>"),
            ),
            (
                "<article>A</head>B</article>",
                format!("<article>A\u{fe00}{meta}</head>B</article>"),
            ),
        ] {
            let article = article_with_selector(html, None);
            assert_eq!(
                render(
                    html,
                    &article,
                    &plan,
                    Some("https://manifest.example.com/m"),
                    usize::MAX,
                )
                .expect("render should succeed"),
                expected
            );
        }
    }

    #[test]
    fn render_omits_manifest_for_an_empty_url_or_missing_head_close() {
        let plan = EmbeddingPlan {
            index_unit: "codepoint".to_owned(),
            operations: vec![api::PlanOperation {
                insert_after_index: 0,
                marker: "\u{fe00}".to_owned(),
            }],
        };
        for (html, selector, manifest_url, expected) in [
            (
                "<article>A</article>",
                None,
                Some("https://manifest.example.com/m"),
                "<article>A\u{fe00}</article>",
            ),
            (
                "<head>A</head>",
                Some("head"),
                Some(""),
                "<head>A\u{fe00}</head>",
            ),
        ] {
            let article = article_with_selector(html, selector);
            assert_eq!(
                render(html, &article, &plan, manifest_url, usize::MAX,)
                    .expect("render should succeed"),
                expected
            );
        }
    }

    #[test]
    fn a_cached_plan_signs_without_calling_the_api() {
        let kv = Arc::new(MemoryKv::default());
        let first = seed_plan(&kv);

        let http = Arc::new(StubHttpClient::new());
        let second = view(
            &factory(),
            &services(Arc::clone(&http) as Arc<dyn PlatformHttpClient>, &kv),
        );

        assert_eq!(
            second, first,
            "a cache hit should serve the same signed page"
        );
        assert!(
            http.recorded_backend_names().is_empty(),
            "should make no API call"
        );
    }

    #[test]
    fn a_cached_skip_passes_through_without_calling_the_api() {
        let kv = Arc::new(MemoryKv::default());
        let first_http = Arc::new(StubHttpClient::new());
        first_http.push_response(200, policy_response(POLICY_DIGEST));
        first_http.push_response(403, b"{}".to_vec());
        view(&factory(), &services(first_http, &kv));
        assert_eq!(kv.only_entry().1["kind"], "skip");

        let http = Arc::new(StubHttpClient::new());
        let output = view(
            &factory(),
            &services(Arc::clone(&http) as Arc<dyn PlatformHttpClient>, &kv),
        );

        assert_eq!(output, ARTICLE);
        assert!(
            http.recorded_backend_names().is_empty(),
            "should make no API call"
        );
    }

    #[test]
    fn a_delayed_skip_write_cannot_overwrite_a_concurrent_plan() {
        let seeded = Arc::new(MemoryKv::default());
        seed_plan(&seeded);
        let (plan_key, _) = seeded.only_entry();
        let policy_key = policy_key();
        let kv = Arc::new(MemoryKv {
            entries: Mutex::new(HashMap::from([(
                policy_key.clone(),
                seeded.entry_bytes(&policy_key),
            )])),
            concurrent_positive_before_skip_write: Mutex::new(Some((
                plan_key,
                seeded
                    .entries()
                    .into_iter()
                    .find(|(key, _)| key.starts_with(PLAN_KEY_PREFIX))
                    .expect("seeded plan")
                    .1,
            ))),
            ..MemoryKv::default()
        });
        let failed = Arc::new(StubHttpClient::new());
        failed.push_response(503, Vec::new());

        assert_eq!(
            view(
                &factory(),
                &services(Arc::clone(&failed) as Arc<dyn PlatformHttpClient>, &kv),
            ),
            ARTICLE,
            "the failed request should pass through"
        );
        assert_eq!(failed.recorded_request_methods(), ["POST"]);
        let entries = kv.entries();
        assert!(
            entries
                .values()
                .any(|value| serde_json::from_slice::<serde_json::Value>(value)
                    .is_ok_and(|entry| entry["kind"] == "plan")),
            "the concurrent plan must survive"
        );
        assert!(
            entries
                .values()
                .any(|value| serde_json::from_slice::<serde_json::Value>(value)
                    .is_ok_and(|entry| entry["kind"] == "skip")),
            "the delayed failure should remain negatively cached under its own key"
        );

        let no_api = Arc::new(StubHttpClient::new());
        assert_signed(&view(
            &factory(),
            &services(Arc::clone(&no_api) as Arc<dyn PlatformHttpClient>, &kv),
        ));
        assert!(no_api.recorded_request_methods().is_empty());
    }

    #[test]
    fn a_delayed_policy_skip_cannot_overwrite_a_concurrent_digest() {
        let seeded = Arc::new(MemoryKv::default());
        seed_plan(&seeded);
        let policy_key = policy_key();
        let kv = Arc::new(MemoryKv {
            concurrent_positive_before_skip_write: Mutex::new(Some((
                policy_key.clone(),
                seeded.entry_bytes(&policy_key),
            ))),
            ..MemoryKv::default()
        });
        let failed = Arc::new(StubHttpClient::new());
        failed.push_response(503, Vec::new());

        assert_eq!(
            view(
                &factory(),
                &services(Arc::clone(&failed) as Arc<dyn PlatformHttpClient>, &kv),
            ),
            ARTICLE,
        );
        assert_eq!(failed.recorded_request_methods(), ["GET"]);
        let entries = kv.entries();
        assert!(
            entries
                .values()
                .any(|value| serde_json::from_slice::<serde_json::Value>(value)
                    .is_ok_and(|entry| entry["kind"] == "digest")),
            "the concurrent policy digest must survive"
        );
        assert!(
            entries
                .values()
                .any(|value| serde_json::from_slice::<serde_json::Value>(value)
                    .is_ok_and(|entry| entry["kind"] == "skip")),
            "the delayed policy failure should remain under its own key"
        );

        let sign = Arc::new(StubHttpClient::new());
        sign.push_response(200, sign_response(MARKER));
        assert_signed(&view(
            &factory(),
            &services(Arc::clone(&sign) as Arc<dyn PlatformHttpClient>, &kv),
        ));
        assert_eq!(sign.recorded_request_methods(), ["POST"]);
    }

    #[test]
    fn sign_failures_pass_through_and_write_a_skip() {
        let oversized = {
            let mut body = sign_response(MARKER);
            body.resize(api::MAX_RESPONSE_BYTES + 2, b' ');
            body
        };
        let quota = serde_json::to_vec(&json!({ "success": false, "error": "quota_exceeded" }))
            .expect("should serialize");
        let insecure_manifest = serde_json::to_vec(&json!({
            "success": true,
            "policy_digest": POLICY_DIGEST,
            "embedding_plan": plan_json(MARKER),
            "manifest_url": "javascript:alert(1)",
        }))
        .expect("should serialize");
        let cases: Vec<FailureCase> = vec![
            ("403", Some((403, b"{}".to_vec()))),
            ("422", Some((422, b"{}".to_vec()))),
            ("429", Some((429, b"{}".to_vec()))),
            ("5xx", Some((502, Vec::new()))),
            ("transport error or first-byte timeout", None),
            ("oversized body", Some((200, oversized))),
            ("malformed body", Some((200, b"not json".to_vec()))),
            (
                "invalid marker",
                Some((200, sign_response("<script>alert(1)</script>"))),
            ),
            ("quota", Some((200, quota))),
            ("non-https manifest url", Some((200, insecure_manifest))),
            (
                "null plan",
                Some((
                    200,
                    serde_json::to_vec(&json!({
                        "success": true, "policy_digest": POLICY_DIGEST, "embedding_plan": null
                    }))
                    .expect("JSON"),
                )),
            ),
        ];

        for (case, response) in cases {
            let http = Arc::new(StubHttpClient::new());
            http.push_response(200, policy_response(POLICY_DIGEST));
            if let Some((status, body)) = response {
                http.push_response(status, body);
            }
            let kv = Arc::new(MemoryKv::default());

            let output = view(
                &factory(),
                &services(Arc::clone(&http) as Arc<dyn PlatformHttpClient>, &kv),
            );

            assert_eq!(
                output, ARTICLE,
                "{case}: should pass the page through unchanged"
            );
            assert_eq!(
                http.recorded_backend_names().len(),
                2,
                "{case}: one policy call and one sign call"
            );
            assert_eq!(
                kv.only_entry().1["kind"],
                "skip",
                "{case}: should cache a skip"
            );
        }
    }

    #[test]
    fn a_tampered_cached_plan_is_evicted_then_the_next_view_recovers() {
        for (field, value) in [
            ("/plan/operations/0/marker", "<img src=x onerror=alert(1)>"),
            ("/manifest_url", "http://manifest.example.com/doc-1"),
        ] {
            let kv = Arc::new(MemoryKv::default());
            seed_plan(&kv);
            let (key, mut entry) = kv.only_entry();
            *entry
                .pointer_mut(field)
                .expect("seeded entry should have the field") = json!(value);
            kv.insert(&key, &entry);

            let http = Arc::new(StubHttpClient::new());
            let services = services(Arc::clone(&http) as Arc<dyn PlatformHttpClient>, &kv);
            assert_eq!(
                view(&factory(), &services),
                ARTICLE,
                "{field}: should pass through"
            );
            assert!(
                http.recorded_backend_names().is_empty(),
                "{field}: should make no API call on the failure view"
            );
            assert!(
                kv.deletes.lock().expect("lock deletes").contains(&key),
                "{field}: the invalid plan should be evicted"
            );

            http.push_response(200, sign_response(MARKER));
            assert_signed(&view(&factory(), &services));
            assert_eq!(http.recorded_request_methods(), ["POST"], "{field}");
        }
    }

    #[test]
    fn malformed_ttl_less_plan_is_evicted_then_the_next_view_recovers() {
        let kv = Arc::new(MemoryKv {
            ttl_unsupported: true,
            ..MemoryKv::default()
        });
        seed_plan(&kv);
        let (key, _) = kv.only_entry();
        kv.insert_raw(&key, Bytes::from_static(b"not json"));
        let http = Arc::new(StubHttpClient::new());
        let services = services(Arc::clone(&http) as Arc<dyn PlatformHttpClient>, &kv);

        assert_eq!(view(&factory(), &services), ARTICLE);
        assert!(http.recorded_request_methods().is_empty());
        assert!(
            kv.deletes.lock().expect("lock deletes").contains(&key),
            "malformed entries must be deleted even when their ttl_less flag cannot be decoded"
        );

        http.push_response(200, sign_response(MARKER));
        assert_signed(&view(&factory(), &services));
        assert_eq!(http.recorded_request_methods(), ["POST"]);
    }

    #[test]
    fn representation_alignment_failure_does_not_evict_a_valid_cached_plan() {
        let kv = Arc::new(MemoryKv::default());
        seed_plan(&kv);
        let (key, _) = kv.only_entry();
        let signer = factory();
        let mut article = signer.extract(ARTICLE).expect("should extract article");
        article.boundary.end = ARTICLE.len() + 1;
        let http = Arc::new(StubHttpClient::new());
        let services = services(http, &kv);

        let result = futures::executor::block_on(async {
            let mut work = Deadline::Unbounded;
            signer
                .resolve(
                    &request(),
                    &services,
                    ARTICLE,
                    &article,
                    POLICY_DIGEST,
                    &mut work,
                )
                .await
        });

        assert_eq!(
            result.expect_err("alignment should fail"),
            Reason::AlignmentFailed
        );
        assert!(
            kv.entries().contains_key(&key),
            "valid plan should remain cached"
        );
        assert!(
            !kv.deletes.lock().expect("lock deletes").contains(&key),
            "representation-specific alignment must not evict a reusable plan"
        );
    }

    #[test]
    fn extraction_cap_counts_the_inter_fragment_separator() {
        let leading = "a".repeat(api::MAX_TEXT_SCALARS - 1);
        let html = format!(
            "<article><p>{leading}</p><p>{}</p></article>",
            "&amp;".repeat(100_000)
        );
        let article = factory().extract(&html).expect("should extract article");

        assert_eq!(article.scalars, api::MAX_TEXT_SCALARS);
        assert_eq!(article.text, format!("{leading} "));
    }

    #[test]
    fn underlength_article_maps_a_valid_api_range_plan_to_alignment_failed() {
        let html = "<article>A</article>";
        let article = article_with_selector(html, None);
        assert_eq!(
            html.get(article.boundary.start..article.boundary.end),
            Some(html)
        );
        let plan = EmbeddingPlan {
            index_unit: "codepoint".to_owned(),
            operations: vec![api::PlanOperation {
                insert_after_index: 1,
                marker: MARKER.to_owned(),
            }],
        };
        assert!(
            api::validate_plan(&plan, None, api::MAX_TEXT_SCALARS).is_ok(),
            "the plan index is valid within the signing API range"
        );

        let reason = render(html, &article, &plan, None, usize::MAX)
            .map_err(render_reason)
            .expect_err("the article has no canonical scalar at index 1");
        assert_eq!(reason, Reason::AlignmentFailed);
    }

    #[test]
    fn a_changed_credential_misses_the_cache() {
        let kv = Arc::new(MemoryKv::default());
        seed_plan(&kv);

        let http = signing_http();
        let output = view(
            &factory_with(config_json("cwk_rotated_token"), 5_000_000),
            &services(Arc::clone(&http) as Arc<dyn PlatformHttpClient>, &kv),
        );

        assert_signed(&output);
        assert_eq!(http.recorded_request_methods(), ["GET", "POST"]);
        assert_eq!(
            kv.entries().len(),
            4,
            "each credential should have its own policy and article entries"
        );
    }

    // --- asynchronous resolve and the hold deadline -------------------------

    #[test]
    fn calls_that_are_pending_before_ready_are_awaited_and_sign() {
        let timers: [Option<Arc<dyn PlatformTimer>>; 3] = [
            None,
            Some(Arc::new(StoppedTimer)),
            Some(Arc::new(TickTimer::default())),
        ];
        for timer in timers {
            let label = if timer.is_some() { "timer" } else { "no timer" };
            let kv = Arc::new(MemoryKv {
                pending_polls: 2,
                ..MemoryKv::default()
            });
            let stub = signing_http();
            let http = Arc::new(YieldingHttpClient {
                inner: Arc::clone(&stub),
                pending_polls: 2,
            });
            let mut services = services(http, &kv);
            if let Some(timer) = timer {
                services = services.with_timer(timer);
            }

            let first = view(&factory(), &services);
            let second = view(&factory(), &services);

            assert_signed(&first);
            assert_eq!(
                second, first,
                "{label}: the second view should hit the cache"
            );
            assert_eq!(stub.recorded_request_methods(), ["GET", "POST"], "{label}");
            assert_eq!(kv.only_entry().1["kind"], "plan", "{label}");
        }
    }

    #[test]
    fn a_hung_policy_call_times_out_writes_a_skip_and_the_next_view_makes_no_call() {
        let kv = Arc::new(MemoryKv::default());
        let hung = Arc::new(HungHttpClient::default());

        let output = view(
            &factory(),
            &timed_services(Arc::clone(&hung) as Arc<dyn PlatformHttpClient>, &kv),
        );

        assert_eq!(output, ARTICLE, "should release the page unchanged");
        assert_eq!(hung.sends.load(Ordering::SeqCst), 1);
        let skip = kv.entry(&policy_skip_key());
        assert_eq!(skip["kind"], "skip");
        assert_eq!(skip["reason"], "timeout");

        let http = Arc::new(StubHttpClient::new());
        let next = view(
            &factory(),
            &timed_services(Arc::clone(&http) as Arc<dyn PlatformHttpClient>, &kv),
        );
        assert_eq!(next, ARTICLE);
        assert!(
            http.recorded_backend_names().is_empty(),
            "the next view should be a cached skip"
        );
    }

    #[test]
    fn a_hung_sign_call_or_body_times_out_writes_a_plan_skip_and_the_next_view_makes_no_call() {
        let kv = Arc::new(MemoryKv::default());
        seed_plan(&kv);
        let article_key = kv.only_entry().0;
        kv.remove(&article_key);
        let hung = Arc::new(HungHttpClient::default());
        let services = timed_services(Arc::clone(&hung) as Arc<dyn PlatformHttpClient>, &kv);

        for _ in 0..2 {
            assert_eq!(view(&factory(), &services), ARTICLE);
        }

        assert_eq!(
            hung.sends.load(Ordering::SeqCst),
            1,
            "one sign call in total"
        );
        let (_, skip) = kv.only_entry();
        assert_eq!(skip["kind"], "skip");
        assert_eq!(skip["reason"], "timeout");
    }

    #[test]
    fn a_hung_kv_read_times_out_without_calling_the_api() {
        let kv = Arc::new(MemoryKv {
            hung_reads: true,
            ..MemoryKv::default()
        });
        let http = Arc::new(StubHttpClient::new());

        let output = view(
            &factory(),
            &timed_services(Arc::clone(&http) as Arc<dyn PlatformHttpClient>, &kv),
        );

        assert_eq!(output, ARTICLE);
        assert_eq!(kv.reads(), 1);
        assert!(http.recorded_backend_names().is_empty());
    }

    #[test]
    fn a_plan_write_that_runs_out_of_time_still_returns_the_signed_page() {
        let kv = Arc::new(MemoryKv {
            hung_writes_to: Some(PLAN_KEY_PREFIX),
            ..MemoryKv::default()
        });
        let http = signing_http();

        let output = view(
            &factory(),
            &timed_services(Arc::clone(&http) as Arc<dyn PlatformHttpClient>, &kv),
        );

        assert_signed(&output);
        assert_eq!(http.recorded_request_methods(), ["GET", "POST"]);
        assert!(
            kv.entries()
                .keys()
                .all(|key| !key.starts_with(PLAN_KEY_PREFIX)),
            "the abandoned plan write should store nothing"
        );
    }

    #[test]
    fn a_policy_digest_write_that_runs_out_of_time_caches_a_policy_skip() {
        let kv = Arc::new(MemoryKv {
            hung_first_write_to: Some(policy_key()),
            ..MemoryKv::default()
        });
        let http = Arc::new(StubHttpClient::new());
        http.push_response(200, policy_response(POLICY_DIGEST));

        let output = view(
            &factory(),
            &timed_services(Arc::clone(&http) as Arc<dyn PlatformHttpClient>, &kv),
        );

        assert_eq!(output, ARTICLE, "should release the page unchanged");
        assert_eq!(
            http.recorded_request_methods(),
            ["GET"],
            "no sign call after the digest write ran out of time"
        );
        let skip = kv.entry(&policy_skip_key());
        assert_eq!(skip["kind"], "skip");
        assert_eq!(skip["reason"], "timeout");
        assert!(
            kv.entries()
                .keys()
                .all(|key| !key.starts_with(PLAN_SKIP_KEY_PREFIX)),
            "no plan skip under a digest the next view cannot know"
        );

        // A hung client turns any API call into a full hold, so zero sends
        // after the positive and negative policy reads shows a cached skip.
        let reads = kv.reads();
        let hung = Arc::new(HungHttpClient::default());
        let next = view(
            &factory(),
            &timed_services(Arc::clone(&hung) as Arc<dyn PlatformHttpClient>, &kv),
        );
        assert_eq!(next, ARTICLE);
        assert_eq!(hung.sends.load(Ordering::SeqCst), 0, "no API call");
        assert_eq!(kv.reads() - reads, 2, "positive policy miss, then skip hit");
    }

    #[test]
    fn the_core_backstop_lies_beyond_the_module_hold_bound() {
        let kv = Arc::new(MemoryKv::default());
        let timer = Arc::new(TickTimer::default());
        let services = services(Arc::new(HungHttpClient::default()), &kv)
            .with_timer(Arc::clone(&timer) as Arc<dyn PlatformTimer>);

        assert_eq!(view(&factory(), &services), ARTICLE);

        let max_hold = Duration::from_millis(5_000);
        let requested = timer.requested.lock().expect("lock").clone();
        let (backstop, module): (Vec<_>, Vec<_>) =
            requested.into_iter().partition(|sleep| *sleep > max_hold);
        assert_eq!(backstop.len(), 1, "one core backstop beyond max_hold_ms");
        // The skip-write reserve starts only after the work deadline, so the
        // module's own sleeps run back to back.
        assert!(
            !module.is_empty() && module.iter().sum::<Duration>() <= max_hold,
            "module sleeps {module:?} should fit within max_hold_ms"
        );
        assert_eq!(
            kv.entry(&policy_skip_key())["reason"],
            "timeout",
            "the module deadline, not the backstop, should release the page"
        );
    }

    // --- expiry, write mode and key namespaces ------------------------------

    #[test]
    fn expired_entries_are_misses() {
        // (entry to expire, API calls the next view makes)
        let cases: [(&str, &[&str]); 4] = [
            ("policy digest", &["GET"]),
            ("policy skip", &["GET"]),
            ("plan", &["POST"]),
            ("plan skip", &["POST"]),
        ];
        for (case, expected_calls) in cases {
            let kv = Arc::new(MemoryKv::default());
            seed_plan(&kv);
            let (plan_key, _) = kv.only_entry();
            let key = if case.starts_with("policy") {
                policy_key()
            } else {
                plan_key
            };
            if case.ends_with("skip") {
                kv.insert(
                    &key,
                    &json!({"kind": "skip", "reason": "server_error", "expires_at": expired()}),
                );
            } else {
                set_expiry(&kv, &key, expired());
            }
            let http = Arc::new(StubHttpClient::new());
            if case.starts_with("policy") {
                http.push_response(200, policy_response(POLICY_DIGEST));
            } else {
                http.push_response(200, sign_response(MARKER));
            }

            let output = view(
                &factory(),
                &services(Arc::clone(&http) as Arc<dyn PlatformHttpClient>, &kv),
            );

            assert_signed(&output);
            assert_eq!(http.recorded_request_methods(), expected_calls, "{case}");
            assert!(
                kv.entry(&key)["expires_at"].as_u64() > Some(unix_now()),
                "{case}: should be replaced by a fresh entry"
            );
        }
    }

    #[test]
    fn an_expired_plan_does_not_block_a_skip_write() {
        let seeded = Arc::new(MemoryKv::default());
        seed_plan(&seeded);
        let kv = Arc::new(MemoryKv {
            entries: Mutex::new(seeded.entries()),
            hidden_reads: AtomicUsize::new(1),
            ..MemoryKv::default()
        });
        let (plan_key, _) = kv.only_entry();
        set_expiry(&kv, &plan_key, expired());
        let failed = Arc::new(StubHttpClient::new());
        failed.push_response(503, Vec::new());

        assert_eq!(
            view(
                &factory(),
                &services(Arc::clone(&failed) as Arc<dyn PlatformHttpClient>, &kv),
            ),
            ARTICLE
        );
        assert_eq!(failed.recorded_request_methods(), ["POST"]);

        let no_api = Arc::new(StubHttpClient::new());
        assert_eq!(
            view(
                &factory(),
                &services(Arc::clone(&no_api) as Arc<dyn PlatformHttpClient>, &kv),
            ),
            ARTICLE
        );
        assert!(
            no_api.recorded_request_methods().is_empty(),
            "the fresh skip should suppress a second signing call"
        );
    }

    #[test]
    fn a_store_without_ttl_writes_switches_once_and_expires_by_value() {
        let kv = Arc::new(MemoryKv {
            ttl_unsupported: true,
            ..MemoryKv::default()
        });
        let factory = factory();
        let http = signing_http();
        let services = services(Arc::clone(&http) as Arc<dyn PlatformHttpClient>, &kv);

        let first = view(&factory, &services);

        assert_signed(&first);
        assert_eq!(
            kv.ttl_writes.load(Ordering::SeqCst),
            1,
            "only the first write should try a TTL"
        );
        let (plan_key, plan) = kv.only_entry();
        assert_eq!(plan["kind"], "plan");
        assert!(plan["expires_at"].as_u64() > Some(unix_now()));
        assert_eq!(kv.entry(&policy_key())["kind"], "digest");
        assert_eq!(view(&factory, &services), first, "should hit the cache");
        assert_eq!(http.recorded_request_methods(), ["GET", "POST"]);

        // Expired entries are misses and are deleted when read.
        set_expiry(&kv, &plan_key, expired());
        http.push_response(200, sign_response(MARKER));
        assert_signed(&view(&factory, &services));
        assert_eq!(*kv.deletes.lock().expect("lock"), [plan_key.as_str()]);
        assert_eq!(
            kv.entry(&plan_key)["kind"],
            "plan",
            "should store a new plan"
        );
    }

    #[test]
    fn a_fresh_instance_deletes_expired_entries_written_without_a_ttl() {
        // Spin instantiates the module per request, so the instance that reads
        // an expired entry has not written anything yet.
        let kv = Arc::new(MemoryKv {
            ttl_unsupported: true,
            ..MemoryKv::default()
        });
        view(&factory(), &services(signing_http(), &kv));
        set_expiry(&kv, &policy_key(), expired());
        let http = Arc::new(StubHttpClient::new());
        http.push_response(503, Vec::new());

        let output = view(
            &factory(),
            &services(Arc::clone(&http) as Arc<dyn PlatformHttpClient>, &kv),
        );

        assert_eq!(output, ARTICLE);
        assert_eq!(http.recorded_request_methods(), ["GET"]);
        assert_eq!(*kv.deletes.lock().expect("lock"), [policy_key()]);
    }

    #[test]
    fn expired_entries_are_not_deleted_on_a_store_with_ttls() {
        let kv = Arc::new(MemoryKv::default());
        let http = signing_http();
        let services = services(Arc::clone(&http) as Arc<dyn PlatformHttpClient>, &kv);
        view(&factory(), &services);
        let (plan_key, _) = kv.only_entry();
        set_expiry(&kv, &plan_key, expired());
        http.push_response(200, sign_response(MARKER));

        assert_signed(&view(&factory(), &services));

        assert!(kv.deletes.lock().expect("lock").is_empty());
    }

    #[test]
    fn entries_under_the_previous_key_versions_are_never_read() {
        let current = Arc::new(MemoryKv::default());
        seed_plan(&current);
        // The same plan and policy, as the previous release stored them.
        let kv = Arc::new(MemoryKv::default());
        for (key, value) in current.entries() {
            let mut entry: serde_json::Value =
                serde_json::from_slice(&value).expect("should store JSON");
            entry.as_object_mut().expect("object").remove("expires_at");
            let old_key = key
                .replace("encypher:plan:v4:", "encypher:v3:")
                .replace("encypher:policy:v3:", "encypher:policy:v2:");
            kv.insert(&old_key, &entry);
        }
        let old_entries = kv.entries();
        let http = signing_http();

        let output = view(
            &factory(),
            &services(Arc::clone(&http) as Arc<dyn PlatformHttpClient>, &kv),
        );

        assert_signed(&output);
        assert_eq!(http.recorded_request_methods(), ["GET", "POST"]);
        let entries = kv.entries();
        assert_eq!(entries.len(), old_entries.len() * 2);
        for (key, value) in old_entries {
            assert_eq!(entries[&key], value, "{key} should be left untouched");
        }
    }

    // --- KV failures --------------------------------------------------------

    #[test]
    fn kv_read_errors_pass_through_without_calling_the_api() {
        for kv in [
            MemoryKv {
                fail_reads: true,
                ..MemoryKv::default()
            },
            MemoryKv {
                unavailable: true,
                ..MemoryKv::default()
            },
        ] {
            let kv = Arc::new(kv);
            let http = Arc::new(StubHttpClient::new());

            let output = view(
                &factory(),
                &services(Arc::clone(&http) as Arc<dyn PlatformHttpClient>, &kv),
            );

            assert_eq!(output, ARTICLE);
            assert_eq!(kv.reads(), 1);
            assert!(
                http.recorded_backend_names().is_empty(),
                "should make no API call"
            );
        }
    }

    // --- gates and skips ----------------------------------------------------

    #[test]
    fn gated_documents_pass_through_with_no_kv_or_api_io() {
        let signer = factory();
        let cases: Vec<(GateCase<'_>, Gate)> = vec![
            (
                ("method", &signer, |r| r.method = Method::POST),
                Gate::Method,
            ),
            (
                ("status", &signer, |r| r.response_status = 404),
                Gate::Status,
            ),
            (
                ("origin private", &signer, |r| {
                    r.origin_response_is_private = true;
                }),
                Gate::OriginPrivate,
            ),
            (
                ("host mismatch", &signer, |r| {
                    r.page_url = "https://other.example.com/news/harbor-vote".to_owned();
                }),
                Gate::HostMismatch,
            ),
            (
                ("excluded path", &signer, |r| {
                    r.page_url = "https://publisher.example.com/WP-Admin/post".to_owned();
                }),
                Gate::ExcludedPath,
            ),
            (
                ("excluded path under an addressing prefix", &signer, |r| {
                    r.page_url =
                        "https://publisher.example.com/www.site.example/wp-admin/post".to_owned();
                    r.removed_prefix = "/www.site.example".to_owned();
                }),
                Gate::ExcludedPath,
            ),
            (
                ("asset suffix", &signer, |r| {
                    r.page_url = "https://publisher.example.com/feed.XML".to_owned();
                }),
                Gate::ExcludedPath,
            ),
            (
                ("invalid page url", &signer, |r| {
                    r.page_url = "not a url".to_owned();
                }),
                Gate::InvalidPageUrl,
            ),
        ];

        for ((case, signer, mutate), gate) in cases {
            let http = Arc::new(StubHttpClient::new());
            let kv = Arc::new(MemoryKv::default());
            let services = services(Arc::clone(&http) as Arc<dyn PlatformHttpClient>, &kv);
            let mut request = request();
            mutate(&mut request);

            assert_eq!(
                signer.admit(request.clone()),
                Err(gate),
                "{case}: should be refused by its gate"
            );
            let output = run(signer, Some(request), &services, ARTICLE);

            assert_eq!(output, ARTICLE, "{case}: should pass through");
            assert_eq!(kv.reads(), 0, "{case}: should read no KV");
            assert!(
                http.recorded_backend_names().is_empty(),
                "{case}: no API call"
            );
        }

        let kv = Arc::new(MemoryKv::default());
        assert_eq!(
            run(
                &factory(),
                None,
                &services(Arc::new(StubHttpClient::new()), &kv),
                ARTICLE
            ),
            ARTICLE,
            "nothing prepared"
        );
    }

    #[test]
    fn a_bare_domain_page_is_signed() {
        let kv = Arc::new(MemoryKv::default());
        let mut request = request();
        request.page_url = "https://publisher.example.com/news/harbor-vote".to_owned();

        let output = run(
            &factory(),
            Some(request),
            &services(signing_http(), &kv),
            ARTICLE,
        );

        assert_signed(&output);
    }

    #[test]
    fn an_oversized_document_is_flushed_and_passed_through() {
        let http = Arc::new(StubHttpClient::new());
        let kv = Arc::new(MemoryKv::default());

        let output = view(
            &factory_with(config_json(TOKEN), 64),
            &services(Arc::clone(&http) as Arc<dyn PlatformHttpClient>, &kv),
        );

        assert_eq!(output, ARTICLE);
        assert_eq!(kv.reads(), 0);
        assert!(http.recorded_backend_names().is_empty());
    }

    #[test]
    fn already_signed_or_short_pages_are_not_sent() {
        let magic = "\u{E01DF}\u{E01E0}\u{E01E1}\u{E01E2}";
        let marked = ARTICLE.replace("The harbor council", &format!("The harbor{magic} council"));
        let with_meta = ARTICLE.replace(
            "</head>",
            r#"<meta name="c2pa-manifest-url" content="https://example.com/m"></head>"#,
        );
        let short = "<html><head></head><body><article><p>Too short.</p></article></body></html>";

        for page in [marked.as_str(), with_meta.as_str(), short] {
            let http = Arc::new(StubHttpClient::new());
            let kv = Arc::new(MemoryKv::default());

            let output = run(
                &factory(),
                Some(request()),
                &services(Arc::clone(&http) as Arc<dyn PlatformHttpClient>, &kv),
                page,
            );

            assert_eq!(output, page);
            assert_eq!(kv.reads(), 0, "should read no KV");
            assert!(
                http.recorded_backend_names().is_empty(),
                "should make no API call"
            );
        }
    }

    #[test]
    fn logs_carry_outcomes_but_never_the_token() {
        let kv = Arc::new(MemoryKv::default());
        let Some(logs) = capture_logs(|| {
            view(&factory(), &services(signing_http(), &kv));
            view(&factory(), &services(Arc::new(StubHttpClient::new()), &kv));
            let failing = Arc::new(StubHttpClient::new());
            failing.push_response(403, b"{}".to_vec());
            view(
                &factory(),
                &services(failing, &Arc::new(MemoryKv::default())),
            );
        }) else {
            // Log lines go to a logger installed elsewhere and cannot be observed here.
            return;
        };

        assert!(
            logs.iter()
                .any(|line| line.ends_with("outcome=signed reason=signed")),
            "should log the signing outcome: {logs:?}"
        );
        for line in &logs {
            assert!(!line.contains(TOKEN), "log line leaks the token: {line}");
            assert!(
                !line.contains("encypher:plan:v4:") && !line.contains("encypher:policy:v3:"),
                "log line leaks the cache key: {line}"
            );
            assert!(
                !line.contains("ferry"),
                "log line leaks article text: {line}"
            );
        }
    }
}
