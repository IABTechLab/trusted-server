//! Object-only copies anchored to held descriptors, never input Git config.

use std::collections::BTreeSet;
use std::ffi::{CStr, CString};
use std::fs::{self, File, Metadata};
use std::io::{Read as _, Write as _};
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Component, Path};

use error_stack::Report;

use super::{GitCollection, material_error};
use crate::error::ReleaseError;

const RAW_LIMIT: u64 = 512 * 1024 * 1024;
const ENTRY_LIMIT: usize = 100_000;

fn stamp(metadata: &Metadata) -> (u64, u64, u64, u64, i64, i64, i64, i64) {
    (
        metadata.dev(),
        metadata.ino(),
        metadata.nlink(),
        metadata.len(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec(),
    )
}

/// Opens each ancestor without following a symlink or accepting relative syntax.
fn anchor(path: &Path) -> Result<File, Report<ReleaseError>> {
    if !path.is_absolute() || path.as_os_str().as_bytes().contains(&0) {
        return Err(material_error(
            "Git object root must be an absolute canonical path",
        ));
    }
    let mut directory = File::open("/").map_err(material_error)?;
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => {
                directory = open_at(&directory, name.as_bytes(), true)?;
            }
            _ => return Err(material_error("noncanonical Git object root")),
        }
    }
    Ok(directory)
}

fn open_at(parent: &File, name: &[u8], directory: bool) -> Result<File, Report<ReleaseError>> {
    let name = CString::new(name).map_err(material_error)?;
    let flags = libc::O_RDONLY
        | libc::O_NOFOLLOW
        | libc::O_NONBLOCK
        | libc::O_CLOEXEC
        | if directory { libc::O_DIRECTORY } else { 0 };
    // The parent descriptor stays alive throughout openat, and name is NUL terminated.
    let descriptor = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags) };
    if descriptor < 0 {
        return Err(material_error(std::io::Error::last_os_error()));
    }
    // A successful openat transfers one fresh descriptor to this File.
    Ok(unsafe { File::from_raw_fd(descriptor) })
}

struct Directory(*mut libc::DIR);

impl Drop for Directory {
    fn drop(&mut self) {
        // fdopendir returned this stream, which is closed exactly once.
        unsafe { libc::closedir(self.0) };
    }
}

#[cfg(target_os = "macos")]
fn errno() -> *mut libc::c_int {
    // libc exposes the calling thread's errno location.
    unsafe { libc::__error() }
}

#[cfg(target_os = "linux")]
fn errno() -> *mut libc::c_int {
    // libc exposes the calling thread's errno location.
    unsafe { libc::__errno_location() }
}

