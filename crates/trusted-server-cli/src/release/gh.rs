//! Bounded, isolated invocation of the pinned GitHub CLI.
//!
//! gh is resolved once (explicit trusted path or one trusted `PATH` lookup)
//! to a canonical executable that every later invocation reuses. Its
//! `--version` text must report exactly the qualified version; that is a
//! compatibility diagnostic only, because version text never authenticates an
//! executable.
//!
//! Each child gets a cleared environment carrying only the explicit token (as
//! `GH_TOKEN`), private home/config/state/cache directories and disabled
//! prompts. It runs in its own process group with fixed argument vectors, a
//! per-child and per-attempt deadline, cancellation, and concurrently drained
//! byte-capped pipes. On any limit the whole group is killed and the leader
//! reaped; a descendant that escaped the group and still holds the pipes is
//! reported instead of awaited. A failed exit never yields parsed claims.

use std::ffi::OsString;
use std::fs::DirBuilder;
use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use error_stack::Report;
use serde_json::Value;
use tempfile::TempDir;

use super::bounds::{self, AttemptDeadline, Cancellation, ProcessBounds};
use super::documents::{ApprovedRelease, ReleaseTag, Role, parse_json};
use super::environment::{CallerEnvironment, GithubToken};
use super::error::{ReleaseError, report};
use super::policy::{
    GH_VERSION, GITHUB_HOST, ISSUER, PROVENANCE_PREDICATE, REPOSITORY, SBOM_PREDICATE,
    certificate_identity,
};
use super::process::supervise;

const MINIMAL_PATH: &str = "/usr/bin:/bin";
const STDERR_EXCERPT_BYTES: usize = 4096;
const VERSION_EXCERPT_CHARS: usize = 128;
/// `gh release` subcommands take a host-qualified repository.
fn release_repository() -> String {
    format!("{GITHUB_HOST}/{REPOSITORY}")
}

/// Where to find the gh executable.
#[derive(Clone, Copy, Debug)]
pub(crate) enum GhLocation<'a> {
    /// An absolute path from trusted installer configuration.
    Explicit(&'a Path),
    /// One lookup through the absolute entries of the snapshot's `PATH`.
    SearchPath,
}

/// The attestation predicate a verification requires.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PredicateKind {
    /// SLSA build provenance.
    Provenance,
    /// SPDX SBOM.
    Spdx,
}

impl PredicateKind {
    fn bundle_suffix(self) -> &'static str {
        match self {
            Self::Provenance => ".provenance.sigstore.json",
            Self::Spdx => ".sbom.sigstore.json",
        }
    }

    fn uri(self) -> &'static str {
        match self {
            Self::Provenance => PROVENANCE_PREDICATE,
            Self::Spdx => SBOM_PREDICATE,
        }
    }
}

fn absolute(path: &Path, what: &str) -> Result<OsString, Report<ReleaseError>> {
    if !path.is_absolute() {
        return Err(report(
            ReleaseError::Policy,
            format!("{what} path must be absolute"),
        ));
    }
    Ok(path.as_os_str().to_owned())
}

