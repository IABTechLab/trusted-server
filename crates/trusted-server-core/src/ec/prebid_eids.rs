//! Browser EID normalization for existing-root enrichment.
//!
//! Auction bodies supply current-request EIDs. Only registered sources produce
//! UID-only storage updates; provenance and lifecycle metadata are never stored.

use crate::openrtb::{Eid, Uid};

use super::kv::PartnerIdUpdate;
use super::kv_types::MAX_UID_LENGTH;
use super::registry::PartnerRegistry;

/// Collects one valid UID per registered source without KV I/O.
pub(crate) fn collect_prebid_eid_updates_from_eids(
    eids: &[Eid],
    registry: &PartnerRegistry,
) -> Vec<PartnerIdUpdate> {
    let updates = eids
        .iter()
        .filter_map(|eid| {
            let partner = registry.find_by_source_domain(&eid.source)?;
            let uid = first_valid_uid(&eid.uids)?;
            Some(PartnerIdUpdate::new(&partner.source_domain, &uid.id))
        })
        .collect();
    dedupe_partner_updates(updates)
}

/// Combines staged body updates with the separate sharedId cookie once.
/// The direct sharedId cookie retains its legacy precedence over body input.
pub(crate) fn collect_browser_eid_updates(
    mut updates: Vec<PartnerIdUpdate>,
    sharedid_cookie: Option<&str>,
    registry: &PartnerRegistry,
) -> Vec<PartnerIdUpdate> {
    if let Some(cookie) = sharedid_cookie
        && let Some(update) = collect_sharedid_update(cookie, registry)
    {
        updates.push(update);
    }
    dedupe_partner_updates(updates)
}

pub(crate) fn dedupe_partner_updates(updates: Vec<PartnerIdUpdate>) -> Vec<PartnerIdUpdate> {
    let mut latest = std::collections::BTreeMap::new();
    for update in updates {
        latest.insert(update.partner_id, update.uid);
    }
    latest
        .into_iter()
        .map(|(partner_id, uid)| PartnerIdUpdate::new(partner_id, uid))
        .collect()
}

fn first_valid_uid(uids: &[Uid]) -> Option<&Uid> {
    uids.iter().find(|uid| is_valid_eid_uid(&uid.id))
}

pub(crate) fn is_valid_eid_uid(uid: &str) -> bool {
    !uid.trim().is_empty() && uid.len() <= MAX_UID_LENGTH
}

pub(crate) fn collect_sharedid_update(
    cookie_value: &str,
    registry: &PartnerRegistry,
) -> Option<PartnerIdUpdate> {
    let cookie_value = cookie_value.trim();
    if !is_valid_eid_uid(cookie_value) {
        return None;
    }
    let partner = registry.find_by_source_domain("sharedid.org")?;
    Some(PartnerIdUpdate::new(&partner.source_domain, cookie_value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::EcPartner;
    use serde_json::json;

    fn registry() -> PartnerRegistry {
        let partners = ["id5-sync.com", "sharedid.org"].map(|source| EcPartner {
            name: source.to_owned(),
            source_domain: source.to_owned(),
            openrtb_atype: EcPartner::default_openrtb_atype(),
            bidstream_enabled: true,
            identity_owner: None,
            api_token: None,
            batch_rate_limit: EcPartner::default_batch_rate_limit(),
            pull_sync_enabled: false,
            pull_sync_url: None,
            pull_sync_allowed_domains: vec![],
            pull_sync_ttl_sec: EcPartner::default_pull_sync_ttl_sec(),
            pull_sync_rate_limit: EcPartner::default_pull_sync_rate_limit(),
            ts_pull_token: None,
        });
        PartnerRegistry::from_config(&partners).expect("should build registry")
    }

    #[test]
    fn body_updates_are_registered_uid_only_and_skip_invalid_candidates() {
        let eids: Vec<Eid> = serde_json::from_value(json!([
            {"source":"ID5-SYNC.COM", "inserter":"forged-owner", "matcher":"", "mm":0,
             "uids":[{"id":" ", "atype":1}, {"id":"opaque-envelope", "ext":{"writer":"forged"}}]},
            {"source":"unknown.example", "uids":[{"id":"current-request-only"}]}
        ]))
        .expect("should parse EIDs");
        assert_eq!(
            collect_prebid_eid_updates_from_eids(&eids, &registry()),
            vec![PartnerIdUpdate::new("id5-sync.com", "opaque-envelope")]
        );
    }

    #[test]
    fn sharedid_combines_with_body_and_retains_direct_cookie_precedence() {
        let updates = vec![
            PartnerIdUpdate::new("sharedid.org", "body-shared"),
            PartnerIdUpdate::new("id5-sync.com", "id5"),
        ];
        assert_eq!(
            collect_browser_eid_updates(updates, Some(" cookie-shared "), &registry()),
            vec![
                PartnerIdUpdate::new("id5-sync.com", "id5"),
                PartnerIdUpdate::new("sharedid.org", "cookie-shared")
            ]
        );
    }

    #[test]
    fn uid_limits_are_byte_bounded() {
        assert!(is_valid_eid_uid(&"x".repeat(MAX_UID_LENGTH)));
        assert!(!is_valid_eid_uid(&"x".repeat(MAX_UID_LENGTH + 1)));
        assert!(!is_valid_eid_uid(" \t"));
        assert!(collect_sharedid_update(&"x".repeat(MAX_UID_LENGTH + 1), &registry()).is_none());
        assert!(collect_sharedid_update("valid", &PartnerRegistry::empty()).is_none());
    }
}
