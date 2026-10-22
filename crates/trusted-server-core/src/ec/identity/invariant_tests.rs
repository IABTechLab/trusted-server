//! The one-write-per-page-load invariant, end to end through the same core
//! steps the adapter runs: the create before routing, `ec_finalize_response`
//! before send, and `run_after_send` after it.

use std::sync::Arc;

use edgezero_core::body::Body as EdgeBody;
use http::{HeaderMap, HeaderValue, Response, StatusCode, header};

use super::flush::{AfterSendInput, FlushOutcome, run_after_send};
use crate::consent::ConsentContext;
use crate::consent::jurisdiction::Jurisdiction;
use crate::consent::types::ConsentSource;
use crate::ec::finalize::ec_finalize_response;
use crate::ec::kv::KvIdentityGraph;
use crate::ec::kv_backend::test_support::{WriteCounter, WriteCounts};
use crate::ec::kv_types::{KvEntry, KvNetwork, KvPartnerId};
use crate::ec::registry::PartnerRegistry;
use crate::ec::{EcContext, EidSyncSource};
use crate::platform::RuntimeServices;
use crate::platform::test_support::{StubHttpClient, build_services_with_http_client};
use crate::settings::{EcPartner, Settings};
use crate::test_support::tests::create_test_settings;

const SHARED: &str = "sharedid.org";
const GAP: &str = "gap.example.com";

fn partner(source: &str) -> EcPartner {
    EcPartner {
        name: format!("Partner {source}"),
        source_domain: source.to_owned(),
        openrtb_atype: EcPartner::default_openrtb_atype(),
        bidstream_enabled: true,
        api_token: None,
        batch_rate_limit: EcPartner::default_batch_rate_limit(),
        pull_sync_enabled: false,
        pull_sync_url: None,
        pull_sync_allowed_domains: vec![],
        pull_sync_ttl_sec: EcPartner::default_pull_sync_ttl_sec(),
        pull_sync_rate_limit: EcPartner::default_pull_sync_rate_limit(),
        ts_pull_token: None,
    }
}

fn consent() -> ConsentContext {
    ConsentContext {
        jurisdiction: Jurisdiction::NonRegulated,
        source: ConsentSource::Cookie,
        ..ConsentContext::default()
    }
}

struct World {
    settings: Settings,
    registry: PartnerRegistry,
    services: RuntimeServices,
    graph: KvIdentityGraph,
    counter: WriteCounter,
}

/// One request: its role, cookies and response status.
struct Visit<'a> {
    writer: bool,
    source: Option<EidSyncSource>,
    cookies: &'a str,
    sharedid: Option<&'a str>,
    status: StatusCode,
}

impl World {
    fn new() -> Self {
        let (graph, counter) = KvIdentityGraph::write_counting("ec-store");
        Self {
            settings: create_test_settings(),
            registry: PartnerRegistry::from_config(&[partner(SHARED), partner(GAP)])
                .expect("should build registry"),
            services: build_services_with_http_client(Arc::new(StubHttpClient::new())),
            graph,
            counter,
        }
    }

    fn ec_id() -> String {
        format!("{}.ABC123", "a".repeat(64))
    }

    fn seed_returning(&self, entry: &KvEntry) {
        self.graph
            .create(&Self::ec_id(), entry)
            .expect("should seed the row");
    }

    fn returning_context(&self, visit: &Visit<'_>) -> EcContext {
        let mut context = EcContext::new_for_test_with_ip(
            Some(Self::ec_id()),
            consent(),
            Some("192.0.2.10".to_owned()),
        );
        let mut headers = HeaderMap::new();
        if !visit.cookies.is_empty() {
            headers.insert(
                header::COOKIE,
                HeaderValue::from_str(visit.cookies).expect("should build cookie header"),
            );
        }
        context.identity_mut().set_raw_cookies_for_test(&headers);
        context.identity_mut().set_writer(visit.writer);
        if let Some(source) = visit.source {
            context.set_eid_sync_source(source);
        }
        context
    }

    /// Runs finalize and the after-send flush, as the adapter does, and
    /// returns the response, the flush outcome and the writes this visit made.
    fn finish(
        &self,
        mut context: EcContext,
        visit: &Visit<'_>,
    ) -> (Response<EdgeBody>, FlushOutcome, WriteCounts) {
        let before = self.counter.writes();
        let mut response = Response::builder()
            .status(visit.status)
            .body(EdgeBody::empty())
            .expect("should build response");
        ec_finalize_response(
            &self.settings,
            &mut context,
            Some(&self.graph),
            &self.registry,
            None,
            visit.sharedid,
            &mut response,
        );
        let outcome = run_after_send(&AfterSendInput {
            settings: &self.settings,
            registry: &self.registry,
            services: &self.services,
            graph: &self.graph,
            context: &context,
            status: visit.status,
            rate_limiter: None,
        });
        let after = self.counter.writes();
        let delta = WriteCounts {
            lookups: after.lookups - before.lookups,
            add: after.add - before.add,
            if_generation_match: after.if_generation_match - before.if_generation_match,
            overwrite: after.overwrite - before.overwrite,
        };
        (response, outcome, delta)
    }

