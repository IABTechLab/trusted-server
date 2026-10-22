//! The Apple developer tools a Darwin recipe compiles and links C with.
//!
//! The developer directory is a host system root: the macOS boundary allows
//! only reading and executing below it and never scans it. Its identity is
//! therefore this explicit record: the developer directory, the selected SDK,
//! `clang`, `ar` and `ld` selected directly from the default toolchain (never
//! through `xcrun` or the `/usr/bin` shims), the Xcode and SDK versions, and
//! the SHA-256 of the three tools and of `SDKSettings.json`. The record is
//! rehashed at binding and immediately before and after the confined run.
//!
//! # Identity scope
//!
//! Ordinary CI selection independently copies the selected SDK into an owned,
//! bounded tree and binds its digest into this record. Headers, `.tbd` stubs,
//! frameworks, modes and internal aliases in that selected copy are rehashed
//! before and after execution and at evidence sealing. Explicit lexical test
//! fixtures retain only the four-file record and never claim SDK coverage.
//!
//! Ordinary CI selection also measures, in place below the selected `clang`'s
//! toolchain, tree digests of the helper directory, the entire selected
//! `usr/lib` directory and the single versioned clang resource directory,
//! plus the SHA-256 of `libLTO.dylib` and
//! optional `libtapi.dylib`, and rechecks them at every verification point.
//! Which helpers clang actually ran, the dependency/load closure, libraries
//! outside the selected measured directories, host runtime and original
//! acquisition remain unresolved. The developer directory still permits broad
//! read/execute access; neither receipt proves every compiler input came from
//! the measured trees.
//!
//! Only the test-only ordinary CI selection constructs a record; the
//! qualified selection is uninhabited until Task 8 qualifies a toolchain.

use std::fs::File;
use std::io::Read as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
#[cfg(test)]
use std::time::Instant;

use error_stack::Report;
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use super::artifact::EXECUTABLE_LIMIT;
use super::fresh::{normal_absolute, real_directory};
use super::recipe_error;
use super::tools::Authority;
use crate::documents::{Role, canonical_json};
use crate::error::ReleaseError;
use crate::inspect::MacosVersion;
#[cfg(test)]
use crate::process::Cancellation;
use sdk::{OwnedSdk, SdkReceipt};
use toolchain::{MeasuredToolchain, ToolchainReceipt};

mod sdk;
mod toolchain;

/// Fixed prefix the C path remap gives the developer directory.
pub(crate) const REMAPPED_DEVELOPER: &str = "/apple-developer";
/// Fixed prefix for a separately owned SDK snapshot.
pub(crate) const REMAPPED_SDK: &str = "/apple-sdk";
/// Default toolchain binary directory below the developer directory.
const TOOLCHAIN_BIN: &str = "Toolchains/XcodeDefault.xctoolchain/usr/bin";
/// macOS SDK alias below the developer directory.
const MACOS_SDK: &str = "Platforms/MacOSX.platform/Developer/SDKs/MacOSX.sdk";
/// SDK settings file whose `Version` is the measured SDK version.
const SDK_SETTINGS: &str = "SDKSettings.json";
/// Largest SDK settings or version file read.
const SETTINGS_LIMIT: u64 = 1024 * 1024;
/// Longest accepted Xcode version text.
const MAX_XCODE_VERSION: usize = 32;
/// File read size while hashing a tool.
const HASH_CHUNK: usize = 64 * 1024;

/// Apple developer tools that passed Task 8 qualification.
///
/// None have, so this type is uninhabited and production selection stays closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum QualifiedAppleTools {}

/// How the developer tools were selected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AppleSelection {
    /// A qualified production selection.
    Qualified(QualifiedAppleTools),
    /// An ordinary CI Xcode; test builds only.
    #[cfg(test)]
    OrdinaryCi,
}

