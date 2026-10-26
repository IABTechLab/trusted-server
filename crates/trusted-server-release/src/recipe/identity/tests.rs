use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::json;
use sha2::Sha256;

use super::*;
use crate::boundary::Umask;
use crate::documents::{Role, canonical_json};
use crate::policy::FIXED_ARTIFACTS;
use crate::source::CommitId;

fn source(commit: char, manifest: bool) -> ApprovedSource {
    ApprovedSource::in_memory_for_tests(
        &CommitId::parse(&commit.to_string().repeat(40))
            .expect("should parse fictional source commit"),
        if manifest { &["Cargo.toml"] } else { &[] },
    )
}

fn asset(index: usize) -> AssetId {
    AssetId::parse(FIXED_ARTIFACTS[index].name).expect("should select a fixed cohort asset")
}

fn plan() -> CargoPlan {
    CargoPlan {
        asset: asset(0),
        program: PathBuf::from("/example/tools/bin/cargo"),
        arguments: vec![
            "build".to_owned(),
            "--release".to_owned(),
            "--jobs".to_owned(),
            "1".to_owned(),
        ],
        environment: BTreeMap::from([("LANG".to_owned(), "C".to_owned())]),
        working_directory: PathBuf::from("/example/source-a"),
        umask: Umask::new(0o022).expect("should accept canonical fixture umask"),
        apple_identity_sha256: None,
    }
}

#[test]
fn shared_definition_binds_exact_source_owned_asset_and_profile_descriptor() {
    let approved = source('a', true);
    let definition = RecipeDefinitionIdentity::from_source(&approved, asset(0))
        .expect("should select source-owned recipe definition");
    let value =
        serde_json::to_value(definition.descriptor()).expect("should serialize typed definition");
    assert_eq!(
        value,
        json!({"domain":"trusted-server.release.recipe-definition","schema_version":1,"source_commit":"a".repeat(40),"asset":FIXED_ARTIFACTS[0].name,"package":FIXED_ARTIFACTS[0].package,"binary":FIXED_ARTIFACTS[0].bin,"target":FIXED_ARTIFACTS[0].target.triple(),"variant":"default","cargo":{"command":"build","profile":"release","default_features":true,"features":[],"frozen":true},"profile_definition":{"path":"Cargo.toml","sha256":"0".repeat(64)}}),
        "should bind only selected source policy and fixed asset fields"
    );
    let bytes = canonical_json(&value, Role::Metadata).expect("should canonicalize descriptor");
    assert_eq!(
        definition.sha256(),
        hex::encode(Sha256::digest(&bytes)),
        "should hash exact domain-separated descriptor bytes"
    );
    assert_eq!(
        definition,
        RecipeDefinitionIdentity::from_source(&approved, asset(0))
            .expect("should repeat identical selection"),
        "should keep identical source and asset stable"
    );
}

#[test]
fn source_asset_target_and_profile_manifest_changes_change_definition_identity() {
    let approved = source('a', true);
    let original = RecipeDefinitionIdentity::from_source(&approved, asset(0))
        .expect("should select baseline definition");
    let changed_source = RecipeDefinitionIdentity::from_source(&source('b', true), asset(0))
        .expect("should select another approved source");
    assert_ne!(
        original.sha256(),
        changed_source.sha256(),
        "should bind the exact approved source commit"
    );
    for index in 1..FIXED_ARTIFACTS.len() {
        let other = RecipeDefinitionIdentity::from_source(&approved, asset(index))
            .expect("should select another cohort asset and target");
        assert_ne!(
            original.sha256(),
            other.sha256(),
            "should distinguish fixed asset/target {index}"
        );
    }
    let arm = RecipeDefinitionIdentity::from_source(&approved, asset(1))
        .expect("should select ARM CLI definition");
    let intel = RecipeDefinitionIdentity::from_source(&approved, asset(2))
        .expect("should select Intel CLI definition");
    assert_ne!(
        arm.sha256(),
        intel.sha256(),
        "should distinguish the same package/binary on different targets"
    );
    let mut changed_manifest = original.descriptor().clone();
    changed_manifest.profile_definition.sha256 = "1".repeat(64);
    assert_ne!(
        original.sha256(),
        hash_definition(&changed_manifest).expect("should hash a changed private descriptor"),
        "should include the raw approved release-profile manifest digest"
    );
}

#[test]
fn missing_approved_root_manifest_and_unknown_assets_are_rejected() {
    assert!(
        RecipeDefinitionIdentity::from_source(&source('a', false), asset(0)).is_err(),
        "should require the approved tracked profile manifest"
    );
    assert!(
        AssetId::parse("example-unknown-asset").is_err(),
        "should preserve the existing fixed-asset constructor boundary"
    );
}

#[test]
fn retained_definition_matches_only_its_approved_source_and_fixed_asset() {
    let approved = source('a', true);
    let identity = RecipeDefinitionIdentity::from_source(&approved, asset(0))
        .expect("should select a source-owned definition");
    assert!(
        identity.matches_selection(approved.commit(), asset(0)),
        "should recognize the same immutable source and fixed asset"
    );
    assert!(
        !identity.matches_selection(source('b', true).commit(), asset(0)),
        "should reject another source commit at the checked handoff"
    );
    assert!(
        !identity.matches_selection(approved.commit(), asset(1)),
        "should reject another fixed asset at the checked handoff"
    );
}

#[test]
fn exact_invocations_preserve_existing_digest_and_ab_variations_do_not_change_definition() {
    let approved = source('a', true);
    let shared = RecipeDefinitionIdentity::from_source(&approved, asset(0))
        .expect("should select common definition");
    let canonical = plan();
    let exact = ExactInvocationDigest::from_plan(&canonical)
        .expect("should bind exact canonical invocation");
    assert_eq!(
        exact.sha256(),
        canonical
            .digest()
            .expect("should calculate existing plan digest"),
        "should preserve the existing exact invocation digest bytes"
    );
    for variation in ["jobs", "umask", "root", "environment", "tool"] {
        let mut changed = canonical.clone();
        match variation {
            "jobs" => changed.arguments[3] = "4".to_owned(),
            "umask" => {
                changed.umask = Umask::new(0o027).expect("should accept reproduction fixture umask")
            }
            "root" => changed.working_directory = PathBuf::from("/example/source-b"),
            "environment" => {
                changed
                    .environment
                    .insert("LANG".to_owned(), "en_US.UTF-8".to_owned());
            }
            "tool" => changed.program = PathBuf::from("/example/other-tools/bin/cargo"),
            _ => unreachable!(),
        }
        let observed =
            ExactInvocationDigest::from_plan(&changed).expect("should bind varied invocation");
        assert_ne!(
            exact, observed,
            "should retain actual {variation} differences in invocation identity"
        );
        assert_eq!(
            shared,
            RecipeDefinitionIdentity::from_source(&approved, changed.asset)
                .expect("should retain shared source-owned definition"),
            "should exclude observed {variation} from shared recipe definition"
        );
    }
}
