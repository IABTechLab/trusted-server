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
use super::apple::{AppleDeveloperTools, REMAPPED_SDK};
use super::paths::{ExecutionPaths, execution_text};
use super::remap::{RemapSources, c_prefix_map_environment_with, remap_arguments_with};
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
const DEVELOPER_DIR_VARIABLE: &str = "DEVELOPER_DIR";
const SDKROOT_VARIABLE: &str = "SDKROOT";
const ZERO_AR_DATE_VARIABLE: &str = "ZERO_AR_DATE";

/// The exact Cargo invocation of one recipe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CargoPlan {
    pub(super) asset: AssetId,
    pub(super) program: PathBuf,
    pub(super) arguments: Vec<String>,
    pub(super) environment: BTreeMap<String, String>,
    pub(super) working_directory: PathBuf,
    pub(super) umask: Umask,
    /// Digest of the Apple developer tool record a Darwin plan links with.
    pub(super) apple_identity_sha256: Option<String>,
}

/// Fixed supporting observations of the selected explicit-target build.
#[derive(Clone, Copy, Debug)]
pub(super) enum CargoQuery {
    /// Unfiltered identities, including host-only build dependencies.
    Metadata,
    /// Selected package's non-dev dependency tree, retained as raw evidence.
    Tree,
}

impl CargoPlan {
    /// Derives supporting queries without changing tools, inputs or the boundary.
    ///
    /// Compiler messages remain the authority for effective unit features.
    /// Metadata/tree features are only package-level graph observations.
    ///
    /// # Errors
    ///
    /// Rejects incomplete config/feature argument pairs in the selected plan.
    pub(super) fn observation_query(&self, kind: CargoQuery) -> Result<Self, Report<ReleaseError>> {
        let artifact = self.asset.artifact();
        let arguments: &[&str] = match kind {
            CargoQuery::Metadata => &["metadata", "--format-version", "1", "--frozen"],
            CargoQuery::Tree => &[
                "tree",
                "--package",
                artifact.package,
                "--target",
                artifact.target.triple(),
                "--edges",
                "normal,build",
                "--prefix",
                "depth",
                "--format",
                "{p}",
                "--charset",
                "ascii",
                "--no-dedupe",
                "--frozen",
            ],
        };
        let mut query = self.clone();
        query.arguments = arguments
            .iter()
            .map(|argument| (*argument).to_owned())
            .collect();
        let mut selected = self.arguments.iter();
        while let Some(argument) = selected.next() {
            match argument.as_str() {
                "--config" | "--features" => {
                    let value = selected.next().ok_or_else(|| {
                        recipe_error("incomplete selected Cargo config or features")
                    })?;
                    query.arguments.extend([argument.clone(), value.clone()]);
                }
                "--all-features" | "--no-default-features" => {
                    query.arguments.push(argument.clone())
                }
                _ => {}
            }
        }
        Ok(query)
    }

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
        let mut description = json!({
            "asset": self.asset.artifact().name,
            "program": text("program", &self.program)?,
            "arguments": self.arguments,
            "environment": self.environment,
            "working_directory": text("working directory", &self.working_directory)?,
            "umask": self.umask.to_string(),
        });
        if let Some(identity) = &self.apple_identity_sha256 {
            description["apple_developer_tools_sha256"] = json!(identity);
        }
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
    let darwin = tools
        .apple()
        .map(|apple| darwin_plan(*paths, target, apple, &tool_root))
        .transpose()?;
    let sources = RemapSources {
        checkout: &checkout,
        vendor: &vendor,
        cargo_home: &environment[CARGO_HOME_VARIABLE],
        tools: &tool_root,
        target: &environment[TARGET_DIR_VARIABLE],
    };
    let link_flags = darwin
        .as_ref()
        .map_or(&[][..], |darwin| darwin.rustflags.as_slice());
    configs.extend(remap_arguments_with(target.triple(), &sources, link_flags)?);
    if target == crate::policy::ReleaseTarget::X86_64UnknownLinuxGnu {
        configs.extend(super::gnu::linker_arguments());
    }
    if target == crate::policy::ReleaseTarget::Wasm32Wasip1
        && cfg!(all(target_os = "linux", target_arch = "x86_64"))
    {
        configs.extend(super::fastly_tools::linker_arguments(&tool_root));
    }
    let developer = darwin.as_ref().map(|darwin| darwin.developer.as_str());
    let c_prefix_map = c_prefix_map_environment_with(target, &sources, developer)?;
    if let Some((name, mut value)) = c_prefix_map {
        if let Some(sdk) = darwin.as_ref().and_then(|darwin| darwin.sdk.as_deref()) {
            super::fresh::disjoint(&[
                ("owned SDK", std::path::Path::new(sdk)),
                ("checkout", std::path::Path::new(&checkout)),
                ("vendor", std::path::Path::new(&vendor)),
                (
                    "Cargo home",
                    std::path::Path::new(&environment[CARGO_HOME_VARIABLE]),
                ),
                ("Rust tools", std::path::Path::new(&tool_root)),
                (
                    "target",
                    std::path::Path::new(&environment[TARGET_DIR_VARIABLE]),
                ),
                (
                    "Apple developer",
                    std::path::Path::new(developer.expect("should have Darwin developer root")),
                ),
            ])?;
            value.push_str(&format!(" -isysroot {sdk} -ffile-prefix-map={sdk}={REMAPPED_SDK} -fdebug-prefix-map={sdk}={REMAPPED_SDK}"));
        }
        environment.insert(name, value);
    }
    if target == crate::policy::ReleaseTarget::X86_64UnknownLinuxGnu {
        environment.extend(super::gnu::environment());
    }
    if target == crate::policy::ReleaseTarget::Wasm32Wasip1
        && cfg!(all(target_os = "linux", target_arch = "x86_64"))
    {
        environment.extend(super::fastly_tools::environment());
    }
    let apple_identity_sha256 = match darwin {
        Some(darwin) => {
            configs.extend(darwin.configs);
            environment.extend(darwin.environment);
            Some(darwin.identity_sha256)
        }
        None => None,
    };
    Ok(CargoPlan {
        asset: request.asset(),
        program: tools.cargo(),
        arguments: cargo_arguments(request.asset(), ambient.jobs(), &configs),
        environment,
        working_directory: ambient.checkout_root().to_owned(),
        umask: ambient.umask()?,
        apple_identity_sha256,
    })
}

