use std::collections::BTreeMap;
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::Path;

use error_stack::Report;
use serde::Serialize;
use serde::de::{DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde::ser::{SerializeMap as _, SerializeSeq as _};
use serde_json::{Map, Number, Value};
use sha2::{Digest as _, Sha256};

use crate::error::ReleaseError;
/// Byte-limit class selected before a document is read or parsed.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Role {
    Metadata,
    Source,
    Companion,
    Spdx,
    Executable,
    /// One Cargo `--message-format=json` line.
    CargoMessage,
    /// Internal Cargo observations; never a published metadata contract.
    CargoObservation,
    /// One immutable hosted artifact ZIP, checked before narrow parsing.
    ArtifactArchive,
    /// One private ordinary observation ZIP, bounded before allocation.
    ObservationArchive,
}

impl Role {
    /// Maximum bytes permitted for this document role.
    pub(crate) fn limit(self) -> u64 {
        match self {
            Self::Metadata | Self::CargoMessage => 1024 * 1024,
            Self::Source => 64 * 1024 * 1024,
            Self::Companion => 32 * 1024 * 1024,
            Self::Spdx | Self::CargoObservation => 16 * 1024 * 1024,
            Self::Executable => 512 * 1024 * 1024,
            Self::ArtifactArchive => 768 * 1024 * 1024,
            Self::ObservationArchive => 2 * 1024 * 1024,
        }
    }
}

/// Parses bounded strict JSON with unique keys and finite numeric tokens.
pub(crate) fn parse_json(bytes: &[u8], role: Role) -> Result<Value, Report<ReleaseError>> {
    if bytes.len() as u64 > role.limit() {
        return Err(invalid("document exceeds role byte limit"));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| invalid("document must be UTF-8"))?;
    if text.starts_with('\u{feff}') {
        return Err(invalid("document must not start with a BOM"));
    }
    validate_numbers(text)?;
    let mut parser = serde_json::Deserializer::from_str(text);
    let value = StrictSeed { depth: 1 }
        .deserialize(&mut parser)
        .map_err(invalid)?;
    parser.end().map_err(invalid)?;
    Ok(value)
}

/// Exact bytes read from one held descriptor and their SHA-256.
pub(crate) struct RawBytes {
    pub(crate) bytes: Vec<u8>,
    pub(crate) sha256: String,
}

/// Exact bounded file bytes measured without retaining their body in memory.
pub(crate) struct FileDigest {
    pub(crate) sha256: String,
    pub(crate) size: u64,
    pub(crate) mode: u32,
}

/// Reads one unlinked regular file without following a leaf symlink.
pub(crate) fn read_bounded(path: &Path, role: Role) -> Result<RawBytes, Report<ReleaseError>> {
    read_bounded_with_check(path, role, &|| Ok(()))
}

/// Reads bounded bytes while checking the owning attempt between 64 KiB chunks.
///
/// # Errors
///
/// Returns an error for cancellation, expired limits, unsafe files, changed
/// metadata or paths, or bytes exceeding the role bound before allocation.
pub(crate) fn read_bounded_with_check(
    path: &Path,
    role: Role,
    check: &dyn Fn() -> Result<(), Report<ReleaseError>>,
) -> Result<RawBytes, Report<ReleaseError>> {
    check()?;
    let file = open_input(path)?;
    let before = file.metadata().map_err(invalid)?;
    let raw = read_held_with_check(&file, &before, role, check)?;
    verify_input_path(path, &before)?;
    check()?;
    Ok(raw)
}

/// Hashes a bounded held file with constant-sized buffers and attempt checks.
///
/// # Errors
///
/// Returns an error for unsafe files, changed metadata or paths, an exceeded
/// byte limit, cancellation or an expired owning attempt.
pub(crate) fn hash_bounded_with_check(
    path: &Path,
    limit: u64,
    check: &dyn Fn() -> Result<(), Report<ReleaseError>>,
) -> Result<FileDigest, Report<ReleaseError>> {
    check()?;
    let file = open_input(path)?;
    let before = file.metadata().map_err(invalid)?;
    let digest = consume_held(&file, &before, limit, check, &mut |_| Ok(()))?;
    verify_input_path(path, &before)?;
    check()?;
    Ok(digest)
}

