use std::fs;

use sha2::{Digest as _, Sha256};
use tempfile::TempDir;

use super::*;
use crate::composition::RustPackage;

struct Paths;
impl ExecutionPaths for Paths {
    fn execution_path(&self, host: &Path) -> Result<PathBuf, Report<ReleaseError>> {
        Ok(Path::new("/execution").join(host.strip_prefix("/").expect("should be absolute")))
    }
    fn host_path(&self, _: &Path) -> Result<PathBuf, Report<ReleaseError>> {
        panic!("should never reverse-map immutable inputs")
    }
}

fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

struct Fixture {
    _source: TempDir,
    _materials: TempDir,
    workspace: WorkspaceOrigins,
    materials: PreparedCargoMaterials,
    observed: CargoObservations,
}

impl Fixture {
    fn workspace() -> Self {
        let source = tempfile::tempdir().expect("should allocate source");
        let root = fs::canonicalize(source.path()).expect("should canonicalize source");
        let manifest = b"[package]\nname='example'\nversion='1.0.0'\n";
        fs::write(root.join("Cargo.toml"), manifest).expect("should write manifest");
        let materials = tempfile::tempdir().expect("should allocate materials");
        let material_root =
            fs::canonicalize(materials.path()).expect("should canonicalize materials");
        let token = PreparedCargoMaterials::for_tests(&material_root, &"a".repeat(40), &[])
            .expect("should prepare token");
        Self {
            observed: CargoObservations {
                packages: vec![RustPackage {
                    id: "opaque identity".to_owned(),
                    name: "example".to_owned(),
                    version: "1.0.0".to_owned(),
                    source: None,
                    manifest_path: Paths
                        .execution_path(&root.join("Cargo.toml"))
                        .expect("should translate manifest"),
                    license: None,
                }],
                units: vec![],
                build_scripts: vec![],
            },
            workspace: WorkspaceOrigins {
                verification: WorkspaceVerification::Fixture,
                root,
                source_commit: "a".repeat(40),
                cargo_lock_sha256: "b".repeat(64),
                manifests: BTreeMap::from([(PathBuf::from("Cargo.toml"), digest(manifest))]),
            },
            materials: token,
            _source: source,
            _materials: materials,
        }
    }
    fn join(&self) -> Result<VerifiedPackageOrigins, Report<ReleaseError>> {
        join_origins(&self.observed, &self.workspace, &self.materials, &Paths)
    }
}

#[test]
fn workspace_joins_exact_opaque_id_to_approved_measured_manifest() {
    let fixture = Fixture::workspace();
    let result = fixture
        .join()
        .expect("should join tracked workspace package");
    assert!(
        result.package("opaque identity").is_some(),
        "should retain exact opaque identity"
    );
    assert!(
        result.package("example").is_none(),
        "should refuse name-only lookups for an exact observed identity"
    );
    let package = result
        .package("opaque identity")
        .expect("should retain package origin");
    assert_eq!(package.name(), "example", "should retain checked name");
    assert_eq!(package.version(), "1.0.0", "should retain checked version");
    assert_eq!(
        package.relative_manifest_path(),
        Path::new("Cargo.toml"),
        "should retain relative manifest"
    );
    assert_eq!(
        package.prepared_manifest_sha256(),
        fixture.workspace.manifests[Path::new("Cargo.toml")],
        "should retain measured manifest identity"
    );
    assert!(
        matches!(package.origin(), Original::Workspace { source_commit, path } if source_commit == &"a".repeat(40) && path == Path::new("Cargo.toml")),
        "should retain approved original identity"
    );
    assert!(
        result.native_sources().is_empty(),
        "should not authorize workspace as registry archive"
    );
}

#[test]
fn workspace_rejects_unapproved_identity_path_and_changed_bytes() {
    for mutation in [
        "name",
        "version",
        "path",
        "bytes",
        "commit",
        "lock",
        "alias",
        "missing",
        "untracked",
    ] {
        let mut fixture = Fixture::workspace();
        match mutation {
            "name" => fixture.observed.packages[0].name = "other".to_owned(),
            "version" => fixture.observed.packages[0].version = "9.0.0".to_owned(),
            "path" => {
                fixture.observed.packages[0].manifest_path = PathBuf::from("/unapproved/Cargo.toml")
            }
            "bytes" => fs::write(fixture.workspace.root.join("Cargo.toml"), b"changed")
                .expect("should mutate bytes"),
            "commit" => fixture.workspace.source_commit = "c".repeat(40),
            "lock" => fixture.workspace.cargo_lock_sha256 = "c".repeat(64),
            "alias" => {
                fs::rename(
                    fixture.workspace.root.join("Cargo.toml"),
                    fixture.workspace.root.join("original"),
                )
                .expect("should move original");
                std::os::unix::fs::symlink("original", fixture.workspace.root.join("Cargo.toml"))
                    .expect("should alias manifest");
            }
            "missing" => fs::remove_file(fixture.workspace.root.join("Cargo.toml"))
                .expect("should remove approved manifest"),
            "untracked" => {
                fs::create_dir(fixture.workspace.root.join("untracked"))
                    .expect("should create untracked dependency");
                fs::copy(
                    fixture.workspace.root.join("Cargo.toml"),
                    fixture.workspace.root.join("untracked/Cargo.toml"),
                )
                .expect("should write unapproved matching identity");
                fixture.observed.packages[0].manifest_path = Paths
                    .execution_path(&fixture.workspace.root.join("untracked/Cargo.toml"))
                    .expect("should translate unapproved dependency");
            }
            _ => unreachable!(),
        }
        assert!(
            fixture.join().is_err(),
            "should reject {mutation} substitution"
        );
    }
}

