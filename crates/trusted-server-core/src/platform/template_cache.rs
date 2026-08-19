//! The shared transformed-template cache for the #1009 ESI validation spike.
//!
//! Three caches are in play and conflating them is what produced the original wrong
//! conclusion in the design doc, so this module names which one it is:
//!
//! | Cache | Contents                          | Owner                          |
//! | ----- | --------------------------------- | ------------------------------ |
//! | C1    | raw origin bytes                  | Fastly read-through. Not this. |
//! | Template cache | post-`lol_html`, pre-assembly     | **This module.**       |
//! | Final response | final per-user assembled response | **Must never exist.** |
//!
//! The template cache holds a *shared template*: no per-user bytes, and no decisions that depend on
//! the request. What may and may not live in it is
//! [§6.7 of the design doc](../../../../docs/superpowers/archive/2026-08-08-esi-cacheable-root-validation-design.md),
//! and the invariant is enforced by the rendered-document byte-identity tests in
//! `publisher`.
//!
//! Spike-only. Remove with the spike.

use core::fmt;
use std::collections::HashSet;

use crate::creative_opportunities::AssemblyMode;

/// Version of the transform that produced a cached template.
///
/// Bump on **any** change to what the transform emits. Without it a deploy reads
/// yesterday's template shape and assembles against markers that moved, which fails
/// as a rendering bug far from its cause rather than as a cache miss.
///
/// | Version | Transform |
/// | ------- | --------- |
/// | 1       | `</body>` seam used an executable ESI include tag targeting the old fragment endpoint |
/// | 2       | Marker became the inert comment `<!--ts-seam-bids-->`; the seam hands slots to `scheduleInitialAdInit` instead of assigning them |
/// | 3       | Marker became `<!--ts-c2-v3-seam-7f4c9e2d-bids-->`; canonical collision-safe key, explicit origin freshness, and complete repeated document-policy metadata |
/// | 4       | Marker is the shorter, accurate [`AD_ASSEMBLY_SEAM`](crate::publisher::AD_ASSEMBLY_SEAM) |
pub const TEMPLATE_SCHEMA_VERSION: u32 = 4;

/// Surrogate key attached to every template so an incident can purge the template cache globally.
pub const TEMPLATE_CACHE_PURGE_ALL_SURROGATE_KEY: &str = "ts-template";

/// Inputs that select one cached template.
///
/// Every field changes the emitted bytes for the same URL. A signal that changes the
/// bytes and is **not** here produces cross-served templates; a signal that is
/// per-user does not belong here at all — it belongs out of the template entirely.
/// That distinction is the whole design: the key holds per-*variant* signals, and
/// per-*user* signals are excluded from the template rather than keyed on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateCacheKey {
    /// Full request URL, stated explicitly rather than inherited from an ambient
    /// request, so the key cannot silently depend on what the caller happened to
    /// mutate first.
    pub url: String,
    /// Host and scheme. The post-processed output is host-dependent by construction:
    /// both reach `IntegrationHtmlContext` and drive URL rewriting.
    pub request_host: String,
    /// See [`Self::request_host`].
    pub request_scheme: String,
    /// Publisher origin identity, including the outbound Host override. Two virtual
    /// hosts can share a connection target while producing unrelated documents.
    pub origin_identity: String,
    /// Inline and ESI modes emit different template bytes. Without this they poison
    /// each other's entries.
    pub assembly_mode: AssemblyMode,
    /// Values of the request headers the **origin** declares it varies on, in the
    /// order the origin listed them. Not a fixed list: the origin is authoritative,
    /// and hard-coding one here would silently drift when the origin's changes.
    pub vary_values: Vec<VaryHeaderValues>,
    /// Digest of every setting that can shape the transformed template plus the tsjs
    /// bundle. Over-invalidating is safe; omitting a shaping input cross-serves bytes.
    pub template_fingerprint: String,
    /// See [`TEMPLATE_SCHEMA_VERSION`].
    pub schema_version: u32,
}

impl TemplateCacheKey {
    /// Render a fixed-size opaque key for the platform cache.
    ///
    /// The canonical input is length-prefixed before hashing, so neither delimiters nor
    /// raw request values can collide or leak into cache diagnostics.
    #[must_use]
    pub fn to_cache_key(&self) -> String {
        use sha2::Digest as _;

        fn push(out: &mut Vec<u8>, part: &[u8]) {
            out.extend_from_slice(&(part.len() as u64).to_be_bytes());
            out.extend_from_slice(part);
        }

        let mut canonical = Vec::new();
        push(&mut canonical, b"ts-template-cache");
        push(&mut canonical, &self.schema_version.to_be_bytes());
        push(
            &mut canonical,
            match self.assembly_mode {
                AssemblyMode::Inline => b"inline",
                AssemblyMode::Esi => b"esi",
            },
        );
        push(&mut canonical, self.request_scheme.as_bytes());
        push(&mut canonical, self.request_host.as_bytes());
        push(&mut canonical, self.origin_identity.as_bytes());
        push(&mut canonical, self.url.as_bytes());
        push(&mut canonical, self.template_fingerprint.as_bytes());
        push(
            &mut canonical,
            &(self.vary_values.len() as u64).to_be_bytes(),
        );
        for varied in &self.vary_values {
            push(&mut canonical, varied.name.to_ascii_lowercase().as_bytes());
            match &varied.values {
                None => push(&mut canonical, b"absent"),
                Some(values) => {
                    push(&mut canonical, b"present");
                    push(&mut canonical, &(values.len() as u64).to_be_bytes());
                    for value in values {
                        push(&mut canonical, value);
                    }
                }
            }
        }

        let digest = sha2::Sha256::digest(canonical);
        format!(
            "ts-template-cache-v{}-{}",
            self.schema_version,
            hex::encode(digest)
        )
    }

