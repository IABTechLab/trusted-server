//! Shared helpers for identity tests.

#![cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "the test-utils library build compiles the crate-private helpers without their unit-test callers"
    )
)]

use std::sync::Arc;

use serde_json::json;

use crate::consent::ConsentContext;
use crate::consent::jurisdiction::Jurisdiction;
use crate::consent::types::TcfConsent;
use crate::ec::identity::descriptor::{Captures, ConsentPolicy, ProviderDescriptor, Resolves};
use crate::ec::identity::provider::IdentityProvider;
use crate::ec::kv_types::{KvEid, KvPartnerId, KvUid};

/// A request with no consent signal and no regulated jurisdiction.
#[must_use]
pub fn test_consent_allowing_everything() -> ConsentContext {
    ConsentContext {
        jurisdiction: Jurisdiction::NonRegulated,
        ..ConsentContext::default()
    }
}

/// A GDPR request whose TCF string grants every purpose but not `vendor_id`.
#[must_use]
pub fn test_consent_gdpr_without_vendor(vendor_id: u16) -> ConsentContext {
    let vendors: Vec<u16> = (1..=20).filter(|vendor| *vendor != vendor_id).collect();
    ConsentContext {
        gdpr_applies: true,
        jurisdiction: Jurisdiction::Gdpr,
        tcf: Some(TcfConsent {
            version: 2,
            cmp_id: 1,
            cmp_version: 1,
            consent_screen: 1,
            consent_language: "EN".to_owned(),
            vendor_list_version: 1,
            tcf_policy_version: 4,
            created_ds: 0,
            last_updated_ds: 0,
            purpose_consents: vec![true; 24],
            purpose_legitimate_interests: vec![false; 24],
            vendor_consents: vendors,
            vendor_legitimate_interests: Vec::new(),
            special_feature_opt_ins: vec![false; 12],
        }),
        ..ConsentContext::default()
    }
}

/// Spec 6.1's example set: two entries, three UIDs, with UID-level and
/// EID-level fields.
#[must_use]
pub fn example_entry_set() -> Vec<KvEid> {
    vec![
        KvEid {
            uids: vec![
                KvUid {
                    id: "example-a".to_owned(),
                    atype: Some(1),
                    ext: None,
                },
                KvUid {
                    id: "example-b".to_owned(),
                    atype: Some(3),
                    ext: Some(json!({ "stype": "ppuid" })),
                },
            ],
            inserter: Some("example.com".to_owned()),
            mm: Some(3),
            ..KvEid::default()
        },
        KvEid {
            uids: vec![KvUid {
                id: "example-c".to_owned(),
                atype: Some(1),
                ext: None,
            }],
            matcher: Some("partner.example.com".to_owned()),
            ..KvEid::default()
        },
    ]
}

/// A record holding `set`, written by `writer`.
#[must_use]
pub fn full_record(writer: Option<&str>, set: Vec<KvEid>) -> KvPartnerId {
    KvPartnerId {
        w: writer.map(str::to_owned),
        eids: set,
        ..KvPartnerId::default()
    }
}

/// A descriptor-only resolver for `id`, declaring `sources`, with a dummy
/// endpoint and the `eid_persistence_only` policy.
///
/// # Panics
///
/// Panics when `id` or a source is not valid.
#[must_use]
pub fn fixed_provider(id: &str, sources: &[&str]) -> Arc<dyn IdentityProvider> {
    let mut builder =
        ProviderDescriptor::builder(id.parse().expect("should parse the provider ID"))
            .with_resolves(Resolves::new().with_endpoint("https://api.example.com"))
            .with_consent_policy(ConsentPolicy::eid_persistence_only());
    for source in sources {
        builder = builder.with_source(source.parse().expect("should parse the source"));
    }
    Arc::new(DescriptorOnly(
        builder.build().expect("should build the descriptor"),
    ))
}

/// A descriptor-only capture-only provider for `id`, declaring `sources`.
///
/// # Panics
///
/// Panics when `id` or a source is not valid.
#[must_use]
pub fn fixed_capture_provider(id: &str, sources: &[&str]) -> Arc<dyn IdentityProvider> {
    let mut builder =
        ProviderDescriptor::builder(id.parse().expect("should parse the provider ID"))
            .with_captures(Captures::new().with_path_prefix(&format!("/integrations/{id}/")))
            .with_consent_policy(ConsentPolicy::eid_persistence_only());
    for source in sources {
        builder = builder.with_source(source.parse().expect("should parse the source"));
    }
    Arc::new(DescriptorOnly(
        builder.build().expect("should build the descriptor"),
    ))
}

struct DescriptorOnly(ProviderDescriptor);

impl IdentityProvider for DescriptorOnly {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.0
    }
}
