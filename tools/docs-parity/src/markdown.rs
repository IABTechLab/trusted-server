//! Deterministic generated regions and semantic Markdown link validation.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io::Read as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use error_stack::{Report, ResultExt as _};
use github_slugger::Slugger as GithubSlugger;
use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use scraper::{Html, Selector};
use serde::Deserialize;
use unicode_normalization::UnicodeNormalization as _;
use url::Url;

use crate::repository::{NormalizedRelativePath, Repository};

const MAXIMUM_DOCUMENT_BYTES: usize = 4 * 1024 * 1024;
const MAXIMUM_LINK_BYTES: usize = 8 * 1024;
const MAXIMUM_REDIRECTS: usize = 5;
const MAXIMUM_RETRY_ATTEMPTS: usize = 3;
const MAXIMUM_RESPONSE_HEADER_BYTES: usize = 64 * 1024;
const MAXIMUM_RESPONSE_BODY_BYTES: usize = 64 * 1024;
const MAXIMUM_RESPONSE_HEADERS: usize = 128;
const MAXIMUM_HEADER_NAME_BYTES: usize = 256;
const MAXIMUM_HEADER_VALUE_BYTES: usize = 8 * 1024;
const MAXIMUM_HEADER_LINE_BYTES: usize = 8 * 1024;
const CURL_TRAILER_BYTES: usize = 128;
const CURL_WRITE_OUT: &str = "\nDOCS_PARITY_COUNTS:%{size_header}:%{size_download}\n";
const PAGES_MANIFEST: &str = "tools/docs-parity/manifests/pages.toml";
const ORPHANS_MANIFEST: &str = "tools/docs-parity/manifests/orphans.toml";
const DIAGRAMS_MANIFEST: &str = "tools/docs-parity/manifests/diagrams.toml";
const MANIFEST_VERSION: u32 = 1;

/// Failure while parsing or validating generated Markdown records.
#[derive(Debug, derive_more::Display)]
pub enum MarkdownError {
    /// A generated marker does not follow the closed grammar.
    #[display("invalid generated marker: {detail}")]
    Marker {
        /// Stable diagnostic detail.
        detail: String,
    },
    /// A generated record is malformed or not bound exactly once.
    #[display("invalid generated record: {detail}")]
    GeneratedRecord {
        /// Stable diagnostic detail.
        detail: String,
    },
    /// Markdown input is malformed or violates a local-link contract.
    #[display("invalid Markdown link: {detail}")]
    LocalLink {
        /// Stable diagnostic detail.
        detail: String,
    },
    /// An external link violates its bounded transport contract.
    #[display("invalid external link: {detail}")]
    ExternalLink {
        /// Stable diagnostic detail.
        detail: String,
    },
}

