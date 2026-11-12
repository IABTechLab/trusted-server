//! Pure inspection of final Fastly Wasm bytes.
mod wasm;
use crate::error::ReleaseError;
use error_stack::Report;
use sha2::{Digest as _, Sha256};
use std::fmt;
pub(crate) use wasm::inspect_wasm;

/// The Wasm inspection rule violated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, derive_more::Display)]
pub(crate) enum Violation {
    #[display("artifact is not a core WebAssembly module")]
    Encoding,
    #[display("core WebAssembly module fails validation")]
    Validation,
    #[display("WebAssembly import is outside the reviewed host ABI")]
    Import,
    #[display("WebAssembly module lacks a required nonempty debug section")]
    #[cfg(test)]
    DebugInfo,
    #[display("WebAssembly module lacks the Fastly Compute entry ABI")]
    #[cfg(test)]
    Abi,
}

fn rejected(violation: Violation, detail: impl fmt::Display) -> Report<ReleaseError> {
    Report::new(ReleaseError::Inspect)
        .attach(violation)
        .attach(detail.to_string())
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// One custom section observed in a core Wasm module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WasmCustomSection {
    name: String,
    size: usize,
}

#[cfg(test)]
impl WasmCustomSection {
    /// Section name as encoded in the module.
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// Payload length in bytes, excluding the name.
    pub(crate) fn size(&self) -> usize {
        self.size
    }
}

/// The kind of one core Wasm export, with a function's arity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WasmExportKind {
    /// A function taking `params` values and returning `results` values.
    Function { params: usize, results: usize },
    /// A linear memory.
    Memory,
    /// A table, global or tag.
    Other,
}

/// One export observed in a core Wasm module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WasmExport {
    name: String,
    kind: WasmExportKind,
}

#[cfg(test)]
impl WasmExport {
    /// Export name as encoded in the module.
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// Kind of the exported item.
    pub(crate) fn kind(&self) -> WasmExportKind {
        self.kind
    }
}

/// Facts observed in an accepted core Wasm module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WasmFacts {
    sha256: String,
    import_modules: Vec<String>,
    function_imports: usize,
    custom_sections: Vec<WasmCustomSection>,
    exports: Vec<WasmExport>,
}

/// DWARF sections whose nonempty presence proves a Wasm module or ELF
/// executable was not stripped.
#[cfg(test)]
const REQUIRED_DEBUG_SECTIONS: [&str; 2] = [".debug_info", ".debug_line"];

/// Hostcall modules a Fastly program must import to receive the downstream
/// request and send its response.
#[cfg(test)]
const REQUIRED_FASTLY_IMPORT_MODULES: [&str; 2] = ["fastly_http_req", "fastly_http_resp"];

#[cfg(test)]
impl WasmFacts {
    /// Exports in module order.
    pub(crate) fn exports(&self) -> &[WasmExport] {
        &self.exports
    }

    /// Requires the Fastly Compute WASI command entry ABI.
    ///
    /// The module must export a `_start` function taking and returning
    /// nothing and a `memory`, and import the downstream request and response
    /// hostcall modules. Import modules outside the reviewed host ABI were
    /// already rejected by [`inspect_wasm`].
    ///
    /// # Errors
    ///
    /// Returns [`Violation::Abi`] naming the first missing or mistyped
    /// requirement.
    pub(crate) fn require_fastly_entry_abi(&self) -> Result<(), Report<ReleaseError>> {
        let kind = |name: &str| {
            self.exports
                .iter()
                .find(|export| export.name == name)
                .map(|export| export.kind)
        };
        match kind("_start") {
            Some(WasmExportKind::Function {
                params: 0,
                results: 0,
            }) => {}
            other => {
                return Err(rejected(
                    Violation::Abi,
                    format!("export _start must be a nullary function, found {other:?}"),
                ));
            }
        }
        if kind("memory") != Some(WasmExportKind::Memory) {
            return Err(rejected(
                Violation::Abi,
                "export memory must be a linear memory",
            ));
        }
        if let Some(missing) = REQUIRED_FASTLY_IMPORT_MODULES
            .iter()
            .find(|module| !self.import_modules.iter().any(|seen| seen == *module))
        {
            return Err(rejected(
                Violation::Abi,
                format!("missing required import module {missing}"),
            ));
        }
        Ok(())
    }

    /// Custom sections in file order.
    pub(crate) fn custom_sections(&self) -> &[WasmCustomSection] {
        &self.custom_sections
    }

    /// Requires nonempty `.debug_info` and `.debug_line` custom sections.
    ///
    /// # Errors
    ///
    /// Returns [`Violation::DebugInfo`] naming the first required section
    /// that is absent or has an empty payload in every occurrence.
    pub(crate) fn require_debug_sections(&self) -> Result<(), Report<ReleaseError>> {
        for required in REQUIRED_DEBUG_SECTIONS {
            let present = self
                .custom_sections
                .iter()
                .any(|section| section.name == required && section.size > 0);
            if !present {
                return Err(rejected(
                    Violation::DebugInfo,
                    format!("missing or empty custom section {required}"),
                ));
            }
        }
        Ok(())
    }

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
pub(crate) mod tests;
