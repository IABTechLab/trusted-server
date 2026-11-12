//! Hand-built Wasm fixtures.

pub(crate) fn wasm_module(imports: &[(&str, &str)]) -> Vec<u8> {
    let mut out = b"\0asm\x01\0\0\0".to_vec();
    if imports.is_empty() {
        return out;
    }
    section(&mut out, 1, &[1, 0x60, 0, 0]);
    let mut body = vec![u8::try_from(imports.len()).expect("should fit")];
    for (module, name) in imports {
        name_bytes(&mut body, module);
        name_bytes(&mut body, name);
        body.extend_from_slice(&[0, 0]);
    }
    section(&mut out, 2, &body);
    out
}

/// Appends a section with a single-byte LEB128 length.
pub(super) fn section(out: &mut Vec<u8>, id: u8, body: &[u8]) {
    out.push(id);
    out.push(u8::try_from(body.len()).expect("should fit one LEB128 byte"));
    out.extend_from_slice(body);
}

fn name_bytes(out: &mut Vec<u8>, text: &str) {
    out.push(u8::try_from(text.len()).expect("should fit one LEB128 byte"));
    out.extend_from_slice(text.as_bytes());
}

/// Builds a core Wasm module importing `() -> ()` functions, then appending
/// custom sections `(name, payload)` in order.
pub(super) fn wasm_module_with_customs(
    imports: &[(&str, &str)],
    customs: &[(&str, &[u8])],
) -> Vec<u8> {
    append_customs(wasm_module(imports), customs)
}

/// Appends custom sections `(name, payload)` in order to `module`.
pub(crate) fn append_customs(mut module: Vec<u8>, customs: &[(&str, &[u8])]) -> Vec<u8> {
    for (name, payload) in customs {
        let mut body = Vec::new();
        name_bytes(&mut body, name);
        body.extend_from_slice(payload);
        section(&mut module, 0, &body);
    }
    module
}

/// The `_start` export shape of a [`command_module`].
#[derive(Clone, Copy)]
pub(crate) enum Entry {
    /// `_start` takes and returns nothing.
    Nullary,
    /// `_start` takes one `i32`.
    TakesI32,
    /// The function is exported as `main`, not `_start`.
    Renamed,
}

/// Builds a WASI-command-shaped core module importing `() -> ()` functions,
/// defining one function exported per `entry`, one memory exported as
/// `memory` when `export_memory` is set, and empty bodies.
pub(crate) fn command_module(
    imports: &[(&str, &str)],
    entry: Entry,
    export_memory: bool,
) -> Vec<u8> {
    let mut out = b"\0asm\x01\0\0\0".to_vec();
    section(&mut out, 1, &[2, 0x60, 0, 0, 0x60, 1, 0x7f, 0]);
    if !imports.is_empty() {
        let mut body = vec![u8::try_from(imports.len()).expect("should fit")];
        for (module, name) in imports {
            name_bytes(&mut body, module);
            name_bytes(&mut body, name);
            body.extend_from_slice(&[0, 0]);
        }
        section(&mut out, 2, &body);
    }
    let entry_type = match entry {
        Entry::Nullary | Entry::Renamed => 0,
        Entry::TakesI32 => 1,
    };
    section(&mut out, 3, &[1, entry_type]);
    section(&mut out, 5, &[1, 0, 1]);
    let entry_index = u8::try_from(imports.len()).expect("should fit");
    let mut exports = vec![if export_memory { 2 } else { 1 }];
    name_bytes(
        &mut exports,
        match entry {
            Entry::Renamed => "main",
            Entry::Nullary | Entry::TakesI32 => "_start",
        },
    );
    exports.extend_from_slice(&[0, entry_index]);
    if export_memory {
        name_bytes(&mut exports, "memory");
        exports.extend_from_slice(&[2, 0]);
    }
    section(&mut out, 7, &exports);
    section(&mut out, 10, &[1, 2, 0, 0x0b]);
    out
}
