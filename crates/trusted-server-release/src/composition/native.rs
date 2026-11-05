//! Candidate native source and archive observations, without release authority.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, Metadata, OpenOptions};
use std::io::Read as _;
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};

use error_stack::Report;
use serde::Serialize;
use sha2::{Digest as _, Sha256};

use super::{
    BuildScriptObservation, CargoObservations, RustPackage, UnitContext, composition_error,
    normal_path,
};
use crate::documents::lower_hex;
use crate::error::ReleaseError;
use crate::policy::ReleaseTarget;
use crate::tool_tree::tool_tree_digest;

const ARCHIVE_LIMIT: u64 = 512 * 1024 * 1024;

/// A source root supplied only by the verified exact-Cargo-ID origin join.
pub(crate) struct NativeSource {
    /// SHA-256 of the verified original registry distribution.
    pub(crate) original_sha256: String,
    /// Host path of the verified prepared package.
    pub(crate) source_root: PathBuf,
}

/// Inputs selected by the recipe, rather than deserialized caller assertions.
pub(crate) struct NativeInputs<'a> {
    /// Exact observed Cargo package IDs joined to verified originals.
    pub(crate) origins: &'a BTreeMap<String, NativeSource>,
    /// Cargo target root as seen by the confined child.
    pub(crate) execution_output_root: &'a Path,
    /// The same Cargo target root in the host namespace.
    pub(crate) host_output_root: &'a Path,
    /// Fixed release target of the recipe.
    pub(crate) target: ReleaseTarget,
    /// Actual selected build environment, including anti-substitution controls.
    pub(crate) environment: &'a BTreeMap<String, String>,
    /// Owning attempt's cancellation/deadline check.
    pub(crate) check: &'a dyn Fn() -> Result<(), Report<ReleaseError>>,
}

/// Candidate native measurements; never an attestation or qualification token.
#[derive(Debug, Serialize)]
pub(crate) struct NativeInventory {
    contributors: Vec<NativeContributor>,
    build_tool_package_ids: Vec<String>,
    material_native_coverage_complete: bool,
}

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(Clone))]
pub(crate) struct NativeContributor {
    package_id: String,
    name: String,
    version: String,
    context: UnitContext,
    features: Vec<String>,
    classification: &'static str,
    original_sha256: String,
    source_tree_sha256: String,
    source_subtrees: Vec<SourceSubtree>,
    linked_libs: Vec<String>,
    linked_paths: Vec<String>,
    archives: Vec<NativeArchive>,
    #[serde(skip)]
    source_root: PathBuf,
}

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(Clone))]
pub(crate) struct SourceSubtree {
    path: &'static str,
    sha256: String,
}

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(Clone))]
pub(crate) struct NativeArchive {
    path: PathBuf,
    size: u64,
    sha256: String,
    #[serde(skip)]
    host_path: PathBuf,
    #[serde(skip)]
    output_root: PathBuf,
}

/// Source-reviewed rules for the locked native implementations.
struct ReviewedNative {
    version: &'static str,
    allowed_features: &'static [&'static str],
    required_features: &'static [&'static str],
    libraries: &'static [&'static str],
    source_subtrees: &'static [&'static str],
}

fn reviewed(name: &str) -> Option<ReviewedNative> {
    Some(match name {
        "aws-lc-sys" => ReviewedNative {
            version: "0.42.0",
            // The locked Rust wrapper disables default bindings. Its NASM
            // feature can select prebuilt objects only on Windows x86_64,
            // outside the selected Linux and macOS build platforms.
            allowed_features: &["all-bindings", "default", "prebuilt-nasm"],
            required_features: &[],
            libraries: &["aws_lc_0_42_0_crypto"],
            source_subtrees: &["aws-lc"],
        },
        "ring" => ReviewedNative {
            version: "0.17.14",
            allowed_features: &["alloc", "default", "dev_urandom_fallback", "std"],
            required_features: &["alloc"],
            // The reviewed ring build script always builds and links its test
            // archive, including for release builds. Neither archive's presence
            // proves that every member survives final linking.
            libraries: &["ring_core_0_17_14_", "ring_core_0_17_14__test"],
            source_subtrees: &["crypto", "include", "pregenerated"],
        },
        "libsqlite3-sys" => ReviewedNative {
            version: "0.38.2",
            allowed_features: &[
                "bundled",
                "bundled_bindings",
                "cc",
                "default",
                "min_sqlite_version_3_34_1",
                "pkg-config",
                "vcpkg",
                "unlock_notify",
            ],
            required_features: &["bundled", "bundled_bindings", "cc"],
            libraries: &["sqlite3"],
            source_subtrees: &["sqlite3"],
        },
        _ => return None,
    })
}

