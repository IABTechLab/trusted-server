//! A fictional layout with a completed JS handoff and every recipe root.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use super::super::ambient::ObservedAmbient;
use super::super::paths::PrefixPaths;
use super::*;
use crate::boundary::Umask;
use crate::materials::PreparedCargoMaterials;
use crate::policy::{FIXED_ARTIFACTS, ReleaseTarget};
use crate::preflight::audit_environment;
use crate::preflight::child::build_child_for_tests;
use crate::process::ProcessLimits;

pub(super) const OTHER_COMMIT: &str = "fedcba9876543210fedcba9876543210fedcba98";
pub(super) const GIT_SELECTOR: &str = "git+https://example.com/org/workspace?tag=v1";

pub(super) fn audited() -> AuditedEnvironment {
    let snapshot: Vec<(OsString, OsString)> = [
        ("PATH", "/usr/bin"),
        ("TRUSTED_SERVER__PROXY__SECRET", "fictional"),
        ("LANG", "en_US.UTF-8"),
    ]
    .into_iter()
    .map(|(name, value)| (OsString::from(name), OsString::from(value)))
    .collect();
    audit_environment(&snapshot).expect("should audit benign environment")
}

pub(super) struct Fixture {
    pub(super) _directory: tempfile::TempDir,
    pub(super) base: PathBuf,
    pub(super) completed: JsBuildCompleted,
}

impl Fixture {
    pub(super) fn new(js_commit: &str) -> Self {
        let directory = tempfile::tempdir().expect("should create fixture directory");
        let base = fs::canonicalize(directory.path()).expect("should canonicalize fixture");
        for relative in [
            "materials/vendor/example-1.0.0",
            "cargo-state",
            "output",
            "checkout-canonical",
            "checkout-reproduction",
            "tools/bin",
            "other-source",
            "other-work",
        ] {
            fs::create_dir_all(base.join(relative)).expect("should create fixture directory");
        }
        fs::write(
            base.join("materials/vendor/example-1.0.0/Cargo.toml"),
            b"fictional package",
        )
        .expect("should write fictional vendored package");
        for tool in ["tools/bin/cargo", "tools/bin/rustc"] {
            fs::write(base.join(tool), b"fictional tool").expect("should write fictional tool");
        }
        for artifact in &FIXED_ARTIFACTS {
            let standard = base
                .join("tools/lib/rustlib")
                .join(artifact.target.triple())
                .join("lib");
            fs::create_dir_all(&standard).expect("should create target standard library");
            fs::write(
                standard.join("libstd-0123456789abcdef.rlib"),
                b"fictional std",
            )
            .expect("should write fictional libstd");
        }
        Self {
            _directory: directory,
            base,
            completed: JsBuildCompleted::fixture_for_tests(js_commit),
        }
    }

    pub(super) fn retained(&self) -> PathBuf {
        fs::canonicalize(
            self.completed
                .rehash_for_handoff()
                .expect("should rehash the fixture handoff"),
        )
        .expect("should canonicalize the retained root")
    }

    pub(super) fn request(&self, asset: &str, role: &str, commit: &str) -> RecipeRequest {
        self.request_at(asset, role, commit, &self.base.join("target"))
    }

    pub(super) fn request_at(
        &self,
        asset: &str,
        role: &str,
        commit: &str,
        target_dir: &Path,
    ) -> RecipeRequest {
        let retained = self.retained();
        let js_dir = retained.join("bundles");
        let js_inventory = retained.join("tsjs-inventory.json");
        RecipeRequest::parse(&RecipeInput {
            asset,
            variant: "default",
            role,
            commit,
            js_dir: &js_dir,
            js_inventory: &js_inventory,
            materials: &self.base.join("materials"),
            cargo_state: &self.base.join("cargo-state"),
            output: &self.base.join("output"),
            target_dir,
        })
        .expect("should parse the fixture request")
    }

