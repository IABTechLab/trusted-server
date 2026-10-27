//! The probe host against a stand-in release written in WebAssembly text.
//!
//! The stand-in implements the probe ABI with no format logic: its
//! operations exercise the host contract (two sequential reads, refused
//! reads, a read budget, fuel, memory growth, an oversized result, a trap and
//! the error object), so no Encypher planner code enters this repository.

use std::cell::{Cell, RefCell};
use std::fmt;
use std::pin::Pin;
use std::task::{Context, Poll};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use ed25519_dalek::{Signer as _, SigningKey};
use futures::executor::block_on;
use serde_json::json;

use super::{
    MAX_ARTIFACT_BYTES, ProbeCallError, ProbeDescriptor, ProbeFailure, ProbeLimits, ProbeModule,
    ProbeSource, ReleaseAuthority, ReleaseError,
};
use crate::media_api::sha256_hex;

const READ_JSON: &str = r#"{"head":"____","next":"____"}"#;
const STATUS_JSON: &str = r#"{"status":0}"#;
const UNKNOWN_JSON: &str =
    r#"{"error":{"code":"UNKNOWN_OP","integrity":false,"message":"no such operation"}}"#;
const READ_FAILED_JSON: &str =
    r#"{"error":{"code":"READ_FAILED","integrity":false,"message":"the host refused a read"}}"#;

const VERSION_AT: usize = 1024;
const READ_AT: usize = 2048;
const STATUS_AT: usize = 3072;
const UNKNOWN_AT: usize = 4096;
const READ_FAILED_AT: usize = 5120;
/// WebAssembly page size, and the stand-in's initial linear memory.
const PAGE_BYTES: usize = 64 * 1024;
const INITIAL_PAGES: usize = 2;

