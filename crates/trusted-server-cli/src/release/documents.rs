//! Byte-first strict JSON parsing and closed release-index validation.
//!
//! This mirrors the producer's strict parser without depending on the release
//! crate: role byte limits apply before decoding, then BOM, UTF-8, lexical
//! number range, duplicate keys (rejected before insertion), nesting depth and
//! trailing data are checked. Hashes always cover the exact distributed bytes.
//!
//! [`parse_release_index`] enforces the closed field set and every semantic
//! rule for the fixed cohort, and additionally evaluates the retained
//! governing release-index schema with explicit Draft 2020-12, offline.

use std::collections::BTreeSet;
use std::fmt;
use std::sync::LazyLock;

use error_stack::Report;
use jsonschema::Draft;
use regex::Regex;
use serde::Deserialize;
use serde::de::{DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Number, Value};
use sha2::{Digest as _, Sha256};

use super::bounds;
use super::error::{ReleaseError, report};
use super::policy::{ConsumerPolicy, DeclaredIdentity, FixedArtifact, check_capability_vocabulary};

/// Largest integer every JSON implementation represents exactly.
const SAFE_INTEGER_MAX: u64 = 9_007_199_254_740_991;
/// Fixed name of the shared JS inventory asset.
const INVENTORY_NAME: &str = "tsjs-inventory.json";
/// Maximum flat asset name length from the governing schema.
const NAME_MAX: usize = 160;
/// Maximum release tag length from the governing schema.
const TAG_MAX: usize = 128;

static TAG_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(?:-(?:0|[1-9][0-9]*|[0-9]*[A-Za-z-][0-9A-Za-z-]*)(?:\.(?:0|[1-9][0-9]*|[0-9]*[A-Za-z-][0-9A-Za-z-]*))*)?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$",
    )
    .expect("should compile the reviewed release tag pattern")
});

/// Byte-limit class selected before a document is parsed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Role {
    Index,
    Policy,
    Inventory,
    Companion,
    Bundle,
    Spdx,
    GhOutput,
    /// Bounded official REST metadata responses.
    Metadata,
}

impl Role {
    /// Returns the R-BOUNDS byte limit for this role.
    pub(crate) const fn limit(self) -> u64 {
        match self {
            Self::Index => bounds::INDEX_BYTES,
            Self::Policy => bounds::POLICY_BYTES,
            Self::Inventory => bounds::INVENTORY_BYTES,
            Self::Companion => bounds::COMPANION_BYTES,
            Self::Bundle => bounds::BUNDLE_BYTES,
            Self::Spdx => bounds::SPDX_BYTES,
            Self::GhOutput => bounds::STDOUT_BYTES,
            Self::Metadata => bounds::METADATA_BYTES,
        }
    }
}

/// Parses bounded strict JSON with unique keys and finite in-range numbers.
///
/// # Errors
///
/// Returns [`ReleaseError::Document`] for oversize input, a BOM, invalid UTF-8,
/// out-of-range numbers, duplicate keys, nesting beyond 64 levels or trailing
/// data.
pub(crate) fn parse_json(bytes: &[u8], role: Role) -> Result<Value, Report<ReleaseError>> {
    if bytes.len() as u64 > role.limit() {
        return Err(report(
            ReleaseError::Document,
            "document exceeds role byte limit",
        ));
    }
    if bytes.starts_with(b"\xef\xbb\xbf") {
        return Err(report(
            ReleaseError::Document,
            "document must not start with a BOM",
        ));
    }
    let text = std::str::from_utf8(bytes)
        .map_err(|_| report(ReleaseError::Document, "document must be UTF-8"))?;
    validate_numbers(text)?;
    let mut parser = serde_json::Deserializer::from_str(text);
    let value = StrictSeed { depth: 1 }
        .deserialize(&mut parser)
        .map_err(|source| report(ReleaseError::Document, source))?;
    parser
        .end()
        .map_err(|source| report(ReleaseError::Document, source))?;
    Ok(value)
}

/// Returns the lowercase hexadecimal SHA-256 of the exact bytes.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Returns whether `value` is exactly `length` lowercase hexadecimal digits.
pub(crate) fn lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Returns whether `name` is a safe flat asset name.
///
/// Rejects path separators, `..`, empty segments, control characters and
/// anything outside the governing schema's name pattern.
pub(crate) fn safe_name(name: &str) -> bool {
    let mut parts = name.split('.');
    let first = parts.next().unwrap_or_default();
    name.len() <= NAME_MAX
        && first
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && std::iter::once(first).chain(parts).all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        })
}

/// An independently approved full 40-character lowercase source commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ApprovedCommit(String);

