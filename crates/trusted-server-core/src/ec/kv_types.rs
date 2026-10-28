//! KV identity graph schema types.
//!
//! These types define the JSON schema stored in the Fastly KV Store for the
//! EC identity graph. Each EC ID (`{64hex}.{6alnum}`) maps to a [`KvEntry`]
//! containing consent state, geo location, and accumulated partner IDs.
//!
//! The schema is versioned (`v: 2`). Version 1 rows, which hold one `uid` per
//! source, still read and are written back as version 2.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::consent::ConsentContext;
use crate::geo::GeoInfo;

/// Current schema version for KV entries.
pub const SCHEMA_VERSION: u8 = 2;

// Versions 1 and 2 read (see [`KvEntry::upgrade_for_read`]). Any other
// version fails closed on read. Future schema bumps must add an explicit lazy
// migration or backfill path before changing this value.

/// Maximum number of publisher-domain entries accepted in
/// [`KvPubProperties::seen_domains`].
///
/// New entries seed this set with the creation domain only. Runtime organic
/// requests no longer append domains.
pub const MAX_SEEN_DOMAINS: usize = 50;

/// Maximum allowed hostname length for publisher domains stored in KV.
pub const MAX_STORED_DOMAIN_LENGTH: usize = 255;

/// Maximum allowed length (in bytes) for a partner UID across all sync
/// mechanisms (pixel, batch, pull). Defined centrally to ensure consistent
/// validation.
pub const MAX_UID_LENGTH: usize = 512;

/// Maximum serialized bytes of a stored UID-level or EID-level `ext`.
pub const MAX_PARTNER_EXT_BYTES: usize = 2048;
/// Maximum bytes of a stored `inserter` or `matcher`.
pub const MAX_EID_FIELD_BYTES: usize = 255;
/// Maximum number of EID entries stored for one source.
pub const MAX_ENTRIES_PER_SOURCE: usize = 8;
/// Maximum number of UIDs stored for one source, across its entries.
pub const MAX_UIDS_PER_SOURCE: usize = 32;
/// Maximum bytes of one source's stored entry set, as measured by
/// [`entry_set_bytes`].
pub const MAX_ENTRY_SET_BYTES: usize = 4096;
/// Maximum serialized bytes of one provider's private state (`ps.st`).
pub const MAX_PROVIDER_STATE_BYTES: usize = 1024;
/// Maximum number of providers with a `ps` entry in one row.
pub const MAX_PROVIDER_STATES: usize = 32;
/// Maximum number of UID hashes kept in each of a provider's `ps.iv` and
/// `ps.ov` lists, enough for four whole 32-UID sets.
pub const MAX_INVALIDATED_UIDS: usize = 128;
/// Length of a record revision (`rv`) in lowercase hex digits.
pub const RECORD_REVISION_HEX_LEN: usize = 16;
/// Maximum length of an opt-out fingerprint (`ps.o.fp`) in lowercase hex
/// digits.
const OPT_OUT_FINGERPRINT_MAX_HEX_LEN: usize = 16;

/// Full KV entry stored as the body of an EC identity graph record.
///
/// **KV key:** Full EC ID (`{64hex}.{6alnum}`).
/// **KV value:** JSON-serialized `KvEntry`. This is a documented estimate, not
/// an enforced limit (spec 6.1): a one-UID record is about 120 bytes, and a
/// record at every cap is about 4.2 KB. With every cap reached, eight owned
/// sources come to about 34 KB, plus 1 KB for each provider with state, and
/// `ps.iv` and `ps.ov` add up to 2.4 KB each per provider. A row with two full
/// sets and two stateful providers at every cap is about 15 KB, and typical
/// sets are far smaller.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct KvEntry {
    /// Schema version — always [`SCHEMA_VERSION`].
    pub v: u8,
    /// Unix timestamp (seconds) of initial entry creation.
    pub created: u64,
    /// Consent state sub-object.
    pub consent: KvConsent,
    /// Geo location sub-object.
    pub geo: KvGeo,
    /// Creation-time publisher property metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pub_properties: Option<KvPubProperties>,
    /// Device class signals (TLS handshake, UA platform).
    /// Written once on creation — never updated after.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<KvDevice>,
    /// Network cluster disambiguation data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<KvNetwork>,
    /// Map of partner ID namespace → UID record.
    /// Populated by pixel sync, batch sync, and pull sync operations.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub ids: BTreeMap<String, KvPartnerId>,
    /// Per-provider scheduling, opt-out, removal and private state, keyed by
    /// identity provider ID. Never sent to bidders.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub ps: BTreeMap<String, KvProviderState>,
    /// The newest pending-key generation merged into this row (spec 5.3,
    /// Pending key). Items at or below it are never merged again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pg: Option<u64>,
}

/// Consent state within a KV entry.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct KvConsent {
    /// Raw TCF v2 consent string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tcf: Option<String>,
    /// Raw GPP consent string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpp: Option<String>,
    /// `true` for a live entry, `false` for a withdrawal tombstone.
    pub ok: bool,
    /// Unix timestamp (seconds) of last consent state change.
    pub updated: u64,
}

/// Geo location within a KV entry.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct KvGeo {
    /// ISO 3166-1 alpha-2 country code (e.g. `"US"`).
    pub country: String,
    /// ISO 3166-2 region code (e.g. `"CA"` for California).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    /// Autonomous System Number (e.g. `7922` = Comcast).
    /// Primary signal for distinguishing home ISP vs. corporate VPN.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asn: Option<u32>,
    /// DMA/metro code (e.g. `807` = San Francisco).
    /// Market-level targeting signal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dma: Option<i64>,
}

/// A source's record within a KV entry (spec 6.1).
///
/// Every record stores its source's entry set in `eids`. A version 1 record,
/// which holds one `uid`, reads as one entry holding that UID.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(from = "KvPartnerIdWire")]
pub struct KvPartnerId {
    /// Writer: an identity provider ID. Absent for browser ingest and
    /// version 1 records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub w: Option<String>,
    /// Record revision: 16 lowercase hex digits, random on every write.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rv: Option<String>,
    /// Unix seconds when the ID was acquired or last refreshed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<u64>,
    /// Unix seconds at or after which the ID must not be sent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exp: Option<u64>,
    /// Unix seconds after which the ID is due for refresh.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ra: Option<u64>,
    /// Unix seconds after which the ID can no longer be refreshed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ru: Option<u64>,
    /// The source's entry set, in order. Never empty in a valid record.
    pub eids: Vec<KvEid>,
}

/// The stored shape of [`KvPartnerId`], which also reads version 1's `uid`.
#[derive(Deserialize)]
struct KvPartnerIdWire {
    #[serde(default)]
    uid: Option<String>,
    #[serde(default)]
    w: Option<String>,
    #[serde(default)]
    rv: Option<String>,
    #[serde(default)]
    at: Option<u64>,
    #[serde(default)]
    exp: Option<u64>,
    #[serde(default)]
    ra: Option<u64>,
    #[serde(default)]
    ru: Option<u64>,
    #[serde(default)]
    eids: Vec<KvEid>,
}

impl From<KvPartnerIdWire> for KvPartnerId {
    fn from(wire: KvPartnerIdWire) -> Self {
        let eids = match (wire.eids.is_empty(), wire.uid) {
            (true, Some(uid)) => vec![KvEid::one_uid(uid)],
            (_, _) => wire.eids,
        };
        Self {
            w: wire.w,
            rv: wire.rv,
            at: wire.at,
            exp: wire.exp,
            ra: wire.ra,
            ru: wire.ru,
            eids,
        }
    }
}

impl KvPartnerId {
    /// A record holding one entry with one UID, and no metadata.
    #[must_use]
    pub fn new(uid: impl Into<String>) -> Self {
        Self {
            eids: vec![KvEid::one_uid(uid.into())],
            ..Self::default()
        }
    }

    /// A browser-ingested record: no writer, with its acquisition time.
    #[must_use]
    pub fn browser(uid: impl Into<String>, acquired_at: Option<u64>) -> Self {
        Self {
            at: acquired_at,
            ..Self::new(uid)
        }
    }

