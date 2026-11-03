//! One claimed job, end to end: a snapshot through the reader pipeline,
//! the same item extraction the reader runs, the per-item stability rule
//! of the job's origin, local prepared hashing, signing outside delivery,
//! and publication the delivery instance verifies through the API.
//!
//! A published item whose delivery record a reader still misses is
//! republished from its durable artifact, never signed again. A verified
//! plan is recorded before its finalize, so a retry after a crash
//! finalizes or recovers the same operation rather than planning a new
//! one.

use std::io::Write as _;
use std::os::unix::fs::FileExt as _;
use std::path::PathBuf;
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use sha2::{Digest as _, Sha256};

use trusted_server_provenance_encypher::preparation::items::{
    DocumentKind, ItemContext, document_items,
};
use trusted_server_provenance_encypher::preparation::pipeline::{
    self, Failure, FailureClass, MediaJob, PlannedMedia, SourceReader,
};
use trusted_server_provenance_encypher::preparation::protocol::{
    Carrier, EnvelopeItem, ItemKind, LargeLocator, MediaIdentity, PREPARATION_PROTOCOL,
    PUBLIC_SOURCE_PREFIX, ResultEnvelope, SnapshotMode, Target, credential_context, is_digest,
    operation_key,
};

use crate::client::{Api, DiskProbeCache, Edge, Snapshot, WorkerConfig};
use crate::store::{Artifact, Claim, JobOrigin, OutcomeClass, Store, outcome_class};

#[cfg(test)]
#[path = "media_tests.rs"]
mod media_tests;

/// Everything a preparation task uses.
pub struct Context {
    /// The durable store.
    pub store: Arc<Store>,
    /// The API.
    pub api: Api,
    /// The delivery instance.
    pub edge: Edge,
    /// The configuration.
    pub config: Arc<WorkerConfig>,
    /// The credential context identities bind.
    pub credential_context: String,
}

impl Context {
    /// The context for `config` with `api` and `edge`.
    #[must_use]
    pub fn new(
        store: Arc<Store>,
        api: Api,
        edge: Edge,
        config: Arc<WorkerConfig>,
        worker_token: &str,
    ) -> Self {
        let credential_context =
            credential_context(&config.api_base_url, &config.domain, worker_token);
        Self {
            store,
            api,
            edge,
            config,
            credential_context,
        }
    }

    fn probes(&self) -> DiskProbeCache {
        DiskProbeCache::new(Arc::clone(&self.store))
    }

    async fn load_probe(
        &self,
        capabilities: &pipeline::Capabilities,
    ) -> Result<pipeline::LoadedProbe, Failure> {
        let probe = pipeline::load_probe(&self.api, &self.probes(), capabilities).await?;
        let release = probe.release();
        self.store
            .refresh_context("probe", &format!("{}:{}", release.sha256, release.release))
            .map_err(|error| store_failure(&error))?;
        Ok(probe)
    }

    async fn load_release(
        &self,
        release: &pipeline::ProbeRelease,
    ) -> Result<pipeline::LoadedProbe, Failure> {
        pipeline::load_release(&self.api, &self.probes(), release).await
    }
}

pub(crate) async fn probe_ready(context: &Context, page: &str) -> Result<(), Failure> {
    let _budget = TemporaryBudget::new(context, "temp:readiness".to_owned(), 8 * 1024 * 1024)?;
    let releases = context
        .store
        .probe_releases()
        .map_err(|error| store_failure(&error))?;
    for release in &releases {
        context.load_release(release).await?;
    }
    if !releases.is_empty()
        || context
            .store
            .has_text_operations()
            .map_err(|error| store_failure(&error))?
    {
        // Completed or interrupted work remains recoverable when creation is closed.
        return Ok(());
    }
    let capabilities = pipeline::capabilities(&context.api, page).await?;
    context.load_probe(&capabilities).await?;
    Ok(())
}

pub(crate) async fn refresh_probe_context(context: &Context, page: &str) -> Result<(), Failure> {
    let _budget = TemporaryBudget::new(context, "temp:probe-observation".to_owned(), 64 * 1024)?;
    let capabilities = pipeline::capabilities(&context.api, page).await?;
    let (sha256, release) = pipeline::advertised_release_identity(&capabilities)?;
    context
        .store
        .refresh_context("probe", &format!("{sha256}:{release}"))
        .map_err(|error| store_failure(&error))
}

