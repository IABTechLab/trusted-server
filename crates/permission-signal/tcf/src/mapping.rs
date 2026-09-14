//! Which Data Use each TCF purpose grants.
//!
//! This table lives here, in the crate for the scheme it belongs to, rather
//! than in the policy file core reads. Core does not know what a TCF purpose
//! is, and a deployment that runs no TCF at all should not carry a table of
//! another scheme's numbers in its configuration.
//!
//! It was moved verbatim from the `signals.tcf.purposes` block of the sample
//! policy, so behavior is unchanged for a deployment that never edited that
//! block. A deployment that had edited it now changes this crate instead.
//!
//! # Where this should eventually come from
//!
//! The IAB Privacy Taxonomy is adding a `tcf` column. When that is finalized
//! it becomes the single source for this mapping and the table below is
//! replaced by reading it, rather than being maintained by hand. Until then
//! this is the authority for this crate.

use trusted_server_core::permissions::Permission;

/// A TCF purpose number and the Data Use identifiers it grants.
///
/// Identifiers rather than [`Permission`] values, so the table reads the same
/// as the policy block it came from and can be checked against the taxonomy by
/// eye.
const PURPOSES: &[(u8, &[&str])] = &[
    (1, &["necessary.operations.storage"]),
    (
        2,
        &[
            "advertising_marketing.first_party.contextual",
            "advertising_marketing.frequency_capping",
            "advertising_marketing.negative_targeting",
        ],
    ),
    (3, &["advertising_marketing.profiling"]),
    (
        4,
        &[
            "advertising_marketing.first_party.targeted",
            "advertising_marketing.third_party.targeted",
        ],
    ),
    (5, &["advertising_marketing.personalize.profiling"]),
    (
        6,
        &[
            "advertising_marketing.personalize.content",
            "advertising_marketing.personalize.system",
            "functional.personalization",
        ],
    ),
    (
        7,
        &[
            "analytics.ad_reporting.measure_ad_performance",
            "analytics.ad_reporting.ad_delivery_and_targeting",
            "analytics.ad_reporting.ad_viewability",
        ],
    ),
    (8, &["analytics.ad_reporting.content_performance"]),
    (
        9,
        &[
            "analytics.ad_reporting.market_research",
            "analytics.ad_reporting.campaign_insights",
        ],
    ),
    (10, &["necessary.operations.improve"]),
    (11, &["select-basic-content"]),
];

/// The TCF purpose that grants `permission`, or `None` when no purpose does.
///
/// A permission no purpose maps to is one TCF has nothing to say about, and
/// the provider answers silence for it rather than a refusal.
///
/// Compared on the identifier string, so a lookup is at most twenty string
/// comparisons. Resolving each identifier back to a [`Permission`] first would
/// scan the whole taxonomy per row, and this runs for every permission on
/// every request.
#[must_use]
pub fn purpose_for(permission: Permission) -> Option<u8> {
    let id = permission.as_str();
    PURPOSES
        .iter()
        .find_map(|(purpose, uses)| uses.contains(&id).then_some(*purpose))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_identifier_in_the_table_is_a_real_data_use() {
        // The failure this guards is a typo silently disabling a purpose. A
        // name that resolves to nothing would make the purpose grant nothing,
        // and no test asserting one specific mapping would notice the ones it
        // does not name.
        let mut unknown = Vec::new();
        for (purpose, uses) in PURPOSES {
            for id in *uses {
                if Permission::from_identifier(id).is_none() {
                    unknown.push(format!("purpose {purpose}: {id}"));
                }
            }
        }
        assert!(
            unknown.is_empty(),
            "these Data Use identifiers are not in the taxonomy: {unknown:?}"
        );
    }

    #[test]
    fn no_data_use_is_granted_by_two_purposes() {
        // The policy parser used to refuse this as a duplicate. With the table
        // in code the check moves here, so a purpose cannot be silently
        // shadowed by an earlier row.
        let mut seen = std::collections::BTreeMap::new();
        for (purpose, uses) in PURPOSES {
            for id in *uses {
                if let Some(first) = seen.insert(*id, *purpose) {
                    panic!("{id} is granted by purpose {first} and again by purpose {purpose}");
                }
            }
        }
    }

    #[test]
    fn the_purposes_the_policy_block_used_to_declare_still_map() {
        // The two mappings the old policy parser test pinned, now pinned here.
        assert_eq!(
            purpose_for(Permission::StoreOnDevice),
            Some(1),
            "Purpose 1 should map to device storage"
        );
        assert_eq!(
            purpose_for(Permission::SelectPersonalisedAds),
            Some(4),
            "Purpose 4 should map to targeted advertising"
        );
    }

    #[test]
    fn a_purpose_granting_several_uses_is_found_from_each_of_them() {
        // Purpose 4 grants two Data Uses, and both must resolve back to it.
        let first = Permission::from_identifier("advertising_marketing.first_party.targeted")
            .expect("should be a known Data Use");
        let third = Permission::from_identifier("advertising_marketing.third_party.targeted")
            .expect("should be a known Data Use");
        assert_eq!(
            purpose_for(first),
            Some(4),
            "the first-party Data Use is Purpose 4"
        );
        assert_eq!(purpose_for(third), Some(4), "and so is the third-party one");
    }

    #[test]
    fn a_data_use_no_purpose_grants_maps_to_nothing() {
        // A sale disclosure is a Data Use the taxonomy carries and no TCF
        // purpose grants. Silence rather than a refusal is the contract, and
        // it starts here.
        let sale = Permission::from_identifier("disclosure.sale")
            .expect("should be a known Data Use with no TCF purpose");
        assert_eq!(
            purpose_for(sale),
            None,
            "TCF has nothing to say about a Data Use none of its purposes grant"
        );
    }
}
