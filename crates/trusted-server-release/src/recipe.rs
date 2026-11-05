//! Closed recipe layer for one reproducible Cargo build.
//!
//! The recipe validates and binds every input a Cargo execution needs, derives
//! the exact plan in the build's own path namespace, runs it confined, and
//! parses and copies the result of a successful run. Only the test-only
//! ordinary CI tool selection reaches the run, so every completed build is
//! unqualified candidate evidence; production execution stays closed until a
//! toolchain, boundary and image are qualified.
//!
//! # Roots
//!
//! The checkout, the retained JS handoff and the prepared vendor materials are
//! mounted read-only. The tools are read-only. Two roots are writable: the
//! fresh `cargo_state` root (Cargo home, `HOME` and `TMPDIR`) and the Cargo
//! target directory. The output directory is not mounted at all: the host
//! copies the executable into it after the run.

use std::path::{Path, PathBuf};

use error_stack::Report;
use sha2::{Digest as _, Sha256};

use crate::boundary::BoundaryRoots;
use crate::composition::origin::WorkspaceOrigins;
use crate::documents::Role as DocumentRole;
use crate::error::ReleaseError;
use crate::javascript::JsBuildCompleted;
use crate::javascript::RetainedJsModule;
use crate::materials::PreparedCargoMaterials;
#[cfg(test)]
use crate::policy::ReleaseTarget;
use crate::policy::{FIXED_ARTIFACTS, FixedArtifact};
use crate::preflight::AuditedEnvironment;
use crate::preflight::child::{ChildEnvironment, ChildRequest, child_environment};
use crate::source::{ApprovedSource, CommitId, FreshSource};
use fresh::{
    absent_in_real_parent, disjoint, empty_directory, normal_absolute, prepared_materials,
    real_directory, real_file,
};

mod ambient;
mod apple;
mod artifact;
mod cargo_home;
mod cargo_output;
mod compare;
mod completed;
mod execute;
pub(crate) mod fastly_tools;
mod final_paths;
mod fresh;
pub(crate) mod gnu;
mod identity;
mod native;
mod paths;
mod plan;
mod remap;

pub(crate) use remap::{
    REMAPPED_CHECKOUT, REMAPPED_TARGET, check_cargo_config, check_no_extensionless_config,
    reject_execution_prefixes, rustflags_arguments,
};
mod runtime;
mod sources;
mod startup;
mod tools;

pub(crate) use ambient::{AmbientProfile, CheckedAmbient};
#[allow(
    unused_imports,
    reason = "the qualified Apple selection has no production caller yet"
)]
pub(crate) use apple::{
    AppleDeveloperTools, AppleToolEvidence, AppleToolFacts, QualifiedAppleTools,
};
pub(crate) use cargo_output::successful_messages as observed_cargo_messages;
pub(crate) use completed::RecipeCompleted;
pub(crate) use identity::{ExactInvocationDigest, RecipeDefinitionIdentity};
pub(crate) use paths::ExecutionPaths;
pub(crate) use plan::CargoPlan;
#[allow(
    unused_imports,
    reason = "the qualified selection has no production caller yet"
)]
pub(crate) use tools::{
    Authority, QualifiedRustTools, RustToolEvidence, SelectedRustTools, ToolSelection,
};

#[cfg(test)]
use execute::RunOutput;
use execute::retained_tree_digest;
use plan::{PlanInputs, check_child_state, plan_for};

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

    /// Resolves a Cargo denial fixture member by package name, for tests only.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Recipe`] for any package not in the test rows.
    #[cfg(test)]
    pub(crate) fn denial_fixture(package: &str) -> Result<Self, Report<ReleaseError>> {
        crate::policy::DENIAL_FIXTURE_ARTIFACTS
            .iter()
            .find(|artifact| artifact.package == package)
            .map(|artifact| Self { artifact })
            .ok_or_else(|| recipe_error(format!("unknown denial fixture member {package:?}")))
    }

    /// Resolves a Cargo denial fixture member built for a Darwin `target`,
    /// for tests only.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Recipe`] for any package or target not in the
    /// Darwin test rows.
    #[cfg(test)]
    pub(crate) fn darwin_denial_fixture(
        package: &str,
        target: ReleaseTarget,
    ) -> Result<Self, Report<ReleaseError>> {
        crate::policy::DARWIN_DENIAL_FIXTURE_ARTIFACTS
            .iter()
            .find(|artifact| artifact.package == package && artifact.target == target)
            .map(|artifact| Self { artifact })
            .ok_or_else(|| {
                recipe_error(format!(
                    "unknown Darwin denial fixture member {package:?} for {}",
                    target.triple()
                ))
            })
    }
}

