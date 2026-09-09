//! Closed dependency-submission schema and deterministic artifact handling.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io::{Cursor, Read as _, Write as _};
use std::path::Path;
use std::process::Command;

use error_stack::Report;
use serde::{Deserialize, Serialize};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::repository::{NormalizedRelativePath, Repository, validate_exact_zip_framing};

const ARCHIVE_NAME: &str = "dependency-snapshot.json";
const MAXIMUM_ARCHIVE_BYTES: usize = 4 * 1024 * 1024;
const MAXIMUM_JSON_BYTES: usize = 2 * 1024 * 1024;
const MAXIMUM_RECORDS: usize = 5_000;
const MAXIMUM_STRING_BYTES: usize = 2_048;
const REPOSITORY: &str = "IABTechLab/trusted-server";
const SOURCE_REF: &str = "refs/heads/main";

/// Failure while generating or validating a dependency snapshot.
#[derive(Debug, derive_more::Display)]
pub enum DependencySnapshotError {
    /// The authenticated workflow context is invalid.
    #[display("invalid dependency snapshot context: {detail}")]
    Context {
        /// Stable failure detail.
        detail: String,
    },
    /// A lockfile cannot be decoded into bounded Cargo package records.
    #[display("invalid dependency lockfile: {detail}")]
    Lockfile {
        /// Stable failure detail.
        detail: String,
    },
    /// Cargo manifest metadata cannot be generated or decoded safely.
    #[display("invalid Cargo dependency metadata: {detail}")]
    Metadata {
        /// Stable failure detail.
        detail: String,
    },
    /// The artifact violates its closed archive or JSON contract.
    #[display("invalid dependency snapshot artifact: {detail}")]
    Artifact {
        /// Stable failure detail.
        detail: String,
    },
}

impl core::error::Error for DependencySnapshotError {}

/// Immutable GitHub context bound into a dependency snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DependencySnapshotContext {
    /// Exact repository identity.
    pub repository: String,
    /// Authenticated lowercase 40-hex source commit.
    pub source_sha: String,
    /// Authenticated default-branch ref.
    pub source_ref: String,
    /// GitHub Actions run identifier.
    pub run_id: u64,
    /// GitHub Actions run attempt.
    pub run_attempt: u64,
}

/// GitHub dependency-submission version-0 request.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DependencySnapshotV0 {
    /// Schema version, always zero.
    pub version: u8,
    /// Authenticated commit SHA.
    pub sha: String,
    /// Authenticated Git ref.
    #[serde(rename = "ref")]
    pub source_ref: String,
    /// Fixed job identity.
    pub job: SnapshotJob,
    /// Fixed detector identity.
    pub detector: SnapshotDetector,
    /// Dependency manifests keyed by repository-relative lockfile path.
    pub manifests: BTreeMap<String, SnapshotManifest>,
}

/// Fixed dependency-submission job identity.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotJob {
    /// Stable graph correlator.
    pub correlator: String,
    /// Run and attempt joined with a period.
    pub id: String,
}

/// Fixed generator identity.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotDetector {
    /// Detector name.
    pub name: String,
    /// Serialized detector version.
    pub version: String,
    /// Canonical detector source URL.
    pub url: String,
}

/// One Cargo lockfile dependency manifest.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotManifest {
    /// Repository-relative manifest name.
    pub name: String,
    /// Repository source location.
    pub file: SnapshotFile,
    /// Resolved packages keyed by stable Cargo identity.
    pub resolved: BTreeMap<String, SnapshotPackage>,
}

/// Location of one submitted manifest.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotFile {
    /// Repository-relative source location.
    pub source_location: String,
}

/// One resolved Cargo package.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotPackage {
    /// Canonical Cargo package URL.
    pub package_url: String,
    /// Directness within the submitted lock graph.
    pub relationship: DependencyRelationship,
    /// Runtime or development scope.
    pub scope: DependencyScope,
}

/// Closed dependency relationship enum.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DependencyRelationship {
    /// Declared directly by a manifest.
    Direct,
    /// Reached transitively through another package.
    Indirect,
}

/// Closed dependency scope enum.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DependencyScope {
    /// Used by shipped code.
    Runtime,
    /// Used only during development or validation.
    Development,
}

#[derive(Deserialize)]
struct CargoLock {
    #[serde(default)]
    package: Vec<CargoPackage>,
}

