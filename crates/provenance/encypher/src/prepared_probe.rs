//! The signed probe release, run on the edge over an asynchronous source.
//!
//! Large media is signed in prepared-hash mode: Encypher's planner decides
//! which structure bytes a manifest binds and how the carrier is built, and
//! the edge never holds the whole file. The planner's format logic ships only
//! inside Encypher's signed probe release, a WebAssembly artifact the signing
//! service advertises and serves. This module is the generic host for that
//! artifact. It holds no format, planner or hashing algorithm of its own: it
//! verifies a release, runs the operations the release implements (`extract`,
//! `verify_plan`, `build_carrier`, `binding_items`, `check_store` and the
//! rest), and returns the JSON the release answers with.
//!
//! # Release trust
//!
//! A release is run only after every check the advertised release contract
//! names, in order: the descriptor's Ed25519 signature (`verify_strict` over
//! its canonical bytes) by a key this build pins, a release that is not
//! revoked, ABI 1, a release at or above the floor, an artifact of at most
//! 4 MiB whose SHA-256 is the descriptor's, a module that imports exactly
//! `env.host_read` and exports the probe ABI, a `probe_abi` of 1, and a
//! `version` that reports the descriptor's `planner_id`. The caller supplies
//! the artifact bytes, fetched through the runtime's own HTTP client and
//! caches from [`ProbeDescriptor::artifact_path`]; nothing here touches the
//! network.
//!
//! # Asynchronous reads
//!
//! The release's only way to see bytes is its one import,
//! `host_read(offset, len, dst) -> status`, which the release calls
//! synchronously. On the edge a read is an HTTP range request, so it cannot
//! complete inside the interpreter's callback without blocking the executor.
//! The callback therefore checks the request against the source size and the
//! per-read bound and, when it is admissible, stops the interpreter with a
//! resumable host trap. [`ProbeModule::call`] then awaits the source outside
//! the interpreter, writing straight into the release's own read buffer in
//! linear memory, and resumes the suspended call with the status. The
//! operation is never restarted, so each read happens once, in order, and no
//! host-side copy of the file exists.
//!
//! Each call runs in a fresh instance with its own fuel, memory, per-read,
//! total-read and result bounds, so an abandoned call leaves nothing behind
//! and one [`ProbeModule`] serves concurrent calls.

use std::convert::Infallible;
use std::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use ed25519_dalek::{Signature, VerifyingKey};
use serde_json::{Value, json};
use wasmi::{
    Caller, Config, Engine, ExternType, Linker, Memory, Module, Store, StoreLimits,
    StoreLimitsBuilder, TrapCode, TypedFunc, TypedResumableCall, Val, ValType,
};

use crate::media_api::sha256_hex;

/// The probe host ABI this host implements.
pub(crate) const PROBE_ABI: u64 = 1;

/// Encypher's production probe release key: a raw Ed25519 public key, lower
/// case hex `9ae0bf4a...6b75d9`. Pinning it means a release Encypher did not
/// sign is refused even when the API or a cache serving the artifact is
/// compromised.
const PRODUCTION_RELEASE_KEY: [u8; 32] = [
    0x9a, 0xe0, 0xbf, 0x4a, 0xea, 0x51, 0x40, 0x00, 0x8d, 0x2f, 0x48, 0xe6, 0xc0, 0x86, 0x91, 0x36,
    0x0a, 0x7f, 0xe2, 0x3e, 0x0d, 0x36, 0xf2, 0x45, 0x22, 0xc7, 0x70, 0xb7, 0x64, 0x6b, 0x75, 0xd9,
];

/// The lowest probe release this build runs. Raise it when Encypher
/// withdraws a release, so a compromised API cannot downgrade the edge to it.
const PRODUCTION_RELEASE_FLOOR: u64 = 8;

/// A release artifact is about 800 KB; anything over this is not one.
const MAX_ARTIFACT_BYTES: usize = 4 * 1024 * 1024;

/// `host_read` succeeded and the destination holds the requested bytes.
const READ_OK: i32 = 0;
/// `host_read` named a destination outside the release's linear memory.
const READ_MEMORY_OUT_OF_BOUNDS: i32 = 4;
/// `host_read` asked for more than one read may carry or for bytes past the
/// end of the source.
const READ_OUT_OF_BOUNDS: i32 = 5;

