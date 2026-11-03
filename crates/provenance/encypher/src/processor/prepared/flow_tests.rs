//! The prepared lane end to end, against a stand-in probe release signed with
//! a test key, a stub service and a stub origin.
//!
//! The stand-in has no format logic: each operation answers JSON the test
//! chose, so these tests exercise the lane's own orchestration and checks,
//! and no Encypher planner code enters this repository. Production always
//! holds releases to the pinned production key; only these tests pass
//! another authority.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use bytes::Bytes;
use ed25519_dalek::{Signer as _, SigningKey};
use edgezero_core::key_value_store::KvPage;
use futures::executor::block_on;
use http::HeaderMap;
use serde_json::{Value, json};
use trusted_server_core::media_signing::{
    LargeMediaJob, LargeSignedMedia, SourceVersion, VersionedMediaSource,
};
use trusted_server_core::platform::test_support::{
    StubHttpClient, build_services_with_http_client,
};
use trusted_server_core::platform::{
    KvError, PlatformHttpClient, PlatformKvStore, RuntimeServices,
};

use super::{LargeRefusal, Signer};
use crate::EncypherConfig;
use crate::media_api::sha256_hex;
use crate::prepared_probe::{ProbeDescriptor, ReleaseAuthority};

const POLICY: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const OTHER_POLICY: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const SOURCE_SHA: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
const SIZE: u64 = 20_000_000;
const READER_URL: &str = "https://publisher.example.com/video/ferry.mp4";
const ORIGIN_HOST: &str = "publisher.example.com";
const PLAN_ID: &str = "0f8fad5b-d9cb-469f-a165-70867728950e";
const STORE: &[u8] = b"jumb-c2pa-remote-manifest-store";

const STAND_IN: &str = r#"
(module
  (import "env" "host_read" (func $host_read (param i64 i32 i32) (result i32)))
  (memory (export "memory") 2)
  (global $heap (mut i32) (i32.const 16384))
  (global $result_ptr (mut i32) (i32.const 0))
  (global $result_len (mut i32) (i32.const 0))
  (data (i32.const 1024) "@VERSION@")
  (data (i32.const 2048) "@EXTRACT@")
  (data (i32.const 4096) "@VERIFY@")
  (data (i32.const 6144) "@CHECK@")
  (data (i32.const 8192) "@UNKNOWN@")
  (func (export "probe_abi") (result i32) (i32.const 1))
  (func (export "probe_alloc") (param $length i32) (result i32)
    (local $pointer i32)
    (local.set $pointer (global.get $heap))
    (global.set $heap (i32.add (global.get $heap) (local.get $length)))
    (local.get $pointer))
  (func (export "probe_free") (param i32 i32))
  (func (export "probe_result_ptr") (result i32) (global.get $result_ptr))
  (func (export "probe_result_len") (result i32) (global.get $result_len))
  (func $answer (param $pointer i32) (param $length i32) (param $code i32) (result i32)
    (global.set $result_ptr (local.get $pointer))
    (global.set $result_len (local.get $length))
    (local.get $code))
  (func (export "probe_call")
    (param $op i32) (param $op_len i32) (param $arg i32) (param $arg_len i32) (result i32)
    (local $kind i32)
    (local.set $kind (i32.load8_u (local.get $op)))
    ;; "version" (7 bytes) and "verify_plan" (11 bytes) both start with v
    (if (i32.and (i32.eq (local.get $kind) (i32.const 118)) (i32.eq (local.get $op_len) (i32.const 7)))
      (then (return (call $answer (i32.const 1024) (i32.const @VERSION_LEN@) (i32.const 0)))))
    (if (i32.eq (local.get $kind) (i32.const 118))
      (then (return (call $answer (i32.const 4096) (i32.const @VERIFY_LEN@) (i32.const 0)))))
    ;; "extract"
    (if (i32.eq (local.get $kind) (i32.const 101))
      (then (return (call $answer (i32.const 2048) (i32.const @EXTRACT_LEN@) (i32.const 0)))))
    ;; "check_store"
    (if (i32.eq (local.get $kind) (i32.const 99))
      (then (return (call $answer (i32.const 6144) (i32.const @CHECK_LEN@) (i32.const 0)))))
    (call $answer (i32.const 8192) (i32.const @UNKNOWN_LEN@) (i32.const 1)))
)
"#;

