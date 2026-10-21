//! Lost responses reconcile read-only before any other mutation.

use super::*;

#[test]
fn lost_create_response_reconciles_read_only_before_continuing() {
    let mut applied = FakeApi::new();
    applied.fault(Operation::Create, Fault::Lost);
    run(&mut applied, CleanupGrant::NotAuthorized).expect("should resume the created draft");
    let create = applied.position(&Call::Create);
    assert_eq!(
        applied.calls.get(create + 1),
        Some(&Call::List),
        "should reconcile next"
    );
    assert_eq!(
        applied.count(&Call::Create),
        1,
        "should not blindly recreate"
    );

    let mut absent = FakeApi::new();
    absent.fault(Operation::Create, Fault::Unreachable);
    let failure = failure(run(&mut absent, CleanupGrant::NotAuthorized));
    assert_message(&failure, "reconciliation found no draft");
    assert_eq!(
        absent.mutations(),
        vec![Call::Create],
        "should stop after reconciliation"
    );
}

#[test]
fn lost_upload_response_reconciles_before_another_mutation() {
    let mut applied = FakeApi::new();
    applied.fault(Operation::Upload, Fault::Lost);
    run(&mut applied, CleanupGrant::NotAuthorized).expect("should accept reconciled upload");
    let first = assets()[0].0.to_owned();
    assert_eq!(
        applied.count(&Call::Upload(first)),
        1,
        "should not re-upload a reconciled asset"
    );

    let mut absent = FakeApi::new();
    absent.fault(Operation::Upload, Fault::Unreachable);
    let failure = failure(run(&mut absent, CleanupGrant::NotAuthorized));
    assert_message(&failure, "absent after reconciliation");
    assert_eq!(absent.count(&Call::Publish), 0, "should not publish");

    absent.calls.clear();
    run(&mut absent, CleanupGrant::NotAuthorized)
        .expect("should resume the partial draft on rerun");
    assert_eq!(
        absent.count(&Call::Create),
        0,
        "should resume the existing draft"
    );
}

#[test]
fn lost_publish_response_reconciles_published_release() {
    let mut api = FakeApi::new();
    api.fault(Operation::Publish, Fault::Lost);

    run(&mut api, CleanupGrant::NotAuthorized).expect("should accept reconciled publication");

    assert_eq!(api.count(&Call::Publish), 1, "should not republish");
}

#[test]
fn lost_publish_response_with_draft_still_unpublished_fails_without_retry() {
    let mut api = FakeApi::new();
    api.fault(Operation::Publish, Fault::Unreachable);

    let failure = failure(run(&mut api, CleanupGrant::Authorized));

    assert_message(&failure, "still unpublished");
    assert_eq!(
        api.count(&Call::Publish),
        1,
        "should not retry publish blindly"
    );
    assert!(
        failure.incident().is_none(),
        "should leave a resumable draft, not an incident"
    );
}

#[test]
fn lost_publish_response_with_unknown_state_raises_incident_without_deletion() {
    let mut api = FakeApi::new();
    api.fault(Operation::Publish, Fault::Lost);
    api.reads_after_publish_unknown = true;

    let failure = failure(run(&mut api, CleanupGrant::Authorized));

    let record = incident(&failure);
    assert_eq!(
        record.kind(),
        IncidentKind::UnknownState,
        "should record unknown state"
    );
    assert_eq!(
        record.cleanup(),
        CleanupOutcome::NotAttempted,
        "should not attempt cleanup"
    );
    assert_eq!(
        record.api_failure(),
        Some(ApiFailure::Indeterminate),
        "should carry the reconciliation failure"
    );
    assert_message(&failure, "Indeterminate");
    assert_eq!(api.count(&Call::Delete), 0, "should never delete blindly");
    assert_eq!(api.count(&Call::Publish), 1, "should not mutate again");
}
