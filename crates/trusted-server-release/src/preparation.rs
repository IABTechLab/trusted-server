//! Fresh, independently owned npm installation state and authenticated observations.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use error_stack::{Report, ResultExt as _};
use hmac::{Hmac, KeyInit as _, Mac as _};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use crate::boundary::{Boundary, BoundaryRoots, ConfinedCommand, DenialProof, random_bytes};
#[cfg(test)]
use crate::documents::hash_bounded_with_check;
use crate::documents::{Role, parse_json};
use crate::error::ReleaseError;
use crate::materials::{LockedMaterials, PreparedNpmOriginals, RootManifestReview};
use crate::process::{Cancellation, ProcessLimits};
use crate::source::{ApprovedSource, FreshSource};
use crate::tool_tree::tool_tree_digest;

pub(super) const LIB: &str = "crates/trusted-server-js/lib";
const PREFIX: &[u8] = b"TS_RELEASE_NPM_OBSERVATION_V1 ";

pub(super) fn preparation_error(detail: impl std::fmt::Display) -> Report<ReleaseError> {
    Report::new(ReleaseError::Material).attach(detail.to_string())
}

/// Authenticated and qualified JS tool selections are supplied by Task 3/8.
///
/// None is qualified yet; ordinary CI fixtures use an explicit test constructor.
#[derive(Clone, Copy)]
pub(crate) enum QualifiedJsTools {}

/// Fixed Node/npm/guard/probe layout, retained as a measured local candidate.
pub(crate) struct SelectedJsTools {
    root: PathBuf,
}

impl SelectedJsTools {
    /// Accepts only a qualified selection; production construction stays closed.
    pub(crate) fn qualified(_root: &Path, qualification: QualifiedJsTools) -> Self {
        match qualification {}
    }

    /// Selects ordinary CI tools without asserting release qualification.
    ///
    /// # Errors
    ///
    /// Returns an error for absent or unsupported candidate tool paths.
    #[cfg(test)]
    pub(crate) fn for_tests(root: &Path) -> Result<Self, Report<ReleaseError>> {
        let root = fs::canonicalize(root).map_err(preparation_error)?;
        for relative in [
            "bin/node",
            "lib/node_modules/npm/bin/npm-cli.js",
            "guard.node",
            "probe",
        ] {
            hash_bounded_with_check(&root.join(relative), 512 * 1024 * 1024, &|| Ok(()))?;
        }
        Ok(Self { root })
    }

    pub(super) fn node(&self) -> PathBuf {
        self.root.join("bin/node")
    }
    fn npm(&self) -> PathBuf {
        self.root.join("lib/node_modules/npm/bin/npm-cli.js")
    }
    fn guard(&self) -> PathBuf {
        self.root.join("guard.node")
    }

    fn digest(&self, attempt: &JsAttempt) -> Result<String, Report<ReleaseError>> {
        tool_tree_digest(&self.root, &|| attempt.check())
    }
}

/// One shared bounded preparation attempt, independent of inherited environment.
#[derive(Clone)]
pub(crate) struct JsAttempt {
    pub(super) deadline: Instant,
    pub(super) cancellation: Cancellation,
}

impl JsAttempt {
    /// Creates a bounded attempt using an explicitly selected deadline and cancellation.
    pub(crate) fn new(deadline: Instant, cancellation: Cancellation) -> Self {
        Self {
            deadline,
            cancellation,
        }
    }
    pub(super) fn check(&self) -> Result<(), Report<ReleaseError>> {
        if self.cancellation.is_cancelled() || Instant::now() >= self.deadline {
            return Err(preparation_error("JS preparation cancelled or timed out"));
        }
        Ok(())
    }
}

/// One side has its own source, installation, cache, home, temporary and output roots.
pub(super) struct Side {
    pub(super) source: FreshSource,
    pub(super) state: tempfile::TempDir,
    metadata: tempfile::TempDir,
    pub(super) expected_tests: BTreeSet<String>,
}

