use super::*;
use std::sync::Arc;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::json;

use crate::auction::endpoints::resolve_auction_eids;
use crate::consent::jurisdiction::Jurisdiction;
use crate::ec::EidSyncSource;
use crate::ec::finalize::ec_finalize_response;
use crate::ec::identity::{
    IdentityConsentSignals, IdentityObservation, IdentityOutcome, send_then_apply_identity,
    stage_capture,
};
use crate::ec::kv::{KvIdentityGraph, PartnerIdUpdate};
use crate::ec::kv_types::{KvEntry, KvPartnerId};
use crate::integrations::ProxyDispatchInput;
use crate::platform::test_support::{
    StubHttpClient, build_services_with_http_client, noop_services,
};
use crate::redacted::Redacted;
use crate::settings::{IdentityHandoffConfig, IdentityHandoffKey};
use crate::test_support::tests::create_test_settings;

const SOURCE: &str = "id5-sync.com";
const UID: &str = "ID5*synthetic-handoff";
const PATH: &str = "/integrations/lockr/api/publisher/app/v2/identityLockr/page-view";

fn id() -> String {
    format!("{}.ABC123", "a".repeat(64))
}
fn now() -> u64 {
    checked_current_timestamp().expect("should read test clock")
}
fn settings() -> Settings {
    let mut settings = create_test_settings();
    settings.ec.identity_handoff = IdentityHandoffConfig {
        enabled: true,
        active_key_id: "current".to_owned(),
        keys: vec![IdentityHandoffKey {
            id: "current".to_owned(),
            secret: Redacted::new(URL_SAFE_NO_PAD.encode([7u8; 32])),
        }],
    };
    settings.ec.partners = vec![serde_json::from_value(json!({
        "name":"Synthetic capture source", "source_domain":SOURCE, "identity_owner":"lockr", "openrtb_atype":1, "bidstream_enabled":true
    })).expect("should parse source policy")];
    settings
        .integrations
        .insert_config(
            "lockr",
            &json!({"app_id":"synthetic", "capture_identity":true}),
        )
        .expect("should configure capture");
    settings
}
fn partners(settings: &Settings) -> PartnerRegistry {
    PartnerRegistry::from_config(&settings.ec.partners).expect("should build registry")
}
fn graph() -> KvIdentityGraph {
    let graph = KvIdentityGraph::in_memory("handoff-tests");
    let mut root = KvEntry::minimal("other.example.com", "other-id", now());
    root.consent.ok = true;
    graph
        .create(&id(), &root)
        .expect("should seed consenting root");
    graph
}
fn entry() -> HandoffEntry {
    HandoffEntry {
        module: "lockr".to_owned(),
        source: SOURCE.to_owned(),
        uid: UID.to_owned(),
        expires_at: Some(now() + 600),
        deadline: now() + 300,
    }
}
fn cookie(settings: &Settings, entries: Vec<HandoffEntry>) -> String {
    codec::encode(
        &settings.ec.identity_handoff,
        &settings.publisher.domain,
        &id(),
        entries,
        now(),
    )
    .expect("should encode fixture")
}
fn context(settings: &Settings, value: Option<&str>) -> EcContext {
    let mut cookies = format!("ts-ec={}", id());
    if let Some(value) = value {
        cookies.push_str(&format!("; ts-eids-s={value}"));
    }
    let request = http::Request::builder()
        .uri("https://publisher.example/auction")
        .header(header::COOKIE, cookies)
        .body(EdgeBody::empty())
        .expect("should build request");
    let mut context = EcContext::read_from_request(settings, &request, &noop_services())
        .expect("should read request");
    context.consent_mut().jurisdiction = Jurisdiction::NonRegulated;
    context.set_identity_handoff_supported(true);
    context
}
fn response() -> Response<EdgeBody> {
    Response::builder()
        .status(200)
        .header(header::CACHE_CONTROL, "public, max-age=60")
        .header(header::CONTENT_ENCODING, "gzip")
        .body(EdgeBody::from("original bytes"))
        .expect("should build response")
}
fn handoff_header(response: &Response<EdgeBody>) -> Option<&str> {
    response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find(|value| value.starts_with("ts-eids-s="))
}
fn captured_effect(
    settings: &Settings,
    context: &EcContext,
    partners: &PartnerRegistry,
) -> IdentityEffects {
    stage_capture(
        "lockr",
        &[SOURCE],
        IdentityObservation {
            outcomes: vec![IdentityOutcome::Issued {
                source: SOURCE.to_owned(),
                uid: UID.to_owned(),
                expires_at: Some(now() + 600),
            }],
            consent: IdentityConsentSignals::default(),
        },
        context,
        partners,
        settings,
    )
    .expect("should seal acquisition")
}
fn send(
    response: &mut Response<EdgeBody>,
    context: &mut EcContext,
    graph: &KvIdentityGraph,
    partners: &PartnerRegistry,
) {
    let effects = response.extensions_mut().remove::<IdentityEffects>();
    assert!(send_then_apply_identity(
        effects,
        context,
        partners,
        || {},
        || Some(graph.clone())
    ));
}

