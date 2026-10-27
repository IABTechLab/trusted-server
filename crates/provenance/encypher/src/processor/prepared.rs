//! Large media signed where it is delivered, through the prepared lane.
//!
//! A source too large to capture is never uploaded and never held whole.
//! Core pins one version of it by its strong `ETag` and hashes it in bounded
//! chunks. This lane then asks the Encypher prepared-signing endpoints for a
//! remote manifest over those exact bytes:
//!
//! 1. The policy, then the lane's capabilities for the asset.
//! 2. The signed probe release the service advertises, verified against the
//!    pinned release key before it runs.
//! 3. The probe's disclosure of the source's container structure, read from
//!    the pinned version. A source that already carries an embedded C2PA
//!    manifest stops here: a remote manifest cannot stand beside it, as an
//!    embedded manifest wins over a `Link`, so the source is delivered as it
//!    is, with its own credentials, reported as existing provenance kept and
//!    never as newly signed or verified.
//! 4. `prepare`, then the probe's check that the server's plan covers the
//!    whole source unchanged, then a last check that the version is still
//!    current, then `finalize`.
//! 5. The returned store is checked to bind the whole-source digest. The
//!    reader gets the origin's bytes with a `Link` to that store.
//!
//! Every failure is a refusal with a code. Core then delivers the origin's
//! bytes unsigned; nothing is published for a source the lane did not
//! verify.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use bytes::Bytes;
use edgezero_core::body::Body as EdgeBody;
use http::{HeaderValue, Method, Request, StatusCode, header};
use serde::Serialize;
use serde_json::{Value, json};
use std::time::Duration;
use trusted_server_core::media_signing::{
    LargeMediaJob, LargeSignedMedia, MediaSourceError, VersionedMediaSource,
};
use trusted_server_core::middleware::ResponseHeader;
use trusted_server_core::platform::RuntimeServices;

use super::{
    Deadline, Reason, SKIP_WRITE_RESERVE, Signer, WORKER_TOKEN_HEADER, is_excluded_prefix,
};
use crate::api::valid_policy_digest;
use crate::media_api::{
    self, IDEMPOTENCY_KEY_HEADER, canonical_media_type, safe_source_reference, sha256_hex,
    valid_origin_host,
};
use crate::prepared_probe::{
    ProbeCallError, ProbeDescriptor, ProbeLimits, ProbeModule, ProbeSource, ReleaseAuthority,
};

const CAPABILITIES_PATH: &str = "/api/v1/public/cdn/media/hash/capabilities";
const PREPARE_PATH: &str = "/api/v1/public/cdn/media/hash/prepare";
const FINALIZE_PATH: &str = "/api/v1/public/cdn/media/hash/finalize";
/// Signed probe releases, served without credentials and never changed.
const PROBE_PATH: &str = "/api/v1/";
const PROTOCOL: &str = "encypher-prepared-signing/2";
/// The cached artifact of a probe release, by its SHA-256.
const PROBE_KEY_PREFIX: &str = "encypher:probe:v1";
/// The largest probe artifact accepted.
const MAX_PROBE_BYTES: usize = 4 * 1024 * 1024;
/// A capabilities answer is a few descriptors and limits.
const MAX_CAPABILITIES_BYTES: usize = 64 * 1024;
/// A prepare answer is ids and the planner's geometry.
const MAX_PREPARE_BYTES: usize = 1024 * 1024;
/// A finalize answer carries the manifest store, base64 encoded, besides
/// ids and URLs.
const FINALIZE_OVERHEAD_BYTES: usize = 64 * 1024;
/// The largest finalized manifest store the lane takes, whatever the
/// service allows: the answer, its decoded store and the probe's copy of it
/// must fit an edge instance. An external manifest of an unsigned source,
/// with no ingredient chain or identity assertion, is far smaller.
const MAX_FINALIZED_MANIFEST_BYTES: usize = 2 * 1024 * 1024;
/// The refusal for a finalized manifest larger than the lane takes.
const MANIFEST_TOO_LARGE: &str = "manifest_too_large";
/// The outcome of a source that already carries an embedded C2PA manifest:
/// delivered as it is, its own provenance kept, nothing newly signed.
const EMBEDDED_PROVENANCE: &str = "source_has_embedded_provenance";
/// The outcome of a source whose XMP already names a C2PA manifest: delivered
/// as it is, its own provenance kept, nothing newly signed.
const REFERENCED_PROVENANCE: &str = "source_has_provenance_reference";

const INSTANCE_ID_HEADER: &str = "x-encypher-manifest-instance-id";
const DOCUMENT_ID_HEADER: &str = "x-encypher-document-id";
const VERIFICATION_URL_HEADER: &str = "x-encypher-verification-url";

/// Why the prepared lane did not sign a large source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LargeRefusal {
    /// A refusal the module shares with its other lanes.
    Module(Reason),
    /// The pinned source could not be read as the version it was pinned at.
    Source(MediaSourceError),
    /// A refusal particular to this lane.
    Lane(&'static str),
}

