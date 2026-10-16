//! Complete final-byte comparison of the canonical and reproduction Fastly
//! builds.
//!
//! The Fastly executable has no normalized or excluded region: the two final
//! files must be identical byte for byte. Before comparing bytes, the pair
//! must be one canonical and one reproduction build of the same asset from
//! the same source, `Cargo.lock`, vendored materials, JS handoff and Rust
//! tools, and their ambient records must vary exactly what the standard claim
//! states.

use std::io::Read as _;

use error_stack::Report;
use sha2::{Digest as _, Sha256};
use wasmparser::{Parser, Payload};

use super::ambient::{AmbientClaim, AmbientRecord};
use super::artifact::EXECUTABLE_LIMIT;
use super::{AssetId, RecipeCompleted, Role, recipe_error};
use crate::error::ReleaseError;

/// The only asset whose canonical and reproduction outputs must be byte-identical.
pub(crate) const FASTLY_ASSET: &str = "trusted-server-fastly.wasm";
/// Bytes shown from each side at the first difference.
const WINDOW: usize = 16;

/// Proof that the canonical and reproduction Fastly builds produced identical
/// final bytes.
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

/// Requires `canonical` and `reproduction` to be equal, naming `label` otherwise.
fn same_input<T: PartialEq + ?Sized>(
    label: &str,
    canonical: &T,
    reproduction: &T,
) -> Result<(), Report<ReleaseError>> {
    if canonical == reproduction {
        return Ok(());
    }
    Err(recipe_error(format!(
        "canonical and reproduction builds used different {label}"
    )))
}

/// Requires one canonical and one reproduction build of `asset` over
/// identical inputs.
pub(super) fn check_pair(
    canonical: &RecipeCompleted,
    reproduction: &RecipeCompleted,
    asset: AssetId,
) -> Result<(), Report<ReleaseError>> {
    if canonical.role() != Role::Canonical || reproduction.role() != Role::Reproduction {
        return Err(recipe_error(
            "a comparison needs a canonical then a reproduction build",
        ));
    }
    if canonical.asset() != asset || reproduction.asset() != asset {
        return Err(recipe_error(format!(
            "this comparison compares only {}",
            asset.artifact().name
        )));
    }
    same_input("source commits", canonical.commit(), reproduction.commit())?;
    same_input(
        "Cargo.lock files",
        canonical.cargo_lock_sha256(),
        reproduction.cargo_lock_sha256(),
    )?;
    same_input(
        "vendored materials",
        canonical.materials_digest(),
        reproduction.materials_digest(),
    )?;
    same_input(
        "producer-normalized Git manifests",
        canonical.normalized_manifest_digests(),
        reproduction.normalized_manifest_digests(),
    )?;
    same_input(
        "derived Git manifests",
        canonical.derived_manifest_digests(),
        reproduction.derived_manifest_digests(),
    )?;
    same_input(
        "JS handoffs",
        &canonical.js_digests(),
        &reproduction.js_digests(),
    )?;
    same_input(
        "Rust tools",
        canonical.rust_tools_digest(),
        reproduction.rust_tools_digest(),
    )
}

/// Reads the retained artifact and requires its recorded size and digest.
pub(super) fn read_artifact(completed: &RecipeCompleted) -> Result<Vec<u8>, Report<ReleaseError>> {
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

/// Compares the complete final Fastly files of a canonical and a reproduction
/// build.
///
/// # Errors
///
/// Returns [`ReleaseError::Recipe`] when the pair is not the canonical then
/// the reproduction build of the Fastly asset, the builds used different
/// source, lock, materials, normalized or derived manifests, JS handoff or
/// tools, the ambient records do not vary exactly what
/// [`AmbientClaim::standard`] states, an artifact no longer rehashes, or the
/// final bytes differ; a difference reports both sizes and digests, the first
/// differing offset, the bytes there and their section.
pub(crate) fn check_fastly_equality(
    canonical: &RecipeCompleted,
    reproduction: &RecipeCompleted,
) -> Result<FastlyEqualityChecked, Report<ReleaseError>> {
    check_pair(canonical, reproduction, AssetId::parse(FASTLY_ASSET)?)?;
    let ambient = AmbientRecord::pair(
        canonical.ambient(),
        reproduction.ambient(),
        &AmbientClaim::standard(),
    )?;
    let canonical_bytes = read_artifact(canonical)?;
    let reproduction_bytes = read_artifact(reproduction)?;
    if canonical_bytes != reproduction_bytes {
        return Err(recipe_error(describe_mismatch(
            &canonical_bytes,
            &reproduction_bytes,
        )));
    }
    Ok(FastlyEqualityChecked {
        asset: canonical.asset(),
        sha256: canonical.artifact().sha256().to_owned(),
        size: canonical.artifact().size(),
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
pub(super) fn describe_mismatch(canonical: &[u8], reproduction: &[u8]) -> String {
    let offset = first_difference(canonical, reproduction).unwrap_or(canonical.len());
    let window = |bytes: &[u8]| {
        let end = bytes.len().min(offset.saturating_add(WINDOW));
        hex::encode(bytes.get(offset..end).unwrap_or_default())
    };
    let differing = canonical
        .iter()
        .zip(reproduction)
        .filter(|(left, right)| left != right)
        .count()
        + canonical.len().abs_diff(reproduction.len());
    format!(
        "final Fastly bytes differ: canonical size {} sha256 {}, \
         reproduction size {} sha256 {}; first differing offset {offset} \
         (canonical {}: {}, reproduction {}: {}); {differing} differing bytes",
        canonical.len(),
        hex::encode(Sha256::digest(canonical)),
        reproduction.len(),
        hex::encode(Sha256::digest(reproduction)),
        section_at(canonical, offset),
        window(canonical),
        section_at(reproduction, offset),
        window(reproduction),
    )
}
