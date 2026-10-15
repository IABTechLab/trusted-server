//! Ambient build conditions: compiled profiles, observations and A/B pairing.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use error_stack::Report;
use serde_json::{Value, json};

use super::fresh::{disjoint, normal_absolute};
use super::{Role, recipe_error};
use crate::boundary::Umask;
use crate::error::ReleaseError;

/// Locale every compiler child sees, whatever the host reports.
const NORMALIZED_LOCALE: &str = "C";
/// Timezone every compiler child sees, whatever the host reports.
const NORMALIZED_TIMEZONE: &str = "UTC";

/// A qualified alternate B profile; none exists, so production B stays closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum QualifiedAlternate {}

/// One compiled ambient profile an independent build must run under.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AmbientProfile {
    checkout_root: &'static str,
    execution_user: &'static str,
    jobs: u32,
    locale: &'static str,
    timezone: &'static str,
    umask: u32,
}

impl AmbientProfile {
    /// Profile of the first build; its symbolic user and root are qualification-selected.
    pub(crate) const A: Self = Self {
        checkout_root: "external-release-A",
        execution_user: "qualification-selected",
        jobs: 4,
        locale: NORMALIZED_LOCALE,
        timezone: NORMALIZED_TIMEZONE,
        umask: 0o022,
    };

    /// Placeholder of the second build; its alternates await qualification.
    pub(crate) const B_PENDING: Self = Self {
        checkout_root: "external-release-B",
        execution_user: "qualified-alternate-or-unsupported",
        jobs: 2,
        locale: "qualification-selected-alternate",
        timezone: "qualification-selected-alternate",
        umask: 0o027,
    };

    /// Selects the production profile of `role`.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Unqualified`] for role B: its alternate
    /// locale, timezone and user have not been qualified.
    pub(crate) fn for_role(role: Role) -> Result<Self, Report<ReleaseError>> {
        match role {
            Role::A => Ok(Self::A),
            Role::B => Err(Report::new(ReleaseError::Unqualified)
                .attach("role B alternate ambient profile is not qualified")),
        }
    }

    /// Accepts only a qualified alternate; production construction stays closed.
    pub(crate) fn alternate(qualification: QualifiedAlternate) -> Self {
        match qualification {}
    }

    /// Selects a fictional alternate for tests without asserting qualification.
    #[cfg(test)]
    pub(crate) fn alternate_for_tests() -> Self {
        Self::alternate_with_host_for_tests("en_US.UTF-8", "Pacific/Auckland")
    }

    /// As [`Self::alternate_for_tests`], with an explicit host locale and timezone.
    #[cfg(test)]
    pub(crate) fn alternate_with_host_for_tests(
        locale: &'static str,
        timezone: &'static str,
    ) -> Self {
        Self {
            locale,
            timezone,
            execution_user: "qualification-selected",
            ..Self::B_PENDING
        }
    }

    /// Renders the profile exactly as `release-tools.json` states it.
    pub(crate) fn to_json(&self) -> Value {
        json!({
            "checkout_root": self.checkout_root,
            "execution_user": self.execution_user,
            "jobs": self.jobs,
            "locale": self.locale,
            "timezone": self.timezone,
            "umask": format!("{:03o}", self.umask),
        })
    }
}

/// Fails when tracked `ambient_profiles` settings differ from the compiled profiles.
///
/// # Errors
///
/// Returns [`ReleaseError::Recipe`] naming the profile that drifted.
pub(crate) fn check_profile_drift(settings: &Value) -> Result<(), Report<ReleaseError>> {
    for (name, compiled) in [("A", AmbientProfile::A), ("B", AmbientProfile::B_PENDING)] {
        if settings["ambient_profiles"][name] != compiled.to_json() {
            return Err(recipe_error(format!(
                "tracked ambient profile {name} differs from the compiled profile"
            )));
        }
    }
    Ok(())
}

/// One independently varied or normalized build condition.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, derive_more::Display)]
pub(crate) enum Dimension {
    /// Process umask.
    Umask,
    /// Cargo job count.
    Jobs,
    /// Checkout root path.
    CheckoutRoot,
    /// Build start time.
    StartTime,
    /// Host and compiler locale.
    Locale,
    /// Host and compiler timezone.
    Timezone,
    /// Executing user.
    ExecutionUser,
}

