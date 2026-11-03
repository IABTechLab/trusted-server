//! The `encypher-preparation/1` protocol between a publisher's delivery
//! instance and its native preparation worker.
//!
//! Everything both sides must agree on byte for byte lives here: the
//! preparation identities, the published records readers look up, the
//! retirement markers, the reader hint, the snapshot capability and the
//! snapshot facts. The reader and the worker call these functions, never a
//! copy of them, so an identity the reader computes from the bytes it
//! delivers is the identity the worker signs under.
//!
//! An identity is a versioned, domain-separated SHA-256 over
//! length-delimited parts. A part's length is written before it, so no two
//! different lists of parts hash alike, and the domain names the kind of
//! item and its identity contract version, so no identity of one kind matches
//! another's. Identity versions can change without changing the wire protocol.

use std::collections::BTreeMap;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use hmac::{Hmac, KeyInit as _, Mac as _};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use subtle::ConstantTimeEq as _;

use crate::api::EmbeddingPlan;
use crate::media_api::{hex_lower, sha256_hex};

/// The protocol every message between the delivery instance and the worker
/// carries.
pub const PREPARATION_PROTOCOL: &str = "encypher-preparation/1";

/// The version of the text projection an identity binds. A corrected
/// projection takes a new version, so it never reuses an old plan.
pub const TEXT_PROJECTION_VERSION: &str = "visible-text/1";

/// The request header the worker and the delivery instance authenticate
/// control requests with. Its value is the scope's secret, compared in
/// constant time, never logged.
pub const TOKEN_HEADER: &str = "x-encypher-preparation-token";

/// The request header carrying a snapshot capability on the delivery
/// instance's own loopback read. It never reaches an origin.
pub const CAPABILITY_HEADER: &str = "x-encypher-preparation-capability";

/// The response header reporting what the origin's own headers said about a
/// snapshot, authenticated for one capability's nonce.
pub const FACTS_HEADER: &str = "x-encypher-preparation-facts";

/// The delivery instance's snapshot route.
pub const SNAPSHOT_PATH: &str = "/_encypher/preparation/snapshot";
/// The delivery instance's result publication route, below which an
/// identity follows.
pub const RESULTS_PREFIX: &str = "/_encypher/preparation/results/";
/// The delivery instance's retirement marker route, below which an identity
/// follows.
pub const MARKERS_PREFIX: &str = "/_encypher/preparation/markers/";
/// The publisher route serving explicitly public source snapshots, below
/// which a raw-source SHA-256 follows.
pub const PUBLIC_SOURCE_PREFIX: &str = "/_encypher/preparation/source/";

/// The worker's reader-signal route.
pub const SIGNALS_PATH: &str = "/v1/preparation/signals";
/// The worker's operator warming route.
pub const WARM_PATH: &str = "/v1/preparation/warm";
/// The worker's operator retirement route.
pub const RETIRE_PATH: &str = "/v1/preparation/retire";
/// The worker's operator status route.
pub const STATUS_PATH: &str = "/v1/preparation/status";
/// The worker's readiness route.
pub const READY_PATH: &str = "/v1/preparation/ready";
/// The worker's public source route, which only the delivery instance
/// calls, below which a raw-source SHA-256 follows.
pub const WORKER_SOURCE_PREFIX: &str = "/v1/preparation/sources/";

/// The largest reader hint, in bytes.
pub const MAX_SIGNAL_BYTES: usize = 8 * 1024;
/// The largest target path a hint or warm names.
pub const MAX_TARGET_PATH_BYTES: usize = 1024;
/// The most variant parameters a schema names.
pub const MAX_VARIANT_PARAMS: usize = 8;
/// The most values one variant parameter admits.
pub const MAX_VARIANT_VALUES: usize = 32;

/// How long a snapshot capability stays valid, in seconds.
pub const CAPABILITY_LIFETIME_SECONDS: u64 = 120;

type HmacSha256 = Hmac<Sha256>;

/// SHA-256, lower case hex, over `domain` and each part, every part's
/// length written before it.
#[must_use]
pub fn domain_digest(domain: &str, parts: &[&[u8]]) -> String {
    let mut digest = Sha256::new();
    digest.update(
        u64::try_from(domain.len())
            .unwrap_or(u64::MAX)
            .to_be_bytes(),
    );
    digest.update(domain.as_bytes());
    for part in parts {
        digest.update(u64::try_from(part.len()).unwrap_or(u64::MAX).to_be_bytes());
        digest.update(part);
    }
    hex_lower(&digest.finalize())
}