    /// Surrogate keys to attach at insert, for purge-based rollback.
    ///
    /// `ts-template` purges every template at once, which is the rollback lever.
    /// The per-URL key allows targeted invalidation. Both are needed: the broad one
    /// for an incident, the narrow one for ordinary invalidation.
    #[must_use]
    pub fn surrogate_keys(&self) -> Vec<String> {
        vec![
            TEMPLATE_CACHE_PURGE_ALL_SURROGATE_KEY.to_string(),
            self.url_surrogate_key(),
        ]
    }

    /// Surrogate key for every variant of this publisher URL.
    ///
    /// Used to evict a malformed object without flushing unrelated article templates.
    #[must_use]
    pub fn url_surrogate_key(&self) -> String {
        format!("ts-template-url-{}", digest_hex(self.url.as_bytes()))
    }
}

fn digest_hex(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    hex::encode(sha2::Sha256::digest(bytes))
}

/// One configured `Vary` input exactly as it appeared on the request.
///
/// `None` means absent. `Some(vec![vec![]])` means present with one empty field
/// value. Repeated fields stay separate and ordered; no UTF-8 conversion is involved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaryHeaderValues {
    /// Validated, lowercase header name.
    pub name: String,
    /// Every raw field value in wire order, or `None` when absent.
    pub values: Option<Vec<Vec<u8>>>,
}

/// Origin response headers safe to store with a shared template and replay on a hit.
///
/// Every one is a per-URL policy statement, identical for every reader. Nothing
/// per-reader (`Set-Cookie`) and nothing cache-controlling (`Cache-Control`, `ETag`,
/// `Surrogate-Control`) appears here, and it is an allowlist so a new origin header is
/// excluded until someone decides otherwise.
pub const REPLAYABLE_POLICY_HEADERS: &[&str] = &[
    "content-security-policy",
    "content-security-policy-report-only",
    "permissions-policy",
    "referrer-policy",
    "strict-transport-security",
    "cross-origin-opener-policy",
    "cross-origin-embedder-policy",
    "cross-origin-resource-policy",
    "origin-agent-cluster",
    "reporting-endpoints",
    "report-to",
    "link",
    "x-frame-options",
    "x-content-type-options",
    "content-language",
    "x-robots-tag",
];

/// Headers the key covers by construction, whatever the operator configured.
///
/// The shared path stores decoded identity bytes and negotiates the reader representation
/// only after assembly, so an origin declaring `Vary: Accept-Encoding` is covered without
/// reader input. This assumes those origin variants differ only by HTTP content coding;
/// operators must leave ESI disabled if an origin changes document semantics instead.
/// Without this carve-out, the ordinary declaration sent by any compressing origin reads
/// as an uncovered gap and disqualifies the response, so **the template cache would never store anything
/// against a real origin** unless the operator redundantly listed a header the transform
/// already normalizes. Found by review before it could make the spike measure a hit rate
/// of approximately zero and read that as a result.
const STRUCTURALLY_COVERED: &[&str] = &["accept-encoding"];

/// Request headers to include in the cache key, and where the list comes from.
///
/// # The chicken-and-egg this resolves
///
/// The key must cover everything the origin varies on, or two requests needing
/// different templates share one entry. But a **lookup happens before the fetch**,
/// so on a cold key the origin's `Vary` is not yet known.
///
/// Three ways out, and the trade-off is real:
///
/// 1. **Configure the list** — what this does. One lookup, no extra round trip, and
///    the operator states what the origin varies on. Cost: it drifts silently if the
///    origin's `Vary` changes and nobody updates config.
/// 2. **Two-phase lookup** — fetch a URL-keyed record holding the last-seen `Vary`,
///    then key properly. Correct, but doubles the lookups on every request.
/// 3. **Store the list alongside** and re-key on mismatch. Same cost as (2) plus
///    complexity.
///
/// (1) is chosen for the spike because Step A already measured the origin's actual
/// `Vary`, the origin response is checked for drift before storage, and the configured
/// template-cache ceiling bounds how long a newly introduced mismatch can survive.
/// **This is a spike-grade choice, not a production one** — see the drift guard below.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VarySpec {
    /// Header names, lowercased, in a fixed order.
    names: Vec<String>,
}

impl VarySpec {
    /// Build from configured header names.
    ///
    /// # Panics
    ///
    /// Panics when a name is not a valid HTTP field name. Runtime configuration is
    /// validated with [`Self::try_new`] before this constructor is used.
    #[must_use]
    pub fn new(names: impl IntoIterator<Item = String>) -> Self {
        Self::try_new(names).expect("VarySpec names should be validated at configuration load")
    }