#[test]
fn capture_sets_private_handoff_before_storage_and_preserves_bytes() {
    let settings = settings();
    let partners = partners(&settings);
    let graph = graph();
    let mut context = context(&settings, None);
    let mut response = response();
    response
        .extensions_mut()
        .insert(captured_effect(&settings, &context, &partners));
    ec_finalize_response(
        &settings,
        &mut context,
        Some(&graph),
        &partners,
        None,
        &mut response,
    );
    let header = handoff_header(&response).expect("should set capture handoff");
    for attribute in [
        "HttpOnly",
        "Secure",
        "SameSite=Lax",
        "Path=/",
        "Max-Age=300",
    ] {
        assert!(
            header.contains(attribute),
            "should use bounded secure cookie"
        );
    }
    let value = header
        .strip_prefix("ts-eids-s=")
        .expect("should get value")
        .split(';')
        .next()
        .expect("should get token");
    let decoded = codec::decode(
        &settings.ec.identity_handoff,
        &settings.publisher.domain,
        &id(),
        value,
        now(),
    )
    .expect("should authenticate cookie");
    assert_eq!(decoded.len(), 1);
    assert_eq!(decoded[0].uid, UID);
    assert!(
        !graph
            .get(&id())
            .expect("should read graph")
            .expect("should find root")
            .0
            .ids
            .contains_key(SOURCE)
    );
    assert!(
        response.headers()[header::CACHE_CONTROL]
            .to_str()
            .expect("should read cache policy")
            .contains("no-store")
    );
    assert_eq!(response.headers()[header::CONTENT_ENCODING], "gzip");
    assert!(matches!(response.body(),EdgeBody::Once(bytes) if bytes.as_ref()==b"original bytes"));
    let effects = response.extensions_mut().remove::<IdentityEffects>();
    assert!(send_then_apply_identity(
        effects,
        &mut context,
        &partners,
        || {
            assert!(
                !graph
                    .get(&id())
                    .expect("should read graph at send")
                    .expect("should find root")
                    .0
                    .ids
                    .contains_key(SOURCE)
            );
        },
        || Some(graph.clone())
    ));
    let stored = graph
        .get(&id())
        .expect("should read persisted acquisition")
        .expect("should find root")
        .0;
    assert_eq!(stored.ids[SOURCE].writer.as_deref(), Some("lockr"));
    assert!(stored.ids[SOURCE].expires_at.is_some());
    assert_eq!(stored.ids["other.example.com"].uid, "other-id");
}