/// Whether `value` is a lower case SHA-256 hex digest, the shape of every
/// identity.
#[must_use]
pub fn is_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// The credential context every identity binds: the API origin, the claimed
/// domain and the worker credential, hashed, so two credentials or two
/// publishers never share a result. Never a secret.
#[must_use]
pub fn credential_context(api_base_url: &str, domain: &str, worker_token: &str) -> String {
    let hash = sha256_hex(format!("{api_base_url}\n{domain}\n{}", worker_token.trim()).as_bytes());
    hash.get(..16).unwrap_or(hash.as_str()).to_owned()
}

/// What an article's preparation identity binds: every field of the sign
/// request that affects its signed assertions, the projection version and
/// the admitted variant. Its text-only hash domain also versions the API
/// cache contract: version 2 requires exact-request isolation, so records
/// made through the old coarse cache never satisfy a current lookup.
#[derive(Debug, Clone, Copy)]
pub struct TextIdentity<'a> {
    /// See [`credential_context`].
    pub credential_context: &'a str,
    /// The policy the article is signed under.
    pub policy_digest: &'a str,
    /// The query-free page address the article is signed for.
    pub page_url: &'a str,
    /// The admitted variant, see [`VariantSchema::admit`].
    pub variant: &'a str,
    /// The exact visible text sent for signing.
    pub text: &'a str,
    /// The page title sent, if any.
    pub document_title: Option<&'a str>,
    /// The article boundary selector sent.
    pub boundary_selector: &'a str,
}

impl TextIdentity<'_> {
    /// The identity.
    #[must_use]
    pub fn digest(&self) -> String {
        let text = sha256_hex(self.text.as_bytes());
        let (title_present, title) = match self.document_title {
            Some(title) => (&b"1"[..], title.as_bytes()),
            None => (&b"0"[..], &b""[..]),
        };
        domain_digest(
            "encypher-preparation/2:text",
            &[
                self.credential_context.as_bytes(),
                self.policy_digest.as_bytes(),
                self.page_url.as_bytes(),
                self.variant.as_bytes(),
                TEXT_PROJECTION_VERSION.as_bytes(),
                text.as_bytes(),
                title_present,
                title,
                self.boundary_selector.as_bytes(),
            ],
        )
    }
}

/// How a media item's manifest is delivered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Carrier {
    /// Unchanged bytes and a remote manifest named by a `Link`.
    External,
    /// The manifest inside the delivered bytes.
    Embedded,
}

impl Carrier {
    /// The name the protocol writes.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::External => "external",
            Self::Embedded => "embedded",
        }
    }
}

/// What a bounded media item's preparation identity binds.
#[derive(Debug, Clone, Copy)]
pub struct MediaIdentity<'a> {
    /// See [`credential_context`].
    pub credential_context: &'a str,
    /// The policy the item is signed under.
    pub policy_digest: &'a str,
    /// The reader-facing address the item is delivered on or in.
    pub page_url: &'a str,
    /// The admitted variant of that address.
    pub variant: &'a str,
    /// How the manifest is delivered.
    pub carrier: Carrier,
    /// The canonical media type of the source.
    pub mime: &'a str,
    /// SHA-256 of the exact source bytes, lower case hex.
    pub source_sha256: &'a str,
}

impl MediaIdentity<'_> {
    /// The identity.
    #[must_use]
    pub fn digest(&self) -> String {
        domain_digest(
            "encypher-preparation/1:media",
            &[
                self.credential_context.as_bytes(),
                self.policy_digest.as_bytes(),
                self.page_url.as_bytes(),
                self.variant.as_bytes(),
                self.carrier.as_str().as_bytes(),
                self.mime.as_bytes(),
                self.source_sha256.as_bytes(),
            ],
        )
    }
}

