//! Sensitive-data detection over every classified tracked file.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use error_stack::{Report, ResultExt as _};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use sha2::{Digest as _, Sha256};
use toml_edit::{
    Document as TomlDocument, Item as TomlItem, Table as TomlTable, Value as TomlValue,
};

use crate::classification::{self, FileKind};
use crate::model::{Expiry, Owner, Rationale};
use crate::repository::{NormalizedRelativePath, Repository};

const ALLOWLIST_MANIFEST: &str = "tools/docs-parity/manifests/sensitive-allowlist.toml";
const RETIRED_MANIFEST: &str = "tools/docs-parity/manifests/retired-identifiers.toml";
const TRACKED_MANIFEST: &str = "tools/docs-parity/manifests/tracked-files.toml";
const MAINTAINED_MANIFEST: &str = "tools/docs-parity/manifests/maintained-sources.toml";
const MANIFEST_VERSION: u32 = 1;
const SERVICE_PATH: &str = "fastly.toml";
const SERVICE_OWNER: &str = "aram356";
const SERVICE_EXPIRY: &str = "2026-09-30T00:00:00Z";
const HISTORICAL_CNAME_FINGERPRINT: &str =
    "sha256:c5c88b1c0fd72489bc2352a680544204f898392cfe43655f761e8e21ae26bddf";
const HISTORICAL_EMAIL_FINGERPRINT: &str =
    "sha256:60a0c7d895777dbb3206205a932557134c63283c4f73dce19e5b1fc2e225bb2a";
const HISTORICAL_BINARY_PATH: &str = "docs/public/images/hero-graphic.jpeg";

/// Failure while scanning classified repository content.
#[derive(Debug, derive_more::Display)]
pub enum ScannerError {
    /// Required scanner governance cannot be read or parsed.
    #[display("cannot read scanner governance")]
    Governance,
    /// The classification universe is incomplete.
    #[display("cannot scan an unclassified repository")]
    Classification,
    /// An exception or retired-identifier record is invalid.
    #[display("invalid scanner governance")]
    InvalidGovernance,
    /// A finding has no exact, active reviewed disposition.
    #[display("sensitive-data scan found an undispositioned value")]
    Finding,
    /// Candidate records cannot be updated safely.
    #[display("cannot bootstrap scanner candidates")]
    Bootstrap,
}