/// Builds the fixed `gh attestation verify` argument vector.
///
/// `bundle` must be the retained, membership-checked companion bundle for
/// `predicate` (`.provenance.sigstore.json` or `.sbom.sigstore.json`), so gh
/// verifies exactly that bundle instead of fetching attestations itself.
///
/// # Errors
///
/// Returns [`ReleaseError::Policy`] unless `subject` and `bundle` are absolute
/// (so neither can be parsed as a flag) and the bundle name matches the
/// predicate.
pub(crate) fn attestation_verify_args(
    subject: &Path,
    bundle: &Path,
    release: &ApprovedRelease,
    predicate: PredicateKind,
) -> Result<Vec<OsString>, Report<ReleaseError>> {
    let subject = absolute(subject, "attestation subject")?;
    let bundle_name = bundle.file_name().map(|name| name.to_string_lossy());
    if !bundle_name.is_some_and(|name| name.ends_with(predicate.bundle_suffix())) {
        return Err(report(
            ReleaseError::Policy,
            "attestation bundle does not match the requested predicate",
        ));
    }
    let bundle = absolute(bundle, "attestation bundle")?;
    let commit = release.commit().as_str();
    let fixed = ["attestation".to_owned(), "verify".to_owned()].map(OsString::from);
    let options = [
        "-R".to_owned(),
        REPOSITORY.to_owned(),
        "--hostname".to_owned(),
        GITHUB_HOST.to_owned(),
        "--cert-identity".to_owned(),
        certificate_identity(release.tag()),
        "--cert-oidc-issuer".to_owned(),
        ISSUER.to_owned(),
        "--source-ref".to_owned(),
        format!("refs/tags/{}", release.tag().as_str()),
        "--source-digest".to_owned(),
        commit.to_owned(),
        "--signer-digest".to_owned(),
        commit.to_owned(),
        "--deny-self-hosted-runners".to_owned(),
        "--predicate-type".to_owned(),
        predicate.uri().to_owned(),
        "--format".to_owned(),
        "json".to_owned(),
    ]
    .map(OsString::from);
    Ok(fixed
        .into_iter()
        .chain([subject, OsString::from("--bundle"), bundle])
        .chain(options)
        .collect())
}

/// Builds the fixed `gh release verify` argument vector.
pub(crate) fn release_verify_args(tag: &ReleaseTag) -> Vec<OsString> {
    vec![
        OsString::from("release"),
        OsString::from("verify"),
        OsString::from(tag.as_str()),
        OsString::from("-R"),
        OsString::from(release_repository()),
    ]
}

/// Builds the fixed `gh release verify-asset` argument vector.
///
/// # Errors
///
/// Returns [`ReleaseError::Policy`] unless `asset` is absolute.
pub(crate) fn release_verify_asset_args(
    tag: &ReleaseTag,
    asset: &Path,
) -> Result<Vec<OsString>, Report<ReleaseError>> {
    let asset = absolute(asset, "release asset")?;
    Ok(vec![
        OsString::from("release"),
        OsString::from("verify-asset"),
        OsString::from(tag.as_str()),
        asset,
        OsString::from("-R"),
        OsString::from(release_repository()),
    ])
}

/// Captured output of a successful child.
#[derive(Debug)]
pub(crate) struct ChildOutput {
    pub(crate) stdout: Vec<u8>,
    /// Lossily decoded stderr with the credential redacted.
    pub(crate) stderr: String,
}

/// Private directories and the reviewed non-secret variables for every gh child.
///
/// The credential is never stored here; it stays in [`GithubToken`] (whose
/// `Debug` is redacted) and is injected only when a child is spawned.
#[derive(Debug)]
struct ChildEnvironment {
    root: TempDir,
    variables: Vec<(OsString, OsString)>,
}

/// Fixed non-secret settings for every gh child.
const FIXED_VARIABLES: [(&str, &str); 8] = [
    ("GH_PROMPT_DISABLED", "1"),
    ("GH_NO_UPDATE_NOTIFIER", "1"),
    ("GH_NO_EXTENSION_UPDATE_NOTIFIER", "1"),
    ("GH_SPINNER_DISABLED", "1"),
    ("DO_NOT_TRACK", "1"),
    ("NO_COLOR", "1"),
    ("LC_ALL", "C"),
    ("PATH", MINIMAL_PATH),
];

/// Private directory variables, relative to the isolated root.
const PRIVATE_DIRECTORIES: [(&str, &str); 7] = [
    ("GH_CONFIG_DIR", "config/gh"),
    ("XDG_CONFIG_HOME", "config"),
    ("XDG_STATE_HOME", "state"),
    ("XDG_CACHE_HOME", "cache"),
    ("XDG_DATA_HOME", "data"),
    ("TMPDIR", "tmp"),
    ("HOME", "home"),
];

