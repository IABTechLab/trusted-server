use std::cell::Cell;
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::PathBuf;

use serde_json::json;
use sha2::Sha256;
use tempfile::TempDir;

use super::*;
use crate::composition::{
    BuildScriptObservation, CargoObservations, RustPackage, RustUnit, UnitContext,
};

#[test]
fn reviewed_native_wrapper_versions_are_present_in_the_source_lockfile() {
    let lock: toml::Value = toml::from_str(include_str!("../../../../../Cargo.lock"))
        .expect("should parse the source lockfile");
    let packages = lock["package"]
        .as_array()
        .expect("should hold locked packages");
    for name in ["aws-lc-sys", "ring", "libsqlite3-sys"] {
        let reviewed = reviewed_wrapper_version(name).expect("should review native wrapper");
        assert!(
            packages.iter().any(|package| {
                package["name"].as_str() == Some(name)
                    && package["version"].as_str() == Some(reviewed)
            }),
            "should retain a source-locked version matching reviewed native wrapper {name}"
        );
    }
}

struct Fixture {
    _temporary: TempDir,
    root: PathBuf,
    observations: CargoObservations,
    origins: BTreeMap<String, NativeSource>,
    environment: BTreeMap<String, String>,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let temporary = tempfile::tempdir().expect("should create native fixture");
        let root = fs::canonicalize(temporary.path()).expect("should resolve fixture root");
        let (version, features, libraries, directories): (&str, &[&str], &[&str], &[&str]) =
            match name {
                "aws-lc-sys" => (
                    "0.42.0",
                    &["all-bindings", "default"],
                    &["aws_lc_0_42_0_crypto"],
                    &["aws-lc"],
                ),
                "ring" => (
                    "0.17.14",
                    &["alloc", "default", "dev_urandom_fallback"],
                    &["ring_core_0_17_14_", "ring_core_0_17_14__test"],
                    &["crypto", "include", "pregenerated"],
                ),
                "libsqlite3-sys" => (
                    "0.38.2",
                    &[
                        "bundled",
                        "bundled_bindings",
                        "cc",
                        "default",
                        "min_sqlite_version_3_34_1",
                        "pkg-config",
                        "vcpkg",
                    ],
                    &["sqlite3"],
                    &["sqlite3"],
                ),
                _ => panic!("should use a reviewed fixture package"),
            };
        let id = format!("registry+https://example.com/index#{name}@{version}");
        let package_root = root.join("vendor").join(name);
        fs::create_dir_all(&package_root).expect("should create source root");
        fs::write(package_root.join("Cargo.toml"), "fictional manifest")
            .expect("should write manifest");
        for directory in directories {
            let path = package_root.join(directory);
            fs::create_dir(&path).expect("should create native source subtree");
            fs::write(path.join("source.c"), "fictional source").expect("should write source");
        }
        let output = root.join("target/aarch64-apple-darwin/release/build/native/out");
        fs::create_dir_all(&output).expect("should create OUT_DIR");
        for library in libraries {
            fs::write(
                output.join(format!("lib{library}.a")),
                b"!<arch>\nfictional archive",
            )
            .expect("should write native archive");
        }
        let features: Vec<_> = features.iter().map(|value| (*value).to_owned()).collect();
        let observations = CargoObservations {
            packages: vec![RustPackage {
                id: id.clone(),
                name: name.to_owned(),
                version: version.to_owned(),
                source: Some("registry+https://example.com/index".to_owned()),
                manifest_path: package_root.join("Cargo.toml"),
                license: None,
            }],
            units: vec![RustUnit {
                package_id: id.clone(),
                target_name: name.to_owned(),
                target_kind: vec!["lib".to_owned()],
                crate_types: vec!["lib".to_owned()],
                context: UnitContext::Target,
                features,
                profile: json!({"test":false}),
                filenames: vec![],
                executable: None,
            }],
            build_scripts: vec![BuildScriptObservation {
                package_id: id.clone(),
                context: UnitContext::Target,
                out_dir: output.clone(),
                linked_libs: libraries
                    .iter()
                    .map(|library| format!("static={library}"))
                    .collect(),
                linked_paths: vec![format!("native={}", output.display())],
                cfgs: vec![],
                environment: vec![],
            }],
        };
        let origins = BTreeMap::from([(
            id,
            NativeSource {
                original_sha256: "a".repeat(64),
                source_root: package_root,
            },
        )]);
        let environment = BTreeMap::from([
            ("AWS_LC_SYS_USE_SYSTEM".to_owned(), "0".to_owned()),
            (
                "AWS_LC_SYS_USE_SYSTEM_aarch64_apple_darwin".to_owned(),
                "0".to_owned(),
            ),
            ("LIBSQLITE3_SYS_USE_PKG_CONFIG".to_owned(), "0".to_owned()),
        ]);
        Self {
            _temporary: temporary,
            root,
            observations,
            origins,
            environment,
        }
    }

    fn collect(&self) -> Result<NativeInventory, Report<ReleaseError>> {
        collect(
            &self.observations,
            &NativeInputs {
                origins: &self.origins,
                execution_output_root: &self.root.join("target"),
                host_output_root: &self.root.join("target"),
                target: ReleaseTarget::Aarch64AppleDarwin,
                environment: &self.environment,
                check: &|| Ok(()),
            },
        )
    }

    fn archive(&self) -> PathBuf {
        self.observations.build_scripts[0].out_dir.join(
            match self.observations.packages[0].name.as_str() {
                "aws-lc-sys" => "libaws_lc_0_42_0_crypto.a",
                "ring" => "libring_core_0_17_14_.a",
                _ => "libsqlite3.a",
            },
        )
    }
}