impl Dimension {
    /// Every dimension a pair must account for.
    pub(crate) const ALL: [Self; 7] = [
        Self::Umask,
        Self::Jobs,
        Self::CheckoutRoot,
        Self::StartTime,
        Self::Locale,
        Self::Timezone,
        Self::ExecutionUser,
    ];
}

/// How one dimension is reported in the A/B pair.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Treatment {
    Varied,
    Normalized,
    Unsupported,
}

/// Conditions an observer reported for one build; carries no authority itself.
#[derive(Clone, Debug)]
pub(crate) struct ObservedAmbient {
    /// Process umask of the build, as a numeric mode.
    pub(crate) umask: u32,
    /// Cargo job count of the build.
    pub(crate) jobs: u32,
    /// Locale of the host session.
    pub(crate) host_locale: String,
    /// Timezone of the host session.
    pub(crate) host_timezone: String,
    /// Locale the compiler child saw.
    pub(crate) child_locale: String,
    /// Timezone the compiler child saw.
    pub(crate) child_timezone: String,
    /// Numeric id of the executing user.
    pub(crate) execution_uid: u32,
    /// Absolute checkout root of the build.
    pub(crate) checkout_root: PathBuf,
    /// Unix start time of the build in seconds.
    pub(crate) started_at_seconds: u64,
}

/// Observed conditions that match the compiled profile of one role.
#[derive(Clone, Debug)]
pub(crate) struct CheckedAmbient {
    role: Role,
    profile: AmbientProfile,
    observed: ObservedAmbient,
}

impl CheckedAmbient {
    /// Checks `observed` against the profile compiled for `role`.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Recipe`] when the profile does not belong to
    /// the role, or the observed umask, jobs, host locale or timezone differ
    /// from it, the compiler child is not normalized, or the checkout root is
    /// not an absolute normal path.
    pub(crate) fn check(
        role: Role,
        profile: &AmbientProfile,
        observed: ObservedAmbient,
    ) -> Result<Self, Report<ReleaseError>> {
        check_profile_role(role, profile)?;
        same("umask", &observed.umask, &profile.umask)?;
        same("jobs", &observed.jobs, &profile.jobs)?;
        same(
            "host locale",
            &observed.host_locale.as_str(),
            &profile.locale,
        )?;
        same(
            "host timezone",
            &observed.host_timezone.as_str(),
            &profile.timezone,
        )?;
        same(
            "child locale",
            &observed.child_locale.as_str(),
            &NORMALIZED_LOCALE,
        )?;
        same(
            "child timezone",
            &observed.child_timezone.as_str(),
            &NORMALIZED_TIMEZONE,
        )?;
        normal_absolute("checkout root", &observed.checkout_root)?;
        Ok(Self {
            role,
            profile: profile.clone(),
            observed,
        })
    }

    /// Returns the checked role.
    pub(crate) fn role(&self) -> Role {
        self.role
    }

    /// Returns the Cargo job count the profile fixes.
    pub(crate) fn jobs(&self) -> u32 {
        self.profile.jobs
    }

    /// Returns the umask the profile fixes.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Process`] when the compiled mask is not a
    /// valid permission mask.
    pub(crate) fn umask(&self) -> Result<Umask, Report<ReleaseError>> {
        Umask::new(self.profile.umask)
    }

    /// Returns the checkout root this build runs in.
    pub(crate) fn checkout_root(&self) -> &Path {
        &self.observed.checkout_root
    }
}

/// Requires an observed value to equal the profile's value.
fn same<T: PartialEq + std::fmt::Debug>(
    name: &str,
    observed: &T,
    expected: &T,
) -> Result<(), Report<ReleaseError>> {
    if observed == expected {
        return Ok(());
    }
    Err(recipe_error(format!(
        "observed {name} {observed:?} differs from profile {expected:?}"
    )))
}

/// Requires the profile to be the runnable one compiled for `role`.
fn check_profile_role(role: Role, profile: &AmbientProfile) -> Result<(), Report<ReleaseError>> {
    let belongs = match role {
        Role::A => *profile == AmbientProfile::A,
        Role::B => {
            *profile != AmbientProfile::B_PENDING
                && profile.umask != AmbientProfile::A.umask
                && profile.jobs != AmbientProfile::A.jobs
                && profile.locale != AmbientProfile::A.locale
                && profile.timezone != AmbientProfile::A.timezone
        }
    };
    if belongs {
        return Ok(());
    }
    Err(recipe_error(format!(
        "ambient profile is not a runnable profile for role {role:?}"
    )))
}

