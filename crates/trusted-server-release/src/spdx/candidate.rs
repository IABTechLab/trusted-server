//! Unqualified candidate whole-document SPDX that states its coverage gaps.
//!
//! The closed [`super::generate_from_completed`] path stays closed: a
//! [`CandidateSpdx`] is never converted into a [`super::GeneratedSpdx`]. Its
//! creation comment names the document a candidate and lists every recorded
//! coverage gap, so it grants no signing, publication or qualification
//! authority.

use std::collections::BTreeSet;

use error_stack::Report;
use sha2::{Digest as _, Sha256};

use super::cargo::CargoProjection;
use super::javascript::JavascriptProjection;
use super::native::NativeProjection;
use super::platform::PlatformProjection;
use super::{Component, Dependency, SpdxInputs, lower_hex, plain, render, spdx_error};
use crate::composition::CompositionView;
use crate::composition::origin::Original;
use crate::documents::Role;
use crate::error::ReleaseError;
use crate::recipe::RecipeCompleted;
use crate::source::ApprovedSource;

#[cfg(test)]
mod tests;

const SECONDS_PER_DAY: u64 = 86_400;
const LAST_FOUR_DIGIT_YEAR: u64 = 9_999;
const MAX_ROOT_VERSION_LENGTH: usize = 128;

/// Validated bounded candidate SPDX bytes with their explicit coverage gaps.
///
/// This type has no conversion into [`super::GeneratedSpdx`] and carries no
/// release-authorization capability.
#[derive(Debug)]
pub(crate) struct CandidateSpdx {
    bytes: Vec<u8>,
    sha256: String,
    gaps: Vec<String>,
}

impl CandidateSpdx {
    /// The exact serialized candidate SPDX document bytes.
    pub(crate) fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// SHA-256 of the exact document bytes, distinct from its executable root.
    pub(crate) fn sha256(&self) -> &str {
        &self.sha256
    }

    /// Sorted, unique coverage gaps stated in the document's creation comment.
    pub(crate) fn gaps(&self) -> &[String] {
        &self.gaps
    }
}

/// Renders a gap-bearing candidate document for one completed build.
///
/// All four projections must come from the same sealed composition receipt,
/// and the root package must be the policy asset's workspace package at the
/// approved commit.
///
/// # Errors
///
/// Rejects another approved source, missing or changed composition receipts,
/// a platform projection over another receipt, an unverified root package,
/// overlapping or dangling projected parts, an empty required part, a
/// gap-free candidate, an unrepresentable creation time, changed final bytes
/// or a document beyond the SPDX size limit.
pub(crate) fn candidate_from_completed(
    completed: &RecipeCompleted,
    source: &ApprovedSource,
) -> Result<CandidateSpdx, Report<ReleaseError>> {
    if source.commit() != completed.commit() {
        return Err(spdx_error(
            "approved source differs from the completed build commit",
        ));
    }
    let created = spdx_created(source.epoch())?;
    let view = completed.composition_view()?;
    let rust = CargoProjection::from_completed(completed)?;
    let native = NativeProjection::from_completed(completed)?;
    let javascript = JavascriptProjection::from_completed(completed)?;
    let platform = PlatformProjection::from_completed(completed)?;
    if platform.receipt_sha256() != view.receipt_sha256() {
        return Err(spdx_error(
            "platform projection belongs to another composition receipt",
        ));
    }
    let version = root_version(completed, &view)?;
    let gaps = platform.coverage_gaps();
    let (components, dependencies) = merge_parts(CandidateParts {
        rust: rust.into_parts(),
        native: native.into_parts(),
        javascript: javascript.into_parts(),
        platform: platform.into_parts(),
    })?;
    let executable = completed.read_final_bytes()?;
    let artifact = completed.asset().artifact();
    let inputs = SpdxInputs {
        executable: &executable,
        expected_artifact_sha256: completed.artifact().sha256().to_owned(),
        name: artifact.name.to_owned(),
        version,
        source_commit: completed.commit().as_str().to_owned(),
        target: artifact.target,
        created,
        document_comment: None,
        components,
        dependencies,
    };
    render_candidate(inputs, gaps)
}

/// Formats `SOURCE_DATE_EPOCH` as an SPDX `created` UTC timestamp.
///
/// Uses proleptic Gregorian civil-date arithmetic for days since 1970-01-01.
fn spdx_created(epoch: u64) -> Result<String, Report<ReleaseError>> {
    let days = epoch / SECONDS_PER_DAY;
    let seconds_of_day = epoch % SECONDS_PER_DAY;
    let shifted = days
        .checked_add(719_468)
        .ok_or_else(|| spdx_error("source epoch exceeds the SPDX timestamp range"))?;
    let era = shifted / 146_097;
    let day_of_era = shifted % 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = era * 400 + year_of_era + u64::from(month <= 2);
    if year > LAST_FOUR_DIGIT_YEAR {
        return Err(spdx_error(
            "source epoch is beyond the four-digit SPDX year range",
        ));
    }
    Ok(format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        seconds_of_day / 3_600,
        seconds_of_day % 3_600 / 60,
        seconds_of_day % 60
    ))
}

/// Pure facts joining the policy asset package to its verified original.
struct RootIdentityInputs<'a> {
    asset_package: &'a str,
    observed_name: &'a str,
    observed_version: &'a str,
    origin_name: &'a str,
    origin_version: &'a str,
    original: &'a Original,
    source_commit: &'a str,
}

