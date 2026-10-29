use std::fs;
use std::os::unix::fs::{PermissionsExt as _, symlink};

use super::*;

/// Writes an image file with the given permission bits.
fn image_file(root: &std::path::Path, relative: &str, mode: u32) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().expect("should have a parent"))
        .expect("should create image directory");
    fs::write(&path, b"privileged").expect("should write image file");
    fs::set_permissions(&path, fs::Permissions::from_mode(mode)).expect("should set mode");
}

/// Reports whether mode bits deny this process; root bypasses them.
fn mode_bits_apply() -> bool {
    // SAFETY: `geteuid` has no preconditions.
    unsafe { libc::geteuid() != 0 }
}

#[test]
fn only_reviewed_unreadable_image_files_are_recorded_by_metadata() {
    if !mode_bits_apply() {
        return;
    }
    let temporary = tempfile::tempdir().expect("should create image fixture");
    let root = temporary.path();
    // SAFETY: `geteuid` has no preconditions.
    let owner_uid = unsafe { libc::geteuid() };
    let paths = ["/usr/sbin/userhelper"];
    let policy = image::UnreadablePolicy {
        paths: &paths,
        owner_uid,
    };
    image_file(root, "usr/sbin/userhelper", 0o111);
    let first = image::installed_snapshot_with(root, &policy)
        .expect("should record the reviewed helper by metadata");
    assert_eq!(
        first.content_unmeasured,
        vec!["/usr/sbin/userhelper".to_owned()],
        "should name the file whose content was not measured"
    );
    fs::set_permissions(
        root.join("usr/sbin/userhelper"),
        fs::Permissions::from_mode(0o100),
    )
    .expect("should change helper mode");
    assert_ne!(
        first.sha256,
        image::installed_snapshot_with(root, &policy)
            .expect("should remeasure")
            .sha256,
        "should bind the unmeasured file's mode"
    );
    fs::set_permissions(
        root.join("usr/sbin/userhelper"),
        fs::Permissions::from_mode(0o121),
    )
    .expect("should add a group write bit");
    assert!(
        image::installed_snapshot_with(root, &policy).is_err(),
        "should reject a group-writable reviewed helper"
    );
    fs::set_permissions(
        root.join("usr/sbin/userhelper"),
        fs::Permissions::from_mode(0o111),
    )
    .expect("should restore helper mode");
    let other_owner = image::UnreadablePolicy {
        paths: &paths,
        owner_uid: owner_uid.wrapping_add(1),
    };
    assert!(
        image::installed_snapshot_with(root, &other_owner).is_err(),
        "should reject a reviewed path with another owner"
    );
    image_file(root, "usr/libexec/gcc/example/lto-wrapper", 0o111);
    let rejected = image::installed_snapshot_with(root, &policy)
        .expect_err("should reject an unreviewed execute-only helper");
    assert!(
        format!("{rejected:?}").contains("/usr/libexec/gcc/example/lto-wrapper"),
        "should name the rejected file: {rejected:?}"
    );
}

#[test]
fn production_policy_reviews_only_the_known_root_owned_helpers() {
    assert_eq!(
        image::PRODUCTION_UNREADABLE.paths,
        image::REVIEWED_UNREADABLE.as_slice(),
        "should use exactly the reviewed paths"
    );
    assert_eq!(
        image::PRODUCTION_UNREADABLE.owner_uid,
        0,
        "should require root ownership"
    );
    assert!(
        image::REVIEWED_UNREADABLE
            .windows(2)
            .all(|pair| pair[0] < pair[1]),
        "should keep the reviewed paths sorted"
    );
}

#[test]
fn content_unmeasured_lists_must_be_sorted_reviewed_paths() {
    assert!(
        valid_content_unmeasured(&[]),
        "should accept no unmeasured files"
    );
    assert!(
        valid_content_unmeasured(&[
            "/usr/libexec/utempter/utempter".to_owned(),
            "/usr/sbin/unix_update".to_owned(),
            "/usr/sbin/userhelper".to_owned(),
        ]),
        "should accept the sorted reviewed paths"
    );
    for files in [
        vec![
            "/usr/sbin/userhelper".to_owned(),
            "/usr/sbin/unix_update".to_owned(),
        ],
        vec![
            "/usr/sbin/userhelper".to_owned(),
            "/usr/sbin/userhelper".to_owned(),
        ],
        vec!["/usr/libexec/gcc/example/lto-wrapper".to_owned()],
        vec!["/usr//sbin/userhelper".to_owned()],
    ] {
        assert!(
            !valid_content_unmeasured(&files),
            "should reject unsorted, duplicate or unreviewed lists {files:?}"
        );
    }
}

