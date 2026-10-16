use std::path::PathBuf;

use serde_json::Value;

use super::super::ambient::{
    AmbientClaim, AmbientRecord, Dimension, ObservedAmbient, check_profile_drift,
};
use super::*;

const TOOLS: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../scripts/release/release-tools.json"
));

fn check_error(role: Role, profile: &AmbientProfile, observed: ObservedAmbient) -> String {
    match CheckedAmbient::check(role, profile, observed) {
        Ok(_) => panic!("should reject the ambient"),
        Err(error) => format!("{error:?}"),
    }
}

#[test]
fn reproduction_with_canonical_umask_is_rejected() {
    checked_canonical();
    checked_reproduction();
    let reproduction_profile = AmbientProfile::alternate_for_tests();
    let wrong_umask = ObservedAmbient {
        umask: 0o022,
        ..observed_reproduction()
    };
    assert!(
        check_error(Role::Reproduction, &reproduction_profile, wrong_umask).contains("umask"),
        "should reject the reproduction build observed under the canonical umask"
    );
    let canonical_with_reproduction_umask = ObservedAmbient {
        umask: 0o027,
        ..observed_canonical()
    };
    assert!(
        check_error(
            Role::Canonical,
            &AmbientProfile::CANONICAL,
            canonical_with_reproduction_umask
        )
        .contains("umask"),
        "should reject the canonical build observed under the reproduction umask"
    );
    assert!(
        check_error(
            Role::Reproduction,
            &AmbientProfile::CANONICAL,
            observed_canonical()
        )
        .contains("runnable profile"),
        "should reject the reproduction build bound to the canonical profile"
    );
    assert!(
        check_error(
            Role::Canonical,
            &reproduction_profile,
            observed_reproduction()
        )
        .contains("runnable profile"),
        "should reject the canonical build bound to an alternate profile"
    );
    let closed = AmbientProfile::for_role(Role::Reproduction)
        .expect_err("should keep the reproduction build closed");
    assert!(
        format!("{closed:?}").contains("not qualified"),
        "should name the missing qualification"
    );
}

#[test]
fn observed_jobs_mismatch_is_rejected() {
    let canonical_wrong = ObservedAmbient {
        jobs: 2,
        ..observed_canonical()
    };
    assert!(
        check_error(Role::Canonical, &AmbientProfile::CANONICAL, canonical_wrong).contains("jobs"),
        "should reject the canonical build observed with reproduction jobs"
    );
    let reproduction_wrong = ObservedAmbient {
        jobs: 8,
        ..observed_reproduction()
    };
    assert!(
        check_error(
            Role::Reproduction,
            &AmbientProfile::alternate_for_tests(),
            reproduction_wrong
        )
        .contains("jobs"),
        "should reject the reproduction build observed with unrelated jobs"
    );
}

fn claim_error(
    canonical: &CheckedAmbient,
    reproduction: &CheckedAmbient,
    claim: &AmbientClaim,
) -> String {
    match AmbientRecord::pair(canonical, reproduction, claim) {
        Ok(_) => panic!("should reject the pairing"),
        Err(error) => format!("{error:?}"),
    }
}

#[test]
fn normalized_locale_reported_as_varied_is_rejected() {
    let record = AmbientRecord::pair(
        &checked_canonical(),
        &checked_reproduction(),
        &AmbientClaim::standard(),
    )
    .expect("should accept the standard claim");
    assert_eq!(
        record.claim(),
        &AmbientClaim::standard(),
        "should keep the verified placement"
    );
    for dimension in [Dimension::Locale, Dimension::Timezone] {
        let mut claim = AmbientClaim::standard();
        claim.normalized.remove(&dimension);
        claim.varied.insert(dimension);
        assert!(
            claim_error(&checked_canonical(), &checked_reproduction(), &claim)
                .contains("misreports"),
            "should reject {dimension} claimed as varied"
        );
    }
    let leaking_child = ObservedAmbient {
        child_locale: "en_US.UTF-8".to_owned(),
        ..observed_reproduction()
    };
    assert!(
        check_error(
            Role::Reproduction,
            &AmbientProfile::alternate_for_tests(),
            leaking_child
        )
        .contains("child locale"),
        "should reject a compiler child that sees the host locale"
    );
}

#[test]
fn unsupported_execution_user_claimed_as_varied_is_rejected() {
    let mut claim = AmbientClaim::standard();
    claim.unsupported.remove(&Dimension::ExecutionUser);
    claim.varied.insert(Dimension::ExecutionUser);
    assert!(
        claim_error(&checked_canonical(), &checked_reproduction(), &claim).contains("misreports"),
        "should reject an unqualified user claimed as varied"
    );
    let other_user = CheckedAmbient::check(
        Role::Reproduction,
        &AmbientProfile::alternate_for_tests(),
        ObservedAmbient {
            execution_uid: 502,
            ..observed_reproduction()
        },
    )
    .expect("should check the alternate user in isolation");
    assert!(
        claim_error(&checked_canonical(), &other_user, &AmbientClaim::standard())
            .contains("execution user"),
        "should reject an actually different unqualified user"
    );
}