/// The source-version locator of a large item: the configured origin and
/// the strong validators it answered with. An entity tag is not
/// cryptographic evidence; this relies on a publisher-controlled origin
/// honoring strong validator semantics, and the published record carries
/// the whole-source digest the worker verified.
#[derive(Debug, Clone, Copy)]
pub struct LargeLocator<'a> {
    /// See [`credential_context`].
    pub credential_context: &'a str,
    /// The policy the item is signed under.
    pub policy_digest: &'a str,
    /// The query-free reader address.
    pub reader_url: &'a str,
    /// The admitted variant of that address.
    pub variant: &'a str,
    /// The lower case host the source is fetched from.
    pub origin_host: &'a str,
    /// SHA-256 of core's configured-origin identity, not just its hostname.
    pub origin_identity_sha256: &'a str,
    /// The canonical media type.
    pub mime: &'a str,
    /// The single strong entity tag, quoted.
    pub etag: &'a str,
    /// The total length.
    pub length: u64,
}

impl LargeLocator<'_> {
    /// The identity.
    #[must_use]
    pub fn digest(&self) -> String {
        let length = self.length.to_string();
        domain_digest(
            "encypher-preparation/1:large",
            &[
                self.credential_context.as_bytes(),
                self.policy_digest.as_bytes(),
                self.reader_url.as_bytes(),
                self.variant.as_bytes(),
                self.origin_host.as_bytes(),
                self.origin_identity_sha256.as_bytes(),
                self.mime.as_bytes(),
                self.etag.as_bytes(),
                length.as_bytes(),
            ],
        )
    }
}

/// The KV key a published result is stored under.
#[must_use]
pub fn result_key(identity: &str) -> String {
    format!("encypher:prep:v1:result:{identity}")
}

/// The KV key a retirement marker is stored under, apart from the result so
/// a late result can never overwrite it.
#[must_use]
pub fn marker_key(identity: &str) -> String {
    format!("encypher:prep:v1:retired:{identity}")
}

/// A result the delivery instance verified through the authenticated API
/// and stored under its immutable identity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublishedRecord {
    /// [`PREPARATION_PROTOCOL`].
    pub protocol: String,
    /// The identity the record is stored under.
    pub identity: String,
    /// The retirement epoch the result was prepared under.
    pub retirement_epoch: u64,
    /// What a reader applies.
    pub result: PublishedResult,
}

/// What a reader applies for one identity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PublishedResult {
    /// An article's marker plan.
    Text {
        /// The verified plan.
        plan: EmbeddingPlan,
        /// The manifest URL written in the head, if any.
        manifest_url: Option<String>,
        /// The scalars of the text the plan was made for.
        scalars: usize,
    },
    /// Unchanged bounded media with a remote manifest.
    External {
        /// The exact source the manifest binds.
        source_sha256: String,
        /// Headers every answer carries: the manifest `Link` and ids.
        headers: Vec<(String, String)>,
    },
    /// Bounded media with its manifest embedded.
    Embedded {
        /// The exact source the output was made from.
        source_sha256: String,
        /// SHA-256 of the output bytes.
        output_sha256: String,
        /// The output's media type.
        content_type: String,
        /// The output bytes, base64.
        output_b64: String,
        /// Headers every answer carries: the manifest ids.
        headers: Vec<(String, String)>,
    },
    /// An unchanged large source with a remote manifest.
    Large {
        /// SHA-256 of every byte of the version.
        sha256: String,
        /// The version's length.
        length: u64,
        /// Headers every answer carries.
        headers: Vec<(String, String)>,
    },
}

/// The marker that retires an identity on the delivery instance. A reader
/// refuses the identity's result while it is present, and a result
/// publication never removes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetirementMarker {
    /// The retirement epoch that retired the identity.
    pub epoch: u64,
}

/// Which kind of item a hint, job or result is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    /// An article's text.
    Text,
    /// A bounded media item delivered on its own address.
    Media,
    /// A media item embedded in a page or stylesheet.
    Embedded,
    /// A large media item, by its validator locator.
    Large,
}

/// One item a reader observed and found no published result for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservedItem {
    /// What kind of item.
    pub kind: ItemKind,
    /// Its identity as the reader computed it from what it delivered.
    pub identity: String,
}

/// A configured target: a publisher path and an admitted variant.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    /// The publisher's path, starting with `/`, without a query.
    pub path: String,
    /// The admitted variant, see [`VariantSchema::admit`], empty for none.
    #[serde(default)]
    pub variant: String,
}

