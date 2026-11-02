//! Transactional scenarios use the real owner, durable queue and filesystem.
use std::sync::{Arc, Barrier};

use trusted_server_preparation_worker::store::{
    Admission, Artifact, Bounds, JobOrigin, JobState, OutcomeClass, Store, StoreError, target_key,
};
use trusted_server_provenance_encypher::preparation::pipeline::{TextCacheScope, TextSignRequest};
use trusted_server_provenance_encypher::preparation::protocol::{
    EnvelopeItem, ItemKind, ObservedItem, PREPARATION_PROTOCOL, ResultEnvelope, Target,
};

fn target(index: usize) -> Target {
    Target {
        path: format!("/article/{index:04}"),
        variant: String::new(),
    }
}
fn item(byte: char) -> ObservedItem {
    ObservedItem {
        identity: byte.to_string().repeat(64),
        kind: ItemKind::Text,
    }
}

#[test]
fn identical_processing_hint_does_not_schedule_signing_churn_and_latest_replaces_history() {
    let dir = tempfile::tempdir().expect("should create store");
    let store = Store::open(&dir.path().join("preparation.redb"), Bounds::default())
        .expect("should own store");
    let target = target(1);
    store
        .admit(&target, JobOrigin::Signal, &[item('a')], 1)
        .expect("should admit");
    let first = store
        .claim(1, 10)
        .expect("should claim")
        .expect("should have work");
    store
        .admit(&target, JobOrigin::Signal, &[item('a')], 2)
        .expect("should coalesce duplicate");
    assert!(
        !store
            .job(&first.job.key)
            .expect("should read")
            .expect("should remain claimed")
            .follow_up
    );
    for byte in ['b', 'c', 'd'] {
        store
            .admit(&target, JobOrigin::Signal, &[item(byte)], 3)
            .expect("should replace desired generation");
    }
    let desired = store
        .job(&first.job.key)
        .expect("should read")
        .expect("should retain desired state");
    assert_eq!(
        desired.observed,
        vec![item('d')],
        "one latest desired observation, not history that fills 64 slots"
    );
    assert_eq!(
        desired.generation,
        first.job.generation + 3,
        "each changed observation advances the desired version"
    );
    store
        .retry(&first.job.key, first.token, 4, "old_failure")
        .expect("should settle old claim");
    let pending = store
        .job(&first.job.key)
        .expect("should read pending follow-up")
        .expect("should preserve latest generation");
    assert!(
        !store
            .complete(&first.job.key, first.token, 4)
            .expect("should refuse duplicate old completion")
    );
    assert_eq!(
        store
            .job(&first.job.key)
            .expect("should read after stale completion"),
        Some(pending),
        "a duplicate old success cannot clear the desired pending version"
    );
    let next = store
        .claim(4, 11)
        .expect("should claim immediately")
        .expect("should keep follow-up despite old failure");
    assert_eq!(next.job.observed, vec![item('d')]);
    assert_eq!(next.job.generation, desired.generation);
    assert_eq!(next.job.attempts, 1);
    assert!(
        !store
            .complete(&first.job.key, first.token, 5)
            .expect("should refuse stale completion")
    );
    assert!(
        store
            .complete(&next.job.key, next.token, 5)
            .expect("should settle latest generation")
    );
}

#[test]
fn exhausted_old_claim_cannot_remove_new_desired_generation() {
    let dir = tempfile::tempdir().expect("should create store");
    let store = Store::open(
        &dir.path().join("preparation.redb"),
        Bounds {
            max_attempts: 1,
            ..Bounds::default()
        },
    )
    .expect("should open");
    let page = target(1);
    store
        .admit(&page, JobOrigin::Signal, &[item('a')], 0)
        .expect("should admit");
    let first = store
        .claim(0, 1)
        .expect("should claim")
        .expect("should have work");
    store
        .admit(&page, JobOrigin::Signal, &[item('b')], 1)
        .expect("should retain desired change");
    store
        .retry(&first.job.key, first.token, 2, "offline")
        .expect("should settle old attempt");
    let latest = store
        .claim(2, 2)
        .expect("should claim")
        .expect("should keep latest despite exhausted old attempt");
    assert_eq!(latest.job.observed, vec![item('b')]);
    assert!(
        store
            .outcome(&page, &item('b').identity)
            .expect("should read")
            .is_none()
    );
}

