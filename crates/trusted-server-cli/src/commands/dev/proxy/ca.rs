//! Per-machine local CA: load-or-generate, mint and cache per-host leaves (spec §7).

use std::collections::HashMap;
use std::fs;
use std::fs::OpenOptions;
use std::io::Write as _;
use std::net::IpAddr;
use std::os::unix::fs::OpenOptionsExt as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use error_stack::{Report, ResultExt as _};
use rcgen::{
    BasicConstraints, Certificate, CertificateParams, DnType, IsCa, KeyPair, KeyUsagePurpose,
    SanType,
};
use rustls::ServerConfig;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};

/// Distinguished CA common name (spec §12).
pub const CA_COMMON_NAME: &str =
    "Trusted Server DEV-ONLY Proxy CA \u{2014} DO NOT TRUST IN PRODUCTION";

const CA_CERT_FILE: &str = "ca-cert.pem";
const CA_KEY_FILE: &str = "ca-key.pem";
const LEAF_VALIDITY_DAYS: i64 = 90;

/// Errors from the certificate authority.
#[derive(Debug, derive_more::Display)]
pub enum CaError {
    /// The CA directory could not be created or secured.
    #[display("cannot prepare CA directory")]
    Dir,
    /// Reading/writing a CA PEM file failed.
    #[display("CA file I/O failed")]
    Io,
    /// Certificate generation/signing failed.
    #[display("certificate generation failed")]
    Generate,
    /// Building the rustls server config failed.
    #[display("rustls server config failed")]
    Rustls,
}

impl core::error::Error for CaError {}

/// Loaded CA material plus a per-host leaf cache.
pub struct CertAuthority {
    /// CA certificate — kept alive to pass as `issuer` to `signed_by`.
    ca_cert: Certificate,
    /// CA key pair used when signing leaf certificates.
    ca_key: KeyPair,
    /// DER-encoded CA cert included in each leaf's certificate chain.
    ca_cert_der: CertificateDer<'static>,
    /// Per-host cache of minted `ServerConfig` instances.
    leaves: Mutex<HashMap<String, Arc<ServerConfig>>>,
}

impl CertAuthority {
    /// Path to the CA certificate under `ca_dir`.
    #[must_use]
    pub fn cert_path(ca_dir: &Path) -> PathBuf {
        ca_dir.join(CA_CERT_FILE)
    }

    /// Loads the CA from `ca_dir`, generating and persisting it on first run.
    ///
    /// The directory is created with mode `0700` and the key file is written
    /// with mode `0600`.  On first run a trust hint is logged.
    ///
    /// # Errors
    ///
    /// Returns [`CaError`] on directory, I/O, or certificate generation failures.
    pub fn load_or_generate(ca_dir: &Path) -> Result<Self, Report<CaError>> {
        let cert_path = ca_dir.join(CA_CERT_FILE);
        let key_path = ca_dir.join(CA_KEY_FILE);

        let (cert_pem, key_pem) = if cert_path.exists() && key_path.exists() {
            // Re-secure existing material before reusing it: a backup restore,
            // manual copy, partial recovery, or older build may have left the
            // directory or the private key group/world-readable. Restore the
            // freshly-generated posture (dir `0700`, key `0600`) so a trusted
            // root CA private key is never used with loose permissions.
            fs::set_permissions(ca_dir, fs::Permissions::from_mode(0o700))
                .change_context(CaError::Dir)?;
            fs::set_permissions(&key_path, fs::Permissions::from_mode(0o600))
                .change_context(CaError::Io)?;
            (
                fs::read_to_string(&cert_path).change_context(CaError::Io)?,
                fs::read_to_string(&key_path).change_context(CaError::Io)?,
            )
        } else {
            let (cert_pem, key_pem) = Self::generate_pems()?;
            Self::persist(ca_dir, &cert_path, &key_path, &cert_pem, &key_pem)?;
            log::info!(
                "generated dev CA at {} — run `ts dev proxy ca install` to trust it",
                cert_path.display()
            );
            (cert_pem, key_pem)
        };

        let ca_key = KeyPair::from_pem(&key_pem).change_context(CaError::Generate)?;
        let ca_params =
            CertificateParams::from_ca_cert_pem(&cert_pem).change_context(CaError::Generate)?;
        let ca_cert_der = pem_to_cert_der(&cert_pem)?;
        // Reconstruct the Certificate struct so we can pass it as issuer to signed_by.
        let ca_cert = ca_params
            .self_signed(&ca_key)
            .change_context(CaError::Generate)?;

        Ok(Self {
            ca_cert,
            ca_key,
            ca_cert_der,
            leaves: Mutex::new(HashMap::new()),
        })
    }

