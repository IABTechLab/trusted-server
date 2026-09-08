//! Canonical development-gate schema and deterministic rendering.

use std::collections::BTreeSet;
use std::path::Path;

use error_stack::Report;
use pulldown_cmark::{Event, Parser, Tag, TagEnd};
use serde::Deserialize;

use crate::repository::{NormalizedRelativePath, Repository};

const MANIFEST_VERSION: u32 = 1;
const MAXIMUM_MANIFEST_BYTES: usize = 256 * 1024;
const MAXIMUM_GATES: usize = 64;
const MAXIMUM_COMMANDS: usize = 256;
const MAXIMUM_DOCUMENT_BYTES: usize = 2 * 1024 * 1024;
const MAXIMUM_STRING_BYTES: usize = 2_048;
const START_MARKER: &str = "<!-- docs-parity:gates:start -->";
const END_MARKER: &str = "<!-- docs-parity:gates:end -->";
const MANIFEST_PATH: &str = "tools/docs-parity/manifests/gates.toml";
const OWNED_CONSUMERS: [(&str, &str); 4] = [
    ("CLAUDE.md", "## CI Gates\n\n"),
    ("AGENTS.md", "## CI Gates\n\n"),
    ("TESTING.md", "## Required local gates\n\n"),
    ("docs/guide/testing.md", "## Required gates\n\n"),
];
const LINK_ONLY_CONSUMERS: [(&str, &str); 7] = [
    ("CONTRIBUTING.md", "TESTING.md"),
    (".github/pull_request_template.md", "/CLAUDE.md#ci-gates"),
    (".claude/commands/check-ci.md", "/CLAUDE.md#ci-gates"),
    (".claude/commands/review-changes.md", "/CLAUDE.md#ci-gates"),
    (".claude/commands/test-all.md", "/CLAUDE.md#ci-gates"),
    (".claude/commands/test-crate.md", "/CLAUDE.md#ci-gates"),
    (".claude/commands/verify.md", "/CLAUDE.md#ci-gates"),
];

/// Failure while parsing or comparing canonical gates.
#[derive(Debug, derive_more::Display)]
pub enum GateError {
    /// The gate manifest is malformed or violates the closed schema.
    #[display("invalid gate manifest: {detail}")]
    Manifest {
        /// Stable failure detail.
        detail: String,
    },
    /// A link-only consumer duplicates or omits canonical gate content.
    #[display("invalid link-only gate consumer: {detail}")]
    Consumer {
        /// Stable failure detail.
        detail: String,
    },
}

impl core::error::Error for GateError {}

/// Reviewed canonical gate manifest.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct GateManifest {
    version: u32,
    reviewed: bool,
    gates: Vec<GateRecord>,
}

/// One owned gate group.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct GateRecord {
    id: String,
    title: String,
    owner: String,
    commands: Vec<String>,
}

/// Parse and validate a closed gate manifest.
///
/// # Errors
///
/// Returns an error for oversized input, unknown fields, missing review,
/// duplicate identifiers or commands, unsafe strings, or empty records.
pub fn parse_manifest(bytes: &[u8]) -> Result<GateManifest, Report<GateError>> {
    if bytes.len() > MAXIMUM_MANIFEST_BYTES {
        return Err(manifest_error("manifest exceeds 262144 bytes"));
    }
    let text =
        core::str::from_utf8(bytes).map_err(|_error| manifest_error("manifest is not UTF-8"))?;
    let manifest = toml::from_str::<GateManifest>(text)
        .map_err(|_error| manifest_error("manifest does not match the closed schema"))?;
    validate_manifest(&manifest)?;
    Ok(manifest)
}

/// Render the complete canonical gate region in lexical gate order.
///
/// # Errors
///
/// Returns an error when the in-memory manifest violates the gate schema.
pub fn render_region(manifest: &GateManifest) -> Result<String, Report<GateError>> {
    validate_manifest(manifest)?;
    let mut gates = manifest.gates.iter().collect::<Vec<_>>();
    gates.sort_unstable_by(|left, right| left.id.cmp(&right.id));
    let mut output = String::from("<!-- docs-parity:gates:start -->\n");
    for gate in gates {
        output.push_str(&format!("\n### {}\n\n", gate.title));
        output.push_str(&format!("<!-- docs-parity:owner:{} -->\n\n", gate.owner));
        for command in &gate.commands {
            output.push_str(&format!("- `{command}`\n"));
        }
    }
    output.push_str("\n<!-- docs-parity:gates:end -->\n");
    Ok(output)
}