impl core::error::Error for MarkdownError {}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PagesManifest {
    version: u32,
    reviewed: bool,
    site_root: String,
    vitepress_config: String,
    #[serde(default)]
    pages: Vec<PageRecord>,
    #[serde(default)]
    regions: Vec<RegionRecord>,
    #[serde(default)]
    ownership: Vec<OwnershipManifestRecord>,
    #[serde(default)]
    external_exceptions: Vec<ExternalExceptionRecord>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum PageRecord {
    Live {
        path: String,
        route: String,
        navigation: bool,
    },
    Tombstone {
        route: String,
        replacement: String,
    },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegionRecord {
    name: String,
    path: String,
    columns: Vec<String>,
    #[serde(default)]
    rows: Vec<GeneratedRowRecord>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GeneratedRowRecord {
    key: String,
    cells: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OwnershipManifestRecord {
    name: String,
    path: String,
    owner: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExternalExceptionRecord {
    url: String,
    owner: String,
    reason: String,
    expires_at: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OrphansManifest {
    version: u32,
    reviewed: bool,
    #[serde(default)]
    exceptions: Vec<OrphanRecord>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum OrphanKind {
    Manual,
    Tombstone,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OrphanRecord {
    kind: OrphanKind,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    route: Option<String>,
    #[serde(default)]
    replacement: Option<String>,
    owner: String,
    reason: String,
    expires_at: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DiagramsManifest {
    version: u32,
    reviewed: bool,
    #[serde(default)]
    diagrams: Vec<DiagramRecord>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DiagramRecord {
    path: String,
    selector: String,
    prose_anchor: String,
    owner: String,
}

fn read_pages_manifest(repository: &Repository) -> Result<PagesManifest, Report<MarkdownError>> {
    let path = NormalizedRelativePath::new(Path::new(PAGES_MANIFEST)).change_context(
        MarkdownError::GeneratedRecord {
            detail: "pages manifest path is invalid".to_owned(),
        },
    )?;
    let bytes = repository
        .read_optional(&path)
        .change_context(MarkdownError::GeneratedRecord {
            detail: "cannot read pages manifest".to_owned(),
        })?
        .ok_or_else(|| generated_error("pages manifest is missing"))?;
    if bytes.len() > MAXIMUM_DOCUMENT_BYTES {
        return Err(generated_error(format!(
            "pages manifest exceeds {MAXIMUM_DOCUMENT_BYTES} bytes"
        )));
    }
    let text = core::str::from_utf8(&bytes)
        .map_err(|_error| generated_error("pages manifest is not valid UTF-8"))?;
    let manifest: PagesManifest = toml::from_str(text)
        .map_err(|error| generated_error(format!("pages manifest is malformed: {error}")))?;
    validate_pages_header(&manifest)?;
    Ok(manifest)
}

fn validate_pages_header(manifest: &PagesManifest) -> Result<(), Report<MarkdownError>> {
    if manifest.version != MANIFEST_VERSION {
        return Err(generated_error(format!(
            "pages manifest version must be {MANIFEST_VERSION}"
        )));
    }
    if !manifest.reviewed {
        return Err(generated_error(
            "pages manifest must be explicitly reviewed",
        ));
    }
    validate_repo_path(&manifest.site_root)?;
    validate_repo_path(&manifest.vitepress_config)?;
    Ok(())
}

fn read_toml_manifest<T: for<'de> Deserialize<'de>>(
    repository: &Repository,
    manifest_path: &str,
) -> Result<T, Report<MarkdownError>> {
    let path = NormalizedRelativePath::new(Path::new(manifest_path)).change_context(
        MarkdownError::LocalLink {
            detail: format!("unsafe manifest path: {manifest_path}"),
        },
    )?;
    let bytes = repository
        .read_optional(&path)
        .change_context(MarkdownError::LocalLink {
            detail: format!("cannot read manifest: {manifest_path}"),
        })?
        .ok_or_else(|| local_error(format!("manifest is missing: {manifest_path}")))?;
    if bytes.len() > MAXIMUM_DOCUMENT_BYTES {
        return Err(local_error(format!(
            "manifest exceeds {MAXIMUM_DOCUMENT_BYTES} bytes: {manifest_path}"
        )));
    }
    let text = core::str::from_utf8(&bytes)
        .map_err(|_error| local_error(format!("manifest is not UTF-8: {manifest_path}")))?;
    toml::from_str(text)
        .map_err(|error| local_error(format!("malformed manifest {manifest_path}: {error}")))
}

/// Check or atomically update every generated region declared in `pages.toml`.
///
/// The function validates and renders all documents before the first write.
/// In check mode it never writes and returns `true` when any document drifts.
///
/// # Errors
///
/// Returns an error for malformed governance, unsafe repository paths or
/// entries, missing target documents, invalid markers, and atomic write
/// failures.
pub(crate) fn generate(
    repository: &Repository,
    update: bool,
) -> Result<bool, Report<MarkdownError>> {
    let manifest = read_pages_manifest(repository)?;
    let mut regions_by_path = BTreeMap::<String, Vec<GeneratedRegion>>::new();
    for record in manifest.regions {
        validate_repo_path(&record.path)?;
        regions_by_path
            .entry(record.path)
            .or_default()
            .push(GeneratedRegion {
                name: record.name,
                columns: record.columns,
                rows: record
                    .rows
                    .into_iter()
                    .map(|row| GeneratedRow {
                        key: row.key,
                        cells: row.cells,
                    })
                    .collect(),
            });
    }
    let mut ownership_by_path = BTreeMap::<String, Vec<OwnershipRecord>>::new();
    for record in manifest.ownership {
        validate_repo_path(&record.path)?;
        ownership_by_path
            .entry(record.path)
            .or_default()
            .push(OwnershipRecord {
                name: record.name,
                owner: record.owner,
            });
    }
    let target_paths = regions_by_path
        .keys()
        .chain(ownership_by_path.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut updates = Vec::new();
    for path_text in target_paths {
        let path = NormalizedRelativePath::new(Path::new(&path_text)).change_context(
            MarkdownError::GeneratedRecord {
                detail: format!("unsafe generated target: {path_text}"),
            },
        )?;
        let original = repository
            .read_optional(&path)
            .change_context(MarkdownError::GeneratedRecord {
                detail: format!("cannot safely read generated target: {path_text}"),
            })?
            .ok_or_else(|| generated_error(format!("generated target is missing: {path_text}")))?;
        let empty_regions = Vec::new();
        let empty_ownership = Vec::new();
        let rendered = render_generated_document(
            &original,
            regions_by_path.get(&path_text).unwrap_or(&empty_regions),
            ownership_by_path
                .get(&path_text)
                .unwrap_or(&empty_ownership),
        )?;
        if rendered != original {
            updates.push((path, rendered));
        }
    }
    let drift = !updates.is_empty();
    if update {
        for (path, contents) in updates {
            repository
                .write_atomically(&path, &contents)
                .change_context(MarkdownError::GeneratedRecord {
                    detail: format!(
                        "cannot atomically update generated target: {}",
                        path.as_path().display()
                    ),
                })?;
        }
    }
    Ok(drift)
}

/// One deterministic row within a generated Markdown table.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GeneratedRow {
    /// Stable, unique ordering key. The key itself is not rendered.
    pub key: String,
    /// Cells rendered in the region's declared column order.
    pub cells: Vec<String>,
}

/// A named, deterministic Markdown table region.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GeneratedRegion {
    /// Marker name using lowercase letters, digits, and hyphens.
    pub name: String,
    /// Non-empty table headings.
    pub columns: Vec<String>,
    /// Rows sorted by [`GeneratedRow::key`] before rendering.
    pub rows: Vec<GeneratedRow>,
}

/// Required ownership marker for adjacent manually maintained prose.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OwnershipRecord {
    /// Unique ownership marker name.
    pub name: String,
    /// Non-empty owner identity rendered in the marker.
    pub owner: String,
}

#[derive(Clone, Debug)]
struct RegionSpan {
    name: String,
    content_start: usize,
    content_end: usize,
    newline: &'static str,
}

#[derive(Clone, Debug)]
struct OpenRegion {
    name: String,
    content_start: usize,
    newline: &'static str,
}

type MarkerParse = (Vec<RegionSpan>, BTreeSet<(String, String)>);

/// Render every named region while preserving all bytes outside region bodies.
///
/// Marker lines use the exact closed grammar
/// `<!-- docs-parity:start NAME -->` and
/// `<!-- docs-parity:end NAME -->`. Ownership markers use
/// `<!-- docs-parity:ownership NAME owner=OWNER -->`.
///
/// # Errors
///
/// Returns an error for invalid UTF-8, oversized input, malformed or unsafe
/// markers, duplicate/missing/nested regions, unknown names, invalid rows, or
/// missing ownership attestations.
pub fn render_generated_document(
    source: &[u8],
    regions: &[GeneratedRegion],
    ownership: &[OwnershipRecord],
) -> Result<Vec<u8>, Report<MarkdownError>> {
    if source.len() > MAXIMUM_DOCUMENT_BYTES {
        return Err(generated_error("document exceeds 4194304 bytes"));
    }
    let text = core::str::from_utf8(source)
        .map_err(|_error| generated_error("document is not valid UTF-8"))?;
    let region_map = validate_regions(regions)?;
    let ownership_map = validate_ownership(ownership)?;
    let (mut spans, found_ownership) = parse_markers(text, &region_map, &ownership_map)?;

    for name in region_map.keys() {
        let count = spans.iter().filter(|span| &span.name == name).count();
        if count != 1 {
            return Err(generated_error(format!(
                "record {name} must have exactly one marker pair; found {count}"
            )));
        }
    }
    for (name, owner) in &ownership_map {
        if !found_ownership.contains(&(name.clone(), owner.clone())) {
            return Err(generated_error(format!(
                "ownership marker {name} with owner {owner} is missing"
            )));
        }
    }

    spans.sort_by_key(|span| span.content_start);
    let mut rendered = source.to_vec();
    for span in spans.into_iter().rev() {
        let region = region_map
            .get(&span.name)
            .ok_or_else(|| generated_error(format!("unknown generated region: {}", span.name)))?;
        let body = render_table(region, span.newline)?;
        rendered.splice(span.content_start..span.content_end, body.bytes());
        if rendered.len() > MAXIMUM_DOCUMENT_BYTES {
            return Err(generated_error(format!(
                "rendered document exceeds {MAXIMUM_DOCUMENT_BYTES} bytes"
            )));
        }
    }
    Ok(rendered)
}

fn validate_regions(
    regions: &[GeneratedRegion],
) -> Result<BTreeMap<String, GeneratedRegion>, Report<MarkdownError>> {
    let mut result = BTreeMap::new();
    for region in regions {
        validate_marker_component(&region.name, "region name")?;
        if region.columns.is_empty() {
            return Err(generated_error(format!(
                "region {} has no columns",
                region.name
            )));
        }
        if result.insert(region.name.clone(), region.clone()).is_some() {
            return Err(generated_error(format!(
                "duplicate generated record: {}",
                region.name
            )));
        }
        let mut keys = BTreeSet::new();
        for row in &region.rows {
            if row.key.trim().is_empty() {
                return Err(generated_error(format!(
                    "region {} has a blank row key",
                    region.name
                )));
            }
            if !keys.insert(row.key.as_str()) {
                return Err(generated_error(format!(
                    "region {} has duplicate row key {}",
                    region.name, row.key
                )));
            }
            if row.cells.len() != region.columns.len() {
                return Err(generated_error(format!(
                    "region {} row {} has {} cells; expected {}",
                    region.name,
                    row.key,
                    row.cells.len(),
                    region.columns.len()
                )));
            }
        }
    }
    Ok(result)
}

fn validate_ownership(
    records: &[OwnershipRecord],
) -> Result<BTreeMap<String, String>, Report<MarkdownError>> {
    let mut result = BTreeMap::new();
    for record in records {
        validate_marker_component(&record.name, "ownership name")?;
        validate_marker_component(&record.owner, "owner")?;
        if result
            .insert(record.name.clone(), record.owner.clone())
            .is_some()
        {
            return Err(generated_error(format!(
                "duplicate ownership record: {}",
                record.name
            )));
        }
    }
    Ok(result)
}

fn validate_marker_component(value: &str, field: &str) -> Result<(), Report<MarkdownError>> {
    let valid = !value.is_empty()
        && value.len() <= 64
        && value.as_bytes()[0].is_ascii_lowercase()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
    if valid {
        Ok(())
    } else {
        Err(generated_error(format!("invalid {field}: {value}")))
    }
}

fn parse_markers(
    text: &str,
    regions: &BTreeMap<String, GeneratedRegion>,
    ownership: &BTreeMap<String, String>,
) -> Result<MarkerParse, Report<MarkdownError>> {
    let mut spans = Vec::new();
    let mut found_ownership = BTreeSet::new();
    let mut open: Option<OpenRegion> = None;
    let mut offset = 0;
    while offset < text.len() {
        let remaining = &text[offset..];
        let line_length = remaining
            .find('\n')
            .map_or(remaining.len(), |index| index + 1);
        let raw_line = &remaining[..line_length];
        let (line, newline) = if let Some(line) = raw_line.strip_suffix("\r\n") {
            (line, "\r\n")
        } else if let Some(line) = raw_line.strip_suffix('\n') {
            (line, "\n")
        } else {
            (raw_line, "")
        };

        if line.contains("docs-parity:") {
            if let Some(name) = marker_name(line, "<!-- docs-parity:start ", " -->") {
                validate_marker_component(name, "region name")?;
                if newline.is_empty() {
                    return Err(marker_error("start marker must end its own line"));
                }
                if !regions.contains_key(name) {
                    return Err(marker_error(format!("unknown start marker: {name}")));
                }
                if open.is_some() {
                    return Err(marker_error(format!("nested start marker: {name}")));
                }
                if spans.iter().any(|span: &RegionSpan| span.name == name) {
                    return Err(marker_error(format!("duplicate marker pair: {name}")));
                }
                open = Some(OpenRegion {
                    name: name.to_owned(),
                    content_start: offset + line_length,
                    newline: if newline == "\r\n" { "\r\n" } else { "\n" },
                });
            } else if let Some(name) = marker_name(line, "<!-- docs-parity:end ", " -->") {
                validate_marker_component(name, "region name")?;
                let Some(start) = open.take() else {
                    return Err(marker_error(format!("end marker without start: {name}")));
                };
                if start.name != name {
                    return Err(marker_error(format!(
                        "mismatched end marker {name}; expected {}",
                        start.name
                    )));
                }
                spans.push(RegionSpan {
                    name: name.to_owned(),
                    content_start: start.content_start,
                    content_end: offset,
                    newline: start.newline,
                });
            } else if let Some(inner) = marker_name(line, "<!-- docs-parity:ownership ", " -->") {
                let Some((name, owner)) = inner.split_once(" owner=") else {
                    return Err(marker_error("malformed ownership marker"));
                };
                validate_marker_component(name, "ownership name")?;
                validate_marker_component(owner, "owner")?;
                match ownership.get(name) {
                    Some(expected) if expected == owner => {}
                    Some(expected) => {
                        return Err(marker_error(format!(
                            "ownership marker {name} has owner {owner}; expected {expected}"
                        )));
                    }
                    None => {
                        return Err(marker_error(format!("unknown ownership marker: {name}")));
                    }
                }
                if !found_ownership.insert((name.to_owned(), owner.to_owned())) {
                    return Err(marker_error(format!("duplicate ownership marker: {name}")));
                }
            } else {
                return Err(marker_error(format!(
                    "unsafe marker placement or unknown marker: {line}"
                )));
            }
        }
        offset += line_length;
    }
    if let Some(start) = open {
        return Err(marker_error(format!("missing end marker: {}", start.name)));
    }
    Ok((spans, found_ownership))
}

fn marker_name<'a>(line: &'a str, prefix: &str, suffix: &str) -> Option<&'a str> {
    line.strip_prefix(prefix)?.strip_suffix(suffix)
}

fn render_table(region: &GeneratedRegion, newline: &str) -> Result<String, Report<MarkdownError>> {
    let mut rows = region.rows.clone();
    rows.sort_by(|left, right| left.key.cmp(&right.key));
    let mut output = String::new();
    output.push_str(&render_cells(&region.columns)?);
    output.push_str(newline);
    output.push('|');
    for _column in &region.columns {
        output.push_str(" --- |");
    }
    output.push_str(newline);
    for row in rows {
        output.push_str(&render_cells(&row.cells)?);
        output.push_str(newline);
    }
    Ok(output)
}

fn render_cells(cells: &[String]) -> Result<String, Report<MarkdownError>> {
    let mut line = String::from("|");
    for cell in cells {
        if cell.contains(['\r', '\n']) {
            return Err(generated_error("generated cells cannot contain newlines"));
        }
        line.push(' ');
        line.push_str(&cell.replace('\\', "\\\\").replace('|', "\\|"));
        line.push_str(" |");
    }
    Ok(line)
}

fn marker_error(detail: impl Into<String>) -> Report<MarkdownError> {
    Report::new(MarkdownError::Marker {
        detail: detail.into(),
    })
}

fn generated_error(detail: impl Into<String>) -> Report<MarkdownError> {
    Report::new(MarkdownError::GeneratedRecord {
        detail: detail.into(),
    })
}

/// Logical Markdown source set governed by the link checker.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum LinkSourceSet {
    /// Maintained public `VitePress` source.
    Public,
    /// Maintained internal documentation under `docs/internal`.
    MaintainedInternal,
    /// Other maintained repository documentation.
    Repository,
}

/// One checked Markdown source and its exact contents.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LinkSource {
    /// Normalized repository-relative path.
    pub path: String,
    /// Governed source set.
    pub set: LinkSourceSet,
    /// UTF-8 Markdown bytes.
    pub markdown: String,
}

#[derive(Clone, Debug)]
struct ParsedMarkdown {
    anchors: BTreeSet<String>,
    heading_anchors: BTreeSet<String>,
    links: Vec<ParsedLink>,
    mermaid_selectors: Vec<String>,
}

#[derive(Clone, Debug)]
struct ParsedLink {
    destination: String,
    start: usize,
    end: usize,
}

struct LinkIndex<'a> {
    source_by_path: &'a BTreeMap<String, &'a LinkSource>,
    parsed_by_path: &'a BTreeMap<String, ParsedMarkdown>,
    route_to_path: &'a BTreeMap<String, String>,
    tombstones: &'a BTreeSet<String>,
    known_excluded_paths: &'a BTreeSet<String>,
    known_paths: &'a BTreeSet<String>,
}

/// Validate local Markdown links and intended public-page membership.
///
/// # Errors
///
/// Returns an error for malformed Markdown destinations, missing files or
/// anchors, links from public pages to non-public sources, tombstone links,
/// or public pages absent from the intended page inventory.
pub fn check_local_links(
    sources: &[LinkSource],
    intended_public_pages: &[String],
    tombstone_routes: &[String],
) -> Result<(), Report<MarkdownError>> {
    let known_paths = sources
        .iter()
        .map(|source| source.path.clone())
        .collect::<BTreeSet<_>>();
    check_local_links_with_known(
        sources,
        intended_public_pages,
        tombstone_routes,
        &BTreeSet::new(),
        &known_paths,
    )
}

fn check_local_links_with_known(
    sources: &[LinkSource],
    intended_public_pages: &[String],
    tombstone_routes: &[String],
    known_excluded_paths: &BTreeSet<String>,
    known_paths: &BTreeSet<String>,
) -> Result<(), Report<MarkdownError>> {
    let mut source_by_path = BTreeMap::new();
    let mut parsed_by_path = BTreeMap::new();
    let mut route_to_path = BTreeMap::new();
    for source in sources {
        validate_repo_path(&source.path)?;
        if source.markdown.len() > MAXIMUM_DOCUMENT_BYTES {
            return Err(local_error(format!(
                "{} exceeds {} bytes",
                source.path, MAXIMUM_DOCUMENT_BYTES
            )));
        }
        if source_by_path.insert(source.path.clone(), source).is_some() {
            return Err(local_error(format!(
                "duplicate Markdown source: {}",
                source.path
            )));
        }
        let parsed = parse_markdown(&source.path, source.set, &source.markdown)?;
        parsed_by_path.insert(source.path.clone(), parsed);
        if source.set == LinkSourceSet::Public {
            let route = public_route(&source.path)?;
            if route_to_path
                .insert(route.clone(), source.path.clone())
                .is_some()
            {
                return Err(local_error(format!("duplicate public route: {route}")));
            }
        }
    }

    if !intended_public_pages.is_empty() {
        let intended = intended_public_pages
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        let actual = sources
            .iter()
            .filter(|source| source.set == LinkSourceSet::Public)
            .map(|source| source.path.clone())
            .collect::<BTreeSet<_>>();
        if intended != actual {
            let missing = actual.difference(&intended).next();
            let stale = intended.difference(&actual).next();
            return Err(local_error(format!(
                "public page inventory mismatch; unlisted orphan={missing:?}, stale page={stale:?}"
            )));
        }
    }

    let tombstones = tombstone_routes.iter().cloned().collect::<BTreeSet<_>>();
    let index = LinkIndex {
        source_by_path: &source_by_path,
        parsed_by_path: &parsed_by_path,
        route_to_path: &route_to_path,
        tombstones: &tombstones,
        known_excluded_paths,
        known_paths,
    };
    for source in sources {
        let parsed = parsed_by_path
            .get(&source.path)
            .ok_or_else(|| local_error(format!("parsed source missing for {}", source.path)))?;
        for destination in &parsed.links {
            check_local_destination(source, destination, &index)?;
        }
    }
    Ok(())
}

fn check_local_destination(
    source: &LinkSource,
    link: &ParsedLink,
    index: &LinkIndex<'_>,
) -> Result<(), Report<MarkdownError>> {
    let destination = link.destination.as_str();
    if destination.len() > MAXIMUM_LINK_BYTES {
        return Err(local_error(format!(
            "{} has an oversized destination at bytes {}-{}",
            source.path, link.start, link.end
        )));
    }
    if destination
        .chars()
        .any(|character| character.is_control() || character == '\\')
    {
        return Err(local_error(format!(
            "{} has an unsafe destination: {destination}",
            source.path
        )));
    }
    if is_external_or_non_file(destination) {
        return Ok(());
    }

    let (without_fragment, raw_fragment) = destination
        .split_once('#')
        .map_or((destination, None), |(path, fragment)| {
            (path, Some(fragment))
        });
    let (raw_path, raw_query) = without_fragment
        .split_once('?')
        .map_or((without_fragment, None), |(path, query)| {
            (path, Some(query))
        });
    let path = strict_percent_decode(raw_path, &source.path)?;
    raw_query
        .map(|query| strict_percent_decode(query, &source.path))
        .transpose()?;
    let fragment = raw_fragment
        .map(|value| strict_percent_decode(value, &source.path))
        .transpose()?;

    let (target_path, target_route) = if path.is_empty() {
        (
            source.path.clone(),
            (source.set == LinkSourceSet::Public)
                .then(|| public_route(&source.path))
                .transpose()?,
        )
    } else if path.starts_with('/') {
        if source.set == LinkSourceSet::Public {
            let route = normalize_route(&path)?;
            if let Some(excluded) =
                excluded_public_target(&path, &route, index.known_excluded_paths)
            {
                return Err(local_error(format!(
                    "{} links to excluded source {excluded}",
                    source.path
                )));
            }
            if index.tombstones.contains(&route) {
                return Err(local_error(format!(
                    "{} links to tombstone route {route}",
                    source.path
                )));
            }
            let target = index.route_to_path.get(&route).cloned().ok_or_else(|| {
                local_error(format!(
                    "{} has missing VitePress route {route}",
                    source.path
                ))
            })?;
            (target, Some(route))
        } else {
            let route = normalize_route(&path)?;
            if let Some(target) = index.route_to_path.get(&route) {
                return validate_target_anchor(
                    source,
                    target,
                    fragment.as_deref(),
                    index.source_by_path,
                    index.parsed_by_path,
                );
            }
            let target = path.trim_start_matches('/').to_owned();
            validate_repo_path(&target)?;
            if !index.source_by_path.contains_key(&target) {
                if index.known_excluded_paths.contains(&target) {
                    return Err(local_error(format!(
                        "{} links to excluded source {target}",
                        source.path
                    )));
                }
                if index.known_paths.contains(&target) && fragment.is_none() {
                    return Ok(());
                }
                return Err(local_error(format!(
                    "{} has missing repository-root file {target}",
                    source.path
                )));
            }
            (target, None)
        }
    } else {
        let target = resolve_relative_path(&source.path, &path)?;
        if let Some(found) = index.source_by_path.get(&target) {
            let route = (found.set == LinkSourceSet::Public)
                .then(|| public_route(&target))
                .transpose()?;
            (target, route)
        } else if index.known_excluded_paths.contains(&target) {
            return Err(local_error(format!(
                "{} links to excluded source {target}",
                source.path
            )));
        } else if index.known_paths.contains(&target) {
            if fragment.is_some() {
                return Err(local_error(format!(
                    "{} uses an anchor on non-Markdown target {target}",
                    source.path
                )));
            }
            return Ok(());
        } else {
            let route_candidate = route_for_relative_target(&target)?;
            if let Some(found) = index.route_to_path.get(&route_candidate) {
                (found.clone(), Some(route_candidate))
            } else if index.known_excluded_paths.contains(&target) {
                return Err(local_error(format!(
                    "{} links to excluded source {target}",
                    source.path
                )));
            } else {
                return Err(local_error(format!(
                    "{} has missing relative file {destination}",
                    source.path
                )));
            }
        }
    };

    if let Some(route) = target_route.as_ref()
        && index.tombstones.contains(route)
    {
        return Err(local_error(format!(
            "{} links to tombstone route {route}",
            source.path
        )));
    }
    let target_source = index.source_by_path.get(&target_path).ok_or_else(|| {
        local_error(format!(
            "{} resolves to an unchecked target {target_path}",
            source.path
        ))
    })?;
    if source.set == LinkSourceSet::Public && target_source.set != LinkSourceSet::Public {
        return Err(local_error(format!(
            "public page {} links to excluded source {target_path}",
            source.path
        )));
    }
    if let Some(fragment) = fragment {
        if fragment.is_empty() {
            return Ok(());
        }
        let target = index
            .parsed_by_path
            .get(&target_path)
            .ok_or_else(|| local_error(format!("target was not parsed: {target_path}")))?;
        if !target.anchors.contains(&fragment) {
            return Err(local_error(format!(
                "{} has missing anchor #{fragment} in {target_path}",
                source.path
            )));
        }
    }
    Ok(())
}

fn validate_target_anchor(
    source: &LinkSource,
    target_path: &str,
    fragment: Option<&str>,
    source_by_path: &BTreeMap<String, &LinkSource>,
    parsed_by_path: &BTreeMap<String, ParsedMarkdown>,
) -> Result<(), Report<MarkdownError>> {
    let target_source = source_by_path.get(target_path).ok_or_else(|| {
        local_error(format!(
            "{} resolves to an unchecked target {target_path}",
            source.path
        ))
    })?;
    if source.set == LinkSourceSet::Public && target_source.set != LinkSourceSet::Public {
        return Err(local_error(format!(
            "public page {} links to excluded source {target_path}",
            source.path
        )));
    }
    if let Some(fragment) = fragment {
        if fragment.is_empty() {
            return Ok(());
        }
        let target = parsed_by_path
            .get(target_path)
            .ok_or_else(|| local_error(format!("target was not parsed: {target_path}")))?;
        if !target.anchors.contains(fragment) {
            return Err(local_error(format!(
                "{} has missing anchor #{fragment} in {target_path}",
                source.path
            )));
        }
    }
    Ok(())
}

fn parse_markdown(
    path: &str,
    set: LinkSourceSet,
    markdown: &str,
) -> Result<ParsedMarkdown, Report<MarkdownError>> {
    let mut options = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_FOOTNOTES;
    if set == LinkSourceSet::Public {
        options |= Options::ENABLE_HEADING_ATTRIBUTES;
    }
    let parser = Parser::new_ext(markdown, options).into_offset_iter();
    let mut anchors = BTreeSet::new();
    let mut heading_anchors = BTreeSet::new();
    let mut links = Vec::new();
    let mut mermaid_selectors = Vec::new();
    let mut heading: Option<HeadingState> = None;
    let mut slugs = HeadingSlugs::new(set);
    let mut fence: Option<FenceState> = None;

    for (event, range) in parser {
        match event {
            Event::Start(Tag::Heading {
                id, classes, attrs, ..
            }) => {
                if heading.is_some() {
                    return Err(local_error(format!("{path} has nested headings")));
                }
                if set == LinkSourceSet::Public && (!classes.is_empty() || !attrs.is_empty()) {
                    return Err(local_error(format!(
                        "{path} has an invalid explicit heading id or unsupported heading attributes"
                    )));
                }
                heading = Some(HeadingState {
                    text: String::new(),
                    explicit_id: id.map(pulldown_cmark::CowStr::into_string),
                    image_depth: 0,
                });
            }
            Event::End(TagEnd::Heading(_level)) => {
                let current = heading
                    .take()
                    .ok_or_else(|| local_error(format!("{path} closes an unopened heading")))?;
                if current.image_depth != 0 {
                    return Err(local_error(format!(
                        "{path} closes a heading inside an image"
                    )));
                }
                let slug = slugs.slug(path, &current.text, current.explicit_id.as_deref())?;
                anchors.insert(slug.clone());
                heading_anchors.insert(slug);
            }
            Event::Text(value) | Event::Code(value)
                if heading.as_ref().is_some_and(|current| {
                    set != LinkSourceSet::Public || current.image_depth == 0
                }) =>
            {
                heading
                    .as_mut()
                    .expect("heading should be present")
                    .text
                    .push_str(&value);
            }
            Event::SoftBreak | Event::HardBreak
                if heading.as_ref().is_some_and(|current| {
                    set != LinkSourceSet::Public || current.image_depth == 0
                }) =>
            {
                heading
                    .as_mut()
                    .expect("heading should be present")
                    .text
                    .push(' ');
            }
            Event::Start(Tag::Link { dest_url, .. }) => {
                links.push(ParsedLink {
                    destination: dest_url.into_string(),
                    start: range.start,
                    end: range.end,
                });
            }
            Event::Start(Tag::Image { dest_url, .. }) => {
                links.push(ParsedLink {
                    destination: dest_url.into_string(),
                    start: range.start,
                    end: range.end,
                });
                if set == LinkSourceSet::Public
                    && let Some(current) = &mut heading
                {
                    current.image_depth += 1;
                }
            }
            Event::End(TagEnd::Image) if set == LinkSourceSet::Public && heading.is_some() => {
                let current = heading.as_mut().expect("heading should be present");
                current.image_depth = current.image_depth.checked_sub(1).ok_or_else(|| {
                    local_error(format!("{path} closes an unopened heading image"))
                })?;
            }
            Event::Html(value) | Event::InlineHtml(value) => {
                parse_html_fragment(
                    path,
                    &value,
                    range.start,
                    range.end,
                    &mut anchors,
                    &mut links,
                )?;
            }
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(info))) => {
                let opening = parse_opening_fence(path, markdown, range.start, &info)?;
                if opening.mermaid {
                    mermaid_selectors.push(format!("mermaid:{}", mermaid_selectors.len() + 1));
                }
                fence = Some(opening);
            }
            Event::Start(Tag::CodeBlock(CodeBlockKind::Indented)) => {
                fence = None;
            }
            Event::End(TagEnd::CodeBlock) => {
                if let Some(opening) = fence.take() {
                    validate_closing_fence(path, markdown, &opening, range.start, range.end)?;
                }
            }
            Event::Text(_)
            | Event::Code(_)
            | Event::SoftBreak
            | Event::HardBreak
            | Event::Rule
            | Event::TaskListMarker(_)
            | Event::FootnoteReference(_)
            | Event::InlineMath(_)
            | Event::DisplayMath(_)
            | Event::Start(_)
            | Event::End(_) => {}
        }
    }
    if heading.is_some() || fence.is_some() {
        return Err(local_error(format!(
            "{path} has an unterminated semantic block"
        )));
    }
    Ok(ParsedMarkdown {
        anchors,
        heading_anchors,
        links,
        mermaid_selectors,
    })
}

struct HeadingState {
    text: String,
    explicit_id: Option<String>,
    image_depth: usize,
}

enum HeadingSlugs {
    Vitepress(UniqueSlugs),
    Github(GithubSlugger),
}

impl HeadingSlugs {
    fn new(set: LinkSourceSet) -> Self {
        if set == LinkSourceSet::Public {
            Self::Vitepress(UniqueSlugs::default())
        } else {
            Self::Github(GithubSlugger::default())
        }
    }