/// Unix seconds now.
#[must_use]
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// One item of the current snapshot.
enum Work {
    Text(pipeline::TextSignRequest),
    Embedded {
        mime: String,
        source: Vec<u8>,
        sha256: String,
    },
    Media {
        mime: String,
        sha256: String,
    },
    Large {
        mime: String,
        etag: String,
        length: u64,
        origin_identity_sha256: String,
    },
}

/// The configured source and validators every acquisition read must retain.
#[derive(Clone, Copy)]
struct LargeVersion<'a> {
    mime: &'a str,
    etag: &'a str,
    length: u64,
    origin_identity_sha256: &'a str,
}

struct Item {
    identity: String,
    kind: ItemKind,
    work: Work,
}

struct SnapshotItems {
    items: Vec<Item>,
    refusals: Vec<&'static str>,
}

/// Runs one claimed job. An `Err` is transient and retried; every other
/// outcome is recorded per item.
///
/// # Errors
///
/// A transient [`Failure`].
pub async fn run(context: &Context, claim: &Claim) -> Result<(), Failure> {
    tokio::time::timeout(
        std::time::Duration::from_secs(context.config.max_preparation_seconds),
        run_claim(context, claim),
    )
    .await
    .map_err(|_| Failure::new(FailureClass::Transient, "preparation_deadline"))?
}

async fn run_claim(context: &Context, claim: &Claim) -> Result<(), Failure> {
    check_claim(context, claim)?;
    // Unknown bounded source sizes reserve their configured ceiling before acquisition.
    let _snapshot_budget = TemporaryBudget::new(
        context,
        format!("temp:snapshot:{}", claim.token),
        (context.config.max_snapshot_bytes as u64).saturating_mul(4),
    )?;
    let job = &claim.job;
    let target = &job.target;
    let snapshot = match context
        .edge
        .snapshot_bounded(
            target,
            SnapshotMode::Whole,
            context.config.max_snapshot_bytes,
        )
        .await
    {
        Ok(snapshot) => snapshot,
        Err(failure)
            if failure.code == "snapshot_too_large" && context.config.max_large_media_bytes > 0 =>
        {
            context.edge.snapshot(target, SnapshotMode::Head).await?
        }
        Err(failure) => return settle_failure(context, claim, &failure),
    };
    if let Err(failure) = admit_snapshot(context, target, &snapshot) {
        return settle_failure(context, claim, &failure);
    }
    let page_url = snapshot.meta.page_url.clone();
    let policy = pipeline::policy_digest(&context.api, &page_url).await?;
    let SnapshotItems { items, refusals } = match current_items(context, target, &snapshot, &policy)
    {
        Ok(items) => items,
        Err(failure) => return settle_failure(context, claim, &failure),
    };
    let identities: Vec<String> = items.iter().map(|item| item.identity.clone()).collect();
    let previous = context
        .store
        .observe_claim_snapshot(claim, &identities, now())
        .map_err(|error| store_failure(&error))?
        .unwrap_or_default();
    let mut transient =
        record_extraction_refusals(context, claim, &snapshot, &policy, refusals).err();
    let selected: Vec<&Item> = match job.origin {
        JobOrigin::Signal => {
            for observed in &job.observed {
                if !identities.contains(&observed.identity) {
                    // The snapshot is not what the reader saw: damped per
                    // item, never signed as if it were.
                    if let Err(failure) = store_outcome(
                        context,
                        claim,
                        &observed.identity,
                        "source_unstable",
                        OutcomeClass::SourceUnstable,
                    ) {
                        transient.get_or_insert(failure);
                    }
                }
            }
            items
                .iter()
                .filter(|item| {
                    job.observed
                        .iter()
                        .any(|observed| observed.identity == item.identity)
                })
                .collect()
        }
        JobOrigin::Reconcile => items
            .iter()
            .filter(|item| previous.contains(&item.identity))
            .collect(),
        JobOrigin::Warm => items.iter().collect(),
    };
    for item in selected {
        if context
            .store
            .outcome(target, &item.identity)
            .map_err(|error| store_failure(&error))?
            .is_some_and(|outcome| now() < outcome.until)
        {
            continue;
        }
        match prepare_item(context, target, &snapshot, &policy, item, claim).await {
            Ok(()) => {}
            Err(failure)
                if matches!(
                    failure.class,
                    FailureClass::Transient | FailureClass::PolicyChanged | FailureClass::Missing
                ) =>
            {
                transient.get_or_insert(failure);
            }
            Err(failure) => {
                log::info!(
                    "preparation: item refused kind={:?} code={}",
                    item.kind,
                    failure.code
                );
                if let Some(class) = outcome_class(failure.class)
                    && let Err(failure) =
                        store_outcome(context, claim, &item.identity, &failure.code, class)
                {
                    transient.get_or_insert(failure);
                }
            }
        }
    }
    transient.map_or(Ok(()), Err)
}

