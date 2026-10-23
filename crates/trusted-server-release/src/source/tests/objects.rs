//! Commit, tree and blob identity and parsing checks.

use super::*;

#[test]
fn authenticates_clean_fixture_with_commit_epoch_and_closure() {
    let fixture = Fixture::new();
    let source = fixture
        .authenticate()
        .expect("should authenticate clean fixture");
    assert_eq!(
        source.commit().as_str(),
        fixture.commit,
        "should keep approved commit"
    );
    assert_eq!(source.epoch(), EPOCH, "should derive committer epoch");
    assert!(
        lower_hex(source.snapshot().fingerprint(), 64),
        "should fingerprint the tracked snapshot"
    );
    assert!(
        lower_hex(source.closure_digest(), 64),
        "should digest closure"
    );
    assert_eq!(
        source.closure_paths(),
        [
            ".cargo/config.toml",
            ".tool-versions",
            "Cargo.lock",
            "Cargo.toml",
            "crates/trusted-server-openrtb/Cargo.toml",
            "crates/trusted-server-release/Cargo.toml",
            "crates/trusted-server-release/src/main.rs",
            "rust-toolchain.toml",
            "scripts/release/bootstrap.sh",
            "scripts/release/build-js.sh",
            "scripts/release/build.sh",
            "scripts/release/collect-js.mjs",
            "scripts/release/observe-npm.mjs",
        ],
        "should record the complete helper-source closure"
    );
    assert_eq!(
        source.tracked_sha256("AGENTS.md"),
        Some(hex::encode(Sha256::digest(b"# Fictional agents\n")).as_str()),
        "should expose tracked content digests"
    );
    source
        .snapshot()
        .verify_unchanged()
        .expect("should verify unchanged worktree");
}

#[test]
fn rejects_malformed_commit_arguments() {
    let fixture = Fixture::new();
    for commit in [
        "ABC",
        "--help",
        &fixture.commit.to_uppercase(),
        &fixture.commit[..39],
    ] {
        assert!(
            approve(&fixture.root, commit, &fixture.audited()).is_err(),
            "should reject commit argument {commit}"
        );
    }
}

#[test]
fn rejects_unknown_commit_with_bounded_git_detail() {
    let fixture = Fixture::new();
    let text = format!(
        "{:?}",
        approve(&fixture.root, &"0".repeat(40), &fixture.audited())
            .err()
            .expect("should reject unknown commit")
    );
    assert!(
        text.contains("git cat-file"),
        "should name the failing git step: {text}"
    );
}

#[test]
fn rejects_non_commit_objects() {
    let fixture = Fixture::new();
    let tree = fixture.git(&["rev-parse", "HEAD^{tree}"]);
    let text = format!(
        "{:?}",
        approve(&fixture.root, &tree, &fixture.audited())
            .err()
            .expect("should reject tree object")
    );
    assert!(text.contains("not a commit"), "should reject tree: {text}");
}

#[test]
fn rejects_head_that_differs_from_approved_commit() {
    let mut fixture = Fixture::new();
    let approved = fixture.commit.clone();
    write(&fixture.root, "AGENTS.md", b"# Changed\n");
    fixture.commit_all();
    let text = format!(
        "{:?}",
        approve(&fixture.root, &approved, &fixture.audited())
            .err()
            .expect("should reject HEAD mismatch")
    );
    assert!(
        text.contains("HEAD"),
        "should explain HEAD mismatch: {text}"
    );
}

#[test]
fn parses_committer_epoch_strictly() {
    let header =
        "tree 0000000000000000000000000000000000000000\nauthor A <a@example.com> 1 +0000\n";
    let valid = format!("{header}committer C <c@example.com> 1700000000 -0130\n\nmessage\n");
    assert_eq!(
        committer_epoch(valid.as_bytes()).expect("should parse committer epoch"),
        EPOCH
    );
    for committer in [
        "",
        "committer C <c@example.com> +1700000000 +0000\n",
        "committer C <c@example.com> 17e8 +0000\n",
        "committer C <c@example.com> 1700000000 0000\n",
        "committer C <c@example.com> 1700000000 +0060\n",
        "committer C <c@example.com> 1700000000 +01000\n",
        "committer C <c@example.com> 1700000000\n",
        "committer C <c@example.com> 1 +0000\ncommitter D <d@example.com> 2 +0000\n",
    ] {
        let raw = format!("{header}{committer}\nmessage\n");
        assert!(
            committer_epoch(raw.as_bytes()).is_err(),
            "should reject committer line {committer:?}"
        );
    }
}

