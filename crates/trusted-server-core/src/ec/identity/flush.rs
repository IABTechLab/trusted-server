//! Phase 4: the writer request's single KV write, after the response is
//! sent.

use std::collections::BTreeSet;

use http::StatusCode;

use super::coordinator::{
    CallOutcome, Eligibility, EligibilityRequest, EligibilityView, OutcomeContext, Phase,
    eligibility, outcome_to_changes,
};
use super::dispatch::{Call, DispatchRequest, IdentityDeps, dispatch};
use super::finalize::{IdentityFinalize, provider_records, request_signals, writer_facts};
use super::mutation::{ApplyContext, ProviderStateChange, apply};
use super::policy::fingerprint_bits;
use super::revision::consent_fingerprint;
use crate::ec::kv::KvIdentityGraph;
use crate::ec::kv_backend::EcKvWriteMode;
use crate::ec::rate_limiter::RateLimiter;
use crate::ec::registry::PartnerRegistry;
use crate::ec::{EcContext, EcKvSnapshot, log_id};
use crate::platform::RuntimeServices;
use crate::settings::Settings;

/// Wait added for a marked provider that was not dispatched for a reason the
/// live row does not show, such as the burst cap.
const NOT_DISPATCHED_BACKOFF_SECS: u64 = 60;

/// What the after-send flush needs.
pub struct AfterSendInput<'a> {
    pub settings: &'a Settings,
    pub registry: &'a PartnerRegistry,
    pub services: &'a RuntimeServices,
    pub graph: &'a KvIdentityGraph,
    pub context: &'a EcContext,
    /// The response status, captured before the response was sent.
    pub status: StatusCode,
    pub rate_limiter: Option<&'a dyn RateLimiter>,
}

