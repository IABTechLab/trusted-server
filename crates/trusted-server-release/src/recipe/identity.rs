//! Shared source-owned recipe definitions, distinct from exact invocations.
//!
//! A definition selects the approved source policy. It does not prove that the
//! policy ran, authenticate the running bootstrap, or qualify tools/boundaries.

use std::path::Path;

use error_stack::Report;
use serde::Serialize;
use sha2::{Digest as _, Sha256};

use super::{AssetId, CargoPlan, recipe_error};
use crate::documents::{Role, canonical_json};
use crate::error::ReleaseError;
use crate::source::{ApprovedSource, CommitId};

/// Canonical shared definition selected from approved source and a fixed asset.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct RecipeDefinitionIdentity {
    descriptor: RecipeDefinition,
    sha256: String,
}

/// Source-owned recipe policy description, without execution authority.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct RecipeDefinition {
    domain: &'static str,
    schema_version: u64,
    source_commit: String,
    asset: &'static str,
    package: &'static str,
    binary: &'static str,
    target: &'static str,
    variant: &'static str,
    cargo: CargoDefinition,
    profile_definition: ProfileDefinition,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct CargoDefinition {
    command: &'static str,
    profile: &'static str,
    default_features: bool,
    features: Vec<String>,
    frozen: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct ProfileDefinition {
    path: &'static str,
    sha256: String,
}

impl RecipeDefinitionIdentity {
    /// Selects a common recipe definition without invocation-specific inputs.
    ///
    /// # Errors
    ///
    /// Rejects missing approved profile bytes or failed canonical serialization.
    pub(crate) fn from_source(
        source: &ApprovedSource,
        asset: AssetId,
    ) -> Result<Self, Report<ReleaseError>> {
        let manifests = source.cargo_manifest_digests();
        let manifest_sha256 = manifests.get(Path::new("Cargo.toml")).ok_or_else(|| {
            recipe_error("recipe definition requires an approved regular root Cargo.toml")
        })?;
        let artifact = asset.artifact();
        let descriptor = RecipeDefinition {
            domain: "trusted-server.release.recipe-definition",
            schema_version: 1,
            source_commit: source.commit().as_str().to_owned(),
            asset: artifact.name,
            package: artifact.package,
            binary: artifact.bin,
            target: artifact.target.triple(),
            variant: "default",
            // The fixed cohort permits only default features. Profile bytes and
            // all other implementation/tool policy are bound by approved source.
            cargo: CargoDefinition {
                command: "build",
                profile: "release",
                default_features: true,
                features: Vec::new(),
                frozen: true,
            },
            profile_definition: ProfileDefinition {
                path: "Cargo.toml",
                sha256: manifest_sha256.clone(),
            },
        };
        let sha256 = hash_definition(&descriptor)?;
        Ok(Self { descriptor, sha256 })
    }

    /// The source-owned descriptor whose canonical bytes were hashed.
    pub(crate) fn descriptor(&self) -> &RecipeDefinition {
        &self.descriptor
    }

    /// SHA-256 of the canonical domain-separated definition descriptor.
    pub(crate) fn sha256(&self) -> &str {
        &self.sha256
    }

    /// Checks the source and fixed asset without granting execution authority.
    pub(super) fn matches_selection(&self, commit: &CommitId, asset: AssetId) -> bool {
        let artifact = asset.artifact();
        self.descriptor.source_commit == commit.as_str()
            && self.descriptor.asset == artifact.name
            && self.descriptor.package == artifact.package
            && self.descriptor.binary == artifact.bin
            && self.descriptor.target == artifact.target.triple()
    }
}

/// Digest of the actual Cargo invocation, including paths and ambient choices.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub(crate) struct ExactInvocationDigest(String);

impl ExactInvocationDigest {
    /// Preserves the existing exact [`CargoPlan::digest`] calculation.
    ///
    /// # Errors
    ///
    /// Rejects plans whose exact digest cannot be serialized.
    pub(crate) fn from_plan(plan: &CargoPlan) -> Result<Self, Report<ReleaseError>> {
        plan.digest().map(Self)
    }

    /// SHA-256 of the exact invocation description.
    pub(crate) fn sha256(&self) -> &str {
        &self.0
    }
}

fn hash_definition(descriptor: &RecipeDefinition) -> Result<String, Report<ReleaseError>> {
    let value =
        serde_json::to_value(descriptor).map_err(|error| recipe_error(error.to_string()))?;
    let bytes = canonical_json(&value, Role::Metadata)
        .map_err(|error| error.change_context(ReleaseError::Recipe))?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

#[cfg(test)]
mod tests;