/// A claimed placement of every dimension, as an attestation would state it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AmbientClaim {
    pub(crate) varied: BTreeSet<Dimension>,
    pub(crate) normalized: BTreeSet<Dimension>,
    pub(crate) unsupported: BTreeSet<Dimension>,
}

impl AmbientClaim {
    /// Returns the only honest placement of the seven dimensions.
    pub(crate) fn standard() -> Self {
        let place = |treatment| {
            Dimension::ALL
                .into_iter()
                .filter(|dimension| expected_treatment(*dimension) == treatment)
                .collect()
        };
        Self {
            varied: place(Treatment::Varied),
            normalized: place(Treatment::Normalized),
            unsupported: place(Treatment::Unsupported),
        }
    }

    fn placements(&self, dimension: Dimension) -> Vec<Treatment> {
        [
            (&self.varied, Treatment::Varied),
            (&self.normalized, Treatment::Normalized),
            (&self.unsupported, Treatment::Unsupported),
        ]
        .into_iter()
        .filter(|(set, _)| set.contains(&dimension))
        .map(|(_, treatment)| treatment)
        .collect()
    }
}

fn expected_treatment(dimension: Dimension) -> Treatment {
    match dimension {
        Dimension::Umask | Dimension::Jobs | Dimension::CheckoutRoot | Dimension::StartTime => {
            Treatment::Varied
        }
        Dimension::Locale | Dimension::Timezone => Treatment::Normalized,
        Dimension::ExecutionUser => Treatment::Unsupported,
    }
}

/// Requires every dimension that must vary to vary, and the user to match.
fn check_variation(
    first: &ObservedAmbient,
    second: &ObservedAmbient,
) -> Result<(), Report<ReleaseError>> {
    for (dimension, differs) in [
        (Dimension::Umask, first.umask != second.umask),
        (Dimension::Jobs, first.jobs != second.jobs),
        (
            Dimension::StartTime,
            first.started_at_seconds != second.started_at_seconds,
        ),
    ] {
        if !differs {
            return Err(recipe_error(format!(
                "{dimension} is identical across the A and B builds"
            )));
        }
    }
    disjoint(&[
        ("checkout root A", first.checkout_root.as_path()),
        ("checkout root B", second.checkout_root.as_path()),
    ])?;
    if first.execution_uid != second.execution_uid {
        return Err(recipe_error(
            "execution user differs but no alternate user is qualified",
        ));
    }
    Ok(())
}

/// Requires the claim to place every dimension exactly once, as expected.
fn check_claim(claim: &AmbientClaim) -> Result<(), Report<ReleaseError>> {
    for dimension in Dimension::ALL {
        let placements = claim.placements(dimension);
        if placements.is_empty() {
            return Err(recipe_error(format!("claim omits {dimension}")));
        }
        if placements != [expected_treatment(dimension)] {
            return Err(recipe_error(format!("claim misreports {dimension}")));
        }
    }
    Ok(())
}

/// A verified A/B ambient pairing.
#[derive(Clone, Debug)]
pub(crate) struct AmbientRecord {
    claim: AmbientClaim,
}

impl AmbientRecord {
    /// Verifies that the A and B builds varied exactly what the claim states.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Recipe`] when the roles are not A then B, a
    /// varied dimension did not vary, checkout roots overlap, the execution
    /// user differs without qualification, or the claim omits or misplaces a
    /// dimension.
    pub(crate) fn pair(
        first: &CheckedAmbient,
        second: &CheckedAmbient,
        claim: &AmbientClaim,
    ) -> Result<Self, Report<ReleaseError>> {
        if first.role != Role::A || second.role != Role::B {
            return Err(recipe_error("ambient pair must be role A then role B"));
        }
        check_variation(&first.observed, &second.observed)?;
        check_claim(claim)?;
        Ok(Self {
            claim: claim.clone(),
        })
    }

    /// Returns the verified placement of every dimension.
    pub(crate) fn claim(&self) -> &AmbientClaim {
        &self.claim
    }
}