/// What each stand-in operation answers.
struct Answers {
    extract: Value,
    verify: Value,
    check: Value,
}

impl Default for Answers {
    fn default() -> Self {
        Self {
            extract: json!({
                "chunks": [{"offset": 0, "data": "AAAA"}],
                "has_provenance_reference": false,
            }),
            verify: json!({"hash_items": [{"range": [0, SIZE]}]}),
            check: json!({"ok": true}),
        }
    }
}

fn planner() -> String {
    "d".repeat(64)
}

fn wat_string(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

/// The stand-in release answering `answers`.
fn release(answers: &Answers) -> Vec<u8> {
    let version = json!({"planner_id": planner()}).to_string();
    let unknown =
        json!({"error": {"code": "UNKNOWN_OP", "integrity": false, "message": "no such op"}})
            .to_string();
    let extract = answers.extract.to_string();
    let verify = answers.verify.to_string();
    let check = answers.check.to_string();
    let text = STAND_IN
        .replace("@VERSION@", &wat_string(&version))
        .replace("@VERSION_LEN@", &version.len().to_string())
        .replace("@EXTRACT@", &wat_string(&extract))
        .replace("@EXTRACT_LEN@", &extract.len().to_string())
        .replace("@VERIFY@", &wat_string(&verify))
        .replace("@VERIFY_LEN@", &verify.len().to_string())
        .replace("@CHECK@", &wat_string(&check))
        .replace("@CHECK_LEN@", &check.len().to_string())
        .replace("@UNKNOWN@", &wat_string(&unknown))
        .replace("@UNKNOWN_LEN@", &unknown.len().to_string());
    wat::parse_str(&text).expect("should assemble the stand-in release")
}

fn release_key() -> SigningKey {
    SigningKey::from_bytes(&[11; 32])
}

/// The descriptor of `artifact`, signed with the test release key.
fn descriptor(artifact: &[u8]) -> ProbeDescriptor {
    let mut descriptor = ProbeDescriptor {
        abi: 1,
        planner_id: planner(),
        release: 3,
        sha256: sha256_hex(artifact),
        signature: String::new(),
    };
    let signature = release_key().sign(descriptor.canonical_json().as_bytes());
    descriptor.signature = STANDARD.encode(signature.to_bytes());
    descriptor
}

fn test_authority() -> ReleaseAuthority {
    ReleaseAuthority::pinned(vec![release_key().verifying_key()], 1, Vec::new())
}

/// A runtime KV store held in memory.
#[derive(Default)]
struct MemoryKv(Mutex<HashMap<String, Bytes>>);

impl MemoryKv {
    fn has(&self, key: &str) -> bool {
        self.0.lock().expect("should lock").contains_key(key)
    }
}

#[async_trait::async_trait(?Send)]
impl PlatformKvStore for MemoryKv {
    async fn get_bytes(&self, key: &str) -> Result<Option<Bytes>, KvError> {
        Ok(self.0.lock().expect("should lock").get(key).cloned())
    }

    async fn put_bytes(&self, key: &str, value: Bytes) -> Result<(), KvError> {
        self.0
            .lock()
            .expect("should lock")
            .insert(key.to_owned(), value);
        Ok(())
    }

    async fn put_bytes_with_ttl(
        &self,
        key: &str,
        value: Bytes,
        _ttl: std::time::Duration,
    ) -> Result<(), KvError> {
        self.put_bytes(key, value).await
    }

    async fn delete(&self, key: &str) -> Result<(), KvError> {
        self.0.lock().expect("should lock").remove(key);
        Ok(())
    }

    async fn list_keys_page(
        &self,
        _prefix: &str,
        _cursor: Option<&str>,
        _limit: usize,
    ) -> Result<KvPage, KvError> {
        Ok(KvPage::default())
    }
}

fn signer() -> Signer {
    let config: EncypherConfig = serde_json::from_value(json!({
        "api_base_url": "https://api.example.com",
        "domain": "publisher.example.com",
        "worker_token": "cwk_test_worker_token_value",
        "max_large_media_bytes": 1_073_741_824_u64,
    }))
    .expect("should parse config");
    Signer::new(config, 5_000_000)
}

fn finalize_answer() -> Value {
    json!({
        "protocol": "encypher-prepared-signing/2",
        "plan_id": PLAN_ID,
        "instance_id": "xmp:iid:0001",
        "document_id": "doc-1",
        "manifest_disposition": "external",
        "content_hash_kind": "hard_binding",
        "binding_algorithm": "c2pa.hash.data",
        "manifest_store_b64": STANDARD.encode(STORE),
        "manifest_sha256": sha256_hex(STORE),
        "manifest_url": "https://api.example.com/manifests/doc-1",
        "xmp_reference": "declined",
        "expected_final_size": SIZE,
        "verification_url": "https://verify.example.com/doc-1",
        "reused": false,
        "policy_digest": POLICY,
    })
}

fn prepare_answer() -> Value {
    json!({
        "protocol": "encypher-prepared-signing/2",
        "plan_id": PLAN_ID,
        "instance_id": "xmp:iid:0001",
        "document_id": "doc-1",
        "mime": "video/mp4",
        "effective_action": "c2pa.opened",
        "geometry": {"disposition": "external"},
        "local_ops": [],
        "policy_digest": POLICY,
    })
}

/// One run of the lane, and what it asked of the service and origin.
struct Run {
    result: Result<LargeSignedMedia, LargeRefusal>,
    http: Arc<StubHttpClient>,
    kv: Arc<MemoryKv>,
    signer: Signer,
}

impl Run {
    fn methods(&self) -> Vec<String> {
        self.http.recorded_request_methods()
    }

    fn refusal(&self) -> LargeRefusal {
        *self.result.as_ref().expect_err("should refuse")
    }

    fn body(&self, index: usize) -> Value {
        serde_json::from_slice(&self.http.recorded_request_bodies()[index])
            .expect("should send JSON")
    }
}

/// The scenario's answers, queued in the order the lane asks for them.
struct Scenario {
    answers: Answers,
    capabilities_policy: &'static str,
    /// The largest manifest store the service says it returns.
    manifest_decoded_bytes: usize,
    prepare: Value,
    finalize: Value,
}

impl Default for Scenario {
    fn default() -> Self {
        Self {
            answers: Answers::default(),
            capabilities_policy: POLICY,
            manifest_decoded_bytes: 1_048_576,
            prepare: prepare_answer(),
            finalize: finalize_answer(),
        }
    }
}

fn run(scenario: &Scenario) -> Run {
    let artifact = release(&scenario.answers);
    let probe = descriptor(&artifact);
    let http = Arc::new(StubHttpClient::new());
    http.set_streaming_responses_supported(true);
    http.push_response(
        200,
        serde_json::to_vec(&json!({"policy_digest": POLICY})).expect("JSON"),
    );
    http.push_response(
        200,
        serde_json::to_vec(&json!({
            "protocol": "encypher-prepared-signing/2",
            "enabled": true,
            "policy_digest": scenario.capabilities_policy,
            "formats": ["video/mp4"],
            "maximums": {
                "total_size_bytes": 9_007_199_246_352_383_u64,
                "structure_decoded_bytes": 33_554_432,
                "structure_chunks": 8192,
                "manifest_decoded_bytes": scenario.manifest_decoded_bytes,
                "prepare_request_bytes": 50_331_648,
            },
            "probe": [probe.to_json()],
        }))
        .expect("JSON"),
    );
    http.push_response(200, artifact);
    http.push_response(200, serde_json::to_vec(&scenario.prepare).expect("JSON"));
    // The pinned one-byte read that confirms the version before finalize.
    http.push_response_with_headers(
        206,
        b"\x00".to_vec(),
        vec![
            ("content-range", format!("bytes 0-0/{SIZE}")),
            ("content-length", "1".to_owned()),
            ("etag", "\"v1\"".to_owned()),
        ],
    );
    http.push_response(200, serde_json::to_vec(&scenario.finalize).expect("JSON"));

    let kv = Arc::new(MemoryKv::default());
    let services: RuntimeServices =
        build_services_with_http_client(Arc::clone(&http) as Arc<dyn PlatformHttpClient>)
            .with_kv_store(Arc::clone(&kv) as Arc<dyn PlatformKvStore>);
    let origin = url::Url::parse("https://publisher.example.com/video/ferry.mp4")
        .expect("should parse the origin URL");
    let headers = HeaderMap::new();
    let source = VersionedMediaSource::new(
        &services,
        &origin,
        &headers,
        "origin",
        SourceVersion::new("\"v1\"", SIZE).expect("should pin a strong ETag"),
    );
    let job = LargeMediaJob {
        reader_url: READER_URL,
        content_type: "video/mp4",
        source: &source,
        source_sha256: SOURCE_SHA,
        source_reference: Some(READER_URL),
        reader_reference: None,
        source_origin_host: ORIGIN_HOST,
        remaining: std::time::Duration::from_secs(60),
    };
    let signer = signer();
    let result = block_on(signer.sign_large_with(&services, &job, &test_authority()));
    Run {
        result,
        http,
        kv,
        signer,
    }
}

#[test]
fn an_unchanged_source_is_bound_whole_after_its_version_is_confirmed() {
    let run = run(&Scenario::default());

    run.result.as_ref().expect("should sign");
    assert_eq!(
        run.methods(),
        ["GET", "GET", "GET", "POST", "GET", "POST"],
        "should confirm the pinned version after prepare and before finalize"
    );
    let finalize = run.body(5);
    assert_eq!(
        finalize["binding"],
        json!({"type": "data_hash", "digest": SOURCE_SHA}),
        "should bind the whole-source digest"
    );
    assert_eq!(
        finalize["xmp_reference"], false,
        "should never edit the source"
    );
    let keys: Vec<String> = run
        .http
        .recorded_request_headers()
        .iter()
        .filter_map(|headers| {
            headers
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case("idempotency-key"))
                .map(|(_, value)| value.clone())
        })
        .collect();
    assert_eq!(keys.len(), 2, "should key prepare and finalize");
    assert_eq!(keys[0], keys[1], "should finalize under the plan's own key");
}

