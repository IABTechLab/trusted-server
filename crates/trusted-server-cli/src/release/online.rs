//! Fixed-origin online orchestration of the private consumer verifier.

use std::future::Future;

use error_stack::Report;
use reqwest::{Client, StatusCode};
use serde_json::Value;
use url::Url;

use super::artifact::{
    AssetRole, RetainedAsset, RetainedDownload, VerificationReport, VerifiedReleaseAsset,
    stream_into,
};
use super::bounds::{AttemptDeadline, Cancellation, ProcessBounds};
use super::documents::{ApprovedCommit, ApprovedRelease, ReleaseTag, safe_name};
use super::environment::{CallerEnvironment, GithubToken};
use super::error::{ReleaseError, report};
use super::evidence::{
    BuildEvidenceVerified, ConsumedArtifact, EvidenceInputs, INDEX_NAME, verify_build_evidence,
    verify_index,
};
use super::gh::{GhLocation, GhRunner, PredicateKind};
use super::github::{self, PublicationInputs};
use super::policy::embedded_policy;
use super::source_policy::{self, ApprovedSourcePolicy};

/// Validates all public inputs before any environment or network access.
pub(super) fn validate_request(
    tag: &str,
    asset: &str,
    commit: &str,
) -> Result<ApprovedRelease, Report<ReleaseError>> {
    let tag = ReleaseTag::parse(tag)?;
    let commit = ApprovedCommit::parse(commit)?;
    if embedded_policy()?.artifact(asset).is_none() {
        return Err(report(
            ReleaseError::Policy,
            "asset is outside the embedded release cohort",
        ));
    }
    Ok(ApprovedRelease::new(tag, commit))
}

/// Constructs only canonical public release URLs; no credential is attached.
pub(super) fn approved_asset_url(
    release: &ApprovedRelease,
    name: &str,
) -> Result<Url, Report<ReleaseError>> {
    if !safe_name(name) {
        return Err(report(ReleaseError::Asset, "release asset name is unsafe"));
    }
    Url::parse(&format!(
        "https://github.com/{}/releases/download/{}/{name}",
        super::policy::REPOSITORY,
        release.tag().as_str()
    ))
    .map_err(|_| report(ReleaseError::Asset, "cannot encode official asset URL"))
}

/// Exact CDN listed by GitHub's runner networking reference under release assets.
///
/// <https://docs.github.com/en/actions/reference/runners/github-hosted-runners>
pub(super) fn allowed_redirect(url: &Url) -> bool {
    url.scheme() == "https"
        && url.host_str() == Some("release-assets.githubusercontent.com")
        && url.port_or_known_default() == Some(443)
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none()
}

/// Internal transport seam; public dispatch always constructs [`OfficialTransport`].
pub(super) trait Transport {
    async fn policy(
        &self,
        deadline: AttemptDeadline,
        cancellation: &Cancellation,
    ) -> Result<ApprovedSourcePolicy, Report<ReleaseError>>;
    async fn asset(
        &self,
        name: &str,
        role: AssetRole,
        deadline: AttemptDeadline,
        cancellation: &Cancellation,
    ) -> Result<RetainedAsset, Report<ReleaseError>>;
    async fn publication(
        &self,
        tag_ref: bool,
        deadline: AttemptDeadline,
        cancellation: &Cancellation,
    ) -> Result<Value, Report<ReleaseError>>;
}

/// Interrupts a pending HTTP send or body read when its attempt is cancelled.
pub(super) async fn cancellable<T>(
    future: impl Future<Output = Result<T, Report<ReleaseError>>>,
    cancellation: &Cancellation,
) -> Result<T, Report<ReleaseError>> {
    tokio::select! {
        biased;
        () = cancellation.cancelled() => Err(report(ReleaseError::Cancelled, "release HTTP operation cancelled")),
        result = future => result,
    }
}

struct OfficialTransport<'a> {
    client: Client,
    release: &'a ApprovedRelease,
    token: GithubToken,
}

