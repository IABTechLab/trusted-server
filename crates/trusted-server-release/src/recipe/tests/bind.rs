use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use super::*;
use crate::policy::ReleaseTarget;
use crate::preflight::audit_environment;
use crate::preflight::child::build_child_for_tests;

const OTHER_COMMIT: &str = "fedcba9876543210fedcba9876543210fedcba98";

fn audited() -> AuditedEnvironment {
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

/// A fictional layout with a completed JS handoff and every recipe root.
struct Fixture {
    _directory: tempfile::TempDir,
    base: PathBuf,
    completed: JsBuildCompleted,
}

impl Fixture {
    fn new(js_commit: &str) -> Self {
        let directory = tempfile::tempdir().expect("should create fixture directory");
        let base = fs::canonicalize(directory.path()).expect("should canonicalize fixture");
        for relative in [
            "materials",
            "output",
            "checkout-a",
            "checkout-b",
            "tools/bin",
            "other-source",
            "other-work",
        ] {
            fs::create_dir_all(base.join(relative)).expect("should create fixture directory");
        }
        for tool in ["tools/bin/cargo", "tools/bin/rustc"] {
            fs::write(base.join(tool), b"fictional tool").expect("should write fictional tool");
        }
        Self {
            _directory: directory,
            base,
            completed: JsBuildCompleted::fixture_for_tests(js_commit),
        }
    }

    fn retained(&self) -> PathBuf {
        fs::canonicalize(
            self.completed
                .rehash_for_handoff()
                .expect("should rehash the fixture handoff"),
        )
        .expect("should canonicalize the retained root")
    }

    fn request(&self, asset: &str, role: &str, commit: &str) -> RecipeRequest {
        self.request_at(asset, role, commit, &self.base.join("target"))
    }

    fn request_at(
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
            output: &self.base.join("output"),
            target_dir,
        })
        .expect("should parse the fixture request")
    }

    fn ambient(&self, role: Role) -> CheckedAmbient {
        match role {
            Role::A => CheckedAmbient::check(
                Role::A,
                &AmbientProfile::A,
                ObservedAmbient {
                    checkout_root: self.base.join("checkout-a"),
                    ..observed_a()
                },
            ),
            Role::B => CheckedAmbient::check(
                Role::B,
                &AmbientProfile::alternate_for_tests(),
                ObservedAmbient {
                    checkout_root: self.base.join("checkout-b"),
                    ..observed_b()
                },
            ),
        }
        .expect("should check the fixture ambient")
    }

    fn tools(&self) -> SelectedRustTools {
        SelectedRustTools::for_tests(&self.base.join("tools")).expect("should select fixture tools")
    }

    fn bind_with(
        &self,
        request: RecipeRequest,
        role: Role,
        commit: &str,
    ) -> Result<PreparedRecipe, Report<ReleaseError>> {
        let commit = CommitId::parse(commit).expect("should parse fixture commit");
        let child =
            build_child_for_tests(&audited(), &commit, 1_700_000_000, &child_request(&request))
                .expect("should build the fixture child environment");
        PreparedRecipe::bind(BindInputs {
            commit: &commit,
            child,
            completed: &self.completed,
            tools: self.tools(),
            ambient: self.ambient(role),
            request,
        })
    }

    fn bind(&self, asset: &str, role: Role) -> Result<PreparedRecipe, Report<ReleaseError>> {
        let text = if role == Role::A { "A" } else { "B" };
        self.bind_with(self.request(asset, text, COMMIT), role, COMMIT)
    }
}

fn plan_error(prepared: &PreparedRecipe) -> String {
    match prepared.check_plan() {
        Ok(()) => panic!("should reject the plan"),
        Err(error) => format!("{error:?}"),
    }
}

fn bind_error(result: Result<PreparedRecipe, Report<ReleaseError>>) -> String {
    match result {
        Ok(_) => panic!("should reject the binding"),
        Err(error) => format!("{error:?}"),
    }
}