fn reject_loader_injection(caller: &CallerEnvironment) -> Result<(), Report<ReleaseError>> {
    let injected = caller.names().find(|name| {
        let name = name.to_string_lossy();
        name.starts_with("LD_") || name.starts_with("DYLD_")
    });
    match injected {
        Some(name) => Err(report(
            ReleaseError::Environment,
            format!(
                "dynamic-loader variable {} is set; refusing to run gh",
                name.to_string_lossy()
            ),
        )),
        None => Ok(()),
    }
}

impl ChildEnvironment {
    fn isolated(caller: &CallerEnvironment) -> Result<(Self, GithubToken), Report<ReleaseError>> {
        reject_loader_injection(caller)?;
        let token = GithubToken::select(caller)?;
        let root = tempfile::Builder::new()
            .prefix("ts-release-gh-")
            .tempdir()
            .map_err(|source| report(ReleaseError::Environment, source))?;
        let mut variables = Vec::with_capacity(PRIVATE_DIRECTORIES.len() + FIXED_VARIABLES.len());
        for (name, relative) in PRIVATE_DIRECTORIES {
            let path = root.path().join(relative);
            DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&path)
                .map_err(|source| report(ReleaseError::Environment, source))?;
            variables.push((OsString::from(name), path.into_os_string()));
        }
        variables.extend(
            FIXED_VARIABLES.map(|(name, value)| (OsString::from(name), OsString::from(value))),
        );
        Ok((Self { root, variables }, token))
    }
}

