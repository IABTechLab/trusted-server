//! Phase 4: the writer's one flush after send (spec 5.2).
//!
//! The writer request stages every identity change in one
//! [`IdentityMutation`]. After the response is sent, this module dispatches
//! the after-send providers and writes the whole mutation once, with
//! `IfGenerationMatch`. A failed flush is not retried as a whole; only its
//! removals are written again, alone (spec 5.2 phase 4 step 5).

use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;
use std::time::Duration;

use http::StatusCode;

use super::coordinator::{
    Call, Eligibility, EligibilityInput, EligibilityView, IdentityDeps, OutcomeChanges,
    OutcomeContext, Phase, RequestFacts, dispatch, eligibility, opt_out_clears, outcome_to_changes,
};
use super::descriptor::{Input, Resolves};
use super::ids::{IdentityProviderId, SourceDomain, UnixTime};
use super::input::{ProviderConsent, Region, ResolveSignals, StoredId, ThreadRandom};
use super::mutation::{ApplyContext, IdentityMutation, ProviderStateChange, apply};
use super::policy::{PolicySignals, fingerprint_bits};
use super::provider::IdentityProvider;
use super::removal::{RemovalTiming, log_removal_lost, write_removals};
use super::revision::StateRevision;
use super::runtime::IdentityRuntime;
use crate::consent::allows_eid_persistence;
use crate::ec::kv::KvIdentityGraph;
use crate::ec::kv_backend::EcKvWriteOutcome;
use crate::ec::kv_types::KvEntry;
use crate::ec::rate_limiter::RateLimiter;
use crate::ec::{EcContext, EcKvSnapshot, current_timestamp, log_id};
use crate::platform::RuntimeServices;
use crate::settings::Settings;

pub use super::removal::RemovalWriteOutcome;

/// What phase 4 needs. The catalog and keys come from `runtime`.
pub struct AfterSendInput<'a> {
    /// The identity runtime of this settings load.
    pub runtime: &'a IdentityRuntime,
    /// The settings the request ran with.
    pub settings: &'a Settings,
    /// The request's platform services, for the after-send calls.
    pub services: &'a RuntimeServices,
    /// The identity graph to read and write.
    pub graph: &'a KvIdentityGraph,
    /// The request's EC state, holding the writer decision and the mutation.
    pub context: &'a mut EcContext,
    /// The status of the response that was sent.
    pub status: StatusCode,
    /// The per-visitor burst cap.
    pub rate_limiter: &'a dyn RateLimiter,
}

/// How phase 4 ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlushOutcome {
    /// Not a returning EC's writer, or nothing to do. Nothing was read.
    Skipped,
    /// The mutation changed nothing in the row, so nothing was written.
    NoChange,
    /// The one write landed.
    Written,
    /// The row was a tombstone, missing or unreadable, or the merged row was
    /// invalid. The mutation was dropped and no provider was called after it.
    Dropped(&'static str),
    /// The one write failed. `removals` is how the removal-only retry ended.
    WriteFailed {
        /// The removal-only retry's outcome.
        removals: RemovalWriteOutcome,
    },
}

/// Whether phase 4 can have work for this request: it is a returning EC's
/// writer. A caller checks this before building the identity graph.
#[must_use]
pub fn may_flush(context: &EcContext) -> bool {
    context.identity().writer() && context.ec_was_present() && !context.ec_generated()
}

