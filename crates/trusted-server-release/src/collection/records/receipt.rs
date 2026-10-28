//! Strict collector parser for the producer's sealed composition receipt.
//!
//! `composition.json` is the exact normalized receipt the producer seals with
//! its owning build (`RecipeCompleted::composition_export`): the Cargo query
//! plan digests, the actual compiler observations and the graph, package
//! origin, native, JavaScript and platform receipts. Every level the collector
//! checks rejects unknown or missing fields. The installed tool subtrees of the
//! platform receipt (`rust`, `apple`, `native_tools`, `external_requirements`
//! runtime facts) are held opaquely; only their artifact and target bindings,
//! Apple evidence presence and the runtime format tag are checked here.
//!
//! The receipt is imported data. These checks bind it to the build record's
//! artifact, source, inventory, materials and target; they do not prove
//! complete source coverage or final executable membership.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;
use serde_json::Value;

use super::{Result, require};
use crate::collection::collection_error;
use crate::documents::{Role, lower_hex, parse_json};
use crate::error::ReleaseError;
use crate::policy::ReleaseTarget;

/// Most compiler units a receipt may name.
const UNIT_LIMIT: usize = 65_536;
/// Most compiled packages, and so package origins, a receipt may name.
const PACKAGE_LIMIT: usize = 16_384;
/// Most prepared JS modules the producer's final-byte check accepts.
const MODULE_LIMIT: usize = 1024;
/// Classification of a complete module found in the final bytes.
const EMBEDDED_JS: &str = "conservative_embedded_js";
/// Classification of a prepared module the final bytes do not contain.
const JS_BUILD_MATERIAL: &str = "js_build_material";

/// Build record facts, already checked by the collector, that a receipt restates.
pub(super) struct ReceiptBinding<'a> {
    /// Exact final artifact digest of the same transfer.
    pub(super) artifact_sha256: &'a str,
    /// Approved source commit of the trusted invocation.
    pub(super) source_commit: &'a str,
    /// Shared original JS inventory digest.
    pub(super) inventory_sha256: &'a str,
    /// Prepared Cargo materials tree digest of the build inputs.
    pub(super) materials_sha256: &'a str,
    /// Target of the fixed asset; Fastly (`wasm32-wasip1`) embeds every module.
    pub(super) target: ReleaseTarget,
}

// Shape-only fields are deserialized so unknown, missing or mistyped members
// are rejected; the collector reads only the bindings it checks.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    metadata_plan_sha256: String,
    tree_plan_sha256: String,
    observations: Observations,
    composition: Composition,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Observations {
    packages: Vec<Package>,
    units: Vec<Unit>,
    build_scripts: Vec<BuildScript>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Context {
    Host,
    Target,
}

#[allow(
    dead_code,
    reason = "every member is parsed so the strict receipt shape rejects missing or mistyped ones"
)]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Package {
    id: String,
    name: String,
    version: String,
    source: Option<String>,
    manifest_path: String,
    license: Option<String>,
}

#[allow(
    dead_code,
    reason = "every member is parsed so the strict receipt shape rejects missing or mistyped ones"
)]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Unit {
    package_id: String,
    target_name: String,
    target_kind: Vec<String>,
    crate_types: Vec<String>,
    context: Context,
    features: Vec<String>,
    profile: Value,
    filenames: Vec<String>,
    executable: Option<String>,
}

#[allow(
    dead_code,
    reason = "every member is parsed so the strict receipt shape rejects missing or mistyped ones"
)]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BuildScript {
    package_id: String,
    context: Context,
    out_dir: String,
    linked_libs: Vec<String>,
    linked_paths: Vec<String>,
    cfgs: Vec<String>,
    environment: Vec<(String, String)>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Composition {
    graph: Graph,
    package_origins: PackageOrigins,
    native: Native,
    javascript: Javascript,
    platform: Platform,
}

#[allow(
    dead_code,
    reason = "every member is parsed so the strict receipt shape rejects missing or mistyped ones"
)]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Graph {
    selected_package_id: String,
    tree_package_ids: Vec<String>,
    observed_package_ids: Vec<String>,
    edges: Vec<Edge>,
}

#[allow(
    dead_code,
    reason = "every member is parsed so the strict receipt shape rejects missing or mistyped ones"
)]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Edge {
    parent: String,
    dependency: String,
    kinds: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PackageOrigins {
    source_commit: String,
    materials_sha256: String,
    packages: BTreeMap<String, PackageOrigin>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PackageOrigin {
    name: String,
    version: String,
    manifest_path: String,
    prepared_manifest_sha256: String,
    origin: Original,
}

#[allow(
    dead_code,
    reason = "every member is parsed so the strict receipt shape rejects missing or mistyped ones"
)]
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Original {
    Workspace {
        source_commit: String,
        path: String,
    },
    Material {
        source: String,
        original: CargoOrigin,
    },
}