/// Explicit facts describing one Apple developer directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AppleToolFacts {
    /// Canonical developer directory, the only system root.
    pub(crate) developer_dir: PathBuf,
    /// Canonical macOS SDK root below the developer directory.
    pub(crate) sdk_root: PathBuf,
    /// The C compiler and linker driver.
    pub(crate) clang: PathBuf,
    /// The archiver.
    pub(crate) ar: PathBuf,
    /// The linker `clang` runs from its own directory.
    pub(crate) ld: PathBuf,
    /// Measured Xcode version, such as `16.4`.
    pub(crate) xcode_version: String,
    /// Measured SDK version from `SDKSettings.json`.
    pub(crate) sdk_version: MacosVersion,
    /// SHA-256 of `clang`.
    pub(crate) clang_sha256: String,
    /// SHA-256 of `ar`.
    pub(crate) ar_sha256: String,
    /// SHA-256 of `ld`.
    pub(crate) ld_sha256: String,
    /// SHA-256 of the SDK's `SDKSettings.json`.
    pub(crate) sdk_settings_sha256: String,
}

/// A checked Apple developer tool record bound to its selection.
#[derive(Clone, Debug)]
pub(crate) struct AppleDeveloperTools {
    facts: AppleToolFacts,
    selection: AppleSelection,
    sdk: Option<Arc<OwnedSdk>>,
    toolchain: Option<MeasuredToolchain>,
}

/// Candidate identity of the explicitly measured Apple files.
///
/// The exclusions describe input classes with unresolved coverage. A detached
/// SDK receipt is present only for a measured owned snapshot, and a
/// toolchain receipt only for a measured in-place toolchain.
#[derive(Debug, Serialize)]
pub(crate) struct AppleToolEvidence {
    identity_sha256: String,
    xcode_version: String,
    sdk_version: String,
    tools: [AppleFileEvidence; 3],
    sdk_settings: AppleFileEvidence,
    sdk_tree: Option<SdkReceipt>,
    toolchain: Option<ToolchainReceipt>,
    coverage_exclusions: Vec<&'static str>,
    origin: &'static str,
    qualification: &'static str,
}

#[derive(Debug, Serialize)]
struct AppleFileEvidence {
    role: &'static str,
    path: PathBuf,
    sha256: String,
}

impl AppleToolEvidence {
    /// Borrows measured tool roles and raw-file digests, excluding private paths.
    pub(crate) fn tool_files(&self) -> impl Iterator<Item = (&str, &str)> {
        self.tools
            .iter()
            .map(|file| (file.role, file.sha256.as_str()))
    }
    /// Returns the SDK settings raw-file digest.
    pub(crate) fn sdk_settings_sha256(&self) -> &str {
        &self.sdk_settings.sha256
    }
    /// Returns the detached SDK's structured-tree digest, when measured.
    pub(crate) fn sdk_tree_sha256(&self) -> Option<&str> {
        self.sdk_tree.as_ref().map(SdkReceipt::tree_sha256)
    }
    /// Returns the in-place `libLTO.dylib` raw-file digest, when measured.
    pub(crate) fn liblto_sha256(&self) -> Option<&str> {
        self.toolchain.as_ref().map(ToolchainReceipt::liblto_sha256)
    }
    /// Returns the in-place `libtapi.dylib` raw-file digest, when present and measured.
    pub(crate) fn libtapi_sha256(&self) -> Option<&str> {
        self.toolchain
            .as_ref()
            .and_then(ToolchainReceipt::libtapi_sha256)
    }
    /// Returns the clang resource directory's structured-tree digest, when measured.
    pub(crate) fn clang_resource_tree_sha256(&self) -> Option<&str> {
        self.toolchain
            .as_ref()
            .map(ToolchainReceipt::clang_resource_tree_sha256)
    }
    /// Returns the toolchain helper directory's structured-tree digest, when measured.
    pub(crate) fn helper_tree_sha256(&self) -> Option<&str> {
        self.toolchain
            .as_ref()
            .map(ToolchainReceipt::helper_tree_sha256)
    }
    /// Returns the selected toolchain library directory's structured-tree digest, when measured.
    pub(crate) fn library_tree_sha256(&self) -> Option<&str> {
        self.toolchain
            .as_ref()
            .map(ToolchainReceipt::library_tree_sha256)
    }
    /// Returns the SDK version used to check the accepted executable.
    pub(crate) fn sdk_version(&self) -> &str {
        &self.sdk_version
    }
    /// Returns the reviewed input classes with unresolved coverage.
    pub(crate) fn coverage_exclusions(&self) -> &[&'static str] {
        &self.coverage_exclusions
    }
}

