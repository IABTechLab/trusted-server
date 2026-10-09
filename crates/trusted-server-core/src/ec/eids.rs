//! Shared EID resolution and formatting helpers.
//!
//! Used by both `/_ts/api/v1/identify` and `/auction` to resolve source-domain
//! keyed IDs from KV entries and convert them to `OpenRTB` EID structures.

use crate::openrtb::{Eid, Uid};

use super::kv_types::KvEntry;
use super::registry::PartnerRegistry;

/// A source-domain keyed ID resolved from a KV entry against the partner registry.
///
/// Only includes usable records from live entries and bidstream-enabled partners.
pub struct ResolvedPartnerId {
    /// The partner's identity source domain and EC KV `ids` key.
    pub source_domain: String,
    /// The synced user ID value.
    pub uid: String,
    /// `OpenRTB` agent type for this partner's identifiers.
    pub openrtb_atype: i32,
}

/// Resolves source-domain keyed IDs from a KV entry against the partner registry.
///
/// Filters to live, consented, bidstream-enabled records with a usable UID at
/// the supplied checked time, sorted deterministically by source domain.
#[must_use]
pub fn resolve_partner_ids(
    registry: &PartnerRegistry,
    entry: &KvEntry,
    now: Option<u64>,
) -> Vec<ResolvedPartnerId> {
    let mut resolved = Vec::new();
    if !entry.consent.ok {
        return resolved;
    }

    for (source_domain, partner_uid) in &entry.ids {
        if !partner_uid.is_usable(now) {
            continue;
        }

        let Some(partner) = registry.get(source_domain) else {
            continue;
        };
        if !partner.bidstream_enabled {
            continue;
        }

        resolved.push(ResolvedPartnerId {
            source_domain: partner.source_domain.clone(),
            uid: partner_uid.uid.clone(),
            openrtb_atype: partner.openrtb_atype,
        });
    }

    resolved.sort_by(|a, b| a.source_domain.cmp(&b.source_domain));
    resolved
}

/// Converts resolved partner IDs to `OpenRTB` `Eid` entries.
#[must_use]
pub fn to_eids(resolved: &[ResolvedPartnerId]) -> Vec<Eid> {
    resolved
        .iter()
        .map(|item| Eid {
            inserter: None,
            matcher: None,
            mm: None,
            source: item.source_domain.clone(),
            uids: vec![Uid {
                id: item.uid.clone(),
                atype: Some(item.openrtb_atype),
                ext: None,
            }],
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::redacted::Redacted;
    use crate::settings::EcPartner;

    fn make_test_partner(source_domain: &str) -> EcPartner {
        EcPartner {
            identity_owner: None,
            name: format!("Partner {source_domain}"),
            source_domain: source_domain.to_owned(),
            openrtb_atype: EcPartner::default_openrtb_atype(),
            bidstream_enabled: true,
            api_token: Some(Redacted::new(format!(
                "token-{source_domain}-32-bytes-minimum-value"
            ))),
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
    fn resolve_partner_ids_omits_expired_identity() {
        let registry = PartnerRegistry::from_config(&[make_test_partner("ids.example.com")])
            .expect("should build registry");
        let mut entry = KvEntry::minimal("ids.example.com", "expired-uid", 1_000);
        entry
            .ids
            .get_mut("ids.example.com")
            .expect("should contain record")
            .expires_at = Some(1);

        assert!(
            resolve_partner_ids(&registry, &entry, Some(2)).is_empty(),
            "should omit a known-expired identity without deleting its record"
        );
    }

    #[test]
    fn resolve_partner_ids_checks_clock_boundaries_and_retains_legacy_compatibility() {
        let registry = PartnerRegistry::from_config(&[make_test_partner("ids.example.com")])
            .expect("should build registry");
        let mut entry = KvEntry::minimal("ids.example.com", "example-uid", 1_000);
        entry
            .ids
            .get_mut("ids.example.com")
            .expect("should contain record")
            .expires_at = Some(2_000);
        assert_eq!(
            resolve_partner_ids(&registry, &entry, Some(1_999)).len(),
            1,
            "should resolve before expiry"
        );
        assert!(
            resolve_partner_ids(&registry, &entry, Some(2_000)).is_empty(),
            "should omit at expiry"
        );
        assert!(
            resolve_partner_ids(&registry, &entry, None).is_empty(),
            "should fail closed for known expiry when time is unavailable"
        );
        entry
            .ids
            .get_mut("ids.example.com")
            .expect("should contain record")
            .expires_at = None;
        assert_eq!(
            resolve_partner_ids(&registry, &entry, None).len(),
            1,
            "should retain unknown-expiry compatibility"
        );
        entry.consent.ok = false;
        assert!(
            resolve_partner_ids(&registry, &entry, Some(1_000)).is_empty(),
            "should never deliver a withdrawn row's records"
        );
    }

    #[test]
    fn resolve_partner_ids_sorts_by_source_domain() {
        let partners = vec![
            make_test_partner("zeta.example.com"),
            make_test_partner("alpha.example.com"),
        ];
        let registry = PartnerRegistry::from_config(&partners).expect("should build registry");

        let mut entry = KvEntry::tombstone(1000);
        entry.consent.ok = true;
        entry.ids.insert(
            "zeta.example.com".to_owned(),
            super::super::kv_types::KvPartnerId {
                uid: "uid-z".to_owned(),
                ..Default::default()
            },
        );
        entry.ids.insert(
            "alpha.example.com".to_owned(),
            super::super::kv_types::KvPartnerId {
                uid: "uid-a".to_owned(),
                ..Default::default()
            },
        );

        let resolved = resolve_partner_ids(&registry, &entry, Some(1_000));
        let source_domains: Vec<&str> = resolved
            .iter()
            .map(|item| item.source_domain.as_str())
            .collect();

        assert_eq!(
            source_domains,
            vec!["alpha.example.com", "zeta.example.com"],
            "should sort deterministically by source domain"
        );
    }

    #[test]
    fn to_eids_maps_resolved_ids_correctly() {
        let resolved = vec![
            ResolvedPartnerId {
                uid: "LR_xyz".to_owned(),
                source_domain: "liveramp.com".to_owned(),
                openrtb_atype: 3,
            },
            ResolvedPartnerId {
                uid: "ID5_abc".to_owned(),
                source_domain: "id5-sync.com".to_owned(),
                openrtb_atype: 1,
            },
            ResolvedPartnerId {
                uid: "pair-id".to_owned(),
                source_domain: "google.com".to_owned(),
                openrtb_atype: 571187,
            },
        ];

        let eids = to_eids(&resolved);

        assert_eq!(eids.len(), 3, "should produce one EID per resolved partner");
        assert_eq!(eids[0].source, "liveramp.com");
        assert_eq!(eids[0].uids[0].id, "LR_xyz");
        assert_eq!(eids[0].uids[0].atype, Some(3));
        assert_eq!(eids[1].source, "id5-sync.com");
        assert_eq!(eids[1].uids[0].id, "ID5_abc");
        assert_eq!(eids[1].uids[0].atype, Some(1));
        assert_eq!(eids[2].source, "google.com", "should preserve PAIR source");
        assert_eq!(eids[2].uids[0].id, "pair-id", "should preserve PAIR ID");
        assert_eq!(
            eids[2].uids[0].atype,
            Some(571187),
            "should preserve PAIR vendor-specific atype"
        );
    }
}