fn is_executable_file(path: &Path) -> bool {
    std::fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

fn locate(
    location: GhLocation<'_>,
    caller: &CallerEnvironment,
) -> Result<PathBuf, Report<ReleaseError>> {
    let candidate = match location {
        GhLocation::Explicit(path) => {
            if !path.is_absolute() {
                return Err(report(
                    ReleaseError::UnsupportedTool,
                    "explicit gh path must be absolute",
                ));
            }
            path.to_path_buf()
        }
        GhLocation::SearchPath => caller
            .get("PATH")
            .and_then(|search| {
                std::env::split_paths(search)
                    .filter(|directory| directory.is_absolute())
                    .map(|directory| directory.join("gh"))
                    .find(|candidate| is_executable_file(candidate))
            })
            .ok_or_else(|| {
                report(
                    ReleaseError::UnsupportedTool,
                    "gh was not found in any absolute PATH entry",
                )
            })?,
    };
    let canonical = candidate
        .canonicalize()
        .map_err(|source| report(ReleaseError::UnsupportedTool, source))?;
    if !is_executable_file(&canonical) {
        return Err(report(
            ReleaseError::UnsupportedTool,
            "gh path is not an executable regular file",
        ));
    }
    Ok(canonical)
}

fn check_version(stdout: &[u8], token: &GithubToken) -> Result<(), Report<ReleaseError>> {
    let text = String::from_utf8_lossy(stdout);
    let first = text.lines().next().unwrap_or_default();
    let qualified = first.strip_prefix("gh version ").is_some_and(|rest| {
        let (version, tail) = rest.split_once(' ').unwrap_or((rest, ""));
        version == GH_VERSION && (tail.is_empty() || (tail.starts_with('(') && tail.ends_with(')')))
    });
    if qualified {
        return Ok(());
    }
    let excerpt: String = token
        .redact(first)
        .chars()
        .take(VERSION_EXCERPT_CHARS)
        .collect();
    Err(report(
        ReleaseError::UnsupportedTool,
        format!(
            "gh reported `{excerpt}`; release verification requires exactly gh {GH_VERSION}. \
             This is a compatibility diagnostic: version text does not authenticate the executable"
        ),
    ))
}

/// A resolved, version-checked gh executable with an isolated environment.
#[derive(Debug)]
pub(crate) struct GhRunner {
    executable: PathBuf,
    environment: ChildEnvironment,
    token: GithubToken,
    bounds: ProcessBounds,
}

impl GhRunner {
    /// Resolves gh once, isolates its environment and checks its version.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Environment`] for loader injection or a missing
    /// explicit token, [`ReleaseError::UnsupportedTool`] when gh is missing or
    /// not exactly the qualified version, or a process error from the
    /// version check.
    pub(crate) fn resolve(
        location: GhLocation<'_>,
        caller: &CallerEnvironment,
        bounds: ProcessBounds,
        deadline: AttemptDeadline,
        cancellation: &Cancellation,
    ) -> Result<Self, Report<ReleaseError>> {
        let (environment, token) = ChildEnvironment::isolated(caller)?;
        let executable = locate(location, caller)?;
        let runner = Self {
            executable,
            environment,
            token,
            bounds,
        };
        let output = runner.run(&[OsString::from("--version")], deadline, cancellation)?;
        check_version(&output.stdout, &runner.token)?;
        Ok(runner)
    }

    /// Replaces the process bounds; tests use short deadlines and caps while
    /// production keeps [`ProcessBounds::production`].
    #[cfg(test)]
    pub(crate) fn with_bounds(mut self, bounds: ProcessBounds) -> Self {
        self.bounds = bounds;
        self
    }

    /// Returns the canonical executable every invocation uses.
    pub(crate) fn executable(&self) -> &Path {
        &self.executable
    }

    /// Runs gh with a fixed argument vector under all bounds.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Timeout`], [`ReleaseError::Cancelled`],
    /// [`ReleaseError::OutputLimit`], [`ReleaseError::DescendantEscaped`],
    /// [`ReleaseError::Process`] or [`ReleaseError::ExitFailure`]; output from
    /// a failed child is never returned.
    pub(crate) fn run(
        &self,
        args: &[OsString],
        deadline: AttemptDeadline,
        cancellation: &Cancellation,
    ) -> Result<ChildOutput, Report<ReleaseError>> {
        let mut command = Command::new(&self.executable);
        command
            .args(args)
            .env_clear()
            .envs(self.environment.variables.iter().cloned())
            .env("GH_TOKEN", self.token.expose())
            .current_dir(self.environment.root.path())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        let completed = supervise(&mut command, &self.bounds, deadline, cancellation)?;
        let stderr = self
            .token
            .redact(&String::from_utf8_lossy(&completed.stderr));
        if !completed.status.success() {
            let excerpt: String = stderr.chars().take(STDERR_EXCERPT_BYTES).collect();
            return Err(report(
                ReleaseError::ExitFailure,
                format!("gh {}; stderr: {excerpt}", completed.status),
            ));
        }
        Ok(ChildOutput {
            stdout: completed.stdout,
            stderr,
        })
    }

    /// Runs one fixed `gh attestation verify` against one retained,
    /// membership-checked bundle and returns its single result.
    ///
    /// # Errors
    ///
    /// Returns any [`Self::run`] error, [`ReleaseError::Document`] for
    /// malformed or incomplete JSON, or [`ReleaseError::Output`] unless stdout
    /// is an array of exactly one attestation result.
    pub(crate) fn verify_attestation(
        &self,
        subject: &Path,
        bundle: &Path,
        release: &ApprovedRelease,
        predicate: PredicateKind,
        deadline: AttemptDeadline,
        cancellation: &Cancellation,
    ) -> Result<Value, Report<ReleaseError>> {
        let args = attestation_verify_args(subject, bundle, release, predicate)?;
        let output = self.run(&args, deadline, cancellation)?;
        let value = parse_json(&output.stdout, Role::GhOutput)?;
        let Value::Array(results) = value else {
            return Err(report(
                ReleaseError::Output,
                "gh attestation output must be a JSON array",
            ));
        };
        if results.len() != bounds::MATCHING_ATTESTATIONS_PER_BUNDLE {
            return Err(report(
                ReleaseError::Output,
                format!(
                    "gh returned {} attestation results; exactly one is required",
                    results.len()
                ),
            ));
        }
        results
            .into_iter()
            .next()
            .ok_or_else(|| report(ReleaseError::Output, "gh returned no attestation result"))
    }
}