/// Which independent build of the pair a recipe belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Role {
    /// The canonical build whose output is proposed for publication.
    Canonical,
    /// The independent reproduction under varied ambient conditions, which
    /// must match the canonical build.
    Reproduction,
}

impl Role {
    /// Parses exactly `A` or `B`, the external spelling of the two roles.
    ///
    /// The spec (R-REPEAT, "Build A" and "Build B") and the closed
    /// `build.sh --role A|B` interface of the implementation plan (Task 9)
    /// name the roles `A` and `B`, as do the tracked `ambient_profiles` keys
    /// of `release-tools.json`. This and [`Self::spec_label`] are the only
    /// places that spelling maps to [`Self::Canonical`] and
    /// [`Self::Reproduction`].
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Recipe`] for any other text.
    pub(crate) fn parse(text: &str) -> Result<Self, Report<ReleaseError>> {
        match text {
            "A" => Ok(Self::Canonical),
            "B" => Ok(Self::Reproduction),
            _ => Err(recipe_error(format!(
                "unknown role {text:?}; expected A or B"
            ))),
        }
    }

    /// Returns the external spelling [`Self::parse`] accepts for this role.
    pub(crate) fn spec_label(self) -> &'static str {
        match self {
            Self::Canonical => "A",
            Self::Reproduction => "B",
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
    /// `A` (canonical) or `B` (reproduction); see [`Role::parse`].
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
        Self::with_asset(asset, input)
    }

    /// Parses a request for an already resolved test-only asset; the asset
    /// text of `input` is ignored.
    ///
    /// # Errors
    ///
    /// As [`Self::parse`], except for the asset name.
    #[cfg(test)]
    pub(crate) fn parse_for_asset(
        asset: AssetId,
        input: &RecipeInput<'_>,
    ) -> Result<Self, Report<ReleaseError>> {
        check_variant(input.variant)?;
        Self::with_asset(asset, input)
    }

    /// Validates every field but the asset, which the caller resolved.
    fn with_asset(asset: AssetId, input: &RecipeInput<'_>) -> Result<Self, Report<ReleaseError>> {
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
#[derive(Debug)]
struct JsFacts {
    inventory_sha256: String,
    settings_sha256: String,
    tools_sha256: String,
    modules: Vec<RetainedJsModule>,
    contributions: Option<crate::javascript::contributions::RetainedContributions>,
}

/// Source authority of one recipe; bypass exists only in explicit test fixtures.
enum WorkspaceSelection {
    Approved(Box<ApprovedWorkspace>),
    #[cfg(test)]
    Fixture,
}

struct ApprovedWorkspace {
    origins: WorkspaceOrigins,
    definition: RecipeDefinitionIdentity,
}

/// Everything [`PreparedRecipe::bind`] cross-checks before a recipe may run.
struct BindInputs<'a> {
    commit: &'a CommitId,
    workspace: WorkspaceSelection,
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
    retained_digest: String,
    js: JsFacts,
    workspace: WorkspaceSelection,
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
    /// materials were derived from another `Cargo.lock`, the tracked Cargo
    /// configuration sets `build.rustflags`, which the path remap would
    /// supersede, a legacy extensionless `.cargo/config` is tracked or on
    /// disk at the checkout root, or as [`Self::bind`].
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
        let config = source
            .read_tracked(remap::CARGO_CONFIG, DocumentRole::Metadata)
            .map_err(|error| error.change_context(ReleaseError::Recipe))?;
        remap::check_cargo_config(&config)?;
        remap::check_no_extensionless_config(
            fresh.root(),
            source.tracked_sha256(remap::LEGACY_CARGO_CONFIG).is_some(),
        )?;
        let child = child_environment(audited, source, &child_request(&request))?;
        let workspace = ApprovedWorkspace {
            origins: WorkspaceOrigins::bind(source, fresh)?,
            definition: RecipeDefinitionIdentity::from_source(source, request.asset)?,
        };
        Self::bind(BindInputs {
            commit: source.commit(),
            workspace: WorkspaceSelection::Approved(Box::new(workspace)),
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
    /// the ambient role differs, a root is no longer fresh, the checkout,
    /// tools, retained JS, materials, state, target and Apple developer roots
    /// overlap, Apple developer tools are missing for a Darwin asset, given
    /// for another, carry another authority or no longer rehash, or a Darwin
    /// tool root and target directory do not share one parent.
    fn bind(inputs: BindInputs<'_>) -> Result<Self, Report<ReleaseError>> {
        check_commits(&inputs)?;
        check_parts(&inputs)?;
        let BindInputs {
            workspace,
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
        check_recipe_owned_parent(&request, &tools)?;
        let tools_digest = tools.digest()?;
        let retained_digest = retained_tree_digest(&retained_root)?;
        let contributions = match &workspace {
            WorkspaceSelection::Approved(_) => Some(completed.retained_contributions()?),
            #[cfg(test)]
            WorkspaceSelection::Fixture => None,
        };
        create_private_roots(&request)?;
        Ok(Self {
            js: JsFacts {
                inventory_sha256: completed.inventory_sha256().to_owned(),
                settings_sha256: completed.settings_sha256().to_owned(),
                tools_sha256: completed.tools_sha256().to_owned(),
                modules: completed.retained_modules()?,
                contributions,
            },
            request,
            tools,
            tools_digest,
            materials,
            ambient,
            child,
            retained_root,
            retained_digest,
            workspace,
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
    /// roots, and the output directory is not mounted. A Darwin build adds
    /// its detached SDK as a scanned immutable root and the Apple developer
    /// directory as its only read-and-execute system root. Explicit lexical
    /// test fixtures have no detached SDK root.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Boundary`] for missing or overlapping roots.
    pub(crate) fn boundary_roots(&self) -> Result<BoundaryRoots, Report<ReleaseError>> {
        let mut immutable = vec![
            self.ambient.checkout_root().to_owned(),
            self.retained_root.clone(),
            self.request.materials.clone(),
        ];
        if let Some(sdk) = self
            .tools
            .apple()
            .and_then(AppleDeveloperTools::owned_sdk_root)
        {
            immutable.push(sdk.to_owned());
        }
        let roots = BoundaryRoots::new(
            &immutable,
            std::slice::from_ref(&self.tools.root().to_owned()),
            &[
                self.request.cargo_state.clone(),
                self.request.target_dir.clone(),
            ],
        )?;
        match self.tools.apple() {
            Some(apple) => roots.with_system(&[apple.developer_dir().to_owned()]),
            None => Ok(roots),
        }
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
    check_apple_tools(request, &inputs.tools)?;
    check_child_state(request, &inputs.child)
}

