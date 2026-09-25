//! Explicit browser trust operations. Linux records imports before changing NSS.

use std::fs::{self, File, OpenOptions};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::Path;
use std::process::{Command, Output};

use error_stack::{Report, ResultExt as _};

use super::ca::CA_COMMON_NAME;
use crate::output;

/// Failures that must not be treated as successful trust removal.
#[derive(Debug, derive_more::Display)]
pub enum TrustError {
    /// Trust metadata or certificate files could not be accessed.
    #[display("cannot access browser trust files")]
    Io,
    /// A command failed, including a failed database query.
    #[display("browser trust command failed")]
    Command,
    /// A managed nickname now identifies a different certificate.
    #[display("managed certificate identity conflict; refusing to change trust")]
    #[cfg(target_os = "linux")]
    Identity,
    /// The saved destinations cannot be safely interpreted.
    #[display("invalid managed trust record; preserve it and recover trust before rotating")]
    #[cfg(target_os = "linux")]
    State,
    /// Another CA command is running.
    #[display("another CA or NSS trust operation is running; retry after it exits")]
    Busy,
}

impl core::error::Error for TrustError {}

type Result<T> = core::result::Result<T, Report<TrustError>>;

/// Locks CA operations until the returned file is dropped. Never waits for a lock.
///
/// # Errors
/// Returns an I/O or lock error without changing CA material.
pub(super) fn lock(ca_dir: &Path) -> Result<File> {
    fs::create_dir_all(ca_dir).change_context(TrustError::Io)?;
    lock_file(&ca_dir.join("ca-operation.lock"))
}

/// Opens a process-scoped, nonblocking lock without creating its parent directory.
/// Returns an I/O or contention error before trust is changed.
fn lock_file(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .change_context(TrustError::Io)?;
    match file.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => return Err(Report::new(TrustError::Busy)),
        Err(err) => return Err(Report::new(err).change_context(TrustError::Io)),
    }
    Ok(file)
}

/// Runs certutil without shell interpretation and captures failures.
fn certutil(args: &[&str]) -> Result<Output> {
    let tool = which::which("certutil").change_context(TrustError::Command).attach(
        "install NSS tools: Debian/Ubuntu libnss3-tools, Fedora nss-tools, Arch nss, macOS brew install nss; no packages are installed automatically",
    )?;
    checked(Command::new(tool).args(args))
}