#[test]
fn reviewed_aws_lc_bindings_profiles_keep_verified_static_archive_evidence() {
    for features in [
        vec![],
        vec!["prebuilt-nasm"],
        vec!["all-bindings", "default"],
    ] {
        let mut fixture = Fixture::new("aws-lc-sys");
        fixture.observations.units[0].features = features
            .iter()
            .map(|feature| (*feature).to_owned())
            .collect();
        let evidence = fixture
            .collect()
            .expect("should accept reviewed bindings profiles for supported Unix targets");
        assert_eq!(
            evidence.contributors[0].features, fixture.observations.units[0].features,
            "should preserve actual effective features without enabling extra bindings"
        );
        assert_eq!(
            evidence.contributors[0].linked_libs,
            ["static=aws_lc_0_42_0_crypto"],
            "should preserve the verified static crypto archive requirement"
        );
        assert_eq!(
            evidence.contributors[0].archives.len(),
            1,
            "should retain the measured native archive"
        );
    }
}

#[test]
fn native_feature_errors_identify_package_context_and_bounded_observed_features() {
    let mut fixture = Fixture::new("aws-lc-sys");
    fixture.observations.units[0]
        .features
        .push("fips".to_owned());
    let error = format!(
        "{:?}",
        fixture
            .collect()
            .expect_err("should reject unreviewed features")
    );
    assert!(
        error.contains("aws-lc-sys") && error.contains("fips") && error.contains("Target"),
        "should diagnose the exact package, context and rejected feature: {error}"
    );
    fixture.observations.units[0].features = vec!["x".repeat(8192); 128];
    let error = format!(
        "{:?}",
        fixture
            .collect()
            .expect_err("should reject excessive unknown feature text")
    );
    assert!(
        error.len() < 4096,
        "should bound native feature diagnostics"
    );
}

#[test]
fn archive_format_and_digest_cannot_come_from_different_inodes() {
    let temporary = tempfile::tempdir().expect("should create native archive fixture");
    let root = fs::canonicalize(temporary.path()).expect("should resolve fixture root");
    let path = root.join("libexample.a");
    fs::write(&path, b"!<arch>\nfictional archive").expect("should write valid archive header");
    let replaced = Cell::new(false);
    let check = || {
        if !replaced.replace(true) {
            let replacement = root.join("replacement");
            fs::write(&replacement, b"fictional non-archive replacement")
                .expect("should write replacement bytes");
            fs::rename(replacement, &path).expect("should replace archive after its format check");
        }
        Ok(())
    };
    assert!(
        archive_digest(&root, &path, &check).is_err(),
        "should reject format and digest observations from different files"
    );
    assert!(replaced.get(), "should exercise the replacement window");
}

