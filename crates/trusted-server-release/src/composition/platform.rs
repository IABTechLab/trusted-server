//! Candidate installed-tool identities and artifact-bound external requirements.

use error_stack::Report;
use serde::Serialize;

use super::composition_error;
use super::external::ExternalRequirements;
use crate::documents::lower_hex;
use crate::error::ReleaseError;
use crate::policy::ReleaseTarget;
use crate::recipe::{AppleDeveloperTools, AppleToolEvidence, RustToolEvidence, SelectedRustTools};

/// Candidate platform observations without original acquisition or qualification.
#[derive(Debug, Serialize)]
pub(crate) struct PlatformEvidence {
    target: &'static str,
    artifact_sha256: String,
    rust: RustToolEvidence,
    apple: Option<AppleToolEvidence>,
    external_requirements: ExternalRequirements,
    native_tools: NativeToolCoverage,
    material_native_coverage_complete: bool,
}

#[derive(Debug, Serialize)]
#[serde(tag = "scope", rename_all = "snake_case")]
enum NativeToolCoverage {
    HostPathUnresolved { roles: [&'static str; 5] },
    ExplicitAppleFiles { full_closure_measured: bool },
}

impl PlatformEvidence {
    /// Binds measured selected tools to requirements from the accepted artifact.
    ///
    /// # Errors
    ///
    /// Rejects mismatched artifact/target identities or changed bound tools.
    pub(crate) fn collect(
        tools: &SelectedRustTools,
        bound_tools_digest: &str,
        requirements: ExternalRequirements,
        target: ReleaseTarget,
        artifact_sha256: &str,
    ) -> Result<Self, Report<ReleaseError>> {
        if !lower_hex(bound_tools_digest, 64)
            || !lower_hex(artifact_sha256, 64)
            || tools.target() != target
            || requirements.target() != target
            || requirements.artifact_sha256() != artifact_sha256
            || tools.apple().is_some() != target.is_darwin()
        {
            return Err(composition_error(
                "candidate platform identities differ from selected inputs",
            ));
        }
        if let Some(apple) = tools.apple()
            && requirements.macho_sdk() != Some(apple.sdk_version().to_string().as_str())
        {
            return Err(composition_error(
                "artifact SDK differs from selected Apple SDK",
            ));
        }
        let rust = tools.candidate_measurement()?;
        if rust.tree_sha256() != bound_tools_digest {
            return Err(composition_error(
                "Rust tools changed before platform measurement",
            ));
        }
        let apple = tools
            .apple()
            .map(AppleDeveloperTools::candidate_measurement)
            .transpose()?;
        if tools.digest()? != bound_tools_digest {
            return Err(composition_error(
                "Rust tools changed during platform measurement",
            ));
        }
        if let Some(apple) = tools.apple() {
            apple.verify()?;
        }
        Ok(Self {
            target: target.triple(),
            artifact_sha256: artifact_sha256.to_owned(),
            rust,
            apple,
            external_requirements: requirements,
            native_tools: if target.is_darwin() {
                NativeToolCoverage::ExplicitAppleFiles {
                    full_closure_measured: false,
                }
            } else {
                NativeToolCoverage::HostPathUnresolved {
                    roles: ["compiler", "assembler", "archiver", "linker", "build_tools"],
                }
            },
            material_native_coverage_complete: false,
        })
    }
}

#[cfg(test)]
mod tests;
