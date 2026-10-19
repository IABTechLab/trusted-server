//! Test-only adoption of the JS handoff an ordinary Linux CI job uploaded.
//!
//! The macOS candidate jobs cannot run the Linux-only two-installation JS
//! build, so they adopt the Linux job's retained handoff as an artifact.
//! Adoption copies exactly `bundles/`, `tsjs-inventory.json` and
//! `npm-observation.json` into a private directory, rejoins the inventory
//! against the authenticated source with the same helpers the real build
//! uses, and requires every module digest and the measurement's inventory
//! digest to match. The artifact itself is not authenticated, and the
//! returned measurement says so.
//!
//! The Linux job exports the artifact as a root holding exactly the
//! retained handoff directory and its measurement beside it.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use error_stack::Report;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use super::super::{JsBuildCompleted, Module, filenames, inventory_modules};
use crate::documents::{Role, parse_json, read_bounded};
use crate::error::ReleaseError;
use crate::preparation::{JsAttempt, preparation_error};
use crate::process::Cancellation;
use crate::source::{ApprovedSource, join_inventory};

/// Kind of the Linux job's JS measurement.
const MEASUREMENT_KIND: &str = "ordinary-ci-two-fresh-js-builds";
/// Origin label of an adopted handoff.
pub(crate) const ADOPTED_ORIGIN: &str = "linux-ordinary-ci-artifact";
/// How long an adopted handoff stays usable by the candidate run.
const ADOPTED_LIFETIME: Duration = Duration::from_secs(4 * 60 * 60);
/// The exact entries of a retained handoff.
const HANDOFF_ENTRIES: [&str; 3] = ["bundles", "npm-observation.json", "tsjs-inventory.json"];
/// Directory of an exported root holding the three retained entries.
const EXPORTED_HANDOFF: &str = "handoff";
/// The Linux job's JS measurement beside the exported handoff.
const EXPORTED_MEASUREMENT: &str = "js-execution-measurement.json";

fn lower_hex_64(value: &Value) -> Option<&str> {
    value.as_str().filter(|text| {
        text.len() == 64
            && text
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

/// The measured facts an adopted handoff must match.
struct MeasuredHandoff<'a> {
    inventory_sha256: &'a str,
    tools_sha256: &'a str,
    settings_sha256: &'a str,
    tests: [u64; 2],
}

/// Requires the Linux job's measurement for `source`.
fn check_measurement<'a>(
    source: &ApprovedSource,
    measurement: &'a Value,
) -> Result<MeasuredHandoff<'a>, Report<ReleaseError>> {
    let shape = measurement["schema_version"] == 1
        && measurement["kind"] == MEASUREMENT_KIND
        && measurement["release_qualification"] == false;
    if !shape {
        return Err(preparation_error(
            "JS measurement is not an ordinary CI two-build measurement",
        ));
    }
    if measurement["source_commit"].as_str() != Some(source.commit().as_str()) {
        return Err(preparation_error(
            "JS measurement names another source commit",
        ));
    }
    let digest = |name: &str| {
        lower_hex_64(&measurement[name])
            .ok_or_else(|| preparation_error(format!("JS measurement {name} is not a digest")))
    };
    let count = |index: usize| {
        measurement["test_counts"][index]
            .as_u64()
            .filter(|count| *count > 0)
            .ok_or_else(|| preparation_error("JS measurement test counts are not positive"))
    };
    Ok(MeasuredHandoff {
        inventory_sha256: digest("inventory_sha256")?,
        tools_sha256: digest("tools_sha256")?,
        settings_sha256: digest("settings_sha256")?,
        tests: [count(0)?, count(1)?],
    })
}

