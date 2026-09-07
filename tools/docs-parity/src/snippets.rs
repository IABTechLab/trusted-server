//! Checked Markdown fence classifications and isolated execution.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use error_stack::Report;
use pulldown_cmark::{CodeBlockKind, Event, Parser, Tag, TagEnd};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};

use crate::model::Expiry;
use crate::repository::{NormalizedRelativePath, Repository};

const MANIFEST_VERSION: u32 = 1;
const MAXIMUM_MANIFEST_BYTES: usize = 1024 * 1024;
const MAXIMUM_SNIPPETS: usize = 2_048;
const MAXIMUM_SNIPPET_BYTES: usize = 1024 * 1024;
const MAXIMUM_STRING_BYTES: usize = 2_048;
const MANIFEST_PATH: &str = "tools/docs-parity/manifests/snippets.toml";
const VALIDATOR_COMMAND: &str = "validate syntax";
const VALIDATOR_TARGET: &str = "host";
const VALIDATOR_TIMEOUT: Duration = Duration::from_secs(15);
const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Failure while validating or executing checked snippets.
#[derive(Debug, derive_more::Display)]
#[display("invalid documentation snippet: {detail}")]
pub struct SnippetError {
    detail: String,
}

impl core::error::Error for SnippetError {}

/// One Markdown source presented to the snippet checker.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SnippetSource {
    /// Repository-relative Markdown path.
    pub path: String,
    /// Exact Markdown bytes decoded as UTF-8.
    pub markdown: String,
}

/// Bounded outcome of one isolated snippet command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SnippetExecution {
    /// Whether the command exited successfully.
    pub success: bool,
    /// Stable execution phase such as `compile`, `validation`, or `execute`.
    pub phase: String,
    /// Bounded diagnostic text.
    pub diagnostic: String,
}

