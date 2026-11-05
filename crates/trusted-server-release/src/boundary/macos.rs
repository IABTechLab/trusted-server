//! macOS `sandbox-exec` deny-default profile.
//!
//! The profile text is assembled only from compiled-in fragments and
//! generated slot indices; every path reaches the sandbox as a `-D NAME=PATH`
//! define read through `(param "NAME")`, never as profile text. A missing
//! define makes `sandbox-exec` reject the profile, so slots fail closed.
//!
//! The base rules follow the observed working deny-default profile that ran
//! offline `npm ci` and the full Vitest suite under denial: system library
//! reads from explicit `/usr` subdirectories (never `/usr/local`),
//! `same-sandbox` signals only, file writes only under private writable roots
//! and `(deny network*)`, after which only Unix-domain sockets whose paths lie
//! under a writable root are re-allowed. `connect` to an INET address from
//! the confined process and every descendant fails with `EPERM`.
//!
//! Observed on macOS 26.6.2: under `(deny default)` alone the `setsid` and
//! `setpgid` system calls are not filtered; `(allow syscall-unix)` followed by
//! `(deny syscall-unix (syscall-number …))` makes both fail with `EPERM` in
//! the confined process and its descendants.
//!
//! System roots, such as the Apple developer directory, are allowed only
//! `file-read*` and `process-exec` and carry the same explicit `file-write*`
//! denial as immutable and tool roots.
//!
//! `xcrun`, `xcodebuild`, `xcode-select`, `sudo`, `su`, `login` and
//! `newgrp` in `/usr/bin` may not be executed. A build script that invokes
//! `xcrun` (for example to locate an SDK) therefore fails closed; the recipe
//! supplies `SDKROOT`, `DEVELOPER_DIR` and the direct Xcode `clang` and `ar`
//! instead. The `/usr/bin` `cc`, `clang` and `ld` shims are not denied.
//!
//! Each run also binds two fresh empty marker files, created outside every
//! root: the profile allows reading one and denies reading the other. That
//! pair of rules identifies processes of exactly this run's sandbox instance
//! to the survivor check, independent of their environment.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use error_stack::Report;
use sha2::{Digest as _, Sha256};

use super::ConfinedCommand;
use super::roots::{BoundaryRoots, RootRole, validate_path_text};
use crate::error::ReleaseError;
use crate::process::{BoundedCommand, ProcessResult};

/// The fixed system `sandbox-exec` executable.
pub(crate) const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";
/// Environment variable carrying the per-run survivor-detection nonce.
pub(crate) const RUN_NONCE_VARIABLE: &str = "TS_RELEASE_BOUNDARY_RUN";
/// Darwin `SYS_setpgid` (`<sys/syscall.h>`), identical on `arm64` and `x86_64`.
pub(crate) const SYS_SETPGID: u32 = 82;
/// Darwin `SYS_setsid` (`<sys/syscall.h>`), identical on `arm64` and `x86_64`.
pub(crate) const SYS_SETSID: u32 = 147;

/// Deny-default base rules; contains no caller-supplied text.
const PROFILE_HEADER: &str = r#"(version 1)
(deny default)
(allow process-fork)
(allow signal (target same-sandbox))
(allow sysctl-read)
(allow syscall-unix)
(deny syscall-unix (syscall-number 147 82))
(allow file-read-metadata)
(allow file-read*
  (literal "/")
  (subpath "/usr/bin")
  (subpath "/usr/lib")
  (subpath "/usr/libexec")
  (subpath "/usr/share")
  (subpath "/bin")
  (subpath "/System")
  (subpath "/Library/Apple")
  (subpath "/private/var/db/dyld")
  (subpath "/dev")
  (literal "/private/var/select/sh"))
(allow process-exec
  (subpath "/usr/bin")
  (subpath "/usr/libexec")
  (subpath "/bin")
  (subpath "/System"))
(allow file-read* file-write* (literal "/dev/null"))
(allow file-ioctl (literal "/dev/null"))
"#;
/// Network denial; later SBPL rules take precedence over earlier ones.
const NETWORK_DENIAL: &str = "(deny network*)\n";
/// Cargo's macOS `LibreSSL` initialization ignores `OPENSSL_CONF` and exits on
/// EPERM for its optional system configuration. Hide only that ambient input
/// as absent instead of granting it access. Apply after every root read grant.
const OPENSSL_CONFIGURATION_DENIAL: &str = "(deny file-read* (with errno ENOENT)\n  \
    (literal \"/private/etc/ssl/openssl.cnf\")\n  \
    (literal \"/etc/ssl/openssl.cnf\"))\n";
/// Tools no build step may start, even though `/usr/bin` is executable: the
/// Xcode selection shims (`xcrun`, `xcodebuild`, `xcode-select`), which
/// would pick a developer directory outside the bound record, and the
/// privilege tools. The `/usr/bin` compiler and linker shims stay allowed:
/// the recipe selects the bound Xcode tools directly, and the shims honour
/// the bound `DEVELOPER_DIR` if a build script runs them anyway.
pub(crate) const DENIED_EXECUTABLES: [&str; 7] = [
    "/usr/bin/sudo",
    "/usr/bin/su",
    "/usr/bin/login",
    "/usr/bin/newgrp",
    "/usr/bin/xcrun",
    "/usr/bin/xcodebuild",
    "/usr/bin/xcode-select",
];