    /// The first UID of the first entry, which `/identify` returns and
    /// today's one-UID readers use.
    #[must_use]
    pub fn first_uid(&self) -> Option<&str> {
        self.eids
            .first()
            .and_then(|entry| entry.uids.first())
            .map(|uid| uid.id.as_str())
    }
}

/// One EID entry of a record (spec 6.1).
///
/// The `OpenRTB` EID object without `source`, which the `ids` key holds.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct KvEid {
    /// The entry's UIDs, in order. Never empty in a valid record.
    pub uids: Vec<KvUid>,
    /// `eid.inserter`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inserter: Option<String>,
    /// `eid.matcher`. Absent means `source`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matcher: Option<String>,
    /// `eid.mm`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mm: Option<i32>,
    /// EID-level `ext`, at most [`MAX_PARTNER_EXT_BYTES`] serialized bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ext: Option<serde_json::Value>,
}

impl KvEid {
    /// One entry holding one UID and nothing else: the shape browser
    /// ingest, generic pull and batch sync write.
    fn one_uid(id: String) -> Self {
        Self {
            uids: vec![KvUid {
                id,
                atype: None,
                ext: None,
            }],
            ..Self::default()
        }
    }
}

/// One UID of a stored entry.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct KvUid {
    /// The user identifier.
    pub id: String,
    /// `uid.atype`. Absent: use the partner's `openrtb_atype`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub atype: Option<i32>,
    /// UID-level `ext`, at most [`MAX_PARTNER_EXT_BYTES`] serialized bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ext: Option<serde_json::Value>,
}

/// A provider's sticky opt-out (spec 8).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct KvOptOut {
    /// Unix seconds when the opt-out was reported.
    pub at: u64,
    /// The consent bits the provider's policy read then, as a lowercase hex
    /// bitmask of at most 16 digits. Plain, not keyed (spec 8).
    pub fp: String,
}

/// Per-provider state in a KV row (`ps.<provider>`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct KvProviderState {
    /// Unix seconds before which the provider must not be called.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub n: Option<u64>,
    /// Consecutive failures, for backoff.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub f: u32,
    /// Sticky opt-out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub o: Option<KvOptOut>,
    /// When the provider's opt-out was last cleared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oc: Option<u64>,
    /// Unkeyed hashes of UIDs the owner invalidated, oldest first, at most
    /// [`MAX_INVALIDATED_UIDS`]. Never emptied (spec 5.4).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub iv: Vec<String>,
    /// Unkeyed hashes of UIDs the provider's opt-out removed, oldest first,
    /// at most [`MAX_INVALIDATED_UIDS`]. Emptied when the opt-out clears.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ov: Vec<String>,
    /// The provider's private state as embedded JSON. KV only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub st: Option<serde_json::Value>,
}

fn is_zero(value: &u32) -> bool {
    *value == 0
}

/// Bytes of a stored entry set as compact JSON: the array of entries without
/// `source`, as `eids` stores it. Both `validate` and the coordinator measure
/// [`MAX_ENTRY_SET_BYTES`] with this function.
pub(crate) fn entry_set_bytes(set: &[KvEid]) -> usize {
    serde_json::to_vec(set).map_or(usize::MAX, |bytes| bytes.len())
}

/// Whether `value` is non-empty lowercase hex of at most `max_len` digits.
///
/// Shared by the `rv` and `ps.o.fp` checks.
pub(crate) fn is_lowercase_hex(value: &str, max_len: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_len
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Whether an `inserter` or `matcher` is stored-valid: not blank and at most
/// [`MAX_EID_FIELD_BYTES`] bytes.
pub(crate) fn is_valid_eid_field(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= MAX_EID_FIELD_BYTES
}

/// Whether a UID-level or EID-level `ext` is stored-valid: a JSON object of at
/// most [`MAX_PARTNER_EXT_BYTES`] bytes as compact JSON.
pub(crate) fn is_valid_partner_ext(ext: &serde_json::Value) -> bool {
    ext.is_object()
        && serde_json::to_vec(ext).is_ok_and(|bytes| bytes.len() <= MAX_PARTNER_EXT_BYTES)
}

/// Publisher property metadata captured when an EC entry is created.
///
/// Earlier schema versions treated `seen_domains` as mutable domain history.
/// To avoid recurring organic-request KV writes, new entries now seed only the
/// creation domain and runtime requests do not append domains. Legacy map-shaped
/// records are accepted on read and reserialized as a domain list on future writes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KvPubProperties {
    /// Apex domain where this EC entry was first created.
    pub origin_domain: String,
    /// Bounded set of publisher apex domains seen for this EC entry.
    ///
    /// New entries include the creation domain only; runtime requests do not
    /// update this set.
    #[serde(
        default,
        deserialize_with = "deserialize_seen_domains",
        skip_serializing_if = "BTreeSet::is_empty"
    )]
    pub seen_domains: BTreeSet<String>,
}

fn deserialize_seen_domains<'de, D>(deserializer: D) -> Result<BTreeSet<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum SeenDomains {
        List(Vec<String>),
        LegacyMap(BTreeMap<String, serde_json::Value>),
    }

    match SeenDomains::deserialize(deserializer)? {
        SeenDomains::List(domains) => Ok(domains.into_iter().collect()),
        SeenDomains::LegacyMap(domains) => Ok(domains.into_keys().collect()),
    }
}

/// Coarse device signals derived from TLS handshake and UA.
///
/// Used by the `/_ts/api/v1/identify` endpoint for cross-suffix propagation decisions
/// and buyer-facing device quality scoring. Written once on
/// [`KvEntry`] creation — never updated after.
///
/// **Privacy:** `ja4_class` (Section 1 only) and `platform_class` are
/// category signals, not unique device identifiers. The full JA4
/// string (Sections 2–3) is never stored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KvDevice {
    /// Mobile signal: `0` = confirmed desktop, `1` = confirmed mobile,
    /// `2` = genuinely unknown (non-standard client).
    /// Derived from UA platform string — no Client Hints required.
    pub is_mobile: u8,
    /// JA4 Section 1 only — browser family class identifier.
    /// e.g. `"t13d1516h2"` = Chrome, `"t13d2013h2"` = Safari.
    /// Never stores the full JA4 string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ja4_class: Option<String>,
    /// Coarse OS family from UA: `"mac"`, `"windows"`, `"ios"`,
    /// `"android"`, `"linux"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform_class: Option<String>,
    /// SHA256 prefix (12 hex chars) of the HTTP/2 SETTINGS string.
    /// Used alongside `ja4_class` for browser confirmation and bot detection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub h2_fp_hash: Option<String>,
    /// `true` = known legitimate browser; `false` = known bot/scraper;
    /// `None` = unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub known_browser: Option<bool>,
}

/// Network cluster disambiguation data.
///
/// Tracks how many distinct EC entries share the same hash prefix. A high
/// count indicates a shared network (corporate VPN, campus); a low count
/// indicates an individual or household.
///
/// Written only by the `/_ts/api/v1/identify` endpoint when `cluster_size` is
/// missing. Once stored, the value is reused because the EC entry no longer
/// stores a cluster-check timestamp.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KvNetwork {
    /// Number of distinct EC suffixes matching this hash prefix.
    /// `None` = not yet evaluated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cluster_size: Option<u32>,
}

/// Compact metadata stored alongside the KV entry body.
///
/// Fastly KV metadata is limited to 2048 bytes and can be read without
/// streaming the full body. Used by batch sync for fast consent checks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KvMetadata {
    /// Mirrors [`KvConsent::ok`] — `false` means tombstone.
    pub ok: bool,
    /// Mirrors [`KvGeo::country`].
    pub country: String,
    /// Mirrors [`KvEntry::v`].
    pub v: u8,
    /// Mirrors [`KvNetwork::cluster_size`]. `None` = not yet evaluated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cluster_size: Option<u32>,
    /// Mirrors [`KvDevice::is_mobile`]. Enables propagation gating without
    /// body read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_mobile: Option<u8>,
    /// Mirrors [`KvDevice::known_browser`]. Buyer-facing quality signal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub known_browser: Option<bool>,
}

