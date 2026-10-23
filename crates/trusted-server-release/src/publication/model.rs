//! Validated publication inputs: tag, source approval and the exact asset set.

use std::collections::BTreeMap;

use error_stack::Report;

use super::{rejected, sha256_hex};
use crate::documents::Role;
use crate::error::ReleaseError;
use crate::source::CommitId;

const ASSET_NAME_LIMIT: usize = 128;
const TAG_LIMIT: usize = 128;
/// Release-wide assets named by the spec's distribution section.
const RELEASE_WIDE_ASSETS: usize = 4;
/// The release-index v1 schema's `artifacts` array has exactly four items.
const INDEX_ARTIFACTS: usize = 4;
/// Each executable plus its six companion documents.
const ASSETS_PER_ARTIFACT: usize = 7;

fn numeric_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && (value == "0" || !value.starts_with('0'))
}

fn dotted_identifiers(value: &str) -> bool {
    value.split('.').all(|part| {
        !part.is_empty()
            && part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    })
}

/// A strict `v`-prefixed semantic-version release tag.
#[derive(Clone, Debug, PartialEq, Eq, derive_more::Display)]
pub(crate) struct ReleaseTag(String);

impl ReleaseTag {
    /// Parses a `vMAJOR.MINOR.PATCH[-prerelease][+build]` tag without normalization.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Publication`] for any other shape, leading zeros
    /// or empty identifiers.
    pub(crate) fn parse(value: &str) -> Result<Self, Report<ReleaseError>> {
        let invalid = || {
            rejected(format!(
                "release tag `{value}` is not a strict v-SemVer tag"
            ))
        };
        if value.len() > TAG_LIMIT {
            return Err(invalid());
        }
        let version = value.strip_prefix('v').ok_or_else(invalid)?;
        let (version, build) = match version.split_once('+') {
            Some((version, build)) => (version, Some(build)),
            None => (version, None),
        };
        let (core, prerelease) = match version.split_once('-') {
            Some((core, prerelease)) => (core, Some(prerelease)),
            None => (version, None),
        };
        let core_valid = core.split('.').count() == 3 && core.split('.').all(numeric_identifier);
        let prerelease_valid = prerelease.is_none_or(|value| {
            dotted_identifiers(value)
                && value.split('.').all(|part| {
                    !part.bytes().all(|byte| byte.is_ascii_digit()) || numeric_identifier(part)
                })
        });
        let build_valid = build.is_none_or(dotted_identifiers);
        if !(core_valid && prerelease_valid && build_valid) {
            return Err(invalid());
        }
        Ok(Self(value.to_owned()))
    }

    /// The tag text.
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

/// How the tagged source commit was approved for release.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SourceApproval {
    /// Ancestor of the approved `main` tip.
    Main,
    /// Ancestor of an explicitly reviewed exact `rc/YYYYMM` branch tip.
    ReviewedRc {
        /// The reviewed `rc/YYYYMM` branch name.
        branch: String,
    },
}

impl SourceApproval {
    /// Records a reviewed exact `rc/YYYYMM` branch approval.
    ///
    /// Approval kind never changes publication permissions; the same
    /// contents-write-only [`super::role::PublicationRole`] applies.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Publication`] unless `branch` is exactly
    /// `rc/YYYYMM` with a month from 01 to 12.
    pub(crate) fn reviewed_rc(branch: &str) -> Result<Self, Report<ReleaseError>> {
        let valid = branch.strip_prefix("rc/").is_some_and(|stamp| {
            stamp.len() == 6
                && stamp.bytes().all(|byte| byte.is_ascii_digit())
                && matches!(stamp[4..].parse::<u8>(), Ok(1..=12))
        });
        if !valid {
            return Err(rejected(format!(
                "release-candidate branch `{branch}` is not an exact rc/YYYYMM branch"
            )));
        }
        Ok(Self::ReviewedRc {
            branch: branch.to_owned(),
        })
    }
}

/// One asset the publication owner intends to attach.
#[derive(Clone, Debug)]
pub(crate) struct IntendedAsset {
    name: String,
    sha256: String,
    bytes: Vec<u8>,
}

impl IntendedAsset {
    /// Records an asset's exact bytes under a portable release asset name.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Publication`] for an empty, overlong, hidden or
    /// non-portable name.
    pub(crate) fn new(name: &str, bytes: Vec<u8>) -> Result<Self, Report<ReleaseError>> {
        let portable = !name.is_empty()
            && name.len() <= ASSET_NAME_LIMIT
            && !name.starts_with('.')
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
        if !portable {
            return Err(rejected(format!(
                "release asset name `{name}` is not a portable asset name"
            )));
        }
        Ok(Self {
            name: name.to_owned(),
            sha256: sha256_hex(&bytes),
            bytes,
        })
    }

    /// The asset name.
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// Lowercase hex SHA-256 of the exact bytes.
    pub(crate) fn sha256(&self) -> &str {
        &self.sha256
    }