    /// Build from configured names, validating and deduplicating them.
    ///
    /// # Errors
    ///
    /// Returns the offending name when it is not a valid HTTP field name.
    pub fn try_new(names: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let mut seen = HashSet::new();
        let mut normalized = Vec::new();
        for raw in names {
            let name = http::header::HeaderName::from_bytes(raw.as_bytes())
                .map_err(|_| raw.clone())?
                .as_str()
                .to_string();
            if STRUCTURALLY_COVERED.contains(&name.as_str()) {
                continue;
            }
            if seen.insert(name.clone()) {
                normalized.push(name);
            }
        }
        Ok(Self { names: normalized })
    }

    /// Configured names, lowercased.
    #[must_use]
    pub fn names(&self) -> &[String] {
        &self.names
    }

    /// Extract the key inputs from a request's headers.
    ///
    /// A header the origin varies on but the request omits still contributes an
    /// entry, with an empty value — otherwise "absent" and "present but empty"
    /// would collide, and those are different requests to the origin.
    #[must_use]
    pub fn values_from(&self, headers: &http::HeaderMap) -> Vec<VaryHeaderValues> {
        self.names
            .iter()
            .map(|name| {
                let values = headers.contains_key(name.as_str()).then(|| {
                    headers
                        .get_all(name.as_str())
                        .iter()
                        .map(|value| value.as_bytes().to_vec())
                        .collect()
                });
                VaryHeaderValues {
                    name: name.clone(),
                    values,
                }
            })
            .collect()
    }

    /// Whether the origin's declared `Vary` contains anything this spec omits.
    ///
    /// The drift guard for choice (1) above. Called **after** the origin responds,
    /// when its `Vary` is finally known: if the origin varies on something the key
    /// did not cover, the template just built is unsafe to store, because a request
    /// differing only in that header would read it.
    ///
    /// Returns the uncovered names, so the caller can log precisely which config is
    /// stale rather than reporting a generic refusal.
    #[must_use]
    pub fn uncovered_by<'a>(&self, origin_vary: impl IntoIterator<Item = &'a str>) -> Vec<String> {
        origin_vary
            .into_iter()
            .flat_map(|value| value.split(','))
            .map(|name| name.trim().to_ascii_lowercase())
            .filter(|name| !name.is_empty() && name != "*")
            .filter(|name| !STRUCTURALLY_COVERED.contains(&name.as_str()))
            .filter(|name| !self.names.contains(name))
            .collect()
    }
}

/// Metadata stored alongside the template bytes.
///
/// `cache::core` carries **no HTTP semantics** — status, headers, encoding and
/// revalidation are all the caller's. Rather than storing origin headers and
/// replaying them, store only what is needed to rebuild a response from scratch.
///
/// That choice is deliberate and load-bearing: the publisher path forces
/// `private, no-store` and strips validators *after* the origin send, so replaying a
/// stored origin header would fight it. Rebuilding every header on a hit means no
/// origin header is ever replayed and the `Set-Cookie` privacy net stays trivially
/// safe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateMetadata {
    /// Encoding of the stored bytes. The template cache writes only `identity`; retaining the field in
    /// metadata makes corrupt or stale representations fail validation on read.
    pub content_encoding: String,
    /// Content type to rebuild the response with.
    pub content_type: String,
    /// Schema version the bytes were produced under. Checked on read: a mismatch is
    /// a miss, not an error, so a rollback to an older binary degrades to
    /// re-transforming rather than misassembling.
    pub schema_version: u32,
    /// Length of the template bytes as written.
    ///
    /// Guards against a partially written entry. `Transaction::insert` consumes the
    /// transaction, so a write that fails part-way cannot cancel the insert — there
    /// is no handle left to cancel it with. Recording the intended length and
    /// checking it on read makes a truncated entry a miss instead of a silently
    /// short template that would assemble into a broken page.
    pub body_len: u64,
    /// Origin response headers that are policy, not per-reader state.
    ///
    /// Reconstructing headers from scratch on a hit keeps origin `Set-Cookie` and caching
    /// directives out of a shared cache — but it also dropped `Content-Security-Policy`,
    /// framing protection and `Content-Language`, weakening the page. These are
    /// per-URL and identical for every reader, so they belong with the template.
    ///
    /// Deliberately an allowlist: anything per-reader or cache-controlling is excluded by
    /// construction rather than by remembering to strip it.
    pub policy_headers: Vec<(String, String)>,
}

/// Why public template metadata could not be represented safely.
#[derive(Debug, Clone, Copy, PartialEq, Eq, derive_more::Display)]
#[display("template metadata field `{field}` contains a line break")]
pub struct TemplateMetadataEncodeError {
    field: &'static str,
}

impl core::error::Error for TemplateMetadataEncodeError {}

impl TemplateMetadata {
    /// Serialize for `user_metadata`. Deliberately a tiny hand-rolled format rather
    /// than JSON — one allocation, no dependency, and a parse failure is
    /// unambiguous.
    ///
    /// # Errors
    ///
    /// Returns an error when any public string field contains CR or LF, which would
    /// otherwise inject another record into the newline-delimited representation.
    pub fn encode(&self) -> Result<Vec<u8>, TemplateMetadataEncodeError> {
        fn reject_line_breaks(
            field: &'static str,
            value: &str,
        ) -> Result<(), TemplateMetadataEncodeError> {
            if value.contains(['\r', '\n']) {
                return Err(TemplateMetadataEncodeError { field });
            }
            Ok(())
        }

        reject_line_breaks("content_encoding", &self.content_encoding)?;
        reject_line_breaks("content_type", &self.content_type)?;
        for (name, value) in &self.policy_headers {
            reject_line_breaks("policy_header_name", name)?;
            reject_line_breaks("policy_header_value", value)?;
        }

        let mut out = format!(
            "v={}\nce={}\nct={}\nlen={}",
            self.schema_version, self.content_encoding, self.content_type, self.body_len
        );
        for (name, value) in &self.policy_headers {
            // Line breaks were rejected above before constructing the delimited form.
            out.push_str(&format!("\nh={name}:{value}"));
        }
        Ok(out.into_bytes())
    }

