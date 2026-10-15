//! Offline build evidence uses independent roots and pins; never publication.

use crate::release::bounds::{AttemptDeadline, Cancellation, ProcessBounds};
use crate::release::documents::sha256_hex;
use crate::release::environment::CallerEnvironment;
use crate::release::gh::{GhLocation, GhRunner};
use crate::release::offline::{OfflineApproval, verify_build};
use crate::release::policy::EMBEDDED_POLICY;
use crate::release::source_policy::ApprovedSourceRoot;
use crate::release::tests::online::Case;
use crate::release::tests::verification::fixture::{self, ASSET, COMMIT};
use std::fs;
#[cfg(target_os = "macos")]
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt as _;
#[cfg(target_os = "macos")]
use std::process::Command;
#[cfg(target_os = "macos")]
use std::time::Duration;

#[test]
fn offline_runner_requires_independent_roots_digest_without_token() {
    let directory = tempfile::tempdir().expect("should create private directory");
    let gh = directory.path().join("gh");
    fs::write(&gh, "#!/bin/sh\n[ -z \"$GH_TOKEN$GITHUB_TOKEN\" ] || exit 9\necho 'gh version 2.97.0 (2026-01-01)'\n").expect("should write fake gh");
    fs::set_permissions(&gh, fs::Permissions::from_mode(0o700)).expect("should mark executable");
    let roots = directory.path().join("trusted_root.jsonl");
    fs::write(&roots, b"fictional independent root snapshot\n").expect("should write roots");
    let caller = CallerEnvironment::default();
    let resolve = |pin: &str| {
        GhRunner::resolve_offline(
            GhLocation::Explicit(&gh),
            &caller,
            ProcessBounds::production(),
            AttemptDeadline::start(),
            &Cancellation::default(),
            &roots,
            pin,
        )
    };
    assert!(
        resolve(&"0".repeat(64)).is_err(),
        "should reject transferred roots that do not match independent pin"
    );
    assert!(
        resolve(&sha256_hex(b"fictional independent root snapshot\n")).is_ok(),
        "should resolve offline with token absent"
    );
}

async fn verify_case(case: &Case, pin: &str) -> bool {
    let source_directory = case.directory.path().join("approved-source");
    fs::create_dir_all(source_directory.join("crates/trusted-server-cli"))
        .expect("should create approved source layout");
    fs::write(
        source_directory.join("crates/trusted-server-cli/release-policy.json"),
        EMBEDDED_POLICY,
    )
    .expect("should write independently approved policy");
    let release = fixture::approved(COMMIT);
    let source = ApprovedSourceRoot::new(release.commit().clone(), &source_directory)
        .expect("should open approved source root");
    let roots = case.directory.path().join("trusted_root.jsonl");
    fs::write(&roots, b"fictional independently transported roots\n")
        .expect("should write root snapshot");
    let roots_sha256 = sha256_hex(b"fictional independently transported roots\n");
    let approval = OfflineApproval {
        release: &release,
        source: &source,
        asset: ASSET,
        artifact_sha256: pin,
        roots: &roots,
        roots_sha256: &roots_sha256,
        gh: &case.gh,
    };
    verify_build(
        &approval,
        case.directory.path(),
        &CallerEnvironment::default(),
        AttemptDeadline::start(),
        &Cancellation::default(),
    )
    .await
    .is_ok()
}

#[tokio::test]
async fn offline_complete_synthetic_graph_checks_independent_artifact_pin() {
    let case = Case::new();
    case.write_assets();
    assert!(
        verify_case(&case, &sha256_hex(&case.signed.documents.executable)).await,
        "should verify synthetic build graph without token"
    );
    assert!(
        !verify_case(&case, &"0".repeat(64)).await,
        "should reject a substituted independently pinned artifact"
    );
}

#[test]
#[cfg(target_os = "macos")]
fn offline_build_verification_runs_under_proven_os_network_denial() {
    const CHILD: &str = "TS_INTERNAL_OFFLINE_DENIAL_TEST_CHILD";
    if let Ok(address) = std::env::var(CHILD) {
        let address: SocketAddr = address.parse().expect("should parse control address");
        assert!(
            TcpStream::connect_timeout(&address, Duration::from_secs(1)).is_err(),
            "should deny socket inside offline verification boundary"
        );
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("should build offline runtime");
        let case = Case::new();
        case.write_assets();
        assert!(
            runtime.block_on(verify_case(
                &case,
                &sha256_hex(&case.signed.documents.executable)
            )),
            "should verify local build evidence with networking denied and tokens absent"
        );
        return;
    }
    let listener = TcpListener::bind("127.0.0.1:0").expect("should bind unconfined socket control");
    let address = listener.local_addr().expect("should read control address");
    let _control = TcpStream::connect_timeout(&address, Duration::from_secs(1))
        .expect("should prove unconfined socket succeeds");
    let output = Command::new("/usr/bin/sandbox-exec")
        .args(["-p", "(version 1)(allow default)(deny network*)"])
        .arg(std::env::current_exe().expect("should locate test binary"))
        .args(["--exact", "release::tests::offline::offline_build_verification_runs_under_proven_os_network_denial", "--nocapture"])
        .env_clear().env("PATH", "/usr/bin:/bin").env(CHILD, address.to_string())
        .output().expect("should execute offline test under network denial");
    assert!(
        output.status.success(),
        "should pass denied-network offline build test: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
async fn actual_offline_proofs_cannot_upgrade_through_online_publication_composition() {
    let case = Case::new();
    let roots = case.directory.path().join("trusted_root.jsonl");
    fs::write(&roots, b"fictional independently approved roots")
        .expect("should write pinned roots");
    let runner = GhRunner::resolve_offline(
        GhLocation::Explicit(&case.gh),
        &CallerEnvironment::default(),
        ProcessBounds::production(),
        AttemptDeadline::start(),
        &Cancellation::default(),
        &roots,
        &sha256_hex(b"fictional independently approved roots"),
    )
    .expect("should resolve token-free offline runner");
    let release = fixture::approved(COMMIT);
    let built = crate::release::online::build_with(
        &case,
        &runner,
        &release,
        ASSET,
        AttemptDeadline::start(),
        &Cancellation::default(),
    )
    .await
    .expect("should build actual offline-mode proofs through fake child");
    let online_statement = case
        .runner()
        .verify_release(&release, AttemptDeadline::start(), &Cancellation::default())
        .expect("should obtain separately successful online-mode release proof");
    let result = crate::release::github::verify_publication(
        built.build,
        &crate::release::github::PublicationInputs {
            release: &case.signed.publication.release,
            tag_ref: &case.signed.publication.tag_ref,
            release_statement: &online_statement,
            index_bundle: &built.index_bundle,
            checksums: &crate::release::artifact::AssetMeasurement::of_bytes(
                &case.signed.publication.checksums,
            ),
        },
    );
    assert!(
        result.is_err(),
        "should prevent offline proofs upgrading through otherwise valid online publication data"
    );
}