#[test]
fn coalescing_cannot_exceed_the_metadata_reservation() {
    let dir = tempfile::tempdir().expect("should create store");
    let bounds = Bounds {
        max_queue_bytes: 1400,
        reserved_bytes: 600,
        max_jobs: 3,
        reserved_jobs: 1,
        ..Bounds::default()
    };
    let store = Store::open(&dir.path().join("preparation.redb"), bounds).expect("should open");
    let page = target(1);
    assert!(matches!(
        store
            .admit(&page, JobOrigin::Signal, &[item('a')], 0)
            .expect("should admit"),
        Admission::Queued { .. }
    ));
    let large: Vec<_> = ('a'..='f').map(item).collect();
    assert_eq!(
        store
            .admit(&page, JobOrigin::Signal, &large, 1)
            .expect("should bound replacement"),
        Admission::QueueFull
    );
    assert_eq!(
        store
            .job(&target_key(&page))
            .expect("should read")
            .expect("should preserve old job")
            .observed,
        vec![item('a')]
    );
    assert!(matches!(
        store
            .admit(&target(2), JobOrigin::Warm, &[], 1)
            .expect("should preserve publisher reservation"),
        Admission::Queued { .. }
    ));
}

#[test]
fn concurrent_inventory_registration_never_silently_exceeds_or_partially_claims_capacity() {
    let dir = tempfile::tempdir().expect("should create store");
    let store = Arc::new(
        Store::open(
            &dir.path().join("preparation.redb"),
            Bounds {
                max_inventory: 2,
                ..Bounds::default()
            },
        )
        .expect("should open"),
    );
    let barrier = Arc::new(Barrier::new(4));
    let handles: Vec<_> = (0..4)
        .map(|index| {
            let store = Arc::clone(&store);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                store.register(&[target(index)]).is_ok()
            })
        })
        .collect();
    let accepted = handles
        .into_iter()
        .map(|handle| handle.join().expect("should join"))
        .filter(|accepted| *accepted)
        .count();
    assert_eq!(accepted, 2);
    assert_eq!(store.counts().expect("should count").inventory, 2);
}

#[test]
fn inventory_cursor_and_consecutive_observations_survive_restart_without_reader_hints() {
    let dir = tempfile::tempdir().expect("should create store");
    {
        let store = Store::open(&dir.path().join("preparation.redb"), Bounds::default())
            .expect("should open");
        store
            .register(&(0..6).map(target).collect::<Vec<_>>())
            .expect("should register independent inventory");
        assert_eq!(
            store.due_inventory(1000, 900, 2).expect("should scan"),
            vec![target(0), target(1)]
        );
        assert_eq!(
            store
                .observe_snapshot(&target(0), &[item('a').identity], 1000)
                .expect("should observe"),
            Some(vec![])
        );
    }
    let store = Store::open(&dir.path().join("preparation.redb"), Bounds::default())
        .expect("should reopen");
    assert_eq!(
        store
            .due_inventory(1060, 900, 2)
            .expect("should resume cursor"),
        vec![target(2), target(3)]
    );
    assert_eq!(
        store
            .observe_snapshot(&target(0), &[item('a').identity, item('b').identity], 1900)
            .expect("should observe independently"),
        Some(vec![item('a').identity])
    );
}