/// Runs phase 4 for the writer request (spec 5.2, steps 1 to 5).
///
/// It reads the row at most once, calls the due after-send providers only
/// after that read confirms a live, consenting row, and writes at most once
/// with the generation it read. Only a failed write that holds removals adds
/// the removal-only write of spec 5.3.
#[must_use]
pub fn run_after_send(input: AfterSendInput<'_>) -> FlushOutcome {
    let AfterSendInput {
        runtime,
        settings,
        services,
        graph,
        context,
        status,
        rate_limiter,
    } = input;

    let returning_writer = may_flush(context);
    let Some(ec_id) = context.ec_value().map(str::to_owned) else {
        return FlushOutcome::Skipped;
    };
    if !returning_writer {
        return FlushOutcome::Skipped;
    }

    let mut mutation = core::mem::take(context.identity_mut().mutation_mut());
    let status_ok = status.is_success() || status == StatusCode::NOT_MODIFIED;
    if !status_ok {
        mutation.retain_for_error_status();
    }

    let now = current_timestamp();
    // Hold-mode providers run in phase 4 unless the hold dispatch already
    // considered them on a usable row (spec 4.7 step 7).
    let facts = request_facts(context, now, context.identity().hold_dispatched());
    let phase_one = context.kv_snapshot().clone();
    let phase_one_entry = phase_one.entry_for(&ec_id);
    let phase_one_view =
        phase_one_entry.map_or_else(EligibilityView::empty, EligibilityView::from_kv);
    let due = if status_ok {
        due_providers(runtime, settings, context, &phase_one_view, &facts)
    } else {
        Vec::new()
    };
    let needs_cluster_size = phase_one_entry.is_some_and(lacks_cluster_size);
    // A re-grant clears a sticky opt-out on the writer's one flush (spec 8).
    let regranted = opt_out_clears(
        &phase_one_view,
        runtime.catalog(),
        context.consent(),
        facts.policy_signals,
    );

    if mutation.is_empty() && due.is_empty() && !needs_cluster_size && regranted.is_empty() {
        return FlushOutcome::Skipped;
    }

    // Phase 1's read decides when no provider is due (spec 5.2 phase 4
    // step 2). A failed or absent phase 1 read counts as no read.
    let reusable = match &phase_one {
        EcKvSnapshot::Present {
            entry,
            generation: Some(generation),
            ..
        } if due.is_empty() && phase_one.belongs_to(&ec_id) => {
            Some((entry.as_ref().clone(), *generation))
        }
        EcKvSnapshot::Missing { .. } if due.is_empty() && phase_one.belongs_to(&ec_id) => {
            return drop_mutation(&ec_id, "missing");
        }
        _ => None,
    };
    let (live, generation) = match reusable {
        Some(snapshot) => snapshot,
        None => match graph.get(&ec_id) {
            Ok(Some(row)) => row,
            Ok(None) => return drop_mutation(&ec_id, "missing"),
            Err(error) => {
                log::warn!(
                    "Identity flush for '{}' could not read the row: {}",
                    log_id(&ec_id),
                    error.current_context()
                );
                return drop_mutation(&ec_id, "read failed");
            }
        },
    };
    if !live.consent.ok {
        return drop_mutation(&ec_id, "tombstone");
    }

    // The row is live and consenting: only now may the EC ID leave TS.
    let live_view = EligibilityView::from_kv(&live);
    let calls: Vec<Call> = due
        .iter()
        .filter_map(|id| live_call(runtime, settings, context, &live_view, &facts, id))
        .collect();
    if !calls.is_empty() {
        let random = ThreadRandom;
        let deps = runtime.deps(settings, services, &random, Some(rate_limiter));
        // Phase 4 runs after the response is sent, outside any executor, so
        // blocking here is safe. The hold dispatch awaits instead: it runs
        // inside the request handler's executor.
        futures::executor::block_on(add_dispatch_results(
            &mut mutation,
            &deps,
            &DispatchInput {
                context,
                ec_id: &ec_id,
                live: &live,
                view: &live_view,
                facts: &facts,
            },
            DispatchRun {
                calls,
                budget: None,
            },
        ));
    }

    for (provider, watermark) in regranted {
        mutation.merge_state(
            provider,
            ProviderStateChange {
                opt_out_clear: Some(watermark),
                ..ProviderStateChange::default()
            },
        );
    }

    if lacks_cluster_size(&live) {
        match graph.compute_cluster_size(&ec_id) {
            Ok(size) => mutation.set_cluster_size(size),
            Err(error) => log::warn!(
                "Identity flush for '{}' could not compute the cluster size: {}",
                log_id(&ec_id),
                error.current_context()
            ),
        }
    }

    let random = ThreadRandom;
    let apply_context = ApplyContext {
        now,
        random: &random,
        catalog: runtime.catalog(),
    };
    let Some(applied) = apply(&mutation, &live, &apply_context) else {
        return drop_mutation(&ec_id, "invalid");
    };
    if !applied.changed {
        return FlushOutcome::NoChange;
    }

    match graph.write_if_generation(&ec_id, &applied.entry, generation) {
        Ok(EcKvWriteOutcome::Written) => FlushOutcome::Written,
        Ok(EcKvWriteOutcome::PreconditionFailed) => {
            log::warn!(
                "Identity flush for '{}' lost its generation check; not retried",
                log_id(&ec_id)
            );
            write_failed(graph, context, &ec_id, &mutation, &apply_context)
        }
        Err(error) => {
            log::warn!(
                "Identity flush for '{}' failed to write: {}",
                log_id(&ec_id),
                error.current_context()
            );
            write_failed(graph, context, &ec_id, &mutation, &apply_context)
        }
    }
}

fn drop_mutation(ec_id: &str, reason: &'static str) -> FlushOutcome {
    log::debug!(
        "Identity flush for '{}' dropped its mutation: {reason}",
        log_id(ec_id)
    );
    FlushOutcome::Dropped(reason)
}

fn lacks_cluster_size(entry: &KvEntry) -> bool {
    entry.consent.ok
        && entry
            .network
            .as_ref()
            .and_then(|network| network.cluster_size)
            .is_none()
}

/// Writes the failed flush's removals alone, and counts each one every
/// attempt failed to write (spec 5.2 phase 4 step 5).
fn write_failed(
    graph: &KvIdentityGraph,
    context: &mut EcContext,
    ec_id: &str,
    mutation: &IdentityMutation,
    apply_context: &ApplyContext<'_>,
) -> FlushOutcome {
    let removals = mutation.removals();
    if !removals.has_removals() {
        return FlushOutcome::WriteFailed {
            removals: RemovalWriteOutcome::NothingToWrite,
        };
    }
    // Phase 4 runs after send, right after the failed flush.
    let outcome = write_removals(
        graph,
        ec_id,
        &removals,
        apply_context,
        RemovalTiming::AfterSend,
    );
    if outcome == RemovalWriteOutcome::Failed {
        log_removal_lost(ec_id, &removals);
        for _ in removals.removal_labels() {
            context.identity_mut().record_removal_lost();
        }
    }
    FlushOutcome::WriteFailed { removals: outcome }
}

/// Tries once more, after send, the capture removal write that failed before
/// send (spec 5.3).
///
/// This is the last attempt: the pending removals are taken, and each one
/// that still cannot be written is logged and counted as lost. Without a
/// pending removal it does nothing.
pub fn retry_pending_removals(context: &mut EcContext, graph: &KvIdentityGraph) {
    let Some(removals) = context.identity().pending_removals().cloned() else {
        return;
    };
    let ec_id = context.ec_value().map(str::to_owned);
    let runtime = context.identity().runtime().cloned();
    let (Some(ec_id), Some(runtime)) = (ec_id, runtime) else {
        log::warn!("Capture removal retry skipped: no EC ID or identity runtime");
        lose_pending_removals(context);
        return;
    };
    let random = ThreadRandom;
    let outcome = write_removals(
        graph,
        &ec_id,
        &removals,
        &ApplyContext {
            now: current_timestamp(),
            random: &random,
            catalog: runtime.catalog(),
        },
        RemovalTiming::AfterSend,
    );
    if outcome == RemovalWriteOutcome::Failed {
        lose_pending_removals(context);
    } else {
        context.identity_mut().set_pending_removals(None);
    }
}