fn checked(command: &mut Command) -> Result<Output> {
    let out = command.output().change_context(TrustError::Command)?;
    if !out.status.success() {
        return Err(Report::new(TrustError::Command).attach(format!(
            "{command:?}: {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    Ok(out)
}

/// Imports only into a freshly created, disposable Firefox profile.
///
/// # Errors
/// Returns an error if NSS initialization or import fails.
pub(super) fn import_firefox(profile: &Path, cert: &Path) -> Result<()> {
    let db = format!("sql:{}", profile.display());
    certutil(&["-N", "--empty-password", "-d", &db])?;
    certutil(&[
        "-A",
        "-n",
        CA_COMMON_NAME,
        "-t",
        "C,,",
        "-i",
        &cert.to_string_lossy(),
        "-d",
        &db,
    ])?;
    Ok(())
}

/// Platform trust contract: validate CA generation, install trust, and confirm removal.
#[cfg(target_os = "linux")]
pub(super) use linux::{ensure_can_generate, install, uninstall};

#[cfg(target_os = "linux")]
mod linux {
    use std::hash::{DefaultHasher, Hash as _, Hasher as _};
    use std::io::Write as _;
    use std::path::PathBuf;

    use error_stack::ResultExt as _;
    use serde::{Deserialize, Serialize};

    use super::{File, Path, Report, Result, TrustError, certutil, fs, lock_file, output};

    const RECORD: &str = "managed-nss-trust.json";

    #[derive(Debug, Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    struct Destination {
        database: PathBuf,
        nickname: String,
        certificate: Vec<u8>,
    }

    fn read_record(ca_dir: &Path) -> Result<Vec<Destination>> {
        let bytes = match fs::read(ca_dir.join(RECORD)) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(Report::new(err).change_context(TrustError::State)),
        };
        let entries: Vec<Destination> =
            serde_json::from_slice(&bytes).change_context(TrustError::State)?;
        if entries.iter().any(|entry| {
            !entry.database.is_absolute()
                || !entry
                    .nickname
                    .strip_prefix("ts-dev-proxy-")
                    .is_some_and(|suffix| {
                        suffix.len() == 16 && suffix.bytes().all(|byte| byte.is_ascii_hexdigit())
                    })
                || entry.certificate.is_empty()
        }) {
            return Err(Report::new(TrustError::State));
        }
        Ok(entries)
    }

    // Atomic replacement and sync before import make interrupted operations retryable.
    fn write_record(ca_dir: &Path, entries: &[Destination]) -> Result<()> {
        let mut tmp = tempfile::NamedTempFile::new_in(ca_dir).change_context(TrustError::Io)?;
        let bytes = serde_json::to_vec(entries).change_context(TrustError::State)?;
        tmp.write_all(&bytes).change_context(TrustError::Io)?;
        tmp.as_file().sync_all().change_context(TrustError::Io)?;
        tmp.persist(ca_dir.join(RECORD))
            .change_context(TrustError::Io)?;
        File::open(ca_dir)
            .change_context(TrustError::Io)?
            .sync_all()
            .change_context(TrustError::Io)?;
        Ok(())
    }

    /// Refuses implicit replacement of material still recorded as trusted.
    ///
    /// # Errors
    /// Returns an error for invalid records or missing material with managed trust.
    pub(crate) fn ensure_can_generate(ca_dir: &Path) -> Result<()> {
        if !read_record(ca_dir)?.is_empty()
            && (!ca_dir
                .join("ca-cert.pem")
                .try_exists()
                .change_context(TrustError::Io)?
                || !ca_dir
                    .join("ca-key.pem")
                    .try_exists()
                    .change_context(TrustError::Io)?)
        {
            return Err(Report::new(TrustError::State).attach("CA files are missing but managed trust remains; run ca uninstall before generating new material"));
        }
        Ok(())
    }

    fn selected_database(home: &Path, data_home: Option<&Path>) -> Result<PathBuf> {
        let legacy = home.join(".pki/nssdb");
        match fs::metadata(&legacy) {
            Ok(meta) if meta.is_dir() => return Ok(legacy),
            Ok(_) => {
                return Err(
                    Report::new(TrustError::Io).attach("legacy NSS path is not a directory")
                );
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(Report::new(err).change_context(TrustError::Io)),
        }
        let data_home = data_home.filter(|path| !path.as_os_str().is_empty());
        if data_home.is_some_and(|path| !path.is_absolute()) {
            return Err(Report::new(TrustError::Io).attach(
                "XDG_DATA_HOME must be absolute for Linux NSS trust; unset it or set an absolute path, then retry ca install",
            ));
        }
        Ok(data_home
            .map_or_else(|| home.join(".local/share"), Path::to_path_buf)
            .join("pki/nssdb"))
    }

    fn certificate(cert_path: &Path) -> Result<Vec<u8>> {
        let bytes = fs::read(cert_path).change_context(TrustError::Io)?;
        rustls_pemfile::certs(&mut bytes.as_slice())
            .next()
            .ok_or_else(|| Report::new(TrustError::State))?
            .change_context(TrustError::State)
            .map(|cert| cert.to_vec())
    }

    fn initialize(database: &Path) -> Result<()> {
        fs::create_dir_all(database).change_context(TrustError::Io)?;
        let db = format!("sql:{}", database.display());
        // Never reset an existing or partially created database. Query it instead.
        let existing = [
            "cert9.db",
            "key4.db",
            "pkcs11.txt",
            "cert8.db",
            "key3.db",
            "secmod.db",
        ]
        .iter()
        .map(|name| database.join(name).try_exists())
        .collect::<std::io::Result<Vec<_>>>()
        .change_context(TrustError::Io)?
        .into_iter()
        .any(|exists| exists);
        if !existing {
            certutil(&["-N", "--empty-password", "-d", &db])?;
        }
        certutil(&["-L", "-d", &db])?;
        Ok(())
    }

    /// Returns exact NSS derSubject identity without applying CA reconstruction rules.
    /// Malformed DER or trailing data is an error, not a skipped certificate.
    fn subject(der: &[u8]) -> Result<Vec<u8>> {
        let (remaining, certificate) = x509_parser::parse_x509_certificate(der).map_err(|err| {
            Report::new(TrustError::State).attach(format!("cannot parse NSS certificate: {err}"))
        })?;
        if !remaining.is_empty() {
            return Err(Report::new(TrustError::State));
        }
        Ok(certificate.subject().as_raw().to_vec())
    }

    /// Queries the CA's subject without reconstructing nicknames from a padded table.
    /// Returns an error on query failure, invalid output, or a different same-subject cert.
    fn check_subject_conflicts(entry: &Destination, cert_path: &Path) -> Result<()> {
        let subject = subject(&entry.certificate)?;
        let db = format!("sql:{}", entry.database.display());
        let cert_path = fs::canonicalize(cert_path).change_context(TrustError::Io)?;
        // NSS accepts a certificate filename when -n does not resolve to a nickname.
        // It loads that cert temporarily and exports the whole matching-subject set,
        // including the supplied cert even if it is not stored. No import is needed.
        let exported = certutil(&["-L", "-n", &cert_path.to_string_lossy(), "-a", "-d", &db])?;
        let certificates = rustls_pemfile::certs(&mut exported.stdout.as_slice())
            .collect::<std::io::Result<Vec<_>>>()
            .change_context(TrustError::State)?;
        let mut found_expected = false;
        for certificate in certificates {
            // A nickname matching the filename must not select an unrelated subject.
            if self::subject(certificate.as_ref())? != subject {
                return Err(Report::new(TrustError::State).attach(
                    "NSS subject query returned an unrelated certificate; trust was not changed",
                ));
            }
            if certificate.as_ref() != entry.certificate {
                return Err(Report::new(TrustError::Identity).attach(format!(
                    "{} already contains a different same-subject certificate; remove its trust with the original CA directory or resolve the manual import before installing another dev CA",
                    entry.database.display()
                )));
            }
            found_expected = true;
        }
        if !found_expected {
            return Err(Report::new(TrustError::State).attach(
                "NSS subject query did not return the supplied CA; trust was not changed",
            ));
        }
        Ok(())
    }

    // A successful whole-store query proves absence. A failed named lookup does not.
    fn contains(entry: &Destination) -> Result<bool> {
        if !entry.database.try_exists().change_context(TrustError::Io)? {
            return Ok(false);
        }
        let db = format!("sql:{}", entry.database.display());
        let list = certutil(&["-L", "-d", &db])?;
        // Match only our known ASCII nickname, excluding the final trust column.
        // Do not reconstruct foreign nicknames or reject their malformed row fragments.
        // Padding and embedded newlines can imitate a managed row, so a match still
        // requires the exact named DER export below, never deletion alone.
        let present = list.stdout.split(|byte| *byte == b'\n').any(|line| {
            let row = line.trim_ascii_end();
            row.iter()
                .rposition(u8::is_ascii_whitespace)
                .is_some_and(|separator| {
                    row[..separator].trim_ascii_end() == entry.nickname.as_bytes()
                })
        });
        if !present {
            return Ok(false);
        }
        let found = certutil(&["-L", "-n", &entry.nickname, "-r", "-d", &db])?;
        if found.stdout != entry.certificate {
            return Err(Report::new(TrustError::Identity).attach(format!(
                "{}: {}",
                entry.database.display(),
                entry.nickname
            )));
        }
        Ok(true)
    }

    fn install_into(ca_dir: &Path, cert_path: &Path, database: &Path) -> Result<()> {
        let mut entries = read_record(ca_dir)?;
        fs::create_dir_all(database).change_context(TrustError::Io)?;
        let database = fs::canonicalize(database).change_context(TrustError::Io)?;
        // All ts CA directories serialize changes to this shared NSS destination.
        let _database_lock = lock_file(&database.join("ts-dev-proxy-trust.lock"))?;
        // A later rejection may leave a newly initialized empty store behind.
        // Certificate trust and the journal remain unchanged until preflight succeeds.
        initialize(&database)?;
        let certificate = certificate(cert_path)?;
        // The hash only names the entry. Full DER equality authorizes all mutations.
        let mut hash = DefaultHasher::new();
        certificate.hash(&mut hash);
        let nickname = format!("ts-dev-proxy-{:016x}", hash.finish());
        let index = match entries
            .iter()
            .position(|entry| entry.database == database && entry.certificate == certificate)
        {
            Some(index) => index,
            None => {
                entries.push(Destination {
                    database,
                    nickname,
                    certificate,
                });
                entries.len() - 1
            }
        };
        let entry = &entries[index];
        // Check identity only; an identical existing certificate is re-imported safely.
        contains(entry)?;
        check_subject_conflicts(entry, cert_path)?;
        write_record(ca_dir, &entries)?;
        let db = format!("sql:{}", entry.database.display());
        certutil(&[
            "-A",
            "-n",
            &entry.nickname,
            "-t",
            "C,,",
            "-i",
            &cert_path.to_string_lossy(),
            "-d",
            &db,
        ])?;
        if !contains(entry)? {
            return Err(Report::new(TrustError::Command));
        }
        output::info(&format!(
            "CA trusted for native Chrome/Chromium in {} (not system-wide)",
            entry.database.display()
        ));
        Ok(())
    }

    /// Installs into the selected user NSS database, retaining earlier destinations.
    ///
    /// # Errors
    /// Returns an error if destination resolution, recording, querying or import fails.
    pub(crate) fn install(ca_dir: &Path, cert_path: &Path) -> Result<()> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .ok_or_else(|| Report::new(TrustError::Io).attach("HOME must be an absolute path"))?;
        let data = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from);
        let database = selected_database(&home, data.as_deref())?;
        install_into(ca_dir, cert_path, &database)
    }

    /// Removes only recorded, identity-checked imports, verifying each deletion.
    ///
    /// # Errors
    /// Returns an error if any recorded removal cannot be confirmed.
    pub(crate) fn uninstall(ca_dir: &Path) -> Result<()> {
        let mut entries = read_record(ca_dir)?;
        while let Some(entry) = entries.last() {
            // Do not create an absent database during uninstall. CA lock is held first.
            let _database_lock = if entry.database.try_exists().change_context(TrustError::Io)? {
                Some(lock_file(&entry.database.join("ts-dev-proxy-trust.lock"))?)
            } else {
                None
            };
            if contains(entry)? {
                let db = format!("sql:{}", entry.database.display());
                certutil(&["-D", "-n", &entry.nickname, "-d", &db])?;
                if contains(entry)? {
                    return Err(Report::new(TrustError::Command));
                }
            }
            entries.pop();
            write_record(ca_dir, &entries)?;
        }
        output::info(
            "Managed user NSS trust is absent; manual imports and running browser profiles are not revoked",
        );
        Ok(())
    }
    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn subject_requires_complete_der() {
            let cert = rcgen::generate_simple_self_signed(vec!["leaf.example.com".into()])
                .expect("should generate leaf fixture")
                .cert;
            assert!(subject(cert.der().as_ref()).is_ok());
            let mut trailing = cert.der().to_vec();
            trailing.push(0);
            assert!(subject(&trailing).is_err());
        }

        #[test]
        fn selected_database_prefers_legacy_and_absolute_xdg() {
            let home = tempfile::tempdir().expect("should create isolated home");
            let data = home.path().join("custom-data");
            assert_eq!(
                selected_database(home.path(), Some(&data)).expect("should resolve"),
                data.join("pki/nssdb")
            );
            assert!(selected_database(home.path(), Some(Path::new("relative"))).is_err());
            for data in [None, Some(Path::new(""))] {
                assert_eq!(
                    selected_database(home.path(), data).expect("should resolve default"),
                    home.path().join(".local/share/pki/nssdb")
                );
            }
            let legacy = home.path().join(".pki/nssdb");
            fs::create_dir_all(&legacy).expect("should create legacy database");
            assert_eq!(
                selected_database(home.path(), Some(Path::new("relative")))
                    .expect("should prefer legacy regardless of XDG"),
                legacy
            );
        }

        #[test]
        fn malformed_journal_and_missing_material_fail_closed() {
            let dir = tempfile::tempdir().expect("should create CA directory");
            fs::write(dir.path().join(RECORD), "invalid").expect("should write journal");
            assert!(uninstall(dir.path()).is_err());
            assert!(ensure_can_generate(dir.path()).is_err());
            let entry = Destination {
                database: dir.path().join("nss"),
                nickname: "ts-dev-proxy-0123456789abcdef".into(),
                certificate: vec![1],
            };
            write_record(dir.path(), &[entry]).expect("should persist record");
            assert!(ensure_can_generate(dir.path()).is_err());
            // A recorded import interrupted before DB creation is confirmed absent.
            uninstall(dir.path()).expect("should remove absent destination");
            ensure_can_generate(dir.path()).expect("should allow generation after revocation");
        }
    }
}

