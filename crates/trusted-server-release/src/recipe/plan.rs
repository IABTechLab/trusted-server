//! The exact Cargo invocation of one recipe, in the build's own namespace.
//!
//! Host paths exist only in `program` and `working_directory`, which
//! [`ConfinedCommand`] translates itself. Every path inside the argument
//! vector and the environment goes through [`ExecutionPaths`], so a root the
//! boundary does not mount is an error here instead of a dangling path in the
//! child.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;

use error_stack::Report;
use serde_json::json;
use sha2::{Digest as _, Sha256};

use super::ambient::CheckedAmbient;
use super::paths::{ExecutionPaths, execution_text};
use super::remap::{RemapSources, c_prefix_map_environment, remap_arguments};
use super::sources::source_replacement_arguments;
use super::tools::SelectedRustTools;
use super::{AssetId, RecipeRequest, recipe_error};
use crate::boundary::{ConfinedCommand, Umask};
use crate::documents::{Role, canonical_json};
use crate::error::ReleaseError;
use crate::materials::PreparedCargoMaterials;
use crate::preflight::child::ChildEnvironment;
use crate::process::ProcessLimits;

const PREBUILT_DIR_VARIABLE: &str = "TSJS_PREBUILT_DIR";
const PREBUILT_INVENTORY_VARIABLE: &str = "TRUSTED_SERVER__TSJS_PREBUILT_INVENTORY";
const TARGET_DIR_VARIABLE: &str = "CARGO_TARGET_DIR";
const CARGO_HOME_VARIABLE: &str = "CARGO_HOME";
const HOME_VARIABLE: &str = "HOME";
const TMPDIR_VARIABLE: &str = "TMPDIR";
const RUSTC_VARIABLE: &str = "RUSTC";
const PATH_VARIABLE: &str = "PATH";

/// The exact Cargo invocation of one recipe.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct CargoPlan {
    pub(super) asset: AssetId,
    pub(super) program: PathBuf,
    pub(super) arguments: Vec<String>,
    pub(super) environment: BTreeMap<String, String>,
    pub(super) working_directory: PathBuf,
    pub(super) umask: Umask,
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
            // The compiler path is the one tool selector the plan sets, on purpose.
            command = if name == RUSTC_VARIABLE {
                command.tool_environment(name, value)?
            } else {
                command.environment(name, value)?
            };
        }
        Ok(command)
    }

    /// Returns the SHA-256 of the canonical description of the whole plan.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Recipe`] when a path is not UTF-8.
    pub(crate) fn digest(&self) -> Result<String, Report<ReleaseError>> {
        let text = |label: &str, path: &std::path::Path| {
            path.to_str()
                .map(str::to_owned)
                .ok_or_else(|| recipe_error(format!("plan {label} is not UTF-8")))
        };
        let description = json!({
            "asset": self.asset.artifact().name,
            "program": text("program", &self.program)?,
            "arguments": self.arguments,
            "environment": self.environment,
            "working_directory": text("working directory", &self.working_directory)?,
            "umask": self.umask.to_string(),
        });
        let bytes = canonical_json(&description, Role::Metadata)
            .map_err(|error| error.change_context(ReleaseError::Recipe))?;
        Ok(hex::encode(Sha256::digest(bytes)))
    }
}

/// Returns the fixed argument vector of the release Cargo build.
///
/// The only `--config` entries are `configs`: the exact source replacements
/// followed by the target path remap.
pub(super) fn cargo_arguments(asset: AssetId, jobs: u32, configs: &[String]) -> Vec<String> {
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
    ]
    .into_iter()
    .map(str::to_owned)
    .chain(configs.iter().cloned())
    .chain(
        ["--message-format=json-render-diagnostics", "--jobs"]
            .into_iter()
            .map(str::to_owned),
    )
    .chain([jobs.to_string()])
    .collect()
}