/// Gives up the capture removal write that failed before send: logs and
/// counts each removal as lost. For an after-send run that cannot even try,
/// such as one without an identity graph.
pub fn lose_pending_removals(context: &mut EcContext) {
    let Some(removals) = context.identity().pending_removals().cloned() else {
        return;
    };
    context.identity_mut().set_pending_removals(None);
    log_removal_lost(context.ec_value().unwrap_or_default(), &removals);
    for _ in removals.removal_labels() {
        context.identity_mut().record_removal_lost();
    }
}

/// What eligibility reads about the request itself.
pub(super) fn request_facts(context: &EcContext, now: u64, runs_auction: bool) -> RequestFacts {
    let identity = context.identity();
    let mut available_inputs = BTreeSet::new();
    for (input, present) in [
        (Input::ClientIp, client_ip(context).is_some()),
        (Input::UserAgent, identity.user_agent().is_some()),
        (Input::PageUrl, identity.page_url().is_some()),
        (Input::Region, region(context).is_some()),
    ] {
        if present {
            available_inputs.insert(input);
        }
    }
    RequestFacts {
        is_real_browser: identity.is_real_browser(),
        ec_persistence_allowed: context.ec_allowed() && allows_eid_persistence(context.consent()),
        policy_signals: PolicySignals {
            dnt: identity.dnt(),
        },
        writer: identity.writer(),
        new_visitor: context.ec_generated(),
        runs_auction,
        now: UnixTime::from_secs(now),
        available_inputs,
    }
}

pub(super) fn client_ip(context: &EcContext) -> Option<IpAddr> {
    context.client_ip().and_then(|ip| ip.parse().ok())
}

pub(super) fn region(context: &EcContext) -> Option<Region> {
    context
        .geo_info()
        .and_then(|geo| Region::from_geo(&geo.country, geo.region.as_deref()))
}

/// The eligibility of `provider` for phase 4 against `view`.
fn after_send_eligibility(
    provider: &dyn IdentityProvider,
    runtime: &IdentityRuntime,
    settings: &Settings,
    context: &EcContext,
    view: &EligibilityView,
    facts: &RequestFacts,
) -> Eligibility {
    let provider_settings = settings.ec.identity.provider(provider.descriptor().id());
    eligibility(
        &EligibilityInput {
            provider,
            settings: &provider_settings,
            identity: &settings.ec.identity,
            catalog: runtime.catalog(),
            consent: context.consent(),
        },
        view,
        facts,
        Phase::AfterSend,
    )
}

/// The after-send providers due on the phase 1 view.
fn due_providers(
    runtime: &IdentityRuntime,
    settings: &Settings,
    context: &EcContext,
    view: &EligibilityView,
    facts: &RequestFacts,
) -> Vec<IdentityProviderId> {
    runtime
        .catalog()
        .providers()
        .filter(|provider| {
            matches!(
                after_send_eligibility(provider.as_ref(), runtime, settings, context, view, facts),
                Eligibility::Eligible { .. }
            )
        })
        .map(|provider| provider.descriptor().id().clone())
        .collect()
}

/// The call of a provider still due on the live row, with its live `ps.st`.
fn live_call(
    runtime: &IdentityRuntime,
    settings: &Settings,
    context: &EcContext,
    view: &EligibilityView,
    facts: &RequestFacts,
    id: &IdentityProviderId,
) -> Option<Call> {
    let provider = runtime.catalog().get(id)?;
    let Eligibility::Eligible { kinds } =
        after_send_eligibility(provider.as_ref(), runtime, settings, context, view, facts)
    else {
        return None;
    };
    Some(Call {
        provider: id.clone(),
        kinds,
        state_json: view
            .state(id)
            .and_then(|state| serde_json::to_string(state).ok()),
    })
}

/// What dispatching the due providers reads.
pub(super) struct DispatchInput<'a> {
    pub(super) context: &'a EcContext,
    pub(super) ec_id: &'a str,
    pub(super) live: &'a KvEntry,
    pub(super) view: &'a EligibilityView,
    pub(super) facts: &'a RequestFacts,
}

/// The calls to send and, for a hold dispatch before the auction, the budget
/// they share.
pub(super) struct DispatchRun {
    pub(super) calls: Vec<Call>,
    pub(super) budget: Option<Duration>,
}

/// Dispatches `run.calls` and adds their outcomes to `mutation`.
pub(super) async fn add_dispatch_results(
    mutation: &mut IdentityMutation,
    deps: &IdentityDeps<'_>,
    input: &DispatchInput<'_>,
    run: DispatchRun,
) {
    let DispatchRun { calls, budget } = run;
    let resolving: BTreeSet<&SourceDomain> =
        calls.iter().flat_map(|call| call.kinds.keys()).collect();
    let stored: BTreeMap<SourceDomain, StoredId> = input
        .view
        .records()
        .filter(|(source, _)| resolving.contains(source))
        .filter_map(|(source, record)| {
            StoredId::from_record(source, record).map(|stored| (source.clone(), stored))
        })
        .collect();
    let identity = input.context.identity();
    let signals = ResolveSignals {
        client_ip: client_ip(input.context),
        user_agent: identity.user_agent(),
        page_url: identity.page_url().cloned(),
        region: region(input.context),
        hashed_email: None,
        consent: ProviderConsent::from_context(input.context.consent()),
        publisher_domain: &deps.settings.publisher.domain,
        now: input.facts.now,
        random: deps.random,
        ec_id: Some(input.ec_id.to_owned()),
        stored,
    };
    let catalog = deps.catalog;
    for (id, outcome) in dispatch(deps, calls, &signals, budget).await {
        let Some(provider) = catalog.get(&id) else {
            continue;
        };
        let provider: &dyn IdentityProvider = provider.as_ref();
        let descriptor = provider.descriptor();
        let provider_settings = deps.settings.ec.identity.provider(&id);
        let fingerprint = fingerprint_bits(
            descriptor.consent_policy(),
            input.context.consent(),
            input.facts.policy_signals,
        );
        let stateful = descriptor
            .resolves()
            .is_some_and(Resolves::has_private_state);
        let outcome_context = OutcomeContext {
            now: input.facts.now,
            catalog,
            settings: &provider_settings,
            identity: &deps.settings.ec.identity,
            expected_state: stateful.then(|| {
                StateRevision::of(
                    input
                        .live
                        .ps
                        .get(id.as_str())
                        .and_then(|state| state.st.as_ref()),
                )
            }),
            fingerprint: &fingerprint,
        };
        let found = outcome.is_found();
        let changes = outcome_to_changes(provider, outcome, input.view, &outcome_context);
        add_outcome(mutation, &id, changes, found);
    }
}

