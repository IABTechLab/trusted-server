use std::os::unix::ffi::OsStringExt as _;

use std::collections::BTreeMap;

use super::child::{ChildRequest, ToolDirectory, build_child};
use super::*;
use crate::policy::ReleaseTarget;

const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

fn commit() -> CommitId {
    CommitId::parse(COMMIT).expect("should parse fictional commit")
}

fn snapshot(entries: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
    entries
        .iter()
        .map(|(name, value)| (OsString::from(name), OsString::from(value)))
        .collect()
}

fn rejection(entries: &[(&str, &str)]) -> String {
    let report = audit_environment(&snapshot(entries))
        .err()
        .expect("should reject inherited environment");
    format!("{report:?}")
}

fn request(target: ReleaseTarget) -> ChildRequest {
    ChildRequest {
        target,
        home: PathBuf::from("/private/release/home"),
        cargo_home: PathBuf::from("/private/release/cargo-home"),
        tool_directories: vec![
            ToolDirectory(PathBuf::from("/private/release/tools/rust/bin")),
            ToolDirectory(PathBuf::from("/private/release/tools/node/bin")),
        ],
    }
}

#[test]
fn accepts_benign_environment_and_keeps_search_path() {
    let audited = audit_environment(&snapshot(&[
        ("PATH", "/usr/bin:/bin"),
        ("HOME", "/home/fictional"),
        ("LANG", "en_US.UTF-8"),
        ("TERM", "xterm"),
    ]))
    .expect("should accept benign variables");
    assert_eq!(audited.search_path(), Some("/usr/bin:/bin"));
    assert!(audited.removed_names().is_empty(), "should remove nothing");
}

#[test]
fn rejects_tsjs_build_controls_with_empty_value() {
    for name in ["TSJS_PREBUILT_DIR", "TSJS_SKIP_BUILD", "TSJS_TEST"] {
        let text = rejection(&[(name, "")]);
        assert!(
            text.contains(name),
            "should name the inherited build control"
        );
    }
}

#[test]
fn rejects_tsjs_build_controls_with_zero_value() {
    for name in ["TSJS_PREBUILT_DIR", "TSJS_SKIP_BUILD", "TSJS_TEST"] {
        let text = rejection(&[(name, "0")]);
        assert!(
            text.contains(name),
            "should name the inherited build control"
        );
    }
}

#[test]
fn rejects_tsjs_build_controls_with_one_value() {
    for name in ["TSJS_PREBUILT_DIR", "TSJS_SKIP_BUILD", "TSJS_TEST"] {
        let text = rejection(&[(name, "1")]);
        assert!(
            text.contains(name),
            "should name the inherited build control"
        );
    }
}

fn assert_each_rejected(class: &str, names: &[&str]) {
    for name in names {
        let text = rejection(&[("PATH", "/usr/bin"), (name, "fictional-value")]);
        assert!(
            text.contains(name),
            "should reject {class} variable {name}: {text}"
        );
        assert!(
            !text.contains("fictional-value"),
            "should never print {class} values"
        );
    }
}

#[test]
fn rejects_compiler_wrappers() {
    assert_each_rejected(
        "wrapper",
        &[
            "RUSTC_WRAPPER",
            "RUSTC_WORKSPACE_WRAPPER",
            "CARGO_BUILD_RUSTC_WRAPPER",
            "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER",
            "RUSTC",
            "RUSTDOC",
        ],
    );
}

#[test]
fn rejects_compiler_and_profile_flags() {
    assert_each_rejected(
        "flag",
        &[
            "RUSTFLAGS",
            "RUSTDOCFLAGS",
            "CARGO_ENCODED_RUSTFLAGS",
            "CARGO_ENCODED_RUSTDOCFLAGS",
            "CARGO_BUILD_RUSTFLAGS",
            "CARGO_TARGET_WASM32_WASIP1_LINKER",
            "CARGO_TARGET_DIR",
            "CARGO_PROFILE_RELEASE_DEBUG",
        ],
    );
}

#[test]
fn rejects_cargo_and_rustup_configuration_layers() {
    assert_each_rejected(
        "cargo-config",
        &[
            "CARGO",
            "CARGO_HOME",
            "CARGO_NET_OFFLINE",
            "CARGO_MANIFEST_DIR",
            "RUSTUP_HOME",
            "RUSTUP_TOOLCHAIN",
        ],
    );
}

