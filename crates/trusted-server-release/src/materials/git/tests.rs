use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use flate2::Compression;
use flate2::write::ZlibEncoder;

use super::*;
use crate::git_objects::checked_sha1;
use crate::materials::locked::CargoMaterials;

const SELECTOR: &str = "git+https://example.com/fictional?rev=locked";

fn temporary() -> tempfile::TempDir {
    let root = fs::canonicalize(std::env::temp_dir()).expect("should resolve temporary parent");
    tempfile::tempdir_in(root).expect("should create canonical temporary directory")
}

fn object(root: &Path, kind: &str, bytes: &[u8]) -> String {
    let header = format!("{kind} {}\0", bytes.len());
    let oid = checked_sha1(&[header.as_bytes(), bytes]).expect("should hash fixture object");
    let path = root.join(&oid[..2]).join(&oid[2..]);
    fs::create_dir_all(path.parent().expect("should have object parent"))
        .expect("should create object directory");
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder
        .write_all(header.as_bytes())
        .expect("should encode header");
    encoder.write_all(bytes).expect("should encode body");
    fs::write(path, encoder.finish().expect("should finish object"))
        .expect("should write fixture object");
    oid
}

fn fixture(root: &Path) -> String {
    let blob = object(root, "blob", b"fictional dependency\n");
    let mut tree = b"100644 Cargo.toml\0".to_vec();
    tree.extend(hex::decode(blob).expect("should decode object id"));
    let tree = object(root, "tree", &tree);
    object(root, "commit", format!("tree {tree}\nauthor Fictional <author@example.com> 1 +0000\ncommitter Fictional <author@example.com> 1 +0000\n\nfictional commit\n").as_bytes())
}

fn locked(revision: &str) -> LockedMaterials {
    LockedMaterials {
        source_commit: "a".repeat(40),
        cargo_lock_sha256: "b".repeat(64),
        npm_lock_sha256: "c".repeat(64),
        cargo: CargoMaterials {
            registry: BTreeMap::new(),
            git: BTreeSet::from([(SELECTOR.to_owned(), revision.to_owned())]),
            git_packages: Vec::new(),
        },
        npm: BTreeMap::new(),
    }
}

#[test]
fn complete_loose_snapshot_is_verified_from_locked_commit() {
    let input = temporary();
    let revision = fixture(input.path());
    let locked = locked(&revision);
    let snapshot = locked
        .git_collection(
            Path::new("/usr/bin/git"),
            Instant::now() + Duration::from_secs(30),
            &Cancellation::default(),
        )
        .expect("should select local Git fixture")
        .verify(SELECTOR, &revision, input.path())
        .expect("should verify all selected objects");
    assert_eq!(
        snapshot.source_commit(),
        "a".repeat(40),
        "should bind source"
    );
    assert_eq!(
        snapshot.cargo_lock_sha256(),
        "b".repeat(64),
        "should bind raw lockfile"
    );
    assert_eq!(snapshot.selector(), SELECTOR, "should bind exact selector");
    assert_eq!(snapshot.revision(), revision, "should bind locked commit");
    assert_eq!(
        snapshot.root_tree().len(),
        40,
        "should retain root identity"
    );
    assert_eq!(snapshot.object_count(), 3, "should retain the full closure");
    assert_ne!(
        snapshot.object_store(),
        input.path(),
        "should own a new store"
    );
}

fn collection(locked: &LockedMaterials) -> GitCollection<'_> {
    locked
        .git_collection(
            Path::new("/usr/bin/git"),
            Instant::now() + Duration::from_secs(60),
            &Cancellation::default(),
        )
        .expect("should select Git for a bounded local fixture")
}

fn commit(root: &Path, tree: &str) -> String {
    object(root, "commit", format!("tree {tree}\nauthor Fictional <author@example.com> 1 +0000\ncommitter Fictional <author@example.com> 1 +0000\n\nfixture\n").as_bytes())
}

