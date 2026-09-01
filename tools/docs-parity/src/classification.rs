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
    #[serde(default = "reviewed_by_default")]
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
    #[serde(default = "reviewed_by_default")]
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
    line: usize,
    contents: String,
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
                let previous_by_line = previous_comments
                    .iter()
                    .filter(|comment| comment.path == record.path)
                    .filter_map(|comment| {
                        parse_selector(&comment.selector)
                            .ok()
                            .map(|line| (line, comment))
                    })
                    .collect::<BTreeMap<_, _>>();
                for span in extract_comments(contents, grammar)? {
                    let expected_fingerprint = fingerprint(span.contents.as_bytes());
                    if let Some(existing) = previous_by_line
                        .get(&span.line)
                        .filter(|record| record.fingerprint == expected_fingerprint)
                    {
                        comments.push((*existing).clone());
                    } else {
                        maintained_changed = true;
                        comments.push(CommentRecord {
                            path: record.path.clone(),
                            selector: format!("line:{}", span.line),
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
                    selector: format!("line:{}", span.line),
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

const fn reviewed_by_default() -> bool {
    true
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
        let line = parse_selector(&record.selector)?;
        validate_fingerprint(&record.fingerprint)?;
        validate_disposition(
            Some(record.disposition),
            record.exclude_kind,
            &format!("comment span: {path}:{line}"),
        )?;
        if records.insert(line, record).is_some() {
            return Err(Report::new(ClassificationError::InvalidManifest)
                .attach(format!("duplicate comment selector: {path}:{line}")));
        }
    }
    for span in spans {
        let Some(record) = records.remove(&span.line) else {
            return Err(Report::new(ClassificationError::Incomplete)
                .attach(format!("unclassified comment span: {path}:{}", span.line)));
        };
        if record.fingerprint != fingerprint(span.contents.as_bytes()) {
            return Err(Report::new(ClassificationError::Incomplete).attach(format!(
                "comment fingerprint mismatch: {path}:{}",
                span.line
            )));
        }
    }
    if let Some((line, _record)) = records.first_key_value() {
        return Err(
            Report::new(ClassificationError::InvalidManifest).attach(format!(
                "comment selector has no extracted span: {path}:{line}"
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
        "shell" | "toml" | "yaml" | "python" | "dockerfile" => {
            Ok(extract_prefixed_lines(text, "#"))
        }
        "rust" | "javascript" | "protobuf" => Ok(extract_slash_comments(text)),
        "markdown" => Ok(extract_markdown_comments(text)),
        _ => Err(Report::new(ClassificationError::InvalidManifest)
            .attach(format!("unsupported comment grammar: {grammar}"))),
    }
}

fn extract_prefixed_lines(text: &str, prefix: &str) -> Vec<CommentSpan> {
    text.lines()
        .enumerate()
        .filter(|(_index, line)| line.trim_start().starts_with(prefix))
        .map(|(index, line)| CommentSpan {
            line: index + 1,
            contents: line.trim().to_owned(),
        })
        .collect()
}

fn extract_slash_comments(text: &str) -> Vec<CommentSpan> {
    let mut spans = Vec::new();
    let mut block_start = None;
    let mut block = String::new();
    for (index, line) in text.lines().enumerate() {
        let line_number = index + 1;
        let trimmed = line.trim();
        if let Some(start) = block_start {
            if !block.is_empty() {
                block.push('\n');
            }
            block.push_str(trimmed);
            if trimmed.contains("*/") {
                spans.push(CommentSpan {
                    line: start,
                    contents: block.clone(),
                });
                block_start = None;
                block.clear();
            }
        } else if trimmed.starts_with("//") {
            spans.push(CommentSpan {
                line: line_number,
                contents: trimmed.to_owned(),
            });
        } else if trimmed.starts_with("/*") {
            if trimmed.contains("*/") {
                spans.push(CommentSpan {
                    line: line_number,
                    contents: trimmed.to_owned(),
                });
            } else {
                block_start = Some(line_number);
                block.push_str(trimmed);
            }
        }
    }
    if let Some(start) = block_start {
        spans.push(CommentSpan {
            line: start,
            contents: block,
        });
    }
    spans
}

fn extract_markdown_comments(text: &str) -> Vec<CommentSpan> {
    let mut spans = Vec::new();
    let mut remaining = text;
    let mut line = 1;
    while let Some(start) = remaining.find("<!--") {
        line += remaining[..start]
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count();
        let after_start = &remaining[start..];
        let Some(end) = after_start.find("-->") else {
            spans.push(CommentSpan {
                line,
                contents: after_start.trim().to_owned(),
            });
            break;
        };
        let contents = &after_start[..end + 3];
        spans.push(CommentSpan {
            line,
            contents: contents.trim().to_owned(),
        });
        line += contents.bytes().filter(|byte| *byte == b'\n').count();
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

fn parse_selector(selector: &str) -> Result<usize, Report<ClassificationError>> {
    selector
        .strip_prefix("line:")
        .and_then(|line| line.parse::<usize>().ok())
        .filter(|line| *line > 0)
        .ok_or_else(|| {
            Report::new(ClassificationError::InvalidManifest)
                .attach(format!("invalid comment selector: {selector}"))
        })
}

fn selector_line(selector: &str) -> usize {
    selector
        .strip_prefix("line:")
        .and_then(|line| line.parse().ok())
        .unwrap_or(usize::MAX)
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
