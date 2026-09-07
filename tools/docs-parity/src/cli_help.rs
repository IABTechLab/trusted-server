//! Native recursive CLI-help capture and authenticated hosted import.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::env;
use std::fs;
use std::io::{Cursor, Read as _, Write as _};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, TryRecvError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use error_stack::Report;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::repository::{NormalizedRelativePath, Repository};

const MAXIMUM_OUTER_BYTES: usize = 4 * 1024 * 1024;
const MAXIMUM_INNER_BYTES: usize = 2 * 1024 * 1024;
const MAXIMUM_HELP_BYTES: usize = 1024 * 1024;
const MAXIMUM_PROVENANCE_BYTES: usize = 64 * 1024;
const MAXIMUM_COMMANDS: usize = 256;
const MAXIMUM_COMMAND_PATH_BYTES: usize = 256;
const MAXIMUM_STRING_BYTES: usize = 8 * 1024;
const REPOSITORY: &str = "IABTechLab/trusted-server";
const WORKFLOW_PATH: &str = ".github/workflows/test.yml";
const CAPTURE_MANIFEST: &str = "tools/docs-parity/manifests/cli-captures.toml";
const OVERRIDES_MANIFEST: &str = "tools/docs-parity/manifests/cli-overrides.toml";
const LINUX_GOLDEN: &str = "tools/docs-parity/goldens/cli-linux.txt";
const MACOS_GOLDEN: &str = "tools/docs-parity/goldens/cli-macos.txt";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CaptureOutputsState {
    Absent,
    Complete,
}

fn capture_outputs_state(present: [bool; 3]) -> Result<CaptureOutputsState, Report<CliHelpError>> {
    match present {
        [false, false, false] => Ok(CaptureOutputsState::Absent),
        [true, true, true] => Ok(CaptureOutputsState::Complete),
        _ => Err(cli_error(
            "CLI capture manifest and both platform goldens must be all absent or all present",
        )),
    }
}

/// Supported native capture platform.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    /// Native Linux capture.
    Linux,
    /// Native macOS capture.
    Macos,
}

impl Platform {
    /// Return the stable platform label used in artifacts.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Linux => "linux",
            Self::Macos => "macos",
        }
    }
}

/// Provenance captured beside native CLI help.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureContext {
    /// Closed schema version.
    pub schema_version: u32,
    /// Exact GitHub repository identity.
    pub repository: String,
    /// Platform derived from the build host.
    pub platform: Platform,
    /// Exact checked-out source SHA.
    pub source_sha: String,
    /// GitHub-hosted runner identity.
    pub runner_name: String,
    /// GitHub runner operating-system label.
    pub runner_os: String,
    /// GitHub runner architecture label.
    pub runner_arch: String,
    /// Raw `uname -a` output.
    pub uname: String,
    /// Raw `rustc -vV` output.
    pub rustc: String,
    /// Raw Node version.
    pub node: String,
    /// Checked repository tool-version record.
    pub tool_versions: String,
    /// GitHub Actions run ID.
    pub run_id: u64,
    /// GitHub Actions run attempt.
    pub run_attempt: u64,
}

/// Closed provenance stored inside a capture archive.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureProvenance {
    /// Closed schema version.
    pub schema_version: u32,
    /// Exact GitHub repository identity.
    pub repository: String,
    /// Platform derived from the build host.
    pub platform: Platform,
    /// Exact checked-out source SHA.
    pub source_sha: String,
    /// GitHub-hosted runner identity.
    pub runner_name: String,
    /// GitHub runner operating-system label.
    pub runner_os: String,
    /// GitHub runner architecture label.
    pub runner_arch: String,
    /// Raw `uname -a` output.
    pub uname: String,
    /// Raw `rustc -vV` output.
    pub rustc: String,
    /// Raw Node version.
    pub node: String,
    /// Checked repository tool-version record.
    pub tool_versions: String,
    /// GitHub Actions run ID.
    pub run_id: u64,
    /// GitHub Actions run attempt.
    pub run_attempt: u64,
    /// Exact captured help byte length.
    pub help_length: usize,
    /// SHA-256 of the captured help bytes.
    pub help_sha256: String,
}

/// Validated contents of one native inner capture archive.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatedCapture {
    /// Recursive help transcript.
    pub help: String,
    /// Authenticated capture provenance.
    pub provenance: CaptureProvenance,
}

/// Injectable source of help text for one CLI command path.
pub trait HelpRunner {
    /// Return stdout for `ts <path> --help`.
    ///
    /// # Errors
    ///
    /// Returns a bounded diagnostic when the native command cannot run.
    fn help(&mut self, path: &[String]) -> Result<String, String>;
}

/// Authenticated workflow-run metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostedRun {
    /// Run ID.
    pub id: u64,
    /// Run attempt.
    pub run_attempt: u64,
    /// Repository full name.
    pub repository: String,
    /// Workflow event.
    pub event: String,
    /// Terminal conclusion.
    pub conclusion: String,
    /// Pull-request number.
    pub pull_request: u64,
    /// Workflow repository path.
    pub workflow_path: String,
    /// Workflow head commit.
    pub head_sha: String,
    /// Attempt start as Unix seconds.
    pub started_at: u64,
    /// Attempt completion as Unix seconds.
    pub completed_at: u64,
}

/// Authenticated GitHub artifact metadata and raw outer ZIP.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostedArtifact {
    /// Artifact ID.
    pub id: u64,
    /// Exact artifact name.
    pub name: String,
    /// API-reported archive size.
    pub size_in_bytes: u64,
    /// Whether GitHub marks the artifact expired.
    pub expired: bool,
    /// Artifact creation time as Unix seconds.
    pub created_at: u64,
    /// API-reported SHA-256 digest.
    pub digest: String,
    /// Raw downloaded outer artifact ZIP.
    pub bytes: Vec<u8>,
}

/// Injectable authenticated GitHub client.
pub trait HostedClient {
    /// Read one workflow run through the authenticated session.
    ///
    /// # Errors
    ///
    /// Returns a bounded diagnostic when authenticated metadata cannot be read.
    fn run(&mut self, run_id: u64) -> Result<HostedRun, String>;

    /// Read the complete artifact list and bounded bytes for one run.
    ///
    /// # Errors
    ///
    /// Returns a bounded diagnostic when metadata or archive bytes cannot be read.
    fn artifacts(&mut self, run_id: u64) -> Result<Vec<HostedArtifact>, String>;
}

/// Byte outputs produced only after both hosted captures authenticate.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportedCaptures {
    /// Linux recursive help golden.
    pub linux_help: String,
    /// macOS recursive help golden.
    pub macos_help: String,
    /// Deterministic checked capture manifest.
    pub capture_manifest: String,
}

struct AuthenticatedImport {
    outputs: ImportedCaptures,
    source_sha: String,
}

/// CLI-help capture or import failure.
#[derive(Debug, derive_more::Display)]
#[display("invalid CLI-help capture: {detail}")]
pub struct CliHelpError {
    detail: String,
}

