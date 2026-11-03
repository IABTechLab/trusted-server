//! The prepared-signing steps the worker runs and the delivery instance
//! re-verifies, over any HTTP transport and any source reader.
//!
//! Nothing here uploads source bytes. A media item is planned from its
//! structure disclosure, hashed locally by the hash program the signed
//! probe verified, and finalized by digest. An external manifest binds the
//! whole unchanged source; an embedded one is carried in output bytes
//! assembled here from the verified edits and the locally built carrier,
//! then walked by the probe's `verify_output` so its binding reproduces the
//! digest the service signed. Every refusal is a [`Failure`] with a class
//! the worker damps by.
//!
//! The delivery instance never trusts what the worker says it signed. It
//! recovers the API's exact completed receipt with
//! [`recover_media`] or [`recover_text`], and checks it against the source
//! bytes and output with [`verify_external_receipt`] and
//! [`verify_embedded_receipt`].

use std::convert::Infallible;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use bytes::Bytes;
use http::Method;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use super::protocol::{Carrier, PublishedResult};
use crate::api::{self, EmbeddingPlan};
use crate::embedded;
use crate::media_api::{hex_lower, sha256_hex};
use crate::prepared_probe::{
    ProbeCallError, ProbeDescriptor, ProbeLimits, ProbeModule, ProbeSource, ReleaseAuthority,
};

/// The prepared protocol every prepared request carries.
pub const PREPARED_PROTOCOL: &str = "encypher-prepared-signing/2";
/// The worker contract version this pipeline speaks.
pub const CONTRACT_VERSION: u8 = 2;

const POLICY_PATH: &str = "/api/v1/public/cdn/trusted-server/policy";
const SIGN_PATH: &str = "/api/v1/public/cdn/sign";
const SIGN_RECOVER_PATH: &str = "/api/v1/public/cdn/sign/recover";
const CAPABILITIES_PATH: &str = "/api/v1/public/cdn/media/hash/capabilities";
const PREPARE_PATH: &str = "/api/v1/public/cdn/media/hash/prepare";
const FINALIZE_PATH: &str = "/api/v1/public/cdn/media/hash/finalize";
const MEDIA_RECOVER_PATH: &str = "/api/v1/public/cdn/media/hash/recover";
const PROBE_PATH: &str = "/api/v1/";

const MAX_POLICY_BYTES: usize = 1024;
const MAX_CAPABILITIES_BYTES: usize = 64 * 1024;
const MAX_PREPARE_BYTES: usize = 1024 * 1024;
const MAX_PROBE_BYTES: usize = 4 * 1024 * 1024;
const MAX_FINALIZED_MANIFEST_BYTES: usize = 2 * 1024 * 1024;
const FINALIZE_OVERHEAD_BYTES: usize = 64 * 1024;
/// The contract ceiling on one `insert_bytes` payload.
const MAX_INSERT_BYTES: usize = 65_536;
/// The contract ceilings on one structure disclosure: its chunks and their
/// decoded bytes.
const MAX_STRUCTURE_CHUNKS: usize = 65_536 + 8;
const MAX_STRUCTURE_BYTES: usize = 32 * 1024 * 1024;
/// The most an embedded output may exceed its source by: the largest
/// manifest store plus as much again for its carrier framing and the
/// plan's inserted bytes. A plan over it is refused before anything is
/// allocated or finalized, and a worker reserves at most this much beyond
/// the source for an output.
pub const MAX_OUTPUT_GROWTH_BYTES: u64 = 2 * MAX_FINALIZED_MANIFEST_BYTES as u64;

/// The largest output an embedded plan over a source of `source_length`
/// bytes may name.
#[must_use]
pub fn output_ceiling(source_length: u64) -> u64 {
    source_length.saturating_add(MAX_OUTPUT_GROWTH_BYTES)
}

/// How a failure is damped by the worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureClass {
    /// May succeed on a later attempt: transport, rate limit, server error.
    Transient,
    /// The policy changed under the operation; a new identity follows.
    PolicyChanged,
    /// A binding, plan, carrier, store or output check failed.
    Integrity,
    /// The source is one reader's, or not admitted as public.
    Privacy,
    /// The format, carrier or contract is not supported by the engine.
    Unsupported,
    /// The source carries provenance of its own, which is kept.
    ExistingProvenance,
    /// The API holds no completed result for the operation.
    Missing,
}

/// Why a step did not complete.
#[derive(Debug, Clone, PartialEq, Eq, derive_more::Display)]
#[display("{code} ({class:?})")]
pub struct Failure {
    /// A short stable code.
    pub code: String,
    /// How the worker damps it.
    pub class: FailureClass,
}

impl core::error::Error for Failure {}

impl Failure {
    /// A failure of `class` with `code`.
    #[must_use]
    pub fn new(class: FailureClass, code: &str) -> Self {
        Self {
            code: code.to_owned(),
            class,
        }
    }

    fn integrity(code: &str) -> Self {
        Self::new(FailureClass::Integrity, code)
    }

    fn transient(code: &str) -> Self {
        Self::new(FailureClass::Transient, code)
    }

    fn unsupported(code: &str) -> Self {
        Self::new(FailureClass::Unsupported, code)
    }
}

/// One request to the Encypher API.
#[derive(Debug, Clone)]
pub struct ApiRequest {
    /// The method.
    pub method: Method,
    /// The path and query below the API origin.
    pub path_and_query: String,
    /// A JSON body, if any.
    pub body: Option<Vec<u8>>,
    /// The `Idempotency-Key`, if any.
    pub idempotency_key: Option<String>,
    /// Whether the worker credential is sent. A probe artifact is fetched
    /// without it.
    pub credentialed: bool,
    /// The most response body bytes read.
    pub max_response_bytes: usize,
}

/// The API's answer: its status and bounded body.
#[derive(Debug, Clone)]
pub struct ApiAnswer {
    /// The status.
    pub status: u16,
    /// The body, at most the request's bound plus one byte.
    pub body: Bytes,
}

/// An HTTP transport to the Encypher API, the edge's platform client on the
/// delivery instance and a native client on the worker.
#[allow(async_fn_in_trait)]
pub trait ApiTransport {
    /// Sends `request` once.
    ///
    /// # Errors
    ///
    /// A transient [`Failure`] where no answer arrived.
    async fn exchange(&self, request: ApiRequest) -> Result<ApiAnswer, Failure>;
}

/// The bytes of one pinned source version, read at offsets.
#[allow(async_fn_in_trait)]
pub trait SourceReader {
    /// The version's size.
    fn size(&self) -> u64;

    /// Fills `destination` from `offset`.
    ///
    /// # Errors
    ///
    /// A [`Failure`] where the read fails or the version changed.
    async fn read_exact_at(&self, offset: u64, destination: &mut [u8]) -> Result<(), Failure>;
}

impl SourceReader for [u8] {
    fn size(&self) -> u64 {
        u64::try_from(self.len()).unwrap_or(u64::MAX)
    }

    async fn read_exact_at(&self, offset: u64, destination: &mut [u8]) -> Result<(), Failure> {
        let start =
            usize::try_from(offset).map_err(|_| Failure::integrity("read_out_of_bounds"))?;
        let end = start
            .checked_add(destination.len())
            .filter(|end| *end <= self.len())
            .ok_or_else(|| Failure::integrity("read_out_of_bounds"))?;
        destination.copy_from_slice(&self[start..end]);
        Ok(())
    }
}

/// A [`SourceReader`] as the probe reads it.
struct Probed<'a, S: ?Sized>(&'a S);

impl<S: SourceReader + ?Sized> ProbeSource for Probed<'_, S> {
    type Error = Failure;

    fn size(&self) -> u64 {
        self.0.size()
    }

    async fn read_exact_at(&self, offset: u64, destination: &mut [u8]) -> Result<(), Failure> {
        self.0.read_exact_at(offset, destination).await
    }
}

fn probe_failure<E: Into<Failure>>(op: &str, error: ProbeCallError<E>) -> Failure {
    log::info!("encypher preparation: probe op={op} code={}", error.code());
    match error {
        ProbeCallError::Source(error) => error.into(),
        ProbeCallError::Refused(refusal) if refusal.integrity => {
            Failure::integrity(&refusal.code.to_ascii_lowercase())
        }
        ProbeCallError::Refused(refusal) => {
            Failure::unsupported(&refusal.code.to_ascii_lowercase())
        }
        ProbeCallError::Failed(_) => Failure::transient("probe_failed"),
    }
}

impl From<Infallible> for Failure {
    fn from(never: Infallible) -> Self {
        match never {}
    }
}

/// The failure an error answer of the API names.
fn answer_failure(status: u16, body: &[u8]) -> Failure {
    let code = crate::media_api::error_code(body);
    let code = code.as_deref();
    match (status, code) {
        (409, Some("TRUSTED_SERVER_POLICY_CHANGED")) => {
            Failure::new(FailureClass::PolicyChanged, "policy_changed")
        }
        (409, Some("PROBE_VERSION_UNSUPPORTED" | "PLAN_STALE")) => {
            Failure::transient("probe_release_stale")
        }
        (409, Some("SOURCE_DIGEST_MISMATCH")) => Failure::integrity("source_digest_mismatch"),
        (422, Some("SOURCE_PROVENANCE_INVALID")) => Failure::integrity("source_provenance_invalid"),
        (422, Some("SOURCE_MANIFEST_PRESENT")) => Failure::new(
            FailureClass::ExistingProvenance,
            "source_has_embedded_provenance",
        ),
        (422, Some("HASH_MODE_UNSUPPORTED")) => Failure::unsupported("format_unsupported"),
        (404, _) => Failure::new(FailureClass::Missing, "result_not_retained"),
        (410, _) => Failure::integrity("result_retired"),
        (403, _) => Failure::new(FailureClass::Privacy, "forbidden"),
        (409, _) => Failure::integrity("prepared_conflict"),
        (413, _) => Failure::unsupported("payload_too_large"),
        (422, _) => Failure::unsupported("unprocessable"),
        (429, _) => Failure::transient("rate_limited"),
        (500..=599, _) => Failure::transient("server_error"),
        _ => Failure::transient("unexpected_status"),
    }
}

async fn exchange_ok<T: ApiTransport>(
    transport: &T,
    request: ApiRequest,
) -> Result<Bytes, Failure> {
    let limit = request.max_response_bytes;
    let answer = transport.exchange(request).await?;
    if answer.status != 200 {
        return Err(answer_failure(answer.status, &answer.body));
    }
    if answer.body.len() > limit {
        return Err(Failure::transient("response_too_large"));
    }
    Ok(answer.body)
}

