//! Producer copy of the consumer's document joins over rendered companions.
//!
//! These mirror the CLI verifier's inventory, build-evidence, SPDX root and
//! informational-companion joins so the producer rejects a candidate the
//! consumer would reject. They parse strictly and read only joined fields.
//!
//! Drift guard: the CLI test
//! `crates/trusted-server-cli/src/release/evidence/candidate_fixture_tests.rs`
//! runs the consumer's own joins over the golden fixtures this producer
//! renders, so a divergence between these copies and the CLI fails there.

use error_stack::Report;
use serde_json::Value;

use super::distribution_error;
use crate::documents::{Role as DocumentRole, lower_hex, parse_json};
use crate::error::ReleaseError;

/// Producer role whose byte limit equals the CLI's `Role::Inventory` bound.
pub(super) const INVENTORY_ROLE: DocumentRole = DocumentRole::Metadata;
/// The CLI's `bounds::INVENTORY_BYTES` inventory limit (1 MiB).
#[cfg(test)]
pub(super) const CLI_INVENTORY_BYTES: u64 = 1_048_576;

/// SPDX document element identifier.
const DOCUMENT_ID: &str = "SPDXRef-DOCUMENT";

/// The JS module identity joined between inventory and build evidence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ModuleIdentity {
    pub(super) id: String,
    pub(super) filename: String,
    pub(super) size: u64,
    pub(super) sha256: String,
}

fn parse(bytes: &[u8], role: DocumentRole) -> Result<Value, Report<ReleaseError>> {
    parse_json(bytes, role).map_err(|error| error.change_context(ReleaseError::Distribution))
}

fn module_identity(value: &Value) -> Result<ModuleIdentity, Report<ReleaseError>> {
    let text = |key: &str| value[key].as_str().unwrap_or_default().to_owned();
    let module = ModuleIdentity {
        id: text("id"),
        filename: text("filename"),
        size: value["size"].as_u64().unwrap_or(u64::MAX),
        sha256: text("sha256"),
    };
    if module.id.is_empty()
        || module.filename.is_empty()
        || module.size == u64::MAX
        || !lower_hex(&module.sha256, 64)
    {
        return Err(distribution_error(
            "JS module record needs id, filename, integer size and SHA-256",
        ));
    }
    Ok(module)
}

fn sorted_modules(value: &Value, what: &str) -> Result<Vec<ModuleIdentity>, Report<ReleaseError>> {
    let modules = value
        .as_array()
        .ok_or_else(|| distribution_error(format!("{what} must be an array")))?
        .iter()
        .map(module_identity)
        .collect::<Result<Vec<_>, _>>()?;
    if modules.windows(2).any(|pair| pair[0].id >= pair[1].id) {
        return Err(distribution_error(format!(
            "{what} must be sorted by unique module id"
        )));
    }
    Ok(modules)
}

fn require_version_and_source(
    document: &Value,
    version_field: &str,
    commit: &str,
    what: &str,
) -> Result<(), Report<ReleaseError>> {
    if !document.is_object() || document[version_field].as_u64() != Some(1) {
        return Err(distribution_error(format!("{what} version must be 1")));
    }
    if document["source_commit"] != commit {
        return Err(distribution_error(format!(
            "{what} source commit is not the candidate source"
        )));
    }
    Ok(())
}

/// Parses the shared JS inventory and returns its module identities.
pub(super) fn inventory_modules(
    bytes: &[u8],
    commit: &str,
) -> Result<Vec<ModuleIdentity>, Report<ReleaseError>> {
    let inventory = parse(bytes, INVENTORY_ROLE)?;
    require_version_and_source(&inventory, "schema_version", commit, "JS inventory")?;
    let modules = sorted_modules(&inventory["modules"], "JS inventory modules")?;
    if modules.is_empty() {
        return Err(distribution_error("JS inventory lists no modules"));
    }
    Ok(modules)
}

/// The index facts one build-evidence companion must join.
pub(super) struct EvidenceJoin<'a> {
    pub(super) source_commit: &'a str,
    pub(super) target: &'a str,
    pub(super) artifact_sha256: &'a str,
    pub(super) inventory_sha256: &'a str,
    pub(super) modules: &'a [ModuleIdentity],
}