/// Replace one existing owned gate region while preserving all other bytes.
///
/// # Errors
///
/// Returns an error for an oversized document, an invalid or non-unique
/// placement anchor, malformed ownership markers, or an invalid manifest.
pub fn render_owned_document(
    contents: &str,
    manifest: &GateManifest,
    placement_anchor: &str,
) -> Result<String, Report<GateError>> {
    if contents.len() > MAXIMUM_DOCUMENT_BYTES {
        return Err(consumer_error("gate consumer exceeds 2097152 bytes"));
    }
    validate_placement(contents, placement_anchor)?;
    let starts = contents.match_indices(START_MARKER).collect::<Vec<_>>();
    let ends = contents.match_indices(END_MARKER).collect::<Vec<_>>();
    let start = starts[0].0;
    if start >= ends[0].0 {
        return Err(consumer_error("gate ownership markers are reordered"));
    }
    let end = ends[0]
        .0
        .checked_add(END_MARKER.len())
        .ok_or_else(|| consumer_error("gate ownership marker position overflowed"))?;
    let region_end = if contents.as_bytes().get(end) == Some(&b'\n') {
        end + 1
    } else {
        end
    };
    let expected = render_region(manifest)?;
    let mut rendered = String::with_capacity(
        contents
            .len()
            .saturating_sub(region_end.saturating_sub(start))
            .saturating_add(expected.len()),
    );
    rendered.push_str(
        contents
            .get(..start)
            .ok_or_else(|| consumer_error("gate region start is invalid"))?,
    );
    rendered.push_str(&expected);
    rendered.push_str(
        contents
            .get(region_end..)
            .ok_or_else(|| consumer_error("gate region end is invalid"))?,
    );
    Ok(rendered)
}

/// Require the unique owned region in a Markdown document to equal the render
/// immediately after one exact placement anchor.
///
/// # Errors
///
/// Returns an error for missing, duplicated, reordered, malformed, or drifted
/// ownership markers and generated bytes.
pub fn check_owned_region(
    contents: &str,
    manifest: &GateManifest,
    placement_anchor: &str,
) -> Result<(), Report<GateError>> {
    let expected = render_region(manifest)?;
    validate_placement(contents, placement_anchor)?;
    let starts = contents.match_indices(START_MARKER).collect::<Vec<_>>();
    let ends = contents.match_indices(END_MARKER).collect::<Vec<_>>();
    let start = starts[0].0;
    let end = ends[0]
        .0
        .checked_add(END_MARKER.len())
        .ok_or_else(|| consumer_error("gate ownership marker position overflowed"))?;
    if start >= ends[0].0 {
        return Err(consumer_error("gate ownership markers are reordered"));
    }
    let region_end = if contents.as_bytes().get(end) == Some(&b'\n') {
        end + 1
    } else {
        end
    };
    if contents.get(start..region_end) != Some(expected.as_str()) {
        return Err(consumer_error(
            "owned gate region differs from canonical render",
        ));
    }
    Ok(())
}

fn validate_placement(contents: &str, placement_anchor: &str) -> Result<(), Report<GateError>> {
    if contents.len() > MAXIMUM_DOCUMENT_BYTES {
        return Err(consumer_error("gate consumer exceeds 2097152 bytes"));
    }
    if placement_anchor.is_empty()
        || placement_anchor.len() > MAXIMUM_STRING_BYTES
        || placement_anchor.contains('\0')
        || placement_anchor.contains("docs-parity:gates:")
    {
        return Err(consumer_error("gate placement anchor is invalid"));
    }
    for line in contents.lines() {
        if line.contains("docs-parity:gates:") && !matches!(line, START_MARKER | END_MARKER) {
            return Err(consumer_error("gate ownership marker is malformed"));
        }
    }
    let starts = contents.match_indices(START_MARKER).collect::<Vec<_>>();
    let ends = contents.match_indices(END_MARKER).collect::<Vec<_>>();
    if starts.len() != 1 || ends.len() != 1 {
        return Err(consumer_error(
            "gate ownership markers must each appear exactly once",
        ));
    }
    let start = starts[0].0;
    let anchors = contents.match_indices(placement_anchor).collect::<Vec<_>>();
    if anchors.len() != 1
        || anchors[0]
            .0
            .checked_add(placement_anchor.len())
            .is_none_or(|expected_start| expected_start != start)
    {
        return Err(consumer_error(
            "owned gate region is not at its unique placement anchor",
        ));
    }
    if start >= ends[0].0 {
        return Err(consumer_error("gate ownership markers are reordered"));
    }
    Ok(())
}