impl ApprovedCommit {
    /// Parses an approved commit supplied by trusted consumer approval.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Policy`] unless the value is 40 lowercase hex digits.
    pub(crate) fn parse(value: &str) -> Result<Self, Report<ReleaseError>> {
        if !lower_hex(value, 40) {
            return Err(report(
                ReleaseError::Policy,
                "approved source commit must be 40 lowercase hexadecimal digits",
            ));
        }
        Ok(Self(value.to_owned()))
    }

    /// Returns the commit text.
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

/// A release tag matching the governing `SemVer` `v`-prefixed pattern.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ReleaseTag(String);

impl ReleaseTag {
    /// Parses a release tag.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Policy`] for tags outside the reviewed pattern.
    pub(crate) fn parse(value: &str) -> Result<Self, Report<ReleaseError>> {
        if value.len() > TAG_MAX || !TAG_PATTERN.is_match(value) {
            return Err(report(
                ReleaseError::Policy,
                "release tag must be a v-prefixed SemVer tag",
            ));
        }
        Ok(Self(value.to_owned()))
    }

    /// Returns the tag text.
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

/// The tag and source commit approved independently of downloaded metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ApprovedRelease {
    tag: ReleaseTag,
    commit: ApprovedCommit,
}

impl ApprovedRelease {
    /// Pairs an approved tag with its approved source commit.
    pub(crate) fn new(tag: ReleaseTag, commit: ApprovedCommit) -> Self {
        Self { tag, commit }
    }

    /// Returns the approved tag.
    pub(crate) fn tag(&self) -> &ReleaseTag {
        &self.tag
    }

    /// Returns the approved source commit.
    pub(crate) fn commit(&self) -> &ApprovedCommit {
        &self.commit
    }
}

/// One named, sized and digested release file.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct FileRecord {
    pub(crate) name: String,
    pub(crate) size: u64,
    pub(crate) sha256: String,
}

/// Declared runtime compatibility of an artifact.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Compatibility {
    pub(crate) runtime: String,
    pub(crate) minimum_version: String,
    pub(crate) architecture: String,
}

/// The six per-executable companion files.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Companions {
    pub(crate) sbom: FileRecord,
    pub(crate) provenance: FileRecord,
    pub(crate) sbom_attestation: FileRecord,
    pub(crate) build_evidence: FileRecord,
    pub(crate) build_recipe: FileRecord,
    pub(crate) rebuild_check: FileRecord,
}

impl Companions {
    fn records(&self) -> [(&'static str, u64, &FileRecord); 6] {
        [
            (".spdx.json", bounds::SPDX_BYTES, &self.sbom),
            (
                ".provenance.sigstore.json",
                bounds::BUNDLE_BYTES,
                &self.provenance,
            ),
            (
                ".sbom.sigstore.json",
                bounds::BUNDLE_BYTES,
                &self.sbom_attestation,
            ),
            (
                ".build-evidence.json",
                bounds::COMPANION_BYTES,
                &self.build_evidence,
            ),
            (
                ".build-recipe.json",
                bounds::COMPANION_BYTES,
                &self.build_recipe,
            ),
            (
                ".rebuild-check.json",
                bounds::COMPANION_BYTES,
                &self.rebuild_check,
            ),
        ]
    }
}

/// One executable entry of a release index.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct IndexArtifact {
    pub(crate) name: String,
    pub(crate) size: u64,
    pub(crate) sha256: String,
    pub(crate) role: String,
    pub(crate) kind: String,
    pub(crate) package: String,
    pub(crate) target: String,
    pub(crate) variant: String,
    pub(crate) assurance_profile: String,
    pub(crate) capabilities: Vec<String>,
    pub(crate) compatibility: Compatibility,
    pub(crate) js_inventory_sha256: String,
    pub(crate) companions: Companions,
}

/// A strictly parsed, policy-checked version-1 release index.
///
/// Its `policy_version`/`policy_sha256` identify the producer policy only;
/// [`super::policy::accept_producer_policy`] must still bind them to
/// approved-source bytes.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReleaseIndex {
    pub(crate) schema_version: u64,
    pub(crate) repository: String,
    pub(crate) tag: String,
    pub(crate) source_commit: String,
    pub(crate) recipe_source_commit: String,
    pub(crate) cohort_source_commit: String,
    pub(crate) cohort: String,
    pub(crate) policy_version: u64,
    pub(crate) policy_sha256: String,
    pub(crate) tsjs_inventory: FileRecord,
    pub(crate) artifacts: Vec<IndexArtifact>,
}

/// Exact bytes of the governing release-index v1 schema.
const INDEX_SCHEMA: &[u8] =
    include_bytes!("../../../../docs/superpowers/specs/schemas/release-index-v1.schema.json");

