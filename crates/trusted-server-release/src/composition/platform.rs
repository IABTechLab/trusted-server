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
    ExplicitFastlyHostFiles {
        evidence: Box<crate::recipe::fastly_tools::FastlyToolEvidence>,
    },
    HostPathUnresolved {
        roles: [&'static str; 5],
    },
    ExplicitAppleFiles {
        full_closure_measured: bool,
    },
    ExplicitGnuImage {
        evidence: Box<crate::recipe::gnu::GnuEvidence>,
    },
}

impl PlatformEvidence {
    /// Returns the selected target triple.
    pub(crate) fn target(&self) -> &str {
        self.target
    }
    /// Returns the accepted executable's raw-byte digest.
    pub(crate) fn artifact_sha256(&self) -> &str {
        &self.artifact_sha256
    }
    /// Borrows the measured installed Rust identity.
    pub(crate) fn rust(&self) -> &RustToolEvidence {
        &self.rust
    }
    /// Borrows exact Apple observations, including their exclusions.
    pub(crate) fn apple(&self) -> Option<&AppleToolEvidence> {
        self.apple.as_ref()
    }
    /// Borrows the owning GNU measurement, when actually captured.
    pub(crate) fn gnu(&self) -> Option<&crate::recipe::gnu::GnuEvidence> {
        match &self.native_tools {
            NativeToolCoverage::ExplicitGnuImage { evidence } => Some(evidence.as_ref()),
            _ => None,
        }
    }
    /// Borrows bounded selected Fastly host files when measured by execution.
    pub(crate) fn fastly_tools(&self) -> Option<&crate::recipe::fastly_tools::FastlyToolEvidence> {
        match &self.native_tools {
            NativeToolCoverage::ExplicitFastlyHostFiles { evidence } => Some(evidence),
            _ => None,
        }
    }
    /// Attaches measured selected Linux host files to the Fastly target.
    ///
    /// # Errors
    ///
    /// Rejects attachment to another target.
    pub(crate) fn with_fastly_tools(
        mut self,
        evidence: crate::recipe::fastly_tools::FastlyToolEvidence,
    ) -> Result<Self, Report<ReleaseError>> {
        if self.target != ReleaseTarget::Wasm32Wasip1.triple() {
            return Err(composition_error(
                "Fastly host tools belong to another target",
            ));
        }
        self.native_tools = NativeToolCoverage::ExplicitFastlyHostFiles {
            evidence: Box::new(evidence),
        };
        Ok(self)
    }
    /// Borrows the accepted executable's logical runtime requirements.
    pub(crate) fn external_requirements(&self) -> &ExternalRequirements {
        &self.external_requirements
    }
    /// Returns native tool roles whose host paths were never resolved or measured.
    pub(crate) fn unresolved_native_tool_roles(&self) -> &[&'static str] {
        match &self.native_tools {
            NativeToolCoverage::HostPathUnresolved { roles } => roles,
            NativeToolCoverage::ExplicitFastlyHostFiles { .. } => &["assembler", "build_tools"],
            NativeToolCoverage::ExplicitAppleFiles { .. }
            | NativeToolCoverage::ExplicitGnuImage { .. } => &[],
        }
    }
    /// Returns whether explicit Apple files lack a measured full tool closure.
    pub(crate) fn apple_tool_closure_unmeasured(&self) -> bool {
        matches!(
            self.native_tools,
            NativeToolCoverage::ExplicitAppleFiles {
                full_closure_measured: false
            }
        )
    }
    /// Returns whether material native coverage is recorded as complete.
    pub(crate) fn material_native_coverage_complete(&self) -> bool {
        self.material_native_coverage_complete
    }
    /// Attaches the owning GNU execution's measured installed-image evidence.
    ///
    /// # Errors
    ///
    /// Rejects attachment to another target; measurement constructors remain
    /// private to the actual denied recipe execution.
    pub(crate) fn with_gnu(
        mut self,
        evidence: crate::recipe::gnu::GnuEvidence,
    ) -> Result<Self, Report<ReleaseError>> {
        if self.target != ReleaseTarget::X86_64UnknownLinuxGnu.triple() {
            return Err(composition_error(
                "GNU image evidence belongs to another target",
            ));
        }
        self.native_tools = NativeToolCoverage::ExplicitGnuImage {
            evidence: Box::new(evidence),
        };
        Ok(self)
    }
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
        // The Apple evidence is copied from the recorded facts after one
        // verification inside `candidate_measurement`; nothing is read from
        // disk afterwards, so a second Apple pass here would rehash the same
        // SDK and toolchain without changing what the evidence states.
        let apple = tools
            .apple()
            .map(AppleDeveloperTools::candidate_measurement)
            .transpose()?;
        if tools.digest()? != bound_tools_digest {
            return Err(composition_error(
                "Rust tools changed during platform measurement",
            ));
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
pub(crate) mod tests;