#[test]
fn js_handoff_for_other_source_commit_is_rejected() {
    Fixture::new(COMMIT)
        .bind("ts-aarch64-apple-darwin", Role::A)
        .expect("should bind the matching handoff");
    let other = Fixture::new(OTHER_COMMIT);
    assert!(
        bind_error(other.bind("ts-aarch64-apple-darwin", Role::A))
            .contains("another source commit"),
        "should reject a JS handoff built from another commit"
    );
    assert!(
        !other.base.join("target").exists(),
        "should not create the target after a rejection"
    );
    let fixture = Fixture::new(COMMIT);
    let request = fixture.request("ts-aarch64-apple-darwin", "A", OTHER_COMMIT);
    assert!(
        bind_error(fixture.bind_with(request, Role::A, COMMIT)).contains("another source commit"),
        "should reject a request naming another commit"
    );
}

#[test]
fn js_handoff_mutated_after_completion_is_rejected() {
    let changed = Fixture::new(COMMIT);
    let request = changed.request("ts-aarch64-apple-darwin", "A", COMMIT);
    fs::write(changed.retained().join("bundles/tsjs-core.js"), b"tampered")
        .expect("should mutate a retained module");
    assert!(
        bind_error(changed.bind_with(request, Role::A, COMMIT)).contains("no longer rehashes"),
        "should reject a mutated retained module"
    );
    let extra = Fixture::new(COMMIT);
    let request = extra.request("ts-aarch64-apple-darwin", "A", COMMIT);
    fs::write(extra.retained().join("unrecorded"), b"extra").expect("should add an entry");
    assert!(
        bind_error(extra.bind_with(request, Role::A, COMMIT)).contains("no longer rehashes"),
        "should reject an unrecorded retained entry"
    );
    assert!(
        !changed.base.join("target").exists(),
        "should not create the target after a rejection"
    );
}

#[test]
fn request_for_other_js_directory_or_role_is_rejected() {
    let fixture = Fixture::new(COMMIT);
    let foreign = Fixture::new(COMMIT);
    let request = foreign.request("ts-aarch64-apple-darwin", "A", COMMIT);
    assert!(
        bind_error(fixture.bind_with(request, Role::A, COMMIT)).contains("does not name"),
        "should reject a request naming another JS handoff directory"
    );
    let request = fixture.request("ts-aarch64-apple-darwin", "B", COMMIT);
    assert!(
        bind_error(fixture.bind_with(request, Role::A, COMMIT)).contains("ambient role"),
        "should reject an ambient of another role"
    );
}

#[test]
fn cargo_plan_has_fixed_release_frozen_json_flags() {
    for (index, artifact) in FIXED_ARTIFACTS.iter().enumerate() {
        let role = if index % 2 == 0 { Role::A } else { Role::B };
        let jobs = if role == Role::A { "4" } else { "2" };
        let fixture = Fixture::new(COMMIT);
        let prepared = fixture
            .bind(artifact.name, role)
            .expect("should bind every fixed asset");
        let expected_arguments = [
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
            jobs,
        ];
        assert_eq!(
            prepared.plan().arguments,
            expected_arguments,
            "should use the exact fixed arguments for {}",
            artifact.name
        );
        assert_eq!(
            prepared.plan().program,
            fixture.base.join("tools/bin/cargo"),
            "should run the selected Cargo"
        );
        let checkout = if role == Role::A {
            "checkout-a"
        } else {
            "checkout-b"
        };
        assert_eq!(
            prepared.plan().working_directory,
            fixture.base.join(checkout),
            "should run in the role checkout"
        );
        assert_environment(&fixture, &prepared, artifact.target);
        assert!(
            fixture.base.join("target").is_dir(),
            "should create the fresh target directory"
        );
        prepared
            .check_plan()
            .expect("should accept the untouched plan");
    }
}

