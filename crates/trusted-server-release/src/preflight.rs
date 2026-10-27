//! Inherited-environment audit and allowlisted child-environment construction.
//!
//! The audit runs over the complete inherited snapshot before anything is
//! cleared, so an empty starting environment cannot hide a rejected channel.
//! Errors name variables but never print their values, which may be secrets.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

use error_stack::Report;

use crate::error::ReleaseError;
use crate::policy;
use crate::source::{self, CommitId};

const RELEASE_BUILD: &str = "TRUSTED_SERVER__RELEASE_BUILD";
const REMOVED_PREFIXES: [&str; 2] = ["TRUSTED_SERVER__", "EDGEZERO__"];

/// Exact (uppercase) names that select compilers, flags, loaders, sources or
/// credentials. Sorted, unique, and never covered by [`REJECTED_PREFIXES`] or
/// [`NATIVE_SELECTORS`].
const REJECTED_EXACT: &[&str] = &[
    "ACTIONS_CACHE_URL",
    "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
    "ACTIONS_ID_TOKEN_REQUEST_URL",
    "ACTIONS_RESULTS_URL",
    "ACTIONS_RUNTIME_TOKEN",
    "ACTIONS_RUNTIME_URL",
    "BASH_ENV",
    "CARGO",
    "CLANG_PATH",
    "CRATE_CC_NO_DEFAULTS",
    "CROSS_COMPILE",
    "DEVELOPER_DIR",
    "ENV",
    "GH_ENTERPRISE_TOKEN",
    "GH_TOKEN",
    "GITHUB_ENTERPRISE_TOKEN",
    "GITHUB_TOKEN",
    "LIBCLANG_PATH",
    "LLVM_CONFIG_PATH",
    "MACOSX_DEPLOYMENT_TARGET",
    "NASM",
    "NODE_AUTH_TOKEN",
    "NODE_ENV",
    "NODE_EXTRA_CA_CERTS",
    "NODE_OPTIONS",
    "NODE_PATH",
    "NODE_TLS_REJECT_UNAUTHORIZED",
    "NPM_TOKEN",
    "PKG_CONFIG",
    "PROTOC",
    "PROTOC_INCLUDE",
    "RUSTC",
    "RUSTDOC",
    "RUSTDOCFLAGS",
    "RUSTFLAGS",
    "SDKROOT",
    "SOURCE_DATE_EPOCH",
    "SSH_AUTH_SOCK",
    "TSJS_PREBUILT_DIR",
    "TSJS_SKIP_BUILD",
    "TSJS_TEST",
    "VCPKG_INSTALLATION_ROOT",
    "VCPKG_ROOT",
];

/// Uppercase prefixes whose whole family is recipe-selected or forbidden.
const REJECTED_PREFIXES: &[&str] = &[
    "AWS_",
    "BINDGEN_",
    "CARGO_",
    "CLOUDFLARE_",
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
const NATIVE_SELECTORS: [&str; 11] = [
    "AR",
    "ARFLAGS",
    "CC",
    "CFLAGS",
    "CPPFLAGS",
    "CXX",
    "CXXFLAGS",
    "CXXSTDLIB",
    "LD",
    "LDFLAGS",
    "RANLIB",
];

/// A hosted setup export accepted only for an exact value whose path resolves to a
/// mandatory reviewed identity; value-only exceptions are not representable.
pub(crate) struct ReviewedException {
    name: &'static str,
    value: &'static str,
    path_identity: &'static str,
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
        } else if REMOVED_PREFIXES
            .iter()
            .any(|prefix| name.to_ascii_uppercase().starts_with(prefix))
        {
            rejected.insert(format!("{name} (ambiguous operator prefix casing)"));
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
    REJECTED_EXACT.contains(&upper.as_str())
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

impl ReviewedException {
    /// Accepts only the exact name and value whose path resolves to the reviewed identity.
    fn matches(&self, name: &str, value: &str) -> bool {
        self.name == name
            && self.value == value
            && std::fs::canonicalize(value).is_ok_and(|path| path == Path::new(self.path_identity))
    }
}

fn is_reviewed(name: &str, value: &str) -> bool {
    REVIEWED_SETUP_EXCEPTIONS
        .iter()
        .any(|exception| exception.matches(name, value))
}

/// Resolves the source root, which is always the current directory.
///
/// # Errors
///
/// Fails when the current directory cannot be resolved.
pub(crate) fn source_root() -> Result<PathBuf, Report<ReleaseError>> {
    std::env::current_dir().map_err(|error| {
        Report::new(ReleaseError::Source).attach(format!("cannot resolve source root: {error}"))
    })
}

fn names_cohort(root: &Path, cohort: &Path) -> bool {
    !cohort.as_os_str().is_empty()
        && !cohort
            .components()
            .any(|component| matches!(component, Component::ParentDir))
        && root
            .join(cohort)
            .components()
            .eq(root.join(policy::COHORT_PATH).components())
}

/// Runs the complete fail-closed preflight over the current-directory source.
///
/// The qualified selector always runs, so omitting `--require-qualified`
/// can never succeed where passing it would fail.
///
/// # Errors
///
/// Fails on any rejected inherited variable, a cohort path other than the
/// source-owned one, unauthenticated source, inputs that differ from the
/// approved commit, or the closed qualified selector.
pub(crate) fn run_preflight(
    inherited: &[(OsString, OsString)],
    cohort: &Path,
    commit: &CommitId,
    require_qualified: bool,
) -> Result<(), Report<ReleaseError>> {
    let audited = audit_environment(inherited)?;
    if !audited.removed_names().is_empty() {
        crate::notice(format_args!(
            "removed inherited operator variables: {}",
            audited.removed_names().join(", ")
        ));
    }
    let root = source_root()?;
    if !names_cohort(&root, cohort) {
        return Err(Report::new(ReleaseError::Policy).attach(format!(
            "--cohort must name {} below the current source root",
            policy::COHORT_PATH
        )));
    }
    let source = source::authenticate(&root, commit, &audited)?;
    let inputs = policy::load_inputs(&root)?;
    for (path, sha256) in inputs.input_digests() {
        if source.tracked_sha256(path) != Some(sha256) {
            return Err(Report::new(ReleaseError::Source)
                .attach(format!("{path} does not match the approved commit")));
        }
    }
    crate::notice(format_args!(
        "source matched locally; git executable and remote eligibility not established \
         (commit {}, epoch {}, snapshot {}, helper closure {} over {} paths; \
         git executable identity: Task 8; remote branch-tip ancestry and rulesets: Task 14)",
        source.commit(),
        source.epoch(),
        source.snapshot().fingerprint(),
        source.closure_digest(),
        source.closure_paths().len()
    ));
    if !require_qualified {
        crate::notice(format_args!(
            "qualified selection is required whether or not --require-qualified is passed"
        ));
    }
    let selection = policy::select_qualified(&inputs, &[])?;
    source.snapshot().verify_unchanged()?;
    crate::notice(format_args!("{}", selection.summary()));
    Ok(())
}

// Recipe execution (Tasks 5 and 7) is the only production consumer of the
// child-environment builder; until it lands, only tests reference it.
#[allow(dead_code)]
pub(crate) mod child;

#[cfg(test)]
mod tests;
