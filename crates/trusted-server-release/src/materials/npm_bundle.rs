//! Original-only inputs shared read-only by fresh npm installations.

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::Instant;

use error_stack::Report;
use tempfile::TempDir;

use super::{LockedMaterials, ReviewedNpmManifest, material_error};
use crate::documents::hash_bounded_with_check;
use crate::error::ReleaseError;
use crate::process::Cancellation;

/// Retained originals and manifest reviews, without installed build authority.
pub(crate) struct PreparedNpmOriginals {
    directory: TempDir,
    originals: BTreeMap<String, String>,
    reviews: BTreeMap<String, ReviewedNpmManifest>,
    source_commit: String,
    lock_sha256: String,
    deadline: Instant,
    cancellation: Cancellation,
}

impl PreparedNpmOriginals {
    /// Measures the selected Linux esbuild executable from its original archive.
    ///
    /// # Errors
    ///
    /// Returns an error for unsupported architectures, absent originals or a
    /// stale extraction. These are original facts, not installed observations.
    pub(crate) fn native_esbuild(
        &self,
        architecture: &str,
    ) -> Result<serde_json::Value, Report<ReleaseError>> {
        let architecture = match architecture {
            "x86_64" => "x64",
            "aarch64" => "arm64",
            other => other,
        };
        if !matches!(architecture, "x64" | "arm64") {
            return Err(material_error(
                "unsupported Linux JS preparation architecture",
            ));
        }
        let location = format!("node_modules/@esbuild/linux-{architecture}");
        let review = self
            .reviews
            .get(&location)
            .ok_or_else(|| material_error("required native esbuild original is absent"))?;
        let (sha256, size, mode) = review.member_facts("bin/esbuild")?;
        Ok(serde_json::json!({"lock_path":location,"sha256":sha256,"size":size,"mode":mode}))
    }
    /// Returns exact reviewed declarations for joining real lifecycle events.
    pub(crate) fn declared_hooks(&self) -> BTreeMap<(String, String), String> {
        self.reviews
            .iter()
            .flat_map(|(location, review)| {
                review.hooks().iter().map(|hook| {
                    (
                        (location.clone(), hook.event().to_owned()),
                        hook.script_sha256().to_owned(),
                    )
                })
            })
            .collect()
    }
    /// Returns the private original archive root for a read-only mount.
    pub(crate) fn root(&self) -> &Path {
        self.directory.path()
    }

    /// Returns sorted authenticated archive paths, never extracted replacements.
    pub(crate) fn archive_paths(&self) -> Vec<PathBuf> {
        self.originals
            .keys()
            .map(|name| self.root().join(name))
            .collect()
    }

    /// Returns the approved source and raw lockfile bindings.
    pub(crate) fn source_binding(&self) -> (&str, &str) {
        (&self.source_commit, &self.lock_sha256)
    }

    /// Rechecks every retained original and reviewed extraction before use.
    ///
    /// # Errors
    ///
    /// Returns an error for changed originals, stale extractions or expiration.
    pub(crate) fn verify_unchanged(&self) -> Result<(), Report<ReleaseError>> {
        let check = || check_attempt(self.deadline, &self.cancellation);
        check()?;
        for (name, expected) in &self.originals {
            let raw = hash_bounded_with_check(&self.root().join(name), 64 * 1024 * 1024, &check)?;
            if raw.sha256 != *expected {
                return Err(material_error("retained npm original changed"));
            }
        }
        for review in self.reviews.values() {
            check()?;
            review.verify_unchanged()?;
        }
        check()
    }
}

impl LockedMaterials {
    /// Authenticates and reviews all locked npm originals into private state.
    ///
    /// # Errors
    ///
    /// Returns an error for any absent or changed archive, unsafe extraction,
    /// unreviewed manifest, canceled attempt or failed private write. Optional
    /// platform packages receive the same original checks before npm selects
    /// which ones to install. Input filenames come only from locked SHA-512.
    pub(crate) fn prepare_npm_originals(
        &self,
        inputs: &Path,
        deadline: Instant,
        cancellation: &Cancellation,
    ) -> Result<PreparedNpmOriginals, Report<ReleaseError>> {
        let directory = tempfile::tempdir().map_err(material_error)?;
        let mut originals = BTreeMap::new();
        let mut reviews = BTreeMap::new();
        for (location, locked) in &self.npm {
            check_attempt(deadline, cancellation)?;
            let name = format!("{}.tgz", hex::encode(locked.sha512));
            let verified = self
                .verify_npm(location, &inputs.join(&name), deadline, cancellation)
                .map_err(|report| {
                    report.attach(format!(
                        "authenticating locked npm package `{location}` original `{name}`"
                    ))
                })?;
            let expected_sha256 = verified.sha256().to_owned();
            let extracted = self.extract_npm(location, verified).map_err(|report| {
                report.attach(format!(
                    "extracting authenticated locked npm package `{location}`"
                ))
            })?;
            let mut reviewed = self.review_npm(location, extracted).map_err(|report| {
                report.attach(format!(
                    "reviewing authenticated locked npm package `{location}`"
                ))
            })?;
            reviewed.verify_unchanged()?;
            if let Entry::Vacant(entry) = originals.entry(name) {
                let name = entry.key();
                let path = directory.path().join(name);
                let mut file = File::create_new(&path).map_err(material_error)?;
                reviewed.copy_original(&mut file)?;
                drop(file);
                let measured = hash_bounded_with_check(&path, 64 * 1024 * 1024, &|| {
                    check_attempt(deadline, cancellation)
                })?;
                if measured.sha256 != expected_sha256 {
                    return Err(material_error(
                        "copied npm original differs from authenticated bytes",
                    ));
                }
                entry.insert(expected_sha256);
            }
            reviews.insert(location.to_owned(), reviewed);
        }
        let prepared = PreparedNpmOriginals {
            directory,
            originals,
            reviews,
            source_commit: self.source_commit.clone(),
            lock_sha256: self.npm_lock_sha256.clone(),
            deadline,
            cancellation: cancellation.clone(),
        };
        prepared.verify_unchanged()?;
        Ok(prepared)
    }
}