#[test]
fn captures_actual_archives_and_verified_source_materials_for_all_reviewed_origins() {
    for name in ["aws-lc-sys", "ring", "libsqlite3-sys"] {
        let fixture = Fixture::new(name);
        let inventory = fixture
            .collect()
            .expect("should capture reviewed native contributors");
        let value = serde_json::to_value(inventory).expect("should serialize observations");
        let contributor = &value["contributors"][0];
        assert_eq!(
            contributor["original_sha256"],
            "a".repeat(64),
            "should bind verified original source"
        );
        assert_eq!(
            contributor["archives"][0]["sha256"],
            hex::encode(Sha256::digest(b"!<arch>\nfictional archive")),
            "should hash actual produced archive"
        );
        assert_eq!(
            contributor["classification"], "conservative_embedded_contributor",
            "should not assert exact linked members"
        );
        assert_eq!(
            value["material_native_coverage_complete"], false,
            "should remain candidate observations"
        );
        assert!(
            contributor["source_tree_sha256"]
                .as_str()
                .is_some_and(|value| value.len() == 64),
            "should bind the prepared source tree"
        );
        assert!(
            contributor["source_subtrees"]
                .as_array()
                .is_some_and(|value| !value.is_empty()),
            "should identify reviewed native subtrees"
        );
        assert_eq!(
            contributor["archives"]
                .as_array()
                .expect("should have archives")
                .len(),
            if name == "ring" { 2 } else { 1 },
            "should include both ring release archives"
        );
    }
}

#[test]
fn rejects_system_substitution_unknown_origins_features_and_missing_outputs() {
    let mut fixture = Fixture::new("aws-lc-sys");
    fixture
        .environment
        .insert("AWS_LC_SYS_USE_SYSTEM".to_owned(), "1".to_owned());
    assert!(
        fixture.collect().is_err(),
        "should reject AWS-LC system substitution"
    );
    let mut fixture = Fixture::new("libsqlite3-sys");
    fixture.observations.units[0]
        .features
        .push("loadable_extension".to_owned());
    assert!(
        fixture.collect().is_err(),
        "should reject unreviewed SQLite feature"
    );
    let mut fixture = Fixture::new("ring");
    fixture.origins.clear();
    assert!(
        fixture.collect().is_err(),
        "should require a verified origin join"
    );
    let fixture = Fixture::new("ring");
    fs::remove_file(fixture.archive()).expect("should remove output archive");
    assert!(
        fixture.collect().is_err(),
        "should require actual output archives"
    );
}

#[test]
fn rejects_archive_aliases_hardlinks_and_link_search_escapes() {
    let fixture = Fixture::new("ring");
    let archive = fixture.archive();
    let other = fixture.root.join("foreign.a");
    fs::rename(&archive, &other).expect("should move archive");
    symlink(&other, &archive).expect("should create alias");
    assert!(
        fixture.collect().is_err(),
        "should reject a leaf archive alias"
    );
    fs::remove_file(&archive).expect("should remove alias");
    fs::hard_link(&other, &archive).expect("should create hardlink");
    assert!(
        fixture.collect().is_err(),
        "should reject externally rewritable archive"
    );
    let mut fixture = Fixture::new("libsqlite3-sys");
    fixture.observations.build_scripts[0].linked_paths = vec!["native=/usr/lib".to_owned()];
    assert!(
        fixture.collect().is_err(),
        "should reject system search substitution"
    );
}

#[test]
fn rejects_unknown_native_scripts_and_missing_native_execution() {
    let mut fixture = Fixture::new("ring");
    fixture.observations.packages[0].name = "fictional-native".to_owned();
    assert!(
        fixture.collect().is_err(),
        "should reject an unknown native library origin"
    );
    let mut fixture = Fixture::new("ring");
    fixture.observations.build_scripts.clear();
    assert!(
        fixture.collect().is_err(),
        "should reject an observed native unit without captured output"
    );
}

#[test]
fn host_native_archives_are_build_tools_instead_of_target_contributors() {
    let mut fixture = Fixture::new("ring");
    let old = fixture.observations.build_scripts[0].out_dir.clone();
    let output = fixture.root.join("target/release/build/native/out");
    fs::create_dir_all(output.parent().expect("should have build directory"))
        .expect("should create host build directory");
    fs::rename(old, &output).expect("should move host outputs");
    fixture.observations.units[0].context = UnitContext::Host;
    fixture.observations.build_scripts[0].context = UnitContext::Host;
    fixture.observations.build_scripts[0].out_dir = output.clone();
    fixture.observations.build_scripts[0].linked_paths =
        vec![format!("native={}", output.display())];
    let value = serde_json::to_value(
        fixture
            .collect()
            .expect("should capture host native outputs"),
    )
    .expect("should serialize host observations");
    assert_eq!(
        value["contributors"][0]["classification"], "build_tool_native_contributor",
        "should distinguish host build tools from target contributors"
    );
    assert_eq!(
        value["build_tool_package_ids"][0], fixture.observations.packages[0].id,
        "should list observed host build tools"
    );
}