#[test]
fn registry_requires_exact_source_path_name_version_and_manifest() {
    for mutation in [
        "none",
        "name",
        "version",
        "source",
        "path",
        "bytes",
        "ambiguous",
    ] {
        let mut fixture = Fixture::workspace();
        let manifest = b"[package]\nname='example'\nversion='1.0.0'\n";
        let root = fixture.materials.vendor_dir().join("example-1.0.0");
        fs::create_dir(&root).expect("should create package root");
        fs::write(root.join("Cargo.toml"), manifest).expect("should write package manifest");
        let token =
            PreparedCargoMaterials::for_tests(fixture.materials.root(), &"a".repeat(40), &[])
                .expect("should measure vendor");
        let package = crate::materials::VerifiedCargoPackage::registry_for_tests(
            "example",
            "1.0.0",
            "registry+https://example.com/index",
            &digest(manifest),
            &"d".repeat(64),
        );
        fixture.materials = token.with_package_for_tests("example-1.0.0", package.clone());
        fixture.observed.packages[0].source = Some(package.source().to_owned());
        fixture.observed.packages[0].manifest_path = Paths
            .execution_path(&root.join("Cargo.toml"))
            .expect("should translate vendor manifest");
        match mutation {
            "name" => fixture.observed.packages[0].name = "other".to_owned(),
            "version" => fixture.observed.packages[0].version = "9.0.0".to_owned(),
            "source" => {
                fixture.observed.packages[0].source =
                    Some("git+https://example.com/repository#abc".to_owned())
            }
            "path" => {
                fixture.observed.packages[0].manifest_path = fixture.observed.packages[0]
                    .manifest_path
                    .with_file_name("Other.toml")
            }
            "bytes" => {
                fs::write(root.join("Cargo.toml"), b"changed").expect("should mutate vendor")
            }
            "ambiguous" => fixture.observed.packages.push(RustPackage {
                id: "different opaque ID".to_owned(),
                name: "example".to_owned(),
                version: "1.0.0".to_owned(),
                source: fixture.observed.packages[0].source.clone(),
                manifest_path: fixture.observed.packages[0].manifest_path.clone(),
                license: None,
            }),
            _ => {}
        }
        let result = fixture.join();
        if mutation == "none" {
            let origins = result.expect("should join exact registry package");
            assert_eq!(
                origins.native_sources()["opaque identity"].original_sha256,
                "d".repeat(64),
                "should authorize verified archive only"
            );
            let serialized = serde_json::to_string(&origins).expect("should serialize origins");
            assert!(
                !serialized.contains(root.to_str().expect("should have UTF-8 fixture root")),
                "should exclude host root"
            );
        } else {
            assert!(result.is_err(), "should reject {mutation} substitution");
        }
    }
}