fn tree(root: &Path, entries: &[(&[u8], &[u8], &str)]) -> String {
    let mut bytes = Vec::new();
    for (mode, name, oid) in entries {
        bytes.extend_from_slice(mode);
        bytes.push(b' ');
        bytes.extend_from_slice(name);
        bytes.push(0);
        bytes.extend(hex::decode(oid).expect("should decode fixture identity"));
    }
    object(root, "tree", &bytes)
}

#[test]
fn unselected_objects_are_not_retained_and_input_replacement_is_irrelevant() {
    let input = temporary();
    let revision = fixture(input.path());
    let extra = object(input.path(), "blob", b"unselected material");
    let locked = locked(&revision);
    let snapshot = collection(&locked)
        .verify(SELECTOR, &revision, input.path())
        .expect("should verify selected closure");
    assert!(
        !snapshot
            .object_store()
            .join(&extra[..2])
            .join(&extra[2..])
            .exists(),
        "should exclude unselected objects"
    );
    let retained = snapshot
        .object_store()
        .join(&revision[..2])
        .join(&revision[2..]);
    let before = fs::read(&retained).expect("should read retained commit");
    fs::write(
        input.path().join(&revision[..2]).join(&revision[2..]),
        b"changed input",
    )
    .expect("should replace untrusted input");
    assert_eq!(
        fs::read(retained).expect("should read independent retained commit"),
        before,
        "should own an independent verified snapshot"
    );
}

#[test]
fn caller_cannot_choose_an_unlocked_selector_or_revision() {
    let input = temporary();
    let revision = fixture(input.path());
    let locked = locked(&revision);
    let collection = collection(&locked);
    assert!(
        collection
            .verify("git+https://example.com/other", &revision, input.path())
            .is_err(),
        "should reject an unlocked selector"
    );
    assert!(
        collection
            .verify(SELECTOR, &"f".repeat(40), input.path())
            .is_err(),
        "should reject an unlocked revision"
    );
}

#[test]
fn locked_revision_must_select_a_commit_object() {
    let input = temporary();
    let revision = object(input.path(), "blob", b"not a commit");
    let locked = locked(&revision);
    assert!(
        collection(&locked)
            .verify(SELECTOR, &revision, input.path())
            .is_err(),
        "should reject an object of the wrong type"
    );
}

#[test]
fn changed_blob_bytes_fail_the_selected_object_identity() {
    let input = temporary();
    let revision = fixture(input.path());
    let blob = object(input.path(), "blob", b"fictional dependency\n");
    let different = object(input.path(), "blob", b"changed dependency\n");
    fs::copy(
        input.path().join(&different[..2]).join(&different[2..]),
        input.path().join(&blob[..2]).join(&blob[2..]),
    )
    .expect("should replace blob bytes under old name");
    let locked = locked(&revision);
    assert!(
        collection(&locked)
            .verify(SELECTOR, &revision, input.path())
            .is_err(),
        "should rehash blob content independently of its filename"
    );
}

#[test]
fn unsafe_entry_outside_cargo_package_root_still_rejects() {
    let input = temporary();
    let blob = object(input.path(), "blob", b"fictional dependency");
    let unrelated = tree(input.path(), &[(b"160000", b"unreviewed-submodule", &blob)]);
    let root = tree(
        input.path(),
        &[
            (b"100644", b"Cargo.toml", &blob),
            (b"40000", b"unrelated", &unrelated),
        ],
    );
    let revision = commit(input.path(), &root);
    let locked = locked(&revision);
    assert!(
        collection(&locked)
            .verify(SELECTOR, &revision, input.path())
            .is_err(),
        "should walk every selected subtree"
    );
}

#[test]
fn missing_blob_in_unrelated_subtree_still_rejects() {
    let input = temporary();
    let blob = object(input.path(), "blob", b"fictional dependency");
    let unrelated = tree(input.path(), &[(b"100644", b"missing", &"d".repeat(40))]);
    let root = tree(
        input.path(),
        &[
            (b"100644", b"Cargo.toml", &blob),
            (b"40000", b"unrelated", &unrelated),
        ],
    );
    let revision = commit(input.path(), &root);
    let locked = locked(&revision);
    assert!(
        collection(&locked)
            .verify(SELECTOR, &revision, input.path())
            .is_err(),
        "should authenticate blobs outside selected crate roots"
    );
}

