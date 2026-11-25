use std::fs;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _, symlink};

use serde_json::Value;

use super::{COMPANION, companion, observation};
use crate::ordinary::transfer::tests::checked_fixture;

#[test]
fn failed_post_persist_or_final_verification_leaves_no_matched_companion() {
    for point in [
        super::WriteCheckpoint::Persisted,
        super::WriteCheckpoint::Verified,
    ] {
        let temporary = tempfile::tempdir().expect("should create failure fixture");
        let parent = fs::canonicalize(temporary.path()).expect("should resolve output parent");
        let directory = parent.join("comparison");
        let result = super::persist_with_checkpoints(
            &directory,
            COMPANION,
            &checked_fixture().receipt(),
            crate::documents::Role::Companion,
            &|site, _| {
                if site == point {
                    Err(super::invalid())
                } else {
                    Ok(())
                }
            },
        );
        assert!(
            result.is_err(),
            "should return the injected sealing failure"
        );
        assert!(
            !directory.join(COMPANION).exists(),
            "should remove the owned matched result after every later error"
        );
    }
}

#[test]
fn real_membership_failure_removes_only_the_new_result() {
    let temporary = tempfile::tempdir().expect("should create membership fixture");
    let parent = fs::canonicalize(temporary.path()).expect("should resolve output parent");
    let directory = parent.join("comparison");
    let result = super::persist_with_checkpoints(
        &directory,
        COMPANION,
        &checked_fixture().receipt(),
        crate::documents::Role::Companion,
        &|site, root| {
            if site == super::WriteCheckpoint::Persisted {
                fs::write(root.join("extra"), b"retained evidence")
                    .expect("should inject an extra output");
            }
            Ok(())
        },
    );
    assert!(
        result.is_err(),
        "should fail the actual exact-membership check"
    );
    assert!(
        !directory.join(COMPANION).exists(),
        "should remove its newly persisted result"
    );
    assert_eq!(
        fs::read(directory.join("extra")).expect("should retain other evidence"),
        b"retained evidence",
        "should not delete unrelated files"
    );
}

#[test]
fn rollback_uses_the_original_directory_without_following_its_replacement() {
    let temporary = tempfile::tempdir().expect("should create root replacement fixture");
    let parent = fs::canonicalize(temporary.path()).expect("should resolve output parent");
    let directory = parent.join("comparison");
    let retained = parent.join("retained");
    let replacement = parent.join("replacement");
    fs::create_dir(&replacement).expect("should create unrelated replacement tree");
    fs::write(replacement.join(COMPANION), b"unrelated bytes")
        .expect("should retain unrelated file");
    let result = super::persist_with_checkpoints(
        &directory,
        COMPANION,
        &checked_fixture().receipt(),
        crate::documents::Role::Companion,
        &|site, root| {
            if site == super::WriteCheckpoint::Persisted {
                fs::rename(root, &retained).expect("should retain the original output directory");
                symlink(&replacement, root).expect("should replace output root with a symlink");
                return Err(super::invalid());
            }
            Ok(())
        },
    );
    assert!(
        result.is_err(),
        "should fail sealing after the injected root change"
    );
    assert!(
        !retained.join(COMPANION).exists(),
        "should unlink only the result inside its held directory"
    );
    assert_eq!(
        fs::read(replacement.join(COMPANION)).expect("should retain unrelated file"),
        b"unrelated bytes",
        "should never traverse the replacement root"
    );
}

#[test]
fn rollback_preserves_a_replacement_leaf_when_its_own_result_is_already_unlinked() {
    let temporary = tempfile::tempdir().expect("should create leaf replacement fixture");
    let parent = fs::canonicalize(temporary.path()).expect("should resolve output parent");
    let directory = parent.join("comparison");
    let result = super::persist_with_checkpoints(
        &directory,
        COMPANION,
        &checked_fixture().receipt(),
        crate::documents::Role::Companion,
        &|site, root| {
            if site == super::WriteCheckpoint::Persisted {
                fs::remove_file(root.join(COMPANION)).expect("should unlink the owned result");
                fs::write(root.join(COMPANION), b"replacement").expect("should replace the leaf");
                return Err(super::invalid());
            }
            Ok(())
        },
    );
    assert!(result.is_err(), "should retain the failed command outcome");
    assert_eq!(
        fs::read(directory.join(COMPANION)).expect("should retain replacement"),
        b"replacement",
        "should not remove a file it did not create"
    );
}

#[test]
fn rollback_reports_a_retained_inode_that_was_moved_away_from_its_destination() {
    let temporary = tempfile::tempdir().expect("should create moved-result fixture");
    let parent = fs::canonicalize(temporary.path()).expect("should resolve output parent");
    let directory = parent.join("comparison");
    let moved = parent.join("moved-result");
    let result = super::persist_with_checkpoints(
        &directory,
        COMPANION,
        &checked_fixture().receipt(),
        crate::documents::Role::Companion,
        &|site, root| {
            if site == super::WriteCheckpoint::Persisted {
                fs::rename(root.join(COMPANION), &moved).expect("should move the owned inode");
                fs::write(root.join(COMPANION), b"unrelated replacement")
                    .expect("should replace the leaf");
                return Err(super::invalid());
            }
            Ok(())
        },
    );
    let error = result.expect_err("should fail rather than removing an observed replacement");
    assert!(
        format!("{error:?}").contains("ordinary output rollback failed"),
        "should report incomplete cleanup loudly"
    );
    assert_eq!(
        fs::read(directory.join(COMPANION)).expect("should preserve replacement"),
        b"unrelated replacement",
        "should not unlink another inode"
    );
    assert!(
        moved.exists(),
        "should not guess an unknown location for the original inode"
    );
}

#[test]
fn rollback_reports_an_unknown_hardlink_after_removing_the_owned_destination() {
    let temporary = tempfile::tempdir().expect("should create linked-result fixture");
    let parent = fs::canonicalize(temporary.path()).expect("should resolve output parent");
    let directory = parent.join("comparison");
    let linked = parent.join("linked-result");
    let result = super::persist_with_checkpoints(
        &directory,
        COMPANION,
        &checked_fixture().receipt(),
        crate::documents::Role::Companion,
        &|site, root| {
            if site == super::WriteCheckpoint::Persisted {
                fs::hard_link(root.join(COMPANION), &linked)
                    .expect("should inject an extra inode link");
            }
            Ok(())
        },
    );
    let error = result.expect_err("should reject the actual linked-file readback");
    assert!(
        !directory.join(COMPANION).exists(),
        "should remove the destination it created"
    );
    assert!(
        linked.exists(),
        "should not guess or sweep other link locations"
    );
    assert!(
        format!("{error:?}").contains("ordinary output rollback failed"),
        "should report observed incomplete cleanup"
    );
}

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
