//! Bounded selected host files, without helper execution or closure claims.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use error_stack::Report;
use serde::{Deserialize, Serialize};

use super::gnu::MeasurementSandbox;
use super::{CargoPlan, SelectedRustTools, recipe_error};
use crate::boundary::ConfinedCommand;
use crate::documents::{Role, canonical_json, hash_bounded_with_check, lower_hex, parse_json};
use crate::error::ReleaseError;
use crate::process::{BoundedCommand, ProcessLimits};

const MODE: &str = "TRUSTED_SERVER_FASTLY_TOOL_MEASUREMENT";
const PROFILE: &str = "fastly-linux-host-selected-files-v1";
const HOST: &str = "x86_64-unknown-linux-gnu";
const ROLES: [&str; 4] = [
    "host_compiler",
    "host_archiver",
    "configured_host_assembler",
    "wasm_rust_linker",
];

/// Selects C tools for host build units only.
pub(super) fn environment() -> BTreeMap<String, String> {
    [("HOST_CC", "/usr/bin/cc"), ("HOST_AR", "/usr/bin/ar")]
        .into_iter()
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect()
}

/// Selects host driver and target linker from the bound Rust root.
pub(super) fn linker_arguments(root: &str) -> Vec<String> {
    vec![
        "--config".to_owned(),
        format!("target.\"{HOST}\".linker=\"/usr/bin/cc\""),
        "--config".to_owned(),
        format!("target.\"wasm32-wasip1\".linker=\"{root}/lib/rustlib/{HOST}/bin/rust-lld\""),
    ]
}