fn lower_hex_digest(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Requires absolute normal text that can travel unquoted in `CFLAGS`, a
/// `-Wl,` list, a `PATH`-like value and a Cargo configuration string.
fn plain_path(label: &str, path: &Path) -> Result<(), Report<ReleaseError>> {
    normal_absolute(label, path)?;
    let plain = path.to_str().is_some_and(|text| {
        !text.contains([',', ':', '"', '\\', '='])
            && !text.chars().any(|c| c.is_whitespace() || c.is_control())
    });
    if !plain {
        return Err(recipe_error(format!(
            "{label} holds whitespace, a separator or a quote"
        )));
    }
    Ok(())
}

/// Requires `path` strictly below `developer_dir` through normal components.
fn below_developer(
    label: &str,
    path: &Path,
    developer_dir: &Path,
) -> Result<(), Report<ReleaseError>> {
    let below = path.strip_prefix(developer_dir).is_ok_and(|relative| {
        relative.components().next().is_some()
            && relative
                .components()
                .all(|component| matches!(component, Component::Normal(_)))
    });
    if !below {
        return Err(recipe_error(format!(
            "{label} is not inside the Apple developer directory"
        )));
    }
    Ok(())
}

/// Checks the facts lexically; nothing on disk is read.
fn check_facts(facts: &AppleToolFacts) -> Result<(), Report<ReleaseError>> {
    plain_path("Apple developer directory", &facts.developer_dir)?;
    let tools = [
        ("clang", &facts.clang),
        ("ar", &facts.ar),
        ("ld", &facts.ld),
    ];
    for (label, path) in [("Apple SDK root", &facts.sdk_root)]
        .into_iter()
        .chain(tools)
    {
        plain_path(label, path)?;
        below_developer(label, path, &facts.developer_dir)?;
    }
    let directory = facts.clang.parent();
    for (name, path) in tools {
        if path.file_name().and_then(|file| file.to_str()) != Some(name)
            || path.parent() != directory
        {
            return Err(recipe_error(format!(
                "Apple {name} must be named {name} beside clang in one toolchain directory"
            )));
        }
    }
    let version_ok = !facts.xcode_version.is_empty()
        && facts.xcode_version.len() <= MAX_XCODE_VERSION
        && facts
            .xcode_version
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'.');
    if !version_ok {
        return Err(recipe_error("Xcode version is not dotted decimal text"));
    }
    let digests = [
        &facts.clang_sha256,
        &facts.ar_sha256,
        &facts.ld_sha256,
        &facts.sdk_settings_sha256,
    ];
    if !digests.iter().all(|digest| lower_hex_digest(digest)) {
        return Err(recipe_error(
            "Apple tool digests must be 64 lowercase hex digits",
        ));
    }
    Ok(())
}

/// Hashes a regular file opened without following links, within `limit`.
fn hash_regular(label: &str, path: &Path, limit: u64) -> Result<String, Report<ReleaseError>> {
    let io = |error: std::io::Error| recipe_error(format!("{label} cannot be hashed: {error}"));
    let mut file = File::options()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(io)?;
    let metadata = file.metadata().map_err(io)?;
    if !metadata.is_file() || metadata.len() > limit {
        return Err(recipe_error(format!(
            "{label} is not a regular file within its size limit"
        )));
    }
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; HASH_CHUNK];
    let mut total = 0_u64;
    loop {
        let read = file.read(&mut buffer).map_err(io)?;
        let Some(chunk) = buffer.get(..read).filter(|chunk| !chunk.is_empty()) else {
            return Ok(hex::encode(hasher.finalize()));
        };
        total += chunk.len() as u64;
        if total > limit {
            return Err(recipe_error(format!("{label} grew past its size limit")));
        }
        hasher.update(chunk);
    }
}