/// One explicit `process-exec` denial for a fixed compiled-in path.
fn execution_denial(path: &str) -> String {
    format!("(deny process-exec (literal \"{path}\"))\n")
}
/// Profile parameter naming the run marker the instance may read.
pub(crate) const RUN_MARK_ALLOW: &str = "RUN_MARK_ALLOW";
/// Profile parameter naming the run marker the instance must not read.
pub(crate) const RUN_MARK_DENY: &str = "RUN_MARK_DENY";
/// Instance-identifying marker rules, last so that they take precedence.
const RUN_MARK_RULES: &str = "(allow file-read-data (literal (param \"RUN_MARK_ALLOW\")))\n\
                              (deny file-read* (literal (param \"RUN_MARK_DENY\")))\n";
/// Allowance before a run's recorded start. It only widens eligibility;
/// identity checks still gate every match.
const START_MARGIN: Duration = Duration::from_secs(1);
/// Profile identity domain for the configuration fingerprint.
const FINGERPRINT_DOMAIN: &[u8] = b"trusted-server-boundary-macos-sbpl-v2\n";

/// A generated profile and the defines that bind its slots.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MacProfile {
    text: String,
    defines: Vec<(String, String)>,
}

fn slot_name(role: RootRole, index: usize) -> String {
    let prefix = match role {
        RootRole::Immutable => "IMMUTABLE",
        RootRole::Tool => "TOOL",
        RootRole::Writable => "WRITABLE",
        RootRole::System => "SYSTEM",
    };
    format!("{prefix}_{index}")
}

fn allow_rule(role: RootRole, name: &str) -> String {
    let operations = match role {
        RootRole::Immutable | RootRole::Tool | RootRole::System => "file-read* process-exec",
        RootRole::Writable => "file-read* file-write* process-exec",
    };
    format!("(allow {operations} (subpath (param \"{name}\")))\n")
}

fn deny_write_rule(name: &str) -> String {
    format!("(deny file-write* (subpath (param \"{name}\")))\n")
}

fn unix_socket_rule(name: &str) -> String {
    format!(
        "(allow network-bind network-outbound network-inbound \
         (local unix-socket (subpath (param \"{name}\"))) \
         (remote unix-socket (subpath (param \"{name}\"))))\n"
    )
}

/// Validates one define; the value must be safe profile-parameter text.
///
/// # Errors
///
/// Returns [`ReleaseError::Boundary`] when `path` fails
/// [`validate_path_text`].
pub(crate) fn define(name: &str, path: &Path) -> Result<(String, String), Report<ReleaseError>> {
    let value = validate_path_text(path)?;
    Ok((name.to_owned(), value.to_owned()))
}

/// Returns 16 random bytes as lowercase hex.
fn run_nonce() -> Result<String, Report<ReleaseError>> {
    Ok(hex::encode(super::random_bytes::<16>()?))
}

impl MacProfile {
    /// Generates the profile for `roots`.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Boundary`] when a root path is not safe define
    /// text.
    pub(crate) fn generate(roots: &BoundaryRoots) -> Result<Self, Report<ReleaseError>> {
        let mut text = String::from(PROFILE_HEADER);
        let mut denials = String::new();
        let mut sockets = String::new();
        let mut defines = Vec::new();
        for (role, index, path) in roots.entries() {
            let name = slot_name(role, index);
            text.push_str(&allow_rule(role, &name));
            if role == RootRole::Writable {
                sockets.push_str(&unix_socket_rule(&name));
            } else {
                denials.push_str(&deny_write_rule(&name));
            }
            defines.push(define(&name, path)?);
        }
        text.push_str(&denials);
        text.push_str(OPENSSL_CONFIGURATION_DENIAL);
        for path in DENIED_EXECUTABLES {
            text.push_str(&execution_denial(path));
        }
        text.push_str(NETWORK_DENIAL);
        text.push_str(&sockets);
        text.push_str(RUN_MARK_RULES);
        Ok(Self { text, defines })
    }

    /// Returns the profile text.
    #[must_use]
    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    /// Returns the `-D` defines in slot order.
    #[must_use]
    pub(crate) fn defines(&self) -> &[(String, String)] {
        &self.defines
    }

    /// Returns `sandbox-exec` arguments preceding the confined program.
    #[must_use]
    pub(crate) fn arguments(&self) -> Vec<OsString> {
        let mut arguments = vec![OsString::from("-p"), OsString::from(&self.text)];
        for (name, value) in &self.defines {
            arguments.push(OsString::from("-D"));
            arguments.push(OsString::from(format!("{name}={value}")));
        }
        arguments
    }