impl core::error::Error for CliHelpError {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureManifest {
    version: u32,
    reviewed: bool,
    repository: String,
    source_sha: String,
    run_id: u64,
    run_attempt: u64,
    captures: Vec<CaptureRecord>,
    sources: Vec<SourceBlob>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureRecord {
    platform: Platform,
    help_sha256: String,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceBlob {
    path: String,
    blob: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OverridesManifest {
    version: u32,
    reviewed: bool,
    #[serde(default)]
    overrides: Vec<HelpOverride>,
    #[serde(default)]
    annotations: Vec<PlatformAnnotation>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HelpOverride {
    command_path: String,
    platform: Platform,
    replacement: String,
    source_fingerprint: String,
    owner: String,
    rationale: String,
    expires_at: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PlatformAnnotation {
    command_path: String,
    platform: Platform,
    source_fingerprint: String,
    owner: String,
    rationale: String,
    expires_at: String,
}

/// Availability of one command in the checked native-help union.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HelpAvailability {
    /// Present with identical checked help on both hosts.
    Both,
    /// Present only in the Linux capture with an explicit annotation.
    LinuxOnly,
    /// Present only in the macOS capture with an explicit annotation.
    MacosOnly,
}

impl HelpAvailability {
    const fn annotation(self) -> Option<&'static str> {
        match self {
            Self::Both => None,
            Self::LinuxOnly => Some("linux-only"),
            Self::MacosOnly => Some("macos-only"),
        }
    }
}

/// One command in the deterministic checked native-help union.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckedHelpRecord {
    /// Exact command path, including the `ts` root.
    pub command_path: String,
    /// Checked native availability.
    pub availability: HelpAvailability,
    /// Exact reconciled help bytes as UTF-8.
    pub help: String,
}

/// Deterministic checked native-help data for documentation rendering.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckedHelp {
    /// Lexically ordered command records.
    pub records: Vec<CheckedHelpRecord>,
    /// Stable annotated transcript derived from `records`.
    pub rendered: String,
}

/// Fingerprint one exact command record rather than an entire platform transcript.
#[must_use]
pub fn transcript_record_fingerprint(command_path: &str, help: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(command_path.as_bytes());
    digest.update([0]);
    digest.update(help.as_bytes());
    format!("sha256:{:x}", digest.finalize())
}

/// Parse, reconcile, annotate, and deterministically render two native transcripts.
///
/// `now_seconds` is injected by tests and supplied from the production system
/// clock. Platform-only commands require exact annotations. Divergent shared
/// records require fingerprint-bound replacements that reconcile to one value.
///
/// # Errors
///
/// Returns an error for malformed transcripts or manifests, command-set drift,
/// missing or stale annotations, stale replacements, expired governance, or
/// unreconciled platform help.
pub fn render_checked_help(
    linux: &str,
    macos: &str,
    manifest_bytes: &[u8],
    now_seconds: u64,
) -> Result<CheckedHelp, Report<CliHelpError>> {
    if manifest_bytes.len() > MAXIMUM_PROVENANCE_BYTES || now_seconds > 253_402_300_799 {
        return Err(cli_error(
            "override manifest or current time exceeds bounds",
        ));
    }
    let manifest_text = core::str::from_utf8(manifest_bytes)
        .map_err(|_error| cli_error("CLI override manifest is not UTF-8"))?;
    let manifest = toml::from_str::<OverridesManifest>(manifest_text)
        .map_err(|_error| cli_error("CLI override manifest does not match the closed schema"))?;
    if manifest.version != 1 || !manifest.reviewed {
        return Err(cli_error(
            "CLI override manifest requires version 1 and review",
        ));
    }
    let linux = parse_transcript(linux)?;
    let macos = parse_transcript(macos)?;
    let overrides = validated_overrides(&manifest.overrides, &linux, &macos, now_seconds)?;
    let annotations = validated_annotations(&manifest.annotations, &linux, &macos, now_seconds)?;
    let paths = linux
        .keys()
        .chain(macos.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut used_overrides = BTreeSet::new();
    let mut used_annotations = BTreeSet::new();
    let mut records = Vec::with_capacity(paths.len());
    for path in paths {
        let linux_help = reconciled_help(
            &path,
            Platform::Linux,
            linux.get(&path),
            &overrides,
            &mut used_overrides,
        );
        let macos_help = reconciled_help(
            &path,
            Platform::Macos,
            macos.get(&path),
            &overrides,
            &mut used_overrides,
        );
        let (availability, help) = match (linux_help, macos_help) {
            (Some(linux_help), Some(macos_help)) => {
                if linux_help != macos_help {
                    return Err(cli_error(format!(
                        "platform help differs without reconciling overrides: {path}"
                    )));
                }
                (HelpAvailability::Both, linux_help)
            }
            (Some(help), None) => {
                require_annotation(&path, Platform::Linux, &annotations, &mut used_annotations)?;
                (HelpAvailability::LinuxOnly, help)
            }
            (None, Some(help)) => {
                require_annotation(&path, Platform::Macos, &annotations, &mut used_annotations)?;
                (HelpAvailability::MacosOnly, help)
            }
            (None, None) => return Err(cli_error("checked command union is internally empty")),
        };
        records.push(CheckedHelpRecord {
            command_path: path,
            availability,
            help,
        });
    }
    if used_overrides.len() != overrides.len() || used_annotations.len() != annotations.len() {
        return Err(cli_error("CLI override or platform annotation is stale"));
    }
    let mut rendered = String::new();
    for record in &records {
        rendered.push_str(&format!("=== {} ===\n", record.command_path));
        if let Some(annotation) = record.availability.annotation() {
            rendered.push_str(&format!("[platform: {annotation}]\n"));
        }
        rendered.push_str(&record.help);
        rendered.push('\n');
    }
    if rendered.len() > MAXIMUM_HELP_BYTES {
        return Err(cli_error("annotated CLI help exceeds 1048576 bytes"));
    }
    Ok(CheckedHelp { records, rendered })
}

/// Capture the recursive CLI help tree into a deterministic inner ZIP.
///
/// # Errors
///
/// Returns an error for invalid provenance, malformed or oversized help,
/// duplicate or unsafe command paths, command-count overflow, or ZIP failure.
pub fn capture_archive<R: HelpRunner>(
    context: &CaptureContext,
    runner: &mut R,
) -> Result<Vec<u8>, Report<CliHelpError>> {
    validate_context(context)?;
    let help = capture_recursive(runner)?;
    let provenance = CaptureProvenance {
        schema_version: context.schema_version,
        repository: context.repository.clone(),
        platform: context.platform,
        source_sha: context.source_sha.clone(),
        runner_name: context.runner_name.clone(),
        runner_os: context.runner_os.clone(),
        runner_arch: context.runner_arch.clone(),
        uname: context.uname.clone(),
        rustc: context.rustc.clone(),
        node: context.node.clone(),
        tool_versions: context.tool_versions.clone(),
        run_id: context.run_id,
        run_attempt: context.run_attempt,
        help_length: help.len(),
        help_sha256: format!("sha256:{:x}", Sha256::digest(help.as_bytes())),
    };
    validate_provenance(&provenance)?;
    let provenance_json = serde_json::to_vec(&provenance)
        .map_err(|_error| cli_error("cannot serialize provenance"))?;
    if provenance_json.len() > MAXIMUM_PROVENANCE_BYTES {
        return Err(cli_error("provenance exceeds 65536 bytes"));
    }
    deterministic_zip(&[
        ("cli-help.txt", help.as_bytes()),
        ("provenance.json", provenance_json.as_slice()),
    ])
}

/// Validate a native inner capture ZIP and its closed provenance.
///
/// # Errors
///
/// Returns an error for size, member, mode, traversal, JSON, hash, platform,
/// field, string, or command-tree violations.
pub fn validate_capture_archive(
    bytes: &[u8],
    platform: Platform,
) -> Result<ValidatedCapture, Report<CliHelpError>> {
    if bytes.len() > MAXIMUM_INNER_BYTES {
        return Err(cli_error("inner ZIP exceeds 2097152 bytes"));
    }
    let members = read_closed_zip(
        bytes,
        &[
            ("cli-help.txt", MAXIMUM_HELP_BYTES),
            ("provenance.json", MAXIMUM_PROVENANCE_BYTES),
        ],
    )?;
    let help =
        String::from_utf8(members[0].clone()).map_err(|_error| cli_error("help is not UTF-8"))?;
    let provenance = serde_json::from_slice::<CaptureProvenance>(&members[1])
        .map_err(|_error| cli_error("provenance does not match the closed schema"))?;
    validate_provenance(&provenance)?;
    if provenance.platform != platform
        || provenance.help_length != help.len()
        || provenance.help_sha256 != format!("sha256:{:x}", Sha256::digest(help.as_bytes()))
    {
        return Err(cli_error("platform, help length, or help digest differs"));
    }
    validate_transcript(&help)?;
    Ok(ValidatedCapture { help, provenance })
}

/// Authenticate and decode two same-attempt hosted CLI captures.
///
/// # Errors
///
/// Returns an error unless run metadata, exact artifacts, API digests,
/// timestamps, outer and inner archives, and provenance all agree with the
/// authenticated PR workflow and supplied current HEAD.
pub fn import_authenticated<C: HostedClient>(
    client: &mut C,
    run_id: u64,
    current_head: &str,
) -> Result<ImportedCaptures, Report<CliHelpError>> {
    Ok(authenticate_import(client, run_id, current_head)?.outputs)
}

fn authenticate_import<C: HostedClient>(
    client: &mut C,
    run_id: u64,
    current_head: &str,
) -> Result<AuthenticatedImport, Report<CliHelpError>> {
    let run = client.run(run_id).map_err(cli_error)?;
    validate_run(&run, run_id, current_head)?;
    let artifacts = client.artifacts(run_id).map_err(cli_error)?;
    if artifacts.len() != 2 {
        return Err(cli_error("run must contain exactly two CLI-help artifacts"));
    }
    let mut captures = Vec::new();
    for platform in [Platform::Linux, Platform::Macos] {
        let name = format!("cli-help-{}", platform.as_str());
        let matching = artifacts
            .iter()
            .filter(|artifact| artifact.name == name)
            .collect::<Vec<_>>();
        if matching.len() != 1 {
            return Err(cli_error(format!(
                "artifact {name} must appear exactly once"
            )));
        }
        let artifact = matching[0];
        validate_artifact_metadata(artifact, &run)?;
        let expected_inner = format!("{name}.zip");
        let inner = read_closed_zip(
            &artifact.bytes,
            &[(expected_inner.as_str(), MAXIMUM_INNER_BYTES)],
        )?
        .into_iter()
        .next()
        .ok_or_else(|| cli_error("outer artifact has no member"))?;
        let capture = validate_capture_archive(&inner, platform)?;
        if capture.provenance.repository != run.repository
            || capture.provenance.source_sha != run.head_sha
            || capture.provenance.run_id != run.id
            || capture.provenance.run_attempt != run.run_attempt
        {
            return Err(cli_error(
                "capture provenance differs from authenticated run",
            ));
        }
        captures.push(capture);
    }
    let linux = &captures[0];
    let macos = &captures[1];
    let capture_manifest = format!(
        concat!(
            "version = 1\nreviewed = true\n",
            "repository = \"IABTechLab/trusted-server\"\n",
            "source_sha = \"{}\"\nrun_id = {}\nrun_attempt = {}\n\n",
            "[[captures]]\nplatform = \"linux\"\nhelp_sha256 = \"{}\"\n\n",
            "[[captures]]\nplatform = \"macos\"\nhelp_sha256 = \"{}\"\n",
        ),
        run.head_sha,
        run.id,
        run.run_attempt,
        linux.provenance.help_sha256,
        macos.provenance.help_sha256,
    );
    Ok(AuthenticatedImport {
        outputs: ImportedCaptures {
            linux_help: linux.help.clone(),
            macos_help: macos.help.clone(),
            capture_manifest,
        },
        source_sha: run.head_sha,
    })
}

/// Detect the native capture platform without accepting a caller override.
#[must_use]
pub const fn detected_platform() -> Platform {
    #[cfg(target_os = "linux")]
    {
        Platform::Linux
    }
    #[cfg(target_os = "macos")]
    {
        Platform::Macos
    }
}

pub(crate) fn capture_repository(
    repository: &Repository,
    output: &Path,
) -> Result<(), Report<CliHelpError>> {
    let rustc = command_text("rustc", &["-vV"], 64 * 1024)?;
    let host = rustc
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .filter(|host| !host.is_empty() && !host.contains(char::is_whitespace))
        .ok_or_else(|| cli_error("rustc did not report a host target"))?;
    let mut build = Command::new("cargo");
    build
        .args(["build", "--package", "trusted-server-cli", "--target", host])
        .current_dir(repository.root())
        .env("TSJS_SKIP_BUILD", "1");
    let build = run_bounded_command(
        &mut build,
        4 * 1024 * 1024,
        4 * 1024 * 1024,
        Duration::from_secs(20 * 60),
    )
    .map_err(cli_error)?;
    if !build.success {
        return Err(cli_error("native ts build failed"));
    }
    let binary = repository.root().join("target").join(host).join("debug/ts");
    let source_sha = git_text(repository.root(), &["rev-parse", "HEAD"], 128)?;
    let tool_versions_path = NormalizedRelativePath::new(Path::new(".tool-versions"))
        .map_err(|_error| cli_error(".tool-versions path is invalid"))?;
    let tool_versions = repository
        .read_tracked_bounded(&tool_versions_path, MAXIMUM_STRING_BYTES)
        .map_err(|_error| cli_error("cannot read bounded tracked .tool-versions"))?;
    let tool_versions = String::from_utf8(tool_versions)
        .map_err(|_error| cli_error(".tool-versions is not UTF-8"))?;
    let context = CaptureContext {
        schema_version: 1,
        repository: required_environment("GITHUB_REPOSITORY")?,
        platform: detected_platform(),
        source_sha,
        runner_name: required_environment("RUNNER_NAME")?,
        runner_os: required_environment("RUNNER_OS")?,
        runner_arch: required_environment("RUNNER_ARCH")?,
        uname: command_text("uname", &["-a"], 64 * 1024)?,
        rustc,
        node: command_text("node", &["--version"], 64 * 1024)?,
        tool_versions,
        run_id: required_environment("GITHUB_RUN_ID")?
            .parse()
            .map_err(|_error| cli_error("GITHUB_RUN_ID is not an unsigned integer"))?,
        run_attempt: required_environment("GITHUB_RUN_ATTEMPT")?
            .parse()
            .map_err(|_error| cli_error("GITHUB_RUN_ATTEMPT is not an unsigned integer"))?,
    };
    let bytes = capture_archive(&context, &mut ProcessHelpRunner { binary })?;
    write_atomic_output(output, &bytes)
}

pub(crate) fn import_hosted_repository(
    repository: &Repository,
    run_id: u64,
) -> Result<(), Report<CliHelpError>> {
    let head = git_text(repository.root(), &["rev-parse", "HEAD"], 128)?;
    let mut client = GhHostedClient;
    let authenticated = authenticate_import(&mut client, run_id, &head)?;
    let captured_sources =
        verified_current_cli_blobs(&authenticated.source_sha, &mut |revision| {
            cli_blobs_at(repository.root(), revision)
        })?;
    let mut imported = authenticated.outputs;
    imported.capture_manifest = append_source_blobs(&imported.capture_manifest, &captured_sources);
    install_import_outputs(repository.root(), &imported, &mut |_event, _path| Ok(()))
}

pub(crate) fn check_repository(repository: &Repository) -> Result<(), Report<CliHelpError>> {
    let manifest_bytes = read_required(repository, CAPTURE_MANIFEST, MAXIMUM_PROVENANCE_BYTES)?;
    let manifest_text = core::str::from_utf8(&manifest_bytes)
        .map_err(|_error| cli_error("capture manifest is not UTF-8"))?;
    let manifest = toml::from_str::<CaptureManifest>(manifest_text)
        .map_err(|_error| cli_error("capture manifest does not match the closed schema"))?;
    if manifest.version != 1
        || !manifest.reviewed
        || manifest.repository != REPOSITORY
        || !lower_hex_sha(&manifest.source_sha)
        || manifest.run_id == 0
        || manifest.run_attempt == 0
        || manifest.captures.len() != 2
    {
        return Err(cli_error("capture manifest identity is invalid"));
    }
    let linux = read_required(repository, LINUX_GOLDEN, MAXIMUM_HELP_BYTES)?;
    let macos = read_required(repository, MACOS_GOLDEN, MAXIMUM_HELP_BYTES)?;
    let mut captured = BTreeMap::new();
    for record in &manifest.captures {
        if !valid_sha256(&record.help_sha256)
            || captured
                .insert(record.platform, record.help_sha256.as_str())
                .is_some()
        {
            return Err(cli_error("capture platform records are invalid"));
        }
    }
    for (platform, bytes) in [(Platform::Linux, &linux), (Platform::Macos, &macos)] {
        validate_transcript(
            core::str::from_utf8(bytes).map_err(|_error| cli_error("CLI golden is not UTF-8"))?,
        )?;
        if captured.get(&platform).copied()
            != Some(format!("sha256:{:x}", Sha256::digest(bytes)).as_str())
        {
            return Err(cli_error(format!(
                "{} golden digest differs",
                platform.as_str()
            )));
        }
    }
    let mut ancestor = Command::new("git");
    ancestor
        .args(["merge-base", "--is-ancestor", &manifest.source_sha, "HEAD"])
        .current_dir(repository.root());
    let ancestor = run_bounded_command(
        &mut ancestor,
        MAXIMUM_PROVENANCE_BYTES,
        MAXIMUM_PROVENANCE_BYTES,
        Duration::from_secs(30),
    )
    .map_err(cli_error)?;
    if !ancestor.success {
        return Err(cli_error("capture source is not an ancestor of HEAD"));
    }
    let current = cli_blobs_at(repository.root(), "HEAD")?;
    if manifest.sources.len() != current.len()
        || manifest
            .sources
            .iter()
            .zip(&current)
            .any(|(record, actual)| record.path != actual.path || record.blob != actual.blob)
    {
        return Err(cli_error("CLI source/blob set changed after capture"));
    }
    let overrides = read_overrides(repository)?;
    let now_seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_error| cli_error("system clock precedes Unix epoch"))?
        .as_secs();
    render_checked_help(
        core::str::from_utf8(&linux).map_err(|_error| cli_error("Linux golden is not UTF-8"))?,
        core::str::from_utf8(&macos).map_err(|_error| cli_error("macOS golden is not UTF-8"))?,
        &overrides,
        now_seconds,
    )?;
    Ok(())
}

pub(crate) fn check_repository_if_materialized(
    repository: &Repository,
) -> Result<(), Report<CliHelpError>> {
    let mut present = [false; 3];
    for (index, path) in [CAPTURE_MANIFEST, LINUX_GOLDEN, MACOS_GOLDEN]
        .into_iter()
        .enumerate()
    {
        let path = NormalizedRelativePath::new(Path::new(path))
            .map_err(|_error| cli_error("capture output path is invalid"))?;
        present[index] = repository
            .regular_file_exists(&path)
            .map_err(|_error| cli_error("cannot inspect capture output materialization"))?;
    }
    match capture_outputs_state(present)? {
        CaptureOutputsState::Absent => Ok(()),
        CaptureOutputsState::Complete => check_repository(repository),
    }
}

fn read_overrides(repository: &Repository) -> Result<Vec<u8>, Report<CliHelpError>> {
    let bytes = read_required(repository, OVERRIDES_MANIFEST, MAXIMUM_PROVENANCE_BYTES)?;
    core::str::from_utf8(&bytes)
        .map_err(|_error| cli_error("CLI override manifest is not UTF-8"))?;
    Ok(bytes)
}

fn parse_transcript(help: &str) -> Result<BTreeMap<String, String>, Report<CliHelpError>> {
    validate_transcript_shape(help)?;
    let mut records = BTreeMap::new();
    let mut offset = 0usize;
    while offset < help.len() {
        let rest = &help[offset..];
        let header_end = rest
            .find('\n')
            .ok_or_else(|| cli_error("help transcript header is unterminated"))?;
        let header = &rest[..header_end];
        let path = header
            .strip_prefix("=== ")
            .and_then(|value| value.strip_suffix(" ==="))
            .ok_or_else(|| cli_error("help transcript header is malformed"))?;
        validate_command_path(path)?;
        let body_start = offset + header_end + 1;
        let body_end = help[body_start..]
            .find("\n=== ")
            .map_or(help.len(), |index| body_start + index + 1);
        let body = &help[body_start..body_end];
        let command_help = body
            .strip_suffix('\n')
            .filter(|value| value.ends_with('\n') && !value.contains('\0'))
            .ok_or_else(|| cli_error("help transcript record framing differs"))?;
        if records.len() >= MAXIMUM_COMMANDS
            || command_help.len() > MAXIMUM_HELP_BYTES
            || records
                .insert(path.to_owned(), command_help.to_owned())
                .is_some()
        {
            return Err(cli_error(
                "help transcript record count, size, or identity differs",
            ));
        }
        offset = body_end;
    }
    if records.first_key_value().map(|(path, _help)| path.as_str()) != Some("ts") {
        return Err(cli_error("help transcript has no root record"));
    }
    for path in records.keys().filter(|path| path.as_str() != "ts") {
        let parent = path
            .rsplit_once(' ')
            .map(|(parent, _component)| parent)
            .ok_or_else(|| cli_error("help command has no parent"))?;
        if !records.contains_key(parent) {
            return Err(cli_error(format!("help command parent is absent: {path}")));
        }
    }
    Ok(records)
}

fn validate_transcript_shape(help: &str) -> Result<(), Report<CliHelpError>> {
    if help.is_empty()
        || help.len() > MAXIMUM_HELP_BYTES
        || !help.starts_with("=== ts ===\n")
        || !help.ends_with("\n\n")
        || help.contains('\0')
    {
        return Err(cli_error("help transcript framing or size differs"));
    }
    Ok(())
}

fn validate_command_path(path: &str) -> Result<(), Report<CliHelpError>> {
    if path.len() > MAXIMUM_COMMAND_PATH_BYTES
        || !path
            .split(' ')
            .enumerate()
            .all(|(index, part)| index == 0 && part == "ts" || index > 0 && valid_component(part))
    {
        return Err(cli_error("help transcript command path is unsafe"));
    }
    Ok(())
}

fn validated_overrides<'a>(
    records: &'a [HelpOverride],
    linux: &BTreeMap<String, String>,
    macos: &BTreeMap<String, String>,
    now_seconds: u64,
) -> Result<BTreeMap<(Platform, String), &'a HelpOverride>, Report<CliHelpError>> {
    let mut result = BTreeMap::new();
    for record in records {
        validate_governance(
            &record.command_path,
            &record.source_fingerprint,
            &record.owner,
            &record.rationale,
            &record.expires_at,
            now_seconds,
        )?;
        if record.replacement.is_empty()
            || !record.replacement.ends_with('\n')
            || record.replacement.len() > MAXIMUM_STRING_BYTES
            || record.replacement.contains('\0')
        {
            return Err(cli_error(
                "CLI replacement is empty, unframed, or oversized",
            ));
        }
        let source = platform_record(record.platform, &record.command_path, linux, macos)
            .ok_or_else(|| cli_error("CLI override command is absent"))?;
        if record.source_fingerprint != transcript_record_fingerprint(&record.command_path, source)
            || result
                .insert((record.platform, record.command_path.clone()), record)
                .is_some()
        {
            return Err(cli_error("CLI override is stale or duplicated"));
        }
    }
    Ok(result)
}