impl AppleDeveloperTools {
    /// Exports typed candidate facts after rechecking the measured Apple files.
    ///
    /// # Errors
    ///
    /// Rejects changed tools or SDK settings, or invalid relative component paths.
    pub(crate) fn candidate_measurement(&self) -> Result<AppleToolEvidence, Report<ReleaseError>> {
        self.verify()?;
        let file = |role, path: &Path, sha256: &str| {
            Ok::<_, Report<ReleaseError>>(AppleFileEvidence {
                role,
                path: path
                    .strip_prefix(&self.facts.developer_dir)
                    .map_err(|error| {
                        recipe_error(format!(
                            "Apple component escapes developer directory: {error}"
                        ))
                    })?
                    .to_owned(),
                sha256: sha256.to_owned(),
            })
        };
        Ok(AppleToolEvidence {
            identity_sha256: self.identity_sha256()?,
            xcode_version: self.facts.xcode_version.clone(),
            sdk_version: self.facts.sdk_version.to_string(),
            tools: [
                file("compiler", &self.facts.clang, &self.facts.clang_sha256)?,
                file("archiver", &self.facts.ar, &self.facts.ar_sha256)?,
                file("linker", &self.facts.ld, &self.facts.ld_sha256)?,
            ],
            sdk_settings: if self.sdk.is_some() {
                AppleFileEvidence {
                    role: "sdk_settings",
                    path: PathBuf::from(SDK_SETTINGS),
                    sha256: self.facts.sdk_settings_sha256.clone(),
                }
            } else {
                file(
                    "sdk_settings",
                    &self.facts.sdk_root.join(SDK_SETTINGS),
                    &self.facts.sdk_settings_sha256,
                )?
            },
            sdk_tree: self.sdk.as_ref().map(|sdk| sdk.receipt().clone()),
            toolchain: self
                .toolchain
                .as_ref()
                .map(|toolchain| toolchain.receipt().clone()),
            coverage_exclusions: self.coverage_exclusions(),
            origin: "candidate_measurement",
            qualification: "unqualified",
        })
    }

