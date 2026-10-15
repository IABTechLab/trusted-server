//! Inherited-environment audit and allowlisted child-environment construction.
//!
//! The audit runs over the complete inherited snapshot before anything is
//! cleared, so an empty starting environment cannot hide a rejected channel.
//! Errors name variables but never print their values, which may be secrets.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use error_stack::Report;

use crate::documents::lower_hex;
use crate::error::ReleaseError;
use crate::policy::ReleaseTarget;
use crate::source::ApprovedSource;

const RELEASE_BUILD: &str = "TRUSTED_SERVER__RELEASE_BUILD";
const REMOVED_PREFIXES: [&str; 2] = ["TRUSTED_SERVER__", "EDGEZERO__"];
const SKIP_BUILD: &str = "TSJS_SKIP_BUILD";
// The child-environment builder below is consumed by recipe execution (Task 5);
// until then only tests and the typed preflight interface reference it.
#[allow(dead_code)]
const MACOS_DEPLOYMENT_TARGET: &str = "13.0";

/// Exact (uppercase) names that select compilers, flags, loaders, sources or credentials.
const REJECTED_EXACT: [&str; 27] = [
    "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
    "ACTIONS_ID_TOKEN_REQUEST_URL",
    "ACTIONS_RUNTIME_TOKEN",
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
    "CARGO",
    "CRATE_CC_NO_DEFAULTS",
    "DEVELOPER_DIR",
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "MACOSX_DEPLOYMENT_TARGET",
    "NODE_OPTIONS",
    "NODE_PATH",
    "RUSTC",
    "RUSTDOC",
    "RUSTDOCFLAGS",
    "RUSTFLAGS",
    "SDKROOT",
    "SOURCE_DATE_EPOCH",
    "TSJS_SKIP_BUILD",
    "VCPKG_INSTALLATION_ROOT",
    "VCPKG_ROOT",
    "LD_PRELOAD",
    "LD_LIBRARY_PATH",
    "LD_AUDIT",
    "PKG_CONFIG",
];

/// Uppercase prefixes whose whole family is recipe-selected or forbidden.
const REJECTED_PREFIXES: [&str; 23] = [
    "AWS_LC_FIPS_SYS_",
    "AWS_LC_SYS_",
    "BINDGEN_",
    "CARGO_",
    "CMAKE",
    "DYLD_",
    "FASTLY_",
    "GIT_",
    "LD_",
    "LIBSQLITE3_",
    "NPM_CONFIG_",
    "OPENSSL_",
    "PKG_CONFIG_",
    "RING_",
    "RUSTC_",
    "RUSTDOC_",
    "RUSTUP_",
    "SIGSTORE_",
    "SQLCIPHER_",
    "SQLITE3_",
    "SQLITE_",
    "VCPKGRS_",
    "VITE_",
];

/// Native tool selectors; `_<target>`, `-<target>`, `TARGET_` and `HOST_` variants are rejected too.
const NATIVE_SELECTORS: [&str; 10] = [
    "AR", "ARFLAGS", "CC", "CFLAGS", "CPPFLAGS", "CXX", "CXXFLAGS", "LD", "LDFLAGS", "RANLIB",
];

/// A hosted setup export accepted only for an exact value and resolved path identity.
pub(crate) struct ReviewedException {
    name: &'static str,
    value: &'static str,
    path_identity: Option<&'static str>,
}

/// Reviewed hosted setup values. Empty until values and paths are qualified.
pub(crate) const REVIEWED_SETUP_EXCEPTIONS: [ReviewedException; 0] = [];

/// Proof that the inherited environment snapshot passed the release audit.
pub(crate) struct AuditedEnvironment {
    removed: Vec<String>,
    search_path: Option<String>,
}

impl AuditedEnvironment {
    /// Names of inherited operator variables removed rather than passed on.
    pub(crate) fn removed_names(&self) -> &[String] {
        &self.removed
    }

    /// Inherited `PATH`, used only to locate preflight helpers such as Git.
    pub(crate) fn search_path(&self) -> Option<&str> {
        self.search_path.as_deref()
    }
}

/// Audits an inherited environment snapshot collected before any clearing.
///
/// # Errors
///
/// Fails when any variable is non-UTF-8, selects a rejected channel, or sets
/// `TRUSTED_SERVER__RELEASE_BUILD` to a value other than `1`. The error lists
/// every rejected name and never a value.
pub(crate) fn audit_environment(
    snapshot: &[(OsString, OsString)],
) -> Result<AuditedEnvironment, Report<ReleaseError>> {
    let mut rejected = BTreeSet::new();
    let mut removed = BTreeSet::new();
    let mut search_path = None;
    for (name, value) in snapshot {
        let Some(name) = name.to_str() else {
            rejected.insert(format!("{} (non-UTF-8 name)", name.to_string_lossy()));
            continue;
        };
        let Some(value) = value.to_str() else {
            rejected.insert(format!("{name} (non-UTF-8 value)"));
            continue;
        };
        if name == RELEASE_BUILD && value != "1" {
            rejected.insert(format!("{name} (must be exactly 1 when present)"));
        } else if REMOVED_PREFIXES
            .iter()
            .any(|prefix| name.starts_with(prefix))
        {
            removed.insert(name.to_owned());
        } else if is_rejected(name) && !is_reviewed(name, value) {
            rejected.insert(name.to_owned());
        } else if name == "PATH" {
            search_path = Some(value.to_owned());
        }
    }
    if !rejected.is_empty() {
        return Err(Report::new(ReleaseError::Environment).attach(format!(
            "rejected inherited variables: {}",
            rejected.into_iter().collect::<Vec<_>>().join(", ")
        )));
    }
    Ok(AuditedEnvironment {
        removed: removed.into_iter().collect(),
        search_path,
    })
}

