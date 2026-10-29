//! Extracts authenticated originals into fresh private state, without execution.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{self, BufReader, Read, Seek as _, Write as _};
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

use error_stack::Report;
use flate2::bufread::GzDecoder;
use sha2::{Digest as _, Sha256};
use tempfile::TempDir;

use super::{VerifiedOriginal, material_error};
use crate::documents::{Role, hash_bounded_with_check, read_bounded};
use crate::error::ReleaseError;

#[derive(Clone, Copy)]
pub(super) struct Selection<'a> {
    pub(super) root: &'a str,
    pub(super) index_alias: bool,
    pub(super) node_tar_timestamps: bool,
    pub(super) local_pax: bool,
    /// Accepts the two encodings `cargo package` itself writes into `.crate`
    /// archives: a GNU long-name record before a member whose path exceeds
    /// the 100-byte header field, and (in older Cargo releases) the regular
    /// file or directory type bits inside the mode field.
    pub(super) cargo_package: bool,
}

mod compatibility;

const DECODED_LIMIT: u64 = 512 * 1024 * 1024;
// The approved npm lock includes a 110,275,248-byte optional native member.
// Keep the compressed-original and total decoded limits independent.
const MEMBER_LIMIT: u64 = 128 * 1024 * 1024;
const ENTRY_LIMIT: usize = 100_000;
const DEPTH_LIMIT: usize = 64;
const PATH_LIMIT: usize = 4096;

#[derive(PartialEq, Eq)]
struct MemberFacts {
    size: u64,
    mode: u32,
    sha256: String,
}

/// A privately extracted original, without installation or execution authority.
pub(crate) struct ExtractedOriginal {
    original: VerifiedOriginal,
    directory: TempDir,
    root: PathBuf,
    files: BTreeMap<String, MemberFacts>,
    root_name: String,
    directories: BTreeSet<String>,
}

impl ExtractedOriginal {
    /// Returns a rechecked regular member's original digest, length and mode.
    ///
    /// # Errors
    ///
    /// Returns an error if the tree changed or the member was not recorded.
    pub(super) fn member_facts(
        &self,
        relative: &str,
    ) -> Result<(String, u64, u32), Report<ReleaseError>> {
        self.verify_unchanged()?;
        let facts = self
            .files
            .get(&format!("{}/{relative}", self.root_name))
            .ok_or_else(|| {
                material_error("required native tool is not an authenticated archive member")
            })?;
        Ok((facts.sha256.clone(), facts.size, facts.mode))
    }
    /// Rejoins the entire private extraction before it is consumed again.
    ///
    /// # Errors
    ///
    /// Returns an error for a changed, missing or added member, unsafe type,
    /// changed executable mode, or an expired attempt.
    pub(crate) fn verify_unchanged(&self) -> Result<(), Report<ReleaseError>> {
        self.original.check()?;
        let mut remaining: BTreeSet<_> = self.files.keys().cloned().collect();
        let mut remaining_directories = self.directories.clone();
        let mut pending = vec![self.root_name.clone()];
        while let Some(directory) = pending.pop() {
            self.original.check()?;
            let metadata = fs::symlink_metadata(self.directory.path().join(&directory))
                .map_err(material_error)?;
            if !metadata.is_dir() {
                return Err(material_error(
                    "extracted directory changed type or became an alias",
                ));
            }
            remaining_directories.remove(&directory);
            let entries =
                fs::read_dir(self.directory.path().join(&directory)).map_err(material_error)?;
            for entry in entries {
                self.original.check()?;
                let entry = entry.map_err(material_error)?;
                let name = entry
                    .file_name()
                    .into_string()
                    .map_err(|_| material_error("extracted member is not UTF-8"))?;
                let key = format!("{directory}/{name}");
                let metadata = fs::symlink_metadata(entry.path()).map_err(material_error)?;
                if metadata.is_dir() {
                    if !self.directories.contains(&key) {
                        return Err(material_error("unrecorded extracted directory appeared"));
                    }
                    pending.push(key);
                } else {
                    let expected = self
                        .files
                        .get(&key)
                        .ok_or_else(|| material_error("unrecorded extracted member appeared"))?;
                    if !metadata.is_file()
                        || metadata.len() != expected.size
                        || metadata.permissions().mode() & 0o7777 != 0o600 | (expected.mode & 0o111)
                    {
                        return Err(material_error(
                            "extracted member type, size or permissions changed",
                        ));
                    }
                    let raw = hash_bounded_with_check(&entry.path(), MEMBER_LIMIT, &|| {
                        self.original.check()
                    })?;
                    if raw.sha256 != expected.sha256
                        || raw.size != expected.size
                        || raw.mode & 0o7777 != 0o600 | (expected.mode & 0o111)
                    {
                        return Err(material_error("extracted member bytes changed"));
                    }
                    remaining.remove(&key);
                }
            }
        }
        if !remaining.is_empty() || !remaining_directories.is_empty() {
            return Err(material_error("recorded extracted member is missing"));
        }
        self.original.check()
    }