#[test]
fn unsafe_or_noncanonical_tree_entries_reject() {
    let oid = "d".repeat(40);
    for (mode, name) in [
        (&b"160000"[..], &b"submodule"[..]),
        (b"100664", b"mode"),
        (b"100644", b".."),
        (b"100644", b".Git"),
        (b"100644", b"back\\slash"),
        (b"100644", b"line\nbreak"),
        (b"100644", b"a/b"),
        (b"100644", b"trailing."),
        (b"100644", b"trailing "),
        (b"100644", b"colon:name"),
        (b"100644", b"\xc3\xa9"),
    ] {
        let mut bytes = mode.to_vec();
        bytes.push(b' ');
        bytes.extend_from_slice(name);
        bytes.push(0);
        bytes.extend(hex::decode(&oid).expect("should decode fixture id"));
        assert!(
            parse_tree(&bytes).is_err(),
            "should reject unsafe tree entry"
        );
    }
    for names in [["A", "a"], ["a", "a"], ["z", "a"]] {
        let mut bytes = Vec::new();
        for name in names {
            bytes.extend_from_slice(format!("100644 {name}\0").as_bytes());
            bytes.extend(hex::decode(&oid).expect("should decode fixture id"));
        }
        assert!(
            parse_tree(&bytes).is_err(),
            "should reject aliases, duplicate names and noncanonical ordering"
        );
    }
}

#[test]
fn linked_blobs_are_retained_as_inert_verified_objects() {
    let input = temporary();
    let target = object(input.path(), "blob", b"example readme bytes\n");
    let link = object(input.path(), "blob", b"../README.md");
    let package = tree(input.path(), &[(b"120000", b"README.md", &link)]);
    let root = tree(
        input.path(),
        &[
            (b"100644", b"README.md", &target),
            (b"40000", b"package", &package),
        ],
    );
    let revision = commit(input.path(), &root);
    let locked = locked(&revision);
    let snapshot = collection(&locked)
        .verify(SELECTOR, &revision, input.path())
        .expect("should retain authenticated link blobs without creating links");
    assert_eq!(
        snapshot.object_count(),
        5,
        "should retain the complete commit closure"
    );
    assert!(
        snapshot
            .object_store()
            .join(&link[..2])
            .join(&link[2..])
            .is_file(),
        "should retain the link target as an inert object"
    );
}

#[test]
fn input_configuration_and_alternates_never_supply_objects() {
    let input = temporary();
    let revision = fixture(input.path());
    fs::create_dir(input.path().join("info")).expect("should create object info fixture");
    fs::write(
        input.path().join("info/alternates"),
        b"/fictional/untrusted\n",
    )
    .expect("should write forbidden indirection");
    let locked = locked(&revision);
    assert!(
        collection(&locked)
            .verify(SELECTOR, &revision, input.path())
            .is_err(),
        "should reject object-store indirection"
    );
    fs::remove_file(input.path().join("info/alternates"))
        .expect("should remove forbidden indirection");
    fs::write(
        input.path().join("config"),
        b"[core]\nrepositoryformatversion=999\n",
    )
    .expect("should write forbidden configuration");
    assert!(
        collection(&locked)
            .verify(SELECTOR, &revision, input.path())
            .is_err(),
        "should reject configuration inside object input"
    );
}

#[test]
fn input_ancestor_symlink_and_object_hardlink_are_rejected() {
    let parent = temporary();
    let input = parent.path().join("objects");
    fs::create_dir(&input).expect("should create object root");
    let revision = fixture(&input);
    let locked = locked(&revision);
    let alias = parent.path().join("alias");
    std::os::unix::fs::symlink(&input, &alias).expect("should create ancestor alias");
    assert!(
        collection(&locked)
            .verify(SELECTOR, &revision, &alias)
            .is_err(),
        "should reject aliased input ancestors"
    );
    let original = input.join(&revision[..2]).join(&revision[2..]);
    fs::hard_link(&original, parent.path().join("hardlink"))
        .expect("should create outside hardlink");
    assert!(
        collection(&locked)
            .verify(SELECTOR, &revision, &input)
            .is_err(),
        "should reject hard-linked input files"
    );
}