    fn slug(
        &mut self,
        path: &str,
        heading: &str,
        explicit_id: Option<&str>,
    ) -> Result<String, Report<MarkdownError>> {
        let slug = match self {
            Self::Vitepress(seen) => {
                if let Some(identifier) = explicit_id {
                    validate_explicit_heading_id(path, identifier)?;
                    seen.claim_explicit(path, identifier)?
                } else {
                    if heading.contains("{#") {
                        return Err(local_error(format!(
                            "{path} has an invalid explicit heading id"
                        )));
                    }
                    seen.unique(&vitepress_slugify(heading))
                }
            }
            Self::Github(slugger) => slugger.slug(heading),
        };
        if slug.is_empty() {
            return Err(local_error(format!("{path} has an empty heading anchor")));
        }
        Ok(slug)
    }
}

#[derive(Default)]
struct UniqueSlugs {
    seen: BTreeSet<String>,
}

impl UniqueSlugs {
    fn unique(&mut self, base: &str) -> String {
        let mut candidate = base.to_owned();
        let mut suffix = 1;
        while self.seen.contains(&candidate) {
            candidate = format!("{base}-{suffix}");
            suffix += 1;
        }
        self.seen.insert(candidate.clone());
        candidate
    }

    fn claim_explicit(
        &mut self,
        path: &str,
        identifier: &str,
    ) -> Result<String, Report<MarkdownError>> {
        if !self.seen.insert(identifier.to_owned()) {
            return Err(local_error(format!(
                "{path} has duplicate explicit heading id: {identifier}"
            )));
        }
        Ok(identifier.to_owned())
    }
}

fn validate_explicit_heading_id(path: &str, identifier: &str) -> Result<(), Report<MarkdownError>> {
    if identifier.is_empty()
        || identifier.len() > 128
        || identifier.chars().any(|character| {
            !(character.is_alphanumeric() || matches!(character, '-' | '_' | ':' | '.'))
        })
    {
        return Err(local_error(format!(
            "{path} has an invalid explicit heading id"
        )));
    }
    Ok(())
}

fn vitepress_slugify(value: &str) -> String {
    let mut output = String::new();
    let mut pending_separator = false;
    for character in value.nfkd() {
        if ('\u{0300}'..='\u{036f}').contains(&character)
            || ('\u{0000}'..='\u{001f}').contains(&character)
        {
            continue;
        }
        if vitepress_special(character) {
            pending_separator = !output.is_empty();
            continue;
        }
        if pending_separator {
            output.push('-');
            pending_separator = false;
        }
        for lowercase in character.to_lowercase() {
            output.push(lowercase);
        }
    }
    while output.ends_with('-') {
        output.pop();
    }
    if output.as_bytes().first().is_some_and(u8::is_ascii_digit) {
        output.insert(0, '_');
    }
    output
}

fn vitepress_special(character: char) -> bool {
    character.is_whitespace()
        || matches!(
            character,
            '~' | '`'
                | '!'
                | '@'
                | '#'
                | '$'
                | '%'
                | '^'
                | '&'
                | '*'
                | '('
                | ')'
                | '-'
                | '_'
                | '+'
                | '='
                | '['
                | ']'
                | '{'
                | '}'
                | '|'
                | '\\'
                | ';'
                | ':'
                | '"'
                | '\''
                | '“'
                | '”'
                | '‘'
                | '’'
                | '<'
                | '>'
                | ','
                | '.'
                | '?'
                | '/'
        )
}

fn parse_html_fragment(
    path: &str,
    raw: &str,
    start: usize,
    end: usize,
    anchors: &mut BTreeSet<String>,
    links: &mut Vec<ParsedLink>,
) -> Result<(), Report<MarkdownError>> {
    if raw.len() > MAXIMUM_LINK_BYTES || raw.chars().any(|character| character == '\0') {
        return Err(local_error(format!(
            "{path} has an oversized or unsafe HTML fragment at bytes {start}-{end}"
        )));
    }
    let document = Html::parse_fragment(raw);
    let selector =
        Selector::parse("*").map_err(|_error| local_error("internal HTML selector is invalid"))?;
    for element in document.select(&selector) {
        for name in ["id", "name"] {
            if let Some(value) = element.value().attr(name)
                && !value.is_empty()
            {
                anchors.insert(value.to_owned());
            }
        }
        if let Some(destination) = element.value().attr("href") {
            links.push(ParsedLink {
                destination: destination.to_owned(),
                start,
                end,
            });
        }
    }
    Ok(())
}

struct FenceState {
    character: u8,
    count: usize,
    mermaid: bool,
}

fn parse_opening_fence(
    path: &str,
    markdown: &str,
    start: usize,
    info: &str,
) -> Result<FenceState, Report<MarkdownError>> {
    let line = markdown[start..]
        .lines()
        .next()
        .ok_or_else(|| local_error(format!("{path} has an empty fenced block")))?;
    let trimmed = line.trim_start_matches(' ');
    if line.len() - trimmed.len() > 3 {
        return Err(local_error(format!("{path} has an invalid fenced block")));
    }
    let character = *trimmed
        .as_bytes()
        .first()
        .ok_or_else(|| local_error(format!("{path} has an empty fence")))?;
    let count = trimmed
        .as_bytes()
        .iter()
        .take_while(|byte| **byte == character)
        .count();
    if !matches!(character, b'`' | b'~') || count < 3 {
        return Err(local_error(format!("{path} has an invalid opening fence")));
    }
    let mermaid = validate_mermaid_info(path, info)?;
    Ok(FenceState {
        character,
        count,
        mermaid,
    })
}

fn validate_mermaid_info(path: &str, info: &str) -> Result<bool, Report<MarkdownError>> {
    let trimmed = info.trim();
    let first = trimmed.split_whitespace().next().unwrap_or_default();
    if first != "mermaid" {
        if first.starts_with("mermaid") {
            return Err(local_error(format!(
                "{path} has invalid mermaid fence info: {trimmed}"
            )));
        }
        return Ok(false);
    }
    let remainder = trimmed[first.len()..].trim();
    let valid = remainder.is_empty()
        || (remainder.starts_with('{')
            && remainder.ends_with('}')
            && valid_mermaid_attributes(&remainder[1..remainder.len() - 1]));
    if !valid {
        return Err(local_error(format!(
            "{path} has invalid mermaid fence attributes: {remainder}"
        )));
    }
    Ok(true)
}

fn valid_mermaid_attributes(attributes: &str) -> bool {
    let mut found = false;
    for attribute in attributes.split_ascii_whitespace() {
        found = true;
        let valid_identifier = |value: &str| {
            !value.is_empty()
                && value.chars().all(|character| {
                    character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | ':' | '.')
                })
        };
        let valid = attribute
            .strip_prefix('#')
            .or_else(|| attribute.strip_prefix('.'))
            .is_some_and(valid_identifier)
            || attribute
                .split_once('=')
                .is_some_and(|(name, value)| valid_identifier(name) && valid_identifier(value));
        if !valid {
            return false;
        }
    }
    found
}

