//! The shared transformed-template cache (C2) for the #1009 ESI validation spike.
//!
//! Three caches are in play and conflating them is what produced the original wrong
//! conclusion in the design doc, so this module names which one it is:
//!
//! | Cache | Contents                          | Owner                          |
//! | ----- | --------------------------------- | ------------------------------ |
//! | C1    | raw origin bytes                  | Fastly read-through. Not this. |
//! | C2    | post-`lol_html`, pre-assembly     | **This module.**               |
//! | C3    | final per-user assembled response | **Must never exist.**          |
//!
//! C2 holds a *shared template*: no per-user bytes, and no decisions that depend on
//! the request. What may and may not live in it is
//! [§6.7 of the design doc](../../../../docs/superpowers/specs/2026-08-08-esi-cacheable-root-validation-design.md),
//! and the invariant is enforced by the rendered-document byte-identity tests in
//! `publisher`.
//!
//! Spike-only. Remove with the spike.

use core::fmt;

use crate::creative_opportunities::AssemblyMode;

/// Version of the transform that produced a cached template.
///
/// Bump on **any** change to what the transform emits. Without it a deploy reads
/// yesterday's template shape and assembles against markers that moved, which fails
/// as a rendering bug far from its cause rather than as a cache miss.
///
/// | Version | Transform |
/// | ------- | --------- |
/// | 1       | `</body>` seam marker was `<esi:include src="/_ts/page-bids?format=fragment"/>` |
/// | 2       | Marker is the inert comment [`SEAM_BIDS_MARKER`](crate::publisher::SEAM_BIDS_MARKER); the seam hands slots to `scheduleInitialAdInit` instead of assigning them |
pub const TEMPLATE_SCHEMA_VERSION: u32 = 2;

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
    /// A2 and A3 emit different template bytes. Without this they poison each
    /// other's entries.
    pub assembly_mode: AssemblyMode,
    /// Values of the request headers the **origin** declares it varies on, in the
    /// order the origin listed them. Not a fixed list: the origin is authoritative,
    /// and hard-coding one here would silently drift when the origin's changes.
    pub vary_values: Vec<(String, String)>,
    /// The `Accept-Encoding` sent to the origin, **not** the encoding the origin
    /// chose.
    ///
    /// The distinction is forced by ordering. The pipeline pairs input encoding to the
    /// same output encoding, so the transformed bytes inherit whatever the origin
    /// negotiated — and serving brotli bytes to a client that asked for gzip is a
    /// broken response, so encoding must be keyed. But **a lookup happens before the
    /// origin has chosen**, so the chosen value is unavailable at exactly the moment
    /// the key is needed. Keying on it would mean storing under `br` and looking up
    /// under `gzip, br`: a cache that never hits.
    ///
    /// Keying on the request side is sound because origin negotiation is a function of
    /// what it was offered, so identical offers yield identical choices. The encoding
    /// actually chosen is recorded in [`TemplateMetadata::content_encoding`] and is
    /// what the served response declares.
    ///
    /// Read as forwarded, after `restrict_accept_encoding` narrows it — the value the
    /// client sent is not necessarily the value the origin saw.
    pub accept_encoding: String,
    /// Identifies the enabled integration set and the tsjs bundle. Both change the
    /// injected markup for the same URL.
    pub integration_fingerprint: String,
    /// See [`TEMPLATE_SCHEMA_VERSION`].
    pub schema_version: u32,
}

impl TemplateCacheKey {
    /// Render the key as the opaque byte string the platform cache is keyed on.
    ///
    /// Fields are length-prefixed rather than delimiter-joined. A delimiter is
    /// ambiguous when a value can contain it — a URL with a `|`, or a `Vary` value
    /// with one — and two distinct keys colliding here means one visitor's template
    /// served to another. Length prefixes make that unrepresentable.
    #[must_use]
    pub fn to_cache_key(&self) -> String {
        let mut out = String::new();
        let mut push = |part: &str| {
            out.push_str(&part.len().to_string());
            out.push(':');
            out.push_str(part);
        };

        push("ts-c2");
        push(&self.schema_version.to_string());
        push(&format!("{:?}", self.assembly_mode));
        push(&self.request_scheme);
        push(&self.request_host);
        push(&self.url);
        push(&self.accept_encoding);
        push(&self.integration_fingerprint);

        push(&self.vary_values.len().to_string());
        for (name, value) in &self.vary_values {
            push(&name.to_ascii_lowercase());
            push(value);
        }

        out
    }