/// Injectable isolated snippet runner.
pub trait SnippetRunner {
    /// Execute one snippet command in the supplied fresh directory.
    ///
    /// # Errors
    ///
    /// Returns a bounded operational diagnostic when execution cannot finish.
    fn run(
        &mut self,
        runner: &str,
        target: &str,
        command: &str,
        contents: &str,
        working_directory: &Path,
    ) -> Result<SnippetExecution, String>;
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SnippetManifest {
    version: u32,
    reviewed: bool,
    #[serde(default)]
    commands: Vec<SnippetCommand>,
    #[serde(default)]
    snippets: Vec<SnippetRecord>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SnippetCommand {
    id: String,
    runner: String,
    target: String,
    language: String,
    mode: SnippetMode,
    command: Option<String>,
    expected_phase: Option<String>,
    expected_diagnostic: Option<String>,
    owner: Option<String>,
    rationale: Option<String>,
    expires_at: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum SnippetMode {
    Executable,
    ExpectedFailure,
    Illustrative,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SnippetRecord {
    path: String,
    selector: String,
    command: String,
    fingerprint: String,
}

struct Fence {
    selector: String,
    language: String,
    contents: String,
}

/// Check manifest equality, fingerprints, modes, and execution oracles.
///
/// # Errors
///
/// Returns an error for unknown fields, missing/stale/duplicate records,
/// changed fence bytes, invalid or expired waivers, or a command result that
/// differs from its declared mode, phase, and stable diagnostic.
pub fn check_snippets<R: SnippetRunner>(
    manifest_bytes: &[u8],
    sources: &[SnippetSource],
    now: &str,
    runner: &mut R,
) -> Result<(), Report<SnippetError>> {
    let manifest = parse_manifest(manifest_bytes)?;
    Expiry::parse(now.to_owned())
        .map_err(|_error| snippet_error("current time is not canonical"))?;
    let commands = command_map(&manifest)?;
    let mut actual = BTreeMap::new();
    for source in sources {
        validate_path(&source.path)?;
        if source.markdown.len() > MAXIMUM_SNIPPET_BYTES {
            return Err(snippet_error(format!(
                "source exceeds byte bound: {}",
                source.path
            )));
        }
        for fence in fences(&source.markdown)? {
            let key = (source.path.clone(), fence.selector.clone());
            if actual
                .insert(key.clone(), (fence.language, fence.contents))
                .is_some()
            {
                return Err(snippet_error(format!(
                    "duplicate fence: {} {}",
                    key.0, key.1
                )));
            }
        }
    }
    let mut records = BTreeMap::new();
    for record in &manifest.snippets {
        validate_path(&record.path)?;
        validate_selector(&record.selector)?;
        validate_fingerprint(&record.fingerprint)?;
        if !commands.contains_key(&record.command) {
            return Err(snippet_error(format!(
                "unknown command: {}",
                record.command
            )));
        }
        let key = (record.path.clone(), record.selector.clone());
        if records.insert(key.clone(), record).is_some() {
            return Err(snippet_error(format!(
                "duplicate classification: {} {}",
                key.0, key.1
            )));
        }
    }
    let actual_keys = actual.keys().cloned().collect::<BTreeSet<_>>();
    let record_keys = records.keys().cloned().collect::<BTreeSet<_>>();
    if actual_keys != record_keys {
        return Err(snippet_error(format!(
            "fence classification mismatch; unlisted={:?}, stale={:?}",
            actual_keys.difference(&record_keys).next(),
            record_keys.difference(&actual_keys).next()
        )));
    }
    for (key, (language, contents)) in actual {
        let record = records
            .get(&key)
            .ok_or_else(|| snippet_error("classified fence disappeared"))?;
        let actual_fingerprint = format!("sha256:{:x}", Sha256::digest(contents.as_bytes()));
        if actual_fingerprint != record.fingerprint {
            return Err(snippet_error(format!(
                "stale fingerprint: {} {}",
                key.0, key.1
            )));
        }
        let command = commands
            .get(&record.command)
            .ok_or_else(|| snippet_error("command disappeared"))?;
        if command.language != language {
            return Err(snippet_error(format!(
                "fence language differs: {} {}",
                key.0, key.1
            )));
        }
        execute_mode(command, &contents, now, runner)?;
    }
    Ok(())
}

pub(crate) fn check_repository(repository: &Repository) -> Result<(), Report<SnippetError>> {
    let manifest_path = NormalizedRelativePath::new(Path::new(MANIFEST_PATH))
        .map_err(|_error| snippet_error("manifest path is invalid"))?;
    let bytes = repository
        .read_tracked_bounded(&manifest_path, MAXIMUM_MANIFEST_BYTES)
        .map_err(|_error| snippet_error("cannot read snippet manifest"))?;
    let classification = crate::classification::checked_markdown_sources(repository)
        .map_err(|_error| snippet_error("cannot resolve checked Markdown source universe"))?;
    let mut sources = Vec::new();
    for path in &classification.included_paths {
        let relative = NormalizedRelativePath::new(Path::new(path))
            .map_err(|_error| snippet_error(format!("invalid snippet path: {path}")))?;
        let contents = repository
            .read_tracked_bounded(&relative, MAXIMUM_SNIPPET_BYTES)
            .map_err(|_error| snippet_error(format!("cannot read snippet source: {path}")))?;
        let markdown = String::from_utf8(contents)
            .map_err(|_error| snippet_error(format!("snippet source is not UTF-8: {path}")))?;
        sources.push(SnippetSource {
            path: path.clone(),
            markdown,
        });
    }
    let now = current_utc()?;
    check_snippets(&bytes, &sources, &now, &mut ProcessSnippetRunner)
}

struct ProcessSnippetRunner;

impl SnippetRunner for ProcessSnippetRunner {
    fn run(
        &mut self,
        runner: &str,
        target: &str,
        command: &str,
        contents: &str,
        working_directory: &Path,
    ) -> Result<SnippetExecution, String> {
        if target != VALIDATOR_TARGET || command != VALIDATOR_COMMAND {
            return Err("snippet validator command is outside the closed grammar".to_owned());
        }
        let extension = match runner {
            "rust-syntax" => "rs",
            "node-syntax" => "js",
            "shell-syntax" => "sh",
            "json" => "json",
            "toml" => "toml",
            "yaml" => "yaml",
            _ => return Err(format!("unsupported runner: {runner}")),
        };
        let snippet_path = working_directory.join(format!("snippet.{extension}"));
        fs::write(&snippet_path, contents).map_err(|error| format!("write snippet: {error}"))?;
        if matches!(runner, "json" | "toml" | "yaml" | "rust-syntax") {
            let result = match runner {
                "json" => serde_json::from_str::<serde_json::Value>(contents)
                    .map(|_value| ())
                    .map_err(|_error| "JSON syntax is invalid"),
                "toml" => toml::from_str::<toml::Value>(contents)
                    .map(|_value| ())
                    .map_err(|_error| "TOML syntax is invalid"),
                "yaml" => serde_yaml::from_str::<serde_yaml::Value>(contents)
                    .map(|_value| ())
                    .map_err(|_error| "YAML syntax is invalid"),
                "rust-syntax" => syn::parse_file(contents)
                    .map(|_file| ())
                    .map_err(|_error| "Rust syntax is invalid"),
                _ => unreachable!("matched parser runner"),
            };
            return Ok(SnippetExecution {
                success: result.is_ok(),
                phase: "validation".to_owned(),
                diagnostic: result.err().unwrap_or_default().to_owned(),
            });
        }
        let (program, arguments) = if runner == "shell-syntax" {
            (
                std::path::PathBuf::from("/bin/bash"),
                vec!["-n".to_owned(), snippet_path.to_string_lossy().into_owned()],
            )
        } else if runner == "node-syntax" {
            (
                resolve_node_executable(working_directory)?,
                vec![
                    "--check".to_owned(),
                    snippet_path.to_string_lossy().into_owned(),
                ],
            )
        } else {
            return Err(format!("unsupported process runner: {runner}"));
        };
        let output = run_validator_process(
            &program,
            &arguments,
            working_directory,
            MAXIMUM_SNIPPET_BYTES,
            VALIDATOR_TIMEOUT,
        )?;
        let mut diagnostic = String::from_utf8_lossy(&output.stderr).into_owned();
        diagnostic.push_str(&String::from_utf8_lossy(&output.stdout));
        Ok(SnippetExecution {
            success: output.success,
            phase: "validation".to_owned(),
            diagnostic,
        })
    }
}

struct ValidatorOutput {
    success: bool,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

fn run_validator_process(
    executable: &Path,
    arguments: &[String],
    working_directory: &Path,
    maximum_output_bytes: usize,
    timeout: Duration,
) -> Result<ValidatorOutput, String> {
    if !executable.is_absolute() || timeout.is_zero() {
        return Err("validator executable and timeout are invalid".to_owned());
    }
    let per_stream_limit = maximum_output_bytes / 2;
    if per_stream_limit == 0 {
        return Err("validator output bound is too small".to_owned());
    }
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| "validator deadline overflowed".to_owned())?;
    let mut child = Command::new(executable)
        .args(arguments)
        .current_dir(working_directory)
        .env_clear()
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("cannot start validator: {error}"))?;
    let stdout = child.stdout.take().ok_or_else(|| {
        cleanup_validator(&mut child);
        "validator stdout pipe is unavailable".to_owned()
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        cleanup_validator(&mut child);
        "validator stderr pipe is unavailable".to_owned()
    })?;
    let stdout_reader = thread::Builder::new()
        .name("docs-parity-snippet-stdout".to_owned())
        .spawn(move || read_validator_stream(stdout, per_stream_limit))
        .map_err(|error| {
            cleanup_validator(&mut child);
            format!("cannot start validator stdout reader: {error}")
        })?;
    let stderr_reader = match thread::Builder::new()
        .name("docs-parity-snippet-stderr".to_owned())
        .spawn(move || read_validator_stream(stderr, per_stream_limit))
    {
        Ok(reader) => reader,
        Err(error) => {
            cleanup_validator(&mut child);
            let _joined = stdout_reader.join();
            return Err(format!("cannot start validator stderr reader: {error}"));
        }
    };
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(
                PROCESS_POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now())),
            ),
            Ok(None) => {
                cleanup_validator(&mut child);
                let _stdout = stdout_reader.join();
                let _stderr = stderr_reader.join();
                return Err("validator exceeded wall-clock timeout".to_owned());
            }
            Err(error) => {
                cleanup_validator(&mut child);
                let _stdout = stdout_reader.join();
                let _stderr = stderr_reader.join();
                return Err(format!("cannot inspect validator: {error}"));
            }
        }
    };
    let stdout = stdout_reader
        .join()
        .map_err(|_panic| "validator stdout reader panicked".to_owned())??;
    let stderr = stderr_reader
        .join()
        .map_err(|_panic| "validator stderr reader panicked".to_owned())??;
    if stdout.len().saturating_add(stderr.len()) > maximum_output_bytes {
        return Err(format!(
            "validator output exceeds {maximum_output_bytes} bytes"
        ));
    }
    Ok(ValidatorOutput {
        success: status.success(),
        stdout,
        stderr,
    })
}

