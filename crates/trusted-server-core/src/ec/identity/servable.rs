//! The one servability check shared by the bidstream, the admin view and
//! `/_ts/api/v1/identify`.

use std::collections::BTreeSet;

use super::catalog::ProviderCatalog;
use super::ids::{IdentityProviderId, SourceDomain, UnixTime};
use super::policy::{PolicySignals, policy_allows};
use crate::consent::ConsentContext;
use crate::ec::kv_types::{KvEntry, KvPartnerId};

/// What a servability decision needs from the request.
pub(crate) struct ServeContext<'a> {
    /// The request's time.
    pub(crate) now: UnixTime,
    /// The visitor's consent on this request, never the row's stored copy.
    pub(crate) consent: &'a ConsentContext,
    /// Signals the consent context does not carry.
    pub(crate) signals: PolicySignals,
    /// Enabled providers and owners.
    pub(crate) catalog: &'a ProviderCatalog,
    /// Providers with a sticky opt-out: KV `ps.o` plus `ts-eids-r` opt-outs.
    pub(crate) opted_out: &'a BTreeSet<IdentityProviderId>,
}

/// Providers whose KV `ps.o` is set and not cleared by a strictly later
/// `ps.oc`. An opt-out and a clear in the same second leave it opted out.
#[must_use]
pub(crate) fn kv_opted_out(entry: &KvEntry) -> BTreeSet<IdentityProviderId> {
    entry
        .ps
        .iter()
        .filter(|(_, state)| {
            state
                .o
                .as_ref()
                .is_some_and(|opt_out| state.oc.is_none_or(|cleared| cleared <= opt_out.at))
        })
        .filter_map(|(provider, _)| provider.parse().ok())
        .collect()
}

/// Whether a stored record may be sent from the server.
///
/// In order: an empty UID or an expired record is never served. A record
/// with a writer needs that writer to be enabled, not opted out, and allowed
/// by its consent policy. An owned source also needs the owner's policy, and
/// when the owner declares the source as expiring, only a record the owner
/// wrote with an unexpired `exp` is served (the rollback rule). Anything else
/// is servable: the caller already applied the generic gates.
#[must_use]
pub(crate) fn is_servable(
    context: &ServeContext<'_>,
    source: &SourceDomain,
    record: &KvPartnerId,
) -> bool {
    if record.uid.is_empty() {
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

    use super::*;
    use crate::ec::identity::descriptor::ConsentPolicy;
    use crate::ec::identity::test_support::{
        TestSignals, allowing_consent, consent_with, fixed_provider, provider_with_policy,
    };
    use crate::ec::kv_types::{KvEntry, KvOptOut, KvPartnerId, KvProviderState};

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
        let consent = allowing_consent();
        let context = ServeContext {
            now: UnixTime::from_secs(100),
            consent: &consent,
            signals: PolicySignals::default(),
            catalog,
            opted_out,
        };
        is_servable(
            &context,
            &source.parse().expect("should parse source"),
            record,
        )
    }

    #[test]
    fn expiry_writer_presence_and_opt_out() {
        let catalog = ProviderCatalog::new(
            vec![fixed_provider("p", &["a.example.com"])],
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
            "control: valid"
        );
        assert!(
            !check(
                &catalog,
                &none,
                "a.example.com",
                &record(Some("p"), Some(100))
            ),
            "expired at exp"
        );
        assert!(
            !check(
                &catalog,
                &none,
                "a.example.com",
                &record(Some("gone"), None)
            ),
            "unknown writer"
        );
        let opted: BTreeSet<_> = ["p".parse().expect("should parse id")]
            .into_iter()
            .collect();
        assert!(
            !check(&catalog, &opted, "a.example.com", &record(Some("p"), None)),
            "opted-out writer"
        );
        assert!(
            check(&catalog, &none, "b.example.com", &record(None, None)),
            "browser record, unowned"
        );
        assert!(
            !check(&catalog, &none, "b.example.com", &KvPartnerId::new("")),
            "empty uid"
        );
    }

    #[test]
    fn rollback_rule_for_expiring_owned_sources() {
        let catalog = ProviderCatalog::new(
            vec![provider_with_policy(
                "owner",
                &["a.example.com"],
                ConsentPolicy::eid_persistence_only(),
                true,
            )],
            [(
                "a.example.com".parse().expect("should parse source"),
                "owner".parse().expect("should parse id"),
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
            "control: owner value"
        );
        assert!(
            !check(&catalog, &none, "a.example.com", &record(None, None)),
            "browser-seeded value is never served from the server"
        );
        assert!(
            !check(
                &catalog,
                &none,
                "a.example.com",
                &record(Some("owner"), None)
            ),
            "stripped exp"
        );
    }

    #[test]
    fn writer_policy_applies_to_unowned_sources() {
        let strict = ConsentPolicy::strict().with_tcf_vendor(9999);
        let catalog = ProviderCatalog::new(
            vec![provider_with_policy("p", &["a.example.com"], strict, false)],
            BTreeMap::new(),
        );
        let consent = consent_with(&TestSignals {
            gdpr: true,
            purposes: vec![1, 4],
            ..TestSignals::default()
        });
        let none = BTreeSet::new();
        let context = ServeContext {
            now: UnixTime::from_secs(100),
            consent: &consent,
            signals: PolicySignals::default(),
            catalog: &catalog,
            opted_out: &none,
        };
        let source = "a.example.com".parse().expect("should parse source");
        assert!(
            !is_servable(&context, &source, &record(Some("p"), None)),
            "writer's vendor denied"
        );
        assert!(
            is_servable(&context, &source, &record(None, None)),
            "control: browser value unaffected"
        );
    }

    #[test]
    fn kv_opt_outs_respect_later_clears() {
        let mut entry = KvEntry::minimal("a.example.com", "u", 1);
        let opt_out = KvOptOut {
            at: 10,
            fp: "0".repeat(16),
        };
        entry.ps.insert(
            "active".to_owned(),
            KvProviderState {
                o: Some(opt_out.clone()),
                oc: Some(10),
                ..KvProviderState::default()
            },
        );
        entry.ps.insert(
            "cleared".to_owned(),
            KvProviderState {
                o: Some(opt_out),
                oc: Some(11),
                ..KvProviderState::default()
            },
        );
        let opted = kv_opted_out(&entry);
        assert!(
            opted.contains(&"active".parse().expect("should parse id")),
            "a same-second clear leaves it opted out"
        );
        assert!(
            !opted.contains(&"cleared".parse().expect("should parse id")),
            "a later clear clears it"
        );
    }
}
