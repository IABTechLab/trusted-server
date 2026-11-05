//! Cargo directory-source vendoring from verified originals.
//!
//! Registry crates come from archives already authenticated against the
//! approved `Cargo.lock` checksum and extracted under the archive policy.
//! Git packages come from a verified object closure. Each registry crate
//! becomes `vendor/<name>-<version>/` and each Git package `vendor/<name>/`
//! (so sibling paths inside its own workspace still resolve), with a
//! producer-written `.cargo-checksum.json` (see [`writer`]). The result is a
//! [`PreparedCargoMaterials`] token whose digest covers the whole vendor tree.
//!
//! # Normalized Git manifests
//!
//! A Git package whose manifest inherits from its workspace
//! (`version.workspace = true`) cannot be used from a directory source as it
//! is. The producer resolves that inheritance itself (see [`normalize`]) from
//! the package manifest and the enclosing workspace root manifest of the same
//! verified tree, writes the result as `Cargo.toml` and keeps the committed
//! manifest as `Cargo.toml.orig`. No caller-supplied manifest is accepted.
//! The normalization is a reviewed producer transformation: its digests are
//! recorded in [`PreparedCargoMaterials::normalized_manifest_digests`] and do
//! not by themselves make a build unqualified.

#[cfg(test)]
mod differential;
mod git_tree;
mod index;
mod normalize;
mod source;
mod writer;

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, DirBuilder};
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _};
use std::path::{Path, PathBuf};
use std::time::Instant;

use error_stack::Report;
use sha2::{Digest as _, Sha256};

use super::git::VerifiedGitMaterial;
use super::locked::CARGO_REGISTRY;
use super::locked::GitPackage;
use super::{LockedMaterials, material_error};
use crate::error::ReleaseError;
use crate::process::Cancellation;
use crate::tool_tree::tool_tree_digest;
#[cfg(all(test, target_os = "linux"))]
pub(crate) use differential::resolution_differences;
use git_tree::{Store, ancestor_manifests, find_package, materialize};
pub(crate) use index::{CargoOrigin, VerifiedCargoPackage};
use normalize::{PackageManifest, check_target_paths, normalize_manifest};
pub(crate) use source::GitSourceConfig;
use writer::PackageWriter;

/// Name of the directory below the materials root that holds every package.
pub(crate) const VENDOR_DIRECTORY: &str = "vendor";
/// Largest manifest parsed.
const MANIFEST_LIMIT: u64 = 1024 * 1024;
/// Largest single vendored file.
const FILE_LIMIT: u64 = 128 * 1024 * 1024;
/// Most files copied from one extracted crate.
const FILE_COUNT_LIMIT: usize = 100_000;

/// A verified Git closure for one locked Git package.
///
/// It carries no manifest: every vendored manifest comes from the verified
/// tree, normalized by the producer when it inherits from its workspace.
pub(crate) struct GitVendorInput<'a> {
    /// The verified closure for the package's locked source and revision.
    pub(crate) material: &'a VerifiedGitMaterial,
    /// Locked package name.
    pub(crate) name: &'a str,
    /// Locked package version.
    pub(crate) version: &'a str,
}

/// Every original the vendor tree is prepared from.
pub(crate) struct VendorInputs<'a> {
    /// Registry archive paths by exact locked `(name, version)`.
    pub(crate) registry_archives: &'a BTreeMap<(String, String), PathBuf>,
    /// One verified closure per locked Git package.
    pub(crate) git: &'a [GitVendorInput<'a>],
}

/// A prepared, digest-bound vendor tree.
///
/// Only [`LockedMaterials::prepare_cargo_vendor`] constructs it outside tests.
#[derive(Debug)]
pub(crate) struct PreparedCargoMaterials {
    root: PathBuf,
    vendor: PathBuf,
    digest: String,
    source_commit: String,
    cargo_lock_sha256: String,
    git_sources: Vec<GitSourceConfig>,
    normalized_manifests: BTreeMap<String, String>,
    derived_manifests: BTreeMap<String, String>,
    packages: BTreeMap<String, VerifiedCargoPackage>,
}

impl PreparedCargoMaterials {
    /// Verified original identities, keyed by the prepared vendor directory.
    ///
    /// This includes prepared but uncompiled dependencies. Observed compiler
    /// units must be joined separately before claiming artifact membership.
    pub(crate) fn packages(&self) -> &BTreeMap<String, VerifiedCargoPackage> {
        &self.packages
    }