impl Side {
    fn new(
        source: FreshSource,
        module_ids: &[String],
        attempt: &JsAttempt,
    ) -> Result<Self, Report<ReleaseError>> {
        let state = tempfile::tempdir().map_err(preparation_error)?;
        let metadata = tempfile::tempdir().map_err(preparation_error)?;
        for relative in ["cache", "home", "tmp", "output", "output/bundles"] {
            fs::create_dir(state.path().join(relative)).map_err(preparation_error)?;
        }
        let context =
            json!({"schema_version":1,"source_commit":source.commit(),"module_ids":module_ids});
        fs::write(
            metadata.path().join("source-context.json"),
            serde_json::to_vec(&context).map_err(preparation_error)?,
        )
        .map_err(preparation_error)?;
        let expected_tests = discover_tests(&source.root().join(LIB), attempt)?;
        Ok(Self {
            source,
            state,
            metadata,
            expected_tests,
        })
    }
    pub(super) fn output(&self) -> PathBuf {
        self.state.path().join("output")
    }
    pub(super) fn library(&self) -> PathBuf {
        self.source.root().join(LIB)
    }
    pub(super) fn verify_source(&self) -> Result<(), Report<ReleaseError>> {
        self.source
            .verify_unchanged(&["crates/trusted-server-js/lib/node_modules"])
    }
}

fn discover_tests(
    root: &Path,
    attempt: &JsAttempt,
) -> Result<BTreeSet<String>, Report<ReleaseError>> {
    let mut pending = vec![root.join("test")];
    let mut tests = BTreeSet::new();
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory).map_err(preparation_error)? {
            attempt.check()?;
            let entry = entry.map_err(preparation_error)?;
            let metadata = fs::symlink_metadata(entry.path()).map_err(preparation_error)?;
            if metadata.is_dir() {
                pending.push(entry.path());
            } else if metadata.is_file() {
                let path = entry.path();
                let name = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .ok_or_else(|| preparation_error("test path is not UTF-8"))?;
                if [
                    ".test.ts",
                    ".test.mjs",
                    ".test.js",
                    ".spec.ts",
                    ".spec.mjs",
                    ".spec.js",
                ]
                .iter()
                .any(|suffix| name.ends_with(suffix))
                {
                    tests.insert(
                        path.strip_prefix(root)
                            .map_err(preparation_error)?
                            .to_str()
                            .ok_or_else(|| preparation_error("test path is not UTF-8"))?
                            .to_owned(),
                    );
                }
            } else {
                return Err(preparation_error(
                    "test registration contains an unsafe file type",
                ));
            }
        }
    }
    if tests.is_empty() || !tests.iter().any(|path| path.ends_with("types.test.ts")) {
        return Err(preparation_error(
            "full JS and registered type-check suites are required",
        ));
    }
    Ok(tests)
}

/// Fresh two-side state, without any installation or completion authority yet.
pub(crate) struct PreparedJsBuild<'a> {
    pub(super) source: &'a ApprovedSource,
    pub(super) originals: &'a PreparedNpmOriginals,
    pub(super) tools: SelectedJsTools,
    pub(super) tools_digest: String,
    pub(super) module_ids: Vec<String>,
    pub(super) sides: [Side; 2],
    pub(super) attempt: JsAttempt,
    declarations: BTreeMap<(String, String), String>,
    _root_review: RootManifestReview,
}

/// Private measured installation data, constructed only after HMAC and joins.
pub(super) struct ObservedInstall {
    preparation: Value,
    pub(super) raw: Vec<u8>,
}