const STAND_IN: &str = r#"
(module
  (import "env" "host_read" (func $host_read (param i64 i32 i32) (result i32)))
  @EXTRA_IMPORT@
  (memory (export "memory") @INITIAL_PAGES@)
  (global $heap (mut i32) (i32.const 16384))
  (global $result_ptr (mut i32) (i32.const 0))
  (global $result_len (mut i32) (i32.const 0))
  (data (i32.const @VERSION_AT@) "@VERSION_JSON@")
  (data (i32.const @READ_AT@) "@READ_JSON@")
  (data (i32.const @STATUS_AT@) "@STATUS_JSON@")
  (data (i32.const @UNKNOWN_AT@) "@UNKNOWN_JSON@")
  (data (i32.const @READ_FAILED_AT@) "@READ_FAILED_JSON@")
  (func (export "probe_abi") (result i32) (i32.const @ABI@))
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
  (func $report (param $value i32) (result i32)
    (i32.store8 (i32.const @STATUS_DIGIT@) (i32.add (i32.const 48) (local.get $value)))
    (call $answer (i32.const @STATUS_AT@) (i32.const @STATUS_LEN@) (i32.const 0)))
  (func $read_failed (result i32)
    (call $answer (i32.const @READ_FAILED_AT@) (i32.const @READ_FAILED_LEN@) (i32.const 1)))
  (func (export "probe_call")
    (param $op i32) (param $op_len i32) (param $arg i32) (param $arg_len i32) (result i32)
    (local $kind i32)
    (local.set $kind (i32.load8_u (local.get $op)))
    ;; "version"
    (if (i32.eq (local.get $kind) (i32.const 118))
      (then (return (call $answer (i32.const @VERSION_AT@) (i32.const @VERSION_LEN@) (i32.const 0)))))
    ;; "read": bytes 0..4, then bytes 4..8, into the template
    (if (i32.eq (local.get $kind) (i32.const 114))
      (then
        (if (call $host_read (i64.const 0) (i32.const 4) (i32.const @HEAD_SLOT@))
          (then (return (call $read_failed))))
        (if (call $host_read (i64.const 4) (i32.const 4) (i32.const @NEXT_SLOT@))
          (then (return (call $read_failed))))
        (return (call $answer (i32.const @READ_AT@) (i32.const @READ_LEN@) (i32.const 0)))))
    ;; "outside": a read past the end of any test source
    (if (i32.eq (local.get $kind) (i32.const 111))
      (then (return (call $report
        (call $host_read (i64.const 1000000) (i32.const 4) (i32.const @HEAD_SLOT@))))))
    ;; "memory": a destination past linear memory
    (if (i32.eq (local.get $kind) (i32.const 109))
      (then (return (call $report
        (call $host_read (i64.const 0) (i32.const 4) (i32.const -16))))))
    ;; "greedy": reads until one is refused
    (if (i32.eq (local.get $kind) (i32.const 103))
      (then
        (loop $again
          (br_if $again (i32.eqz
            (call $host_read (i64.const 0) (i32.const 4) (i32.const @HEAD_SLOT@)))))
        (return (call $read_failed))))
    ;; "Grow": memory to one page past the bound; reports 0 when refused
    (if (i32.eq (local.get $kind) (i32.const 71))
      (then (return (call $report
        (i32.add (i32.const 1) (memory.grow (i32.const @OVER_PAGES@)))))))
    ;; "allocate": memory up to exactly the bound; reports 0 when refused
    (if (i32.eq (local.get $kind) (i32.const 97))
      (then (return (call $report
        (i32.add (i32.const 1) (memory.grow (i32.const @FIT_PAGES@)))))))
    ;; "Beyond": one byte more than the per-read bound
    (if (i32.eq (local.get $kind) (i32.const 66))
      (then (return (call $report
        (call $host_read (i64.const 0) (i32.const @READ_BOUND_PLUS_ONE@) (i32.const 0))))))
    ;; "bound": exactly the per-read bound, into more than linear memory
    (if (i32.eq (local.get $kind) (i32.const 98))
      (then (return (call $report
        (call $host_read (i64.const 0) (i32.const @READ_BOUND@) (i32.const 0))))))
    ;; "flood": per-read-bound reads forever, ignoring every status
    (if (i32.eq (local.get $kind) (i32.const 102))
      (then
        (loop $again
          (drop (call $host_read (i64.const 0) (i32.const @READ_BOUND@) (i32.const 0)))
          (br $again))))
    ;; "echo": the argument as received
    (if (i32.eq (local.get $kind) (i32.const 101))
      (then (return (call $answer (local.get $arg) (local.get $arg_len) (i32.const 0)))))
    ;; "spin": never returns
    (if (i32.eq (local.get $kind) (i32.const 115))
      (then (loop $forever (br $forever))))
    ;; "huge": claims a 2 GiB result
    (if (i32.eq (local.get $kind) (i32.const 104))
      (then (return (call $answer (i32.const 0) (i32.const 0x7fffffff) (i32.const 0)))))
    ;; "x": traps
    (if (i32.eq (local.get $kind) (i32.const 120))
      (then (unreachable)))
    (call $answer (i32.const @UNKNOWN_AT@) (i32.const @UNKNOWN_LEN@) (i32.const 1)))
)
"#;

fn planner() -> String {
    "a".repeat(64)
}