    /// Returns the profile fingerprint (before the instance nonce). The marker
    /// rules are covered; per-run marker paths are not.
    #[must_use]
    pub(crate) fn fingerprint(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(FINGERPRINT_DOMAIN);
        for argument in self.arguments() {
            let bytes = argument.as_encoded_bytes();
            hasher.update((bytes.len() as u64).to_be_bytes());
            hasher.update(bytes);
        }
        hasher.finalize().into()
    }

    /// Wraps `command` in `sandbox-exec`, adding the run's marker defines and
    /// nonce variable.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Process`] when the bounded command rejects the
    /// working directory or an environment variable (including a caller
    /// variable named [`RUN_NONCE_VARIABLE`]).
    pub(crate) fn wrap(
        &self,
        command: &ConfinedCommand,
        run: &RunIdentity,
    ) -> Result<BoundedCommand, Report<ReleaseError>> {
        let mut arguments = self.arguments();
        for (name, value) in &run.defines {
            arguments.push(OsString::from("-D"));
            arguments.push(OsString::from(format!("{name}={value}")));
        }
        arguments.push(command.program.clone().into_os_string());
        arguments.extend(command.arguments.iter().cloned());
        let mut bounded = BoundedCommand::new(
            Path::new(SANDBOX_EXEC),
            &command.working_directory,
            command.limits,
        )?
        .arguments(arguments)
        .environment(RUN_NONCE_VARIABLE, &run.nonce)?;
        if let Some(umask) = command.umask {
            bounded = bounded.umask(umask);
        }
        for (name, value) in &command.environment {
            bounded = if command.is_tool_selector(name) {
                bounded.tool_selector_environment(name, value)?
            } else {
                bounded.environment(name, value)?
            };
        }
        Ok(command.finish(bounded))
    }

    /// Runs `command` confined, then kills and rejects any process of this
    /// run's sandbox instance, or carrying its nonce, that outlived it.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Boundary`] when the run identity cannot be
    /// created or an escaped descendant was found (even if the run itself
    /// succeeded), or the run's own error.
    pub(crate) fn run(
        &self,
        command: &ConfinedCommand,
    ) -> Result<ProcessResult, Report<ReleaseError>> {
        let run = RunIdentity::new(self)?;
        let outcome = self.wrap(command, &run).and_then(|bounded| bounded.run());
        let survivors = reap_survivors(&run);
        match (outcome, survivors) {
            (Ok(result), Ok(())) => Ok(result),
            (Err(run), Ok(())) => Err(run),
            (Ok(_), Err(survivors)) => Err(survivors),
            (Err(run), Err(survivors)) => {
                Err(survivors.attach(format!("the run also failed: {run:?}")))
            }
        }
    }
}

/// One run's survivor identity: an environment nonce and two private marker
/// files outside every boundary root.
pub(crate) struct RunIdentity {
    nonce: String,
    started_after_micros: u64,
    defines: [(String, String); 2],
    allow: PathBuf,
    deny: PathBuf,
    _directory: tempfile::TempDir,
}

impl RunIdentity {
    /// Creates fresh markers for one run of `profile`.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Boundary`] when the markers cannot be created,
    /// are not safe define text, or overlap a boundary root.
    pub(crate) fn new(profile: &MacProfile) -> Result<Self, Report<ReleaseError>> {
        let directory = tempfile::Builder::new()
            .prefix("ts-boundary-run-")
            .tempdir()
            .map_err(super::boundary_error)?;
        let base = fs::canonicalize(directory.path()).map_err(super::boundary_error)?;
        if profile.defines.iter().any(|(_, root)| {
            let root = Path::new(root);
            base.starts_with(root) || root.starts_with(&base)
        }) {
            return Err(super::boundary_error("run markers overlap a boundary root"));
        }
        let allow = base.join("allow");
        let deny = base.join("deny");
        for marker in [&allow, &deny] {
            fs::write(marker, b"").map_err(super::boundary_error)?;
        }
        Ok(Self {
            nonce: run_nonce()?,
            started_after_micros: started_after_micros()?,
            defines: [
                define(RUN_MARK_ALLOW, &allow)?,
                define(RUN_MARK_DENY, &deny)?,
            ],
            allow,
            deny,
            _directory: directory,
        })
    }
}

/// Returns the earliest process start this run may own: now, less a
/// [`START_MARGIN`], in microseconds since the epoch.
fn started_after_micros() -> Result<u64, Report<ReleaseError>> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(super::boundary_error)?;
    u64::try_from(now.saturating_sub(START_MARGIN).as_micros()).map_err(super::boundary_error)
}

#[cfg(target_os = "macos")]
fn reap_survivors(run: &RunIdentity) -> Result<(), Report<ReleaseError>> {
    super::survivors::reap(&super::survivors::RunSignature {
        variable: RUN_NONCE_VARIABLE,
        nonce: &run.nonce,
        allow: &run.allow,
        deny: &run.deny,
        started_after_micros: run.started_after_micros,
    })
}

#[cfg(not(target_os = "macos"))]
fn reap_survivors(_run: &RunIdentity) -> Result<(), Report<ReleaseError>> {
    Err(Report::new(ReleaseError::Boundary).attach("sandbox-exec survivor checks are macOS-only"))
}