#[test]
fn cancelled_and_expired_collections_fail_closed() {
    let input = temporary();
    let revision = fixture(input.path());
    let locked = locked(&revision);
    let cancellation = Cancellation::default();
    let collection = locked
        .git_collection(
            Path::new("/usr/bin/git"),
            Instant::now() + Duration::from_secs(60),
            &cancellation,
        )
        .expect("should create live collection");
    cancellation.cancel();
    assert!(
        collection
            .verify(SELECTOR, &revision, input.path())
            .is_err(),
        "should reject cancelled work"
    );
    assert!(
        locked
            .git_collection(
                Path::new("/usr/bin/git"),
                Instant::now() - Duration::from_secs(1),
                &Cancellation::default()
            )
            .is_err(),
        "should reject an expired collection"
    );
}

fn pack(input: &Path, revision: &str) {
    let repository = input.parent().expect("should have repository parent");
    fs::create_dir(repository.join("refs")).expect("should create bare refs");
    fs::write(repository.join("HEAD"), b"ref: refs/heads/unused\n")
        .expect("should write bare HEAD");
    fs::write(
        repository.join("config"),
        b"[core]\nbare=true\nrepositoryformatversion=0\n",
    )
    .expect("should write bare config");
    fs::create_dir(input.join("pack")).expect("should create pack directory");
    let output = BoundedCommand::new(
        Path::new("/usr/bin/git"),
        repository,
        ProcessLimits {
            wall: Duration::from_secs(30),
            stdout_max: 1024,
            stderr_max: 4096,
        },
    )
    .expect("should describe real Git fixture")
    .arguments([
        OsString::from("--git-dir"),
        repository.as_os_str().to_owned(),
        OsString::from("pack-objects"),
        OsString::from("--revs"),
        input.join("pack/pack").into_os_string(),
    ])
    .environment("HOME", repository)
    .expect("should pin HOME")
    .environment("GIT_CONFIG_NOSYSTEM", "1")
    .expect("should disable system config")
    .stdin_bytes(format!("{revision}\n").into_bytes())
    .run()
    .and_then(ProcessResult::require_success)
    .expect("should create real Git pack and index");
    assert_eq!(output.len(), 41, "should emit one pack identity");
    for entry in fs::read_dir(input).expect("should read fixture object root") {
        let entry = entry.expect("should read object entry");
        if entry.file_name().len() == 2 {
            fs::remove_dir_all(entry.path()).expect("should remove loose originals");
        }
    }
}

#[test]
fn real_packed_objects_are_verified_and_reemitted_as_selected_loose_objects() {
    let parent = temporary();
    let input = parent.path().join("objects");
    fs::create_dir(&input).expect("should create object root");
    let revision = fixture(&input);
    pack(&input, &revision);
    let locked = locked(&revision);
    let snapshot = collection(&locked)
        .verify(SELECTOR, &revision, &input)
        .expect("should independently verify real packed objects");
    assert_eq!(
        snapshot.object_count(),
        3,
        "should retain all selected objects"
    );
    assert!(
        !snapshot.object_store().join("pack").exists(),
        "should not retain incoming pack extras"
    );
}

#[test]
fn malformed_packed_material_fails_closed() {
    let parent = temporary();
    let input = parent.path().join("objects");
    fs::create_dir(&input).expect("should create object root");
    let revision = fixture(&input);
    pack(&input, &revision);
    let index = fs::read_dir(input.join("pack"))
        .expect("should list packs")
        .map(|entry| entry.expect("should read pack entry").path())
        .find(|path| path.extension().is_some_and(|extension| extension == "idx"))
        .expect("should have pack index");
    fs::remove_file(&index).expect("should remove read-only fixture index");
    fs::write(index, b"truncated index").expect("should corrupt pack index");
    let locked = locked(&revision);
    assert!(
        collection(&locked)
            .verify(SELECTOR, &revision, &input)
            .is_err(),
        "should reject malformed packed material"
    );
}

