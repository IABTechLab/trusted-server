use std::ffi::CString;
use std::fs;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::symlink;
use std::time::Duration;

use super::*;

/// A fictional developer directory with one default toolchain.
struct Toolchain {
    _directory: tempfile::TempDir,
    developer: PathBuf,
    usr: PathBuf,
}

impl Toolchain {
    fn new() -> Self {
        let directory = tempfile::tempdir().expect("should create toolchain fixture");
        let developer = fs::canonicalize(directory.path())
            .expect("should canonicalize fixture")
            .join("developer");
        let usr = developer.join("Toolchains/XcodeDefault.xctoolchain/usr");
        for relative in ["bin", "lib/clang/17/include", "lib/clang/17/lib/darwin"] {
            fs::create_dir_all(usr.join(relative)).expect("should create toolchain directory");
        }
        for (relative, bytes) in [
            ("bin/clang", "fictional clang"),
            ("bin/ar", "fictional ar"),
            ("bin/ld", "fictional ld"),
            ("lib/libLTO.dylib", "fictional libLTO"),
            ("lib/clang/17/include/stddef.h", "fictional header"),
            (
                "lib/clang/17/lib/darwin/libclang_rt.osx.a",
                "fictional runtime",
            ),
        ] {
            fs::write(usr.join(relative), bytes).expect("should write toolchain file");
        }
        symlink("clang", usr.join("bin/clang++")).expect("should alias clang++");
        symlink("17", usr.join("lib/clang/17.0.0")).expect("should alias resource version");
        Self {
            _directory: directory,
            developer,
            usr,
        }
    }

    fn measure(&self) -> Result<MeasuredToolchain, Report<ReleaseError>> {
        MeasuredToolchain::measure(
            &self.usr.join("bin/clang"),
            &self.developer,
            Instant::now() + Duration::from_secs(30),
            &Cancellation::default(),
        )
    }

    fn receipt(&self) -> ToolchainReceipt {
        self.measure()
            .expect("should measure fictional toolchain")
            .receipt()
            .clone()
    }

    fn error(&self) -> String {
        format!(
            "{:?}",
            self.measure()
                .expect_err("should reject the fictional toolchain")
        )
    }
}

/// A named fixture edit of one toolchain directory.
type Mutation = fn(&Path);

fn make_fifo(path: &Path) {
    let fifo = CString::new(path.to_str().expect("should have UTF-8 path"))
        .expect("should encode FIFO path");
    // SAFETY: the CString remains alive and points to a NUL-terminated path.
    assert_eq!(
        unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) },
        0,
        "should create special file"
    );
}

#[test]
fn libtapi_absence_requires_a_real_unaliased_parent() {
    let toolchain = Toolchain::new();
    let missing = toolchain.usr.join("missing");
    let alias = toolchain.usr.join("aliased-lib");
    symlink("lib", &alias).expect("should alias library directory");
    let dangling = toolchain.usr.join("dangling-lib");
    symlink("missing", &dangling).expect("should create dangling parent alias");
    for parent in [&missing, &alias, &dangling] {
        assert!(
            optional_libtapi_digest(&parent.join("libtapi.dylib"), &|| Ok(())).is_err(),
            "should not record absence below a missing or aliased parent"
        );
    }
}

#[test]
fn libtapi_receipt_records_digest_or_explicit_null_and_detects_presence_changes() {
    let toolchain = Toolchain::new();
    let library = toolchain.usr.join(LIBTAPI);
    let absent = toolchain.measure().expect("should measure without libtapi");
    let serialized = serde_json::to_value(absent.receipt()).expect("should serialize receipt");
    assert_eq!(
        serialized.get("libtapi_sha256"),
        Some(&serde_json::Value::Null),
        "should explicitly record genuine absence"
    );
    assert_eq!(
        absent.receipt().libtapi_sha256(),
        None,
        "should expose absence"
    );
    fs::write(&library, b"fictional libtapi").expect("should add libtapi");
    assert!(absent.verify().is_err(), "should reject added libtapi");
    let present = toolchain.measure().expect("should measure present libtapi");
    let digest = hex::encode(Sha256::digest(b"fictional libtapi"));
    assert_eq!(
        present.receipt().libtapi_sha256(),
        Some(digest.as_str()),
        "should hash exact selected libtapi bytes"
    );
    let serialized = serde_json::to_value(present.receipt()).expect("should serialize receipt");
    assert_eq!(
        serialized["libtapi_sha256"], digest,
        "should serialize measured digest"
    );
    present.verify().expect("should verify unchanged libtapi");
    fs::remove_file(&library).expect("should remove libtapi");
    assert!(present.verify().is_err(), "should reject removed libtapi");
    assert_eq!(
        toolchain.receipt(),
        *absent.receipt(),
        "should remeasure genuine absence identically"
    );
}

