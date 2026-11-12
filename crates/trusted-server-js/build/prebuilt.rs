//! Copy checked prebuilt inputs through held directory handles.
//!
//! Cargo owns `OUT_DIR` and its parents. Other processes able to write that
//! private build directory are outside this input-validation trust boundary.
//! Source parents may contain links; a held directory descriptor anchors all
//! reads, and reopening the path after copying must resolve to the same inode.

use std::collections::BTreeMap;
use std::ffi::{CStr, CString};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::os::fd::{AsRawFd as _, FromRawFd as _};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _};
use std::path::Path;

use sha2::{Digest as _, Sha256};

use super::bundle_set::{bundle_file_name, check_bundle_set, safe_module_id};
use super::inventory::{Inventory, MAX_BUNDLE, invalid};

#[derive(Debug, Eq, PartialEq)]
struct Identity {
    device: u64,
    inode: u64,
    size: u64,
    modified: (i64, i64),
    changed: (i64, i64),
    links: u64,
}

fn identity(file: &File) -> io::Result<Identity> {
    let metadata = file.metadata()?;
    Ok(Identity {
        device: metadata.dev(),
        inode: metadata.ino(),
        size: metadata.len(),
        modified: (metadata.mtime(), metadata.mtime_nsec()),
        changed: (metadata.ctime(), metadata.ctime_nsec()),
        links: metadata.nlink(),
    })
}

fn open_directory(path: &Path) -> io::Result<File> {
    // Components remove trailing separators and '.' without following links.
    let leaf_path = path.components().collect::<std::path::PathBuf>();
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&leaf_path)
        .map_err(|error| invalid(format!("cannot open directory {}: {error}", path.display())))
}

fn open_file(directory: &File, name: &str) -> io::Result<File> {
    let name = CString::new(name).map_err(|_| invalid("unsafe bundle filename"))?;
    // SAFETY: the descriptor and C string remain alive for the syscall; a
    // successful descriptor is immediately transferred into an owning File.
    let descriptor = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
        )
    };
    if descriptor < 0 {
        return Err(invalid(format!(
            "cannot open bundle {name:?}: {}",
            io::Error::last_os_error()
        )));
    }
    // SAFETY: openat returned a fresh descriptor owned solely by this File.
    let file = unsafe { File::from_raw_fd(descriptor) };
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.nlink() != 1 {
        return Err(invalid(format!(
            "bundle {name:?} must be a regular file with exactly one hard link"
        )));
    }
    Ok(file)
}

struct DirectoryStream(*mut libc::DIR);

impl Drop for DirectoryStream {
    fn drop(&mut self) {
        // SAFETY: this stream owns the successful fdopendir result.
        unsafe {
            libc::closedir(self.0);
        }
    }
}

fn names(directory: &File, maximum: usize) -> io::Result<Vec<String>> {
    // Opening '.' creates a separate enumeration offset for every scan.
    let dot = c".";
    // SAFETY: the descriptor and static C string are valid for this syscall.
    let descriptor = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            dot.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if descriptor < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: descriptor is fresh; ownership passes to the stream on success.
    let stream = unsafe { libc::fdopendir(descriptor) };
    if stream.is_null() {
        let error = io::Error::last_os_error();
        // SAFETY: fdopendir failed and did not take ownership.
        unsafe {
            libc::close(descriptor);
        }
        return Err(error);
    }
    let stream = DirectoryStream(stream);
    let mut result = Vec::new();
    loop {
        clear_errno();
        // SAFETY: the stream is owned, live, and used on this thread only.
        let entry = unsafe { libc::readdir(stream.0) };
        if entry.is_null() {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(0) {
                return Err(error);
            }
            break;
        }
        // SAFETY: readdir supplies a NUL-terminated name valid until next call.
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
        let name = name
            .to_str()
            .map_err(|_| invalid("bundle filename must be UTF-8"))?;
        if name != "." && name != ".." {
            if result.len() >= maximum {
                return Err(invalid(format!(
                    "unexpected extra bundle directory entry: {name}"
                )));
            }
            result.push(name.to_owned());
        }
    }
    result.sort();
    Ok(result)
}

fn clear_errno() {
    // SAFETY: these functions return the current thread's errno storage.
    #[cfg(target_os = "macos")]
    unsafe {
        *libc::__error() = 0;
    }
    #[cfg(target_os = "linux")]
    unsafe {
        *libc::__errno_location() = 0;
    }
}