fn read_validator_stream(
    mut stream: impl Read,
    maximum_output_bytes: usize,
) -> Result<Vec<u8>, String> {
    let limit = maximum_output_bytes
        .checked_add(1)
        .ok_or_else(|| "validator stream bound overflowed".to_owned())?;
    let mut bytes = Vec::new();
    stream
        .by_ref()
        .take(limit as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read validator output: {error}"))?;
    if bytes.len() > maximum_output_bytes {
        return Err(format!(
            "validator stream exceeds {maximum_output_bytes} bytes"
        ));
    }
    Ok(bytes)
}

fn cleanup_validator(child: &mut Child) {
    if child.try_wait().ok().flatten().is_none() {
        let _kill = child.kill();
    }
    let _wait = child.wait();
}

fn resolve_node_executable(working_directory: &Path) -> Result<std::path::PathBuf, String> {
    let path = std::env::var_os("PATH").ok_or_else(|| "PATH is unavailable".to_owned())?;
    for directory in std::env::split_paths(&path) {
        let candidate = directory.join("node");
        let Ok(canonical) = fs::canonicalize(&candidate) else {
            continue;
        };
        if canonical.starts_with(working_directory) {
            continue;
        }
        if is_executable_file(&canonical) && !is_script(&canonical) {
            return Ok(canonical);
        }
        if candidate.parent().and_then(Path::file_name) == Some(std::ffi::OsStr::new("shims")) {
            let Some(manager_root) = candidate.parent().and_then(Path::parent) else {
                continue;
            };
            let Ok(entries) = fs::read_dir(manager_root.join("installs/nodejs")) else {
                continue;
            };
            let mut binaries = entries
                .take(129)
                .filter_map(Result::ok)
                .map(|entry| entry.path().join("bin/node"))
                .filter_map(|path| fs::canonicalize(path).ok())
                .filter(|path| is_executable_file(path) && !is_script(path))
                .collect::<Vec<_>>();
            if binaries.len() > 128 {
                return Err("Node version-manager inventory exceeds 128 entries".to_owned());
            }
            binaries.sort_unstable();
            if let Some(binary) = binaries.pop() {
                return Ok(binary);
            }
        }
    }
    Err("cannot resolve an absolute Node validator executable".to_owned())
}

fn is_script(path: &Path) -> bool {
    let Ok(mut file) = fs::File::open(path) else {
        return true;
    };
    let mut prefix = [0_u8; 2];
    file.read_exact(&mut prefix).is_ok() && prefix == *b"#!"
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        true
    }
}