#[test]
fn rehash_rejects_archive_and_prepared_source_mutation() {
    let fixture = Fixture::new("ring");
    let inventory = fixture.collect().expect("should capture native outputs");
    inventory
        .rehash(&|| Ok(()))
        .expect("should rehash unchanged native evidence");
    fs::write(fixture.archive(), b"!<arch>\nchanged archive").expect("should alter archive");
    assert!(
        inventory.rehash(&|| Ok(())).is_err(),
        "should reject archive changed after capture"
    );

    let fixture = Fixture::new("libsqlite3-sys");
    let inventory = fixture.collect().expect("should capture SQLite source");
    let source = &fixture
        .origins
        .values()
        .next()
        .expect("should have origin")
        .source_root;
    fs::write(source.join("sqlite3/source.c"), "changed fictional source")
        .expect("should change native source");
    assert!(
        inventory.rehash(&|| Ok(())).is_err(),
        "should reject native source changed after capture"
    );
}

#[test]
fn maps_observed_execution_search_paths_to_host_outputs() {
    let mut fixture = Fixture::new("ring");
    let host_root = fixture.root.join("target");
    let execution_root = PathBuf::from("/boundary/build/target");
    let script = &mut fixture.observations.build_scripts[0];
    script.out_dir = execution_root.join(
        script
            .out_dir
            .strip_prefix(&host_root)
            .expect("should be below host root"),
    );
    script.linked_paths = vec![format!("native={}", script.out_dir.display())];
    let value = serde_json::to_value(
        collect(
            &fixture.observations,
            &NativeInputs {
                origins: &fixture.origins,
                execution_output_root: &execution_root,
                host_output_root: &host_root,
                target: ReleaseTarget::Aarch64AppleDarwin,
                environment: &fixture.environment,
                check: &|| Ok(()),
            },
        )
        .expect("should translate boundary namespace"),
    )
    .expect("should serialize boundary observations");
    assert!(
        value["contributors"][0]["archives"][0]["path"]
            .as_str()
            .expect("should have archive path")
            .starts_with("/boundary/build/target/"),
        "should preserve observed namespace rather than invent host observations"
    );
}

#[test]
fn rejects_ancestor_aliases_ambiguous_searches_and_cross_context_outputs() {
    let fixture = Fixture::new("ring");
    let output = fixture.observations.build_scripts[0].out_dir.clone();
    let relocated = fixture.root.join("relocated-out");
    fs::rename(&output, &relocated).expect("should move OUT_DIR");
    symlink(&relocated, &output).expect("should alias OUT_DIR");
    assert!(
        fixture.collect().is_err(),
        "should reject an ancestor alias"
    );

    let mut fixture = Fixture::new("ring");
    let second = fixture.observations.build_scripts[0]
        .out_dir
        .join("duplicate");
    fs::create_dir(&second).expect("should create duplicate search directory");
    fs::copy(fixture.archive(), second.join("libring_core_0_17_14_.a"))
        .expect("should duplicate archive candidate");
    fixture.observations.build_scripts[0]
        .linked_paths
        .push(format!("native={}", second.display()));
    assert!(
        fixture.collect().is_err(),
        "should reject ambiguous archive resolution"
    );

    let mut fixture = Fixture::new("ring");
    fixture.observations.build_scripts[0].context = UnitContext::Host;
    fixture.observations.units[0].context = UnitContext::Host;
    assert!(
        fixture.collect().is_err(),
        "should reject target outputs labeled host context"
    );
}

#[test]
fn rejects_unreviewed_native_controls_directives_versions_and_disabled_bundling() {
    for feature in ["fips", "bindgen", "ssl", "asan"] {
        let mut fixture = Fixture::new("aws-lc-sys");
        fixture.observations.units[0]
            .features
            .push(feature.to_owned());
        assert!(
            fixture.collect().is_err(),
            "should reject AWS-LC feature {feature}"
        );
    }
    let mut fixture = Fixture::new("aws-lc-sys");
    fixture.environment.insert(
        "AWS_LC_SYS_SYSTEM_DIR".to_owned(),
        "/example/system".to_owned(),
    );
    assert!(
        fixture.collect().is_err(),
        "should reject explicit AWS-LC system material"
    );
    let mut fixture = Fixture::new("libsqlite3-sys");
    fixture
        .environment
        .insert("LIBSQLITE3_SYS_USE_PKG_CONFIG".to_owned(), "1".to_owned());
    assert!(
        fixture.collect().is_err(),
        "should reject SQLite system discovery"
    );
    let mut fixture = Fixture::new("libsqlite3-sys");
    fixture.observations.units[0]
        .features
        .retain(|feature| feature != "bundled");
    assert!(
        fixture.collect().is_err(),
        "should require bundled SQLite source"
    );
    let mut fixture = Fixture::new("ring");
    fixture.observations.build_scripts[0].linked_libs[0] = "dylib=ring_core_0_17_14_".to_owned();
    assert!(
        fixture.collect().is_err(),
        "should reject dynamic native substitution"
    );
    let mut fixture = Fixture::new("ring");
    fixture.observations.packages[0].version = "0.17.15".to_owned();
    assert!(
        fixture.collect().is_err(),
        "should require review for changed native version"
    );
}