fn wat_string(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

/// The stand-in release, reporting `abi` and `planner`, optionally with a
/// second import no release may have.
fn stand_in(abi: u32, planner: &str, extra_import: bool) -> Vec<u8> {
    let version = format!(r#"{{"planner_id":"{planner}"}}"#);
    let head_slot = READ_AT + READ_JSON.find("____").expect("should hold a head slot");
    let next_slot = READ_AT + READ_JSON.rfind("____").expect("should hold a next slot");
    let status_digit = STATUS_AT + STATUS_JSON.find('0').expect("should hold a digit");
    let extra = if extra_import {
        r#"(import "wasi_snapshot_preview1" "fd_write" (func (param i32 i32 i32 i32) (result i32)))"#
    } else {
        ""
    };
    let edge = ProbeLimits::default();
    let fit_pages = edge.memory_bytes / PAGE_BYTES - INITIAL_PAGES;
    let read_bound = edge.max_read_bytes;
    let text = STAND_IN
        .replace("@EXTRA_IMPORT@", extra)
        .replace("@ABI@", &abi.to_string())
        .replace("@VERSION_AT@", &VERSION_AT.to_string())
        .replace("@VERSION_JSON@", &wat_string(&version))
        .replace("@VERSION_LEN@", &version.len().to_string())
        .replace("@READ_AT@", &READ_AT.to_string())
        .replace("@READ_JSON@", &wat_string(READ_JSON))
        .replace("@READ_LEN@", &READ_JSON.len().to_string())
        .replace("@HEAD_SLOT@", &head_slot.to_string())
        .replace("@NEXT_SLOT@", &next_slot.to_string())
        .replace("@STATUS_AT@", &STATUS_AT.to_string())
        .replace("@STATUS_JSON@", &wat_string(STATUS_JSON))
        .replace("@STATUS_LEN@", &STATUS_JSON.len().to_string())
        .replace("@STATUS_DIGIT@", &status_digit.to_string())
        .replace("@UNKNOWN_AT@", &UNKNOWN_AT.to_string())
        .replace("@UNKNOWN_JSON@", &wat_string(UNKNOWN_JSON))
        .replace("@UNKNOWN_LEN@", &UNKNOWN_JSON.len().to_string())
        .replace("@READ_FAILED_AT@", &READ_FAILED_AT.to_string())
        .replace("@READ_FAILED_JSON@", &wat_string(READ_FAILED_JSON))
        .replace("@READ_FAILED_LEN@", &READ_FAILED_JSON.len().to_string())
        .replace("@INITIAL_PAGES@", &INITIAL_PAGES.to_string())
        .replace("@FIT_PAGES@", &fit_pages.to_string())
        .replace("@OVER_PAGES@", &(fit_pages + 1).to_string())
        .replace("@READ_BOUND@", &read_bound.to_string())
        .replace("@READ_BOUND_PLUS_ONE@", &(read_bound + 1).to_string());
    wat::parse_str(&text).expect("should assemble the stand-in release")
}

fn release_key() -> SigningKey {
    SigningKey::from_bytes(&[7; 32])
}

fn signed(
    key: &SigningKey,
    abi: u64,
    release: u64,
    planner: &str,
    sha256: &str,
) -> ProbeDescriptor {
    let mut descriptor = ProbeDescriptor {
        abi,
        planner_id: planner.to_owned(),
        release,
        sha256: sha256.to_owned(),
        signature: String::new(),
    };
    let signature = key.sign(descriptor.canonical_json().as_bytes());
    descriptor.signature = STANDARD.encode(signature.to_bytes());
    descriptor
}

fn authority(floor: u64, revoked: Vec<String>) -> ReleaseAuthority {
    ReleaseAuthority::pinned(vec![release_key().verifying_key()], floor, revoked)
}

/// The stand-in, signed as release 3, loaded under `limits`.
fn loaded(limits: ProbeLimits) -> ProbeModule {
    let artifact = stand_in(1, &planner(), false);
    let descriptor = signed(&release_key(), 1, 3, &planner(), &sha256_hex(&artifact));
    block_on(ProbeModule::load(
        &authority(1, Vec::new()),
        &descriptor,
        &artifact,
        limits,
    ))
    .expect("should load the signed stand-in")
}

fn load_error(artifact: &[u8], descriptor: &ProbeDescriptor) -> ReleaseError {
    block_on(ProbeModule::load(
        &authority(1, Vec::new()),
        descriptor,
        artifact,
        ProbeLimits::default(),
    ))
    .expect_err("should refuse the release")
}

/// The pinned version changed under the call.
#[derive(Debug, PartialEq, Eq)]
struct VersionChanged;

impl fmt::Display for VersionChanged {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("the source's ETag changed")
    }
}

/// Returns `Pending` once before completing, as a network read does.
#[derive(Default)]
struct YieldOnce {
    yielded: bool,
}

impl Future for YieldOnce {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<()> {
        if self.yielded {
            Poll::Ready(())
        } else {
            self.yielded = true;
            context.waker().wake_by_ref();
            Poll::Pending
        }
    }
}

/// An in-memory source whose every read first suspends.
struct AsyncSource {
    bytes: Vec<u8>,
    size: u64,
    fail: bool,
    reads: RefCell<Vec<(u64, usize)>>,
    suspensions: Cell<usize>,
}

impl AsyncSource {
    fn new(bytes: &[u8]) -> Self {
        Self {
            bytes: bytes.to_vec(),
            size: bytes.len() as u64,
            fail: false,
            reads: RefCell::new(Vec::new()),
            suspensions: Cell::new(0),
        }
    }

    fn failing(bytes: &[u8]) -> Self {
        Self {
            fail: true,
            ..Self::new(bytes)
        }
    }

    /// A source that claims `size` bytes and holds none, for reads the host
    /// must refuse before asking it.
    fn claiming(size: u64) -> Self {
        Self {
            size,
            ..Self::new(&[])
        }
    }

    fn reads(&self) -> Vec<(u64, usize)> {
        self.reads.borrow().clone()
    }
}

impl ProbeSource for AsyncSource {
    type Error = VersionChanged;

    fn size(&self) -> u64 {
        self.size
    }

    async fn read_exact_at(
        &self,
        offset: u64,
        destination: &mut [u8],
    ) -> Result<(), VersionChanged> {
        self.suspensions.set(self.suspensions.get() + 1);
        YieldOnce::default().await;
        self.reads.borrow_mut().push((offset, destination.len()));
        if self.fail {
            return Err(VersionChanged);
        }
        let start = usize::try_from(offset).expect("should fit a test offset");
        destination.copy_from_slice(&self.bytes[start..start + destination.len()]);
        Ok(())
    }
}

/// Release 8 as the production signing service advertised it, signed by
/// Encypher's production release key. Only the public descriptor is kept;
/// its artifact is not part of this repository.
const PRODUCTION_RELEASE_8: &str = r#"{
    "abi": 1,
    "planner_id": "0685f74f2b47b171a62be7d8f38224c85b5fdc1fc849d316e8c93910406ece04",
    "release": 8,
    "sha256": "45c8e965b33e6763a52a9164392fb584f1dd6d993e9583a9f51945caa4ebed59",
    "signature": "Mge6x1crHLQtuvn0eDjtapacOVTiEfU7/GOoxRd09e2bFGZaiSn9CvBlf/hSZs6Auvb2aRhWDVeOqnw+iTMMCw=="
}"#;