    /// Returns a cached or freshly minted leaf [`ServerConfig`] for `host`.
    ///
    /// Minting happens outside the cache lock; a double-check after re-acquiring
    /// the lock ensures concurrent callers for the same host return the same [`Arc`].
    ///
    /// # Errors
    ///
    /// Returns [`CaError`] if leaf minting or rustls config construction fails.
    pub fn server_config(&self, host: &str) -> Result<Arc<ServerConfig>, Report<CaError>> {
        // Fast path: return a cached config without holding the lock during minting.
        {
            let cache = self
                .leaves
                .lock()
                .expect("should be able to acquire leaf cache lock");
            if let Some(existing) = cache.get(host) {
                return Ok(Arc::clone(existing));
            }
        }

        let config = Arc::new(self.mint(host)?);

        let mut cache = self
            .leaves
            .lock()
            .expect("should be able to acquire leaf cache lock");
        // Double-check: another task may have minted concurrently.
        let entry = cache.entry(host.to_string()).or_insert(config);
        Ok(Arc::clone(entry))
    }

    fn mint(&self, host: &str) -> Result<ServerConfig, Report<CaError>> {
        let leaf_key = KeyPair::generate().change_context(CaError::Generate)?;

        // Build the SAN explicitly: an IP-literal host gets an IP-type SAN (not DNS).
        let san = match host.parse::<IpAddr>() {
            Ok(ip) => SanType::IpAddress(ip),
            Err(_) => {
                let ia5 = rcgen::Ia5String::try_from(host).change_context(CaError::Generate)?;
                SanType::DnsName(ia5)
            }
        };

        let mut params =
            CertificateParams::new(Vec::<String>::new()).change_context(CaError::Generate)?;
        params.subject_alt_names = vec![san];

        let now = time::OffsetDateTime::now_utc();
        params.not_before = now - time::Duration::days(1);
        params.not_after = now + time::Duration::days(LEAF_VALIDITY_DAYS);

        let leaf = params
            .signed_by(&leaf_key, &self.ca_cert, &self.ca_key)
            .change_context(CaError::Generate)?;

        let chain = vec![leaf.der().clone(), self.ca_cert_der.clone()];
        let key_der = PrivateKeyDer::try_from(leaf_key.serialize_der())
            .map_err(|_| Report::new(CaError::Rustls))?;

        let mut config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(chain, key_der)
            .change_context(CaError::Rustls)?;
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        Ok(config)
    }

    fn generate_pems() -> Result<(String, String), Report<CaError>> {
        let key = KeyPair::generate().change_context(CaError::Generate)?;
        let mut params =
            CertificateParams::new(Vec::<String>::new()).change_context(CaError::Generate)?;
        params
            .distinguished_name
            .push(DnType::CommonName, CA_COMMON_NAME);
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        // ~10 years from generation (spec §7.1); rotate via `ca regenerate`.
        let now = time::OffsetDateTime::now_utc();
        params.not_before = now - time::Duration::days(1);
        params.not_after = now + time::Duration::days(3650);
        let cert = params.self_signed(&key).change_context(CaError::Generate)?;
        Ok((cert.pem(), key.serialize_pem()))
    }