fn validate_closing_fence(
    path: &str,
    markdown: &str,
    opening: &FenceState,
    start: usize,
    end: usize,
) -> Result<(), Report<MarkdownError>> {
    let raw = markdown
        .get(start..end)
        .ok_or_else(|| local_error(format!("{path} has invalid fence offsets")))?;
    let line = raw
        .trim_end_matches(['\r', '\n'])
        .lines()
        .last()
        .unwrap_or_default()
        .trim_start_matches(' ');
    let count = line
        .as_bytes()
        .iter()
        .take_while(|byte| **byte == opening.character)
        .count();
    if count < opening.count
        || line[count..]
            .chars()
            .any(|character| !character.is_ascii_whitespace())
    {
        return Err(local_error(format!(
            "{path} has a mismatched or unterminated fenced block at bytes {start}-{end}: {raw:?}"
        )));
    }
    Ok(())
}

fn strict_percent_decode(value: &str, source: &str) -> Result<String, Report<MarkdownError>> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let high = bytes
                .get(index + 1)
                .and_then(|byte| hexadecimal(*byte))
                .ok_or_else(|| {
                    local_error(format!("{source} has invalid percent encoding: {value}"))
                })?;
            let low = bytes
                .get(index + 2)
                .and_then(|byte| hexadecimal(*byte))
                .ok_or_else(|| {
                    local_error(format!("{source} has invalid percent encoding: {value}"))
                })?;
            decoded.push(high * 16 + low);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded)
        .map_err(|_error| local_error(format!("{source} percent-decodes to invalid UTF-8")))
        .and_then(|decoded| {
            let bytes = decoded.as_bytes();
            if bytes.windows(3).any(|window| {
                window[0] == b'%'
                    && hexadecimal(window[1]).is_some()
                    && hexadecimal(window[2]).is_some()
            }) {
                Err(local_error(format!(
                    "{source} has residual percent encoding after one decode"
                )))
            } else {
                Ok(decoded)
            }
        })
}

const fn hexadecimal(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn is_external_or_non_file(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    [
        "https://",
        "http://",
        "mailto:",
        "tel:",
        "data:",
        "javascript:",
    ]
    .iter()
    .any(|prefix| lower.starts_with(prefix))
}

fn validate_repo_path(path: &str) -> Result<(), Report<MarkdownError>> {
    if path.is_empty()
        || path.starts_with('/')
        || path.contains('\\')
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(local_error(format!("unsafe repository path: {path}")));
    }
    Ok(())
}

fn resolve_relative_path(source: &str, relative: &str) -> Result<String, Report<MarkdownError>> {
    let mut components = source
        .rsplit_once('/')
        .map_or(Vec::new(), |(parent, _file)| {
            parent.split('/').map(str::to_owned).collect()
        });
    for component in relative.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                if components.pop().is_none() {
                    return Err(local_error(format!(
                        "relative link escapes repository: {relative}"
                    )));
                }
            }
            value => components.push(value.to_owned()),
        }
    }
    if components.is_empty() {
        return Err(local_error(format!(
            "relative link has no target: {relative}"
        )));
    }
    Ok(components.join("/"))
}

fn public_route(path: &str) -> Result<String, Report<MarkdownError>> {
    let Some(relative) = path.strip_prefix("docs/") else {
        return Err(local_error(format!("public path is outside docs: {path}")));
    };
    if relative == "index.md" {
        return Ok("/".to_owned());
    }
    let without_extension = relative
        .strip_suffix(".md")
        .ok_or_else(|| local_error(format!("public source is not Markdown: {path}")))?;
    if let Some(parent) = without_extension.strip_suffix("/index") {
        Ok(format!("/{parent}/"))
    } else {
        Ok(format!("/{without_extension}"))
    }
}