    /// Copies only the retained authenticated compressed original.
    ///
    /// # Errors
    ///
    /// Returns an error if the retained original changed or the copy fails.
    pub(super) fn copy_original(
        &mut self,
        destination: &mut impl io::Write,
    ) -> Result<(), Report<ReleaseError>> {
        self.original.copy_verified(destination)
    }
    /// Returns the locked acquisition identity supplying this extracted tree.
    pub(super) fn identity(&self) -> &str {
        self.original.identity()
    }

    /// Reads a bounded manifest input and rejoins its extraction-time digest.
    ///
    /// # Errors
    ///
    /// Returns an error for an unrecorded, changed, linked or oversized input,
    /// or when the original's deadline/cancellation boundary has expired.
    pub(super) fn read_member_checked(
        &self,
        relative: &str,
    ) -> Result<Vec<u8>, Report<ReleaseError>> {
        self.original.check()?;
        let key = format!("{}/{relative}", self.root_name);
        let expected = self
            .files
            .get(&key)
            .ok_or_else(|| material_error("manifest input is not a recorded archive member"))?;
        let raw = read_bounded(&self.root.join(relative), Role::Metadata)?;
        if raw.sha256 != expected.sha256 || raw.bytes.len() as u64 != expected.size {
            return Err(material_error("extracted manifest input changed"));
        }
        self.original.check()?;
        Ok(raw.bytes)
    }

    /// Checks recorded manifest-input presence without allowing a later omission.
    ///
    /// # Errors
    ///
    /// Returns an error if the selected original input was removed or changed,
    /// an unrecorded input appeared, or the attempt expired.
    pub(super) fn contains_member_checked(
        &self,
        relative: &str,
    ) -> Result<bool, Report<ReleaseError>> {
        self.original.check()?;
        let key = format!("{}/{relative}", self.root_name);
        if self.files.contains_key(&key) {
            self.read_member_checked(relative)?;
            return Ok(true);
        }
        match fs::symlink_metadata(self.root.join(relative)) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            _ => Err(material_error(
                "unrecorded or invalid manifest input appeared",
            )),
        }
    }
    /// Returns the privately owned package root for later preparation checks.
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    /// Returns the approved source identity of the retained original.
    pub(crate) fn source_commit(&self) -> &str {
        self.original.source_commit()
    }
}

/// Checks cancellation and decoded expansion on every bounded read.
struct Decoded<'a, R> {
    reader: R,
    original: &'a VerifiedOriginal,
    count: u64,
}

impl<R: Read> Read for Decoded<'_, R> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.original
            .check()
            .map_err(|_| io::Error::other("archive cancelled or timed out"))?;
        let maximum = bytes.len().min(64 * 1024);
        let count = self.reader.read(&mut bytes[..maximum])?;
        self.count += count as u64;
        if self.count > DECODED_LIMIT {
            return Err(io::Error::other("archive exceeds decoded byte limit"));
        }
        Ok(count)
    }
}