#[test]
fn pending_capture_delivers_before_visibility_then_replays_and_acknowledges() {
    let settings = settings();
    let partners = partners(&settings);
    let graph = graph();
    let original_entry = entry();
    let value = cookie(&settings, vec![original_entry.clone()]);
    let mut context = context(&settings, Some(&value));
    context.set_kv_snapshot(graph.load_snapshot(&id()));
    context.set_eid_sync_source(EidSyncSource::Auction);
    let eids = resolve_auction_eids(
        context.kv_snapshot(),
        Some(&partners),
        &context,
        Some(now()),
    )
    .expect("should resolve handoff auction");
    assert_eq!(eids.len(), 1);
    assert_eq!(eids[0].uids[0].id, UID);
    assert_eq!(eids[0].inserter.as_deref(), Some("Lockr-For-Publishers"));
    assert_eq!(eids[0].matcher.as_deref(), Some(""));
    assert_eq!(eids[0].mm, Some(2));
    // The matching browser value must not be persisted as unknown-expiry browser state.
    context.stage_browser_eid_updates(vec![PartnerIdUpdate::new(SOURCE, UID)]);
    let mut response = response();
    ec_finalize_response(
        &settings,
        &mut context,
        Some(&graph),
        &partners,
        None,
        &mut response,
    );
    assert!(
        !graph
            .get(&id())
            .expect("should read before replay")
            .expect("should find root")
            .0
            .ids
            .contains_key(SOURCE)
    );
    send(&mut response, &mut context, &graph, &partners);
    let stored = graph
        .get(&id())
        .expect("should read replay")
        .expect("should find root")
        .0;
    assert_eq!(stored.ids[SOURCE].uid, UID);
    assert_eq!(stored.ids[SOURCE].writer.as_deref(), Some("lockr"));
    assert_eq!(stored.ids[SOURCE].expires_at, original_entry.expires_at);
    let mut acknowledged = self::context(&settings, Some(&value));
    acknowledged.set_kv_snapshot(graph.load_snapshot(&id()));
    let mut response = self::response();
    ec_finalize_response(
        &settings,
        &mut acknowledged,
        Some(&graph),
        &partners,
        None,
        &mut response,
    );
    assert!(
        handoff_header(&response)
            .expect("should clear acknowledged cookie")
            .contains("Max-Age=0")
    );
    assert!(response.extensions().get::<IdentityEffects>().is_none());
}

#[test]
fn matching_capture_retains_pending_deadline_and_missing_expiry_without_overriding_kv() {
    let settings = settings();
    let partners = partners(&settings);
    let mut original_entry = entry();
    let expiry = now() + 500;
    original_entry.expires_at = Some(expiry);
    original_entry.deadline = now() + 200;
    let value = cookie(&settings, vec![original_entry]);
    for reported_expiry in [Some(expiry), None] {
        let graph = graph();
        let mut context = context(&settings, Some(&value));
        let effects = stage_capture(
            "lockr",
            &[SOURCE],
            IdentityObservation {
                outcomes: vec![IdentityOutcome::Issued {
                    source: SOURCE.to_owned(),
                    uid: UID.to_owned(),
                    expires_at: reported_expiry,
                }],
                consent: IdentityConsentSignals::default(),
            },
            &context,
            &partners,
            &settings,
        )
        .expect("should seal matching capture");
        let mut response = response();
        response.extensions_mut().insert(effects);
        ec_finalize_response(
            &settings,
            &mut context,
            Some(&graph),
            &partners,
            None,
            &mut response,
        );
        let returned = handoff_header(&response)
            .expect("should retain pending observation")
            .strip_prefix("ts-eids-s=")
            .expect("should get value")
            .split(';')
            .next()
            .expect("should get token");
        assert_eq!(
            returned, value,
            "identical capture or missing expiry must not renew pending metadata"
        );
        if reported_expiry.is_none() {
            // A newer stored expiry appears after seal. The cookie fallback is
            // fill-absent-only metadata, never authority over this record.
            let mut fresh_context = self::context(&settings, None);
            captured_effect(&settings, &fresh_context, &partners).apply(
                &graph,
                &partners,
                &mut fresh_context,
            );
            let before = graph.get(&id()).expect("should read newer expiry");
            send(&mut response, &mut context, &graph, &partners);
            assert_eq!(
                graph.get(&id()).expect("should retain newer expiry"),
                before
            );
        } else {
            send(&mut response, &mut context, &graph, &partners);
            assert_eq!(
                graph
                    .get(&id())
                    .expect("should read capture")
                    .expect("should find root")
                    .0
                    .ids[SOURCE]
                    .expires_at,
                Some(expiry)
            );
        }
    }
    // Missing provider expiry also preserves known pending expiry when the
    // post-send read still finds the source absent.
    let graph = graph();
    let mut context = context(&settings, Some(&value));
    let effects = stage_capture(
        "lockr",
        &[SOURCE],
        IdentityObservation {
            outcomes: vec![IdentityOutcome::Issued {
                source: SOURCE.to_owned(),
                uid: UID.to_owned(),
                expires_at: None,
            }],
            consent: IdentityConsentSignals::default(),
        },
        &context,
        &partners,
        &settings,
    )
    .expect("should seal capture without expiry");
    effects.apply(&graph, &partners, &mut context);
    assert_eq!(
        graph
            .get(&id())
            .expect("should read filled source")
            .expect("should find root")
            .0
            .ids[SOURCE]
            .expires_at,
        Some(expiry)
    );
}

