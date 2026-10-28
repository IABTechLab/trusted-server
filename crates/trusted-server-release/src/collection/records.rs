//! Strict imported diagnostics. None of these structs is a local execution proof.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path};

use error_stack::Report;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use self::receipt::{ReceiptBinding, check_receipt};
use super::{Transfer, ValidatedTransfers, collection_error, digest};
use crate::documents::{Role, lower_hex, parse_json};
use crate::error::ReleaseError;
use crate::inspect::inspect_wasm;
use crate::policy::FIXED_ARTIFACTS;

mod receipt;

/// A bounded native reproduction measurement.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Outcome {
    Matched,
    Mismatched,
    Failed,
    Unavailable,
}

/// The selected profile belongs to the trusted dependency capability.
#[derive(Clone, Debug)]
pub(super) struct RoleProfile {
    umask: u32,
    jobs: u32,
    host_locale: String,
    host_timezone: String,
}

impl RoleProfile {
    #[cfg(test)]
    pub(super) fn fixtures() -> [Self; 2] {
        [
            Self {
                umask: 0o022,
                jobs: 4,
                host_locale: "C".to_owned(),
                host_timezone: "UTC".to_owned(),
            },
            Self {
                umask: 0o027,
                jobs: 2,
                host_locale: "en_US.UTF-8".to_owned(),
                host_timezone: "Pacific/Auckland".to_owned(),
            },
        ]
    }

    /// Selects an arbitrary fictional profile so pair checks can be isolated.
    #[cfg(test)]
    pub(super) fn for_tests(umask: u32, jobs: u32, host_locale: &str, host_timezone: &str) -> Self {
        Self {
            umask,
            jobs,
            host_locale: host_locale.to_owned(),
            host_timezone: host_timezone.to_owned(),
        }
    }
}