/// A signed release descriptor, as the capabilities response and a 409
/// refusal carry it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProbeDescriptor {
    pub(crate) abi: u64,
    pub(crate) planner_id: String,
    pub(crate) release: u64,
    pub(crate) sha256: String,
    pub(crate) signature: String,
}

impl ProbeDescriptor {
    /// Reads one descriptor, or `None` when its shape is not the contract's:
    /// unsigned integers `abi` and `release`, lower case 64-hex `planner_id`
    /// and `sha256`, and a non-empty `signature`.
    #[must_use]
    pub(crate) fn parse(value: &Value) -> Option<Self> {
        let hex64 = |text: &&str| {
            text.len() == 64
                && text
                    .bytes()
                    .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        };
        Some(Self {
            abi: value.get("abi")?.as_u64()?,
            planner_id: value.get("planner_id")?.as_str().filter(hex64)?.to_owned(),
            release: value.get("release")?.as_u64()?,
            sha256: value.get("sha256")?.as_str().filter(hex64)?.to_owned(),
            signature: value
                .get("signature")?
                .as_str()
                .filter(|text| !text.is_empty())?
                .to_owned(),
        })
    }

    /// Every well-formed descriptor in a `probe` array; malformed entries
    /// are dropped, and anything other than an array is none.
    #[must_use]
    pub(crate) fn parse_list(value: Option<&Value>) -> Vec<Self> {
        value
            .and_then(Value::as_array)
            .map(|items| items.iter().filter_map(Self::parse).collect())
            .unwrap_or_default()
    }

    #[cfg(test)]
    /// The descriptor as the signing API receives it back.
    #[must_use]
    pub(crate) fn to_json(&self) -> Value {
        json!({
            "abi": self.abi,
            "planner_id": self.planner_id,
            "release": self.release,
            "sha256": self.sha256,
            "signature": self.signature,
        })
    }

    /// The exact bytes the release key signs: the four signed fields, keys
    /// sorted, no whitespace.
    #[must_use]
    pub(crate) fn canonical_json(&self) -> String {
        format!(
            "{{\"abi\":{},\"planner_id\":\"{}\",\"release\":{},\"sha256\":\"{}\"}}",
            self.abi, self.planner_id, self.release, self.sha256
        )
    }

    /// The signing API path that serves this release's artifact, relative to
    /// the API base.
    #[must_use]
    pub(crate) fn artifact_path(&self) -> String {
        format!("sign/media/probe/{}.wasm", self.sha256)
    }
}

/// The release keys, floor and revocations a release must satisfy.
///
/// Production code obtains only [`ReleaseAuthority::production`]: the keys
/// are compiled in and nothing a configuration or a request carries can
/// change them.
#[derive(Debug, Clone)]
pub(crate) struct ReleaseAuthority {
    keys: Vec<VerifyingKey>,
    floor: u64,
    /// Lower case artifact SHA-256s withdrawn for cause.
    revoked: Vec<String>,
}

impl ReleaseAuthority {
    /// Encypher's production release key and this build's floor.
    ///
    /// # Panics
    ///
    /// Never: the pinned key is a valid Ed25519 point, which a test proves.
    #[must_use]
    pub(crate) fn production() -> Self {
        let key = VerifyingKey::from_bytes(&PRODUCTION_RELEASE_KEY)
            .expect("should decode the pinned production release key");
        Self {
            keys: vec![key],
            floor: PRODUCTION_RELEASE_FLOOR,
            revoked: Vec::new(),
        }
    }

    /// An authority over test keys.
    #[cfg(test)]
    pub(crate) fn pinned(keys: Vec<VerifyingKey>, floor: u64, revoked: Vec<String>) -> Self {
        Self {
            keys,
            floor,
            revoked,
        }
    }