#[test]
fn existing_records_including_expired_and_same_uid_refresh_block_replay() {
    let settings = settings();
    let partners = partners(&settings);
    for (uid, expiry) in [
        ("newer-uid", Some(now() + 900)),
        (UID, Some(now() + 900)),
        (UID, Some(1)),
    ] {
        let graph = KvIdentityGraph::in_memory("handoff-existing-records");
        let mut root = KvEntry::minimal(SOURCE, uid, now());
        root.consent.ok = true;
        let record = root.ids.get_mut(SOURCE).expect("should get fixture record");
        record.expires_at = expiry;
        record.writer = Some("lockr".to_owned());
        record.revision = Some("newer-revision".to_owned());
        graph
            .create(&id(), &root)
            .expect("should seed existing record");
        let before = graph.get(&id()).expect("should read before");
        let value = cookie(&settings, vec![entry()]);
        let mut context = context(&settings, Some(&value));
        context.set_kv_snapshot(graph.load_snapshot(&id()));
        context.set_eid_sync_source(EidSyncSource::Auction);
        assert!(resolve_eids(&context, context.kv_snapshot(), &partners, Some(now())).is_empty());
        let mut response = response();
        ec_finalize_response(
            &settings,
            &mut context,
            Some(&graph),
            &partners,
            None,
            &mut response,
        );
        assert!(
            handoff_header(&response)
                .expect("should retire conflicting handoff")
                .contains("Max-Age=0")
        );
        send(&mut response, &mut context, &graph, &partners);
        assert_eq!(graph.get(&id()).expect("should read after"), before);
    }
}

#[test]
fn replay_cas_cannot_revive_withdrawal_or_replace_a_concurrent_capture() {
    let settings = settings();
    let partners = partners(&settings);
    for withdrawal in [true, false] {
        let graph = graph();
        let value = cookie(&settings, vec![entry()]);
        let mut context = context(&settings, Some(&value));
        context.set_kv_snapshot(graph.load_snapshot(&id()));
        context.set_eid_sync_source(EidSyncSource::Auction);
        let mut response = response();
        ec_finalize_response(
            &settings,
            &mut context,
            Some(&graph),
            &partners,
            None,
            &mut response,
        );
        if withdrawal {
            graph.tombstone_existing_from_snapshot(&id(), graph.load_snapshot(&id()));
        } else {
            graph
                .upsert_partner_id(&id(), SOURCE, "concurrent-newer-uid")
                .expect("should write concurrent fixture");
        }
        let before = graph.get(&id()).expect("should read concurrent state");
        send(&mut response, &mut context, &graph, &partners);
        assert_eq!(graph.get(&id()).expect("should read after replay"), before);
    }
}

#[test]
fn observed_stale_snapshot_can_deliver_but_replay_rechecks_authoritative_generation() {
    let settings = settings();
    let partners = partners(&settings);
    let graph = graph();
    let value = cookie(&settings, vec![entry()]);
    let mut context = context(&settings, Some(&value));
    context.set_kv_snapshot(graph.load_snapshot(&id()));
    context.set_eid_sync_source(EidSyncSource::Auction);
    // Model an older live point read after another request has withdrawn.
    graph.tombstone_existing_from_snapshot(&id(), graph.load_snapshot(&id()));
    assert_eq!(
        resolve_eids(&context, context.kv_snapshot(), &partners, Some(now())).len(),
        1,
        "delivery retains the existing snapshot consistency limit"
    );
    let before = graph
        .get(&id())
        .expect("should read authoritative tombstone");
    let mut response = response();
    ec_finalize_response(
        &settings,
        &mut context,
        Some(&graph),
        &partners,
        None,
        &mut response,
    );
    send(&mut response, &mut context, &graph, &partners);
    assert_eq!(
        graph.get(&id()).expect("should read after replay CAS"),
        before,
        "replay must not revive the authoritative tombstone"
    );
}

