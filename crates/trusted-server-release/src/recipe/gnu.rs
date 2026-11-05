//! Fixed GNU candidate profile; image identity supplies no release authority.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use error_stack::Report;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use super::{CargoPlan, SelectedRustTools, recipe_error};
use crate::boundary::{Boundary, ConfinedCommand, DenialProof, ProbeProgram};
use crate::documents::{Role, canonical_json, hash_bounded_with_check, lower_hex, parse_json};
use crate::error::ReleaseError;
use crate::process::{BoundedCommand, ProcessLimits};
use image::{image_path, installed_snapshot};

mod image;
#[cfg(test)]
mod tests;

/// Explicit host and target C tools and fail-closed AWS-LC backend selection.
pub(super) fn environment() -> BTreeMap<String, String> {
    [
        ("CC_x86_64_unknown_linux_gnu", "/usr/bin/gcc"),
        ("AR_x86_64_unknown_linux_gnu", "/usr/bin/ar"),
        ("HOST_CC", "/usr/bin/gcc"),
        ("HOST_AR", "/usr/bin/ar"),
        ("AWS_LC_SYS_CMAKE_BUILDER", "0"),
        ("AWS_LC_SYS_CMAKE_BUILDER_x86_64_unknown_linux_gnu", "0"),
    ]
    .into_iter()
    .map(|(name, value)| (name.to_owned(), value.to_owned()))
    .collect()
}

/// Explicit Rust linker driver selected inside the image namespace.
///
/// Rust may ask this driver to use rust-lld from the measured Rust tree.
/// Configured GCC leaf-linker candidates are not execution observations.
pub(super) fn linker_arguments() -> Vec<String> {
    vec![
        "--config".to_owned(),
        "target.\"x86_64-unknown-linux-gnu\".linker=\"/usr/bin/gcc\"".to_owned(),
    ]
}

const PROFILE: &str = "gnu-cc-installed-image-v1";
const HELPER_ARGUMENT: &str = "gnu-image-measurement";
const MEASUREMENT_LIMIT: u64 = 64 * 1024;
const FIXED_SELECTED_FILES: [(&str, &str); 4] = [
    ("compiler", "/usr/bin/gcc"),
    ("archiver", "/usr/bin/ar"),
    ("gcc_linker_candidate", "/usr/bin/ld"),
    ("gcc_assembler_candidate", "/usr/bin/as"),
];
const SELECTED_ROLES: [&str; 9] = [
    "compiler",
    "archiver",
    "gcc_linker_candidate",
    "gcc_assembler_candidate",
    "configured_cc1",
    "configured_collect2",
    "configured_as",
    "configured_ld",
    "compiler_runtime",
];
const COMPILER_QUERIES: [&str; 8] = [
    "-print-prog-name=cc1",
    "-print-prog-name=collect2",
    "-print-prog-name=as",
    "-print-prog-name=ld",
    "-print-libgcc-file-name",
    "-print-file-name=include",
    "-print-sysroot",
    "-print-search-dirs",
];

/// Selected/configured installed-file facts, never a claim of executed helpers.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Measurement {
    profile: String,
    installed_files_sha256: String,
    /// Image files recorded by metadata only because the helper cannot read them.
    content_unmeasured_files: Vec<String>,
    roots: Vec<String>,
    selected_files: BTreeMap<String, SelectedFile>,
    compiler_queries: BTreeMap<String, String>,
    sysroot: String,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SelectedFile {
    selected: PathBuf,
    resolved: PathBuf,
    sha256: String,
    size: u64,
}

/// Candidate GNU receipt retained inside the owning composition receipt.
///
/// Installed bytes and configured helper paths do not authenticate acquisition
/// or prove that every installed helper executed.
#[derive(Debug, Serialize)]
pub(crate) struct GnuEvidence {
    measurement: Measurement,
    profile_sha256: String,
    helper_sha256: String,
    /// The boundary that ran the Cargo build this receipt belongs to.
    build_boundary_fingerprint: String,
    /// The sibling sandbox, derived from the build boundary without its target
    /// root, that ran both measurements.
    measurement_boundary_fingerprint: String,
    plan_sha256: String,
    identity_scope: &'static str,
    acquisition: &'static str,
    full_closure_measured: bool,
}

