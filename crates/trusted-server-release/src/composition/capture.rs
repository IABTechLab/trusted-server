//! Retained raw observations and their normalized conservative unit inventory.

use std::fs;
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::Path;

use error_stack::Report;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use super::{CargoLayout, CargoObservations, composition_error, observe_cargo};
use crate::documents::{hash_bounded_with_check, lower_hex};
use crate::error::ReleaseError;

const LIMIT: u64 = 16 * 1024 * 1024;
const METADATA: &str = "cargo-metadata.json";
const TREE: &str = "cargo-tree.txt";
const NORMALIZED: &str = "cargo-observations.json";

/// Raw Cargo observations bound to their checked unit normalization.
///
/// This records observations only; verified material/native coverage remains
/// a separate requirement before a build composition can be attested.
#[derive(Debug)]
pub(crate) struct CargoCapture {
    metadata: Vec<u8>,
    tree: Vec<u8>,
    normalized: Vec<u8>,
    digests: [String; 3],
    observations: CargoObservations,
    query_plan_digests: [String; 2],
}

impl CargoCapture {
    /// Normalizes a successful build and its same-boundary query output.
    ///
    /// # Errors
    ///
    /// Rejects invalid unit/metadata joins, non-text or oversized tree output,
    /// or normalization exceeding the internal observation bound.
    pub(crate) fn new(
        metadata: Vec<u8>,
        tree: Vec<u8>,
        stdout: &[u8],
        layout: &CargoLayout,
        query_plan_digests: [String; 2],
    ) -> Result<Self, Report<ReleaseError>> {
        if query_plan_digests
            .iter()
            .any(|digest| !lower_hex(digest, 64))
        {
            return Err(composition_error(
                "query plans have no exact SHA-256 identities",
            ));
        }
        if tree.is_empty()
            || tree.len() as u64 > LIMIT
            || tree.contains(&0)
            || std::str::from_utf8(&tree).is_err()
            || !tree.ends_with(b"\n")
        {
            return Err(composition_error(
                "Cargo tree output is not bounded newline-terminated text",
            ));
        }
        let observations = observe_cargo(&metadata, stdout, layout)?;
        let normalized = serde_json::to_vec(&json!({
            "metadata_plan_sha256": query_plan_digests[0],
            "tree_plan_sha256": query_plan_digests[1],
            "observations": observations,
        }))
        .map_err(composition_error)?;
        if normalized.len() as u64 > LIMIT {
            return Err(composition_error(
                "normalized observations exceed their bound",
            ));
        }
        let digests =
            [&metadata, &tree, &normalized].map(|bytes| hex::encode(Sha256::digest(bytes)));
        Ok(Self {
            metadata,
            tree,
            normalized,
            digests,
            observations,
            query_plan_digests,
        })
    }

    /// Requires the unit inventory to name the exact executable accepted by the recipe.
    ///
    /// # Errors
    ///
    /// Rejects missing or ambiguous binary membership and mismatched manifests.
    pub(crate) fn require_binary(
        &self,
        manifest: &Path,
        bin: &str,
        executable: &Path,
    ) -> Result<(), Report<ReleaseError>> {
        let matching = self
            .observations
            .units
            .iter()
            .filter(|unit| {
                unit.target_name == bin
                    && unit.target_kind == ["bin"]
                    && unit.context == super::UnitContext::Target
                    && unit.executable.as_deref() == Some(executable)
                    && self.observations.packages.iter().any(|package| {
                        package.id == unit.package_id && package.manifest_path == manifest
                    })
            })
            .count();
        if matching != 1 {
            return Err(composition_error(
                "composition does not name the exact accepted binary",
            ));
        }
        Ok(())
    }

    /// Retains every raw/normalized observation as a new private file.
    ///
    /// # Errors
    ///
    /// Returns an error on existing files or failed writes/synchronization.
    pub(crate) fn retain(&self, directory: &Path) -> Result<(), Report<ReleaseError>> {
        for (name, bytes) in [
            (METADATA, &self.metadata),
            (TREE, &self.tree),
            (NORMALIZED, &self.normalized),
        ] {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(directory.join(name))
                .map_err(composition_error)?;
            file.write_all(bytes)
                .and_then(|()| file.sync_all())
                .map_err(composition_error)?;
        }
        Ok(())
    }

    /// Verifies retained observations immediately before a later handoff.
    ///
    /// # Errors
    ///
    /// Rejects changed, aliased, linked, missing or oversized retained observations.
    pub(crate) fn rehash(&self, directory: &Path) -> Result<(), Report<ReleaseError>> {
        for (name, expected) in [METADATA, TREE, NORMALIZED].into_iter().zip(&self.digests) {
            let measured = hash_bounded_with_check(&directory.join(name), LIMIT, &|| Ok(()))
                .map_err(|error| error.change_context(ReleaseError::Composition))?;
            if &measured.sha256 != expected {
                return Err(composition_error(format!("retained {name} changed")));
            }
        }
        Ok(())
    }

    /// Describes internal observations without claiming complete native/material coverage.
    pub(crate) fn summary(&self) -> Value {
        json!({
            "metadata_sha256": self.digests[0], "tree_sha256": self.digests[1],
            "metadata_plan_sha256": self.query_plan_digests[0],
            "tree_plan_sha256": self.query_plan_digests[1],
            "normalized_sha256": self.digests[2],
            "observed_packages": self.observations.packages.len(),
            "observed_units": self.observations.units.len(),
            "build_script_executions": self.observations.build_scripts.len(),
            "material_native_coverage_complete": false,
        })
    }
}
