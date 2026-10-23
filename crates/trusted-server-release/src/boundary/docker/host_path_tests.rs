//! Pure tests of the container-to-host path inverse; no process is started.

use std::fs;
use std::path::{Path, PathBuf};

use super::*;

struct Layout {
    _directory: tempfile::TempDir,
    roots: BoundaryRoots,
    base: PathBuf,
}

fn layout() -> Layout {
    let directory = tempfile::tempdir().expect("should create layout directory");
    let base = fs::canonicalize(directory.path()).expect("should canonicalize layout");
    for name in ["source", "tools", "work-a", "work-b"] {
        fs::create_dir(base.join(name)).expect("should create root directory");
    }
    let roots = BoundaryRoots::new(
        &[base.join("source")],
        &[base.join("tools")],
        &[base.join("work-a"), base.join("work-b")],
    )
    .expect("should build roots");
    Layout {
        _directory: directory,
        roots,
        base,
    }
}

#[test]
fn writable_container_paths_map_back_to_their_host_roots() {
    let layout = layout();
    for (name, container) in [
        ("work-a", "/boundary/writable/0"),
        ("work-b", "/boundary/writable/1"),
    ] {
        let host = layout.base.join(name);
        assert_eq!(
            host_path(&layout.roots, Path::new(container)).expect("should map the root itself"),
            host,
            "should map {container} to its root"
        );
        let nested = layout.base.join(name).join("release/example.wasm");
        let forward = container_path(&layout.roots, &nested).expect("should map forward");
        assert_eq!(
            host_path(&layout.roots, &forward).expect("should map the nested file back"),
            nested,
            "should invert container_path for {name}"
        );
    }
}

#[test]
fn immutable_tool_unknown_and_malformed_container_paths_are_rejected() {
    let layout = layout();
    for text in [
        "/boundary/immutable/0/file",
        "/boundary/tool/0/bin/cargo",
        "/boundary/writable/2/file",
        "/boundary/writable/x/file",
        "/boundary/writable/00/file",
        "/boundary/writable/+1/file",
        "/boundary/writable/+0/file",
        "/boundary/writable/-0/file",
        "/boundary/writable/1e0/file",
        "/boundary/writable/１/file",
        "/boundary/writable/0/../1/file",
        "/boundary/writable/0/./file",
        "/boundary/writable",
        "/boundary/writable/",
        "/boundaryx/writable/0/file",
        "/boundary/writable/0file",
        "boundary/writable/0/file",
        "/etc/passwd",
        "",
    ] {
        assert!(
            host_path(&layout.roots, Path::new(text)).is_err(),
            "should reject container path {text:?}"
        );
    }
}