fn open_input(path: &Path) -> Result<File, Report<ReleaseError>> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .map_err(invalid)
}

fn verify_input_path(path: &Path, before: &std::fs::Metadata) -> Result<(), Report<ReleaseError>> {
    let current = std::fs::symlink_metadata(path).map_err(invalid)?;
    if !current.is_file() || stamp(before) != stamp(&current) {
        return Err(invalid("input path changes while reading held descriptor"));
    }
    Ok(())
}

#[cfg(test)]
fn read_held(
    file: &File,
    before: &std::fs::Metadata,
    role: Role,
) -> Result<RawBytes, Report<ReleaseError>> {
    read_held_with_check(file, before, role, &|| Ok(()))
}

fn read_held_with_check(
    file: &File,
    before: &std::fs::Metadata,
    role: Role,
    check: &dyn Fn() -> Result<(), Report<ReleaseError>>,
) -> Result<RawBytes, Report<ReleaseError>> {
    check()?;
    validate_held(before, role.limit())?;
    let capacity = usize::try_from(before.len()).map_err(invalid)?;
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(capacity).map_err(invalid)?;
    let digest = consume_held(file, before, role.limit(), check, &mut |chunk| {
        bytes.extend_from_slice(chunk);
        Ok(())
    })?;
    Ok(RawBytes {
        bytes,
        sha256: digest.sha256,
    })
}

fn validate_held(before: &std::fs::Metadata, limit: u64) -> Result<(), Report<ReleaseError>> {
    if !before.is_file() || before.nlink() != 1 {
        return Err(invalid("input must be a regular file with one hard link"));
    }
    if before.len() > limit {
        return Err(invalid("input exceeds role byte limit"));
    }
    Ok(())
}

fn consume_held(
    file: &File,
    before: &std::fs::Metadata,
    limit: u64,
    check: &dyn Fn() -> Result<(), Report<ReleaseError>>,
    consume: &mut impl FnMut(&[u8]) -> Result<(), Report<ReleaseError>>,
) -> Result<FileDigest, Report<ReleaseError>> {
    check()?;
    validate_held(before, limit)?;
    let mut input = file;
    let mut size = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    let mut digest = Sha256::new();
    loop {
        check()?;
        let count = input.read(&mut buffer).map_err(invalid)?;
        check()?;
        if count == 0 {
            break;
        }
        size = size
            .checked_add(count as u64)
            .ok_or_else(|| invalid("input length overflows"))?;
        if size > before.len() || size > limit {
            return Err(invalid("input grows while reading held descriptor"));
        }
        digest.update(&buffer[..count]);
        consume(&buffer[..count])?;
    }
    let after = file.metadata().map_err(invalid)?;
    if stamp(before) != stamp(&after) || size != before.len() {
        return Err(invalid("input changes while reading held descriptor"));
    }
    check()?;
    Ok(FileDigest {
        sha256: hex::encode(digest.finalize()),
        size,
        mode: before.permissions().mode(),
    })
}

