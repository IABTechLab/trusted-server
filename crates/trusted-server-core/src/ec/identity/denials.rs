//! Provider denials of browser EID candidates, read from KV (spec 7.1,
//! "Provider denials apply to browser candidates").

use std::collections::{BTreeMap, BTreeSet};

use super::catalog::ProviderCatalog;
use super::ids::SourceDomain;
use super::request::opted_out;
use super::revision::uid_hash;
use crate::ec::kv_types::KvEntry;
use crate::openrtb::Eid;

/// What the providers that claim a source deny of the browser's candidates.
///
/// Empty without a KV read.
#[derive(Debug, Default)]
pub(crate) struct Denials {
    /// Per source, the `ps.iv` and `ps.ov` hashes of every claiming provider.
    hashes: BTreeMap<SourceDomain, BTreeSet<String>>,
    /// Sources whose browser value is dropped whole: those of an opted-out
    /// owner, and every source of an opted-out capture-only provider.
    dropped: BTreeSet<SourceDomain>,
}

impl Denials {
    fn is_empty(&self) -> bool {
        self.hashes.is_empty() && self.dropped.is_empty()
    }
}

/// The denials `entry` holds for the providers in `catalog` (spec 7.1).
#[must_use]
pub(crate) fn denials(entry: &KvEntry, catalog: &ProviderCatalog) -> Denials {
    let mut result = Denials::default();
    let sources: BTreeSet<&SourceDomain> = catalog
        .providers()
        .flat_map(|provider| catalog.sources_of(provider.descriptor().id()))
        .collect();
    for source in sources {
        let hashes: BTreeSet<String> = catalog
            .claimants(source)
            .filter_map(|provider| entry.ps.get(provider.as_str()))
            .flat_map(|state| state.iv.iter().chain(&state.ov).cloned())
            .collect();
        if !hashes.is_empty() {
            result.hashes.insert(source.clone(), hashes);
        }
    }
    for provider in opted_out(entry) {
        let capture_only = catalog.is_capture_only(&provider);
        for source in catalog.sources_of(&provider) {
            if capture_only || catalog.owner_of(source) == Some(&provider) {
                result.dropped.insert(source.clone());
            }
        }
    }
    result
}