/// Reads the exact module set of `bundles` and requires each digest.
fn read_modules(
    bundles: &Path,
    modules: &BTreeMap<String, Module>,
) -> Result<Vec<(String, Vec<u8>)>, Report<ReleaseError>> {
    let expected: BTreeSet<_> = modules
        .values()
        .map(|module| module.filename.clone())
        .collect();
    if filenames(bundles, expected.len()).ok() != Some(expected) {
        return Err(preparation_error(
            "adopted JS bundle set differs from the inventory",
        ));
    }
    modules
        .values()
        .map(|module| {
            let raw = read_bounded(&bundles.join(&module.filename), Role::Bundle)?;
            if raw.sha256 != module.sha256 || raw.bytes.len() as u64 != module.size {
                return Err(preparation_error(
                    "adopted JS module differs from the inventory",
                ));
            }
            Ok((module.filename.clone(), raw.bytes))
        })
        .collect()
}

impl JsBuildCompleted {
    /// Exports the retained handoff and its ordinary CI `measurement` into
    /// the fresh `destination` for the macOS candidate jobs.
    ///
    /// The source handoff is rehashed before and after the copy. The caller
    /// must read and adopt the export to verify its copied bytes against the
    /// measurement before transferring or using it.
    ///
    /// # Errors
    ///
    /// Returns an error when the retained root no longer rehashes,
    /// `destination` exists or its parent does not, or a copy or the
    /// measurement cannot be written.
    pub(crate) fn export_ordinary_ci_handoff_for_tests(
        &self,
        measurement: &Value,
        destination: &Path,
    ) -> Result<(), Report<ReleaseError>> {
        let retained = self.rehash_for_handoff()?;
        fs::create_dir(destination).map_err(preparation_error)?;
        let handoff = destination.join(EXPORTED_HANDOFF);
        fs::create_dir_all(handoff.join("bundles")).map_err(preparation_error)?;
        for name in ["tsjs-inventory.json", "npm-observation.json"] {
            fs::copy(retained.join(name), handoff.join(name)).map_err(preparation_error)?;
        }
        for module in self.modules.values() {
            fs::copy(
                retained.join("bundles").join(&module.filename),
                handoff.join("bundles").join(&module.filename),
            )
            .map_err(preparation_error)?;
        }
        let encoded = serde_json::to_vec_pretty(measurement).map_err(preparation_error)?;
        fs::write(destination.join(EXPORTED_MEASUREMENT), encoded).map_err(preparation_error)?;
        self.rehash_for_handoff()?;
        Ok(())
    }

    /// Reads a root the Linux job exported: exactly the handoff directory and
    /// the measurement file, nothing else.
    ///
    /// Returns the handoff directory and the parsed measurement; neither is
    /// authenticated, and [`Self::adopt_ordinary_ci_handoff_for_tests`]
    /// checks both.
    ///
    /// # Errors
    ///
    /// Returns an error when the root holds any other entry, or the
    /// measurement cannot be read or parsed as JSON.
    pub(crate) fn read_exported_ordinary_ci_handoff_for_tests(
        root: &Path,
    ) -> Result<(PathBuf, Value), Report<ReleaseError>> {
        let expected: BTreeSet<String> = [EXPORTED_HANDOFF, EXPORTED_MEASUREMENT]
            .map(str::to_owned)
            .into();
        if filenames(root, expected.len()).ok() != Some(expected) {
            return Err(preparation_error(
                "exported JS root holds entries beyond the handoff and its measurement",
            ));
        }
        let raw = read_bounded(&root.join(EXPORTED_MEASUREMENT), Role::Metadata)?;
        let measurement = parse_json(&raw.bytes, Role::Metadata)?;
        Ok((root.join(EXPORTED_HANDOFF), measurement))
    }