    /// Names the reviewed input classes with unresolved coverage.
    ///
    /// The dependency/load closure and libraries outside selected measured
    /// directories remain unresolved, so the list is never empty.
    fn coverage_exclusions(&self) -> Vec<&'static str> {
        let mut exclusions = Vec::new();
        if self.sdk.is_none() {
            exclusions.extend(["sdk_headers", "sdk_stubs", "sdk_frameworks"]);
        }
        if self.toolchain.is_none() {
            exclusions.extend(["tool_helpers", "liblto", "clang_resource_directory"]);
        }
        exclusions.push("toolchain_libraries");
        exclusions
    }

    /// Accepts only a qualified selection; production construction stays closed.
    pub(crate) fn qualified(_facts: AppleToolFacts, qualification: QualifiedAppleTools) -> Self {
        match qualification {}
    }

    /// Records explicit facts as an ordinary CI selection without reading disk.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Recipe`] when a path is not absolute normal
    /// plain text, the SDK root or a tool is not strictly inside the
    /// developer directory, `clang`, `ar` and `ld` are not so named in one
    /// directory, the Xcode version is not dotted decimal, or a digest is not
    /// 64 lowercase hex digits.
    #[cfg(test)]
    pub(crate) fn from_facts_for_tests(
        facts: AppleToolFacts,
    ) -> Result<Self, Report<ReleaseError>> {
        check_facts(&facts)?;
        Ok(Self {
            facts,
            selection: AppleSelection::OrdinaryCi,
            sdk: None,
            toolchain: None,
        })
    }

    /// Measures the selected toolchain's helpers, entire `usr/lib`, `libLTO`,
    /// optional `libtapi` and clang resources in place and binds them into this record.
    ///
    /// # Errors
    ///
    /// Rejects changed recorded files, or as the toolchain measurement:
    /// ambiguous resource versions, a missing `libLTO`, unsafe entries,
    /// exhausted limits, cancellation or expiration.
    #[cfg(test)]
    pub(crate) fn with_measured_toolchain_for_tests(
        mut self,
        deadline: Instant,
        cancellation: &Cancellation,
    ) -> Result<Self, Report<ReleaseError>> {
        self.toolchain = Some(MeasuredToolchain::measure(
            &self.facts.clang,
            &self.facts.developer_dir,
            deadline,
            cancellation,
        )?);
        // One verification after measuring suffices: it rehashes every
        // recorded file against the facts (so an earlier change is still
        // caught) and also remeasures the new toolchain receipt. A second
        // pass before measuring would only rehash the same files again.
        self.verify()?;
        Ok(self)
    }

    /// Stages an explicit test selection through the same detached SDK policy.
    ///
    /// # Errors
    ///
    /// Rejects invalid source facts, unsafe trees, changes, cancellation or expiration.
    #[cfg(test)]
    pub(crate) fn with_detached_sdk_for_tests(
        facts: AppleToolFacts,
        deadline: Instant,
        cancellation: &Cancellation,
    ) -> Result<Self, Report<ReleaseError>> {
        let selected = Self::stage_detached_sdk(facts, deadline, cancellation)?;
        selected.verify()?;
        Ok(selected)
    }

    /// Stages the detached SDK of an explicit test selection without the
    /// trailing full verification.
    ///
    /// The recorded tools and SDK settings are verified before staging. Only
    /// a caller that runs a full [`Self::verify`] next, such as
    /// [`Self::measure_selected_developer`] through
    /// [`Self::with_measured_toolchain_for_tests`], may skip the trailing
    /// pass; every other caller uses [`Self::with_detached_sdk_for_tests`].
    ///
    /// # Errors
    ///
    /// Rejects invalid source facts, unsafe trees, changes, cancellation or expiration.
    #[cfg(test)]
    fn stage_detached_sdk(
        facts: AppleToolFacts,
        deadline: Instant,
        cancellation: &Cancellation,
    ) -> Result<Self, Report<ReleaseError>> {
        let mut selected = Self::from_facts_for_tests(facts)?;
        selected.verify()?;
        let sdk = OwnedSdk::stage(&selected.facts.sdk_root, deadline, cancellation)?;
        plain_path("owned Apple SDK root", sdk.root())?;
        selected.sdk = Some(Arc::new(sdk));
        Ok(selected)
    }

    /// Measures the selected Xcode of an ordinary CI macOS runner.
    ///
    /// The developer directory is the target of `/var/db/xcode_select_link`,
    /// which `xcode-select --switch` maintains; nothing is executed. The SDK
    /// version is `SDKSettings.json`'s `Version` and the Xcode version is the
    /// `CFBundleShortVersionString` of the bundle's `version.plist`. This
    /// measures the host, so only CI runs it.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Recipe`] when the selection link, the default
    /// toolchain, the macOS SDK or a version cannot be read, or as
    /// [`Self::from_facts_for_tests`].
    #[cfg(test)]
    pub(crate) fn for_ordinary_ci(
        deadline: Instant,
        cancellation: &Cancellation,
    ) -> Result<Self, Report<ReleaseError>> {
        let io = |error: std::io::Error| recipe_error(format!("cannot measure Xcode: {error}"));
        let link = std::fs::read_link("/var/db/xcode_select_link").map_err(io)?;
        let developer_dir = std::fs::canonicalize(link).map_err(io)?;
        Self::measure_selected_developer(developer_dir, deadline, cancellation)
    }

    /// Measures one explicitly selected canonical test developer directory.
    ///
    /// This is shared only by the strict CI link selection and the ignored
    /// local smoke override. Neither grants qualification or acquisition trust.
    #[cfg(test)]
    fn measure_selected_developer(
        developer_dir: PathBuf,
        deadline: Instant,
        cancellation: &Cancellation,
    ) -> Result<Self, Report<ReleaseError>> {
        plain_path("Apple developer directory", &developer_dir)?;
        real_directory("Apple developer directory", &developer_dir)?;
        let io = |error: std::io::Error| recipe_error(format!("cannot measure Xcode: {error}"));
        let sdk_root = std::fs::canonicalize(developer_dir.join(MACOS_SDK)).map_err(io)?;
        let settings = read_small(&sdk_root.join(SDK_SETTINGS))?;
        let sdk_version = sdk_settings_version(&settings)?;
        let bundle = developer_dir
            .parent()
            .ok_or_else(|| recipe_error("Apple developer directory has no bundle"))?;
        let plist = read_small(&bundle.join("version.plist"))?;
        let xcode_version = plist_string(&plist, "CFBundleShortVersionString")?;
        let bin = developer_dir.join(TOOLCHAIN_BIN);
        let (clang, ar, ld) = (bin.join("clang"), bin.join("ar"), bin.join("ld"));
        let facts = AppleToolFacts {
            clang_sha256: hash_regular("Apple clang", &clang, EXECUTABLE_LIMIT)?,
            ar_sha256: hash_regular("Apple ar", &ar, EXECUTABLE_LIMIT)?,
            ld_sha256: hash_regular("Apple ld", &ld, EXECUTABLE_LIMIT)?,
            sdk_settings_sha256: hex::encode(Sha256::digest(&settings)),
            developer_dir,
            sdk_root,
            clang,
            ar,
            ld,
            xcode_version,
            sdk_version,
        };
        // The toolchain measurement ends with a full verification that
        // rehashes the staged SDK as well, so staging skips its own trailing
        // pass over the same tree.
        Self::stage_detached_sdk(facts, deadline, cancellation)?
            .with_measured_toolchain_for_tests(deadline, cancellation)
    }

    /// The authority of this selection.
    pub(crate) fn authority(&self) -> Authority {
        match self.selection {
            AppleSelection::Qualified(qualification) => match qualification {},
            #[cfg(test)]
            AppleSelection::OrdinaryCi => Authority::Unqualified,
        }
    }

    /// The developer directory, the only system root.
    pub(crate) fn developer_dir(&self) -> &Path {
        &self.facts.developer_dir
    }

    /// The macOS SDK root.
    pub(crate) fn sdk_root(&self) -> &Path {
        self.sdk
            .as_ref()
            .map_or(&self.facts.sdk_root, |sdk| sdk.root())
    }

    /// The separately owned SDK root, absent for lexical fixture records.
    pub(crate) fn owned_sdk_root(&self) -> Option<&Path> {
        self.sdk.as_ref().map(|sdk| sdk.root())
    }

    /// The selected `clang`.
    pub(crate) fn clang(&self) -> &Path {
        &self.facts.clang
    }

    /// The selected `ar`.
    pub(crate) fn ar(&self) -> &Path {
        &self.facts.ar
    }

    /// The measured SDK version a linked executable must record.
    pub(crate) fn sdk_version(&self) -> MacosVersion {
        self.facts.sdk_version
    }

    /// Rehashes the tools, SDK settings, detached SDK and measured toolchain
    /// against the record.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Recipe`] when the developer directory or SDK
    /// root is not a real directory, a tool or the settings file is not a
    /// regular file, or any digest or toolchain receipt differs.
    pub(crate) fn verify(&self) -> Result<(), Report<ReleaseError>> {
        real_directory("Apple developer directory", &self.facts.developer_dir)?;
        real_directory("Apple SDK root", self.sdk_root())?;
        if let Some(sdk) = &self.sdk {
            sdk.verify()?;
        }
        if let Some(toolchain) = &self.toolchain {
            toolchain.verify()?;
        }
        let settings = self.sdk_root().join(SDK_SETTINGS);
        let expected = [
            (
                "Apple clang",
                &self.facts.clang,
                &self.facts.clang_sha256,
                EXECUTABLE_LIMIT,
            ),
            (
                "Apple ar",
                &self.facts.ar,
                &self.facts.ar_sha256,
                EXECUTABLE_LIMIT,
            ),
            (
                "Apple ld",
                &self.facts.ld,
                &self.facts.ld_sha256,
                EXECUTABLE_LIMIT,
            ),
            (
                "Apple SDK settings",
                &settings,
                &self.facts.sdk_settings_sha256,
                SETTINGS_LIMIT,
            ),
        ];
        for (label, path, digest, limit) in expected {
            if hash_regular(label, path, limit)? != *digest {
                return Err(recipe_error(format!("{label} changed after measurement")));
            }
        }
        Ok(())
    }

    /// The identity record as a measurement value.
    pub(crate) fn measurement(&self) -> Value {
        let text = |path: &Path| path.to_string_lossy().into_owned();
        let mut value = json!({
            "developer_dir": text(&self.facts.developer_dir),
            "sdk_root": if self.sdk.is_some() { REMAPPED_SDK.to_owned() } else { text(&self.facts.sdk_root) },
            "clang": text(&self.facts.clang),
            "ar": text(&self.facts.ar),
            "ld": text(&self.facts.ld),
            "xcode_version": self.facts.xcode_version,
            "sdk_version": self.facts.sdk_version.to_string(),
            "clang_sha256": self.facts.clang_sha256,
            "ar_sha256": self.facts.ar_sha256,
            "ld_sha256": self.facts.ld_sha256,
            "sdk_settings_sha256": self.facts.sdk_settings_sha256,
        });
        if let Some(sdk) = &self.sdk {
            value["sdk_tree"] = json!(sdk.receipt());
        }
        if let Some(toolchain) = &self.toolchain {
            value["toolchain"] = json!(toolchain.receipt());
        }
        value
    }

    /// SHA-256 of the canonical identity record, bound into the plan digest.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Recipe`] when the record cannot be encoded.
    pub(crate) fn identity_sha256(&self) -> Result<String, Report<ReleaseError>> {
        let bytes = canonical_json(&self.measurement(), Role::Metadata)
            .map_err(|error| error.change_context(ReleaseError::Recipe))?;
        Ok(hex::encode(Sha256::digest(bytes)))
    }
}

