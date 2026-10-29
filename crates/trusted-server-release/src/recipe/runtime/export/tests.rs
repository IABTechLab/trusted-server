use std::time::Duration;

use super::super::{FastlyRuntimeAccepted, GateObservation, PublisherObservation, ViceroyOutput};
use crate::process::ProcessOutcome;
use crate::recipe::{AssetId, Role};
use crate::source::CommitId;

// A fictional observation used only to exercise export joins and formatting.
// Production acceptance is still minted only by the actual final-byte gate.
fn accepted() -> FastlyRuntimeAccepted {
    FastlyRuntimeAccepted {
        source_commit: CommitId::parse(&"a".repeat(40)).expect("should parse fictional source"),
        role: Role::Canonical,
        observed: GateObservation {
            artifact_sha256: "b".repeat(64),
            artifact_size: 12,
            viceroy_version: "0.21.0".to_owned(),
            viceroy_sha256: "c".repeat(64),
            fixture_sha256: "d".repeat(64),
            config_sha256: "e".repeat(64),
            health_status: 200,
            publisher: PublisherObservation {
                status: 200,
                body_size: 42,
                body_sha256: "f".repeat(64),
                script_src: "/static/tsjs=example".to_owned(),
            },
            origin_requests: vec!["GET / HTTP/1.1".to_owned()],
            startup: Duration::from_millis(1234),
            viceroy_outcome: ProcessOutcome::Cancelled,
            viceroy_output: ViceroyOutput {
                stdout_sha256: "1".repeat(64),
                stderr_sha256: "2".repeat(64),
                stderr_tail: "fixture output".to_owned(),
            },
        },
    }
}

#[test]
fn runtime_export_rejects_another_source_role_asset_digest_or_size() {
    let accepted = accepted();
    let source = accepted.source_commit();
    let fastly = AssetId::parse("trusted-server-fastly.wasm").expect("should parse Fastly asset");
    assert!(
        accepted
            .check_export_binding(source, Role::Canonical, fastly, &"b".repeat(64), 12)
            .is_ok(),
        "should accept the exact runtime build binding"
    );
    for (commit, role, asset, digest, size) in [
        (
            CommitId::parse(&"f".repeat(40)).expect("should parse different source"),
            Role::Canonical,
            fastly,
            "b".repeat(64),
            12,
        ),
        (
            source.clone(),
            Role::Reproduction,
            fastly,
            "b".repeat(64),
            12,
        ),
        (
            source.clone(),
            Role::Canonical,
            AssetId::parse("ts-x86_64-unknown-linux-gnu").expect("should parse native asset"),
            "b".repeat(64),
            12,
        ),
        (source.clone(), Role::Canonical, fastly, "c".repeat(64), 12),
        (source.clone(), Role::Canonical, fastly, "b".repeat(64), 13),
    ] {
        assert!(
            accepted
                .check_export_binding(&commit, role, asset, &digest, size)
                .is_err(),
            "should reject a runtime acceptance attached to another build"
        );
    }
}

#[test]
fn runtime_export_preserves_actual_gate_facts_and_integer_duration() {
    let accepted = accepted();
    let record = accepted
        .diagnostic_record()
        .expect("should export checked diagnostic facts");
    assert_eq!(record["role"], "A", "should retain the accepted build role");
    assert_eq!(
        record["viceroy"]["sha256"],
        "c".repeat(64),
        "should retain actual runtime executable identity"
    );
    assert_eq!(
        record["publisher"]["body_sha256"],
        "f".repeat(64),
        "should retain accepted publisher body digest"
    );
    assert_eq!(
        record["startup_milliseconds"], 1234,
        "should avoid lossy floating-point duration"
    );
    assert_eq!(
        record["release_qualification"], false,
        "should grant no release authority"
    );
}

#[test]
fn runtime_export_rejects_oversized_records_and_duration_overflow() {
    let mut oversized = accepted();
    oversized.observed.origin_requests = vec!["x".repeat(1024 * 1024)];
    assert!(
        oversized.diagnostic_record().is_err(),
        "should bound retained runtime diagnostic bytes"
    );
    let mut duration = accepted();
    duration.observed.startup = Duration::MAX;
    assert!(
        duration.diagnostic_record().is_err(),
        "should reject integer duration overflow"
    );
}