    /// Surrogate keys to attach at insert, for purge-based rollback.
    ///
    /// `ts-template` purges every template at once, which is the rollback lever.
    /// The per-URL key allows targeted invalidation. Both are needed: the broad one
    /// for an incident, the narrow one for ordinary invalidation.
    #[must_use]
    pub fn surrogate_keys(&self) -> Vec<String> {
        vec![
            "ts-template".to_string(),
            format!("ts-template-{}", surrogate_safe(&self.url)),
        ]
    }
}

/// Reduce a URL to characters valid in a Fastly surrogate key.
///
/// Surrogate keys are space-delimited, so any whitespace would split one key into
/// several and purge more than intended.
fn surrogate_safe(url: &str) -> String {
    url.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
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
    "x-frame-options",
    "x-content-type-options",
    "content-language",
    "x-robots-tag",
];

/// Headers the key covers by construction, whatever the operator configured.
///
/// `Accept-Encoding` has a dedicated key field ([`TemplateCacheKey::accept_encoding`]),
/// so an origin declaring `Vary: Accept-Encoding` is already keyed correctly. Without
/// this, that extremely ordinary declaration — any compressing origin sends it — reads
/// as an uncovered gap and disqualifies the response, so **C2 would never cache anything
/// against a real origin** unless the operator redundantly listed a header the key
/// already covers. Found by review before it could make the spike measure a hit rate of
/// approximately zero and read that as a result.
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
/// `Vary` and the spike's TTL is short, so drift is bounded by a minute rather than
/// indefinite. **This is a spike-grade choice, not a production one** — see the
/// drift guard below.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VarySpec {
    /// Header names, lowercased, in a fixed order.
    names: Vec<String>,
}