#[test]
fn omitted_ambient_dimension_is_rejected() {
    for dimension in Dimension::ALL {
        let mut claim = AmbientClaim::standard();
        claim.varied.remove(&dimension);
        claim.normalized.remove(&dimension);
        claim.unsupported.remove(&dimension);
        assert!(
            claim_error(&checked_canonical(), &checked_reproduction(), &claim).contains("omits"),
            "should reject a claim omitting {dimension}"
        );
    }
    let mut duplicated = AmbientClaim::standard();
    duplicated.normalized.insert(Dimension::Jobs);
    assert!(
        claim_error(&checked_canonical(), &checked_reproduction(), &duplicated)
            .contains("misreports"),
        "should reject a dimension placed twice"
    );
}

#[test]
fn identical_canonical_and_reproduction_checkout_roots_are_rejected() {
    for root in [
        "/example/release-canonical",
        "/example/release-canonical/nested",
        "/example",
    ] {
        let overlapping = CheckedAmbient::check(
            Role::Reproduction,
            &AmbientProfile::alternate_for_tests(),
            ObservedAmbient {
                checkout_root: PathBuf::from(root),
                ..observed_reproduction()
            },
        )
        .expect("should check the alternate root in isolation");
        assert!(
            claim_error(
                &checked_canonical(),
                &overlapping,
                &AmbientClaim::standard()
            )
            .contains("overlap"),
            "should reject checkout root {root} overlapping the canonical build"
        );
    }
    let same_start = CheckedAmbient::check(
        Role::Reproduction,
        &AmbientProfile::alternate_for_tests(),
        ObservedAmbient {
            started_at_seconds: observed_canonical().started_at_seconds,
            ..observed_reproduction()
        },
    )
    .expect("should check the alternate start in isolation");
    assert!(
        claim_error(&checked_canonical(), &same_start, &AmbientClaim::standard())
            .contains("StartTime"),
        "should reject builds that did not start at different times"
    );
}

#[test]
fn rust_profile_drift_from_release_tools_json_is_rejected() {
    let tools: Value = serde_json::from_str(TOOLS).expect("should parse tracked tools");
    let recipes = tools["recipes"].as_array().expect("should list recipes");
    let mut checked = 0;
    for recipe in recipes
        .iter()
        .filter(|recipe| recipe["script"] == "scripts/release/build.sh")
    {
        check_profile_drift(&recipe["settings"])
            .expect("should accept the tracked ambient profiles");
        checked += 1;
        for (profile, field, value) in [
            (Role::Canonical.spec_label(), "umask", Value::from("027")),
            (Role::Canonical.spec_label(), "jobs", Value::from(2)),
            (Role::Reproduction.spec_label(), "jobs", Value::from(4)),
            (Role::Reproduction.spec_label(), "umask", Value::from("022")),
            (Role::Reproduction.spec_label(), "locale", Value::from("C")),
        ] {
            let mut drifted = recipe["settings"].clone();
            drifted["ambient_profiles"][profile][field] = value;
            assert!(
                check_profile_drift(&drifted)
                    .is_err_and(|error| format!("{error:?}").contains(profile)),
                "should reject drifted {profile}.{field}"
            );
        }
        let mut missing = recipe["settings"].clone();
        missing["ambient_profiles"]
            .as_object_mut()
            .expect("should hold profiles")
            .remove(Role::Reproduction.spec_label());
        assert!(
            check_profile_drift(&missing).is_err_and(|error| {
                format!("{error:?}")
                    .contains(&format!("profile {}", Role::Reproduction.spec_label()))
            }),
            "should reject a missing reproduction profile"
        );
    }
    assert_eq!(checked, 4, "should check every artifact recipe");
}

#[test]
fn reproduction_with_canonical_locale_or_timezone_is_rejected() {
    let same_locale = AmbientProfile::alternate_with_host_for_tests("C", "Pacific/Auckland");
    let observed = ObservedAmbient {
        host_locale: "C".to_owned(),
        ..observed_reproduction()
    };
    assert!(
        check_error(Role::Reproduction, &same_locale, observed).contains("runnable profile"),
        "should reject a reproduction build that keeps the canonical locale"
    );
    let same_timezone = AmbientProfile::alternate_with_host_for_tests("en_US.UTF-8", "UTC");
    let observed = ObservedAmbient {
        host_timezone: "UTC".to_owned(),
        ..observed_reproduction()
    };
    assert!(
        check_error(Role::Reproduction, &same_timezone, observed).contains("runnable profile"),
        "should reject a reproduction build that keeps the canonical timezone"
    );
}
