//! The version-resolution rule `build.rs` uses for `TRUSTED_SERVER__GIT_VERSION`.

#[path = "../build_support/git_version.rs"]
mod git_version;

use git_version::{Candidates, resolve_git_version};
use std::ffi::OsStr;
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt as _;

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
        "should prefer the pipeline-supplied TRUSTED_SERVER__GIT_VERSION"
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

#[test]
fn release_source_requires_an_exact_full_lowercase_commit() {
    let commit = "abcdef0123456789abcdef0123456789abcdef01";
    assert_eq!(
        git_version::release_source(Some(OsStr::new("1")), Some(OsStr::new(commit))),
        Ok(Some(commit.to_owned())),
        "should preserve the complete release source"
    );
    for source in [
        None,
        Some(""),
        Some("main"),
        Some("v1.2.3"),
        Some("abcdef"),
        Some("ABCDEF0123456789abcdef0123456789abcdef01"),
        Some(" abcdef0123456789abcdef0123456789abcdef01"),
        Some("abcdef0123456789abcdef0123456789abcdef01\n"),
        Some("abcdef0123456789abcdef0123456789abcdef0g"),
    ] {
        assert!(
            git_version::release_source(Some(OsStr::new("1")), source.map(OsStr::new)).is_err(),
            "should reject malformed release source {source:?}"
        );
    }
}

#[test]
fn release_control_accepts_only_absence_or_exact_one() {
    assert_eq!(
        git_version::release_source(None, Some(OsStr::new("main"))),
        Ok(None),
        "should retain development resolution when release control is absent"
    );
    for control in ["", "0", "true", " 1", "1\n"] {
        assert!(
            git_version::release_source(
                Some(OsStr::new(control)),
                Some(OsStr::new("abcdef0123456789abcdef0123456789abcdef01"))
            )
            .is_err(),
            "should reject invalid control even with valid source"
        );
    }
}

#[cfg(unix)]
#[test]
fn non_unicode_release_inputs_fail_closed() {
    let non_unicode = OsStr::from_bytes(b"\xff");
    assert!(
        git_version::release_source(Some(non_unicode), None).is_err(),
        "should recognize non-Unicode control as present and invalid"
    );
    assert!(
        git_version::release_source(Some(OsStr::new("1")), Some(non_unicode)).is_err(),
        "should reject non-Unicode source"
    );
}

#[test]
fn actual_build_source_resolution_never_calls_git_in_release_mode() {
    let source =
        git_version::resolve_build_source(Some(OsStr::new("1")), Some(OsStr::new(COMMIT)), || {
            panic!("should not call Git in release mode")
        })
        .expect("should resolve valid release source");
    assert_eq!(
        source.as_deref(),
        Some(COMMIT),
        "should retain the complete release commit"
    );
    let result =
        git_version::resolve_build_source(Some(OsStr::new("1")), Some(OsStr::new("main")), || {
            panic!("should not fall back after invalid release source")
        });
    assert!(
        result.is_err(),
        "should reject malformed release source without Git fallback"
    );
}

#[test]
fn actual_build_source_resolution_preserves_development_override_and_fallback() {
    for identity in ["main", " v1.2.3 ", COMMIT] {
        let source = git_version::resolve_build_source(None, Some(OsStr::new(identity)), || {
            panic!("should not call Git with a usable development override")
        })
        .expect("should resolve development override");
        assert_eq!(
            source.as_deref(),
            Some(identity.trim()),
            "should preserve development override"
        );
    }
    for identity in [None, Some(OsStr::new("")), Some(OsStr::new("bad identity"))] {
        let mut called = false;
        let source = git_version::resolve_build_source(None, identity, || {
            called = true;
            Some("local-branch".to_owned())
        })
        .expect("should resolve development fallback");
        assert!(called, "should call actual fallback for unusable override");
        assert_eq!(
            source.as_deref(),
            Some("local-branch"),
            "should use fallback result"
        );
    }
}