#[test]
fn deadline_and_provider_expiry_are_rechecked_after_send() {
    let settings = settings();
    let partners = partners(&settings);
    let graph = graph();
    let before = graph.get(&id()).expect("should read before");
    for expired_deadline in [true, false] {
        let mut update = entry();
        if expired_deadline {
            update.deadline = 1;
        } else {
            update.expires_at = Some(1);
        }
        let mut context = context(&settings, None);
        context.set_kv_snapshot(graph.load_snapshot(&id()));
        IdentityEffects::handoff_replay(&context, vec![update]).apply(
            &graph,
            &partners,
            &mut context,
        );
        assert_eq!(
            graph.get(&id()).expect("should read after expired replay"),
            before
        );
    }
}

#[test]
fn missing_failed_or_withdrawn_roots_and_unknown_clock_do_not_deliver() {
    let settings = settings();
    let partners = partners(&settings);
    let value = cookie(&settings, vec![entry()]);
    let mut context = context(&settings, Some(&value));
    let mut withdrawn = KvEntry::tombstone(now());
    withdrawn
        .ids
        .insert(SOURCE.to_owned(), KvPartnerId::default());
    for snapshot in [
        EcKvSnapshot::NotRead,
        EcKvSnapshot::Missing { ec_id: id() },
        EcKvSnapshot::Failed { ec_id: id() },
        EcKvSnapshot::Present {
            ec_id: id(),
            entry: Box::new(withdrawn),
            generation: Some(1),
        },
    ] {
        context.set_kv_snapshot(snapshot);
        assert!(resolve_eids(&context, context.kv_snapshot(), &partners, Some(now())).is_empty());
    }
    context.set_kv_snapshot(graph().load_snapshot(&id()));
    assert!(resolve_eids(&context, context.kv_snapshot(), &partners, None).is_empty());
}

#[test]
fn tampering_rotation_disabled_capture_and_request_denial_never_authorize_handoff() {
    let settings = settings();
    let partners = partners(&settings);
    let mut value = cookie(&settings, vec![entry()]);
    value.push('x');
    let mut context = context(&settings, Some(&value));
    context.set_kv_snapshot(graph().load_snapshot(&id()));
    assert!(resolve_eids(&context, context.kv_snapshot(), &partners, Some(now())).is_empty());
    let value = cookie(&settings, vec![entry()]);
    let mut disabled = settings.clone();
    disabled
        .integrations
        .insert_config(
            "lockr",
            &json!({"app_id":"synthetic","capture_identity":false}),
        )
        .expect("should disable acquisition");
    let mut context = self::context(&disabled, Some(&value));
    context.set_kv_snapshot(graph().load_snapshot(&id()));
    assert!(resolve_eids(&context, context.kv_snapshot(), &partners, Some(now())).is_empty());
    let mut context = self::context(&settings, Some(&value));
    context.set_kv_snapshot(graph().load_snapshot(&id()));
    context.consent_mut().gpc = true;
    assert!(resolve_eids(&context, context.kv_snapshot(), &partners, Some(now())).is_empty());
    let mut context = self::context(&settings, Some(&value));
    context.replace_with_generated(format!("{}.XYZ789", "a".repeat(64)), EcKvSnapshot::NotRead);
    assert!(resolve_eids(&context, context.kv_snapshot(), &partners, Some(now())).is_empty());
    assert!(!format!("{context:?}").contains(UID));
}