fn validated_annotations<'a>(
    records: &'a [PlatformAnnotation],
    linux: &BTreeMap<String, String>,
    macos: &BTreeMap<String, String>,
    now_seconds: u64,
) -> Result<BTreeMap<(Platform, String), &'a PlatformAnnotation>, Report<CliHelpError>> {
    let mut result = BTreeMap::new();
    for record in records {
        validate_governance(
            &record.command_path,
            &record.source_fingerprint,
            &record.owner,
            &record.rationale,
            &record.expires_at,
            now_seconds,
        )?;
        let source = platform_record(record.platform, &record.command_path, linux, macos)
            .ok_or_else(|| cli_error("platform annotation command is absent"))?;
        if record.source_fingerprint != transcript_record_fingerprint(&record.command_path, source)
            || result
                .insert((record.platform, record.command_path.clone()), record)
                .is_some()
        {
            return Err(cli_error("platform annotation is stale or duplicated"));
        }
    }
    Ok(result)
}

fn validate_governance(
    command_path: &str,
    source_fingerprint: &str,
    owner: &str,
    rationale: &str,
    expires_at: &str,
    now_seconds: u64,
) -> Result<(), Report<CliHelpError>> {
    validate_command_path(command_path)?;
    for value in [source_fingerprint, owner, rationale, expires_at] {
        if value.trim().is_empty() || value.len() > MAXIMUM_STRING_BYTES || value.contains('\0') {
            return Err(cli_error("CLI governance field is blank or oversized"));
        }
    }
    if !valid_sha256(source_fingerprint) {
        return Err(cli_error("CLI governance fingerprint is malformed"));
    }
    let expiry = parse_utc_seconds(expires_at)
        .ok_or_else(|| cli_error("CLI governance expiry is not canonical UTC"))?;
    if now_seconds >= expiry {
        return Err(cli_error("CLI governance record is expired"));
    }
    Ok(())
}

