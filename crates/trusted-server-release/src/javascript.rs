//! Full observed two-installation JS builds and private retained completion.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use error_stack::Report;
use sha2::{Digest as _, Sha256};

use crate::boundary::{Boundary, DenialProof};
use crate::documents::{RawBytes, Role, parse_json, read_bounded_with_check};
use crate::error::ReleaseError;
use crate::preparation::{JsAttempt, PreparedJsBuild, preparation_error};
use crate::process::ProcessOutcome;
use crate::source::join_inventory;

pub(crate) mod contributions;
pub(crate) use contributions::RetainedContributions;

/// Exact retained module facts produced by the actual two-side comparison.
struct Module {
    filename: String,
    size: u64,
    sha256: String,
}

/// Complete module identity exported only from a rehashed owned JS handoff.
#[derive(Clone, Debug, serde::Serialize)]
pub(crate) struct RetainedJsModule {
    id: String,
    filename: String,
    size: u64,
    sha256: String,
}

impl RetainedJsModule {
    /// The approved discovered module identity.
    pub(crate) fn id(&self) -> &str {
        &self.id
    }
    /// The checked flat bundle filename.
    pub(crate) fn filename(&self) -> &str {
        &self.filename
    }
    /// Complete final bundle length.
    pub(crate) fn size(&self) -> u64 {
        self.size
    }
    /// SHA-256 of complete final bundle bytes.
    pub(crate) fn sha256(&self) -> &str {
        &self.sha256
    }
}

/// Captured immediately after the collector exits, before later writable stages.
struct CapturedInventory {
    raw: RawBytes,
    modules: BTreeMap<String, Module>,
}

fn verify_captured_output(
    output: &Path,
    captured: &CapturedInventory,
    check: &dyn Fn() -> Result<(), Report<ReleaseError>>,
) -> Result<(), Report<ReleaseError>> {
    check()?;
    let current =
        read_bounded_with_check(&output.join("tsjs-inventory.json"), Role::Metadata, check)?;
    if current.bytes != captured.raw.bytes || current.sha256 != captured.raw.sha256 {
        return Err(preparation_error(
            "original collector inventory changed after measurement",
        ));
    }
    let bundles = output.join("bundles");
    let expected: BTreeSet<_> = captured
        .modules
        .values()
        .map(|module| module.filename.clone())
        .collect();
    if filenames(&bundles, expected.len())? != expected {
        return Err(preparation_error("original collector module set changed"));
    }
    for module in captured.modules.values() {
        let current =
            read_bounded_with_check(&bundles.join(&module.filename), Role::Bundle, check)?;
        if current.sha256 != module.sha256 || current.bytes.len() as u64 != module.size {
            return Err(preparation_error("original collector module bytes changed"));
        }
    }
    check()
}

fn settings_digest(fingerprints: &[[u8; 32]; 2]) -> String {
    let mut digest = Sha256::new();
    digest.update(b"source-owned-js-two-install-v2\nnode24.20.0\nnpm11.19.0\nLinux-owned-PID-cleanup\noffline-include-dev\ninstall-umask022\nLANG=C.UTF-8\nLC_ALL=C.UTF-8\nTZ=UTC\nrelease-tests-1\nfull-registered-typecheck\nfull-final-module-bytes\n");
    for fingerprint in fingerprints {
        digest.update(fingerprint);
    }
    hex::encode(digest.finalize())
}

fn finalize_execution(
    execution: Result<JsBuildCompleted, Report<ReleaseError>>,
    cleanup: Result<(), Report<ReleaseError>>,
) -> Result<JsBuildCompleted, Report<ReleaseError>> {
    match (execution, cleanup) {
        (Ok(completed), Ok(())) => {
            completed.rehash_for_handoff()?;
            Ok(completed)
        }
        (Ok(_), Err(error)) => Err(error),
        (Err(error), Ok(())) => Err(error),
        (Err(error), Err(cleanup)) => {
            Err(error.attach(format!("private JS cleanup also failed: {cleanup:?}")))
        }
    }
}