#[test]
fn retained_inventory_refuses_reduced_capacity_without_losing_targets_or_artifacts() {
    let dir = tempfile::tempdir().expect("should create store");
    let path = dir.path().join("preparation.redb");
    let bounds = Bounds {
        max_inventory: 15,
        ..Bounds::default()
    };
    let inventory: Vec<_> = (0..15).map(target).collect();
    let identity = item('a').identity;
    let artifact = Artifact {
        envelope: ResultEnvelope {
            protocol: PREPARATION_PROTOCOL.to_owned(),
            deployment_id: "retained-inventory-test".to_owned(),
            identity: identity.clone(),
            retirement_epoch: 0,
            variant: String::new(),
            item: EnvelopeItem::Text {
                request: TextSignRequest {
                    text: "durable exact text".to_owned(),
                    page_url: "https://www.example.com/article/0000".to_owned(),
                    document_title: None,
                    boundary_selector: "article".to_owned(),
                    expected_policy_digest: "b".repeat(64),
                    cache_scope: TextCacheScope::ExactRequest,
                },
                idempotency_key: "retained-operation".to_owned(),
            },
        },
        target: inventory[0].clone(),
        acknowledged: true,
    };
    let job = {
        let store = Store::open(&path, bounds).expect("should open at exact capacity");
        assert_eq!(
            store
                .register(&inventory)
                .expect("should register all targets"),
            15
        );
        store
            .observe_snapshot(&inventory[0], std::slice::from_ref(&identity), 1000)
            .expect("should retain consecutive-observation evidence");
        assert_eq!(
            store
                .due_inventory(1000, 900, 1)
                .expect("should advance cursor"),
            vec![inventory[0].clone()]
        );
        assert!(
            store
                .record_artifact(&artifact)
                .expect("should retain real artifact bytes and metadata")
        );
        store
            .admit(&inventory[3], JobOrigin::Warm, &[], 1000)
            .expect("should retain pending work");
        store
            .job(&target_key(&inventory[3]))
            .expect("should read pending work")
            .expect("should retain job")
    };
    let artifact_path = dir
        .path()
        .join("artifacts")
        .join(format!("{identity}.json"));
    let artifact_bytes =
        std::fs::read(&artifact_path).expect("should read retained artifact before refusal");
    assert!(matches!(
        Store::open(
            &path,
            Bounds {
                max_inventory: 14,
                ..bounds
            }
        ),
        Err(StoreError::InventoryFull)
    ));
    assert_eq!(
        std::fs::read(&artifact_path).expect("refused startup must preserve artifact bytes"),
        artifact_bytes
    );
    let store = Store::open(&path, bounds)
        .expect("raising capacity back to the persisted count should release the refusal");
    store
        .recover_storage()
        .expect("normal startup recovery must retain valid stored work");
    assert_eq!(store.counts().expect("should count").inventory, 15);
    assert_eq!(
        store
            .artifact(&identity)
            .expect("should read durable artifact"),
        Some(artifact)
    );
    assert_eq!(
        std::fs::read(&artifact_path).expect("adequate reopen must preserve exact artifact bytes"),
        artifact_bytes
    );
    assert_eq!(
        store
            .job(&target_key(&inventory[3]))
            .expect("should read pending work after reopen"),
        Some(job)
    );
    assert_eq!(
        store
            .due_inventory(1001, 900, 1)
            .expect("should resume the retained cursor"),
        vec![inventory[1].clone()]
    );
    assert_eq!(
        store
            .observe_snapshot(&inventory[0], std::slice::from_ref(&identity), 1001)
            .expect("should read retained observation"),
        Some(vec![identity])
    );
    let mut retained = store
        .due_inventory(2000, 900, 128)
        .expect("should enumerate every retained target without waiting");
    retained.sort_by(|left, right| left.path.cmp(&right.path));
    assert_eq!(retained, inventory);
    assert_eq!(
        store.register(&[target(15)]),
        Err(StoreError::InventoryFull),
        "reopening at the exact retained count must not create extra capacity"
    );
}

