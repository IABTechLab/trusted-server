//! Real redb/filesystem scenarios. Probe bytes here exercise storage, not probe execution.
use std::sync::{Arc, Barrier};

use bytes::Bytes;
use sha2::{Digest as _, Sha256};
use trusted_server_preparation_worker::client::DiskProbeCache;
use trusted_server_preparation_worker::store::{Bounds, Store};
use trusted_server_provenance_encypher::preparation::pipeline::{
    ProbeArtifactCache, TextCacheScope, TextSignRequest,
};

fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[test]
fn four_tasks_cannot_overcommit_probe_files_and_restart_keeps_their_charge() {
    let dir = tempfile::tempdir().expect("should create storage");
    let probe = Bytes::from(
        std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../provenance/encypher/src/fixtures/prepared/probe.wasm"),
        )
        .expect("should read the common real probe"),
    );
    let probe_bytes = probe.len() as u64;
    let bounds = Bounds {
        quota_bytes: probe_bytes + 300,
        ..Bounds::default()
    };
    {
        let store = Arc::new(
            Store::open(&dir.path().join("preparation.redb"), bounds).expect("should open"),
        );
        let barrier = Arc::new(Barrier::new(4));
        let tasks: Vec<_> = (0..4)
            .map(|index| {
                let store = Arc::clone(&store);
                let barrier = Arc::clone(&barrier);
                let bytes = probe.clone();
                std::thread::spawn(move || {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .expect("should build runtime");
                    let cache = DiskProbeCache::new(Arc::clone(&store));
                    let sha = digest(&bytes);
                    runtime
                        .block_on(cache.put(&sha, bytes))
                        .expect("should retain shared real probe");
                    barrier.wait();
                    store
                        .reserve(&format!("temp:task:{index}"), 100)
                        .expect("should reserve shared capacity")
                })
            })
            .collect();
        let admitted = tasks
            .into_iter()
            .map(|task| task.join().expect("should join"))
            .filter(|admitted| *admitted)
            .count();
        assert_eq!(
            admitted, 3,
            "real probe plus three task budgets exactly reaches the quota; the fourth refuses"
        );
        assert_eq!(store.reserved().expect("should count"), probe_bytes + 300);
        assert!(!store.reserve("temp:fourth", 1).expect("should reserve"));
    }
    let store = Store::open(&dir.path().join("preparation.redb"), bounds).expect("should reopen");
    store
        .recover_storage()
        .expect("should reconcile real files");
    assert_eq!(
        store.reserved().expect("should count"),
        probe_bytes,
        "restart reclaims dead task budgets, never the retained real probe"
    );
}

#[tokio::test]
async fn real_probe_cache_refuses_before_writing_when_its_durable_reservation_cannot_fit() {
    let dir = tempfile::tempdir().expect("should create storage");
    let probe = Bytes::from(
        std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../provenance/encypher/src/fixtures/prepared/probe.wasm"),
        )
        .expect("should read common real probe"),
    );
    let sha = digest(&probe);
    let store = Arc::new(
        Store::open(
            &dir.path().join("preparation.redb"),
            Bounds {
                quota_bytes: probe.len() as u64 - 1,
                ..Bounds::default()
            },
        )
        .expect("should open"),
    );
    let cache = DiskProbeCache::new(Arc::clone(&store));
    let failure = cache
        .put(&sha, probe)
        .await
        .expect_err("should refuse unreserved probe retention");
    assert_eq!(failure.code, "quota_exhausted");
    assert!(cache.get(&sha).await.is_none());
    assert_eq!(store.reserved().expect("should count"), 0);
}

#[tokio::test]
async fn duplicate_probe_writes_are_charged_once_and_corrupt_cache_is_not_returned() {
    let dir = tempfile::tempdir().expect("should create storage");
    let store = Arc::new(
        Store::open(
            &dir.path().join("preparation.redb"),
            Bounds {
                quota_bytes: 10,
                ..Bounds::default()
            },
        )
        .expect("should open"),
    );
    let cache = DiskProbeCache::new(Arc::clone(&store));
    let bytes = Bytes::from_static(b"probe-data");
    let sha = digest(&bytes);
    cache
        .put(&sha, bytes.clone())
        .await
        .expect("should retain probe");
    cache
        .put(&sha, bytes.clone())
        .await
        .expect("should reuse probe");
    assert_eq!(cache.get(&sha).await, Some(bytes));
    assert_eq!(store.reserved().expect("should count"), 10);
    std::fs::write(
        dir.path().join("probes").join(format!("{sha}.wasm")),
        b"tampered!!",
    )
    .expect("should tamper");
    assert!(
        cache.get(&sha).await.is_none(),
        "digest-addressed files must verify on read"
    );
    assert_eq!(
        store.reserved().expect("should count"),
        10,
        "corruption does not release a file's charge"
    );
}