    /// Parse `user_metadata`. Returns `None` on anything unexpected, which callers
    /// must treat as a cache miss.
    #[must_use]
    pub fn decode(raw: &[u8]) -> Option<Self> {
        let text = core::str::from_utf8(raw).ok()?;
        let mut schema_version = None;
        let mut policy_headers = Vec::new();
        let mut content_encoding = None;
        let mut content_type = None;
        let mut body_len = None;
        for line in text.lines() {
            let (key, value) = line.split_once('=')?;
            match key {
                "v" => {
                    if schema_version.replace(value.parse().ok()?).is_some() {
                        return None;
                    }
                }
                "ce" => {
                    if content_encoding.replace(value.to_string()).is_some() {
                        return None;
                    }
                }
                "h" => {
                    let (name, header_value) = value.split_once(':')?;
                    let name = http::header::HeaderName::from_bytes(name.as_bytes()).ok()?;
                    if !REPLAYABLE_POLICY_HEADERS.contains(&name.as_str()) {
                        return None;
                    }
                    http::HeaderValue::from_bytes(header_value.as_bytes()).ok()?;
                    policy_headers.push((name.as_str().to_string(), header_value.to_string()));
                }
                "ct" => {
                    if content_type.replace(value.to_string()).is_some() {
                        return None;
                    }
                }
                "len" => {
                    if body_len.replace(value.parse().ok()?).is_some() {
                        return None;
                    }
                }
                _ => return None,
            }
        }
        let content_encoding = content_encoding?;
        // Every template is decoded before insert. Accepting another value here would
        // let corrupt metadata label plaintext bytes as gzip on a warm hit.
        if content_encoding != "identity" {
            return None;
        }
        let content_type = content_type?;
        http::HeaderValue::from_bytes(content_type.as_bytes()).ok()?;
        if !content_type
            .split(';')
            .next()
            .is_some_and(|media_type| media_type.trim().eq_ignore_ascii_case("text/html"))
        {
            return None;
        }
        Some(Self {
            schema_version: schema_version?,
            policy_headers,
            content_encoding,
            content_type,
            body_len: body_len?,
        })
    }
}

/// Why a template read did not produce usable bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, derive_more::Display)]
pub enum TemplateCacheMiss {
    /// No entry for this key.
    #[display("no cached template for this key")]
    NotFound,
    /// Found, but produced by a different transform version.
    #[display("cached template has a different schema version")]
    SchemaMismatch,
    /// Found, but its metadata could not be parsed.
    #[display("cached template metadata is unreadable")]
    UnreadableMetadata,
    /// Found, but shorter than the metadata says it should be — a write that failed
    /// part-way. See [`TemplateMetadata::body_len`].
    #[display("cached template is truncated")]
    Truncated,
    /// This platform has no template cache.
    #[display("no template cache on this platform")]
    Unsupported,
}

impl core::error::Error for TemplateCacheMiss {}

/// Errors a template cache write can produce.
#[derive(Debug, derive_more::Display)]
pub enum TemplateCacheError {
    /// This platform has no template cache.
    #[display("no template cache on this platform")]
    Unsupported,
    /// The platform rejected the operation.
    #[display("template cache backend error: {message}")]
    Backend {
        /// What the backend reported.
        message: String,
    },
}

impl core::error::Error for TemplateCacheError {}

/// Result of the pre-origin cache transaction.
pub enum TemplateCacheLookup {
    /// A fresh usable template.
    Hit(TemplateEntry),
    /// This request owns the obligation to provide or cancel the cold object.
    Reserved(TemplateCacheReservation),
    /// This adapter deliberately has no shared-template cache.
    Unsupported,
    /// A cache object existed but failed schema, metadata, or length validation.
    Invalid(TemplateCacheMiss),
}

/// Platform-owned insert obligation. Dropping it cancels, making every early-return
/// path safe without an async cleanup ladder in the publisher pipeline.
pub struct TemplateCacheReservation {
    inner: Option<Box<dyn PlatformTemplateCacheReservation>>,
}

impl core::fmt::Debug for TemplateCacheReservation {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("TemplateCacheReservation")
            .finish_non_exhaustive()
    }
}

impl TemplateCacheReservation {
    /// Wrap a platform reservation.
    #[must_use]
    pub fn new(inner: Box<dyn PlatformTemplateCacheReservation>) -> Self {
        Self { inner: Some(inner) }
    }