/// Validates a publisher domain before storing it in KV.
///
/// Applies a lightweight hostname-shape check and lowercases ASCII labels so
/// stored values remain bounded and consistent.
#[must_use]
pub(crate) fn validated_stored_domain(domain: &str) -> Option<String> {
    if domain.is_empty() || domain.len() > MAX_STORED_DOMAIN_LENGTH {
        return None;
    }
    if !domain.is_ascii() {
        return None;
    }

    let normalized = domain.trim_end_matches('.').to_ascii_lowercase();
    if normalized.is_empty() || normalized.len() > MAX_STORED_DOMAIN_LENGTH {
        return None;
    }

    let mut labels = normalized.split('.');
    let mut saw_label = false;
    for label in &mut labels {
        saw_label = true;
        if label.is_empty() || label.len() > 63 {
            return None;
        }

        let bytes = label.as_bytes();
        let first = bytes.first().copied().expect("should have non-empty label");
        let last = bytes.last().copied().expect("should have non-empty label");
        if !first.is_ascii_alphanumeric() || !last.is_ascii_alphanumeric() {
            return None;
        }
        if !bytes
            .iter()
            .copied()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return None;
        }
    }

    saw_label.then_some(normalized)
}

impl KvEntry {
    /// Creates a new live entry from the current request context.
    ///
    /// `domain` is the publisher's apex domain (e.g. `"example.com"`),
    /// used to initialize the [`KvPubProperties`] origin and first visit.
    #[must_use]
    pub fn new(consent: &ConsentContext, geo: Option<&GeoInfo>, now: u64, domain: &str) -> Self {
        let pub_properties = validated_stored_domain(domain).map(|validated_domain| {
            let mut seen_domains = BTreeSet::new();
            seen_domains.insert(validated_domain.clone());

            KvPubProperties {
                origin_domain: validated_domain,
                seen_domains,
            }
        });

        Self {
            v: SCHEMA_VERSION,
            created: now,
            consent: KvConsent {
                tcf: consent.raw_tc_string.clone(),
                gpp: consent.raw_gpp_string.clone(),
                ok: true,
                updated: now,
            },
            geo: KvGeo::from_geo_info(geo),
            pub_properties,
            device: None,
            network: None,
            ids: BTreeMap::new(),
            ps: BTreeMap::new(),
            pg: None,
        }
    }

    /// Creates a minimal live entry.
    ///
    /// Used by tests and fixtures that need a valid live entry with one
    /// partner UID and placeholder metadata.
    #[must_use]
    pub fn minimal(partner_id: &str, uid: &str, now: u64) -> Self {
        let mut ids = BTreeMap::new();
        ids.insert(partner_id.to_owned(), KvPartnerId::new(uid));
        Self {
            v: SCHEMA_VERSION,
            created: now,
            consent: KvConsent {
                tcf: None,
                gpp: None,
                ok: true,
                updated: now,
            },
            geo: KvGeo {
                country: "ZZ".to_owned(),
                region: None,
                asn: None,
                dma: None,
            },
            pub_properties: None,
            device: None,
            network: None,
            ids,
            ps: BTreeMap::new(),
            pg: None,
        }
    }

    /// Creates a withdrawal tombstone entry.
    ///
    /// Sets `consent.ok = false`, clears all partner IDs, and uses a
    /// placeholder geo. The caller should apply a 24-hour TTL when writing.
    ///
    /// **Note:** The original `created` timestamp is intentionally not
    /// preserved — reading the existing entry first would add latency on
    /// the consent-withdrawal hot path, and the tombstone expires in 24h.
    #[must_use]
    pub fn tombstone(now: u64) -> Self {
        Self {
            v: SCHEMA_VERSION,
            created: now,
            consent: KvConsent {
                tcf: None,
                gpp: None,
                ok: false,
                updated: now,
            },
            geo: KvGeo {
                country: "ZZ".to_owned(),
                region: None,
                asn: None,
                dma: None,
            },
            pub_properties: None,
            device: None,
            network: None,
            ids: BTreeMap::new(),
            ps: BTreeMap::new(),
            pg: None,
        }
    }

    /// Validates a deserialized entry loaded from KV.
    ///
    /// Rejects legacy or corrupt records that exceed the current bounded
    /// storage contract rather than re-serializing them at unbounded size.
    ///
    /// # Errors
    ///
    /// Returns an error string describing the first bounds or shape violation
    /// found in the deserialized record.
    pub fn validate(&self) -> Result<(), String> {
        if self.v != SCHEMA_VERSION {
            return Err(format!(
                "unsupported KV entry schema version {} (expected {})",
                self.v, SCHEMA_VERSION
            ));
        }

        for (partner_id, record) in &self.ids {
            validate_record(partner_id, record)?;
        }

        if self.ps.len() > MAX_PROVIDER_STATES {
            return Err(format!(
                "ps exceeds MAX_PROVIDER_STATES ({})",
                self.ps.len()
            ));
        }
        for (provider, state) in &self.ps {
            for (name, hashes) in [("iv", &state.iv), ("ov", &state.ov)] {
                if hashes.len() > MAX_INVALIDATED_UIDS {
                    return Err(format!(
                        "ps '{provider}' {name} exceeds {MAX_INVALIDATED_UIDS} hashes"
                    ));
                }
            }
            if let Some(opt_out) = &state.o
                && !is_lowercase_hex(&opt_out.fp, OPT_OUT_FINGERPRINT_MAX_HEX_LEN)
            {
                return Err(format!("ps '{provider}' o.fp is not a hex bitmask"));
            }
            if let Some(st) = &state.st
                && serde_json::to_vec(st)
                    .map_or(true, |bytes| bytes.len() > MAX_PROVIDER_STATE_BYTES)
            {
                return Err(format!(
                    "ps '{provider}' st exceeds {MAX_PROVIDER_STATE_BYTES} bytes"
                ));
            }
        }

        if let Some(pub_properties) = &self.pub_properties {
            if validated_stored_domain(&pub_properties.origin_domain).as_deref()
                != Some(pub_properties.origin_domain.as_str())
            {
                return Err(format!(
                    "origin_domain '{}' is invalid for stored KV data",
                    pub_properties.origin_domain
                ));
            }

            if pub_properties.seen_domains.len() > MAX_SEEN_DOMAINS {
                return Err(format!(
                    "seen_domains exceeds MAX_SEEN_DOMAINS ({})",
                    pub_properties.seen_domains.len()
                ));
            }

            for domain in &pub_properties.seen_domains {
                if validated_stored_domain(domain).as_deref() != Some(domain.as_str()) {
                    return Err(format!(
                        "seen_domains contains invalid stored domain '{domain}'"
                    ));
                }
            }
        }

        Ok(())
    }

    /// Brings a row read from KV to the current schema (spec 6.1).
    ///
    /// Accepts schema versions 1 and 2 and sets the version to 2. Drops each
    /// record that breaks a rule of spec 4.5 or 6.1, and returns one message
    /// per dropped record, naming the source and the rule, never a UID.
    /// Called on read only, before [`Self::validate`], so one bad record
    /// never makes the row unreadable.
    ///
    /// # Errors
    ///
    /// Returns an error naming the version when the row's version is
    /// neither 1 nor 2.
    pub(crate) fn upgrade_for_read(&mut self) -> Result<Vec<String>, String> {
        if !(1..=SCHEMA_VERSION).contains(&self.v) {
            return Err(format!(
                "unsupported KV entry schema version {} (expected 1 or {})",
                self.v, SCHEMA_VERSION
            ));
        }
        self.v = SCHEMA_VERSION;
        let mut messages = Vec::new();
        self.ids.retain(
            |partner_id, record| match validate_record(partner_id, record) {
                Ok(()) => true,
                Err(message) => {
                    messages.push(message);
                    false
                }
            },
        );
        Ok(messages)
    }
}

