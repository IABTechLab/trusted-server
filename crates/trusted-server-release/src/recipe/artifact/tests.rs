use std::fs;
use std::os::unix::fs::symlink;
use std::path::PathBuf;

use sha2::Sha256;

use super::*;

const LIMIT: u64 = 1024;

struct Layout {
    _directory: tempfile::TempDir,
    target: PathBuf,
    output: PathBuf,
}

fn layout() -> Layout {
    let directory = tempfile::tempdir().expect("should create layout");
    let base = fs::canonicalize(directory.path()).expect("should canonicalize layout");
    let target = base.join("target/release");
    let output = base.join("output");
    fs::create_dir_all(&target).expect("should create target release directory");
    fs::create_dir(&output).expect("should create output directory");
    fs::write(target.join("example-bin"), b"fictional executable").expect("should write source");
    Layout {
        _directory: directory,
        target,
        output,
    }
}

impl Layout {
    /// The writable root that holds `release/`.
    fn root(&self) -> PathBuf {
        self.target
            .parent()
            .expect("should have a writable root")
            .to_path_buf()
    }
}

fn copy(layout: &Layout, name: &str) -> Result<CopiedArtifact, Report<ReleaseError>> {
    copy_artifact(
        &layout.target.join("example-bin"),
        &layout.root(),
        &layout.output,
        name,
        LIMIT,
    )
}

fn rejection(layout: &Layout, name: &str) -> String {
    match copy(layout, name) {
        Ok(_) => panic!("should reject the copy"),
        Err(error) => format!("{error:?}"),
    }
}

#[test]
fn copy_records_name_size_and_sha256_of_the_written_bytes() {
    let layout = layout();
    let copied = copy(&layout, "ts-example-asset").expect("should copy the executable");
    assert_eq!(
        copied.name(),
        "ts-example-asset",
        "should keep the asset name"
    );
    assert_eq!(copied.size(), 20, "should record the byte count");
    assert_eq!(
        copied.sha256(),
        hex::encode(Sha256::digest(b"fictional executable")),
        "should hash the source bytes"
    );
    let written = layout.output.join("ts-example-asset");
    assert_eq!(
        fs::read(&written).expect("should read the output"),
        b"fictional executable",
        "should write identical bytes"
    );
    let metadata = fs::symlink_metadata(&written).expect("should stat the output");
    assert!(
        metadata.is_file() && metadata.nlink() == 1,
        "should write one regular file"
    );
    assert_eq!(
        metadata.permissions().mode() & 0o7777,
        0o755,
        "should set the mode independent of the umask"
    );
    assert_eq!(copied.path(), written, "should report the output path");
}

#[test]
fn cargo_deps_hardlink_is_accepted_but_more_links_are_not() {
    let layout = layout();
    fs::create_dir(layout.target.join("deps")).expect("should create deps");
    fs::hard_link(
        layout.target.join("example-bin"),
        layout.target.join("deps/example-bin-1234"),
    )
    .expect("should link like Cargo does");
    copy(&layout, "ts-example-asset").expect("should accept the two-link Cargo layout");
    fs::hard_link(
        layout.target.join("example-bin"),
        layout.target.join("deps/extra"),
    )
    .expect("should add a third link");
    assert!(
        rejection(&layout, "ts-example-other").contains("link"),
        "should reject a source with three links"
    );
    assert!(
        !layout.output.join("ts-example-other").exists(),
        "should leave no output after a rejection"
    );
}

#[test]
fn symlink_directory_and_oversize_sources_are_rejected_without_output() {
    let layout = layout();
    fs::remove_file(layout.target.join("example-bin")).expect("should remove source");
    fs::write(layout.target.join("real"), b"x").expect("should write real file");
    symlink("real", layout.target.join("example-bin")).expect("should create source symlink");
    assert!(
        rejection(&layout, "ts-example-asset").contains("source"),
        "should reject a symlink source"
    );
    fs::remove_file(layout.target.join("example-bin")).expect("should remove symlink");
    fs::create_dir(layout.target.join("example-bin")).expect("should create directory source");
    assert!(
        rejection(&layout, "ts-example-asset").contains("source"),
        "should reject a directory source"
    );
    fs::remove_dir(layout.target.join("example-bin")).expect("should remove directory");
    fs::write(layout.target.join("example-bin"), vec![0_u8; 1025]).expect("should write oversize");
    assert!(
        rejection(&layout, "ts-example-asset").contains("limit"),
        "should reject an oversize source"
    );
    assert_eq!(
        fs::read_dir(&layout.output)
            .expect("should list output")
            .count(),
        0,
        "should leave the output directory empty after every rejection"
    );
}

