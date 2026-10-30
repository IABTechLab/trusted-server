//! Filesystem helpers shared by `ts` subcommands.

use std::fs;
use std::io::Write as _;
use std::path::Path;

/// Writes `contents` to `path` atomically: a same-directory temp file is
/// written and fsynced, then renamed over the target, then the directory entry
/// is fsynced.
///
/// A plain `fs::write` truncates the destination before writing, so a full disk
/// or an interrupted run would leave the target empty or half-written. `rename`
/// within a directory is atomic, so a reader sees either the old file or the
/// complete new one. If any step fails, the temp file is removed on drop.
///
/// The temp file is created 0600. When the target already exists, its mode
/// bits are carried onto the replacement; a new file stays 0600. Because the
/// target is replaced by rename, its owner, group, ACLs and hard links are not
/// preserved, and a symlink at `path` is replaced by a regular file rather than
/// updating the file it points to.
///
/// # Errors
///
/// Returns the underlying I/O error when the temp file cannot be created,
/// written, synced, or renamed over `path`.
pub(crate) fn write_file_atomically(path: &Path, contents: &str) -> std::io::Result<()> {
    let directory = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));

    let mut temp = tempfile::Builder::new()
        .prefix(".ts-write-")
        .tempfile_in(directory)?;
    temp.write_all(contents.as_bytes())?;
    temp.as_file().sync_all()?;
    if let Ok(metadata) = fs::metadata(path) {
        temp.as_file().set_permissions(metadata.permissions())?;
    }
    temp.persist(path).map_err(|error| error.error)?;

    // Best-effort durability for the rename itself. Opening a directory handle
    // is not portable (Windows rejects it), and the content is already safely
    // on disk either way, so a failure here is not worth failing the command.
    let _ = fs::File::open(directory).and_then(|handle| handle.sync_all());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_file_atomically_removes_temp_file_when_rename_fails() {
        let temp = tempfile::TempDir::new().expect("should create temp dir");
        let target = temp.path().join("occupied");
        fs::create_dir(&target).expect("should create directory at target");
        fs::write(target.join("child"), "x").expect("should populate target directory");

        write_file_atomically(&target, "contents")
            .expect_err("should fail to rename over a non-empty directory");

        let entries: Vec<_> = fs::read_dir(temp.path())
            .expect("should list temp dir")
            .map(|entry| entry.expect("should read entry").file_name())
            .collect();
        assert_eq!(
            entries,
            vec![std::ffi::OsString::from("occupied")],
            "should leave no temp file behind after a failed write"
        );
    }
}