/// Checks one record against the limits of spec 4.5 and 6.1.
///
/// Messages name the partner ID and the rule and never a UID, so they are safe
/// to log.
fn validate_record(partner_id: &str, record: &KvPartnerId) -> Result<(), String> {
    if record.eids.is_empty() {
        return Err(format!("partner ID '{partner_id}' has an empty eids"));
    }
    if let Some(rv) = &record.rv
        && (rv.len() != RECORD_REVISION_HEX_LEN || !is_lowercase_hex(rv, RECORD_REVISION_HEX_LEN))
    {
        return Err(format!(
            "partner ID '{partner_id}' rv is not {RECORD_REVISION_HEX_LEN} hex digits"
        ));
    }
    if record.eids.len() > MAX_ENTRIES_PER_SOURCE {
        return Err(format!(
            "partner ID '{partner_id}' has more than {MAX_ENTRIES_PER_SOURCE} entries"
        ));
    }
    let mut uid_count = 0;
    for entry in &record.eids {
        if entry.uids.is_empty() {
            return Err(format!(
                "partner ID '{partner_id}' has an entry without a uid"
            ));
        }
        for (name, value) in [("inserter", &entry.inserter), ("matcher", &entry.matcher)] {
            if value
                .as_deref()
                .is_some_and(|value| !is_valid_eid_field(value))
            {
                return Err(format!(
                    "partner ID '{partner_id}' {name} is blank or over {MAX_EID_FIELD_BYTES} bytes"
                ));
            }
        }
        if entry.mm.is_some_and(|mm| mm < 0) {
            return Err(format!("partner ID '{partner_id}' mm is negative"));
        }
        validate_ext(partner_id, entry.ext.as_ref())?;
        for uid in &entry.uids {
            if uid.id.trim().is_empty() || uid.id.len() > MAX_UID_LENGTH {
                return Err(format!(
                    "partner ID '{partner_id}' has a blank uid or one over MAX_UID_LENGTH ({MAX_UID_LENGTH})"
                ));
            }
            if uid.atype.is_some_and(|atype| atype < 0) {
                return Err(format!("partner ID '{partner_id}' atype is negative"));
            }
            validate_ext(partner_id, uid.ext.as_ref())?;
        }
        uid_count += entry.uids.len();
    }
    if uid_count > MAX_UIDS_PER_SOURCE {
        return Err(format!(
            "partner ID '{partner_id}' has more than {MAX_UIDS_PER_SOURCE} uids"
        ));
    }
    if entry_set_bytes(&record.eids) > MAX_ENTRY_SET_BYTES {
        return Err(format!(
            "partner ID '{partner_id}' entry set exceeds {MAX_ENTRY_SET_BYTES} bytes"
        ));
    }
    Ok(())
}

/// Checks an optional UID-level or EID-level `ext` with
/// [`is_valid_partner_ext`].
fn validate_ext(partner_id: &str, ext: Option<&serde_json::Value>) -> Result<(), String> {
    if ext.is_some_and(|ext| !is_valid_partner_ext(ext)) {
        return Err(format!(
            "partner ID '{partner_id}' ext is not an object of at most {MAX_PARTNER_EXT_BYTES} bytes"
        ));
    }
    Ok(())
}

impl KvMetadata {
    /// Extracts metadata from a full entry.
    #[must_use]
    pub fn from_entry(entry: &KvEntry) -> Self {
        Self {
            ok: entry.consent.ok,
            country: entry.geo.country.clone(),
            v: entry.v,
            cluster_size: entry.network.as_ref().and_then(|n| n.cluster_size),
            is_mobile: entry.device.as_ref().map(|d| d.is_mobile),
            known_browser: entry.device.as_ref().and_then(|d| d.known_browser),
        }
    }
}