#[test]
fn selected_tools_must_be_readable_even_when_executable() {
    if !mode_bits_apply() {
        return;
    }
    let temporary = tempfile::tempdir().expect("should create image fixture");
    let root = temporary.path();
    image_file(root, "usr/bin/gcc", 0o111);
    assert!(
        file_identity_at(root, std::path::Path::new("/usr/bin/gcc")).is_err(),
        "should refuse to select an execute-only compiler without hashing it"
    );
}

#[test]
fn compiler_default_sysroot_can_have_no_output() {
    for output in ["", "\n", "/\n"] {
        assert!(
            check_image_sysroot(output).is_ok(),
            "should accept GCC's default or explicit image-root sysroot response"
        );
    }
    for output in ["/other\n", "relative\n", " /\n", "/\r\n", "\n\n"] {
        assert!(
            check_image_sysroot(output).is_err(),
            "should reject an unreviewed sysroot response {output:?}"
        );
    }
}

pub(super) fn projection_fixture() -> GnuEvidence {
    GnuEvidence {
        measurement: Measurement {
            profile: PROFILE.to_owned(),
            installed_files_sha256: "a".repeat(64),
            content_unmeasured_files: Vec::new(),
            roots: image::ROOTS.iter().map(|root| (*root).to_owned()).collect(),
            selected_files: [
                ("compiler", "/usr/bin/gcc"),
                ("archiver", "/usr/bin/ar"),
                ("gcc_linker_candidate", "/usr/bin/ld"),
                ("gcc_assembler_candidate", "/usr/bin/as"),
                ("configured_cc1", "/usr/libexec/gcc/example/cc1"),
                ("configured_collect2", "/usr/libexec/gcc/example/collect2"),
                ("configured_as", "/usr/bin/as"),
                ("configured_ld", "/usr/bin/ld"),
                ("compiler_runtime", "/usr/lib/gcc/example/libgcc.a"),
            ]
            .into_iter()
            .map(|(role, path)| {
                (
                    role.to_owned(),
                    SelectedFile {
                        selected: path.into(),
                        resolved: path.into(),
                        sha256: "c".repeat(64),
                        size: 128,
                    },
                )
            })
            .collect(),
            compiler_queries: BTreeMap::new(),
            sysroot: "/".to_owned(),
        },
        profile_sha256: profile_digest(),
        helper_sha256: "d".repeat(64),
        build_boundary_fingerprint: hex::encode([1; 32]),
        measurement_boundary_fingerprint: hex::encode([3; 32]),
        plan_sha256: "b".repeat(64),
        identity_scope: "fictional_projection_fixture",
        acquisition: "unqualified_candidate_measurement",
        full_closure_measured: false,
    }
}

fn measurement_fixture() -> Measurement {
    let mut measurement = projection_fixture().measurement;
    measurement.compiler_queries = [
        ("-print-prog-name=cc1", "/usr/libexec/gcc/example/cc1\n"),
        ("-print-prog-name=collect2", "/usr/libexec/gcc/example/collect2\n"),
        ("-print-prog-name=as", "as\n"),
        ("-print-prog-name=ld", "ld\n"),
        ("-print-libgcc-file-name", "/usr/lib/gcc/example/libgcc.a\n"),
        ("-print-file-name=include", "/usr/lib/gcc/example/include\n"),
        ("-print-sysroot", ""),
        ("-print-search-dirs", "install: /usr/lib/gcc/example/\nprograms: =/usr/libexec/gcc/example/\nlibraries: =/usr/lib/gcc/example/\n"),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_owned(), value.to_owned()))
    .collect();
    measurement
}

#[test]
fn measurement_requires_exact_selected_roles_and_query_keys() {
    for selected in [true, false] {
        let mut measurement = measurement_fixture();
        if selected {
            let value = measurement
                .selected_files
                .remove("compiler")
                .expect("should have compiler");
            measurement
                .selected_files
                .insert("example_replacement".to_owned(), value);
        } else {
            let value = measurement
                .compiler_queries
                .remove("-print-sysroot")
                .expect("should have sysroot query");
            measurement
                .compiler_queries
                .insert("example_replacement".to_owned(), value);
        }
        assert!(
            validate_measurement(&measurement).is_err(),
            "should reject a missing required identity even when counts match"
        );
    }
}