/// A completed private execution, unavailable from JSON assertions or paths.
///
/// Only the actual successful preparation/install/build/test/negative/control/
/// equality path below constructs this value. Production tool selection remains
/// unavailable until authenticated bootstrap and qualification are complete.
pub(crate) struct JsBuildCompleted {
    directory: tempfile::TempDir,
    source_commit: String,
    tools_sha256: String,
    settings_sha256: String,
    inventory_sha256: String,
    observation_sha256: String,
    modules: BTreeMap<String, Module>,
    tests: [u64; 2],
    attempt: JsAttempt,
}

impl JsBuildCompleted {
    /// Exports conservative rendered origins from the checked original inventory.
    ///
    /// # Errors
    ///
    /// Rejects changed handoffs, expired attempts and inconsistent origin joins.
    pub(crate) fn retained_contributions(
        &self,
    ) -> Result<RetainedContributions, Report<ReleaseError>> {
        contributions::export(self)
    }
    /// Exports module facts after checking the complete retained handoff.
    ///
    /// # Errors
    ///
    /// Rejects changed files, aliases or expired preparation attempts.
    pub(crate) fn retained_modules(&self) -> Result<Vec<RetainedJsModule>, Report<ReleaseError>> {
        self.rehash_for_handoff()?;
        Ok(self
            .modules
            .iter()
            .map(|(id, module)| RetainedJsModule {
                id: id.clone(),
                filename: module.filename.clone(),
                size: module.size,
                sha256: module.sha256.clone(),
            })
            .collect())
    }

    /// Returns the approved source bound to the executed build.
    pub(crate) fn source_commit(&self) -> &str {
        &self.source_commit
    }
    /// Returns the exact selected native tool-input digest.
    pub(crate) fn tools_sha256(&self) -> &str {
        &self.tools_sha256
    }
    /// Returns the fixed recipe and both actual execution-boundary settings digest.
    pub(crate) fn settings_sha256(&self) -> &str {
        &self.settings_sha256
    }
    /// Returns the raw original A inventory digest.
    pub(crate) fn inventory_sha256(&self) -> &str {
        &self.inventory_sha256
    }
    /// Returns the successful registered test counts of both independent JS builds.
    pub(crate) fn test_counts(&self) -> [u64; 2] {
        self.tests
    }

    /// Rejoins the retained A inventory and all exact module bytes before handoff.
    ///
    /// # Errors
    ///
    /// Returns an error if an owned retained file changed, vanished or appeared.
    pub(crate) fn rehash_for_handoff(&self) -> Result<&Path, Report<ReleaseError>> {
        self.attempt.check()?;
        let expected = BTreeSet::from([
            "bundles".to_owned(),
            "tsjs-inventory.json".to_owned(),
            "npm-observation.json".to_owned(),
        ]);
        if filenames(self.directory.path(), expected.len())? != expected {
            return Err(preparation_error(
                "retained JS root contains an unrecorded entry",
            ));
        }
        let inventory = read_bounded_with_check(
            &self.directory.path().join("tsjs-inventory.json"),
            Role::Metadata,
            &|| self.attempt.check(),
        )?;
        if inventory.sha256 != self.inventory_sha256 {
            return Err(preparation_error("retained original JS inventory changed"));
        }
        let observation = read_bounded_with_check(
            &self.directory.path().join("npm-observation.json"),
            Role::Metadata,
            &|| self.attempt.check(),
        )?;
        if observation.sha256 != self.observation_sha256 {
            return Err(preparation_error("retained actual npm observation changed"));
        }
        let bundles = self.directory.path().join("bundles");
        let expected: BTreeSet<_> = self
            .modules
            .values()
            .map(|module| module.filename.clone())
            .collect();
        let actual = filenames(&bundles, expected.len())?;
        if actual != expected {
            return Err(preparation_error("retained JS module set changed"));
        }
        for module in self.modules.values() {
            let raw =
                read_bounded_with_check(&bundles.join(&module.filename), Role::Bundle, &|| {
                    self.attempt.check()
                })?;
            if raw.sha256 != module.sha256 || raw.bytes.len() as u64 != module.size {
                return Err(preparation_error("retained JS module changed"));
            }
        }
        Ok(self.directory.path())
    }
}