impl<'a> PreparedJsBuild<'a> {
    /// Closes every independent source, installation and metadata root, even after a failure.
    ///
    /// # Errors
    ///
    /// Returns all observed cleanup failures; expiration never prevents cleanup.
    pub(super) fn cleanup(self) -> Result<(), Report<ReleaseError>> {
        let mut failure = None::<Report<ReleaseError>>;
        for side in self.sides {
            for result in [
                side.source.close(),
                side.state.close().map_err(preparation_error),
                side.metadata.close().map_err(preparation_error),
            ] {
                if let Err(error) = result {
                    failure = Some(match failure.take() {
                        Some(previous) => {
                            previous.attach(format!("another private JS cleanup failed: {error:?}"))
                        }
                        None => error.attach("private JS resource cleanup failed"),
                    });
                }
            }
        }
        match failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
    /// Creates both independent source/state trees from approved tracked bytes.
    ///
    /// # Errors
    ///
    /// Returns an error for changed/unjoined inputs, root hooks or failed private state.
    pub(crate) fn new(
        source: &'a ApprovedSource,
        locked: &LockedMaterials,
        originals: &'a PreparedNpmOriginals,
        tools: SelectedJsTools,
        attempt: JsAttempt,
    ) -> Result<Self, Report<ReleaseError>> {
        attempt.check()?;
        let root_review = locked.review_root(source)?;
        let lock = source.read_tracked(
            "crates/trusted-server-js/lib/package-lock.json",
            Role::Metadata,
        )?;
        if originals.source_binding()
            != (
                source.commit().as_str(),
                hex::encode(Sha256::digest(lock)).as_str(),
            )
        {
            return Err(preparation_error(
                "npm originals are bound to different approved source or lockfile",
            ));
        }
        originals.verify_unchanged()?;
        let first = source.materialize(attempt.deadline, &attempt.cancellation)?;
        let module_ids = first.module_ids();
        if module_ids.is_empty() || !module_ids.iter().any(|id| id == "core") {
            return Err(preparation_error(
                "approved source lists no full JS module set",
            ));
        }
        let sides = [
            Side::new(first, &module_ids, &attempt)?,
            Side::new(
                source.materialize(attempt.deadline, &attempt.cancellation)?,
                &module_ids,
                &attempt,
            )?,
        ];
        let tools_digest = tools.digest(&attempt)?;
        let declarations = originals.declared_hooks();
        Ok(Self {
            source,
            originals,
            tools,
            tools_digest,
            module_ids,
            sides,
            attempt,
            declarations,
            _root_review: root_review,
        })
    }

    /// Returns exactly one side's roots; the other side is never mounted.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown side or unsafe/overlapping roots.
    pub(crate) fn boundary_roots(
        &self,
        index: usize,
    ) -> Result<BoundaryRoots, Report<ReleaseError>> {
        let side = self
            .sides
            .get(index)
            .ok_or_else(|| preparation_error("unknown JS comparison side"))?;
        BoundaryRoots::new(
            &[
                self.originals.root().to_owned(),
                side.metadata.path().to_owned(),
            ],
            std::slice::from_ref(&self.tools.root),
            &[side.source.root().to_owned(), side.state.path().to_owned()],
        )
    }

    pub(super) fn check(&self) -> Result<(), Report<ReleaseError>> {
        self.attempt.check()?;
        self.originals.verify_unchanged()?;
        if self.tools.digest(&self.attempt)? != self.tools_digest {
            return Err(preparation_error("selected JS tools changed"));
        }
        Ok(())
    }

    pub(super) fn command(
        &self,
        index: usize,
        boundary: &Boundary,
    ) -> Result<ConfinedCommand, Report<ReleaseError>> {
        self.command_with_npm_path(index, boundary, true)
    }

    pub(super) fn command_with_npm_path(
        &self,
        index: usize,
        boundary: &Boundary,
        include_npm: bool,
    ) -> Result<ConfinedCommand, Report<ReleaseError>> {
        self.check()?;
        let side = &self.sides[index];
        let remaining = self
            .attempt
            .deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| preparation_error("JS deadline expired"))?;
        let bin = boundary
            .execution_path(&self.tools.node())?
            .parent()
            .ok_or_else(|| preparation_error("selected Node has no parent"))?
            .to_owned();
        let path = if include_npm {
            format!("{}:/usr/bin:/bin", bin.display())
        } else {
            "/usr/bin:/bin".to_owned()
        };
        ConfinedCommand::new(
            &self.tools.node(),
            &side.library(),
            ProcessLimits {
                wall: remaining.min(Duration::from_secs(1200)),
                stdout_max: 16 * 1024 * 1024,
                stderr_max: 16 * 1024 * 1024,
            },
        )?
        .environment("PATH", path)?
        .environment(
            "HOME",
            boundary.execution_path(&side.state.path().join("home"))?,
        )?
        .environment(
            "TMPDIR",
            boundary.execution_path(&side.state.path().join("tmp"))?,
        )?
        .environment("LANG", "C.UTF-8")?
        .environment("LC_ALL", "C.UTF-8")?
        .environment("TZ", "UTC")?
        .environment("TRUSTED_SERVER__RELEASE_BUILD", "1")?
        .environment("TRUSTED_SERVER__GIT_VERSION", self.source.commit().as_str())?
        .environment("TRUSTED_SERVER__RELEASE_JS_TESTS", "1")
        .and_then(|command| {
            command.environment(
                "TRUSTED_SERVER__RELEASE_JS_SOURCE_CONTEXT",
                boundary.execution_path(&side.metadata.path().join("source-context.json"))?,
            )
        })
        .map(|command| command.cancellation(self.attempt.cancellation.clone()))
    }

    pub(super) fn run_command(
        &self,
        index: usize,
        boundary: &Boundary,
        proof: &DenialProof,
        arguments: Vec<OsString>,
    ) -> Result<Vec<u8>, Report<ReleaseError>> {
        let command = self.command(index, boundary)?.arguments(arguments);
        let output = boundary.run_confined(&command, proof)?.require_success()?;
        self.sides[index].verify_source()?;
        self.check()?;
        Ok(output)
    }

