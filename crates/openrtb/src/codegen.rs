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

        // Enum derives: strip prost::Enumeration (no runtime prost dependency),
        // add serde. Do NOT add Default — the `#[repr(i32)]` enums in this
        // proto are catalog/documentation enums not used as struct fields.
        if trimmed.contains("::prost::Enumeration)]") && trimmed.starts_with("#[derive(") {
            let indent = leading_whitespace(line);
            output.push_str(&format!(
                "{indent}#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, \
                 PartialOrd, Ord, ::serde::Serialize, ::serde::Deserialize)]\n",
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
                if is_bool_prost_attr(trimmed) && is_option_bool_field(next_trimmed) {
                    // OpenRTB JSON uses 0/1 for boolean fields, not true/false.
                    output.push_str(&format!(
                        "{indent}#[serde(with = \"crate::bool_as_int\", \
                         skip_serializing_if = \"Option::is_none\")]\n"
                    ));
                } else if is_option_field(next_trimmed) {
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

        if trimmed.starts_with("pub struct ")
            && trimmed.ends_with('{')
            && let Some(name) = extract_struct_name(trimmed)
            && EXT_STRUCTS.contains(&name)
        {
            struct_ext_stack.push(brace_depth + 1);
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
        if trimmed == "}"
            && let Some(&expected_depth) = struct_ext_stack.last()
            && brace_depth + 1 == expected_depth
        {
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

        output.push_str(line);
        output.push('\n');
        i += 1;
    }

    // Replace prost alloc types with std equivalents so the generated code
    // does not depend on the prost crate at runtime.
    output
        .replace("::prost::alloc::string::String", "::std::string::String")
        .replace("::prost::alloc::vec::Vec", "::std::vec::Vec")
        .replace("::prost::alloc::boxed::Box", "::std::boxed::Box")
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

/// Returns true if the prost attribute declares a `bool` field.
fn is_bool_prost_attr(trimmed: &str) -> bool {
    trimmed.starts_with("#[prost(bool,")
}

/// Returns true if the line declares an `Option<bool>` field.
fn is_option_bool_field(trimmed: &str) -> bool {
    trimmed.starts_with("pub ") && trimmed.contains("::core::option::Option<bool>")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal prost-like struct output to verify derive replacement and ext
    /// injection.
    const PROST_STRUCT: &str = "\
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct BidRequest {
    #[prost(string, optional, tag = \"1\")]
    pub id: ::core::option::Option<::prost::alloc::string::String>,
    #[prost(message, repeated, tag = \"2\")]
    pub imp: ::prost::alloc::vec::Vec<Imp>,
}
";

    #[test]
    fn replaces_struct_derives_with_serde() {
        let output = postprocess(PROST_STRUCT);
        assert!(
            output.contains("::serde::Serialize, ::serde::Deserialize"),
            "should add serde derives: {output}"
        );
        assert!(
            !output.contains("::prost::Message"),
            "should strip prost::Message: {output}"
        );
        assert!(
            output.contains("#[serde(default)]"),
            "should add #[serde(default)]: {output}"
        );
    }

    #[test]
    fn strips_prost_field_attrs_and_injects_serde_skip() {
        let output = postprocess(PROST_STRUCT);
        assert!(
            !output.contains("#[prost("),
            "should strip all #[prost(...)] attrs: {output}"
        );
        assert!(
            output.contains("skip_serializing_if = \"Option::is_none\""),
            "should add skip_serializing_if for Option fields: {output}"
        );
        assert!(
            output.contains("skip_serializing_if = \"Vec::is_empty\""),
            "should add skip_serializing_if for Vec fields: {output}"
        );
    }

    #[test]
    fn injects_ext_field_for_ext_structs() {
        let output = postprocess(PROST_STRUCT);
        assert!(
            output.contains("pub ext: ::core::option::Option<"),
            "should inject ext field for BidRequest: {output}"
        );
    }

    #[test]
    fn does_not_inject_ext_for_unlisted_structs() {
        let input = "\
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct Unlisted {
    #[prost(string, optional, tag = \"1\")]
    pub name: ::core::option::Option<::prost::alloc::string::String>,
}
";
        let output = postprocess(input);
        assert!(
            !output.contains("pub ext:"),
            "should not inject ext for unlisted struct: {output}"
        );
    }

    /// Verify enum derives strip prost::Enumeration and add serde.
    const PROST_ENUM: &str = "\
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, ::prost::Enumeration)]
#[repr(i32)]
pub enum BannerAdType {
    Unknown = 0,
    XhtmlTextAd = 1,
}
";

    #[test]
    fn replaces_enum_derives_without_prost() {
        let output = postprocess(PROST_ENUM);
        assert!(
            !output.contains("::prost::Enumeration"),
            "should strip prost::Enumeration: {output}"
        );
        assert!(
            output.contains("::serde::Serialize, ::serde::Deserialize"),
            "should add serde derives to enums: {output}"
        );
        assert!(
            output.contains("#[repr(i32)]"),
            "should preserve #[repr(i32)]: {output}"
        );
    }

    #[test]
    fn bool_field_gets_bool_as_int_serde_module() {
        let input = "\
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct Regs {
    #[prost(bool, optional, tag = \"1\")]
    pub coppa: ::core::option::Option<bool>,
}
";
        let output = postprocess(input);
        assert!(
            output.contains("crate::bool_as_int"),
            "should use bool_as_int for Option<bool> fields: {output}"
        );
    }

    #[test]
    fn oneof_derives_strip_prost_oneof() {
        let input = "\
#[derive(Clone, PartialEq, ::prost::Oneof)]
pub enum DistributionChannel {
    Site(Site),
    App(App),
}
";
        let output = postprocess(input);
        assert!(
            !output.contains("::prost::Oneof"),
            "should strip prost::Oneof: {output}"
        );
        assert!(
            output.contains("::serde::Serialize"),
            "should add serde to oneof: {output}"
        );
    }
}