#[derive(Clone, Deserialize)]
struct CargoPackage {
    name: String,
    version: String,
    source: Option<String>,
    #[serde(default)]
    dependencies: Vec<String>,
}

#[derive(Deserialize)]
struct CargoMetadata {
    #[serde(default)]
    packages: Vec<CargoMetadataPackage>,
}

#[derive(Deserialize)]
struct CargoMetadataPackage {
    name: String,
    version: String,
    source: Option<String>,
    #[serde(default)]
    dependencies: Vec<CargoMetadataDependency>,
}

#[derive(Deserialize)]
struct CargoMetadataDependency {
    name: String,
    kind: Option<String>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct PackageId {
    name: String,
    version: String,
    source: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum TraversalScope {
    Runtime,
    Development,
}

#[derive(Clone, Copy, Debug, Default)]
struct DependencyKinds {
    runtime: bool,
    development: bool,
}

#[derive(Clone, Copy, Debug, Default)]
struct PackageClassification {
    direct: bool,
    runtime: bool,
    development: bool,
}

/// Generate the deterministic inner dependency-snapshot ZIP.
///
/// # Errors
///
/// Returns an error for untrusted context, malformed lockfiles, invalid Cargo
/// package identities, schema bounds, serialization, or archive generation.
pub fn generate_archive_with_metadata(
    context: &DependencySnapshotContext,
    root_lock: &[u8],
    root_metadata: &[u8],
    tool_lock: &[u8],
    tool_metadata: &[u8],
) -> Result<Vec<u8>, Report<DependencySnapshotError>> {
    validate_context(context)?;
    let mut manifests = BTreeMap::new();
    for (path, lock_bytes, metadata_bytes) in [
        ("Cargo.lock", root_lock, root_metadata),
        ("tools/docs-parity/Cargo.lock", tool_lock, tool_metadata),
    ] {
        let resolved = classify_lock_graph(lock_bytes, metadata_bytes, path)?;
        manifests.insert(
            path.to_owned(),
            SnapshotManifest {
                name: path.to_owned(),
                file: SnapshotFile {
                    source_location: path.to_owned(),
                },
                resolved,
            },
        );
    }
    let snapshot = DependencySnapshotV0 {
        version: 0,
        sha: context.source_sha.clone(),
        source_ref: context.source_ref.clone(),
        job: SnapshotJob {
            correlator: "trusted-server-docs-parity-v1".to_owned(),
            id: format!("{}.{}", context.run_id, context.run_attempt),
        },
        detector: SnapshotDetector {
            name: "trusted-server-docs-parity".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            url: "https://github.com/IABTechLab/trusted-server/tree/main/tools/docs-parity"
                .to_owned(),
        },
        manifests,
    };
    validate_snapshot(&snapshot, context)?;
    let json =
        serde_json::to_vec(&snapshot).map_err(|_error| artifact_error("cannot serialize JSON"))?;
    if json.len() > MAXIMUM_JSON_BYTES {
        return Err(artifact_error("JSON exceeds 2097152 bytes"));
    }
    deterministic_zip(ARCHIVE_NAME, &json)
}

pub(crate) fn generate_repository_archive(
    repository: &Repository,
    context: &DependencySnapshotContext,
) -> Result<Vec<u8>, Report<DependencySnapshotError>> {
    let root_path = NormalizedRelativePath::new(Path::new("Cargo.lock"))
        .map_err(|_error| lock_error("root lockfile path is invalid"))?;
    let tool_path = NormalizedRelativePath::new(Path::new("tools/docs-parity/Cargo.lock"))
        .map_err(|_error| lock_error("tool lockfile path is invalid"))?;
    let root_lock = repository
        .read_tracked_bounded(&root_path, MAXIMUM_JSON_BYTES)
        .map_err(|_error| lock_error("root lockfile is not a bounded tracked regular file"))?;
    let tool_lock = repository
        .read_tracked_bounded(&tool_path, MAXIMUM_JSON_BYTES)
        .map_err(|_error| lock_error("tool lockfile is not a bounded tracked regular file"))?;
    let root_metadata = generate_cargo_metadata(repository, "Cargo.toml")?;
    let tool_metadata = generate_cargo_metadata(repository, "tools/docs-parity/Cargo.toml")?;
    generate_archive_with_metadata(
        context,
        &root_lock,
        &root_metadata,
        &tool_lock,
        &tool_metadata,
    )
}

/// Validate and decode a dependency-snapshot ZIP against immutable context.
///
/// # Errors
///
/// Returns an error for an oversized or non-closed archive, malformed JSON,
/// unknown schema fields, invalid bounds, or context mismatch.
pub fn validate_archive(
    archive: &[u8],
    context: &DependencySnapshotContext,
) -> Result<DependencySnapshotV0, Report<DependencySnapshotError>> {
    validate_context(context)?;
    if archive.len() > MAXIMUM_ARCHIVE_BYTES {
        return Err(artifact_error("ZIP exceeds 4194304 bytes"));
    }
    let json = closed_zip_member(archive, ARCHIVE_NAME, MAXIMUM_JSON_BYTES)?;
    let snapshot = serde_json::from_slice::<DependencySnapshotV0>(&json)
        .map_err(|_error| artifact_error("JSON does not match version-0 schema"))?;
    validate_snapshot(&snapshot, context)?;
    let canonical_json = serde_json::to_vec(&snapshot)
        .map_err(|_error| artifact_error("cannot serialize canonical JSON"))?;
    let canonical_archive = deterministic_zip(ARCHIVE_NAME, &canonical_json)?;
    if canonical_archive != archive {
        return Err(artifact_error("ZIP or JSON framing is not canonical"));
    }
    Ok(snapshot)
}

fn validate_context(
    context: &DependencySnapshotContext,
) -> Result<(), Report<DependencySnapshotError>> {
    if context.repository != REPOSITORY
        || context.source_ref != SOURCE_REF
        || !lower_hex_sha(&context.source_sha)
        || context.run_id == 0
        || context.run_attempt == 0
    {
        return Err(context_error("repository, ref, SHA, run ID, or attempt"));
    }
    Ok(())
}

fn classify_lock_graph(
    lock_bytes: &[u8],
    metadata_bytes: &[u8],
    path: &str,
) -> Result<BTreeMap<String, SnapshotPackage>, Report<DependencySnapshotError>> {
    if lock_bytes.len() > MAXIMUM_JSON_BYTES {
        return Err(lock_error(format!("{path} exceeds 2097152 bytes")));
    }
    if metadata_bytes.len() > MAXIMUM_JSON_BYTES {
        return Err(metadata_error(format!(
            "metadata for {path} exceeds 2097152 bytes"
        )));
    }
    let text = core::str::from_utf8(lock_bytes)
        .map_err(|_error| lock_error(format!("{path} is not UTF-8")))?;
    let lock = toml::from_str::<CargoLock>(text)
        .map_err(|_error| lock_error(format!("cannot parse {path}")))?;
    if lock.package.len() > MAXIMUM_RECORDS {
        return Err(lock_error(format!("{path} exceeds 5000 package records")));
    }
    let metadata = serde_json::from_slice::<CargoMetadata>(metadata_bytes)
        .map_err(|_error| metadata_error(format!("cannot parse metadata for {path}")))?;
    if metadata.packages.len() > MAXIMUM_RECORDS {
        return Err(metadata_error(format!(
            "metadata for {path} exceeds 5000 package records"
        )));
    }

    let mut packages = BTreeMap::new();
    for package in lock.package {
        validate_package_component(&package.name, "name")?;
        validate_package_component(&package.version, "version")?;
        if let Some(source) = &package.source {
            validate_string(source, "package source")?;
        }
        for dependency in &package.dependencies {
            validate_string(dependency, "lockfile dependency")?;
        }
        let id = PackageId {
            name: package.name.clone(),
            version: package.version.clone(),
            source: package.source.clone(),
        };
        if packages.insert(id.clone(), package).is_some() {
            return Err(lock_error(format!(
                "duplicate Cargo package {}@{}",
                id.name, id.version
            )));
        }
    }

    let mut workspace_dependencies = BTreeMap::new();
    for package in metadata.packages {
        validate_package_component(&package.name, "metadata package name")?;
        validate_package_component(&package.version, "metadata package version")?;
        if package.source.is_some() {
            return Err(metadata_error(format!(
                "metadata for {path} contains a non-workspace root"
            )));
        }
        let id = PackageId {
            name: package.name,
            version: package.version,
            source: None,
        };
        if !packages.contains_key(&id) {
            return Err(metadata_error(format!(
                "workspace package {}@{} is absent from {path}",
                id.name, id.version
            )));
        }
        let mut dependencies = BTreeMap::<String, DependencyKinds>::new();
        for dependency in package.dependencies {
            validate_package_component(&dependency.name, "metadata dependency name")?;
            let kinds = dependencies.entry(dependency.name).or_default();
            match dependency.kind.as_deref() {
                None | Some("build") => kinds.runtime = true,
                Some("dev") => kinds.development = true,
                Some(_) => {
                    return Err(metadata_error(format!(
                        "metadata for {path} contains an unknown dependency kind"
                    )));
                }
            }
        }
        if workspace_dependencies.insert(id, dependencies).is_some() {
            return Err(metadata_error(format!(
                "metadata for {path} contains a duplicate workspace package"
            )));
        }
    }

    let mut classifications = BTreeMap::<PackageId, PackageClassification>::new();
    let mut queue = VecDeque::new();
    for root in workspace_dependencies.keys() {
        queue.push_back((root.clone(), TraversalScope::Runtime));
    }
    let mut visited = BTreeSet::new();
    while let Some((current_id, current_scope)) = queue.pop_front() {
        if !visited.insert((current_id.clone(), current_scope)) {
            continue;
        }
        let package = packages
            .get(&current_id)
            .ok_or_else(|| lock_error(format!("dependency disappeared from {path}")))?;
        let direct_kinds = workspace_dependencies.get(&current_id);
        for dependency_text in &package.dependencies {
            let dependency_id = resolve_lock_dependency(dependency_text, &packages, path)?;
            let scopes = traversal_scopes(
                current_scope,
                direct_kinds,
                dependency_id.name.as_str(),
                path,
            )?;
            for scope in scopes {
                if dependency_id.source.is_some() {
                    let classification = classifications.entry(dependency_id.clone()).or_default();
                    classification.direct |= direct_kinds.is_some();
                    match scope {
                        TraversalScope::Runtime => classification.runtime = true,
                        TraversalScope::Development => classification.development = true,
                    }
                }
                queue.push_back((dependency_id.clone(), scope));
            }
        }
    }

    let mut resolved = BTreeMap::new();
    for (id, classification) in classifications {
        let key = format!("{}@{}", id.name, id.version);
        let value = SnapshotPackage {
            package_url: format!("pkg:cargo/{}@{}", id.name, id.version),
            relationship: if classification.direct {
                DependencyRelationship::Direct
            } else {
                DependencyRelationship::Indirect
            },
            scope: if classification.runtime {
                DependencyScope::Runtime
            } else if classification.development {
                DependencyScope::Development
            } else {
                return Err(lock_error(format!(
                    "dependency {key} has no reachable scope"
                )));
            },
        };
        if resolved.insert(key.clone(), value).is_some() {
            return Err(lock_error(format!("duplicate Cargo identity {key}")));
        }
    }
    Ok(resolved)
}

fn traversal_scopes(
    current_scope: TraversalScope,
    direct_kinds: Option<&BTreeMap<String, DependencyKinds>>,
    dependency_name: &str,
    path: &str,
) -> Result<Vec<TraversalScope>, Report<DependencySnapshotError>> {
    if current_scope == TraversalScope::Development {
        return Ok(vec![TraversalScope::Development]);
    }
    let Some(direct_kinds) = direct_kinds else {
        return Ok(vec![TraversalScope::Runtime]);
    };
    let kinds = direct_kinds.get(dependency_name).ok_or_else(|| {
        metadata_error(format!(
            "workspace dependency {dependency_name} from {path} is absent from metadata"
        ))
    })?;
    let mut scopes = Vec::with_capacity(2);
    if kinds.runtime {
        scopes.push(TraversalScope::Runtime);
    }
    if kinds.development {
        scopes.push(TraversalScope::Development);
    }
    if scopes.is_empty() {
        return Err(metadata_error(format!(
            "workspace dependency {dependency_name} from {path} has no supported kind"
        )));
    }
    Ok(scopes)
}

fn resolve_lock_dependency(
    value: &str,
    packages: &BTreeMap<PackageId, CargoPackage>,
    path: &str,
) -> Result<PackageId, Report<DependencySnapshotError>> {
    let parts = value.split_ascii_whitespace().collect::<Vec<_>>();
    if parts.is_empty() || parts.len() > 3 {
        return Err(lock_error(format!(
            "invalid dependency reference in {path}"
        )));
    }
    let name = parts[0];
    let version = parts.get(1).copied();
    let source = parts.get(2).and_then(|value| {
        value
            .strip_prefix('(')
            .and_then(|value| value.strip_suffix(')'))
    });
    if parts.len() == 3 && source.is_none() {
        return Err(lock_error(format!(
            "invalid dependency source reference in {path}"
        )));
    }
    let matches = packages
        .keys()
        .filter(|candidate| {
            candidate.name == name
                && version.is_none_or(|version| candidate.version == version)
                && source.is_none_or(|source| candidate.source.as_deref() == Some(source))
        })
        .cloned()
        .collect::<Vec<_>>();
    if matches.len() != 1 {
        return Err(lock_error(format!(
            "dependency reference {value} in {path} is missing or ambiguous"
        )));
    }
    matches
        .into_iter()
        .next()
        .ok_or_else(|| lock_error("dependency resolution failed"))
}

fn generate_cargo_metadata(
    repository: &Repository,
    manifest_path: &str,
) -> Result<Vec<u8>, Report<DependencySnapshotError>> {
    let normalized = NormalizedRelativePath::new(Path::new(manifest_path))
        .map_err(|_error| metadata_error("Cargo manifest path is invalid"))?;
    repository
        .read_tracked_bounded(&normalized, MAXIMUM_JSON_BYTES)
        .map_err(|_error| {
            metadata_error(format!("{manifest_path} is not a bounded tracked file"))
        })?;
    let output = Command::new("cargo")
        .args([
            "metadata",
            "--manifest-path",
            manifest_path,
            "--format-version",
            "1",
            "--no-deps",
            "--locked",
            "--offline",
        ])
        .current_dir(repository.root())
        .output()
        .map_err(|_error| {
            metadata_error(format!("cannot execute cargo metadata for {manifest_path}"))
        })?;
    if !output.status.success() {
        let diagnostic = String::from_utf8_lossy(&output.stderr)
            .chars()
            .take(MAXIMUM_STRING_BYTES)
            .collect::<String>();
        return Err(metadata_error(format!(
            "cargo metadata failed for {manifest_path}: {}",
            diagnostic.trim()
        )));
    }
    if output.stdout.len() > MAXIMUM_JSON_BYTES {
        return Err(metadata_error(format!(
            "cargo metadata for {manifest_path} exceeds 2097152 bytes"
        )));
    }
    Ok(output.stdout)
}

fn validate_snapshot(
    snapshot: &DependencySnapshotV0,
    context: &DependencySnapshotContext,
) -> Result<(), Report<DependencySnapshotError>> {
    if snapshot.version != 0
        || snapshot.sha != context.source_sha
        || snapshot.source_ref != context.source_ref
        || snapshot.job.correlator != "trusted-server-docs-parity-v1"
        || snapshot.job.id != format!("{}.{}", context.run_id, context.run_attempt)
        || snapshot.detector.name != "trusted-server-docs-parity"
        || snapshot.detector.version != env!("CARGO_PKG_VERSION")
        || snapshot.detector.url
            != "https://github.com/IABTechLab/trusted-server/tree/main/tools/docs-parity"
        || snapshot.manifests.len() != 2
    {
        return Err(artifact_error(
            "fixed identity or authenticated context differs",
        ));
    }
    let expected = ["Cargo.lock", "tools/docs-parity/Cargo.lock"];
    let mut records = 0usize;
    for path in expected {
        let manifest = snapshot
            .manifests
            .get(path)
            .ok_or_else(|| artifact_error(format!("missing manifest {path}")))?;
        if manifest.name != path || manifest.file.source_location != path {
            return Err(artifact_error(format!(
                "manifest identity differs for {path}"
            )));
        }
        records = records
            .checked_add(manifest.resolved.len())
            .ok_or_else(|| artifact_error("record count overflow"))?;
        for (key, package) in &manifest.resolved {
            validate_string(key, "resolved key")?;
            validate_string(&package.package_url, "package URL")?;
            if canonical_cargo_purl(key).as_deref() != Some(package.package_url.as_str()) {
                return Err(artifact_error("package URL is not canonical Cargo PURL"));
            }
        }
    }
    if records > MAXIMUM_RECORDS {
        return Err(artifact_error("snapshot exceeds 5000 resolved records"));
    }
    Ok(())
}

fn validate_package_component(
    value: &str,
    field: &'static str,
) -> Result<(), Report<DependencySnapshotError>> {
    validate_string(value, field)?;
    if !is_package_component(value) {
        return Err(lock_error(format!("invalid Cargo package {field}")));
    }
    Ok(())
}

fn canonical_cargo_purl(key: &str) -> Option<String> {
    let (name, version) = key.rsplit_once('@')?;
    if !is_package_component(name) || !is_package_component(version) {
        return None;
    }
    Some(format!("pkg:cargo/{name}@{version}"))
}

fn is_package_component(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'+'))
}