#[test]
fn repeated_crashes_do_not_reset_attempts_or_the_one_hour_exhaustion_cooldown() {
    let dir = tempfile::tempdir().expect("should create store");
    let bounds = Bounds {
        max_attempts: 2,
        ..Bounds::default()
    };
    for token in 1..=2 {
        let store = Store::open(&dir.path().join("preparation.redb"), bounds).expect("should open");
        if token == 1 {
            store
                .admit(&target(1), JobOrigin::Warm, &[], 10)
                .expect("should admit");
        }
        store
            .recover_all(10)
            .expect("should recover startup ownership");
        let claim = store
            .claim(10, token)
            .expect("should claim")
            .expect("should retain unfinished work");
        assert_eq!(claim.job.attempts, token as u32);
    }
    let store = Store::open(&dir.path().join("preparation.redb"), bounds).expect("should reopen");
    store
        .recover_all(11)
        .expect("should recover exhausted claim");
    assert!(
        store
            .claim(11, 3)
            .expect("should refuse third attempt")
            .is_none()
    );
    assert_eq!(
        store
            .admit(&target(1), JobOrigin::Reconcile, &[], 12)
            .expect("should respect target cooldown"),
        Admission::Damped
    );
    assert_eq!(
        store
            .outcome(&target(1), "*")
            .expect("should retain terminal damping")
            .expect("should damp")
            .class,
        OutcomeClass::Exhausted
    );
    assert!(matches!(
        store
            .admit(&target(1), JobOrigin::Reconcile, &[], 3612)
            .expect("should allow after cooldown"),
        Admission::Queued { .. }
    ));
}

#[test]
fn lease_boundary_and_restart_preserve_new_desired_generation_against_every_stale_settlement() {
    let dir = tempfile::tempdir().expect("should create store");
    let bounds = Bounds {
        lease_seconds: 10,
        max_attempts: 1,
        ..Bounds::default()
    };
    let page = target(1);
    let old = {
        let store = Store::open(&dir.path().join("preparation.redb"), bounds).expect("should open");
        store
            .admit(&page, JobOrigin::Signal, &[item('a')], 100)
            .expect("should admit");
        let old = store
            .claim(100, 1)
            .expect("should claim")
            .expect("should have work");
        assert!(
            !store
                .renew(&old.job.key, 2, 109)
                .expect("should refuse another owner")
        );
        assert!(
            store
                .renew(&old.job.key, old.token, 109)
                .expect("should renew just before expiry")
        );
        store
            .admit(&page, JobOrigin::Signal, &[item('b')], 118)
            .expect("should replace desired version during renewed lease");
        let desired = store
            .job(&old.job.key)
            .expect("should read")
            .expect("should retain follow-up");
        assert_eq!(desired.generation, old.job.generation + 1);
        assert_eq!(
            store
                .recover_expired(118)
                .expect("should leave live owner alone"),
            0
        );
        assert!(
            !store
                .renew(&old.job.key, old.token, 119)
                .expect("should refuse renewal at exact expiry")
        );
        assert!(
            !store
                .complete(&old.job.key, old.token, 119)
                .expect("should refuse success at exact expiry")
        );
        assert!(
            !store
                .retry(&old.job.key, old.token, 119, "late_failure")
                .expect("should refuse failure at exact expiry")
        );
        assert_eq!(
            store
                .job(&old.job.key)
                .expect("should preserve desired state"),
            Some(desired)
        );
        old
    };
    let store = Store::open(&dir.path().join("preparation.redb"), bounds)
        .expect("should reopen real owner");
    assert_eq!(
        store
            .recover_expired(119)
            .expect("should recover exactly expired lease"),
        1
    );
    let pending = store
        .job(&old.job.key)
        .expect("should read")
        .expect("should retain latest despite exhausted old attempt");
    assert_eq!(pending.state, JobState::Pending { ready_at: 119 });
    assert_eq!(pending.observed, vec![item('b')]);
    let latest = store
        .claim(119, 2)
        .expect("should claim immediately")
        .expect("should retain follow-up after restart");
    assert_eq!(latest.job.generation, old.job.generation + 1);
    assert_eq!(
        latest.job.attempts, 1,
        "the old exhausted generation cannot spend the new generation's attempts"
    );
    store
        .record_claim_outcome(
            &old,
            &item('b').identity,
            "stale_terminal",
            OutcomeClass::Terminal,
            120,
        )
        .expect("should fence old negative outcome");
    assert!(
        !store
            .renew(&old.job.key, old.token, 120)
            .expect("should refuse old renewal")
    );
    assert!(
        !store
            .complete(&old.job.key, old.token, 120)
            .expect("should refuse old completion")
    );
    assert!(
        !store
            .retry(&old.job.key, old.token, 120, "stale_retry")
            .expect("should refuse old failure")
    );
    assert_eq!(
        store
            .job(&latest.job.key)
            .expect("should read current ownership"),
        Some(latest.job.clone())
    );
    assert!(
        store
            .outcome(&page, &item('b').identity)
            .expect("should read current outcome")
            .is_none()
    );
    assert!(
        store
            .complete(&latest.job.key, latest.token, 120)
            .expect("should complete latest version")
    );
    assert!(
        store
            .claim(120, 3)
            .expect("should have no duplicate work")
            .is_none()
    );
}