    pub(super) fn install(
        &self,
        index: usize,
        boundary: &Boundary,
        proof: &DenialProof,
    ) -> Result<ObservedInstall, Report<ReleaseError>> {
        if !boundary.matches_roots(&self.boundary_roots(index)?)
            || !boundary.has_owned_descendant_cleanup()
        {
            return Err(preparation_error(
                "JS installation requires exact side roots and owned descendant cleanup",
            ));
        }
        let side = &self.sides[index];
        let npm = boundary.execution_path(&self.tools.npm())?;
        let cache = boundary.execution_path(&side.state.path().join("cache"))?;
        let versions =
            self.run_command(index, boundary, proof, vec![OsString::from("--version")])?;
        if versions != b"v24.20.0\n" {
            return Err(preparation_error(
                "selected Node version differs from approved pin",
            ));
        }
        let version = self.run_command(
            index,
            boundary,
            proof,
            vec![npm.as_os_str().to_owned(), OsString::from("--version")],
        )?;
        if version != b"11.19.0\n" {
            return Err(preparation_error(
                "selected npm version differs from approved pin",
            ));
        }
        for archives in self.originals.archive_paths().chunks(32) {
            let mut arguments = vec![
                npm.as_os_str().to_owned(),
                OsString::from("cache"),
                OsString::from("add"),
            ];
            for archive in archives {
                arguments.push(boundary.execution_path(archive)?.into_os_string());
            }
            arguments.extend(
                [
                    "--offline",
                    "--ignore-scripts",
                    "--no-audit",
                    "--no-fund",
                    "--progress=false",
                    "--userconfig=/dev/null",
                ]
                .map(OsString::from),
            );
            arguments.push(OsString::from(format!(
                "--globalconfig={}/release-disabled-global.npmrc",
                cache.display()
            )));
            arguments.extend([OsString::from("--cache"), cache.as_os_str().to_owned()]);
            self.run_command(index, boundary, proof, arguments)?;
        }
        let key = random_bytes::<32>().change_context(ReleaseError::Material)?;
        let nonce = random_bytes::<32>().change_context(ReleaseError::Material)?;
        let guard = boundary.execution_path(&self.tools.guard())?;
        let request = json!({"schema_version":1,"key":hex::encode(key),"nonce":hex::encode(nonce),"source_commit":self.source.commit().as_str(),"repo_root":boundary.execution_path(side.source.root())?,"cache":cache,"guard_path":guard,"native_esbuild":self.originals.native_esbuild(std::env::consts::ARCH)?});
        let command = self
            .command(index, boundary)?
            .arguments([
                boundary
                    .execution_path(&side.source.root().join("scripts/release/observe-npm.mjs"))?
                    .into_os_string(),
                OsString::from("--guard"),
                guard.into_os_string(),
            ])
            .stdin_bytes(serde_json::to_vec(&request).map_err(preparation_error)?);
        let output = boundary.run_confined(&command, proof)?.require_success()?;
        let observation = authenticate_observation(&output, &key, &request, &self.declarations)?;
        fs::write(
            side.metadata.path().join("preparation.json"),
            serde_json::to_vec(&observation.preparation).map_err(preparation_error)?,
        )
        .map_err(preparation_error)?;
        side.verify_source()?;
        self.check()?;
        Ok(observation)
    }

    pub(super) fn collector_arguments(
        &self,
        index: usize,
        boundary: &Boundary,
    ) -> Result<Vec<OsString>, Report<ReleaseError>> {
        let side = &self.sides[index];
        Ok(vec![
            boundary
                .execution_path(&side.source.root().join("scripts/release/collect-js.mjs"))?
                .into_os_string(),
            OsString::from("--out-dir"),
            boundary
                .execution_path(&side.output().join("bundles"))?
                .into_os_string(),
            OsString::from("--inventory"),
            boundary
                .execution_path(&side.output().join("tsjs-inventory.json"))?
                .into_os_string(),
            OsString::from("--preparation"),
            boundary
                .execution_path(&side.metadata.path().join("preparation.json"))?
                .into_os_string(),
            OsString::from("--source-context"),
            boundary
                .execution_path(&side.metadata.path().join("source-context.json"))?
                .into_os_string(),
        ])
    }
}

