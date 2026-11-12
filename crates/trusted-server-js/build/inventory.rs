//! Strict, bounded parsing of the prebuilt input inventory.
//!
//! Contribution, tool and material values are opaque here: composition joins
//! belong to the release producer, rather than the Rust input consumer.

use std::collections::BTreeSet;
use std::fmt;
#[cfg(not(unix))]
use std::fs::File;
#[cfg(unix)]
use std::fs::OpenOptions;
use std::io::{self, Read as _};
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::path::Path;

use serde::Deserialize;
use serde::de::{DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Number, Value};
use sha2::{Digest as _, Sha256};

use super::bundle_set::{bundle_file_name, check_bundle_set, safe_module_id};

const MAX_INVENTORY: u64 = 1024 * 1024;
// Release producers must enforce the same per-module input budget.
pub(crate) const MAX_BUNDLE: u64 = 32 * 1024 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Inventory {
    schema_version: u64,
    source_commit: String,
    pub(crate) modules: Vec<Module>,
    tools: Vec<Value>,
    materials: Vec<Value>,
}

/// Parsed input and the digest of its exact bounded raw bytes.
pub(crate) struct CheckedInventory {
    pub(crate) inventory: Inventory,
    pub(crate) raw_sha256: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Module {
    pub(crate) id: String,
    pub(crate) filename: String,
    pub(crate) size: u64,
    pub(crate) sha256: String,
    contributions: Vec<Value>,
}

pub(crate) fn invalid(message: impl fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_string())
}

pub(crate) fn read_inventory(
    path: &Path,
    expected: &[String],
    source: Option<&str>,
) -> io::Result<CheckedInventory> {
    let leaf_path = path.components().collect::<std::path::PathBuf>();
    #[cfg(unix)]
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(&leaf_path)
        .map_err(|error| invalid(format!("cannot open inventory {}: {error}", path.display())))?;
    #[cfg(not(unix))]
    let file = File::open(&leaf_path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(invalid("inventory must be a regular file"));
    }
    #[cfg(unix)]
    if metadata.nlink() != 1 {
        return Err(invalid("inventory must have exactly one hard link"));
    }
    let mut bytes = Vec::new();
    (&file).take(MAX_INVENTORY + 1).read_to_end(&mut bytes)?;
    #[cfg(unix)]
    {
        let after = file.metadata()?;
        if (
            metadata.len(),
            metadata.mtime(),
            metadata.mtime_nsec(),
            metadata.ctime(),
            metadata.ctime_nsec(),
            metadata.nlink(),
        ) != (
            after.len(),
            after.mtime(),
            after.mtime_nsec(),
            after.ctime(),
            after.ctime_nsec(),
            after.nlink(),
        ) {
            return Err(invalid("inventory changed while reading"));
        }
    }
    if bytes.len() as u64 > MAX_INVENTORY {
        return Err(invalid("inventory exceeds 1 MiB"));
    }
    let text = std::str::from_utf8(&bytes).map_err(|_| invalid("inventory must be UTF-8"))?;
    validate_numbers(text)?;
    let mut parser = serde_json::Deserializer::from_str(text);
    let value = StrictValue(0).deserialize(&mut parser).map_err(invalid)?;
    parser.end().map_err(invalid)?;
    let inventory: Inventory = serde_json::from_value(value).map_err(invalid)?;
    if inventory.schema_version != 1
        || !lower_hex(&inventory.source_commit, 40)
        || source.is_some_and(|source| source != inventory.source_commit)
    {
        return Err(invalid("inventory schema version or source does not match"));
    }
    let mut previous: Option<&str> = None;
    let mut filenames = BTreeSet::new();
    for module in &inventory.modules {
        if !safe_module_id(&module.id)
            || module.filename != bundle_file_name(&module.id)
            || !filenames.insert(&module.filename)
            || previous.is_some_and(|id| id >= module.id.as_str())
            || module.size == 0
            || module.size > MAX_BUNDLE
            || !lower_hex(&module.sha256, 64)
        {
            return Err(invalid(
                "inventory module identity, order, size or digest is invalid",
            ));
        }
        previous = Some(&module.id);
    }
    let found = inventory
        .modules
        .iter()
        .map(|module| (module.id.clone(), module.size))
        .collect::<Vec<_>>();
    check_bundle_set(expected, &found).map_err(invalid)?;
    // Required arrays were parsed strictly, but are not composition evidence.
    let _ = (&inventory.tools, &inventory.materials);
    for module in &inventory.modules {
        let _ = &module.contributions;
    }
    Ok(CheckedInventory {
        inventory,
        raw_sha256: hex::encode(Sha256::digest(&bytes)),
    })
}

fn lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

