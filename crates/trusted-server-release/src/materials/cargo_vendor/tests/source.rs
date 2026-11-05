use crate::materials::cargo_vendor::GitSourceConfig;
use crate::materials::locked::parse_cargo;

/// Selector, derived URL and derived reference of one locked Git source.
type Case = (
    &'static str,
    &'static str,
    Option<(&'static str, &'static str)>,
);

#[test]
fn locked_selectors_map_to_exact_cargo_source_tables() {
    let cases: [Case; 4] = [
        (
            "git+https://example.com/org/repo?tag=v1.2.3",
            "https://example.com/org/repo",
            Some(("tag", "v1.2.3")),
        ),
        (
            "git+https://example.com/org/repo?branch=release-1",
            "https://example.com/org/repo",
            Some(("branch", "release-1")),
        ),
        (
            "git+https://example.com/org/repo?rev=abc123",
            "https://example.com/org/repo",
            Some(("rev", "abc123")),
        ),
        (
            "git+https://example.com:8443/org/repo.git",
            "https://example.com:8443/org/repo.git",
            None,
        ),
    ];
    for (selector, url, reference) in cases {
        let config = GitSourceConfig::parse(selector).expect("should parse the selector");
        assert_eq!(
            config.key(),
            selector,
            "should keep the exact selector as the key"
        );
        assert_eq!(config.url(), url, "should derive the repository URL");
        assert_eq!(config.reference(), reference, "should derive the reference");
    }
    assert_eq!(
        GitSourceConfig::parse("git+https://example.com/org/repo")
            .expect("should parse")
            .reference(),
        None,
        "should report the default branch without a reference"
    );
}

#[test]
fn unusual_selectors_are_rejected() {
    for selector in [
        "",
        "https://example.com/org/repo?tag=v1",
        "git+http://example.com/org/repo?tag=v1",
        "git+ssh://example.com/org/repo",
        "git+file:///local/repo",
        "git+https://",
        "git+https://example.com/org/repo?tag=",
        "git+https://example.com/org/repo?tag=a&branch=b",
        "git+https://example.com/org/repo?tag=a%2Fb",
        "git+https://example.com/org/repo?tag=a/b",
        "git+https://example.com/org/repo?unknown=a",
        "git+https://example.com/org/repo?tag",
        "git+https://example.com/org/\"repo",
        "git+https://example.com/org\\repo",
        "git+https://example.com/org repo",
        "git+https://example.com/org/repo#abc",
        "git+https://example.com/org/repo\n",
        "git+https://example.com/org/%2Frepo",
    ] {
        assert!(
            GitSourceConfig::parse(selector).is_err(),
            "should reject selector {selector:?}"
        );
    }
}

#[test]
fn tracked_lockfile_git_packages_all_parse_as_sources() {
    let lock = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../Cargo.lock"))
        .expect("should read the tracked Cargo.lock");
    let materials = parse_cargo(&lock).expect("should parse the tracked lockfile");
    assert!(
        !materials.git_packages.is_empty(),
        "should select a nonzero number of locked Git packages"
    );
    for package in &materials.git_packages {
        let config = GitSourceConfig::parse(&package.selector)
            .expect("should parse every tracked Git source");
        assert!(
            config.reference().is_some(),
            "should pin {} with a tag, branch or rev",
            package.name
        );
        assert_eq!(package.revision.len(), 40, "should carry a full revision");
    }
}
