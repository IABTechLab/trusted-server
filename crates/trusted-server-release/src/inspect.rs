//! Pure inspection of final release executables.
//!
//! Each inspector reads only the final bytes of one artifact: it never runs
//! the artifact, rebuilds it or consults the build environment. A successful
//! inspection returns facts that record what was observed — loader, libraries,
//! version floors, Wasm import modules and the SHA-256 of the inspected bytes —
//! for later build evidence. Facts make no claim beyond that observation;
//! startup and runtime behaviour belong to the recipe and qualification
//! fixtures.
//!
//! Every inspector fails closed: an unsupported container, a truncated or
//! malformed structure, or any value outside the compiled-in policy rejects
//! the artifact with [`ReleaseError::Inspect`] and an attached [`Violation`]
//! naming the rule.
//!
//! - [`inspect_elf`]: `x86_64-unknown-linux-gnu` executables against the
//!   glibc 2.34 loader floor.
//! - [`inspect_macho`]: thin 64-bit macOS executables against the macOS 13.0
//!   deployment target.
//! - [`inspect_wasm`]: Fastly Compute core modules.

mod elf;
mod macho;
mod wasm;

use std::fmt;

use error_stack::Report;
use sha2::{Digest as _, Sha256};

use crate::error::ReleaseError;

// Only the tests consume the inspectors until the Task 9 recipes call them.
#[allow(unused_imports, reason = "consumed by Task 9 recipes")]
pub(crate) use self::{
    elf::{ElfPolicy, inspect_elf},
    macho::{MachoCpu, MachoPolicy, inspect_macho},
    wasm::inspect_wasm,
};

/// Absolute path prefixes that only exist on a build host or runner.
///
/// A final executable embedding any of them leaks the build environment
/// (unremapped source, home, temporary or workspace paths). The scan is
/// deliberately conservative: it matches anywhere in the file, not only in
/// loader strings, and a reviewed exception requires changing this list.
///
/// Matches are substrings, not path starts: Rust panic locations sit in
/// unterminated rodata, so the byte before a path is arbitrary. Entries are
/// therefore specific enough not to match ordinary crate-relative paths; a
/// bare `/private/` would match serde's `src/private/de.rs`.
pub(crate) const RUNNER_PATH_PREFIXES: &[&str] = &[
    "/home/",
    "/Users/",
    "/root/",
    "/private/var/",
    "/private/tmp/",
    "/var/folders/",
    "/tmp/",
    "/github/workspace/",
    "/__w/",
];

/// Maximum number of embedded-path offsets reported in a rejection.
const REPORTED_PATH_HITS: usize = 16;

/// The inspection rule an artifact violated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, derive_more::Display)]
pub(crate) enum Violation {
    /// The bytes are truncated, malformed or not the expected container.
    #[display("malformed or unsupported executable container")]
    Malformed,
    /// The container is valid but the class, CPU or file type is not the
    /// selected target's.
    #[display("executable architecture or file type does not match the target")]
    Architecture,
    /// The program interpreter or dynamic linker is missing or unexpected.
    #[display("program interpreter is missing or unexpected")]
    Interpreter,
    /// A linked library is outside the reviewed allowlist.
    #[display("linked library is outside the reviewed allowlist")]
    Library,
    /// A library link uses a kind other than a plain required load.
    #[display("library link kind is not a plain required load")]
    LinkKind,
    /// A symbol version requirement is unparsable, unexpected or above the
    /// compatibility ceiling.
    #[display("symbol version requirement is unparsable or exceeds the ceiling")]
    SymbolVersion,
    /// The executable carries a runtime library search path or loader
    /// environment.
    #[display("executable carries a runtime library search path")]
    SearchPath,
    /// The executable embeds a runner-only absolute path.
    #[display("executable embeds a runner-only absolute path")]
    EmbeddedPath,
    /// The deployment target is missing, ambiguous or not the selected floor.
    #[display("deployment target is missing or does not match the floor")]
    DeploymentTarget,
    /// The bytes are a Wasm component or not Wasm at all.
    #[display("artifact is not a core WebAssembly module")]
    WasmEncoding,
    /// The core module fails validation under the pinned feature set.
    #[display("core WebAssembly module fails validation")]
    WasmValidation,
    /// The module imports from an unreviewed module or a non-function item.
    #[display("WebAssembly import is outside the reviewed host ABI")]
    WasmImport,
}

