//! Logical runtime requirements observed in inspected final executable bytes.

use serde::{Serialize, Serializer};

use crate::inspect::{ElfFacts, MachoCpu, MachoFacts, VersionRequirement, WasmFacts};
use crate::policy::ReleaseTarget;

/// Observed external requirements, without installed-runtime provenance claims.
#[derive(Debug, Serialize)]
pub(crate) struct ExternalRequirements {
    #[serde(serialize_with = "serialize_target")]
    target: ReleaseTarget,
    artifact_sha256: String,
    runtime: Runtime,
}

#[derive(Debug, Serialize)]
#[serde(tag = "format", rename_all = "snake_case")]
pub(crate) enum Runtime {
    Elf {
        interpreter: String,
        needed: Vec<String>,
        version_requirements: Vec<VersionRequirement>,
        max_glibc: String,
    },
    Macho {
        loader: String,
        cpu: String,
        dylibs: Vec<String>,
        minimum_os: String,
        sdk: String,
    },
    Wasm {
        import_modules: Vec<String>,
    },
}

fn serialize_target<S: Serializer>(
    target: &ReleaseTarget,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(target.triple())
}

impl ExternalRequirements {
    /// Borrows exact logical requirements without installed-runtime identity.
    pub(crate) fn runtime(&self) -> &Runtime {
        &self.runtime
    }
    /// Returns the SDK recorded in a Mach-O executable, when present.
    pub(crate) fn macho_sdk(&self) -> Option<&str> {
        match &self.runtime {
            Runtime::Macho { sdk, .. } => Some(sdk),
            Runtime::Elf { .. } | Runtime::Wasm { .. } => None,
        }
    }

    /// Retains logical requirements from accepted GNU/Linux executable facts.
    pub(crate) fn from_elf(facts: &ElfFacts) -> Self {
        Self {
            target: ReleaseTarget::X86_64UnknownLinuxGnu,
            artifact_sha256: facts.sha256().to_owned(),
            runtime: Runtime::Elf {
                interpreter: facts.interpreter().to_owned(),
                needed: facts.needed().to_vec(),
                version_requirements: facts.version_requirements().to_vec(),
                max_glibc: facts.max_glibc().to_string(),
            },
        }
    }

    /// Retains logical requirements from accepted macOS executable facts.
    pub(crate) fn from_macho(facts: &MachoFacts) -> Self {
        Self {
            target: match facts.cpu() {
                MachoCpu::Arm64 => ReleaseTarget::Aarch64AppleDarwin,
                MachoCpu::X86_64 => ReleaseTarget::X86_64AppleDarwin,
            },
            artifact_sha256: facts.sha256().to_owned(),
            runtime: Runtime::Macho {
                loader: facts.loader().to_owned(),
                cpu: facts.cpu().to_string(),
                dylibs: facts.dylibs().to_vec(),
                minimum_os: facts.minos().to_string(),
                sdk: facts.sdk().to_string(),
            },
        }
    }

    /// Retains logical host imports from accepted Fastly core Wasm facts.
    pub(crate) fn from_wasm(facts: &WasmFacts) -> Self {
        Self {
            target: ReleaseTarget::Wasm32Wasip1,
            artifact_sha256: facts.sha256().to_owned(),
            runtime: Runtime::Wasm {
                import_modules: facts.import_modules().to_vec(),
            },
        }
    }

    /// Target derived from the accepted executable format and CPU.
    pub(crate) fn target(&self) -> ReleaseTarget {
        self.target
    }

    /// SHA-256 computed by the inspector over the accepted executable bytes.
    pub(crate) fn artifact_sha256(&self) -> &str {
        &self.artifact_sha256
    }
}

#[cfg(test)]
mod tests;