impl Target {
    /// Whether the target is well formed: an absolute path within its
    /// bound, without a query, fragment, `..` segment or control byte.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        self.path.starts_with('/')
            && !self.path.starts_with("//")
            && self.path.len() <= MAX_TARGET_PATH_BYTES
            && self
                .path
                .bytes()
                .all(|byte| byte.is_ascii_graphic() && !matches!(byte, b'?' | b'#' | b'\\'))
            && !self.path.split('/').any(|segment| segment == "..")
            && self.variant.len() <= MAX_TARGET_PATH_BYTES
    }

    /// The path and query the delivery instance reads for this target.
    #[must_use]
    pub fn path_and_query(&self) -> String {
        if self.variant.is_empty() {
            self.path.clone()
        } else {
            format!("{}?{}", self.path, self.variant)
        }
    }
}

/// A reader's hint that the items it delivered are not prepared.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignalRequest {
    /// [`PREPARATION_PROTOCOL`].
    pub protocol: String,
    /// The deployment the hint is for.
    pub deployment_id: String,
    /// The target the reader was served.
    pub target: Target,
    /// The items it found unprepared, cache misses first.
    pub items: Vec<ObservedItem>,
    /// How many more unprepared items did not fit the hint.
    #[serde(default)]
    pub unobserved: u32,
}

impl SignalRequest {
    /// The hint's JSON, holding as many of `items` as fit in
    /// [`MAX_SIGNAL_BYTES`] in their order, and counting the rest as
    /// unobserved. `None` where not even an empty hint fits.
    #[must_use]
    pub fn encode_bounded(
        deployment_id: &str,
        target: Target,
        items: &[ObservedItem],
    ) -> Option<Vec<u8>> {
        let mut request = Self {
            protocol: PREPARATION_PROTOCOL.to_owned(),
            deployment_id: deployment_id.to_owned(),
            target,
            items: Vec::with_capacity(items.len()),
            unobserved: 0,
        };
        let mut encoded = serde_json::to_vec(&request).ok()?;
        if encoded.len() > MAX_SIGNAL_BYTES {
            return None;
        }
        for (index, item) in items.iter().enumerate() {
            request.items.push(item.clone());
            request.unobserved = u32::try_from(items.len() - index - 1).unwrap_or(u32::MAX);
            let next = serde_json::to_vec(&request).ok()?;
            if next.len() > MAX_SIGNAL_BYTES {
                request.items.pop();
                request.unobserved = u32::try_from(items.len() - index).unwrap_or(u32::MAX);
                return serde_json::to_vec(&request).ok();
            }
            encoded = next;
        }
        Some(encoded)
    }
}

/// One variant parameter a deployment admits, with every value it may take.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VariantParam {
    /// The query parameter's name.
    pub name: String,
    /// Every value the parameter may take.
    pub values: Vec<String>,
}

/// The bounded variant schema a deployment configures: which query
/// parameters name a variant, and with which values.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct VariantSchema(pub Vec<VariantParam>);

impl VariantSchema {
    /// Whether the schema is within its bounds: at most
    /// [`MAX_VARIANT_PARAMS`] distinct non-empty names, each with between
    /// one and [`MAX_VARIANT_VALUES`] distinct values, every name and value
    /// at most 64 URL-safe bytes.
    #[must_use]
    pub fn is_bounded(&self) -> bool {
        let safe = |value: &str| {
            !value.is_empty()
                && value.len() <= 64
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        };
        let mut names = std::collections::BTreeSet::new();
        self.0.len() <= MAX_VARIANT_PARAMS
            && self.0.iter().all(|param| {
                let mut values = std::collections::BTreeSet::new();
                safe(&param.name)
                    && names.insert(param.name.as_str())
                    && !param.values.is_empty()
                    && param.values.len() <= MAX_VARIANT_VALUES
                    && param
                        .values
                        .iter()
                        .all(|value| safe(value) && values.insert(value.as_str()))
            })
    }

    /// The canonical variant a reader's `query` names, its admitted pairs
    /// sorted by name, or `None` where it carries anything the schema does
    /// not admit: an unknown name, an unlisted value, a name given twice. An
    /// empty query is the empty variant.
    #[must_use]
    pub fn admit(&self, query: &str) -> Option<String> {
        if query.is_empty() {
            return Some(String::new());
        }
        let mut pairs = BTreeMap::new();
        for (name, value) in url::form_urlencoded::parse(query.as_bytes()) {
            let param = self.0.iter().find(|param| param.name == name)?;
            if !param.values.iter().any(|allowed| *allowed == value) {
                return None;
            }
            if pairs
                .insert(param.name.clone(), value.into_owned())
                .is_some()
            {
                return None;
            }
        }
        Some(
            pairs
                .into_iter()
                .map(|(name, value)| format!("{name}={value}"))
                .collect::<Vec<_>>()
                .join("&"),
        )
    }
}