    /// The materials root that holds only the vendor directory.
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    /// The directory-source root passed to Cargo.
    pub(crate) fn vendor_dir(&self) -> &Path {
        &self.vendor
    }

    /// Digest of every vendored path, mode, size and content.
    pub(crate) fn digest(&self) -> &str {
        &self.digest
    }

    /// The approved source commit whose lock selected these originals.
    pub(crate) fn source_commit(&self) -> &str {
        &self.source_commit
    }

    /// SHA-256 of the approved raw `Cargo.lock`.
    pub(crate) fn cargo_lock_sha256(&self) -> &str {
        &self.cargo_lock_sha256
    }

    /// Distinct locked Git sources, sorted by key, for source replacement.
    pub(crate) fn git_sources(&self) -> &[GitSourceConfig] {
        &self.git_sources
    }

    /// Digests of the manifests the producer normalized, by vendor directory.
    ///
    /// Each was derived by [`normalize`] from the verified tree alone, a
    /// reviewed transformation; it does not make the build unqualified.
    pub(crate) fn normalized_manifest_digests(&self) -> &BTreeMap<String, String> {
        &self.normalized_manifests
    }

    /// Digests of caller-derived manifests, by vendor directory.
    ///
    /// [`LockedMaterials::prepare_cargo_vendor`] accepts no caller-supplied
    /// manifest, so it always leaves this empty. It stays part of the token
    /// so any caller-derived manifest, such as one a test constructs, still
    /// marks the build unqualified.
    pub(crate) fn derived_manifest_digests(&self) -> &BTreeMap<String, String> {
        &self.derived_manifests
    }

    /// Recomputes the digest and requires the root to hold only the vendor tree.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Material`] when anything under the root was
    /// edited, added, removed or linked since preparation.
    pub(crate) fn rehash(&self) -> Result<(), Report<ReleaseError>> {
        let names = fs::read_dir(&self.root)
            .map_err(material_error)?
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(material_error)?;
        if names != [std::ffi::OsString::from(VENDOR_DIRECTORY)] {
            return Err(material_error(
                "materials root holds something other than the vendor directory",
            ));
        }
        if tool_tree_digest(&self.vendor, &|| Ok(()))? != self.digest {
            return Err(material_error(
                "vendored materials changed after preparation",
            ));
        }
        Ok(())
    }

    /// Records a derived manifest digest on a test token.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_derived_for_tests(mut self, directory: &str, digest: &str) -> Self {
        self.derived_manifests
            .insert(directory.to_owned(), digest.to_owned());
        self
    }

    /// Records a producer-normalized manifest digest on a test token.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_normalized_for_tests(mut self, directory: &str, digest: &str) -> Self {
        self.normalized_manifests
            .insert(directory.to_owned(), digest.to_owned());
        self
    }

    /// Builds a token over an existing `root/vendor`, for tests only.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Material`] when the vendor tree cannot be
    /// digested or a selector is not a supported Git source.
    #[cfg(test)]
    pub(crate) fn for_tests(
        root: &Path,
        source_commit: &str,
        git_selectors: &[&str],
    ) -> Result<Self, Report<ReleaseError>> {
        let vendor = root.join(VENDOR_DIRECTORY);
        match fs::create_dir(&vendor) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(material_error(error)),
        }
        Ok(Self {
            root: root.to_owned(),
            digest: tool_tree_digest(&vendor, &|| Ok(()))?,
            vendor,
            source_commit: source_commit.to_owned(),
            cargo_lock_sha256: "b".repeat(64),
            git_sources: git_selectors
                .iter()
                .map(|selector| GitSourceConfig::parse(selector))
                .collect::<Result<_, _>>()?,
            normalized_manifests: BTreeMap::new(),
            derived_manifests: BTreeMap::new(),
            packages: BTreeMap::new(),
        })
    }
}

fn attempt_check(
    deadline: Instant,
    cancellation: &Cancellation,
) -> Result<(), Report<ReleaseError>> {
    if cancellation.is_cancelled() || Instant::now() >= deadline {
        return Err(material_error("Cargo vendoring cancelled or timed out"));
    }
    Ok(())
}

/// Requires a vendor directory name that cannot escape or hide.
fn plain_directory(directory: String) -> Result<String, Report<ReleaseError>> {
    let plain = !directory.is_empty()
        && !directory.starts_with('.')
        && directory
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'+'));
    if !plain {
        return Err(material_error(format!(
            "package identity {directory:?} is not a safe vendor directory name"
        )));
    }
    Ok(directory)
}