async fn post_json<T: ApiTransport>(
    transport: &T,
    path: &str,
    body: &impl Serialize,
    idempotency_key: &str,
    max_response_bytes: usize,
) -> Result<Value, Failure> {
    let body = serde_json::to_vec(body).map_err(|_| Failure::integrity("request_build"))?;
    let answer = exchange_ok(
        transport,
        ApiRequest {
            method: Method::POST,
            path_and_query: path.to_owned(),
            body: Some(body),
            idempotency_key: Some(idempotency_key.to_owned()),
            credentialed: true,
            max_response_bytes,
        },
    )
    .await?;
    serde_json::from_slice(&answer).map_err(|_| Failure::transient("malformed_response"))
}

fn path_with_page_url(path: &str, page_url: &str) -> String {
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    query.append_pair("page_url", page_url);
    format!("{path}?{}", query.finish())
}

/// The policy the API holds now for `page_url`.
///
/// # Errors
///
/// A [`Failure`] for any answer other than a valid digest.
pub async fn policy_digest<T: ApiTransport>(
    transport: &T,
    page_url: &str,
) -> Result<String, Failure> {
    let body = exchange_ok(
        transport,
        ApiRequest {
            method: Method::GET,
            path_and_query: path_with_page_url(POLICY_PATH, page_url),
            body: None,
            idempotency_key: None,
            credentialed: true,
            max_response_bytes: MAX_POLICY_BYTES,
        },
    )
    .await?;
    api::parse_policy_response(&body).map_err(|_| Failure::transient("malformed_policy"))
}

/// What the prepared lane signs for a host, as the API answers it.
#[derive(Debug, Clone)]
pub struct Capabilities {
    /// The policy the answer was made under.
    pub policy_digest: String,
    /// Formats signed with an external manifest.
    pub formats: Vec<String>,
    /// Formats signed with an embedded manifest under contract version 2.
    pub embedded_formats: Vec<String>,
    /// Worker contract versions the API speaks.
    pub contract_versions: Vec<u8>,
    /// The largest source.
    pub total_size_bytes: u64,
    /// The largest decoded manifest store.
    pub manifest_decoded_bytes: usize,
    probes: Vec<ProbeDescriptor>,
}

impl Capabilities {
    fn parse(body: &[u8]) -> Option<Self> {
        let value: Value = serde_json::from_slice(body).ok()?;
        if value.get("protocol")?.as_str()? != PREPARED_PROTOCOL
            || !value.get("enabled")?.as_bool()?
        {
            return None;
        }
        let strings = |name: &str| -> Option<Vec<String>> {
            Some(match value.get(name) {
                None => Vec::new(),
                Some(list) => list
                    .as_array()?
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect(),
            })
        };
        let maximums = value.get("maximums")?;
        Some(Self {
            policy_digest: value.get("policy_digest")?.as_str()?.to_owned(),
            formats: strings("formats")?,
            embedded_formats: strings("embedded_formats")?,
            contract_versions: match value.get("contract_versions") {
                None => vec![1],
                Some(list) => list
                    .as_array()?
                    .iter()
                    .filter_map(Value::as_u64)
                    .filter_map(|version| u8::try_from(version).ok())
                    .collect(),
            },
            total_size_bytes: maximums.get("total_size_bytes")?.as_u64()?,
            manifest_decoded_bytes: usize::try_from(
                maximums.get("manifest_decoded_bytes")?.as_u64()?,
            )
            .ok()?,
            probes: ProbeDescriptor::parse_list(value.get("probe")),
        })
    }

    /// Whether the API signs `mime` with `carrier` under this pipeline's
    /// contract version. Decided by what the engine advertises, never by a
    /// list of the edge's own.
    #[must_use]
    pub fn supports(&self, mime: &str, carrier: Carrier) -> bool {
        self.contract_versions.contains(&CONTRACT_VERSION)
            && match carrier {
                Carrier::External => self.formats.iter().any(|format| format == mime),
                Carrier::Embedded => self.embedded_formats.iter().any(|format| format == mime),
            }
    }
}

/// The prepared lane's capabilities for `page_url`.
///
/// # Errors
///
/// A [`Failure`] for any answer other than enabled capabilities.
pub async fn capabilities<T: ApiTransport>(
    transport: &T,
    page_url: &str,
) -> Result<Capabilities, Failure> {
    let body = exchange_ok(
        transport,
        ApiRequest {
            method: Method::GET,
            path_and_query: path_with_page_url(CAPABILITIES_PATH, page_url),
            body: None,
            idempotency_key: None,
            credentialed: true,
            max_response_bytes: MAX_CAPABILITIES_BYTES,
        },
    )
    .await?;
    Capabilities::parse(&body).ok_or_else(|| Failure::unsupported("capabilities_unavailable"))
}

/// A signed probe release descriptor, as a reconstruction record keeps it so
/// recovery runs exactly the release a plan was verified with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProbeRelease {
    /// The probe ABI.
    pub abi: u64,
    /// The planner.
    pub planner_id: String,
    /// The release number.
    pub release: u64,
    /// The artifact SHA-256.
    pub sha256: String,
    /// The release key's signature over the other fields.
    pub signature: String,
}

impl ProbeRelease {
    fn descriptor(&self) -> ProbeDescriptor {
        ProbeDescriptor {
            abi: self.abi,
            planner_id: self.planner_id.clone(),
            release: self.release,
            sha256: self.sha256.clone(),
            signature: self.signature.clone(),
        }
    }
}

/// A verified, compiled probe release.
#[derive(Debug)]
pub struct LoadedProbe(ProbeModule);

impl LoadedProbe {
    /// The release's artifact SHA-256 and planner, which reconstruction
    /// records bind.
    #[must_use]
    pub fn identity(&self) -> (&str, &str) {
        let descriptor = self.0.descriptor();
        (&descriptor.sha256, &descriptor.planner_id)
    }

    /// The signed descriptor of the release, which a reconstruction record
    /// keeps.
    #[must_use]
    pub fn release(&self) -> ProbeRelease {
        let descriptor = self.0.descriptor();
        ProbeRelease {
            abi: descriptor.abi,
            planner_id: descriptor.planner_id.clone(),
            release: descriptor.release,
            sha256: descriptor.sha256.clone(),
            signature: descriptor.signature.clone(),
        }
    }
}

/// Where verified probe artifacts are kept between loads.
#[allow(async_fn_in_trait)]
pub trait ProbeArtifactCache {
    /// The artifact with SHA-256 `sha256`, if kept.
    async fn get(&self, sha256: &str) -> Option<Bytes>;
    /// Keeps `artifact`, whose SHA-256 is `sha256`.
    ///
    /// # Errors
    ///
    /// A [`Failure`] where the artifact may not be kept, such as a store
    /// whose byte quota refuses it. The load then fails before the release
    /// plans, finalizes or reconstructs anything, so nothing comes to
    /// depend on an artifact that was never retained.
    async fn put(&self, sha256: &str, artifact: Bytes) -> Result<(), Failure>;
}

/// The authenticated advertised artifact and release, without loading its WASM.
///
/// # Errors
///
/// A [`Failure`] when the newest compatible descriptor fails production trust.
pub fn advertised_release_identity(capabilities: &Capabilities) -> Result<(&str, u64), Failure> {
    let authority = ReleaseAuthority::production();
    let descriptor = authority
        .select(&capabilities.probes)
        .map_err(|error| Failure::unsupported(&error.code().to_ascii_lowercase()))?;
    Ok((descriptor.sha256.as_str(), descriptor.release))
}

/// The newest release the capabilities advertise that Encypher's pinned
/// production key signed, loaded from `cache` or fetched without
/// credentials and verified before it runs.
///
/// # Errors
///
/// A [`Failure`] where no advertised release verifies.
pub async fn load_probe<T: ApiTransport, C: ProbeArtifactCache>(
    transport: &T,
    cache: &C,
    capabilities: &Capabilities,
) -> Result<LoadedProbe, Failure> {
    let authority = ReleaseAuthority::production();
    let descriptor = authority
        .select(&capabilities.probes)
        .map_err(|error| Failure::unsupported(&error.code().to_ascii_lowercase()))?;
    load_descriptor(transport, cache, descriptor, &authority).await
}

/// Exactly `release`, for recovering a completed receipt: verified against
/// Encypher's pinned production key, floor and revocations before it runs,
/// never replaced by a newer release, and refused explicitly where it is no
/// longer allowed to run. Needs no capabilities, so recovery works while
/// new preparation is closed.
///
/// # Errors
///
/// A [`Failure`] where the release does not verify or cannot be fetched.
pub async fn load_release<T: ApiTransport, C: ProbeArtifactCache>(
    transport: &T,
    cache: &C,
    release: &ProbeRelease,
) -> Result<LoadedProbe, Failure> {
    load_descriptor(
        transport,
        cache,
        &release.descriptor(),
        &ReleaseAuthority::production(),
    )
    .await
}

async fn load_descriptor<T: ApiTransport, C: ProbeArtifactCache>(
    transport: &T,
    cache: &C,
    descriptor: &ProbeDescriptor,
    authority: &ReleaseAuthority,
) -> Result<LoadedProbe, Failure> {
    authority
        .verify(descriptor)
        .map_err(|error| Failure::integrity(&error.code().to_ascii_lowercase()))?;
    let artifact = match cache.get(&descriptor.sha256).await {
        Some(bytes) if sha256_hex(&bytes) == descriptor.sha256 => bytes,
        _ => {
            let bytes = exchange_ok(
                transport,
                ApiRequest {
                    method: Method::GET,
                    path_and_query: format!("{PROBE_PATH}{}", descriptor.artifact_path()),
                    body: None,
                    idempotency_key: None,
                    credentialed: false,
                    max_response_bytes: MAX_PROBE_BYTES,
                },
            )
            .await?;
            if sha256_hex(&bytes) != descriptor.sha256 {
                return Err(Failure::integrity("probe_release_invalid"));
            }
            cache.put(&descriptor.sha256, bytes.clone()).await?;
            bytes
        }
    };
    ProbeModule::load(authority, descriptor, &artifact, ProbeLimits::default())
        .await
        .map(LoadedProbe)
        .map_err(|error| Failure::unsupported(&error.code().to_ascii_lowercase()))
}

/// The API cache partition required by prepared text operations.
///
/// A closed scope keeps old or unsupported requests from using the legacy
/// content-and-rights cache during signing, recovery or result publication.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextCacheScope {
    /// Reuse only a result for the exact canonical sign request.
    ExactRequest,
}