#[test]
fn measurement_binds_selected_drivers_and_configured_query_paths() {
    for role in [
        "compiler",
        "archiver",
        "gcc_linker_candidate",
        "gcc_assembler_candidate",
        "configured_cc1",
        "configured_collect2",
        "configured_as",
        "configured_ld",
        "compiler_runtime",
    ] {
        let mut measurement = measurement_fixture();
        measurement
            .selected_files
            .get_mut(role)
            .expect("should have selected role")
            .selected = PathBuf::from("/usr/bin/example-replacement");
        assert!(
            validate_measurement(&measurement).is_err(),
            "should join selected {role} to its fixed driver or compiler query"
        );
    }
}

#[test]
fn measurement_rechecks_the_compiler_sysroot_observation() {
    let mut measurement = measurement_fixture();
    measurement.compiler_queries.insert(
        "-print-sysroot".to_owned(),
        "/example-other-root\n".to_owned(),
    );
    assert!(
        validate_measurement(&measurement).is_err(),
        "should reject a query contradicting the recorded image-root sysroot"
    );
}

#[test]
fn measurement_preserves_reviewed_bare_names_and_resolved_aliases() {
    for sysroot in ["", "\n", "/\n"] {
        for explicit_paths in [false, true] {
            let mut measurement = measurement_fixture();
            measurement
                .compiler_queries
                .insert("-print-sysroot".to_owned(), sysroot.to_owned());
            for name in ["as", "ld"] {
                let role = format!("configured_{name}");
                let selected = &mut measurement
                    .selected_files
                    .get_mut(&role)
                    .expect("should have configured helper")
                    .selected;
                *selected = PathBuf::from(format!("/bin/{name}"));
                if explicit_paths {
                    measurement
                        .compiler_queries
                        .insert(format!("-print-prog-name={name}"), format!("/bin/{name}\n"));
                }
            }
            measurement
                .selected_files
                .get_mut("compiler")
                .expect("should have compiler")
                .resolved = PathBuf::from("/usr/bin/example-gcc");
            validate_measurement(&measurement)
                .expect("should accept measured aliases and supported GCC query spellings");
        }
    }
}

#[test]
fn measurement_rejects_bare_helpers_with_conflicting_image_identities() {
    for role in ["configured_as", "configured_ld"] {
        for changed in ["resolved", "sha256", "size"] {
            let mut measurement = measurement_fixture();
            let file = measurement
                .selected_files
                .get_mut(role)
                .expect("should have configured helper");
            match changed {
                "resolved" => file.resolved = PathBuf::from("/usr/libexec/example-other-tool"),
                "sha256" => file.sha256 = "d".repeat(64),
                _ => file.size += 1,
            }
            assert!(
                validate_measurement(&measurement).is_err(),
                "should join bare {role} to the fixed image tool {changed}"
            );
        }
    }
}

#[test]
fn measurement_accepts_an_explicit_alternate_image_helper() {
    let mut measurement = measurement_fixture();
    let file = measurement
        .selected_files
        .get_mut("configured_as")
        .expect("should have assembler");
    file.selected = PathBuf::from("/usr/libexec/example-as");
    file.resolved = file.selected.clone();
    file.sha256 = "d".repeat(64);
    measurement.compiler_queries.insert(
        "-print-prog-name=as".to_owned(),
        "/usr/libexec/example-as\n".to_owned(),
    );
    validate_measurement(&measurement)
        .expect("should preserve an explicit measured compiler-selected helper");
}

#[test]
fn installed_image_identity_covers_helpers_headers_modes_and_aliases() {
    let temporary = tempfile::tempdir().expect("should create image fixture");
    let root = temporary.path();
    fs::create_dir_all(root.join("usr/lib/gcc")).expect("should create resources");
    fs::create_dir_all(root.join("usr/include")).expect("should create headers");
    fs::write(root.join("usr/lib/gcc/cc1"), b"helper").expect("should write helper");
    fs::write(root.join("usr/include/example.h"), b"header").expect("should write header");
    symlink("usr/lib", root.join("lib")).expect("should create image alias");
    let first = installed_snapshot(root).expect("should measure image files");
    fs::write(root.join("usr/include/example.h"), b"changed").expect("should change header");
    assert_ne!(
        first,
        installed_snapshot(root).expect("should remeasure"),
        "should bind header bytes"
    );
    fs::write(root.join("usr/include/example.h"), b"header").expect("should restore header");
    fs::write(root.join("usr/lib/gcc/cc1"), b"changed").expect("should change helper");
    assert_ne!(
        first,
        installed_snapshot(root).expect("should remeasure"),
        "should bind helper bytes"
    );
    fs::write(root.join("usr/lib/gcc/cc1"), b"helper").expect("should restore helper");
    fs::remove_file(root.join("lib")).expect("should remove alias");
    symlink("usr/include", root.join("lib")).expect("should retarget alias");
    assert_ne!(
        first,
        installed_snapshot(root).expect("should remeasure"),
        "should bind alias targets"
    );
}

