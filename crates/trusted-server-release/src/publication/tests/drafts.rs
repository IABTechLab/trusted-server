//! Draft location, resume, asset validation and tag-ref guards.

use super::*;

#[test]
fn publishes_fresh_release_after_exact_redownload_and_immutable_membership() {
    let mut api = FakeApi::new();

    let release = run(&mut api, CleanupGrant::Authorized).expect("should publish");

    assert_eq!(
        release.id(),
        ReleaseId(FIRST_RELEASE_ID),
        "should publish the created draft"
    );
    assert_eq!(release.tag(), "v1.2.3", "should publish the intended tag");
    let publish_index = api.position(&Call::Publish);
    for (name, _) in assets() {
        let download = api.position(&Call::Download(name.to_owned()));
        assert!(
            download < publish_index,
            "should validate {name} before publishing"
        );
    }
    assert_eq!(
        api.calls.last(),
        Some(&Call::Statement),
        "should end with release statement verification"
    );
    assert_eq!(api.count(&Call::Delete), 0, "should never delete");
}

#[test]
fn resumes_matching_partial_draft_without_reuploading_present_assets() {
    let mut api = FakeApi::new();
    let present = assets()[..1].to_vec();
    api.seed(draft_with(COMMIT, &present));

    run(&mut api, CleanupGrant::NotAuthorized).expect("should resume and publish");

    assert_eq!(
        api.count(&Call::Create),
        0,
        "should resume rather than create"
    );
    assert_eq!(
        api.count(&Call::Upload(present[0].0.to_owned())),
        0,
        "should not overwrite a matching present asset"
    );
    assert_eq!(
        api.count(&Call::Upload("example-server.wasm".to_owned())),
        1,
        "should upload missing"
    );
}

#[test]
fn refuses_draft_with_conflicting_source_commit() {
    let mut api = FakeApi::new();
    api.seed(draft_with(OTHER_COMMIT, &[]));

    let failure = failure(run(&mut api, CleanupGrant::Authorized));

    assert_message(&failure, "conflicting draft");
    assert!(
        api.mutations().is_empty(),
        "should not mutate a conflicting draft"
    );
}

#[test]
fn refuses_draft_with_unexpected_or_altered_asset() {
    let mut extra = FakeApi::new();
    extra.seed(draft_with(COMMIT, &[("example-unexpected.txt", b"extra")]));
    let failure_extra = failure(run(&mut extra, CleanupGrant::Authorized));
    assert_message(&failure_extra, "outside the intended set");
    assert!(
        extra.mutations().is_empty(),
        "should not mutate a draft with extra assets"
    );

    let mut altered = FakeApi::new();
    altered.seed(draft_with(
        COMMIT,
        &[("example-server.wasm", b"\0asm\x01\0\0\0altered")],
    ));
    let failure_altered = failure(run(&mut altered, CleanupGrant::Authorized));
    assert_message(&failure_altered, "explicit recovery");
    assert!(
        altered.mutations().is_empty(),
        "should not overwrite or publish altered bytes"
    );
}

#[test]
fn refuses_publication_when_an_upload_goes_missing() {
    let mut api = FakeApi::new();
    api.fault(Operation::Upload, Fault::Dropped);

    let failure = failure(run(&mut api, CleanupGrant::Authorized));

    assert_message(&failure, "missing");
    assert_eq!(
        api.count(&Call::Publish),
        0,
        "should not publish an incomplete set"
    );
}

#[test]
fn refuses_publication_when_redownloaded_bytes_are_altered() {
    let mut api = FakeApi::new();
    api.fault(Operation::Upload, Fault::Corrupted);

    let failure = failure(run(&mut api, CleanupGrant::Authorized));

    assert_message(&failure, "explicit recovery");
    assert_eq!(
        api.count(&Call::Publish),
        0,
        "should not publish altered bytes"
    );
}

#[test]
fn refuses_oversized_download_before_comparing_bytes() {
    let mut api = FakeApi::new();
    api.seed(draft_with(COMMIT, &assets()[..1]));
    api.fault(Operation::Download, Fault::Oversized);

    let failure = failure(run(&mut api, CleanupGrant::Authorized));

    assert_message(&failure, "exceeds its intended size");
    assert!(
        api.mutations().is_empty(),
        "should not upload or publish after an oversized read"
    );
}

