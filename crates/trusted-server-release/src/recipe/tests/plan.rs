use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;

use super::super::paths::PrefixPaths;
use super::super::plan::CargoQuery;
use super::complete::{built, wasm_with};
use super::fixture::*;
use super::*;
use crate::policy::FIXED_ARTIFACTS;
use crate::preflight::audit_environment;

/// The exact source-replacement pairs for the fixture's fictional Git source.
fn expected_replacement() -> Vec<String> {
    [
        "--config",
        "source.crates-io.replace-with=\"ts-vendor\"",
        "--config",
        "source.\"git+https://example.com/org/workspace?tag=v1\".git=\"https://example.com/org/workspace\"",
        "--config",
        "source.\"git+https://example.com/org/workspace?tag=v1\".tag=\"v1\"",
        "--config",
        "source.\"git+https://example.com/org/workspace?tag=v1\".replace-with=\"ts-vendor\"",
        "--config",
        "source.ts-vendor.directory=\"/boundary/immutable/2/vendor\"",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

/// Execution path of the fictional Apple clang in the fixture layout.
const FIXTURE_CLANG: &str = "/boundary/system/0/Toolchains/XcodeDefault.xctoolchain/usr/bin/clang";

#[test]
fn gnu_plan_pins_native_tools_and_rejects_backend_fallback() {
    let fixture = Fixture::new(COMMIT);
    let prepared = fixture
        .bind("ts-x86_64-unknown-linux-gnu", Role::Canonical)
        .expect("should bind GNU recipe");
    let plan = prepared
        .plan(&fixture.paths(Role::Canonical))
        .expect("should derive GNU plan");
    for (name, expected) in [
        ("CC_x86_64_unknown_linux_gnu", "/usr/bin/gcc"),
        ("AR_x86_64_unknown_linux_gnu", "/usr/bin/ar"),
        ("HOST_CC", "/usr/bin/gcc"),
        ("HOST_AR", "/usr/bin/ar"),
        ("AWS_LC_SYS_CMAKE_BUILDER", "0"),
        ("AWS_LC_SYS_CMAKE_BUILDER_x86_64_unknown_linux_gnu", "0"),
    ] {
        assert_eq!(
            plan.environment.get(name).map(String::as_str),
            Some(expected),
            "should enforce {name}"
        );
    }
    assert!(
        plan.arguments
            .iter()
            .any(|argument| argument
                == "target.\"x86_64-unknown-linux-gnu\".linker=\"/usr/bin/gcc\""),
        "should select the reviewed linker driver"
    );
    plan.confined_command(crate::process::ProcessLimits {
        wall: std::time::Duration::from_secs(1),
        stdout_max: 1024,
        stderr_max: 1024,
    })
    .expect("should accept reviewed backend controls");
}

#[test]
fn pure_completion_fixture_cannot_export_a_source_owned_recipe_definition() {
    let bytes = wasm_with(&[(".debug_info", b"i"), (".debug_line", b"l")]);
    let mut build = built("trusted-server-fastly.wasm", Role::Canonical, &bytes);
    let stdout = build.stdout(|_| {});
    let completed = build
        .finish(stdout)
        .expect("should complete the accepted pure fixture");
    assert!(
        completed.recipe_definition().is_err(),
        "should require the actual approved source binding to export a shared recipe definition"
    );
}

#[test]
fn recipe_binding_keeps_checked_module_facts_and_explicit_fixture_source() {
    let fixture = Fixture::new(COMMIT);
    let prepared = fixture
        .bind(FIXED_ARTIFACTS[0].name, Role::Canonical)
        .expect("should bind pure recipe fixture");
    assert!(
        matches!(prepared.workspace, WorkspaceSelection::Fixture),
        "should label the source authority bypass as test-only"
    );
    assert_eq!(
        prepared.js.modules.len(),
        1,
        "should bind the exact prepared JS module facts"
    );
    assert_eq!(
        prepared.js.modules[0].id(),
        "core",
        "should derive module identity from the owned handoff"
    );
}

#[test]
fn composition_queries_keep_selected_tools_environment_and_source_replacement() {
    for artifact in FIXED_ARTIFACTS {
        let fixture = Fixture::new(COMMIT);
        let prepared = fixture
            .bind(artifact.name, Role::Canonical)
            .expect("should bind the selected recipe");
        let plan = prepared
            .plan(&fixture.paths(Role::Canonical))
            .expect("should derive the selected plan");
        for kind in [CargoQuery::Metadata, CargoQuery::Tree] {
            let query = plan
                .observation_query(kind)
                .expect("should derive the bounded query");
            assert_eq!(query.program, plan.program, "should use the selected Cargo");
            assert_eq!(
                query.environment, plan.environment,
                "should retain the sanitized build environment"
            );
            assert_eq!(
                query.working_directory, plan.working_directory,
                "should retain the source root"
            );
            assert_eq!(query.umask, plan.umask, "should retain the selected umask");
            let configs = |arguments: &[String]| {
                arguments
                    .windows(2)
                    .filter(|pair| pair[0] == "--config")
                    .map(|pair| pair[1].clone())
                    .collect::<Vec<_>>()
            };
            assert_eq!(
                configs(&query.arguments),
                configs(&plan.arguments),
                "should retain every source/tool/remap config"
            );
            assert!(
                query.arguments.iter().any(|value| value == "--frozen"),
                "should prohibit new resolution"
            );
            match kind {
                CargoQuery::Metadata => assert!(
                    !query
                        .arguments
                        .iter()
                        .any(|value| value == "--filter-platform" || value == "--target"),
                    "should retain host-only package identities in the metadata catalog"
                ),
                CargoQuery::Tree => assert!(
                    query
                        .arguments
                        .windows(2)
                        .any(|pair| pair == ["--target", artifact.target.triple()]),
                    "should select the exact target for graph observations"
                ),
            }
            assert_ne!(
                query.digest().expect("should hash the query"),
                plan.digest().expect("should hash the build")
            );
        }
    }
}

/// The exact target path remap for the fixture's mount layout, with the
/// Darwin debug-map prefix and target linker for a Darwin target.
fn expected_remap(target: ReleaseTarget) -> Vec<String> {
    let triple = target.triple();
    let mut flags = vec![
        "\"--remap-path-prefix=/boundary/immutable/0=/trusted-server\"",
        "\"--remap-path-prefix=/boundary/immutable/2/vendor=/cargo-vendor\"",
        "\"--remap-path-prefix=/boundary/writable/0/cargo-home=/cargo-home\"",
        "\"--remap-path-prefix=/boundary/tool/0=/rust-tools\"",
        "\"--remap-path-prefix=/boundary/writable/1=/cargo-target\"",
    ];
    if target.is_darwin() {
        flags.push("\"-Clink-arg=-Wl,-oso_prefix,/boundary/tool/\"");
    }
    let mut arguments = vec![
        "--config".to_owned(),
        format!("target.\"{triple}\".rustflags=[{}]", flags.join(",")),
    ];
    if target.is_darwin() {
        arguments.push("--config".to_owned());
        arguments.push(format!("target.\"{triple}\".linker=\"{FIXTURE_CLANG}\""));
    }
    if target == ReleaseTarget::X86_64UnknownLinuxGnu {
        arguments.extend([
            "--config".to_owned(),
            "target.\"x86_64-unknown-linux-gnu\".linker=\"/usr/bin/gcc\"".to_owned(),
        ]);
    }
    if target == ReleaseTarget::Wasm32Wasip1
        && cfg!(all(target_os = "linux", target_arch = "x86_64"))
    {
        arguments.extend([
            "--config".to_owned(),
            "target.\"x86_64-unknown-linux-gnu\".linker=\"/usr/bin/cc\"".to_owned(),
            "--config".to_owned(),
            "target.\"wasm32-wasip1\".linker=\"/boundary/tool/0/lib/rustlib/x86_64-unknown-linux-gnu/bin/rust-lld\"".to_owned(),
        ]);
    }
    arguments
}

/// The exact C path remap a native asset's plan sets for the fixture layout.
fn expected_c_prefix_map(target: ReleaseTarget) -> String {
    let developer = [("/boundary/system/0", "/apple-developer")];
    [
        ("/boundary/immutable/0", "/trusted-server"),
        ("/boundary/immutable/2/vendor", "/cargo-vendor"),
        ("/boundary/writable/0/cargo-home", "/cargo-home"),
        ("/boundary/tool/0", "/rust-tools"),
        ("/boundary/writable/1", "/cargo-target"),
    ]
    .iter()
    .chain(developer.iter().filter(|_| target.is_darwin()))
    .flat_map(|(from, to)| {
        [
            format!("-ffile-prefix-map={from}={to}"),
            format!("-fdebug-prefix-map={from}={to}"),
        ]
    })
    .collect::<Vec<_>>()
    .join(" ")
}

#[test]
fn cargo_plan_has_fixed_release_frozen_json_flags_and_exact_source_replacement() {
    assert!(
        !FIXED_ARTIFACTS.is_empty(),
        "should select a nonzero number of assets"
    );
    for (index, artifact) in FIXED_ARTIFACTS.iter().enumerate() {
        let role = if index % 2 == 0 {
            Role::Canonical
        } else {
            Role::Reproduction
        };
        let jobs = if role == Role::Canonical { "4" } else { "2" };
        let fixture = Fixture::new(COMMIT);
        let prepared = fixture
            .bind(artifact.name, role)
            .expect("should bind every fixed asset");
        let plan = prepared
            .plan(&fixture.paths(role))
            .expect("should derive the plan");
        let mut expected_arguments: Vec<String> = [
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
        .collect();
        expected_arguments.extend(expected_replacement());
        expected_arguments.extend(expected_remap(artifact.target));
        expected_arguments.extend(
            ["--message-format=json-render-diagnostics", "--jobs", jobs]
                .into_iter()
                .map(str::to_owned),
        );
        assert_eq!(
            plan.arguments, expected_arguments,
            "should use the exact fixed arguments for {}",
            artifact.name
        );
        assert_eq!(
            plan.program,
            fixture.base.join("tools/bin/cargo"),
            "should run the selected Cargo by its host path"
        );
        let checkout = if role == Role::Canonical {
            "checkout-canonical"
        } else {
            "checkout-reproduction"
        };
        assert_eq!(
            plan.working_directory,
            fixture.base.join(checkout),
            "should run in the role checkout by its host path"
        );
        assert_environment(&plan.environment, &fixture, artifact.target);
        prepared
            .plan(&fixture.paths(role))
            .expect("should derive the plan again");
    }
}

fn assert_environment(actual: &BTreeMap<String, String>, fixture: &Fixture, target: ReleaseTarget) {
    let mut expected: BTreeMap<String, String> = [
        ("AWS_LC_SYS_USE_SYSTEM", "0"),
        ("CARGO_HOME", "/boundary/writable/0/cargo-home"),
        ("CARGO_TARGET_DIR", "/boundary/writable/1"),
        ("HOME", "/boundary/writable/0/home"),
        ("LC_ALL", "C"),
        ("LIBSQLITE3_SYS_USE_PKG_CONFIG", "0"),
        ("PATH", "/boundary/tool/0/bin:/usr/bin:/bin"),
        ("RUSTC", "/boundary/tool/0/bin/rustc"),
        ("SOURCE_DATE_EPOCH", "1700000000"),
        ("TMPDIR", "/boundary/writable/0/tmp"),
        ("TRUSTED_SERVER__GIT_VERSION", COMMIT),
        ("TRUSTED_SERVER__RELEASE_BUILD", "1"),
        (
            "TRUSTED_SERVER__TSJS_PREBUILT_INVENTORY",
            "/boundary/immutable/1/tsjs-inventory.json",
        ),
        ("TSJS_PREBUILT_DIR", "/boundary/immutable/1/bundles"),
        ("TZ", "UTC"),
    ]
    .into_iter()
    .map(|(name, value)| (name.to_owned(), value.to_owned()))
    .collect();
    expected.insert(
        format!(
            "AWS_LC_SYS_USE_SYSTEM_{}",
            target.triple().replace('-', "_")
        ),
        "0".to_owned(),
    );
    let underscored = target.triple().replace('-', "_");
    if target == ReleaseTarget::X86_64UnknownLinuxGnu {
        for (name, value) in [
            ("CC_x86_64_unknown_linux_gnu", "/usr/bin/gcc"),
            ("AR_x86_64_unknown_linux_gnu", "/usr/bin/ar"),
            ("HOST_CC", "/usr/bin/gcc"),
            ("HOST_AR", "/usr/bin/ar"),
            ("AWS_LC_SYS_CMAKE_BUILDER", "0"),
            ("AWS_LC_SYS_CMAKE_BUILDER_x86_64_unknown_linux_gnu", "0"),
        ] {
            expected.insert(name.to_owned(), value.to_owned());
        }
    }
    if target == ReleaseTarget::Wasm32Wasip1
        && cfg!(all(target_os = "linux", target_arch = "x86_64"))
    {
        expected.insert("HOST_CC".to_owned(), "/usr/bin/cc".to_owned());
        expected.insert("HOST_AR".to_owned(), "/usr/bin/ar".to_owned());
    }
    if target.is_darwin() {
        let bin = "/boundary/system/0/Toolchains/XcodeDefault.xctoolchain/usr/bin";
        for (name, value) in [
            ("MACOSX_DEPLOYMENT_TARGET".to_owned(), "13.0".to_owned()),
            ("DEVELOPER_DIR".to_owned(), "/boundary/system/0".to_owned()),
            (
                "SDKROOT".to_owned(),
                format!("/boundary/system/0/{SDK_ROOT}"),
            ),
            (format!("CC_{underscored}"), FIXTURE_CLANG.to_owned()),
            (format!("AR_{underscored}"), format!("{bin}/ar")),
            ("ZERO_AR_DATE".to_owned(), "1".to_owned()),
            ("OPENSSL_CONF".to_owned(), "/dev/null".to_owned()),
        ] {
            expected.insert(name, value);
        }
    }
    if target != ReleaseTarget::Wasm32Wasip1 {
        expected.insert(
            format!("CFLAGS_{underscored}"),
            expected_c_prefix_map(target),
        );
    }
    assert_eq!(
        actual, &expected,
        "should hold exactly the child environment with every path in the execution namespace"
    );
    for (name, value) in actual {
        assert!(
            !value.contains(fixture.base.to_str().expect("should be UTF-8")),
            "should not leak a host path into {name}"
        );
    }
}

/// One mapped location per path-carrying variable, so each can be omitted alone.
fn granular(fixture: &Fixture, omit: &[&str]) -> PrefixPaths {
    let retained = fixture.retained();
    let all: [(&str, PathBuf, &str, bool); 12] = [
        ("developer", fixture.developer_dir(), "/c/developer", false),
        ("target", fixture.base.join("target"), "/c/target", true),
        (
            "cargo-home",
            fixture.base.join("cargo-state/cargo-home"),
            "/c/cargo-home",
            true,
        ),
        (
            "home",
            fixture.base.join("cargo-state/home"),
            "/c/home",
            true,
        ),
        ("tmp", fixture.base.join("cargo-state/tmp"), "/c/tmp", true),
        ("bundles", retained.join("bundles"), "/c/bundles", false),
        (
            "inventory",
            retained.join("tsjs-inventory.json"),
            "/c/inventory.json",
            false,
        ),
        (
            "rustc",
            fixture.base.join("tools/bin/rustc"),
            "/c/rustc",
            false,
        ),
        ("bin", fixture.base.join("tools/bin"), "/c/bin", false),
        (
            "vendor",
            fixture.base.join("materials/vendor"),
            "/c/vendor",
            false,
        ),
        (
            "checkout",
            fixture.base.join("checkout-canonical"),
            "/c/checkout",
            false,
        ),
        ("tools", fixture.base.join("tools"), "/c/tools", false),
    ];
    all.iter().filter(|(name, ..)| !omit.contains(name)).fold(
        PrefixPaths::new(),
        |paths, (_, host, execution, writable)| paths.with(host, execution, *writable),
    )
}

#[test]
fn path_variables_outside_every_root_are_rejected() {
    let fixture = Fixture::new(COMMIT);
    let prepared = fixture
        .bind("ts-aarch64-apple-darwin", Role::Canonical)
        .expect("should bind the recipe");
    prepared
        .plan(&granular(&fixture, &[]))
        .expect("should derive the plan when every location is mapped");
    let cases: [(&str, &[&str]); 11] = [
        ("Apple developer directory", &["developer"]),
        ("CARGO_TARGET_DIR", &["target"]),
        ("CARGO_HOME", &["cargo-home"]),
        ("HOME", &["home"]),
        ("TMPDIR", &["tmp"]),
        ("TSJS_PREBUILT_DIR", &["bundles"]),
        ("TRUSTED_SERVER__TSJS_PREBUILT_INVENTORY", &["inventory"]),
        ("RUSTC", &["rustc", "bin", "tools"]),
        ("vendor directory", &["vendor"]),
        ("checkout root", &["checkout"]),
        ("Rust tools root", &["tools"]),
    ];
    for (label, omit) in cases {
        let message = plan_error(&prepared, &granular(&fixture, omit));
        assert!(
            message.contains(label) && message.contains("outside the boundary roots"),
            "should reject {label} outside the roots: {message}"
        );
    }
    let message = plan_error(&prepared, &granular(&fixture, &["bin", "tools"]));
    assert!(
        message.contains("PATH") && message.contains("outside the boundary roots"),
        "should reject the tool directory in PATH outside the roots: {message}"
    );
}

#[test]
fn child_home_and_cargo_home_must_be_the_request_state() {
    let fixture = Fixture::new(COMMIT);
    let request = fixture.request(
        "ts-aarch64-apple-darwin",
        Role::Canonical.spec_label(),
        COMMIT,
    );
    let commit = CommitId::parse(COMMIT).expect("should parse commit");
    let mut forged = child_request(&request);
    forged.home = fixture.base.join("other-work/home");
    let child =
        crate::preflight::child::build_child_for_tests(&audited(), &commit, 1_700_000_000, &forged)
            .expect("should build a child with another HOME");
    let error = PreparedRecipe::bind(BindInputs {
        workspace: WorkspaceSelection::Fixture,
        commit: &commit,
        child,
        completed: &fixture.completed,
        tools: fixture.tools_for(ReleaseTarget::Aarch64AppleDarwin),
        materials: fixture.materials(COMMIT),
        ambient: fixture.ambient(Role::Canonical),
        request,
    });
    assert!(
        bind_error(error).contains("HOME is not the request's private cargo state"),
        "should reject a child HOME outside the request state"
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
    let prepared = fixture
        .bind("ts-x86_64-unknown-linux-gnu", Role::Canonical)
        .expect("should bind from a benign environment");
    let plan = prepared
        .plan(&fixture.paths(Role::Canonical))
        .expect("should derive the plan");
    for name in plan.environment.keys() {
        assert!(
            name == "RUSTC" || (!name.starts_with("RUST") && !name.starts_with("CARGO_BUILD")),
            "should carry no selector besides the explicit RUSTC: {name}"
        );
    }
    let configs: Vec<&String> = plan
        .arguments
        .iter()
        .enumerate()
        .filter(|(_, argument)| *argument == "--config")
        .filter_map(|(index, _)| plan.arguments.get(index + 1))
        .collect();
    assert_eq!(
        configs.len(),
        expected_replacement().len() / 2 + 2,
        "should carry the exact source replacements, path remap and reviewed GNU linker"
    );
    assert_eq!(
        configs.last().map(|config| config.as_str()),
        expected_remap(ReleaseTarget::X86_64UnknownLinuxGnu)
            .last()
            .map(String::as_str),
        "should select the reviewed GNU linker after target path remapping"
    );
    for argument in &plan.arguments {
        assert!(
            ![
                "--features",
                "--all-features",
                "--no-default-features",
                "--manifest-path",
                "--config=",
            ]
            .iter()
            .any(|selector| argument.starts_with(selector))
                && !argument.starts_with('+'),
            "should not carry selector argument {argument}"
        );
    }
    assert!(
        plan.arguments
            .iter()
            .any(|argument| argument == "--release")
            && plan.arguments.iter().any(|argument| argument == "--frozen"),
        "should keep the release and frozen flags"
    );
}

#[test]
fn plan_digest_binds_every_part_of_the_plan() {
    let fixture = Fixture::new(COMMIT);
    let prepared = fixture
        .bind("ts-aarch64-apple-darwin", Role::Canonical)
        .expect("should bind the recipe");
    let paths = fixture.paths(Role::Canonical);
    let plan = prepared.plan(&paths).expect("should derive the plan");
    let digest = plan.digest().expect("should digest the plan");
    assert_eq!(digest.len(), 64, "should be a SHA-256 hex digest");
    assert_eq!(
        digest,
        prepared
            .plan(&paths)
            .expect("should derive the plan again")
            .digest()
            .expect("should digest again"),
        "should be a pure function of the bound inputs"
    );
    type Edit = fn(&mut CargoPlan);
    let edits: [(&str, Edit); 6] = [
        ("argument", |p| p.arguments.push("--offline".to_owned())),
        ("dropped argument", |p| {
            p.arguments.retain(|argument| argument != "--release");
        }),
        ("environment value", |p| {
            p.environment
                .insert("RUSTC".to_owned(), "/other/rustc".to_owned());
        }),
        ("injected variable", |p| {
            p.environment
                .insert("RUSTFLAGS".to_owned(), "--cfg x".to_owned());
        }),
        ("program", |p| p.program = PathBuf::from("/other/cargo")),
        ("umask", |p| p.umask = umask(0o000)),
    ];
    for (label, edit) in edits {
        let mut changed = prepared.plan(&paths).expect("should derive the plan");
        edit(&mut changed);
        assert_ne!(
            changed.digest().expect("should digest the edited plan"),
            digest,
            "should change the digest for an edited {label}"
        );
    }
    let mut moved = prepared.plan(&paths).expect("should derive the plan");
    moved.working_directory = fixture.base.join("checkout-reproduction");
    assert_ne!(
        moved.digest().expect("should digest the moved plan"),
        digest,
        "should change the digest for another working directory"
    );
}

#[test]
fn plan_carries_the_role_umask() {
    for (role, expected) in [(Role::Canonical, 0o022), (Role::Reproduction, 0o027)] {
        let fixture = Fixture::new(COMMIT);
        let prepared = fixture
            .bind("ts-aarch64-apple-darwin", role)
            .expect("should bind the recipe");
        assert_eq!(
            prepared
                .plan(&fixture.paths(role))
                .expect("should derive the plan")
                .umask,
            umask(expected),
            "should derive the umask of role {role:?} from its ambient profile"
        );
    }
}

#[test]
fn confined_command_applies_the_role_umask_and_keeps_host_program_paths() {
    for (role, expected) in [(Role::Canonical, "022"), (Role::Reproduction, "027")] {
        let fixture = Fixture::new(COMMIT);
        let prepared = fixture
            .bind("ts-aarch64-apple-darwin", role)
            .expect("should bind the recipe");
        let plan = prepared
            .plan(&fixture.paths(role))
            .expect("should derive the plan");
        let command = plan
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
            format!("{:?}", plan.program),
            format!("{:?}", plan.working_directory),
            format!(
                "{:?}",
                plan.arguments
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

#[cfg(target_os = "macos")]
#[test]
fn real_boundary_translates_every_plan_path() {
    use crate::boundary::Boundary;
    let fixture = Fixture::new(COMMIT);
    let prepared = fixture
        .bind("ts-aarch64-apple-darwin", Role::Canonical)
        .expect("should bind the recipe");
    let roots = prepared.boundary_roots().expect("should derive roots");
    let boundary = Boundary::mac_sandbox_for_tests(roots).expect("should configure, not run");
    let plan = prepared
        .plan(&boundary)
        .expect("should derive the plan through the boundary");
    assert_eq!(
        plan.environment.get("CARGO_TARGET_DIR").map(String::as_str),
        fixture.base.join("target").to_str(),
        "should map writable roots to themselves on macOS"
    );
    assert!(
        plan.environment["PATH"].starts_with(
            fixture
                .base
                .join("tools/bin")
                .to_str()
                .expect("should be UTF-8")
        ),
        "should put the selected tool directory first in PATH"
    );
}

#[test]
fn execution_paths_with_control_characters_or_separators_are_rejected() {
    let fixture = Fixture::new(COMMIT);
    let prepared = fixture
        .bind("ts-aarch64-apple-darwin", Role::Canonical)
        .expect("should bind the recipe");
    for bad in [
        "/c/bin\nx",
        "/c/bin\tx",
        "/c/bin\u{7f}x",
        "/c/bi:n",
        "/c/bi\"n",
        "/c/bi\\n",
    ] {
        let paths = PrefixPaths::new()
            .with(&fixture.base.join("tools"), bad, false)
            .with(&fixture.retained(), "/c/retained", false)
            .with(&fixture.base, "/c/base", true);
        let message = plan_error(&prepared, &paths);
        assert!(
            message.contains("not plain absolute UTF-8"),
            "should reject execution path {bad:?}: {message}"
        );
    }
}

#[test]
fn fake_host_path_rejects_non_normal_components_like_the_real_inverse() {
    let paths = PrefixPaths::new().with(std::path::Path::new("/host/target"), "/c/target", true);
    assert!(
        paths
            .host_path(std::path::Path::new("/c/target/release/ts"))
            .is_ok()
    );
    for bad in [
        "/c/target/../target/ts",
        "/c/target/./ts",
        "/c/target//ts",
        "/c/target/ts/",
    ] {
        assert!(
            paths.host_path(std::path::Path::new(bad)).is_err(),
            "should reject {bad}"
        );
    }
}