fn stamp(metadata: &std::fs::Metadata) -> (u64, u64, u64, u64, i64, i64, i64, i64, u32) {
    (
        metadata.dev(),
        metadata.ino(),
        metadata.nlink(),
        metadata.len(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec(),
        metadata.permissions().mode(),
    )
}
struct LimitedBytes {
    bytes: Vec<u8>,
    limit: u64,
}

impl io::Write for LimitedBytes {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() as u64 > self.limit.saturating_sub(self.bytes.len() as u64) {
            return Err(io::Error::other("output exceeds role byte limit"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct Canonical<'a> {
    value: &'a Value,
    depth: usize,
}

impl Serialize for Canonical<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if self.depth > 64 && matches!(self.value, Value::Object(_) | Value::Array(_)) {
            return Err(serde::ser::Error::custom("JSON container depth exceeds 64"));
        }
        match self.value {
            Value::Object(values) => {
                let ordered: BTreeMap<_, _> = values.iter().collect();
                let mut map = serializer.serialize_map(Some(ordered.len()))?;
                for (key, value) in ordered {
                    map.serialize_entry(
                        key,
                        &Canonical {
                            value,
                            depth: self.depth + 1,
                        },
                    )?;
                }
                map.end()
            }
            Value::Array(values) => {
                let mut sequence = serializer.serialize_seq(Some(values.len()))?;
                for value in values {
                    sequence.serialize_element(&Canonical {
                        value,
                        depth: self.depth + 1,
                    })?;
                }
                sequence.end()
            }
            scalar => scalar.serialize(serializer),
        }
    }
}

/// Serializes JSON with sorted keys and a trailing newline.
pub(crate) fn canonical_json(value: &Value, role: Role) -> Result<Vec<u8>, Report<ReleaseError>> {
    let mut output = LimitedBytes {
        bytes: Vec::new(),
        limit: role.limit(),
    };
    serde_json::to_writer(&mut output, &Canonical { value, depth: 1 }).map_err(invalid)?;
    output.write_all(b"\n").map_err(invalid)?;
    Ok(output.bytes)
}

fn invalid(message: impl fmt::Display) -> Report<ReleaseError> {
    Report::new(ReleaseError::Document).attach(message.to_string())
}

// Serde may otherwise convert overflowing integer tokens to finite floats.
fn validate_numbers(text: &str) -> Result<(), Report<ReleaseError>> {
    let bytes = text.as_bytes();
    let mut cursor = 0;
    while cursor < bytes.len() {
        if bytes[cursor] == b'"' {
            cursor += 1;
            while cursor < bytes.len() {
                match bytes[cursor] {
                    b'\\' => cursor += 2,
                    b'"' => {
                        cursor += 1;
                        break;
                    }
                    _ => cursor += 1,
                }
            }
        } else if bytes[cursor] == b'-' || bytes[cursor].is_ascii_digit() {
            let start = cursor;
            while cursor < bytes.len()
                && matches!(
                    bytes[cursor],
                    b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9'
                )
            {
                cursor += 1;
            }
            let token = &text[start..cursor];
            let valid = if token.contains(['.', 'e', 'E']) {
                token.parse::<f64>().is_ok_and(f64::is_finite)
            } else if token.starts_with('-') {
                token.parse::<i64>().is_ok()
            } else {
                token.parse::<u64>().is_ok()
            };
            if !valid {
                return Err(invalid("JSON number exceeds its finite numeric range"));
            }
        } else {
            cursor += 1;
        }
    }
    Ok(())
}

struct StrictSeed {
    depth: usize,
}

impl<'de> DeserializeSeed<'de> for StrictSeed {
    type Value = Value;

    fn deserialize<D: serde::Deserializer<'de>>(self, deserializer: D) -> Result<Value, D::Error> {
        deserializer.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for StrictSeed {
    type Value = Value;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("JSON with unique object keys")
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

    fn visit_seq<A: SeqAccess<'de>>(self, mut access: A) -> Result<Value, A::Error> {
        if self.depth > 64 {
            return Err(serde::de::Error::custom("JSON container depth exceeds 64"));
        }
        let mut values = Vec::new();
        while let Some(value) = access.next_element_seed(StrictSeed {
            depth: self.depth + 1,
        })? {
            values.push(value);
        }
        Ok(Value::Array(values))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut access: A) -> Result<Value, A::Error> {
        if self.depth > 64 {
            return Err(serde::de::Error::custom("JSON container depth exceeds 64"));
        }
        let mut values = Map::new();
        while let Some(key) = access.next_key::<String>()? {
            if values.contains_key(&key) {
                return Err(serde::de::Error::custom("duplicate object key"));
            }
            values.insert(
                key,
                access.next_value_seed(StrictSeed {
                    depth: self.depth + 1,
                })?,
            );
        }
        Ok(Value::Object(values))
    }
}

#[cfg(test)]
mod tests;