fn platform_record<'a>(
    platform: Platform,
    path: &str,
    linux: &'a BTreeMap<String, String>,
    macos: &'a BTreeMap<String, String>,
) -> Option<&'a str> {
    match platform {
        Platform::Linux => linux.get(path),
        Platform::Macos => macos.get(path),
    }
    .map(String::as_str)
}

fn reconciled_help(
    path: &str,
    platform: Platform,
    source: Option<&String>,
    overrides: &BTreeMap<(Platform, String), &HelpOverride>,
    used: &mut BTreeSet<(Platform, String)>,
) -> Option<String> {
    let source = source?;
    let key = (platform, path.to_owned());
    if let Some(record) = overrides.get(&key) {
        used.insert(key);
        Some(record.replacement.clone())
    } else {
        Some(source.clone())
    }
}

fn require_annotation(
    path: &str,
    platform: Platform,
    annotations: &BTreeMap<(Platform, String), &PlatformAnnotation>,
    used: &mut BTreeSet<(Platform, String)>,
) -> Result<(), Report<CliHelpError>> {
    let key = (platform, path.to_owned());
    if !annotations.contains_key(&key) {
        return Err(cli_error(format!(
            "platform-only command has no annotation: {path}"
        )));
    }
    used.insert(key);
    Ok(())
}

struct ProcessHelpRunner {
    binary: PathBuf,
}

impl HelpRunner for ProcessHelpRunner {
    fn help(&mut self, path: &[String]) -> Result<String, String> {
        let mut command = Command::new(&self.binary);
        command.args(path).arg("--help");
        let output = run_bounded_command(
            &mut command,
            MAXIMUM_HELP_BYTES,
            MAXIMUM_PROVENANCE_BYTES,
            Duration::from_secs(30),
        )?;
        if !output.success || !output.stderr.is_empty() || output.stdout.len() > MAXIMUM_HELP_BYTES
        {
            return Err(format!("native ts help failed for {}", path.join(" ")));
        }
        String::from_utf8(output.stdout)
            .map_err(|error| format!("native ts help is not UTF-8: {error}"))
    }
}

struct GhHostedClient;

impl HostedClient for GhHostedClient {
    fn run(&mut self, run_id: u64) -> Result<HostedRun, String> {
        let endpoint = format!("repos/{REPOSITORY}/actions/runs/{run_id}");
        let bytes = run_gh_api(&endpoint, MAXIMUM_PROVENANCE_BYTES)?;
        let raw = serde_json::from_slice::<GhRun>(&bytes)
            .map_err(|error| format!("decode authenticated workflow run: {error}"))?;
        Ok(HostedRun {
            id: raw.id,
            run_attempt: raw.run_attempt,
            repository: raw.repository.full_name,
            event: raw.event,
            conclusion: raw.conclusion,
            pull_request: raw
                .pull_requests
                .first()
                .ok_or_else(|| "workflow run has no pull request".to_owned())?
                .number,
            workflow_path: raw.path,
            head_sha: raw.head_sha,
            started_at: parse_utc_seconds(&raw.run_started_at)
                .ok_or_else(|| "workflow start time is invalid".to_owned())?,
            completed_at: parse_utc_seconds(&raw.updated_at)
                .ok_or_else(|| "workflow completion time is invalid".to_owned())?,
        })
    }

    fn artifacts(&mut self, run_id: u64) -> Result<Vec<HostedArtifact>, String> {
        let endpoint = format!("repos/{REPOSITORY}/actions/runs/{run_id}/artifacts?per_page=100");
        let bytes = run_gh_api(&endpoint, MAXIMUM_PROVENANCE_BYTES)?;
        let raw = serde_json::from_slice::<GhArtifacts>(&bytes)
            .map_err(|error| format!("decode authenticated artifacts: {error}"))?;
        if raw.total_count != raw.artifacts.len() as u64 {
            return Err("artifact response is paginated or inconsistent".to_owned());
        }
        raw.artifacts
            .into_iter()
            .map(|artifact| {
                if artifact.size_in_bytes > MAXIMUM_OUTER_BYTES as u64 {
                    return Err(format!("artifact {} exceeds outer bound", artifact.name));
                }
                let download = format!("repos/{REPOSITORY}/actions/artifacts/{}/zip", artifact.id);
                let bytes = run_gh_api(&download, MAXIMUM_OUTER_BYTES)?;
                Ok(HostedArtifact {
                    id: artifact.id,
                    name: artifact.name,
                    size_in_bytes: artifact.size_in_bytes,
                    expired: artifact.expired,
                    created_at: parse_utc_seconds(&artifact.created_at)
                        .ok_or_else(|| "artifact creation time is invalid".to_owned())?,
                    digest: artifact.digest,
                    bytes,
                })
            })
            .collect()
    }
}

#[derive(Deserialize)]
struct GhRun {
    id: u64,
    run_attempt: u64,
    event: String,
    conclusion: String,
    path: String,
    head_sha: String,
    run_started_at: String,
    updated_at: String,
    repository: GhRepository,
    pull_requests: Vec<GhPullRequest>,
}

#[derive(Deserialize)]
struct GhRepository {
    full_name: String,
}

#[derive(Deserialize)]
struct GhPullRequest {
    number: u64,
}

#[derive(Deserialize)]
struct GhArtifacts {
    total_count: u64,
    artifacts: Vec<GhArtifact>,
}

#[derive(Deserialize)]
struct GhArtifact {
    id: u64,
    name: String,
    size_in_bytes: u64,
    expired: bool,
    created_at: String,
    digest: String,
}

