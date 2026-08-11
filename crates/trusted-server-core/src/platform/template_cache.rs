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
pub const TEMPLATE_SCHEMA_VERSION: u32 = 1;

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
    /// The negotiated content encoding of the stored bytes.
    ///
    /// The streaming pipeline pairs input encoding to the same output encoding, so
    /// the transformed bytes inherit whatever the origin chose from the client's
    /// `Accept-Encoding`. Serving brotli bytes to a client that asked for gzip is a
    /// broken response, so this is part of the key rather than of the payload.
    pub content_encoding: String,
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
        push(&self.content_encoding);
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
}

impl TemplateMetadata {
    /// Serialize for `user_metadata`. Deliberately a tiny hand-rolled format rather
    /// than JSON — one allocation, no dependency, and a parse failure is
    /// unambiguous.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        format!(
            "v={}\nce={}\nct={}\nlen={}",
            self.schema_version, self.content_encoding, self.content_type, self.body_len
        )
        .into_bytes()
    }

    /// Parse `user_metadata`. Returns `None` on anything unexpected, which callers
    /// must treat as a cache miss.
    #[must_use]
    pub fn decode(raw: &[u8]) -> Option<Self> {
        let text = core::str::from_utf8(raw).ok()?;
        let mut schema_version = None;
        let mut content_encoding = None;
        let mut content_type = None;
        let mut body_len = None;
        for line in text.lines() {
            let (key, value) = line.split_once('=')?;
            match key {
                "v" => schema_version = Some(value.parse().ok()?),
                "ce" => content_encoding = Some(value.to_string()),
                "ct" => content_type = Some(value.to_string()),
                "len" => body_len = Some(value.parse().ok()?),
                _ => return None,
            }
        }
        Some(Self {
            schema_version: schema_version?,
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
            content_encoding: "gzip".to_string(),
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
        mode.assembly_mode = AssemblyMode::ClientFill;
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
        encoding.content_encoding = "br".to_string();
        assert_ne!(
            encoding.to_cache_key(),
            base,
            "content encoding must change the key; serving brotli to a gzip client \
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
    fn metadata_round_trips() {
        let metadata = TemplateMetadata {
            content_encoding: "gzip".to_string(),
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