fn limits() -> ProcessLimits {
    ProcessLimits {
        wall: Duration::from_secs(180),
        stdout_max: MEASUREMENT_LIMIT,
        stderr_max: MEASUREMENT_LIMIT,
    }
}

/// Limits of the measurement sandbox's denial probe.
fn probe_limits() -> ProcessLimits {
    ProcessLimits {
        wall: Duration::from_secs(60),
        stdout_max: 4096,
        stderr_max: 4096,
    }
}

/// Selects the staged tool-root release helper as the denial probe.
#[cfg(test)]
fn measurement_probe(tools: &SelectedRustTools) -> Result<ProbeProgram, Report<ReleaseError>> {
    ProbeProgram::from_staged_release_binary(&tools.root().join("probe"))
}

/// No production selection runs a candidate build, so none selects a probe.
#[cfg(not(test))]
fn measurement_probe(_tools: &SelectedRustTools) -> Result<ProbeProgram, Report<ReleaseError>> {
    Err(Report::new(ReleaseError::Unqualified)
        .attach("GNU measurement sandbox has no qualified denial probe"))
}

/// The sandbox that runs both GNU measurements, separate from the build.
///
/// It is a sibling of the build boundary without the recipe target root:
/// after the build that root legitimately holds Cargo's in-root hard links,
/// which the unchanged pre-run single-link scan rejects. Both measurements
/// run in this one instance, so their receipts stay byte-identical, and the
/// sandbox carries its own denial proof.
pub(super) struct MeasurementSandbox {
    pub(super) boundary: Boundary,
    pub(super) proof: DenialProof,
}

impl MeasurementSandbox {
    /// Derives the sibling of `build` without `target_dir` and proves denial
    /// in it with the staged tool-root probe.
    ///
    /// # Errors
    ///
    /// Returns an error when the sibling cannot be derived (see
    /// [`Boundary::sibling_without_writable`]), no probe is selectable, or
    /// denial is not established.
    pub(super) fn establish(
        build: &Boundary,
        tools: &SelectedRustTools,
        target_dir: &Path,
    ) -> Result<Self, Report<ReleaseError>> {
        let boundary = build.sibling_without_writable(target_dir)?;
        let probe = measurement_probe(tools)?;
        let proof = boundary.establish_denial(&probe, probe_limits())?;
        Ok(Self { boundary, proof })
    }
}

/// Identity of the source-owned GNU selection and installed-root scope.
fn profile_digest() -> String {
    let mut digest = Sha256::new();
    digest.update(PROFILE);
    for root in image::ROOTS {
        digest.update([0]);
        digest.update(root);
    }
    for (name, value) in environment() {
        digest.update([0]);
        digest.update(name);
        digest.update([0]);
        digest.update(value);
    }
    for argument in linker_arguments() {
        digest.update([0]);
        digest.update(argument);
    }
    hex::encode(digest.finalize())
}

fn file_identity(selected: &Path) -> Result<SelectedFile, Report<ReleaseError>> {
    file_identity_at(Path::new("/"), selected)
}

fn file_identity_at(root: &Path, selected: &Path) -> Result<SelectedFile, Report<ReleaseError>> {
    let root = fs::canonicalize(root).map_err(|error| recipe_error(error.to_string()))?;
    let relative = selected
        .strip_prefix("/")
        .map_err(|error| recipe_error(error.to_string()))?;
    let actual = fs::canonicalize(root.join(relative))
        .map_err(|error| recipe_error(format!("cannot resolve selected GNU file: {error}")))?;
    let resolved = Path::new("/").join(
        actual
            .strip_prefix(&root)
            .map_err(|error| recipe_error(error.to_string()))?,
    );
    image_path(&resolved)?;
    if !fs::metadata(&actual)
        .map_err(|error| recipe_error(error.to_string()))?
        .is_file()
    {
        return Err(recipe_error("selected GNU tool is not a regular file"));
    }
    let (sha256, size) = image::image_file_hash(&actual)?;
    Ok(SelectedFile {
        selected: selected.to_owned(),
        resolved,
        sha256,
        size,
    })
}