#[allow(
    dead_code,
    reason = "every member is parsed so the strict receipt shape rejects missing or mistyped ones"
)]
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum CargoOrigin {
    Registry {
        archive_sha256: String,
    },
    Git {
        revision: String,
        repository_tree: String,
        package_tree: String,
        package_path: String,
        committed_manifest_blob: String,
        committed_manifest_sha256: String,
        normalized_manifest_sha256: Option<String>,
    },
}

#[allow(
    dead_code,
    reason = "every member is parsed so the strict receipt shape rejects missing or mistyped ones"
)]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Native {
    contributors: Vec<Contributor>,
    build_tool_package_ids: Vec<String>,
    material_native_coverage_complete: bool,
}

#[allow(
    dead_code,
    reason = "every member is parsed so the strict receipt shape rejects missing or mistyped ones"
)]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Contributor {
    package_id: String,
    name: String,
    version: String,
    context: Context,
    features: Vec<String>,
    classification: String,
    original_sha256: String,
    source_tree_sha256: String,
    source_subtrees: Vec<Subtree>,
    linked_libs: Vec<String>,
    linked_paths: Vec<String>,
    archives: Vec<Archive>,
}

#[allow(
    dead_code,
    reason = "every member is parsed so the strict receipt shape rejects missing or mistyped ones"
)]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Subtree {
    path: String,
    sha256: String,
}

#[allow(
    dead_code,
    reason = "every member is parsed so the strict receipt shape rejects missing or mistyped ones"
)]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Archive {
    path: String,
    size: u64,
    sha256: String,
}

#[allow(
    dead_code,
    reason = "every member is parsed so the strict receipt shape rejects missing or mistyped ones"
)]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Javascript {
    artifact_sha256: String,
    inventory_sha256: String,
    modules: Vec<ModuleMembership>,
    method: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ModuleMembership {
    module: Module,
    classification: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Module {
    id: String,
    filename: String,
    size: u64,
    sha256: String,
}

#[allow(
    dead_code,
    reason = "every member is parsed so the strict receipt shape rejects missing or mistyped ones"
)]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Platform {
    target: String,
    artifact_sha256: String,
    rust: Value,
    apple: Value,
    external_requirements: ExternalRequirements,
    native_tools: Value,
    material_native_coverage_complete: bool,
}

#[allow(
    dead_code,
    reason = "every member is parsed so the strict receipt shape rejects missing or mistyped ones"
)]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExternalRequirements {
    target: String,
    artifact_sha256: String,
    runtime: Value,
}

/// Parses one sealed receipt strictly and binds it to its checked build record.
///
/// # Errors
///
/// Rejects oversized, duplicate-key, non-finite or mistyped JSON, unknown or
/// missing members, malformed digests, another artifact, source, inventory,
/// materials tree or target, Apple evidence or a runtime format foreign to the
/// target, a complete native coverage claim, no compiled units, malformed or unjoined package
/// origins, malformed JS membership, and a Fastly receipt that does not embed
/// every prepared module.
pub(super) fn check_receipt(bytes: &[u8], binding: &ReceiptBinding<'_>) -> Result<()> {
    let value = parse_json(bytes, Role::CargoObservation)
        .map_err(|error| error.change_context(ReleaseError::Collection))?;
    let receipt: Receipt = serde_json::from_value(value).map_err(collection_error)?;
    let composition = &receipt.composition;
    let platform = &composition.platform;
    let javascript = &composition.javascript;
    let origins = &composition.package_origins;
    require(
        [
            &receipt.metadata_plan_sha256,
            &receipt.tree_plan_sha256,
            &origins.materials_sha256,
        ]
        .into_iter()
        .all(|digest| lower_hex(digest, 64))
            && native_digests_are_valid(&composition.native),
        "composition receipt digests are malformed",
    )?;
    require(
        javascript.artifact_sha256 == binding.artifact_sha256
            && platform.artifact_sha256 == binding.artifact_sha256
            && platform.external_requirements.artifact_sha256 == binding.artifact_sha256,
        "composition receipt is not bound to the final artifact",
    )?;
    require(
        origins.source_commit == binding.source_commit
            && origins
                .packages
                .values()
                .all(|package| match &package.origin {
                    Original::Workspace { source_commit, .. } => {
                        source_commit == binding.source_commit
                    }
                    Original::Material { .. } => true,
                }),
        "composition receipt is not bound to the approved source",
    )?;
    require(
        javascript.inventory_sha256 == binding.inventory_sha256,
        "composition receipt is not bound to the shared JS inventory",
    )?;
    require(
        origins.materials_sha256 == binding.materials_sha256,
        "composition receipt is not bound to the build materials",
    )?;
    let target = binding.target.triple();
    require(
        platform.target == target && platform.external_requirements.target == target,
        "composition receipt names another target",
    )?;
    check_platform_shape(platform, binding.target)?;
    require(
        !composition.native.material_native_coverage_complete
            && !platform.material_native_coverage_complete,
        "composition receipt claims complete material native coverage",
    )?;
    let units = &receipt.observations.units;
    require(
        !units.is_empty() && units.len() <= UNIT_LIMIT,
        "composition receipt has no compiled units within its bound",
    )?;
    require(
        origins_are_joined(&receipt),
        "composition receipt package origins are malformed or ambiguous",
    )?;
    check_javascript(javascript, binding.target == ReleaseTarget::Wasm32Wasip1)
}