#[test]
fn a_manifest_larger_than_the_lane_takes_is_refused() {
    let run = run(&Scenario {
        manifest_decoded_bytes: STORE.len() - 1,
        ..Scenario::default()
    });
    assert_eq!(
        run.refusal().code(),
        "manifest_too_large",
        "should hold the store to the smaller of the service's and the edge's limit"
    );
}

#[test]
fn a_source_with_provenance_of_its_own_keeps_it_and_is_never_prepared() {
    for (extract, reason) in [
        (
            json!({
                "chunks": [{"offset": 0, "data": "AAAA"}],
                "credentials": {"manifests": 1},
                "has_provenance_reference": false,
            }),
            "source_has_embedded_provenance",
        ),
        (
            json!({
                "chunks": [{"offset": 0, "data": "AAAA"}],
                "has_provenance_reference": true,
            }),
            "source_has_provenance_reference",
        ),
    ] {
        let run = run(&Scenario {
            answers: Answers {
                extract,
                ..Answers::default()
            },
            ..Scenario::default()
        });

        let refusal = run.refusal();
        assert_eq!(refusal.code(), reason);
        assert!(refusal.retains_provenance(), "{reason}: should keep it");
        assert!(
            !run.methods().iter().any(|method| method == "POST"),
            "{reason}: should never prepare or finalize"
        );
    }
}