#[cfg(test)]
impl JsBuildCompleted {
    /// Builds a retained fixture bound to `source_commit` without executing a build.
    ///
    /// Only tests construct this; production completion stays on the real path.
    pub(crate) fn fixture_for_tests(source_commit: &str) -> Self {
        let directory = tempfile::tempdir().expect("should create retained fixture");
        fs::create_dir(directory.path().join("bundles")).expect("should create bundle root");
        let inventory = b"fictional original inventory";
        let observation = b"fictional private observation";
        let module = b"fictional module";
        fs::write(directory.path().join("tsjs-inventory.json"), inventory)
            .expect("should write exact fixture inventory");
        fs::write(directory.path().join("npm-observation.json"), observation)
            .expect("should write exact fixture observation");
        fs::write(directory.path().join("bundles/tsjs-core.js"), module)
            .expect("should write fixture module");
        JsBuildCompleted {
            directory,
            source_commit: source_commit.to_owned(),
            tools_sha256: "b".repeat(64),
            settings_sha256: "c".repeat(64),
            inventory_sha256: hex::encode(Sha256::digest(inventory)),
            observation_sha256: hex::encode(Sha256::digest(observation)),
            modules: BTreeMap::from([(
                "core".to_owned(),
                Module {
                    filename: "tsjs-core.js".to_owned(),
                    size: module.len() as u64,
                    sha256: hex::encode(Sha256::digest(module)),
                },
            )]),
            tests: [1, 1],
            // Real-fixture denial cases bind after a full Apple developer-tool
            // verify, which exceeded a 30-second lifetime on a hosted
            // macos-15 runner (run 39552419463), so allow ten minutes.
            attempt: JsAttempt::new(
                std::time::Instant::now() + std::time::Duration::from_secs(10 * 60),
                crate::process::Cancellation::default(),
            ),
        }
    }
}

fn filenames(root: &Path, limit: usize) -> Result<BTreeSet<String>, Report<ReleaseError>> {
    if !fs::symlink_metadata(root)
        .map_err(preparation_error)?
        .is_dir()
    {
        return Err(preparation_error(
            "retained JS directory changed type or became an alias",
        ));
    }
    let mut names = BTreeSet::new();
    for entry in fs::read_dir(root).map_err(preparation_error)? {
        let name = entry
            .map_err(preparation_error)?
            .file_name()
            .into_string()
            .map_err(|_| preparation_error("bundle name is not UTF-8"))?;
        if !names.insert(name) || names.len() > limit {
            return Err(preparation_error("unexpected retained module entries"));
        }
    }
    Ok(names)
}

fn inventory_modules(
    bytes: &[u8],
    expected: &[String],
) -> Result<BTreeMap<String, Module>, Report<ReleaseError>> {
    let inventory = parse_json(bytes, Role::Metadata)?;
    let array = inventory["modules"]
        .as_array()
        .ok_or_else(|| preparation_error("JS inventory has no module table"))?;
    let mut modules = BTreeMap::new();
    let mut previous = None::<String>;
    for entry in array {
        let id = entry["id"]
            .as_str()
            .ok_or_else(|| preparation_error("JS module has no id"))?;
        if !expected.iter().any(|expected| expected == id)
            || previous.as_deref().is_some_and(|previous| previous >= id)
        {
            return Err(preparation_error(
                "JS module set is untracked, duplicate or unordered",
            ));
        }
        let filename = entry["filename"]
            .as_str()
            .ok_or_else(|| preparation_error("JS module has no filename"))?;
        if filename != format!("tsjs-{id}.js") {
            return Err(preparation_error(
                "JS module filename differs from tracked identity",
            ));
        }
        let size = entry["size"]
            .as_u64()
            .filter(|size| *size > 0 && *size <= 32 * 1024 * 1024)
            .ok_or_else(|| preparation_error("JS module size is invalid"))?;
        let sha256 = entry["sha256"]
            .as_str()
            .filter(|digest| {
                digest.len() == 64
                    && digest
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            })
            .ok_or_else(|| preparation_error("JS module digest is invalid"))?;
        modules.insert(
            id.to_owned(),
            Module {
                filename: filename.to_owned(),
                size,
                sha256: sha256.to_owned(),
            },
        );
        previous = Some(id.to_owned());
    }
    if modules.keys().ne(expected.iter()) {
        return Err(preparation_error(
            "JS inventory does not cover every tracked module",
        ));
    }
    Ok(modules)
}