/// Candidate data only; no field authorizes signing or deployment.
#[derive(Debug)]
pub(crate) struct CollectionDiagnostic {
    /// Exact complete canonical artifact bytes.
    pub(crate) canonical: BTreeMap<String, Vec<u8>>,
    /// Native B outcomes, including honest no-upload outcomes.
    pub(crate) native: BTreeMap<String, Outcome>,
    /// Unqualified rebuild details for future evidence assembly.
    pub(crate) rebuild: BTreeMap<String, Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BuildRecord {
    schema_version: u64,
    source_commit: String,
    recipe_sha256: String,
    asset: String,
    role: String,
    job: String,
    attempt: u64,
    inputs: BTreeMap<String, String>,
    inventory_sha256: String,
    artifact_sha256: String,
    artifact_size: u64,
    composition_sha256: String,
    ambient: Ambient,
    claim: Claim,
    boundary: Boundary,
    runtime: Option<Runtime>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Ambient {
    umask: u32,
    jobs: u32,
    checkout_root: String,
    started_at_seconds: u64,
    execution_uid: u32,
    host_locale: String,
    host_timezone: String,
    child_locale: String,
    child_timezone: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Claim {
    varied: Vec<String>,
    normalized: Vec<String>,
    unsupported: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Boundary {
    /// Boundary instance the measured socket denial receipt names.
    fingerprint: String,
    /// Boundary instance that executed the build; instance identity only,
    /// including a fresh per-instance nonce, not the invocation content.
    execution_fingerprint: String,
    /// SHA-256 of the exact build invocation: program, arguments including
    /// jobs, environment, working directory (the checkout root) and umask.
    invocation_digest: String,
    confined: bool,
    control_exit: i32,
    control_accepts: u32,
    probe_exit: i32,
    probe_errno: i32,
    probe_accepts: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Runtime {
    /// The build role the producer claims for the Viceroy run that accepted
    /// these final bytes; self-asserted, not an independent run identity.
    role: String,
    source_commit: String,
    final_wasm_sha256: String,
    final_wasm_size: u64,
    publisher_status: u16,
    injected_scripts: u32,
    origin_marker_matched: bool,
    viceroy_version: String,
}

type Result<T> = std::result::Result<T, Report<ReleaseError>>;

/// Build record facts a sealed composition receipt must restate.
pub(crate) struct ReceiptFacts<'a> {
    /// Exact final artifact digest.
    pub(crate) artifact_sha256: &'a str,
    /// Approved source commit.
    pub(crate) source_commit: &'a str,
    /// Shared original JS inventory digest.
    pub(crate) inventory_sha256: &'a str,
    /// Prepared Cargo materials tree digest (`inputs.materials`).
    pub(crate) materials_sha256: &'a str,
    /// Fixed asset name; selects the target and the Fastly JS rule.
    pub(crate) asset: &'a str,
}

/// Checks a sealed composition receipt exactly as the strict collector does.
///
/// The collector calls this after binding the receipt bytes to the record's
/// `composition_sha256`; the real candidate tests call it on actual producer
/// exports so the collector schema cannot drift from the producer's.
///
/// # Errors
///
/// Rejects an unknown asset and every receipt rejection of the collector.
pub(crate) fn check_composition_receipt(bytes: &[u8], facts: &ReceiptFacts<'_>) -> Result<()> {
    let fixed = FIXED_ARTIFACTS
        .iter()
        .find(|asset| asset.name == facts.asset)
        .ok_or_else(|| collection_error("unknown cohort artifact"))?;
    check_receipt(
        bytes,
        &ReceiptBinding {
            artifact_sha256: facts.artifact_sha256,
            source_commit: facts.source_commit,
            inventory_sha256: facts.inventory_sha256,
            materials_sha256: facts.materials_sha256,
            target: fixed.target,
        },
    )
}

fn require(condition: bool, detail: &str) -> Result<()> {
    if !condition {
        return Err(collection_error(detail));
    }
    Ok(())
}
fn parse<T: DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    let value = parse_json(bytes, Role::Metadata)
        .map_err(|error| error.change_context(ReleaseError::Collection))?;
    serde_json::from_value(value).map_err(collection_error)
}
fn member<'a>(transfer: &'a Transfer, name: &str) -> Result<&'a [u8]> {
    transfer
        .members
        .get(name)
        .map(Vec::as_slice)
        .ok_or_else(|| collection_error(format!("missing declared {name}")))
}
fn exact_names(values: &[String], expected: &[&str]) -> bool {
    values.len() == expected.len()
        && values.iter().map(String::as_str).collect::<BTreeSet<_>>()
            == expected.iter().copied().collect()
}
fn normal_path(value: &str) -> bool {
    let path = Path::new(value);
    path.is_absolute()
        && path != Path::new("/")
        && path
            .components()
            .all(|part| matches!(part, Component::RootDir | Component::Normal(_)))
        && path
            .components()
            .collect::<std::path::PathBuf>()
            .as_os_str()
            == path.as_os_str()
}

fn check_ambient(record: &BuildRecord, profile: &RoleProfile) -> Result<()> {
    let ambient = &record.ambient;
    require(
        ambient.umask == profile.umask
            && ambient.jobs == profile.jobs
            && ambient.host_locale == profile.host_locale
            && ambient.host_timezone == profile.host_timezone,
        "observed ambient differs from selected role profile",
    )?;
    require(
        ambient.child_locale == "C"
            && ambient.child_timezone == "UTC"
            && normal_path(&ambient.checkout_root),
        "child normalization or observed checkout root is invalid",
    )?;
    require(
        exact_names(
            &record.claim.varied,
            &["umask", "jobs", "checkout_root", "start_time"],
        ) && exact_names(&record.claim.normalized, &["locale", "timezone"])
            && exact_names(&record.claim.unsupported, &["execution_user"]),
        "dishonest or incomplete varied/normalized/unsupported claim",
    )
}

fn check_record(transfer: &Transfer, all: &ValidatedTransfers) -> Result<BuildRecord> {
    require(
        transfer.members.len() == 4,
        "unexpected build transfer member set",
    )?;
    let record: BuildRecord = parse(member(transfer, "build-record.json")?)?;
    let upload = &transfer.upload;
    require(
        super::bounded_upload(upload),
        "successful build has no bounded completion metadata",
    )?;
    let invocation = &all.invocation;
    require(
        record.schema_version == 1
            && record.source_commit == invocation.source_commit
            && record.recipe_sha256 == upload.recipe_sha256
            && record.asset == upload.asset
            && record.role == upload.role
            && record.job == upload.job
            && record.attempt == upload.attempt,
        "build record differs from trusted dependency identity",
    )?;
    let input_names = record
        .inputs
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    require(
        input_names
            == BTreeSet::from([
                "cargo_lock",
                "materials",
                "rust_tools",
                "js_settings",
                "js_tools",
            ])
            && record.inputs.values().all(|value| lower_hex(value, 64)),
        "missing or invalid build input digests",
    )?;
    let artifact = member(transfer, "artifact")?;
    require(
        record.artifact_size == artifact.len() as u64 && record.artifact_sha256 == digest(artifact),
        "final artifact digest or size changed",
    )?;
    let inventory = member(transfer, "tsjs-inventory.json")?;
    parse_json(inventory, Role::Metadata)
        .map_err(|error| error.change_context(ReleaseError::Collection))?;
    require(
        record.inventory_sha256 == invocation.inventory_sha256
            && digest(inventory) == invocation.inventory_sha256,
        "shared JS inventory changed",
    )?;
    let composition = member(transfer, "composition.json")?;
    require(
        digest(composition) == record.composition_sha256,
        "composition document digest changed",
    )?;
    let materials_sha256 = record
        .inputs
        .get("materials")
        .ok_or_else(|| collection_error("missing or invalid build input digests"))?;
    check_composition_receipt(
        composition,
        &ReceiptFacts {
            artifact_sha256: &record.artifact_sha256,
            source_commit: &record.source_commit,
            inventory_sha256: &record.inventory_sha256,
            materials_sha256,
            asset: &record.asset,
        },
    )?;
    let boundary = &record.boundary;
    require(
        lower_hex(&boundary.fingerprint, 64)
            && boundary.fingerprint == boundary.execution_fingerprint
            && lower_hex(&boundary.invocation_digest, 64)
            && boundary.confined
            && boundary.control_exit == 0
            && boundary.control_accepts == 1
            && boundary.probe_exit == 3
            && matches!(boundary.probe_errno, 1 | 13)
            && boundary.probe_accepts == 0,
        "missing/failed socket controls or unconfined build invocation",
    )?;
    let profile = match record.role.as_str() {
        "A" => &invocation.profiles[0],
        "B" => &invocation.profiles[1],
        _ => return Err(collection_error("unknown build role")),
    };
    check_ambient(&record, profile)?;
    if record.asset == FIXED_ARTIFACTS[0].name {
        let facts = inspect_wasm(artifact)
            .map_err(|error| error.change_context(ReleaseError::Collection))?;
        facts
            .require_fastly_entry_abi()
            .and_then(|()| facts.require_debug_sections())
            .map_err(|error| error.change_context(ReleaseError::Collection))?;
        let runtime = record
            .runtime
            .as_ref()
            .ok_or_else(|| collection_error("missing final-byte runtime proof diagnostic"))?;
        // A and B final bytes must be equal, so without the role one build's
        // runtime run could stand in for the other's. The role is asserted by
        // the producer inside its own record: it catches mix-ups and reuse in
        // an honest pipeline, not a malicious producer copying A's runtime
        // object and changing its role. Binding strength comes from the trusted
        // upload role and job isolation; a per-run Viceroy identity remains a
        // named gap.
        require(
            runtime.role == record.role
                && runtime.source_commit == record.source_commit
                && runtime.final_wasm_sha256 == record.artifact_sha256
                && runtime.final_wasm_size == record.artifact_size
                && runtime.publisher_status == 200
                && runtime.injected_scripts == 1
                && runtime.origin_marker_matched
                && runtime.viceroy_version == "0.21.0",
            "runtime diagnostic is not bound to accepted final Wasm bytes and publisher request",
        )?;
    } else {
        require(
            record.runtime.is_none(),
            "native transfer carries a Fastly runtime diagnostic",
        )?;
    }
    Ok(record)
}

fn compare_pair(a: &BuildRecord, b: &BuildRecord) -> Result<()> {
    require(
        a.inputs == b.inputs && a.recipe_sha256 == b.recipe_sha256,
        "A/B build inputs or recipe differ",
    )?;
    // The exact invocation digest covers role-specific paths, jobs, environment
    // and umask, so a reproduction reusing the canonical one did not vary them.
    require(
        a.boundary.invocation_digest != b.boundary.invocation_digest,
        "A/B share one exact invocation",
    )?;
    // Each build runs in its own fresh boundary instance. Instance fingerprints
    // carry a per-instance nonce, so this proves separate instances, not
    // varied invocation content.
    require(
        a.boundary.execution_fingerprint != b.boundary.execution_fingerprint,
        "A/B share one boundary instance",
    )?;
    let a = &a.ambient;
    let b = &b.ambient;
    // Normalized dimensions still vary on the host; only the compiler child
    // observes the fixed normalized values. The spec requires B to differ in
    // TZ and locale, so a qualified reproduction profile must select values
    // distinct from canonical; the claim shape cannot express `not-varied`
    // for them, and such a profile fails here by design.
    require(
        a.umask != b.umask
            && a.jobs != b.jobs
            && a.started_at_seconds != b.started_at_seconds
            && a.host_locale != b.host_locale
            && a.host_timezone != b.host_timezone,
        "supported A/B variation is missing",
    )?;
    let a_root = Path::new(&a.checkout_root);
    let b_root = Path::new(&b.checkout_root);
    require(
        !a_root.starts_with(b_root) && !b_root.starts_with(a_root),
        "A/B checkout roots overlap",
    )?;
    require(
        a.execution_uid == b.execution_uid,
        "unsupported execution user variation was claimed",
    )
}

pub(super) fn diagnose(all: &ValidatedTransfers) -> Result<CollectionDiagnostic> {
    let mut builds = BTreeMap::new();
    for transfer in &all.transfers {
        require(
            FIXED_ARTIFACTS
                .iter()
                .any(|asset| asset.name == transfer.upload.asset),
            "unknown cohort artifact",
        )?;
        let record = check_record(transfer, all)?;
        require(
            builds
                .insert(
                    (record.asset.clone(), record.role.clone()),
                    (record, transfer),
                )
                .is_none(),
            "duplicate asset/role record",
        )?;
    }
    let mut native = BTreeMap::new();
    for absent in &all.absent {
        let outcome = absent
            .outcome()
            .filter(|_| {
                FIXED_ARTIFACTS
                    .iter()
                    .skip(1)
                    .any(|fixed| fixed.name == absent.asset)
                    && !builds.contains_key(&(absent.asset.clone(), "B".to_owned()))
            })
            .ok_or_else(|| {
                collection_error("optional B absence is not a bounded distinct native result")
            })?;
        require(
            native.insert(absent.asset.clone(), outcome).is_none(),
            "optional B absence is not a bounded distinct native result",
        )?;
    }
    let mut canonical = BTreeMap::new();
    for asset in FIXED_ARTIFACTS {
        let (a, a_transfer) = builds
            .get(&(asset.name.to_owned(), "A".to_owned()))
            .ok_or_else(|| collection_error(format!("missing mandatory A for {}", asset.name)))?;
        if let Some((b, b_transfer)) = builds.get(&(asset.name.to_owned(), "B".to_owned())) {
            compare_pair(a, b)?;
            let same = member(a_transfer, "artifact")? == member(b_transfer, "artifact")?;
            if asset.name == FIXED_ARTIFACTS[0].name {
                require(same, "final Fastly bytes differ")?;
            } else {
                require(
                    native
                        .insert(
                            asset.name.to_owned(),
                            if same {
                                Outcome::Matched
                            } else {
                                Outcome::Mismatched
                            },
                        )
                        .is_none(),
                    "native B has both uploaded and absent results",
                )?;
            }
        } else if asset.name == FIXED_ARTIFACTS[0].name {
            return Err(collection_error("missing mandatory Fastly B"));
        } else {
            require(
                native.contains_key(asset.name),
                "native B has no bounded result",
            )?;
        }
        canonical.insert(
            asset.name.to_owned(),
            member(a_transfer, "artifact")?.to_vec(),
        );
    }
    let mut rebuild = BTreeMap::new();
    for asset in FIXED_ARTIFACTS {
        let (a, transfer) = builds
            .get(&(asset.name.to_owned(), "A".to_owned()))
            .ok_or_else(|| collection_error("canonical diagnostic disappeared"))?;
        let (outcome, reproduction) =
            if let Some((b, transfer)) = builds.get(&(asset.name.to_owned(), "B".to_owned())) {
                let outcome = if asset.name == FIXED_ARTIFACTS[0].name {
                    Outcome::Matched
                } else {
                    *native
                        .get(asset.name)
                        .ok_or_else(|| collection_error("native measurement disappeared"))?
                };
                (outcome, build_diagnostic(b, transfer)?)
            } else {
                all.absent
                    .iter()
                    .filter(|absent| absent.asset == asset.name)
                    .find_map(|absent| {
                        absent.outcome().map(|outcome| {
                            (
                                outcome,
                                json!({
                                    "outcome": outcome,
                                    "job_state": absent.job,
                                    "elapsed_seconds": absent.elapsed_seconds,
                                    "deadline_seconds": absent.deadline_seconds,
                                }),
                            )
                        })
                    })
                    .ok_or_else(|| collection_error("bounded native absence disappeared"))?
            };
        rebuild.insert(
            asset.name.to_owned(),
            json!({
                "schema_version":1,
                "kind":"candidate-collection-diagnostic",
                "release_qualification":false,
                "source_commit":a.source_commit,
                "recipe_sha256":a.recipe_sha256,
                "inventory_sha256":a.inventory_sha256,
                "outcome":outcome,
                "canonical":build_diagnostic(a,transfer)?,
                "reproduction":reproduction,
            }),
        );
    }
    Ok(CollectionDiagnostic {
        canonical,
        native,
        rebuild,
    })
}

fn build_diagnostic(record: &BuildRecord, transfer: &Transfer) -> Result<Value> {
    Ok(json!({
        "job":transfer.upload.job,
        "attempt":transfer.upload.attempt,
        "artifact_id":transfer.upload.artifact_id,
        "archive_sha256":transfer.upload.archive_sha256,
        "elapsed_seconds":transfer.upload.elapsed_seconds,
        "deadline_seconds":transfer.upload.deadline_seconds,
        "artifact_sha256":record.artifact_sha256,
        "artifact_size":record.artifact_size,
        "build_record_sha256":digest(member(transfer,"build-record.json")?),
        "composition_sha256":record.composition_sha256,
        "ambient":record.ambient,
        "claim":record.claim,
    }))
}