fn authenticate_observation(
    output: &[u8],
    key: &[u8; 32],
    request: &Value,
    declarations: &BTreeMap<(String, String), String>,
) -> Result<ObservedInstall, Report<ReleaseError>> {
    let mut accepted = None;
    for line in output.split(|byte| *byte == b'\n') {
        let Some(encoded) = line.strip_prefix(PREFIX) else {
            continue;
        };
        let Some(space) = encoded.iter().position(|byte| *byte == b' ') else {
            continue;
        };
        let Ok(bytes) = STANDARD.decode(&encoded[..space]) else {
            continue;
        };
        let Ok(signature) = hex::decode(&encoded[space + 1..]) else {
            continue;
        };
        let mut mac = Hmac::<Sha256>::new_from_slice(key).map_err(preparation_error)?;
        mac.update(&bytes);
        if mac.verify_slice(&signature).is_err() {
            continue;
        }
        if accepted.is_some() {
            return Err(preparation_error("multiple authenticated npm observations"));
        }
        accepted = Some((parse_json(&bytes, Role::Metadata)?, bytes));
    }
    let (observation, raw) =
        accepted.ok_or_else(|| preparation_error("no authenticated npm lifecycle observation"))?;
    let object = observation
        .as_object()
        .ok_or_else(|| preparation_error("npm observation is not an object"))?;
    let expected_keys = BTreeSet::from([
        "schema_version",
        "nonce",
        "source_commit",
        "repo_root",
        "cache",
        "guard_path",
        "native_esbuild",
        "normalizations",
        "success",
        "npm_exit_code",
        "events",
        "preparation",
    ]);
    if object.keys().map(String::as_str).collect::<BTreeSet<_>>() != expected_keys
        || observation["success"] != true
        || observation["npm_exit_code"] != 0
    {
        return Err(preparation_error(
            "npm observation did not prove complete successful execution",
        ));
    }
    for field in [
        "schema_version",
        "nonce",
        "source_commit",
        "repo_root",
        "cache",
        "guard_path",
        "native_esbuild",
    ] {
        if observation.get(field) != request.get(field) {
            return Err(preparation_error(
                "npm observation belongs to another private request",
            ));
        }
    }
    let events = observation["events"]
        .as_array()
        .ok_or_else(|| preparation_error("npm observation has no event array"))?;
    let mut states = BTreeMap::<(String, String), u8>::new();
    for event in events {
        let text = |name: &str| {
            event
                .get(name)
                .and_then(Value::as_str)
                .ok_or_else(|| preparation_error("lifecycle event lacks a required string"))
        };
        let identity = (text("lock_path")?.to_owned(), text("event")?.to_owned());
        if declarations.get(&identity).map(String::as_str) != Some(text("script_sha256")?) {
            return Err(preparation_error(
                "observed hook differs from authenticated declaration",
            ));
        }
        let state = states.entry(identity).or_default();
        match (text("stage")?, *state) {
            ("start", 0) => *state = 1,
            ("success", 1) if event["exit_code"] == 0 && event["signal"].is_null() => *state = 2,
            _ => {
                return Err(preparation_error(
                    "hook failed, repeated or lacks successful completion",
                ));
            }
        }
    }
    if states.len() != declarations.len() || states.values().any(|state| *state != 2) {
        return Err(preparation_error(
            "not every reviewed hook executed successfully",
        ));
    }
    if !observation["preparation"].is_object() || !observation["normalizations"].is_array() {
        return Err(preparation_error(
            "npm observation lacks measured preparation",
        ));
    }
    let normalizations = observation["normalizations"]
        .as_array()
        .ok_or_else(|| preparation_error("normalizations must be an array"))?;
    if normalizations.len() > 1 {
        return Err(preparation_error(
            "native tool was normalized more than once",
        ));
    }
    for normalization in normalizations {
        let native = &request["native_esbuild"];
        let lock_path = native["lock_path"]
            .as_str()
            .ok_or_else(|| preparation_error("native expectation lacks locked package identity"))?;
        let expected = json!({
            "kind":"esbuild-hardlink-copy-v1",
            "path":"node_modules/esbuild/bin/esbuild",
            "native_path":format!("{lock_path}/bin/esbuild"),
            "sha256":native["sha256"],"size":native["size"],"mode":native["mode"],
        });
        if *normalization != expected {
            return Err(preparation_error(
                "native normalization differs from independently authenticated original",
            ));
        }
    }
    Ok(ObservedInstall {
        preparation: observation["preparation"].clone(),
        raw,
    })
}

#[cfg(test)]
pub(crate) mod tests;