#[test]
fn libtapi_rejects_aliases_directories_special_files_and_oversize_files() {
    let cases: [(&str, Mutation); 5] = [
        ("alias", |path| {
            symlink("libLTO.dylib", path).expect("should alias libtapi");
        }),
        ("dangling alias", |path| {
            symlink("missing.dylib", path).expect("should add dangling alias");
        }),
        ("directory", |path| {
            fs::create_dir(path).expect("should create directory");
        }),
        ("special file", make_fifo),
        ("oversize file", |path| {
            File::create(path)
                .expect("should create sparse file")
                .set_len(128 * 1024 * 1024 + 1)
                .expect("should exceed fixed TAPI limit");
        }),
    ];
    for (label, mutate) in cases {
        let toolchain = Toolchain::new();
        mutate(&toolchain.usr.join(LIBTAPI));
        assert!(
            toolchain.measure().is_err(),
            "should reject libtapi {label}"
        );
    }
}

#[test]
fn libtapi_rejects_unreadable_files_and_honors_the_attempt_budget() {
    let toolchain = Toolchain::new();
    let library = toolchain.usr.join(LIBTAPI);
    fs::write(&library, b"fictional libtapi").expect("should write libtapi");
    let measured = toolchain.measure().expect("should measure libtapi");
    fs::set_permissions(&library, fs::Permissions::from_mode(0o0))
        .expect("should remove read permissions");
    assert!(
        toolchain.measure().is_err(),
        "should reject unreadable libtapi"
    );
    fs::set_permissions(&library, fs::Permissions::from_mode(0o600))
        .expect("should restore permissions");
    measured.cancellation.cancel();
    assert!(
        measured.verify().is_err(),
        "should stop cancelled libtapi remeasurement"
    );
    for present in [false, true] {
        if !present {
            fs::remove_file(&library).expect("should remove libtapi");
        } else {
            fs::write(&library, b"fictional libtapi").expect("should restore libtapi");
        }
        assert!(
            optional_libtapi_digest(&library, &|| budget(
                Instant::now() - Duration::from_secs(1),
                &Cancellation::default()
            ))
            .is_err(),
            "should reject an expired budget even when presence is {present}"
        );
    }
}

#[test]
fn libtapi_change_is_detected_during_remeasurement() {
    let toolchain = Toolchain::new();
    let library = toolchain.usr.join("lib/libtapi.dylib");
    fs::write(&library, b"fictional libtapi").expect("should write libtapi");
    let measured = toolchain.measure().expect("should measure toolchain");
    fs::write(&library, b"changed libtapi").expect("should change libtapi");
    assert!(measured.verify().is_err(), "should reject changed libtapi");
}

#[test]
fn receipt_records_single_resource_version_aliases_and_three_digests() {
    let toolchain = Toolchain::new();
    let receipt = toolchain.receipt();
    assert_eq!(
        receipt.clang_resource_version, "17",
        "should select the only real version directory"
    );
    assert_eq!(
        receipt.clang_resource_aliases,
        vec!["17.0.0".to_owned()],
        "should record the version alias"
    );
    assert_eq!(
        receipt.liblto_sha256(),
        hex::encode(Sha256::digest(b"fictional libLTO")),
        "should hash libLTO bytes"
    );
    assert_eq!(
        receipt.clang_resource_tree.entries, 5,
        "should count every resource entry"
    );
    assert_eq!(
        receipt.helper_tree.entries, 4,
        "should count helper files and aliases"
    );
    assert_eq!(
        toolchain.receipt(),
        receipt,
        "should remeasure identically when nothing changed"
    );
    toolchain
        .measure()
        .expect("should measure")
        .verify()
        .expect("should verify unchanged toolchain");
}