fn compare_modules(
    first: &Path,
    second: &Path,
    modules: &BTreeMap<String, Module>,
    check: &dyn Fn() -> Result<(), Report<ReleaseError>>,
) -> Result<(), Report<ReleaseError>> {
    let expected: BTreeSet<_> = modules
        .values()
        .map(|module| module.filename.clone())
        .collect();
    if filenames(first, expected.len())? != expected
        || filenames(second, expected.len())? != expected
    {
        return Err(preparation_error(
            "independent JS build bundle sets differ from approved source",
        ));
    }
    for module in modules.values() {
        check()?;
        let first = read_bounded_with_check(&first.join(&module.filename), Role::Bundle, check)?;
        let second = read_bounded_with_check(&second.join(&module.filename), Role::Bundle, check)?;
        if first.bytes != second.bytes
            || first.sha256 != module.sha256
            || first.bytes.len() as u64 != module.size
        {
            return Err(preparation_error(
                "final JS module bytes differ between the independent builds",
            ));
        }
    }
    Ok(())
}

fn test_report(
    bytes: &[u8],
    expected: &BTreeSet<String>,
    execution_library: &Path,
) -> Result<u64, Report<ReleaseError>> {
    let report = parse_json(bytes, Role::Companion)?;
    let total = report["numTotalTests"]
        .as_u64()
        .filter(|total| *total > 0)
        .ok_or_else(|| preparation_error("registered JS test suite selected zero tests"))?;
    if report["success"] != true
        || report["numFailedTests"] != 0
        || report["numPassedTests"] != total
        || report["numPendingTests"] != 0
    {
        return Err(preparation_error(
            "full registered JS tests did not all pass",
        ));
    }
    let results = report["testResults"]
        .as_array()
        .ok_or_else(|| preparation_error("JS tests omitted per-file results"))?;
    let mut actual = BTreeSet::new();
    for result in results {
        let name = result["name"]
            .as_str()
            .ok_or_else(|| preparation_error("JS test result lacks its file"))?;
        let relative = Path::new(name)
            .strip_prefix(execution_library)
            .map_err(preparation_error)?
            .to_str()
            .ok_or_else(|| preparation_error("JS test result is not UTF-8"))?;
        if result["status"] != "passed" || !expected.contains(relative) {
            return Err(preparation_error(
                "JS test file failed or is not registered",
            ));
        }
        actual.insert(relative.to_owned());
    }
    if actual != *expected {
        return Err(preparation_error(
            "JS test/type-check report omits registered files",
        ));
    }
    Ok(total)
}

// Failure diagnostics deliberately exclude assertions, stacks and child output.
fn vitest_failure_diagnostic(
    bytes: &[u8],
    expected: &BTreeSet<String>,
    execution_library: &Path,
) -> Result<String, Report<ReleaseError>> {
    let report = parse_json(bytes, Role::Companion)?;
    let results = report["testResults"]
        .as_array()
        .ok_or_else(|| preparation_error("Vitest report lacks suite results"))?;
    let sanitize = |name: &str| -> String {
        name.chars()
            .take(256)
            .map(|character| {
                if character.is_ascii() && !character.is_ascii_control() {
                    character
                } else {
                    '?'
                }
            })
            .collect()
    };
    let mut names = Vec::new();
    for result in results {
        if names.len() >= 16 {
            break;
        }
        let Some(relative) = result["name"].as_str().and_then(|name| {
            Path::new(name)
                .strip_prefix(execution_library)
                .ok()?
                .to_str()
        }) else {
            continue;
        };
        if !expected.contains(relative) {
            continue;
        }
        if result["status"] == "failed" {
            names.push(format!("suite failed: {}", sanitize(relative)));
        }
        if let Some(assertions) = result["assertionResults"].as_array() {
            for assertion in assertions {
                if names.len() >= 16 {
                    break;
                }
                if assertion["status"] == "failed"
                    && let Some(name) = assertion["fullName"].as_str()
                {
                    names.push(format!("test failed: {}", sanitize(name)));
                }
            }
        }
    }
    Ok(if names.is_empty() {
        "Vitest failed without registered failed names".to_owned()
    } else {
        format!("Vitest failed names (at most 16): {}", names.join("; "))
    })
}