    /// Checks a descriptor before any of its release's bytes are fetched:
    /// revocation, signature, ABI and floor.
    ///
    /// # Errors
    ///
    /// The [`ReleaseError`] naming the first check the descriptor fails.
    pub(crate) fn verify(&self, descriptor: &ProbeDescriptor) -> Result<(), ReleaseError> {
        if self
            .revoked
            .iter()
            .any(|sha256| sha256.eq_ignore_ascii_case(&descriptor.sha256))
        {
            return Err(ReleaseError::Revoked {
                release: descriptor.release,
            });
        }
        let signature = STANDARD
            .decode(&descriptor.signature)
            .ok()
            .and_then(|bytes| Signature::from_slice(&bytes).ok())
            .ok_or(ReleaseError::MalformedSignature {
                release: descriptor.release,
            })?;
        let message = descriptor.canonical_json();
        let signed = self
            .keys
            .iter()
            .any(|key| key.verify_strict(message.as_bytes(), &signature).is_ok());
        if !signed {
            return Err(ReleaseError::Unsigned {
                release: descriptor.release,
            });
        }
        if descriptor.abi != PROBE_ABI {
            return Err(ReleaseError::AbiUnsupported {
                abi: descriptor.abi,
            });
        }
        if descriptor.release < self.floor {
            return Err(ReleaseError::BelowFloor {
                release: descriptor.release,
                floor: self.floor,
            });
        }
        Ok(())
    }

    /// The newest advertised release this host implements the ABI of,
    /// verified.
    ///
    /// # Errors
    ///
    /// [`ReleaseError::NoneAdvertised`] for an empty list,
    /// [`ReleaseError::NoCompatibleRelease`] when none uses this ABI, and the
    /// chosen descriptor's verification failure otherwise. An older release
    /// is never chosen in place of a newer one that fails.
    pub(crate) fn select<'a>(
        &self,
        descriptors: &'a [ProbeDescriptor],
    ) -> Result<&'a ProbeDescriptor, ReleaseError> {
        if descriptors.is_empty() {
            return Err(ReleaseError::NoneAdvertised);
        }
        let descriptor = descriptors
            .iter()
            .filter(|descriptor| descriptor.abi == PROBE_ABI)
            .max_by_key(|descriptor| descriptor.release)
            .ok_or(ReleaseError::NoCompatibleRelease)?;
        self.verify(descriptor)?;
        Ok(descriptor)
    }
}

/// Why a release was not run. Every variant is refused before the release
/// reads a byte of any source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReleaseError {
    /// The service advertised no release.
    NoneAdvertised,
    /// The service advertised no release for [`PROBE_ABI`].
    NoCompatibleRelease,
    /// The release was withdrawn for cause.
    Revoked { release: u64 },
    /// The signature is not 64 bytes of standard padded base64.
    MalformedSignature { release: u64 },
    /// No pinned key signed the descriptor.
    Unsigned { release: u64 },
    /// The descriptor names an ABI this host does not implement.
    AbiUnsupported { abi: u64 },
    /// The release is older than this build's floor.
    BelowFloor { release: u64, floor: u64 },
    /// The artifact is larger than any release.
    ArtifactTooLarge { bytes: usize },
    /// The artifact does not hash to the signed digest.
    DigestMismatch,
    /// The artifact is not a valid WebAssembly module.
    InvalidModule(String),
    /// The module imports anything other than exactly `env.host_read`.
    ImportsMismatch,
    /// The module does not export the probe ABI, or its start or
    /// `probe_abi` trapped.
    ExportsMismatch(String),
    /// `probe_abi` reports an ABI other than the descriptor's.
    AbiReported { reported: u32 },
    /// `version` failed or did not report the descriptor's `planner_id`.
    PlannerMismatch,
}

impl ReleaseError {
    /// The client contract's code for this refusal.
    #[must_use]
    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::NoneAdvertised => "PROBE_RELEASE_UNAVAILABLE",
            Self::NoCompatibleRelease | Self::AbiUnsupported { .. } => "PROBE_UPGRADE_REQUIRED",
            _ => "PROBE_RELEASE_INVALID",
        }
    }
}

