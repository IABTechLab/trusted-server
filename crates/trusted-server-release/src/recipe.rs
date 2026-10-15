//! Closed recipe layer for one reproducible Cargo build.
//!
//! The recipe validates and binds every input a Cargo execution needs, derives
//! the exact plan in the build's own path namespace, and parses and copies the
//! result of a successful run. The execute gate stays closed: it checks the
//! boundary and plan, then reports the unqualified selection. The real
//! confined run is wired by a later slice.
//!
//! # Roots
//!
//! The checkout, the retained JS handoff and the prepared vendor materials are
//! mounted read-only. The tools are read-only. Two roots are writable: the
//! fresh `cargo_state` root (Cargo home, `HOME` and `TMPDIR`) and the Cargo
//! target directory. The output directory is not mounted at all: the host
//! copies the executable into it after the run.

use std::path::{Path, PathBuf};
use std::time::Duration;

use error_stack::Report;
use sha2::{Digest as _, Sha256};

use crate::boundary::{Boundary, BoundaryRoots, DenialProof};
use crate::documents::Role as DocumentRole;
use crate::error::ReleaseError;
use crate::inspect::inspect_wasm;
use crate::javascript::JsBuildCompleted;
use crate::materials::PreparedCargoMaterials;
use crate::policy::{FIXED_ARTIFACTS, FixedArtifact, ReleaseTarget};
use crate::preflight::AuditedEnvironment;
use crate::preflight::child::{ChildEnvironment, ChildRequest, child_environment};
use crate::process::ProcessLimits;
use crate::source::{ApprovedSource, CommitId, FreshSource};
use fresh::{
    absent_in_real_parent, disjoint, empty_directory, normal_absolute, prepared_materials,
    real_directory, real_file,
};

mod ambient;
mod artifact;
mod cargo_output;
mod completed;
mod fresh;
mod paths;
mod plan;
mod sources;
mod tools;

pub(crate) use ambient::{AmbientProfile, CheckedAmbient};
pub(crate) use completed::RecipeCompleted;
pub(crate) use paths::ExecutionPaths;
pub(crate) use plan::CargoPlan;
#[allow(
    unused_imports,
    reason = "the qualified selection has no production caller yet"
)]
pub(crate) use tools::{Authority, QualifiedRustTools, SelectedRustTools, ToolSelection};

use artifact::{EXECUTABLE_LIMIT, copy_artifact};
use cargo_output::{ExpectedBinary, locate_host_binary};
use completed::SuccessFacts;
use plan::{PlanInputs, check_child_state, plan_for};

/// The only released feature variant of every asset.
const DEFAULT_VARIANT: &str = "default";

/// Wall limit of one Cargo build.
const BUILD_WALL: Duration = Duration::from_secs(3600);
/// Captured output limit of each Cargo stream.
const BUILD_STREAM_MAX: u64 = 64 * 1024 * 1024;

/// Builds a recipe error carrying `message`.
pub(crate) fn recipe_error(message: impl std::fmt::Display) -> Report<ReleaseError> {
    Report::new(ReleaseError::Recipe).attach(message.to_string())
}

/// One source-owned asset identity, reachable only through the fixed table.
#[derive(Clone, Copy)]
pub(crate) struct AssetId {
    artifact: &'static FixedArtifact,
}

impl std::fmt::Debug for AssetId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("AssetId")
            .field(&self.artifact.name)
            .finish()
    }
}

impl PartialEq for AssetId {
    fn eq(&self, other: &Self) -> bool {
        self.artifact.name == other.artifact.name
    }
}

impl Eq for AssetId {}

impl AssetId {
    /// Resolves an asset name against the compiled fixed table.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Recipe`] for any name not in the table.
    pub(crate) fn parse(name: &str) -> Result<Self, Report<ReleaseError>> {
        FIXED_ARTIFACTS
            .iter()
            .find(|artifact| artifact.name == name)
            .map(|artifact| Self { artifact })
            .ok_or_else(|| recipe_error(format!("unknown asset id {name:?}")))
    }

    /// Returns the fixed table row; JSON can never supply its fields.
    pub(crate) fn artifact(&self) -> &'static FixedArtifact {
        self.artifact
    }
}

/// Which independent build of the pair a recipe belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Role {
    /// The first, reference build.
    A,
    /// The second build under varied ambient conditions.
    B,
}