fn store_failure(error: &crate::store::StoreError) -> Failure {
    match error {
        crate::store::StoreError::Quota => Failure::new(FailureClass::Transient, "quota_exhausted"),
        _ => Failure::new(FailureClass::Transient, "store_unavailable"),
    }
}

fn store_outcome(
    context: &Context,
    claim: &Claim,
    identity: &str,
    code: &str,
    class: OutcomeClass,
) -> Result<(), Failure> {
    context
        .store
        .record_claim_outcome(claim, identity, code, class, now())
        .map_err(|error| store_failure(&error))
}

/// Retain extraction refusals without damping the whole target or its valid
/// siblings. One outcome per reason and admitted page scope is a diagnostic,
/// not a count of assets or unrelated document revisions.
fn record_extraction_refusals(
    context: &Context,
    claim: &Claim,
    snapshot: &Snapshot,
    policy: &str,
    mut refusals: Vec<&'static str>,
) -> Result<(), Failure> {
    // These nested bytes are already covered by the containing SVG item.
    refusals.retain(|reason| *reason != "inside_inline_svg");
    if refusals.is_empty() {
        return Ok(());
    }
    refusals.sort_unstable();
    refusals.dedup();
    let mut scope = Sha256::new();
    scope.update(b"encypher.document-extraction-refusal.v2\0");
    for part in [
        context.credential_context.as_bytes(),
        policy.as_bytes(),
        snapshot.meta.page_url.as_bytes(),
        claim.job.target.variant.as_bytes(),
    ] {
        scope.update((part.len() as u64).to_be_bytes());
        scope.update(part);
    }
    for reason in refusals {
        let mut digest = scope.clone();
        digest.update(reason.as_bytes());
        let identity = crate::hex_lower(&digest.finalize());
        store_outcome(context, claim, &identity, reason, OutcomeClass::Terminal)?;
    }
    Ok(())
}

fn settle_failure(context: &Context, claim: &Claim, failure: &Failure) -> Result<(), Failure> {
    let Some(class) = outcome_class(failure.class) else {
        return Err(failure.clone());
    };
    if claim.job.observed.is_empty() {
        store_outcome(context, claim, "*", &failure.code, class)?;
    }
    for item in &claim.job.observed {
        store_outcome(context, claim, &item.identity, &failure.code, class)?;
    }
    Ok(())
}