fn real_directory(path: &Path) -> Result<(), Report<ReleaseError>> {
    normal_path(path)?;
    let metadata = fs::symlink_metadata(path).map_err(composition_error)?;
    let resolved = fs::canonicalize(path).map_err(composition_error)?;
    if !metadata.is_dir() || resolved != path {
        return Err(composition_error(
            "native directory is aliased or not a directory",
        ));
    }
    Ok(())
}

fn archive_parent(root: &Path, file: &Path) -> Result<(), Report<ReleaseError>> {
    normal_path(file)?;
    real_directory(root)?;
    let parent = file
        .parent()
        .ok_or_else(|| composition_error("native archive has no parent"))?;
    let relative = parent.strip_prefix(root).map_err(composition_error)?;
    let mut current = root.to_owned();
    for component in relative.components() {
        current.push(component);
        real_directory(&current)?;
    }
    Ok(())
}

fn same_archive(before: &Metadata, after: &Metadata) -> bool {
    after.is_file()
        && before.dev() == after.dev()
        && before.ino() == after.ino()
        && before.nlink() == after.nlink()
        && before.len() == after.len()
        && before.mode() == after.mode()
        && before.mtime() == after.mtime()
        && before.mtime_nsec() == after.mtime_nsec()
        && before.ctime() == after.ctime()
        && before.ctime_nsec() == after.ctime_nsec()
}

fn archive_digest(
    root: &Path,
    path: &Path,
    check: &dyn Fn() -> Result<(), Report<ReleaseError>>,
) -> Result<crate::documents::FileDigest, Report<ReleaseError>> {
    archive_parent(root, path)?;
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .map_err(composition_error)?;
    let before = file.metadata().map_err(composition_error)?;
    if !before.is_file() || before.nlink() != 1 || before.len() > ARCHIVE_LIMIT {
        return Err(composition_error(
            "native archive is not a bounded regular single-link file",
        ));
    }
    let mut magic = [0; 8];
    file.read_exact(&mut magic).map_err(composition_error)?;
    if &magic != b"!<arch>\n" {
        return Err(composition_error("native output is not a regular archive"));
    }
    // Format and content identity must describe the same held descriptor.
    let mut hash = Sha256::new();
    hash.update(magic);
    let mut size = magic.len() as u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        check()?;
        let count = file.read(&mut buffer).map_err(composition_error)?;
        if count == 0 {
            break;
        }
        size += count as u64;
        if size > before.len() || size > ARCHIVE_LIMIT {
            return Err(composition_error("native archive grows while hashing"));
        }
        hash.update(&buffer[..count]);
    }
    check()?;
    if size != before.len()
        || !same_archive(&before, &file.metadata().map_err(composition_error)?)
        || !same_archive(
            &before,
            &fs::symlink_metadata(path).map_err(composition_error)?,
        )
    {
        return Err(composition_error(
            "native archive content, metadata or path changes while hashing",
        ));
    }
    archive_parent(root, path)?;
    Ok(crate::documents::FileDigest {
        sha256: hex::encode(hash.finalize()),
        size,
        mode: before.mode(),
    })
}

fn source_digest(
    path: &Path,
    check: &dyn Fn() -> Result<(), Report<ReleaseError>>,
) -> Result<String, Report<ReleaseError>> {
    real_directory(path)?;
    let sha256 = tool_tree_digest(path, check)
        .map_err(|error| error.change_context(ReleaseError::Composition))?;
    real_directory(path)?;
    Ok(sha256)
}