/// The browser's candidates after `denials` (spec 7.1).
///
/// Drops each entry of a dropped-whole source; otherwise drops each UID whose
/// hash a claiming provider holds, then each entry left with no UID. Every
/// other entry goes through untouched, in order.
#[must_use]
pub(crate) fn filter_browser_candidates(client: &[Eid], denials: &Denials) -> Vec<Eid> {
    if denials.is_empty() {
        return client.to_vec();
    }
    let mut kept = Vec::with_capacity(client.len());
    for eid in client {
        let Ok(source) = eid.source.parse::<SourceDomain>() else {
            kept.push(eid.clone());
            continue;
        };
        if denials.dropped.contains(&source) {
            continue;
        }
        let Some(hashes) = denials.hashes.get(&source) else {
            kept.push(eid.clone());
            continue;
        };
        let mut filtered = eid.clone();
        filtered
            .uids
            .retain(|uid| !hashes.contains(&uid_hash(&uid.id)));
        if !filtered.uids.is_empty() {
            kept.push(filtered);
        }
    }
    kept
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::ec::identity::test_support::{fixed_capture_provider, fixed_provider, pid, source};
    use crate::ec::kv_types::{KvOptOut, KvProviderState};
    use crate::openrtb::Uid;

    fn uid(id: &str) -> Uid {
        Uid {
            id: id.to_owned(),
            atype: Some(1),
            ext: None,
        }
    }

    fn browser(source: &str, ids: &[&str]) -> Eid {
        Eid {
            source: source.to_owned(),
            uids: ids.iter().map(|id| uid(id)).collect(),
            inserter: Some("inserter.example.com".to_owned()),
            matcher: None,
            mm: Some(1),
            ext: None,
        }
    }

    fn entry_with(states: &[(&str, KvProviderState)]) -> KvEntry {
        let mut entry = KvEntry::minimal("unused.example.com", "unused", 1);
        for (provider, state) in states {
            entry.ps.insert((*provider).to_owned(), state.clone());
        }
        entry
    }

    fn invalidated(uids: &[&str]) -> KvProviderState {
        KvProviderState {
            iv: uids.iter().map(|uid| uid_hash(uid)).collect(),
            ..KvProviderState::default()
        }
    }

    fn opted_out_state() -> KvProviderState {
        KvProviderState {
            o: Some(KvOptOut {
                at: 10,
                fp: "0".to_owned(),
            }),
            ..KvProviderState::default()
        }
    }

    fn unowned_catalog() -> ProviderCatalog {
        ProviderCatalog::new(
            vec![
                fixed_provider("pa", &["a.example.com"]),
                fixed_provider("pb", &["b.example.com"]),
            ],
            BTreeMap::new(),
        )
    }

    #[test]
    fn denial_drops_a_browser_uid_in_a_claimants_iv_for_an_unowned_source() {
        let entry = entry_with(&[("pa", invalidated(&["a-old"]))]);
        let denials = denials(&entry, &unowned_catalog());

        let kept = filter_browser_candidates(
            &[
                browser("a.example.com", &["a-old"]),
                browser("b.example.com", &["b1"]),
            ],
            &denials,
        );

        assert_eq!(
            kept,
            vec![browser("b.example.com", &["b1"])],
            "should drop the invalidated UID and its emptied entry"
        );
    }

    #[test]
    fn denial_drops_a_browser_uid_in_a_claimants_ov() {
        let state = KvProviderState {
            ov: vec![uid_hash("a-removed")],
            ..KvProviderState::default()
        };
        let entry = entry_with(&[("pa", state)]);
        let denials = denials(&entry, &unowned_catalog());

        let kept = filter_browser_candidates(&[browser("a.example.com", &["a-removed"])], &denials);

        assert_eq!(
            kept,
            Vec::<Eid>::new(),
            "should drop a UID the opt-out removed"
        );
    }

    #[test]
    fn denial_filters_uid_by_uid_and_drops_an_emptied_entry() {
        let entry = entry_with(&[("pa", invalidated(&["A"]))]);
        let denials = denials(&entry, &unowned_catalog());

        let kept = filter_browser_candidates(
            &[
                browser("a.example.com", &["A", "B"]),
                browser("a.example.com", &["A"]),
            ],
            &denials,
        );

        assert_eq!(
            kept,
            vec![Eid {
                source: "a.example.com".to_owned(),
                uids: vec![uid("B")],
                inserter: Some("inserter.example.com".to_owned()),
                matcher: None,
                mm: Some(1),
                ext: None,
            }],
            "should keep B alone and drop the entry holding only A"
        );
    }

    #[test]
    fn denial_ignores_hashes_of_a_provider_that_does_not_claim_the_source() {
        let entry = entry_with(&[("pb", invalidated(&["shared"]))]);
        let denials = denials(&entry, &unowned_catalog());

        let kept = filter_browser_candidates(&[browser("a.example.com", &["shared"])], &denials);

        assert_eq!(
            kept,
            vec![browser("a.example.com", &["shared"])],
            "should not apply another source's hashes"
        );
    }

    #[test]
    fn denial_drops_every_owned_source_while_the_owner_is_opted_out() {
        let catalog = ProviderCatalog::new(
            vec![fixed_provider("owner", &["a.example.com", "b.example.com"])],
            BTreeMap::from([(source("a.example.com"), pid("owner"))]),
        );
        let entry = entry_with(&[("owner", opted_out_state())]);
        let denials = denials(&entry, &catalog);

        let kept = filter_browser_candidates(
            &[
                browser("a.example.com", &["a1"]),
                browser("b.example.com", &["b1"]),
            ],
            &denials,
        );

        assert_eq!(
            kept,
            vec![browser("b.example.com", &["b1"])],
            "should drop the owned source and keep the unowned one"
        );
    }

    #[test]
    fn denial_drops_every_source_of_an_opted_out_capture_only_provider() {
        let catalog = ProviderCatalog::new(
            vec![
                fixed_capture_provider("cap", &["a.example.com", "b.example.com"]),
                fixed_provider("other", &["c.example.com"]),
            ],
            BTreeMap::from([(source("a.example.com"), pid("cap"))]),
        );
        let client = [
            browser("a.example.com", &["a1"]),
            browser("b.example.com", &["b1"]),
            browser("c.example.com", &["c1"]),
        ];
        let entry = entry_with(&[("cap", opted_out_state())]);

        let kept = filter_browser_candidates(&client, &denials(&entry, &catalog));

        assert_eq!(
            kept,
            vec![browser("c.example.com", &["c1"])],
            "should drop owned and unowned sources of the capture-only provider"
        );

        let cleared = KvProviderState {
            oc: Some(20),
            ..opted_out_state()
        };
        let entry = entry_with(&[("cap", cleared)]);
        let kept = filter_browser_candidates(&client, &denials(&entry, &catalog));
        assert_eq!(
            kept,
            client.to_vec(),
            "control: after a later oc the browser values go out again"
        );
    }

    #[test]
    fn denial_mirror_id_fed_back_through_the_body_is_filtered() {
        let entry = entry_with(&[("pa", invalidated(&["mirror-only"]))]);
        let denials = denials(&entry, &unowned_catalog());

        let kept =
            filter_browser_candidates(&[browser("a.example.com", &["mirror-only"])], &denials);

        assert_eq!(
            kept,
            Vec::<Eid>::new(),
            "a stale mirror ID fed back as a browser candidate should not be sent"
        );
    }
}