/// Require a link-only consumer to reference the canonical gate once.
///
/// # Errors
///
/// Returns an error when the exact destination is absent or duplicated, or
/// when canonical command text is copied into the consumer.
pub fn check_link_only_consumer(
    contents: &str,
    canonical_destination: &str,
    canonical_commands: &[&str],
) -> Result<(), Report<GateError>> {
    validate_destination(canonical_destination)?;
    let mut link_count = 0usize;
    let mut visible = String::new();
    let mut in_fence = false;
    for event in Parser::new(contents) {
        match event {
            Event::Start(Tag::CodeBlock(_)) => in_fence = true,
            Event::End(TagEnd::CodeBlock) => in_fence = false,
            Event::Start(Tag::Link { dest_url, .. })
                if !in_fence && dest_url.as_ref() == canonical_destination =>
            {
                link_count = link_count
                    .checked_add(1)
                    .ok_or_else(|| consumer_error("canonical link count overflowed"))?;
            }
            Event::Text(text) | Event::Code(text) if !in_fence => {
                visible.push_str(&text);
            }
            _ => {}
        }
    }
    if link_count != 1 {
        return Err(consumer_error(
            "canonical destination must appear exactly once",
        ));
    }
    for command in canonical_commands {
        validate_string(command, "canonical command")?;
        if !command.is_empty() && visible.contains(command) {
            return Err(consumer_error(format!(
                "copied canonical command: {command}"
            )));
        }
    }
    Ok(())
}

/// Check every generated and link-only gate consumer in the repository.
///
/// # Errors
///
/// Returns an error when the reviewed manifest or any declared consumer is
/// missing, unsafe, oversized, malformed, or different from its canonical
/// content.
pub(crate) fn check_repository(repository: &Repository) -> Result<(), Report<GateError>> {
    let manifest = read_repository_manifest(repository)?;
    let commands = canonical_commands(&manifest);
    for (path, anchor) in OWNED_CONSUMERS {
        let contents = read_repository_text(repository, path)?;
        check_owned_region(&contents, &manifest, anchor)?;
    }
    for (path, destination) in LINK_ONLY_CONSUMERS {
        let contents = read_repository_text(repository, path)?;
        check_link_only_consumer(&contents, destination, &commands)?;
    }
    Ok(())
}

/// Check or atomically update every generated gate consumer.
///
/// All target documents are validated and rendered before the first write.
/// The return value is `true` when at least one document differs.
///
/// # Errors
///
/// Returns an error for an invalid manifest or consumer, or when a bounded
/// replacement cannot be committed atomically.
pub(crate) fn generate_repository(
    repository: &Repository,
    update: bool,
) -> Result<bool, Report<GateError>> {
    let manifest = read_repository_manifest(repository)?;
    let mut updates = Vec::new();
    for (path_text, anchor) in OWNED_CONSUMERS {
        let path = normalized_path(path_text)?;
        let original = repository
            .read_tracked_bounded(&path, MAXIMUM_DOCUMENT_BYTES)
            .map_err(|_error| consumer_error(format!("cannot read gate consumer: {path_text}")))?;
        let text = core::str::from_utf8(&original)
            .map_err(|_error| consumer_error(format!("gate consumer is not UTF-8: {path_text}")))?;
        let rendered = render_owned_document(text, &manifest, anchor)?.into_bytes();
        if rendered != original {
            updates.push((path, original, rendered));
        }
    }
    let drift = !updates.is_empty();
    if update {
        for (path, original, rendered) in updates {
            repository
                .replace_atomically_after_precommit_validation(&path, Some(&original), &rendered)
                .map_err(|_error| {
                    consumer_error(format!(
                        "cannot update gate consumer: {}",
                        path.as_path().display()
                    ))
                })?;
        }
    }
    Ok(drift)
}

fn read_repository_manifest(repository: &Repository) -> Result<GateManifest, Report<GateError>> {
    let path = normalized_path(MANIFEST_PATH)?;
    let bytes = repository
        .read_tracked_bounded(&path, MAXIMUM_MANIFEST_BYTES)
        .map_err(|_error| manifest_error("cannot read gate manifest"))?;
    parse_manifest(&bytes)
}

