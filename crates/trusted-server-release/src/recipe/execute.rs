//! The confined run of a bound recipe and the checks around it.
//!
//! Every input is rehashed immediately before and after the confined Cargo
//! process, Cargo home must gain no registry or Git cache, and only a
//! successful run is parsed, copied and bound into a [`RecipeCompleted`].

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use error_stack::Report;
use sha2::{Digest as _, Sha256};

use super::artifact::{EXECUTABLE_LIMIT, copy_artifact};
use super::cargo_output::{ExpectedBinary, locate_host_binary};
use super::completed::SuccessFacts;
use super::{Authority, ExecutionPaths, PreparedRecipe, RecipeCompleted, recipe_error};
use crate::boundary::{Boundary, DenialProof};
use crate::error::ReleaseError;
use crate::inspect::inspect_wasm;
use crate::policy::ReleaseTarget;
use crate::process::ProcessLimits;
use crate::tool_tree::tool_tree_digest;

/// Wall limit of one Cargo build.
const BUILD_WALL: Duration = Duration::from_secs(25 * 60);
/// Captured output limit of each Cargo stream.
const BUILD_STREAM_MAX: u64 = 16 * 1024 * 1024;

/// Limits of the confined Cargo process.
fn build_limits() -> ProcessLimits {
    ProcessLimits {
        wall: BUILD_WALL,
        stdout_max: BUILD_STREAM_MAX,
        stderr_max: BUILD_STREAM_MAX,
    }
}

/// What a successful confined run produced, as the run path reports it.
pub(super) struct RunOutput {
    pub(super) stdout: Vec<u8>,
    pub(super) stderr_sha256: String,
    pub(super) started_at_seconds: u64,
    pub(super) elapsed: Duration,
}

impl PreparedRecipe {
    /// Requires the tools to be the tree measured at binding.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Recipe`] when anything under the tool root was
    /// edited, linked, added or removed.
    pub(super) fn check_tools(&self) -> Result<(), Report<ReleaseError>> {
        if self.tools.digest()? != self.tools_digest {
            return Err(recipe_error("Rust tools changed after binding"));
        }
        Ok(())
    }

    /// Requires the prepared materials to be unchanged.
    pub(super) fn check_materials(&self) -> Result<(), Report<ReleaseError>> {
        self.materials.rehash().map_err(|error| {
            error
                .change_context(ReleaseError::Recipe)
                .attach("prepared materials changed after binding")
        })
    }

    /// Requires the retained JS handoff to be the tree measured at binding.
    fn check_js_handoff(&self) -> Result<(), Report<ReleaseError>> {
        if retained_tree_digest(&self.retained_root)? != self.retained_digest {
            return Err(recipe_error("retained JS handoff changed after binding"));
        }
        Ok(())
    }

    /// Rehashes the JS handoff, the tools and the materials.
    pub(super) fn check_inputs(&self) -> Result<(), Report<ReleaseError>> {
        self.check_js_handoff()?;
        self.check_tools()?;
        self.check_materials()
    }

    /// Requires Cargo's private home to hold no registry or Git cache.
    ///
    /// Every dependency comes from the vendored directory source, so a cache
    /// appearing in Cargo home means Cargo resolved something elsewhere.
    pub(super) fn check_no_fetched_sources(&self) -> Result<(), Report<ReleaseError>> {
        let cargo_home = self.request.cargo_home();
        for name in ["registry", "git"] {
            match std::fs::symlink_metadata(cargo_home.join(name)) {
                Ok(_) => {
                    return Err(recipe_error(format!(
                        "Cargo home holds a {name} cache; the build bypassed the vendored sources"
                    )));
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(recipe_error(format!(
                        "Cargo home {name} cache cannot be inspected: {error}"
                    )));
                }
            }
        }
        Ok(())
    }

    /// Requires unchanged inputs, a derivable plan, the exact planned roots
    /// and owned descendant cleanup.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Recipe`] when the tools or materials changed,
    /// the plan cannot be derived in the boundary's namespace, the boundary
    /// roots differ from [`Self::boundary_roots`], or the boundary does not
    /// own descendant cleanup.
    fn check_boundary(&self, boundary: &Boundary) -> Result<(), Report<ReleaseError>> {
        self.check_tools()?;
        self.check_materials()?;
        self.plan(boundary)?;
        if !boundary.matches_roots(&self.boundary_roots()?) {
            return Err(recipe_error(
                "boundary roots differ from the exact recipe roots",
            ));
        }
        if !boundary.has_owned_descendant_cleanup() {
            return Err(recipe_error(
                "boundary does not own descendant cleanup for Cargo build scripts",
            ));
        }
        Ok(())
    }