fn cli_blobs_at(root: &Path, revision: &str) -> Result<Vec<SourceBlob>, Report<CliHelpError>> {
    if revision != "HEAD" && !lower_hex_sha(revision) {
        return Err(cli_error("CLI source tree revision is invalid"));
    }
    let mut command = Command::new("git");
    command
        .args([
            "ls-tree",
            "-r",
            "--full-tree",
            revision,
            "--",
            ".cargo/config.toml",
            ".tool-versions",
            "Cargo.toml",
            "Cargo.lock",
            "rust-toolchain.toml",
            "crates/trusted-server-cli",
        ])
        .current_dir(root);
    let output = run_bounded_command(
        &mut command,
        MAXIMUM_PROVENANCE_BYTES,
        MAXIMUM_PROVENANCE_BYTES,
        Duration::from_secs(60),
    )
    .map_err(|detail| {
        cli_error(format!(
            "CLI source blob enumeration failed or exceeded bounds: {detail}"
        ))
    })?;
    if !output.success || !output.stderr.is_empty() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(cli_error(format!(
            "CLI source blob enumeration failed or exceeded bounds: success={}, stderr={stderr}",
            output.success
        )));
    }
    let text = core::str::from_utf8(&output.stdout)
        .map_err(|_error| cli_error("CLI source blob record is not UTF-8"))?;
    let mut sources = Vec::new();
    for line in text.lines() {
        let (metadata, path) = line
            .split_once('\t')
            .ok_or_else(|| cli_error("CLI source blob record is malformed"))?;
        let mut fields = metadata.split_ascii_whitespace();
        let mode = fields.next().unwrap_or_default();
        let kind = fields.next().unwrap_or_default();
        let blob = fields.next().unwrap_or_default();
        if mode != "100644"
            || kind != "blob"
            || blob.len() != 40
            || !blob
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
            || fields.next().is_some()
            || path.contains(['\r', '\n'])
        {
            return Err(cli_error("CLI source blob record is unsafe"));
        }
        sources.push(SourceBlob {
            path: path.to_owned(),
            blob: blob.to_ascii_lowercase(),
        });
    }
    sources.sort_unstable_by(|left, right| left.path.cmp(&right.path));
    if sources.is_empty() {
        return Err(cli_error("CLI source blob set is empty"));
    }
    Ok(sources)
}

fn require_matching_source_blobs(
    captured: &[SourceBlob],
    current: &[SourceBlob],
) -> Result<(), Report<CliHelpError>> {
    if captured.is_empty()
        || captured.len() != current.len()
        || captured.iter().zip(current).any(|(captured, current)| {
            captured.path != current.path || captured.blob != current.blob
        })
    {
        return Err(cli_error(
            "current CLI source set/tree blobs differ from authenticated capture SHA",
        ));
    }
    Ok(())
}

fn verified_current_cli_blobs<F>(
    authenticated_sha: &str,
    blobs_at: &mut F,
) -> Result<Vec<SourceBlob>, Report<CliHelpError>>
where
    F: FnMut(&str) -> Result<Vec<SourceBlob>, Report<CliHelpError>>,
{
    if !lower_hex_sha(authenticated_sha) {
        return Err(cli_error("authenticated CLI source SHA is invalid"));
    }
    let captured = blobs_at(authenticated_sha)?;
    let current = blobs_at("HEAD")?;
    require_matching_source_blobs(&captured, &current)?;
    Ok(captured)
}

fn append_source_blobs(manifest: &str, sources: &[SourceBlob]) -> String {
    let mut output = manifest.to_owned();
    for source in sources {
        output.push_str(&format!(
            "\n[[sources]]\npath = {:?}\nblob = {:?}\n",
            source.path, source.blob
        ));
    }
    output
}

fn read_required(
    repository: &Repository,
    path: &str,
    maximum: usize,
) -> Result<Vec<u8>, Report<CliHelpError>> {
    let path = NormalizedRelativePath::new(Path::new(path))
        .map_err(|_error| cli_error("required CLI path is invalid"))?;
    repository
        .read_tracked_bounded(&path, maximum)
        .map_err(|_error| {
            cli_error(format!(
                "cannot read required CLI path: {}",
                path.as_path().display()
            ))
        })
}

fn required_environment(name: &str) -> Result<String, Report<CliHelpError>> {
    let value = env::var(name).map_err(|_error| cli_error(format!("missing {name}")))?;
    if value.trim().is_empty() || value.len() > MAXIMUM_STRING_BYTES || value.contains('\0') {
        return Err(cli_error(format!("invalid {name}")));
    }
    Ok(value)
}

struct BoundedCommandOutput {
    success: bool,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

#[derive(Clone, Copy)]
enum CommandStream {
    Stdout,
    Stderr,
}

fn run_bounded_command(
    command: &mut Command,
    maximum_stdout: usize,
    maximum_stderr: usize,
    timeout: Duration,
) -> Result<BoundedCommandOutput, String> {
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| "bounded subprocess deadline overflow".to_owned())?;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|error| format!("start bounded subprocess: {error}"))?;
    let stdout = child.stdout.take().ok_or_else(|| {
        cleanup_child(&mut child, "bounded subprocess stdout is absent".to_owned())
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        cleanup_child(&mut child, "bounded subprocess stderr is absent".to_owned())
    })?;
    let (sender, receiver) = mpsc::channel();
    let stdout_sender = sender.clone();
    let stdout_reader = std::thread::Builder::new()
        .name("docs-parity-stdout".to_owned())
        .spawn(move || {
            let result = read_bounded_stream(stdout, maximum_stdout, "stdout");
            let _ignored = stdout_sender.send((CommandStream::Stdout, result));
        })
        .map_err(|error| cleanup_child(&mut child, format!("start stdout reader: {error}")))?;
    let stderr_reader = match std::thread::Builder::new()
        .name("docs-parity-stderr".to_owned())
        .spawn(move || {
            let result = read_bounded_stream(stderr, maximum_stderr, "stderr");
            let _ignored = sender.send((CommandStream::Stderr, result));
        }) {
        Ok(reader) => reader,
        Err(error) => {
            let mut stdout_reader = Some(stdout_reader);
            return Err(finish_failed_command(
                &mut child,
                &mut stdout_reader,
                &mut None,
                format!("start stderr reader: {error}"),
            ));
        }
    };
    let mut stdout_reader = Some(stdout_reader);
    let mut stderr_reader = Some(stderr_reader);
    let mut status = None;
    let mut stdout = None;
    let mut stderr = None;
    loop {
        loop {
            match receiver.try_recv() {
                Ok((CommandStream::Stdout, result)) => stdout = Some(result),
                Ok((CommandStream::Stderr, result)) => stderr = Some(result),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if stdout.is_none() || stderr.is_none() {
                        return Err(finish_failed_command(
                            &mut child,
                            &mut stdout_reader,
                            &mut stderr_reader,
                            "bounded subprocess reader disconnected".to_owned(),
                        ));
                    }
                    break;
                }
            }
        }
        let output_error = stdout
            .as_ref()
            .and_then(|result| result.as_ref().err())
            .or_else(|| stderr.as_ref().and_then(|result| result.as_ref().err()));
        if let Some(error) = output_error {
            return Err(finish_failed_command(
                &mut child,
                &mut stdout_reader,
                &mut stderr_reader,
                error.clone(),
            ));
        }
        if status.is_none() {
            match child.try_wait() {
                Ok(result) => status = result,
                Err(error) => {
                    return Err(finish_failed_command(
                        &mut child,
                        &mut stdout_reader,
                        &mut stderr_reader,
                        format!("poll bounded subprocess: {error}"),
                    ));
                }
            }
        }
        if status.is_some() && stdout.is_some() && stderr.is_some() {
            break;
        }
        if Instant::now() >= deadline {
            return Err(finish_failed_command(
                &mut child,
                &mut stdout_reader,
                &mut stderr_reader,
                "bounded subprocess deadline elapsed".to_owned(),
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let status = status.ok_or_else(|| "bounded subprocess status is absent".to_owned())?;
    let stdout =
        stdout.ok_or_else(|| "bounded subprocess stdout result is absent".to_owned())??;
    let stderr =
        stderr.ok_or_else(|| "bounded subprocess stderr result is absent".to_owned())??;
    join_reader(&mut stdout_reader, "stdout")?;
    join_reader(&mut stderr_reader, "stderr")?;
    Ok(BoundedCommandOutput {
        success: status.success(),
        stdout,
        stderr,
    })
}

fn finish_failed_command(
    child: &mut std::process::Child,
    stdout_reader: &mut Option<JoinHandle<()>>,
    stderr_reader: &mut Option<JoinHandle<()>>,
    primary: String,
) -> String {
    let mut diagnostics = vec![cleanup_child(child, primary)];
    for (reader, name) in [(stdout_reader, "stdout"), (stderr_reader, "stderr")] {
        if join_reader(reader, name).is_err() {
            diagnostics.push(format!("bounded subprocess {name} reader panicked"));
        }
    }
    diagnostics.join("; ")
}

fn join_reader(reader: &mut Option<JoinHandle<()>>, name: &str) -> Result<(), String> {
    reader
        .take()
        .ok_or_else(|| format!("bounded subprocess {name} reader handle is absent"))?
        .join()
        .map_err(|_panic| format!("bounded subprocess {name} reader panicked"))
}

fn cleanup_child(child: &mut std::process::Child, primary: String) -> String {
    let mut diagnostics = vec![primary];
    match child.try_wait() {
        Ok(Some(_status)) => return diagnostics.join("; "),
        Ok(None) => {}
        Err(error) => diagnostics.push(format!("cleanup poll failed: {error}")),
    }
    if let Err(error) = child.kill() {
        diagnostics.push(format!("cleanup kill failed: {error}"));
    }
    if let Err(error) = child.wait() {
        diagnostics.push(format!("cleanup wait failed: {error}"));
    }
    diagnostics.join("; ")
}

fn read_bounded_stream(
    stream: impl std::io::Read,
    maximum: usize,
    name: &str,
) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    stream
        .take((maximum as u64).saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| format!("read bounded subprocess {name}: {error}"))?;
    if bytes.len() > maximum {
        return Err(format!(
            "bounded subprocess {name} exceeds configured limit"
        ));
    }
    Ok(bytes)
}

fn command_text(
    program: &str,
    arguments: &[&str],
    maximum: usize,
) -> Result<String, Report<CliHelpError>> {
    let mut command = Command::new(program);
    command.args(arguments);
    let output = run_bounded_command(
        &mut command,
        maximum,
        MAXIMUM_PROVENANCE_BYTES,
        Duration::from_secs(30),
    )
    .map_err(cli_error)?;
    if !output.success || !output.stderr.is_empty() {
        return Err(cli_error(format!(
            "{program} output is invalid or oversized"
        )));
    }
    let text = String::from_utf8(output.stdout)
        .map_err(|_error| cli_error(format!("{program} output is not UTF-8")))?;
    Ok(text.trim_end_matches(['\r', '\n']).to_owned())
}

fn git_text(
    root: &Path,
    arguments: &[&str],
    maximum: usize,
) -> Result<String, Report<CliHelpError>> {
    let mut command = Command::new("git");
    command.args(arguments).current_dir(root);
    let output = run_bounded_command(
        &mut command,
        maximum,
        MAXIMUM_PROVENANCE_BYTES,
        Duration::from_secs(30),
    )
    .map_err(cli_error)?;
    if !output.success || !output.stderr.is_empty() {
        return Err(cli_error("Git output is invalid or oversized"));
    }
    String::from_utf8(output.stdout)
        .map(|value| value.trim_end_matches(['\r', '\n']).to_owned())
        .map_err(|_error| cli_error("Git output is not UTF-8"))
}

fn write_atomic_output(path: &Path, bytes: &[u8]) -> Result<(), Report<CliHelpError>> {
    let parent = path
        .parent()
        .ok_or_else(|| cli_error("capture output has no parent"))?;
    fs::create_dir_all(parent)
        .map_err(|_error| cli_error("cannot create capture output directory"))?;
    if path
        .symlink_metadata()
        .is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        return Err(cli_error("capture output cannot be a symlink"));
    }
    let mut stage = tempfile::Builder::new()
        .prefix(".docs-parity-cli-help-")
        .tempfile_in(parent)
        .map_err(|_error| cli_error("cannot create capture output stage"))?;
    stage
        .write_all(bytes)
        .map_err(|_error| cli_error("cannot write capture output stage"))?;
    #[cfg(unix)]
    stage
        .as_file()
        .set_permissions(fs::Permissions::from_mode(0o644))
        .map_err(|_error| cli_error("cannot set capture output stage mode"))?;
    stage
        .as_file()
        .sync_all()
        .map_err(|_error| cli_error("cannot sync capture output stage"))?;
    stage
        .persist(path)
        .map_err(|_error| cli_error("cannot commit capture output"))?;
    Ok(())
}