impl Role {
    /// Parses exactly `A` or `B`.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Recipe`] for any other text.
    pub(crate) fn parse(text: &str) -> Result<Self, Report<ReleaseError>> {
        match text {
            "A" => Ok(Self::A),
            "B" => Ok(Self::B),
            _ => Err(recipe_error(format!(
                "unknown role {text:?}; expected A or B"
            ))),
        }
    }
}

/// Requires the default feature variant.
///
/// # Errors
///
/// Returns [`ReleaseError::Recipe`] for any other variant name.
pub(crate) fn check_variant(variant: &str) -> Result<(), Report<ReleaseError>> {
    if variant != DEFAULT_VARIANT {
        return Err(recipe_error(format!(
            "variant {variant:?} is rejected; only the default feature set is released"
        )));
    }
    Ok(())
}

/// Unvalidated textual request fields, exactly as an operator supplied them.
pub(crate) struct RecipeInput<'a> {
    /// Asset name, resolved only against the compiled fixed table.
    pub(crate) asset: &'a str,
    /// Feature variant; only `default` is accepted.
    pub(crate) variant: &'a str,
    /// `A` or `B`.
    pub(crate) role: &'a str,
    /// Approved 40-digit source commit.
    pub(crate) commit: &'a str,
    /// Retained directory of the final JS bundles.
    pub(crate) js_dir: &'a Path,
    /// Retained JS inventory file beside the bundle directory.
    pub(crate) js_inventory: &'a Path,
    /// Prepared materials root holding exactly the vendor directory.
    pub(crate) materials: &'a Path,
    /// Fresh empty directory for Cargo home, `HOME` and `TMPDIR`.
    pub(crate) cargo_state: &'a Path,
    /// Fresh empty directory receiving the built artifact; never mounted.
    pub(crate) output: &'a Path,
    /// Cargo target directory; must not exist yet.
    pub(crate) target_dir: &'a Path,
}

/// Checks the lexical form, freshness and disjointness of every request path.
fn check_roots(input: &RecipeInput<'_>) -> Result<(), Report<ReleaseError>> {
    let paths = [
        ("JS bundle directory", input.js_dir),
        ("JS inventory", input.js_inventory),
        ("materials root", input.materials),
        ("cargo state root", input.cargo_state),
        ("output root", input.output),
        ("target directory", input.target_dir),
    ];
    for (label, path) in paths {
        normal_absolute(label, path)?;
    }
    real_directory(paths[0].0, input.js_dir)?;
    real_file(paths[1].0, input.js_inventory)?;
    let retained = input
        .js_dir
        .parent()
        .ok_or_else(|| recipe_error("JS bundle directory has no retained parent"))?;
    if input.js_inventory.parent() != Some(retained) {
        return Err(recipe_error(
            "JS inventory must sit beside the JS bundle directory in the retained root",
        ));
    }
    prepared_materials(paths[2].0, input.materials)?;
    empty_directory(paths[3].0, input.cargo_state)?;
    empty_directory(paths[4].0, input.output)?;
    absent_in_real_parent(paths[5].0, input.target_dir)?;
    disjoint(&[
        ("retained JS root", retained),
        paths[2],
        paths[3],
        paths[4],
        paths[5],
    ])?;
    Ok(())
}

/// A validated recipe request with fresh, disjoint roots.
#[derive(Debug)]
pub(crate) struct RecipeRequest {
    asset: AssetId,
    role: Role,
    commit: CommitId,
    js_dir: PathBuf,
    js_inventory: PathBuf,
    materials: PathBuf,
    cargo_state: PathBuf,
    output: PathBuf,
    target_dir: PathBuf,
}

impl RecipeRequest {
    /// Parses and validates one request against the compiled cohort and disk.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Recipe`] for an unknown asset or role, a
    /// non-default variant, a malformed commit, non-normal or aliased paths, a
    /// materials root that is not exactly a prepared vendor directory, a
    /// reused state, output or target root, or overlapping roots.
    pub(crate) fn parse(input: &RecipeInput<'_>) -> Result<Self, Report<ReleaseError>> {
        check_variant(input.variant)?;
        let asset = AssetId::parse(input.asset)?;
        let role = Role::parse(input.role)?;
        let commit = CommitId::parse(input.commit)?;
        check_roots(input)?;
        Ok(Self {
            asset,
            role,
            commit,
            js_dir: input.js_dir.to_owned(),
            js_inventory: input.js_inventory.to_owned(),
            materials: input.materials.to_owned(),
            cargo_state: input.cargo_state.to_owned(),
            output: input.output.to_owned(),
            target_dir: input.target_dir.to_owned(),
        })
    }