#[test]
fn a_release_that_does_not_report_a_provenance_reference_is_not_trusted() {
    for flag in [None, Some(json!("false")), Some(json!(0))] {
        let mut extract = json!({"chunks": [{"offset": 0, "data": "AAAA"}]});
        if let Some(flag) = flag.clone() {
            extract["has_provenance_reference"] = flag;
        }
        let run = run(&Scenario {
            answers: Answers {
                extract,
                ..Answers::default()
            },
            ..Scenario::default()
        });

        let refusal = run.refusal();
        assert_eq!(refusal.code(), "probe_result_invalid", "{flag:?}");
        assert!(!refusal.retains_provenance(), "{flag:?}: should not claim");
        assert!(
            !run.methods().iter().any(|method| method == "POST"),
            "{flag:?}: should never prepare"
        );
    }
}

#[test]
fn a_plan_that_is_not_the_unchanged_whole_source_is_never_finalized() {
    let mut edits = prepare_answer();
    edits["local_ops"] = json!([{"op": "xmp"}]);
    let mut edited = prepare_answer();
    edited["effective_action"] = json!("c2pa.edited");
    let mut embedded = prepare_answer();
    embedded["geometry"] = json!({"disposition": "embedded"});
    for prepare in [edits, edited, embedded] {
        let run = run(&Scenario {
            prepare,
            ..Scenario::default()
        });
        assert_eq!(run.refusal().code(), "prepare_answer_invalid");
        assert_eq!(run.methods().len(), 4, "should stop after prepare");
    }

    let run = run(&Scenario {
        answers: Answers {
            verify: json!({"hash_items": [{"range": [0, SIZE - 1]}]}),
            ..Answers::default()
        },
        ..Scenario::default()
    });
    assert_eq!(run.refusal().code(), "plan_not_whole_source");
    assert_eq!(
        run.methods().len(),
        4,
        "should stop before the version check"
    );
}

