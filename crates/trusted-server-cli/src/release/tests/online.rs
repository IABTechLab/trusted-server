//! Deterministic transport and retained-byte orchestration tests.

use crate::release::artifact::{AssetRole, RetainedAsset, RetainedDownload};
use crate::release::bounds::{AttemptDeadline, Cancellation, ProcessBounds};
use crate::release::documents::ApprovedCommit;
use crate::release::environment::CallerEnvironment;
use crate::release::error::{ReleaseError, report};
use crate::release::gh::{GhLocation, GhRunner};
use crate::release::online::{Transport, verify_with};
use crate::release::online::{allowed_redirect, approved_asset_url, validate_request};
use crate::release::policy::EMBEDDED_POLICY;
use crate::release::source_policy::ApprovedSourcePolicy;
use crate::release::tests::verification::fixture::{self, ASSET, Signed};
use error_stack::Report;
use serde_json::{Value, json};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;
use tempfile::TempDir;
use url::Url;

const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

#[test]
fn inputs_are_validated_without_environment() {
    assert!(
        validate_request("v1.2.0", "ts-aarch64-apple-darwin", COMMIT).is_ok(),
        "should accept required public path"
    );
    for (tag, asset, commit) in [
        ("latest", "ts-aarch64-apple-darwin", COMMIT),
        ("v1.2.0", "../../ts", COMMIT),
        ("v1.2.0", "ts-aarch64-apple-darwin", "main"),
    ] {
        assert!(
            validate_request(tag, asset, commit).is_err(),
            "should reject invalid request before any I/O"
        );
    }
}

#[test]
fn download_url_and_redirects_are_fixed_and_credential_free() {
    let release = validate_request("v1.2.0", "ts-aarch64-apple-darwin", COMMIT)
        .expect("should validate request");
    let url =
        approved_asset_url(&release, "release-index.json").expect("should construct canonical URL");
    assert_eq!(
        url.as_str(),
        "https://github.com/IABTechLab/trusted-server/releases/download/v1.2.0/release-index.json",
        "should use fixed official repository"
    );
    for value in [
        "https://example.com/file",
        "http://release-assets.githubusercontent.com/file", // allow-domain: release-assets.githubusercontent.com
        "https://user:secret@release-assets.githubusercontent.com/file", // allow-domain: release-assets.githubusercontent.com
        "https://release-assets.githubusercontent.com:444/file", // allow-domain: release-assets.githubusercontent.com
        "https://release-assets.githubusercontent.com/file#fragment", // allow-domain: release-assets.githubusercontent.com
        "https://github.com/other/file",
    ] {
        assert!(
            !allowed_redirect(&Url::parse(value).expect("should parse URL")),
            "should reject unapproved redirect {value}"
        );
    }
    assert!(
        allowed_redirect(
            &Url::parse("https://release-assets.githubusercontent.com/file?signature=fictional") // allow-domain: release-assets.githubusercontent.com
                .expect("should parse CDN URL")
        ),
        "should allow documented release CDN"
    );
    assert!(
        approved_asset_url(&release, "../bad").is_err(),
        "should reject traversal"
    );
}

#[test]
fn retained_materialization_preserves_canonical_bytes_without_overwrite() {
    let directory = tempfile::tempdir().expect("should create private directory");
    let mut download = RetainedDownload::start("ts-aarch64-apple-darwin", AssetRole::Executable)
        .expect("should start retention");
    download
        .push(&vec![17; 128 * 1024 + 1])
        .expect("should retain multiple chunks");
    let mut asset = download.finish().expect("should finish retention");
    let path = asset
        .materialize(
            directory.path(),
            AttemptDeadline::start(),
            &Cancellation::default(),
        )
        .expect("should materialize retained executable");
    assert_eq!(
        std::fs::metadata(&path).expect("should stat copy").len(),
        asset.measurement().length(),
        "should preserve measured length"
    );
    assert_eq!(
        crate::release::artifact::AssetMeasurement::of_bytes(
            &std::fs::read(path).expect("should read copy")
        ),
        *asset.measurement(),
        "should preserve retained digest"
    );
    assert!(
        asset
            .materialize(
                directory.path(),
                AttemptDeadline::start(),
                &Cancellation::default()
            )
            .is_err(),
        "should never overwrite existing canonical path"
    );
}