#[test]
fn selected_image_paths_reject_build_roots_and_unmeasured_files() {
    for path in [
        "/boundary/writable/0/gcc",
        "/tmp/cc1",
        "/opt/cc1",
        "/usr/../tmp/cc1",
    ] {
        assert!(
            image_path(std::path::Path::new(path)).is_err(),
            "should reject {path}"
        );
    }
    assert!(
        image_path(std::path::Path::new("/usr/libexec/gcc/cc1")).is_ok(),
        "should accept measured compiler resources"
    );
}

#[test]
fn read_only_image_hardlinks_are_measured_and_selected_aliases_resolve_in_scope() {
    let temporary = tempfile::tempdir().expect("should create image fixture");
    let root = temporary.path();
    fs::create_dir_all(root.join("usr/bin")).expect("should create image tools");
    fs::create_dir_all(root.join("usr/lib")).expect("should create image resources");
    fs::create_dir_all(root.join("tmp")).expect("should create outside scope");
    fs::write(root.join("usr/bin/gcc"), b"compiler").expect("should write image compiler");
    fs::hard_link(root.join("usr/bin/gcc"), root.join("usr/bin/cc"))
        .expect("should create distro hardlink");
    let before = installed_snapshot(root).expect("should accept read-only image hardlinks");
    fs::write(root.join("usr/bin/cc"), b"changed").expect("should update fixture contents");
    assert_ne!(
        before,
        installed_snapshot(root).expect("should remeasure hardlinks"),
        "should bind hardlinked bytes"
    );
    assert!(
        file_identity_at(root, std::path::Path::new("/usr/lib/../bin/gcc")).is_ok(),
        "should accept GCC query dot segments resolving inside measured scope"
    );
    fs::write(root.join("tmp/helper"), b"foreign").expect("should write foreign helper");
    symlink("../../tmp/helper", root.join("usr/bin/helper"))
        .expect("should create escaping helper alias");
    assert!(
        file_identity_at(root, std::path::Path::new("/usr/bin/helper")).is_err(),
        "should reject a resolved helper outside measured image roots"
    );
}

#[test]
fn helper_observations_must_agree_and_match_boundary_and_plan() {
    let fixture = || GnuEvidence {
        measurement: Measurement {
            profile: PROFILE.to_owned(),
            installed_files_sha256: "a".repeat(64),
            content_unmeasured_files: Vec::new(),
            roots: vec![],
            selected_files: BTreeMap::new(),
            compiler_queries: BTreeMap::new(),
            sysroot: "/".to_owned(),
        },
        profile_sha256: profile_digest(),
        helper_sha256: "b".repeat(64),
        build_boundary_fingerprint: hex::encode([1; 32]),
        measurement_boundary_fingerprint: hex::encode([3; 32]),
        plan_sha256: "c".repeat(64),
        identity_scope: "fixture",
        acquisition: "unqualified",
        full_closure_measured: false,
    };
    let first = fixture();
    let mut after = fixture();
    assert!(
        first.verify_after(&after).is_ok(),
        "should accept equal observations"
    );
    after.measurement.installed_files_sha256 = "d".repeat(64);
    assert!(
        first.verify_after(&after).is_err(),
        "should reject changed installed inputs"
    );
    assert!(
        first.verify_binding(&[1; 32], &"c".repeat(64)).is_ok(),
        "should bind the actual completed invocation"
    );
    assert!(
        first.verify_binding(&[2; 32], &"c".repeat(64)).is_err(),
        "should reject another completed boundary"
    );
    assert!(
        first.verify_binding(&[1; 32], &"d".repeat(64)).is_err(),
        "should reject another completed plan"
    );
    assert!(
        first.verify_binding(&[3; 32], &"c".repeat(64)).is_err(),
        "should not accept the measurement sandbox as the build boundary"
    );
}