fn current_utc() -> Result<String, Report<SnippetError>> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_error| snippet_error("system clock precedes Unix epoch"))?
        .as_secs();
    format_utc(seconds).ok_or_else(|| snippet_error("system clock is outside canonical UTC range"))
}

fn format_utc(seconds: u64) -> Option<String> {
    let days = i64::try_from(seconds / 86_400).ok()?;
    let seconds_of_day = seconds % 86_400;
    let shifted = days.checked_add(719_468)?;
    let era = shifted / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    let hour = seconds_of_day / 3_600;
    let minute = seconds_of_day % 3_600 / 60;
    let second = seconds_of_day % 60;
    if !(0..=9_999).contains(&year) {
        return None;
    }
    Some(format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z"
    ))
}

fn parse_manifest(bytes: &[u8]) -> Result<SnippetManifest, Report<SnippetError>> {
    if bytes.len() > MAXIMUM_MANIFEST_BYTES {
        return Err(snippet_error("manifest exceeds 1048576 bytes"));
    }
    let text =
        core::str::from_utf8(bytes).map_err(|_error| snippet_error("manifest is not UTF-8"))?;
    let manifest = toml::from_str::<SnippetManifest>(text)
        .map_err(|_error| snippet_error("manifest does not match the closed schema"))?;
    if manifest.version != MANIFEST_VERSION
        || !manifest.reviewed
        || manifest.snippets.len() > MAXIMUM_SNIPPETS
    {
        return Err(snippet_error(
            "version, review, or snippet count is invalid",
        ));
    }
    Ok(manifest)
}