fn compiler_query(argument: &str, working: &Path) -> Result<String, Report<ReleaseError>> {
    let output = BoundedCommand::new(Path::new("/usr/bin/gcc"), working, limits())?
        .arguments([OsString::from(argument)])
        .environment(
            "PATH",
            std::env::var_os("PATH")
                .ok_or_else(|| recipe_error("GNU helper requires the exact recipe PATH"))?,
        )?
        .environment("LC_ALL", "C")?
        .environment("TMPDIR", working)?
        .run()?
        .require_success()?;
    String::from_utf8(output)
        .map_err(|error| recipe_error(format!("GNU query is not UTF-8: {error}")))
}

/// Writes the fixed installed-image measurement in the selected Linux namespace.
///
/// # Errors
///
/// Rejects non-Linux hosts, missing/escaping tools/resources, failed bounded
/// compiler queries, unsupported sysroots and oversized installed inventories.
/// This private helper supplies no selection or qualification capability.
pub(crate) fn helper_measurement() -> Result<Vec<u8>, Report<ReleaseError>> {
    if std::env::var_os("TRUSTED_SERVER_FASTLY_TOOL_MEASUREMENT").is_some() {
        return super::fastly_tools::helper_measurement();
    }
    if !cfg!(target_os = "linux") {
        return Err(recipe_error("GNU measurement helper requires Linux"));
    }
    let working = std::env::var_os("TMPDIR")
        .map(PathBuf::from)
        .ok_or_else(|| recipe_error("GNU helper requires private TMPDIR"))?;
    if !working.is_absolute() || !working.starts_with("/boundary/writable") {
        return Err(recipe_error(
            "GNU helper TMPDIR is outside the private writable mount",
        ));
    }
    let mut selected_files = BTreeMap::new();
    for (role, path) in FIXED_SELECTED_FILES {
        selected_files.insert(role.to_owned(), file_identity(Path::new(path))?);
    }
    let mut compiler_queries = BTreeMap::new();
    for name in ["cc1", "collect2", "as", "ld"] {
        let query = format!("-print-prog-name={name}");
        let output = compiler_query(&query, &working)?;
        let text = output
            .strip_suffix('\n')
            .ok_or_else(|| recipe_error("GNU helper query has no final newline"))?;
        let selected = if text == name && matches!(name, "as" | "ld") {
            let path = std::env::var_os("PATH")
                .ok_or_else(|| recipe_error("GNU helper requires recipe PATH"))?;
            std::env::split_paths(&path)
                .map(|root| root.join(name))
                .find(|candidate| candidate.is_file())
                .ok_or_else(|| recipe_error("GNU helper is absent from recipe PATH"))?
        } else {
            PathBuf::from(text)
        };
        selected_files.insert(format!("configured_{name}"), file_identity(&selected)?);
        compiler_queries.insert(query, output);
    }
    let query = "-print-libgcc-file-name";
    let output = compiler_query(query, &working)?;
    selected_files.insert(
        "compiler_runtime".to_owned(),
        file_identity(Path::new(output.trim_end_matches('\n')))?,
    );
    compiler_queries.insert(query.to_owned(), output);
    let include = compiler_query("-print-file-name=include", &working)?;
    let include_path = fs::canonicalize(include.trim_end_matches('\n'))
        .map_err(|error| recipe_error(error.to_string()))?;
    image_path(&include_path)?;
    if !include_path.is_dir() {
        return Err(recipe_error(
            "GNU compiler include resource is not a directory",
        ));
    }
    compiler_queries.insert("-print-file-name=include".to_owned(), include);
    let sysroot = compiler_query("-print-sysroot", &working)?;
    check_image_sysroot(&sysroot)?;
    compiler_queries.insert("-print-sysroot".to_owned(), sysroot);
    compiler_queries.insert(
        "-print-search-dirs".to_owned(),
        compiler_query("-print-search-dirs", &working)?,
    );
    let installed = installed_snapshot(Path::new("/"))?;
    let measurement = Measurement {
        profile: PROFILE.to_owned(),
        installed_files_sha256: installed.sha256,
        content_unmeasured_files: installed.content_unmeasured,
        roots: image::ROOTS.iter().map(|root| (*root).to_owned()).collect(),
        selected_files,
        compiler_queries,
        sysroot: "/".to_owned(),
    };
    canonical_json(
        &serde_json::to_value(measurement).map_err(|error| recipe_error(error.to_string()))?,
        Role::Metadata,
    )
    .map_err(|error| error.change_context(ReleaseError::Recipe))
}

