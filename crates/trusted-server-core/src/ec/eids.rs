//! Shared EID resolution and formatting helpers.
//!
//! Used by both `/_ts/api/v1/identify` and `/auction` to resolve partner IDs from KV
//! entries, convert them to `OpenRTB` EID structures, and build base64-encoded
//! response headers.

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use error_stack::{Report, ResultExt};

use crate::error::TrustedServerError;
use crate::openrtb::{Eid, Uid};

use super::kv_types::KvEntry;
use super::registry::PartnerRegistry;

/// Maximum size (in bytes) for the base64-encoded `x-ts-eids` header value.
pub const MAX_EIDS_HEADER_BYTES: usize = 4096;

/// A partner ID resolved from a KV entry against the partner registry.
///
/// Only includes partners with `bidstream_enabled = true` and a non-empty UID.
pub struct ResolvedPartnerId {
    /// Partner namespace key (e.g. `"liveramp"`).
    pub partner_id: String,
    /// The synced user ID value.
    pub uid: String,
    /// The partner's identity source domain (e.g. `"liveramp.com"`).
    pub source_domain: String,
    /// `OpenRTB` agent type for this partner's identifiers.
    pub openrtb_atype: u8,
}

/// Resolves partner IDs from a KV entry against the partner registry.
///
/// Filters to partners with `bidstream_enabled = true` and non-empty UIDs,
/// sorted deterministically by partner ID.
#[must_use]
pub fn resolve_partner_ids(registry: &PartnerRegistry, entry: &KvEntry) -> Vec<ResolvedPartnerId> {
    let mut resolved = Vec::new();

    for (partner_id, partner_uid) in &entry.ids {
        if partner_uid.uid.is_empty() {
            continue;
        }

        let Some(partner) = registry.get(partner_id) else {
            continue;
        };
        if !partner.bidstream_enabled {
            continue;
        }

        resolved.push(ResolvedPartnerId {
            partner_id: partner_id.clone(),
            uid: partner_uid.uid.clone(),
            source_domain: partner.source_domain.clone(),
            openrtb_atype: partner.openrtb_atype,
        });
    }

    resolved.sort_by(|a, b| a.partner_id.cmp(&b.partner_id));
    resolved
}

/// Converts resolved partner IDs to `OpenRTB` `Eid` entries.
#[must_use]
pub fn to_eids(resolved: &[ResolvedPartnerId]) -> Vec<Eid> {
    resolved
        .iter()
        .map(|item| Eid {
            source: item.source_domain.clone(),
            uids: vec![Uid {
                id: item.uid.clone(),
                atype: Some(item.openrtb_atype),
                ext: None,
            }],
        })
        .collect()
}

/// Builds a base64-encoded EID header value, truncating if needed.
///
/// Returns `(encoded_value, was_truncated)`. If the full set of EIDs exceeds
/// [`MAX_EIDS_HEADER_BYTES`] after base64 encoding, partners are removed
/// from the end of the deterministic partner ordering until it fits.
///
/// # Errors
///
/// Returns an error if JSON serialization fails.
pub fn build_eids_header(
    resolved: &[ResolvedPartnerId],
) -> Result<(String, bool), Report<TrustedServerError>> {
    let eids = to_eids(resolved);
    encode_eids_header(&eids)
}

