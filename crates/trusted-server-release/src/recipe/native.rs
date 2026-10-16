//! Native executable evidence: repeatability measured, operator values gated.
//!
//! Complete final-byte equality is a shipping gate for Fastly only. A native
//! `ts` executable built once canonically and once as a reproduction is
//! compared over identical inputs and the outcome is recorded either way, so
//! a nonmatching native pair is a preserved measurement, never a rejection.
//!
//! Operator configuration must never be compiled in. A build whose inherited
//! environment carried fictional operator values (which preflight removes)
//! must hold none of them in its final bytes; that scan is a gate.

use error_stack::Report;
use object::Endianness;
use object::elf::{FileHeader64, SHT_NOBITS};
use object::read::elf::{FileHeader as _, SectionHeader as _};
use sha2::{Digest as _, Sha256};

use super::ambient::{AmbientClaim, AmbientRecord};
use super::compare::{check_pair, first_difference, read_artifact};
use super::{AssetId, RecipeCompleted, recipe_error};
use crate::error::ReleaseError;
use crate::policy::ReleaseTarget;

/// Size and digest of one final executable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ArtifactIdentity {
    /// Lowercase hex SHA-256 of the final bytes.
    pub(crate) sha256: String,
    /// Length of the final bytes.
    pub(crate) size: u64,
}

impl ArtifactIdentity {
    fn of(bytes: &[u8]) -> Self {
        Self {
            sha256: hex::encode(Sha256::digest(bytes)),
            size: bytes.len() as u64,
        }
    }
}

/// Where a nonmatching native pair first differs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NativeDifference {
    /// First differing offset (the shorter length for a strict prefix).
    pub(crate) first_offset: usize,
    /// Differing positions plus the length difference.
    pub(crate) differing_bytes: usize,
    /// The canonical ELF section holding the offset, when one does.
    pub(crate) canonical_section: Option<String>,
}

/// The recorded repeatability of one native canonical/reproduction pair.
///
/// It has private fields and only [`measure_native_repeatability`]
/// constructs it; a difference is data, not an error.
#[derive(Debug)]
pub(crate) struct NativeRepeatability {
    asset: AssetId,
    canonical: ArtifactIdentity,
    reproduction: ArtifactIdentity,
    difference: Option<NativeDifference>,
    ambient: AmbientRecord,
}

impl NativeRepeatability {
    /// The compared native asset.
    pub(crate) fn asset(&self) -> AssetId {
        self.asset
    }

    /// Identity of the canonical final bytes.
    pub(crate) fn canonical(&self) -> &ArtifactIdentity {
        &self.canonical
    }

    /// Identity of the reproduction final bytes.
    pub(crate) fn reproduction(&self) -> &ArtifactIdentity {
        &self.reproduction
    }

    /// Whether the two final files are byte-identical.
    pub(crate) fn identical(&self) -> bool {
        self.difference.is_none()
    }

    /// Where the files first differ, if they do.
    pub(crate) fn difference(&self) -> Option<&NativeDifference> {
        self.difference.as_ref()
    }

    /// The verified ambient pairing of the two builds.
    pub(crate) fn ambient(&self) -> &AmbientRecord {
        &self.ambient
    }
}

/// Measures whether a native canonical and reproduction build produced
/// identical final bytes, recording a mismatch instead of failing on it.
///
/// # Errors
///
/// Returns [`ReleaseError::Recipe`] for the Fastly asset (which takes the
/// equality gate instead), a pair that is not the canonical then the
/// reproduction build of one asset over identical source, lock, materials,
/// manifests, JS handoff and tools, an ambient pairing that does not vary
/// exactly what [`AmbientClaim::standard`] states, or an artifact that no
/// longer rehashes. A byte difference is never an error.
pub(crate) fn measure_native_repeatability(
    canonical: &RecipeCompleted,
    reproduction: &RecipeCompleted,
) -> Result<NativeRepeatability, Report<ReleaseError>> {
    let asset = canonical.asset();
    if asset.artifact().target == ReleaseTarget::Wasm32Wasip1 {
        return Err(recipe_error(
            "Fastly outputs take the equality gate, not a native measurement",
        ));
    }
    check_pair(canonical, reproduction, asset)?;
    let ambient = AmbientRecord::pair(
        canonical.ambient(),
        reproduction.ambient(),
        &AmbientClaim::standard(),
    )?;
    let canonical_bytes = read_artifact(canonical)?;
    let reproduction_bytes = read_artifact(reproduction)?;
    let difference = first_difference(&canonical_bytes, &reproduction_bytes).map(|first_offset| {
        NativeDifference {
            first_offset,
            differing_bytes: differing_bytes(&canonical_bytes, &reproduction_bytes),
            canonical_section: elf_section_at(&canonical_bytes, first_offset),
        }
    });
    Ok(NativeRepeatability {
        asset,
        canonical: ArtifactIdentity::of(&canonical_bytes),
        reproduction: ArtifactIdentity::of(&reproduction_bytes),
        difference,
        ambient,
    })
}

/// Counts differing positions plus the length difference.
fn differing_bytes(first: &[u8], second: &[u8]) -> usize {
    first
        .iter()
        .zip(second)
        .filter(|(left, right)| left != right)
        .count()
        + first.len().abs_diff(second.len())
}

/// Names the ELF section whose file bytes hold `offset`, if the file parses
/// as a 64-bit ELF and one does.
pub(crate) fn elf_section_at(bytes: &[u8], offset: usize) -> Option<String> {
    let header = FileHeader64::<Endianness>::parse(bytes).ok()?;
    let endian = header.endian().ok()?;
    let sections = header.sections(endian, bytes).ok()?;
    let offset = u64::try_from(offset).ok()?;
    sections.iter().find_map(|section| {
        let start = section.sh_offset(endian);
        let end = start.checked_add(section.sh_size(endian))?;
        let holds = section.sh_type(endian) != SHT_NOBITS && (start..end).contains(&offset);
        holds
            .then(|| sections.section_name(endian, section).ok())
            .flatten()
            .map(|name| String::from_utf8_lossy(name).into_owned())
    })
}

/// Most planted operator values one scan accepts.
const MAX_OPERATOR_VALUES: usize = 16;
/// Shortest planted value: anything shorter could occur by chance.
const MIN_OPERATOR_VALUE_BYTES: usize = 12;

/// Requires the final bytes to hold none of the planted operator `values`.
///
/// Each value is a distinctive fictional string that the caller placed in
/// the inherited environment as operator configuration. Preflight removes
/// such variables, so finding one in the executable means configuration
/// reached the build through another channel.
///
/// # Errors
///
/// Returns [`ReleaseError::Recipe`] for no values, more than 16, a value
/// shorter than 12 bytes, or any value found in `bytes`.
pub(crate) fn check_operator_values_absent(
    bytes: &[u8],
    values: &[&str],
) -> Result<(), Report<ReleaseError>> {
    if values.is_empty() || values.len() > MAX_OPERATOR_VALUES {
        return Err(recipe_error(
            "an operator value scan needs between 1 and 16 planted values",
        ));
    }
    for value in values {
        if value.len() < MIN_OPERATOR_VALUE_BYTES {
            return Err(recipe_error(
                "a planted operator value is too short to be distinctive",
            ));
        }
        if bytes
            .windows(value.len())
            .any(|window| window == value.as_bytes())
        {
            return Err(recipe_error(format!(
                "final bytes embed the planted operator value {value:?}"
            )));
        }
    }
    Ok(())
}
