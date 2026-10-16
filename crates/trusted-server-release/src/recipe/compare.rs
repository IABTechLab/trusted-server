//! Complete final-byte comparison of the role A and role B Fastly builds.
//!
//! The Fastly executable has no normalized or excluded region: the two final
//! files must be identical byte for byte. Before comparing bytes, the pair
//! must be one A and one B build of the same asset from the same source,
//! `Cargo.lock`, vendored materials, JS handoff and Rust tools, and their
//! ambient records must vary exactly what the standard claim states.

use std::io::Read as _;

use error_stack::Report;
use sha2::{Digest as _, Sha256};
use wasmparser::{Parser, Payload};

use super::ambient::{AmbientClaim, AmbientRecord};
use super::artifact::EXECUTABLE_LIMIT;
use super::{AssetId, RecipeCompleted, Role, recipe_error};
use crate::error::ReleaseError;

/// The only asset whose A/B outputs must be byte-identical.
pub(crate) const FASTLY_ASSET: &str = "trusted-server-fastly.wasm";
/// Bytes shown from each side at the first difference.
const WINDOW: usize = 16;

/// Proof that the A and B Fastly builds produced identical final bytes.
///
/// It has private fields and only [`check_fastly_equality`] constructs it.
#[derive(Debug)]
pub(crate) struct FastlyEqualityChecked {
    asset: AssetId,
    sha256: String,
    size: u64,
    ambient: AmbientRecord,
}

impl FastlyEqualityChecked {
    /// The compared asset.
    pub(crate) fn asset(&self) -> AssetId {
        self.asset
    }

    /// SHA-256 of the identical final bytes.
    pub(crate) fn sha256(&self) -> &str {
        &self.sha256
    }

    /// Size of the identical final bytes.
    pub(crate) fn size(&self) -> u64 {
        self.size
    }

    /// The verified ambient pairing of the two builds.
    pub(crate) fn ambient(&self) -> &AmbientRecord {
        &self.ambient
    }
}

/// Requires `first` and `second` to be equal, naming `label` otherwise.
fn same_input<T: PartialEq + ?Sized>(
    label: &str,
    first: &T,
    second: &T,
) -> Result<(), Report<ReleaseError>> {
    if first == second {
        return Ok(());
    }
    Err(recipe_error(format!(
        "A and B Fastly builds used different {label}"
    )))
}

/// Requires one A and one B build of the Fastly asset over identical inputs.
fn check_pair(a: &RecipeCompleted, b: &RecipeCompleted) -> Result<(), Report<ReleaseError>> {
    if a.role() != Role::A || b.role() != Role::B {
        return Err(recipe_error(
            "Fastly equality needs a role A then a role B build",
        ));
    }
    let fastly = AssetId::parse(FASTLY_ASSET)?;
    if a.asset() != fastly || b.asset() != fastly {
        return Err(recipe_error(format!(
            "Fastly equality compares only {FASTLY_ASSET}"
        )));
    }
    same_input("source commits", a.commit(), b.commit())?;
    same_input(
        "Cargo.lock files",
        a.cargo_lock_sha256(),
        b.cargo_lock_sha256(),
    )?;
    same_input(
        "vendored materials",
        a.materials_digest(),
        b.materials_digest(),
    )?;
    same_input(
        "derived Git manifests",
        a.derived_manifest_digests(),
        b.derived_manifest_digests(),
    )?;
    same_input("JS handoffs", &a.js_digests(), &b.js_digests())?;
    same_input("Rust tools", a.rust_tools_digest(), b.rust_tools_digest())
}

/// Reads the retained artifact and requires its recorded size and digest.
fn read_artifact(completed: &RecipeCompleted) -> Result<Vec<u8>, Report<ReleaseError>> {
    completed.rehash_for_handoff()?;
    let artifact = completed.artifact();
    if artifact.size() > EXECUTABLE_LIMIT {
        return Err(recipe_error("artifact exceeds the executable limit"));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(artifact.path())
        .and_then(|file| {
            file.take(EXECUTABLE_LIMIT.saturating_add(1))
                .read_to_end(&mut bytes)
        })
        .map_err(|error| recipe_error(format!("cannot read the final artifact: {error}")))?;
    if bytes.len() as u64 != artifact.size()
        || hex::encode(Sha256::digest(&bytes)) != artifact.sha256()
    {
        return Err(recipe_error("final artifact changed after its rehash"));
    }
    Ok(bytes)
}

/// Compares the complete final Fastly files of a role A and a role B build.
///
/// # Errors
///
/// Returns [`ReleaseError::Recipe`] when the pair is not role A then role B
/// of the Fastly asset, the builds used different source, lock, materials,
/// derived manifests, JS handoff or tools, the ambient records do not vary
/// exactly what [`AmbientClaim::standard`] states, an artifact no longer
/// rehashes, or the final bytes differ; a difference reports both sizes and
/// digests, the first differing offset, the bytes there and their section.
pub(crate) fn check_fastly_equality(
    a: &RecipeCompleted,
    b: &RecipeCompleted,
) -> Result<FastlyEqualityChecked, Report<ReleaseError>> {
    check_pair(a, b)?;
    let ambient = AmbientRecord::pair(a.ambient(), b.ambient(), &AmbientClaim::standard())?;
    let first = read_artifact(a)?;
    let second = read_artifact(b)?;
    if first != second {
        return Err(recipe_error(describe_mismatch(&first, &second)));
    }
    Ok(FastlyEqualityChecked {
        asset: a.asset(),
        sha256: a.artifact().sha256().to_owned(),
        size: a.artifact().size(),
        ambient,
    })
}

/// Returns the first offset where `first` and `second` differ, if any.
///
/// When one is a strict prefix of the other, the offset is the shorter length.
pub(super) fn first_difference(first: &[u8], second: &[u8]) -> Option<usize> {
    first
        .iter()
        .zip(second)
        .position(|(left, right)| left != right)
        .or_else(|| (first.len() != second.len()).then(|| first.len().min(second.len())))
}

/// Names the Wasm section whose payload holds `offset`, if the module parses.
fn section_at(bytes: &[u8], offset: usize) -> String {
    for payload in Parser::new(0).parse_all(bytes) {
        let Ok(payload) = payload else {
            return "unparsed module".to_owned();
        };
        let Some((id, range)) = payload.as_section() else {
            continue;
        };
        if range.contains(&offset) {
            return match payload {
                Payload::CustomSection(reader) => format!("custom section {:?}", reader.name()),
                _ => format!("section id {id}"),
            };
        }
    }
    "no section payload".to_owned()
}

/// Describes a final-byte mismatch with bounded diagnostics.
pub(super) fn describe_mismatch(first: &[u8], second: &[u8]) -> String {
    let offset = first_difference(first, second).unwrap_or(first.len());
    let window = |bytes: &[u8]| {
        let end = bytes.len().min(offset.saturating_add(WINDOW));
        hex::encode(bytes.get(offset..end).unwrap_or_default())
    };
    let differing = first
        .iter()
        .zip(second)
        .filter(|(left, right)| left != right)
        .count()
        + first.len().abs_diff(second.len());
    format!(
        "final Fastly bytes differ: A size {} sha256 {}, B size {} sha256 {}; \
         first differing offset {offset} (A {}: {}, B {}: {}); {differing} differing bytes",
        first.len(),
        hex::encode(Sha256::digest(first)),
        second.len(),
        hex::encode(Sha256::digest(second)),
        section_at(first, offset),
        window(first),
        section_at(second, offset),
        window(second),
    )
}