fn snapshot(directory: &File, expected: &[String]) -> io::Result<BTreeMap<String, Identity>> {
    let mut snapshot = BTreeMap::new();
    let mut found = Vec::new();
    for name in names(directory, expected.len())? {
        let id = name
            .strip_prefix("tsjs-")
            .and_then(|stem| stem.strip_suffix(".js"))
            .filter(|id| safe_module_id(id))
            .ok_or_else(|| invalid(format!("unexpected or unsafe bundle filename: {name}")))?;
        let file = open_file(directory, &name)?;
        let identity = identity(&file)?;
        found.push((id.to_owned(), identity.size));
        snapshot.insert(name, identity);
    }
    check_bundle_set(expected, &found).map_err(invalid)?;
    Ok(snapshot)
}

fn prepare_private(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(path)?,
        Ok(_) => {
            return Err(invalid(
                "private bundle destination must be a directory, never a link",
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    fs::DirBuilder::new().mode(0o700).create(path)
}

fn copy_one(source: &mut File, destination: &Path, size: u64) -> io::Result<String> {
    let mut target = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(destination)?;
    let mut hasher = Sha256::new();
    let mut reader = source.take(size + 1);
    let mut buffer = [0; 64 * 1024];
    let mut length = 0u64;
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        length += count as u64;
        if length > size {
            return Err(invalid("inventory bundle size mismatch during copy"));
        }
        hasher.update(&buffer[..count]);
        target.write_all(&buffer[..count])?;
    }
    if length != size {
        return Err(invalid("inventory bundle size mismatch during copy"));
    }
    target.flush()?;
    Ok(hex::encode(hasher.finalize()))
}

pub(crate) fn copy_checked(
    expected: &[String],
    source_path: &Path,
    destination: &Path,
    inventory: &Inventory,
    mut hook: impl FnMut(&Path, &Path),
) -> io::Result<()> {
    let source = open_directory(source_path)?;
    let source_identity = identity(&source)?;
    let before = snapshot(&source, expected)?;
    prepare_private(destination)?;
    let mut digests = BTreeMap::new();
    for id in expected {
        let name = bundle_file_name(id);
        let mut file = open_file(&source, &name)?;
        if before.get(&name) != Some(&identity(&file)?) {
            return Err(invalid("source bundle identity changed before copy"));
        }
        let module = inventory
            .modules
            .iter()
            .find(|module| module.id == *id)
            .ok_or_else(|| invalid(format!("inventory missing bundle {name}")))?;
        let size = module.size;
        if size > MAX_BUNDLE {
            return Err(invalid("bundle exceeds 32 MiB"));
        }
        let digest = copy_one(&mut file, &destination.join(&name), size)?;
        if module.sha256 != digest {
            return Err(invalid(format!("inventory bundle digest mismatch: {name}")));
        }
        if before.get(&name) != Some(&identity(&file)?) {
            return Err(invalid("source bundle changed during copy"));
        }
        digests.insert(name, (size, digest));
    }
    // Crate-private deterministic race injection; production supplies a no-op.
    hook(source_path, destination);
    if snapshot(&source, expected)? != before
        || identity(&source)? != source_identity
        || identity(&open_directory(source_path)?)? != source_identity
    {
        return Err(invalid(
            "source directory or bundle identity changed during copy",
        ));
    }
    let private = open_directory(destination)?;
    snapshot(&private, expected)?;
    for (name, (size, digest)) in digests {
        let mut file = open_file(&private, &name)?;
        let mut bytes = Vec::new();
        (&mut file).take(size + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 != size || hex::encode(Sha256::digest(&bytes)) != digest {
            return Err(invalid(
                "private copied bundle does not match inventory input bytes",
            ));
        }
    }
    Ok(())
}

pub(crate) fn inventory_outside(source: &Path, inventory: &Path) -> io::Result<()> {
    // Comparing canonical paths prevents aliases from placing metadata inside
    // the exact bundle-only directory. The bytes are read separately once.
    if inventory
        .canonicalize()?
        .starts_with(source.canonicalize()?)
    {
        return Err(invalid(
            "inventory must be outside the exact bundle directory",
        ));
    }
    // Reject a link at the inventory leaf as well as nonregular metadata.
    let metadata = fs::symlink_metadata(inventory)?;
    if !metadata.is_file() || metadata.nlink() != 1 || inventory.as_os_str().as_bytes().is_empty() {
        return Err(invalid("inventory must be a regular single-link file"));
    }
    Ok(())
}