struct MissingFiles {
    paths: Vec<(PathBuf, PathBuf)>,
    _directory: tempfile::TempDir,
}
impl MissingFiles {
    fn remove(paths: &[PathBuf]) -> Result<Self, Report<ReleaseError>> {
        let directory = tempfile::tempdir().map_err(preparation_error)?;
        let mut missing = Self {
            paths: Vec::new(),
            _directory: directory,
        };
        for (index, path) in paths.iter().enumerate() {
            let destination = missing._directory.path().join(index.to_string());
            fs::rename(path, &destination).map_err(preparation_error)?;
            missing.paths.push((path.clone(), destination));
        }
        Ok(missing)
    }
    fn restore(&mut self) -> Result<(), Report<ReleaseError>> {
        while let Some((original, temporary)) = self.paths.last() {
            fs::rename(temporary, original).map_err(preparation_error)?;
            self.paths.pop();
        }
        Ok(())
    }
}
impl Drop for MissingFiles {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

impl PreparedJsBuild<'_> {
    fn prohibited_install_download_probe(
        &self,
        index: usize,
        boundary: &Boundary,
        proof: &DenialProof,
        captured: &CapturedInventory,
    ) -> Result<(), Report<ReleaseError>> {
        let side = &self.sides[index];
        let probe = side
            .state
            .path()
            .join("install-download/node_modules/esbuild");
        fs::create_dir_all(probe.join("lib")).map_err(preparation_error)?;
        let mut originals = BTreeMap::new();
        for relative in ["install.js", "package.json", "lib/main.js"] {
            let raw = read_bounded_with_check(
                &side.library().join("node_modules/esbuild").join(relative),
                Role::Bundle,
                &|| self.attempt.check(),
            )?;
            fs::write(probe.join(relative), &raw.bytes).map_err(preparation_error)?;
            originals.insert(relative, raw.sha256);
        }
        // This separate tree contains neither a native package nor an optimized
        // binary. Omitting npm from PATH exercises esbuild's actual direct
        // download fallback without waiting through npm's network retries.
        let command = self
            .command_with_npm_path(index, boundary, false)?
            .arguments([boundary
                .execution_path(&probe.join("install.js"))?
                .into_os_string()]);
        let result = boundary.run_confined(&command, proof)?;
        verify_captured_output(&side.output(), captured, &|| self.attempt.check())?;
        let stderr = std::str::from_utf8(result.stderr()).map_err(preparation_error)?;
        if result.outcome() != (ProcessOutcome::Exited { code: 1 })
            || !stderr.contains("[esbuild] Trying to download ")
            || !stderr.contains("Failed to install package")
        {
            return Err(preparation_error(
                "actual missing-native install download did not fail under denial",
            ));
        }
        let mut pending = vec![probe.clone()];
        let mut remaining = originals;
        while let Some(directory) = pending.pop() {
            self.attempt.check()?;
            for entry in fs::read_dir(directory).map_err(preparation_error)? {
                let entry = entry.map_err(preparation_error)?;
                let path = entry.path();
                let metadata = fs::symlink_metadata(&path).map_err(preparation_error)?;
                if metadata.is_dir() {
                    pending.push(path);
                } else {
                    let relative = path
                        .strip_prefix(&probe)
                        .map_err(preparation_error)?
                        .to_str()
                        .ok_or_else(|| preparation_error("probe member is not UTF-8"))?;
                    let expected = remaining.remove(relative).ok_or_else(|| {
                        preparation_error("denied install produced a replacement native tool")
                    })?;
                    if read_bounded_with_check(&path, Role::Bundle, &|| self.attempt.check())?
                        .sha256
                        != expected
                    {
                        return Err(preparation_error("denied install changed probe inputs"));
                    }
                }
            }
        }
        if !remaining.is_empty() {
            return Err(preparation_error("denied install removed probe inputs"));
        }
        side.verify_source()?;
        self.check()
    }