fn command_map(
    manifest: &SnippetManifest,
) -> Result<BTreeMap<String, &SnippetCommand>, Report<SnippetError>> {
    let mut result = BTreeMap::new();
    for command in &manifest.commands {
        for (field, value) in [
            ("id", command.id.as_str()),
            ("runner", command.runner.as_str()),
            ("target", command.target.as_str()),
            ("language", command.language.as_str()),
        ] {
            validate_string(value, field)?;
        }
        if command.id.is_empty()
            || !matches!(
                command.runner.as_str(),
                "none" | "json" | "toml" | "yaml" | "rust-syntax" | "shell-syntax" | "node-syntax"
            )
            || command.target.is_empty()
            || command.language.is_empty()
        {
            return Err(snippet_error(
                "command identity, runner, or target is invalid",
            ));
        }
        match command.mode {
            SnippetMode::Executable => {
                require_some(command.command.as_deref(), "executable command")?;
                require_none(command, false)?;
                validate_validator_identity(command)?;
            }
            SnippetMode::ExpectedFailure => {
                require_some(command.command.as_deref(), "expected-failure command")?;
                require_some(command.expected_phase.as_deref(), "expected phase")?;
                require_some(
                    command.expected_diagnostic.as_deref(),
                    "expected diagnostic",
                )?;
                if command.owner.is_some()
                    || command.rationale.is_some()
                    || command.expires_at.is_some()
                {
                    return Err(snippet_error("expected-failure command has waiver fields"));
                }
                validate_validator_identity(command)?;
            }
            SnippetMode::Illustrative => {
                if command.runner != "none"
                    || command.target != "none"
                    || command.command.is_some()
                    || command.expected_phase.is_some()
                    || command.expected_diagnostic.is_some()
                {
                    return Err(snippet_error("illustrative command has executable fields"));
                }
                require_some(command.owner.as_deref(), "waiver owner")?;
                require_some(command.rationale.as_deref(), "waiver rationale")?;
                let expiry = require_some(command.expires_at.as_deref(), "waiver expiry")?;
                Expiry::parse(expiry.to_owned())
                    .map_err(|_error| snippet_error("waiver expiry is invalid"))?;
            }
        }
        if result.insert(command.id.clone(), command).is_some() {
            return Err(snippet_error(format!(
                "duplicate command id: {}",
                command.id
            )));
        }
    }
    Ok(result)
}

fn validate_validator_identity(command: &SnippetCommand) -> Result<(), Report<SnippetError>> {
    let runner_matches_language = match command.runner.as_str() {
        "json" => command.language == "json",
        "toml" => command.language == "toml",
        "yaml" => command.language == "yaml",
        "rust-syntax" => command.language == "rust",
        "shell-syntax" => matches!(command.language.as_str(), "bash" | "sh"),
        "node-syntax" => matches!(command.language.as_str(), "javascript" | "js"),
        _ => false,
    };
    if command.target != VALIDATOR_TARGET
        || command.command.as_deref() != Some(VALIDATOR_COMMAND)
        || !runner_matches_language
    {
        return Err(snippet_error(
            "executable command is outside the closed validator grammar",
        ));
    }
    Ok(())
}