struct ImportOutputPlan {
    target: PathBuf,
    original: Option<Vec<u8>>,
    original_mode: u32,
    stage: Option<tempfile::NamedTempFile>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ImportInstallEvent {
    BeforeCommit(usize),
    BeforeSync(usize),
}

fn install_import_outputs<F>(
    root: &Path,
    imported: &ImportedCaptures,
    before_commit: &mut F,
) -> Result<(), Report<CliHelpError>>
where
    F: FnMut(ImportInstallEvent, &Path) -> Result<(), std::io::Error>,
{
    let outputs = [
        (
            LINUX_GOLDEN,
            imported.linux_help.as_bytes(),
            MAXIMUM_HELP_BYTES,
        ),
        (
            MACOS_GOLDEN,
            imported.macos_help.as_bytes(),
            MAXIMUM_HELP_BYTES,
        ),
        (
            CAPTURE_MANIFEST,
            imported.capture_manifest.as_bytes(),
            MAXIMUM_PROVENANCE_BYTES,
        ),
    ];
    let canonical_root = root
        .canonicalize()
        .map_err(|_error| cli_error("cannot canonicalize import root"))?;
    let mut plans = Vec::new();
    for (relative, contents, maximum) in outputs {
        if contents.len() > maximum {
            return Err(cli_error("import output exceeds its byte bound"));
        }
        NormalizedRelativePath::new(Path::new(relative))
            .map_err(|_error| cli_error("import target path is invalid"))?;
        let target = root.join(relative);
        let parent = target
            .parent()
            .ok_or_else(|| cli_error("import target has no parent"))?;
        fs::create_dir_all(parent)
            .map_err(|_error| cli_error("cannot create import target parent"))?;
        if !parent
            .canonicalize()
            .is_ok_and(|value| value.starts_with(&canonical_root))
        {
            return Err(cli_error("import target parent escapes repository"));
        }
        let metadata = target.symlink_metadata().ok();
        if metadata
            .as_ref()
            .is_some_and(|value| !value.is_file() || value.file_type().is_symlink())
        {
            return Err(cli_error("import target is not a regular file"));
        }
        let original = if let Some(metadata) = &metadata {
            if metadata.len() > maximum as u64 {
                return Err(cli_error("original import target exceeds bounds"));
            }
            let mut file = fs::File::open(&target)
                .map_err(|_error| cli_error("cannot open original import target"))?;
            let mut bytes = Vec::with_capacity(
                usize::try_from(metadata.len())
                    .map_err(|_error| cli_error("original import target size is invalid"))?,
            );
            std::io::Read::by_ref(&mut file)
                .take((maximum as u64).saturating_add(1))
                .read_to_end(&mut bytes)
                .map_err(|_error| cli_error("cannot read original import target"))?;
            if bytes.len() > maximum {
                return Err(cli_error("original import target exceeds bounds"));
            }
            Some(bytes)
        } else {
            None
        };
        #[cfg(unix)]
        let original_mode = metadata
            .as_ref()
            .map_or(0o644, |value| value.permissions().mode() & 0o777);
        #[cfg(not(unix))]
        let original_mode = 0o644;
        let mut stage = tempfile::Builder::new()
            .prefix(".docs-parity-cli-import-")
            .tempfile_in(parent)
            .map_err(|_error| cli_error("cannot create import output stage"))?;
        stage
            .write_all(contents)
            .map_err(|_error| cli_error("cannot write import output stage"))?;
        #[cfg(unix)]
        stage
            .as_file()
            .set_permissions(fs::Permissions::from_mode(original_mode))
            .map_err(|_error| cli_error("cannot set import output stage mode"))?;
        stage
            .as_file()
            .sync_all()
            .map_err(|_error| cli_error("cannot sync import output stage"))?;
        plans.push(ImportOutputPlan {
            target,
            original,
            original_mode,
            stage: Some(stage),
        });
    }

    for index in 0..plans.len() {
        if before_commit(
            ImportInstallEvent::BeforeCommit(index),
            &plans[index].target,
        )
        .is_err()
        {
            return fail_import_with_rollback(
                cli_error("injected import commit failure"),
                &plans[..index],
            );
        }
        let Some(stage) = plans[index].stage.take() else {
            return fail_import_with_rollback(
                cli_error("import output stage disappeared"),
                &plans[..index],
            );
        };
        if stage.persist(&plans[index].target).is_err() {
            return fail_import_with_rollback(
                cli_error("cannot commit import output stage"),
                &plans[..index],
            );
        }
        if before_commit(ImportInstallEvent::BeforeSync(index), &plans[index].target).is_err() {
            return fail_import_with_rollback(
                cli_error("injected import parent-sync failure"),
                &plans[..=index],
            );
        }
        if let Err(error) = sync_parent(&plans[index].target) {
            return fail_import_with_rollback(error, &plans[..=index]);
        }
    }
    Ok(())
}

fn fail_import_with_rollback(
    failure: Report<CliHelpError>,
    committed: &[ImportOutputPlan],
) -> Result<(), Report<CliHelpError>> {
    if let Err(rollback) = rollback_import_outputs(committed) {
        return Err(cli_error(format!(
            "CLI import failed and rollback also failed: {failure:?}; {rollback:?}"
        )));
    }
    Err(failure)
}

fn rollback_import_outputs(plans: &[ImportOutputPlan]) -> Result<(), Report<CliHelpError>> {
    for plan in plans.iter().rev() {
        match &plan.original {
            Some(original) => {
                write_atomic_output_with_mode(&plan.target, original, plan.original_mode)?
            }
            None => fs::remove_file(&plan.target)
                .map_err(|_error| cli_error("cannot remove rolled-back import target"))?,
        }
        sync_parent(&plan.target)?;
    }
    Ok(())
}

fn write_atomic_output_with_mode(
    path: &Path,
    bytes: &[u8],
    mode: u32,
) -> Result<(), Report<CliHelpError>> {
    let parent = path
        .parent()
        .ok_or_else(|| cli_error("rollback target has no parent"))?;
    let mut stage = tempfile::Builder::new()
        .prefix(".docs-parity-cli-rollback-")
        .tempfile_in(parent)
        .map_err(|_error| cli_error("cannot create import rollback stage"))?;
    stage
        .write_all(bytes)
        .and_then(|()| stage.as_file().sync_all())
        .map_err(|_error| cli_error("cannot write import rollback stage"))?;
    #[cfg(unix)]
    stage
        .as_file()
        .set_permissions(fs::Permissions::from_mode(mode))
        .map_err(|_error| cli_error("cannot set import rollback stage mode"))?;
    stage
        .persist(path)
        .map_err(|_error| cli_error("cannot restore import target"))?;
    Ok(())
}

fn sync_parent(path: &Path) -> Result<(), Report<CliHelpError>> {
    fs::File::open(
        path.parent()
            .ok_or_else(|| cli_error("import target has no parent"))?,
    )
    .and_then(|directory| directory.sync_all())
    .map_err(|_error| cli_error("cannot sync import target parent"))
}

fn run_gh_api(endpoint: &str, maximum: usize) -> Result<Vec<u8>, String> {
    let mut command = Command::new("gh");
    command.args(["api", endpoint]);
    let output = run_bounded_command(
        &mut command,
        maximum,
        MAXIMUM_PROVENANCE_BYTES,
        Duration::from_secs(60),
    )?;
    if !output.success || !output.stderr.is_empty() {
        return Err("authenticated gh api failed".to_owned());
    }
    Ok(output.stdout)
}

fn parse_utc_seconds(value: &str) -> Option<u64> {
    let bytes = value.as_bytes();
    if bytes.len() != 20
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
        || bytes[19] != b'Z'
    {
        return None;
    }
    let number = |start: usize, length: usize| {
        bytes
            .get(start..start + length)?
            .iter()
            .try_fold(0u64, |value, byte| {
                byte.is_ascii_digit()
                    .then(|| value * 10 + u64::from(*byte - b'0'))
            })
    };
    let year = number(0, 4)?;
    let month = number(5, 2)?;
    let day = number(8, 2)?;
    let hour = number(11, 2)?;
    let minute = number(14, 2)?;
    let second = number(17, 2)?;
    if year < 1970
        || !(1..=12).contains(&month)
        || day == 0
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }
    let leap_days_before = |year: u64| year / 4 - year / 100 + year / 400;
    let prior_year = year - 1;
    let days_before_year =
        365 * (year - 1970) + leap_days_before(prior_year) - leap_days_before(1969);
    let month_days = [31u64, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let leap = year.is_multiple_of(400) || (year.is_multiple_of(4) && !year.is_multiple_of(100));
    let maximum_day = month_days[(month - 1) as usize] + u64::from(month == 2 && leap);
    if day > maximum_day {
        return None;
    }
    let before_month =
        month_days[..(month - 1) as usize].iter().sum::<u64>() + u64::from(leap && month > 2);
    Some((days_before_year + before_month + day - 1) * 86_400 + hour * 3_600 + minute * 60 + second)
}

fn capture_recursive<R: HelpRunner>(runner: &mut R) -> Result<String, Report<CliHelpError>> {
    let mut queue = VecDeque::from([Vec::<String>::new()]);
    let mut seen = BTreeSet::new();
    let mut transcript = String::new();
    while let Some(path) = queue.pop_front() {
        let path_text = command_path(&path)?;
        if !seen.insert(path_text.clone()) {
            return Err(cli_error(format!("duplicate command path: {path_text}")));
        }
        if seen.len() > MAXIMUM_COMMANDS {
            return Err(cli_error("command count exceeds 256"));
        }
        let help = runner.help(&path).map_err(cli_error)?;
        if help.len() > MAXIMUM_HELP_BYTES || !help.ends_with('\n') || help.contains('\0') {
            return Err(cli_error(format!(
                "help is malformed or oversized: {path_text}"
            )));
        }
        transcript.push_str(&format!("=== {path_text} ===\n{help}"));
        if !help.ends_with("\n\n") {
            transcript.push('\n');
        }
        for child in help_subcommands(&help)? {
            let mut child_path = path.clone();
            child_path.push(child);
            queue.push_back(child_path);
        }
        if transcript.len() > MAXIMUM_HELP_BYTES {
            return Err(cli_error("recursive help exceeds 1048576 bytes"));
        }
    }
    Ok(transcript)
}

fn help_subcommands(help: &str) -> Result<Vec<String>, Report<CliHelpError>> {
    let mut in_commands = false;
    let mut commands = Vec::new();
    for line in help.lines() {
        if line == "Commands:" {
            if in_commands {
                return Err(cli_error("help contains duplicate Commands sections"));
            }
            in_commands = true;
            continue;
        }
        if !in_commands {
            continue;
        }
        if line.is_empty() || (!line.starts_with("  ") && !line.trim().is_empty()) {
            break;
        }
        let Some(trimmed) = line.strip_prefix("  ") else {
            continue;
        };
        if trimmed.starts_with(char::is_whitespace) {
            continue;
        }
        let Some((candidate, description)) = trimmed.split_once(char::is_whitespace) else {
            continue;
        };
        if description.trim().is_empty() || candidate == "help" {
            continue;
        }
        validate_command_component(candidate)?;
        commands.push(candidate.to_owned());
    }
    commands.sort_unstable();
    if commands.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(cli_error("help contains duplicate subcommands"));
    }
    Ok(commands)
}

