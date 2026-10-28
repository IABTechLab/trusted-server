//! Checks of the candidate index entry each real CI build constructs from
//! its canonical completed build and actual acceptance, and the summary it
//! adds to that job's retained measurement.
//!
//! The four entries come from different CI jobs, so they are never joined
//! here. Each job records its own entry and JS inventory digest and exports
//! the rechecked entry, with its final executable, as flat files that the
//! ordinary-CI collector (`distribution::ordinary`) downloads, rebuilds and
//! joins into the unqualified candidate index.

use std::path::Path;

use serde_json::Value;

use super::candidate::js_inventory_sha256;
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

/// Where one real candidate job exports its entry for the index collector.
pub(super) struct EntryExport<'a> {
    /// New directory whose flat files the job uploads.
    pub(super) upload_directory: &'a Path,
    /// New transfer file, outside the upload, the job's describe step reads.
    pub(super) transfer_file: &'a Path,
}

/// Builds and rechecks the candidate entry of one real canonical build,
/// exports it for the ordinary-CI index collector and returns its
/// measurement summary.
///
/// # Panics
///
/// Panics when the entry cannot be constructed, its rendered artifact fails
/// the `#/$defs/artifact` sub-schema, a producer join fails, a companion's
/// recomputed digest or size differs, the gaps miss the target family's
/// platform gaps, the rebuild-check outcome is not the expected one, or the
/// final executable or export cannot be read or written.
pub(super) fn check_real_entry(
    completed: &RecipeCompleted,
    source: &ApprovedSource,
    acceptance: &CandidateAcceptance<'_>,
    export: &EntryExport<'_>,
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
    let final_bytes = completed
        .read_final_bytes()
        .expect("should read the rehashed final executable for export");
    entry
        .export_flat_for_ordinary_ci(&final_bytes, export.upload_directory, export.transfer_file)
        .unwrap_or_else(|error| {
            panic!("should export the real entry for the index collector: {error:?}")
        });
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
