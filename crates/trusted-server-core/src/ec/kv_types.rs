//! KV identity graph schema types.
//!
//! These types define the JSON schema stored in the Fastly KV Store for the
//! EC identity graph. Each EC ID (`{64hex}.{6alnum}`) maps to a [`KvEntry`]
//! containing consent state, geo location, and accumulated partner IDs.
//!
//! The schema is versioned (`v: 1`) to allow future migrations.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Deserializer, Serialize, de};
use serde_json::value::RawValue;

use super::eid_types::{
    AgentType, EidFields, Expiry, MAX_EID_FIELD_BYTES, MatchMethod, ProviderId, RawJson, UidValue,
    UnixSeconds, optional_exp,
};
use crate::consent::ConsentContext;
use crate::geo::GeoInfo;

/// Current schema version for KV entries.
pub const SCHEMA_VERSION: u8 = 1;

// Unsupported schema versions fail closed on read. Future schema bumps must
// add an explicit lazy migration or backfill path before changing this value.

/// Maximum number of publisher-domain entries accepted in
/// [`KvPubProperties::seen_domains`].
///
/// New entries seed this set with the creation domain only. Runtime organic
/// requests no longer append domains.
pub const MAX_SEEN_DOMAINS: usize = 50;

/// Maximum allowed hostname length for publisher domains stored in KV.
pub const MAX_STORED_DOMAIN_LENGTH: usize = 255;

/// Maximum allowed length (in bytes) for a partner UID across all sync
/// mechanisms (pixel, batch, pull) and identity providers. Sized for
/// `LiveRamp` envelopes (about 700 bytes). Defined centrally to ensure
/// consistent validation.
pub const MAX_UID_LENGTH: usize = 1024;

/// Full KV entry stored as the body of an EC identity graph record.
///
/// **KV key:** Full EC ID (`{64hex}.{6alnum}`).
/// **KV value:** JSON-serialized `KvEntry` (max ~5KB).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
}

/// Consent state within a KV entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

/// Maximum serialized size of a KV entry body written by a page load.
pub const MAX_ENTRY_BYTES: usize = 16 * 1024;

/// A partner ID record within a KV entry, keyed by source domain.
///
/// `uid` is always present so older readers keep working. Records written by
/// an identity provider also carry full-fidelity `eids`; new code reads
/// `eids` whenever it is non-empty and treats `uid` as derived.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KvPartnerId {
    /// First live UID of `eids`, or the single UID for records written by
    /// browser ingest, batch sync or older versions.
    pub uid: String,
    /// Full-fidelity entries written by a provider.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    eids: Vec<KvEid>,
}

impl KvPartnerId {
    /// Builds a single-UID record (browser ingest, batch sync, pull sync).
    #[must_use]
    pub fn single(uid: impl Into<String>) -> Self {
        Self {
            uid: uid.into(),
            eids: Vec::new(),
        }
    }

    /// Builds a provider record, keeping only entries with live UIDs.
    ///
    /// Returns `None` when no live UID remains.
    #[must_use]
    pub fn from_eids(eids: Vec<KvEid>, now: UnixSeconds) -> Option<Self> {
        let mut record = Self {
            uid: String::new(),
            eids,
        };
        record.prune_expired(now);
        (!record.eids.is_empty()).then_some(record)
    }

    /// Full-fidelity provider entries; empty for single-UID records.
    #[must_use]
    pub fn eids(&self) -> &[KvEid] {
        &self.eids
    }

    /// Whether this record was written by an identity provider.
    #[must_use]
    pub fn is_provider_written(&self) -> bool {
        !self.eids.is_empty()
    }

    /// Whether a UID in the record asks its provider for a refresh by `now`.
    #[must_use]
    pub fn refresh_due(&self, now: UnixSeconds) -> bool {
        self.eids
            .iter()
            .flat_map(KvEid::uids)
            .any(|uid| uid.refresh_due(now))
    }