#[test]
fn decoded_budget_is_shared_across_separate_dependency_verifications() {
    let input = temporary();
    let revision = fixture(input.path());
    let locked = locked(&revision);
    let collection = collection(&locked);
    collection
        .verify(SELECTOR, &revision, input.path())
        .expect("should verify first snapshot");
    collection.objects.set_budget_for_tests(1);
    assert!(
        collection
            .verify(SELECTOR, &revision, input.path())
            .is_err(),
        "should preserve collection budget instead of resetting per repository"
    );
}

#[test]
fn object_symlink_and_fifo_are_rejected_without_blocking() {
    let input = temporary();
    let revision = fixture(input.path());
    let locked = locked(&revision);
    let path = input.path().join(&revision[..2]).join(&revision[2..]);
    fs::remove_file(&path).expect("should remove selected loose object");
    std::os::unix::fs::symlink("/fictional/untrusted", &path)
        .expect("should create dangling object alias");
    assert!(
        collection(&locked)
            .verify(SELECTOR, &revision, input.path())
            .is_err(),
        "should reject symlink input objects"
    );
    fs::remove_file(&path).expect("should remove object alias");
    let raw = std::ffi::CString::new(std::os::unix::ffi::OsStrExt::as_bytes(path.as_os_str()))
        .expect("should encode FIFO path");
    // The NUL-terminated path stays alive while mkfifo creates a test-only inode.
    assert_eq!(
        unsafe { libc::mkfifo(raw.as_ptr(), 0o600) },
        0,
        "should create FIFO fixture"
    );
    assert!(
        collection(&locked)
            .verify(SELECTOR, &revision, input.path())
            .is_err(),
        "should reject FIFO objects without waiting for a writer"
    );
}

#[test]
fn changed_selected_git_is_rejected_before_execution() {
    let input = temporary();
    let revision = fixture(input.path());
    let locked = locked(&revision);
    let tool_root = temporary();
    let program = tool_root.path().join("git");
    fs::copy("/usr/bin/git", &program).expect("should copy selected executable fixture");
    let collection = locked
        .git_collection(
            &program,
            Instant::now() + Duration::from_secs(30),
            &Cancellation::default(),
        )
        .expect("should select fixture executable");
    fs::write(&program, b"changed selected executable").expect("should mutate executable fixture");
    assert!(
        collection
            .verify(SELECTOR, &revision, input.path())
            .is_err(),
        "should rejoin selected executable bytes before spawning"
    );
}

#[test]
fn deeply_nested_selected_tree_rejects_under_depth_bound() {
    let input = temporary();
    let mut root = object(input.path(), "tree", b"");
    for _ in 0..TREE_DEPTH {
        root = tree(input.path(), &[(b"40000", b"d", &root)]);
    }
    let revision = commit(input.path(), &root);
    let locked = locked(&revision);
    assert!(
        collection(&locked)
            .verify(SELECTOR, &revision, input.path())
            .is_err(),
        "should reject a selected tree deeper than the fixed bound"
    );
}

#[test]
fn truncated_pack_body_is_rejected() {
    let parent = temporary();
    let input = parent.path().join("objects");
    fs::create_dir(&input).expect("should create object root");
    let revision = fixture(&input);
    pack(&input, &revision);
    let body = fs::read_dir(input.join("pack"))
        .expect("should list packs")
        .map(|entry| entry.expect("should read pack entry").path())
        .find(|path| {
            path.extension()
                .is_some_and(|extension| extension == "pack")
        })
        .expect("should have pack body");
    fs::remove_file(&body).expect("should remove read-only fixture pack");
    fs::write(body, b"PACK").expect("should truncate pack body");
    let locked = locked(&revision);
    assert!(
        collection(&locked)
            .verify(SELECTOR, &revision, &input)
            .is_err(),
        "should reject truncated packed material"
    );
}