#[test]
fn rejects_invalid_material_archive_source_alias_and_missing_native_subtree() {
    let mut fixture = Fixture::new("ring");
    fixture
        .origins
        .values_mut()
        .next()
        .expect("should have origin")
        .original_sha256 = "unverified".to_owned();
    assert!(
        fixture.collect().is_err(),
        "should require a complete original SHA-256"
    );
    let fixture = Fixture::new("ring");
    fs::write(fixture.archive(), "fictional non-archive").expect("should replace output");
    assert!(
        fixture.collect().is_err(),
        "should reject a file without archive magic"
    );
    let mut fixture = Fixture::new("ring");
    let source = &mut fixture
        .origins
        .values_mut()
        .next()
        .expect("should have origin")
        .source_root;
    let alias = fixture.root.join("source-alias");
    symlink(&source, &alias).expect("should create source alias");
    *source = alias;
    assert!(
        fixture.collect().is_err(),
        "should reject an aliased prepared source root"
    );
    let fixture = Fixture::new("libsqlite3-sys");
    let source = &fixture
        .origins
        .values()
        .next()
        .expect("should have origin")
        .source_root;
    fs::remove_dir_all(source.join("sqlite3")).expect("should remove native source subtree");
    assert!(
        fixture.collect().is_err(),
        "should require the reviewed native source subtree"
    );
}

#[test]
fn checks_attempt_deadlines_during_collection_and_rehash() {
    let fixture = Fixture::new("ring");
    let inventory = fixture.collect().expect("should capture native outputs");
    assert!(
        inventory
            .rehash(&|| Err(composition_error("fictional expired attempt")))
            .is_err(),
        "should reject an expired handoff attempt"
    );
    let result = collect(
        &fixture.observations,
        &NativeInputs {
            origins: &fixture.origins,
            execution_output_root: &fixture.root.join("target"),
            host_output_root: &fixture.root.join("target"),
            target: ReleaseTarget::Aarch64AppleDarwin,
            environment: &fixture.environment,
            check: &|| Err(composition_error("fictional cancelled attempt")),
        },
    );
    assert!(result.is_err(), "should reject a cancelled capture attempt");
}

#[test]
fn native_spdx_join_rejects_changed_source_features_context_and_duplicate_contributors() {
    for mutation in [
        "source",
        "features",
        "context",
        "duplicate",
        "missing",
        "name",
        "version",
        "classification",
        "unknown",
    ] {
        let mut fixture = Fixture::new("ring");
        let mut inventory = fixture
            .collect()
            .expect("should collect checked native input");
        let origins = crate::composition::origin::VerifiedPackageOrigins::fixture_native_registry(
            &fixture.observations,
        );
        match mutation {
            "source" => inventory.contributors[0].original_sha256 = "b".repeat(64),
            "features" => inventory.contributors[0]
                .features
                .push("changed".to_owned()),
            "context" => inventory.contributors[0].context = UnitContext::Host,
            "duplicate" => inventory
                .contributors
                .push(inventory.contributors[0].clone()),
            "missing" => fixture.observations.packages.clear(),
            "name" => inventory.contributors[0].name = "other".to_owned(),
            "version" => inventory.contributors[0].version = "99.0.0".to_owned(),
            "classification" => inventory.contributors[0].classification = "unknown",
            "unknown" => inventory.contributors[0].package_id = "unknown opaque ID".to_owned(),
            _ => unreachable!("should select known mutation"),
        }
        assert!(
            crate::spdx::native::validate_for_tests(&inventory, &fixture.observations, &origins)
                .is_err(),
            "should reject native join mutation {mutation}"
        );
    }
}