    fn visit(&self, visit: &Visit<'_>) -> (Response<EdgeBody>, FlushOutcome, WriteCounts) {
        let context = self.returning_context(visit);
        self.finish(context, visit)
    }
}

fn navigation<'a>(cookies: &'a str, sharedid: Option<&'a str>) -> Visit<'a> {
    Visit {
        writer: true,
        source: Some(EidSyncSource::Navigation),
        cookies,
        sharedid,
        status: StatusCode::OK,
    }
}

fn live_row() -> KvEntry {
    let mut entry = KvEntry::minimal("z.example.com", "x", 1);
    entry.network = Some(KvNetwork {
        cluster_size: Some(1),
    });
    entry
}

fn s_cookie_header(response: &Response<EdgeBody>) -> String {
    response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find(|value| value.starts_with("__Host-ts-eids-s=") && !value.contains("Max-Age=0"))
        .and_then(|value| value.split(';').next())
        .expect("should set ts-eids-s")
        .to_owned()
}

#[test]
fn new_visitor_navigation_writes_once() {
    let world = World::new();
    let mut context =
        EcContext::new_for_test_with_ip(None, consent(), Some("192.0.2.10".to_owned()));
    context.identity_mut().set_writer(true);
    context.seed_generation_from_cookies(&world.registry, None, Some("shared-cookie-id"));
    let before = world.counter.writes();
    context
        .generate_if_needed(&world.settings, Some(&world.graph))
        .expect("should create the row");
    let visit = navigation("", Some("shared-cookie-id"));
    let (response, outcome, finish) = world.finish(context, &visit);

    assert_eq!(
        world.counter.writes().add - before.add,
        1,
        "one Add: the create"
    );
    assert_eq!(
        finish.total(),
        0,
        "finalize and the flush write nothing more"
    );
    assert_eq!(
        outcome,
        FlushOutcome::Skipped,
        "a new visitor's flush is skipped"
    );
    assert!(
        response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .any(|value| value
                .to_str()
                .is_ok_and(|value| value.starts_with("ts-ec="))),
        "the EC cookie is set"
    );
}

#[test]
fn returning_visitor_with_changes_writes_once() {
    let world = World::new();
    world.seed_returning(&live_row());

    let (_, outcome, writes) = world.visit(&navigation("", Some("new-shared-id")));

    assert_eq!(outcome, FlushOutcome::Written, "flushed");
    assert_eq!(writes.if_generation_match, 1, "one conditional write");
    assert_eq!(writes.total(), 1, "and nothing else");
    assert_eq!(writes.lookups, 1, "one read, reused after send");
}

#[test]
fn returning_visitor_with_nothing_changed_writes_nothing() {
    let world = World::new();
    let mut entry = live_row();
    entry
        .ids
        .insert(SHARED.to_owned(), KvPartnerId::browser("same-id", Some(1)));
    world.seed_returning(&entry);

    let (_, outcome, writes) = world.visit(&navigation("", Some("same-id")));

    assert_eq!(writes.total(), 0, "no write");
    assert!(
        matches!(outcome, FlushOutcome::NoChange | FlushOutcome::Skipped),
        "nothing to flush: {outcome}"
    );
}

#[test]
fn revalidation_304_with_changes_writes_once() {
    let world = World::new();
    world.seed_returning(&live_row());
    let visit = Visit {
        status: StatusCode::NOT_MODIFIED,
        ..navigation("", Some("new-shared-id"))
    };

    let (_, outcome, writes) = world.visit(&visit);

    assert_eq!(outcome, FlushOutcome::Written, "a 304 flushes");
    assert_eq!(writes.total(), 1, "one write");
}

#[test]
fn returning_navigation_answered_3xx_4xx_or_5xx_writes_nothing() {
    for status in [
        StatusCode::FOUND,
        StatusCode::NOT_FOUND,
        StatusCode::INTERNAL_SERVER_ERROR,
    ] {
        let world = World::new();
        world.seed_returning(&live_row());
        let visit = Visit {
            status,
            ..navigation("", Some("new-shared-id"))
        };
        let (_, _, writes) = world.visit(&visit);
        assert_eq!(writes.total(), 0, "{status} writes nothing");
    }
}

#[test]
fn returning_non_writer_navigations_write_nothing() {
    for (name, source) in [
        (
            "prefetch, iframe or no Sec-Fetch-Dest",
            Some(EidSyncSource::Navigation),
        ),
        ("auction", Some(EidSyncSource::Auction)),
        ("page-bids", Some(EidSyncSource::PageBids)),
        ("identify, proxy or assets", None),
    ] {
        let world = World::new();
        world.seed_returning(&live_row());
        let visit = Visit {
            writer: false,
            source,
            cookies: "",
            sharedid: Some("new-shared-id"),
            status: StatusCode::OK,
        };
        let (_, outcome, writes) = world.visit(&visit);
        assert_eq!(writes.total(), 0, "{name}: no write");
        assert_eq!(outcome, FlushOutcome::Skipped, "{name}: no flush");
    }
}