fn require_none(
    command: &SnippetCommand,
    expected_failure: bool,
) -> Result<(), Report<SnippetError>> {
    if command.owner.is_some()
        || command.rationale.is_some()
        || command.expires_at.is_some()
        || (!expected_failure
            && (command.expected_phase.is_some() || command.expected_diagnostic.is_some()))
    {
        return Err(snippet_error("command contains fields outside its mode"));
    }
    Ok(())
}

fn require_some<'a>(value: Option<&'a str>, field: &str) -> Result<&'a str, Report<SnippetError>> {
    let value = value.ok_or_else(|| snippet_error(format!("missing {field}")))?;
    validate_string(value, "mode field")?;
    if value.trim().is_empty() {
        return Err(snippet_error(format!("blank {field}")));
    }
    Ok(value)
}

fn execute_mode<R: SnippetRunner>(
    command: &SnippetCommand,
    contents: &str,
    now: &str,
    runner: &mut R,
) -> Result<(), Report<SnippetError>> {
    if command.mode == SnippetMode::Illustrative {
        let expiry = command
            .expires_at
            .as_deref()
            .ok_or_else(|| snippet_error("illustrative command has no expiry"))?;
        if now >= expiry {
            return Err(snippet_error(format!(
                "illustrative waiver expired: {}",
                command.id
            )));
        }
        return Ok(());
    }
    let directory = tempfile::tempdir()
        .map_err(|_error| snippet_error("cannot create isolated snippet directory"))?;
    let result = runner
        .run(
            &command.runner,
            &command.target,
            command
                .command
                .as_deref()
                .ok_or_else(|| snippet_error("command is absent"))?,
            contents,
            directory.path(),
        )
        .map_err(snippet_error)?;
    if result.diagnostic.len() > MAXIMUM_SNIPPET_BYTES {
        return Err(snippet_error("snippet diagnostic exceeds bounds"));
    }
    match command.mode {
        SnippetMode::Executable if result.success => Ok(()),
        SnippetMode::Executable => Err(snippet_error(format!("executable failed: {}", command.id))),
        SnippetMode::ExpectedFailure if result.success => Err(snippet_error(format!(
            "formerly invalid snippet now succeeds: {}",
            command.id
        ))),
        SnippetMode::ExpectedFailure => {
            let phase = command.expected_phase.as_deref().unwrap_or_default();
            let diagnostic = command.expected_diagnostic.as_deref().unwrap_or_default();
            if result.phase == phase && result.diagnostic.contains(diagnostic) {
                Ok(())
            } else {
                Err(snippet_error(format!(
                    "failure oracle differs: {}",
                    command.id
                )))
            }
        }
        SnippetMode::Illustrative => Ok(()),
    }
}

fn fences(markdown: &str) -> Result<Vec<Fence>, Report<SnippetError>> {
    let mut result = Vec::new();
    let mut current = None::<(String, String)>;
    for event in Parser::new(markdown) {
        match event {
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(info))) => {
                let language = info
                    .split_ascii_whitespace()
                    .next()
                    .filter(|value| !value.is_empty())
                    .unwrap_or("plain")
                    .to_ascii_lowercase();
                if current.replace((language, String::new())).is_some() {
                    return Err(snippet_error("nested fenced code block"));
                }
            }
            Event::Text(text) if current.is_some() => {
                current
                    .as_mut()
                    .expect("current fence should exist")
                    .1
                    .push_str(&text);
            }
            Event::End(TagEnd::CodeBlock) if current.is_some() => {
                let (language, contents) = current.take().expect("current fence should exist");
                result.push(Fence {
                    selector: format!("fence:{}", result.len() + 1),
                    language,
                    contents,
                });
            }
            _ => {}
        }
    }
    if current.is_some() {
        return Err(snippet_error("unterminated fenced code block"));
    }
    Ok(result)
}