/// The items of the snapshot, by the reader's own extraction.
fn current_items(
    context: &Context,
    target: &Target,
    snapshot: &Snapshot,
    policy: &str,
) -> Result<SnapshotItems, Failure> {
    let meta = &snapshot.meta;
    let media_type = meta
        .content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let item_context = ItemContext {
        credential_context: &context.credential_context,
        policy_digest: policy,
        page_url: &meta.page_url,
        variant: &target.variant,
        article_selector: &context.config.article_selector,
        min_text_length: context.config.min_text_length,
        max_media_bytes: context.config.max_media_bytes,
    };
    let document_kind = match media_type.as_str() {
        "text/html" => Some(DocumentKind::Page),
        "text/css" => Some(DocumentKind::Stylesheet),
        _ => None,
    };
    if let Some(kind) = document_kind {
        let document = std::str::from_utf8(&snapshot.bytes)
            .map_err(|_| Failure::new(FailureClass::Unsupported, "not_utf8"))?;
        let found = document_items(&item_context, document, kind, None);
        let mut items = Vec::new();
        if let Some(text) = found.text {
            items.push(Item {
                identity: text.identity,
                kind: ItemKind::Text,
                work: Work::Text(text.request),
            });
        }
        for embedded in found.embedded {
            items.push(Item {
                identity: embedded.identity,
                kind: ItemKind::Embedded,
                work: Work::Embedded {
                    mime: embedded.mime,
                    source: embedded.source,
                    sha256: embedded.sha256,
                },
            });
        }
        let mut refusals = found.refused;
        refusals.extend(found.text_refusal);
        return Ok(SnapshotItems { items, refusals });
    }
    if snapshot.bytes.is_empty() {
        if !is_digest(&meta.origin.origin_identity_sha256) {
            return Err(Failure::new(
                FailureClass::Integrity,
                "source_origin_identity_invalid",
            ));
        }
        let (Some(etag), Some(length)) = (meta.etag.clone(), meta.length) else {
            return Err(Failure::new(FailureClass::Unsupported, "no_strong_etag"));
        };
        if length > context.config.max_large_media_bytes {
            return Err(Failure::new(FailureClass::Unsupported, "media_too_large"));
        }
        let identity = LargeLocator {
            credential_context: &context.credential_context,
            policy_digest: policy,
            reader_url: &meta.page_url,
            variant: &target.variant,
            origin_host: &meta.origin.origin_host,
            origin_identity_sha256: &meta.origin.origin_identity_sha256,
            mime: &media_type,
            etag: &etag,
            length,
        }
        .digest();
        return Ok(SnapshotItems {
            items: vec![Item {
                identity,
                kind: ItemKind::Large,
                work: Work::Large {
                    mime: media_type,
                    etag,
                    length,
                    origin_identity_sha256: meta.origin.origin_identity_sha256.clone(),
                },
            }],
            refusals: Vec::new(),
        });
    }
    let sha256 = crate::hex_lower(&Sha256::digest(&snapshot.bytes));
    let identity = MediaIdentity {
        credential_context: &context.credential_context,
        policy_digest: policy,
        page_url: &meta.page_url,
        variant: &target.variant,
        carrier: Carrier::External,
        mime: &media_type,
        source_sha256: &sha256,
    }
    .digest();
    Ok(SnapshotItems {
        items: vec![Item {
            identity,
            kind: ItemKind::Media,
            work: Work::Media {
                mime: media_type,
                sha256,
            },
        }],
        refusals: Vec::new(),
    })
}

fn admit_snapshot(context: &Context, target: &Target, snapshot: &Snapshot) -> Result<(), Failure> {
    let meta = &snapshot.meta;
    if meta.protocol != PREPARATION_PROTOCOL
        || meta.page_url != format!("https://{}{}", context.config.public_host, target.path)
    {
        return Err(Failure::new(
            FailureClass::Integrity,
            "snapshot_identity_mismatch",
        ));
    }
    if meta.origin.private {
        return Err(Failure::new(FailureClass::Privacy, "source_private"));
    }
    if meta.origin.no_transform || meta.origin.manifest_link {
        return Err(Failure::new(
            FailureClass::ExistingProvenance,
            "source_protected",
        ));
    }
    if !matches!(meta.origin.status, 200 | 206)
        || (!meta.origin.content_encoding.is_empty()
            && !meta
                .origin
                .content_encoding
                .eq_ignore_ascii_case("identity"))
    {
        return Err(Failure::new(
            FailureClass::Unsupported,
            "source_not_admitted",
        ));
    }
    Ok(())
}

struct TemporaryBudget {
    store: Arc<Store>,
    owner: String,
}

impl TemporaryBudget {
    fn new(context: &Context, owner: String, bytes: u64) -> Result<Self, Failure> {
        reserve(context, &owner, bytes)?;
        Ok(Self {
            store: Arc::clone(&context.store),
            owner,
        })
    }
}

impl Drop for TemporaryBudget {
    fn drop(&mut self) {
        if self.store.release(&self.owner).is_err() {
            log::warn!("preparation: temporary reservation retained for restart cleanup");
        }
    }
}

struct ArtifactBudget<'a> {
    context: &'a Context,
    claim: &'a Claim,
    identity: &'a str,
}

