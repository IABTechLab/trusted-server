use std::cell::Cell;
use std::os::unix::fs::symlink;

use super::*;

fn home() -> tempfile::TempDir {
    tempfile::tempdir().expect("should create a private Cargo home")
}

fn registry(home: &Path) -> std::path::PathBuf {
    let registry = home.join("registry");
    fs::create_dir(&registry).expect("should create registry bookkeeping");
    registry
}

#[test]
fn only_absent_empty_or_exact_tag_bookkeeping_is_accepted() {
    let home = home();
    check(home.path(), &|| Ok(())).expect("should accept no registry directory");
    let registry = registry(home.path());
    check(home.path(), &|| Ok(())).expect("should accept empty registry bookkeeping");
    assert_eq!(
        CACHE_TAG.len(),
        177,
        "should match the reviewed Cargo tag length"
    );
    fs::write(registry.join("CACHEDIR.TAG"), CACHE_TAG)
        .expect("should write exact Cargo bookkeeping");
    check(home.path(), &|| Ok(())).expect("should accept only the exact cache tag");
}

#[test]
fn fetched_sources_and_unexpected_entries_are_rejected_even_with_a_valid_tag() {
    for name in ["index", "cache", "src", "unexpected"] {
        let home = home();
        let registry = registry(home.path());
        fs::write(registry.join("CACHEDIR.TAG"), CACHE_TAG).expect("should write a valid tag");
        fs::create_dir(registry.join(name)).expect("should create a forbidden entry");
        assert!(
            check(home.path(), &|| Ok(())).is_err(),
            "should reject any registry child beyond bookkeeping"
        );
    }
    let home = home();
    let registry = registry(home.path());
    fs::write(registry.join("unexpected"), b"example").expect("should create a forbidden file");
    assert!(
        check(home.path(), &|| Ok(())).is_err(),
        "should reject unexpected regular files"
    );
}

#[test]
fn changed_truncated_or_extended_tag_bytes_are_rejected() {
    for bytes in [
        &b"example bookkeeping"[..],
        &CACHE_TAG[..CACHE_TAG.len() - 1],
        &[CACHE_TAG, b"extra"].concat(),
        &vec![b'x'; CACHE_TAG.len()],
    ] {
        let home = home();
        let registry = registry(home.path());
        fs::write(registry.join("CACHEDIR.TAG"), bytes).expect("should write invalid tag bytes");
        assert!(
            check(home.path(), &|| Ok(())).is_err(),
            "should reject every noncanonical tag"
        );
    }
}

#[test]
fn registry_aliases_and_nondirectories_are_rejected() {
    let home = home();
    let outside = home.path().join("example");
    fs::create_dir(&outside).expect("should create a link target");
    symlink(&outside, home.path().join("registry")).expect("should create an alias");
    assert!(
        check(home.path(), &|| Ok(())).is_err(),
        "should reject a registry alias"
    );
    fs::remove_file(home.path().join("registry")).expect("should remove alias");
    fs::write(home.path().join("registry"), b"example").expect("should create a nondirectory");
    assert!(
        check(home.path(), &|| Ok(())).is_err(),
        "should require a directory"
    );
}

#[test]
fn tag_aliases_hardlinks_and_directories_are_rejected() {
    let home = home();
    let registry = registry(home.path());
    let outside = home.path().join("example-tag");
    let tag = registry.join("CACHEDIR.TAG");
    fs::write(&outside, CACHE_TAG).expect("should write an external tag fixture");
    symlink(&outside, &tag).expect("should create tag alias");
    assert!(
        check(home.path(), &|| Ok(())).is_err(),
        "should reject a tag alias"
    );
    fs::remove_file(&tag).expect("should remove tag alias");
    fs::hard_link(&outside, &tag).expect("should create a shared tag inode");
    assert!(
        check(home.path(), &|| Ok(())).is_err(),
        "should reject a hard-linked tag"
    );
    fs::remove_file(&tag).expect("should remove hard link");
    fs::create_dir(&tag).expect("should create a tag directory");
    assert!(
        check(home.path(), &|| Ok(())).is_err(),
        "should require a regular tag file"
    );
}

#[test]
fn git_cache_stays_forbidden_including_empty_directories_and_dangling_aliases() {
    let home = home();
    let git = home.path().join("git");
    fs::create_dir(&git).expect("should create an empty Git cache");
    assert!(
        check(home.path(), &|| Ok(())).is_err(),
        "should reject even an empty Git cache"
    );
    fs::remove_dir(&git).expect("should remove the directory");
    symlink("missing", &git).expect("should create a dangling Git alias");
    assert!(
        check(home.path(), &|| Ok(())).is_err(),
        "should reject a dangling Git alias"
    );
}

#[test]
fn cancellation_is_not_skipped_for_absent_or_present_registry_state() {
    let home = home();
    let cancelled = || Err(recipe_error("example cancelled attempt"));
    assert!(
        check(home.path(), &cancelled).is_err(),
        "should check an absent registry attempt"
    );
    registry(home.path());
    assert!(
        check(home.path(), &cancelled).is_err(),
        "should check a present registry attempt"
    );
}

#[test]
fn replaced_registry_directory_is_rejected_even_with_identical_tag_bytes() {
    let home = home();
    let registry = registry(home.path());
    fs::write(registry.join("CACHEDIR.TAG"), CACHE_TAG).expect("should write a tag");
    let calls = Cell::new(0);
    let mutate = || {
        calls.set(calls.get() + 1);
        if calls.get() == 2 {
            fs::rename(&registry, home.path().join("original"))
                .expect("should replace the registry directory");
            fs::create_dir(&registry).expect("should create a replacement directory");
            fs::write(registry.join("CACHEDIR.TAG"), CACHE_TAG)
                .expect("should copy identical bytes");
        }
        Ok(())
    };
    assert!(
        check(home.path(), &mutate).is_err(),
        "should bind inspection to the original directory inode"
    );
    assert!(
        calls.get() >= 2,
        "should exercise replacement during inspection"
    );
}
