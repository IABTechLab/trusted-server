//! Command and environment validation.

use super::*;

/// Names that inject code or configuration; rejected even as secrets.
const INJECTION_NAMES: [&str; 54] = [
    "LD_PRELOAD",
    "LD_LIBRARY_PATH",
    "LD_AUDIT",
    "LD_ANYTHING",
    "DYLD_INSERT_LIBRARIES",
    "dyld_insert_libraries",
    "NODE_OPTIONS",
    "BASH_ENV",
    "ENV",
    "GCONV_PATH",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
    "GIT_CONFIG_KEY_0",
    "GIT_CONFIG_VALUE_0",
    "GIT_EXEC_PATH",
    "GIT_SSH_COMMAND",
    "GIT_SSH",
    "npm_config_registry",
    "NPM_CONFIG_REGISTRY",
    "PYTHONPATH",
    "PYTHONSTARTUP",
    "PERL5OPT",
    "PERL5LIB",
    "RUBYOPT",
    "RUBYLIB",
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_SYSTEM",
    "GIT_CONFIG",
    "GIT_ASKPASS",
    "SSH_ASKPASS",
    "GIT_PROXY_COMMAND",
    "GIT_EXTERNAL_DIFF",
    "GIT_EDITOR",
    "EDITOR",
    "VISUAL",
    "GIT_PAGER",
    "PAGER",
    "GIT_TEMPLATE_DIR",
    "RUSTC_WRAPPER",
    "RUSTC_WORKSPACE_WRAPPER",
    "CARGO_BUILD_RUSTC_WRAPPER",
    "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER",
    "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER",
    "CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER",
    "RUSTC",
    "RUSTDOC",
    "RUSTFLAGS",
    "CARGO_ENCODED_RUSTFLAGS",
    "CARGO_BUILD_RUSTC",
    "CARGO_BUILD_RUSTDOC",
    "CARGO_BUILD_RUSTFLAGS",
    "NODE_PATH",
    "JAVA_TOOL_OPTIONS",
    "_JAVA_OPTIONS",
];

/// Credential names accepted only through `secret_environment`.
const CREDENTIAL_NAMES: [&str; 12] = [
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "NPM_TOKEN",
    "NODE_AUTH_TOKEN",
    "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
    "ACTIONS_RUNTIME_TOKEN",
    "AWS_SECRET_ACCESS_KEY",
    "FASTLY_API_TOKEN",
    "github_token",
    "CARGO_REGISTRY_TOKEN",
    "CARGO_REGISTRIES_EXAMPLE_TOKEN",
    "SIGSTORE_ID_TOKEN",
];

#[test]
fn relative_program_is_rejected() {
    let dir = workspace();

    let report = BoundedCommand::new(Path::new("helper"), dir.path(), limits(SLACK))
        .expect_err("should reject a relative program path");

    assert!(
        matches!(failure(&report), Some(ProcessFailure::InvalidCommand)),
        "should classify an invalid command"
    );
}

#[test]
fn relative_working_directory_is_rejected() {
    let report = BoundedCommand::new(&test_binary(), Path::new("relative"), limits(SLACK))
        .expect_err("should reject a relative working directory");

    assert!(
        matches!(failure(&report), Some(ProcessFailure::InvalidCommand)),
        "should classify an invalid command"
    );
}

#[test]
fn environment_is_exactly_the_explicit_map() {
    let dir = workspace();
    let command = helper(&dir, "env", limits(SLACK))
        .environment("ALPHA", "1")
        .expect("should accept a plain variable")
        .environment("BETA", "two words")
        .expect("should accept a plain variable");

    let stdout = command
        .run()
        .expect("should run the env helper")
        .require_success()
        .expect("should exit zero");

    let text = String::from_utf8(payload(&stdout).to_vec()).expect("should print UTF-8");
    let names: Vec<&str> = text
        .lines()
        .filter_map(|line| line.split_once('=').map(|(name, _)| name))
        .collect();
    assert_eq!(
        names,
        vec!["ALPHA", "BETA", HELPER_SELECTOR, HELPER_NONCE, HELPER_PIDS],
        "should pass exactly the explicit environment"
    );
    assert!(
        text.contains("ALPHA=1\nBETA=two words\n"),
        "should pass explicit values: {text}"
    );
}

