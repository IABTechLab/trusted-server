use super::*;

fn owner() -> Owner {
    Owner {
        source_commit: crate::source::CommitId::parse(&"a".repeat(40))
            .expect("should parse source"),
        role: crate::recipe::Role::Canonical,
        asset: crate::recipe::AssetId::parse("ts-x86_64-unknown-linux-gnu")
            .expect("should resolve CLI"),
        artifact_sha256: "b".repeat(64),
        artifact_size: 128,
        package_version: "9.8.7".to_owned(),
    }
}

#[test]
fn startup_expected_output_uses_the_sealed_cli_version() {
    let owner = owner();
    assert_eq!(
        owner.expected_stdout(),
        format!(
            "ts package=9.8.7 source={} target=x86_64-unknown-linux-gnu variant=default\n",
            "a".repeat(40)
        ),
        "should derive output from owning package facts rather than producer version"
    );
}

#[test]
fn linux_startup_requires_three_exact_distinct_selections() {
    let images: Vec<_> = crate::recipe::startup::StartupDistribution::ALL
        .iter()
        .enumerate()
        .map(|(index, distribution)| {
            crate::recipe::startup::StartupImage::new(
                *distribution,
                &format!("sha256:{}", (index + 1).to_string().repeat(64)),
            )
            .expect("should create selected image")
        })
        .collect();
    check_linux_images(&images).expect("should accept the complete reviewed startup set");
    assert!(
        check_linux_images(&images[..2]).is_err(),
        "should reject a missing distribution"
    );
    let duplicates = vec![images[0].clone(), images[0].clone(), images[2].clone()];
    assert!(
        check_linux_images(&duplicates).is_err(),
        "should reject repeated distribution"
    );
    let wrong_id = vec![
        images[0].clone(),
        crate::recipe::startup::StartupImage::new(
            crate::recipe::startup::StartupDistribution::Ubuntu2204,
            images[0].id(),
        )
        .expect("should select local image"),
        images[2].clone(),
    ];
    assert!(
        check_linux_images(&wrong_id).is_err(),
        "should not reuse one image as multiple distributions"
    );
}

#[test]
fn native_startup_owner_rejects_another_source_role_asset_artifact_or_cli_version() {
    let original = owner();
    original
        .check_same(&owner())
        .expect("should accept exact owner binding");
    for field in ["source", "role", "asset", "digest", "size", "version"] {
        let mut other = owner();
        match field {
            "source" => {
                other.source_commit =
                    crate::source::CommitId::parse(&"c".repeat(40)).expect("should parse source")
            }
            "role" => other.role = crate::recipe::Role::Reproduction,
            "asset" => {
                other.asset = crate::recipe::AssetId::parse("ts-aarch64-apple-darwin")
                    .expect("should resolve other asset")
            }
            "digest" => other.artifact_sha256 = "c".repeat(64),
            "size" => other.artifact_size += 1,
            "version" => other.package_version = "1.2.3".to_owned(),
            _ => unreachable!("should select fixture field"),
        }
        assert!(
            original.check_same(&other).is_err(),
            "should reject different {field} owner"
        );
    }
}

#[test]
fn cli_version_requires_the_exact_selected_approved_workspace_original() {
    let owner = owner();
    let original = crate::composition::origin::Original::Workspace {
        source_commit: owner.source_commit.as_str().to_owned(),
        path: "crates/trusted-server-cli/Cargo.toml".into(),
    };
    let valid = CliVersionInputs {
        name: "trusted-server-cli",
        version: "9.8.7",
        origin_name: "trusted-server-cli",
        origin_version: "9.8.7",
        original: &original,
        commit: &owner.source_commit,
    };
    assert_eq!(
        checked_cli_version(&valid).expect("should use verified selected CLI version"),
        "9.8.7",
        "should retain source-owned CLI package version"
    );
    for original in [
        Original::Workspace {
            source_commit: "f".repeat(40),
            path: "crates/trusted-server-cli/Cargo.toml".into(),
        },
        Original::Workspace {
            source_commit: owner.source_commit.as_str().to_owned(),
            path: "crates/fictional-cli/Cargo.toml".into(),
        },
    ] {
        assert!(
            checked_cli_version(&CliVersionInputs {
                original: &original,
                ..valid
            })
            .is_err(),
            "should reject another approved workspace source or package path"
        );
    }
    assert!(
        checked_cli_version(&CliVersionInputs {
            origin_version: "1.0.0",
            ..valid
        })
        .is_err(),
        "should reject metadata version differing from verified source"
    );
    assert!(
        checked_cli_version(&CliVersionInputs {
            version: "9.8.7\nforged",
            origin_version: "9.8.7\nforged",
            ..valid
        })
        .is_err(),
        "should reject version output injection"
    );
}

#[test]
fn retained_linux_diagnostic_owns_all_run_facts_and_allows_explicit_optional_b() {
    let mut owner = owner();
    owner.role = crate::recipe::Role::Reproduction;
    let runs = StartupDistribution::ALL
        .iter()
        .enumerate()
        .map(|(index, distribution)| StartupObserved {
            image: StartupImage::new(
                *distribution,
                &format!("sha256:{}", (index + 1).to_string().repeat(64)),
            )
            .expect("should select image"),
            stdout: owner.expected_stdout(),
            stderr_sha256: "d".repeat(64),
            elapsed: std::time::Duration::from_millis(12),
        })
        .collect();
    let accepted = NativeStartupAccepted {
        owner,
        observed: Observed::Linux(runs),
    };
    let record = accepted
        .diagnostic_record()
        .expect("should retain owned run facts without startup working roots");
    assert_eq!(
        record["role"], "B",
        "should preserve an actually observed optional reproduction startup"
    );
    assert_eq!(
        record["package_version"], "9.8.7",
        "should retain actual selected CLI version"
    );
    assert_eq!(
        record["startup"]["runs"]
            .as_array()
            .expect("should retain startup runs")
            .len(),
        3,
        "should retain all reviewed clean-image observations"
    );
    assert_eq!(
        record["startup"]["runs"][0]["elapsed_milliseconds"], 12,
        "should retain integer bounded duration"
    );
    assert_eq!(
        record["startup"]["runs"][0]["image_id"],
        format!("sha256:{}", "1".repeat(64)),
        "should bind actual selected local image identity"
    );
    assert_eq!(
        record["release_qualification"], false,
        "should not infer qualification from retained observations"
    );
}