pub(super) fn fixture_for_projection(
    name: &str,
    context: UnitContext,
) -> (NativeInventory, CargoObservations) {
    let mut fixture = Fixture::new(name);
    if context == UnitContext::Host {
        let output = fixture.root.join("target/release/build/native/out");
        fs::create_dir_all(&output).expect("should create host output");
        for entry in fs::read_dir(&fixture.observations.build_scripts[0].out_dir)
            .expect("should list archives")
        {
            let entry = entry.expect("should read archive entry");
            fs::copy(entry.path(), output.join(entry.file_name()))
                .expect("should copy host archive");
        }
        fixture.observations.units[0].context = context;
        fixture.observations.build_scripts[0].context = context;
        fixture.observations.build_scripts[0].out_dir = output.clone();
        fixture.observations.build_scripts[0].linked_paths =
            vec![format!("native={}", output.display())];
    }
    let inventory = fixture
        .collect()
        .expect("should collect owner-checked native fixture");
    (inventory, fixture.observations)
}

#[test]
fn native_spdx_requires_each_reviewed_consuming_context_and_unambiguous_features() {
    let mut fixture = Fixture::new("ring");
    let mut inventory = fixture.collect().expect("should collect native inputs");
    let origins = crate::composition::origin::VerifiedPackageOrigins::fixture_native_registry(
        &fixture.observations,
    );
    inventory.contributors.clear();
    assert!(
        crate::spdx::native::validate_for_tests(&inventory, &fixture.observations, &origins)
            .is_err(),
        "should reject missing measured receipt for observed native unit"
    );
    let inventory = fixture.collect().expect("should collect complete inputs");
    fixture.observations.units.push(RustUnit {
        package_id: fixture.observations.packages[0].id.clone(),
        target_name: "ring".to_owned(),
        target_kind: vec!["lib".to_owned()],
        crate_types: vec!["lib".to_owned()],
        context: UnitContext::Target,
        features: vec!["alloc".to_owned()],
        profile: json!({"test":false}),
        filenames: vec![],
        executable: None,
    });
    assert!(
        crate::spdx::native::validate_for_tests(&inventory, &fixture.observations, &origins)
            .is_err(),
        "should reject ambiguous effective features within one native context"
    );
}

#[test]
fn native_spdx_rejects_aliases_of_the_same_verified_original_context() {
    let mut fixture = Fixture::new("ring");
    let mut inventory = fixture.collect().expect("should collect native inputs");
    let mut alias = inventory.contributors[0].clone();
    alias.package_id = "relocated opaque alias".to_owned();
    fixture.observations.packages.push(RustPackage {
        id: alias.package_id.clone(),
        name: "ring".to_owned(),
        version: "0.17.14".to_owned(),
        source: Some("registry+https://example.com/index".to_owned()),
        manifest_path: fixture.observations.packages[0].manifest_path.clone(),
        license: None,
    });
    fixture.observations.units.push(RustUnit {
        package_id: alias.package_id.clone(),
        target_name: "ring".to_owned(),
        target_kind: vec!["lib".to_owned()],
        crate_types: vec!["lib".to_owned()],
        context: UnitContext::Target,
        features: alias.features.clone(),
        profile: json!({"test":false}),
        filenames: vec![],
        executable: None,
    });
    inventory.contributors.push(alias);
    let origins = crate::composition::origin::VerifiedPackageOrigins::fixture_native_registry(
        &fixture.observations,
    );
    assert!(
        crate::spdx::native::validate_for_tests(&inventory, &fixture.observations, &origins)
            .is_err(),
        "should reject ambiguous aliases of one stable original/context identity"
    );
}

#[test]
fn native_spdx_rejects_missing_or_conflicting_verified_package_facts() {
    for mutation in ["missing", "name", "version"] {
        let mut fixture = Fixture::new("ring");
        let inventory = fixture.collect().expect("should collect native inputs");
        match mutation {
            "missing" => fixture.observations.packages.clear(),
            "name" => fixture.observations.packages[0].name = "other".to_owned(),
            "version" => fixture.observations.packages[0].version = "99.0.0".to_owned(),
            _ => unreachable!("should select known mutation"),
        }
        let origins = crate::composition::origin::VerifiedPackageOrigins::fixture_native_registry(
            &fixture.observations,
        );
        let (_, observed) = fixture_for_projection("ring", UnitContext::Target);
        assert!(
            crate::spdx::native::validate_for_tests(&inventory, &observed, &origins).is_err(),
            "should reject verified facts with {mutation}"
        );
    }
}