    /// The exact bytes.
    pub(crate) fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// The exact size in bytes.
    pub(crate) fn size(&self) -> u64 {
        u64::try_from(self.bytes.len()).unwrap_or(u64::MAX)
    }
}

/// Count and byte bounds on an intended asset set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AssetBounds {
    count: usize,
    asset_bytes: u64,
    total_bytes: u64,
}

impl AssetBounds {
    pub(super) fn new(count: usize, asset_bytes: u64, total_bytes: u64) -> Self {
        Self {
            count,
            asset_bytes,
            total_bytes,
        }
    }

    /// Bounds for one official release, derived from existing contracts.
    ///
    /// - Count: the spec's release-wide assets (index, index provenance
    ///   bundle, JS inventory, `SHA256SUMS`) plus, for each of the index
    ///   schema's exactly four artifacts, the executable and its six
    ///   companions — 32 in total.
    /// - Per asset: the largest published role limit,
    ///   [`Role::Executable`].
    /// - Total: four executables at that limit plus every other asset at the
    ///   largest companion role limit,
    ///   [`Role::Companion`].
    pub(crate) fn release() -> Self {
        let count = RELEASE_WIDE_ASSETS + INDEX_ARTIFACTS * ASSETS_PER_ARTIFACT;
        let executable = Role::Executable.limit();
        let companion = Role::Companion.limit();
        let total = executable
            .saturating_mul(INDEX_ARTIFACTS as u64)
            .saturating_add(companion.saturating_mul((count - INDEX_ARTIFACTS) as u64));
        Self::new(count, executable, total)
    }

    /// Maximum asset count.
    pub(crate) fn count(&self) -> usize {
        self.count
    }

    /// Maximum bytes per asset.
    pub(crate) fn asset_bytes(&self) -> u64 {
        self.asset_bytes
    }
}

/// The complete release the sole owner publishes for one tag.
#[derive(Clone, Debug)]
pub(crate) struct IntendedRelease {
    tag: ReleaseTag,
    commit: CommitId,
    approval: SourceApproval,
    assets: BTreeMap<String, IntendedAsset>,
}

impl IntendedRelease {
    /// Binds the exact asset set to its tag and approved source commit.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Publication`] for an empty set, duplicate names
    /// or a set outside [`AssetBounds::release`].
    pub(crate) fn new(
        tag: ReleaseTag,
        commit: CommitId,
        approval: SourceApproval,
        assets: Vec<IntendedAsset>,
    ) -> Result<Self, Report<ReleaseError>> {
        Self::new_bounded(tag, commit, approval, assets, &AssetBounds::release())
    }

    pub(super) fn new_bounded(
        tag: ReleaseTag,
        commit: CommitId,
        approval: SourceApproval,
        assets: Vec<IntendedAsset>,
        bounds: &AssetBounds,
    ) -> Result<Self, Report<ReleaseError>> {
        if assets.is_empty() {
            return Err(rejected("release asset set must not be empty"));
        }
        if assets.len() > bounds.count {
            return Err(rejected(format!(
                "release asset set has {} assets; at most {} are allowed",
                assets.len(),
                bounds.count
            )));
        }
        let mut total: u64 = 0;
        for asset in &assets {
            if asset.size() > bounds.asset_bytes {
                return Err(rejected(format!(
                    "release asset `{}` exceeds the {}-byte asset bound",
                    asset.name, bounds.asset_bytes
                )));
            }
            total = total.saturating_add(asset.size());
        }
        if total > bounds.total_bytes {
            return Err(rejected(format!(
                "release asset set exceeds the {}-byte total bound",
                bounds.total_bytes
            )));
        }
        let mut by_name = BTreeMap::new();
        for asset in assets {
            let name = asset.name.clone();
            if by_name.insert(name.clone(), asset).is_some() {
                return Err(rejected(format!("release asset `{name}` is duplicated")));
            }
        }
        Ok(Self {
            tag,
            commit,
            approval,
            assets: by_name,
        })
    }

    /// The release tag.
    pub(crate) fn tag(&self) -> &ReleaseTag {
        &self.tag
    }

    /// The approved source commit.
    pub(crate) fn commit(&self) -> &CommitId {
        &self.commit
    }

    /// The source approval.
    pub(crate) fn approval(&self) -> &SourceApproval {
        &self.approval
    }

    /// The exact asset set keyed by name.
    pub(crate) fn assets(&self) -> &BTreeMap<String, IntendedAsset> {
        &self.assets
    }
}

/// Tag-scoped serialization of publication runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConcurrencyPolicy {
    group: String,
}

impl ConcurrencyPolicy {
    /// The concurrency group serializing every publication run of `tag`.
    pub(crate) fn for_tag(tag: &ReleaseTag) -> Self {
        Self {
            group: format!("release-publication-{}", tag.as_str()),
        }
    }

    /// The tag-scoped group name.
    pub(crate) fn group(&self) -> &str {
        &self.group
    }

    /// A publishing run is never cancelled by a newer run.
    pub(crate) fn cancel_in_progress(&self) -> bool {
        false
    }
}