#[test]
fn relevant_context_changes_clear_terminal_damping_but_never_retirement_or_retry_cooldown() {
    let dir = tempfile::tempdir().expect("should create store");
    let store =
        Store::open(&dir.path().join("preparation.redb"), Bounds::default()).expect("should open");
    store
        .refresh_context("probe", "release-a")
        .expect("should record authenticated release");
    store
        .record_outcome(
            &target(1),
            &item('a').identity,
            "unsupported_carrier",
            OutcomeClass::Terminal,
            0,
        )
        .expect("should damp terminal outcome");
    store
        .record_outcome(
            &target(1),
            &item('b').identity,
            "offline",
            OutcomeClass::Exhausted,
            0,
        )
        .expect("should damp exhausted attempt");
    store
        .retire(&item('c').identity, "deployment")
        .expect("should retire independently");
    store
        .refresh_context("probe", "release-b")
        .expect("should invalidate old terminal context");
    assert!(
        store
            .outcome(&target(1), &item('a').identity)
            .expect("should read")
            .is_none()
    );
    assert!(
        store
            .outcome(&target(1), &item('b').identity)
            .expect("should retain cooldown")
            .is_some()
    );
    assert!(
        store
            .retirement(&item('c').identity)
            .expect("should retain retirement")
            .is_some()
    );
    assert_eq!(
        store
            .admit(&target(1), JobOrigin::Signal, &[item('c')], 1)
            .expect("should refuse spent identity"),
        Admission::Damped
    );
}

#[test]
fn a_failed_marker_cannot_starve_later_retirements_and_cursor_survives_restart() {
    let dir = tempfile::tempdir().expect("should create store");
    {
        let store = Store::open(&dir.path().join("preparation.redb"), Bounds::default())
            .expect("should open");
        for index in 0..20 {
            store
                .retire(&format!("{index:064x}"), "deployment")
                .expect("should queue marker");
        }
        let first = store
            .outbox_batch(16)
            .expect("should publish bounded batch");
        assert_eq!(first.len(), 16);
        assert_eq!(first[0].identity, format!("{:064x}", 0));
        // All sixteen sends fail: no acknowledgement, but the fair cursor is durable.
    }
    let store = Store::open(&dir.path().join("preparation.redb"), Bounds::default())
        .expect("should reopen");
    let next = store.outbox_batch(16).expect("should resume cursor");
    assert_eq!(next[0].identity, format!("{:064x}", 16));
    assert!(
        next.iter()
            .any(|marker| marker.identity == format!("{:064x}", 19))
    );
    assert_eq!(
        store
            .counts()
            .expect("should count pending versus acknowledged")
            .retirement_pending,
        20
    );
}