fn read_repository_text(
    repository: &Repository,
    path_text: &str,
) -> Result<String, Report<GateError>> {
    let path = normalized_path(path_text)?;
    let bytes = repository
        .read_tracked_bounded(&path, MAXIMUM_DOCUMENT_BYTES)
        .map_err(|_error| consumer_error(format!("cannot read gate consumer: {path_text}")))?;
    String::from_utf8(bytes)
        .map_err(|_error| consumer_error(format!("gate consumer is not UTF-8: {path_text}")))
}

fn normalized_path(path: &str) -> Result<NormalizedRelativePath, Report<GateError>> {
    NormalizedRelativePath::new(Path::new(path))
        .map_err(|_error| consumer_error(format!("unsafe gate path: {path}")))
}

fn canonical_commands(manifest: &GateManifest) -> Vec<&str> {
    manifest
        .gates
        .iter()
        .flat_map(|gate| gate.commands.iter().map(String::as_str))
        .collect()
}

fn validate_manifest(manifest: &GateManifest) -> Result<(), Report<GateError>> {
    if manifest.version != MANIFEST_VERSION || !manifest.reviewed {
        return Err(manifest_error("version 1 and reviewed=true are required"));
    }
    if manifest.gates.is_empty() || manifest.gates.len() > MAXIMUM_GATES {
        return Err(manifest_error("gate count is outside bounds"));
    }
    let mut identifiers = BTreeSet::new();
    let mut commands = BTreeSet::new();
    let mut command_count = 0usize;
    for gate in &manifest.gates {
        validate_identifier(&gate.id)?;
        validate_title(&gate.title)?;
        validate_owner(&gate.owner)?;
        if gate.title.trim().is_empty() || gate.owner.trim().is_empty() || gate.commands.is_empty()
        {
            return Err(manifest_error("title, owner, and commands are required"));
        }
        if !identifiers.insert(&gate.id) {
            return Err(manifest_error(format!("duplicate gate id: {}", gate.id)));
        }
        command_count = command_count
            .checked_add(gate.commands.len())
            .ok_or_else(|| manifest_error("command count overflow"))?;
        for command in &gate.commands {
            validate_string(command, "command")?;
            if command.trim().is_empty() || command.contains(['\r', '\n', '`']) {
                return Err(manifest_error("commands must be non-empty single lines"));
            }
            if !commands.insert(command) {
                return Err(manifest_error(format!("duplicate command: {command}")));
            }
        }
    }
    if command_count > MAXIMUM_COMMANDS {
        return Err(manifest_error("command count exceeds 256"));
    }
    Ok(())
}

fn validate_title(value: &str) -> Result<(), Report<GateError>> {
    validate_string(value, "title")?;
    if value.is_empty()
        || !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b' ' | b'-' | b'_' | b'/' | b'&' | b'+' | b'.' | b':' | b',' | b'(' | b')'
                )
        })
    {
        return Err(manifest_error("title contains unsafe Markdown characters"));
    }
    Ok(())
}

fn validate_owner(value: &str) -> Result<(), Report<GateError>> {
    validate_string(value, "owner")?;
    if value.is_empty()
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
    {
        return Err(manifest_error("owner is not normalized"));
    }
    Ok(())
}

fn validate_destination(value: &str) -> Result<(), Report<GateError>> {
    validate_string(value, "canonical destination")?;
    if value.is_empty()
        || value.bytes().any(|byte| {
            byte.is_ascii_whitespace() || matches!(byte, b'(' | b')' | b'<' | b'>' | b'\\')
        })
    {
        return Err(manifest_error("canonical destination is unsafe"));
    }
    Ok(())
}

fn validate_identifier(value: &str) -> Result<(), Report<GateError>> {
    if value.is_empty()
        || value.len() > 64
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
    {
        return Err(manifest_error("gate id is not normalized"));
    }
    Ok(())
}

fn validate_string(value: &str, field: &'static str) -> Result<(), Report<GateError>> {
    if value.len() > MAXIMUM_STRING_BYTES || value.chars().any(char::is_control) {
        return Err(manifest_error(format!("{field} exceeds string bounds")));
    }
    Ok(())
}

fn manifest_error(detail: impl Into<String>) -> Report<GateError> {
    Report::new(GateError::Manifest {
        detail: detail.into(),
    })
}

fn consumer_error(detail: impl Into<String>) -> Report<GateError> {
    Report::new(GateError::Consumer {
        detail: detail.into(),
    })
}