    fn missing_tool_probes(
        &self,
        index: usize,
        boundary: &Boundary,
        proof: &DenialProof,
        captured: &CapturedInventory,
    ) -> Result<(), Report<ReleaseError>> {
        let side = &self.sides[index];
        let architecture = match std::env::consts::ARCH {
            "x86_64" => "x64",
            "aarch64" => "arm64",
            _ => {
                return Err(preparation_error(
                    "unsupported native JS probe architecture",
                ));
            }
        };
        let cases = [
            (vec![side.library().join(format!("node_modules/@esbuild/linux-{architecture}/bin/esbuild")), side.library().join("node_modules/esbuild/bin/esbuild"), side.library().join("node_modules/.bin/esbuild")], "const e=require('esbuild'); e.transformSync('const x=1',{minify:true})"),
            (vec![side.library().join(format!("node_modules/@rollup/rollup-linux-{architecture}-gnu/rollup.linux-{architecture}-gnu.node"))], "require('rollup')"),
        ];
        for (paths, operation) in cases {
            let mut missing = MissingFiles::remove(&paths)?;
            let script = format!(
                "try{{{operation};process.exitCode=0}}catch{{process.stdout.write('TS_RELEASE_MISSING_TOOL_DENIED\\n');process.exitCode=73}}"
            );
            let command = self
                .command(index, boundary)?
                .arguments([OsString::from("--eval"), OsString::from(script)]);
            let result = boundary.run_confined(&command, proof);
            missing.restore()?;
            let result = result?;
            verify_captured_output(&side.output(), captured, &|| self.attempt.check())?;
            if result.outcome() != (ProcessOutcome::Exited { code: 73 })
                || result.stdout() != b"TS_RELEASE_MISSING_TOOL_DENIED\n"
            {
                return Err(preparation_error(
                    "missing native tool fetched a substitute or lacked an actual denied failure",
                ));
            }
            side.verify_source()?;
            self.check()?;
        }
        Ok(())
    }

    /// Executes both independent full builds under their exact proven boundaries.
    ///
    /// # Errors
    ///
    /// Returns an error for any input mutation, installation/hook/control failure,
    /// failed or incomplete test/type-check suite, or any final module difference.
    pub(crate) fn run(
        self,
        boundaries: [&Boundary; 2],
        proofs: [&DenialProof; 2],
    ) -> Result<JsBuildCompleted, Report<ReleaseError>> {
        let execution = self.execute(boundaries, proofs);
        let cleanup = self.cleanup();
        finalize_execution(execution, cleanup)
    }