impl KvGeo {
    /// Creates a `KvGeo` from an optional [`GeoInfo`].
    ///
    /// Returns `country: "ZZ"` (unknown) when geo data is unavailable.
    #[must_use]
    pub fn from_geo_info(geo: Option<&GeoInfo>) -> Self {
        match geo {
            Some(info) => {
                let dma = (info.metro_code > 0).then_some(info.metro_code);
                Self {
                    country: info.country.clone(),
                    region: info.region.clone(),
                    asn: info.asn,
                    dma,
                }
            }
            None => Self {
                country: "ZZ".to_owned(),
                region: None,
                asn: None,
                dma: None,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample_consent_context() -> ConsentContext {
        ConsentContext {
            raw_tc_string: Some("CP_test_tc_string".to_owned()),
            raw_gpp_string: Some("DBA_test_gpp".to_owned()),
            ..ConsentContext::default()
        }
    }

    fn sample_geo_info() -> GeoInfo {
        GeoInfo {
            city: "San Francisco".to_owned(),
            country: "US".to_owned(),
            continent: "NorthAmerica".to_owned(),
            latitude: 37.7749,
            longitude: -122.4194,
            metro_code: 807,
            region: Some("CA".to_owned()),
            asn: Some(7922),
        }
    }

    #[test]
    fn entry_serialization_roundtrip() {
        let geo = sample_geo_info();
        let consent = sample_consent_context();
        let mut entry = KvEntry::new(&consent, Some(&geo), 1_741_824_000, "example.com");
        entry
            .ids
            .insert("liveramp".to_owned(), KvPartnerId::new("LR_xyz"));

        let json = serde_json::to_string(&entry).expect("should serialize KvEntry");
        let deserialized: KvEntry =
            serde_json::from_str(&json).expect("should deserialize KvEntry");

        assert_eq!(deserialized.v, SCHEMA_VERSION);
        assert_eq!(deserialized.created, 1_741_824_000);
        assert_eq!(
            deserialized.consent.tcf.as_deref(),
            Some("CP_test_tc_string")
        );
        assert_eq!(deserialized.consent.gpp.as_deref(), Some("DBA_test_gpp"));
        assert!(deserialized.consent.ok, "should be a live entry");
        assert_eq!(deserialized.geo.country, "US");
        assert_eq!(deserialized.geo.region.as_deref(), Some("CA"));
        assert_eq!(deserialized.geo.asn, Some(7922));
        assert_eq!(deserialized.geo.dma, Some(807));
        assert_eq!(
            deserialized.ids.get("liveramp").and_then(|p| p.first_uid()),
            Some("LR_xyz"),
        );
        assert!(
            !json.contains("last_seen")
                && !json.contains("synced")
                && !json.contains("first")
                && !json.contains("last")
                && !json.contains("cluster_checked")
                && !json.contains("visits"),
            "serialized entry should omit removed timestamp and visit fields: {json}"
        );
    }

    #[test]
    fn legacy_timestamp_fields_deserialize_and_are_omitted_on_reserialize() {
        let json = r#"{
            "v": 1,
            "created": 1000,
            "last_seen": 1200,
            "consent": { "ok": true, "updated": 1000 },
            "geo": { "country": "US" },
            "ids": {
                "liveramp": { "uid": "abc", "synced": 1100 }
            },
            "pub_properties": {
                "origin_domain": "example.com",
                "seen_domains": {
                    "example.com": { "first": 1000, "last": 1200, "visits": 3 }
                }
            },
            "network": {
                "cluster_size": 5,
                "cluster_checked": 1200
            }
        }"#;

        let entry: KvEntry = serde_json::from_str(json)
            .expect("should deserialize legacy entry with removed timestamp fields");

        assert_eq!(entry.created, 1000);
        assert_eq!(entry.consent.updated, 1000);
        assert_eq!(
            entry
                .ids
                .get("liveramp")
                .and_then(|partner| partner.first_uid()),
            Some("abc")
        );
        assert!(
            entry
                .pub_properties
                .as_ref()
                .is_some_and(|props| props.seen_domains.contains("example.com")),
            "should preserve legacy seen domain key"
        );
        assert_eq!(
            entry
                .network
                .as_ref()
                .and_then(|network| network.cluster_size),
            Some(5)
        );

        let reserialized = serde_json::to_string(&entry).expect("should reserialize entry");
        assert!(
            !reserialized.contains("last_seen")
                && !reserialized.contains("synced")
                && !reserialized.contains("first")
                && !reserialized.contains("last")
                && !reserialized.contains("cluster_checked")
                && !reserialized.contains("visits"),
            "reserialized entry should omit removed timestamp and visit fields: {reserialized}"
        );
    }

    #[test]
    fn metadata_serialization_roundtrip() {
        let meta = KvMetadata {
            ok: true,
            country: "US".to_owned(),
            v: 1,
            cluster_size: None,
            is_mobile: None,
            known_browser: None,
        };

        let json = serde_json::to_string(&meta).expect("should serialize KvMetadata");
        let deserialized: KvMetadata =
            serde_json::from_str(&json).expect("should deserialize KvMetadata");

        assert!(deserialized.ok, "should be ok=true");
        assert_eq!(deserialized.country, "US");
        assert_eq!(deserialized.v, 1);
        assert!(deserialized.cluster_size.is_none());
    }

    #[test]
    fn metadata_with_cluster_size_roundtrip() {
        let meta = KvMetadata {
            ok: true,
            country: "US".to_owned(),
            v: 1,
            cluster_size: Some(3),
            is_mobile: None,
            known_browser: None,
        };

        let json = serde_json::to_string(&meta).expect("should serialize KvMetadata");
        let deserialized: KvMetadata =
            serde_json::from_str(&json).expect("should deserialize KvMetadata");

        assert_eq!(deserialized.cluster_size, Some(3));
    }

    #[test]
    fn metadata_without_cluster_size_deserializes() {
        // Simulates metadata stored before cluster_size was added.
        let json = r#"{"ok":true,"country":"US","v":1}"#;
        let meta: KvMetadata = serde_json::from_str(json).expect("should deserialize old metadata");

        assert!(meta.cluster_size.is_none(), "should default to None");
    }

    #[test]
    fn metadata_fits_in_2048_bytes() {
        // Worst case: all fields populated.
        let meta = KvMetadata {
            ok: false,
            country: "XX".to_owned(),
            v: SCHEMA_VERSION,
            cluster_size: Some(u32::MAX),
            is_mobile: Some(2),
            known_browser: Some(true),
        };
        let json = serde_json::to_string(&meta).expect("should serialize KvMetadata");
        assert!(
            json.len() <= 2048,
            "metadata must fit in Fastly's 2048-byte limit, got {} bytes",
            json.len()
        );
    }

    #[test]
    fn new_entry_has_correct_initial_state() {
        let consent = sample_consent_context();
        let geo = sample_geo_info();
        let entry = KvEntry::new(&consent, Some(&geo), 1000, "example.com");

        assert_eq!(entry.v, SCHEMA_VERSION);
        assert_eq!(entry.created, 1000);
        assert!(entry.consent.ok, "should be a live entry");
        assert_eq!(entry.consent.updated, 1000);
        assert_eq!(entry.geo.country, "US");
        assert!(entry.ids.is_empty(), "should have no partner IDs initially");

        let props = entry
            .pub_properties
            .as_ref()
            .expect("should have pub_properties");
        assert_eq!(props.origin_domain, "example.com");
        assert_eq!(props.seen_domains.len(), 1);
        assert!(
            props.seen_domains.contains("example.com"),
            "should have origin domain"
        );
    }

    #[test]
    fn new_entry_without_geo_uses_zz() {
        let consent = ConsentContext::default();
        let entry = KvEntry::new(&consent, None, 1000, "example.com");
        assert_eq!(
            entry.geo.country, "ZZ",
            "should use ZZ when geo is unavailable"
        );
        assert!(entry.geo.region.is_none());
        assert!(entry.geo.asn.is_none());
        assert!(entry.geo.dma.is_none());
    }

    #[test]
    fn validated_stored_domain_accepts_and_normalizes_ascii_hostnames() {
        assert_eq!(
            validated_stored_domain("Example.COM."),
            Some("example.com".to_owned()),
            "should lowercase and trim a trailing dot"
        );
    }

    #[test]
    fn validated_stored_domain_rejects_invalid_shapes() {
        assert!(
            validated_stored_domain("bad_domain").is_none(),
            "underscores should be rejected"
        );
        assert!(
            validated_stored_domain("-example.com").is_none(),
            "labels should not start with hyphens"
        );
        assert!(
            validated_stored_domain(&"a".repeat(MAX_STORED_DOMAIN_LENGTH + 1)).is_none(),
            "overlong hostnames should be rejected"
        );
    }

    #[test]
    fn new_entry_skips_pub_properties_for_invalid_domain() {
        let consent = sample_consent_context();
        let geo = sample_geo_info();
        let entry = KvEntry::new(&consent, Some(&geo), 1000, "bad_domain");

        assert!(
            entry.pub_properties.is_none(),
            "invalid stored domains should not be persisted into pub_properties"
        );
    }

    #[test]
    fn validate_rejects_oversized_partner_uid() {
        let mut entry = KvEntry::tombstone(1000);
        entry.ids.insert(
            "ssp_x".to_owned(),
            KvPartnerId::new("x".repeat(MAX_UID_LENGTH + 1)),
        );

        let err = entry
            .validate()
            .expect_err("should reject oversized partner UIDs");
        assert!(
            err.contains("MAX_UID_LENGTH"),
            "should describe the UID length validation failure"
        );
    }

    #[test]
    fn validate_rejects_unexpected_schema_version() {
        let mut entry = KvEntry::tombstone(1000);
        entry.v = SCHEMA_VERSION + 1;

        let err = entry
            .validate()
            .expect_err("should reject unsupported schema versions");
        assert!(
            err.contains("unsupported KV entry schema version"),
            "should describe the schema version validation failure"
        );
    }

    #[test]
    fn validate_rejects_seen_domains_over_cap() {
        let consent = sample_consent_context();
        let geo = sample_geo_info();
        let mut entry = KvEntry::new(&consent, Some(&geo), 1000, "example.com");
        let pub_properties = entry
            .pub_properties
            .as_mut()
            .expect("should initialize pub_properties");

        for idx in 0..MAX_SEEN_DOMAINS {
            pub_properties
                .seen_domains
                .insert(format!("extra-{idx}.example.com"));
        }

        let err = entry
            .validate()
            .expect_err("should reject entries with too many seen domains");
        assert!(
            err.contains("MAX_SEEN_DOMAINS"),
            "should describe the seen_domains bound violation"
        );
    }

    #[test]
    fn seen_domains_serialize_in_deterministic_key_order() {
        let consent = sample_consent_context();
        let geo = sample_geo_info();
        let mut entry = KvEntry::new(&consent, Some(&geo), 1000, "example.com");
        let pub_properties = entry
            .pub_properties
            .as_mut()
            .expect("should initialize pub_properties");

        pub_properties
            .seen_domains
            .insert("z.example.com".to_owned());
        pub_properties
            .seen_domains
            .insert("a.example.com".to_owned());

        let json = serde_json::to_string(&entry).expect("should serialize KV entry");
        let a_index = json
            .find("a.example.com")
            .expect("should contain a.example.com");
        let z_index = json
            .find("z.example.com")
            .expect("should contain z.example.com");
        assert!(
            a_index < z_index,
            "should serialize seen_domains in lexical key order"
        );
    }

    #[test]
    fn validate_rejects_invalid_stored_domain_shapes() {
        let consent = sample_consent_context();
        let geo = sample_geo_info();
        let mut entry = KvEntry::new(&consent, Some(&geo), 1000, "example.com");
        let pub_properties = entry
            .pub_properties
            .as_mut()
            .expect("should initialize pub_properties");
        pub_properties.origin_domain = "bad_domain".to_owned();

        let err = entry
            .validate()
            .expect_err("should reject invalid stored origin domains");
        assert!(
            err.contains("origin_domain"),
            "should report the invalid stored origin_domain"
        );
    }

    #[test]
    fn minimal_entry_has_partner_id_and_placeholder_geo() {
        let entry = KvEntry::minimal("ssp_x", "abc123", 1_741_824_000);

        assert_eq!(entry.v, SCHEMA_VERSION);
        assert!(entry.consent.ok, "should be a live entry");
        assert_eq!(entry.geo.country, "ZZ");
        assert!(
            entry.pub_properties.is_none(),
            "minimal entry should have no pub_properties"
        );
        assert_eq!(entry.ids.len(), 1);
        let partner = entry.ids.get("ssp_x").expect("should have ssp_x entry");
        assert_eq!(partner.first_uid(), Some("abc123"));
    }

    #[test]
    fn tombstone_entry_has_correct_shape() {
        let entry = KvEntry::tombstone(1_741_910_400);

        assert_eq!(entry.v, SCHEMA_VERSION);
        assert!(!entry.consent.ok, "should be a tombstone");
        assert!(entry.ids.is_empty(), "tombstone should have no partner IDs");
        assert_eq!(entry.geo.country, "ZZ");
        assert_eq!(entry.consent.updated, 1_741_910_400);
        assert!(
            entry.pub_properties.is_none(),
            "tombstone should have no pub_properties"
        );
    }

    #[test]
    fn metadata_from_entry_mirrors_fields() {
        let consent = sample_consent_context();
        let geo = sample_geo_info();
        let entry = KvEntry::new(&consent, Some(&geo), 1000, "example.com");
        let meta = KvMetadata::from_entry(&entry);

        assert_eq!(meta.ok, entry.consent.ok);
        assert_eq!(meta.country, entry.geo.country);
        assert_eq!(meta.v, entry.v);
    }

    #[test]
    fn tombstone_metadata_has_ok_false() {
        let entry = KvEntry::tombstone(1000);
        let meta = KvMetadata::from_entry(&entry);

        assert!(!meta.ok, "tombstone metadata should have ok=false");
    }

    #[test]
    fn empty_ids_omitted_from_json() {
        let entry = KvEntry::tombstone(1000);
        let json = serde_json::to_string(&entry).expect("should serialize");
        assert!(
            !json.contains("\"ids\""),
            "empty ids should be omitted from JSON, got: {json}"
        );
    }

    #[test]
    fn none_consent_fields_omitted_from_json() {
        let entry = KvEntry::tombstone(1000);
        let json = serde_json::to_string(&entry).expect("should serialize");
        assert!(
            !json.contains("\"tcf\""),
            "None tcf should be omitted from JSON"
        );
        assert!(
            !json.contains("\"gpp\""),
            "None gpp should be omitted from JSON"
        );
    }

    #[test]
    fn none_geo_fields_omitted_from_json() {
        let entry = KvEntry::tombstone(1000);
        let json = serde_json::to_string(&entry).expect("should serialize");
        assert!(
            !json.contains("\"asn\""),
            "None asn should be omitted from JSON"
        );
        assert!(
            !json.contains("\"dma\""),
            "None dma should be omitted from JSON"
        );
    }

    #[test]
    fn geo_with_asn_and_dma_roundtrips() {
        let geo = KvGeo {
            country: "US".to_owned(),
            region: Some("CA".to_owned()),
            asn: Some(7922),
            dma: Some(807),
        };
        let json = serde_json::to_string(&geo).expect("should serialize KvGeo");
        let deserialized: KvGeo = serde_json::from_str(&json).expect("should deserialize KvGeo");

        assert_eq!(deserialized.asn, Some(7922));
        assert_eq!(deserialized.dma, Some(807));
    }

    #[test]
    fn geo_without_asn_deserializes_from_v1_json() {
        // Simulates a KvGeo stored before asn/dma fields were added.
        let v1_json = r#"{"country":"US","region":"CA"}"#;
        let geo: KvGeo = serde_json::from_str(v1_json).expect("should deserialize v1 KvGeo");

        assert_eq!(geo.country, "US");
        assert_eq!(geo.region.as_deref(), Some("CA"));
        assert!(geo.asn.is_none(), "asn should default to None");
        assert!(geo.dma.is_none(), "dma should default to None");
    }

    #[test]
    fn pub_properties_roundtrip() {
        let consent = sample_consent_context();
        let geo = sample_geo_info();
        let entry = KvEntry::new(&consent, Some(&geo), 1000, "example.com");

        let json = serde_json::to_string(&entry).expect("should serialize");
        let deserialized: KvEntry = serde_json::from_str(&json).expect("should deserialize");

        let props = deserialized
            .pub_properties
            .expect("should have pub_properties");
        assert_eq!(props.origin_domain, "example.com");
        assert_eq!(props.seen_domains.len(), 1);
        assert!(
            props.seen_domains.contains("example.com"),
            "should have origin domain"
        );
    }

    #[test]
    fn none_pub_properties_omitted_from_json() {
        let entry = KvEntry::tombstone(1000);
        let json = serde_json::to_string(&entry).expect("should serialize");
        assert!(
            !json.contains("\"pub_properties\""),
            "None pub_properties should be omitted from JSON, got: {json}"
        );
    }

    #[test]
    fn entry_without_pub_properties_deserializes() {
        // Simulates an entry stored before pub_properties was added.
        let json = r#"{
            "v": 1,
            "created": 1000,
            "last_seen": 1000,
            "consent": { "ok": true, "updated": 1000 },
            "geo": { "country": "US" }
        }"#;
        let entry: KvEntry =
            serde_json::from_str(json).expect("should deserialize entry without pub_properties");

        assert!(
            entry.pub_properties.is_none(),
            "missing pub_properties should deserialize as None"
        );
    }

    #[test]
    fn pub_properties_deserializes_new_seen_domains_list_shape() {
        let json = r#"{
            "origin_domain": "example.com",
            "seen_domains": ["example.com"]
        }"#;

        let props: KvPubProperties =
            serde_json::from_str(json).expect("should deserialize new seen_domains list shape");

        assert_eq!(props.origin_domain, "example.com");
        assert_eq!(props.seen_domains.len(), 1);
        assert!(
            props.seen_domains.contains("example.com"),
            "should include listed domain"
        );
    }

