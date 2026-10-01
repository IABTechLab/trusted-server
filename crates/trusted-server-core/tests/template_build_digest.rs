//! Host-only tests for the build script's filesystem inputs.

#![cfg(not(target_arch = "wasm32"))]

use std::fs;
#[cfg(unix)]
use std::os::unix::fs::symlink;
use std::path::PathBuf;

use tempfile::TempDir;

#[allow(dead_code)]
#[path = "../build.rs"]
mod build_script;

struct SourceTree {
    root: TempDir,
    core: PathBuf,
}

impl SourceTree {
    fn new(reverse: bool) -> Self {
        let root = tempfile::tempdir().expect("should create temporary workspace");
        let core = root.path().join("crates/trusted-server-core");
        fs::create_dir_all(core.join("src/integrations"))
            .expect("should create source directories");
        let mut files = vec![
            ("Cargo.toml", "workspace manifest"),
            ("Cargo.lock", "locked dependencies"),
            ("edgezero.toml", "app manifest"),
            ("crates/trusted-server-core/Cargo.toml", "core manifest"),
            ("crates/trusted-server-core/build.rs", "build script"),
            ("crates/trusted-server-core/src/lib.rs", "mod integrations;"),
            (
                "crates/trusted-server-core/src/integrations/gpt.rs",
                "inline head program",
            ),
            (
                "crates/trusted-server-core/src/integrations/gpt_bootstrap.js",
                "window.example = true;",
            ),
        ];
        if reverse {
            files.reverse();
        }
        for (path, content) in files {
            fs::write(root.path().join(path), content).expect("should write fixture");
        }
        Self { root, core }
    }

    fn digest(&self) -> String {
        build_script::template_build_digest(&self.core)
    }
}

#[test]
fn identical_sources_ignore_checkout_path_and_creation_order() {
    let first = SourceTree::new(false);
    let second = SourceTree::new(true);

    assert_eq!(first.digest(), first.digest(), "should be repeatable");
    assert_eq!(
        first.digest(),
        second.digest(),
        "should depend on logical paths and bytes, not checkout location or traversal order"
    );
}

#[test]
fn editor_and_os_dotfiles_do_not_change_the_digest() {
    let tree = SourceTree::new(false);
    let original = tree.digest();
    fs::write(tree.core.join("src/.DS_Store"), b"finder").expect("should write OS metadata");
    fs::write(tree.core.join("src/integrations/.gpt.rs.swp"), b"swap")
        .expect("should write editor swap file");
    fs::create_dir(tree.core.join("src/.editor")).expect("should create hidden editor directory");
    fs::write(tree.core.join("src/.editor/state"), b"state")
        .expect("should write hidden editor state");

    assert_eq!(
        tree.digest(),
        original,
        "should ignore dot-prefixed editor and OS artifacts"
    );
}

#[cfg(unix)]
#[test]
fn dangling_editor_lock_symlink_does_not_change_the_digest() {
    let tree = SourceTree::new(false);
    let original = tree.digest();
    // Emacs lock files are dangling symlinks while a buffer has unsaved edits.
    symlink(
        "user@example.com.1234:1700000000",
        tree.core.join("src/integrations/.#gpt.rs"),
    )
    .expect("should create an editor lock symlink");

    assert_eq!(
        tree.digest(),
        original,
        "should ignore dangling dot-prefixed editor lock symlinks"
    );
}

#[test]
fn editor_auto_save_and_backup_files_do_not_change_the_digest() {
    let tree = SourceTree::new(false);
    let original = tree.digest();
    for name in ["#gpt.rs#", "gpt.rs~"] {
        let path = tree.core.join("src/integrations").join(name);
        fs::write(&path, b"unsaved editor content").expect("should write editor artifact");

        assert_eq!(
            tree.digest(),
            original,
            "should ignore editor artifact {name}"
        );

        fs::remove_file(path).expect("should remove editor artifact");
    }
}

#[test]
fn every_source_and_build_input_changes_the_digest() {
    let tree = SourceTree::new(false);
    let original = tree.digest();
    for path in [
        "Cargo.toml",
        "Cargo.lock",
        "edgezero.toml",
        "crates/trusted-server-core/Cargo.toml",
        "crates/trusted-server-core/build.rs",
        "crates/trusted-server-core/src/lib.rs",
        "crates/trusted-server-core/src/integrations/gpt.rs",
        "crates/trusted-server-core/src/integrations/gpt_bootstrap.js",
    ] {
        let path = tree.root.path().join(path);
        let bytes = fs::read(&path).expect("should read fixture");
        fs::write(&path, b"changed input").expect("should modify fixture");
        assert_ne!(tree.digest(), original, "should cover {}", path.display());
        fs::write(&path, bytes).expect("should restore fixture");
    }
    assert_eq!(tree.digest(), original, "should reproduce restored inputs");
}

#[test]
fn adding_renaming_and_removing_sources_changes_the_digest() {
    let tree = SourceTree::new(false);
    let original = tree.digest();
    let path = tree.core.join("src/integrations/new_asset.js");
    fs::write(&path, b"").expect("should add even an empty source file");
    let added = tree.digest();
    assert_ne!(added, original, "should include new files");

    let renamed = path.with_file_name("renamed_asset.js");
    fs::rename(&path, &renamed).expect("should rename source");
    assert_ne!(tree.digest(), added, "should include relative paths");

    fs::remove_file(renamed).expect("should remove source");
    assert_eq!(
        tree.digest(),
        original,
        "should reproduce the original tree"
    );
    fs::remove_file(tree.core.join("src/integrations/gpt_bootstrap.js"))
        .expect("should remove existing source");
    assert_ne!(tree.digest(), original, "should include source deletions");
}

#[test]
fn missing_lockfile_is_supported_and_distinct_from_an_empty_lockfile() {
    let tree = SourceTree::new(false);
    let original = tree.digest();
    let lockfile = tree.root.path().join("Cargo.lock");
    fs::remove_file(&lockfile).expect("should remove optional lockfile");
    let missing = tree.digest();
    assert_ne!(missing, original, "should reflect lockfile removal");
    fs::write(lockfile, b"").expect("should create empty lockfile");
    assert_ne!(
        tree.digest(),
        missing,
        "should distinguish absent and empty inputs"
    );
}

#[test]
fn path_and_content_boundaries_are_unambiguous() {
    let first = SourceTree::new(false);
    let second = SourceTree::new(false);
    fs::write(first.core.join("src/a"), b"bc").expect("should write first source");
    fs::write(second.core.join("src/ab"), b"c").expect("should write second source");

    assert_ne!(
        first.digest(),
        second.digest(),
        "should frame paths and contents separately"
    );
}

#[test]
#[should_panic(expected = "should read template build input")]
fn missing_required_manifest_fails_the_build() {
    let tree = SourceTree::new(false);
    fs::remove_file(tree.core.join("Cargo.toml")).expect("should remove required manifest");
    tree.digest();
}

#[test]
#[should_panic(expected = "should read template build input")]
fn missing_required_app_manifest_fails_the_build() {
    let tree = SourceTree::new(false);
    fs::remove_file(tree.root.path().join("edgezero.toml"))
        .expect("should remove required app manifest");
    tree.digest();
}