/// The exact text sign request, as sent and as recovered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TextSignRequest {
    /// Canonical visible article text.
    pub text: String,
    /// Absolute page URL without its query.
    pub page_url: String,
    /// Optional page title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document_title: Option<String>,
    /// The article boundary selector.
    pub boundary_selector: String,
    /// The policy the request is frozen under.
    pub expected_policy_digest: String,
    /// Required exact-request cache isolation, including on recovery.
    pub cache_scope: TextCacheScope,
}

/// A text sign result.
#[derive(Debug, Clone, PartialEq)]
pub struct TextSigned {
    /// The marker plan.
    pub plan: EmbeddingPlan,
    /// The manifest URL, if any.
    pub manifest_url: Option<String>,
}

fn text_result(body: &[u8], request: &TextSignRequest) -> Result<TextSigned, Failure> {
    let parsed =
        api::parse_sign_response(body).map_err(|_| Failure::transient("malformed_response"))?;
    if !parsed.success {
        return Err(if parsed.error.as_deref() == Some("quota_exceeded") {
            Failure::transient("quota_exceeded")
        } else {
            Failure::unsupported("sign_rejected")
        });
    }
    if parsed.policy_digest.as_deref() != Some(request.expected_policy_digest.as_str()) {
        return Err(Failure::new(
            FailureClass::PolicyChanged,
            "policy_digest_mismatch",
        ));
    }
    let plan = parsed
        .embedding_plan
        .ok_or_else(|| Failure::integrity("no_plan"))?;
    let scalars = request.text.chars().count();
    api::validate_plan(&plan, parsed.manifest_url.as_deref(), scalars)
        .map_err(|_| Failure::integrity("invalid_plan"))?;
    Ok(TextSigned {
        plan,
        manifest_url: parsed.manifest_url,
    })
}

async fn text_call<T: ApiTransport>(
    transport: &T,
    path: &str,
    request: &TextSignRequest,
    idempotency_key: &str,
) -> Result<TextSigned, Failure> {
    let body = serde_json::to_vec(request).map_err(|_| Failure::integrity("request_build"))?;
    let answer = exchange_ok(
        transport,
        ApiRequest {
            method: Method::POST,
            path_and_query: path.to_owned(),
            body: Some(body),
            idempotency_key: Some(idempotency_key.to_owned()),
            credentialed: true,
            max_response_bytes: api::MAX_RESPONSE_BYTES,
        },
    )
    .await?;
    text_result(&answer, request)
}

/// Signs an article's text once under `idempotency_key`. Only the worker
/// calls this; a reader never does.
///
/// # Errors
///
/// A [`Failure`] for any answer other than a valid plan under the request's
/// policy.
pub async fn sign_text<T: ApiTransport>(
    transport: &T,
    request: &TextSignRequest,
    idempotency_key: &str,
) -> Result<TextSigned, Failure> {
    text_call(transport, SIGN_PATH, request, idempotency_key).await
}

/// Recovers the exact completed text result for `request` and
/// `idempotency_key`, without signing.
///
/// # Errors
///
/// A [`FailureClass::Missing`] failure where the API holds none.
pub async fn recover_text<T: ApiTransport>(
    transport: &T,
    request: &TextSignRequest,
    idempotency_key: &str,
) -> Result<TextSigned, Failure> {
    text_call(transport, SIGN_RECOVER_PATH, request, idempotency_key).await
}

/// The prepared source a prepare request names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedSourceRef {
    /// The public address of the exact source bytes.
    pub reference: String,
    /// The host the source was fetched from.
    pub origin_host: String,
    /// The canonical media type.
    pub mime: String,
    /// SHA-256 of every source byte.
    pub sha256: String,
    /// The source length.
    pub byte_length: u64,
    /// `origin` or `publisher_snapshot`.
    pub reference_kind: String,
}

/// The probe release a prepare request selects.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProbeSelection {
    /// The artifact SHA-256.
    pub sha256: String,
    /// The planner.
    pub planner_id: String,
}

/// One disclosed structure chunk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StructureChunk {
    /// Its source offset.
    pub offset: u64,
    /// Its bytes, base64.
    pub b64: String,
}

/// The exact contract version 2 prepare request, as sent and as recovered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrepareRequest {
    /// [`CONTRACT_VERSION`].
    pub contract_version: u8,
    /// `external_allowed` or `embedded_required`.
    pub carrier_requirement: String,
    /// The reader-facing address of the item.
    pub page_url: String,
    /// The policy frozen into the plan.
    pub expected_policy_digest: String,
    /// [`PREPARED_PROTOCOL`].
    pub protocol: String,
    /// The probe release.
    pub probe: ProbeSelection,
    /// The source.
    pub source: PreparedSourceRef,
    /// The structure disclosure.
    pub structure: Vec<StructureChunk>,
}

impl PrepareRequest {
    /// The carrier the request requires.
    #[must_use]
    pub fn carrier(&self) -> Carrier {
        if self.carrier_requirement == "embedded_required" {
            Carrier::Embedded
        } else {
            Carrier::External
        }
    }

    fn disclosure(&self) -> Value {
        Value::Array(
            self.structure
                .iter()
                .map(|chunk| json!({"offset": chunk.offset, "data": chunk.b64}))
                .collect(),
        )
    }
}

/// What one media item is prepared from.
#[derive(Debug, Clone)]
pub struct MediaJob<'a> {
    /// The reader-facing address of the item.
    pub page_url: &'a str,
    /// The policy it is prepared under.
    pub policy_digest: &'a str,
    /// The carrier required.
    pub carrier: Carrier,
    /// The canonical media type.
    pub mime: &'a str,
    /// SHA-256 of every source byte, computed by the caller as it captured
    /// the source.
    pub source_sha256: &'a str,
    /// The public reference, its host and its kind.
    pub reference: &'a str,
    /// The host the source was fetched from.
    pub origin_host: &'a str,
    /// `origin` or `publisher_snapshot`.
    pub reference_kind: &'a str,
    /// The idempotency key prepare and finalize send.
    pub idempotency_key: &'a str,
}

/// The external extract of `source`, refusing a source with provenance of
/// its own. It alone reports whether the XMP names a manifest, which the
/// embedded planner cannot see.
async fn inspect_provenance<S: SourceReader + ?Sized>(
    probe: &LoadedProbe,
    source: &S,
    mime: &str,
) -> Result<Value, Failure> {
    let inspected = probe
        .0
        .call(
            "extract",
            &json!({"mime": mime, "total_size": source.size(), "disposition": "external"}),
            &Probed(source),
        )
        .await
        .map_err(|error| probe_failure("extract", error))?;
    // An embedded manifest wins over a new one, and a reference in the XMP
    // is provenance of its own: either way the source keeps it.
    if inspected.get("credentials").is_some() {
        return Err(Failure::new(
            FailureClass::ExistingProvenance,
            "source_has_embedded_provenance",
        ));
    }
    match inspected
        .get("has_provenance_reference")
        .and_then(Value::as_bool)
    {
        Some(true) => Err(Failure::new(
            FailureClass::ExistingProvenance,
            "source_has_provenance_reference",
        )),
        Some(false) => Ok(inspected),
        None => Err(Failure::integrity("probe_result_invalid")),
    }
}

/// Refuses a disclosure that is not the source's own bytes. Every chunk is
/// non-empty, in order, without overlap, within the contract's ceilings and
/// byte for byte `source` at its offset. The probe takes a disclosure as
/// the parse of the source and never reads a disclosed byte again, so a
/// chunk the source does not hold would plan the output from structure the
/// source does not have.
fn check_structure(structure: &[StructureChunk], source: &[u8]) -> Result<(), Failure> {
    let mismatch = || Failure::integrity("structure_mismatch");
    if structure.is_empty() || structure.len() > MAX_STRUCTURE_CHUNKS {
        return Err(mismatch());
    }
    let mut decoded = Vec::new();
    let mut total = 0_usize;
    let mut end = 0_usize;
    for chunk in structure {
        // Three bytes per four characters bounds the decode before it
        // allocates.
        if chunk.b64.len() / 4 * 3 > MAX_STRUCTURE_BYTES - total {
            return Err(mismatch());
        }
        decoded.clear();
        STANDARD
            .decode_vec(&chunk.b64, &mut decoded)
            .map_err(|_| mismatch())?;
        total += decoded.len();
        let start = usize::try_from(chunk.offset).map_err(|_| mismatch())?;
        let chunk_end = start
            .checked_add(decoded.len())
            .filter(|chunk_end| *chunk_end <= source.len())
            .ok_or_else(mismatch)?;
        if decoded.is_empty() || start < end || source[start..chunk_end] != decoded[..] {
            return Err(mismatch());
        }
        end = chunk_end;
    }
    Ok(())
}

/// The extract step's disclosure, or the failure that keeps the source as
/// it is.
///
/// The provenance inspection, [`inspect_provenance`], runs first for every
/// carrier. A source with provenance of either kind is kept, never
/// overwritten. An embedded item's disclosure then comes from the embedded
/// extract.
async fn extract<S: SourceReader + ?Sized>(
    probe: &LoadedProbe,
    source: &S,
    mime: &str,
    carrier: Carrier,
) -> Result<Vec<StructureChunk>, Failure> {
    let inspected = inspect_provenance(probe, source, mime).await?;
    let extracted = match carrier {
        Carrier::External => inspected,
        Carrier::Embedded => {
            let embedded = probe
                .0
                .call(
                    "extract",
                    &json!({"mime": mime, "total_size": source.size()}),
                    &Probed(source),
                )
                .await
                .map_err(|error| probe_failure("extract", error))?;
            if embedded.get("credentials").is_some() {
                return Err(Failure::new(
                    FailureClass::ExistingProvenance,
                    "source_has_embedded_provenance",
                ));
            }
            embedded
        }
    };
    extracted
        .get("chunks")
        .and_then(Value::as_array)
        .ok_or_else(|| Failure::integrity("probe_result_invalid"))?
        .iter()
        .map(|chunk| {
            Some(StructureChunk {
                offset: chunk.get("offset")?.as_u64()?,
                b64: chunk.get("data")?.as_str()?.to_owned(),
            })
        })
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| Failure::integrity("probe_result_invalid"))
}