/// Requires the child's `HOME` and `CARGO_HOME` to be the request's private state.
///
/// # Errors
///
/// Returns [`ReleaseError::Recipe`] when either variable is missing or names
/// another location.
pub(super) fn check_child_state(
    request: &RecipeRequest,
    child: &ChildEnvironment,
) -> Result<(), Report<ReleaseError>> {
    for (name, host) in [
        (HOME_VARIABLE, request.home()),
        (CARGO_HOME_VARIABLE, request.cargo_home()),
    ] {
        if child.variables().get(name).map(String::as_str) != host.to_str() {
            return Err(recipe_error(format!(
                "child {name} is not the request's private cargo state"
            )));
        }
    }
    Ok(())
}

/// Everything [`plan_for`] derives the plan from.
pub(super) struct PlanInputs<'a> {
    pub(super) request: &'a RecipeRequest,
    pub(super) tools: &'a SelectedRustTools,
    pub(super) ambient: &'a CheckedAmbient,
    pub(super) child: &'a ChildEnvironment,
    pub(super) materials: &'a PreparedCargoMaterials,
    pub(super) paths: &'a dyn ExecutionPaths,
}

/// Derives the exact Cargo invocation from the bound inputs.
///
/// The arguments carry the source replacement and the deterministic target
/// path remap of [`super::remap`]; a native asset's environment also carries
/// the same remap for C code. All of it is bound by [`CargoPlan::digest`].
///
/// # Errors
///
/// Returns [`ReleaseError::Recipe`] when the child environment's `HOME` or
/// `CARGO_HOME` is not the request's private state, or any path variable,
/// the tool directory, the vendor directory, the checkout root or the Rust
/// tool root lies outside the boundary roots or is not plain text, two
/// remapped roots overlap in the execution namespace, or a native asset's
/// remapped root holds whitespace.
pub(super) fn plan_for(inputs: &PlanInputs<'_>) -> Result<CargoPlan, Report<ReleaseError>> {
    let PlanInputs {
        request,
        tools,
        ambient,
        child,
        materials,
        paths,
    } = inputs;
    check_child_state(request, child)?;
    let cargo_home = request.cargo_home();
    let home = request.home();
    let tmpdir = request.tmpdir();
    let rustc = tools.rustc();
    let mut environment = child.variables().clone();
    for (name, host) in [
        (TARGET_DIR_VARIABLE, request.target_dir()),
        (CARGO_HOME_VARIABLE, cargo_home.as_path()),
        (HOME_VARIABLE, home.as_path()),
        (TMPDIR_VARIABLE, tmpdir.as_path()),
        (PREBUILT_DIR_VARIABLE, request.js_dir()),
        (PREBUILT_INVENTORY_VARIABLE, request.js_inventory()),
        (RUSTC_VARIABLE, rustc.as_path()),
    ] {
        environment.insert(name.to_owned(), execution_text(*paths, name, host)?);
    }
    let tool_bin = execution_text(*paths, PATH_VARIABLE, &tools.bin_dir())?;
    environment.insert(
        PATH_VARIABLE.to_owned(),
        format!("{tool_bin}:/usr/bin:/bin"),
    );
    let vendor = execution_text(*paths, "vendor directory", materials.vendor_dir())?;
    let mut configs = source_replacement_arguments(&vendor, materials.git_sources());
    let target = request.asset().artifact().target;
    let checkout = execution_text(*paths, "checkout root", ambient.checkout_root())?;
    let tool_root = execution_text(*paths, "Rust tools root", tools.root())?;
    let sources = RemapSources {
        checkout: &checkout,
        vendor: &vendor,
        cargo_home: &environment[CARGO_HOME_VARIABLE],
        tools: &tool_root,
        target: &environment[TARGET_DIR_VARIABLE],
    };
    configs.extend(remap_arguments(target.triple(), &sources)?);
    let c_prefix_map = c_prefix_map_environment(target, &sources)?;
    if let Some((name, value)) = c_prefix_map {
        environment.insert(name, value);
    }
    Ok(CargoPlan {
        asset: request.asset(),
        program: tools.cargo(),
        arguments: cargo_arguments(request.asset(), ambient.jobs(), &configs),
        environment,
        working_directory: ambient.checkout_root().to_owned(),
        umask: ambient.umask()?,
    })
}
