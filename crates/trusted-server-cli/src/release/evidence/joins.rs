//! Document joins of the evidence graph: JS inventory, build evidence, SPDX
//! root/document equality and the informational rebuild outcome.
//!
//! Every document is parsed with the strict byte-first parser, so duplicate
//! keys, BOMs, excess depth and oversize input are rejected before any join.
//! Only the fields the joins need are read; their names follow the R-INDEX
//! field contracts because no closed build-evidence, inventory or
//! rebuild-check schema is retained yet.

use error_stack::Report;
use serde_json::Value;

use super::{at, evidence_error, object};
use crate::release::documents::{IndexArtifact, ReleaseIndex, Role, lower_hex, parse_json};
use crate::release::error::ReleaseError;

/// SPDX document element identifier.
const DOCUMENT_ID: &str = "SPDXRef-DOCUMENT";

/// One JS module identity shared by the inventory and build evidence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ModuleRecord {
    id: String,
    filename: String,
    size: u64,
    sha256: String,
}

/// Informational rebuild comparison outcome; never an authorization input.
#[derive(Clone, Copy, Debug, PartialEq, Eq, derive_more::Display)]
pub(crate) enum ComparisonOutcome {
    #[display("matched")]
    Matched,
    #[display("mismatched")]
    Mismatched,
    #[display("failed")]
    Failed,
    #[display("unavailable")]
    Unavailable,
}

fn module_record(value: &Value) -> Result<ModuleRecord, Report<ReleaseError>> {
    let text = |key: &str| value.get(key).and_then(Value::as_str);
    let record = ModuleRecord {
        id: text("id").unwrap_or_default().to_owned(),
        filename: text("filename").unwrap_or_default().to_owned(),
        size: value
            .get("size")
            .and_then(Value::as_u64)
            .unwrap_or(u64::MAX),
        sha256: text("sha256").unwrap_or_default().to_owned(),
    };
    if record.id.is_empty()
        || record.filename.is_empty()
        || record.size == u64::MAX
        || !lower_hex(&record.sha256, 64)
    {
        return Err(evidence_error(
            "JS module record needs id, filename, integer size and SHA-256",
        ));
    }
    Ok(record)
}

/// Parses an array of module records and requires strictly ascending ids.
fn sorted_modules(
    value: Option<&Value>,
    what: &str,
) -> Result<Vec<ModuleRecord>, Report<ReleaseError>> {
    let records = value
        .and_then(Value::as_array)
        .ok_or_else(|| evidence_error(format!("{what} must be an array")))?
        .iter()
        .map(module_record)
        .collect::<Result<Vec<_>, _>>()?;
    if records.windows(2).any(|pair| match pair {
        [first, second] => first.id >= second.id,
        _ => false,
    }) {
        return Err(evidence_error(format!(
            "{what} must be sorted by unique module id"
        )));
    }
    Ok(records)
}

fn require_version_and_source(
    document: &Value,
    commit: &str,
    what: &str,
) -> Result<(), Report<ReleaseError>> {
    object(document, what)?;
    if document.get("schema_version").and_then(Value::as_u64) != Some(1) {
        return Err(evidence_error(format!("{what} schema version must be 1")));
    }
    if document.get("source_commit").and_then(Value::as_str) != Some(commit) {
        return Err(evidence_error(format!(
            "{what} source commit is not the approved commit"
        )));
    }
    Ok(())
}

/// Parses the shared JS inventory and returns its module table.
///
/// # Errors
///
/// Returns [`ReleaseError::Document`] for malformed JSON and
/// [`ReleaseError::Evidence`] for another source commit or a malformed,
/// unsorted or duplicated module table.
pub(crate) fn parse_inventory(
    bytes: &[u8],
    commit: &str,
) -> Result<Vec<ModuleRecord>, Report<ReleaseError>> {
    let inventory = parse_json(bytes, Role::Inventory)?;
    require_version_and_source(&inventory, commit, "JS inventory")?;
    let modules = sorted_modules(inventory.get("modules"), "JS inventory modules")?;
    if modules.is_empty() {
        return Err(evidence_error("JS inventory lists no modules"));
    }
    Ok(modules)
}

/// Joins one artifact's build evidence to the index and shared inventory.
///
/// # Errors
///
/// Returns [`ReleaseError::Evidence`] when the build evidence names another
/// source, target, variant, executable or inventory digest, when its prepared
/// module table differs from the inventory, or when a claimed retained module
/// is not exactly an inventory module.
pub(crate) fn check_build_evidence(
    bytes: &[u8],
    entry: &IndexArtifact,
    index: &ReleaseIndex,
    modules: &[ModuleRecord],
) -> Result<(), Report<ReleaseError>> {
    let evidence = parse_json(bytes, Role::Companion)?;
    require_version_and_source(&evidence, &index.source_commit, "build evidence")?;
    let joins = [
        ("target", entry.target.as_str()),
        ("variant", entry.variant.as_str()),
        ("artifact_sha256", entry.sha256.as_str()),
        ("js_inventory_sha256", index.tsjs_inventory.sha256.as_str()),
    ];
    for (field, wanted) in joins {
        if evidence.get(field).and_then(Value::as_str) != Some(wanted) {
            return Err(evidence_error(format!(
                "build evidence `{field}` differs from the signed index"
            )));
        }
    }
    let prepared = sorted_modules(evidence.get("prepared_modules"), "prepared modules")?;
    if prepared != modules {
        return Err(evidence_error(
            "prepared JS modules differ from the shared inventory",
        ));
    }
    let retained = sorted_modules(evidence.get("embedded_modules"), "embedded modules")?;
    if let Some(unknown) = retained.iter().find(|module| !modules.contains(module)) {
        return Err(evidence_error(format!(
            "retained JS module `{}` is not exactly an inventory module",
            unknown.id
        )));
    }
    Ok(())
}