fn assert_environment(fixture: &Fixture, prepared: &PreparedRecipe, target: ReleaseTarget) {
    let retained = fixture.retained();
    let path = |path: PathBuf| path.to_str().expect("should be UTF-8").to_owned();
    let mut expected = BTreeMap::from([
        ("AWS_LC_SYS_USE_SYSTEM".to_owned(), "0".to_owned()),
        (
            format!(
                "AWS_LC_SYS_USE_SYSTEM_{}",
                target.triple().replace('-', "_")
            ),
            "0".to_owned(),
        ),
        (
            "CARGO_HOME".to_owned(),
            path(fixture.base.join("materials")),
        ),
        (
            "CARGO_TARGET_DIR".to_owned(),
            path(fixture.base.join("target")),
        ),
        ("HOME".to_owned(), path(fixture.base.join("materials/home"))),
        ("LC_ALL".to_owned(), "C".to_owned()),
        ("LIBSQLITE3_SYS_USE_PKG_CONFIG".to_owned(), "0".to_owned()),
        ("SOURCE_DATE_EPOCH".to_owned(), "1700000000".to_owned()),
        ("TRUSTED_SERVER__GIT_VERSION".to_owned(), COMMIT.to_owned()),
        ("TRUSTED_SERVER__RELEASE_BUILD".to_owned(), "1".to_owned()),
        (
            "TRUSTED_SERVER__TSJS_PREBUILT_INVENTORY".to_owned(),
            path(retained.join("tsjs-inventory.json")),
        ),
        (
            "TSJS_PREBUILT_DIR".to_owned(),
            path(retained.join("bundles")),
        ),
        ("TZ".to_owned(), "UTC".to_owned()),
    ]);
    if target.is_darwin() {
        expected.insert("MACOSX_DEPLOYMENT_TARGET".to_owned(), "13.0".to_owned());
    }
    assert_eq!(
        prepared.plan().environment,
        expected,
        "should hold exactly the child environment plus three recipe variables"
    );
}