/// Requires a strictly sorted subset of the reviewed unreadable image paths.
fn valid_content_unmeasured(files: &[String]) -> bool {
    files.windows(2).all(|pair| pair[0] < pair[1])
        && files
            .iter()
            .all(|file| image::REVIEWED_UNREADABLE.contains(&file.as_str()))
}

fn check_image_sysroot(value: &str) -> Result<(), Report<ReleaseError>> {
    // GCC prints no bytes when no alternate target sysroot was configured.
    if !matches!(value, "" | "\n" | "/\n") {
        let observed = value.chars().take(256).collect::<String>();
        return Err(recipe_error(format!(
            "GNU profile requires the image-root sysroot; compiler reported {observed:?}"
        )));
    }
    Ok(())
}

fn validate_measurement(measurement: &Measurement) -> Result<(), Report<ReleaseError>> {
    if measurement.profile != PROFILE
        || !lower_hex(&measurement.installed_files_sha256, 64)
        || !valid_content_unmeasured(&measurement.content_unmeasured_files)
        || measurement.roots != image::ROOTS
        || measurement.sysroot != "/"
        || measurement.selected_files.len() != SELECTED_ROLES.len()
        || SELECTED_ROLES
            .iter()
            .any(|role| !measurement.selected_files.contains_key(*role))
        || measurement.compiler_queries.len() != COMPILER_QUERIES.len()
        || COMPILER_QUERIES
            .iter()
            .any(|query| !measurement.compiler_queries.contains_key(*query))
    {
        return Err(recipe_error(
            "GNU helper returned an unsupported measurement",
        ));
    }
    for file in measurement.selected_files.values() {
        if !file.selected.is_absolute() {
            return Err(recipe_error("GNU query returned a relative file path"));
        }
        image_path(&file.resolved)?;
        if !lower_hex(&file.sha256, 64) {
            return Err(recipe_error("invalid GNU file identity"));
        }
    }
    check_image_sysroot(&measurement.compiler_queries["-print-sysroot"])?;
    for (role, path) in FIXED_SELECTED_FILES {
        if measurement.selected_files[role].selected != Path::new(path) {
            return Err(recipe_error(
                "GNU selected driver differs from the fixed profile",
            ));
        }
    }
    for (role, query, bare_name) in [
        ("configured_cc1", "-print-prog-name=cc1", None),
        ("configured_collect2", "-print-prog-name=collect2", None),
        ("configured_as", "-print-prog-name=as", Some("as")),
        ("configured_ld", "-print-prog-name=ld", Some("ld")),
        ("compiler_runtime", "-print-libgcc-file-name", None),
    ] {
        let output = measurement.compiler_queries[query]
            .strip_suffix('\n')
            .filter(|value| !value.is_empty() && !value.chars().any(char::is_control))
            .ok_or_else(|| recipe_error("GNU path query lacks one bounded result line"))?;
        let file = &measurement.selected_files[role];
        let selected = &file.selected;
        let matches = if let Some(name) = bare_name.filter(|name| *name == output) {
            let candidate = &measurement.selected_files[if name == "as" {
                "gcc_assembler_candidate"
            } else {
                "gcc_linker_candidate"
            }];
            // The fixed image candidate precedes /bin in the selected PATH.
            // An alternate spelling must identify the same measured image file.
            (selected == Path::new(&format!("/usr/bin/{name}"))
                || selected == Path::new(&format!("/bin/{name}")))
                && file.resolved == candidate.resolved
                && file.sha256 == candidate.sha256
                && file.size == candidate.size
        } else {
            Path::new(output).is_absolute() && selected == Path::new(output)
        };
        if !matches {
            return Err(recipe_error(
                "GNU selected file differs from its compiler query",
            ));
        }
    }
    Ok(())
}