/// How the flush ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, derive_more::Display)]
pub enum FlushOutcome {
    /// Nothing to do: not a returning writer, or nothing queued or marked.
    #[display("skipped")]
    Skipped,
    /// The live row already held every change.
    #[display("no_change")]
    NoChange,
    /// The one conditional write landed.
    #[display("written")]
    Written,
    /// The mutation was dropped: the reason is the label.
    #[display("dropped_{_0}")]
    Dropped(&'static str),
    /// The one write failed and is not retried.
    #[display("write_failed")]
    WriteFailed,
}

fn returning_writer(context: &EcContext) -> bool {
    context.identity().writer() && context.ec_was_present() && !context.ec_generated()
}

fn status_allows_flush(status: StatusCode) -> bool {
    status.is_success() || status == StatusCode::NOT_MODIFIED
}

/// Whether the flush has anything to do. The adapter checks this before it
/// builds a KV graph, so most responses pay nothing after send.
#[must_use]
pub fn after_send_needed(context: &EcContext, status: StatusCode) -> bool {
    returning_writer(context)
        && status_allows_flush(status)
        && (!context.identity().mutation().is_empty() || !context.identity().marked().is_empty())
}

/// Phase 4 (spec 5.2): reuse or re-read the row, dispatch the marked
/// after-send providers, and apply the whole mutation with one
/// `IfGenerationMatch` write that is never retried.
#[must_use]
pub fn run_after_send(input: &AfterSendInput<'_>) -> FlushOutcome {
    let context = input.context;
    if !returning_writer(context) {
        return FlushOutcome::Skipped;
    }
    if !status_allows_flush(input.status) {
        return FlushOutcome::Dropped("status");
    }
    let marked = context.identity().marked();
    if context.identity().mutation().is_empty() && marked.is_empty() {
        return FlushOutcome::Skipped;
    }
    let Some(ec_id) = context.ec_value() else {
        return FlushOutcome::Skipped;
    };
    let Some(finalize) = IdentityFinalize::from_settings(input.settings, input.registry) else {
        return FlushOutcome::Dropped("clock");
    };

    let reused = match context.kv_snapshot() {
        EcKvSnapshot::Present {
            entry,
            generation: Some(generation),
            ..
        } if marked.is_empty() && context.kv_snapshot().belongs_to(ec_id) => {
            Some(((**entry).clone(), *generation))
        }
        _ => None,
    };
    let (live, generation) = match reused {
        Some(found) => found,
        None => match input.graph.get(ec_id) {
            Ok(Some(found)) => found,
            Ok(None) => return FlushOutcome::Dropped("missing"),
            Err(error) => {
                log::warn!(
                    "Identity flush read failed for '{}': {}",
                    log_id(ec_id),
                    error.current_context()
                );
                return FlushOutcome::Dropped("read_failed");
            }
        },
    };
    if !live.consent.ok {
        return FlushOutcome::Dropped("tombstone");
    }

    let mut mutation = context.identity().mutation().clone();
    if !marked.is_empty() {
        dispatch_marked(input, &finalize, &live, &mut mutation);
    }
    if live
        .network
        .as_ref()
        .and_then(|network| network.cluster_size)
        .is_none()
    {
        match input.graph.cluster_size(ec_id, &live) {
            Ok(Some(size)) => mutation.set_cluster_size(size),
            Ok(None) => {}
            Err(error) => log::debug!(
                "Identity flush skipped cluster size: {}",
                error.current_context()
            ),
        }
    }

    let Some(applied) = apply(
        &mutation,
        &live,
        &ApplyContext {
            now: finalize.now,
            keys: &finalize.keys,
            random: finalize.random(),
            catalog: &finalize.catalog,
        },
    ) else {
        return FlushOutcome::Dropped("invalid");
    };
    if !applied.changed {
        return FlushOutcome::NoChange;
    }
    match input.graph.write_identity(
        ec_id,
        &applied.entry,
        EcKvWriteMode::IfGenerationMatch(generation),
    ) {
        Ok(true) => FlushOutcome::Written,
        Ok(false) => {
            log::warn!(
                "Identity flush lost a CAS race for '{}'; staged changes stay in the cookie",
                log_id(ec_id)
            );
            FlushOutcome::WriteFailed
        }
        Err(error) => {
            log::warn!(
                "Identity flush write failed for '{}': {}",
                log_id(ec_id),
                error.current_context()
            );
            FlushOutcome::WriteFailed
        }
    }
}

/// Rotates a sorted list by an hourly offset, so a different provider leads
/// each hour and `max_concurrency` stays fair, as pull sync always has.
fn rotate_hourly<T>(items: &mut [T], now: u64) {
    if items.is_empty() {
        return;
    }
    let hours = usize::try_from(now / 3600).unwrap_or(usize::MAX);
    items.rotate_left(hours % items.len());
}

/// Re-checks eligibility for each marked provider on the live row, so no EC
/// ID is disclosed before consent is confirmed, then dispatches them and adds
/// their results to the mutation.
fn dispatch_marked(
    input: &AfterSendInput<'_>,
    finalize: &IdentityFinalize,
    live: &crate::ec::kv_types::KvEntry,
    mutation: &mut super::mutation::IdentityMutation,
) {
    let context = input.context;
    let mut marked: Vec<_> = context
        .identity()
        .marked()
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    rotate_hourly(&mut marked, finalize.now);
    let batches: Vec<_> = context
        .identity()
        .opened()
        .map(|opened| {
            opened
                .removals
                .cookies
                .iter()
                .map(|(_, batch)| batch.clone())
                .collect()
        })
        .unwrap_or_default();
    let view = EligibilityView::merge(
        Some(live),
        None,
        &batches,
        &finalize.catalog,
        &finalize.keys,
    );
    let facts = writer_facts(context, finalize);
    let request = EligibilityRequest {
        identity: &finalize.identity,
        view: &view,
        facts: &facts,
        consent: context.consent(),
        catalog: &finalize.catalog,
        keys: &finalize.keys,
    };

    let mut calls = Vec::new();
    for id in &marked {
        let Some(provider) = finalize.catalog.get(id) else {
            continue;
        };
        let settings = finalize.catalog.provider_settings(&finalize.identity, id);
        let Eligibility::Eligible { kinds } =
            eligibility(provider.as_ref(), &settings, Phase::AfterSend, &request)
        else {
            continue;
        };
        let (stored, state_json) = provider_records(provider.as_ref(), &view);
        calls.push(Call {
            settings,
            provider: id.clone(),
            kinds,
            stored,
            state_json,
        });
    }
    if calls.is_empty() {
        return;
    }

    let deps = IdentityDeps {
        settings: input.settings,
        catalog: &finalize.catalog,
        keys: &finalize.keys,
        services: input.services,
        rate_limiter: input.rate_limiter,
    };
    let signals = request_signals(
        context,
        finalize,
        &input.settings.publisher.domain,
        input.services.client_info().client_ip,
    );
    let reports = dispatch(
        &DispatchRequest {
            deps: &deps,
            signals: &signals,
            hold_budget: None,
            visitor: context.ec_value(),
        },
        calls,
    );

    for report in reports {
        let Some(provider) = finalize.catalog.get(&report.provider) else {
            continue;
        };
        if report.outcome == CallOutcome::NotDispatched {
            mutation.merge_state(
                report.provider.clone(),
                ProviderStateChange {
                    n: Some(finalize.now + NOT_DISPATCHED_BACKOFF_SECS),
                    ..ProviderStateChange::default()
                },
            );
            continue;
        }
        let settings = finalize
            .catalog
            .provider_settings(&finalize.identity, &report.provider);
        let bits = fingerprint_bits(
            provider.descriptor().consent_policy(),
            context.consent(),
            facts.policy_signals,
        );
        let changes = outcome_to_changes(
            provider.as_ref(),
            report.outcome,
            &view,
            &OutcomeContext {
                now: finalize.now,
                keys: &finalize.keys,
                catalog: &finalize.catalog,
                provider_settings: &settings,
                identity: &finalize.identity,
                expected_state: report.expected_state,
                fingerprint: consent_fingerprint(&finalize.keys, &bits),
            },
        );
        for change in changes.removals.into_iter().chain(changes.positive) {
            mutation.add_change(change);
        }
        if let Some(result) = changes.result {
            mutation.add_result(result);
        }
        mutation.merge_state(report.provider, changes.state);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use error_stack::Report;

    use super::*;
    use crate::consent::ConsentContext;
    use crate::consent::jurisdiction::Jurisdiction;
    use crate::consent::types::ConsentSource;
    use crate::ec::identity::mutation::PartnerIdChange;
    use crate::ec::identity::{IdentityProviderId, SourceDomain};
    use crate::ec::kv_types::{KvEntry, KvNetwork, KvPartnerId};
    use crate::error::TrustedServerError;
    use crate::platform::test_support::{StubHttpClient, build_services_with_http_client};
    use crate::redacted::Redacted;
    use crate::settings::EcPartner;
    use crate::test_support::tests::create_test_settings;

    const PULL_SOURCE: &str = "ssp.example.com";
    const PULL_ID: &str = "pull-ssp-example-com";

    fn ec_id() -> String {
        format!("{}.ABC123", "a".repeat(64))
    }

    fn registry(pull: bool) -> PartnerRegistry {
        let partner = EcPartner {
            name: "SSP".to_owned(),
            source_domain: PULL_SOURCE.to_owned(),
            openrtb_atype: EcPartner::default_openrtb_atype(),
            bidstream_enabled: true,
            api_token: None,
            batch_rate_limit: EcPartner::default_batch_rate_limit(),
            pull_sync_enabled: pull,
            pull_sync_url: pull.then(|| "https://sync.ssp.example.com/pull".to_owned()),
            pull_sync_allowed_domains: if pull {
                vec!["sync.ssp.example.com".to_owned()]
            } else {
                Vec::new()
            },
            pull_sync_ttl_sec: EcPartner::default_pull_sync_ttl_sec(),
            pull_sync_rate_limit: EcPartner::default_pull_sync_rate_limit(),
            ts_pull_token: pull
                .then(|| Redacted::new("example-token-value-0123456789abcdef".to_owned())),
        };
        PartnerRegistry::from_config(&[partner]).expect("should build registry")
    }

    fn row() -> KvEntry {
        let mut entry = KvEntry::minimal("z.example.com", "x", 1);
        entry.network = Some(KvNetwork {
            cluster_size: Some(1),
        });
        entry
    }

    fn writer_context(graph: &KvIdentityGraph, reuse_snapshot: bool) -> EcContext {
        let consent = ConsentContext {
            jurisdiction: Jurisdiction::NonRegulated,
            source: ConsentSource::Cookie,
            ..ConsentContext::default()
        };
        let mut context = EcContext::new_for_test(Some(ec_id()), consent);
        context.identity_mut().set_writer(true);
        if reuse_snapshot {
            context.set_kv_snapshot(graph.load_snapshot(&ec_id()));
        }
        context
    }

    fn queue_fill(context: &mut EcContext) {
        context
            .identity_mut()
            .mutation_mut()
            .add_change(PartnerIdChange::FillMissing {
                source: PULL_SOURCE
                    .parse::<SourceDomain>()
                    .expect("should parse source"),
                record: KvPartnerId::browser("browser-uid", Some(1)),
                writer: None,
                staged_at: None,
            });
    }

    struct Fixture {
        settings: Settings,
        registry: PartnerRegistry,
        stub: Arc<StubHttpClient>,
        services: RuntimeServices,
    }

    impl Fixture {
        fn new(pull: bool) -> Self {
            let stub = Arc::new(StubHttpClient::new());
            let services = build_services_with_http_client(
                Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>
            );
            Self {
                settings: create_test_settings(),
                registry: registry(pull),
                stub,
                services,
            }
        }

        fn run(
            &self,
            graph: &KvIdentityGraph,
            context: &EcContext,
            status: StatusCode,
            limiter: Option<&dyn RateLimiter>,
        ) -> FlushOutcome {
            run_after_send(&AfterSendInput {
                settings: &self.settings,
                registry: &self.registry,
                services: &self.services,
                graph,
                context,
                status,
                rate_limiter: limiter,
            })
        }
    }

    struct AlwaysExceeded;

    impl RateLimiter for AlwaysExceeded {
        fn exceeded(&self, _key: &str, _limit: u32) -> Result<bool, Report<TrustedServerError>> {
            Ok(true)
        }
    }

    #[test]
    fn writer_flushes_its_mutation_once_reusing_the_phase_one_read() {
        let fixture = Fixture::new(false);
        let (graph, counter) = KvIdentityGraph::write_counting("ec-store");
        graph.create(&ec_id(), &row()).expect("should seed");
        let mut context = writer_context(&graph, true);
        queue_fill(&mut context);
        let before = counter.writes();

        let outcome = fixture.run(&graph, &context, StatusCode::OK, None);

        assert_eq!(outcome, FlushOutcome::Written, "should write");
        let after = counter.writes();
        assert_eq!(
            after.if_generation_match - before.if_generation_match,
            1,
            "one CAS write"
        );
        assert_eq!(after.lookups, before.lookups, "reuses the phase 1 read");
        let (stored, _) = graph
            .get(&ec_id())
            .expect("should read")
            .expect("should exist");
        assert_eq!(
            stored.ids[PULL_SOURCE].uid, "browser-uid",
            "the fill landed"
        );
    }

    #[test]
    fn nothing_queued_reads_and_writes_nothing() {
        let fixture = Fixture::new(false);
        let (graph, counter) = KvIdentityGraph::write_counting("ec-store");
        graph.create(&ec_id(), &row()).expect("should seed");
        let context = writer_context(&graph, false);
        let before = counter.writes();

        assert!(
            !after_send_needed(&context, StatusCode::OK),
            "nothing needed"
        );
        assert_eq!(
            fixture.run(&graph, &context, StatusCode::OK, None),
            FlushOutcome::Skipped,
            "skipped"
        );
        assert_eq!(counter.writes(), before, "no read and no write");
    }

    #[test]
    fn non_success_status_drops_the_mutation() {
        let fixture = Fixture::new(false);
        let (graph, counter) = KvIdentityGraph::write_counting("ec-store");
        graph.create(&ec_id(), &row()).expect("should seed");
        let mut context = writer_context(&graph, true);
        queue_fill(&mut context);
        let before = counter.writes();

        for status in [
            StatusCode::FOUND,
            StatusCode::NOT_FOUND,
            StatusCode::BAD_GATEWAY,
        ] {
            assert!(
                !after_send_needed(&context, status),
                "{status} needs nothing"
            );
            assert_eq!(
                fixture.run(&graph, &context, status, None),
                FlushOutcome::Dropped("status"),
                "{status} drops the mutation"
            );
        }
        assert!(
            after_send_needed(&context, StatusCode::NOT_MODIFIED),
            "control: a 304 still flushes"
        );
        assert_eq!(counter.writes(), before, "nothing read or written");
    }

    #[test]
    fn tombstone_on_reread_drops_and_calls_no_provider() {
        let fixture = Fixture::new(true);
        let (graph, _counter) = KvIdentityGraph::write_counting("ec-store");
        graph
            .create(&ec_id(), &KvEntry::tombstone(1))
            .expect("should seed");
        let mut context = writer_context(&graph, false);
        context
            .identity_mut()
            .mark(PULL_ID.parse().expect("should parse id"));

        let outcome = fixture.run(&graph, &context, StatusCode::OK, None);

        assert_eq!(outcome, FlushOutcome::Dropped("tombstone"), "dropped");
        assert!(
            fixture.stub.recorded_request_uris().is_empty(),
            "no provider was called"
        );
    }

    #[test]
    fn marked_pull_partner_dispatches_after_reread_and_its_uid_lands_in_the_one_write() {
        let fixture = Fixture::new(true);
        fixture
            .stub
            .push_response(200, br#"{"uid":"partner-uid"}"#.to_vec());
        let (graph, counter) = KvIdentityGraph::write_counting("ec-store");
        graph.create(&ec_id(), &row()).expect("should seed");
        let mut context = writer_context(&graph, true);
        context
            .identity_mut()
            .mark(PULL_ID.parse().expect("should parse id"));
        let before = counter.writes();

        let outcome = fixture.run(&graph, &context, StatusCode::OK, None);

        assert_eq!(outcome, FlushOutcome::Written, "written");
        assert_eq!(
            counter.writes().lookups - before.lookups,
            1,
            "re-reads once because a provider is marked"
        );
        assert_eq!(
            counter.writes().if_generation_match - before.if_generation_match,
            1,
            "one write"
        );
        assert_eq!(
            fixture.stub.recorded_request_uris().len(),
            1,
            "one partner call"
        );
        let (stored, _) = graph
            .get(&ec_id())
            .expect("should read")
            .expect("should exist");
        let record = &stored.ids[PULL_SOURCE];
        assert_eq!(record.uid, "partner-uid", "the partner UID landed");
        assert_eq!(
            record.w.as_deref(),
            Some(PULL_ID),
            "written by the pull provider"
        );
    }

    #[test]
    fn cas_conflict_is_not_retried() {
        let fixture = Fixture::new(false);
        let (graph, counter) = KvIdentityGraph::write_counting("ec-store");
        graph.create(&ec_id(), &row()).expect("should seed");
        let mut context = writer_context(&graph, true);
        queue_fill(&mut context);
        counter.conflict_next(1);
        let before = counter.writes();

        assert_eq!(
            fixture.run(&graph, &context, StatusCode::OK, None),
            FlushOutcome::WriteFailed,
            "a lost race fails"
        );
        assert_eq!(
            counter.writes().if_generation_match - before.if_generation_match,
            1,
            "one attempt, never retried"
        );
    }

    #[test]
    fn burst_capped_marked_provider_backs_off_a_minute() {
        let fixture = Fixture::new(true);
        let (graph, _counter) = KvIdentityGraph::write_counting("ec-store");
        graph.create(&ec_id(), &row()).expect("should seed");
        let mut context = writer_context(&graph, false);
        context
            .identity_mut()
            .mark(PULL_ID.parse().expect("should parse id"));
        let now = crate::ec::current_timestamp();

        let outcome = fixture.run(&graph, &context, StatusCode::OK, Some(&AlwaysExceeded));

        assert_eq!(outcome, FlushOutcome::Written, "the backoff is written");
        assert!(
            fixture.stub.recorded_request_uris().is_empty(),
            "not dispatched"
        );
        let (stored, _) = graph
            .get(&ec_id())
            .expect("should read")
            .expect("should exist");
        let next = stored.ps[PULL_ID].n.expect("should set n");
        assert!(next >= now + 60, "waits at least a minute");
    }

    #[test]
    fn missing_cluster_size_rides_on_the_one_write() {
        let fixture = Fixture::new(false);
        let (graph, counter) = KvIdentityGraph::write_counting("ec-store");
        let mut entry = row();
        entry.network = None;
        graph.create(&ec_id(), &entry).expect("should seed");
        let mut context = writer_context(&graph, true);
        queue_fill(&mut context);
        let before = counter.writes();

        assert_eq!(
            fixture.run(&graph, &context, StatusCode::OK, None),
            FlushOutcome::Written,
            "written"
        );
        assert_eq!(
            counter.writes().total() - before.total(),
            1,
            "still one write"
        );
        let (stored, _) = graph
            .get(&ec_id())
            .expect("should read")
            .expect("should exist");
        assert!(
            stored
                .network
                .and_then(|network| network.cluster_size)
                .is_some(),
            "cluster size stored"
        );
    }

    #[test]
    fn hourly_rotation_changes_the_leading_provider() {
        let ids: Vec<IdentityProviderId> = ["a", "b", "c"]
            .iter()
            .map(|id| id.parse().expect("should parse id"))
            .collect();
        let mut hour_one = ids.clone();
        rotate_hourly(&mut hour_one, 3600);
        assert_eq!(hour_one[0].as_str(), "b", "hour 1 starts at b");
        let mut hour_three = ids.clone();
        rotate_hourly(&mut hour_three, 3 * 3600);
        assert_eq!(hour_three, ids, "hour 3 wraps");
    }
}