    /// Fulfil the reservation with a validated template.
    ///
    /// # Errors
    ///
    /// Returns the platform cache error when the reservation cannot be fulfilled.
    pub fn insert(
        mut self,
        metadata: &TemplateMetadata,
        body: Vec<u8>,
        max_age: std::time::Duration,
    ) -> Result<(), TemplateCacheError> {
        self.inner
            .take()
            .ok_or_else(|| TemplateCacheError::Backend {
                message: "template reservation was already consumed".to_string(),
            })?
            .insert(metadata, body, max_age)
    }

    /// Explicitly give up the reservation. Drop performs the same operation as a net.
    ///
    /// # Errors
    ///
    /// Returns the platform cache error when the reservation cannot be cancelled.
    pub fn cancel(mut self) -> Result<(), TemplateCacheError> {
        self.inner
            .take()
            .ok_or_else(|| TemplateCacheError::Backend {
                message: "template reservation was already consumed".to_string(),
            })?
            .cancel()
    }
}

impl Drop for TemplateCacheReservation {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.take()
            && let Err(err) = inner.cancel()
        {
            log::warn!("template_cache reservation cancellation failed: {err}");
        }
    }
}

/// Adapter-specific ownership token returned by a transactional lookup.
pub trait PlatformTemplateCacheReservation: Send {
    /// Insert and discharge the obligation.
    ///
    /// # Errors
    ///
    /// Returns an adapter-specific cache error when the insert fails.
    fn insert(
        self: Box<Self>,
        metadata: &TemplateMetadata,
        body: Vec<u8>,
        max_age: std::time::Duration,
    ) -> Result<(), TemplateCacheError>;

    /// Cancel and allow a waiting request to take ownership.
    ///
    /// # Errors
    ///
    /// Returns an adapter-specific cache error when cancellation fails.
    fn cancel(self: Box<Self>) -> Result<(), TemplateCacheError>;
}

impl fmt::Debug for dyn PlatformTemplateCache {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PlatformTemplateCache")
    }
}

/// A platform's shared-template cache.
///
/// Only the Fastly adapter implements this; every other adapter uses
/// [`UnavailableTemplateCache`], which reports [`TemplateCacheMiss::Unsupported`] so
/// the caller transforms every time rather than failing.
///
/// `Send + Sync` on the trait, `?Send` on the futures: `RuntimeServices` is held in a
/// `LazyLock` static, so the trait object must cross threads even though the futures
/// themselves never do — the platform layer is `!Send` by construction.
#[async_trait::async_trait(?Send)]
pub trait PlatformTemplateCache: Send + Sync {
    /// Transactionally look up a template before origin work begins.
    ///
    /// This compatibility default exists for implementations with no transactional
    /// reservation support. It reports ordinary cold misses as `Unsupported`; an
    /// adapter that supports template-cache reservations must override it so cold requests can
    /// return [`TemplateCacheLookup::Reserved`].
    async fn lookup_or_reserve(
        &self,
        key: &TemplateCacheKey,
    ) -> Result<TemplateCacheLookup, TemplateCacheError> {
        Ok(match self.get(key).await {
            Ok(entry) => TemplateCacheLookup::Hit(entry),
            Err(TemplateCacheMiss::Unsupported | TemplateCacheMiss::NotFound) => {
                TemplateCacheLookup::Unsupported
            }
            Err(miss) => TemplateCacheLookup::Invalid(miss),
        })
    }

    /// Read a template. `Err` is a miss, not a failure — every variant means
    /// "transform it yourself".
    async fn get(&self, key: &TemplateCacheKey) -> Result<TemplateEntry, TemplateCacheMiss>;

    /// Store a template.
    ///
    /// Callers must not call this without having consulted the template-cache eligibility gate
    /// first: this method stores what it is given and cannot tell a shared template
    /// from a per-user one.
    async fn put(
        &self,
        key: &TemplateCacheKey,
        metadata: &TemplateMetadata,
        body: Vec<u8>,
        max_age: std::time::Duration,
    ) -> Result<(), TemplateCacheError>;

    /// Purge every cached variant for one publisher URL.
    async fn purge_url(&self, key: &TemplateCacheKey) -> Result<(), TemplateCacheError>;

    /// Purge every stored template. The rollback lever.
    async fn purge_all(&self) -> Result<(), TemplateCacheError>;
}

/// A template read from the cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateEntry {
    /// Metadata stored at insert.
    pub metadata: TemplateMetadata,
    /// The transformed template bytes.
    pub body: Vec<u8>,
}

/// The null object, used by every adapter without a template cache.
///
/// Reporting [`TemplateCacheMiss::Unsupported`] rather than erroring means the
/// ESI assembly mode degrades to transforming per request on Cloudflare, Axum and Spin
/// instead of failing — the mode stays portable, only the caching is not.
pub struct UnavailableTemplateCache;

#[async_trait::async_trait(?Send)]
impl PlatformTemplateCache for UnavailableTemplateCache {
    async fn lookup_or_reserve(
        &self,
        _key: &TemplateCacheKey,
    ) -> Result<TemplateCacheLookup, TemplateCacheError> {
        Ok(TemplateCacheLookup::Unsupported)
    }

    async fn get(&self, _key: &TemplateCacheKey) -> Result<TemplateEntry, TemplateCacheMiss> {
        Err(TemplateCacheMiss::Unsupported)
    }

    async fn put(
        &self,
        _key: &TemplateCacheKey,
        _metadata: &TemplateMetadata,
        _body: Vec<u8>,
        _max_age: std::time::Duration,
    ) -> Result<(), TemplateCacheError> {
        Err(TemplateCacheError::Unsupported)
    }