fn validate_transcript(help: &str) -> Result<(), Report<CliHelpError>> {
    let records = parse_transcript(help)?;
    if records.len() > MAXIMUM_COMMANDS {
        return Err(cli_error("help transcript command count differs"));
    }
    Ok(())
}

fn command_path(path: &[String]) -> Result<String, Report<CliHelpError>> {
    for component in path {
        validate_command_component(component)?;
    }
    let result = if path.is_empty() {
        "ts".to_owned()
    } else {
        format!("ts {}", path.join(" "))
    };
    if result.len() > MAXIMUM_COMMAND_PATH_BYTES {
        return Err(cli_error("command path exceeds 256 bytes"));
    }
    Ok(result)
}

fn validate_command_component(value: &str) -> Result<(), Report<CliHelpError>> {
    if valid_component(value) {
        Ok(())
    } else {
        Err(cli_error(format!("unsafe command component: {value}")))
    }
}

fn valid_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
}

fn validate_context(context: &CaptureContext) -> Result<(), Report<CliHelpError>> {
    if context.schema_version != 1
        || context.repository != REPOSITORY
        || !lower_hex_sha(&context.source_sha)
        || context.run_id == 0
        || context.run_attempt == 0
    {
        return Err(cli_error("capture identity is invalid"));
    }
    for value in [
        &context.repository,
        &context.source_sha,
        &context.runner_name,
        &context.runner_os,
        &context.runner_arch,
        &context.uname,
        &context.rustc,
        &context.node,
        &context.tool_versions,
    ] {
        if value.len() > MAXIMUM_STRING_BYTES || value.contains('\0') {
            return Err(cli_error("provenance string exceeds bounds"));
        }
    }
    if [
        &context.runner_name,
        &context.runner_os,
        &context.runner_arch,
        &context.uname,
        &context.rustc,
        &context.node,
        &context.tool_versions,
    ]
    .iter()
    .any(|value| value.trim().is_empty())
    {
        return Err(cli_error("provenance field is blank"));
    }
    Ok(())
}

fn validate_provenance(provenance: &CaptureProvenance) -> Result<(), Report<CliHelpError>> {
    let context = CaptureContext {
        schema_version: provenance.schema_version,
        repository: provenance.repository.clone(),
        platform: provenance.platform,
        source_sha: provenance.source_sha.clone(),
        runner_name: provenance.runner_name.clone(),
        runner_os: provenance.runner_os.clone(),
        runner_arch: provenance.runner_arch.clone(),
        uname: provenance.uname.clone(),
        rustc: provenance.rustc.clone(),
        node: provenance.node.clone(),
        tool_versions: provenance.tool_versions.clone(),
        run_id: provenance.run_id,
        run_attempt: provenance.run_attempt,
    };
    validate_context(&context)?;
    if provenance.help_length == 0
        || provenance.help_length > MAXIMUM_HELP_BYTES
        || !valid_sha256(&provenance.help_sha256)
    {
        return Err(cli_error("help provenance is invalid"));
    }
    Ok(())
}

fn validate_run(run: &HostedRun, run_id: u64, head: &str) -> Result<(), Report<CliHelpError>> {
    if run.id != run_id
        || run.repository != REPOSITORY
        || run.event != "pull_request"
        || run.conclusion != "success"
        || run.pull_request != 1049
        || run.workflow_path != WORKFLOW_PATH
        || run.head_sha != head
        || !lower_hex_sha(head)
        || run.run_attempt == 0
        || run.started_at > run.completed_at
    {
        return Err(cli_error(
            "workflow run is not the authenticated PR #1049 head",
        ));
    }
    Ok(())
}

fn validate_artifact_metadata(
    artifact: &HostedArtifact,
    run: &HostedRun,
) -> Result<(), Report<CliHelpError>> {
    if artifact.id == 0
        || artifact.expired
        || artifact.size_in_bytes > MAXIMUM_OUTER_BYTES as u64
        || artifact.bytes.len() > MAXIMUM_OUTER_BYTES
        || artifact.created_at < run.started_at
        || artifact.created_at > run.completed_at
        || !valid_sha256(&artifact.digest)
        || artifact.digest != format!("sha256:{:x}", Sha256::digest(&artifact.bytes))
    {
        return Err(cli_error(format!(
            "artifact metadata differs: {}",
            artifact.name
        )));
    }
    Ok(())
}