    fn execute(
        &self,
        boundaries: [&Boundary; 2],
        proofs: [&DenialProof; 2],
    ) -> Result<JsBuildCompleted, Report<ReleaseError>> {
        for (index, boundary) in boundaries.iter().enumerate() {
            if !boundary.matches_roots(&self.boundary_roots(index)?)
                || !boundary.has_owned_descendant_cleanup()
            {
                return Err(preparation_error(
                    "JS execution requires independent exact Linux boundary roots",
                ));
            }
        }
        let mut counts = [0_u64; 2];
        let mut observations = Vec::new();
        let mut captured_inventories = Vec::new();
        for index in 0..2 {
            observations.push(self.install(index, boundaries[index], proofs[index])?);
            self.run_command(
                index,
                boundaries[index],
                proofs[index],
                self.collector_arguments(index, boundaries[index])?,
            )?;
            let side = &self.sides[index];
            let raw = read_bounded_with_check(
                &side.output().join("tsjs-inventory.json"),
                Role::Metadata,
                &|| self.attempt.check(),
            )?;
            join_inventory(self.source, &raw.bytes)?;
            let modules = inventory_modules(&raw.bytes, &self.module_ids)?;
            let captured = CapturedInventory { raw, modules };
            verify_captured_output(&side.output(), &captured, &|| self.attempt.check())?;
            self.missing_tool_probes(index, boundaries[index], proofs[index], &captured)?;
            self.prohibited_install_download_probe(
                index,
                boundaries[index],
                proofs[index],
                &captured,
            )?;
            let arguments = vec![
                boundaries[index]
                    .execution_path(&side.library().join("node_modules/vitest/vitest.mjs"))?
                    .into_os_string(),
                OsString::from("run"),
                OsString::from("--reporter=json"),
                OsString::from("--outputFile"),
                boundaries[index]
                    .execution_path(&side.output().join("vitest.json"))?
                    .into_os_string(),
            ];
            if let Err(mut error) =
                self.run_command(index, boundaries[index], proofs[index], arguments)
            {
                let diagnostic = read_bounded_with_check(
                    &side.output().join("vitest.json"),
                    Role::Companion,
                    &|| self.attempt.check(),
                )
                .and_then(|report| {
                    vitest_failure_diagnostic(
                        &report.bytes,
                        &side.expected_tests,
                        &boundaries[index].execution_path(&side.library())?,
                    )
                });
                error = error.attach(
                    diagnostic
                        .unwrap_or_else(|_| "Bounded Vitest failure names unavailable".to_owned()),
                );
                if let Err(changed) =
                    verify_captured_output(&side.output(), &captured, &|| self.attempt.check())
                {
                    error = error.attach(format!(
                        "post-failure collector recheck failed: {changed:?}"
                    ));
                }
                if let Err(changed) = side.verify_source() {
                    error =
                        error.attach(format!("post-failure source recheck failed: {changed:?}"));
                }
                return Err(error);
            }
            verify_captured_output(&side.output(), &captured, &|| self.attempt.check())?;
            let report = read_bounded_with_check(
                &side.output().join("vitest.json"),
                Role::Companion,
                &|| self.attempt.check(),
            )?;
            counts[index] = test_report(
                &report.bytes,
                &side.expected_tests,
                &boundaries[index].execution_path(&side.library())?,
            )?;
            side.verify_source()?;
            captured_inventories.push(captured);
        }
        self.check()?;
        for (index, captured) in captured_inventories.iter().enumerate() {
            verify_captured_output(&self.sides[index].output(), captured, &|| {
                self.attempt.check()
            })?;
        }
        let [first, second]: [CapturedInventory; 2] = captured_inventories
            .try_into()
            .map_err(|_| preparation_error("both original collector inventories are required"))?;
        let modules = first.modules;
        for (id, first) in &modules {
            let last = second
                .modules
                .get(id)
                .ok_or_else(|| preparation_error("B omitted a tracked module"))?;
            if first.sha256 != last.sha256 || first.size != last.size {
                return Err(preparation_error(
                    "independent JS build inventory module measurements differ",
                ));
            }
        }
        compare_modules(
            &self.sides[0].output().join("bundles"),
            &self.sides[1].output().join("bundles"),
            &modules,
            &|| self.attempt.check(),
        )?;
        let directory = tempfile::tempdir().map_err(preparation_error)?;
        fs::create_dir(directory.path().join("bundles")).map_err(preparation_error)?;
        fs::write(
            directory.path().join("tsjs-inventory.json"),
            &first.raw.bytes,
        )
        .map_err(preparation_error)?;
        let observation = &observations
            .first()
            .ok_or_else(|| preparation_error("actual A installation observation is absent"))?
            .raw;
        fs::write(directory.path().join("npm-observation.json"), observation)
            .map_err(preparation_error)?;
        for module in modules.values() {
            let raw = read_bounded_with_check(
                &self.sides[0]
                    .output()
                    .join("bundles")
                    .join(&module.filename),
                Role::Bundle,
                &|| self.attempt.check(),
            )?;
            if raw.sha256 != module.sha256 || raw.bytes.len() as u64 != module.size {
                return Err(preparation_error("A output changed before retention"));
            }
            fs::write(
                directory.path().join("bundles").join(&module.filename),
                raw.bytes,
            )
            .map_err(preparation_error)?;
        }
        let completed = JsBuildCompleted {
            directory,
            source_commit: self.source.commit().as_str().to_owned(),
            tools_sha256: self.tools_digest.clone(),
            settings_sha256: settings_digest(&[
                boundaries[0].execution_fingerprint(),
                boundaries[1].execution_fingerprint(),
            ]),
            inventory_sha256: first.raw.sha256,
            observation_sha256: hex::encode(Sha256::digest(observation)),
            modules,
            tests: counts,
            attempt: self.attempt.clone(),
        };
        completed.rehash_for_handoff()?;
        Ok(completed)
    }
}

#[cfg(test)]
mod tests;