fn object_path(fixture: &Fixture, oid: &str) -> PathBuf {
    fixture
        .root
        .join(".git/objects")
        .join(&oid[..2])
        .join(&oid[2..])
}

/// Replaces the loose object file for `target` with the bytes stored for `source`.
fn substitute_object(fixture: &Fixture, target: &str, source: &str) {
    let destination = object_path(fixture, target);
    assert!(destination.exists(), "should have a loose target object");
    fs::remove_file(&destination).expect("should remove read-only loose object");
    fs::copy(object_path(fixture, source), &destination).expect("should copy loose object");
}

#[test]
fn rejects_substituted_blob_object() {
    let fixture = Fixture::new();
    let target = fixture.git(&["rev-parse", "HEAD:AGENTS.md"]);
    let source = fixture.git(&["rev-parse", "HEAD:Cargo.lock"]);
    substitute_object(&fixture, &target, &source);
    assert_rejects(&fixture, "object identity");
}

#[test]
fn rejects_substituted_tree_object() {
    let fixture = Fixture::new();
    let target = fixture.git(&["rev-parse", "HEAD^{tree}"]);
    let source = fixture.git(&["rev-parse", "HEAD:crates"]);
    substitute_object(&fixture, &target, &source);
    assert_rejects(&fixture, "object identity");
}

#[test]
fn rejects_substituted_nested_tree_object() {
    let fixture = Fixture::new();
    let target = fixture.git(&["rev-parse", "HEAD:crates/other/src"]);
    let source = fixture.git(&["rev-parse", "HEAD:crates/trusted-server-release/src"]);
    substitute_object(&fixture, &target, &source);
    assert_rejects(&fixture, "object identity");
}

#[test]
fn rejects_substituted_commit_object() {
    let fixture = Fixture::new();
    let tree = fixture.git(&["rev-parse", "HEAD^{tree}"]);
    let other = fixture.git(&["commit-tree", &tree, "-p", &fixture.commit, "-m", "other"]);
    substitute_object(&fixture, &fixture.commit, &other);
    assert_rejects(&fixture, "object identity");
}

fn tree_entry(mode: &str, name: &[u8]) -> Vec<u8> {
    let mut bytes = format!("{mode} ").into_bytes();
    bytes.extend_from_slice(name);
    bytes.push(0);
    bytes.extend_from_slice(&[0xab; 20]);
    bytes
}

type ParsedTree = (Vec<TreeEntry>, Vec<(String, String)>);

fn parse_tree_bytes(bytes: &[u8]) -> Result<ParsedTree, String> {
    let mut entries = Vec::new();
    let mut subtrees = Vec::new();
    parse_tree_object("", bytes, &mut entries, &mut subtrees)
        .map(|()| (entries, subtrees))
        .map_err(|report| format!("{report:?}"))
}

#[test]
fn parses_canonical_tree_objects() {
    let bytes = [
        tree_entry("100644", b"a"),
        tree_entry("40000", b"a-dir"),
        tree_entry("100755", b"b"),
        tree_entry("100644", b"c.txt"),
        tree_entry("40000", b"c"),
    ]
    .concat();
    let (entries, subtrees) = parse_tree_bytes(&bytes).expect("should parse canonical tree");
    assert_eq!(entries.len(), 3, "should collect blob entries");
    assert_eq!(subtrees.len(), 2, "should collect subtrees");
    assert_eq!(subtrees[0].1, "ab".repeat(20), "should hex-encode raw ids");
}

#[test]
fn orders_tree_names_as_if_suffixed_by_slash() {
    let canonical = [tree_entry("100644", b"c.txt"), tree_entry("40000", b"c")].concat();
    assert!(
        parse_tree_bytes(&canonical).is_ok(),
        "should accept `c.txt` before tree `c` because `c/` sorts after `c.txt`"
    );
    let swapped = [tree_entry("40000", b"c"), tree_entry("100644", b"c.txt")].concat();
    assert!(
        parse_tree_bytes(&swapped).is_err(),
        "should reject tree `c` before `c.txt`"
    );
}