#[test]
fn binding_requires_a_separate_well_formed_measurement_sandbox() {
    let mut evidence = GnuEvidence::projection_fixture();
    let plan = "b".repeat(64);
    assert_eq!(
        serde_json::to_value(&evidence).expect("should serialize the receipt")["measurement_boundary_fingerprint"],
        hex::encode([3; 32]),
        "should record the measurement sandbox fingerprint"
    );
    assert!(
        evidence.verify_binding(&[1; 32], &plan).is_ok(),
        "should accept a distinct measurement sandbox"
    );

    evidence.measurement_boundary_fingerprint = hex::encode([1; 32]);
    assert!(
        evidence.verify_binding(&[1; 32], &plan).is_err(),
        "should reject measurements recorded as the build boundary itself"
    );
    evidence.measurement_boundary_fingerprint = "not-a-fingerprint".to_owned();
    assert!(
        evidence.verify_binding(&[1; 32], &plan).is_err(),
        "should reject a malformed measurement sandbox fingerprint"
    );
}

/// Ordinary CI tools, a fictional GNU plan and a sandbox with `parent`.
struct DerivationFixture {
    _directory: tempfile::TempDir,
    tools: SelectedRustTools,
    plan: CargoPlan,
    sandbox: MeasurementSandbox,
}

fn derivation_fixture(parent: Option<[u8; 32]>) -> DerivationFixture {
    let directory = tempfile::tempdir().expect("should create fixture");
    let base = fs::canonicalize(directory.path()).expect("should canonicalize fixture");
    let target = crate::policy::ReleaseTarget::X86_64UnknownLinuxGnu;
    let libraries = base.join(format!("tools/lib/rustlib/{}/lib", target.triple()));
    fs::create_dir_all(&libraries).expect("should create tool libraries");
    fs::create_dir_all(base.join("tools/bin")).expect("should create tool binaries");
    for file in ["tools/bin/cargo", "tools/bin/rustc"] {
        fs::write(base.join(file), b"fictional").expect("should write tool");
    }
    fs::write(libraries.join("libstd-example.rlib"), b"fictional").expect("should write std");
    for root in ["checkout", "cargo-state"] {
        fs::create_dir(base.join(root)).expect("should create root");
    }
    let roots = crate::boundary::BoundaryRoots::new(
        &[base.join("checkout")],
        &[base.join("tools")],
        &[base.join("cargo-state")],
    )
    .expect("should build sandbox roots");
    let asset = super::super::AssetId::parse(crate::policy::FIXED_ARTIFACTS[0].name)
        .expect("should select a fixed asset");
    DerivationFixture {
        tools: SelectedRustTools::for_tests(&base.join("tools"), target)
            .expect("should select fictional tools"),
        plan: CargoPlan {
            asset,
            program: base.join("tools/bin/cargo"),
            arguments: linker_arguments(),
            environment: environment(),
            working_directory: base.join("checkout"),
            umask: crate::boundary::Umask::new(0o022).expect("should accept fixture umask"),
            apple_identity_sha256: None,
        },
        sandbox: MeasurementSandbox {
            boundary: Boundary::derivation_fixture_for_tests(roots, parent)
                .expect("should build the derivation fixture"),
            proof: DenialProof::for_tests(),
        },
        _directory: directory,
    }
}

#[test]
fn measurements_require_a_sandbox_derived_from_the_build_boundary() {
    for parent in [None, Some([9; 32])] {
        let fixture = derivation_fixture(parent);
        let error = GnuEvidence::measure(&fixture.tools, &fixture.plan, &[1; 32], &fixture.sandbox)
            .expect_err("should reject a sandbox from another build");
        assert!(
            format!("{error:?}").contains("not derived from the build boundary"),
            "should name the derivation rule for parent {parent:?}"
        );
    }

    let fixture = derivation_fixture(Some([1; 32]));
    let error = GnuEvidence::measure(&fixture.tools, &fixture.plan, &[1; 32], &fixture.sandbox)
        .expect_err("should still require a container-owned sandbox");
    assert!(
        format!("{error:?}").contains("requires the selected Docker boundary"),
        "should pass derivation and reach the container gate"
    );
}

#[test]
fn backend_switch_exception_accepts_only_zero() {
    let root = std::path::Path::new("/example");
    let limits = crate::process::ProcessLimits {
        wall: std::time::Duration::from_secs(1),
        stdout_max: 1024,
        stderr_max: 1024,
    };
    for name in [
        "AWS_LC_SYS_CMAKE_BUILDER",
        "AWS_LC_SYS_CMAKE_BUILDER_x86_64_unknown_linux_gnu",
    ] {
        assert!(
            crate::process::BoundedCommand::new(&root.join("tool"), root, limits)
                .expect("should describe command")
                .environment(name, "0")
                .is_ok(),
            "should accept exact reviewed backend switch"
        );
        assert!(
            crate::process::BoundedCommand::new(&root.join("tool"), root, limits)
                .expect("should describe command")
                .environment(name, "1")
                .is_err(),
            "should reject alternate backend value"
        );
    }
}