/// The key a snapshot capability is signed with: derived from the
/// publication secret, so the secret itself never signs anything a
/// request carries.
fn capability_key(publication_secret: &str, purpose: &str) -> HmacSha256 {
    let mut derive = HmacSha256::new_from_slice(publication_secret.as_bytes())
        .expect("should key an HMAC with any length");
    derive.update(b"encypher-preparation/1:");
    derive.update(purpose.as_bytes());
    let key = derive.finalize().into_bytes();
    HmacSha256::new_from_slice(&key).expect("should key an HMAC with a derived key")
}

fn mac_hex(mut mac: HmacSha256, message: &[u8]) -> String {
    mac.update(message);
    hex_lower(&mac.finalize().into_bytes())
}

/// The capability a snapshot route grants its own loopback read of
/// `path_and_query`, valid until `expires` with this `nonce`.
#[must_use]
pub fn snapshot_capability(
    publication_secret: &str,
    path_and_query: &str,
    nonce: &str,
    expires: u64,
) -> String {
    let message = format!("{path_and_query}\n{nonce}\n{expires}");
    let mac = mac_hex(
        capability_key(publication_secret, "capability"),
        message.as_bytes(),
    );
    format!("v1.{expires}.{nonce}.{mac}")
}

/// The nonce of a capability valid for `path_and_query` at `now`, or `None`
/// for any other value: malformed, expired, too far in the future, or not
/// signed with the publication secret. Compared in constant time.
#[must_use]
pub fn verify_snapshot_capability(
    publication_secret: &str,
    path_and_query: &str,
    capability: &str,
    now: u64,
) -> Option<String> {
    let mut parts = capability.splitn(4, '.');
    if parts.next()? != "v1" {
        return None;
    }
    let expires: u64 = parts.next()?.parse().ok()?;
    let nonce = parts.next()?;
    let mac = parts.next()?;
    if now > expires
        || expires.saturating_sub(now) > CAPABILITY_LIFETIME_SECONDS
        || nonce.len() != 32
        || !nonce.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return None;
    }
    let expected = snapshot_capability(publication_secret, path_and_query, nonce, expires);
    let expected_mac = expected.rsplit('.').next()?;
    bool::from(expected_mac.as_bytes().ct_eq(mac.as_bytes())).then(|| nonce.to_owned())
}

/// What the origin's own headers said about a snapshot, as the delivery
/// instance reports it, kept apart from the snapshot's own private
/// transport headers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotOrigin {
    /// The origin's status.
    pub status: u16,
    /// The origin marked its answer as one reader's.
    pub private: bool,
    /// The origin forbade transforming the body.
    pub no_transform: bool,
    /// The origin's own `Link` names a C2PA manifest.
    pub manifest_link: bool,
    /// The origin's `Content-Encoding`, empty for identity.
    pub content_encoding: String,
    /// The lower case host the source was fetched from.
    pub origin_host: String,
    /// SHA-256 of core's configured media-origin identity. Empty for HTML;
    /// large-source admission requires a valid digest.
    pub origin_identity_sha256: String,
    /// The exact fetched address, where it carries no user information,
    /// query or fragment.
    pub source_reference: Option<String>,
}

/// The facts header for `origin`, authenticated for `nonce`.
#[must_use]
pub fn facts_header(publication_secret: &str, nonce: &str, origin: &SnapshotOrigin) -> String {
    let body = serde_json::to_vec(origin).unwrap_or_default();
    let encoded = STANDARD.encode(&body);
    let mut message = nonce.as_bytes().to_vec();
    message.push(b'\n');
    message.extend_from_slice(encoded.as_bytes());
    let mac = mac_hex(capability_key(publication_secret, "facts"), &message);
    format!("v1.{encoded}.{mac}")
}