impl LargeRefusal {
    pub(super) fn code(self) -> &'static str {
        match self {
            Self::Module(reason) => reason.code(),
            Self::Source(error) => error.code(),
            Self::Lane(code) => code,
        }
    }

    /// Whether the source was delivered with its own embedded credentials,
    /// its provenance kept rather than lost to a failure.
    pub(super) fn retains_provenance(self) -> bool {
        self == Self::Lane(EMBEDDED_PROVENANCE) || self == Self::Lane(REFERENCED_PROVENANCE)
    }
}

impl From<Reason> for LargeRefusal {
    fn from(reason: Reason) -> Self {
        Self::Module(reason)
    }
}

impl From<MediaSourceError> for LargeRefusal {
    fn from(error: MediaSourceError) -> Self {
        Self::Source(error)
    }
}

/// The pinned version, as the probe reads it.
struct PinnedSource<'a>(&'a VersionedMediaSource<'a>);

impl ProbeSource for PinnedSource<'_> {
    type Error = MediaSourceError;

    fn size(&self) -> u64 {
        self.0.size()
    }

    async fn read_exact_at(
        &self,
        offset: u64,
        destination: &mut [u8],
    ) -> Result<(), MediaSourceError> {
        self.0.read_exact_at(offset, destination).await
    }
}

/// The lane's capabilities for one asset, as the service answers them.
struct Capabilities {
    policy_digest: String,
    formats: Vec<String>,
    total_size_bytes: u64,
    manifest_decoded_bytes: usize,
    probes: Vec<ProbeDescriptor>,
}

impl Capabilities {
    /// The largest finalized store the lane takes: the service's maximum,
    /// never more than the edge's own.
    fn manifest_limit(&self) -> usize {
        self.manifest_decoded_bytes
            .min(MAX_FINALIZED_MANIFEST_BYTES)
    }

    fn parse(body: &[u8]) -> Option<Self> {
        let value: Value = serde_json::from_slice(body).ok()?;
        if value.get("protocol")?.as_str()? != PROTOCOL || !value.get("enabled")?.as_bool()? {
            return None;
        }
        let maximums = value.get("maximums")?;
        Some(Self {
            policy_digest: value.get("policy_digest")?.as_str()?.to_owned(),
            formats: value
                .get("formats")?
                .as_array()?
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect(),
            total_size_bytes: maximums.get("total_size_bytes")?.as_u64()?,
            manifest_decoded_bytes: usize::try_from(
                maximums.get("manifest_decoded_bytes")?.as_u64()?,
            )
            .ok()?,
            probes: ProbeDescriptor::parse_list(value.get("probe")),
        })
    }
}

/// What the lane signs one source with, once the gates passed.
#[derive(Clone, Copy)]
struct Lane<'a> {
    job: &'a LargeMediaJob<'a>,
    reference: &'a str,
    mime: &'a str,
    policy_digest: &'a str,
    /// The release key probes are held to: production, except in tests.
    authority: &'a ReleaseAuthority,
}

/// The prepare request, serialized from borrowed values so the disclosure
/// is not copied.
#[derive(Serialize)]
struct PrepareBody<'a> {
    page_url: &'a str,
    expected_policy_digest: &'a str,
    protocol: &'a str,
    probe: ProbeSelection<'a>,
    source: PreparedSource<'a>,
    structure: Vec<StructureChunk<'a>>,
}

#[derive(Serialize)]
struct ProbeSelection<'a> {
    sha256: &'a str,
    planner_id: &'a str,
}

#[derive(Serialize)]
struct PreparedSource<'a> {
    reference: &'a str,
    origin_host: &'a str,
    mime: &'a str,
    sha256: &'a str,
    byte_length: u64,
}

#[derive(Serialize)]
struct StructureChunk<'a> {
    offset: u64,
    b64: &'a str,
}

/// The verified finalize answer: what the delivered headers name.
struct Finalized {
    manifest_url: String,
    instance_id: String,
    document_id: String,
    verification_url: String,
}

/// Whether `value` is an identifier safe in a response header.
fn valid_identifier(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && value.bytes().all(|byte| byte.is_ascii_graphic())
}

/// Whether `value` is a canonical absolute `https` URL safe inside a `Link`.
fn valid_https_url(value: &str) -> bool {
    value.len() <= 2048
        && value
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && !matches!(byte, b'<' | b'>' | b'"'))
        && url::Url::parse(value).is_ok_and(|parsed| {
            parsed.scheme() == "https" && parsed.host_str().is_some() && parsed.as_str() == value
        })
}