/// Synthetic proofs are emitted by a real bounded OS child, not a crypto fixture.
pub(super) struct Case {
    pub(super) directory: TempDir,
    pub(super) gh: PathBuf,
    pub(super) assets: BTreeMap<String, Vec<u8>>,
    pub(super) signed: Signed,
    pub(super) requests: RefCell<Vec<String>>,
}

impl Case {
    pub(super) fn new() -> Self {
        let signed = fixture::consistent();
        let directory = tempfile::tempdir().expect("should create fake transport directory");
        let documents = &signed.documents;
        let mut assets = BTreeMap::from([
            ("release-index.json".to_owned(), signed.index_bytes.clone()),
            (
                "release-index.provenance.sigstore.json".to_owned(),
                signed.publication.index_bundle.clone(),
            ),
            (
                "tsjs-inventory.json".to_owned(),
                documents.inventory.clone(),
            ),
            (
                "SHA256SUMS".to_owned(),
                signed.publication.checksums.clone(),
            ),
            (ASSET.to_owned(), documents.executable.clone()),
        ]);
        for (suffix, bytes) in [
            (".provenance.sigstore.json", &documents.provenance_bundle),
            (".sbom.sigstore.json", &documents.sbom_bundle),
            (".spdx.json", &documents.spdx),
            (".build-evidence.json", &documents.build_evidence),
            (".build-recipe.json", &documents.build_recipe),
            (".rebuild-check.json", &documents.rebuild_check),
        ] {
            assets.insert(format!("{ASSET}{suffix}"), bytes.clone());
        }
        let gh = directory.path().join("gh");
        let case = Self {
            directory,
            gh,
            assets,
            signed,
            requests: RefCell::new(Vec::new()),
        };
        case.install_proofs();
        case
    }

    pub(super) fn install_proofs(&self) {
        for (name, proof) in [
            ("index-proof", json!([self.signed.proofs.index])),
            ("artifact-proof", json!([self.signed.proofs.provenance])),
            ("sbom-proof", json!([self.signed.proofs.spdx])),
            ("release-proof", self.signed.publication.statement.clone()),
        ] {
            fs::write(
                self.directory.path().join(name),
                serde_json::to_vec(&proof).expect("should encode synthetic proof"),
            )
            .expect("should write proof response");
        }
        let root = self.directory.path().display();
        let script = format!(
            "#!/bin/sh\nif [ \"$1\" = --version ]; then echo 'gh version 2.97.0 (2026-01-01)'; exit 0; fi\nif [ \"$1\" = release ]; then /bin/cat '{root}/release-proof'; exit 0; fi\ncase \"$*\" in *--custom-trusted-root*) [ -z \"$GH_TOKEN$GITHUB_TOKEN\" ] || exit 9;; esac\ncase \"$3\" in */release-index.json) /bin/cat '{root}/index-proof';; *) case \"$*\" in *spdx.dev*) /bin/cat '{root}/sbom-proof';; *) /bin/cat '{root}/artifact-proof';; esac;; esac\n"
        );
        fs::write(&self.gh, script).expect("should install fake gh");
        fs::set_permissions(&self.gh, fs::Permissions::from_mode(0o700))
            .expect("should mark fake gh executable");
    }

    pub(super) fn runner(&self) -> GhRunner {
        GhRunner::resolve(
            GhLocation::Explicit(&self.gh),
            &CallerEnvironment::from_pairs([("GH_TOKEN", "fictional-read-only-token")]),
            ProcessBounds::production(),
            AttemptDeadline::start(),
            &Cancellation::default(),
        )
        .expect("should resolve fake trusted gh")
    }

    pub(super) fn write_assets(&self) {
        for (name, bytes) in &self.assets {
            fs::write(self.directory.path().join(name), bytes)
                .expect("should write transferred evidence");
        }
    }
}

impl Transport for Case {
    async fn policy(
        &self,
        deadline: AttemptDeadline,
        cancellation: &Cancellation,
    ) -> Result<ApprovedSourcePolicy, Report<ReleaseError>> {
        crate::release::artifact::check_attempt(deadline, cancellation)?;
        self.requests.borrow_mut().push("policy".to_owned());
        Ok(ApprovedSourcePolicy::for_tests(
            ApprovedCommit::parse(COMMIT)?,
            EMBEDDED_POLICY.to_vec(),
        ))
    }

