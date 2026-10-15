use std::fs;
use std::os::unix::fs::symlink;
use std::path::PathBuf;

use super::ambient::ObservedAmbient;
use super::*;
use serde_json::Value;

const COHORT: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../scripts/release/release-cohort.json"
));

#[test]
fn unknown_asset_id_is_rejected() {
    for name in [
        "",
        "ts-aarch64-apple-darwin ",
        "TS-AARCH64-APPLE-DARWIN",
        "../ts-aarch64-apple-darwin",
        "ts-riscv64-example-linux",
    ] {
        assert!(
            AssetId::parse(name).is_err_and(|error| format!("{error:?}").contains("unknown asset")),
            "should reject unknown asset id {name:?}"
        );
    }
}

#[test]
fn asset_selection_comes_from_fixed_table_not_json() {
    let cohort: Value = serde_json::from_str(COHORT).expect("should parse tracked cohort");
    for entry in cohort["artifacts"]
        .as_array()
        .expect("should list cohort artifacts")
    {
        let name = entry["name"].as_str().expect("should name artifact");
        let asset = AssetId::parse(name).expect("should accept every fixed asset");
        let fixed = FIXED_ARTIFACTS
            .iter()
            .find(|fixed| fixed.name == name)
            .expect("should find the fixed table row");
        assert_eq!(
            asset.artifact().package,
            fixed.package,
            "should use table package"
        );
        assert_eq!(asset.artifact().bin, fixed.bin, "should use table binary");
        assert_eq!(
            asset.artifact().target,
            fixed.target,
            "should use table target"
        );
        let mut forged = entry.clone();
        forged["package"] = Value::from("example-forged-package");
        assert_eq!(
            AssetId::parse(forged["name"].as_str().expect("should keep forged name"))
                .expect("should still resolve")
                .artifact()
                .package,
            fixed.package,
            "should ignore JSON-supplied package"
        );
    }
}

#[test]
fn unknown_role_is_rejected() {
    assert_eq!(Role::parse("A").expect("should accept A"), Role::A);
    assert_eq!(Role::parse("B").expect("should accept B"), Role::B);
    for text in ["", "a", "b", "C", "AB", " A"] {
        assert!(
            Role::parse(text).is_err_and(|error| format!("{error:?}").contains("unknown role")),
            "should reject role {text:?}"
        );
    }
}

#[test]
fn non_default_variant_is_rejected() {
    check_variant("default").expect("should accept the default variant");
    for variant in ["", "Default", "minimal", "default "] {
        assert!(
            check_variant(variant).is_err_and(|error| format!("{error:?}").contains("variant")),
            "should reject variant {variant:?}"
        );
    }
}

/// A fictional on-disk layout in which every request root is individually valid.
struct Workspace {
    directory: tempfile::TempDir,
    root: PathBuf,
}

impl Workspace {
    fn new() -> Self {
        let directory = tempfile::tempdir().expect("should create workspace");
        let root = fs::canonicalize(directory.path()).expect("should canonicalize workspace");
        fs::create_dir_all(root.join("js/bundles")).expect("should create JS bundle root");
        fs::write(root.join("js/bundles/tsjs-core.js"), b"fictional module")
            .expect("should write JS module");
        fs::write(root.join("js/tsjs-inventory.json"), b"fictional inventory")
            .expect("should write JS inventory");
        fs::create_dir(root.join("materials")).expect("should create materials root");
        fs::create_dir(root.join("output")).expect("should create output root");
        Self { directory, root }
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.root.join(relative)
    }

    fn input<'a>(&'a self, paths: &'a [PathBuf; 5]) -> RecipeInput<'a> {
        RecipeInput {
            asset: "ts-aarch64-apple-darwin",
            variant: "default",
            role: "A",
            commit: COMMIT,
            js_dir: &paths[0],
            js_inventory: &paths[1],
            materials: &paths[2],
            output: &paths[3],
            target_dir: &paths[4],
        }
    }

    fn defaults(&self) -> [PathBuf; 5] {
        [
            self.path("js/bundles"),
            self.path("js/tsjs-inventory.json"),
            self.path("materials"),
            self.path("output"),
            self.path("target"),
        ]
    }
}

const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

fn parse_error(paths: &[PathBuf; 5], workspace: &Workspace) -> String {
    let error = match RecipeRequest::parse(&workspace.input(paths)) {
        Ok(_) => panic!("should reject the request"),
        Err(error) => error,
    };
    format!("{error:?}")
}