fn validate_path(value: &str) -> Result<(), Report<SnippetError>> {
    NormalizedRelativePath::new(Path::new(value))
        .map(|_path| ())
        .map_err(|_error| snippet_error(format!("unsafe path: {value}")))
}

fn validate_selector(value: &str) -> Result<(), Report<SnippetError>> {
    let Some(number) = value.strip_prefix("fence:") else {
        return Err(snippet_error(format!("invalid selector: {value}")));
    };
    if number
        .parse::<usize>()
        .ok()
        .filter(|number| *number > 0)
        .is_none()
    {
        return Err(snippet_error(format!("invalid selector: {value}")));
    }
    Ok(())
}

fn validate_fingerprint(value: &str) -> Result<(), Report<SnippetError>> {
    let Some(hex) = value.strip_prefix("sha256:") else {
        return Err(snippet_error("fingerprint has no sha256 prefix"));
    };
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(snippet_error("fingerprint is not lowercase SHA-256"));
    }
    Ok(())
}

fn validate_string(value: &str, field: &'static str) -> Result<(), Report<SnippetError>> {
    if value.len() > MAXIMUM_STRING_BYTES || value.chars().any(|character| character == '\0') {
        return Err(snippet_error(format!("{field} exceeds string bounds")));
    }
    Ok(())
}

fn snippet_error(detail: impl Into<String>) -> Report<SnippetError> {
    Report::new(SnippetError {
        detail: detail.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_formatting_is_canonical_at_epoch_and_leap_day() {
        assert_eq!(format_utc(0).as_deref(), Some("1970-01-01T00:00:00Z"));
        assert_eq!(
            format_utc(951_782_400).as_deref(),
            Some("2000-02-29T00:00:00Z")
        );
        assert!(format_utc(u64::MAX).is_none());
    }

    #[test]
    fn production_runner_never_executes_snippet_contents_or_arbitrary_commands() {
        let directory = tempfile::tempdir().expect("should create runner fixture");
        let side_effect = directory.path().join("must-not-exist");
        let contents = format!("touch {}\n", side_effect.display());
        let result = ProcessSnippetRunner
            .run(
                "shell-syntax",
                VALIDATOR_TARGET,
                VALIDATOR_COMMAND,
                &contents,
                directory.path(),
            )
            .expect("syntax validation should finish");
        assert!(result.success);
        assert!(
            !side_effect.exists(),
            "syntax validation must not execute input"
        );
        assert!(
            ProcessSnippetRunner
                .run(
                    "shell",
                    VALIDATOR_TARGET,
                    "touch must-not-exist",
                    "echo ignored\n",
                    directory.path(),
                )
                .is_err(),
            "arbitrary shell runners and command text must be rejected"
        );
    }

    #[test]
    fn validator_process_scrubs_environment_and_enforces_stream_and_time_bounds() {
        let directory = tempfile::tempdir().expect("should create process fixture");
        let scrubbed = run_validator_process(
            Path::new("/usr/bin/env"),
            &[],
            directory.path(),
            1_024,
            Duration::from_secs(1),
        )
        .expect("bounded process should run");
        assert!(scrubbed.success);
        let environment = String::from_utf8(scrubbed.stdout).expect("environment should be UTF-8");
        let mut variables = environment.lines().collect::<Vec<_>>();
        variables.sort_unstable();
        assert_eq!(variables, ["LANG=C", "LC_ALL=C"]);

        assert!(
            run_validator_process(
                Path::new("/usr/bin/yes"),
                &[],
                directory.path(),
                64,
                Duration::from_secs(1),
            )
            .is_err(),
            "unbounded process output must fail"
        );
        assert!(
            run_validator_process(
                Path::new("/bin/sleep"),
                &["1".to_owned()],
                directory.path(),
                64,
                Duration::from_millis(10),
            )
            .is_err(),
            "long-running validators must be killed at the deadline"
        );
    }
}
