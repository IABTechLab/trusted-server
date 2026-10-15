//! Closed recipe layer for one reproducible Cargo build.
//!
//! Slice 1 validates and binds every input a later Cargo execution needs, but
//! never starts a process: the execute gate rejects a mismatched boundary and
//! otherwise reports the closed production selection.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use error_stack::Report;

use crate::boundary::{Boundary, BoundaryRoots, ConfinedCommand, DenialProof, Umask};
use crate::error::ReleaseError;
use crate::javascript::JsBuildCompleted;
use crate::policy::{FIXED_ARTIFACTS, FixedArtifact};
use crate::preflight::AuditedEnvironment;
use crate::preflight::child::{ChildEnvironment, ChildRequest, child_environment};
use crate::process::ProcessLimits;
use crate::source::{ApprovedSource, CommitId, FreshSource};
use fresh::{
    absent_in_real_parent, disjoint, empty_directory, normal_absolute, real_directory, real_file,
};

mod ambient;
mod fresh;

pub(crate) use ambient::{AmbientProfile, CheckedAmbient};

/// The only released feature variant of every asset.
const DEFAULT_VARIANT: &str = "default";

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
    /// Fresh empty directory for prepared Cargo materials.
    pub(crate) materials: &'a Path,
    /// Fresh empty directory receiving the built artifact.
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
    empty_directory(paths[2].0, input.materials)?;
    empty_directory(paths[3].0, input.output)?;
    absent_in_real_parent(paths[4].0, input.target_dir)?;
    disjoint(&[("retained JS root", retained), paths[2], paths[3], paths[4]])?;
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
    /// reused root or target, or overlapping roots.
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
            output: input.output.to_owned(),
            target_dir: input.target_dir.to_owned(),
        })
    }
}

/// A Rust toolchain that passed Task 8 qualification.
///
/// None has, so this type is uninhabited and production selection stays closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum QualifiedRustTools {}

/// Fixed `bin/cargo` and `bin/rustc` layout, retained as a measured local candidate.
pub(crate) struct SelectedRustTools {
    root: PathBuf,
}

impl SelectedRustTools {
    /// Accepts only a qualified selection; production construction stays closed.
    pub(crate) fn qualified(_root: &Path, qualification: QualifiedRustTools) -> Self {
        match qualification {}
    }

    /// Selects ordinary CI tools without asserting release qualification.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Recipe`] unless `bin/cargo` and `bin/rustc`
    /// are regular files below the canonical `root`.
    #[cfg(test)]
    pub(crate) fn for_tests(root: &Path) -> Result<Self, Report<ReleaseError>> {
        let root = std::fs::canonicalize(root)
            .map_err(|error| recipe_error(format!("cannot resolve Rust tools: {error}")))?;
        for relative in ["bin/cargo", "bin/rustc"] {
            real_file("Rust tool", &root.join(relative))?;
        }
        Ok(Self { root })
    }

    fn cargo(&self) -> PathBuf {
        self.root.join("bin/cargo")
    }
}

/// Environment variables the recipe adds on top of the allowlisted child environment.
const PREBUILT_DIR_VARIABLE: &str = "TSJS_PREBUILT_DIR";
const PREBUILT_INVENTORY_VARIABLE: &str = "TRUSTED_SERVER__TSJS_PREBUILT_INVENTORY";
const TARGET_DIR_VARIABLE: &str = "CARGO_TARGET_DIR";
/// Child variable carrying the authenticated source commit.
const SOURCE_COMMIT_VARIABLE: &str = "TRUSTED_SERVER__GIT_VERSION";

/// The exact Cargo invocation of one recipe; it cannot be started in slice 1.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct CargoPlan {
    asset: AssetId,
    program: PathBuf,
    arguments: Vec<String>,
    environment: BTreeMap<String, String>,
    working_directory: PathBuf,
    umask: Umask,
}

