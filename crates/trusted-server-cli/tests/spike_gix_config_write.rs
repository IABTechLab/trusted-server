//! Spike: prove that gix-config::File can read and write
//! <repo>/.git/config so that `ts dev install-hooks` can persist
//! core.hooksPath without a subprocess. Locks the read/write APIs
//! for Phase 6.
//!
//! No shell, no `git` binary. The repo is created via gix::init;
//! the config file is read and written via gix-config::File.

use std::fs;
use std::path::Path;

use tempfile::tempdir;

#[test]
fn write_core_hooks_path_via_gix_config_persists_to_disk() {
    let temp = tempdir().expect("should create tempdir");
    let repo_path = temp.path();
    let _repo = gix::init(repo_path).expect("should init gix repo");

    set_local_config_value(repo_path, "core.hooksPath", ".githooks")
        .expect("should write core.hooksPath via gix-config");

    // Read it back via gix-config.
    let value = read_local_config_value(repo_path, "core.hooksPath")
        .expect("should read core.hooksPath back");
    assert_eq!(value.as_deref(), Some(".githooks"));

    // Sanity: the on-disk .git/config shows the section and key.
    let on_disk = fs::read_to_string(repo_path.join(".git/config"))
        .expect("should read .git/config from disk");
    assert!(
        on_disk.contains("[core]") && on_disk.contains("hooksPath"),
        "should contain core/hooksPath: {on_disk:?}"
    );
}

#[test]
fn read_local_config_value_returns_none_when_unset() {
    let temp = tempdir().expect("should create tempdir");
    let repo_path = temp.path();
    let _repo = gix::init(repo_path).expect("should init gix repo");

    let value = read_local_config_value(repo_path, "core.hooksPath")
        .expect("should read core.hooksPath (returning None)");
    assert!(value.is_none(), "unset value reads as None: {value:?}");
}

// === Conceptual operations under test ===

/// `dotted_key` is a `section.key` form (e.g., `core.hooksPath`).
/// Subsections are not needed for `core.hooksPath`; the production
/// install-hooks code only ever sets that one key.
fn set_local_config_value(
    repo_path: &Path,
    dotted_key: &str,
    value: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    use gix::bstr::BStr;
    use gix_config::File;

    let config_path = repo_path.join(".git").join("config");

    // Read existing file; start empty if missing.
    let mut file = match File::from_path_no_includes(config_path.clone(), gix_config::Source::Local)
    {
        Ok(f) => f,
        Err(_) => File::new(gix_config::file::Metadata::from(gix_config::Source::Local)),
    };

    let value_bstr: &BStr = value.into();
    // `set_raw_value` takes a dotted `AsKey` and clones the value
    // name internally — avoids tying the File's invariant 'event
    // lifetime to a short-lived borrow.
    file.set_raw_value(dotted_key, value_bstr)?;

    // Serialize and write atomically (temp file in the same dir, then rename).
    let serialized = file.to_bstring();
    write_atomic(&config_path, serialized.as_slice())?;
    Ok(())
}

fn read_local_config_value(
    repo_path: &Path,
    dotted_key: &str,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    use gix_config::File;

    let config_path = repo_path.join(".git").join("config");
    let file = match File::from_path_no_includes(config_path, gix_config::Source::Local) {
        Ok(f) => f,
        Err(_) => return Ok(None),
    };
    Ok(file
        .raw_value(dotted_key)
        .ok()
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned()))
}

/// Write `content` to `path` atomically: write a sibling temp file,
/// then rename over the target (atomic on the same filesystem).
fn write_atomic(path: &Path, content: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
    let dir = path.parent().ok_or("config path has no parent directory")?;
    let tmp = dir.join(format!("config.tmp.{}", std::process::id()));
    fs::write(&tmp, content)?;
    fs::rename(&tmp, path)?;
    Ok(())
}