impl Drop for ArtifactBudget<'_> {
    fn drop(&mut self) {
        // A newer claim may already be acquiring this identity. A stale
        // attempt never releases shared source/output ownership underneath it.
        if check_claim(self.context, self.claim).is_err() {
            return;
        }
        match self.context.store.has_reconstruction(self.identity) {
            Ok(false) => {
                if self
                    .context
                    .store
                    .release(&format!("artifact:{}", self.identity))
                    .is_err()
                    || self
                        .context
                        .store
                        .discard_unplanned_sources(self.identity)
                        .is_err()
                {
                    log::warn!(
                        "preparation: unplanned material remains charged for restart cleanup"
                    );
                }
            }
            Ok(true) => {}
            Err(_) => log::warn!("preparation: uncertain output reservation remains charged"),
        }
    }
}

fn check_claim(context: &Context, claim: &Claim) -> Result<(), Failure> {
    let current = context
        .store
        .job(&claim.job.key)
        .map_err(|error| store_failure(&error))?;
    if current.is_some_and(|job| matches!(job.state, crate::store::JobState::Processing { token, lease_until } if token == claim.token && lease_until > now())) {
        Ok(())
    } else {
        Err(Failure::new(FailureClass::Transient, "lease_lost"))
    }
}

/// Prepares and publishes one item, or republishes its artifact.
async fn prepare_item(
    context: &Context,
    target: &Target,
    snapshot: &Snapshot,
    policy: &str,
    item: &Item,
    claim: &Claim,
) -> Result<(), Failure> {
    check_claim(context, claim)?;
    if context
        .store
        .retirement(&item.identity)
        .map_err(|error| store_failure(&error))?
        .is_some()
    {
        return Ok(());
    }
    if let Some(artifact) = context
        .store
        .artifact(&item.identity)
        .map_err(|error| store_failure(&error))?
    {
        // Republished from the durable artifact: the delivery instance
        // recovers the same receipt, and nothing is signed again.
        context.edge.publish(&artifact.envelope).await?;
        context
            .store
            .acknowledge_artifact(&item.identity)
            .map_err(|error| store_failure(&error))?;
        return Ok(());
    }
    let epoch = context
        .store
        .retirement_epoch()
        .map_err(|error| store_failure(&error))?;
    let meta = &snapshot.meta;
    let page_url = meta.page_url.as_str();
    let source_size = match &item.work {
        Work::Text(request) => request.text.len() as u64,
        Work::Embedded { source, .. } => source.len() as u64,
        Work::Media { .. } => snapshot.bytes.len() as u64,
        Work::Large { .. } => 0,
    };
    // Source copies, base64 encodings, probe transfer, manifest, carrier and
    // output assembly are all bounded and charged before any API creation.
    let _assembly_budget = TemporaryBudget::new(
        context,
        format!("temp:assembly:{}", claim.token),
        source_size
            .saturating_mul(8)
            .saturating_add(16 * 1024 * 1024),
    )?;
    reserve(
        context,
        &format!("artifact:{}", item.identity),
        source_size
            .saturating_mul(4)
            .saturating_add(8 * 1024 * 1024),
    )?;
    let _artifact_budget = ArtifactBudget {
        context,
        claim,
        identity: &item.identity,
    };
    let envelope_item = match &item.work {
        Work::Text(request) => {
            let key = operation_key(&item.identity, 0);
            match pipeline::recover_text(&context.api, request, &key).await {
                Ok(_) => {
                    context
                        .store
                        .begin_text(&item.identity, request)
                        .map_err(|error| store_failure(&error))?;
                }
                Err(failure) if failure.class == FailureClass::Missing => {
                    if !context
                        .store
                        .begin_text(&item.identity, request)
                        .map_err(|error| store_failure(&error))?
                    {
                        return Err(Failure::new(
                            FailureClass::Integrity,
                            "text_result_unrecoverable",
                        ));
                    }
                    check_claim(context, claim)?;
                    pipeline::sign_text(&context.api, request, &key).await?;
                }
                Err(failure) => return Err(failure),
            }
            EnvelopeItem::Text {
                request: request.clone(),
                idempotency_key: key,
            }
        }
        Work::Embedded {
            mime,
            source,
            sha256,
        } => {
            keep_public_source(context, sha256, mime, source, &item.identity)?;
            let reference = format!(
                "https://{}{PUBLIC_SOURCE_PREFIX}{sha256}",
                context.config.public_host
            );
            let job = MediaJob {
                page_url,
                policy_digest: policy,
                carrier: Carrier::Embedded,
                mime,
                source_sha256: sha256,
                reference: &reference,
                origin_host: &meta.origin.origin_host,
                reference_kind: "publisher_snapshot",
                idempotency_key: &operation_key(&item.identity, 0),
            };
            let planned = planned(context, &item.identity, &job, source.as_slice()).await?;
            let output = recover_or_finalize(
                context,
                claim,
                &item.identity,
                &planned,
                Some(source.as_slice()),
            )
            .await?
            .ok_or_else(|| Failure::new(FailureClass::Integrity, "no_output"))?;
            EnvelopeItem::Embedded {
                planned,
                source_b64: STANDARD.encode(source),
                output_b64: STANDARD.encode(output),
            }
        }
        Work::Media { mime, sha256 } => {
            let source = snapshot.bytes.as_ref();
            let origin = &meta.origin;
            let origin_reference = origin.source_reference.as_deref().filter(|reference| {
                url::Url::parse(reference).is_ok_and(|url| {
                    url.scheme() == "https" && url.host_str() == Some(origin.origin_host.as_str())
                })
            });
            let snapshot_reference = format!(
                "https://{}{PUBLIC_SOURCE_PREFIX}{sha256}",
                context.config.public_host
            );
            let (reference, kind, origin_host) = match origin_reference {
                Some(reference) => (reference.to_owned(), "origin", origin.origin_host.clone()),
                None => {
                    keep_public_source(context, sha256, mime, source, &item.identity)?;
                    (
                        snapshot_reference,
                        "publisher_snapshot",
                        origin.origin_host.clone(),
                    )
                }
            };
            let job = MediaJob {
                page_url,
                policy_digest: policy,
                carrier: Carrier::External,
                mime,
                source_sha256: sha256,
                reference: &reference,
                origin_host: &origin_host,
                reference_kind: kind,
                idempotency_key: &operation_key(&item.identity, 0),
            };
            let planned = planned(context, &item.identity, &job, source).await?;
            recover_or_finalize(context, claim, &item.identity, &planned, None).await?;
            EnvelopeItem::Media {
                planned,
                source_b64: STANDARD.encode(source),
            }
        }
        Work::Large {
            mime,
            etag,
            length,
            origin_identity_sha256,
        } => {
            let version = LargeVersion {
                mime,
                etag,
                length: *length,
                origin_identity_sha256,
            };
            let file =
                download_large(context, target, &item.identity, claim.token, version).await?;
            let reference = meta.origin.source_reference.clone().ok_or_else(|| {
                Failure::new(FailureClass::Unsupported, "source_reference_required")
            })?;
            let job = MediaJob {
                page_url,
                policy_digest: policy,
                carrier: Carrier::External,
                mime,
                source_sha256: &file.sha256,
                reference: &reference,
                origin_host: &meta.origin.origin_host,
                reference_kind: "origin",
                idempotency_key: &operation_key(&item.identity, 0),
            };
            let result = async {
                let planned = planned(context, &item.identity, &job, &file).await?;
                confirm_large(context, target, version).await?;
                recover_or_finalize(context, claim, &item.identity, &planned, None).await?;
                confirm_large(context, target, version).await?;
                Ok::<_, Failure>(planned)
            }
            .await;
            EnvelopeItem::Large {
                planned: result?,
                etag: etag.clone(),
                origin_identity_sha256: origin_identity_sha256.clone(),
            }
        }
    };
    let envelope = ResultEnvelope {
        protocol: PREPARATION_PROTOCOL.to_owned(),
        deployment_id: context.config.deployment_id.clone(),
        identity: item.identity.clone(),
        retirement_epoch: epoch,
        variant: target.variant.clone(),
        item: envelope_item,
    };
    let artifact = Artifact {
        envelope,
        target: target.clone(),
        acknowledged: false,
    };
    // The result is durable before it is acknowledged, and never outlives a
    // retirement that happened while it was prepared.
    if !context
        .store
        .record_artifact(&artifact)
        .map_err(|error| store_failure(&error))?
    {
        context
            .store
            .release(&format!("artifact:{}", item.identity))
            .map_err(|error| store_failure(&error))?;
        return Ok(());
    }
    check_claim(context, claim)?;
    if context
        .store
        .retirement(&item.identity)
        .map_err(|error| store_failure(&error))?
        .is_some()
    {
        return Ok(());
    }
    context.edge.publish(&artifact.envelope).await?;
    context
        .store
        .acknowledge_artifact(&item.identity)
        .map_err(|error| store_failure(&error))
}