/// Encodes a pre-built EID slice into a base64 header value with truncation.
///
/// Like [`build_eids_header`] but operates on already-constructed `Eid` values
/// (e.g., from `UserInfo.eids` in the auction response path).
///
/// Returns `(encoded_value, was_truncated)`.
///
/// # Errors
///
/// Returns an error if JSON serialization fails.
pub fn encode_eids_header(eids: &[Eid]) -> Result<(String, bool), Report<TrustedServerError>> {
    let try_encode = |size: usize| -> Result<String, Report<TrustedServerError>> {
        let json = serde_json::to_vec(&eids[..size]).change_context(
            TrustedServerError::Configuration {
                message: "Failed to serialize eids header payload".to_owned(),
            },
        )?;
        Ok(BASE64.encode(json))
    };

    // Fast path: try the full slice first (common case — no truncation).
    let encoded = try_encode(eids.len())?;
    if encoded.len() <= MAX_EIDS_HEADER_BYTES {
        return Ok((encoded, false));
    }

    // Binary search for the largest count that fits within the limit.
    // Invariant: lo always fits, hi never fits.
    let mut lo: usize = 0;
    let mut hi: usize = eids.len();

    while lo + 1 < hi {
        let mid = lo + (hi - lo) / 2;
        let encoded = try_encode(mid)?;
        if encoded.len() <= MAX_EIDS_HEADER_BYTES {
            lo = mid;
        } else {
            hi = mid;
        }
    }

    // `lo` is the largest size that fits. Re-encode it for the final value.
    if lo == 0 && !eids.is_empty() {
        log::warn!(
            "encode_eids_header: no EIDs fit within {MAX_EIDS_HEADER_BYTES}B; emitting empty truncated header"
        );
    }
    let encoded = try_encode(lo)?;
    Ok((encoded, true))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::redacted::Redacted;
    use crate::settings::EcPartner;

    fn make_test_partner(id: &str, source_domain: &str) -> EcPartner {
        EcPartner {
            id: id.to_owned(),
            name: format!("Partner {id}"),
            source_domain: source_domain.to_owned(),
            openrtb_atype: EcPartner::default_openrtb_atype(),
            bidstream_enabled: true,
            api_token: Redacted::new(format!("token-{id}-32-bytes-minimum-value")),
            batch_rate_limit: EcPartner::default_batch_rate_limit(),
            pull_sync_enabled: false,
            pull_sync_url: None,
            pull_sync_allowed_domains: vec![],
            pull_sync_ttl_sec: EcPartner::default_pull_sync_ttl_sec(),
            pull_sync_rate_limit: EcPartner::default_pull_sync_rate_limit(),
            ts_pull_token: None,
        }
    }

    #[test]
    fn resolve_partner_ids_sorts_by_partner_id() {
        let partners = vec![
            make_test_partner("zeta", "zeta.example.com"),
            make_test_partner("alpha", "alpha.example.com"),
        ];
        let registry = PartnerRegistry::from_config(&partners).expect("should build registry");

        let mut entry = KvEntry::tombstone(1000);
        entry.consent.ok = true;
        entry.ids.insert(
            "zeta".to_owned(),
            super::super::kv_types::KvPartnerId {
                uid: "uid-z".to_owned(),
            },
        );
        entry.ids.insert(
            "alpha".to_owned(),
            super::super::kv_types::KvPartnerId {
                uid: "uid-a".to_owned(),
            },
        );

        let resolved = resolve_partner_ids(&registry, &entry);
        let partner_ids: Vec<&str> = resolved
            .iter()
            .map(|item| item.partner_id.as_str())
            .collect();

        assert_eq!(
            partner_ids,
            vec!["alpha", "zeta"],
            "should sort deterministically by partner ID"
        );
    }

    #[test]
    fn to_eids_maps_resolved_ids_correctly() {
        let resolved = vec![
            ResolvedPartnerId {
                partner_id: "liveramp".to_owned(),
                uid: "LR_xyz".to_owned(),
                source_domain: "liveramp.com".to_owned(),
                openrtb_atype: 3,
            },
            ResolvedPartnerId {
                partner_id: "id5".to_owned(),
                uid: "ID5_abc".to_owned(),
                source_domain: "id5-sync.com".to_owned(),
                openrtb_atype: 1,
            },
        ];

        let eids = to_eids(&resolved);

        assert_eq!(eids.len(), 2, "should produce one EID per resolved partner");
        assert_eq!(eids[0].source, "liveramp.com");
        assert_eq!(eids[0].uids[0].id, "LR_xyz");
        assert_eq!(eids[0].uids[0].atype, Some(3));
        assert_eq!(eids[1].source, "id5-sync.com");
        assert_eq!(eids[1].uids[0].id, "ID5_abc");
        assert_eq!(eids[1].uids[0].atype, Some(1));
    }

    #[test]
    fn build_eids_header_truncates_when_too_large() {
        let mut resolved = Vec::new();
        for idx in 0..64 {
            resolved.push(ResolvedPartnerId {
                partner_id: format!("partner_{idx}"),
                uid: format!("uid_{}", "x".repeat(100)),
                source_domain: format!("partner-{idx}.example.com"),
                openrtb_atype: 3,
            });
        }

        let (encoded, truncated) =
            build_eids_header(&resolved).expect("should build truncated header");

        assert!(truncated, "should report truncation for large payload");
        assert!(
            encoded.len() <= MAX_EIDS_HEADER_BYTES,
            "should cap encoded header bytes"
        );
    }

    #[test]
    fn build_eids_header_fits_without_truncation() {
        let resolved = vec![ResolvedPartnerId {
            partner_id: "ssp".to_owned(),
            uid: "u1".to_owned(),
            source_domain: "ssp.com".to_owned(),
            openrtb_atype: 3,
        }];

        let (encoded, truncated) =
            build_eids_header(&resolved).expect("should build header without truncation");

        assert!(!truncated, "should not truncate small payload");
        assert!(!encoded.is_empty(), "should produce non-empty value");
    }
}