/// Runs a verified hash program over `source`: every range, then every
/// literal, in order, into one SHA-256.
async fn run_hash_program<S: SourceReader + ?Sized>(
    items: &Value,
    source: &S,
) -> Result<String, Failure> {
    const CHUNK: u64 = 1024 * 1024;
    let mut hasher = Sha256::new();
    let mut buffer = Vec::new();
    for item in items
        .as_array()
        .ok_or_else(|| Failure::integrity("hash_program_invalid"))?
    {
        if let Some(range) = item.get("range").and_then(Value::as_array) {
            let (Some(offset), Some(length)) = (
                range.first().and_then(Value::as_u64),
                range.get(1).and_then(Value::as_u64),
            ) else {
                return Err(Failure::integrity("hash_program_invalid"));
            };
            let end = offset
                .checked_add(length)
                .filter(|end| *end <= source.size())
                .ok_or_else(|| Failure::integrity("hash_program_out_of_bounds"))?;
            let mut at = offset;
            while at < end {
                let take = (end - at).min(CHUNK);
                buffer.resize(usize::try_from(take).unwrap_or(0), 0);
                source.read_exact_at(at, &mut buffer).await?;
                hasher.update(&buffer);
                at += take;
            }
        } else if let Some(literal) = item.get("literal").and_then(Value::as_str) {
            let bytes = STANDARD
                .decode(literal)
                .map_err(|_| Failure::integrity("hash_program_invalid"))?;
            hasher.update(&bytes);
        } else {
            return Err(Failure::integrity("hash_program_invalid"));
        }
    }
    Ok(hex_lower(&hasher.finalize()))
}

/// Whether `items` is the one program of an unchanged external plan: the
/// whole source, once, with nothing excluded or inserted.
fn covers_whole_source(items: &Value, size: u64) -> bool {
    matches!(items.as_array().map(Vec::as_slice), Some([item]) if item.get("range").and_then(Value::as_array).is_some_and(|range| {
        range.len() == 2
            && range.first().and_then(Value::as_u64) == Some(0)
            && range.get(1).and_then(Value::as_u64) == Some(size)
    }) && item.as_object().is_some_and(|object| object.len() == 1))
}

/// Writes `source` through the verified edits of `geometry` with the
/// locally built `carrier`, refusing any edit out of order, beyond the
/// source, malformed or over its bound, and any output whose size is not
/// the plan's.
fn compose(source: &[u8], geometry: &Value, carrier: &[u8]) -> Result<Vec<u8>, Failure> {
    let mismatch = || Failure::integrity("plan_mismatch");
    let rows = geometry
        .get("edits")
        .and_then(Value::as_array)
        .ok_or_else(mismatch)?;
    let source_length = u64::try_from(source.len()).unwrap_or(u64::MAX);
    let expected = geometry
        .get("expected_final_size")
        .and_then(Value::as_u64)
        .filter(|size| *size <= output_ceiling(source_length))
        .and_then(|size| usize::try_from(size).ok())
        .ok_or_else(mismatch)?;
    let mut output = Vec::with_capacity(expected);
    let mut cursor = 0_usize;
    for row in rows {
        let offset = row
            .get("offset")
            .and_then(Value::as_u64)
            .and_then(|offset| usize::try_from(offset).ok())
            .filter(|offset| *offset >= cursor && *offset <= source.len())
            .ok_or_else(mismatch)?;
        output.extend_from_slice(&source[cursor..offset]);
        cursor = offset;
        let decode = |row: &Value| {
            row.get("b64")
                .and_then(Value::as_str)
                .and_then(|text| STANDARD.decode(text).ok())
        };
        match row.get("op").and_then(Value::as_str) {
            Some("delete") => {
                let length = row
                    .get("length")
                    .and_then(Value::as_u64)
                    .and_then(|length| usize::try_from(length).ok())
                    .ok_or_else(mismatch)?;
                cursor = cursor
                    .checked_add(length)
                    .filter(|end| *end <= source.len())
                    .ok_or_else(mismatch)?;
            }
            Some("patch") => {
                let bytes = decode(row).ok_or_else(mismatch)?;
                cursor = cursor
                    .checked_add(bytes.len())
                    .filter(|end| *end <= source.len())
                    .ok_or_else(mismatch)?;
                output.extend_from_slice(&bytes);
            }
            Some("insert_carrier") => {
                let length = row.get("length").and_then(Value::as_u64);
                if length != Some(u64::try_from(carrier.len()).unwrap_or(u64::MAX)) {
                    return Err(Failure::integrity("carrier_mismatch"));
                }
                output.extend_from_slice(carrier);
            }
            Some("insert_bytes") => {
                let bytes = decode(row)
                    .filter(|bytes| !bytes.is_empty() && bytes.len() <= MAX_INSERT_BYTES)
                    .ok_or_else(mismatch)?;
                output.extend_from_slice(&bytes);
            }
            _ => return Err(mismatch()),
        }
        if output.len() > expected {
            return Err(Failure::integrity("output_verification_failed"));
        }
    }
    output.extend_from_slice(&source[cursor..]);
    if output.len() != expected {
        return Err(Failure::integrity("output_verification_failed"));
    }
    Ok(output)
}

fn text_field<'a>(value: &'a Value, name: &str) -> Option<&'a str> {
    value.get(name).and_then(Value::as_str)
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

/// The identifier headers a finalize answer names.
fn identity_headers(finalized: &Value) -> Result<Vec<(String, String)>, Failure> {
    let instance_id = text_field(finalized, "instance_id")
        .filter(|id| valid_identifier(id))
        .ok_or_else(|| Failure::integrity("invalid_signed_response"))?;
    let document_id = text_field(finalized, "document_id")
        .filter(|id| valid_identifier(id))
        .ok_or_else(|| Failure::integrity("invalid_signed_response"))?;
    let mut headers = vec![
        (
            "x-encypher-manifest-instance-id".to_owned(),
            instance_id.to_owned(),
        ),
        ("x-encypher-document-id".to_owned(), document_id.to_owned()),
    ];
    if let Some(url) = text_field(finalized, "verification_url") {
        if !valid_https_url(url) {
            return Err(Failure::integrity("invalid_signed_response"));
        }
        headers.push(("x-encypher-verification-url".to_owned(), url.to_owned()));
    }
    Ok(headers)
}

/// The decoded manifest store a finalize answer carries, checked against
/// its stated digest and `limit`.
fn finalized_store(finalized: &Value, limit: usize) -> Result<(String, Vec<u8>), Failure> {
    let store_b64 = text_field(finalized, "manifest_store_b64")
        .ok_or_else(|| Failure::integrity("unsigned_response"))?;
    if store_b64.len() > limit.saturating_mul(4).div_ceil(3).saturating_add(3) {
        return Err(Failure::unsupported("manifest_too_large"));
    }
    let store = STANDARD
        .decode(store_b64)
        .map_err(|_| Failure::integrity("invalid_signed_response"))?;
    if store.is_empty() || store.len() > limit {
        return Err(Failure::integrity("invalid_signed_response"));
    }
    if text_field(finalized, "manifest_sha256") != Some(sha256_hex(&store).as_str()) {
        return Err(Failure::integrity("signed_digest_mismatch"));
    }
    Ok((store_b64.to_owned(), store))
}

async fn check_store(
    probe: &LoadedProbe,
    store_b64: &str,
    digest: &str,
    recipe: &Value,
) -> Result<(), Failure> {
    let checked = probe
        .0
        .call_without_source::<Failure>(
            "check_store",
            &json!({"manifest_store_b64": store_b64, "digest": digest, "binding_recipe": recipe}),
        )
        .await
        .map_err(|error| probe_failure("check_store", error))?;
    if checked.get("ok").and_then(Value::as_bool) == Some(false) {
        return Err(Failure::integrity("store_mismatch"));
    }
    Ok(())
}

fn prepare_request(
    job: &MediaJob<'_>,
    probe: &LoadedProbe,
    size: u64,
    structure: Vec<StructureChunk>,
) -> PrepareRequest {
    let (sha256, planner_id) = probe.identity();
    PrepareRequest {
        contract_version: CONTRACT_VERSION,
        carrier_requirement: match job.carrier {
            Carrier::External => "external_allowed",
            Carrier::Embedded => "embedded_required",
        }
        .to_owned(),
        page_url: job.page_url.to_owned(),
        expected_policy_digest: job.policy_digest.to_owned(),
        protocol: PREPARED_PROTOCOL.to_owned(),
        probe: ProbeSelection {
            sha256: sha256.to_owned(),
            planner_id: planner_id.to_owned(),
        },
        source: PreparedSourceRef {
            reference: job.reference.to_owned(),
            origin_host: job.origin_host.to_owned(),
            mime: job.mime.to_owned(),
            sha256: job.source_sha256.to_owned(),
            byte_length: size,
            reference_kind: job.reference_kind.to_owned(),
        },
        structure,
    }
}

/// The scalar binding type a verified plan finalizes with: `data_hash` for
/// every external plan, whose manifest binds the whole unchanged source,
/// and the type of the embedded plan's own recipe otherwise, which the
/// server chose from the format (`bmff_hash` for ISO BMFF, `data_hash` for
/// the rest). Any other recipe, such as a Merkle or collection binding, is
/// not a scalar this lane finalizes.
fn binding_type(request: &PrepareRequest, geometry: &Value) -> Result<&'static str, Failure> {
    match request.carrier() {
        Carrier::External => Ok("data_hash"),
        Carrier::Embedded => match geometry
            .get("binding_recipe")
            .and_then(|recipe| text_field(recipe, "type"))
        {
            Some("data_hash") => Ok("data_hash"),
            Some("bmff_hash") => Ok("bmff_hash"),
            _ => Err(Failure::unsupported("binding_recipe_unsupported")),
        },
    }
}

fn finalize_body(planned: &PlannedMedia) -> Result<Value, Failure> {
    let request = &planned.request;
    let binding = binding_type(request, &planned.geometry)?;
    Ok(json!({
        "contract_version": request.contract_version,
        "carrier_requirement": request.carrier_requirement,
        "page_url": request.page_url,
        "expected_policy_digest": request.expected_policy_digest,
        "protocol": PREPARED_PROTOCOL,
        "plan_id": planned.plan_id,
        "binding": {"type": binding, "digest": planned.digest},
        "xmp_reference": false,
    }))
}