    /// Checks every gate, runs Cargo confined and completes the run.
    ///
    /// Only a selection that [permits a candidate run](super::SelectedRustTools::permits_candidate_run)
    /// reaches the run: the test-only ordinary CI selection. That run is
    /// unqualified CI evidence; production builds can name no selection, so
    /// their gate stays closed. The JS handoff, tools and materials are
    /// rehashed immediately before and after the run, and Cargo home must
    /// gain no registry or Git cache.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Recipe`] when an input, plan, roots or
    /// descendant cleanup gate fails, an input changed around the run, Cargo
    /// fetched into its home, the build did not succeed or its completion is
    /// rejected; [`ReleaseError::Unqualified`] for a selection that permits no
    /// candidate run; or [`ReleaseError::Boundary`] / [`ReleaseError::Process`]
    /// when the confined run itself fails.
    pub(crate) fn execute(
        self,
        boundary: &Boundary,
        proof: &DenialProof,
    ) -> Result<RecipeCompleted, Report<ReleaseError>> {
        self.check_boundary(boundary)?;
        let command = self.plan(boundary)?.confined_command(build_limits())?;
        if !self.tools.permits_candidate_run() {
            return Err(Report::new(ReleaseError::Unqualified)
                .attach("Cargo execution is closed until Rust tools and boundary are qualified"));
        }
        self.check_inputs()?;
        self.check_no_fetched_sources()?;
        let started_at_seconds = unix_seconds()?;
        let run = boundary.run_confined(&command, proof);
        let after = self
            .check_inputs()
            .and_then(|()| self.check_no_fetched_sources());
        let result = match (run, after) {
            (Ok(result), Ok(())) => result,
            (Ok(_), Err(changed)) => return Err(changed),
            (Err(run), Ok(())) => return Err(run),
            (Err(run), Err(changed)) => {
                return Err(run.attach(format!("inputs also failed their checks: {changed:?}")));
            }
        };
        let stderr_sha256 = hex::encode(Sha256::digest(result.stderr()));
        let elapsed = result.elapsed();
        let stdout = result.require_success().map_err(|error| {
            error
                .change_context(ReleaseError::Recipe)
                .attach("confined Cargo build did not succeed")
        })?;
        let output = RunOutput {
            stdout,
            stderr_sha256,
            started_at_seconds,
            elapsed,
        };
        self.complete(output, boundary.execution_fingerprint(), boundary)
    }

    /// Turns one successful run into a [`RecipeCompleted`].
    ///
    /// Parses Cargo's stream, maps the executable back through a writable
    /// root, copies it into the output directory and, for Wasm, requires the
    /// debug sections a stripped module lacks.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Recipe`] when the tools or materials changed,
    /// the stream is not exactly one fresh debug-info binary, the executable
    /// cannot be copied, or a Wasm artifact is stripped, or
    /// [`ReleaseError::Inspect`] when the Wasm module is rejected.
    pub(super) fn complete(
        self,
        run: RunOutput,
        boundary_fingerprint: [u8; 32],
        paths: &dyn ExecutionPaths,
    ) -> Result<RecipeCompleted, Report<ReleaseError>> {
        self.check_tools()?;
        self.check_materials()?;
        let plan_digest = self.plan(paths)?.digest()?;
        let artifact = self.request.asset.artifact();
        let manifest = paths.execution_path(
            &self
                .ambient
                .checkout_root()
                .join("crates")
                .join(artifact.package)
                .join("Cargo.toml"),
        )?;
        let release_dir = paths
            .execution_path(&self.request.target_dir)?
            .join(artifact.target.triple())
            .join("release");
        let located = locate_host_binary(
            &run.stdout,
            &ExpectedBinary {
                bin: artifact.bin,
                manifest: &manifest,
                release_dir: &release_dir,
            },
            paths,
        )?;
        let copied = copy_artifact(
            &located.host,
            &self.request.target_dir,
            &self.request.output,
            artifact.name,
            EXECUTABLE_LIMIT,
        )?;
        if artifact.target == ReleaseTarget::Wasm32Wasip1 {
            let bytes = std::fs::read(copied.path()).map_err(|error| {
                recipe_error(format!("cannot read the copied artifact: {error}"))
            })?;
            if hex::encode(Sha256::digest(&bytes)) != copied.sha256() {
                return Err(recipe_error("copied artifact changed before inspection"));
            }
            inspect_wasm(&bytes)?.require_debug_sections()?;
        }
        if self.tools.authority() == Authority::Qualified
            && !self.materials.derived_manifest_digests().is_empty()
        {
            return Err(Report::new(ReleaseError::Unqualified)
                .attach("a build from caller-derived manifests cannot carry qualified authority"));
        }
        RecipeCompleted::record_success(SuccessFacts {
            asset: self.request.asset,
            role: self.request.role,
            commit: self.request.commit.clone(),
            cargo_lock_sha256: self.materials.cargo_lock_sha256().to_owned(),
            materials_digest: self.materials.digest().to_owned(),
            js_inventory_sha256: self.js.inventory_sha256.clone(),
            js_settings_sha256: self.js.settings_sha256.clone(),
            js_tools_sha256: self.js.tools_sha256.clone(),
            rust_tools_digest: self.tools_digest.clone(),
            boundary_fingerprint,
            plan_digest,
            ambient: self.ambient.clone(),
            started_at_seconds: run.started_at_seconds,
            elapsed: run.elapsed,
            stdout: run.stdout,
            stderr_sha256: run.stderr_sha256,
            artifact: copied,
            output_dir: self.request.output.clone(),
            normalized_manifest_digests: self.materials.normalized_manifest_digests().clone(),
            derived_manifest_digests: self.materials.derived_manifest_digests().clone(),
        })
    }
}

/// Digest of the whole retained JS root: contents, modes, names and aliases.
pub(super) fn retained_tree_digest(root: &Path) -> Result<String, Report<ReleaseError>> {
    tool_tree_digest(root, &|| Ok(())).map_err(|error| {
        error
            .change_context(ReleaseError::Recipe)
            .attach("retained JS handoff cannot be digested")
    })
}

/// Current Unix time in whole seconds.
fn unix_seconds() -> Result<u64, Report<ReleaseError>> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .map_err(|error| recipe_error(format!("system clock is before the Unix epoch: {error}")))
}