    async fn asset(
        &self,
        name: &str,
        role: AssetRole,
        deadline: AttemptDeadline,
        cancellation: &Cancellation,
    ) -> Result<RetainedAsset, Report<ReleaseError>> {
        crate::release::artifact::check_attempt(deadline, cancellation)?;
        self.requests.borrow_mut().push(name.to_owned());
        if std::time::Instant::now() >= deadline.instant() {
            return Err(report(
                ReleaseError::Timeout,
                "fake transport deadline elapsed",
            ));
        }
        let bytes = self
            .assets
            .get(name)
            .ok_or_else(|| report(ReleaseError::Asset, "fake transport asset absent"))?;
        let mut retained = RetainedDownload::start(name, role)?;
        for chunk in bytes.chunks(17) {
            retained.push(chunk)?;
        }
        retained.finish()
    }

    async fn publication(
        &self,
        tag_ref: bool,
        deadline: AttemptDeadline,
        cancellation: &Cancellation,
    ) -> Result<Value, Report<ReleaseError>> {
        crate::release::artifact::check_attempt(deadline, cancellation)?;
        self.requests.borrow_mut().push("publication".to_owned());
        Ok(if tag_ref {
            self.signed.publication.tag_ref.clone()
        } else {
            self.signed.publication.release.clone()
        })
    }
}

#[tokio::test]
async fn full_online_orchestration_joins_retained_bytes_and_typed_child_proofs() {
    let case = Case::new();
    let report = verify_with(
        &case,
        &case.runner(),
        &fixture::approved(COMMIT),
        ASSET,
        AttemptDeadline::start(),
        &Cancellation::default(),
    )
    .await
    .expect("should verify complete synthetic graph through bounded child");
    assert_eq!(report.asset, ASSET, "should report selected verified asset");
    assert_eq!(
        report.source_commit, COMMIT,
        "should retain independently approved commit"
    );
}

#[tokio::test]
async fn untrusted_index_proof_fails_before_any_index_selected_download() {
    let mut case = Case::new();
    case.signed.proofs.index["verificationResult"]["signature"]["certificate"]["sourceRepositoryDigest"] =
        json!("0".repeat(40));
    case.install_proofs();
    assert!(
        verify_with(
            &case,
            &case.runner(),
            &fixture::approved(COMMIT),
            ASSET,
            AttemptDeadline::start(),
            &Cancellation::default()
        )
        .await
        .is_err(),
        "should reject certificate mismatch"
    );
    assert_eq!(
        *case.requests.borrow(),
        [
            "policy",
            "release-index.json",
            "release-index.provenance.sigstore.json"
        ],
        "should verify index provenance before using its fields"
    );
}

#[tokio::test]
async fn malformed_truncated_and_failed_proof_children_never_produce_reports() {
    for output in [
        "printf '[{'",
        "printf '[{}]'; exit 7",
        "printf '[{\"a\":1,\"a\":2}]'",
    ] {
        let case = Case::new();
        fs::write(&case.gh, format!("#!/bin/sh\nif [ \"$1\" = --version ]; then echo 'gh version 2.97.0 (2026-01-01)'; exit 0; fi\n{output}\n")).expect("should install malformed proof child");
        assert!(
            verify_with(
                &case,
                &case.runner(),
                &fixture::approved(COMMIT),
                ASSET,
                AttemptDeadline::start(),
                &Cancellation::default()
            )
            .await
            .is_err(),
            "should reject incomplete, ambiguous or failed proof"
        );
    }
}

#[tokio::test]
async fn changed_companion_and_publication_fail_closed() {
    for mutation in ["companion", "publication"] {
        let mut case = Case::new();
        if mutation == "companion" {
            case.assets
                .insert(format!("{ASSET}.build-evidence.json"), b"{}".to_vec());
        } else {
            case.signed.publication.release["immutable"] = json!(false);
        }
        assert!(
            verify_with(
                &case,
                &case.runner(),
                &fixture::approved(COMMIT),
                ASSET,
                AttemptDeadline::start(),
                &Cancellation::default()
            )
            .await
            .is_err(),
            "should reject {mutation} mismatch"
        );
    }
}

#[tokio::test]
async fn elapsed_attempt_deadline_produces_no_verification_report() {
    let case = Case::new();
    assert!(
        verify_with(
            &case,
            &case.runner(),
            &fixture::approved(COMMIT),
            ASSET,
            AttemptDeadline::at(std::time::Instant::now()),
            &Cancellation::default()
        )
        .await
        .is_err(),
        "should reject elapsed attempt"
    );
}