impl core::error::Error for ScannerError {}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct AllowlistManifest {
    version: u32,
    reviewed: bool,
    #[serde(default)]
    exceptions: Vec<ExceptionRecord>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ExceptionRecord {
    class: ExceptionClass,
    path: String,
    detector: Detector,
    scope: String,
    selector: String,
    fingerprint: String,
    owner: String,
    rationale: String,
    expires_at: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
enum ExceptionClass {
    VendorUrl,
    HashPinnedFakeCredentialFixture,
    HistoricalExample,
    ServiceId,
    ProjectOwnedPublicDomain,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
enum Detector {
    Domain,
    Email,
    CredentialShape,
    ServiceId,
    EncodedToken,
    BinaryString,
    LockfileField,
    MediaMetadata,
    RetiredIdentifier,
}

impl Detector {
    const fn label(self) -> &'static str {
        match self {
            Self::Domain => "domain",
            Self::Email => "email",
            Self::CredentialShape => "credential_shape",
            Self::ServiceId => "service_id",
            Self::EncodedToken => "encoded_token",
            Self::BinaryString => "binary_string",
            Self::LockfileField => "lockfile_field",
            Self::MediaMetadata => "media_metadata",
            Self::RetiredIdentifier => "retired_identifier",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RetiredManifest {
    version: u32,
    reviewed: bool,
    #[serde(default)]
    identifiers: Vec<RetiredIdentifier>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RetiredIdentifier {
    kind: RetiredKind,
    fingerprint: String,
    normalized_length: usize,
    word_count: usize,
    case_insensitive: bool,
    whitespace_tolerant: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum RetiredKind {
    Identifier,
    AccessPhrase,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct Finding {
    path: String,
    detector: Detector,
    selector: String,
    matched: String,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct ExceptionKey {
    path: String,
    detector: Detector,
    selector: String,
    fingerprint: String,
}

struct RetiredToken {
    text: String,
    start: usize,
    end: usize,
}

struct ScanState {
    findings: Vec<Finding>,
    allowlist: AllowlistManifest,
}

struct SourceLexicalContext {
    literal_or_comment: Vec<bool>,
}

struct DomainContext {
    tracked_paths: BTreeSet<String>,
}

struct TomlLockScanner<'a> {
    path: &'a str,
    text: &'a str,
    findings: Vec<Finding>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum TomlStringKind {
    Basic,
    Literal,
    MultilineBasic,
    MultilineLiteral,
}

/// Scan every tracked file and require exact active dispositions for findings.
///
/// This covers the named mechanical detector classes. Human semantic
/// sensitivity outside those classes still requires reviewed disposition; the
/// scanner does not claim detector completeness for prose meaning.
///
/// # Errors
///
/// Returns an error for incomplete classification, invalid governance,
/// expired or stale exceptions, and unallowlisted findings.
pub(crate) fn check(repository: &Repository) -> Result<(), Report<ScannerError>> {
    let state = scan_repository(repository)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .change_context(ScannerError::InvalidGovernance)?;
    validate_findings(&state.findings, &state.allowlist, now.as_secs())
}

/// Replace the allowlist with deterministic, deliberately unreviewed candidates.
///
/// # Errors
///
/// Returns an error when classification, scanning, rendering, or atomic
/// replacement fails.
pub(crate) fn bootstrap(repository: &Repository) -> Result<(), Report<ScannerError>> {
    let state = scan_repository(repository)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .change_context(ScannerError::InvalidGovernance)?;
    let mut previous = BTreeMap::new();
    if state.allowlist.reviewed {
        for record in state.allowlist.exceptions {
            let key = validate_exception(&record, now.as_secs())?;
            if previous.insert(key, record).is_some() {
                return Err(Report::new(ScannerError::InvalidGovernance)
                    .attach("duplicate sensitive-data exception"));
            }
        }
    }

    let mut changed = !state.allowlist.reviewed;
    let mut exceptions = Vec::with_capacity(state.findings.len());
    for finding in state.findings {
        let key = finding_key(&finding);
        if let Some(record) = previous.remove(&key) {
            exceptions.push(record);
        } else {
            changed = true;
            exceptions.push(candidate_exception(finding));
        }
    }
    changed |= !previous.is_empty();
    if !changed {
        return Ok(());
    }
    write_manifest(
        repository,
        ALLOWLIST_MANIFEST,
        &AllowlistManifest {
            version: MANIFEST_VERSION,
            reviewed: false,
            exceptions,
        },
    )
}

fn scan_repository(repository: &Repository) -> Result<ScanState, Report<ScannerError>> {
    let classified =
        classification::checked_files(repository).change_context(ScannerError::Classification)?;
    let domain_context = DomainContext::build(&classified);
    for path in [TRACKED_MANIFEST, MAINTAINED_MANIFEST] {
        let text = read_manifest_text(repository, path)?;
        reject_toml_comments(path, &text)?;
    }
    let (allowlist, allowlist_text): (AllowlistManifest, String) =
        read_manifest_document(repository, ALLOWLIST_MANIFEST)?;
    reject_toml_comments(ALLOWLIST_MANIFEST, &allowlist_text)?;
    let (retired, retired_text): (RetiredManifest, String) =
        read_manifest_document(repository, RETIRED_MANIFEST)?;
    reject_toml_comments(RETIRED_MANIFEST, &retired_text)?;
    validate_retired_manifest(&retired)?;
    validate_governance_free_text(&allowlist, &retired.identifiers)?;
    let mut result = BTreeSet::new();
    for file in classified {
        let normalized = NormalizedRelativePath::new(Path::new(&file.path))
            .change_context(ScannerError::Classification)?;
        let contents = repository
            .read_tracked(&normalized)
            .change_context(ScannerError::Classification)?;
        match file.kind {
            FileKind::Text => {
                let text =
                    core::str::from_utf8(&contents).change_context(ScannerError::Classification)?;
                if is_lockfile(&file.path) {
                    let structured = scan_lockfile(&file.path, text)?;
                    result.extend(
                        scan_text(&file.path, text, 0, &domain_context)
                            .into_iter()
                            .filter(|finding| {
                                finding.detector != Detector::Domain
                                    || !structured.iter().any(|structural| {
                                        structural.detector == Detector::LockfileField
                                            && selector_contains(
                                                &structural.selector,
                                                &finding.selector,
                                            )
                                    })
                            }),
                    );
                    result.extend(structured);
                } else if !is_governance_manifest(&file.path) {
                    result.extend(scan_text(&file.path, text, 0, &domain_context));
                }
            }
            FileKind::Binary => {
                let media_findings = scan_media_metadata(&file.path, &contents, &domain_context)?;
                let media_matches = media_findings
                    .iter()
                    .map(|finding| (finding.selector.as_str(), finding.matched.as_str()))
                    .collect::<BTreeSet<_>>();
                result.extend(
                    scan_binary(&file.path, &contents, &domain_context)
                        .into_iter()
                        .filter(|finding| {
                            !media_matches
                                .contains(&(finding.selector.as_str(), finding.matched.as_str()))
                        }),
                );
                result.extend(media_findings);
            }
        }
        if !is_governance_manifest(&file.path) {
            result.extend(scan_retired(&file.path, &contents, &retired.identifiers)?);
        }
    }
    Ok(ScanState {
        findings: result.into_iter().collect(),
        allowlist,
    })
}

fn is_governance_manifest(path: &str) -> bool {
    matches!(
        path,
        ALLOWLIST_MANIFEST | RETIRED_MANIFEST | TRACKED_MANIFEST | MAINTAINED_MANIFEST
    )
}

fn scan_text(
    path: &str,
    text: &str,
    base_offset: usize,
    domain_context: &DomainContext,
) -> Vec<Finding> {
    let mut result = Vec::new();
    let lexical_context = SourceLexicalContext::for_path(path, text);
    let url_authority_spans = url_regex()
        .find_iter(text)
        .filter_map(|matched| {
            extracted_host_span(matched.as_str())
                .map(|(_host, start, end)| (matched.start() + start, matched.start() + end))
        })
        .collect::<Vec<_>>();
    for matched in url_regex().find_iter(text) {
        let value = trim_url(matched.as_str());
        let Some((host, host_start, host_end)) = extracted_host_span(value) else {
            continue;
        };
        if !valid_public_host(&host.to_ascii_lowercase()) && !fictional_or_local_url(value) {
            continue;
        }
        if !fictional_or_local_url(value) {
            result.push(finding(
                path,
                Detector::Domain,
                host,
                base_offset + matched.start() + host_start,
                base_offset + matched.start() + host_end,
            ));
        }
        let Some(component_start) = value.find(['?', '#']) else {
            continue;
        };
        let component = &value[component_start..];
        for nested in domain_host_regex().find_iter(component) {
            let raw_host = nested.as_str();
            let start = matched.start() + component_start + nested.start();
            let end = start + raw_host.len();
            if valid_public_host(&raw_host.to_ascii_lowercase())
                && !fictional_or_local_url(raw_host)
                && bare_domain_context_allowed(
                    path,
                    text,
                    start,
                    end,
                    domain_context,
                    lexical_context.as_ref(),
                )
                && bare_domain_has_terminator(text, end)
            {
                result.push(finding(
                    path,
                    Detector::Domain,
                    raw_host,
                    base_offset + start,
                    base_offset + end,
                ));
            }
        }
        for nested in email_regex().find_iter(component) {
            if !fictional_email(nested.as_str()) && !at_sign_filename(nested.as_str()) {
                let start = matched.start() + component_start + nested.start();
                result.push(finding(
                    path,
                    Detector::Email,
                    nested.as_str(),
                    base_offset + start,
                    base_offset + matched.start() + component_start + nested.end(),
                ));
            }
        }
    }
    for matched in bare_domain_regex().find_iter(text) {
        if matched.start() > 0 && text.as_bytes()[matched.start() - 1] == b'@' {
            continue;
        }
        if overlaps_url_authority(&url_authority_spans, matched.start(), matched.end()) {
            continue;
        }
        if !bare_domain_context_allowed(
            path,
            text,
            matched.start(),
            matched.end(),
            domain_context,
            lexical_context.as_ref(),
        ) {
            continue;
        }
        if !bare_domain_has_terminator(text, matched.end()) {
            continue;
        }
        let value = trim_url(matched.as_str());
        let raw_host = value.split('/').next().unwrap_or(value);
        let host = raw_host.to_ascii_lowercase();
        if !valid_public_host(&host) {
            continue;
        }
        if !fictional_or_local_url(value) {
            result.push(finding(
                path,
                Detector::Domain,
                raw_host,
                base_offset + matched.start(),
                base_offset + matched.start() + host.len(),
            ));
        }
    }
    for matched in email_regex().find_iter(text) {
        if !overlaps_url_authority(&url_authority_spans, matched.start(), matched.end())
            && !fictional_email(matched.as_str())
            && !at_sign_filename(matched.as_str())
        {
            result.push(finding(
                path,
                Detector::Email,
                matched.as_str(),
                base_offset + matched.start(),
                base_offset + matched.end(),
            ));
        }
    }
    for captures in service_regex().captures_iter(text) {
        if let Some(value) = captures.get(1) {
            result.push(finding(
                path,
                Detector::ServiceId,
                value.as_str(),
                base_offset + value.start(),
                base_offset + value.end(),
            ));
        }
    }
    for captures in credential_regex().captures_iter(text) {
        if let Some(value) = captures
            .get(1)
            .or_else(|| captures.get(2))
            .or_else(|| captures.get(3))
        {
            let is_quoted = captures.get(1).is_some() || captures.get(2).is_some();
            if lexical_context
                .as_ref()
                .is_some_and(|context| !context.allows(value.start()))
                || lexical_context.is_some()
                    && !is_quoted
                    && credential_source_expression(
                        text,
                        captures
                            .get(0)
                            .expect("credential match should exist")
                            .start(),
                        value.as_str(),
                    )
            {
                continue;
            }
            if !fictional_credential(value.as_str()) {
                result.push(finding(
                    path,
                    Detector::CredentialShape,
                    value.as_str(),
                    base_offset + value.start(),
                    base_offset + value.end(),
                ));
            }
        }
    }
    for captures in encoded_token_regex().captures_iter(text) {
        if let Some(value) = captures.get(1) {
            result.push(finding(
                path,
                Detector::EncodedToken,
                value.as_str(),
                base_offset + value.start(),
                base_offset + value.end(),
            ));
        }
    }
    result
}

fn at_sign_filename(value: &str) -> bool {
    value.rsplit_once('@').is_some_and(|(_name, suffix)| {
        let lower = suffix.to_ascii_lowercase();
        [".jpg", ".jpeg", ".png", ".gif", ".webp", ".svg"]
            .iter()
            .any(|extension| {
                lower.strip_suffix(extension).is_some_and(|scale| {
                    scale.strip_suffix('x').is_some_and(|digits| {
                        !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
                    })
                })
            })
    })
}

fn credential_source_expression(text: &str, match_start: usize, value: &str) -> bool {
    if value.contains('.') || value.contains("::") {
        return true;
    }
    let line_start = text[..match_start].rfind('\n').map_or(0, |index| index + 1);
    let prefix = &text[line_start..match_start];
    let trimmed = prefix.trim_end();
    trimmed.ends_with('.')
        || trimmed.ends_with(']')
        || trimmed.ends_with('"')
        || trimmed.ends_with('\'')
        || trimmed.ends_with('`')
        || prefix
            .split_whitespace()
            .any(|token| matches!(token, "let" | "const" | "var" | "static" | "final"))
}

fn valid_public_host(host: &str) -> bool {
    host.ends_with(".internal")
        || psl::suffix(host.as_bytes()).is_some_and(|suffix| suffix.typ().is_some())
            && psl::domain_str(host).is_some()
}

fn extracted_host_span(value: &str) -> Option<(&str, usize, usize)> {
    let scheme_end = value.find("://")? + 3;
    let authority_end = value[scheme_end..]
        .find(['/', '?', '#'])
        .map_or(value.len(), |n| scheme_end + n);
    let authority = &value[scheme_end..authority_end];
    let user_end = authority.rfind('@').map_or(0, |n| n + 1);
    let host_port = &authority[user_end..];
    let host_len = host_port.find(':').unwrap_or(host_port.len());
    let start = scheme_end + user_end;
    let end = start + host_len;
    (start < end).then(|| (&value[start..end], start, end))
}

fn bare_domain_context_allowed(
    path: &str,
    text: &str,
    start: usize,
    end: usize,
    domain_context: &DomainContext,
    lexical_context: Option<&SourceLexicalContext>,
) -> bool {
    if repository_path_token(path, text, start, end, &domain_context.tracked_paths) {
        return false;
    }
    lexical_context.is_none_or(|context| context.allows(start))
}

fn repository_path_token(
    current_path: &str,
    text: &str,
    start: usize,
    end: usize,
    tracked_paths: &BTreeSet<String>,
) -> bool {
    let bytes = text.as_bytes();
    let mut token_start = start;
    while token_start > 0 && path_token_byte(bytes[token_start - 1]) {
        token_start -= 1;
    }
    let mut token_end = end;
    while token_end < bytes.len() && path_token_byte(bytes[token_end]) {
        token_end += 1;
    }
    let token = text[token_start..token_end]
        .trim_end_matches('.')
        .trim_start_matches('/');
    if token.is_empty() || token.contains("//") {
        return false;
    }

    if tracked_paths.iter().any(|candidate| {
        candidate == token
            || candidate
                .strip_suffix(token)
                .is_some_and(|prefix| prefix.ends_with('/'))
    }) {
        return true;
    }
    if let Some((root, _remainder)) = token.split_once('/')
        && tracked_paths
            .iter()
            .any(|candidate| candidate.starts_with(&format!("{root}/")))
    {
        return true;
    }
    let root_candidate = normalize_repository_path("", token);
    if root_candidate
        .as_ref()
        .is_some_and(|candidate| tracked_paths.contains(candidate))
    {
        return true;
    }
    let parent = current_path
        .rsplit_once('/')
        .map_or("", |(parent, _name)| parent);
    normalize_repository_path(parent, token)
        .as_ref()
        .is_some_and(|candidate| tracked_paths.contains(candidate))
        || !token.contains('/')
            && tracked_paths.iter().any(|candidate| {
                Path::new(candidate)
                    .file_name()
                    .and_then(|name| name.to_str())
                    == Some(token)
            })
}

fn path_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'/')
}

fn normalize_repository_path(base: &str, token: &str) -> Option<String> {
    let mut components = base
        .split('/')
        .filter(|component| !component.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    for component in token.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                components.pop()?;
            }
            component => components.push(component.to_owned()),
        }
    }
    (!components.is_empty()).then(|| components.join("/"))
}

impl SourceLexicalContext {
    fn for_path(path: &str, text: &str) -> Option<Self> {
        match Path::new(path)
            .extension()
            .and_then(|extension| extension.to_str())
        {
            Some("rs") => Some(Self {
                literal_or_comment: rust_literal_and_comment_bytes(text),
            }),
            Some("js" | "jsx" | "ts" | "tsx" | "mjs" | "mts") => Some(Self {
                literal_or_comment: javascript_literal_and_comment_bytes(text),
            }),
            _ => None,
        }
    }

    fn allows(&self, offset: usize) -> bool {
        self.literal_or_comment
            .get(offset)
            .copied()
            .unwrap_or(false)
    }
}

impl DomainContext {
    fn empty() -> Self {
        Self {
            tracked_paths: BTreeSet::new(),
        }
    }

    fn build(classified: &[classification::ClassifiedFile]) -> Self {
        let tracked_paths = classified
            .iter()
            .map(|file| file.path.clone())
            .collect::<BTreeSet<_>>();
        Self { tracked_paths }
    }
}

fn mark_bytes(marked: &mut [bool], start: usize, end: usize) {
    for byte in &mut marked[start..end] {
        *byte = true;
    }
}

fn rust_literal_and_comment_bytes(text: &str) -> Vec<bool> {
    let bytes = text.as_bytes();
    let mut marked = vec![false; bytes.len()];
    let mut index = 0;
    while index < bytes.len() {
        if index + 1 < bytes.len() && &bytes[index..index + 2] == b"//" {
            let end = bytes[index..]
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(bytes.len(), |length| index + length);
            mark_bytes(&mut marked, index, end);
            index = end;
            continue;
        }
        if index + 1 < bytes.len() && &bytes[index..index + 2] == b"/*" {
            let mut depth = 1usize;
            let mut end = index + 2;
            while end < bytes.len() && depth > 0 {
                if end + 1 < bytes.len() && &bytes[end..end + 2] == b"/*" {
                    depth += 1;
                    end += 2;
                } else if end + 1 < bytes.len() && &bytes[end..end + 2] == b"*/" {
                    depth -= 1;
                    end += 2;
                } else {
                    end += 1;
                }
            }
            mark_bytes(&mut marked, index, end);
            index = end;
            continue;
        }
        if let Some((content_start, hashes)) = rust_raw_string_start(bytes, index) {
            let terminator = format!("\"{}", "#".repeat(hashes));
            let end = text[content_start..]
                .find(&terminator)
                .map_or(bytes.len(), |length| {
                    content_start + length + terminator.len()
                });
            mark_bytes(&mut marked, index, end);
            index = end;
            continue;
        }
        if bytes[index] == b'"' {
            let end = quoted_literal_end(bytes, index, b'"');
            mark_bytes(&mut marked, index, end);
            index = end;
            continue;
        }
        if bytes[index] == b'\''
            && let Some(end) = rust_character_end(bytes, index)
        {
            index = end;
            continue;
        }
        index += 1;
    }
    marked
}

fn rust_raw_string_start(bytes: &[u8], start: usize) -> Option<(usize, usize)> {
    let mut index = start;
    if bytes.get(index) == Some(&b'b') {
        index += 1;
    }
    if bytes.get(index) != Some(&b'r') {
        return None;
    }
    index += 1;
    let hash_start = index;
    while bytes.get(index) == Some(&b'#') {
        index += 1;
    }
    (bytes.get(index) == Some(&b'"')).then_some((index + 1, index - hash_start))
}

fn quoted_literal_end(bytes: &[u8], start: usize, delimiter: u8) -> usize {
    let mut index = start + 1;
    let mut escaped = false;
    while index < bytes.len() {
        if escaped {
            escaped = false;
        } else if bytes[index] == b'\\' {
            escaped = true;
        } else if bytes[index] == delimiter {
            return index + 1;
        }
        index += 1;
    }
    bytes.len()
}

fn rust_character_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut index = start + 1;
    let mut escaped = false;
    while index < bytes.len() && bytes[index] != b'\n' {
        if escaped {
            escaped = false;
        } else if bytes[index] == b'\\' {
            escaped = true;
        } else if bytes[index] == b'\'' {
            return Some(index + 1);
        }
        index += 1;
    }
    None
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum JavascriptLexicalMode {
    Code,
    SingleQuoted,
    DoubleQuoted,
    Template,
    LineComment,
    BlockComment,
    RegularExpression,
}

fn javascript_literal_and_comment_bytes(text: &str) -> Vec<bool> {
    let bytes = text.as_bytes();
    let mut marked = vec![false; bytes.len()];
    let mut mode = JavascriptLexicalMode::Code;
    let mut escaped = false;
    let mut regex_character_class = false;
    let mut interpolation_depths = Vec::new();
    let mut index = 0;
    let mut previous_code_byte = None;
    while index < bytes.len() {
        let byte = bytes[index];
        match mode {
            JavascriptLexicalMode::Code => {
                if index + 1 < bytes.len() && &bytes[index..index + 2] == b"//" {
                    mode = JavascriptLexicalMode::LineComment;
                    mark_bytes(&mut marked, index, index + 2);
                    index += 2;
                } else if index + 1 < bytes.len() && &bytes[index..index + 2] == b"/*" {
                    mode = JavascriptLexicalMode::BlockComment;
                    mark_bytes(&mut marked, index, index + 2);
                    index += 2;
                } else if matches!(byte, b'\'' | b'"') {
                    mode = if byte == b'\'' {
                        JavascriptLexicalMode::SingleQuoted
                    } else {
                        JavascriptLexicalMode::DoubleQuoted
                    };
                    marked[index] = true;
                    index += 1;
                } else if byte == b'`' {
                    mode = JavascriptLexicalMode::Template;
                    marked[index] = true;
                    index += 1;
                } else if byte == b'/'
                    && javascript_regex_can_start(text, index, previous_code_byte)
                {
                    mode = JavascriptLexicalMode::RegularExpression;
                    marked[index] = true;
                    index += 1;
                } else {
                    if let Some(depth) = interpolation_depths.last_mut() {
                        if byte == b'{' {
                            *depth += 1;
                        } else if byte == b'}' {
                            *depth -= 1;
                            if *depth == 0 {
                                interpolation_depths.pop();
                                mode = JavascriptLexicalMode::Template;
                                marked[index] = true;
                                index += 1;
                                continue;
                            }
                        }
                    }
                    if !byte.is_ascii_whitespace() {
                        previous_code_byte = Some(byte);
                    }
                    index += 1;
                }
            }
            JavascriptLexicalMode::SingleQuoted | JavascriptLexicalMode::DoubleQuoted => {
                marked[index] = true;
                if escaped {
                    escaped = false;
                } else if byte == b'\\' {
                    escaped = true;
                } else if (mode == JavascriptLexicalMode::SingleQuoted && byte == b'\'')
                    || (mode == JavascriptLexicalMode::DoubleQuoted && byte == b'"')
                {
                    mode = JavascriptLexicalMode::Code;
                }
                index += 1;
            }
            JavascriptLexicalMode::Template => {
                marked[index] = true;
                if escaped {
                    escaped = false;
                    index += 1;
                } else if byte == b'\\' {
                    escaped = true;
                    index += 1;
                } else if index + 1 < bytes.len() && &bytes[index..index + 2] == b"${" {
                    marked[index + 1] = true;
                    interpolation_depths.push(1);
                    mode = JavascriptLexicalMode::Code;
                    previous_code_byte = Some(b'{');
                    index += 2;
                } else if byte == b'`' {
                    mode = JavascriptLexicalMode::Code;
                    previous_code_byte = Some(b'`');
                    index += 1;
                } else {
                    index += 1;
                }
            }
            JavascriptLexicalMode::LineComment => {
                if byte == b'\n' {
                    mode = JavascriptLexicalMode::Code;
                } else {
                    marked[index] = true;
                }
                index += 1;
            }
            JavascriptLexicalMode::BlockComment => {
                marked[index] = true;
                if index + 1 < bytes.len() && &bytes[index..index + 2] == b"*/" {
                    marked[index + 1] = true;
                    mode = JavascriptLexicalMode::Code;
                    index += 2;
                } else {
                    index += 1;
                }
            }
            JavascriptLexicalMode::RegularExpression => {
                marked[index] = true;
                if escaped {
                    escaped = false;
                } else if byte == b'\\' {
                    escaped = true;
                } else if byte == b'[' {
                    regex_character_class = true;
                } else if byte == b']' {
                    regex_character_class = false;
                } else if byte == b'/' && !regex_character_class {
                    mode = JavascriptLexicalMode::Code;
                    previous_code_byte = Some(b'/');
                }
                index += 1;
            }
        }
    }
    marked
}

fn javascript_regex_can_start(text: &str, slash: usize, previous: Option<u8>) -> bool {
    previous.is_none_or(|byte| b"=(:,![{;?+-*%&|^~<>".contains(&byte))
        || javascript_preceding_keyword(text, slash).is_some_and(|keyword| {
            matches!(
                keyword,
                "return"
                    | "throw"
                    | "case"
                    | "delete"
                    | "void"
                    | "typeof"
                    | "yield"
                    | "await"
                    | "new"
                    | "in"
                    | "of"
                    | "instanceof"
            )
        })
}

fn javascript_preceding_keyword(text: &str, offset: usize) -> Option<&str> {
    let prefix = text.get(..offset)?.trim_end();
    let start = prefix
        .rfind(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
        .map_or(0, |index| index + 1);
    prefix.get(start..)
}

fn bare_domain_has_terminator(text: &str, end: usize) -> bool {
    let remainder = &text[end..];
    let Some(next) = remainder.chars().next() else {
        return true;
    };
    if next.is_whitespace()
        || matches!(
            next,
            ':' | '/' | '?' | '#' | ',' | ';' | ')' | ']' | '}' | '>' | '"' | '\'' | '`'
        )
    {
        return true;
    }
    next == '.'
        && remainder[1..]
            .chars()
            .next()
            .is_none_or(|after| after.is_whitespace() || ",;)]}>\"'`".contains(after))
}

fn overlaps_url_authority(spans: &[(usize, usize)], start: usize, end: usize) -> bool {
    spans
        .iter()
        .any(|(authority_start, authority_end)| start < *authority_end && end > *authority_start)
}

fn scan_binary(path: &str, contents: &[u8], domain_context: &DomainContext) -> Vec<Finding> {
    printable_strings(contents)
        .into_iter()
        .flat_map(|(offset, text)| scan_text(path, &text, offset, domain_context))
        .map(|finding| match finding.detector {
            Detector::ServiceId => finding,
            _ => Finding {
                detector: Detector::BinaryString,
                ..finding
            },
        })
        .collect()
}

fn scan_media_metadata(
    path: &str,
    contents: &[u8],
    domain_context: &DomainContext,
) -> Result<Vec<Finding>, Report<ScannerError>> {
    let metadata = if path.ends_with(".png") {
        png_metadata(contents)?
    } else if path.ends_with(".jpg") || path.ends_with(".jpeg") {
        jpeg_metadata(contents)
    } else {
        Vec::new()
    };
    Ok(metadata
        .into_iter()
        .flat_map(|(offset, text)| scan_text(path, &text, offset, domain_context))
        .map(|finding| match finding.detector {
            Detector::ServiceId => finding,
            _ => Finding {
                detector: Detector::MediaMetadata,
                ..finding
            },
        })
        .collect())
}

fn scan_lockfile(path: &str, text: &str) -> Result<Vec<Finding>, Report<ScannerError>> {
    if path.ends_with("Cargo.lock") {
        let document = TomlDocument::parse(text)
            .change_context(ScannerError::Finding)
            .attach(format!("cannot parse structured lockfile: {path}"))?;
        TomlLockScanner::new(path, text).scan(&document)
    } else if path.ends_with("package-lock.json") {
        let _value: JsonValue = serde_json::from_str(text)
            .change_context(ScannerError::Finding)
            .attach(format!("cannot parse structured lockfile: {path}"))?;
        scan_json_lock_fields(path, text)
    } else {
        Ok(Vec::new())
    }
}

impl<'a> TomlLockScanner<'a> {
    fn new(path: &'a str, text: &'a str) -> Self {
        Self {
            path,
            text,
            findings: Vec::new(),
        }
    }

    fn scan(mut self, document: &TomlDocument<&str>) -> Result<Vec<Finding>, Report<ScannerError>> {
        self.scan_table(document.as_table())?;
        Ok(self.findings)
    }

    fn scan_table(&mut self, table: &TomlTable) -> Result<(), Report<ScannerError>> {
        for (key, item) in table.iter() {
            self.scan_item(key, item)?;
        }
        Ok(())
    }

    fn scan_item(&mut self, key: &str, item: &TomlItem) -> Result<(), Report<ScannerError>> {
        if lock_field(Some(key)) {
            let value = item.as_value().ok_or_else(|| self.unsupported_shape(key))?;
            return self.scan_lock_value(key, value);
        }
        match item {
            TomlItem::None => Ok(()),
            TomlItem::Value(value) => self.scan_nested_value(value),
            TomlItem::Table(table) => self.scan_table(table),
            TomlItem::ArrayOfTables(tables) => {
                for table in tables.iter() {
                    self.scan_table(table)?;
                }
                Ok(())
            }
        }
    }

    fn scan_nested_value(&mut self, value: &TomlValue) -> Result<(), Report<ScannerError>> {
        match value {
            TomlValue::Array(values) => {
                for value in values.iter() {
                    self.scan_nested_value(value)?;
                }
                Ok(())
            }
            TomlValue::InlineTable(table) => {
                for (key, value) in table.iter() {
                    if lock_field(Some(key)) {
                        self.scan_lock_value(key, value)?;
                    } else {
                        self.scan_nested_value(value)?;
                    }
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn scan_lock_value(
        &mut self,
        key: &str,
        value: &TomlValue,
    ) -> Result<(), Report<ScannerError>> {
        let decoded = value.as_str().ok_or_else(|| self.unsupported_shape(key))?;
        if !sensitive_lock_value(decoded) {
            return Ok(());
        }
        let span = value.span().ok_or_else(|| {
            Report::new(ScannerError::Finding).attach(format!(
                "unmappable structured lockfile field: {}:{key}",
                self.path
            ))
        })?;
        let raw_span = toml_string_content_span(self.text, span).ok_or_else(|| {
            Report::new(ScannerError::Finding).attach(format!(
                "unmappable structured lockfile string: {}:{key}",
                self.path
            ))
        })?;
        let raw = self.text.get(raw_span.clone()).ok_or_else(|| {
            Report::new(ScannerError::Finding).attach(format!(
                "out-of-bounds structured lockfile string: {}:{key}",
                self.path
            ))
        })?;
        self.findings.push(finding(
            self.path,
            Detector::LockfileField,
            raw,
            raw_span.start,
            raw_span.end,
        ));
        Ok(())
    }

    fn unsupported_shape(&self, key: &str) -> Report<ScannerError> {
        Report::new(ScannerError::Finding).attach(format!(
            "unsupported structured lockfile field shape: {}:{key}",
            self.path
        ))
    }
}

fn toml_string_content_span(
    text: &str,
    span: std::ops::Range<usize>,
) -> Option<std::ops::Range<usize>> {
    let token = text.get(span.clone())?;
    let delimiter_length = if token.starts_with("\"\"\"") && token.ends_with("\"\"\"")
        || token.starts_with("'''") && token.ends_with("'''")
    {
        3
    } else if token.starts_with('"') && token.ends_with('"')
        || token.starts_with('\'') && token.ends_with('\'')
    {
        1
    } else {
        return None;
    };
    (token.len() >= delimiter_length * 2)
        .then_some(span.start + delimiter_length..span.end - delimiter_length)
}

fn scan_json_lock_fields(path: &str, text: &str) -> Result<Vec<Finding>, Report<ScannerError>> {
    scan_json_lock_fields_counted(path, text).map(|(findings, _steps)| findings)
}

fn scan_json_lock_fields_counted(
    path: &str,
    text: &str,
) -> Result<(Vec<Finding>, usize), Report<ScannerError>> {
    let bytes = text.as_bytes();
    let mut findings = Vec::new();
    let mut objects = Vec::<(usize, u8)>::new();
    let mut i = 0;
    let mut steps = 0;
    while i < bytes.len() {
        steps += 1;
        if bytes[i] == b'{' {
            objects.push((i, 0));
            i += 1;
            continue;
        }
        if bytes[i] == b'}' {
            objects.pop().ok_or_else(|| {
                Report::new(ScannerError::Finding)
                    .attach(format!("unbalanced JSON lockfile object: {path}"))
            })?;
            i += 1;
            continue;
        }
        if bytes[i] != b'"' {
            i += 1;
            continue;
        }
        let Some(end) = quoted_end_counted(bytes, i, &mut steps) else {
            return Err(Report::new(ScannerError::Finding)
                .attach(format!("unterminated JSON lockfile string: {path}")));
        };
        let raw = &text[i..=end];
        steps += raw.len();
        let value: String = serde_json::from_str(raw).change_context(ScannerError::Finding)?;
        let mut cursor = end + 1;
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            steps += 1;
            cursor += 1;
        }
        if cursor < bytes.len() && bytes[cursor] == b':' && lock_field(Some(&value)) {
            let (_object_start, seen) = objects.last_mut().ok_or_else(|| {
                Report::new(ScannerError::Finding).attach(format!(
                    "structured lockfile field outside object: {path}:{value}"
                ))
            })?;
            let bit = lock_field_bit(&value).expect("known lock field should have a bit");
            if *seen & bit != 0 {
                return Err(Report::new(ScannerError::Finding).attach(format!(
                    "duplicate structured lockfile field: {path}:{value}"
                )));
            }
            *seen |= bit;
            cursor += 1;
            while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
                steps += 1;
                cursor += 1;
            }
            if cursor >= bytes.len() || bytes[cursor] != b'"' {
                return Err(Report::new(ScannerError::Finding).attach(format!(
                    "unsupported structured lockfile field shape: {path}:{value}"
                )));
            }
            let value_end = quoted_end_counted(bytes, cursor, &mut steps).ok_or_else(|| {
                Report::new(ScannerError::Finding)
                    .attach(format!("unterminated JSON lockfile string: {path}"))
            })?;
            let raw = &text[cursor + 1..value_end];
            steps += value_end + 1 - cursor;
            let decoded: String = serde_json::from_str(&text[cursor..=value_end])
                .change_context(ScannerError::Finding)?;
            if sensitive_lock_value(&decoded) {
                findings.push(finding(
                    path,
                    Detector::LockfileField,
                    raw,
                    cursor + 1,
                    value_end,
                ));
            }
            i = value_end + 1;
        } else {
            i = end + 1;
        }
    }
    if !objects.is_empty() {
        return Err(Report::new(ScannerError::Finding)
            .attach(format!("unbalanced JSON lockfile object: {path}")));
    }
    Ok((findings, steps))
}

fn quoted_end_counted(bytes: &[u8], start: usize, steps: &mut usize) -> Option<usize> {
    let mut escaped = false;
    for (index, byte) in bytes.iter().enumerate().skip(start + 1) {
        *steps += 1;
        if escaped {
            escaped = false;
            continue;
        }
        if *byte == b'\\' {
            escaped = true;
            continue;
        }
        if *byte == b'"' {
            return Some(index);
        }
    }
    None
}

fn lock_field(key: Option<&str>) -> bool {
    matches!(key, Some("resolved" | "registry" | "source" | "url"))
}

fn lock_field_bit(key: &str) -> Option<u8> {
    match key {
        "resolved" => Some(1),
        "registry" => Some(2),
        "source" => Some(4),
        "url" => Some(8),
        _ => None,
    }
}

fn sensitive_lock_value(value: &str) -> bool {
    if value.starts_with("registry+https://github.com/rust-lang/crates.io-index")
        || value.starts_with("https://registry.npmjs.org/")
    {
        return false;
    }
    value.starts_with("http://")
        || value.starts_with("https://")
        || value.starts_with("registry+")
        || value.starts_with("git+")
}

fn scan_retired(
    path: &str,
    contents: &[u8],
    identifiers: &[RetiredIdentifier],
) -> Result<Vec<Finding>, Report<ScannerError>> {
    let tokens = retired_tokens(contents);
    let mut result = Vec::new();
    for record in identifiers {
        for window in tokens.windows(record.word_count) {
            let candidate = window
                .iter()
                .map(|token| token.text.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            let normalized = candidate.to_ascii_lowercase();
            if normalized.len() == record.normalized_length
                && fingerprint(normalized.as_bytes()) == record.fingerprint
            {
                result.push(finding(
                    path,
                    Detector::RetiredIdentifier,
                    &candidate,
                    window.first().expect("window should not be empty").start,
                    window.last().expect("window should not be empty").end,
                ));
            }
        }
    }
    Ok(result)
}

fn retired_tokens(contents: &[u8]) -> Vec<RetiredToken> {
    let mut tokens = Vec::new();
    let mut offset = 0;
    while offset < contents.len() {
        match core::str::from_utf8(&contents[offset..]) {
            Ok(text) => {
                tokens.extend(retired_tokens_in_text(text, offset));
                break;
            }
            Err(error) => {
                let valid_end = offset + error.valid_up_to();
                if valid_end > offset {
                    let text = core::str::from_utf8(&contents[offset..valid_end])
                        .expect("validated UTF-8 prefix should decode");
                    tokens.extend(retired_tokens_in_text(text, offset));
                }
                let Some(error_length) = error.error_len() else {
                    break;
                };
                offset = valid_end + error_length;
            }
        }
    }
    tokens
}

fn retired_tokens_in_text(text: &str, base_offset: usize) -> Vec<RetiredToken> {
    non_whitespace_regex()
        .find_iter(text)
        .filter_map(|matched| {
            let raw = matched.as_str();
            let token = raw.trim_matches(|character: char| {
                matches!(
                    character,
                    '.' | ','
                        | ';'
                        | ':'
                        | '!'
                        | '?'
                        | '('
                        | ')'
                        | '['
                        | ']'
                        | '{'
                        | '}'
                        | '<'
                        | '>'
                        | '"'
                        | '\''
                        | '`'
                        | '*'
                        | '~'
                        | '|'
                )
            });
            if token.is_empty() {
                return None;
            }
            let leading = raw
                .find(token)
                .expect("trimmed token should be a substring");
            Some(RetiredToken {
                text: token.to_owned(),
                start: base_offset + matched.start() + leading,
                end: base_offset + matched.start() + leading + token.len(),
            })
        })
        .collect()
}

fn validate_findings(
    findings: &[Finding],
    allowlist: &AllowlistManifest,
    now_seconds: u64,
) -> Result<(), Report<ScannerError>> {
    if allowlist.version != MANIFEST_VERSION {
        return Err(Report::new(ScannerError::InvalidGovernance)
            .attach("sensitive allowlist version must be 1"));
    }
    if !allowlist.reviewed {
        return Err(Report::new(ScannerError::InvalidGovernance)
            .attach("sensitive-data candidates require review"));
    }
    let mut exceptions = BTreeMap::new();
    for record in &allowlist.exceptions {
        let key = validate_exception(record, now_seconds)?;
        if exceptions.insert(key, (record, false)).is_some() {
            return Err(Report::new(ScannerError::InvalidGovernance).attach(format!(
                "duplicate sensitive-data exception: {}",
                record.path
            )));
        }
    }
    for finding in findings {
        let key = ExceptionKey {
            path: finding.path.clone(),
            detector: finding.detector,
            selector: finding.selector.clone(),
            fingerprint: fingerprint(finding.matched.as_bytes()),
        };
        if let Some((record, used)) = exceptions.get_mut(&key) {
            validate_class_semantics(record, finding)?;
            *used = true;
        } else {
            return Err(Report::new(ScannerError::Finding).attach(format!(
                "sensitive finding [{}] in {} (fingerprint {})",
                finding.detector.label(),
                finding.path,
                key.fingerprint
            )));
        }
    }
    if let Some((key, _record)) = exceptions.iter().find(|(_key, (_record, used))| !*used) {
        return Err(Report::new(ScannerError::InvalidGovernance).attach(format!(
            "stale sensitive-data exception: {} [{}] {}",
            key.path,
            key.detector.label(),
            key.fingerprint
        )));
    }
    Ok(())
}

fn validate_exception(
    record: &ExceptionRecord,
    now_seconds: u64,
) -> Result<ExceptionKey, Report<ScannerError>> {
    NormalizedRelativePath::new(Path::new(&record.path))
        .change_context(ScannerError::InvalidGovernance)
        .attach("exception path must be an exact normalized path")?;
    if record.scope != "exact-occurrence" {
        return Err(Report::new(ScannerError::InvalidGovernance)
            .attach("exception scope must be exact-occurrence"));
    }
    validate_selector(&record.selector)?;
    validate_fingerprint(&record.fingerprint)?;
    Owner::new(record.owner.clone()).change_context(ScannerError::InvalidGovernance)?;
    Rationale::new(record.rationale.clone()).change_context(ScannerError::InvalidGovernance)?;
    Expiry::parse(record.expires_at.clone()).change_context(ScannerError::InvalidGovernance)?;
    let expiry_seconds = timestamp_seconds(&record.expires_at).ok_or_else(|| {
        Report::new(ScannerError::InvalidGovernance)
            .attach(format!("invalid exception expiry: {}", record.expires_at))
    })?;
    if now_seconds >= expiry_seconds {
        return Err(Report::new(ScannerError::InvalidGovernance).attach(format!(
            "expired sensitive-data exception: {} at {}",
            record.path, record.expires_at
        )));
    }
    validate_class_pair(record)?;
    if record.class == ExceptionClass::ServiceId {
        if record.path != SERVICE_PATH {
            return Err(Report::new(ScannerError::InvalidGovernance)
                .attach("service-ID exception path must be exactly fastly.toml"));
        }
        if record.owner != SERVICE_OWNER {
            return Err(Report::new(ScannerError::InvalidGovernance)
                .attach("service-ID exception owner must be aram356"));
        }
        if record.expires_at != SERVICE_EXPIRY {
            return Err(Report::new(ScannerError::InvalidGovernance)
                .attach("service-ID exception expiry must be 2026-09-30T00:00:00Z"));
        }
    }
    Ok(ExceptionKey {
        path: record.path.clone(),
        detector: record.detector,
        selector: record.selector.clone(),
        fingerprint: record.fingerprint.clone(),
    })
}

fn validate_class_pair(record: &ExceptionRecord) -> Result<(), Report<ScannerError>> {
    let valid = match record.class {
        ExceptionClass::VendorUrl => {
            matches!(
                record.detector,
                Detector::Domain
                    | Detector::LockfileField
                    | Detector::BinaryString
                    | Detector::MediaMetadata
            )
        }
        ExceptionClass::HashPinnedFakeCredentialFixture => matches!(
            record.detector,
            Detector::CredentialShape
                | Detector::EncodedToken
                | Detector::BinaryString
                | Detector::MediaMetadata
        ),
        ExceptionClass::HistoricalExample => matches!(
            record.detector,
            Detector::Domain
                | Detector::BinaryString
                | Detector::Email
                | Detector::MediaMetadata
                | Detector::RetiredIdentifier
        ),
        ExceptionClass::ProjectOwnedPublicDomain => record.detector == Detector::Domain,
        ExceptionClass::ServiceId => record.detector == Detector::ServiceId,
    };
    if valid {
        Ok(())
    } else {
        Err(Report::new(ScannerError::InvalidGovernance).attach(format!(
            "exception class is incompatible with detector: {:?}/{}",
            record.class,
            record.detector.label()
        )))
    }
}

fn validate_class_semantics(
    record: &ExceptionRecord,
    finding: &Finding,
) -> Result<(), Report<ScannerError>> {
    let valid = match record.class {
        ExceptionClass::VendorUrl => {
            finding.detector == Detector::LockfileField
                || matches!(
                    finding.detector,
                    Detector::Domain | Detector::BinaryString | Detector::MediaMetadata
                ) && valid_public_host(&finding.matched.to_ascii_lowercase())
                    && !project_owned_host(&finding.matched)
                    && fingerprint(finding.matched.to_ascii_lowercase().as_bytes())
                        != HISTORICAL_CNAME_FINGERPRINT
        }
        ExceptionClass::HashPinnedFakeCredentialFixture => {
            fake_credential_evidence(&finding.path, &finding.matched)
        }
        ExceptionClass::HistoricalExample => historical_example_evidence(record, finding),
        ExceptionClass::ProjectOwnedPublicDomain => project_owned_host(&finding.matched),
        ExceptionClass::ServiceId => true,
    };
    if valid {
        Ok(())
    } else {
        Err(Report::new(ScannerError::InvalidGovernance).attach(format!(
            "exception class lacks provable finding semantics: {:?}/{} in {}",
            record.class,
            finding.detector.label(),
            finding.path
        )))
    }
}

fn fake_credential_evidence(path: &str, value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    let tokens = lower
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>();
    let production_looking = tokens
        .iter()
        .any(|token| matches!(*token, "production" | "prod" | "live"));
    let exact_size_marker = tokens
        .windows(3)
        .any(|triplet| triplet == ["password", "32", "bytes"]);
    let exact_marker = tokens.iter().any(|token| {
        matches!(
            *token,
            "fake" | "test" | "example" | "fixture" | "integration" | "unit" | "placeholder"
        )
    }) || tokens.windows(2).any(|pair| pair == ["change", "me"])
        || exact_size_marker;
    let exact_placeholder = matches!(
        lower.as_str(),
        "admin-password"
            | "admin_password"
            | "handler_password"
            | "secure_handler_password"
            | "api_handler_password"
            | "secret_value"
            | "<high-entropy-32-plus-character-value>"
            | "<your-fastly-api-token>"
            | "{}/{credential_scope}"
            | "store.get(\"ts-2025-10-a\")?"
    );
    let published_example = lower == "akiaiosfodnn7example/20130524/us-east-1/s3/aws4_request";
    (!production_looking || exact_size_marker)
        && (categorized_fixture_path(path)
            || path.ends_with(".example.toml")
            || exact_marker
            || exact_placeholder
            || published_example)
}

fn categorized_fixture_path(path: &str) -> bool {
    path.starts_with("tests/")
        || path.contains("/tests/")
        || path.starts_with("fixtures/")
        || path.contains("/fixtures/")
}

fn historical_example_evidence(record: &ExceptionRecord, finding: &Finding) -> bool {
    match finding.detector {
        Detector::Domain => {
            record.fingerprint == HISTORICAL_CNAME_FINGERPRINT
                && matches!(
                    (record.path.as_str(), record.selector.as_str()),
                    (
                        "docs/internal/audits/documentation-refresh-decisions.md",
                        "bytes:5027-5049"
                    ) | (
                        "docs/superpowers/specs/2026-08-19-documentation-refresh-design.md",
                        "bytes:3892-3914"
                    )
                )
        }
        Detector::BinaryString => record.path == HISTORICAL_BINARY_PATH,
        Detector::Email => {
            categorized_fixture_path(&record.path)
                && (record.path != "tools/docs-parity/tests/scanner.rs"
                    || record.fingerprint == HISTORICAL_EMAIL_FINGERPRINT)
        }
        Detector::MediaMetadata => {
            record.path == HISTORICAL_BINARY_PATH || categorized_fixture_path(&record.path)
        }
        Detector::RetiredIdentifier => true,
        Detector::CredentialShape
        | Detector::ServiceId
        | Detector::EncodedToken
        | Detector::LockfileField => false,
    }
}

fn validate_retired_manifest(manifest: &RetiredManifest) -> Result<(), Report<ScannerError>> {
    if manifest.version != MANIFEST_VERSION {
        return Err(Report::new(ScannerError::InvalidGovernance)
            .attach("retired-identifiers manifest version must be 1"));
    }
    if !manifest.reviewed {
        return Err(Report::new(ScannerError::InvalidGovernance)
            .attach("retired-identifier candidates require review"));
    }
    let mut values = BTreeSet::new();
    for record in &manifest.identifiers {
        validate_fingerprint(&record.fingerprint)?;
        if record.normalized_length == 0 || record.word_count == 0 {
            return Err(Report::new(ScannerError::InvalidGovernance)
                .attach("retired identifier shape must not be empty"));
        }
        if !record.case_insensitive || record.whitespace_tolerant != (record.word_count > 1) {
            return Err(Report::new(ScannerError::InvalidGovernance)
                .attach("retired identifier normalization metadata is inconsistent"));
        }
        match record.kind {
            RetiredKind::Identifier if record.word_count != 1 || record.whitespace_tolerant => {
                return Err(Report::new(ScannerError::InvalidGovernance)
                    .attach("identifier denylist record must contain exactly one token"));
            }
            RetiredKind::AccessPhrase if record.word_count <= 1 || !record.whitespace_tolerant => {
                return Err(Report::new(ScannerError::InvalidGovernance)
                    .attach("access-phrase denylist record must contain multiple words"));
            }
            RetiredKind::Identifier | RetiredKind::AccessPhrase => {}
        }
        if !values.insert(record.fingerprint.clone()) {
            return Err(Report::new(ScannerError::InvalidGovernance).attach(format!(
                "duplicate retired identifier fingerprint: {}",
                record.fingerprint
            )));
        }
    }
    Ok(())
}

fn validate_governance_free_text(
    allowlist: &AllowlistManifest,
    retired: &[RetiredIdentifier],
) -> Result<(), Report<ScannerError>> {
    for record in &allowlist.exceptions {
        for (field, value) in [("owner", &record.owner), ("rationale", &record.rationale)] {
            if !scan_text(ALLOWLIST_MANIFEST, value, 0, &DomainContext::empty()).is_empty()
                || !scan_retired(ALLOWLIST_MANIFEST, value.as_bytes(), retired)?.is_empty()
            {
                return Err(Report::new(ScannerError::InvalidGovernance).attach(format!(
                    "sensitive value in governance free-text: {ALLOWLIST_MANIFEST}:{field}"
                )));
            }
        }
    }
    Ok(())
}

fn read_manifest_document<T>(
    repository: &Repository,
    path: &str,
) -> Result<(T, String), Report<ScannerError>>
where
    T: for<'de> Deserialize<'de>,
{
    let text = read_manifest_text(repository, path)?;
    let manifest = toml::from_str(&text).change_context(ScannerError::Governance)?;
    Ok((manifest, text))
}

fn read_manifest_text(repository: &Repository, path: &str) -> Result<String, Report<ScannerError>> {
    let normalized =
        NormalizedRelativePath::new(Path::new(path)).change_context(ScannerError::Governance)?;
    let contents = repository
        .read_optional(&normalized)
        .change_context(ScannerError::Governance)?
        .ok_or_else(|| {
            Report::new(ScannerError::Governance).attach(format!("missing manifest: {path}"))
        })?;
    let text = core::str::from_utf8(&contents).change_context(ScannerError::Governance)?;
    Ok(text.to_owned())
}

fn reject_toml_comments(path: &str, text: &str) -> Result<(), Report<ScannerError>> {
    let bytes = text.as_bytes();
    let mut string_kind = None;
    let mut index = 0;
    while index < bytes.len() {
        match string_kind {
            None if bytes[index] == b'#' => {
                return Err(Report::new(ScannerError::InvalidGovernance)
                    .attach(format!("governance manifest must be comment-free: {path}")));
            }
            None if bytes[index..].starts_with(b"\"\"\"") => {
                string_kind = Some(TomlStringKind::MultilineBasic);
                index += 3;
            }
            None if bytes[index..].starts_with(b"'''") => {
                string_kind = Some(TomlStringKind::MultilineLiteral);
                index += 3;
            }
            None if bytes[index] == b'\"' => {
                string_kind = Some(TomlStringKind::Basic);
                index += 1;
            }
            None if bytes[index] == b'\'' => {
                string_kind = Some(TomlStringKind::Literal);
                index += 1;
            }
            Some(TomlStringKind::Basic) if bytes[index] == b'\\' => {
                index = (index + 2).min(bytes.len());
            }
            Some(TomlStringKind::Basic) if bytes[index] == b'\"' => {
                string_kind = None;
                index += 1;
            }
            Some(TomlStringKind::Literal) if bytes[index] == b'\'' => {
                string_kind = None;
                index += 1;
            }
            Some(TomlStringKind::MultilineBasic) if bytes[index] == b'\\' => {
                index = (index + 2).min(bytes.len());
            }
            Some(TomlStringKind::MultilineBasic) if bytes[index..].starts_with(b"\"\"\"") => {
                string_kind = None;
                index += 3;
            }
            Some(TomlStringKind::MultilineLiteral) if bytes[index..].starts_with(b"'''") => {
                string_kind = None;
                index += 3;
            }
            None
            | Some(TomlStringKind::Basic)
            | Some(TomlStringKind::Literal)
            | Some(TomlStringKind::MultilineBasic)
            | Some(TomlStringKind::MultilineLiteral) => index += 1,
        }
    }
    Ok(())
}

fn write_manifest<T>(
    repository: &Repository,
    path: &str,
    manifest: &T,
) -> Result<(), Report<ScannerError>>
where
    T: Serialize,
{
    let mut contents = toml::to_string_pretty(manifest)
        .change_context(ScannerError::Bootstrap)?
        .into_bytes();
    if !contents.ends_with(b"\n") {
        contents.push(b'\n');
    }
    let normalized =
        NormalizedRelativePath::new(Path::new(path)).change_context(ScannerError::Bootstrap)?;
    repository
        .write_atomically(&normalized, &contents)
        .change_context(ScannerError::Bootstrap)
}

fn candidate_class(finding: &Finding) -> ExceptionClass {
    if fingerprint(finding.matched.to_ascii_lowercase().as_bytes()) == HISTORICAL_CNAME_FINGERPRINT
    {
        return ExceptionClass::HistoricalExample;
    }
    if finding.detector == Detector::BinaryString && finding.path == HISTORICAL_BINARY_PATH {
        return ExceptionClass::HistoricalExample;
    }
    if finding.detector == Detector::Domain && project_owned_host(&finding.matched) {
        return ExceptionClass::ProjectOwnedPublicDomain;
    }
    match finding.detector {
        Detector::Domain | Detector::LockfileField => ExceptionClass::VendorUrl,
        Detector::BinaryString | Detector::MediaMetadata
            if valid_public_host(&finding.matched.to_ascii_lowercase()) =>
        {
            ExceptionClass::VendorUrl
        }
        Detector::CredentialShape | Detector::EncodedToken => {
            ExceptionClass::HashPinnedFakeCredentialFixture
        }
        Detector::ServiceId => ExceptionClass::ServiceId,
        Detector::Email
        | Detector::BinaryString
        | Detector::MediaMetadata
        | Detector::RetiredIdentifier => ExceptionClass::HistoricalExample,
    }
}

fn candidate_exception(finding: Finding) -> ExceptionRecord {
    let class = candidate_class(&finding);
    ExceptionRecord {
        class,
        path: finding.path,
        detector: finding.detector,
        scope: "exact-occurrence".to_owned(),
        selector: finding.selector,
        fingerprint: fingerprint(finding.matched.as_bytes()),
        owner: SERVICE_OWNER.to_owned(),
        rationale: candidate_rationale(class, finding.detector).to_owned(),
        expires_at: candidate_expiry(class, finding.detector).to_owned(),
    }
}

fn finding_key(finding: &Finding) -> ExceptionKey {
    ExceptionKey {
        path: finding.path.clone(),
        detector: finding.detector,
        selector: finding.selector.clone(),
        fingerprint: fingerprint(finding.matched.as_bytes()),
    }
}

fn selector_contains(container: &str, nested: &str) -> bool {
    let Some((container_start, container_end)) = parse_selector_span(container) else {
        return false;
    };
    let Some((nested_start, nested_end)) = parse_selector_span(nested) else {
        return false;
    };
    container_start <= nested_start && nested_end <= container_end
}

fn parse_selector_span(selector: &str) -> Option<(usize, usize)> {
    let (start, end) = selector.strip_prefix("bytes:")?.split_once('-')?;
    Some((start.parse().ok()?, end.parse().ok()?))
}

fn project_owned_host(value: &str) -> bool {
    let remainder = value
        .split_once("://")
        .map_or(value, |(_scheme, remainder)| remainder);
    let authority = remainder
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .rsplit('@')
        .next()
        .unwrap_or_default();
    let host = authority
        .split(':')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    ["iabtechlab.com", "iabtechlab.github.io"]
        .iter()
        .any(|owned| host == *owned || host.ends_with(&format!(".{owned}")))
}

fn candidate_rationale(class: ExceptionClass, detector: Detector) -> &'static str {
    match class {
        ExceptionClass::HistoricalExample if detector == Detector::Domain => {
            "Preserve the approved audit's exact record of the deleted placeholder CNAME."
        }
        ExceptionClass::HistoricalExample => "Reviewed exact historical repository record.",
        ExceptionClass::ProjectOwnedPublicDomain => {
            "Reviewed exact project-owned public domain reference."
        }
        ExceptionClass::ServiceId => {
            "Preserve the existing Fastly service binding during this refresh; removal is independent."
        }
        ExceptionClass::HashPinnedFakeCredentialFixture => {
            "Reviewed exact hash-pinned synthetic credential fixture."
        }
        ExceptionClass::VendorUrl => match detector {
            Detector::Domain | Detector::BinaryString | Detector::LockfileField => {
                "Reviewed exact public vendor reference required by repository content."
            }
            Detector::Email
            | Detector::CredentialShape
            | Detector::ServiceId
            | Detector::EncodedToken
            | Detector::MediaMetadata
            | Detector::RetiredIdentifier => "Reviewed exact governed repository value.",
        },
    }
}

const fn candidate_expiry(class: ExceptionClass, detector: Detector) -> &'static str {
    if matches!(class, ExceptionClass::HistoricalExample) {
        "2027-08-31T00:00:00Z"
    } else if matches!(detector, Detector::ServiceId) {
        SERVICE_EXPIRY
    } else {
        "2027-09-01T00:00:00Z"
    }
}

fn finding(path: &str, detector: Detector, matched: &str, start: usize, end: usize) -> Finding {
    Finding {
        path: path.to_owned(),
        detector,
        selector: format!("bytes:{start}-{end}"),
        matched: matched.to_owned(),
    }
}

fn is_lockfile(path: &str) -> bool {
    path.ends_with("Cargo.lock") || path.ends_with("package-lock.json")
}

fn trim_url(value: &str) -> &str {
    value.trim_end_matches(|character: char| {
        matches!(character, '.' | ',' | ';' | ':' | ')' | ']' | '}')
    })
}

fn fictional_or_local_url(value: &str) -> bool {
    let authority = value
        .split_once("://")
        .map_or(value, |(_scheme, remainder)| remainder)
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .rsplit('@')
        .next()
        .unwrap_or_default();
    let host = if authority.starts_with('[') {
        authority
            .find(']')
            .map_or(authority, |end| &authority[..=end])
    } else {
        authority.split(':').next().unwrap_or_default()
    }
    .to_ascii_lowercase();
    host == "localhost"
        || host.ends_with(".localhost")
        || host == "example"
        || host.ends_with(".example")
        || host == "invalid"
        || host.ends_with(".invalid")
        || host == "test"
        || host.ends_with(".test")
        || ["example.com", "example.net", "example.org"]
            .iter()
            .any(|reserved| host == *reserved || host.ends_with(&format!(".{reserved}")))
        || host == "127.0.0.1"
        || host == "[::1]"
}

fn fictional_email(value: &str) -> bool {
    value
        .rsplit_once('@')
        .is_some_and(|(_local, domain)| fictional_or_local_url(domain))
}

fn fictional_credential(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.starts_with("example") || lower.starts_with("your-") || lower.contains("placeholder")
}

fn printable_strings(contents: &[u8]) -> Vec<(usize, String)> {
    let mut strings = Vec::new();
    let mut current = Vec::new();
    let mut start = 0;
    for (index, byte) in contents.iter().copied().chain([0]).enumerate() {
        if byte.is_ascii_graphic() || byte == b' ' {
            if current.is_empty() {
                start = index;
            }
            current.push(byte);
        } else {
            if current.len() >= 6 {
                strings.push((start, String::from_utf8_lossy(&current).into_owned()));
            }
            current.clear();
        }
    }
    strings
}

fn png_metadata(contents: &[u8]) -> Result<Vec<(usize, String)>, Report<ScannerError>> {
    if !contents.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err(Report::new(ScannerError::Finding).attach("invalid PNG signature"));
    }
    let mut offset = 8;
    let mut metadata = Vec::new();
    let mut first = true;
    let mut saw_end = false;
    while offset < contents.len() {
        if offset + 12 > contents.len() {
            return Err(Report::new(ScannerError::Finding).attach("truncated PNG chunk"));
        }
        let length = u32::from_be_bytes([
            contents[offset],
            contents[offset + 1],
            contents[offset + 2],
            contents[offset + 3],
        ]) as usize;
        let Some(end) = offset
            .checked_add(12)
            .and_then(|value| value.checked_add(length))
        else {
            return Err(Report::new(ScannerError::Finding).attach("PNG chunk length overflow"));
        };
        if end > contents.len() {
            return Err(Report::new(ScannerError::Finding).attach("truncated PNG chunk data"));
        }
        let chunk_type = &contents[offset + 4..offset + 8];
        let data = &contents[offset + 8..offset + 8 + length];
        let expected_crc = u32::from_be_bytes(
            contents[offset + 8 + length..end]
                .try_into()
                .expect("PNG CRC has four bytes"),
        );
        if png_crc32(&contents[offset + 4..offset + 8 + length]) != expected_crc {
            return Err(Report::new(ScannerError::Finding).attach("invalid PNG chunk CRC"));
        }
        if first && chunk_type != b"IHDR" {
            return Err(Report::new(ScannerError::Finding).attach("PNG must begin with IHDR"));
        }
        first = false;
        if saw_end {
            return Err(Report::new(ScannerError::Finding).attach("PNG data follows IEND"));
        }
        if chunk_type == b"tEXt" {
            let position = png_keyword_end(data)?;
            let text = core::str::from_utf8(&data[position + 1..])
                .change_context(ScannerError::Finding)
                .attach("PNG tEXt metadata is not byte-mappable UTF-8")?;
            metadata.push((offset + 8 + position + 1, text.to_owned()));
        } else if chunk_type == b"zTXt" {
            let position = png_keyword_end(data)?;
            if data.get(position + 1) != Some(&0) || data.len() <= position + 2 {
                return Err(Report::new(ScannerError::Finding)
                    .attach("invalid PNG zTXt compression fields"));
            }
            return Err(Report::new(ScannerError::Finding)
                .attach("compressed PNG zTXt metadata requires review"));
        } else if chunk_type == b"iTXt" {
            let keyword_end = png_keyword_end(data)?;
            let flag = *data.get(keyword_end + 1).ok_or_else(|| {
                Report::new(ScannerError::Finding).attach("missing PNG iTXt compression flag")
            })?;
            let method = *data.get(keyword_end + 2).ok_or_else(|| {
                Report::new(ScannerError::Finding).attach("missing PNG iTXt compression method")
            })?;
            if flag > 1 || method != 0 {
                return Err(Report::new(ScannerError::Finding)
                    .attach("invalid PNG iTXt compression fields"));
            }
            let language_start = keyword_end + 3;
            let language_end = data[language_start..]
                .iter()
                .position(|byte| *byte == 0)
                .map(|position| language_start + position)
                .ok_or_else(|| {
                    Report::new(ScannerError::Finding).attach("missing PNG iTXt language separator")
                })?;
            let translated_start = language_end + 1;
            let translated_end = data[translated_start..]
                .iter()
                .position(|byte| *byte == 0)
                .map(|position| translated_start + position)
                .ok_or_else(|| {
                    Report::new(ScannerError::Finding)
                        .attach("missing PNG iTXt translated-keyword separator")
                })?;
            if flag == 1 {
                return Err(Report::new(ScannerError::Finding)
                    .attach("compressed PNG iTXt metadata requires review"));
            }
            let text_start = translated_end + 1;
            let text = core::str::from_utf8(&data[text_start..])
                .change_context(ScannerError::Finding)
                .attach("PNG iTXt text is not valid UTF-8")?;
            metadata.push((offset + 8 + text_start, text.to_owned()));
        }
        if chunk_type == b"IEND" {
            if length != 0 {
                return Err(Report::new(ScannerError::Finding).attach("invalid PNG IEND"));
            }
            saw_end = true;
        }
        offset = end;
    }
    if !saw_end {
        return Err(Report::new(ScannerError::Finding).attach("PNG has no IEND"));
    }
    Ok(metadata)
}

fn png_keyword_end(data: &[u8]) -> Result<usize, Report<ScannerError>> {
    let end = data.iter().position(|byte| *byte == 0).ok_or_else(|| {
        Report::new(ScannerError::Finding).attach("PNG text keyword has no separator")
    })?;
    if !(1..=79).contains(&end) {
        return Err(Report::new(ScannerError::Finding)
            .attach("PNG text keyword length must be 1 through 79 bytes"));
    }
    Ok(end)
}

fn png_crc32(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & 0_u32.wrapping_sub(crc & 1));
        }
    }
    !crc
}

fn jpeg_metadata(contents: &[u8]) -> Vec<(usize, String)> {
    if !contents.starts_with(&[0xff, 0xd8]) {
        return Vec::new();
    }
    let mut offset = 2;
    let mut metadata = Vec::new();
    while offset + 4 <= contents.len() && contents[offset] == 0xff {
        let marker = contents[offset + 1];
        if marker == 0xd9 || marker == 0xda {
            break;
        }
        let length = usize::from(u16::from_be_bytes([
            contents[offset + 2],
            contents[offset + 3],
        ]));
        if length < 2 || offset + 2 + length > contents.len() {
            break;
        }
        let data = &contents[offset + 4..offset + 2 + length];
        if marker == 0xe1 || marker == 0xfe {
            metadata.extend(
                printable_strings(data)
                    .into_iter()
                    .map(|(position, text)| (offset + 4 + position, text)),
            );
        }
        offset += 2 + length;
    }
    metadata
}

fn fingerprint(contents: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(contents))
}

fn validate_fingerprint(value: &str) -> Result<(), Report<ScannerError>> {
    let valid = value.len() == 71
        && value.starts_with("sha256:")
        && value[7..].bytes().all(|byte| byte.is_ascii_hexdigit());
    if valid {
        Ok(())
    } else {
        Err(Report::new(ScannerError::InvalidGovernance)
            .attach(format!("invalid SHA-256 fingerprint: {value}")))
    }
}

fn validate_selector(value: &str) -> Result<(), Report<ScannerError>> {
    let Some((start, end)) = value
        .strip_prefix("bytes:")
        .and_then(|range| range.split_once('-'))
        .and_then(|(start, end)| Some((start.parse::<usize>().ok()?, end.parse::<usize>().ok()?)))
    else {
        return Err(Report::new(ScannerError::InvalidGovernance)
            .attach(format!("invalid exact occurrence selector: {value}")));
    };
    if start >= end {
        return Err(Report::new(ScannerError::InvalidGovernance)
            .attach(format!("invalid exact occurrence selector: {value}")));
    }
    Ok(())
}

#[cfg(test)]
fn exception_is_active(expiry: &str, now: &str) -> bool {
    match (timestamp_seconds(expiry), timestamp_seconds(now)) {
        (Some(expiry), Some(now)) => now < expiry,
        _ => false,
    }
}

fn timestamp_seconds(value: &str) -> Option<u64> {
    Expiry::parse(value.to_owned()).ok()?;
    let bytes = value.as_bytes();
    let year = i64::try_from(decimal(bytes, 0, 4)?).ok()?;
    let month = i64::try_from(decimal(bytes, 5, 2)?).ok()?;
    let day = i64::try_from(decimal(bytes, 8, 2)?).ok()?;
    let hour = decimal(bytes, 11, 2)?;
    let minute = decimal(bytes, 14, 2)?;
    let second = decimal(bytes, 17, 2)?;
    let days = days_from_civil(year, month, day);
    if days < 0 {
        return None;
    }
    u64::try_from(days)
        .ok()?
        .checked_mul(86_400)?
        .checked_add(hour.checked_mul(3_600)?)?
        .checked_add(minute.checked_mul(60)?)?
        .checked_add(second)
}

fn decimal(bytes: &[u8], start: usize, length: usize) -> Option<u64> {
    bytes
        .get(start..start + length)?
        .iter()
        .try_fold(0_u64, |value, byte| {
            byte.is_ascii_digit()
                .then(|| value * 10 + u64::from(*byte - b'0'))
        })
}

fn days_from_civil(mut year: i64, month: i64, day: i64) -> i64 {
    if month <= 2 {
        year -= 1;
    }
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let adjusted_month = month + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * adjusted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn url_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r#"(?i)https?://[^\s<>\"'`]+"#).expect("URL detector should compile")
    })
}

