//! KV identity graph schema types.
//!
//! These types define the JSON schema stored in the Fastly KV Store for the
//! EC identity graph. Each EC hash (64-char hex prefix) maps to a [`KvEntry`]
//! containing consent state, geo location, and accumulated partner IDs.
//!
//! The schema is versioned (`v: 1`) to allow future migrations.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::consent::ConsentContext;
use crate::geo::GeoInfo;

/// Current schema version for KV entries.
pub const SCHEMA_VERSION: u8 = 1;

/// Full KV entry stored as the body of an EC identity graph record.
///
/// **KV key:** 64-character hex hash (the stable prefix from the EC ID).
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
}

/// A synced partner user ID within a KV entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KvPartnerId {
    /// The partner's user identifier.
    pub uid: String,
    /// Unix timestamp (seconds) when this UID was written/updated.
    pub synced: u64,
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
}

impl KvEntry {
    /// Creates a new live entry from the current request context.
    #[must_use]
    pub fn new(consent: &ConsentContext, geo: Option<&GeoInfo>, now: u64) -> Self {
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
            },
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
            },
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
            Some(info) => Self {
                country: info.country.clone(),
                region: info.region.clone(),
            },
            None => Self {
                country: "ZZ".to_owned(),
                region: None,
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
        }
    }

    #[test]
    fn entry_serialization_roundtrip() {
        let geo = sample_geo_info();
        let consent = sample_consent_context();
        let mut entry = KvEntry::new(&consent, Some(&geo), 1741824000);
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
        };

        let json = serde_json::to_string(&meta).expect("should serialize KvMetadata");
        let deserialized: KvMetadata =
            serde_json::from_str(&json).expect("should deserialize KvMetadata");

        assert!(deserialized.ok, "should be ok=true");
        assert_eq!(deserialized.country, "US");
        assert_eq!(deserialized.v, 1);
    }

    #[test]
    fn metadata_fits_in_2048_bytes() {
        // Worst case: long country code (though ISO 3166-1 is always 2 chars)
        let meta = KvMetadata {
            ok: false,
            country: "XX".to_owned(),
            v: SCHEMA_VERSION,
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
        let entry = KvEntry::new(&consent, Some(&geo), 1000);

        assert_eq!(entry.v, SCHEMA_VERSION);
        assert_eq!(entry.created, 1000);
        assert_eq!(entry.last_seen, 1000);
        assert!(entry.consent.ok, "should be a live entry");
        assert_eq!(entry.consent.updated, 1000);
        assert_eq!(entry.geo.country, "US");
        assert!(entry.ids.is_empty(), "should have no partner IDs initially");
    }

    #[test]
    fn new_entry_without_geo_uses_zz() {
        let consent = ConsentContext::default();
        let entry = KvEntry::new(&consent, None, 1000);
        assert_eq!(
            entry.geo.country, "ZZ",
            "should use ZZ when geo is unavailable"
        );
        assert!(entry.geo.region.is_none());
    }

    #[test]
    fn minimal_entry_has_partner_id_and_placeholder_geo() {
        let entry = KvEntry::minimal("ssp_x", "abc123", 1741824000);

        assert_eq!(entry.v, SCHEMA_VERSION);
        assert!(entry.consent.ok, "should be a live entry");
        assert_eq!(entry.geo.country, "ZZ");
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
    }

    #[test]
    fn metadata_from_entry_mirrors_fields() {
        let consent = sample_consent_context();
        let geo = sample_geo_info();
        let entry = KvEntry::new(&consent, Some(&geo), 1000);
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
}