fn checked_plan(prepared: &Value, request: &PrepareRequest) -> Result<(String, Value), Failure> {
    let plan_id = text_field(prepared, "plan_id")
        .filter(|id| id.len() == 36)
        .ok_or_else(|| Failure::integrity("prepare_answer_invalid"))?
        .to_owned();
    if text_field(prepared, "policy_digest") != Some(request.expected_policy_digest.as_str()) {
        return Err(Failure::new(
            FailureClass::PolicyChanged,
            "policy_digest_mismatch",
        ));
    }
    let geometry = prepared
        .get("geometry")
        .cloned()
        .ok_or_else(|| Failure::integrity("prepare_answer_invalid"))?;
    let disposition = text_field(&geometry, "disposition");
    match request.carrier() {
        Carrier::External => {
            let unchanged = prepared
                .get("local_ops")
                .and_then(Value::as_array)
                .is_some_and(Vec::is_empty)
                && text_field(prepared, "effective_action") == Some("c2pa.opened");
            if disposition != Some("external") || !unchanged {
                return Err(Failure::integrity("prepare_answer_invalid"));
            }
        }
        Carrier::Embedded => {
            if disposition != Some("embedded") {
                return Err(Failure::unsupported("embedded_plan_unavailable"));
            }
            binding_type(request, &geometry)?;
            let within = geometry
                .get("expected_final_size")
                .and_then(Value::as_u64)
                .is_some_and(|size| size <= output_ceiling(request.source.byte_length));
            if !within {
                return Err(Failure::unsupported("output_too_large"));
            }
        }
    }
    Ok((plan_id, geometry))
}

/// The digest a verified plan's hash program yields over `source`, after
/// the probe recomputed the server's plan from the same bytes.
async fn verified_digest<S: SourceReader + ?Sized>(
    probe: &LoadedProbe,
    request: &PrepareRequest,
    geometry: &Value,
    source: &S,
) -> Result<String, Failure> {
    let verified = probe
        .0
        .call(
            "verify_plan",
            &json!({
                "mime": request.source.mime,
                "total_size": source.size(),
                "disclosure": request.disclosure(),
                "geometry": geometry,
            }),
            &Probed(source),
        )
        .await
        .map_err(|error| probe_failure("verify_plan", error))?;
    let items = verified
        .get("hash_items")
        .ok_or_else(|| Failure::integrity("probe_result_invalid"))?;
    if request.carrier() == Carrier::External {
        if !covers_whole_source(items, source.size()) {
            return Err(Failure::integrity("plan_not_whole_source"));
        }
        // The whole source, once: its digest is the one the caller
        // computed while it captured every byte.
        return Ok(request.source.sha256.clone());
    }
    run_hash_program(items, source).await
}

/// Whether a finalize answer's scalar binding is exactly `kind` over
/// `digest`.
fn binding_names(finalized: &Value, kind: &str, digest: &str) -> bool {
    finalized.get("binding").is_some_and(|binding| {
        text_field(binding, "type") == Some(kind) && text_field(binding, "digest") == Some(digest)
    })
}

/// The external result a finalize answer supports for `request`.
async fn external_result(
    probe: &LoadedProbe,
    request: &PrepareRequest,
    finalized: &Value,
    manifest_limit: usize,
) -> Result<PublishedResult, Failure> {
    if text_field(finalized, "policy_digest") != Some(request.expected_policy_digest.as_str()) {
        return Err(Failure::new(
            FailureClass::PolicyChanged,
            "policy_digest_mismatch",
        ));
    }
    let external = text_field(finalized, "manifest_disposition") == Some("external")
        && text_field(finalized, "binding_algorithm") == Some("c2pa.hash.data")
        && finalized.get("expected_final_size").and_then(Value::as_u64)
            == Some(request.source.byte_length)
        && binding_names(finalized, "data_hash", &request.source.sha256);
    if !external {
        return Err(Failure::integrity("finalize_answer_invalid"));
    }
    let (store_b64, _) = finalized_store(finalized, manifest_limit)?;
    check_store(
        probe,
        &store_b64,
        &request.source.sha256,
        &json!({"type": "data_hash", "algorithm": "sha256", "exclusions": []}),
    )
    .await?;
    let manifest_url = text_field(finalized, "manifest_url")
        .filter(|url| valid_https_url(url))
        .ok_or_else(|| Failure::integrity("invalid_signed_response"))?;
    let mut headers = vec![(
        "link".to_owned(),
        format!("<{manifest_url}>; rel=\"c2pa-manifest\""),
    )];
    headers.extend(identity_headers(finalized)?);
    if !headers
        .iter()
        .any(|(name, _)| name == "x-encypher-verification-url")
    {
        return Err(Failure::integrity("invalid_signed_response"));
    }
    if request.source.byte_length > u64::try_from(usize::MAX).unwrap_or(u64::MAX) {
        return Err(Failure::integrity("source_too_large"));
    }
    Ok(PublishedResult::External {
        source_sha256: request.source.sha256.clone(),
        headers,
    })
}

/// The embedded output a finalize answer supports for `request` over
/// `source`, built and walked here, and the result a reader applies.
async fn embedded_output(
    probe: &LoadedProbe,
    request: &PrepareRequest,
    geometry: &Value,
    digest: &str,
    finalized: &Value,
    source: &[u8],
    manifest_limit: usize,
) -> Result<(Vec<u8>, PublishedResult), Failure> {
    if text_field(finalized, "policy_digest") != Some(request.expected_policy_digest.as_str()) {
        return Err(Failure::new(
            FailureClass::PolicyChanged,
            "policy_digest_mismatch",
        ));
    }
    let bound = binding_names(finalized, binding_type(request, geometry)?, digest)
        && finalized.get("expected_final_size") == geometry.get("expected_final_size");
    if !bound {
        return Err(Failure::integrity("finalize_answer_invalid"));
    }
    let (store_b64, _) = finalized_store(finalized, manifest_limit)?;
    let carrier_sha256 = text_field(finalized, "carrier_sha256")
        .filter(|sha| super::protocol::is_digest(sha))
        .ok_or_else(|| Failure::integrity("invalid_signed_response"))?;
    let built = probe
        .0
        .call_without_source::<Failure>(
            "build_carrier",
            &json!({
                "mime": request.source.mime,
                "geometry": geometry,
                "manifest_store_b64": store_b64,
                "carrier_sha256": carrier_sha256,
            }),
        )
        .await
        .map_err(|error| probe_failure("build_carrier", error))?;
    let carrier_b64 = text_field(&built, "carrier_b64")
        .ok_or_else(|| Failure::integrity("probe_result_invalid"))?;
    let carrier = STANDARD
        .decode(carrier_b64)
        .map_err(|_| Failure::integrity("probe_result_invalid"))?;
    if sha256_hex(&carrier) != carrier_sha256 {
        return Err(Failure::integrity("carrier_mismatch"));
    }
    let output = compose(source, geometry, &carrier)?;
    let walked = probe
        .0
        .call(
            "verify_output",
            &json!({
                "total_size": output.len(),
                "mime": request.source.mime,
                "geometry": geometry,
                "carrier_b64": carrier_b64,
            }),
            &Probed(output.as_slice()),
        )
        .await
        .map_err(|error| probe_failure("verify_output", error))?;
    let items = walked
        .get("hash_items")
        .ok_or_else(|| Failure::integrity("probe_result_invalid"))?;
    if run_hash_program(items, output.as_slice()).await? != digest {
        return Err(Failure::integrity("output_verification_failed"));
    }
    let recipe = geometry
        .get("binding_recipe")
        .cloned()
        .ok_or_else(|| Failure::integrity("plan_mismatch"))?;
    check_store(probe, &store_b64, digest, &recipe).await?;
    if request.source.mime == "image/svg+xml" {
        embedded::verify_svg_carrier(source, &output)
            .map_err(|_| Failure::integrity("inline_svg_altered"))?;
    }
    let result = PublishedResult::Embedded {
        source_sha256: request.source.sha256.clone(),
        output_sha256: sha256_hex(&output),
        content_type: request.source.mime.clone(),
        output_b64: STANDARD.encode(&output),
        headers: identity_headers(finalized)?,
    };
    Ok((output, result))
}

/// A verified plan, recorded durably by the worker before it finalizes, so
/// a crash after finalize recovers the receipt rather than planning again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlannedMedia {
    /// The exact prepare request sent.
    pub request: PrepareRequest,
    /// The signed release the plan was verified with.
    pub release: ProbeRelease,
    /// The plan's identifier.
    pub plan_id: String,
    /// The geometry the probe verified against the source, persisted before
    /// finalize as a reconstruction input. Never authoritative on its own:
    /// every receipt check requires the API's recovered plan to carry
    /// exactly this geometry and verifies the recovered one itself.
    pub geometry: Value,
    /// The digest the verified hash program yields over the source.
    pub digest: String,
    /// The idempotency key prepare and finalize send.
    pub idempotency_key: String,
}

/// Plans one media item: extract, prepare and the probe's plan check, then
/// the local hash program. No source byte is uploaded, and nothing is
/// signed yet.
///
/// # Errors
///
/// A [`Failure`] naming the step that refused.
pub async fn plan_media<T: ApiTransport, S: SourceReader + ?Sized>(
    transport: &T,
    probe: &LoadedProbe,
    capabilities: &Capabilities,
    job: &MediaJob<'_>,
    source: &S,
) -> Result<PlannedMedia, Failure> {
    if !capabilities.supports(job.mime, job.carrier) {
        return Err(Failure::unsupported(match job.carrier {
            Carrier::External => "format_unsupported",
            Carrier::Embedded => "unsupported_carrier",
        }));
    }
    if capabilities.policy_digest != job.policy_digest {
        return Err(Failure::new(FailureClass::PolicyChanged, "policy_changed"));
    }
    if source.size() == 0 || source.size() > capabilities.total_size_bytes {
        return Err(Failure::unsupported("media_too_large"));
    }
    let structure = extract(probe, source, job.mime, job.carrier).await?;
    let request = prepare_request(job, probe, source.size(), structure);
    let prepared = post_json(
        transport,
        PREPARE_PATH,
        &request,
        job.idempotency_key,
        MAX_PREPARE_BYTES,
    )
    .await?;
    let (plan_id, geometry) = checked_plan(&prepared, &request)?;
    let digest = verified_digest(probe, &request, &geometry, source).await?;
    Ok(PlannedMedia {
        request,
        release: probe.release(),
        plan_id,
        geometry,
        digest,
        idempotency_key: job.idempotency_key.to_owned(),
    })
}