/// Names a registry crate's vendor directory `<name>-<version>`.
fn directory_name(name: &str, version: &str) -> Result<String, Report<ReleaseError>> {
    plain_directory(format!("{name}-{version}"))
}

/// Names a Git package's vendor directory after the package alone.
///
/// This is the layout `cargo vendor` gives a unique name, and it keeps paths
/// between sibling members of the package's own workspace resolving: a
/// package that reads `../<sibling>/...` from its manifest directory, as a
/// checkout allows, finds that sibling in the vendor directory too. A
/// registry crate or second Git package with the same directory name still
/// fails closed, because every vendor directory is created exclusively.
fn git_directory_name(name: &str) -> Result<String, Report<ReleaseError>> {
    plain_directory(name.to_owned())
}

/// Requires a real, empty directory that no symlink aliases.
fn check_output_root(root: &Path) -> Result<(), Report<ReleaseError>> {
    let metadata = fs::symlink_metadata(root).map_err(material_error)?;
    let resolved = fs::canonicalize(root).map_err(material_error)?;
    if !metadata.is_dir() || resolved.as_os_str() != root.as_os_str() {
        return Err(material_error(
            "materials root is not a real unaliased directory",
        ));
    }
    if fs::read_dir(root).map_err(material_error)?.next().is_some() {
        return Err(material_error("materials root already has entries"));
    }
    Ok(())
}

/// Parses a manifest of at most one MiB into a TOML table.
fn parse_manifest(bytes: &[u8]) -> Result<toml::Table, Report<ReleaseError>> {
    if bytes.len() as u64 > MANIFEST_LIMIT {
        return Err(material_error("package manifest exceeds the size limit"));
    }
    let text = std::str::from_utf8(bytes).map_err(material_error)?;
    toml::from_str(text).map_err(material_error)
}

/// Returns the declared `(name, version)`; the version is `None` when it is
/// not a plain string (inherited from a workspace).
fn declared_identity(table: &toml::Table) -> Option<(String, Option<String>)> {
    let package = table.get("package")?;
    let name = package.get("name")?.as_str()?.to_owned();
    let version = package
        .get("version")
        .and_then(toml::Value::as_str)
        .map(str::to_owned);
    Some((name, version))
}

/// Reports whether any table below `value` has `workspace = true`.
fn inherits_workspace(value: &toml::Value) -> bool {
    match value {
        toml::Value::Table(table) => {
            table.get("workspace") == Some(&toml::Value::Boolean(true))
                || table.values().any(inherits_workspace)
        }
        toml::Value::Array(values) => values.iter().any(inherits_workspace),
        _ => false,
    }
}

/// Requires a plain-string identity equal to the locked one.
fn require_identity(
    manifest: &[u8],
    name: &str,
    version: &str,
) -> Result<(), Report<ReleaseError>> {
    let table = parse_manifest(manifest)?;
    match declared_identity(&table) {
        Some((found_name, Some(found_version)))
            if found_name == name && found_version == version =>
        {
            Ok(())
        }
        _ => Err(material_error(format!(
            "manifest does not declare package {name} {version}"
        ))),
    }
}

/// Copies every file of an extracted crate into `writer`.
fn copy_extracted(root: &Path, writer: &mut PackageWriter) -> Result<(), Report<ReleaseError>> {
    let mut pending = vec![(root.to_owned(), String::new())];
    let mut count = 0_usize;
    while let Some((directory, prefix)) = pending.pop() {
        for entry in fs::read_dir(&directory).map_err(material_error)? {
            let entry = entry.map_err(material_error)?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| material_error("extracted member is not UTF-8"))?;
            let relative = format!("{prefix}{name}");
            let metadata = fs::symlink_metadata(entry.path()).map_err(material_error)?;
            if metadata.is_dir() {
                pending.push((entry.path(), format!("{relative}/")));
            } else if metadata.is_file() && metadata.len() <= FILE_LIMIT {
                count += 1;
                if count > FILE_COUNT_LIMIT {
                    return Err(material_error("extracted crate exceeds the file limit"));
                }
                let bytes = fs::read(entry.path()).map_err(material_error)?;
                writer.write(&relative, &bytes, metadata.mode() & 0o111 != 0)?;
            } else {
                return Err(material_error(
                    "extracted crate has an unsafe file type or an oversize file",
                ));
            }
        }
    }
    Ok(())
}