fn deterministic_zip(members: &[(&str, &[u8])]) -> Result<Vec<u8>, Report<CliHelpError>> {
    let cursor = Cursor::new(Vec::new());
    let mut writer = ZipWriter::new(cursor);
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Stored)
        .unix_permissions(0o644);
    for (name, contents) in members {
        writer
            .start_file(*name, options)
            .map_err(|_error| cli_error("cannot create ZIP member"))?;
        writer
            .write_all(contents)
            .map_err(|_error| cli_error("cannot write ZIP member"))?;
    }
    let bytes = writer
        .finish()
        .map_err(|_error| cli_error("cannot finish ZIP"))?
        .into_inner();
    if bytes.len() > MAXIMUM_INNER_BYTES {
        return Err(cli_error("inner ZIP exceeds 2097152 bytes"));
    }
    Ok(bytes)
}

fn read_closed_zip(
    bytes: &[u8],
    expected: &[(&str, usize)],
) -> Result<Vec<Vec<u8>>, Report<CliHelpError>> {
    validate_zip_framing(bytes, expected.len())?;
    let mut archive =
        ZipArchive::new(Cursor::new(bytes)).map_err(|_error| cli_error("cannot parse ZIP"))?;
    if archive.len() != expected.len() {
        return Err(cli_error("ZIP member count differs"));
    }
    let mut result = Vec::new();
    for (index, (name, maximum)) in expected.iter().enumerate() {
        let mut member = archive
            .by_index(index)
            .map_err(|_error| cli_error("cannot read ZIP member"))?;
        if member.name() != *name
            || member.enclosed_name().is_none()
            || !member.is_file()
            || !safe_regular_mode(member.unix_mode())
            || member.size() > *maximum as u64
        {
            return Err(cli_error(
                "ZIP member name, type, mode, order, or size differs",
            ));
        }
        let mut contents = Vec::with_capacity(
            usize::try_from(member.size())
                .map_err(|_error| cli_error("ZIP member is too large"))?,
        );
        std::io::Read::take(&mut member, (*maximum as u64).saturating_add(1))
            .read_to_end(&mut contents)
            .map_err(|_error| cli_error("cannot read ZIP member"))?;
        if contents.len() > *maximum {
            return Err(cli_error("ZIP member exceeds bound"));
        }
        result.push(contents);
    }
    Ok(result)
}

fn validate_zip_framing(bytes: &[u8], expected_members: usize) -> Result<(), Report<CliHelpError>> {
    let end_offset = bytes
        .len()
        .checked_sub(22)
        .ok_or_else(|| cli_error("ZIP end record is absent"))?;
    let end = &bytes[end_offset..];
    let read_u16 = |offset: usize| {
        end.get(offset..offset + 2)
            .map(|value| u16::from_le_bytes([value[0], value[1]]))
    };
    let read_u32 = |offset: usize| {
        end.get(offset..offset + 4)
            .map(|value| u32::from_le_bytes([value[0], value[1], value[2], value[3]]))
    };
    let member_count = u16::try_from(expected_members)
        .map_err(|_error| cli_error("ZIP expected member count exceeds format bounds"))?;
    let directory_size = read_u32(12).ok_or_else(|| cli_error("ZIP end record is truncated"))?;
    let directory_offset = read_u32(16).ok_or_else(|| cli_error("ZIP end record is truncated"))?;
    if end.get(..4) != Some(b"PK\x05\x06")
        || bytes.get(..4) != Some(b"PK\x03\x04")
        || read_u16(4) != Some(0)
        || read_u16(6) != Some(0)
        || read_u16(8) != Some(member_count)
        || read_u16(10) != Some(member_count)
        || read_u16(20) != Some(0)
        || usize::try_from(directory_offset).ok().and_then(|offset| {
            usize::try_from(directory_size)
                .ok()
                .and_then(|size| offset.checked_add(size))
        }) != Some(end_offset)
    {
        return Err(cli_error(
            "ZIP preamble, trailer, disk, comment, or directory framing differs",
        ));
    }
    Ok(())
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

fn valid_sha256(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    })
}

fn cli_error(detail: impl Into<String>) -> Report<CliHelpError> {
    Report::new(CliHelpError {
        detail: detail.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggregate_capture_state_skips_only_a_completely_absent_output_set() {
        assert_eq!(
            capture_outputs_state([false, false, false]).expect("should classify absent outputs"),
            CaptureOutputsState::Absent
        );
        assert_eq!(
            capture_outputs_state([true, true, true]).expect("should classify complete outputs"),
            CaptureOutputsState::Complete
        );
        for partial in [
            [true, false, false],
            [false, true, false],
            [false, false, true],
            [true, true, false],
            [true, false, true],
            [false, true, true],
        ] {
            assert!(
                capture_outputs_state(partial).is_err(),
                "partial capture outputs must fail: {partial:?}"
            );
        }
    }

    #[test]
    fn import_source_blobs_require_exact_authenticated_tree_equality() {
        let captured = vec![SourceBlob {
            path: "crates/trusted-server-cli/src/lib.rs".to_owned(),
            blob: "0123456789abcdef0123456789abcdef01234567".to_owned(),
        }];
        assert!(
            require_matching_source_blobs(&captured, &captured).is_ok(),
            "identical authenticated and current trees should pass"
        );
        let changed = vec![SourceBlob {
            path: captured[0].path.clone(),
            blob: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
        }];
        assert!(
            require_matching_source_blobs(&captured, &changed).is_err(),
            "changed current tree blob must fail before import writes"
        );
        assert!(
            require_matching_source_blobs(&captured, &[]).is_err(),
            "changed current source set must fail before import writes"
        );
    }

    #[test]
    fn import_source_lookup_uses_the_authenticated_sha_before_current_head() {
        let capture_sha = "0123456789abcdef0123456789abcdef01234567";
        let expected = vec![SourceBlob {
            path: "crates/trusted-server-cli/src/lib.rs".to_owned(),
            blob: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
        }];
        let mut revisions = Vec::new();

        let actual = verified_current_cli_blobs(capture_sha, &mut |revision| {
            revisions.push(revision.to_owned());
            Ok(expected.clone())
        })
        .expect("identical captured and current source trees should pass");

        assert_eq!(revisions, [capture_sha, "HEAD"]);
        assert_eq!(
            actual
                .iter()
                .map(|source| (source.path.as_str(), source.blob.as_str()))
                .collect::<Vec<_>>(),
            [(
                "crates/trusted-server-cli/src/lib.rs",
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            )]
        );
    }

    #[test]
    fn three_file_import_rolls_back_every_target_after_an_injected_failure() {
        let root = tempfile::tempdir().expect("should create import root");
        let old = [
            (LINUX_GOLDEN, b"old-linux\n".as_slice()),
            (MACOS_GOLDEN, b"old-macos\n".as_slice()),
            (CAPTURE_MANIFEST, b"old-manifest\n".as_slice()),
        ];
        for (path, contents) in old {
            let target = root.path().join(path);
            fs::create_dir_all(target.parent().expect("should have parent"))
                .expect("should create target parent");
            fs::write(target, contents).expect("should write original target");
        }
        let imported = ImportedCaptures {
            linux_help: "new-linux\n".to_owned(),
            macos_help: "new-macos\n".to_owned(),
            capture_manifest: "new-manifest\n".to_owned(),
        };
        let mut before_commit = |event: ImportInstallEvent, _path: &Path| {
            if event == ImportInstallEvent::BeforeCommit(1) {
                Err(std::io::Error::other("injected commit failure"))
            } else {
                Ok(())
            }
        };

        assert!(
            install_import_outputs(root.path(), &imported, &mut before_commit).is_err(),
            "injected second-target failure must reject the import"
        );
        for (path, contents) in old {
            assert_eq!(
                fs::read(root.path().join(path)).expect("should read rolled-back target"),
                contents,
                "every prior target should be restored"
            );
        }
    }

    #[test]
    fn three_file_import_removes_new_targets_after_post_commit_failure() {
        let root = tempfile::tempdir().expect("should create import root");
        let imported = ImportedCaptures {
            linux_help: "new-linux\n".to_owned(),
            macos_help: "new-macos\n".to_owned(),
            capture_manifest: "new-manifest\n".to_owned(),
        };
        let mut fail_sync = |event: ImportInstallEvent, _path: &Path| {
            if event == ImportInstallEvent::BeforeSync(1) {
                Err(std::io::Error::other("injected parent sync failure"))
            } else {
                Ok(())
            }
        };

        assert!(
            install_import_outputs(root.path(), &imported, &mut fail_sync).is_err(),
            "a post-commit sync failure must reject the complete import"
        );
        for path in [LINUX_GOLDEN, MACOS_GOLDEN, CAPTURE_MANIFEST] {
            assert!(
                !root.path().join(path).exists(),
                "a newly created target must be removed during rollback: {path}"
            );
        }
    }

    #[test]
    fn subprocess_runner_bounds_both_streams_and_reaps_on_deadline() {
        let mut stdout_overflow = Command::new("/bin/sh");
        stdout_overflow.args(["-c", "printf 12345"]);
        assert!(
            run_bounded_command(&mut stdout_overflow, 4, 4, Duration::from_secs(1)).is_err(),
            "stdout overflow must fail while bounded"
        );

        let mut stderr_overflow = Command::new("/bin/sh");
        stderr_overflow.args(["-c", "printf 12345 >&2"]);
        assert!(
            run_bounded_command(&mut stderr_overflow, 4, 4, Duration::from_secs(1)).is_err(),
            "stderr overflow must fail while bounded"
        );

        let started = Instant::now();
        let mut deadline = Command::new("/bin/sh");
        deadline.args(["-c", "sleep 5"]);
        assert!(
            run_bounded_command(&mut deadline, 4, 4, Duration::from_millis(50)).is_err(),
            "deadline must fail and reap the child"
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "deadline cleanup must be bounded"
        );
    }
}