#[test]
fn relative_or_dotted_paths_are_rejected() {
    let workspace = Workspace::new();
    RecipeRequest::parse(&workspace.input(&workspace.defaults()))
        .expect("should accept the baseline layout");
    let bad_values = [
        PathBuf::from("relative/path"),
        workspace.path("output/../output"),
        workspace.path("./output"),
    ];
    for index in 0..5 {
        for bad in &bad_values {
            let mut paths = workspace.defaults();
            paths[index] = bad.clone();
            assert!(
                parse_error(&paths, &workspace).contains("absolute normal path"),
                "should reject non-normal path {bad:?} at field {index}"
            );
        }
    }
}

#[test]
fn existing_nonempty_output_root_is_rejected() {
    let workspace = Workspace::new();
    fs::write(workspace.path("output/leftover"), b"previous attempt")
        .expect("should write a leftover entry");
    assert!(
        parse_error(&workspace.defaults(), &workspace).contains("cannot be reused"),
        "should reject an output root with prior entries"
    );
    let mut absent = workspace.defaults();
    absent[3] = workspace.path("missing-output");
    assert!(
        parse_error(&absent, &workspace).contains("cannot be resolved"),
        "should reject an output root that does not exist"
    );
}

#[test]
fn symlinked_or_preexisting_materials_root_is_rejected() {
    let workspace = Workspace::new();
    fs::create_dir(workspace.path("real-materials")).expect("should create alias target");
    symlink(
        workspace.path("real-materials"),
        workspace.path("linked-materials"),
    )
    .expect("should create materials alias");
    let mut linked = workspace.defaults();
    linked[2] = workspace.path("linked-materials");
    assert!(
        parse_error(&linked, &workspace).contains("symlink"),
        "should reject a symlinked materials root"
    );
    fs::write(workspace.path("materials/cache"), b"prepared earlier")
        .expect("should populate materials");
    assert!(
        parse_error(&workspace.defaults(), &workspace).contains("cannot be reused"),
        "should reject a pre-populated materials root"
    );
}

#[test]
fn reused_target_directory_is_rejected() {
    let workspace = Workspace::new();
    fs::create_dir(workspace.path("target")).expect("should create a previous target");
    assert!(
        parse_error(&workspace.defaults(), &workspace).contains("cannot be reused"),
        "should reject a target directory that already exists"
    );
}

#[test]
fn overlapping_roots_are_rejected() {
    let workspace = Workspace::new();
    let cases: [(usize, &str); 4] = [
        (3, "materials"),
        (4, "output/target"),
        (4, "materials/target"),
        (4, "js/target"),
    ];
    for (index, relative) in cases {
        let mut paths = workspace.defaults();
        paths[index] = workspace.path(relative);
        assert!(
            parse_error(&paths, &workspace).contains("overlap"),
            "should reject {relative} overlapping another root"
        );
    }
}

fn observed_a() -> ObservedAmbient {
    ObservedAmbient {
        umask: 0o022,
        jobs: 4,
        host_locale: "C".to_owned(),
        host_timezone: "UTC".to_owned(),
        child_locale: "C".to_owned(),
        child_timezone: "UTC".to_owned(),
        execution_uid: 501,
        checkout_root: PathBuf::from("/example/release-a"),
        started_at_seconds: 1_700_000_000,
    }
}

fn observed_b() -> ObservedAmbient {
    ObservedAmbient {
        umask: 0o027,
        jobs: 2,
        host_locale: "en_US.UTF-8".to_owned(),
        host_timezone: "Pacific/Auckland".to_owned(),
        child_locale: "C".to_owned(),
        child_timezone: "UTC".to_owned(),
        execution_uid: 501,
        checkout_root: PathBuf::from("/example/release-b"),
        started_at_seconds: 1_700_000_900,
    }
}

fn checked_a() -> CheckedAmbient {
    CheckedAmbient::check(Role::A, &AmbientProfile::A, observed_a())
        .expect("should accept the baseline A ambient")
}

fn checked_b() -> CheckedAmbient {
    CheckedAmbient::check(
        Role::B,
        &AmbientProfile::alternate_for_tests(),
        observed_b(),
    )
    .expect("should accept the baseline B ambient")
}

mod ambient;
mod bind;

#[test]
fn symlinked_ancestor_of_target_parent_is_rejected() {
    let workspace = Workspace::new();
    fs::create_dir(workspace.path("real-parent")).expect("should create alias target");
    symlink(
        workspace.path("real-parent"),
        workspace.path("linked-parent"),
    )
    .expect("should create parent alias");
    fs::create_dir(workspace.path("real-parent/inner")).expect("should create inner parent");
    let mut paths = workspace.defaults();
    paths[4] = workspace.path("linked-parent/inner/target");
    assert!(
        parse_error(&paths, &workspace).contains("symlink"),
        "should reject a target below a symlinked ancestor"
    );
}