    async fn purge_url(&self, _key: &TemplateCacheKey) -> Result<(), TemplateCacheError> {
        Err(TemplateCacheError::Unsupported)
    }

    async fn purge_all(&self) -> Result<(), TemplateCacheError> {
        Err(TemplateCacheError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn key() -> TemplateCacheKey {
        TemplateCacheKey {
            url: "https://example.com/news/article".to_string(),
            request_host: "example.com".to_string(),
            request_scheme: "https".to_string(),
            origin_identity: "https://origin.example.com\0origin.example.com".to_string(),
            assembly_mode: AssemblyMode::Esi,
            vary_values: vec![VaryHeaderValues {
                name: "rsc".to_string(),
                values: Some(vec![b"1".to_vec()]),
            }],
            template_fingerprint: "abc123".to_string(),
            schema_version: TEMPLATE_SCHEMA_VERSION,
        }
    }

    struct CountingReservation(Arc<AtomicUsize>);

    impl PlatformTemplateCacheReservation for CountingReservation {
        fn insert(
            self: Box<Self>,
            _metadata: &TemplateMetadata,
            _body: Vec<u8>,
            _max_age: std::time::Duration,
        ) -> Result<(), TemplateCacheError> {
            Ok(())
        }

        fn cancel(self: Box<Self>) -> Result<(), TemplateCacheError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[test]
    fn dropping_an_unfulfilled_reservation_cancels_exactly_once() {
        let cancellations = Arc::new(AtomicUsize::new(0));
        drop(TemplateCacheReservation::new(Box::new(
            CountingReservation(Arc::clone(&cancellations)),
        )));
        assert_eq!(cancellations.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn fulfilling_a_reservation_does_not_also_cancel_on_drop() {
        let cancellations = Arc::new(AtomicUsize::new(0));
        TemplateCacheReservation::new(Box::new(CountingReservation(Arc::clone(&cancellations))))
            .insert(
                &TemplateMetadata {
                    content_encoding: "identity".to_string(),
                    policy_headers: Vec::new(),
                    content_type: "text/html".to_string(),
                    schema_version: TEMPLATE_SCHEMA_VERSION,
                    body_len: 0,
                },
                Vec::new(),
                std::time::Duration::from_secs(1),
            )
            .expect("should fulfil the reservation");

        assert_eq!(
            cancellations.load(Ordering::SeqCst),
            0,
            "should discharge a fulfilled reservation without cancelling it"
        );
    }

    /// Every field must change the key. A field that does not is a cross-serving
    /// bug: two requests needing different templates would share one entry.
    #[test]
    fn every_field_changes_the_key() {
        let base = key().to_cache_key();

        let mut mode = key();
        mode.assembly_mode = AssemblyMode::Inline;
        assert_ne!(
            mode.to_cache_key(),
            base,
            "assembly mode must change the key"
        );

        let mut url = key();
        url.url = "https://example.com/other".to_string();
        assert_ne!(url.to_cache_key(), base, "url must change the key");

        let mut host = key();
        host.request_host = "other.example.com".to_string();
        assert_ne!(host.to_cache_key(), base, "host must change the key");

        let mut scheme = key();
        scheme.request_scheme = "http".to_string();
        assert_ne!(scheme.to_cache_key(), base, "scheme must change the key");

        let mut origin = key();
        origin.origin_identity = "https://origin.example.com\0other.example.com".to_string();
        assert_ne!(
            origin.to_cache_key(),
            base,
            "origin Host identity must change the key"
        );

        let mut fingerprint = key();
        fingerprint.template_fingerprint = "def456".to_string();
        assert_ne!(
            fingerprint.to_cache_key(),
            base,
            "template fingerprint must change the key"
        );

        let mut schema = key();
        schema.schema_version = TEMPLATE_SCHEMA_VERSION + 1;
        assert_ne!(
            schema.to_cache_key(),
            base,
            "schema version must change the key"
        );

        let mut vary = key();
        vary.vary_values = vec![VaryHeaderValues {
            name: "rsc".to_string(),
            values: Some(vec![b"0".to_vec()]),
        }];
        assert_ne!(vary.to_cache_key(), base, "vary values must change the key");
    }

    /// The reason for length prefixes rather than a delimiter.
    #[test]
    fn values_containing_delimiters_cannot_collide() {
        let mut a = key();
        a.request_host = "a".to_string();
        a.url = "b:c".to_string();

        let mut b = key();
        b.request_host = "a:b".to_string();
        b.url = "c".to_string();

        assert_ne!(
            a.to_cache_key(),
            b.to_cache_key(),
            "field values containing the delimiter must not produce the same key; a \
             collision here serves one visitor's template to another"
        );
    }

    #[test]
    fn rendered_key_is_fixed_size_and_contains_no_request_material() {
        let rendered = key().to_cache_key();
        assert_eq!(
            rendered,
            "ts-template-cache-v4-54431eb4ea82644d6378717a8c3f18302fafbf739e684598da79e392b16900a6"
        );
        assert!(rendered.starts_with("ts-template-cache-v4-"));
        assert_eq!(rendered.len(), 85);
        for sensitive in ["example.com", "/news/article", "rsc", "abc123"] {
            assert!(
                !rendered.contains(sensitive),
                "key leaked `{sensitive}`: {rendered}"
            );
        }
    }

    #[test]
    fn vary_header_names_are_matched_case_insensitively() {
        let mut upper = key();
        upper.vary_values = vec![VaryHeaderValues {
            name: "RSC".to_string(),
            values: Some(vec![b"1".to_vec()]),
        }];
        assert_eq!(
            upper.to_cache_key(),
            key().to_cache_key(),
            "header names are case-insensitive, so casing must not split the cache"
        );
    }

    #[test]
    fn vary_values_are_order_sensitive() {
        // The origin lists them in a fixed order and the caller preserves it, so a
        // differing order means differing inputs rather than the same request.
        let mut a = key();
        a.vary_values = vec![
            VaryHeaderValues {
                name: "rsc".to_string(),
                values: Some(vec![b"1".to_vec()]),
            },
            VaryHeaderValues {
                name: "x-route".to_string(),
                values: Some(vec![b"article".to_vec()]),
            },
        ];
        let mut b = key();
        b.vary_values = vec![
            VaryHeaderValues {
                name: "x-route".to_string(),
                values: Some(vec![b"article".to_vec()]),
            },
            VaryHeaderValues {
                name: "rsc".to_string(),
                values: Some(vec![b"1".to_vec()]),
            },
        ];
        assert_ne!(a.to_cache_key(), b.to_cache_key());
    }

    #[test]
    fn surrogate_keys_carry_a_global_and_a_per_url_lever() {
        let keys = key().surrogate_keys();
        assert!(
            keys.contains(&TEMPLATE_CACHE_PURGE_ALL_SURROGATE_KEY.to_string()),
            "a global purge lever is what makes rollback possible"
        );
        assert_eq!(keys.len(), 2, "global plus per-URL");
        assert!(
            !keys[1].contains(char::is_whitespace),
            "surrogate keys are space-delimited; whitespace would purge more than \
             intended, got {:?}",
            keys[1]
        );
        assert!(
            !keys[1].contains('/') && !keys[1].contains(':'),
            "URL punctuation must be reduced, got {:?}",
            keys[1]
        );
    }

    #[test]
    fn punctuation_distinct_urls_have_distinct_surrogate_keys() {
        let mut slash = key();
        slash.url = "https://example.com/a/b".to_string();
        let mut colon = key();
        colon.url = "https://example.com/a:b".to_string();
        assert_ne!(slash.surrogate_keys()[1], colon.surrogate_keys()[1]);
    }

    #[test]
    fn an_absent_vary_header_is_distinct_from_an_empty_one() {
        // "absent" and "present but empty" are different requests to the origin, so
        // they must not share a template.
        let spec = VarySpec::new(["RSC".to_string()]);
        let absent_headers = http::HeaderMap::new();
        let absent = spec.values_from(&absent_headers);
        let mut empty_headers = http::HeaderMap::new();
        empty_headers.insert("rsc", http::HeaderValue::from_static(""));
        let empty = spec.values_from(&empty_headers);
        assert_ne!(absent, empty);

        // The distinction that does matter: a present value differs from both.
        let mut present_headers = http::HeaderMap::new();
        present_headers.insert("rsc", http::HeaderValue::from_static("1"));
        let present = spec.values_from(&present_headers);
        assert_ne!(present, absent);
    }

    #[test]
    fn repeated_and_non_utf8_vary_values_are_preserved() {
        let spec = VarySpec::new(["x-route".to_string()]);
        let mut headers = http::HeaderMap::new();
        headers.append("x-route", http::HeaderValue::from_static("first"));
        headers.append(
            "x-route",
            http::HeaderValue::from_bytes(b"\xffsecond").expect("obs-text is valid field data"),
        );
        assert_eq!(
            spec.values_from(&headers),
            vec![VaryHeaderValues {
                name: "x-route".to_string(),
                values: Some(vec![b"first".to_vec(), b"\xffsecond".to_vec()]),
            }]
        );
    }

    #[test]
    fn vary_spec_lowercases_configured_names() {
        assert_eq!(
            VarySpec::new(["RSC".to_string(), "Accept-Encoding".to_string()]).names(),
            ["rsc"]
        );
    }

    #[test]
    fn vary_spec_rejects_invalid_names_and_deduplicates_case_insensitively() {
        assert_eq!(
            VarySpec::try_new(["not a header".to_string()]),
            Err("not a header".to_string())
        );
        assert_eq!(
            VarySpec::try_new(["RSC".to_string(), "rsc".to_string()])
                .expect("valid names")
                .names(),
            ["rsc"]
        );
    }

    #[test]
    fn drift_is_detected_when_the_origin_varies_on_something_unconfigured() {
        // The failure mode configured-Vary has: the origin adds a header to its Vary,
        // nobody updates config, and requests differing only in that header start
        // sharing a template.
        let spec = VarySpec::new(["rsc".to_string()]);

        assert!(
            spec.uncovered_by(["rsc"]).is_empty(),
            "a fully covered Vary is not drift"
        );
        assert_eq!(
            spec.uncovered_by(["rsc, next-router-prefetch, Accept-Encoding"]),
            vec!["next-router-prefetch"],
            "uncovered names must be reported so the stale config is identifiable; \
             accept-encoding is excluded because the key covers it structurally"
        );
    }

    #[test]
    fn a_key_field_counts_as_coverage_without_being_configured() {
        // The failure this prevents is silent and total: every compressing origin sends
        // `Vary: Accept-Encoding`, so treating it as a gap means the cache never stores
        // anything, and a spike measuring hit rate would report ~0 and look like a
        // finding rather than a bug.
        let spec = VarySpec::new([]);

        assert!(
            spec.uncovered_by(["Accept-Encoding"]).is_empty(),
            "the shared path uses one upstream encoding offer and stores identity bytes"
        );
        assert_eq!(
            spec.uncovered_by(["accept-encoding, rsc"]),
            vec!["rsc"],
            "only the genuinely uncovered name should be reported"
        );
    }

    #[test]
    fn a_wildcard_vary_is_not_reported_as_a_named_gap() {
        // `Vary: *` means uncacheable, which the eligibility gate handles. Reporting
        // it here would produce a nonsense "configure a header called *".
        let spec = VarySpec::new(["rsc".to_string()]);
        assert!(spec.uncovered_by(["*"]).is_empty());
    }

    #[test]
    fn metadata_round_trips() {
        let metadata = TemplateMetadata {
            content_encoding: "identity".to_string(),
            policy_headers: vec![
                (
                    "content-security-policy".to_string(),
                    "default-src 'self'".to_string(),
                ),
                (
                    "content-security-policy".to_string(),
                    "script-src 'self'".to_string(),
                ),
                (
                    "link".to_string(),
                    "</app.js>; rel=preload; as=script".to_string(),
                ),
            ],
            content_type: "text/html; charset=utf-8".to_string(),
            schema_version: TEMPLATE_SCHEMA_VERSION,
            body_len: 42,
        };
        let encoded = metadata.encode().expect("valid metadata should encode");
        let decoded = TemplateMetadata::decode(&encoded).expect("should decode what it encoded");
        assert_eq!(decoded, metadata);
    }

    #[test]
    fn metadata_encoding_rejects_line_break_injection() {
        for metadata in [
            TemplateMetadata {
                content_encoding: "identity\nh=link:</evil>".to_string(),
                policy_headers: Vec::new(),
                content_type: "text/html".to_string(),
                schema_version: TEMPLATE_SCHEMA_VERSION,
                body_len: 0,
            },
            TemplateMetadata {
                content_encoding: "identity".to_string(),
                policy_headers: vec![(
                    "content-security-policy".to_string(),
                    "default-src 'self'\r\nh=link:</evil>".to_string(),
                )],
                content_type: "text/html".to_string(),
                schema_version: TEMPLATE_SCHEMA_VERSION,
                body_len: 0,
            },
        ] {
            assert!(
                metadata.encode().is_err(),
                "should reject metadata fields that can inject another line"
            );
        }
    }

    #[test]
    fn unparseable_metadata_is_a_miss_not_a_panic() {
        for raw in [
            &b"not-key-value"[..],
            &b"v=notanumber\nce=gzip\nct=text/html\nlen=1"[..],
            &b"v=1\nce=gzip\nct=text/html"[..],
            &b"v=1\nce=gzip\nct=text/html\nlen=1\nunexpected=1"[..],
            &b"v=1\nv=1\nce=identity\nct=text/html\nlen=1"[..],
            &b"v=1\nce=identity\nct=text/html\nlen=1\nh=cache-control:public"[..],
            &b"v=1\nce=identity\nct=text/html\nlen=1\nh=not-a-policy:value"[..],
            &b"v=1\nce=identity\nct=text/html\nlen=1\nh=malformed"[..],
            &b"v=1\nce=identity\nct=application/json\nlen=1"[..],
            &[0xff, 0xfe][..],
        ] {
            assert_eq!(
                TemplateMetadata::decode(raw),
                None,
                "malformed metadata must be a miss, not a partial read: {raw:?}"
            );
        }
    }

    #[test]
    fn the_policy_allowlist_covers_document_security_and_delivery_headers() {
        for required in [
            "strict-transport-security",
            "cross-origin-opener-policy",
            "cross-origin-embedder-policy",
            "cross-origin-resource-policy",
            "origin-agent-cluster",
            "reporting-endpoints",
            "report-to",
            "link",
        ] {
            assert!(
                REPLAYABLE_POLICY_HEADERS.contains(&required),
                "warm ESI hits must preserve {required}"
            );
        }
    }

    #[tokio::test]
    async fn the_null_object_reports_unsupported_rather_than_failing() {
        // Degrading to per-request transformation keeps the shared modes portable on
        // adapters with no cache; erroring would make them Fastly-only outright.
        let cache = UnavailableTemplateCache;
        assert_eq!(
            cache.get(&key()).await.err(),
            Some(TemplateCacheMiss::Unsupported)
        );
        assert!(matches!(
            cache
                .put(
                    &key(),
                    &TemplateMetadata {
                        content_encoding: "identity".to_string(),
                        policy_headers: Vec::new(),
                        content_type: "text/html".to_string(),
                        schema_version: TEMPLATE_SCHEMA_VERSION,
                        body_len: 0,
                    },
                    Vec::new(),
                    std::time::Duration::from_secs(1)
                )
                .await,
            Err(TemplateCacheError::Unsupported)
        ));
    }
}