#[test]
fn existing_or_aliased_output_is_never_overwritten() {
    let layout = layout();
    fs::write(layout.output.join("ts-example-asset"), b"previous").expect("should write existing");
    assert!(
        rejection(&layout, "ts-example-asset").contains("output"),
        "should reject an existing file"
    );
    assert_eq!(
        fs::read(layout.output.join("ts-example-asset")).expect("should read existing"),
        b"previous",
        "should leave the existing file untouched"
    );
    let victim = layout.target.join("victim");
    fs::write(&victim, b"victim").expect("should write victim");
    symlink(&victim, layout.output.join("ts-example-link")).expect("should plant symlink");
    assert!(
        rejection(&layout, "ts-example-link").contains("output"),
        "should reject a planted symlink"
    );
    assert_eq!(
        fs::read(&victim).expect("should read victim"),
        b"victim",
        "should not write through the link"
    );
    symlink("missing", layout.output.join("ts-example-dangling"))
        .expect("should plant dangling link");
    assert!(
        rejection(&layout, "ts-example-dangling").contains("output"),
        "should reject a dangling link"
    );
    fs::create_dir(layout.output.join("ts-example-dir")).expect("should plant directory");
    assert!(
        rejection(&layout, "ts-example-dir").contains("output"),
        "should reject a directory"
    );
}

#[test]
fn unsafe_names_and_aliased_output_directories_are_rejected() {
    let layout = layout();
    for name in [
        "",
        ".",
        "..",
        "a/b",
        "../escape",
        "/abs",
        "has space",
        "nul\0byte",
        ".hidden",
    ] {
        assert!(
            copy(&layout, name).is_err(),
            "should reject asset file name {name:?}"
        );
    }
    let alias = layout
        .output
        .parent()
        .expect("should have parent")
        .join("alias");
    symlink(&layout.output, &alias).expect("should alias the output directory");
    let error = copy_artifact(
        &layout.target.join("example-bin"),
        &layout.root(),
        &alias,
        "ts-example-asset",
        LIMIT,
    )
    .expect_err("should reject a symlinked output directory");
    assert!(
        format!("{error:?}").contains("output"),
        "should name the output directory"
    );
    assert!(
        fs::read_dir(&layout.output)
            .expect("should list output")
            .next()
            .is_none(),
        "should write nothing through the alias"
    );
}

#[test]
fn symlinked_source_directories_below_the_writable_root_are_rejected() {
    let layout = layout();
    let root = layout.root();
    let real = root
        .parent()
        .expect("should have parent")
        .join("real-release");
    fs::rename(&layout.target, &real).expect("should move the release directory");
    symlink(&real, &layout.target).expect("should alias the release directory");
    assert!(
        rejection(&layout, "ts-example-asset").contains("source directory"),
        "should reject a symlinked release directory"
    );

    let layout = self::layout();
    let root = layout.root();
    let triple = root.join("example-triple");
    let outside = root
        .parent()
        .expect("should have parent")
        .join("outside-triple");
    fs::create_dir_all(outside.join("release")).expect("should create the outside tree");
    fs::write(outside.join("release/example-bin"), b"outside executable")
        .expect("should write the outside executable");
    symlink(&outside, &triple).expect("should alias the triple directory");
    let error = copy_artifact(
        &triple.join("release/example-bin"),
        &root,
        &layout.output,
        "ts-example-asset",
        LIMIT,
    )
    .expect_err("should reject a symlinked triple directory");
    assert!(
        format!("{error:?}").contains("source directory"),
        "should name the aliased source directory"
    );
    assert!(
        fs::read_dir(&layout.output)
            .expect("should list output")
            .next()
            .is_none(),
        "should copy nothing through the alias"
    );
}

#[test]
fn source_outside_the_writable_root_or_with_dot_segments_is_rejected() {
    let layout = layout();
    let root = layout.root();
    for source in [
        layout.output.join("example-bin"),
        root.join("release/../release/example-bin"),
        root.join("release/./example-bin"),
        PathBuf::from("relative/example-bin"),
    ] {
        let error = copy_artifact(&source, &root, &layout.output, "ts-example-asset", LIMIT)
            .expect_err("should reject the source path");
        assert!(
            format!("{error:?}").contains("source"),
            "should reject {source:?} as a source path"
        );
    }
}