fn field(bytes: &[u8]) -> Result<&str, Report<ReleaseError>> {
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    if bytes[end..].iter().any(|byte| *byte != 0) {
        return Err(material_error(
            "TAR name has hidden bytes after its terminator",
        ));
    }
    std::str::from_utf8(&bytes[..end]).map_err(material_error)
}

/// Uses raw fields because [`tar::Entry::path_bytes`] normalizes backslashes.
fn raw_name(
    header: &tar::Header,
    selection: &Selection<'_>,
) -> Result<String, Report<ReleaseError>> {
    let name = field(&header.as_old().name)?;
    match header.as_ustar() {
        Some(ustar) => {
            let prefix =
                if selection.node_tar_timestamps && ustar.prefix.iter().any(|byte| *byte != 0) {
                    compatibility::timestamp_prefix(header)?
                } else {
                    field(&ustar.prefix)?
                };
            Ok(if prefix.is_empty() {
                name.to_owned()
            } else {
                format!("{prefix}/{name}")
            })
        }
        None => Ok(name.to_owned()),
    }
}

fn member_name(
    raw: &str,
    directory: bool,
    selection: &Selection<'_>,
) -> Result<String, Report<ReleaseError>> {
    if raw.is_empty()
        || raw.len() > PATH_LIMIT
        || !raw.is_ascii()
        || raw
            .bytes()
            .any(|byte| byte.is_ascii_control() || matches!(byte, b'\\' | b':'))
    {
        return Err(material_error("TAR path is not a bounded portable name"));
    }
    let name = if directory {
        raw.strip_suffix('/').unwrap_or(raw)
    } else {
        raw
    };
    let name = if !directory && selection.index_alias && name == "package/./dist/index.js" {
        "package/dist/index.js"
    } else {
        name
    };
    let parts: Vec<&str> = name.split('/').collect();
    if parts.len() > DEPTH_LIMIT
        || parts
            .iter()
            .any(|part| part.is_empty() || *part == "." || *part == "..")
        || parts[0] != selection.root
        || (!directory && parts.len() == 1)
    {
        return Err(material_error("TAR path leaves its exact locked root"));
    }
    Ok(name.to_owned())
}

struct State {
    directory: TempDir,
    spellings: BTreeSet<String>,
    portable: BTreeMap<String, String>,
    files: BTreeMap<String, MemberFacts>,
    first_spelling: BTreeMap<String, String>,
    directories: BTreeSet<String>,
}

impl State {
    fn portable_name(&mut self, name: &str) -> Result<(), Report<ReleaseError>> {
        let mut prefix = String::new();
        for part in name.split('/') {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(part);
            let lower = prefix.to_ascii_lowercase();
            if let Some(previous) = self.portable.insert(lower, prefix.clone())
                && previous != prefix
            {
                return Err(material_error("TAR has case-aliased paths"));
            }
        }
        Ok(())
    }