impl LockedMaterials {
    /// Requires the supplied inputs to be exactly the locked originals.
    fn check_vendor_inputs(&self, inputs: &VendorInputs<'_>) -> Result<(), Report<ReleaseError>> {
        let locked: BTreeSet<_> = self.cargo.registry.keys().collect();
        let supplied: BTreeSet<_> = inputs.registry_archives.keys().collect();
        if locked != supplied {
            return Err(material_error(
                "registry archives are not exactly the locked registry crates",
            ));
        }
        if inputs.git.len() != self.cargo.git_packages.len() {
            return Err(material_error(
                "Git inputs are not exactly the locked Git packages",
            ));
        }
        for package in &self.cargo.git_packages {
            self.matching_git_input(package, inputs)?;
        }
        Ok(())
    }

    /// Finds the single input for a locked Git package and checks its bindings.
    fn matching_git_input<'a>(
        &self,
        package: &GitPackage,
        inputs: &'a VendorInputs<'a>,
    ) -> Result<&'a GitVendorInput<'a>, Report<ReleaseError>> {
        let mut matching = inputs
            .git
            .iter()
            .filter(|input| input.name == package.name && input.version == package.version);
        let (Some(input), None) = (matching.next(), matching.next()) else {
            return Err(material_error(format!(
                "Git package {} {} needs exactly one input",
                package.name, package.version
            )));
        };
        let material = input.material;
        if material.selector() != package.selector
            || material.revision() != package.revision
            || material.source_commit() != self.source_commit
            || material.cargo_lock_sha256() != self.cargo_lock_sha256
        {
            return Err(material_error(format!(
                "Git closure for {} is not bound to its locked source, revision or approved lock",
                package.name
            )));
        }
        Ok(input)
    }

    /// Prepares `output_root/vendor` from verified originals.
    ///
    /// The root must be an empty real directory. On failure it may hold a
    /// partial tree and must be discarded.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Material`] for a non-fresh root, a missing,
    /// extra or unverifiable original, a manifest that does not declare the
    /// locked identity, an ambiguous or missing Git package root, a Git
    /// manifest whose workspace inheritance cannot be normalized, a repeated
    /// vendor directory, or an expired or cancelled attempt.
    pub(crate) fn prepare_cargo_vendor(
        &self,
        inputs: &VendorInputs<'_>,
        output_root: &Path,
        deadline: Instant,
        cancellation: &Cancellation,
    ) -> Result<PreparedCargoMaterials, Report<ReleaseError>> {
        attempt_check(deadline, cancellation)?;
        check_output_root(output_root)?;
        self.check_vendor_inputs(inputs)?;
        let sources: BTreeMap<String, GitSourceConfig> = self
            .cargo
            .git_packages
            .iter()
            .map(|package| {
                GitSourceConfig::parse(&package.selector)
                    .map(|source| (source.key().to_owned(), source))
            })
            .collect::<Result<_, _>>()?;
        let vendor = output_root.join(VENDOR_DIRECTORY);
        DirBuilder::new()
            .mode(0o755)
            .create(&vendor)
            .map_err(material_error)?;
        let mut packages = BTreeMap::new();
        for ((name, version), path) in inputs.registry_archives {
            attempt_check(deadline, cancellation)?;
            let identity =
                self.vendor_registry(name, version, path, &vendor, deadline, cancellation)?;
            packages.insert(directory_name(name, version)?, identity);
        }
        let mut normalized = BTreeMap::new();
        for package in &self.cargo.git_packages {
            attempt_check(deadline, cancellation)?;
            let input = self.matching_git_input(package, inputs)?;
            let identity = vendor_git(package, input, &vendor)?;
            if let CargoOrigin::Git {
                normalized_manifest_sha256: Some(digest),
                ..
            } = &identity.origin
            {
                normalized.insert(git_directory_name(&package.name)?, digest.clone());
            }
            packages.insert(git_directory_name(&package.name)?, identity);
        }
        attempt_check(deadline, cancellation)?;
        Ok(PreparedCargoMaterials {
            root: output_root.to_owned(),
            digest: tool_tree_digest(&vendor, &|| attempt_check(deadline, cancellation))?,
            vendor,
            source_commit: self.source_commit.clone(),
            cargo_lock_sha256: self.cargo_lock_sha256.clone(),
            git_sources: sources.into_values().collect(),
            normalized_manifests: normalized,
            derived_manifests: BTreeMap::new(),
            packages,
        })
    }

    fn vendor_registry(
        &self,
        name: &str,
        version: &str,
        path: &Path,
        vendor: &Path,
        deadline: Instant,
        cancellation: &Cancellation,
    ) -> Result<VerifiedCargoPackage, Report<ReleaseError>> {
        let directory = directory_name(name, version)?;
        let checksum = self
            .cargo
            .registry
            .get(&(name.to_owned(), version.to_owned()))
            .map(|material| material.sha256.clone())
            .ok_or_else(|| material_error("registry crate is not locked"))?;
        let original = self.verify_registry(name, version, path, deadline, cancellation)?;
        let extracted = self.extract_registry(name, version, original)?;
        extracted.verify_unchanged()?;
        let manifest = extracted.read_member_checked("Cargo.toml")?;
        require_identity(&manifest, name, version)?;
        let mut writer = PackageWriter::create(vendor, &directory)?;
        copy_extracted(extracted.root(), &mut writer)?;
        extracted.verify_unchanged()?;
        let files = writer.finish(Some(&checksum))?;
        let prepared_manifest_sha256 = files
            .get("Cargo.toml")
            .ok_or_else(|| material_error("prepared registry manifest is missing"))?
            .clone();
        Ok(VerifiedCargoPackage {
            name: name.to_owned(),
            version: version.to_owned(),
            source: CARGO_REGISTRY.to_owned(),
            prepared_manifest_sha256,
            origin: CargoOrigin::Registry {
                archive_sha256: checksum,
            },
        })
    }
}