    pub(super) fn asset(&self) -> AssetId {
        self.asset
    }

    pub(super) fn js_dir(&self) -> &Path {
        &self.js_dir
    }

    pub(super) fn js_inventory(&self) -> &Path {
        &self.js_inventory
    }

    pub(super) fn target_dir(&self) -> &Path {
        &self.target_dir
    }

    /// Private `HOME` below the writable cargo state root.
    pub(super) fn home(&self) -> PathBuf {
        self.cargo_state.join("home")
    }

    /// Private Cargo home below the writable cargo state root.
    pub(super) fn cargo_home(&self) -> PathBuf {
        self.cargo_state.join("cargo-home")
    }

    /// Private `TMPDIR` below the writable cargo state root.
    pub(super) fn tmpdir(&self) -> PathBuf {
        self.cargo_state.join("tmp")
    }
}

/// Derives the private locations a child build receives from the request.
fn child_request(request: &RecipeRequest) -> ChildRequest {
    ChildRequest {
        target: request.asset.artifact().target,
        home: request.home(),
        cargo_home: request.cargo_home(),
        tool_directories: Vec::new(),
    }
}

/// JS handoff digests an attestation binds.
#[derive(Clone, Debug)]
struct JsFacts {
    inventory_sha256: String,
    settings_sha256: String,
    tools_sha256: String,
}

/// Everything [`PreparedRecipe::bind`] cross-checks before a recipe may run.
struct BindInputs<'a> {
    commit: &'a CommitId,
    child: ChildEnvironment,
    completed: &'a JsBuildCompleted,
    tools: SelectedRustTools,
    materials: PreparedCargoMaterials,
    ambient: CheckedAmbient,
    request: RecipeRequest,
}

/// A recipe whose source, JS handoff, tools, materials, ambient and roots are bound.
pub(crate) struct PreparedRecipe {
    request: RecipeRequest,
    tools: SelectedRustTools,
    tools_digest: String,
    materials: PreparedCargoMaterials,
    ambient: CheckedAmbient,
    child: ChildEnvironment,
    retained_root: PathBuf,
    js: JsFacts,
}

/// What a successful confined run produced, as the run path reports it.
struct RunOutput {
    stdout: Vec<u8>,
    stderr_sha256: String,
    started_at_seconds: u64,
    elapsed: Duration,
}