/// Requires Apple developer tools for exactly the Darwin targets, carrying
/// the Rust tools' authority and still matching their record.
fn check_apple_tools(
    request: &RecipeRequest,
    tools: &SelectedRustTools,
) -> Result<(), Report<ReleaseError>> {
    let darwin = request.asset.artifact().target.is_darwin();
    let apple = match (darwin, tools.apple()) {
        (false, None) => return Ok(()),
        (false, Some(_)) => {
            return Err(recipe_error(
                "only Darwin assets take Apple developer tools",
            ));
        }
        (true, None) => {
            return Err(recipe_error("Darwin assets require Apple developer tools"));
        }
        (true, Some(apple)) => apple,
    };
    if apple.authority() != tools.authority() {
        return Err(recipe_error(
            "Apple developer tools and Rust tools carry different authority",
        ));
    }
    apple.verify()
}

/// Requires a Darwin build's Rust tool root and target directory to be
/// direct children of one recipe-owned parent.
///
/// The linker strips that parent from every debug-map object path
/// (`-oso_prefix`), so the standard library objects and the build's own
/// objects both become relative.
fn check_recipe_owned_parent(
    request: &RecipeRequest,
    tools: &SelectedRustTools,
) -> Result<(), Report<ReleaseError>> {
    if tools.apple().is_none() {
        return Ok(());
    }
    let target_parent = request
        .target_dir
        .parent()
        .ok_or_else(|| recipe_error("Darwin target directory has no parent"))?;
    let target_parent = std::fs::canonicalize(target_parent).map_err(|error| {
        recipe_error(format!(
            "cannot resolve the Darwin target directory's parent: {error}"
        ))
    })?;
    if tools.root().parent() != Some(target_parent.as_path()) {
        return Err(recipe_error(
            "Darwin Rust tool root and target directory must be direct children of one \
             recipe-owned parent",
        ));
    }
    Ok(())
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
    let mut roots = vec![
        ("checkout root", checkout.as_path()),
        ("Rust tools root", tools.root()),
        ("retained JS root", retained_root),
        ("materials root", materials.root()),
        ("cargo state root", request.cargo_state.as_path()),
        ("output root", request.output.as_path()),
        ("target directory", request.target_dir.as_path()),
    ];
    if let Some(apple) = tools.apple() {
        roots.push(("Apple developer directory", apple.developer_dir()));
        if let Some(sdk) = apple.owned_sdk_root() {
            roots.push(("owned Apple SDK", sdk));
        }
    }
    disjoint(&roots)
}

#[cfg(test)]
mod tests;