fn check_environment(name: &str, inputs: &NativeInputs<'_>) -> Result<(), Report<ReleaseError>> {
    if name == "aws-lc-sys" {
        let target_control = format!(
            "AWS_LC_SYS_USE_SYSTEM_{}",
            inputs.target.triple().replace('-', "_")
        );
        for control in ["AWS_LC_SYS_USE_SYSTEM", target_control.as_str()] {
            if inputs.environment.get(control).map(String::as_str) != Some("0") {
                return Err(composition_error(
                    "AWS-LC system discovery is not explicitly disabled",
                ));
            }
        }
        if inputs.environment.iter().any(|(key, value)| {
            key.starts_with("AWS_LC_SYS_")
                && !(key.starts_with("AWS_LC_SYS_USE_SYSTEM") && value == "0")
                && !(inputs.target == ReleaseTarget::X86_64UnknownLinuxGnu
                    && matches!(
                        key.as_str(),
                        "AWS_LC_SYS_CMAKE_BUILDER"
                            | "AWS_LC_SYS_CMAKE_BUILDER_x86_64_unknown_linux_gnu"
                    )
                    && value == "0")
        }) {
            return Err(composition_error(
                "AWS-LC has an unreviewed environment selection",
            ));
        }
    }
    if name == "libsqlite3-sys" {
        if inputs
            .environment
            .get("LIBSQLITE3_SYS_USE_PKG_CONFIG")
            .map(String::as_str)
            != Some("0")
        {
            return Err(composition_error(
                "SQLite system discovery is not explicitly disabled",
            ));
        }
        if inputs.environment.keys().any(|key| {
            key.starts_with("SQLITE3_")
                || key.starts_with("SQLCIPHER_")
                || key.starts_with("LIBSQLITE3_") && key != "LIBSQLITE3_SYS_USE_PKG_CONFIG"
        }) {
            return Err(composition_error(
                "SQLite has an unreviewed environment selection",
            ));
        }
    }
    Ok(())
}

fn features_for(
    observations: &CargoObservations,
    package: &RustPackage,
    script: &BuildScriptObservation,
    rule: &ReviewedNative,
) -> Result<Vec<String>, Report<ReleaseError>> {
    let mut matches = observations.units.iter().filter(|unit| {
        unit.package_id == script.package_id
            && unit.context == script.context
            && !unit.target_kind.iter().any(|kind| kind == "custom-build")
    });
    let first = matches
        .next()
        .ok_or_else(|| composition_error("native script has no observed consuming unit"))?;
    let conflicting = matches.any(|unit| unit.features != first.features);
    let unreviewed = first
        .features
        .iter()
        .any(|feature| !rule.allowed_features.contains(&feature.as_str()));
    let missing = rule
        .required_features
        .iter()
        .any(|feature| !first.features.iter().any(|actual| actual == feature));
    if conflicting || unreviewed || missing {
        let observed: Vec<String> = first
            .features
            .iter()
            .take(16)
            .map(|feature| feature.chars().take(96).collect())
            .collect();
        return Err(composition_error(format!(
            "native package {}@{} {:?}: conflicting_units={conflicting}, unreviewed_features={unreviewed}, missing_required_features={missing}; observed={observed:?} ({} total); required={:?}; allowed={:?}",
            package.name.chars().take(96).collect::<String>(),
            package.version.chars().take(96).collect::<String>(),
            script.context,
            first.features.len(),
            rule.required_features,
            rule.allowed_features,
        )));
    }
    Ok(first.features.clone())
}

fn search_paths(
    script: &BuildScriptObservation,
    inputs: &NativeInputs<'_>,
) -> Result<Vec<(PathBuf, PathBuf)>, Report<ReleaseError>> {
    let release = match script.context {
        UnitContext::Host => inputs.execution_output_root.join("release"),
        UnitContext::Target => inputs
            .execution_output_root
            .join(inputs.target.triple())
            .join("release"),
    };
    normal_path(&script.out_dir)?;
    let relative = script
        .out_dir
        .strip_prefix(release.join("build"))
        .map_err(composition_error)?;
    let components: Vec<_> = relative.components().collect();
    if components.len() != 2 || components[1].as_os_str() != "out" {
        return Err(composition_error(
            "native OUT_DIR differs from the selected Cargo layout",
        ));
    }
    let mut paths = BTreeMap::new();
    for linked in &script.linked_paths {
        let path = PathBuf::from(
            linked
                .strip_prefix("native=")
                .ok_or_else(|| composition_error("unreviewed native search directive"))?,
        );
        normal_path(&path)?;
        if !path.starts_with(&script.out_dir) {
            return Err(composition_error(
                "native search path escapes the observed OUT_DIR",
            ));
        }
        let relative = path
            .strip_prefix(inputs.execution_output_root)
            .map_err(composition_error)?;
        let host = inputs.host_output_root.join(relative);
        real_directory(&host)?;
        paths.insert(path, host);
    }
    if paths.is_empty() {
        return Err(composition_error(
            "native script has no observed archive search path",
        ));
    }
    Ok(paths.into_iter().collect())
}

