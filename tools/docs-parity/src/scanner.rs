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
                    result.extend(
                        scan_text(&file.path, text, 0)
                            .into_iter()
                            .filter(|finding| finding.detector != Detector::Domain),
                    );
                    result.extend(scan_lockfile(&file.path, text)?);
                } else if !is_governance_manifest(&file.path) {
                    result.extend(scan_text(&file.path, text, 0));
                }
            }
            FileKind::Binary => {
                let media_findings = scan_media_metadata(&file.path, &contents);
                let media_matches = media_findings
                    .iter()
                    .map(|finding| (finding.selector.as_str(), finding.matched.as_str()))
                    .collect::<BTreeSet<_>>();
                result.extend(
                    scan_binary(&file.path, &contents)
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

fn scan_text(path: &str, text: &str, base_offset: usize) -> Vec<Finding> {
    let mut result = Vec::new();
    for matched in url_regex().find_iter(text) {
        let value = trim_url(matched.as_str());
        if !fictional_or_local_url(value) {
            result.push(finding(
                path,
                Detector::Domain,
                value,
                base_offset + matched.start(),
                base_offset + matched.start() + value.len(),
            ));
        }
    }
    for matched in bare_domain_regex().find_iter(text) {
        if matched.start() > 0 && text.as_bytes()[matched.start() - 1] == b'@' {
            continue;
        }
        if preceding_token_is_url(text, matched.start()) {
            continue;
        }
        if !bare_domain_has_terminator(text, matched.end()) {
            continue;
        }
        let value = trim_url(matched.as_str());
        if !fictional_or_local_url(value) {
            result.push(finding(
                path,
                Detector::Domain,
                value,
                base_offset + matched.start(),
                base_offset + matched.start() + value.len(),
            ));
        }
    }
    for matched in email_regex().find_iter(text) {
        if !preceding_token_is_url(text, matched.start()) && !fictional_email(matched.as_str()) {
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
        if let Some(value) = captures.get(1).or_else(|| captures.get(2)) {
            let is_quoted = captures.get(1).is_some();
            if !is_quoted && !value.as_str().bytes().any(|byte| byte.is_ascii_digit()) {
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

fn preceding_token_is_url(text: &str, start: usize) -> bool {
    text[..start]
        .rsplit(|character: char| {
            character.is_whitespace()
                || matches!(character, '"' | '\'' | '`' | '<' | '>' | '(' | '[' | '{')
        })
        .next()
        .is_some_and(|prefix| prefix.contains("://"))
}

fn scan_binary(path: &str, contents: &[u8]) -> Vec<Finding> {
    printable_strings(contents)
        .into_iter()
        .flat_map(|(offset, text)| scan_text(path, &text, offset))
        .map(|finding| Finding {
            detector: Detector::BinaryString,
            ..finding
        })
        .collect()
}

fn scan_media_metadata(path: &str, contents: &[u8]) -> Vec<Finding> {
    let metadata = if path.ends_with(".png") {
        png_metadata(contents)
    } else if path.ends_with(".jpg") || path.ends_with(".jpeg") {
        jpeg_metadata(contents)
    } else {
        Vec::new()
    };
    metadata
        .into_iter()
        .flat_map(|(offset, text)| scan_text(path, &text, offset))
        .map(|finding| Finding {
            detector: Detector::MediaMetadata,
            ..finding
        })
        .collect()
}

fn scan_lockfile(path: &str, text: &str) -> Result<Vec<Finding>, Report<ScannerError>> {
    if path.ends_with("Cargo.lock") {
        let _value: toml::Value = toml::from_str(text)
            .change_context(ScannerError::Finding)
            .attach(format!("cannot parse structured lockfile: {path}"))?;
        scan_toml_lock_fields(path, text)
    } else if path.ends_with("package-lock.json") {
        let _value: JsonValue = serde_json::from_str(text)
            .change_context(ScannerError::Finding)
            .attach(format!("cannot parse structured lockfile: {path}"))?;
        scan_json_lock_fields(path, text)
    } else {
        Ok(Vec::new())
    }
}

fn scan_toml_lock_fields(path: &str, text: &str) -> Result<Vec<Finding>, Report<ScannerError>> {
    let mut findings = Vec::new();
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if let Some((key, rest)) = trimmed.split_once('=')
            && lock_field(Some(key.trim()))
        {
            let value_text = rest.trim_start();
            if !value_text.starts_with('"') {
                return Err(Report::new(ScannerError::Finding).attach(format!(
                    "unsupported structured lockfile field shape: {path}"
                )));
            }
            let Some(end_quote) = quoted_end(value_text.as_bytes(), 0) else {
                return Err(Report::new(ScannerError::Finding)
                    .attach(format!("unterminated structured lockfile field: {path}")));
            };
            let raw = &value_text[..=end_quote];
            let value: String = toml::from_str(&format!("value = {raw}"))
                .ok()
                .and_then(|v: toml::Value| v.get("value")?.as_str().map(str::to_owned))
                .ok_or_else(|| {
                    Report::new(ScannerError::Finding).attach("invalid TOML lockfile string")
                })?;
            if sensitive_lock_value(&value) {
                let start = offset + line.len() - trimmed.len() + trimmed.len() - rest.len()
                    + rest.len()
                    - value_text.len()
                    + 1;
                findings.push(finding(
                    path,
                    Detector::LockfileField,
                    &value,
                    start,
                    start + raw.len() - 2,
                ));
            }
        }
        offset += line.len();
    }
    Ok(findings)
}

fn scan_json_lock_fields(path: &str, text: &str) -> Result<Vec<Finding>, Report<ScannerError>> {
    let bytes = text.as_bytes();
    let mut strings = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'"' {
            i += 1;
            continue;
        }
        let Some(end) = quoted_end(bytes, i) else {
            return Err(Report::new(ScannerError::Finding)
                .attach(format!("unterminated JSON lockfile string: {path}")));
        };
        let raw = &text[i..=end];
        let value: String = serde_json::from_str(raw).change_context(ScannerError::Finding)?;
        strings.push((i, end + 1, value));
        i = end + 1;
    }
    let mut findings = Vec::new();
    for pair in strings.windows(2) {
        let (ks, ke, key) = &pair[0];
        let (vs, ve, value) = &pair[1];
        let between = &text[*ke..*vs];
        if lock_field(Some(key)) && between.trim_start().starts_with(':') {
            if sensitive_lock_value(value) {
                findings.push(finding(
                    path,
                    Detector::LockfileField,
                    value,
                    vs + 1,
                    ve - 1,
                ));
            }
        } else if lock_field(Some(key)) {
            let _ = ks;
            return Err(Report::new(ScannerError::Finding).attach(format!(
                "unsupported structured lockfile field shape: {path}"
            )));
        }
    }
    Ok(findings)
}

fn quoted_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut escaped = false;
    for (index, byte) in bytes.iter().enumerate().skip(start + 1) {
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
    let text = String::from_utf8_lossy(contents);
    let tokens = retired_tokens(&text);
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

fn retired_tokens(text: &str) -> Vec<RetiredToken> {
    non_whitespace_regex()
        .find_iter(text)
        .filter_map(|matched| {
            let raw = matched.as_str();
            let token = raw.trim_matches(|character: char| {
                matches!(
                    character,
                    '"' | '\'' | '`' | ',' | ';' | ':' | '!' | '?' | '(' | ')' | '{' | '}'
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
                start: matched.start() + leading,
                end: matched.start() + leading + token.len(),
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
        if exceptions.insert(key, false).is_some() {
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
        if let Some(used) = exceptions.get_mut(&key) {
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
    if let Some((key, _used)) = exceptions.iter().find(|(_key, used)| !**used) {
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
        ExceptionClass::VendorUrl | ExceptionClass::ProjectOwnedPublicDomain => matches!(
            record.detector,
            Detector::Domain
                | Detector::BinaryString
                | Detector::LockfileField
                | Detector::MediaMetadata
        ),
        ExceptionClass::HashPinnedFakeCredentialFixture => matches!(
            record.detector,
            Detector::CredentialShape
                | Detector::EncodedToken
                | Detector::BinaryString
                | Detector::MediaMetadata
        ),
        ExceptionClass::HistoricalExample => record.detector != Detector::ServiceId,
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

fn validate_retired_manifest(manifest: &RetiredManifest) -> Result<(), Report<ScannerError>> {
    if manifest.version != MANIFEST_VERSION {
        return Err(Report::new(ScannerError::InvalidGovernance)
            .attach("retired-identifiers manifest version must be 1"));
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
            if !scan_text(ALLOWLIST_MANIFEST, value, 0).is_empty()
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
    if project_owned_host(&finding.matched) {
        return ExceptionClass::ProjectOwnedPublicDomain;
    }
    match finding.detector {
        Detector::Domain | Detector::LockfileField => ExceptionClass::VendorUrl,
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

fn png_metadata(contents: &[u8]) -> Vec<(usize, String)> {
    if !contents.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Vec::new();
    }
    let mut offset = 8;
    let mut metadata = Vec::new();
    while offset + 12 <= contents.len() {
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
            break;
        };
        if end > contents.len() {
            break;
        }
        let chunk_type = &contents[offset + 4..offset + 8];
        let data = &contents[offset + 8..offset + 8 + length];
        if chunk_type == b"tEXt" {
            if let Some(position) = data.iter().position(|byte| *byte == 0) {
                metadata.push((
                    offset + 8 + position + 1,
                    String::from_utf8_lossy(&data[position + 1..]).into_owned(),
                ));
            }
        } else if chunk_type == b"iTXt" {
            metadata.extend(
                printable_strings(data)
                    .into_iter()
                    .map(|(position, text)| (offset + 8 + position, text)),
            );
        }
        offset = end;
    }
    metadata
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
            r#"(?i)\b(?:[A-Z0-9](?:[A-Z0-9-]*[A-Z0-9])?\.)+(?:com|org|net|io|dev|test|invalid|internal|cloud|app|co|gov|edu)\b(?:/[A-Z0-9._~!$&()*+,;=:@%/?#-]*)?"#,
        )
        .expect("bare-domain detector should compile")
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
            r#"(?im)\b(?:api[_-]?secret|client[_-]?secret|password|secret|credential|access[_-]?token)[\"']?\s*[:=]\s*(?:[\"']([A-Za-z0-9_./+=:-]{12,})[\"']|([A-Za-z0-9_./+=:-]{12,}))"#,
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
    use super::exception_is_active;

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
