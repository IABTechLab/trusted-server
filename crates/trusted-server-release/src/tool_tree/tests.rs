use std::fs;
use std::os::unix::fs::symlink;
use std::path::PathBuf;

use super::*;

/// A fictional tool tree below a canonical temporary root.
struct Tree {
    _directory: tempfile::TempDir,
    root: PathBuf,
}

impl Tree {
    fn new() -> Self {
        let directory = tempfile::tempdir().expect("should create tree directory");
        let outer = fs::canonicalize(directory.path()).expect("should canonicalize");
        let root = outer.join("tools");
        fs::create_dir_all(root.join("bin")).expect("should create bin");
        fs::write(root.join("bin/tool"), b"fictional tool").expect("should write tool");
        fs::create_dir(outer.join("outside")).expect("should create outside directory");
        fs::write(outer.join("outside/secret"), b"outside").expect("should write outside file");
        Self {
            _directory: directory,
            root,
        }
    }

    fn digest(&self) -> Result<String, Report<ReleaseError>> {
        tool_tree_digest(&self.root, &|| Ok(()))
    }

    fn rejection(&self) -> String {
        match self.digest() {
            Ok(_) => panic!("should reject the tree"),
            Err(error) => format!("{error:?}"),
        }
    }
}

#[test]
fn identical_trees_have_identical_digests_and_edits_change_them() {
    let tree = Tree::new();
    let baseline = tree.digest().expect("should digest the tree");
    assert_eq!(
        baseline,
        tree.digest().expect("should digest again"),
        "should be deterministic over an unchanged tree"
    );
    assert_eq!(baseline.len(), 64, "should be a SHA-256 hex digest");

    fs::write(tree.root.join("bin/tool"), b"fictional tool!").expect("should edit content");
    let edited = tree.digest().expect("should digest the edited tree");
    assert_ne!(baseline, edited, "should detect a content edit");

    fs::write(tree.root.join("bin/tool"), b"fictional tool").expect("should restore content");
    assert_eq!(
        baseline,
        tree.digest().expect("should digest the restored tree"),
        "should return to the baseline when content is restored"
    );

    fs::set_permissions(
        tree.root.join("bin/tool"),
        fs::Permissions::from_mode(0o755),
    )
    .expect("should change mode");
    assert_ne!(
        baseline,
        tree.digest().expect("should digest the chmod tree"),
        "should detect a permission change"
    );
}

#[test]
fn added_and_renamed_entries_change_the_digest() {
    let tree = Tree::new();
    let baseline = tree.digest().expect("should digest the tree");
    fs::rename(tree.root.join("bin/tool"), tree.root.join("bin/renamed"))
        .expect("should rename the tool");
    assert_ne!(
        baseline,
        tree.digest().expect("should digest the renamed tree"),
        "should detect a rename"
    );
    fs::rename(tree.root.join("bin/renamed"), tree.root.join("bin/tool"))
        .expect("should restore the name");
    fs::write(tree.root.join("extra"), b"").expect("should add an entry");
    assert_ne!(
        baseline,
        tree.digest().expect("should digest the extended tree"),
        "should detect an added empty file"
    );
}

#[test]
fn hard_linked_files_are_rejected() {
    let tree = Tree::new();
    fs::hard_link(tree.root.join("bin/tool"), tree.root.join("bin/alias"))
        .expect("should link inside the tree");
    assert!(
        tree.rejection().contains("hardlink"),
        "should reject a second link inside the tree"
    );
    let tree = Tree::new();
    let outside = tree
        .root
        .parent()
        .expect("should have parent")
        .join("outside/secret");
    fs::hard_link(&outside, tree.root.join("bin/leak")).expect("should link from outside");
    assert!(
        tree.rejection().contains("hardlink"),
        "should reject a link shared with a path outside the tree"
    );
}

#[test]
fn escaping_dangling_or_absolute_aliases_are_rejected() {
    let cases: [(&str, &str); 3] = [
        ("relative escape", "../../outside/secret"),
        ("dangling", "missing"),
        ("absolute", "/usr/bin/true"),
    ];
    for (label, target) in cases {
        let tree = Tree::new();
        symlink(target, tree.root.join("bin/alias")).expect("should create alias");
        let rejection = tree.rejection();
        assert!(
            rejection.contains("alias") || rejection.contains("No such file"),
            "should reject the {label} alias: {rejection}"
        );
    }
}

#[test]
fn inside_relative_alias_is_accepted_and_recorded() {
    let tree = Tree::new();
    let baseline = tree.digest().expect("should digest the tree");
    symlink("tool", tree.root.join("bin/alias")).expect("should create inside alias");
    let with_alias = tree
        .digest()
        .expect("should accept an inside relative alias");
    assert_ne!(
        baseline, with_alias,
        "should record the alias in the digest"
    );
    fs::remove_file(tree.root.join("bin/alias")).expect("should remove alias");
    symlink("./tool", tree.root.join("bin/alias")).expect("should create alias with other text");
    assert_ne!(
        with_alias,
        tree.digest().expect("should digest the other alias"),
        "should bind the alias target text"
    );
}

#[test]
fn failing_check_stops_the_digest() {
    let tree = Tree::new();
    let error = tool_tree_digest(&tree.root, &|| Err(tree_error("cancelled")))
        .expect_err("should stop when the owning attempt fails");
    assert!(
        format!("{error:?}").contains("cancelled"),
        "should surface the check failure"
    );
}