    pub(super) fn ambient(&self, role: Role) -> CheckedAmbient {
        match role {
            Role::Canonical => CheckedAmbient::check(
                Role::Canonical,
                &AmbientProfile::CANONICAL,
                ObservedAmbient {
                    checkout_root: self.base.join("checkout-canonical"),
                    ..observed_canonical()
                },
            ),
            Role::Reproduction => CheckedAmbient::check(
                Role::Reproduction,
                &AmbientProfile::alternate_for_tests(),
                ObservedAmbient {
                    checkout_root: self.base.join("checkout-reproduction"),
                    ..observed_reproduction()
                },
            ),
        }
        .expect("should check the fixture ambient")
    }

    pub(super) fn tools_for(&self, target: ReleaseTarget) -> SelectedRustTools {
        SelectedRustTools::for_tests(&self.base.join("tools"), target)
            .expect("should select fixture tools")
    }

    pub(super) fn materials(&self, commit: &str) -> PreparedCargoMaterials {
        PreparedCargoMaterials::for_tests(&self.base.join("materials"), commit, &[GIT_SELECTOR])
            .expect("should describe fixture materials")
    }

    /// Binds with explicit tools and materials.
    pub(super) fn bind_parts(
        &self,
        request: RecipeRequest,
        role: Role,
        commit: &str,
        tools: SelectedRustTools,
        materials: PreparedCargoMaterials,
    ) -> Result<PreparedRecipe, Report<ReleaseError>> {
        let commit = CommitId::parse(commit).expect("should parse fixture commit");
        let child =
            build_child_for_tests(&audited(), &commit, 1_700_000_000, &child_request(&request))
                .expect("should build the fixture child environment");
        PreparedRecipe::bind(BindInputs {
            commit: &commit,
            child,
            completed: &self.completed,
            tools,
            materials,
            ambient: self.ambient(role),
            request,
        })
    }

    pub(super) fn bind_with(
        &self,
        request: RecipeRequest,
        role: Role,
        commit: &str,
    ) -> Result<PreparedRecipe, Report<ReleaseError>> {
        let tools = self.tools_for(request.asset.artifact().target);
        let materials = self.materials(commit);
        self.bind_parts(request, role, commit, tools, materials)
    }

    pub(super) fn bind(
        &self,
        asset: &str,
        role: Role,
    ) -> Result<PreparedRecipe, Report<ReleaseError>> {
        let text = role.spec_label();
        self.bind_with(self.request(asset, text, COMMIT), role, COMMIT)
    }

    /// The fake namespace mirroring the boundary's mount layout for `role`.
    pub(super) fn paths(&self, role: Role) -> PrefixPaths {
        let checkout = if role == Role::Canonical {
            "checkout-canonical"
        } else {
            "checkout-reproduction"
        };
        PrefixPaths::new()
            .with(&self.base.join(checkout), "/boundary/immutable/0", false)
            .with(&self.retained(), "/boundary/immutable/1", false)
            .with(&self.base.join("materials"), "/boundary/immutable/2", false)
            .with(&self.base.join("tools"), "/boundary/tool/0", false)
            .with(&self.base.join("cargo-state"), "/boundary/writable/0", true)
            .with(&self.base.join("target"), "/boundary/writable/1", true)
    }
}

pub(super) fn plan_error(prepared: &PreparedRecipe, paths: &PrefixPaths) -> String {
    match prepared.plan(paths) {
        Ok(_) => panic!("should reject the plan"),
        Err(error) => format!("{error:?}"),
    }
}

pub(super) fn bind_error(result: Result<PreparedRecipe, Report<ReleaseError>>) -> String {
    match result {
        Ok(_) => panic!("should reject the binding"),
        Err(error) => format!("{error:?}"),
    }
}

pub(super) fn confinement_limits() -> ProcessLimits {
    ProcessLimits {
        wall: std::time::Duration::from_secs(60),
        stdout_max: 1024,
        stderr_max: 1024,
    }
}

pub(super) fn umask(mask: u32) -> Umask {
    Umask::new(mask).expect("should accept mask")
}