fn check_attempt(
    deadline: Instant,
    cancellation: &Cancellation,
) -> Result<(), Report<ReleaseError>> {
    if cancellation.is_cancelled() || Instant::now() >= deadline {
        return Err(material_error(
            "npm original collection cancelled or timed out",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::Duration;

    use sha2::{Digest as _, Sha256};

    use super::*;

    fn fixture() -> PreparedNpmOriginals {
        let directory = tempfile::tempdir().expect("should create retained originals");
        fs::write(directory.path().join("original.tgz"), b"original")
            .expect("should retain fixture original");
        PreparedNpmOriginals {
            directory,
            originals: BTreeMap::from([(
                "original.tgz".to_owned(),
                hex::encode(Sha256::digest(b"original")),
            )]),
            reviews: BTreeMap::new(),
            source_commit: "a".repeat(40),
            lock_sha256: "b".repeat(64),
            deadline: Instant::now() + Duration::from_secs(30),
            cancellation: Cancellation::default(),
        }
    }

    #[test]
    fn retained_original_rechecks_obey_attempt_controls() {
        let prepared = fixture();
        prepared
            .verify_unchanged()
            .expect("should hash retained original");
        prepared.cancellation.cancel();
        assert!(
            prepared.verify_unchanged().is_err(),
            "should reject retained canceled attempts"
        );
        let mut prepared = fixture();
        prepared.deadline = Instant::now();
        assert!(
            prepared.verify_unchanged().is_err(),
            "should reject retained expired attempts"
        );
    }

    #[test]
    fn retained_original_rechecks_keep_the_archive_bound_and_digest() {
        let prepared = fixture();
        let path = prepared.archive_paths().remove(0);
        fs::write(&path, b"tampered").expect("should change retained bytes");
        assert!(
            prepared.verify_unchanged().is_err(),
            "should reject changed retained digest"
        );
        fs::File::create(path)
            .expect("should create sparse oversized archive")
            .set_len(64 * 1024 * 1024 + 1)
            .expect("should exceed original archive limit");
        assert!(
            prepared.verify_unchanged().is_err(),
            "should reject oversized retained archives before allocation"
        );
    }

    #[test]
    #[ignore = "requires explicitly supplied current-lock original archives; no acquisition qualification"]
    fn actual_current_lock_npm_originals() {
        let inputs = std::env::var_os("TS_RELEASE_NPM_ORIGINALS")
            .map(PathBuf::from)
            .expect("should explicitly supply current locked compressed originals");
        let bytes = include_bytes!("../../../trusted-server-js/lib/package-lock.json");
        let npm = crate::materials::locked::parse_npm(bytes)
            .expect("should parse every actual current npm lock identity");
        let count = npm.len();
        let archives: std::collections::BTreeSet<_> =
            npm.values().map(|entry| entry.sha512).collect();
        assert!(count > 0, "should select nonzero current locked packages");
        let locked = LockedMaterials {
            // This original-format diagnostic supplies no official source approval.
            source_commit: "a".repeat(40),
            cargo_lock_sha256: "b".repeat(64),
            npm_lock_sha256: hex::encode(Sha256::digest(bytes)),
            cargo: crate::materials::locked::CargoMaterials {
                registry: BTreeMap::new(),
                git: std::collections::BTreeSet::new(),
                git_packages: Vec::new(),
            },
            npm,
        };
        let prepared = locked
            .prepare_npm_originals(
                &inputs,
                Instant::now() + Duration::from_secs(600),
                &Cancellation::default(),
            )
            .expect("should authenticate, extract and review every actual current npm original");
        assert_eq!(
            prepared.reviews.len(),
            count,
            "should review every locked package including optional platform entries"
        );
        assert_eq!(
            prepared.archive_paths().len(),
            archives.len(),
            "should retain every distinct original compressed distribution"
        );
        assert_eq!(
            prepared.source_binding(),
            (
                "a".repeat(40).as_str(),
                hex::encode(Sha256::digest(bytes)).as_str()
            ),
            "should retain the diagnostic source and exact current raw lock binding"
        );
        prepared
            .verify_unchanged()
            .expect("should rejoin every retained original and complete reviewed extraction");
    }
}