#[test]
fn cas_conflict_on_the_flush_is_one_attempt() {
    let world = World::new();
    world.seed_returning(&live_row());
    world.counter.conflict_next(1);

    let (_, outcome, writes) = world.visit(&navigation("", Some("new-shared-id")));

    assert_eq!(outcome, FlushOutcome::WriteFailed, "the race is lost");
    assert_eq!(writes.if_generation_match, 1, "one attempt, not retried");
}

#[test]
fn recent_cookie_with_nothing_due_reads_zero_times() {
    let world = World::new();
    world.seed_returning(&live_row());
    let (first, _, _) = world.visit(&navigation("", None));
    let cookie = s_cookie_header(&first);

    let (_, outcome, writes) = world.visit(&navigation(&cookie, None));

    assert_eq!(writes.lookups, 0, "no KV read");
    assert_eq!(writes.total(), 0, "no write");
    assert_eq!(outcome, FlushOutcome::Skipped, "nothing to flush");
}

#[test]
fn body_only_source_reaches_kv_on_the_next_navigation() {
    let world = World::new();
    world.seed_returning(&live_row());
    let auction = Visit {
        writer: false,
        source: Some(EidSyncSource::Auction),
        cookies: "",
        sharedid: None,
        status: StatusCode::OK,
    };
    let mut context = world.returning_context(&auction);
    context.set_kv_snapshot(world.graph.load_snapshot(&World::ec_id()));
    context.set_client_eids(vec![crate::openrtb::Eid::new(
        GAP.to_owned(),
        vec![crate::openrtb::Uid {
            id: "body-only".to_owned(),
            atype: Some(1),
            ext: None,
        }],
    )]);
    context.identity_mut().set_client_eids_from_body(true);
    let (auction_response, _, auction_writes) = world.finish(context, &auction);
    assert_eq!(auction_writes.total(), 0, "/auction writes nothing");
    let cookie = s_cookie_header(&auction_response);

    let (_, outcome, writes) = world.visit(&navigation(&cookie, None));

    assert_eq!(
        outcome,
        FlushOutcome::Written,
        "the next navigation flushes"
    );
    assert_eq!(writes.total(), 1, "in one write");
    let (stored, _) = world
        .graph
        .get(&World::ec_id())
        .expect("should read")
        .expect("should exist");
    assert_eq!(
        stored.ids[GAP].uid, "body-only",
        "the body-only source reached KV"
    );
}

#[test]
fn refused_flush_keeps_staged_entries_for_the_next_navigation() {
    let world = World::new();
    world.seed_returning(&live_row());
    let mut context = world.returning_context(&Visit {
        writer: false,
        source: Some(EidSyncSource::Auction),
        cookies: "",
        sharedid: None,
        status: StatusCode::OK,
    });
    context.set_kv_snapshot(world.graph.load_snapshot(&World::ec_id()));
    context.set_client_eids(vec![crate::openrtb::Eid::new(
        GAP.to_owned(),
        vec![crate::openrtb::Uid {
            id: "body-only".to_owned(),
            atype: Some(1),
            ext: None,
        }],
    )]);
    context.identity_mut().set_client_eids_from_body(true);
    let auction = Visit {
        writer: false,
        source: Some(EidSyncSource::Auction),
        cookies: "",
        sharedid: None,
        status: StatusCode::OK,
    };
    let (auction_response, _, _) = world.finish(context, &auction);
    let cookie = s_cookie_header(&auction_response);

    world.counter.conflict_next(1);
    let (refused_response, refused, _) = world.visit(&navigation(&cookie, None));
    assert_eq!(refused, FlushOutcome::WriteFailed, "the flush is refused");
    let still_staged = refused_response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .any(|value| {
            value
                .to_str()
                .is_ok_and(|value| value.starts_with("__Host-ts-eids-s="))
        });
    let next_cookie = if still_staged {
        s_cookie_header(&refused_response)
    } else {
        cookie
    };

    let (_, outcome, _) = world.visit(&navigation(&next_cookie, None));
    assert_eq!(
        outcome,
        FlushOutcome::Written,
        "the next navigation writes it"
    );
    let (stored, _) = world
        .graph
        .get(&World::ec_id())
        .expect("should read")
        .expect("should exist");
    assert_eq!(stored.ids[GAP].uid, "body-only", "nothing was lost");
}

#[test]
fn deleted_row_is_found_and_rotated_before_send() {
    let world = World::new();
    let mut context = world.returning_context(&navigation("", None));
    context.set_recovery_eligible(true);
    let visit = navigation("", None);
    let before = world.counter.writes();
    let (response, _, _) = world.finish(context, &visit);

    assert_eq!(
        world.counter.writes().add - before.add,
        1,
        "one create for the rotation"
    );
    assert!(
        response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .any(|value| value
                .to_str()
                .is_ok_and(|value| value.starts_with("ts-ec="))),
        "a new EC cookie is set"
    );
}