/// Evaluates `value` against `schema` with explicit Draft 2020-12, offline.
///
/// Offline evaluation rejects every unregistered reference, so no remote or
/// file retrieval can occur.
///
/// # Errors
///
/// Returns [`ReleaseError::Document`] if the schema does not compile.
pub(crate) fn schema_accepts(schema: &Value, value: &Value) -> Result<bool, Report<ReleaseError>> {
    let validator = jsonschema::options()
        .with_draft(Draft::Draft202012)
        .offline()
        .build(schema)
        .map_err(|source| report(ReleaseError::Document, source))?;
    Ok(validator.is_valid(value))
}

/// Validates a strictly parsed index against the retained governing schema.
///
/// # Errors
///
/// Returns [`ReleaseError::Document`] when the schema rejects the document.
pub(crate) fn validate_index_schema(value: &Value) -> Result<(), Report<ReleaseError>> {
    let schema = parse_json(INDEX_SCHEMA, Role::Index)?;
    if !schema_accepts(&schema, value)? {
        return Err(report(
            ReleaseError::Document,
            "release index does not match the governing release-index v1 schema",
        ));
    }
    Ok(())
}

/// Parses and validates release-index bytes against the consumer policy.
///
/// Semantic checks run first so diagnostics name the precise contract rule;
/// the governing schema is then evaluated as an independent structural gate.
///
/// # Errors
///
/// Returns [`ReleaseError::Document`] for malformed, ambiguous or open-field
/// JSON and [`ReleaseError::Policy`] for any semantic contract violation.
pub(crate) fn parse_release_index(
    bytes: &[u8],
    policy: &ConsumerPolicy,
    approved: &ApprovedRelease,
) -> Result<ReleaseIndex, Report<ReleaseError>> {
    let value = parse_json(bytes, Role::Index)?;
    validate_safe_integers(&value)?;
    let index = ReleaseIndex::deserialize(&value)
        .map_err(|source| report(ReleaseError::Document, source))?;
    check_index_header(&index, policy, approved)?;
    let mut names = BTreeSet::new();
    if index.tsjs_inventory.name != INVENTORY_NAME {
        return Err(report(
            ReleaseError::Policy,
            "JS inventory must be named tsjs-inventory.json",
        ));
    }
    check_file(
        &index.tsjs_inventory,
        0,
        bounds::INVENTORY_BYTES,
        &mut names,
    )?;
    if index.artifacts.len() != policy.artifacts().len() {
        return Err(report(
            ReleaseError::Policy,
            "release index must list exactly the cohort artifacts",
        ));
    }
    for artifact in &index.artifacts {
        let fixed = policy.artifact(&artifact.name).ok_or_else(|| {
            report(
                ReleaseError::Policy,
                format!("unknown artifact `{}`", artifact.name),
            )
        })?;
        check_artifact(artifact, fixed, &index.tsjs_inventory, &mut names)?;
    }
    validate_index_schema(&value)?;
    Ok(index)
}

fn check_index_header(
    index: &ReleaseIndex,
    policy: &ConsumerPolicy,
    approved: &ApprovedRelease,
) -> Result<(), Report<ReleaseError>> {
    if index.schema_version != 1 {
        return Err(report(
            ReleaseError::Policy,
            "unsupported release-index schema version",
        ));
    }
    if index.repository != policy.repository() || index.cohort != policy.cohort() {
        return Err(report(
            ReleaseError::Policy,
            "release index names another repository or cohort",
        ));
    }
    ReleaseTag::parse(&index.tag)?;
    if index.tag != approved.tag().as_str() {
        return Err(report(
            ReleaseError::Policy,
            "release index tag is not the approved tag",
        ));
    }
    if !lower_hex(&index.source_commit, 40) || index.source_commit != approved.commit().as_str() {
        return Err(report(
            ReleaseError::Policy,
            "release index source commit is not the approved commit",
        ));
    }
    if index.recipe_source_commit != index.source_commit
        || index.cohort_source_commit != index.source_commit
    {
        return Err(report(
            ReleaseError::Policy,
            "recipe and cohort source commits must equal the approved source",
        ));
    }
    if index.policy_version < 1 || !lower_hex(&index.policy_sha256, 64) {
        return Err(report(
            ReleaseError::Policy,
            "release index producer policy identity is malformed",
        ));
    }
    Ok(())
}

fn check_file(
    record: &FileRecord,
    minimum: u64,
    maximum: u64,
    names: &mut BTreeSet<String>,
) -> Result<(), Report<ReleaseError>> {
    if !safe_name(&record.name) {
        return Err(report(
            ReleaseError::Policy,
            "release file name is not a safe flat name",
        ));
    }
    if record.size < minimum || record.size > maximum {
        return Err(report(
            ReleaseError::Policy,
            format!(
                "release file `{}` length is outside its role bound",
                record.name
            ),
        ));
    }
    if !lower_hex(&record.sha256, 64) {
        return Err(report(
            ReleaseError::Policy,
            format!(
                "release file `{}` digest must be 64 lowercase hex digits",
                record.name
            ),
        ));
    }
    if !names.insert(record.name.to_ascii_lowercase()) {
        return Err(report(
            ReleaseError::Policy,
            format!("release file `{}` is listed more than once", record.name),
        ));
    }
    Ok(())
}