#[test]
fn resource_tree_digest_changes_when_any_resource_entry_changes() {
    let toolchain = Toolchain::new();
    let measured = toolchain.measure().expect("should measure");
    let original = measured.receipt().clang_resource_tree_sha256().to_owned();
    fs::write(
        toolchain.usr.join("lib/clang/17/include/stddef.h"),
        b"edited header",
    )
    .expect("should edit resource header");
    assert_ne!(
        toolchain.receipt().clang_resource_tree_sha256(),
        original,
        "should bind resource header bytes"
    );
    assert!(
        measured.verify().is_err(),
        "should reject an edited resource directory"
    );
    fs::write(toolchain.usr.join("lib/clang/17/include/new.h"), b"added")
        .expect("should add resource header");
    assert!(
        measured.verify().is_err(),
        "should reject an added resource entry"
    );
}

#[test]
fn resource_selection_fails_closed_on_ambiguous_or_foreign_entries() {
    let cases: [(&str, Mutation); 5] = [
        ("second version", |resources| {
            fs::create_dir(resources.join("18")).expect("should add second version");
        }),
        ("foreign alias", |resources| {
            fs::create_dir(resources.join("18")).expect("should add directory");
            fs::remove_dir_all(resources.join("17")).expect("should remove version");
            symlink("../..", resources.join("17")).expect("should add foreign alias");
        }),
        ("alias to other", |resources| {
            fs::remove_file(resources.join("17.0.0")).expect("should remove alias");
            symlink("17/include", resources.join("17.0.0")).expect("should add nested alias");
        }),
        ("regular file", |resources| {
            fs::write(resources.join("19"), b"file").expect("should add file");
        }),
        ("non version name", |resources| {
            fs::create_dir(resources.join("include")).expect("should add directory");
        }),
    ];
    for (label, mutate) in cases {
        let toolchain = Toolchain::new();
        mutate(&toolchain.usr.join("lib/clang"));
        let error = toolchain.error();
        assert!(
            error.contains("clang resource"),
            "should reject {label} in resource selection: {error}"
        );
    }
    let toolchain = Toolchain::new();
    fs::remove_dir_all(toolchain.usr.join("lib/clang/17")).expect("should remove version");
    fs::remove_file(toolchain.usr.join("lib/clang/17.0.0")).expect("should remove alias");
    assert!(
        toolchain.error().contains("exactly one version directory"),
        "should reject a missing resource version"
    );
}

#[test]
fn liblto_must_be_present_regular_and_unchanged() {
    let toolchain = Toolchain::new();
    let measured = toolchain.measure().expect("should measure");
    fs::write(toolchain.usr.join("lib/libLTO.dylib"), b"changed libLTO")
        .expect("should change libLTO");
    assert!(measured.verify().is_err(), "should reject changed libLTO");
    fs::remove_file(toolchain.usr.join("lib/libLTO.dylib")).expect("should remove libLTO");
    assert!(
        toolchain.measure().is_err(),
        "should reject a missing libLTO"
    );
    fs::write(toolchain.usr.join("lib/real-lto.dylib"), b"aliased libLTO")
        .expect("should write alias target");
    symlink("real-lto.dylib", toolchain.usr.join("lib/libLTO.dylib")).expect("should alias libLTO");
    assert!(
        toolchain.measure().is_err(),
        "should reject an aliased libLTO"
    );
    fs::remove_file(toolchain.usr.join("lib/libLTO.dylib")).expect("should remove alias");
    fs::create_dir(toolchain.usr.join("lib/libLTO.dylib")).expect("should add directory");
    assert!(
        toolchain.measure().is_err(),
        "should reject a non-regular libLTO"
    );
}

#[test]
fn helper_tree_records_alias_targets() {
    let toolchain = Toolchain::new();
    let measured = toolchain.measure().expect("should measure");
    let original = measured.receipt().helper_tree_sha256().to_owned();
    fs::remove_file(toolchain.usr.join("bin/clang++")).expect("should remove alias");
    symlink("ld", toolchain.usr.join("bin/clang++")).expect("should retarget alias");
    assert_ne!(
        toolchain.receipt().helper_tree_sha256(),
        original,
        "should bind alias targets"
    );
    assert!(
        measured.verify().is_err(),
        "should reject a retargeted alias"
    );
}

