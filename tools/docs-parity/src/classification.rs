//! Closed-universe classification for every Git-tracked repository path.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use error_stack::{Report, ResultExt as _};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::repository::{NormalizedRelativePath, Repository};

const TRACKED_MANIFEST: &str = "tools/docs-parity/manifests/tracked-files.toml";
const MAINTAINED_MANIFEST: &str = "tools/docs-parity/manifests/maintained-sources.toml";
const MANIFEST_VERSION: u32 = 1;
const DEFAULT_MAXIMUM_TEXT_BYTES: usize = 4 * 1024 * 1024;
const MAXIMUM_CONFIGURED_TEXT_BYTES: usize = 16 * 1024 * 1024;

/// Failure while checking or updating source classification records.
#[derive(Debug, derive_more::Display)]
pub enum ClassificationError {
    /// A classification manifest cannot be read safely.
    #[display("cannot read a classification manifest")]
    ReadManifest,
    /// A classification manifest is malformed or internally inconsistent.
    #[display("invalid classification manifest")]
    InvalidManifest,
    /// A tracked path lacks a complete, current classification.
    #[display("tracked-file classification is incomplete")]
    Incomplete,
    /// A manifest update cannot be committed atomically.
    #[display("cannot update classification manifests")]
    Update,
}

impl core::error::Error for ClassificationError {}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct TrackedManifest {
    version: u32,
    max_text_bytes: usize,
    reviewed: bool,
    #[serde(default)]
    files: Vec<TrackedFile>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct TrackedFile {
    path: String,
    kind: FileKind,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FileKind {
    Text,
    Binary,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct MaintainedManifest {
    version: u32,
    reviewed: bool,
    #[serde(default)]
    sources: Vec<SourceRecord>,
    #[serde(default)]
    comments: Vec<CommentRecord>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SourceRecord {
    path: String,
    mode: SourceMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    disposition: Option<Disposition>,
    #[serde(skip_serializing_if = "Option::is_none")]
    exclude_kind: Option<ExcludeKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    grammar: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum SourceMode {
    Whole,
    Comments,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Disposition {
    Include,
    Exclude,
    ReviewRequired,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum ExcludeKind {
    Generated,
    Historical,
    MachineData,
    NonDocumentation,
    SourceCode,
    TestFixture,
    Vendored,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CommentRecord {
    path: String,
    selector: String,
    fingerprint: String,
    disposition: Disposition,
    #[serde(skip_serializing_if = "Option::is_none")]
    exclude_kind: Option<ExcludeKind>,
}

#[derive(Clone, Debug)]
struct CommentSpan {
    start: usize,
    end: usize,
    contents: String,
}

impl CommentSpan {
    fn selector(&self) -> String {
        format!("bytes:{}-{}", self.start, self.end)
    }
}

pub(crate) struct ClassifiedFile {
    pub(crate) path: String,
    pub(crate) kind: FileKind,
}

/// Validate that every tracked path and expected text surface is classified.
///
/// # Errors
///
/// Returns an error for unsafe repository entries, malformed or stale
/// manifests, unclassified paths or comments, invalid UTF-8, and oversized
/// expected text.
pub(crate) fn check(repository: &Repository) -> Result<(), Report<ClassificationError>> {
    checked_files(repository).map(|_files| ())
}

pub(crate) fn checked_files(
    repository: &Repository,
) -> Result<Vec<ClassifiedFile>, Report<ClassificationError>> {
    let tracked_manifest: TrackedManifest = read_manifest(repository, TRACKED_MANIFEST)?;
    let maintained_manifest: MaintainedManifest = read_manifest(repository, MAINTAINED_MANIFEST)?;
    validate(repository, &tracked_manifest, &maintained_manifest)?;
    Ok(tracked_manifest
        .files
        .into_iter()
        .map(|record| ClassifiedFile {
            path: record.path,
            kind: record.kind,
        })
        .collect())
}

/// Refresh deterministic classification candidates while preserving reviewed records.
///
/// A changed candidate set clears the manifest-level review attestation; check
/// mode refuses candidates until a reviewer verifies every explicit record.
///
/// # Errors
///
/// Returns an error when repository paths cannot be enumerated or read, an
/// existing manifest is invalid, or either manifest cannot be replaced safely.
pub(crate) fn update(repository: &Repository) -> Result<(), Report<ClassificationError>> {
    let previous_tracked = read_manifest_optional::<TrackedManifest>(repository, TRACKED_MANIFEST)?;
    let previous_maintained =
        read_manifest_optional::<MaintainedManifest>(repository, MAINTAINED_MANIFEST)?;
    let previous_kinds = previous_tracked
        .as_ref()
        .map(|manifest| {
            manifest
                .files
                .iter()
                .map(|record| (record.path.clone(), record.kind))
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();
    let previous_path_set = previous_kinds.keys().cloned().collect::<BTreeSet<_>>();
    let previous_sources = previous_maintained
        .as_ref()
        .filter(|manifest| manifest.reviewed)
        .map(|manifest| {
            manifest
                .sources
                .iter()
                .cloned()
                .map(|record| (record.path.clone(), record))
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();
    let previous_comments = previous_maintained
        .as_ref()
        .filter(|manifest| manifest.reviewed)
        .map(|manifest| manifest.comments.clone())
        .unwrap_or_default();

    let paths = repository
        .tracked_paths()
        .change_context(ClassificationError::Update)?;
    let mut files = Vec::with_capacity(paths.len());
    let mut contents_by_path = BTreeMap::new();
    for path in paths {
        let path_text = path
            .as_utf8()
            .change_context(ClassificationError::Update)?
            .to_owned();
        let contents = repository
            .read_tracked(&path)
            .change_context(ClassificationError::Update)?;
        let kind = previous_kinds.get(&path_text).copied().unwrap_or_else(|| {
            if contents.len() <= DEFAULT_MAXIMUM_TEXT_BYTES
                && core::str::from_utf8(&contents).is_ok()
            {
                FileKind::Text
            } else {
                FileKind::Binary
            }
        });
        files.push(TrackedFile {
            path: path_text.clone(),
            kind,
        });
        contents_by_path.insert(path_text, contents);
    }
    files.sort_by(|left, right| left.path.cmp(&right.path));
    let current_path_set = files
        .iter()
        .map(|record| record.path.clone())
        .collect::<BTreeSet<_>>();

    let tracked = TrackedManifest {
        version: MANIFEST_VERSION,
        max_text_bytes: previous_tracked_maximum(repository)?,
        reviewed: previous_tracked
            .as_ref()
            .is_some_and(|manifest| manifest.reviewed)
            && previous_path_set == current_path_set,
        files,
    };
    let mut sources = Vec::new();
    let mut comments = Vec::new();
    let mut maintained_changed = false;
    for record in &tracked.files {
        if record.kind == FileKind::Binary {
            continue;
        }
        if let Some(previous) = previous_sources.get(&record.path) {
            sources.push(previous.clone());
            if previous.mode == SourceMode::Comments {
                let grammar = previous.grammar.as_deref().ok_or_else(|| {
                    Report::new(ClassificationError::InvalidManifest)
                        .attach(format!("comment source has no grammar: {}", record.path))
                })?;
                let contents = core::str::from_utf8(
                    contents_by_path
                        .get(&record.path)
                        .expect("tracked text contents should exist"),
                )
                .change_context(ClassificationError::Update)?;
                let previous_by_selector = previous_comments
                    .iter()
                    .filter(|comment| comment.path == record.path)
                    .filter_map(|comment| {
                        parse_selector(&comment.selector)
                            .ok()
                            .map(|selector| (selector, comment))
                    })
                    .collect::<BTreeMap<_, _>>();
                for span in extract_comments(contents, grammar)? {
                    let expected_fingerprint = fingerprint(span.contents.as_bytes());
                    if let Some(existing) = previous_by_selector
                        .get(&(span.start, span.end))
                        .filter(|record| record.fingerprint == expected_fingerprint)
                    {
                        comments.push((*existing).clone());
                    } else {
                        maintained_changed = true;
                        comments.push(CommentRecord {
                            path: record.path.clone(),
                            selector: span.selector(),
                            fingerprint: expected_fingerprint,
                            disposition: Disposition::Include,
                            exclude_kind: None,
                        });
                    }
                }
            }
            continue;
        }

        maintained_changed = true;

        if let Some(grammar) = operational_comment_grammar(&record.path) {
            sources.push(SourceRecord {
                path: record.path.clone(),
                mode: SourceMode::Comments,
                disposition: None,
                exclude_kind: None,
                grammar: Some(grammar.to_owned()),
            });
            let contents = core::str::from_utf8(
                contents_by_path
                    .get(&record.path)
                    .expect("tracked text contents should exist"),
            )
            .expect("candidate text should be valid UTF-8");
            for span in extract_comments(contents, grammar)
                .expect("bootstrap grammar should always have an extractor")
            {
                comments.push(CommentRecord {
                    path: record.path.clone(),
                    selector: span.selector(),
                    fingerprint: fingerprint(span.contents.as_bytes()),
                    disposition: Disposition::Include,
                    exclude_kind: None,
                });
            }
        } else {
            let (disposition, exclude_kind) = whole_file_disposition(&record.path);
            sources.push(SourceRecord {
                path: record.path.clone(),
                mode: SourceMode::Whole,
                disposition: Some(disposition),
                exclude_kind,
                grammar: None,
            });
        }
    }
    sources.sort_by(|left, right| left.path.cmp(&right.path));
    comments.sort_by(|left, right| {
        (&left.path, selector_line(&left.selector))
            .cmp(&(&right.path, selector_line(&right.selector)))
    });
    let current_comment_keys = comments
        .iter()
        .map(|record| {
            (
                record.path.as_str(),
                record.selector.as_str(),
                record.fingerprint.as_str(),
            )
        })
        .collect::<BTreeSet<_>>();
    let previous_comment_keys = previous_comments
        .iter()
        .map(|record| {
            (
                record.path.as_str(),
                record.selector.as_str(),
                record.fingerprint.as_str(),
            )
        })
        .collect::<BTreeSet<_>>();
    maintained_changed |=
        sources.len() != previous_sources.len() || current_comment_keys != previous_comment_keys;
    let maintained = MaintainedManifest {
        version: MANIFEST_VERSION,
        reviewed: previous_maintained
            .as_ref()
            .is_some_and(|manifest| manifest.reviewed)
            && !maintained_changed,
        sources,
        comments,
    };

    write_manifest(repository, TRACKED_MANIFEST, &tracked)?;
    write_manifest(repository, MAINTAINED_MANIFEST, &maintained)
}

fn validate(
    repository: &Repository,
    tracked_manifest: &TrackedManifest,
    maintained_manifest: &MaintainedManifest,
) -> Result<(), Report<ClassificationError>> {
    validate_versions(tracked_manifest, maintained_manifest)?;
    let tracked_paths = repository
        .tracked_paths()
        .change_context(ClassificationError::Incomplete)?;
    let actual_paths = tracked_paths
        .iter()
        .map(|path| {
            path.as_utf8()
                .map(str::to_owned)
                .change_context(ClassificationError::Incomplete)
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    let declared_files = unique_files(&tracked_manifest.files)?;
    let declared_paths = declared_files.keys().cloned().collect();
    if let Some(path) = actual_paths.difference(&declared_paths).next() {
        return Err(Report::new(ClassificationError::Incomplete)
            .attach(format!("unclassified tracked path: {path}")));
    }
    for path in declared_files.keys() {
        if !actual_paths.contains(path) {
            return Err(
                Report::new(ClassificationError::InvalidManifest).attach(format!(
                    "classification references an untracked path: {path}"
                )),
            );
        }
    }

    let sources = unique_sources(&maintained_manifest.sources)?;
    let text_paths = declared_files
        .iter()
        .filter(|(_path, kind)| **kind == FileKind::Text)
        .map(|(path, _kind)| path.clone())
        .collect::<BTreeSet<_>>();
    let source_paths = sources.keys().cloned().collect();
    if let Some(path) = text_paths.difference(&source_paths).next() {
        return Err(Report::new(ClassificationError::Incomplete).attach(format!(
            "expected text has no maintained-source disposition: {path}"
        )));
    }
    for path in sources.keys() {
        if !text_paths.contains(path) {
            return Err(Report::new(ClassificationError::InvalidManifest)
                .attach(format!("maintained source is not expected text: {path}")));
        }
    }
    for comment in &maintained_manifest.comments {
        let Some(source) = sources.get(&comment.path) else {
            return Err(
                Report::new(ClassificationError::InvalidManifest).attach(format!(
                    "comment record references an unknown text source: {}",
                    comment.path
                )),
            );
        };
        if source.mode != SourceMode::Comments {
            return Err(
                Report::new(ClassificationError::InvalidManifest).attach(format!(
                    "comment record references a non-comment source: {}",
                    comment.path
                )),
            );
        }
    }

    for path in tracked_paths {
        let path_text = path
            .as_utf8()
            .change_context(ClassificationError::Incomplete)?;
        let kind = declared_files
            .get(path_text)
            .expect("set equality should provide a classification");
        let contents = repository
            .read_tracked(&path)
            .change_context(ClassificationError::Incomplete)?;
        if *kind == FileKind::Text {
            if contents.len() > tracked_manifest.max_text_bytes {
                return Err(Report::new(ClassificationError::Incomplete).attach(format!(
                    "expected text exceeds {} bytes: {path_text}",
                    tracked_manifest.max_text_bytes
                )));
            }
            let text = core::str::from_utf8(&contents).map_err(|_error| {
                Report::new(ClassificationError::Incomplete)
                    .attach(format!("expected text is not valid UTF-8: {path_text}"))
            })?;
            validate_source(
                path_text,
                text,
                sources
                    .get(path_text)
                    .expect("text path should have a source record"),
                &maintained_manifest.comments,
            )?;
        }
    }
    Ok(())
}

fn validate_versions(
    tracked: &TrackedManifest,
    maintained: &MaintainedManifest,
) -> Result<(), Report<ClassificationError>> {
    if tracked.version != MANIFEST_VERSION || maintained.version != MANIFEST_VERSION {
        return Err(Report::new(ClassificationError::InvalidManifest)
            .attach("classification manifest version must be 1"));
    }
    if !tracked.reviewed {
        return Err(Report::new(ClassificationError::Incomplete)
            .attach("tracked-file candidates require review"));
    }
    if !maintained.reviewed {
        return Err(Report::new(ClassificationError::Incomplete)
            .attach("maintained-source candidates require review"));
    }
    if tracked.max_text_bytes == 0 || tracked.max_text_bytes > MAXIMUM_CONFIGURED_TEXT_BYTES {
        return Err(
            Report::new(ClassificationError::InvalidManifest).attach(format!(
                "max_text_bytes must be between 1 and {MAXIMUM_CONFIGURED_TEXT_BYTES}"
            )),
        );
    }
    Ok(())
}

fn unique_files(
    records: &[TrackedFile],
) -> Result<BTreeMap<String, FileKind>, Report<ClassificationError>> {
    let mut result = BTreeMap::new();
    for record in records {
        validate_manifest_path(&record.path)?;
        if result.insert(record.path.clone(), record.kind).is_some() {
            return Err(
                Report::new(ClassificationError::InvalidManifest).attach(format!(
                    "duplicate tracked-file classification: {}",
                    record.path
                )),
            );
        }
    }
    Ok(result)
}

fn unique_sources(
    records: &[SourceRecord],
) -> Result<BTreeMap<String, SourceRecord>, Report<ClassificationError>> {
    let mut result = BTreeMap::new();
    for record in records {
        validate_manifest_path(&record.path)?;
        validate_source_shape(record)?;
        if result.insert(record.path.clone(), record.clone()).is_some() {
            return Err(
                Report::new(ClassificationError::InvalidManifest).attach(format!(
                    "duplicate maintained-source record: {}",
                    record.path
                )),
            );
        }
    }
    Ok(result)
}

fn validate_source_shape(record: &SourceRecord) -> Result<(), Report<ClassificationError>> {
    match record.mode {
        SourceMode::Whole if record.grammar.is_some() => Err(Report::new(
            ClassificationError::InvalidManifest,
        )
        .attach(format!(
            "whole-file source cannot declare a grammar: {}",
            record.path
        ))),
        SourceMode::Whole => validate_disposition(
            record.disposition,
            record.exclude_kind,
            &format!("whole-file source: {}", record.path),
        ),
        SourceMode::Comments if record.disposition.is_some() || record.exclude_kind.is_some() => {
            Err(
                Report::new(ClassificationError::InvalidManifest).attach(format!(
                    "comment source dispositions belong to extracted spans: {}",
                    record.path
                )),
            )
        }
        SourceMode::Comments if record.grammar.is_none() => {
            Err(Report::new(ClassificationError::InvalidManifest)
                .attach(format!("comment source has no grammar: {}", record.path)))
        }
        SourceMode::Comments => Ok(()),
    }
}

fn validate_disposition(
    disposition: Option<Disposition>,
    exclude_kind: Option<ExcludeKind>,
    context: &str,
) -> Result<(), Report<ClassificationError>> {
    match (disposition, exclude_kind) {
        (Some(Disposition::Include), None) => Ok(()),
        (Some(Disposition::Exclude), Some(_kind)) => Ok(()),
        (Some(Disposition::ReviewRequired), _) => Err(Report::new(ClassificationError::Incomplete)
            .attach(format!("review-required disposition: {context}"))),
        _ => Err(Report::new(ClassificationError::InvalidManifest)
            .attach(format!("invalid disposition shape: {context}"))),
    }
}

fn validate_source(
    path: &str,
    text: &str,
    source: &SourceRecord,
    comment_records: &[CommentRecord],
) -> Result<(), Report<ClassificationError>> {
    if source.mode == SourceMode::Whole {
        if comment_records.iter().any(|comment| comment.path == path) {
            return Err(Report::new(ClassificationError::InvalidManifest)
                .attach(format!("whole-file source has comment selectors: {path}")));
        }
        return Ok(());
    }

    let grammar = source
        .grammar
        .as_deref()
        .expect("comment source shape should require a grammar");
    let spans = extract_comments(text, grammar)?;
    let mut records = BTreeMap::new();
    for record in comment_records.iter().filter(|record| record.path == path) {
        validate_manifest_path(&record.path)?;
        let selector = parse_selector(&record.selector)?;
        validate_fingerprint(&record.fingerprint)?;
        validate_disposition(
            Some(record.disposition),
            record.exclude_kind,
            &format!("comment span: {path}:{}", record.selector),
        )?;
        if records.insert(selector, record).is_some() {
            return Err(
                Report::new(ClassificationError::InvalidManifest).attach(format!(
                    "duplicate comment selector: {path}:{}",
                    record.selector
                )),
            );
        }
    }
    for span in spans {
        let selector = (span.start, span.end);
        let Some(record) = records.remove(&selector) else {
            return Err(Report::new(ClassificationError::Incomplete).attach(format!(
                "unclassified comment span: {path}:{}",
                span.selector()
            )));
        };
        if record.fingerprint != fingerprint(span.contents.as_bytes()) {
            return Err(Report::new(ClassificationError::Incomplete).attach(format!(
                "comment fingerprint mismatch: {path}:{}",
                span.selector()
            )));
        }
    }
    if let Some((selector, _record)) = records.first_key_value() {
        return Err(
            Report::new(ClassificationError::InvalidManifest).attach(format!(
                "comment selector has no extracted span: {path}:bytes:{}-{}",
                selector.0, selector.1
            )),
        );
    }
    Ok(())
}

fn extract_comments(
    text: &str,
    grammar: &str,
) -> Result<Vec<CommentSpan>, Report<ClassificationError>> {
    match grammar {
        "shell" | "python" | "dockerfile" => Ok(extract_hash_comments(text, true, false)),
        "toml" => Ok(extract_toml_comments(text)),
        "yaml" => Ok(extract_yaml_comments(text)),
        "rust" | "javascript" | "protobuf" => Ok(extract_slash_comments(text)),
        "markdown" => Ok(extract_markdown_comments(text)),
        _ => Err(Report::new(ClassificationError::InvalidManifest)
            .attach(format!("unsupported comment grammar: {grammar}"))),
    }
}

fn extract_hash_comments(
    text: &str,
    escaped_hash: bool,
    whitespace_hash: bool,
) -> Vec<CommentSpan> {
    let bytes = text.as_bytes();
    let mut spans = Vec::new();
    let mut quote = None;
    let mut escaped = false;
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b'\n' {
            quote = None;
            escaped = false;
            index += 1;
            continue;
        }
        if escaped {
            escaped = false;
            index += 1;
            continue;
        }
        if byte == b'\\' && (quote == Some(b'"') || quote.is_none() && escaped_hash) {
            escaped = true;
            index += 1;
            continue;
        }
        if byte == b'\'' || byte == b'"' {
            if quote == Some(byte) {
                quote = None;
            } else if quote.is_none() {
                quote = Some(byte);
            }
            index += 1;
            continue;
        }
        if byte == b'#'
            && quote.is_none()
            && (!whitespace_hash || index == 0 || bytes[index - 1].is_ascii_whitespace())
        {
            let end = bytes[index..]
                .iter()
                .position(|b| *b == b'\n')
                .map_or(bytes.len(), |n| index + n);
            spans.push(CommentSpan {
                start: index,
                end,
                contents: text[index..end].to_owned(),
            });
            index = end;
            continue;
        }
        index += 1;
    }
    spans
}

fn extract_toml_comments(text: &str) -> Vec<CommentSpan> {
    let bytes = text.as_bytes();
    let mut spans = Vec::new();
    let mut i = 0;
    let mut quote: Option<(u8, bool)> = None;
    let mut escaped = false;
    while i < bytes.len() {
        if let Some((delimiter, multiline)) = quote {
            if escaped {
                escaped = false;
                i += 1;
                continue;
            }
            if delimiter == b'"' && bytes[i] == b'\\' {
                escaped = true;
                i += 1;
                continue;
            }
            let closes = if multiline {
                i + 2 < bytes.len() && bytes[i..i + 3] == [delimiter; 3]
            } else {
                bytes[i] == delimiter
            };
            if closes {
                quote = None;
                i += if multiline { 3 } else { 1 };
            } else {
                i += 1;
            }
            continue;
        }
        if matches!(bytes[i], b'"' | b'\'') {
            let multiline = i + 2 < bytes.len() && bytes[i..i + 3] == [bytes[i]; 3];
            quote = Some((bytes[i], multiline));
            i += if multiline { 3 } else { 1 };
            continue;
        }
        if bytes[i] == b'#' {
            let end = bytes[i..]
                .iter()
                .position(|b| *b == b'\n')
                .map_or(bytes.len(), |n| i + n);
            spans.push(CommentSpan {
                start: i,
                end,
                contents: text[i..end].to_owned(),
            });
            i = end;
            continue;
        }
        i += 1;
    }
    spans
}

fn extract_yaml_comments(text: &str) -> Vec<CommentSpan> {
    let mut spans = Vec::new();
    let mut offset = 0;
    let mut block_indent = None;
    for line in text.split_inclusive('\n') {
        let body = line.strip_suffix('\n').unwrap_or(line);
        let indent = body.len() - body.trim_start().len();
        if block_indent.is_some_and(|required| body.trim().is_empty() || indent >= required) {
            offset += line.len();
            continue;
        }
        block_indent = None;
        let candidates = extract_hash_comments(body, false, true);
        spans.extend(candidates.into_iter().map(|span| CommentSpan {
            start: offset + span.start,
            end: offset + span.end,
            contents: span.contents,
        }));
        let plain = body.split('#').next().unwrap_or(body).trim_end();
        if plain.ends_with('|') || plain.ends_with('>') {
            block_indent = Some(indent + 1);
        }
        offset += line.len();
    }
    spans
}

fn extract_slash_comments(text: &str) -> Vec<CommentSpan> {
    let bytes = text.as_bytes();
    let mut spans = Vec::new();
    let mut i = 0;
    let mut quote = None;
    let mut escaped = false;
    let mut template_depth = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if escaped {
            escaped = false;
            i += 1;
            continue;
        }
        if quote.is_some() && b == b'\\' {
            escaped = true;
            i += 1;
            continue;
        }
        if quote == Some(b'`') && i + 1 < bytes.len() && &bytes[i..i + 2] == b"${" {
            quote = None;
            template_depth = 1;
            i += 2;
            continue;
        }
        if quote.is_none() && template_depth > 0 {
            if b == b'{' {
                template_depth += 1;
                i += 1;
                continue;
            }
            if b == b'}' {
                template_depth -= 1;
                i += 1;
                if template_depth == 0 {
                    quote = Some(b'`');
                }
                continue;
            }
        }
        if matches!(b, b'\'' | b'"' | b'`') {
            if quote == Some(b) {
                quote = None
            } else if quote.is_none() {
                quote = Some(b)
            };
            i += 1;
            continue;
        }
        if quote.is_none() && i + 1 < bytes.len() && &bytes[i..i + 2] == b"//" {
            let end = bytes[i..]
                .iter()
                .position(|b| *b == b'\n')
                .map_or(bytes.len(), |n| i + n);
            spans.push(CommentSpan {
                start: i,
                end,
                contents: text[i..end].to_owned(),
            });
            i = end;
            continue;
        }
        if quote.is_none() && i + 1 < bytes.len() && &bytes[i..i + 2] == b"/*" {
            let end = text[i + 2..]
                .find("*/")
                .map_or(bytes.len(), |n| i + 2 + n + 2);
            spans.push(CommentSpan {
                start: i,
                end,
                contents: text[i..end].to_owned(),
            });
            i = end;
            continue;
        }
        i += 1;
    }
    spans
}

fn extract_markdown_comments(text: &str) -> Vec<CommentSpan> {
    let mut spans = Vec::new();
    let mut remaining = text;
    let mut consumed = 0;
    while let Some(start) = remaining.find("<!--") {
        let absolute_start = consumed + start;
        let after_start = &remaining[start..];
        let Some(end) = after_start.find("-->") else {
            spans.push(CommentSpan {
                start: absolute_start,
                end: text.len(),
                contents: after_start.to_owned(),
            });
            break;
        };
        let contents = &after_start[..end + 3];
        spans.push(CommentSpan {
            start: absolute_start,
            end: absolute_start + contents.len(),
            contents: contents.to_owned(),
        });
        consumed = absolute_start + contents.len();
        remaining = &after_start[end + 3..];
    }
    spans
}

fn operational_comment_grammar(path: &str) -> Option<&'static str> {
    let file_name = Path::new(path).file_name()?.to_str()?;
    if path.starts_with(".github/workflows/")
        || path.starts_with(".github/actions/")
        || path.starts_with(".github/ISSUE_TEMPLATE/")
        || path == ".cargo/config.toml"
        || path.ends_with(".sh")
        || matches!(
            file_name,
            "fastly.toml"
                | "wrangler.toml"
                | "wrangler.ci.toml"
                | "spin.toml"
                | "axum.toml"
                | "cloudflare.toml"
                | "Dockerfile"
        )
    {
        return Some(if file_name == "Dockerfile" {
            "dockerfile"
        } else if path.ends_with(".toml") {
            "toml"
        } else if path.ends_with(".yml") || path.ends_with(".yaml") {
            "yaml"
        } else {
            "shell"
        });
    }
    if path.ends_with(".mjs") {
        return Some("javascript");
    }
    if path.ends_with(".proto") {
        return Some("protobuf");
    }
    None
}

fn whole_file_disposition(path: &str) -> (Disposition, Option<ExcludeKind>) {
    if path.starts_with("docs/superpowers/") {
        return (Disposition::Exclude, Some(ExcludeKind::Historical));
    }
    if path.contains("/fixtures/") {
        return (Disposition::Exclude, Some(ExcludeKind::TestFixture));
    }
    if path.ends_with("Cargo.lock") || path.ends_with("package-lock.json") {
        return (Disposition::Exclude, Some(ExcludeKind::MachineData));
    }
    if maintained_document(path) {
        return (Disposition::Include, None);
    }
    if path.contains("/dist/") || path.contains("/generated/") {
        return (Disposition::Exclude, Some(ExcludeKind::Generated));
    }
    if source_code(path) {
        return (Disposition::Exclude, Some(ExcludeKind::SourceCode));
    }
    if path.contains("/vendor/") || path.contains("/vendored/") {
        return (Disposition::Exclude, Some(ExcludeKind::Vendored));
    }
    (Disposition::Exclude, Some(ExcludeKind::NonDocumentation))
}

fn maintained_document(path: &str) -> bool {
    let file_name = Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let root_markdown = !path.contains('/') && path.ends_with(".md");
    let docs_markdown = path.starts_with("docs/") && path.ends_with(".md");
    let maintained_markdown = file_name.eq_ignore_ascii_case("README.md")
        || path.starts_with(".claude/commands/")
        || path.starts_with(".claude/agents/")
        || path.starts_with(".claude/skills/")
        || path == ".github/pull_request_template.md";
    root_markdown
        || docs_markdown
        || maintained_markdown
        || matches!(
            path,
            ".env.example" | ".env.dev" | "edgezero.toml" | "trusted-server.example.toml"
        )
}

fn source_code(path: &str) -> bool {
    matches!(
        Path::new(path)
            .extension()
            .and_then(|extension| extension.to_str()),
        Some(
            "c" | "css"
                | "html"
                | "js"
                | "json"
                | "jsx"
                | "mts"
                | "py"
                | "rs"
                | "scss"
                | "ts"
                | "tsx"
                | "vcl"
                | "vue"
                | "wasm"
        )
    )
}

fn parse_selector(selector: &str) -> Result<(usize, usize), Report<ClassificationError>> {
    selector
        .strip_prefix("bytes:")
        .and_then(|value| value.split_once('-'))
        .and_then(|(start, end)| Some((start.parse().ok()?, end.parse().ok()?)))
        .filter(|(start, end)| start < end)
        .ok_or_else(|| {
            Report::new(ClassificationError::InvalidManifest)
                .attach(format!("invalid comment selector: {selector}"))
        })
}

fn selector_line(selector: &str) -> usize {
    parse_selector(selector).map_or(usize::MAX, |value| value.0)
}

fn validate_manifest_path(path: &str) -> Result<(), Report<ClassificationError>> {
    NormalizedRelativePath::new(Path::new(path))
        .change_context(ClassificationError::InvalidManifest)?;
    Ok(())
}

fn validate_fingerprint(value: &str) -> Result<(), Report<ClassificationError>> {
    let valid = value.len() == 71
        && value.starts_with("sha256:")
        && value[7..].bytes().all(|byte| byte.is_ascii_hexdigit());
    if valid {
        Ok(())
    } else {
        Err(Report::new(ClassificationError::InvalidManifest)
            .attach(format!("invalid SHA-256 fingerprint: {value}")))
    }
}

fn fingerprint(contents: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(contents))
}

fn read_manifest<T>(repository: &Repository, path: &str) -> Result<T, Report<ClassificationError>>
where
    T: for<'de> Deserialize<'de>,
{
    read_manifest_optional(repository, path)?.ok_or_else(|| {
        Report::new(ClassificationError::ReadManifest).attach(format!("missing manifest: {path}"))
    })
}

fn read_manifest_optional<T>(
    repository: &Repository,
    path: &str,
) -> Result<Option<T>, Report<ClassificationError>>
where
    T: for<'de> Deserialize<'de>,
{
    let path = NormalizedRelativePath::new(Path::new(path))
        .change_context(ClassificationError::ReadManifest)?;
    let Some(contents) = repository
        .read_optional(&path)
        .change_context(ClassificationError::ReadManifest)?
    else {
        return Ok(None);
    };
    let text = core::str::from_utf8(&contents).change_context(ClassificationError::ReadManifest)?;
    toml::from_str(text)
        .map(Some)
        .change_context(ClassificationError::InvalidManifest)
}

fn write_manifest<T>(
    repository: &Repository,
    path: &str,
    manifest: &T,
) -> Result<(), Report<ClassificationError>>
where
    T: Serialize,
{
    let mut contents = toml::to_string_pretty(manifest)
        .change_context(ClassificationError::Update)?
        .into_bytes();
    if !contents.ends_with(b"\n") {
        contents.push(b'\n');
    }
    let path =
        NormalizedRelativePath::new(Path::new(path)).change_context(ClassificationError::Update)?;
    repository
        .write_atomically(&path, &contents)
        .change_context(ClassificationError::Update)
}

fn previous_tracked_maximum(repository: &Repository) -> Result<usize, Report<ClassificationError>> {
    Ok(
        read_manifest_optional::<TrackedManifest>(repository, TRACKED_MANIFEST)?
            .map_or(DEFAULT_MAXIMUM_TEXT_BYTES, |manifest| {
                manifest.max_text_bytes
            }),
    )
}
