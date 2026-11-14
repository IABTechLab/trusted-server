//! Shared lifecycle assertions for real record-backed EC provider fixtures.

use std::time::Duration;

use edgezero_core::body::Body;
use edgezero_core::http::{Response, request_builder};

use super::nextjs_auction::{self, NextJsAuctionOrigin};
use crate::ec::EcContext;
use crate::ec::finalize::ec_finalize_response;
use crate::ec::kv::{KvIdentityGraph, UpsertResult};
use crate::ec::kv_backend::{EcKvStore, EcKvWrite, EcKvWriteMode, EcKvWriteOutcome};
use crate::ec::kv_types::KvEntry;
use crate::ec::registry::PartnerRegistry;
use crate::platform::GeoInfo;

/// Exercise concurrent identity updates, withdrawal and stale-token rejection.
///
/// The caller supplies an isolated real provider. This helper does not claim
/// native expiry, process restart or dropped-acknowledgement qualification.
///
/// # Panics
/// Panics if a lifecycle assertion or provider operation fails.
pub async fn assert_record_ec_lifecycle<Store: EcKvStore + Clone + 'static>(p: &Store) {
    let ec = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.Ab1234";
    let settings = nextjs_auction::settings();
    let origin = NextJsAuctionOrigin::default();
    let services = nextjs_auction::services(&origin);
    let graph = KvIdentityGraph::new(p.clone());
    graph
        .create(ec, &KvEntry::minimal("initial.example", "initial-uid", 1))
        .await
        .expect("should create graph root");
    let (first, second) = futures::join!(
        graph.upsert_partner_id_if_exists(ec, "first.example", "first-uid"),
        graph.upsert_partner_id_if_exists(ec, "second.example", "second-uid"),
    );
    assert_eq!(
        first.expect("should merge first concurrent UID"),
        UpsertResult::Written
    );
    assert_eq!(
        second.expect("should merge second concurrent UID"),
        UpsertResult::Written
    );
    let (merged, _) = graph
        .get(ec)
        .await
        .expect("should read merged graph")
        .expect("should retain merged graph");
    assert_eq!(merged.ids["first.example"].uid, "first-uid");
    assert_eq!(merged.ids["second.example"].uid, "second-uid");
    let before_withdrawal = p
        .lookup(ec)
        .await
        .expect("should read pre-withdrawal row")
        .expect("should retain pre-withdrawal row");
    let stale_body = std::str::from_utf8(&before_withdrawal.body).expect("should store JSON body");
    let stale_meta = std::str::from_utf8(
        before_withdrawal
            .metadata
            .as_deref()
            .expect("should store JSON metadata"),
    )
    .expect("should encode metadata as UTF-8");
    let request = request_builder()
        .uri("https://test-publisher.example.com/article")
        .header("cookie", format!("ts-ec={ec}"))
        .header("sec-gpc", "1")
        .body(Body::empty())
        .expect("should build synthetic withdrawal request");
    let geo = GeoInfo::country_region("US", Some("TN".to_owned()));
    let mut context =
        EcContext::read_from_request_with_geo(&settings, &request, &services, Some(&geo))
            .expect("should decode synthetic GPC withdrawal");
    let registry = PartnerRegistry::from_config(&[]).expect("should create empty fixture registry");
    let mut response = Response::new(Body::empty());
    let (_, raced) = futures::join!(
        ec_finalize_response(
            &settings,
            &mut context,
            Some(&graph),
            &registry,
            None,
            None,
            &mut response
        ),
        graph.upsert_partner_id_if_exists(ec, "late.example", "late-uid"),
    );
    assert!(matches!(
        raced.expect("should resolve partner update racing withdrawal"),
        UpsertResult::Written | UpsertResult::ConsentWithdrawn
    ));
    let (tombstone, _) = graph
        .get(ec)
        .await
        .expect("should read authoritative tombstone")
        .expect("should retain tombstone");
    assert!(!tombstone.consent.ok);
    assert!(tombstone.ids.is_empty());
    assert_eq!(
        graph
            .upsert_partner_id_if_exists(ec, "late.example", "retry-uid")
            .await
            .expect("should reject post-withdrawal UID"),
        UpsertResult::ConsentWithdrawn
    );
    assert_eq!(
        p.insert(
            ec,
            EcKvWrite {
                body: stale_body,
                metadata: stale_meta,
                ttl: Duration::from_secs(60),
                mode: EcKvWriteMode::IfGenerationMatch(before_withdrawal.generation)
            }
        )
        .await
        .expect("should reject stale pre-withdrawal CAS"),
        EcKvWriteOutcome::PreconditionFailed
    );
    assert!(
        response
            .headers()
            .get_all("set-cookie")
            .iter()
            .any(|value| value
                .to_str()
                .is_ok_and(|value| value.starts_with("ts-ec=;") && value.contains("Max-Age=0"))),
        "withdrawal should expire browser EC cookie"
    );
}