#[test]
fn restart_accounts_legacy_unreserved_probe_and_preserves_overquota_evidence() {
    let dir = tempfile::tempdir().expect("should create storage");
    std::fs::create_dir(dir.path().join("probes")).expect("should create probes");
    let bytes = b"retained probe";
    let path = dir
        .path()
        .join("probes")
        .join(format!("{}.wasm", digest(bytes)));
    std::fs::write(&path, bytes).expect("should write legacy probe");
    let store = Store::open(
        &dir.path().join("preparation.redb"),
        Bounds {
            quota_bytes: 5,
            ..Bounds::default()
        },
    )
    .expect("should open");
    assert!(
        store.recover_storage().is_err(),
        "cannot report ready with historical bytes over quota"
    );
    assert_eq!(
        std::fs::read(path).expect("should preserve retained file"),
        bytes
    );
}

#[test]
fn restart_releases_temporary_ownership_only_after_removing_its_actual_file() {
    let dir = tempfile::tempdir().expect("should create storage");
    let bounds = Bounds {
        quota_bytes: 100,
        ..Bounds::default()
    };
    {
        let store = Store::open(&dir.path().join("preparation.redb"), bounds).expect("should open");
        assert!(
            store
                .reserve("temp:large:dead-claim", 80)
                .expect("should reserve")
        );
        std::fs::create_dir(dir.path().join("large")).expect("should create temporary directory");
        std::fs::write(dir.path().join("large/dead-claim"), [0; 80])
            .expect("should write temporary source");
    }
    let store = Store::open(&dir.path().join("preparation.redb"), bounds).expect("should reopen");
    store
        .recover_storage()
        .expect("should reclaim inactive temporary source");
    assert!(!dir.path().join("large/dead-claim").exists());
    assert_eq!(store.reserved().expect("should count"), 0);
    assert!(
        store
            .reserve("temp:new", 100)
            .expect("should reserve reclaimed capacity")
    );
}

#[test]
fn restart_refuses_legacy_or_unknown_text_scope_in_retained_publication() {
    for scope in [None, Some(serde_json::json!("legacy"))] {
        let dir = tempfile::tempdir().expect("should create storage");
        let path = dir.path().join("preparation.redb");
        let identity = "a".repeat(64);
        let mut artifact = serde_json::json!({
            "envelope": {
                "protocol": "encypher-preparation/1",
                "deployment_id": "deployment",
                "identity": identity,
                "retirement_epoch": 0,
                "variant": "",
                "item": {
                    "kind": "text",
                    "request": {
                        "text": "A retained article",
                        "page_url": "https://www.example.com/article",
                        "document_title": "Harbor",
                        "boundary_selector": "article",
                        "expected_policy_digest": "b".repeat(64),
                    },
                    "idempotency_key": "retained-operation",
                },
            },
            "target": {"path": "/article", "variant": ""},
            "acknowledged": false,
        });
        if let Some(scope) = scope {
            artifact["envelope"]["item"]["request"]["cache_scope"] = scope;
        }
        {
            let store = Store::open(&path, Bounds::default()).expect("should open storage");
            store
                .recover_storage()
                .expect("should initialize object directories");
            std::fs::write(
                dir.path()
                    .join("artifacts")
                    .join(format!("{identity}.json")),
                serde_json::to_vec(&artifact).expect("should encode the retained publication"),
            )
            .expect("should retain a pre-cutover artifact at the filesystem boundary");
        }
        let store = Store::open(&path, Bounds::default()).expect("should reopen storage");
        assert!(
            store.recover_storage().is_err(),
            "unsupported scope must block recovery, never adopt a legacy publication"
        );
        assert!(
            store
                .artifact(&identity)
                .expect("should read metadata")
                .is_none(),
            "unsupported text must not become a publishable retained result"
        );
    }
}