    /// Whether the record holds a UID that may be served at `now`.
    #[must_use]
    pub fn has_live_uid(&self, now: UnixSeconds) -> bool {
        if self.eids.is_empty() {
            return !self.uid.is_empty();
        }
        self.eids
            .iter()
            .flat_map(KvEid::uids)
            .any(|uid| uid.is_live(now))
    }

    /// Drops expired UIDs and empty entries, then resyncs `uid`.
    ///
    /// Returns whether anything changed. Single-UID records never change.
    pub fn prune_expired(&mut self, now: UnixSeconds) -> bool {
        if self.eids.is_empty() && !self.uid.is_empty() {
            return false;
        }
        let before = self.clone();
        for eid in &mut self.eids {
            eid.uids.retain(|uid| uid.is_live(now));
        }
        self.eids.retain(|eid| !eid.uids.is_empty());
        self.uid = self
            .eids
            .first()
            .and_then(|eid| eid.uids.first())
            .map(|uid| uid.id().as_str().to_owned())
            .unwrap_or_default();
        *self != before
    }

    /// Drops entries left without UIDs and resyncs `uid`.
    ///
    /// Returns whether the record still holds a UID within
    /// [`MAX_UID_LENGTH`].
    fn retain_readable(&mut self) -> bool {
        if self.eids.is_empty() {
            return self.uid.len() <= MAX_UID_LENGTH;
        }
        self.eids.retain(|eid| !eid.uids.is_empty());
        self.uid = self
            .eids
            .first()
            .and_then(|eid| eid.uids.first())
            .map(|uid| uid.id().as_str().to_owned())
            .unwrap_or_default();
        !self.eids.is_empty()
    }

    /// Removes the last UID of the last entry and resyncs `uid`; used by the
    /// write planner's size cap.
    pub(crate) fn drop_last_uid(&mut self) {
        if let Some(eid) = self.eids.last_mut() {
            eid.uids.pop();
        }
        self.eids.retain(|eid| !eid.uids.is_empty());
        self.uid = self
            .eids
            .first()
            .and_then(|eid| eid.uids.first())
            .map(|uid| uid.id().as_str().to_owned())
            .unwrap_or_default();
    }
}

/// One stored EID entry with its EID-level fields.
///
/// Stored flat (`{"inserter": .., "uids": [..]}`). Serialization flattens
/// [`EidFields`]; deserialization goes through a flat private representation
/// because serde's `flatten` cannot deserialize the `RawValue` behind `ext`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(from = "KvEidRepr")]
pub struct KvEid {
    #[serde(flatten)]
    fields: EidFields,
    uids: Vec<KvUid>,
}

/// Flat stored shape of [`KvEid`], used only to deserialize it.
#[derive(Deserialize)]
struct KvEidRepr {
    #[serde(default)]
    inserter: Option<Box<str>>,
    #[serde(default)]
    matcher: Option<Box<str>>,
    #[serde(default)]
    mm: Option<MatchMethod>,
    #[serde(default)]
    ext: Option<RawJson>,
    #[serde(deserialize_with = "deserialize_readable_uids")]
    uids: Vec<KvUid>,
}

/// The `id` of a stored UID, read without validating the rest of it.
#[derive(Deserialize)]
struct StoredId<'a> {
    #[serde(borrow)]
    id: Cow<'a, str>,
}

/// Deserializes stored UIDs, skipping any whose `id` exceeds
/// [`MAX_UID_LENGTH`].
///
/// A row written under a higher limit then still loads; an entry left
/// without UIDs is removed by [`KvEntry::drop_unreadable_ids`]. Any other
/// invalid UID still fails the row.
fn deserialize_readable_uids<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<KvUid>, D::Error> {
    let raw = Vec::<Box<RawValue>>::deserialize(deserializer)?;
    let mut uids = Vec::with_capacity(raw.len());
    for value in &raw {
        let oversized = serde_json::from_str::<StoredId<'_>>(value.get())
            .is_ok_and(|stored| stored.id.len() > MAX_UID_LENGTH);
        if oversized {
            continue;
        }
        uids.push(serde_json::from_str(value.get()).map_err(de::Error::custom)?);
    }
    Ok(uids)
}