/// Finalizes a verified plan by its digest, then recovers the exact
/// receipt and rebuilds the item from it with
/// [`reconstruct_media`], so the worker's own output is made exactly as the
/// delivery instance will check it. An embedded item needs its whole
/// `source`. Returns embedded output bytes, or `None` for an external item.
///
/// # Errors
///
/// A [`Failure`] naming the step that refused.
pub async fn finalize_media<T: ApiTransport>(
    transport: &T,
    probe: &LoadedProbe,
    planned: &PlannedMedia,
    whole_source: Option<&[u8]>,
) -> Result<Option<Vec<u8>>, Failure> {
    same_release(probe, &planned.request)?;
    if planned.request.carrier() == Carrier::Embedded && whole_source.is_none() {
        return Err(Failure::unsupported("embedded_requires_bounded_source"));
    }
    let answer_limit = MAX_FINALIZED_MANIFEST_BYTES
        .saturating_mul(4)
        .div_ceil(3)
        .saturating_add(FINALIZE_OVERHEAD_BYTES);
    post_json(
        transport,
        FINALIZE_PATH,
        &finalize_body(planned)?,
        &planned.idempotency_key,
        answer_limit,
    )
    .await?;
    let recovered = recover_media(transport, &planned.request, &planned.idempotency_key).await?;
    reconstruct_media(probe, planned, &recovered, whole_source).await
}

/// The exact completed receipt the API recovers for a prepare request.
#[derive(Debug, Clone)]
pub struct RecoveredMedia {
    prepare: Value,
    finalize: Value,
}

/// Recovers the API's exact completed receipt for `request` under
/// `idempotency_key`, without preparing, finalizing or reserving anything.
///
/// # Errors
///
/// A [`FailureClass::Missing`] failure where the API holds none, and a
/// [`Failure`] for any other refusal or a receipt of another protocol or
/// policy.
pub async fn recover_media<T: ApiTransport>(
    transport: &T,
    request: &PrepareRequest,
    idempotency_key: &str,
) -> Result<RecoveredMedia, Failure> {
    let answer = post_json(
        transport,
        MEDIA_RECOVER_PATH,
        request,
        idempotency_key,
        MAX_FINALIZED_MANIFEST_BYTES
            .saturating_mul(4)
            .div_ceil(3)
            .saturating_add(MAX_PREPARE_BYTES)
            .saturating_add(FINALIZE_OVERHEAD_BYTES),
    )
    .await?;
    if text_field(&answer, "protocol") != Some(PREPARED_PROTOCOL)
        || text_field(&answer, "policy_digest") != Some(request.expected_policy_digest.as_str())
    {
        return Err(Failure::integrity("receipt_invalid"));
    }
    let prepare = answer
        .get("prepare")
        .cloned()
        .ok_or_else(|| Failure::integrity("receipt_invalid"))?;
    let finalize = answer
        .get("finalize")
        .cloned()
        .ok_or_else(|| Failure::integrity("receipt_invalid"))?;
    Ok(RecoveredMedia { prepare, finalize })
}

/// Whether `probe` is exactly the release `request` was planned with.
fn same_release(probe: &LoadedProbe, request: &PrepareRequest) -> Result<(), Failure> {
    let (sha256, planner_id) = probe.identity();
    if sha256 == request.probe.sha256 && planner_id == request.probe.planner_id {
        Ok(())
    } else {
        Err(Failure::integrity("probe_release_mismatch"))
    }
}

/// The geometry of the API's recovered plan, after its plan check, where it
/// is exactly the plan `planned` recorded: the same plan identifier in the
/// prepare and finalize answers and the same verified geometry. The
/// recovered geometry is the authoritative one; the recorded copy only has
/// to agree with it.
fn recovered_geometry(
    planned: &PlannedMedia,
    recovered: &RecoveredMedia,
) -> Result<Value, Failure> {
    let (plan_id, geometry) = checked_plan(&recovered.prepare, &planned.request)?;
    if plan_id != planned.plan_id
        || text_field(&recovered.finalize, "plan_id") != Some(planned.plan_id.as_str())
    {
        return Err(Failure::integrity("receipt_plan_mismatch"));
    }
    if geometry != planned.geometry {
        return Err(Failure::integrity("receipt_geometry_mismatch"));
    }
    Ok(geometry)
}

/// The embedded output and result a recovered receipt supports over
/// `source`, the hash program of the recovered plan run here over the
/// actual bytes.
async fn embedded_from_receipt(
    probe: &LoadedProbe,
    planned: &PlannedMedia,
    recovered: &RecoveredMedia,
    source: &[u8],
) -> Result<(Vec<u8>, PublishedResult), Failure> {
    let request = &planned.request;
    if sha256_hex(source) != request.source.sha256
        || u64::try_from(source.len()).ok() != Some(request.source.byte_length)
    {
        return Err(Failure::integrity("source_digest_mismatch"));
    }
    let geometry = recovered_geometry(planned, recovered)?;
    let digest = verified_digest(probe, request, &geometry, source).await?;
    if digest != planned.digest {
        return Err(Failure::integrity("plan_digest_mismatch"));
    }
    embedded_output(
        probe,
        request,
        &geometry,
        &digest,
        &recovered.finalize,
        source,
        MAX_FINALIZED_MANIFEST_BYTES,
    )
    .await
}

/// Rebuilds a completed item from its recovered receipt with exactly the
/// release it was planned with, without preparing or finalizing again: the
/// worker's path after a crash, a lost publication or an evicted result.
/// An embedded item needs its whole `source`. Plan expiry is never checked:
/// a completed receipt is reconstructed whatever its plan's lifetime was.
/// Returns embedded output bytes, or `None` for an external item.
///
/// # Errors
///
/// A [`Failure`] for a receipt that does not verify against the recorded
/// plan and source.
pub async fn reconstruct_media(
    probe: &LoadedProbe,
    planned: &PlannedMedia,
    recovered: &RecoveredMedia,
    whole_source: Option<&[u8]>,
) -> Result<Option<Vec<u8>>, Failure> {
    let request = &planned.request;
    same_release(probe, request)?;
    match (request.carrier(), whole_source) {
        (Carrier::External, _) => {
            recovered_geometry(planned, recovered)?;
            external_result(
                probe,
                request,
                &recovered.finalize,
                MAX_FINALIZED_MANIFEST_BYTES,
            )
            .await?;
            Ok(None)
        }
        (Carrier::Embedded, Some(source)) => {
            let (output, _) = embedded_from_receipt(probe, planned, recovered, source).await?;
            Ok(Some(output))
        }
        (Carrier::Embedded, None) => Err(Failure::unsupported("embedded_requires_bounded_source")),
    }
}

/// Refuses `source` where it carries provenance of its own: an embedded
/// manifest, or an XMP reference to one. An external manifest delivered
/// over either would never be read, so the source keeps it, whatever the
/// worker's disclosure showed. Runs exactly the release `request` was
/// planned with, under its probe budgets, over the caller's actual bytes:
/// a bounded item's hash-checked bytes or a large one's pinned version.
///
/// # Errors
///
/// A [`FailureClass::ExistingProvenance`] failure for a source with
/// provenance, and a [`Failure`] where `probe` is not the request's release
/// or cannot inspect the source.
pub async fn inspect_external_source<S: SourceReader + ?Sized>(
    probe: &LoadedProbe,
    request: &PrepareRequest,
    source: &S,
) -> Result<(), Failure> {
    same_release(probe, request)?;
    inspect_provenance(probe, source, &request.source.mime).await?;
    Ok(())
}

/// Checks a recovered external receipt against `planned`, whose source the
/// caller verified independently and inspected with
/// [`inspect_external_source`]: the bytes of a bounded item hashed by the
/// caller, a large one by its own pinned whole read. Needs no capabilities,
/// so a completed receipt is checked while new preparation is closed.
///
/// # Errors
///
/// A [`Failure`] for a receipt that does not bind the request's exact
/// source, policy, plan and manifest.
pub async fn verify_external_receipt(
    probe: &LoadedProbe,
    planned: &PlannedMedia,
    recovered: &RecoveredMedia,
) -> Result<PublishedResult, Failure> {
    let request = &planned.request;
    if request.carrier() != Carrier::External {
        return Err(Failure::integrity("carrier_mismatch"));
    }
    if planned.digest != request.source.sha256 {
        return Err(Failure::integrity("plan_digest_mismatch"));
    }
    same_release(probe, request)?;
    recovered_geometry(planned, recovered)?;
    external_result(
        probe,
        request,
        &recovered.finalize,
        MAX_FINALIZED_MANIFEST_BYTES,
    )
    .await
}