fn normalize_route(route: &str) -> Result<String, Report<MarkdownError>> {
    if !route.starts_with('/') || route.contains('\\') {
        return Err(local_error(format!("invalid VitePress route: {route}")));
    }
    let mut components = Vec::new();
    for component in route.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                if components.pop().is_none() {
                    return Err(local_error(format!("route escapes site root: {route}")));
                }
            }
            value => components.push(value),
        }
    }
    let mut result = format!("/{}", components.join("/"));
    if route.ends_with('/') && result != "/" {
        result.push('/');
    }
    if let Some(stripped) = result.strip_suffix(".md") {
        result = stripped.to_owned();
    }
    if let Some(stripped) = result.strip_suffix(".html") {
        result = stripped.to_owned();
    }
    Ok(result)
}

fn route_for_relative_target(path: &str) -> Result<String, Report<MarkdownError>> {
    if path.starts_with("docs/") {
        let candidate = if path.ends_with(".md") {
            path.to_owned()
        } else {
            format!("{path}.md")
        };
        public_route(&candidate)
    } else {
        Err(local_error(format!(
            "relative route is outside public docs: {path}"
        )))
    }
}

fn excluded_public_target<'a>(
    raw_path: &str,
    route: &str,
    excluded_paths: &'a BTreeSet<String>,
) -> Option<&'a str> {
    let repository_path = raw_path.trim_start_matches('/');
    excluded_paths.iter().find_map(|path| {
        if path == repository_path
            || public_route(path).is_ok_and(|excluded_route| excluded_route == route)
        {
            Some(path.as_str())
        } else {
            None
        }
    })
}

fn local_error(detail: impl Into<String>) -> Report<MarkdownError> {
    Report::new(MarkdownError::LocalLink {
        detail: detail.into(),
    })
}

/// Validate the checked repository's active Markdown sets and publication records.
///
/// # Errors
///
/// Returns an error when classification is incomplete, a source cannot be
/// read safely, local links fail, or page/navigation/orphan/diagram manifests
/// differ from the repository and `VitePress` configuration.
pub(crate) fn check_local_repository(repository: &Repository) -> Result<(), Report<MarkdownError>> {
    let pages = read_pages_manifest(repository)?;
    let orphans: OrphansManifest = read_toml_manifest(repository, ORPHANS_MANIFEST)?;
    let diagrams: DiagramsManifest = read_toml_manifest(repository, DIAGRAMS_MANIFEST)?;
    validate_manifest_attestation(orphans.version, orphans.reviewed, "orphans")?;
    validate_manifest_attestation(diagrams.version, diagrams.reviewed, "diagrams")?;

    let intended_paths = live_pages(&pages.pages)
        .map(|page| page.path.clone())
        .collect::<BTreeSet<_>>();
    let loaded = load_link_sources(repository, &intended_paths)?;

    let config_path = NormalizedRelativePath::new(Path::new(&pages.vitepress_config))
        .change_context(MarkdownError::LocalLink {
            detail: "unsafe VitePress configuration path".to_owned(),
        })?;
    let config_bytes = repository
        .read_optional(&config_path)
        .change_context(MarkdownError::LocalLink {
            detail: "cannot safely read VitePress configuration".to_owned(),
        })?
        .ok_or_else(|| local_error("VitePress configuration is missing"))?;
    let config_text = core::str::from_utf8(&config_bytes)
        .map_err(|_error| local_error("VitePress configuration is not UTF-8"))?;
    let vitepress = parse_vitepress_config(config_text)?;
    let page_inventory = validate_page_inventory(repository, &pages, &vitepress)?;
    let orphan_inventory = validate_orphan_records(&orphans)?;
    if page_inventory.tombstones != orphan_inventory.tombstones {
        return Err(local_error(format!(
            "tombstone inventory mismatch; pages-only={:?}, orphans-only={:?}",
            page_inventory
                .tombstones
                .iter()
                .find(|entry| !orphan_inventory.tombstones.contains(*entry)),
            orphan_inventory
                .tombstones
                .iter()
                .find(|entry| !page_inventory.tombstones.contains(*entry))
        )));
    }
    let intended = page_inventory
        .live
        .iter()
        .map(|page| page.path.clone())
        .collect::<Vec<_>>();
    check_local_links_with_known(
        &loaded.sources,
        &intended,
        &page_inventory
            .tombstones
            .iter()
            .map(|(route, _replacement)| route.clone())
            .collect::<Vec<_>>(),
        &loaded.excluded_paths,
        &loaded.known_paths,
    )?;
    validate_reachability(
        &loaded.sources,
        &page_inventory.live,
        &orphan_inventory.manual,
    )?;
    validate_diagrams(&loaded.sources, &diagrams)?;
    Ok(())
}

struct LoadedLinkSources {
    sources: Vec<LinkSource>,
    excluded_paths: BTreeSet<String>,
    known_paths: BTreeSet<String>,
}

fn load_link_sources(
    repository: &Repository,
    intended_paths: &BTreeSet<String>,
) -> Result<LoadedLinkSources, Report<MarkdownError>> {
    let classification = crate::classification::checked_markdown_sources(repository)
        .change_context(MarkdownError::LocalLink {
            detail: "Markdown source classification is incomplete".to_owned(),
        })?;
    let mut sources = Vec::new();
    for path_text in &classification.included_paths {
        let path = NormalizedRelativePath::new(Path::new(path_text)).change_context(
            MarkdownError::LocalLink {
                detail: format!("unsafe Markdown source path: {path_text}"),
            },
        )?;
        let bytes = repository
            .read_optional(&path)
            .change_context(MarkdownError::LocalLink {
                detail: format!("cannot safely read Markdown source: {path_text}"),
            })?
            .ok_or_else(|| local_error(format!("Markdown source is missing: {path_text}")))?;
        if bytes.len() > MAXIMUM_DOCUMENT_BYTES {
            return Err(local_error(format!(
                "Markdown source exceeds {MAXIMUM_DOCUMENT_BYTES} bytes: {path_text}"
            )));
        }
        let markdown = String::from_utf8(bytes)
            .map_err(|_error| local_error(format!("Markdown source is not UTF-8: {path_text}")))?;
        let set = if intended_paths.contains(path_text) {
            LinkSourceSet::Public
        } else if path_text.starts_with("docs/internal/") {
            LinkSourceSet::MaintainedInternal
        } else {
            LinkSourceSet::Repository
        };
        sources.push(LinkSource {
            path: path_text.clone(),
            set,
            markdown,
        });
    }
    sources.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(LoadedLinkSources {
        sources,
        excluded_paths: classification.excluded_paths,
        known_paths: classification.known_paths,
    })
}

fn validate_manifest_attestation(
    version: u32,
    reviewed: bool,
    name: &str,
) -> Result<(), Report<MarkdownError>> {
    if version != MANIFEST_VERSION {
        return Err(local_error(format!(
            "{name} manifest version must be {MANIFEST_VERSION}"
        )));
    }
    if !reviewed {
        return Err(local_error(format!(
            "{name} manifest must be explicitly reviewed"
        )));
    }
    Ok(())
}