impl fmt::Display for ReleaseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoneAdvertised => {
                formatter.write_str("the signing service advertised no probe release")
            }
            Self::NoCompatibleRelease => write!(
                formatter,
                "the signing service advertised no probe release for ABI {PROBE_ABI}"
            ),
            Self::Revoked { release } => {
                write!(formatter, "probe release {release} was withdrawn")
            }
            Self::MalformedSignature { release } => {
                write!(
                    formatter,
                    "probe release {release} carries a malformed signature"
                )
            }
            Self::Unsigned { release } => write!(
                formatter,
                "probe release {release} is not signed by a pinned release key"
            ),
            Self::AbiUnsupported { abi } => write!(
                formatter,
                "the probe release uses ABI {abi}; this host implements ABI {PROBE_ABI}"
            ),
            Self::BelowFloor { release, floor } => write!(
                formatter,
                "probe release {release} is below this build's floor of {floor}"
            ),
            Self::ArtifactTooLarge { bytes } => write!(
                formatter,
                "the probe artifact is {bytes} bytes, over the {MAX_ARTIFACT_BYTES} byte limit"
            ),
            Self::DigestMismatch => {
                formatter.write_str("the probe artifact does not hash to its signed digest")
            }
            Self::InvalidModule(error) => write!(
                formatter,
                "the probe artifact is not a valid WebAssembly module: {error}"
            ),
            Self::ImportsMismatch => {
                formatter.write_str("the probe release imports something other than env.host_read")
            }
            Self::ExportsMismatch(error) => write!(
                formatter,
                "the probe release does not implement the host ABI: {error}"
            ),
            Self::AbiReported { reported } => write!(
                formatter,
                "the probe release reports ABI {reported}, not its descriptor's"
            ),
            Self::PlannerMismatch => {
                formatter.write_str("the probe release does not report its descriptor's planner")
            }
        }
    }
}

impl std::error::Error for ReleaseError {}

/// Bounds on one call.
///
/// Fuel, the total read and the result abandon the call when exceeded. A
/// read over the per-read bound is answered with status 5, and growth past
/// the memory bound fails inside the release, which then answers as it
/// handles an allocation failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProbeLimits {
    /// Linear memory one instance may grow to.
    pub(crate) memory_bytes: usize,
    /// Instructions one call may execute.
    pub(crate) fuel_per_call: u64,
    /// The largest single read; a larger request is answered with status 5.
    pub(crate) max_read_bytes: u32,
    /// Bytes all reads of one call may carry together.
    pub(crate) max_total_read_bytes: u64,
    /// The largest result a call may return.
    pub(crate) max_result_bytes: u32,
}

impl Default for ProbeLimits {
    /// Leave room for the host, parsed JSON and the signed module on
    /// 128 MiB-class runtimes. Media payload hashing stays outside WASM.
    /// Larger structure or result disclosures are refused explicitly.
    fn default() -> Self {
        Self {
            memory_bytes: 32 * 1024 * 1024,
            fuel_per_call: 200_000_000,
            max_read_bytes: 8 * 1024 * 1024,
            max_total_read_bytes: 64 * 1024 * 1024,
            max_result_bytes: 4 * 1024 * 1024,
        }
    }
}

/// One version of one file, read at offsets.
///
/// An implementation MUST serve every read of a call from the same version
/// (an HTTP source pins a strong `ETag` and refuses a range answered from
/// any other) and MUST fail rather than return fewer bytes than asked for.
pub(crate) trait ProbeSource {
    /// Why a read failed. The call is abandoned and the error returned as
    /// [`ProbeCallError::Source`].
    type Error;

    /// The size of the pinned version, in bytes. The host refuses any read
    /// past it before the source is asked.
    fn size(&self) -> u64;

    /// Fills `destination` with the bytes at `offset`.
    fn read_exact_at(
        &self,
        offset: u64,
        destination: &mut [u8],
    ) -> impl Future<Output = Result<(), Self::Error>>;
}

/// The source of an operation that reads no file, such as `version`,
/// `build_carrier` or `check_store`. It has no bytes, so the host refuses
/// every non-empty read with status 5.
struct NoSource;

impl ProbeSource for NoSource {
    type Error = Infallible;

    fn size(&self) -> u64 {
        0
    }

    async fn read_exact_at(&self, _offset: u64, _destination: &mut [u8]) -> Result<(), Infallible> {
        Ok(())
    }
}

/// The release's own error object, returned with status 1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProbeRefusal {
    /// The probe's code, such as `PLAN_MISMATCH` or `UNSUPPORTED_FORMAT`.
    pub(crate) code: String,
    /// Whether the refusal reports an integrity failure.
    pub(crate) integrity: bool,
    pub(crate) message: String,
}