// serde_json normally converts overflowing integer literals into floats.
// Reject those before parsing, including numbers in otherwise opaque values.
fn validate_numbers(text: &str) -> io::Result<()> {
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'"' => {
                index += 1;
                while index < bytes.len() && bytes[index] != b'"' {
                    if bytes[index] == b'\\' {
                        index += 1;
                    }
                    index += 1;
                }
            }
            b'-' | b'0'..=b'9' => {
                let start = index;
                while index < bytes.len()
                    && matches!(bytes[index], b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')
                {
                    index += 1;
                }
                let number = &text[start..index];
                let value = number
                    .parse::<f64>()
                    .map_err(|_| invalid("invalid inventory number"))?;
                if !value.is_finite() {
                    return Err(invalid("inventory number overflows"));
                }
                if value.fract() == 0.0 && value.abs() > 9_007_199_254_740_991.0 {
                    return Err(invalid("inventory contains an unsafe integer"));
                }
                if !number.contains(['.', 'e', 'E'])
                    && ((number.starts_with('-') && number.parse::<i64>().is_err())
                        || (!number.starts_with('-') && number.parse::<u64>().is_err()))
                {
                    return Err(invalid("inventory integer overflows"));
                }
                continue;
            }
            _ => {}
        }
        index += 1;
    }
    Ok(())
}

struct StrictValue(usize);

impl<'de> DeserializeSeed<'de> for StrictValue {
    type Value = Value;

    fn deserialize<D: serde::Deserializer<'de>>(self, deserializer: D) -> Result<Value, D::Error> {
        deserializer.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for StrictValue {
    type Value = Value;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("strict JSON with unique keys and depth at most 64")
    }

    fn visit_bool<E: serde::de::Error>(self, value: bool) -> Result<Value, E> {
        Ok(Value::Bool(value))
    }
    fn visit_unit<E: serde::de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Value, E> {
        Ok(Value::String(value.to_owned()))
    }
    fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<Value, E> {
        Ok(Value::Number(Number::from(value)))
    }
    fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<Value, E> {
        Ok(Value::Number(Number::from(value)))
    }
    fn visit_f64<E: serde::de::Error>(self, value: f64) -> Result<Value, E> {
        Number::from_f64(value)
            .map(Value::Number)
            .ok_or_else(|| E::custom("nonfinite number"))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Value, A::Error> {
        if self.0 >= 64 {
            return Err(serde::de::Error::custom("JSON depth exceeds 64"));
        }
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element_seed(StrictValue(self.0 + 1))? {
            values.push(value);
        }
        Ok(Value::Array(values))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut object: A) -> Result<Value, A::Error> {
        if self.0 >= 64 {
            return Err(serde::de::Error::custom("JSON depth exceeds 64"));
        }
        let mut values = Map::new();
        while let Some(key) = object.next_key::<String>()? {
            if values.contains_key(&key) {
                return Err(serde::de::Error::custom("duplicate JSON key"));
            }
            let value = object.next_value_seed(StrictValue(self.0 + 1))?;
            values.insert(key, value);
        }
        Ok(Value::Object(values))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::fs;
    use std::os::unix::fs::symlink;

    use serde_json::json;
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn rejects_inventory_leaf_links_even_when_pointing_at_valid_bytes() {
        let root = tempdir().expect("should create inventory fixture");
        let real = root.path().join("real.json");
        let link = root.path().join("inventory.json");
        fs::write(
            &real,
            serde_json::to_vec(&json!({
                "schema_version": 1, "source_commit": "a".repeat(40),
                "modules": [], "tools": [], "materials": []
            }))
            .expect("should serialize inventory"),
        )
        .expect("should write inventory");
        symlink(&real, &link).expect("should link inventory");

        let result = read_inventory(&link, &[], None);

        assert!(
            result.is_err(),
            "should reject links at the actual inventory read"
        );
    }

    #[test]
    fn declared_bundle_cap_is_enforced_by_inventory_parser() {
        let root = tempdir().expect("should create inventory fixture");
        let path = root.path().join("inventory.json");
        const CONTRACT_LIMIT: u64 = 32 * 1024 * 1024;
        for (size, accepted) in [(CONTRACT_LIMIT, true), (CONTRACT_LIMIT + 1, false)] {
            fs::write(
                &path,
                serde_json::to_vec(&json!({
                    "schema_version": 1, "source_commit": "a".repeat(40),
                    "modules": [{"id": "core", "filename": "tsjs-core.js", "size": size,
                        "sha256": "a".repeat(64), "contributions": []}],
                    "tools": [], "materials": []
                }))
                .expect("should serialize inventory"),
            )
            .expect("should write inventory");
            let result = read_inventory(&path, &["core".to_owned()], None);
            if accepted {
                assert!(result.is_ok(), "should accept exact declared bundle cap");
            } else {
                let error = result
                    .err()
                    .expect("should reject oversized declared bundle");
                assert!(
                    error.to_string().contains("size"),
                    "should explain inventory size rejection: {error}"
                );
            }
        }
    }
}