#[test]
fn production_authority_accepts_only_releases_encypher_signed() {
    let authority = ReleaseAuthority::production();
    let published: serde_json::Value =
        serde_json::from_str(PRODUCTION_RELEASE_8).expect("should parse the fixture");
    let published = ProbeDescriptor::parse(&published).expect("should be a descriptor");
    assert_eq!(
        authority.select(std::slice::from_ref(&published)),
        Ok(&published),
        "should accept the release Encypher published"
    );

    let mut retargeted = published.clone();
    retargeted.sha256 = "b".repeat(64);
    assert_eq!(
        authority.verify(&retargeted),
        Err(ReleaseError::Unsigned { release: 8 }),
        "should refuse the published signature over another artifact"
    );
    let foreign = signed(
        &release_key(),
        1,
        8,
        &published.planner_id,
        &published.sha256,
    );
    assert_eq!(
        authority.verify(&foreign),
        Err(ReleaseError::Unsigned { release: 8 }),
        "should refuse the same release signed by any other key"
    );
}

#[test]
fn descriptors_parse_only_in_the_contract_shape() {
    let descriptor = signed(&release_key(), 1, 3, &planner(), &"b".repeat(64));
    assert_eq!(
        ProbeDescriptor::parse(&descriptor.to_json()),
        Some(descriptor.clone())
    );
    assert_eq!(
        descriptor.canonical_json(),
        format!(
            r#"{{"abi":1,"planner_id":"{}","release":3,"sha256":"{}"}}"#,
            planner(),
            "b".repeat(64)
        ),
        "should sign the four fields, sorted, without whitespace"
    );
    assert_eq!(
        descriptor.artifact_path(),
        format!("sign/media/probe/{}.wasm", "b".repeat(64))
    );

    let mut upper = descriptor.to_json();
    upper["sha256"] = json!("B".repeat(64));
    let mut short = descriptor.to_json();
    short["planner_id"] = json!("a".repeat(63));
    let mut unsigned = descriptor.to_json();
    unsigned["signature"] = json!("");
    let mut textual = descriptor.to_json();
    textual["release"] = json!("3");
    for malformed in [&upper, &short, &unsigned, &textual] {
        assert_eq!(
            ProbeDescriptor::parse(malformed),
            None,
            "should refuse {malformed}"
        );
    }
    let list = json!([descriptor.to_json(), upper, "noise"]);
    assert_eq!(ProbeDescriptor::parse_list(Some(&list)), vec![descriptor]);
    assert!(ProbeDescriptor::parse_list(Some(&json!({}))).is_empty());
}

