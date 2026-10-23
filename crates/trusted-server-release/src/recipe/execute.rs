//! The confined run of a bound recipe and the checks around it.
//!
//! Every input is rehashed immediately before and after the confined Cargo
//! process, Cargo home must gain no fetched registry or Git sources, and only a
//! successful run is parsed, copied and bound into a [`RecipeCompleted`].

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use error_stack::Report;
use sha2::{Digest as _, Sha256};

use super::apple::AppleDeveloperTools;
use super::artifact::{EXECUTABLE_LIMIT, copy_artifact};
use super::cargo_output::{ExpectedBinary, locate_host_binary};
use super::completed::SuccessFacts;
use super::gnu::{GnuEvidence, MeasurementSandbox};
use super::plan::CargoQuery;
use super::{
    Authority, ExactInvocationDigest, ExecutionPaths, PreparedRecipe, RecipeCompleted,
    SelectedRustTools, WorkspaceSelection, recipe_error,
};
use crate::boundary::{Boundary, DenialProof, DenialReceipt, DescendantCleanup};
use crate::composition::external::ExternalRequirements;
use crate::composition::javascript::{JsInputs, JsRetentionPolicy, collect_js};
use crate::composition::native::{self, NativeInputs};
use crate::composition::origin::join_origins;
use crate::composition::platform::PlatformEvidence;
use crate::composition::{CargoCapture, CargoLayout};
use crate::documents::{Role as DocumentRole, read_bounded};
use crate::error::ReleaseError;
use crate::inspect::{ElfPolicy, MachoCpu, MachoPolicy, inspect_elf, inspect_macho, inspect_wasm};
use crate::policy::ReleaseTarget;
use crate::process::ProcessLimits;
use crate::tool_tree::tool_tree_digest;

/// Captured output limit of each Cargo stream.
const BUILD_STREAM_MAX: u64 = 16 * 1024 * 1024;

/// Limits of the confined Cargo process under the selection's build wall.
fn build_limits(tools: &SelectedRustTools) -> ProcessLimits {
    ProcessLimits {
        wall: tools.build_wall(),
        stdout_max: BUILD_STREAM_MAX,
        stderr_max: BUILD_STREAM_MAX,
    }
}

/// What a successful confined run produced, as the run path reports it.
pub(super) struct RunOutput {
    pub(super) gnu: Option<GnuEvidence>,
    pub(super) stdout: Vec<u8>,
    pub(super) stderr_sha256: String,
    pub(super) started_at_seconds: u64,
    pub(super) elapsed: Duration,
    /// The descendant cleanup of the boundary that ran the build.
    pub(super) cleanup: DescendantCleanup,
    /// Supporting output captured by this same denied recipe invocation.
    pub(super) queries: CargoQueryOutput,
}

/// Supporting observations from execution, or an explicit pure-test fixture.
pub(super) enum CargoQueryOutput {
    /// Raw metadata and tree output from the selected Cargo under the proof.
    Captured {
        metadata: Vec<u8>,
        tree: Vec<u8>,
        plan_digests: [String; 2],
        denial: DenialReceipt,
        fastly_tools: Option<Box<super::fastly_tools::FastlyToolEvidence>>,
    },
    /// Pure completion tests never execute Cargo and cannot supply real observations.
    #[cfg(test)]
    Fixture,
}

/// Admits a boundary's descendant cleanup for a selection.
///
/// Container-owned cleanup is always admitted. Post-run-reaped cleanup,
/// where survivors exist until the reap after the run, is admitted only when
/// `allowance` (the selection's
/// [`permits_post_run_reaped_cleanup`](super::SelectedRustTools::permits_post_run_reaped_cleanup))
/// holds.
///
/// # Errors
///
/// Returns [`ReleaseError::Recipe`] for post-run-reaped cleanup without the
/// allowance.
pub(super) fn cleanup_gate(
    cleanup: DescendantCleanup,
    allowance: bool,
) -> Result<(), Report<ReleaseError>> {
    match cleanup {
        DescendantCleanup::OwnedByContainer => Ok(()),
        DescendantCleanup::ReapedAfterRun if allowance => Ok(()),
        DescendantCleanup::ReapedAfterRun => Err(recipe_error(
            "post-run-reaped cleanup is not permitted for this selection",
        )),
    }
}