    fn persist(
        ca_dir: &Path,
        cert_path: &Path,
        key_path: &Path,
        cert_pem: &str,
        key_pem: &str,
    ) -> Result<(), Report<CaError>> {
        fs::create_dir_all(ca_dir).change_context(CaError::Dir)?;
        fs::set_permissions(ca_dir, fs::Permissions::from_mode(0o700))
            .change_context(CaError::Dir)?;
        // Clear any stale pair first so `create_new` on the key always succeeds
        // and the written cert/key pair is always self-consistent. A leftover
        // key from a prior partial write would otherwise survive next to the new
        // cert, leaving a mismatched (cert, key) that future runs would load.
        fs::remove_file(cert_path).ok();
        fs::remove_file(key_path).ok();
        fs::write(cert_path, cert_pem).change_context(CaError::Io)?;
        let mut key_file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(key_path)
            .change_context(CaError::Io)?;
        key_file
            .write_all(key_pem.as_bytes())
            .change_context(CaError::Io)?;
        Ok(())
    }
}

fn pem_to_cert_der(cert_pem: &str) -> Result<CertificateDer<'static>, Report<CaError>> {
    let mut reader = std::io::BufReader::new(cert_pem.as_bytes());
    rustls_pemfile::certs(&mut reader)
        .next()
        .ok_or_else(|| Report::new(CaError::Io))?
        .change_context(CaError::Io)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_then_reloads_with_0600_key() {
        let dir = tempfile::tempdir().expect("should create tempdir");
        let ca1 = CertAuthority::load_or_generate(dir.path()).expect("should generate");
        let key_path = dir.path().join("ca-key.pem");
        assert!(key_path.exists(), "key persisted");
        let mode = std::fs::metadata(&key_path)
            .expect("should read key metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "key file is 0600");

        // Second run reloads the same CA cert bytes (no regeneration).
        let cert_before = std::fs::read(dir.path().join("ca-cert.pem")).expect("should read cert");
        let _ca2 = CertAuthority::load_or_generate(dir.path()).expect("should reload");
        let cert_after = std::fs::read(dir.path().join("ca-cert.pem")).expect("should read cert");
        assert_eq!(cert_before, cert_after, "reload does not rewrite the CA");
        drop(ca1);
    }

    #[test]
    fn reload_resecures_loosened_key_permissions() {
        let dir = tempfile::tempdir().expect("should create tempdir");
        CertAuthority::load_or_generate(dir.path()).expect("should generate");
        let key_path = dir.path().join("ca-key.pem");

        // Simulate a drifted/backup-restored key with group/world-readable perms.
        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o644))
            .expect("should loosen perms");

        // Reloading must re-secure it to 0600 before reusing the key material.
        CertAuthority::load_or_generate(dir.path()).expect("should reload");
        let mode = std::fs::metadata(&key_path)
            .expect("should read key metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "reload re-secures the key file to 0600");
    }

    #[test]
    fn leaf_cache_returns_same_arc_for_same_host() {
        let dir = tempfile::tempdir().expect("should create tempdir");
        let ca = CertAuthority::load_or_generate(dir.path()).expect("should generate");
        let a = ca
            .server_config("www.example-publisher.com")
            .expect("should mint");
        let b = ca
            .server_config("www.example-publisher.com")
            .expect("should return cached");
        assert!(Arc::ptr_eq(&a, &b), "same host returns the cached Arc");
        let c = ca
            .server_config("other.example.com")
            .expect("should mint other");
        assert!(!Arc::ptr_eq(&a, &c), "different host mints a new config");
    }

    #[test]
    fn mints_leaf_for_ip_literal_host() {
        // An IP-literal host must mint successfully (IP-type SAN, not DNS) — spec §8.3.
        let dir = tempfile::tempdir().expect("should create tempdir");
        let ca = CertAuthority::load_or_generate(dir.path()).expect("should generate");
        assert!(
            ca.server_config("127.0.0.1").is_ok(),
            "IP-literal host mints a leaf"
        );
    }
}