#[tokio::test]
async fn bounded_child_timeout_fails_full_orchestration_without_publication() {
    let case = Case::new();
    fs::write(&case.gh, "#!/bin/sh\nif [ \"$1\" = --version ]; then echo 'gh version 2.97.0 (2026-01-01)'; exit 0; fi\nexec /bin/sleep 10\n").expect("should install stalled proof child");
    let mut bounds = ProcessBounds::production();
    bounds.child_timeout = std::time::Duration::from_millis(50);
    let runner = case.runner().with_bounds(bounds);
    assert!(
        verify_with(
            &case,
            &runner,
            &fixture::approved(COMMIT),
            ASSET,
            AttemptDeadline::start(),
            &Cancellation::default()
        )
        .await
        .is_err(),
        "should fail complete orchestration when proof child times out"
    );
    assert!(
        !case.requests.borrow().contains(&"publication".to_owned()),
        "should not acquire publication after failed build proof"
    );
}

#[tokio::test]
async fn stalled_http_headers_and_bodies_cancel_promptly() {
    for headers in [false, true] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("should bind stalled HTTP fixture");
        let address = listener
            .local_addr()
            .expect("should obtain fixture address");
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("should accept HTTP request");
            let mut request = [0_u8; 1024];
            tokio::io::AsyncReadExt::read(&mut socket, &mut request)
                .await
                .expect("should read request");
            if headers {
                tokio::io::AsyncWriteExt::write_all(
                    &mut socket,
                    b"HTTP/1.1 200 OK\r\nContent-Length: 4096\r\n\r\n",
                )
                .await
                .expect("should send stalled body headers");
            }
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
        });
        let cancellation = Cancellation::default();
        let requested = cancellation.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            requested.cancel();
        });
        let client = crate::release::source_policy::plain_http_policy_client_for_tests()
            .expect("should create no-redirect fixture client");
        let transfer = async {
            let response = client
                .get(format!("http://{address}/fixture"))
                .send()
                .await
                .map_err(|_| report(ReleaseError::Asset, "fixture request failed"))?;
            crate::release::artifact::stream_into(
                response,
                RetainedDownload::start("release-index.json", AssetRole::Metadata)?,
                AttemptDeadline::start(),
            )
            .await
        };
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            crate::release::online::cancellable(transfer, &cancellation),
        )
        .await
        .expect("should cancel stalled HTTP before normal attempt deadline");
        assert_eq!(
            result
                .expect_err("should reject cancelled transfer")
                .current_context(),
            &ReleaseError::Cancelled,
            "should cancel both header and chunk waits"
        );
        server.abort();
    }
}

#[test]
fn final_asset_and_report_hashes_honor_deadline_and_cancellation() {
    let signed = fixture::consistent();
    let publication = signed
        .publish()
        .expect("should verify synthetic publication");
    let retain = || {
        let mut download = RetainedDownload::start(ASSET, AssetRole::Executable)
            .expect("should start retained executable");
        download
            .push(&signed.documents.executable)
            .expect("should retain fixture bytes");
        download.finish().expect("should finish retained bytes")
    };
    let expired = AttemptDeadline::at(std::time::Instant::now());
    assert!(
        crate::release::artifact::VerifiedReleaseAsset::from_publication_bounded(
            &publication,
            retain(),
            expired,
            &Cancellation::default()
        )
        .is_err(),
        "should not construct final asset after deadline"
    );
    let mut asset = crate::release::artifact::VerifiedReleaseAsset::from_publication_bounded(
        &publication,
        retain(),
        AttemptDeadline::start(),
        &Cancellation::default(),
    )
    .expect("should construct bounded final asset");
    assert!(
        crate::release::artifact::VerificationReport::new_bounded(
            &publication,
            &mut asset,
            expired,
            &Cancellation::default()
        )
        .is_err(),
        "should produce no report after final-phase deadline"
    );
    let cancelled = Cancellation::default();
    cancelled.cancel();
    assert!(
        crate::release::artifact::VerificationReport::new_bounded(
            &publication,
            &mut asset,
            AttemptDeadline::start(),
            &cancelled
        )
        .is_err(),
        "should produce no report after final-phase cancellation"
    );
}
