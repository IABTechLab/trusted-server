//! Environment validation for [`BoundedCommand`].
//!
//! The denylists are a backstop; the Task 7 boundary and preparation callers
//! supply per-tool allowlists.

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt as _;

use error_stack::Report;

use super::{BoundedCommand, invalid};
use crate::error::ReleaseError;

/// Uppercased name prefixes that inject code or configuration into children.
const INJECTION_PREFIXES: [&str; 5] = [
    "DYLD_",
    "GIT_CONFIG_KEY_",
    "GIT_CONFIG_VALUE_",
    "LD_",
    "NPM_CONFIG_",
];
/// Uppercased `CARGO_TARGET_<triple>_<suffix>` suffixes that select tools.
const CARGO_TARGET_TOOL_SUFFIXES: [&str; 2] = ["_LINKER", "_RUNNER"];
/// Uppercased exact names that inject code or configuration into children.
const INJECTION_EXACT: [&str; 42] = [
    "BASH_ENV",
    "CARGO_BUILD_RUSTC",
    "CARGO_BUILD_RUSTDOC",
    "CARGO_BUILD_RUSTFLAGS",
    "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER",
    "CARGO_BUILD_RUSTC_WRAPPER",
    "CARGO_ENCODED_RUSTFLAGS",
    "EDITOR",
    "ENV",
    "GCONV_PATH",
    "GIT_ASKPASS",
    "GIT_CONFIG",
    "GIT_CONFIG_COUNT",
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_SYSTEM",
    "GIT_EDITOR",
    "GIT_EXEC_PATH",
    "GIT_EXTERNAL_DIFF",
    "GIT_PAGER",
    "GIT_PROXY_COMMAND",
    "GIT_SSH",
    "GIT_SSH_COMMAND",
    "GIT_TEMPLATE_DIR",
    "JAVA_TOOL_OPTIONS",
    "NODE_OPTIONS",
    "NODE_PATH",
    "PAGER",
    "PERL5LIB",
    "PERL5OPT",
    "PYTHONPATH",
    "PYTHONSTARTUP",
    "RUBYLIB",
    "RUBYOPT",
    "RUSTC",
    "RUSTC_WORKSPACE_WRAPPER",
    "RUSTC_WRAPPER",
    "RUSTDOC",
    "RUSTFLAGS",
    "SSH_ASKPASS",
    "VISUAL",
    "_JAVA_OPTIONS",
];
/// Uppercased credential name prefixes accepted only as secrets.
const CREDENTIAL_PREFIXES: [&str; 2] = ["AWS_", "FASTLY_"];
/// Uppercased `aws-lc-sys` system-library switch, also per target as
/// `<name>_<triple>`. It shares the `AWS_` prefix but carries no credential.
const BUILD_SWITCH: &str = "AWS_LC_SYS_USE_SYSTEM";
/// Uppercased `CARGO_REGISTRIES_<name>_TOKEN` registry credential affixes.
const CARGO_REGISTRIES_TOKEN: (&str, &str) = ("CARGO_REGISTRIES_", "_TOKEN");
/// Uppercased credential names accepted only as secrets.
const CREDENTIAL_EXACT: [&str; 8] = [
    "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
    "ACTIONS_RUNTIME_TOKEN",
    "CARGO_REGISTRY_TOKEN",
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "NODE_AUTH_TOKEN",
    "NPM_TOKEN",
    "SIGSTORE_ID_TOKEN",
];

/// One explicit child environment entry.
#[derive(Clone)]
pub(super) struct EnvironmentValue {
    pub(super) value: OsString,
    pub(super) secret: bool,
}

fn upper_name(name: &OsStr) -> String {
    name.to_string_lossy().to_ascii_uppercase()
}

fn is_injection(name: &OsStr) -> bool {
    let upper = upper_name(name);
    let cargo_target_tool = upper.starts_with("CARGO_TARGET_")
        && CARGO_TARGET_TOOL_SUFFIXES
            .iter()
            .any(|suffix| upper.ends_with(suffix));
    cargo_target_tool
        || INJECTION_PREFIXES
            .iter()
            .any(|prefix| upper.starts_with(prefix))
        || INJECTION_EXACT.contains(&upper.as_str())
}