#[test]
fn rejects_node_npm_and_vite_overrides() {
    assert_each_rejected(
        "node",
        &[
            "NODE_OPTIONS",
            "NODE_PATH",
            "npm_config_registry",
            "NPM_CONFIG_CACHE",
            "Npm_Config_Ignore_Scripts",
            "VITE_MODE",
        ],
    );
}

#[test]
fn rejects_native_build_selectors_and_target_variants() {
    assert_each_rejected(
        "native",
        &[
            "AWS_LC_SYS_USE_SYSTEM",
            "AWS_LC_SYS_CMAKE_BUILDER",
            "AWS_LC_SYS_PREBUILT_NASM_x86_64_unknown_linux_gnu",
            "AWS_LC_FIPS_SYS_STATIC",
            "LIBSQLITE3_SYS_USE_PKG_CONFIG",
            "SQLITE3_LIB_DIR",
            "CC",
            "CXX",
            "AR",
            "LD",
            "CFLAGS",
            "CXXFLAGS",
            "LDFLAGS",
            "CPPFLAGS",
            "CC_x86_64_unknown_linux_gnu",
            "CFLAGS_aarch64-apple-darwin",
            "TARGET_CC",
            "HOST_CFLAGS",
            "PKG_CONFIG",
            "PKG_CONFIG_PATH",
            "BINDGEN_EXTRA_CLANG_ARGS",
            "SDKROOT",
            "DEVELOPER_DIR",
            "MACOSX_DEPLOYMENT_TARGET",
            "VCPKG_INSTALLATION_ROOT",
            "VCPKG_ROOT",
        ],
    );
}

#[test]
fn rejects_sqlite_selectors() {
    assert_each_rejected(
        "sqlite",
        &[
            "LIBSQLITE3_FLAGS",
            "SQLITE_MAX_VARIABLE_NUMBER",
            "SQLCIPHER_LIB_DIR",
        ],
    );
}

#[test]
fn rejects_ring_and_openssl_selectors() {
    assert_each_rejected(
        "crypto",
        &[
            "RING_PREGENERATE_ASM",
            "OPENSSL_DIR",
            "OPENSSL_STATIC",
            "OPENSSL_CONF",
            "OPENSSL_CONF_INCLUDE",
        ],
    );
}

#[test]
fn rejects_cmake_selectors() {
    assert_each_rejected(
        "cmake",
        &["CMAKE", "CMAKE_TOOLCHAIN_FILE", "CMAKE_GENERATOR"],
    );
}

#[test]
fn rejects_vcpkg_rs_selectors() {
    assert_each_rejected("vcpkg", &["VCPKGRS_DYNAMIC", "VCPKGRS_TRIPLET"]);
}

#[test]
fn rejects_cc_crate_controls() {
    assert_each_rejected(
        "cc-crate",
        &[
            "CRATE_CC_NO_DEFAULTS",
            "CC_ENABLE_DEBUG_OUTPUT",
            "CC_FORCE_DISABLE",
            "CC_SHELL_ESCAPED_FLAGS",
        ],
    );
}

#[test]
fn rejects_lowercase_target_suffixed_native_flags() {
    assert_each_rejected(
        "target-suffixed",
        &[
            "CFLAGS_aarch64_apple_darwin",
            "CXXFLAGS_x86_64_unknown_linux_gnu",
            "AWS_LC_SYS_USE_SYSTEM_aarch64_apple_darwin",
            "AWS_LC_SYS_STATIC_x86_64_apple_darwin",
        ],
    );
}

#[test]
fn rejects_loader_injection() {
    assert_each_rejected(
        "loader",
        &[
            "LD_PRELOAD",
            "LD_LIBRARY_PATH",
            "LD_AUDIT",
            "DYLD_INSERT_LIBRARIES",
            "DYLD_LIBRARY_PATH",
        ],
    );
}

#[test]
fn rejects_git_channels_and_source_date_epoch() {
    assert_each_rejected(
        "git",
        &[
            "GIT_DIR",
            "GIT_CONFIG_PARAMETERS",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            "SOURCE_DATE_EPOCH",
        ],
    );
}