impl PreparedRecipe {
    /// Binds the locally authenticated source and a fresh private checkout.
    ///
    /// The commit and committer epoch reach the child environment only
    /// through [`child_environment`], which requires the authenticated source.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Recipe`] when the fresh checkout is not the
    /// approved commit or not the ambient checkout root, the prepared
    /// materials were derived from another `Cargo.lock`, or as [`Self::bind`].
    #[allow(
        clippy::too_many_arguments,
        reason = "each argument is a distinct authority"
    )]
    pub(crate) fn new(
        source: &ApprovedSource,
        fresh: &FreshSource,
        audited: &AuditedEnvironment,
        completed: &JsBuildCompleted,
        tools: SelectedRustTools,
        materials: PreparedCargoMaterials,
        ambient: CheckedAmbient,
        request: RecipeRequest,
    ) -> Result<Self, Report<ReleaseError>> {
        if fresh.commit() != source.commit().as_str() {
            return Err(recipe_error("fresh checkout is not the approved commit"));
        }
        if fresh.root() != ambient.checkout_root() {
            return Err(recipe_error(
                "fresh checkout is not the ambient checkout root",
            ));
        }
        let lock = source
            .read_tracked("Cargo.lock", DocumentRole::Metadata)
            .map_err(|error| error.change_context(ReleaseError::Recipe))?;
        if hex::encode(Sha256::digest(&lock)) != materials.cargo_lock_sha256() {
            return Err(recipe_error(
                "prepared materials were derived from another Cargo.lock",
            ));
        }
        let child = child_environment(audited, source, &child_request(&request))?;
        Self::bind(BindInputs {
            commit: source.commit(),
            child,
            completed,
            tools,
            materials,
            ambient,
            request,
        })
    }

    /// Cross-checks every input and creates the fresh target and state directories.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Recipe`] when the request, JS handoff, child
    /// environment or materials name another commit, the JS handoff no longer
    /// rehashes or is not the requested directory, the materials are not the
    /// requested root or changed, the tools were selected for another target,
    /// the ambient role differs, a root is no longer fresh, or the checkout,
    /// tools, retained JS, materials, state and target roots overlap.
    fn bind(inputs: BindInputs<'_>) -> Result<Self, Report<ReleaseError>> {
        check_commits(&inputs)?;
        check_parts(&inputs)?;
        let BindInputs {
            child,
            completed,
            tools,
            materials,
            ambient,
            request,
            ..
        } = inputs;
        let retained_root = resolve_js_handoff(completed, &request)?;
        check_fresh_and_disjoint(&request, &ambient, &tools, &materials, &retained_root)?;
        let tools_digest = tools.digest()?;
        create_private_roots(&request)?;
        Ok(Self {
            js: JsFacts {
                inventory_sha256: completed.inventory_sha256().to_owned(),
                settings_sha256: completed.settings_sha256().to_owned(),
                tools_sha256: completed.tools_sha256().to_owned(),
            },
            request,
            tools,
            tools_digest,
            materials,
            ambient,
            child,
            retained_root,
        })
    }

    /// Derives the exact Cargo invocation in the namespace `paths` describes.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Recipe`] when any path variable, the tool
    /// directory or the vendor directory is outside the roots `paths` knows.
    pub(crate) fn plan(
        &self,
        paths: &dyn ExecutionPaths,
    ) -> Result<CargoPlan, Report<ReleaseError>> {
        plan_for(&PlanInputs {
            request: &self.request,
            tools: &self.tools,
            ambient: &self.ambient,
            child: &self.child,
            materials: &self.materials,
            paths,
        })
    }

    /// Returns the boundary roles this recipe requires.
    ///
    /// The checkout, retained JS and prepared materials are read-only. Cargo
    /// home, `HOME`, `TMPDIR` and the target directory are the only writable
    /// roots, and the output directory is not mounted.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Boundary`] for missing or overlapping roots.
    pub(crate) fn boundary_roots(&self) -> Result<BoundaryRoots, Report<ReleaseError>> {
        BoundaryRoots::new(
            &[
                self.ambient.checkout_root().to_owned(),
                self.retained_root.clone(),
                self.request.materials.clone(),
            ],
            std::slice::from_ref(&self.tools.root().to_owned()),
            &[
                self.request.cargo_state.clone(),
                self.request.target_dir.clone(),
            ],
        )
    }

    /// Requires the tools to be the tree measured at binding.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Recipe`] when anything under the tool root was
    /// edited, linked, added or removed.
    fn check_tools(&self) -> Result<(), Report<ReleaseError>> {
        if self.tools.digest()? != self.tools_digest {
            return Err(recipe_error("Rust tools changed after binding"));
        }
        Ok(())
    }

    /// Requires the prepared materials to be unchanged.
    fn check_materials(&self) -> Result<(), Report<ReleaseError>> {
        self.materials.rehash().map_err(|error| {
            error
                .change_context(ReleaseError::Recipe)
                .attach("prepared materials changed after binding")
        })
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

    /// Checks every gate, then reports the closed selection.
    ///
    /// The confined command is described but never started: the real run is
    /// wired by a later slice, and no tool selection is qualified.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Recipe`] when an input, plan, roots or
    /// descendant cleanup gate fails, then always [`ReleaseError::Unqualified`].
    pub(crate) fn execute(
        self,
        boundary: &Boundary,
        _proof: &DenialProof,
    ) -> Result<(), Report<ReleaseError>> {
        self.check_boundary(boundary)?;
        self.plan(boundary)?.confined_command(ProcessLimits {
            wall: BUILD_WALL,
            stdout_max: BUILD_STREAM_MAX,
            stderr_max: BUILD_STREAM_MAX,
        })?;
        let reason = match self.tools.authority() {
            Authority::Qualified => "the confined Cargo run is not wired yet",
            Authority::Unqualified => {
                "Cargo execution is closed until Rust tools and boundary are qualified"
            }
        };
        Err(Report::new(ReleaseError::Unqualified).attach(reason))
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
    fn complete(
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
                .attach("a build from derived manifests cannot carry qualified authority"));
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
            derived_manifest_digests: self.materials.derived_manifest_digests().clone(),
        })
    }
}