pub(super) fn is_credential(name: &OsStr) -> bool {
    let upper = upper_name(name);
    let (registries, token) = CARGO_REGISTRIES_TOKEN;
    let registry_token = upper.starts_with(registries) && upper.ends_with(token);
    let build_switch = upper == BUILD_SWITCH
        || upper
            .strip_prefix(BUILD_SWITCH)
            .and_then(|suffix| suffix.strip_prefix('_'))
            .is_some_and(|triple| {
                !triple.is_empty()
                    && triple
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            });
    registry_token
        || (!build_switch
            && upper != "AWS_LC_SYS_CMAKE_BUILDER"
            && upper != "AWS_LC_SYS_CMAKE_BUILDER_X86_64_UNKNOWN_LINUX_GNU"
            && CREDENTIAL_PREFIXES
                .iter()
                .any(|prefix| upper.starts_with(prefix)))
        || CREDENTIAL_EXACT.contains(&upper.as_str())
}

/// Exact names an orchestrator may set explicitly through
/// [`BoundedCommand::tool_selector_environment`] despite the injection denylist.
const TOOL_SELECTORS: [&str; 1] = ["RUSTC"];

impl BoundedCommand {
    pub(super) fn insert_environment(
        self,
        name: &OsStr,
        value: &OsStr,
        secret: bool,
    ) -> Result<Self, Report<ReleaseError>> {
        self.insert_checked(name, value, secret, false)
    }

    /// Adds one explicitly selected tool path, such as the compiler.
    ///
    /// The generic [`Self::environment`] keeps rejecting every tool selector,
    /// because an inherited or attacker-chosen `RUSTC` swaps the compiler.
    /// This method accepts only the names in [`TOOL_SELECTORS`], only with an
    /// absolute path, and only once.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Process`] for any other name, a relative or
    /// empty value, or a repeated name.
    pub(crate) fn tool_selector_environment(
        self,
        name: impl AsRef<OsStr>,
        value: impl AsRef<OsStr>,
    ) -> Result<Self, Report<ReleaseError>> {
        let (name, value) = (name.as_ref(), value.as_ref());
        let selectable = TOOL_SELECTORS
            .iter()
            .any(|selector| name == OsStr::new(selector));
        if !selectable || !value.as_bytes().starts_with(b"/") {
            return Err(invalid(format!(
                "{name:?} is not an explicitly selectable tool with an absolute path"
            )));
        }
        self.insert_checked(name, value, false, true)
    }

    fn insert_checked(
        mut self,
        name: &OsStr,
        value: &OsStr,
        secret: bool,
        tool_selector: bool,
    ) -> Result<Self, Report<ReleaseError>> {
        let name_bytes = name.as_bytes();
        if name_bytes.is_empty() || name_bytes.contains(&b'=') || name_bytes.contains(&0) {
            return Err(invalid(format!(
                "environment variable name {name:?} is malformed"
            )));
        }
        if value.as_bytes().contains(&0) {
            return Err(invalid(format!(
                "environment variable {name:?} value contains NUL"
            )));
        }
        if !tool_selector && is_injection(name) {
            return Err(invalid(format!(
                "environment variable {name:?} injects code or configuration"
            )));
        }
        if matches!(
            upper_name(name).as_str(),
            "AWS_LC_SYS_CMAKE_BUILDER" | "AWS_LC_SYS_CMAKE_BUILDER_X86_64_UNKNOWN_LINUX_GNU"
        ) && value != OsStr::new("0")
        {
            return Err(invalid(
                "only the reviewed AWS-LC cc backend switch is permitted",
            ));
        }
        if self.environment.contains_key(name) {
            return Err(invalid(format!(
                "environment variable {name:?} is set twice"
            )));
        }
        self.environment.insert(
            name.to_os_string(),
            EnvironmentValue {
                value: value.to_os_string(),
                secret,
            },
        );
        Ok(self)
    }
}

#[cfg(test)]
mod selector_tests;