/// Why a call ended without the operation's value.
#[derive(Debug)]
pub(crate) enum ProbeCallError<E> {
    /// The source failed a read. The suspended call was dropped.
    Source(E),
    /// The release answered with its error object.
    Refused(ProbeRefusal),
    /// The host stopped the call, or the release broke the ABI.
    Failed(ProbeFailure),
}

impl<E> ProbeCallError<E> {
    /// The client contract's code: the release's own code for a refusal,
    /// `PROBE_FAILED` otherwise.
    #[must_use]
    pub(crate) fn code(&self) -> &str {
        match self {
            Self::Refused(refusal) => &refusal.code,
            Self::Source(_) | Self::Failed(_) => "PROBE_FAILED",
        }
    }
}

impl<E: fmt::Display> fmt::Display for ProbeCallError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Source(error) => write!(formatter, "the probe's source failed a read: {error}"),
            Self::Refused(refusal) => {
                write!(
                    formatter,
                    "the probe refused: {}: {}",
                    refusal.code, refusal.message
                )
            }
            Self::Failed(failure) => failure.fmt(formatter),
        }
    }
}

impl<E: fmt::Debug + fmt::Display> std::error::Error for ProbeCallError<E> {}

impl ProbeCallError<Infallible> {
    /// The same error over any source error type, for a call that reads no
    /// source.
    fn widen<E>(self) -> ProbeCallError<E> {
        match self {
            Self::Source(never) => match never {},
            Self::Refused(refusal) => ProbeCallError::Refused(refusal),
            Self::Failed(failure) => ProbeCallError::Failed(failure),
        }
    }
}

/// How the host stopped a call, or how the release broke the ABI during it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProbeFailure {
    /// The instance could not be created or lacked an export.
    Instantiate(String),
    /// The operation name or argument does not fit a 32-bit length.
    ArgumentTooLarge,
    /// The release trapped, or a host call failed outside `host_read`.
    Trap(String),
    /// The call used its fuel.
    FuelExhausted,
    /// The call's reads together exceeded the total read bound.
    ReadBudgetExceeded { budget: u64 },
    /// `probe_call` returned a status other than 0 or 1.
    UnknownStatus(i32),
    /// The result is larger than the result bound.
    ResultTooLarge { bytes: u32, limit: u32 },
    /// The result lies outside linear memory.
    ResultOutOfBounds,
    /// The result is not JSON, or a status 1 result is not an error object.
    MalformedResult(String),
}

impl ProbeFailure {
    /// An interpreter error: fuel exhaustion, or any other trap.
    fn stopped(error: &wasmi::Error) -> Self {
        if error.as_trap_code() == Some(TrapCode::OutOfFuel) {
            Self::FuelExhausted
        } else {
            Self::Trap(error.to_string())
        }
    }
}

impl fmt::Display for ProbeFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Instantiate(error) => {
                write!(
                    formatter,
                    "the probe release could not be instantiated: {error}"
                )
            }
            Self::ArgumentTooLarge => {
                formatter.write_str("a probe argument exceeds a 32-bit length")
            }
            Self::Trap(error) => write!(
                formatter,
                "the probe release stopped with an error: {error}"
            ),
            Self::FuelExhausted => formatter.write_str("the probe release ran out of fuel"),
            Self::ReadBudgetExceeded { budget } => write!(
                formatter,
                "the probe release read more than its {budget} byte budget"
            ),
            Self::UnknownStatus(status) => {
                write!(
                    formatter,
                    "the probe release returned unknown status {status}"
                )
            }
            Self::ResultTooLarge { bytes, limit } => write!(
                formatter,
                "the probe release returned {bytes} bytes, over the {limit} byte limit"
            ),
            Self::ResultOutOfBounds => {
                formatter.write_str("the probe release's result lies outside its memory")
            }
            Self::MalformedResult(error) => {
                write!(
                    formatter,
                    "the probe release returned a malformed result: {error}"
                )
            }
        }
    }
}

/// A verified, compiled release.
pub(crate) struct ProbeModule {
    module: Module,
    linker: Linker<HostState>,
    descriptor: ProbeDescriptor,
    limits: ProbeLimits,
}