/// Checks the SPDX predicate against the published document and executable.
///
/// The published document is parsed strictly (duplicate keys rejected) and
/// compared as parsed JSON: object key order is ignored, array order is
/// significant. File-byte digest equality is checked separately against the
/// signed index; this comparison never substitutes for it.
///
/// # Errors
///
/// Returns [`ReleaseError::Evidence`] if the predicate differs from the
/// document or the document root checksum is not the executable digest.
pub(crate) fn check_spdx(
    predicate: &Value,
    document_bytes: &[u8],
    executable_sha256: &str,
) -> Result<(), Report<ReleaseError>> {
    let document = parse_json(document_bytes, Role::Spdx)?;
    if predicate != &document {
        return Err(evidence_error(
            "SPDX predicate differs from the published SPDX document",
        ));
    }
    if at(&document, &["spdxVersion"]).and_then(Value::as_str) != Some("SPDX-2.3")
        || at(&document, &["SPDXID"]).and_then(Value::as_str) != Some(DOCUMENT_ID)
    {
        return Err(evidence_error("published SBOM is not an SPDX 2.3 document"));
    }
    let root = spdx_root(&document)?;
    if root_sha256(&document, &root)? != executable_sha256 {
        return Err(evidence_error(
            "SPDX root package checksum differs from the executable digest",
        ));
    }
    Ok(())
}

/// Returns the single element the document describes.
fn spdx_root(document: &Value) -> Result<String, Report<ReleaseError>> {
    let mut described: Vec<&str> = document
        .get("documentDescribes")
        .and_then(Value::as_array)
        .map(|ids| ids.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let relationships = document
        .get("relationships")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    described.extend(relationships.iter().filter_map(|relationship| {
        let field = |key: &str| relationship.get(key).and_then(Value::as_str);
        (field("spdxElementId") == Some(DOCUMENT_ID)
            && field("relationshipType") == Some("DESCRIBES"))
        .then(|| field("relatedSpdxElement"))
        .flatten()
    }));
    described.sort_unstable();
    described.dedup();
    match described.as_slice() {
        [root] => Ok((*root).to_owned()),
        _ => Err(evidence_error(
            "SPDX document must describe exactly one root",
        )),
    }
}

fn root_sha256<'a>(document: &'a Value, root: &str) -> Result<&'a str, Report<ReleaseError>> {
    let packages: Vec<&Value> = document
        .get("packages")
        .and_then(Value::as_array)
        .map(|packages| {
            packages
                .iter()
                .filter(|package| package.get("SPDXID").and_then(Value::as_str) == Some(root))
                .collect()
        })
        .unwrap_or_default();
    let [package] = packages.as_slice() else {
        return Err(evidence_error("SPDX root must be exactly one package"));
    };
    let checksums: Vec<&str> = package
        .get("checksums")
        .and_then(Value::as_array)
        .map(|checksums| {
            checksums
                .iter()
                .filter(|checksum| {
                    checksum.get("algorithm").and_then(Value::as_str) == Some("SHA256")
                })
                .filter_map(|checksum| checksum.get("checksumValue").and_then(Value::as_str))
                .collect()
        })
        .unwrap_or_default();
    match checksums.as_slice() {
        [sha256] => Ok(sha256),
        _ => Err(evidence_error(
            "SPDX root package must carry exactly one SHA256 checksum",
        )),
    }
}

/// Parses the informational rebuild-check outcome.
///
/// # Errors
///
/// Returns [`ReleaseError::Evidence`] for an outcome outside the closed enum.
pub(crate) fn parse_comparison(bytes: &[u8]) -> Result<ComparisonOutcome, Report<ReleaseError>> {
    let document = parse_informational(bytes)?;
    match document.get("outcome").and_then(Value::as_str) {
        Some("matched") => Ok(ComparisonOutcome::Matched),
        Some("mismatched") => Ok(ComparisonOutcome::Mismatched),
        Some("failed") => Ok(ComparisonOutcome::Failed),
        Some("unavailable") => Ok(ComparisonOutcome::Unavailable),
        _ => Err(evidence_error(
            "rebuild check outcome must be matched, mismatched, failed or unavailable",
        )),
    }
}

/// Strictly parses a version-1 informational companion without executing it.
///
/// # Errors
///
/// Returns a document error for invalid JSON or an evidence error for an
/// unsupported version.
pub(super) fn parse_informational(bytes: &[u8]) -> Result<Value, Report<ReleaseError>> {
    let document = parse_json(bytes, Role::Companion)?;
    object(&document, "informational companion")?;
    if document.get("version").and_then(Value::as_u64) != Some(1) {
        return Err(evidence_error("informational companion version must be 1"));
    }
    Ok(document)
}
