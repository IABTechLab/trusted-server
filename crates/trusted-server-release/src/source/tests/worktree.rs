//! Worktree comparison and mutation checks.

use super::*;

#[test]
fn rejects_modified_tracked_file() {
    let fixture = Fixture::new();
    write(&fixture.root, "AGENTS.md", b"# Modified\n");
    assert_rejects(&fixture, "AGENTS.md");
}

#[test]
fn rejects_changed_executable_bit() {
    let fixture = Fixture::new();
    fs::set_permissions(
        fixture.root.join("scripts/release/bootstrap.sh"),
        fs::Permissions::from_mode(0o644),
    )
    .expect("should chmod");
    assert_rejects(&fixture, "scripts/release/bootstrap.sh");
}

#[test]
fn rejects_file_replaced_by_symlink() {
    let fixture = Fixture::new();
    let outside = fixture.parent.path().join("outside.md");
    fs::write(&outside, b"# Fictional agents\n").expect("should write outside file");
    fs::remove_file(fixture.root.join("AGENTS.md")).expect("should remove tracked file");
    symlink(&outside, fixture.root.join("AGENTS.md")).expect("should create symlink");
    assert_rejects(&fixture, "AGENTS.md");
}

#[test]
fn rejects_preexisting_hard_link() {
    let fixture = Fixture::new();
    fs::hard_link(
        fixture.root.join("Cargo.lock"),
        fixture.parent.path().join("lock-link"),
    )
    .expect("should create hard link outside source");
    assert_rejects(&fixture, "Cargo.lock");
}

#[test]
fn rejects_missing_tracked_file() {
    let fixture = Fixture::new();
    fs::remove_file(fixture.root.join("crates/other/src/lib.rs")).expect("should remove file");
    assert_rejects(&fixture, "crates/other/src/lib.rs");
}

#[test]
fn rejects_untracked_and_ignored_paths() {
    for (path, needle) in [
        (".DS_Store", ".DS_Store"),
        (
            "crates/trusted-server-js/lib/node_modules/x/index.js",
            "crates/trusted-server-js",
        ),
        (
            "crates/other/src/extra_test.rs",
            "crates/other/src/extra_test.rs",
        ),
        ("target/debug/fictional", "target"),
    ] {
        let mut fixture = Fixture::new();
        write(
            &fixture.root,
            ".gitignore",
            b".DS_Store\nnode_modules\ntarget\n*_test.rs\n",
        );
        fixture.commit_all();
        write(&fixture.root, path, b"untracked\n");
        assert_rejects(&fixture, needle);
    }
}

#[test]
fn rejects_empty_untracked_directory() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.root.join("empty")).expect("should create directory");
    assert_rejects(&fixture, "empty");
}

#[test]
fn rejects_unreviewed_tracked_symlink() {
    let mut fixture = Fixture::new();
    symlink("Cargo.toml", fixture.root.join("manifest-link")).expect("should create symlink");
    fixture.commit_all();
    assert_rejects(&fixture, "manifest-link");
}

#[test]
fn rejects_tracked_submodule() {
    let mut fixture = Fixture::new();
    let gitlink = format!("160000,{},vendor/module", fixture.commit);
    fixture.git(&["update-index", "--add", "--cacheinfo", &gitlink]);
    fixture.git(&["commit", "-q", "-m", "submodule"]);
    fixture.commit = fixture.git(&["rev-parse", "HEAD"]);
    assert_rejects(&fixture, "vendor/module");
}

type Mutation = fn(&Fixture);

#[test]
fn verify_unchanged_detects_mutations_after_snapshot() {
    let mutations: [(&str, Mutation); 5] = [
        ("content", |fixture| {
            write(&fixture.root, "AGENTS.md", b"# Changed\n")
        }),
        ("mode", |fixture| {
            fs::set_permissions(
                fixture.root.join("Cargo.lock"),
                fs::Permissions::from_mode(0o755),
            )
            .expect("should chmod");
        }),
        ("symlink", |fixture| {
            fs::remove_file(fixture.root.join("Cargo.lock")).expect("should remove");
            symlink("Cargo.toml", fixture.root.join("Cargo.lock")).expect("should link");
        }),
        ("hard link", |fixture| {
            fs::hard_link(
                fixture.root.join("Cargo.lock"),
                fixture.parent.path().join("late-link"),
            )
            .expect("should hard link");
        }),
        ("new file", |fixture| {
            write(&fixture.root, "late.txt", b"late\n")
        }),
    ];
    for (label, mutate) in mutations {
        let fixture = Fixture::new();
        let source = fixture.authenticate().expect("should authenticate");
        mutate(&fixture);
        assert!(
            source.snapshot().verify_unchanged().is_err(),
            "should detect {label} mutation"
        );
    }
}