async fn recover_or_finalize(
    context: &Context,
    claim: &Claim,
    identity: &str,
    planned: &PlannedMedia,
    source: Option<&[u8]>,
) -> Result<Option<Vec<u8>>, Failure> {
    check_claim(context, claim)?;
    if context
        .store
        .retirement(identity)
        .map_err(|error| store_failure(&error))?
        .is_some()
    {
        return Err(Failure::new(FailureClass::Integrity, "retired"));
    }
    let probe = context.load_release(&planned.release).await?;
    match pipeline::recover_media(&context.api, &planned.request, &planned.idempotency_key).await {
        Ok(recovered) => pipeline::reconstruct_media(&probe, planned, &recovered, source).await,
        Err(failure) if failure.class == FailureClass::Missing => {
            check_claim(context, claim)?;
            if context
                .store
                .retirement(identity)
                .map_err(|error| store_failure(&error))?
                .is_some()
            {
                return Err(Failure::new(FailureClass::Integrity, "retired"));
            }
            pipeline::finalize_media(&context.api, &probe, planned, source).await
        }
        Err(failure) => Err(failure),
    }
}

/// The verified plan of an identity: the one recorded before an earlier
/// finalize, or a new one, recorded before it is finalized.
async fn planned<S: SourceReader + ?Sized>(
    context: &Context,
    identity: &str,
    job: &MediaJob<'_>,
    source: &S,
) -> Result<PlannedMedia, Failure> {
    if let Some(planned) = context
        .store
        .plan(identity)
        .map_err(|error| store_failure(&error))?
    {
        if planned.request.source.sha256 != job.source_sha256
            || planned.request.source.byte_length != source.size()
            || planned.request.source.mime != job.mime
            || planned.request.expected_policy_digest != job.policy_digest
        {
            return Err(Failure::new(
                FailureClass::Integrity,
                "stored_plan_source_mismatch",
            ));
        }
        return Ok(planned);
    }
    let capabilities = pipeline::capabilities(&context.api, job.page_url).await?;
    let probe = context.load_probe(&capabilities).await?;
    let planned = pipeline::plan_media(&context.api, &probe, &capabilities, job, source).await?;
    context
        .store
        .record_plan(identity, &planned)
        .map_err(|error| store_failure(&error))?;
    Ok(planned)
}