impl fmt::Debug for ProbeModule {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProbeModule")
            .field("descriptor", &self.descriptor)
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

impl ProbeModule {
    /// Verifies `artifact` as the release `descriptor` names and compiles it.
    ///
    /// The descriptor is checked against `authority` first, so an unsigned
    /// artifact is never parsed. The release's `version` runs once, with no
    /// source, to confirm its planner.
    ///
    /// # Errors
    ///
    /// The [`ReleaseError`] naming the first check that failed.
    pub(crate) async fn load(
        authority: &ReleaseAuthority,
        descriptor: &ProbeDescriptor,
        artifact: &[u8],
        limits: ProbeLimits,
    ) -> Result<Self, ReleaseError> {
        authority.verify(descriptor)?;
        if artifact.len() > MAX_ARTIFACT_BYTES {
            return Err(ReleaseError::ArtifactTooLarge {
                bytes: artifact.len(),
            });
        }
        if sha256_hex(artifact) != descriptor.sha256 {
            return Err(ReleaseError::DigestMismatch);
        }
        let mut config = Config::default();
        config.consume_fuel(true);
        let engine = Engine::new(&config);
        let module = Module::new(&engine, artifact)
            .map_err(|error| ReleaseError::InvalidModule(error.to_string()))?;
        if !imports_only_host_read(&module) {
            return Err(ReleaseError::ImportsMismatch);
        }
        let mut linker = Linker::<HostState>::new(&engine);
        linker
            .func_wrap("env", "host_read", host_read)
            .map_err(|error| ReleaseError::ExportsMismatch(error.to_string()))?;
        let probe = Self {
            module,
            linker,
            descriptor: descriptor.clone(),
            limits,
        };

        let mut instance = probe
            .instantiate(0)
            .map_err(|error| ReleaseError::ExportsMismatch(error.to_string()))?;
        let reported = instance
            .abi
            .call(&mut instance.store, ())
            .map_err(|error| ReleaseError::ExportsMismatch(error.to_string()))?;
        if u64::from(reported) != descriptor.abi {
            return Err(ReleaseError::AbiReported { reported });
        }
        let version = probe
            .call_without_source::<Infallible>("version", &json!({}))
            .await
            .map_err(|_| ReleaseError::PlannerMismatch)?;
        if version.get("planner_id").and_then(Value::as_str) != Some(descriptor.planner_id.as_str())
        {
            return Err(ReleaseError::PlannerMismatch);
        }
        Ok(probe)
    }

    /// The verified descriptor of this release.
    #[must_use]
    pub(crate) fn descriptor(&self) -> &ProbeDescriptor {
        &self.descriptor
    }

    /// Runs `op` with `arg` over `source` and returns the release's value.
    ///
    /// The operation name is passed through, so every operation the release
    /// implements is available. Each read the release makes is awaited from
    /// `source` outside the interpreter and the suspended call resumed.
    ///
    /// # Errors
    ///
    /// [`ProbeCallError::Source`] when a read fails,
    /// [`ProbeCallError::Refused`] when the release answers with its error
    /// object, and [`ProbeCallError::Failed`] when a bound stops the call or
    /// the release breaks the ABI.
    pub(crate) async fn call<S: ProbeSource>(
        &self,
        op: &str,
        arg: &Value,
        source: &S,
    ) -> Result<Value, ProbeCallError<S::Error>> {
        let mut instance = self.instantiate(source.size()).map_err(|error| {
            ProbeCallError::Failed(ProbeFailure::Instantiate(error.to_string()))
        })?;
        let (op_pointer, op_length) = instance
            .put(op.as_bytes())
            .map_err(ProbeCallError::Failed)?;
        let (arg_pointer, arg_length) = instance
            .put(arg.to_string().as_bytes())
            .map_err(ProbeCallError::Failed)?;

        let mut total_read = 0_u64;
        let mut state = instance
            .call
            .call_resumable(
                &mut instance.store,
                (op_pointer, op_length, arg_pointer, arg_length),
            )
            .map_err(stopped)?;
        let status = loop {
            match state {
                TypedResumableCall::Finished(status) => break status,
                TypedResumableCall::OutOfFuel(_) => {
                    return Err(ProbeCallError::Failed(ProbeFailure::FuelExhausted));
                }
                TypedResumableCall::HostTrap(suspended) => {
                    // `host_read` is the only import, so its request is the
                    // only host error a call can stop with.
                    let Some(request) = suspended
                        .host_error()
                        .downcast_ref::<ReadRequest>()
                        .copied()
                    else {
                        return Err(ProbeCallError::Failed(ProbeFailure::Trap(
                            suspended.host_error().to_string(),
                        )));
                    };
                    let budget = self.limits.max_total_read_bytes;
                    total_read = total_read
                        .checked_add(u64::from(request.length))
                        .filter(|total| *total <= budget)
                        .ok_or(ProbeCallError::Failed(ProbeFailure::ReadBudgetExceeded {
                            budget,
                        }))?;
                    let status = instance.serve(request, source).await?;
                    state = suspended
                        .resume(&mut instance.store, &[Val::I32(status)])
                        .map_err(stopped)?;
                }
            }
        };
        let result = instance
            .result(self.limits.max_result_bytes)
            .map_err(ProbeCallError::Failed)?;
        let value: Value = serde_json::from_slice(&result).map_err(|error| {
            ProbeCallError::Failed(ProbeFailure::MalformedResult(error.to_string()))
        })?;
        match status {
            0 => Ok(value),
            1 => Err(ProbeCallError::Refused(
                refusal(&value).map_err(ProbeCallError::Failed)?,
            )),
            other => Err(ProbeCallError::Failed(ProbeFailure::UnknownStatus(other))),
        }
    }

