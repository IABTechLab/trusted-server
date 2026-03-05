//! Build script that compiles the `OpenRTB` proto file with prost-build, then
//! post-processes the generated Rust code to:
//!
//! 1. Strip prost runtime derives and attributes (we only use JSON, not
//!    protobuf binary encoding).
//! 2. Add serde `Serialize` / `Deserialize` derives with `skip_serializing_if`
//!    annotations for `Option` and `Vec` fields.
//! 3. Inject `ext: Option<serde_json::Map<String, Value>>` into structs that
//!    correspond to extensible `OpenRTB` objects.

use std::fs;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=proto/openrtb.proto");

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("should have OUT_DIR"));

    // Phase 1: Compile proto with prost-build.
    prost_build::Config::new()
        .out_dir(&out_dir)
        .compile_protos(&["proto/openrtb.proto"], &["proto/"])
        .expect("should compile openrtb.proto");

    // Phase 2: Post-process the generated file.
    let generated_path = out_dir.join("com.iabtechlab.openrtb.v2.rs");
    let code = fs::read_to_string(&generated_path).expect("should read generated file");
    let processed = postprocess(&code);
    fs::write(&generated_path, processed).expect("should write processed file");
}

/// Names of structs that should receive an `ext` field.
const EXT_STRUCTS: &[&str] = &[
    "BidRequest",
    "Source",
    "SupplyChain",
    "SupplyChainNode",
    "Imp",
    "Metric",
    "Banner",
    "Format",
    "Video",
    "Audio",
    "Native",
    "Qty",
    "Refresh",
    "RefSettings",
    "Pmp",
    "Deal",
    "Site",
    "App",
    "Dooh",
    "Publisher",
    "Content",
    "Producer",
    "Network",
    "Channel",
    "Device",
    "Geo",
    "UserAgent",
    "BrandVersion",
    "User",
    "Eid",
    "Uid",
    "Data",
    "Segment",
    "Regs",
    "DurFloors",
    "BidResponse",
    "SeatBid",
    "Bid",
    "Transparency",
];