impl VarySpec {
    /// Build from configured header names.
    #[must_use]
    pub fn new(names: impl IntoIterator<Item = String>) -> Self {
        Self {
            names: names.into_iter().map(|n| n.to_ascii_lowercase()).collect(),
        }
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
    pub fn values_from<'a, F>(&self, header: F) -> Vec<(String, String)>
    where
        F: Fn(&str) -> Option<&'a str>,
    {
        self.names
            .iter()
            .map(|name| (name.clone(), header(name).unwrap_or_default().to_string()))
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
    /// Encoding of the stored bytes. Also in the key; stored so a reader need not
    /// re-derive it.
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

impl TemplateMetadata {
    /// Serialize for `user_metadata`. Deliberately a tiny hand-rolled format rather
    /// than JSON — one allocation, no dependency, and a parse failure is
    /// unambiguous.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = format!(
            "v={}\nce={}\nct={}\nlen={}",
            self.schema_version, self.content_encoding, self.content_type, self.body_len
        );
        for (name, value) in &self.policy_headers {
            // Header values cannot contain newlines (the HTTP parser rejects them), so a
            // newline-delimited encoding cannot be broken by a header value.
            out.push_str(&format!("\nh={name}:{value}"));
        }
        out.into_bytes()
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
                "v" => schema_version = Some(value.parse().ok()?),
                "ce" => content_encoding = Some(value.to_string()),
                "h" => {
                    if let Some((name, header_value)) = value.split_once(':') {
                        policy_headers.push((name.to_string(), header_value.to_string()));
                    }
                }
                "ct" => content_type = Some(value.to_string()),
                "len" => body_len = Some(value.parse().ok()?),
                _ => return None,
            }
        }
        Some(Self {
            schema_version: schema_version?,
            policy_headers,
            content_encoding: content_encoding?,
            content_type: content_type?,
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
    /// Read a template. `Err` is a miss, not a failure — every variant means
    /// "transform it yourself".
    async fn get(&self, key: &TemplateCacheKey) -> Result<TemplateEntry, TemplateCacheMiss>;

    /// Store a template.
    ///
    /// Callers must not call this without having consulted the C2 eligibility gate
    /// first: this method stores what it is given and cannot tell a shared template
    /// from a per-user one.
    async fn put(
        &self,
        key: &TemplateCacheKey,
        metadata: &TemplateMetadata,
        body: Vec<u8>,
    ) -> Result<(), TemplateCacheError>;

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
/// shared assembly modes degrade to transforming per request on Cloudflare, Axum and
/// Spin instead of failing — the modes stay portable, only the caching is not.
pub struct UnavailableTemplateCache;

#[async_trait::async_trait(?Send)]
impl PlatformTemplateCache for UnavailableTemplateCache {
    async fn get(&self, _key: &TemplateCacheKey) -> Result<TemplateEntry, TemplateCacheMiss> {
        Err(TemplateCacheMiss::Unsupported)
    }

    async fn put(
        &self,
        _key: &TemplateCacheKey,
        _metadata: &TemplateMetadata,
        _body: Vec<u8>,
    ) -> Result<(), TemplateCacheError> {
        Err(TemplateCacheError::Unsupported)
    }

    async fn purge_all(&self) -> Result<(), TemplateCacheError> {
        Err(TemplateCacheError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> TemplateCacheKey {
        TemplateCacheKey {
            url: "https://example.com/news/article".to_string(),
            request_host: "example.com".to_string(),
            request_scheme: "https".to_string(),
            assembly_mode: AssemblyMode::Esi,
            vary_values: vec![("rsc".to_string(), "1".to_string())],
            accept_encoding: "gzip".to_string(),
            integration_fingerprint: "abc123".to_string(),
            schema_version: TEMPLATE_SCHEMA_VERSION,
        }
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

        let mut encoding = key();
        encoding.accept_encoding = "br".to_string();
        assert_ne!(
            encoding.to_cache_key(),
            base,
            "accept encoding must change the key; serving brotli to a gzip client \
             is a broken response"
        );

        let mut fingerprint = key();
        fingerprint.integration_fingerprint = "def456".to_string();
        assert_ne!(
            fingerprint.to_cache_key(),
            base,
            "integration fingerprint must change the key"
        );

        let mut schema = key();
        schema.schema_version = TEMPLATE_SCHEMA_VERSION + 1;
        assert_ne!(
            schema.to_cache_key(),
            base,
            "schema version must change the key"
        );

        let mut vary = key();
        vary.vary_values = vec![("rsc".to_string(), "0".to_string())];
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
    fn vary_header_names_are_matched_case_insensitively() {
        let mut upper = key();
        upper.vary_values = vec![("RSC".to_string(), "1".to_string())];
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
            ("rsc".to_string(), "1".to_string()),
            ("accept-encoding".to_string(), "gzip".to_string()),
        ];
        let mut b = key();
        b.vary_values = vec![
            ("accept-encoding".to_string(), "gzip".to_string()),
            ("rsc".to_string(), "1".to_string()),
        ];
        assert_ne!(a.to_cache_key(), b.to_cache_key());
    }

    #[test]
    fn surrogate_keys_carry_a_global_and_a_per_url_lever() {
        let keys = key().surrogate_keys();
        assert!(
            keys.contains(&"ts-template".to_string()),
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
    fn an_absent_vary_header_is_distinct_from_an_empty_one() {
        // "absent" and "present but empty" are different requests to the origin, so
        // they must not share a template.
        let spec = VarySpec::new(["RSC".to_string()]);
        let absent = spec.values_from(|_| None);
        let empty = spec.values_from(|_| Some(""));
        assert_eq!(absent, empty, "both render as an empty value by design");

        // The distinction that does matter: a present value differs from both.
        let present = spec.values_from(|_| Some("1"));
        assert_ne!(present, absent);
    }

    #[test]
    fn vary_spec_lowercases_configured_names() {
        assert_eq!(
            VarySpec::new(["RSC".to_string(), "Accept-Encoding".to_string()]).names(),
            ["rsc", "accept-encoding"]
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
            "the key has a dedicated accept_encoding field, so this is already covered"
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
            content_encoding: "gzip".to_string(),
            policy_headers: Vec::new(),
            content_type: "text/html; charset=utf-8".to_string(),
            schema_version: TEMPLATE_SCHEMA_VERSION,
            body_len: 42,
        };
        let decoded =
            TemplateMetadata::decode(&metadata.encode()).expect("should decode what it encoded");
        assert_eq!(decoded, metadata);
    }

    #[test]
    fn unparseable_metadata_is_a_miss_not_a_panic() {
        for raw in [
            &b"not-key-value"[..],
            &b"v=notanumber\nce=gzip\nct=text/html\nlen=1"[..],
            &b"v=1\nce=gzip\nct=text/html"[..],
            &b"v=1\nce=gzip\nct=text/html\nlen=1\nunexpected=1"[..],
            &[0xff, 0xfe][..],
        ] {
            assert_eq!(
                TemplateMetadata::decode(raw),
                None,
                "malformed metadata must be a miss, not a partial read: {raw:?}"
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
                    Vec::new()
                )
                .await,
            Err(TemplateCacheError::Unsupported)
        ));
    }
}