    /// Runs `op` with `arg` and no source, for operations that read no file.
    ///
    /// # Errors
    ///
    /// As [`ProbeModule::call`], without a source error.
    pub(crate) async fn call_without_source<E>(
        &self,
        op: &str,
        arg: &Value,
    ) -> Result<Value, ProbeCallError<E>> {
        self.call(op, arg, &NoSource)
            .await
            .map_err(ProbeCallError::widen)
    }

    /// A fresh instance whose single import reads a source of `source_size`
    /// bytes.
    fn instantiate(&self, source_size: u64) -> Result<Instance, wasmi::Error> {
        let mut store = Store::new(
            self.module.engine(),
            HostState {
                limits: StoreLimitsBuilder::new()
                    .memory_size(self.limits.memory_bytes)
                    .instances(1)
                    .memories(1)
                    .tables(4)
                    .build(),
                source_size,
                max_read_bytes: self.limits.max_read_bytes,
            },
        );
        store.limiter(|state| &mut state.limits);
        store.set_fuel(self.limits.fuel_per_call)?;
        let instance = self
            .linker
            .instantiate_and_start(&mut store, &self.module)?;
        let memory = instance
            .get_memory(&store, "memory")
            .ok_or_else(|| wasmi::Error::new("the release exports no memory"))?;
        let _free: TypedFunc<(u32, u32), ()> = instance.get_typed_func(&store, "probe_free")?;
        Ok(Instance {
            abi: instance.get_typed_func(&store, "probe_abi")?,
            alloc: instance.get_typed_func(&store, "probe_alloc")?,
            call: instance.get_typed_func(&store, "probe_call")?,
            result_ptr: instance.get_typed_func(&store, "probe_result_ptr")?,
            result_len: instance.get_typed_func(&store, "probe_result_len")?,
            memory,
            store,
        })
    }
}

/// Whether the module's only import is `env.host_read(i64, i32, i32) ->
/// i32`.
fn imports_only_host_read(module: &Module) -> bool {
    let mut imports = module.imports();
    let Some(import) = imports.next() else {
        return false;
    };
    let host_read = import.module() == "env"
        && import.name() == "host_read"
        && matches!(
            import.ty(),
            ExternType::Func(ty)
                if ty.params() == [ValType::I64, ValType::I32, ValType::I32]
                    && ty.results() == [ValType::I32]
        );
    host_read && imports.next().is_none()
}

/// The release's error object, from a status 1 result.
fn refusal(value: &Value) -> Result<ProbeRefusal, ProbeFailure> {
    let error = value
        .get("error")
        .ok_or_else(|| ProbeFailure::MalformedResult("a status 1 result has no error".into()))?;
    let text = |field: &str| {
        error
            .get(field)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| ProbeFailure::MalformedResult(format!("the error has no {field}")))
    };
    Ok(ProbeRefusal {
        code: text("code")?,
        integrity: error
            .get("integrity")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        message: text("message")?,
    })
}