#[test]
fn injection_variables_are_rejected() {
    let dir = workspace();
    for name in INJECTION_NAMES {
        let plain = BoundedCommand::new(&test_binary(), dir.path(), limits(SLACK))
            .expect("should accept an absolute program")
            .environment(name, "/tmp/fixture.so")
            .expect_err("should reject an injection variable");
        assert!(
            matches!(failure(&plain), Some(ProcessFailure::InvalidCommand)),
            "should classify {name} as an invalid command"
        );
        assert!(
            !format!("{plain:?}").contains("/tmp/fixture.so"),
            "should not echo the rejected value"
        );
        let secret = BoundedCommand::new(&test_binary(), dir.path(), limits(SLACK))
            .expect("should accept an absolute program")
            .secret_environment(name, "/tmp/fixture.so");
        assert!(
            secret.is_err(),
            "should reject {name} as a secret variable too"
        );
    }
}

#[test]
fn credentials_require_secret_classification() {
    let dir = workspace();
    for name in CREDENTIAL_NAMES {
        let plain = BoundedCommand::new(&test_binary(), dir.path(), limits(SLACK))
            .expect("should accept an absolute program")
            .environment(name, "credential");
        assert!(
            plain.is_err(),
            "should refuse {name} through the plain environment"
        );
        let secret = BoundedCommand::new(&test_binary(), dir.path(), limits(SLACK))
            .expect("should accept an absolute program")
            .secret_environment(name, "credential");
        assert!(secret.is_ok(), "should accept {name} as a secret variable");
    }
}

#[test]
fn malformed_environment_names_are_rejected() {
    let dir = workspace();
    for name in ["", "A=B", "NUL\0NAME"] {
        let command = BoundedCommand::new(&test_binary(), dir.path(), limits(SLACK))
            .expect("should accept an absolute program");
        assert!(
            command.environment(name, "value").is_err(),
            "should reject environment name {name:?}"
        );
    }
}

#[test]
fn harmless_names_are_accepted() {
    let dir = workspace();
    for name in ["CARGO_TARGET_DIR", "CARGO_HOME", "LANG", "HOME", "TZ"] {
        let command = BoundedCommand::new(&test_binary(), dir.path(), limits(SLACK))
            .expect("should accept an absolute program");
        assert!(
            command.environment(name, "value").is_ok(),
            "should accept {name}"
        );
    }
}

#[test]
fn aws_lc_sys_build_switches_are_plain_variables() {
    let dir = workspace();
    for name in [
        "AWS_LC_SYS_USE_SYSTEM",
        "AWS_LC_SYS_USE_SYSTEM_aarch64_apple_darwin",
    ] {
        let plain = BoundedCommand::new(&test_binary(), dir.path(), limits(SLACK))
            .expect("should accept an absolute program")
            .environment(name, "0");
        assert!(plain.is_ok(), "should accept {name} as a plain variable");
    }
    for name in [
        "AWS_SECRET_ACCESS_KEY",
        "AWS_LC_SYS_OTHER",
        "AWS_LC_SYS_USE_SYSTEM_",
        "AWS_LC_SYS_USE_SYSTEMX",
        "AWS_LC_SYS_USE_SYSTEM_x86-64",
    ] {
        let credential = BoundedCommand::new(&test_binary(), dir.path(), limits(SLACK))
            .expect("should accept an absolute program")
            .environment(name, "fictional");
        assert!(
            credential.is_err(),
            "should still refuse {name} as a plain variable"
        );
    }
}