struct VitepressRecords {
    src_excludes: BTreeSet<String>,
    navigation_routes: BTreeSet<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct LivePage {
    path: String,
    route: String,
    navigation: bool,
}

struct PageInventory {
    live: Vec<LivePage>,
    tombstones: BTreeSet<(String, String)>,
}

struct OrphanInventory {
    manual: BTreeSet<String>,
    tombstones: BTreeSet<(String, String)>,
}

fn live_pages(records: &[PageRecord]) -> impl Iterator<Item = LivePage> + '_ {
    records.iter().filter_map(|record| match record {
        PageRecord::Live {
            path,
            route,
            navigation,
        } => Some(LivePage {
            path: path.clone(),
            route: route.clone(),
            navigation: *navigation,
        }),
        PageRecord::Tombstone { .. } => None,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum TypeScriptToken {
    Identifier(String),
    String(String),
    Punctuation(char),
}

fn parse_vitepress_config(source: &str) -> Result<VitepressRecords, Report<MarkdownError>> {
    let tokens = lex_typescript(source)?;
    let mut src_excludes = None;
    let mut navigation_routes = BTreeSet::new();
    let mut index = 0;
    while index < tokens.len() {
        match tokens.get(index) {
            Some(TypeScriptToken::Identifier(name)) if name == "srcExclude" => {
                if src_excludes.is_some() {
                    return Err(local_error("VitePress srcExclude is declared twice"));
                }
                if tokens.get(index + 1) != Some(&TypeScriptToken::Punctuation(':'))
                    || tokens.get(index + 2) != Some(&TypeScriptToken::Punctuation('['))
                {
                    return Err(local_error("VitePress srcExclude has an unknown shape"));
                }
                index += 3;
                let mut values = BTreeSet::new();
                loop {
                    match tokens.get(index) {
                        Some(TypeScriptToken::Punctuation(']')) => break,
                        Some(TypeScriptToken::String(value)) => {
                            if !values.insert(value.clone()) {
                                return Err(local_error(format!(
                                    "duplicate VitePress srcExclude value: {value}"
                                )));
                            }
                            index += 1;
                            if tokens.get(index) == Some(&TypeScriptToken::Punctuation(',')) {
                                index += 1;
                            }
                        }
                        _ => {
                            return Err(local_error(
                                "VitePress srcExclude contains an unknown expression",
                            ));
                        }
                    }
                }
                src_excludes = Some(values);
            }
            Some(TypeScriptToken::Identifier(name))
                if name == "link"
                    && tokens.get(index + 1) == Some(&TypeScriptToken::Punctuation(':')) =>
            {
                let Some(TypeScriptToken::String(value)) = tokens.get(index + 2) else {
                    return Err(local_error("VitePress link has a nonliteral value"));
                };
                if value.starts_with('/') {
                    navigation_routes.insert(normalize_route(value)?);
                }
            }
            _ => {}
        }
        index += 1;
    }
    Ok(VitepressRecords {
        src_excludes: src_excludes.ok_or_else(|| local_error("VitePress srcExclude is missing"))?,
        navigation_routes,
    })
}

fn lex_typescript(source: &str) -> Result<Vec<TypeScriptToken>, Report<MarkdownError>> {
    if source.len() > MAXIMUM_DOCUMENT_BYTES {
        return Err(local_error("VitePress configuration exceeds size bound"));
    }
    let bytes = source.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index].is_ascii_whitespace() {
            index += 1;
        } else if bytes[index] == b'/' && bytes.get(index + 1) == Some(&b'/') {
            index += 2;
            while bytes.get(index).is_some_and(|byte| *byte != b'\n') {
                index += 1;
            }
        } else if bytes[index] == b'/' && bytes.get(index + 1) == Some(&b'*') {
            let Some(end) = source[index + 2..].find("*/") else {
                return Err(local_error("unclosed TypeScript block comment"));
            };
            index += end + 4;
        } else if matches!(bytes[index], b'\'' | b'"') {
            let (value, end) = parse_typescript_string(source, index, bytes[index])?;
            tokens.push(TypeScriptToken::String(value));
            index = end;
        } else if bytes[index] == b'`' {
            index = skip_typescript_template(source, index)?;
        } else if bytes[index].is_ascii_alphabetic() || matches!(bytes[index], b'_' | b'$') {
            let start = index;
            index += 1;
            while bytes
                .get(index)
                .is_some_and(|byte| byte.is_ascii_alphanumeric() || matches!(*byte, b'_' | b'$'))
            {
                index += 1;
            }
            tokens.push(TypeScriptToken::Identifier(source[start..index].to_owned()));
        } else {
            tokens.push(TypeScriptToken::Punctuation(char::from(bytes[index])));
            index += 1;
        }
    }
    Ok(tokens)
}

fn parse_typescript_string(
    source: &str,
    start: usize,
    quote: u8,
) -> Result<(String, usize), Report<MarkdownError>> {
    let bytes = source.as_bytes();
    let mut output = String::new();
    let mut index = start + 1;
    while index < bytes.len() {
        if bytes[index] == quote {
            return Ok((output, index + 1));
        }
        if bytes[index] == b'\\' {
            index += 1;
            let escaped = *bytes
                .get(index)
                .ok_or_else(|| local_error("truncated TypeScript string escape"))?;
            let value = match escaped {
                b'\\' | b'\'' | b'"' | b'/' => char::from(escaped),
                b'n' => '\n',
                b'r' => '\r',
                b't' => '\t',
                _ => return Err(local_error("unknown TypeScript string escape")),
            };
            output.push(value);
            index += 1;
        } else {
            let character = source[index..]
                .chars()
                .next()
                .ok_or_else(|| local_error("invalid TypeScript string"))?;
            if character.is_control() {
                return Err(local_error("control character in TypeScript string"));
            }
            output.push(character);
            index += character.len_utf8();
        }
    }
    Err(local_error("unclosed TypeScript string"))
}

fn skip_typescript_template(source: &str, start: usize) -> Result<usize, Report<MarkdownError>> {
    let bytes = source.as_bytes();
    let mut index = start + 1;
    while index < bytes.len() {
        if bytes[index] == b'\\' {
            index += 2;
        } else if bytes[index] == b'`' {
            return Ok(index + 1);
        } else {
            index += 1;
        }
    }
    Err(local_error("unclosed TypeScript template string"))
}

fn validate_page_inventory(
    repository: &Repository,
    manifest: &PagesManifest,
    vitepress: &VitepressRecords,
) -> Result<PageInventory, Report<MarkdownError>> {
    let mut manifest_paths = BTreeSet::new();
    let mut manifest_routes = BTreeSet::new();
    let mut manifest_navigation = BTreeSet::new();
    let live = live_pages(&manifest.pages).collect::<Vec<_>>();
    for page in &live {
        validate_repo_path(&page.path)?;
        let expected_route = public_route(&page.path)?;
        let route = normalize_route(&page.route)?;
        if route != expected_route {
            return Err(local_error(format!(
                "page {} route {route} does not match {expected_route}",
                page.path
            )));
        }
        if !manifest_paths.insert(page.path.clone()) {
            return Err(local_error(format!("duplicate page path: {}", page.path)));
        }
        if !manifest_routes.insert(route.clone()) {
            return Err(local_error(format!("duplicate page route: {route}")));
        }
        if page.navigation {
            manifest_navigation.insert(route);
        }
    }
    let mut tombstone_routes = BTreeSet::new();
    let mut tombstones = BTreeSet::new();
    for page in &manifest.pages {
        let PageRecord::Tombstone { route, replacement } = page else {
            continue;
        };
        let route = normalize_route(route)?;
        let replacement = normalize_route(replacement)?;
        if manifest_routes.contains(&route) {
            return Err(local_error(format!(
                "tombstone route collides with live page: {route}"
            )));
        }
        if !manifest_routes.contains(&replacement) {
            return Err(local_error(format!(
                "tombstone {route} replacement is not a live route: {replacement}"
            )));
        }
        if !tombstone_routes.insert(route.clone()) {
            return Err(local_error(format!("duplicate tombstone page: {route}")));
        }
        tombstones.insert((route, replacement));
    }
    let expected_navigation = vitepress
        .navigation_routes
        .iter()
        .map(|route| normalize_route(route))
        .collect::<Result<BTreeSet<_>, _>>()?;
    if manifest_navigation != expected_navigation {
        return Err(local_error(format!(
            "navigation inventory mismatch; manifest-only={:?}, config-only={:?}",
            manifest_navigation.difference(&expected_navigation).next(),
            expected_navigation.difference(&manifest_navigation).next()
        )));
    }

    let root_prefix = format!("{}/", manifest.site_root);
    let mut built_paths = BTreeSet::new();
    for path in repository
        .tracked_paths()
        .change_context(MarkdownError::LocalLink {
            detail: "cannot enumerate tracked VitePress sources".to_owned(),
        })?
    {
        let text = path.as_utf8().change_context(MarkdownError::LocalLink {
            detail: "tracked VitePress path is not UTF-8".to_owned(),
        })?;
        let Some(relative) = text.strip_prefix(&root_prefix) else {
            continue;
        };
        if !relative.ends_with(".md") {
            continue;
        }
        if !vitepress
            .src_excludes
            .iter()
            .any(|pattern| matches_src_exclude(relative, pattern))
        {
            built_paths.insert(text.to_owned());
        }
    }
    if built_paths != manifest_paths {
        return Err(local_error(format!(
            "built page inventory mismatch; unlisted={:?}, stale={:?}",
            built_paths.difference(&manifest_paths).next(),
            manifest_paths.difference(&built_paths).next()
        )));
    }
    Ok(PageInventory { live, tombstones })
}

fn matches_src_exclude(path: &str, pattern: &str) -> bool {
    pattern
        .strip_suffix("/**")
        .is_some_and(|prefix| path == prefix || path.starts_with(&format!("{prefix}/")))
        || path == pattern
}

fn validate_orphan_records(
    manifest: &OrphansManifest,
) -> Result<OrphanInventory, Report<MarkdownError>> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_error| local_error("system clock precedes Unix epoch"))?
        .as_secs();
    let mut manual = BTreeSet::new();
    let mut tombstone_routes = BTreeSet::new();
    let mut tombstones = BTreeSet::new();
    for record in &manifest.exceptions {
        if record.owner.trim().is_empty() || record.reason.trim().is_empty() {
            return Err(local_error("orphan exception requires owner and reason"));
        }
        let expiry = timestamp_seconds(&record.expires_at)
            .ok_or_else(|| local_error("orphan exception has invalid expiry"))?;
        if now >= expiry {
            return Err(local_error("orphan exception is expired"));
        }
        match record.kind {
            OrphanKind::Manual => {
                let path = record
                    .path
                    .as_ref()
                    .ok_or_else(|| local_error("manual orphan requires path"))?;
                validate_repo_path(path)?;
                if record.route.is_some() || record.replacement.is_some() {
                    return Err(local_error(
                        "manual orphan cannot carry route or replacement",
                    ));
                }
                if !manual.insert(path.clone()) {
                    return Err(local_error(format!("duplicate manual orphan: {path}")));
                }
            }
            OrphanKind::Tombstone => {
                let route = record
                    .route
                    .as_ref()
                    .ok_or_else(|| local_error("tombstone requires route"))?;
                let route = normalize_route(route)?;
                if record.path.is_some() {
                    return Err(local_error("tombstone cannot carry a path"));
                }
                let replacement = record
                    .replacement
                    .as_ref()
                    .ok_or_else(|| local_error("tombstone requires replacement"))?;
                let replacement = normalize_route(replacement)?;
                if !tombstone_routes.insert(route.clone()) {
                    return Err(local_error(format!("duplicate tombstone: {route}")));
                }
                tombstones.insert((route, replacement));
            }
        }
    }
    Ok(OrphanInventory { manual, tombstones })
}

fn validate_reachability(
    sources: &[LinkSource],
    pages: &[LivePage],
    manual_orphans: &BTreeSet<String>,
) -> Result<(), Report<MarkdownError>> {
    let public = sources
        .iter()
        .filter(|source| source.set == LinkSourceSet::Public)
        .map(|source| (source.path.clone(), source))
        .collect::<BTreeMap<_, _>>();
    let route_to_path = pages
        .iter()
        .map(|page| (page.route.clone(), page.path.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut reachable = pages
        .iter()
        .filter(|page| page.navigation)
        .map(|page| page.path.clone())
        .collect::<BTreeSet<_>>();
    let mut queue = reachable.iter().cloned().collect::<VecDeque<_>>();
    while let Some(path) = queue.pop_front() {
        let source = public
            .get(&path)
            .ok_or_else(|| local_error(format!("navigation page is not public: {path}")))?;
        let parsed = parse_markdown(&path, source.set, &source.markdown)?;
        for destination in parsed.links {
            if let Some(target) =
                public_link_target(&path, &destination.destination, &route_to_path)?
                && reachable.insert(target.clone())
            {
                queue.push_back(target);
            }
        }
    }
    let actual_orphans = public
        .keys()
        .filter(|path| !reachable.contains(*path))
        .cloned()
        .collect::<BTreeSet<_>>();
    if &actual_orphans != manual_orphans {
        return Err(local_error(format!(
            "orphan inventory mismatch; unlisted={:?}, stale={:?}",
            actual_orphans.difference(manual_orphans).next(),
            manual_orphans.difference(&actual_orphans).next()
        )));
    }
    Ok(())
}

fn public_link_target(
    source_path: &str,
    destination: &str,
    route_to_path: &BTreeMap<String, String>,
) -> Result<Option<String>, Report<MarkdownError>> {
    if is_external_or_non_file(destination) || destination.starts_with('#') {
        return Ok(None);
    }
    let raw = destination
        .split_once('#')
        .map_or(destination, |(path, _fragment)| path)
        .split_once('?')
        .map_or_else(
            || {
                destination
                    .split_once('#')
                    .map_or(destination, |(path, _)| path)
            },
            |(path, _)| path,
        );
    let decoded = strict_percent_decode(raw, source_path)?;
    if decoded.is_empty() {
        return Ok(None);
    }
    if decoded.starts_with('/') {
        return Ok(route_to_path.get(&normalize_route(&decoded)?).cloned());
    }
    let relative = resolve_relative_path(source_path, &decoded)?;
    if route_to_path.values().any(|path| path == &relative) {
        Ok(Some(relative))
    } else {
        let route = route_for_relative_target(&relative)?;
        Ok(route_to_path.get(&route).cloned())
    }
}

fn validate_diagrams(
    sources: &[LinkSource],
    manifest: &DiagramsManifest,
) -> Result<(), Report<MarkdownError>> {
    let public = sources
        .iter()
        .filter(|source| source.set == LinkSourceSet::Public)
        .map(|source| (source.path.clone(), source))
        .collect::<BTreeMap<_, _>>();
    let mut actual = BTreeSet::new();
    for source in public.values() {
        let parsed = parse_markdown(&source.path, source.set, &source.markdown)?;
        for selector in parsed.mermaid_selectors {
            actual.insert((source.path.clone(), selector));
        }
    }
    let mut recorded = BTreeSet::new();
    for record in &manifest.diagrams {
        validate_repo_path(&record.path)?;
        if record.owner.trim().is_empty() || record.prose_anchor.trim().is_empty() {
            return Err(local_error(format!(
                "diagram {} {} requires prose anchor and owner",
                record.path, record.selector
            )));
        }
        let source = public
            .get(&record.path)
            .ok_or_else(|| local_error(format!("diagram source is not public: {}", record.path)))?;
        let parsed = parse_markdown(&record.path, source.set, &source.markdown)?;
        if !parsed.heading_anchors.contains(&record.prose_anchor) {
            return Err(local_error(format!(
                "diagram {} {} has missing prose heading #{}",
                record.path, record.selector, record.prose_anchor
            )));
        }
        if !recorded.insert((record.path.clone(), record.selector.clone())) {
            return Err(local_error(format!(
                "duplicate diagram record: {} {}",
                record.path, record.selector
            )));
        }
    }
    if actual != recorded {
        return Err(local_error(format!(
            "diagram inventory mismatch; unlisted={:?}, stale={:?}",
            actual.difference(&recorded).next(),
            recorded.difference(&actual).next()
        )));
    }
    Ok(())
}

/// Exact external-link exception with mandatory ownership and expiry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExternalException {
    /// Exact canonical URL skipped by the transport.
    pub url: String,
    /// Non-empty accountable owner.
    pub owner: String,
    /// Non-empty bounded reason.
    pub reason: String,
    /// Canonical UTC expiry timestamp.
    pub expires_at: String,
}

/// One bounded external transport request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExternalRequest {
    /// `HEAD` or fallback `GET`.
    pub method: String,
    /// Exact HTTPS URL.
    pub url: String,
    /// Per-attempt timeout.
    pub timeout_seconds: u64,
    /// Maximum response body bytes accepted by the transport.
    pub maximum_body_bytes: usize,
}