/// Keeps an admitted public source snapshot the publisher route serves by
/// digest, within the byte quota.
fn keep_public_source(
    context: &Context,
    sha256: &str,
    mime: &str,
    bytes: &[u8],
    identity: &str,
) -> Result<(), Failure> {
    context
        .store
        .keep_source(sha256, mime, bytes, identity)
        .map_err(|error| store_failure(&error))
}

/// Reserves `bytes` for `owner` against the one hard quota every task
/// shares, in a store transaction, before anything is written. A full
/// quota refuses the work and retries later; it never evicts a retained
/// result.
fn reserve(context: &Context, owner: &str, bytes: u64) -> Result<(), Failure> {
    if context
        .store
        .reserve(owner, bytes)
        .map_err(|error| store_failure(&error))?
    {
        Ok(())
    } else {
        Err(Failure::new(FailureClass::Transient, "quota_exhausted"))
    }
}

/// A large source streamed to a private file through pinned range reads.
struct LargeFile {
    file: std::fs::File,
    size: u64,
    sha256: String,
    _cleanup: TemporarySource,
}

struct TemporarySource {
    store: Arc<Store>,
    owner: String,
    path: PathBuf,
}

impl Drop for TemporarySource {
    fn drop(&mut self) {
        let removed = match std::fs::remove_file(&self.path) {
            Ok(()) => true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
            Err(_) => false,
        };
        if removed {
            let synced = self.path.parent().is_some_and(|parent| {
                std::fs::File::open(parent)
                    .and_then(|file| file.sync_all())
                    .is_ok()
            });
            if synced && self.store.release(&self.owner).is_ok() {
                return;
            }
        }
        log::warn!("preparation: temporary source remains charged for restart cleanup");
    }
}