#[test]
fn rejects_credentials_and_signing_material() {
    assert_each_rejected(
        "credential",
        &[
            "GH_TOKEN",
            "GITHUB_TOKEN",
            "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
            "ACTIONS_ID_TOKEN_REQUEST_URL",
            "ACTIONS_RUNTIME_TOKEN",
            "FASTLY_API_TOKEN",
            "AWS_ACCESS_KEY_ID",
            "AWS_SECRET_ACCESS_KEY",
            "AWS_SESSION_TOKEN",
            "SIGSTORE_ID_TOKEN",
        ],
    );
}

#[test]
fn rejects_unreviewed_setup_exports_even_when_empty() {
    assert!(
        REVIEWED_SETUP_EXCEPTIONS.is_empty(),
        "should have no reviewed hosted setup values yet"
    );
    for (name, value) in [
        ("RUSTFLAGS", ""),
        ("VCPKG_INSTALLATION_ROOT", "/usr/local/share/vcpkg"),
    ] {
        let text = rejection(&[(name, value)]);
        assert!(text.contains(name), "should reject unreviewed {name}");
    }
}

#[test]
fn rejects_non_utf8_names_and_values() {
    let bad_value = vec![(
        OsString::from("FICTIONAL_SETTING"),
        OsString::from_vec(vec![0x66, 0xff, 0x6f]),
    )];
    let text = format!(
        "{:?}",
        audit_environment(&bad_value)
            .err()
            .expect("should reject non-UTF-8 value")
    );
    assert!(text.contains("FICTIONAL_SETTING"), "should name variable");
    let bad_name = vec![(
        OsString::from_vec(vec![0x46, 0xff, 0x4f]),
        OsString::from("value"),
    )];
    assert!(
        audit_environment(&bad_name).is_err(),
        "should reject non-UTF-8 name"
    );
}

#[test]
fn rejects_release_build_flag_other_than_one() {
    for value in ["0", "", "true", "11"] {
        let text = rejection(&[("TRUSTED_SERVER__RELEASE_BUILD", value)]);
        assert!(
            text.contains("TRUSTED_SERVER__RELEASE_BUILD"),
            "should reject release flag value"
        );
    }
}

#[test]
fn removes_operator_prefixes_by_full_prefix() {
    let audited = audit_environment(&snapshot(&[
        ("TRUSTED_SERVER__RELEASE_BUILD", "1"),
        ("TRUSTED_SERVER__PUBLISHER__DOMAIN", "publisher.example.com"),
        ("EDGEZERO__STORES__CONFIG__ID", "fictional"),
        ("MY_TRUSTED_SERVER__SETTING", "kept-out"),
        ("TRUSTED_SERVERX", "kept-out"),
    ]))
    .expect("should accept removable operator variables");
    assert_eq!(
        audited.removed_names(),
        [
            "EDGEZERO__STORES__CONFIG__ID",
            "TRUSTED_SERVER__PUBLISHER__DOMAIN",
            "TRUSTED_SERVER__RELEASE_BUILD",
        ],
        "should record exactly the case-exact prefixed names"
    );
}

#[test]
fn reports_every_rejected_name_without_secret_values() {
    let text = rejection(&[
        ("GH_TOKEN", "ghp_fictionalsecretvalue"),
        ("AWS_SECRET_ACCESS_KEY", "fictional/secret/access/key"),
        ("RUSTFLAGS", "-C link-arg=fictional-secret-flag"),
        ("TSJS_SKIP_BUILD", "1"),
    ]);
    for name in [
        "GH_TOKEN",
        "AWS_SECRET_ACCESS_KEY",
        "RUSTFLAGS",
        "TSJS_SKIP_BUILD",
    ] {
        assert!(text.contains(name), "should name {name}");
    }
    for secret in [
        "ghp_fictionalsecretvalue",
        "fictional/secret/access/key",
        "fictional-secret-flag",
    ] {
        assert!(!text.contains(secret), "should never print variable values");
    }
}

