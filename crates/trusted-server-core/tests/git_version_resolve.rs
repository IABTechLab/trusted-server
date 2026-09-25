//! The version-resolution rule `build.rs` uses for `TS_GIT_VERSION`.

#[path = "../build_support/git_version.rs"]
mod git_version;

use git_version::{Candidates, resolve_git_version};

const COMMIT: &str = "abcdef0123456789abcdef0123456789abcdef01";

fn local(exact_tag: Option<&'static str>, branch: Option<&'static str>) -> Candidates<'static> {
    Candidates {
        override_value: None,
        exact_tag,
        branch,
        commit: Some(COMMIT),
    }
}

#[test]
fn override_wins_over_local_git() {
    let candidates = Candidates {
        override_value: Some("v1.2.3"),
        ..local(Some("v9.9.9"), Some("main"))
    };
    assert_eq!(
        resolve_git_version(&candidates).as_deref(),
        Some("v1.2.3"),
        "should prefer the pipeline-supplied TRUSTED_SERVER_GIT_VERSION"
    );
}

#[test]
fn override_is_trimmed() {
    let candidates = Candidates {
        override_value: Some("  feature/x\n"),
        ..local(None, None)
    };
    assert_eq!(
        resolve_git_version(&candidates).as_deref(),
        Some("feature/x"),
        "should trim surrounding whitespace from the override"
    );
}

#[test]
fn tag_wins_over_branch() {
    assert_eq!(
        resolve_git_version(&local(Some("v1.2.3"), Some("main"))).as_deref(),
        Some("v1.2.3"),
        "should prefer an exact tag over the branch"
    );
}

#[test]
fn branch_when_no_tag() {
    assert_eq!(
        resolve_git_version(&local(None, Some("feature/x"))).as_deref(),
        Some("feature/x"),
        "should use the branch when HEAD is not tagged"
    );
}

#[test]
fn six_char_hash_when_no_tag_or_branch() {
    assert_eq!(
        resolve_git_version(&local(None, None)).as_deref(),
        Some("abcdef"),
        "should use exactly the first 6 characters of the commit"
    );
}

#[test]
fn blank_candidates_are_skipped() {
    let candidates = Candidates {
        override_value: Some("  "),
        exact_tag: Some(""),
        branch: Some(" "),
        commit: Some(COMMIT),
    };
    assert_eq!(
        resolve_git_version(&candidates).as_deref(),
        Some("abcdef"),
        "should skip empty or whitespace-only candidates"
    );
}

#[test]
fn non_ascii_override_falls_back_to_local_git() {
    let candidates = Candidates {
        override_value: Some("v1-été"),
        ..local(None, None)
    };
    assert_eq!(
        resolve_git_version(&candidates).as_deref(),
        Some("abcdef"),
        "should skip a non-ASCII override and fall back to local git"
    );
}

#[test]
fn override_with_inner_space_falls_back_to_local_git() {
    let candidates = Candidates {
        override_value: Some("v1 2"),
        ..local(None, Some("main"))
    };
    assert_eq!(
        resolve_git_version(&candidates).as_deref(),
        Some("main"),
        "should skip an override containing a space"
    );
}

#[test]
fn none_when_nothing_is_known() {
    let candidates = Candidates {
        override_value: None,
        exact_tag: None,
        branch: None,
        commit: None,
    };
    assert_eq!(
        resolve_git_version(&candidates),
        None,
        "should leave the version unset so the header is omitted"
    );
}

#[test]
fn is_usable_accepts_only_visible_ascii() {
    assert!(git_version::is_usable("v1.2.3"), "should accept a tag");
    assert!(
        git_version::is_usable(" main "),
        "should accept after trimming"
    );
    assert!(!git_version::is_usable(""), "should reject empty");
    assert!(
        !git_version::is_usable("a\tb"),
        "should reject a control character"
    );
    assert!(!git_version::is_usable("v1-été"), "should reject non-ASCII");
}