/// Bounded external response metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExternalResponse {
    /// HTTP status code.
    pub status: u16,
    /// Lowercase response headers. Transport implementations must reject
    /// duplicate security-relevant headers and oversized header sections.
    pub headers: BTreeMap<String, String>,
    /// Exact number of header bytes read by the transport.
    pub header_bytes: usize,
    /// Exact number of body bytes read by the transport.
    pub body_bytes: usize,
}

/// Injected external HTTP transport.
pub trait ExternalTransport {
    /// Send one request without following redirects or retrying.
    ///
    /// # Errors
    ///
    /// Returns a bounded diagnostic when the request cannot be completed.
    fn send(&mut self, request: &ExternalRequest) -> Result<ExternalResponse, String>;
}

/// Injected bounded sleeper used by retry tests and production execution.
pub trait Sleeper {
    /// Sleep for the supplied whole-second delay.
    fn sleep_seconds(&mut self, seconds: u64);
}

/// Run the explicitly requested production external-link check.
///
/// This is the only documentation-parity path that performs network I/O.
/// Local and generated checks never construct the production transport.
///
/// # Errors
///
/// Returns an error for incomplete source classification, malformed page or
/// exception governance, system-clock failure, unavailable bounded transport,
/// or any external-link failure.
pub(crate) fn check_external_repository(
    repository: &Repository,
) -> Result<(), Report<MarkdownError>> {
    let pages = read_pages_manifest(repository)?;
    let intended = live_pages(&pages.pages)
        .map(|page| page.path)
        .collect::<BTreeSet<_>>();
    let loaded = load_link_sources(repository, &intended)?;
    let exceptions = pages
        .external_exceptions
        .into_iter()
        .map(|record| ExternalException {
            url: record.url,
            owner: record.owner,
            reason: record.reason,
            expires_at: record.expires_at,
        })
        .collect::<Vec<_>>();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_error| external_error("system clock precedes Unix epoch"))?
        .as_secs();
    let mut transport = CurlTransport::production();
    let mut sleeper = ThreadSleeper;
    check_external_links(
        &loaded.sources,
        &exceptions,
        now,
        &mut transport,
        &mut sleeper,
    )
}

struct ThreadSleeper;

impl Sleeper for ThreadSleeper {
    fn sleep_seconds(&mut self, seconds: u64) {
        std::thread::sleep(Duration::from_secs(seconds));
    }
}

/// Bounded result returned by an injected external command runner.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandOutput {
    /// Whether the process exited successfully.
    pub success: bool,
    /// Platform exit code when one was available.
    pub status_code: Option<i32>,
    /// Complete stdout, bounded during the read by [`CommandRunner`].
    pub stdout: Vec<u8>,
}

/// Injectable runner used by [`CurlTransport`].
pub trait CommandRunner {
    /// Run one command and cap stdout while it is being read.
    ///
    /// # Errors
    ///
    /// Returns an error when spawning, reading, waiting, or the byte cap fails.
    fn run(
        &mut self,
        program: &str,
        arguments: &[String],
        maximum_output_bytes: usize,
    ) -> Result<CommandOutput, String>;
}

/// Production process runner for the bounded curl transport.
pub struct ProcessCommandRunner;

impl CommandRunner for ProcessCommandRunner {
    fn run(
        &mut self,
        program: &str,
        arguments: &[String],
        maximum_output_bytes: usize,
    ) -> Result<CommandOutput, String> {
        let mut child = Command::new(program)
            .args(arguments)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("cannot start {program}: {error}"))?;
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| format!("{program} stdout pipe is unavailable"))?;
        let mut bytes = Vec::new();
        stdout
            .by_ref()
            .take((maximum_output_bytes + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|error| format!("cannot read {program} stdout: {error}"))?;
        if bytes.len() > maximum_output_bytes {
            let _kill_result = child.kill();
            let _wait_result = child.wait();
            return Err(format!(
                "{program} stdout exceeds {maximum_output_bytes} bytes"
            ));
        }
        let status = child
            .wait()
            .map_err(|error| format!("cannot wait for {program}: {error}"))?;
        Ok(CommandOutput {
            success: status.success(),
            status_code: status.code(),
            stdout: bytes,
        })
    }
}

/// HTTPS-only, non-redirecting curl transport with an injectable command seam.
pub struct CurlTransport<R = ProcessCommandRunner> {
    runner: R,
}

impl CurlTransport<ProcessCommandRunner> {
    fn production() -> Self {
        Self {
            runner: ProcessCommandRunner,
        }
    }
}

impl<R> CurlTransport<R> {
    /// Create a transport from an injected command runner.
    #[must_use]
    pub const fn new(runner: R) -> Self {
        Self { runner }
    }

    /// Consume the transport and return its runner.
    #[must_use]
    pub fn into_runner(self) -> R {
        self.runner
    }
}

impl<R: CommandRunner> ExternalTransport for CurlTransport<R> {
    fn send(&mut self, request: &ExternalRequest) -> Result<ExternalResponse, String> {
        validate_transport_request(request)?;
        let mut arguments = [
            "--disable".to_owned(),
            "--silent".to_owned(),
            "--show-error".to_owned(),
            "--proto".to_owned(),
            "=https".to_owned(),
            "--proto-redir".to_owned(),
            "=https".to_owned(),
            "--max-redirs".to_owned(),
            "0".to_owned(),
            "--connect-timeout".to_owned(),
            "5".to_owned(),
            "--max-time".to_owned(),
            request.timeout_seconds.to_string(),
            "--max-filesize".to_owned(),
            request.maximum_body_bytes.to_string(),
            "--dump-header".to_owned(),
            "-".to_owned(),
            "--output".to_owned(),
            "-".to_owned(),
            "--write-out".to_owned(),
            CURL_WRITE_OUT.to_owned(),
        ]
        .to_vec();
        if request.method == "HEAD" {
            arguments.push("--head".to_owned());
        } else {
            arguments.extend(["--request".to_owned(), "GET".to_owned()]);
        }
        arguments.extend(["--url".to_owned(), request.url.clone()]);
        let maximum_output =
            MAXIMUM_RESPONSE_HEADER_BYTES + request.maximum_body_bytes + CURL_TRAILER_BYTES;
        let output = self.runner.run("curl", &arguments, maximum_output)?;
        if !output.success {
            return Err(format!(
                "curl exited unsuccessfully with status {:?}",
                output.status_code
            ));
        }
        parse_curl_output(&output.stdout, request.maximum_body_bytes)
    }
}

fn validate_transport_request(request: &ExternalRequest) -> Result<(), String> {
    if !matches!(request.method.as_str(), "HEAD" | "GET") {
        return Err(format!("unsupported transport method: {}", request.method));
    }
    if !(1..=30).contains(&request.timeout_seconds) {
        return Err("transport timeout is outside 1..=30 seconds".to_owned());
    }
    if request.maximum_body_bytes == 0 || request.maximum_body_bytes > MAXIMUM_RESPONSE_BODY_BYTES {
        return Err("transport body bound is outside 1..=65536 bytes".to_owned());
    }
    let url = Url::parse(&request.url).map_err(|_error| "transport URL is malformed".to_owned())?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("transport URL must be credential-free HTTPS".to_owned());
    }
    Ok(())
}

fn parse_curl_output(bytes: &[u8], maximum_body_bytes: usize) -> Result<ExternalResponse, String> {
    let marker = b"\nDOCS_PARITY_COUNTS:";
    let marker_start = bytes
        .windows(marker.len())
        .rposition(|window| window == marker)
        .ok_or_else(|| "curl output has no byte-count trailer".to_owned())?;
    let trailer = bytes
        .get(marker_start + marker.len()..)
        .and_then(|value| value.strip_suffix(b"\n"))
        .ok_or_else(|| "curl byte-count trailer is malformed".to_owned())?;
    let trailer = core::str::from_utf8(trailer)
        .map_err(|_error| "curl byte-count trailer is not UTF-8".to_owned())?;
    let (header_text, body_text) = trailer
        .split_once(':')
        .ok_or_else(|| "curl byte-count trailer is malformed".to_owned())?;
    let header_bytes = header_text
        .parse::<usize>()
        .map_err(|_error| "curl header byte count is malformed".to_owned())?;
    let body_bytes = body_text
        .parse::<usize>()
        .map_err(|_error| "curl body byte count is malformed".to_owned())?;
    if header_bytes > MAXIMUM_RESPONSE_HEADER_BYTES {
        return Err(format!(
            "curl response headers exceed {MAXIMUM_RESPONSE_HEADER_BYTES} bytes"
        ));
    }
    if body_bytes > maximum_body_bytes {
        return Err(format!(
            "curl response body exceeds {maximum_body_bytes} bytes"
        ));
    }
    if marker_start != header_bytes + body_bytes {
        return Err("curl byte counts do not match captured output".to_owned());
    }
    let headers = bytes
        .get(..header_bytes)
        .ok_or_else(|| "curl header byte count exceeds output".to_owned())?;
    let (status, headers) = parse_curl_headers(headers)?;
    Ok(ExternalResponse {
        status,
        headers,
        header_bytes,
        body_bytes,
    })
}

fn parse_curl_headers(bytes: &[u8]) -> Result<(u16, BTreeMap<String, String>), String> {
    let text = core::str::from_utf8(bytes)
        .map_err(|_error| "curl response headers are not UTF-8".to_owned())?;
    let blocks = text
        .strip_suffix("\r\n\r\n")
        .ok_or_else(|| "curl response headers have no terminator".to_owned())?
        .split("\r\n\r\n")
        .collect::<Vec<_>>();
    let mut final_response = None;
    for block in blocks {
        final_response = Some(parse_curl_header_block(block)?);
    }
    final_response.ok_or_else(|| "curl response has no HTTP header block".to_owned())
}

fn parse_curl_header_block(block: &str) -> Result<(u16, BTreeMap<String, String>), String> {
    let mut lines = block.split("\r\n");
    let status_line = lines
        .next()
        .ok_or_else(|| "curl response has no status line".to_owned())?;
    let mut status_parts = status_line.split_ascii_whitespace();
    let protocol = status_parts
        .next()
        .ok_or_else(|| "curl status has no protocol".to_owned())?;
    let status_text = status_parts
        .next()
        .ok_or_else(|| "curl status has no code".to_owned())?;
    let status = status_text
        .parse::<u16>()
        .map_err(|_error| "curl status code is malformed".to_owned())?;
    if !protocol.starts_with("HTTP/") || status_text.len() != 3 || !(100..=599).contains(&status) {
        return Err("curl status line is malformed".to_owned());
    }
    let mut headers = BTreeMap::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        if line.len() > MAXIMUM_HEADER_LINE_BYTES {
            return Err("curl header line exceeds 8192 bytes".to_owned());
        }
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| "curl header line is malformed".to_owned())?;
        if name.len() > MAXIMUM_HEADER_NAME_BYTES || !valid_header_name(name) {
            return Err("curl header name is malformed".to_owned());
        }
        let name = name.to_ascii_lowercase();
        let value = value.trim();
        if value.len() > MAXIMUM_HEADER_VALUE_BYTES || value.chars().any(char::is_control) {
            return Err("curl header value exceeds bounds".to_owned());
        }
        let value = value.to_owned();
        if headers.insert(name.clone(), value).is_some() {
            return Err(format!("duplicate curl response header: {name}"));
        }
        if headers.len() > MAXIMUM_RESPONSE_HEADERS {
            return Err("curl response has too many headers".to_owned());
        }
    }
    Ok((status, headers))
}