#[test]
fn child_environment_contains_exactly_expected_linux_keys() {
    let audited = audit_environment(&snapshot(&[
        ("PATH", "/usr/bin"),
        ("TRUSTED_SERVER__PROXY__SECRET", "fictional"),
        ("LANG", "en_US.UTF-8"),
    ]))
    .expect("should audit benign environment");
    let child = build_child(
        &audited,
        &commit(),
        1_700_000_000,
        &request(ReleaseTarget::X86_64UnknownLinuxGnu),
    )
    .expect("should build child environment");
    let expected = BTreeMap::from([
        ("AWS_LC_SYS_USE_SYSTEM", "0"),
        ("AWS_LC_SYS_USE_SYSTEM_x86_64_unknown_linux_gnu", "0"),
        ("CARGO_HOME", "/private/release/cargo-home"),
        ("HOME", "/private/release/home"),
        ("LC_ALL", "C"),
        ("LIBSQLITE3_SYS_USE_PKG_CONFIG", "0"),
        (
            "PATH",
            "/private/release/tools/rust/bin:/private/release/tools/node/bin",
        ),
        ("SOURCE_DATE_EPOCH", "1700000000"),
        ("TRUSTED_SERVER__GIT_VERSION", COMMIT),
        ("TRUSTED_SERVER__RELEASE_BUILD", "1"),
        ("TZ", "UTC"),
    ]);
    let actual: BTreeMap<&str, &str> = child
        .variables()
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    assert_eq!(
        actual, expected,
        "should construct the allowlist from scratch"
    );
    assert_eq!(
        child.injected_names(),
        expected.keys().copied().collect::<Vec<_>>(),
        "should record injected names as input facts"
    );
    assert_eq!(
        child.removed_names(),
        ["TRUSTED_SERVER__PROXY__SECRET"],
        "should record removed operator names"
    );
}

#[test]
fn child_environment_sets_macos_floor_only_for_darwin() {
    let audited = audit_environment(&[]).expect("should audit empty snapshot");
    for target in [
        ReleaseTarget::Aarch64AppleDarwin,
        ReleaseTarget::X86_64AppleDarwin,
    ] {
        let child = build_child(&audited, &commit(), 1, &request(target))
            .expect("should build darwin child");
        assert_eq!(
            child
                .variables()
                .get("MACOSX_DEPLOYMENT_TARGET")
                .map(String::as_str),
            Some("13.0"),
            "should set loader floor for {target:?}"
        );
    }
    for target in [
        ReleaseTarget::Wasm32Wasip1,
        ReleaseTarget::X86_64UnknownLinuxGnu,
    ] {
        let child = build_child(&audited, &commit(), 1, &request(target))
            .expect("should build non-darwin child");
        assert!(
            !child.variables().contains_key("MACOSX_DEPLOYMENT_TARGET"),
            "should not set macOS floor for {target:?}"
        );
    }
}

#[test]
fn child_environment_rejects_unsafe_paths_and_commits() {
    let audited = audit_environment(&[]).expect("should audit empty snapshot");
    let mut relative = request(ReleaseTarget::Wasm32Wasip1);
    relative.home = PathBuf::from("home");
    assert!(
        build_child(&audited, &commit(), 1, &relative).is_err(),
        "should reject relative HOME"
    );
    let mut colon = request(ReleaseTarget::Wasm32Wasip1);
    colon.tool_directories = vec![ToolDirectory(PathBuf::from("/tools:/usr/bin"))];
    assert!(
        build_child(&audited, &commit(), 1, &colon).is_err(),
        "should reject PATH separator inside a tool directory"
    );
    for malformed in ["ABC", "--help", &COMMIT.to_uppercase(), &COMMIT[..39]] {
        assert!(
            CommitId::parse(malformed).is_err(),
            "should reject malformed commit {malformed}"
        );
    }
}