#[test]
fn duplicate_root_tree_header_is_not_accepted() {
    let input = temporary();
    let tree = object(input.path(), "tree", b"");
    let revision = object(
        input.path(),
        "commit",
        format!("tree {tree}\ntree {tree}\n\nfixture\n").as_bytes(),
    );
    let locked = locked(&revision);
    assert!(
        collection(&locked)
            .verify(SELECTOR, &revision, input.path())
            .is_err(),
        "should reject an ambiguous root-tree binding"
    );
}

#[test]
fn fifo_replacement_of_selected_git_does_not_wait_for_a_writer() {
    let input = temporary();
    let revision = fixture(input.path());
    let locked = locked(&revision);
    let tools = temporary();
    let program = tools.path().join("git");
    fs::copy("/usr/bin/git", &program).expect("should copy executable fixture");
    let collection = locked
        .git_collection(
            &program,
            Instant::now() + Duration::from_secs(30),
            &Cancellation::default(),
        )
        .expect("should select copied executable");
    fs::remove_file(&program).expect("should remove selected executable");
    let raw = std::ffi::CString::new(std::os::unix::ffi::OsStrExt::as_bytes(program.as_os_str()))
        .expect("should encode FIFO path");
    // The test replaces an owned executable path with a test-only FIFO.
    assert_eq!(
        unsafe { libc::mkfifo(raw.as_ptr(), 0o700) },
        0,
        "should create FIFO replacement"
    );
    std::thread::scope(|scope| {
        let (sender, receiver) = std::sync::mpsc::channel();
        let worker = scope.spawn(move || {
            let rejected = collection
                .verify(SELECTOR, &revision, input.path())
                .is_err();
            sender
                .send(rejected)
                .expect("should report the FIFO verification outcome");
        });
        let outcome = receiver.recv_timeout(Duration::from_secs(10));
        // A read/write nonblocking descriptor also releases a regressed reader
        // that reaches open after the timeout, without relying on timer ordering.
        let rescued = if outcome.is_err() {
            Some(
                std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .custom_flags(libc::O_NONBLOCK)
                    .open(&program)
                    .expect("should release a regressed blocking FIFO reader"),
            )
        } else {
            None
        };
        worker
            .join()
            .expect("should join the FIFO verification worker");
        let rejected = outcome
            .unwrap_or_else(|_| receiver.recv().expect("should receive the rescued outcome"));
        assert!(rejected, "should reject FIFO executable replacement");
        assert!(
            rescued.is_none(),
            "should reject without requiring a FIFO writer"
        );
    });
}

fn independent_object_copy(input: &Path, output: &Path) {
    let mut total = 0_u64;
    let mut count = 0_usize;
    for entry in fs::read_dir(input).expect("should list original object data") {
        let entry = entry.expect("should read original object directory");
        assert!(
            fs::symlink_metadata(entry.path())
                .expect("should inspect original object directory")
                .is_dir(),
            "should copy object directories only"
        );
        let name = entry.file_name();
        let name = name.to_str().expect("should use canonical object names");
        assert!(
            name == "pack"
                || name == "info"
                || super::lower_oid(format!("{name}{}", "0".repeat(38)).as_bytes()),
            "should reject unrelated object-store entries"
        );
        fs::create_dir(output.join(name)).expect("should create independent object directory");
        for member in fs::read_dir(entry.path()).expect("should list object directory") {
            let member = member.expect("should read original object member");
            let metadata =
                fs::symlink_metadata(member.path()).expect("should inspect original object member");
            assert!(metadata.is_file(), "should not follow original aliases");
            total += metadata.len();
            count += 1;
            assert!(
                total <= 512 * 1024 * 1024 && count <= 100_000,
                "should bound diagnostic input copying before original verification"
            );
            let destination = output.join(name).join(member.file_name());
            fs::copy(member.path(), &destination).expect("should independently copy object bytes");
            assert_eq!(
                fs::metadata(destination)
                    .expect("should inspect private copy")
                    .nlink(),
                1,
                "should detach ambient cache hard links"
            );
        }
    }
}