    #[test]
    fn network_roundtrip() {
        let network = KvNetwork {
            cluster_size: Some(3),
        };
        let json = serde_json::to_string(&network).expect("should serialize KvNetwork");
        let deserialized: KvNetwork =
            serde_json::from_str(&json).expect("should deserialize KvNetwork");

        assert_eq!(deserialized.cluster_size, Some(3));
    }

    #[test]
    fn network_none_fields_omitted_from_json() {
        let network = KvNetwork { cluster_size: None };
        let json = serde_json::to_string(&network).expect("should serialize");
        assert!(
            !json.contains("\"cluster_size\""),
            "None cluster_size should be omitted, got: {json}"
        );
    }

    #[test]
    fn none_network_omitted_from_entry_json() {
        let entry = KvEntry::tombstone(1000);
        let json = serde_json::to_string(&entry).expect("should serialize");
        assert!(
            !json.contains("\"network\""),
            "None network should be omitted from JSON, got: {json}"
        );
    }

    #[test]
    fn entry_without_network_deserializes() {
        // Simulates an entry stored before network was added.
        let json = r#"{
            "v": 1,
            "created": 1000,
            "last_seen": 1000,
            "consent": { "ok": true, "updated": 1000 },
            "geo": { "country": "US" }
        }"#;
        let entry: KvEntry =
            serde_json::from_str(json).expect("should deserialize entry without network");

