//! Shared policy for framework-associated identity observations.
//!
//! Integrations interpret provider data; this module validates authority and
//! stages effects. Only the adapter's post-send executor applies capture writes.

use std::collections::BTreeMap;

use crate::consent::{RawConsentSignals, identity_capture_consent_allowed};
use crate::settings::Settings;

use super::kv::KvIdentityGraph;
use super::kv_types::MAX_UID_LENGTH;
use super::partner::normalize_partner_source_domain;
use super::registry::PartnerRegistry;
use super::{EcContext, EcKvSnapshot, checked_current_timestamp, is_valid_ec_id};

/// Maximum distinct sources contributed by one observation.
const MAX_CAPTURE_SOURCES: usize = 64;

/// Provider interpretation. Browser submissions cannot construct trusted effects.
pub enum IdentityOutcome {
    /// A provider issued a UID, optionally with expiry in Unix seconds.
    Issued {
        /// Registered identity namespace.
        source: String,
        /// Opaque provider UID.
        uid: String,
        /// Supported provider expiry, when supplied.
        expires_at: Option<u64>,
    },
    /// No new token, not invalidation or withdrawal.
    NoChange,
    /// Explicit approved publisher-wide withdrawal, independent of token gating.
    ConsentWithdrawn,
}

/// Body consent signals that may restrict, but never grant request permission.
#[derive(Clone, Default)]
pub struct IdentityConsentSignals {
    /// Populated TCF string.
    pub tcf: Option<String>,
    /// Populated GPP string.
    pub gpp: Option<String>,
    /// Populated US Privacy string.
    pub usp: Option<String>,
    /// Malformed body or signal shape must fail acquisition closed.
    pub malformed: bool,
}

/// Untrusted interpretation returned by a registered capture capability.
pub struct IdentityObservation {
    /// Interpreted token or withdrawal outcomes.
    pub outcomes: Vec<IdentityOutcome>,
    /// Optional body restrictions, checked by the shared consent policy.
    pub consent: IdentityConsentSignals,
}

/// Validated capture update. Writer and revisions are assigned by EC, not JSON.
#[derive(Clone)]
pub(crate) struct ManagedPartnerIdUpdate {
    pub(crate) source: String,
    pub(crate) uid: String,
    pub(crate) expires_at: Option<u64>,
}

/// Request-scoped effects sealed by the framework's proxy dispatch boundary.
///
/// This type intentionally has neither deserialization nor a public constructor.
/// It contains only bounded UID data and the existing full EC-ID-bound snapshot.
#[derive(Clone)]
pub struct IdentityEffects {
    module: &'static str,
    ec_id: String,
    snapshot: EcKvSnapshot,
    action: IdentityAction,
}

#[derive(Clone)]
enum IdentityAction {
    Enrich(Vec<ManagedPartnerIdUpdate>),
    Withdraw,
}

impl IdentityEffects {
    /// Whether this effect requests withdrawal for the current full EC identity.
    #[must_use]
    pub fn withdraws(&self, context: &EcContext) -> bool {
        matches!(self.action, IdentityAction::Withdraw)
            && context.ec_value() == Some(self.ec_id.as_str())
    }

    /// Applies once after send, then supplies persisted state to later pull work.
    ///
    /// Missing, failed, mismatched and withdrawn roots never authorize enrichment.
    /// A withdrawal already completed by finalization does not write again.
    pub fn apply(
        self,
        graph: &KvIdentityGraph,
        partners: &PartnerRegistry,
        context: &mut EcContext,
    ) {
        if context.ec_value() != Some(self.ec_id.as_str()) {
            return;
        }
        let snapshot = if context.kv_snapshot().belongs_to(&self.ec_id) {
            context.kv_snapshot().clone()
        } else {
            self.snapshot
        };
        let result = match self.action {
            IdentityAction::Withdraw => {
                graph.tombstone_existing_from_snapshot(&self.ec_id, snapshot)
            }
            IdentityAction::Enrich(updates) if context.ec_allowed() => graph
                .upsert_managed_ids_from_snapshot(
                    &self.ec_id,
                    self.module,
                    partners,
                    &updates,
                    snapshot,
                ),
            IdentityAction::Enrich(_) => return,
        };
        context.set_kv_snapshot(result);
    }
}