fn tool_root(path: &str) -> Result<PathBuf, Report<ReleaseError>> {
    let bin = path
        .strip_suffix(":/usr/bin:/bin")
        .ok_or_else(|| recipe_error("Fastly helper requires exact recipe PATH"))?;
    let root = Path::new(
        bin.strip_suffix("/bin")
            .ok_or_else(|| recipe_error("Fastly PATH lacks tool bin"))?,
    );
    if !root.starts_with("/boundary/tool")
        || !root.is_absolute()
        || root
            .components()
            .any(|part| !matches!(part, Component::RootDir | Component::Normal(_)))
        || bin.contains(':')
        || bin
            .split('/')
            .skip(1)
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(recipe_error(
            "Fastly Rust root is outside bound readonly tools",
        ));
    }
    Ok(root.to_owned())
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileIdentity {
    selected: PathBuf,
    resolved: PathBuf,
    sha256: String,
    size: u64,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Measurement {
    profile: String,
    rust_root: PathBuf,
    rust_host: String,
    assembler_query: String,
    files: BTreeMap<String, FileIdentity>,
}

/// Files selected or configured by the bounded Linux Fastly recipe.
#[derive(Debug, Serialize)]
pub(crate) struct FastlyToolEvidence {
    measurement: Measurement,
    helper_sha256: String,
    build_boundary_fingerprint: String,
    measurement_boundary_fingerprint: String,
    plan_sha256: String,
    identity_scope: &'static str,
    acquisition: &'static str,
    full_closure_measured: bool,
}

fn limits() -> ProcessLimits {
    ProcessLimits {
        wall: Duration::from_secs(60),
        stdout_max: 64 * 1024,
        stderr_max: 64 * 1024,
    }
}

fn query(
    program: &Path,
    argument: &str,
    working: &Path,
    path: &str,
) -> Result<String, Report<ReleaseError>> {
    let raw = BoundedCommand::new(program, working, limits())?
        .arguments([OsString::from(argument)])
        .environment("PATH", path)?
        .environment("LC_ALL", "C")?
        .environment("TMPDIR", working)?
        .run()?
        .require_success()?;
    String::from_utf8(raw).map_err(|error| recipe_error(error.to_string()))
}

fn file_identity(
    selected: &Path,
    root: Option<&Path>,
) -> Result<FileIdentity, Report<ReleaseError>> {
    let resolved = fs::canonicalize(selected).map_err(|error| recipe_error(error.to_string()))?;
    let permitted = root.map_or_else(
        || {
            resolved.starts_with("/usr")
                || resolved.starts_with("/bin")
                || resolved.starts_with("/lib")
                || resolved.starts_with("/lib64")
        },
        |root| resolved.starts_with(root),
    );
    if !permitted {
        return Err(recipe_error(
            "Fastly selected file escapes reviewed image or Rust root",
        ));
    }
    let (sha256, size) = super::gnu::hash_image_file(&resolved)?;
    Ok(FileIdentity {
        selected: selected.to_owned(),
        resolved,
        sha256,
        size,
    })
}

/// Measures fixed selected files inside the denied Linux image.
///
/// # Errors
///
/// Rejects another helper mode, host, root, missing tool or failed query.
pub(super) fn helper_measurement() -> Result<Vec<u8>, Report<ReleaseError>> {
    if !cfg!(all(target_os = "linux", target_arch = "x86_64"))
        || std::env::var(MODE).as_deref() != Ok(PROFILE)
    {
        return Err(recipe_error(
            "Fastly measurement requires fixed Linux x86_64 helper mode",
        ));
    }
    let path = std::env::var("PATH").map_err(|error| recipe_error(error.to_string()))?;
    let root = tool_root(&path)?;
    let working =
        PathBuf::from(std::env::var("TMPDIR").map_err(|error| recipe_error(error.to_string()))?);
    if !working.starts_with("/boundary/writable")
        || working
            .components()
            .any(|part| matches!(part, Component::ParentDir))
    {
        return Err(recipe_error("Fastly helper requires private TMPDIR"));
    }
    let version = query(&root.join("bin/rustc"), "-vV", &working, &path)?;
    if version
        .lines()
        .filter(|line| line.starts_with("host: "))
        .collect::<Vec<_>>()
        != [format!("host: {HOST}")]
    {
        return Err(recipe_error("Fastly Rust host is not Linux x86_64"));
    }
    let assembler_query = query(
        Path::new("/usr/bin/cc"),
        "-print-prog-name=as",
        &working,
        &path,
    )?;
    let assembler = assembler_path(&assembler_query, &path)?;
    let mut files = BTreeMap::new();
    for (role, selected) in [
        ("host_compiler", PathBuf::from("/usr/bin/cc")),
        ("host_archiver", PathBuf::from("/usr/bin/ar")),
        ("configured_host_assembler", assembler),
    ] {
        files.insert(role.to_owned(), file_identity(&selected, None)?);
    }
    files.insert(
        "wasm_rust_linker".to_owned(),
        file_identity(
            &root.join(format!("lib/rustlib/{HOST}/bin/rust-lld")),
            Some(&root),
        )?,
    );
    let measurement = Measurement {
        profile: PROFILE.to_owned(),
        rust_root: root,
        rust_host: HOST.to_owned(),
        assembler_query,
        files,
    };
    validate(&measurement, &measurement.rust_root)?;
    canonical_json(
        &serde_json::to_value(measurement).map_err(|error| recipe_error(error.to_string()))?,
        Role::Metadata,
    )
    .map_err(|error| error.change_context(ReleaseError::Recipe))
}

fn assembler_path(output: &str, path: &str) -> Result<PathBuf, Report<ReleaseError>> {
    let line = output
        .strip_suffix('\n')
        .filter(|line| !line.is_empty() && !line.chars().any(char::is_control))
        .ok_or_else(|| recipe_error("Fastly assembler query lacks one result line"))?;
    if line == "as" {
        // The fixed recipe PATH puts measured Rust tools before image tools.
        return std::env::split_paths(path)
            .map(|root| root.join("as"))
            .find(|candidate| candidate.is_file())
            .ok_or_else(|| recipe_error("configured assembler missing from exact PATH"));
    }
    let selected = PathBuf::from(line);
    if !selected.is_absolute() {
        return Err(recipe_error("configured assembler is relative"));
    }
    Ok(selected)
}

fn validate(value: &Measurement, root: &Path) -> Result<(), Report<ReleaseError>> {
    if value.profile != PROFILE
        || value.rust_root != root
        || value.rust_host != HOST
        || value.files.len() != ROLES.len()
        || ROLES.iter().any(|role| !value.files.contains_key(*role))
    {
        return Err(recipe_error("unsupported Fastly tool measurement"));
    }
    for file in value.files.values() {
        if !file.selected.is_absolute()
            || !file.resolved.is_absolute()
            || !lower_hex(&file.sha256, 64)
            || file
                .resolved
                .components()
                .any(|part| matches!(part, Component::ParentDir))
        {
            return Err(recipe_error("invalid Fastly file identity"));
        }
    }
    for (role, selected) in [
        ("host_compiler", PathBuf::from("/usr/bin/cc")),
        ("host_archiver", PathBuf::from("/usr/bin/ar")),
        (
            "wasm_rust_linker",
            root.join(format!("lib/rustlib/{HOST}/bin/rust-lld")),
        ),
    ] {
        if value.files[role].selected != selected {
            return Err(recipe_error("Fastly selected tools differ from profile"));
        }
    }
    if !value.files["wasm_rust_linker"].resolved.starts_with(root) {
        return Err(recipe_error("Fastly Rust linker escaped measured root"));
    }
    let line = value
        .assembler_query
        .strip_suffix('\n')
        .filter(|line| !line.is_empty() && !line.chars().any(char::is_control))
        .ok_or_else(|| recipe_error("invalid Fastly assembler query"))?;
    let assembler = &value.files["configured_host_assembler"];
    if line == "as" {
        if ![
            root.join("bin/as"),
            PathBuf::from("/usr/bin/as"),
            PathBuf::from("/bin/as"),
        ]
        .contains(&assembler.selected)
        {
            return Err(recipe_error("assembler differs from recipe PATH"));
        }
    } else if !Path::new(line).is_absolute() || assembler.selected != Path::new(line) {
        return Err(recipe_error("assembler differs from compiler query"));
    }
    for role in [
        "host_compiler",
        "host_archiver",
        "configured_host_assembler",
    ] {
        let resolved = &value.files[role].resolved;
        if !["/usr", "/bin", "/lib", "/lib64"]
            .iter()
            .any(|allowed| resolved.starts_with(allowed))
        {
            return Err(recipe_error("Fastly host tool escaped image roots"));
        }
    }
    Ok(())
}

impl FastlyToolEvidence {
    /// Supplies fictional typed observations for pure projection tests.
    #[cfg(test)]
    pub(crate) fn projection_fixture() -> Self {
        let mut fixture = validation_tests::fixture();
        fixture.plan_sha256 = "b".repeat(64);
        fixture
    }

    /// Borrows measured raw file roles, including the configured assembler.
    pub(crate) fn measured_files(&self) -> impl Iterator<Item = (&str, &str)> {
        self.measurement
            .files
            .iter()
            .map(|(role, file)| (role.as_str(), file.sha256.as_str()))
    }
    /// Requires the completed build boundary and exact plan.
    ///
    /// # Errors
    ///
    /// Rejects another owner or malformed selected-file measurement.
    pub(crate) fn verify_binding(
        &self,
        boundary: &[u8; 32],
        plan: &str,
    ) -> Result<(), Report<ReleaseError>> {
        if self.build_boundary_fingerprint != hex::encode(boundary)
            || self.plan_sha256 != plan
            || !lower_hex(&self.measurement_boundary_fingerprint, 64)
            || self.measurement_boundary_fingerprint == self.build_boundary_fingerprint
            || self.full_closure_measured
            || self.acquisition != "unqualified_candidate_measurement"
        {
            return Err(recipe_error("Fastly tools differ from completed owner"));
        }
        validate(&self.measurement, &self.measurement.rust_root)
    }
    /// Captures selected files using the sibling sandbox and its denial proof.
    ///
    /// # Errors
    ///
    /// Rejects another boundary, plan, helper or malformed measurement.
    pub(super) fn measure(
        tools: &SelectedRustTools,
        plan: &CargoPlan,
        build: &[u8; 32],
        sandbox: &MeasurementSandbox,
    ) -> Result<Self, Report<ReleaseError>> {
        if sandbox.boundary.derived_from() != Some(*build)
            || !sandbox.boundary.has_owned_descendant_cleanup()
        {
            return Err(recipe_error(
                "Fastly tools require derived Docker measurement boundary",
            ));
        }
        let path = plan
            .environment
            .get("PATH")
            .ok_or_else(|| recipe_error("missing Fastly PATH"))?;
        let root = tool_root(path)?;
        if root != sandbox.boundary.execution_path(tools.root())? {
            return Err(recipe_error("Fastly PATH root differs from bound tools"));
        }
        for (name, expected) in environment() {
            if plan.environment.get(&name) != Some(&expected) {
                return Err(recipe_error("Fastly host tools differ from profile"));
            }
        }
        for pair in linker_arguments(
            root.to_str()
                .ok_or_else(|| recipe_error("non-UTF8 tool root"))?,
        )
        .as_chunks::<2>()
        .0
        {
            if !plan.arguments.windows(2).any(|window| window == pair) {
                return Err(recipe_error("Fastly linker differs from profile"));
            }
        }
        let helper = tools.root().join("probe");
        let before = hash_bounded_with_check(&helper, 512 * 1024 * 1024, &|| Ok(()))
            .map_err(|error| error.change_context(ReleaseError::Recipe))?;
        let mut command = ConfinedCommand::new(&helper, &plan.working_directory, limits())?
            .arguments([OsString::from("gnu-image-measurement")])
            .environment(MODE, PROFILE)?;
        for name in ["HOME", "TMPDIR", "LC_ALL", "PATH"] {
            command = command.environment(
                name,
                plan.environment
                    .get(name)
                    .ok_or_else(|| recipe_error("incomplete Fastly measurement environment"))?,
            )?;
        }
        let raw = sandbox
            .boundary
            .run_confined(&command, &sandbox.proof)?
            .require_success()?;
        let measurement: Measurement = serde_json::from_value(
            parse_json(&raw, Role::Metadata)
                .map_err(|error| error.change_context(ReleaseError::Recipe))?,
        )
        .map_err(|error| recipe_error(error.to_string()))?;
        validate(&measurement, &root)?;
        let after = hash_bounded_with_check(&helper, 512 * 1024 * 1024, &|| Ok(()))
            .map_err(|error| error.change_context(ReleaseError::Recipe))?;
        if before.sha256 != after.sha256 {
            return Err(recipe_error(
                "Fastly staged helper changed during measurement",
            ));
        }
        Ok(Self {
            measurement,
            helper_sha256: before.sha256,
            build_boundary_fingerprint: hex::encode(build),
            measurement_boundary_fingerprint: hex::encode(sandbox.boundary.execution_fingerprint()),
            plan_sha256: plan.digest()?,
            identity_scope: "selected_and_configured_files_in_read_only_image",
            acquisition: "unqualified_candidate_measurement",
            full_closure_measured: false,
        })
    }
    /// Requires stable selected files, helper, profile, boundaries and plan.
    ///
    /// # Errors
    ///
    /// Rejects any changed before/after receipt field.
    pub(super) fn verify_after(&self, after: &Self) -> Result<(), Report<ReleaseError>> {
        if serde_json::to_vec(self).map_err(|error| recipe_error(error.to_string()))?
            != serde_json::to_vec(after).map_err(|error| recipe_error(error.to_string()))?
        {
            return Err(recipe_error("Fastly tools changed around execution"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fastly_host_tools_are_explicit_and_target_linker_is_bound() {
        assert_eq!(
            environment()["HOST_CC"],
            "/usr/bin/cc",
            "should select host driver"
        );
        assert_eq!(
            environment()["HOST_AR"],
            "/usr/bin/ar",
            "should select host archiver"
        );
        let arguments = linker_arguments("/boundary/tool/0");
        assert!(
            arguments.as_chunks::<2>().0.iter().any(|pair| pair
                == &[
                    "--config".to_owned(),
                    "target.\"x86_64-unknown-linux-gnu\".linker=\"/usr/bin/cc\"".to_owned()
                ]),
            "should select the explicit host Rust linker driver"
        );
        assert!(arguments.iter().any(|argument| argument.contains("wasm32-wasip1\".linker=\"/boundary/tool/0/lib/rustlib/x86_64-unknown-linux-gnu/bin/rust-lld")), "should bind target linker to measured Rust root");
    }

    #[test]
    fn fastly_tool_root_rejects_ambient_and_extra_path_entries() {
        assert!(
            tool_root("/usr/bin:/bin").is_err(),
            "should reject ambient tools"
        );
        assert!(
            tool_root("/boundary/tool/0/bin:/usr/bin:/bin:/tmp").is_err(),
            "should reject extra roots"
        );
        assert_eq!(
            tool_root("/boundary/tool/0/bin:/usr/bin:/bin").expect("should accept bound root"),
            std::path::PathBuf::from("/boundary/tool/0"),
            "should derive root from exact plan PATH"
        );
    }
}

#[cfg(test)]
mod validation_tests {
    use super::*;

    pub(super) fn fixture() -> FastlyToolEvidence {
        let root = PathBuf::from("/boundary/tool/0");
        let mut files = BTreeMap::new();
        for (role, selected) in [
            ("host_compiler", PathBuf::from("/usr/bin/cc")),
            ("host_archiver", PathBuf::from("/usr/bin/ar")),
            ("configured_host_assembler", PathBuf::from("/usr/bin/as")),
            (
                "wasm_rust_linker",
                root.join(format!("lib/rustlib/{HOST}/bin/rust-lld")),
            ),
        ] {
            files.insert(
                role.to_owned(),
                FileIdentity {
                    resolved: selected.clone(),
                    selected,
                    sha256: "a".repeat(64),
                    size: 12,
                },
            );
        }
        FastlyToolEvidence {
            measurement: Measurement {
                profile: PROFILE.to_owned(),
                rust_root: root,
                rust_host: HOST.to_owned(),
                assembler_query: "as\n".to_owned(),
                files,
            },
            helper_sha256: "b".repeat(64),
            build_boundary_fingerprint: hex::encode([1; 32]),
            measurement_boundary_fingerprint: hex::encode([2; 32]),
            plan_sha256: "c".repeat(64),
            identity_scope: "selected_and_configured_files_in_read_only_image",
            acquisition: "unqualified_candidate_measurement",
            full_closure_measured: false,
        }
    }

    #[test]
    fn fastly_measurement_rejects_wrong_roles_hosts_roots_and_query_claims() {
        let valid = fixture();
        let root = valid.measurement.rust_root.clone();
        assert!(
            validate(&valid.measurement, &root).is_ok(),
            "should accept fixed selected-file scope"
        );
        let mut changed = fixture();
        changed.measurement.rust_host = "example-host".to_owned();
        assert!(
            validate(&changed.measurement, &root).is_err(),
            "should reject other Rust host"
        );
        let mut changed = fixture();
        changed
            .measurement
            .files
            .remove("configured_host_assembler");
        assert!(
            validate(&changed.measurement, &root).is_err(),
            "should reject missing role"
        );
        let mut changed = fixture();
        changed
            .measurement
            .files
            .get_mut("wasm_rust_linker")
            .expect("should have linker")
            .resolved = PathBuf::from("/usr/bin/ld");
        assert!(
            validate(&changed.measurement, &root).is_err(),
            "should reject escaped Rust linker"
        );
        let mut changed = fixture();
        changed.measurement.assembler_query = "/usr/bin/other-as\n".to_owned();
        assert!(
            validate(&changed.measurement, &root).is_err(),
            "should reject query mismatch"
        );
        let mut changed = fixture();
        changed
            .measurement
            .files
            .get_mut("host_compiler")
            .expect("should have compiler")
            .sha256 = "z".repeat(64);
        assert!(
            validate(&changed.measurement, &root).is_err(),
            "should reject malformed digest"
        );
    }

    #[test]
    fn fastly_measurement_rejects_raw_path_aliases() {
        for path in [
            "/boundary/tool/0/../1/bin:/usr/bin:/bin",
            "/boundary/tool/./0/bin:/usr/bin:/bin",
            "/boundary//tool/0/bin:/usr/bin:/bin",
        ] {
            assert!(
                tool_root(path).is_err(),
                "should reject noncanonical tool root {path}"
            );
        }
    }

    #[test]
    fn fastly_assembler_query_rejects_malformed_and_relative_results() {
        for output in [
            "",
            "as",
            "as\nas\n",
            "as\r\n",
            "../as\n",
            "relative/as\n",
            "/usr/bin/as\0\n",
        ] {
            assert!(
                assembler_path(output, "/boundary/tool/0/bin:/usr/bin:/bin").is_err(),
                "should reject malformed compiler query {output:?}"
            );
        }
        assert_eq!(
            assembler_path("/usr/bin/as\n", "/boundary/tool/0/bin:/usr/bin:/bin")
                .expect("should parse one absolute assembler path"),
            PathBuf::from("/usr/bin/as"),
            "should preserve the path for subsequent independent hashing"
        );
    }

    #[test]
    fn fastly_receipt_rejects_another_owner_and_changed_measurement() {
        let before = fixture();
        assert!(
            before.verify_binding(&[1; 32], &"c".repeat(64)).is_ok(),
            "should accept exact owner"
        );
        assert!(
            before.verify_binding(&[3; 32], &"c".repeat(64)).is_err(),
            "should reject another build boundary"
        );
        assert!(
            before.verify_binding(&[1; 32], &"d".repeat(64)).is_err(),
            "should reject another plan"
        );
        assert!(
            before.verify_after(&fixture()).is_ok(),
            "should accept stable before and after"
        );
        let mut after = fixture();
        after
            .measurement
            .files
            .get_mut("host_compiler")
            .expect("should have compiler")
            .sha256 = "e".repeat(64);
        assert!(
            before.verify_after(&after).is_err(),
            "should reject changed tool bytes"
        );
        let mut after = fixture();
        after.helper_sha256 = "e".repeat(64);
        assert!(
            before.verify_after(&after).is_err(),
            "should reject changed helper bytes"
        );
        let mut after = fixture();
        after.measurement_boundary_fingerprint = after.build_boundary_fingerprint.clone();
        assert!(
            after.verify_binding(&[1; 32], &"c".repeat(64)).is_err(),
            "should require distinct measurement boundary"
        );
    }
}

#[cfg(test)]
mod file_tests {
    use std::os::unix::fs::symlink;

    use sha2::{Digest as _, Sha256};

    use super::*;

    #[test]
    fn fastly_file_identity_hashes_bytes_and_rejects_escape_and_nonregular_files() {
        let directory = tempfile::tempdir().expect("should allocate fictional tool root");
        let root = fs::canonicalize(directory.path()).expect("should resolve tool root");
        let selected = root.join("rust-lld");
        fs::write(&selected, b"fictional linker before").expect("should write selected file");
        let before = file_identity(&selected, Some(&root)).expect("should measure selected file");
        assert_eq!(
            before.sha256,
            hex::encode(Sha256::digest(b"fictional linker before")),
            "should independently hash selected bytes"
        );
        assert_eq!(before.size, 23, "should measure raw byte length");
        fs::write(&selected, b"fictional linker after").expect("should change selected bytes");
        let after = file_identity(&selected, Some(&root)).expect("should measure changed file");
        assert_ne!(before.sha256, after.sha256, "should bind changed raw bytes");
        assert!(
            file_identity(&root, Some(&root)).is_err(),
            "should reject directory as selected file"
        );
        let outside = tempfile::tempdir().expect("should allocate fictional outside root");
        let outside_file = outside.path().join("other-linker");
        fs::write(&outside_file, b"fictional outside file").expect("should write outside file");
        let alias = root.join("escaping-alias");
        symlink(&outside_file, &alias).expect("should create fixture alias");
        assert!(
            file_identity(&alias, Some(&root)).is_err(),
            "should reject alias escaping selected Rust root"
        );
    }
}