#[test]
fn root_workspace_version_inheritance_requires_approved_root_bytes() {
    for mutation in ["none", "root_bytes", "version", "nested", "redirect"] {
        let mut fixture = Fixture::workspace();
        let root = b"[workspace]\nmembers=['member']\n[workspace.package]\nversion='1.0.0'\n";
        let member = if mutation == "redirect" {
            b"[package]\nname='example'\nversion.workspace=true\nworkspace='../elsewhere'\n"
                .as_slice()
        } else {
            b"[package]\nname='example'\nversion.workspace=true\n".as_slice()
        };
        fs::write(fixture.workspace.root.join("Cargo.toml"), root)
            .expect("should write workspace root");
        fs::create_dir(fixture.workspace.root.join("member")).expect("should create member");
        fs::write(fixture.workspace.root.join("member/Cargo.toml"), member)
            .expect("should write member");
        fixture.workspace.manifests = BTreeMap::from([
            (PathBuf::from("Cargo.toml"), digest(root)),
            (PathBuf::from("member/Cargo.toml"), digest(member)),
        ]);
        fixture.observed.packages[0].manifest_path = Paths
            .execution_path(&fixture.workspace.root.join("member/Cargo.toml"))
            .expect("should translate member");
        match mutation {
            "root_bytes" => fs::write(
                fixture.workspace.root.join("Cargo.toml"),
                b"[workspace.package]\nversion='9.0.0'\n",
            )
            .expect("should replace root"),
            "version" => fixture.observed.packages[0].version = "9.0.0".to_owned(),
            "nested" => {
                let nested = b"[workspace]\n[workspace.package]\nversion='1.0.0'\n";
                fs::create_dir(fixture.workspace.root.join("member/nested"))
                    .expect("should create nested member");
                fs::write(fixture.workspace.root.join("member/Cargo.toml"), nested)
                    .expect("should write nested workspace");
                fixture
                    .workspace
                    .manifests
                    .insert(PathBuf::from("member/Cargo.toml"), digest(nested));
                fs::write(
                    fixture.workspace.root.join("member/nested/Cargo.toml"),
                    member,
                )
                .expect("should write nested package");
                fixture
                    .workspace
                    .manifests
                    .insert(PathBuf::from("member/nested/Cargo.toml"), digest(member));
                fixture.observed.packages[0].manifest_path = Paths
                    .execution_path(&fixture.workspace.root.join("member/nested/Cargo.toml"))
                    .expect("should translate nested member");
            }
            _ => {}
        }
        assert_eq!(
            fixture.join().is_ok(),
            mutation == "none",
            "should enforce reviewed inheritance for {mutation}"
        );
    }
}

#[test]
fn git_join_preserves_exact_revision_trees_and_normalization_without_native_archive_authority() {
    for mutation in ["none", "revision", "manifest", "source_null"] {
        let mut fixture = Fixture::workspace();
        let manifest = b"[package]\nname='example'\nversion='1.0.0'\n";
        let root = fixture.materials.vendor_dir().join("example");
        fs::create_dir(&root).expect("should create Git package");
        fs::write(root.join("Cargo.toml"), manifest).expect("should write normalized manifest");
        let source = format!(
            "git+https://example.com/repository?rev=reviewed#{}",
            "e".repeat(40)
        );
        let original = CargoOrigin::Git {
            revision: "e".repeat(40),
            repository_tree: "f".repeat(40),
            package_tree: "1".repeat(40),
            package_path: "member".to_owned(),
            committed_manifest_blob: "2".repeat(40),
            committed_manifest_sha256: "3".repeat(64),
            normalized_manifest_sha256: Some(digest(manifest)),
        };
        let package = crate::materials::VerifiedCargoPackage::registry_for_tests(
            "example",
            "1.0.0",
            &source,
            &digest(manifest),
            &"d".repeat(64),
        )
        .with_origin_for_tests(original);
        let unused = package.clone();
        fs::create_dir(fixture.materials.vendor_dir().join("unused"))
            .expect("should create unused prepared package");
        fs::write(
            fixture.materials.vendor_dir().join("unused/Cargo.toml"),
            manifest,
        )
        .expect("should write unused prepared manifest");
        fixture.materials =
            PreparedCargoMaterials::for_tests(fixture.materials.root(), &"a".repeat(40), &[])
                .expect("should measure Git vendor")
                .with_package_for_tests("example", package)
                .with_package_for_tests("unused", unused);
        fixture.observed.packages[0].manifest_path = Paths
            .execution_path(&root.join("Cargo.toml"))
            .expect("should translate Git manifest");
        fixture.observed.packages[0].source = Some(source);
        match mutation {
            "revision" => {
                fixture.observed.packages[0].source = Some(format!(
                    "git+https://example.com/repository?rev=reviewed#{}",
                    "9".repeat(40)
                ))
            }
            "manifest" => {
                fixture.observed.packages[0].manifest_path = Paths
                    .execution_path(&fixture.workspace.root.join("Cargo.toml"))
                    .expect("should translate wrong manifest")
            }
            "source_null" => fixture.observed.packages[0].source = None,
            _ => {}
        }
        if mutation == "none" {
            let result = fixture.join().expect("should join exact Git identity");
            assert_eq!(
                result.packages.len(),
                1,
                "should exclude prepared unused package"
            );
            assert!(
                result.native_sources().is_empty(),
                "should not treat Git tree as original registry archive"
            );
            let serialized = serde_json::to_value(result).expect("should serialize exact origin");
            assert_eq!(
                serialized["packages"]["opaque identity"]["origin"]["original"]["revision"],
                "e".repeat(40),
                "should retain exact revision"
            );
            assert_eq!(
                serialized["packages"]["opaque identity"]["origin"]["original"]["normalized_manifest_sha256"],
                digest(manifest),
                "should retain normalization digest"
            );
        } else {
            assert!(
                fixture.join().is_err(),
                "should reject {mutation} substitution"
            );
        }
    }
}