#[test]
fn verification_refuses_every_tampered_descriptor() {
    let authority = authority(2, vec!["c".repeat(64)]);
    let good = signed(&release_key(), 1, 3, &planner(), &"b".repeat(64));
    assert_eq!(authority.verify(&good), Ok(()));

    let mut retargeted = good.clone();
    retargeted.sha256 = "d".repeat(64);
    assert_eq!(
        authority.verify(&retargeted),
        Err(ReleaseError::Unsigned { release: 3 })
    );

    let foreign = signed(
        &SigningKey::from_bytes(&[9; 32]),
        1,
        3,
        &planner(),
        &"b".repeat(64),
    );
    assert_eq!(
        authority.verify(&foreign),
        Err(ReleaseError::Unsigned { release: 3 })
    );

    for signature in ["not base64!", "AAAA", &STANDARD.encode([0_u8; 63])] {
        let mut garbled = good.clone();
        garbled.signature = signature.to_owned();
        assert_eq!(
            authority.verify(&garbled),
            Err(ReleaseError::MalformedSignature { release: 3 }),
            "should refuse signature {signature:?}"
        );
    }

    let future_abi = signed(&release_key(), 2, 3, &planner(), &"b".repeat(64));
    assert_eq!(
        authority.verify(&future_abi),
        Err(ReleaseError::AbiUnsupported { abi: 2 })
    );
    assert_eq!(
        authority.verify(&future_abi).map_err(|error| error.code()),
        Err("PROBE_UPGRADE_REQUIRED")
    );

    let old = signed(&release_key(), 1, 1, &planner(), &"b".repeat(64));
    assert_eq!(
        authority.verify(&old),
        Err(ReleaseError::BelowFloor {
            release: 1,
            floor: 2
        })
    );

    let withdrawn = signed(&release_key(), 1, 4, &planner(), &"C".repeat(64));
    assert_eq!(
        authority.verify(&withdrawn),
        Err(ReleaseError::Revoked { release: 4 })
    );
}

#[test]
fn selection_takes_the_newest_compatible_release_and_never_falls_back() {
    let authority = authority(1, Vec::new());
    let older = signed(&release_key(), 1, 3, &planner(), &"b".repeat(64));
    let newer = signed(&release_key(), 1, 5, &planner(), &"c".repeat(64));
    let future = signed(&release_key(), 2, 9, &planner(), &"d".repeat(64));
    let advertised = [older.clone(), future.clone(), newer.clone()];
    assert_eq!(authority.select(&advertised), Ok(&newer));

    assert_eq!(authority.select(&[]), Err(ReleaseError::NoneAdvertised));
    assert_eq!(
        authority.select(std::slice::from_ref(&future)),
        Err(ReleaseError::NoCompatibleRelease)
    );

    let mut tampered = newer;
    tampered.planner_id = "e".repeat(64);
    assert_eq!(
        authority.select(&[older, tampered]),
        Err(ReleaseError::Unsigned { release: 5 }),
        "should refuse the newest rather than run an older release"
    );
}