/// Builds the inspection rejection carrying `violation` and a detail line.
fn rejected(violation: Violation, detail: impl fmt::Display) -> Report<ReleaseError> {
    Report::new(ReleaseError::Inspect)
        .attach(violation)
        .attach(detail.to_string())
}

/// Maps a parser error onto [`Violation::Malformed`].
fn malformed(context: &str, error: impl fmt::Display) -> Report<ReleaseError> {
    rejected(Violation::Malformed, format!("{context}: {error}"))
}

/// Returns the lowercase hex SHA-256 of `bytes`.
fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Decodes a loader string, rejecting non-UTF-8 bytes.
fn utf8<'a>(bytes: &'a [u8], context: &str) -> Result<&'a str, Report<ReleaseError>> {
    std::str::from_utf8(bytes).map_err(|error| malformed(context, error))
}

/// One occurrence of a denied path prefix in the inspected bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PathHit {
    prefix: &'static str,
    offset: usize,
}

/// Finds every occurrence of any `prefixes` entry anywhere in `bytes`.
///
/// # Performance
///
/// Candidates are found by scanning for `/` only, so the cost is one pass
/// plus a short comparison per slash rather than one window per prefix.
fn find_denied_paths(bytes: &[u8], prefixes: &[&'static str]) -> Vec<PathHit> {
    bytes
        .iter()
        .enumerate()
        .filter(|(_, byte)| **byte == b'/')
        .filter_map(|(offset, _)| {
            prefixes
                .iter()
                .find(|prefix| bytes[offset..].starts_with(prefix.as_bytes()))
                .map(|prefix| PathHit { prefix, offset })
        })
        .collect()
}

/// Rejects `bytes` if they embed any denied path prefix.
///
/// # Errors
///
/// Returns [`Violation::EmbeddedPath`] listing the match count and the first
/// [`REPORTED_PATH_HITS`] prefix/offset pairs.
fn reject_denied_paths(
    bytes: &[u8],
    prefixes: &[&'static str],
) -> Result<(), Report<ReleaseError>> {
    let hits = find_denied_paths(bytes, prefixes);
    if hits.is_empty() {
        return Ok(());
    }
    let shown = hits
        .iter()
        .take(REPORTED_PATH_HITS)
        .map(|hit| format!("{}@{:#x}", hit.prefix, hit.offset))
        .collect::<Vec<_>>()
        .join(", ");
    Err(rejected(
        Violation::EmbeddedPath,
        format!("{} runner-only path occurrence(s): {shown}", hits.len()),
    ))
}

/// A numeric `GLIBC_x.y[.z]` symbol version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct GlibcVersion {
    major: u32,
    minor: u32,
    patch: u32,
}

impl GlibcVersion {
    /// Creates `major.minor`.
    pub(crate) const fn new(major: u32, minor: u32) -> Self {
        Self {
            major,
            minor,
            patch: 0,
        }
    }

    /// Parses `x.y` or `x.y.z` made of plain decimal components.
    ///
    /// Signs, empty components, leading zeros and more than three
    /// components are rejected, so names such as `GLIBC_PRIVATE` or
    /// `GLIBC_ABI_DT_RELR` never compare as versions.
    pub(crate) fn parse(text: &str) -> Option<Self> {
        let parts = text.split('.').map(decimal).collect::<Option<Vec<_>>>()?;
        match parts.as_slice() {
            [major, minor] => Some(Self::new(*major, *minor)),
            [major, minor, patch] => Some(Self {
                major: *major,
                minor: *minor,
                patch: *patch,
            }),
            _ => None,
        }
    }
}

impl fmt::Display for GlibcVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}", self.major, self.minor)?;
        if self.patch != 0 {
            write!(formatter, ".{}", self.patch)?;
        }
        Ok(())
    }
}