fn bare_domain_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(
            r#"(?i)\b(?:[A-Z0-9](?:[A-Z0-9-]*[A-Z0-9])?\.)+(?:XN--[A-Z0-9-]{2,}|[A-Z]{2,63})(?:/[A-Z0-9._~!$&()*+,;=:@%/?#-]*)?"#,
        )
        .expect("bare-domain detector should compile")
    })
}

fn domain_host_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(
            r#"(?i)\b(?:[A-Z0-9](?:[A-Z0-9-]*[A-Z0-9])?\.)+(?:XN--[A-Z0-9-]{2,}|[A-Z]{2,63})\b"#,
        )
        .expect("domain-host detector should compile")
    })
}

fn email_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"(?i)\b[A-Z0-9._%+-]+@[A-Z0-9.-]+\.[A-Z]{2,}\b")
            .expect("email detector should compile")
    })
}

fn service_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r#"(?im)\bservice_id\s*=\s*[\"']([A-Za-z0-9]{12,})[\"']"#)
            .expect("service-ID detector should compile")
    })
}

fn credential_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(
            r#"(?im)\b(?:api[_-]?secret|client[_-]?secret|password|secret|credential|access[_-]?token)[\"']?\s*[:=]\s*(?:\"((?:\\.|[^\"\\\r\n]){12,})\"|'((?:\\.|[^'\\\r\n]){12,})'|([^\s#;\"'`,]{12,}))"#,
        )
        .expect("credential detector should compile")
    })
}