#[test]
fn loading_refuses_artifacts_that_are_not_the_signed_release() {
    let artifact = stand_in(1, &planner(), false);
    let descriptor = signed(&release_key(), 1, 3, &planner(), &sha256_hex(&artifact));

    let mut tampered = artifact.clone();
    let last = tampered.len() - 1;
    tampered[last] ^= 1;
    assert_eq!(
        load_error(&tampered, &descriptor),
        ReleaseError::DigestMismatch
    );

    let oversized = vec![0_u8; MAX_ARTIFACT_BYTES + 1];
    let oversized_descriptor = signed(&release_key(), 1, 3, &planner(), &sha256_hex(&oversized));
    assert_eq!(
        load_error(&oversized, &oversized_descriptor),
        ReleaseError::ArtifactTooLarge {
            bytes: MAX_ARTIFACT_BYTES + 1
        }
    );

    let garbage = b"not a module".to_vec();
    let garbage_descriptor = signed(&release_key(), 1, 3, &planner(), &sha256_hex(&garbage));
    assert!(matches!(
        load_error(&garbage, &garbage_descriptor),
        ReleaseError::InvalidModule(_)
    ));
    let mut unsigned = garbage_descriptor;
    unsigned.release = 4;
    assert_eq!(
        load_error(&garbage, &unsigned),
        ReleaseError::Unsigned { release: 4 },
        "should refuse the descriptor before parsing its artifact"
    );
}

#[test]
fn loading_refuses_a_signed_release_that_breaks_the_abi() {
    let importing = stand_in(1, &planner(), true);
    let descriptor = signed(&release_key(), 1, 3, &planner(), &sha256_hex(&importing));
    assert_eq!(
        load_error(&importing, &descriptor),
        ReleaseError::ImportsMismatch
    );

    let misreporting = stand_in(2, &planner(), false);
    let descriptor = signed(&release_key(), 1, 3, &planner(), &sha256_hex(&misreporting));
    assert_eq!(
        load_error(&misreporting, &descriptor),
        ReleaseError::AbiReported { reported: 2 }
    );

    let other_planner = stand_in(1, &"f".repeat(64), false);
    let descriptor = signed(
        &release_key(),
        1,
        3,
        &planner(),
        &sha256_hex(&other_planner),
    );
    assert_eq!(
        load_error(&other_planner, &descriptor),
        ReleaseError::PlannerMismatch
    );
}

#[test]
fn reads_are_awaited_in_order_and_the_call_resumes_without_replay() {
    let probe = loaded(ProbeLimits::default());
    assert_eq!(probe.descriptor().release, 3);
    let source = AsyncSource::new(b"abcdefgh-rest-of-a-large-file");

    let value = block_on(probe.call("read", &json!({}), &source)).expect("should read");

    assert_eq!(value, json!({"head": "abcd", "next": "efgh"}));
    assert_eq!(
        source.reads(),
        vec![(0, 4), (4, 4)],
        "each read should reach the source once, in order"
    );
    assert_eq!(
        source.suspensions.get(),
        2,
        "each read should suspend the call"
    );

    let again = block_on(probe.call("read", &json!({}), &source)).expect("should read again");
    assert_eq!(
        again, value,
        "a second call should start from a fresh instance"
    );
    assert_eq!(source.reads().len(), 4);
}

#[test]
fn the_argument_reaches_the_release_intact() {
    let probe = loaded(ProbeLimits::default());
    let arg = json!({"plan": {"items": [1, 2, 3]}, "text": "caf\u{e9} \"quoted\""});

    let echoed =
        block_on(probe.call_without_source::<VersionChanged>("echo", &arg)).expect("should echo");

    assert_eq!(echoed, arg);
}

#[test]
fn reads_outside_the_source_or_memory_are_refused_without_asking_the_source() {
    let probe = loaded(ProbeLimits::default());
    let source = AsyncSource::new(b"abcdefgh");

    let outside = block_on(probe.call("outside", &json!({}), &source)).expect("should answer");
    assert_eq!(outside, json!({"status": 5}));

    let memory = block_on(probe.call("memory", &json!({}), &source)).expect("should answer");
    assert_eq!(memory, json!({"status": 4}));

    assert!(
        source.reads().is_empty(),
        "no refused read should reach the source"
    );
}