impl SourceReader for LargeFile {
    fn size(&self) -> u64 {
        self.size
    }

    async fn read_exact_at(&self, offset: u64, destination: &mut [u8]) -> Result<(), Failure> {
        self.file
            .read_exact_at(destination, offset)
            .map_err(|_| Failure::new(FailureClass::Transient, "source_read"))
    }
}

async fn confirm_large(
    context: &Context,
    target: &Target,
    version: LargeVersion<'_>,
) -> Result<(), Failure> {
    let LargeVersion {
        mime,
        etag,
        length,
        origin_identity_sha256,
    } = version;
    let current = context.edge.snapshot(target, SnapshotMode::Head).await?;
    admit_snapshot(context, target, &current)?;
    if current.meta.etag.as_deref() != Some(etag)
        || current.meta.length != Some(length)
        || current.meta.origin.origin_identity_sha256 != origin_identity_sha256
        || current
            .meta
            .content_type
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            != mime
    {
        return Err(Failure::new(
            FailureClass::Integrity,
            "source_version_changed",
        ));
    }
    Ok(())
}

/// Streams every byte of the version `etag` names, in pinned ranges read
/// through the reader pipeline, into a private file, hashing as it goes.
async fn download_large(
    context: &Context,
    target: &Target,
    identity: &str,
    token: u64,
    version: LargeVersion<'_>,
) -> Result<LargeFile, Failure> {
    let LargeVersion {
        mime,
        etag,
        length,
        origin_identity_sha256,
    } = version;
    const CHUNK: u64 = 8 * 1024 * 1024;
    if length == 0
        || length > context.config.max_large_media_bytes
        || !is_digest(origin_identity_sha256)
        || !etag.starts_with('"')
        || !etag.ends_with('"')
        || etag.len() < 2
        || etag.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err(Failure::new(
            FailureClass::Unsupported,
            "no_strong_source_version",
        ));
    }
    let name = format!("{identity}-{token}");
    let owner = format!("temp:large:{name}");
    reserve(context, &owner, length)?;
    let dir = context.config.store_dir.join("large");
    std::fs::create_dir_all(&dir).map_err(|_| Failure::new(FailureClass::Transient, "storage"))?;
    let path = dir.join(name);
    let cleanup = TemporarySource {
        store: Arc::clone(&context.store),
        owner,
        path: path.clone(),
    };
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .read(true)
        .create_new(true)
        .open(&path)
        .map_err(|_| Failure::new(FailureClass::Transient, "storage"))?;
    let mut hasher = Sha256::new();
    let mut first = 0_u64;
    while first < length {
        let last = (first + CHUNK).min(length) - 1;
        let part = context
            .edge
            .snapshot(
                target,
                SnapshotMode::Range {
                    first,
                    last,
                    if_match: etag.to_owned(),
                },
            )
            .await;
        let part = part?;
        admit_snapshot(context, target, &part)?;
        if part.meta.etag.as_deref() != Some(etag)
            || part.meta.length != Some(length)
            || part.meta.origin.origin_identity_sha256 != origin_identity_sha256
            || part.bytes.len() as u64 != last - first + 1
            || part
                .meta
                .content_type
                .split(';')
                .next()
                .unwrap_or_default()
                .trim()
                != mime
        {
            return Err(Failure::new(
                FailureClass::Integrity,
                "source_version_changed",
            ));
        }
        hasher.update(&part.bytes);
        file.write_all(&part.bytes)
            .map_err(|_| Failure::new(FailureClass::Transient, "storage"))?;
        first = last + 1;
    }
    file.sync_all()
        .map_err(|_| Failure::new(FailureClass::Transient, "storage"))?;
    Ok(LargeFile {
        _cleanup: cleanup,
        file,
        size: length,
        sha256: crate::hex_lower(&hasher.finalize()),
    })
}