#[test]
fn a_finalized_answer_that_does_not_bind_the_source_is_never_delivered() {
    let mut resized = finalize_answer();
    resized["expected_final_size"] = json!(SIZE + 1);
    let mut swapped = finalize_answer();
    swapped["manifest_sha256"] = json!(sha256_hex(b"another store"));
    let mut embedded = finalize_answer();
    embedded["manifest_disposition"] = json!("embedded");
    for (finalize, reason) in [
        (resized, "finalize_answer_invalid"),
        (swapped, "signed_digest_mismatch"),
        (embedded, "finalize_answer_invalid"),
    ] {
        let run = run(&Scenario {
            finalize,
            ..Scenario::default()
        });
        assert_eq!(run.refusal().code(), reason);
    }

    let run = run(&Scenario {
        answers: Answers {
            check: json!({"ok": false}),
            ..Answers::default()
        },
        ..Scenario::default()
    });
    assert_eq!(
        run.refusal().code(),
        "signed_digest_mismatch",
        "should refuse a store that does not bind the whole-source digest"
    );
}

#[test]
fn a_changed_policy_evicts_the_cached_policy_and_publishes_nothing() {
    let changed = run(&Scenario {
        capabilities_policy: OTHER_POLICY,
        ..Scenario::default()
    });
    assert_eq!(changed.refusal().code(), "policy_changed");
    assert!(
        !changed.kv.has(&changed.signer.policy_key),
        "should evict the policy the lane held"
    );
    assert!(!changed.methods().iter().any(|method| method == "POST"));

    let mut prepared = prepare_answer();
    prepared["policy_digest"] = json!(OTHER_POLICY);
    let mismatched = run(&Scenario {
        prepare: prepared,
        ..Scenario::default()
    });
    assert_eq!(mismatched.refusal().code(), "policy_digest_mismatch");
    assert!(!mismatched.kv.has(&mismatched.signer.policy_key));

    let mut finalized = finalize_answer();
    finalized["policy_digest"] = json!(OTHER_POLICY);
    let late = run(&Scenario {
        finalize: finalized,
        ..Scenario::default()
    });
    assert_eq!(late.refusal().code(), "policy_digest_mismatch");
    assert!(!late.kv.has(&late.signer.policy_key));
}