#[test]
fn cargo_plan_rejects_inherited_rustflags_and_selectors() {
    for name in [
        "RUSTFLAGS",
        "CARGO_ENCODED_RUSTFLAGS",
        "CARGO_BUILD_TARGET",
        "RUSTUP_TOOLCHAIN",
        "CC",
        "TSJS_SKIP_BUILD",
    ] {
        let snapshot = [(OsString::from(name), OsString::from("fictional"))];
        let rejection = match audit_environment(&snapshot) {
            Ok(_) => panic!("should reject inherited {name}"),
            Err(error) => format!("{error:?}"),
        };
        assert!(
            rejection.contains(name),
            "should reject inherited {name} before any plan exists"
        );
    }
    let fixture = Fixture::new(COMMIT);
    let mut prepared = fixture
        .bind("ts-x86_64-unknown-linux-gnu", Role::A)
        .expect("should bind from a benign environment");
    let plan = prepared.plan();
    for name in plan.environment.keys() {
        assert!(
            !name.starts_with("RUST") && !name.starts_with("CARGO_BUILD"),
            "should not carry selector {name}"
        );
    }
    for argument in &plan.arguments {
        assert!(
            ![
                "--config",
                "--features",
                "--all-features",
                "--no-default-features",
                "--manifest-path"
            ]
            .iter()
            .any(|selector| argument.starts_with(selector))
                && !argument.starts_with('+'),
            "should not carry selector argument {argument}"
        );
    }
    prepared
        .plan
        .environment
        .insert("RUSTFLAGS".to_owned(), "--cfg fictional".to_owned());
    assert!(
        plan_error(&prepared).contains("differs"),
        "should reject an injected RUSTFLAGS"
    );
    prepared.plan.environment.remove("RUSTFLAGS");
    prepared
        .check_plan()
        .expect("should accept the restored plan");
    prepared.plan.environment.remove("LC_ALL");
    assert!(
        plan_error(&prepared).contains("differs"),
        "should reject a removed normalization variable"
    );
    let fixture = Fixture::new(COMMIT);
    let mut prepared = fixture
        .bind("ts-x86_64-unknown-linux-gnu", Role::A)
        .expect("should bind again");
    for change in ["--features=fictional", "+nightly", "--config=fictional"] {
        prepared.plan.arguments.push(change.to_owned());
        assert!(
            plan_error(&prepared).contains("differs"),
            "should reject added argument {change}"
        );
        prepared.plan.arguments.pop();
    }
    prepared
        .plan
        .arguments
        .retain(|argument| argument != "--release");
    assert!(
        plan_error(&prepared).contains("differs"),
        "should reject a dropped --release"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn execute_rejects_boundary_with_other_roots() {
    let fixture = Fixture::new(COMMIT);
    let prepared = fixture
        .bind("ts-aarch64-apple-darwin", Role::A)
        .expect("should bind the recipe");
    let other = BoundaryRoots::new(
        &[fixture.base.join("other-source")],
        &[fixture.base.join("tools")],
        &[fixture.base.join("other-work")],
    )
    .expect("should build unrelated roots");
    let boundary = Boundary::mac_sandbox_for_tests(other).expect("should configure, not run");
    let error = prepared
        .execute(&boundary, &DenialProof::for_tests())
        .expect_err("should reject a boundary over other roots");
    assert!(
        format!("{error:?}").contains("roots differ"),
        "should name the root mismatch"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn execute_rejects_boundary_without_owned_descendant_cleanup() {
    let fixture = Fixture::new(COMMIT);
    let prepared = fixture
        .bind("ts-aarch64-apple-darwin", Role::A)
        .expect("should bind the recipe");
    let roots = prepared
        .boundary_roots()
        .expect("should derive exact roots");
    let boundary =
        Boundary::mac_sandbox_for_tests(roots.clone()).expect("should configure, not run");
    assert!(
        boundary.matches_roots(&roots),
        "should hold the exact recipe roots so only cleanup can reject"
    );
    let error = prepared
        .execute(&boundary, &DenialProof::for_tests())
        .expect_err("should reject a boundary without owned cleanup");
    assert!(
        format!("{error:?}").contains("descendant cleanup"),
        "should name the missing descendant cleanup"
    );
}

#[test]
fn qualified_rust_tools_constructor_is_uninhabited() {
    let never: fn(QualifiedRustTools) -> ! = |qualification| match qualification {};
    let _constructor: fn(&Path, QualifiedRustTools) -> SelectedRustTools =
        SelectedRustTools::qualified;
    let _ = never;
    let closed =
        AmbientProfile::for_role(Role::B).expect_err("should keep the alternate ambient closed");
    assert!(
        format!("{closed:?}").contains("not qualified"),
        "should name the missing qualification"
    );
}

#[test]
fn root_filled_between_parse_and_bind_is_rejected() {
    for (relative, label) in [("materials", "materials"), ("output", "output")] {
        let fixture = Fixture::new(COMMIT);
        let request = fixture.request("ts-aarch64-apple-darwin", "A", COMMIT);
        fs::write(
            fixture.base.join(relative).join("late"),
            b"written after parse",
        )
        .expect("should fill the root after parsing");
        let error = bind_error(fixture.bind_with(request, Role::A, COMMIT));
        assert!(
            error.contains(label) && error.contains("cannot be reused"),
            "should reject a {label} root filled after parsing"
        );
        assert!(
            !fixture.base.join("target").exists(),
            "should not create the target after a rejection"
        );
    }
    let fixture = Fixture::new(COMMIT);
    let request = fixture.request("ts-aarch64-apple-darwin", "A", COMMIT);
    fs::create_dir(fixture.base.join("target")).expect("should create the target after parse");
    assert!(
        bind_error(fixture.bind_with(request, Role::A, COMMIT)).contains("cannot be reused"),
        "should reject a target created after parsing"
    );
}

#[test]
fn target_inside_checkout_is_rejected_before_creation() {
    let fixture = Fixture::new(COMMIT);
    let inside = fixture.base.join("checkout-a/target");
    let request = fixture.request_at("ts-aarch64-apple-darwin", "A", COMMIT, &inside);
    assert!(
        bind_error(fixture.bind_with(request, Role::A, COMMIT)).contains("overlap"),
        "should reject a target inside the checkout root"
    );
    assert!(!inside.exists(), "should create nothing in the checkout");
    let fixture = Fixture::new(COMMIT);
    let inside_tools = fixture.base.join("tools/target");
    let request = fixture.request_at("ts-aarch64-apple-darwin", "A", COMMIT, &inside_tools);
    assert!(
        bind_error(fixture.bind_with(request, Role::A, COMMIT)).contains("overlap"),
        "should reject a target inside the tools root"
    );
    assert!(!inside_tools.exists(), "should create nothing in the tools");
}

#[test]
fn tampered_program_or_working_directory_is_rejected() {
    let fixture = Fixture::new(COMMIT);
    let mut prepared = fixture
        .bind("ts-aarch64-apple-darwin", Role::A)
        .expect("should bind the recipe");
    prepared.plan.program = fixture.base.join("tools/bin/rustc");
    assert!(
        plan_error(&prepared).contains("differs"),
        "should reject a swapped program"
    );
    prepared.plan.program = fixture.base.join("tools/bin/cargo");
    prepared.plan.working_directory = fixture.base.join("checkout-b");
    assert!(
        plan_error(&prepared).contains("differs"),
        "should reject a swapped working directory"
    );
}

fn confinement_limits() -> ProcessLimits {
    ProcessLimits {
        wall: std::time::Duration::from_secs(60),
        stdout_max: 1024,
        stderr_max: 1024,
    }
}

#[test]
fn plan_carries_the_role_umask() {
    for (role, expected) in [(Role::A, 0o022), (Role::B, 0o027)] {
        let fixture = Fixture::new(COMMIT);
        let prepared = fixture
            .bind("ts-aarch64-apple-darwin", role)
            .expect("should bind the recipe");
        assert_eq!(
            prepared.plan().umask,
            Umask::new(expected).expect("should accept mask"),
            "should derive the umask of role {role:?} from its ambient profile"
        );
    }
}

#[test]
fn tampered_umask_is_rejected_by_the_plan_recheck() {
    let fixture = Fixture::new(COMMIT);
    let mut prepared = fixture
        .bind("ts-aarch64-apple-darwin", Role::A)
        .expect("should bind the recipe");
    prepared.plan.umask = Umask::new(0o000).expect("should accept mask");
    assert!(
        plan_error(&prepared).contains("differs"),
        "should reject a weakened umask"
    );
    prepared.plan.umask = Umask::new(0o027).expect("should accept mask");
    assert!(
        plan_error(&prepared).contains("differs"),
        "should reject another role's umask"
    );
    prepared.plan.umask = Umask::new(0o022).expect("should accept mask");
    prepared
        .check_plan()
        .expect("should accept the restored umask");
}

#[test]
fn confined_command_applies_the_role_umask() {
    for (role, expected) in [(Role::A, "022"), (Role::B, "027")] {
        let fixture = Fixture::new(COMMIT);
        let prepared = fixture
            .bind("ts-aarch64-apple-darwin", role)
            .expect("should bind the recipe");
        let command = prepared
            .plan()
            .confined_command(confinement_limits())
            .expect("should describe the confined command");
        assert_eq!(
            command
                .requested_umask()
                .map(|umask| umask.to_string())
                .as_deref(),
            Some(expected),
            "should set the umask of role {role:?} on the confined command"
        );
        let described = format!("{command:?}");
        for part in [
            format!("{:?}", prepared.plan().program),
            format!("{:?}", prepared.plan().working_directory),
            format!(
                "{:?}",
                prepared
                    .plan()
                    .arguments
                    .iter()
                    .map(OsString::from)
                    .collect::<Vec<_>>()
            ),
        ] {
            assert!(
                described.contains(&part),
                "should describe {part} in {described}"
            );
        }
    }
}
