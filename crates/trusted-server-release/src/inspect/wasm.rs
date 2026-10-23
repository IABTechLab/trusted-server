//! Core WebAssembly inspection for the Fastly Compute artifact.

use std::collections::BTreeSet;

use error_stack::Report;
use wasmparser::types::{EntityType, Types};
use wasmparser::{CompositeInnerType, Parser, Payload, TypeRef, Validator, WasmFeatures};

use super::{
    Violation, WasmCustomSection, WasmExport, WasmExportKind, WasmFacts, rejected, sha256_hex,
};
use crate::error::ReleaseError;

/// Reviewed import modules of the Fastly Compute host ABI.
///
/// Observed in release builds of `trusted-server-adapter-fastly`: WASI
/// preview 1 plus these `fastly_*` hostcall modules. The locked SDK 0.13.1
/// also uses the reviewed asynchronous I/O module supported by Viceroy 0.21.0.
/// Adding a module changes the host ABI policy and requires review.
pub(crate) const FASTLY_IMPORT_MODULES: &[&str] = &[
    "fastly_async_io",
    "fastly_cache",
    "fastly_compute_runtime",
    "fastly_config_store",
    "fastly_erl",
    "fastly_geo",
    "fastly_http_body",
    "fastly_http_cache",
    "fastly_http_downstream",
    "fastly_http_req",
    "fastly_http_resp",
    "fastly_image_optimizer",
    "fastly_kv_store",
    "fastly_log",
    "fastly_purge",
    "fastly_secret_store",
    "wasi_snapshot_preview1",
];

/// Pinned validation feature set: the WebAssembly 2.0 specification.
const FEATURES: WasmFeatures = WasmFeatures::WASM2;

/// Inspects a final Fastly Compute Wasm artifact.
///
/// # Errors
///
/// Returns [`ReleaseError::Inspect`] with a [`Violation`] when the bytes are
/// a component or not a core module ([`Violation::WasmEncoding`]), fail
/// validation under [`WasmFeatures::WASM2`] ([`Violation::WasmValidation`]),
/// or import a non-function item or from a module outside
/// [`FASTLY_IMPORT_MODULES`] ([`Violation::WasmImport`]).
pub(crate) fn inspect_wasm(bytes: &[u8]) -> Result<WasmFacts, Report<ReleaseError>> {
    if Parser::is_component(bytes) {
        return Err(rejected(
            Violation::WasmEncoding,
            "component, not a core module",
        ));
    }
    if !Parser::is_core_wasm(bytes) {
        return Err(rejected(
            Violation::WasmEncoding,
            "missing core module header",
        ));
    }
    let types = Validator::new_with_features(FEATURES)
        .validate_all(bytes)
        .map_err(|error| rejected(Violation::WasmValidation, error))?;
    let exports = exports(&types)?;
    let (import_modules, function_imports) = imports(bytes)?;
    if let Some(module) = import_modules
        .iter()
        .find(|module| !FASTLY_IMPORT_MODULES.contains(&module.as_str()))
    {
        return Err(rejected(
            Violation::WasmImport,
            format!("import module {module:?}"),
        ));
    }
    Ok(WasmFacts {
        sha256: sha256_hex(bytes),
        import_modules: import_modules.into_iter().collect(),
        function_imports,
        custom_sections: custom_sections(bytes)?,
        exports,
    })
}

/// Returns every export of the validated module in module order.
fn exports(types: &Types) -> Result<Vec<WasmExport>, Report<ReleaseError>> {
    let types = types.as_ref();
    let Some(exports) = types.core_exports() else {
        return Err(rejected(
            Violation::WasmEncoding,
            "validated types describe a component",
        ));
    };
    Ok(exports
        .map(|(name, entity)| {
            let kind = match entity {
                EntityType::Func(id) | EntityType::FuncExact(id) => {
                    match &types[id].composite_type.inner {
                        CompositeInnerType::Func(function) => WasmExportKind::Function {
                            params: function.params().len(),
                            results: function.results().len(),
                        },
                        _ => WasmExportKind::Other,
                    }
                }
                EntityType::Memory(_) => WasmExportKind::Memory,
                EntityType::Table(_) | EntityType::Global(_) | EntityType::Tag(_) => {
                    WasmExportKind::Other
                }
            };
            WasmExport {
                name: name.to_owned(),
                kind,
            }
        })
        .collect())
}

/// Returns every custom section in file order.
fn custom_sections(bytes: &[u8]) -> Result<Vec<WasmCustomSection>, Report<ReleaseError>> {
    let mut sections = Vec::new();
    for payload in Parser::new(0).parse_all(bytes) {
        let payload = payload.map_err(|error| rejected(Violation::WasmValidation, error))?;
        if let Payload::CustomSection(reader) = payload {
            sections.push(WasmCustomSection {
                name: reader.name().to_owned(),
                size: reader.data().len(),
            });
        }
    }
    Ok(sections)
}

/// Returns the distinct import modules and the function import count.
fn imports(bytes: &[u8]) -> Result<(BTreeSet<String>, usize), Report<ReleaseError>> {
    let parse_error = |error| rejected(Violation::WasmValidation, error);
    let mut modules = BTreeSet::new();
    let mut functions = 0;
    for payload in Parser::new(0).parse_all(bytes) {
        let Payload::ImportSection(reader) = payload.map_err(parse_error)? else {
            continue;
        };
        for import in reader.into_imports() {
            let import = import.map_err(parse_error)?;
            if !matches!(import.ty, TypeRef::Func(_)) {
                return Err(rejected(
                    Violation::WasmImport,
                    format!("non-function import {}::{}", import.module, import.name),
                ));
            }
            modules.insert(import.module.to_owned());
            functions += 1;
        }
    }
    Ok((modules, functions))
}