/// Validate all external Markdown destinations using a deterministic transport.
///
/// `now_seconds` is a Unix timestamp injected by callers. The checker follows
/// no more than five redirects, retries 429/5xx responses no more than three
/// total attempts, and falls back from HEAD to GET only for 405 or 501.
///
/// # Errors
///
/// Returns an error for malformed or credential-bearing URLs, non-HTTPS final
/// URLs, redirect loops/depth, exhausted retries, invalid exception records,
/// expired or stale exceptions, and non-success final status codes.
pub fn check_external_links<T: ExternalTransport, S: Sleeper>(
    sources: &[LinkSource],
    exceptions: &[ExternalException],
    now_seconds: u64,
    transport: &mut T,
    sleeper: &mut S,
) -> Result<(), Report<MarkdownError>> {
    let mut urls = BTreeSet::new();
    for source in sources {
        let parsed = parse_markdown(&source.path, source.set, &source.markdown)?;
        urls.extend(parsed.links.into_iter().filter_map(|link| {
            let lower = link.destination.to_ascii_lowercase();
            (lower.starts_with("https://") || lower.starts_with("http://"))
                .then_some(link.destination)
        }));
    }
    let exception_map = validate_external_exceptions(exceptions, now_seconds, &urls)?;
    for url in urls {
        if exception_map.contains(&url) {
            continue;
        }
        check_external_url(&url, now_seconds, transport, sleeper)?;
    }
    Ok(())
}

fn validate_external_exceptions(
    exceptions: &[ExternalException],
    now_seconds: u64,
    urls: &BTreeSet<String>,
) -> Result<BTreeSet<String>, Report<MarkdownError>> {
    let mut exact = BTreeSet::new();
    for record in exceptions {
        if record.owner.trim().is_empty() || record.reason.trim().is_empty() {
            return Err(external_error(format!(
                "exception {} requires owner and reason",
                record.url
            )));
        }
        validate_external_url(&record.url)?;
        let expires = timestamp_seconds(&record.expires_at).ok_or_else(|| {
            external_error(format!(
                "exception {} has invalid expiry {}",
                record.url, record.expires_at
            ))
        })?;
        if now_seconds >= expires {
            return Err(external_error(format!(
                "exception {} is expired",
                record.url
            )));
        }
        if !urls.contains(&record.url) {
            return Err(external_error(format!("exception {} is stale", record.url)));
        }
        if !exact.insert(record.url.clone()) {
            return Err(external_error(format!(
                "duplicate exception {}",
                record.url
            )));
        }
    }
    Ok(exact)
}

fn check_external_url<T: ExternalTransport, S: Sleeper>(
    initial: &str,
    now_seconds: u64,
    transport: &mut T,
    sleeper: &mut S,
) -> Result<(), Report<MarkdownError>> {
    let mut current = validate_external_url(initial)?;
    let mut visited = BTreeSet::new();
    let mut redirects = 0;
    let mut method = "HEAD";
    let mut retry_attempts = 0;
    let mut fallback_used = false;
    loop {
        let canonical = current.as_str().to_owned();
        if !visited.insert((method, canonical.clone())) {
            return Err(external_error(format!("redirect loop at {canonical}")));
        }
        let request = ExternalRequest {
            method: method.to_owned(),
            url: canonical.clone(),
            timeout_seconds: 15,
            maximum_body_bytes: 64 * 1024,
        };
        let response = transport.send(&request).map_err(|diagnostic| {
            external_error(format!("request failed for {canonical}: {diagnostic}"))
        })?;
        validate_response_headers(&response, request.maximum_body_bytes)?;

        if method == "HEAD" && matches!(response.status, 405 | 501) && !fallback_used {
            method = "GET";
            fallback_used = true;
            continue;
        }
        if matches!(response.status, 429 | 500..=599) {
            retry_attempts += 1;
            if retry_attempts >= MAXIMUM_RETRY_ATTEMPTS {
                return Err(external_error(format!(
                    "retry attempts exhausted for {canonical}"
                )));
            }
            let local_delay = if retry_attempts == 1 { 1 } else { 2 };
            let delay = response
                .headers
                .get("retry-after")
                .and_then(|value| retry_after_seconds(value, now_seconds))
                .filter(|delay| *delay <= 30)
                .unwrap_or(local_delay);
            sleeper.sleep_seconds(delay);
            visited.remove(&(method, canonical));
            continue;
        }
        if (300..=399).contains(&response.status) {
            if redirects >= MAXIMUM_REDIRECTS {
                return Err(external_error(format!(
                    "redirect depth exceeds {MAXIMUM_REDIRECTS}"
                )));
            }
            let location = response.headers.get("location").ok_or_else(|| {
                external_error(format!("redirect {canonical} has no Location header"))
            })?;
            let next = current.join(location).map_err(|_error| {
                external_error(format!("redirect {canonical} has malformed Location"))
            })?;
            validate_url_parts(&next)?;
            redirects += 1;
            current = next;
            method = "HEAD";
            fallback_used = false;
            continue;
        }
        if !(200..=299).contains(&response.status) {
            return Err(external_error(format!(
                "final status {} for {canonical}",
                response.status
            )));
        }
        validate_url_parts(&current)?;
        return Ok(());
    }
}

fn validate_external_url(value: &str) -> Result<Url, Report<MarkdownError>> {
    if value.len() > MAXIMUM_LINK_BYTES
        || value.chars().any(char::is_control)
        || value.contains('\\')
    {
        return Err(external_error("external URL is oversized or unsafe"));
    }
    let url = Url::parse(value)
        .map_err(|_error| external_error(format!("malformed external URL: {value}")))?;
    validate_url_parts(&url)?;
    Ok(url)
}

fn validate_url_parts(url: &Url) -> Result<(), Report<MarkdownError>> {
    if url.scheme() != "https" {
        return Err(external_error(format!("final URL must use HTTPS: {url}")));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(external_error(format!(
            "credential-bearing URL is prohibited: {url}"
        )));
    }
    if url.host_str().is_none() {
        return Err(external_error(format!("URL has no host: {url}")));
    }
    Ok(())
}

fn validate_response_headers(
    response: &ExternalResponse,
    maximum_body_bytes: usize,
) -> Result<(), Report<MarkdownError>> {
    if !(100..=599).contains(&response.status) {
        return Err(external_error("response status is outside HTTP bounds"));
    }
    if response.headers.len() > MAXIMUM_RESPONSE_HEADERS {
        return Err(external_error("response has too many headers"));
    }
    let total = response
        .headers
        .iter()
        .map(|(name, value)| name.len() + value.len() + 4)
        .sum::<usize>();
    if response.header_bytes > MAXIMUM_RESPONSE_HEADER_BYTES
        || total > MAXIMUM_RESPONSE_HEADER_BYTES
        || response.headers.iter().any(|(name, value)| {
            name.len() > MAXIMUM_HEADER_NAME_BYTES
                || value.len() > MAXIMUM_HEADER_VALUE_BYTES
                || name.len() + value.len() + 4 > MAXIMUM_HEADER_LINE_BYTES
                || !valid_header_name(name)
                || value.chars().any(char::is_control)
        })
    {
        return Err(external_error("response headers exceed bounds"));
    }
    if response.body_bytes > maximum_body_bytes {
        return Err(external_error("response body exceeds bound"));
    }
    Ok(())
}

fn valid_header_name(name: &str) -> bool {
    !name.is_empty()
        && name.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
}

fn retry_after_seconds(value: &str, now_seconds: u64) -> Option<u64> {
    if value.bytes().all(|byte| byte.is_ascii_digit()) {
        return value.parse().ok();
    }
    http_date_seconds(value).map(|timestamp| timestamp.saturating_sub(now_seconds))
}

fn http_date_seconds(value: &str) -> Option<u64> {
    let bytes = value.as_bytes();
    if bytes.len() != 29
        || bytes[3] != b','
        || ![4, 7, 11, 16, 25]
            .into_iter()
            .all(|index| bytes[index] == b' ')
        || bytes[19] != b':'
        || bytes[22] != b':'
        || ![5, 6, 12, 13, 14, 15, 17, 18, 20, 21, 23, 24]
            .into_iter()
            .all(|index| bytes[index].is_ascii_digit())
    {
        return None;
    }
    let mut parts = value.split_ascii_whitespace();
    let weekday = parts.next()?;
    let day = parts.next()?.parse::<u32>().ok()?;
    let month = match parts.next()? {
        "Jan" => 1,
        "Feb" => 2,
        "Mar" => 3,
        "Apr" => 4,
        "May" => 5,
        "Jun" => 6,
        "Jul" => 7,
        "Aug" => 8,
        "Sep" => 9,
        "Oct" => 10,
        "Nov" => 11,
        "Dec" => 12,
        _ => return None,
    };
    let year = parts.next()?.parse::<u32>().ok()?;
    let time = parts.next()?;
    let zone = parts.next()?;
    if parts.next().is_some()
        || !matches!(
            weekday,
            "Sun," | "Mon," | "Tue," | "Wed," | "Thu," | "Fri," | "Sat,"
        )
        || zone != "GMT"
    {
        return None;
    }
    let mut clock = time.split(':');
    let hour = clock.next()?.parse::<u32>().ok()?;
    let minute = clock.next()?.parse::<u32>().ok()?;
    let second = clock.next()?.parse::<u32>().ok()?;
    if clock.next().is_some() {
        return None;
    }
    let timestamp = timestamp_components(year, month, day, hour, minute, second)?;
    let weekday_index = ((timestamp / 86_400 + 4) % 7) as usize;
    let expected = ["Sun,", "Mon,", "Tue,", "Wed,", "Thu,", "Fri,", "Sat,"];
    (weekday == expected[weekday_index]).then_some(timestamp)
}

fn timestamp_seconds(value: &str) -> Option<u64> {
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
    timestamp_components(
        decimal(bytes, 0, 4)?,
        decimal(bytes, 5, 2)?,
        decimal(bytes, 8, 2)?,
        decimal(bytes, 11, 2)?,
        decimal(bytes, 14, 2)?,
        decimal(bytes, 17, 2)?,
    )
}

fn timestamp_components(
    year: u32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
) -> Option<u64> {
    if year < 1970
        || !(1..=12).contains(&month)
        || day == 0
        || day > days_in_month(year, month)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }
    let mut days = 0_u64;
    for prior_year in 1970..year {
        days += u64::from(if is_leap(prior_year) {
            366_u16
        } else {
            365_u16
        });
    }
    for prior_month in 1..month {
        days += u64::from(days_in_month(year, prior_month));
    }
    days += u64::from(day - 1);
    Some(days * 86_400 + u64::from(hour * 3_600 + minute * 60 + second))
}

fn decimal(bytes: &[u8], start: usize, length: usize) -> Option<u32> {
    bytes
        .get(start..start + length)?
        .iter()
        .try_fold(0, |value, byte| {
            byte.is_ascii_digit()
                .then(|| value * 10 + u32::from(*byte - b'0'))
        })
}

const fn days_in_month(year: u32, month: u32) -> u32 {
    match month {
        2 if is_leap(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

const fn is_leap(year: u32) -> bool {
    year.is_multiple_of(400) || (year.is_multiple_of(4) && !year.is_multiple_of(100))
}

fn external_error(detail: impl Into<String>) -> Report<MarkdownError> {
    Report::new(MarkdownError::ExternalLink {
        detail: detail.into(),
    })
}