impl PreparedRecipe {
    /// Requires the tools to be the tree measured at binding and any Apple
    /// developer tools to still match their record.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Recipe`] when anything under the tool root was
    /// edited, linked, added or removed, or an Apple tool, the SDK settings,
    /// the detached SDK or the measured toolchain changed.
    pub(super) fn check_tools(&self) -> Result<(), Report<ReleaseError>> {
        self.check_rust_tools()?;
        self.tools
            .apple()
            .map_or(Ok(()), super::apple::AppleDeveloperTools::verify)
    }

    /// Requires the Rust tools to be the tree measured at binding.
    ///
    /// Unlike [`Self::check_tools`] this never rehashes the Apple developer
    /// tools. Callers use it only where a full [`Self::check_inputs`]
    /// brackets the same step: before the run (followed by the pre-run
    /// inputs check) and at completion (preceded by the post-run one).
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Recipe`] when anything under the tool root was
    /// edited, linked, added or removed.
    pub(super) fn check_rust_tools(&self) -> Result<(), Report<ReleaseError>> {
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

    /// Rechecks source and rehashes the JS handoff, tools and materials.
    pub(super) fn check_inputs(&self) -> Result<(), Report<ReleaseError>> {
        self.check_source()?;
        self.check_js_handoff()?;
        self.check_tools()?;
        self.check_materials()
    }

    /// Rechecks source while its working root still exists, before receipt sealing.
    fn check_source(&self) -> Result<(), Report<ReleaseError>> {
        match &self.workspace {
            WorkspaceSelection::Approved(workspace) => workspace.origins.verify_unchanged(),
            #[cfg(test)]
            WorkspaceSelection::Fixture => Ok(()),
        }
    }

    /// Requires Cargo's private home to hold no registry or Git cache.
    ///
    /// Cargo may initialize its registry directory and cache tag before source
    /// replacement. Only that exact bookkeeping is admitted; source/cache/index
    /// entries and every Git cache remain forbidden.
    pub(super) fn check_no_fetched_sources(&self) -> Result<(), Report<ReleaseError>> {
        super::cargo_home::check(&self.request.cargo_home(), &|| Ok(()))
    }

    /// Requires unchanged inputs, a derivable plan, the exact planned roots
    /// and a descendant cleanup the selection accepts; returns that cleanup.
    ///
    /// Only the Rust tools are rehashed here. The Apple developer tools are
    /// rehashed by the full [`Self::check_inputs`] that [`Self::execute`]
    /// runs next, with only pure plan and command derivation in between, so
    /// a pass here would hash the same SDK and toolchain twice.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Recipe`] when the Rust tools or materials
    /// changed, the plan cannot be derived in the boundary's namespace, the
    /// boundary roots differ from [`Self::boundary_roots`], or
    /// [`cleanup_gate`] rejects the boundary's descendant cleanup.
    fn check_boundary(
        &self,
        boundary: &Boundary,
    ) -> Result<DescendantCleanup, Report<ReleaseError>> {
        self.check_rust_tools()?;
        self.check_materials()?;
        self.plan(boundary)?;
        if !boundary.matches_roots(&self.boundary_roots()?) {
            return Err(recipe_error(
                "boundary roots differ from the exact recipe roots",
            ));
        }
        let cleanup = boundary.descendant_cleanup();
        cleanup_gate(cleanup, self.tools.permits_post_run_reaped_cleanup())?;
        Ok(cleanup)
    }

    /// Checks every gate, runs Cargo confined and completes the run.
    ///
    /// Only a selection that [permits a candidate run](super::SelectedRustTools::permits_candidate_run)
    /// reaches the run: the test-only ordinary CI selection. That run is
    /// unqualified CI evidence; production builds can name no selection, so
    /// their gate stays closed. The JS handoff, tools and materials are
    /// rehashed immediately before and after the run, and Cargo home must
    /// gain no fetched sources beyond exact Cargo registry bookkeeping.
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
        let cleanup = self.check_boundary(boundary)?;
        let command = self
            .plan(boundary)?
            .confined_command(build_limits(&self.tools))?;
        if !self.tools.permits_candidate_run() {
            return Err(Report::new(ReleaseError::Unqualified)
                .attach("Cargo execution is closed until Rust tools and boundary are qualified"));
        }
        self.check_inputs()?;
        self.check_no_fetched_sources()?;
        // GNU measurements run in a sibling sandbox without the target root:
        // after the build that root holds Cargo's in-root hard links, which
        // the build boundary's pre-run single-link scan rejects.
        let gnu_sandbox =
            if self.request.asset.artifact().target == ReleaseTarget::X86_64UnknownLinuxGnu {
                Some(MeasurementSandbox::establish(
                    boundary,
                    &self.tools,
                    &self.request.target_dir,
                )?)
            } else {
                None
            };
        let gnu = gnu_sandbox
            .as_ref()
            .map(|sandbox| {
                GnuEvidence::measure(
                    &self.tools,
                    &self.plan(boundary)?,
                    &boundary.execution_fingerprint(),
                    sandbox,
                )
            })
            .transpose()?;
        let fastly_sandbox = if self.request.asset.artifact().target == ReleaseTarget::Wasm32Wasip1
            && cfg!(all(target_os = "linux", target_arch = "x86_64"))
        {
            Some(MeasurementSandbox::establish(
                boundary,
                &self.tools,
                &self.request.target_dir,
            )?)
        } else {
            None
        };
        let fastly_tools = fastly_sandbox
            .as_ref()
            .map(|sandbox| {
                super::fastly_tools::FastlyToolEvidence::measure(
                    &self.tools,
                    &self.plan(boundary)?,
                    &boundary.execution_fingerprint(),
                    sandbox,
                )
            })
            .transpose()?;
        let denial = proof.receipt_for(&boundary.execution_fingerprint())?;
        let run: Result<_, Report<ReleaseError>> = (|| {
            let query = |kind| {
                let plan = self.plan(boundary)?.observation_query(kind)?;
                let limits = ProcessLimits {
                    wall: Duration::from_secs(120),
                    ..build_limits(&self.tools)
                };
                let digest = plan.digest()?;
                let stdout = boundary
                    .run_confined(&plan.confined_command(limits)?, proof)?
                    .require_success()?;
                Ok::<_, Report<ReleaseError>>((stdout, digest))
            };
            let (metadata, metadata_plan) = query(CargoQuery::Metadata)?;
            let (tree, tree_plan) = query(CargoQuery::Tree)?;
            let started_at_seconds = unix_seconds()?;
            let result = boundary.run_confined(&command, proof)?;
            Ok((
                CargoQueryOutput::Captured {
                    metadata,
                    tree,
                    plan_digests: [metadata_plan, tree_plan],
                    denial,
                    fastly_tools: fastly_tools.map(Box::new),
                },
                result,
                started_at_seconds,
            ))
        })();
        let after = self
            .check_inputs()
            .and_then(|()| self.check_no_fetched_sources())
            .and_then(|()| {
                if let (Some(before), Some(sandbox)) = (&gnu, &gnu_sandbox) {
                    let after = GnuEvidence::measure(
                        &self.tools,
                        &self.plan(boundary)?,
                        &boundary.execution_fingerprint(),
                        sandbox,
                    )?;
                    before.verify_after(&after)?;
                }
                if let (
                    Some(sandbox),
                    Ok((
                        CargoQueryOutput::Captured {
                            fastly_tools: Some(before),
                            ..
                        },
                        _,
                        _,
                    )),
                ) = (&fastly_sandbox, &run)
                {
                    let after = super::fastly_tools::FastlyToolEvidence::measure(
                        &self.tools,
                        &self.plan(boundary)?,
                        &boundary.execution_fingerprint(),
                        sandbox,
                    )?;
                    before.verify_after(&after)?;
                }
                Ok(())
            });

        let (queries, result, started_at_seconds) = match (run, after) {
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
            gnu,
            stdout,
            stderr_sha256,
            started_at_seconds,
            elapsed,
            cleanup,
            queries,
        };
        self.complete(output, boundary.execution_fingerprint(), boundary)
    }

    /// Turns one successful run into a [`RecipeCompleted`].
    ///
    /// Parses Cargo's stream, maps the executable back through a writable
    /// root, copies it into the output directory and inspects the copy: a
    /// Wasm module, Linux or macOS executable must pass its inspector and
    /// keep the debug sections or debug map a stripped file lacks (see
    /// [`inspect_copied`]). Every target must also exclude all bound host and
    /// execution roots and contain a fixed remapped prefix.
    ///
    /// Only the Rust tools are rehashed here, since the receipt binds their
    /// digest. [`Self::execute`] already rehashed the Apple developer tools
    /// in its post-run [`Self::check_inputs`], with only in-process checks
    /// in between, and platform evidence rechecks them when it seals.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Recipe`] when the Rust tools or materials changed,
    /// the stream is not exactly one fresh debug-info binary, or the
    /// executable cannot be copied or rehashed, a bound path remains, no fixed
    /// remapped prefix exists, or [`ReleaseError::Inspect`] when the
    /// Wasm module or Linux executable is rejected or stripped.
    pub(super) fn complete(
        self,
        run: RunOutput,
        boundary_fingerprint: [u8; 32],
        paths: &dyn ExecutionPaths,
    ) -> Result<RecipeCompleted, Report<ReleaseError>> {
        self.check_rust_tools()?;
        self.check_materials()?;
        let plan_digest = ExactInvocationDigest::from_plan(&self.plan(paths)?)?;
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
        let (mut cargo_observations, denial_receipt, fastly_tools) = match run.queries {
            CargoQueryOutput::Captured {
                metadata,
                tree,
                plan_digests,
                denial,
                fastly_tools,
            } => {
                if !denial.matches_fingerprint(&boundary_fingerprint) {
                    return Err(recipe_error(
                        "socket observations belong to another execution boundary",
                    ));
                }
                let layout = CargoLayout::new(
                    artifact.target,
                    paths.execution_path(&self.request.target_dir)?,
                    paths.execution_path(self.ambient.checkout_root())?,
                )?;
                let captured =
                    CargoCapture::new(metadata, tree, &run.stdout, &layout, plan_digests)?;
                let selected_package_id =
                    captured.require_binary(&manifest, artifact.bin, &located.execution)?;
                (
                    Some((captured, selected_package_id)),
                    Some(denial),
                    fastly_tools,
                )
            }
            #[cfg(test)]
            CargoQueryOutput::Fixture => (None, None, None),
        };
        let copied = copy_artifact(
            &located.host,
            &self.request.target_dir,
            &self.request.output,
            artifact.name,
            EXECUTABLE_LIMIT,
        )?;
        let raw = read_bounded(copied.path(), DocumentRole::Executable)
            .map_err(|error| error.change_context(ReleaseError::Recipe))?;
        if raw.sha256 != copied.sha256() || raw.bytes.len() as u64 != copied.size() {
            return Err(recipe_error(
                "copied artifact changed before final-byte checks",
            ));
        }
        let requirements = inspect_copied(artifact.target, &raw.bytes, self.tools.apple())?;
        self.check_final_paths(&raw.bytes, paths)?;

        let cargo_observations = if let Some((captured, selected_package_id)) =
            cargo_observations.take()
        {
            Some(match &self.workspace {
                WorkspaceSelection::Approved(workspace) => {
                    let origins = join_origins(
                        captured.observations(),
                        &workspace.origins,
                        &self.materials,
                        paths,
                    )?;
                    let plan = self.plan(paths)?;
                    let native = native::collect(
                        captured.observations(),
                        &NativeInputs {
                            origins: origins.native_sources(),
                            execution_output_root: &paths
                                .execution_path(&self.request.target_dir)?,
                            host_output_root: &self.request.target_dir,
                            target: artifact.target,
                            environment: &plan.environment,
                            check: &|| Ok(()),
                        },
                    )?;
                    let javascript = collect_js(
                        &raw.bytes,
                        copied.sha256(),
                        &JsInputs {
                            retained_root: &self.retained_root,
                            modules: &self.js.modules,
                            inventory_sha256: &self.js.inventory_sha256,
                            policy: if artifact.target == ReleaseTarget::Wasm32Wasip1 {
                                JsRetentionPolicy::FastlyAll
                            } else {
                                JsRetentionPolicy::ObserveCli
                            },
                        },
                    )?;
                    self.check_source()?;
                    let platform = PlatformEvidence::collect(
                        &self.tools,
                        &self.tools_digest,
                        requirements,
                        artifact.target,
                        copied.sha256(),
                    )?;
                    let platform = if artifact.target == ReleaseTarget::X86_64UnknownLinuxGnu {
                        let gnu = run.gnu.ok_or_else(|| {
                            recipe_error("GNU completion lacks enforced in-image tool measurements")
                        })?;
                        gnu.verify_binding(&boundary_fingerprint, plan_digest.sha256())?;
                        platform.with_gnu(gnu)?
                    } else {
                        platform
                    };
                    if artifact.target == ReleaseTarget::Wasm32Wasip1
                        && cfg!(all(target_os = "linux", target_arch = "x86_64"))
                        && fastly_tools.is_none()
                    {
                        return Err(recipe_error(
                            "Fastly completion lacks enforced host-tool measurements",
                        ));
                    }
                    let platform = if let Some(evidence) = fastly_tools {
                        evidence.verify_binding(&boundary_fingerprint, plan_digest.sha256())?;
                        platform.with_fastly_tools(*evidence)?
                    } else {
                        platform
                    };
                    let captured = captured.with_evidence(
                        &selected_package_id,
                        origins,
                        native,
                        javascript,
                        platform,
                    )?;
                    self.check_source()?;
                    captured
                }
                #[cfg(test)]
                WorkspaceSelection::Fixture => captured,
            })
        } else {
            None
        };

        if self.tools.authority() == Authority::Qualified
            && !self.materials.derived_manifest_digests().is_empty()
        {
            return Err(Report::new(ReleaseError::Unqualified)
                .attach("a build from caller-derived manifests cannot carry qualified authority"));
        }
        let recipe_definition = match self.workspace {
            WorkspaceSelection::Approved(workspace) => Some(workspace.definition),
            #[cfg(test)]
            WorkspaceSelection::Fixture => None,
        };
        RecipeCompleted::record_success(SuccessFacts {
            asset: self.request.asset,
            role: self.request.role,
            commit: self.request.commit.clone(),
            cargo_lock_sha256: self.materials.cargo_lock_sha256().to_owned(),
            materials_digest: self.materials.digest().to_owned(),
            js_inventory_sha256: self.js.inventory_sha256.clone(),
            js_settings_sha256: self.js.settings_sha256.clone(),
            js_tools_sha256: self.js.tools_sha256.clone(),
            js_contributions: self.js.contributions,
            rust_tools_digest: self.tools_digest.clone(),
            boundary_fingerprint,
            denial_receipt,
            recipe_definition,
            plan_digest,
            ambient: self.ambient.clone(),
            started_at_seconds: run.started_at_seconds,
            elapsed: run.elapsed,
            descendant_cleanup: run.cleanup,
            stdout: run.stdout,
            stderr_sha256: run.stderr_sha256,
            artifact: copied,
            output_dir: self.request.output.clone(),
            normalized_manifest_digests: self.materials.normalized_manifest_digests().clone(),
            derived_manifest_digests: self.materials.derived_manifest_digests().clone(),
            cargo_observations,
        })
    }
}