#[test]
fn refuses_already_published_tag_without_mutation() {
    for immutable in [Some(true), Some(false), None] {
        let mut api = FakeApi::new();
        api.seed(published(immutable));

        let failure = failure(run(&mut api, CleanupGrant::Authorized));

        assert_message(&failure, "already has a published release");
        assert!(
            api.mutations().is_empty(),
            "should never overwrite or delete a published tag"
        );
    }
}

#[test]
fn concurrent_rerun_resumes_draft_created_by_racing_run() {
    let mut api = FakeApi::new();
    api.fault(Operation::Create, Fault::Raced(422));

    run(&mut api, CleanupGrant::NotAuthorized).expect("should resume the raced draft");

    assert_eq!(
        api.count(&Call::Create),
        1,
        "should not retry draft creation"
    );
    assert_eq!(api.releases.len(), 1, "should leave exactly one release");
}

#[test]
fn concurrent_rerun_refuses_multiple_drafts_and_published_successor() {
    let mut drafts = FakeApi::new();
    drafts.seed(draft_with(COMMIT, &[]));
    drafts.seed(draft_with(COMMIT, &[]));
    let failure_drafts = failure(run(&mut drafts, CleanupGrant::Authorized));
    assert_message(&failure_drafts, "multiple drafts");
    assert!(
        drafts.mutations().is_empty(),
        "should not pick one of several drafts"
    );

    let mut sequential = FakeApi::new();
    run(&mut sequential, CleanupGrant::Authorized).expect("should publish the first run");
    let before = sequential.mutations().len();
    let failure_rerun = failure(run(&mut sequential, CleanupGrant::Authorized));
    assert_message(&failure_rerun, "already has a published release");
    assert_eq!(
        sequential.mutations().len(),
        before,
        "should not mutate on rerun"
    );
}

#[test]
fn bare_tag_at_another_commit_is_refused_before_creating_a_draft() {
    let mut api = FakeApi::new();
    api.tags
        .insert("v1.2.3".to_owned(), OTHER_COMMIT.to_owned());

    let failure = failure(run(&mut api, CleanupGrant::Authorized));

    assert_message(&failure, "already points at");
    assert!(api.mutations().is_empty(), "should not create a draft");
}

#[test]
fn bare_tag_at_the_intended_commit_is_allowed() {
    let mut api = FakeApi::new();
    api.tags.insert("v1.2.3".to_owned(), COMMIT.to_owned());

    run(&mut api, CleanupGrant::Authorized).expect("should publish onto the matching tag");
}

#[test]
fn unreadable_tag_ref_is_refused_without_mutation() {
    let mut api = FakeApi::new();
    api.fault(Operation::TagRef, Fault::Unreachable);

    let failure = failure(run(&mut api, CleanupGrant::Authorized));

    assert_message(&failure, "tag reference");
    assert!(api.mutations().is_empty(), "should not mutate");
}

#[test]
fn tag_moved_to_another_commit_before_publish_is_refused() {
    let mut api = FakeApi::new();
    api.tag_ref_hook = Some((2, |api| {
        api.tags
            .insert("v1.2.3".to_owned(), OTHER_COMMIT.to_owned());
    }));

    let failure = failure(run(&mut api, CleanupGrant::Authorized));

    assert_message(&failure, "already points at");
    assert_eq!(api.count(&Call::Publish), 0, "should not publish");
    assert!(
        failure.incident().is_none(),
        "should keep the draft quietly"
    );
    assert!(
        api.releases.values().all(|release| release.draft),
        "should keep the draft"
    );
}

#[test]
fn draft_changed_after_validation_is_refused_before_publish() {
    let mut api = FakeApi::new();
    api.tag_ref_hook = Some((2, |api| api.reupload_with_new_id("example-server.wasm")));

    let failure = failure(run(&mut api, CleanupGrant::Authorized));

    assert_message(&failure, "changed after validation");
    assert_eq!(api.count(&Call::Publish), 0, "should not publish");
    assert!(
        api.releases.values().all(|release| release.draft),
        "should keep the draft"
    );
    let tag_ref = api
        .calls
        .iter()
        .rposition(|call| *call == Call::TagRef)
        .expect("should check the tag ref");
    assert_eq!(
        api.calls.get(tag_ref + 1),
        Some(&Call::Get),
        "should re-read the draft immediately before publishing"
    );
}