/// Returns the root version when the asset package is the approved workspace package.
fn checked_root_version(inputs: &RootIdentityInputs<'_>) -> Result<String, Report<ReleaseError>> {
    plain(inputs.asset_package)?;
    if inputs.asset_package.contains(['/', '\\'])
        || !lower_hex(inputs.source_commit, 40)
        || inputs.observed_name != inputs.asset_package
        || inputs.origin_name != inputs.asset_package
        || inputs.origin_version != inputs.observed_version
        || inputs.observed_version.is_empty()
        || inputs.observed_version.len() > MAX_ROOT_VERSION_LENGTH
        || inputs
            .observed_version
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
    {
        return Err(spdx_error(
            "candidate root is not the verified policy asset package version",
        ));
    }
    let expected_manifest = format!("crates/{}/Cargo.toml", inputs.asset_package);
    match inputs.original {
        Original::Workspace {
            source_commit,
            path,
        } if source_commit == inputs.source_commit
            && path.to_str() == Some(expected_manifest.as_str()) =>
        {
            Ok(inputs.observed_version.to_owned())
        }
        _ => Err(spdx_error(
            "candidate root has no matching approved workspace origin",
        )),
    }
}

/// Joins the selected Cargo package to the policy asset through its verified original.
fn root_version(
    completed: &RecipeCompleted,
    view: &CompositionView<'_>,
) -> Result<String, Report<ReleaseError>> {
    let id = view.graph().selected_package_id();
    let mut packages = view
        .observations()
        .packages()
        .iter()
        .filter(|package| package.id() == id);
    let package = packages
        .next()
        .ok_or_else(|| spdx_error("candidate has no selected root package"))?;
    if packages.next().is_some() {
        return Err(spdx_error("candidate selected root package is ambiguous"));
    }
    let origin = view
        .package_origins()
        .package(id)
        .ok_or_else(|| spdx_error("candidate selected root package has no verified origin"))?;
    checked_root_version(&RootIdentityInputs {
        asset_package: completed.asset().artifact().package,
        observed_name: package.name(),
        observed_version: package.version(),
        origin_name: origin.name(),
        origin_version: origin.version(),
        original: origin.origin(),
        source_commit: completed.commit().as_str(),
    })
}

type ProjectedPart = (Vec<Component>, Vec<Dependency>);

/// The four adapter projections of one sealed composition receipt.
struct CandidateParts {
    rust: ProjectedPart,
    native: ProjectedPart,
    javascript: ProjectedPart,
    platform: ProjectedPart,
}

/// Concatenates disjoint parts whose edges stay inside their own part.
///
/// Native may be empty (an artifact can have no native contributors); Rust,
/// JavaScript and platform parts may not.
fn merge_parts(parts: CandidateParts) -> Result<ProjectedPart, Report<ReleaseError>> {
    let CandidateParts {
        rust,
        native,
        javascript,
        platform,
    } = parts;
    let mut identities = BTreeSet::new();
    let mut components = Vec::new();
    let mut dependencies = Vec::new();
    for (label, required, (part_components, part_dependencies)) in [
        ("Rust", true, rust),
        ("native", false, native),
        ("JavaScript", true, javascript),
        ("platform", true, platform),
    ] {
        if required && part_components.is_empty() {
            return Err(spdx_error(format!("candidate {label} projection is empty")));
        }
        let local = part_components
            .iter()
            .map(|component| component.identity.as_str())
            .collect::<BTreeSet<_>>();
        if local.len() != part_components.len()
            || part_dependencies.iter().any(|dependency| {
                !local.contains(dependency.from.as_str()) || !local.contains(dependency.to.as_str())
            })
        {
            return Err(spdx_error(format!(
                "candidate {label} projection repeats an identity or has a dangling edge"
            )));
        }
        for component in &part_components {
            if !identities.insert(component.identity.clone()) {
                return Err(spdx_error(format!(
                    "candidate {label} projection repeats another adapter's identity"
                )));
            }
        }
        components.extend(part_components);
        dependencies.extend(part_dependencies);
    }
    Ok((components, dependencies))
}

/// Renders validated inputs with a creation comment naming every gap.
fn render_candidate(
    mut inputs: SpdxInputs<'_>,
    gaps: Vec<String>,
) -> Result<CandidateSpdx, Report<ReleaseError>> {
    let gaps = gaps
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    if gaps.is_empty() {
        return Err(spdx_error(
            "candidate SPDX requires at least one stated coverage gap",
        ));
    }
    for gap in &gaps {
        plain(gap)?;
    }
    inputs.document_comment = Some(format!(
        "Unqualified candidate composition document; not a complete SBOM and grants no signing, publication or qualification authority. Known coverage gaps: {}.",
        gaps.join(", ")
    ));
    let generated = render(&inputs)?;
    bounded_candidate(generated.bytes, gaps)
}

/// Wraps rendered bytes after enforcing the SPDX document size limit.
fn bounded_candidate(
    bytes: Vec<u8>,
    gaps: Vec<String>,
) -> Result<CandidateSpdx, Report<ReleaseError>> {
    if bytes.is_empty() || bytes.len() as u64 > Role::Spdx.limit() {
        return Err(spdx_error("candidate SPDX bytes exceed the document limit"));
    }
    let sha256 = hex::encode(Sha256::digest(&bytes));
    Ok(CandidateSpdx {
        bytes,
        sha256,
        gaps,
    })
}