impl CargoPlan {
    /// Describes the plan as a confined command running under the role's umask.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Boundary`] for a non-normal program or working
    /// directory, or [`ReleaseError::Process`] for a rejected variable.
    pub(crate) fn confined_command(
        &self,
        limits: ProcessLimits,
    ) -> Result<ConfinedCommand, Report<ReleaseError>> {
        let mut command = ConfinedCommand::new(&self.program, &self.working_directory, limits)?
            .arguments(self.arguments.iter().map(OsString::from))
            .umask(self.umask);
        for (name, value) in &self.environment {
            command = command.environment(name, value)?;
        }
        Ok(command)
    }
}

/// Returns the fixed argument vector of the release Cargo build.
fn cargo_arguments(asset: AssetId, jobs: u32) -> Vec<String> {
    let artifact = asset.artifact();
    [
        "build",
        "--package",
        artifact.package,
        "--bin",
        artifact.bin,
        "--target",
        artifact.target.triple(),
        "--release",
        "--frozen",
        "--message-format=json-render-diagnostics",
        "--jobs",
    ]
    .into_iter()
    .map(str::to_owned)
    .chain([jobs.to_string()])
    .collect()
}

/// Derives the private locations a child build receives from the request.
fn child_request(request: &RecipeRequest) -> ChildRequest {
    ChildRequest {
        target: request.asset.artifact().target,
        home: request.materials.join("home"),
        cargo_home: request.materials.clone(),
        tool_directories: Vec::new(),
    }
}

/// Everything [`PreparedRecipe::bind`] cross-checks before a recipe may run.
struct BindInputs<'a> {
    commit: &'a CommitId,
    child: ChildEnvironment,
    completed: &'a JsBuildCompleted,
    tools: SelectedRustTools,
    ambient: CheckedAmbient,
    request: RecipeRequest,
}

/// A recipe whose source, JS handoff, tools, ambient and roots are all bound.
pub(crate) struct PreparedRecipe {
    request: RecipeRequest,
    tools: SelectedRustTools,
    ambient: CheckedAmbient,
    child: ChildEnvironment,
    retained_root: PathBuf,
    plan: CargoPlan,
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
    /// approved commit or not the ambient checkout root, or as [`Self::bind`].
    pub(crate) fn new(
        source: &ApprovedSource,
        fresh: &FreshSource,
        audited: &AuditedEnvironment,
        completed: &JsBuildCompleted,
        tools: SelectedRustTools,
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
        let child = child_environment(audited, source, &child_request(&request))?;
        Self::bind(BindInputs {
            commit: source.commit(),
            child,
            completed,
            tools,
            ambient,
            request,
        })
    }

    /// Cross-checks every input and creates the fresh target directory.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Recipe`] when the request, JS handoff or child
    /// environment names another commit, the JS handoff no longer rehashes or
    /// is not the requested directory, the ambient role differs, a root is no
    /// longer fresh, or the checkout, tools, retained JS, materials, output
    /// and target roots overlap.
    fn bind(inputs: BindInputs<'_>) -> Result<Self, Report<ReleaseError>> {
        let BindInputs {
            commit,
            child,
            completed,
            tools,
            ambient,
            request,
        } = inputs;
        if request.commit != *commit {
            return Err(recipe_error("request names another source commit"));
        }
        if completed.source_commit() != commit.as_str() {
            return Err(recipe_error(
                "JS handoff was built from another source commit",
            ));
        }
        if child
            .variables()
            .get(SOURCE_COMMIT_VARIABLE)
            .map(String::as_str)
            != Some(commit.as_str())
        {
            return Err(recipe_error(
                "child environment names another source commit",
            ));
        }
        if ambient.role() != request.role {
            return Err(recipe_error("ambient role differs from the request role"));
        }
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
        check_fresh_and_disjoint(&request, &ambient, &tools, &retained_root)?;
        let plan = plan_for(&request, &tools, &ambient, &child)?;
        std::fs::create_dir(&request.target_dir)
            .map_err(|error| recipe_error(format!("cannot create fresh target: {error}")))?;
        Ok(Self {
            request,
            tools,
            ambient,
            child,
            retained_root,
            plan,
        })
    }

    /// Returns the exact planned Cargo invocation.
    pub(crate) fn plan(&self) -> &CargoPlan {
        &self.plan
    }