/// Parses one canonical unsigned decimal component.
fn decimal(text: &str) -> Option<u32> {
    let canonical = !text.is_empty()
        && text.bytes().all(|byte| byte.is_ascii_digit())
        && (text == "0" || !text.starts_with('0'));
    if canonical { text.parse().ok() } else { None }
}

/// One `(library, version name)` pair from `.gnu.version_r`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct VersionRequirement {
    file: String,
    name: String,
}

/// Facts observed in an accepted ELF executable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ElfFacts {
    sha256: String,
    interpreter: String,
    needed: Vec<String>,
    version_requirements: Vec<VersionRequirement>,
    max_glibc: GlibcVersion,
}

impl ElfFacts {
    /// Lowercase hex SHA-256 of the inspected bytes.
    pub(crate) fn sha256(&self) -> &str {
        &self.sha256
    }

    /// Observed `PT_INTERP` path.
    pub(crate) fn interpreter(&self) -> &str {
        &self.interpreter
    }

    /// Observed `DT_NEEDED` entries in file order.
    pub(crate) fn needed(&self) -> &[String] {
        &self.needed
    }

    /// Observed `.gnu.version_r` requirements, sorted.
    pub(crate) fn version_requirements(&self) -> &[VersionRequirement] {
        &self.version_requirements
    }

    /// Highest observed `GLIBC_` requirement.
    pub(crate) fn max_glibc(&self) -> GlibcVersion {
        self.max_glibc
    }
}

/// A macOS `X.Y.Z` version as encoded in `LC_BUILD_VERSION`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, derive_more::Display)]
#[display("{major}.{minor}.{patch}")]
pub(crate) struct MacosVersion {
    major: u16,
    minor: u8,
    patch: u8,
}

impl MacosVersion {
    /// Creates `major.minor.0`.
    pub(crate) const fn new(major: u16, minor: u8) -> Self {
        Self {
            major,
            minor,
            patch: 0,
        }
    }

    /// Decodes the `xxxx.yy.zz` nibble encoding.
    fn from_encoded(encoded: u32) -> Self {
        let [high, low, minor, patch] = encoded.to_be_bytes();
        Self {
            major: u16::from_be_bytes([high, low]),
            minor,
            patch,
        }
    }
}

/// Facts observed in an accepted Mach-O executable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MachoFacts {
    sha256: String,
    cpu: MachoCpu,
    minos: MacosVersion,
    sdk: MacosVersion,
    dylibs: Vec<String>,
}

impl MachoFacts {
    /// Lowercase hex SHA-256 of the inspected bytes.
    pub(crate) fn sha256(&self) -> &str {
        &self.sha256
    }

    /// Observed CPU type.
    pub(crate) fn cpu(&self) -> MachoCpu {
        self.cpu
    }

    /// Observed `LC_BUILD_VERSION` minimum OS version.
    pub(crate) fn minos(&self) -> MacosVersion {
        self.minos
    }

    /// Observed `LC_BUILD_VERSION` SDK version.
    pub(crate) fn sdk(&self) -> MacosVersion {
        self.sdk
    }

    /// Observed `LC_LOAD_DYLIB` install names in load-command order.
    pub(crate) fn dylibs(&self) -> &[String] {
        &self.dylibs
    }
}

/// Facts observed in an accepted core Wasm module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WasmFacts {
    sha256: String,
    import_modules: Vec<String>,
    function_imports: usize,
}

impl WasmFacts {
    /// Lowercase hex SHA-256 of the inspected bytes.
    pub(crate) fn sha256(&self) -> &str {
        &self.sha256
    }

    /// Distinct import module names, sorted.
    pub(crate) fn import_modules(&self) -> &[String] {
        &self.import_modules
    }

    /// Total number of imported functions.
    pub(crate) fn function_imports(&self) -> usize {
        self.function_imports
    }
}

#[cfg(test)]
mod tests;