/// Apple evidence and the runtime format follow the target, as the producer writes them.
///
/// `PlatformEvidence::collect` retains Apple tool evidence exactly for Darwin
/// targets, and `ExternalRequirements` is built from the inspected executable
/// format (`from_wasm`, `from_elf`, `from_macho`), serialized with tag
/// `format` as `wasm`, `elf` or `macho`.
fn check_platform_shape(platform: &Platform, target: ReleaseTarget) -> Result<()> {
    require(
        platform.apple.is_null() != target.is_darwin(),
        "composition receipt Apple evidence does not match its target",
    )?;
    let format = match target {
        ReleaseTarget::Wasm32Wasip1 => "wasm",
        ReleaseTarget::X86_64UnknownLinuxGnu => "elf",
        ReleaseTarget::Aarch64AppleDarwin | ReleaseTarget::X86_64AppleDarwin => "macho",
    };
    require(
        platform.external_requirements.runtime["format"] == format,
        "composition receipt runtime format does not match its target",
    )
}

/// Native measurement digests are exact SHA-256 identities.
fn native_digests_are_valid(native: &Native) -> bool {
    native.contributors.iter().all(|contributor| {
        lower_hex(&contributor.original_sha256, 64)
            && lower_hex(&contributor.source_tree_sha256, 64)
            && contributor
                .source_subtrees
                .iter()
                .all(|subtree| lower_hex(&subtree.sha256, 64))
            && contributor
                .archives
                .iter()
                .all(|archive| lower_hex(&archive.sha256, 64))
    })
}

/// Every observed package has exactly one well-formed origin, and nothing else does.
///
/// Package IDs are unique because duplicate object keys are rejected at parse
/// time and observed IDs must be distinct; prepared manifest paths must also be
/// distinct, as the producer's origin join requires.
fn origins_are_joined(receipt: &Receipt) -> bool {
    let observed = &receipt.observations.packages;
    let origins = &receipt.composition.package_origins.packages;
    let observed_ids = observed
        .iter()
        .map(|package| package.id.as_str())
        .collect::<BTreeSet<_>>();
    let manifests = origins
        .values()
        .map(|origin| origin.manifest_path.as_str())
        .collect::<BTreeSet<_>>();
    let joined = |id: &str| origins.contains_key(id);
    !origins.is_empty()
        && origins.len() <= PACKAGE_LIMIT
        && observed_ids.len() == observed.len()
        && observed_ids.len() == origins.len()
        && manifests.len() == origins.len()
        && observed.iter().all(|package| {
            origins.get(&package.id).is_some_and(|origin| {
                origin.name == package.name && origin.version == package.version
            })
        })
        && origins.values().all(origin_is_well_formed)
        && receipt
            .observations
            .units
            .iter()
            .all(|unit| joined(&unit.package_id))
        && receipt
            .observations
            .build_scripts
            .iter()
            .all(|script| joined(&script.package_id))
        && receipt
            .composition
            .native
            .contributors
            .iter()
            .all(|contributor| joined(&contributor.package_id))
        && joined(&receipt.composition.graph.selected_package_id)
}

fn origin_is_well_formed(origin: &PackageOrigin) -> bool {
    lower_hex(&origin.prepared_manifest_sha256, 64)
        && !origin.manifest_path.is_empty()
        && match &origin.origin {
            Original::Workspace { path, .. } => path == &origin.manifest_path,
            Original::Material { source, original } => {
                !source.is_empty()
                    && match original {
                        CargoOrigin::Registry { archive_sha256 } => lower_hex(archive_sha256, 64),
                        CargoOrigin::Git {
                            committed_manifest_sha256,
                            normalized_manifest_sha256,
                            ..
                        } => {
                            lower_hex(committed_manifest_sha256, 64)
                                && normalized_manifest_sha256
                                    .as_deref()
                                    .is_none_or(|digest| lower_hex(digest, 64))
                        }
                    }
            }
        }
}

fn check_javascript(javascript: &Javascript, fastly: bool) -> Result<()> {
    let modules = &javascript.modules;
    let ids = modules
        .iter()
        .map(|entry| entry.module.id.as_str())
        .collect::<BTreeSet<_>>();
    let filenames = modules
        .iter()
        .map(|entry| entry.module.filename.as_str())
        .collect::<BTreeSet<_>>();
    require(
        !modules.is_empty()
            && modules.len() <= MODULE_LIMIT
            && ids.len() == modules.len()
            && filenames.len() == modules.len()
            && modules.iter().all(|entry| {
                entry.module.size > 0
                    && lower_hex(&entry.module.sha256, 64)
                    && [EMBEDDED_JS, JS_BUILD_MATERIAL].contains(&entry.classification.as_str())
            }),
        "composition receipt JS membership is malformed",
    )?;
    require(
        !fastly
            || modules
                .iter()
                .all(|entry| entry.classification == EMBEDDED_JS),
        "composition receipt omits a complete Fastly JS module",
    )
}