#[test]
fn the_edge_per_read_bound_admits_its_size_and_refuses_one_byte_more() {
    let probe = loaded(ProbeLimits::default());
    let bound = u64::from(ProbeLimits::default().max_read_bytes);
    let source = AsyncSource::claiming(bound + 1);

    let beyond = block_on(probe.call("Beyond", &json!({}), &source)).expect("should answer");
    assert_eq!(
        beyond,
        json!({"status": 5}),
        "a read over the bound should be refused"
    );

    let at_bound = block_on(probe.call("bound", &json!({}), &source)).expect("should answer");
    assert_eq!(
        at_bound,
        json!({"status": 4}),
        "a read at the bound should pass the bound and stop only at linear memory"
    );
    assert!(
        source.reads().is_empty(),
        "neither read should reach the source"
    );
}

#[test]
fn the_edge_total_read_budget_stops_a_release_that_keeps_reading() {
    let probe = loaded(ProbeLimits::default());
    let source = AsyncSource::claiming(u64::from(ProbeLimits::default().max_read_bytes));

    let error = block_on(probe.call("flood", &json!({}), &source)).expect_err("should stop");

    assert!(
        matches!(
            error,
            ProbeCallError::Failed(ProbeFailure::ReadBudgetExceeded { .. })
        ),
        "the read budget, not fuel, should stop the release: {error}"
    );
}

#[test]
fn the_read_budget_counts_every_admitted_read() {
    let probe = loaded(ProbeLimits {
        max_total_read_bytes: 8,
        ..ProbeLimits::default()
    });
    let source = AsyncSource::new(b"abcdefgh");

    let error = block_on(probe.call("greedy", &json!({}), &source)).expect_err("should stop");

    assert!(matches!(
        error,
        ProbeCallError::Failed(ProbeFailure::ReadBudgetExceeded { budget: 8 })
    ));
    assert_eq!(source.reads(), vec![(0, 4), (0, 4)]);
}

#[test]
fn a_failed_read_abandons_the_call_with_the_source_error() {
    let probe = loaded(ProbeLimits::default());
    let source = AsyncSource::failing(b"abcdefgh");

    let error = block_on(probe.call("read", &json!({}), &source)).expect_err("should fail");

    assert!(matches!(error, ProbeCallError::Source(VersionChanged)));
    assert_eq!(
        source.reads(),
        vec![(0, 4)],
        "no read should follow the failure"
    );
}

#[test]
fn edge_fuel_stops_a_release_that_never_returns() {
    let probe = loaded(ProbeLimits::default());

    let spin = block_on(probe.call_without_source::<VersionChanged>("spin", &json!({})));

    assert!(matches!(
        spin,
        Err(ProbeCallError::Failed(ProbeFailure::FuelExhausted))
    ));
}

#[test]
fn edge_memory_grows_to_its_bound_and_no_further() {
    let probe = loaded(ProbeLimits::default());

    let to_bound = block_on(probe.call_without_source::<VersionChanged>("allocate", &json!({})))
        .expect("should answer");
    assert_eq!(
        to_bound,
        json!({"status": 3}),
        "growth to exactly the bound should succeed"
    );

    let past_bound = block_on(probe.call_without_source::<VersionChanged>("Grow", &json!({})))
        .expect("should answer");
    assert_eq!(
        past_bound,
        json!({"status": 0}),
        "growth one page past the bound should fail inside the release"
    );
}

#[test]
fn an_oversized_result_is_refused() {
    let probe = loaded(ProbeLimits::default());

    let huge = block_on(probe.call_without_source::<VersionChanged>("huge", &json!({})));

    assert!(matches!(
        huge,
        Err(ProbeCallError::Failed(ProbeFailure::ResultTooLarge { .. }))
    ));
}

#[test]
fn traps_and_error_objects_are_reported_as_such() {
    let probe = loaded(ProbeLimits::default());

    let trap = block_on(probe.call_without_source::<VersionChanged>("x", &json!({})));
    assert!(matches!(
        trap,
        Err(ProbeCallError::Failed(ProbeFailure::Trap(_)))
    ));

    let unknown = block_on(probe.call_without_source::<VersionChanged>("unknown", &json!({})))
        .expect_err("should refuse");
    assert_eq!(unknown.code(), "UNKNOWN_OP");
    let ProbeCallError::Refused(refusal) = unknown else {
        panic!("should carry the release's error object");
    };
    assert!(!refusal.integrity);
    assert_eq!(refusal.message, "no such operation");
}