/// Child variable carrying the authenticated source commit.
const SOURCE_COMMIT_VARIABLE: &str = "TRUSTED_SERVER__GIT_VERSION";

/// Requires the request, JS handoff, child environment and materials to name
/// the approved commit.
fn check_commits(inputs: &BindInputs<'_>) -> Result<(), Report<ReleaseError>> {
    let commit = inputs.commit;
    if inputs.request.commit != *commit {
        return Err(recipe_error("request names another source commit"));
    }
    if inputs.completed.source_commit() != commit.as_str() {
        return Err(recipe_error(
            "JS handoff was built from another source commit",
        ));
    }
    let child_commit = inputs
        .child
        .variables()
        .get(SOURCE_COMMIT_VARIABLE)
        .map(String::as_str);
    if child_commit != Some(commit.as_str()) {
        return Err(recipe_error(
            "child environment names another source commit",
        ));
    }
    if inputs.materials.source_commit() != commit.as_str() {
        return Err(recipe_error(
            "prepared materials were derived from another source commit",
        ));
    }
    Ok(())
}

/// Requires the materials, tools, ambient role and child state to match the request.
fn check_parts(inputs: &BindInputs<'_>) -> Result<(), Report<ReleaseError>> {
    let request = &inputs.request;
    if inputs.materials.root().as_os_str() != request.materials.as_os_str() {
        return Err(recipe_error("request does not name the prepared materials"));
    }
    inputs.materials.rehash().map_err(|error| {
        error
            .change_context(ReleaseError::Recipe)
            .attach("prepared materials no longer rehash")
    })?;
    if inputs.tools.target() != request.asset.artifact().target {
        return Err(recipe_error(
            "Rust tools were selected for another target than the asset",
        ));
    }
    if inputs.ambient.role() != request.role {
        return Err(recipe_error("ambient role differs from the request role"));
    }
    check_child_state(request, &inputs.child)
}

/// Rehashes the JS handoff and requires the request to name its directories.
fn resolve_js_handoff(
    completed: &JsBuildCompleted,
    request: &RecipeRequest,
) -> Result<PathBuf, Report<ReleaseError>> {
    let retained = completed.rehash_for_handoff().map_err(|error| {
        error
            .change_context(ReleaseError::Recipe)
            .attach("JS handoff no longer rehashes")
    })?;
    let retained_root = std::fs::canonicalize(retained)
        .map_err(|error| recipe_error(format!("cannot resolve JS handoff: {error}")))?;
    if request.js_dir != retained_root.join("bundles")
        || request.js_inventory != retained_root.join("tsjs-inventory.json")
    {
        return Err(recipe_error("request does not name the JS handoff"));
    }
    Ok(retained_root)
}

/// Creates the fresh target directory and the private cargo state directories.
fn create_private_roots(request: &RecipeRequest) -> Result<(), Report<ReleaseError>> {
    std::fs::create_dir(&request.target_dir)
        .map_err(|error| recipe_error(format!("cannot create fresh target: {error}")))?;
    for state in [request.home(), request.cargo_home(), request.tmpdir()] {
        std::fs::create_dir(&state)
            .map_err(|error| recipe_error(format!("cannot create cargo state: {error}")))?;
    }
    Ok(())
}

/// Re-checks root freshness and disjointness immediately before the target is created.
fn check_fresh_and_disjoint(
    request: &RecipeRequest,
    ambient: &CheckedAmbient,
    tools: &SelectedRustTools,
    materials: &PreparedCargoMaterials,
    retained_root: &Path,
) -> Result<(), Report<ReleaseError>> {
    prepared_materials("materials root", &request.materials)?;
    empty_directory("cargo state root", &request.cargo_state)?;
    empty_directory("output root", &request.output)?;
    absent_in_real_parent("target directory", &request.target_dir)?;
    let checkout = std::fs::canonicalize(ambient.checkout_root())
        .map_err(|error| recipe_error(format!("cannot resolve checkout root: {error}")))?;
    disjoint(&[
        ("checkout root", checkout.as_path()),
        ("Rust tools root", tools.root()),
        ("retained JS root", retained_root),
        ("materials root", materials.root()),
        ("cargo state root", request.cargo_state.as_path()),
        ("output root", request.output.as_path()),
        ("target directory", request.target_dir.as_path()),
    ])
}

#[cfg(test)]
mod tests;