    /// Re-derives the plan from the bound inputs and compares it with the held one.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Recipe`] when the held plan's program,
    /// arguments, environment, working directory or umask differ from the derivation.
    pub(crate) fn check_plan(&self) -> Result<(), Report<ReleaseError>> {
        let expected = plan_for(&self.request, &self.tools, &self.ambient, &self.child)?;
        if self.plan != expected {
            return Err(recipe_error(
                "Cargo plan differs from the plan derived from the bound inputs",
            ));
        }
        Ok(())
    }

    /// Returns the boundary roles this recipe requires.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Boundary`] for missing or overlapping roots.
    pub(crate) fn boundary_roots(&self) -> Result<BoundaryRoots, Report<ReleaseError>> {
        BoundaryRoots::new(
            &[
                self.ambient.checkout_root().to_owned(),
                self.retained_root.clone(),
            ],
            std::slice::from_ref(&self.tools.root),
            &[
                self.request.output.clone(),
                self.request.target_dir.clone(),
                self.request.materials.clone(),
            ],
        )
    }

    /// Requires the exact planned roots and owned descendant cleanup.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Recipe`] when the plan was tampered with, the
    /// boundary roots differ from [`Self::boundary_roots`], or the boundary
    /// does not own descendant cleanup.
    fn check_boundary(&self, boundary: &Boundary) -> Result<(), Report<ReleaseError>> {
        self.check_plan()?;
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

    /// Checks every gate, then reports the closed production selection.
    ///
    /// No process is started in slice 1, even for a matching boundary.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Recipe`] when the plan, roots or descendant
    /// cleanup gate fails, then always [`ReleaseError::Unqualified`] because no
    /// Rust toolchain or boundary profile is qualified.
    pub(crate) fn execute(
        self,
        boundary: &Boundary,
        _proof: &DenialProof,
    ) -> Result<(), Report<ReleaseError>> {
        self.check_boundary(boundary)?;
        Err(Report::new(ReleaseError::Unqualified)
            .attach("Cargo execution is closed until Rust tools and boundary are qualified"))
    }
}

/// Re-checks root freshness and disjointness immediately before the target is created.
fn check_fresh_and_disjoint(
    request: &RecipeRequest,
    ambient: &CheckedAmbient,
    tools: &SelectedRustTools,
    retained_root: &Path,
) -> Result<(), Report<ReleaseError>> {
    empty_directory("materials root", &request.materials)?;
    empty_directory("output root", &request.output)?;
    absent_in_real_parent("target directory", &request.target_dir)?;
    let checkout = std::fs::canonicalize(ambient.checkout_root())
        .map_err(|error| recipe_error(format!("cannot resolve checkout root: {error}")))?;
    disjoint(&[
        ("checkout root", checkout.as_path()),
        ("Rust tools root", tools.root.as_path()),
        ("retained JS root", retained_root),
        ("materials root", request.materials.as_path()),
        ("output root", request.output.as_path()),
        ("target directory", request.target_dir.as_path()),
    ])
}

/// Derives the exact Cargo invocation from the allowlisted child environment.
fn plan_for(
    request: &RecipeRequest,
    tools: &SelectedRustTools,
    ambient: &CheckedAmbient,
    child: &ChildEnvironment,
) -> Result<CargoPlan, Report<ReleaseError>> {
    let mut environment = child.variables().clone();
    for (name, path) in [
        (TARGET_DIR_VARIABLE, &request.target_dir),
        (PREBUILT_DIR_VARIABLE, &request.js_dir),
        (PREBUILT_INVENTORY_VARIABLE, &request.js_inventory),
    ] {
        let text = path
            .to_str()
            .ok_or_else(|| recipe_error(format!("{name} is not UTF-8")))?;
        environment.insert(name.to_owned(), text.to_owned());
    }
    Ok(CargoPlan {
        asset: request.asset,
        program: tools.cargo(),
        arguments: cargo_arguments(request.asset, ambient.jobs()),
        environment,
        working_directory: ambient.checkout_root().to_owned(),
        umask: ambient.umask()?,
    })
}

#[cfg(test)]
mod tests;