#[test]
fn large_text_attempt_is_streamed_retained_and_retired_without_losing_its_spent_key() {
    let dir = tempfile::tempdir().expect("should create storage");
    let bounds = Bounds {
        quota_bytes: 3 * 1024 * 1024,
        ..Bounds::default()
    };
    let identity = "a".repeat(64);
    let request = TextSignRequest {
        text: "a".repeat(2 * 1024 * 1024),
        page_url: "https://www.example.com/article".to_owned(),
        document_title: None,
        boundary_selector: "article".to_owned(),
        expected_policy_digest: "b".repeat(64),
        cache_scope: TextCacheScope::ExactRequest,
    };
    {
        let store =
            Store::open(&dir.path().join("preparation.redb"), bounds).expect("should own store");
        assert!(
            store
                .begin_text(&identity, &request)
                .expect("should persist before first signing attempt")
        );
        let path = dir.path().join("texts").join(format!("{identity}.json"));
        let persisted: serde_json::Value = serde_json::from_slice(
            &std::fs::read(&path).expect("should retain the exact text request"),
        )
        .expect("should store a JSON request");
        assert_eq!(persisted["cache_scope"], "exact_request");
        assert_eq!(
            serde_json::from_value::<TextSignRequest>(persisted)
                .expect("should recover the current cache contract"),
            request
        );
        assert!(
            std::fs::metadata(path)
                .expect("should stream a filesystem object")
                .len()
                > request.text.len() as u64
        );
        assert!(
            store.reserved().expect("should charge retained input") > request.text.len() as u64
        );
    }
    let store = Store::open(&dir.path().join("preparation.redb"), bounds).expect("should reopen");
    store
        .recover_storage()
        .expect("should retain reconstruction inputs across restart");
    assert!(
        !store
            .begin_text(&identity, &request)
            .expect("should require non-signing recovery for an existing attempt")
    );
    let mut substituted = request.clone();
    substituted.text.push('x');
    assert!(
        store.begin_text(&identity, &substituted).is_err(),
        "one identity cannot change its frozen request"
    );
    let epoch = store
        .retire(&identity, "deployment")
        .expect("should retire through durable owner");
    assert!(
        !dir.path()
            .join("texts")
            .join(format!("{identity}.json"))
            .exists()
    );
    assert_eq!(
        store
            .reserved()
            .expect("should keep funded spent-key material"),
        1024
    );
    let marker = store
        .outbox()
        .expect("should commit delayed marker with spent key")
        .pop()
        .expect("should retain marker");
    assert_eq!(marker.epoch, epoch);
    drop(store);
    let store = Store::open(&dir.path().join("preparation.redb"), bounds)
        .expect("should reopen after retirement");
    store
        .recover_storage()
        .expect("should preserve retirement while reconciling real files");
    assert_eq!(
        store
            .reserved()
            .expect("should preserve funded spent key across restart"),
        1024
    );
    assert_eq!(
        store
            .outbox()
            .expect("should retain pending marker across restart"),
        vec![marker]
    );
    let retirement = store
        .retirement(&identity)
        .expect("should read spent key")
        .expect("should preserve tombstone");
    assert_eq!(retirement.epoch, epoch);
    assert!(!retirement.acknowledged);
    assert!(
        store.begin_text(&identity, &request).is_err(),
        "retirement must refuse the original signing request after restart"
    );
    assert!(
        store.begin_text(&identity, &substituted).is_err(),
        "a changed request must not reuse the retired operation identity"
    );
    assert!(
        !dir.path()
            .join("texts")
            .join(format!("{identity}.json"))
            .exists(),
        "refused resurrection must not recreate the retained request"
    );
    assert_eq!(
        store
            .reserved()
            .expect("should not allocate a fresh signing operation"),
        1024
    );
}

#[test]
fn ambiguous_temporary_file_blocks_cleanup_without_releasing_any_reservation() {
    let dir = tempfile::tempdir().expect("should create storage");
    let store = Store::open(
        &dir.path().join("preparation.redb"),
        Bounds {
            quota_bytes: 100,
            ..Bounds::default()
        },
    )
    .expect("should open");
    store
        .reserve("temp:known", 80)
        .expect("should reserve known ownership");
    std::fs::create_dir(dir.path().join("large")).expect("should create temporary directory");
    let path = dir.path().join("large/ambiguous");
    std::fs::write(&path, b"unowned evidence").expect("should create uncertain crash evidence");
    assert!(
        store.recover_storage().is_err(),
        "unowned material must not be deleted to manufacture quota"
    );
    assert!(path.exists());
    assert_eq!(
        store
            .reserved()
            .expect("should preserve ownership accounting"),
        80
    );
}

#[test]
fn expired_negative_outcomes_release_their_charge_but_terminal_outcomes_do_not_time_expire() {
    let dir = tempfile::tempdir().expect("should create storage");
    let store =
        Store::open(&dir.path().join("preparation.redb"), Bounds::default()).expect("should open");
    let target = trusted_server_provenance_encypher::preparation::protocol::Target {
        path: "/article".to_owned(),
        variant: String::new(),
    };
    store
        .record_outcome(
            &target,
            &"a".repeat(64),
            "source_unstable",
            trusted_server_preparation_worker::store::OutcomeClass::SourceUnstable,
            0,
        )
        .expect("should retain bounded negative state");
    store
        .record_outcome(
            &target,
            &"b".repeat(64),
            "source_private",
            trusted_server_preparation_worker::store::OutcomeClass::Terminal,
            0,
        )
        .expect("should retain terminal state");
    let before = store
        .reserved()
        .expect("should charge all retained outcome bytes");
    assert_eq!(
        store
            .prune_expired_outcomes(301)
            .expect("should reclaim only expired state"),
        1
    );
    assert!(
        store
            .reserved()
            .expect("should release expired bytes atomically")
            < before
    );
    assert_eq!(
        store
            .active_outcome_counts(301)
            .expect("should expose refusal reasons")
            .get("source_private"),
        Some(&1)
    );
    assert!(
        store
            .outcome(&target, &"b".repeat(64))
            .expect("should preserve terminal suppression")
            .is_some()
    );
}
