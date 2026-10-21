//! Post-publish immutability, membership, cleanup and RC permission cases.

use super::*;

#[test]
fn confirmed_mutable_release_is_removed_when_cleanup_is_authorized() {
    let mut api = FakeApi::new();
    api.immutable_after_publish = Some(false);

    let failure = failure(run(&mut api, CleanupGrant::Authorized));

    let record = incident(&failure);
    assert_eq!(
        record.kind(),
        IncidentKind::MutablePublication,
        "should record mutable release"
    );
    assert_eq!(
        record.cleanup(),
        CleanupOutcome::Removed,
        "should record removal"
    );
    assert_eq!(record.tag(), "v1.2.3", "should name the tag");
    assert_eq!(
        record.release(),
        Some(ReleaseId(FIRST_RELEASE_ID)),
        "should name the release"
    );
    assert!(
        api.releases.is_empty(),
        "should remove the still-mutable release"
    );
    let delete = api.position(&Call::Delete);
    assert_eq!(
        api.calls[delete - 1],
        Call::Get,
        "should reconfirm mutability before deletion"
    );
}

#[test]
fn confirmed_mutable_release_cleanup_failures_are_recorded() {
    let mut denied = FakeApi::new();
    denied.immutable_after_publish = Some(false);
    denied.fault(Operation::Delete, Fault::Status(403));
    let failure_denied = failure(run(&mut denied, CleanupGrant::Authorized));
    assert_eq!(
        incident(&failure_denied).cleanup(),
        CleanupOutcome::Denied { status: 403 },
        "should record the denied cleanup"
    );

    let mut lost = FakeApi::new();
    lost.immutable_after_publish = Some(false);
    lost.fault(Operation::Delete, Fault::Lost);
    let failure_lost = failure(run(&mut lost, CleanupGrant::Authorized));
    assert_eq!(
        incident(&failure_lost).cleanup(),
        CleanupOutcome::Removed,
        "should reconcile a lost delete response read-only"
    );
    assert_eq!(lost.count(&Call::Delete), 1, "should not retry deletion");

    let mut unreachable = FakeApi::new();
    unreachable.immutable_after_publish = Some(false);
    unreachable.fault(Operation::Delete, Fault::Unreachable);
    let failure_unreachable = failure(run(&mut unreachable, CleanupGrant::Authorized));
    assert_eq!(
        incident(&failure_unreachable).cleanup(),
        CleanupOutcome::Indeterminate,
        "should record an indeterminate cleanup"
    );
    assert_eq!(
        incident(&failure_unreachable).api_failure(),
        Some(ApiFailure::Indeterminate),
        "should carry the cleanup failure"
    );
}

#[test]
fn unknown_final_state_raises_incident_without_deletion() {
    let mut unknown_flag = FakeApi::new();
    unknown_flag.immutable_after_publish = None;
    let failure_flag = failure(run(&mut unknown_flag, CleanupGrant::Authorized));
    assert_eq!(
        incident(&failure_flag).kind(),
        IncidentKind::UnknownState,
        "should flag unknown"
    );
    assert_eq!(
        unknown_flag.count(&Call::Delete),
        0,
        "should not delete on unknown state"
    );

    let mut unreadable = FakeApi::new();
    unreadable.reads_after_publish_unknown = true;
    let failure_read = failure(run(&mut unreadable, CleanupGrant::Authorized));
    assert_eq!(
        incident(&failure_read).kind(),
        IncidentKind::UnknownState,
        "should flag unknown"
    );
    assert_eq!(
        incident(&failure_read).cleanup(),
        CleanupOutcome::NotAttempted,
        "should not attempt cleanup"
    );
    assert_eq!(
        unreadable.count(&Call::Delete),
        0,
        "should not delete on unreadable state"
    );

    let mut still_draft = FakeApi::new();
    still_draft.after_publish = Some(|release| release.draft = true);
    let failure_draft = failure(run(&mut still_draft, CleanupGrant::Authorized));
    assert_eq!(
        incident(&failure_draft).kind(),
        IncidentKind::UnknownState,
        "should flag a release still reported as draft"
    );
    assert_eq!(
        still_draft.count(&Call::Delete),
        0,
        "should not delete a release reported as draft after publish"
    );
}