#[test]
#[ignore = "requires explicitly selected current-lock Git object data; no acquisition qualification"]
fn actual_current_lock_git_material_closure() {
    let input = std::env::var_os("TS_RELEASE_CARGO_GIT_FIXTURE")
        .map(PathBuf::from)
        .expect("should explicitly select original Git object data");
    let bytes = fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.lock"))
        .expect("should read the actual current source lockfile");
    let cargo = crate::materials::locked::parse_cargo(&bytes)
        .expect("should parse all current locked identities");
    assert_eq!(
        cargo.git.len(),
        1,
        "should explicitly review the current unique Git material"
    );
    let (selector, revision) = cargo
        .git
        .iter()
        .next()
        .expect("should select locked Git material")
        .clone();
    let locked = LockedMaterials {
        // This diagnostic fixture supplies no official source approval.
        source_commit: "a".repeat(40),
        cargo_lock_sha256: hex::encode(Sha256::digest(&bytes)),
        npm_lock_sha256: "b".repeat(64),
        cargo,
        npm: BTreeMap::new(),
    };
    let copied = temporary();
    independent_object_copy(&input, copied.path());
    let snapshot = locked
        .git_collection(
            Path::new("/usr/bin/git"),
            Instant::now() + Duration::from_secs(180),
            &Cancellation::default(),
        )
        .expect("should select bounded local diagnostic Git")
        .verify(&selector, &revision, copied.path())
        .expect("should verify the actual current locked whole-tree material");
    assert_eq!(
        snapshot.revision(),
        revision,
        "should retain the exact current locked revision"
    );
    assert_eq!(
        snapshot.selector(),
        selector,
        "should retain the exact current locked selector"
    );
    assert_eq!(
        snapshot.object_count(),
        557,
        "should cover the independently measured current whole closure"
    );
    assert_eq!(
        snapshot.cargo_lock_sha256(),
        hex::encode(Sha256::digest(&bytes)),
        "should bind exact current raw lock bytes"
    );
    let mut total = 0_u64;
    let mut largest = 0_u64;
    let mut blob_path = None;
    for directory in
        fs::read_dir(snapshot.object_store()).expect("should inspect retained object directories")
    {
        for member in fs::read_dir(directory.expect("should read retained directory").path())
            .expect("should inspect retained objects")
        {
            let path = member.expect("should read retained object").path();
            let decoder = flate2::read::ZlibDecoder::new(
                fs::File::open(&path).expect("should open verified loose object"),
            );
            let mut decoded = Vec::new();
            decoder
                .take(32 * 1024 * 1024 + 128)
                .read_to_end(&mut decoded)
                .expect("should decode bounded verified loose object");
            let end = decoded
                .iter()
                .position(|byte| *byte == 0)
                .expect("should retain canonical object header");
            let size = (decoded.len() - end - 1) as u64;
            assert!(
                size <= 32 * 1024 * 1024,
                "should preserve the Cargo per-object bound"
            );
            total += size;
            largest = largest.max(size);
            if decoded.starts_with(b"blob ") {
                blob_path = Some(path);
            }
        }
    }
    assert_eq!(
        total, 6_334_558,
        "should retain exactly the independently measured decoded closure bytes"
    );
    assert_eq!(
        largest, 636_919,
        "should retain the independently measured largest object"
    );
    assert!(
        total <= 512 * 1024 * 1024,
        "should remain within the shared Cargo budget"
    );
    let altered = temporary();
    independent_object_copy(snapshot.object_store(), altered.path());
    let blob = blob_path.expect("should retain at least one real blob");
    let relative = blob
        .strip_prefix(snapshot.object_store())
        .expect("should select a private retained object");
    fs::write(altered.path().join(relative), b"corrupt object")
        .expect("should corrupt only independently copied data");
    assert!(
        collection(&locked)
            .verify(&selector, &revision, altered.path())
            .is_err(),
        "should reject changed actual selected object bytes"
    );
    let root = snapshot.root_tree();
    fs::remove_file(altered.path().join(&root[..2]).join(&root[2..]))
        .expect("should remove the actual selected root tree from private data");
    assert!(
        collection(&locked)
            .verify(&selector, &revision, altered.path())
            .is_err(),
        "should reject missing actual selected root tree"
    );
}