fn encoded_token_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r#"(?im)\bencoded_token\s*[:=]\s*[\"']?([A-Za-z0-9+/]{32,}={0,2})"#)
            .expect("encoded-token detector should compile")
    })
}

fn non_whitespace_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| Regex::new(r"\S+").expect("token detector should compile"))
}

#[cfg(test)]
mod tests {
    use super::{exception_is_active, scan_json_lock_fields_counted};

    #[test]
    fn dense_json_lock_span_association_is_single_pass() {
        let mut entries = Vec::new();
        for index in 0..20_000 {
            entries.push(format!(
                r#"{{"description":"decoy-{index}","resolved":"https://registry.npmjs.org/package-{index}/-/package.tgz"}}"#
            ));
        }
        let text = format!("[{}]", entries.join(","));
        let (findings, steps) =
            scan_json_lock_fields_counted("package-lock.json", &text).expect("valid JSON scan");
        assert!(findings.is_empty());
        assert!(
            steps <= text.len() * 4,
            "accounted lexical work must remain linear: {steps} > 4*{}",
            text.len()
        );
    }

    #[test]
    fn service_exception_fails_at_the_exact_approved_expiry_instant() {
        let expiry = "2026-09-30T00:00:00Z";

        assert!(
            exception_is_active(expiry, "2026-09-29T23:59:59Z"),
            "exception should remain active one second before expiry"
        );
        assert!(
            !exception_is_active(expiry, "2026-09-30T00:00:00Z"),
            "exception should fail at the exact expiry instant"
        );
        assert!(
            !exception_is_active(expiry, "2026-09-30T00:00:01Z"),
            "exception should remain failed after expiry"
        );
    }
}
