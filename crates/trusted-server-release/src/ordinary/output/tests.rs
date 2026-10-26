use std::fs;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _, symlink};

use serde_json::Value;

use super::{COMPANION, companion, observation};
use crate::ordinary::transfer::tests::checked_fixture;

#[test]
fn directory_iteration_errors_never_count_as_a_successful_entry() {
    let entries = [Err(std::io::Error::other("fictional enumeration error"))];
    assert!(
        super::exact_entry(entries.into_iter(), COMPANION).is_err(),
        "should fail closed when directory enumeration fails"
    );
    assert!(
        !super::exact_entry([Ok("other.json".into())].into_iter(), COMPANION)
            .expect("should inspect one successful entry"),
        "should require the exact permanent filename"
    );
}

#[test]
fn writes_one_real_matched_companion_atomically_in_a_private_fresh_directory() {
    let temporary = tempfile::tempdir().expect("should create output parent");
    let parent = fs::canonicalize(temporary.path()).expect("should resolve output parent");
    let directory = parent.join("comparison");
    let path = companion(&checked_fixture(), &directory)
        .expect("should seal the real ordinary comparison");
    assert_eq!(
        path,
        directory.join(COMPANION),
        "should retain the permanent index-contract filename"
    );
    let value: Value =
        serde_json::from_slice(&fs::read(&path).expect("should read sealed receipt"))
            .expect("should parse real receipt");
    assert_eq!(
        value["outcome"], "matched",
        "should publish only the actual successful outcome"
    );
    assert_eq!(
        value["release_qualification"], false,
        "should preserve ordinary scope"
    );
    assert_eq!(
        fs::read_dir(&directory)
            .expect("should inspect exact outputs")
            .count(),
        1,
        "should emit no placeholder, index or extra companion"
    );
    let metadata = fs::metadata(path).expect("should inspect receipt");
    assert_eq!(
        metadata.nlink(),
        1,
        "should retain a single-link regular output"
    );
    assert_eq!(
        metadata.permissions().mode() & 0o777,
        0o600,
        "should write private file permissions"
    );
    assert_eq!(
        fs::metadata(directory)
            .expect("should inspect output root")
            .permissions()
            .mode()
            & 0o777,
        0o700,
        "should create a private output root"
    );
}

#[test]
fn refuses_reused_output_roots_without_overwriting_any_existing_bytes() {
    let temporary = tempfile::tempdir().expect("should create output parent");
    let parent = fs::canonicalize(temporary.path()).expect("should resolve output parent");
    let directory = parent.join("comparison");
    companion(&checked_fixture(), &directory).expect("should create the first real comparison");
    let original = fs::read(directory.join(COMPANION)).expect("should retain first output");
    assert!(
        companion(&checked_fixture(), &directory).is_err(),
        "should refuse reusing a comparison directory"
    );
    assert_eq!(
        fs::read(directory.join(COMPANION)).expect("should retain original receipt"),
        original,
        "should never overwrite an earlier result"
    );
}

#[test]
fn refuses_aliased_output_parents_before_creating_a_result() {
    let temporary = tempfile::tempdir().expect("should create output parent");
    let parent = fs::canonicalize(temporary.path()).expect("should resolve output parent");
    fs::create_dir(parent.join("real")).expect("should create real parent");
    symlink("real", parent.join("alias")).expect("should alias output parent");
    assert!(
        companion(&checked_fixture(), &parent.join("alias/comparison")).is_err(),
        "should reject an output parent alias"
    );
    assert!(
        !parent.join("real/comparison").exists(),
        "should produce no result after alias rejection"
    );
}

#[test]
fn observation_transfer_is_separate_from_a_publishable_companion() {
    let temporary = tempfile::tempdir().expect("should create output parent");
    let parent = fs::canonicalize(temporary.path()).expect("should resolve output parent");
    let observed = crate::ordinary::observation(
        &serde_json::to_vec(&crate::ordinary::tests::fixture())
            .expect("should encode an ordinary observation fixture"),
    )
    .expect("should parse ordinary data without granting qualification");
    let root = parent.join("observation");
    let path =
        observation(&observed, &root).expect("should seal only the unpublished transfer sidecar");
    assert_eq!(
        path,
        root.join("fastly-observation.json"),
        "should match the fixed observation archive profile"
    );
    assert!(
        !root.join(COMPANION).exists(),
        "should not manufacture an equality result from an observation"
    );
    assert!(
        observation(&observed, &root).is_err(),
        "should refuse reusing an observation transfer root too"
    );
}
