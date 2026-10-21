//! Checks of the candidate index entry each real CI build constructs from
//! its canonical completed build and actual acceptance, and the summary it
//! adds to that job's retained measurement.
//!
//! The four entries come from different CI jobs, so they are never joined
//! here; each job only records its own entry and JS inventory digest, which
//! later shows whether the four inventories differ.

use serde_json::Value;

use super::spdx_candidate::check_platform_gaps;
use super::*;
use crate::distribution::{CandidateAcceptance, candidate_entry_from_completed};
use crate::policy::{FIXED_ARTIFACTS, ReleaseTarget};

/// Gap every native entry states while no reproduction comparison exists.
const NATIVE_REPRODUCTION_GAP: &str = "native-reproduction-unavailable";

/// The rebuild-check outcome a canonical entry for `target` must record.
fn expected_rebuild_outcome(target: ReleaseTarget) -> &'static str {
    match target {
        ReleaseTarget::Wasm32Wasip1 => "matched",
        ReleaseTarget::X86_64UnknownLinuxGnu
        | ReleaseTarget::Aarch64AppleDarwin
        | ReleaseTarget::X86_64AppleDarwin => "unavailable",
    }
}

/// SHA-256 of the exact original JS inventory this build consumed.
///
/// # Panics
///
/// Panics when the completed build retains no original JS inventory.
pub(super) fn js_inventory_sha256(completed: &RecipeCompleted) -> String {
    completed
        .javascript_contributions()
        .expect("should retain the original JS inventory")
        .inventory_sha256()
        .to_owned()
}

/// Builds and rechecks the candidate entry of one real canonical build and
/// returns its measurement summary.
///
/// # Panics
///
/// Panics when the entry cannot be constructed, its rendered artifact fails
/// the `#/$defs/artifact` sub-schema, a producer join fails, a companion's
/// recomputed digest or size differs, the gaps miss the target family's
/// platform gaps, or the rebuild-check outcome is not the expected one.
pub(super) fn check_real_entry(
    completed: &RecipeCompleted,
    source: &ApprovedSource,
    acceptance: &CandidateAcceptance<'_>,
) -> Value {
    let entry =
        candidate_entry_from_completed(completed, source, acceptance).unwrap_or_else(|error| {
            panic!("should build the candidate entry of the real canonical build: {error:?}")
        });
    let artifact = completed.asset().artifact();
    assert_eq!(
        entry.name(),
        artifact.name,
        "should name the completed asset"
    );
    let summary = entry.recheck_for_measurement().unwrap_or_else(|error| {
        panic!("should recheck the real entry's schema, joins and companions: {error:?}")
    });
    assert_eq!(
        summary.rebuild_outcome(),
        expected_rebuild_outcome(artifact.target),
        "should record the {} rebuild-check outcome",
        artifact.target.triple()
    );
    assert_eq!(
        summary.inventory_sha256(),
        js_inventory_sha256(completed),
        "should name the exact JS inventory this build consumed"
    );
    assert!(!entry.gaps().is_empty(), "should state its gaps");
    check_platform_gaps(completed, entry.gaps());
    assert_eq!(
        entry
            .gaps()
            .iter()
            .any(|gap| gap == NATIVE_REPRODUCTION_GAP),
        artifact.target != ReleaseTarget::Wasm32Wasip1,
        "should state the native reproduction gap exactly for native assets"
    );
    summary.measurement()
}

#[test]
fn only_the_fastly_asset_expects_a_matched_rebuild_check() {
    let outcomes: Vec<(&str, &str)> = FIXED_ARTIFACTS
        .iter()
        .map(|fixed| (fixed.name, expected_rebuild_outcome(fixed.target)))
        .collect();

    assert_eq!(
        outcomes,
        [
            ("trusted-server-fastly.wasm", "matched"),
            ("ts-aarch64-apple-darwin", "unavailable"),
            ("ts-x86_64-apple-darwin", "unavailable"),
            ("ts-x86_64-unknown-linux-gnu", "unavailable"),
        ],
        "should expect a matched comparison only for the Fastly asset"
    );
}