/// The reason a probe call failed, logged with the probe's own code.
fn probe_refusal<E: Into<LargeRefusal>>(op: &str, error: ProbeCallError<E>) -> LargeRefusal {
    log::info!("encypher: large media probe op={op} code={}", error.code());
    match error {
        ProbeCallError::Source(error) => error.into(),
        ProbeCallError::Refused(refusal) if refusal.integrity => {
            LargeRefusal::Lane("source_provenance_invalid")
        }
        ProbeCallError::Refused(_) => LargeRefusal::Lane("probe_refused"),
        ProbeCallError::Failed(_) => LargeRefusal::Lane("probe_failed"),
    }
}

/// Whether `items` is the one program of an unchanged external plan: the
/// whole source, once, with nothing excluded or inserted.
fn covers_whole_source(items: &[Value], size: u64) -> bool {
    matches!(items, [item] if item.get("range").and_then(Value::as_array).is_some_and(|range| {
        range.len() == 2
            && range.first().and_then(Value::as_u64) == Some(0)
            && range.get(1).and_then(Value::as_u64) == Some(size)
    }) && item.as_object().is_some_and(|object| object.len() == 1))
}

/// The reason for an error answer of the prepared endpoints, by status and
/// failure code.
fn answer_refusal(status: StatusCode, code: Option<&str>) -> LargeRefusal {
    match (status.as_u16(), code) {
        (409, Some("TRUSTED_SERVER_POLICY_CHANGED")) => Reason::PolicyChanged.into(),
        (409, Some("PROBE_VERSION_UNSUPPORTED" | "PLAN_STALE")) => {
            LargeRefusal::Lane("probe_release_stale")
        }
        (409, Some("SOURCE_DIGEST_MISMATCH")) => LargeRefusal::Lane("source_digest_mismatch"),
        (422, Some("SOURCE_PROVENANCE_INVALID")) => LargeRefusal::Lane("source_provenance_invalid"),
        // A store the probe's disclosure did not report: kept, not signed.
        (422, Some("SOURCE_MANIFEST_PRESENT")) => LargeRefusal::Lane(EMBEDDED_PROVENANCE),
        (422, Some("HASH_MODE_UNSUPPORTED")) => LargeRefusal::Lane("format_unsupported"),
        (403 | 404, _) => Reason::Forbidden.into(),
        (409, _) => LargeRefusal::Lane("prepared_conflict"),
        (413, _) => Reason::PayloadTooLarge.into(),
        (422, _) => Reason::Unprocessable.into(),
        (429, _) => Reason::RateLimited.into(),
        (500..=599, _) => Reason::ServerError.into(),
        _ => Reason::UnexpectedStatus.into(),
    }
}

impl Signer {
    /// Largest source the prepared lane signs; larger media is delivered
    /// unsigned.
    pub(super) fn max_large_media_bytes(&self) -> u64 {
        self.config.max_large_media_bytes
    }

    /// The whole time core and this lane may spend on one large source, or
    /// none when the lane is off.
    pub(super) fn max_large_media_duration(&self) -> Duration {
        if self.config.max_large_media_bytes == 0 {
            Duration::ZERO
        } else {
            Duration::from_millis(self.config.max_large_media_ms)
        }
    }

    /// Sign one large source core pinned and hashed, returning the headers
    /// every response of it carries and the namespace it was signed under.
    /// Probe releases are always held to the pinned production key.
    pub(super) async fn sign_large(
        &self,
        services: &RuntimeServices,
        job: &LargeMediaJob<'_>,
    ) -> Result<LargeSignedMedia, LargeRefusal> {
        self.sign_large_with(services, job, &ReleaseAuthority::production())
            .await
    }

