//! Input validation, bounds, role and concurrency policy.

use super::*;

#[test]
fn concurrency_group_is_tag_scoped_without_cancellation() {
    let policy = ConcurrencyPolicy::for_tag(&tag());
    let other = ConcurrencyPolicy::for_tag(&ReleaseTag::parse("v1.2.4").expect("should parse tag"));

    assert_eq!(
        policy.group(),
        "release-publication-v1.2.3",
        "should scope the group to the tag"
    );
    assert_ne!(
        policy.group(),
        other.group(),
        "should not share groups across tags"
    );
    assert!(
        !policy.cancel_in_progress(),
        "should never cancel a publishing run"
    );
}

#[test]
fn rejects_invalid_tags_assets_and_rc_branches() {
    for value in [
        "1.2.3",
        "v1.2",
        "v01.2.3",
        "v1.2.3-",
        "v1.2.3+",
        "v1.2.3 ",
        "refs/tags/v1.2.3",
    ] {
        assert!(
            ReleaseTag::parse(value).is_err(),
            "should reject tag {value:?}"
        );
    }
    for value in ["v1.2.3", "v1.2.3-rc.1", "v1.2.3-rc.1+build.5"] {
        assert!(
            ReleaseTag::parse(value).is_ok(),
            "should accept tag {value:?}"
        );
    }
    for name in ["", ".hidden", "dir/asset", "asset name"] {
        assert!(
            IntendedAsset::new(name, vec![1]).is_err(),
            "should reject asset name {name:?}"
        );
    }
    let duplicate = vec![
        IntendedAsset::new("example-server.wasm", vec![1]).expect("should accept asset"),
        IntendedAsset::new("example-server.wasm", vec![2]).expect("should accept asset"),
    ];
    assert!(
        IntendedRelease::new(tag(), commit(COMMIT), SourceApproval::Main, duplicate).is_err(),
        "should reject duplicate asset names"
    );
    assert!(
        IntendedRelease::new(tag(), commit(COMMIT), SourceApproval::Main, Vec::new()).is_err(),
        "should reject an empty asset set"
    );
    for branch in ["rc/2026", "rc/202613", "release/202610", "rc/202610-extra"] {
        assert!(
            SourceApproval::reviewed_rc(branch).is_err(),
            "should reject RC branch {branch:?}"
        );
    }
}

fn numbered_assets(count: usize, size: usize) -> Vec<IntendedAsset> {
    (0..count)
        .map(|index| {
            IntendedAsset::new(&format!("example-asset-{index}"), vec![b'x'; size])
                .expect("should accept asset")
        })
        .collect()
}

#[test]
fn release_bounds_derive_from_the_index_asset_set_and_role_limits() {
    let bounds = AssetBounds::release();

    assert_eq!(
        bounds.count(),
        32,
        "should allow four release-wide assets plus seven per index artifact"
    );
    assert_eq!(
        bounds.asset_bytes(),
        crate::documents::Role::Executable.limit(),
        "should cap each asset at the executable role limit"
    );
    assert!(
        IntendedRelease::new(
            tag(),
            commit(COMMIT),
            SourceApproval::Main,
            numbered_assets(33, 1)
        )
        .is_err(),
        "should reject more assets than the index asset set"
    );
    assert!(
        IntendedRelease::new(
            tag(),
            commit(COMMIT),
            SourceApproval::Main,
            numbered_assets(32, 1)
        )
        .is_ok(),
        "should accept the full index asset set"
    );
}

#[test]
fn rejects_oversized_asset_and_total_under_bounds() {
    let bounds = AssetBounds::new(4, 8, 12);
    let build = |assets| {
        IntendedRelease::new_bounded(tag(), commit(COMMIT), SourceApproval::Main, assets, &bounds)
    };

    assert!(
        build(numbered_assets(1, 9)).is_err(),
        "should reject an asset over the per-asset bound"
    );
    assert!(
        build(numbered_assets(2, 7)).is_err(),
        "should reject a set over the total bound"
    );
    assert!(
        build(numbered_assets(5, 1)).is_err(),
        "should reject a set over the count bound"
    );
    assert!(
        build(numbered_assets(2, 6)).is_ok(),
        "should accept a set within every bound"
    );
}

#[test]
fn role_accepts_only_contents_write() {
    for granted in [
        vec![JobPermission::ContentsWrite],
        vec![JobPermission::ContentsRead, JobPermission::ContentsWrite],
    ] {
        assert!(
            PublicationRole::from_job(&granted, CleanupGrant::NotAuthorized).is_ok(),
            "should accept {granted:?}"
        );
    }
    for extra in [
        JobPermission::IdTokenWrite,
        JobPermission::AttestationsWrite,
        JobPermission::ActionsRead,
        JobPermission::ChecksRead,
        JobPermission::ProviderCredential,
        JobPermission::RepositorySettings,
    ] {
        let granted = [JobPermission::ContentsWrite, extra];
        assert!(
            PublicationRole::from_job(&granted, CleanupGrant::Authorized).is_err(),
            "should reject publication role carrying {extra:?}"
        );
    }
    assert!(
        PublicationRole::from_job(&[JobPermission::ContentsRead], CleanupGrant::NotAuthorized)
            .is_err(),
        "should require contents write"
    );
}