/// The origin facts a header reports, where it is authenticated for
/// `nonce`; `None` for anything else, such as a header an origin wrote.
#[must_use]
pub fn verify_facts_header(
    publication_secret: &str,
    nonce: &str,
    header: &str,
) -> Option<SnapshotOrigin> {
    let rest = header.strip_prefix("v1.")?;
    let (encoded, mac) = rest.rsplit_once('.')?;
    let mut message = nonce.as_bytes().to_vec();
    message.push(b'\n');
    message.extend_from_slice(encoded.as_bytes());
    let expected = mac_hex(capability_key(publication_secret, "facts"), &message);
    if !bool::from(expected.as_bytes().ct_eq(mac.as_bytes())) {
        return None;
    }
    serde_json::from_slice(&STANDARD.decode(encoded).ok()?).ok()
}

/// Whether `presented` is `secret`, compared in constant time. An empty
/// secret authenticates nothing.
#[must_use]
pub fn token_matches(secret: &str, presented: &str) -> bool {
    !secret.is_empty() && bool::from(secret.as_bytes().ct_eq(presented.as_bytes()))
}

/// The idempotency key a text preparation sends, derived from its identity
/// and attempt generation so a retry of one operation replays it.
#[must_use]
pub fn operation_key(identity: &str, generation: u64) -> String {
    domain_digest(
        "encypher-preparation/1:operation",
        &[identity.as_bytes(), generation.to_string().as_bytes()],
    )
}

/// The response header carrying a snapshot's metadata, base64 JSON of
/// [`SnapshotMeta`]; the body is the snapshot's bytes.
pub const SNAPSHOT_META_HEADER: &str = "x-encypher-preparation-snapshot";

/// What a snapshot request reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum SnapshotMode {
    /// The whole bounded representation.
    Whole,
    /// Headers only, for a source too large to hold whole.
    Head,
    /// One byte range of the version `if_match` names.
    Range {
        /// The first byte.
        first: u64,
        /// The last byte, inclusive.
        last: u64,
        /// The strong entity tag the range must come from.
        if_match: String,
    },
}

/// The worker's request for a snapshot of one configured target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotRequest {
    /// [`PREPARATION_PROTOCOL`].
    pub protocol: String,
    /// The deployment.
    pub deployment_id: String,
    /// The target read, which must lie under a configured route with an
    /// admitted variant.
    pub target: Target,
    /// 32 lower case hex characters the worker chose, which the facts the
    /// snapshot reports are authenticated for.
    pub nonce: String,
    /// What is read.
    pub read: SnapshotMode,
}

/// What a snapshot answer reports besides its bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotMeta {
    /// [`PREPARATION_PROTOCOL`].
    pub protocol: String,
    /// The reader-facing address the snapshot was read at, without query.
    pub page_url: String,
    /// The representation's `Content-Type`.
    pub content_type: String,
    /// Its single strong entity tag, quoted, where it has one.
    pub etag: Option<String>,
    /// Its total length, where known.
    pub length: Option<u64>,
    /// What the origin's own headers said, apart from the snapshot's own
    /// private transport headers.
    pub origin: SnapshotOrigin,
}

/// One result the worker asks the delivery instance to verify and publish.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResultEnvelope {
    /// [`PREPARATION_PROTOCOL`].
    pub protocol: String,
    /// The deployment.
    pub deployment_id: String,
    /// The identity the result is published under; the delivery instance
    /// recomputes it from the item and refuses any other.
    pub identity: String,
    /// The retirement epoch the result was prepared under.
    pub retirement_epoch: u64,
    /// The admitted variant the item was read under.
    #[serde(default)]
    pub variant: String,
    /// The item, as the delivery instance recovers and checks it.
    pub item: EnvelopeItem,
}

/// An item a [`ResultEnvelope`] carries. Never a signed result the worker
/// asserts: the delivery instance recovers each receipt from the API itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum EnvelopeItem {
    /// An article: the exact sign request and its idempotency key.
    Text {
        /// The exact sign request.
        request: super::pipeline::TextSignRequest,
        /// Its idempotency key.
        idempotency_key: String,
    },
    /// A bounded standalone item with an external manifest.
    Media {
        /// The verified plan.
        planned: super::pipeline::PlannedMedia,
        /// The source bytes, base64, which the delivery instance hashes.
        source_b64: String,
    },
    /// An embedded item.
    Embedded {
        /// The verified plan.
        planned: super::pipeline::PlannedMedia,
        /// The source bytes, base64.
        source_b64: String,
        /// The worker's output bytes, base64, which must be exactly the
        /// output the delivery instance builds from the receipt.
        output_b64: String,
    },
    /// A large item, named by its strong validator.
    Large {
        /// The verified plan.
        planned: super::pipeline::PlannedMedia,
        /// The strong entity tag the worker read every byte under.
        etag: String,
        /// Configured-origin identity observed in authenticated source facts.
        origin_identity_sha256: String,
    },
}

