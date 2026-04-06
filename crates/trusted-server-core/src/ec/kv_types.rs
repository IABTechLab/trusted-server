//! KV identity graph schema types.
//!
//! These types define the JSON schema stored in the Fastly KV Store for the
//! EC identity graph. Each EC ID (`{64hex}.{6alnum}`) maps to a [`KvEntry`]
//! containing consent state, geo location, and accumulated partner IDs.
//!
//! The schema is versioned (`v: 1`) to allow future migrations.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::consent::ConsentContext;
use crate::geo::GeoInfo;

/// Current schema version for KV entries.
pub const SCHEMA_VERSION: u8 = 1;

/// Maximum number of domains tracked in [`KvPubProperties::seen_domains`].
/// When the cap is reached, new domains are silently dropped.
pub const MAX_SEEN_DOMAINS: usize = 50;

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
    /// Unix timestamp (seconds) of last organic request.
    /// Updated by [`super::kv::KvIdentityGraph::update_last_seen`] with
    /// a 300-second debounce.
    pub last_seen: u64,
    /// Consent state sub-object.
    pub consent: KvConsent,
    /// Geo location sub-object.
    pub geo: KvGeo,
    /// Publisher domain history for consortium-level identity sharing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pub_properties: Option<KvPubProperties>,
    /// Device class signals (TLS fingerprint, UA platform).
    /// Written once on creation — never updated after.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<KvDevice>,
    /// Network cluster disambiguation data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<KvNetwork>,
    /// Map of partner ID namespace → synced UID record.
    /// Populated by pixel sync, batch sync, and pull sync operations.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub ids: HashMap<String, KvPartnerId>,
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
    /// Market-level targeting signal; not personal data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dma: Option<i64>,
}

/// A synced partner user ID within a KV entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KvPartnerId {
    /// The partner's user identifier.
    pub uid: String,
    /// Unix timestamp (seconds) when this UID was written/updated.
    pub synced: u64,
}

/// A single domain visit record within [`KvPubProperties::seen_domains`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KvDomainVisit {
    /// Unix timestamp (seconds) of first visit to this domain.
    pub first: u64,
    /// Unix timestamp (seconds) of most recent visit to this domain.
    pub last: u64,
    /// Lifetime visit count for this domain.
    pub visits: u32,
}

/// Publisher domain history for consortium-level identity sharing.
///
/// Tracks which publisher properties a user has been seen on, keyed by apex
/// domain. History only accumulates within a shared-passphrase group (same
/// EC hash), so this does not enable cross-site tracking across unrelated
/// publishers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KvPubProperties {
    /// Apex domain where this EC entry was first created.
    pub origin_domain: String,
    /// Per-domain visit history, keyed by apex domain.
    /// Updated on each organic request; capped at [`MAX_SEEN_DOMAINS`] entries.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub seen_domains: HashMap<String, KvDomainVisit>,
}

/// Coarse, non-PII device signals derived from TLS handshake and UA.
///
/// Used by the `/identify` endpoint for cross-suffix propagation decisions
/// and buyer-facing device quality scoring. Written once on
/// [`KvEntry`] creation — never updated after.
///
/// **Privacy:** `ja4_class` (Section 1 only) and `platform_class` are
/// category signals, not unique device identifiers. The full JA4
/// fingerprint (Sections 2–3) is never stored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KvDevice {
    /// Mobile signal: `0` = confirmed desktop, `1` = confirmed mobile,
    /// `2` = genuinely unknown (non-standard client).
    /// Derived from UA platform string — no Client Hints required.
    pub is_mobile: u8,
    /// JA4 Section 1 only — browser family class identifier.
    /// e.g. `"t13d1516h2"` = Chrome, `"t13d2013h2"` = Safari.
    /// Never stores the full JA4 fingerprint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ja4_class: Option<String>,
    /// Coarse OS family from UA: `"mac"`, `"windows"`, `"ios"`,
    /// `"android"`, `"linux"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform_class: Option<String>,
    /// SHA256 prefix (12 hex chars) of the HTTP/2 SETTINGS fingerprint.
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
/// Written only by the `/identify` endpoint — the prefix-match list API
/// call required to compute `cluster_size` is too expensive for the
/// organic proxy hot path.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KvNetwork {
    /// Number of distinct EC suffixes matching this hash prefix.
    /// `None` = not yet evaluated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cluster_size: Option<u32>,
    /// Unix timestamp (seconds) of last cluster check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cluster_checked: Option<u64>,
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

impl KvEntry {
    /// Creates a new live entry from the current request context.
    ///
    /// `domain` is the publisher's apex domain (e.g. `"autoblog.com"`),
    /// used to initialize the [`KvPubProperties`] origin and first visit.
    #[must_use]
    pub fn new(consent: &ConsentContext, geo: Option<&GeoInfo>, now: u64, domain: &str) -> Self {
        let mut seen_domains = HashMap::new();
        seen_domains.insert(
            domain.to_owned(),
            KvDomainVisit {
                first: now,
                last: now,
                visits: 1,
            },
        );

        Self {
            v: SCHEMA_VERSION,
            created: now,
            last_seen: now,
            consent: KvConsent {
                tcf: consent.raw_tc_string.clone(),
                gpp: consent.raw_gpp_string.clone(),
                ok: true,
                updated: now,
            },
            geo: KvGeo::from_geo_info(geo),
            pub_properties: Some(KvPubProperties {
                origin_domain: domain.to_owned(),
                seen_domains,
            }),
            device: None,
            network: None,
            ids: HashMap::new(),
        }
    }