    /// [`Self::sign_large`] with the release `authority` given, which only
    /// tests set to anything but production.
    async fn sign_large_with(
        &self,
        services: &RuntimeServices,
        job: &LargeMediaJob<'_>,
        authority: &ReleaseAuthority,
    ) -> Result<LargeSignedMedia, LargeRefusal> {
        let source = job.source;
        let size = source.size();
        let reader_url = url::Url::parse(job.reader_url).map_err(|_| Reason::OutsideClaim)?;
        let reader_host = reader_url.host_str().ok_or(Reason::OutsideClaim)?;
        if !matches!(reader_url.scheme(), "https" | "http")
            || !self.claims_host(reader_host)
            || is_excluded_prefix(reader_url.path())
        {
            return Err(Reason::OutsideClaim.into());
        }
        if size == 0 || size > self.max_large_media_bytes() {
            return Err(Reason::MediaTooLarge.into());
        }
        if !valid_origin_host(job.source_origin_host) {
            return Err(Reason::InvalidOriginHost.into());
        }
        if !valid_policy_digest(job.source_sha256) {
            return Err(Reason::SourceDigestMismatch.into());
        }
        // The fetched address when it names the origin's bytes; otherwise
        // the reader's own address, which names the same bytes here because
        // they are delivered unchanged. A source with neither is refused.
        let reference = job
            .source_reference
            .filter(|reference| safe_source_reference(reference, job.source_origin_host))
            .or_else(|| {
                job.reader_reference.filter(|reference| {
                    *reference == job.reader_url && safe_source_reference(reference, reader_host)
                })
            })
            .ok_or(LargeRefusal::Lane("source_reference_required"))?;
        let mime = canonical_media_type(job.content_type);

        // What is left of the lane's budget after core hashed the source,
        // less the reserve a policy eviction may still use.
        let mut work = Deadline::new(
            services.timer(),
            job.remaining.saturating_sub(SKIP_WRITE_RESERVE),
        );
        work.start().await;
        // Asked live: the namespace core keeps the result under must be the
        // policy the service holds now, never a cached one a revocation
        // outlived.
        let policy_digest = self
            .request_policy(job.reader_url, services, &mut work)
            .await?;
        let lane = Lane {
            job,
            reference,
            mime: &mime,
            policy_digest: &policy_digest,
            authority,
        };
        let result = self.run_prepared(services, &lane, &mut work).await;
        if let Err(LargeRefusal::Module(Reason::PolicyChanged | Reason::PolicyDigestMismatch)) =
            &result
        {
            self.evict_policy(services.kv_store(), &mut Self::skip_deadline(services))
                .await;
        }
        let finalized = result?;
        let link = format!("<{}>; rel=\"c2pa-manifest\"", finalized.manifest_url);
        let mut headers: Vec<ResponseHeader> = Vec::with_capacity(4);
        for (name, value) in [
            (header::LINK, link.as_str()),
            (
                http::HeaderName::from_static(INSTANCE_ID_HEADER),
                finalized.instance_id.as_str(),
            ),
            (
                http::HeaderName::from_static(DOCUMENT_ID_HEADER),
                finalized.document_id.as_str(),
            ),
            (
                http::HeaderName::from_static(VERIFICATION_URL_HEADER),
                finalized.verification_url.as_str(),
            ),
        ] {
            headers.push((
                name,
                HeaderValue::from_str(value).map_err(|_| Reason::InvalidSignedResponse)?,
            ));
        }
        Ok(LargeSignedMedia {
            headers,
            namespace: self.kept_namespace(&policy_digest),
        })
    }

    /// The lane's steps after the gates: capabilities, probe, disclosure,
    /// prepare, plan check, version check, finalize, store check.
    async fn run_prepared(
        &self,
        services: &RuntimeServices,
        lane: &Lane<'_>,
        work: &mut Deadline<'_>,
    ) -> Result<Finalized, LargeRefusal> {
        let Lane {
            job,
            reference,
            mime,
            policy_digest,
            authority,
        } = *lane;
        let source = job.source;
        let size = source.size();
        let capabilities = self.capabilities(services, job.reader_url, work).await?;
        if capabilities.policy_digest != policy_digest {
            return Err(Reason::PolicyChanged.into());
        }
        if !capabilities.formats.iter().any(|format| format == mime) {
            return Err(LargeRefusal::Lane("format_unsupported"));
        }
        if size > capabilities.total_size_bytes {
            return Err(Reason::MediaTooLarge.into());
        }
        let probe = self
            .load_probe(services, authority, &capabilities.probes, work)
            .await?;
        let pinned = PinnedSource(source);

        let mut extracted = match work
            .race(probe.call(
                "extract",
                &json!({"mime": mime, "total_size": size, "disposition": "external"}),
                &pinned,
            ))
            .await
        {
            None => return Err(Reason::Timeout.into()),
            Some(result) => result.map_err(|error| probe_refusal("extract", error))?,
        };
        // An embedded manifest wins over a `Link`, so a remote one would be
        // ignored beside it, and a source whose XMP already names a manifest
        // has provenance of its own: either way the source keeps it. A
        // release that does not say whether the XMP names one cannot be
        // trusted to have looked.
        if extracted.get("credentials").is_some() {
            return Err(LargeRefusal::Lane(EMBEDDED_PROVENANCE));
        }
        match extracted
            .get("has_provenance_reference")
            .and_then(Value::as_bool)
        {
            Some(true) => return Err(LargeRefusal::Lane(REFERENCED_PROVENANCE)),
            Some(false) => {}
            None => return Err(LargeRefusal::Lane("probe_result_invalid")),
        }
        // The disclosure is taken out of the result, not copied: it is sent
        // at prepare and handed back to the probe at plan verification.
        let disclosure = extracted
            .get_mut("chunks")
            .map(Value::take)
            .ok_or(LargeRefusal::Lane("probe_result_invalid"))?;
        drop(extracted);
        let structure = disclosure
            .as_array()
            .ok_or(LargeRefusal::Lane("probe_result_invalid"))?
            .iter()
            .map(|chunk| {
                Some(StructureChunk {
                    offset: chunk.get("offset")?.as_u64()?,
                    b64: chunk.get("data")?.as_str()?,
                })
            })
            .collect::<Option<Vec<_>>>()
            .ok_or(LargeRefusal::Lane("probe_result_invalid"))?;

        let idempotency_key = sha256_hex(
            format!(
                "prepared\n{}\n{policy_digest}\n{}\n{reference}\n{}\n{mime}\n{}\n{size}\n{}",
                self.cache_context,
                job.reader_url,
                job.source_origin_host,
                job.source_sha256,
                source.version().etag(),
            )
            .as_bytes(),
        );
        let descriptor = probe.descriptor();
        let prepare = PrepareBody {
            page_url: job.reader_url,
            expected_policy_digest: policy_digest,
            protocol: PROTOCOL,
            probe: ProbeSelection {
                sha256: &descriptor.sha256,
                planner_id: &descriptor.planner_id,
            },
            source: PreparedSource {
                reference,
                origin_host: job.source_origin_host,
                mime,
                sha256: job.source_sha256,
                byte_length: size,
            },
            structure,
        };
        let mut prepared = self
            .post(
                services,
                PREPARE_PATH,
                &prepare,
                &idempotency_key,
                MAX_PREPARE_BYTES,
                work,
            )
            .await?;
        drop(prepare);
        let plan_id = prepared
            .get("plan_id")
            .and_then(Value::as_str)
            .filter(|id| id.len() == 36)
            .ok_or(LargeRefusal::Lane("prepare_answer_invalid"))?
            .to_owned();
        if prepared.get("policy_digest").and_then(Value::as_str) != Some(policy_digest) {
            return Err(Reason::PolicyDigestMismatch.into());
        }
        let unchanged = prepared
            .get("local_ops")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty)
            && prepared.get("effective_action").and_then(Value::as_str) == Some("c2pa.opened");
        let geometry = prepared
            .get_mut("geometry")
            .map(Value::take)
            .filter(|geometry| {
                geometry.get("disposition").and_then(Value::as_str) == Some("external")
            });
        drop(prepared);
        let Some(geometry) = geometry.filter(|_| unchanged) else {
            return Err(LargeRefusal::Lane("prepare_answer_invalid"));
        };