        assert!(
            entry.network.is_none(),
            "missing network should deserialize as None"
        );
    }

    #[test]
    fn metadata_from_entry_mirrors_cluster_size() {
        let consent = sample_consent_context();
        let geo = sample_geo_info();
        let mut entry = KvEntry::new(&consent, Some(&geo), 1000, "example.com");
        entry.network = Some(KvNetwork {
            cluster_size: Some(5),
        });

        let meta = KvMetadata::from_entry(&entry);
        assert_eq!(
            meta.cluster_size,
            Some(5),
            "metadata should mirror entry network cluster_size"
        );
    }

    #[test]
    fn metadata_from_entry_without_network_has_none_cluster_size() {
        let entry = KvEntry::tombstone(1000);
        let meta = KvMetadata::from_entry(&entry);
        assert!(
            meta.cluster_size.is_none(),
            "metadata should have None cluster_size when entry has no network"
        );
    }

    #[test]
    fn device_roundtrip() {
        let device = KvDevice {
            is_mobile: 0,
            ja4_class: Some("t13d1516h2".to_owned()),
            platform_class: Some("mac".to_owned()),
            h2_fp_hash: Some("a3f9d21c8b04".to_owned()),
            known_browser: Some(true),
        };
        let json = serde_json::to_string(&device).expect("should serialize KvDevice");
        let deserialized: KvDevice =
            serde_json::from_str(&json).expect("should deserialize KvDevice");

        assert_eq!(deserialized.is_mobile, 0);
        assert_eq!(deserialized.ja4_class.as_deref(), Some("t13d1516h2"));
        assert_eq!(deserialized.platform_class.as_deref(), Some("mac"));
        assert_eq!(deserialized.h2_fp_hash.as_deref(), Some("a3f9d21c8b04"));
        assert_eq!(deserialized.known_browser, Some(true));
    }

    #[test]
    fn device_none_fields_omitted_from_json() {
        let device = KvDevice {
            is_mobile: 2,
            ja4_class: None,
            platform_class: None,
            h2_fp_hash: None,
            known_browser: None,
        };
        let json = serde_json::to_string(&device).expect("should serialize");
        assert!(
            !json.contains("\"ja4_class\""),
            "None ja4_class should be omitted, got: {json}"
        );
        assert!(
            !json.contains("\"known_browser\""),
            "None known_browser should be omitted, got: {json}"
        );
    }

    #[test]
    fn none_device_omitted_from_entry_json() {
        let entry = KvEntry::tombstone(1000);
        let json = serde_json::to_string(&entry).expect("should serialize");
        assert!(
            !json.contains("\"device\""),
            "None device should be omitted from JSON, got: {json}"
        );
    }

    #[test]
    fn entry_without_device_deserializes() {
        let json = r#"{
            "v": 1,
            "created": 1000,
            "last_seen": 1000,
            "consent": { "ok": true, "updated": 1000 },
            "geo": { "country": "US" }
        }"#;
        let entry: KvEntry =
            serde_json::from_str(json).expect("should deserialize entry without device");

        assert!(
            entry.device.is_none(),
            "missing device should deserialize as None"
        );
    }

    #[test]
    fn metadata_from_entry_mirrors_device_fields() {
        let consent = sample_consent_context();
        let geo = sample_geo_info();
        let mut entry = KvEntry::new(&consent, Some(&geo), 1000, "example.com");
        entry.device = Some(KvDevice {
            is_mobile: 1,
            ja4_class: Some("t13d2013h2".to_owned()),
            platform_class: Some("ios".to_owned()),
            h2_fp_hash: None,
            known_browser: Some(true),
        });

        let meta = KvMetadata::from_entry(&entry);
        assert_eq!(
            meta.is_mobile,
            Some(1),
            "metadata should mirror device is_mobile"
        );
        assert_eq!(
            meta.known_browser,
            Some(true),
            "metadata should mirror device known_browser"
        );
    }

    #[test]
    fn metadata_without_device_fields_deserializes() {
        let json = r#"{"ok":true,"country":"US","v":1}"#;
        let meta: KvMetadata = serde_json::from_str(json).expect("should deserialize old metadata");

        assert!(meta.is_mobile.is_none(), "is_mobile should default to None");
        assert!(
            meta.known_browser.is_none(),
            "known_browser should default to None"
        );
    }

    fn kv_uid(id: &str, atype: Option<i32>) -> KvUid {
        KvUid {
            id: id.to_owned(),
            atype,
            ext: None,
        }
    }

    fn kv_eid(uids: Vec<KvUid>) -> KvEid {
        KvEid {
            uids,
            inserter: None,
            matcher: None,
            mm: None,
            ext: None,
        }
    }

    /// The set of spec 6.1's `example-ids.example.com` record.
    fn example_full_set() -> Vec<KvEid> {
        vec![
            KvEid {
                uids: vec![
                    kv_uid("example-a", Some(1)),
                    KvUid {
                        id: "example-b".to_owned(),
                        atype: Some(3),
                        ext: Some(json!({ "stype": "ppuid" })),
                    },
                ],
                inserter: Some("example.com".to_owned()),
                matcher: None,
                mm: Some(3),
                ext: None,
            },
            KvEid {
                uids: vec![kv_uid("example-c", Some(1))],
                inserter: None,
                matcher: Some("partner.example.com".to_owned()),
                mm: None,
                ext: None,
            },
        ]
    }

    fn with_set(set: Vec<KvEid>) -> KvEntry {
        let mut entry = KvEntry::minimal("example.com", "example-uid", 1);
        entry.ids.get_mut("example.com").expect("should exist").eids = set;
        entry
    }

    #[test]
    fn golden_version_1_row_reads_as_one_entry_records_and_writes_version_2() {
        let main_row = json!({
            "v": 1,
            "created": 1_760_000_000,
            "consent": { "ok": true, "updated": 1_760_000_000 },
            "geo": { "country": "US" },
            "ids": {
                "id5-sync.com": { "uid": "ID5*example" },
                "sharedid.org": { "uid": "example-shared-id" }
            }
        });

        let mut entry: KvEntry =
            serde_json::from_value(main_row).expect("should read a row from main");
        let messages = entry.upgrade_for_read().expect("should accept version 1");
        entry.validate().expect("should be valid once upgraded");

        assert!(
            messages.is_empty(),
            "should drop nothing from a valid version 1 row"
        );
        assert_eq!(
            entry.ids["id5-sync.com"],
            KvPartnerId::new("ID5*example"),
            "should read uid as one entry"
        );
        assert_eq!(
            serde_json::to_value(&entry).expect("should write"),
            json!({
                "v": 2,
                "created": 1_760_000_000,
                "consent": { "ok": true, "updated": 1_760_000_000 },
                "geo": { "country": "US" },
                "ids": {
                    "id5-sync.com": { "eids": [{ "uids": [{ "id": "ID5*example" }] }] },
                    "sharedid.org": { "eids": [{ "uids": [{ "id": "example-shared-id" }] }] }
                }
            }),
            "should write version 2, with no uid key and no ps"
        );
    }

    #[test]
    fn version_2_row_and_provider_state_round_trip() {
        let stored = json!({
            "v": 2,
            "created": 1_760_000_000,
            "consent": { "ok": true, "updated": 1_760_000_000 },
            "geo": { "country": "US" },
            "ids": {
                "id5-sync.com": {
                    "w": "id5",
                    "rv": "9c1d04b7e6a83f2a",
                    "at": 1_760_000_000,
                    "exp": 1_767_776_000,
                    "ra": 1_760_086_400,
                    "ru": 1_770_000_000,
                    "eids": [{ "uids": [{ "id": "ID5*example", "atype": 1, "ext": { "linkType": 2 } }] }]
                },
                "example-ids.example.com": {
                    "w": "exampleid",
                    "rv": "4be71f0c2d9a6e35",
                    "at": 1_760_000_000,
                    "eids": [
                        {
                            "uids": [
                                { "id": "example-a", "atype": 1 },
                                { "id": "example-b", "atype": 3, "ext": { "stype": "ppuid" } }
                            ],
                            "inserter": "example.com",
                            "mm": 3
                        },
                        {
                            "uids": [{ "id": "example-c", "atype": 1 }],
                            "matcher": "partner.example.com"
                        }
                    ]
                }
            },
            "ps": {
                "id5": {
                    "n": 1_760_003_600,
                    "f": 1,
                    "o": { "at": 1_759_000_000, "fp": "1a" },
                    "oc": 1_759_500_000,
                    "iv": ["aaaaaaaaaaaaaaaa"],
                    "ov": ["bbbbbbbbbbbbbbbb"],
                    "st": { "signature": "example-opaque-state" }
                }
            }
        });

        let entry: KvEntry = serde_json::from_value(stored.clone()).expect("should read");
        entry.validate().expect("should be valid");
        assert_eq!(
            serde_json::to_value(&entry).expect("should write"),
            stored,
            "should round-trip exactly"
        );
        assert_eq!(
            entry.ids["example-ids.example.com"].eids,
            example_full_set(),
            "should read the set whole and in order"
        );
    }

    #[test]
    fn a_record_reads_its_eids_and_its_version_1_uid_only_without_them() {
        let both: KvPartnerId = serde_json::from_value(json!({
            "uid": "stale-copy",
            "eids": [{ "uids": [{ "id": "example-a" }] }]
        }))
        .expect("should read a record with both keys");
        assert_eq!(
            both.first_uid(),
            Some("example-a"),
            "should take eids over a uid copy"
        );

        let neither: KvPartnerId =
            serde_json::from_value(json!({ "w": "exampleid" })).expect("should read");
        assert!(
            neither.eids.is_empty(),
            "should read no entry from a record with neither key"
        );
        assert_eq!(neither.first_uid(), None, "should have no first UID");
        with_set(Vec::new())
            .validate()
            .expect_err("should reject a record with an empty eids on write");
    }

    #[test]
    fn upgrade_for_read_drops_bad_records_and_names_the_source_only() {
        let mut set = example_full_set();
        set[1].matcher = Some("  ".to_owned());
        let mut entry = with_set(set);
        entry.ids.insert(
            "sharedid.org".to_owned(),
            KvPartnerId::new("example-shared-id"),
        );
        entry.v = 1;

        let messages = entry.upgrade_for_read().expect("should accept the row");

        assert_eq!(entry.v, SCHEMA_VERSION, "should upgrade the version");
        assert_eq!(messages.len(), 1, "should report one dropped record");
        assert!(
            messages[0].contains("example.com"),
            "should name the source"
        );
        assert!(
            !messages[0].contains("example-a"),
            "should never name a UID"
        );
        assert!(
            !entry.ids.contains_key("example.com"),
            "should drop the bad record"
        );
        assert_eq!(
            entry.ids["sharedid.org"],
            KvPartnerId::new("example-shared-id"),
            "should leave a valid record alone"
        );
        entry.validate().expect("should validate once upgraded");

        let mut future = KvEntry::minimal("example.com", "example-uid", 1);
        future.v = SCHEMA_VERSION + 1;
        future
            .upgrade_for_read()
            .expect_err("should reject a version it does not know");
    }

    #[test]
    fn entry_set_bytes_measures_the_stored_array() {
        let set = example_full_set();
        let bytes = serde_json::to_vec(&set).expect("should serialize the set");

        assert_eq!(
            entry_set_bytes(&set),
            bytes.len(),
            "should measure the stored array"
        );
        assert!(
            !String::from_utf8(bytes)
                .expect("should be UTF-8")
                .contains("\"source\""),
            "should measure entries without source"
        );
    }

    #[test]
    fn validate_rejects_entry_set_limits() {
        let rejects = |entry: &KvEntry, field: &str| {
            let error = entry
                .validate()
                .expect_err(&format!("should reject {field}"));
            assert!(error.contains(field), "should name {field}, got {error}");
        };

        rejects(&with_set(Vec::new()), "eids");

        let mut set = example_full_set();
        set[0].ext = Some(json!({ "pad": "x".repeat(MAX_PARTNER_EXT_BYTES) }));
        rejects(&with_set(set), "ext");

        let mut set = example_full_set();
        set[0].uids[0].ext = Some(json!("not an object"));
        rejects(&with_set(set), "ext");

        let mut set = example_full_set();
        set[0].inserter = Some("  ".to_owned());
        rejects(&with_set(set), "inserter");

        let mut set = example_full_set();
        set[1].matcher = Some("m".repeat(MAX_EID_FIELD_BYTES + 1));
        rejects(&with_set(set), "matcher");

        let mut set = example_full_set();
        set[0].mm = Some(-1);
        rejects(&with_set(set), "mm");

        let mut set = example_full_set();
        set[0].uids[1].atype = Some(-1);
        rejects(&with_set(set), "atype");

        let mut set = example_full_set();
        set[1].uids[0].id = " ".to_owned();
        rejects(&with_set(set), "uid");

        let mut set = example_full_set();
        set[1].uids.clear();
        rejects(&with_set(set), "uid");

        let nine_entries = (0..=MAX_ENTRIES_PER_SOURCE)
            .map(|index| kv_eid(vec![kv_uid(&format!("example-{index}"), None)]))
            .collect();
        rejects(&with_set(nine_entries), "entries");

        let thirty_three_uids = (0..=MAX_UIDS_PER_SOURCE)
            .map(|index| kv_uid(&format!("example-{index}"), None))
            .collect();
        rejects(&with_set(vec![kv_eid(thirty_three_uids)]), "uids");

        let oversized = (0..4)
            .map(|index| KvEid {
                ext: Some(json!({ "pad": "x".repeat(1_100) })),
                ..kv_eid(vec![kv_uid(&format!("example-{index}"), None)])
            })
            .collect();
        rejects(&with_set(oversized), "entry set");

        let mut old = with_set(example_full_set());
        old.v = 1;
        rejects(&old, "version");

        with_set(example_full_set())
            .validate()
            .expect("should accept the control set");
    }

    #[test]
    fn validate_rejects_oversized_state_and_too_many_providers() {
        let with_state = |state: KvProviderState| {
            let mut entry = KvEntry::minimal("example.com", "example-uid", 1);
            entry.ps.insert("id5".to_owned(), state);
            entry
        };
        let hashes = |count: usize| vec!["a".repeat(16); count];

        let error = with_state(KvProviderState {
            st: Some(json!("x".repeat(MAX_PROVIDER_STATE_BYTES))),
            ..KvProviderState::default()
        })
        .validate()
        .expect_err("should reject st");
        assert!(error.contains("st"), "should name st");

        for (name, state) in [
            (
                "iv",
                KvProviderState {
                    iv: hashes(MAX_INVALIDATED_UIDS + 1),
                    ..KvProviderState::default()
                },
            ),
            (
                "ov",
                KvProviderState {
                    ov: hashes(MAX_INVALIDATED_UIDS + 1),
                    ..KvProviderState::default()
                },
            ),
            (
                "fp",
                KvProviderState {
                    o: Some(KvOptOut {
                        at: 1,
                        fp: "XYZ".to_owned(),
                    }),
                    ..KvProviderState::default()
                },
            ),
        ] {
            let error = with_state(state)
                .validate()
                .expect_err(&format!("should reject {name}"));
            assert!(error.contains(name), "should name {name}, got {error}");
        }
        with_state(KvProviderState {
            iv: hashes(MAX_INVALIDATED_UIDS),
            ov: hashes(MAX_INVALIDATED_UIDS),
            o: Some(KvOptOut {
                at: 1,
                fp: "1a".to_owned(),
            }),
            ..KvProviderState::default()
        })
        .validate()
        .expect("should accept 128 hashes in each list and a hex fingerprint");

        let mut entry = KvEntry::minimal("example.com", "example-uid", 1);
        for index in 0..=MAX_PROVIDER_STATES {
            entry
                .ps
                .insert(format!("p{index}"), KvProviderState::default());
        }
        assert!(
            entry
                .validate()
                .expect_err("should reject ps size")
                .contains("ps"),
            "should name ps"
        );

        let mut entry = KvEntry::minimal("example.com", "example-uid", 1);
        entry.ids.get_mut("example.com").expect("should exist").rv = Some("not-hex".to_owned());
        assert!(
            entry
                .validate()
                .expect_err("should reject rv")
                .contains("rv"),
            "should name rv"
        );

        KvEntry::minimal("example.com", "example-uid", 1)
            .validate()
            .expect("should accept the control entry");
    }
}
