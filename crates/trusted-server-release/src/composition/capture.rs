//! Retained raw observations and their normalized conservative unit inventory.

use std::fs;
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::Path;

use error_stack::Report;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use super::graph::{self, GraphEvidence};
use super::javascript::JsMembership;
use super::native::NativeInventory;
use super::origin::VerifiedPackageOrigins;
use super::platform::PlatformEvidence;
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
pub(crate) struct CargoCapture {
    metadata: Vec<u8>,
    tree: Vec<u8>,
    normalized: Vec<u8>,
    digests: [String; 3],
    observations: CargoObservations,
    query_plan_digests: [String; 2],
    receipt: Option<CompositionReceipt>,
}

#[derive(serde::Serialize)]
struct CompositionReceipt {
    graph: GraphEvidence,
    package_origins: VerifiedPackageOrigins,
    native: NativeInventory,
    javascript: JsMembership,
    platform: PlatformEvidence,
}

/// Borrowed sealed observations after their retained bytes have been rechecked.
///
/// This grants read access only. Potential graph membership and incomplete
/// native, tool and platform coverage retain their existing meanings.
pub(crate) struct CompositionView<'a> {
    observations: &'a CargoObservations,
    receipt: &'a CompositionReceipt,
    receipt_sha256: &'a str,
}

impl CompositionView<'_> {
    /// Returns the exact retained normalized receipt identity without reserialization.
    pub(crate) fn receipt_sha256(&self) -> &str {
        self.receipt_sha256
    }

    /// Returns actual compiler observations, including unit context and features.
    pub(crate) fn observations(&self) -> &CargoObservations {
        self.observations
    }

    /// Returns potential graph evidence separately from compiler observations.
    pub(crate) fn graph(&self) -> &GraphEvidence {
        &self.receipt.graph
    }

    /// Returns exact observed package IDs joined to verified originals.
    pub(crate) fn package_origins(&self) -> &VerifiedPackageOrigins {
        &self.receipt.package_origins
    }

    /// Returns candidate native measurements with their existing coverage gaps.
    pub(crate) fn native(&self) -> &NativeInventory {
        &self.receipt.native
    }

    /// Returns final-byte JavaScript module observations.
    pub(crate) fn javascript(&self) -> &JsMembership {
        &self.receipt.javascript
    }

    /// Returns candidate platform measurements without acquisition authority.
    pub(crate) fn platform(&self) -> &PlatformEvidence {
        &self.receipt.platform
    }
}

/// Exact retained composition bytes, without a claim of complete coverage.
///
/// Only the owning capture can construct this diagnostic export. It retains
/// sealed observations after working source and native output roots are removed.
#[derive(Debug)]
pub(crate) struct RetainedCompositionExport {
    bytes: Vec<u8>,
    sha256: String,
    summary: Value,
}

impl RetainedCompositionExport {
    /// Returns the original normalized receipt bytes without reserialization.
    pub(crate) fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Returns the SHA-256 of these exact retained bytes.
    pub(crate) fn sha256(&self) -> &str {
        &self.sha256
    }

    /// Returns observation counts and explicit incomplete coverage diagnostics.
    pub(crate) fn summary(&self) -> &Value {
        &self.summary
    }
}

impl core::fmt::Debug for CargoCapture {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("CargoCapture")
            .field("digests", &self.digests)
            .field("has_composition_receipt", &self.receipt.is_some())
            .finish_non_exhaustive()
    }
}

impl CargoCapture {
    /// Borrows sealed typed evidence after rechecking all retained observations.
    ///
    /// # Errors
    ///
    /// Rejects changed retained bytes or observations without sealed receipts.
    pub(crate) fn view(
        &self,
        directory: &Path,
    ) -> Result<CompositionView<'_>, Report<ReleaseError>> {
        self.rehash(directory)?;
        let receipt = self.receipt.as_ref().ok_or_else(|| {
            composition_error("composition view requires actual sealed component receipts")
        })?;
        Ok(CompositionView {
            observations: &self.observations,
            receipt,
            receipt_sha256: &self.digests[2],
        })
    }

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
            receipt: None,
        })
    }

    /// The observed units whose inputs must be joined before sealing evidence.
    pub(crate) fn observations(&self) -> &CargoObservations {
        &self.observations
    }

    /// Seals checked component receipts while their working inputs still exist.
    ///
    /// Native sources and archives are rehashed here. Later handoffs verify the
    /// retained receipt bytes, allowing completed working roots to be cleaned.
    /// Full tool/platform coverage and qualification remain separate gates.
    ///
    /// # Errors
    ///
    /// Rejects changed native inputs or oversized serialized evidence.
    pub(crate) fn with_evidence(
        mut self,
        selected_package_id: &str,
        package_origins: VerifiedPackageOrigins,
        native: NativeInventory,
        javascript: JsMembership,
        platform: PlatformEvidence,
    ) -> Result<Self, Report<ReleaseError>> {
        let graph = graph::reconcile(
            &self.metadata,
            &self.tree,
            &self.observations,
            selected_package_id,
        )?;
        native.rehash(&|| Ok(()))?;
        let receipt = CompositionReceipt {
            graph,
            package_origins,
            native,
            javascript,
            platform,
        };
        let normalized = serde_json::to_vec(&json!({
            "metadata_plan_sha256": self.query_plan_digests[0],
            "tree_plan_sha256": self.query_plan_digests[1],
            "observations": self.observations,
            "composition": receipt,
        }))
        .map_err(composition_error)?;
        if normalized.len() as u64 > LIMIT {
            return Err(composition_error(
                "normalized composition exceeds its bound",
            ));
        }
        self.digests[2] = hex::encode(Sha256::digest(&normalized));
        self.normalized = normalized;
        self.receipt = Some(receipt);
        Ok(self)
    }

    /// Returns the exact package identity of the binary accepted by the recipe.
    ///
    /// # Errors
    ///
    /// Rejects missing or ambiguous binary membership and mismatched manifests.
    pub(crate) fn require_binary(
        &self,
        manifest: &Path,
        bin: &str,
        executable: &Path,
    ) -> Result<String, Report<ReleaseError>> {
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
            .collect::<Vec<_>>();
        if matching.len() != 1 {
            return Err(composition_error(
                "composition does not name the exact accepted binary",
            ));
        }
        Ok(matching[0].package_id.clone())
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

    /// Exports exact sealed composition bytes after checking every retained input.
    ///
    /// # Errors
    ///
    /// Rejects changed retained files or observations without actual graph,
    /// origin, native, JavaScript and platform receipts. Existing coverage gaps
    /// remain explicit; this cannot construct complete or qualified evidence.
    pub(crate) fn export(
        &self,
        directory: &Path,
    ) -> Result<RetainedCompositionExport, Report<ReleaseError>> {
        self.rehash(directory)?;
        if self.receipt.is_none() {
            return Err(composition_error(
                "composition export requires actual sealed component receipts",
            ));
        }
        Ok(RetainedCompositionExport {
            bytes: self.normalized.clone(),
            sha256: self.digests[2].clone(),
            summary: self.summary(),
        })
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
            "composition_receipts_retained": self.receipt.is_some(),
            "material_native_coverage_complete": false,
        })
    }
}