/// A retirement marker the worker asks the delivery instance to publish.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarkerEnvelope {
    /// [`PREPARATION_PROTOCOL`].
    pub protocol: String,
    /// The deployment.
    pub deployment_id: String,
    /// The identity retired.
    pub identity: String,
    /// The retirement epoch.
    pub epoch: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "publication-secret-for-tests";

    fn text(title: Option<&'static str>, variant: &'static str) -> TextIdentity<'static> {
        TextIdentity {
            credential_context: "0123456789abcdef",
            policy_digest: "a",
            page_url: "https://www.example.com/news/a",
            variant,
            text: "Harbor council votes",
            document_title: title,
            boundary_selector: "article",
        }
    }

    #[test]
    fn identities_bind_every_part_and_never_collide_by_concatenation() {
        let base = text(Some("Title"), "").digest();
        assert_eq!(base, text(Some("Title"), "").digest(), "should be stable");
        assert_ne!(base, text(None, "").digest(), "should bind title presence");
        assert_ne!(
            base,
            text(Some(""), "").digest(),
            "should tell empty title from none"
        );
        assert_ne!(
            base,
            text(Some("Title"), "edition=uk").digest(),
            "should bind the variant"
        );
        assert_ne!(
            domain_digest("d", &[b"ab", b"c"]),
            domain_digest("d", &[b"a", b"bc"]),
            "length prefixes should separate parts"
        );
        let media = MediaIdentity {
            credential_context: "0123456789abcdef",
            policy_digest: "a",
            page_url: "https://www.example.com/news/a",
            variant: "",
            carrier: Carrier::Embedded,
            mime: "image/png",
            source_sha256: "b",
        };
        let external = MediaIdentity {
            carrier: Carrier::External,
            ..media
        };
        assert_ne!(media.digest(), external.digest(), "should bind the carrier");
    }

    #[test]
    fn exact_request_text_identity_retires_the_coarse_cache_domain_only() {
        let article = text(Some("Title"), "");
        let text_hash = sha256_hex(article.text.as_bytes());
        let parts = [
            article.credential_context.as_bytes(),
            article.policy_digest.as_bytes(),
            article.page_url.as_bytes(),
            article.variant.as_bytes(),
            TEXT_PROJECTION_VERSION.as_bytes(),
            text_hash.as_bytes(),
            &b"1"[..],
            &b"Title"[..],
            article.boundary_selector.as_bytes(),
        ];
        assert_ne!(
            article.digest(),
            domain_digest("encypher-preparation/1:text", &parts),
            "old coarse-cache prepared records must not satisfy a current text lookup"
        );
        assert_eq!(
            article.digest(),
            domain_digest("encypher-preparation/2:text", &parts),
            "the new domain versions the text cache contract, not the projection"
        );
        let media = MediaIdentity {
            credential_context: article.credential_context,
            policy_digest: article.policy_digest,
            page_url: article.page_url,
            variant: article.variant,
            carrier: Carrier::Embedded,
            mime: "image/png",
            source_sha256: "b",
        };
        assert_eq!(
            media.digest(),
            domain_digest(
                "encypher-preparation/1:media",
                &[
                    media.credential_context.as_bytes(),
                    media.policy_digest.as_bytes(),
                    media.page_url.as_bytes(),
                    media.variant.as_bytes(),
                    media.carrier.as_str().as_bytes(),
                    media.mime.as_bytes(),
                    media.source_sha256.as_bytes(),
                ],
            ),
            "unchanged media must keep its existing prepared identity"
        );
    }

    #[test]
    fn capability_admits_only_its_own_path_secret_and_lifetime() {
        let nonce = "0123456789abcdef0123456789abcdef";
        let capability = snapshot_capability(SECRET, "/news/a", nonce, 1_100);
        assert_eq!(
            verify_snapshot_capability(SECRET, "/news/a", &capability, 1_000).as_deref(),
            Some(nonce),
            "should admit a fresh capability for its path"
        );
        assert!(
            verify_snapshot_capability(SECRET, "/news/b", &capability, 1_000).is_none(),
            "should refuse another path"
        );
        assert!(
            verify_snapshot_capability("other-secret", "/news/a", &capability, 1_000).is_none(),
            "should refuse another secret"
        );
        assert!(
            verify_snapshot_capability(SECRET, "/news/a", &capability, 1_101).is_none(),
            "should refuse an expired capability"
        );
        let far = snapshot_capability(SECRET, "/news/a", nonce, 10_000);
        assert!(
            verify_snapshot_capability(SECRET, "/news/a", &far, 1_000).is_none(),
            "should refuse a capability valid for longer than its lifetime"
        );
    }

    #[test]
    fn facts_header_verifies_only_for_its_nonce_and_secret() {
        let origin = SnapshotOrigin {
            status: 200,
            private: true,
            origin_host: "origin.example.com".to_owned(),
            ..SnapshotOrigin::default()
        };
        let header = facts_header(SECRET, "n1", &origin);
        assert_eq!(
            verify_facts_header(SECRET, "n1", &header),
            Some(origin),
            "should read back the facts"
        );
        assert!(
            verify_facts_header(SECRET, "n2", &header).is_none(),
            "should refuse another nonce"
        );
        assert!(
            verify_facts_header("other", "n1", &header).is_none(),
            "should refuse another secret, as an origin-written header"
        );
    }

    #[test]
    fn variant_schema_admits_only_bounded_listed_pairs() {
        let schema = VariantSchema(vec![
            VariantParam {
                name: "edition".to_owned(),
                values: vec!["us".to_owned(), "uk".to_owned()],
            },
            VariantParam {
                name: "amp".to_owned(),
                values: vec!["1".to_owned()],
            },
        ]);
        assert!(schema.is_bounded(), "should be within bounds");
        assert_eq!(schema.admit("").as_deref(), Some(""));
        assert_eq!(
            schema.admit("edition=uk&amp=1").as_deref(),
            Some("amp=1&edition=uk"),
            "should sort admitted pairs"
        );
        assert!(
            schema.admit("edition=fr").is_none(),
            "should refuse an unlisted value"
        );
        assert!(
            schema.admit("utm_source=x").is_none(),
            "should refuse an unknown name"
        );
        assert!(
            schema.admit("edition=us&edition=uk").is_none(),
            "should refuse a repeated name"
        );
        let unbounded = VariantSchema(vec![VariantParam {
            name: "q".to_owned(),
            values: Vec::new(),
        }]);
        assert!(!unbounded.is_bounded(), "should refuse an open parameter");
    }

    #[test]
    fn bounded_signal_keeps_items_in_order_and_counts_the_rest() {
        let items: Vec<ObservedItem> = (0..400)
            .map(|index| ObservedItem {
                kind: ItemKind::Embedded,
                identity: format!("{index:064}"),
            })
            .collect();
        let target = Target {
            path: "/news/a".to_owned(),
            variant: String::new(),
        };
        let encoded =
            SignalRequest::encode_bounded("deployment", target, &items).expect("should encode");
        assert!(encoded.len() <= MAX_SIGNAL_BYTES, "should fit the bound");
        let decoded: SignalRequest = serde_json::from_slice(&encoded).expect("should decode");
        assert!(!decoded.items.is_empty(), "should carry some items");
        assert_eq!(
            decoded.items[0], items[0],
            "should keep the first item first"
        );
        assert_eq!(
            decoded.items.len() + usize::try_from(decoded.unobserved).expect("should fit"),
            items.len(),
            "should count every item that did not fit"
        );
    }

    #[test]
    fn target_refuses_queries_traversal_and_relative_paths() {
        let ok = Target {
            path: "/news/a".to_owned(),
            variant: String::new(),
        };
        assert!(ok.is_well_formed());
        for path in [
            "news/a",
            "//evil.example.com/a",
            "/a?b=1",
            "/a/../b",
            "/a#b",
        ] {
            let target = Target {
                path: path.to_owned(),
                variant: String::new(),
            };
            assert!(!target.is_well_formed(), "should refuse {path}");
        }
    }
}