/// Vendors one Git package and records its verified original and prepared identity.
fn vendor_git(
    package: &GitPackage,
    input: &GitVendorInput<'_>,
    vendor: &Path,
) -> Result<VerifiedCargoPackage, Report<ReleaseError>> {
    let directory = git_directory_name(&package.name)?;
    let store = Store::new(input.material.object_store());
    let root = find_package(&store, input.material.root_tree(), &package.name)?;
    let committed = store.read("blob", &root.manifest, MANIFEST_LIMIT)?;
    let committed_manifest_sha256 = hex::encode(Sha256::digest(&committed));
    let table = parse_manifest(&committed)?;
    check_target_paths(&table)?;
    match declared_identity(&table) {
        Some((name, _)) if name != package.name => {
            return Err(material_error(format!(
                "Git manifest declares package {name}, not {}",
                package.name
            )));
        }
        Some((_, Some(version))) if version != package.version => {
            return Err(material_error(format!(
                "Git manifest declares {} {version}, not {} {}",
                package.name, package.name, package.version
            )));
        }
        Some(_) => {}
        None => return Err(material_error("Git manifest declares no package")),
    }
    let (manifest, original) = if inherits_workspace(&toml::Value::Table(table)) {
        let ancestors = ancestor_manifests(&store, input.material.root_tree(), &root)?;
        let normalized = normalize_manifest(
            &PackageManifest {
                name: &package.name,
                version: &package.version,
                directory: &root.path,
                bytes: &committed,
            },
            &ancestors,
        )?;
        (normalized, Some(committed))
    } else {
        (committed, None)
    };
    let mut writer = PackageWriter::create(vendor, &directory)?;
    materialize(&store, input.material.root_tree(), &root, &mut writer)?;
    writer.write("Cargo.toml", &manifest, false)?;
    if let Some(original) = &original {
        writer.write("Cargo.toml.orig", original, false)?;
    }
    let normalized = original
        .as_ref()
        .and_then(|_| writer.digest_of("Cargo.toml").map(str::to_owned));
    let files = writer.finish(None)?;
    let prepared_manifest_sha256 = files
        .get("Cargo.toml")
        .ok_or_else(|| material_error("prepared Git manifest is missing"))?
        .clone();
    Ok(VerifiedCargoPackage {
        name: package.name.clone(),
        version: package.version.clone(),
        source: format!("{}#{}", package.selector, package.revision),
        prepared_manifest_sha256,
        origin: CargoOrigin::Git {
            revision: package.revision.clone(),
            repository_tree: input.material.root_tree().to_owned(),
            package_tree: root.tree,
            package_path: root.path,
            committed_manifest_blob: root.manifest,
            committed_manifest_sha256,
            normalized_manifest_sha256: normalized,
        },
    })
}

#[cfg(test)]
mod tests;

#[cfg(test)]
impl PreparedCargoMaterials {
    /// Adds an identity fixture without bypassing whole-tree rehashing.
    pub(crate) fn with_package_for_tests(
        mut self,
        directory: &str,
        package: VerifiedCargoPackage,
    ) -> Self {
        self.packages.insert(directory.to_owned(), package);
        self
    }
}