impl From<KvEidRepr> for KvEid {
    fn from(repr: KvEidRepr) -> Self {
        let KvEidRepr {
            inserter,
            matcher,
            mm,
            ext,
            uids,
        } = repr;
        Self {
            fields: EidFields {
                inserter,
                matcher,
                mm,
                ext,
            },
            uids,
        }
    }
}

impl KvEid {
    /// Builds an entry from validated parts.
    #[must_use]
    pub fn new(fields: EidFields, uids: Vec<KvUid>) -> Self {
        Self { fields, uids }
    }

    /// EID-level fields.
    #[must_use]
    pub fn fields(&self) -> &EidFields {
        &self.fields
    }

    /// Stored UIDs, live or not.
    #[must_use]
    pub fn uids(&self) -> &[KvUid] {
        &self.uids
    }
}

/// One stored UID with its owner and expiry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KvUid {
    id: UidValue,
    atype: AgentType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ext: Option<RawJson>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    owner: Option<ProviderId>,
    #[serde(
        default,
        rename = "exp",
        with = "optional_exp",
        skip_serializing_if = "Expiry::is_never"
    )]
    expiry: Expiry,
    /// Private refresh state; never served, mirrored or forwarded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    refresh: Option<KvRefresh>,
}

/// Refresh state a provider keeps with one of its UIDs.
///
/// It stays in the row only: outbound EIDs are built from the UID's `id`,
/// `atype` and `ext`, so this state never reaches bidders or cookies.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct KvRefresh {
    /// Provider-defined state, opaque to the framework.
    pub state: String,
    /// When the owning provider should be asked to refresh the UID.
    pub from: UnixSeconds,
}

impl core::fmt::Debug for KvRefresh {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("KvRefresh")
            .field("from", &self.from)
            .finish_non_exhaustive()
    }
}

impl KvUid {
    /// Builds a UID from validated parts.
    #[must_use]
    pub fn new(
        id: UidValue,
        atype: AgentType,
        ext: Option<RawJson>,
        owner: Option<ProviderId>,
        expiry: Expiry,
    ) -> Self {
        Self {
            id,
            atype,
            ext,
            owner,
            expiry,
            refresh: None,
        }
    }

    /// Attaches the owning provider's private refresh state.
    #[must_use]
    pub fn with_refresh(mut self, refresh: KvRefresh) -> Self {
        self.refresh = Some(refresh);
        self
    }

    /// The owning provider's private refresh state.
    #[must_use]
    pub fn refresh(&self) -> Option<&KvRefresh> {
        self.refresh.as_ref()
    }

    /// Whether the owning provider asked to refresh this live UID by `now`.
    ///
    /// An expired UID is never refresh-due: its source is due anyway, and a
    /// refresh that keeps failing must not outlive the UID.
    #[must_use]
    pub fn refresh_due(&self, now: UnixSeconds) -> bool {
        self.is_live(now)
            && self
                .refresh
                .as_ref()
                .is_some_and(|refresh| refresh.from <= now)
    }

    /// The UID value.
    #[must_use]
    pub fn id(&self) -> &UidValue {
        &self.id
    }

    /// The agent type.
    #[must_use]
    pub fn atype(&self) -> AgentType {
        self.atype
    }

    /// The UID-level `ext`.
    #[must_use]
    pub fn ext(&self) -> Option<&RawJson> {
        self.ext.as_ref()
    }

    /// The provider that wrote this UID.
    #[must_use]
    pub fn owner(&self) -> Option<&ProviderId> {
        self.owner.as_ref()
    }

    /// When the UID stops being served.
    #[must_use]
    pub fn expiry(&self) -> Expiry {
        self.expiry
    }