fn names(
    directory: &File,
    collection: &GitCollection<'_>,
) -> Result<Vec<String>, Report<ReleaseError>> {
    // A duplicate keeps fdopendir ownership separate from the held anchor.
    let descriptor = unsafe { libc::fcntl(directory.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
    if descriptor < 0 {
        return Err(material_error(std::io::Error::last_os_error()));
    }
    // A successful fcntl returns a fresh owned descriptor.
    let owned = unsafe { OwnedFd::from_raw_fd(descriptor) };
    // fdopendir takes ownership only on success.
    let stream = unsafe { libc::fdopendir(owned.as_raw_fd()) };
    if stream.is_null() {
        return Err(material_error(std::io::Error::last_os_error()));
    }
    std::mem::forget(owned);
    let stream = Directory(stream);
    let mut output = Vec::new();
    let mut seen = BTreeSet::new();
    loop {
        collection.check()?;
        // Reset errno before readdir so EOF can be distinguished from failure.
        unsafe { *errno() = 0 };
        // The stream remains owned until all returned name bytes are copied.
        let entry = unsafe { libc::readdir(stream.0) };
        if entry.is_null() {
            // readdir sets errno on error and leaves the cleared value at EOF.
            if unsafe { *errno() } != 0 {
                return Err(material_error(std::io::Error::last_os_error()));
            }
            break;
        }
        // readdir supplies a NUL-terminated d_name valid until its next call.
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if matches!(name, b"." | b"..") {
            continue;
        }
        let name = std::str::from_utf8(name)
            .map_err(material_error)?
            .to_owned();
        if !seen.insert(name.clone()) || output.len() >= ENTRY_LIMIT {
            return Err(material_error(
                "Git object directory repeats names or exceeds entry limit",
            ));
        }
        output.push(name);
    }
    output.sort();
    Ok(output)
}

fn hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

struct CopyBudget {
    bytes: u64,
    entries: usize,
}

fn copy_file(
    parent: &File,
    name: &str,
    destination: &Path,
    budget: &mut CopyBudget,
    collection: &GitCollection<'_>,
) -> Result<(), Report<ReleaseError>> {
    collection.check()?;
    let mut input = open_at(parent, name.as_bytes(), false)?;
    let before = input.metadata().map_err(material_error)?;
    if !before.is_file() || before.nlink() != 1 || before.len() > RAW_LIMIT - budget.bytes {
        return Err(material_error(
            "Git object input must be a bounded regular single-link file",
        ));
    }
    budget.entries += 1;
    if budget.entries > ENTRY_LIMIT {
        return Err(material_error("Git object input exceeds entry limit"));
    }
    let mut output = File::create_new(destination).map_err(material_error)?;
    let mut copied = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        collection.check()?;
        let count = input.read(&mut buffer).map_err(material_error)?;
        if count == 0 {
            break;
        }
        copied += count as u64;
        if copied > before.len() {
            return Err(material_error("Git object input grew"));
        }
        output.write_all(&buffer[..count]).map_err(material_error)?;
    }
    if copied != before.len() || stamp(&before) != stamp(&input.metadata().map_err(material_error)?)
    {
        return Err(material_error("Git object input changed while copying"));
    }
    budget.bytes += copied;
    Ok(())
}

/// Copies only canonical loose and pack objects under independent raw limits.
pub(super) fn copy(
    input: &Path,
    destination: &Path,
    collection: &GitCollection<'_>,
) -> Result<(), Report<ReleaseError>> {
    collection.check()?;
    let root = anchor(input)?;
    let before = root.metadata().map_err(material_error)?;
    let mut budget = CopyBudget {
        bytes: 0,
        entries: 0,
    };
    for name in names(&root, collection)? {
        if !(hex(&name, 2) || name == "pack" || name == "info") {
            return Err(material_error("unsupported Git object input entry"));
        }
        let directory = open_at(&root, name.as_bytes(), true)?;
        let directory_before = directory.metadata().map_err(material_error)?;
        let children = names(&directory, collection)?;
        fs::create_dir(destination.join(&name)).map_err(material_error)?;
        let mut pack = BTreeSet::new();
        let mut index = BTreeSet::new();
        for child in children {
            if name == "info" {
                return Err(material_error("Git object info/indirection is forbidden"));
            }
            if name == "pack" {
                let (base, extension) = child
                    .rsplit_once('.')
                    .ok_or_else(|| material_error("noncanonical Git pack name"))?;
                if !base.strip_prefix("pack-").is_some_and(|oid| hex(oid, 40))
                    || !matches!(extension, "pack" | "idx" | "rev")
                {
                    return Err(material_error("unsupported Git pack entry"));
                }
                if extension == "pack" {
                    pack.insert(base.to_owned());
                }
                if extension == "idx" {
                    index.insert(base.to_owned());
                }
            } else if !hex(&child, 38) {
                return Err(material_error("noncanonical loose Git object name"));
            }
            copy_file(
                &directory,
                &child,
                &destination.join(&name).join(&child),
                &mut budget,
                collection,
            )?;
        }
        if name == "pack" && pack != index {
            return Err(material_error("Git pack and index sets differ"));
        }
        if stamp(&directory_before) != stamp(&directory.metadata().map_err(material_error)?) {
            return Err(material_error("Git object directory changed while copying"));
        }
    }
    if stamp(&before) != stamp(&root.metadata().map_err(material_error)?) {
        return Err(material_error("Git object root changed while copying"));
    }
    collection.check()
}