        // The probe recomputes the server's plan from the same bytes; an
        // unchanged external plan hashes the whole source once.
        let verified = match work
            .race(probe.call(
                "verify_plan",
                &json!({
                    "mime": mime,
                    "total_size": size,
                    "disclosure": disclosure,
                    "geometry": geometry,
                }),
                &pinned,
            ))
            .await
        {
            None => return Err(Reason::Timeout.into()),
            Some(result) => result.map_err(|error| probe_refusal("verify_plan", error))?,
        };
        let items = verified
            .get("hash_items")
            .and_then(Value::as_array)
            .ok_or(LargeRefusal::Lane("probe_result_invalid"))?;
        if !covers_whole_source(items, size) {
            return Err(LargeRefusal::Lane("plan_not_whole_source"));
        }
        drop(verified);
        match work.race(source.confirm_current()).await {
            None => return Err(Reason::Timeout.into()),
            Some(result) => result?,
        }

        let finalize = json!({
            "page_url": job.reader_url,
            "expected_policy_digest": policy_digest,
            "protocol": PROTOCOL,
            "plan_id": plan_id,
            "binding": {"type": "data_hash", "digest": job.source_sha256},
            "xmp_reference": false,
        });
        let manifest_limit = capabilities.manifest_limit();
        let answer_limit = manifest_limit
            .saturating_mul(4)
            .div_ceil(3)
            .saturating_add(FINALIZE_OVERHEAD_BYTES);
        let finalized = self
            .post(
                services,
                FINALIZE_PATH,
                &finalize,
                &idempotency_key,
                answer_limit,
                work,
            )
            .await
            .map_err(|refusal| match refusal {
                LargeRefusal::Module(Reason::ResponseTooLarge) => {
                    LargeRefusal::Lane(MANIFEST_TOO_LARGE)
                }
                other => other,
            })?;
        self.verify_finalized(&probe, &finalized, job, policy_digest, &capabilities, work)
            .await
    }

    /// Checks the finalize answer against the plan and the pinned source,
    /// and that the store it returns binds the whole-source digest.
    async fn verify_finalized(
        &self,
        probe: &ProbeModule,
        finalized: &Value,
        job: &LargeMediaJob<'_>,
        policy_digest: &str,
        capabilities: &Capabilities,
        work: &mut Deadline<'_>,
    ) -> Result<Finalized, LargeRefusal> {
        let text = |name: &str| finalized.get(name).and_then(Value::as_str);
        if text("policy_digest") != Some(policy_digest) {
            return Err(Reason::PolicyDigestMismatch.into());
        }
        let external = text("manifest_disposition") == Some("external")
            && text("xmp_reference") == Some("declined")
            && text("binding_algorithm") == Some("c2pa.hash.data")
            && finalized.get("expected_final_size").and_then(Value::as_u64)
                == Some(job.source.size());
        if !external {
            return Err(LargeRefusal::Lane("finalize_answer_invalid"));
        }
        let store_b64 = text("manifest_store_b64").ok_or(Reason::UnsignedResponse)?;
        let manifest_limit = capabilities.manifest_limit();
        if store_b64.len()
            > manifest_limit
                .saturating_mul(4)
                .div_ceil(3)
                .saturating_add(3)
        {
            return Err(LargeRefusal::Lane(MANIFEST_TOO_LARGE));
        }
        let store = STANDARD
            .decode(store_b64)
            .map_err(|_| Reason::InvalidSignedResponse)?;
        if store.is_empty() {
            return Err(Reason::InvalidSignedResponse.into());
        }
        if store.len() > manifest_limit {
            return Err(LargeRefusal::Lane(MANIFEST_TOO_LARGE));
        }
        if text("manifest_sha256") != Some(sha256_hex(&store).as_str()) {
            return Err(Reason::SignedDigestMismatch.into());
        }
        let manifest_url = text("manifest_url")
            .filter(|url| valid_https_url(url))
            .ok_or(Reason::InvalidSignedResponse)?;
        let verification_url = text("verification_url")
            .filter(|url| valid_https_url(url))
            .ok_or(Reason::InvalidSignedResponse)?;
        let instance_id = text("instance_id")
            .filter(|id| valid_identifier(id))
            .ok_or(Reason::InvalidSignedResponse)?;
        let document_id = text("document_id")
            .filter(|id| valid_identifier(id))
            .ok_or(Reason::InvalidSignedResponse)?;
        // The store's hard binding must be over the whole source, nothing
        // excluded, with the digest core computed over the pinned version.
        let checked = match work
            .race(probe.call_without_source::<MediaSourceError>(
                "check_store",
                &json!({
                    "manifest_store_b64": store_b64,
                    "digest": job.source_sha256,
                    "binding_recipe": {"type": "data_hash", "algorithm": "sha256", "exclusions": []},
                }),
            ))
            .await
        {
            None => return Err(Reason::Timeout.into()),
            Some(result) => result.map_err(|error| probe_refusal("check_store", error))?,
        };
        if checked.get("ok").and_then(Value::as_bool) != Some(true) {
            return Err(Reason::SignedDigestMismatch.into());
        }
        Ok(Finalized {
            manifest_url: manifest_url.to_owned(),
            instance_id: instance_id.to_owned(),
            document_id: document_id.to_owned(),
            verification_url: verification_url.to_owned(),
        })
    }

    /// The lane's capabilities for the asset at `page_url`.
    async fn capabilities(
        &self,
        services: &RuntimeServices,
        page_url: &str,
        work: &mut Deadline<'_>,
    ) -> Result<Capabilities, LargeRefusal> {
        let mut url = url::Url::parse(&format!(
            "{}{CAPABILITIES_PATH}",
            self.config.api_base_url.trim_end_matches('/')
        ))
        .map_err(|_| Reason::RequestBuild)?;
        url.query_pairs_mut().append_pair("page_url", page_url);
        let request = Request::builder()
            .method(Method::GET)
            .uri(url.as_str())
            .header(WORKER_TOKEN_HEADER, self.worker_token()?)
            .body(EdgeBody::empty())
            .map_err(|_| Reason::RequestBuild)?;
        let body = self
            .exchange(services, request, MAX_CAPABILITIES_BYTES, work)
            .await?;
        Capabilities::parse(&body).ok_or(LargeRefusal::Lane("capabilities_invalid"))
    }

    /// The newest verified release the service advertises, from the cache or
    /// fetched, compiled with the edge's limits.
    async fn load_probe(
        &self,
        services: &RuntimeServices,
        authority: &ReleaseAuthority,
        advertised: &[ProbeDescriptor],
        work: &mut Deadline<'_>,
    ) -> Result<ProbeModule, LargeRefusal> {
        let descriptor = authority.select(advertised).map_err(|error| {
            log::info!("encypher: large media probe release code={}", error.code());
            LargeRefusal::Lane(error.code())
        })?;
        let kv = services.kv_store();
        let key = format!("{PROBE_KEY_PREFIX}:{}", descriptor.sha256);
        let cached = match work.race(kv.get_bytes(&key)).await {
            Some(Ok(Some(bytes))) if sha256_hex(&bytes) == descriptor.sha256 => Some(bytes),
            _ => None,
        };
        let artifact = match cached {
            Some(bytes) => bytes,
            None => {
                let request = Request::builder()
                    .method(Method::GET)
                    .uri(format!(
                        "{}{PROBE_PATH}{}",
                        self.config.api_base_url.trim_end_matches('/'),
                        descriptor.artifact_path()
                    ))
                    .body(EdgeBody::empty())
                    .map_err(|_| Reason::RequestBuild)?;
                let bytes = self
                    .exchange(services, request, MAX_PROBE_BYTES, work)
                    .await?;
                if sha256_hex(&bytes) != descriptor.sha256 {
                    return Err(LargeRefusal::Lane("PROBE_RELEASE_INVALID"));
                }
                // Releases never change, so a cached copy needs no expiry
                // beyond the plan lifetime.
                let ttl = Duration::from_secs(self.config.plan_ttl_seconds);
                if !matches!(
                    work.race(kv.put_bytes_with_ttl(&key, bytes.clone(), ttl))
                        .await,
                    Some(Ok(()))
                ) {
                    log::debug!("encypher: probe release cache write failed");
                }
                bytes
            }
        };
        let loaded = match work
            // The host's default limits are the edge's: structure only, never
            // the payload. A release that runs out of fuel is refused.
            .race(ProbeModule::load(
                authority,
                descriptor,
                &artifact,
                ProbeLimits::default(),
            ))
            .await
        {
            None => return Err(Reason::Timeout.into()),
            Some(result) => result,
        };
        loaded.map_err(|error| {
            log::info!("encypher: large media probe release code={}", error.code());
            LargeRefusal::Lane(error.code())
        })
    }

    /// One authenticated JSON `POST` to a prepared endpoint.
    async fn post(
        &self,
        services: &RuntimeServices,
        path: &str,
        body: &impl Serialize,
        idempotency_key: &str,
        max_response_bytes: usize,
        work: &mut Deadline<'_>,
    ) -> Result<Value, LargeRefusal> {
        let body = serde_json::to_vec(body).map_err(|_| Reason::RequestBuild)?;
        let request = Request::builder()
            .method(Method::POST)
            .uri(format!(
                "{}{path}",
                self.config.api_base_url.trim_end_matches('/')
            ))
            .header(header::CONTENT_TYPE, "application/json")
            .header(WORKER_TOKEN_HEADER, self.worker_token()?)
            .header(IDEMPOTENCY_KEY_HEADER, idempotency_key)
            .body(EdgeBody::from(body))
            .map_err(|_| Reason::RequestBuild)?;
        let answer = self
            .exchange(services, request, max_response_bytes, work)
            .await?;
        serde_json::from_slice(&answer).map_err(|_| Reason::MalformedResponse.into())
    }

    /// One bounded call; a non-`200` answer is a refusal with its code.
    async fn exchange(
        &self,
        services: &RuntimeServices,
        request: Request<EdgeBody>,
        max_response_bytes: usize,
        work: &mut Deadline<'_>,
    ) -> Result<Bytes, LargeRefusal> {
        let limit = max_response_bytes.saturating_add(1);
        let response = self.send_api(services, request, limit, work).await?;
        let status = response.status();
        if status != StatusCode::OK {
            let code = work
                .race(
                    response
                        .into_body()
                        .into_bytes_bounded(media_api::MAX_ERROR_BODY_BYTES),
                )
                .await
                .and_then(Result::ok)
                .and_then(|body| media_api::error_code(&body));
            log::info!(
                "encypher: large media answer status={} code={}",
                status.as_u16(),
                code.as_deref().unwrap_or("none")
            );
            return Err(answer_refusal(status, code.as_deref()));
        }
        let body = match work
            .race(response.into_body().into_bytes_bounded(limit))
            .await
        {
            None => return Err(Reason::Timeout.into()),
            Some(Err(_)) => return Err(Reason::ResponseTooLarge.into()),
            Some(Ok(body)) => body,
        };
        if body.len() > max_response_bytes {
            return Err(Reason::ResponseTooLarge.into());
        }
        Ok(body)
    }
}