#[test]
fn helper_tree_measures_vendor_hardlinks_and_detects_edits_through_the_other_path() {
    let toolchain = Toolchain::new();
    let unlinked = toolchain.receipt();
    fs::hard_link(
        toolchain.usr.join("bin/ar"),
        toolchain.usr.join("bin/ranlib"),
    )
    .expect("should add in-tree vendor hardlink");
    fs::hard_link(
        toolchain.usr.join("bin/ld"),
        toolchain.usr.join("lib/ld-copy"),
    )
    .expect("should add out-of-tree vendor hardlink");
    let measured = toolchain
        .measure()
        .expect("should measure hardlinked vendor helpers in place");
    assert_eq!(
        measured.receipt().helper_tree.entries,
        5,
        "should count the in-tree hardlink as an entry"
    );
    assert_ne!(
        measured.receipt().helper_tree_sha256(),
        unlinked.helper_tree_sha256(),
        "should bind the added hardlinked entry"
    );
    fs::write(
        toolchain.usr.join("lib/ld-copy"),
        b"edited through the other link",
    )
    .expect("should edit shared content outside the tree");
    assert!(
        measured.verify().is_err(),
        "should reject helper content edited through an out-of-tree hardlink"
    );
}

#[test]
fn trees_accept_nested_directories_and_internal_directory_aliases() {
    let toolchain = Toolchain::new();
    let unnested = toolchain.receipt();
    fs::create_dir_all(toolchain.usr.join("bin/nested/deeper")).expect("should nest helpers");
    fs::write(
        toolchain.usr.join("bin/nested/deeper/helper"),
        b"fictional nested helper",
    )
    .expect("should write nested helper");
    symlink(
        "darwin",
        toolchain.usr.join("lib/clang/17/lib/macho_embedded"),
    )
    .expect("should alias a resource subdirectory");
    let measured = toolchain
        .measure()
        .expect("should accept nested directories and internal directory aliases");
    let receipt = measured.receipt();
    assert_eq!(
        receipt.helper_tree.entries, 7,
        "should count nested helper directories and files"
    );
    assert_eq!(
        receipt.clang_resource_tree.entries, 6,
        "should count the internal directory alias"
    );
    assert!(
        receipt.helper_tree_sha256() != unnested.helper_tree_sha256()
            && receipt.clang_resource_tree_sha256() != unnested.clang_resource_tree_sha256(),
        "should bind nested and aliased entries"
    );
    fs::write(toolchain.usr.join("bin/nested/deeper/helper"), b"edited")
        .expect("should edit nested helper");
    assert!(
        measured.verify().is_err(),
        "should reject an edited nested helper"
    );
}

#[test]
fn resource_tree_rejects_special_files() {
    let toolchain = Toolchain::new();
    make_fifo(&toolchain.usr.join("lib/clang/17/include/fifo"));
    assert!(
        toolchain.error().contains("special file"),
        "should reject a special file inside the resource tree"
    );
}

#[test]
fn trees_reject_non_utf8_names_where_the_filesystem_allows_them() {
    let toolchain = Toolchain::new();
    let name = std::ffi::OsStr::from_bytes(b"helper-\xff");
    // APFS refuses invalid UTF-8 names, so on macOS the hazard cannot exist
    // and creation fails; filesystems that accept the name must be rejected.
    if fs::write(toolchain.usr.join("bin").join(name), b"fictional helper").is_err() {
        return;
    }
    assert!(
        toolchain.error().contains("not UTF-8"),
        "should reject a non-UTF-8 helper name"
    );
}

#[test]
fn tree_kinds_are_domain_separated() {
    let toolchain = Toolchain::new();
    let bin = toolchain.usr.join("bin");
    let unbounded = || Ok(());
    let as_helpers =
        vendor_tree_digest(&bin, HELPER_POLICY, &unbounded).expect("should digest as helpers");
    let as_resources =
        vendor_tree_digest(&bin, RESOURCE_POLICY, &unbounded).expect("should digest as resources");
    assert_ne!(
        as_helpers.tree_sha256, as_resources.tree_sha256,
        "should separate identical contents by tree kind"
    );
}