fn is_rejected(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    upper == SKIP_BUILD
        || REJECTED_EXACT.contains(&upper.as_str())
        || REJECTED_PREFIXES
            .iter()
            .any(|prefix| upper.starts_with(prefix))
        || NATIVE_SELECTORS
            .iter()
            .any(|selector| is_native_variant(&upper, selector))
}

fn is_native_variant(upper: &str, selector: &str) -> bool {
    let base = upper
        .strip_prefix("TARGET_")
        .or_else(|| upper.strip_prefix("HOST_"))
        .unwrap_or(upper);
    base == selector
        || upper
            .strip_prefix(selector)
            .is_some_and(|suffix| suffix.starts_with(['_', '-']))
}

fn is_reviewed(name: &str, value: &str) -> bool {
    REVIEWED_SETUP_EXCEPTIONS.iter().any(|exception| {
        exception.name == name
            && exception.value == value
            && exception.path_identity.is_none_or(|identity| {
                std::fs::canonicalize(value).is_ok_and(|path| path == Path::new(identity))
            })
    })
}

/// A tool directory produced by authenticated acquisition (Task 8).
#[allow(dead_code)]
pub(crate) struct ToolDirectory(PathBuf);

/// Recipe-selected private locations for one child build.
#[allow(dead_code)]
pub(crate) struct ChildRequest {
    pub(crate) target: ReleaseTarget,
    pub(crate) home: PathBuf,
    pub(crate) cargo_home: PathBuf,
    pub(crate) tool_directories: Vec<ToolDirectory>,
}

/// An allowlisted child environment constructed from scratch.
#[allow(dead_code)]
pub(crate) struct ChildEnvironment {
    variables: BTreeMap<String, String>,
    removed: Vec<String>,
}

#[allow(dead_code)]
impl ChildEnvironment {
    /// Every variable the child receives; nothing is inherited.
    pub(crate) fn variables(&self) -> &BTreeMap<String, String> {
        &self.variables
    }

    /// Sorted names injected by the recipe, recorded as input facts.
    pub(crate) fn injected_names(&self) -> Vec<&str> {
        self.variables.keys().map(String::as_str).collect()
    }

    /// Inherited operator names removed during the audit.
    pub(crate) fn removed_names(&self) -> &[String] {
        &self.removed
    }
}

#[allow(dead_code)]
fn absolute_text<'a>(label: &str, path: &'a Path) -> Result<&'a str, Report<ReleaseError>> {
    path.to_str()
        .filter(|text| path.is_absolute() && !text.contains([':', '\0']))
        .ok_or_else(|| {
            Report::new(ReleaseError::Environment).attach(format!(
                "{label} must be an absolute UTF-8 path without ':' or NUL"
            ))
        })
}

#[allow(dead_code)]
fn build_child(
    audited: &AuditedEnvironment,
    commit: &str,
    epoch: u64,
    request: &ChildRequest,
) -> Result<ChildEnvironment, Report<ReleaseError>> {
    if !lower_hex(commit, 40) {
        return Err(Report::new(ReleaseError::Environment)
            .attach("approved commit must be 40 lowercase hex digits"));
    }
    let mut variables = BTreeMap::from([
        ("AWS_LC_SYS_USE_SYSTEM".to_owned(), "0".to_owned()),
        (
            format!(
                "AWS_LC_SYS_USE_SYSTEM_{}",
                request.target.triple().replace('-', "_")
            ),
            "0".to_owned(),
        ),
        (
            "CARGO_HOME".to_owned(),
            absolute_text("CARGO_HOME", &request.cargo_home)?.to_owned(),
        ),
        (
            "HOME".to_owned(),
            absolute_text("HOME", &request.home)?.to_owned(),
        ),
        ("LC_ALL".to_owned(), "C".to_owned()),
        ("LIBSQLITE3_SYS_USE_PKG_CONFIG".to_owned(), "0".to_owned()),
        ("SOURCE_DATE_EPOCH".to_owned(), epoch.to_string()),
        ("TRUSTED_SERVER__GIT_VERSION".to_owned(), commit.to_owned()),
        (RELEASE_BUILD.to_owned(), "1".to_owned()),
        ("TZ".to_owned(), "UTC".to_owned()),
    ]);
    if !request.tool_directories.is_empty() {
        let entries = request
            .tool_directories
            .iter()
            .map(|directory| absolute_text("tool directory", &directory.0))
            .collect::<Result<Vec<_>, _>>()?;
        variables.insert("PATH".to_owned(), entries.join(":"));
    }
    if request.target.is_darwin() {
        variables.insert(
            "MACOSX_DEPLOYMENT_TARGET".to_owned(),
            MACOS_DEPLOYMENT_TARGET.to_owned(),
        );
    }
    Ok(ChildEnvironment {
        variables,
        removed: audited.removed.clone(),
    })
}

/// Builds the allowlisted child environment for one recipe execution.
///
/// Requires both the audited inherited environment and the locally
/// authenticated source, whose commit and committer epoch are injected.
///
/// # Errors
///
/// Fails when a private location or tool directory is not an absolute UTF-8
/// path without `:` or NUL.
#[allow(dead_code)]
pub(crate) fn child_environment(
    audited: &AuditedEnvironment,
    source: &ApprovedSource,
    request: &ChildRequest,
) -> Result<ChildEnvironment, Report<ReleaseError>> {
    build_child(audited, source.commit(), source.epoch(), request)
}

#[cfg(test)]
mod tests;