#[test]
fn rejects_malformed_tree_objects() {
    let mut missing_nul = b"100644 name".to_vec();
    missing_nul.extend_from_slice(&[0xab; 20]);
    let mut truncated = b"100644 name\0".to_vec();
    truncated.extend_from_slice(&[0xab; 19]);
    let cases: [(&str, Vec<u8>); 15] = [
        ("missing NUL", missing_nul),
        ("truncated id", truncated),
        ("missing space", b"100644name\0".to_vec()),
        ("zero-padded tree mode", tree_entry("040000", b"dir")),
        ("legacy group-writable mode", tree_entry("100664", b"file")),
        ("unknown mode", tree_entry("100600", b"file")),
        ("slash in name", tree_entry("100644", b"a/b")),
        ("empty name", tree_entry("100644", b"")),
        ("dot name", tree_entry("40000", b".")),
        ("dot-dot name", tree_entry("40000", b"..")),
        ("git name", tree_entry("40000", b".git")),
        ("uppercase git name", tree_entry("40000", b".GIT")),
        (
            "duplicate name",
            [tree_entry("100644", b"a"), tree_entry("100644", b"a")].concat(),
        ),
        (
            "file and tree with one name",
            [tree_entry("100644", b"a"), tree_entry("40000", b"a")].concat(),
        ),
        (
            "out of order",
            [tree_entry("100644", b"b"), tree_entry("100644", b"a")].concat(),
        ),
    ];
    for (label, bytes) in cases {
        assert!(parse_tree_bytes(&bytes).is_err(), "should reject {label}");
    }
}

fn walk_with(limits: &TreeLimits) -> Result<Vec<TreeEntry>, Report<ReleaseError>> {
    let fixture = Fixture::new();
    let git = Git::locate(&fixture.root, &fixture.audited()).expect("should locate git");
    let tree = fixture.git(&["rev-parse", "HEAD^{tree}"]);
    walk_tree(&git, &tree, limits)
}

#[test]
fn enforces_tree_depth_limit_exactly() {
    let exact = TreeLimits {
        depth: 4,
        entries: 1_000,
        objects: 1_000,
    };
    assert!(
        walk_with(&exact).is_ok(),
        "should accept a tree exactly four levels deep"
    );
    let shallow = TreeLimits { depth: 3, ..exact };
    let text = format!(
        "{:?}",
        walk_with(&shallow).expect_err("should reject depth")
    );
    assert!(
        text.contains("depth"),
        "should name the depth limit: {text}"
    );
}

#[test]
fn enforces_tree_entry_and_object_limits() {
    let entries = TreeLimits {
        depth: 64,
        entries: 5,
        objects: 1_000,
    };
    let text = format!(
        "{:?}",
        walk_with(&entries).expect_err("should reject entry count")
    );
    assert!(
        text.contains("entry limit"),
        "should name the entry limit: {text}"
    );
    let objects = TreeLimits {
        depth: 64,
        entries: 1_000,
        objects: 3,
    };
    let text = format!(
        "{:?}",
        walk_with(&objects).expect_err("should reject tree object count")
    );
    assert!(
        text.contains("object limit"),
        "should name the object limit: {text}"
    );
}

#[test]
fn rejects_detected_sha1_collision_input() {
    let vector = include_bytes!("../../../tests/fixtures/sha-mbles-1.bin");
    let text = format!(
        "{:?}",
        checked_sha1(&[vector.as_slice()])
            .expect_err("should detect the SHA-mbles chosen-prefix collision")
    );
    assert!(
        text.contains("collision"),
        "should report a collision: {text}"
    );
    assert_eq!(
        checked_sha1(&[b"blob 0\0".as_slice()]).expect("should hash ordinary input"),
        "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391",
        "should compute Git's empty-blob id"
    );
}

#[test]
fn rejects_non_ascii_tracked_names() {
    let bytes = tree_entry("100644", "caf\u{e9}.txt".as_bytes());
    let text = parse_tree_bytes(&bytes).expect_err("should reject non-ASCII names");
    assert!(
        text.contains("ASCII"),
        "should explain the ASCII requirement: {text}"
    );
}

#[test]
fn enforces_aggregate_object_byte_budget() {
    let fixture = Fixture::new();
    let git = Git::locate(&fixture.root, &fixture.audited())
        .expect("should locate git")
        .with_object_budget(64);
    let tree = fixture.git(&["rev-parse", "HEAD^{tree}"]);
    let text = format!(
        "{:?}",
        walk_tree(&git, &tree, &TREE_LIMITS).expect_err("should exhaust the byte budget")
    );
    assert!(
        text.contains("aggregate"),
        "should name the aggregate bound: {text}"
    );
}
