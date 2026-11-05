//! Conservative final-byte membership for verified uncompressed JS modules.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use error_stack::Report;
use serde::Serialize;
use sha2::{Digest as _, Sha256};

use super::composition_error;
use crate::documents::{Role, lower_hex, read_bounded};
use crate::error::ReleaseError;
use crate::javascript::RetainedJsModule;

/// Selected fixed recipe's required final JS coverage.
#[derive(Clone, Copy)]
pub(crate) enum JsRetentionPolicy {
    /// Fastly can serve each prepared module and must retain its complete bytes.
    FastlyAll,
    /// The operator CLI may discard modules it does not use.
    ObserveCli,
}

/// Checked JS handoff inputs selected by the recipe.
pub(crate) struct JsInputs<'a> {
    /// Retained root owned by the successful two-build JS handoff.
    pub(crate) retained_root: &'a Path,
    /// Module facts exported from that rehashed owning handoff.
    pub(crate) modules: &'a [RetainedJsModule],
    /// Exact raw handoff inventory identity.
    pub(crate) inventory_sha256: &'a str,
    /// Required coverage of the fixed asset recipe.
    pub(crate) policy: JsRetentionPolicy,
}

/// Per-artifact retained-module observations, without function reachability claims.
#[derive(Debug, Serialize)]
pub(crate) struct JsMembership {
    artifact_sha256: String,
    inventory_sha256: String,
    modules: Vec<ModuleMembership>,
    method: &'static str,
}

#[derive(Debug, Serialize)]
struct ModuleMembership {
    module: RetainedJsModule,
    classification: &'static str,
}

/// Checks complete prepared modules against the exact inspected final bytes.
///
/// # Errors
///
/// Rejects changed modules, invalid bindings, incomplete module sets or a
/// Fastly executable missing any required complete module.
pub(crate) fn collect_js(
    bytes: &[u8],
    artifact_sha256: &str,
    inputs: &JsInputs<'_>,
) -> Result<JsMembership, Report<ReleaseError>> {
    if bytes.is_empty()
        || bytes.len() as u64 > Role::Executable.limit()
        || !lower_hex(artifact_sha256, 64)
        || !lower_hex(inputs.inventory_sha256, 64)
        || hex::encode(Sha256::digest(bytes)) != artifact_sha256
        || inputs.modules.is_empty()
        || inputs.modules.len() > 1024
    {
        return Err(composition_error(
            "invalid final executable or JS input binding",
        ));
    }
    let bundles = inputs.retained_root.join("bundles");
    for directory in [inputs.retained_root, bundles.as_path()] {
        if fs::canonicalize(directory).map_err(composition_error)? != directory {
            return Err(composition_error("retained JS directory is aliased"));
        }
    }
    let inventory = read_bounded(
        &inputs.retained_root.join("tsjs-inventory.json"),
        Role::Metadata,
    )
    .map_err(|error| error.change_context(ReleaseError::Composition))?;
    if inventory.sha256 != inputs.inventory_sha256 {
        return Err(composition_error(
            "retained JS inventory differs from the bound identity",
        ));
    }
    let mut ids = BTreeSet::new();
    let mut filenames = BTreeSet::new();
    for module in inputs.modules {
        if !ids.insert(module.id()) || !filenames.insert(module.filename()) {
            return Err(composition_error("duplicate prepared JS module identity"));
        }
    }
    let actual = fs::read_dir(&bundles)
        .map_err(composition_error)?
        .map(|entry| {
            entry
                .map_err(composition_error)?
                .file_name()
                .into_string()
                .map_err(|_| composition_error("JS filename is not UTF-8"))
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    if actual.iter().map(String::as_str).collect::<BTreeSet<_>>() != filenames {
        return Err(composition_error(
            "prepared JS file set differs from checked module facts",
        ));
    }
    let mut modules = Vec::with_capacity(inputs.modules.len());
    for module in inputs.modules {
        let raw = read_bounded(&bundles.join(module.filename()), Role::Bundle)
            .map_err(|error| error.change_context(ReleaseError::Composition))?;
        if raw.bytes.is_empty()
            || raw.sha256 != module.sha256()
            || raw.bytes.len() as u64 != module.size()
        {
            return Err(composition_error(
                "prepared JS module changed before final-byte inspection",
            ));
        }
        let retained = bytes
            .windows(raw.bytes.len())
            .any(|window| window == raw.bytes);
        if !retained && matches!(inputs.policy, JsRetentionPolicy::FastlyAll) {
            return Err(composition_error(format!(
                "Fastly executable omits complete module {}",
                module.id()
            )));
        }
        modules.push(ModuleMembership {
            module: module.clone(),
            classification: if retained {
                "conservative_embedded_js"
            } else {
                "js_build_material"
            },
        });
    }
    Ok(JsMembership {
        artifact_sha256: artifact_sha256.to_owned(),
        inventory_sha256: inputs.inventory_sha256.to_owned(),
        modules,
        method: "complete-uncompressed-module-substring; no function reachability claim",
    })
}

#[cfg(test)]
mod tests;

impl JsMembership {
    /// Returns the exact inspected executable identity.
    pub(crate) fn artifact_sha256(&self) -> &str {
        &self.artifact_sha256
    }
    /// Returns the checked original collector inventory identity.
    pub(crate) fn inventory_sha256(&self) -> &str {
        &self.inventory_sha256
    }
    /// Returns every prepared module with final artifact classification.
    pub(crate) fn modules(&self) -> impl Iterator<Item = (&RetainedJsModule, &str)> {
        self.modules
            .iter()
            .map(|entry| (&entry.module, entry.classification))
    }
}