/// Adds one outcome's changes to the mutation.
///
/// A `found` outcome's state change is tagged, so an error status drops its
/// `n` and `f` with its positive answer (spec 5.1).
fn add_outcome(
    mutation: &mut IdentityMutation,
    id: &IdentityProviderId,
    changes: OutcomeChanges,
    found: bool,
) {
    let OutcomeChanges {
        changes,
        result,
        state,
    } = changes;
    for change in changes {
        mutation.add_change(change);
    }
    if let Some(result) = result {
        mutation.add_result(result);
    }
    if found {
        mutation.merge_found_state(id.clone(), state);
    } else {
        mutation.merge_state(id.clone(), state);
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::sync::Arc;

    use error_stack::Report;

    use super::*;
    use crate::consent::types::GppConsent;
    use crate::ec::identity::denials::{denials, filter_browser_candidates};
    use crate::ec::identity::mutation::PartnerIdChange;
    use crate::ec::identity::removal::PER_KEY_WRITE_WINDOW;
    use crate::ec::identity::request::WriterDecision;
    use crate::ec::identity::revision::{record_revision, uid_hash, visitor_key};
    use crate::ec::identity::test_support::fake_http::{
        FakeBackend, FakeHttpClient, fake_services,
    };
    use crate::ec::identity::test_support::{
        TEST_EC_ID, WriteCountingEcKv, allowing_consent, ec_partner, example_entry_set,
        full_record, pid, record_for_test, source,
    };
    use crate::ec::kv_types::{KvNetwork, KvOptOut, KvPartnerId, KvProviderState};
    use crate::error::TrustedServerError;
    use crate::openrtb::{Eid, Uid};
    use crate::settings::{ProviderMode, ProviderSettings};
    use crate::test_support::tests::create_test_settings;

    const PULL_HOST: &str = "sync.ssp.example.com";
    const PULL_SOURCE: &str = "ssp.example.com";
    const PULL_ID: &str = "pull-ssp-example-com";

    /// Answers every burst-cap read with `exceeded`, and records the keys.
    struct ScriptedLimiter {
        exceeded: bool,
        keys: RefCell<Vec<String>>,
    }

    impl ScriptedLimiter {
        fn allowing() -> Self {
            Self {
                exceeded: false,
                keys: RefCell::new(Vec::new()),
            }
        }

        fn capped() -> Self {
            Self {
                exceeded: true,
                keys: RefCell::new(Vec::new()),
            }
        }
    }

    impl RateLimiter for ScriptedLimiter {
        fn exceeded(&self, key: &str, _limit: u32) -> Result<bool, Report<TrustedServerError>> {
            self.keys.borrow_mut().push(key.to_owned());
            Ok(self.exceeded)
        }

        fn record(&self, _key: &str) -> Result<(), Report<TrustedServerError>> {
            Ok(())
        }

        fn count(&self, _key: &str) -> Result<u32, Report<TrustedServerError>> {
            Ok(0)
        }
    }

    struct Fixture {
        settings: Settings,
        runtime: IdentityRuntime,
        client: Arc<FakeHttpClient>,
        services: RuntimeServices,
        graph: KvIdentityGraph,
        store: Arc<WriteCountingEcKv>,
        context: EcContext,
    }

    impl Fixture {
        /// A returning writer whose phase 1 read found `row`. With
        /// `pull_partner`, one generic pull partner is configured.
        fn new(row: Option<&KvEntry>, pull_partner: bool) -> Self {
            Self::with_settings(row, Self::settings(pull_partner))
        }

        fn settings(pull_partner: bool) -> Settings {
            let mut settings = create_test_settings();
            settings.ec.partners = if pull_partner {
                vec![ec_partner(
                    PULL_SOURCE,
                    &format!("https://{PULL_HOST}/pull"),
                    "example-token-value-0123456789abcdef",
                )]
            } else {
                Vec::new()
            };
            settings
        }

        fn with_settings(row: Option<&KvEntry>, settings: Settings) -> Self {
            let runtime = IdentityRuntime::from_settings(
                &settings,
                &crate::integrations::IntegrationRegistry::default(),
            )
            .expect("should build runtime");
            let client = Arc::new(FakeHttpClient::new());
            let backend = Arc::new(FakeBackend::new());
            let services = fake_services(&client, &backend);
            let (graph, store) = KvIdentityGraph::write_counting("flush-store");
            let mut context = EcContext::new_for_test_with_ip(
                Some(TEST_EC_ID.to_owned()),
                allowing_consent(),
                Some("192.0.2.10".to_owned()),
            );
            context.identity_mut().set_writer_decision(WriterDecision {
                writer: true,
                is_real_browser: true,
                user_agent: Some("ExampleBrowser/1.0".to_owned()),
                dnt: false,
                page_url: None,
            });
            if let Some(row) = row {
                graph.create(TEST_EC_ID, row).expect("should seed the row");
                context.set_kv_snapshot(graph.load_snapshot(TEST_EC_ID));
            }
            store.reset_counts();
            Self {
                settings,
                runtime,
                client,
                services,
                graph,
                store,
                context,
            }
        }

        fn run(&mut self, limiter: &ScriptedLimiter) -> FlushOutcome {
            self.run_with_status(limiter, StatusCode::OK)
        }

        fn run_with_status(
            &mut self,
            limiter: &ScriptedLimiter,
            status: StatusCode,
        ) -> FlushOutcome {
            run_after_send(AfterSendInput {
                runtime: &self.runtime,
                settings: &self.settings,
                services: &self.services,
                graph: &self.graph,
                context: &mut self.context,
                status,
                rate_limiter: limiter,
            })
        }

        fn stage(&mut self, change: PartnerIdChange) {
            self.context
                .identity_mut()
                .mutation_mut()
                .add_change(change);
        }

        fn stage_sharedid(&mut self) {
            self.stage(PartnerIdChange::FillMissing {
                source: source("sharedid.org"),
                record: KvPartnerId::browser("shared-uid", Some(current_timestamp())),
            });
        }

        fn row(&self) -> KvEntry {
            self.graph
                .get(TEST_EC_ID)
                .expect("should read the row")
                .expect("should hold the row")
                .0
        }
    }

    /// A live, consenting row; `cluster_size` is set unless `sized` is false.
    fn live_row(sized: bool) -> KvEntry {
        let mut entry = KvEntry::new(&allowing_consent(), None, 1_000, "test-publisher.com");
        if sized {
            entry.network = Some(KvNetwork {
                cluster_size: Some(1),
            });
        }
        entry
    }

    fn pull_answers(fixture: &Fixture, uid: &str) {
        fixture
            .client
            .respond(PULL_HOST, 200, format!(r#"{{"uid":"{uid}"}}"#).as_bytes());
    }

    #[test]
    fn writer_flushes_mutation_once_with_generation_match() {
        let mut fixture = Fixture::new(Some(&live_row(true)), false);
        fixture.stage_sharedid();

        let outcome = fixture.run(&ScriptedLimiter::allowing());

        assert_eq!(outcome, FlushOutcome::Written, "should write the mutation");
        let writes = fixture.store.writes();
        assert_eq!(
            writes.if_generation_match, 1,
            "should write once with a CAS"
        );
        assert_eq!(writes.total(), 1, "should make no other write");
        assert_eq!(writes.lookups, 0, "should reuse the phase 1 read");
        assert!(
            fixture.row().ids.contains_key("sharedid.org"),
            "should store the sharedId fill"
        );
    }

    #[test]
    fn writer_with_nothing_to_do_reads_and_writes_nothing() {
        let mut fixture = Fixture::new(Some(&live_row(true)), false);

        let outcome = fixture.run(&ScriptedLimiter::allowing());

        assert_eq!(outcome, FlushOutcome::Skipped, "should have nothing to do");
        let writes = fixture.store.writes();
        assert_eq!(
            (writes.lookups, writes.total()),
            (0, 0),
            "should touch no KV"
        );
    }

    #[test]
    fn non_2xx_or_304_status_drops_ingest_and_dispatches_nothing() {
        let mut fixture = Fixture::new(Some(&live_row(true)), true);
        pull_answers(&fixture, "pulled-uid");
        fixture.stage_sharedid();

        let outcome = fixture.run_with_status(
            &ScriptedLimiter::allowing(),
            StatusCode::INTERNAL_SERVER_ERROR,
        );

        assert_eq!(outcome, FlushOutcome::Skipped, "should drop the ingest");
        assert!(
            fixture.client.sent_hosts().is_empty(),
            "should call no provider"
        );
        let writes = fixture.store.writes();
        assert_eq!(
            (writes.lookups, writes.total()),
            (0, 0),
            "should touch no KV"
        );
    }

    #[test]
    fn not_modified_status_still_flushes() {
        let mut fixture = Fixture::new(Some(&live_row(true)), false);
        fixture.stage_sharedid();

        let outcome =
            fixture.run_with_status(&ScriptedLimiter::allowing(), StatusCode::NOT_MODIFIED);

        assert_eq!(
            outcome,
            FlushOutcome::Written,
            "should treat 304 as success"
        );
    }

    #[test]
    fn tombstone_on_reread_drops_mutation_and_calls_no_provider() {
        let mut fixture = Fixture::new(Some(&live_row(true)), true);
        pull_answers(&fixture, "leaked-uid");
        fixture.stage_sharedid();
        let tombstoned = fixture
            .graph
            .write_withdrawal_tombstone(TEST_EC_ID, drop)
            .expect("should tombstone the row");
        assert_eq!(
            tombstoned,
            crate::ec::kv::TombstoneOutcome::Written,
            "should tombstone the live row"
        );
        fixture.store.reset_counts();

        let outcome = fixture.run(&ScriptedLimiter::allowing());

        assert_eq!(
            outcome,
            FlushOutcome::Dropped("tombstone"),
            "should drop the mutation on a tombstone"
        );
        assert!(
            fixture.client.sent_hosts().is_empty(),
            "should disclose the EC ID to no provider"
        );
        assert_eq!(fixture.store.writes().total(), 0, "should write nothing");
        assert!(
            fixture.row().ids.is_empty(),
            "should leave the tombstone bare"
        );
    }

    #[test]
    fn failed_phase_one_read_rereads_before_writing() {
        let mut fixture = Fixture::new(Some(&live_row(true)), false);
        fixture.context.set_kv_snapshot(EcKvSnapshot::Failed {
            ec_id: TEST_EC_ID.to_owned(),
        });
        fixture.stage_sharedid();

        let outcome = fixture.run(&ScriptedLimiter::allowing());

        assert_eq!(
            outcome,
            FlushOutcome::Written,
            "should write after the re-read"
        );
        let writes = fixture.store.writes();
        assert_eq!(writes.lookups, 1, "should re-read once");
        assert_eq!(writes.if_generation_match, 1, "should write once");
    }

    #[test]
    fn ingest_queued_against_an_unread_snapshot_is_written_after_a_read() {
        let mut fixture = Fixture::new(Some(&live_row(true)), false);
        fixture.context.set_kv_snapshot(EcKvSnapshot::NotRead);
        fixture.stage_sharedid();

        let outcome = fixture.run(&ScriptedLimiter::allowing());

        assert_eq!(outcome, FlushOutcome::Written, "should write the ingest");
        assert_eq!(
            fixture.store.writes().lookups,
            1,
            "should read before writing"
        );
        assert!(
            fixture.row().ids.contains_key("sharedid.org"),
            "should store the sharedId the writer queued"
        );
    }

    #[test]
    fn due_generic_pull_provider_dispatches_after_reread_and_its_uid_lands_in_the_same_write() {
        let mut fixture = Fixture::new(Some(&live_row(true)), true);
        pull_answers(&fixture, "pulled-uid");
        fixture.stage_sharedid();

        let outcome = fixture.run(&ScriptedLimiter::allowing());

        assert_eq!(outcome, FlushOutcome::Written, "should write once");
        let writes = fixture.store.writes();
        assert_eq!(writes.lookups, 1, "should re-read before dispatch");
        assert_eq!(writes.total(), 1, "should make one write");
        assert_eq!(
            fixture.client.sent_hosts(),
            [PULL_HOST],
            "should call the partner"
        );
        let row = fixture.row();
        let record = row
            .ids
            .get(PULL_SOURCE)
            .expect("should store the pulled UID");
        assert_eq!(record.eids.len(), 1, "should store a one-entry record");
        assert_eq!(
            record.first_uid(),
            Some("pulled-uid"),
            "should store the UID"
        );
        assert_eq!(
            record.w.as_deref(),
            Some(PULL_ID),
            "should name the provider"
        );
        assert!(
            row.ids.contains_key("sharedid.org"),
            "should land the ingest too"
        );
    }

    #[test]
    fn generic_pull_results_join_the_single_flush() {
        let mut settings = Fixture::settings(true);
        settings.ec.partners.push(ec_partner(
            "dsp.example.com",
            "https://sync.dsp.example.com/pull",
            "example-token-value-0123456789abcdef",
        ));
        settings.ec.identity.max_concurrency = 1;
        let mut fixture = Fixture::with_settings(Some(&live_row(true)), settings);
        pull_answers(&fixture, "pulled-uid");
        fixture
            .client
            .respond("sync.dsp.example.com", 200, br#"{"uid":"other-uid"}"#);

        let outcome = fixture.run(&ScriptedLimiter::allowing());

        assert_eq!(outcome, FlushOutcome::Written, "should write once");
        assert_eq!(fixture.store.writes().total(), 1, "should make one write");
        let row = fixture.row();
        assert!(
            row.ids.contains_key(PULL_SOURCE) && row.ids.contains_key("dsp.example.com"),
            "should land both partners' UIDs in the one write"
        );
    }

    #[test]
    fn hold_provider_on_a_navigation_without_an_auction_runs_in_phase_four() {
        let mut settings = Fixture::settings(true);
        settings.ec.identity.providers.insert(
            PULL_ID.to_owned(),
            ProviderSettings {
                mode: ProviderMode::Hold,
                ..ProviderSettings::default()
            },
        );
        let mut fixture = Fixture::with_settings(Some(&live_row(true)), settings);
        pull_answers(&fixture, "pulled-uid");

        let outcome = fixture.run(&ScriptedLimiter::allowing());

        assert_eq!(outcome, FlushOutcome::Written, "should write the result");
        assert_eq!(
            fixture.client.sent_hosts(),
            [PULL_HOST],
            "should call the provider"
        );
    }

    #[test]
    fn cas_conflict_is_not_retried_as_a_whole() {
        let mut fixture = Fixture::new(Some(&live_row(true)), false);
        fixture.stage_sharedid();
        fixture.store.refuse_conditional_writes(TEST_EC_ID, 1);

        let outcome = fixture.run(&ScriptedLimiter::allowing());

        assert_eq!(
            outcome,
            FlushOutcome::WriteFailed {
                removals: RemovalWriteOutcome::NothingToWrite
            },
            "should fail without a removal retry"
        );
        assert_eq!(
            fixture.store.writes().if_generation_match,
            1,
            "should attempt the flush once"
        );
    }

    /// A row holding `PULL_SOURCE`, and a mutation with a fill, a removal of
    /// that record and an opt-out of the pull provider.
    fn removal_fixture() -> (Fixture, u64) {
        let mut row = live_row(true);
        let record = record_for_test("old-uid", Some(PULL_ID), 900);
        let expected = record_revision(&record);
        row.ids.insert(PULL_SOURCE.to_owned(), record);
        let mut fixture = Fixture::new(Some(&row), true);
        fixture.stage_sharedid();
        fixture.stage(PartnerIdChange::OwnerRemove {
            source: source(PULL_SOURCE),
            provider: pid(PULL_ID),
            expected,
        });
        let at = current_timestamp();
        fixture.context.identity_mut().mutation_mut().merge_state(
            pid(PULL_ID),
            ProviderStateChange {
                opt_out: Some(KvOptOut {
                    at,
                    fp: "0".to_owned(),
                }),
                ..ProviderStateChange::default()
            },
        );
        (fixture, at)
    }

    #[test]
    fn failed_flush_writes_its_removals_alone() {
        let (mut fixture, at) = removal_fixture();
        fixture.store.refuse_conditional_writes(TEST_EC_ID, 1);

        let outcome = fixture.run(&ScriptedLimiter::allowing());

        assert_eq!(
            outcome,
            FlushOutcome::WriteFailed {
                removals: RemovalWriteOutcome::Written
            },
            "should write the removals alone"
        );
        assert_eq!(
            fixture.store.writes().if_generation_match,
            2,
            "should make one flush attempt and one removal write"
        );
        let row = fixture.row();
        assert!(
            !row.ids.contains_key(PULL_SOURCE),
            "should apply the removal"
        );
        assert!(
            !row.ids.contains_key("sharedid.org"),
            "should drop the fill"
        );
        assert_eq!(
            row.ps
                .get(PULL_ID)
                .and_then(|state| state.o.as_ref())
                .map(|o| o.at),
            Some(at),
            "should store the opt-out"
        );
    }

    #[test]
    fn failed_flush_waits_the_per_key_window_before_its_removal_retry() {
        let (mut fixture, _) = removal_fixture();
        fixture.store.refuse_conditional_writes(TEST_EC_ID, 1);

        fixture.run(&ScriptedLimiter::allowing());

        assert_eq!(
            fixture.store.pauses(),
            vec![PER_KEY_WRITE_WINDOW],
            "should wait one per-key window after the failed flush"
        );
    }

    #[test]
    fn written_flush_never_waits() {
        let (mut fixture, _) = removal_fixture();

        fixture.run(&ScriptedLimiter::allowing());

        assert!(
            fixture.store.pauses().is_empty(),
            "should not wait when the flush is written"
        );
    }

    #[test]
    fn failed_removal_retry_counts_removal_lost() {
        let (mut fixture, _) = removal_fixture();
        fixture
            .store
            .refuse_conditional_writes(TEST_EC_ID, usize::MAX);

        let outcome = fixture.run(&ScriptedLimiter::allowing());

        assert_eq!(
            outcome,
            FlushOutcome::WriteFailed {
                removals: RemovalWriteOutcome::Failed
            },
            "should lose the removals"
        );
        assert_eq!(
            fixture.store.writes().if_generation_match,
            1 + 5,
            "should make one flush attempt and five removal attempts"
        );
        assert_eq!(
            fixture.context.identity().removal_lost(),
            2,
            "should count the removal and the opt-out"
        );
    }

    #[test]
    fn burst_capped_provider_gets_sixty_second_backoff_in_the_write() {
        let mut fixture = Fixture::new(Some(&live_row(true)), true);
        pull_answers(&fixture, "pulled-uid");
        let before = current_timestamp();

        let outcome = fixture.run(&ScriptedLimiter::capped());

        assert_eq!(outcome, FlushOutcome::Written, "should write the backoff");
        assert!(
            fixture.client.sent_hosts().is_empty(),
            "should not call the provider"
        );
        let next = fixture
            .row()
            .ps
            .get(PULL_ID)
            .and_then(|state| state.n)
            .expect("should store n");
        assert!(
            (before + 60..=current_timestamp() + 60).contains(&next),
            "should wait one 60-second window"
        );
    }

    #[test]
    fn missing_cluster_size_is_added_to_the_one_write() {
        let mut fixture = Fixture::new(Some(&live_row(false)), false);

        let outcome = fixture.run(&ScriptedLimiter::allowing());

        assert_eq!(outcome, FlushOutcome::Written, "should write the size");
        assert_eq!(fixture.store.writes().total(), 1, "should write once");
        assert_eq!(
            fixture
                .row()
                .network
                .and_then(|network| network.cluster_size),
            Some(1),
            "should store the cluster size"
        );
    }

    #[test]
    fn untouched_entry_set_is_preserved_by_the_flush() {
        let mut row = live_row(true);
        let multi = full_record(Some("other"), example_entry_set());
        row.ids.insert("multi.example.com".to_owned(), multi);
        let mut fixture = Fixture::new(Some(&row), false);
        let live = fixture
            .graph
            .get(TEST_EC_ID)
            .expect("should read")
            .expect("should hold the row")
            .0;
        fixture.store.reset_counts();
        fixture.stage_sharedid();

        let outcome = fixture.run(&ScriptedLimiter::allowing());

        assert_eq!(outcome, FlushOutcome::Written, "should write the fill");
        assert_eq!(
            fixture.row().ids.get("multi.example.com"),
            live.ids.get("multi.example.com"),
            "should keep the untouched record field for field"
        );
    }

    #[test]
    fn generic_pull_runs_only_on_returning_writer() {
        let mut stager = Fixture::new(Some(&live_row(true)), true);
        pull_answers(&stager, "pulled-uid");
        stager
            .context
            .identity_mut()
            .set_writer_decision(WriterDecision {
                writer: false,
                is_real_browser: true,
                user_agent: None,
                dnt: false,
                page_url: None,
            });

        assert_eq!(
            stager.run(&ScriptedLimiter::allowing()),
            FlushOutcome::Skipped,
            "should skip a stager"
        );
        assert!(
            stager.client.sent_hosts().is_empty(),
            "should call no partner"
        );
    }

    #[test]
    fn new_visitor_reaches_pull_partners_from_second_page_load() {
        let mut first = Fixture::new(None, true);
        pull_answers(&first, "pulled-uid");
        let snapshot = EcKvSnapshot::Present {
            ec_id: TEST_EC_ID.to_owned(),
            entry: Box::new(live_row(true)),
            generation: None,
        };
        first
            .context
            .replace_with_generated(TEST_EC_ID.to_owned(), snapshot);

        assert_eq!(
            first.run(&ScriptedLimiter::allowing()),
            FlushOutcome::Skipped,
            "should run no provider on the first navigation"
        );
        assert!(
            first.client.sent_hosts().is_empty(),
            "should call no partner"
        );

        let mut second = Fixture::new(Some(&live_row(true)), true);
        pull_answers(&second, "pulled-uid");
        assert_eq!(
            second.run(&ScriptedLimiter::allowing()),
            FlushOutcome::Written,
            "should pull on the second navigation"
        );
        assert_eq!(
            second.client.sent_hosts(),
            [PULL_HOST],
            "should call the partner"
        );
    }

    #[test]
    fn failing_pull_partner_backs_off_through_ps() {
        let mut fixture = Fixture::new(Some(&live_row(true)), true);
        fixture.client.respond(PULL_HOST, 503, b"");

        let outcome = fixture.run(&ScriptedLimiter::allowing());

        assert_eq!(outcome, FlushOutcome::Written, "should write the backoff");
        let row = fixture.row();
        let state = row.ps.get(PULL_ID).expect("should store provider state");
        assert_eq!(state.f, 1, "should count the failure");
        assert!(
            state.n.is_some_and(|next| next > current_timestamp()),
            "should push n into the future"
        );

        let mut again = Fixture::new(Some(&row), true);
        again.client.respond(PULL_HOST, 503, b"");
        assert_eq!(
            again.run(&ScriptedLimiter::allowing()),
            FlushOutcome::Skipped,
            "should not retry the partner before n"
        );
        assert!(
            again.client.sent_hosts().is_empty(),
            "should call no partner"
        );
    }

    #[test]
    fn rate_limit_key_is_per_visitor_not_per_ip() {
        let mut fixture = Fixture::new(Some(&live_row(true)), true);
        pull_answers(&fixture, "pulled-uid");
        let limiter = ScriptedLimiter::allowing();

        let _ = fixture.run(&limiter);

        let expected = format!(
            "identity:{PULL_ID}:{}",
            visitor_key(fixture.runtime.keys(), TEST_EC_ID)
        );
        assert_eq!(*limiter.keys.borrow(), [expected], "should key by visitor");
    }

    #[test]
    fn eid_persistence_denied_dispatches_no_pull_partner() {
        let mut fixture = Fixture::new(Some(&live_row(true)), true);
        pull_answers(&fixture, "pulled-uid");
        *fixture.context.consent_mut() = crate::test_support::tests::purpose_one_only_consent();

        let outcome = fixture.run(&ScriptedLimiter::allowing());

        assert_eq!(outcome, FlushOutcome::Skipped, "should have nothing to do");
        assert!(
            fixture.client.sent_hosts().is_empty(),
            "should call no partner"
        );
    }

    #[test]
    fn pull_partner_with_a_stored_uid_is_not_called() {
        let mut row = live_row(true);
        row.ids
            .insert(PULL_SOURCE.to_owned(), KvPartnerId::new("already-known"));
        let mut fixture = Fixture::new(Some(&row), true);
        pull_answers(&fixture, "pulled-uid");

        let outcome = fixture.run(&ScriptedLimiter::allowing());

        assert_eq!(outcome, FlushOutcome::Skipped, "should have nothing to do");
        assert!(
            fixture.client.sent_hosts().is_empty(),
            "should call no partner"
        );
        assert_eq!(fixture.store.writes().lookups, 0, "should not re-read");
    }

    #[test]
    fn regrant_clear_empties_ov() {
        let held = "browser-held-uid";
        let mut row = live_row(true);
        row.ps.insert(
            PULL_ID.to_owned(),
            KvProviderState {
                o: Some(KvOptOut {
                    at: 1_000,
                    fp: "800".to_owned(),
                }),
                ov: vec![uid_hash(held)],
                ..KvProviderState::default()
            },
        );
        let browser = Eid {
            source: PULL_SOURCE.to_owned(),
            uids: vec![Uid {
                id: held.to_owned(),
                atype: Some(1),
                ext: None,
            }],
            inserter: None,
            matcher: None,
            mm: None,
            ext: None,
        };
        let mut fixture = Fixture::new(Some(&row), true);
        let before = denials(&row, fixture.runtime.catalog());
        let mut consent = allowing_consent();
        consent.gpp = Some(GppConsent {
            version: 1,
            section_ids: vec![7],
            eu_tcf: None,
            us_sale_opt_out: Some(false),
            us_sharing_opt_out: None,
            us_targeted_advertising_opt_out: None,
            us_gpc: None,
            us_known_child: None,
            us_privacy_section: None,
            us_section_decode_failed: false,
        });
        *fixture.context.consent_mut() = consent;

        let outcome = fixture.run(&ScriptedLimiter::allowing());

        assert_eq!(outcome, FlushOutcome::Written, "should flush the clear");
        assert_eq!(
            fixture.store.writes().if_generation_match,
            1,
            "should ride the one write"
        );
        let stored = fixture.row();
        let state = stored
            .ps
            .get(PULL_ID)
            .expect("should keep the provider state");
        assert_eq!(
            state.oc,
            Some(1_000),
            "should store the clear with the opt-out it read as its watermark"
        );
        assert!(state.ov.is_empty(), "should empty ps.ov");
        assert!(
            filter_browser_candidates(core::slice::from_ref(&browser), &before).is_empty(),
            "control: before the clear the browser UID should be denied"
        );
        let after = denials(&stored, fixture.runtime.catalog());
        assert_eq!(
            filter_browser_candidates(core::slice::from_ref(&browser), &after),
            vec![browser],
            "should send the browser UID again after the clear"
        );
    }
}