/// Returns observed runtime requirements after accepting the copied final bytes.
///
/// Wasm must be a core module that kept its debug sections. A Linux
/// executable must pass the reviewed glibc 2.34 ELF policy (`x86_64`, the
/// reviewed loader and libraries, no search path, no runner path) and keep
/// its DWARF sections. A macOS executable must pass the reviewed Mach-O
/// policy for its CPU (minimum macOS 13.0, reviewed loader and libraries,
/// linker ad-hoc signature on arm64, relative zero-time debug map), keep a
/// non-empty debug map, and record the SDK version measured from the bound
/// Apple developer tools.
///
/// # Errors
///
/// Returns [`ReleaseError::Inspect`] when the final bytes are rejected, or
/// [`ReleaseError::Recipe`] for a Darwin artifact without Apple developer
/// tools or whose recorded SDK differs from the measured one.
fn inspect_copied(
    target: ReleaseTarget,
    bytes: &[u8],
    apple: Option<&AppleDeveloperTools>,
) -> Result<ExternalRequirements, Report<ReleaseError>> {
    let cpu = match target {
        ReleaseTarget::Wasm32Wasip1 => {
            let facts = inspect_wasm(bytes)?;
            facts.require_debug_sections()?;
            return Ok(ExternalRequirements::from_wasm(&facts));
        }
        ReleaseTarget::X86_64UnknownLinuxGnu => {
            let facts = inspect_elf(bytes, &ElfPolicy::linux_x86_64_gnu())?;
            facts.require_debug_sections()?;
            return Ok(ExternalRequirements::from_elf(&facts));
        }
        ReleaseTarget::Aarch64AppleDarwin => MachoCpu::Arm64,
        ReleaseTarget::X86_64AppleDarwin => MachoCpu::X86_64,
    };
    let apple =
        apple.ok_or_else(|| recipe_error("Darwin artifact has no Apple developer tools"))?;
    let facts = inspect_macho(bytes, &MachoPolicy::macos(cpu))?;
    facts.require_debug_map()?;
    if facts.sdk() != apple.sdk_version() {
        return Err(recipe_error(format!(
            "Mach-O records SDK {}, but the bound Apple SDK is {}",
            facts.sdk(),
            apple.sdk_version()
        )));
    }
    Ok(ExternalRequirements::from_macho(&facts))
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
