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

use std::ffi::OsString;
use std::path::Path;

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
    };
    format!("{prefix}_{index}")
}

fn allow_rule(role: RootRole, name: &str) -> String {
    let operations = match role {
        RootRole::Immutable | RootRole::Tool => "file-read* process-exec",
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
        text.push_str(NETWORK_DENIAL);
        text.push_str(&sockets);
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

    /// Returns the profile fingerprint (before the instance nonce).
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

    /// Wraps `command` in `sandbox-exec`, adding the run nonce variable.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Process`] when the bounded command rejects the
    /// working directory or an environment variable (including a caller
    /// variable named [`RUN_NONCE_VARIABLE`]).
    pub(crate) fn wrap(
        &self,
        command: &ConfinedCommand,
        run_nonce: &str,
    ) -> Result<BoundedCommand, Report<ReleaseError>> {
        let mut arguments = self.arguments();
        arguments.push(command.program.clone().into_os_string());
        arguments.extend(command.arguments.iter().cloned());
        let mut bounded = BoundedCommand::new(
            Path::new(SANDBOX_EXEC),
            &command.working_directory,
            command.limits,
        )?
        .arguments(arguments)
        .environment(RUN_NONCE_VARIABLE, run_nonce)?;
        for (name, value) in &command.environment {
            bounded = bounded.environment(name, value)?;
        }
        Ok(command.finish(bounded))
    }

    /// Runs `command` confined, then kills and rejects any descendant that
    /// escaped the process group while keeping the run nonce.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Boundary`] when an escaped descendant was found
    /// (even if the run itself succeeded), or the run's own error.
    pub(crate) fn run(
        &self,
        command: &ConfinedCommand,
    ) -> Result<ProcessResult, Report<ReleaseError>> {
        let nonce = run_nonce()?;
        let outcome = self.wrap(command, &nonce).and_then(|bounded| bounded.run());
        let survivors = reap_survivors(&nonce);
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

#[cfg(target_os = "macos")]
fn reap_survivors(nonce: &str) -> Result<(), Report<ReleaseError>> {
    super::survivors::reap(RUN_NONCE_VARIABLE, nonce)
}

#[cfg(not(target_os = "macos"))]
fn reap_survivors(_nonce: &str) -> Result<(), Report<ReleaseError>> {
    Err(Report::new(ReleaseError::Boundary).attach("sandbox-exec survivor checks are macOS-only"))
}