#[cfg(target_os = "macos")]
fn login_keychain() -> Result<std::path::PathBuf> {
    let home = std::env::var_os("HOME").ok_or_else(|| Report::new(TrustError::Io))?;
    Ok(Path::new(&home).join("Library/Keychains/login.keychain-db"))
}

#[cfg(target_os = "macos")]
pub(super) fn ensure_can_generate(_ca_dir: &Path) -> Result<()> {
    Ok(())
}

/// Adds trust to the existing macOS login-keychain destination.
#[cfg(target_os = "macos")]
pub(super) fn install(_ca_dir: &Path, cert_path: &Path) -> Result<()> {
    checked(
        Command::new("security")
            .args(["add-trusted-cert", "-r", "trustRoot", "-k"])
            .arg(login_keychain()?)
            .arg(cert_path),
    )?;
    output::info("CA added to the login keychain");
    Ok(())
}

/// Preserves the macOS common-name removal contract, but fails closed on query errors.
#[cfg(target_os = "macos")]
pub(super) fn uninstall(_ca_dir: &Path) -> Result<()> {
    let keychain = login_keychain()?;
    for _ in 0..16 {
        let found = Command::new("security")
            .args(["find-certificate", "-c", CA_COMMON_NAME])
            .arg(&keychain)
            .output()
            .change_context(TrustError::Command)?;
        // security exits with errSecItemNotFound (-25300) truncated to 8 bits.
        if found.status.code() == Some(44) {
            return Ok(());
        }
        if !found.status.success() {
            return Err(Report::new(TrustError::Command)
                .attach(String::from_utf8_lossy(&found.stderr).into_owned()));
        }
        checked(
            Command::new("security")
                .args(["delete-certificate", "-c", CA_COMMON_NAME])
                .arg(&keychain),
        )?;
    }
    Err(Report::new(TrustError::Command)
        .attach("could not confirm complete login-keychain removal"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ca_lock_rejects_concurrent_operations_and_releases_on_drop() {
        let dir = tempfile::tempdir().expect("should create isolated CA directory");
        let first = lock(dir.path()).expect("should acquire first lock");
        assert!(matches!(
            lock(dir.path())
                .expect_err("should reject contention")
                .current_context(),
            TrustError::Busy
        ));
        drop(first);
        assert!(lock(dir.path()).is_ok());
    }
}
