use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::Path;

use error_stack::Report;
use jsonschema::Draft;
use serde::Deserialize;
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
    Companion,
    Bundle,
    Spdx,
    Executable,
}

impl Role {
    fn limit(self) -> u64 {
        match self {
            Self::Metadata => 1024 * 1024,
            Self::Companion | Self::Bundle => 32 * 1024 * 1024,
            Self::Spdx => 16 * 1024 * 1024,
            Self::Executable => 512 * 1024 * 1024,
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

/// Reads one unlinked regular file without following a leaf symlink.
pub(crate) fn read_bounded(path: &Path, role: Role) -> Result<RawBytes, Report<ReleaseError>> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .map_err(invalid)?;
    let before = file.metadata().map_err(invalid)?;
    read_held(&file, &before, role)
}

fn read_held(
    file: &File,
    before: &std::fs::Metadata,
    role: Role,
) -> Result<RawBytes, Report<ReleaseError>> {
    if !before.is_file() || before.nlink() != 1 {
        return Err(invalid("input must be a regular file with one hard link"));
    }
    if before.len() > role.limit() {
        return Err(invalid("input exceeds role byte limit"));
    }
    let mut bytes = Vec::new();
    file.take(role.limit() + 1)
        .read_to_end(&mut bytes)
        .map_err(invalid)?;
    let after = file.metadata().map_err(invalid)?;
    if stamp(before) != stamp(&after) || bytes.len() as u64 != before.len() {
        return Err(invalid("input changes while reading held descriptor"));
    }
    if bytes.len() as u64 > role.limit() {
        return Err(invalid("input exceeds role byte limit"));
    }
    let sha256 = hex::encode(Sha256::digest(&bytes));
    Ok(RawBytes { bytes, sha256 })
}

fn stamp(metadata: &std::fs::Metadata) -> (u64, u64, u64, u64, i64, i64, i64, i64) {
    (
        metadata.dev(),
        metadata.ino(),
        metadata.nlink(),
        metadata.len(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec(),
    )
}

struct FlatName<'a>(&'a str);

fn safe_name(name: &str) -> Result<FlatName<'_>, Report<ReleaseError>> {
    let mut parts = name.split('.');
    let first = parts.next().unwrap_or_default();
    if name.len() > 160
        || !first
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        || std::iter::once(first).chain(parts).any(|part| {
            part.is_empty()
                || !part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        })
    {
        return Err(invalid("unsafe flat filename"));
    }
    Ok(FlatName(name))
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

// The caller must select a trusted parent directory; Task 3 owns source/path policy.
fn atomic_json(path: &Path, value: &Value, role: Role) -> Result<(), Report<ReleaseError>> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| invalid("output needs a UTF-8 flat filename"))?;
    let name = safe_name(name)?;
    let bytes = canonical_json(value, role)?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(parent)
        .map_err(invalid)?;
    let destination = parent.join(name.0);
    match std::fs::symlink_metadata(&destination) {
        Ok(metadata) if !metadata.is_file() || metadata.nlink() != 1 => {
            return Err(invalid("output must be an unlinked regular file or absent"));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(invalid(error)),
    }
    let mut temporary = tempfile::Builder::new()
        .prefix(".release-")
        .tempfile_in(parent)
        .map_err(invalid)?;
    temporary
        .as_file()
        .set_permissions(std::fs::Permissions::from_mode(0o600))
        .map_err(invalid)?;
    temporary.write_all(&bytes).map_err(invalid)?;
    temporary.as_file().sync_all().map_err(invalid)?;
    let held = directory.metadata().map_err(invalid)?;
    let current = std::fs::metadata(parent).map_err(invalid)?;
    if (held.dev(), held.ino()) != (current.dev(), current.ino()) {
        return Err(invalid("output parent changes before replacement"));
    }
    commit_output(temporary, &destination, &directory)
}

fn commit_output(
    temporary: tempfile::NamedTempFile,
    path: &Path,
    directory: &File,
) -> Result<(), Report<ReleaseError>> {
    temporary
        .persist(path)
        .map_err(|error| invalid(error.error))?;
    directory.sync_all().map_err(invalid)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileRecord {
    name: String,
    size: u64,
    sha256: String,
}

fn validate_file_records(values: &[Value], role: Role) -> Result<(), Report<ReleaseError>> {
    let mut names = BTreeSet::new();
    for value in values {
        let record = FileRecord::deserialize(value).map_err(invalid)?;
        record.check(role, &mut names)?;
    }
    Ok(())
}

impl FileRecord {
    fn check(&self, role: Role, names: &mut BTreeSet<String>) -> Result<(), Report<ReleaseError>> {
        safe_name(&self.name)?;
        if self.size > role.limit()
            || !lower_hex(&self.sha256, 64)
            || !names.insert(self.name.to_ascii_lowercase())
        {
            return Err(invalid(
                "file record has invalid size, digest or duplicate identity",
            ));
        }
        Ok(())
    }

    fn fields(value: &Value) -> Result<Self, Report<ReleaseError>> {
        Ok(Self {
            name: value["name"]
                .as_str()
                .ok_or_else(|| invalid("missing file name"))?
                .to_owned(),
            size: value["size"]
                .as_u64()
                .ok_or_else(|| invalid("file length must be a nonnegative integer token"))?,
            sha256: value["sha256"]
                .as_str()
                .ok_or_else(|| invalid("missing file digest"))?
                .to_owned(),
        })
    }
}

/// Returns whether `value` is exactly `length` lowercase hexadecimal digits.
pub(crate) fn lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[derive(Clone, Copy)]
enum KnownSchema {
    ReleaseIndex,
    Qualification,
    Spdx,
}

impl KnownSchema {
    fn bytes(&self) -> &'static [u8] {
        match self {
            Self::ReleaseIndex => include_bytes!(
                "../../../docs/superpowers/specs/schemas/release-index-v1.schema.json"
            ),
            Self::Qualification => {
                include_bytes!("../../../scripts/release/qualification/results.schema.json")
            }
            Self::Spdx => include_bytes!("../schemas/spdx-2.3.schema.json"),
        }
    }
    fn draft(&self) -> Draft {
        match self {
            Self::ReleaseIndex | Self::Qualification => Draft::Draft202012,
            Self::Spdx => Draft::Draft7,
        }
    }
}

fn schema_accepts(
    schema: &Value,
    draft: Draft,
    value: &Value,
) -> Result<bool, Report<ReleaseError>> {
    let validator = jsonschema::options()
        .with_draft(draft)
        .offline()
        .build(schema)
        .map_err(invalid)?;
    Ok(validator.is_valid(value))
}

fn validate_schema(schema: KnownSchema, value: &Value) -> Result<(), Report<ReleaseError>> {
    let retained = if matches!(schema, KnownSchema::Spdx) {
        parse_pinned_spdx_schema(schema.bytes())?
    } else {
        parse_json(schema.bytes(), Role::Metadata)?
    };
    if !schema_accepts(&retained, schema.draft(), value)? {
        return Err(invalid("document does not match retained schema"));
    }
    if matches!(
        schema,
        KnownSchema::ReleaseIndex | KnownSchema::Qualification
    ) {
        validate_safe_integers(value)?;
    }
    if matches!(schema, KnownSchema::ReleaseIndex) {
        let mut names = BTreeSet::new();
        FileRecord::fields(&value["tsjs_inventory"])?.check(Role::Metadata, &mut names)?;
        for artifact in value["artifacts"]
            .as_array()
            .ok_or_else(|| invalid("missing artifacts"))?
        {
            FileRecord::fields(artifact)?.check(Role::Executable, &mut names)?;
            for (kind, companion) in artifact["companions"]
                .as_object()
                .ok_or_else(|| invalid("missing companions"))?
            {
                let role = if kind == "sbom" {
                    Role::Spdx
                } else {
                    Role::Companion
                };
                FileRecord::fields(companion)?.check(role, &mut names)?;
            }
        }
    }
    Ok(())
}

fn parse_pinned_spdx_schema(bytes: &[u8]) -> Result<Value, Report<ReleaseError>> {
    if bytes.len() != 45_312
        || hex::encode(Sha256::digest(bytes))
            != "239208b7ac287b3cf5d9a9af23f9d69863971102a5e1587a27a398b43490b89b"
    {
        return Err(invalid(
            "retained SPDX schema raw identity does not match pinned source",
        ));
    }
    let value = parse_json(bytes, Role::Metadata)?;
    if value["$schema"] != "http://json-schema.org/draft-07/schema#" {
        return Err(invalid("retained SPDX schema must declare Draft 7"));
    }
    Ok(value)
}

fn validate_safe_integers(value: &Value) -> Result<(), Report<ReleaseError>> {
    match value {
        Value::Number(number)
            if number
                .as_u64()
                .is_none_or(|integer| integer > 9_007_199_254_740_991) =>
        {
            return Err(invalid(
                "closed role requires nonnegative safe integer tokens",
            ));
        }
        Value::Object(values) => {
            for value in values.values() {
                validate_safe_integers(value)?;
            }
        }
        Value::Array(values) => {
            for value in values {
                validate_safe_integers(value)?;
            }
        }
        _ => {}
    }
    Ok(())
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