fn check_artifact(
    artifact: &IndexArtifact,
    fixed: &FixedArtifact,
    inventory: &FileRecord,
    names: &mut BTreeSet<String>,
) -> Result<(), Report<ReleaseError>> {
    let file = FileRecord {
        name: artifact.name.clone(),
        size: artifact.size,
        sha256: artifact.sha256.clone(),
    };
    check_file(&file, 1, bounds::EXECUTABLE_BYTES, names)?;
    check_capabilities(artifact)?;
    let declared = DeclaredIdentity {
        role: &artifact.role,
        kind: &artifact.kind,
        package: &artifact.package,
        target: &artifact.target,
        variant: &artifact.variant,
        assurance_profile: &artifact.assurance_profile,
        capabilities: &artifact.capabilities,
        compatibility: &artifact.compatibility,
    };
    if !fixed.matches(&declared) {
        return Err(report(
            ReleaseError::Policy,
            format!(
                "artifact `{}` differs from the supported role/kind/package/target/variant/profile/capability combination",
                artifact.name
            ),
        ));
    }
    if artifact.js_inventory_sha256 != inventory.sha256 {
        return Err(report(
            ReleaseError::Policy,
            format!(
                "artifact `{}` names a different JS inventory",
                artifact.name
            ),
        ));
    }
    check_companions(artifact, names)
}

fn check_companions(
    artifact: &IndexArtifact,
    names: &mut BTreeSet<String>,
) -> Result<(), Report<ReleaseError>> {
    for (suffix, maximum, record) in artifact.companions.records() {
        if record.name != format!("{}{suffix}", artifact.name) {
            return Err(report(
                ReleaseError::Policy,
                format!(
                    "artifact `{}` companion must be named `{}{suffix}`",
                    artifact.name, artifact.name
                ),
            ));
        }
        check_file(record, 0, maximum, names)?;
    }
    Ok(())
}

fn check_capabilities(artifact: &IndexArtifact) -> Result<(), Report<ReleaseError>> {
    let unique: BTreeSet<&str> = artifact.capabilities.iter().map(String::as_str).collect();
    if unique.is_empty() || unique.len() != artifact.capabilities.len() || unique.len() > 2 {
        return Err(report(
            ReleaseError::Policy,
            format!(
                "artifact `{}` capabilities must be one or two unique values",
                artifact.name
            ),
        ));
    }
    check_capability_vocabulary(&artifact.capabilities)
}

/// Requires every number to be a nonnegative safe integer token.
///
/// # Errors
///
/// Returns [`ReleaseError::Document`] for negative, fractional or unsafe numbers.
pub(crate) fn validate_safe_integers(value: &Value) -> Result<(), Report<ReleaseError>> {
    match value {
        Value::Number(number)
            if number
                .as_u64()
                .is_none_or(|integer| integer > SAFE_INTEGER_MAX) =>
        {
            Err(report(
                ReleaseError::Document,
                "closed release documents require nonnegative safe integer tokens",
            ))
        }
        Value::Object(values) => values.values().try_for_each(validate_safe_integers),
        Value::Array(values) => values.iter().try_for_each(validate_safe_integers),
        _ => Ok(()),
    }
}

// Serde may otherwise convert overflowing integer tokens to finite floats, so
// every numeric token outside strings is range-checked lexically first.
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
            let token = text.get(start..cursor).unwrap_or_default();
            let valid = if token.contains(['.', 'e', 'E']) {
                token.parse::<f64>().is_ok_and(f64::is_finite)
            } else if token.starts_with('-') {
                token.parse::<i64>().is_ok()
            } else {
                token.parse::<u64>().is_ok()
            };
            if !valid {
                return Err(report(
                    ReleaseError::Document,
                    "JSON number exceeds its finite numeric range",
                ));
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
        if self.depth > bounds::JSON_DEPTH {
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
        if self.depth > bounds::JSON_DEPTH {
            return Err(serde::de::Error::custom("JSON container depth exceeds 64"));
        }
        let mut values = Map::new();
        while let Some(key) = access.next_key::<String>()? {
            if values.contains_key(&key) {
                return Err(serde::de::Error::custom("duplicate object key"));
            }
            let value = access.next_value_seed(StrictSeed {
                depth: self.depth + 1,
            })?;
            values.insert(key, value);
        }
        Ok(Value::Object(values))
    }
}