impl Transport for OfficialTransport<'_> {
    async fn policy(
        &self,
        deadline: AttemptDeadline,
        cancellation: &Cancellation,
    ) -> Result<ApprovedSourcePolicy, Report<ReleaseError>> {
        super::artifact::check_attempt(deadline, cancellation)?;
        cancellable(
            source_policy::fetch_approved_policy(
                &self.client,
                self.release.commit(),
                &self.token,
                deadline,
            ),
            cancellation,
        )
        .await
    }

    async fn asset(
        &self,
        name: &str,
        role: AssetRole,
        deadline: AttemptDeadline,
        cancellation: &Cancellation,
    ) -> Result<RetainedAsset, Report<ReleaseError>> {
        super::artifact::check_attempt(deadline, cancellation)?;
        let transfer = async {
            let mut url = approved_asset_url(self.release, name)?;
            for hop in 0..=3 {
                // No bearer, cookies, referrer or caller-controlled headers on public downloads.
                let response = self
                    .client
                    .get(url.clone())
                    .header(reqwest::header::USER_AGENT, "trusted-server-cli")
                    .send()
                    .await
                    .map_err(|_| report(ReleaseError::Asset, "official asset request failed"))?;
                if response.url() != &url {
                    return Err(report(
                        ReleaseError::Asset,
                        "asset client followed an unexpected redirect",
                    ));
                }
                if response.status().is_redirection() && hop < 3 {
                    let location = response
                        .headers()
                        .get(reqwest::header::LOCATION)
                        .and_then(|value| value.to_str().ok())
                        .ok_or_else(|| {
                            report(ReleaseError::Asset, "asset redirect has no valid location")
                        })?;
                    let next = url
                        .join(location)
                        .map_err(|_| report(ReleaseError::Asset, "invalid asset redirect"))?;
                    if !allowed_redirect(&next) {
                        return Err(report(
                            ReleaseError::Asset,
                            "asset redirect is outside the approved HTTPS host",
                        ));
                    }
                    url = next;
                    continue;
                }
                if response.status() != StatusCode::OK {
                    return Err(report(
                        ReleaseError::Asset,
                        "official asset request did not return 200",
                    ));
                }
                return stream_into(response, RetainedDownload::start(name, role)?, deadline).await;
            }
            Err(report(ReleaseError::Asset, "asset redirect limit exceeded"))
        };
        cancellable(
            async {
                tokio::time::timeout_at(
                    tokio::time::Instant::from_std(deadline.instant()),
                    transfer,
                )
                .await
                .map_err(|_| {
                    report(
                        ReleaseError::Timeout,
                        "asset request exceeded attempt deadline",
                    )
                })?
            },
            cancellation,
        )
        .await
    }

    async fn publication(
        &self,
        tag_ref: bool,
        deadline: AttemptDeadline,
        cancellation: &Cancellation,
    ) -> Result<Value, Report<ReleaseError>> {
        super::artifact::check_attempt(deadline, cancellation)?;
        let request = if tag_ref {
            github::tag_ref_request(self.release.tag(), &self.token)?
        } else {
            github::release_request(self.release.tag(), &self.token)?
        };
        cancellable(
            github::fetch_json(&self.client, request, &self.token, deadline),
            cancellation,
        )
        .await
    }
}