    fn entry<R: Read>(
        &mut self,
        mut entry: tar::Entry<'_, R>,
        selection: &Selection<'_>,
        long_name: Option<String>,
    ) -> Result<(), Report<ReleaseError>> {
        let header = entry.header();
        let directory = header.entry_type().is_dir();
        if !directory && !header.entry_type().is_file() {
            return Err(material_error(
                "TAR member is not a regular file or directory",
            ));
        }
        let raw = match long_name {
            Some(long) => compatibility::join_long_name(long, &raw_name(header, selection)?)?,
            None => raw_name(header, selection)?,
        };
        let name = member_name(&raw, directory, selection)?;
        if !self.spellings.insert(raw.clone()) {
            return Err(material_error("TAR repeats a member spelling"));
        }
        self.portable_name(&name)?;
        let mode = header.mode().map_err(material_error)?;
        let mode = if selection.cargo_package {
            compatibility::strip_member_type(mode, directory)?
        } else {
            mode
        };
        let size = entry.size();
        if mode & !0o777 != 0 || (directory && size != 0) {
            return Err(material_error(format!(
                "TAR member `{name}` has privileged mode {mode:o} or invalid size {size}",
            )));
        }
        if size > MEMBER_LIMIT {
            return Err(material_error(format!(
                "TAR member `{name}` size {size} exceeds member byte limit {MEMBER_LIMIT}",
            )));
        }
        let path = self.directory.path().join(&name);
        if directory {
            if !self.directories.insert(name) {
                return Err(material_error("TAR repeats a directory path"));
            }
            fs::create_dir_all(&path).map_err(material_error)?;
            return Ok(());
        }
        let duplicate = self.files.contains_key(&name);
        if duplicate
            && !(selection.index_alias
                && name == "package/dist/index.js"
                && matches!(
                    (
                        self.first_spelling.get(&name).map(String::as_str),
                        raw.as_str()
                    ),
                    (Some("package/./dist/index.js"), "package/dist/index.js")
                        | (Some("package/dist/index.js"), "package/./dist/index.js")
                ))
        {
            return Err(material_error("TAR has an unreviewed duplicate path"));
        }
        let mut file = if duplicate {
            None
        } else {
            fs::create_dir_all(
                path.parent()
                    .ok_or_else(|| material_error("member has no parent"))?,
            )
            .map_err(material_error)?;
            Some(
                File::options()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&path)
                    .map_err(material_error)?,
            )
        };
        let mut hasher = Sha256::new();
        let mut count = 0_u64;
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let read = entry.read(&mut buffer).map_err(material_error)?;
            if read == 0 {
                break;
            }
            count += read as u64;
            if count > size {
                return Err(material_error("TAR member exceeds declared size"));
            }
            hasher.update(&buffer[..read]);
            if let Some(file) = &mut file {
                file.write_all(&buffer[..read]).map_err(material_error)?;
            }
        }
        if count != size {
            return Err(material_error("TAR member is truncated"));
        }
        let facts = MemberFacts {
            size,
            mode,
            sha256: hex::encode(hasher.finalize()),
        };
        if duplicate {
            if self.files.get(&name) != Some(&facts) {
                return Err(material_error("reviewed TAR aliases differ"));
            }
        } else {
            // Only executable bits carry across; ownership and ambient write access do not.
            file.as_ref()
                .expect("should have exclusively created member")
                .set_permissions(fs::Permissions::from_mode(0o600 | (mode & 0o111)))
                .map_err(material_error)?;
            self.first_spelling.insert(name.clone(), raw);
            self.files.insert(name, facts);
        }
        Ok(())
    }
}

pub(super) fn extract(
    original: VerifiedOriginal,
    selection: Selection<'_>,
) -> Result<ExtractedOriginal, Report<ReleaseError>> {
    extract_bounded(original, selection, ENTRY_LIMIT)
}