/// Reads a small regular file without following links.
#[cfg(test)]
fn read_small(path: &Path) -> Result<Vec<u8>, Report<ReleaseError>> {
    let io =
        |error: std::io::Error| recipe_error(format!("cannot read {}: {error}", path.display()));
    let file = File::options()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(io)?;
    let mut bytes = Vec::new();
    file.take(SETTINGS_LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(io)?;
    if bytes.len() as u64 > SETTINGS_LIMIT {
        return Err(recipe_error(format!("{} is too large", path.display())));
    }
    Ok(bytes)
}

/// Returns the SDK version `SDKSettings.json` states.
///
/// # Errors
///
/// Returns [`ReleaseError::Recipe`] when the bytes are not JSON with a
/// dotted decimal `Version`.
pub(crate) fn sdk_settings_version(bytes: &[u8]) -> Result<MacosVersion, Report<ReleaseError>> {
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|error| recipe_error(format!("SDK settings are not JSON: {error}")))?;
    value["Version"]
        .as_str()
        .and_then(MacosVersion::parse)
        .ok_or_else(|| recipe_error("SDK settings hold no dotted decimal Version"))
}

/// Returns the `<string>` that follows `<key>name</key>` in a property list.
///
/// # Errors
///
/// Returns [`ReleaseError::Recipe`] when the key or its string is absent or
/// the text is not UTF-8.
pub(crate) fn plist_string(bytes: &[u8], name: &str) -> Result<String, Report<ReleaseError>> {
    let text = std::str::from_utf8(bytes)
        .map_err(|error| recipe_error(format!("property list is not UTF-8: {error}")))?;
    let key = format!("<key>{name}</key>");
    let after = text
        .find(&key)
        .map(|start| text[start + key.len()..].trim_start())
        .and_then(|rest| rest.strip_prefix("<string>"))
        .and_then(|rest| rest.split_once("</string>"))
        .map(|(value, _)| value.to_owned());
    after.ok_or_else(|| recipe_error(format!("property list holds no string {name}")))
}