#[test]
fn operator_retry_fences_late_negative_outcomes_and_schedules_one_fresh_generation() {
    let dir = tempfile::tempdir().expect("should create store");
    let store =
        Store::open(&dir.path().join("preparation.redb"), Bounds::default()).expect("should open");
    let page = target(1);
    store
        .admit(&page, JobOrigin::Signal, &[item('a')], 1)
        .expect("should admit");
    let old = store
        .claim(1, 1)
        .expect("should claim")
        .expect("should have work");
    store
        .clear_outcomes(&page, 2)
        .expect("should authorize explicit operator retry");
    store
        .record_claim_outcome(
            &old,
            &item('a').identity,
            "late_integrity_failure",
            OutcomeClass::Terminal,
            3,
        )
        .expect("should fence old failure");
    assert!(
        store
            .outcome(&page, &item('a').identity)
            .expect("should read")
            .is_none()
    );
    store
        .complete(&old.job.key, old.token, 4)
        .expect("should settle old claim");
    let fresh = store
        .claim(4, 2)
        .expect("should claim retry")
        .expect("should preserve retry");
    assert!(fresh.job.generation > old.job.generation);
    store
        .record_claim_outcome(
            &fresh,
            &item('a').identity,
            "current_failure",
            OutcomeClass::Terminal,
            5,
        )
        .expect("should record live failure");
    assert_eq!(
        store
            .outcome(&page, &item('a').identity)
            .expect("should read")
            .expect("should damp live outcome")
            .code,
        "current_failure"
    );
}

#[test]
fn stale_snapshot_completion_cannot_replace_follow_up_stability_evidence() {
    let dir = tempfile::tempdir().expect("should create store");
    let store =
        Store::open(&dir.path().join("preparation.redb"), Bounds::default()).expect("should open");
    let page = target(1);
    store
        .register(std::slice::from_ref(&page))
        .expect("should register independent target");
    store
        .admit(&page, JobOrigin::Signal, &[item('a')], 1)
        .expect("should admit");
    let old = store
        .claim(1, 1)
        .expect("should claim")
        .expect("should have work");
    assert_eq!(
        store
            .observe_claim_snapshot(&old, &[item('a').identity], 2)
            .expect("should observe live claim"),
        Some(Vec::new())
    );
    store
        .admit(&page, JobOrigin::Signal, &[item('b')], 3)
        .expect("should retain desired follow-up");
    assert_eq!(
        store
            .observe_claim_snapshot(&old, &[item('c').identity], 4)
            .expect("should fence out-of-order snapshot"),
        None
    );
    store
        .complete(&old.job.key, old.token, 5)
        .expect("should schedule follow-up");
    let next = store
        .claim(5, 2)
        .expect("should claim")
        .expect("should have follow-up");
    assert_eq!(
        store
            .observe_claim_snapshot(&next, &[item('b').identity], 6)
            .expect("should observe current generation"),
        Some(vec![item('a').identity])
    );
}

#[test]
fn reader_follow_up_cannot_consume_publisher_reservation_through_coalescing() {
    let dir = tempfile::tempdir().expect("should create store");
    let store = Store::open(
        &dir.path().join("preparation.redb"),
        Bounds {
            max_jobs: 2,
            reserved_jobs: 1,
            ..Bounds::default()
        },
    )
    .expect("should open");
    let publication = target(0);
    store
        .admit(&publication, JobOrigin::Warm, &[], 0)
        .expect("should admit publisher job");
    let publisher = store
        .claim(0, 1)
        .expect("should claim")
        .expect("should process publisher job");
    store
        .admit(&target(1), JobOrigin::Signal, &[item('a')], 1)
        .expect("should use the reader slot");
    assert_eq!(
        store
            .admit(&publication, JobOrigin::Signal, &[item('b')], 2)
            .expect("should account desired reader generation"),
        Admission::QueueFull
    );
    let retained = store
        .job(&publisher.job.key)
        .expect("should read")
        .expect("should keep publisher work");
    assert_eq!(retained.origin, JobOrigin::Warm);
    assert!(
        retained.reserved && !retained.follow_up,
        "refusal preserves publisher capacity and its in-flight claim"
    );
}