/// Runs actual downloads, proofs and joins; returns only a read-only report.
pub(super) async fn build_with(
    transport: &impl Transport,
    runner: &GhRunner,
    request: &ApprovedRelease,
    asset_name: &str,
    deadline: AttemptDeadline,
    cancellation: &Cancellation,
) -> Result<BuildResult, Report<ReleaseError>> {
    let policy = embedded_policy()?;
    let source_policy = transport.policy(deadline, cancellation).await?;
    let mut index = transport
        .asset(INDEX_NAME, AssetRole::Metadata, deadline, cancellation)
        .await?;
    let mut index_bundle = transport
        .asset(
            github::INDEX_BUNDLE_NAME,
            AssetRole::Companion,
            deadline,
            cancellation,
        )
        .await?;
    let private = tempfile::Builder::new()
        .prefix("ts-release-inputs-")
        .tempdir()
        .map_err(|error| report(ReleaseError::Asset, error))?;
    let index_path = index.materialize(private.path(), deadline, cancellation)?;
    let index_bundle_path = index_bundle.materialize(private.path(), deadline, cancellation)?;
    let index_proof = runner.verify_attestation(
        &index_path,
        &index_bundle_path,
        request,
        PredicateKind::Provenance,
        deadline,
        cancellation,
    )?;
    let index_bytes = index.document_bytes()?;
    let parsed = verify_index(
        request,
        &policy,
        &source_policy,
        &index_bytes,
        &index_proof,
        index_bundle.measurement(),
    )?;
    let selected = parsed
        .artifacts
        .iter()
        .find(|artifact| artifact.name == asset_name)
        .ok_or_else(|| {
            report(
                ReleaseError::Policy,
                "selected asset is absent from signed index",
            )
        })?;
    let mut executable = transport
        .asset(asset_name, AssetRole::Executable, deadline, cancellation)
        .await?;
    let companions = &selected.companions;
    let mut provenance = transport
        .asset(
            &companions.provenance.name,
            AssetRole::Companion,
            deadline,
            cancellation,
        )
        .await?;
    let mut sbom_bundle = transport
        .asset(
            &companions.sbom_attestation.name,
            AssetRole::Companion,
            deadline,
            cancellation,
        )
        .await?;
    let mut spdx = transport
        .asset(
            &companions.sbom.name,
            AssetRole::Spdx,
            deadline,
            cancellation,
        )
        .await?;
    let mut build_evidence = transport
        .asset(
            &companions.build_evidence.name,
            AssetRole::Companion,
            deadline,
            cancellation,
        )
        .await?;
    let mut build_recipe = transport
        .asset(
            &companions.build_recipe.name,
            AssetRole::Companion,
            deadline,
            cancellation,
        )
        .await?;
    let mut rebuild_check = transport
        .asset(
            &companions.rebuild_check.name,
            AssetRole::Companion,
            deadline,
            cancellation,
        )
        .await?;
    let mut inventory = transport
        .asset(
            &parsed.tsjs_inventory.name,
            AssetRole::Metadata,
            deadline,
            cancellation,
        )
        .await?;
    for (record, retained) in [
        (&companions.provenance, &provenance),
        (&companions.sbom_attestation, &sbom_bundle),
        (&companions.sbom, &spdx),
        (&companions.build_evidence, &build_evidence),
        (&companions.build_recipe, &build_recipe),
        (&companions.rebuild_check, &rebuild_check),
        (&parsed.tsjs_inventory, &inventory),
    ] {
        if retained.name() != record.name
            || retained.measurement().sha256() != record.sha256
            || retained.measurement().length() != record.size
        {
            return Err(report(
                ReleaseError::Evidence,
                "download does not match signed companion membership",
            ));
        }
    }
    if executable.name() != selected.name
        || executable.measurement().sha256() != selected.sha256
        || executable.measurement().length() != selected.size
    {
        return Err(report(
            ReleaseError::Evidence,
            "download does not match signed executable membership",
        ));
    }
    let executable_path = executable.materialize(private.path(), deadline, cancellation)?;
    let provenance_path = provenance.materialize(private.path(), deadline, cancellation)?;
    let sbom_path = sbom_bundle.materialize(private.path(), deadline, cancellation)?;
    let provenance_proof = runner.verify_attestation(
        &executable_path,
        &provenance_path,
        request,
        PredicateKind::Provenance,
        deadline,
        cancellation,
    )?;
    let spdx_proof = runner.verify_attestation(
        &executable_path,
        &sbom_path,
        request,
        PredicateKind::Spdx,
        deadline,
        cancellation,
    )?;
    let spdx_bytes = spdx.document_bytes()?;
    let build_evidence_bytes = build_evidence.document_bytes()?;
    let build_recipe_bytes = build_recipe.document_bytes()?;
    let rebuild_check_bytes = rebuild_check.document_bytes()?;
    let inventory_bytes = inventory.document_bytes()?;
    let consumed = [ConsumedArtifact {
        name: asset_name,
        executable: executable.measurement(),
        spdx: &spdx_bytes,
        build_evidence: &build_evidence_bytes,
        build_recipe: &build_recipe_bytes,
        rebuild_check: &rebuild_check_bytes,
        provenance_bundle: provenance.measurement(),
        sbom_bundle: sbom_bundle.measurement(),
        provenance_proof: &provenance_proof,
        spdx_proof: &spdx_proof,
    }];
    let build = verify_build_evidence(&EvidenceInputs {
        release: request,
        consumer_policy: &policy,
        source_policy: &source_policy,
        index_bytes: &index_bytes,
        index_proof: &index_proof,
        index_bundle: index_bundle.measurement(),
        inventory_bytes: &inventory_bytes,
        artifacts: &consumed,
    })?;
    Ok(BuildResult {
        build,
        executable,
        index_bundle: index_bundle.measurement().clone(),
    })
}