#[test]
fn immutable_release_with_wrong_statement_is_never_deleted() {
    let mut api = FakeApi::new();
    api.statement = Some(ReleaseStatement {
        tag: "v1.2.3".to_owned(),
        commit: OTHER_COMMIT.to_owned(),
        subjects: Vec::new(),
    });

    let failure_statement = failure(run(&mut api, CleanupGrant::Authorized));

    let record = incident(&failure_statement);
    assert_eq!(
        record.kind(),
        IncidentKind::MembershipMismatch,
        "should flag membership"
    );
    assert_eq!(
        record.cleanup(),
        CleanupOutcome::RetainedImmutable,
        "should retain"
    );
    assert_eq!(
        api.count(&Call::Delete),
        0,
        "should never delete an immutable release"
    );

    let mut missing_subject = FakeApi::new();
    let mut statement = correct_statement();
    statement.subjects.pop();
    missing_subject.statement = Some(statement);
    let failure_subject = failure(run(&mut missing_subject, CleanupGrant::Authorized));
    assert_eq!(
        incident(&failure_subject).kind(),
        IncidentKind::MembershipMismatch,
        "should flag a missing statement subject"
    );
}

#[test]
fn immutable_release_with_wrong_observed_tag_or_commit_is_never_deleted() {
    let alterations: [fn(&mut fake::FakeRelease); 2] = [
        |release| release.tag = "v9.9.9".to_owned(),
        |release| release.commit = OTHER_COMMIT.to_owned(),
    ];
    for alter in alterations {
        let mut api = FakeApi::new();
        api.statement = Some(correct_statement());
        api.after_publish = Some(alter);

        let failure = failure(run(&mut api, CleanupGrant::Authorized));

        let record = incident(&failure);
        assert_eq!(
            record.kind(),
            IncidentKind::MembershipMismatch,
            "should flag the observed release identity"
        );
        assert_eq!(
            record.cleanup(),
            CleanupOutcome::RetainedImmutable,
            "should retain"
        );
        assert_eq!(
            api.count(&Call::Delete),
            0,
            "should never delete an immutable release"
        );
    }
}

#[test]
fn unverifiable_statement_raises_incident_without_deletion() {
    let mut api = FakeApi::new();
    api.fault(Operation::Statement, Fault::Unreachable);

    let failure = failure(run(&mut api, CleanupGrant::Authorized));

    let record = incident(&failure);
    assert_eq!(
        record.kind(),
        IncidentKind::UnverifiedStatement,
        "should flag unverified"
    );
    assert_eq!(
        record.cleanup(),
        CleanupOutcome::RetainedImmutable,
        "should retain"
    );
    assert_eq!(
        record.api_failure(),
        Some(ApiFailure::Indeterminate),
        "should carry the statement failure"
    );
    assert_eq!(
        api.count(&Call::Delete),
        0,
        "should never delete an immutable release"
    );
}

#[test]
fn approved_rc_commit_uses_the_same_contents_write_only_role() {
    let approval = SourceApproval::reviewed_rc("rc/202610").expect("should accept RC branch");
    let rc = intended_with(approval.clone());
    let widened = [JobPermission::ContentsWrite, JobPermission::IdTokenWrite];
    assert!(
        PublicationRole::from_job(&widened, CleanupGrant::Authorized).is_err(),
        "should not widen the role for an RC commit"
    );
    let mut api = FakeApi::new();

    let release = publish(&mut api, &role(CleanupGrant::NotAuthorized), &rc)
        .expect("should publish an approved RC commit");

    assert_eq!(
        release.approval(),
        &approval,
        "should record the RC approval"
    );
}

#[test]
fn approved_rc_permission_differences_fail_closed() {
    let approval = SourceApproval::reviewed_rc("rc/202610").expect("should accept RC branch");
    let rc = intended_with(approval);

    let mut denied = FakeApi::new();
    denied.fault(Operation::Publish, Fault::Status(403));
    let failure_denied = failure(publish(&mut denied, &role(CleanupGrant::Authorized), &rc));
    assert_message(&failure_denied, "403");
    assert!(
        failure_denied.incident().is_none(),
        "should leave the draft without incident"
    );
    assert_eq!(
        denied.count(&Call::Delete),
        0,
        "should not delete after a denied publish"
    );
    assert!(
        denied.releases.values().all(|release| release.draft),
        "should keep the draft"
    );

    let mut mutable = FakeApi::new();
    mutable.immutable_after_publish = Some(false);
    let failure_mutable = failure(publish(
        &mut mutable,
        &role(CleanupGrant::NotAuthorized),
        &rc,
    ));
    let record = incident(&failure_mutable);
    assert_eq!(
        record.kind(),
        IncidentKind::MutablePublication,
        "should flag mutable release"
    );
    assert_eq!(
        record.cleanup(),
        CleanupOutcome::NotAuthorized,
        "should record missing authority"
    );
    assert_eq!(
        mutable.count(&Call::Delete),
        0,
        "should not delete without authority"
    );
    assert_eq!(
        mutable.releases.len(),
        1,
        "should retain the release for the incident"
    );
}