#[cfg(test)]
mod flow_tests;

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Arc;

    use futures::executor::block_on;
    use http::HeaderMap;
    use trusted_server_core::media_signing::SourceVersion;
    use trusted_server_core::platform::PlatformHttpClient;
    use trusted_server_core::platform::test_support::{
        StubHttpClient, build_services_with_http_client,
    };

    use crate::EncypherConfig;

    const SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

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

    /// The gate refusal for one job, made before any request is sent.
    fn refusal(
        reader_url: &str,
        source_reference: Option<&str>,
        reader_reference: Option<&str>,
        origin_host: &str,
    ) -> &'static str {
        let http = Arc::new(StubHttpClient::new());
        let services =
            build_services_with_http_client(Arc::clone(&http) as Arc<dyn PlatformHttpClient>);
        let origin = url::Url::parse("https://origin.example.com/video/ferry.mp4")
            .expect("should parse the origin URL");
        let headers = HeaderMap::new();
        let source = VersionedMediaSource::new(
            &services,
            &origin,
            &headers,
            "origin",
            SourceVersion::new("\"v1\"", 64 * 1024 * 1024).expect("should pin a strong ETag"),
        );
        let job = LargeMediaJob {
            reader_url,
            content_type: "video/mp4",
            source: &source,
            source_sha256: SHA,
            source_reference,
            reader_reference,
            source_origin_host: origin_host,
            remaining: Duration::from_secs(60),
        };
        let code = block_on(signer().sign_large(&services, &job))
            .expect_err("should refuse at the gates")
            .code();
        assert!(
            http.recorded_request_methods().is_empty(),
            "should refuse before any request"
        );
        code
    }

    #[test]
    fn a_large_source_without_a_usable_reference_is_refused_before_any_request() {
        let reader = "https://publisher.example.com/video/ferry.mp4";
        assert_eq!(
            refusal(
                "https://cdn.example.org/video/ferry.mp4",
                None,
                None,
                "origin.example.com"
            ),
            "outside_claim"
        );
        assert_eq!(
            refusal(reader, None, None, "Origin.example.com:8080"),
            "invalid_origin_host"
        );
        // A fetched address on another host than the one fetched, and a
        // reader address that is not the delivered one, name no source.
        assert_eq!(
            refusal(
                reader,
                Some("https://elsewhere.example.com/video/ferry.mp4"),
                Some("https://publisher.example.com/video/other.mp4"),
                "origin.example.com",
            ),
            "source_reference_required"
        );
        assert_eq!(
            refusal(reader, None, None, "origin.example.com"),
            "source_reference_required"
        );
    }

    #[test]
    fn only_a_single_whole_source_range_is_an_unchanged_plan() {
        assert!(covers_whole_source(&[json!({"range": [0, 100]})], 100));
        for items in [
            vec![json!({"range": [0, 99]})],
            vec![json!({"range": [0, 50]}), json!({"range": [50, 50]})],
            vec![json!({"range": [0, 100]}), json!({"literal": "QQ=="})],
            vec![json!({"range": [0, 100], "skip": true})],
            vec![],
        ] {
            assert!(
                !covers_whole_source(&items, 100),
                "should refuse {items:?} as an unchanged plan"
            );
        }
    }

    #[test]
    fn a_source_with_its_own_manifest_is_reported_as_kept_not_failed_or_signed() {
        let kept = answer_refusal(
            StatusCode::UNPROCESSABLE_ENTITY,
            Some("SOURCE_MANIFEST_PRESENT"),
        );
        assert_eq!(kept.code(), "source_has_embedded_provenance");
        assert!(
            kept.retains_provenance(),
            "should report the provenance kept"
        );
        for failed in [
            answer_refusal(
                StatusCode::UNPROCESSABLE_ENTITY,
                Some("HASH_MODE_UNSUPPORTED"),
            ),
            answer_refusal(
                StatusCode::UNPROCESSABLE_ENTITY,
                Some("SOURCE_PROVENANCE_INVALID"),
            ),
            LargeRefusal::Module(Reason::Timeout),
        ] {
            assert!(
                !failed.retains_provenance(),
                "{}: should not claim kept provenance",
                failed.code()
            );
        }
    }

    #[test]
    fn prepared_errors_map_to_specific_refusals() {
        assert_eq!(
            answer_refusal(StatusCode::CONFLICT, Some("TRUSTED_SERVER_POLICY_CHANGED")),
            LargeRefusal::Module(Reason::PolicyChanged)
        );
        assert_eq!(
            answer_refusal(StatusCode::CONFLICT, Some("PROBE_VERSION_UNSUPPORTED")).code(),
            "probe_release_stale"
        );
        assert_eq!(
            answer_refusal(
                StatusCode::UNPROCESSABLE_ENTITY,
                Some("SOURCE_PROVENANCE_INVALID")
            )
            .code(),
            "source_provenance_invalid"
        );
        assert_eq!(
            answer_refusal(StatusCode::FORBIDDEN, Some("claim_changed")),
            LargeRefusal::Module(Reason::Forbidden)
        );
    }

    #[test]
    fn capabilities_parse_only_an_enabled_v2_lane() {
        let body = |enabled: bool, protocol: &str| {
            serde_json::to_vec(&json!({
                "protocol": protocol,
                "enabled": enabled,
                "policy_digest": "a".repeat(64),
                "formats": ["video/mp4"],
                "maximums": {
                    "total_size_bytes": 1_000_000_000_u64,
                    "structure_decoded_bytes": 33_554_432,
                    "structure_chunks": 8192,
                    "manifest_decoded_bytes": 1_048_576,
                    "prepare_request_bytes": 50_331_648,
                },
                "probe": [],
            }))
            .expect("should encode")
        };
        let parsed = Capabilities::parse(&body(true, PROTOCOL)).expect("should parse");
        assert_eq!(parsed.formats, ["video/mp4"]);
        assert_eq!(parsed.manifest_decoded_bytes, 1_048_576);
        assert!(Capabilities::parse(&body(false, PROTOCOL)).is_none());
        assert!(Capabilities::parse(&body(true, "encypher-prepared-signing/1")).is_none());
    }
}
