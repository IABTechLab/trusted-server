use std::fs;
use std::os::unix::fs::symlink;

use super::*;

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
        boundary_fingerprint: hex::encode([1; 32]),
        plan_sha256: "b".repeat(64),
        identity_scope: "fictional_projection_fixture",
        acquisition: "unqualified_candidate_measurement",
        full_closure_measured: false,
    }
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
            roots: vec![],
            selected_files: BTreeMap::new(),
            compiler_queries: BTreeMap::new(),
            sysroot: "/".to_owned(),
        },
        profile_sha256: profile_digest(),
        helper_sha256: "b".repeat(64),
        boundary_fingerprint: hex::encode([1; 32]),
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