fn validate_string(
    value: &str,
    field: &'static str,
) -> Result<(), Report<DependencySnapshotError>> {
    if value.len() > MAXIMUM_STRING_BYTES || value.chars().any(char::is_control) {
        return Err(artifact_error(format!("{field} exceeds string bounds")));
    }
    Ok(())
}

fn deterministic_zip(
    name: &str,
    contents: &[u8],
) -> Result<Vec<u8>, Report<DependencySnapshotError>> {
    let cursor = Cursor::new(Vec::new());
    let mut writer = ZipWriter::new(cursor);
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Stored)
        .unix_permissions(0o644);
    writer
        .start_file(name, options)
        .map_err(|_error| artifact_error("cannot create ZIP member"))?;
    writer
        .write_all(contents)
        .map_err(|_error| artifact_error("cannot write ZIP member"))?;
    let bytes = writer
        .finish()
        .map_err(|_error| artifact_error("cannot finish ZIP"))?
        .into_inner();
    if bytes.len() > MAXIMUM_ARCHIVE_BYTES {
        return Err(artifact_error("ZIP exceeds 4194304 bytes"));
    }
    Ok(bytes)
}

fn closed_zip_member(
    bytes: &[u8],
    expected_name: &str,
    maximum_member_bytes: usize,
) -> Result<Vec<u8>, Report<DependencySnapshotError>> {
    validate_exact_zip_framing(bytes).map_err(artifact_error)?;
    let cursor = Cursor::new(bytes);
    let mut archive =
        ZipArchive::new(cursor).map_err(|_error| artifact_error("cannot parse ZIP"))?;
    if archive.len() != 1 {
        return Err(artifact_error("ZIP must contain exactly one member"));
    }
    let mut member = archive
        .by_index(0)
        .map_err(|_error| artifact_error("cannot read ZIP member"))?;
    if member.name() != expected_name
        || member.enclosed_name().is_none()
        || !member.is_file()
        || !safe_regular_mode(member.unix_mode())
        || member.size() > maximum_member_bytes as u64
    {
        return Err(artifact_error(
            "ZIP member name, type, mode, or size differs",
        ));
    }
    let mut contents = Vec::with_capacity(
        usize::try_from(member.size()).map_err(|_error| artifact_error("member is too large"))?,
    );
    std::io::Read::take(&mut member, (maximum_member_bytes as u64).saturating_add(1))
        .read_to_end(&mut contents)
        .map_err(|_error| artifact_error("cannot read ZIP member"))?;
    if contents.len() > maximum_member_bytes {
        return Err(artifact_error("ZIP member exceeds its byte bound"));
    }
    Ok(contents)
}

fn safe_regular_mode(mode: Option<u32>) -> bool {
    mode.is_some_and(|value| value & 0o7777 == 0o644 && matches!(value & 0o170000, 0 | 0o100000))
}

fn lower_hex_sha(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn context_error(detail: impl Into<String>) -> Report<DependencySnapshotError> {
    Report::new(DependencySnapshotError::Context {
        detail: detail.into(),
    })
}

fn lock_error(detail: impl Into<String>) -> Report<DependencySnapshotError> {
    Report::new(DependencySnapshotError::Lockfile {
        detail: detail.into(),
    })
}

fn metadata_error(detail: impl Into<String>) -> Report<DependencySnapshotError> {
    Report::new(DependencySnapshotError::Metadata {
        detail: detail.into(),
    })
}

fn artifact_error(detail: impl Into<String>) -> Report<DependencySnapshotError> {
    Report::new(DependencySnapshotError::Artifact {
        detail: detail.into(),
    })
}