fn contributor(
    observations: &CargoObservations,
    package: &RustPackage,
    script: &BuildScriptObservation,
    inputs: &NativeInputs<'_>,
) -> Result<NativeContributor, Report<ReleaseError>> {
    let rule = reviewed(&package.name)
        .ok_or_else(|| composition_error("unreviewed native package origin"))?;
    if package.version != rule.version {
        return Err(composition_error(
            "native package version has not been reviewed",
        ));
    }
    check_environment(&package.name, inputs)?;
    let features = features_for(observations, package, script, &rule)?;
    let source = inputs
        .origins
        .get(&package.id)
        .ok_or_else(|| composition_error("native package has no verified exact-ID original"))?;
    if !lower_hex(&source.original_sha256, 64) {
        return Err(composition_error("native original has no SHA-256 identity"));
    }
    let expected: BTreeSet<_> = rule
        .libraries
        .iter()
        .map(|name| format!("static={name}"))
        .collect();
    if script.linked_libs.iter().collect::<BTreeSet<_>>() != expected.iter().collect()
        || script.linked_libs.len() != expected.len()
    {
        return Err(composition_error(
            "native link directives differ from reviewed static archives",
        ));
    }
    let searches = search_paths(script, inputs)?;
    let mut archives = Vec::new();
    for library in rule.libraries {
        let filename = format!("lib{library}.a");
        let mut candidates = Vec::new();
        for (execution, host) in &searches {
            let host_path = host.join(&filename);
            match fs::symlink_metadata(&host_path) {
                Ok(_) => candidates.push((execution.join(&filename), host_path)),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(composition_error(error)),
            }
        }
        let [(path, host_path)] = candidates.as_slice() else {
            return Err(composition_error(
                "native archive is missing or ambiguous across search paths",
            ));
        };
        let identity = archive_digest(inputs.host_output_root, host_path, inputs.check)?;
        archives.push(NativeArchive {
            path: path.clone(),
            size: identity.size,
            sha256: identity.sha256,
            host_path: host_path.clone(),
            output_root: inputs.host_output_root.to_owned(),
        });
    }
    let source_tree_sha256 = source_digest(&source.source_root, inputs.check)?;
    let source_subtrees = rule
        .source_subtrees
        .iter()
        .map(|path| {
            source_digest(&source.source_root.join(path), inputs.check)
                .map(|sha256| SourceSubtree { path, sha256 })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(NativeContributor {
        package_id: package.id.clone(),
        name: package.name.clone(),
        version: package.version.clone(),
        context: script.context,
        features,
        classification: match script.context {
            UnitContext::Host => "build_tool_native_contributor",
            UnitContext::Target => "conservative_embedded_contributor",
        },
        original_sha256: source.original_sha256.clone(),
        source_tree_sha256,
        source_subtrees,
        linked_libs: script.linked_libs.clone(),
        linked_paths: script.linked_paths.clone(),
        archives,
        source_root: source.source_root.clone(),
    })
}

/// Captures reviewed native outputs and their verified original sources.
///
/// # Errors
///
/// Rejects unknown native origins, features, directives or substitutions, unsafe
/// paths and absent outputs. Toolchain and final platform evidence are separate.
pub(crate) fn collect(
    observations: &CargoObservations,
    inputs: &NativeInputs<'_>,
) -> Result<NativeInventory, Report<ReleaseError>> {
    (inputs.check)()?;
    normal_path(inputs.execution_output_root)?;
    real_directory(inputs.host_output_root)?;
    let mut contributors = Vec::new();
    let mut captured = BTreeSet::new();
    for script in &observations.build_scripts {
        (inputs.check)()?;
        let package = observations
            .packages
            .iter()
            .find(|package| package.id == script.package_id)
            .ok_or_else(|| composition_error("native script has no observed package"))?;
        if reviewed(&package.name).is_some()
            || !script.linked_libs.is_empty()
            || !script.linked_paths.is_empty()
        {
            if !captured.insert((&script.package_id, script.context == UnitContext::Target)) {
                return Err(composition_error("native script context is duplicated"));
            }
            contributors.push(contributor(observations, package, script, inputs)?);
        }
    }
    for unit in &observations.units {
        let package = observations
            .packages
            .iter()
            .find(|package| package.id == unit.package_id)
            .ok_or_else(|| composition_error("native unit has no observed package"))?;
        if reviewed(&package.name).is_some()
            && !unit.target_kind.iter().any(|kind| kind == "custom-build")
            && !captured.contains(&(&unit.package_id, unit.context == UnitContext::Target))
        {
            return Err(composition_error(
                "observed native package has no captured build-script execution",
            ));
        }
    }
    let build_tool_package_ids = observations
        .units
        .iter()
        .filter(|unit| unit.context == UnitContext::Host)
        .map(|unit| unit.package_id.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    (inputs.check)()?;
    Ok(NativeInventory {
        contributors,
        build_tool_package_ids,
        material_native_coverage_complete: false,
    })
}

impl NativeInventory {
    /// Rechecks the native source trees and actual output archives before handoff.
    ///
    /// # Errors
    ///
    /// Rejects changed or aliased files, unsafe paths and an expired attempt.
    pub(crate) fn rehash(
        &self,
        check: &dyn Fn() -> Result<(), Report<ReleaseError>>,
    ) -> Result<(), Report<ReleaseError>> {
        check()?;
        for contributor in &self.contributors {
            if source_digest(&contributor.source_root, check)? != contributor.source_tree_sha256 {
                return Err(composition_error(
                    "native prepared source tree changed after capture",
                ));
            }
            for archive in &contributor.archives {
                let identity = archive_digest(&archive.output_root, &archive.host_path, check)?;
                if identity.sha256 != archive.sha256 || identity.size != archive.size {
                    return Err(composition_error(
                        "native output archive changed after capture",
                    ));
                }
            }
        }
        check()
    }
}

#[cfg(test)]
mod tests;

impl NativeInventory {
    /// Returns measured native inputs without granting coverage completeness.
    pub(crate) fn contributors(&self) -> &[NativeContributor] {
        &self.contributors
    }
}

impl NativeContributor {
    /// Returns the exact observed Cargo package ID for joining only.
    pub(crate) fn package_id(&self) -> &str {
        &self.package_id
    }
    /// Returns the observed Cargo wrapper package name.
    pub(crate) fn name(&self) -> &str {
        &self.name
    }
    /// Returns the observed Cargo wrapper package version.
    pub(crate) fn version(&self) -> &str {
        &self.version
    }
    /// Returns its observed consuming compiler context.
    pub(crate) fn context(&self) -> UnitContext {
        self.context
    }
    /// Returns effective consuming-unit features.
    pub(crate) fn features(&self) -> &[String] {
        &self.features
    }
    /// Returns its conservative collector classification.
    pub(crate) fn classification(&self) -> &str {
        self.classification
    }
    /// Returns the verified original registry archive digest.
    pub(crate) fn original_sha256(&self) -> &str {
        &self.original_sha256
    }
    /// Returns structured prepared-source tree evidence, not a raw-file checksum.
    pub(crate) fn source_tree_sha256(&self) -> &str {
        &self.source_tree_sha256
    }
    /// Returns reviewed structured native source subtree observations.
    pub(crate) fn source_subtrees(&self) -> &[SourceSubtree] {
        &self.source_subtrees
    }
    /// Returns observed reviewed static-link library directives.
    pub(crate) fn linked_libs(&self) -> &[String] {
        &self.linked_libs
    }
    /// Returns measured static output archives with private paths inaccessible.
    pub(crate) fn archives(&self) -> &[NativeArchive] {
        &self.archives
    }
    /// Returns whether this wrapper name and version have reviewed native rules.
    pub(crate) fn reviewed_wrapper(&self) -> bool {
        reviewed(&self.name).is_some_and(|rule| rule.version == self.version)
    }
}

impl SourceSubtree {
    /// Returns the reviewed source-relative subtree path.
    pub(crate) fn path(&self) -> &str {
        self.path
    }
    /// Returns structured subtree evidence, not a raw-file checksum.
    pub(crate) fn sha256(&self) -> &str {
        &self.sha256
    }
}

impl NativeArchive {
    /// Returns the logical archive filename without exposing output roots.
    pub(crate) fn filename(&self) -> Option<&str> {
        self.path.file_name().and_then(|name| name.to_str())
    }
    /// Returns measured static archive length.
    pub(crate) fn size(&self) -> u64 {
        self.size
    }
    /// Returns the raw static-output archive digest, distinct from its source.
    pub(crate) fn sha256(&self) -> &str {
        &self.sha256
    }
}

#[cfg(test)]
pub(crate) fn fixture_for_projection(
    name: &str,
    context: UnitContext,
) -> (NativeInventory, CargoObservations) {
    tests::fixture_for_projection(name, context)
}

/// Returns the reviewed Cargo wrapper version for a native implementation.
pub(crate) fn reviewed_wrapper_version(name: &str) -> Option<&'static str> {
    reviewed(name).map(|rule| rule.version)
}