/// Post-process prost-generated Rust code.
///
/// Strips `::prost::Message` derives and `#[prost(...)]` field attributes (we
/// don't need protobuf binary encoding), then adds serde derives, skip
/// annotations, and `ext` fields.
fn postprocess(code: &str) -> String {
    let mut output = String::with_capacity(code.len() * 2);
    let lines: Vec<&str> = code.lines().collect();
    let len = lines.len();

    // Stack tracking for ext injection: when we enter a struct that needs ext,
    // push the brace depth at entry. On the matching close brace we inject.
    let mut struct_ext_stack: Vec<usize> = Vec::new();
    let mut brace_depth: usize = 0;

    let mut i = 0;
    while i < len {
        let line = lines[i];
        let trimmed = line.trim();

        // --- Derive replacement ---

        // Struct derives: replace prost::Message with Default + serde.
        // Handles both `Clone, PartialEq` and `Clone, Copy, PartialEq`.
        // Remove Copy since ext fields (Map<String, Value>) are not Copy.
        if trimmed.contains("::prost::Message)]") && trimmed.starts_with("#[derive(Clone,") {
            let indent = leading_whitespace(line);
            output.push_str(&format!(
                "{indent}#[derive(Clone, Debug, Default, PartialEq, \
                 ::serde::Serialize, ::serde::Deserialize)]\n"
            ));
            output.push_str(&format!("{indent}#[serde(default)]\n"));
            i += 1;
            continue;
        }

        // Enum derives: keep prost::Enumeration (provides Default + From<i32>),
        // add serde. Do NOT add Default — prost::Enumeration provides it.
        if trimmed.contains("::prost::Enumeration)]") && trimmed.starts_with("#[derive(") {
            let indent = leading_whitespace(line);
            output.push_str(&format!(
                "{indent}#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, \
                 PartialOrd, Ord, ::prost::Enumeration, \
                 ::serde::Serialize, ::serde::Deserialize)]\n",
            ));
            i += 1;
            continue;
        }

        // Oneof derives: strip prost::Oneof, add serde.
        if trimmed.contains("::prost::Oneof)]") && trimmed.starts_with("#[derive(") {
            let indent = leading_whitespace(line);
            output.push_str(&format!(
                "{indent}#[derive(Clone, Debug, PartialEq, \
                 ::serde::Serialize, ::serde::Deserialize)]\n",
            ));
            i += 1;
            continue;
        }

        // --- Strip #[prost(...)] attributes, inject serde attributes ---

        if is_prost_attr(trimmed) {
            // Look ahead to the next line to determine serde annotation.
            if i + 1 < len {
                let next_trimmed = lines[i + 1].trim();
                let indent = leading_whitespace(lines[i + 1]);
                if is_option_field(next_trimmed) {
                    output.push_str(&format!(
                        "{indent}#[serde(skip_serializing_if = \"Option::is_none\")]\n"
                    ));
                } else if is_vec_field(next_trimmed) {
                    output.push_str(&format!(
                        "{indent}#[serde(skip_serializing_if = \"Vec::is_empty\", default)]\n"
                    ));
                } else if is_hashmap_field(next_trimmed) {
                    output.push_str(&format!(
                        "{indent}#[serde(skip_serializing_if = \
                         \"::std::collections::HashMap::is_empty\", default)]\n"
                    ));
                }
                // else: plain scalar field — no serde attribute needed.
            }
            // Skip the #[prost(...)] line entirely.
            i += 1;
            continue;
        }

        // --- Track struct entries for ext injection ---

        if trimmed.starts_with("pub struct ") && trimmed.ends_with('{') {
            if let Some(name) = extract_struct_name(trimmed) {
                if EXT_STRUCTS.contains(&name) {
                    struct_ext_stack.push(brace_depth + 1);
                }
            }
        }

        // Count braces on the current line.
        for ch in trimmed.chars() {
            match ch {
                '{' => brace_depth += 1,
                '}' => brace_depth = brace_depth.saturating_sub(1),
                _ => {}
            }
        }

        // Check if this closing brace matches a struct that needs ext.
        if trimmed == "}" {
            if let Some(&expected_depth) = struct_ext_stack.last() {
                if brace_depth + 1 == expected_depth {
                    struct_ext_stack.pop();
                    let indent = leading_whitespace(line);
                    let field_indent = format!("{indent}    ");
                    output.push_str(&format!(
                        "{field_indent}/// Placeholder for exchange-specific extensions to OpenRTB.\n"
                    ));
                    output.push_str(&format!(
                        "{field_indent}#[serde(skip_serializing_if = \"Option::is_none\")]\n"
                    ));
                    output.push_str(&format!(
                        "{field_indent}pub ext: ::core::option::Option<\
                         ::serde_json::Map<::std::string::String, ::serde_json::Value>>,\n"
                    ));
                }
            }
        }

        output.push_str(line);
        output.push('\n');
        i += 1;
    }

    output
}

/// Extract leading whitespace from a line.
fn leading_whitespace(line: &str) -> &str {
    let trimmed_len = line.trim_start().len();
    &line[..line.len() - trimmed_len]
}

/// Extract struct name from a line like `pub struct Foo {`.
fn extract_struct_name(trimmed: &str) -> Option<&str> {
    let after = trimmed.strip_prefix("pub struct ")?;
    let end = after.find(|c: char| !c.is_alphanumeric() && c != '_')?;
    Some(&after[..end])
}

/// Returns true if the line declares an `Option<...>` field.
fn is_option_field(trimmed: &str) -> bool {
    trimmed.starts_with("pub ") && trimmed.contains("::core::option::Option<")
}

/// Returns true if the line declares a `Vec<...>` field (not inside Option).
fn is_vec_field(trimmed: &str) -> bool {
    trimmed.starts_with("pub ")
        && trimmed.contains("::prost::alloc::vec::Vec<")
        && !trimmed.contains("Option<")
}

/// Returns true if the line declares a `HashMap<...>` field.
fn is_hashmap_field(trimmed: &str) -> bool {
    trimmed.starts_with("pub ") && trimmed.contains("::std::collections::HashMap<")
}

/// Returns true if the line is a `#[prost(...)]` attribute.
fn is_prost_attr(trimmed: &str) -> bool {
    trimmed.starts_with("#[prost(")
}