/// Verified build and retained executable, before online publication checks.
pub(super) struct BuildResult {
    pub(super) build: BuildEvidenceVerified,
    pub(super) executable: RetainedAsset,
    pub(super) index_bundle: super::artifact::AssetMeasurement,
}

/// Completes online publication checks and emits a read-only report.
pub(super) async fn verify_with(
    transport: &impl Transport,
    runner: &GhRunner,
    request: &ApprovedRelease,
    asset_name: &str,
    deadline: AttemptDeadline,
    cancellation: &Cancellation,
) -> Result<VerificationReport, Report<ReleaseError>> {
    let built = build_with(
        transport,
        runner,
        request,
        asset_name,
        deadline,
        cancellation,
    )
    .await?;
    let checksums = transport
        .asset(
            github::CHECKSUMS_NAME,
            AssetRole::Metadata,
            deadline,
            cancellation,
        )
        .await?;
    let release = transport.publication(false, deadline, cancellation).await?;
    let tag_ref = transport.publication(true, deadline, cancellation).await?;
    let statement = runner.verify_release(request, deadline, cancellation)?;
    let publication = github::verify_publication(
        built.build,
        &PublicationInputs {
            release: &release,
            tag_ref: &tag_ref,
            release_statement: &statement,
            index_bundle: &built.index_bundle,
            checksums: checksums.measurement(),
        },
    )?;
    let mut asset = VerifiedReleaseAsset::from_publication_bounded(
        &publication,
        built.executable,
        deadline,
        cancellation,
    )?;
    VerificationReport::new_bounded(&publication, &mut asset, deadline, cancellation)
}

/// Entry point used only by the public command adapter.
pub(crate) fn verify(tag: &str, asset: &str, commit: &str) -> Result<(), String> {
    let request = validate_request(tag, asset, commit).map_err(|error| format!("{error:?}"))?;
    let caller = CallerEnvironment::from_process();
    let token = GithubToken::select(&caller).map_err(|error| format!("{error:?}"))?;
    let deadline = AttemptDeadline::start();
    let cancellation = Cancellation::default();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .map_err(|_| "cannot create release verification runtime".to_owned())?;
    // Register the listener synchronously before any gh child can start.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    let signal_task = {
        let mut interrupt = {
            let _entered = runtime.enter();
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
                .map_err(|_| "cannot register release cancellation signal".to_owned())?
        };
        let requested = cancellation.clone();
        runtime.spawn(async move {
            if interrupt.recv().await.is_some() {
                requested.cancel();
            }
        })
    };
    let result = runtime
        .block_on(async {
            let runner = GhRunner::resolve(
                GhLocation::SearchPath,
                &caller,
                ProcessBounds::production(),
                deadline,
                &cancellation,
            )?;
            let client = source_policy::policy_client()?;
            let transport = OfficialTransport {
                client,
                release: &request,
                token,
            };
            verify_with(
                &transport,
                &runner,
                &request,
                asset,
                deadline,
                &cancellation,
            )
            .await
        })
        .map_err(|error| format!("{error:?}"))?;
    super::artifact::check_attempt(deadline, &cancellation)
        .map_err(|error| format!("{error:?}"))?;
    let written = crate::output::write_stdout_line(format_report(&result))
        .map_err(|error| format!("{error:?}"));
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    signal_task.abort();
    written
}

fn format_report(report: &VerificationReport) -> String {
    format!(
        "Verified {}/{}\nSource: {}\nAsset: {}\nSHA-256: {}\nBytes: {}\nConsumer policy: {} {}\nProducer policy: {} {}\nAssurance: {}\nBuild evidence: verified\nOfficial immutable publication: verified\nRebuild comparison: {}",
        report.repository,
        report.tag,
        report.source_commit,
        report.asset,
        report.asset_sha256,
        report.asset_length,
        report.consumer_policy_version,
        report.consumer_policy_sha256,
        report.producer_policy_version,
        report.producer_policy_sha256,
        report.assurance_profile,
        report.comparison
    )
}