#[test]
fn child_environment_sets_target_suffixed_aws_lc_selector() {
    let audited = audit_environment(&[]).expect("should audit empty snapshot");
    for (target, name) in [
        (
            ReleaseTarget::Wasm32Wasip1,
            "AWS_LC_SYS_USE_SYSTEM_wasm32_wasip1",
        ),
        (
            ReleaseTarget::Aarch64AppleDarwin,
            "AWS_LC_SYS_USE_SYSTEM_aarch64_apple_darwin",
        ),
        (
            ReleaseTarget::X86_64AppleDarwin,
            "AWS_LC_SYS_USE_SYSTEM_x86_64_apple_darwin",
        ),
    ] {
        let child = build_child(&audited, &commit(), 1, &request(target))
            .expect("should build child environment");
        assert_eq!(
            child.variables().get(name).map(String::as_str),
            Some("0"),
            "should pin the target-suffixed selector for {target:?}"
        );
        assert_eq!(
            child
                .variables()
                .keys()
                .filter(|key| key.starts_with("AWS_LC_SYS_USE_SYSTEM"))
                .count(),
            2,
            "should set only the generic and selected-target selectors"
        );
    }
}

#[test]
fn rejects_ambiguous_case_operator_prefixes() {
    for name in [
        "trusted_server__lowercase",
        "Trusted_Server__Mixed",
        "edgezero__stores",
        "EdgeZero__Stores",
    ] {
        let text = rejection(&[(name, "fictional-value")]);
        assert!(text.contains(name), "should reject ambiguous casing {name}");
    }
}

#[test]
fn rejects_package_registry_and_cloud_credentials() {
    assert_each_rejected(
        "registry-credential",
        &[
            "NPM_TOKEN",
            "NODE_AUTH_TOKEN",
            "AWS_REGION",
            "AWS_PROFILE",
            "AWS_CONFIG_FILE",
            "SSH_AUTH_SOCK",
            "GH_ENTERPRISE_TOKEN",
            "GITHUB_ENTERPRISE_TOKEN",
            "CLOUDFLARE_API_TOKEN",
            "CLOUDFLARE_ACCOUNT_ID",
        ],
    );
}

#[test]
fn rejects_hosted_runner_service_endpoints() {
    assert_each_rejected(
        "runner-service",
        &[
            "ACTIONS_CACHE_URL",
            "ACTIONS_RESULTS_URL",
            "ACTIONS_RUNTIME_URL",
        ],
    );
}

#[test]
fn rejects_shell_startup_injection() {
    assert_each_rejected("shell", &["BASH_ENV", "ENV"]);
}

#[test]
fn rejects_native_toolchain_locators() {
    assert_each_rejected(
        "native-locator",
        &[
            "CXXSTDLIB",
            "CXXSTDLIB_x86_64_unknown_linux_gnu",
            "TARGET_CXXSTDLIB",
            "CROSS_COMPILE",
            "LIBCLANG_PATH",
            "CLANG_PATH",
            "LLVM_CONFIG_PATH",
            "PROTOC",
            "PROTOC_INCLUDE",
            "NASM",
        ],
    );
}

#[test]
fn rejects_node_trust_and_mode_overrides() {
    assert_each_rejected(
        "node-trust",
        &[
            "NODE_EXTRA_CA_CERTS",
            "NODE_TLS_REJECT_UNAUTHORIZED",
            "NODE_ENV",
        ],
    );
}

#[test]
fn reviewed_exceptions_require_a_path_identity() {
    let exception = ReviewedException {
        name: "VCPKG_INSTALLATION_ROOT",
        value: "/fictional/vcpkg",
        path_identity: "/fictional/vcpkg",
    };
    assert!(
        !exception.matches("VCPKG_INSTALLATION_ROOT", "/fictional/vcpkg"),
        "should not accept a value whose path identity cannot be resolved"
    );
    assert!(
        !exception.matches("VCPKG_ROOT", "/fictional/vcpkg"),
        "should not accept a different name"
    );
}

#[test]
fn rejected_exact_names_are_sorted_unique_and_not_covered_elsewhere() {
    assert!(
        REJECTED_EXACT.windows(2).all(|pair| pair[0] < pair[1]),
        "should keep exact names strictly sorted, which also rules out duplicates"
    );
    for name in REJECTED_EXACT {
        assert!(
            !REJECTED_PREFIXES
                .iter()
                .any(|prefix| name.starts_with(prefix)),
            "{name} is already covered by a rejected prefix"
        );
        assert!(
            !NATIVE_SELECTORS
                .iter()
                .any(|selector| is_native_variant(name, selector)),
            "{name} is already covered by a native selector"
        );
    }
}