fn extract_bounded(
    mut original: VerifiedOriginal,
    selection: Selection<'_>,
    entry_limit: usize,
) -> Result<ExtractedOriginal, Report<ReleaseError>> {
    let mut compressed = tempfile::tempfile().map_err(material_error)?;
    original.copy_verified(&mut compressed)?;
    compressed.rewind().map_err(material_error)?;
    let decoder = GzDecoder::new(BufReader::new(compressed));
    let decoded = Decoded {
        reader: decoder,
        original: &original,
        count: 0,
    };
    let mut archive = tar::Archive::new(decoded);
    let mut state = State {
        directory: tempfile::tempdir().map_err(material_error)?,
        spellings: BTreeSet::new(),
        portable: BTreeMap::new(),
        files: BTreeMap::new(),
        first_spelling: BTreeMap::new(),
        directories: BTreeSet::new(),
    };
    let mut pending_pax: Option<compatibility::LocalPax> = None;
    let mut pending_long: Option<String> = None;
    for (index, entry) in archive
        .entries()
        .map_err(material_error)?
        .raw(true)
        .enumerate()
    {
        if index >= entry_limit {
            return Err(material_error("TAR exceeds entry count limit"));
        }
        let mut entry = entry.map_err(material_error)?;
        if entry.header().entry_type().is_gnu_longname() {
            if !selection.cargo_package || pending_long.is_some() || pending_pax.is_some() {
                return Err(material_error(
                    "unreviewed, consecutive or mixed GNU long-name record",
                ));
            }
            pending_long = Some(compatibility::read_long_name(&mut entry, PATH_LIMIT)?);
            continue;
        }
        if entry.header().entry_type().as_byte() == b'x' {
            if pending_long.is_some() {
                return Err(material_error(
                    "GNU long-name record must immediately precede its member",
                ));
            }
            if !selection.local_pax || pending_pax.is_some() || entry.size() > 64 * 1024 {
                return Err(material_error(
                    "unreviewed, consecutive or oversized local PAX header",
                ));
            }
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).map_err(material_error)?;
            let pax = compatibility::parse_pax(&bytes)?;
            if raw_name(entry.header(), &selection)? != pax.header_name()
                || entry.header().mode().map_err(material_error)? & !0o777 != 0
                || !field(&entry.header().as_old().linkname)?.is_empty()
            {
                return Err(material_error("unsafe reviewed local PAX header"));
            }
            pending_pax = Some(pax);
            continue;
        }
        if let Some(pax) = pending_pax.take() {
            if !entry.header().entry_type().is_file() {
                return Err(material_error(
                    "local PAX must immediately precede a regular file",
                ));
            }
            pax.join(
                &raw_name(entry.header(), &selection)?,
                entry.header().size().map_err(material_error)?,
            )?;
        }
        state.entry(entry, &selection, pending_long.take())?;
    }
    if pending_pax.is_some() {
        return Err(material_error("dangling local PAX header"));
    }
    if pending_long.is_some() {
        return Err(material_error("dangling GNU long-name record"));
    }
    if state.spellings.contains("package/./dist/index.js")
        && !state.spellings.contains("package/dist/index.js")
    {
        return Err(material_error(
            "reviewed dot alias lacks its canonical partner",
        ));
    }
    // The parser consumed the first zero header. Require at least the second,
    // then only complete zero blocks through the checked gzip trailer.
    let mut decoded = archive.into_inner();
    let mut tail = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = decoded.read(&mut buffer).map_err(material_error)?;
        if count == 0 {
            break;
        }
        tail += count as u64;
        if buffer[..count].iter().any(|byte| *byte != 0) {
            return Err(material_error("TAR hides data after its terminator"));
        }
    }
    if tail < 512 || !tail.is_multiple_of(512) {
        return Err(material_error("TAR lacks complete terminator blocks"));
    }
    let mut compressed = decoded.reader.into_inner();
    if compressed.read(&mut buffer[..1]).map_err(material_error)? != 0 {
        return Err(material_error(
            "gzip has additional members or trailing bytes",
        ));
    }
    original.check()?;
    let root = state.directory.path().join(selection.root);
    if !root.is_dir() || state.files.is_empty() {
        return Err(material_error("archive has no package files"));
    }
    for file in state.files.keys() {
        let mut current = file.as_str();
        while let Some((parent, _)) = current.rsplit_once('/') {
            state.directories.insert(parent.to_owned());
            current = parent;
        }
    }
    Ok(ExtractedOriginal {
        root_name: selection.root.to_owned(),
        original,
        root,
        directory: state.directory,
        files: state.files,
        directories: state.directories,
    })
}

#[cfg(test)]
mod tests;