/// The Darwin additions to one plan, all in the execution namespace.
struct DarwinPlan {
    /// Linker arguments appended inside the target rustflags entry.
    rustflags: Vec<String>,
    /// Extra `--config` pairs: the target linker.
    configs: Vec<String>,
    /// `DEVELOPER_DIR`, `SDKROOT`, `CC_<triple>`, `AR_<triple>` and
    /// `ZERO_AR_DATE` and the fixed Cargo OpenSSL configuration.
    environment: BTreeMap<String, String>,
    /// Execution path of the developer directory for the C path remap.
    developer: String,
    /// Execution path of a separately owned SDK.
    sdk: Option<String>,
    /// Digest of the Apple tool identity record.
    identity_sha256: String,
}

/// Derives the Darwin linker, C tool and debug-map settings.
///
/// The Mach-O debug map is kept, but `-oso_prefix` makes the linker strip
/// the recipe-owned parent of the Rust tool root (and, as binding requires,
/// of the target directory) from every object path, and `ZERO_AR_DATE=1`
/// makes it record zero object modification times. The linker is selected
/// through `--config target."<triple>".linker` because the environment form
/// is a denied tool selector; `xcrun` and the `/usr/bin` shims are never used.
///
/// # Errors
///
/// Returns [`ReleaseError::Recipe`] when the target is not Darwin, an Apple
/// path is outside the boundary roots or not plain text, or the parent of the
/// Rust tool root is missing or holds a `,` that `-Wl,` would split.
fn darwin_plan(
    paths: &dyn ExecutionPaths,
    target: crate::policy::ReleaseTarget,
    apple: &AppleDeveloperTools,
    tool_root: &str,
) -> Result<DarwinPlan, Report<ReleaseError>> {
    if !target.is_darwin() {
        return Err(recipe_error(
            "Apple developer tools apply only to Darwin targets",
        ));
    }
    let developer = execution_text(paths, "Apple developer directory", apple.developer_dir())?;
    let sdk = apple
        .owned_sdk_root()
        .map(|root| execution_text(paths, "owned Apple SDK", root))
        .transpose()?;
    let clang = execution_text(paths, "Apple clang", apple.clang())?;
    let parent = std::path::Path::new(tool_root)
        .parent()
        .and_then(std::path::Path::to_str)
        .filter(|parent| *parent != "/" && !parent.contains(','))
        .ok_or_else(|| recipe_error("Rust tool root has no plain recipe-owned parent"))?;
    let triple = target.triple();
    let underscored = triple.replace('-', "_");
    let environment = BTreeMap::from([
        (DEVELOPER_DIR_VARIABLE.to_owned(), developer.clone()),
        (
            SDKROOT_VARIABLE.to_owned(),
            execution_text(paths, "Apple SDK root", apple.sdk_root())?,
        ),
        (format!("CC_{underscored}"), clang.clone()),
        (
            format!("AR_{underscored}"),
            execution_text(paths, "Apple ar", apple.ar())?,
        ),
        (ZERO_AR_DATE_VARIABLE.to_owned(), "1".to_owned()),
        // Cargo's LibreSSL initialization must not read ambient host configuration.
        ("OPENSSL_CONF".to_owned(), "/dev/null".to_owned()),
    ]);
    let mut rustflags = vec![format!("-Clink-arg=-Wl,-oso_prefix,{parent}/")];
    if let Some(sdk) = &sdk {
        rustflags.extend([
            format!("--remap-path-prefix={sdk}={REMAPPED_SDK}"),
            "-Clink-arg=-isysroot".to_owned(),
            format!("-Clink-arg={sdk}"),
        ]);
    }
    Ok(DarwinPlan {
        rustflags,
        configs: vec![
            "--config".to_owned(),
            format!("target.\"{triple}\".linker=\"{clang}\""),
        ],
        environment,
        developer,
        sdk,
        identity_sha256: apple.identity_sha256()?,
    })
}