#[test]
fn clang_path_must_be_normal_and_inside_usr_bin() {
    let toolchain = Toolchain::new();
    fs::create_dir_all(toolchain.usr.join("libexec")).expect("should create other directory");
    fs::write(toolchain.usr.join("libexec/clang"), b"fictional clang")
        .expect("should write misplaced clang");
    fs::create_dir_all(toolchain.usr.join("bin/bin")).expect("should create nested bin");
    fs::write(toolchain.usr.join("bin/bin/clang"), b"fictional clang")
        .expect("should write nested clang");
    let cases = [
        ("parent component", toolchain.usr.join("bin/../bin/clang")),
        ("relative path", PathBuf::from("usr/bin/clang")),
        ("not in bin", toolchain.usr.join("libexec/clang")),
        ("bin not in usr", toolchain.usr.join("bin/bin/clang")),
    ];
    for (label, clang) in cases {
        let error = format!(
            "{:?}",
            MeasuredToolchain::measure(
                &clang,
                &toolchain.developer,
                Instant::now() + Duration::from_secs(30),
                &Cancellation::default(),
            )
            .expect_err("should reject the clang path")
        );
        assert!(
            error.contains("Apple clang"),
            "should reject {label} before measuring: {error}"
        );
    }
}

#[test]
fn helper_tree_rejects_unsafe_aliases_and_special_files() {
    let cases: [(&str, Mutation); 4] = [
        ("absolute alias", |bin| {
            symlink("/usr/bin/true", bin.join("absolute")).expect("should add alias");
        }),
        ("escaping alias", |bin| {
            symlink("../lib/libLTO.dylib", bin.join("escape")).expect("should add alias");
        }),
        ("dangling alias", |bin| {
            symlink("missing", bin.join("dangling")).expect("should add alias");
        }),
        ("special file", |bin| make_fifo(&bin.join("fifo"))),
    ];
    for (label, mutate) in cases {
        let toolchain = Toolchain::new();
        mutate(&toolchain.usr.join("bin"));
        assert!(toolchain.measure().is_err(), "should reject {label}");
    }
}

#[test]
fn vendor_tree_enforces_entry_file_and_tree_bounds() {
    let toolchain = Toolchain::new();
    let bin = toolchain.usr.join("bin");
    let unbounded = || Ok(());
    for policy in [
        TreePolicy {
            entries: 3,
            ..HELPER_POLICY
        },
        TreePolicy {
            file_bytes: 4,
            ..HELPER_POLICY
        },
        TreePolicy {
            tree_bytes: 20,
            ..HELPER_POLICY
        },
    ] {
        assert!(
            vendor_tree_digest(&bin, policy, &unbounded).is_err(),
            "should reject exhausted {policy:?}"
        );
    }
    assert!(
        vendor_tree_digest(&bin, HELPER_POLICY, &unbounded).is_ok(),
        "should accept the fixture within real limits"
    );
}

#[test]
fn measurement_stops_when_cancelled_expired_or_outside_developer_dir() {
    let toolchain = Toolchain::new();
    let cancellation = Cancellation::default();
    cancellation.cancel();
    let clang = toolchain.usr.join("bin/clang");
    assert!(
        MeasuredToolchain::measure(
            &clang,
            &toolchain.developer,
            Instant::now() + Duration::from_secs(30),
            &cancellation
        )
        .is_err(),
        "should stop a cancelled measurement"
    );
    assert!(
        MeasuredToolchain::measure(
            &clang,
            &toolchain.developer,
            Instant::now() - Duration::from_secs(1),
            &Cancellation::default()
        )
        .is_err(),
        "should stop an expired measurement"
    );
    assert!(
        MeasuredToolchain::measure(
            &clang,
            &toolchain.usr.join("bin"),
            Instant::now() + Duration::from_secs(30),
            &Cancellation::default()
        )
        .is_err(),
        "should reject a toolchain outside the developer directory"
    );
}