#[test]
fn no_token_proxy_request_repairs_pending_capture_after_send_but_body_optout_clears_it() {
    let settings = settings();
    let partners = partners(&settings);
    let value = cookie(&settings, vec![entry()]);
    for denied in [false, true] {
        let graph = graph();
        let mut context = context(&settings, Some(&value));
        let stub = Arc::new(StubHttpClient::new());
        stub.push_response(
            200,
            serde_json::to_vec(&json!({"success":true,"aimTokens":[]}))
                .expect("should encode no-token response"),
        );
        let services = build_services_with_http_client(
            Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>
        );
        let registry =
            IntegrationRegistry::new(&settings).expect("should create integration registry");
        let body = if denied {
            json!({"ccpaString":"1YYN"})
        } else {
            json!({})
        };
        let request = http::Request::builder()
            .method(http::Method::POST)
            .uri(PATH)
            .body(EdgeBody::from(
                serde_json::to_vec(&body).expect("should serialize body"),
            ))
            .expect("should build proxy request");
        let mut response = futures::executor::block_on(registry.handle_proxy(ProxyDispatchInput {
            method: &http::Method::POST,
            path: PATH,
            settings: &settings,
            kv: Some(&graph),
            ec_context: &mut context,
            services: &services,
            req: request,
        }))
        .expect("should select proxy")
        .expect("should proxy upstream");
        ec_finalize_response(
            &settings,
            &mut context,
            Some(&graph),
            &partners,
            None,
            &mut response,
        );
        assert!(
            matches!(context.kv_snapshot(), EcKvSnapshot::NotRead),
            "no-token proxy must not add a pre-send KV lookup"
        );
        if denied {
            assert!(
                handoff_header(&response)
                    .expect("should clear denied handoff")
                    .contains("Max-Age=0")
            );
        } else {
            let header = handoff_header(&response).expect("should retain pending handoff");
            let returned_value = header
                .strip_prefix("ts-eids-s=")
                .expect("should get cookie value")
                .split(';')
                .next()
                .expect("should get token");
            assert_eq!(
                returned_value, value,
                "NoChange must retain the original authenticated deadline"
            );
        }
        send(&mut response, &mut context, &graph, &partners);
        assert_eq!(
            graph
                .get(&id())
                .expect("should read repair")
                .expect("should find root")
                .0
                .ids
                .contains_key(SOURCE),
            !denied
        );
    }
}

#[test]
fn oversized_replacement_expires_existing_cookie_when_nothing_fits() {
    let settings = settings();
    let partners = partners(&settings);
    let graph = graph();
    let value = cookie(&settings, vec![entry()]);
    let mut context = context(&settings, Some(&value));
    let effects = stage_capture(
        "lockr",
        &[SOURCE],
        IdentityObservation {
            outcomes: vec![IdentityOutcome::Issued {
                source: SOURCE.to_owned(),
                uid: "\u{1}".repeat(512),
                expires_at: None,
            }],
            consent: IdentityConsentSignals::default(),
        },
        &context,
        &partners,
        &settings,
    )
    .expect("should seal bounded but JSON-expanding UID");
    let mut response = response();
    response.extensions_mut().insert(effects);
    ec_finalize_response(
        &settings,
        &mut context,
        Some(&graph),
        &partners,
        None,
        &mut response,
    );
    assert!(
        handoff_header(&response)
            .expect("should retire oversized replacement")
            .contains("Max-Age=0")
    );
}

#[test]
fn capture_and_handoff_stay_off_on_unsupported_adapters_and_unrelated_requests_do_not_clean_up() {
    let settings = settings();
    let partners = partners(&settings);
    let graph = graph();
    let mut context = context(&settings, None);
    context.set_identity_handoff_supported(false);
    let mut response = response();
    response
        .extensions_mut()
        .insert(captured_effect(&settings, &context, &partners));
    ec_finalize_response(
        &settings,
        &mut context,
        Some(&graph),
        &partners,
        None,
        &mut response,
    );
    assert!(handoff_header(&response).is_none());
    let mut context = self::context(&settings, None);
    let mut response = self::response();
    ec_finalize_response(
        &settings,
        &mut context,
        Some(&graph),
        &partners,
        None,
        &mut response,
    );
    assert!(handoff_header(&response).is_none());
}