/// Sends exactly once before opening capture storage or applying effects.
///
/// Returns false if capture storage is unavailable, suppressing later disclosure.
/// No effect means no capture lookup. Adapters must finalize withdrawal cookies
/// before handing their actual response sender to this function.
pub fn send_then_apply_identity<F, G>(
    effects: Option<IdentityEffects>,
    context: &mut EcContext,
    partners: &PartnerRegistry,
    send: F,
    graph_factory: G,
) -> bool
where
    F: FnOnce(),
    G: FnOnce() -> Option<KvIdentityGraph>,
{
    send();
    if let Some(effects) = effects {
        let Some(graph) = graph_factory() else {
            return false;
        };
        effects.apply(&graph, partners, context);
    }
    true
}

/// Seals observations from the actual registered proxy response, never headers.
pub(crate) fn stage_capture(
    module: &'static str,
    claims: &[&str],
    observation: IdentityObservation,
    context: &EcContext,
    partners: &PartnerRegistry,
    settings: &Settings,
) -> Option<IdentityEffects> {
    let ec_id = context.ec_value().filter(|id| is_valid_ec_id(id))?;
    if !context.ec_was_present() || context.ec_generated() {
        return None;
    }
    if observation
        .outcomes
        .iter()
        .any(|outcome| matches!(outcome, IdentityOutcome::ConsentWithdrawn))
    {
        return Some(IdentityEffects {
            module,
            ec_id: ec_id.to_owned(),
            snapshot: context.kv_snapshot().clone(),
            action: IdentityAction::Withdraw,
        });
    }
    let signals = RawConsentSignals {
        raw_tc_string: observation.consent.tcf,
        raw_gpp_string: observation.consent.gpp,
        raw_us_privacy: observation.consent.usp,
        ..Default::default()
    };
    if observation.consent.malformed
        || !context.ec_allowed()
        || !identity_capture_consent_allowed(context.consent(), &signals, &settings.consent)
    {
        return None;
    }
    let now = checked_current_timestamp();
    let mut updates = BTreeMap::new();
    for outcome in observation.outcomes {
        let IdentityOutcome::Issued {
            source,
            uid,
            expires_at,
        } = outcome
        else {
            continue;
        };
        let Ok(source) = normalize_partner_source_domain(&source) else {
            continue;
        };
        let Some(partner) = partners.get(&source) else {
            continue;
        };
        if partner.identity_owner.as_deref() != Some(module)
            || !claims.iter().any(|claim| {
                normalize_partner_source_domain(claim).ok().as_deref() == Some(&source)
            })
            || uid.trim().is_empty()
            || uid.len() > MAX_UID_LENGTH
            || expires_at.is_some_and(|expiry| now.is_none_or(|now| now >= expiry))
        {
            continue;
        }
        if !updates.contains_key(&source) && updates.len() >= MAX_CAPTURE_SOURCES {
            continue;
        }
        updates.insert(
            source.clone(),
            ManagedPartnerIdUpdate {
                source,
                uid,
                expires_at,
            },
        );
    }
    if updates.is_empty() {
        return None;
    }
    Some(IdentityEffects {
        module,
        ec_id: ec_id.to_owned(),
        snapshot: context.kv_snapshot().clone(),
        action: IdentityAction::Enrich(updates.into_values().collect()),
    })
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use super::*;
    use crate::consent::jurisdiction::Jurisdiction;
    use crate::consent::{ConsentContext, ConsentSource};
    use crate::consent_config::ConsentMode;
    use crate::ec::kv_types::{KvEntry, KvPartnerId};
    use crate::settings::EcPartner;
    use crate::test_support::tests::create_test_settings;

    const SOURCE: &str = "ids.example.com";
    const MODULE: &str = "example_identity";
    const CLAIMS: &[&str] = &[SOURCE];

    fn ec_id(suffix: &str) -> String {
        format!("{}.{}", "a".repeat(64), &suffix[..6])
    }

    fn context(id: &str) -> EcContext {
        EcContext::new_for_test(
            Some(id.to_owned()),
            ConsentContext {
                jurisdiction: Jurisdiction::NonRegulated,
                source: ConsentSource::Cookie,
                ..Default::default()
            },
        )
    }

    fn registry(owner: Option<&str>) -> PartnerRegistry {
        PartnerRegistry::from_config(&[EcPartner {
            name: "Synthetic ID source".to_owned(),
            source_domain: SOURCE.to_owned(),
            identity_owner: owner.map(str::to_owned),
            openrtb_atype: 1,
            bidstream_enabled: true,
            api_token: None,
            batch_rate_limit: EcPartner::default_batch_rate_limit(),
            pull_sync_enabled: false,
            pull_sync_url: None,
            pull_sync_allowed_domains: vec![],
            pull_sync_ttl_sec: EcPartner::default_pull_sync_ttl_sec(),
            pull_sync_rate_limit: EcPartner::default_pull_sync_rate_limit(),
            ts_pull_token: None,
        }])
        .expect("should build synthetic registry")
    }

    fn issued(source: &str, uid: &str, expiry: Option<u64>) -> IdentityObservation {
        IdentityObservation {
            outcomes: vec![IdentityOutcome::Issued {
                source: source.to_owned(),
                uid: uid.to_owned(),
                expires_at: expiry,
            }],
            consent: IdentityConsentSignals::default(),
        }
    }

    fn live_graph(id: &str) -> KvIdentityGraph {
        let graph = KvIdentityGraph::in_memory("identity-capture-tests");
        let mut entry = KvEntry::minimal(
            "other.example.com",
            "untouched",
            super::super::current_timestamp(),
        );
        entry.consent.ok = true;
        graph
            .create(id, &entry)
            .expect("should seed consenting EC row");
        graph
    }

    fn capture_and_apply(
        graph: &KvIdentityGraph,
        context: &mut EcContext,
        partners: &PartnerRegistry,
        settings: &Settings,
        observation: IdentityObservation,
        claims: &[&str],
    ) -> bool {
        let Some(effect) = stage_capture(MODULE, claims, observation, context, partners, settings)
        else {
            return false;
        };
        effect.apply(graph, partners, context);
        true
    }

    fn stored(graph: &KvIdentityGraph, id: &str) -> (KvEntry, u64) {
        graph
            .get(id)
            .expect("should read KV")
            .expect("should find existing EC row")
    }

    #[test]
    fn capture_rejects_unclaimed_unowned_and_unknown_sources_without_mutation() {
        let id = ec_id("source01");
        let graph = live_graph(&id);
        let settings = create_test_settings();
        let owned = registry(Some(MODULE));
        let unowned = registry(None);
        let original = stored(&graph, &id);
        for (partners, claims, source) in [
            (&owned, &[][..], SOURCE),
            (&unowned, CLAIMS, SOURCE),
            (&owned, CLAIMS, "unknown.example.com"),
        ] {
            let mut request = context(&id);
            request.set_kv_snapshot(graph.load_snapshot(&id));
            assert!(!capture_and_apply(
                &graph,
                &mut request,
                partners,
                &settings,
                issued(source, "synthetic-uid", None),
                claims
            ));
            assert_eq!(
                stored(&graph, &id),
                original,
                "rejected source cannot write"
            );
        }
        let mut request = context(&id);
        for bad in [
            issued("https://ids.example.com/path", "synthetic-uid", None),
            issued(SOURCE, &"x".repeat(MAX_UID_LENGTH + 1), None),
        ] {
            assert!(!capture_and_apply(
                &graph,
                &mut request,
                &owned,
                &settings,
                bad,
                CLAIMS
            ));
            assert_eq!(stored(&graph, &id), original);
        }
    }

    #[test]
    fn trusted_writer_and_revision_are_assigned_by_ec_not_observation() {
        let id = ec_id("writer01");
        let graph = KvIdentityGraph::in_memory("identity-writer-test");
        let mut entry = KvEntry::minimal(
            "other.example.com",
            "untouched",
            super::super::current_timestamp(),
        );
        entry.consent.ok = true;
        entry.ids.insert(
            SOURCE.to_owned(),
            KvPartnerId {
                uid: "synthetic-uid".to_owned(),
                expires_at: None,
                writer: Some("browser".to_owned()),
                revision: Some("untrusted-label".to_owned()),
            },
        );
        graph
            .create(&id, &entry)
            .expect("should seed non-authoritative source metadata");
        let partners = registry(Some(MODULE));
        let settings = create_test_settings();
        let mut request = context(&id);
        request.set_kv_snapshot(graph.load_snapshot(&id));
        assert!(capture_and_apply(
            &graph,
            &mut request,
            &partners,
            &settings,
            issued("IDS.EXAMPLE.COM", "synthetic-uid", None),
            CLAIMS
        ));
        let (entry, _) = stored(&graph, &id);
        let record = &entry.ids[SOURCE];
        assert_eq!(record.uid, "synthetic-uid");
        assert_eq!(record.writer.as_deref(), Some(MODULE));
        assert!(
            record
                .revision
                .as_ref()
                .is_some_and(|revision| !revision.is_empty() && revision != "untrusted-label")
        );
        assert!(record.expires_at.is_none());
        assert_eq!(entry.ids["other.example.com"].uid, "untouched");
    }

    #[test]
    fn restrictive_body_signals_fail_closed_even_when_jurisdiction_or_mode_is_permissive() {
        let id = ec_id("consent1");
        let graph = live_graph(&id);
        let partners = registry(Some(MODULE));
        for mode in [ConsentMode::Proxy, ConsentMode::Interpreter] {
            let mut settings = create_test_settings();
            settings.consent.mode = mode;
            for consent in [
                IdentityConsentSignals {
                    tcf: Some("not-tcf".to_owned()),
                    ..Default::default()
                },
                IdentityConsentSignals {
                    gpp: Some("not-gpp".to_owned()),
                    ..Default::default()
                },
                IdentityConsentSignals {
                    usp: Some("not-usp".to_owned()),
                    ..Default::default()
                },
                IdentityConsentSignals {
                    usp: Some("1YYN".to_owned()),
                    ..Default::default()
                },
                IdentityConsentSignals {
                    malformed: true,
                    ..Default::default()
                },
            ] {
                let mut observation = issued(SOURCE, "synthetic-uid", None);
                observation.consent = consent;
                let mut request = context(&id);
                assert!(
                    stage_capture(MODULE, CLAIMS, observation, &request, &partners, &settings)
                        .is_none()
                );
                assert_eq!(stored(&graph, &id).0.ids.get(SOURCE), None);
                request.set_kv_snapshot(graph.load_snapshot(&id));
            }
        }
    }

    #[test]
    fn request_denial_wins_over_permissive_body_and_body_optout_over_cookie_permission() {
        let id = ec_id("consent2");
        let partners = registry(Some(MODULE));
        let settings = create_test_settings();
        let graph = live_graph(&id);
        for (raw_usp, gpc) in [(Some("1YYN"), false), (None, true)] {
            let mut denied = context(&id);
            denied.consent.raw_us_privacy = raw_usp.map(str::to_owned);
            denied.consent.gpc = gpc;
            assert!(
                stage_capture(
                    MODULE,
                    CLAIMS,
                    issued(SOURCE, "synthetic-uid", None),
                    &denied,
                    &partners,
                    &settings
                )
                .is_none()
            );
        }
        let mut allowed = context(&id);
        let mut observation = issued(SOURCE, "synthetic-uid", None);
        observation.consent.usp = Some("1YYN".to_owned());
        assert!(!capture_and_apply(
            &graph,
            &mut allowed,
            &partners,
            &settings,
            observation,
            CLAIMS
        ));
        assert!(!stored(&graph, &id).0.ids.contains_key(SOURCE));
    }

    #[test]
    fn no_change_and_absent_identity_do_not_create_or_mutate_kv_rows() {
        let id = ec_id("nochange");
        let graph = live_graph(&id);
        let partners = registry(Some(MODULE));
        let settings = create_test_settings();
        let mut request = context(&id);
        request.set_kv_snapshot(graph.load_snapshot(&id));
        let original = stored(&graph, &id);
        assert!(!capture_and_apply(
            &graph,
            &mut request,
            &partners,
            &settings,
            IdentityObservation {
                outcomes: vec![IdentityOutcome::NoChange],
                consent: Default::default()
            },
            CLAIMS
        ));
        assert_eq!(stored(&graph, &id), original);

        let missing_id = ec_id("missing1");
        let mut missing = context(&missing_id);
        missing.set_kv_snapshot(graph.load_snapshot(&missing_id));
        assert!(capture_and_apply(
            &graph,
            &mut missing,
            &partners,
            &settings,
            issued(SOURCE, "synthetic-uid", None),
            CLAIMS
        ));
        assert!(graph.get(&missing_id).expect("should read KV").is_none());
        assert!(matches!(
            missing.kv_snapshot(),
            EcKvSnapshot::Missing { .. }
        ));
        assert_eq!(stored(&graph, &id), original);

        let no_token = EcContext::new_for_test(None, context(&id).consent().clone());
        assert!(
            stage_capture(
                MODULE,
                CLAIMS,
                issued(SOURCE, "synthetic-uid", None),
                &no_token,
                &partners,
                &settings
            )
            .is_none()
        );
        let generated = EcContext::new_for_test_with_cookie(
            Some(ec_id("newborn")),
            None,
            false,
            true,
            context(&id).consent().clone(),
        );
        assert!(
            stage_capture(
                MODULE,
                CLAIMS,
                issued(SOURCE, "synthetic-uid", None),
                &generated,
                &partners,
                &settings
            )
            .is_none()
        );
    }

    #[test]
    fn unreadable_or_tombstoned_roots_cannot_be_revived() {
        let id = ec_id("unread1");
        let graph = live_graph(&id);
        let partners = registry(Some(MODULE));
        let settings = create_test_settings();
        let original = stored(&graph, &id);
        let mut failed = context(&id);
        failed.set_kv_snapshot(EcKvSnapshot::Failed { ec_id: id.clone() });
        assert!(capture_and_apply(
            &graph,
            &mut failed,
            &partners,
            &settings,
            issued(SOURCE, "synthetic-uid", None),
            CLAIMS
        ));
        assert!(matches!(failed.kv_snapshot(), EcKvSnapshot::Failed { .. }));
        assert_eq!(stored(&graph, &id), original);

        let mut stale = context(&id);
        stale.set_kv_snapshot(graph.load_snapshot(&id));
        let late_effect = stage_capture(
            MODULE,
            CLAIMS,
            issued(SOURCE, "stale-uid", None),
            &stale,
            &partners,
            &settings,
        )
        .expect("should stage while live");
        graph.tombstone_existing_from_snapshot(&id, graph.load_snapshot(&id));
        late_effect.apply(&graph, &partners, &mut stale);
        let (_, before_generation) = stored(&graph, &id);
        let mut withdrawn = context(&id);
        withdrawn.set_kv_snapshot(graph.load_snapshot(&id));
        assert!(capture_and_apply(
            &graph,
            &mut withdrawn,
            &partners,
            &settings,
            issued(SOURCE, "late-uid", None),
            CLAIMS
        ));
        let (after, after_generation) = stored(&graph, &id);
        assert!(!after.consent.ok);
        assert!(after.ids.is_empty());
        assert_eq!(before_generation, after_generation);
    }

    #[test]
    fn managed_same_uid_retains_expiry_extends_it_and_never_restores_an_old_revision() {
        let id = ec_id("revision");
        let graph = live_graph(&id);
        let partners = registry(Some(MODULE));
        let settings = create_test_settings();
        let initial_expiry = checked_current_timestamp().expect("should read test clock") + 3600;
        let extended_expiry = initial_expiry + 3600;
        let mut request = context(&id);
        let mut versions: Vec<(Option<String>, u64)> = Vec::new();
        for (uid, expiry, expected_expiry, should_change) in [
            (
                "synthetic-uid",
                Some(initial_expiry),
                Some(initial_expiry),
                true,
            ),
            ("synthetic-uid", None, Some(initial_expiry), false),
            (
                "synthetic-uid",
                Some(extended_expiry),
                Some(extended_expiry),
                true,
            ),
            ("other-uid", None, None, true),
            (
                "synthetic-uid",
                Some(initial_expiry),
                Some(initial_expiry),
                true,
            ),
        ] {
            request.set_kv_snapshot(graph.load_snapshot(&id));
            assert!(capture_and_apply(
                &graph,
                &mut request,
                &partners,
                &settings,
                issued(SOURCE, uid, expiry),
                CLAIMS
            ));
            let (entry, generation) = stored(&graph, &id);
            let record = &entry.ids[SOURCE];
            assert_eq!(record.uid, uid);
            assert_eq!(record.expires_at, expected_expiry);
            assert_eq!(record.writer.as_deref(), Some(MODULE));
            if let Some((earlier_revision, earlier_generation)) = versions.last() {
                assert_eq!(
                    generation != *earlier_generation,
                    should_change,
                    "only material changes should write"
                );
                assert_eq!(
                    record.revision.as_ref() != earlier_revision.as_ref(),
                    should_change,
                    "revision must change only for material updates"
                );
            }
            versions.push((record.revision.clone(), generation));
        }
        let (before, generation) = stored(&graph, &id);
        assert_ne!(
            versions[0].0, versions[4].0,
            "ABA must not restore an older revision"
        );
        request.set_kv_snapshot(graph.load_snapshot(&id));
        assert!(capture_and_apply(
            &graph,
            &mut request,
            &partners,
            &settings,
            issued(SOURCE, "synthetic-uid", None),
            CLAIMS
        ));
        assert_eq!(
            stored(&graph, &id),
            (before, generation),
            "same UID without expiry cannot clear or bump metadata"
        );
    }

    #[test]
    fn expiry_is_rechecked_when_the_deferred_effect_is_applied() {
        let id = ec_id("expire01");
        let graph = live_graph(&id);
        let partners = registry(Some(MODULE));
        let mut request = context(&id);
        request.set_kv_snapshot(graph.load_snapshot(&id));
        let original = stored(&graph, &id);
        // Construct a sealed effect inside the owning module to model an ID
        // that was valid at capture but expired while the response was sent.
        let effect = IdentityEffects {
            module: MODULE,
            ec_id: id.clone(),
            snapshot: request.kv_snapshot().clone(),
            action: IdentityAction::Enrich(vec![ManagedPartnerIdUpdate {
                source: SOURCE.to_owned(),
                uid: "expired-uid".to_owned(),
                expires_at: Some(1),
            }]),
        };
        effect.apply(&graph, &partners, &mut request);
        assert_eq!(
            stored(&graph, &id),
            original,
            "expired token cannot be written"
        );
    }

    #[test]
    fn capture_effects_are_bound_to_the_full_ec_id() {
        let id = ec_id("original");
        let rotated = ec_id("rotated1");
        let graph = live_graph(&id);
        let partners = registry(Some(MODULE));
        let settings = create_test_settings();
        let first = context(&id);
        let effect = stage_capture(
            MODULE,
            CLAIMS,
            issued(SOURCE, "synthetic-uid", None),
            &first,
            &partners,
            &settings,
        )
        .expect("should stage eligible capture");
        let mut later = context(&rotated);
        assert!(!effect.withdraws(&later));
        effect.apply(&graph, &partners, &mut later);
        assert!(!stored(&graph, &id).0.ids.contains_key(SOURCE));
        assert!(graph.get(&rotated).expect("should read KV").is_none());
    }

    #[test]
    fn finalized_withdrawal_skips_duplicate_tombstone_and_late_enrichment() {
        let id = ec_id("withdraw");
        let graph = live_graph(&id);
        let partners = registry(Some(MODULE));
        let settings = create_test_settings();
        let mut request = context(&id);
        request.set_kv_snapshot(graph.load_snapshot(&id));
        let withdrawal = stage_capture(
            MODULE,
            CLAIMS,
            IdentityObservation {
                outcomes: vec![IdentityOutcome::ConsentWithdrawn],
                consent: Default::default(),
            },
            &request,
            &partners,
            &settings,
        )
        .expect("should stage explicit withdrawal");
        assert!(withdrawal.withdraws(&request));
        let finalized = graph.tombstone_existing_from_snapshot(&id, request.kv_snapshot().clone());
        request.set_kv_snapshot(finalized);
        let (first, generation) = stored(&graph, &id);
        assert!(!first.consent.ok);
        assert!(first.ids.is_empty());
        withdrawal.apply(&graph, &partners, &mut request);
        let (second, after_generation) = stored(&graph, &id);
        assert_eq!(
            after_generation, generation,
            "post-send must not duplicate finalization"
        );
        assert_eq!(second, first);
        assert!(capture_and_apply(
            &graph,
            &mut request,
            &partners,
            &settings,
            issued(SOURCE, "late-uid", None),
            CLAIMS
        ));
        assert_eq!(
            stored(&graph, &id),
            (first, generation),
            "late result cannot revive tombstone"
        );
    }

    #[test]
    fn send_boundary_persists_staged_effect_only_after_the_response() {
        let id = ec_id("send01");
        let graph = live_graph(&id);
        let partners = registry(Some(MODULE));
        let settings = create_test_settings();
        let mut request = context(&id);
        request.set_kv_snapshot(graph.load_snapshot(&id));
        let before = stored(&graph, &id);
        let expires_at = checked_current_timestamp().expect("should read test clock") + 3600;
        let effect = stage_capture(
            MODULE,
            CLAIMS,
            issued(SOURCE, "synthetic-uid", Some(expires_at)),
            &request,
            &partners,
            &settings,
        )
        .expect("should stage a real validated effect");
        let order = RefCell::new(Vec::new());
        let handled = send_then_apply_identity(
            Some(effect),
            &mut request,
            &partners,
            || {
                assert_eq!(
                    stored(&graph, &id),
                    before,
                    "send cannot observe capture mutation"
                );
                order.borrow_mut().push("sent");
            },
            || {
                assert_eq!(
                    order.borrow().as_slice(),
                    &["sent"],
                    "open graph only after send"
                );
                assert_eq!(stored(&graph, &id), before);
                order.borrow_mut().push("opened graph");
                Some(graph.clone())
            },
        );
        assert!(handled);
        assert_eq!(order.borrow().as_slice(), &["sent", "opened graph"]);
        let (persisted, generation) = stored(&graph, &id);
        assert!(
            generation > before.1,
            "managed update must reach storage after send"
        );
        let record = &persisted.ids[SOURCE];
        assert_eq!(record.uid, "synthetic-uid");
        assert_eq!(record.expires_at, Some(expires_at));
        assert_eq!(record.writer.as_deref(), Some(MODULE));
        assert!(record.revision.is_some());
        let handed_to_later_pull = request
            .kv_snapshot()
            .entry_for(&id)
            .expect("should keep persisted identity state in post-send context");
        assert_eq!(
            handed_to_later_pull, &persisted,
            "later pull must receive persisted outcome, not the pre-send snapshot"
        );
    }

    #[test]
    fn send_boundary_skips_storage_without_effects_and_fails_closed_when_unavailable() {
        let id = ec_id("send02");
        let graph = live_graph(&id);
        let partners = registry(Some(MODULE));
        let settings = create_test_settings();
        let mut request = context(&id);
        request.set_kv_snapshot(graph.load_snapshot(&id));
        let before = stored(&graph, &id);
        let sends = Cell::new(0);
        let factories = Cell::new(0);
        assert!(send_then_apply_identity(
            None,
            &mut request,
            &partners,
            || sends.set(sends.get() + 1),
            || {
                factories.set(factories.get() + 1);
                Some(graph.clone())
            }
        ));
        assert_eq!(sends.get(), 1);
        assert_eq!(factories.get(), 0, "no token must not open capture storage");
        assert_eq!(stored(&graph, &id), before);

        let effect = stage_capture(
            MODULE,
            CLAIMS,
            issued(SOURCE, "synthetic-uid", None),
            &request,
            &partners,
            &settings,
        )
        .expect("should stage real effect");
        let available = send_then_apply_identity(
            Some(effect),
            &mut request,
            &partners,
            || {
                assert_eq!(stored(&graph, &id), before);
                sends.set(sends.get() + 1);
            },
            || {
                assert_eq!(sends.get(), 2, "factory must follow response send");
                factories.set(factories.get() + 1);
                None
            },
        );
        assert!(
            !available,
            "unavailable graph suppresses post-send identity work"
        );
        assert_eq!(sends.get(), 2, "each invocation sends exactly once");
        assert_eq!(factories.get(), 1);
        assert_eq!(stored(&graph, &id), before);
        assert_eq!(
            request.kv_snapshot().entry_for(&id),
            Some(&before.0),
            "failed capture must not claim a persisted UID"
        );
    }
}