    /// Adopts a Linux ordinary CI JS handoff for a macOS candidate run.
    ///
    /// Returns the private copy and a measurement labelling its origin as
    /// `linux-ordinary-ci-artifact` with `authenticated: false`.
    ///
    /// # Errors
    ///
    /// Returns an error when the measurement is not the Linux job's for this
    /// source, the handoff holds any entry beyond the three retained ones,
    /// the inventory digest differs from the measurement, the inventory does
    /// not rejoin the source or name exactly its modules, a module differs
    /// from its inventory digest, or the private copy cannot be written.
    pub(crate) fn adopt_ordinary_ci_handoff_for_tests(
        source: &ApprovedSource,
        handoff: &Path,
        measurement: &Value,
    ) -> Result<(Self, Value), Report<ReleaseError>> {
        let measured = check_measurement(source, measurement)?;
        let expected: BTreeSet<String> = HANDOFF_ENTRIES.map(str::to_owned).into();
        if filenames(handoff, expected.len()).ok() != Some(expected) {
            return Err(preparation_error(
                "adopted JS handoff holds entries beyond the retained three",
            ));
        }
        let inventory = read_bounded(&handoff.join("tsjs-inventory.json"), Role::Metadata)?;
        if inventory.sha256 != measured.inventory_sha256 {
            return Err(preparation_error(
                "adopted JS inventory differs from the measured inventory",
            ));
        }
        join_inventory(source, &inventory.bytes)?;
        let modules = inventory_modules(&inventory.bytes, &source.module_ids())?;
        let bundles = read_modules(&handoff.join("bundles"), &modules)?;
        let observation = read_bounded(&handoff.join("npm-observation.json"), Role::Metadata)?;

        let directory = tempfile::tempdir().map_err(preparation_error)?;
        fs::create_dir(directory.path().join("bundles")).map_err(preparation_error)?;
        fs::write(
            directory.path().join("tsjs-inventory.json"),
            &inventory.bytes,
        )
        .map_err(preparation_error)?;
        fs::write(
            directory.path().join("npm-observation.json"),
            &observation.bytes,
        )
        .map_err(preparation_error)?;
        for (filename, bytes) in &bundles {
            fs::write(directory.path().join("bundles").join(filename), bytes)
                .map_err(preparation_error)?;
        }
        let adopted = Self {
            directory,
            source_commit: source.commit().as_str().to_owned(),
            tools_sha256: measured.tools_sha256.to_owned(),
            settings_sha256: measured.settings_sha256.to_owned(),
            inventory_sha256: inventory.sha256.clone(),
            observation_sha256: hex::encode(Sha256::digest(&observation.bytes)),
            modules,
            tests: measured.tests,
            attempt: JsAttempt::new(Instant::now() + ADOPTED_LIFETIME, Cancellation::default()),
        };
        adopted.rehash_for_handoff()?;
        let label = json!({
            "js_handoff": {
                "origin": ADOPTED_ORIGIN,
                "authenticated": false,
                "source_commit": adopted.source_commit,
                "inventory_sha256": adopted.inventory_sha256,
                "tools_sha256": adopted.tools_sha256,
                "settings_sha256": adopted.settings_sha256,
                "module_count": adopted.modules.len(),
            }
        });
        Ok((adopted, label))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::CommitId;

    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
    const OTHER_COMMIT: &str = "fedcba9876543210fedcba9876543210fedcba98";
    const MODULE: &[u8] = b"fictional core module";

    fn source(commit: &str) -> ApprovedSource {
        ApprovedSource::in_memory_for_tests(
            &CommitId::parse(commit).expect("should parse the commit"),
            &["crates/trusted-server-js/lib/src/core/index.ts"],
        )
    }

    fn inventory(commit: &str) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "schema_version": 1,
            "source_commit": commit,
            "materials": [],
            "tools": [],
            "modules": [{
                "id": "core",
                "filename": "tsjs-core.js",
                "size": MODULE.len(),
                "sha256": hex::encode(Sha256::digest(MODULE)),
            }],
        }))
        .expect("should encode the inventory")
    }

    /// A retained Linux handoff and its measurement.
    fn handoff(commit: &str) -> (tempfile::TempDir, Value) {
        let directory = tempfile::tempdir().expect("should create the handoff");
        let root = directory.path();
        fs::create_dir(root.join("bundles")).expect("should create bundles");
        fs::write(root.join("bundles/tsjs-core.js"), MODULE).expect("should write the module");
        let inventory = inventory(commit);
        fs::write(root.join("tsjs-inventory.json"), &inventory).expect("should write inventory");
        fs::write(root.join("npm-observation.json"), b"fictional observation")
            .expect("should write the observation");
        let measurement = json!({
            "schema_version": 1,
            "kind": MEASUREMENT_KIND,
            "source_commit": commit,
            "test_counts": [3, 3],
            "module_count": 1,
            "tools_sha256": "b".repeat(64),
            "settings_sha256": "c".repeat(64),
            "inventory_sha256": hex::encode(Sha256::digest(&inventory)),
            "private_roots_removed": 2,
            "release_qualification": false,
        });
        (directory, measurement)
    }

    fn rejection(source: &ApprovedSource, root: &Path, measurement: &Value) -> String {
        match JsBuildCompleted::adopt_ordinary_ci_handoff_for_tests(source, root, measurement) {
            Ok(_) => panic!("should reject the handoff"),
            Err(error) => format!("{error:?}"),
        }
    }

    #[test]
    fn adopted_handoff_is_a_private_rejoined_copy_labelled_unauthenticated() {
        let (directory, measurement) = handoff(COMMIT);

        let (adopted, label) = JsBuildCompleted::adopt_ordinary_ci_handoff_for_tests(
            &source(COMMIT),
            directory.path(),
            &measurement,
        )
        .expect("should adopt the handoff");

        let retained = adopted
            .rehash_for_handoff()
            .expect("should rehash the copy");
        assert_ne!(
            retained,
            directory.path(),
            "should copy into its own directory"
        );
        assert_eq!(adopted.source_commit(), COMMIT, "should bind the source");
        assert_eq!(adopted.inventory_sha256(), measurement["inventory_sha256"]);
        assert_eq!(adopted.tools_sha256(), "b".repeat(64));
        assert_eq!(adopted.test_counts(), [3, 3]);
        assert_eq!(label["js_handoff"]["origin"], "linux-ordinary-ci-artifact");
        assert_eq!(label["js_handoff"]["authenticated"], false);
        assert_eq!(label["js_handoff"]["module_count"], 1);
        fs::remove_dir_all(directory.path()).expect("should remove the original");
        adopted
            .rehash_for_handoff()
            .expect("should not depend on the original artifact");
    }

    #[test]
    fn exported_handoff_reads_back_and_adopts_unchanged() {
        let (directory, measurement) = handoff(COMMIT);
        let (adopted, _) = JsBuildCompleted::adopt_ordinary_ci_handoff_for_tests(
            &source(COMMIT),
            directory.path(),
            &measurement,
        )
        .expect("should adopt the original handoff");
        let scratch = tempfile::tempdir().expect("should create the export parent");
        let exported = scratch.path().join("js-handoff");

        adopted
            .export_ordinary_ci_handoff_for_tests(&measurement, &exported)
            .expect("should export the retained handoff");
        let (handoff_root, read_measurement) =
            JsBuildCompleted::read_exported_ordinary_ci_handoff_for_tests(&exported)
                .expect("should read the exported root");

        assert_eq!(handoff_root, exported.join("handoff"));
        assert_eq!(
            read_measurement, measurement,
            "should carry the measurement unchanged"
        );
        let (readopted, label) = JsBuildCompleted::adopt_ordinary_ci_handoff_for_tests(
            &source(COMMIT),
            &handoff_root,
            &read_measurement,
        )
        .expect("should adopt the exported handoff");
        assert_eq!(readopted.inventory_sha256(), adopted.inventory_sha256());
        assert_eq!(label["js_handoff"]["authenticated"], false);
        assert!(
            adopted
                .export_ordinary_ci_handoff_for_tests(&measurement, &exported)
                .is_err(),
            "should refuse an existing destination"
        );
    }

    #[test]
    fn exported_roots_with_extra_or_missing_entries_are_rejected() {
        let scratch = tempfile::tempdir().expect("should create the export root");
        let root = scratch.path();
        fs::create_dir(root.join("handoff")).expect("should create the handoff");
        assert!(
            JsBuildCompleted::read_exported_ordinary_ci_handoff_for_tests(root).is_err(),
            "should reject a root without its measurement"
        );
        fs::write(root.join("js-execution-measurement.json"), b"{}")
            .expect("should write the measurement");
        JsBuildCompleted::read_exported_ordinary_ci_handoff_for_tests(root)
            .expect("should read an exact root");
        fs::write(root.join("extra.json"), b"{}").expect("should add an entry");
        let error = JsBuildCompleted::read_exported_ordinary_ci_handoff_for_tests(root)
            .expect_err("should reject an extra entry");
        assert!(
            format!("{error:?}").contains("beyond the handoff and its measurement"),
            "should name the unexpected entry: {error:?}"
        );
        fs::remove_file(root.join("extra.json")).expect("should remove the extra entry");
        fs::write(root.join("js-execution-measurement.json"), b"not json")
            .expect("should corrupt the measurement");
        assert!(
            JsBuildCompleted::read_exported_ordinary_ci_handoff_for_tests(root).is_err(),
            "should reject a measurement that is not JSON"
        );
    }

    #[test]
    fn handoffs_with_extra_or_changed_entries_are_rejected() {
        let (directory, measurement) = handoff(COMMIT);
        fs::write(directory.path().join("extra.json"), b"{}").expect("should add an entry");
        assert!(
            rejection(&source(COMMIT), directory.path(), &measurement)
                .contains("beyond the retained three"),
            "should reject an extra root entry"
        );

        let (directory, measurement) = handoff(COMMIT);
        fs::write(directory.path().join("bundles/tsjs-extra.js"), b"x").expect("should add");
        assert!(
            rejection(&source(COMMIT), directory.path(), &measurement)
                .contains("bundle set differs"),
            "should reject an extra bundle"
        );

        let (directory, measurement) = handoff(COMMIT);
        fs::write(
            directory.path().join("bundles/tsjs-core.js"),
            b"fictional core modulE",
        )
        .expect("should change the module");
        assert!(
            rejection(&source(COMMIT), directory.path(), &measurement)
                .contains("module differs from the inventory"),
            "should reject a module digest mismatch"
        );
    }

    #[test]
    fn handoffs_from_another_commit_or_measurement_are_rejected() {
        let (directory, measurement) = handoff(OTHER_COMMIT);
        assert!(
            rejection(&source(COMMIT), directory.path(), &measurement)
                .contains("another source commit"),
            "should reject a measurement of a foreign commit"
        );

        let (directory, mut measurement) = handoff(OTHER_COMMIT);
        measurement["source_commit"] = json!(COMMIT);
        let inventory = fs::read(directory.path().join("tsjs-inventory.json"))
            .expect("should read the inventory");
        measurement["inventory_sha256"] = json!(hex::encode(Sha256::digest(&inventory)));
        assert!(
            rejection(&source(COMMIT), directory.path(), &measurement)
                .contains("does not match the approved commit"),
            "should reject an inventory of a foreign commit"
        );

        let (directory, mut measurement) = handoff(COMMIT);
        measurement["inventory_sha256"] = json!("d".repeat(64));
        assert!(
            rejection(&source(COMMIT), directory.path(), &measurement)
                .contains("differs from the measured inventory"),
            "should reject an inventory the measurement did not record"
        );

        let (directory, mut measurement) = handoff(COMMIT);
        measurement["release_qualification"] = json!(true);
        assert!(
            rejection(&source(COMMIT), directory.path(), &measurement)
                .contains("not an ordinary CI two-build measurement"),
            "should reject a measurement claiming qualification"
        );
    }
}