impl GnuEvidence {
    /// Returns whether installed-file measurements lack a complete tool closure.
    pub(crate) fn tool_closure_unmeasured(&self) -> bool {
        !self.full_closure_measured
    }
    /// Returns whether acquisition is recorded as an unqualified candidate measurement.
    pub(crate) fn original_acquisition_unverified(&self) -> bool {
        self.acquisition == "unqualified_candidate_measurement"
    }
    /// Supplies fictional typed observations for projection tests only.
    #[cfg(test)]
    pub(crate) fn projection_fixture() -> Self {
        tests::projection_fixture()
    }
    /// Supplies an unreviewed fictional file role for fail-closed adapter tests.
    #[cfg(test)]
    pub(crate) fn projection_fixture_with_unknown_file_role() -> Self {
        let mut fixture = Self::projection_fixture();
        let file = fixture
            .measurement
            .selected_files
            .remove("compiler_runtime")
            .expect("should have fictional runtime archive");
        fixture
            .measurement
            .selected_files
            .insert("unreviewed_compiler_resource".to_owned(), file);
        fixture
    }
    /// Returns the conservative installed-image tree identity.
    pub(crate) fn installed_files_sha256(&self) -> &str {
        &self.measurement.installed_files_sha256
    }
    /// Lists image files covered by metadata only, never by content.
    pub(crate) fn content_unmeasured_files(&self) -> &[String] {
        &self.measurement.content_unmeasured_files
    }
    /// Borrows selected/configured raw-file identities without private paths.
    pub(crate) fn measured_files(&self) -> impl Iterator<Item = (&str, &str)> {
        self.measurement
            .selected_files
            .iter()
            .map(|(role, file)| (role.as_str(), file.sha256.as_str()))
    }
    /// Requires this receipt's build boundary and exact plan at completion
    /// sealing.
    ///
    /// `build_boundary` is the boundary that ran the Cargo build. The
    /// measurement sandbox was required to be derived from that boundary when
    /// the receipt was measured; here its recorded fingerprint must be a
    /// distinct well-formed instance fingerprint. This relies on in-process
    /// construction through [`GnuEvidence::measure`]: a receipt read back from
    /// serialized data cannot prove that derivation.
    ///
    /// # Errors
    ///
    /// Rejects another build boundary, invocation or source-owned GNU
    /// profile, or a malformed measurement sandbox fingerprint.
    pub(crate) fn verify_binding(
        &self,
        build_boundary: &[u8; 32],
        plan: &str,
    ) -> Result<(), Report<ReleaseError>> {
        let build = hex::encode(build_boundary);
        if self.build_boundary_fingerprint != build
            || self.plan_sha256 != plan
            || self.profile_sha256 != profile_digest()
        {
            return Err(recipe_error(
                "GNU receipt differs from the completed boundary or plan",
            ));
        }
        if !lower_hex(&self.measurement_boundary_fingerprint, 64)
            || self.measurement_boundary_fingerprint == build
        {
            return Err(recipe_error(
                "GNU receipt lacks a separate measurement sandbox fingerprint",
            ));
        }
        Ok(())
    }
    /// Measures through the staged helper in the sandbox derived from the
    /// build boundary `build_boundary`, under that sandbox's denial proof.
    ///
    /// # Errors
    ///
    /// Rejects a sandbox not derived from `build_boundary`, another boundary
    /// type, missing helper, changed profile/plan or malformed, unsupported or
    /// failed in-image measurements.
    pub(super) fn measure(
        tools: &SelectedRustTools,
        plan: &CargoPlan,
        build_boundary: &[u8; 32],
        sandbox: &MeasurementSandbox,
    ) -> Result<Self, Report<ReleaseError>> {
        let boundary = &sandbox.boundary;
        if boundary.derived_from() != Some(*build_boundary) {
            return Err(recipe_error(
                "GNU measurement sandbox was not derived from the build boundary",
            ));
        }
        if !boundary.has_owned_descendant_cleanup() {
            return Err(recipe_error(
                "GNU profile requires the selected Docker boundary",
            ));
        }
        for (name, expected) in environment() {
            if plan.environment.get(&name) != Some(&expected) {
                return Err(recipe_error("GNU plan differs from fixed native profile"));
            }
        }
        let linker = linker_arguments();
        if !plan
            .arguments
            .windows(2)
            .any(|arguments| arguments == linker)
        {
            return Err(recipe_error(
                "GNU plan does not select the reviewed linker driver",
            ));
        }
        let helper = tools.root().join("probe");
        let helper_hash = hash_bounded_with_check(&helper, 512 * 1024 * 1024, &|| Ok(()))
            .map_err(|error| error.change_context(ReleaseError::Recipe))?;
        let mut command = ConfinedCommand::new(&helper, &plan.working_directory, limits())?
            .arguments([OsString::from(HELPER_ARGUMENT)]);
        for name in ["HOME", "TMPDIR", "LC_ALL", "PATH"] {
            command = command.environment(
                name,
                plan.environment
                    .get(name)
                    .ok_or_else(|| recipe_error("GNU measurement environment is incomplete"))?,
            )?;
        }
        let raw = boundary
            .run_confined(&command, &sandbox.proof)?
            .require_success()?;
        let value = parse_json(&raw, Role::Metadata)
            .map_err(|error| error.change_context(ReleaseError::Recipe))?;
        let measurement: Measurement = serde_json::from_value(value)
            .map_err(|error| recipe_error(format!("invalid GNU measurement: {error}")))?;
        validate_measurement(&measurement)?;
        Ok(Self {
            measurement,
            profile_sha256: profile_digest(),
            helper_sha256: helper_hash.sha256,
            build_boundary_fingerprint: hex::encode(build_boundary),
            measurement_boundary_fingerprint: hex::encode(boundary.execution_fingerprint()),
            plan_sha256: plan.digest()?,
            identity_scope: "selected_and_configured_files_in_read_only_image",
            acquisition: "unqualified_candidate_measurement",
            full_closure_measured: false,
        })
    }

    /// Requires identical before/after in-image measurements for this invocation.
    ///
    /// # Errors
    ///
    /// Rejects changed installed bytes, helper, boundary, profile or Cargo plan.
    pub(super) fn verify_after(&self, after: &Self) -> Result<(), Report<ReleaseError>> {
        let encode = |value: &Self| {
            serde_json::to_vec(value).map_err(|error| recipe_error(error.to_string()))
        };
        if encode(self)? != encode(after)? {
            return Err(recipe_error(
                "GNU image inputs or invocation changed around execution",
            ));
        }
        Ok(())
    }
}

/// Hashes a resolved image regular file without following its final alias.
pub(super) fn hash_image_file(path: &Path) -> Result<(String, u64), Report<ReleaseError>> {
    image::image_file_hash(path)
}