/// Joins rendered build evidence to the index facts and shared inventory.
pub(super) fn check_build_evidence(
    bytes: &[u8],
    join: &EvidenceJoin<'_>,
) -> Result<(), Report<ReleaseError>> {
    let evidence = parse(bytes, DocumentRole::Companion)?;
    require_version_and_source(
        &evidence,
        "schema_version",
        join.source_commit,
        "build evidence",
    )?;
    for (field, wanted) in [
        ("target", join.target),
        ("variant", "default"),
        ("artifact_sha256", join.artifact_sha256),
        ("js_inventory_sha256", join.inventory_sha256),
    ] {
        if evidence[field] != wanted {
            return Err(distribution_error(format!(
                "build evidence `{field}` differs from the index entry"
            )));
        }
    }
    let prepared = sorted_modules(&evidence["prepared_modules"], "prepared modules")?;
    if prepared != join.modules {
        return Err(distribution_error(
            "prepared JS modules differ from the shared inventory",
        ));
    }
    let embedded = sorted_modules(&evidence["embedded_modules"], "embedded modules")?;
    if let Some(unknown) = embedded
        .iter()
        .find(|module| !join.modules.contains(module))
    {
        return Err(distribution_error(format!(
            "embedded JS module `{}` is not exactly an inventory module",
            unknown.id
        )));
    }
    Ok(())
}

/// Requires an SPDX 2.3 document describing one root with the executable digest.
pub(super) fn check_spdx_root(
    bytes: &[u8],
    artifact_sha256: &str,
) -> Result<(), Report<ReleaseError>> {
    let document = parse(bytes, DocumentRole::Spdx)?;
    if document["spdxVersion"] != "SPDX-2.3" || document["SPDXID"] != DOCUMENT_ID {
        return Err(distribution_error("SBOM is not an SPDX 2.3 document"));
    }
    let mut described: Vec<&str> = document["documentDescribes"]
        .as_array()
        .map(|ids| ids.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    described.extend(
        document["relationships"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .filter(|relationship| {
                relationship["spdxElementId"] == DOCUMENT_ID
                    && relationship["relationshipType"] == "DESCRIBES"
            })
            .filter_map(|relationship| relationship["relatedSpdxElement"].as_str()),
    );
    described.sort_unstable();
    described.dedup();
    let [root] = described.as_slice() else {
        return Err(distribution_error(
            "SPDX document must describe exactly one root",
        ));
    };
    let packages: Vec<&Value> = document["packages"]
        .as_array()
        .map(|packages| {
            packages
                .iter()
                .filter(|package| package["SPDXID"] == *root)
                .collect()
        })
        .unwrap_or_default();
    let [package] = packages.as_slice() else {
        return Err(distribution_error("SPDX root must be exactly one package"));
    };
    let checksums: Vec<&str> = package["checksums"]
        .as_array()
        .map(|checksums| {
            checksums
                .iter()
                .filter(|checksum| checksum["algorithm"] == "SHA256")
                .filter_map(|checksum| checksum["checksumValue"].as_str())
                .collect()
        })
        .unwrap_or_default();
    if checksums.as_slice() != [artifact_sha256] {
        return Err(distribution_error(
            "SPDX root package checksum differs from the executable digest",
        ));
    }
    Ok(())
}

/// Strictly parses a version-1 informational companion for `source_commit`.
pub(super) fn parse_informational(
    bytes: &[u8],
    source_commit: &str,
) -> Result<Value, Report<ReleaseError>> {
    let document = parse(bytes, DocumentRole::Companion)?;
    require_version_and_source(
        &document,
        "version",
        source_commit,
        "informational companion",
    )?;
    Ok(document)
}

/// Requires a rebuild-check outcome from the closed consumer vocabulary.
pub(super) fn check_comparison(
    bytes: &[u8],
    source_commit: &str,
) -> Result<(), Report<ReleaseError>> {
    let document = parse_informational(bytes, source_commit)?;
    match document["outcome"].as_str() {
        Some("matched" | "mismatched" | "failed" | "unavailable") => Ok(()),
        _ => Err(distribution_error(
            "rebuild check outcome must be matched, mismatched, failed or unavailable",
        )),
    }
}
