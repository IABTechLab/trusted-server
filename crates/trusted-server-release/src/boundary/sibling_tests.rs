//! Pure tests of sibling boundary root derivation; nothing runs.

use std::fs;
use std::path::PathBuf;

use super::docker::container_path;
use super::macos::MacProfile;
use super::*;

struct Layout {
    _directory: tempfile::TempDir,
    base: PathBuf,
}

fn layout() -> Layout {
    let directory = tempfile::tempdir().expect("should create layout");
    let base = fs::canonicalize(directory.path()).expect("should canonicalize layout");
    for relative in ["checkout", "tools/bin", "cargo-state/home", "target"] {
        fs::create_dir_all(base.join(relative)).expect("should create root");
    }
    Layout {
        _directory: directory,
        base,
    }
}

impl Layout {
    fn recipe_roots(&self) -> BoundaryRoots {
        BoundaryRoots::new(
            &[self.base.join("checkout")],
            &[self.base.join("tools")],
            &[self.base.join("cargo-state"), self.base.join("target")],
        )
        .expect("should build recipe roots")
    }
}

#[test]
fn excluding_the_last_writable_root_keeps_every_remaining_execution_path() {
    let layout = layout();
    let roots = layout.recipe_roots();

    let sibling = roots
        .without_last_writable(&layout.base.join("target"))
        .expect("should exclude the target root");

    assert_eq!(
        sibling.writable(),
        [layout.base.join("cargo-state")],
        "should keep only the Cargo state root writable"
    );
    assert_eq!(
        sibling.immutable(),
        roots.immutable(),
        "should keep the immutable roots"
    );
    assert_eq!(sibling.tools(), roots.tools(), "should keep the tool roots");
    for host in [
        layout.base.join("checkout"),
        layout.base.join("tools/bin"),
        layout.base.join("cargo-state/home"),
    ] {
        assert_eq!(
            container_path(&sibling, &host).expect("should translate in the sibling"),
            container_path(&roots, &host).expect("should translate in the build"),
            "should keep the execution path of {}",
            host.display()
        );
    }
    assert!(
        container_path(&sibling, &layout.base.join("target")).is_err(),
        "should not expose the excluded target root"
    );
}

#[test]
fn excluding_a_writable_root_that_would_shift_indices_is_rejected() {
    let layout = layout();

    let error = layout
        .recipe_roots()
        .without_last_writable(&layout.base.join("cargo-state"))
        .expect_err("should reject excluding the first of two writable roots");

    assert!(
        format!("{error:?}").contains("only the last writable root"),
        "should explain the index-preservation rule"
    );
}

#[test]
fn excluding_the_only_writable_root_or_a_foreign_path_is_rejected() {
    let layout = layout();
    let single = BoundaryRoots::new(
        &[layout.base.join("checkout")],
        &[layout.base.join("tools")],
        &[layout.base.join("target")],
    )
    .expect("should build single-writable roots");

    assert!(
        single
            .without_last_writable(&layout.base.join("target"))
            .is_err(),
        "should keep a private writable root"
    );
    assert!(
        layout
            .recipe_roots()
            .without_last_writable(&layout.base.join("checkout"))
            .is_err(),
        "should reject excluding a root that is not writable"
    );
    assert!(
        layout
            .recipe_roots()
            .without_last_writable(&layout.base.join("missing"))
            .is_err(),
        "should reject an unresolvable exclusion"
    );
}

#[test]
fn a_macos_boundary_derives_no_sibling() {
    let layout = layout();
    let roots = layout.recipe_roots();
    let boundary = Boundary {
        platform: Platform::MacSandbox(
            MacProfile::generate(&roots).expect("should generate the profile"),
        ),
        roots,
        fingerprint: [1; 32],
        derived_from: None,
    };

    let error = boundary
        .sibling_without_writable(&layout.base.join("target"))
        .expect_err("should reject a macOS sibling");

    assert!(
        format!("{error:?}").contains("only a Linux container boundary"),
        "should name the Linux-only rule"
    );
    assert_eq!(
        boundary.derived_from(),
        None,
        "should record no parent for a directly configured boundary"
    );
}