/// A call stopped by the interpreter: out of fuel, or any other trap.
#[allow(
    clippy::needless_pass_by_value,
    reason = "used directly as a Result error mapper"
)]
fn stopped<E>(error: wasmi::Error) -> ProbeCallError<E> {
    ProbeCallError::Failed(ProbeFailure::stopped(&error))
}

/// What the store holds for the single import.
struct HostState {
    limits: StoreLimits,
    source_size: u64,
    max_read_bytes: u32,
}

/// An admissible read, carried out of the interpreter as a resumable host
/// trap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ReadRequest {
    offset: u64,
    length: u32,
    destination: u32,
}

impl fmt::Display for ReadRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "read of {} bytes at offset {} into {}",
            self.length, self.offset, self.destination
        )
    }
}

impl wasmi::errors::HostError for ReadRequest {}

/// `env.host_read`. Refuses a read over the per-read bound or past the
/// source with status 5, and suspends the call for any other.
#[allow(
    clippy::needless_pass_by_value,
    reason = "wasmi's host import ABI owns its Caller"
)]
fn host_read(
    caller: Caller<'_, HostState>,
    offset: u64,
    length: u32,
    destination: u32,
) -> Result<i32, wasmi::Error> {
    let state = caller.data();
    let in_bounds = length <= state.max_read_bytes
        && offset
            .checked_add(u64::from(length))
            .is_some_and(|end| end <= state.source_size);
    if !in_bounds {
        return Ok(READ_OUT_OF_BOUNDS);
    }
    Err(wasmi::Error::host(ReadRequest {
        offset,
        length,
        destination,
    }))
}

/// One instance, for one call.
struct Instance {
    store: Store<HostState>,
    memory: Memory,
    abi: TypedFunc<(), u32>,
    alloc: TypedFunc<u32, u32>,
    call: TypedFunc<(u32, u32, u32, u32), i32>,
    result_ptr: TypedFunc<(), u32>,
    result_len: TypedFunc<(), u32>,
}

impl Instance {
    /// Copies `bytes` into a buffer the release allocates.
    fn put(&mut self, bytes: &[u8]) -> Result<(u32, u32), ProbeFailure> {
        let length = u32::try_from(bytes.len()).map_err(|_| ProbeFailure::ArgumentTooLarge)?;
        let pointer = self
            .alloc
            .call(&mut self.store, length)
            .map_err(|error| ProbeFailure::stopped(&error))?;
        self.memory
            .write(&mut self.store, pointer as usize, bytes)
            .map_err(|error| ProbeFailure::Trap(error.to_string()))?;
        Ok((pointer, length))
    }

    /// Awaits one read from `source` straight into the release's buffer and
    /// returns the status to resume with.
    async fn serve<S: ProbeSource>(
        &mut self,
        request: ReadRequest,
        source: &S,
    ) -> Result<i32, ProbeCallError<S::Error>> {
        let start = request.destination as usize;
        let memory = self.memory.data_mut(&mut self.store);
        let destination = match start.checked_add(request.length as usize) {
            Some(end) => memory.get_mut(start..end),
            None => None,
        };
        let Some(destination) = destination else {
            return Ok(READ_MEMORY_OUT_OF_BOUNDS);
        };
        source
            .read_exact_at(request.offset, destination)
            .await
            .map_err(ProbeCallError::Source)?;
        Ok(READ_OK)
    }

    /// The finished call's result bytes, bounded.
    fn result(&mut self, limit: u32) -> Result<Vec<u8>, ProbeFailure> {
        let trap = |error: wasmi::Error| ProbeFailure::stopped(&error);
        let pointer = self.result_ptr.call(&mut self.store, ()).map_err(trap)?;
        let length = self.result_len.call(&mut self.store, ()).map_err(trap)?;
        if length > limit {
            return Err(ProbeFailure::ResultTooLarge {
                bytes: length,
                limit,
            });
        }
        let mut bytes = vec![0_u8; length as usize];
        self.memory
            .read(&self.store, pointer as usize, &mut bytes)
            .map_err(|_| ProbeFailure::ResultOutOfBounds)?;
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests;