/// Checks a recovered embedded receipt against the actual `source` bytes
/// and the worker's `output`. The worker's disclosure must be the source's
/// own bytes, and the source must carry no provenance of its own, however
/// little of it the disclosure shows. The probe's plan check and hash
/// program over the source must yield the digest the receipt signed, and
/// the output the delivery instance builds itself from the receipt must be
/// exactly the worker's and walk back to that digest. A self-consistent
/// output with changed pixels, markup or structure is refused. Needs no
/// capabilities.
///
/// # Errors
///
/// A [`Failure`] for any mismatch.
pub async fn verify_embedded_receipt(
    probe: &LoadedProbe,
    planned: &PlannedMedia,
    recovered: &RecoveredMedia,
    source: &[u8],
    output: &[u8],
) -> Result<PublishedResult, Failure> {
    if planned.request.carrier() != Carrier::Embedded {
        return Err(Failure::integrity("carrier_mismatch"));
    }
    same_release(probe, &planned.request)?;
    // The disclosure seeds the probe's plan check, and an embedded one never
    // shows the XMP: neither is taken on the worker's word.
    check_structure(&planned.request.structure, source)?;
    inspect_provenance(probe, source, &planned.request.source.mime).await?;
    let (built, result) = embedded_from_receipt(probe, planned, recovered, source).await?;
    if built != output {
        return Err(Failure::integrity("output_substituted"));
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;

    #[test]
    fn prepared_text_requires_the_exact_request_cache_scope() {
        let request = json!({
            "text": "Harbor council votes",
            "page_url": "https://www.example.com/news/a",
            "document_title": "Harbor",
            "boundary_selector": "article",
            "expected_policy_digest": "a".repeat(64),
            "cache_scope": "exact_request",
        });
        let parsed: TextSignRequest =
            serde_json::from_value(request.clone()).expect("should admit the exact scope");
        assert_eq!(
            serde_json::to_value(parsed).expect("should preserve the frozen request"),
            request
        );
        let mut missing = request.clone();
        missing
            .as_object_mut()
            .expect("should be an object")
            .remove("cache_scope");
        assert!(
            serde_json::from_value::<TextSignRequest>(missing).is_err(),
            "an old request must not silently inherit the current cache contract"
        );
        for scope in [
            Value::Null,
            json!(""),
            json!("legacy"),
            json!("EXACT_REQUEST"),
        ] {
            let mut unsupported = request.clone();
            unsupported["cache_scope"] = scope;
            assert!(
                serde_json::from_value::<TextSignRequest>(unsupported).is_err(),
                "only the exact_request cache scope is admitted"
            );
        }
    }

    #[test]
    fn synchronous_text_request_keeps_its_existing_wire_contract() {
        let request = api::SignRequest {
            text: "Harbor council votes",
            page_url: "https://www.example.com/news/a",
            document_title: Some("Harbor"),
            boundary_selector: "article",
            expected_policy_digest: "policy",
        };
        assert_eq!(
            serde_json::to_value(request).expect("should serialize the synchronous request"),
            json!({
                "text": "Harbor council votes",
                "page_url": "https://www.example.com/news/a",
                "document_title": "Harbor",
                "boundary_selector": "article",
                "expected_policy_digest": "policy",
            }),
            "synchronous signing must not opt into prepared cache isolation"
        );
    }

    #[test]
    fn compose_applies_edits_in_order_and_refuses_overlap_and_wrong_sizes() {
        let source = b"abcdefgh";
        let geometry = json!({
            "edits": [
                {"op": "patch", "offset": 1, "b64": STANDARD.encode(b"XY")},
                {"op": "insert_carrier", "offset": 4, "length": 3},
                {"op": "delete", "offset": 6, "length": 2},
            ],
            "expected_final_size": 9,
        });
        let output = compose(source, &geometry, b"CCC").expect("should compose");
        assert_eq!(output, b"aXYdCCCef", "should apply every edit in order");

        let wrong_carrier = compose(source, &geometry, b"CC").expect_err("should refuse");
        assert_eq!(wrong_carrier.code, "carrier_mismatch");

        let overlapping = json!({
            "edits": [
                {"op": "delete", "offset": 4, "length": 2},
                {"op": "patch", "offset": 3, "b64": STANDARD.encode(b"Z")},
            ],
            "expected_final_size": 6,
        });
        assert_eq!(
            compose(source, &overlapping, b"")
                .expect_err("should refuse")
                .code,
            "plan_mismatch"
        );

        let wrong_size = json!({"edits": [], "expected_final_size": 7});
        assert_eq!(
            compose(source, &wrong_size, b"")
                .expect_err("should refuse")
                .code,
            "output_verification_failed"
        );
    }

    #[test]
    fn hash_program_hashes_ranges_and_literals_and_refuses_out_of_bounds() {
        let source = b"hello world";
        let items = json!([
            {"range": [0, 5]},
            {"literal": STANDARD.encode(b"!")},
            {"range": [5, 6]},
        ]);
        let digest = futures::executor::block_on(run_hash_program(&items, source.as_slice()))
            .expect("should hash");
        assert_eq!(
            digest,
            sha256_hex(b"hello! world"),
            "should hash in program order"
        );
        let beyond = json!([{"range": [6, 6]}]);
        assert_eq!(
            futures::executor::block_on(run_hash_program(&beyond, source.as_slice()))
                .expect_err("should refuse")
                .code,
            "hash_program_out_of_bounds"
        );
    }

    #[test]
    fn whole_source_program_is_only_one_unexcluded_range() {
        assert!(covers_whole_source(&json!([{"range": [0, 10]}]), 10));
        assert!(!covers_whole_source(&json!([{"range": [0, 9]}]), 10));
        assert!(!covers_whole_source(
            &json!([{"range": [0, 5]}, {"range": [5, 5]}]),
            10
        ));
        assert!(!covers_whole_source(
            &json!([{"range": [0, 10], "extra": 1}]),
            10
        ));
    }

    #[test]
    fn capabilities_decide_carrier_support_from_engine_advertisement() {
        let body = json!({
            "protocol": PREPARED_PROTOCOL,
            "enabled": true,
            "policy_digest": "a".repeat(64),
            "formats": ["image/jpeg"],
            "contract_versions": [1, 2],
            "embedded_formats": ["image/png"],
            "maximums": {"total_size_bytes": 10, "manifest_decoded_bytes": 10},
            "probe": [],
        });
        let parsed = Capabilities::parse(body.to_string().as_bytes()).expect("should parse");
        assert!(parsed.supports("image/jpeg", Carrier::External));
        assert!(!parsed.supports("image/jpeg", Carrier::Embedded));
        assert!(parsed.supports("image/png", Carrier::Embedded));

        let mut legacy = body.clone();
        legacy
            .as_object_mut()
            .expect("object")
            .remove("contract_versions");
        let legacy = Capabilities::parse(legacy.to_string().as_bytes()).expect("should parse");
        assert!(
            !legacy.supports("image/png", Carrier::Embedded),
            "an engine that never advertised version 2 supports no new carrier"
        );
    }

    // --- the publication boundary over the real signed probe ---------------

    const TIFF_MIME: &str = "image/tiff";
    const PHOTOMETRIC: u16 = 262;
    const TIFF_CARRIER_LENGTH: u64 = 4097;
    const POLICY_DIGEST: &str = "1111111111111111111111111111111111111111111111111111111111111111";
    const PLAN_ID: &str = "00000000-0000-4000-8000-000000000018";
    const IDEMPOTENCY_KEY: &str = "tiff-publication-fixture";

    /// The production-signed release 18 probe every retained receipt
    /// fixture was planned with.
    fn signed_probe() -> LoadedProbe {
        let release: ProbeRelease =
            serde_json::from_slice(include_bytes!("../fixtures/prepared/descriptor.json"))
                .expect("should parse the production-signed descriptor");
        let module = block_on(ProbeModule::load(
            &ReleaseAuthority::production(),
            &release.descriptor(),
            include_bytes!("../fixtures/prepared/probe.wasm"),
            ProbeLimits::default(),
        ))
        .expect("should load the production-signed probe");
        LoadedProbe(module)
    }

    fn le_u16(bytes: &[u8], at: usize) -> u16 {
        u16::from_le_bytes([bytes[at], bytes[at + 1]])
    }

    fn le_u32(bytes: &[u8], at: usize) -> u32 {
        u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
    }

    /// One classic little-endian IFD entry.
    fn tiff_entry(tag: u16, kind: u16, count: u32, value: u32) -> [u8; 12] {
        let mut entry = [0_u8; 12];
        entry[..2].copy_from_slice(&tag.to_le_bytes());
        entry[2..4].copy_from_slice(&kind.to_le_bytes());
        entry[4..8].copy_from_slice(&count.to_le_bytes());
        entry[8..].copy_from_slice(&value.to_le_bytes());
        entry
    }

    /// A manifest-free, single-IFD, uncompressed 4x2 grayscale TIFF whose
    /// samples read as `BlackIsZero`, with `xmp` as its tag 700 packet.
    fn grayscale_tiff(xmp: Option<&[u8]>) -> Vec<u8> {
        const SAMPLES: [u8; 8] = [0, 32, 64, 96, 128, 160, 192, 255];
        let count: u16 = if xmp.is_some() { 10 } else { 9 };
        let pixels = 8 + 2 + 12 * u32::from(count) + 4;
        let mut entries = vec![
            tiff_entry(256, 3, 1, 4),
            tiff_entry(257, 3, 1, 2),
            tiff_entry(258, 3, 1, 8),
            tiff_entry(259, 3, 1, 1),
            tiff_entry(PHOTOMETRIC, 3, 1, 1),
            tiff_entry(273, 4, 1, pixels),
            tiff_entry(277, 3, 1, 1),
            tiff_entry(278, 3, 1, 2),
            tiff_entry(279, 4, 1, 8),
        ];
        if let Some(xmp) = xmp {
            let length = u32::try_from(xmp.len()).expect("should fit");
            entries.push(tiff_entry(700, 1, length, pixels + 8));
        }
        let mut tiff = b"II".to_vec();
        tiff.extend_from_slice(&42_u16.to_le_bytes());
        tiff.extend_from_slice(&8_u32.to_le_bytes());
        tiff.extend_from_slice(&count.to_le_bytes());
        for entry in &entries {
            tiff.extend_from_slice(entry);
        }
        tiff.extend_from_slice(&0_u32.to_le_bytes());
        tiff.extend_from_slice(&SAMPLES);
        if let Some(xmp) = xmp {
            tiff.extend_from_slice(xmp);
        }
        tiff
    }

    /// The entries of the IFD the header of `tiff` points at.
    fn active_entries(tiff: &[u8]) -> Vec<[u8; 12]> {
        let ifd = usize::try_from(le_u32(tiff, 4)).expect("should fit");
        (0..usize::from(le_u16(tiff, ifd)))
            .map(|index| {
                let start = ifd + 2 + 12 * index;
                tiff[start..start + 12]
                    .try_into()
                    .expect("should be one entry")
            })
            .collect()
    }

    /// The active IFD's `PhotometricInterpretation`.
    fn photometric(tiff: &[u8]) -> Option<u16> {
        active_entries(tiff)
            .iter()
            .find(|entry| le_u16(entry.as_slice(), 0) == PHOTOMETRIC)
            .map(|entry| le_u16(entry.as_slice(), 8))
    }

    /// The canonical C2PA 2.4 `main_ifd_when_single` plan for a classic
    /// little-endian TIFF of `source_length` bytes whose one IFD holds
    /// `entries`, laid out as the engine's TIFF planner lays it: the header
    /// repointed at a rebuilt IFD after the carrier, holding those entries
    /// and the C2PA entry.
    fn tiff_geometry(source_length: u64, entries: &[[u8; 12]]) -> Value {
        let carrier = TIFF_CARRIER_LENGTH;
        let leading = source_length % 2;
        let manifest_length = carrier - leading;
        let trailing = manifest_length % 2;
        let rebuilt_at = source_length + carrier + trailing;
        let mut rebuilt = entries.to_vec();
        rebuilt.push(tiff_entry(
            0xCD41,
            7,
            u32::try_from(manifest_length).expect("should fit"),
            u32::try_from(source_length + leading).expect("should fit"),
        ));
        rebuilt.sort_by_key(|entry| le_u16(entry.as_slice(), 0));
        let mut inserted = vec![0_u8; usize::try_from(trailing).expect("should fit")];
        inserted.extend_from_slice(
            &u16::try_from(rebuilt.len())
                .expect("should fit")
                .to_le_bytes(),
        );
        for entry in &rebuilt {
            inserted.extend_from_slice(entry);
        }
        inserted.extend_from_slice(&0_u32.to_le_bytes());
        let pointer = u32::try_from(rebuilt_at).expect("should fit").to_le_bytes();
        let inserted_length = u64::try_from(inserted.len()).expect("should fit");
        json!({
            "disposition": "embedded",
            "edits": [
                {"op": "patch", "offset": 4, "b64": STANDARD.encode(pointer)},
                {"op": "insert_carrier", "offset": source_length, "length": carrier},
                {"op": "insert_bytes", "offset": source_length, "b64": STANDARD.encode(&inserted)},
            ],
            "carrier_span": [0, carrier],
            "carrier_output_span": [source_length, carrier],
            "expected_final_size": source_length + carrier + inserted_length,
            "binding_recipe": {
                "type": "data_hash",
                "algorithm": "sha256",
                "exclusions": [{"offset": source_length, "length": carrier}],
            },
            "carrier_recipe": {
                "type": "tiff",
                "big": false,
                "little_endian": true,
                "entries": entries.iter().map(|entry| entry.to_vec()).collect::<Vec<_>>(),
                "next_ifd": 0,
                "original_size": source_length,
            },
            "planner_options": {"tiff_placement": "main_ifd_when_single"},
        })
    }

    fn length_of(source: &[u8]) -> u64 {
        u64::try_from(source.len()).expect("should fit")
    }

    fn tiff_request(
        probe: &LoadedProbe,
        source: &[u8],
        structure: Vec<StructureChunk>,
    ) -> PrepareRequest {
        let source_sha256 = sha256_hex(source);
        let reference =
            format!("https://publisher.example.com/_encypher/preparation/source/{source_sha256}");
        let job = MediaJob {
            page_url: "https://publisher.example.com/article",
            policy_digest: POLICY_DIGEST,
            carrier: Carrier::Embedded,
            mime: TIFF_MIME,
            source_sha256: &source_sha256,
            reference: &reference,
            origin_host: "publisher.example.com",
            reference_kind: "publisher_snapshot",
            idempotency_key: IDEMPOTENCY_KEY,
        };
        prepare_request(&job, probe, length_of(source), structure)
    }

    /// The plan record for `request` and a recovered receipt carrying
    /// `geometry`. The finalize answer names the plan but holds no signed
    /// manifest: no genuine receipt for a TIFF is among the fixtures, so a
    /// check that gets this far accepted the structure, the geometry and the
    /// digest over the source, and refuses only for want of a signature.
    fn tiff_receipt(
        probe: &LoadedProbe,
        request: PrepareRequest,
        geometry: &Value,
        digest: String,
    ) -> (PlannedMedia, RecoveredMedia) {
        let recovered = RecoveredMedia {
            prepare: json!({
                "plan_id": PLAN_ID,
                "policy_digest": POLICY_DIGEST,
                "geometry": geometry,
            }),
            finalize: json!({"plan_id": PLAN_ID, "policy_digest": POLICY_DIGEST}),
        };
        let planned = PlannedMedia {
            request,
            release: probe.release(),
            plan_id: PLAN_ID.to_owned(),
            geometry: geometry.clone(),
            digest,
            idempotency_key: IDEMPOTENCY_KEY.to_owned(),
        };
        (planned, recovered)
    }

    /// `structure` with the `PhotometricInterpretation` value rewritten to
    /// `value`, every other disclosed byte as it was.
    fn with_photometric(structure: &[StructureChunk], value: u16) -> Vec<StructureChunk> {
        // The fifth entry of the IFD at offset 8; its value field starts
        // eight bytes into the entry.
        let field = 8 + 2 + 12 * 4 + 8;
        let mut forged = structure.to_vec();
        let chunk = forged
            .iter_mut()
            .find(|chunk| {
                let length = STANDARD
                    .decode(&chunk.b64)
                    .map_or(0, |bytes| length_of(&bytes));
                chunk.offset <= field && field + 2 <= chunk.offset + length
            })
            .expect("the disclosure should hold the photometric entry");
        let mut bytes = STANDARD.decode(&chunk.b64).expect("should be base64");
        let at = usize::try_from(field - chunk.offset).expect("should fit");
        bytes[at..at + 2].copy_from_slice(&value.to_le_bytes());
        chunk.b64 = STANDARD.encode(bytes);
        forged
    }

    fn zero_carrier() -> Vec<u8> {
        vec![0_u8; usize::try_from(TIFF_CARRIER_LENGTH).expect("should fit")]
    }

    /// A worker that discloses a false TIFF structure for the true source
    /// gets a plan the signed probe verifies over the true bytes, because
    /// the disclosure seeds the probe's parse: the output would carry a false
    /// `PhotometricInterpretation` under the true source digest. Publication
    /// refuses it, and the honest structure still passes every source check.
    #[test]
    fn publication_refuses_a_disclosure_the_source_does_not_hold() {
        let probe = signed_probe();
        let source = grayscale_tiff(None);
        let honest = block_on(extract(
            &probe,
            source.as_slice(),
            TIFF_MIME,
            Carrier::Embedded,
        ))
        .expect("the signed probe should extract a manifest-free TIFF");
        let forged = with_photometric(&honest, 0);
        assert_ne!(forged, honest, "the forgery should change the disclosure");

        let honest_geometry = tiff_geometry(length_of(&source), &active_entries(&source));
        let mut forged_entries = active_entries(&source);
        forged_entries[4][8..10].copy_from_slice(&0_u16.to_le_bytes());
        let forged_geometry = tiff_geometry(length_of(&source), &forged_entries);

        // The geometry is the engine's own: the probe verifies the honest
        // plan over the true source and refuses the forged one there.
        let honest_request = tiff_request(&probe, &source, honest);
        let honest_digest = block_on(verified_digest(
            &probe,
            &honest_request,
            &honest_geometry,
            source.as_slice(),
        ))
        .expect("the signed probe should verify the canonical plan");
        let refused = block_on(verified_digest(
            &probe,
            &honest_request,
            &forged_geometry,
            source.as_slice(),
        ))
        .expect_err("an honest disclosure should not admit the forged plan");
        assert_eq!(
            refused.class,
            FailureClass::Integrity,
            "the forged plan should be a plan mismatch over honest structure"
        );

        // Seeded with the false structure, the same probe verifies the
        // forged plan over the same true bytes.
        let forged_request = tiff_request(&probe, &source, forged);
        assert_eq!(
            forged_request.source, honest_request.source,
            "the forged request should name the true source, digest and length"
        );
        let forged_digest = block_on(verified_digest(
            &probe,
            &forged_request,
            &forged_geometry,
            source.as_slice(),
        ))
        .expect("the signed probe should take the disclosure as the source's structure");
        assert_ne!(
            forged_digest, honest_digest,
            "the forged plan should bind other bytes"
        );

        // What that plan delivers: the true samples under a WhiteIsZero
        // active IFD.
        let honest_output =
            compose(&source, &honest_geometry, &zero_carrier()).expect("should compose");
        let forged_output =
            compose(&source, &forged_geometry, &zero_carrier()).expect("should compose");
        assert_eq!(photometric(&source), Some(1), "the source is BlackIsZero");
        assert_eq!(
            photometric(&honest_output),
            Some(1),
            "the honest output should keep BlackIsZero"
        );
        assert_eq!(
            photometric(&forged_output),
            Some(0),
            "the forged output should read the same samples as WhiteIsZero"
        );
        assert_eq!(
            &forged_output[8..source.len()],
            &source[8..],
            "every source byte after the header pointer should be unchanged"
        );

        // Publication refuses the forged structure before it reads the
        // receipt; the honest structure reaches the receipt.
        let (planned, recovered) =
            tiff_receipt(&probe, forged_request, &forged_geometry, forged_digest);
        let failure = block_on(verify_embedded_receipt(
            &probe,
            &planned,
            &recovered,
            &source,
            &forged_output,
        ))
        .expect_err("publication should refuse a disclosure the source does not hold");
        assert_eq!(
            failure,
            Failure::integrity("structure_mismatch"),
            "the forged disclosure should be refused as not the source's own"
        );

        let (planned, recovered) =
            tiff_receipt(&probe, honest_request, &honest_geometry, honest_digest);
        let failure = block_on(verify_embedded_receipt(
            &probe,
            &planned,
            &recovered,
            &source,
            &honest_output,
        ))
        .expect_err("the fixture receipt holds no signed manifest");
        assert_eq!(
            failure.code, "finalize_answer_invalid",
            "the honest structure should pass every source check and stop only at the unsigned receipt"
        );
    }

    /// A worker that skips the provenance inspection and discloses only the
    /// embedded structure of a TIFF whose XMP names a remote manifest gets a
    /// plan the probe verifies over the true bytes: the packet lies outside
    /// the structure. Publication inspects the source itself and keeps its
    /// provenance.
    #[test]
    fn publication_keeps_provenance_the_disclosure_leaves_out() {
        let probe = signed_probe();
        let xmp = br#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description xmlns:dcterms="http://purl.org/dc/terms/" dcterms:provenance="https://manifests.example.com/original.c2pa"/></rdf:RDF></x:xmpmeta>"#;
        let source = grayscale_tiff(Some(xmp));
        let kept = block_on(extract(
            &probe,
            source.as_slice(),
            TIFF_MIME,
            Carrier::Embedded,
        ))
        .expect_err("the worker's extract should keep the source's provenance");
        assert_eq!(
            kept,
            Failure::new(
                FailureClass::ExistingProvenance,
                "source_has_provenance_reference"
            ),
            "the signed probe should find the XMP reference"
        );

        // The embedded extract alone, as a worker skipping the inspection
        // runs it.
        let embedded = block_on(probe.0.call(
            "extract",
            &json!({"mime": TIFF_MIME, "total_size": length_of(&source)}),
            &Probed(source.as_slice()),
        ))
        .map_err(|error| probe_failure("extract", error))
        .expect("the embedded extract should not read the XMP");
        let structure = embedded["chunks"]
            .as_array()
            .expect("should disclose chunks")
            .iter()
            .map(|chunk| StructureChunk {
                offset: chunk["offset"].as_u64().expect("should have an offset"),
                b64: chunk["data"].as_str().expect("should have data").to_owned(),
            })
            .collect();
        let geometry = tiff_geometry(length_of(&source), &active_entries(&source));
        let request = tiff_request(&probe, &source, structure);
        let digest = block_on(verified_digest(
            &probe,
            &request,
            &geometry,
            source.as_slice(),
        ))
        .expect("the signed probe should verify the plan over the true source");
        let output = compose(&source, &geometry, &zero_carrier()).expect("should compose");

        let (planned, recovered) = tiff_receipt(&probe, request, &geometry, digest);
        let failure = block_on(verify_embedded_receipt(
            &probe, &planned, &recovered, &source, &output,
        ))
        .expect_err("publication should keep the source's provenance");
        assert_eq!(
            failure,
            Failure::new(
                FailureClass::ExistingProvenance,
                "source_has_provenance_reference"
            ),
            "publication should inspect the source it publishes over"
        );
    }
}