    /// Creates a minimal live entry for the recovery path.
    ///
    /// Used by [`super::kv::KvIdentityGraph::upsert_partner_id`] when the
    /// root KV entry is missing (e.g. the initial best-effort
    /// `create_or_revive` failed on EC generation).
    #[must_use]
    pub fn minimal(partner_id: &str, uid: &str, synced: u64) -> Self {
        let mut ids = HashMap::new();
        ids.insert(
            partner_id.to_owned(),
            KvPartnerId {
                uid: uid.to_owned(),
                synced,
            },
        );
        Self {
            v: SCHEMA_VERSION,
            created: synced,
            last_seen: synced,
            consent: KvConsent {
                tcf: None,
                gpp: None,
                ok: true,
                updated: synced,
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
            last_seen: now,
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
            ids: HashMap::new(),
        }
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
                let dma = if info.metro_code > 0 {
                    Some(info.metro_code)
                } else {
                    None
                };
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
        let mut entry = KvEntry::new(&consent, Some(&geo), 1741824000, "example.com");
        entry.ids.insert(
            "liveramp".to_owned(),
            KvPartnerId {
                uid: "LR_xyz".to_owned(),
                synced: 1741890000,
            },
        );

        let json = serde_json::to_string(&entry).expect("should serialize KvEntry");
        let deserialized: KvEntry =
            serde_json::from_str(&json).expect("should deserialize KvEntry");

        assert_eq!(deserialized.v, SCHEMA_VERSION);
        assert_eq!(deserialized.created, 1741824000);
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
        assert_eq!(entry.last_seen, 1000);
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
        let visit = props
            .seen_domains
            .get("example.com")
            .expect("should have origin domain visit");
        assert_eq!(visit.first, 1000);
        assert_eq!(visit.last, 1000);
        assert_eq!(visit.visits, 1);
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
    fn minimal_entry_has_partner_id_and_placeholder_geo() {
        let entry = KvEntry::minimal("ssp_x", "abc123", 1741824000);

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
        assert_eq!(partner.synced, 1741824000);
    }

    #[test]
    fn tombstone_entry_has_correct_shape() {
        let entry = KvEntry::tombstone(1741910400);

        assert_eq!(entry.v, SCHEMA_VERSION);
        assert!(!entry.consent.ok, "should be a tombstone");
        assert!(entry.ids.is_empty(), "tombstone should have no partner IDs");
        assert_eq!(entry.geo.country, "ZZ");
        assert_eq!(entry.consent.updated, 1741910400);
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
        let entry = KvEntry::new(&consent, Some(&geo), 1000, "autoblog.com");

        let json = serde_json::to_string(&entry).expect("should serialize");
        let deserialized: KvEntry = serde_json::from_str(&json).expect("should deserialize");

        let props = deserialized
            .pub_properties
            .expect("should have pub_properties");
        assert_eq!(props.origin_domain, "autoblog.com");
        assert_eq!(props.seen_domains.len(), 1);
        let visit = props
            .seen_domains
            .get("autoblog.com")
            .expect("should have origin visit");
        assert_eq!(visit.first, 1000);
        assert_eq!(visit.last, 1000);
        assert_eq!(visit.visits, 1);
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
    fn domain_visit_roundtrip() {
        let visit = KvDomainVisit {
            first: 1000,
            last: 2000,
            visits: 5,
        };
        let json = serde_json::to_string(&visit).expect("should serialize");
        let deserialized: KvDomainVisit = serde_json::from_str(&json).expect("should deserialize");

        assert_eq!(deserialized.first, 1000);
        assert_eq!(deserialized.last, 2000);
        assert_eq!(deserialized.visits, 5);
    }

    #[test]
    fn network_roundtrip() {
        let network = KvNetwork {
            cluster_size: Some(3),
            cluster_checked: Some(1774921179),
        };
        let json = serde_json::to_string(&network).expect("should serialize KvNetwork");
        let deserialized: KvNetwork =
            serde_json::from_str(&json).expect("should deserialize KvNetwork");

        assert_eq!(deserialized.cluster_size, Some(3));
        assert_eq!(deserialized.cluster_checked, Some(1774921179));
    }

    #[test]
    fn network_none_fields_omitted_from_json() {
        let network = KvNetwork {
            cluster_size: None,
            cluster_checked: None,
        };
        let json = serde_json::to_string(&network).expect("should serialize");
        assert!(
            !json.contains("\"cluster_size\""),
            "None cluster_size should be omitted, got: {json}"
        );
        assert!(
            !json.contains("\"cluster_checked\""),
            "None cluster_checked should be omitted, got: {json}"
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
            cluster_checked: Some(1000),
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
}
