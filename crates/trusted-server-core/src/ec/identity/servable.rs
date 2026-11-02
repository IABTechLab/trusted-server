//! The one servability check shared by the bidstream, the admin view and
//! `/_ts/api/v1/identify` (spec 7.1).

use std::collections::BTreeSet;

use super::catalog::ProviderCatalog;
use super::ids::{IdentityProviderId, SourceDomain, UnixTime};
use super::policy::{PolicySignals, policy_allows};
use crate::consent::ConsentContext;
use crate::ec::kv_types::KvPartnerId;

/// What a servability decision needs from the request.
pub(crate) struct ServeContext<'a> {
    pub(crate) now: UnixTime,
    pub(crate) consent: &'a ConsentContext,
    pub(crate) signals: PolicySignals,
    pub(crate) catalog: &'a ProviderCatalog,
    /// The providers with a `ps.o` in the row the request read.
    pub(crate) opted_out: &'a BTreeSet<IdentityProviderId>,
}

/// Whether a stored record, the source's whole entry set, may be sent from
/// the server.
#[must_use]
pub(crate) fn is_servable(
    context: &ServeContext<'_>,
    source: &SourceDomain,
    record: &KvPartnerId,
) -> bool {
    if record.eids.is_empty() {
        return false;
    }
    if record.exp.is_some_and(|exp| context.now.as_secs() >= exp) {
        return false;
    }
    let writer = record
        .w
        .as_deref()
        .and_then(|writer| writer.parse::<IdentityProviderId>().ok());
    if record.w.is_some() {
        let Some(writer) = writer.as_ref() else {
            return false;
        };
        let Some(provider) = context.catalog.get(writer) else {
            return false;
        };
        if context.opted_out.contains(writer)
            || !policy_allows(
                provider.descriptor().consent_policy(),
                context.consent,
                context.signals,
            )
        {
            return false;
        }
    }
    if let Some(owner) = context.catalog.owner_of(source) {
        // An opted-out owner stops its source whoever wrote the record, so a
        // browser-seeded record its opt-out did not remove is not served.
        // No stored record matches the owner's `ps.iv`: every invalidation
        // removes its record, and nothing refills one (spec 5.4).
        if context.opted_out.contains(owner) {
            return false;
        }
        let Some(owner_provider) = context.catalog.get(owner) else {
            return false;
        };
        let descriptor = owner_provider.descriptor();
        if !policy_allows(
            descriptor.consent_policy(),
            context.consent,
            context.signals,
        ) {
            return false;
        }
        if descriptor.is_expiring(source)
            && (writer.as_ref() != Some(owner) || record.exp.is_none())
        {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::Arc;

    use super::*;
    use crate::ec::identity::descriptor::{Captures, ConsentPolicy, ProviderDescriptor, Resolves};
    use crate::ec::identity::provider::IdentityProvider;
    use crate::ec::identity::test_support::resolver_policy;
    use crate::ec::identity::test_support::{
        example_entry_set, full_record, test_consent_allowing_everything,
        test_consent_gdpr_without_vendor,
    };
    use crate::ec::kv_types::KvPartnerId;

    struct Fixed(ProviderDescriptor);
    impl IdentityProvider for Fixed {
        fn descriptor(&self) -> &ProviderDescriptor {
            &self.0
        }
    }

    fn provider(
        id: &str,
        source: &str,
        policy: ConsentPolicy,
        expiring: bool,
    ) -> Arc<dyn IdentityProvider> {
        let mut builder =
            ProviderDescriptor::builder(id.parse().expect("should parse the provider ID"))
                .with_source(source.parse().expect("should parse the source"))
                .with_resolves(Resolves::new().with_endpoint("https://api.example.com"))
                .with_consent_policy(policy);
        if expiring {
            builder = builder.with_expiring_ids();
        }
        Arc::new(Fixed(builder.build().expect("should build")))
    }

    fn record(writer: Option<&str>, exp: Option<u64>) -> KvPartnerId {
        KvPartnerId {
            w: writer.map(str::to_owned),
            exp,
            ..KvPartnerId::new("u")
        }
    }

    fn check(
        catalog: &ProviderCatalog,
        opted_out: &BTreeSet<IdentityProviderId>,
        source: &str,
        record: &KvPartnerId,
    ) -> bool {
        let consent = test_consent_allowing_everything();
        let context = ServeContext {
            now: UnixTime::from_secs(100),
            consent: &consent,
            signals: PolicySignals::default(),
            catalog,
            opted_out,
        };
        is_servable(
            &context,
            &source.parse().expect("should parse the source"),
            record,
        )
    }

    #[test]
    fn expiry_writer_presence_and_opt_out() {
        let catalog = ProviderCatalog::new(
            vec![provider(
                "p",
                "a.example.com",
                ConsentPolicy::eid_persistence_only(),
                false,
            )],
            BTreeMap::new(),
        );
        let none = BTreeSet::new();
        assert!(
            check(
                &catalog,
                &none,
                "a.example.com",
                &record(Some("p"), Some(101))
            ),
            "should serve a valid record"
        );
        assert!(
            !check(
                &catalog,
                &none,
                "a.example.com",
                &record(Some("p"), Some(100))
            ),
            "should treat now == exp as expired"
        );
        assert!(
            !check(
                &catalog,
                &none,
                "a.example.com",
                &record(Some("gone"), None)
            ),
            "should not serve an unknown writer"
        );
        let opted: BTreeSet<_> = ["p".parse().expect("should parse the provider ID")]
            .into_iter()
            .collect();
        assert!(
            !check(&catalog, &opted, "a.example.com", &record(Some("p"), None)),
            "should not serve an opted-out writer"
        );
        assert!(
            check(&catalog, &none, "b.example.com", &record(None, None)),
            "should serve an unowned browser record"
        );
    }

    #[test]
    fn an_opted_out_owner_stops_every_record_of_its_source() {
        let catalog = ProviderCatalog::new(
            vec![provider(
                "owner",
                "a.example.com",
                ConsentPolicy::eid_persistence_only(),
                false,
            )],
            [(
                "a.example.com".parse().expect("should parse the source"),
                "owner".parse().expect("should parse the provider ID"),
            )]
            .into_iter()
            .collect(),
        );
        let opted: BTreeSet<_> = ["owner".parse().expect("should parse the provider ID")]
            .into_iter()
            .collect();

        assert!(
            !check(&catalog, &opted, "a.example.com", &record(None, None)),
            "should not serve a browser-seeded record of the owned source"
        );
        assert!(
            check(
                &catalog,
                &BTreeSet::new(),
                "a.example.com",
                &record(None, None)
            ),
            "control: should serve it while the owner is not opted out"
        );
    }

    #[test]
    fn a_full_record_is_servable_or_not_as_a_whole() {
        let catalog = ProviderCatalog::new(
            vec![provider(
                "p",
                "a.example.com",
                ConsentPolicy::eid_persistence_only(),
                false,
            )],
            BTreeMap::new(),
        );
        let none = BTreeSet::new();
        let valid = KvPartnerId {
            exp: Some(101),
            ..full_record(Some("p"), example_entry_set())
        };
        let expired = KvPartnerId {
            exp: Some(100),
            ..full_record(Some("p"), example_entry_set())
        };
        assert!(
            check(&catalog, &none, "a.example.com", &valid),
            "should serve the whole valid set"
        );
        assert!(
            !check(&catalog, &none, "a.example.com", &expired),
            "should never serve an expired set"
        );
    }

    #[test]
    fn rollback_rule_for_expiring_owned_sources() {
        let owner: IdentityProviderId = "owner".parse().expect("should parse the provider ID");
        let catalog = ProviderCatalog::new(
            vec![provider(
                "owner",
                "a.example.com",
                ConsentPolicy::eid_persistence_only(),
                true,
            )],
            [(
                "a.example.com".parse().expect("should parse the source"),
                owner,
            )]
            .into_iter()
            .collect(),
        );
        let none = BTreeSet::new();
        assert!(
            check(
                &catalog,
                &none,
                "a.example.com",
                &record(Some("owner"), Some(200))
            ),
            "should serve the owner's value"
        );
        assert!(
            !check(&catalog, &none, "a.example.com", &record(None, None)),
            "should never serve a browser-seeded value from the server"
        );
        assert!(
            !check(
                &catalog,
                &none,
                "a.example.com",
                &record(Some("owner"), None)
            ),
            "should not serve a value with a stripped exp"
        );
    }

    #[test]
    fn narrowing_limits_claims_and_capture_only_sources() {
        let capture_only: Arc<dyn IdentityProvider> = Arc::new(Fixed(
            ProviderDescriptor::builder("c".parse().expect("should parse the provider ID"))
                .with_source("a.example.com".parse().expect("should parse the source"))
                .with_source("b.example.com".parse().expect("should parse the source"))
                .with_captures(Captures::new().with_path_prefix("/integrations/c/"))
                .build()
                .expect("should build"),
        ));
        let resolver = provider(
            "p",
            "a.example.com",
            ConsentPolicy::eid_persistence_only(),
            false,
        );
        let a: SourceDomain = "a.example.com".parse().expect("should parse the source");
        let b: SourceDomain = "b.example.com".parse().expect("should parse the source");
        let c: IdentityProviderId = "c".parse().expect("should parse the provider ID");
        let p: IdentityProviderId = "p".parse().expect("should parse the provider ID");

        let catalog = ProviderCatalog::new(vec![capture_only, resolver], BTreeMap::new())
            .with_narrowing([(c.clone(), vec![b.clone()])].into_iter().collect());

        assert_eq!(
            catalog.sources_of(&c),
            std::slice::from_ref(&b),
            "should narrow the capture-only provider"
        );
        assert!(
            catalog.is_capture_only(&c) && !catalog.is_capture_only(&p),
            "should tell capture-only apart"
        );
        assert_eq!(
            catalog.capture_only_sources(&c),
            std::slice::from_ref(&b),
            "should list its narrowed sources"
        );
        assert!(
            catalog.capture_only_sources(&p).is_empty(),
            "should list none for a resolver"
        );
        assert_eq!(
            catalog.claimants(&a).collect::<Vec<_>>(),
            vec![&p],
            "should drop a narrowed-away claim"
        );
        assert_eq!(
            catalog.claimants(&b).collect::<Vec<_>>(),
            vec![&c],
            "should keep the narrowed claim"
        );
    }

    #[test]
    fn writer_policy_applies_to_unowned_sources() {
        let strict = resolver_policy().with_tcf_vendor(9999);
        let catalog = ProviderCatalog::new(
            vec![provider("p", "a.example.com", strict, false)],
            BTreeMap::new(),
        );
        let consent = test_consent_gdpr_without_vendor(9999);
        let none = BTreeSet::new();
        let context = ServeContext {
            now: UnixTime::from_secs(100),
            consent: &consent,
            signals: PolicySignals::default(),
            catalog: &catalog,
            opted_out: &none,
        };
        let source = "a.example.com".parse().expect("should parse the source");
        assert!(
            !is_servable(&context, &source, &record(Some("p"), None)),
            "should not serve when the writer's vendor is denied"
        );
        assert!(
            is_servable(&context, &source, &record(None, None)),
            "should leave a browser value unaffected"
        );
    }
}