    /// Whether the UID may be served at `now`.
    #[must_use]
    pub fn is_live(&self, now: UnixSeconds) -> bool {
        self.expiry.is_live(now)
    }
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
    /// Removes partner records holding no UID this build can serve.
    ///
    /// Lets this build load rows written by one with a higher
    /// [`MAX_UID_LENGTH`] instead of rejecting the whole row. Returns the
    /// number of records removed.
    pub fn drop_unreadable_ids(&mut self) -> usize {
        let before = self.ids.len();
        self.ids.retain(|_, record| record.retain_readable());
        before - self.ids.len()
    }

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
        }
    }

    /// Creates a minimal live entry.
    ///
    /// Used by tests and fixtures that need a valid live entry with one
    /// partner UID and placeholder metadata.
    #[must_use]
    pub fn minimal(partner_id: &str, uid: &str, now: u64) -> Self {
        let mut ids = BTreeMap::new();
        ids.insert(partner_id.to_owned(), KvPartnerId::single(uid.to_owned()));
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

        for (partner_id, partner_uid) in &self.ids {
            if partner_uid.uid.len() > MAX_UID_LENGTH {
                return Err(format!(
                    "partner ID '{partner_id}' exceeds MAX_UID_LENGTH ({})",
                    partner_uid.uid.len()
                ));
            }
            let oversized_field = partner_uid.eids().iter().any(|eid| {
                let fields = eid.fields();
                [fields.inserter.as_deref(), fields.matcher.as_deref()]
                    .into_iter()
                    .flatten()
                    .any(|field| field.len() > MAX_EID_FIELD_BYTES)
            });
            if oversized_field {
                return Err(format!(
                    "partner ID '{partner_id}' has an EID inserter or matcher over MAX_EID_FIELD_BYTES"
                ));
            }
            if partner_uid.eids().iter().any(|eid| eid.uids().is_empty()) {
                return Err(format!(
                    "partner ID '{partner_id}' has an EID entry with no UIDs"
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
    use crate::ec::eid_types::{
        AgentType, EidFields, Expiry, MatchMethod, ProviderId, RawJson, UidValue, UnixSeconds,
    };

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
        entry.ids.insert(
            "liveramp".to_owned(),
            KvPartnerId::single("LR_xyz".to_owned()),
        );

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
            deserialized.ids.get("liveramp").map(|p| p.uid.as_str()),
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
                .map(|partner| partner.uid.as_str()),
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
            KvPartnerId::single("x".repeat(MAX_UID_LENGTH + 1)),
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
        assert_eq!(partner.uid, "abc123");
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

    fn provider_uid(id: &str, expiry: Expiry) -> KvUid {
        KvUid::new(
            UidValue::try_from(id).expect("should build uid"),
            AgentType::try_from(1).expect("should build atype"),
            None,
            Some(ProviderId::new("example-provider")),
            expiry,
        )
    }

    #[test]
    fn legacy_partner_id_round_trips_without_eids() {
        let legacy: KvPartnerId = serde_json::from_value(serde_json::json!({ "uid": "abc" }))
            .expect("should read legacy record");
        assert_eq!(legacy, KvPartnerId::single("abc"), "should read legacy uid");
        assert_eq!(
            serde_json::to_value(&legacy).expect("should serialize"),
            serde_json::json!({ "uid": "abc" }),
            "should not add eids to legacy records"
        );
    }

    #[test]
    fn from_eids_sets_uid_to_first_live_uid() {
        let now = UnixSeconds::new(100);
        let eid = KvEid::new(
            EidFields::default(),
            vec![
                provider_uid("expired", Expiry::At(UnixSeconds::new(50))),
                provider_uid("live", Expiry::Never),
            ],
        );
        let record = KvPartnerId::from_eids(vec![eid], now).expect("should keep live uid");
        assert_eq!(record.uid, "live", "should mirror first live uid into uid");
        assert!(record.is_provider_written(), "should mark provider record");
    }

    #[test]
    fn prune_expired_drops_dead_uids_and_resyncs_uid() {
        let eid = KvEid::new(
            EidFields::default(),
            vec![
                provider_uid("first", Expiry::At(UnixSeconds::new(150))),
                provider_uid("second", Expiry::Never),
            ],
        );
        let mut record =
            KvPartnerId::from_eids(vec![eid], UnixSeconds::new(100)).expect("should build");
        assert_eq!(record.uid, "first", "should start on first uid");

        let changed = record.prune_expired(UnixSeconds::new(200));

        assert!(changed, "should report pruning");
        assert_eq!(record.eids()[0].uids().len(), 1, "should drop expired uid");
        assert_eq!(record.uid, "second", "should resync uid");
    }

    #[test]
    fn provider_record_round_trips_with_owner_and_exp() {
        let eid = KvEid::new(
            EidFields::default(),
            vec![provider_uid("v", Expiry::At(UnixSeconds::new(500)))],
        );
        let record = KvPartnerId::from_eids(vec![eid], UnixSeconds::new(1)).expect("should build");
        let json = serde_json::to_value(&record).expect("should serialize");
        assert_eq!(
            json,
            serde_json::json!({ "uid": "v", "eids": [{ "uids": [{ "id": "v", "atype": 1, "owner": "example-provider", "exp": 500 }] }] }),
            "should store owner and exp"
        );
        let back: KvPartnerId = serde_json::from_value(json).expect("should deserialize");
        assert_eq!(back, record, "should round-trip");
    }

    #[test]
    fn validate_rejects_empty_provider_eid() {
        let mut entry = KvEntry::minimal("ids.example.com", "v", 1);
        let record: KvPartnerId =
            serde_json::from_value(serde_json::json!({ "uid": "v", "eids": [{ "uids": [] }] }))
                .expect("should parse");
        entry.ids.insert("ids.example.com".to_owned(), record);
        assert!(entry.validate().is_err(), "should reject EID with no UIDs");
    }

    #[test]
    fn validate_accepts_liveramp_sized_uid() {
        let entry = KvEntry::minimal("liveramp.example.com", &"x".repeat(703), 1);
        assert!(
            entry.validate().is_ok(),
            "should accept a 703-byte envelope-sized UID"
        );
    }

    #[test]
    fn drop_unreadable_ids_removes_oversized_single_uid() {
        let mut entry = KvEntry::minimal("ids.example.com", "kept", 1);
        entry.ids.insert(
            "long.example.com".to_owned(),
            KvPartnerId::single("x".repeat(MAX_UID_LENGTH + 1)),
        );

        let dropped = entry.drop_unreadable_ids();

        assert_eq!(dropped, 1, "should drop the oversized record");
        assert_eq!(
            entry.ids.keys().collect::<Vec<_>>(),
            vec!["ids.example.com"],
            "should keep the readable record"
        );
        assert!(entry.validate().is_ok(), "should validate after dropping");
    }

    #[test]
    fn deserialize_drops_oversized_provider_uids() {
        let long = "x".repeat(MAX_UID_LENGTH + 1);
        let json = serde_json::json!({
            "v": SCHEMA_VERSION,
            "created": 1,
            "consent": { "ok": true, "updated": 1 },
            "geo": { "country": "US" },
            "ids": {
                "partial.example.com": { "uid": long, "eids": [
                    { "uids": [{ "id": long, "atype": 1 }] },
                    { "uids": [{ "id": "short", "atype": 1 }] }
                ] },
                "gone.example.com": { "uid": long, "eids": [
                    { "uids": [{ "id": long, "atype": 1 }] }
                ] }
            }
        })
        .to_string();
        let mut entry: KvEntry =
            serde_json::from_str(&json).expect("should parse a row with oversized UIDs");

        let dropped = entry.drop_unreadable_ids();

        assert_eq!(dropped, 1, "should drop the record left without UIDs");
        let partial = entry
            .ids
            .get("partial.example.com")
            .expect("should keep the record with a readable UID");
        assert_eq!(partial.uid, "short", "should resync uid to the kept UID");
        assert_eq!(partial.eids().len(), 1, "should drop the emptied entry");
        assert!(entry.validate().is_ok(), "should validate after dropping");
    }

    #[test]
    fn provider_record_round_trips_eid_level_fields() {
        let ext = serde_json::value::to_raw_value(&serde_json::json!({ "k": 1 }))
            .expect("should encode ext");
        let fields = EidFields {
            inserter: Some("inserter.example.com".into()),
            matcher: Some("matcher.example.com".into()),
            mm: Some(MatchMethod::Authenticated),
            ext: Some(RawJson::try_from(ext.as_ref()).expect("should accept ext")),
        };
        let eid = KvEid::new(fields, vec![provider_uid("v", Expiry::Never)]);
        let record =
            KvPartnerId::from_eids(vec![eid], UnixSeconds::new(1)).expect("should build record");

        let json = serde_json::to_string(&record).expect("should serialize");
        let back: KvPartnerId = serde_json::from_str(&json).expect("should deserialize");

        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&json).expect("should parse"),
            serde_json::json!({ "uid": "v", "eids": [{
                "inserter": "inserter.example.com",
                "matcher": "matcher.example.com",
                "mm": 3,
                "ext": { "k": 1 },
                "uids": [{ "id": "v", "atype": 1, "owner": "example-provider" }]
            }] }),
            "should store EID-level fields flat beside uids"
        );
        assert_eq!(back, record, "should round-trip EID-level fields");
    }

    #[test]
    fn provider_record_rejects_invalid_stored_ext() {
        let big = "x".repeat(crate::ec::eid_types::MAX_EXT_BYTES);
        let cases = [
            serde_json::json!({ "uid": "v", "eids": [{ "ext": [1], "uids": [{ "id": "v", "atype": 1 }] }] }),
            serde_json::json!({ "uid": "v", "eids": [{ "ext": { "k": big }, "uids": [{ "id": "v", "atype": 1 }] }] }),
            serde_json::json!({ "uid": "v", "eids": [{ "uids": [{ "id": "v", "atype": 1, "ext": { "k": big } }] }] }),
            serde_json::json!({ "uid": "v", "eids": [{ "uids": [{ "id": "v", "atype": 1, "ext": "text" }] }] }),
        ];

        for case in cases {
            let text = serde_json::to_string(&case).expect("should encode case");
            assert!(
                serde_json::from_str::<KvPartnerId>(&text).is_err(),
                "should reject invalid stored ext: {text}"
            );
        }
    }

    #[test]
    fn validate_rejects_oversized_inserter_and_matcher() {
        let long = "a".repeat(crate::ec::eid_types::MAX_EID_FIELD_BYTES + 1);
        for field in ["inserter", "matcher"] {
            let mut eid = serde_json::json!({ "uids": [{ "id": "v", "atype": 1 }] });
            eid[field] = serde_json::Value::String(long.clone());
            let record: KvPartnerId =
                serde_json::from_value(serde_json::json!({ "uid": "v", "eids": [eid] }))
                    .expect("should parse record");
            let mut entry = KvEntry::minimal("ids.example.com", "v", 1);
            entry.ids.insert("ids.example.com".to_owned(), record);

            assert!(entry.validate().is_err(), "should reject oversized {field}");
        }
    }

    #[test]
    fn refresh_is_due_only_while_the_uid_is_live() {
        let uid = KvUid::new(
            UidValue::try_from("p-1").expect("should build uid"),
            AgentType::try_from(1).expect("should build atype"),
            None,
            None,
            Expiry::At(UnixSeconds::new(5000)),
        )
        .with_refresh(KvRefresh {
            state: "refresh-state".to_owned(),
            from: UnixSeconds::new(1000),
        });

        assert!(
            !uid.refresh_due(UnixSeconds::new(999)),
            "should wait for `from`"
        );
        assert!(
            uid.refresh_due(UnixSeconds::new(1000)),
            "should be due at `from`"
        );
        assert!(
            !uid.refresh_due(UnixSeconds::new(5000)),
            "should stop refreshing once the UID expires"
        );
    }
}
